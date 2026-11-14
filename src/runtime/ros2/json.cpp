#include "json.hpp"

#include <charconv>
#include <cmath>
#include <cstring>
#include <limits>

namespace iris_ros2
{

const Json * Json::find(const std::string & key) const
{
  for (const auto & field : fields) {
    if (field.first == key) {
      return &field.second;
    }
  }
  return nullptr;
}

const char * Json::kind_name() const
{
  switch (kind) {
    case Kind::Null: return "null";
    case Kind::Bool: return "a boolean";
    case Kind::Int:
    case Kind::UInt:
    case Kind::Float: return "a number";
    case Kind::String: return "a string";
    case Kind::Array: return "an array";
    case Kind::Object: return "an object";
  }
  return "a value";
}

namespace
{

class Parser
{
public:
  explicit Parser(const std::string & text)
  : text_(text) {}

  bool parse(Json & out, std::string & error)
  {
    skip_space();
    if (!value(out, 0)) {
      error = error_ + " at byte " + std::to_string(pos_);
      return false;
    }
    skip_space();
    if (pos_ != text_.size()) {
      error = "unexpected trailing characters at byte " + std::to_string(pos_);
      return false;
    }
    return true;
  }

private:
  bool fail(const char * message)
  {
    if (error_.empty()) {
      error_ = message;
    }
    return false;
  }

  void skip_space()
  {
    while (pos_ < text_.size()) {
      char c = text_[pos_];
      if (c == ' ' || c == '\t' || c == '\n' || c == '\r') {
        ++pos_;
      } else {
        break;
      }
    }
  }

  bool literal(const char * word)
  {
    size_t n = std::strlen(word);
    if (text_.compare(pos_, n, word) == 0) {
      pos_ += n;
      return true;
    }
    return false;
  }

  bool value(Json & out, int depth)
  {
    if (depth > 128) {
      return fail("JSON nested too deeply");
    }
    if (pos_ >= text_.size()) {
      return fail("unexpected end of JSON");
    }
    char c = text_[pos_];
    if (c == '{') {
      return object(out, depth);
    }
    if (c == '[') {
      return array(out, depth);
    }
    if (c == '"') {
      out.kind = Json::Kind::String;
      return string(out.text);
    }
    if (literal("true")) {
      out.kind = Json::Kind::Bool;
      out.boolean = true;
      return true;
    }
    if (literal("false")) {
      out.kind = Json::Kind::Bool;
      out.boolean = false;
      return true;
    }
    if (literal("null")) {
      out.kind = Json::Kind::Null;
      return true;
    }
    // Non-standard but common spellings of the IEEE specials, so a message
    // holding NaN can be written back the way other ROS tools print it.
    if (literal("NaN") || literal("nan")) {
      out.kind = Json::Kind::Float;
      out.float_value = std::numeric_limits<double>::quiet_NaN();
      return true;
    }
    if (literal("Infinity") || literal("inf")) {
      out.kind = Json::Kind::Float;
      out.float_value = std::numeric_limits<double>::infinity();
      return true;
    }
    if (literal("-Infinity") || literal("-inf")) {
      out.kind = Json::Kind::Float;
      out.float_value = -std::numeric_limits<double>::infinity();
      return true;
    }
    if (c == '-' || (c >= '0' && c <= '9')) {
      return number(out);
    }
    return fail("unexpected character in JSON");
  }

  bool number(Json & out)
  {
    size_t start = pos_;
    bool is_float = false;
    if (text_[pos_] == '-') {
      ++pos_;
    }
    while (pos_ < text_.size()) {
      char c = text_[pos_];
      if (c >= '0' && c <= '9') {
        ++pos_;
      } else if (c == '.' || c == 'e' || c == 'E' || c == '+' || c == '-') {
        is_float = true;
        ++pos_;
      } else {
        break;
      }
    }
    const char * first = text_.data() + start;
    const char * last = text_.data() + pos_;
    if (!is_float) {
      if (*first == '-') {
        int64_t v = 0;
        auto r = std::from_chars(first, last, v);
        if (r.ec == std::errc() && r.ptr == last) {
          out.kind = Json::Kind::Int;
          out.int_value = v;
          return true;
        }
      } else {
        uint64_t v = 0;
        auto r = std::from_chars(first, last, v);
        if (r.ec == std::errc() && r.ptr == last) {
          if (v <= static_cast<uint64_t>(std::numeric_limits<int64_t>::max())) {
            out.kind = Json::Kind::Int;
            out.int_value = static_cast<int64_t>(v);
          } else {
            out.kind = Json::Kind::UInt;
            out.uint_value = v;
          }
          return true;
        }
      }
      // Too large for 64 bits: fall through and keep it as a double.
    }
    double d = 0.0;
    auto r = std::from_chars(first, last, d);
    if (r.ec != std::errc() || r.ptr != last) {
      pos_ = start;
      return fail("malformed number");
    }
    out.kind = Json::Kind::Float;
    out.float_value = d;
    return true;
  }

  static void append_utf8(std::string & out, uint32_t cp)
  {
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

  bool hex4(uint32_t & out)
  {
    if (pos_ + 4 > text_.size()) {
      return fail("truncated \\u escape");
    }
    out = 0;
    for (int i = 0; i < 4; ++i) {
      char c = text_[pos_++];
      out <<= 4;
      if (c >= '0' && c <= '9') {
        out |= static_cast<uint32_t>(c - '0');
      } else if (c >= 'a' && c <= 'f') {
        out |= static_cast<uint32_t>(c - 'a' + 10);
      } else if (c >= 'A' && c <= 'F') {
        out |= static_cast<uint32_t>(c - 'A' + 10);
      } else {
        return fail("invalid \\u escape");
      }
    }
    return true;
  }

  bool string(std::string & out)
  {
    ++pos_;  // opening quote
    out.clear();
    while (pos_ < text_.size()) {
      char c = text_[pos_++];
      if (c == '"') {
        return true;
      }
      if (c != '\\') {
        out.push_back(c);
        continue;
      }
      if (pos_ >= text_.size()) {
        break;
      }
      char e = text_[pos_++];
      switch (e) {
        case '"': out.push_back('"'); break;
        case '\\': out.push_back('\\'); break;
        case '/': out.push_back('/'); break;
        case 'b': out.push_back('\b'); break;
        case 'f': out.push_back('\f'); break;
        case 'n': out.push_back('\n'); break;
        case 'r': out.push_back('\r'); break;
        case 't': out.push_back('\t'); break;
        case 'u': {
            uint32_t cp = 0;
            if (!hex4(cp)) {
              return false;
            }
            if (cp >= 0xD800 && cp <= 0xDBFF && pos_ + 6 <= text_.size() &&
              text_[pos_] == '\\' && text_[pos_ + 1] == 'u')
            {
              pos_ += 2;
              uint32_t low = 0;
              if (!hex4(low)) {
                return false;
              }
              cp = 0x10000 + ((cp - 0xD800) << 10) + (low - 0xDC00);
            }
            append_utf8(out, cp);
            break;
          }
        default:
          return fail("invalid escape in string");
      }
    }
    return fail("unterminated string");
  }

  bool array(Json & out, int depth)
  {
    ++pos_;
    out.kind = Json::Kind::Array;
    skip_space();
    if (pos_ < text_.size() && text_[pos_] == ']') {
      ++pos_;
      return true;
    }
    while (true) {
      skip_space();
      out.items.emplace_back();
      if (!value(out.items.back(), depth + 1)) {
        return false;
      }
      skip_space();
      if (pos_ >= text_.size()) {
        return fail("unterminated array");
      }
      char c = text_[pos_++];
      if (c == ']') {
        return true;
      }
      if (c != ',') {
        return fail("expected ',' or ']' in array");
      }
    }
  }

  bool object(Json & out, int depth)
  {
    ++pos_;
    out.kind = Json::Kind::Object;
    skip_space();
    if (pos_ < text_.size() && text_[pos_] == '}') {
      ++pos_;
      return true;
    }
    while (true) {
      skip_space();
      if (pos_ >= text_.size() || text_[pos_] != '"') {
        return fail("expected a field name");
      }
      std::string key;
      if (!string(key)) {
        return false;
      }
      skip_space();
      if (pos_ >= text_.size() || text_[pos_] != ':') {
        return fail("expected ':' after a field name");
      }
      ++pos_;
      skip_space();
      out.fields.emplace_back(std::move(key), Json());
      if (!value(out.fields.back().second, depth + 1)) {
        return false;
      }
      skip_space();
      if (pos_ >= text_.size()) {
        return fail("unterminated object");
      }
      char c = text_[pos_++];
      if (c == '}') {
        return true;
      }
      if (c != ',') {
        return fail("expected ',' or '}' in object");
      }
    }
  }

  const std::string & text_;
  size_t pos_ = 0;
  std::string error_;
};

}  // namespace

bool parse_json(const std::string & text, Json & out, std::string & error)
{
  out = Json();
  Parser parser(text);
  return parser.parse(out, error);
}

void write_json_string(std::string & out, const std::string & value)
{
  static const char * hex = "0123456789abcdef";
  out.push_back('"');
  for (unsigned char c : value) {
    switch (c) {
      case '"': out += "\\\""; break;
      case '\\': out += "\\\\"; break;
      case '\n': out += "\\n"; break;
      case '\r': out += "\\r"; break;
      case '\t': out += "\\t"; break;
      case '\b': out += "\\b"; break;
      case '\f': out += "\\f"; break;
      default:
        if (c < 0x20) {
          out += "\\u00";
          out.push_back(hex[c >> 4]);
          out.push_back(hex[c & 0xF]);
        } else {
          out.push_back(static_cast<char>(c));
        }
    }
  }
  out.push_back('"');
}

void write_json_double(std::string & out, double value)
{
  if (!std::isfinite(value)) {
    out += "null";
    return;
  }
  char buf[64];
  auto r = std::to_chars(buf, buf + sizeof(buf), value);
  std::string text(buf, r.ptr);
  // Keep floats recognisably floating-point (`1.0`, not `1`) so a consumer
  // that types by syntax does not read a float64 field back as an integer.
  if (text.find_first_of(".eEn") == std::string::npos) {
    text += ".0";
  }
  out += text;
}

void write_json(std::string & out, const Json & value)
{
  switch (value.kind) {
    case Json::Kind::Null: out += "null"; break;
    case Json::Kind::Bool: out += value.boolean ? "true" : "false"; break;
    case Json::Kind::Int: out += std::to_string(value.int_value); break;
    case Json::Kind::UInt: out += std::to_string(value.uint_value); break;
    case Json::Kind::Float: write_json_double(out, value.float_value); break;
    case Json::Kind::String: write_json_string(out, value.text); break;
    case Json::Kind::Array: {
        out.push_back('[');
        for (size_t i = 0; i < value.items.size(); ++i) {
          if (i) {
            out.push_back(',');
          }
          write_json(out, value.items[i]);
        }
        out.push_back(']');
        break;
      }
    case Json::Kind::Object: {
        out.push_back('{');
        for (size_t i = 0; i < value.fields.size(); ++i) {
          if (i) {
            out.push_back(',');
          }
          write_json_string(out, value.fields[i].first);
          out.push_back(':');
          write_json(out, value.fields[i].second);
        }
        out.push_back('}');
        break;
      }
  }
}

}  // namespace iris_ros2
