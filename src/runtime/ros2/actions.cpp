// Actions, on rclcpp_action's type-erased ServerBase and ClientBase.
//
// rclcpp_action implements the protocol -- the goal, cancel and result
// services, the feedback and status topics, goal state bookkeeping and
// expiry -- in its base classes and leaves only message handling to the
// templated Server<ActionT> / Client<ActionT>. The bridge supplies that
// message handling through C++ introspection, so any action type works and
// the wire behaviour is exactly rclcpp_action's: `ros2 action send_goal` and
// compiled C++ servers and clients interoperate with IRIS ones.
#ifndef NOMINMAX
#define NOMINMAX
#endif
#ifndef NOGDI
#define NOGDI
#endif

#include <cstring>
#include <map>
#include <memory>
#include <mutex>
#include <string>
#include <unordered_map>

#include <action_msgs/msg/goal_status_array.hpp>
#include <action_msgs/srv/cancel_goal.hpp>
#include <rcl_action/rcl_action.h>
#include <rclcpp/rclcpp.hpp>
#include <rclcpp_action/client.hpp>
#include <rclcpp_action/server.hpp>
#include <rclcpp_action/types.hpp>

#include "bridge.hpp"
#include "entities.hpp"

namespace iris_ros2
{

namespace
{

using rclcpp_action::GoalUUID;

GoalUUID read_uuid(const intro::MessageMembers * members, void * message)
{
  GoalUUID uuid;
  std::memcpy(uuid.data(), field_pointer(members, message, "goal_id.uuid", nullptr), uuid.size());
  return uuid;
}

void write_uuid(const intro::MessageMembers * members, void * message, const GoalUUID & uuid)
{
  std::memcpy(field_pointer(members, message, "goal_id.uuid", nullptr), uuid.data(), uuid.size());
}

// A typed view of a message struct rclcpp handed over as `void *`.
MessagePtr view_of(const MessageTypePtr & type, void * data, std::shared_ptr<const void> owner = nullptr)
{
  return Message::view(type, data, std::move(owner));
}

// The `field` sub-message of `outer`, as a message of type `type` sharing
// outer's storage.
MessagePtr part_of(const MessagePtr & outer, const char * field, const MessageTypePtr & type)
{
  return Message::view(type, field_pointer(outer->members(), outer->data(), field, nullptr), outer);
}

void check_state(rcl_ret_t ret, const char * what)
{
  if (ret != RCL_RET_OK) {
    rclcpp::exceptions::throw_from_rcl_error(ret, what);
  }
}

int8_t goal_status(rcl_action_goal_handle_t * handle)
{
  rcl_action_goal_state_t status = GOAL_STATE_UNKNOWN;
  check_state(rcl_action_goal_handle_get_status(handle, &status), "cannot read the goal's status");
  return status;
}

// ===========================================================================
// Server
// ===========================================================================

class GenericActionServer : public rclcpp_action::ServerBase
{
public:
  struct Hooks
  {
    std::function<rclcpp_action::GoalResponse(const GoalUUID &)> decide;
    std::function<void(std::shared_ptr<rcl_action_goal_handle_t>, const GoalUUID &, MessagePtr)> accepted;
    std::function<rclcpp_action::CancelResponse(const GoalUUID &)> cancel;
  };

  GenericActionServer(
    NodeEntity & node, const std::string & name, std::shared_ptr<const ActionType> type, Hooks hooks)
  : rclcpp_action::ServerBase(
      node.base(), node.clock_interface(), node.logging(), name, type->cpp,
      rcl_action_server_get_default_options()),
    type_(std::move(type)), hooks_(std::move(hooks)), clock_(node.clock())
  {}

  void publish_status_now() {publish_status();}
  void publish_feedback_now(const MessagePtr & feedback_message)
  {
    publish_feedback(erased(feedback_message));
  }
  void finish_now(const GoalUUID & uuid, const MessagePtr & result_response)
  {
    publish_result(uuid, erased(result_response));
    publish_status();
    notify_goal_terminal_state();
  }

protected:
  std::pair<rclcpp_action::GoalResponse, std::shared_ptr<void>>
  call_handle_goal_callback(GoalUUID & uuid, std::shared_ptr<void>) override
  {
    rclcpp_action::GoalResponse decision = hooks_.decide(uuid);
    auto response = Message::create(type_->send_goal_response);
    set_bool(*response, "accepted", decision != rclcpp_action::GoalResponse::REJECT);
    int64_t now = clock_->now().nanoseconds();
    set_i64(*response, "stamp.sec", now / 1000000000LL);
    set_i64(*response, "stamp.nanosec", now % 1000000000LL);
    return {decision, erased(response)};
  }

  rclcpp_action::CancelResponse call_handle_cancel_callback(const GoalUUID & uuid) override
  {
    return hooks_.cancel(uuid);
  }

  GoalUUID get_goal_id_from_goal_request(void * message) override
  {
    return read_uuid(type_->send_goal_request->members, message);
  }

  std::shared_ptr<void> create_goal_request() override
  {
    return erased(Message::create(type_->send_goal_request));
  }

  void call_goal_accepted_callback(
    std::shared_ptr<rcl_action_goal_handle_t> handle, GoalUUID uuid,
    std::shared_ptr<void> request) override
  {
    // A view over the request rclcpp took, kept alive by rclcpp's pointer.
    hooks_.accepted(std::move(handle), uuid, view_of(type_->send_goal_request, request.get(), request));
  }

  GoalUUID get_goal_id_from_result_request(void * message) override
  {
    return read_uuid(type_->get_result_request->members, message);
  }

  std::shared_ptr<void> create_result_request() override
  {
    return erased(Message::create(type_->get_result_request));
  }

  std::shared_ptr<void> create_result_response(int8_t status) override
  {
    auto response = Message::create(type_->get_result_response);
    set_i64(*response, "status", status);
    return erased(response);
  }

private:
  std::shared_ptr<const ActionType> type_;
  Hooks hooks_;
  rclcpp::Clock::SharedPtr clock_;
};

struct ActionServerEntity;

struct GoalEntity : Entity
{
  const char * kind() const override {return "goal";}
  std::weak_ptr<ActionServerEntity> server;
  GoalUUID uuid{};
  std::shared_ptr<rcl_action_goal_handle_t> handle;
  MessagePtr goal;              // view of the SendGoal request's `goal`
  MessagePtr feedback_message;  // FeedbackMessage: goal_id + feedback
  MessagePtr feedback;          // view of its `feedback`
  MessagePtr result_response;   // GetResult response: status + result
  MessagePtr result;            // view of its `result`
  int64_t goal_handle = 0;
  int64_t feedback_handle = 0;
  int64_t result_handle = 0;
};

struct ActionServerEntity : Entity
{
  const char * kind() const override {return "action server";}
  std::shared_ptr<NodeEntity> node;
  std::shared_ptr<const ActionType> type;
  std::string name;
  std::shared_ptr<GenericActionServer> server;

  int64_t goal_policy = 0;    // 0 accept and execute, 1 accept and defer, 2 reject
  int64_t cancel_policy = 0;  // 0 accept, 1 reject
  int64_t max_goals = 0;      // 0: no limit

  std::mutex mutex;
  std::map<GoalUUID, int64_t> goals;  // active goals -> goal handle

  ~ActionServerEntity() override
  {
    try {
      if (server && node) {
        node->waitables()->remove_waitable(server, nullptr);
      }
    } catch (...) {
    }
  }
};

std::shared_ptr<GoalEntity> goal_of(int64_t handle)
{
  return registry().get<GoalEntity>(handle, "goal");
}

std::shared_ptr<ActionServerEntity> server_of(const GoalEntity & goal)
{
  auto server = goal.server.lock();
  if (!server) {
    throw std::runtime_error("the goal's action server has been destroyed");
  }
  return server;
}

// ===========================================================================
// Client
// ===========================================================================

class GenericActionClient : public rclcpp_action::ClientBase
{
public:
  using Callback = std::function<void (std::shared_ptr<void>)>;
  struct Hooks
  {
    std::function<void(const GoalUUID &, MessagePtr)> feedback;
    std::function<void(const action_msgs::msg::GoalStatusArray &)> status;
  };

  GenericActionClient(
    NodeEntity & node, const std::string & name, std::shared_ptr<const ActionType> type, Hooks hooks)
  : rclcpp_action::ClientBase(
      node.base(), node.graph(), node.logging(), name, type->cpp,
      rcl_action_client_get_default_options()),
    type_(std::move(type)), hooks_(std::move(hooks))
  {}

  GoalUUID new_goal_id() {return generate_goal_id();}
  void send_goal(std::shared_ptr<void> request, Callback done) {send_goal_request(request, done);}
  void send_result(std::shared_ptr<void> request, Callback done) {send_result_request(request, done);}
  void send_cancel(std::shared_ptr<void> request, Callback done) {send_cancel_request(request, done);}

protected:
  std::shared_ptr<void> create_goal_response() const override
  {
    return erased(Message::create(type_->send_goal_response));
  }

  std::shared_ptr<void> create_result_response() const override
  {
    return erased(Message::create(type_->get_result_response));
  }

  std::shared_ptr<void> create_cancel_response() const override
  {
    return std::make_shared<action_msgs::srv::CancelGoal::Response>();
  }

  std::shared_ptr<void> create_feedback_message() const override
  {
    return erased(Message::create(type_->feedback_message));
  }

  void handle_feedback_message(std::shared_ptr<void> message) override
  {
    GoalUUID uuid = read_uuid(type_->feedback_message->members, message.get());
    auto feedback = Message::create(type_->feedback);
    copy_message(
      type_->feedback->members, feedback->data(),
      field_pointer(type_->feedback_message->members, message.get(), "feedback", nullptr));
    hooks_.feedback(uuid, std::move(feedback));
  }

  std::shared_ptr<void> create_status_message() const override
  {
    return std::make_shared<action_msgs::msg::GoalStatusArray>();
  }

  void handle_status_message(std::shared_ptr<void> message) override
  {
    hooks_.status(*std::static_pointer_cast<action_msgs::msg::GoalStatusArray>(message));
  }

private:
  std::shared_ptr<const ActionType> type_;
  Hooks hooks_;
};

struct ActionClientEntity : Entity
{
  const char * kind() const override {return "action client";}
  std::shared_ptr<NodeEntity> node;
  std::shared_ptr<const ActionType> type;
  std::string name;
  std::shared_ptr<GenericActionClient> client;

  struct Goal
  {
    GoalUUID uuid{};
    int accepted = -1;  // -1 no answer yet, 0 rejected, 1 accepted
    int8_t status = GOAL_STATUS_UNKNOWN;
    bool done = false;  // a result has arrived (or the goal was rejected)
    MessagePtr result;  // until taken
  };
  std::mutex mutex;
  std::unordered_map<int64_t, Goal> goals;  // token -> goal
  std::map<GoalUUID, int64_t> tokens;
  int64_t next_token = 1;

  MessagePtr take_result(int64_t token)
  {
    std::lock_guard<std::mutex> lock(mutex);
    auto it = goals.find(token);
    if (it == goals.end() || !it->second.result) {
      return nullptr;
    }
    return std::move(it->second.result);
  }

  ~ActionClientEntity() override
  {
    try {
      if (client && node) {
        node->waitables()->remove_waitable(client, nullptr);
      }
    } catch (...) {
    }
  }
};

std::shared_ptr<ActionClientEntity> action_client_of(int64_t handle)
{
  return registry().get<ActionClientEntity>(handle, "action client");
}

}  // namespace

}  // namespace iris_ros2

using namespace iris_ros2;

// ===========================================================================
// Action servers
// ===========================================================================

// `goal_policy`: 0 accept and execute, 1 accept and defer (call
// iris_ros2_goal_execute), 2 reject everything. `cancel_policy`: 0 accept
// cancel requests, 1 reject them. `max_goals`: reject new goals while this
// many are active (0: no limit). Goals arrive as EVENT_GOAL, cancellations
// as EVENT_CANCEL.
IRIS_ROS2_API int64_t iris_ros2_action_server_create(
  int64_t node_handle, const char * name, const char * type, int64_t goal_policy,
  int64_t cancel_policy, int64_t max_goals)
{
  return guarded<int64_t>(0, [&]() -> int64_t {
             auto node = node_of(node_handle);
             auto entity = std::make_shared<ActionServerEntity>();
             entity->node = node;
             entity->type = action_type(type ? type : "");
             entity->name = name ? name : "";
             entity->goal_policy = goal_policy;
             entity->cancel_policy = cancel_policy;
             entity->max_goals = max_goals;
             return create_under(node, entity, [&](ActionServerEntity & e) {
               std::weak_ptr<ActionServerEntity> weak = entity;
               GenericActionServer::Hooks hooks;
               hooks.decide = [weak](const GoalUUID &) {
                   auto s = weak.lock();
                   if (!s || s->goal_policy == 2) {
                     return rclcpp_action::GoalResponse::REJECT;
                   }
                   std::lock_guard<std::mutex> lock(s->mutex);
                   if (s->max_goals > 0 && static_cast<int64_t>(s->goals.size()) >= s->max_goals) {
                     return rclcpp_action::GoalResponse::REJECT;
                   }
                   return s->goal_policy == 1 ? rclcpp_action::GoalResponse::ACCEPT_AND_DEFER :
                   rclcpp_action::GoalResponse::ACCEPT_AND_EXECUTE;
                 };
               hooks.accepted = [weak](
                 std::shared_ptr<rcl_action_goal_handle_t> handle, const GoalUUID & uuid,
                 MessagePtr request) {
                   auto s = weak.lock();
                   if (!s) {
                     return;
                   }
                   auto goal = std::make_shared<GoalEntity>();
                   goal->server = s;
                   goal->uuid = uuid;
                   goal->handle = std::move(handle);
                   goal->goal = part_of(request, "goal", s->type->goal);
                   goal->feedback_message = Message::create(s->type->feedback_message);
                   write_uuid(s->type->feedback_message->members, goal->feedback_message->data(), uuid);
                   goal->feedback = part_of(goal->feedback_message, "feedback", s->type->feedback);
                   goal->result_response = Message::create(s->type->get_result_response);
                   goal->result = part_of(goal->result_response, "result", s->type->result);
                   int64_t goal_handle = registry().add(goal, s->handle);
                   goal->goal_handle = register_message(goal->goal, goal_handle);
                   goal->feedback_handle = register_message(goal->feedback, goal_handle);
                   goal->result_handle = register_message(goal->result, goal_handle);
                   {
                     std::lock_guard<std::mutex> lock(s->mutex);
                     s->goals[uuid] = goal_handle;
                   }
                   Event event;
                   event.kind = EVENT_GOAL;
                   event.source = s->handle;
                   event.node = s->node->handle;
                   event.id = goal_handle;
                   event.message = goal->goal_handle;
                   event.aux = goal_status(goal->handle.get());
                   s->node->ctx->push(std::move(event));
                 };
               hooks.cancel = [weak](const GoalUUID & uuid) {
                   auto s = weak.lock();
                   if (!s || s->cancel_policy == 1) {
                     return rclcpp_action::CancelResponse::REJECT;
                   }
                   int64_t goal_handle = 0;
                   {
                     std::lock_guard<std::mutex> lock(s->mutex);
                     auto it = s->goals.find(uuid);
                     if (it == s->goals.end()) {
                       return rclcpp_action::CancelResponse::REJECT;
                     }
                     goal_handle = it->second;
                   }
                   auto goal = std::dynamic_pointer_cast<GoalEntity>(registry().find(goal_handle));
                   if (!goal) {
                     return rclcpp_action::CancelResponse::REJECT;
                   }
                   // As rclcpp_action's Server does on ACCEPT: move the goal to
                   // CANCELING; the program then finishes it as canceled (or
                   // succeeded/aborted, if it was too late to stop).
                   if (rcl_action_update_goal_state(goal->handle.get(), GOAL_EVENT_CANCEL_GOAL) != RCL_RET_OK) {
                     rcl_reset_error();
                     return rclcpp_action::CancelResponse::REJECT;
                   }
                   Event event;
                   event.kind = EVENT_CANCEL;
                   event.source = s->handle;
                   event.node = s->node->handle;
                   event.id = goal_handle;
                   event.aux = GOAL_STATUS_CANCELING;
                   s->node->ctx->push(std::move(event));
                   return rclcpp_action::CancelResponse::ACCEPT;
                 };
               e.server = std::make_shared<GenericActionServer>(*node, e.name, e.type, hooks);
               node->waitables()->add_waitable(e.server, nullptr);
             });
           });
}

IRIS_ROS2_API int64_t iris_ros2_action_server_destroy(int64_t srv)
{
  return guarded([&]() -> int64_t {
             registry().get<ActionServerEntity>(srv, "action server");
             registry().destroy(srv);
             return 0;
           });
}

// The goal's request (the action's Goal message). Valid until the goal finishes.
IRIS_ROS2_API int64_t iris_ros2_goal_message(int64_t goal)
{
  return guarded<int64_t>(0, [&]() -> int64_t {return goal_of(goal)->goal_handle;});
}

// The goal's feedback message, to fill and publish (reusable).
IRIS_ROS2_API int64_t iris_ros2_goal_feedback(int64_t goal)
{
  return guarded<int64_t>(0, [&]() -> int64_t {return goal_of(goal)->feedback_handle;});
}

// The goal's result message, to fill before iris_ros2_goal_finish.
IRIS_ROS2_API int64_t iris_ros2_goal_result(int64_t goal)
{
  return guarded<int64_t>(0, [&]() -> int64_t {return goal_of(goal)->result_handle;});
}

// action_msgs/msg/GoalStatus: 1 accepted, 2 executing, 3 canceling,
// 4 succeeded, 5 canceled, 6 aborted.
IRIS_ROS2_API int64_t iris_ros2_goal_status(int64_t goal)
{
  return guarded([&]() -> int64_t {return goal_status(goal_of(goal)->handle.get());});
}

IRIS_ROS2_API int64_t iris_ros2_goal_uuid(int64_t goal)
{
  return guarded([&]() -> int64_t {return set_result(rclcpp_action::to_string(goal_of(goal)->uuid));});
}

// Starts a deferred (accepted) goal.
IRIS_ROS2_API int64_t iris_ros2_goal_execute(int64_t goal_handle)
{
  return guarded([&]() -> int64_t {
             auto goal = goal_of(goal_handle);
             auto server = server_of(*goal);
             check_state(
               rcl_action_update_goal_state(goal->handle.get(), GOAL_EVENT_EXECUTE),
               "cannot execute the goal");
             server->server->publish_status_now();
             return 0;
           });
}

IRIS_ROS2_API int64_t iris_ros2_goal_publish_feedback(int64_t goal_handle)
{
  return guarded([&]() -> int64_t {
             auto goal = goal_of(goal_handle);
             server_of(*goal)->server->publish_feedback_now(goal->feedback_message);
             return 0;
           });
}

// Ends the goal with `status` 4 succeeded, 5 canceled or 6 aborted, sending
// its result message to every client that asked. The goal handle and its
// messages are released.
IRIS_ROS2_API int64_t iris_ros2_goal_finish(int64_t goal_handle, int64_t status)
{
  return guarded([&]() -> int64_t {
             auto goal = goal_of(goal_handle);
             auto server = server_of(*goal);
             rcl_action_goal_handle_t * h = goal->handle.get();
             int8_t current = goal_status(h);
             if (status == GOAL_STATUS_SUCCEEDED || status == GOAL_STATUS_ABORTED) {
               if (current == GOAL_STATUS_ACCEPTED) {
                 check_state(rcl_action_update_goal_state(h, GOAL_EVENT_EXECUTE), "cannot execute the goal");
               }
               check_state(
                 rcl_action_update_goal_state(
                   h, status == GOAL_STATUS_SUCCEEDED ? GOAL_EVENT_SUCCEED : GOAL_EVENT_ABORT),
                 "cannot finish the goal");
             } else if (status == GOAL_STATUS_CANCELED) {
               if (current == GOAL_STATUS_ACCEPTED || current == GOAL_STATUS_EXECUTING) {
                 check_state(rcl_action_update_goal_state(h, GOAL_EVENT_CANCEL_GOAL), "cannot cancel the goal");
               }
               check_state(rcl_action_update_goal_state(h, GOAL_EVENT_CANCELED), "cannot cancel the goal");
             } else {
               throw std::runtime_error(
                 "a goal finishes as succeeded (4), canceled (5) or aborted (6), not " +
                 std::to_string(status));
             }
             set_i64(*goal->result_response, "status", status);
             server->server->finish_now(goal->uuid, goal->result_response);
             {
               std::lock_guard<std::mutex> lock(server->mutex);
               server->goals.erase(goal->uuid);
             }
             registry().destroy(goal_handle);
             return 0;
           });
}

// ===========================================================================
// Action clients
// ===========================================================================

IRIS_ROS2_API int64_t iris_ros2_action_client_create(int64_t node_handle, const char * name, const char * type)
{
  return guarded<int64_t>(0, [&]() -> int64_t {
             auto node = node_of(node_handle);
             auto entity = std::make_shared<ActionClientEntity>();
             entity->node = node;
             entity->type = action_type(type ? type : "");
             entity->name = name ? name : "";
             return create_under(node, entity, [&](ActionClientEntity & e) {
               std::weak_ptr<ActionClientEntity> weak = entity;
               GenericActionClient::Hooks hooks;
               hooks.feedback = [weak](const GoalUUID & uuid, MessagePtr feedback) {
                   auto c = weak.lock();
                   if (!c) {
                     return;
                   }
                   int64_t token = 0;
                   {
                     std::lock_guard<std::mutex> lock(c->mutex);
                     auto it = c->tokens.find(uuid);
                     if (it == c->tokens.end()) {
                       return;  // feedback for another client's goal
                     }
                     token = it->second;
                   }
                   Event event;
                   event.kind = EVENT_FEEDBACK;
                   event.source = c->handle;
                   event.node = c->node->handle;
                   event.id = token;
                   event.payload = std::move(feedback);
                   c->node->ctx->push(std::move(event));
                 };
               hooks.status = [weak](const action_msgs::msg::GoalStatusArray & array) {
                   auto c = weak.lock();
                   if (!c) {
                     return;
                   }
                   std::vector<std::pair<int64_t, int8_t>> changed;
                   {
                     std::lock_guard<std::mutex> lock(c->mutex);
                     for (const auto & s : array.status_list) {
                       auto it = c->tokens.find(s.goal_info.goal_id.uuid);
                       if (it == c->tokens.end()) {
                         continue;
                       }
                       auto & goal = c->goals[it->second];
                       if (goal.status != s.status) {
                         goal.status = s.status;
                         changed.emplace_back(it->second, s.status);
                       }
                     }
                   }
                   for (const auto & change : changed) {
                     Event event;
                     event.kind = EVENT_GOAL_STATUS;
                     event.source = c->handle;
                     event.node = c->node->handle;
                     event.id = change.first;
                     event.aux = change.second;
                     c->node->ctx->push(std::move(event));
                   }
                 };
               e.client = std::make_shared<GenericActionClient>(*node, e.name, e.type, hooks);
               node->waitables()->add_waitable(e.client, nullptr);
             });
           });
}

IRIS_ROS2_API int64_t iris_ros2_action_client_destroy(int64_t cli)
{
  return guarded([&]() -> int64_t {
             action_client_of(cli);
             registry().destroy(cli);
             return 0;
           });
}

// A new Goal message of the client's action type (owned: free it).
IRIS_ROS2_API int64_t iris_ros2_action_new_goal(int64_t cli)
{
  return guarded<int64_t>(0, [&]() -> int64_t {
             return register_message(Message::create(action_client_of(cli)->type->goal));
           });
}

IRIS_ROS2_API int64_t iris_ros2_action_server_ready(int64_t cli)
{
  return guarded([&]() -> int64_t {return action_client_of(cli)->client->action_server_is_ready() ? 1 : 0;});
}

// Waits for the action server on the node's graph events.
//
// Not ClientBase::wait_for_action_server: in Humble its wait loop checks
// rclcpp::ok() against a context it never sets, which falls back to the
// global default context. The bridge gives every IRIS context its own
// rclcpp::Context and never initialises the global one, so that wait gave up
// at once unless the server was already matched. This is the same loop
// against the node's own context.
IRIS_ROS2_API int64_t iris_ros2_action_wait_server(int64_t cli, int64_t timeout_ms)
{
  return guarded([&]() -> int64_t {
             auto c = action_client_of(cli);
             if (c->client->action_server_is_ready()) {
               return 1;
             }
             if (timeout_ms == 0) {
               return 0;
             }
             using Clock = std::chrono::steady_clock;
             const auto deadline = Clock::now() + std::chrono::milliseconds(std::max<int64_t>(timeout_ms, 0));
             auto graph = c->node->graph();
             auto event = graph->get_graph_event();
             while (c->node->ctx->context->is_valid()) {
               auto slice = std::chrono::nanoseconds(std::chrono::milliseconds(100));
               if (timeout_ms > 0) {
                 auto now = Clock::now();
                 if (now >= deadline) {
                   break;
                 }
                 slice = std::min<std::chrono::nanoseconds>(slice, deadline - now);
               }
               graph->wait_for_graph_change(event, slice);
               event->check_and_clear();
               if (c->client->action_server_is_ready()) {
                 return 1;
               }
             }
             return c->client->action_server_is_ready() ? 1 : 0;
           });
}

// Sends a goal (a copy of `goal`); returns its token. The answer arrives as
// EVENT_GOAL_RESPONSE, then EVENT_FEEDBACK / EVENT_GOAL_STATUS while it runs
// and EVENT_RESULT when it ends; the bridge requests the result itself.
IRIS_ROS2_API int64_t iris_ros2_action_send_goal(int64_t cli, int64_t goal_msg)
{
  return guarded([&]() -> int64_t {
             auto c = action_client_of(cli);
             auto goal = message_of(goal_msg);
             if (goal->type().name != c->type->goal->name) {
               throw std::runtime_error(
                 "action '" + c->name + "' takes " + c->type->goal->name + ", not " + goal->type().name);
             }
             auto request = Message::create(c->type->send_goal_request);
             GoalUUID uuid = c->client->new_goal_id();
             write_uuid(c->type->send_goal_request->members, request->data(), uuid);
             copy_message(
               c->type->goal->members,
               field_pointer(c->type->send_goal_request->members, request->data(), "goal", nullptr),
               goal->data());
             int64_t token;
             {
               std::lock_guard<std::mutex> lock(c->mutex);
               token = c->next_token++;
               c->goals[token].uuid = uuid;
               c->tokens[uuid] = token;
             }
             std::weak_ptr<ActionClientEntity> weak = c;
             c->client->send_goal(erased(request), [weak, token, uuid](std::shared_ptr<void> response) {
               auto e = weak.lock();
               if (!e) {
                 return;
               }
               auto answer = view_of(e->type->send_goal_response, response.get());
               bool accepted = get_bool(*answer, "accepted");
               {
                 std::lock_guard<std::mutex> lock(e->mutex);
                 auto & goal = e->goals[token];
                 goal.accepted = accepted ? 1 : 0;
                 if (accepted) {
                   goal.status = std::max<int8_t>(goal.status, GOAL_STATUS_ACCEPTED);
                 } else {
                   goal.done = true;
                 }
               }
               Event event;
               event.kind = EVENT_GOAL_RESPONSE;
               event.source = e->handle;
               event.node = e->node->handle;
               event.id = token;
               event.aux = accepted ? 1 : 0;
               e->node->ctx->push(std::move(event));
               if (!accepted) {
                 return;
               }
               auto result_request = Message::create(e->type->get_result_request);
               write_uuid(e->type->get_result_request->members, result_request->data(), uuid);
               e->client->send_result(erased(result_request), [weak, token](std::shared_ptr<void> reply) {
                 auto c2 = weak.lock();
                 if (!c2) {
                   return;
                 }
                 auto answer2 = view_of(c2->type->get_result_response, reply.get());
                 auto status = static_cast<int8_t>(get_i64(*answer2, "status"));
                 auto result = Message::create(c2->type->result);
                 copy_message(
                   c2->type->result->members, result->data(),
                   field_pointer(c2->type->get_result_response->members, reply.get(), "result", nullptr));
                 {
                   std::lock_guard<std::mutex> lock(c2->mutex);
                   auto & goal = c2->goals[token];
                   goal.status = status;
                   goal.result = result;
                   goal.done = true;
                 }
                 Event event2;
                 event2.kind = EVENT_RESULT;
                 event2.source = c2->handle;
                 event2.node = c2->node->handle;
                 event2.id = token;
                 event2.aux = status;
                 event2.take = [weak, token]() -> MessagePtr {
                     auto c3 = weak.lock();
                     return c3 ? c3->take_result(token) : nullptr;
                   };
                 c2->node->ctx->push(std::move(event2));
               });
             });
             return token;
           });
}

// Asks the server to cancel goal `token`; the answer arrives as
// EVENT_CANCEL_RESPONSE (aux: action_msgs CancelGoal return code, 0 = accepted).
IRIS_ROS2_API int64_t iris_ros2_action_cancel(int64_t cli, int64_t token)
{
  return guarded([&]() -> int64_t {
             auto c = action_client_of(cli);
             GoalUUID uuid;
             {
               std::lock_guard<std::mutex> lock(c->mutex);
               auto it = c->goals.find(token);
               if (it == c->goals.end()) {
                 throw std::runtime_error("no goal " + std::to_string(token) + " on action '" + c->name + "'");
               }
               uuid = it->second.uuid;
             }
             auto request = std::make_shared<action_msgs::srv::CancelGoal::Request>();
             request->goal_info.goal_id.uuid = uuid;
             std::weak_ptr<ActionClientEntity> weak = c;
             c->client->send_cancel(request, [weak, token](std::shared_ptr<void> response) {
               auto e = weak.lock();
               if (!e) {
                 return;
               }
               auto answer = std::static_pointer_cast<action_msgs::srv::CancelGoal::Response>(response);
               Event event;
               event.kind = EVENT_CANCEL_RESPONSE;
               event.source = e->handle;
               event.node = e->node->handle;
               event.id = token;
               event.aux = answer->return_code;
               e->node->ctx->push(std::move(event));
             });
             return 0;
           });
}

// The goal's last known status (GoalStatus values; 0 unknown).
IRIS_ROS2_API int64_t iris_ros2_action_status(int64_t cli, int64_t token)
{
  return guarded([&]() -> int64_t {
             auto c = action_client_of(cli);
             std::lock_guard<std::mutex> lock(c->mutex);
             auto it = c->goals.find(token);
             if (it == c->goals.end()) {
               throw std::runtime_error("no goal " + std::to_string(token) + " on action '" + c->name + "'");
             }
             return it->second.status;
           });
}

// -1 no answer yet, 0 rejected, 1 accepted.
IRIS_ROS2_API int64_t iris_ros2_action_accepted(int64_t cli, int64_t token)
{
  return guarded([&]() -> int64_t {
             auto c = action_client_of(cli);
             std::lock_guard<std::mutex> lock(c->mutex);
             auto it = c->goals.find(token);
             if (it == c->goals.end()) {
               throw std::runtime_error("no goal " + std::to_string(token) + " on action '" + c->name + "'");
             }
             return it->second.accepted;
           });
}

// Runs the executor until goal `token` ends; returns its result (a handle
// living until the next call on this client or the next spin_once), or 0 on
// timeout or rejection. The final status is iris_ros2_action_status.
IRIS_ROS2_API int64_t iris_ros2_action_wait_result(int64_t cli, int64_t token, int64_t timeout_ms)
{
  return guarded([&]() -> int64_t {
             auto c = action_client_of(cli);
             auto ctx = c->node->ctx;
             auto done = [&]() {
                 std::lock_guard<std::mutex> lock(c->mutex);
                 auto it = c->goals.find(token);
                 return it == c->goals.end() || it->second.done;
               };
             spin_until(*ctx, timeout_ms, done);
             auto result = c->take_result(token);
             if (!result) {
               return 0;
             }
             ctx->release_scope(cli);
             return ctx->keep_scoped(result, cli);
           });
}
