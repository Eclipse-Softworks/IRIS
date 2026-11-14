// Per-thread status and result string, and the handle registry.
#include <algorithm>

#include "bridge.hpp"

namespace iris_ros2
{

namespace
{

struct ThreadState
{
  int64_t status = 0;
  std::string error;
  std::string result;
};

ThreadState & thread_state()
{
  thread_local ThreadState state;
  return state;
}

}  // namespace

void clear_status()
{
  thread_state().status = 0;
}

void record_error(const std::string & message)
{
  auto & state = thread_state();
  state.status = 1;
  state.error = message;
}

int64_t fail(const std::string & message)
{
  record_error(message);
  return -1;
}

int64_t set_result(std::string text)
{
  auto & state = thread_state();
  state.result = std::move(text);
  return static_cast<int64_t>(state.result.size());
}

std::string & result_buffer()
{
  return thread_state().result;
}

const std::string & last_error_text()
{
  return thread_state().error;
}

int64_t last_status()
{
  return thread_state().status;
}

// The registry is deliberately never destroyed. Tearing down DDS participants
// from static destructors -- during DLL unload, under the loader lock -- can
// deadlock on Windows; a program that wants a clean shutdown destroys its
// context, and one that simply exits lets the OS reclaim everything.
Registry & registry()
{
  static Registry * instance = new Registry();
  return *instance;
}

int64_t Registry::add(std::shared_ptr<Entity> entity, int64_t parent)
{
  std::lock_guard<std::mutex> lock(mutex_);
  int64_t handle = next_++;
  entity->handle = handle;
  entity->parent = parent;
  entities_[handle] = std::move(entity);
  if (parent != 0) {
    children_[parent].push_back(handle);
  }
  return handle;
}

std::shared_ptr<Entity> Registry::find(int64_t handle)
{
  std::lock_guard<std::mutex> lock(mutex_);
  auto it = entities_.find(handle);
  return it == entities_.end() ? nullptr : it->second;
}

void Registry::collect(int64_t handle, std::vector<std::shared_ptr<Entity>> & out)
{
  auto kids = children_.find(handle);
  if (kids != children_.end()) {
    // Newest first, so dependants created later are released earlier.
    std::vector<int64_t> list = std::move(kids->second);
    children_.erase(kids);
    for (auto it = list.rbegin(); it != list.rend(); ++it) {
      collect(*it, out);
    }
  }
  auto it = entities_.find(handle);
  if (it != entities_.end()) {
    out.push_back(std::move(it->second));
    entities_.erase(it);
  }
}

bool Registry::destroy(int64_t handle)
{
  std::vector<std::shared_ptr<Entity>> doomed;
  {
    std::lock_guard<std::mutex> lock(mutex_);
    auto it = entities_.find(handle);
    if (it == entities_.end()) {
      return false;
    }
    int64_t parent = it->second->parent;
    collect(handle, doomed);
    auto siblings = children_.find(parent);
    if (siblings != children_.end()) {
      auto & list = siblings->second;
      list.erase(std::remove(list.begin(), list.end(), handle), list.end());
    }
  }
  // Destructors run here, without the lock: tearing down rclcpp entities can
  // wait on the executor, which may be calling back into the registry.
  for (auto & entity : doomed) {
    entity.reset();
  }
  return true;
}

MessagePtr message_of(int64_t handle)
{
  return registry().get<MessageEntity>(handle, "message")->message;
}

int64_t register_message(MessagePtr message, int64_t parent)
{
  return registry().add(std::make_shared<MessageEntity>(std::move(message)), parent);
}

}  // namespace iris_ros2
