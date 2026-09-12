/* A tiny library with known-answer functions, so the FFI surface can be
 * asserted against values rather than "it did not crash". */
#include <string.h>
#include <stdint.h>
#if defined(_WIN32) || defined(__CYGWIN__)
#define FFI_EXPORT __declspec(dllexport)
#else
#define FFI_EXPORT __attribute__((visibility("default")))
#endif

FFI_EXPORT int64_t t_zero(void)                             { return 7; }
FFI_EXPORT int64_t t_one(int64_t a)                         { return a * 2; }
FFI_EXPORT int64_t t_two(int64_t a, int64_t b)              { return a + b; }
FFI_EXPORT int64_t t_three(int64_t a, int64_t b, int64_t c) { return a + b + c; }
FFI_EXPORT int64_t t_seven(
    int64_t a, int64_t b, int64_t c, int64_t d, int64_t e, int64_t f, int64_t g) {
    return a + 2*b + 3*c + 4*d + 5*e + 6*f + 7*g;
}
FFI_EXPORT int64_t t_eight(
    int64_t a, int64_t b, int64_t c, int64_t d,
    int64_t e, int64_t f, int64_t g, int64_t h) {
    return a + 2*b + 3*c + 4*d + 5*e + 6*f + 7*g + 8*h;
}
FFI_EXPORT double  t_f64_one(int64_t a)                     { return (double)a / 2.0; }
FFI_EXPORT double  t_f64_two(int64_t a, int64_t b)          { return (double)(a + b) / 4.0; }
FFI_EXPORT const char* t_str(void)                          { return "hello-ffi"; }
FFI_EXPORT int64_t t_fill(char* buf, int64_t max) {
    const char* src = "world";
    if (!buf || max < 6) return 0;
    strncpy(buf, src, (size_t)max - 1);
    buf[max - 1] = '\0';
    return 1;
}
