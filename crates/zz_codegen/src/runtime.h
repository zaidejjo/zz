// ZZ native runtime — value model and operations.
//
// A zz_value is a tagged 16-byte union (mirroring the Rust `Value` size
// target): int/float/bool stored inline, everything else heap-allocated
// behind a refcounted pointer. String/array/dict/funcs are refcounted.
//
// The runtime is minimal on purpose: it only implements what the generated
// code actually uses. Dead stdlib modules are never referenced by the
// lowerer, so unused natives/procs simply don't appear here (DCE at the C
// level via -ffunction-sections + gc-sections on release).

#ifndef ZZ_RUNTIME_H
#define ZZ_RUNTIME_H

#include <stdarg.h>
#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <pthread.h>
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

// Channel operations.
zz_value zz_chan_new(int *err);
zz_value zz_chan_send(zz_value chan, zz_value val, int *err);
zz_value zz_chan_recv(zz_value chan, int *err);
zz_value zz_chan_try_recv(zz_value chan, int *err);

// Spawn / task join.
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

// Initialize an arena with a pre-allocated buffer of `cap` bytes.
// The buffer must outlive the arena (typically stack or a single malloc).
void zz_arena_init(zz_arena *a, size_t cap);

// Bump-allocate `size` bytes with `align` alignment from the arena.
// Returns NULL only if the arena is full (caller should fall back to ARC).
void *zz_arena_alloc(zz_arena *a, size_t size, size_t align);

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

// Destroy the arena, freeing its buffer.
void zz_arena_destroy(zz_arena *a);

// ---- thread-safe ARC ---------------------------------------------------
// Atomic reference counting for heap-objects that escape their creating
// scope. Thread-safe via __atomic builtins (no mutex overhead).
//
// All heap-allocated containers (arrays, dicts, funcs) carry an atomic
// refcount. zz_retain/zz_release use atomic increments/decrements.
// When the refcount drops to zero, the object is freed.

// Thread-safe retain: atomically increment the reference count.
void zz_retain_arc(zz_value *v);

// Thread-safe release: atomically decrement the reference count.
// If it reaches zero, free the object and recursively release contained values.
void zz_release_arc(zz_value *v);

// Clone for ARC objects: atomically bump refcount and return a copy.
zz_value zz_clone_arc(zz_value v);

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

zz_value zz_str_new(const char *s, size_t len);
zz_value zz_str_owned(char *s);            // takes ownership
zz_value zz_str_static(const char *s);     // copy of a C literal
zz_value zz_array_new(void);
zz_value zz_dict_new(void);
zz_value zz_range(int64_t start, int64_t end, int64_t step);

// ---- arena-aware constructors -------------------------------------------
// When arena is non-NULL, the object header is bump-allocated on the arena
// (O(1) alloc, freed in bulk at arena reset). When arena is NULL, falls
// back to the standard malloc path (thread-safe ARC).
//
// The items/data buffers ALWAYS use malloc (they may realloc on growth).
// Only the *header structs* (zz_array, zz_dict, zz_str) are arena-eligible.
zz_value zz_array_new_arena(zz_arena *arena);
zz_value zz_dict_new_arena(zz_arena *arena);
zz_value zz_dict_new_arena_sized(zz_arena *arena, size_t hint);
zz_value zz_str_new_arena(const char *s, size_t len, zz_arena *arena);

// Fixed-size array literal constructor. When `arena` is non-NULL, both the
// header and the items buffer (exactly `n` slots) are bump-allocated on the
// arena: `a->items = zz_arena_alloc(...)` pre-allocates capacity so appends
// never hit realloc, and `zz_array_push_lit` stores elements directly
// without `zz_clone` atomic refcount bumps. When `arena` is NULL, the
// standard ARC heap path is used (calloc + one malloc, refs=1).
zz_value zz_array_new_lit(zz_arena *arena, size_t n);

// Store `item` into the next free slot of a fixed-capacity literal array.
// No clone, no realloc, no bounds branch. Only valid on arrays built by
// `zz_array_new_lit` (or newly heap-allocated literals) with spare capacity.
void zz_array_push_lit(zz_array *a, zz_value item);

// ---- refcounting -------------------------------------------------------
void zz_retain(zz_value *v);
void zz_release(zz_value *v);
void zz_assign(zz_value *dst, zz_value src);  // release dst, move src in
zz_value zz_clone(zz_value v);

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

// ---- containers --------------------------------------------------------
void zz_array_push(zz_array *a, zz_value v);
zz_value zz_array_get(const zz_array *a, zz_value idx, int *err);
void zz_array_set(zz_array *a, zz_value idx, zz_value v, int *err);
size_t zz_array_len(const zz_array *a);
zz_value zz_array_slice(const zz_array *a, zz_value start, zz_value end, int *err);
zz_value zz_array_dup(const zz_array *a);

// Index expression support (lowered from `obj[idx]`). Dispatch on the
// object tag at runtime: arrays and dicts. Returns unit + *err=1 on unsupported.
zz_value zz_index_get(zz_value obj, zz_value idx, int *err);
void zz_index_set(zz_value obj, zz_value idx, zz_value item, int *err);

// Slice expression (`obj[a:b]`): arrays (items) and strings (bytes).
zz_value zz_slice_value(zz_value obj, zz_value start, zz_value end, int *err);

void zz_dict_set(zz_dict *d, zz_value key, zz_value val);
zz_value zz_dict_get(const zz_dict *d, zz_value key, int *err);
size_t zz_dict_len(const zz_dict *d);

// ---- calls --------------------------------------------------------------
typedef zz_value (*zz_dispatch_fn)(zz_value *args, size_t argc);

// Build a callable closure value from a generated function pointer. The
// payload stores the function pointer (not a refcounted object).
zz_value zz_closure_make(zz_dispatch_fn f);
// Extract the generated function pointer from a closure value.
zz_dispatch_fn zz_closure_target(zz_value v);

// Higher-order iterators (map/filter/enumerate/zip) — call closures per item.
zz_value zz_iter_map(zz_value items, zz_value f, int *err);
zz_value zz_iter_filter(zz_value items, zz_value f, int *err);
zz_value zz_iter_enumerate(zz_value items, int *err);
zz_value zz_iter_zip(zz_value a, zz_value b, int *err);
zz_value zz_range3(zz_value a, zz_value b, zz_value c, int *err);
// Tuples: display as `(a, b)` (distinct from arrays' `[a, b]`).
zz_value zz_tuple(zz_value a, zz_value b);

zz_value zz_call(zz_value fn, zz_value *args, size_t argc, int *err);
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

// http AOT server — thread-per-connection, returns "OK" for all requests
// All functions follow the native call convention: (zz_value... , int *err)
zz_value zz_http_server(zz_value unused, int *err);  // 0 args → unused=unit
zz_value zz_http_route_get(zz_value server, zz_value path, zz_value handler, int *err);  // 3 args (handler ignored in AOT)
zz_value zz_http_log(zz_value server, zz_value enabled, int *err);  // 2 args
zz_value zz_http_listen(zz_value server, zz_value port, int *err);  // 2 args
zz_value zz_http_handle(zz_value server, zz_value method, zz_value path, zz_value body, int *err);  // 4 args

// Codegen helper shims.
zz_value zz_call_native1(zz_value (*f)(zz_value, int *), zz_value a);
zz_value zz_call_native0(zz_value (*f)(zz_value, int *));
zz_value zz_call_native2(zz_value (*f)(zz_value, zz_value, int *), zz_value a, zz_value b);
zz_value zz_call_native3(zz_value (*f)(zz_value, zz_value, zz_value, int *), zz_value a, zz_value b, zz_value c);
zz_value zz_binop_cat(zz_value a, zz_value b);       // str concat
zz_value zz_binop_cat_str(zz_value a, zz_value b);   // str + Display(b)
// In-place append: reuses *a->s buffer if refs==1 and capacity allows.
// Returns void; *a is mutated. Generated for hot `s = s + literal` loops.
void zz_str_append_str(zz_value *a, zz_value b);
void zz_str_append_lit(zz_value *a, const char *lit, size_t len);
zz_value zz_range_build(zz_value start, zz_value end);
zz_value zz_elvis(zz_value left, zz_value right);

// str functions
zz_value zz_str_trim(zz_value s, int *err);
zz_value zz_str_trim_start(zz_value s, int *err);
zz_value zz_str_trim_end(zz_value s, int *err);
zz_value zz_str_join(zz_value items, zz_value sep, int *err);
zz_value zz_str_split(zz_value s, zz_value sep, int *err);

// env functions
zz_value zz_env_var(zz_value name, int *err);

// json functions
zz_value zz_json_parse(zz_value s, int *err);
zz_value zz_json_stringify(zz_value v, int *err);
zz_value zz_json_null(zz_value unused, int *err);
zz_value zz_json_get(zz_value j, zz_value key, int *err);
zz_value zz_json_as_str(zz_value j, int *err);
zz_value zz_json_as_int(zz_value j, int *err);
zz_value zz_json_as_float(zz_value j, int *err);
zz_value zz_json_as_bool(zz_value j, int *err);

// option/result
zz_value zz_option_expect(zz_value opt, zz_value msg, int *err);
zz_value zz_result_expect(zz_value res, zz_value msg, int *err);

// encoding functions
zz_value zz_encoding_hex_encode(zz_value s, int *err);
zz_value zz_encoding_hex_decode(zz_value s, int *err);

// Variant constructors (Option / Result).
zz_value zz_variant_some(zz_value inner);
zz_value zz_variant_ok(zz_value inner);
zz_value zz_variant_err(zz_value inner);

// Boxed struct constructors and accessors.
zz_value zz_object_new(const char *type_name, zz_value *field_names, size_t n);
void zz_object_set_field(zz_value *obj, const char *name, zz_value val);
zz_value zz_object_get_field(zz_value *obj, const char *name);

// Match extraction helpers.
zz_value zz_match_ok(zz_value v);
zz_value zz_match_err(zz_value v);
zz_value zz_match_some(zz_value v);

// HTTP client stubs (native mode — returns mock responses)
zz_value zz_http_get(zz_value url, zz_value headers, int *err);
zz_value zz_http_post(zz_value url, zz_value body, zz_value headers, int *err);

// ---- formatting --------------------------------------------------------
void zz_print_value(FILE *out, const zz_value *v);
char *zz_value_to_string(const zz_value *v);  // malloc'd
char *zz_to_str_fmt(zz_value v, const char *spec);  // malloc'd

// ---- runtime glue ------------------------------------------------------
// Generated code calls zz_main (top-level statements) then zz_call_main.
int zz_run(void);

// Externs defined by generated code:
extern void zz_main(void);
extern int zz_call_main(void);

#ifdef __cplusplus
}
#endif

#endif // ZZ_RUNTIME_H