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
} zz_tag;

typedef struct zz_value zz_value;

// Container forward declarations.
typedef struct zz_array zz_array;
typedef struct zz_dict zz_dict;
typedef struct zz_dict_entry zz_dict_entry;

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
    };
};

struct zz_array {
    size_t refs;     // atomic reference count (ARC)
    size_t len;
    size_t cap;
    zz_value *items;
};

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

typedef zz_value (*zz_native_fn)(zz_value *args, size_t argc);

struct zz_func {
    size_t refs;
    zz_value fn;      // either ZZ_NATIVE (builtin) or the dispatch table
    zz_value *env;    // captured slot values (SYNTHETIC: extended below)
    size_t env_len;
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

void zz_dict_set(zz_dict *d, zz_value key, zz_value val);
zz_value zz_dict_get(const zz_dict *d, zz_value key, int *err);
size_t zz_dict_len(const zz_dict *d);

// ---- calls --------------------------------------------------------------
zz_value zz_call(zz_value fn, zz_value *args, size_t argc, int *err);
zz_value zz_io_println(zz_value v, int *err);
zz_value zz_io_print(zz_value v, int *err);
zz_value zz_io_input(zz_value prompt, int *err);
zz_value zz_math_pow(zz_value a, zz_value b, int *err);
zz_value zz_time_now_ms(zz_value unused, int *err);

// Codegen helper shims.
zz_value zz_call_native1(zz_value (*f)(zz_value, int *), zz_value a);
zz_value zz_call_native0(zz_value (*f)(zz_value, int *));
zz_value zz_binop_cat(zz_value a, zz_value b);       // str concat
zz_value zz_binop_cat_str(zz_value a, zz_value b);   // str + Display(b)
// In-place append: reuses *a->s buffer if refs==1 and capacity allows.
// Returns void; *a is mutated. Generated for hot `s = s + literal` loops.
void zz_str_append_str(zz_value *a, zz_value b);
void zz_str_append_lit(zz_value *a, const char *lit, size_t len);
zz_value zz_range_build(zz_value start, zz_value end);

// ---- formatting --------------------------------------------------------
void zz_print_value(FILE *out, const zz_value *v);
char *zz_value_to_string(const zz_value *v);  // malloc'd

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