// The rclcpp-backed entities shared by the bridge's translation units.
#pragma once

#include <chrono>
#include <deque>
#include <memory>
#include <mutex>
#include <string>
#include <vector>

#include <rclcpp/rclcpp.hpp>
#include <rclcpp_lifecycle/lifecycle_node.hpp>

#include "bridge.hpp"

namespace iris_ros2
{

// A ROS 2 context with its own executor. Every node created in it is added to
// that executor, and every callback the executor runs queues an Event here.
struct ContextEntity : Entity, EventQueue
{
  const char * kind() const override {return "context";}

  rclcpp::Context::SharedPtr context;
  std::shared_ptr<rclcpp::executors::SingleThreadedExecutor> executor;

  // Held while a thread runs the executor; rclcpp executors are not reentrant.
  std::timed_mutex spin_mutex;

  static constexpr size_t MAX_QUEUED_EVENTS = 10000;

  void push(Event event) override;
  // The next deliverable event, resolving late-bound payloads.
  bool pop(Event & out);
  size_t pending();
  // Events discarded because the queue was full.
  uint64_t dropped();

  // Registers `message` as a handle that lives until the next
  // `iris_ros2_spin_once` on this context, or until `scope` is released.
  int64_t keep_scoped(MessagePtr message, int64_t scope);
  // Releases scoped handles: all of them (scope 0) or one scope's.
  void release_scope(int64_t scope);

  // The last event `iris_ros2_spin_once` delivered (for its text).
  Event current;

  ~ContextEntity() override;

private:
  std::mutex queue_mutex_;
  std::deque<Event> queue_;
  uint64_t dropped_ = 0;
  std::vector<std::pair<int64_t, int64_t>> scoped_;  // (scope, handle)
};

// An rclcpp::Node or an rclcpp_lifecycle::LifecycleNode; everything the
// bridge creates on a node goes through its node interfaces, so both kinds
// get the same publishers, services, timers, parameters and tf2 support.
struct NodeEntity : Entity
{
  const char * kind() const override {return "node";}

  std::shared_ptr<ContextEntity> ctx;
  rclcpp::Node::SharedPtr node;
  rclcpp_lifecycle::LifecycleNode::SharedPtr lifecycle;
  rclcpp::node_interfaces::OnSetParametersCallbackHandle::SharedPtr on_set_parameters;

  rclcpp::node_interfaces::NodeBaseInterface::SharedPtr base() const;
  rclcpp::node_interfaces::NodeGraphInterface::SharedPtr graph() const;
  rclcpp::node_interfaces::NodeTopicsInterface::SharedPtr topics() const;
  rclcpp::node_interfaces::NodeServicesInterface::SharedPtr services() const;
  rclcpp::node_interfaces::NodeTimersInterface::SharedPtr timers() const;
  rclcpp::node_interfaces::NodeClockInterface::SharedPtr clock_interface() const;
  rclcpp::node_interfaces::NodeLoggingInterface::SharedPtr logging() const;
  rclcpp::node_interfaces::NodeParametersInterface::SharedPtr parameters() const;
  rclcpp::node_interfaces::NodeWaitablesInterface::SharedPtr waitables() const;
  rclcpp::Clock::SharedPtr clock() const;
  rclcpp::Logger logger() const;

  // False on a lifecycle node outside the active state: its publishers are
  // managed, as rclcpp_lifecycle's are, and drop what they are given.
  bool publishing_enabled() const;

  // The value a lifecycle callback returns (0 success, 1 failure, 2 error),
  // by callback: 1 configure, 2 cleanup, 3 activate, 4 deactivate,
  // 5 shutdown, 6 error.
  int transition_result(int callback) const;
  void set_transition_result(int callback, int result);

  ~NodeEntity() override;

private:
  mutable std::mutex results_mutex_;
  int transition_results_[8] = {0, 0, 0, 0, 0, 0, 0, 0};
};

std::shared_ptr<ContextEntity> context_of(int64_t handle);
std::shared_ptr<NodeEntity> node_of(int64_t handle);

// Runs the context's executor until `ready()` holds or `timeout_ms` passes
// (negative: no limit). Returns `ready()`.
bool spin_until(ContextEntity & ctx, int64_t timeout_ms, const std::function<bool()> & ready);
// Executes work that is already waiting, without blocking.
void spin_ready(ContextEntity & ctx);

// Registers `entity` under its node and returns the handle, or destroys the
// half-built entity and rethrows if `build` fails.
template<class T, class F>
int64_t create_under(const std::shared_ptr<NodeEntity> & node, std::shared_ptr<T> entity, F && build)
{
  int64_t handle = registry().add(entity, node->handle);
  try {
    build(*entity);
  } catch (...) {
    registry().destroy(handle);
    throw;
  }
  return handle;
}

}  // namespace iris_ros2
