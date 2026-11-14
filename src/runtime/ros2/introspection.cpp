// Runtime-typed messages on ROS 2's C++ introspection typesupport.
//
// A type name is resolved the way ROS 2 resolves it everywhere else: the
// package's `rosidl_typesupport_cpp` library supplies the middleware-facing
// handle, and its `func` hands out the `rosidl_typesupport_introspection_cpp`
// handle that describes each field's offset, type and container functions.
// Messages are the real C++ structs those describe, so publishing one,
// answering a service with one or completing a goal with one is the same
// operation compiled C++ would perform.
#include <algorithm>
#include <cctype>
#include <charconv>
#include <cmath>
#include <cstring>
#include <limits>
#include <map>
#include <sstream>

#include <rclcpp/typesupport_helpers.hpp>
#include <rcpputils/shared_library.hpp>
#include <rosidl_runtime_cpp/message_initialization.hpp>
#include <rosidl_typesupport_introspection_cpp/field_types.hpp>
#include <rosidl_typesupport_introspection_cpp/identifier.hpp>

#if defined(_WIN32)
#ifndef WIN32_LEAN_AND_MEAN
#define WIN32_LEAN_AND_MEAN
#endif
#ifndef NOMINMAX
#define NOMINMAX
#endif
#include <windows.h>
#else
#include <dlfcn.h>
#include <unistd.h>
#endif

#include <cstdlib>

#include "bridge.hpp"
#include "json.hpp"

namespace iris_ros2
{

namespace
{

// ---------------------------------------------------------------------------
// Type names and typesupport libraries
// ---------------------------------------------------------------------------

struct TypeName
{
  std::string package;
  std::string ns;
  std::string name;
  std::string full() const {return package + "/" + ns + "/" + name;}
};

bool is_identifier(const std::string & s)
{
  if (s.empty()) {
    return false;
  }
  for (char c : s) {
    if (!(std::isalnum(static_cast<unsigned char>(c)) || c == '_')) {
      return false;
    }
  }
  return true;
}

TypeName split_type_name(std::string text, const char * default_ns)
{
  for (size_t pos; (pos = text.find("::")) != std::string::npos; ) {
    text.replace(pos, 2, "/");
  }
  std::vector<std::string> parts;
  std::stringstream stream(text);
  for (std::string part; std::getline(stream, part, '/'); ) {
    parts.push_back(part);
  }
  TypeName t;
  if (parts.size() == 2) {
    t = {parts[0], default_ns, parts[1]};
  } else if (parts.size() == 3) {
    t = {parts[0], parts[1], parts[2]};
  } else {
    throw std::runtime_error(
            "'" + text + "' is not an interface type name; expected package/" + default_ns +
            "/Name");
  }
  if (!is_identifier(t.package) || !is_identifier(t.ns) || !is_identifier(t.name)) {
    throw std::runtime_error("'" + text + "' is not an interface type name");
  }
  return t;
}

std::recursive_mutex & types_mutex()
{
  static auto * m = new std::recursive_mutex();
  return *m;
}

// One `<package>__rosidl_typesupport_cpp` library per package, loaded once and
// kept for the life of the process: typesupport handles point into it.
std::shared_ptr<rcpputils::SharedLibrary> cpp_library(const TypeName & t)
{
  static auto * libraries = new std::map<std::string, std::shared_ptr<rcpputils::SharedLibrary>>();
  auto it = libraries->find(t.package);
  if (it != libraries->end()) {
    return it->second;
  }
  ensure_ament_prefix_path();
  std::shared_ptr<rcpputils::SharedLibrary> library;
  try {
    library = rclcpp::get_typesupport_library(t.full(), "rosidl_typesupport_cpp");
  } catch (const std::exception & e) {
    throw std::runtime_error(
            "cannot load the typesupport for package '" + t.package + "': " + e.what() +
            " (is the ROS 2 environment sourced, and is the package installed?)");
  }
  (*libraries)[t.package] = library;
  return library;
}

const void * introspection_of(
  const char * what, const std::string & name, const void * handle,
  const void * (*lookup)(const void *, const char *))
{
  const void * result = lookup(handle, rosidl_typesupport_introspection_cpp::typesupport_identifier);
  if (!result) {
    throw std::runtime_error(
            std::string("no C++ introspection typesupport for ") + what + " '" + name +
            "' (rosidl_typesupport_introspection_cpp was not built for its package)");
  }
  return result;
}

const intro::MessageMembers * message_members(
  const std::string & name, const rosidl_message_type_support_t * ts)
{
  auto * intro_ts = static_cast<const rosidl_message_type_support_t *>(introspection_of(
      "message", name, ts, [](const void * h, const char * id) -> const void * {
        auto * handle = static_cast<const rosidl_message_type_support_t *>(h);
        return handle->func(handle, id);
      }));
  return static_cast<const intro::MessageMembers *>(intro_ts->data);
}

std::string itanium(const std::string & s)
{
  return std::to_string(s.size()) + s;
}

// The C++ service and action typesupports are exported only as the template
// specialisations `rosidl_typesupport_cpp::get_service_type_support_handle<T>`
// and `get_action_type_support_handle<T>` -- there is no C symbol for them, as
// there is for messages -- so the bridge resolves the specialisation by its
// mangled name. The functions are the ones compiled rclcpp code calls.
std::string service_symbol(const TypeName & t)
{
#if defined(_MSC_VER)
  return "??$get_service_type_support_handle@U" + t.name + "@" + t.ns + "@" + t.package +
         "@@@rosidl_typesupport_cpp@@YAPEBUrosidl_service_type_support_t@@XZ";
#else
  return "_ZN22rosidl_typesupport_cpp31get_service_type_support_handleIN" +
         itanium(t.package) + itanium(t.ns) + itanium(t.name) +
         "EEEPK29rosidl_service_type_support_tv";
#endif
}

std::string action_symbol(const TypeName & t)
{
#if defined(_MSC_VER)
  return "??$get_action_type_support_handle@U" + t.name + "@" + t.ns + "@" + t.package +
         "@@@rosidl_typesupport_cpp@@YAPEBUrosidl_action_type_support_t@@XZ";
#else
  return "_ZN22rosidl_typesupport_cpp30get_action_type_support_handleIN" +
         itanium(t.package) + itanium(t.ns) + itanium(t.name) +
         "EEEPK28rosidl_action_type_support_tv";
#endif
}

#if defined(_MSC_VER)
// MSVC mangling back-references repeated names (`foo/srv/foo`), so a
// constructed name can miss. Fall back to the module's export table and match
// the unambiguous parts.
void * find_export(
  rcpputils::SharedLibrary & library, const std::string & prefix, const std::string & suffix)
{
  HMODULE module = GetModuleHandleA(library.get_library_path().c_str());
  if (!module) {
    return nullptr;
  }
  auto base = reinterpret_cast<const unsigned char *>(module);
  auto dos = reinterpret_cast<const IMAGE_DOS_HEADER *>(base);
  auto nt = reinterpret_cast<const IMAGE_NT_HEADERS *>(base + dos->e_lfanew);
  const auto & dir = nt->OptionalHeader.DataDirectory[IMAGE_DIRECTORY_ENTRY_EXPORT];
  if (dir.Size == 0) {
    return nullptr;
  }
  auto exports = reinterpret_cast<const IMAGE_EXPORT_DIRECTORY *>(base + dir.VirtualAddress);
  auto names = reinterpret_cast<const DWORD *>(base + exports->AddressOfNames);
  for (DWORD i = 0; i < exports->NumberOfNames; ++i) {
    std::string name(reinterpret_cast<const char *>(base + names[i]));
    if (name.size() >= prefix.size() + suffix.size() &&
      name.compare(0, prefix.size(), prefix) == 0 &&
      name.compare(name.size() - suffix.size(), suffix.size(), suffix) == 0)
    {
      return reinterpret_cast<void *>(GetProcAddress(module, name.c_str()));
    }
  }
  return nullptr;
}
#endif

void * typesupport_function(
  const TypeName & t, const std::string & symbol, const char * kind_word, const char * suffix)
{
  auto library = cpp_library(t);
  if (library->has_symbol(symbol)) {
    return library->get_symbol(symbol);
  }
#if defined(_MSC_VER)
  std::string prefix = std::string("??$get_") + kind_word + "_type_support_handle@U" + t.name +
    "@" + t.ns + "@";
  if (void * fn = find_export(*library, prefix, suffix)) {
    return fn;
  }
#else
  (void)suffix;
#endif
  throw std::runtime_error(
          std::string("package '") + t.package + "' has no " + kind_word + " type '" + t.full() +
          "'");
}

// ---------------------------------------------------------------------------
// Field access
// ---------------------------------------------------------------------------

using namespace rosidl_typesupport_introspection_cpp;

std::runtime_error path_error(const std::string & path, const std::string & what)
{
  return std::runtime_error("field '" + path + "': " + what);
}

struct Segment
{
  std::string name;
  bool indexed = false;
  size_t index = 0;
};

std::vector<Segment> parse_path(const std::string & path)
{
  std::vector<Segment> out;
  size_t i = 0;
  const size_t n = path.size();
  while (i < n) {
    Segment seg;
    size_t start = i;
    while (i < n && path[i] != '.' && path[i] != '[') {
      ++i;
    }
    seg.name = path.substr(start, i - start);
    if (seg.name.empty()) {
      throw path_error(path, "empty field name");
    }
    if (i < n && path[i] == '[') {
      size_t digits = ++i;
      while (i < n && std::isdigit(static_cast<unsigned char>(path[i]))) {
        ++i;
      }
      if (i == digits || i >= n || path[i] != ']') {
        throw path_error(path, "malformed index");
      }
      seg.indexed = true;
      seg.index = static_cast<size_t>(std::stoull(path.substr(digits, i - digits)));
      ++i;
    }
    out.push_back(seg);
    if (i < n) {
      if (path[i] != '.' || i + 1 == n) {
        throw path_error(path, "expected '.' between field names");
      }
      ++i;
    }
  }
  return out;
}

const intro::MessageMembers * nested_members(const MessageMember & m)
{
  return static_cast<const intro::MessageMembers *>(m.members_->data);
}

const MessageMember * find_member(const intro::MessageMembers * members, const std::string & name)
{
  for (uint32_t i = 0; i < members->member_count_; ++i) {
    if (name == members->members_[i].name_) {
      return &members->members_[i];
    }
  }
  return nullptr;
}

std::string field_list(const intro::MessageMembers * members)
{
  std::string out;
  for (uint32_t i = 0; i < members->member_count_; ++i) {
    out += (i ? ", " : "") + std::string(members->members_[i].name_);
  }
  return out;
}

bool is_fixed_array(const MessageMember & m)
{
  return m.is_array_ && !m.is_upper_bound_ && m.array_size_ > 0;
}

size_t container_size(const MessageMember & m, const void * container)
{
  if (m.size_function) {
    return m.size_function(container);
  }
  if (is_fixed_array(m)) {
    return m.array_size_;
  }
  throw std::runtime_error(std::string("field '") + m.name_ + "' has no size function");
}

void resize_container(const MessageMember & m, void * container, size_t size, const std::string & path)
{
  if (is_fixed_array(m)) {
    if (size != m.array_size_) {
      throw path_error(path, "a fixed array of " + std::to_string(m.array_size_) + " elements");
    }
    return;
  }
  if (m.is_upper_bound_ && size > m.array_size_) {
    throw path_error(path, "holds at most " + std::to_string(m.array_size_) + " elements");
  }
  if (!m.resize_function) {
    throw path_error(path, "cannot be resized");
  }
  m.resize_function(container, size);
}

struct Slot
{
  enum Kind { MESSAGE, FIELD, CONTAINER, ELEMENT } kind = MESSAGE;
  const intro::MessageMembers * members = nullptr;  // MESSAGE
  const MessageMember * member = nullptr;           // FIELD, CONTAINER, ELEMENT
  void * ptr = nullptr;        // the message, field, container or element
  void * container = nullptr;  // ELEMENT: its container (for fetch/assign)
  size_t index = 0;            // ELEMENT
};

Slot resolve(const intro::MessageMembers * members, void * data, const std::string & path, bool grow)
{
  Slot slot;
  slot.members = members;
  slot.ptr = data;
  if (path.empty()) {
    return slot;
  }
  std::string walked;
  for (const auto & seg : parse_path(path)) {
    if (slot.kind != Slot::MESSAGE) {
      throw path_error(path, "'" + walked + "' is not a message");
    }
    const MessageMember * m = find_member(slot.members, seg.name);
    if (!m) {
      throw path_error(
              path, type_name_of(slot.members) + " has no field '" + seg.name + "' (fields: " +
              field_list(slot.members) + ")");
    }
    walked += (walked.empty() ? "" : ".") + seg.name;
    void * field = static_cast<char *>(slot.ptr) + m->offset_;
    if (m->is_array_) {
      if (!seg.indexed) {
        slot = Slot{Slot::CONTAINER, nullptr, m, field};
        continue;
      }
      size_t size = container_size(*m, field);
      if (seg.index >= size) {
        if (grow && seg.index == size && !is_fixed_array(*m)) {
          resize_container(*m, field, size + 1, path);
        } else {
          throw path_error(
                  path, "index " + std::to_string(seg.index) + " is out of range for '" + walked +
                  "' of length " + std::to_string(size));
        }
      }
      walked += "[" + std::to_string(seg.index) + "]";
      void * element = m->get_function ? m->get_function(field, seg.index) : nullptr;
      if (m->type_id_ == ROS_TYPE_MESSAGE) {
        if (!element) {
          throw path_error(path, "elements of '" + walked + "' are not addressable");
        }
        slot = Slot{Slot::MESSAGE, nested_members(*m), nullptr, element};
      } else {
        slot = Slot{Slot::ELEMENT, nullptr, m, element, field, seg.index};
      }
    } else {
      if (seg.indexed) {
        throw path_error(path, "'" + walked + "' is not an array");
      }
      if (m->type_id_ == ROS_TYPE_MESSAGE) {
        slot = Slot{Slot::MESSAGE, nested_members(*m), nullptr, field};
      } else {
        slot = Slot{Slot::FIELD, nullptr, m, field};
      }
    }
  }
  return slot;
}

// ---- scalars ----

struct Scalar
{
  enum Kind { BOOL, INT, UINT, FLOAT } kind = INT;
  bool b = false;
  int64_t i = 0;
  uint64_t u = 0;
  double f = 0.0;

  static Scalar of_bool(bool v) {Scalar s; s.kind = BOOL; s.b = v; return s;}
  static Scalar of_int(int64_t v) {Scalar s; s.kind = INT; s.i = v; return s;}
  static Scalar of_uint(uint64_t v) {Scalar s; s.kind = UINT; s.u = v; return s;}
  static Scalar of_float(double v) {Scalar s; s.kind = FLOAT; s.f = v; return s;}
};

bool is_string_type(uint8_t type)
{
  return type == ROS_TYPE_STRING || type == ROS_TYPE_WSTRING;
}

const char * type_word(uint8_t type)
{
  switch (type) {
    case ROS_TYPE_FLOAT: return "float32";
    case ROS_TYPE_DOUBLE: return "float64";
    case ROS_TYPE_LONG_DOUBLE: return "long double";
    case ROS_TYPE_CHAR: return "char";
    case ROS_TYPE_WCHAR: return "wchar";
    case ROS_TYPE_BOOLEAN: return "bool";
    case ROS_TYPE_OCTET: return "byte";
    case ROS_TYPE_UINT8: return "uint8";
    case ROS_TYPE_INT8: return "int8";
    case ROS_TYPE_UINT16: return "uint16";
    case ROS_TYPE_INT16: return "int16";
    case ROS_TYPE_UINT32: return "uint32";
    case ROS_TYPE_INT32: return "int32";
    case ROS_TYPE_UINT64: return "uint64";
    case ROS_TYPE_INT64: return "int64";
    case ROS_TYPE_STRING: return "string";
    case ROS_TYPE_WSTRING: return "wstring";
    case ROS_TYPE_MESSAGE: return "message";
  }
  return "unknown";
}

size_t scalar_size(uint8_t type)
{
  switch (type) {
    case ROS_TYPE_FLOAT: return sizeof(float);
    case ROS_TYPE_DOUBLE: return sizeof(double);
    case ROS_TYPE_LONG_DOUBLE: return sizeof(long double);
    case ROS_TYPE_CHAR:
    case ROS_TYPE_OCTET:
    case ROS_TYPE_UINT8:
    case ROS_TYPE_INT8: return 1;
    case ROS_TYPE_BOOLEAN: return sizeof(bool);
    case ROS_TYPE_WCHAR:
    case ROS_TYPE_UINT16:
    case ROS_TYPE_INT16: return 2;
    case ROS_TYPE_UINT32:
    case ROS_TYPE_INT32: return 4;
    case ROS_TYPE_UINT64:
    case ROS_TYPE_INT64: return 8;
  }
  return 0;
}

Scalar load(uint8_t type, const void * p)
{
  switch (type) {
    case ROS_TYPE_FLOAT: return Scalar::of_float(*static_cast<const float *>(p));
    case ROS_TYPE_DOUBLE: return Scalar::of_float(*static_cast<const double *>(p));
    case ROS_TYPE_LONG_DOUBLE:
      return Scalar::of_float(static_cast<double>(*static_cast<const long double *>(p)));
    case ROS_TYPE_BOOLEAN: return Scalar::of_bool(*static_cast<const bool *>(p));
    case ROS_TYPE_CHAR:
    case ROS_TYPE_OCTET:
    case ROS_TYPE_UINT8: return Scalar::of_uint(*static_cast<const uint8_t *>(p));
    case ROS_TYPE_INT8: return Scalar::of_int(*static_cast<const int8_t *>(p));
    case ROS_TYPE_WCHAR:
    case ROS_TYPE_UINT16: return Scalar::of_uint(*static_cast<const uint16_t *>(p));
    case ROS_TYPE_INT16: return Scalar::of_int(*static_cast<const int16_t *>(p));
    case ROS_TYPE_UINT32: return Scalar::of_uint(*static_cast<const uint32_t *>(p));
    case ROS_TYPE_INT32: return Scalar::of_int(*static_cast<const int32_t *>(p));
    case ROS_TYPE_UINT64: return Scalar::of_uint(*static_cast<const uint64_t *>(p));
    case ROS_TYPE_INT64: return Scalar::of_int(*static_cast<const int64_t *>(p));
  }
  throw std::runtime_error(std::string("not a numeric field (") + type_word(type) + ")");
}

template<class T>
void store_integer(void * p, const Scalar & s, uint8_t type, const std::string & path)
{
  constexpr auto lo = std::numeric_limits<T>::min();
  constexpr auto hi = std::numeric_limits<T>::max();
  bool ok = false;
  T value = 0;
  if (s.kind == Scalar::INT) {
    ok = std::is_signed<T>::value ?
      (s.i >= static_cast<int64_t>(lo) && s.i <= static_cast<int64_t>(hi)) :
      (s.i >= 0 && static_cast<uint64_t>(s.i) <= static_cast<uint64_t>(hi));
    value = static_cast<T>(s.i);
  } else if (s.kind == Scalar::UINT) {
    ok = s.u <= static_cast<uint64_t>(hi);
    value = static_cast<T>(s.u);
  } else if (s.kind == Scalar::FLOAT) {
    ok = std::isfinite(s.f) && std::floor(s.f) == s.f &&
      s.f >= static_cast<double>(lo) && s.f <= static_cast<double>(hi);
    value = static_cast<T>(s.f);
  } else if (s.kind == Scalar::BOOL) {
    throw path_error(path, std::string("is ") + type_word(type) + ", not bool");
  }
  if (!ok) {
    throw path_error(path, std::string("value out of range for ") + type_word(type));
  }
  std::memcpy(p, &value, sizeof(T));
}

void store(uint8_t type, void * p, const Scalar & s, const std::string & path)
{
  auto as_double = [&]() -> double {
      switch (s.kind) {
        case Scalar::FLOAT: return s.f;
        case Scalar::INT: return static_cast<double>(s.i);
        case Scalar::UINT: return static_cast<double>(s.u);
        case Scalar::BOOL: break;
      }
      throw path_error(path, std::string("is ") + type_word(type) + ", not bool");
    };
  switch (type) {
    case ROS_TYPE_FLOAT: *static_cast<float *>(p) = static_cast<float>(as_double()); return;
    case ROS_TYPE_DOUBLE: *static_cast<double *>(p) = as_double(); return;
    case ROS_TYPE_LONG_DOUBLE:
      *static_cast<long double *>(p) = static_cast<long double>(as_double());
      return;
    case ROS_TYPE_BOOLEAN:
      if (s.kind == Scalar::BOOL) {
        *static_cast<bool *>(p) = s.b;
      } else if ((s.kind == Scalar::INT && (s.i == 0 || s.i == 1)) ||
        (s.kind == Scalar::UINT && s.u <= 1))
      {
        *static_cast<bool *>(p) = s.kind == Scalar::INT ? s.i != 0 : s.u != 0;
      } else {
        throw path_error(path, "is bool");
      }
      return;
    case ROS_TYPE_CHAR:
    case ROS_TYPE_OCTET:
    case ROS_TYPE_UINT8: store_integer<uint8_t>(p, s, type, path); return;
    case ROS_TYPE_INT8: store_integer<int8_t>(p, s, type, path); return;
    case ROS_TYPE_WCHAR:
    case ROS_TYPE_UINT16: store_integer<uint16_t>(p, s, type, path); return;
    case ROS_TYPE_INT16: store_integer<int16_t>(p, s, type, path); return;
    case ROS_TYPE_UINT32: store_integer<uint32_t>(p, s, type, path); return;
    case ROS_TYPE_INT32: store_integer<int32_t>(p, s, type, path); return;
    case ROS_TYPE_UINT64: store_integer<uint64_t>(p, s, type, path); return;
    case ROS_TYPE_INT64: store_integer<int64_t>(p, s, type, path); return;
  }
  throw path_error(path, std::string("is ") + type_word(type) + ", not a number");
}

// Elements of `std::vector<bool>` have no address; their introspection data
// offers only fetch/assign, which copy through a value buffer.
Scalar read_scalar(const Slot & slot, const std::string & path)
{
  if (slot.kind == Slot::MESSAGE || slot.kind == Slot::CONTAINER) {
    throw path_error(
            path, slot.kind == Slot::MESSAGE ? "is a message, not a scalar" :
            "is an array; index it or use msg_len / msg_get_json");
  }
  uint8_t type = slot.member->type_id_;
  if (is_string_type(type)) {
    throw path_error(path, std::string("is ") + type_word(type) + ", not a number");
  }
  if (slot.ptr) {
    return load(type, slot.ptr);
  }
  alignas(16) unsigned char buffer[16] = {};
  slot.member->fetch_function(slot.container, slot.index, buffer);
  return load(type, buffer);
}

void write_scalar(const Slot & slot, const Scalar & value, const std::string & path)
{
  if (slot.kind == Slot::MESSAGE || slot.kind == Slot::CONTAINER) {
    throw path_error(
            path, slot.kind == Slot::MESSAGE ? "is a message, not a scalar" :
            "is an array; index it or use msg_set_json");
  }
  uint8_t type = slot.member->type_id_;
  if (is_string_type(type)) {
    throw path_error(path, std::string("is ") + type_word(type) + ", not a number");
  }
  if (slot.ptr) {
    store(type, slot.ptr, value, path);
    return;
  }
  alignas(16) unsigned char buffer[16] = {};
  store(type, buffer, value, path);
  slot.member->assign_function(slot.container, slot.index, buffer);
}

// ---- strings ----

std::string utf16_to_utf8(const std::u16string & in)
{
  std::string out;
  for (size_t i = 0; i < in.size(); ++i) {
    uint32_t cp = in[i];
    if (cp >= 0xD800 && cp <= 0xDBFF && i + 1 < in.size()) {
      uint32_t low = in[i + 1];
      if (low >= 0xDC00 && low <= 0xDFFF) {
        cp = 0x10000 + ((cp - 0xD800) << 10) + (low - 0xDC00);
        ++i;
      }
    }
    if (cp < 0x80) {
      out.push_back(static_cast<char>(cp));
    } else if (cp < 0x800) {
      out.push_back(static_cast<char>(0xC0 | (cp >> 6)));
      out.push_back(static_cast<char>(0x80 | (cp & 0x3F)));
    } else if (cp < 0x10000) {
      out.push_back(static_cast<char>(0xE0 | (cp >> 12)));
      out.push_back(static_cast<char>(0x80 | ((cp >> 6) & 0x3F)));
      out.push_back(static_cast<char>(0x80 | (cp & 0x3F)));
    } else {
      out.push_back(static_cast<char>(0xF0 | (cp >> 18)));
      out.push_back(static_cast<char>(0x80 | ((cp >> 12) & 0x3F)));
      out.push_back(static_cast<char>(0x80 | ((cp >> 6) & 0x3F)));
      out.push_back(static_cast<char>(0x80 | (cp & 0x3F)));
    }
  }
  return out;
}

std::u16string utf8_to_utf16(const std::string & in)
{
  std::u16string out;
  for (size_t i = 0; i < in.size(); ) {
    unsigned char c = static_cast<unsigned char>(in[i]);
    uint32_t cp = 0;
    size_t extra = 0;
    if (c < 0x80) {
      cp = c;
    } else if ((c >> 5) == 0x6) {
      cp = c & 0x1F;
      extra = 1;
    } else if ((c >> 4) == 0xE) {
      cp = c & 0x0F;
      extra = 2;
    } else {
      cp = c & 0x07;
      extra = 3;
    }
    ++i;
    for (size_t k = 0; k < extra && i < in.size(); ++k, ++i) {
      cp = (cp << 6) | (static_cast<unsigned char>(in[i]) & 0x3F);
    }
    if (cp >= 0x10000) {
      cp -= 0x10000;
      out.push_back(static_cast<char16_t>(0xD800 + (cp >> 10)));
      out.push_back(static_cast<char16_t>(0xDC00 + (cp & 0x3FF)));
    } else {
      out.push_back(static_cast<char16_t>(cp));
    }
  }
  return out;
}

std::string read_string(const Slot & slot, const std::string & path)
{
  if (slot.kind != Slot::FIELD && slot.kind != Slot::ELEMENT) {
    throw path_error(path, "is not a string field");
  }
  uint8_t type = slot.member->type_id_;
  if (type == ROS_TYPE_STRING) {
    return *static_cast<const std::string *>(slot.ptr);
  }
  if (type == ROS_TYPE_WSTRING) {
    return utf16_to_utf8(*static_cast<const std::u16string *>(slot.ptr));
  }
  throw path_error(path, std::string("is ") + type_word(type) + ", not a string");
}

void write_string(const Slot & slot, const std::string & value, const std::string & path)
{
  if (slot.kind != Slot::FIELD && slot.kind != Slot::ELEMENT) {
    throw path_error(path, "is not a string field");
  }
  const MessageMember & m = *slot.member;
  if (m.string_upper_bound_ > 0 && value.size() > m.string_upper_bound_) {
    throw path_error(path, "holds at most " + std::to_string(m.string_upper_bound_) + " characters");
  }
  if (m.type_id_ == ROS_TYPE_STRING) {
    *static_cast<std::string *>(slot.ptr) = value;
  } else if (m.type_id_ == ROS_TYPE_WSTRING) {
    *static_cast<std::u16string *>(slot.ptr) = utf8_to_utf16(value);
  } else {
    throw path_error(path, std::string("is ") + type_word(m.type_id_) + ", not a string");
  }
}

// ---- JSON out ----

void write_message_json(std::string & out, const intro::MessageMembers * members, const void * data);

void write_scalar_json(std::string & out, uint8_t type, const void * p)
{
  if (type == ROS_TYPE_STRING) {
    write_json_string(out, *static_cast<const std::string *>(p));
    return;
  }
  if (type == ROS_TYPE_WSTRING) {
    write_json_string(out, utf16_to_utf8(*static_cast<const std::u16string *>(p)));
    return;
  }
  Scalar s = load(type, p);
  switch (s.kind) {
    case Scalar::BOOL: out += s.b ? "true" : "false"; break;
    case Scalar::INT: out += std::to_string(s.i); break;
    case Scalar::UINT: out += std::to_string(s.u); break;
    case Scalar::FLOAT:
      if (type == ROS_TYPE_FLOAT) {
        // Print the float32 value, not its widened double digits.
        char buf[32];
        auto r = std::to_chars(buf, buf + sizeof(buf), static_cast<float>(s.f));
        std::string text(buf, r.ptr);
        if (!std::isfinite(s.f)) {
          out += "null";
        } else {
          out += text.find_first_of(".eEn") == std::string::npos ? text + ".0" : text;
        }
      } else {
        write_json_double(out, s.f);
      }
      break;
  }
}

void write_element_json(std::string & out, const MessageMember & m, const void * container, size_t i)
{
  const void * element = m.get_const_function ? m.get_const_function(container, i) : nullptr;
  if (m.type_id_ == ROS_TYPE_MESSAGE) {
    write_message_json(out, nested_members(m), element);
    return;
  }
  if (element) {
    write_scalar_json(out, m.type_id_, element);
    return;
  }
  alignas(16) unsigned char buffer[16] = {};
  m.fetch_function(container, i, buffer);
  write_scalar_json(out, m.type_id_, buffer);
}

void write_member_json(std::string & out, const MessageMember & m, const void * field)
{
  if (m.is_array_) {
    size_t n = container_size(m, field);
    out.push_back('[');
    for (size_t i = 0; i < n; ++i) {
      if (i) {
        out.push_back(',');
      }
      write_element_json(out, m, field, i);
    }
    out.push_back(']');
  } else if (m.type_id_ == ROS_TYPE_MESSAGE) {
    write_message_json(out, nested_members(m), field);
  } else {
    write_scalar_json(out, m.type_id_, field);
  }
}

void write_message_json(std::string & out, const intro::MessageMembers * members, const void * data)
{
  out.push_back('{');
  for (uint32_t i = 0; i < members->member_count_; ++i) {
    const MessageMember & m = members->members_[i];
    if (i) {
      out.push_back(',');
    }
    write_json_string(out, m.name_);
    out.push_back(':');
    write_member_json(out, m, static_cast<const char *>(data) + m.offset_);
  }
  out.push_back('}');
}

void write_slot_json(std::string & out, const Slot & slot)
{
  switch (slot.kind) {
    case Slot::MESSAGE: write_message_json(out, slot.members, slot.ptr); return;
    case Slot::CONTAINER: write_member_json(out, *slot.member, slot.ptr); return;
    case Slot::FIELD: write_scalar_json(out, slot.member->type_id_, slot.ptr); return;
    case Slot::ELEMENT:
      if (slot.ptr) {
        write_scalar_json(out, slot.member->type_id_, slot.ptr);
      } else {
        alignas(16) unsigned char buffer[16] = {};
        slot.member->fetch_function(slot.container, slot.index, buffer);
        write_scalar_json(out, slot.member->type_id_, buffer);
      }
      return;
  }
}

// ---- JSON in ----

void read_message_json(
  const intro::MessageMembers * members, void * data, const Json & j, const std::string & where);

Scalar scalar_from_json(const Json & j, uint8_t type, const std::string & where)
{
  bool floating = type == ROS_TYPE_FLOAT || type == ROS_TYPE_DOUBLE ||
    type == ROS_TYPE_LONG_DOUBLE;
  switch (j.kind) {
    case Json::Kind::Bool: return Scalar::of_bool(j.boolean);
    case Json::Kind::Int: return Scalar::of_int(j.int_value);
    case Json::Kind::UInt: return Scalar::of_uint(j.uint_value);
    case Json::Kind::Float: return Scalar::of_float(j.float_value);
    case Json::Kind::Null:
      if (floating) {
        return Scalar::of_float(std::numeric_limits<double>::quiet_NaN());
      }
      break;
    case Json::Kind::String:
      if (floating) {
        if (j.text == "nan" || j.text == "NaN") {
          return Scalar::of_float(std::numeric_limits<double>::quiet_NaN());
        }
        if (j.text == "inf" || j.text == "Infinity") {
          return Scalar::of_float(std::numeric_limits<double>::infinity());
        }
        if (j.text == "-inf" || j.text == "-Infinity") {
          return Scalar::of_float(-std::numeric_limits<double>::infinity());
        }
      }
      break;
    default:
      break;
  }
  throw path_error(where, std::string("expected ") + type_word(type) + ", got " + j.kind_name());
}

void read_value_json(uint8_t type, const MessageMember & m, void * p, const Json & j, const std::string & where)
{
  if (type == ROS_TYPE_STRING || type == ROS_TYPE_WSTRING) {
    if (j.kind != Json::Kind::String) {
      throw path_error(where, std::string("expected a string, got ") + j.kind_name());
    }
    Slot slot{Slot::FIELD, nullptr, &m, p};
    write_string(slot, j.text, where);
    return;
  }
  store(type, p, scalar_from_json(j, type, where), where);
}

void read_element_json(const MessageMember & m, void * container, size_t i, const Json & j, const std::string & where)
{
  void * element = m.get_function ? m.get_function(container, i) : nullptr;
  if (m.type_id_ == ROS_TYPE_MESSAGE) {
    read_message_json(nested_members(m), element, j, where);
    return;
  }
  if (element) {
    read_value_json(m.type_id_, m, element, j, where);
    return;
  }
  alignas(16) unsigned char buffer[16] = {};
  store(m.type_id_, buffer, scalar_from_json(j, m.type_id_, where), where);
  m.assign_function(container, i, buffer);
}

void read_member_json(const MessageMember & m, void * field, const Json & j, const std::string & where)
{
  if (m.is_array_) {
    if (j.kind != Json::Kind::Array) {
      throw path_error(where, std::string("expected an array, got ") + j.kind_name());
    }
    resize_container(m, field, j.items.size(), where);
    for (size_t i = 0; i < j.items.size(); ++i) {
      read_element_json(m, field, i, j.items[i], where + "[" + std::to_string(i) + "]");
    }
  } else if (m.type_id_ == ROS_TYPE_MESSAGE) {
    read_message_json(nested_members(m), field, j, where);
  } else {
    read_value_json(m.type_id_, m, field, j, where);
  }
}

// Fields absent from the JSON object keep their current value, the way
// `ros2 topic pub` fills in what a YAML message leaves out; unknown fields are
// an error, because a misspelled field silently ignored is a command not sent.
void read_message_json(
  const intro::MessageMembers * members, void * data, const Json & j, const std::string & where)
{
  if (j.kind != Json::Kind::Object) {
    throw path_error(
            where.empty() ? type_name_of(members) : where,
            std::string("expected an object, got ") + j.kind_name());
  }
  for (const auto & field : j.fields) {
    const MessageMember * m = find_member(members, field.first);
    std::string here = where.empty() ? field.first : where + "." + field.first;
    if (!m) {
      throw path_error(
              here, type_name_of(members) + " has no such field (fields: " +
              field_list(members) + ")");
    }
    read_member_json(*m, static_cast<char *>(data) + m->offset_, field.second, here);
  }
}

void read_slot_json(const Slot & slot, const Json & j, const std::string & path)
{
  switch (slot.kind) {
    case Slot::MESSAGE: read_message_json(slot.members, slot.ptr, j, path); return;
    case Slot::CONTAINER: read_member_json(*slot.member, slot.ptr, j, path); return;
    case Slot::FIELD: read_value_json(slot.member->type_id_, *slot.member, slot.ptr, j, path); return;
    case Slot::ELEMENT:
      if (is_string_type(slot.member->type_id_)) {
        read_value_json(slot.member->type_id_, *slot.member, slot.ptr, j, path);
      } else {
        write_scalar(slot, scalar_from_json(j, slot.member->type_id_, path), path);
      }
      return;
  }
}

// ---- copying ----

void copy_element(const MessageMember & m, void * dst, const void * src, size_t i)
{
  if (m.type_id_ == ROS_TYPE_MESSAGE) {
    copy_message(nested_members(m), m.get_function(dst, i), m.get_const_function(src, i));
  } else if (m.type_id_ == ROS_TYPE_STRING) {
    *static_cast<std::string *>(m.get_function(dst, i)) =
      *static_cast<const std::string *>(m.get_const_function(src, i));
  } else if (m.type_id_ == ROS_TYPE_WSTRING) {
    *static_cast<std::u16string *>(m.get_function(dst, i)) =
      *static_cast<const std::u16string *>(m.get_const_function(src, i));
  } else if (m.get_function && m.get_const_function) {
    std::memcpy(m.get_function(dst, i), m.get_const_function(src, i), scalar_size(m.type_id_));
  } else {
    alignas(16) unsigned char buffer[16] = {};
    m.fetch_function(src, i, buffer);
    m.assign_function(dst, i, buffer);
  }
}

}  // namespace

// ---------------------------------------------------------------------------
// Public: environment
// ---------------------------------------------------------------------------

void ensure_ament_prefix_path()
{
  const char * existing = std::getenv("AMENT_PREFIX_PATH");
  if (existing && *existing) {
    return;
  }
  // The module rclcpp was loaded from: <prefix>/bin/rclcpp.dll on Windows,
  // <prefix>/lib/librclcpp.so elsewhere. Its install prefix is the ament
  // prefix a sourced environment would have set.
  std::string library;
#if defined(_WIN32)
  HMODULE module = nullptr;
  if (!GetModuleHandleExA(
      GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS | GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT,
      reinterpret_cast<LPCSTR>(&rclcpp::get_typesupport_library), &module))
  {
    return;
  }
  char path[MAX_PATH];
  DWORD n = GetModuleFileNameA(module, path, MAX_PATH);
  if (n == 0 || n >= MAX_PATH) {
    return;
  }
  library.assign(path, n);
#else
  Dl_info info;
  if (!dladdr(reinterpret_cast<void *>(&rclcpp::get_typesupport_library), &info) ||
    !info.dli_fname)
  {
    return;
  }
  library = info.dli_fname;
#endif
  size_t dir = library.find_last_of("\\/");
  if (dir == std::string::npos || dir == 0) {
    return;
  }
  size_t root = library.find_last_of("\\/", dir - 1);
  if (root == std::string::npos) {
    return;
  }
  std::string prefix = library.substr(0, root);
  std::string index = prefix + "/share/ament_index/resource_index/packages";
#if defined(_WIN32)
  if (GetFileAttributesA(index.c_str()) == INVALID_FILE_ATTRIBUTES) {
    return;
  }
  _putenv_s("AMENT_PREFIX_PATH", prefix.c_str());
#else
  if (access(index.c_str(), F_OK) != 0) {
    return;
  }
  setenv("AMENT_PREFIX_PATH", prefix.c_str(), 0);
#endif
}

// ---------------------------------------------------------------------------
// Public: types
// ---------------------------------------------------------------------------

std::string type_name_of(const intro::MessageMembers * members)
{
  std::string ns = members->message_namespace_;
  for (size_t pos; (pos = ns.find("::")) != std::string::npos; ) {
    ns.replace(pos, 2, "/");
  }
  return ns + "/" + members->message_name_;
}

MessageTypePtr message_type(const std::string & name)
{
  TypeName t = split_type_name(name, "msg");
  const std::string full = t.full();
  std::lock_guard<std::recursive_mutex> lock(types_mutex());
  static auto * cache = new std::unordered_map<std::string, MessageTypePtr>();
  auto it = cache->find(full);
  if (it != cache->end()) {
    return it->second;
  }
  auto library = cpp_library(t);
  const rosidl_message_type_support_t * ts = nullptr;
  try {
    ts = rclcpp::get_typesupport_handle(full, "rosidl_typesupport_cpp", *library);
  } catch (const std::exception &) {
    throw std::runtime_error("package '" + t.package + "' has no message type '" + full + "'");
  }
  auto type = std::make_shared<MessageType>();
  type->name = full;
  type->cpp = ts;
  type->members = message_members(full, ts);
  (*cache)[full] = type;
  return type;
}

MessageTypePtr message_type_of(const intro::MessageMembers * members)
{
  return message_type(type_name_of(members));
}

std::shared_ptr<const ServiceType> service_type(const std::string & name)
{
  TypeName t = split_type_name(name, "srv");
  const std::string full = t.full();
  std::lock_guard<std::recursive_mutex> lock(types_mutex());
  static auto * cache = new std::unordered_map<std::string, std::shared_ptr<const ServiceType>>();
  auto it = cache->find(full);
  if (it != cache->end()) {
    return it->second;
  }
  using Fn = const rosidl_service_type_support_t * (*)();
  auto fn = reinterpret_cast<Fn>(typesupport_function(
      t, service_symbol(t), "service",
      "@rosidl_typesupport_cpp@@YAPEBUrosidl_service_type_support_t@@XZ"));
  auto type = std::make_shared<ServiceType>();
  type->name = full;
  type->cpp = fn();
  if (!type->cpp) {
    throw std::runtime_error("the typesupport for '" + full + "' returned no handle");
  }
  // Request and response are ordinary message types ("pkg/srv/Name_Request").
  type->request = message_type(full + "_Request");
  type->response = message_type(full + "_Response");
  (*cache)[full] = type;
  return type;
}

std::shared_ptr<const ActionType> action_type(const std::string & name)
{
  TypeName t = split_type_name(name, "action");
  const std::string full = t.full();
  std::lock_guard<std::recursive_mutex> lock(types_mutex());
  static auto * cache = new std::unordered_map<std::string, std::shared_ptr<const ActionType>>();
  auto it = cache->find(full);
  if (it != cache->end()) {
    return it->second;
  }
  using Fn = const rosidl_action_type_support_t * (*)();
  auto fn = reinterpret_cast<Fn>(typesupport_function(
      t, action_symbol(t), "action",
      "@rosidl_typesupport_cpp@@YAPEBUrosidl_action_type_support_t@@XZ"));
  auto type = std::make_shared<ActionType>();
  type->name = full;
  type->cpp = fn();
  if (!type->cpp) {
    throw std::runtime_error("the typesupport for '" + full + "' returned no handle");
  }
  type->goal = message_type(full + "_Goal");
  type->result = message_type(full + "_Result");
  type->feedback = message_type(full + "_Feedback");
  type->send_goal_request = message_type(full + "_SendGoal_Request");
  type->send_goal_response = message_type(full + "_SendGoal_Response");
  type->get_result_request = message_type(full + "_GetResult_Request");
  type->get_result_response = message_type(full + "_GetResult_Response");
  type->feedback_message = message_type(full + "_FeedbackMessage");
  (*cache)[full] = type;
  return type;
}

// ---------------------------------------------------------------------------
// Public: messages
// ---------------------------------------------------------------------------

std::shared_ptr<Message> Message::create(MessageTypePtr type)
{
  std::shared_ptr<Message> m(new Message());
  m->type_ = std::move(type);
  const intro::MessageMembers * members = m->type_->members;
  m->data_ = ::operator new(members->size_of_);
  try {
    members->init_function(m->data_, rosidl_runtime_cpp::MessageInitialization::ALL);
  } catch (...) {
    ::operator delete(m->data_);
    m->data_ = nullptr;
    throw;
  }
  m->owned_ = true;
  return m;
}

std::shared_ptr<Message> Message::view(
  MessageTypePtr type, void * data, std::shared_ptr<const void> owner)
{
  std::shared_ptr<Message> m(new Message());
  m->type_ = std::move(type);
  m->data_ = data;
  m->owner_ = std::move(owner);
  return m;
}

Message::~Message()
{
  if (owned_ && data_) {
    type_->members->fini_function(data_);
    ::operator delete(data_);
  }
}

std::shared_ptr<Message> Message::clone() const
{
  auto copy = create(type_);
  copy_message(type_->members, copy->data_, data_);
  return copy;
}

double get_f64(const Message & m, const std::string & path)
{
  Scalar s = read_scalar(resolve(m.members(), m.data(), path, false), path);
  switch (s.kind) {
    case Scalar::FLOAT: return s.f;
    case Scalar::INT: return static_cast<double>(s.i);
    case Scalar::UINT: return static_cast<double>(s.u);
    case Scalar::BOOL: return s.b ? 1.0 : 0.0;
  }
  return 0.0;
}

int64_t get_i64(const Message & m, const std::string & path)
{
  Scalar s = read_scalar(resolve(m.members(), m.data(), path, false), path);
  switch (s.kind) {
    case Scalar::INT: return s.i;
    case Scalar::UINT:
      if (s.u > static_cast<uint64_t>(std::numeric_limits<int64_t>::max())) {
        throw path_error(path, "uint64 value " + std::to_string(s.u) + " does not fit in i64");
      }
      return static_cast<int64_t>(s.u);
    case Scalar::BOOL: return s.b ? 1 : 0;
    case Scalar::FLOAT:
      if (std::isfinite(s.f) && std::floor(s.f) == s.f &&
        std::fabs(s.f) < 9.2233720368547758e18)
      {
        return static_cast<int64_t>(s.f);
      }
      throw path_error(path, "is floating-point; read it with msg_f64");
  }
  return 0;
}

bool get_bool(const Message & m, const std::string & path)
{
  Scalar s = read_scalar(resolve(m.members(), m.data(), path, false), path);
  switch (s.kind) {
    case Scalar::BOOL: return s.b;
    case Scalar::INT: return s.i != 0;
    case Scalar::UINT: return s.u != 0;
    case Scalar::FLOAT: break;
  }
  throw path_error(path, "is floating-point, not bool");
}

std::string get_str(const Message & m, const std::string & path)
{
  return read_string(resolve(m.members(), m.data(), path, false), path);
}

std::string get_json(const Message & m, const std::string & path)
{
  std::string out;
  write_slot_json(out, resolve(m.members(), m.data(), path, false));
  return out;
}

void set_f64(Message & m, const std::string & path, double value)
{
  write_scalar(resolve(m.members(), m.data(), path, true), Scalar::of_float(value), path);
}

void set_i64(Message & m, const std::string & path, int64_t value)
{
  write_scalar(resolve(m.members(), m.data(), path, true), Scalar::of_int(value), path);
}

void set_bool(Message & m, const std::string & path, bool value)
{
  write_scalar(resolve(m.members(), m.data(), path, true), Scalar::of_bool(value), path);
}

void set_str(Message & m, const std::string & path, const std::string & value)
{
  write_string(resolve(m.members(), m.data(), path, true), value, path);
}

void set_json(Message & m, const std::string & path, const std::string & json)
{
  Json j;
  std::string error;
  if (!parse_json(json, j, error)) {
    throw std::runtime_error(
            "invalid JSON for " + (path.empty() ? m.type().name : "field '" + path + "'") + ": " +
            error);
  }
  read_slot_json(resolve(m.members(), m.data(), path, true), j, path);
}

int64_t length_of(const Message & m, const std::string & path)
{
  Slot slot = resolve(m.members(), m.data(), path, false);
  if (slot.kind == Slot::CONTAINER) {
    return static_cast<int64_t>(container_size(*slot.member, slot.ptr));
  }
  if ((slot.kind == Slot::FIELD || slot.kind == Slot::ELEMENT) &&
    is_string_type(slot.member->type_id_))
  {
    return static_cast<int64_t>(read_string(slot, path).size());
  }
  throw path_error(path, "is not an array or a string");
}

void resize(Message & m, const std::string & path, size_t size)
{
  Slot slot = resolve(m.members(), m.data(), path, false);
  if (slot.kind != Slot::CONTAINER) {
    throw path_error(path, "is not an array");
  }
  resize_container(*slot.member, slot.ptr, size, path);
}

bool has_path(const Message & m, const std::string & path)
{
  try {
    resolve(m.members(), m.data(), path, false);
    return true;
  } catch (const std::runtime_error &) {
    return false;
  }
}

namespace
{

Slot numeric_container(const Message & m, const std::string & path)
{
  Slot slot = resolve(m.members(), m.data(), path, false);
  if (slot.kind != Slot::CONTAINER || slot.member->type_id_ == ROS_TYPE_MESSAGE ||
    is_string_type(slot.member->type_id_))
  {
    throw path_error(path, "is not a numeric array");
  }
  return slot;
}

Slot element_of(const Slot & container, size_t i)
{
  const MessageMember * m = container.member;
  void * element = m->get_function ? m->get_function(container.ptr, i) : nullptr;
  return Slot{Slot::ELEMENT, nullptr, m, element, container.ptr, i};
}

}  // namespace

size_t get_f64s(const Message & m, const std::string & path, double * out, size_t max)
{
  Slot slot = numeric_container(m, path);
  size_t n = container_size(*slot.member, slot.ptr);
  for (size_t i = 0; i < n && i < max; ++i) {
    Scalar s = read_scalar(element_of(slot, i), path);
    out[i] = s.kind == Scalar::FLOAT ? s.f :
      s.kind == Scalar::INT ? static_cast<double>(s.i) :
      s.kind == Scalar::UINT ? static_cast<double>(s.u) :
      (s.b ? 1.0 : 0.0);
  }
  return n;
}

void set_f64s(Message & m, const std::string & path, const double * values, size_t count)
{
  Slot slot = numeric_container(m, path);
  resize_container(*slot.member, slot.ptr, count, path);
  for (size_t i = 0; i < count; ++i) {
    write_scalar(
      element_of(slot, i), Scalar::of_float(values[i]), path + "[" + std::to_string(i) + "]");
  }
}

namespace
{

void write_schema(std::string & out, const intro::MessageMembers * members)
{
  out += "{\"type\":";
  write_json_string(out, type_name_of(members));
  out += ",\"fields\":[";
  for (uint32_t i = 0; i < members->member_count_; ++i) {
    const MessageMember & m = members->members_[i];
    if (i) {
      out.push_back(',');
    }
    out += "{\"name\":";
    write_json_string(out, m.name_);
    out += ",\"type\":";
    write_json_string(
      out, m.type_id_ == ROS_TYPE_MESSAGE ? type_name_of(nested_members(m)) : type_word(m.type_id_));
    if (m.is_array_) {
      if (is_fixed_array(m)) {
        out += ",\"array\":\"fixed\",\"size\":" + std::to_string(m.array_size_);
      } else if (m.is_upper_bound_) {
        out += ",\"array\":\"bounded\",\"size\":" + std::to_string(m.array_size_);
      } else {
        out += ",\"array\":\"sequence\"";
      }
    }
    if (m.string_upper_bound_ > 0) {
      out += ",\"string_bound\":" + std::to_string(m.string_upper_bound_);
    }
    out.push_back('}');
  }
  out += "]}";
}

}  // namespace

std::string schema_json(const MessageType & type)
{
  std::string out;
  write_schema(out, type.members);
  return out;
}

void * field_pointer(
  const intro::MessageMembers * members, void * data, const std::string & path,
  const intro::MessageMembers ** nested_out)
{
  Slot slot = resolve(members, data, path, false);
  if (nested_out) {
    *nested_out = slot.kind == Slot::MESSAGE ? slot.members : nullptr;
  }
  return slot.ptr;
}

void copy_message(const intro::MessageMembers * members, void * dst, const void * src)
{
  for (uint32_t i = 0; i < members->member_count_; ++i) {
    const MessageMember & m = members->members_[i];
    void * d = static_cast<char *>(dst) + m.offset_;
    const void * s = static_cast<const char *>(src) + m.offset_;
    if (m.is_array_) {
      size_t n = container_size(m, s);
      if (!is_fixed_array(m)) {
        m.resize_function(d, n);
      }
      for (size_t k = 0; k < n; ++k) {
        copy_element(m, d, s, k);
      }
    } else if (m.type_id_ == ROS_TYPE_MESSAGE) {
      copy_message(nested_members(m), d, s);
    } else if (m.type_id_ == ROS_TYPE_STRING) {
      *static_cast<std::string *>(d) = *static_cast<const std::string *>(s);
    } else if (m.type_id_ == ROS_TYPE_WSTRING) {
      *static_cast<std::u16string *>(d) = *static_cast<const std::u16string *>(s);
    } else {
      std::memcpy(d, s, scalar_size(m.type_id_));
    }
  }
}

std::string message_to_json(const intro::MessageMembers * members, const void * data)
{
  std::string out;
  write_message_json(out, members, data);
  return out;
}

}  // namespace iris_ros2
