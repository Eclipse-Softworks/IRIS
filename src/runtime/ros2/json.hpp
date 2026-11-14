// Minimal JSON for the bridge: a DOM for parsing message JSON written by IRIS
// programs, and helpers for writing it. Numbers keep their integer-ness so a
// uint64 field round-trips exactly, and doubles are written in the shortest
// form that parses back to the same value.
#pragma once

#include <cstdint>
#include <string>
#include <utility>
#include <vector>

namespace iris_ros2
{

struct Json
{
  enum class Kind { Null, Bool, Int, UInt, Float, String, Array, Object };

  Kind kind = Kind::Null;
  bool boolean = false;
  int64_t int_value = 0;
  uint64_t uint_value = 0;
  double float_value = 0.0;
  std::string text;
  std::vector<Json> items;
  std::vector<std::pair<std::string, Json>> fields;

  bool is_number() const
  {
    return kind == Kind::Int || kind == Kind::UInt || kind == Kind::Float;
  }
  const Json * find(const std::string & key) const;
  const char * kind_name() const;
};

// Parses `text` as one JSON value. On failure returns false and describes the
// problem (with its byte offset) in `error`.
bool parse_json(const std::string & text, Json & out, std::string & error);

void write_json_string(std::string & out, const std::string & value);
// Non-finite values have no JSON form and are written as `null`.
void write_json_double(std::string & out, double value);
void write_json(std::string & out, const Json & value);

}  // namespace iris_ros2
