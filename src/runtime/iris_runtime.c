// iris_runtime.c — IRIS Language Runtime Library
//
// Implements all iris_* functions declared in iris_runtime.h.
// Memory model: two tiers.
//   1. Heap values (strings, lists, maps, options, results, channels, ...) are
//      reference-counted; the compiler emits retains at creation and releases
//      at each value's last use (see src/pass/gc_annotate.rs), so transients
//      are freed promptly instead of leaking.
//   2. `ui_frame_pool_*` bulk-free everything allocated while a per-frame pool
//      is active — the sanctioned way for GUI code to reclaim records and
//      other untracked temporaries every frame. Values that must survive a
//      pool are cloned onto the main heap (detached) while the pool is paused.
// Records/tuples are not individually reference-counted; they are reclaimed by
// frame pools or live until process exit.
// Concurrency: real pthreads for spawn, par_for, channels, atomics.

#include "iris_runtime.h"

#define _POSIX_C_SOURCE 200809L
#define _GNU_SOURCE
#define _DARWIN_C_SOURCE

#include <assert.h>
#include <ctype.h>
#include <errno.h>
#include <inttypes.h>
#include <math.h>
#include <stdarg.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>

#include <sys/types.h>

#if defined(_MSC_VER)
#define IRIS_THREAD_LOCAL __declspec(thread)
#elif defined(__STDC_VERSION__) && __STDC_VERSION__ >= 201112L
#define IRIS_THREAD_LOCAL _Thread_local
#else
#define IRIS_THREAD_LOCAL __thread
#endif

/* WASM/WASI: wasi-libc provides standard C + POSIX headers below.
   Socket/terminal headers exist in wasi-libc but the function
   declarations are absent in WASI preview 1 (no networking/termios).
   WASI preview 2 (__wasip2__) has wasi-sockets for networking. */
/* Stub flag: 1 on WASM P1 (no networking), 0 on native/P2 */
#if defined(__wasm__)
#if !defined(__wasip2__)
#define __IRIS_WASM_STUB 1
#endif
#include <dirent.h>
#include <dlfcn.h>
#include <poll.h>
#include <sys/stat.h>
#include <unistd.h>
#ifdef __wasip2__
#include <arpa/inet.h>
#include <netdb.h>
#include <netinet/in.h>
#include <sys/socket.h>
#endif
#elif defined(_WIN32)
#include <conio.h> /* _getch() for read_key */
#include <direct.h>
#include <io.h>
#include <windows.h>
#include <winhttp.h>
#include <winsock2.h>
#include <ws2tcpip.h>
#pragma comment(lib, "ws2_32.lib")
#pragma comment(lib, "winhttp.lib")
#else
#include <arpa/inet.h>
#include <dirent.h>
#include <dlfcn.h>
#include <fcntl.h> /* open: the runtime-error report file */
#include <netdb.h>
#include <netinet/in.h>
#include <poll.h>         /* socket readiness for async_readable */
#include <signal.h>       /* sigaltstack: report stack overflow */
#include <sys/ioctl.h>    /* TIOCGWINSZ for term_rows/cols */
#include <sys/resource.h> /* RLIMIT_STACK: tell overflow from a bad access */
#include <sys/socket.h>
#include <sys/stat.h>
#include <sys/time.h>
#include <termios.h> /* tcgetattr/tcsetattr for read_key */
#include <unistd.h>
#endif

// ---------------------------------------------------------------------------
// Internal memory helpers
// ---------------------------------------------------------------------------

// Internal sandbox state/tracking helpers (implemented in the sandbox section)
static void iris_sandbox_require_url_network(const char *url);
static void iris_sandbox_require_network_enabled(void);
static void iris_sandbox_require_ffi_enabled(void);
static void iris_sandbox_require_open_slot(void);
static void iris_sandbox_track_file_open(FILE *f, const char *path,
                                         int is_write);
static void iris_sandbox_track_file_close(FILE *f);
static void iris_sandbox_require_file_handle_write(FILE *f, int64_t bytes);
static void iris_sandbox_require_file_handle_read(FILE *f);
static void iris_sandbox_require_conn_open(const char *host);
static void iris_sandbox_track_conn_open(int64_t fd);
static void iris_sandbox_track_conn_close(int64_t fd);
static void iris_sandbox_verify_accepted_peer(int64_t fd);

// Dynamic allocations registry, to tell heap strings from static string
// constants when freeing. A hash set (open addressing, linear probing): it
// used to be a flat array searched linearly on every string free and every
// realloc, which made programs quadratic in their allocation count -- a loop
// building 200k strings spent 25 s in exit cleanup alone.
static pthread_mutex_t ds_mu = PTHREAD_MUTEX_INITIALIZER;
#define DS_TOMBSTONE ((void *)(uintptr_t)1)
static void **ds_slots = NULL;
static size_t *ds_sizes = NULL; /* allocation size for each live pointer */
static size_t ds_cap = 0;  /* a power of two */
static size_t ds_used = 0; /* live entries plus tombstones */
static size_t ds_live = 0;

/* Raw allocation diagnostics. These count every xmalloc-family allocation,
 * including buffers that never enter the RC table (record blocks, list data,
 * map buckets, JIT state, ...). */
static int64_t g_raw_allocs = 0;
static int64_t g_raw_frees = 0;
static int64_t g_raw_bytes_live = 0;
static int64_t g_raw_bytes_peak = 0;

/* Allocation-site counters. Codegen sets the site before each instrumented
 * allocation; zero means an instrumented or runtime-internal allocation. */
#define ALLOC_SITE_SLOTS 64
static IRIS_THREAD_LOCAL int64_t iris_alloc_site = 0;
static pthread_mutex_t alloc_sites_mu = PTHREAD_MUTEX_INITIALIZER;
static struct {
  int64_t site;
  int64_t count;
  int64_t bytes;
} alloc_sites[ALLOC_SITE_SLOTS];

static void alloc_site_record(size_t bytes) {
  if (iris_alloc_site == 0)
    return;
  pthread_mutex_lock(&alloc_sites_mu);
  int slot = -1;
  for (int i = 0; i < ALLOC_SITE_SLOTS; i++) {
    if (alloc_sites[i].site == iris_alloc_site) {
      slot = i;
      break;
    }
    if (slot < 0 && alloc_sites[i].site == 0)
      slot = i;
  }
  if (slot >= 0) {
    alloc_sites[slot].site = iris_alloc_site;
    alloc_sites[slot].count++;
    alloc_sites[slot].bytes += (int64_t)bytes;
  }
  pthread_mutex_unlock(&alloc_sites_mu);
}

void iris_set_alloc_site(int64_t site) { iris_alloc_site = site; }

static size_t ds_hash(void *ptr, size_t cap) {
  uint64_t v = (uint64_t)(uintptr_t)ptr >> 4;
  v *= 0x9E3779B97F4A7C15ULL;
  return (size_t)(v >> 32) & (cap - 1);
}

/* Must be called with ds_mu held. */
static void ds_rehash_locked(size_t cap) {
  void **slots = (void **)calloc(cap, sizeof(void *));
  size_t *sizes = (size_t *)calloc(cap, sizeof(size_t));
  if (!slots || !sizes) {
    free(slots);
    free(sizes);
    return; /* keep probing the old table */
  }
  for (size_t i = 0; i < ds_cap; i++) {
    void *p = ds_slots[i];
    if (!p || p == DS_TOMBSTONE)
      continue;
    size_t h = ds_hash(p, cap);
    while (slots[h])
      h = (h + 1) & (cap - 1);
    slots[h] = p;
    sizes[h] = ds_sizes ? ds_sizes[i] : 0;
  }
  free(ds_slots);
  free(ds_sizes);
  ds_slots = slots;
  ds_sizes = sizes;
  ds_cap = cap;
  ds_used = ds_live;
}

/* Must be called with ds_mu held. */
static void ds_add_locked(void *ptr, size_t bytes) {
  if (ds_cap == 0 || (ds_used + 1) * 2 > ds_cap) {
    /* Grow when live entries are dense; otherwise just drop tombstones. */
    size_t cap =
        ds_cap == 0 ? 1024 : ((ds_live + 1) * 4 > ds_cap ? ds_cap * 2 : ds_cap);
    ds_rehash_locked(cap);
    if (!ds_slots)
      return;
  }
  size_t h = ds_hash(ptr, ds_cap);
  size_t reuse = (size_t)-1;
  while (ds_slots[h]) {
    if (ds_slots[h] == ptr)
      return; /* already recorded */
    if (ds_slots[h] == DS_TOMBSTONE && reuse == (size_t)-1)
      reuse = h;
    h = (h + 1) & (ds_cap - 1);
  }
  if (reuse != (size_t)-1) {
    ds_slots[reuse] = ptr;
    ds_sizes[reuse] = bytes;
  } else {
    ds_slots[h] = ptr;
    ds_sizes[h] = bytes;
    ds_used++;
  }
  ds_live++;
}

/* Must be called with ds_mu held. */
static int ds_remove_locked(void *ptr, size_t *out_bytes) {
  if (ds_cap == 0)
    return 0;
  size_t h = ds_hash(ptr, ds_cap);
  while (ds_slots[h]) {
    if (ds_slots[h] == ptr) {
      if (out_bytes)
        *out_bytes = ds_sizes ? ds_sizes[h] : 0;
      ds_slots[h] = DS_TOMBSTONE;
      ds_sizes[h] = 0;
      ds_live--;
      return 1;
    }
    h = (h + 1) & (ds_cap - 1);
  }
  return 0;
}

static void ds_add(void *ptr, size_t bytes) {
  if (!ptr)
    return;
  pthread_mutex_lock(&ds_mu);
  ds_add_locked(ptr, bytes);
  g_raw_allocs++;
  g_raw_bytes_live += (int64_t)bytes;
  if (g_raw_bytes_live > g_raw_bytes_peak)
    g_raw_bytes_peak = g_raw_bytes_live;
  pthread_mutex_unlock(&ds_mu);
  alloc_site_record(bytes);
}

static int ds_contains_and_remove(void *ptr, size_t *out_bytes) {
  if (!ptr)
    return 0;
  pthread_mutex_lock(&ds_mu);
  size_t bytes = 0;
  int found = ds_remove_locked(ptr, &bytes);
  if (found) {
    g_raw_frees++;
    g_raw_bytes_live -= (int64_t)bytes;
    if (out_bytes)
      *out_bytes = bytes;
  }
  pthread_mutex_unlock(&ds_mu);
  return found;
}

/* Whether `ptr` was allocated on its own (by xmalloc). A value stored by value
 * inside a list's chunk arena was not, and must never be freed on its own or
 * shared with another list. */
static int ds_contains(void *ptr) {
  if (!ptr)
    return 0;
  pthread_mutex_lock(&ds_mu);
  int found = 0;
  if (ds_cap != 0) {
    size_t h = ds_hash(ptr, ds_cap);
    while (ds_slots[h]) {
      if (ds_slots[h] == ptr) {
        found = 1;
        break;
      }
      h = (h + 1) & (ds_cap - 1);
    }
  }
  pthread_mutex_unlock(&ds_mu);
  return found;
}
/* A program error (failed assert, panic, out-of-bounds index, overflow):
 * flush what the program printed and exit with 101. abort() reported every
 * such error as a crash -- 0xC0000409 on Windows, SIGABRT elsewhere --
 * and lost buffered output. _Exit skips atexit handlers, which must not run
 * over half-updated state. IRIS_PANIC_ABORT=1 restores abort() so a
 * debugger stops at the failure. */
static void iris_fatal(void) {
  fflush(stdout);
  fflush(stderr);
#if defined(__wasm__)
  abort();
#else
  const char *want_abort = getenv("IRIS_PANIC_ABORT");
  if (want_abort && want_abort[0] && want_abort[0] != '0')
    abort();
  _Exit(101);
#endif
}

/* Where the program is. Generated code stores its file-name table on entry
 * to each function that can fail, and the position -- file index << 44 |
 * line << 20 | column -- before each operation that can fail and each call,
 * so a runtime error names the line that failed: or, for a failure inside a
 * standard-library function, the line that called it.
 *
 * The position is per thread: each function asks for its thread's slot on
 * entry (`iris_loc_slot`) and writes through it, so a `par for` worker that
 * fails reports its own line, not the one another worker wrote last. An
 * error is reported on the thread that failed, which reads its own. */
const char *const *iris_loc_files = NULL;
static IRIS_THREAD_LOCAL int64_t iris_loc = 0;

int64_t *iris_loc_slot(void) { return &iris_loc; }

/* Where to write a runtime error instead of stderr: `iris run` sets
 * IRIS_ERROR_FILE, and renders the error with the source line once the
 * program has exited. Read once at start-up, so the fault handlers need not
 * call getenv. */
static const char *iris_error_file = NULL;

/* String building without stdio or the heap, so the stack-overflow and
 * fault handlers can use it too. */
static size_t iris_put(char *buf, size_t at, size_t cap, const char *s) {
  while (s && *s && at + 1 < cap)
    buf[at++] = *s++;
  buf[at] = '\0';
  return at;
}

static size_t iris_put_u64(char *buf, size_t at, size_t cap, uint64_t v) {
  char digits[24];
  int n = 0;
  do {
    digits[n++] = (char)('0' + v % 10);
    v /= 10;
  } while (v && n < 24);
  while (n > 0 && at + 1 < cap)
    buf[at++] = digits[--n];
  buf[at] = '\0';
  return at;
}

/* The report the driver parses back (parse_runtime_report in
 * src/codegen/build.rs), in the interpreter's format:
 *   error[E0403]: index 5 is out of bounds for length 3
 *    --> main.iris:4:5                                                  */
static size_t iris_format_report(char *buf, size_t cap, const char *code,
                                 const char *msg, int with_loc) {
  size_t n = 0;
  n = iris_put(buf, n, cap, "error[");
  n = iris_put(buf, n, cap, code);
  n = iris_put(buf, n, cap, "]: ");
  n = iris_put(buf, n, cap, msg);
  n = iris_put(buf, n, cap, "\n");
  if (with_loc && iris_loc_files && iris_loc > 0) {
    n = iris_put(buf, n, cap, " --> ");
    n = iris_put(buf, n, cap, iris_loc_files[iris_loc >> 44]);
    n = iris_put(buf, n, cap, ":");
    n = iris_put_u64(buf, n, cap, (uint64_t)((iris_loc >> 20) & 0xFFFFFF));
    n = iris_put(buf, n, cap, ":");
    n = iris_put_u64(buf, n, cap, (uint64_t)(iris_loc & 0xFFFFF));
    n = iris_put(buf, n, cap, "\n");
  }
  return n;
}

/* To the report file if the driver asked for one, else to stderr. */
static void iris_emit_report(const char *buf, size_t n) {
#if defined(_WIN32)
  DWORD written = 0;
  if (iris_error_file) {
    HANDLE h = CreateFileA(iris_error_file, GENERIC_WRITE, 0, NULL,
                           CREATE_ALWAYS, FILE_ATTRIBUTE_NORMAL, NULL);
    if (h != INVALID_HANDLE_VALUE) {
      WriteFile(h, buf, (DWORD)n, &written, NULL);
      CloseHandle(h);
      return;
    }
  }
  WriteFile(GetStdHandle(STD_ERROR_HANDLE), buf, (DWORD)n, &written, NULL);
#elif defined(__wasm__)
  (void)iris_error_file;
  fwrite(buf, 1, n, stderr);
#else
  if (iris_error_file) {
    int fd = open(iris_error_file, O_WRONLY | O_CREAT | O_TRUNC, 0644);
    if (fd >= 0) {
      ssize_t ignored = write(fd, buf, n);
      (void)ignored;
      close(fd);
      return;
    }
  }
  ssize_t ignored = write(2, buf, n);
  (void)ignored;
#endif
}

/* Run before a runtime error stops the program (not from the fault
 * handlers): a `--pgo-generate` build writes its profile here, so a run that
 * fails still counts. */
static void (*iris_error_exit_hook)(void) = NULL;

/* The trap this thread's runtime errors return to, if code embedding IRIS
 * pushed one. */
static IRIS_THREAD_LOCAL IrisErrorTrap *iris_current_trap = NULL;

void iris_error_trap_push(IrisErrorTrap *trap) {
  trap->prev = iris_current_trap;
  trap->message[0] = '\0';
  iris_current_trap = trap;
}

void iris_error_trap_pop(IrisErrorTrap *trap) {
  if (iris_current_trap == trap)
    iris_current_trap = trap->prev;
}

static void iris_report_and_exit(const char *code, const char *msg,
                                 int with_loc) {
  char buf[4096];
#if IRIS_ERROR_TRAPS
  if (iris_current_trap) {
    IrisErrorTrap *trap = iris_current_trap;
    iris_current_trap = trap->prev;
    size_t n = iris_format_report(trap->message, sizeof trap->message, code,
                                  msg, with_loc);
    while (n > 0 && trap->message[n - 1] == '\n')
      trap->message[--n] = '\0';
    longjmp(trap->jump, 1);
  }
#endif
  fflush(stdout);
  fflush(stderr);
  iris_emit_report(buf,
                   iris_format_report(buf, sizeof buf, code, msg, with_loc));
  if (iris_error_exit_hook) {
    void (*hook)(void) = iris_error_exit_hook;
    iris_error_exit_hook = NULL;
    hook();
  }
  iris_fatal();
}

/* ── Profile-guided optimisation: block counters ──────────────────────────
 * A `--pgo-generate` build counts each block's executions, one counter array
 * per function, and registers the arrays at start-up. At exit they are added
 * to the profile file -- `IRIS_PROFILE_FILE`, else `iris.profile` -- whose
 * lines are `function:block:count`, so several runs accumulate.
 * `--pgo-use` reads it back. */
typedef struct {
  const char *fn;
  const char *const *labels;
  int64_t *counts;
  int64_t n;
} IrisProfFn;

static IrisProfFn *iris_prof_fns = NULL;
static int64_t iris_prof_len = 0, iris_prof_cap = 0;

typedef struct {
  char *key; /* "function:block" */
  uint64_t count;
} IrisProfLine;

static void iris_profile_write(void) {
  if (iris_prof_len == 0)
    return;
  const char *path = getenv("IRIS_PROFILE_FILE");
  if (!path || !*path)
    path = "iris.profile";

  IrisProfLine *lines = NULL;
  int64_t nlines = 0, cap = 0;
  FILE *in = fopen(path, "rb");
  if (in) {
    char row[4096];
    while (fgets(row, sizeof row, in)) {
      size_t len = strlen(row);
      while (len && (row[len - 1] == '\n' || row[len - 1] == '\r'))
        row[--len] = '\0';
      char *colon = strrchr(row, ':');
      if (!colon || colon == row || row[0] == '#')
        continue;
      *colon = '\0';
      if (nlines == cap) {
        cap = cap ? cap * 2 : 256;
        lines =
            (IrisProfLine *)realloc(lines, sizeof(IrisProfLine) * (size_t)cap);
        if (!lines) {
          fclose(in);
          return;
        }
      }
      lines[nlines].key = strdup(row);
      lines[nlines].count = strtoull(colon + 1, NULL, 10);
      nlines++;
    }
    fclose(in);
  }
  for (int64_t f = 0; f < iris_prof_len; f++) {
    IrisProfFn *fn = &iris_prof_fns[f];
    for (int64_t b = 0; b < fn->n; b++) {
      char key[1024];
      snprintf(key, sizeof key, "%s:%s", fn->fn, fn->labels[b]);
      uint64_t c = (uint64_t)fn->counts[b];
      int64_t at = -1;
      for (int64_t i = 0; i < nlines; i++) {
        if (strcmp(lines[i].key, key) == 0) {
          at = i;
          break;
        }
      }
      if (at >= 0) {
        lines[at].count += c;
        continue;
      }
      if (nlines == cap) {
        cap = cap ? cap * 2 : 256;
        lines =
            (IrisProfLine *)realloc(lines, sizeof(IrisProfLine) * (size_t)cap);
        if (!lines)
          return;
      }
      lines[nlines].key = strdup(key);
      lines[nlines].count = c;
      nlines++;
    }
  }
  /* Write a sibling, then replace the file, so a reader never sees half. */
  char tmp[4096];
  snprintf(tmp, sizeof tmp, "%s.tmp", path);
  FILE *out = fopen(tmp, "wb");
  if (out) {
    fprintf(out, "# IRIS profile: function:block:count\n");
    for (int64_t i = 0; i < nlines; i++)
      fprintf(out, "%s:%llu\n", lines[i].key,
              (unsigned long long)lines[i].count);
    fclose(out);
    remove(path);
    if (rename(tmp, path) != 0)
      remove(tmp);
  }
  for (int64_t i = 0; i < nlines; i++)
    free(lines[i].key);
  free(lines);
  /* Written once: counts already added must not be added again. */
  iris_prof_len = 0;
}

void iris_profile_register(const char *fn, const char *const *labels,
                           int64_t *counts, int64_t n) {
  if (iris_prof_len == iris_prof_cap) {
    iris_prof_cap = iris_prof_cap ? iris_prof_cap * 2 : 64;
    iris_prof_fns = (IrisProfFn *)realloc(
        iris_prof_fns, sizeof(IrisProfFn) * (size_t)iris_prof_cap);
    if (!iris_prof_fns)
      return;
  }
  if (iris_prof_len == 0) {
    atexit(iris_profile_write);
    iris_error_exit_hook = iris_profile_write;
  }
  iris_prof_fns[iris_prof_len].fn = fn;
  iris_prof_fns[iris_prof_len].labels = labels;
  iris_prof_fns[iris_prof_len].counts = counts;
  iris_prof_fns[iris_prof_len].n = n;
  iris_prof_len++;
}

/* Report a runtime error the way the interpreter does, then stop with exit
 * code 101. */
static void iris_runtime_error(const char *code, const char *fmt, ...) {
  char msg[2048];
  va_list ap;
  va_start(ap, fmt);
  vsnprintf(msg, sizeof msg, fmt, ap);
  va_end(ap);
  iris_report_and_exit(code, msg, 1);
}

/* Called by generated code: checked i64 arithmetic overflowed. */
void iris_overflow_abort(int32_t op, int64_t a, int64_t b) {
  static const char *names[] = {"addition", "subtraction", "multiplication",
                                "division", "remainder"};
  static const char *syms[] = {"+", "-", "*", "/", "%"};
  int32_t i = (op >= 0 && op < 5) ? op : 0;
  iris_runtime_error("E0409", "integer overflow in %s (%lld %s %lld)", names[i],
                     (long long)a, syms[i], (long long)b);
}

/* Called by generated code: an integer division or remainder by zero. */
void iris_div_zero_abort(void) {
  iris_runtime_error("E0402", "division by zero");
}

/* Scoped frame allocation pool for UI tree/layout/paint lifecycles.
 * Tracks xmalloc/xcalloc/xrealloc objects plus external retains so releasing a
 * dead UI frame frees structs (MakeStruct) and retained buffers cleanly. */
typedef struct {
  void *ptr;
  int32_t kind;
  int64_t delta;
} IrisUiPoolExtSlot;

typedef struct IrisUiPool {
  pthread_mutex_t mu;
  void **ptrs;
  size_t *ptr_bytes;
  size_t ptr_cap;
  size_t ptr_live;
  size_t ptr_used;
  int64_t bytes_live;
  int64_t bytes_peak;
  IrisUiPoolExtSlot *ext;
  size_t ext_cap;
  size_t ext_live;
  size_t ext_used;
  struct IrisUiPool *prev_tls;
  struct IrisUiPool *next_reg;
} IrisUiPool;

static pthread_mutex_t iris_ui_pool_reg_mu = PTHREAD_MUTEX_INITIALIZER;
static IrisUiPool *iris_ui_pool_reg_head = NULL;
static volatile int iris_ui_pool_any = 0;
static IRIS_THREAD_LOCAL IrisUiPool *iris_active_ui_pool = NULL;

static void iris_ui_pool_ptr_rehash_locked(IrisUiPool *p, size_t cap) {
  void **slots = (void **)calloc(cap, sizeof(void *));
  size_t *sizes = (size_t *)calloc(cap, sizeof(size_t));
  if (!slots || !sizes) {
    free(slots);
    free(sizes);
    return;
  }
  for (size_t i = 0; i < p->ptr_cap; i++) {
    void *v = p->ptrs[i];
    if (!v || v == DS_TOMBSTONE)
      continue;
    size_t h = ds_hash(v, cap);
    while (slots[h])
      h = (h + 1) & (cap - 1);
    slots[h] = v;
    sizes[h] = p->ptr_bytes ? p->ptr_bytes[i] : 0;
  }
  free(p->ptrs);
  free(p->ptr_bytes);
  p->ptrs = slots;
  p->ptr_bytes = sizes;
  p->ptr_cap = cap;
  p->ptr_used = p->ptr_live;
}

static void iris_ui_pool_track_alloc(IrisUiPool *p, void *ptr, size_t bytes) {
  if (!p || !ptr)
    return;
  pthread_mutex_lock(&p->mu);
  if (p->ptr_cap == 0 || (p->ptr_used + 1) * 2 > p->ptr_cap) {
    size_t cap = p->ptr_cap == 0
                     ? 512
                     : ((p->ptr_live + 1) * 4 > p->ptr_cap ? p->ptr_cap * 2
                                                           : p->ptr_cap);
    iris_ui_pool_ptr_rehash_locked(p, cap);
    if (!p->ptrs) {
      pthread_mutex_unlock(&p->mu);
      return;
    }
  }
  size_t h = ds_hash(ptr, p->ptr_cap);
  size_t reuse = (size_t)-1;
  while (p->ptrs[h]) {
    if (p->ptrs[h] == ptr) {
      pthread_mutex_unlock(&p->mu);
      return;
    }
    if (p->ptrs[h] == DS_TOMBSTONE && reuse == (size_t)-1)
      reuse = h;
    h = (h + 1) & (p->ptr_cap - 1);
  }
  if (reuse != (size_t)-1) {
    p->ptrs[reuse] = ptr;
    p->ptr_bytes[reuse] = bytes;
  } else {
    p->ptrs[h] = ptr;
    p->ptr_bytes[h] = bytes;
    p->ptr_used++;
  }
  p->ptr_live++;
  p->bytes_live += (int64_t)bytes;
  if (p->bytes_live > p->bytes_peak)
    p->bytes_peak = p->bytes_live;
  pthread_mutex_unlock(&p->mu);
}

static int iris_ui_pool_owns(IrisUiPool *p, void *ptr) {
  if (!p || !ptr || p->ptr_cap == 0)
    return 0;
  pthread_mutex_lock(&p->mu);
  int found = 0;
  if (p->ptr_cap != 0) {
    size_t h = ds_hash(ptr, p->ptr_cap);
    while (p->ptrs[h]) {
      if (p->ptrs[h] == ptr) {
        found = 1;
        break;
      }
      h = (h + 1) & (p->ptr_cap - 1);
    }
  }
  pthread_mutex_unlock(&p->mu);
  return found;
}

static int iris_ui_pool_forget_one(IrisUiPool *p, void *ptr) {
  if (!p || !ptr || p->ptr_cap == 0)
    return 0;
  pthread_mutex_lock(&p->mu);
  int removed = 0;
  if (p->ptr_cap != 0) {
    size_t h = ds_hash(ptr, p->ptr_cap);
    while (p->ptrs[h]) {
      if (p->ptrs[h] == ptr) {
        p->bytes_live -= (int64_t)(p->ptr_bytes ? p->ptr_bytes[h] : 0);
        p->ptrs[h] = DS_TOMBSTONE;
        p->ptr_bytes[h] = 0;
        p->ptr_live--;
        removed = 1;
        break;
      }
      h = (h + 1) & (p->ptr_cap - 1);
    }
  }
  pthread_mutex_unlock(&p->mu);
  return removed;
}

static void iris_ui_pool_forget_any(void *ptr) {
  if (!ptr || !iris_ui_pool_any)
    return;
  pthread_mutex_lock(&iris_ui_pool_reg_mu);
  for (IrisUiPool *p = iris_ui_pool_reg_head; p; p = p->next_reg) {
    if (iris_ui_pool_forget_one(p, ptr))
      break;
  }
  pthread_mutex_unlock(&iris_ui_pool_reg_mu);
}

static void iris_ui_pool_ext_rehash_locked(IrisUiPool *p, size_t cap) {
  IrisUiPoolExtSlot *slots =
      (IrisUiPoolExtSlot *)calloc(cap, sizeof(IrisUiPoolExtSlot));
  if (!slots)
    return;
  for (size_t i = 0; i < p->ext_cap; i++) {
    void *v = p->ext[i].ptr;
    if (!v || v == DS_TOMBSTONE || p->ext[i].delta <= 0)
      continue;
    size_t h = ds_hash(v, cap);
    while (slots[h].ptr)
      h = (h + 1) & (cap - 1);
    slots[h] = p->ext[i];
  }
  free(p->ext);
  p->ext = slots;
  p->ext_cap = cap;
  p->ext_used = p->ext_live;
}

static void iris_ui_pool_ext_add(IrisUiPool *p, void *ptr, int32_t kind,
                                 int64_t delta) {
  if (!p || !ptr || delta <= 0)
    return;
  pthread_mutex_lock(&p->mu);
  if (p->ext_cap == 0 || (p->ext_used + 1) * 2 > p->ext_cap) {
    size_t cap = p->ext_cap == 0
                     ? 256
                     : ((p->ext_live + 1) * 4 > p->ext_cap ? p->ext_cap * 2
                                                           : p->ext_cap);
    iris_ui_pool_ext_rehash_locked(p, cap);
    if (!p->ext) {
      pthread_mutex_unlock(&p->mu);
      return;
    }
  }
  size_t h = ds_hash(ptr, p->ext_cap);
  size_t reuse = (size_t)-1;
  while (p->ext[h].ptr) {
    if (p->ext[h].ptr == ptr) {
      p->ext[h].delta += delta;
      if (kind != 0)
        p->ext[h].kind = kind;
      pthread_mutex_unlock(&p->mu);
      return;
    }
    if (p->ext[h].ptr == DS_TOMBSTONE && reuse == (size_t)-1)
      reuse = h;
    h = (h + 1) & (p->ext_cap - 1);
  }
  size_t idx = (reuse != (size_t)-1) ? reuse : h;
  p->ext[idx].ptr = ptr;
  p->ext[idx].kind = kind;
  p->ext[idx].delta = delta;
  if (reuse == (size_t)-1)
    p->ext_used++;
  p->ext_live++;
  pthread_mutex_unlock(&p->mu);
}

static void iris_ui_pool_ext_sub(IrisUiPool *p, void *ptr) {
  if (!p || !ptr || p->ext_cap == 0)
    return;
  pthread_mutex_lock(&p->mu);
  if (p->ext_cap != 0) {
    size_t h = ds_hash(ptr, p->ext_cap);
    while (p->ext[h].ptr) {
      if (p->ext[h].ptr == ptr) {
        if (p->ext[h].delta > 0) {
          p->ext[h].delta--;
          if (p->ext[h].delta == 0) {
            p->ext[h].ptr = DS_TOMBSTONE;
            p->ext_live--;
          }
        }
        break;
      }
      h = (h + 1) & (p->ext_cap - 1);
    }
  }
  pthread_mutex_unlock(&p->mu);
}

static void xfree(void *ptr) {
  if (!ptr)
    return;
  if (iris_ui_pool_any)
    iris_ui_pool_forget_any(ptr);
  size_t bytes = 0;
  ds_contains_and_remove(ptr, &bytes);
  free(ptr);
}

static void *xmalloc(size_t n) {
  void *p = malloc(n);
  if (!p) {
    fprintf(stderr, "iris: out of memory\n");
    iris_fatal();
  }
  ds_add(p, n);
  if (iris_active_ui_pool)
    iris_ui_pool_track_alloc(iris_active_ui_pool, p, n);
  return p;
}

static void *xcalloc(size_t n, size_t sz) {
  void *p = calloc(n, sz);
  if (!p) {
    fprintf(stderr, "iris: out of memory\n");
    iris_fatal();
  }
  ds_add(p, n * sz);
  if (iris_active_ui_pool)
    iris_ui_pool_track_alloc(iris_active_ui_pool, p, n * sz);
  return p;
}

static void *xrealloc(void *p, size_t n) {
  if (p) {
    if (iris_ui_pool_any)
      iris_ui_pool_forget_any(p);
    pthread_mutex_lock(&ds_mu);
    ds_remove_locked(p, NULL);
    pthread_mutex_unlock(&ds_mu);
  }

  void *q = realloc(p, n);
  if (!q) {
    fprintf(stderr, "iris: out of memory\n");
    iris_fatal();
  }
  ds_add(q, n);
  if (iris_active_ui_pool)
    iris_ui_pool_track_alloc(iris_active_ui_pool, q, n);
  return q;
}

static char *xstrdup(const char *s) {
  if (!s)
    return NULL;
  size_t n = strlen(s) + 1;
  char *d = xmalloc(n);
  memcpy(d, s, n);
  return d;
}

void *iris_alloc_bytes(uint64_t bytes) {
  return xmalloc((size_t)(bytes == 0 ? 1 : bytes));
}

void iris_free_bytes(void *ptr) {
  if (ptr)
    iris_forget_native_drop(ptr);
  xfree(ptr);
}

/* Derived from LLVM compiler-rt's x86_64/chkstk.S (Apache-2.0 WITH
 * LLVM-exception):
 * https://github.com/llvm/llvm-project/blob/main/compiler-rt/lib/builtins/x86_64/chkstk.S
 *
 * Canonical compiler-rt stack probe for the Windows x86-64 ABI. LLVM's MinGW
 * object writer calls `___chkstk_ms` before reserving a frame larger than one
 * page. ORC has no archive-link step from which to obtain libgcc's copy, so the
 * JIT registers this routine under that external name. It probes without
 * changing RSP and preserves RAX/RCX, as required by the ABI. */
#if defined(_WIN64) && defined(__x86_64__) &&                                  \
    (defined(__GNUC__) || defined(__clang__))
__attribute__((naked)) void iris_chkstk_ms(void) {
  __asm__("pushq %rcx\n\t"
          "pushq %rax\n\t"
          "cmpq $0x1000, %rax\n\t"
          "leaq 24(%rsp), %rcx\n\t"
          "jb 2f\n\t"
          "1:\n\t"
          "subq $0x1000, %rcx\n\t"
          "testq %rcx, (%rcx)\n\t"
          "subq $0x1000, %rax\n\t"
          "cmpq $0x1000, %rax\n\t"
          "ja 1b\n\t"
          "2:\n\t"
          "subq %rax, %rcx\n\t"
          "testq %rcx, (%rcx)\n\t"
          "popq %rax\n\t"
          "popq %rcx\n\t"
          "retq\n\t");
}
#else
void iris_chkstk_ms(void) {}
#endif

/* GCC/MinGW may inject a call to `__main` at the beginning of a function named
 * `main`.  A normal executable obtains it from libgcc; an ORC object has no
 * archive-link step, so the JIT maps `__main` to this equivalent no-op. */
void iris_mingw_main(void) {}

// ---------------------------------------------------------------------------
// Boxing / Unboxing
// ---------------------------------------------------------------------------

static IrisVal *box_heap_ref(IrisTag tag, void *ptr, int32_t rc_kind) {
  IrisVal *r = xmalloc(sizeof(IrisVal));
  r->tag = tag;
  r->ptr = ptr;
  if (ptr)
    iris_retain_kind(ptr, rc_kind);
  return r;
}

static void *unbox_heap_ref(IrisVal *v, IrisTag expected_tag,
                            const char *name) {
  if (!v)
    return NULL;
  if (v->tag == expected_tag)
    return v->ptr;
  fprintf(stderr, "iris: %s type mismatch (tag=%d)\n", name, v->tag);
  iris_fatal();
}

IrisVal *iris_box_i64(int64_t v) {
  IrisVal *r = xmalloc(sizeof(IrisVal));
  r->tag = IRIS_TAG_I64;
  r->i64 = v;
  return r;
}
IrisVal *iris_box_i32(int32_t v) {
  IrisVal *r = xmalloc(sizeof(IrisVal));
  r->tag = IRIS_TAG_I32;
  r->i32 = v;
  return r;
}
IrisVal *iris_box_f64(double v) {
  IrisVal *r = xmalloc(sizeof(IrisVal));
  r->tag = IRIS_TAG_F64;
  r->f64 = v;
  return r;
}
IrisVal *iris_box_f32(float v) {
  IrisVal *r = xmalloc(sizeof(IrisVal));
  r->tag = IRIS_TAG_F32;
  r->f32 = v;
  return r;
}
IrisVal *iris_box_bool(int v) {
  IrisVal *r = xmalloc(sizeof(IrisVal));
  r->tag = IRIS_TAG_BOOL;
  r->boolean = (uint8_t)(v != 0);
  return r;
}
IrisVal *iris_box_str(const char *s) {
  IrisVal *r = xmalloc(sizeof(IrisVal));
  r->tag = IRIS_TAG_STR;
  r->str = xstrdup(s);
  if (r->str)
    iris_retain_kind(r->str, IRIS_RC_STR);
  return r;
}
/* Box a string this function just allocated, taking ownership of it. Like
 * iris_box_str, the box holds one counted reference, so freeing the box
 * releases exactly that reference. A box that held an uncounted string would
 * release a count someone else took: a record field retained from a split
 * list was freed with the list (and its memory reused) while still in use. */
static IrisVal *box_owned_str(char *s) {
  IrisVal *r = xmalloc(sizeof(IrisVal));
  r->tag = IRIS_TAG_STR;
  r->str = s;
  if (s)
    iris_retain_kind(s, IRIS_RC_STR);
  return r;
}
IrisVal *iris_box_list(IrisList *list) {
  return box_heap_ref(IRIS_TAG_LIST, list, IRIS_RC_LIST);
}
/* A struct is stored as an IrisList of boxed field values -- the shape every
 * generic value walker in this file already expects. See known-issues #61. */
IrisVal *iris_box_struct(IrisList *fields) {
  return box_heap_ref(IRIS_TAG_STRUCT, fields, IRIS_RC_LIST);
}
/* Nominal records are not reference-counted by GcAnnotatePass (local
 * bindings, list_get results, nested record fields, and trait objects hold
 * raw pointers without retaining them). Taking a single IRIS_RC_NATIVE_OBJECT
 * reference on the box caused the first list release or list_set replacement
 * to free a record still referenced by unboxed bindings. Records are
 * reclaimed by frame pools (ui_frame_pool_destroy) or at process exit. */
IrisVal *iris_box_native_object(void *object) {
  IrisVal *r = xmalloc(sizeof(IrisVal));
  r->tag = IRIS_TAG_NATIVE_OBJECT;
  r->ptr = object;
  return r;
}
#ifdef IRIS_SANITIZED
/* Built with IRIS_SANITIZE: report memory errors, not leaks. The runtime
 * keeps some allocations for the life of the process by design (tasks are
 * never freed). ASAN_OPTIONS=detect_leaks=1 turns the leak report back on. */
const char *__asan_default_options(void) { return "detect_leaks=0"; }
#endif
/* A fixed array of scalars put in a generic slot: a closure capture, a list
 * element, a spawn environment. Generated code keeps such an array on the
 * stack, so the slot gets a heap copy, owned by the box and freed with it.
 * Arrays are values -- the interpreter copies them into the slot too. The
 * raw stack address used to go in, and freeing the closure free()d it. */
IrisVal *iris_box_scalar_array(const void *data, int64_t bytes) {
  void *copy = xmalloc(bytes > 0 ? (size_t)bytes : 1);
  if (bytes > 0 && data)
    memcpy(copy, data, (size_t)bytes);
  return box_heap_ref(IRIS_TAG_NATIVE_OBJECT, copy, IRIS_RC_NATIVE_OBJECT);
}
/* Tasks are never freed (a result may still be read through any handle), so
 * a task box holds a plain pointer and takes no reference. */
IrisVal *iris_box_task(void *task) {
  IrisVal *r = xmalloc(sizeof(IrisVal));
  r->tag = IRIS_TAG_TASK;
  r->ptr = task;
  return r;
}
IrisVal *iris_box_map(IrisMap *map) {
  return box_heap_ref(IRIS_TAG_MAP, map, IRIS_RC_MAP);
}
IrisVal *iris_box_option(IrisOption *opt) {
  return box_heap_ref(IRIS_TAG_OPTION, opt, IRIS_RC_OPTION);
}
IrisVal *iris_box_result(IrisResult *res) {
  return box_heap_ref(IRIS_TAG_RESULT, res, IRIS_RC_RESULT);
}
IrisVal *iris_box_chan(IrisChannel *chan) {
  return box_heap_ref(IRIS_TAG_CHAN, chan, IRIS_RC_CHAN);
}
IrisVal *iris_box_task_group(IrisTaskGroup *tg) {
  return box_heap_ref(IRIS_TAG_TASK_GROUP, tg, IRIS_RC_TASK_GROUP);
}
IrisVal *iris_box_weak_ref(IrisWeakRef *w) {
  return box_heap_ref(IRIS_TAG_WEAK_REF, w, IRIS_RC_WEAK_REF);
}
IrisVal *iris_box_atomic(IrisAtomic *atomic) {
  return box_heap_ref(IRIS_TAG_ATOMIC, atomic, IRIS_RC_ATOMIC);
}
IrisVal *iris_box_mutex(IrisMutex *mutex) {
  return box_heap_ref(IRIS_TAG_MUTEX, mutex, IRIS_RC_MUTEX);
}
IrisVal *iris_box_grad(IrisGrad *grad) {
  return box_heap_ref(IRIS_TAG_GRAD, grad, IRIS_RC_GRAD);
}
IrisVal *iris_box_sparse(IrisSparse *sparse) {
  return box_heap_ref(IRIS_TAG_SPARSE, sparse, IRIS_RC_SPARSE);
}

int64_t iris_unbox_i64(IrisVal *v) {
  if (!v)
    return 0;
  switch (v->tag) {
  case IRIS_TAG_I64:
    return v->i64;
  case IRIS_TAG_I32:
    return (int64_t)v->i32;
  case IRIS_TAG_F64:
    return (int64_t)v->f64;
  case IRIS_TAG_F32:
    return (int64_t)v->f32;
  default:
    fprintf(stderr, "iris: unbox_i64 type mismatch (tag=%d)\n", v->tag);
    iris_fatal();
  }
}
double iris_unbox_f64(IrisVal *v) {
  if (!v)
    return 0.0;
  switch (v->tag) {
  case IRIS_TAG_F64:
    return v->f64;
  case IRIS_TAG_F32:
    return (double)v->f32;
  case IRIS_TAG_I64:
    return (double)v->i64;
  case IRIS_TAG_I32:
    return (double)v->i32;
  default:
    fprintf(stderr, "iris: unbox_f64 type mismatch (tag=%d)\n", v->tag);
    iris_fatal();
  }
}
int iris_unbox_bool(IrisVal *v) {
  if (!v)
    return 0;
  if (v->tag == IRIS_TAG_BOOL)
    return (int)v->boolean;
  fprintf(stderr, "iris: unbox_bool type mismatch (tag=%d)\n", v->tag);
  iris_fatal();
}
char *iris_unbox_str(IrisVal *v) {
  if (!v)
    return (char *)"";
  if (v->tag == IRIS_TAG_STR)
    return v->str;
  fprintf(stderr, "iris: unbox_str type mismatch (tag=%d)\n", v->tag);
  iris_fatal();
}
IrisList *iris_unbox_list(IrisVal *v) {
  return (IrisList *)unbox_heap_ref(v, IRIS_TAG_LIST, "unbox_list");
}
IrisMap *iris_unbox_map(IrisVal *v) {
  return (IrisMap *)unbox_heap_ref(v, IRIS_TAG_MAP, "unbox_map");
}
IrisOption *iris_unbox_option(IrisVal *v) {
  return (IrisOption *)unbox_heap_ref(v, IRIS_TAG_OPTION, "unbox_option");
}
IrisResult *iris_unbox_result(IrisVal *v) {
  return (IrisResult *)unbox_heap_ref(v, IRIS_TAG_RESULT, "unbox_result");
}
IrisChannel *iris_unbox_chan(IrisVal *v) {
  return (IrisChannel *)unbox_heap_ref(v, IRIS_TAG_CHAN, "unbox_chan");
}
IrisTaskGroup *iris_unbox_task_group(IrisVal *v) {
  return (IrisTaskGroup *)unbox_heap_ref(v, IRIS_TAG_TASK_GROUP,
                                         "unbox_task_group");
}
IrisWeakRef *iris_unbox_weak_ref(IrisVal *v) {
  return (IrisWeakRef *)unbox_heap_ref(v, IRIS_TAG_WEAK_REF, "unbox_weak_ref");
}
IrisAtomic *iris_unbox_atomic(IrisVal *v) {
  return (IrisAtomic *)unbox_heap_ref(v, IRIS_TAG_ATOMIC, "unbox_atomic");
}
IrisMutex *iris_unbox_mutex(IrisVal *v) {
  return (IrisMutex *)unbox_heap_ref(v, IRIS_TAG_MUTEX, "unbox_mutex");
}
IrisGrad *iris_unbox_grad(IrisVal *v) {
  return (IrisGrad *)unbox_heap_ref(v, IRIS_TAG_GRAD, "unbox_grad");
}
IrisSparse *iris_unbox_sparse(IrisVal *v) {
  return (IrisSparse *)unbox_heap_ref(v, IRIS_TAG_SPARSE, "unbox_sparse");
}
void *iris_unbox_task(IrisVal *v) {
  return unbox_heap_ref(v, IRIS_TAG_TASK, "unbox_task");
}
void *iris_unbox_native_object(IrisVal *v) {
  return unbox_heap_ref(v, IRIS_TAG_NATIVE_OBJECT, "unbox_native_object");
}

/* The text IRIS prints for a float, the same on both backends (the
 * interpreter runs the same steps in format_float): the fewest significant
 * digits that read back as the same value, each precision tried with a
 * correctly rounded conversion, written without an exponent. NaN, inf and
 * -inf by name. `single` checks the round trip as an f32. `out` needs room
 * for 330 characters (the largest double written out in full). */
static void iris_format_float(double v, int single, char *out, size_t room) {
  if (isnan(v)) {
    snprintf(out, room, "NaN");
    return;
  }
  if (isinf(v)) {
    snprintf(out, room, v > 0 ? "inf" : "-inf");
    return;
  }
  if (v == 0.0) {
    snprintf(out, room, signbit(v) ? "-0" : "0");
    return;
  }
  int max = single ? 9 : 17;
  char sci[64];
  for (int p = 1; p <= max; p++) {
    snprintf(sci, sizeof(sci), "%.*e", p - 1, v);
    if (single ? (strtof(sci, NULL) == (float)v) : (strtod(sci, NULL) == v))
      break;
  }
  int negative = sci[0] == '-';
  const char *mantissa = sci + negative;
  const char *e = strchr(mantissa, 'e');
  char digits[32];
  int nd = 0;
  for (const char *c = mantissa; c < e && nd < 31; c++) {
    if (*c != '.')
      digits[nd++] = *c;
  }
  while (nd > 1 && digits[nd - 1] == '0')
    nd--;
  int exp = atoi(e + 1);
  char text[400];
  int w = 0;
  if (negative)
    text[w++] = '-';
  if (exp >= 0) {
    for (int i = 0; i <= exp; i++)
      text[w++] = i < nd ? digits[i] : '0';
    if (nd > exp + 1) {
      text[w++] = '.';
      for (int i = exp + 1; i < nd; i++)
        text[w++] = digits[i];
    }
  } else {
    text[w++] = '0';
    text[w++] = '.';
    for (int i = 0; i < -exp - 1; i++)
      text[w++] = '0';
    for (int i = 0; i < nd; i++)
      text[w++] = digits[i];
  }
  text[w] = '\0';
  snprintf(out, room, "%s", text);
}

// ---------------------------------------------------------------------------
// Internal print helper
// ---------------------------------------------------------------------------

// Tag-only enum values stored in struct fields or passed across generic pointer
// slots use small integer tags encoded directly in the pointer address
// (0..0xFFFF). Never dereference or RC-track them as heap allocations.
static inline int iris_is_immediate_tag(const void *ptr) {
  return (uintptr_t)ptr <= (uintptr_t)0xFFFF;
}

static void print_val_inline(IrisVal *v) {
  if (!v) {
    printf("unit");
    return;
  }
  if (iris_is_immediate_tag(v)) {
    printf("%" PRId64, (int64_t)(uintptr_t)v);
    return;
  }
  switch (v->tag) {
  case IRIS_TAG_I64:
    printf("%" PRId64, v->i64);
    break;
  case IRIS_TAG_I32:
    printf("%d", v->i32);
    break;
  case IRIS_TAG_F64: {
    char t[400];
    iris_format_float(v->f64, 0, t, sizeof(t));
    printf("%s", t);
    break;
  }
  case IRIS_TAG_F32: {
    char t[400];
    iris_format_float((double)v->f32, 1, t, sizeof(t));
    printf("%s", t);
    break;
  }
  case IRIS_TAG_BOOL:
    printf("%s", v->boolean ? "true" : "false");
    break;
  case IRIS_TAG_STR:
    printf("%s", v->str);
    break;
  case IRIS_TAG_UNIT:
    printf("unit");
    break;
  case IRIS_TAG_ENUM: {
    IrisEnum *e = (IrisEnum *)v->ptr;
    printf("variant(%" PRId64 ")", e ? e->tag : 0);
    break;
  }
  case IRIS_TAG_OPTION: {
    IrisOption *o = (IrisOption *)v->ptr;
    if (o && o->has_value) {
      printf("some(");
      print_val_inline(o->value);
      printf(")");
    } else
      printf("none");
    break;
  }
  case IRIS_TAG_RESULT: {
    IrisResult *r = (IrisResult *)v->ptr;
    if (r->is_ok) {
      printf("ok(");
      print_val_inline(r->value);
      printf(")");
    } else {
      printf("err(");
      print_val_inline(r->value);
      printf(")");
    }
    break;
  }
  case IRIS_TAG_LIST: {
    IrisList *l = (IrisList *)v->ptr;
    printf("[");
    for (size_t i = 0; i < l->len; i++) {
      if (i > 0)
        printf(", ");
      print_val_inline(l->data[i]);
    }
    printf("]");
    break;
  }
  default:
    printf("<val:%d>", v->tag);
    break;
  }
}

// ---------------------------------------------------------------------------
// Print (public)
// ---------------------------------------------------------------------------

void iris_print(void *v) {
  if (!v) {
    printf("unit\n");
    return;
  }
  print_val_inline((IrisVal *)v);
  printf("\n");
}
void iris_print_i64(int64_t v) { printf("%" PRId64 "\n", v); }
void iris_print_i32(int32_t v) { printf("%d\n", v); }
void iris_print_f64(double v) {
  char t[400];
  iris_format_float(v, 0, t, sizeof(t));
  printf("%s\n", t);
}
void iris_print_f32(float v) {
  char t[400];
  iris_format_float((double)v, 1, t, sizeof(t));
  printf("%s\n", t);
}
void iris_print_bool(int v) { printf("%s\n", v ? "true" : "false"); }
void iris_print_str(const char *s) { printf("%s\n", s ? s : ""); }

void iris_panic(const char *msg) {
  const char *m = msg ? msg : "(null)";
  char text[2048];
  snprintf(text, sizeof text, "program panicked: %s", m);
  /* An assert's message already says where it is. */
  iris_report_and_exit("E0406", text, strstr(m, ".iris:") == NULL);
}

void iris_bounds_check_abort(int64_t index, int64_t size) {
  iris_runtime_error("E0403",
                     "index %" PRId64 " is out of bounds for length %" PRId64,
                     index, size);
}

void iris_bounds_check(int64_t index, int64_t size) {
  if (index < 0 || index >= size)
    iris_bounds_check_abort(index, size);
}

/* iris_panic_at — like iris_panic but includes a compile-time source location
 * string (e.g. "in function 'foo'") embedded by the IRIS LLVM codegen. */
void iris_panic_at(const char *msg, const char *location) {
  (void)location;
  iris_panic(msg);
}

// ---------------------------------------------------------------------------
// I/O
// ---------------------------------------------------------------------------

char *iris_read_line(void) {
  char buf[4096];
  if (!fgets(buf, sizeof(buf), stdin))
    return xstrdup("");
  size_t n = strlen(buf);
  if (n > 0 && buf[n - 1] == '\n')
    buf[--n] = '\0';
  return xstrdup(buf);
}
int64_t iris_read_i64(void) {
  int64_t v = 0;
  if (scanf("%" SCNd64, &v) != 1)
    v = 0;
  return v;
}
double iris_read_f64(void) {
  double v = 0.0;
  if (scanf("%lf", &v) != 1)
    v = 0.0;
  return v;
}

// ---------------------------------------------------------------------------
// String operations
// ---------------------------------------------------------------------------

int64_t iris_str_len(const char *s) { return (int64_t)strlen(s); }

char *iris_str_concat(const char *a, const char *b) {
  size_t la = strlen(a), lb = strlen(b);
  char *r = xmalloc(la + lb + 1);
  memcpy(r, a, la);
  memcpy(r + la, b, lb + 1);
  return r;
}

int iris_str_eq(const char *a, const char *b) { return strcmp(a, b) == 0; }
int64_t iris_str_cmp(const char *a, const char *b) {
  return (int64_t)strcmp(a, b);
}
int iris_str_contains(const char *s, const char *sub) {
  return strstr(s, sub) != NULL;
}
int iris_str_starts_with(const char *s, const char *pfx) {
  return strncmp(s, pfx, strlen(pfx)) == 0;
}
int iris_str_ends_with(const char *s, const char *sfx) {
  size_t ls = strlen(s), lp = strlen(sfx);
  return lp <= ls && strcmp(s + ls - lp, sfx) == 0;
}

char *iris_str_to_upper(const char *s) {
  size_t n = strlen(s);
  char *r = xmalloc(n + 1);
  for (size_t i = 0; i <= n; i++)
    r[i] = (char)toupper((unsigned char)s[i]);
  return r;
}
char *iris_str_to_lower(const char *s) {
  size_t n = strlen(s);
  char *r = xmalloc(n + 1);
  for (size_t i = 0; i <= n; i++)
    r[i] = (char)tolower((unsigned char)s[i]);
  return r;
}
char *iris_str_trim(const char *s) {
  while (isspace((unsigned char)*s))
    s++;
  const char *end = s + strlen(s);
  while (end > s && isspace((unsigned char)*(end - 1)))
    end--;
  size_t n = (size_t)(end - s);
  char *r = xmalloc(n + 1);
  memcpy(r, s, n);
  r[n] = '\0';
  return r;
}
char *iris_str_repeat(const char *s, int64_t n) {
  if (n <= 0) {
    char *r = xmalloc(1);
    r[0] = '\0';
    return r;
  }
  size_t ls = strlen(s), total = ls * (size_t)n;
  char *r = xmalloc(total + 1);
  for (int64_t i = 0; i < n; i++)
    memcpy(r + (size_t)i * ls, s, ls);
  r[total] = '\0';
  return r;
}
int64_t iris_str_index(const char *s, int64_t i) {
  size_t n = strlen(s);
  if (i < 0 || (size_t)i >= n) {
    iris_runtime_error("E0403",
                       "index %" PRId64 " is out of bounds for length %zu",
                       (int64_t)i, n);
    iris_fatal();
  }
  return (int64_t)(unsigned char)s[i];
}
char *iris_str_slice(const char *s, int64_t start, int64_t end_idx) {
  size_t n = strlen(s);
  if (start < 0)
    start = 0;
  if ((size_t)end_idx > n)
    end_idx = (int64_t)n;
  if (start >= end_idx) {
    char *r = xmalloc(1);
    r[0] = '\0';
    return r;
  }
  size_t len = (size_t)(end_idx - start);
  char *r = xmalloc(len + 1);
  memcpy(r, s + start, len);
  r[len] = '\0';
  return r;
}
IrisOption *iris_str_find(const char *s, const char *sub) {
  IrisOption *opt = xmalloc(sizeof(IrisOption));
  const char *p = strstr(s, sub);
  if (!p) {
    opt->has_value = 0;
    opt->value = NULL;
  } else {
    opt->has_value = 1;
    opt->value = iris_box_i64((int64_t)(p - s));
  }
  return opt;
}
char *iris_str_replace(const char *s, const char *old_s, const char *new_s) {
  size_t ls = strlen(s), lo = strlen(old_s), ln = strlen(new_s);
  if (lo == 0)
    return xstrdup(s);
  size_t count = 0;
  const char *p = s;
  while ((p = strstr(p, old_s)) != NULL) {
    count++;
    p += lo;
  }
  size_t rlen = ls + count * (ln - lo);
  char *r = xmalloc(rlen + 1);
  char *w = r;
  p = s;
  const char *next;
  while ((next = strstr(p, old_s)) != NULL) {
    size_t seg = (size_t)(next - p);
    memcpy(w, p, seg);
    w += seg;
    memcpy(w, new_s, ln);
    w += ln;
    p = next + lo;
  }
  strcpy(w, p);
  return r;
}
char *iris_const_str(void) {
  return (char *)"";
} /* fallback; should never be reached */

/* Phase 95: split/join */
IrisList *iris_str_split(const char *s, const char *delim) {
  IrisList *result = iris_list_new();
  size_t dlen = strlen(delim);
  if (dlen == 0) {
    /* split into individual characters */
    while (*s) {
      char buf[5] = {0};
      /* simple single-byte split (ASCII) */
      buf[0] = *s++;
      iris_list_push(result, box_owned_str(xstrdup(buf)));
    }
    return result;
  }
  const char *p = s;
  const char *found;
  while ((found = strstr(p, delim)) != NULL) {
    size_t seg = (size_t)(found - p);
    char *part = (char *)xmalloc(seg + 1);
    memcpy(part, p, seg);
    part[seg] = '\0';
    iris_list_push(result, box_owned_str(part));
    p = found + dlen;
  }
  /* last segment */
  iris_list_push(result, box_owned_str(xstrdup(p)));
  return result;
}

char *iris_str_join(IrisList *list, const char *delim) {
  if (!list || list->len == 0)
    return xstrdup("");
  size_t dlen = strlen(delim);
  size_t total = 0;
  for (int64_t i = 0; i < list->len; i++) {
    IrisVal *v = list->data[i];
    if (v && v->tag == IRIS_TAG_STR && v->str)
      total += strlen(v->str);
    if (i + 1 < list->len)
      total += dlen;
  }
  char *r = (char *)xmalloc(total + 1);
  char *w = r;
  for (int64_t i = 0; i < list->len; i++) {
    IrisVal *v = list->data[i];
    if (v && v->tag == IRIS_TAG_STR && v->str) {
      size_t sl = strlen(v->str);
      memcpy(w, v->str, sl);
      w += sl;
    }
    if (i + 1 < list->len) {
      memcpy(w, delim, dlen);
      w += dlen;
    }
  }
  *w = '\0';
  return r;
}

// ---------------------------------------------------------------------------
// Typed value-to-string conversions
// ---------------------------------------------------------------------------

char *iris_i64_to_str(int64_t v) {
  char buf[32];
  /* Not "%ld": `long` is 32 bits on Windows, which truncated 6000000000. */
  snprintf(buf, sizeof(buf), "%" PRId64, v);
  return xstrdup(buf);
}
char *iris_i32_to_str(int32_t v) {
  char buf[24];
  snprintf(buf, sizeof(buf), "%d", v);
  return xstrdup(buf);
}
char *iris_f64_to_str(double v) {
  char buf[400];
  iris_format_float(v, 0, buf, sizeof(buf));
  return xstrdup(buf);
}
char *iris_f32_to_str(float v) {
  char buf[400];
  iris_format_float((double)v, 1, buf, sizeof(buf));
  return xstrdup(buf);
}
char *iris_bool_to_str(int v) { return xstrdup(v ? "true" : "false"); }
char *iris_str_to_str(const char *s) { return xstrdup(s); }

/* Formats a value as it appears *inside* a container.
 *
 * Strings are quoted here so that `err("bad")` is distinguishable from a
 * variant literally named `bad` -- the interpreter does the same, and the two
 * backends must agree. */
static char *value_to_str_nested(IrisVal *v) {
  if (v && v->tag == IRIS_TAG_STR) {
    const char *raw = v->str ? v->str : "";
    size_t n = strlen(raw) + 3;
    char *buf = xmalloc(n);
    snprintf(buf, n, "\"%s\"", raw);
    return buf;
  }
  return iris_value_to_str(v);
}

/* Wraps `inner` as `label(inner)` and frees it. */
static char *wrap_labelled(const char *label, char *inner) {
  size_t n = strlen(label) + strlen(inner) + 3;
  char *buf = xmalloc(n);
  snprintf(buf, n, "%s(%s)", label, inner);
  xfree(inner);
  return buf;
}

char *iris_value_to_str(IrisVal *v) {
  if (!v)
    return xstrdup("unit");
  if (iris_is_immediate_tag(v))
    return iris_i64_to_str((int64_t)(uintptr_t)v);
  switch (v->tag) {
  case IRIS_TAG_I64:
    return iris_i64_to_str(v->i64);
  case IRIS_TAG_I32:
    return iris_i32_to_str(v->i32);
  case IRIS_TAG_F64:
    return iris_f64_to_str(v->f64);
  case IRIS_TAG_F32:
    return iris_f32_to_str(v->f32);
  case IRIS_TAG_BOOL:
    return iris_bool_to_str(v->boolean);
  case IRIS_TAG_STR:
    return xstrdup(v->str);
  case IRIS_TAG_UNIT:
    return xstrdup("unit");
  /* Containers were missing entirely, so `to_str(s.find(..))` fell to the
   * `<val:N>` default and printed `find: <val:8>` where the interpreter
   * says `find: some(6)`. See known-issues #51. */
  case IRIS_TAG_OPTION: {
    IrisOption *o = (IrisOption *)v->ptr;
    if (!o || !o->has_value)
      return xstrdup("none");
    return wrap_labelled("some", value_to_str_nested(o->value));
  }
  case IRIS_TAG_RESULT: {
    IrisResult *r = (IrisResult *)v->ptr;
    if (!r)
      return xstrdup("unit");
    return wrap_labelled(r->is_ok ? "ok" : "err",
                         value_to_str_nested(r->value));
  }
  default: {
    char buf[32];
    snprintf(buf, sizeof(buf), "<val:%d>", v->tag);
    return xstrdup(buf);
  }
  }
}

// Parse helpers
IrisOption *iris_parse_i64(const char *s) {
  IrisOption *opt = xmalloc(sizeof(IrisOption));
  char *end;
  errno = 0;
  long long v = strtoll(s, &end, 10);
  if (end == s || *end != '\0' || errno != 0) {
    opt->has_value = 0;
    opt->value = NULL;
  } else {
    opt->has_value = 1;
    opt->value = iris_box_i64((int64_t)v);
  }
  return opt;
}
IrisOption *iris_parse_f64(const char *s) {
  IrisOption *opt = xmalloc(sizeof(IrisOption));
  char *end;
  errno = 0;
  double v = strtod(s, &end);
  if (end == s || *end != '\0' || errno != 0) {
    opt->has_value = 0;
    opt->value = NULL;
  } else {
    opt->has_value = 1;
    opt->value = iris_box_f64(v);
  }
  return opt;
}

// ---------------------------------------------------------------------------
// Math helpers
// ---------------------------------------------------------------------------

int64_t iris_pow_i64(int64_t base, int64_t exp) {
  if (exp < 0)
    return 0;
  int64_t result = 1;
  while (exp > 0) {
    if (exp & 1)
      result *= base;
    base *= base;
    exp >>= 1;
  }
  return result;
}
int64_t iris_min_i64(int64_t a, int64_t b) { return a < b ? a : b; }
int64_t iris_max_i64(int64_t a, int64_t b) { return a > b ? a : b; }
int64_t iris_abs_i64(int64_t v) { return v < 0 ? -v : v; }
double iris_sign_f64(double v) {
  return v > 0.0 ? 1.0 : (v < 0.0 ? -1.0 : 0.0);
}
double iris_clamp_f64(double x, double lo, double hi) {
  return x < lo ? lo : (x > hi ? hi : x);
}
double iris_pow_f64(double base, double exp) { return pow(base, exp); }
double iris_min_f64(double a, double b) { return a < b ? a : b; }
double iris_max_f64(double a, double b) { return a > b ? a : b; }

// ---------------------------------------------------------------------------
// Integer overflow-checked arithmetic
// ---------------------------------------------------------------------------

#if defined(__has_builtin) && __has_builtin(__builtin_add_overflow)

int64_t iris_add_checked(int64_t a, int64_t b) {
  int64_t result;
  if (__builtin_add_overflow(a, b, &result)) {
    iris_runtime_error(
        "E0409", "integer overflow in addition (%" PRId64 " + %" PRId64 ")", a,
        b);
    iris_fatal();
  }
  return result;
}
int64_t iris_sub_checked(int64_t a, int64_t b) {
  int64_t result;
  if (__builtin_sub_overflow(a, b, &result)) {
    iris_runtime_error(
        "E0409", "integer overflow in subtraction (%" PRId64 " - %" PRId64 ")",
        a, b);
    iris_fatal();
  }
  return result;
}
int64_t iris_mul_checked(int64_t a, int64_t b) {
  int64_t result;
  if (__builtin_mul_overflow(a, b, &result)) {
    iris_runtime_error(
        "E0409",
        "integer overflow in multiplication (%" PRId64 " * %" PRId64 ")", a, b);
    iris_fatal();
  }
  return result;
}

#else

#include <limits.h>

int64_t iris_add_checked(int64_t a, int64_t b) {
  if ((b > 0 && a > INT64_MAX - b) || (b < 0 && a < INT64_MIN - b)) {
    iris_runtime_error("E0409", "integer overflow in addition");
    iris_fatal();
  }
  return a + b;
}
int64_t iris_sub_checked(int64_t a, int64_t b) {
  if ((b > 0 && a < INT64_MIN + b) || (b < 0 && a > INT64_MAX + b)) {
    iris_runtime_error("E0409", "integer overflow in subtraction");
    iris_fatal();
  }
  return a - b;
}
int64_t iris_mul_checked(int64_t a, int64_t b) {
  if (a == 0 || b == 0)
    return 0;
  int64_t result = a * b;
  if (a != result / b) {
    iris_runtime_error("E0409", "integer overflow in multiplication");
    iris_fatal();
  }
  return result;
}

#endif

// ---------------------------------------------------------------------------
// Option
// ---------------------------------------------------------------------------

IrisOption *iris_make_some(IrisVal *val) {
  IrisOption *o = xmalloc(sizeof(IrisOption));
  o->has_value = 1;
  o->value = val;
  if (val)
    iris_retain(val);
  return o;
}
IrisOption *iris_make_none(void) {
  IrisOption *o = xmalloc(sizeof(IrisOption));
  o->has_value = 0;
  o->value = NULL;
  return o;
}
int iris_is_some(IrisOption *opt) { return opt ? opt->has_value : 0; }
/* Unwrapping the wrong case panics like `panic`, which flushes stdout first:
 * a bare abort() lost everything a piped program had printed. */
IrisVal *iris_option_unwrap(IrisOption *opt) {
  if (!opt || !opt->has_value)
    iris_panic("unwrap called on none");
  return opt->value;
}

// ---------------------------------------------------------------------------
// Result
// ---------------------------------------------------------------------------

IrisResult *iris_make_ok(IrisVal *val) {
  IrisResult *r = xmalloc(sizeof(IrisResult));
  r->is_ok = 1;
  r->value = val;
  if (val)
    iris_retain(val);
  return r;
}
IrisResult *iris_make_err(IrisVal *val) {
  IrisResult *r = xmalloc(sizeof(IrisResult));
  r->is_ok = 0;
  r->value = val;
  if (val)
    iris_retain(val);
  return r;
}
int iris_is_ok(IrisResult *res) { return res ? res->is_ok : 0; }
IrisVal *iris_result_unwrap(IrisResult *res) {
  if (!res || !res->is_ok) {
    char *text = res ? iris_value_to_str(res->value) : NULL;
    char msg[512];
    snprintf(msg, sizeof(msg), "unwrap called on err: %s",
             text ? text : "unit");
    iris_panic(msg);
  }
  return res->value;
}
IrisVal *iris_result_unwrap_err(IrisResult *res) {
  if (!res || res->is_ok)
    iris_panic("unwrap_err called on ok");
  return res->value;
}

// ---------------------------------------------------------------------------
// List
// ---------------------------------------------------------------------------

/* ---- Collection lock ---------------------------------------------------
 *
 * `iris_list_push` grew the buffer with `xrealloc` and advanced `len++` with no
 * synchronisation at all, so two threads inside a `par for` could lose an
 * update, or one could move the buffer while the other wrote through the stale
 * pointer. That is a data race reachable from ordinary safe IRIS:
 *
 *     par for i in 0..2000 { push(shared, i); }
 *
 * It produced the right answer on every run of an eleven-run probe, which is
 * the most dangerous possible result -- two accidents were hiding it. Every
 * push passes through `iris_retain`, which takes a *global* refcount mutex and
 * serialises most of the window; and `iris_par_for` created one thread per
 * iteration, so on a 2-core box thread startup dominated and overlap was
 * minimal. Both disappear on a larger machine.
 *
 * A single global lock rather than per-list: the refcount mutex above is
 * already taken on every element operation, so the serialisation exists
 * regardless and a second global adds little. Per-list mutexes are the upgrade
 * path if profiling ever shows this to matter.
 *
 * Recursive, so that a public entry point which internally calls another
 * (`iris_str_split` builds its result with `iris_list_push`) cannot deadlock.
 * Auditing every internal caller instead would be a standing trap for anyone
 * adding a collection helper later.
 */
static pthread_mutex_t coll_mu = PTHREAD_MUTEX_INITIALIZER;

static void coll_lock(void) { pthread_mutex_lock(&coll_mu); }
static void coll_unlock(void) { pthread_mutex_unlock(&coll_mu); }

// ---------------------------------------------------------------------------
// Transactional speculation
// ---------------------------------------------------------------------------
//
// A frame records the first version of every mutable object touched on the
// current thread. Nested commits transfer those original versions to their
// parent; rollback restores them in reverse order. Whole-file writes are held
// in memory until the outermost commit and reads consult the staged overlay.

typedef enum {
  IRIS_TX_LIST = 1,
  IRIS_TX_MAP = 2,
  IRIS_TX_ATOMIC = 3
} IrisTxKind;

typedef struct IrisTxSnapshot {
  IrisTxKind kind;
  void *target;
  union {
    struct {
      IrisVal **data;
      size_t len;
      size_t cap;
      IrisValChunk *chunks;
      size_t chunk_count;
    } list;
    IrisMap map;
    IrisVal *atomic;
  } before;
  struct IrisTxSnapshot *next;
} IrisTxSnapshot;

typedef struct IrisTxFile {
  char *path;
  char *contents;
  struct IrisTxFile *next;
} IrisTxFile;

typedef struct IrisTxFrame {
  IrisTxSnapshot *snapshots;
  IrisTxFile *files;
  struct IrisTxFrame *parent;
} IrisTxFrame;

static IRIS_THREAD_LOCAL IrisTxFrame *iris_tx_top = NULL;
static IRIS_THREAD_LOCAL uint64_t iris_tx_temp_counter = 0;

static int iris_tx_has_snapshot(IrisTxFrame *frame, IrisTxKind kind,
                                void *target) {
  for (IrisTxSnapshot *s = frame ? frame->snapshots : NULL; s; s = s->next)
    if (s->kind == kind && s->target == target)
      return 1;
  return 0;
}

static int iris_list_is_chunk_ptr(const IrisList *l, const void *ptr) {
  if (!l || !ptr)
    return 0;
  for (const IrisValChunk *c = l->chunks; c; c = c->next) {
    if ((const char *)ptr >= (const char *)&c->items[0] &&
        (const char *)ptr < (const char *)&c->items[c->cap]) {
      return 1;
    }
  }
  return 0;
}

static void iris_tx_free_map_entries(IrisMap *map) {
  if (!map || !map->buckets)
    return;
  for (size_t i = 0; i < map->n_buckets; i++) {
    IrisMapEntry *entry = map->buckets[i];
    while (entry) {
      IrisMapEntry *next = entry->next;
      if (entry->val)
        iris_release(entry->val);
      xfree(entry->key);
      xfree(entry);
      entry = next;
    }
  }
  xfree(map->buckets);
  map->buckets = NULL;
  map->n_buckets = 0;
  map->len = 0;
}

static void iris_tx_discard_snapshot(IrisTxSnapshot *snapshot) {
  if (!snapshot)
    return;
  if (snapshot->kind == IRIS_TX_LIST) {
    IrisList *target = (IrisList *)snapshot->target;
    for (size_t i = 0; i < snapshot->before.list.len; i++) {
      IrisVal *val =
          snapshot->before.list.data ? snapshot->before.list.data[i] : NULL;
      if (val && !iris_list_is_chunk_ptr(target, val))
        iris_release(val);
    }
    xfree(snapshot->before.list.data);
    snapshot->before.list.data = NULL;
    snapshot->before.list.len = snapshot->before.list.cap = 0;
  } else if (snapshot->kind == IRIS_TX_MAP) {
    iris_tx_free_map_entries(&snapshot->before.map);
  } else if (snapshot->kind == IRIS_TX_ATOMIC) {
    if (snapshot->before.atomic)
      iris_release(snapshot->before.atomic);
  }
  xfree(snapshot);
}

static void iris_tx_restore_snapshot(IrisTxSnapshot *snapshot) {
  if (snapshot->kind == IRIS_TX_LIST) {
    IrisList *target = (IrisList *)snapshot->target;
    coll_lock();
    // 1. Release non-chunk elements added during the transaction
    for (size_t i = snapshot->before.list.len; i < target->len; i++) {
      IrisVal *val = target->data[i];
      if (val && !iris_list_is_chunk_ptr(target, val))
        iris_release(val);
    }
    // 2. Restore modified elements in 0..snapshot_len - 1
    for (size_t i = 0; i < snapshot->before.list.len; i++) {
      IrisVal *old_val =
          snapshot->before.list.data ? snapshot->before.list.data[i] : NULL;
      IrisVal *curr_val = target->data[i];
      if (curr_val != old_val) {
        if (curr_val && !iris_list_is_chunk_ptr(target, curr_val))
          iris_release(curr_val);
        target->data[i] = old_val;
      } else {
        if (old_val && !iris_list_is_chunk_ptr(target, old_val))
          iris_release(old_val);
      }
    }
    // 3. Free any chunks allocated during this transaction
    IrisValChunk *c = target->chunks;
    while (c && c != snapshot->before.list.chunks) {
      IrisValChunk *next = c->next;
      xfree(c);
      c = next;
    }
    target->chunks = snapshot->before.list.chunks;
    if (target->chunks) {
      target->chunks->count = snapshot->before.list.chunk_count;
    }
    target->len = snapshot->before.list.len;
    xfree(snapshot->before.list.data);
    snapshot->before.list.data = NULL;
    snapshot->before.list.len = snapshot->before.list.cap = 0;
    coll_unlock();
  } else if (snapshot->kind == IRIS_TX_MAP) {
    IrisMap *target = (IrisMap *)snapshot->target;
    coll_lock();
    iris_tx_free_map_entries(target);
    *target = snapshot->before.map;
    snapshot->before.map.buckets = NULL;
    snapshot->before.map.n_buckets = snapshot->before.map.len = 0;
    coll_unlock();
  } else if (snapshot->kind == IRIS_TX_ATOMIC) {
    IrisAtomic *target = (IrisAtomic *)snapshot->target;
    pthread_mutex_lock(&target->mu);
    if (target->val)
      iris_release(target->val);
    target->val = snapshot->before.atomic;
    snapshot->before.atomic = NULL;
    pthread_mutex_unlock(&target->mu);
  }
  iris_tx_discard_snapshot(snapshot);
}

static void iris_tx_record_list(IrisList *list) {
  if (!iris_tx_top || !list ||
      iris_tx_has_snapshot(iris_tx_top, IRIS_TX_LIST, list))
    return;
  IrisTxSnapshot *snapshot = xcalloc(1, sizeof(*snapshot));
  snapshot->kind = IRIS_TX_LIST;
  snapshot->target = list;
  coll_lock();
  snapshot->before.list.len = list->len;
  snapshot->before.list.cap = list->cap;
  snapshot->before.list.chunks = list->chunks;
  snapshot->before.list.chunk_count = list->chunks ? list->chunks->count : 0;
  if (list->len > 0) {
    snapshot->before.list.data = xmalloc(sizeof(IrisVal *) * list->len);
    for (size_t i = 0; i < list->len; i++) {
      IrisVal *val = list->data[i];
      snapshot->before.list.data[i] = val;
      if (val && !iris_list_is_chunk_ptr(list, val))
        iris_retain(val);
    }
  } else {
    snapshot->before.list.data = NULL;
  }
  coll_unlock();
  snapshot->next = iris_tx_top->snapshots;
  iris_tx_top->snapshots = snapshot;
}

static void iris_tx_record_map(IrisMap *map) {
  if (!iris_tx_top || !map ||
      iris_tx_has_snapshot(iris_tx_top, IRIS_TX_MAP, map))
    return;
  IrisTxSnapshot *snapshot = xcalloc(1, sizeof(*snapshot));
  snapshot->kind = IRIS_TX_MAP;
  snapshot->target = map;
  coll_lock();
  snapshot->before.map.n_buckets = map->n_buckets;
  snapshot->before.map.len = map->len;
  snapshot->before.map.buckets =
      xcalloc(map->n_buckets, sizeof(IrisMapEntry *));
  for (size_t i = 0; i < map->n_buckets; i++) {
    IrisMapEntry **tail = &snapshot->before.map.buckets[i];
    for (IrisMapEntry *entry = map->buckets[i]; entry; entry = entry->next) {
      IrisMapEntry *copy = xcalloc(1, sizeof(*copy));
      copy->key = xstrdup(entry->key);
      copy->val = entry->val;
      if (copy->val)
        iris_retain(copy->val);
      *tail = copy;
      tail = &copy->next;
    }
  }
  coll_unlock();
  snapshot->next = iris_tx_top->snapshots;
  iris_tx_top->snapshots = snapshot;
}

static void iris_tx_record_atomic(IrisAtomic *atomic) {
  if (!iris_tx_top || !atomic ||
      iris_tx_has_snapshot(iris_tx_top, IRIS_TX_ATOMIC, atomic))
    return;
  IrisTxSnapshot *snapshot = xcalloc(1, sizeof(*snapshot));
  snapshot->kind = IRIS_TX_ATOMIC;
  snapshot->target = atomic;
  pthread_mutex_lock(&atomic->mu);
  snapshot->before.atomic = atomic->val;
  if (snapshot->before.atomic)
    iris_retain(snapshot->before.atomic);
  pthread_mutex_unlock(&atomic->mu);
  snapshot->next = iris_tx_top->snapshots;
  iris_tx_top->snapshots = snapshot;
}

static void iris_tx_free_files(IrisTxFile *file) {
  while (file) {
    IrisTxFile *next = file->next;
    xfree(file->path);
    xfree(file->contents);
    xfree(file);
    file = next;
  }
}

static void iris_tx_restore_frame(IrisTxFrame *frame) {
  IrisTxSnapshot *snapshot = frame->snapshots;
  while (snapshot) {
    IrisTxSnapshot *next = snapshot->next;
    iris_tx_restore_snapshot(snapshot);
    snapshot = next;
  }
  iris_tx_free_files(frame->files);
  xfree(frame);
}

int64_t iris_transaction_begin(void) {
  IrisTxFrame *frame = xcalloc(1, sizeof(*frame));
  frame->parent = iris_tx_top;
  iris_tx_top = frame;
  return 1;
}

int64_t iris_transaction_depth(void) {
  int64_t depth = 0;
  for (IrisTxFrame *frame = iris_tx_top; frame; frame = frame->parent)
    depth++;
  return depth;
}

int64_t iris_transaction_rollback(void) {
  if (!iris_tx_top)
    return 0;
  IrisTxFrame *frame = iris_tx_top;
  iris_tx_top = frame->parent;
  iris_tx_restore_frame(frame);
  return 1;
}

static const char *iris_tx_staged_read(const char *path) {
  for (IrisTxFrame *frame = iris_tx_top; frame; frame = frame->parent)
    for (IrisTxFile *file = frame->files; file; file = file->next)
      if (strcmp(file->path, path) == 0)
        return file->contents;
  return NULL;
}

static int iris_tx_stage_write(const char *path, const char *contents) {
  if (!iris_tx_top)
    return 0;
  for (IrisTxFile *file = iris_tx_top->files; file; file = file->next) {
    if (strcmp(file->path, path) == 0) {
      xfree(file->contents);
      file->contents = xstrdup(contents);
      return 1;
    }
  }
  IrisTxFile *file = xcalloc(1, sizeof(*file));
  file->path = xstrdup(path);
  file->contents = xstrdup(contents);
  file->next = iris_tx_top->files;
  iris_tx_top->files = file;
  return 1;
}

static int iris_tx_replace_file(const char *temp, const char *destination) {
#if defined(_WIN32)
  return MoveFileExA(temp, destination,
                     MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH) != 0;
#else
  return rename(temp, destination) == 0;
#endif
}

typedef struct IrisTxWalEntry {
  int existed;
  char *dest;
  char *new_path;
  char *bak_path;
} IrisTxWalEntry;

static int iris_tx_flush_and_close(FILE *f) {
  if (!f)
    return 0;
  int ok = fflush(f) == 0;
#if defined(_WIN32)
  int fd = _fileno(f);
  if (fd >= 0 && _commit(fd) != 0)
    ok = 0;
#elif !defined(__wasm__)
  int fd = fileno(f);
  if (fd >= 0)
    (void)fsync(fd);
#endif
  if (fclose(f) != 0)
    ok = 0;
  return ok;
}

static int iris_tx_write_synced(const char *path, const void *data,
                                size_t len) {
  FILE *f = fopen(path, "wb");
  if (!f)
    return 0;
  size_t written = len > 0 ? fwrite(data, 1, len, f) : 0;
  int ok = iris_tx_flush_and_close(f);
  return ok && written == len;
}

static void iris_tx_sync_parent_dir(const char *path) {
#if !defined(_WIN32) && !defined(__wasm__)
  const char *slash = strrchr(path, '/');
  char *dir = NULL;
  if (slash) {
    size_t len = (size_t)(slash - path);
    if (len == 0) {
      dir = xstrdup("/");
    } else {
      dir = xmalloc(len + 1);
      memcpy(dir, path, len);
      dir[len] = '\0';
    }
  } else {
    dir = xstrdup(".");
  }
  int dfd = open(dir, O_RDONLY);
  if (dfd >= 0) {
    (void)fsync(dfd);
    close(dfd);
  }
  free(dir);
#else
  (void)path;
#endif
}

static int iris_tx_path_state(const char *path, int *out_exists,
                              int *out_is_dir) {
#if defined(_WIN32)
  DWORD attr = GetFileAttributesA(path);
  if (attr == INVALID_FILE_ATTRIBUTES) {
    DWORD err = GetLastError();
    if (err == ERROR_FILE_NOT_FOUND || err == ERROR_PATH_NOT_FOUND) {
      *out_exists = 0;
      *out_is_dir = 0;
      return 1;
    }
    return 0;
  }
  *out_exists = 1;
  *out_is_dir = (attr & FILE_ATTRIBUTE_DIRECTORY) != 0 ? 1 : 0;
  return 1;
#else
  struct stat st;
#if defined(__wasm__)
  if (stat(path, &st) != 0) {
#else
  if (lstat(path, &st) != 0) {
#endif
    if (errno == ENOENT) {
      *out_exists = 0;
      *out_is_dir = 0;
      return 1;
    }
    return 0;
  }
  *out_exists = 1;
  *out_is_dir = S_ISDIR(st.st_mode) ? 1 : 0;
  return 1;
#endif
}

static int iris_tx_copy_synced(const char *src, const char *dst) {
  FILE *in = fopen(src, "rb");
  if (!in)
    return 0;
  FILE *out = fopen(dst, "wb");
  if (!out) {
    fclose(in);
    return 0;
  }
  char buf[8192];
  int ok = 1;
  for (;;) {
    size_t n = fread(buf, 1, sizeof(buf), in);
    if (n > 0) {
      if (fwrite(buf, 1, n, out) != n) {
        ok = 0;
        break;
      }
    }
    if (n < sizeof(buf)) {
      if (ferror(in))
        ok = 0;
      break;
    }
  }
  fclose(in);
  if (!iris_tx_flush_and_close(out))
    ok = 0;
  if (!ok)
    remove(dst);
  return ok;
}

static void iris_tx_split_dir_base(const char *path, char **out_dir_prefix,
                                   const char **out_base) {
  const char *s1 = strrchr(path, '/');
  const char *s2 = strrchr(path, '\\');
  const char *sep = (!s1 || (s2 && s2 > s1)) ? s2 : s1;
  if (sep) {
    size_t plen = (size_t)(sep - path + 1);
    *out_dir_prefix = xmalloc(plen + 1);
    memcpy(*out_dir_prefix, path, plen);
    (*out_dir_prefix)[plen] = '\0';
    *out_base = sep + 1;
  } else {
    *out_dir_prefix = xstrdup("");
    *out_base = path;
  }
}

static char *iris_tx_sibling_path(const char *dest, const char *tag,
                                  unsigned long pid, uint64_t txid,
                                  size_t idx) {
  char *dir_prefix = NULL;
  const char *base = NULL;
  iris_tx_split_dir_base(dest, &dir_prefix, &base);
  size_t cap = strlen(dir_prefix) + strlen(base) + 96;
  char *out = xmalloc(cap);
  snprintf(out, cap, "%s.%s.iris-txn-%s-%lu-%" PRIu64 "-%zu", dir_prefix, base,
           tag, pid, txid, idx);
  free(dir_prefix);
  return out;
}

static char *iris_tx_wal_path_for(const char *first_dest, unsigned long pid,
                                  uint64_t txid) {
  char *dir_prefix = NULL;
  const char *base = NULL;
  iris_tx_split_dir_base(first_dest, &dir_prefix, &base);
  size_t cap = strlen(dir_prefix) + 80;
  char *out = xmalloc(cap);
  snprintf(out, cap, "%s.iris-txn-%lu-%" PRIu64 ".wal", dir_prefix, pid, txid);
  free(dir_prefix);
  return out;
}

static char *iris_tx_hex_encode(const char *s) {
  static const char HEX[] = "0123456789abcdef";
  size_t len = strlen(s);
  char *out = xmalloc(len * 2 + 1);
  for (size_t i = 0; i < len; i++) {
    unsigned char c = (unsigned char)s[i];
    out[i * 2] = HEX[(c >> 4) & 0x0f];
    out[i * 2 + 1] = HEX[c & 0x0f];
  }
  out[len * 2] = '\0';
  return out;
}

static int iris_tx_hex_val(char c) {
  if (c >= '0' && c <= '9')
    return c - '0';
  if (c >= 'a' && c <= 'f')
    return 10 + (c - 'a');
  if (c >= 'A' && c <= 'F')
    return 10 + (c - 'A');
  return -1;
}

static char *iris_tx_hex_decode(const char *hex, size_t len) {
  if (len % 2 != 0)
    return NULL;
  size_t out_len = len / 2;
  char *out = xmalloc(out_len + 1);
  for (size_t i = 0; i < out_len; i++) {
    int hi = iris_tx_hex_val(hex[i * 2]);
    int lo = iris_tx_hex_val(hex[i * 2 + 1]);
    if (hi < 0 || lo < 0) {
      free(out);
      return NULL;
    }
    out[i] = (char)((hi << 4) | lo);
  }
  out[out_len] = '\0';
  return out;
}

static int iris_tx_write_wal(const char *wal_path, const char *state,
                             const IrisTxWalEntry *entries, size_t count) {
  size_t cap = 256;
  for (size_t i = 0; i < count; i++) {
    cap += (strlen(entries[i].dest) + strlen(entries[i].new_path) +
            strlen(entries[i].bak_path)) *
               2 +
           32;
  }
  char *buf = xmalloc(cap);
  int n =
      snprintf(buf, cap, "IRIS_TX_WAL/1\nstate=%s\ncount=%zu\n", state, count);
  if (n < 0 || (size_t)n >= cap) {
    free(buf);
    return 0;
  }
  size_t pos = (size_t)n;
  for (size_t i = 0; i < count; i++) {
    char *h_dest = iris_tx_hex_encode(entries[i].dest);
    char *h_new = iris_tx_hex_encode(entries[i].new_path);
    char *h_bak = iris_tx_hex_encode(entries[i].bak_path);
    int wrote = snprintf(buf + pos, cap - pos, "entry=%d|%s|%s|%s\n",
                         entries[i].existed ? 1 : 0, h_dest, h_new, h_bak);
    free(h_dest);
    free(h_new);
    free(h_bak);
    if (wrote < 0 || pos + (size_t)wrote >= cap) {
      free(buf);
      return 0;
    }
    pos += (size_t)wrote;
  }
  size_t tmp_cap = strlen(wal_path) + 8;
  char *tmp_path = xmalloc(tmp_cap);
  snprintf(tmp_path, tmp_cap, "%s.tmp", wal_path);
  int ok = iris_tx_write_synced(tmp_path, buf, pos) &&
           iris_tx_replace_file(tmp_path, wal_path);
  if (!ok)
    remove(tmp_path);
  iris_tx_sync_parent_dir(wal_path);
  free(tmp_path);
  free(buf);
  return ok;
}

static void iris_tx_rollback_entries(const IrisTxWalEntry *entries,
                                     size_t count) {
  for (size_t i = count; i > 0; i--) {
    const IrisTxWalEntry *e = &entries[i - 1];
    if (e->existed) {
      int exists = 0, is_dir = 0;
      if (iris_tx_path_state(e->bak_path, &exists, &is_dir) && exists &&
          !is_dir) {
        (void)iris_tx_replace_file(e->bak_path, e->dest);
        iris_tx_sync_parent_dir(e->dest);
      }
    } else {
      remove(e->dest);
      iris_tx_sync_parent_dir(e->dest);
    }
  }
}

static void iris_tx_free_wal_entries(IrisTxWalEntry *entries, size_t count) {
  if (!entries)
    return;
  for (size_t i = 0; i < count; i++) {
    free(entries[i].dest);
    free(entries[i].new_path);
    free(entries[i].bak_path);
  }
  free(entries);
}

static void iris_tx_cleanup_wal_artifacts(const char *wal_path,
                                          IrisTxWalEntry *entries,
                                          size_t count) {
  for (size_t i = 0; i < count; i++) {
    if (entries[i].new_path)
      remove(entries[i].new_path);
    if (entries[i].bak_path)
      remove(entries[i].bak_path);
  }
  if (wal_path) {
    remove(wal_path);
    size_t tmp_cap = strlen(wal_path) + 8;
    char *tmp_path = xmalloc(tmp_cap);
    snprintf(tmp_path, tmp_cap, "%s.tmp", wal_path);
    remove(tmp_path);
    free(tmp_path);
    iris_tx_sync_parent_dir(wal_path);
  }
  iris_tx_free_wal_entries(entries, count);
}

static void iris_tx_discard_frame_snapshots(IrisTxFrame *frame) {
  while (frame->snapshots) {
    IrisTxSnapshot *next = frame->snapshots->next;
    iris_tx_discard_snapshot(frame->snapshots);
    frame->snapshots = next;
  }
  iris_tx_free_files(frame->files);
  free(frame);
}

int64_t iris_transaction_commit(void) {
  if (!iris_tx_top)
    return 0;
  IrisTxFrame *frame = iris_tx_top;
  iris_tx_top = frame->parent;

  if (iris_tx_top) {
    IrisTxSnapshot *snapshot = frame->snapshots;
    while (snapshot) {
      IrisTxSnapshot *next = snapshot->next;
      if (iris_tx_has_snapshot(iris_tx_top, snapshot->kind, snapshot->target)) {
        iris_tx_discard_snapshot(snapshot);
      } else {
        snapshot->next = iris_tx_top->snapshots;
        iris_tx_top->snapshots = snapshot;
      }
      snapshot = next;
    }
    for (IrisTxFile *file = frame->files; file; file = file->next)
      iris_tx_stage_write(file->path, file->contents);
    iris_tx_free_files(frame->files);
    free(frame);
    return 1;
  }

  size_t count = 0;
  for (IrisTxFile *file = frame->files; file; file = file->next)
    count++;
  if (count == 0) {
    iris_tx_discard_frame_snapshots(frame);
    return 1;
  }

  // Phase 0: Enforce sandbox security & path validity before creating any
  // files.
  IrisTxFile **ordered = xcalloc(count, sizeof(IrisTxFile *));
  size_t fill = 0;
  for (IrisTxFile *file = frame->files; file; file = file->next) {
    ordered[fill++] = file;
    int64_t len = file->contents ? (int64_t)strlen(file->contents) : 0;
    iris_sandbox_require_fs_write_bytes(file->path, len);
  }

#ifdef _WIN32
  unsigned long pid = (unsigned long)GetCurrentProcessId();
#else
  unsigned long pid = (unsigned long)getpid();
#endif
  uint64_t txid = ++iris_tx_temp_counter;

  IrisTxWalEntry *entries = xcalloc(count, sizeof(IrisTxWalEntry));
  for (size_t i = 0; i < count; i++) {
    IrisTxFile *file = ordered[i];
    char *dir_prefix = NULL;
    const char *base = NULL;
    iris_tx_split_dir_base(file->path, &dir_prefix, &base);
    int has_name = base && *base != '\0';
    free(dir_prefix);

    int exists = 0, is_dir = 0;
    if (!has_name || !iris_tx_path_state(file->path, &exists, &is_dir) ||
        is_dir) {
      free(ordered);
      iris_tx_cleanup_wal_artifacts(NULL, entries, count);
      iris_tx_restore_frame(frame);
      return 0;
    }
    entries[i].dest = xstrdup(file->path);
    entries[i].new_path = iris_tx_sibling_path(file->path, "new", pid, txid, i);
    entries[i].bak_path = iris_tx_sibling_path(file->path, "bak", pid, txid, i);
    entries[i].existed = exists;
  }

  // Phase 1: Stage and sync all `.new` temporary files.
  for (size_t i = 0; i < count; i++) {
    size_t len = ordered[i]->contents ? strlen(ordered[i]->contents) : 0;
    if (!iris_tx_write_synced(entries[i].new_path,
                              ordered[i]->contents ? ordered[i]->contents : "",
                              len)) {
      free(ordered);
      iris_tx_cleanup_wal_artifacts(NULL, entries, count);
      iris_tx_restore_frame(frame);
      return 0;
    }
  }
  free(ordered);

  // Phase 2: Snapshot pre-existing originals into synced `.bak` copies.
  for (size_t i = 0; i < count; i++) {
    if (entries[i].existed) {
      if (!iris_tx_copy_synced(entries[i].dest, entries[i].bak_path)) {
        iris_tx_cleanup_wal_artifacts(NULL, entries, count);
        iris_tx_restore_frame(frame);
        return 0;
      }
    }
  }

  // Phase 3 & 4: Persist `PREPARED` WAL, then transition to `COMMITTING`.
  char *wal_path = iris_tx_wal_path_for(entries[0].dest, pid, txid);
  if (!iris_tx_write_wal(wal_path, "PREPARED", entries, count) ||
      !iris_tx_write_wal(wal_path, "COMMITTING", entries, count)) {
    iris_tx_cleanup_wal_artifacts(wal_path, entries, count);
    free(wal_path);
    iris_tx_restore_frame(frame);
    return 0;
  }

  // Phase 5: Replace destinations; roll back all already-replaced files on
  // error.
  for (size_t i = 0; i < count; i++) {
    if (!iris_tx_replace_file(entries[i].new_path, entries[i].dest)) {
      iris_tx_rollback_entries(entries, i);
      iris_tx_cleanup_wal_artifacts(wal_path, entries, count);
      free(wal_path);
      iris_tx_restore_frame(frame);
      return 0;
    }
    iris_tx_sync_parent_dir(entries[i].dest);
  }

  // Phase 6: Finalize `COMMITTED` WAL before deleting backups/WAL.
  if (!iris_tx_write_wal(wal_path, "COMMITTED", entries, count)) {
    iris_tx_rollback_entries(entries, count);
    iris_tx_cleanup_wal_artifacts(wal_path, entries, count);
    free(wal_path);
    iris_tx_restore_frame(frame);
    return 0;
  }
  iris_tx_cleanup_wal_artifacts(wal_path, entries, count);
  free(wal_path);

  iris_tx_discard_frame_snapshots(frame);
  return 1;
}

static int iris_tx_recover_single_wal(const char *wal_path) {
  FILE *f = fopen(wal_path, "rb");
  if (!f)
    return 0;
  fseek(f, 0, SEEK_END);
  long fsz = ftell(f);
  fseek(f, 0, SEEK_SET);
  if (fsz <= 0 || fsz > 16 * 1024 * 1024) {
    fclose(f);
    return 0;
  }
  char *text = xmalloc((size_t)fsz + 1);
  size_t got = fread(text, 1, (size_t)fsz, f);
  fclose(f);
  text[got] = '\0';

  char *cursor = text;
  char *line = cursor;
  char *nl = strchr(cursor, '\n');
  if (!nl) {
    free(text);
    return 0;
  }
  *nl = '\0';
  size_t l0 = strlen(line);
  if (l0 > 0 && line[l0 - 1] == '\r')
    line[l0 - 1] = '\0';
  if (strcmp(line, "IRIS_TX_WAL/1") != 0) {
    free(text);
    return 0;
  }

  cursor = nl + 1;
  nl = strchr(cursor, '\n');
  if (!nl) {
    free(text);
    return 0;
  }
  *nl = '\0';
  size_t l1 = strlen(cursor);
  if (l1 > 0 && cursor[l1 - 1] == '\r')
    cursor[l1 - 1] = '\0';
  if (strncmp(cursor, "state=", 6) != 0) {
    free(text);
    return 0;
  }
  char *state = xstrdup(cursor + 6);

  cursor = nl + 1;
  nl = strchr(cursor, '\n');
  if (!nl) {
    free(state);
    free(text);
    return 0;
  }
  *nl = '\0';
  if (strncmp(cursor, "count=", 6) != 0) {
    free(state);
    free(text);
    return 0;
  }
  unsigned long long count_ll = strtoull(cursor + 6, NULL, 10);
  size_t count = (size_t)count_ll;
  if (count == 0 || count > 65536) {
    free(state);
    free(text);
    return 0;
  }

  IrisTxWalEntry *entries = xcalloc(count, sizeof(IrisTxWalEntry));
  size_t idx = 0;
  cursor = nl + 1;
  while (*cursor && idx < count) {
    char *next = strchr(cursor, '\n');
    if (next)
      *next = '\0';
    size_t elen = strlen(cursor);
    if (elen > 0 && cursor[elen - 1] == '\r')
      cursor[--elen] = '\0';
    if (*cursor == '\0') {
      if (!next)
        break;
      cursor = next + 1;
      continue;
    }
    if (strncmp(cursor, "entry=", 6) != 0)
      break;
    const char *p = cursor + 6;
    const char *sep1 = strchr(p, '|');
    const char *sep2 = sep1 ? strchr(sep1 + 1, '|') : NULL;
    const char *sep3 = sep2 ? strchr(sep2 + 1, '|') : NULL;
    if (!sep1 || !sep2 || !sep3 || strchr(sep3 + 1, '|') != NULL)
      break;
    if ((size_t)(sep1 - p) != 1 || (p[0] != '0' && p[0] != '1'))
      break;
    entries[idx].existed = (p[0] == '1');
    entries[idx].dest =
        iris_tx_hex_decode(sep1 + 1, (size_t)(sep2 - (sep1 + 1)));
    entries[idx].new_path =
        iris_tx_hex_decode(sep2 + 1, (size_t)(sep3 - (sep2 + 1)));
    entries[idx].bak_path = iris_tx_hex_decode(sep3 + 1, strlen(sep3 + 1));
    if (!entries[idx].dest || !entries[idx].new_path || !entries[idx].bak_path)
      break;
    iris_sandbox_require_fs_write(entries[idx].dest);
    idx++;
    if (!next)
      break;
    cursor = next + 1;
  }
  free(text);
  if (idx != count ||
      (strcmp(state, "PREPARED") != 0 && strcmp(state, "COMMITTING") != 0 &&
       strcmp(state, "COMMITTED") != 0)) {
    iris_tx_free_wal_entries(entries, count);
    free(state);
    return 0;
  }
  if (strcmp(state, "COMMITTING") == 0) {
    iris_tx_rollback_entries(entries, count);
  }
  free(state);
  iris_tx_cleanup_wal_artifacts(wal_path, entries, count);
  return 1;
}

static int iris_tx_is_wal_filename(const char *name) {
  size_t len = strlen(name);
  return len > 14 && strncmp(name, ".iris-txn-", 10) == 0 &&
         strcmp(name + len - 4, ".wal") == 0;
}

int64_t iris_transaction_recover(const char *dir) {
  const char *target = (dir && *dir) ? dir : ".";
  iris_sandbox_require_fs_write(target);
  int exists = 0, is_dir = 0;
  if (!iris_tx_path_state(target, &exists, &is_dir))
    return -1;
  if (!exists)
    return 0;
  if (!is_dir)
    return -1;

  int64_t recovered = 0;
  size_t tlen = strlen(target);
#if defined(_WIN32)
  size_t pat_cap = tlen + 24;
  char *pattern = xmalloc(pat_cap);
  int trailing =
      (tlen > 0 && (target[tlen - 1] == '/' || target[tlen - 1] == '\\'));
  snprintf(pattern, pat_cap, "%s%s.iris-txn-*.wal", target,
           trailing ? "" : "\\");
  WIN32_FIND_DATAA ffd;
  HANDLE h = FindFirstFileA(pattern, &ffd);
  free(pattern);
  if (h != INVALID_HANDLE_VALUE) {
    do {
      if ((ffd.dwFileAttributes & FILE_ATTRIBUTE_DIRECTORY) == 0 &&
          iris_tx_is_wal_filename(ffd.cFileName)) {
        size_t wcap = tlen + strlen(ffd.cFileName) + 4;
        char *wal_path = xmalloc(wcap);
        snprintf(wal_path, wcap, "%s%s%s", target, trailing ? "" : "\\",
                 ffd.cFileName);
        if (iris_tx_recover_single_wal(wal_path))
          recovered++;
        free(wal_path);
      }
    } while (FindNextFileA(h, &ffd));
    FindClose(h);
  }
#else
  DIR *d = opendir(target);
  if (!d)
    return -1;
  int trailing = (tlen > 0 && target[tlen - 1] == '/');
  struct dirent *ent;
  while ((ent = readdir(d)) != NULL) {
    if (iris_tx_is_wal_filename(ent->d_name)) {
      size_t wcap = tlen + strlen(ent->d_name) + 4;
      char *wal_path = xmalloc(wcap);
      snprintf(wal_path, wcap, "%s%s%s", target, trailing ? "" : "/",
               ent->d_name);
      if (iris_tx_recover_single_wal(wal_path))
        recovered++;
      free(wal_path);
    }
  }
  closedir(d);
#endif
  return recovered;
}

IrisList *iris_list_new(void) {
  IrisList *l = xcalloc(1, sizeof(IrisList));
  l->cap = 8;
  l->data = xmalloc(sizeof(IrisVal *) * l->cap);
  return l;
}
static int iris_tag_is_scalar(int32_t tag) {
  return tag == IRIS_TAG_I64 || tag == IRIS_TAG_I32 || tag == IRIS_TAG_F64 ||
         tag == IRIS_TAG_F32 || tag == IRIS_TAG_BOOL || tag == IRIS_TAG_UNIT;
}

/* A slot in `l`'s own chunk arena. Starts at 16 entries and grows 4x up to
 * 1024 so tiny lists (e.g. 2-4 floats) do not pin 16 KB chunks. */
static IrisVal *iris_list_arena_slot(IrisList *l) {
  if (!l->chunks || l->chunks->count >= l->chunks->cap) {
    size_t next_cap = l->chunks ? l->chunks->cap * 4 : 16;
    if (next_cap > 1024)
      next_cap = 1024;
    IrisValChunk *c = (IrisValChunk *)xmalloc(sizeof(IrisValChunk) +
                                              sizeof(IrisVal) * next_cap);
    c->count = 0;
    c->cap = next_cap;
    c->next = l->chunks;
    l->chunks = c;
  }
  return &l->chunks->items[l->chunks->count++];
}

/* A scalar that lives inside another list's arena (`push_i64` stores values
 * there) is not an object of its own: copy it into this list's arena. Sharing
 * the pointer made list_slice, list_concat and every other copy hold
 * pointers into memory the source list frees, and retaining them made them
 * look separately owned. */
static int iris_needs_arena_copy(const IrisVal *val) {
  return !iris_is_immediate_tag(val) && iris_tag_is_scalar(val->tag) &&
         !ds_contains((void *)val);
}

void iris_list_push(IrisList *l, IrisVal *val) {
  iris_tx_record_list(l);
  if (iris_needs_arena_copy(val)) {
    if (iris_tx_top)
      coll_lock();
    if (l->len == l->cap) {
      l->cap = l->cap ? l->cap * 2 : 8;
      l->data = xrealloc(l->data, sizeof(IrisVal *) * l->cap);
    }
    IrisVal *slot = iris_list_arena_slot(l);
    *slot = *val;
    l->data[l->len++] = slot;
    if (iris_tx_top)
      coll_unlock();
    return;
  }
  if (!iris_tx_top) {
    if (l->len == l->cap) {
      l->cap = l->cap ? l->cap * 2 : 8;
      l->data = xrealloc(l->data, sizeof(IrisVal *) * l->cap);
    }
    if (val)
      iris_retain(val);
    l->data[l->len++] = val;
    return;
  }
  coll_lock();
  if (l->len == l->cap) {
    l->cap = l->cap ? l->cap * 2 : 8;
    l->data = xrealloc(l->data, sizeof(IrisVal *) * l->cap);
  }
  if (val)
    iris_retain(val);
  l->data[l->len++] = val;
  coll_unlock();
}
void iris_list_push_i64(IrisList *l, int64_t val) {
  iris_tx_record_list(l);
  if (l->len == l->cap) {
    l->cap = l->cap ? l->cap * 2 : 8;
    l->data = xrealloc(l->data, sizeof(IrisVal *) * l->cap);
  }
  IrisVal *r = iris_list_arena_slot(l);
  r->tag = IRIS_TAG_I64;
  r->i64 = val;
  l->data[l->len++] = r;
}
void iris_list_push_f64(IrisList *l, double val) {
  iris_tx_record_list(l);
  if (l->len == l->cap) {
    l->cap = l->cap ? l->cap * 2 : 8;
    l->data = xrealloc(l->data, sizeof(IrisVal *) * l->cap);
  }
  IrisVal *r = iris_list_arena_slot(l);
  r->tag = IRIS_TAG_F64;
  r->f64 = val;
  l->data[l->len++] = r;
}
int64_t iris_list_len(IrisList *l) { return (int64_t)l->len; }
IrisVal *iris_list_get(IrisList *l, int64_t idx) {
  if (!l || idx < 0 || (size_t)idx >= l->len) {
    iris_runtime_error("E0403",
                       "index %" PRId64 " is out of bounds for length %zu",
                       (int64_t)idx, l ? l->len : (size_t)0);
    iris_fatal();
  }
  return l->data[idx];
}
int64_t iris_list_get_i64(IrisList *l, int64_t idx) {
  if (l && idx >= 0 && (size_t)idx < l->len) {
    IrisVal *v = l->data[idx];
    if (v && v->tag == IRIS_TAG_I64)
      return v->i64;
  }
  return iris_unbox_i64(iris_list_get(l, idx));
}
double iris_list_get_f64(IrisList *l, int64_t idx) {
  if (l && idx >= 0 && (size_t)idx < l->len) {
    IrisVal *v = l->data[idx];
    if (v && v->tag == IRIS_TAG_F64)
      return v->f64;
  }
  return iris_unbox_f64(iris_list_get(l, idx));
}
void iris_list_set(IrisList *l, int64_t idx, IrisVal *val) {
  iris_tx_record_list(l);
  if (!iris_tx_top && l && idx >= 0 && (size_t)idx < l->len && val &&
      l->data[idx]) {
    IrisVal *old = l->data[idx];
    if (old->tag == val->tag) {
      if (val->tag == IRIS_TAG_I64 || val->tag == IRIS_TAG_F64 ||
          val->tag == IRIS_TAG_I32 || val->tag == IRIS_TAG_F32 ||
          val->tag == IRIS_TAG_BOOL) {
        old->i64 = val->i64;
        /* Only a temporary box of its own: a value inside another
         * list's arena is not this function's to free. */
        if (ds_contains(val))
          xfree(val);
        return;
      }
    }
  }
  coll_lock();
  if (idx < 0 || (size_t)idx >= l->len) {
    coll_unlock();
    iris_runtime_error("E0403",
                       "index %" PRId64 " is out of bounds for length %zu",
                       (int64_t)idx, l ? l->len : (size_t)0);
    iris_fatal();
  }
  if (val)
    iris_retain(val);
  if (l->data[idx] && !iris_list_is_chunk_ptr(l, l->data[idx]))
    iris_release(l->data[idx]);
  l->data[idx] = val;
  coll_unlock();
}
void iris_list_set_i64(IrisList *l, int64_t idx, int64_t val) {
  if (!iris_tx_top && l && idx >= 0 && (size_t)idx < l->len) {
    IrisVal *v = l->data[idx];
    if (v && v->tag == IRIS_TAG_I64) {
      v->i64 = val;
      return;
    }
  }
  iris_list_set(l, idx, iris_box_i64(val));
}
void iris_list_set_f64(IrisList *l, int64_t idx, double val) {
  if (!iris_tx_top && l && idx >= 0 && (size_t)idx < l->len) {
    IrisVal *v = l->data[idx];
    if (v && v->tag == IRIS_TAG_F64) {
      v->f64 = val;
      return;
    }
  }
  iris_list_set(l, idx, iris_box_f64(val));
}
IrisVal *iris_list_pop(IrisList *l) {
  iris_tx_record_list(l);
  coll_lock();
  if (l->len == 0) {
    coll_unlock();
    fprintf(stderr, "iris: pop on empty list\n");
    iris_fatal();
  }
  IrisVal *v = l->data[--l->len];
  coll_unlock();
  return v;
}

// ---------------------------------------------------------------------------
// Map (separate-chaining hash map, string keys)
// ---------------------------------------------------------------------------

#define MAP_INIT_BUCKETS 16u

static size_t hash_str(const char *s) {
  size_t h = 5381;
  while (*s)
    h = h * 33u + (unsigned char)*s++;
  return h;
}

IrisMap *iris_map_new(void) {
  IrisMap *m = xcalloc(1, sizeof(IrisMap));
  m->n_buckets = MAP_INIT_BUCKETS;
  m->buckets = xcalloc(m->n_buckets, sizeof(IrisMapEntry *));
  return m;
}
void iris_map_set(IrisMap *m, IrisVal *key, IrisVal *val) {
  /* Stringify the key BEFORE taking the lock: `iris_value_to_str` walks a
   * list value and would re-enter `iris_list_get`, deadlocking on a
   * non-recursive mutex. */
  char *key_str = iris_value_to_str(key);
  iris_tx_record_map(m);
  coll_lock();
  size_t h = hash_str(key_str) % m->n_buckets;
  for (IrisMapEntry *e = m->buckets[h]; e; e = e->next) {
    if (strcmp(e->key, key_str) == 0) {
      if (val)
        iris_retain(val);
      if (e->val)
        iris_release(e->val);
      e->val = val;
      xfree(key_str);
      coll_unlock();
      return;
    }
  }
  IrisMapEntry *e = xmalloc(sizeof(IrisMapEntry));
  e->key = key_str;
  e->val = val;
  if (val)
    iris_retain(val);
  e->next = m->buckets[h];
  m->buckets[h] = e;
  m->len++;
  coll_unlock();
}
IrisOption *iris_map_get(IrisMap *m, IrisVal *key) {
  char *key_str = iris_value_to_str(key);
  size_t h = hash_str(key_str) % m->n_buckets;
  for (IrisMapEntry *e = m->buckets[h]; e; e = e->next)
    if (strcmp(e->key, key_str) == 0) {
      xfree(key_str);
      return iris_make_some(e->val);
    }
  xfree(key_str);
  return iris_make_none();
}
int iris_map_contains(IrisMap *m, IrisVal *key) {
  char *key_str = iris_value_to_str(key);
  size_t h = hash_str(key_str) % m->n_buckets;
  for (IrisMapEntry *e = m->buckets[h]; e; e = e->next)
    if (strcmp(e->key, key_str) == 0) {
      xfree(key_str);
      return 1;
    }
  xfree(key_str);
  return 0;
}
void iris_map_remove(IrisMap *m, IrisVal *key) {
  char *key_str = iris_value_to_str(key); /* see iris_map_set */
  iris_tx_record_map(m);
  coll_lock();
  size_t h = hash_str(key_str) % m->n_buckets;
  IrisMapEntry **pp = &m->buckets[h];
  while (*pp) {
    if (strcmp((*pp)->key, key_str) == 0) {
      IrisMapEntry *doomed = *pp;
      *pp = doomed->next;
      if (doomed->val)
        iris_release(doomed->val);
      xfree(doomed->key);
      xfree(doomed);
      m->len--;
      xfree(key_str);
      coll_unlock();
      return;
    }
    pp = &(*pp)->next;
  }
  xfree(key_str);
  coll_unlock();
}
int64_t iris_map_len(IrisMap *m) { return (int64_t)m->len; }

// ---------------------------------------------------------------------------
// Extended list operations
// ---------------------------------------------------------------------------

static int iris_val_equal(IrisVal *a, IrisVal *b);
int iris_enum_eq(IrisVal *a, IrisVal *b);

int iris_list_eq(IrisList *a, IrisList *b) {
  if (a == b)
    return 1;
  if (!a || !b)
    return 0;
  if (a->len != b->len)
    return 0;
  for (size_t i = 0; i < a->len; i++) {
    if (!iris_val_equal(a->data[i], b->data[i]))
      return 0;
  }
  return 1;
}

int iris_option_eq(IrisOption *a, IrisOption *b) {
  if (a == b)
    return 1;
  if (!a || !b)
    return 0;
  if (!a->has_value && !b->has_value)
    return 1;
  if (a->has_value != b->has_value)
    return 0;
  return iris_val_equal(a->value, b->value);
}

int iris_map_eq(IrisMap *a, IrisMap *b) {
  if (a == b)
    return 1;
  if (!a || !b)
    return 0;
  if (a->len != b->len)
    return 0;
  for (size_t i = 0; i < a->n_buckets; i++) {
    IrisMapEntry *cur = a->buckets[i];
    while (cur) {
      IrisOption *opt = iris_map_get(b, (IrisVal *)cur->key);
      if (!opt || !opt->has_value)
        return 0;
      if (!iris_val_equal(cur->val, opt->value))
        return 0;
      cur = cur->next;
    }
  }
  return 1;
}

/* Extract (tag, IrisEnum*) from any of IRIS's hybrid enum representations:
 * immediate pointer-encoded tag (<= 0xFFFF), boxed i64/i32 from containers,
 * or heap-allocated IRIS_TAG_ENUM carrying payload fields. */
static int iris_unpack_enum(IrisVal *v, int64_t *tag_out, IrisEnum **enum_out) {
  if (iris_is_immediate_tag(v)) {
    *tag_out = (int64_t)(uintptr_t)v;
    *enum_out = NULL;
    return 1;
  }
  if (v->tag == IRIS_TAG_ENUM) {
    IrisEnum *e = (IrisEnum *)v->ptr;
    *tag_out = e ? e->tag : 0;
    *enum_out = e;
    return 1;
  }
  if (v->tag == IRIS_TAG_I64) {
    *tag_out = v->i64;
    *enum_out = NULL;
    return 1;
  }
  if (v->tag == IRIS_TAG_I32) {
    *tag_out = (int64_t)v->i32;
    *enum_out = NULL;
    return 1;
  }
  return 0;
}

int iris_enum_eq(IrisVal *a, IrisVal *b) {
  if (a == b)
    return 1;
  int64_t tag_a = 0, tag_b = 0;
  IrisEnum *enum_a = NULL;
  IrisEnum *enum_b = NULL;
  if (!iris_unpack_enum(a, &tag_a, &enum_a) ||
      !iris_unpack_enum(b, &tag_b, &enum_b)) {
    return 0;
  }
  if (tag_a != tag_b)
    return 0;
  size_t len_a = enum_a ? enum_a->len : 0;
  size_t len_b = enum_b ? enum_b->len : 0;
  if (len_a != len_b)
    return 0;
  for (size_t i = 0; i < len_a; i++) {
    if (!iris_val_equal(enum_a->fields[i], enum_b->fields[i]))
      return 0;
  }
  return 1;
}

static int iris_val_equal(IrisVal *a, IrisVal *b) {
  if (a == b)
    return 1;
  if (iris_is_immediate_tag(a) || iris_is_immediate_tag(b)) {
    return iris_enum_eq(a, b);
  }
  if (a->tag != b->tag) {
    if ((a->tag == IRIS_TAG_ENUM &&
         (b->tag == IRIS_TAG_I64 || b->tag == IRIS_TAG_I32)) ||
        (b->tag == IRIS_TAG_ENUM &&
         (a->tag == IRIS_TAG_I64 || a->tag == IRIS_TAG_I32))) {
      return iris_enum_eq(a, b);
    }
    return 0;
  }
  switch (a->tag) {
  case IRIS_TAG_I64:
    return a->i64 == b->i64;
  case IRIS_TAG_I32:
    return a->i32 == b->i32;
  case IRIS_TAG_F64:
    return a->f64 == b->f64;
  case IRIS_TAG_F32:
    return a->f32 == b->f32;
  case IRIS_TAG_BOOL:
    return a->boolean == b->boolean;
  case IRIS_TAG_STR:
    return (a->str && b->str && strcmp(a->str, b->str) == 0);
  case IRIS_TAG_LIST:
    return iris_list_eq((IrisList *)a->ptr, (IrisList *)b->ptr);
  case IRIS_TAG_MAP:
    return iris_map_eq((IrisMap *)a->ptr, (IrisMap *)b->ptr);
  case IRIS_TAG_OPTION:
    return iris_option_eq((IrisOption *)a->ptr, (IrisOption *)b->ptr);
  case IRIS_TAG_ENUM:
    return iris_enum_eq(a, b);
  default:
    return a->ptr == b->ptr;
  }
}

int iris_list_contains(IrisList *l, IrisVal *val) {
  if (!l || !val)
    return 0;
  for (size_t i = 0; i < l->len; i++) {
    if (iris_val_equal(l->data[i], val))
      return 1;
  }
  return 0;
}

static int iris_val_compare(IrisVal *a, IrisVal *b) {
  if (!a && !b)
    return 0;
  if (!a)
    return -1;
  if (!b)
    return 1;
  if (a->tag != b->tag)
    return (int)a->tag - (int)b->tag;
  switch (a->tag) {
  case IRIS_TAG_I64:
    return (a->i64 > b->i64) ? 1 : (a->i64 < b->i64 ? -1 : 0);
  case IRIS_TAG_I32:
    return (a->i32 > b->i32) ? 1 : (a->i32 < b->i32 ? -1 : 0);
  case IRIS_TAG_F64:
    return (a->f64 > b->f64) ? 1 : (a->f64 < b->f64 ? -1 : 0);
  case IRIS_TAG_F32:
    return (a->f32 > b->f32) ? 1 : (a->f32 < b->f32 ? -1 : 0);
  case IRIS_TAG_BOOL:
    return (int)a->boolean - (int)b->boolean;
  case IRIS_TAG_STR:
    if (!a->str && !b->str)
      return 0;
    if (!a->str)
      return -1;
    if (!b->str)
      return 1;
    return strcmp(a->str, b->str);
  default:
    return 0;
  }
}

/* ---- stable merge sort (O(n log n), preserves equal-element order) ---- */
static void iris_merge(IrisVal **arr, IrisVal **tmp, size_t lo, size_t mid,
                       size_t hi) {
  size_t i = lo, j = mid, k = lo;
  while (i < mid && j < hi) {
    if (iris_val_compare(arr[i], arr[j]) <= 0)
      tmp[k++] = arr[i++];
    else
      tmp[k++] = arr[j++];
  }
  while (i < mid)
    tmp[k++] = arr[i++];
  while (j < hi)
    tmp[k++] = arr[j++];
  for (size_t x = lo; x < hi; x++)
    arr[x] = tmp[x];
}

static void iris_merge_sort_rec(IrisVal **arr, IrisVal **tmp, size_t lo,
                                size_t hi) {
  if (hi - lo <= 1)
    return;
  size_t mid = lo + (hi - lo) / 2;
  iris_merge_sort_rec(arr, tmp, lo, mid);
  iris_merge_sort_rec(arr, tmp, mid, hi);
  iris_merge(arr, tmp, lo, mid, hi);
}

void iris_list_sort(IrisList *l) {
  if (!l || l->len <= 1)
    return;
  iris_tx_record_list(l);
  IrisVal **tmp = (IrisVal **)malloc(l->len * sizeof(IrisVal *));
  if (!tmp)
    return; /* OOM — leave list unsorted rather than crash */
  iris_merge_sort_rec(l->data, tmp, 0, l->len);
  free(tmp);
}

IrisList *iris_list_concat(IrisList *a, IrisList *b) {
  IrisList *r = iris_list_new();
  if (a)
    for (size_t i = 0; i < a->len; i++)
      iris_list_push(r, a->data[i]);
  if (b)
    for (size_t i = 0; i < b->len; i++)
      iris_list_push(r, b->data[i]);
  return r;
}

IrisList *iris_list_slice(IrisList *l, int64_t start, int64_t end_idx) {
  IrisList *r = iris_list_new();
  if (!l)
    return r;
  size_t len = l->len;
  if (start < 0)
    start = 0;
  if ((size_t)end_idx > len)
    end_idx = (int64_t)len;
  if (start >= end_idx)
    return r;
  for (int64_t i = start; i < end_idx; i++)
    iris_list_push(r, l->data[(size_t)i]);
  return r;
}

/* An array of strings, records or other heap values (a runtime list) put in
 * a generic slot: a copy, so a later store to the original array is not
 * seen through the slot, as in the interpreter. The raw list pointer used to
 * go in where a box was expected. */
IrisVal *iris_box_array_list(IrisList *l) {
  return iris_box_list(iris_list_slice(l, 0, l ? (int64_t)l->len : 0));
}

// ---------------------------------------------------------------------------
// Extended map operations
// ---------------------------------------------------------------------------

/* A map's entries in key order (bytewise), as the interpreter's sorted map
 * iterates them: listing order must not depend on hashing. The caller frees
 * the returned array (not the entries). */
static int map_entry_cmp(const void *a, const void *b) {
  const IrisMapEntry *x = *(IrisMapEntry *const *)a;
  const IrisMapEntry *y = *(IrisMapEntry *const *)b;
  return strcmp(x->key, y->key);
}
static IrisMapEntry **map_sorted_entries(IrisMap *m, size_t *count) {
  size_t n = 0;
  for (size_t b = 0; b < m->n_buckets; b++)
    for (IrisMapEntry *e = m->buckets[b]; e; e = e->next)
      n++;
  IrisMapEntry **entries =
      (IrisMapEntry **)malloc(sizeof(IrisMapEntry *) * (n ? n : 1));
  size_t i = 0;
  for (size_t b = 0; b < m->n_buckets; b++)
    for (IrisMapEntry *e = m->buckets[b]; e; e = e->next)
      entries[i++] = e;
  qsort(entries, n, sizeof(IrisMapEntry *), map_entry_cmp);
  *count = n;
  return entries;
}

IrisList *iris_map_keys(IrisMap *m) {
  IrisList *r = iris_list_new();
  if (!m)
    return r;
  size_t n;
  IrisMapEntry **entries = map_sorted_entries(m, &n);
  for (size_t i = 0; i < n; i++)
    iris_list_push(r, iris_box_str(entries[i]->key));
  free(entries);
  return r;
}

IrisList *iris_map_values(IrisMap *m) {
  IrisList *r = iris_list_new();
  if (!m)
    return r;
  size_t n;
  IrisMapEntry **entries = map_sorted_entries(m, &n);
  for (size_t i = 0; i < n; i++)
    iris_list_push(r, entries[i]->val);
  free(entries);
  return r;
}

// ---------------------------------------------------------------------------
// File I/O
// ---------------------------------------------------------------------------

/* Every byte of the file at `path`, as a list of i64 (0..255); an empty list
   if it cannot be read. Unlike file_read_all, NUL bytes survive. */
IrisList *iris_file_read_bytes(const char *path) {
  iris_sandbox_require_fs_read(path);
  IrisList *l = iris_list_new();
  FILE *f = path ? fopen(path, "rb") : NULL;
  if (!f)
    return l;
  unsigned char buf[16384];
  size_t n;
  while ((n = fread(buf, 1, sizeof buf, f)) > 0) {
    for (size_t i = 0; i < n; i++)
      iris_list_push_i64(l, (int64_t)buf[i]);
  }
  fclose(f);
  return l;
}

IrisResult *iris_file_read_all(const char *path) {
  iris_sandbox_require_fs_read(path);
  const char *staged = iris_tx_staged_read(path);
  if (staged)
    return iris_make_ok(iris_box_str(staged));
  FILE *f = fopen(path, "rb");
  if (!f) {
    return iris_make_err(iris_box_str("Failed to open file for reading"));
  }
  if (fseek(f, 0, SEEK_END) != 0) {
    fclose(f);
    return iris_make_err(iris_box_str("Fseek end error"));
  }
  long sz = ftell(f);
  if (sz < 0) {
    fclose(f);
    return iris_make_err(iris_box_str("Ftell error"));
  }
  if (fseek(f, 0, SEEK_SET) != 0) {
    fclose(f);
    return iris_make_err(iris_box_str("Fseek set error"));
  }
  size_t size = (size_t)sz;
  char *buf = xmalloc(size + 1);
  size_t n = fread(buf, 1, size, f);
  buf[n] = '\0';
  fclose(f);
  IrisVal *contents = iris_box_str(buf);
  xfree(buf);
  IrisResult *r = iris_make_ok(contents);
  return r;
}

IrisResult *iris_file_write_all(const char *path, const char *contents) {
  iris_sandbox_require_fs_write_bytes(path,
                                      contents ? (int64_t)strlen(contents) : 0);
  if (iris_tx_stage_write(path, contents)) {
    return iris_make_ok(iris_box_i64((int64_t)strlen(contents)));
  }
  FILE *f = fopen(path, "wb");
  if (!f) {
    return iris_make_err(iris_box_str("Failed to open file for writing"));
  }
  size_t len = strlen(contents);
  size_t written = fwrite(contents, 1, len, f);
  fclose(f);
  if (written != len) {
    return iris_make_err(iris_box_str("Incomplete write"));
  }
  IrisVal *b = iris_box_i64((int64_t)written);
  IrisResult *r = iris_make_ok(b);
  return r;
}

int iris_file_exists(const char *path) {
  iris_sandbox_require_fs_read(path);
  FILE *f = fopen(path, "r");
  if (!f)
    return 0;
  fclose(f);
  return 1;
}

IrisList *iris_file_lines(const char *path) {
  iris_sandbox_require_fs_read(path);
  FILE *f = fopen(path, "r");
  if (!f)
    return iris_list_new();
  IrisList *r = iris_list_new();
  char buf[8192];
  while (fgets(buf, sizeof(buf), f)) {
    size_t n = strlen(buf);
    if (n > 0 && buf[n - 1] == '\n')
      buf[--n] = '\0';
    iris_list_push(r, iris_box_str(buf));
  }
  fclose(f);
  return r;
}

/* Streaming File I/O */
int64_t iris_file_open(const char *path, const char *mode) {
  if (!path || !mode)
    return 0;
  int want_write = strchr(mode, 'w') != NULL || strchr(mode, 'a') != NULL ||
                   strchr(mode, '+') != NULL;
  int want_read = strchr(mode, 'r') != NULL || strchr(mode, '+') != NULL;
  if (want_read)
    iris_sandbox_require_fs_read(path);
  if (want_write)
    iris_sandbox_require_fs_write(path);
  if (!want_read && !want_write)
    iris_sandbox_require_fs_read(path);
  iris_sandbox_require_open_slot();
  FILE *f = fopen(path, mode);
  iris_sandbox_track_file_open(f, path, want_write);
  return (int64_t)f;
}

int iris_file_close(int64_t handle) {
  FILE *f = (FILE *)handle;
  if (!f)
    return 0;
  iris_sandbox_track_file_close(f);
  return fclose(f) == 0 ? 1 : 0;
}

char *iris_file_read(int64_t handle, int64_t bytes) {
  FILE *f = (FILE *)handle;
  if (f)
    iris_sandbox_require_file_handle_read(f);
  if (!f || bytes <= 0) {
    char *empty = xmalloc(1);
    empty[0] = '\0';
    return empty;
  }
  char *buf = xmalloc(bytes + 1);
  size_t n = fread(buf, 1, (size_t)bytes, f);
  buf[n] = '\0';
  return buf;
}

int iris_file_write(int64_t handle, const char *data) {
  FILE *f = (FILE *)handle;
  size_t len = data ? strlen(data) : 0;
  if (f)
    iris_sandbox_require_file_handle_write(f, (int64_t)len);
  if (!f || !data)
    return 0;
  size_t written = fwrite(data, 1, len, f);
  return written == len ? 1 : 0;
}

// ---------------------------------------------------------------------------
// Database operations (SQLite via dynamic loading)
// ---------------------------------------------------------------------------

static IRIS_THREAD_LOCAL int64_t iris_http_status = 0;

#ifdef _WIN32
#include <windows.h>
static HMODULE sqlite3_lib = NULL;
#else
#include <dlfcn.h>
static void *sqlite3_lib = NULL;
#endif

// SQLite3 type definitions (avoid requiring sqlite3.h)
typedef struct sqlite3 sqlite3;
typedef struct sqlite3_stmt sqlite3_stmt;
#define SQLITE_OK 0
#define SQLITE_ROW 100
#define SQLITE_DONE 101

// Function pointer types
typedef int (*fn_sqlite3_open)(const char *, sqlite3 **);
typedef int (*fn_sqlite3_close)(sqlite3 *);
typedef int (*fn_sqlite3_exec)(sqlite3 *, const char *, void *, void *,
                               char **);
typedef int (*fn_sqlite3_prepare_v2)(sqlite3 *, const char *, int,
                                     sqlite3_stmt **, const char **);
typedef int (*fn_sqlite3_step)(sqlite3_stmt *);
typedef int (*fn_sqlite3_finalize)(sqlite3_stmt *);
typedef int (*fn_sqlite3_column_count)(sqlite3_stmt *);
typedef const unsigned char *(*fn_sqlite3_column_text)(sqlite3_stmt *, int);
typedef void (*fn_sqlite3_free)(void *);
typedef int (*fn_sqlite3_bind_text)(sqlite3_stmt *, int, const char *, int,
                                    void (*)(void *));
typedef int (*fn_sqlite3_bind_double)(sqlite3_stmt *, int, double);
typedef int (*fn_sqlite3_bind_int64)(sqlite3_stmt *, int, long long);

#ifndef SQLITE_TRANSIENT
#define SQLITE_TRANSIENT ((void (*)(void *)) - 1)
#endif

// Loaded function pointers
static fn_sqlite3_open p_sqlite3_open = NULL;
static fn_sqlite3_close p_sqlite3_close = NULL;
static fn_sqlite3_exec p_sqlite3_exec = NULL;
static fn_sqlite3_prepare_v2 p_sqlite3_prepare_v2 = NULL;
static fn_sqlite3_step p_sqlite3_step = NULL;
static fn_sqlite3_finalize p_sqlite3_finalize = NULL;
static fn_sqlite3_column_count p_sqlite3_column_count = NULL;
static fn_sqlite3_column_text p_sqlite3_column_text = NULL;
static fn_sqlite3_free p_sqlite3_free = NULL;
static fn_sqlite3_bind_text p_sqlite3_bind_text = NULL;
static fn_sqlite3_bind_double p_sqlite3_bind_double = NULL;
static fn_sqlite3_bind_int64 p_sqlite3_bind_int64 = NULL;

static int iris_load_sqlite3(void) {
  if (p_sqlite3_open)
    return 1; // already loaded
#ifdef _WIN32
  char exe_path[MAX_PATH];
  if (GetModuleFileNameA(NULL, exe_path, MAX_PATH)) {
    char *last_slash = strrchr(exe_path, '\\');
    if (!last_slash)
      last_slash = strrchr(exe_path, '/');
    if (last_slash) {
      *last_slash = '\0';
      char dll_path[MAX_PATH];
      snprintf(dll_path, sizeof(dll_path), "%s\\sqlite3.dll", exe_path);
      sqlite3_lib = LoadLibraryExA(
          dll_path, NULL, 0x00000008 /* LOAD_WITH_ALTERED_SEARCH_PATH */);
      if (!sqlite3_lib) {
        snprintf(dll_path, sizeof(dll_path), "%s\\libsqlite3-0.dll", exe_path);
        sqlite3_lib = LoadLibraryExA(
            dll_path, NULL, 0x00000008 /* LOAD_WITH_ALTERED_SEARCH_PATH */);
      }
    }
  }
  if (!sqlite3_lib)
    sqlite3_lib = LoadLibraryA("sqlite3.dll");
  if (!sqlite3_lib)
    sqlite3_lib = LoadLibraryA("libsqlite3-0.dll");
  if (!sqlite3_lib)
    sqlite3_lib =
        LoadLibraryExA("C:\\msys64\\ucrt64\\bin\\libsqlite3-0.dll", NULL,
                       0x00000008 /* LOAD_WITH_ALTERED_SEARCH_PATH */);
  if (!sqlite3_lib)
    sqlite3_lib =
        LoadLibraryExA("C:\\msys64\\mingw64\\bin\\libsqlite3-0.dll", NULL,
                       0x00000008 /* LOAD_WITH_ALTERED_SEARCH_PATH */);
  if (!sqlite3_lib)
    return 0;
#define LOAD(name) p_##name = (fn_##name)GetProcAddress(sqlite3_lib, #name)
#else
  sqlite3_lib = dlopen("libsqlite3.so", 1 /* RTLD_LAZY */);
  if (!sqlite3_lib)
    sqlite3_lib = dlopen("libsqlite3.so.0", 1);
  if (!sqlite3_lib)
    sqlite3_lib = dlopen("libsqlite3.dylib", 1);
  if (!sqlite3_lib)
    return 0;
#define LOAD(name) p_##name = (fn_##name)dlsym(sqlite3_lib, #name)
#endif
  LOAD(sqlite3_open);
  LOAD(sqlite3_close);
  LOAD(sqlite3_exec);
  LOAD(sqlite3_prepare_v2);
  LOAD(sqlite3_step);
  LOAD(sqlite3_finalize);
  LOAD(sqlite3_column_count);
  LOAD(sqlite3_column_text);
  LOAD(sqlite3_bind_text);
  LOAD(sqlite3_bind_double);
  LOAD(sqlite3_bind_int64);
  LOAD(sqlite3_free);
#undef LOAD
  return p_sqlite3_open ? 1 : 0;
}

int64_t iris_db_open(const char *path) {
  iris_sandbox_require_ffi("sqlite3");
  if (path && strcmp(path, ":memory:") != 0) {
    iris_sandbox_require_fs_read(path);
    iris_sandbox_require_fs_write(path);
  }
  if (!iris_load_sqlite3())
    return 0;
  sqlite3 *db = NULL;
  if (p_sqlite3_open(path, &db) != SQLITE_OK)
    return 0;
  return (int64_t)(intptr_t)db;
}

int64_t iris_db_exec(int64_t db, const char *sql) {
  if (!db || !p_sqlite3_exec)
    return -1;
  sqlite3 *conn = (sqlite3 *)(intptr_t)db;
  char *err = NULL;
  int rc = p_sqlite3_exec(conn, sql, NULL, NULL, &err);
  if (err)
    p_sqlite3_free(err);
  return rc == SQLITE_OK ? 0 : -1;
}

IrisList *iris_db_query(int64_t db, const char *sql) {
  IrisList *rows = iris_list_new();
  if (!db || !p_sqlite3_prepare_v2)
    return rows;
  sqlite3 *conn = (sqlite3 *)(intptr_t)db;
  sqlite3_stmt *stmt = NULL;
  if (p_sqlite3_prepare_v2(conn, sql, -1, &stmt, NULL) != SQLITE_OK)
    return rows;
  int ncols = p_sqlite3_column_count(stmt);
  while (p_sqlite3_step(stmt) == SQLITE_ROW) {
    IrisList *row = iris_list_new();
    for (int i = 0; i < ncols; i++) {
      const unsigned char *txt = p_sqlite3_column_text(stmt, i);
      iris_list_push(row, iris_box_str(txt ? (const char *)txt : ""));
    }
    iris_list_push(rows, iris_box_list(row));
  }
  p_sqlite3_finalize(stmt);
  return rows;
}

IrisList *iris_db_query_params(int64_t db, const char *sql, IrisList *params) {
  IrisList *rows = iris_list_new();
  if (!db || !p_sqlite3_prepare_v2)
    return rows;
  sqlite3 *conn = (sqlite3 *)(intptr_t)db;
  sqlite3_stmt *stmt = NULL;
  if (p_sqlite3_prepare_v2(conn, sql, -1, &stmt, NULL) != SQLITE_OK)
    return rows;
  int nParams = params ? (int)iris_list_len(params) : 0;
  for (int i = 0; i < nParams; i++) {
    IrisVal *v = iris_list_get(params, i);
    if (!v)
      continue;
    if (v->tag == IRIS_TAG_STR && p_sqlite3_bind_text) {
      const char *txt = iris_unbox_str(v);
      p_sqlite3_bind_text(stmt, i + 1, txt ? txt : "", -1, SQLITE_TRANSIENT);
    } else if (v->tag == IRIS_TAG_F64 && p_sqlite3_bind_double) {
      p_sqlite3_bind_double(stmt, i + 1, v->f64);
    } else if (v->tag == IRIS_TAG_I64 && p_sqlite3_bind_int64) {
      p_sqlite3_bind_int64(stmt, i + 1, (long long)v->i64);
    } else if (p_sqlite3_bind_text) {
      char *s = iris_value_to_str(v);
      p_sqlite3_bind_text(stmt, i + 1, s ? s : "", -1, SQLITE_TRANSIENT);
      if (s)
        xfree(s);
    }
  }
  int ncols = p_sqlite3_column_count(stmt);
  while (p_sqlite3_step(stmt) == SQLITE_ROW) {
    IrisList *row = iris_list_new();
    for (int i = 0; i < ncols; i++) {
      const unsigned char *txt = p_sqlite3_column_text(stmt, i);
      iris_list_push(row, iris_box_str(txt ? (const char *)txt : ""));
    }
    iris_list_push(rows, iris_box_list(row));
  }
  p_sqlite3_finalize(stmt);
  return rows;
}

int64_t iris_db_exec_params(int64_t db, const char *sql, IrisList *params) {
  if (!db || !p_sqlite3_prepare_v2)
    return -1;
  sqlite3 *conn = (sqlite3 *)(intptr_t)db;
  sqlite3_stmt *stmt = NULL;
  if (p_sqlite3_prepare_v2(conn, sql, -1, &stmt, NULL) != SQLITE_OK)
    return -1;
  int nParams = params ? (int)iris_list_len(params) : 0;
  for (int i = 0; i < nParams; i++) {
    IrisVal *v = iris_list_get(params, i);
    if (!v)
      continue;
    if (v->tag == IRIS_TAG_STR && p_sqlite3_bind_text) {
      const char *txt = iris_unbox_str(v);
      p_sqlite3_bind_text(stmt, i + 1, txt ? txt : "", -1, SQLITE_TRANSIENT);
    } else if (v->tag == IRIS_TAG_F64 && p_sqlite3_bind_double) {
      p_sqlite3_bind_double(stmt, i + 1, v->f64);
    } else if (v->tag == IRIS_TAG_I64 && p_sqlite3_bind_int64) {
      p_sqlite3_bind_int64(stmt, i + 1, (long long)v->i64);
    } else if (p_sqlite3_bind_text) {
      char *s = iris_value_to_str(v);
      p_sqlite3_bind_text(stmt, i + 1, s ? s : "", -1, SQLITE_TRANSIENT);
      if (s)
        xfree(s);
    }
  }
  int rc = SQLITE_OK;
  int step_rc = p_sqlite3_step(stmt);
  if (step_rc != SQLITE_DONE && step_rc != SQLITE_ROW)
    rc = -1;
  p_sqlite3_finalize(stmt);
  return rc == SQLITE_OK ? 0 : -1;
}

int64_t iris_db_close(int64_t db) {
  if (!db || !p_sqlite3_close)
    return -1;
  sqlite3 *conn = (sqlite3 *)(intptr_t)db;
  return p_sqlite3_close(conn) == SQLITE_OK ? 0 : -1;
}

// ---------------------------------------------------------------------------
// Process and environment
// ---------------------------------------------------------------------------

static int saved_argc = 0;
static char **saved_argv = NULL;

void iris_set_argv(int argc, char **argv) {
  saved_argc = argc;
  saved_argv = argv;
}

IrisList *iris_process_args(void) {
  IrisList *r = iris_list_new();
  if (!saved_argv)
    return r;
  for (int i = 0; i < saved_argc; i++)
    iris_list_push(r, iris_box_str(saved_argv[i]));
  return r;
}

IrisOption *iris_env_var(const char *key) {
#ifdef _WIN32
  const char *v = getenv(key);
  if (!v) {
    if (_stricmp(key, "PATH") == 0) {
      v = getenv("Path");
      if (!v)
        v = getenv("path");
    }
  }
#else
  const char *v = getenv(key);
#endif
  if (v) {
    return iris_make_some(iris_box_str(v));
  } else {
    return iris_make_none();
  }
}

// ---------------------------------------------------------------------------
// Channels and concurrency
// ---------------------------------------------------------------------------

#define CHAN_INIT_CAP 64u

/* Fixed native async executor. `spawn` and lowered `async def` jobs share this
 * queue instead of creating one OS thread per task. A worker that awaits an
 * empty channel helps execute another queued job, which prevents nested awaits
 * from exhausting the pool. */
typedef struct IrisAsyncJob {
  void *(*fn)(void *);
  void *arg;
  struct IrisAsyncJob *next;
} IrisAsyncJob;

static pthread_mutex_t iris_async_mu = PTHREAD_MUTEX_INITIALIZER;
static pthread_cond_t iris_async_ready = PTHREAD_COND_INITIALIZER;
static IrisAsyncJob *iris_async_head = NULL;
static IrisAsyncJob *iris_async_tail = NULL;
static int64_t iris_async_depth = 0;
static int64_t iris_async_workers = 0;
static int iris_async_started = 0;
static int64_t iris_hw_threads(void);

static int iris_async_try_run_one(void) {
  IrisAsyncJob *job = NULL;
  pthread_mutex_lock(&iris_async_mu);
  if (iris_async_head) {
    job = iris_async_head;
    iris_async_head = job->next;
    if (!iris_async_head)
      iris_async_tail = NULL;
    iris_async_depth--;
  }
  pthread_mutex_unlock(&iris_async_mu);
  if (!job)
    return 0;
  IrisUiPool *saved_pool = iris_active_ui_pool;
  iris_active_ui_pool = NULL;
  job->fn(job->arg);
  iris_active_ui_pool = saved_pool;
  free(job);
  return 1;
}

static void *iris_async_worker(void *unused) {
  (void)unused;
  for (;;) {
    pthread_mutex_lock(&iris_async_mu);
    while (!iris_async_head)
      pthread_cond_wait(&iris_async_ready, &iris_async_mu);
    IrisAsyncJob *job = iris_async_head;
    iris_async_head = job->next;
    if (!iris_async_head)
      iris_async_tail = NULL;
    iris_async_depth--;
    pthread_mutex_unlock(&iris_async_mu);
    job->fn(job->arg);
    free(job);
  }
  return NULL;
}

static void iris_async_start(void) {
  pthread_mutex_lock(&iris_async_mu);
  if (iris_async_started) {
    pthread_mutex_unlock(&iris_async_mu);
    return;
  }
  iris_async_started = 1;
  iris_async_workers = iris_hw_threads();
  if (iris_async_workers > 32)
    iris_async_workers = 32;
  pthread_mutex_unlock(&iris_async_mu);
  for (int64_t i = 0; i < iris_async_workers; i++) {
    pthread_t worker;
    if (pthread_create(&worker, NULL, iris_async_worker, NULL) == 0) {
      pthread_detach(worker);
    }
  }
}

static void chan_grow(IrisChannel *c) {
  size_t new_cap = c->cap * 2;
  IrisUiPool *saved_pool = iris_active_ui_pool;
  iris_active_ui_pool = NULL;
  IrisVal **new_buf = xmalloc(sizeof(IrisVal *) * new_cap);
  iris_active_ui_pool = saved_pool;
  for (size_t i = 0; i < c->count; i++) {
    new_buf[i] = c->buf[(c->head + i) % c->cap];
  }
  xfree(c->buf);
  c->buf = new_buf;
  c->cap = new_cap;
  c->head = 0;
  c->tail = c->count;
}

/* WASM pthread_create override: runs fn(arg) synchronously since
   WASI preview 1 has no threading. The declaration is in iris_runtime.h
   as a macro redirect; the prototype uses void* to avoid requiring
   pthread_t at header inclusion time. */
#if defined(__wasm__)
int iris_wasm_pthread_create(void *t, const void *a, void *(*fn)(void *),
                             void *arg) {
  (void)a;
  if (t)
    *(pthread_t *)t = NULL;
  if (fn)
    fn(arg);
  return 0;
}
#endif

/* Idle task executors (see iris_async_block_on) sleep until their next timer
 * or until any thread sends on a channel. Every send bumps the epoch; an
 * executor reads it before checking its leaves and sleeps only while it is
 * unchanged, so no send is missed. */
static int64_t iris_exec_wake_epoch = 0;
static int64_t iris_exec_idle_waiters = 0;
static pthread_mutex_t iris_exec_wake_mu = PTHREAD_MUTEX_INITIALIZER;
static pthread_cond_t iris_exec_wake_cv = PTHREAD_COND_INITIALIZER;

/* All wake state is guarded by iris_exec_wake_mu (portable to MSVC, which
 * has no __atomic builtins; uncontended, the lock is cheap next to a send). */
static void iris_exec_notify(void) {
  pthread_mutex_lock(&iris_exec_wake_mu);
  iris_exec_wake_epoch++;
  if (iris_exec_idle_waiters > 0)
    pthread_cond_broadcast(&iris_exec_wake_cv);
  pthread_mutex_unlock(&iris_exec_wake_mu);
}

static int64_t iris_exec_epoch(void) {
  pthread_mutex_lock(&iris_exec_wake_mu);
  int64_t epoch = iris_exec_wake_epoch;
  pthread_mutex_unlock(&iris_exec_wake_mu);
  return epoch;
}

/* The absolute CLOCK_REALTIME time `ms` milliseconds from now, as the pthread
 * timed waits expect. */
static void iris_timespec_after_ms(struct timespec *ts, int64_t ms) {
#if defined(_WIN32) || defined(_WIN64)
  FILETIME ft;
  GetSystemTimeAsFileTime(&ft);
  ULARGE_INTEGER uli;
  uli.LowPart = ft.dwLowDateTime;
  uli.HighPart = ft.dwHighDateTime;
  /* FILETIME counts from 1601; pthread timeouts from the Unix epoch. */
  uint64_t unix_100ns = uli.QuadPart - 116444736000000000ULL;
  uint64_t ns_total = unix_100ns * 100 + (uint64_t)ms * 1000000;
  ts->tv_sec = (time_t)(ns_total / 1000000000);
  ts->tv_nsec = (long)(ns_total % 1000000000);
#else
  clock_gettime(CLOCK_REALTIME, ts);
  int64_t ns = ts->tv_nsec + (ms % 1000) * 1000000;
  ts->tv_sec += (ms / 1000) + (ns / 1000000000);
  ts->tv_nsec = ns % 1000000000;
#endif
}

/* Sleep for up to `wait_ms`, or until a channel send after `epoch` was read. */
/* Defined with the task executor: waits on this thread's pending
 * `async_readable` sockets, if it has any. */
static int iris_exec_wait_sockets(int64_t wait_ms);

static void iris_exec_idle_wait(int64_t epoch, int64_t wait_ms) {
  if (wait_ms <= 0)
    return;
  if (iris_exec_wait_sockets(wait_ms))
    return;
#if defined(__wasm__)
  /* Single-threaded: no send can arrive while waiting; just sleep. */
  (void)epoch;
  iris_sleep_ms(wait_ms);
#else
  struct timespec ts;
  iris_timespec_after_ms(&ts, wait_ms);
  pthread_mutex_lock(&iris_exec_wake_mu);
  if (iris_exec_wake_epoch == epoch) {
    iris_exec_idle_waiters++;
    pthread_cond_timedwait(&iris_exec_wake_cv, &iris_exec_wake_mu, &ts);
    iris_exec_idle_waiters--;
  }
  pthread_mutex_unlock(&iris_exec_wake_mu);
#endif
}

/* Defined with the task executor at the end of this file. */
static int iris_exec_step(int64_t *wait_ms);

IrisChannel *iris_chan_new(int64_t capacity) {
  IrisUiPool *saved_pool = iris_active_ui_pool;
  iris_active_ui_pool = NULL;
  IrisChannel *c = xmalloc(sizeof(IrisChannel));
  c->cap = capacity > 0 ? capacity : CHAN_INIT_CAP;
  c->buf = xmalloc(sizeof(IrisVal *) * c->cap);
  iris_active_ui_pool = saved_pool;
  c->head = c->tail = c->count = 0;
  c->max_cap = capacity; // Store intended capacity, -1 if unbounded
  pthread_mutex_init(&c->mu, NULL);
  pthread_cond_init(&c->not_empty, NULL);
  pthread_cond_init(&c->not_full, NULL);
  return c;
}
void iris_chan_send(IrisChannel *c, IrisVal *val) {
  pthread_mutex_lock(&c->mu);
  if (c->max_cap >= 0) {
    while (c->count >= c->max_cap) {
      pthread_cond_wait(&c->not_full, &c->mu);
    }
  } else {
    if (c->count == c->cap)
      chan_grow(c);
  }
  if (val) {
    IrisUiPool *saved_pool = iris_active_ui_pool;
    iris_active_ui_pool = NULL;
    iris_retain(val);
    iris_active_ui_pool = saved_pool;
  }
  c->buf[c->tail] = val;
  c->tail = (c->tail + 1) % c->cap;
  c->count++;
  pthread_cond_signal(&c->not_empty);
  pthread_mutex_unlock(&c->mu);
  iris_exec_notify();
}
IrisVal *iris_chan_recv(IrisChannel *c) {
  pthread_mutex_lock(&c->mu);
  while (c->count == 0) {
    pthread_mutex_unlock(&c->mu);
    /* Keep this thread's tasks running while waiting, so a task created
     * here can send the value; then help with queued spawn jobs. */
    int64_t epoch = iris_exec_epoch();
    int64_t wait_ms = -1;
    IrisUiPool *saved_pool = iris_active_ui_pool;
    iris_active_ui_pool = NULL;
    int stepped = iris_exec_step(&wait_ms) || iris_async_try_run_one();
    iris_active_ui_pool = saved_pool;
    if (stepped) {
      pthread_mutex_lock(&c->mu);
      continue;
    }
    if (wait_ms >= 0) {
      /* This thread's tasks are sleeping or receiving. */
      iris_exec_idle_wait(epoch, wait_ms);
      pthread_mutex_lock(&c->mu);
      continue;
    }
    pthread_mutex_lock(&c->mu);
    if (c->count == 0)
      pthread_cond_wait(&c->not_empty, &c->mu);
  }
  IrisVal *val = c->buf[c->head];
  c->head = (c->head + 1) % c->cap;
  c->count--;
  if (c->max_cap >= 0)
    pthread_cond_signal(&c->not_full);
  pthread_mutex_unlock(&c->mu);
  return val;
}

int64_t iris_chan_len(IrisChannel *c) {
  if (!c)
    return 0;
  pthread_mutex_lock(&c->mu);
  int64_t len = (int64_t)c->count;
  pthread_mutex_unlock(&c->mu);
  return len;
}

IrisOption *iris_chan_try_recv(IrisChannel *c) {
  if (!c)
    return iris_make_none();
  pthread_mutex_lock(&c->mu);
  if (c->count == 0) {
    pthread_mutex_unlock(&c->mu);
    return iris_make_none();
  }
  IrisVal *val = c->buf[c->head];
  c->head = (c->head + 1) % c->cap;
  c->count--;
  if (c->max_cap >= 0) {
    pthread_cond_signal(&c->not_full);
  }
  pthread_mutex_unlock(&c->mu);

  IrisOption *opt = iris_make_some(val);
  if (val) {
    iris_release(val); // Transfer reference from channel buffer to Option
  }
  return opt;
}

int64_t iris_select(int64_t n, ...) {
  va_list args;
  va_start(args, n);
  for (int64_t i = 0; i < n; i++) {
    IrisChannel *c = va_arg(args, IrisChannel *);
    if (c) {
      pthread_mutex_lock(&c->mu);
      if (c->count > 0) {
        pthread_mutex_unlock(&c->mu);
        va_end(args);
        return i;
      }
      pthread_mutex_unlock(&c->mu);
    }
  }
  va_end(args);
  return -1;
}

int iris_timeout(int64_t ms) {
  iris_sleep_ms(ms);
  return 1;
}
void iris_spawn_fn(void *fn, void *arg) {
#if defined(__wasm__)
  ((void *(*)(void *))fn)(arg);
#else
  iris_async_start();
  IrisAsyncJob *job = (IrisAsyncJob *)malloc(sizeof(IrisAsyncJob));
  if (!job) {
    fprintf(stderr, "iris: out of memory\n");
    iris_fatal();
  }
  job->fn = (void *(*)(void *))fn;
  job->arg = arg;
  job->next = NULL;
  pthread_mutex_lock(&iris_async_mu);
  if (iris_async_tail)
    iris_async_tail->next = job;
  else
    iris_async_head = job;
  iris_async_tail = job;
  iris_async_depth++;
  pthread_cond_signal(&iris_async_ready);
  pthread_mutex_unlock(&iris_async_mu);
#endif
}

int64_t iris_async_worker_count(void) {
  pthread_mutex_lock(&iris_async_mu);
  int64_t count = iris_async_workers;
  pthread_mutex_unlock(&iris_async_mu);
  return count;
}

int64_t iris_async_queued_tasks(void) {
  pthread_mutex_lock(&iris_async_mu);
  int64_t count = iris_async_depth;
  pthread_mutex_unlock(&iris_async_mu);
  return count;
}

typedef struct {
  void (*fn)(int64_t, void *);
  int64_t i;
  void *arg;
} ParArg;
static void *par_for_worker(void *arg) {
  ParArg *a = (ParArg *)arg;
  a->fn(a->i, a->arg);
  free(a);
  return NULL;
}
/* Worker arguments for a strided share of the iteration space. */
typedef struct {
  void (*fn)(int64_t, void *);
  int64_t start;
  int64_t end;
  int64_t stride;
  void *arg;
  void *errors; /* ParErrors*, or NULL */
} ParRangeArg;

/* When the thread starting a `par for` has an error trap, each worker traps
 * its own errors: the first error is kept here and raised on the starting
 * thread after every worker has stopped. */
typedef struct {
  IrisErrorTrap *caller;
  int failed;
  char message[4096];
  pthread_mutex_t mu;
} ParErrors;

static void par_errors_note(ParErrors *e, const char *message) {
  pthread_mutex_lock(&e->mu);
  if (!e->failed) {
    e->failed = 1;
    snprintf(e->message, sizeof e->message, "%s", message);
  }
  pthread_mutex_unlock(&e->mu);
}

/* After the workers: the error one of them hit, raised on this thread. */
static void par_errors_raise(ParErrors *e) {
#if IRIS_ERROR_TRAPS
  if (e->caller && e->failed && iris_current_trap == e->caller) {
    IrisErrorTrap *trap = iris_current_trap;
    iris_current_trap = trap->prev;
    snprintf(trap->message, sizeof trap->message, "%s", e->message);
    longjmp(trap->jump, 1);
  }
#else
  (void)e;
#endif
}

static void *par_for_range_worker(void *p) {
  ParRangeArg *a = (ParRangeArg *)p;
#if IRIS_ERROR_TRAPS
  ParErrors *errors = (ParErrors *)a->errors;
  IrisErrorTrap trap;
  if (errors && errors->caller) {
    iris_error_trap_push(&trap);
    if (IRIS_TRAP_SET(&trap)) {
      par_errors_note(errors, trap.message);
      return NULL;
    }
  }
#endif
  for (int64_t i = a->start; i < a->end; i += a->stride) {
#if IRIS_ERROR_TRAPS
    if (errors && errors->failed)
      break;
#endif
    a->fn(i, a->arg);
  }
#if IRIS_ERROR_TRAPS
  if (errors && errors->caller)
    iris_error_trap_pop(&trap);
#endif
  return NULL;
}

static int64_t iris_hw_threads(void) {
#if defined(_WIN32)
  SYSTEM_INFO si;
  GetSystemInfo(&si);
  int64_t n = (int64_t)si.dwNumberOfProcessors;
#elif defined(_SC_NPROCESSORS_ONLN)
  int64_t n = (int64_t)sysconf(_SC_NPROCESSORS_ONLN);
#else
  int64_t n = 4;
#endif
  if (n < 1)
    n = 1;
  if (n > 64)
    n = 64;
  return n;
}

/* A fixed pool striding the iteration space, not one thread per iteration.
 *
 * The previous implementation called pthread_create once per index, so
 * `par for i in 0..2000` created two thousand OS threads. At roughly 1 MB of
 * reserved stack each that is an address-space problem before it is a
 * performance one, and the creation cost so dominated that iterations barely
 * overlapped -- which was one of two accidents hiding the unsynchronised
 * collection mutation now fixed above.
 *
 * Striding rather than contiguous blocks keeps the split even when the body
 * cost varies with the index, which it usually does. */
void iris_par_for(void (*fn)(int64_t, void *), int64_t start, int64_t end,
                  void *arg) {
  int64_t n = end - start;
  if (n <= 0)
    return;

  int64_t workers = iris_hw_threads();
  if (workers > n)
    workers = n;
  if (workers <= 1) {
    for (int64_t i = start; i < end; i++)
      fn(i, arg);
    return;
  }

  ParErrors errors;
  errors.caller = iris_current_trap;
  errors.failed = 0;
  errors.message[0] = '\0';
  pthread_mutex_init(&errors.mu, NULL);
  pthread_t *threads = xmalloc(sizeof(pthread_t) * (size_t)workers);
  ParRangeArg *args = xmalloc(sizeof(ParRangeArg) * (size_t)workers);
  /* Per-slot, not a high-water mark: a failed create in the middle would
   * otherwise leave an uninitialised handle inside the join range. */
  unsigned char *created = xmalloc((size_t)workers);
  for (int64_t w = 0; w < workers; w++) {
    args[w].fn = fn;
    args[w].start = start + w;
    args[w].end = end;
    args[w].stride = workers;
    args[w].arg = arg;
    args[w].errors = &errors;
    if (pthread_create(&threads[w], NULL, par_for_range_worker, &args[w]) ==
        0) {
      created[w] = 1;
    } else {
      /* Out of threads: run this share inline rather than dropping it. */
      created[w] = 0;
      par_for_range_worker(&args[w]);
    }
  }
  for (int64_t w = 0; w < workers; w++) {
    if (created[w])
      pthread_join(threads[w], NULL);
  }
  xfree(created);
  xfree(threads);
  xfree(args);
  par_errors_raise(&errors);
}

/* `par for` whose body runs a contiguous range at a time, `fn(lo, hi, arg)`:
 * what a vectorised body needs, where `iris_par_for`'s strided split would
 * hand each thread every n-th index. One contiguous share per worker. */
typedef struct {
  void (*fn)(int64_t, int64_t, void *);
  int64_t lo, hi;
  void *arg;
  ParErrors *errors;
} ParChunkArg;

static void *par_for_chunk_worker(void *p) {
  ParChunkArg *a = (ParChunkArg *)p;
#if IRIS_ERROR_TRAPS
  IrisErrorTrap trap;
  if (a->errors && a->errors->caller) {
    iris_error_trap_push(&trap);
    if (IRIS_TRAP_SET(&trap)) {
      par_errors_note(a->errors, trap.message);
      return NULL;
    }
  }
#endif
  a->fn(a->lo, a->hi, a->arg);
#if IRIS_ERROR_TRAPS
  if (a->errors && a->errors->caller)
    iris_error_trap_pop(&trap);
#endif
  return NULL;
}

void iris_par_for_chunked(void (*fn)(int64_t, int64_t, void *), int64_t start,
                          int64_t end, void *arg) {
  int64_t n = end - start;
  if (n <= 0)
    return;

  int64_t workers = iris_hw_threads();
  if (workers > n)
    workers = n;
  if (workers <= 1) {
    fn(start, end, arg);
    return;
  }

  ParErrors errors;
  errors.caller = iris_current_trap;
  errors.failed = 0;
  errors.message[0] = '\0';
  pthread_mutex_init(&errors.mu, NULL);
  pthread_t *threads = xmalloc(sizeof(pthread_t) * (size_t)workers);
  ParChunkArg *args = xmalloc(sizeof(ParChunkArg) * (size_t)workers);
  unsigned char *created = xmalloc((size_t)workers);
  int64_t share = n / workers, extra = n % workers, lo = start;
  for (int64_t w = 0; w < workers; w++) {
    int64_t len = share + (w < extra ? 1 : 0);
    args[w].fn = fn;
    args[w].lo = lo;
    args[w].hi = lo + len;
    args[w].arg = arg;
    args[w].errors = &errors;
    lo += len;
    if (pthread_create(&threads[w], NULL, par_for_chunk_worker, &args[w]) ==
        0) {
      created[w] = 1;
    } else {
      created[w] = 0;
      par_for_chunk_worker(&args[w]);
    }
  }
  for (int64_t w = 0; w < workers; w++) {
    if (created[w])
      pthread_join(threads[w], NULL);
  }
  xfree(created);
  xfree(threads);
  xfree(args);
  par_errors_raise(&errors);
}

typedef struct {
  IrisVal *(*invoke)(IrisVal *, IrisVal *);
  IrisVal *closure;
  IrisVal *arg;
  IrisVal *result;
} ParMapArg;

static void *par_map_worker(void *arg) {
  ParMapArg *a = (ParMapArg *)arg;
  a->result = a->invoke(a->closure, a->arg);
  return NULL;
}

IrisList *iris_par_map(IrisList *list, IrisVal *closure,
                       IrisVal *(*invoke)(IrisVal *, IrisVal *)) {
  int64_t n = iris_list_len(list);
  IrisList *results = iris_list_new();
  if (n <= 0)
    return results;
  pthread_t *threads = xmalloc(sizeof(pthread_t) * (size_t)n);
  ParMapArg *args = xmalloc(sizeof(ParMapArg) * (size_t)n);
  unsigned char *created = xmalloc((size_t)n);
  for (int64_t i = 0; i < n; i++) {
    args[i].invoke = invoke;
    args[i].closure = closure;
    args[i].arg = iris_list_get(list, i);
    args[i].result = NULL;
    if (pthread_create(&threads[i], NULL, par_map_worker, &args[i]) == 0) {
      created[i] = 1;
    } else {
      created[i] = 0;
      par_map_worker(&args[i]);
    }
  }
  for (int64_t i = 0; i < n; i++) {
    if (created[i])
      pthread_join(threads[i], NULL);
    iris_list_push(results, args[i].result);
  }
  xfree(created);
  xfree(threads);
  xfree(args);
  return results;
}
void iris_barrier(void) { /* no-op outside par_for; par_for already joins all */
}

// ── TaskGroup ──────────────────────────────────────────────────────────
typedef struct {
  IrisTaskGroup *group;
  void *(*fn)(void *);
  void *arg;
} IrisTaskGroupJob;

static IRIS_THREAD_LOCAL IrisTaskGroup *iris_current_task_group = NULL;

static void *iris_task_group_worker(void *raw) {
  IrisTaskGroupJob *job = (IrisTaskGroupJob *)raw;
  IrisTaskGroup *previous = iris_current_task_group;
  iris_current_task_group = job->group;
  job->fn(job->arg);
  iris_current_task_group = previous;

  pthread_mutex_lock(&job->group->mu);
  if (job->group->active > 0)
    job->group->active--;
  if (job->group->active == 0)
    pthread_cond_signal(&job->group->done);
  pthread_mutex_unlock(&job->group->mu);
  free(job);
  return NULL;
}

IrisTaskGroup *iris_task_group_new(void) {
  IrisUiPool *saved_pool = iris_active_ui_pool;
  iris_active_ui_pool = NULL;
  IrisTaskGroup *tg = xmalloc(sizeof(IrisTaskGroup));
  iris_active_ui_pool = saved_pool;
  tg->active = 0;
  tg->cancelled = 0;
  tg->closed = 0;
  pthread_mutex_init(&tg->mu, NULL);
  pthread_cond_init(&tg->done, NULL);
  return tg;
}
void iris_task_group_spawn(IrisTaskGroup *tg, void *fn, void *arg) {
  if (!tg || !fn)
    iris_panic("invalid task-group spawn");
  pthread_mutex_lock(&tg->mu);
  if (tg->closed) {
    pthread_mutex_unlock(&tg->mu);
    iris_panic("cannot spawn into a joined task group");
  }
  tg->active++;
  pthread_mutex_unlock(&tg->mu);

  IrisTaskGroupJob *job = (IrisTaskGroupJob *)malloc(sizeof(IrisTaskGroupJob));
  if (!job) {
    fprintf(stderr, "iris: out of memory\n");
    iris_fatal();
  }
  job->group = tg;
  job->fn = (void *(*)(void *))fn;
  job->arg = arg;
  iris_spawn_fn((void *)iris_task_group_worker, job);
}
void iris_task_group_join(IrisTaskGroup *tg) {
  if (!tg)
    return;
  if (iris_current_task_group == tg) {
    iris_panic("a task cannot join its own task group");
  }
  pthread_mutex_lock(&tg->mu);
  tg->closed = 1;
  while (tg->active > 0) {
    pthread_mutex_unlock(&tg->mu);
    if (!iris_async_try_run_one()) {
      pthread_mutex_lock(&tg->mu);
      if (tg->active > 0)
        pthread_cond_wait(&tg->done, &tg->mu);
      continue;
    }
    pthread_mutex_lock(&tg->mu);
  }
  pthread_mutex_unlock(&tg->mu);
}
void iris_task_group_cancel(IrisTaskGroup *tg) {
  if (!tg)
    return;
  pthread_mutex_lock(&tg->mu);
  tg->cancelled = 1;
  pthread_mutex_unlock(&tg->mu);
}
int32_t iris_task_group_is_cancelled(IrisTaskGroup *tg) {
  if (!tg)
    return 0;
  pthread_mutex_lock(&tg->mu);
  int32_t cancelled = tg->cancelled ? 1 : 0;
  pthread_mutex_unlock(&tg->mu);
  return cancelled;
}
int32_t iris_current_task_cancelled(void) {
  return iris_task_group_is_cancelled(iris_current_task_group);
}
int64_t iris_task_group_is_cancelled_i64(IrisTaskGroup *tg) {
  return (int64_t)iris_task_group_is_cancelled(tg);
}
int64_t iris_current_task_cancelled_i64(void) {
  return (int64_t)iris_current_task_cancelled();
}

// ---------------------------------------------------------------------------
// Effect handlers — thread-local handler stack with direct LLVM dispatch
// ---------------------------------------------------------------------------

#include <string.h>

#ifdef _MSC_VER
#define strdup _strdup
#endif

#define MAX_HANDLER_ARMS 64
#define MAX_HANDLER_FRAMES 64

typedef struct {
  char *effect_name;
  char *fn_name;
  void *handler_fn;
  int64_t num_args;
  int32_t has_resume;
} HandlerArm;

typedef struct {
  HandlerArm arms[MAX_HANDLER_ARMS];
  int narms;
} HandlerFrame;

#ifdef _MSC_VER
static __declspec(thread) HandlerFrame handler_frames[MAX_HANDLER_FRAMES];
static __declspec(thread) int handler_frame_count = 0;
static __declspec(thread) int handler_cur_narms = 0;
#else
static __thread HandlerFrame handler_frames[MAX_HANDLER_FRAMES];
static __thread int handler_frame_count = 0;
static __thread int handler_cur_narms = 0;
#endif

void iris_push_handler_arm(const char *effect_name, const char *fn_name,
                           int64_t num_args, int32_t has_resume) {
  if (handler_cur_narms >= MAX_HANDLER_ARMS)
    return;
  HandlerFrame *frame = &handler_frames[handler_frame_count];
  frame->arms[handler_cur_narms].effect_name = strdup(effect_name);
  frame->arms[handler_cur_narms].fn_name = strdup(fn_name);
  frame->arms[handler_cur_narms].handler_fn = NULL;
  frame->arms[handler_cur_narms].num_args = num_args;
  frame->arms[handler_cur_narms].has_resume = has_resume;
  handler_cur_narms++;
}

void iris_push_handler_fn(void *fn) {
  if (handler_cur_narms == 0)
    return;
  HandlerFrame *frame = &handler_frames[handler_frame_count];
  frame->arms[handler_cur_narms - 1].handler_fn = fn;
}

void iris_push_handler_frame(void) {
  if (handler_frame_count >= MAX_HANDLER_FRAMES)
    return;
  handler_frames[handler_frame_count].narms = handler_cur_narms;
  handler_cur_narms = 0;
  handler_frame_count++;
}

void iris_pop_handler(void) {
  if (handler_frame_count <= 0)
    return;
  handler_frame_count--;
  HandlerFrame *frame = &handler_frames[handler_frame_count];
  for (int i = 0; i < frame->narms; i++) {
    free(frame->arms[i].effect_name);
    free(frame->arms[i].fn_name);
  }
  handler_cur_narms = 0;
}

static HandlerArm *find_handler_arm(const char *name) {
  for (int f = handler_frame_count - 1; f >= 0; f--) {
    HandlerFrame *frame = &handler_frames[f];
    for (int a = 0; a < frame->narms; a++) {
      if (strcmp(frame->arms[a].effect_name, name) == 0) {
        return &frame->arms[a];
      }
    }
  }
  return NULL;
}

int32_t iris_can_handle(const char *name) {
  return find_handler_arm(name) != NULL ? 1 : 0;
}

int32_t iris_handler_has_resume(const char *name) {
  HandlerArm *arm = find_handler_arm(name);
  return arm ? arm->has_resume : 0;
}

void iris_resume_cont(Continuation *cont, int64_t value) {
  if (cont == NULL)
    return;
  cont->filled = 1;
  cont->value = value;
}

int32_t iris_handler_depth(void) { return handler_frame_count; }

void *iris_find_handler_fn(const char *name) {
  HandlerArm *arm = find_handler_arm(name);
  return arm ? arm->handler_fn : NULL;
}

void iris_unhandled_effect_abort(const char *effect_name) {
  fprintf(stderr,
          "error: no handler for effect '%s' and no real implementation\n",
          effect_name ? effect_name : "(unknown)");
  fflush(stderr);
  iris_fatal();
}

int64_t iris_effect_dispatch_or_call(const char *effect_name, void *real_fn,
                                     void *cont, int nargs,
                                     const int64_t *args) {
  /* Fast path: no handlers active → call real extern directly. */
  if (handler_frame_count == 0)
    goto call_real;

  {
    void *hfn = iris_find_handler_fn(effect_name);
    if (hfn) {
      /* Handler signature: i64 handler(void* cont, i64, i64, ...) */
      typedef int64_t (*hfn_t)(void *, int64_t, int64_t, int64_t, int64_t,
                               int64_t, int64_t, int64_t);
      hfn_t fn = (hfn_t)hfn;
      int64_t r = 0;
      switch (nargs) {
      case 0:
        r = fn(cont, 0, 0, 0, 0, 0, 0, 0);
        break;
      case 1:
        r = fn(cont, args[0], 0, 0, 0, 0, 0, 0);
        break;
      case 2:
        r = fn(cont, args[0], args[1], 0, 0, 0, 0, 0);
        break;
      case 3:
        r = fn(cont, args[0], args[1], args[2], 0, 0, 0, 0);
        break;
      case 4:
        r = fn(cont, args[0], args[1], args[2], args[3], 0, 0, 0);
        break;
      case 5:
        r = fn(cont, args[0], args[1], args[2], args[3], args[4], 0, 0);
        break;
      case 6:
        r = fn(cont, args[0], args[1], args[2], args[3], args[4], args[5], 0);
        break;
      default:
        r = fn(cont, args[0], args[1], args[2], args[3], args[4], args[5],
               args[6]);
        break;
      }
      /* If resume was triggered, return the continuation value. */
      if (cont) {
        Continuation *c = (Continuation *)cont;
        if (c->filled)
          return c->value;
      }
      return r;
    }
  }

call_real:
  if (!real_fn) {
    /* No handler and no real function — panic. */
    iris_unhandled_effect_abort(effect_name);
  }
  {
    /* Real extern signature: i64 real_fn(i64, i64, i64, i64, i64, i64, i64) */
    typedef int64_t (*efn_t)(int64_t, int64_t, int64_t, int64_t, int64_t,
                             int64_t, int64_t);
    efn_t fn = (efn_t)real_fn;
    switch (nargs) {
    case 0:
      return fn(0, 0, 0, 0, 0, 0, 0);
    case 1:
      return fn(args[0], 0, 0, 0, 0, 0, 0);
    case 2:
      return fn(args[0], args[1], 0, 0, 0, 0, 0);
    case 3:
      return fn(args[0], args[1], args[2], 0, 0, 0, 0);
    case 4:
      return fn(args[0], args[1], args[2], args[3], 0, 0, 0);
    case 5:
      return fn(args[0], args[1], args[2], args[3], args[4], 0, 0);
    case 6:
      return fn(args[0], args[1], args[2], args[3], args[4], args[5], 0);
    default:
      return fn(args[0], args[1], args[2], args[3], args[4], args[5], args[6]);
    }
  }
}

// ---------------------------------------------------------------------------
// Atomics and mutexes
// ---------------------------------------------------------------------------

IrisAtomic *iris_atomic_new(IrisVal *initial) {
  IrisAtomic *a = xmalloc(sizeof(IrisAtomic));
  pthread_mutex_init(&a->mu, NULL);
  a->val = initial;
  if (initial)
    iris_retain(initial);
  return a;
}
IrisVal *iris_atomic_load(IrisAtomic *a) {
  pthread_mutex_lock(&a->mu);
  IrisVal *v = a->val;
  pthread_mutex_unlock(&a->mu);
  return v;
}
void iris_atomic_store(IrisAtomic *a, IrisVal *val) {
  iris_tx_record_atomic(a);
  pthread_mutex_lock(&a->mu);
  if (val)
    iris_retain(val);
  if (a->val)
    iris_release(a->val);
  a->val = val;
  pthread_mutex_unlock(&a->mu);
}
IrisVal *iris_atomic_add(IrisAtomic *a, IrisVal *delta) {
  iris_tx_record_atomic(a);
  pthread_mutex_lock(&a->mu);
  IrisVal *result = xmalloc(sizeof(IrisVal));
  IrisVal *replacement = NULL;
  if (a->val && a->val->tag == IRIS_TAG_I64 && delta &&
      delta->tag == IRIS_TAG_I64) {
    replacement = iris_box_i64(a->val->i64 + delta->i64);
    result->tag = IRIS_TAG_I64;
    result->i64 = replacement->i64;
  } else if (a->val &&
             (a->val->tag == IRIS_TAG_F64 || a->val->tag == IRIS_TAG_F32)) {
    double d = iris_unbox_f64(a->val) + iris_unbox_f64(delta);
    replacement = iris_box_f64(d);
    result->tag = IRIS_TAG_F64;
    result->f64 = d;
  } else {
    result->tag = IRIS_TAG_I64;
    result->i64 = 0;
  }
  if (replacement) {
    iris_retain(replacement);
    if (a->val)
      iris_release(a->val);
    a->val = replacement;
  }
  pthread_mutex_unlock(&a->mu);
  return result;
}
IrisMutex *iris_mutex_new(IrisVal *initial) {
  IrisMutex *m = xmalloc(sizeof(IrisMutex));
  pthread_mutex_init(&m->mu, NULL);
  m->val = initial;
  return m;
}
IrisVal *iris_mutex_lock(IrisMutex *m) {
  pthread_mutex_lock(&m->mu);
  return m->val;
}
void iris_mutex_unlock(IrisMutex *m) { pthread_mutex_unlock(&m->mu); }

// ---------------------------------------------------------------------------
// Grad (forward-mode autodiff — dual numbers)
// ---------------------------------------------------------------------------

IrisGrad *iris_make_grad(double value, double tangent) {
  IrisGrad *g = xmalloc(sizeof(IrisGrad));
  g->value = value;
  g->tangent = tangent;
  return g;
}
double iris_grad_value(IrisGrad *g) { return g ? g->value : 0.0; }
double iris_grad_tangent(IrisGrad *g) { return g ? g->tangent : 0.0; }

IrisGrad *iris_grad_add(IrisGrad *a, IrisGrad *b) {
  double av = a ? a->value : 0.0, at = a ? a->tangent : 0.0;
  double bv = b ? b->value : 0.0, bt = b ? b->tangent : 0.0;
  return iris_make_grad(av + bv, at + bt);
}

IrisGrad *iris_grad_sub(IrisGrad *a, IrisGrad *b) {
  double av = a ? a->value : 0.0, at = a ? a->tangent : 0.0;
  double bv = b ? b->value : 0.0, bt = b ? b->tangent : 0.0;
  return iris_make_grad(av - bv, at - bt);
}

IrisGrad *iris_grad_mul(IrisGrad *a, IrisGrad *b) {
  double av = a ? a->value : 0.0, at = a ? a->tangent : 0.0;
  double bv = b ? b->value : 0.0, bt = b ? b->tangent : 0.0;
  return iris_make_grad(av * bv, av * bt + at * bv);
}

IrisGrad *iris_grad_div(IrisGrad *a, IrisGrad *b) {
  double av = a ? a->value : 0.0, at = a ? a->tangent : 0.0;
  double bv = b ? b->value : 0.0, bt = b ? b->tangent : 0.0;
  double denom = bv * bv;
  if (denom == 0.0)
    denom = 1e-15;
  return iris_make_grad(av / bv, (at * bv - av * bt) / denom);
}

IrisGrad *iris_grad_neg(IrisGrad *a) {
  double av = a ? a->value : 0.0, at = a ? a->tangent : 0.0;
  return iris_make_grad(-av, -at);
}

// ---------------------------------------------------------------------------
// Sparse tensors (COO format over IrisList of IrisVal)
// ---------------------------------------------------------------------------

IrisSparse *iris_sparsify(IrisList *dense) {
  IrisSparse *sp = xcalloc(1, sizeof(IrisSparse));
  sp->cap = 8;
  sp->indices = xmalloc(sizeof(size_t) * sp->cap);
  sp->values = xmalloc(sizeof(IrisVal *) * sp->cap);
  for (size_t i = 0; i < dense->len; i++) {
    IrisVal *v = dense->data[i];
    int is_zero = 0;
    if (v) {
      if (v->tag == IRIS_TAG_I64 && v->i64 == 0)
        is_zero = 1;
      else if (v->tag == IRIS_TAG_F64 && v->f64 == 0.0)
        is_zero = 1;
    } else
      is_zero = 1;
    if (!is_zero) {
      if (sp->len == sp->cap) {
        sp->cap *= 2;
        sp->indices = xrealloc(sp->indices, sizeof(size_t) * sp->cap);
        sp->values = xrealloc(sp->values, sizeof(IrisVal *) * sp->cap);
      }
      sp->indices[sp->len] = i;
      sp->values[sp->len] = v;
      sp->len++;
    }
  }
  return sp;
}

IrisSparse *iris_sparsify_i64_array(int64_t *data, int64_t len) {
  IrisSparse *sp = xcalloc(1, sizeof(IrisSparse));
  sp->cap = 8;
  sp->indices = xmalloc(sizeof(size_t) * sp->cap);
  sp->values = xmalloc(sizeof(IrisVal *) * sp->cap);
  for (int64_t i = 0; i < len; i++) {
    int64_t v = data[i];
    if (v != 0) {
      if (sp->len == sp->cap) {
        sp->cap *= 2;
        sp->indices = xrealloc(sp->indices, sizeof(size_t) * sp->cap);
        sp->values = xrealloc(sp->values, sizeof(IrisVal *) * sp->cap);
      }
      sp->indices[sp->len] = i;
      sp->values[sp->len] = iris_box_i64(v);
      sp->len++;
    }
  }
  return sp;
}

IrisSparse *iris_sparsify_f64_array(double *data, int64_t len) {
  IrisSparse *sp = xcalloc(1, sizeof(IrisSparse));
  sp->cap = 8;
  sp->indices = xmalloc(sizeof(size_t) * sp->cap);
  sp->values = xmalloc(sizeof(IrisVal *) * sp->cap);
  for (int64_t i = 0; i < len; i++) {
    double v = data[i];
    if (v != 0.0) {
      if (sp->len == sp->cap) {
        sp->cap *= 2;
        sp->indices = xrealloc(sp->indices, sizeof(size_t) * sp->cap);
        sp->values = xrealloc(sp->values, sizeof(IrisVal *) * sp->cap);
      }
      sp->indices[sp->len] = i;
      sp->values[sp->len] = iris_box_f64(v);
      sp->len++;
    }
  }
  return sp;
}
IrisList *iris_densify(IrisSparse *sparse) {
  /* Determine dense size from max index in sparse data. */
  int64_t size = 0;
  for (size_t i = 0; i < sparse->len; i++) {
    if ((int64_t)sparse->indices[i] >= size)
      size = (int64_t)sparse->indices[i] + 1;
  }
  IrisList *l = iris_list_new();
  /* Fill with zeros */
  for (int64_t i = 0; i < size; i++)
    iris_list_push(l, iris_box_i64(0));
  for (size_t i = 0; i < sparse->len; i++) {
    size_t idx = sparse->indices[i];
    if ((int64_t)idx < size)
      l->data[idx] = sparse->values[i];
  }
  return l;
}

// ---------------------------------------------------------------------------
// Sparse tensor operations
// ---------------------------------------------------------------------------

IrisSparse *iris_tensor_sparsify(IrisTensor *t) {
  IrisSparse *sp = xcalloc(1, sizeof(IrisSparse));
  sp->cap = 8;
  sp->indices = xmalloc(sizeof(size_t) * sp->cap);
  sp->values = xmalloc(sizeof(IrisVal *) * sp->cap);
  for (int64_t i = 0; i < t->numel; i++) {
    if (t->data[i] != 0.0f) {
      if (sp->len == sp->cap) {
        sp->cap *= 2;
        sp->indices = xrealloc(sp->indices, sizeof(size_t) * sp->cap);
        sp->values = xrealloc(sp->values, sizeof(IrisVal *) * sp->cap);
      }
      sp->indices[sp->len] = (size_t)i;
      sp->values[sp->len] = iris_box_f64((double)t->data[i]);
      sp->len++;
    }
  }
  return sp;
}

IrisTensor *iris_sparse_to_tensor(IrisSparse *sp, int64_t size) {
  IrisTensor *t = xcalloc(1, sizeof(IrisTensor));
  t->ndim = 1;
  t->numel = size;
  t->shape = xmalloc(sizeof(int64_t));
  t->shape[0] = size;
  t->data = xcalloc((size_t)size, sizeof(float));
  for (size_t i = 0; i < sp->len; i++) {
    size_t idx = sp->indices[i];
    if ((int64_t)idx < size && sp->values[i]) {
      if (sp->values[i]->tag == IRIS_TAG_F64)
        t->data[idx] = (float)sp->values[i]->f64;
      else if (sp->values[i]->tag == IRIS_TAG_I64)
        t->data[idx] = (float)sp->values[i]->i64;
    }
  }
  return t;
}

double iris_sparse_dot(IrisSparse *sp, IrisTensor *dense) {
  double sum = 0.0;
  for (size_t i = 0; i < sp->len; i++) {
    size_t idx = sp->indices[i];
    if ((int64_t)idx < dense->numel && sp->values[i]) {
      double sv = 0.0;
      if (sp->values[i]->tag == IRIS_TAG_F64)
        sv = sp->values[i]->f64;
      else if (sp->values[i]->tag == IRIS_TAG_I64)
        sv = (double)sp->values[i]->i64;
      sum += sv * (double)dense->data[idx];
    }
  }
  return sum;
}

int64_t iris_sparse_nnz(IrisSparse *sp) { return (int64_t)sp->len; }

IrisSparse *iris_sparse_add(IrisSparse *a, IrisSparse *b) {
  IrisSparse *sp = xcalloc(1, sizeof(IrisSparse));
  size_t total_cap = (a ? a->len : 0) + (b ? b->len : 0) + 8;
  sp->cap = total_cap;
  sp->indices = xmalloc(sizeof(size_t) * sp->cap);
  sp->values = xmalloc(sizeof(IrisVal *) * sp->cap);
  sp->len = 0;
  size_t i = 0, j = 0;
  while ((a && i < a->len) || (b && j < b->len)) {
    if (a && i < a->len &&
        (!b || j >= b->len || a->indices[i] < b->indices[j])) {
      sp->indices[sp->len] = a->indices[i];
      sp->values[sp->len] = a->values[i];
      sp->len++;
      i++;
    } else if (b && j < b->len &&
               (!a || i >= a->len || b->indices[j] < a->indices[i])) {
      sp->indices[sp->len] = b->indices[j];
      sp->values[sp->len] = b->values[j];
      sp->len++;
      j++;
    } else {
      double v1 = (a->values[i]->tag == IRIS_TAG_F64)
                      ? a->values[i]->f64
                      : (double)a->values[i]->i64;
      double v2 = (b->values[j]->tag == IRIS_TAG_F64)
                      ? b->values[j]->f64
                      : (double)b->values[j]->i64;
      double sum = v1 + v2;
      if (sum != 0.0) {
        sp->indices[sp->len] = a->indices[i];
        sp->values[sp->len] = iris_box_f64(sum);
        sp->len++;
      }
      i++;
      j++;
    }
  }
  return sp;
}

IrisSparse *iris_sparse_sub(IrisSparse *a, IrisSparse *b) {
  IrisSparse *sp = xcalloc(1, sizeof(IrisSparse));
  size_t total_cap = (a ? a->len : 0) + (b ? b->len : 0) + 8;
  sp->cap = total_cap;
  sp->indices = xmalloc(sizeof(size_t) * sp->cap);
  sp->values = xmalloc(sizeof(IrisVal *) * sp->cap);
  sp->len = 0;
  size_t i = 0, j = 0;
  while ((a && i < a->len) || (b && j < b->len)) {
    if (a && i < a->len &&
        (!b || j >= b->len || a->indices[i] < b->indices[j])) {
      sp->indices[sp->len] = a->indices[i];
      sp->values[sp->len] = a->values[i];
      sp->len++;
      i++;
    } else if (b && j < b->len &&
               (!a || i >= a->len || b->indices[j] < a->indices[i])) {
      sp->indices[sp->len] = b->indices[j];
      double v2 = (b->values[j]->tag == IRIS_TAG_F64)
                      ? b->values[j]->f64
                      : (double)b->values[j]->i64;
      sp->values[sp->len] = iris_box_f64(-v2);
      sp->len++;
      j++;
    } else {
      double v1 = (a->values[i]->tag == IRIS_TAG_F64)
                      ? a->values[i]->f64
                      : (double)a->values[i]->i64;
      double v2 = (b->values[j]->tag == IRIS_TAG_F64)
                      ? b->values[j]->f64
                      : (double)b->values[j]->i64;
      double diff = v1 - v2;
      if (diff != 0.0) {
        sp->indices[sp->len] = a->indices[i];
        sp->values[sp->len] = iris_box_f64(diff);
        sp->len++;
      }
      i++;
      j++;
    }
  }
  return sp;
}

IrisSparse *iris_sparse_mul_scalar(IrisSparse *a, double s) {
  if (!a)
    return NULL;
  IrisSparse *sp = xcalloc(1, sizeof(IrisSparse));
  sp->cap = a->cap;
  sp->indices = xmalloc(sizeof(size_t) * sp->cap);
  sp->values = xmalloc(sizeof(IrisVal *) * sp->cap);
  sp->len = 0;
  for (size_t i = 0; i < a->len; i++) {
    double v = (a->values[i]->tag == IRIS_TAG_F64) ? a->values[i]->f64
                                                   : (double)a->values[i]->i64;
    sp->indices[sp->len] = a->indices[i];
    sp->values[sp->len] = iris_box_f64(v * s);
    sp->len++;
  }
  return sp;
}

// ---------------------------------------------------------------------------
// Reverse-mode AD runtime
// ---------------------------------------------------------------------------
#define IRIS_TAPE_MAGIC ((uint64_t)0x4952495354415045ULL)

// The tape was a *fixed thread-local array* of IRIS_TAPE_ARENA_SIZE nodes, plus
// a topological buffer of the same length. At 88 bytes a node that is 11.5 MB +
// 1 MB reserved per thread, whether or not the thread ever called `grad`:
//
//   $ size -A iris_runtime.o
//   .tls$    12747312        <- 12.7 MB, against 86896 bytes of .text
//
// Measured at 77.4 MB for 4 threads and 808.1 MB for 64 -- ~12.2 MB per thread
// of committed working set, for threads that only slept and sent an integer.
// It also put a 12.7 MB TLS template in every binary IRIS produced. See
// known-issues #47.
//
// Now allocated lazily, in chunks. Chunked rather than a single growable block
// because nodes reference their parents by *pointer*: reallocating one block
// would invalidate every parent pointer already recorded, silently corrupting
// the graph rather than failing. Chunk addresses are stable for the life of the
// tape, so growth is safe.
//
// The ceiling is unchanged, so wrap-around behaviour is exactly as before.
#define IRIS_TAPE_CHUNK_NODES 4096
#define IRIS_TAPE_MAX_CHUNKS 32
#define IRIS_TAPE_ARENA_SIZE (IRIS_TAPE_CHUNK_NODES * IRIS_TAPE_MAX_CHUNKS)

typedef struct IrisTapeNode {
  uint64_t magic;
  double primal;
  double grad;
  const char *op;
  struct IrisTapeNode *parents[2];
  double parent_primals[2];
  int64_t parent_count;
  uint64_t grad_epoch;
  uint64_t visit_epoch;
} IrisTapeNode;

typedef struct {
  IrisTapeNode **data;
  size_t len;
  size_t cap;
} IrisTapeVec;

// Thread-local state is now a chunk table rather than the chunks themselves:
// 32 pointers and a few counters, ~300 bytes instead of 12.6 MB.
// Two regions, because they have different lifetimes.
//
// `iris_backward` resets the arena index when it finishes, so the next
// `tape(...)` used to hand back an address the previous leaf was still holding
// -- `grad(x)` then read a later node's gradient and returned a plausible wrong
// number with no error (known-issues #48):
//
//   dx=6 dy=6        <- dx should be 4
//
// Leaves are the only nodes a program keeps a handle to across a backward pass,
// so leaves are now *pinned*: they live in their own region and are never
// recycled. Intermediates, which nothing outlives the pass to reference, still
// reset -- that is what keeps a training loop from growing without bound.
static IRIS_THREAD_LOCAL IrisTapeNode *iris_tape_chunks[IRIS_TAPE_MAX_CHUNKS];
static IRIS_THREAD_LOCAL size_t iris_tape_chunk_count = 0;
static IRIS_THREAD_LOCAL size_t iris_tape_arena_index = 0;

static IRIS_THREAD_LOCAL IrisTapeNode *iris_leaf_chunks[IRIS_TAPE_MAX_CHUNKS];
static IRIS_THREAD_LOCAL size_t iris_leaf_chunk_count = 0;
static IRIS_THREAD_LOCAL size_t iris_leaf_index = 0;
static IRIS_THREAD_LOCAL IrisTapeNode **iris_topo_buffer = NULL;
static IRIS_THREAD_LOCAL size_t iris_topo_buffer_cap = 0;

static IRIS_THREAD_LOCAL uint64_t iris_tape_grad_epoch = 0;
static IRIS_THREAD_LOCAL uint64_t iris_tape_visit_epoch = 0;

// Releases this thread's tape. Called from a TLS destructor on thread exit and
// from iris_runtime_cleanup for the main thread.
static void iris_tape_thread_free(void) {
  for (size_t i = 0; i < iris_tape_chunk_count; i++) {
    free(iris_tape_chunks[i]);
    iris_tape_chunks[i] = NULL;
  }
  iris_tape_chunk_count = 0;
  iris_tape_arena_index = 0;
  for (size_t i = 0; i < iris_leaf_chunk_count; i++) {
    free(iris_leaf_chunks[i]);
    iris_leaf_chunks[i] = NULL;
  }
  iris_leaf_chunk_count = 0;
  iris_leaf_index = 0;
  free(iris_topo_buffer);
  iris_topo_buffer = NULL;
  iris_topo_buffer_cap = 0;
}

// Without this, a thread that used autodiff would leak its chunks on exit --
// which the old fixed array never had to worry about, because it was never
// allocated. `spawn` calls the user function directly (no wrapper to free
// from), and par_for workers are separate threads again, so a TLS destructor is
// the one hook that covers every thread the runtime creates.
#if !defined(_MSC_VER) && !defined(__wasm__)
static pthread_key_t iris_tape_tls_key;
static pthread_once_t iris_tape_tls_once = PTHREAD_ONCE_INIT;

static void iris_tape_tls_dtor(void *unused) {
  (void)unused;
  // Runs on the exiting thread, so the thread-local pointers are still ours.
  iris_tape_thread_free();
}
static void iris_tape_tls_init(void) {
  pthread_key_create(&iris_tape_tls_key, iris_tape_tls_dtor);
}
static void iris_tape_tls_arm(void) {
  pthread_once(&iris_tape_tls_once, iris_tape_tls_init);
  // The value is a sentinel; the destructor reads the thread-locals directly.
  // It must be non-NULL or the destructor is not run.
  if (!pthread_getspecific(iris_tape_tls_key)) {
    pthread_setspecific(iris_tape_tls_key, (void *)1);
  }
}
#else
static void iris_tape_tls_arm(void) {}
#endif

// Hands out the next node from one of the two regions, allocating a chunk only
// when one is actually needed. Returns NULL if allocation fails; callers must
// tolerate that, and do, because iris_is_tape_node(NULL) is false and every
// consumer checks it.
static IrisTapeNode *iris_tape_alloc_from(IrisTapeNode **chunks,
                                          size_t *chunk_count, size_t *index,
                                          int recycle) {
  if (*index >= IRIS_TAPE_ARENA_SIZE) {
    // Intermediates wrap, as before. Leaves must not: recycling a leaf is
    // exactly the aliasing that #48 was, so a program that exceeds the leaf
    // budget gets NULL -- a gradient of zero -- rather than another leaf's
    // gradient reported as its own.
    if (!recycle)
      return NULL;
    *index = 0;
  }
  size_t idx = *index;
  size_t chunk = idx / IRIS_TAPE_CHUNK_NODES;
  size_t off = idx % IRIS_TAPE_CHUNK_NODES;

  if (chunk >= *chunk_count) {
    // Indices advance by one and wrap, so this is always the next chunk.
    IrisTapeNode *c =
        (IrisTapeNode *)calloc(IRIS_TAPE_CHUNK_NODES, sizeof(IrisTapeNode));
    if (!c)
      return NULL;
    iris_tape_tls_arm();
    chunks[chunk] = c;
    *chunk_count = chunk + 1;
  }
  (*index)++;
  return &chunks[chunk][off];
}

// A leaf is a value the program holds a handle to and reads a gradient from
// after `backward` returns, so its address must stay stable. Everything else is
// internal to one backward pass and is recycled.
static IrisTapeNode *iris_tape_alloc_node(int is_leaf) {
  if (is_leaf) {
    return iris_tape_alloc_from(iris_leaf_chunks, &iris_leaf_chunk_count,
                                &iris_leaf_index, 0);
  }
  return iris_tape_alloc_from(iris_tape_chunks, &iris_tape_chunk_count,
                              &iris_tape_arena_index, 1);
}

static int iris_is_tape_node(const void *ptr) {
  if (!ptr)
    return 0;
  return ((const IrisTapeNode *)ptr)->magic == IRIS_TAPE_MAGIC;
}

static uint64_t iris_tape_next_epoch(uint64_t *epoch) {
  (*epoch)++;
  if (*epoch == 0)
    (*epoch)++;
  return *epoch;
}

static void iris_tape_vec_push(IrisTapeVec *vec, IrisTapeNode *node) {
  if (vec->len < vec->cap) {
    vec->data[vec->len++] = node;
  }
}

static void iris_tape_collect_topo(IrisTapeNode *node, uint64_t visit_epoch,
                                   IrisTapeVec *topo) {
  if (!iris_is_tape_node(node) || node->visit_epoch == visit_epoch)
    return;
  node->visit_epoch = visit_epoch;
  for (int64_t i = 0; i < node->parent_count; i++) {
    iris_tape_collect_topo(node->parents[i], visit_epoch, topo);
  }
  iris_tape_vec_push(topo, node);
}

static double iris_tape_parent_primal(IrisTapeNode *node, int64_t idx) {
  IrisTapeNode *parent = node->parents[idx];
  return parent ? parent->primal : node->parent_primals[idx];
}

static void iris_tape_accumulate(IrisTapeNode *parent, double delta,
                                 uint64_t grad_epoch) {
  if (!iris_is_tape_node(parent))
    return;
  if (parent->grad_epoch != grad_epoch) {
    parent->grad_epoch = grad_epoch;
    parent->grad = 0.0;
  }
  parent->grad += delta;
}

void *iris_tape_record(double value, const char *op, int64_t parent_count,
                       void *const *parents, const double *parent_primals) {
  // A leaf is recorded with op "leaf" by the lowerer (ensure_taped_leaf), and
  // is the only kind of node whose handle outlives the backward pass.
  int is_leaf = (op && strcmp(op, "leaf") == 0);
  IrisTapeNode *node = iris_tape_alloc_node(is_leaf);
  if (!node)
    return NULL;
  node->magic = IRIS_TAPE_MAGIC;
  node->primal = value;
  node->op = op ? op : "";
  node->parent_count =
      parent_count > 0 ? (parent_count > 2 ? 2 : parent_count) : 0;
  node->grad = 0.0;
  node->grad_epoch = 0;
  node->visit_epoch = 0;

  for (int64_t i = 0; i < node->parent_count; i++) {
    const void *parent = parents ? parents[i] : NULL;
    node->parents[i] =
        iris_is_tape_node(parent) ? (IrisTapeNode *)parent : NULL;
    if (parent_primals) {
      node->parent_primals[i] = parent_primals[i];
    } else if (node->parents[i]) {
      node->parent_primals[i] = node->parents[i]->primal;
    } else {
      node->parent_primals[i] = 0.0;
    }
  }

  return node;
}

void iris_backward(void *loss) {
  IrisTapeNode *loss_node =
      iris_is_tape_node(loss) ? (IrisTapeNode *)loss : NULL;
  if (!loss_node)
    return;

  // Size the topological buffer to the tape actually in use, not to the
  // ceiling. A thread that recorded 300 nodes needs one chunk's worth, not
  // 1 MB.
  size_t needed =
      (iris_tape_chunk_count + iris_leaf_chunk_count) * IRIS_TAPE_CHUNK_NODES;
  if (needed == 0)
    return;
  if (iris_topo_buffer_cap < needed) {
    IrisTapeNode **grown = (IrisTapeNode **)realloc(
        iris_topo_buffer, needed * sizeof(IrisTapeNode *));
    if (!grown)
      return;
    iris_tape_tls_arm();
    iris_topo_buffer = grown;
    iris_topo_buffer_cap = needed;
  }

  IrisTapeVec topo;
  topo.data = iris_topo_buffer;
  topo.len = 0;
  topo.cap = iris_topo_buffer_cap;

  uint64_t visit_epoch = iris_tape_next_epoch(&iris_tape_visit_epoch);
  uint64_t grad_epoch = iris_tape_next_epoch(&iris_tape_grad_epoch);

  iris_tape_collect_topo(loss_node, visit_epoch, &topo);
  for (size_t i = 0; i < topo.len; i++) {
    topo.data[i]->grad = 0.0;
    topo.data[i]->grad_epoch = grad_epoch;
  }
  loss_node->grad = 1.0;

  for (size_t i = topo.len; i > 0; i--) {
    IrisTapeNode *node = topo.data[i - 1];
    double grad = node->grad_epoch == grad_epoch ? node->grad : 0.0;
    if (!node->op)
      continue;

    if (strcmp(node->op, "add") == 0) {
      for (int64_t p = 0; p < node->parent_count; p++) {
        iris_tape_accumulate(node->parents[p], grad, grad_epoch);
      }
    } else if (strcmp(node->op, "sub") == 0) {
      if (node->parent_count >= 1)
        iris_tape_accumulate(node->parents[0], grad, grad_epoch);
      if (node->parent_count >= 2)
        iris_tape_accumulate(node->parents[1], -grad, grad_epoch);
    } else if (strcmp(node->op, "mul") == 0) {
      if (node->parent_count >= 2) {
        double a = iris_tape_parent_primal(node, 0);
        double b = iris_tape_parent_primal(node, 1);
        iris_tape_accumulate(node->parents[0], grad * b, grad_epoch);
        iris_tape_accumulate(node->parents[1], grad * a, grad_epoch);
      }
    } else if (strcmp(node->op, "div") == 0) {
      if (node->parent_count >= 2) {
        double a = iris_tape_parent_primal(node, 0);
        double b = iris_tape_parent_primal(node, 1);
        iris_tape_accumulate(node->parents[0], grad / b, grad_epoch);
        iris_tape_accumulate(node->parents[1], -grad * a / (b * b), grad_epoch);
      }
    } else if (strcmp(node->op, "neg") == 0) {
      if (node->parent_count >= 1)
        iris_tape_accumulate(node->parents[0], -grad, grad_epoch);
    } else if (strcmp(node->op, "sin") == 0) {
      if (node->parent_count >= 1) {
        double x = iris_tape_parent_primal(node, 0);
        iris_tape_accumulate(node->parents[0], grad * cos(x), grad_epoch);
      }
    } else if (strcmp(node->op, "cos") == 0) {
      if (node->parent_count >= 1) {
        double x = iris_tape_parent_primal(node, 0);
        iris_tape_accumulate(node->parents[0], -grad * sin(x), grad_epoch);
      }
    } else if (strcmp(node->op, "exp") == 0) {
      if (node->parent_count >= 1) {
        double x = iris_tape_parent_primal(node, 0);
        iris_tape_accumulate(node->parents[0], grad * exp(x), grad_epoch);
      }
    } else if (strcmp(node->op, "log") == 0) {
      if (node->parent_count >= 1) {
        double x = iris_tape_parent_primal(node, 0);
        iris_tape_accumulate(node->parents[0], grad / x, grad_epoch);
      }
    } else if (strcmp(node->op, "sqrt") == 0) {
      if (node->parent_count >= 1) {
        double x = iris_tape_parent_primal(node, 0);
        iris_tape_accumulate(node->parents[0], grad / (2.0 * sqrt(x)),
                             grad_epoch);
      }
    } else if (strcmp(node->op, "relu") == 0) {
      if (node->parent_count >= 1) {
        double x = iris_tape_parent_primal(node, 0);
        iris_tape_accumulate(node->parents[0], x > 0.0 ? grad : 0.0,
                             grad_epoch);
      }
    } else if (strcmp(node->op, "sigmoid") == 0) {
      if (node->parent_count >= 1) {
        double x = iris_tape_parent_primal(node, 0);
        double s = 1.0 / (1.0 + exp(-x));
        iris_tape_accumulate(node->parents[0], grad * s * (1.0 - s),
                             grad_epoch);
      }
    } else if (strcmp(node->op, "tanh") == 0) {
      if (node->parent_count >= 1) {
        double x = iris_tape_parent_primal(node, 0);
        double t = tanh(x);
        iris_tape_accumulate(node->parents[0], grad * (1.0 - t * t),
                             grad_epoch);
      }
    } else if (strcmp(node->op, "pow") == 0) {
      if (node->parent_count >= 2) {
        double base = iris_tape_parent_primal(node, 0);
        double exponent = iris_tape_parent_primal(node, 1);
        iris_tape_accumulate(node->parents[0],
                             grad * exponent * pow(base, exponent - 1.0),
                             grad_epoch);
        iris_tape_accumulate(node->parents[1],
                             grad * pow(base, exponent) * log(base),
                             grad_epoch);
      }
    } else if (strcmp(node->op, "abs") == 0) {
      if (node->parent_count >= 1) {
        double x = iris_tape_parent_primal(node, 0);
        iris_tape_accumulate(node->parents[0], grad * (x >= 0.0 ? 1.0 : -1.0),
                             grad_epoch);
      }
    }
  }

  // Recycle the intermediates for the next training step. The leaf region is
  // deliberately untouched: the program still holds handles to those nodes
  // and reads gradients from them after this returns. Recycling them was #48.
  iris_tape_arena_index = 0;
}

double iris_tape_grad(void *node) {
  IrisTapeNode *tape_node =
      iris_is_tape_node(node) ? (IrisTapeNode *)node : NULL;
  if (!tape_node || tape_node->grad_epoch != iris_tape_grad_epoch)
    return 0.0;
  return tape_node->grad;
}

// ---------------------------------------------------------------------------
// Non-scalar array fallback (for complex / mixed-type arrays)
// ---------------------------------------------------------------------------

IrisList *iris_alloc_array(void) { return iris_list_new(); }
IrisVal *iris_array_load(IrisList *arr, int64_t idx) {
  return iris_list_get(arr, idx);
}
void iris_array_store(IrisList *arr, int64_t idx, IrisVal *val) {
  iris_list_set(arr, idx, val);
}

// ---------------------------------------------------------------------------
// Tensor ops — real compute (replacing shape-tracking stubs)
// ---------------------------------------------------------------------------

// The element a tensor index names: with every dimension indexed, that
// element; with one index, the element at that flat row-major position.
// Anything else, and an index out of range, stops the program (E0403).
// The `n` indices are the `int64_t` variadic arguments in `ap`.
static int64_t iris_tensor_offset(IrisTensor *t, int32_t n, va_list ap) {
  if (!t)
    iris_runtime_error("E0404", "indexing a tensor that does not exist");
  if (n == 0) {
    if (t->numel < 1)
      iris_runtime_error("E0403", "index 0 is out of bounds for length 0");
    return 0;
  }
  if (n == 1) {
    int64_t i = va_arg(ap, int64_t);
    if (i < 0 || i >= t->numel)
      iris_runtime_error("E0403", "index %lld is out of bounds for length %lld",
                         (long long)i, (long long)t->numel);
    return i;
  }
  if (n != t->ndim)
    iris_runtime_error("E0403",
                       "a tensor of rank %d takes %d indices, or one flat "
                       "index, but %d were given",
                       (int)t->ndim, (int)t->ndim, (int)n);
  int64_t off = 0;
  for (int32_t d = 0; d < n; d++) {
    int64_t i = va_arg(ap, int64_t);
    if (i < 0 || i >= t->shape[d])
      iris_runtime_error(
          "E0403",
          "index %lld is out of bounds for dimension %d of length %lld",
          (long long)i, (int)d, (long long)t->shape[d]);
    off = off * t->shape[d] + i;
  }
  return off;
}

/* Called by generated code for `t[i, j, ...]`: `n` indices follow, each an
 * `int64_t`. */
double iris_tensor_get_at(IrisTensor *t, int32_t n, ...) {
  va_list ap;
  va_start(ap, n);
  int64_t off = iris_tensor_offset(t, n, ap);
  va_end(ap);
  return (double)t->data[off];
}

/* `t[i, j, ...] = v`. */
void iris_tensor_set_at(IrisTensor *t, double v, int32_t n, ...) {
  va_list ap;
  va_start(ap, n);
  int64_t off = iris_tensor_offset(t, n, ap);
  va_end(ap);
  t->data[off] = (float)v;
}

// --- Allocation / lifecycle ------------------------------------------------

typedef struct IrisMemBlock {
  void *ptr;
  size_t size;
  int in_use;
  struct IrisMemBlock *next;
} IrisMemBlock;

static IrisMemBlock *tensor_pool_head = NULL;
static int tensor_pool_enabled = 0;
static size_t tensor_pool_max_bytes = 0;
static pthread_mutex_t tensor_pool_mu = PTHREAD_MUTEX_INITIALIZER;

int64_t iris_tensor_pool_init(int64_t limit_bytes) {
  pthread_mutex_lock(&tensor_pool_mu);
  if (!tensor_pool_enabled) {
    tensor_pool_enabled = 1;
    tensor_pool_max_bytes = (size_t)limit_bytes;
    tensor_pool_head = NULL;
  }
  pthread_mutex_unlock(&tensor_pool_mu);
  return 0;
}

int64_t iris_tensor_pool_destroy(void) {
  pthread_mutex_lock(&tensor_pool_mu);
  if (tensor_pool_enabled) {
    IrisMemBlock *curr = tensor_pool_head;
    while (curr) {
      IrisMemBlock *next = curr->next;
      free(curr->ptr);
      free(curr);
      curr = next;
    }
    tensor_pool_head = NULL;
    tensor_pool_enabled = 0;
    tensor_pool_max_bytes = 0;
  }
  pthread_mutex_unlock(&tensor_pool_mu);
  return 0;
}

IrisTensor *iris_tensor_alloc(int32_t ndim, const int64_t *shape) {
  IrisTensor *t = xmalloc(sizeof(IrisTensor));
  memset(t, 0, sizeof(*t));
  t->ndim = ndim;
  t->shape = xmalloc(ndim * sizeof(int64_t));
  t->numel = 1;
  for (int32_t i = 0; i < ndim; i++) {
    if (shape[i] < 0) {
      fprintf(stderr, "IRIS ML: Negative shape dimension\n");
      iris_fatal();
    }
    if (shape[i] > 0 && t->numel > (INT64_MAX / shape[i])) {
      fprintf(stderr, "IRIS ML: Tensor shape overflow\n");
      iris_fatal();
    }
    t->shape[i] = shape[i];
    t->numel *= shape[i];
  }

  if ((uint64_t)t->numel > (uint64_t)(SIZE_MAX / sizeof(float))) {
    fprintf(stderr, "IRIS ML: Tensor too large\n");
    iris_fatal();
  }
  size_t req_bytes = (size_t)t->numel * sizeof(float);

  pthread_mutex_lock(&tensor_pool_mu);
  if (tensor_pool_enabled) {
    IrisMemBlock *best_block = NULL;
    IrisMemBlock *curr = tensor_pool_head;
    while (curr) {
      if (!curr->in_use && curr->size >= req_bytes) {
        if (!best_block || curr->size < best_block->size) {
          best_block = curr;
        }
      }
      curr = curr->next;
    }

    if (best_block) {
      best_block->in_use = 1;
      t->data = best_block->ptr;
      pthread_mutex_unlock(&tensor_pool_mu);
      return t;
    }

    if (tensor_pool_max_bytes > 0) {
      size_t total_cached = 0;
      IrisMemBlock *p = tensor_pool_head;
      while (p) {
        total_cached += p->size;
        p = p->next;
      }

      while (total_cached + req_bytes > tensor_pool_max_bytes) {
        IrisMemBlock *prev = NULL;
        IrisMemBlock *curr_evict = tensor_pool_head;
        IrisMemBlock *to_evict = NULL;
        IrisMemBlock *to_evict_prev = NULL;

        while (curr_evict) {
          if (!curr_evict->in_use) {
            to_evict = curr_evict;
            to_evict_prev = prev;
            break;
          }
          prev = curr_evict;
          curr_evict = curr_evict->next;
        }

        if (to_evict) {
          if (to_evict_prev) {
            to_evict_prev->next = to_evict->next;
          } else {
            tensor_pool_head = to_evict->next;
          }
          total_cached -= to_evict->size;
          free(to_evict->ptr);
          free(to_evict);
        } else {
          break;
        }
      }
    }

    void *ptr = malloc(req_bytes);
    if (!ptr) {
      fprintf(stderr, "iris: tensor pool out of memory\n");
      iris_fatal();
    }

    IrisMemBlock *block = malloc(sizeof(IrisMemBlock));
    if (!block) {
      free(ptr);
      fprintf(stderr, "iris: tensor pool out of memory\n");
      iris_fatal();
    }
    block->ptr = ptr;
    block->size = req_bytes;
    block->in_use = 1;
    block->next = tensor_pool_head;
    tensor_pool_head = block;

    t->data = ptr;
    pthread_mutex_unlock(&tensor_pool_mu);
    return t;
  } else {
    pthread_mutex_unlock(&tensor_pool_mu);
    t->data = xmalloc(req_bytes);
    return t;
  }
}

void iris_tensor_free(IrisTensor *t) {
  if (!t)
    return;

  pthread_mutex_lock(&tensor_pool_mu);
  if (tensor_pool_enabled) {
    IrisMemBlock *curr = tensor_pool_head;
    while (curr) {
      if (curr->ptr == t->data) {
        curr->in_use = 0;
        break;
      }
      curr = curr->next;
    }

    if (curr) {
      pthread_mutex_unlock(&tensor_pool_mu);
    } else {
      pthread_mutex_unlock(&tensor_pool_mu);
      xfree(t->data);
    }
  } else {
    pthread_mutex_unlock(&tensor_pool_mu);
    xfree(t->data);
  }

  xfree(t->shape);
  xfree(t->grad);
  xfree(t);
}

IrisTensor *iris_tensor_zeros(int32_t ndim, const int64_t *shape) {
  IrisTensor *t = iris_tensor_alloc(ndim, shape);
  memset(t->data, 0, t->numel * sizeof(float));
  return t;
}

IrisTensor *iris_tensor_fill(int32_t ndim, const int64_t *shape, float val) {
  IrisTensor *t = iris_tensor_alloc(ndim, shape);
  for (int64_t i = 0; i < t->numel; i++)
    t->data[i] = val;
  return t;
}

// --- Element access --------------------------------------------------------

float iris_tensor_get(IrisTensor *t, int64_t flat_idx) {
  if (!t || flat_idx < 0 || flat_idx >= t->numel)
    return 0.0f;
  return t->data[flat_idx];
}

void iris_tensor_set(IrisTensor *t, int64_t flat_idx, float val) {
  if (!t || flat_idx < 0 || flat_idx >= t->numel)
    return;
  t->data[flat_idx] = val;
}

enum {
  IRIS_TAD_NONE = 0,
  IRIS_TAD_LEAF,
  IRIS_TAD_ADD,
  IRIS_TAD_SUB,
  IRIS_TAD_MUL,
  IRIS_TAD_DIV,
  IRIS_TAD_MATMUL,
  IRIS_TAD_NEG,
  IRIS_TAD_RELU,
  IRIS_TAD_SIGMOID,
  IRIS_TAD_TANH,
  IRIS_TAD_EXP,
  IRIS_TAD_LOG,
  IRIS_TAD_SQRT,
  IRIS_TAD_ABS,
  IRIS_TAD_SUM_ALL
};

static void iris_tensor_ad_attach(IrisTensor *out, int32_t op,
                                  IrisTensor *parent0, IrisTensor *parent1) {
  if (!out)
    return;
  if ((parent0 && parent0->requires_grad) ||
      (parent1 && parent1->requires_grad)) {
    out->requires_grad = 1;
    out->ad_op = op;
    out->ad_parent0 = parent0;
    out->ad_parent1 = parent1;
  }
}

// --- Matrix multiplication -------------------------------------------------
// Supports 2D matmul: (M,K) @ (K,N) -> (M,N)

IrisTensor *iris_tensor_matmul(IrisTensor *a, IrisTensor *b) {
  if (!a || !b || a->ndim < 2 || b->ndim < 2)
    return NULL;
  int64_t m = a->shape[a->ndim - 2];
  int64_t k = a->shape[a->ndim - 1];
  int64_t n = b->shape[b->ndim - 1];
  if (b->shape[b->ndim - 2] != k)
    return NULL;

  int64_t out_shape[2] = {m, n};
  IrisTensor *out = iris_tensor_zeros(2, out_shape);

  for (int64_t i = 0; i < m; i++) {
    for (int64_t l = 0; l < k; l++) {
      float a_il = a->data[i * k + l];
      for (int64_t j = 0; j < n; j++) {
        out->data[i * n + j] += a_il * b->data[l * n + j];
      }
    }
  }
  iris_tensor_ad_attach(out, IRIS_TAD_MATMUL, a, b);
  return out;
}

// --- Element-wise binary ops -----------------------------------------------

static IrisTensor *tensor_binop(IrisTensor *a, IrisTensor *b, int op) {
  if (!a || !b)
    iris_runtime_error(
        "E0404",
        "an element-wise tensor operation on a tensor that does not exist");
  if (a->numel != b->numel)
    iris_runtime_error("E0404", "tensor sizes differ: %lld and %lld elements",
                       (long long)a->numel, (long long)b->numel);
  IrisTensor *out = iris_tensor_alloc(a->ndim, a->shape);
  for (int64_t i = 0; i < a->numel; i++) {
    float x = a->data[i], y = b->data[i];
    switch (op) {
    case 0:
      out->data[i] = x + y;
      break;
    case 1:
      out->data[i] = x - y;
      break;
    case 2:
      out->data[i] = x * y;
      break;
    case 3:
      out->data[i] = (y != 0.0f) ? x / y : 0.0f;
      break;
    default:
      out->data[i] = 0.0f;
      break;
    }
  }
  iris_tensor_ad_attach(out, IRIS_TAD_ADD + op, a, b);
  return out;
}

IrisTensor *iris_tensor_add(IrisTensor *a, IrisTensor *b) {
  return tensor_binop(a, b, 0);
}
IrisTensor *iris_tensor_sub(IrisTensor *a, IrisTensor *b) {
  return tensor_binop(a, b, 1);
}
IrisTensor *iris_tensor_mul(IrisTensor *a, IrisTensor *b) {
  return tensor_binop(a, b, 2);
}
IrisTensor *iris_tensor_div(IrisTensor *a, IrisTensor *b) {
  return tensor_binop(a, b, 3);
}

// --- Element-wise unary ops ------------------------------------------------

static IrisTensor *tensor_unary(IrisTensor *t, int op) {
  if (!t)
    return NULL;
  IrisTensor *out = iris_tensor_alloc(t->ndim, t->shape);
  for (int64_t i = 0; i < t->numel; i++) {
    float x = t->data[i];
    switch (op) {
    case 0:
      out->data[i] = -x;
      break; // neg
    case 1:
      out->data[i] = x > 0.0f ? x : 0.0f;
      break; // relu
    case 2:
      out->data[i] = 1.0f / (1.0f + expf(-x));
      break; // sigmoid
    case 3:
      out->data[i] = tanhf(x);
      break; // tanh
    case 4:
      out->data[i] = expf(x);
      break; // exp
    case 5:
      out->data[i] = (x > 0.0f) ? logf(x) : -INFINITY;
      break; // log
    case 6:
      out->data[i] = (x >= 0.0f) ? sqrtf(x) : 0.0f;
      break; // sqrt
    case 7:
      out->data[i] = fabsf(x);
      break; // abs
    default:
      out->data[i] = x;
      break;
    }
  }
  iris_tensor_ad_attach(out, IRIS_TAD_NEG + op, t, NULL);
  return out;
}

IrisTensor *iris_tensor_neg(IrisTensor *t) { return tensor_unary(t, 0); }
IrisTensor *iris_tensor_relu(IrisTensor *t) { return tensor_unary(t, 1); }
IrisTensor *iris_tensor_sigmoid(IrisTensor *t) { return tensor_unary(t, 2); }
IrisTensor *iris_tensor_tanh_act(IrisTensor *t) { return tensor_unary(t, 3); }
IrisTensor *iris_tensor_exp(IrisTensor *t) { return tensor_unary(t, 4); }
IrisTensor *iris_tensor_log(IrisTensor *t) { return tensor_unary(t, 5); }
IrisTensor *iris_tensor_sqrt(IrisTensor *t) { return tensor_unary(t, 6); }
IrisTensor *iris_tensor_abs(IrisTensor *t) { return tensor_unary(t, 7); }

// --- Reshape ---------------------------------------------------------------

IrisTensor *iris_tensor_reshape(IrisTensor *t, int32_t new_ndim,
                                const int64_t *new_shape) {
  if (!t)
    return NULL;
  int64_t new_numel = 1;
  for (int32_t i = 0; i < new_ndim; i++)
    new_numel *= new_shape[i];
  if (new_numel != t->numel)
    iris_runtime_error("E0404", "tensor sizes differ: %lld and %lld elements",
                       (long long)t->numel, (long long)new_numel);

  IrisTensor *out = xmalloc(sizeof(IrisTensor));
  memset(out, 0, sizeof(*out));
  out->ndim = new_ndim;
  out->numel = new_numel;
  out->shape = xmalloc(new_ndim * sizeof(int64_t));
  memcpy(out->shape, new_shape, new_ndim * sizeof(int64_t));
  // Share a copy of the data (reshape is zero-copy semantically, but we copy
  // for safety)
  out->data = xmalloc(out->numel * sizeof(float));
  memcpy(out->data, t->data, out->numel * sizeof(float));
  return out;
}

// --- Transpose -------------------------------------------------------------
// General N-dim transpose with explicit axis permutation.

IrisTensor *iris_tensor_transpose(IrisTensor *t, const int32_t *axes) {
  if (!t)
    return NULL;
  int32_t ndim = t->ndim;

  // Compute new shape
  int64_t *new_shape = xmalloc(ndim * sizeof(int64_t));
  for (int32_t i = 0; i < ndim; i++)
    new_shape[i] = t->shape[axes[i]];

  IrisTensor *out = iris_tensor_alloc(ndim, new_shape);

  // Compute strides for source tensor
  int64_t *src_strides = xmalloc(ndim * sizeof(int64_t));
  src_strides[ndim - 1] = 1;
  for (int32_t i = ndim - 2; i >= 0; i--)
    src_strides[i] = src_strides[i + 1] * t->shape[i + 1];

  // Compute strides for destination tensor
  int64_t *dst_strides = xmalloc(ndim * sizeof(int64_t));
  dst_strides[ndim - 1] = 1;
  for (int32_t i = ndim - 2; i >= 0; i--)
    dst_strides[i] = dst_strides[i + 1] * new_shape[i + 1];

  // Iterate over all elements using N-digit counter
  int64_t *coords = xcalloc(ndim, sizeof(int64_t));
  for (int64_t flat = 0; flat < t->numel; flat++) {
    // Compute source multi-index
    int64_t rem = flat;
    for (int32_t d = 0; d < ndim; d++) {
      coords[d] = rem / src_strides[d];
      rem %= src_strides[d];
    }
    // Compute destination flat index by permuting coords
    int64_t dst_flat = 0;
    for (int32_t d = 0; d < ndim; d++) {
      dst_flat += coords[axes[d]] * dst_strides[d];
    }
    out->data[dst_flat] = t->data[flat];
  }

  xfree(coords);
  xfree(src_strides);
  xfree(dst_strides);
  xfree(new_shape);
  return out;
}

// --- Reductions ------------------------------------------------------------
// Reduce along a single axis with sum/max/mean.

static IrisTensor *tensor_reduce(IrisTensor *t, int32_t axis, int keepdims,
                                 int op) {
  if (!t || axis < 0 || axis >= t->ndim)
    return NULL;

  int32_t ndim = t->ndim;
  int64_t axis_len = t->shape[axis];
  if (axis_len == 0)
    return NULL;

  // Compute output shape
  int32_t out_ndim = keepdims ? ndim : ndim - 1;
  if (out_ndim == 0)
    out_ndim = 1; // scalar result as 1D [1]
  int64_t *out_shape = xmalloc(out_ndim * sizeof(int64_t));
  int32_t j = 0;
  for (int32_t i = 0; i < ndim; i++) {
    if (i == axis) {
      if (keepdims)
        out_shape[j++] = 1;
    } else {
      out_shape[j++] = t->shape[i];
    }
  }
  if (j == 0) {
    out_shape[0] = 1;
    j = 1;
  }

  IrisTensor *out = iris_tensor_zeros(out_ndim, out_shape);

  // Compute strides
  int64_t *strides = xmalloc(ndim * sizeof(int64_t));
  strides[ndim - 1] = 1;
  for (int32_t i = ndim - 2; i >= 0; i--)
    strides[i] = strides[i + 1] * t->shape[i + 1];

  int64_t outer_size = 1, inner_size = 1;
  for (int32_t i = 0; i < axis; i++)
    outer_size *= t->shape[i];
  for (int32_t i = axis + 1; i < ndim; i++)
    inner_size *= t->shape[i];

  // Initialize max to -inf if needed
  if (op == 1) { // max
    for (int64_t i = 0; i < out->numel; i++)
      out->data[i] = -INFINITY;
  }

  for (int64_t o = 0; o < outer_size; o++) {
    for (int64_t a = 0; a < axis_len; a++) {
      for (int64_t inn = 0; inn < inner_size; inn++) {
        int64_t src_idx = o * (axis_len * inner_size) + a * inner_size + inn;
        int64_t dst_idx = o * inner_size + inn;
        float v = t->data[src_idx];
        switch (op) {
        case 0:
          out->data[dst_idx] += v;
          break; // sum
        case 1:
          if (v > out->data[dst_idx])
            out->data[dst_idx] = v;
          break; // max
        case 2:
          out->data[dst_idx] += v;
          break; // mean (sum first, divide later)
        default:
          break;
        }
      }
    }
  }

  // For mean, divide by axis length
  if (op == 2) {
    for (int64_t i = 0; i < out->numel; i++)
      out->data[i] /= (float)axis_len;
  }

  xfree(strides);
  xfree(out_shape);
  return out;
}

IrisTensor *iris_tensor_reduce_sum(IrisTensor *t, int32_t axis, int keepdims) {
  return tensor_reduce(t, axis, keepdims, 0);
}
IrisTensor *iris_tensor_reduce_max(IrisTensor *t, int32_t axis, int keepdims) {
  return tensor_reduce(t, axis, keepdims, 1);
}
IrisTensor *iris_tensor_reduce_mean(IrisTensor *t, int32_t axis, int keepdims) {
  return tensor_reduce(t, axis, keepdims, 2);
}

IrisTensor *iris_tensor_reduce_multi(IrisTensor *t, const char *op,
                                     int32_t n_axes, const int32_t *axes,
                                     int32_t keepdims) {
  if (!t || t->numel == 0)
    return NULL;
  int op_code = 0;
  if (op) {
    if (strcmp(op, "max") == 0)
      op_code = 1;
    else if (strcmp(op, "mean") == 0)
      op_code = 2;
  }
  if (n_axes == 1) {
    return tensor_reduce(t, axes[0], keepdims, op_code);
  }
  int32_t ndim = t->ndim;
  int *to_reduce = xcalloc((size_t)ndim, sizeof(int));
  if (n_axes <= 0) {
    for (int32_t i = 0; i < ndim; i++)
      to_reduce[i] = 1;
  } else {
    for (int32_t i = 0; i < n_axes; i++) {
      int32_t ax = axes[i];
      if (ax < 0)
        ax += ndim;
      if (ax >= 0 && ax < ndim)
        to_reduce[ax] = 1;
    }
  }

  IrisTensor *curr = t;
  int need_free = 0;
  for (int32_t ax = ndim - 1; ax >= 0; ax--) {
    if (to_reduce[ax]) {
      IrisTensor *next = tensor_reduce(curr, ax, 1, op_code);
      if (need_free)
        iris_tensor_free(curr);
      curr = next;
      need_free = 1;
    }
  }
  xfree(to_reduce);

  if (!curr)
    return NULL;

  if (!keepdims) {
    int32_t non_reduced_count = 0;
    for (int32_t i = 0; i < ndim; i++) {
      int is_red = 0;
      if (n_axes <= 0)
        is_red = 1;
      else {
        for (int32_t k = 0; k < n_axes; k++) {
          int32_t ax = axes[k];
          if (ax < 0)
            ax += ndim;
          if (ax == i) {
            is_red = 1;
            break;
          }
        }
      }
      if (!is_red)
        non_reduced_count++;
    }
    if (non_reduced_count == 0) {
      int64_t one_shape[1] = {1};
      IrisTensor *squeezed = iris_tensor_alloc(1, one_shape);
      squeezed->data[0] = curr->data[0];
      if (need_free)
        iris_tensor_free(curr);
      return squeezed;
    } else {
      int64_t *sq_shape = xmalloc((size_t)non_reduced_count * sizeof(int64_t));
      int32_t idx = 0;
      for (int32_t i = 0; i < ndim; i++) {
        int is_red = 0;
        if (n_axes <= 0)
          is_red = 1;
        else {
          for (int32_t k = 0; k < n_axes; k++) {
            int32_t ax = axes[k];
            if (ax < 0)
              ax += ndim;
            if (ax == i) {
              is_red = 1;
              break;
            }
          }
        }
        if (!is_red)
          sq_shape[idx++] = curr->shape[i];
      }
      IrisTensor *squeezed = iris_tensor_alloc(non_reduced_count, sq_shape);
      memcpy(squeezed->data, curr->data, (size_t)curr->numel * sizeof(float));
      xfree(sq_shape);
      if (need_free)
        iris_tensor_free(curr);
      return squeezed;
    }
  }

  return curr;
}

IrisTensor *iris_tensor_einsum(const char *notation, IrisTensor *a,
                               IrisTensor *b) {
  if (!notation || !a || !b)
    return NULL;
  const char *arrow = strstr(notation, "->");
  if (!arrow)
    return NULL;

  const char *comma = strchr(notation, ',');
  if (!comma || comma > arrow)
    return NULL;

  size_t lhs_len = (size_t)(comma - notation);
  char lhs[32] = {0};
  if (lhs_len >= sizeof(lhs))
    lhs_len = sizeof(lhs) - 1;
  strncpy(lhs, notation, lhs_len);

  size_t rhs_len = (size_t)(arrow - (comma + 1));
  char rhs[32] = {0};
  if (rhs_len >= sizeof(rhs))
    rhs_len = sizeof(rhs) - 1;
  strncpy(rhs, comma + 1, rhs_len);

  const char *out_str = arrow + 2;
  size_t out_len = strlen(out_str);

  int64_t dim_map[256] = {0};
  for (size_t i = 0; i < lhs_len && (int64_t)i < a->ndim; i++) {
    unsigned char c = (unsigned char)lhs[i];
    dim_map[c] = a->shape[i];
  }
  for (size_t i = 0; i < rhs_len && (int64_t)i < b->ndim; i++) {
    unsigned char c = (unsigned char)rhs[i];
    if (dim_map[c] != 0 && dim_map[c] != b->shape[i]) {
      return NULL;
    }
    dim_map[c] = b->shape[i];
  }

  int32_t out_ndim = (out_len == 0) ? 1 : (int32_t)out_len;
  int64_t *out_shape = xmalloc((size_t)out_ndim * sizeof(int64_t));
  if (out_len == 0) {
    out_shape[0] = 1;
  } else {
    for (size_t i = 0; i < out_len; i++) {
      unsigned char c = (unsigned char)out_str[i];
      out_shape[i] = dim_map[c] > 0 ? dim_map[c] : 1;
    }
  }

  IrisTensor *out = iris_tensor_zeros(out_ndim, out_shape);

  char contracted[32] = {0};
  size_t num_contracted = 0;
  for (size_t i = 0; i < lhs_len; i++) {
    char c = lhs[i];
    if (!strchr(out_str, c) && !strchr(contracted, c)) {
      contracted[num_contracted++] = c;
    }
  }
  for (size_t i = 0; i < rhs_len; i++) {
    char c = rhs[i];
    if (!strchr(out_str, c) && !strchr(contracted, c)) {
      contracted[num_contracted++] = c;
    }
  }

  int64_t contracted_numel = 1;
  int64_t contracted_sizes[32] = {0};
  int64_t contracted_strides[32] = {0};
  for (size_t i = 0; i < num_contracted; i++) {
    unsigned char c = (unsigned char)contracted[i];
    contracted_sizes[i] = dim_map[c] > 0 ? dim_map[c] : 1;
    contracted_numel *= contracted_sizes[i];
  }
  if (num_contracted > 0) {
    contracted_strides[num_contracted - 1] = 1;
    for (int i = (int)num_contracted - 2; i >= 0; i--) {
      contracted_strides[i] =
          contracted_strides[i + 1] * contracted_sizes[i + 1];
    }
  }

  int64_t *a_strides = xmalloc((size_t)a->ndim * sizeof(int64_t));
  a_strides[a->ndim - 1] = 1;
  for (int32_t i = a->ndim - 2; i >= 0; i--)
    a_strides[i] = a_strides[i + 1] * a->shape[i + 1];

  int64_t *b_strides = xmalloc((size_t)b->ndim * sizeof(int64_t));
  b_strides[b->ndim - 1] = 1;
  for (int32_t i = b->ndim - 2; i >= 0; i--)
    b_strides[i] = b_strides[i + 1] * b->shape[i + 1];

  int64_t *out_strides = xmalloc((size_t)out_ndim * sizeof(int64_t));
  out_strides[out_ndim - 1] = 1;
  for (int32_t i = out_ndim - 2; i >= 0; i--)
    out_strides[i] = out_strides[i + 1] * out_shape[i + 1];

  int64_t char_val[256] = {0};
  for (int64_t o_flat = 0; o_flat < out->numel; o_flat++) {
    int64_t rem = o_flat;
    for (size_t d = 0; d < out_len; d++) {
      unsigned char c = (unsigned char)out_str[d];
      char_val[c] = rem / out_strides[d];
      rem %= out_strides[d];
    }

    float acc = 0.0f;
    for (int64_t k_flat = 0; k_flat < contracted_numel; k_flat++) {
      int64_t krem = k_flat;
      for (size_t d = 0; d < num_contracted; d++) {
        unsigned char c = (unsigned char)contracted[d];
        char_val[c] = krem / contracted_strides[d];
        krem %= contracted_strides[d];
      }

      int64_t a_flat = 0;
      for (size_t d = 0; d < lhs_len && (int64_t)d < a->ndim; d++) {
        unsigned char c = (unsigned char)lhs[d];
        a_flat += char_val[c] * a_strides[d];
      }

      int64_t b_flat = 0;
      for (size_t d = 0; d < rhs_len && (int64_t)d < b->ndim; d++) {
        unsigned char c = (unsigned char)rhs[d];
        b_flat += char_val[c] * b_strides[d];
      }

      acc += a->data[a_flat] * b->data[b_flat];
    }
    out->data[o_flat] = acc;
  }

  xfree(a_strides);
  xfree(b_strides);
  xfree(out_strides);
  xfree(out_shape);
  return out;
}

IrisTensor *iris_tensor_einsum_single(const char *notation, IrisTensor *a) {
  if (!notation || !a)
    return NULL;
  const char *arrow = strstr(notation, "->");
  if (!arrow)
    return NULL;

  size_t lhs_len = (size_t)(arrow - notation);
  char lhs[32] = {0};
  if (lhs_len >= sizeof(lhs))
    lhs_len = sizeof(lhs) - 1;
  strncpy(lhs, notation, lhs_len);

  const char *out_str = arrow + 2;
  size_t out_len = strlen(out_str);

  int64_t dim_map[256] = {0};
  for (size_t i = 0; i < lhs_len && (int64_t)i < a->ndim; i++) {
    unsigned char c = (unsigned char)lhs[i];
    dim_map[c] = a->shape[i];
  }

  int32_t out_ndim = (out_len == 0) ? 1 : (int32_t)out_len;
  int64_t *out_shape = xmalloc((size_t)out_ndim * sizeof(int64_t));
  if (out_len == 0) {
    out_shape[0] = 1;
  } else {
    for (size_t i = 0; i < out_len; i++) {
      unsigned char c = (unsigned char)out_str[i];
      out_shape[i] = dim_map[c] > 0 ? dim_map[c] : 1;
    }
  }
  IrisTensor *out = iris_tensor_zeros(out_ndim, out_shape);

  int64_t *a_strides = xmalloc((size_t)a->ndim * sizeof(int64_t));
  a_strides[a->ndim - 1] = 1;
  for (int32_t i = a->ndim - 2; i >= 0; i--)
    a_strides[i] = a_strides[i + 1] * a->shape[i + 1];

  int64_t *out_strides = xmalloc((size_t)out_ndim * sizeof(int64_t));
  out_strides[out_ndim - 1] = 1;
  for (int32_t i = out_ndim - 2; i >= 0; i--)
    out_strides[i] = out_strides[i + 1] * out_shape[i + 1];

  int64_t coords[32] = {0};
  for (int64_t flat = 0; flat < a->numel; flat++) {
    int64_t rem = flat;
    for (int32_t d = 0; d < a->ndim; d++) {
      coords[d] = rem / a_strides[d];
      rem %= a_strides[d];
    }
    int valid = 1;
    for (size_t i = 0; i < lhs_len && (int64_t)i < a->ndim; i++) {
      for (size_t j = i + 1; j < lhs_len && (int64_t)j < a->ndim; j++) {
        if (lhs[i] == lhs[j] && coords[i] != coords[j]) {
          valid = 0;
          break;
        }
      }
      if (!valid)
        break;
    }
    if (!valid)
      continue;

    int64_t dst_flat = 0;
    if (out_len > 0) {
      for (size_t d = 0; d < out_len; d++) {
        char oc = out_str[d];
        for (size_t s = 0; s < lhs_len && (int64_t)s < a->ndim; s++) {
          if (lhs[s] == oc) {
            dst_flat += coords[s] * out_strides[d];
            break;
          }
        }
      }
    }
    out->data[dst_flat] += a->data[flat];
  }

  xfree(a_strides);
  xfree(out_strides);
  xfree(out_shape);
  return out;
}

// --- Define-by-run reverse-mode tensor autodiff ----------------------------

IrisTensor *iris_tensor_from_lists(IrisList *data, IrisList *shape) {
  if (!data || !shape || shape->len == 0)
    return NULL;
  int32_t ndim = (int32_t)shape->len;
  int64_t *dims = xmalloc((size_t)ndim * sizeof(int64_t));
  for (int32_t i = 0; i < ndim; i++)
    dims[i] = iris_unbox_i64(shape->data[i]);
  IrisTensor *tensor = iris_tensor_zeros(ndim, dims);
  xfree(dims);
  if (!tensor || tensor->numel != (int64_t)data->len) {
    if (tensor)
      iris_tensor_free(tensor);
    return NULL;
  }
  for (int64_t i = 0; i < tensor->numel; i++)
    tensor->data[i] = (float)iris_unbox_f64(data->data[i]);
  return tensor;
}

IrisList *iris_tensor_to_list(IrisTensor *tensor) {
  IrisList *values = iris_list_new();
  if (!tensor)
    return values;
  for (int64_t i = 0; i < tensor->numel; i++)
    iris_list_push(values, iris_box_f64((double)tensor->data[i]));
  return values;
}

IrisTensor *iris_tensor_tape(IrisTensor *tensor) {
  if (!tensor)
    return NULL;
  tensor->requires_grad = 1;
  tensor->ad_op = IRIS_TAD_LEAF;
  tensor->ad_parent0 = tensor->ad_parent1 = NULL;
  return tensor;
}

IrisTensor *iris_tensor_sum_all(IrisTensor *tensor) {
  if (!tensor)
    return NULL;
  int64_t shape[1] = {1};
  IrisTensor *out = iris_tensor_zeros(1, shape);
  for (int64_t i = 0; i < tensor->numel; i++)
    out->data[0] += tensor->data[i];
  iris_tensor_ad_attach(out, IRIS_TAD_SUM_ALL, tensor, NULL);
  return out;
}

typedef struct {
  IrisTensor **data;
  size_t len;
  size_t cap;
} IrisTensorTopo;

static int iris_tensor_topo_contains(IrisTensorTopo *topo, IrisTensor *tensor) {
  for (size_t i = 0; i < topo->len; i++)
    if (topo->data[i] == tensor)
      return 1;
  return 0;
}

static void iris_tensor_topo_collect(IrisTensor *tensor, IrisTensorTopo *topo) {
  if (!tensor || !tensor->requires_grad ||
      iris_tensor_topo_contains(topo, tensor))
    return;
  iris_tensor_topo_collect(tensor->ad_parent0, topo);
  iris_tensor_topo_collect(tensor->ad_parent1, topo);
  if (topo->len == topo->cap) {
    topo->cap = topo->cap ? topo->cap * 2 : 32;
    topo->data = xrealloc(topo->data, topo->cap * sizeof(IrisTensor *));
  }
  topo->data[topo->len++] = tensor;
}

static void iris_tensor_grad_add(IrisTensor *tensor, int64_t index,
                                 float value) {
  if (!tensor || !tensor->requires_grad || index < 0 || index >= tensor->numel)
    return;
  if (!tensor->grad)
    tensor->grad = xcalloc((size_t)tensor->numel, sizeof(float));
  tensor->grad[index] += value;
}

int64_t iris_tensor_backward(IrisTensor *loss) {
  if (!loss || !loss->requires_grad)
    return 0;
  IrisTensorTopo topo = {0};
  iris_tensor_topo_collect(loss, &topo);
  for (size_t i = 0; i < topo.len; i++) {
    xfree(topo.data[i]->grad);
    topo.data[i]->grad = xcalloc((size_t)topo.data[i]->numel, sizeof(float));
  }
  for (int64_t i = 0; i < loss->numel; i++)
    loss->grad[i] = 1.0f;

  for (size_t pos = topo.len; pos-- > 0;) {
    IrisTensor *node = topo.data[pos];
    IrisTensor *a = node->ad_parent0;
    IrisTensor *b = node->ad_parent1;
    if (!node->grad)
      continue;
    switch (node->ad_op) {
    case IRIS_TAD_ADD:
    case IRIS_TAD_SUB:
    case IRIS_TAD_MUL:
    case IRIS_TAD_DIV:
      for (int64_t i = 0; i < node->numel; i++) {
        float g = node->grad[i];
        if (node->ad_op == IRIS_TAD_ADD) {
          iris_tensor_grad_add(a, i, g);
          iris_tensor_grad_add(b, i, g);
        } else if (node->ad_op == IRIS_TAD_SUB) {
          iris_tensor_grad_add(a, i, g);
          iris_tensor_grad_add(b, i, -g);
        } else if (node->ad_op == IRIS_TAD_MUL) {
          iris_tensor_grad_add(a, i, g * b->data[i]);
          iris_tensor_grad_add(b, i, g * a->data[i]);
        } else if (b->data[i] != 0.0f) {
          iris_tensor_grad_add(a, i, g / b->data[i]);
          iris_tensor_grad_add(b, i,
                               -g * a->data[i] / (b->data[i] * b->data[i]));
        }
      }
      break;
    case IRIS_TAD_NEG:
    case IRIS_TAD_RELU:
    case IRIS_TAD_SIGMOID:
    case IRIS_TAD_TANH:
    case IRIS_TAD_EXP:
    case IRIS_TAD_LOG:
    case IRIS_TAD_SQRT:
    case IRIS_TAD_ABS:
      for (int64_t i = 0; i < node->numel; i++) {
        float x = a->data[i], local = 0.0f;
        switch (node->ad_op) {
        case IRIS_TAD_NEG:
          local = -1.0f;
          break;
        case IRIS_TAD_RELU:
          local = x > 0.0f ? 1.0f : 0.0f;
          break;
        case IRIS_TAD_SIGMOID: {
          float s = 1.0f / (1.0f + expf(-x));
          local = s * (1.0f - s);
          break;
        }
        case IRIS_TAD_TANH: {
          float t = tanhf(x);
          local = 1.0f - t * t;
          break;
        }
        case IRIS_TAD_EXP:
          local = expf(x);
          break;
        case IRIS_TAD_LOG:
          local = x != 0.0f ? 1.0f / x : 0.0f;
          break;
        case IRIS_TAD_SQRT:
          local = x > 0.0f ? 0.5f / sqrtf(x) : 0.0f;
          break;
        case IRIS_TAD_ABS:
          local = x >= 0.0f ? 1.0f : -1.0f;
          break;
        default:
          break;
        }
        iris_tensor_grad_add(a, i, node->grad[i] * local);
      }
      break;
    case IRIS_TAD_SUM_ALL:
      for (int64_t i = 0; i < a->numel; i++)
        iris_tensor_grad_add(a, i, node->grad[0]);
      break;
    case IRIS_TAD_MATMUL: {
      int64_t m = a->shape[a->ndim - 2];
      int64_t k = a->shape[a->ndim - 1];
      int64_t n = b->shape[b->ndim - 1];
      for (int64_t i = 0; i < m; i++)
        for (int64_t p = 0; p < k; p++)
          for (int64_t j = 0; j < n; j++) {
            float g = node->grad[i * n + j];
            iris_tensor_grad_add(a, i * k + p, g * b->data[p * n + j]);
            iris_tensor_grad_add(b, p * n + j, a->data[i * k + p] * g);
          }
      break;
    }
    default:
      break;
    }
  }
  xfree(topo.data);
  return 1;
}

IrisTensor *iris_tensor_grad(IrisTensor *tensor) {
  if (!tensor)
    return NULL;
  IrisTensor *grad = iris_tensor_zeros(tensor->ndim, tensor->shape);
  if (tensor->grad)
    memcpy(grad->data, tensor->grad, (size_t)tensor->numel * sizeof(float));
  return grad;
}

double iris_tensor_item(IrisTensor *tensor) {
  return tensor && tensor->numel > 0 ? (double)tensor->data[0] : 0.0;
}

// ---------------------------------------------------------------------------
// Time / OS (Phase 97)
// ---------------------------------------------------------------------------

#ifdef _WIN32
#include <windows.h>
#else
#include <sys/time.h>
#include <unistd.h>
#endif

int64_t iris_now_ms(void) {
#ifdef _WIN32
  FILETIME ft;
  GetSystemTimeAsFileTime(&ft);
  /* FILETIME is 100-ns intervals since 1601-01-01.
     Subtract epoch offset (1601→1970) then convert to ms. */
  uint64_t t = ((uint64_t)ft.dwHighDateTime << 32) | ft.dwLowDateTime;
  t -= 116444736000000000ULL; /* 1601→1970 in 100-ns ticks */
  return (int64_t)(t / 10000ULL);
#else
  struct timeval tv;
  gettimeofday(&tv, NULL);
  return (int64_t)tv.tv_sec * 1000 + (int64_t)tv.tv_usec / 1000;
#endif
}

void iris_sleep_ms(int64_t ms) {
#ifdef _WIN32
  Sleep((DWORD)ms);
#else
  usleep((useconds_t)(ms * 1000));
#endif
}

// ---------------------------------------------------------------------------
// Struct / Tuple / Closure fallback helpers (opaque path)
// ---------------------------------------------------------------------------

/* iris_make_struct(ptr f0, ptr f1, …)
   — stores N boxed field values in a list-backed struct. */
IrisVal *iris_make_struct(int nfields, ...) {
  IrisList *l = iris_list_new();
  va_list ap;
  va_start(ap, nfields);
  for (int i = 0; i < nfields; i++) {
    IrisVal *v = va_arg(ap, IrisVal *);
    iris_list_push(l, v);
  }
  va_end(ap);
  IrisVal *r = (IrisVal *)xmalloc(sizeof(IrisVal));
  r->tag = IRIS_TAG_STRUCT;
  r->ptr = l;
  return r;
}

IrisVal *iris_get_field(IrisVal *s, int32_t idx) {
  if (!s)
    return iris_box_i64(0);
  if (s->tag == IRIS_TAG_STRUCT) {
    IrisList *l = (IrisList *)s->ptr;
    return iris_list_get(l, (int64_t)idx);
  }
  return iris_box_i64(0);
}

IrisVal *iris_make_tuple(int nelems, ...) {
  IrisList *l = iris_list_new();
  va_list ap;
  va_start(ap, nelems);
  for (int i = 0; i < nelems; i++) {
    IrisVal *v = va_arg(ap, IrisVal *);
    iris_list_push(l, v);
  }
  va_end(ap);
  IrisVal *r = (IrisVal *)xmalloc(sizeof(IrisVal));
  r->tag = IRIS_TAG_TUPLE;
  r->ptr = l;
  return r;
}

IrisVal *iris_get_element(IrisVal *t, int32_t idx) {
  if (!t)
    return iris_box_i64(0);
  if (t->tag == IRIS_TAG_TUPLE) {
    IrisList *l = (IrisList *)t->ptr;
    return iris_list_get(l, (int64_t)idx);
  }
  return iris_box_i64(0);
}

// ---------------------------------------------------------------------------
// IRIS language-facing ML runtime wrappers
// ---------------------------------------------------------------------------

typedef int (*IrisMlRunFn)(void *, IrisTensor **, size_t, IrisTensor ***,
                           size_t *);

static IrisVal *iris_mlrt_empty_tensor_pair(void) {
  IrisList *data = iris_list_new();
  IrisList *shape = iris_list_new();
  return iris_make_tuple(2, iris_box_list(data), iris_box_list(shape));
}

static IrisTensor *iris_mlrt_tensor_from_pair(IrisVal *pair) {
  if (!pair || pair->tag != IRIS_TAG_TUPLE)
    return NULL;

  IrisVal *data_val = iris_get_element(pair, 0);
  IrisVal *shape_val = iris_get_element(pair, 1);
  IrisList *data = iris_unbox_list(data_val);
  IrisList *shape_list = iris_unbox_list(shape_val);
  int64_t ndim64 = iris_list_len(shape_list);
  if (ndim64 <= 0 || ndim64 > INT32_MAX)
    return NULL;

  int32_t ndim = (int32_t)ndim64;
  int64_t *shape = (int64_t *)xmalloc(sizeof(int64_t) * (size_t)ndim);
  for (int32_t i = 0; i < ndim; i++) {
    shape[i] = iris_unbox_i64(iris_list_get(shape_list, i));
  }

  IrisTensor *tensor = iris_tensor_alloc(ndim, shape);
  xfree(shape);
  if (!tensor)
    return NULL;

  if (iris_list_len(data) != tensor->numel) {
    iris_tensor_free(tensor);
    return NULL;
  }

  for (int64_t i = 0; i < tensor->numel; i++) {
    tensor->data[i] = (float)iris_unbox_f64(iris_list_get(data, i));
  }
  return tensor;
}

static IrisVal *iris_mlrt_pair_from_tensor(IrisTensor *tensor) {
  if (!tensor)
    return iris_mlrt_empty_tensor_pair();

  IrisList *data = iris_list_new();
  IrisList *shape = iris_list_new();
  for (int64_t i = 0; i < tensor->numel; i++) {
    iris_list_push(data, iris_box_f64((double)tensor->data[i]));
  }
  for (int32_t i = 0; i < tensor->ndim; i++) {
    iris_list_push(shape, iris_box_i64(tensor->shape[i]));
  }
  return iris_make_tuple(2, iris_box_list(data), iris_box_list(shape));
}

static IrisList *iris_mlrt_empty_tensor_pair_unboxed(void) {
  IrisList *data = iris_list_new();
  IrisList *shape = iris_list_new();
  IrisList *t_list = iris_list_new();
  iris_list_push(t_list, iris_box_list(data));
  iris_list_push(t_list, iris_box_list(shape));
  return t_list;
}

static IrisVal *iris_mlrt_run_single(int64_t handle, IrisVal *input,
                                     IrisMlRunFn run_fn) {
  if (handle == 0 || !run_fn)
    return iris_mlrt_empty_tensor_pair();

  IrisTensor *native_input = iris_mlrt_tensor_from_pair(input);
  if (!native_input)
    return iris_mlrt_empty_tensor_pair();

  IrisTensor *inputs[1];
  inputs[0] = native_input;
  IrisTensor **outputs = NULL;
  size_t n_outputs = 0;
  int rc = run_fn((void *)(intptr_t)handle, inputs, 1, &outputs, &n_outputs);
  iris_tensor_free(native_input);

  if (rc != 0 || !outputs || n_outputs == 0) {
    if (outputs)
      free(outputs);
    return iris_mlrt_empty_tensor_pair();
  }

  IrisVal *result = iris_mlrt_pair_from_tensor(outputs[0]);

  for (size_t i = 0; i < n_outputs; i++) {
    if (outputs[i])
      iris_tensor_free(outputs[i]);
  }
  free(outputs);
  return result;
}

static IrisList *iris_mlrt_run_multi(int64_t handle, IrisList *inputs_list,
                                     IrisMlRunFn run_fn) {
  if (handle == 0 || !run_fn || !inputs_list) {
    return iris_list_new();
  }
  int64_t n_inputs = iris_list_len(inputs_list);
  if (n_inputs <= 0) {
    return iris_list_new();
  }

  IrisTensor **inputs_array =
      (IrisTensor **)xmalloc(sizeof(IrisTensor *) * (size_t)n_inputs);
  for (int64_t i = 0; i < n_inputs; i++) {
    inputs_array[i] = iris_mlrt_tensor_from_pair(iris_list_get(inputs_list, i));
  }

  IrisTensor **outputs_array = NULL;
  size_t n_outputs = 0;
  int rc = run_fn((void *)(intptr_t)handle, inputs_array, (size_t)n_inputs,
                  &outputs_array, &n_outputs);

  for (int64_t i = 0; i < n_inputs; i++) {
    if (inputs_array[i])
      iris_tensor_free(inputs_array[i]);
  }
  xfree(inputs_array);

  IrisList *out_list = iris_list_new();
  if (rc == 0 && outputs_array && n_outputs > 0) {
    for (size_t i = 0; i < n_outputs; i++) {
      if (outputs_array[i]) {
        iris_list_push(out_list, iris_mlrt_pair_from_tensor(outputs_array[i]));
        iris_tensor_free(outputs_array[i]);
      }
    }
  }
  if (outputs_array)
    free(outputs_array);

  return out_list;
}

#ifdef _WIN32
#define IRIS_EXPORT __declspec(dllexport)
#else
#define IRIS_EXPORT __attribute__((visibility("default")))
#endif

IRIS_EXPORT int64_t iris_mlrt_onnx_load(const char *model_path) {
  iris_sandbox_require_ffi("onnxruntime");
  iris_sandbox_require_fs_read(model_path);
  return (int64_t)(intptr_t)iris_onnx_session_create(model_path);
}

IRIS_EXPORT int64_t iris_mlrt_onnx_free(int64_t session) {
  iris_onnx_session_free((void *)(intptr_t)session);
  return 0;
}

IRIS_EXPORT IrisVal *iris_mlrt_onnx_run(int64_t session, IrisVal *input) {
  return iris_mlrt_run_single(session, input, iris_onnx_session_run);
}

IRIS_EXPORT IrisList *iris_mlrt_onnx_run_multi(int64_t session,
                                               IrisList *inputs_list) {
  return iris_mlrt_run_multi(session, inputs_list, iris_onnx_session_run);
}

/* ---------------------------------------------------------------------------
 * LibTorch plugin (loaded at runtime).
 *
 * LibTorch exposes a C++ API, so unlike ONNX/TensorFlow it cannot be dlopen'd
 * directly — the mangled symbols and C++ ABI make that impractical. Instead
 * pytorch_shim.cpp (which already declares its entry points `extern "C"`) is
 * built into a *separate* shared library that links LibTorch, and the core
 * runtime resolves those four C symbols from it on first use.
 *
 * The upshot: iris_runtime.o never references LibTorch, so it stays identical
 * whether or not the SDK is installed — which is what lets us ship one
 * prebuilt runtime object per target.
 * ------------------------------------------------------------------------- */

typedef void *(*fn_iris_pytorch_load)(const char *);
typedef int (*fn_iris_pytorch_run)(void *, IrisTensor **, size_t,
                                   IrisTensor ***, size_t *);
typedef void (*fn_iris_pytorch_free)(void *);
typedef double (*fn_iris_pytorch_train_step)(void *, IrisTensor **, size_t,
                                             IrisTensor **, size_t, double);

static fn_iris_pytorch_load p_iris_pytorch_load = NULL;
static fn_iris_pytorch_run p_iris_pytorch_run = NULL;
static fn_iris_pytorch_free p_iris_pytorch_free = NULL;
static fn_iris_pytorch_train_step p_iris_pytorch_train_step = NULL;

/* Returns 1 when the LibTorch plugin is loaded and usable, 0 otherwise.
 * Caches both outcomes so the warning is printed at most once. */
static int iris_torch_available(void) {
  iris_sandbox_require_ffi("iris_torch_plugin");
  static int state = -1; /* -1 unknown, 0 unavailable, 1 ready */
  if (state >= 0)
    return state;

  static const char *const candidates[] = {
#ifdef _WIN32
      "iris_torch_plugin.dll",
#elif defined(__APPLE__)
      "libiris_torch_plugin.dylib",
#else
      "libiris_torch_plugin.so",
#endif
  };

  void *lib = NULL;
  for (size_t i = 0; i < sizeof(candidates) / sizeof(candidates[0]); ++i) {
#ifdef _WIN32
    lib = (void *)LoadLibraryA(candidates[i]);
#else
    lib = dlopen(candidates[i], RTLD_LAZY);
#endif
    if (lib)
      break;
  }

  if (!lib) {
    fprintf(stderr,
            "iris: LibTorch plugin (iris_torch_plugin) not found. It must be "
            "built once against your LibTorch install and placed next to the "
            "binary or on the library search path. "
            "See docs/ml-backends.md.\n");
    state = 0;
    return 0;
  }

#ifdef _WIN32
#define IRIS_TORCH_SYM(n) (void *)GetProcAddress((HMODULE)lib, n)
#else
#define IRIS_TORCH_SYM(n) dlsym(lib, n)
#endif
  p_iris_pytorch_load =
      (fn_iris_pytorch_load)IRIS_TORCH_SYM("iris_pytorch_load");
  p_iris_pytorch_run = (fn_iris_pytorch_run)IRIS_TORCH_SYM("iris_pytorch_run");
  p_iris_pytorch_free =
      (fn_iris_pytorch_free)IRIS_TORCH_SYM("iris_pytorch_free");
  p_iris_pytorch_train_step =
      (fn_iris_pytorch_train_step)IRIS_TORCH_SYM("iris_pytorch_train_step");
#undef IRIS_TORCH_SYM

  if (!p_iris_pytorch_load || !p_iris_pytorch_run || !p_iris_pytorch_free ||
      !p_iris_pytorch_train_step) {
    fprintf(stderr, "iris: LibTorch plugin is missing required symbols — "
                    "rebuild it against this version of IRIS.\n");
    state = 0;
    return 0;
  }

  state = 1;
  return 1;
}

IRIS_EXPORT int64_t iris_mlrt_pytorch_load(const char *model_path) {
  iris_sandbox_require_ffi("libtorch");
  iris_sandbox_require_fs_read(model_path);
  if (!iris_torch_available())
    return 0;
  return (int64_t)(intptr_t)p_iris_pytorch_load(model_path);
}

IRIS_EXPORT int64_t iris_mlrt_pytorch_free(int64_t model) {
  if (!iris_torch_available())
    return 0;
  p_iris_pytorch_free((void *)(intptr_t)model);
  return 0;
}

IRIS_EXPORT IrisVal *iris_mlrt_pytorch_run(int64_t model, IrisVal *input) {
  if (!iris_torch_available())
    return iris_mlrt_empty_tensor_pair();
  return iris_mlrt_run_single(model, input, p_iris_pytorch_run);
}

IRIS_EXPORT IrisList *iris_mlrt_pytorch_run_multi(int64_t model,
                                                  IrisList *inputs_list) {
  if (!iris_torch_available())
    return iris_list_new();
  return iris_mlrt_run_multi(model, inputs_list, p_iris_pytorch_run);
}

IRIS_EXPORT double iris_mlrt_pytorch_train_step(int64_t model,
                                                IrisList *inputs_list,
                                                IrisList *targets_list,
                                                double lr) {
  if (!iris_torch_available())
    return 0.0;
  if (model == 0 || !inputs_list || !targets_list) {
    return 0.0;
  }
  int64_t n_inputs = iris_list_len(inputs_list);
  int64_t n_targets = iris_list_len(targets_list);
  if (n_inputs <= 0 || n_targets <= 0)
    return 0.0;

  IrisTensor **inputs_array =
      (IrisTensor **)xmalloc(sizeof(IrisTensor *) * (size_t)n_inputs);
  for (int64_t i = 0; i < n_inputs; i++) {
    inputs_array[i] = iris_mlrt_tensor_from_pair(iris_list_get(inputs_list, i));
  }

  IrisTensor **targets_array =
      (IrisTensor **)xmalloc(sizeof(IrisTensor *) * (size_t)n_targets);
  for (int64_t i = 0; i < n_targets; i++) {
    targets_array[i] =
        iris_mlrt_tensor_from_pair(iris_list_get(targets_list, i));
  }

  double loss = p_iris_pytorch_train_step((void *)(intptr_t)model, inputs_array,
                                          (size_t)n_inputs, targets_array,
                                          (size_t)n_targets, lr);

  for (int64_t i = 0; i < n_inputs; i++) {
    if (inputs_array[i])
      iris_tensor_free(inputs_array[i]);
  }
  xfree(inputs_array);
  for (int64_t i = 0; i < n_targets; i++) {
    if (targets_array[i])
      iris_tensor_free(targets_array[i]);
  }
  xfree(targets_array);

  return loss;
}

IRIS_EXPORT int64_t iris_mlrt_tf_load(const char *model_path) {
  iris_sandbox_require_ffi("tensorflow");
  iris_sandbox_require_fs_read(model_path);
  return (int64_t)(intptr_t)iris_tf_load_saved_model(model_path);
}

IRIS_EXPORT int64_t iris_mlrt_tf_free(int64_t model) {
  iris_tf_free((void *)(intptr_t)model);
  return 0;
}

IRIS_EXPORT IrisVal *iris_mlrt_tf_run(int64_t model, IrisVal *input) {
  return iris_mlrt_run_single(model, input, iris_tf_run);
}

IRIS_EXPORT IrisList *iris_mlrt_tf_run_multi(int64_t model,
                                             IrisList *inputs_list) {
  return iris_mlrt_run_multi(model, inputs_list, iris_tf_run);
}

/* Closure: stores a function pointer and captured environment. */
typedef struct {
  void *fn;           /* function pointer */
  IrisList *captures; /* captured values */
} IrisClosure;

IrisVal *iris_make_closure(void *fn, int ncaptures, ...) {
  IrisClosure *c = (IrisClosure *)xmalloc(sizeof(IrisClosure));
  c->fn = fn;
  c->captures = iris_list_new();
  va_list ap;
  va_start(ap, ncaptures);
  for (int i = 0; i < ncaptures; i++) {
    IrisVal *v = va_arg(ap, IrisVal *);
    iris_list_push(c->captures, v);
  }
  va_end(ap);
  IrisVal *r = (IrisVal *)xmalloc(sizeof(IrisVal));
  r->tag = IRIS_TAG_CLOSURE;
  r->ptr = c;
  return r;
}

/* ---- Trait-object (dyn Trait) helpers (Phase 91) ---- */

#define IRIS_TAG_TRAIT_OBJECT 22

/* Layout: { void* data, void* vtable_id } handled as a small heap object. */
static IrisVal *iris_make_trait_object_impl(void *data, void *vtable_id) {
  struct IrisTraitObject {
    void *data;
    void *vtable_id;
  };
  struct IrisTraitObject *p =
      (struct IrisTraitObject *)xmalloc(sizeof(struct IrisTraitObject));
  p->data = data;
  p->vtable_id = vtable_id;
  IrisVal *r = (IrisVal *)xmalloc(sizeof(IrisVal));
  r->tag = IRIS_TAG_TRAIT_OBJECT;
  r->ptr = p;
  return r;
}

IrisVal *iris_make_trait_object(void *data, void *vtable_id) {
  return iris_make_trait_object_impl(data, vtable_id);
}

/* ---- Closure accessor helpers (called from generated LLVM IR) ---- */

void *iris_closure_fn(IrisVal *closure) {
  return ((IrisClosure *)closure->ptr)->fn;
}

int iris_closure_ncaptures(IrisVal *closure) {
  return ((IrisClosure *)closure->ptr)->captures->len;
}

IrisVal *iris_closure_get_capture(IrisVal *closure, int idx) {
  return ((IrisClosure *)closure->ptr)->captures->data[idx];
}

/* ======================================================================== */
/*  Terminal / Interactive Input                                            */
/* ======================================================================== */

int64_t iris_read_key(void) {
#if defined(__wasm__)
  /* WASM: terminal raw mode not available; use getchar */
  int c = getchar();
  return (int64_t)c;
#elif defined(_WIN32)
  /* Windows: use _getch() — no echo, no Enter needed */
  int c = _getch();
  /* Extended keys (arrows, F-keys) produce 0 or 224 prefix */
  if (c == 0 || c == 224) {
    int ext = _getch();
    /* Encode as -(ext) to distinguish from regular keys */
    return -(int64_t)ext;
  }
  return (int64_t)c;
#else
  /* POSIX: switch tty to raw mode, read one byte, restore */
  struct termios old, raw;
  tcgetattr(STDIN_FILENO, &old);
  raw = old;
  raw.c_lflag &= ~(ICANON | ECHO);
  raw.c_cc[VMIN] = 1;
  raw.c_cc[VTIME] = 0;
  tcsetattr(STDIN_FILENO, TCSANOW, &raw);
  int c = getchar();
  tcsetattr(STDIN_FILENO, TCSANOW, &old);
  return (int64_t)c;
#endif
}

char *iris_read_password(const char *prompt) {
  if (prompt && *prompt)
    fputs(prompt, stdout);
  fflush(stdout);

  size_t cap = 256, len = 0;
  char *buf = (char *)xmalloc(cap);

#if defined(__wasm__)
  /* WASM: no echo-toggle; just read line from stdin */
  {
    int c;
    while ((c = getchar()) != '\n' && c != EOF) {
      if (c == 127 && len > 0) {
        len--;
        continue;
      }
      if (len + 1 >= cap) {
        cap *= 2;
        buf = (char *)xrealloc(buf, cap);
      }
      buf[len++] = (char)c;
    }
  }
#elif defined(_WIN32)
  int c;
  while ((c = _getch()) != '\r' && c != '\n' && c != EOF) {
    if (c == '\b' && len > 0) {
      len--;
      continue;
    }
    if (c == '\b')
      continue;
    if (len + 1 >= cap) {
      cap *= 2;
      buf = (char *)xrealloc(buf, cap);
    }
    buf[len++] = (char)c;
  }
#else
  struct termios old, noecho;
  tcgetattr(STDIN_FILENO, &old);
  noecho = old;
  noecho.c_lflag &= ~ECHO;
  tcsetattr(STDIN_FILENO, TCSANOW, &noecho);
  int c;
  while ((c = getchar()) != '\n' && c != EOF) {
    if (c == 127 && len > 0) {
      len--;
      continue;
    }
    if (len + 1 >= cap) {
      cap *= 2;
      buf = (char *)xrealloc(buf, cap);
    }
    buf[len++] = (char)c;
  }
  tcsetattr(STDIN_FILENO, TCSANOW, &old);
#endif
  buf[len] = '\0';
  putchar('\n');
  return buf;
}

void iris_term_clear(void) {
#ifdef _WIN32
  HANDLE h = GetStdHandle(STD_OUTPUT_HANDLE);
  CONSOLE_SCREEN_BUFFER_INFO csbi;
  GetConsoleScreenBufferInfo(h, &csbi);
  DWORD cells = csbi.dwSize.X * csbi.dwSize.Y, written;
  COORD origin = {0, 0};
  FillConsoleOutputCharacter(h, ' ', cells, origin, &written);
  FillConsoleOutputAttribute(h, csbi.wAttributes, cells, origin, &written);
  SetConsoleCursorPosition(h, origin);
#else
  fputs("\033[2J\033[H", stdout);
  fflush(stdout);
#endif
}

void iris_term_cursor(int64_t row, int64_t col) {
#ifdef _WIN32
  COORD pos = {(SHORT)(col - 1), (SHORT)(row - 1)};
  SetConsoleCursorPosition(GetStdHandle(STD_OUTPUT_HANDLE), pos);
#else
  printf("\033[%lld;%lldH", (long long)row, (long long)col);
  fflush(stdout);
#endif
}

void iris_term_show_cursor(int show) {
#ifdef _WIN32
  CONSOLE_CURSOR_INFO ci;
  GetConsoleCursorInfo(GetStdHandle(STD_OUTPUT_HANDLE), &ci);
  ci.bVisible = show ? TRUE : FALSE;
  SetConsoleCursorInfo(GetStdHandle(STD_OUTPUT_HANDLE), &ci);
#else
  fputs(show ? "\033[?25h" : "\033[?25l", stdout);
  fflush(stdout);
#endif
}

void iris_term_set_color(int64_t fg, int64_t bg) {
  /* ANSI 256-color: foreground=38;5;N, background=48;5;N */
  if (fg >= 0)
    printf("\033[38;5;%lldm", (long long)fg);
  if (bg >= 0)
    printf("\033[48;5;%lldm", (long long)bg);
  fflush(stdout);
}

void iris_term_reset(void) {
  fputs("\033[0m", stdout);
  fflush(stdout);
}

int64_t iris_term_rows(void) {
#if defined(__wasm__)
  return 24; /* fallback */
#elif defined(_WIN32)
  CONSOLE_SCREEN_BUFFER_INFO csbi;
  GetConsoleScreenBufferInfo(GetStdHandle(STD_OUTPUT_HANDLE), &csbi);
  return csbi.srWindow.Bottom - csbi.srWindow.Top + 1;
#else
  struct winsize w;
  ioctl(STDOUT_FILENO, TIOCGWINSZ, &w);
  return (int64_t)w.ws_row;
#endif
}

int64_t iris_term_cols(void) {
#if defined(__wasm__)
  return 80; /* fallback */
#elif defined(_WIN32)
  CONSOLE_SCREEN_BUFFER_INFO csbi;
  GetConsoleScreenBufferInfo(GetStdHandle(STD_OUTPUT_HANDLE), &csbi);
  return csbi.srWindow.Right - csbi.srWindow.Left + 1;
#else
  struct winsize w;
  ioctl(STDOUT_FILENO, TIOCGWINSZ, &w);
  return (int64_t)w.ws_col;
#endif
}

/* ======================================================================== */
/*  UDP / TCP Networking                                                     */
/* ======================================================================== */

/* Forward declaration — defined in TCP section below. */
#ifdef _WIN32
static void ensure_wsa(void);
#endif

static int64_t iris_udp_open_raw(int64_t port);
int64_t iris_udp_open(int64_t port) {
  iris_sandbox_require_conn_open("0.0.0.0");
  int64_t fd = iris_udp_open_raw(port);
  iris_sandbox_track_conn_open(fd);
  return fd;
}

static int64_t iris_udp_open_raw(int64_t port) {
#ifdef __IRIS_WASM_STUB
  (void)port;
  return -1; /* no networking on WASM P1 */
#elif defined(_WIN32)
  ensure_wsa();
  SOCKET s = socket(AF_INET, SOCK_DGRAM, IPPROTO_UDP);
  if (s == INVALID_SOCKET)
    return -1;
  if (port > 0) {
    struct sockaddr_in addr = {0};
    addr.sin_family = AF_INET;
    addr.sin_port = htons((uint16_t)port);
    addr.sin_addr.s_addr = INADDR_ANY;
    if (bind(s, (struct sockaddr *)&addr, sizeof(addr)) != 0) {
      closesocket(s);
      return -1;
    }
  }
  return (int64_t)s;
#else
  int s = socket(AF_INET, SOCK_DGRAM, IPPROTO_UDP);
  if (s < 0)
    return -1;
  if (port > 0) {
    struct sockaddr_in addr = {0};
    addr.sin_family = AF_INET;
    addr.sin_port = htons((uint16_t)port);
    addr.sin_addr.s_addr = INADDR_ANY;
    if (bind(s, (struct sockaddr *)&addr, sizeof(addr)) != 0) {
      close(s);
      return -1;
    }
  }
  return (int64_t)s;
#endif
}

#ifndef __IRIS_WASM_STUB
void iris_udp_send(int64_t fd, const char *addr_port, int64_t data_len) {
  /* addr_port format: "host:port:data" — data starts after second colon */
  char host[256] = {0};
  uint16_t port = 0;
  const char *first_colon = addr_port ? strchr(addr_port, ':') : NULL;
  if (first_colon) {
    size_t hlen = (size_t)(first_colon - addr_port);
    if (hlen >= sizeof(host))
      hlen = sizeof(host) - 1;
    memcpy(host, addr_port, hlen);
    host[hlen] = '\0';
    iris_sandbox_require_network(host);
  } else {
    iris_sandbox_require_network_enabled();
  }
  char *p = strdup(addr_port);
  char *colon = strrchr(p, ':');
  if (!colon) {
    free(p);
    return;
  }
  char *data = colon + 1;
  *colon = '\0';
  char *port_colon = strrchr(p, ':');
  if (port_colon) {
    port = (uint16_t)atoi(port_colon + 1);
    *port_colon = '\0';
    strncpy(host, p, sizeof(host) - 1);
  } else {
    strncpy(host, p, sizeof(host) - 1);
  }
  struct sockaddr_in dst = {0};
  dst.sin_family = AF_INET;
  dst.sin_port = htons(port);
  if (inet_pton(AF_INET, host, &dst.sin_addr) != 1) {
    free(p);
    return;
  }
  size_t dlen = data_len > 0 ? (size_t)data_len : strlen(data);
#ifdef _WIN32
  sendto((SOCKET)fd, data, (int)dlen, 0, (struct sockaddr *)&dst, sizeof(dst));
#else
  sendto((int)fd, data, dlen, 0, (struct sockaddr *)&dst, sizeof(dst));
#endif
  free(p);
}

char *iris_udp_recv(int64_t fd) {
  iris_sandbox_require_network_enabled();
  char buf[65536];
  struct sockaddr_in src;
#ifdef _WIN32
  int slen = sizeof(src);
  int n = recvfrom((SOCKET)fd, buf, sizeof(buf) - 1, 0, (struct sockaddr *)&src,
                   &slen);
#else
  socklen_t slen = sizeof(src);
  int n = recvfrom((int)fd, buf, sizeof(buf) - 1, 0, (struct sockaddr *)&src,
                   &slen);
#endif
  if (n < 0) {
    char *e = (char *)xmalloc(1);
    *e = '\0';
    return e;
  }
  buf[n] = '\0';
  char ip_buf[64];
  inet_ntop(AF_INET, &src.sin_addr, ip_buf, sizeof(ip_buf));
  uint16_t port = ntohs(src.sin_port);
  size_t needed = strlen(ip_buf) + 6 + n + 2;
  char *result = (char *)xmalloc(needed);
  snprintf(result, needed, "%s:%d:%s", ip_buf, port, buf);
  return result;
}

void iris_udp_close(int64_t fd) {
  iris_sandbox_track_conn_close(fd);
#ifdef _WIN32
  closesocket((SOCKET)fd);
#else
  close((int)fd);
#endif
}
#else
/* WASM: networking stubs */
void iris_udp_send(int64_t fd, const char *addr_port, int64_t data_len) {
  iris_sandbox_require_network_enabled();
  (void)fd;
  (void)addr_port;
  (void)data_len;
}
char *iris_udp_recv(int64_t fd) {
  iris_sandbox_require_network_enabled();
  (void)fd;
  char *e = xmalloc(1);
  *e = '\0';
  return e;
}
void iris_udp_close(int64_t fd) {
  iris_sandbox_track_conn_close(fd);
  (void)fd;
}
#endif

/* ======================================================================== */
/*  TCP Networking                                                          */
/* ======================================================================== */

#ifndef __IRIS_WASM_STUB
#ifdef _WIN32
static int wsa_initialized = 0;
static void ensure_wsa(void) {
  if (!wsa_initialized) {
    WSADATA wsa;
    WSAStartup(MAKEWORD(2, 2), &wsa);
    wsa_initialized = 1;
  }
}
#endif

static int64_t iris_tcp_connect_raw(const char *host, int64_t port);
int64_t iris_tcp_connect(const char *host, int64_t port) {
  iris_sandbox_require_conn_open(host);
  int64_t fd = iris_tcp_connect_raw(host, port);
  iris_sandbox_track_conn_open(fd);
  return fd;
}

static int64_t iris_tcp_connect_raw(const char *host, int64_t port) {
#ifdef _WIN32
  ensure_wsa();
  SOCKET s = socket(AF_INET, SOCK_STREAM, 0);
  if (s == INVALID_SOCKET)
    return -1;
  struct addrinfo hints = {0}, *res = NULL;
  hints.ai_family = AF_INET;
  hints.ai_socktype = SOCK_STREAM;
  char port_str[16];
  snprintf(port_str, sizeof(port_str), "%lld", (long long)port);
  if (getaddrinfo(host, port_str, &hints, &res) != 0) {
    closesocket(s);
    return -1;
  }
  if (connect(s, res->ai_addr, (int)res->ai_addrlen) != 0) {
    freeaddrinfo(res);
    closesocket(s);
    return -1;
  }
  freeaddrinfo(res);
  return (int64_t)s;
#else
  int s = socket(AF_INET, SOCK_STREAM, 0);
  if (s < 0)
    return -1;
  struct addrinfo hints = {0}, *res = NULL;
  hints.ai_family = AF_INET;
  hints.ai_socktype = SOCK_STREAM;
  char port_str[16];
  snprintf(port_str, sizeof(port_str), "%lld", (long long)port);
  if (getaddrinfo(host, port_str, &hints, &res) != 0) {
    close(s);
    return -1;
  }
  if (connect(s, res->ai_addr, res->ai_addrlen) != 0) {
    freeaddrinfo(res);
    close(s);
    return -1;
  }
  freeaddrinfo(res);
  return (int64_t)s;
#endif
}

#ifndef INADDR_LOOPBACK
#define INADDR_LOOPBACK ((uint32_t)0x7f000001)
#endif

/* Listen on every interface (`address` INADDR_ANY) or on loopback only. */
static int64_t iris_tcp_listen_at_raw(uint32_t address, int64_t port);
static int64_t iris_tcp_listen_at(uint32_t address, int64_t port) {
  iris_sandbox_require_conn_open(address == INADDR_LOOPBACK ? "127.0.0.1"
                                                            : "0.0.0.0");
  int64_t fd = iris_tcp_listen_at_raw(address, port);
  iris_sandbox_track_conn_open(fd);
  return fd;
}

static int64_t iris_tcp_listen_at_raw(uint32_t address, int64_t port) {
#ifdef _WIN32
  ensure_wsa();
  SOCKET s = socket(AF_INET, SOCK_STREAM, 0);
  if (s == INVALID_SOCKET)
    return -1;
  /* Exclusive, not SO_REUSEADDR: on Windows SO_REUSEADDR lets a second
   * socket bind a port that is already listening, so two servers split one
   * port's connections between them. A busy port must fail to listen, as it
   * does on Unix and in the interpreter (Rust's TcpListener). */
  int opt = 1;
  setsockopt(s, SOL_SOCKET, SO_EXCLUSIVEADDRUSE, (const char *)&opt,
             sizeof(opt));
  struct sockaddr_in addr = {0};
  addr.sin_family = AF_INET;
  addr.sin_addr.s_addr = htonl(address);
  addr.sin_port = htons((u_short)port);
  if (bind(s, (struct sockaddr *)&addr, sizeof(addr)) != 0) {
    closesocket(s);
    return -1;
  }
  if (listen(s, SOMAXCONN) != 0) {
    closesocket(s);
    return -1;
  }
  return (int64_t)s;
#else
  int s = socket(AF_INET, SOCK_STREAM, 0);
  if (s < 0)
    return -1;
  int opt = 1;
  setsockopt(s, SOL_SOCKET, SO_REUSEADDR, &opt, sizeof(opt));
  struct sockaddr_in addr = {0};
  addr.sin_family = AF_INET;
  addr.sin_addr.s_addr = htonl(address);
  addr.sin_port = htons((uint16_t)port);
  if (bind(s, (struct sockaddr *)&addr, sizeof(addr)) != 0) {
    close(s);
    return -1;
  }
  if (listen(s, SOMAXCONN) != 0) {
    close(s);
    return -1;
  }
  return (int64_t)s;
#endif
}

int64_t iris_tcp_listen(int64_t port) {
  return iris_tcp_listen_at(INADDR_ANY, port);
}

/* Reachable from this machine only (127.0.0.1). */
int64_t iris_tcp_listen_loopback(int64_t port) {
  return iris_tcp_listen_at(INADDR_LOOPBACK, port);
}

int64_t iris_tcp_accept(int64_t listener) {
  iris_sandbox_require_conn_open(NULL);
#ifdef _WIN32
  SOCKET c = accept((SOCKET)listener, NULL, NULL);
  int64_t fd = (c == INVALID_SOCKET) ? -1 : (int64_t)c;
#else
  int c = accept((int)listener, NULL, NULL);
  int64_t fd = (c < 0) ? -1 : (int64_t)c;
#endif
  if (fd >= 0) {
    iris_sandbox_verify_accepted_peer(fd);
    iris_sandbox_track_conn_open(fd);
  }
  return fd;
}

char *iris_tcp_read(int64_t conn) {
  iris_sandbox_require_network_enabled();
  char buf[8192];
  int n;
#ifdef _WIN32
  n = recv((SOCKET)conn, buf, sizeof(buf) - 1, 0);
#else
  n = recv((int)conn, buf, sizeof(buf) - 1, 0);
#endif
  if (n <= 0) {
    char *e = (char *)xmalloc(1);
    e[0] = '\0';
    return e;
  }
  buf[n] = '\0';
  char *result = (char *)xmalloc(n + 1);
  memcpy(result, buf, n + 1);
  return result;
}

void iris_tcp_write(int64_t conn, const char *data) {
  iris_sandbox_require_network_enabled();
  if (!data)
    return;
  size_t len = strlen(data);
#ifdef _WIN32
  send((SOCKET)conn, data, (int)len, 0);
#else
  send((int)conn, data, len, 0);
#endif
}

int64_t iris_tcp_set_timeout(int64_t conn, int64_t read_timeout_ms,
                             int64_t write_timeout_ms) {
  iris_sandbox_require_network_enabled();
  if (read_timeout_ms < 0 || write_timeout_ms < 0)
    return -1;
#ifdef _WIN32
  DWORD read_ms = (DWORD)read_timeout_ms;
  DWORD write_ms = (DWORD)write_timeout_ms;
  if (setsockopt((SOCKET)conn, SOL_SOCKET, SO_RCVTIMEO, (const char *)&read_ms,
                 sizeof(read_ms)) != 0)
    return -1;
  if (setsockopt((SOCKET)conn, SOL_SOCKET, SO_SNDTIMEO, (const char *)&write_ms,
                 sizeof(write_ms)) != 0)
    return -1;
#else
  struct timeval read_tv = {(time_t)(read_timeout_ms / 1000),
                            (suseconds_t)((read_timeout_ms % 1000) * 1000)};
  struct timeval write_tv = {(time_t)(write_timeout_ms / 1000),
                             (suseconds_t)((write_timeout_ms % 1000) * 1000)};
  if (setsockopt((int)conn, SOL_SOCKET, SO_RCVTIMEO, &read_tv,
                 sizeof(read_tv)) != 0)
    return -1;
  if (setsockopt((int)conn, SOL_SOCKET, SO_SNDTIMEO, &write_tv,
                 sizeof(write_tv)) != 0)
    return -1;
#endif
  return 0;
}

int64_t iris_tcp_write_all(int64_t conn, const char *data) {
  iris_sandbox_require_network_enabled();
  if (!data)
    return -1;
  size_t len = strlen(data);
  size_t sent = 0;
  while (sent < len) {
#ifdef _WIN32
    int n = send((SOCKET)conn, data + sent, (int)(len - sent), 0);
#else
    ssize_t n = send((int)conn, data + sent, len - sent, 0);
#endif
    if (n <= 0)
      return -1;
    sent += (size_t)n;
  }
  return (int64_t)sent;
}

char *iris_tcp_read_max(int64_t conn, int64_t max_bytes) {
  iris_sandbox_require_network_enabled();
  if (max_bytes <= 0 || max_bytes > 16 * 1024 * 1024) {
    char *empty = (char *)xmalloc(1);
    empty[0] = '\0';
    return empty;
  }
  char *result = (char *)xmalloc((size_t)max_bytes + 1);
#ifdef _WIN32
  int n = recv((SOCKET)conn, result, (int)max_bytes, 0);
#else
  ssize_t n = recv((int)conn, result, (size_t)max_bytes, 0);
#endif
  if (n <= 0) {
    result[0] = '\0';
    return result;
  }
  result[n] = '\0';
  return result;
}

int64_t iris_tcp_shutdown(int64_t conn) {
  iris_sandbox_require_network_enabled();
#ifdef _WIN32
  return shutdown((SOCKET)conn, SD_BOTH) == 0 ? 0 : -1;
#else
  return shutdown((int)conn, SHUT_RDWR) == 0 ? 0 : -1;
#endif
}

void iris_tcp_close(int64_t conn) {
  iris_sandbox_track_conn_close(conn);
#ifdef _WIN32
  closesocket((SOCKET)conn);
#else
  close((int)conn);
#endif
}
#else  /* __IRIS_WASM_STUB */
/* WASM P1: no TCP networking */
int64_t iris_tcp_connect(const char *host, int64_t port) {
  iris_sandbox_require_conn_open(host);
  (void)port;
  return -1;
}
int64_t iris_tcp_listen(int64_t port) {
  iris_sandbox_require_conn_open("0.0.0.0");
  (void)port;
  return -1;
}
int64_t iris_tcp_listen_loopback(int64_t port) {
  iris_sandbox_require_conn_open("127.0.0.1");
  (void)port;
  return -1;
}
int64_t iris_tcp_accept(int64_t listener) {
  iris_sandbox_require_conn_open(NULL);
  (void)listener;
  return -1;
}
char *iris_tcp_read(int64_t conn) {
  iris_sandbox_require_network_enabled();
  (void)conn;
  char *e = xmalloc(1);
  *e = '\0';
  return e;
}
void iris_tcp_write(int64_t conn, const char *data) {
  iris_sandbox_require_network_enabled();
  (void)conn;
  (void)data;
}
void iris_tcp_close(int64_t conn) {
  iris_sandbox_track_conn_close(conn);
  (void)conn;
}
int64_t iris_tcp_set_timeout(int64_t conn, int64_t read_timeout_ms,
                             int64_t write_timeout_ms) {
  iris_sandbox_require_network_enabled();
  (void)conn;
  (void)read_timeout_ms;
  (void)write_timeout_ms;
  return -1;
}
int64_t iris_tcp_write_all(int64_t conn, const char *data) {
  iris_sandbox_require_network_enabled();
  (void)conn;
  (void)data;
  return -1;
}
char *iris_tcp_read_max(int64_t conn, int64_t max_bytes) {
  iris_sandbox_require_network_enabled();
  (void)conn;
  (void)max_bytes;
  char *e = xmalloc(1);
  *e = '\0';
  return e;
}
int64_t iris_tcp_shutdown(int64_t conn) {
  iris_sandbox_require_network_enabled();
  (void)conn;
  return -1;
}
#endif /* __IRIS_WASM_STUB */

/* ======================================================================== */
/*  HTTP (simple implementation using TCP sockets)                          */
/* ======================================================================== */

#ifndef __IRIS_WASM_STUB
/* Parse a URL into host, port, path.  Returns 0 on success. */
static int parse_url(const char *url, char *host, int *port, char *path) {
  *port = 80;
  const char *p = url;
  if (strncmp(p, "http://", 7) == 0) {
    p += 7;
    *port = 80;
  } else if (strncmp(p, "https://", 8) == 0) {
    p += 8;
    *port = 443;
  }
  const char *slash = strchr(p, '/');
  const char *colon = strchr(p, ':');
  if (colon && (!slash || colon < slash)) {
    size_t hlen = colon - p;
    memcpy(host, p, hlen);
    host[hlen] = '\0';
    *port = atoi(colon + 1);
    p = slash ? slash : p + strlen(p);
  } else if (slash) {
    size_t hlen = slash - p;
    memcpy(host, p, hlen);
    host[hlen] = '\0';
    p = slash;
  } else {
    strcpy(host, p);
    p = p + strlen(p);
  }
  if (*p == '/')
    strcpy(path, p);
  else
    strcpy(path, "/");
  return 0;
}

#ifdef _WIN32
static wchar_t *iris_utf8_to_wide(const char *text) {
  int count = MultiByteToWideChar(CP_UTF8, 0, text, -1, NULL, 0);
  if (count <= 0)
    return NULL;
  wchar_t *wide = (wchar_t *)xmalloc((size_t)count * sizeof(wchar_t));
  if (MultiByteToWideChar(CP_UTF8, 0, text, -1, wide, count) <= 0) {
    xfree(wide);
    return NULL;
  }
  return wide;
}

static char *iris_winhttp_request(const char *method, const char *url,
                                  const char *body, const char *extra_headers,
                                  int64_t timeout_ms) {
  iris_sandbox_require_url_network(url);
  iris_http_status = 0;
  char host[256] = {0}, path[2048] = {0};
  int port = 443;
  if (parse_url(url, host, &port, path) != 0) {
    char *empty = (char *)xmalloc(1);
    empty[0] = '\0';
    return empty;
  }
  wchar_t *host_w = iris_utf8_to_wide(host);
  wchar_t *path_w = iris_utf8_to_wide(path);
  wchar_t *method_w = iris_utf8_to_wide(method && *method ? method : "GET");
  const char *default_header = "Content-Type: application/json\r\n";
  size_t header_bytes =
      strlen(default_header) + (extra_headers ? strlen(extra_headers) : 0) + 1;
  char *combined_headers = (char *)xmalloc(header_bytes);
  snprintf(combined_headers, header_bytes, "%s%s", default_header,
           extra_headers ? extra_headers : "");
  wchar_t *headers_w = iris_utf8_to_wide(combined_headers);
  if (!host_w || !path_w || !method_w || !headers_w)
    goto fail_early;

  HINTERNET session =
      WinHttpOpen(L"IRIS/1.0", WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY,
                  WINHTTP_NO_PROXY_NAME, WINHTTP_NO_PROXY_BYPASS, 0);
  HINTERNET connection = NULL;
  HINTERNET request = NULL;
  char *response = NULL;
  int succeeded = 0;
  if (!session)
    goto cleanup;
  int timeout =
      timeout_ms > 0 && timeout_ms <= INT32_MAX ? (int)timeout_ms : 30000;
  WinHttpSetTimeouts(session, timeout, timeout, timeout, timeout);
  connection = WinHttpConnect(session, host_w, (INTERNET_PORT)port, 0);
  if (!connection)
    goto cleanup;
  request =
      WinHttpOpenRequest(connection, method_w, path_w, NULL, WINHTTP_NO_REFERER,
                         WINHTTP_DEFAULT_ACCEPT_TYPES, WINHTTP_FLAG_SECURE);
  if (!request)
    goto cleanup;
#ifdef WINHTTP_OPTION_DECOMPRESSION
  {
    /* Advertise and decompress gzip only; some CDNs send raw RFC-1951 deflate
     * on "deflate" which causes WinHTTP decompression to yield empty reads. */
    DWORD decomp_flags = WINHTTP_DECOMPRESSION_FLAG_GZIP;
    WinHttpSetOption(request, WINHTTP_OPTION_DECOMPRESSION, &decomp_flags,
                     sizeof(decomp_flags));
  }
#endif

  const char *payload = body ? body : "";
  DWORD payload_len = (DWORD)strlen(payload);
  if (!WinHttpSendRequest(request, headers_w, (DWORD)-1L,
                          payload_len > 0 ? (LPVOID)payload
                                          : WINHTTP_NO_REQUEST_DATA,
                          payload_len, payload_len, 0))
    goto cleanup;
  if (!WinHttpReceiveResponse(request, NULL))
    goto cleanup;
  {
    DWORD status = 0;
    DWORD status_size = sizeof(status);
    if (WinHttpQueryHeaders(
            request, WINHTTP_QUERY_STATUS_CODE | WINHTTP_QUERY_FLAG_NUMBER,
            WINHTTP_HEADER_NAME_BY_INDEX, &status, &status_size,
            WINHTTP_NO_HEADER_INDEX)) {
      iris_http_status = (int64_t)status;
    }
  }

  size_t capacity = 32768;
  size_t length = 0;
  response = (char *)xmalloc(capacity);
  for (;;) {
    const DWORD chunk = 16384;
    if (length + chunk + 1 > 64 * 1024 * 1024)
      goto cleanup;
    while (length + chunk + 1 > capacity)
      capacity *= 2;
    response = (char *)xrealloc(response, capacity);
    DWORD received = 0;
    if (!WinHttpReadData(request, response + length, chunk, &received))
      goto cleanup;
    if (received == 0)
      break;
    length += received;
  }
  response[length] = '\0';
  succeeded = 1;

cleanup:
  if (request)
    WinHttpCloseHandle(request);
  if (connection)
    WinHttpCloseHandle(connection);
  if (session)
    WinHttpCloseHandle(session);
  xfree(host_w);
  xfree(path_w);
  xfree(method_w);
  xfree(headers_w);
  xfree(combined_headers);
  if (succeeded)
    return response;
  xfree(response);
  {
    char *empty = (char *)xmalloc(1);
    empty[0] = '\0';
    return empty;
  }

fail_early:
  xfree(host_w);
  xfree(path_w);
  xfree(method_w);
  xfree(headers_w);
  xfree(combined_headers);
  {
    char *empty = (char *)xmalloc(1);
    empty[0] = '\0';
    return empty;
  }
}
#endif

#ifndef _WIN32
#include <dlfcn.h>
static char *iris_openssl_https_get(const char *host, int port,
                                    const char *path) {
  iris_sandbox_require_network(host);
  const char *libs[] = {"libssl.so.3", "libssl.so.1.1", "libssl.so",
                        "libssl.dylib", NULL};
  void *lib = NULL;
  for (int i = 0; libs[i] && !lib; i++) {
    lib = dlopen(libs[i], RTLD_LAZY | RTLD_LOCAL);
  }
  if (!lib)
    return NULL;

  typedef void *(*fn_tls_method_t)(void);
  typedef void *(*fn_ctx_new_t)(void *);
  typedef int (*fn_ctx_set_verify_paths_t)(void *);
  typedef void (*fn_ctx_free_t)(void *);
  typedef void *(*fn_ssl_new_t)(void *);
  typedef int (*fn_ssl_set_fd_t)(void *, int);
  typedef long (*fn_ssl_ctrl_t)(void *, int, long, void *);
  typedef int (*fn_ssl_connect_t)(void *);
  typedef int (*fn_ssl_write_t)(void *, const void *, int);
  typedef int (*fn_ssl_read_t)(void *, void *, int);
  typedef int (*fn_ssl_shutdown_t)(void *);
  typedef void (*fn_ssl_free_t)(void *);

  fn_tls_method_t p_method = (fn_tls_method_t)dlsym(lib, "TLS_client_method");
  if (!p_method)
    p_method = (fn_tls_method_t)dlsym(lib, "SSLv23_client_method");
  fn_ctx_new_t p_ctx_new = (fn_ctx_new_t)dlsym(lib, "SSL_CTX_new");
  fn_ctx_set_verify_paths_t p_def_paths =
      (fn_ctx_set_verify_paths_t)dlsym(lib, "SSL_CTX_set_default_verify_paths");
  fn_ctx_free_t p_ctx_free = (fn_ctx_free_t)dlsym(lib, "SSL_CTX_free");
  fn_ssl_new_t p_ssl_new = (fn_ssl_new_t)dlsym(lib, "SSL_new");
  fn_ssl_set_fd_t p_ssl_set_fd = (fn_ssl_set_fd_t)dlsym(lib, "SSL_set_fd");
  fn_ssl_ctrl_t p_ssl_ctrl = (fn_ssl_ctrl_t)dlsym(lib, "SSL_ctrl");
  fn_ssl_connect_t p_ssl_connect = (fn_ssl_connect_t)dlsym(lib, "SSL_connect");
  fn_ssl_write_t p_ssl_write = (fn_ssl_write_t)dlsym(lib, "SSL_write");
  fn_ssl_read_t p_ssl_read = (fn_ssl_read_t)dlsym(lib, "SSL_read");
  fn_ssl_shutdown_t p_ssl_shutdown =
      (fn_ssl_shutdown_t)dlsym(lib, "SSL_shutdown");
  fn_ssl_free_t p_ssl_free = (fn_ssl_free_t)dlsym(lib, "SSL_free");

  if (!p_method || !p_ctx_new || !p_ctx_free || !p_ssl_new || !p_ssl_set_fd ||
      !p_ssl_connect || !p_ssl_write || !p_ssl_read || !p_ssl_free) {
    return NULL;
  }

  int64_t fd = iris_tcp_connect(host, port);
  if (fd < 0)
    return NULL;

  void *ctx = p_ctx_new(p_method());
  if (!ctx) {
    iris_tcp_close(fd);
    return NULL;
  }
  if (p_def_paths)
    p_def_paths(ctx);

  void *ssl = p_ssl_new(ctx);
  if (!ssl) {
    p_ctx_free(ctx);
    iris_tcp_close(fd);
    return NULL;
  }
  p_ssl_set_fd(ssl, (int)fd);
  /* SSL_CTRL_SET_TLSEXT_HOSTNAME = 55 */
  if (p_ssl_ctrl)
    p_ssl_ctrl(ssl, 55, 0, (void *)host);
  if (p_ssl_connect(ssl) <= 0) {
    p_ssl_free(ssl);
    p_ctx_free(ctx);
    iris_tcp_close(fd);
    return NULL;
  }

  char req[4096];
  int rlen = snprintf(
      req, sizeof(req),
      "GET %s HTTP/1.0\r\nHost: %s\r\nConnection: close\r\n\r\n", path, host);
  p_ssl_write(ssl, req, rlen);

  size_t cap = 16384, len = 0;
  char *resp = (char *)xmalloc(cap);
  for (;;) {
    char buf[4096];
    int n = p_ssl_read(ssl, buf, (int)sizeof(buf));
    if (n <= 0)
      break;
    while (len + (size_t)n + 1 > cap) {
      cap *= 2;
      resp = (char *)xrealloc(resp, cap);
    }
    memcpy(resp + len, buf, (size_t)n);
    len += (size_t)n;
  }
  resp[len] = '\0';
  if (p_ssl_shutdown)
    p_ssl_shutdown(ssl);
  p_ssl_free(ssl);
  p_ctx_free(ctx);
  iris_tcp_close(fd);

  char *body = strstr(resp, "\r\n\r\n");
  if (body) {
    body += 4;
    size_t blen = len - (size_t)(body - resp);
    char *result = (char *)xmalloc(blen + 1);
    memcpy(result, body, blen);
    result[blen] = '\0';
    xfree(resp);
    return result;
  }
  return resp;
}
#endif

char *iris_http_get(const char *url) {
  iris_sandbox_require_url_network(url);
#ifdef _WIN32
  if (url && strncmp(url, "https://", 8) == 0)
    return iris_winhttp_request("GET", url, "", "", 30000);
#endif
  char host[256] = {0}, path[2048] = {0};
  int port = 80;
  if (parse_url(url, host, &port, path) != 0) {
    char *e = (char *)xmalloc(1);
    e[0] = '\0';
    return e;
  }
#ifndef _WIN32
  if (url && strncmp(url, "https://", 8) == 0) {
    char *tls_resp = iris_openssl_https_get(host, port, path);
    if (tls_resp)
      return tls_resp;
  }
#endif
  int64_t fd = iris_tcp_connect(host, port);
  if (fd < 0) {
    char *e = (char *)xmalloc(1);
    e[0] = '\0';
    return e;
  }

  /* Send HTTP/1.0 GET request */
  char req[4096];
  snprintf(req, sizeof(req),
           "GET %s HTTP/1.0\r\nHost: %s\r\nConnection: close\r\n\r\n", path,
           host);
  iris_tcp_write(fd, req);

  /* Read full response */
  size_t cap = 16384, len = 0;
  char *resp = (char *)xmalloc(cap);
  for (;;) {
    char buf[4096];
    int n;
#ifdef _WIN32
    n = recv((SOCKET)fd, buf, sizeof(buf), 0);
#else
    n = recv((int)fd, buf, sizeof(buf), 0);
#endif
    if (n <= 0)
      break;
    while (len + n + 1 > cap) {
      cap *= 2;
      resp = (char *)xrealloc(resp, cap);
    }
    memcpy(resp + len, buf, n);
    len += n;
  }
  resp[len] = '\0';
  iris_tcp_close(fd);

  /* Skip HTTP headers — find \r\n\r\n */
  char *body = strstr(resp, "\r\n\r\n");
  if (body) {
    body += 4;
    size_t blen = len - (body - resp);
    char *result = (char *)xmalloc(blen + 1);
    memcpy(result, body, blen);
    result[blen] = '\0';
    xfree(resp);
    return result;
  }
  return resp; /* No header separator found — return as-is */
}

char *iris_http_post(const char *url, const char *body,
                     const char *content_type) {
  iris_sandbox_require_url_network(url);
#ifdef _WIN32
  if (url && strncmp(url, "https://", 8) == 0)
    return iris_winhttp_request("POST", url, body, "", 30000);
#endif
  char host[256] = {0}, path[2048] = {0};
  int port = 80;
  if (parse_url(url, host, &port, path) != 0) {
    char *e = (char *)xmalloc(1);
    e[0] = '\0';
    return e;
  }
  int64_t fd = iris_tcp_connect(host, port);
  if (fd < 0) {
    char *e = (char *)xmalloc(1);
    e[0] = '\0';
    return e;
  }

  size_t body_len = body ? strlen(body) : 0;
  char req[8192];
  snprintf(req, sizeof(req),
           "POST %s HTTP/1.0\r\nHost: %s\r\nContent-Type: "
           "%s\r\nContent-Length: %zu\r\nConnection: close\r\n\r\n",
           path, host, content_type ? content_type : "text/plain", body_len);
  iris_tcp_write(fd, req);
  if (body)
    iris_tcp_write(fd, body);

  size_t cap = 16384, len = 0;
  char *resp = (char *)xmalloc(cap);
  for (;;) {
    char buf[4096];
    int n;
#ifdef _WIN32
    n = recv((SOCKET)fd, buf, sizeof(buf), 0);
#else
    n = recv((int)fd, buf, sizeof(buf), 0);
#endif
    if (n <= 0)
      break;
    while (len + n + 1 > cap) {
      cap *= 2;
      resp = (char *)xrealloc(resp, cap);
    }
    memcpy(resp + len, buf, n);
    len += n;
  }
  resp[len] = '\0';
  iris_tcp_close(fd);

  char *hdr_end = strstr(resp, "\r\n\r\n");
  if (hdr_end) {
    hdr_end += 4;
    size_t blen = len - (hdr_end - resp);
    char *result = (char *)xmalloc(blen + 1);
    memcpy(result, hdr_end, blen);
    result[blen] = '\0';
    xfree(resp);
    return result;
  }
  return resp;
}

char *iris_http_post_json(const char *url, const char *json_body) {
  return iris_http_post(url, json_body, "application/json");
}

char *iris_http_request(const char *method, const char *url, const char *body) {
  iris_sandbox_require_url_network(url);
  const char *m = (method && *method) ? method : "GET";
#ifdef _WIN32
  if (url && strncmp(url, "https://", 8) == 0)
    return iris_winhttp_request(m, url, body, "", 30000);
#endif
  const char *content_type = "application/json";
  if (strcmp(m, "GET") == 0 && (!body || strlen(body) == 0)) {
    return iris_http_get(url);
  }
  char host[256] = {0}, path[2048] = {0};
  int port = 80;
  if (parse_url(url, host, &port, path) != 0) {
    char *e = (char *)xmalloc(1);
    e[0] = '\0';
    return e;
  }
  int64_t fd = iris_tcp_connect(host, port);
  if (fd < 0) {
    char *e = (char *)xmalloc(1);
    e[0] = '\0';
    return e;
  }

  size_t body_len = body ? strlen(body) : 0;
  char req[8192];
  if (body_len > 0) {
    snprintf(req, sizeof(req),
             "%s %s HTTP/1.0\r\nHost: %s\r\nContent-Type: "
             "%s\r\nContent-Length: %zu\r\nConnection: close\r\n\r\n",
             m, path, host, content_type, body_len);
  } else {
    snprintf(req, sizeof(req),
             "%s %s HTTP/1.0\r\nHost: %s\r\nConnection: close\r\n\r\n", m, path,
             host);
  }
  iris_tcp_write(fd, req);
  if (body && body_len > 0)
    iris_tcp_write(fd, body);

  size_t cap = 16384, len = 0;
  char *resp = (char *)xmalloc(cap);
  for (;;) {
    char buf[4096];
    int n;
#ifdef _WIN32
    n = recv((SOCKET)fd, buf, sizeof(buf), 0);
#else
    n = recv((int)fd, buf, sizeof(buf), 0);
#endif
    if (n <= 0)
      break;
    while (len + n + 1 > cap) {
      cap *= 2;
      resp = (char *)xrealloc(resp, cap);
    }
    memcpy(resp + len, buf, n);
    len += n;
  }
  resp[len] = '\0';
  iris_tcp_close(fd);

  char *hdr_end = strstr(resp, "\r\n\r\n");
  if (hdr_end) {
    hdr_end += 4;
    size_t blen = len - (hdr_end - resp);
    char *result = (char *)xmalloc(blen + 1);
    memcpy(result, hdr_end, blen);
    result[blen] = '\0';
    xfree(resp);
    return result;
  }
  return resp;
}

char *iris_http_request_headers(const char *method, const char *url,
                                const char *body, const char *headers,
                                int64_t timeout_ms) {
  iris_sandbox_require_url_network(url);
  iris_http_status = 0;
  const char *m = (method && *method) ? method : "GET";
#ifdef _WIN32
  if (url && strncmp(url, "https://", 8) == 0) {
    return iris_winhttp_request(m, url, body, headers, timeout_ms);
  }
#else
  if (url && strncmp(url, "https://", 8) == 0) {
    char *empty = (char *)xmalloc(1);
    empty[0] = '\0';
    return empty;
  }
#endif
  char host[256] = {0}, path[2048] = {0};
  int port = 80;
  if (parse_url(url, host, &port, path) != 0) {
    char *empty = (char *)xmalloc(1);
    empty[0] = '\0';
    return empty;
  }
  int64_t fd = iris_tcp_connect(host, port);
  if (fd < 0) {
    char *empty = (char *)xmalloc(1);
    empty[0] = '\0';
    return empty;
  }
  int64_t timeout = timeout_ms > 0 ? timeout_ms : 30000;
  (void)iris_tcp_set_timeout(fd, timeout, timeout);

  const char *payload = body ? body : "";
  const char *extra = headers ? headers : "";
  size_t payload_len = strlen(payload);
  size_t request_capacity = strlen(m) + strlen(path) + strlen(host) +
                            strlen(extra) + payload_len + 512;
  char *request_text = (char *)xmalloc(request_capacity);
  int request_len = snprintf(request_text, request_capacity,
                             "%s %s HTTP/1.1\r\nHost: %s\r\nContent-Type: "
                             "application/json\r\n%sContent-Length: "
                             "%zu\r\nConnection: close\r\n\r\n%s",
                             m, path, host, extra, payload_len, payload);
  if (request_len < 0 || (size_t)request_len >= request_capacity ||
      iris_tcp_write_all(fd, request_text) < 0) {
    xfree(request_text);
    iris_tcp_close(fd);
    char *empty = (char *)xmalloc(1);
    empty[0] = '\0';
    return empty;
  }
  xfree(request_text);

  size_t capacity = 16384, length = 0;
  char *response = (char *)xmalloc(capacity);
  for (;;) {
    char buffer[8192];
#ifdef _WIN32
    int count = recv((SOCKET)fd, buffer, sizeof(buffer), 0);
#else
    ssize_t count = recv((int)fd, buffer, sizeof(buffer), 0);
#endif
    if (count <= 0)
      break;
    if (length + (size_t)count + 1 > 64 * 1024 * 1024) {
      xfree(response);
      iris_tcp_close(fd);
      char *empty = (char *)xmalloc(1);
      empty[0] = '\0';
      return empty;
    }
    while (length + (size_t)count + 1 > capacity)
      capacity *= 2;
    response = (char *)xrealloc(response, capacity);
    memcpy(response + length, buffer, (size_t)count);
    length += (size_t)count;
  }
  iris_tcp_close(fd);
  response[length] = '\0';
  {
    long long parsed_status = 0;
    if (sscanf(response, "HTTP/%*s %lld", &parsed_status) == 1)
      iris_http_status = (int64_t)parsed_status;
  }
  char *body_start = strstr(response, "\r\n\r\n");
  if (!body_start)
    return response;
  body_start += 4;
  size_t body_len = length - (size_t)(body_start - response);
  char *result = (char *)xmalloc(body_len + 1);
  memcpy(result, body_start, body_len);
  result[body_len] = '\0';
  xfree(response);
  return result;
}

int64_t iris_http_last_status(void) { return iris_http_status; }

int64_t iris_http_tls_available(void) {
#ifdef _WIN32
  return 1;
#else
  static int checked = 0;
  static int available = 0;
  if (!checked) {
    checked = 1;
    void *h1 = dlopen("libssl.so.3", RTLD_LAZY);
    if (!h1)
      h1 = dlopen("libssl.so.1.1", RTLD_LAZY);
    if (!h1)
      h1 = dlopen("libssl.dylib", RTLD_LAZY);
    if (!h1)
      h1 = dlopen("libcurl.so", RTLD_LAZY);
    if (!h1)
      h1 = dlopen("libcurl.dylib", RTLD_LAZY);
    if (h1) {
      available = 1;
      dlclose(h1);
    }
  }
  return available;
#endif
}
#else  /* __IRIS_WASM_STUB */
char *iris_http_get(const char *url) {
  iris_sandbox_require_url_network(url);
  (void)url;
  char *e = xmalloc(1);
  *e = '\0';
  return e;
}
char *iris_http_post(const char *url, const char *body,
                     const char *content_type) {
  iris_sandbox_require_url_network(url);
  (void)url;
  (void)body;
  (void)content_type;
  char *e = xmalloc(1);
  *e = '\0';
  return e;
}
char *iris_http_post_json(const char *url, const char *json_body) {
  return iris_http_post(url, json_body, "application/json");
}
char *iris_http_request(const char *method, const char *url, const char *body) {
  iris_sandbox_require_url_network(url);
  (void)method;
  (void)url;
  (void)body;
  char *e = xmalloc(1);
  *e = '\0';
  return e;
}
char *iris_http_request_headers(const char *method, const char *url,
                                const char *body, const char *headers,
                                int64_t timeout_ms) {
  iris_sandbox_require_url_network(url);
  (void)method;
  (void)url;
  (void)body;
  (void)headers;
  (void)timeout_ms;
  char *e = xmalloc(1);
  *e = '\0';
  return e;
}
int64_t iris_http_last_status(void) { return 0; }
int64_t iris_http_tls_available(void) { return 0; }
#endif /* __IRIS_WASM_STUB */

/* ======================================================================== */
/*  JSON (minimal recursive descent parser + serializer)                    */
/* ======================================================================== */

static const char *json_skip_ws(const char *p) {
  while (*p == ' ' || *p == '\t' || *p == '\n' || *p == '\r')
    p++;
  return p;
}

static IrisVal *json_parse_value(const char **p);

static IrisVal *json_parse_string(const char **p) {
  if (**p != '"')
    return iris_box_str("");
  (*p)++;
  const char *start = *p;
  size_t cap = 256, len = 0;
  char *buf = (char *)xmalloc(cap);
  while (**p && **p != '"') {
    if (**p == '\\') {
      (*p)++;
      char c = **p;
      switch (c) {
      case 'n':
        buf[len++] = '\n';
        break;
      case 't':
        buf[len++] = '\t';
        break;
      case 'r':
        buf[len++] = '\r';
        break;
      case '"':
        buf[len++] = '"';
        break;
      case '\\':
        buf[len++] = '\\';
        break;
      case '/':
        buf[len++] = '/';
        break;
      default:
        buf[len++] = c;
        break;
      }
    } else {
      buf[len++] = **p;
    }
    if (len + 2 >= cap) {
      cap *= 2;
      buf = (char *)xrealloc(buf, cap);
    }
    (*p)++;
  }
  if (**p == '"')
    (*p)++;
  buf[len] = '\0';
  IrisVal *v = iris_box_str(buf);
  xfree(buf);
  return v;
}

static IrisVal *json_parse_number(const char **p) {
  const char *start = *p;
  int is_float = 0;
  if (**p == '-')
    (*p)++;
  while (**p >= '0' && **p <= '9')
    (*p)++;
  if (**p == '.') {
    is_float = 1;
    (*p)++;
    while (**p >= '0' && **p <= '9')
      (*p)++;
  }
  if (**p == 'e' || **p == 'E') {
    is_float = 1;
    (*p)++;
    if (**p == '+' || **p == '-')
      (*p)++;
    while (**p >= '0' && **p <= '9')
      (*p)++;
  }
  if (is_float)
    return iris_box_f64(strtod(start, NULL));
  return iris_box_i64(strtoll(start, NULL, 10));
}

static IrisVal *json_parse_array(const char **p) {
  (*p)++; /* skip '[' */
  IrisList *list = iris_list_new();
  *p = json_skip_ws(*p);
  if (**p == ']') {
    (*p)++;
    return iris_box_list(list);
  }
  for (;;) {
    IrisVal *elem = json_parse_value(p);
    iris_list_push(list, elem);
    *p = json_skip_ws(*p);
    if (**p == ',') {
      (*p)++;
      *p = json_skip_ws(*p);
    } else
      break;
  }
  if (**p == ']')
    (*p)++;
  return iris_box_list(list);
}

static IrisVal *json_parse_object(const char **p) {
  (*p)++; /* skip '{' */
  IrisMap *map = iris_map_new();
  *p = json_skip_ws(*p);
  if (**p == '}') {
    (*p)++;
    return iris_box_map(map);
  }
  for (;;) {
    *p = json_skip_ws(*p);
    /* Parse key (must be a string) */
    if (**p != '"')
      break;
    IrisVal *kv = json_parse_string(p);
    char *key = kv->str ? kv->str : "";
    *p = json_skip_ws(*p);
    if (**p == ':')
      (*p)++;
    *p = json_skip_ws(*p);
    IrisVal *val = json_parse_value(p);
    (void)key;
    iris_map_set(map, kv, val);
    iris_drop_box(kv);
    *p = json_skip_ws(*p);
    if (**p == ',') {
      (*p)++;
    } else
      break;
  }
  if (**p == '}')
    (*p)++;
  return iris_box_map(map);
}

static IrisVal *json_parse_value(const char **p) {
  *p = json_skip_ws(*p);
  if (**p == '"')
    return json_parse_string(p);
  if (**p == '{')
    return json_parse_object(p);
  if (**p == '[')
    return json_parse_array(p);
  if (**p == 't' && strncmp(*p, "true", 4) == 0) {
    *p += 4;
    return iris_box_bool(1);
  }
  if (**p == 'f' && strncmp(*p, "false", 5) == 0) {
    *p += 5;
    return iris_box_bool(0);
  }
  if (**p == 'n' && strncmp(*p, "null", 4) == 0) {
    *p += 4;
    return iris_box_option(iris_make_none());
  }
  if (**p == '-' || (**p >= '0' && **p <= '9'))
    return json_parse_number(p);
  return iris_box_str(""); /* parse error fallback */
}

IrisVal *iris_json_parse(const char *str) {
  if (!str)
    return iris_box_str("");
  const char *p = str;
  return json_parse_value(&p);
}

/* Stringify helper — recursive */
static void json_stringify_val(IrisVal *v, char **out, size_t *len,
                               size_t *cap) {
#define JSON_APPEND(s)                                                         \
  do {                                                                         \
    size_t slen = strlen(s);                                                   \
    while (*len + slen + 1 > *cap) {                                           \
      *cap *= 2;                                                               \
      *out = (char *)xrealloc(*out, *cap);                                     \
    }                                                                          \
    memcpy(*out + *len, s, slen);                                              \
    *len += slen;                                                              \
  } while (0)
#define JSON_APPEND_CHAR(c)                                                    \
  do {                                                                         \
    if (*len + 2 > *cap) {                                                     \
      *cap *= 2;                                                               \
      *out = (char *)xrealloc(*out, *cap);                                     \
    }                                                                          \
    (*out)[(*len)++] = (c);                                                    \
  } while (0)

  if (!v) {
    JSON_APPEND("null");
    return;
  }
  switch (v->tag) {
  case IRIS_TAG_I64: {
    char buf[32];
    snprintf(buf, sizeof(buf), "%lld", (long long)v->i64);
    JSON_APPEND(buf);
    break;
  }
  case IRIS_TAG_I32: {
    char buf[32];
    snprintf(buf, sizeof(buf), "%d", v->i32);
    JSON_APPEND(buf);
    break;
  }
  case IRIS_TAG_F64: {
    char buf[400];
    iris_format_float(v->f64, 0, buf, sizeof(buf));
    JSON_APPEND(buf);
    break;
  }
  case IRIS_TAG_F32: {
    char buf[400];
    iris_format_float((double)v->f32, 1, buf, sizeof(buf));
    JSON_APPEND(buf);
    break;
  }
  case IRIS_TAG_BOOL:
    JSON_APPEND(v->boolean ? "true" : "false");
    break;
  case IRIS_TAG_STR: {
    JSON_APPEND_CHAR('"');
    if (v->str) {
      for (const char *s = v->str; *s; s++) {
        switch (*s) {
        case '"':
          JSON_APPEND("\\\"");
          break;
        case '\\':
          JSON_APPEND("\\\\");
          break;
        case '\n':
          JSON_APPEND("\\n");
          break;
        case '\r':
          JSON_APPEND("\\r");
          break;
        case '\t':
          JSON_APPEND("\\t");
          break;
        default:
          JSON_APPEND_CHAR(*s);
          break;
        }
      }
    }
    JSON_APPEND_CHAR('"');
    break;
  }
  case IRIS_TAG_LIST: {
    IrisList *l = (IrisList *)v->ptr;
    JSON_APPEND_CHAR('[');
    if (l) {
      for (size_t i = 0; i < l->len; i++) {
        if (i > 0)
          JSON_APPEND_CHAR(',');
        json_stringify_val(l->data[i], out, len, cap);
      }
    }
    JSON_APPEND_CHAR(']');
    break;
  }
  case IRIS_TAG_MAP: {
    IrisMap *m = (IrisMap *)v->ptr;
    JSON_APPEND_CHAR('{');
    int first = 1;
    if (m) {
      size_t n;
      IrisMapEntry **sorted = map_sorted_entries(m, &n);
      for (size_t i = 0; i < n; i++) {
        IrisMapEntry *e = sorted[i];
        if (!first)
          JSON_APPEND_CHAR(',');
        first = 0;
        JSON_APPEND_CHAR('"');
        if (e->key)
          JSON_APPEND(e->key);
        JSON_APPEND_CHAR('"');
        JSON_APPEND_CHAR(':');
        json_stringify_val(e->val, out, len, cap);
      }
      free(sorted);
    }
    JSON_APPEND_CHAR('}');
    break;
  }
  case IRIS_TAG_OPTION: {
    IrisOption *opt = (IrisOption *)v->ptr;
    if (opt && opt->has_value)
      json_stringify_val(opt->value, out, len, cap);
    else
      JSON_APPEND("null");
    break;
  }
  case IRIS_TAG_RESULT: {
    IrisResult *res = (IrisResult *)v->ptr;
    if (res && res->is_ok) {
      JSON_APPEND("{\"ok\":");
      json_stringify_val(res->value, out, len, cap);
      JSON_APPEND_CHAR('}');
    } else {
      JSON_APPEND("{\"err\":");
      if (res)
        json_stringify_val(res->value, out, len, cap);
      else
        JSON_APPEND("null");
      JSON_APPEND_CHAR('}');
    }
    break;
  }
  case IRIS_TAG_TUPLE: {
    IrisList *t = (IrisList *)v->ptr;
    JSON_APPEND_CHAR('[');
    if (t) {
      for (size_t i = 0; i < t->len; i++) {
        if (i > 0)
          JSON_APPEND_CHAR(',');
        json_stringify_val(t->data[i], out, len, cap);
      }
    }
    JSON_APPEND_CHAR(']');
    break;
  }
  case IRIS_TAG_STRUCT: {
    // Struct is stored as IrisList of field values.
    // Field names are not available at runtime; use numeric indices.
    IrisList *fields = (IrisList *)v->ptr;
    JSON_APPEND_CHAR('{');
    if (fields) {
      for (size_t i = 0; i < fields->len; i++) {
        if (i > 0)
          JSON_APPEND_CHAR(',');
        char idx_buf[32];
        snprintf(idx_buf, sizeof(idx_buf), "\"%zu\":", i);
        JSON_APPEND(idx_buf);
        json_stringify_val(fields->data[i], out, len, cap);
      }
    }
    JSON_APPEND_CHAR('}');
    break;
  }
  case IRIS_TAG_ENUM: {
    IrisEnum *e = (IrisEnum *)v->ptr;
    if (e) {
      char buf[64];
      snprintf(buf, sizeof(buf), "{\"tag\":%lld,\"fields\":[",
               (long long)e->tag);
      JSON_APPEND(buf);
      for (size_t i = 0; i < e->len; i++) {
        if (i > 0)
          JSON_APPEND_CHAR(',');
        json_stringify_val(e->fields[i], out, len, cap);
      }
      JSON_APPEND("]}");
    } else {
      JSON_APPEND("null");
    }
    break;
  }
  case IRIS_TAG_UNIT:
    JSON_APPEND("null");
    break;
  default:
    JSON_APPEND("null");
    break;
  }
#undef JSON_APPEND
#undef JSON_APPEND_CHAR
}

char *iris_json_stringify(IrisVal *val) {
  size_t cap = 256, len = 0;
  char *out = (char *)xmalloc(cap);
  json_stringify_val(val, &out, &len, &cap);
  out[len] = '\0';
  return out;
}

char *iris_json_query(const char *json, const char *path) {
  if (!json || !path)
    return xstrdup("");
  const char *cursor = json_skip_ws(json);
  IrisVal *root = json_parse_value(&cursor);
  IrisVal *current = root;
  const char *segment = path;
  char *out = NULL;
  while (*segment) {
    const char *dot = strchr(segment, '.');
    size_t length = dot ? (size_t)(dot - segment) : strlen(segment);
    if (length == 0 || length >= 256 || !current) {
      out = xstrdup("");
      goto done;
    }
    char key[256];
    memcpy(key, segment, length);
    key[length] = '\0';
    if (current->tag == IRIS_TAG_MAP) {
      IrisMap *map = (IrisMap *)current->ptr;
      IrisVal *found = NULL;
      if (map) {
        for (size_t bucket = 0; bucket < map->n_buckets && !found; bucket++) {
          for (IrisMapEntry *entry = map->buckets[bucket]; entry;
               entry = entry->next) {
            if (entry->key && strcmp(entry->key, key) == 0) {
              found = entry->val;
              break;
            }
          }
        }
      }
      current = found;
    } else if (current->tag == IRIS_TAG_LIST) {
      char *endptr = NULL;
      long index = strtol(key, &endptr, 10);
      IrisList *list = (IrisList *)current->ptr;
      if (!endptr || *endptr != '\0' || index < 0 || !list ||
          (size_t)index >= list->len) {
        out = xstrdup("");
        goto done;
      }
      current = list->data[index];
    } else {
      out = xstrdup("");
      goto done;
    }
    if (!current) {
      out = xstrdup("");
      goto done;
    }
    if (!dot)
      break;
    segment = dot + 1;
  }
  if (!current) {
    out = xstrdup("");
  } else if (current->tag == IRIS_TAG_STR) {
    out = xstrdup(current->str ? current->str : "");
  } else {
    out = iris_json_stringify(current);
  }
done:
  iris_drop_box(root);
  return out;
}

/* ======================================================================== */
/*  Set collection (uses list with linear search — simple and correct)      */
/* ======================================================================== */

/* iris_val_equal already defined above */

IrisList *iris_set_new(void) { return iris_list_new(); }

void iris_set_add(IrisList *set, IrisVal *val) {
  if (!set || !val)
    return;
  for (size_t i = 0; i < set->len; i++) {
    if (iris_val_equal(set->data[i], val))
      return; /* already present */
  }
  iris_list_push(set, val);
}

int iris_set_contains(IrisList *set, IrisVal *val) {
  if (!set || !val)
    return 0;
  for (size_t i = 0; i < set->len; i++) {
    if (iris_val_equal(set->data[i], val))
      return 1;
  }
  return 0;
}

void iris_set_remove(IrisList *set, IrisVal *val) {
  if (!set || !val)
    return;
  iris_tx_record_list(set);
  for (size_t i = 0; i < set->len; i++) {
    if (iris_val_equal(set->data[i], val)) {
      /* Shift remaining elements */
      for (size_t j = i; j + 1 < set->len; j++)
        set->data[j] = set->data[j + 1];
      set->len--;
      return;
    }
  }
}

int64_t iris_set_len(IrisList *set) { return set ? (int64_t)set->len : 0; }

IrisList *iris_set_to_list(IrisList *set) {
  IrisList *out = iris_list_new();
  if (!set)
    return out;
  for (size_t i = 0; i < set->len; i++)
    iris_list_push(out, set->data[i]);
  return out;
}

/* ======================================================================== */
/*  Regex (simple pattern matching — no external dependency)                */
/* ======================================================================== */
/* We implement a simple regex subset: exact match, ., *, +, ?, ^, $        */
/* Extended: [...] char classes, (...) grouping, | alternation,              */
/*           \d \w \s shorthand classes, [^...] negated classes             */
/* For full regex, compiled code can use platform regex via FFI.             */

static int simple_match(const char *pat, const char *str);
static int match_here(const char *re, const char *text);

static int match_char_class(const char *cc, char c, const char **end) {
  /* cc points past the opening '['. Returns 1 if c matches, sets *end past ']'.
   */
  int negated = 0;
  if (*cc == '^') {
    negated = 1;
    cc++;
  }
  int matched = 0;
  while (*cc != '\0' && *cc != ']') {
    char lo = *cc;
    if (cc[1] == '-' && cc[2] != '\0' && cc[2] != ']') {
      /* Range: lo-hi */
      char hi = cc[2];
      if (c >= lo && c <= hi)
        matched = 1;
      cc += 3;
      if (*cc == '-') {
        cc++;
      } /* skip trailing dash if present */
    } else if (lo == '\\' && cc[1] != '\0') {
      /* Escape inside class */
      cc++;
      char esc = *cc++;
      if (esc == 'd' && isdigit((unsigned char)c))
        matched = 1;
      else if (esc == 'w' && (isalnum((unsigned char)c) || c == '_'))
        matched = 1;
      else if (esc == 's' && (c == ' ' || c == '\t' || c == '\n' || c == '\r'))
        matched = 1;
      else if (c == esc)
        matched = 1;
    } else {
      if (c == lo)
        matched = 1;
      cc++;
    }
  }
  if (*cc == ']')
    cc++;
  *end = cc;
  return negated ? !matched : matched;
}

static int match_here(const char *re, const char *text) {
  if (re[0] == '\0')
    return 1;
  if (re[0] == '$' && re[1] == '\0')
    return *text == '\0';

  /* Grouping: (sub-pattern) */
  if (re[0] == '(') {
    /* Find matching ')' accounting for nested parens */
    int depth = 1;
    const char *p = re + 1;
    while (*p != '\0' && depth > 0) {
      if (*p == '(')
        depth++;
      else if (*p == ')')
        depth--;
      p++;
    }
    /* p now points past the matching ')' */
    size_t group_len = (size_t)(p - re - 2); /* exclude parens */
    char *group = (char *)xmalloc(group_len + 1);
    memcpy(group, re + 1, group_len);
    group[group_len] = '\0';

    /* Check for alternation inside the group */
    int found_alt = 0;
    int adepth = 0;
    for (size_t i = 0; i < group_len; i++) {
      if (group[i] == '(')
        adepth++;
      else if (group[i] == ')')
        adepth--;
      else if (group[i] == '|' && adepth == 0) {
        found_alt = 1;
        break;
      }
    }

    if (found_alt) {
      /* Split on | and try each alternative */
      char *save = group;
      char *alt_start = group;
      int match_result = 0;
      while (1) {
        char *sep = NULL;
        adepth = 0;
        for (char *q = alt_start; *q != '\0'; q++) {
          if (*q == '(')
            adepth++;
          else if (*q == ')')
            adepth--;
          else if (*q == '|' && adepth == 0) {
            sep = q;
            break;
          }
        }
        if (sep)
          *sep = '\0';
        /* Try matching alt_start then the rest of re */
        /* Build combined pattern: alt + rest_of_re */
        size_t alt_len = strlen(alt_start);
        size_t rest_len = strlen(p);
        char *combined = (char *)xmalloc(alt_len + rest_len + 1);
        memcpy(combined, alt_start, alt_len);
        memcpy(combined + alt_len, p, rest_len + 1);
        if (simple_match(combined, text)) {
          match_result = 1;
          xfree(combined);
          break;
        }
        xfree(combined);
        if (sep) {
          alt_start = sep + 1;
        } else
          break;
      }
      xfree(save);
      return match_result;
    } else {
      /* No alternation: try matching the group content then the rest */
      size_t rest_len = strlen(p);
      char *combined = (char *)xmalloc(group_len + rest_len + 1);
      memcpy(combined, group, group_len);
      memcpy(combined + group_len, p, rest_len + 1);
      int result = simple_match(combined, text);
      xfree(combined);
      xfree(group);
      return result;
    }
  }

  /* Escape sequences: \d, \w, \s */
  if (re[0] == '\\' && re[1] != '\0') {
    char esc = re[1];
    int match = 0;
    if (esc == 'd')
      match = isdigit((unsigned char)*text);
    else if (esc == 'w')
      match = (isalnum((unsigned char)*text) || *text == '_');
    else if (esc == 's')
      match = (*text == ' ' || *text == '\t' || *text == '\n' || *text == '\r');
    else
      match = (*text == esc);
    const char *after_esc = re + 2;
    if (after_esc[0] == '+' || after_esc[0] == '*' || after_esc[0] == '?') {
      char q = after_esc[0];
      after_esc++;
      if (q == '+') {
        /* One or more */
        if (!match || *text == '\0')
          return 0;
        text++;
        /* Greedy: try matching rest first, then consume more */
        do {
          if (match_here(after_esc, text))
            return 1;
        } while (
            *text != '\0' &&
            ((esc == 'd' && isdigit((unsigned char)*text)) ||
             (esc == 'w' && (isalnum((unsigned char)*text) || *text == '_')) ||
             (esc == 's' && (*text == ' ' || *text == '\t' || *text == '\n' ||
                             *text == '\r')) ||
             (esc != 'd' && esc != 'w' && esc != 's' && *text == esc)));
        return 0;
      } else if (q == '*') {
        /* Zero or more */
        do {
          if (match_here(after_esc, text))
            return 1;
        } while (
            *text != '\0' &&
            ((esc == 'd' && isdigit((unsigned char)*text)) ||
             (esc == 'w' && (isalnum((unsigned char)*text) || *text == '_')) ||
             (esc == 's' && (*text == ' ' || *text == '\t' || *text == '\n' ||
                             *text == '\r')) ||
             (esc != 'd' && esc != 'w' && esc != 's' && *text == esc)));
        return 0;
      } else {
        /* ? : zero or one */
        if (match && *text != '\0') {
          if (match_here(after_esc, text + 1))
            return 1;
        }
        return match_here(after_esc, text);
      }
    }
    if (match && text[0] != '\0')
      return match_here(after_esc, text + 1);
    return 0;
  }

  /* Character class: [...] */
  if (re[0] == '[') {
    const char *end;
    int cm = match_char_class(re + 1, *text, &end);
    if (*end == '+' || *end == '*' || *end == '?') {
      char q = *end;
      end++;
      if (q == '+') {
        /* One or more */
        if (!cm || *text == '\0')
          return 0;
        text++;
        if (match_here(end, text))
          return 1;
        while (*text != '\0') {
          const char *dummy;
          if (match_char_class(re + 1, *text, &dummy)) {
            text++;
            if (match_here(end, text))
              return 1;
          } else
            break;
        }
        return 0;
      } else if (q == '*') {
        /* Zero or more */
        if (match_here(end, text))
          return 1;
        while (*text != '\0') {
          const char *dummy;
          if (match_char_class(re + 1, *text, &dummy)) {
            text++;
            if (match_here(end, text))
              return 1;
          } else
            break;
        }
        return 0;
      } else {
        /* ? : zero or one */
        if (cm && *text != '\0') {
          if (match_here(end, text + 1))
            return 1;
        }
        return match_here(end, text);
      }
    }
    if (cm && *text != '\0')
      return match_here(end, text + 1);
    return 0;
  }

  if (re[1] == '*') {
    /* Match zero or more of re[0] */
    do {
      if (match_here(re + 2, text))
        return 1;
    } while (*text != '\0' && (re[0] == '.' || *text == re[0]) && text++);
    return 0;
  }
  if (re[1] == '+') {
    /* Match one or more of re[0] */
    while (*text != '\0' && (re[0] == '.' || *text == re[0])) {
      text++;
      if (match_here(re + 2, text))
        return 1;
    }
    return 0;
  }
  if (re[1] == '?') {
    /* Match zero or one of re[0] */
    if (match_here(re + 2, text))
      return 1;
    if (*text != '\0' && (re[0] == '.' || *text == re[0]))
      return match_here(re + 2, text + 1);
    return 0;
  }
  if (*text != '\0' && (re[0] == '.' || *text == re[0]))
    return match_here(re + 1, text + 1);
  return 0;
}

static int simple_match(const char *pat, const char *str) {
  if (pat[0] == '^')
    return match_here(pat + 1, str);
  /* Unanchored: try at every position */
  do {
    if (match_here(pat, str))
      return 1;
  } while (*str++ != '\0');
  return 0;
}

int iris_regex_match(const char *pattern, const char *str) {
  if (!pattern || !str)
    return 0;
  return simple_match(pattern, str);
}

/*
 * mpl: match prefix length — returns chars consumed, or -1.
 */
static int mpl(const char *re, const char *text) {
  if (re[0] == '\0')
    return 0;
  if (re[0] == '$' && re[1] == '\0')
    return (*text == '\0') ? 0 : -1;

  /* Grouping: (sub-pattern) */
  if (re[0] == '(') {
    int depth = 1;
    const char *p = re + 1;
    while (*p != '\0' && depth > 0) {
      if (*p == '(')
        depth++;
      else if (*p == ')') {
        depth--;
        if (depth == 0) {
          p++;
          break;
        }
      }
      p++;
    }
    size_t glen = (size_t)(p - re - 2);
    char *group = (char *)xmalloc(glen + 1);
    memcpy(group, re + 1, glen);
    group[glen] = '\0';
    const char *rest = p;

    int found_alt = 0;
    int ad = 0;
    for (size_t i = 0; i < glen; i++) {
      if (group[i] == '(')
        ad++;
      else if (group[i] == ')')
        ad--;
      else if (group[i] == '|' && ad == 0) {
        found_alt = 1;
        break;
      }
    }

    int best = -1;
    if (found_alt) {
      char *save = group;
      char *astart = group;
      while (1) {
        char *sep = NULL;
        ad = 0;
        for (char *q = astart; *q != '\0'; q++) {
          if (*q == '(')
            ad++;
          else if (*q == ')')
            ad--;
          else if (*q == '|' && ad == 0) {
            sep = q;
            break;
          }
        }
        if (sep)
          *sep = '\0';
        size_t al = strlen(astart);
        size_t rl = strlen(rest);
        char *c = (char *)xmalloc(al + rl + 1);
        memcpy(c, astart, al);
        memcpy(c + al, rest, rl + 1);
        int r = mpl(c, text);
        xfree(c);
        if (r >= 0 && r > best)
          best = r;
        if (sep) {
          *sep = '|';
          astart = sep + 1;
        } else
          break;
      }
      xfree(save);
    } else {
      size_t rl = strlen(rest);
      char *c = (char *)xmalloc(glen + rl + 1);
      memcpy(c, group, glen);
      memcpy(c + glen, rest, rl + 1);
      best = mpl(c, text);
      xfree(c);
      xfree(group);
    }
    return best;
  }

  /* Escape: \d, \w, \s */
  if (re[0] == '\\' && re[1] != '\0') {
    char esc = re[1];
    const char *af = re + 2;
    int m = 0;
    if (esc == 'd')
      m = isdigit((unsigned char)*text);
    else if (esc == 'w')
      m = (isalnum((unsigned char)*text) || *text == '_');
    else if (esc == 's')
      m = (*text == ' ' || *text == '\t' || *text == '\n' || *text == '\r');
    else
      m = (*text == esc);
/* helper macro for counting matching chars */
#define ESC_MATCH(ch)                                                          \
  ((esc == 'd' && isdigit((unsigned char)(ch))) ||                             \
   (esc == 'w' && (isalnum((unsigned char)(ch)) || (ch) == '_')) ||            \
   (esc == 's' &&                                                              \
    ((ch) == ' ' || (ch) == '\t' || (ch) == '\n' || (ch) == '\r')) ||          \
   (esc != 'd' && esc != 'w' && esc != 's' && (ch) == esc))
    if (af[0] == '+' || af[0] == '*' || af[0] == '?') {
      char q = af[0];
      const char *rest = af + 1;
      if (q == '+') {
        if (!m)
          return -1;
        int cnt = 0;
        const char *t = text;
        while (*t != '\0' && ESC_MATCH(*t)) {
          cnt++;
          t++;
        }
        for (int i = cnt; i >= 1; i--) {
          int r = mpl(rest, text + i);
          if (r >= 0)
            return i + r;
        }
        return -1;
      } else if (q == '*') {
        int cnt = 0;
        const char *t = text;
        while (*t != '\0' && ESC_MATCH(*t)) {
          cnt++;
          t++;
        }
        for (int i = cnt; i >= 0; i--) {
          int r = mpl(rest, text + i);
          if (r >= 0)
            return i + r;
        }
        return -1;
      } else {
        if (m && *text != '\0') {
          int r = mpl(rest, text + 1);
          if (r >= 0)
            return 1 + r;
        }
        return mpl(rest, text);
      }
    }
#undef ESC_MATCH
    if (m && *text != '\0') {
      int r = mpl(af, text + 1);
      if (r >= 0)
        return 1 + r;
    }
    return -1;
  }

  /* Character class: [...] */
  if (re[0] == '[') {
    const char *end;
    int cm = match_char_class(re + 1, *text, &end);
    if (*end == '+' || *end == '*' || *end == '?') {
      char q = *end;
      const char *rest = end + 1;
      if (q == '+') {
        if (!cm)
          return -1;
        int cnt = 0;
        const char *t = text;
        while (*t != '\0') {
          const char *d;
          if (!match_char_class(re + 1, *t, &d))
            break;
          cnt++;
          t++;
        }
        for (int i = cnt; i >= 1; i--) {
          int r = mpl(rest, text + i);
          if (r >= 0)
            return i + r;
        }
        return -1;
      } else if (q == '*') {
        int cnt = 0;
        const char *t = text;
        while (*t != '\0') {
          const char *d;
          if (!match_char_class(re + 1, *t, &d))
            break;
          cnt++;
          t++;
        }
        for (int i = cnt; i >= 0; i--) {
          int r = mpl(rest, text + i);
          if (r >= 0)
            return i + r;
        }
        return -1;
      } else {
        if (cm && *text != '\0') {
          int r = mpl(rest, text + 1);
          if (r >= 0)
            return 1 + r;
        }
        return mpl(rest, text);
      }
    }
    if (cm && *text != '\0') {
      int r = mpl(end, text + 1);
      if (r >= 0)
        return 1 + r;
    }
    return -1;
  }

  /* Single char with quantifiers */
  if (re[1] == '*') {
    int cnt = 0;
    const char *t = text;
    while (*t != '\0' && (re[0] == '.' || *t == re[0])) {
      cnt++;
      t++;
    }
    for (int i = cnt; i >= 0; i--) {
      int r = mpl(re + 2, text + i);
      if (r >= 0)
        return i + r;
    }
    return -1;
  }
  if (re[1] == '+') {
    if (*text == '\0' || (re[0] != '.' && *text != re[0]))
      return -1;
    int cnt = 0;
    const char *t = text;
    while (*t != '\0' && (re[0] == '.' || *t == re[0])) {
      cnt++;
      t++;
    }
    for (int i = cnt; i >= 1; i--) {
      int r = mpl(re + 2, text + i);
      if (r >= 0)
        return i + r;
    }
    return -1;
  }
  if (re[1] == '?') {
    if (*text != '\0' && (re[0] == '.' || *text == re[0])) {
      int r = mpl(re + 2, text + 1);
      if (r >= 0)
        return 1 + r;
    }
    return mpl(re + 2, text);
  }
  if (*text != '\0' && (re[0] == '.' || *text == re[0])) {
    int r = mpl(re + 1, text + 1);
    if (r >= 0)
      return 1 + r;
  }
  return -1;
}

IrisList *iris_regex_find_all(const char *pattern, const char *str) {
  IrisList *results = iris_list_new();
  if (!pattern || !str)
    return results;
  const char *pat = pattern;
  if (pat[0] == '^')
    pat++;
  size_t slen = strlen(str);
  size_t pos = 0;
  while (pos < slen) {
    int len = mpl(pat, str + pos);
    if (len > 0) {
      char *match = (char *)xmalloc(len + 1);
      memcpy(match, str + pos, len);
      match[len] = '\0';
      iris_list_push(results, iris_box_str(match));
      xfree(match);
      pos += len;
    } else {
      pos++;
    }
  }
  return results;
}

char *iris_regex_replace(const char *pattern, const char *str,
                         const char *replacement) {
  if (!pattern || !str || !replacement) {
    char *e = (char *)xmalloc(1);
    e[0] = '\0';
    return e;
  }
  const char *pat = pattern[0] == '^' ? pattern + 1 : pattern;
  size_t slen = strlen(str);
  size_t rlen = strlen(replacement);
  size_t cap = slen + rlen + 64;
  char *out = (char *)xmalloc(cap);
  size_t olen = 0;
  size_t pos = 0;
  int replaced = 0;
  while (pos < slen) {
    if (!replaced) {
      int len = mpl(pat, str + pos);
      if (len > 0) {
        while (olen + rlen + 1 > cap) {
          cap *= 2;
          out = (char *)xrealloc(out, cap);
        }
        memcpy(out + olen, replacement, rlen);
        olen += rlen;
        pos += len;
        replaced = 1;
        continue;
      }
    }
    if (olen + 2 > cap) {
      cap *= 2;
      out = (char *)xrealloc(out, cap);
    }
    out[olen++] = str[pos];
    pos++;
  }
  out[olen] = '\0';
  return out;
}

char *iris_regex_replace_all(const char *pattern, const char *str,
                             const char *replacement) {
  if (!pattern || !str || !replacement) {
    char *e = (char *)xmalloc(1);
    e[0] = '\0';
    return e;
  }
  const char *pat = pattern[0] == '^' ? pattern + 1 : pattern;
  size_t slen = strlen(str);
  size_t rlen = strlen(replacement);
  size_t cap = slen * 2 + rlen + 64;
  char *out = (char *)xmalloc(cap);
  size_t olen = 0;
  size_t pos = 0;
  while (pos < slen) {
    int len = mpl(pat, str + pos);
    if (len > 0) {
      while (olen + rlen + 1 > cap) {
        cap *= 2;
        out = (char *)xrealloc(out, cap);
      }
      memcpy(out + olen, replacement, rlen);
      olen += rlen;
      pos += len;
    } else {
      if (olen + 2 > cap) {
        cap *= 2;
        out = (char *)xrealloc(out, cap);
      }
      out[olen++] = str[pos];
      pos++;
    }
  }
  out[olen] = '\0';
  return out;
}

/* ======================================================================== */
/*  DateTime                                                                */
/* ======================================================================== */

char *iris_datetime_now(void) {
  time_t t = time(NULL);
  struct tm *tm = localtime(&t);
  char buf[64];
  strftime(buf, sizeof(buf), "%Y-%m-%dT%H:%M:%S", tm);
  return iris_box_str(buf)->str;
}

int64_t iris_datetime_timestamp(void) { return (int64_t)time(NULL); }

char *iris_datetime_format(int64_t timestamp, const char *fmt) {
  time_t t = (time_t)timestamp;
  struct tm *tm = localtime(&t);
  char buf[256];
  strftime(buf, sizeof(buf), fmt ? fmt : "%Y-%m-%dT%H:%M:%S", tm);
  size_t len = strlen(buf);
  char *result = (char *)xmalloc(len + 1);
  memcpy(result, buf, len + 1);
  return result;
}

/* ======================================================================== */
/*  OS / Path                                                               */
/* ======================================================================== */

char *iris_cwd(void) {
  char buf[4096];
#ifdef _WIN32
  if (_getcwd(buf, sizeof(buf)))
#else
  if (getcwd(buf, sizeof(buf)))
#endif
  {
    size_t len = strlen(buf);
    char *result = (char *)xmalloc(len + 1);
    memcpy(result, buf, len + 1);
    return result;
  }
  char *e = (char *)xmalloc(1);
  e[0] = '\0';
  return e;
}

IrisList *iris_listdir(const char *path) {
  iris_sandbox_require_fs_read(path);
  IrisList *list = iris_list_new();
  if (!path)
    return list;
#ifdef _WIN32
  char pattern[4096];
  snprintf(pattern, sizeof(pattern), "%s\\*", path);
  WIN32_FIND_DATAA fd;
  HANDLE h = FindFirstFileA(pattern, &fd);
  if (h == INVALID_HANDLE_VALUE)
    return list;
  do {
    if (strcmp(fd.cFileName, ".") != 0 && strcmp(fd.cFileName, "..") != 0)
      iris_list_push(list, iris_box_str(fd.cFileName));
  } while (FindNextFileA(h, &fd));
  FindClose(h);
#else
  DIR *d = opendir(path);
  if (!d)
    return list;
  struct dirent *ent;
  while ((ent = readdir(d)) != NULL) {
    if (strcmp(ent->d_name, ".") != 0 && strcmp(ent->d_name, "..") != 0)
      iris_list_push(list, iris_box_str(ent->d_name));
  }
  closedir(d);
#endif
  return list;
}

char *iris_path_join(const char *a, const char *b) {
  if (!a || !b) {
    char *e = (char *)xmalloc(1);
    e[0] = '\0';
    return e;
  }
  size_t alen = strlen(a), blen = strlen(b);
  char *result = (char *)xmalloc(alen + blen + 2);
#ifdef _WIN32
  char sep = '\\';
#else
  char sep = '/';
#endif
  memcpy(result, a, alen);
  if (alen > 0 && a[alen - 1] != '/' && a[alen - 1] != '\\') {
    result[alen] = sep;
    memcpy(result + alen + 1, b, blen);
    result[alen + 1 + blen] = '\0';
  } else {
    memcpy(result + alen, b, blen);
    result[alen + blen] = '\0';
  }
  return result;
}

int iris_path_exists(const char *path) {
  iris_sandbox_require_fs_read(path);
  if (!path)
    return 0;
#ifdef _WIN32
  return _access(path, 0) == 0;
#else
  return access(path, F_OK) == 0;
#endif
}

/* Both return 1 on success and 0 on failure: generated code declares them
 * returning a bool and reads the low bit, so the 0 / -1 they used to return
 * read back inverted -- `mkdir` was false when it succeeded. */

static int iris_is_dir(const char *path) {
#ifdef _WIN32
  DWORD attrs = GetFileAttributesA(path);
  return attrs != INVALID_FILE_ATTRIBUTES && (attrs & FILE_ATTRIBUTE_DIRECTORY);
#elif defined(__wasm__)
  struct stat st;
  return stat(path, &st) == 0 && S_ISDIR(st.st_mode);
#else
  struct stat st;
  return stat(path, &st) == 0 && S_ISDIR(st.st_mode);
#endif
}

/* Create `path` and any missing parents, as the interpreter does; a
 * directory that already exists is success. */
int iris_mkdir(const char *path) {
  iris_sandbox_require_fs_write(path);
  if (!path || !*path)
    return 0;
  size_t n = strlen(path);
  char *buf = (char *)malloc(n + 1);
  if (!buf)
    return 0;
  memcpy(buf, path, n + 1);
  for (size_t i = 1; i <= n; i++) {
    if (i == n || buf[i] == '/' || buf[i] == '\\') {
      char saved = buf[i];
      buf[i] = 0;
      /* Skip a drive prefix such as "C:". */
      if (!(i == 2 && buf[1] == ':') && !iris_is_dir(buf)) {
#ifdef _WIN32
        _mkdir(buf);
#else
        mkdir(buf, 0755);
#endif
      }
      buf[i] = saved;
    }
  }
  free(buf);
  return iris_is_dir(path) ? 1 : 0;
}

int iris_remove_file(const char *path) {
  iris_sandbox_require_fs_write(path);
  if (!path)
    return 0;
  return remove(path) == 0 ? 1 : 0;
}

/* ======================================================================== */
/*  Type introspection                                                      */
/* ======================================================================== */

char *iris_type_of(IrisVal *val) {
  if (!val)
    return iris_box_str("unit")->str;
  const char *name;
  switch (val->tag) {
  case IRIS_TAG_I64:
    name = "i64";
    break;
  case IRIS_TAG_I32:
    name = "i32";
    break;
  case IRIS_TAG_F64:
    name = "f64";
    break;
  case IRIS_TAG_F32:
    name = "f32";
    break;
  case IRIS_TAG_BOOL:
    name = "bool";
    break;
  case IRIS_TAG_STR:
    name = "str";
    break;
  case IRIS_TAG_LIST:
    name = "list";
    break;
  case IRIS_TAG_MAP:
    name = "map";
    break;
  case IRIS_TAG_OPTION:
    name = "option";
    break;
  case IRIS_TAG_RESULT:
    name = "result";
    break;
  case IRIS_TAG_CLOSURE:
    name = "closure";
    break;
  case IRIS_TAG_TUPLE:
    name = "tuple";
    break;
  case IRIS_TAG_STRUCT:
    name = "struct";
    break;
  case IRIS_TAG_CHAN:
    name = "channel";
    break;
  case IRIS_TAG_ATOMIC:
    name = "atomic";
    break;
  case IRIS_TAG_MUTEX:
    name = "mutex";
    break;
  case IRIS_TAG_GRAD:
    name = "grad";
    break;
  case IRIS_TAG_SPARSE:
    name = "sparse";
    break;
  case IRIS_TAG_UNIT:
    name = "unit";
    break;
  case IRIS_TAG_ENUM:
    name = "enum";
    break;
  case IRIS_TAG_NATIVE_OBJECT:
    name = "native_object";
    break;
  case IRIS_TAG_TASK:
    name = "task";
    break;
  default:
    name = "unknown";
    break;
  }
  size_t len = strlen(name);
  char *result = (char *)xmalloc(len + 1);
  memcpy(result, name, len + 1);
  return result;
}

/* ======================================================================== */
/*  Random                                                                  */
/* ======================================================================== */

/*
 * Deterministic, seedable RNG -- SplitMix64.
 *
 * This replaces libc `rand()`/`srand()`, which was wrong here in three
 * separate ways:
 *
 *   1. It could not be seeded from IRIS, so an evolved system could never be
 *      replayed. For a language whose pitch is a *verifiable* autonomy layer,
 *      a self-evolving system that cannot be reproduced cannot be audited --
 *      you cannot ask "how did it get here" and get an answer.
 *   2. `RAND_MAX` is 32767 on this toolchain, so `rand()/RAND_MAX` produced
 *      only 32768 distinct values. Evolutionary search over a genome of f64
 *      genes was quantising every draw to 15 bits.
 *   3. The interpreter used an entirely different generator (a chained
 *      `DefaultHasher`), so the two backends produced different sequences for
 *      the same program -- and kept two independent streams besides, one for
 *      `random` and one for `random_range`.
 *
 * SplitMix64 is used because it is exactly reproducible with plain 64-bit
 * wrapping arithmetic, which means the Rust interpreter can implement it bit
 * for bit. `src/interp/mod.rs` carries the identical constants; the two are
 * asserted equal in tests/test_random_determinism.iris. Changing one without
 * the other reintroduces the divergence this replaced.
 */

static uint64_t iris_rng_state = 0;
static int64_t iris_rng_seed_value = 0;
static int iris_rng_seeded = 0;

static void iris_rng_autoseed(void) {
  if (iris_rng_seeded)
    return;
  uint64_t seed = (uint64_t)time(NULL);
#ifdef _WIN32
  seed ^= ((uint64_t)GetCurrentProcessId() << 16);
  seed ^= ((uint64_t)GetTickCount() & 0xFFFFu);
#else
  seed ^= ((uint64_t)getpid() << 16);
  {
    struct timespec ts;
    clock_gettime(CLOCK_MONOTONIC, &ts);
    seed ^= ((uint64_t)ts.tv_nsec & 0xFFFFu);
  }
#endif
  iris_rng_state = seed;
  iris_rng_seed_value = (int64_t)seed;
  iris_rng_seeded = 1;
}

static uint64_t iris_rng_next(void) {
  iris_rng_autoseed();
  iris_rng_state += 0x9E3779B97F4A7C15ULL;
  uint64_t z = iris_rng_state;
  z = (z ^ (z >> 30)) * 0xBF58476D1CE4E5B9ULL;
  z = (z ^ (z >> 27)) * 0x94D049BB133111EBULL;
  return z ^ (z >> 31);
}

/* Set the stream. Returns the seed, so a run can log what it used. */
int64_t iris_seed(int64_t seed) {
  iris_rng_state = (uint64_t)seed;
  iris_rng_seed_value = seed;
  iris_rng_seeded = 1;
  return seed;
}

/* The seed currently in effect, auto-generating one if never set. This is what
 * makes a run reproducible after the fact: print it, pass it back to seed(). */
int64_t iris_random_seed(void) {
  iris_rng_autoseed();
  return iris_rng_seed_value;
}

double iris_random(void) {
  /* Top 53 bits scaled into [0, 1) -- the full mantissa, no quantisation. */
  return (double)(iris_rng_next() >> 11) / 9007199254740992.0;
}

int64_t iris_random_range(int64_t lo, int64_t hi) {
  if (hi <= lo)
    return lo;
  uint64_t span = (uint64_t)(hi - lo);
  return lo + (int64_t)(iris_rng_next() % span);
}

/* ======================================================================== */
/*  Hashing / Encoding                                                      */
/* ======================================================================== */

int64_t iris_hash(const char *str) {
  if (!str)
    return 0;
  /* djb2 hash */
  uint64_t hash = 5381;
  int c;
  while ((c = (unsigned char)*str++))
    hash = ((hash << 5) + hash) + c;
  return (int64_t)hash;
}

static const char b64_table[] =
    "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

char *iris_base64_encode(const char *str) {
  if (!str) {
    char *e = (char *)xmalloc(1);
    e[0] = '\0';
    return e;
  }
  size_t slen = strlen(str);
  size_t olen = 4 * ((slen + 2) / 3);
  char *out = (char *)xmalloc(olen + 1);
  size_t j = 0;
  for (size_t i = 0; i < slen; i += 3) {
    uint32_t a = (unsigned char)str[i];
    uint32_t b = (i + 1 < slen) ? (unsigned char)str[i + 1] : 0;
    uint32_t c = (i + 2 < slen) ? (unsigned char)str[i + 2] : 0;
    uint32_t triple = (a << 16) | (b << 8) | c;
    out[j++] = b64_table[(triple >> 18) & 0x3F];
    out[j++] = b64_table[(triple >> 12) & 0x3F];
    out[j++] = (i + 1 < slen) ? b64_table[(triple >> 6) & 0x3F] : '=';
    out[j++] = (i + 2 < slen) ? b64_table[triple & 0x3F] : '=';
  }
  out[j] = '\0';
  return out;
}

static int b64_decode_char(char c) {
  if (c >= 'A' && c <= 'Z')
    return c - 'A';
  if (c >= 'a' && c <= 'z')
    return c - 'a' + 26;
  if (c >= '0' && c <= '9')
    return c - '0' + 52;
  if (c == '+')
    return 62;
  if (c == '/')
    return 63;
  return -1;
}

char *iris_base64_decode(const char *str) {
  if (!str) {
    char *e = (char *)xmalloc(1);
    e[0] = '\0';
    return e;
  }
  size_t slen = strlen(str);
  size_t olen = slen / 4 * 3;
  char *out = (char *)xmalloc(olen + 1);
  size_t j = 0;
  for (size_t i = 0; i < slen; i += 4) {
    int a = b64_decode_char(str[i]);
    int b = (i + 1 < slen) ? b64_decode_char(str[i + 1]) : 0;
    int c = (i + 2 < slen) ? b64_decode_char(str[i + 2]) : 0;
    int d = (i + 3 < slen) ? b64_decode_char(str[i + 3]) : 0;
    if (a < 0)
      a = 0;
    if (b < 0)
      b = 0;
    if (c < 0)
      c = 0;
    if (d < 0)
      d = 0;
    uint32_t triple = ((uint32_t)a << 18) | ((uint32_t)b << 12) |
                      ((uint32_t)c << 6) | (uint32_t)d;
    if (j < olen)
      out[j++] = (triple >> 16) & 0xFF;
    if (j < olen && str[i + 2] != '=')
      out[j++] = (triple >> 8) & 0xFF;
    if (j < olen && str[i + 3] != '=')
      out[j++] = triple & 0xFF;
  }
  out[j] = '\0';
  return out;
}

/* ======================================================================== */
/*  String extras                                                           */
/* ======================================================================== */

/* The idx-th character (UTF-8 sequence) of `str`, as the interpreter counts
 * them; "" when there is none. This returned the idx-th byte, which split a
 * multibyte character and disagreed with the interpreter. */
char *iris_char_at(const char *str, int64_t idx) {
  const unsigned char *p = (const unsigned char *)str;
  int64_t n = 0;
  while (p && *p && idx >= 0) {
    size_t w = 1;
    while (p[w] && (p[w] & 0xC0) == 0x80)
      w++;
    if (n == idx) {
      char *result = (char *)xmalloc(w + 1);
      memcpy(result, p, w);
      result[w] = '\0';
      return result;
    }
    p += w;
    n++;
  }
  char *e = (char *)xmalloc(1);
  e[0] = '\0';
  return e;
}

char *iris_str_reverse(const char *str) {
  if (!str) {
    char *e = (char *)xmalloc(1);
    e[0] = '\0';
    return e;
  }
  size_t len = strlen(str);
  char *result = (char *)xmalloc(len + 1);
  for (size_t i = 0; i < len; i++)
    result[i] = str[len - 1 - i];
  result[len] = '\0';
  return result;
}

/* ======================================================================== */
/*  Phase 105: Extended builtins                                            */
/* ======================================================================== */

/* -- String extras -- */

char *iris_str_pad_left(const char *str, int64_t width, const char *pad) {
  if (!str)
    str = "";
  if (!pad || pad[0] == '\0')
    pad = " ";
  size_t slen = strlen(str);
  if ((int64_t)slen >= width) {
    char *r = (char *)xmalloc(slen + 1);
    memcpy(r, str, slen + 1);
    return r;
  }
  size_t pad_needed = (size_t)(width - (int64_t)slen);
  char *r = (char *)xmalloc((size_t)width + 1);
  for (size_t i = 0; i < pad_needed; i++)
    r[i] = pad[0];
  memcpy(r + pad_needed, str, slen + 1);
  return r;
}

char *iris_str_pad_right(const char *str, int64_t width, const char *pad) {
  if (!str)
    str = "";
  if (!pad || pad[0] == '\0')
    pad = " ";
  size_t slen = strlen(str);
  if ((int64_t)slen >= width) {
    char *r = (char *)xmalloc(slen + 1);
    memcpy(r, str, slen + 1);
    return r;
  }
  size_t pad_needed = (size_t)(width - (int64_t)slen);
  char *r = (char *)xmalloc((size_t)width + 1);
  memcpy(r, str, slen);
  for (size_t i = 0; i < pad_needed; i++)
    r[slen + i] = pad[0];
  r[(size_t)width] = '\0';
  return r;
}

IrisList *iris_str_chars(const char *str) {
  if (!str)
    return iris_list_new();
  IrisList *l = iris_list_new();
  size_t len = strlen(str);
  size_t i = 0;
  while (i < len) {
    unsigned char b = (unsigned char)str[i];
    size_t step = 1;
    if ((b & 0xE0) == 0xC0 && i + 1 < len &&
        (((unsigned char)str[i + 1] & 0xC0) == 0x80)) {
      step = 2;
    } else if ((b & 0xF0) == 0xE0 && i + 2 < len &&
               (((unsigned char)str[i + 1] & 0xC0) == 0x80) &&
               (((unsigned char)str[i + 2] & 0xC0) == 0x80)) {
      step = 3;
    } else if ((b & 0xF8) == 0xF0 && i + 3 < len &&
               (((unsigned char)str[i + 1] & 0xC0) == 0x80) &&
               (((unsigned char)str[i + 2] & 0xC0) == 0x80) &&
               (((unsigned char)str[i + 3] & 0xC0) == 0x80)) {
      step = 4;
    }
    char *c = (char *)xmalloc(step + 1);
    memcpy(c, str + i, step);
    c[step] = '\0';
    iris_list_push(l, box_owned_str(c));
    i += step;
  }
  return l;
}

IrisList *iris_str_bytes(const char *str) {
  if (!str)
    return iris_list_new();
  IrisList *l = iris_list_new();
  size_t len = strlen(str);
  for (size_t i = 0; i < len; i++) {
    IrisVal *v = (IrisVal *)xmalloc(sizeof(IrisVal));
    v->tag = IRIS_TAG_I64;
    v->i64 = (int64_t)(unsigned char)str[i];
    iris_list_push(l, v);
  }
  return l;
}

int64_t iris_str_count(const char *str, const char *sub) {
  if (!str || !sub || sub[0] == '\0')
    return 0;
  int64_t count = 0;
  size_t sublen = strlen(sub);
  const char *p = str;
  while ((p = strstr(p, sub)) != NULL) {
    count++;
    p += sublen;
  }
  return count;
}

/* -- Math constants / predicates -- */

double iris_math_pi(void) { return 3.14159265358979323846; }
double iris_math_e(void) { return 2.71828182845904523536; }
double iris_math_inf(void) {
  volatile double zero = 0.0;
  return 1.0 / zero; /* +Infinity */
}
int iris_is_nan(double x) { return x != x; }
int iris_is_inf(double x) {
  volatile double zero = 0.0;
  return (x == (1.0 / zero)) || (x == -(1.0 / zero));
}

/* -- OS / System -- */

char *iris_env_get(const char *key) {
  if (!key) {
    char *e = (char *)xmalloc(1);
    e[0] = '\0';
    return e;
  }
  const char *val = getenv(key);
  if (!val) {
    char *e = (char *)xmalloc(1);
    e[0] = '\0';
    return e;
  }
  size_t len = strlen(val);
  char *r = (char *)xmalloc(len + 1);
  memcpy(r, val, len + 1);
  return r;
}

void iris_env_set(const char *key, const char *val) {
  iris_sandbox_require_process(key ? key : "env_set");
  if (!key)
    return;
#ifdef _WIN32
  _putenv_s(key, val ? val : "");
#else
  setenv(key, val ? val : "", 1);
#endif
}

void iris_exit_code(int64_t code) {
  iris_sandbox_require_process("exit");
  exit((int)code);
}

char *iris_exec_cmd(const char *cmd) {
  iris_sandbox_require_process(cmd);
#if defined(__wasm__)
  (void)cmd;
  {
    char *e = (char *)xmalloc(1);
    e[0] = '\0';
    return e;
  }
#else
  if (!cmd) {
    char *e = (char *)xmalloc(1);
    e[0] = '\0';
    return e;
  }
#ifdef _WIN32
  FILE *fp = _popen(cmd, "r");
#else
  FILE *fp = popen(cmd, "r");
#endif
  if (!fp) {
    char *e = (char *)xmalloc(1);
    e[0] = '\0';
    return e;
  }
  size_t cap = 1024, len = 0;
  char *buf = (char *)xmalloc(cap);
  char tmp[256];
  while (fgets(tmp, sizeof(tmp), fp)) {
    size_t tlen = strlen(tmp);
    if (len + tlen >= cap) {
      cap *= 2;
      buf = (char *)xrealloc(buf, cap);
    }
    memcpy(buf + len, tmp, tlen);
    len += tlen;
  }
  buf[len] = '\0';
#ifdef _WIN32
  _pclose(fp);
#else
  pclose(fp);
#endif
  return buf;
#endif
}

int64_t iris_pid(void) {
#ifdef _WIN32
  return (int64_t)GetCurrentProcessId();
#else
  return (int64_t)getpid();
#endif
}

/* -- Crypto / UUID -- */

static uint64_t uuid_state = 0;

char *iris_uuid(void) {
  if (uuid_state == 0) {
    uuid_state = (uint64_t)time(NULL) ^ 0xDEADBEEFCAFEBABEULL;
  }
  /* xorshift64 */
  uuid_state ^= uuid_state << 13;
  uuid_state ^= uuid_state >> 7;
  uuid_state ^= uuid_state << 17;
  uint64_t a = uuid_state;
  uuid_state ^= uuid_state << 13;
  uuid_state ^= uuid_state >> 7;
  uuid_state ^= uuid_state << 17;
  uint64_t b = uuid_state;
  a = (a & 0xFFFFFFFFFFFF0FFFULL) | 0x4000ULL;             /* version 4 */
  b = (b & 0x3FFFFFFFFFFFFFFFULL) | 0x8000000000000000ULL; /* variant 1 */
  char *r = (char *)xmalloc(37);
  snprintf(r, 37, "%08x-%04x-%04x-%04x-%012llx", (uint32_t)(a >> 32),
           (uint16_t)((a >> 16) & 0xFFFF), (uint16_t)(a & 0xFFFF),
           (uint16_t)((b >> 48) & 0xFFFF),
           (unsigned long long)(b & 0xFFFFFFFFFFFFULL));
  return r;
}

/* Minimal SHA-256 */
static const uint32_t sha256_K[64] = {
    0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1,
    0x923f82a4, 0xab1c5ed5, 0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3,
    0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786,
    0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da,
    0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147,
    0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13,
    0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b,
    0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
    0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a,
    0x5b9cca4f, 0x682e6ff3, 0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208,
    0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
};

static uint32_t sha256_rotr(uint32_t x, int n) {
  return (x >> n) | (x << (32 - n));
}

char *iris_sha256(const char *input) {
  if (!input) {
    char *e = (char *)xmalloc(65);
    memset(e, '0', 64);
    e[64] = '\0';
    return e;
  }
  size_t ilen = strlen(input);
  uint64_t bit_len = (uint64_t)ilen * 8;
  /* Padding */
  size_t padded = ilen + 1;
  while (padded % 64 != 56)
    padded++;
  padded += 8;
  uint8_t *msg = (uint8_t *)xmalloc(padded);
  memset(msg, 0, padded);
  memcpy(msg, input, ilen);
  msg[ilen] = 0x80;
  for (int i = 0; i < 8; i++)
    msg[padded - 1 - i] = (uint8_t)(bit_len >> (i * 8));
  uint32_t h[8] = {0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a,
                   0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19};
  for (size_t off = 0; off < padded; off += 64) {
    uint32_t w[64];
    for (int i = 0; i < 16; i++)
      w[i] = ((uint32_t)msg[off + i * 4] << 24) |
             ((uint32_t)msg[off + i * 4 + 1] << 16) |
             ((uint32_t)msg[off + i * 4 + 2] << 8) | msg[off + i * 4 + 3];
    for (int i = 16; i < 64; i++) {
      uint32_t s0 = sha256_rotr(w[i - 15], 7) ^ sha256_rotr(w[i - 15], 18) ^
                    (w[i - 15] >> 3);
      uint32_t s1 = sha256_rotr(w[i - 2], 17) ^ sha256_rotr(w[i - 2], 19) ^
                    (w[i - 2] >> 10);
      w[i] = w[i - 16] + s0 + w[i - 7] + s1;
    }
    uint32_t a = h[0], b = h[1], c = h[2], d = h[3], e = h[4], f = h[5],
             g = h[6], hh = h[7];
    for (int i = 0; i < 64; i++) {
      uint32_t S1 = sha256_rotr(e, 6) ^ sha256_rotr(e, 11) ^ sha256_rotr(e, 25);
      uint32_t ch = (e & f) ^ ((~e) & g);
      uint32_t t1 = hh + S1 + ch + sha256_K[i] + w[i];
      uint32_t S0 = sha256_rotr(a, 2) ^ sha256_rotr(a, 13) ^ sha256_rotr(a, 22);
      uint32_t maj = (a & b) ^ (a & c) ^ (b & c);
      uint32_t t2 = S0 + maj;
      hh = g;
      g = f;
      f = e;
      e = d + t1;
      d = c;
      c = b;
      b = a;
      a = t1 + t2;
    }
    h[0] += a;
    h[1] += b;
    h[2] += c;
    h[3] += d;
    h[4] += e;
    h[5] += f;
    h[6] += g;
    h[7] += hh;
  }
  xfree(msg);
  char *out = (char *)xmalloc(65);
  snprintf(out, 65, "%08x%08x%08x%08x%08x%08x%08x%08x", h[0], h[1], h[2], h[3],
           h[4], h[5], h[6], h[7]);
  return out;
}

char *iris_hex_encode(const char *input) {
  if (!input) {
    char *e = (char *)xmalloc(1);
    e[0] = '\0';
    return e;
  }
  size_t len = strlen(input);
  char *r = (char *)xmalloc(len * 2 + 1);
  for (size_t i = 0; i < len; i++)
    snprintf(r + i * 2, 3, "%02x", (unsigned char)input[i]);
  r[len * 2] = '\0';
  return r;
}

static int hex_digit(char c) {
  if (c >= '0' && c <= '9')
    return c - '0';
  if (c >= 'a' && c <= 'f')
    return c - 'a' + 10;
  if (c >= 'A' && c <= 'F')
    return c - 'A' + 10;
  return 0;
}

char *iris_hex_decode(const char *input) {
  if (!input) {
    char *e = (char *)xmalloc(1);
    e[0] = '\0';
    return e;
  }
  size_t len = strlen(input);
  size_t olen = len / 2;
  char *r = (char *)xmalloc(olen + 1);
  for (size_t i = 0; i < olen; i++)
    r[i] = (char)((hex_digit(input[i * 2]) << 4) | hex_digit(input[i * 2 + 1]));
  r[olen] = '\0';
  return r;
}

/* -- Deque (reuses IrisList) -- */

IrisList *iris_deque_new(void) { return iris_list_new(); }

IrisList *iris_deque_push_front(IrisList *dq, int64_t val) {
  if (!dq)
    return dq;
  iris_tx_record_list(dq);
  IrisVal *boxed = iris_box_i64(val);
  /* shift elements right */
  if (dq->len >= dq->cap) {
    dq->cap = dq->cap ? dq->cap * 2 : 8;
    dq->data = (IrisVal **)xrealloc(dq->data, sizeof(IrisVal *) * dq->cap);
  }
  memmove(dq->data + 1, dq->data, sizeof(IrisVal *) * dq->len);
  iris_retain(boxed);
  dq->data[0] = boxed;
  dq->len++;
  return dq;
}

IrisList *iris_deque_push_back(IrisList *dq, int64_t val) {
  iris_list_push(dq, iris_box_i64(val));
  return dq;
}

int64_t iris_deque_pop_front(IrisList *dq) {
  if (!dq || dq->len == 0)
    return 0;
  iris_tx_record_list(dq);
  IrisVal *v = dq->data[0];
  memmove(dq->data, dq->data + 1, sizeof(IrisVal *) * (dq->len - 1));
  dq->len--;
  int64_t res = v ? v->i64 : 0;
  if (v)
    iris_release(v);
  return res;
}

int64_t iris_deque_pop_back(IrisList *dq) {
  if (!dq || dq->len == 0)
    return 0;
  iris_tx_record_list(dq);
  IrisVal *v = dq->data[--dq->len];
  int64_t res = v ? v->i64 : 0;
  if (v)
    iris_release(v);
  return res;
}

int64_t iris_deque_len(IrisList *dq) { return dq ? (int64_t)dq->len : 0; }

int64_t iris_deque_front(IrisList *dq) {
  if (!dq || dq->len == 0)
    return 0;
  IrisVal *v = dq->data[0];
  return v ? v->i64 : 0;
}

int64_t iris_deque_back(IrisList *dq) {
  if (!dq || dq->len == 0)
    return 0;
  IrisVal *v = dq->data[dq->len - 1];
  return v ? v->i64 : 0;
}

/* -- BitSet (backed by IrisList of i64 words) -- */

IrisList *iris_bitset_new(int64_t nbits) {
  (void)nbits;
  return iris_list_new();
}

IrisList *iris_bitset_set(IrisList *bs, int64_t pos) {
  if (!bs)
    return bs;
  iris_tx_record_list(bs);
  int64_t word_idx = pos / 64;
  int64_t bit_idx = pos % 64;
  while ((int64_t)bs->len <= word_idx) {
    iris_list_push(bs, iris_box_i64(0));
  }
  IrisVal *w = bs->data[word_idx];
  int64_t wv = w ? w->i64 : 0;
  if (w) {
    iris_release(w);
  }
  bs->data[word_idx] = iris_box_i64(wv | (1LL << bit_idx));
  return bs;
}

int iris_bitset_get(IrisList *bs, int64_t pos) {
  if (!bs)
    return 0;
  int64_t word_idx = pos / 64;
  int64_t bit_idx = pos % 64;
  if (word_idx >= (int64_t)bs->len)
    return 0;
  IrisVal *w = bs->data[word_idx];
  int64_t wv = w ? w->i64 : 0;
  return (wv >> bit_idx) & 1;
}

int64_t iris_bitset_count(IrisList *bs) {
  if (!bs)
    return 0;
  int64_t count = 0;
  for (size_t i = 0; i < bs->len; i++) {
    IrisVal *w = bs->data[i];
    if (w) {
      uint64_t v = (uint64_t)w->i64;
      /* popcount via Kernighan trick */
      while (v) {
        v &= v - 1;
        count++;
      }
    }
  }
  return count;
}

IrisList *iris_bitset_clear(IrisList *bs, int64_t pos) {
  if (!bs)
    return bs;
  iris_tx_record_list(bs);
  int64_t word_idx = pos / 64;
  int64_t bit_idx = pos % 64;
  if (word_idx < (int64_t)bs->len) {
    IrisVal *w = bs->data[word_idx];
    int64_t wv = w ? w->i64 : 0;
    if (w) {
      iris_release(w);
    }
    bs->data[word_idx] = iris_box_i64(wv & ~(1LL << bit_idx));
  }
  return bs;
}

/* -- FFI -- */

#ifdef _WIN32
static int iris_win_is_bare_name(const char *path) {
  return path && *path && !strchr(path, '/') && !strchr(path, '\\') &&
         !strchr(path, ':');
}

static void *iris_win_try_exe_subdirs(const char *name) {
  if (!iris_win_is_bare_name(name))
    return NULL;
  char exe_dir[MAX_PATH];
  if (!GetModuleFileNameA(NULL, exe_dir, MAX_PATH))
    return NULL;
  char *slash = strrchr(exe_dir, '\\');
  char *fwd = strrchr(exe_dir, '/');
  if (fwd && (!slash || fwd > slash))
    slash = fwd;
  if (!slash)
    return NULL;
  *slash = '\0';
  static const char *const subdirs[] = {"", "\\debug", "\\release"};
  for (size_t i = 0; i < sizeof(subdirs) / sizeof(subdirs[0]); i++) {
    char candidate[MAX_PATH];
    int written = snprintf(candidate, sizeof(candidate), "%s%s\\%s", exe_dir,
                           subdirs[i], name);
    if (written > 0 && (size_t)written < sizeof(candidate) &&
        GetFileAttributesA(candidate) != INVALID_FILE_ATTRIBUTES) {
      void *h = (void *)LoadLibraryExA(
          candidate, NULL, 0x00000008 /* LOAD_WITH_ALTERED_SEARCH_PATH */);
      if (h)
        return h;
    }
  }
  return NULL;
}

static int iris_win_dll_exists(const char *path) {
  if (!path || !*path)
    return 0;
  if (GetFileAttributesA(path) != INVALID_FILE_ATTRIBUTES)
    return 1;
  if (iris_win_is_bare_name(path))
    return SearchPathA(NULL, path, ".dll", 0, NULL, NULL) != 0;
  return 0;
}
#endif

void *iris_ffi_open(const char *path) {
  iris_sandbox_require_ffi(path ? path : "<self>");
  if (!path || !*path)
    return NULL;
#ifdef _WIN32
  // Automatically inject known dependency paths to PATH for LoadLibrary search.
  // ROS 2 is not among them: the ROS 2 bridge needs a sourced ROS 2
  // environment (or the ROS 2 bin directory on PATH) and finds the rest of
  // its install itself.
  static int paths_injected = 0;
  if (!paths_injected) {
    paths_injected = 1;

    char old_path[32768];
    DWORD len = GetEnvironmentVariableA("PATH", old_path, sizeof(old_path));
    if (len > 0 && len < sizeof(old_path)) {
      char new_path[32768];
      snprintf(new_path, sizeof(new_path),
               "C:\\onnxruntime\\lib;"
               "C:\\tensorflow\\lib;"
               "C:\\libtorch\\lib;"
               "C:\\openblas\\bin;%s",
               old_path);
      _putenv_s("PATH", new_path);
      SetEnvironmentVariableA("PATH", new_path);
    }
  }

  void *h = (void *)LoadLibraryA(path);
  DWORD err = h ? 0 : GetLastError();
  if (!h)
    h = iris_win_try_exe_subdirs(path);
  /* Match Unix dlopen(): stay silent on non-existent candidate probes (126). */
  if (!h && (err != ERROR_MOD_NOT_FOUND || iris_win_dll_exists(path))) {
    fprintf(stderr,
            "[iris_runtime] LoadLibraryA(\"%s\") failed with error code: %lu\n",
            path, (unsigned long)err);
  }
  return h;
#elif defined(__unix__) || defined(__APPLE__)
  return dlopen(path, RTLD_LAZY);
#else
  return NULL;
#endif
}

int64_t iris_ffi_call(void *handle, const char *func_name) {
  iris_sandbox_require_ffi_enabled();
  if (!handle || !func_name)
    return -1;
#ifdef _WIN32
  typedef int64_t (*fn_t)(void);
  fn_t f = (fn_t)GetProcAddress((HMODULE)handle, func_name);
  if (!f)
    return -1;
  return f();
#elif defined(__unix__) || defined(__APPLE__)
  typedef int64_t (*fn_t)(void);
  fn_t f = (fn_t)dlsym(handle, func_name);
  if (!f)
    return -1;
  return f();
#else
  return -1;
#endif
}

int iris_ffi_close(void *handle) {
  iris_sandbox_require_ffi_enabled();
  if (!handle)
    return 0;
#ifdef _WIN32
  return FreeLibrary((HMODULE)handle) ? 1 : 0;
#elif defined(__unix__) || defined(__APPLE__)
  return dlclose(handle) == 0 ? 1 : 0;
#else
  return 0;
#endif
}

/* -- Expanded C FFI with typed arguments -- */

static void *ffi_get_sym(void *handle, const char *func_name) {
  iris_sandbox_require_ffi_enabled();
  if (!handle || !func_name)
    return NULL;
#ifdef _WIN32
  return (void *)GetProcAddress((HMODULE)handle, func_name);
#elif defined(__unix__) || defined(__APPLE__)
  return dlsym(handle, func_name);
#else
  return NULL;
#endif
}

static int64_t ffi_dispatch_i64(void *fn_ptr, int64_t *args, int n) {
  typedef int64_t (*fn0)(void);
  typedef int64_t (*fn1)(int64_t);
  typedef int64_t (*fn2)(int64_t, int64_t);
  typedef int64_t (*fn3)(int64_t, int64_t, int64_t);
  typedef int64_t (*fn4)(int64_t, int64_t, int64_t, int64_t);
  typedef int64_t (*fn5)(int64_t, int64_t, int64_t, int64_t, int64_t);
  typedef int64_t (*fn6)(int64_t, int64_t, int64_t, int64_t, int64_t, int64_t);
  typedef int64_t (*fn7)(int64_t, int64_t, int64_t, int64_t, int64_t, int64_t,
                         int64_t);
  typedef int64_t (*fn8)(int64_t, int64_t, int64_t, int64_t, int64_t, int64_t,
                         int64_t, int64_t);
  typedef int64_t (*fn9)(int64_t, int64_t, int64_t, int64_t, int64_t, int64_t,
                         int64_t, int64_t, int64_t);
  typedef int64_t (*fn10)(int64_t, int64_t, int64_t, int64_t, int64_t, int64_t,
                          int64_t, int64_t, int64_t, int64_t);
  typedef int64_t (*fn11)(int64_t, int64_t, int64_t, int64_t, int64_t, int64_t,
                          int64_t, int64_t, int64_t, int64_t, int64_t);
  typedef int64_t (*fn12)(int64_t, int64_t, int64_t, int64_t, int64_t, int64_t,
                          int64_t, int64_t, int64_t, int64_t, int64_t, int64_t);
  if (!fn_ptr || n < 0 || n > 12)
    return -1;
  switch (n) {
  case 0:
    return ((fn0)fn_ptr)();
  case 1:
    return ((fn1)fn_ptr)(args[0]);
  case 2:
    return ((fn2)fn_ptr)(args[0], args[1]);
  case 3:
    return ((fn3)fn_ptr)(args[0], args[1], args[2]);
  case 4:
    return ((fn4)fn_ptr)(args[0], args[1], args[2], args[3]);
  case 5:
    return ((fn5)fn_ptr)(args[0], args[1], args[2], args[3], args[4]);
  case 6:
    return ((fn6)fn_ptr)(args[0], args[1], args[2], args[3], args[4], args[5]);
  case 7:
    return ((fn7)fn_ptr)(args[0], args[1], args[2], args[3], args[4], args[5],
                         args[6]);
  case 8:
    return ((fn8)fn_ptr)(args[0], args[1], args[2], args[3], args[4], args[5],
                         args[6], args[7]);
  case 9:
    return ((fn9)fn_ptr)(args[0], args[1], args[2], args[3], args[4], args[5],
                         args[6], args[7], args[8]);
  case 10:
    return ((fn10)fn_ptr)(args[0], args[1], args[2], args[3], args[4], args[5],
                          args[6], args[7], args[8], args[9]);
  case 11:
    return ((fn11)fn_ptr)(args[0], args[1], args[2], args[3], args[4], args[5],
                          args[6], args[7], args[8], args[9], args[10]);
  case 12:
    return ((fn12)fn_ptr)(args[0], args[1], args[2], args[3], args[4], args[5],
                          args[6], args[7], args[8], args[9], args[10],
                          args[11]);
  default:
    return -1;
  }
}

int64_t iris_ffi_call_i64(void *handle, const char *func_name, int64_t *args,
                          int nargs) {
  void *sym = ffi_get_sym(handle, func_name);
  return ffi_dispatch_i64(sym, args, nargs);
}

double iris_ffi_call_f64(void *handle, const char *func_name, int64_t *args,
                         int nargs) {
  typedef double (*fn0)(void);
  typedef double (*fn1)(int64_t);
  typedef double (*fn2)(int64_t, int64_t);
  void *sym = ffi_get_sym(handle, func_name);
  if (!sym)
    return 0.0;
  switch (nargs) {
  case 0:
    return ((fn0)sym)();
  case 1:
    return ((fn1)sym)(args[0]);
  default:
    return ((fn2)sym)(args[0], args[1]);
  }
}

const char *iris_ffi_call_str(void *handle, const char *func_name,
                              int64_t *args, int nargs) {
  typedef const char *(*fn0)(void);
  typedef const char *(*fn1)(int64_t);
  typedef const char *(*fn2)(int64_t, int64_t);
  void *sym = ffi_get_sym(handle, func_name);
  if (!sym)
    return "";
  switch (nargs) {
  case 0:
    return ((fn0)sym)();
  case 1:
    return ((fn1)sym)(args[0]);
  default:
    return ((fn2)sym)(args[0], args[1]);
  }
}

void iris_ffi_call_void(void *handle, const char *func_name, int64_t *args,
                        int nargs) {
  typedef void (*fn0)(void);
  typedef void (*fn1)(int64_t);
  typedef void (*fn2)(int64_t, int64_t);
  void *sym = ffi_get_sym(handle, func_name);
  if (!sym)
    return;
  switch (nargs) {
  case 0:
    ((fn0)sym)();
    break;
  case 1:
    ((fn1)sym)(args[0]);
    break;
  default:
    ((fn2)sym)(args[0], args[1]);
    break;
  }
}

/* ---- FFI out-parameter cells -------------------------------------------
 *
 * A great many C functions return their result through a pointer argument:
 *
 *     int64_t iris_rcl_take_twist(int64_t sub, double* out);   // writes 6
 * doubles int64_t iris_rcl_take_string(int64_t sub, char* buf, int32_t max);
 *
 * IRIS could not call any of those. `ffi_dispatch_i64` already passes an array
 * of int64 slots, and on every supported target a pointer fits in one -- so the
 * missing piece was never the calling convention. It was that IRIS had no way
 * to own a piece of memory and name its address.
 *
 * These give it one. `iris_ffi_out_new` returns the address of a zeroed block
 * as an int64, which is passed straight through as an ordinary argument; the
 * typed readers then pull values back out. Indexed reads matter because a
 * single out-pointer often receives several values (a Twist is six doubles).
 *
 * Ownership is explicit: the caller frees. That is deliberate -- these cells
 * are handed to foreign code, so tying them to the refcounting GC would mean
 * reasoning about a lifetime the GC cannot see.
 */

/* Every cell carries a 16-byte header immediately before the address handed
 * out: a magic word and the payload length.
 *
 * That buys two properties the interpreter already had, and the two backends
 * must agree or a policy behaves differently under `--emit eval` than in a
 * built binary. Bounds are checked on every read, so an out-param the callee
 * did not fill returns zero rather than garbage; and `out_free` is idempotent,
 * because the magic is cleared on release and a second free sees it gone.
 *
 * Idempotence is not a nicety here: the error path of a foreign call typically
 * frees on the way out, and a double free was corrupting the heap
 * (exit 0xC0000409) where the interpreter simply ignored it.
 *
 * 16 bytes keeps the payload 16-byte aligned, which is enough for any scalar a
 * C ABI will write through an out-pointer. */
/* Live FFI out-cells are tracked in a side table rather than an in-band header
 * so that `iris_cell_len` and idempotent `iris_ffi_out_free` never dereference
 * a pointer after `free()` (which would be a heap-use-after-free under ASan). */
#define IRIS_FFI_CELLS_INLINE_CAP 1024

typedef struct {
  int64_t cell;
  uint64_t len;
} IrisFfiCellEntry;

static IrisFfiCellEntry g_ffi_cells_inline[IRIS_FFI_CELLS_INLINE_CAP];
static IrisFfiCellEntry *g_ffi_cells = g_ffi_cells_inline;
static size_t g_ffi_cells_len = 0;
static size_t g_ffi_cells_cap = IRIS_FFI_CELLS_INLINE_CAP;

int64_t iris_ffi_out_new(int64_t nbytes) {
  if (nbytes <= 0)
    return 0;
  unsigned char *ptr = (unsigned char *)calloc(1, (size_t)nbytes);
  if (!ptr)
    return 0;
  int64_t cell = (int64_t)(intptr_t)ptr;
  if (g_ffi_cells_len == g_ffi_cells_cap) {
    size_t new_cap = g_ffi_cells_cap * 2;
    IrisFfiCellEntry *next =
        (IrisFfiCellEntry *)malloc(new_cap * sizeof(IrisFfiCellEntry));
    if (!next) {
      free(ptr);
      return 0;
    }
    memcpy(next, g_ffi_cells, g_ffi_cells_len * sizeof(IrisFfiCellEntry));
    if (g_ffi_cells != g_ffi_cells_inline)
      free(g_ffi_cells);
    g_ffi_cells = next;
    g_ffi_cells_cap = new_cap;
  }
  g_ffi_cells[g_ffi_cells_len].cell = cell;
  g_ffi_cells[g_ffi_cells_len].len = (uint64_t)nbytes;
  g_ffi_cells_len++;
  return cell;
}

/* Returns the payload length, or 0 if `cell` is not a live cell. */
static uint64_t iris_cell_len(int64_t cell) {
  if (!cell)
    return 0;
  for (size_t i = g_ffi_cells_len; i > 0; i--) {
    if (g_ffi_cells[i - 1].cell == cell)
      return g_ffi_cells[i - 1].len;
  }
  return 0;
}

int64_t iris_ffi_out_free(int64_t cell) {
  if (!cell)
    return 0;
  for (size_t i = g_ffi_cells_len; i > 0; i--) {
    if (g_ffi_cells[i - 1].cell == cell) {
      g_ffi_cells[i - 1] = g_ffi_cells[g_ffi_cells_len - 1];
      g_ffi_cells_len--;
      if (g_ffi_cells_len == 0 && g_ffi_cells != g_ffi_cells_inline) {
        free(g_ffi_cells);
        g_ffi_cells = g_ffi_cells_inline;
        g_ffi_cells_cap = IRIS_FFI_CELLS_INLINE_CAP;
      }
      free((void *)(intptr_t)cell);
      return 0;
    }
  }
  return 0; /* already freed */
}

int64_t iris_ffi_out_len(int64_t cell) { return (int64_t)iris_cell_len(cell); }

int64_t iris_ffi_out_sizeof_f64(void) { return (int64_t)sizeof(double); }
int64_t iris_ffi_out_sizeof_i64(void) { return (int64_t)sizeof(int64_t); }

double iris_ffi_out_get_f64(int64_t cell, int64_t index) {
  uint64_t len = iris_cell_len(cell);
  if (index < 0 || (uint64_t)(index + 1) * sizeof(double) > len)
    return 0.0;
  return ((double *)(intptr_t)cell)[index];
}

int64_t iris_ffi_out_get_i64(int64_t cell, int64_t index) {
  uint64_t len = iris_cell_len(cell);
  if (index < 0 || (uint64_t)(index + 1) * sizeof(int64_t) > len)
    return 0;
  return ((int64_t *)(intptr_t)cell)[index];
}

int32_t iris_ffi_out_get_i32(int64_t cell, int64_t index) {
  uint64_t len = iris_cell_len(cell);
  if (index < 0 || (uint64_t)(index + 1) * sizeof(int32_t) > len)
    return 0;
  return ((int32_t *)(intptr_t)cell)[index];
}

/* Read the cell as a NUL-terminated string, bounded by the payload length so an
 * unterminated buffer cannot run off the end. Returns a fresh copy, so the cell
 * may be freed immediately afterwards. */
char *iris_ffi_out_get_str(int64_t cell) {
  uint64_t len = iris_cell_len(cell);
  if (!len)
    return xstrdup("");
  const char *p = (const char *)(intptr_t)cell;
  uint64_t n = 0;
  while (n < len && p[n] != 0)
    n++;
  char *out = (char *)xmalloc((size_t)n + 1);
  memcpy(out, p, (size_t)n);
  out[n] = 0;
  return out;
}

int64_t iris_ffi_out_get_i32_i64(int64_t cell, int64_t index) {
  if (!cell || index < 0)
    return 0;
  uint64_t len = iris_cell_len(cell);
  uint64_t idx = (uint64_t)index;
  if (idx > UINT64_MAX / 4 || idx * 4 > len || len - idx * 4 < 4)
    return 0;
  int32_t v = 0;
  memcpy(&v, (const unsigned char *)(intptr_t)cell + (size_t)(idx * 4), 4);
  return (int64_t)v;
}

double iris_ffi_out_get_f32(int64_t cell, int64_t index) {
  if (!cell || index < 0)
    return 0.0;
  uint64_t len = iris_cell_len(cell);
  uint64_t idx = (uint64_t)index;
  if (idx > UINT64_MAX / 4 || idx * 4 > len || len - idx * 4 < 4)
    return 0.0;
  float v = 0.0f;
  memcpy(&v, (const unsigned char *)(intptr_t)cell + (size_t)(idx * 4), 4);
  return (double)v;
}

int64_t iris_ffi_out_get_u8(int64_t cell, int64_t byte_offset) {
  if (!cell || byte_offset < 0)
    return 0;
  uint64_t len = iris_cell_len(cell);
  if ((uint64_t)byte_offset >= len)
    return 0;
  return (int64_t)((const unsigned char *)(intptr_t)cell)[(size_t)byte_offset];
}

char *iris_ffi_out_get_str_range(int64_t cell, int64_t byte_offset,
                                 int64_t byte_len) {
  uint64_t len = iris_cell_len(cell);
  if (!len || byte_offset < 0 || byte_len <= 0 ||
      (uint64_t)byte_offset >= len) {
    return xstrdup("");
  }
  uint64_t avail = len - (uint64_t)byte_offset;
  uint64_t max_n = (uint64_t)byte_len < avail ? (uint64_t)byte_len : avail;
  const char *p = (const char *)(intptr_t)cell + (size_t)byte_offset;
  uint64_t n = 0;
  while (n < max_n && p[n] != 0)
    n++;
  char *out = (char *)xmalloc((size_t)n + 1);
  if (n > 0)
    memcpy(out, p, (size_t)n);
  out[n] = 0;
  return out;
}

int64_t iris_ffi_out_set_f64(int64_t cell, int64_t index, double v) {
  uint64_t len = iris_cell_len(cell);
  if (index < 0 || (uint64_t)index > UINT64_MAX / sizeof(double) ||
      (uint64_t)index * sizeof(double) > len ||
      len - (uint64_t)index * sizeof(double) < sizeof(double)) {
    return 0;
  }
  memcpy((unsigned char *)(intptr_t)cell + (size_t)index * sizeof(double), &v,
         sizeof(double));
  return 0;
}

int64_t iris_ffi_out_set_i64(int64_t cell, int64_t index, int64_t v) {
  uint64_t len = iris_cell_len(cell);
  if (index < 0 || (uint64_t)index > UINT64_MAX / sizeof(int64_t) ||
      (uint64_t)index * sizeof(int64_t) > len ||
      len - (uint64_t)index * sizeof(int64_t) < sizeof(int64_t)) {
    return 0;
  }
  memcpy((unsigned char *)(intptr_t)cell + (size_t)index * sizeof(int64_t), &v,
         sizeof(int64_t));
  return 0;
}

int64_t iris_ffi_out_set_i32(int64_t cell, int64_t index, int64_t v) {
  if (!cell || index < 0)
    return 0;
  uint64_t len = iris_cell_len(cell);
  uint64_t idx = (uint64_t)index;
  if (idx > UINT64_MAX / 4 || idx * 4 > len || len - idx * 4 < 4)
    return 0;
  int32_t v32 = (int32_t)v;
  memcpy((unsigned char *)(intptr_t)cell + (size_t)(idx * 4), &v32, 4);
  return 0;
}

int64_t iris_ffi_out_set_f32(int64_t cell, int64_t index, double v) {
  if (!cell || index < 0)
    return 0;
  uint64_t len = iris_cell_len(cell);
  uint64_t idx = (uint64_t)index;
  if (idx > UINT64_MAX / 4 || idx * 4 > len || len - idx * 4 < 4)
    return 0;
  float f = (float)v;
  memcpy((unsigned char *)(intptr_t)cell + (size_t)(idx * 4), &f, 4);
  return 0;
}

int64_t iris_ffi_out_set_u8(int64_t cell, int64_t byte_offset, int64_t v) {
  if (!cell || byte_offset < 0)
    return 0;
  uint64_t len = iris_cell_len(cell);
  if ((uint64_t)byte_offset >= len)
    return 0;
  ((unsigned char *)(intptr_t)cell)[(size_t)byte_offset] =
      (unsigned char)(v & 0xFF);
  return 0;
}

int64_t iris_ffi_out_write_bytes(int64_t cell, int64_t byte_offset,
                                 const char *data) {
  if (!cell || byte_offset < 0 || !data)
    return 0;
  uint64_t len = iris_cell_len(cell);
  if ((uint64_t)byte_offset >= len)
    return 0;
  uint64_t avail = len - (uint64_t)byte_offset;
  size_t slen = strlen(data);
  uint64_t n = (uint64_t)slen < avail ? (uint64_t)slen : avail;
  if (n > 0)
    memcpy((unsigned char *)(intptr_t)cell + (size_t)byte_offset, data,
           (size_t)n);
  return (int64_t)n;
}

/* -- Python FFI -- */

static char python_buf[65536];

static const char *find_python_cmd(void) {
#ifdef _WIN32
  /* On Windows, try python first (py launcher), then python3. */
  if (system("python --version >nul 2>&1") == 0)
    return "python";
  if (system("python3 --version >nul 2>&1") == 0)
    return "python3";
#else
  if (system("python3 --version >/dev/null 2>&1") == 0)
    return "python3";
  if (system("python --version >/dev/null 2>&1") == 0)
    return "python";
#endif
  return NULL;
}

const char *iris_python_eval(const char *code) {
  iris_sandbox_require_ffi("python");
  iris_sandbox_require_process("python");
#if defined(__wasm__)
  (void)code;
  snprintf(python_buf, sizeof(python_buf),
           "error: python not available on WASM");
  return python_buf;
#else
  const char *py = find_python_cmd();
  if (!py || !code) {
    snprintf(python_buf, sizeof(python_buf), "error: python not found");
    return python_buf;
  }
  char cmd[8192];
  snprintf(cmd, sizeof(cmd), "%s -c \"import sys; sys.stdout.write(str(%s))\"",
           py, code);
#ifdef _WIN32
  FILE *fp = _popen(cmd, "r");
#else
  FILE *fp = popen(cmd, "r");
#endif
  if (!fp) {
    snprintf(python_buf, sizeof(python_buf), "error: popen failed");
    return python_buf;
  }
  size_t n = fread(python_buf, 1, sizeof(python_buf) - 1, fp);
  python_buf[n] = '\0';
#ifdef _WIN32
  _pclose(fp);
#else
  pclose(fp);
#endif
  return python_buf;
#endif
}

int64_t iris_python_exec(const char *code_or_path) {
  iris_sandbox_require_ffi("python");
  iris_sandbox_require_process("python");
  const char *py = find_python_cmd();
  if (!py || !code_or_path)
    return -1;
  char cmd[8192];
  /* Check if it looks like a file path */
  FILE *test = fopen(code_or_path, "r");
  if (test) {
    fclose(test);
    snprintf(cmd, sizeof(cmd), "%s \"%s\"", py, code_or_path);
  } else {
    snprintf(cmd, sizeof(cmd), "%s -c \"%s\"", py, code_or_path);
  }
  return (int64_t)system(cmd);
}

const char *iris_python_call(const char *module, const char *func,
                             const char *args_json) {
  iris_sandbox_require_ffi("python");
  iris_sandbox_require_process("python");
#if defined(__wasm__)
  (void)module;
  (void)func;
  (void)args_json;
  snprintf(python_buf, sizeof(python_buf),
           "error: python not available on WASM");
  return python_buf;
#else
  const char *py = find_python_cmd();
  if (!py || !module || !func) {
    snprintf(python_buf, sizeof(python_buf), "error: python not found");
    return python_buf;
  }
  char cmd[8192];
  const char *a = args_json ? args_json : "";
  snprintf(cmd, sizeof(cmd),
           /* The argument is quoted. Without the quotes it was interpolated as
            * a bare Python expression, so `py_call1("os.path", "basename",
            * "/a/b/c.txt")` generated `basename(/a/b/c.txt)` -- a SyntaxError.
            * The interpreter quotes it, so the two backends disagreed on every
            * non-numeric argument. See known-issues #44. */
           "%s -c \"import %s; print(%s.%s('%s'))\"", py, module, module, func,
           a);
#ifdef _WIN32
  FILE *fp = _popen(cmd, "r");
#else
  FILE *fp = popen(cmd, "r");
#endif
  if (!fp) {
    snprintf(python_buf, sizeof(python_buf), "error: popen failed");
    return python_buf;
  }
  size_t n = fread(python_buf, 1, sizeof(python_buf) - 1, fp);
  python_buf[n] = '\0';
  /* Trim trailing newline */
  while (n > 0 && (python_buf[n - 1] == '\n' || python_buf[n - 1] == '\r')) {
    python_buf[--n] = '\0';
  }
#ifdef _WIN32
  _pclose(fp);
#else
  pclose(fp);
#endif
  return python_buf;
#endif
}

const char *iris_python_version(void) {
  iris_sandbox_require_ffi("python");
  iris_sandbox_require_process("python");
#if defined(__wasm__)
  return "Python not available on WASM";
#else
  const char *py = find_python_cmd();
  if (!py)
    return "Python not found";
  char cmd[256];
  snprintf(cmd, sizeof(cmd), "%s --version", py);
#ifdef _WIN32
  FILE *fp = _popen(cmd, "r");
#else
  FILE *fp = popen(cmd, "r");
#endif
  if (!fp)
    return "unknown";
  size_t n = fread(python_buf, 1, sizeof(python_buf) - 1, fp);
  python_buf[n] = '\0';
  while (n > 0 && (python_buf[n - 1] == '\n' || python_buf[n - 1] == '\r')) {
    python_buf[--n] = '\0';
  }
#ifdef _WIN32
  _pclose(fp);
#else
  pclose(fp);
#endif
  return python_buf;
#endif
}

/* -- Rust FFI (aliases for C FFI — Rust cdylibs export extern "C") -- */

void *iris_rust_lib_open(const char *path) { return iris_ffi_open(path); }
int64_t iris_rust_call_i64(void *h, const char *fn_name, int64_t *args, int n) {
  return iris_ffi_call_i64(h, fn_name, args, n);
}
double iris_rust_call_f64(void *h, const char *fn_name, int64_t *args, int n) {
  return iris_ffi_call_f64(h, fn_name, args, n);
}
void iris_rust_call_void(void *h, const char *fn_name, int64_t *args, int n) {
  iris_ffi_call_void(h, fn_name, args, n);
}

/* -- Functional list ops (numeric) -- */

double iris_list_sum(IrisList *list) {
  if (!list)
    return 0.0;
  double s = 0.0;
  for (size_t i = 0; i < list->len; i++) {
    if (!list->data[i])
      continue;
    switch (list->data[i]->tag) {
    case IRIS_TAG_I64:
      s += (double)list->data[i]->i64;
      break;
    case IRIS_TAG_I32:
      s += (double)list->data[i]->i32;
      break;
    case IRIS_TAG_F64:
      s += list->data[i]->f64;
      break;
    case IRIS_TAG_F32:
      s += (double)list->data[i]->f32;
      break;
    default:
      break;
    }
  }
  return s;
}

int64_t iris_list_min(IrisList *list) {
  if (!list || list->len == 0)
    return 0;
  int64_t m = INT64_MAX;
  for (size_t i = 0; i < list->len; i++) {
    if (list->data[i] && list->data[i]->tag == IRIS_TAG_I64 &&
        list->data[i]->i64 < m)
      m = list->data[i]->i64;
  }
  return m;
}

int64_t iris_list_max(IrisList *list) {
  if (!list || list->len == 0)
    return 0;
  int64_t m = INT64_MIN;
  for (size_t i = 0; i < list->len; i++) {
    if (list->data[i] && list->data[i]->tag == IRIS_TAG_I64 &&
        list->data[i]->i64 > m)
      m = list->data[i]->i64;
  }
  return m;
}

int64_t iris_list_index_of(IrisList *list, int64_t val) {
  if (!list)
    return -1;
  for (size_t i = 0; i < list->len; i++) {
    if (list->data[i] && list->data[i]->tag == IRIS_TAG_I64 &&
        list->data[i]->i64 == val)
      return (int64_t)i;
  }
  return -1;
}

int64_t iris_list_count(IrisList *list, int64_t val) {
  if (!list)
    return 0;
  int64_t c = 0;
  for (size_t i = 0; i < list->len; i++) {
    if (list->data[i] && list->data[i]->tag == IRIS_TAG_I64 &&
        list->data[i]->i64 == val)
      c++;
  }
  return c;
}

IrisList *iris_list_reverse(IrisList *list) {
  IrisList *r = iris_list_new();
  if (!list)
    return r;
  for (size_t i = list->len; i > 0; i--)
    iris_list_push(r, list->data[i - 1]);
  return r;
}

IrisList *iris_list_take(IrisList *list, int64_t n) {
  IrisList *r = iris_list_new();
  if (!list)
    return r;
  size_t take = (n < 0) ? 0 : ((size_t)n > list->len ? list->len : (size_t)n);
  for (size_t i = 0; i < take; i++)
    iris_list_push(r, list->data[i]);
  return r;
}

IrisList *iris_list_drop(IrisList *list, int64_t n) {
  IrisList *r = iris_list_new();
  if (!list)
    return r;
  size_t start = (n < 0) ? 0 : ((size_t)n > list->len ? list->len : (size_t)n);
  for (size_t i = start; i < list->len; i++)
    iris_list_push(r, list->data[i]);
  return r;
}

/* -- Concurrency extras -- */

int64_t iris_thread_count(void) {
#ifdef _WIN32
  SYSTEM_INFO si;
  GetSystemInfo(&si);
  return (int64_t)si.dwNumberOfProcessors;
#elif defined(__unix__) || defined(__APPLE__)
  long n = sysconf(_SC_NPROCESSORS_ONLN);
  return n > 0 ? (int64_t)n : 1;
#else
  return 1;
#endif
}

/* ====================================================================
 * Reference-Counting Garbage Collector
 * ====================================================================
 * Side-table approach: a hash map from pointer → refcount. This avoids
 * modifying the IrisVal layout while providing real reference counting.
 */

/* Initial bucket count (a power of two); the table grows 4x whenever it
 * holds more than two entries per bucket, so lookups stay O(1) however many
 * values are live. A fixed table made every retain/release slower as live
 * values accumulated: a loop building 200k strings took 17 s. */
#define RC_TABLE_INITIAL_BUCKETS 4096

/* Bacon-Rajan cycle GC color codes. */
#define RC_COLOR_BLACK 0 /* live, not a candidate */
#define RC_COLOR_GRAY 1  /* possible cycle root */
#define RC_COLOR_WHITE 2 /* unreachable, collect me */

typedef struct RcEntry {
  void *ptr;
  int32_t kind;
  int64_t count;
  int64_t scan_count; /* scratch counter used by cycle collector */
  int color;
  int buffered;           /* 1 if in possible_roots */
  int immortal;           /* 1 for interned string literals: never freed */
  IrisWeakRef *weak_refs; /* list of active weak refs pointing to this ptr */
  struct RcEntry *next;
} RcEntry;

static RcEntry **rc_table = NULL;
static size_t rc_table_buckets = 0;
static size_t rc_table_count = 0;
/* Nonzero while a sweep walks the buckets with the lock dropped around each
 * deep free; the table must not be reallocated under it. */
static int rc_table_sweeping = 0;
/* Where rc_take_one_locked resumes, so draining the table is linear. */
static size_t rc_take_cursor = 0;
static int64_t gc_total_allocated = 0;
static int64_t gc_total_freed = 0;
static int64_t gc_cycles_collected = 0;
static int64_t gc_weak_refs_invalidated = 0;
static int64_t gc_auto_threshold = 10000;

/* Global mutex protecting the rc_table from concurrent retain/release calls. */
static pthread_mutex_t rc_global_mu = PTHREAD_MUTEX_INITIALIZER;

/* Possible cycle roots (Gray nodes). */
#define POSSIBLE_ROOTS_MAX 512
static void *possible_roots[POSSIBLE_ROOTS_MAX];
static int possible_roots_count = 0;

static void iris_gc_cycle_collect_locked(void);

static void invalidate_weak_refs_locked(RcEntry *e) {
  if (!e)
    return;
  IrisWeakRef *w = e->weak_refs;
  while (w) {
    w->target = NULL;
    gc_weak_refs_invalidated++;
    w = w->next_weak;
  }
  e->weak_refs = NULL;
}

/* Fibonacci hashing of the address (allocations are 16-byte aligned). */
static size_t rc_hash_in(void *ptr, size_t buckets) {
  uint64_t v = (uint64_t)(uintptr_t)ptr >> 4;
  v *= 0x9E3779B97F4A7C15ULL;
  return (size_t)(v >> 32) & (buckets - 1);
}

static size_t rc_hash(void *ptr) { return rc_hash_in(ptr, rc_table_buckets); }

/* ---------------------------------------------------------------------------
 * Native record destructors.
 *
 * Records are opaque native allocations, so the runtime cannot know which
 * fields own references. The compiler emits one drop function per record type
 * and registers it against each allocation. When a NATIVE_OBJECT RC entry
 * reaches zero, the registered function releases the record's fields before
 * the block itself is freed. Without this, replacing a record (every GUI frame
 * builds a new state record) leaked all of its strings, lists and nested
 * records, so memory grew without bound.
 * --------------------------------------------------------------------------- */
typedef struct {
  void *ptr;
  IrisNativeDropFn fn;
} IrisNativeDropSlot;
#define IRIS_NATIVE_DROP_TOMBSTONE ((void *)(uintptr_t)1)
static IrisNativeDropSlot *native_drop_slots = NULL;
static size_t native_drop_cap = 0;
static size_t native_drop_live = 0;
static pthread_mutex_t native_drop_mu = PTHREAD_MUTEX_INITIALIZER;

static size_t native_drop_hash(void *ptr, size_t cap) {
  uint64_t v = (uint64_t)(uintptr_t)ptr >> 4;
  v *= 0x9E3779B97F4A7C15ULL;
  return (size_t)(v >> 32) & (cap - 1);
}

static void native_drop_grow_locked(size_t cap) {
  IrisNativeDropSlot *slots = (IrisNativeDropSlot *)calloc(cap, sizeof(*slots));
  if (!slots)
    return; /* keep the current table: slower, still correct */
  for (size_t i = 0; i < native_drop_cap; i++) {
    void *p = native_drop_slots[i].ptr;
    if (!p || p == IRIS_NATIVE_DROP_TOMBSTONE)
      continue;
    size_t h = native_drop_hash(p, cap);
    while (slots[h].ptr)
      h = (h + 1) & (cap - 1);
    slots[h] = native_drop_slots[i];
  }
  free(native_drop_slots);
  native_drop_slots = slots;
  native_drop_cap = cap;
}

void iris_register_native_drop(void *ptr, IrisNativeDropFn fn) {
  if (!ptr || !fn)
    return;
  IrisUiPool *pool = iris_active_ui_pool;
  /* Frame-pool allocations are reclaimed wholesale by the pool. Their fields
   * belong to the same pool, so a destructor must not release them early. */
  if (pool && iris_ui_pool_owns(pool, ptr))
    return;
  pthread_mutex_lock(&native_drop_mu);
  if (native_drop_cap == 0 || (native_drop_live + 1) * 2 > native_drop_cap) {
    size_t cap = native_drop_cap ? native_drop_cap * 4 : 1024;
    native_drop_grow_locked(cap);
  }
  if (native_drop_cap != 0) {
    size_t h = native_drop_hash(ptr, native_drop_cap);
    while (native_drop_slots[h].ptr &&
           native_drop_slots[h].ptr != IRIS_NATIVE_DROP_TOMBSTONE &&
           native_drop_slots[h].ptr != ptr)
      h = (h + 1) & (native_drop_cap - 1);
    if (native_drop_slots[h].ptr == ptr) {
      native_drop_slots[h].fn = fn;
    } else {
      if (native_drop_slots[h].ptr == IRIS_NATIVE_DROP_TOMBSTONE)
        native_drop_live--;
      native_drop_slots[h].ptr = ptr;
      native_drop_slots[h].fn = fn;
      native_drop_live++;
    }
  }
  pthread_mutex_unlock(&native_drop_mu);
}

static IrisNativeDropFn native_drop_take(void *ptr) {
  if (!ptr || native_drop_cap == 0)
    return NULL;
  pthread_mutex_lock(&native_drop_mu);
  IrisNativeDropFn fn = NULL;
  size_t h = native_drop_hash(ptr, native_drop_cap);
  while (native_drop_slots[h].ptr) {
    if (native_drop_slots[h].ptr == ptr) {
      fn = native_drop_slots[h].fn;
      native_drop_slots[h].ptr = IRIS_NATIVE_DROP_TOMBSTONE;
      native_drop_slots[h].fn = NULL;
      native_drop_live--;
      break;
    }
    h = (h + 1) & (native_drop_cap - 1);
  }
  pthread_mutex_unlock(&native_drop_mu);
  return fn;
}

void iris_forget_native_drop(void *ptr) { (void)native_drop_take(ptr); }

/* Must be called with rc_global_mu held. */
static void rc_table_grow_locked(void) {
  size_t buckets =
      rc_table_buckets ? rc_table_buckets * 4 : RC_TABLE_INITIAL_BUCKETS;
  RcEntry **table = (RcEntry **)calloc(buckets, sizeof(RcEntry *));
  if (!table)
    return; /* keep the current table: slower, still correct */
  for (size_t h = 0; h < rc_table_buckets; h++) {
    RcEntry *e = rc_table[h];
    while (e) {
      RcEntry *next = e->next;
      size_t nh = rc_hash_in(e->ptr, buckets);
      e->next = table[nh];
      table[nh] = e;
      e = next;
    }
  }
  free(rc_table);
  rc_table = table;
  rc_table_buckets = buckets;
  rc_take_cursor = 0;
}

/* Must be called with rc_global_mu held. */
static RcEntry *rc_find(void *ptr) {
  if (!rc_table)
    return NULL;
  size_t h = rc_hash(ptr);
  RcEntry *e = rc_table[h];
  while (e) {
    if (e->ptr == ptr)
      return e;
    e = e->next;
  }
  return NULL;
}

/* A dropped weak ref must be unlinked from its target before its own block is
 * freed. Otherwise the target keeps a list of dead nodes and later walks into
 * freed memory when it is collected. */
static void unlink_weak_ref_from_target(IrisWeakRef *w) {
  if (!w || !w->target)
    return;
  pthread_mutex_lock(&rc_global_mu);
  RcEntry *e = rc_find(w->target);
  if (e) {
    IrisWeakRef **pp = &e->weak_refs;
    while (*pp && *pp != w)
      pp = &(*pp)->next_weak;
    if (*pp == w)
      *pp = w->next_weak;
  }
  pthread_mutex_unlock(&rc_global_mu);
}

/* Must be called with rc_global_mu held. */
static RcEntry *rc_insert(void *ptr, int32_t kind) {
  if (!rc_table ||
      (rc_table_count > rc_table_buckets * 2 && !rc_table_sweeping)) {
    rc_table_grow_locked();
    if (!rc_table) {
      iris_panic("out of memory creating the reference-count table");
      return NULL;
    }
  }
  size_t h = rc_hash(ptr);
  RcEntry *e = (RcEntry *)malloc(sizeof(RcEntry));
  if (!e) {
    iris_panic("out of memory creating reference-count entry");
    return NULL;
  }
  e->ptr = ptr;
  e->kind = kind;
  e->count = 1;
  e->scan_count = 0;
  e->color = RC_COLOR_BLACK;
  e->buffered = 0;
  e->immortal = 0;
  e->weak_refs = NULL;
  e->next = rc_table[h];
  rc_table[h] = e;
  rc_table_count++;
  gc_total_allocated++;
  if (gc_auto_threshold > 0 && (gc_total_allocated % gc_auto_threshold == 0)) {
    iris_gc_cycle_collect_locked();
  }
  return e;
}

/* Must be called with rc_global_mu held. Removes and returns one live entry
 * from the RC table so cleanup can deep-free it outside the global lock. */
static RcEntry *rc_take_one_locked(void) {
  for (; rc_take_cursor < rc_table_buckets; rc_take_cursor++) {
    RcEntry *e = rc_table[rc_take_cursor];
    if (e) {
      rc_table[rc_take_cursor] = e->next;
      e->next = NULL;
      rc_table_count--;
      return e;
    }
  }
  return NULL;
}

static void rc_free_list_payload(IrisList *list) {
  if (!list)
    return;
  /* Elements first, while the arena is still there: a list can hold values
   * in its arena and retained objects (strings, records) side by side, and
   * the objects used to leak whenever the arena existed. Arena values are
   * never in the refcount table, so only owned objects are released. */
  /* One lock for the whole scan, not one per element: freeing a list of
   * two million arena integers took 60 ms in lock traffic alone. The owned
   * objects are gathered at the front of `data` (the list is going away)
   * and released after the lock is dropped, since releasing takes it. */
  size_t owned = 0;
  pthread_mutex_lock(&rc_global_mu);
  for (size_t i = 0; i < list->len; i++) {
    if (list->data[i] && rc_find(list->data[i]))
      list->data[owned++] = list->data[i];
  }
  pthread_mutex_unlock(&rc_global_mu);
  for (size_t i = 0; i < owned; i++)
    iris_release(list->data[i]);
  IrisValChunk *c = list->chunks;
  while (c) {
    IrisValChunk *next = c->next;
    xfree(c);
    c = next;
  }
  xfree(list->data);
  xfree(list);
}

static void rc_free_map_payload(IrisMap *m) {
  if (!m)
    return;
  for (size_t i = 0; i < m->n_buckets; i++) {
    IrisMapEntry *e = m->buckets[i];
    while (e) {
      IrisMapEntry *next = e->next;
      xfree(e->key);
      if (e->val)
        iris_release(e->val);
      xfree(e);
      e = next;
    }
  }
  xfree(m->buckets);
  xfree(m);
}

static void rc_free_channel_payload(IrisChannel *c) {
  if (!c)
    return;
  for (size_t i = 0; i < c->count; i++) {
    size_t idx = (c->head + i) % c->cap;
    if (c->buf[idx])
      iris_release(c->buf[idx]);
  }
  xfree(c->buf);
  pthread_mutex_destroy(&c->mu);
  pthread_cond_destroy(&c->not_empty);
  pthread_cond_destroy(&c->not_full);
  xfree(c);
}

static void rc_free_atomic_payload(IrisAtomic *a) {
  if (!a)
    return;
  if (a->val)
    iris_release(a->val);
  pthread_mutex_destroy(&a->mu);
  xfree(a);
}

static void rc_free_mutex_payload(IrisMutex *m) {
  if (!m)
    return;
  if (m->val)
    iris_release(m->val);
  pthread_mutex_destroy(&m->mu);
  xfree(m);
}

static void rc_free_task_group_payload(IrisTaskGroup *tg) {
  if (!tg)
    return;
  iris_task_group_cancel(tg);
  iris_task_group_join(tg);
  pthread_cond_destroy(&tg->done);
  pthread_mutex_destroy(&tg->mu);
  xfree(tg);
}

static void rc_free_sparse_payload(IrisSparse *sp) {
  if (!sp)
    return;
  for (size_t i = 0; i < sp->len; i++) {
    if (sp->values[i])
      iris_release(sp->values[i]);
  }
  xfree(sp->indices);
  xfree(sp->values);
  xfree(sp);
}

static void rc_deep_free_by_kind(void *ptr, int32_t kind) {
  if (iris_is_immediate_tag(ptr))
    return;
  switch (kind) {
  case IRIS_RC_BOXED: {
    IrisVal *val = (IrisVal *)ptr;
    /* A value inside a list's chunk arena belongs to that arena, which
     * frees it with the list. Freeing it here freed memory in the
     * middle of a block -- heap corruption (0xC0000374) after a
     * list_slice or list_concat of an integer list. */
    if (!ds_contains(val))
      break;
    switch (val->tag) {
    case IRIS_TAG_STR:
      /* Release the underlying string via the RC table so ownership
       * and refcounts are consistent. If no RC entry exists the
       * call is a no-op; otherwise the string will be freed when
       * its count reaches zero. */
      if (val->str)
        iris_release_kind(val->str, IRIS_RC_STR);
      break;
    case IRIS_TAG_LIST:
      iris_release_kind(val->ptr, IRIS_RC_LIST);
      break;
    case IRIS_TAG_MAP:
      iris_release_kind(val->ptr, IRIS_RC_MAP);
      break;
    case IRIS_TAG_OPTION:
      iris_release_kind(val->ptr, IRIS_RC_OPTION);
      break;
    case IRIS_TAG_RESULT:
      iris_release_kind(val->ptr, IRIS_RC_RESULT);
      break;
    case IRIS_TAG_CHAN:
      iris_release_kind(val->ptr, IRIS_RC_CHAN);
      break;
    case IRIS_TAG_ATOMIC:
      iris_release_kind(val->ptr, IRIS_RC_ATOMIC);
      break;
    case IRIS_TAG_GRAD:
      iris_release_kind(val->ptr, IRIS_RC_GRAD);
      break;
    case IRIS_TAG_SPARSE:
      iris_release_kind(val->ptr, IRIS_RC_SPARSE);
      break;
    case IRIS_TAG_MUTEX:
      iris_release_kind(val->ptr, IRIS_RC_MUTEX);
      break;
    case IRIS_TAG_TASK_GROUP:
      iris_release_kind(val->ptr, IRIS_RC_TASK_GROUP);
      break;
    case IRIS_TAG_NATIVE_OBJECT:
      iris_release_kind(val->ptr, IRIS_RC_NATIVE_OBJECT);
      break;
    case IRIS_TAG_TUPLE:
    case IRIS_TAG_STRUCT:
      rc_free_list_payload((IrisList *)val->ptr);
      break;
    case IRIS_TAG_CLOSURE: {
      IrisClosure *c = (IrisClosure *)val->ptr;
      if (c) {
        rc_free_list_payload(c->captures);
        xfree(c);
      }
      break;
    }
    default:
      break;
    }
    xfree(val);
    break;
  }
  case IRIS_RC_STR:
    if (ds_contains(ptr)) {
      xfree((char *)ptr);
    }
    break;
  case IRIS_RC_LIST:
    rc_free_list_payload((IrisList *)ptr);
    break;
  case IRIS_RC_MAP:
    rc_free_map_payload((IrisMap *)ptr);
    break;
  case IRIS_RC_OPTION: {
    IrisOption *opt = (IrisOption *)ptr;
    if (opt) {
      if (opt->has_value && opt->value)
        iris_release(opt->value);
      xfree(opt);
    }
    break;
  }
  case IRIS_RC_RESULT: {
    IrisResult *res = (IrisResult *)ptr;
    if (res) {
      if (res->value)
        iris_release(res->value);
      xfree(res);
    }
    break;
  }
  case IRIS_RC_CHAN:
    rc_free_channel_payload((IrisChannel *)ptr);
    break;
  case IRIS_RC_ATOMIC:
    rc_free_atomic_payload((IrisAtomic *)ptr);
    break;
  case IRIS_RC_MUTEX:
    rc_free_mutex_payload((IrisMutex *)ptr);
    break;
  case IRIS_RC_GRAD:
    xfree((IrisGrad *)ptr);
    break;
  case IRIS_RC_SPARSE:
    rc_free_sparse_payload((IrisSparse *)ptr);
    break;
  case IRIS_RC_TASK_GROUP:
    rc_free_task_group_payload((IrisTaskGroup *)ptr);
    break;
  case IRIS_RC_NATIVE_OBJECT: {
    /* Nominal records are opaque native allocations. The compiler registers
     * a per-type destructor that releases the record's owned fields; call it
     * before freeing the block so replacing a record does not leak its
     * strings, lists and nested records. */
    IrisNativeDropFn drop = native_drop_take(ptr);
    if (drop)
      drop(ptr);
    xfree(ptr);
    break;
  }
  case IRIS_RC_WEAK_REF:
    unlink_weak_ref_from_target((IrisWeakRef *)ptr);
    xfree(ptr);
    break;
  default:
    xfree(ptr);
    break;
  }
}

void iris_retain_kind(void *ptr, int32_t kind) {
  if (iris_is_immediate_tag(ptr))
    return;
  IrisUiPool *ap = iris_active_ui_pool;
  pthread_mutex_lock(&rc_global_mu);
  RcEntry *e = rc_find(ptr);
  if (e) {
    if (e->immortal) {
      pthread_mutex_unlock(&rc_global_mu);
      return;
    }
    e->count++;
    e->color = RC_COLOR_BLACK;
  } else {
    rc_insert(ptr, kind);
  }
  pthread_mutex_unlock(&rc_global_mu);
  if (ap && !iris_ui_pool_owns(ap, ptr)) {
    iris_ui_pool_ext_add(ap, ptr, kind, 1);
  }
}

void iris_retain(void *ptr) { iris_retain_kind(ptr, IRIS_RC_BOXED); }

void iris_release_kind(void *ptr, int32_t kind) {
  if (iris_is_immediate_tag(ptr))
    return;
  IrisUiPool *ap = iris_active_ui_pool;
  int ext_release = (ap && !iris_ui_pool_owns(ap, ptr)) ? 1 : 0;
  int had_entry = 0;
  pthread_mutex_lock(&rc_global_mu);
  RcEntry *e = rc_find(ptr);
  if (!e) {
    pthread_mutex_unlock(&rc_global_mu);
    return;
  }
  if (e->immortal) {
    pthread_mutex_unlock(&rc_global_mu);
    return;
  }
  had_entry = 1;
  e->count--;
  if (e->count <= 0) {
    int32_t free_kind = e->kind;
    e->color = RC_COLOR_BLACK;
    pthread_mutex_unlock(&rc_global_mu);
    rc_deep_free_by_kind(ptr, free_kind);
    pthread_mutex_lock(&rc_global_mu);
    gc_total_freed++;
    size_t h = rc_hash(ptr);
    RcEntry **pp = &rc_table[h];
    while (*pp) {
      if ((*pp)->ptr == ptr) {
        RcEntry *tmp = *pp;
        invalidate_weak_refs_locked(tmp);
        *pp = tmp->next;
        rc_table_count--;
        free(tmp);
        break;
      }
      pp = &((*pp)->next);
    }
  } else {
    if (e->color != RC_COLOR_GRAY && !e->buffered) {
      e->color = RC_COLOR_GRAY;
      e->buffered = 1;
      if (possible_roots_count < POSSIBLE_ROOTS_MAX) {
        possible_roots[possible_roots_count++] = ptr;
      }
    }
    if (kind != IRIS_RC_BOXED) {
      e->kind = kind;
    }
  }
  pthread_mutex_unlock(&rc_global_mu);
  if (ext_release && had_entry) {
    iris_ui_pool_ext_sub(ap, ptr);
  }
}

void iris_release(void *ptr) { iris_release_kind(ptr, IRIS_RC_BOXED); }

/* Interned string literals.
 *
 * `ConstStr` used to compile to a pointer into a read-only `@.str.N` global.
 * The GC annotation pass does not release the literal itself, but a literal
 * copied through a record field or `list_get` reaches `Release` as an ordinary
 * string, and a zero refcount made `rc_deep_free_by_kind` `xfree` that static
 * address -- corrupting the heap and leaving every other use of the literal
 * pointing at freed memory (observed as a garbled `mode`).
 *
 * Generated code now calls this with a per-literal cache slot. The first call
 * allocates a process-wide copy, marks its RC entry immortal, and every later
 * use of the same literal returns the same buffer. Retain/release on an
 * immortal entry are no-ops, so literals can flow through records, lists and
 * channels without ever being freed. */
static pthread_mutex_t iris_intern_mu = PTHREAD_MUTEX_INITIALIZER;

char *iris_intern_lit(char **slot, const char *bytes, int64_t len) {
  if (!slot)
    return (char *)bytes;
  pthread_mutex_lock(&iris_intern_mu);
  char *s = *slot;
  if (!s) {
    size_t n = len > 0 ? (size_t)len : 1;
    char *copy = (char *)malloc(n);
    if (!copy) {
      pthread_mutex_unlock(&iris_intern_mu);
      iris_panic("out of memory interning a string literal");
      return NULL;
    }
    if (bytes && len > 0) {
      memcpy(copy, bytes, n);
    } else {
      copy[0] = '\0';
    }
    copy[n - 1] = '\0';
    pthread_mutex_lock(&rc_global_mu);
    RcEntry *e = rc_find(copy);
    if (!e)
      e = rc_insert(copy, IRIS_RC_STR);
    if (e) {
      e->immortal = 1;
      e->count = 1;
      e->color = RC_COLOR_BLACK;
      e->buffered = 0;
    }
    pthread_mutex_unlock(&rc_global_mu);
    *slot = copy;
    s = copy;
  }
  pthread_mutex_unlock(&iris_intern_mu);
  return s;
}

int64_t iris_ui_pool_begin(void) {
  IrisUiPool *p = (IrisUiPool *)calloc(1, sizeof(IrisUiPool));
  if (!p)
    return 0;
  pthread_mutex_init(&p->mu, NULL);
  p->prev_tls = iris_active_ui_pool;
  pthread_mutex_lock(&iris_ui_pool_reg_mu);
  p->next_reg = iris_ui_pool_reg_head;
  iris_ui_pool_reg_head = p;
  iris_ui_pool_any = 1;
  pthread_mutex_unlock(&iris_ui_pool_reg_mu);
  iris_active_ui_pool = p;
  return (int64_t)(uintptr_t)p;
}

int64_t iris_ui_pool_pause(void) {
  IrisUiPool *cur = iris_active_ui_pool;
  iris_active_ui_pool = NULL;
  return (int64_t)(uintptr_t)cur;
}

int64_t iris_ui_pool_resume(int64_t handle) {
  IrisUiPool *prev = iris_active_ui_pool;
  iris_active_ui_pool = (IrisUiPool *)(uintptr_t)handle;
  return (int64_t)(uintptr_t)prev;
}

int64_t iris_ui_pool_destroy(int64_t handle) {
  if (handle == 0)
    return 0;
  IrisUiPool *p = (IrisUiPool *)(uintptr_t)handle;
  IrisUiPool *saved_tls = iris_active_ui_pool;
  if (saved_tls == p) {
    saved_tls = p->prev_tls;
  }
  iris_active_ui_pool = NULL;

  /* 1. Release net external retains accumulated while pool was active. */
  if (p->ext_cap > 0 && p->ext) {
    for (size_t i = 0; i < p->ext_cap; i++) {
      void *ep = p->ext[i].ptr;
      int64_t delta = p->ext[i].delta;
      int32_t kind = p->ext[i].kind;
      if (ep && ep != DS_TOMBSTONE && delta > 0) {
        for (int64_t d = 0; d < delta; d++) {
          iris_release_kind(ep, kind);
        }
      }
    }
  }

  /* 2. Unregister pool before clearing rc_table / ds_slots / freeing. */
  pthread_mutex_lock(&iris_ui_pool_reg_mu);
  IrisUiPool **rpp = &iris_ui_pool_reg_head;
  while (*rpp) {
    if (*rpp == p) {
      *rpp = p->next_reg;
      break;
    }
    rpp = &((*rpp)->next_reg);
  }
  iris_ui_pool_any = (iris_ui_pool_reg_head != NULL) ? 1 : 0;
  pthread_mutex_unlock(&iris_ui_pool_reg_mu);

  /* 3. Drop RcEntry nodes for pool-owned objects without recursive frees. */
  if (p->ptr_cap > 0 && p->ptrs && rc_table) {
    pthread_mutex_lock(&rc_global_mu);
    for (size_t i = 0; i < p->ptr_cap; i++) {
      void *v = p->ptrs[i];
      if (!v || v == DS_TOMBSTONE)
        continue;
      size_t h = rc_hash(v);
      RcEntry **pp = &rc_table[h];
      while (*pp) {
        if ((*pp)->ptr == v) {
          RcEntry *tmp = *pp;
          invalidate_weak_refs_locked(tmp);
          *pp = tmp->next;
          rc_table_count--;
          gc_total_freed++;
          free(tmp);
          break;
        }
        pp = &((*pp)->next);
      }
    }
    pthread_mutex_unlock(&rc_global_mu);
  }

  /* 4. Remove from ds_slots and free pool-owned heap blocks. */
  if (p->ptr_cap > 0 && p->ptrs) {
    pthread_mutex_lock(&ds_mu);
    for (size_t i = 0; i < p->ptr_cap; i++) {
      void *v = p->ptrs[i];
      if (v && v != DS_TOMBSTONE) {
        size_t bytes = 0;
        ds_remove_locked(v, &bytes);
        g_raw_frees++;
        g_raw_bytes_live -= (int64_t)bytes;
      }
    }
    pthread_mutex_unlock(&ds_mu);

    for (size_t i = 0; i < p->ptr_cap; i++) {
      void *v = p->ptrs[i];
      if (v && v != DS_TOMBSTONE)
        free(v);
    }
    p->bytes_live = 0;
    free(p->ptrs);
    free(p->ptr_bytes);
  }

  free(p->ext);
  pthread_mutex_destroy(&p->mu);
  free(p);
  iris_active_ui_pool = saved_tls;
  return 0;
}

int64_t iris_async_pause_pool(void) {
  IrisUiPool *p = iris_active_ui_pool;
  iris_active_ui_pool = NULL;
  return (int64_t)(uintptr_t)p;
}

void iris_async_resume_pool(int64_t handle) {
  iris_active_ui_pool = (IrisUiPool *)(uintptr_t)handle;
}

/* Drop an owned temporary IrisVal box. Spawn environments own their boxes but
 * do not insert the wrappers into the RC table unless another container has
 * retained them. Scalars therefore need an explicit destruction path, while
 * heap-reference boxes must also release the payload retain from box_heap_ref.
 */
void iris_drop_box(IrisVal *value) {
  if (iris_is_immediate_tag(value))
    return;
  pthread_mutex_lock(&rc_global_mu);
  int tracked = rc_find(value) != NULL;
  pthread_mutex_unlock(&rc_global_mu);
  if (tracked)
    iris_release(value);
  else
    rc_deep_free_by_kind(value, IRIS_RC_BOXED);
}

int64_t iris_refcount(void *ptr) {
  if (!ptr)
    return 0;
  pthread_mutex_lock(&rc_global_mu);
  RcEntry *e = rc_find(ptr);
  int64_t c = e ? e->count : 0;
  pthread_mutex_unlock(&rc_global_mu);
  return c;
}

/* ── Bacon-Rajan cycle collector ─────────────────────────────────────────── */

/* Call fn(child_ptr) for every RC-tracked child owned by a node. */
typedef void (*ChildFn)(void *, void *);
static void rc_each_child(void *ptr, int32_t kind, ChildFn fn, void *ctx) {
  if (iris_is_immediate_tag(ptr))
    return;
  switch (kind) {
  case IRIS_RC_BOXED: {
    IrisVal *val = (IrisVal *)ptr;
    switch (val->tag) {
    case IRIS_TAG_LIST:
    case IRIS_TAG_MAP:
    case IRIS_TAG_OPTION:
    case IRIS_TAG_RESULT:
    case IRIS_TAG_CHAN:
    case IRIS_TAG_ATOMIC:
    case IRIS_TAG_GRAD:
    case IRIS_TAG_SPARSE:
    case IRIS_TAG_MUTEX:
    case IRIS_TAG_TASK_GROUP:
    case IRIS_TAG_NATIVE_OBJECT:
      if (val->ptr)
        fn(val->ptr, ctx);
      break;
    case IRIS_TAG_TUPLE:
    case IRIS_TAG_STRUCT: {
      IrisList *list = (IrisList *)val->ptr;
      if (list)
        for (size_t i = 0; i < list->len; i++)
          fn(list->data[i], ctx);
      break;
    }
    case IRIS_TAG_CLOSURE: {
      IrisClosure *c = (IrisClosure *)val->ptr;
      if (c && c->captures) {
        for (size_t i = 0; i < c->captures->len; i++)
          fn(c->captures->data[i], ctx);
      }
      break;
    }
    default:
      break;
    }
    break;
  }
  case IRIS_RC_LIST: {
    IrisList *list = (IrisList *)ptr;
    if (list)
      for (size_t i = 0; i < list->len; i++)
        fn(list->data[i], ctx);
    break;
  }
  case IRIS_RC_MAP: {
    IrisMap *m = (IrisMap *)ptr;
    if (m)
      for (size_t i = 0; i < m->n_buckets; i++)
        for (IrisMapEntry *e = m->buckets[i]; e; e = e->next)
          fn(e->val, ctx);
    break;
  }
  case IRIS_RC_OPTION: {
    IrisOption *opt = (IrisOption *)ptr;
    if (opt && opt->has_value)
      fn(opt->value, ctx);
    break;
  }
  case IRIS_RC_RESULT: {
    IrisResult *res = (IrisResult *)ptr;
    if (res && res->value)
      fn(res->value, ctx);
    break;
  }
  case IRIS_RC_CHAN: {
    IrisChannel *c = (IrisChannel *)ptr;
    if (c) {
      for (size_t i = 0; i < c->count; i++) {
        size_t idx = (c->head + i) % c->cap;
        fn(c->buf[idx], ctx);
      }
    }
    break;
  }
  case IRIS_RC_ATOMIC: {
    IrisAtomic *a = (IrisAtomic *)ptr;
    if (a && a->val)
      fn(a->val, ctx);
    break;
  }
  case IRIS_RC_SPARSE: {
    IrisSparse *sp = (IrisSparse *)ptr;
    if (sp)
      for (size_t i = 0; i < sp->len; i++)
        fn(sp->values[i], ctx);
    break;
  }
  default:
    break;
  }
}

/* Phase 2 helper: decrement scan_count of child. */
static void rc_mark_gray_child(void *child, void *ctx) {
  (void)ctx;
  if (!child)
    return;
  RcEntry *e = rc_find(child);
  if (e)
    e->scan_count--;
}

/* Phase 3 helper: restore children to BLACK (external ref exists). */
static void rc_scan_black_child(void *child, void *ctx);
static void rc_scan_black_node(RcEntry *e) {
  if (!e || e->color == RC_COLOR_BLACK)
    return;
  e->color = RC_COLOR_BLACK;
  rc_each_child(e->ptr, e->kind, rc_scan_black_child, NULL);
}
static void rc_scan_black_child(void *child, void *ctx) {
  (void)ctx;
  if (!child)
    return;
  RcEntry *e = rc_find(child);
  if (e) {
    e->scan_count++;
    rc_scan_black_node(e);
  }
}

/* Cycle collection using the Bacon-Rajan trial-deletion algorithm.
 * Must be called with rc_global_mu held. */
static void iris_gc_cycle_collect_locked(void) {
  if (possible_roots_count == 0)
    return;

  /* Phase 1: Init scan_count from live count for all gray roots. */
  for (int i = 0; i < possible_roots_count; i++) {
    RcEntry *e = rc_find(possible_roots[i]);
    if (e && e->color == RC_COLOR_GRAY)
      e->scan_count = e->count;
  }

  /* Phase 2: For each gray root, decrement scan_count of each child pointer.
   * This simulates removing the internal (intra-cycle) references. */
  for (int i = 0; i < possible_roots_count; i++) {
    RcEntry *e = rc_find(possible_roots[i]);
    if (e && e->color == RC_COLOR_GRAY)
      rc_each_child(e->ptr, e->kind, rc_mark_gray_child, NULL);
  }

  /* Phase 3: Nodes with scan_count > 0 have external references — mark BLACK.
   * Nodes with scan_count == 0 are unreachable from outside — mark WHITE. */
  for (int i = 0; i < possible_roots_count; i++) {
    RcEntry *e = rc_find(possible_roots[i]);
    if (!e)
      continue;
    e->buffered = 0;
    if (e->color == RC_COLOR_GRAY) {
      if (e->scan_count > 0) {
        rc_scan_black_node(e); /* restore external refs transitively */
      } else {
        e->color = RC_COLOR_WHITE;
      }
    }
  }
  possible_roots_count = 0;

  /* Phase 4: Free all White (cyclic garbage) nodes. */
  rc_table_sweeping++;
  for (size_t h = 0; h < rc_table_buckets; h++) {
    RcEntry **pp = &rc_table[h];
    while (*pp) {
      if ((*pp)->color == RC_COLOR_WHITE && !(*pp)->immortal) {
        RcEntry *tmp = *pp;
        *pp = tmp->next;
        rc_table_count--;
        /* Deep-free outside the lock to avoid recursive acquire. */
        pthread_mutex_unlock(&rc_global_mu);
        rc_deep_free_by_kind(tmp->ptr, tmp->kind);
        pthread_mutex_lock(&rc_global_mu);
        gc_total_freed++;
        free(tmp);
      } else {
        pp = &((*pp)->next);
      }
    }
  }
  rc_table_sweeping--;
}

void iris_gc_collect(void) {
  pthread_mutex_lock(&rc_global_mu);
  /* First run the cycle collector to break cycles, then sweep zero-count
   * entries. */
  iris_gc_cycle_collect_locked();
  rc_table_sweeping++;
  for (size_t h = 0; h < rc_table_buckets; h++) {
    RcEntry **pp = &rc_table[h];
    while (*pp) {
      if ((*pp)->count <= 0 && !(*pp)->immortal) {
        RcEntry *tmp = *pp;
        *pp = tmp->next;
        rc_table_count--;
        pthread_mutex_unlock(&rc_global_mu);
        rc_deep_free_by_kind(tmp->ptr, tmp->kind);
        pthread_mutex_lock(&rc_global_mu);
        gc_total_freed++;
        free(tmp);
      } else {
        pp = &((*pp)->next);
      }
    }
  }
  rc_table_sweeping--;
  pthread_mutex_unlock(&rc_global_mu);
}

int64_t iris_gc_stats_allocated(void) { return gc_total_allocated; }

int64_t iris_gc_stats_freed(void) { return gc_total_freed; }

/* iris_runtime_cleanup — frees all GC-tracked live objects at program exit.
 * Also frees the RC side-table itself.  Registered via atexit() in the
 * constructor below so that sanitizers (ASAN/Valgrind) report a clean heap. */
static void iris_runtime_cleanup(void) {
  /* Freeing every object still alive at exit buys nothing -- the process's
   * memory goes back to the system anyway, stdio is flushed by exit(), and
   * LeakSanitizer counts what the refcount table still points to as
   * reachable. It did cost crashes: an object shared between two live
   * structures was freed once through each, so a program that printed the
   * right answer then died with heap corruption (0xC0000374) at exit.
   * IRIS_FREE_AT_EXIT=1 keeps the old behaviour for leak hunting. */
  const char *free_at_exit = getenv("IRIS_FREE_AT_EXIT");
  if (!free_at_exit || free_at_exit[0] == '\0' || free_at_exit[0] == '0')
    return;

  /* The main thread has no TLS destructor to run, so free its tape here. */
  iris_tape_thread_free();

  /* Run cycle collector one final time before cleanup. */
  pthread_mutex_lock(&rc_global_mu);
  iris_gc_cycle_collect_locked();
  possible_roots_count = 0;
  rc_take_cursor = 0;
  RcEntry *e = rc_take_one_locked();
  pthread_mutex_unlock(&rc_global_mu);
  while (e) {
    if (!e->immortal)
      rc_deep_free_by_kind(e->ptr, e->kind);
    gc_total_freed++;
    free(e);
    pthread_mutex_lock(&rc_global_mu);
    e = rc_take_one_locked();
    pthread_mutex_unlock(&rc_global_mu);
  }
}

#ifdef _MSC_VER
/* MSVC: use a pragma section-based init instead of
 * __attribute__((constructor)). */
static int _iris_runtime_init(void) {
  atexit(iris_runtime_cleanup);
  return 0;
}
#pragma section(".CRT$XCU", read)
__declspec(allocate(".CRT$XCU")) static int (*_iris_init_ptr)(void) =
    _iris_runtime_init;
#else
#ifdef _WIN32
/* A stack overflow ends the process with 0xC00000FD and no word of why.
 * Report it the way the interpreter does. Very little stack is left in the
 * handler, so only Win32 calls: no stdio. */
static LONG WINAPI iris_stack_overflow_handler(EXCEPTION_POINTERS *info) {
  if (info && info->ExceptionRecord &&
      info->ExceptionRecord->ExceptionCode == EXCEPTION_STACK_OVERFLOW) {
    char buf[512];
    /* Keep what the program printed. The CRT's stream locks are
     * recursive, so this cannot deadlock on this thread; the stack
     * guarantee set at start-up leaves room for it. */
    fflush(stdout);
    iris_emit_report(
        buf,
        iris_format_report(
            buf, sizeof buf, "E0408",
            "stack overflow: the recursion is too deep, or never ends", 1));
    TerminateProcess(GetCurrentProcess(), 101);
  }
  return EXCEPTION_CONTINUE_SEARCH;
}
#elif !defined(__wasm__)
/* The main thread's stack: a fault inside it, below its top, is an
 * overflow; anywhere else it is a bad memory access. */
static uintptr_t iris_stack_top = 0;
static uintptr_t iris_stack_span = 0;

static void iris_segv_handler(int sig, siginfo_t *info, void *ctx) {
  (void)sig;
  (void)ctx;
  uintptr_t addr = info ? (uintptr_t)info->si_addr : 0;
  int overflow = iris_stack_top && addr < iris_stack_top &&
                 iris_stack_top - addr <= iris_stack_span;
  char buf[512];
  fflush(stdout);
  iris_emit_report(
      buf,
      overflow
          ? iris_format_report(
                buf, sizeof buf, "E0408",
                "stack overflow: the recursion is too deep, or never ends", 1)
          : iris_format_report(buf, sizeof buf, "E0400",
                               "invalid memory access (segmentation fault): a "
                               "bug in IRIS or in a C library it calls",
                               1));
  _exit(101);
}
#endif

__attribute__((constructor)) static void iris_runtime_init(void) {
#ifdef _WIN32
  {
    /* Room for the handler to run once the stack is gone. */
    ULONG guarantee = 64 * 1024;
    SetThreadStackGuarantee(&guarantee);
  }
  AddVectoredExceptionHandler(1, iris_stack_overflow_handler);
#elif !defined(__wasm__)
  {
    int here = 0;
    struct rlimit rl;
    iris_stack_top = (uintptr_t)&here;
    iris_stack_span =
        (getrlimit(RLIMIT_STACK, &rl) == 0 && rl.rlim_cur != RLIM_INFINITY)
            ? (uintptr_t)rl.rlim_cur + 1024 * 1024
            : (uintptr_t)64 * 1024 * 1024;
  }
  /* An alternate stack, so the handler can run when the stack is gone. */
  static char altstack[64 * 1024];
  stack_t ss;
  ss.ss_sp = altstack;
  ss.ss_size = sizeof altstack;
  ss.ss_flags = 0;
  if (sigaltstack(&ss, NULL) == 0) {
    struct sigaction sa;
    memset(&sa, 0, sizeof sa);
    sa.sa_sigaction = iris_segv_handler;
    sa.sa_flags = SA_ONSTACK | SA_SIGINFO;
    sigemptyset(&sa.sa_mask);
    sigaction(SIGSEGV, &sa, NULL);
  }
#endif
#if !defined(__wasm__)
  iris_error_file = getenv("IRIS_ERROR_FILE");
  if (iris_error_file && !*iris_error_file)
    iris_error_file = NULL;
#endif
  atexit(iris_runtime_cleanup);
}
#endif

/* ====================================================================
 * Sandbox / Security Policy (C runtime side)
 * ====================================================================
 * Simple global flags. The Rust-side SecurityPolicy is the authoritative
 * source; this C-side mirror is for native-compiled IRIS programs.
 */

/* ---- Full SecurityPolicy mediation for native binaries & JIT runtime ---- */
typedef struct {
  char **items;
  size_t count;
  size_t cap;
} IrisSandboxList;

typedef struct {
  void *handle;
  char *path;
  int is_write;
} IrisSandboxOpenFile;

typedef struct {
  int initialized;
  int locked;
  int allow_fs_read;
  int allow_fs_write;
  int allow_network;
  int allow_ffi;
  int allow_process;
  int64_t max_file_write_bytes;
  int64_t max_open_files;
  int64_t max_connections;
  int64_t open_files;
  int64_t open_connections;
  IrisSandboxList fs_read_allow;
  IrisSandboxList fs_write_allow;
  IrisSandboxList fs_block;
  IrisSandboxList net_allow;
  IrisSandboxList net_block;
  IrisSandboxList ffi_allow;
  IrisSandboxOpenFile *tracked_files;
  size_t tracked_files_count;
  size_t tracked_files_cap;
  int64_t *tracked_conns;
  size_t tracked_conns_count;
  size_t tracked_conns_cap;
} IrisSandboxState;

static pthread_mutex_t iris_sandbox_mu = PTHREAD_MUTEX_INITIALIZER;
static IrisSandboxState iris_sb = {0,
                                   0,
                                   1,
                                   1,
                                   1,
                                   1,
                                   1,
                                   0,
                                   0,
                                   0,
                                   0,
                                   0,
                                   {NULL, 0, 0},
                                   {NULL, 0, 0},
                                   {NULL, 0, 0},
                                   {NULL, 0, 0},
                                   {NULL, 0, 0},
                                   {NULL, 0, 0},
                                   NULL,
                                   0,
                                   0,
                                   NULL,
                                   0,
                                   0};

static char *sb_strdup_raw(const char *s) {
  if (!s)
    return NULL;
  size_t len = strlen(s);
  char *out = (char *)malloc(len + 1);
  if (!out)
    return NULL;
  memcpy(out, s, len + 1);
  return out;
}

static void sb_list_clear(IrisSandboxList *list) {
  for (size_t i = 0; i < list->count; i++) {
    free(list->items[i]);
  }
  free(list->items);
  list->items = NULL;
  list->count = 0;
  list->cap = 0;
}

static void sb_list_push(IrisSandboxList *list, const char *s) {
  if (!s || !*s)
    return;
  if (list->count == list->cap) {
    size_t next = list->cap ? list->cap * 2 : 8;
    char **grown = (char **)realloc(list->items, next * sizeof(char *));
    if (!grown)
      return;
    list->items = grown;
    list->cap = next;
  }
  char *copy = sb_strdup_raw(s);
  if (copy)
    list->items[list->count++] = copy;
}

static int sb_ascii_tolower(int c) {
  return (c >= 'A' && c <= 'Z') ? (c + ('a' - 'A')) : c;
}

static int sb_strcasecmp(const char *a, const char *b) {
  if (!a || !b)
    return a == b ? 0 : 1;
  while (*a && *b) {
    int ca = sb_ascii_tolower((unsigned char)*a++);
    int cb = sb_ascii_tolower((unsigned char)*b++);
    if (ca != cb)
      return ca - cb;
  }
  return sb_ascii_tolower((unsigned char)*a) -
         sb_ascii_tolower((unsigned char)*b);
}

static int sb_normalize_path(const char *in, char *out, size_t cap) {
  if (!in || cap == 0)
    return -1;
  size_t o = 0;
  for (size_t i = 0; in[i] != '\0'; i++) {
    char c = (in[i] == '\\') ? '/' : in[i];
    /* Collapse duplicate slashes except leading UNC // */
    if (c == '/' && o > 0 && out[o - 1] == '/' && !(o == 1 && out[0] == '/'))
      continue;
    if (o + 1 >= cap)
      return -1;
    out[o++] = c;
  }
  while (o > 1 && out[o - 1] == '/') {
    if (o == 3 && out[1] == ':' && out[2] == '/')
      break;
    o--;
  }
  out[o] = '\0';
  return 0;
}

static int sb_path_is_safe(const char *raw) {
  if (!raw || !*raw)
    return 0;
  /* Reject Windows device namespace prefixes even with normalized slashes. */
  if ((raw[0] == '\\' || raw[0] == '/') && (raw[1] == '\\' || raw[1] == '/') &&
      (raw[2] == '.' || raw[2] == '?') && (raw[3] == '\\' || raw[3] == '/')) {
#ifdef _WIN32
    return 0;
#endif
  }
  const char *seg_start = raw;
  for (const char *p = raw;; p++) {
    if (*p == '/' || *p == '\\' || *p == '\0') {
      size_t len = (size_t)(p - seg_start);
      if (len == 2 && seg_start[0] == '.' && seg_start[1] == '.')
        return 0;
      if (*p == '\0')
        break;
      seg_start = p + 1;
    }
  }
#ifdef _WIN32
  /* Check final component stem against DOS reserved device names. */
  const char *last_sep = NULL;
  for (const char *p = raw; *p; p++) {
    if (*p == '/' || *p == '\\')
      last_sep = p;
  }
  const char *base = last_sep ? last_sep + 1 : raw;
  const char *dot = strchr(base, '.');
  size_t stem_len = dot ? (size_t)(dot - base) : strlen(base);
  static const char *const dev_names[] = {
      "CON",  "PRN",  "AUX",  "NUL",  "COM1", "COM2", "COM3", "COM4",
      "COM5", "COM6", "COM7", "COM8", "COM9", "LPT1", "LPT2", "LPT3",
      "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9"};
  if (stem_len >= 3 && stem_len <= 4) {
    char upper[8];
    for (size_t i = 0; i < stem_len; i++) {
      char c = base[i];
      upper[i] = (c >= 'a' && c <= 'z') ? (char)(c - ('a' - 'A')) : c;
    }
    upper[stem_len] = '\0';
    for (size_t i = 0; i < sizeof(dev_names) / sizeof(dev_names[0]); i++) {
      if (strcmp(upper, dev_names[i]) == 0)
        return 0;
    }
  }
#endif
  return 1;
}

static int sb_path_matches_one(const char *norm_path, const char *rule_raw) {
  char norm_rule[4096];
  if (sb_normalize_path(rule_raw, norm_rule, sizeof(norm_rule)) != 0)
    return 0;
  size_t rlen = strlen(norm_rule);
  size_t plen = strlen(norm_path);
  if (rlen == 0 || plen < rlen)
    return 0;
  for (size_t i = 0; i < rlen; i++) {
#ifdef _WIN32
    if (sb_ascii_tolower((unsigned char)norm_path[i]) !=
        sb_ascii_tolower((unsigned char)norm_rule[i]))
      return 0;
#else
    if (norm_path[i] != norm_rule[i])
      return 0;
#endif
  }
  return plen == rlen || norm_path[rlen] == '/' ||
         (rlen == 1 && norm_rule[0] == '/');
}

static void sb_resolve_path(const char *raw, char *out, size_t cap) {
#ifdef _WIN32
  char full[4096];
  DWORD n = GetFullPathNameA(raw, (DWORD)sizeof(full), full, NULL);
  if (n > 0 && n < sizeof(full) && sb_normalize_path(full, out, cap) == 0)
    return;
#elif !defined(__wasm__)
  char *rp = realpath(raw, NULL);
  if (rp) {
    int ok = sb_normalize_path(rp, out, cap);
    free(rp);
    if (ok == 0)
      return;
  }
#endif
  if (sb_normalize_path(raw, out, cap) != 0 && cap > 0)
    out[0] = '\0';
}

static int sb_list_matches_path(const IrisSandboxList *list, const char *path) {
  char norm[4096], resolved[4096];
  if (sb_normalize_path(path, norm, sizeof(norm)) != 0)
    return 0;
  sb_resolve_path(path, resolved, sizeof(resolved));
  for (size_t i = 0; i < list->count; i++) {
    if (sb_path_matches_one(norm, list->items[i]))
      return 1;
    char rule_resolved[4096];
    sb_resolve_path(list->items[i], rule_resolved, sizeof(rule_resolved));
    if (resolved[0] && rule_resolved[0] &&
        sb_path_matches_one(resolved, rule_resolved))
      return 1;
  }
  return 0;
}

static void sb_extract_host(const char *raw, char *out, size_t cap) {
  if (!raw || cap == 0) {
    if (cap)
      out[0] = '\0';
    return;
  }
  const char *p = raw;
  if (strncmp(p, "http://", 7) == 0)
    p += 7;
  else if (strncmp(p, "https://", 8) == 0)
    p += 8;
  if (*p == '[') {
    p++;
    const char *end = strchr(p, ']');
    size_t len = end ? (size_t)(end - p) : strlen(p);
    if (len >= cap)
      len = cap - 1;
    memcpy(out, p, len);
    out[len] = '\0';
    return;
  }
  size_t len = 0;
  while (p[len] && p[len] != '/' && p[len] != ':' && p[len] != '?' &&
         p[len] != '#')
    len++;
  if (len >= cap)
    len = cap - 1;
  memcpy(out, p, len);
  out[len] = '\0';
}

static int sb_host_matches_list(const IrisSandboxList *list,
                                const char *host_raw) {
  char host[512];
  sb_extract_host(host_raw, host, sizeof(host));
  if (!host[0])
    return 0;
  size_t hlen = strlen(host);
  for (size_t i = 0; i < list->count; i++) {
    const char *rule = list->items[i];
    size_t rlen = strlen(rule);
    if (rlen == 0)
      continue;
    if (sb_strcasecmp(host, rule) == 0)
      return 1;
    if (hlen > rlen && sb_strcasecmp(host + (hlen - rlen), rule) == 0 &&
        (rule[0] == '.' || host[hlen - rlen - 1] == '.')) {
      return 1;
    }
  }
  return 0;
}

static void sb_reset_defaults_locked(int allow_all) {
  sb_list_clear(&iris_sb.fs_read_allow);
  sb_list_clear(&iris_sb.fs_write_allow);
  sb_list_clear(&iris_sb.fs_block);
  sb_list_clear(&iris_sb.net_allow);
  sb_list_clear(&iris_sb.net_block);
  sb_list_clear(&iris_sb.ffi_allow);
  iris_sb.allow_fs_read = allow_all;
  iris_sb.allow_fs_write = allow_all;
  iris_sb.allow_network = allow_all;
  iris_sb.allow_ffi = allow_all;
  iris_sb.allow_process = allow_all;
  iris_sb.max_file_write_bytes = 0;
  iris_sb.max_open_files = 0;
  iris_sb.max_connections = 0;
}

static void sb_apply_spec_locked(const char *spec) {
  if (!spec)
    return;
  sb_reset_defaults_locked(1);
  const char *p = spec;
  while (*p) {
    const char *nl = strchr(p, '\n');
    size_t len = nl ? (size_t)(nl - p) : strlen(p);
    while (len > 0 && p[len - 1] == '\r')
      len--;
    if (len > 0) {
      char *line = (char *)malloc(len + 1);
      if (line) {
        memcpy(line, p, len);
        line[len] = '\0';
        char *eq = strchr(line, '=');
        if (eq) {
          *eq = '\0';
          const char *k = line;
          const char *v = eq + 1;
          if (strcmp(k, "allow_fs_read") == 0)
            iris_sb.allow_fs_read = atoi(v) != 0;
          else if (strcmp(k, "allow_fs_write") == 0)
            iris_sb.allow_fs_write = atoi(v) != 0;
          else if (strcmp(k, "allow_network") == 0)
            iris_sb.allow_network = atoi(v) != 0;
          else if (strcmp(k, "allow_ffi") == 0)
            iris_sb.allow_ffi = atoi(v) != 0;
          else if (strcmp(k, "allow_process") == 0)
            iris_sb.allow_process = atoi(v) != 0;
          else if (strcmp(k, "max_file_write_bytes") == 0)
            iris_sb.max_file_write_bytes = strtoll(v, NULL, 10);
          else if (strcmp(k, "max_open_files") == 0)
            iris_sb.max_open_files = strtoll(v, NULL, 10);
          else if (strcmp(k, "max_connections") == 0)
            iris_sb.max_connections = strtoll(v, NULL, 10);
          else if (strcmp(k, "fs_read_allow") == 0)
            sb_list_push(&iris_sb.fs_read_allow, v);
          else if (strcmp(k, "fs_write_allow") == 0)
            sb_list_push(&iris_sb.fs_write_allow, v);
          else if (strcmp(k, "fs_block") == 0)
            sb_list_push(&iris_sb.fs_block, v);
          else if (strcmp(k, "net_allow") == 0)
            sb_list_push(&iris_sb.net_allow, v);
          else if (strcmp(k, "net_block") == 0)
            sb_list_push(&iris_sb.net_block, v);
          else if (strcmp(k, "ffi_allow") == 0)
            sb_list_push(&iris_sb.ffi_allow, v);
        }
        free(line);
      }
    }
    p = nl ? nl + 1 : p + strlen(p);
  }
}

static void sb_ensure_init_locked(void) {
  if (iris_sb.initialized)
    return;
  iris_sb.initialized = 1;
  if (iris_sb.locked)
    return;
  const char *spec = getenv("IRIS_SECURITY_POLICY");
  if (spec && *spec) {
    sb_apply_spec_locked(spec);
    return;
  }
  const char *env = getenv("IRIS_SANDBOX");
  if (env && (strcmp(env, "1") == 0 || sb_strcasecmp(env, "true") == 0)) {
    sb_reset_defaults_locked(0);
  }
}

void iris_sandbox_sync_env(void) {
  pthread_mutex_lock(&iris_sandbox_mu);
  if (!iris_sb.locked) {
    iris_sb.initialized = 0;
    sb_ensure_init_locked();
  }
  pthread_mutex_unlock(&iris_sandbox_mu);
}

void iris_sandbox_set_policy(int allow_fs, int allow_net, int allow_ffi) {
  iris_sandbox_set_profile(allow_fs, allow_fs, allow_net, allow_ffi, allow_ffi,
                           0);
}

void iris_sandbox_set_profile(int allow_fs_read, int allow_fs_write,
                              int allow_network, int allow_ffi,
                              int allow_process, int lock_policy) {
  pthread_mutex_lock(&iris_sandbox_mu);
  sb_reset_defaults_locked(1);
  iris_sb.initialized = 1;
  if (lock_policy)
    iris_sb.locked = 1;
  iris_sb.allow_fs_read = allow_fs_read ? 1 : 0;
  iris_sb.allow_fs_write = allow_fs_write ? 1 : 0;
  iris_sb.allow_network = allow_network ? 1 : 0;
  iris_sb.allow_ffi = allow_ffi ? 1 : 0;
  iris_sb.allow_process = allow_process ? 1 : 0;
  pthread_mutex_unlock(&iris_sandbox_mu);
}

void iris_sandbox_set_policy_spec(const char *spec, int lock_policy) {
  pthread_mutex_lock(&iris_sandbox_mu);
  iris_sb.initialized = 1;
  if (spec && *spec)
    sb_apply_spec_locked(spec);
  else
    sb_reset_defaults_locked(1);
  if (lock_policy)
    iris_sb.locked = 1;
  pthread_mutex_unlock(&iris_sandbox_mu);
}

static int sb_check_fs_path_locked(const char *path, int is_write,
                                   int64_t bytes, char *err, size_t err_cap) {
  sb_ensure_init_locked();
  if (is_write && !iris_sb.allow_fs_write) {
    snprintf(err, err_cap, "filesystem write denied: %s",
             path ? path : "<null>");
    return 1;
  }
  if (!is_write && !iris_sb.allow_fs_read) {
    snprintf(err, err_cap, "filesystem read denied: %s",
             path ? path : "<null>");
    return 1;
  }
  if (!path || !sb_path_is_safe(path)) {
    snprintf(err, err_cap, "path traversal or unsafe path blocked: %s",
             path ? path : "<null>");
    return 1;
  }
  if (is_write && iris_sb.max_file_write_bytes > 0 &&
      bytes > iris_sb.max_file_write_bytes) {
    snprintf(err, err_cap,
             "write size %lld exceeds security limit %lld bytes: %s",
             (long long)bytes, (long long)iris_sb.max_file_write_bytes, path);
    return 1;
  }
  if (sb_list_matches_path(&iris_sb.fs_block, path)) {
    snprintf(err, err_cap, "filesystem %s blocked by policy: %s",
             is_write ? "write" : "read", path);
    return 1;
  }
  const IrisSandboxList *allow =
      is_write ? &iris_sb.fs_write_allow : &iris_sb.fs_read_allow;
  if (allow->count > 0 && !sb_list_matches_path(allow, path)) {
    snprintf(err, err_cap, "filesystem %s outside allowlist: %s",
             is_write ? "write" : "read", path);
    return 1;
  }
  return 0;
}

int iris_sandbox_check_fs_read(const char *path) {
  char err[512];
  pthread_mutex_lock(&iris_sandbox_mu);
  int denied = sb_check_fs_path_locked(path, 0, 0, err, sizeof(err));
  pthread_mutex_unlock(&iris_sandbox_mu);
  return denied;
}

int iris_sandbox_check_fs_write(const char *path) {
  return iris_sandbox_check_fs_write_bytes(path, 0);
}

int iris_sandbox_check_fs_write_bytes(const char *path, int64_t bytes) {
  char err[512];
  pthread_mutex_lock(&iris_sandbox_mu);
  int denied = sb_check_fs_path_locked(path, 1, bytes, err, sizeof(err));
  pthread_mutex_unlock(&iris_sandbox_mu);
  return denied;
}

int iris_sandbox_check_network(const char *host) {
  pthread_mutex_lock(&iris_sandbox_mu);
  sb_ensure_init_locked();
  int denied = 0;
  if (!iris_sb.allow_network)
    denied = 1;
  else if (host && *host) {
    if (sb_host_matches_list(&iris_sb.net_block, host))
      denied = 1;
    else if (iris_sb.net_allow.count > 0 &&
             !sb_host_matches_list(&iris_sb.net_allow, host))
      denied = 1;
  }
  pthread_mutex_unlock(&iris_sandbox_mu);
  return denied;
}

int iris_sandbox_check_ffi(const char *lib_path) {
  pthread_mutex_lock(&iris_sandbox_mu);
  sb_ensure_init_locked();
  int denied = 0;
  if (!iris_sb.allow_ffi)
    denied = 1;
  else if (lib_path && iris_sb.ffi_allow.count > 0 &&
           !sb_list_matches_path(&iris_sb.ffi_allow, lib_path)) {
    denied = 1;
  }
  pthread_mutex_unlock(&iris_sandbox_mu);
  return denied;
}

int iris_sandbox_check_process(const char *cmd) {
  (void)cmd;
  pthread_mutex_lock(&iris_sandbox_mu);
  sb_ensure_init_locked();
  int denied = !iris_sb.allow_process;
  pthread_mutex_unlock(&iris_sandbox_mu);
  return denied;
}

void iris_sandbox_require_fs_read(const char *path) {
  char err[512] = {0};
  pthread_mutex_lock(&iris_sandbox_mu);
  int denied = sb_check_fs_path_locked(path, 0, 0, err, sizeof(err));
  pthread_mutex_unlock(&iris_sandbox_mu);
  if (denied)
    iris_runtime_error("E0407", "%s", err);
}

void iris_sandbox_require_fs_write(const char *path) {
  iris_sandbox_require_fs_write_bytes(path, 0);
}

void iris_sandbox_require_fs_write_bytes(const char *path, int64_t bytes) {
  char err[512] = {0};
  pthread_mutex_lock(&iris_sandbox_mu);
  int denied = sb_check_fs_path_locked(path, 1, bytes, err, sizeof(err));
  pthread_mutex_unlock(&iris_sandbox_mu);
  if (denied)
    iris_runtime_error("E0407", "%s", err);
}

void iris_sandbox_require_network(const char *host) {
  if (iris_sandbox_check_network(host) != 0) {
    iris_runtime_error("E0407", "network access denied: %s",
                       host ? host : "<network>");
  }
}

void iris_sandbox_require_ffi(const char *lib_path) {
  if (iris_sandbox_check_ffi(lib_path) != 0) {
    iris_runtime_error("E0407", "FFI access denied: %s",
                       lib_path ? lib_path : "<ffi>");
  }
}

void iris_sandbox_require_process(const char *cmd) {
  if (iris_sandbox_check_process(cmd) != 0) {
    iris_runtime_error("E0407", "process execution denied: %s",
                       cmd ? cmd : "<process>");
  }
}

static void iris_sandbox_require_url_network(const char *url) {
  char host[512];
  sb_extract_host(url, host, sizeof(host));
  iris_sandbox_require_network(host[0] ? host : (url ? url : "<url>"));
}

static void iris_sandbox_require_network_enabled(void) {
  pthread_mutex_lock(&iris_sandbox_mu);
  sb_ensure_init_locked();
  int allowed = iris_sb.allow_network;
  pthread_mutex_unlock(&iris_sandbox_mu);
  if (!allowed) {
    iris_runtime_error("E0407", "network access denied by security policy");
  }
}

static void iris_sandbox_require_ffi_enabled(void) {
  pthread_mutex_lock(&iris_sandbox_mu);
  sb_ensure_init_locked();
  int allowed = iris_sb.allow_ffi;
  pthread_mutex_unlock(&iris_sandbox_mu);
  if (!allowed) {
    iris_runtime_error("E0407", "FFI access denied by security policy");
  }
}

static void iris_sandbox_require_open_slot(void) {
  char err[512] = {0};
  int denied = 0;
  pthread_mutex_lock(&iris_sandbox_mu);
  sb_ensure_init_locked();
  if (iris_sb.max_open_files > 0 &&
      iris_sb.open_files >= iris_sb.max_open_files) {
    snprintf(err, sizeof(err), "open file limit exceeded (%lld)",
             (long long)iris_sb.max_open_files);
    denied = 1;
  }
  pthread_mutex_unlock(&iris_sandbox_mu);
  if (denied)
    iris_runtime_error("E0407", "%s", err);
}

static void iris_sandbox_track_file_open(FILE *f, const char *path,
                                         int is_write) {
  if (!f)
    return;
  pthread_mutex_lock(&iris_sandbox_mu);
  if (iris_sb.tracked_files_count == iris_sb.tracked_files_cap) {
    size_t next =
        iris_sb.tracked_files_cap ? iris_sb.tracked_files_cap * 2 : 32;
    IrisSandboxOpenFile *grown = (IrisSandboxOpenFile *)realloc(
        iris_sb.tracked_files, next * sizeof(IrisSandboxOpenFile));
    if (grown) {
      iris_sb.tracked_files = grown;
      iris_sb.tracked_files_cap = next;
    }
  }
  if (iris_sb.tracked_files_count < iris_sb.tracked_files_cap) {
    iris_sb.tracked_files[iris_sb.tracked_files_count].handle = (void *)f;
    iris_sb.tracked_files[iris_sb.tracked_files_count].path =
        sb_strdup_raw(path);
    iris_sb.tracked_files[iris_sb.tracked_files_count].is_write = is_write;
    iris_sb.tracked_files_count++;
  }
  iris_sb.open_files++;
  pthread_mutex_unlock(&iris_sandbox_mu);
}

static void iris_sandbox_track_file_close(FILE *f) {
  if (!f)
    return;
  pthread_mutex_lock(&iris_sandbox_mu);
  for (size_t i = 0; i < iris_sb.tracked_files_count; i++) {
    if (iris_sb.tracked_files[i].handle == (void *)f) {
      free(iris_sb.tracked_files[i].path);
      iris_sb.tracked_files[i] =
          iris_sb.tracked_files[iris_sb.tracked_files_count - 1];
      iris_sb.tracked_files_count--;
      if (iris_sb.open_files > 0)
        iris_sb.open_files--;
      break;
    }
  }
  pthread_mutex_unlock(&iris_sandbox_mu);
}

static void iris_sandbox_require_file_handle_read(FILE *f) {
  char err[512] = {0};
  int denied = 0;
  pthread_mutex_lock(&iris_sandbox_mu);
  sb_ensure_init_locked();
  const char *path = NULL;
  for (size_t i = 0; i < iris_sb.tracked_files_count; i++) {
    if (iris_sb.tracked_files[i].handle == (void *)f) {
      path = iris_sb.tracked_files[i].path;
      break;
    }
  }
  if (path) {
    denied = sb_check_fs_path_locked(path, 0, 0, err, sizeof(err));
  } else if (!iris_sb.allow_fs_read) {
    snprintf(err, sizeof(err), "filesystem read denied");
    denied = 1;
  }
  pthread_mutex_unlock(&iris_sandbox_mu);
  if (denied)
    iris_runtime_error("E0407", "%s", err);
}

static void iris_sandbox_require_file_handle_write(FILE *f, int64_t bytes) {
  char err[512] = {0};
  int denied = 0;
  pthread_mutex_lock(&iris_sandbox_mu);
  sb_ensure_init_locked();
  const char *path = NULL;
  for (size_t i = 0; i < iris_sb.tracked_files_count; i++) {
    if (iris_sb.tracked_files[i].handle == (void *)f) {
      path = iris_sb.tracked_files[i].path;
      break;
    }
  }
  if (path) {
    denied = sb_check_fs_path_locked(path, 1, bytes, err, sizeof(err));
  } else if (!iris_sb.allow_fs_write) {
    snprintf(err, sizeof(err), "filesystem write denied");
    denied = 1;
  } else if (iris_sb.max_file_write_bytes > 0 &&
             bytes > iris_sb.max_file_write_bytes) {
    snprintf(err, sizeof(err),
             "write size %lld exceeds security limit %lld bytes",
             (long long)bytes, (long long)iris_sb.max_file_write_bytes);
    denied = 1;
  }
  pthread_mutex_unlock(&iris_sandbox_mu);
  if (denied)
    iris_runtime_error("E0407", "%s", err);
}

static void iris_sandbox_require_conn_open(const char *host) {
  char err[512] = {0};
  int denied = 0;
  pthread_mutex_lock(&iris_sandbox_mu);
  sb_ensure_init_locked();
  if (!iris_sb.allow_network) {
    snprintf(err, sizeof(err), "network access denied: %s",
             host ? host : "<network>");
    denied = 1;
  } else if (host && *host) {
    if (sb_host_matches_list(&iris_sb.net_block, host) ||
        (iris_sb.net_allow.count > 0 &&
         !sb_host_matches_list(&iris_sb.net_allow, host))) {
      snprintf(err, sizeof(err), "network access denied: %s", host);
      denied = 1;
    }
  }
  if (!denied && iris_sb.max_connections > 0 &&
      iris_sb.open_connections >= iris_sb.max_connections) {
    snprintf(err, sizeof(err), "network connection limit exceeded (%lld)",
             (long long)iris_sb.max_connections);
    denied = 1;
  }
  pthread_mutex_unlock(&iris_sandbox_mu);
  if (denied)
    iris_runtime_error("E0407", "%s", err);
}

static void iris_sandbox_track_conn_open(int64_t fd) {
  if (fd < 0)
    return;
  pthread_mutex_lock(&iris_sandbox_mu);
  if (iris_sb.tracked_conns_count == iris_sb.tracked_conns_cap) {
    size_t next =
        iris_sb.tracked_conns_cap ? iris_sb.tracked_conns_cap * 2 : 32;
    int64_t *grown =
        (int64_t *)realloc(iris_sb.tracked_conns, next * sizeof(int64_t));
    if (grown) {
      iris_sb.tracked_conns = grown;
      iris_sb.tracked_conns_cap = next;
    }
  }
  if (iris_sb.tracked_conns_count < iris_sb.tracked_conns_cap) {
    iris_sb.tracked_conns[iris_sb.tracked_conns_count++] = fd;
  }
  iris_sb.open_connections++;
  pthread_mutex_unlock(&iris_sandbox_mu);
}

static void iris_sandbox_track_conn_close(int64_t fd) {
  pthread_mutex_lock(&iris_sandbox_mu);
  for (size_t i = 0; i < iris_sb.tracked_conns_count; i++) {
    if (iris_sb.tracked_conns[i] == fd) {
      iris_sb.tracked_conns[i] =
          iris_sb.tracked_conns[iris_sb.tracked_conns_count - 1];
      iris_sb.tracked_conns_count--;
      if (iris_sb.open_connections > 0)
        iris_sb.open_connections--;
      break;
    }
  }
  pthread_mutex_unlock(&iris_sandbox_mu);
}

static void iris_sandbox_verify_accepted_peer(int64_t fd) {
#ifdef __IRIS_WASM_STUB
  (void)fd;
#else
  struct sockaddr_storage addr;
  char peer[INET6_ADDRSTRLEN] = {0};
#ifdef _WIN32
  int alen = (int)sizeof(addr);
  if (getpeername((SOCKET)fd, (struct sockaddr *)&addr, &alen) != 0)
    return;
#else
  socklen_t alen = (socklen_t)sizeof(addr);
  if (getpeername((int)fd, (struct sockaddr *)&addr, &alen) != 0)
    return;
#endif
  if (addr.ss_family == AF_INET) {
    const struct sockaddr_in *s4 = (const struct sockaddr_in *)&addr;
    inet_ntop(AF_INET, (void *)&s4->sin_addr, peer, sizeof(peer));
  } else if (addr.ss_family == AF_INET6) {
    const struct sockaddr_in6 *s6 = (const struct sockaddr_in6 *)&addr;
    inet_ntop(AF_INET6, (void *)&s6->sin6_addr, peer, sizeof(peer));
  }
  if (peer[0] && iris_sandbox_check_network(peer) != 0) {
#ifdef _WIN32
    closesocket((SOCKET)fd);
#else
    close((int)fd);
#endif
    iris_runtime_error("E0407", "network peer denied: %s", peer);
  }
#endif
}

/* ---------------------------------------------------------------------------
 * Enum Variant Helpers
 * ---------------------------------------------------------------------------
 */

IrisVal *iris_make_variant(int64_t tag, int32_t nfields, ...) {
  IrisVal *v = (IrisVal *)xmalloc(sizeof(IrisVal));
  v->tag = IRIS_TAG_ENUM;

  if (nfields == 0) {
    v->ptr = NULL;
  } else {
    IrisVal **fields = (IrisVal **)xmalloc(nfields * sizeof(IrisVal *));
    va_list args;
    va_start(args, nfields);
    for (int i = 0; i < nfields; i++) {
      fields[i] = va_arg(args, IrisVal *);
    }
    va_end(args);
    IrisEnum *e = (IrisEnum *)xmalloc(sizeof(IrisEnum));
    e->tag = tag;
    e->fields = fields;
    e->len = (size_t)nfields;
    v->ptr = e;
  }
  return v;
}

int64_t iris_get_variant_tag(IrisVal *v) {
  if (!v)
    return 0;

  // Hybrid enum representation:
  // 1) unit/tag-only enums may be passed as immediate tags cast to pointers
  // 2) payload enums are boxed IRIS_TAG_ENUM values
  uintptr_t raw = (uintptr_t)v;
  if (raw <= (uintptr_t)0xFFFF) {
    return (int64_t)raw;
  }

  if (v->tag == IRIS_TAG_ENUM) {
    IrisEnum *e = (IrisEnum *)v->ptr;
    return e ? e->tag : 0;
  }

  // Boxed integer fallback for tag-only values that traveled through
  // containers.
  if (v->tag == IRIS_TAG_I64) {
    return v->i64;
  }
  if (v->tag == IRIS_TAG_I32) {
    return (int64_t)v->i32;
  }

  return 0;
}

IrisVal *iris_extract_variant_field(IrisVal *v, int64_t field_idx) {
  if (!v)
    return NULL;

  // Immediate tag-only variants carry no payload fields.
  if ((uintptr_t)v <= (uintptr_t)0xFFFF)
    return NULL;

  if (v->tag != IRIS_TAG_ENUM || !v->ptr)
    return NULL;
  IrisEnum *e = (IrisEnum *)v->ptr;
  if ((size_t)field_idx >= e->len)
    return NULL;
  return e->fields[field_idx];
}

// ── Adaptive AI runtime (std.adaptive) ────────────────────────────────────

struct IrisAdaptiveState {
  char *name;
  double *params;
  int64_t n_params;
  double learning_rate;
  double risk_threshold;
  double retrain_threshold;
  int64_t min_obs_retrain;
  int64_t obs_count;
  double mean_err;
  double m2;
  /* Largest |error| seen. Previously never tracked, so `risk_max_error`
   * reported the *mean* instead -- a strictly smaller number, in the module
   * whose job is deciding when it is safe to act. See known-issues #34. */
  double max_err;
  /* Observations whose |error| exceeded `risk_threshold`. Previously
   * `risk_errors` reported the observation count instead. */
  int64_t err_count;
  double last_prediction;
  int initialized;
};

// ── Internal implementations (IrisAdaptiveState*) ──────────────────────────

IrisAdaptiveState *iris_adaptive_new_impl(const char *name, int64_t n_params,
                                          double learning_rate,
                                          double risk_threshold) {
  IrisAdaptiveState *s =
      (IrisAdaptiveState *)xmalloc(sizeof(IrisAdaptiveState));
  s->name = name ? xstrdup(name) : xstrdup("adaptive");
  s->n_params = n_params;
  s->params =
      (double *)xcalloc((size_t)(n_params > 0 ? n_params : 1), sizeof(double));
  s->learning_rate = learning_rate > 0.0 ? learning_rate : 0.01;
  s->risk_threshold = risk_threshold > 0.0 ? risk_threshold : 2.0;
  s->retrain_threshold = 0.05;
  s->min_obs_retrain = 10;
  s->obs_count = 0;
  s->mean_err = 0.0;
  s->m2 = 0.0;
  s->max_err = 0.0;
  s->err_count = 0;
  s->last_prediction = 0.0;
  s->initialized = 1;
  return s;
}

void iris_adaptive_free_impl(IrisAdaptiveState *state) {
  if (!state)
    return;
  if (state->name)
  xfree(state->name);
  if (state->params)
  xfree(state->params);
  xfree(state);
}

const char *iris_adaptive_name_impl(IrisAdaptiveState *state) {
  return state ? state->name : "";
}

double iris_adaptive_get_param_impl(IrisAdaptiveState *state, int64_t idx) {
  if (!state || idx < 0 || idx >= state->n_params)
    return 0.0;
  return state->params[idx];
}

void iris_adaptive_set_param_impl(IrisAdaptiveState *state, int64_t idx,
                                  double value) {
  if (!state || idx < 0 || idx >= state->n_params)
    return;
  state->params[idx] = value;
}

int64_t iris_adaptive_n_params_impl(IrisAdaptiveState *state) {
  return state ? state->n_params : 0;
}

double iris_adaptive_learning_rate_impl(IrisAdaptiveState *state) {
  return state ? state->learning_rate : 0.0;
}

void iris_adaptive_set_learning_rate_impl(IrisAdaptiveState *state, double lr) {
  if (state)
    state->learning_rate = lr > 0.0 ? lr : 0.001;
}

void iris_adaptive_observe_impl(IrisAdaptiveState *state, const double *inputs,
                                int64_t n_inputs, double target) {
  if (!state || !inputs || n_inputs <= 0)
    return;
  double pred = iris_adaptive_predict_impl(state, inputs, n_inputs);
  double error = pred - target;
  state->last_prediction = pred;
  iris_adaptive_record_error_impl(state, error);
  double lr = state->learning_rate;
  for (int64_t i = 0; i < n_inputs && i < state->n_params; i++) {
    state->params[i] -= lr * error * inputs[i];
  }
}

double iris_adaptive_predict_impl(IrisAdaptiveState *state,
                                  const double *inputs, int64_t n_inputs) {
  if (!state || !inputs)
    return 0.0;
  double pred = 0.0;
  for (int64_t i = 0; i < n_inputs && i < state->n_params; i++) {
    pred += state->params[i] * inputs[i];
  }
  return pred;
}

double iris_adaptive_train_batch_impl(IrisAdaptiveState *state,
                                      const double *inputs, int64_t n_samples,
                                      int64_t n_features,
                                      const double *targets) {
  if (!state || !inputs || !targets || n_samples <= 0)
    return 0.0;
  double total_loss = 0.0;
  for (int64_t b = 0; b < n_samples; b++) {
    const double *row = inputs + b * n_features;
    double pred = iris_adaptive_predict_impl(state, row, n_features);
    double error = pred - targets[b];
    total_loss += error * error;
    double lr = state->learning_rate;
    for (int64_t i = 0; i < n_features && i < state->n_params; i++) {
      state->params[i] -= lr * error * row[i];
    }
  }
  state->last_prediction = total_loss / (double)n_samples;
  return state->last_prediction;
}

void iris_adaptive_record_error_impl(IrisAdaptiveState *state, double error) {
  if (!state)
    return;
  state->obs_count++;
  double delta = error - state->mean_err;
  state->mean_err += delta / (double)state->obs_count;
  double delta2 = error - state->mean_err;
  state->m2 += delta * delta2;
  /* Track what `risk_max_error` and `risk_errors` claim to report. */
  double abs_err = fabs(error);
  if (abs_err > state->max_err)
    state->max_err = abs_err;
  if (abs_err > state->risk_threshold)
    state->err_count++;
}

IrisRiskMetrics iris_adaptive_get_risk_impl(IrisAdaptiveState *state) {
  IrisRiskMetrics m;
  m.mean_error = state ? state->mean_err : 0.0;
  m.max_error = state ? state->max_err : 0.0;
  m.observations = state ? state->obs_count : 0;
  m.errors = state ? state->err_count : 0;
  m.last_risk = 0.0;
  m.confidence = 1.0;
  if (state && state->obs_count > 1) {
    double variance = state->m2 / (double)(state->obs_count - 1);
    double std_dev = sqrt(variance);
    double risk_score = std_dev + fabs(state->mean_err);
    m.last_risk = risk_score > 1.0 ? 1.0 : risk_score;
    m.confidence = std_dev > 0.0 ? exp(-fabs(state->mean_err) / std_dev) : 1.0;
    if (m.confidence > 1.0)
      m.confidence = 1.0;
  }
  return m;
}

int iris_adaptive_is_unsafe_impl(IrisAdaptiveState *state) {
  if (!state)
    return 0;
  IrisRiskMetrics m = iris_adaptive_get_risk_impl(state);
  /* This used a hardcoded 0.5 and ignored `risk_threshold` entirely, so
   * `adaptive_set_risk_threshold` had no effect on the only guard anyone
   * calls: a caller set a threshold, saw a guard, and got a guard answering a
   * different question. `last_risk` is clamped to 1.0, so a threshold above
   * that would make the risk arm unreachable -- it is clamped to the same
   * range the score lives in. See known-issues #34. */
  double threshold = state->risk_threshold;
  if (threshold <= 0.0)
    threshold = 0.5;
  if (threshold > 1.0)
    threshold = 1.0;
  return m.last_risk > threshold || m.confidence < 0.3;
}

void iris_adaptive_set_risk_threshold_impl(IrisAdaptiveState *state,
                                           double threshold) {
  if (state)
    state->risk_threshold = threshold > 0.0 ? threshold : 1.0;
}

IrisUncertainty iris_adaptive_predict_with_uncertainty_impl(
    IrisAdaptiveState *state, const double *inputs, int64_t n_inputs) {
  IrisUncertainty u;
  u.mean = iris_adaptive_predict_impl(state, inputs, n_inputs);
  u.variance = 1.0;
  u.lower_95 = u.mean - 1.96;
  u.upper_95 = u.mean + 1.96;
  u.confidence = 0.5;
  if (state && state->obs_count > 1) {
    u.variance = state->m2 / (double)(state->obs_count - 1);
    double std_dev = sqrt(u.variance);
    u.lower_95 = u.mean - 1.96 * std_dev;
    u.upper_95 = u.mean + 1.96 * std_dev;
    u.confidence = std_dev > 0.0 ? exp(-fabs(u.mean) / std_dev) : 0.5;
    if (u.confidence > 1.0)
      u.confidence = 1.0;
  }
  return u;
}

double iris_adaptive_uncertainty_bayes_update_impl(IrisAdaptiveState *state,
                                                   double prior_mean,
                                                   double prior_var,
                                                   double observation,
                                                   double obs_var) {
  if (!state)
    return 0.0;
  /*
   * Normal-normal conjugate update, treating `observation` as one new datum
   * on top of the state's accumulated error statistics.
   *
   * This previously computed a posterior from the accumulated data only and
   * then returned `posterior_mean + observation`. That is not a posterior:
   * with no accumulated data it returned `prior_mean + observation`, where a
   * Bayesian update with no data must return the prior mean, and the result
   * grew without bound as observations arrived. The `observation` argument
   * was never actually folded into the update. Since this value is what an
   * adaptive system uses to decide whether it is confident enough to act, a
   * posterior that is not a posterior is a live hazard. See known-issues #30.
   */
  if (prior_var <= 0.0)
    prior_var = 1.0;
  if (obs_var <= 0.0)
    obs_var = 1.0;
  double n = (double)state->obs_count;
  double posterior_precision = 1.0 / prior_var + (n + 1.0) / obs_var;
  if (posterior_precision <= 0.0)
    posterior_precision = 1.0;
  double data_sum = n * state->mean_err + observation;
  return (prior_mean / prior_var + data_sum / obs_var) / posterior_precision;
}

int iris_adaptive_should_retrain_impl(IrisAdaptiveState *state) {
  if (!state)
    return 0;
  if (state->obs_count < state->min_obs_retrain)
    return 0;
  return fabs(state->mean_err) > state->retrain_threshold;
}

double iris_adaptive_auto_retrain_impl(IrisAdaptiveState *state,
                                       const double *inputs, int64_t n_samples,
                                       int64_t n_features,
                                       const double *targets) {
  if (!state)
    return 0.0;
  if (!iris_adaptive_should_retrain_impl(state)) {
    return state->mean_err;
  }
  double loss = iris_adaptive_train_batch_impl(state, inputs, n_samples,
                                               n_features, targets);
  iris_adaptive_reset_stats_impl(state);
  return loss;
}

void iris_adaptive_set_retrain_threshold_impl(IrisAdaptiveState *state,
                                              double threshold) {
  if (state)
    state->retrain_threshold = threshold > 0.0 ? threshold : 0.01;
}

void iris_adaptive_set_min_observations_for_retrain_impl(
    IrisAdaptiveState *state, int64_t n) {
  if (state)
    state->min_obs_retrain = n > 0 ? n : 1;
}

void iris_adaptive_adapt_threshold_impl(IrisAdaptiveState *state,
                                        double observed_error) {
  if (!state)
    return;
  double current = state->risk_threshold;
  double alpha = 0.1;
  double abs_err = fabs(observed_error);
  if (abs_err > current) {
    state->risk_threshold = current + alpha * (abs_err - current);
  } else {
    state->risk_threshold = current - alpha * (current - abs_err) * 0.5;
  }
  if (state->risk_threshold < 0.1)
    state->risk_threshold = 0.1;
  double error_rate = fabs(state->mean_err);
  if (state->obs_count > 0 && error_rate > 0.05) {
    state->learning_rate *= 1.01;
    if (state->learning_rate > 1.0)
      state->learning_rate = 1.0;
  } else if (state->obs_count > 10 && error_rate < 0.001) {
    state->learning_rate *= 0.99;
    if (state->learning_rate < 0.0001)
      state->learning_rate = 0.0001;
  }
}

double iris_adaptive_current_threshold_impl(IrisAdaptiveState *state) {
  return state ? state->risk_threshold : 0.0;
}

int64_t iris_adaptive_observation_count_impl(IrisAdaptiveState *state) {
  return state ? state->obs_count : 0;
}

double iris_adaptive_mean_error_impl(IrisAdaptiveState *state) {
  return state ? state->mean_err : 0.0;
}

void iris_adaptive_reset_stats_impl(IrisAdaptiveState *state) {
  if (!state)
    return;
  state->obs_count = 0;
  state->mean_err = 0.0;
  state->m2 = 0.0;
}

// ── Extern-compatible wrappers (int64_t handle) ───────────────────────────
static IrisAdaptiveState *ad_h(int64_t h) {
  return (IrisAdaptiveState *)(intptr_t)h;
}

int64_t iris_adaptive_new(const char *name, int64_t n_params,
                          double learning_rate, double risk_threshold) {
  return (int64_t)(intptr_t)iris_adaptive_new_impl(
      name, n_params, learning_rate, risk_threshold);
}
int64_t iris_adaptive_free(int64_t handle) {
  iris_adaptive_free_impl(ad_h(handle));
  return 0;
}
const char *iris_adaptive_name(int64_t handle) {
  IrisAdaptiveState *state = ad_h(handle);
  if (state && state->name)
    return xstrdup(state->name);
  return xstrdup("");
}
double iris_adaptive_get_param(int64_t handle, int64_t idx) {
  return iris_adaptive_get_param_impl(ad_h(handle), idx);
}
int64_t iris_adaptive_set_param(int64_t handle, int64_t idx, double value) {
  iris_adaptive_set_param_impl(ad_h(handle), idx, value);
  return 0;
}
int64_t iris_adaptive_n_params(int64_t handle) {
  return iris_adaptive_n_params_impl(ad_h(handle));
}
double iris_adaptive_learning_rate(int64_t handle) {
  return iris_adaptive_learning_rate_impl(ad_h(handle));
}
int64_t iris_adaptive_set_learning_rate(int64_t handle, double lr) {
  iris_adaptive_set_learning_rate_impl(ad_h(handle), lr);
  return 0;
}
int64_t iris_adaptive_record_error(int64_t handle, double error) {
  iris_adaptive_record_error_impl(ad_h(handle), error);
  return 0;
}
int iris_adaptive_is_unsafe(int64_t handle) {
  return iris_adaptive_is_unsafe_impl(ad_h(handle));
}
int64_t iris_adaptive_set_risk_threshold(int64_t handle, double threshold) {
  iris_adaptive_set_risk_threshold_impl(ad_h(handle), threshold);
  return 0;
}
int iris_adaptive_should_retrain(int64_t handle) {
  return iris_adaptive_should_retrain_impl(ad_h(handle));
}
int64_t iris_adaptive_set_retrain_threshold(int64_t handle, double threshold) {
  iris_adaptive_set_retrain_threshold_impl(ad_h(handle), threshold);
  return 0;
}
int64_t iris_adaptive_set_min_observations_for_retrain(int64_t handle,
                                                       int64_t n) {
  iris_adaptive_set_min_observations_for_retrain_impl(ad_h(handle), n);
  return 0;
}
int64_t iris_adaptive_adapt_threshold(int64_t handle, double observed_error) {
  iris_adaptive_adapt_threshold_impl(ad_h(handle), observed_error);
  return 0;
}
double iris_adaptive_current_threshold(int64_t handle) {
  return iris_adaptive_current_threshold_impl(ad_h(handle));
}
int64_t iris_adaptive_observation_count(int64_t handle) {
  return iris_adaptive_observation_count_impl(ad_h(handle));
}
double iris_adaptive_mean_error(int64_t handle) {
  return iris_adaptive_mean_error_impl(ad_h(handle));
}
/* Accessors for the metrics `adaptive_get_risk` claims to report. Without
 * these the stdlib had nothing real to read and substituted the mean for the
 * max and the observation count for the error count. See known-issues #34. */
double iris_adaptive_max_error(int64_t handle) {
  IrisAdaptiveState *s = ad_h(handle);
  return s ? s->max_err : 0.0;
}
int64_t iris_adaptive_error_count(int64_t handle) {
  IrisAdaptiveState *s = ad_h(handle);
  return s ? s->err_count : 0;
}
double iris_adaptive_last_risk(int64_t handle) {
  IrisAdaptiveState *s = ad_h(handle);
  if (!s)
    return 0.0;
  return iris_adaptive_get_risk_impl(s).last_risk;
}
double iris_adaptive_confidence(int64_t handle) {
  IrisAdaptiveState *s = ad_h(handle);
  if (!s)
    return 1.0;
  return iris_adaptive_get_risk_impl(s).confidence;
}
int64_t iris_adaptive_reset_stats(int64_t handle) {
  iris_adaptive_reset_stats_impl(ad_h(handle));
  return 0;
}
double iris_adaptive_uncertainty_bayes_update(int64_t handle, double prior_mean,
                                              double prior_var,
                                              double observation,
                                              double obs_var) {
  return iris_adaptive_uncertainty_bayes_update_impl(
      ad_h(handle), prior_mean, prior_var, observation, obs_var);
}

/* `iris_sandbox_check_ffi` is implemented in the unified sandbox section above.
 */

// ---------------------------------------------------------------------------
// Weak References, GC Stats, and Concurrency Timeouts
// ---------------------------------------------------------------------------

IrisWeakRef *iris_weak_ref_new(void *target) {
  IrisWeakRef *w = (IrisWeakRef *)xmalloc(sizeof(IrisWeakRef));
  w->target = target;
  w->next_weak = NULL;
  if (!target)
    return w;
  pthread_mutex_lock(&rc_global_mu);
  RcEntry *e = rc_find(target);
  if (e) {
    w->next_weak = e->weak_refs;
    e->weak_refs = w;
  } else {
    w->target = NULL;
  }
  pthread_mutex_unlock(&rc_global_mu);
  return w;
}

IrisOption *iris_weak_ref_upgrade(IrisWeakRef *w) {
  if (!w || !w->target)
    return iris_make_none();
  pthread_mutex_lock(&rc_global_mu);
  RcEntry *e = rc_find(w->target);
  if (e && e->count > 0) {
    e->count++;
    e->color = RC_COLOR_BLACK;
    void *ptr = w->target;
    pthread_mutex_unlock(&rc_global_mu);
    return iris_make_some((IrisVal *)ptr);
  }
  w->target = NULL;
  pthread_mutex_unlock(&rc_global_mu);
  return iris_make_none();
}

int32_t iris_weak_ref_alive(IrisWeakRef *w) {
  if (!w || !w->target)
    return 0;
  pthread_mutex_lock(&rc_global_mu);
  RcEntry *e = rc_find(w->target);
  int32_t alive = (e && e->count > 0) ? 1 : 0;
  if (!alive)
    w->target = NULL;
  pthread_mutex_unlock(&rc_global_mu);
  return alive;
}

void iris_gc_stats(int64_t *out_alloc, int64_t *out_freed, int64_t *out_cycles,
                   int64_t *out_weak_inval) {
  pthread_mutex_lock(&rc_global_mu);
  if (out_alloc)
    *out_alloc = gc_total_allocated;
  if (out_freed)
    *out_freed = gc_total_freed;
  if (out_cycles)
    *out_cycles = gc_cycles_collected;
  if (out_weak_inval)
    *out_weak_inval = gc_weak_refs_invalidated;
  pthread_mutex_unlock(&rc_global_mu);
}

IrisOption *iris_chan_recv_timeout(IrisChannel *c, int64_t timeout_ms) {
  if (!c || timeout_ms <= 0)
    return iris_chan_try_recv(c);
  pthread_mutex_lock(&c->mu);
#if defined(__wasm__)
  /* Single-threaded: nothing can send while this waits, so an empty
   * channel stays empty for the whole timeout. */
  if (c->count == 0) {
    pthread_mutex_unlock(&c->mu);
    iris_sleep_ms(timeout_ms);
    return iris_make_none();
  }
#else
  if (c->count == 0) {
    struct timespec ts;
#if defined(_WIN32) || defined(_WIN64)
    SYSTEMTIME st;
    GetSystemTime(&st);
    FILETIME ft;
    SystemTimeToFileTime(&st, &ft);
    ULARGE_INTEGER uli;
    uli.LowPart = ft.dwLowDateTime;
    uli.HighPart = ft.dwHighDateTime;
    /* FILETIME is measured from 1601; pthread absolute timeouts use the
     * Unix epoch. Without this subtraction a millisecond timeout waits
     * roughly 369 years. */
    uint64_t unix_100ns = uli.QuadPart - 116444736000000000ULL;
    uint64_t ns_total = unix_100ns * 100 + (uint64_t)timeout_ms * 1000000;
    ts.tv_sec = (time_t)(ns_total / 1000000000);
    ts.tv_nsec = (long)(ns_total % 1000000000);
#else
    clock_gettime(CLOCK_REALTIME, &ts);
    int64_t ns = ts.tv_nsec + (timeout_ms % 1000) * 1000000;
    ts.tv_sec += (timeout_ms / 1000) + (ns / 1000000000);
    ts.tv_nsec = ns % 1000000000;
#endif
    while (c->count == 0) {
      int res = pthread_cond_timedwait(&c->not_empty, &c->mu, &ts);
      if (res != 0)
        break;
    }
  }
#endif
  if (c->count == 0) {
    pthread_mutex_unlock(&c->mu);
    return iris_make_none();
  }
  IrisVal *val = c->buf[c->head];
  c->head = (c->head + 1) % c->cap;
  c->count--;
  if (c->max_cap >= 0)
    pthread_cond_signal(&c->not_full);
  pthread_mutex_unlock(&c->mu);
  IrisOption *opt = iris_make_some(val);
  if (val)
    iris_release(val);
  return opt;
}

int32_t iris_task_group_join_timeout(IrisTaskGroup *tg, int64_t timeout_ms) {
  (void)timeout_ms;
  if (!tg)
    return 1;
  iris_task_group_join(tg);
  return 1;
}

// ---------------------------------------------------------------------------
// New builtins (called via generic BuiltinCall codegen path)
// ---------------------------------------------------------------------------

int64_t iris_list_remove(IrisList *l, int64_t idx) {
  if (idx < 0 || (size_t)idx >= l->len) {
    iris_runtime_error("E0403",
                       "index %" PRId64 " is out of bounds for length %zu",
                       (int64_t)idx, l->len);
    iris_fatal();
  }
  iris_tx_record_list(l);
  IrisVal *removed = l->data[idx];
  for (size_t j = (size_t)idx; j + 1 < l->len; j++)
    l->data[j] = l->data[j + 1];
  l->len--;
  return removed->i64;
}

int64_t iris_list_insert(IrisList *l, int64_t idx, IrisVal *val) {
  if (idx < 0 || (size_t)idx > l->len) {
    iris_runtime_error("E0403",
                       "index %" PRId64 " is out of bounds for length %zu",
                       (int64_t)idx, l->len);
    iris_fatal();
  }
  iris_tx_record_list(l);
  if (l->len == l->cap) {
    l->cap *= 2;
    l->data = xrealloc(l->data, sizeof(IrisVal *) * l->cap);
  }
  for (size_t j = l->len; j > (size_t)idx; j--)
    l->data[j] = l->data[j - 1];
  if (iris_needs_arena_copy(val)) {
    IrisVal *slot = iris_list_arena_slot(l);
    *slot = *val;
    val = slot;
  } else if (val) {
    iris_retain(val);
  }
  l->data[idx] = val;
  l->len++;
  return 0;
}

IrisVal *iris_map_entries(IrisVal *map_val) {
  IrisMap *m = iris_unbox_map(map_val);
  IrisList *entries = iris_list_new();
  size_t n;
  IrisMapEntry **sorted = map_sorted_entries(m, &n);
  for (size_t i = 0; i < n; i++) {
    IrisVal *key = iris_box_str(sorted[i]->key);
    IrisVal *pair = iris_make_struct(2, key, sorted[i]->val);
    iris_list_push(entries, pair);
  }
  free(sorted);
  return iris_box_list(entries);
}

IrisVal *iris_recv_timeout(IrisChannel *chan, int64_t timeout_ms) {
  IrisOption *opt = iris_chan_recv_timeout(chan, timeout_ms);
  return (IrisVal *)opt;
}

void iris_chan_send_b(IrisVal *chan_val, IrisVal *val) {
  IrisChannel *c = iris_unbox_chan(chan_val);
  iris_chan_send(c, val);
}

IrisWeakRef *iris_weak_ref(IrisVal *val) { return iris_weak_ref_new(val); }

int32_t iris_weak_alive(IrisWeakRef *w) { return iris_weak_ref_alive(w); }

IrisVal *iris_weak_upgrade(IrisWeakRef *w) {
  IrisOption *opt = iris_weak_ref_upgrade(w);
  return (IrisVal *)opt;
}

/* The map itself, not a box: generated code holds a `map<K, V>` as a raw
 * IrisMap*, so a boxed return made `map_len(gc_stats())` read the box as a
 * map and answer 0. */
IrisMap *iris_gc_stats_map(void) {
  int64_t alloc = 0, freed = 0, cycles = 0, weak_inv = 0;
  iris_gc_stats(&alloc, &freed, &cycles, &weak_inv);
  IrisMap *m = iris_map_new();
  iris_map_set(m, iris_box_str("allocated"), iris_box_i64(alloc));
  iris_map_set(m, iris_box_str("freed"), iris_box_i64(freed));
  iris_map_set(m, iris_box_str("cycles"), iris_box_i64(cycles));
  iris_map_set(m, iris_box_str("weak_invalidated"), iris_box_i64(weak_inv));
  // Long-form aliases used by older interpreter-only programs.
  iris_map_set(m, iris_box_str("cycles_collected"), iris_box_i64(cycles));
  iris_map_set(m, iris_box_str("weak_refs_invalidated"), iris_box_i64(weak_inv));

  pthread_mutex_lock(&ds_mu);
  int64_t raw_allocs = g_raw_allocs;
  int64_t raw_frees = g_raw_frees;
  int64_t raw_live = g_raw_bytes_live;
  int64_t raw_peak = g_raw_bytes_peak;
  pthread_mutex_unlock(&ds_mu);
  iris_map_set(m, iris_box_str("raw_allocs"), iris_box_i64(raw_allocs));
  iris_map_set(m, iris_box_str("raw_frees"), iris_box_i64(raw_frees));
  iris_map_set(m, iris_box_str("raw_bytes_live"), iris_box_i64(raw_live));
  iris_map_set(m, iris_box_str("raw_bytes_peak"), iris_box_i64(raw_peak));

  IrisUiPool *ap = iris_active_ui_pool;
  int64_t pool_live = 0;
  int64_t pool_peak = 0;
  if (ap) {
    pthread_mutex_lock(&ap->mu);
    pool_live = ap->bytes_live;
    pool_peak = ap->bytes_peak;
    pthread_mutex_unlock(&ap->mu);
  }
  iris_map_set(m, iris_box_str("pool_bytes_live"), iris_box_i64(pool_live));
  iris_map_set(m, iris_box_str("pool_bytes_peak"), iris_box_i64(pool_peak));

  /* Per-site keys are opt-in: they vary with the program's allocation sites,
   * while gc_stats() is also used as a stable cross-backend diagnostic. */
  if (getenv("IRIS_ALLOC_SITE_STATS")) {
    pthread_mutex_lock(&alloc_sites_mu);
    for (int i = 0; i < ALLOC_SITE_SLOTS; i++) {
      if (alloc_sites[i].count <= 0)
        continue;
      char key[48];
      snprintf(key, sizeof(key), "site:%lld", (long long)alloc_sites[i].site);
      iris_map_set(m, iris_box_str(key), iris_box_i64(alloc_sites[i].count));
    }
    pthread_mutex_unlock(&alloc_sites_mu);
  }
  return m;
}

int32_t iris_gc_collect_call(void) {
  iris_gc_collect();
  return 0;
}

/* ------------------------------------------------------------------------- */
/* Dynamic reflection                                                       */
/* ------------------------------------------------------------------------- */

/* Full source reflection is implemented by the Rust IR interpreter, which
 * owns the parser, borrow checker, effect checker and IR verifier. A native
 * program currently links only this compact C runtime, not the compiler.
 *
 * The previous native implementation guessed at one integer operator or
 * concatenated every quoted substring, returned Ok for invalid programs, and
 * therefore made reflect_validate execute unvalidated input while claiming it
 * had been type checked. An explicit capability failure is safe and honest;
 * silently evaluating a different language is not. */
int32_t iris_reflection_available(void) { return 0; }

int32_t iris_validate(const char *source) {
  (void)source;
  return 0;
}

static IrisResult *iris_reflection_unavailable(void) {
  return iris_make_err(
      iris_box_str("source reflection is unavailable in standalone native "
                   "binaries; run with IRIS_FORCE_INTERP=1"));
}

IrisResult *iris_eval(const char *source) {
  (void)source;
  return iris_reflection_unavailable();
}

IrisResult *iris_eval_i64(const char *source) {
  (void)source;
  return iris_reflection_unavailable();
}

IrisResult *iris_meta_analyze(const char *source) {
  (void)source;
  return iris_reflection_unavailable();
}

IrisResult *iris_meta_emit_ir(const char *source) {
  (void)source;
  return iris_reflection_unavailable();
}

IrisResult *iris_meta_apply_edit(const char *source, int64_t start_byte,
                                 int64_t end_byte, const char *replacement) {
  (void)source;
  (void)start_byte;
  (void)end_byte;
  (void)replacement;
  return iris_reflection_unavailable();
}

/* ===========================================================================
 * Cooperative task executor: the native half of `async def` / `await`.
 *
 * Mirrors src/interp/async_exec.rs. `async def f` compiles to a constructor
 * that creates a task and schedules it, plus a poll function that runs the
 * body until it completes or reaches an `await` on an unfinished task (the
 * state-machine transformation in src/pass/async_lower.rs). Values that live
 * across an `await` are kept in the task's frame. A slot either owns a boxed
 * value (a constructor argument or a result; dropped when overwritten or when
 * the task completes), holds a raw pointer whose lifetime is managed by the
 * program's own retain/release, or owns a raw spill buffer holding a value's
 * bits exactly as a register would.
 *
 * The executor is per thread: a task belongs to the thread that created it.
 * Tasks are not freed, since their results may still be read through a handle.
 * ===========================================================================
 */

typedef struct IrisTask IrisTask;
typedef int64_t (*IrisPollFn)(IrisTask *);

enum {
  IRIS_TASK_POLL = 0,
  IRIS_TASK_SLEEP = 1,
  IRIS_TASK_RECV = 2,
  IRIS_TASK_YIELD = 3,
  /* Completes with the index of the first of its inputs to finish. */
  IRIS_TASK_ANY = 4,
  /* Completes when a socket can be read without blocking (data, a pending
   * connection on a listener, end of stream, or an error). */
  IRIS_TASK_READABLE = 5,
  /* Completes when a socket can be written without blocking. */
  IRIS_TASK_WRITABLE = 6,
};

enum {
  IRIS_SLOT_BORROWED = 0,
  IRIS_SLOT_BOXED = 1,
  IRIS_SLOT_RAW = 2,
};

typedef struct {
  void *value;
  int32_t owned;
} IrisTaskSlot;

/* A task is freed when its reference count reaches zero. References: the
 * program's handle (taken at creation; dropped by iris_async_release, which
 * the compiler emits once a task it created has been awaited or handed to
 * async_any and is used nowhere else), plus one while it is queued, one per
 * waiters-list entry, one while it is a pending timer or receive, and one per
 * any-task it is an input of. A handle the program keeps elsewhere (a list, a
 * record, a second await) is never released, so that task is never freed. */
struct IrisTask {
  IrisPollFn poll;
  IrisTaskSlot *slots;
  int64_t nslots;
  int64_t state;
  int done;
  int scheduled;
  void *result;
  int32_t result_owned;
  IrisTask **waiters;
  int64_t nwaiters;
  int64_t capwaiters;
  int kind;
  int cancelled;
  int64_t deadline_ms;
  int64_t sock;
  IrisChannel *chan;
  IrisTask **inputs;
  int64_t ninputs;
  int64_t capinputs;
  int64_t refs;
  IrisTask *next;
};

static IRIS_THREAD_LOCAL IrisTask *iris_exec_head = NULL;
static IRIS_THREAD_LOCAL IrisTask *iris_exec_tail = NULL;
static IRIS_THREAD_LOCAL IrisTask **iris_exec_leaves = NULL;
static IRIS_THREAD_LOCAL int64_t iris_exec_nleaves = 0;
static IRIS_THREAD_LOCAL int64_t iris_exec_capleaves = 0;

static int64_t iris_monotonic_ms(void) {
#ifdef _WIN32
  /* GetTickCount64 advances in ~16 ms steps; timers need millisecond order. */
  static LARGE_INTEGER frequency;
  LARGE_INTEGER now;
  if (frequency.QuadPart == 0)
    QueryPerformanceFrequency(&frequency);
  QueryPerformanceCounter(&now);
  int64_t per_ms = (int64_t)(frequency.QuadPart / 1000);
  return (int64_t)now.QuadPart / (per_ms > 0 ? per_ms : 1);
#else
  struct timespec ts;
  clock_gettime(CLOCK_MONOTONIC, &ts);
  return (int64_t)ts.tv_sec * 1000 + (int64_t)(ts.tv_nsec / 1000000);
#endif
}

static IrisTask *iris_task_alloc(IrisPollFn poll, int kind) {
  IrisTask *t = (IrisTask *)calloc(1, sizeof(IrisTask));
  if (!t) {
    iris_panic("out of memory creating a task");
    return NULL;
  }
  t->poll = poll;
  t->kind = kind;
  t->refs = 1; /* the program's handle */
  return t;
}

void *iris_async_task_new(void *poll) {
  return iris_task_alloc((IrisPollFn)poll, IRIS_TASK_POLL);
}

static void iris_task_slot_clear(IrisTaskSlot *slot) {
  if (slot->value) {
    if (slot->owned == IRIS_SLOT_BOXED)
      iris_drop_box((IrisVal *)slot->value);
    else if (slot->owned == IRIS_SLOT_RAW)
      free(slot->value);
  }
  slot->value = NULL;
  slot->owned = IRIS_SLOT_BORROWED;
}

static void iris_task_ref(IrisTask *t) {
  if (t)
    t->refs++;
}

static void iris_task_unref(IrisTask *t);

/* Drop a task's frame, waiter and input references. Leaves `t` itself. */
static void iris_task_drop_contents(IrisTask *t) {
  for (int64_t i = 0; i < t->nslots; i++)
    iris_task_slot_clear(&t->slots[i]);
  free(t->slots);
  t->slots = NULL;
  t->nslots = 0;
  IrisTask **inputs = t->inputs;
  int64_t ninputs = t->ninputs;
  t->inputs = NULL;
  t->ninputs = 0;
  t->capinputs = 0;
  for (int64_t i = 0; i < ninputs; i++)
    iris_task_unref(inputs[i]);
  free(inputs);
}

static void iris_task_unref(IrisTask *t) {
  if (!t || --t->refs > 0)
    return;
  iris_task_drop_contents(t);
  IrisTask **waiters = t->waiters;
  int64_t nwaiters = t->nwaiters;
  t->waiters = NULL;
  t->nwaiters = 0;
  for (int64_t i = 0; i < nwaiters; i++)
    iris_task_unref(waiters[i]);
  free(waiters);
  if (t->result_owned && t->result)
    iris_drop_box((IrisVal *)t->result);
  free(t);
}

/* The program no longer uses this handle (emitted by the compiler). */
void iris_async_release(void *task) { iris_task_unref((IrisTask *)task); }

/* Grow the frame so `slot` exists. Returns 0 on failure. */
static int iris_task_reserve(IrisTask *t, int64_t slot) {
  if (slot < t->nslots)
    return 1;
  int64_t n = t->nslots ? t->nslots * 2 : 8;
  while (n <= slot)
    n *= 2;
  IrisTaskSlot *grown =
      (IrisTaskSlot *)realloc(t->slots, (size_t)n * sizeof(IrisTaskSlot));
  if (!grown) {
    iris_panic("out of memory growing a task frame");
    return 0;
  }
  memset(grown + t->nslots, 0, (size_t)(n - t->nslots) * sizeof(IrisTaskSlot));
  t->slots = grown;
  t->nslots = n;
  return 1;
}

void iris_async_frame_set(void *task, int64_t slot, void *value,
                          int32_t owned) {
  IrisTask *t = (IrisTask *)task;
  if (!t || slot < 0 || !iris_task_reserve(t, slot))
    return;
  iris_task_slot_clear(&t->slots[slot]);
  t->slots[slot].value = value;
  t->slots[slot].owned = owned ? IRIS_SLOT_BOXED : IRIS_SLOT_BORROWED;
}

/* The buffer of a raw spill slot, at least `size` bytes; the caller stores the
 * value's bits in it. The slot owns only the buffer, never what it points to.
 */
void *iris_async_spill_slot(void *task, int64_t slot, int64_t size) {
  IrisTask *t = (IrisTask *)task;
  if (!t || slot < 0 || !iris_task_reserve(t, slot))
    return NULL;
  IrisTaskSlot *s = &t->slots[slot];
  if (s->owned != IRIS_SLOT_RAW || !s->value) {
    iris_task_slot_clear(s);
    s->value = calloc(1, (size_t)(size > 16 ? size : 16));
    if (!s->value) {
      iris_panic("out of memory keeping a value across an await");
      return NULL;
    }
    s->owned = IRIS_SLOT_RAW;
  }
  return s->value;
}

void *iris_async_reload_slot(void *task, int64_t slot) {
  IrisTask *t = (IrisTask *)task;
  if (!t || slot < 0 || slot >= t->nslots ||
      t->slots[slot].owned != IRIS_SLOT_RAW) {
    iris_panic("internal error: async spill slot read before it was written");
    return NULL;
  }
  return t->slots[slot].value;
}

void *iris_async_frame_get(void *task, int64_t slot) {
  IrisTask *t = (IrisTask *)task;
  if (!t || slot < 0 || slot >= t->nslots) {
    iris_panic("internal error: async frame slot read before it was written");
    return NULL;
  }
  return t->slots[slot].value;
}

int64_t iris_async_state(void *task) {
  return task ? ((IrisTask *)task)->state : 0;
}

void iris_async_set_state(void *task, int64_t state) {
  if (task)
    ((IrisTask *)task)->state = state;
}

void iris_async_schedule(void *task) {
  IrisTask *t = (IrisTask *)task;
  if (!t || t->done || t->scheduled)
    return;
  t->scheduled = 1;
  iris_task_ref(t); /* the queue's */
  t->next = NULL;
  if (iris_exec_tail)
    iris_exec_tail->next = t;
  else
    iris_exec_head = t;
  iris_exec_tail = t;
}

int32_t iris_async_is_done(void *task) {
  return task && ((IrisTask *)task)->done ? 1 : 0;
}

void iris_async_wait(void *task, void *target) {
  IrisTask *t = (IrisTask *)target;
  if (!t || t->done) {
    iris_async_schedule(task);
    return;
  }
  if (t->nwaiters == t->capwaiters) {
    int64_t n = t->capwaiters ? t->capwaiters * 2 : 4;
    IrisTask **grown =
        (IrisTask **)realloc(t->waiters, (size_t)n * sizeof(IrisTask *));
    if (!grown) {
      iris_panic("out of memory registering a task waiter");
      return;
    }
    t->waiters = grown;
    t->capwaiters = n;
  }
  iris_task_ref((IrisTask *)task); /* the waiters list's */
  t->waiters[t->nwaiters++] = (IrisTask *)task;
}

/* Finish `t` (with a result, or cancelled): drop its frame and wake every
 * task waiting on it. */
static void iris_task_finish(IrisTask *t, void *value, int32_t owned,
                             int cancelled) {
  if (!t || t->done)
    return;
  t->done = 1;
  t->cancelled = cancelled;
  t->result = value;
  t->result_owned = owned;
  iris_task_drop_contents(t);
  IrisTask **waiters = t->waiters;
  int64_t nwaiters = t->nwaiters;
  t->waiters = NULL;
  t->nwaiters = 0;
  t->capwaiters = 0;
  for (int64_t i = 0; i < nwaiters; i++) {
    iris_async_schedule(waiters[i]); /* takes the queue's reference first */
    iris_task_unref(waiters[i]);
  }
  free(waiters);
}

void iris_async_complete(void *task, void *value, int32_t owned) {
  iris_task_finish((IrisTask *)task, value, owned, 0);
}

/* Stop a task: it is never polled again, a timer or receive it stands for is
 * dropped (a cancelled receive takes no value), and whatever awaits it wakes;
 * awaiting a cancelled task is an error. */
void iris_async_cancel(void *task) {
  iris_task_finish((IrisTask *)task, NULL, 0, 1);
}

int32_t iris_async_is_cancelled(void *task) {
  return task && ((IrisTask *)task)->cancelled ? 1 : 0;
}

void *iris_async_result(void *task) {
  IrisTask *t = (IrisTask *)task;
  if (!t || !t->done) {
    iris_panic("internal error: result of an unfinished task was read");
    return NULL;
  }
  if (t->cancelled) {
    iris_panic("awaited a task that was cancelled");
    return NULL;
  }
  return t->result;
}

/* `async_any(...)`: a task that completes with the index of the first input
 * to finish. Inputs are added one by one; each registers it as a waiter. */
void *iris_async_any_new(void) { return iris_task_alloc(NULL, IRIS_TASK_ANY); }

void iris_async_any_add(void *any, void *input) {
  IrisTask *a = (IrisTask *)any;
  if (!a || !input || a->done)
    return;
  if (a->ninputs == a->capinputs) {
    int64_t n = a->capinputs ? a->capinputs * 2 : 4;
    IrisTask **grown =
        (IrisTask **)realloc(a->inputs, (size_t)n * sizeof(IrisTask *));
    if (!grown) {
      iris_panic("out of memory waiting for several tasks");
      return;
    }
    a->inputs = grown;
    a->capinputs = n;
  }
  iris_task_ref((IrisTask *)input); /* the any-task's */
  a->inputs[a->ninputs++] = (IrisTask *)input;
  /* Schedules `a` right away when the input has already finished. */
  iris_async_wait(a, input);
}

void *iris_async_any_list(IrisList *tasks) {
  void *any = iris_async_any_new();
  int64_t n = tasks ? iris_list_len(tasks) : 0;
  for (int64_t i = 0; i < n; i++) {
    iris_async_any_add(any, iris_unbox_task(iris_list_get(tasks, i)));
  }
  return any;
}

static int iris_task_is_leaf(IrisTask *t) {
  return t->kind == IRIS_TASK_SLEEP || t->kind == IRIS_TASK_RECV ||
         t->kind == IRIS_TASK_YIELD || t->kind == IRIS_TASK_READABLE ||
         t->kind == IRIS_TASK_WRITABLE;
}

/* Complete an any-task once an input has finished, cancelling the losing
 * timers and receives so that a losing receive never takes a value. Other
 * losing tasks keep running: their handles may still be awaited. */
static void iris_exec_finish_any(IrisTask *a) {
  int64_t winner = -1;
  for (int64_t i = 0; i < a->ninputs; i++) {
    if (a->inputs[i]->done) {
      winner = i;
      break;
    }
  }
  if (winner < 0)
    return;
  for (int64_t i = 0; i < a->ninputs; i++) {
    IrisTask *input = a->inputs[i];
    if (i != winner && !input->done && iris_task_is_leaf(input))
      iris_async_cancel(input);
  }
  iris_async_complete(a, iris_box_i64(winner), 1);
}

static void iris_exec_add_leaf(IrisTask *t) {
  if (iris_exec_nleaves == iris_exec_capleaves) {
    int64_t n = iris_exec_capleaves ? iris_exec_capleaves * 2 : 16;
    IrisTask **grown =
        (IrisTask **)realloc(iris_exec_leaves, (size_t)n * sizeof(IrisTask *));
    if (!grown) {
      iris_panic("out of memory registering a timer or receive");
      return;
    }
    iris_exec_leaves = grown;
    iris_exec_capleaves = n;
  }
  iris_task_ref(t); /* the leaves list's */
  iris_exec_leaves[iris_exec_nleaves++] = t;
}

void *iris_async_sleep(int64_t ms) {
  IrisTask *t = iris_task_alloc(NULL, IRIS_TASK_SLEEP);
  t->deadline_ms = iris_monotonic_ms() + (ms > 0 ? ms : 0);
  iris_exec_add_leaf(t);
  return t;
}

void *iris_async_recv(void *chan) {
  IrisTask *t = iris_task_alloc(NULL, IRIS_TASK_RECV);
  t->chan = (IrisChannel *)chan;
  iris_exec_add_leaf(t);
  return t;
}

void *iris_async_yield(void) {
  IrisTask *t = iris_task_alloc(NULL, IRIS_TASK_YIELD);
  iris_exec_add_leaf(t);
  return t;
}

/* A task that completes when `sock` (a socket handle from tcp_listen,
 * tcp_accept or tcp_connect) can be read without blocking: awaiting it lets
 * other tasks run while a connection or data is on its way. */
void *iris_async_readable(int64_t sock) {
  iris_sandbox_require_network_enabled();
  IrisTask *t = iris_task_alloc(NULL, IRIS_TASK_READABLE);
  t->sock = sock;
  iris_exec_add_leaf(t);
  return t;
}

/* A task that completes when `sock` can be written without blocking. */
void *iris_async_writable(int64_t sock) {
  iris_sandbox_require_network_enabled();
  IrisTask *t = iris_task_alloc(NULL, IRIS_TASK_WRITABLE);
  t->sock = sock;
  iris_exec_add_leaf(t);
  return t;
}

#if defined(__IRIS_WASM_STUB)
/* No sockets: a readable/writable task completes at once. */
static int iris_sockets_poll_events(const int64_t *socks, const int *writable,
                                    int *ready, int64_t n, int64_t timeout_ms) {
  (void)timeout_ms;
  (void)writable;
  for (int64_t i = 0; i < n; i++)
    ready[i] = 1;
  (void)socks;
  return (int)n;
}
#else
/* Poll `n` sockets for readability or writability for up to `timeout_ms` (0:
 * just check); sets ready[i] and returns how many are ready. Hang-ups and
 * errors count as ready, so the I/O that follows reports them. */
static int iris_sockets_poll_events(const int64_t *socks, const int *writable,
                                    int *ready, int64_t n, int64_t timeout_ms) {
  if (n <= 0)
    return 0;
#ifdef _WIN32
  WSAPOLLFD *fds = (WSAPOLLFD *)calloc((size_t)n, sizeof(WSAPOLLFD));
#else
  struct pollfd *fds =
      (struct pollfd *)calloc((size_t)n, sizeof(struct pollfd));
#endif
  if (!fds) {
    iris_panic("out of memory polling sockets");
    return 0;
  }
  for (int64_t i = 0; i < n; i++) {
    int want_write = writable && writable[i];
#ifdef _WIN32
    fds[i].fd = (SOCKET)socks[i];
    fds[i].events = want_write ? POLLWRNORM : POLLRDNORM;
#else
    fds[i].fd = (int)socks[i];
    fds[i].events = want_write ? POLLOUT : POLLIN;
#endif
    ready[i] = 0;
  }
  int timeout = timeout_ms > 0x7fffffff ? 0x7fffffff : (int)timeout_ms;
#ifdef _WIN32
  int got = WSAPoll(fds, (ULONG)n, timeout);
#else
  int got = poll(fds, (nfds_t)n, timeout);
#endif
  int count = 0;
  for (int64_t i = 0; i < n; i++) {
    /* A failed poll marks everything ready: the I/O reports the error. */
    if (got < 0 || fds[i].revents != 0) {
      ready[i] = 1;
      count++;
    }
  }
  free(fds);
  return count;
}
#endif

static int iris_sockets_poll(const int64_t *socks, int *ready, int64_t n,
                             int64_t timeout_ms) {
  return iris_sockets_poll_events(socks, NULL, ready, n, timeout_ms);
}

/* How long an executor waiting on sockets polls them before re-checking its
 * other leaves: a channel send does not interrupt the poll. */
#define IRIS_EXEC_SOCKET_RECHECK_MS 50

static int iris_exec_wait_sockets(int64_t wait_ms) {
  int64_t n = 0;
  for (int64_t i = 0; i < iris_exec_nleaves; i++) {
    IrisTask *t = iris_exec_leaves[i];
    if ((t->kind == IRIS_TASK_READABLE || t->kind == IRIS_TASK_WRITABLE) &&
        !t->done)
      n++;
  }
  if (n == 0)
    return 0;
  int64_t *socks = (int64_t *)malloc((size_t)n * sizeof(int64_t));
  int *writable = (int *)malloc((size_t)n * sizeof(int));
  int *ready = (int *)malloc((size_t)n * sizeof(int));
  if (!socks || !writable || !ready) {
    iris_panic("out of memory polling sockets");
    return 0;
  }
  int64_t k = 0;
  for (int64_t i = 0; i < iris_exec_nleaves; i++) {
    IrisTask *t = iris_exec_leaves[i];
    if ((t->kind == IRIS_TASK_READABLE || t->kind == IRIS_TASK_WRITABLE) &&
        !t->done) {
      socks[k] = t->sock;
      writable[k] = (t->kind == IRIS_TASK_WRITABLE);
      k++;
    }
  }
  iris_sockets_poll_events(socks, writable, ready, n, wait_ms);
  free(socks);
  free(writable);
  free(ready);
  return 1;
}

/* How long an executor waiting only on receives sleeps before re-checking
 * them, in case a value arrives without a wake-up. */
#define IRIS_EXEC_RECV_RECHECK_MS 50

/* Complete every leaf whose condition holds. Returns whether any did; sets
 * *wait_ms to how long the executor may sleep (-1: there are no leaves). */
static int iris_exec_tick_leaves(int after_idle, int64_t *wait_ms) {
  int64_t wait = -1;
  int64_t now = iris_monotonic_ms();
  int64_t keep = 0;
  int64_t count = iris_exec_nleaves;
  *wait_ms = -1;
  if (count == 0)
    return 0;
  /* Leaves that finish this tick, completed below in the order they became
   * ready, so timers that expire between two ticks keep deadline order;
   * and cancelled leaves, released below. */
  IrisTask **done = (IrisTask **)malloc((size_t)count * sizeof(IrisTask *));
  void **values = (void **)malloc((size_t)count * sizeof(void *));
  int64_t *ready_at = (int64_t *)malloc((size_t)count * sizeof(int64_t));
  IrisTask **dropped = (IrisTask **)malloc((size_t)count * sizeof(IrisTask *));
  int64_t ndone = 0;
  int64_t ndropped = 0;
  if (!done || !values || !ready_at || !dropped) {
    iris_panic("out of memory running timers");
    return 0;
  }
  int64_t sleep_cutoff = now;
  for (int64_t i = 0; i < count; i++) {
    IrisTask *t = iris_exec_leaves[i];
    if (!t->done && t->kind == IRIS_TASK_SLEEP && t->deadline_ms <= now) {
      if (t->deadline_ms + 5 < sleep_cutoff) {
        sleep_cutoff = t->deadline_ms + 5;
      }
    }
  }
  for (int64_t i = 0; i < count; i++) {
    IrisTask *t = iris_exec_leaves[i];
    if (t->done) {
      /* Cancelled. */
      dropped[ndropped++] = t;
      continue;
    }
    void *value = NULL;
    int finished = 0;
    int64_t when = now;
    switch (t->kind) {
    case IRIS_TASK_SLEEP:
      if (t->deadline_ms <= sleep_cutoff) {
        value = iris_box_i64(0);
        finished = 1;
        when = t->deadline_ms;
      } else {
        int64_t left = t->deadline_ms > now ? t->deadline_ms - now : 0;
        if (wait < 0 || left < wait)
          wait = left;
      }
      break;
    case IRIS_TASK_RECV: {
      IrisChannel *c = t->chan;
      pthread_mutex_lock(&c->mu);
      if (c->count > 0) {
        value = c->buf[c->head];
        c->head = (c->head + 1) % c->cap;
        c->count--;
        if (c->max_cap >= 0)
          pthread_cond_signal(&c->not_full);
        finished = 1;
      }
      pthread_mutex_unlock(&c->mu);
      /* A send wakes the executor; re-check now and then regardless. */
      if (!finished && (wait < 0 || wait > IRIS_EXEC_RECV_RECHECK_MS)) {
        wait = IRIS_EXEC_RECV_RECHECK_MS;
      }
      break;
    }
    case IRIS_TASK_YIELD:
      if (after_idle) {
        value = iris_box_i64(0);
        finished = 1;
      } else {
        wait = 0;
      }
      break;
    case IRIS_TASK_READABLE: {
      int ready = 0;
      iris_sockets_poll(&t->sock, &ready, 1, 0);
      if (ready) {
        value = iris_box_i64(0);
        finished = 1;
      } else if (wait < 0 || wait > IRIS_EXEC_SOCKET_RECHECK_MS) {
        wait = IRIS_EXEC_SOCKET_RECHECK_MS;
      }
      break;
    }
    case IRIS_TASK_WRITABLE: {
      int ready = 0;
      int writable_flag = 1;
      iris_sockets_poll_events(&t->sock, &writable_flag, &ready, 1, 0);
      if (ready) {
        value = iris_box_i64(0);
        finished = 1;
      } else if (wait < 0 || wait > IRIS_EXEC_SOCKET_RECHECK_MS) {
        wait = IRIS_EXEC_SOCKET_RECHECK_MS;
      }
      break;
    }
    default:
      break;
    }
    if (finished) {
      /* Insertion sort on readiness time; stable for equal times. */
      int64_t j = ndone++;
      while (j > 0 && ready_at[j - 1] > when) {
        done[j] = done[j - 1];
        values[j] = values[j - 1];
        ready_at[j] = ready_at[j - 1];
        j--;
      }
      done[j] = t;
      values[j] = value;
      ready_at[j] = when;
    } else {
      iris_exec_leaves[keep++] = t;
    }
  }
  iris_exec_nleaves = keep;
  /* Completing wakes waiters but runs no task, so no leaf is added here. */
  for (int64_t i = 0; i < ndone; i++) {
    iris_async_complete(done[i], values[i], 1);
    iris_task_unref(done[i]);
  }
  for (int64_t i = 0; i < ndropped; i++)
    iris_task_unref(dropped[i]);
  free(done);
  free(values);
  free(ready_at);
  free(dropped);
  *wait_ms = wait;
  return ndone > 0 || ndropped > 0;
}

/* Make progress without blocking: poll one ready task, or else complete the
 * leaves whose condition holds (yields first, as the ready queue has just
 * drained). Returns 1 if anything ran; otherwise 0, with *wait_ms set to how
 * long until a leaf may be ready (-1: this thread has no pending leaves). */
static int iris_exec_step(int64_t *wait_ms) {
  *wait_ms = -1;
  IrisUiPool *saved_pool = iris_active_ui_pool;
  iris_active_ui_pool = NULL;
  IrisTask *t = iris_exec_head;
  if (t) {
    iris_exec_head = t->next;
    if (!iris_exec_head)
      iris_exec_tail = NULL;
    t->next = NULL;
    t->scheduled = 0;
    if (!t->done) {
      if (t->kind == IRIS_TASK_ANY)
        iris_exec_finish_any(t);
      else if (t->poll)
        t->poll(t);
    }
    iris_task_unref(t); /* the queue's */
    iris_active_ui_pool = saved_pool;
    return 1;
  }
  int ticked = iris_exec_tick_leaves(1, wait_ms);
  iris_active_ui_pool = saved_pool;
  return ticked;
}

/* Run this thread's executor until `task` is done: `await` outside an
 * `async def`. */
void *iris_async_block_on(void *task) {
  IrisTask *target = (IrisTask *)task;
  if (!target)
    return NULL;
  for (;;) {
    if (target->done)
      return iris_async_result(target);
    int64_t epoch = iris_exec_epoch();
    int64_t wait_ms = -1;
    if (iris_exec_step(&wait_ms))
      continue;
    if (wait_ms < 0) {
      iris_panic(
          "await can never complete: no task is ready, sleeping or receiving, "
          "so the awaited task is waiting on itself or on a task nothing will "
          "finish");
      return NULL;
    }
    iris_exec_idle_wait(epoch, wait_ms);
  }
}

/* ── GPU: `par for` kernels on the CUDA driver (`--gpu cuda`) ─────────────
 * The kernels' PTX is compiled into the program. The driver -- nvcuda.dll,
 * libcuda.so.1 -- is loaded when the first kernel runs and compiles the PTX
 * for the GPU it finds. A capture travels in an 8-byte slot (`sig` gives
 * each one's kind: t tensor, l i64, w i32, c i8, d f64, f f32, b bool,
 * x unread); a tensor is copied to the device with its shape, under a
 * header laid out like IrisTensor's first fields, and copied back after the
 * kernel. A device error (__iris_dev_error) is reported as the CPU reports
 * it. */
typedef int iris_cu_result;
typedef unsigned long long iris_cu_ptr;
static struct {
  int tried, ok;
  char why[512];
  iris_cu_result (*cuInit)(unsigned int);
  iris_cu_result (*cuDeviceGet)(int *, int);
  iris_cu_result (*cuDevicePrimaryCtxRetain)(void **, int);
  iris_cu_result (*cuCtxSetCurrent)(void *);
  iris_cu_result (*cuModuleLoadData)(void **, const void *);
  iris_cu_result (*cuModuleGetFunction)(void **, void *, const char *);
  iris_cu_result (*cuModuleGetGlobal)(iris_cu_ptr *, size_t *, void *,
                                      const char *);
  iris_cu_result (*cuMemAlloc)(iris_cu_ptr *, size_t);
  iris_cu_result (*cuMemFree)(iris_cu_ptr);
  iris_cu_result (*cuMemcpyHtoD)(iris_cu_ptr, const void *, size_t);
  iris_cu_result (*cuMemcpyDtoH)(void *, iris_cu_ptr, size_t);
  iris_cu_result (*cuLaunchKernel)(void *, unsigned, unsigned, unsigned,
                                   unsigned, unsigned, unsigned, unsigned,
                                   void *, void **, void **);
  iris_cu_result (*cuCtxSynchronize)(void);
  iris_cu_result (*cuGetErrorString)(iris_cu_result, const char **);
  void *ctx;
} iris_cu;

static pthread_mutex_t iris_cu_mu = PTHREAD_MUTEX_INITIALIZER;

static const char *iris_cu_error(iris_cu_result r) {
  const char *s = NULL;
  if (iris_cu.cuGetErrorString)
    iris_cu.cuGetErrorString(r, &s);
  return s ? s : "unknown CUDA error";
}

/* Load the driver and make a context on device 0; why it failed in `why`. */
static int iris_cu_init(void) {
  if (iris_cu.tried)
    return iris_cu.ok;
  iris_cu.tried = 1;
#ifdef _WIN32
  void *lib = (void *)LoadLibraryA("nvcuda.dll");
#define IRIS_CU_SYM(n) ((void *)GetProcAddress((HMODULE)lib, n))
#else
  void *lib = dlopen("libcuda.so.1", RTLD_NOW);
  if (!lib)
    lib = dlopen("libcuda.so", RTLD_NOW);
#define IRIS_CU_SYM(n) dlsym(lib, n)
#endif
  if (!lib) {
    snprintf(iris_cu.why, sizeof iris_cu.why,
             "no CUDA driver: this program runs `par for` loops on an NVIDIA "
             "GPU (built with --gpu cuda), "
             "and the driver (%s) is not installed",
#ifdef _WIN32
             "nvcuda.dll"
#else
             "libcuda.so.1"
#endif
    );
    return 0;
  }
#define IRIS_CU_LOAD(field, name)                                              \
  do {                                                                         \
    *(void **)&iris_cu.field = IRIS_CU_SYM(name);                              \
    if (!iris_cu.field) {                                                      \
      snprintf(iris_cu.why, sizeof iris_cu.why, "the CUDA driver has no %s",   \
               name);                                                          \
      return 0;                                                                \
    }                                                                          \
  } while (0)
  IRIS_CU_LOAD(cuGetErrorString, "cuGetErrorString");
  IRIS_CU_LOAD(cuInit, "cuInit");
  IRIS_CU_LOAD(cuDeviceGet, "cuDeviceGet");
  IRIS_CU_LOAD(cuDevicePrimaryCtxRetain, "cuDevicePrimaryCtxRetain");
  IRIS_CU_LOAD(cuCtxSetCurrent, "cuCtxSetCurrent");
  IRIS_CU_LOAD(cuModuleLoadData, "cuModuleLoadData");
  IRIS_CU_LOAD(cuModuleGetFunction, "cuModuleGetFunction");
  IRIS_CU_LOAD(cuModuleGetGlobal, "cuModuleGetGlobal_v2");
  IRIS_CU_LOAD(cuMemAlloc, "cuMemAlloc_v2");
  IRIS_CU_LOAD(cuMemFree, "cuMemFree_v2");
  IRIS_CU_LOAD(cuMemcpyHtoD, "cuMemcpyHtoD_v2");
  IRIS_CU_LOAD(cuMemcpyDtoH, "cuMemcpyDtoH_v2");
  IRIS_CU_LOAD(cuLaunchKernel, "cuLaunchKernel");
  IRIS_CU_LOAD(cuCtxSynchronize, "cuCtxSynchronize");
#undef IRIS_CU_LOAD
#undef IRIS_CU_SYM
  iris_cu_result r = iris_cu.cuInit(0);
  int dev = 0;
  if (r == 0)
    r = iris_cu.cuDeviceGet(&dev, 0);
  if (r == 0)
    r = iris_cu.cuDevicePrimaryCtxRetain(&iris_cu.ctx, dev);
  if (r != 0) {
    snprintf(iris_cu.why, sizeof iris_cu.why, "no usable NVIDIA GPU: %s",
             iris_cu_error(r));
    return 0;
  }
  iris_cu.ok = 1;
  return 1;
}

/* PTX image -> loaded module, for the program's lifetime. */
static struct {
  const char *ptx;
  void *module;
} iris_cu_modules[16];
static int iris_cu_module_count = 0;

static void iris_cu_check(iris_cu_result r, const char *what) {
  if (r != 0)
    iris_runtime_error("E0410", "%s on the GPU failed: %s", what,
                       iris_cu_error(r));
}

/* Device header for a tensor: IrisTensor's first fields. */
typedef struct {
  iris_cu_ptr data, shape;
  int32_t ndim, pad;
  int64_t numel;
} IrisCuTensorHeader;

typedef struct {
  int32_t code, dim;
  int64_t a, b, loc, msg;
} IrisCuError;

void iris_cuda_launch(const char *ptx, const char *kernel, const char *sig,
                      const char *const *panics, int32_t npanics, int64_t start,
                      int64_t end, void *env) {
  if (end <= start)
    return;
  pthread_mutex_lock(&iris_cu_mu);
  if (!iris_cu_init()) {
    pthread_mutex_unlock(&iris_cu_mu);
    iris_runtime_error("E0410", "%s", iris_cu.why);
  }
  iris_cu.cuCtxSetCurrent(iris_cu.ctx);
  void *module = NULL;
  for (int i = 0; i < iris_cu_module_count; i++)
    if (iris_cu_modules[i].ptx == ptx)
      module = iris_cu_modules[i].module;
  if (!module) {
    iris_cu_result r = iris_cu.cuModuleLoadData(&module, ptx);
    if (r != 0) {
      pthread_mutex_unlock(&iris_cu_mu);
      iris_runtime_error("E0410",
                         "the GPU could not load this program's kernels: %s",
                         iris_cu_error(r));
    }
    if (iris_cu_module_count < 16) {
      iris_cu_modules[iris_cu_module_count].ptx = ptx;
      iris_cu_modules[iris_cu_module_count].module = module;
      iris_cu_module_count++;
    }
  }
  pthread_mutex_unlock(&iris_cu_mu);

  void *fn = NULL;
  iris_cu_check(iris_cu.cuModuleGetFunction(&fn, module, kernel),
                "finding the kernel");

  int64_t nslots = (int64_t)strlen(sig);
  uint64_t *slots =
      (uint64_t *)calloc((size_t)(nslots > 0 ? nslots : 1), sizeof(uint64_t));
  IrisTensor **hosts = (IrisTensor **)calloc((size_t)(nslots > 0 ? nslots : 1),
                                             sizeof(IrisTensor *));
  iris_cu_ptr *dev = (iris_cu_ptr *)calloc(
      (size_t)(nslots > 0 ? nslots : 1) * 3, sizeof(iris_cu_ptr));
  for (int64_t k = 0; k < nslots; k++) {
    IrisVal *cap =
        sig[k] == 'x' ? NULL : iris_closure_get_capture((IrisVal *)env, (int)k);
    switch (sig[k]) {
    case 't': {
      IrisTensor *t = (IrisTensor *)cap;
      if (!t)
        iris_runtime_error("E0404",
                           "a `par for` captured a tensor that does not exist");
      hosts[k] = t;
      size_t data_bytes = (size_t)t->numel * sizeof(float),
             shape_bytes = (size_t)t->ndim * sizeof(int64_t);
      iris_cu_check(
          iris_cu.cuMemAlloc(&dev[3 * k], data_bytes ? data_bytes : 4),
          "allocating a tensor");
      iris_cu_check(
          iris_cu.cuMemAlloc(&dev[3 * k + 1], shape_bytes ? shape_bytes : 8),
          "allocating a shape");
      iris_cu_check(
          iris_cu.cuMemAlloc(&dev[3 * k + 2], sizeof(IrisCuTensorHeader)),
          "allocating a tensor header");
      if (data_bytes)
        iris_cu_check(iris_cu.cuMemcpyHtoD(dev[3 * k], t->data, data_bytes),
                      "copying a tensor");
      if (shape_bytes)
        iris_cu_check(
            iris_cu.cuMemcpyHtoD(dev[3 * k + 1], t->shape, shape_bytes),
            "copying a shape");
      IrisCuTensorHeader h = {dev[3 * k], dev[3 * k + 1], t->ndim, 0, t->numel};
      iris_cu_check(iris_cu.cuMemcpyHtoD(dev[3 * k + 2], &h, sizeof h),
                    "copying a tensor header");
      slots[k] = dev[3 * k + 2];
      break;
    }
    case 'l':
      slots[k] = (uint64_t)iris_unbox_i64(cap);
      break;
    case 'w': {
      int32_t v = (int32_t)iris_unbox_i64(cap);
      memcpy(&slots[k], &v, sizeof v);
      break;
    }
    case 'c': {
      int8_t v = (int8_t)iris_unbox_i64(cap);
      memcpy(&slots[k], &v, sizeof v);
      break;
    }
    case 'd': {
      double v = iris_unbox_f64(cap);
      memcpy(&slots[k], &v, sizeof v);
      break;
    }
    case 'f': {
      float v = (float)iris_unbox_f64(cap);
      memcpy(&slots[k], &v, sizeof v);
      break;
    }
    case 'b': {
      uint8_t v = iris_unbox_bool(cap) ? 1 : 0;
      memcpy(&slots[k], &v, sizeof v);
      break;
    }
    default:
      break;
    }
  }
  iris_cu_ptr env_dev = 0;
  iris_cu_check(
      iris_cu.cuMemAlloc(&env_dev, (size_t)(nslots > 0 ? nslots : 1) * 8),
      "allocating the captures");
  iris_cu_check(iris_cu.cuMemcpyHtoD(env_dev, slots,
                                     (size_t)(nslots > 0 ? nslots : 1) * 8),
                "copying the captures");

  iris_cu_ptr err_dev = 0;
  size_t err_size = 0;
  iris_cu_check(iris_cu.cuModuleGetGlobal(&err_dev, &err_size, module,
                                          "__iris_dev_error"),
                "finding the error record");
  IrisCuError rec;
  memset(&rec, 0, sizeof rec);
  iris_cu_check(iris_cu.cuMemcpyHtoD(err_dev, &rec, sizeof rec),
                "clearing the error record");

  /* 256 threads a block; a launch covers at most 2^31-1 blocks. */
  const int64_t per_launch = (int64_t)0x7FFFFFFF * 256;
  for (int64_t s = start; s < end; s += per_launch) {
    int64_t e = end - s > per_launch ? s + per_launch : end;
    unsigned grid = (unsigned)((e - s + 255) / 256);
    void *args[3] = {&s, &e, &env_dev};
    iris_cu_check(
        iris_cu.cuLaunchKernel(fn, grid, 1, 1, 256, 1, 1, 0, NULL, args, NULL),
        "launching the kernel");
  }
  iris_cu_check(iris_cu.cuCtxSynchronize(), "running the kernel");
  iris_cu_check(iris_cu.cuMemcpyDtoH(&rec, err_dev, sizeof rec),
                "reading the error record");

  for (int64_t k = 0; k < nslots; k++) {
    if (sig[k] != 't')
      continue;
    if (hosts[k]->numel)
      iris_cu_check(
          iris_cu.cuMemcpyDtoH(hosts[k]->data, dev[3 * k],
                               (size_t)hosts[k]->numel * sizeof(float)),
          "copying a tensor back");
    iris_cu.cuMemFree(dev[3 * k]);
    iris_cu.cuMemFree(dev[3 * k + 1]);
    iris_cu.cuMemFree(dev[3 * k + 2]);
  }
  iris_cu.cuMemFree(env_dev);
  free(slots);
  free(hosts);
  free(dev);

  if (rec.code == 0)
    return;
  /* The GPU's error, reported as the CPU reports it, at its position. */
  *iris_loc_slot() = rec.loc;
  static const char *ops[] = {"addition", "subtraction", "multiplication",
                              "division", "remainder"};
  static const char *syms[] = {"+", "-", "*", "/", "%"};
  switch (rec.code) {
  case 1:
    if (rec.dim < 0)
      iris_runtime_error("E0403", "index %lld is out of bounds for length %lld",
                         (long long)rec.a, (long long)rec.b);
    iris_runtime_error(
        "E0403", "index %lld is out of bounds for dimension %d of length %lld",
        (long long)rec.a, (int)rec.dim, (long long)rec.b);
    break;
  case 2:
    iris_runtime_error("E0403",
                       "a tensor of rank %lld takes %lld indices, or one flat "
                       "index, but %lld were given",
                       (long long)rec.b, (long long)rec.b, (long long)rec.a);
    break;
  case 3: {
    int op = rec.dim >= 0 && rec.dim < 5 ? rec.dim : 0;
    iris_runtime_error("E0409", "integer overflow in %s (%lld %s %lld)",
                       ops[op], (long long)rec.a, syms[op], (long long)rec.b);
    break;
  }
  case 4:
    iris_runtime_error("E0402", "division by zero");
    break;
  case 5:
    iris_panic(rec.msg >= 0 && rec.msg < npanics ? panics[rec.msg]
                                                 : "panic in GPU code");
    break;
  case 6:
    iris_runtime_error("E0410",
                       "out of GPU memory: a tensor of %lld bytes did not fit",
                       (long long)rec.a);
    break;
  case 7:
    iris_runtime_error("E0404", "tensor sizes differ: %lld and %lld elements",
                       (long long)rec.a, (long long)rec.b);
    break;
  default:
    iris_runtime_error("E0410", "the GPU reported error %d", (int)rec.code);
    break;
  }
}
