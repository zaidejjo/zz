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
// platform.h is concatenated before this header in AOT builds and defines
// ZZ_HAS_CURL; default to 1 when this header is used standalone.
#if !defined(ZZ_HAS_CURL) || ZZ_HAS_CURL
#include <curl/curl.h>
#endif
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
    ZZ_DB,
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

// Refcounted string with Small String Optimization (SSO).
//
// Strings <= 23 bytes are stored inline in `sso` — zero malloc beyond
// the zz_str header.  Longer strings are heap-allocated via a separate
// buffer pointed to by `heap`.
//
// Layout (56 bytes on x86_64):
//   refs     (8)  — reference count. 0 = arena sentinel.
//   interned (4)  — non-zero if permanent singleton.
//   _pad     (4)
//   cap      (8)  — heap buffer capacity. 0 = SSO mode.
//   len      (8)  — payload length in bytes.
//   sso      (24) — inline SSO data (cap == 0).
//   heap     (8)  — pointer to heap buffer (cap > 0, sso unused).
//
// Use ZZ_STR_PTR(s) to get a usable char* in either mode.
#define ZZ_SSO_MAX 23

typedef struct {
    size_t refs;
    int interned;
    size_t cap;     // 0 = SSO, >0 = heap capacity
    size_t len;
    union {
        char sso[ZZ_SSO_MAX + 1]; // 24 bytes: inline data (cap == 0)
        char *heap;                // 8 bytes:  heap pointer  (cap > 0)
    };
} zz_str;

// Get a usable char* pointer for string data in either SSO or heap mode.
// Always inlined: these sit on every string fast path (concat, compare,
// print, hash) and must compile down to a single branchless select.
static inline __attribute__((always_inline)) char *zz_str_ptr(zz_str *s) {
    return s->cap == 0 ? s->sso : s->heap;
}
static inline __attribute__((always_inline)) const char *zz_str_cptr(const zz_str *s) {
    return s->cap == 0 ? s->sso : s->heap;
}

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
        void *db;            // opaque sqlite3* handle (ZZ_DB)
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

// ---- thread-safe channels (lock-free fast path + spillover) ----------
// Two tiers, one FIFO (mirrors the VM's `ChanState`): a Vyukov bounded
// MPMC ring serves all traffic while it fits — pure `__atomic`
// operations, zero locks, zero futex syscalls — and bursts past ring
// capacity spill to the mutex queue below (channels stay unbounded).
// Receivers drain the spill first whenever non-empty, preserving order.
// Sends notify the condvar only when sleepers exist (counted with an
// atomic), so waiter-free traffic pays no futex wake at all.
#define ZZ_RING_CAP 1024
#define ZZ_RING_MASK (ZZ_RING_CAP - 1)

// One ring slot: sequence number + payload, padded to a full cache line
// so adjacent slots never share a line between producer and consumer
// cores. This padding (not the algorithm) is what kills cache-line
// bouncing on the fast path.
typedef struct {
    size_t seq; // __atomic access only (see zz_ring_* below)
    zz_value value; // valid only under the sequence protocol
    char _pad[64 - sizeof(size_t) - sizeof(zz_value)];
} zz_ring_cell;
_Static_assert(sizeof(zz_ring_cell) == 64, "zz_ring_cell must fill a cache line");

struct zz_chan {
    // Fast path: Vyukov ring. Head/tail live on separate cache lines —
    // the producer core writes tail, the consumer core writes head, and
    // neither line is ever read-for-ownership by the other hot loop.
    // ALL accesses via `__atomic` builtins (see below), never plain.
    zz_ring_cell *ring;
    size_t ring_head __attribute__((aligned(64)));
    size_t ring_tail __attribute__((aligned(64)));
    // Slow path: mutex spillover queue + signaling condvar. Unbounded
    // bursts land here; drained before the ring whenever non-empty.
    pthread_mutex_t lock;
    pthread_cond_t  cond;
    zz_value       *queue;   // spill ring buffer of zz_value
    size_t          len;     // current spill items
    size_t          cap;     // spill buffer capacity
    size_t          head;    // next spill read position (mod cap)
    size_t          tail;    // next spill write position (mod cap)
    size_t          sleepers; // __atomic: condvar sleepers in flight
    // Number of values in the spill queue. Written only while holding
    // `lock`; read lock-free by fast paths (Release/Acquire). A zero
    // count means every queued value sits in the ring in FIFO order, so
    // a lock-free ring pop is exactly ordered — no lock needed.
    size_t          spill_count; // __atomic
    // Green waiters (B3): suspended tasks/threads queued for direct
    // handoff on send (no condvar round-trip for task waiters). Sends
    // check the atomic count first and divert to the handoff path while
    // any waiter is queued; receivers register under `lock` after
    // verifying both tiers empty, so no send can slip between the check
    // and the park. Served FIFO before the ring.
    struct zz_green_waiter *gwait_head;
    struct zz_green_waiter *gwait_tail;
    struct zz_green_waiter *gwait_pool; // lock-protected node free-list
    size_t          gwait_pool_n;
    size_t          green_waiters; // __atomic
    // Thread-parked (sync-frame) green waiters. Handoff broadcasts the
    // condvar only when this is non-zero: task waiters requeue without
    // any futex, so the steady state pays zero wakeups.
    size_t          gparked; // __atomic
};

// ---- green tasks: suspendable frames (B3) ------------------------------
// A suspendable frame carries a task's heap cells (locals that must
// survive a suspend/resume round-trip), the resume label id, and the
// handoff value slot. Task frames live on the heap (owned by the task,
// freed by the trampoline on completion); sync-called green closures
// (NULL TLS frame) run on stack-backed frames that vanish with the call.
//
// Generated state-machine closures access cells through frame slots;
// the runtime only moves opaque pointers and the handoff value.
typedef struct {
    void         **cells;      // heap-cell pointers (frame slots)
    unsigned char *cell_kind;  // per-slot ZZ_CELL_VALUE/ZZ_CELL_RAW
    size_t        *cell_size;  // per-slot byte size (RAW cells)
    size_t         ncells;
    int            owns_cells; // task frames: heap arrays (free on completion)
    int            is_task;    // set by the trampoline (suspend returns)
    int            resume;     // resume label id (0 = fresh entry)
    zz_value       value;      // handoff slot (sender deposits here)
    int            has_value;
} zz_task_frame;

// One suspended waiter: either a task (requeue on handoff — no futex)
// or a parked thread (condvar signal). Frames are always the handoff
// target; `fn`/`join` rebuild the task for requeue (core.c-private
// `zz_task_t` layout stays out of this header).
typedef struct zz_green_waiter {
    struct zz_green_waiter *next;
    zz_value                fn;    // task closure (shallow; freed once at completion)
    zz_task_join           *join;  // task join handle
    zz_task_frame          *frame; // handoff target (value lands here)
    int                     is_task;
} zz_green_waiter;

void zz_task_frame_init(zz_task_frame *fr);
zz_task_frame *zz_green_frame(void); // TLS current frame (NULL off-task)
void zz_green_frame_set(zz_task_frame *fr); // sync-call hygiene endpoint
zz_task_frame *zz_frame_new(void);   // heap frame from the B4 pool
void zz_frame_free(zz_task_frame *fr); // release cells; pool/free struct
void zz_sync_frame_cleanup(zz_task_frame *fr); // `cleanup` attr endpoint
// Per-thread suspend verdict (B3): set by green entries on every return
// (1 = this call suspended the task, 0 = value returned). Thread-local
// so a resuming run can never corrupt the suspending run's unwind that
// it legitimately overlaps (ownership already transferred at handoff).
int zz_green_suspended(void);

// Blocking points with green fast paths: hit → value; miss → register
// the frame as a waiter and either suspend (task frames: return to the
// trampoline) or park the thread (sync frames). NULL frame degrades to
// the blocking call (thread parks; always correct).
//
// `resume_id` is recorded into the frame under the same lock that queues
// the waiter — before the waiter becomes visible — so a resuming run can
// never dispatch on a stale id (the handoff that requeues it is ordered
// after the write by that lock).
zz_value zz_chan_recv_green(zz_value chan, zz_task_frame *fr, int resume_id, int *err);
zz_value zz_task_join_recv_green(zz_value join, zz_task_frame *fr, int resume_id, int *err);

// Task join handle for spawned threads.
struct zz_task_join {
    pthread_t       thread;
    zz_value        result;
    int             completed;
    int             consumed;   // set once `task.try_join` takes the result
    pthread_mutex_t lock;
    pthread_cond_t  cond;
    // Green waiters (B3): tasks/threads suspended in `task.join` that
    // must be resumed by direct handoff on completion (no condvar
    // round-trip for task waiters). Served FIFO before the condvar.
    struct zz_green_waiter *gwait_head;
    struct zz_green_waiter *gwait_tail;
    struct zz_green_waiter *gwait_pool; // lock-protected node free-list
    size_t          gwait_pool_n;
    size_t          green_waiters; // __atomic: queued green waiters
    size_t          gparked; // __atomic: thread-parked green waiters (gate broadcasts)
};

// ---- arena allocator ---------------------------------------------------
// A bump allocator for non-escaping local allocations. O(1) alloc, O(1)
// reset. Each function gets its own arena that is initialized on entry and
// reset on exit. Objects allocated here do NOT need individual free calls.
//
// Thread-local: each thread maintains its own arena (no locking needed).
//
// Overflow chunks form a singly-linked list so that zz_arena_destroy can
// free them all in one walk (instead of the old code that leaked or
// free'd the primary buffer prematurely).
typedef struct zz_arena_chunk {
    struct zz_arena_chunk *next;
    size_t cap;
    char buf[];              // flexible array
} zz_arena_chunk;

typedef struct zz_arena {
    char  *buf;              // current contiguous block (primary or overflow)
    size_t cap;              // total capacity in bytes of current block
    size_t offset;           // next free byte position
    zz_arena_chunk *chunks;  // linked list of overflow chunks (for destroy)
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

// ---- refcount fast path ------------------------------------------------
// Unified refcounting inlined: strings use atomic refcounts (plain would
// race when a value crosses threads via channels or spawn captures —
// channel send/recv and spawn/closure paths share strings across threads),
// arrays/dicts/funcs delegate to the out-of-line atomic ARC helpers.
// Inlining removes a call + switch dispatch on every boxed touch
// (clone/assign/index-load) in hot loops; the atomic slow paths stay
// out-of-line in memory.c so hot call sites stay small.
//
// Forward declarations for helpers defined later in the TU.
void zz_retain_array(zz_array *a);
void zz_release_array(zz_array *a);
void zz_retain_dict(zz_dict *d);
void zz_release_dict(zz_dict *d);
void zz_retain_func(zz_func *f);
void zz_release_func(zz_func *f);
void zz_release_variant(zz_value *v);
void zz_release_object(zz_value *v);
void zz_str_header_free(zz_str *s);

static inline void zz_retain(zz_value *v) {
    switch (v->tag) {
    case ZZ_STR:
        if (v->s && !v->s->interned && v->s->refs > 0) {
            __atomic_add_fetch(&v->s->refs, 1, __ATOMIC_RELAXED);
        }
        break;
    case ZZ_ARRAY:
        zz_retain_array(v->arr);
        break;
    case ZZ_DICT:
        zz_retain_dict(v->dict);
        break;
    case ZZ_FUNC:
        zz_retain_func(v->fn);
        break;
    case ZZ_OPTION_SOME:
    case ZZ_RESULT_OK:
    case ZZ_RESULT_ERR:
    case ZZ_JSON:
        if (v->payload) zz_retain(v->payload);
        break;
    default:
        break;
    }
}

static inline void zz_release(zz_value *v) {
    switch (v->tag) {
    case ZZ_STR:
        if (v->s && !v->s->interned) {
            // Arena-allocated strings have refs==0 sentinel — skip free.
            if (v->s->refs == 0) {
                return;
            }
            if (__atomic_sub_fetch(&v->s->refs, 1, __ATOMIC_ACQ_REL) == 0) {
                // SSO strings (cap==0) have no separate heap buffer.
                // Heap strings (cap>0) store data in a separate malloc'd buffer.
                if (v->s->cap > 0) free(v->s->heap);
                zz_str_header_free(v->s);
            }
        }
        break;
    case ZZ_ARRAY:
        zz_release_array(v->arr);
        break;
    case ZZ_DICT:
        zz_release_dict(v->dict);
        break;
    case ZZ_FUNC:
        zz_release_func(v->fn);
        break;
    case ZZ_OPTION_SOME:
    case ZZ_RESULT_OK:
    case ZZ_RESULT_ERR:
    case ZZ_JSON:
        zz_release_variant(v);
        break;
    case ZZ_OBJECT:
        zz_release_object(v);
        break;
    default:
        break;
    }
}

static inline void zz_assign(zz_value *dst, zz_value src) {
    // Release old value if it's a refcounted type.
    if (dst->tag == ZZ_STR || dst->tag == ZZ_ARRAY ||
        dst->tag == ZZ_DICT || dst->tag == ZZ_FUNC || dst->tag == ZZ_OBJECT ||
        dst->tag == ZZ_OPTION_SOME || dst->tag == ZZ_RESULT_OK ||
        dst->tag == ZZ_RESULT_ERR || dst->tag == ZZ_JSON) {
        zz_release(dst);
    }
    *dst = src;
    // Retain the new value for refcounted types.
    if (src.tag == ZZ_ARRAY || src.tag == ZZ_DICT || src.tag == ZZ_FUNC ||
        src.tag == ZZ_OPTION_SOME || src.tag == ZZ_RESULT_OK ||
        src.tag == ZZ_RESULT_ERR || src.tag == ZZ_JSON) {
        zz_retain(dst);
    }
}

static inline zz_value zz_clone(zz_value v) {
    switch (v.tag) {
    case ZZ_STR:
        // The refs>0 guard matters: arena strings (refs==0 sentinel) must
        // never be promoted to heap ownership by a bare bump.
        if (v.s && !v.s->interned && v.s->refs > 0) {
            __atomic_add_fetch(&v.s->refs, 1, __ATOMIC_RELAXED);
        }
        break;
    case ZZ_ARRAY:
        zz_retain_array(v.arr);
        break;
    case ZZ_DICT:
        zz_retain_dict(v.dict);
        break;
    case ZZ_FUNC:
        zz_retain_func(v.fn);
        break;
    case ZZ_OPTION_SOME:
    case ZZ_RESULT_OK:
    case ZZ_RESULT_ERR:
    case ZZ_JSON:
        if (v.payload) zz_retain(v.payload);
        break;
    default:
        break;
    }
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

// ---- calls --------------------------------------------------------------// Closure entry point: args, argc, then the captured environment (array of
// shared heap-cell pointers, one per free variable at the creation site).
typedef zz_value (*zz_dispatch_fn)(zz_value *args, size_t argc, void **env, size_t nenv);

// Build a callable closure value from a generated function pointer. The
// payload stores the function pointer (not a refcounted object).
zz_value zz_closure_make(zz_dispatch_fn f);
// Build a closure value with a captured environment: `cells` (array of
// `nenv` shared-cell pointers) is copied into the heap payload so the
// environment outlives the creating scope. Cells themselves are shared,
// never copied — owner and closures read/write the same storage.
zz_value zz_closure_make_ex(zz_dispatch_fn f, void **cells, size_t nenv);
// Typed variant: `kinds[i]`/`sizes[i]` describe `cells[i]` (`ZZ_CELL_VALUE`
// = boxed `zz_value*` cell, deep-copied by `zz_value_dup`; `ZZ_CELL_RAW` =
// scalar/struct cell of `sizes[i]` bytes, copied with a fresh memcpy).
// NULL `kinds`/`sizes` means every cell is `ZZ_CELL_VALUE`. The rep owns
// its copies of both arrays.
#define ZZ_CELL_VALUE 0
#define ZZ_CELL_RAW 1
zz_value zz_closure_make_ex_typed(
    zz_dispatch_fn f,
    void **cells,
    const unsigned char *kinds,
    const size_t *sizes,
    size_t nenv);
// Extract the generated function pointer from a closure value.
zz_dispatch_fn zz_closure_target(zz_value v);
// Suspendable-frame (B3) constructors: like the plain makers, but the
// rep is flagged green so the trampoline runs it with a task frame and
// `recv`/`join` inside suspend instead of parking the thread.
zz_value zz_closure_make_green(zz_dispatch_fn f);
zz_value zz_closure_make_ex_typed_green(
    zz_dispatch_fn f,
    void **cells,
    const unsigned char *kinds,
    const size_t *sizes,
    size_t nenv);
void zz_closure_set_green(zz_value v);
int zz_closure_is_green(zz_value v);
// Extract the captured environment from a closure value (NULL + 0 when none).
void **zz_closure_env(zz_value v, size_t *nenv);
// Call a closure value with args. Returns unit when `f` is not a closure
// (null target) so unresolvable callees keep the old unit behavior
// instead of crashing.
zz_value zz_call_closure(zz_value f, zz_value *args, size_t argc);
// Raw variant: no TLS frame management (the trampoline manages TLS
// itself around this call).
zz_value zz_call_closure_raw(zz_value f, zz_value *args, size_t argc);
// Publish a task result (trampoline completion path).
void zz_task_join_complete(zz_task_join *join, zz_value result);

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
// Deep copy for thread boundaries (snapshot isolation): heap-owned,
// fully independent of the source (arena pointers healed, closure cells
// duplicated). Handles are shared, never duplicated.
zz_value zz_value_dup(zz_value v);
zz_value zz_chan_new(zz_value unused, int *err);
zz_value zz_chan_send(zz_value chan, zz_value val, int *err);
zz_value zz_chan_recv(zz_value chan, int *err);
zz_value zz_chan_try_recv(zz_value chan, int *err);
zz_value zz_spawn(zz_value fn, int *err);
zz_value zz_task_join_recv(zz_value join, int *err);
zz_value zz_task_try_join(zz_value join, int *err);
// Cooperative safepoint for loop tops (emitted by the AOT lowerer for
// every `for`/`while`). Budget-guarded courtesy yield: lets sibling task
// threads run on quantum expiry. No task switch — C frames cannot suspend
// like VM frames (see executor docs for the VM counterpart).
void zz_safepoint(void);

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

// ---- std.db SQLite ------------------------------------------------------
// Opaque handle: sqlite3* boxed as ZZ_DB (refcounted pointer payload).
// query/exec take a static SQL template (with ?N placeholders) plus
// bound zz_values; binding uses sqlite3_bind_* (never concatenation).
// Native call convention: (args..., int *err) so codegen can route via
// zz_call_nativeN. exec/query pack (db, sql_str, binds_array).
zz_value zz_db_open(zz_value path, int *err);
zz_value zz_db_exec(zz_value db, zz_value sql, zz_value binds, int *err);
zz_value zz_db_query(zz_value db, zz_value sql, zz_value binds, int *err);
zz_value zz_db_close(zz_value db, int *err);
// Low-level FFI used by emit_db_call's inline path (kept for tests).
zz_value zz_db_exec_raw(zz_value db, const char *sql, zz_value *binds, size_t nbinds, int *err);
zz_value zz_db_query_raw(zz_value db, const char *sql, zz_value *binds, size_t nbinds, int *err);

// Transaction error flag — set by zz_db_exec_raw / zz_db_query_raw when
// a statement fails.  Cleared by the inlined transaction prologue.
void zz_tx_set_error(void);
void zz_tx_reset_error(void);
int  zz_tx_has_error(void);

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