// Shared declarations of the IRIS ROS 2 bridge.
//
// IRIS talks to the bridge through `ffi_call_*`: every argument is one int64
// slot (strings arrive as `const char *`, doubles as their bit pattern) and
// every object lives behind an opaque int64 handle. This header defines the
// handle registry, the per-thread status and result string that carry errors
// and text back to IRIS, the runtime-typed message model built on ROS 2's C++
// introspection typesupport, and the event queue that turns rclcpp callbacks
// into values an IRIS program polls for.
#pragma once

#include <cstdint>
#include <deque>
#include <functional>
#include <memory>
#include <mutex>
#include <stdexcept>
#include <string>
#include <unordered_map>
#include <utility>
#include <vector>

#include <rosidl_runtime_c/action_type_support_struct.h>
#include <rosidl_runtime_c/message_type_support_struct.h>
#include <rosidl_runtime_c/service_type_support_struct.h>
#include <rosidl_typesupport_introspection_cpp/message_introspection.hpp>
#include <rosidl_typesupport_introspection_cpp/service_introspection.hpp>

#if defined(_WIN32)
#define IRIS_ROS2_API extern "C" __declspec(dllexport)
#else
#define IRIS_ROS2_API extern "C" __attribute__((visibility("default")))
#endif

namespace iris_ros2
{

namespace intro = rosidl_typesupport_introspection_cpp;

// ---------------------------------------------------------------------------
// Status, errors and the result string (all per thread).
//
// A call that fails records why; `iris_ros2_status()` reports whether the most
// recent call on this thread failed and `iris_ros2_last_error()` returns the
// text. Text results (JSON, names, string fields) are left in the thread's
// result string and copied into an IRIS out-cell by `iris_ros2_result_copy`.
// The bridge never hands IRIS a pointer it would have to free.
// ---------------------------------------------------------------------------
void clear_status();
void record_error(const std::string & message);
int64_t fail(const std::string & message);  // records the error, returns -1
int64_t set_result(std::string text);       // returns the text's length
std::string & result_buffer();
const std::string & last_error_text();
int64_t last_status();

// Runs `body`, turning any C++ exception into a recorded error and `on_error`.
// No exception may cross the C ABI into IRIS code.
template<class T, class F>
T guarded(T on_error, F && body)
{
  clear_status();
  try {
    return body();
  } catch (const std::exception & e) {
    record_error(e.what());
  } catch (...) {
    record_error("unknown C++ exception in the ROS 2 bridge");
  }
  return on_error;
}

template<class F>
int64_t guarded(F && body)
{
  return guarded<int64_t>(-1, std::forward<F>(body));
}

// ROS 2 locates typesupport libraries through the ament index, which lives
// under AMENT_PREFIX_PATH. A sourced environment sets it; when only PATH was
// set up, this points it at the install the ROS 2 libraries were loaded from.
void ensure_ament_prefix_path();

// ---------------------------------------------------------------------------
// Interface types, resolved at run time from ROS 2's own typesupport
// libraries: `rosidl_typesupport_cpp` for the middleware and
// `rosidl_typesupport_introspection_cpp` for field layout.
// ---------------------------------------------------------------------------
struct MessageType
{
  std::string name;  // "pkg/msg/Name"
  const rosidl_message_type_support_t * cpp = nullptr;
  const intro::MessageMembers * members = nullptr;
};
using MessageTypePtr = std::shared_ptr<const MessageType>;

// Accepts "pkg/msg/Name", "pkg/Name" and "pkg::msg::Name". Throws when the
// type cannot be found (unknown package, not built, environment not sourced).
MessageTypePtr message_type(const std::string & name);
MessageTypePtr message_type_of(const intro::MessageMembers * members);
std::string type_name_of(const intro::MessageMembers * members);

struct ServiceType
{
  std::string name;  // "pkg/srv/Name"
  const rosidl_service_type_support_t * cpp = nullptr;
  MessageTypePtr request;
  MessageTypePtr response;
};
std::shared_ptr<const ServiceType> service_type(const std::string & name);

struct ActionType
{
  std::string name;  // "pkg/action/Name"
  const rosidl_action_type_support_t * cpp = nullptr;
  MessageTypePtr goal, result, feedback;
  MessageTypePtr send_goal_request, send_goal_response;
  MessageTypePtr get_result_request, get_result_response;
  MessageTypePtr feedback_message;
};
std::shared_ptr<const ActionType> action_type(const std::string & name);

// ---------------------------------------------------------------------------
// Message values: real C++ message structs, laid out and initialised by the
// type's introspection functions, so rmw and rclcpp see exactly what compiled
// C++ code would hand them.
// ---------------------------------------------------------------------------
class Message
{
public:
  static std::shared_ptr<Message> create(MessageTypePtr type);
  // A message stored inside another one (a field); `owner` keeps the
  // enclosing storage alive for as long as the view exists.
  static std::shared_ptr<Message> view(
    MessageTypePtr type, void * data, std::shared_ptr<const void> owner);

  Message(const Message &) = delete;
  Message & operator=(const Message &) = delete;
  ~Message();

  void * data() const {return data_;}
  const MessageType & type() const {return *type_;}
  const MessageTypePtr & type_ptr() const {return type_;}
  const intro::MessageMembers * members() const {return type_->members;}
  std::shared_ptr<Message> clone() const;

private:
  Message() = default;
  MessageTypePtr type_;
  void * data_ = nullptr;
  bool owned_ = false;
  std::shared_ptr<const void> owner_;
};
using MessagePtr = std::shared_ptr<Message>;

// A type-erased pointer to the message struct that keeps the Message alive;
// the form rclcpp's ServiceBase, ClientBase and action bases traffic in.
inline std::shared_ptr<void> erased(const MessagePtr & message)
{
  return std::shared_ptr<void>(message, message->data());
}

// Field access by path: "linear.x", "ranges[3]", "header.frame_id", or ""
// for the message itself. Setting index N of a sequence of length N appends.
// Every function throws std::runtime_error naming the path on failure.
double get_f64(const Message & m, const std::string & path);
int64_t get_i64(const Message & m, const std::string & path);
bool get_bool(const Message & m, const std::string & path);
std::string get_str(const Message & m, const std::string & path);
std::string get_json(const Message & m, const std::string & path);
void set_f64(Message & m, const std::string & path, double value);
void set_i64(Message & m, const std::string & path, int64_t value);
void set_bool(Message & m, const std::string & path, bool value);
void set_str(Message & m, const std::string & path, const std::string & value);
void set_json(Message & m, const std::string & path, const std::string & json);
int64_t length_of(const Message & m, const std::string & path);
void resize(Message & m, const std::string & path, size_t size);
bool has_path(const Message & m, const std::string & path);
size_t get_f64s(const Message & m, const std::string & path, double * out, size_t max);
void set_f64s(Message & m, const std::string & path, const double * values, size_t count);
std::string schema_json(const MessageType & type);

// Raw-pointer forms used by the service and action bases, which receive
// message structs from rclcpp as `void *`.
void * field_pointer(
  const intro::MessageMembers * members, void * data, const std::string & path,
  const intro::MessageMembers ** nested_out);
void copy_message(const intro::MessageMembers * members, void * dst, const void * src);
std::string message_to_json(const intro::MessageMembers * members, const void * data);

// ---------------------------------------------------------------------------
// Handles.
// ---------------------------------------------------------------------------
struct Entity
{
  virtual ~Entity() = default;
  virtual const char * kind() const = 0;
  int64_t handle = 0;
  int64_t parent = 0;
};

struct MessageEntity : Entity
{
  explicit MessageEntity(MessagePtr m)
  : message(std::move(m)) {}
  const char * kind() const override {return "message";}
  MessagePtr message;
};

class Registry
{
public:
  // Registers `entity` as a child of `parent` (0 for none); destroying the
  // parent destroys its children first.
  int64_t add(std::shared_ptr<Entity> entity, int64_t parent = 0);
  std::shared_ptr<Entity> find(int64_t handle);
  // Removes the entity and its descendants. The objects themselves are
  // released outside the registry lock, children before parents.
  bool destroy(int64_t handle);

  template<class T>
  std::shared_ptr<T> get(int64_t handle, const char * expected)
  {
    auto entity = find(handle);
    if (!entity) {
      throw std::runtime_error(
              std::string("invalid ") + expected + " handle " + std::to_string(handle) +
              " (never created, or already destroyed)");
    }
    auto typed = std::dynamic_pointer_cast<T>(entity);
    if (!typed) {
      throw std::runtime_error(
              "handle " + std::to_string(handle) + " is a " + entity->kind() + ", not a " +
              expected);
    }
    return typed;
  }

private:
  void collect(int64_t handle, std::vector<std::shared_ptr<Entity>> & out);
  std::mutex mutex_;
  std::unordered_map<int64_t, std::shared_ptr<Entity>> entities_;
  std::unordered_map<int64_t, std::vector<int64_t>> children_;
  int64_t next_ = 1;
};

Registry & registry();

MessagePtr message_of(int64_t handle);
int64_t register_message(MessagePtr message, int64_t parent = 0);

// ---------------------------------------------------------------------------
// Events. rclcpp runs callbacks inside the executor; the bridge's callbacks
// only queue an Event, which `iris_ros2_spin_once` hands to IRIS.
// ---------------------------------------------------------------------------
enum EventKind : int64_t
{
  EVENT_NONE = 0,
  EVENT_MESSAGE = 1,          // source: subscription, message: the message
  EVENT_TIMER = 2,            // source: timer, id: times fired
  EVENT_REQUEST = 3,          // source: service, id: request token, message: request, aux: response
  EVENT_RESPONSE = 4,         // source: client, id: request token, message: response
  EVENT_PARAMETER = 5,        // source: node, text: parameter name (changed from outside)
  EVENT_TRANSITION = 6,       // source: node, id: callback (1 configure .. 6 error), aux: result
  EVENT_GOAL = 7,             // source: action server, id: goal handle, message: goal
  EVENT_CANCEL = 8,           // source: action server, id: goal handle
  EVENT_GOAL_RESPONSE = 9,    // source: action client, id: goal token, aux: 1 accepted / 0 rejected
  EVENT_FEEDBACK = 10,        // source: action client, id: goal token, message: feedback
  EVENT_RESULT = 11,          // source: action client, id: goal token, aux: status, message: result
  EVENT_CANCEL_RESPONSE = 12, // source: action client, id: goal token, aux: return code
  EVENT_GOAL_STATUS = 13,     // source: action client, id: goal token, aux: status
};

struct Event
{
  int64_t kind = EVENT_NONE;
  int64_t source = 0;
  int64_t node = 0;
  int64_t id = 0;
  int64_t aux = 0;
  // A message handle that already exists and outlives the event (service
  // requests, goals). Otherwise `payload` (or what `take` returns) is
  // registered when the event is delivered and lives until the next
  // `iris_ros2_spin_once` on the same context.
  int64_t message = 0;
  MessagePtr payload;
  // Late-bound payload: subscription inboxes and client responses can be
  // consumed directly (`take`, a synchronous call) before the event is
  // delivered; an event whose `take` comes back empty is dropped.
  std::function<MessagePtr()> take;
  std::string text;
};

class EventQueue
{
public:
  virtual ~EventQueue() = default;
  virtual void push(Event event) = 0;
};

// Values ROS 2 defines, repeated where the bridge needs them without the
// message headers.
constexpr int8_t GOAL_STATUS_UNKNOWN = 0;
constexpr int8_t GOAL_STATUS_ACCEPTED = 1;
constexpr int8_t GOAL_STATUS_EXECUTING = 2;
constexpr int8_t GOAL_STATUS_CANCELING = 3;
constexpr int8_t GOAL_STATUS_SUCCEEDED = 4;
constexpr int8_t GOAL_STATUS_CANCELED = 5;
constexpr int8_t GOAL_STATUS_ABORTED = 6;

}  // namespace iris_ros2
