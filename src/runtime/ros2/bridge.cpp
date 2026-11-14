// The C API of the IRIS ROS 2 bridge (everything but actions, which live in
// actions.cpp).
//
// Conventions, so the IRIS side (src/stdlib/ros2.iris) can stay mechanical:
//   * functions that create something return its handle, or 0 on failure;
//   * other functions return >= 0 on success and -1 on failure;
//   * a failure always records a message (`iris_ros2_last_error`);
//   * f64 arguments arrive as their IEEE-754 bit pattern in an int64 slot,
//     which is how both IRIS backends pass doubles through `ffi_call_i64`;
//   * text comes back through the per-thread result string.
#ifndef NOMINMAX
#define NOMINMAX
#endif
#ifndef NOGDI
#define NOGDI
#endif

#include <algorithm>
#include <atomic>
#include <chrono>
#include <cmath>
#include <cstring>
#include <limits>
#include <set>
#include <string>
#include <unordered_map>
#include <vector>

#include <geometry_msgs/msg/transform_stamped.hpp>
#include <lifecycle_msgs/msg/state.hpp>
#include <rcl_interfaces/msg/parameter_descriptor.hpp>
#include <rclcpp/create_generic_publisher.hpp>
#include <rclcpp/create_generic_subscription.hpp>
#include <rclcpp/generic_publisher.hpp>
#include <rclcpp/generic_subscription.hpp>
#include <rclcpp/rclcpp.hpp>
#include <rclcpp/serialization.hpp>
#include <rclcpp_lifecycle/lifecycle_node.hpp>
#include <tf2/buffer_core.h>
#include <tf2/exceptions.h>
#include <tf2_ros/buffer.h>
#include <tf2_ros/static_transform_broadcaster.h>
#include <tf2_ros/transform_broadcaster.h>
#include <tf2_ros/transform_listener.h>

#include "bridge.hpp"
#include "entities.hpp"
#include "json.hpp"

namespace iris_ros2
{

namespace
{

constexpr int64_t BRIDGE_ABI_VERSION = 2;

// Node creation flags.
constexpr int64_t NODE_LIFECYCLE = 1;
constexpr int64_t NODE_ALLOW_UNDECLARED = 2;
constexpr int64_t NODE_AUTO_DECLARE = 4;
constexpr int64_t NODE_NO_PARAMETER_SERVICES = 8;
constexpr int64_t NODE_NO_CLOCK_THREAD = 16;

const char * str(const char * s)
{
  return s ? s : "";
}

const char * str_slot(int64_t slot)
{
  return str(reinterpret_cast<const char *>(static_cast<intptr_t>(slot)));
}

double f64_of(int64_t bits)
{
  double value;
  std::memcpy(&value, &bits, sizeof(value));
  return value;
}

int64_t * cell_i64(int64_t cell)
{
  if (cell == 0) {
    throw std::runtime_error("null out-cell");
  }
  return reinterpret_cast<int64_t *>(static_cast<intptr_t>(cell));
}

double * cell_f64(int64_t cell)
{
  if (cell == 0) {
    throw std::runtime_error("null out-cell");
  }
  return reinterpret_cast<double *>(static_cast<intptr_t>(cell));
}

std::vector<std::string> split_args(const char * text)
{
  std::vector<std::string> out;
  std::string current;
  bool quoted = false;
  bool have = false;
  for (const char * p = str(text); *p; ++p) {
    char c = *p;
    if (c == '"') {
      quoted = !quoted;
      have = true;
    } else if (!quoted && (c == ' ' || c == '\t' || c == '\n' || c == '\r')) {
      if (have) {
        out.push_back(current);
        current.clear();
        have = false;
      }
    } else {
      current.push_back(c);
      have = true;
    }
  }
  if (have) {
    out.push_back(current);
  }
  return out;
}

// QoS from the rmw policy enums IRIS passes (history 1 keep-last / 2 keep-all,
// reliability 1 reliable / 2 best-effort, durability 1 transient-local /
// 2 volatile; 0 is "system default" for each).
rclcpp::QoS make_qos(int64_t history, int64_t depth, int64_t reliability, int64_t durability)
{
  if (history < RMW_QOS_POLICY_HISTORY_SYSTEM_DEFAULT || history > RMW_QOS_POLICY_HISTORY_KEEP_ALL) {
    throw std::runtime_error("invalid QoS history " + std::to_string(history));
  }
  if (reliability < RMW_QOS_POLICY_RELIABILITY_SYSTEM_DEFAULT ||
    reliability > RMW_QOS_POLICY_RELIABILITY_BEST_EFFORT)
  {
    throw std::runtime_error("invalid QoS reliability " + std::to_string(reliability));
  }
  if (durability < RMW_QOS_POLICY_DURABILITY_SYSTEM_DEFAULT ||
    durability > RMW_QOS_POLICY_DURABILITY_VOLATILE)
  {
    throw std::runtime_error("invalid QoS durability " + std::to_string(durability));
  }
  if (depth < 0 || (history == RMW_QOS_POLICY_HISTORY_KEEP_LAST && depth == 0)) {
    throw std::runtime_error("invalid QoS depth " + std::to_string(depth));
  }
  rmw_qos_profile_t profile = rmw_qos_profile_default;
  profile.history = static_cast<rmw_qos_history_policy_t>(history);
  profile.depth = static_cast<size_t>(depth);
  profile.reliability = static_cast<rmw_qos_reliability_policy_t>(reliability);
  profile.durability = static_cast<rmw_qos_durability_policy_t>(durability);
  return rclcpp::QoS(rclcpp::QoSInitialization::from_rmw(profile), profile);
}

rclcpp::QoS default_qos()
{
  return make_qos(RMW_QOS_POLICY_HISTORY_KEEP_LAST, 10, RMW_QOS_POLICY_RELIABILITY_RELIABLE,
           RMW_QOS_POLICY_DURABILITY_VOLATILE);
}

void split_stamp(int64_t ns, int32_t & sec, uint32_t & nanosec)
{
  int64_t s = ns / 1000000000LL;
  int64_t rem = ns % 1000000000LL;
  if (rem < 0) {
    rem += 1000000000LL;
    s -= 1;
  }
  sec = static_cast<int32_t>(s);
  nanosec = static_cast<uint32_t>(rem);
}

int64_t join_stamp(int64_t sec, int64_t nanosec)
{
  return sec * 1000000000LL + nanosec;
}

// Changes the program makes itself are not reported back to it as
// EVENT_PARAMETER; only changes from outside (ros2 param set, other nodes).
thread_local int t_internal_parameter_change = 0;

struct InternalParameterChange
{
  InternalParameterChange() {++t_internal_parameter_change;}
  ~InternalParameterChange() {--t_internal_parameter_change;}
};

}  // namespace

// ---------------------------------------------------------------------------
// Context and node entities
// ---------------------------------------------------------------------------

void ContextEntity::push(Event event)
{
  std::lock_guard<std::mutex> lock(queue_mutex_);
  queue_.push_back(std::move(event));
  // A program that never asks for events (it only waits and takes) must not
  // grow the queue without bound; the oldest notifications go first.
  while (queue_.size() > MAX_QUEUED_EVENTS) {
    queue_.pop_front();
    ++dropped_;
  }
}

uint64_t ContextEntity::dropped()
{
  std::lock_guard<std::mutex> lock(queue_mutex_);
  return dropped_;
}

bool ContextEntity::pop(Event & out)
{
  while (true) {
    {
      std::lock_guard<std::mutex> lock(queue_mutex_);
      if (queue_.empty()) {
        return false;
      }
      out = std::move(queue_.front());
      queue_.pop_front();
    }
    // Resolved outside the queue lock: `take` locks the source's own inbox,
    // which its callback holds while queueing.
    if (out.take) {
      out.payload = out.take();
      out.take = nullptr;
      if (!out.payload) {
        continue;  // already consumed by take()/a synchronous call
      }
    }
    return true;
  }
}

size_t ContextEntity::pending()
{
  std::lock_guard<std::mutex> lock(queue_mutex_);
  return queue_.size();
}

int64_t ContextEntity::keep_scoped(MessagePtr message, int64_t scope)
{
  int64_t handle = register_message(std::move(message), this->handle);
  std::lock_guard<std::mutex> lock(queue_mutex_);
  scoped_.emplace_back(scope, handle);
  return handle;
}

void ContextEntity::release_scope(int64_t scope)
{
  std::vector<int64_t> doomed;
  {
    std::lock_guard<std::mutex> lock(queue_mutex_);
    auto keep = std::stable_partition(
      scoped_.begin(), scoped_.end(),
      [scope](const std::pair<int64_t, int64_t> & s) {return scope != 0 && s.first != scope;});
    for (auto it = keep; it != scoped_.end(); ++it) {
      doomed.push_back(it->second);
    }
    scoped_.erase(keep, scoped_.end());
  }
  for (int64_t handle : doomed) {
    registry().destroy(handle);
  }
}

ContextEntity::~ContextEntity()
{
  try {
    executor.reset();
    if (context && context->is_valid()) {
      context->shutdown("iris_ros2: context destroyed");
    }
  } catch (...) {
  }
}

rclcpp::node_interfaces::NodeBaseInterface::SharedPtr NodeEntity::base() const
{
  return node ? node->get_node_base_interface() : lifecycle->get_node_base_interface();
}

rclcpp::node_interfaces::NodeGraphInterface::SharedPtr NodeEntity::graph() const
{
  return node ? node->get_node_graph_interface() : lifecycle->get_node_graph_interface();
}

rclcpp::node_interfaces::NodeTopicsInterface::SharedPtr NodeEntity::topics() const
{
  return node ? node->get_node_topics_interface() : lifecycle->get_node_topics_interface();
}

rclcpp::node_interfaces::NodeServicesInterface::SharedPtr NodeEntity::services() const
{
  return node ? node->get_node_services_interface() : lifecycle->get_node_services_interface();
}

rclcpp::node_interfaces::NodeTimersInterface::SharedPtr NodeEntity::timers() const
{
  return node ? node->get_node_timers_interface() : lifecycle->get_node_timers_interface();
}

rclcpp::node_interfaces::NodeClockInterface::SharedPtr NodeEntity::clock_interface() const
{
  return node ? node->get_node_clock_interface() : lifecycle->get_node_clock_interface();
}

rclcpp::node_interfaces::NodeLoggingInterface::SharedPtr NodeEntity::logging() const
{
  return node ? node->get_node_logging_interface() : lifecycle->get_node_logging_interface();
}

rclcpp::node_interfaces::NodeParametersInterface::SharedPtr NodeEntity::parameters() const
{
  return node ? node->get_node_parameters_interface() :
         lifecycle->get_node_parameters_interface();
}

rclcpp::node_interfaces::NodeWaitablesInterface::SharedPtr NodeEntity::waitables() const
{
  return node ? node->get_node_waitables_interface() : lifecycle->get_node_waitables_interface();
}

rclcpp::Clock::SharedPtr NodeEntity::clock() const
{
  return node ? node->get_clock() : lifecycle->get_clock();
}

rclcpp::Logger NodeEntity::logger() const
{
  return node ? node->get_logger() : lifecycle->get_logger();
}

bool NodeEntity::publishing_enabled() const
{
  return !lifecycle ||
         lifecycle->get_current_state().id() == lifecycle_msgs::msg::State::PRIMARY_STATE_ACTIVE;
}

int NodeEntity::transition_result(int callback) const
{
  std::lock_guard<std::mutex> lock(results_mutex_);
  return callback >= 0 && callback < 8 ? transition_results_[callback] : 0;
}

void NodeEntity::set_transition_result(int callback, int result)
{
  std::lock_guard<std::mutex> lock(results_mutex_);
  if (callback >= 0 && callback < 8) {
    transition_results_[callback] = result;
  }
}

NodeEntity::~NodeEntity()
{
  try {
    if (ctx && ctx->executor) {
      ctx->executor->remove_node(base(), true);
    }
  } catch (...) {
  }
  on_set_parameters.reset();
  lifecycle.reset();
  node.reset();
}

std::shared_ptr<ContextEntity> context_of(int64_t handle)
{
  return registry().get<ContextEntity>(handle, "context");
}

std::shared_ptr<NodeEntity> node_of(int64_t handle)
{
  return registry().get<NodeEntity>(handle, "node");
}

// ---------------------------------------------------------------------------
// Spinning
// ---------------------------------------------------------------------------

bool spin_until(ContextEntity & ctx, int64_t timeout_ms, const std::function<bool()> & ready)
{
  using Clock = std::chrono::steady_clock;
  if (ready()) {
    return true;
  }
  const auto deadline = Clock::now() + std::chrono::milliseconds(std::max<int64_t>(timeout_ms, 0));
  std::unique_lock<std::timed_mutex> lock(ctx.spin_mutex, std::defer_lock);
  if (timeout_ms < 0) {
    lock.lock();
  } else if (!lock.try_lock_until(deadline)) {
    return ready();
  }
  while (true) {
    if (ready()) {
      return true;
    }
    if (!ctx.context->is_valid()) {
      throw std::runtime_error("the ROS 2 context has been shut down");
    }
    auto now = Clock::now();
    if (timeout_ms >= 0 && now >= deadline) {
      return ready();
    }
    auto remaining = timeout_ms < 0 ? std::chrono::nanoseconds(-1) :
      std::chrono::duration_cast<std::chrono::nanoseconds>(deadline - now);
    ctx.executor->spin_once(remaining);
  }
}

void spin_ready(ContextEntity & ctx)
{
  std::unique_lock<std::timed_mutex> lock(ctx.spin_mutex, std::try_to_lock);
  if (lock.owns_lock() && ctx.context->is_valid()) {
    ctx.executor->spin_some(std::chrono::nanoseconds(0));
  }
}

namespace
{

// ---------------------------------------------------------------------------
// Publishers and subscriptions
// ---------------------------------------------------------------------------

struct PublisherEntity : Entity
{
  const char * kind() const override {return "publisher";}
  std::shared_ptr<NodeEntity> node;
  MessageTypePtr type;
  std::string topic;
  std::shared_ptr<rclcpp::GenericPublisher> publisher;
  std::shared_ptr<rclcpp::SerializationBase> serializer;

  // 0 published, 1 dropped because the (lifecycle) node is not active.
  int64_t publish(const Message & m)
  {
    if (m.type().name != type->name) {
      throw std::runtime_error(
              "the publisher on '" + topic + "' carries " + type->name + ", not " + m.type().name);
    }
    if (!node->publishing_enabled()) {
      return 1;
    }
    rclcpp::SerializedMessage serialized;
    serializer->serialize_message(m.data(), &serialized);
    publisher->publish(serialized);
    return 0;
  }
};

struct SubscriptionEntity : Entity
{
  const char * kind() const override {return "subscription";}
  std::shared_ptr<NodeEntity> node;
  MessageTypePtr type;
  std::string topic;
  std::shared_ptr<rclcpp::GenericSubscription> subscription;
  std::shared_ptr<rclcpp::SerializationBase> serializer;
  size_t capacity = 10;

  void deliver(MessagePtr m)
  {
    std::lock_guard<std::mutex> lock(inbox_mutex_);
    inbox_.push_back(std::move(m));
    while (inbox_.size() > capacity) {
      inbox_.pop_front();  // keep-last semantics, like the middleware's own queue
    }
  }

  MessagePtr take_one()
  {
    std::lock_guard<std::mutex> lock(inbox_mutex_);
    if (inbox_.empty()) {
      return nullptr;
    }
    MessagePtr m = std::move(inbox_.front());
    inbox_.pop_front();
    return m;
  }

  bool has_message()
  {
    std::lock_guard<std::mutex> lock(inbox_mutex_);
    return !inbox_.empty();
  }

private:
  std::mutex inbox_mutex_;
  std::deque<MessagePtr> inbox_;
};

int64_t create_publisher(
  int64_t node_handle, const char * topic, const char * type, const rclcpp::QoS & qos)
{
  auto node = node_of(node_handle);
  auto entity = std::make_shared<PublisherEntity>();
  entity->node = node;
  entity->type = message_type(str(type));
  entity->topic = str(topic);
  return create_under(node, entity, [&](PublisherEntity & e) {
             e.serializer = std::make_shared<rclcpp::SerializationBase>(e.type->cpp);
             e.publisher = rclcpp::create_generic_publisher(
               node->topics(), e.topic, e.type->name, qos);
           });
}

int64_t create_subscription(
  int64_t node_handle, const char * topic, const char * type, const rclcpp::QoS & qos)
{
  auto node = node_of(node_handle);
  auto entity = std::make_shared<SubscriptionEntity>();
  entity->node = node;
  entity->type = message_type(str(type));
  entity->topic = str(topic);
  const auto & profile = qos.get_rmw_qos_profile();
  entity->capacity = profile.history == RMW_QOS_POLICY_HISTORY_KEEP_ALL ? 10000 :
    profile.depth > 0 ? profile.depth : 10;
  return create_under(node, entity, [&](SubscriptionEntity & e) {
             e.serializer = std::make_shared<rclcpp::SerializationBase>(e.type->cpp);
             std::weak_ptr<SubscriptionEntity> weak = entity;
             auto callback = [weak](std::shared_ptr<rclcpp::SerializedMessage> serialized) {
                 auto sub = weak.lock();
                 if (!sub) {
                   return;
                 }
                 auto message = Message::create(sub->type);
                 try {
                   sub->serializer->deserialize_message(serialized.get(), message->data());
                 } catch (const std::exception & ex) {
                   RCLCPP_ERROR(
                     sub->node->logger(), "dropping an undecodable %s on '%s': %s",
                     sub->type->name.c_str(), sub->topic.c_str(), ex.what());
                   return;
                 }
                 sub->deliver(std::move(message));
                 Event event;
                 event.kind = EVENT_MESSAGE;
                 event.source = sub->handle;
                 event.node = sub->node->handle;
                 event.take = [weak]() -> MessagePtr {
                     auto s = weak.lock();
                     return s ? s->take_one() : nullptr;
                   };
                 sub->node->ctx->push(std::move(event));
               };
             e.subscription = rclcpp::create_generic_subscription(
               node->topics(), e.topic, e.type->name, qos, callback);
           });
}

std::shared_ptr<PublisherEntity> publisher_of(int64_t handle)
{
  return registry().get<PublisherEntity>(handle, "publisher");
}

std::shared_ptr<SubscriptionEntity> subscription_of(int64_t handle)
{
  return registry().get<SubscriptionEntity>(handle, "subscription");
}

// Waits up to `timeout_ms` for a message and takes it (nullptr on timeout).
MessagePtr wait_and_take(SubscriptionEntity & sub, int64_t timeout_ms)
{
  if (!sub.has_message()) {
    spin_ready(*sub.node->ctx);
  }
  if (!sub.has_message() && timeout_ms != 0) {
    spin_until(*sub.node->ctx, timeout_ms, [&]() {return sub.has_message();});
  }
  return sub.take_one();
}

// ---------------------------------------------------------------------------
// Timers
// ---------------------------------------------------------------------------

struct TimerEntity : Entity
{
  const char * kind() const override {return "timer";}
  std::shared_ptr<NodeEntity> node;
  rclcpp::TimerBase::SharedPtr timer;
  std::atomic<int64_t> fired{0};
};

// ---------------------------------------------------------------------------
// Services and clients, on rclcpp's type-erased bases
// ---------------------------------------------------------------------------

class GenericService : public rclcpp::ServiceBase
{
public:
  using Handler = std::function<void(std::shared_ptr<rmw_request_id_t>, MessagePtr)>;

  GenericService(
    std::shared_ptr<rcl_node_t> node_handle, const std::string & name,
    std::shared_ptr<const ServiceType> type, const rclcpp::QoS & qos, Handler handler)
  : rclcpp::ServiceBase(node_handle), type_(std::move(type)), handler_(std::move(handler))
  {
    rcl_service_options_t options = rcl_service_get_default_options();
    options.qos = qos.get_rmw_qos_profile();
    service_handle_ = std::shared_ptr<rcl_service_t>(
      new rcl_service_t, [handle = node_handle_](rcl_service_t * service) {
        if (rcl_service_fini(service, handle.get()) != RCL_RET_OK) {
          rcl_reset_error();
        }
        delete service;
      });
    *service_handle_ = rcl_get_zero_initialized_service();
    rcl_ret_t ret = rcl_service_init(
      service_handle_.get(), node_handle.get(), type_->cpp, name.c_str(), &options);
    if (ret != RCL_RET_OK) {
      rclcpp::exceptions::throw_from_rcl_error(ret, "could not create service '" + name + "'");
    }
  }

  std::shared_ptr<void> create_request() override
  {
    last_request_ = Message::create(type_->request);
    return erased(last_request_);
  }

  std::shared_ptr<rmw_request_id_t> create_request_header() override
  {
    return std::make_shared<rmw_request_id_t>();
  }

  // The executor creates a request, takes into it and hands it back here,
  // in that order and on one thread, so the Message wrapping it is the one
  // just created.
  void handle_request(
    std::shared_ptr<rmw_request_id_t> header, std::shared_ptr<void> request) override
  {
    MessagePtr message = std::move(last_request_);
    if (message && message->data() == request.get()) {
      handler_(std::move(header), std::move(message));
    }
  }

  void send_response(rmw_request_id_t & header, const Message & response)
  {
    rcl_ret_t ret = rcl_send_response(get_service_handle().get(), &header, response.data());
    if (ret != RCL_RET_OK) {
      rclcpp::exceptions::throw_from_rcl_error(ret, "failed to send the service response");
    }
  }

private:
  std::shared_ptr<const ServiceType> type_;
  Handler handler_;
  MessagePtr last_request_;
};

class GenericClient : public rclcpp::ClientBase
{
public:
  using Handler = std::function<void(int64_t, MessagePtr)>;

  GenericClient(
    rclcpp::node_interfaces::NodeBaseInterface * node_base,
    rclcpp::node_interfaces::NodeGraphInterface::SharedPtr node_graph, const std::string & name,
    std::shared_ptr<const ServiceType> type, const rclcpp::QoS & qos, Handler handler)
  : rclcpp::ClientBase(node_base, node_graph), type_(std::move(type)), handler_(std::move(handler))
  {
    rcl_client_options_t options = rcl_client_get_default_options();
    options.qos = qos.get_rmw_qos_profile();
    rcl_ret_t ret = rcl_client_init(
      get_client_handle().get(), get_rcl_node_handle(), type_->cpp, name.c_str(), &options);
    if (ret != RCL_RET_OK) {
      rclcpp::exceptions::throw_from_rcl_error(ret, "could not create client '" + name + "'");
    }
  }

  std::shared_ptr<void> create_response() override
  {
    last_response_ = Message::create(type_->response);
    return erased(last_response_);
  }

  std::shared_ptr<rmw_request_id_t> create_request_header() override
  {
    return std::make_shared<rmw_request_id_t>();
  }

  void handle_response(
    std::shared_ptr<rmw_request_id_t> header, std::shared_ptr<void> response) override
  {
    MessagePtr message = std::move(last_response_);
    if (message && message->data() == response.get()) {
      handler_(header->sequence_number, std::move(message));
    }
  }

  int64_t send(const Message & request)
  {
    int64_t sequence = 0;
    rcl_ret_t ret = rcl_send_request(get_client_handle().get(), request.data(), &sequence);
    if (ret != RCL_RET_OK) {
      rclcpp::exceptions::throw_from_rcl_error(ret, "failed to send the service request");
    }
    return sequence;
  }

private:
  std::shared_ptr<const ServiceType> type_;
  Handler handler_;
  MessagePtr last_response_;
};

struct ServiceEntity : Entity
{
  const char * kind() const override {return "service";}
  std::shared_ptr<NodeEntity> node;
  std::shared_ptr<const ServiceType> type;
  std::string name;
  std::shared_ptr<GenericService> service;

  struct Pending
  {
    std::shared_ptr<rmw_request_id_t> header;
    MessagePtr response;
    int64_t request_handle = 0;
    int64_t response_handle = 0;
  };
  std::mutex mutex;
  std::unordered_map<int64_t, Pending> pending;
  int64_t next_token = 1;
};

struct ClientEntity : Entity
{
  const char * kind() const override {return "client";}
  std::shared_ptr<NodeEntity> node;
  std::shared_ptr<const ServiceType> type;
  std::string name;
  std::shared_ptr<GenericClient> client;

  std::mutex mutex;
  std::unordered_map<int64_t, MessagePtr> responses;

  MessagePtr take(int64_t token)
  {
    std::lock_guard<std::mutex> lock(mutex);
    auto it = responses.find(token);
    if (it == responses.end()) {
      return nullptr;
    }
    MessagePtr m = std::move(it->second);
    responses.erase(it);
    return m;
  }

  bool has(int64_t token)
  {
    std::lock_guard<std::mutex> lock(mutex);
    return responses.count(token) != 0;
  }
};

std::shared_ptr<ServiceEntity> service_of(int64_t handle)
{
  return registry().get<ServiceEntity>(handle, "service");
}

std::shared_ptr<ClientEntity> client_of(int64_t handle)
{
  return registry().get<ClientEntity>(handle, "client");
}

// ---------------------------------------------------------------------------
// tf2
// ---------------------------------------------------------------------------

struct TfBufferEntity : Entity
{
  const char * kind() const override {return "tf buffer";}
  std::shared_ptr<NodeEntity> node;
  std::shared_ptr<tf2::BufferCore> core;
  // Declared after `core`: the listener holds a reference to the buffer and
  // must go first.
  std::shared_ptr<tf2_ros::TransformListener> listener;
};

struct TfBroadcasterEntity : Entity
{
  const char * kind() const override {return "tf broadcaster";}
  std::shared_ptr<NodeEntity> node;
  std::shared_ptr<tf2_ros::TransformBroadcaster> dynamic;
  std::shared_ptr<tf2_ros::StaticTransformBroadcaster> fixed;
};

geometry_msgs::msg::TransformStamped make_transform(
  const char * parent, const char * child, int64_t stamp_ns, const double * v)
{
  geometry_msgs::msg::TransformStamped t;
  t.header.frame_id = str(parent);
  t.child_frame_id = str(child);
  split_stamp(stamp_ns, t.header.stamp.sec, t.header.stamp.nanosec);
  t.transform.translation.x = v[0];
  t.transform.translation.y = v[1];
  t.transform.translation.z = v[2];
  t.transform.rotation.x = v[3];
  t.transform.rotation.y = v[4];
  t.transform.rotation.z = v[5];
  t.transform.rotation.w = v[6];
  return t;
}

// ---------------------------------------------------------------------------
// Parameters
// ---------------------------------------------------------------------------

// Parameter value kinds IRIS passes: the value slot is read according to it.
constexpr int64_t PARAM_JSON = 0;    // value: const char * JSON (arrays, any type)
constexpr int64_t PARAM_BOOL = 1;    // value: 0 / 1
constexpr int64_t PARAM_I64 = 2;     // value: the integer
constexpr int64_t PARAM_F64 = 3;     // value: the double's bits
constexpr int64_t PARAM_STR = 4;     // value: const char *

rclcpp::ParameterValue parameter_from_json(const std::string & text)
{
  Json j;
  std::string error;
  if (!parse_json(text, j, error)) {
    throw std::runtime_error("invalid parameter JSON: " + error);
  }
  switch (j.kind) {
    case Json::Kind::Bool: return rclcpp::ParameterValue(j.boolean);
    case Json::Kind::Int: return rclcpp::ParameterValue(j.int_value);
    case Json::Kind::Float: return rclcpp::ParameterValue(j.float_value);
    case Json::Kind::String: return rclcpp::ParameterValue(j.text);
    case Json::Kind::Array: {
        if (j.items.empty()) {
          throw std::runtime_error("an empty array has no parameter type; give at least one element");
        }
        bool all_bool = true, all_int = true, all_number = true, all_string = true;
        for (const auto & item : j.items) {
          all_bool &= item.kind == Json::Kind::Bool;
          all_int &= item.kind == Json::Kind::Int;
          all_number &= item.kind == Json::Kind::Int || item.kind == Json::Kind::Float;
          all_string &= item.kind == Json::Kind::String;
        }
        if (all_bool) {
          std::vector<bool> v;
          for (const auto & item : j.items) {
            v.push_back(item.boolean);
          }
          return rclcpp::ParameterValue(v);
        }
        if (all_int) {
          std::vector<int64_t> v;
          for (const auto & item : j.items) {
            v.push_back(item.int_value);
          }
          return rclcpp::ParameterValue(v);
        }
        if (all_number) {
          std::vector<double> v;
          for (const auto & item : j.items) {
            v.push_back(item.kind == Json::Kind::Int ? static_cast<double>(item.int_value) :
              item.float_value);
          }
          return rclcpp::ParameterValue(v);
        }
        if (all_string) {
          std::vector<std::string> v;
          for (const auto & item : j.items) {
            v.push_back(item.text);
          }
          return rclcpp::ParameterValue(v);
        }
        throw std::runtime_error("a parameter array must hold one type (bool, integer, double or string)");
      }
    default:
      break;
  }
  throw std::runtime_error(std::string("a parameter cannot be ") + j.kind_name());
}

rclcpp::ParameterValue parameter_value(int64_t kind, int64_t value)
{
  switch (kind) {
    case PARAM_BOOL: return rclcpp::ParameterValue(value != 0);
    case PARAM_I64: return rclcpp::ParameterValue(static_cast<int64_t>(value));
    case PARAM_F64: return rclcpp::ParameterValue(f64_of(value));
    case PARAM_STR: return rclcpp::ParameterValue(std::string(str_slot(value)));
    case PARAM_JSON: return parameter_from_json(str_slot(value));
  }
  throw std::runtime_error("unknown parameter kind " + std::to_string(kind));
}

std::string parameter_json(const rclcpp::ParameterValue & v)
{
  std::string out;
  auto list = [&out](const auto & items, auto && write) {
      out.push_back('[');
      bool first = true;
      for (const auto & item : items) {
        if (!first) {
          out.push_back(',');
        }
        first = false;
        write(item);
      }
      out.push_back(']');
    };
  switch (v.get_type()) {
    case rclcpp::ParameterType::PARAMETER_NOT_SET: out = "null"; break;
    case rclcpp::ParameterType::PARAMETER_BOOL: out = v.get<bool>() ? "true" : "false"; break;
    case rclcpp::ParameterType::PARAMETER_INTEGER: out = std::to_string(v.get<int64_t>()); break;
    case rclcpp::ParameterType::PARAMETER_DOUBLE: write_json_double(out, v.get<double>()); break;
    case rclcpp::ParameterType::PARAMETER_STRING: write_json_string(out, v.get<std::string>()); break;
    case rclcpp::ParameterType::PARAMETER_BYTE_ARRAY:
      list(v.get<std::vector<uint8_t>>(), [&](uint8_t b) {out += std::to_string(b);});
      break;
    case rclcpp::ParameterType::PARAMETER_BOOL_ARRAY:
      list(v.get<std::vector<bool>>(), [&](bool b) {out += b ? "true" : "false";});
      break;
    case rclcpp::ParameterType::PARAMETER_INTEGER_ARRAY:
      list(v.get<std::vector<int64_t>>(), [&](int64_t i) {out += std::to_string(i);});
      break;
    case rclcpp::ParameterType::PARAMETER_DOUBLE_ARRAY:
      list(v.get<std::vector<double>>(), [&](double d) {write_json_double(out, d);});
      break;
    case rclcpp::ParameterType::PARAMETER_STRING_ARRAY:
      list(v.get<std::vector<std::string>>(), [&](const std::string & s) {write_json_string(out, s);});
      break;
  }
  return out;
}

// {"description": "...", "read_only": true, "dynamic_typing": false,
//  "additional_constraints": "...", "min": 0, "max": 10, "step": 1}
rcl_interfaces::msg::ParameterDescriptor parameter_descriptor(
  const char * text, const rclcpp::ParameterValue & value)
{
  rcl_interfaces::msg::ParameterDescriptor d;
  if (!text || !*text) {
    return d;
  }
  Json j;
  std::string error;
  if (!parse_json(text, j, error) || j.kind != Json::Kind::Object) {
    throw std::runtime_error("a parameter descriptor must be a JSON object" + (error.empty() ? "" : ": " + error));
  }
  auto number = [](const Json & n) {
      return n.kind == Json::Kind::Int ? static_cast<double>(n.int_value) : n.float_value;
    };
  const Json * min = nullptr;
  const Json * max = nullptr;
  const Json * step = nullptr;
  for (const auto & field : j.fields) {
    const std::string & key = field.first;
    const Json & v = field.second;
    if (key == "description" && v.kind == Json::Kind::String) {
      d.description = v.text;
    } else if (key == "additional_constraints" && v.kind == Json::Kind::String) {
      d.additional_constraints = v.text;
    } else if (key == "read_only" && v.kind == Json::Kind::Bool) {
      d.read_only = v.boolean;
    } else if (key == "dynamic_typing" && v.kind == Json::Kind::Bool) {
      d.dynamic_typing = v.boolean;
    } else if (key == "min" && v.is_number()) {
      min = &v;
    } else if (key == "max" && v.is_number()) {
      max = &v;
    } else if (key == "step" && v.is_number()) {
      step = &v;
    } else {
      throw std::runtime_error("unknown or mistyped parameter descriptor field '" + key + "'");
    }
  }
  if (min || max || step) {
    if (value.get_type() == rclcpp::ParameterType::PARAMETER_INTEGER) {
      rcl_interfaces::msg::IntegerRange range;
      range.from_value = min ? static_cast<int64_t>(number(*min)) : std::numeric_limits<int64_t>::min();
      range.to_value = max ? static_cast<int64_t>(number(*max)) : std::numeric_limits<int64_t>::max();
      range.step = step ? static_cast<uint64_t>(number(*step)) : 0;
      d.integer_range.push_back(range);
    } else if (value.get_type() == rclcpp::ParameterType::PARAMETER_DOUBLE) {
      rcl_interfaces::msg::FloatingPointRange range;
      range.from_value = min ? number(*min) : -std::numeric_limits<double>::max();
      range.to_value = max ? number(*max) : std::numeric_limits<double>::max();
      range.step = step ? number(*step) : 0.0;
      d.floating_point_range.push_back(range);
    } else {
      throw std::runtime_error("min/max/step apply only to integer and double parameters");
    }
  }
  return d;
}

}  // namespace

}  // namespace iris_ros2

using namespace iris_ros2;

// ===========================================================================
// Status and results
// ===========================================================================

IRIS_ROS2_API int64_t iris_ros2_version()
{
  return BRIDGE_ABI_VERSION;
}

IRIS_ROS2_API int64_t iris_ros2_status()
{
  return last_status();
}

IRIS_ROS2_API int64_t iris_ros2_last_error()
{
  return set_result(last_error_text());
}

// Copies the result string into an IRIS out-cell of `capacity` bytes
// (NUL-terminated, truncated if it does not fit). Returns the bytes copied.
IRIS_ROS2_API int64_t iris_ros2_result_copy(int64_t cell, int64_t capacity)
{
  if (cell == 0 || capacity <= 0) {
    return 0;
  }
  const std::string & text = result_buffer();
  size_t n = std::min(text.size(), static_cast<size_t>(capacity - 1));
  char * out = reinterpret_cast<char *>(static_cast<intptr_t>(cell));
  std::memcpy(out, text.data(), n);
  out[n] = '\0';
  return static_cast<int64_t>(n);
}

// ===========================================================================
// Contexts
// ===========================================================================

// `args`: ROS arguments for the whole context ("--ros-args --log-level debug").
// `domain_id`: the DDS domain, or negative for ROS_DOMAIN_ID / the default.
IRIS_ROS2_API int64_t iris_ros2_context_create(const char * args, int64_t domain_id)
{
  return guarded<int64_t>(0, [&]() -> int64_t {
             ensure_ament_prefix_path();
             auto entity = std::make_shared<ContextEntity>();
             entity->context = std::make_shared<rclcpp::Context>();
             rclcpp::InitOptions options;
             if (domain_id >= 0) {
               options.set_domain_id(static_cast<size_t>(domain_id));
             }
             std::vector<std::string> words = split_args(args);
             words.insert(words.begin(), "iris");
             std::vector<const char *> argv;
             for (const auto & w : words) {
               argv.push_back(w.c_str());
             }
             entity->context->init(static_cast<int>(argv.size()), argv.data(), options);
             rclcpp::ExecutorOptions executor_options;
             executor_options.context = entity->context;
             entity->executor =
             std::make_shared<rclcpp::executors::SingleThreadedExecutor>(executor_options);
             return registry().add(entity);
           });
}

IRIS_ROS2_API int64_t iris_ros2_context_destroy(int64_t ctx)
{
  return guarded([&]() -> int64_t {
             context_of(ctx);
             registry().destroy(ctx);
             return 0;
           });
}

IRIS_ROS2_API int64_t iris_ros2_context_ok(int64_t ctx)
{
  return guarded([&]() -> int64_t {return context_of(ctx)->context->is_valid() ? 1 : 0;});
}

// ===========================================================================
// Nodes
// ===========================================================================

// `args`: ROS arguments for this node only ("--ros-args -p use_sim_time:=true
// -r __ns:=/robot"). `flags`: 1 lifecycle node, 2 allow undeclared
// parameters, 4 declare parameters from overrides, 8 no parameter services,
// 16 no dedicated /clock thread.
IRIS_ROS2_API int64_t iris_ros2_node_create(
  int64_t ctx_handle, const char * name, const char * ns, const char * args, int64_t flags)
{
  return guarded<int64_t>(0, [&]() -> int64_t {
             auto ctx = context_of(ctx_handle);
             rclcpp::NodeOptions options;
             options.context(ctx->context);
             auto words = split_args(args);
             if (!words.empty()) {
               options.arguments(words);
             }
             options.allow_undeclared_parameters((flags & NODE_ALLOW_UNDECLARED) != 0);
             options.automatically_declare_parameters_from_overrides((flags & NODE_AUTO_DECLARE) != 0);
             options.start_parameter_services((flags & NODE_NO_PARAMETER_SERVICES) == 0);
             options.use_clock_thread((flags & NODE_NO_CLOCK_THREAD) == 0);

             auto entity = std::make_shared<NodeEntity>();
             entity->ctx = ctx;
             if (flags & NODE_LIFECYCLE) {
               entity->lifecycle =
               std::make_shared<rclcpp_lifecycle::LifecycleNode>(str(name), str(ns), options);
             } else {
               entity->node = std::make_shared<rclcpp::Node>(str(name), str(ns), options);
             }
             int64_t handle = registry().add(entity, ctx_handle);
             try {
               std::weak_ptr<NodeEntity> weak = entity;
               entity->on_set_parameters = entity->parameters()->add_on_set_parameters_callback(
                 [weak](const std::vector<rclcpp::Parameter> & parameters) {
                   rcl_interfaces::msg::SetParametersResult result;
                   result.successful = true;
                   auto e = weak.lock();
                   if (e && t_internal_parameter_change == 0) {
                     for (const auto & p : parameters) {
                       Event event;
                       event.kind = EVENT_PARAMETER;
                       event.source = e->handle;
                       event.node = e->handle;
                       event.aux = static_cast<int64_t>(p.get_type());
                       event.text = p.get_name();
                       e->ctx->push(std::move(event));
                     }
                   }
                   return result;
                 });
               if (entity->lifecycle) {
                 using CallbackReturn =
                 rclcpp_lifecycle::node_interfaces::LifecycleNodeInterface::CallbackReturn;
                 auto callback = [weak](int which, const char * label) {
                     return [weak, which, label](const rclcpp_lifecycle::State &) -> CallbackReturn {
                              auto e = weak.lock();
                              if (!e) {
                                return CallbackReturn::SUCCESS;
                              }
                              int result = e->transition_result(which);
                              Event event;
                              event.kind = EVENT_TRANSITION;
                              event.source = e->handle;
                              event.node = e->handle;
                              event.id = which;
                              event.aux = result;
                              event.text = label;
                              e->ctx->push(std::move(event));
                              return result == 1 ? CallbackReturn::FAILURE :
                              result == 2 ? CallbackReturn::ERROR : CallbackReturn::SUCCESS;
                            };
                   };
                 entity->lifecycle->register_on_configure(callback(1, "configure"));
                 entity->lifecycle->register_on_cleanup(callback(2, "cleanup"));
                 entity->lifecycle->register_on_activate(callback(3, "activate"));
                 entity->lifecycle->register_on_deactivate(callback(4, "deactivate"));
                 entity->lifecycle->register_on_shutdown(callback(5, "shutdown"));
                 entity->lifecycle->register_on_error(callback(6, "error"));
               }
               ctx->executor->add_node(entity->base());
             } catch (...) {
               registry().destroy(handle);
               throw;
             }
             return handle;
           });
}

IRIS_ROS2_API int64_t iris_ros2_node_destroy(int64_t node)
{
  return guarded([&]() -> int64_t {
             node_of(node);
             registry().destroy(node);
             return 0;
           });
}

IRIS_ROS2_API int64_t iris_ros2_node_name(int64_t node)
{
  return guarded([&]() -> int64_t {
             return set_result(node_of(node)->base()->get_fully_qualified_name());
           });
}

IRIS_ROS2_API int64_t iris_ros2_node_is_lifecycle(int64_t node)
{
  return guarded([&]() -> int64_t {return node_of(node)->lifecycle ? 1 : 0;});
}

// The node's clock: ROS time, which follows /clock when use_sim_time is set.
IRIS_ROS2_API int64_t iris_ros2_node_now(int64_t node)
{
  return guarded<int64_t>(std::numeric_limits<int64_t>::min(), [&]() -> int64_t {
             return node_of(node)->clock()->now().nanoseconds();
           });
}

// 1 when the node's clock is driven by /clock (use_sim_time).
IRIS_ROS2_API int64_t iris_ros2_node_sim_time(int64_t node)
{
  return guarded([&]() -> int64_t {return node_of(node)->clock()->ros_time_is_active() ? 1 : 0;});
}

// Severity follows rcutils: 10 debug, 20 info, 30 warn, 40 error, 50 fatal.
// Goes to the console, the log files and /rosout like any ROS 2 log line.
IRIS_ROS2_API int64_t iris_ros2_node_log(int64_t node, int64_t severity, const char * text)
{
  return guarded([&]() -> int64_t {
             auto logger = node_of(node)->logger();
             const char * t = str(text);
             if (severity >= 50) {
               RCLCPP_FATAL(logger, "%s", t);
             } else if (severity >= 40) {
               RCLCPP_ERROR(logger, "%s", t);
             } else if (severity >= 30) {
               RCLCPP_WARN(logger, "%s", t);
             } else if (severity >= 20) {
               RCLCPP_INFO(logger, "%s", t);
             } else {
               RCLCPP_DEBUG(logger, "%s", t);
             }
             return 0;
           });
}

// The ROS graph as JSON: what 0 = topics {name: [types]}, 1 = services
// {name: [types]}, 2 = nodes [fully qualified names].
IRIS_ROS2_API int64_t iris_ros2_graph(int64_t node, int64_t what)
{
  return guarded([&]() -> int64_t {
             auto graph = node_of(node)->graph();
             std::string out;
             if (what == 2) {
               out.push_back('[');
               bool first = true;
               for (const auto & name : graph->get_node_names()) {
                 if (!first) {
                   out.push_back(',');
                 }
                 first = false;
                 write_json_string(out, name);
               }
               out.push_back(']');
               return set_result(out);
             }
             auto names = what == 1 ? graph->get_service_names_and_types() :
             graph->get_topic_names_and_types();
             out.push_back('{');
             bool first = true;
             for (const auto & entry : names) {
               if (!first) {
                 out.push_back(',');
               }
               first = false;
               write_json_string(out, entry.first);
               out += ":[";
               for (size_t i = 0; i < entry.second.size(); ++i) {
                 if (i) {
                   out.push_back(',');
                 }
                 write_json_string(out, entry.second[i]);
               }
               out.push_back(']');
             }
             out.push_back('}');
             return set_result(out);
           });
}

IRIS_ROS2_API int64_t iris_ros2_count_publishers(int64_t node, const char * topic)
{
  return guarded([&]() -> int64_t {
             auto n = node_of(node);
             auto name = n->topics()->resolve_topic_name(str(topic));
             return static_cast<int64_t>(n->graph()->count_publishers(name));
           });
}

IRIS_ROS2_API int64_t iris_ros2_count_subscribers(int64_t node, const char * topic)
{
  return guarded([&]() -> int64_t {
             auto n = node_of(node);
             auto name = n->topics()->resolve_topic_name(str(topic));
             return static_cast<int64_t>(n->graph()->count_subscribers(name));
           });
}

// ===========================================================================
// The executor
// ===========================================================================

// Runs the context's executor until an event is ready or `timeout_ms`
// passes (negative: wait indefinitely; 0: only what is ready now). Writes the
// event into `cell` (6 int64 slots: kind, source, message, id, aux, node) and
// returns its kind, or 0 when nothing happened.
//
// Message handles delivered by the previous call are released first: an
// event's message lives until the next spin_once on the same context.
IRIS_ROS2_API int64_t iris_ros2_spin_once(int64_t ctx_handle, int64_t timeout_ms, int64_t cell)
{
  return guarded([&]() -> int64_t {
             auto ctx = context_of(ctx_handle);
             ctx->release_scope(0);
             Event event;
             bool got = false;
             spin_until(*ctx, timeout_ms, [&]() {
               if (!got) {
                 got = ctx->pop(event);
               }
               return got;
             });
             if (!got) {
               ctx->current = Event();
               if (cell) {
                 std::fill(cell_i64(cell), cell_i64(cell) + 6, 0);
               }
               return 0;
             }
             int64_t message = event.message;
             if (!message && event.payload) {
               message = ctx->keep_scoped(event.payload, 0);
             }
             if (cell) {
               int64_t * out = cell_i64(cell);
               out[0] = event.kind;
               out[1] = event.source;
               out[2] = message;
               out[3] = event.id;
               out[4] = event.aux;
               out[5] = event.node;
             }
             event.payload.reset();
             event.message = message;
             ctx->current = std::move(event);
             return ctx->current.kind;
           });
}

// Executes the work that is ready now (bounded by `max_ms`), leaving its
// events queued. Returns the number of queued events.
IRIS_ROS2_API int64_t iris_ros2_spin_some(int64_t ctx_handle, int64_t max_ms)
{
  return guarded([&]() -> int64_t {
             auto ctx = context_of(ctx_handle);
             std::unique_lock<std::timed_mutex> lock(ctx->spin_mutex, std::try_to_lock);
             if (lock.owns_lock()) {
               ctx->executor->spin_some(std::chrono::milliseconds(std::max<int64_t>(max_ms, 0)));
             }
             return static_cast<int64_t>(ctx->pending());
           });
}

IRIS_ROS2_API int64_t iris_ros2_pending(int64_t ctx_handle)
{
  return guarded([&]() -> int64_t {return static_cast<int64_t>(context_of(ctx_handle)->pending());});
}

IRIS_ROS2_API int64_t iris_ros2_dropped_events(int64_t ctx_handle)
{
  return guarded([&]() -> int64_t {return static_cast<int64_t>(context_of(ctx_handle)->dropped());});
}

// The text of the last delivered event: a parameter's name, a transition's
// label.
IRIS_ROS2_API int64_t iris_ros2_event_text(int64_t ctx_handle)
{
  return guarded([&]() -> int64_t {return set_result(context_of(ctx_handle)->current.text);});
}

// ===========================================================================
// Messages
// ===========================================================================

IRIS_ROS2_API int64_t iris_ros2_msg_new(const char * type)
{
  return guarded<int64_t>(0, [&]() -> int64_t {
             return register_message(Message::create(message_type(str(type))));
           });
}

IRIS_ROS2_API int64_t iris_ros2_msg_from_json(const char * type, const char * json)
{
  return guarded<int64_t>(0, [&]() -> int64_t {
             auto m = Message::create(message_type(str(type)));
             set_json(*m, "", str(json));
             return register_message(m);
           });
}

IRIS_ROS2_API int64_t iris_ros2_msg_free(int64_t msg)
{
  return guarded([&]() -> int64_t {
             message_of(msg);
             registry().destroy(msg);
             return 0;
           });
}

IRIS_ROS2_API int64_t iris_ros2_msg_clone(int64_t msg)
{
  return guarded<int64_t>(0, [&]() -> int64_t {return register_message(message_of(msg)->clone());});
}

IRIS_ROS2_API int64_t iris_ros2_msg_type(int64_t msg)
{
  return guarded([&]() -> int64_t {return set_result(message_of(msg)->type().name);});
}

IRIS_ROS2_API int64_t iris_ros2_msg_json(int64_t msg, const char * path)
{
  return guarded([&]() -> int64_t {return set_result(get_json(*message_of(msg), str(path)));});
}

IRIS_ROS2_API int64_t iris_ros2_msg_set_json(int64_t msg, const char * path, const char * json)
{
  return guarded([&]() -> int64_t {
             set_json(*message_of(msg), str(path), str(json));
             return 0;
           });
}

// NaN on failure; `iris_ros2_status` tells a failure from a NaN field.
IRIS_ROS2_API double iris_ros2_msg_get_f64(int64_t msg, const char * path)
{
  return guarded<double>(std::numeric_limits<double>::quiet_NaN(), [&]() -> double {
             return get_f64(*message_of(msg), str(path));
           });
}

// INT64_MIN on failure; `iris_ros2_status` tells a failure from that value.
IRIS_ROS2_API int64_t iris_ros2_msg_get_i64(int64_t msg, const char * path)
{
  return guarded<int64_t>(std::numeric_limits<int64_t>::min(), [&]() -> int64_t {
             return get_i64(*message_of(msg), str(path));
           });
}

IRIS_ROS2_API int64_t iris_ros2_msg_get_bool(int64_t msg, const char * path)
{
  return guarded([&]() -> int64_t {return get_bool(*message_of(msg), str(path)) ? 1 : 0;});
}

IRIS_ROS2_API int64_t iris_ros2_msg_get_str(int64_t msg, const char * path)
{
  return guarded([&]() -> int64_t {return set_result(get_str(*message_of(msg), str(path)));});
}

IRIS_ROS2_API int64_t iris_ros2_msg_set_f64(int64_t msg, const char * path, int64_t bits)
{
  return guarded([&]() -> int64_t {
             set_f64(*message_of(msg), str(path), f64_of(bits));
             return 0;
           });
}

IRIS_ROS2_API int64_t iris_ros2_msg_set_i64(int64_t msg, const char * path, int64_t value)
{
  return guarded([&]() -> int64_t {
             set_i64(*message_of(msg), str(path), value);
             return 0;
           });
}

IRIS_ROS2_API int64_t iris_ros2_msg_set_bool(int64_t msg, const char * path, int64_t value)
{
  return guarded([&]() -> int64_t {
             set_bool(*message_of(msg), str(path), value != 0);
             return 0;
           });
}

IRIS_ROS2_API int64_t iris_ros2_msg_set_str(int64_t msg, const char * path, const char * value)
{
  return guarded([&]() -> int64_t {
             set_str(*message_of(msg), str(path), str(value));
             return 0;
           });
}

IRIS_ROS2_API int64_t iris_ros2_msg_len(int64_t msg, const char * path)
{
  return guarded([&]() -> int64_t {return length_of(*message_of(msg), str(path));});
}

IRIS_ROS2_API int64_t iris_ros2_msg_resize(int64_t msg, const char * path, int64_t size)
{
  return guarded([&]() -> int64_t {
             if (size < 0) {
               throw std::runtime_error("negative size");
             }
             resize(*message_of(msg), str(path), static_cast<size_t>(size));
             return 0;
           });
}

IRIS_ROS2_API int64_t iris_ros2_msg_has(int64_t msg, const char * path)
{
  return guarded([&]() -> int64_t {return has_path(*message_of(msg), str(path)) ? 1 : 0;});
}

// Copies up to `max` elements of a numeric array into `cell` (as f64);
// returns the array's full length.
IRIS_ROS2_API int64_t iris_ros2_msg_get_f64s(int64_t msg, const char * path, int64_t cell, int64_t max)
{
  return guarded([&]() -> int64_t {
             return static_cast<int64_t>(get_f64s(
               *message_of(msg), str(path), max > 0 ? cell_f64(cell) : nullptr,
               static_cast<size_t>(std::max<int64_t>(max, 0))));
           });
}

IRIS_ROS2_API int64_t iris_ros2_msg_set_f64s(int64_t msg, const char * path, int64_t cell, int64_t count)
{
  return guarded([&]() -> int64_t {
             if (count < 0) {
               throw std::runtime_error("negative count");
             }
             set_f64s(
               *message_of(msg), str(path), count > 0 ? cell_f64(cell) : nullptr,
               static_cast<size_t>(count));
             return 0;
           });
}

namespace
{

std::vector<std::string> split_paths(const char * text)
{
  std::vector<std::string> out;
  std::string current;
  for (const char * p = str(text); ; ++p) {
    if (*p == ',' || *p == '\0') {
      if (!current.empty()) {
        out.push_back(current);
      }
      current.clear();
      if (*p == '\0') {
        break;
      }
    } else if (*p != ' ') {
      current.push_back(*p);
    }
  }
  return out;
}

}  // namespace

// Sets the f64 fields named by the comma-separated `paths` from the `count`
// doubles in `cell` -- a whole Twist or Pose in one call.
IRIS_ROS2_API int64_t iris_ros2_msg_set_f64_fields(int64_t msg, const char * paths, int64_t cell, int64_t count)
{
  return guarded([&]() -> int64_t {
             auto m = message_of(msg);
             auto names = split_paths(paths);
             if (static_cast<int64_t>(names.size()) != count) {
               throw std::runtime_error(
                 "msg_set_f64_fields: " + std::to_string(names.size()) + " paths for " +
                 std::to_string(count) + " values");
             }
             const double * values = count > 0 ? cell_f64(cell) : nullptr;
             for (size_t i = 0; i < names.size(); ++i) {
               set_f64(*m, names[i], values[i]);
             }
             return 0;
           });
}

// Reads the f64 fields named by the comma-separated `paths` into `cell`.
IRIS_ROS2_API int64_t iris_ros2_msg_get_f64_fields(int64_t msg, const char * paths, int64_t cell, int64_t count)
{
  return guarded([&]() -> int64_t {
             auto m = message_of(msg);
             auto names = split_paths(paths);
             if (static_cast<int64_t>(names.size()) != count) {
               throw std::runtime_error(
                 "msg_get_f64_fields: " + std::to_string(names.size()) + " paths for " +
                 std::to_string(count) + " values");
             }
             double * out = count > 0 ? cell_f64(cell) : nullptr;
             for (size_t i = 0; i < names.size(); ++i) {
               out[i] = get_f64(*m, names[i]);
             }
             return 0;
           });
}

// Sets a builtin_interfaces/Time field (e.g. "header.stamp") from nanoseconds.
IRIS_ROS2_API int64_t iris_ros2_msg_set_stamp(int64_t msg, const char * path, int64_t ns)
{
  return guarded([&]() -> int64_t {
             auto m = message_of(msg);
             std::string base = str(path);
             int32_t sec;
             uint32_t nanosec;
             split_stamp(ns, sec, nanosec);
             set_i64(*m, base.empty() ? "sec" : base + ".sec", sec);
             set_i64(*m, base.empty() ? "nanosec" : base + ".nanosec", nanosec);
             return 0;
           });
}

IRIS_ROS2_API int64_t iris_ros2_msg_get_stamp(int64_t msg, const char * path)
{
  return guarded<int64_t>(std::numeric_limits<int64_t>::min(), [&]() -> int64_t {
             auto m = message_of(msg);
             std::string base = str(path);
             return join_stamp(
               get_i64(*m, base.empty() ? "sec" : base + ".sec"),
               get_i64(*m, base.empty() ? "nanosec" : base + ".nanosec"));
           });
}

// The type's fields as JSON: {"type": ..., "fields": [{"name", "type", ...}]}.
IRIS_ROS2_API int64_t iris_ros2_msg_schema(const char * type)
{
  return guarded([&]() -> int64_t {return set_result(schema_json(*message_type(str(type))));});
}

// ===========================================================================
// Publishers and subscriptions
// ===========================================================================

IRIS_ROS2_API int64_t iris_ros2_publisher_create(
  int64_t node, const char * topic, const char * type,
  int64_t history, int64_t depth, int64_t reliability, int64_t durability)
{
  return guarded<int64_t>(0, [&]() -> int64_t {
             return create_publisher(node, topic, type, make_qos(history, depth, reliability, durability));
           });
}

IRIS_ROS2_API int64_t iris_ros2_publisher_destroy(int64_t pub)
{
  return guarded([&]() -> int64_t {
             publisher_of(pub);
             registry().destroy(pub);
             return 0;
           });
}

// 0 published; 1 dropped because the lifecycle node is not active.
IRIS_ROS2_API int64_t iris_ros2_publish(int64_t pub, int64_t msg)
{
  return guarded([&]() -> int64_t {return publisher_of(pub)->publish(*message_of(msg));});
}

IRIS_ROS2_API int64_t iris_ros2_publish_json(int64_t pub, const char * json)
{
  return guarded([&]() -> int64_t {
             auto p = publisher_of(pub);
             auto m = Message::create(p->type);
             set_json(*m, "", str(json));
             return p->publish(*m);
           });
}

IRIS_ROS2_API int64_t iris_ros2_publisher_new_msg(int64_t pub)
{
  return guarded<int64_t>(0, [&]() -> int64_t {
             return register_message(Message::create(publisher_of(pub)->type));
           });
}

// The number of subscriptions matched with this publisher.
IRIS_ROS2_API int64_t iris_ros2_publisher_matches(int64_t pub)
{
  return guarded([&]() -> int64_t {
             return static_cast<int64_t>(publisher_of(pub)->publisher->get_subscription_count());
           });
}

IRIS_ROS2_API int64_t iris_ros2_subscription_create(
  int64_t node, const char * topic, const char * type,
  int64_t history, int64_t depth, int64_t reliability, int64_t durability)
{
  return guarded<int64_t>(0, [&]() -> int64_t {
             return create_subscription(node, topic, type, make_qos(history, depth, reliability, durability));
           });
}

IRIS_ROS2_API int64_t iris_ros2_subscription_destroy(int64_t sub)
{
  return guarded([&]() -> int64_t {
             subscription_of(sub);
             registry().destroy(sub);
             return 0;
           });
}

// The oldest received message, or 0 if none has arrived. The handle lives
// until the next take on this subscription or the next spin_once.
IRIS_ROS2_API int64_t iris_ros2_take(int64_t sub_handle)
{
  return guarded([&]() -> int64_t {
             auto sub = subscription_of(sub_handle);
             auto ctx = sub->node->ctx;
             ctx->release_scope(sub_handle);
             auto m = wait_and_take(*sub, 0);
             return m ? ctx->keep_scoped(m, sub_handle) : 0;
           });
}

// Runs the executor until this subscription has a message (1) or
// `timeout_ms` passes (0).
IRIS_ROS2_API int64_t iris_ros2_wait(int64_t sub_handle, int64_t timeout_ms)
{
  return guarded([&]() -> int64_t {
             auto sub = subscription_of(sub_handle);
             if (sub->has_message()) {
               return 1;
             }
             spin_ready(*sub->node->ctx);
             return spin_until(*sub->node->ctx, timeout_ms, [&]() {return sub->has_message();}) ? 1 : 0;
           });
}

// The index of the first of `count` subscriptions (their handles in `cell`)
// holding a message, running the executor up to `timeout_ms` for one to
// arrive; -1 on timeout. The messages stay queued for take.
IRIS_ROS2_API int64_t iris_ros2_wait_any(int64_t cell, int64_t count, int64_t timeout_ms)
{
  return guarded([&]() -> int64_t {
             if (count <= 0) {
               return -1;
             }
             const int64_t * handles = cell_i64(cell);
             std::vector<std::shared_ptr<SubscriptionEntity>> subs;
             for (int64_t i = 0; i < count; ++i) {
               subs.push_back(subscription_of(handles[i]));
             }
             auto ctx = subs[0]->node->ctx;
             for (const auto & s : subs) {
               if (s->node->ctx != ctx) {
                 throw std::runtime_error("wait_any: the subscriptions belong to different contexts");
               }
             }
             int64_t found = -1;
             auto ready = [&]() {
                 for (size_t i = 0; i < subs.size(); ++i) {
                   if (subs[i]->has_message()) {
                     found = static_cast<int64_t>(i);
                     return true;
                   }
                 }
                 return false;
               };
             if (!ready()) {
               spin_ready(*ctx);
               if (!ready()) {
                 spin_until(*ctx, timeout_ms, ready);
               }
             }
             return found;
           });
}

// The number of publishers matched with this subscription.
IRIS_ROS2_API int64_t iris_ros2_subscription_matches(int64_t sub)
{
  return guarded([&]() -> int64_t {
             return static_cast<int64_t>(subscription_of(sub)->subscription->get_publisher_count());
           });
}

// ===========================================================================
// Timers
// ===========================================================================

// `wall` = 0: the node's clock (ROS time; sim time when use_sim_time is set).
IRIS_ROS2_API int64_t iris_ros2_timer_create(int64_t node_handle, int64_t period_ns, int64_t wall)
{
  return guarded<int64_t>(0, [&]() -> int64_t {
             if (period_ns <= 0) {
               throw std::runtime_error("a timer period must be positive");
             }
             auto node = node_of(node_handle);
             auto entity = std::make_shared<TimerEntity>();
             entity->node = node;
             return create_under(node, entity, [&](TimerEntity & e) {
               std::weak_ptr<TimerEntity> weak = entity;
               auto callback = [weak]() {
                   auto t = weak.lock();
                   if (!t) {
                     return;
                   }
                   Event event;
                   event.kind = EVENT_TIMER;
                   event.source = t->handle;
                   event.node = t->node->handle;
                   event.id = ++t->fired;
                   t->node->ctx->push(std::move(event));
                 };
               // Moved, not passed as an lvalue: create_timer forwards its
               // callback type into GenericTimer, which would then hold a
               // reference to this local.
               if (wall) {
                 e.timer = rclcpp::create_wall_timer(
                   std::chrono::nanoseconds(period_ns), std::move(callback), nullptr,
                   node->base().get(), node->timers().get());
               } else {
                 e.timer = rclcpp::create_timer(
                   node->base(), node->timers(), node->clock(),
                   rclcpp::Duration(std::chrono::nanoseconds(period_ns)), std::move(callback));
               }
             });
           });
}

IRIS_ROS2_API int64_t iris_ros2_timer_cancel(int64_t timer)
{
  return guarded([&]() -> int64_t {
             registry().get<TimerEntity>(timer, "timer")->timer->cancel();
             return 0;
           });
}

IRIS_ROS2_API int64_t iris_ros2_timer_reset(int64_t timer)
{
  return guarded([&]() -> int64_t {
             registry().get<TimerEntity>(timer, "timer")->timer->reset();
             return 0;
           });
}

IRIS_ROS2_API int64_t iris_ros2_timer_destroy(int64_t timer)
{
  return guarded([&]() -> int64_t {
             auto t = registry().get<TimerEntity>(timer, "timer");
             t->timer->cancel();
             registry().destroy(timer);
             return 0;
           });
}

// ===========================================================================
// Parameters -- the node's real rclcpp parameters, so `ros2 param` lists,
// gets and sets them, descriptors constrain them, and /parameter_events
// reports their changes.
// ===========================================================================

// `kind`: 0 JSON, 1 bool, 2 i64, 3 f64 (bits), 4 string; `value` is read
// accordingly. `descriptor`: JSON (see parameter_descriptor) or "". Returns 0,
// or 1 when the parameter was already declared (left unchanged).
IRIS_ROS2_API int64_t iris_ros2_param_declare(
  int64_t node, const char * name, int64_t kind, int64_t value, const char * descriptor)
{
  return guarded([&]() -> int64_t {
             auto params = node_of(node)->parameters();
             if (params->has_parameter(str(name))) {
               return 1;
             }
             auto v = parameter_value(kind, value);
             auto d = parameter_descriptor(descriptor, v);
             InternalParameterChange internal;
             params->declare_parameter(str(name), v, d, false);
             return 0;
           });
}

// Declares a numeric parameter with a range, enforced by rclcpp on every
// set (and shown by `ros2 param describe`). kind 2: value, min, max and step
// are integers; kind 3: they are f64 bit patterns. step 0: continuous.
IRIS_ROS2_API int64_t iris_ros2_param_declare_range(
  int64_t node, const char * name, int64_t kind, int64_t value, int64_t min, int64_t max,
  int64_t step, const char * description, int64_t read_only)
{
  return guarded([&]() -> int64_t {
             auto params = node_of(node)->parameters();
             if (params->has_parameter(str(name))) {
               return 1;
             }
             rcl_interfaces::msg::ParameterDescriptor d;
             d.description = str(description);
             d.read_only = read_only != 0;
             rclcpp::ParameterValue v;
             if (kind == PARAM_I64) {
               v = rclcpp::ParameterValue(static_cast<int64_t>(value));
               rcl_interfaces::msg::IntegerRange range;
               range.from_value = min;
               range.to_value = max;
               range.step = step > 0 ? static_cast<uint64_t>(step) : 0;
               d.integer_range.push_back(range);
             } else if (kind == PARAM_F64) {
               v = rclcpp::ParameterValue(f64_of(value));
               rcl_interfaces::msg::FloatingPointRange range;
               range.from_value = f64_of(min);
               range.to_value = f64_of(max);
               range.step = f64_of(step);
               d.floating_point_range.push_back(range);
             } else {
               throw std::runtime_error("a ranged parameter is an integer (2) or a double (3)");
             }
             InternalParameterChange internal;
             params->declare_parameter(str(name), v, d, false);
             return 0;
           });
}

IRIS_ROS2_API int64_t iris_ros2_param_set(int64_t node, const char * name, int64_t kind, int64_t value)
{
  return guarded([&]() -> int64_t {
             auto params = node_of(node)->parameters();
             InternalParameterChange internal;
             auto results = params->set_parameters({rclcpp::Parameter(str(name), parameter_value(kind, value))});
             if (results.empty() || !results[0].successful) {
               throw std::runtime_error(
                 "parameter '" + std::string(str(name)) + "' was not set: " +
                 (results.empty() ? std::string("no result") : results[0].reason));
             }
             return 0;
           });
}

IRIS_ROS2_API double iris_ros2_param_get_f64(int64_t node, const char * name)
{
  return guarded<double>(std::numeric_limits<double>::quiet_NaN(), [&]() -> double {
             auto p = node_of(node)->parameters()->get_parameter(str(name));
             if (p.get_type() == rclcpp::ParameterType::PARAMETER_DOUBLE) {
               return p.as_double();
             }
             if (p.get_type() == rclcpp::ParameterType::PARAMETER_INTEGER) {
               return static_cast<double>(p.as_int());
             }
             throw std::runtime_error(
               "parameter '" + std::string(str(name)) + "' is " + p.get_type_name() + ", not a double");
           });
}

IRIS_ROS2_API int64_t iris_ros2_param_get_i64(int64_t node, const char * name)
{
  return guarded<int64_t>(std::numeric_limits<int64_t>::min(), [&]() -> int64_t {
             auto p = node_of(node)->parameters()->get_parameter(str(name));
             if (p.get_type() == rclcpp::ParameterType::PARAMETER_INTEGER) {
               return p.as_int();
             }
             throw std::runtime_error(
               "parameter '" + std::string(str(name)) + "' is " + p.get_type_name() + ", not an integer");
           });
}

IRIS_ROS2_API int64_t iris_ros2_param_get_bool(int64_t node, const char * name)
{
  return guarded([&]() -> int64_t {
             auto p = node_of(node)->parameters()->get_parameter(str(name));
             if (p.get_type() == rclcpp::ParameterType::PARAMETER_BOOL) {
               return p.as_bool() ? 1 : 0;
             }
             throw std::runtime_error(
               "parameter '" + std::string(str(name)) + "' is " + p.get_type_name() + ", not a bool");
           });
}

IRIS_ROS2_API int64_t iris_ros2_param_get_str(int64_t node, const char * name)
{
  return guarded([&]() -> int64_t {
             auto p = node_of(node)->parameters()->get_parameter(str(name));
             if (p.get_type() == rclcpp::ParameterType::PARAMETER_STRING) {
               return set_result(p.as_string());
             }
             throw std::runtime_error(
               "parameter '" + std::string(str(name)) + "' is " + p.get_type_name() + ", not a string");
           });
}

IRIS_ROS2_API int64_t iris_ros2_param_get_json(int64_t node, const char * name)
{
  return guarded([&]() -> int64_t {
             return set_result(parameter_json(
               node_of(node)->parameters()->get_parameter(str(name)).get_parameter_value()));
           });
}

IRIS_ROS2_API int64_t iris_ros2_param_has(int64_t node, const char * name)
{
  return guarded([&]() -> int64_t {return node_of(node)->parameters()->has_parameter(str(name)) ? 1 : 0;});
}

// rcl_interfaces/msg/ParameterType: 0 not set, 1 bool, 2 integer, 3 double,
// 4 string, 5 byte[], 6 bool[], 7 integer[], 8 double[], 9 string[].
IRIS_ROS2_API int64_t iris_ros2_param_type(int64_t node, const char * name)
{
  return guarded([&]() -> int64_t {
             auto params = node_of(node)->parameters();
             if (!params->has_parameter(str(name))) {
               return 0;
             }
             return static_cast<int64_t>(params->get_parameter(str(name)).get_type());
           });
}

IRIS_ROS2_API int64_t iris_ros2_param_list(int64_t node)
{
  return guarded([&]() -> int64_t {
             auto result = node_of(node)->parameters()->list_parameters({}, 0);
             std::string out = "[";
             for (size_t i = 0; i < result.names.size(); ++i) {
               if (i) {
                 out.push_back(',');
               }
               write_json_string(out, result.names[i]);
             }
             out.push_back(']');
             return set_result(out);
           });
}

// ===========================================================================
// Lifecycle -- rclcpp_lifecycle's state machine, with its standard services
// (`ros2 lifecycle get/set/list` work against an IRIS node).
// ===========================================================================

namespace
{

rclcpp_lifecycle::LifecycleNode & lifecycle_of(int64_t handle)
{
  auto n = node_of(handle);
  if (!n->lifecycle) {
    throw std::runtime_error("node " + std::to_string(handle) + " is not a lifecycle node");
  }
  return *n->lifecycle;
}

}  // namespace

// lifecycle_msgs/msg/State ids: 1 unconfigured, 2 inactive, 3 active,
// 4 finalized; 10-15 while a transition runs.
IRIS_ROS2_API int64_t iris_ros2_lifecycle_state(int64_t node)
{
  return guarded([&]() -> int64_t {return lifecycle_of(node).get_current_state().id();});
}

IRIS_ROS2_API int64_t iris_ros2_lifecycle_state_label(int64_t node)
{
  return guarded([&]() -> int64_t {return set_result(lifecycle_of(node).get_current_state().label());});
}

// Runs a lifecycle_msgs/msg/Transition (1 configure, 2 cleanup, 3 activate,
// 4 deactivate, 5/6/7 shutdown from unconfigured/inactive/active). Returns
// the state the node is in afterwards; an invalid transition leaves it where
// it was.
IRIS_ROS2_API int64_t iris_ros2_lifecycle_trigger(int64_t node, int64_t transition)
{
  return guarded([&]() -> int64_t {
             if (transition < 0 || transition > 255) {
               throw std::runtime_error("invalid transition id " + std::to_string(transition));
             }
             return lifecycle_of(node).trigger_transition(static_cast<uint8_t>(transition)).id();
           });
}

// What the bridge's callback for `callback` (1 configure, 2 cleanup,
// 3 activate, 4 deactivate, 5 shutdown, 6 error) returns from now on:
// 0 success, 1 failure, 2 error.
IRIS_ROS2_API int64_t iris_ros2_lifecycle_set_result(int64_t node, int64_t callback, int64_t result)
{
  return guarded([&]() -> int64_t {
             if (callback < 1 || callback > 6 || result < 0 || result > 2) {
               throw std::runtime_error("lifecycle_set_result: callback 1-6, result 0-2");
             }
             lifecycle_of(node);
             node_of(node)->set_transition_result(static_cast<int>(callback), static_cast<int>(result));
             return 0;
           });
}

// ===========================================================================
// Services
// ===========================================================================

IRIS_ROS2_API int64_t iris_ros2_service_create(int64_t node_handle, const char * name, const char * type)
{
  return guarded<int64_t>(0, [&]() -> int64_t {
             auto node = node_of(node_handle);
             auto entity = std::make_shared<ServiceEntity>();
             entity->node = node;
             entity->type = service_type(str(type));
             entity->name = str(name);
             return create_under(node, entity, [&](ServiceEntity & e) {
               std::weak_ptr<ServiceEntity> weak = entity;
               e.service = std::make_shared<GenericService>(
                 node->base()->get_shared_rcl_node_handle(), e.name, e.type, rclcpp::ServicesQoS(),
                 [weak](std::shared_ptr<rmw_request_id_t> header, MessagePtr request) {
                   auto s = weak.lock();
                   if (!s) {
                     return;
                   }
                   ServiceEntity::Pending pending;
                   pending.header = std::move(header);
                   pending.response = Message::create(s->type->response);
                   pending.request_handle = register_message(std::move(request), s->handle);
                   pending.response_handle = register_message(pending.response, s->handle);
                   int64_t token;
                   {
                     std::lock_guard<std::mutex> lock(s->mutex);
                     token = s->next_token++;
                     s->pending[token] = pending;
                   }
                   Event event;
                   event.kind = EVENT_REQUEST;
                   event.source = s->handle;
                   event.node = s->node->handle;
                   event.id = token;
                   event.message = pending.request_handle;
                   event.aux = pending.response_handle;
                   s->node->ctx->push(std::move(event));
                 });
               node->services()->add_service(e.service, nullptr);
             });
           });
}

IRIS_ROS2_API int64_t iris_ros2_service_destroy(int64_t srv)
{
  return guarded([&]() -> int64_t {
             service_of(srv);
             registry().destroy(srv);
             return 0;
           });
}

// Sends the response message of request `token` (fill it first: its handle
// is the REQUEST event's aux) and releases the request.
IRIS_ROS2_API int64_t iris_ros2_service_reply(int64_t srv, int64_t token)
{
  return guarded([&]() -> int64_t {
             auto s = service_of(srv);
             ServiceEntity::Pending pending;
             {
               std::lock_guard<std::mutex> lock(s->mutex);
               auto it = s->pending.find(token);
               if (it == s->pending.end()) {
                 throw std::runtime_error(
                   "service '" + s->name + "' has no pending request " + std::to_string(token) +
                   " (already answered?)");
               }
               pending = it->second;
               s->pending.erase(it);
             }
             s->service->send_response(*pending.header, *pending.response);
             registry().destroy(pending.request_handle);
             registry().destroy(pending.response_handle);
             return 0;
           });
}

IRIS_ROS2_API int64_t iris_ros2_service_request(int64_t srv, int64_t token)
{
  return guarded<int64_t>(0, [&]() -> int64_t {
             auto s = service_of(srv);
             std::lock_guard<std::mutex> lock(s->mutex);
             auto it = s->pending.find(token);
             return it == s->pending.end() ? 0 : it->second.request_handle;
           });
}

IRIS_ROS2_API int64_t iris_ros2_service_response(int64_t srv, int64_t token)
{
  return guarded<int64_t>(0, [&]() -> int64_t {
             auto s = service_of(srv);
             std::lock_guard<std::mutex> lock(s->mutex);
             auto it = s->pending.find(token);
             return it == s->pending.end() ? 0 : it->second.response_handle;
           });
}

// ===========================================================================
// Clients
// ===========================================================================

IRIS_ROS2_API int64_t iris_ros2_client_create(int64_t node_handle, const char * name, const char * type)
{
  return guarded<int64_t>(0, [&]() -> int64_t {
             auto node = node_of(node_handle);
             auto entity = std::make_shared<ClientEntity>();
             entity->node = node;
             entity->type = service_type(str(type));
             entity->name = str(name);
             return create_under(node, entity, [&](ClientEntity & e) {
               std::weak_ptr<ClientEntity> weak = entity;
               e.client = std::make_shared<GenericClient>(
                 node->base().get(), node->graph(), e.name, e.type, rclcpp::ServicesQoS(),
                 [weak](int64_t token, MessagePtr response) {
                   auto c = weak.lock();
                   if (!c) {
                     return;
                   }
                   {
                     std::lock_guard<std::mutex> lock(c->mutex);
                     c->responses[token] = std::move(response);
                   }
                   Event event;
                   event.kind = EVENT_RESPONSE;
                   event.source = c->handle;
                   event.node = c->node->handle;
                   event.id = token;
                   event.take = [weak, token]() -> MessagePtr {
                       auto cl = weak.lock();
                       return cl ? cl->take(token) : nullptr;
                     };
                   c->node->ctx->push(std::move(event));
                 });
               node->services()->add_client(e.client, nullptr);
             });
           });
}

IRIS_ROS2_API int64_t iris_ros2_client_destroy(int64_t cli)
{
  return guarded([&]() -> int64_t {
             client_of(cli);
             registry().destroy(cli);
             return 0;
           });
}

IRIS_ROS2_API int64_t iris_ros2_client_new_request(int64_t cli)
{
  return guarded<int64_t>(0, [&]() -> int64_t {
             return register_message(Message::create(client_of(cli)->type->request));
           });
}

// Sends `request`; returns its token (the RESPONSE event's id).
IRIS_ROS2_API int64_t iris_ros2_client_send(int64_t cli, int64_t request)
{
  return guarded([&]() -> int64_t {
             auto c = client_of(cli);
             auto m = message_of(request);
             if (m->type().name != c->type->request->name) {
               throw std::runtime_error(
                 "client '" + c->name + "' sends " + c->type->request->name + ", not " +
                 m->type().name);
             }
             return c->client->send(*m);
           });
}

IRIS_ROS2_API int64_t iris_ros2_client_ready(int64_t cli)
{
  return guarded([&]() -> int64_t {return client_of(cli)->client->service_is_ready() ? 1 : 0;});
}

IRIS_ROS2_API int64_t iris_ros2_client_wait_service(int64_t cli, int64_t timeout_ms)
{
  return guarded([&]() -> int64_t {
             return client_of(cli)->client->wait_for_service(
               std::chrono::milliseconds(timeout_ms)) ? 1 : 0;
           });
}

// Runs the executor until the response to `token` arrives; returns it (a
// handle living until the next call on this client or the next spin_once),
// or 0 on timeout.
IRIS_ROS2_API int64_t iris_ros2_client_wait_response(int64_t cli, int64_t token, int64_t timeout_ms)
{
  return guarded([&]() -> int64_t {
             auto c = client_of(cli);
             auto ctx = c->node->ctx;
             spin_until(*ctx, timeout_ms, [&]() {return c->has(token);});
             auto response = c->take(token);
             if (!response) {
               return 0;
             }
             ctx->release_scope(cli);
             return ctx->keep_scoped(response, cli);
           });
}

// ===========================================================================
// tf2 -- tf2's own BufferCore, filled by a tf2_ros::TransformListener on
// /tf and /tf_static, and tf2_ros broadcasters.
// ===========================================================================

// node 0: a local buffer (no listener; fill it with tf_set). Otherwise a
// tf2_ros::Buffer on the node's clock with a listener in the node, fed as the
// context's executor spins. `cache_ms` <= 0 keeps tf2's default 10 s.
IRIS_ROS2_API int64_t iris_ros2_tf_buffer_create(int64_t node_handle, int64_t cache_ms)
{
  return guarded<int64_t>(0, [&]() -> int64_t {
             tf2::Duration cache = cache_ms > 0 ?
             tf2::Duration(std::chrono::milliseconds(cache_ms)) :
             tf2::Duration(tf2::BUFFER_CORE_DEFAULT_CACHE_TIME);
             auto entity = std::make_shared<TfBufferEntity>();
             if (node_handle == 0) {
               entity->core = std::make_shared<tf2::BufferCore>(cache);
               return registry().add(entity);
             }
             auto node = node_of(node_handle);
             entity->node = node;
             return create_under(node, entity, [&](TfBufferEntity & e) {
               auto buffer = std::make_shared<tf2_ros::Buffer>(node->clock(), cache);
               e.core = buffer;
               if (node->node) {
                 e.listener = std::make_shared<tf2_ros::TransformListener>(*buffer, node->node, false);
               } else {
                 e.listener = std::make_shared<tf2_ros::TransformListener>(*buffer, node->lifecycle, false);
               }
             });
           });
}

IRIS_ROS2_API int64_t iris_ros2_tf_buffer_destroy(int64_t buf)
{
  return guarded([&]() -> int64_t {
             registry().get<TfBufferEntity>(buf, "tf buffer");
             registry().destroy(buf);
             return 0;
           });
}

// Inserts parent->child at `stamp_ns`; `cell` holds 7 f64s: tx ty tz qx qy qz qw.
IRIS_ROS2_API int64_t iris_ros2_tf_set(
  int64_t buf, const char * parent, const char * child, int64_t stamp_ns, int64_t cell,
  int64_t is_static)
{
  return guarded([&]() -> int64_t {
             auto e = registry().get<TfBufferEntity>(buf, "tf buffer");
             auto t = make_transform(parent, child, stamp_ns, cell_f64(cell));
             if (!e->core->setTransform(t, "iris", is_static != 0)) {
               throw std::runtime_error(
                 "tf2 rejected the transform " + std::string(str(parent)) + " -> " + str(child) +
                 " (invalid frame names or a non-finite / non-normalisable value)");
             }
             return 0;
           });
}

// The transform taking data in `source` into `target` at `time_ns` (0: the
// latest available), into `cell`: 7 f64s (tx ty tz qx qy qz qw) then the
// stamp as i64 in slot 7. Returns 1, or 0 when tf2 cannot answer (the reason
// is the last error).
IRIS_ROS2_API int64_t iris_ros2_tf_lookup(
  int64_t buf, const char * target, const char * source, int64_t time_ns, int64_t cell)
{
  return guarded([&]() -> int64_t {
             auto e = registry().get<TfBufferEntity>(buf, "tf buffer");
             double * out = cell_f64(cell);
             geometry_msgs::msg::TransformStamped t;
             try {
               t = e->core->lookupTransform(
                 str(target), str(source), tf2::TimePoint(std::chrono::nanoseconds(time_ns)));
             } catch (const tf2::TransformException & ex) {
               record_error(ex.what());
               return 0;
             }
             out[0] = t.transform.translation.x;
             out[1] = t.transform.translation.y;
             out[2] = t.transform.translation.z;
             out[3] = t.transform.rotation.x;
             out[4] = t.transform.rotation.y;
             out[5] = t.transform.rotation.z;
             out[6] = t.transform.rotation.w;
             cell_i64(cell)[7] = join_stamp(t.header.stamp.sec, t.header.stamp.nanosec);
             return 1;
           });
}

IRIS_ROS2_API int64_t iris_ros2_tf_can_transform(
  int64_t buf, const char * target, const char * source, int64_t time_ns)
{
  return guarded([&]() -> int64_t {
             auto e = registry().get<TfBufferEntity>(buf, "tf buffer");
             std::string why;
             bool ok = e->core->canTransform(
               str(target), str(source), tf2::TimePoint(std::chrono::nanoseconds(time_ns)), &why);
             if (!ok) {
               record_error(why);
             }
             return ok ? 1 : 0;
           });
}

IRIS_ROS2_API int64_t iris_ros2_tf_frames(int64_t buf)
{
  return guarded([&]() -> int64_t {
             auto names = registry().get<TfBufferEntity>(buf, "tf buffer")->core->getAllFrameNames();
             std::string out = "[";
             for (size_t i = 0; i < names.size(); ++i) {
               if (i) {
                 out.push_back(',');
               }
               write_json_string(out, names[i]);
             }
             out.push_back(']');
             return set_result(out);
           });
}

IRIS_ROS2_API int64_t iris_ros2_tf_broadcaster_create(int64_t node_handle, int64_t is_static)
{
  return guarded<int64_t>(0, [&]() -> int64_t {
             auto node = node_of(node_handle);
             auto entity = std::make_shared<TfBroadcasterEntity>();
             entity->node = node;
             return create_under(node, entity, [&](TfBroadcasterEntity & e) {
               if (is_static) {
                 e.fixed = node->node ?
                 std::make_shared<tf2_ros::StaticTransformBroadcaster>(node->node) :
                 std::make_shared<tf2_ros::StaticTransformBroadcaster>(node->lifecycle);
               } else {
                 e.dynamic = node->node ?
                 std::make_shared<tf2_ros::TransformBroadcaster>(node->node) :
                 std::make_shared<tf2_ros::TransformBroadcaster>(node->lifecycle);
               }
             });
           });
}

IRIS_ROS2_API int64_t iris_ros2_tf_broadcaster_destroy(int64_t bc)
{
  return guarded([&]() -> int64_t {
             registry().get<TfBroadcasterEntity>(bc, "tf broadcaster");
             registry().destroy(bc);
             return 0;
           });
}

// Broadcasts parent->child on /tf (or /tf_static); `stamp_ns` 0 stamps it
// with the node's clock. `cell` as for tf_set.
IRIS_ROS2_API int64_t iris_ros2_tf_send(
  int64_t bc, const char * parent, const char * child, int64_t stamp_ns, int64_t cell)
{
  return guarded([&]() -> int64_t {
             auto e = registry().get<TfBroadcasterEntity>(bc, "tf broadcaster");
             if (stamp_ns == 0) {
               stamp_ns = e->node->clock()->now().nanoseconds();
             }
             auto t = make_transform(parent, child, stamp_ns, cell_f64(cell));
             if (e->fixed) {
               e->fixed->sendTransform(t);
             } else {
               e->dynamic->sendTransform(t);
             }
             return 0;
           });
}

// ===========================================================================
// Time
// ===========================================================================

// 0: system time (ns since the epoch); 1: steady time.
IRIS_ROS2_API int64_t iris_ros2_time_now(int64_t kind)
{
  return guarded<int64_t>(std::numeric_limits<int64_t>::min(), [&]() -> int64_t {
             if (kind == 1) {
               return rclcpp::Clock(RCL_STEADY_TIME).now().nanoseconds();
             }
             return rclcpp::Clock(RCL_SYSTEM_TIME).now().nanoseconds();
           });
}

// ===========================================================================
// The original `iris_rcl_*` API, kept call-compatible on the new runtime.
// Fractional values are fixed-point scaled by 1e8, as they always were here;
// new code should use the iris_ros2_* message API, which carries exact f64s.
// ===========================================================================

namespace
{

constexpr double LEGACY_SCALE = 100000000.0;

std::mutex & legacy_mutex()
{
  static auto * m = new std::mutex();
  return *m;
}

int64_t g_legacy_ctx = 0;
int64_t g_legacy_node = 0;
int64_t g_legacy_pub = 0;

// Legacy type names were exact; keep accepting only those six.
const char * legacy_type(const char * type)
{
  static const std::set<std::string> known = {
    "std_msgs/msg/Float64", "std_msgs/msg/Int64", "std_msgs/msg/String",
    "geometry_msgs/msg/Vector3", "geometry_msgs/msg/Twist", "geometry_msgs/msg/Pose"};
  if (!known.count(str(type))) {
    throw std::runtime_error(
      "unknown message type '" + std::string(str(type)) +
      "' for the legacy API; use iris_ros2_publisher_create for other types");
  }
  return type;
}

int64_t legacy_publish(int64_t pub, const char * type, std::initializer_list<std::pair<const char *, double>> fields)
{
  auto p = publisher_of(pub);
  if (p->type->name != type) {
    throw std::runtime_error("the publisher carries " + p->type->name + ", not " + type);
  }
  auto m = Message::create(p->type);
  for (const auto & f : fields) {
    set_f64(*m, f.first, f.second);
  }
  return p->publish(*m) >= 0 ? 0 : -1;
}

// Takes one message of `type` (1) or reports none ready (0).
MessagePtr legacy_take(int64_t sub, const char * type)
{
  auto s = subscription_of(sub);
  if (s->type->name != type) {
    throw std::runtime_error("the subscription carries " + s->type->name + ", not " + type);
  }
  return wait_and_take(*s, 0);
}

}  // namespace

IRIS_ROS2_API int64_t iris_rcl_init()
{
  std::lock_guard<std::mutex> lock(legacy_mutex());
  if (g_legacy_ctx) {
    return 0;
  }
  g_legacy_ctx = iris_ros2_context_create("", -1);
  return g_legacy_ctx ? 0 : -1;
}

IRIS_ROS2_API int64_t iris_rcl_get_zero_initialized_node()
{
  std::lock_guard<std::mutex> lock(legacy_mutex());
  if (!g_legacy_ctx) {
    return -1;
  }
  if (!g_legacy_node) {
    g_legacy_node = iris_ros2_node_create(g_legacy_ctx, "ai_orchestrator", "robotics", "", 0);
  }
  return g_legacy_node ? g_legacy_node : -1;
}

IRIS_ROS2_API int64_t iris_rcl_get_zero_initialized_publisher()
{
  std::lock_guard<std::mutex> lock(legacy_mutex());
  if (!g_legacy_node) {
    return -1;
  }
  if (!g_legacy_pub) {
    g_legacy_pub = iris_ros2_publisher_create(
      g_legacy_node, "/orchestrator/actuation", "std_msgs/msg/Float64",
      RMW_QOS_POLICY_HISTORY_KEEP_LAST, 10, RMW_QOS_POLICY_RELIABILITY_RELIABLE,
      RMW_QOS_POLICY_DURABILITY_VOLATILE);
  }
  return g_legacy_pub ? g_legacy_pub : -1;
}

IRIS_ROS2_API int64_t iris_rcl_publish(int64_t pub, int64_t scaled)
{
  return guarded([&]() -> int64_t {
             return legacy_publish(pub, "std_msgs/msg/Float64", {{"data", scaled / LEGACY_SCALE}});
           });
}

IRIS_ROS2_API int64_t iris_rcl_shutdown()
{
  std::lock_guard<std::mutex> lock(legacy_mutex());
  if (g_legacy_ctx) {
    registry().destroy(g_legacy_ctx);
  }
  g_legacy_ctx = g_legacy_node = g_legacy_pub = 0;
  return 0;
}

IRIS_ROS2_API int64_t iris_rcl_context_create()
{
  return iris_ros2_context_create("", -1);
}

IRIS_ROS2_API int64_t iris_rcl_context_destroy(int64_t ctx)
{
  return iris_ros2_context_destroy(ctx);
}

IRIS_ROS2_API int64_t iris_rcl_node_create(int64_t ctx, const char * name, const char * ns)
{
  return iris_ros2_node_create(ctx, name, ns, "", 0);
}

IRIS_ROS2_API int64_t iris_rcl_node_destroy(int64_t node)
{
  return iris_ros2_node_destroy(node);
}

IRIS_ROS2_API int64_t iris_rcl_publisher_create(int64_t node, const char * topic, const char * type)
{
  return guarded<int64_t>(0, [&]() -> int64_t {
             return create_publisher(node, topic, legacy_type(type), default_qos());
           });
}

IRIS_ROS2_API int64_t iris_rcl_publisher_create_qos(
  int64_t node, const char * topic, const char * type,
  int64_t history, int64_t depth, int64_t reliability, int64_t durability)
{
  return guarded<int64_t>(0, [&]() -> int64_t {
             return create_publisher(
               node, topic, legacy_type(type), make_qos(history, depth, reliability, durability));
           });
}

IRIS_ROS2_API int64_t iris_rcl_publisher_destroy(int64_t pub, int64_t /*node*/)
{
  return iris_ros2_publisher_destroy(pub);
}

IRIS_ROS2_API int64_t iris_rcl_subscription_create(int64_t node, const char * topic, const char * type)
{
  return guarded<int64_t>(0, [&]() -> int64_t {
             return create_subscription(node, topic, legacy_type(type), default_qos());
           });
}

IRIS_ROS2_API int64_t iris_rcl_subscription_create_qos(
  int64_t node, const char * topic, const char * type,
  int64_t history, int64_t depth, int64_t reliability, int64_t durability)
{
  return guarded<int64_t>(0, [&]() -> int64_t {
             return create_subscription(
               node, topic, legacy_type(type), make_qos(history, depth, reliability, durability));
           });
}

IRIS_ROS2_API int64_t iris_rcl_subscription_destroy(int64_t sub, int64_t /*node*/)
{
  return iris_ros2_subscription_destroy(sub);
}

IRIS_ROS2_API int64_t iris_rcl_publish_float64(int64_t pub, int64_t scaled)
{
  return guarded([&]() -> int64_t {
             return legacy_publish(pub, "std_msgs/msg/Float64", {{"data", scaled / LEGACY_SCALE}});
           });
}

IRIS_ROS2_API int64_t iris_rcl_publish_int64(int64_t pub, int64_t value)
{
  return guarded([&]() -> int64_t {
             auto p = publisher_of(pub);
             auto m = Message::create(p->type);
             set_i64(*m, "data", value);
             return p->publish(*m) >= 0 ? 0 : -1;
           });
}

IRIS_ROS2_API int64_t iris_rcl_publish_string(int64_t pub, const char * value)
{
  return guarded([&]() -> int64_t {
             auto p = publisher_of(pub);
             auto m = Message::create(p->type);
             set_str(*m, "data", str(value));
             return p->publish(*m) >= 0 ? 0 : -1;
           });
}

IRIS_ROS2_API int64_t iris_rcl_publish_vector3(int64_t pub, int64_t x, int64_t y, int64_t z)
{
  return guarded([&]() -> int64_t {
             return legacy_publish(pub, "geometry_msgs/msg/Vector3", {
               {"x", x / LEGACY_SCALE}, {"y", y / LEGACY_SCALE}, {"z", z / LEGACY_SCALE}});
           });
}

IRIS_ROS2_API int64_t iris_rcl_publish_twist(
  int64_t pub, int64_t lx, int64_t ly, int64_t lz, int64_t ax, int64_t ay, int64_t az)
{
  return guarded([&]() -> int64_t {
             return legacy_publish(pub, "geometry_msgs/msg/Twist", {
               {"linear.x", lx / LEGACY_SCALE}, {"linear.y", ly / LEGACY_SCALE},
               {"linear.z", lz / LEGACY_SCALE}, {"angular.x", ax / LEGACY_SCALE},
               {"angular.y", ay / LEGACY_SCALE}, {"angular.z", az / LEGACY_SCALE}});
           });
}

IRIS_ROS2_API int64_t iris_rcl_publish_pose(
  int64_t pub, int64_t px, int64_t py, int64_t pz, int64_t ox, int64_t oy, int64_t oz, int64_t ow)
{
  return guarded([&]() -> int64_t {
             return legacy_publish(pub, "geometry_msgs/msg/Pose", {
               {"position.x", px / LEGACY_SCALE}, {"position.y", py / LEGACY_SCALE},
               {"position.z", pz / LEGACY_SCALE}, {"orientation.x", ox / LEGACY_SCALE},
               {"orientation.y", oy / LEGACY_SCALE}, {"orientation.z", oz / LEGACY_SCALE},
               {"orientation.w", ow / LEGACY_SCALE}});
           });
}

IRIS_ROS2_API int64_t iris_rcl_wait_for_message(int64_t sub, int64_t /*ctx*/, int64_t timeout_ms)
{
  return iris_ros2_wait(sub, timeout_ms);
}

IRIS_ROS2_API int64_t iris_rcl_take_float64(int64_t sub, double * out)
{
  return guarded<int64_t>(0, [&]() -> int64_t {
             auto m = legacy_take(sub, "std_msgs/msg/Float64");
             if (!m || !out) {
               return 0;
             }
             *out = get_f64(*m, "data");
             return 1;
           });
}

IRIS_ROS2_API int64_t iris_rcl_take_int64_val(int64_t sub, int64_t * out)
{
  return guarded<int64_t>(0, [&]() -> int64_t {
             auto m = legacy_take(sub, "std_msgs/msg/Int64");
             if (!m || !out) {
               return 0;
             }
             *out = get_i64(*m, "data");
             return 1;
           });
}

IRIS_ROS2_API int64_t iris_rcl_take_string(int64_t sub, char * buf, int64_t max_len)
{
  return guarded<int64_t>(0, [&]() -> int64_t {
             auto m = legacy_take(sub, "std_msgs/msg/String");
             if (!m || !buf || max_len <= 0) {
               return 0;
             }
             std::string text = get_str(*m, "data");
             size_t n = std::min(text.size(), static_cast<size_t>(max_len - 1));
             std::memcpy(buf, text.data(), n);
             buf[n] = '\0';
             return 1;
           });
}

namespace
{

int64_t legacy_take_doubles(
  int64_t sub, const char * type, std::initializer_list<const char *> paths, double * out)
{
  auto m = legacy_take(sub, type);
  if (!m || !out) {
    return 0;
  }
  size_t i = 0;
  for (const char * path : paths) {
    out[i++] = get_f64(*m, path);
  }
  return 1;
}

}  // namespace

IRIS_ROS2_API int64_t iris_rcl_take_vector3(int64_t sub, double * out)
{
  return guarded<int64_t>(0, [&]() -> int64_t {
             return legacy_take_doubles(sub, "geometry_msgs/msg/Vector3", {"x", "y", "z"}, out);
           });
}

IRIS_ROS2_API int64_t iris_rcl_take_twist(int64_t sub, double * out)
{
  return guarded<int64_t>(0, [&]() -> int64_t {
             return legacy_take_doubles(sub, "geometry_msgs/msg/Twist", {
               "linear.x", "linear.y", "linear.z", "angular.x", "angular.y", "angular.z"}, out);
           });
}

IRIS_ROS2_API int64_t iris_rcl_take_pose(int64_t sub, double * out)
{
  return guarded<int64_t>(0, [&]() -> int64_t {
             return legacy_take_doubles(sub, "geometry_msgs/msg/Pose", {
               "position.x", "position.y", "position.z", "orientation.x", "orientation.y",
               "orientation.z", "orientation.w"}, out);
           });
}
