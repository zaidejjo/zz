// ZZ native runtime — core value model, entry point, and core operations.
//
// A zz_value is a tagged 16-byte union (mirroring the Rust `Value` size
// target): int/float/bool stored inline, everything else heap-allocated
// behind a refcounted pointer. String/array/dict/funcs are refcounted.
//
// The runtime is minimal on purpose: it only implements what the generated
// code actually uses. Dead stdlib modules are never referenced by the
// lowerer, so unused natives/procs simply don't appear here (DCE at the C
// level via -ffunction-sections + gc-sections on release).
//
// This header defines the base types shared by every runtime module plus
// the entry point and core operation declarations. The umbrella header
// `runtime.h` includes this and the domain sub-headers in dependency order.

#ifndef ZZ_RUNTIME_CORE_H
#define ZZ_RUNTIME_CORE_H

#include <stdarg.h>
#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <pthread.h>
#include <math.h>
#include <time.h>
#include <sys/stat.h>
#include <curl/curl.h>
#ifdef __GLIBC__
#include <malloc.h>
#endif

#ifdef __cplusplus
extern "C" {
#endif

typedef enum {
    ZZ_UNIT = 0,
    ZZ_INT,
    ZZ_FLOAT,
    ZZ_BOOL,
    ZZ_STR,
    ZZ_ARRAY,
    ZZ_DICT,
    ZZ_FUNC,
    ZZ_NATIVE,
    ZZ_OPTION_SOME,
    ZZ_OPTION_NONE,
    ZZ_RESULT_OK,
    ZZ_RESULT_ERR,
    ZZ_RANGE,
    ZZ_CHAN,
    ZZ_TASK_JOIN,
    ZZ_OBJECT,
    ZZ_JSON,
    ZZ_TCP_STREAM,
    ZZ_TCP_LISTENER,
    ZZ_TUPLE,
} zz_tag;

typedef struct zz_value zz_value;

// Container forward declarations.
typedef struct zz_array zz_array;
typedef struct zz_dict zz_dict;
typedef struct zz_dict_entry zz_dict_entry;
typedef struct zz_chan zz_chan;
typedef struct zz_task_join zz_task_join;
typedef struct zz_object zz_object;

// A TCP stream or listener: the underlying socket fd.
typedef struct zz_tcp zz_tcp;

// Refcounted string (null-terminated for C interop).
//
// Layout:
//   - `refs`   : reference count (size_t). 0 only valid for freed.
//   - `interned`: non-zero if this object is a permanent singleton owned by
//     the global interning table; zz_release must NOT free it.
//   - `cap`    : allocated buffer capacity (>= len). For heap strings this
//     allows amortized O(1) append and in-place concatenation.
//   - `len`    : payload length in bytes (excluding trailing NUL).
//   - `data[]` : flexible array, `data[len] == '\0'`.
typedef struct {
    size_t refs;
    int interned;
    size_t cap;
    size_t len;
    char data[];
} zz_str;

// A function value: signature + optional captured environment.
typedef struct zz_func zz_func;

struct zz_value {
    zz_tag tag;
    union {
        int64_t i;
        double f;
        bool b;
        zz_str *s;
        zz_array *arr;
        zz_dict *dict;
        zz_func *fn;
        zz_chan *chan;
        zz_task_join *task;
        zz_value *payload;   // Option/Result inner value (heap-allocated)
        zz_object *obj;      // boxed struct instance
        zz_tcp *net;         // TCP stream or listener (ZZ_TCP_STREAM/LISTENER)
    };
};

struct zz_array {
    size_t refs;     // atomic reference count (ARC)
    size_t len;
    size_t cap;
    zz_value *items;
};

// Sentinel value for `zz_array::refs` indicating the array (and its items
// buffer) live on the C stack — emitted by codegen when it can prove the
// array literal is non-escaping and all elements are scalar. Stack-promoted
// arrays must NEVER be freed via free(): both the header struct and the
// items buffer are part of the current C stack frame and are reclaimed
// automatically when the function returns. zz_release_array checks for
// this sentinel and skips the free paths accordingly.
#define ZZ_ARRAY_STACK_MAGIC ((size_t)0xD0D0D0D0D0D0D0D0ULL)

// Sentinel value for `zz_array::refs` on fixed-size array literals whose
// header AND items buffer are both bump-allocated from the function arena
// (`zz_array_new_lit`). The items buffer has the exact literal capacity, so
// element stores never call realloc, and element values are stored directly
// without `zz_clone` atomic bumps (scalar literal contents only). Release is
// a no-op: both blocks die at the next arena reset.
#define ZZ_ARRAY_LIT_MAGIC ((size_t)0xC0DEC0DEC0DEC0DEULL)

// Sentinel for dicts whose entries buffer is bump-allocated on the arena.
// zz_dict_set must NOT call realloc on these — the buffer is fixed-capacity
// and dies at arena reset. Release is a no-op.
#define ZZ_DICT_ARENA_MAGIC ((size_t)0xA1A2A3A4A5A6A7A8ULL)

// Sentinel for arena-allocated arrays with pre-sized items buffer.
// Items are bump-allocated on the arena. If zz_vec_append needs to grow
// beyond the pre-sized capacity, it migrates to heap (malloc) and resets
// refs to 0 (arena-header sentinel). Release frees items (if heap-migrated)
// but skips free(header) since the header stays on the arena.
#define ZZ_ARRAY_ARENA_MAGIC ((size_t)0xB3B4B5B6B7B8B9B0ULL)

struct zz_dict {
    size_t refs;     // atomic reference count (ARC)
    size_t len;
    size_t cap;
    zz_dict_entry *entries;
};

struct zz_dict_entry {
    zz_str *key;
    zz_value val;
};

// Boxed struct instance (reference-counted).
// Fields are stored as alternating name/value pairs (zz_value) for
// runtime field access by name. Field names are interned strings.
struct zz_object {
    size_t refs;
    const char *type_name;  // struct type name for method dispatch
    size_t len;             // number of fields (pairs count = len * 2)
    zz_value fields[];      // alternating: name (str), value, name, value, ...
};

typedef zz_value (*zz_native_fn)(zz_value *args, size_t argc);

struct zz_func {
    size_t refs;
    zz_value fn;      // either ZZ_NATIVE (builtin) or the dispatch table
    zz_value *env;    // captured slot values (SYNTHETIC: extended below)
    size_t env_len;
};

// ---- thread-safe channels (pthread-based) ------------------------------
// Thread-safe channel for inter-thread communication.
struct zz_chan {
    pthread_mutex_t lock;
    pthread_cond_t  cond;
    zz_value       *queue;   // ring buffer of zz_value
    size_t          len;     // current number of items
    size_t          cap;     // buffer capacity
};

// Task join handle for spawned threads.
struct zz_task_join {
    pthread_t       thread;
    zz_value        result;
    int             completed;
    pthread_mutex_t lock;
    pthread_cond_t  cond;
};

// ---- arena allocator ---------------------------------------------------
// A bump allocator for non-escaping local allocations. O(1) alloc, O(1)
// reset. Each function gets its own arena that is initialized on entry and
// reset on exit. Objects allocated here do NOT need individual free calls.
//
// Thread-local: each thread maintains its own arena (no locking needed).
typedef struct zz_arena {
    char  *buf;       // contiguous memory block
    size_t cap;       // total capacity in bytes
    size_t offset;    // next free byte position
} zz_arena;

// O(1) reset: free all arena allocations at once by resetting the offset.
// The arena's buffer is NOT freed — it is reused for the next function call.
static inline void zz_arena_reset(zz_arena *a) {
    a->offset = 0;
}

// Reset the arena and hint the C allocator to return freed heap pages to
// the OS. Slower than bare reset — call only at function-level cleanup,
// not per-iteration in tight loops.
static inline void zz_arena_reset_trim(zz_arena *a) {
    a->offset = 0;
#ifdef __GLIBC__
    malloc_trim(0);
#endif
}

// ---- constructors ------------------------------------------------------
static inline zz_value zz_unit(void) {
    zz_value v = {ZZ_UNIT, {0}};
    return v;
}
static inline zz_value zz_int(int64_t i) {
    zz_value v = {ZZ_INT, {0}};
    v.i = i;
    return v;
}
static inline zz_value zz_float(double f) {
    zz_value v = {ZZ_FLOAT, {0}};
    v.f = f;
    return v;
}
static inline zz_value zz_bool(bool b) {
    zz_value v = {ZZ_BOOL, {0}};
    v.b = b;
    return v;
}

// ---- binaries ----------------------------------------------------------
#define ZZOP_ADD 1
#define ZZOP_SUB 2
#define ZZOP_MUL 3
#define ZZOP_DIV 4
#define ZZOP_REM 5
#define ZZOP_POW 6
#define ZZOP_EQ 7
#define ZZOP_NE 8
#define ZZOP_LT 9
#define ZZOP_GT 10
#define ZZOP_LE 11
#define ZZOP_GE 12

zz_value zz_binop(int op, zz_value a, zz_value b);
zz_value zz_neg(zz_value a);
zz_value zz_not(zz_value a);
bool zz_truthy(zz_value v);

// ---- calls --------------------------------------------------------------
typedef zz_value (*zz_dispatch_fn)(zz_value *args, size_t argc);

// Build a callable closure value from a generated function pointer. The
// payload stores the function pointer (not a refcounted object).
zz_value zz_closure_make(zz_dispatch_fn f);
// Extract the generated function pointer from a closure value.
zz_dispatch_fn zz_closure_target(zz_value v);

zz_value zz_call(zz_value fn, zz_value *args, size_t argc, int *err);

// Codegen helper shims.
zz_value zz_call_native1(zz_value (*f)(zz_value, int *), zz_value a);
zz_value zz_call_native0(zz_value (*f)(zz_value, int *));
zz_value zz_call_native2(zz_value (*f)(zz_value, zz_value, int *), zz_value a, zz_value b);
zz_value zz_call_native3(zz_value (*f)(zz_value, zz_value, zz_value, int *), zz_value a, zz_value b, zz_value c);

// ---- io / math / time natives ------------------------------------------
zz_value zz_io_println(zz_value v, int *err);
zz_value zz_io_print(zz_value v, int *err);
zz_value zz_io_input(zz_value prompt, int *err);
zz_value zz_math_abs(zz_value v, int *err);
zz_value zz_math_sqrt(zz_value v, int *err);
zz_value zz_math_pow(zz_value a, zz_value b, int *err);
zz_value zz_math_floor(zz_value v, int *err);
zz_value zz_math_ceil(zz_value v, int *err);
zz_value zz_math_round(zz_value v, int *err);
zz_value zz_math_trunc(zz_value v, int *err);
zz_value zz_math_signum(zz_value v, int *err);
zz_value zz_math_hypot(zz_value x, zz_value y, int *err);
zz_value zz_math_clamp(zz_value val, zz_value min, zz_value max, int *err);
zz_value zz_math_root(zz_value x, zz_value n, int *err);
zz_value zz_math_factorial(zz_value n, int *err);
zz_value zz_math_gcd(zz_value a, zz_value b, int *err);
zz_value zz_math_lcm(zz_value a, zz_value b, int *err);
zz_value zz_math_sin(zz_value x, int *err);
zz_value zz_math_cos(zz_value x, int *err);
zz_value zz_math_tan(zz_value x, int *err);
zz_value zz_math_asin(zz_value x, int *err);
zz_value zz_math_acos(zz_value x, int *err);
zz_value zz_math_atan(zz_value x, int *err);
zz_value zz_math_sin_deg(zz_value x, int *err);
zz_value zz_math_cos_deg(zz_value x, int *err);
zz_value zz_math_tan_deg(zz_value x, int *err);
zz_value zz_math_to_radians(zz_value deg, int *err);
zz_value zz_math_to_degrees(zz_value rad, int *err);
zz_value zz_math_log(zz_value x, int *err);
zz_value zz_math_log10(zz_value x, int *err);
zz_value zz_math_exp(zz_value x, int *err);
zz_value zz_math_random(zz_value unused, int *err);
zz_value zz_math_is_nan(zz_value x, int *err);
zz_value zz_math_is_inf(zz_value x, int *err);
zz_value zz_math_pi(zz_value unused, int *err);
zz_value zz_math_e(zz_value unused, int *err);
zz_value zz_math_tau(zz_value unused, int *err);
zz_value zz_math_inf(zz_value unused, int *err);
zz_value zz_math_nan(zz_value unused, int *err);
zz_value zz_math_isqrt(zz_value n, int *err);
zz_value zz_math_mean(zz_value list, int *err);
zz_value zz_math_median(zz_value list, int *err);
zz_value zz_math_rand_range(zz_value min, zz_value max, int *err);
zz_value zz_math_dot_product(zz_value v1, zz_value v2, int *err);
zz_value zz_math_magnitude(zz_value v, int *err);
zz_value zz_math_matrix_mul(zz_value m1, zz_value m2, int *err);
zz_value zz_time_now_ms(zz_value unused, int *err);
zz_value zz_time_sleep_ms(zz_value ms, int *err);

// ---- type casts ---------------------------------------------------------
zz_value zz_typeof(zz_value v, int *err);
zz_value zz_int_cast(zz_value v, int *err);
zz_value zz_float_cast(zz_value v, int *err);
zz_value zz_bool_cast(zz_value v, int *err);

// ---- env natives --------------------------------------------------------
zz_value zz_env_get(zz_value name, int *err);
zz_value zz_env_var(zz_value name, int *err);
zz_value zz_env_args(zz_value unused, int *err);

// ---- fs natives ---------------------------------------------------------
zz_value zz_fs_read(zz_value path, int *err);
zz_value zz_fs_write(zz_value path, zz_value data, int *err);
zz_value zz_fs_exists(zz_value path, int *err);
zz_value zz_fs_remove(zz_value path, int *err);
zz_value zz_fs_mkdir(zz_value path, int *err);
zz_value zz_fs_readdir(zz_value path, int *err);

// ---- encoding natives ---------------------------------------------------
zz_value zz_encoding_url_encode(zz_value s, int *err);
zz_value zz_encoding_url_decode(zz_value s, int *err);
zz_value zz_encoding_base64_encode(zz_value s, int *err);
zz_value zz_encoding_base64_decode(zz_value s, int *err);
zz_value zz_encoding_hex_encode(zz_value s, int *err);
zz_value zz_encoding_hex_decode(zz_value s, int *err);

// ---- channels / spawn ---------------------------------------------------
zz_value zz_chan_new(int *err);
zz_value zz_chan_send(zz_value chan, zz_value val, int *err);
zz_value zz_chan_recv(zz_value chan, int *err);
zz_value zz_chan_try_recv(zz_value chan, int *err);
zz_value zz_spawn(zz_value fn, int *err);
zz_value zz_task_join_recv(zz_value join, int *err);

// ---- std.net TCP --------------------------------------------------------
zz_value zz_tcp_listen(zz_value addr, int *err);
zz_value zz_tcp_connect(zz_value addr, zz_value timeout_ms, int *err);
zz_value zz_tcp_accept(zz_value listener, int *err);
zz_value zz_tcp_write(zz_value stream, zz_value data, int *err);
zz_value zz_tcp_read(zz_value stream, zz_value max_bytes, int *err);
zz_value zz_tcp_readline(zz_value stream, int *err);
zz_value zz_tcp_close(zz_value stream, int *err);
zz_value zz_tcp_peer_addr(zz_value stream, int *err);
zz_value zz_tcp_local_addr(zz_value stream, int *err);
zz_value zz_tcp_set_read_timeout(zz_value stream, zz_value ms, int *err);
zz_value zz_tcp_set_write_timeout(zz_value stream, zz_value ms, int *err);

// http AOT server — thread-per-connection, returns "OK" for all requests
// All functions follow the native call convention: (zz_value... , int *err)
zz_value zz_http_server(zz_value unused, int *err);  // 0 args → unused=unit
zz_value zz_http_route_get(zz_value server, zz_value path, zz_value handler, int *err);  // 3 args (handler ignored in AOT)
zz_value zz_http_log(zz_value server, zz_value enabled, int *err);  // 2 args
zz_value zz_http_listen(zz_value server, zz_value port, int *err);  // 2 args
zz_value zz_http_handle(zz_value server, zz_value method, zz_value path, zz_value body, int *err);  // 4 args

// HTTP client stubs (native mode — returns mock responses)
zz_value zz_http_get(zz_value url, zz_value headers, int *err);
zz_value zz_http_post(zz_value url, zz_value body, zz_value headers, int *err);
zz_value zz_http_response_status(zz_value resp, int *err);
zz_value zz_http_response_text(zz_value resp, int *err);
zz_value zz_http_response_json(zz_value resp, int *err);
zz_value zz_http_response_headers(zz_value resp, int *err);

// ---- option / result ----------------------------------------------------
zz_value zz_option_expect(zz_value opt, zz_value msg, int *err);
zz_value zz_result_expect(zz_value res, zz_value msg, int *err);

// ---- runtime glue ------------------------------------------------------
// Generated code calls zz_main (top-level statements) then zz_call_main.
int zz_run(void);

// Externs defined by generated code:
extern void zz_main(void);
extern int zz_call_main(void);

#ifdef __cplusplus
}
#endif

#endif // ZZ_RUNTIME_CORE_H