// ZZ native runtime — entry point and core operations.
//
// Truthiness, arithmetic, call machinery, io/math/time/env/fs/
// encoding natives, channels, spawn, TCP, HTTP, and the entry point.
#include "runtime.h"

// ---- truthiness --------------------------------------------------------
bool zz_truthy(zz_value v) {
    switch (v.tag) {
    case ZZ_BOOL:
        return v.b;
    case ZZ_INT:
        return v.i != 0;
    case ZZ_FLOAT:
        return v.f != 0.0;
    case ZZ_STR:
        return v.s->len > 0;
    case ZZ_UNIT:
        return false;
    case ZZ_JSON:
        return v.payload && zz_truthy(*v.payload);
    default:
        return true;
    }
}

// ---- arithmetic --------------------------------------------------------
zz_value zz_neg(zz_value a) {
    if (a.tag == ZZ_INT)
        return zz_int(-a.i);
    if (a.tag == ZZ_FLOAT)
        return zz_float(-a.f);
    return zz_unit();
}

zz_value zz_not(zz_value a) {
    return zz_bool(!zz_truthy(a));
}

static double dpow(double a, double b) {
    if (b == 0)
        return 1;
    double r = 1;
    double base = a;
    int64_t n = (int64_t)b;
    bool neg = n < 0;
    if (neg)
        n = -n;
    while (n > 0) {
        if (n & 1)
            r *= base;
        base *= base;
        n >>= 1;
    }
    return neg ? 1.0 / r : r;
}

zz_value zz_binop(int op, zz_value a, zz_value b) {
    // int fast path
    if (a.tag == ZZ_INT && b.tag == ZZ_INT) {
        switch (op) {
        case ZZOP_ADD:
            return zz_int(a.i + b.i);
        case ZZOP_SUB:
            return zz_int(a.i - b.i);
        case ZZOP_MUL:
            return zz_int(a.i * b.i);
        case ZZOP_DIV:
            if (b.i == 0) {
                fprintf(stderr, "zz error: integer division by zero\n");
                exit(1);
            }
            return zz_int(a.i / b.i);
        case ZZOP_REM:
            if (b.i == 0) {
                fprintf(stderr, "zz error: integer modulo by zero\n");
                exit(1);
            }
            return zz_int(a.i % b.i);
        case ZZOP_POW:
            return zz_int((int64_t)dpow((double)a.i, (double)b.i));
        case ZZOP_EQ:
            return zz_bool(a.i == b.i);
        case ZZOP_NE:
            return zz_bool(a.i != b.i);
        case ZZOP_LT:
            return zz_bool(a.i < b.i);
        case ZZOP_GT:
            return zz_bool(a.i > b.i);
        case ZZOP_LE:
            return zz_bool(a.i <= b.i);
        case ZZOP_GE:
            return zz_bool(a.i >= b.i);
        }
    }
    // float
    if ((a.tag == ZZ_FLOAT || a.tag == ZZ_INT) && (b.tag == ZZ_FLOAT || b.tag == ZZ_INT)) {
        double x = a.tag == ZZ_FLOAT ? a.f : (double)a.i;
        double y = b.tag == ZZ_FLOAT ? b.f : (double)b.i;
        switch (op) {
        case ZZOP_ADD:
            return zz_float(x + y);
        case ZZOP_SUB:
            return zz_float(x - y);
        case ZZOP_MUL:
            return zz_float(x * y);
        case ZZOP_DIV:
            return zz_float(x / y);
        case ZZOP_REM:
            return zz_float(fmod(x, y));
        case ZZOP_POW:
            return zz_float(dpow(x, y));
        case ZZOP_EQ:
            return zz_bool(x == y);
        case ZZOP_NE:
            return zz_bool(x != y);
        case ZZOP_LT:
            return zz_bool(x < y);
        case ZZOP_GT:
            return zz_bool(x > y);
        case ZZOP_LE:
            return zz_bool(x <= y);
        case ZZOP_GE:
            return zz_bool(x >= y);
        }
    }
    // string ops
    if (a.tag == ZZ_STR && b.tag == ZZ_STR) {
        if (op == ZZOP_ADD) {
            zz_str *out = str_alloc(a.s->len + b.s->len);
            memcpy(zz_str_ptr(out), zz_str_cptr(a.s), a.s->len);
            memcpy(zz_str_ptr(out) + a.s->len, zz_str_cptr(b.s), b.s->len);
            zz_value v;
            v.tag = ZZ_STR;
            v.s = out;
            return v;
        }
        int cmp = memcmp(zz_str_cptr(a.s), zz_str_cptr(b.s),
                         a.s->len < b.s->len ? a.s->len : b.s->len);
        // If common prefix matches, shorter string is "less than"
        if (cmp == 0 && a.s->len != b.s->len) {
            cmp = (a.s->len < b.s->len) ? -1 : 1;
        }
        switch (op) {
        case ZZOP_EQ:
            return zz_bool(a.s->len == b.s->len &&
                           memcmp(zz_str_cptr(a.s), zz_str_cptr(b.s), a.s->len) == 0);
        case ZZOP_NE:
            return zz_bool(!(a.s->len == b.s->len &&
                             memcmp(zz_str_cptr(a.s), zz_str_cptr(b.s), a.s->len) == 0));
        case ZZOP_LT:
            return zz_bool(cmp < 0);
        case ZZOP_GT:
            return zz_bool(cmp > 0);
        case ZZOP_LE:
            return zz_bool(cmp <= 0);
        case ZZOP_GE:
            return zz_bool(cmp >= 0);
        default:
            break;
        }
    }
    // bool AND/OR handled by control flow in generated code; comparison
    // fallback:
    if (a.tag == ZZ_BOOL && b.tag == ZZ_BOOL) {
        switch (op) {
        case ZZOP_EQ:
            return zz_bool(a.b == b.b);
        case ZZOP_NE:
            return zz_bool(a.b != b.b);
        default:
            break;
        }
    }
    // JSON equality: compare the wrapped payloads (mirrors VM's Json == Json).
    if (a.tag == ZZ_JSON && b.tag == ZZ_JSON && a.payload && b.payload) {
        if (op == ZZOP_EQ || op == ZZOP_NE) {
            return zz_binop(op, *a.payload, *b.payload);
        }
    }
    return zz_unit();
}

// ---- funcs & calls ------------------------------------------------------
zz_value zz_call(zz_value fn, zz_value *args, size_t argc, int *err) {
    (void)fn; (void)args; (void)argc; (void)err;
    *err = 0;
    if (fn.tag == ZZ_NATIVE) {
        // The fn payload holds a slice table; find by arity later. For now
        // natives are dispatched via the generated switch in the codegen.
        *err = 2; // unsupported direct native call
        return zz_unit();
    }
    *err = 2;
    return zz_unit();
}

// ---- closures -----------------------------------------------------------
// A closure value is a ZZ_NATIVE whose payload points to a heap struct
// holding a generated `zz_dispatch_fn` pointer plus the captured
// environment (array of shared heap-cell pointers). Not refcounted;
// released as a no-op.
//
// Forward: executor task unit (full definition in the executor section
// below). Declared here because trampoline TLS lives alongside closure
// dispatch, ahead of the executor block.
typedef struct zz_task_st zz_task_t;
static __thread zz_task_frame *zz_current_frame = NULL;
static __thread zz_task_t *zz_current_task = NULL;
// Green suspend verdict for the running thread (see header).
static __thread int zz_suspend_verdict = 0;

int zz_green_suspended(void) {
    return zz_suspend_verdict;
}
typedef struct {
    zz_dispatch_fn fn;
    size_t nenv;
    // Per-cell kind (`ZZ_CELL_VALUE`/`ZZ_CELL_RAW`) and byte size for RAW
    // cells. NULL when nenv == 0. Owned by the rep (copied at creation).
    unsigned char *cell_kind;
    size_t *cell_size;
    // B3 suspendable frame: the trampoline runs green closures with a
    // task frame so `recv`/`join` suspend instead of parking the thread.
    int is_green;
    void *env[];
} zz_closure_rep;

zz_value zz_closure_make(zz_dispatch_fn f) {
    zz_closure_rep *rep =
        (zz_closure_rep *)malloc(sizeof(zz_closure_rep));
    if (!rep) return zz_unit();
    rep->fn = f;
    rep->nenv = 0;
    rep->cell_kind = NULL;
    rep->cell_size = NULL;
    rep->is_green = 0;
    zz_value v;
    v.tag = ZZ_NATIVE;
    v.payload = (zz_value *)rep;
    return v;
}

zz_value zz_closure_make_ex(zz_dispatch_fn f, void **cells, size_t nenv) {
    return zz_closure_make_ex_typed(f, cells, NULL, NULL, nenv);
}

zz_value zz_closure_make_ex_typed(
    zz_dispatch_fn f,
    void **cells,
    const unsigned char *kinds,
    const size_t *sizes,
    size_t nenv)
{
    zz_closure_rep *rep = (zz_closure_rep *)malloc(
        sizeof(zz_closure_rep) + nenv * sizeof(void *));
    if (!rep) return zz_unit();
    rep->fn = f;
    rep->nenv = nenv;
    rep->cell_kind = NULL;
    rep->cell_size = NULL;
    rep->is_green = 0;
    if (nenv) {
        rep->cell_kind = (unsigned char *)malloc(nenv * sizeof(unsigned char));
        rep->cell_size = (size_t *)malloc(nenv * sizeof(size_t));
        if (!rep->cell_kind || !rep->cell_size) {
            free(rep->cell_kind);
            free(rep->cell_size);
            free(rep);
            return zz_unit();
        }
        for (size_t i = 0; i < nenv; i++) {
            rep->cell_kind[i] = kinds ? kinds[i] : ZZ_CELL_VALUE;
            rep->cell_size[i] =
                sizes ? sizes[i] : sizeof(zz_value);
        }
        for (size_t i = 0; i < nenv; i++) rep->env[i] = cells[i];
    }
    zz_value v;
    v.tag = ZZ_NATIVE;
    v.payload = (zz_value *)rep;
    return v;
}

// Cell kind/size of a closure value's i-th env slot (defaults: VALUE,
// sizeof(zz_value) — covers reps built before typing existed).
static void zz_closure_cell_info(zz_value v, size_t i,
                                 unsigned char *kind, size_t *size) {
    *kind = ZZ_CELL_VALUE;
    *size = sizeof(zz_value);
    if (v.tag != ZZ_NATIVE || !v.payload) return;
    zz_closure_rep *rep = (zz_closure_rep *)(void *)v.payload;
    if (i >= rep->nenv) return;
    if (rep->cell_kind) *kind = rep->cell_kind[i];
    if (rep->cell_size) *size = rep->cell_size[i];
}

zz_dispatch_fn zz_closure_target(zz_value v) {
    if (v.tag != ZZ_NATIVE || !v.payload) return NULL;
    return ((zz_closure_rep *)(void *)v.payload)->fn;
}

void zz_closure_set_green(zz_value v) {
    if (v.tag != ZZ_NATIVE || !v.payload) return;
    ((zz_closure_rep *)(void *)v.payload)->is_green = 1;
}

int zz_closure_is_green(zz_value v) {
    if (v.tag != ZZ_NATIVE || !v.payload) return 0;
    return ((zz_closure_rep *)(void *)v.payload)->is_green;
}

zz_value zz_closure_make_green(zz_dispatch_fn f) {
    zz_value v = zz_closure_make(f);
    zz_closure_set_green(v);
    return v;
}

zz_value zz_closure_make_ex_typed_green(
    zz_dispatch_fn f,
    void **cells,
    const unsigned char *kinds,
    const size_t *sizes,
    size_t nenv)
{
    zz_value v = zz_closure_make_ex_typed(f, cells, kinds, sizes, nenv);
    zz_closure_set_green(v);
    return v;
}

// Release a closure value's rep: VALUE cells release their duplicated
// values, RAW cells are plain bytes, then the kinds/sizes arrays and the
// rep itself are freed. The old pthread-per-task path leaked all of this
// per spawn (the whole spawn context went unfreed); the M:N executor
// frees it here after each run.
static void zz_closure_release_rep(zz_value v) {
    if (v.tag != ZZ_NATIVE || !v.payload) return;
    zz_closure_rep *rep = (zz_closure_rep *)(void *)v.payload;
    for (size_t i = 0; i < rep->nenv; i++) {
        if (!rep->env[i]) continue;
        unsigned char kind = ZZ_CELL_VALUE;
        if (rep->cell_kind) kind = rep->cell_kind[i];
        if (kind == ZZ_CELL_VALUE) {
            zz_value cellval = *(zz_value *)rep->env[i];
            zz_release(&cellval);
        }
        free(rep->env[i]);
    }
    free(rep->cell_kind);
    free(rep->cell_size);
    free(rep);
}

void **zz_closure_env(zz_value v, size_t *nenv) {
    if (v.tag != ZZ_NATIVE || !v.payload) {
        if (nenv) *nenv = 0;
        return NULL;
    }
    zz_closure_rep *rep = (zz_closure_rep *)(void *)v.payload;
    if (nenv) *nenv = rep->nenv;
    return rep->nenv ? rep->env : NULL;
}

zz_value zz_call_closure_raw(zz_value f, zz_value *args, size_t argc) {
    zz_dispatch_fn fn = zz_closure_target(f);
    if (!fn) return zz_unit();
    size_t nenv = 0;
    void **env = zz_closure_env(f, &nenv);
    return fn(args, argc, env, nenv);
}

zz_value zz_call_closure(zz_value f, zz_value *args, size_t argc) {
    // Sync-call hygiene: a green closure invoked outside the trampoline
    // (first-class calls, map/filter callbacks) must see a NULL frame so
    // its prologue takes the stack-frame + thread-park path. A stale
    // task frame here would corrupt the owning task's resume state.
    zz_task_frame *saved_fr = zz_current_frame;
    zz_task_t *saved_task = zz_current_task;
    zz_current_frame = NULL;
    zz_current_task = NULL;
    zz_value r = zz_call_closure_raw(f, args, argc);
    zz_current_frame = saved_fr;
    zz_current_task = saved_task;
    return r;
}

// ---- green frames (B3 suspendable state) --------------------------------
void zz_task_frame_init(zz_task_frame *fr) {
    fr->cells = NULL;
    fr->cell_kind = NULL;
    fr->cell_size = NULL;
    fr->ncells = 0;
    fr->owns_cells = 0;
    fr->is_task = 0;
    fr->resume = 0;
    fr->value = zz_unit();
    fr->has_value = 0;
}

zz_task_frame *zz_green_frame(void) {
    return zz_current_frame;
}

void zz_green_frame_set(zz_task_frame *fr) {
    zz_current_frame = fr;
}

// B4 pool: heap frames check out of a thread-local free-list (capped —
// overflow frees) instead of hitting malloc per green spawn.
#define ZZ_FRAME_POOL_CAP 64
static __thread zz_task_frame *zz_frame_pool = NULL;
static __thread size_t zz_frame_pool_n = 0;

zz_task_frame *zz_frame_new(void) {
    zz_task_frame *fr = zz_frame_pool;
    if (fr) {
        zz_frame_pool = *(zz_task_frame **)fr; // next link in slot 0
        zz_frame_pool_n--;
    } else {
        fr = (zz_task_frame *)malloc(sizeof(zz_task_frame));
        if (!fr) {
            fprintf(stderr, "zz: out of memory (task frame)\n");
            exit(1);
        }
    }
    zz_task_frame_init(fr);
    return fr;
}

// Cleanup for sync (stack-backed) green frames: runs at scope exit via
// `__attribute__((cleanup))`, including early `return`s. Frees cell
// CONTENTS (heap cells) but never the arrays (caller stack) or the
// struct. Task frames never take this path (trampoline frees them via
// `zz_frame_free` on completion; suspend-returns keep them alive).
// Non-static: invoked from generated code (separate TU) via the
// cleanup attribute on sync-frame storage.
void zz_sync_frame_cleanup(zz_task_frame *fr) {
    if (!fr || fr->is_task) return;
    if (fr->cells) {
        for (size_t i = 0; i < fr->ncells; i++) {
            if (!fr->cells[i]) continue;
            unsigned char kind = ZZ_CELL_VALUE;
            if (fr->cell_kind) kind = fr->cell_kind[i];
            if (kind == ZZ_CELL_VALUE) {
                zz_value cellval = *(zz_value *)fr->cells[i];
                zz_release(&cellval);
            }
            free(fr->cells[i]);
            fr->cells[i] = NULL;
        }
    }
}

void zz_frame_free(zz_task_frame *fr) {
    if (!fr) return;
    // Release cell contents: VALUE cells release their values, RAW cells
    // are plain bytes. Mirrors zz_closure_release_rep discipline.
    if (fr->cells) {
        for (size_t i = 0; i < fr->ncells; i++) {
            if (!fr->cells[i]) continue;
            unsigned char kind = ZZ_CELL_VALUE;
            if (fr->cell_kind) kind = fr->cell_kind[i];
            if (kind == ZZ_CELL_VALUE) {
                zz_value cellval = *(zz_value *)fr->cells[i];
                zz_release(&cellval);
            }
            free(fr->cells[i]);
        }
    }
    if (fr->owns_cells) {
        free(fr->cells);
        free(fr->cell_kind);
        free(fr->cell_size);
    }
    fr->cells = NULL;
    fr->cell_kind = NULL;
    fr->cell_size = NULL;
    fr->ncells = 0;
    fr->owns_cells = 0;
    // Sync (stack-backed) frames vanish with the call — only heap task
    // frames return to the pool. `is_task` marks heap ownership here:
    // task frames are always heap (zz_frame_new), sync frames stack.
    if (fr->is_task && zz_frame_pool_n < ZZ_FRAME_POOL_CAP) {
        *(zz_task_frame **)fr = zz_frame_pool;
        zz_frame_pool = fr;
        zz_frame_pool_n++;
        return;
    }
    if (fr->is_task) {
        free(fr);
    }
}

zz_value zz_io_println(zz_value v, int *err) {
    (void)err;
    zz_print_value(stdout, &v);
    fputc('\n', stdout);
    fflush(stdout);
    return zz_unit();
}

zz_value zz_io_print(zz_value v, int *err) {
    (void)err;
    zz_print_value(stdout, &v);
    return zz_unit();
}

/// `input("prompt")` — print the prompt and read a line from stdin.
/// The prompt string lacks a trailing newline, so stdout must be flushed
/// explicitly or the terminal stays silent while the program blocks on
/// `fgets`.
zz_value zz_io_input(zz_value prompt, int *err) {
    (void)err;
    if (prompt.tag == ZZ_STR) {
        fwrite(zz_str_cptr(prompt.s), 1, prompt.s->len, stdout);
        fflush(stdout);
    }
    char buf[1024];
    if (fgets(buf, sizeof buf, stdin) == NULL) {
        return zz_str_static("");
    }
    // Strip trailing newline (and CR for Windows line endings).
    size_t len = strlen(buf);
    while (len > 0 && (buf[len - 1] == '\n' || buf[len - 1] == '\r')) {
        buf[--len] = '\0';
    }
    return zz_str_new(buf, len);
}

zz_value zz_math_pow(zz_value a, zz_value b, int *err) {
    (void)err;
    // Always return float to match VM behavior.
    double x = a.tag == ZZ_FLOAT ? a.f : (a.tag == ZZ_INT ? (double)a.i : 0.0);
    double y = b.tag == ZZ_FLOAT ? b.f : (b.tag == ZZ_INT ? (double)b.i : 0.0);
    return zz_float(dpow(x, y));
}

/// `time.now_ms()` — monotonic milliseconds since an arbitrary epoch,
/// matching the stdlib native's behavior for elapsed-time measurements.
/// The `zz_value` arg is ignored (native has zero zz-level arguments).
zz_value zz_time_now_ms(zz_value unused, int *err) {
    (void)unused;
    (void)err;
    struct timespec ts;
    clock_gettime(CLOCK_MONOTONIC, &ts);
    int64_t ms = (int64_t)ts.tv_sec * 1000 + ts.tv_nsec / 1000000;
    return zz_int(ms);
}

/// `time.sleep_ms(ms)` — sleep for the given number of milliseconds.
zz_value zz_time_sleep_ms(zz_value ms, int *err) {
    (void)err;
    int64_t m = ms.tag == ZZ_INT ? ms.i : 0;
    if (m > 0) {
        struct timespec ts;
        ts.tv_sec = m / 1000;
        ts.tv_nsec = (m % 1000) * 1000000;
        nanosleep(&ts, NULL);
    }
    return zz_unit();
}
// =====================================================================
//  Deep copy for thread boundaries (snapshot isolation)
//
//  `zz_clone` shares heap objects via refcounts — wrong when a value moves
//  to another thread: sharing the string refcount is racy (now atomic, but
//  sharing arena pointers is fatal — arenas are thread-local), and sharing
//  closure cells aliases mutable state across threads. `zz_value_dup`
//  instead builds a fully independent, heap-owned copy: the receiving
//  thread can free or mutate it without touching the sender's memory.
//  Handles (channels, join handles, sockets, db) are intentionally shared
//  by pointer — they are the communication mechanism itself.
// =====================================================================

// Pointer memo so cyclic values (arrays/dicts/closures referencing
// themselves) terminate and shared subgraphs stay shared within one copy.
typedef struct {
    const void *src;
    zz_value dst;
} zz_dup_entry;

typedef struct {
    zz_dup_entry *items;
    size_t len;
    size_t cap;
} zz_dup_memo;

static int zz_dup_memo_get(zz_dup_memo *m, const void *src, zz_value *out) {
    for (size_t i = 0; i < m->len; i++) {
        if (m->items[i].src == src) {
            *out = m->items[i].dst;
            return 1;
        }
    }
    return 0;
}

static void zz_dup_memo_put(zz_dup_memo *m, const void *src, zz_value dst) {
    if (m->len == m->cap) {
        size_t nc = m->cap == 0 ? 8 : m->cap * 2;
        m->items = (zz_dup_entry *)realloc(m->items, nc * sizeof(zz_dup_entry));
        m->cap = nc;
    }
    m->items[m->len].src = src;
    m->items[m->len].dst = dst;
    m->len++;
}

static zz_value zz_value_dup_inner(zz_value v, zz_dup_memo *m) {
    switch (v.tag) {
    case ZZ_INT:
    case ZZ_FLOAT:
    case ZZ_BOOL:
    case ZZ_UNIT:
        return v;
    case ZZ_STR: {
        if (!v.s) return v;
        // Interned literals are immortal process singletons: share freely.
        if (v.s->interned) return v;
        // Otherwise build an independent heap-owned copy. This also heals
        // arena strings (refs==0 sentinel): the copy is a normal refcounted
        // heap string, safe to free on any thread.
        zz_str *s = str_alloc(v.s->len);
        memcpy(zz_str_ptr(s), zz_str_cptr(v.s), v.s->len);
        zz_str_ptr(s)[v.s->len] = '\0';
        zz_value out;
        out.tag = ZZ_STR;
        out.s = s;
        return out;
    }
    case ZZ_ARRAY: {
        if (!v.arr) return v;
        zz_value hit;
        if (zz_dup_memo_get(m, v.arr, &hit)) return hit;
        zz_value out = zz_array_new();
        // Memoize before recursing so self-referential arrays terminate.
        zz_dup_memo_put(m, v.arr, out);
        for (size_t i = 0; i < v.arr->len; i++) {
            // NOTE: zz_array_push moves (no clone) — ownership transfers.
            zz_array_push(out.arr, zz_value_dup_inner(v.arr->items[i], m));
        }
        return out;
    }
    case ZZ_DICT: {
        if (!v.dict) return v;
        zz_value hit;
        if (zz_dup_memo_get(m, v.dict, &hit)) return hit;
        zz_value out = zz_dict_new();
        zz_dup_memo_put(m, v.dict, out);
        for (size_t i = 0; i < v.dict->len; i++) {
            zz_value k = zz_value_dup_inner(
                (zz_value){ZZ_STR, {.s = v.dict->entries[i].key}}, m);
            zz_value val = zz_value_dup_inner(v.dict->entries[i].val, m);
            // Fresh dict: keys unique, always the new-entry path (moves).
            if (k.tag == ZZ_STR) zz_dict_set(out.dict, k, val);
            else { zz_release(&k); zz_release(&val); }
        }
        return out;
    }
    case ZZ_TUPLE:
    case ZZ_OPTION_SOME:
    case ZZ_RESULT_OK:
    case ZZ_RESULT_ERR:
    case ZZ_JSON: {
        if (!v.payload) return v;
        zz_value *p = (zz_value *)malloc(sizeof(zz_value));
        *p = zz_value_dup_inner(*v.payload, m);
        zz_value out = v;
        out.payload = p;
        return out;
    }
    case ZZ_OBJECT: {
        if (!v.obj) return v;
        zz_value hit;
        if (zz_dup_memo_get(m, v.obj, &hit)) return hit;
        size_t n = v.obj->len;
        zz_object *o = (zz_object *)malloc(sizeof(zz_object) + n * 2 * sizeof(zz_value));
        o->refs = 1;
        o->type_name = v.obj->type_name; // static C string: share
        o->len = n;
        zz_value out;
        out.tag = ZZ_OBJECT;
        out.obj = o;
        zz_dup_memo_put(m, v.obj, out);
        for (size_t i = 0; i < n * 2; i++) {
            o->fields[i] = zz_value_dup_inner(v.obj->fields[i], m);
        }
        return out;
    }
    case ZZ_FUNC: {
        if (!v.fn) return v;
        zz_value hit;
        if (zz_dup_memo_get(m, v.fn, &hit)) return hit;
        zz_func *f = (zz_func *)malloc(sizeof(zz_func));
        f->refs = 1;
        f->fn = zz_value_dup_inner(v.fn->fn, m);
        f->env_len = v.fn->env_len;
        f->env = f->env_len ? (zz_value *)malloc(f->env_len * sizeof(zz_value)) : NULL;
        zz_value out;
        out.tag = ZZ_FUNC;
        out.fn = f;
        zz_dup_memo_put(m, v.fn, out);
        for (size_t i = 0; i < f->env_len; i++) {
            f->env[i] = zz_value_dup_inner(v.fn->env[i], m);
        }
        return out;
    }
    case ZZ_NATIVE: {
        // Closure value: duplicate the rep with FRESH cells holding
        // duplicates of the captured values. VALUE cells deep-copy;
        // RAW cells (unboxed int/bool/double/struct captures) get a
        // fresh cell with the same bytes — the worker's writes stay
        // invisible to the spawner (and vice versa) either way. This is
        // the core of spawn snapshot isolation. Memoized: closures shared
        // between cells stay shared within one copy.
        zz_dispatch_fn fn = zz_closure_target(v);
        if (!fn) return v;
        size_t nenv = 0;
        void **env = zz_closure_env(v, &nenv);
        if (!env || nenv == 0) return zz_closure_make(fn);
        // Memo key: the rep payload (shared closures dedup).
        zz_value hit;
        if (v.payload && zz_dup_memo_get(m, v.payload, &hit)) return hit;
        void **cells = (void **)malloc(nenv * sizeof(void *));
        unsigned char *kinds = (unsigned char *)malloc(nenv);
        size_t *sizes = (size_t *)malloc(nenv * sizeof(size_t));
        if (!cells || !kinds || !sizes) {
            free(cells);
            free(kinds);
            free(sizes);
            return v;
        }
        for (size_t i = 0; i < nenv; i++) {
            unsigned char kind;
            size_t size;
            zz_closure_cell_info(v, i, &kind, &size);
            kinds[i] = kind;
            sizes[i] = size;
            if (kind == ZZ_CELL_RAW) {
                // Unboxed cell: bitwise copy into a fresh cell. (Interior
                // pointers, e.g. refcounted strings inside unboxed struct
                // cells, are shared — documented limitation; never
                // misread as zz_value, which segfaulted.)
                void *cell = malloc(size ? size : 1);
                if (!cell) {
                    for (size_t j = 0; j < i; j++) free(cells[j]);
                    free(cells);
                    free(kinds);
                    free(sizes);
                    return v;
                }
                memcpy(cell, env[i], size);
                cells[i] = cell;
            } else {
                zz_value *cell = (zz_value *)malloc(sizeof(zz_value));
                if (!cell) {
                    for (size_t j = 0; j < i; j++) free(cells[j]);
                    free(cells);
                    free(kinds);
                    free(sizes);
                    return v;
                }
                *cell = zz_value_dup_inner(*(zz_value *)env[i], m);
                cells[i] = cell;
            }
        }
        zz_value out =
            zz_closure_make_ex_typed(fn, cells, kinds, sizes, nenv);
        // Spawn snapshot isolation dups the closure rep: carry the green
        // flag so duplicated tasks keep suspending instead of parking.
        if (v.tag == ZZ_NATIVE && v.payload
            && ((zz_closure_rep *)(void *)v.payload)->is_green) {
            zz_closure_set_green(out);
        }
        free(cells);
        free(kinds);
        free(sizes);
        if (v.payload) zz_dup_memo_put(m, v.payload, out);
        return out;
    }
    default:
        // Handles (chan, task join, tcp, db) and anything else: shared by
        // pointer. They are the communication mechanism, not data.
        return v;
    }
}

// Deep-copy a value for transfer across a thread boundary. See above.
zz_value zz_value_dup(zz_value v) {
    zz_dup_memo m = {0};
    zz_value out = zz_value_dup_inner(v, &m);
    free(m.items);
    return out;
}
// =====================================================================
//  Thread-safe channels (pthread-based)
// =====================================================================

// Forward declarations for the M:N executor below (defined after the
// spawn code): worker-thread flag and park-capacity gate for top-up
// discipline.
static __thread int zz_is_worker;
static void zz_executor_top_up(void);
static size_t zz_executor_parked(void);
// Executor task unit + routing (full executor block lives below; the
// channel handoff paths need these earlier).
typedef struct zz_task_st {
    zz_value fn;
    zz_task_join *join;
    zz_task_frame *frame;
} zz_task_t;
static void zz_enqueue_task(zz_task_t task);
// Green handoff helper (defined in the B3 block below, used by send).
static int zz_chan_handoff_locked(
    zz_chan *ch, zz_value v, zz_task_t *task_out, int *is_task_out);

zz_value zz_chan_new(zz_value unused, int *err) {
    (void)unused;
    (void)err;
    zz_chan *ch = (zz_chan *)malloc(sizeof(zz_chan));
    if (!ch) {
        fprintf(stderr, "zz: out of memory (channel)\n");
        exit(1);
    }
    ch->ring = (zz_ring_cell *)malloc(sizeof(zz_ring_cell) * ZZ_RING_CAP);
    if (!ch->ring) {
        free(ch);
        fprintf(stderr, "zz: out of memory (channel ring)\n");
        exit(1);
    }
    // Cell i starts with sequence i (first lap ready); counters at 0.
    for (size_t i = 0; i < ZZ_RING_CAP; i++) {
        __atomic_store_n(&ch->ring[i].seq, i, __ATOMIC_RELAXED);
    }
    __atomic_store_n(&ch->ring_head, 0, __ATOMIC_RELAXED);
    __atomic_store_n(&ch->ring_tail, 0, __ATOMIC_RELAXED);
    pthread_mutex_init(&ch->lock, NULL);
    pthread_cond_init(&ch->cond, NULL);
    ch->len = 0;
    ch->cap = 16;
    ch->head = 0;
    ch->tail = 0;
    ch->queue = (zz_value *)malloc(sizeof(zz_value) * ch->cap);
    if (!ch->queue) {
        free(ch->ring);
        free(ch);
        fprintf(stderr, "zz: out of memory (channel buffer)\n");
        exit(1);
    }
    __atomic_store_n(&ch->sleepers, 0, __ATOMIC_RELAXED);
    __atomic_store_n(&ch->spill_count, 0, __ATOMIC_RELAXED);
    ch->gwait_head = NULL;
    ch->gwait_tail = NULL;
    ch->gwait_pool = NULL;
    ch->gwait_pool_n = 0;
    __atomic_store_n(&ch->green_waiters, 0, __ATOMIC_RELAXED);
    __atomic_store_n(&ch->gparked, 0, __ATOMIC_RELAXED);
    zz_value v;
    v.tag = ZZ_CHAN;
    v.chan = ch;
    return v;
}

// Vyukov MPMC bounded-ring enqueue, non-blocking. Returns 1 on success
// (ownership of `v` transferred into the cell); 0 when full or contended
// past the retry budget (caller spills to the mutex queue — value NOT
// consumed). Bounded retries: a racing peer usually settles in one or
// two; sustained contention means the slow path is the right call.
static int zz_ring_try_enqueue(zz_chan *ch, zz_value v) {
    for (int attempt = 0; attempt < 4; attempt++) {
        size_t pos = __atomic_load_n(&ch->ring_tail, __ATOMIC_RELAXED);
        zz_ring_cell *cell = &ch->ring[pos & ZZ_RING_MASK];
        // seq == pos: our slot. seq < pos: full. seq > pos: a racing
        // producer claimed ahead — reload and retry.
        size_t seq = __atomic_load_n(&cell->seq, __ATOMIC_ACQUIRE);
        long dif = (long)(seq - pos);
        if (dif == 0) {
            if (__atomic_compare_exchange_n(&ch->ring_tail, &pos, pos + 1, 1,
                                            __ATOMIC_ACQ_REL, __ATOMIC_RELAXED)) {
                // Claimed: exclusive owner until publish (only the CAS
                // winner writes a cell whose seq matched).
                cell->value = v;
                __atomic_store_n(&cell->seq, pos + 1, __ATOMIC_RELEASE);
                return 1;
            }
            // CAS lost: another producer won — retry with fresh pos.
        } else if (dif < 0) {
            return 0;
        }
        // dif > 0: fall through to reload.
    }
    return 0;
}

// Vyukov MPMC bounded-ring dequeue, non-blocking. Returns 1 with the
// value moved into `*out` on success; 0 when empty or contended past
// the retry budget (caller re-checks under the channel mutex before
// parking — no lost wakeups by construction).
static int zz_ring_try_dequeue(zz_chan *ch, zz_value *out) {
    for (int attempt = 0; attempt < 4; attempt++) {
        size_t pos = __atomic_load_n(&ch->ring_head, __ATOMIC_RELAXED);
        zz_ring_cell *cell = &ch->ring[pos & ZZ_RING_MASK];
        // seq == pos+1: value published for us. Less: the producer hasn't
        // published this lap yet (empty). Greater: a racing consumer
        // claimed ahead — reload and retry.
        size_t seq = __atomic_load_n(&cell->seq, __ATOMIC_ACQUIRE);
        long dif = (long)(seq - (pos + 1));
        if (dif == 0) {
            if (__atomic_compare_exchange_n(&ch->ring_head, &pos, pos + 1, 1,
                                            __ATOMIC_ACQ_REL, __ATOMIC_RELAXED)) {
                // Claimed: take ownership, then free the cell for lap
                // pos+CAP (seq skips a full capacity ahead so neither
                // side mistakes laps).
                *out = cell->value;
                __atomic_store_n(&cell->seq, pos + ZZ_RING_CAP, __ATOMIC_RELEASE);
                return 1;
            }
        } else if (dif < 0) {
            return 0;
        }
    }
    return 0;
}

// Estimated ring depth (concurrent producers/consumers make this stale on
// arrival — only for sleep predicates that re-check; never for protocol
// decisions).
static size_t zz_ring_len_estimate(zz_chan *ch) {
    size_t tail = __atomic_load_n(&ch->ring_tail, __ATOMIC_ACQUIRE);
    size_t head = __atomic_load_n(&ch->ring_head, __ATOMIC_ACQUIRE);
    size_t n = tail - head;
    return n > ZZ_RING_CAP ? ZZ_RING_CAP : n;
}

zz_value zz_chan_send(zz_value chan, zz_value val, int *err) {
    if (chan.tag != ZZ_CHAN) { *err = 1; return zz_unit(); }
    zz_chan *ch = chan.chan;
    // The dup happens before publish either way (ownership transfers
    // into the cell/queue, exactly like the old queue push — the
    // receiving thread frees or keeps it without touching the sender's
    // memory).
    zz_value owned = zz_value_dup(val);
    // Single decision point under `lock`: queued green waiters are
    // served first (they registered on empty tiers, so they predate
    // anything we could enqueue), else the value goes to the ring (or
    // the spill when the ring is full).
    //
    // The lock is load-bearing, not just a queue guard: waiter
    // registration (verify-empty + register) runs under this same lock,
    // so the two decisions are mutually ordered — either the waiter
    // sees our value (no suspend) or we see the waiter (handoff). A
    // lock-free pre-check would leave an unordered hole (stale-zero
    // count vs late registration + early publish = orphaned value with
    // a suspended waiter). One uncontended mutex (~20ns) per send is
    // the same trade Go's channels make; receives keep their fully
    // lock-free fast path.
    pthread_mutex_lock(&ch->lock);
    {
        zz_task_t wt;
        int wis_task = 0;
        if (zz_chan_handoff_locked(ch, owned, &wt, &wis_task)) {
            int gparked =
                __atomic_load_n(&ch->gparked, __ATOMIC_ACQUIRE) > 0;
            pthread_mutex_unlock(&ch->lock);
            if (wis_task) zz_enqueue_task(wt);
            if (gparked) {
                pthread_mutex_lock(&ch->lock);
                pthread_cond_broadcast(&ch->cond);
                pthread_mutex_unlock(&ch->lock);
            }
            *err = 0;
            return zz_unit();
        }
    }
    if (zz_ring_try_enqueue(ch, owned)) {
        // Counted sleepers: sends skip the condvar notify when zero, so
        // waiter-free traffic pays no futex wake.
        int wake = __atomic_load_n(&ch->sleepers, __ATOMIC_ACQUIRE) > 0;
        pthread_mutex_unlock(&ch->lock);
        if (wake) {
            pthread_mutex_lock(&ch->lock);
            pthread_cond_signal(&ch->cond);
            pthread_mutex_unlock(&ch->lock);
        }
        *err = 0;
        return zz_unit();
    }
    if (ch->len == ch->cap) {
        size_t new_cap = ch->cap * 2;
        zz_value *new_queue = (zz_value *)malloc(sizeof(zz_value) * new_cap);
        if (!new_queue) {
            pthread_mutex_unlock(&ch->lock);
            *err = 1;
            return zz_unit();
        }
        for (size_t i = 0; i < ch->len; i++) {
            new_queue[i] = ch->queue[(ch->head + i) % ch->cap];
        }
        free(ch->queue);
        ch->queue = new_queue;
        ch->cap = new_cap;
        ch->head = 0;
        ch->tail = ch->len;
    }
    ch->queue[ch->tail] = owned;
    ch->tail = (ch->tail + 1) % ch->cap;
    ch->len++;
    __atomic_add_fetch(&ch->spill_count, 1, __ATOMIC_RELEASE);
    pthread_cond_signal(&ch->cond);
    pthread_mutex_unlock(&ch->lock);
    *err = 0;
    return zz_unit();
}

zz_value zz_chan_recv(zz_value chan, int *err) {
    (void)err;
    if (chan.tag != ZZ_CHAN) { *err = 1; return zz_unit(); }
    zz_chan *ch = chan.chan;
    // Fast path: lock-free ring pop, zero locks. Valid only while the
    // spill is empty — spilled values are older than anything that
    // arrived while the ring was full.
    zz_value v;
    if (__atomic_load_n(&ch->spill_count, __ATOMIC_ACQUIRE) == 0
        && zz_ring_try_dequeue(ch, &v)) {
        return v;
    }
    // Adaptive spin: a rendezvous landing within microseconds (the
    // ping-pong steady state) costs PAUSEs instead of a futex pair.
    // Lock-free ring only, and only while BOTH tiers look empty: when
    // the spill is non-empty the value waits under the mutex — spinning
    // would burn ~10µs before taking the lock (measured 170x slowdown
    // draining a stocked buffer). Bounded (~1024) so a genuinely empty
    // channel still parks promptly.
    if (__atomic_load_n(&ch->spill_count, __ATOMIC_ACQUIRE) == 0) {
        for (int spin = 0; spin < 1024; spin++) {
            if (__atomic_load_n(&ch->spill_count, __ATOMIC_ACQUIRE) != 0) {
                break;
            }
            if (zz_ring_try_dequeue(ch, &v)) {
                return v;
            }
#if defined(__x86_64__) || defined(__i386__)
        __asm__ volatile("pause");
#elif defined(__aarch64__)
        __asm__ volatile("yield");
#else
        sched_yield();
#endif
        }
    }
    pthread_mutex_lock(&ch->lock);
    // Slow loop: spill first (older than anything that arrived while the
    // ring was full), then the ring. A ring miss with both tiers
    // nominally non-empty is transient (a publisher mid-claim) — loop
    // and re-check rather than sleeping on a value already on its way.
    // Counted sleepers: sends skip the condvar notify when this is zero,
    // so waiter-free traffic pays no futex wake. Re-checked after every
    // wake (spurious wakeups and racing consumers just loop).
    //
    // Top-up discipline: only when genuinely about to park (value
    // confirmed absent above — never on mere slow-path entry), at most
    // once per call, and only when no executor worker is already parked
    // to absorb the stranded deque (see zz_executor_top_up's own cap).
    // Unconditional top-up here spawned one immortal 1MB-stack thread
    // per blocking recv — 100k latency round-trips = 100k threads.
    int topped_up = 0;
    for (;;) {
        if (ch->len > 0) {
            // O(1) pop from the spill head — no memmove.
            v = ch->queue[ch->head];
            ch->head = (ch->head + 1) % ch->cap;
            ch->len--;
            __atomic_sub_fetch(&ch->spill_count, 1, __ATOMIC_RELEASE);
            pthread_mutex_unlock(&ch->lock);
            return v;
        }
        if (zz_ring_try_dequeue(ch, &v)) {
            pthread_mutex_unlock(&ch->lock);
            return v;
        }
        if (zz_is_worker && !topped_up) {
            topped_up = 1;
            zz_executor_top_up();
        }
        __atomic_add_fetch(&ch->sleepers, 1, __ATOMIC_ACQ_REL);
        while (ch->len == 0 && zz_ring_len_estimate(ch) == 0) {
            pthread_cond_wait(&ch->cond, &ch->lock);
        }
        __atomic_sub_fetch(&ch->sleepers, 1, __ATOMIC_ACQ_REL);
    }
}

zz_value zz_chan_try_recv(zz_value chan, int *err) {
    if (chan.tag != ZZ_CHAN) { *err = 1; return zz_unit(); }
    zz_chan *ch = chan.chan;
    zz_value v;
    // Fast path: zero spill count means every queued value sits in the
    // ring in FIFO order — a lock-free pop is exactly ordered.
    if (__atomic_load_n(&ch->spill_count, __ATOMIC_ACQUIRE) == 0
        && zz_ring_try_dequeue(ch, &v)) {
        *err = 0;
        return zz_variant_some(v);
    }
    pthread_mutex_lock(&ch->lock);
    // Spill first (older), then the ring — same order as recv.
    if (ch->len > 0) {
        v = ch->queue[ch->head];
        ch->head = (ch->head + 1) % ch->cap;
        ch->len--;
        __atomic_sub_fetch(&ch->spill_count, 1, __ATOMIC_RELEASE);
        pthread_mutex_unlock(&ch->lock);
        *err = 0;
        return zz_variant_some(v);
    }
    if (zz_ring_try_dequeue(ch, &v)) {
        pthread_mutex_unlock(&ch->lock);
        *err = 0;
        return zz_variant_some(v);
    }
    pthread_mutex_unlock(&ch->lock);
    // Match the VM (`chan.try_recv` yields `.none`): return the variant
    // directly instead of signaling through `err` (the call shims drop
    // `err`, which used to turn empty channels into a bare unit).
    *err = 0;
    return (zz_value){ZZ_OPTION_NONE, {.payload = NULL}};
}

// =====================================================================
//  Green channels: suspendable waiters + direct handoff (Phase B3)
// =====================================================================
// A green miss (empty tiers under `lock`) registers the frame as a
// waiter instead of parking an OS thread. Sends serve queued waiters
// first under the same lock (see `zz_chan_send`) and hand the value
// DIRECTLY into the waiter's frame: task waiters requeue onto the
// executor (zero futexes), thread waiters get one broadcast.
//
// Airtightness (no lost wakeup): waiter registration (verify-empty +
// register) and the sender's decision (serve-waiter vs enqueue) both
// run under `ch->lock`, so the two are mutually ordered — either the
// waiter sees the value (no suspend) or the sender sees the waiter
// (handoff). A lock-free send-side pre-check would leave an unordered
// hole (stale-zero count vs late registration + early publish).

#define ZZ_GWAIT_POOL_CAP 32

static zz_green_waiter *zz_gwait_checkout(zz_chan *ch) {
    zz_green_waiter *w = ch->gwait_pool;
    if (w) {
        ch->gwait_pool = w->next;
        ch->gwait_pool_n--;
    } else {
        w = (zz_green_waiter *)malloc(sizeof(zz_green_waiter));
        if (!w) {
            fprintf(stderr, "zz: out of memory (green waiter)\n");
            exit(1);
        }
    }
    return w;
}

static void zz_gwait_recycle(zz_chan *ch, zz_green_waiter *w) {
    if (ch->gwait_pool_n < ZZ_GWAIT_POOL_CAP) {
        w->next = ch->gwait_pool;
        ch->gwait_pool = w;
        ch->gwait_pool_n++;
    } else {
        free(w);
    }
}

// Pop the oldest waiter and deposit an owned value into its frame.
// Caller holds `ch->lock`. Returns 1 when a waiter was served (node
// recycled, count decremented); 0 when the queue is empty. Waking (task
// requeue / condvar broadcast) happens AFTER unlock, by the caller.
static int zz_chan_handoff_locked(
    zz_chan *ch, zz_value v, zz_task_t *task_out, int *is_task_out)
{
    zz_green_waiter *w = ch->gwait_head;
    if (!w) return 0;
    ch->gwait_head = w->next;
    if (!ch->gwait_head) ch->gwait_tail = NULL;
    __atomic_sub_fetch(&ch->green_waiters, 1, __ATOMIC_RELEASE);
    w->frame->value = v;
    w->frame->has_value = 1;
    *is_task_out = w->is_task;
    if (w->is_task) {
        task_out->fn = w->fn;
        task_out->join = w->join;
        task_out->frame = w->frame;
    }
    zz_gwait_recycle(ch, w);
    return 1;
}

// Register the frame as a waiter. Caller holds `ch->lock` and has
// verified both tiers empty. Returns 1 when the caller must suspend
// (task waiter: frame queued, `suspended` set); 0 when the caller must
// park the thread instead (sync frame, or degenerate task state).
static int zz_chan_wait_register(zz_chan *ch, zz_task_frame *fr) {
    zz_green_waiter *w = zz_gwait_checkout(ch);
    w->next = NULL;
    w->frame = fr;
    w->join = NULL;
    w->is_task = 0;
    if (fr->is_task && zz_current_task) {
        w->fn = zz_current_task->fn;
        w->join = zz_current_task->join;
        w->is_task = 1;
    }
    if (ch->gwait_tail) {
        ch->gwait_tail->next = w;
    } else {
        ch->gwait_head = w;
    }
    ch->gwait_tail = w;
    __atomic_add_fetch(&ch->green_waiters, 1, __ATOMIC_ACQ_REL);
    return w->is_task;
}

zz_value zz_chan_recv_green(zz_value chan, zz_task_frame *fr, int resume_id, int *err) {
    // No frame (sync call outside any task): degrade to the blocking
    // call — the thread parks, which is always correct.
    if (!fr) {
        zz_value r = zz_chan_recv(chan, err);
        zz_suspend_verdict = 0;
        return r;
    }
    if (chan.tag != ZZ_CHAN) { *err = 1; return zz_unit(); }
    zz_chan *ch = chan.chan;
    zz_value v;
    // Fast path first (same exactness rule as the blocking call).
    if (__atomic_load_n(&ch->spill_count, __ATOMIC_ACQUIRE) == 0
        && zz_ring_try_dequeue(ch, &v)) {
        *err = 0;
        zz_suspend_verdict = 0;
        return v;
    }
    pthread_mutex_lock(&ch->lock);
    if (ch->len > 0) {
        v = ch->queue[ch->head];
        ch->head = (ch->head + 1) % ch->cap;
        ch->len--;
        __atomic_sub_fetch(&ch->spill_count, 1, __ATOMIC_RELEASE);
        pthread_mutex_unlock(&ch->lock);
        *err = 0;
        zz_suspend_verdict = 0;
        return v;
    }
    if (zz_ring_try_dequeue(ch, &v)) {
        pthread_mutex_unlock(&ch->lock);
        *err = 0;
        zz_suspend_verdict = 0;
        return v;
    }
    // Miss: suspend (task) or park (sync) until a send hands off.
    fr->resume = resume_id;
    if (zz_chan_wait_register(ch, fr)) {
        pthread_mutex_unlock(&ch->lock);
        *err = 0;
        zz_suspend_verdict = 1;
        return zz_unit();
    }
    __atomic_add_fetch(&ch->gparked, 1, __ATOMIC_ACQ_REL);
    while (!fr->has_value) {
        pthread_cond_wait(&ch->cond, &ch->lock);
    }
    __atomic_sub_fetch(&ch->gparked, 1, __ATOMIC_ACQ_REL);
    fr->has_value = 0;
    v = fr->value;
    pthread_mutex_unlock(&ch->lock);
    *err = 0;
    return v;
}

// =====================================================================
//  M:N task executor (Phase B2)
// =====================================================================
// Fixed worker pool (one per CPU) with Chase–Lev work-stealing deques,
// a global injector for main-thread spawns, and park-token sleeping —
// a direct C port of the VM executor's proven design. Spawning enqueues
// (~1µs) instead of creating a pthread (~40µs); tasks multiplex onto a
// few OS threads. Overflow past fixed deque capacity spills to the
// injector (same two-tier spirit as the channel ring).
//
// Workers never block without replacement: any blocking wait on a
// worker thread (channel recv, task join) tops up a replacement worker
// first, so throughput never collapses. Top-up workers carry private
// deques and steal like everyone else, but are never stolen from
// (their work is always theirs to run) — again mirroring the VM.

#define ZZ_DEQUE_CAP 4096
#define ZZ_DEQUE_MASK (ZZ_DEQUE_CAP - 1)

// One unit of work: see the full definition ahead of the channel
// section (needed early by the green handoff paths).
// (B3 suspendable frame `frame`: heap, owned by the task.)

// Chase–Lev work-stealing deque: the owner pushes/pops the bottom
// (lock-free fast path); thieves steal the top via CAS. Fixed capacity
// with injector spillover (never reallocates under thieves — the ABA
// class the VM pool designs avoid by the same rule).
typedef struct {
    zz_task_t *buf;   // ZZ_DEQUE_CAP slots, owner-written
    size_t top;       // __atomic: steal end
    size_t bottom;    // owner end (plain owner access + fences)
} zz_deque_t;

// 1 when running on an executor worker (set per thread): blocking waits
// top up a replacement before parking so throughput never collapses.
static __thread int zz_is_worker = 0;

typedef struct {
    zz_deque_t *deques;     // one per founding worker (fixed at init)
    size_t nworkers;
    // Global injector: main-thread spawns + deque overflow. Mutex-guarded
    // (spawns are rarer than steals; one uncontended lock ~25ns).
    pthread_mutex_t inj_lock;
    pthread_cond_t inj_cond;
    zz_task_t *inj_buf;
    size_t inj_len;
    size_t inj_cap;
    size_t inj_head;
    size_t sleepers;        // __atomic: parked workers (skip wake if 0)
} zz_executor_t;

static zz_executor_t zz_executor;
static pthread_once_t zz_executor_once = PTHREAD_ONCE_INIT;
// This thread's deque (NULL off workers, e.g. the main thread).
static __thread zz_deque_t *zz_local_deque = NULL;
// This thread's steal seed (xorshift64 — no dep, no lock).
static __thread unsigned long long zz_steal_seed = 0;

static unsigned long long zz_next_rand(void) {
    unsigned long long x = zz_steal_seed;
    if (x == 0) x = 0x9E3779B97F4A7C15ULL;
    x ^= x << 13;
    x ^= x >> 7;
    x ^= x << 17;
    zz_steal_seed = x;
    return x;
}

// Owner push: bottom end, no atomics needed (single owner) beyond the
// release store that publishes to thieves.
static void zz_deque_push(zz_deque_t *dq, zz_task_t task) {
    size_t bottom = dq->bottom;
    dq->buf[bottom & ZZ_DEQUE_MASK] = task;
    __atomic_store_n(&dq->bottom, bottom + 1, __ATOMIC_RELEASE);
}

// Owner pop: LIFO hot path. Returns 1 with the task, 0 when empty.
static int zz_deque_pop(zz_deque_t *dq, zz_task_t *out) {
    size_t bottom = dq->bottom;
    if (bottom == 0) return 0;
    bottom--;
    dq->bottom = bottom;
    __atomic_thread_fence(__ATOMIC_SEQ_CST);
    size_t top = __atomic_load_n(&dq->top, __ATOMIC_RELAXED);
    if (top > bottom) {
        // Empty (a thief took the last one): restore and report empty.
        dq->bottom = bottom + 1;
        return 0;
    }
    *out = dq->buf[bottom & ZZ_DEQUE_MASK];
    if (top == bottom) {
        // Single element: race thieves for it via CAS.
        size_t expect = top;
        if (!__atomic_compare_exchange_n(&dq->top, &expect, top + 1, 0,
                                         __ATOMIC_ACQ_REL, __ATOMIC_RELAXED)) {
            // Lost: a thief won it. Restore and report empty.
            dq->bottom = bottom + 1;
            return 0;
        }
    }
    return 1;
}

// Steal one task from the top (FIFO — oldest first, the right victim
// choice: big subtrees migrate, small continuations stay local).
// Returns 1 on success, 0 when empty or on a lost CAS race (caller
// moves to the next victim; the next round retries).
static int zz_deque_steal(zz_deque_t *dq, zz_task_t *out) {
    size_t top = __atomic_load_n(&dq->top, __ATOMIC_ACQUIRE);
    __atomic_thread_fence(__ATOMIC_SEQ_CST);
    size_t bottom = __atomic_load_n(&dq->bottom, __ATOMIC_ACQUIRE);
    if (top >= bottom) return 0;
    *out = dq->buf[top & ZZ_DEQUE_MASK];
    size_t expect = top;
    return __atomic_compare_exchange_n(&dq->top, &expect, top + 1, 0,
                                       __ATOMIC_ACQ_REL, __ATOMIC_RELAXED)
        ? 1
        : 0;
}

// Injector push (main-thread spawns, deque overflow): one mutex, plus a
// sleeper wake when someone sleeps (skipped entirely when nobody does).
static void zz_injector_push(zz_task_t task) {
    zz_executor_t *ex = &zz_executor;
    pthread_mutex_lock(&ex->inj_lock);
    if (ex->inj_len == ex->inj_cap) {
        size_t new_cap = ex->inj_cap == 0 ? 64 : ex->inj_cap * 2;
        zz_task_t *nb =
            (zz_task_t *)realloc(ex->inj_buf, new_cap * sizeof(zz_task_t));
        if (!nb) {
            fprintf(stderr, "zz: out of memory (injector)\n");
            exit(1);
        }
        // Linearize any wrap before growing.
        if (ex->inj_head != 0) {
            zz_task_t *tmp =
                (zz_task_t *)malloc(ex->inj_len * sizeof(zz_task_t));
            if (!tmp) {
                fprintf(stderr, "zz: out of memory (injector)\n");
                exit(1);
            }
            for (size_t i = 0; i < ex->inj_len; i++) {
                tmp[i] = nb[(ex->inj_head + i) % ex->inj_cap];
            }
            for (size_t i = 0; i < ex->inj_len; i++) nb[i] = tmp[i];
            free(tmp);
        }
        ex->inj_buf = nb;
        ex->inj_cap = new_cap;
        ex->inj_head = 0;
    }
    ex->inj_buf[(ex->inj_head + ex->inj_len) % ex->inj_cap] = task;
    ex->inj_len++;
    // Wake one sleeper per push (round-robin thundering is pointless for
    // a single task); skipped when nobody sleeps.
    int wake = __atomic_load_n(&ex->sleepers, __ATOMIC_ACQUIRE) > 0;
    pthread_mutex_unlock(&ex->inj_lock);
    if (wake) {
        pthread_mutex_lock(&ex->inj_lock);
        pthread_cond_signal(&ex->inj_cond);
        pthread_mutex_unlock(&ex->inj_lock);
    }
}

// Injector pop-from-front for stealers. Returns 1 on success.
static int zz_injector_steal(zz_task_t *out) {
    zz_executor_t *ex = &zz_executor;
    int got = 0;
    pthread_mutex_lock(&ex->inj_lock);
    if (ex->inj_len > 0) {
        *out = ex->inj_buf[ex->inj_head];
        ex->inj_head = (ex->inj_head + 1) % ex->inj_cap;
        ex->inj_len--;
        got = 1;
    }
    pthread_mutex_unlock(&ex->inj_lock);
    return got;
}

// Route a task: the current worker's local deque (LIFO warmth — the
// thread that made progress very likely runs it next) when on a worker,
// else the global injector. Deque overflow spills to the injector.
static void zz_enqueue_task(zz_task_t task) {
    if (zz_local_deque != NULL) {
        zz_deque_t *dq = zz_local_deque;
        size_t bottom = dq->bottom;
        size_t top = __atomic_load_n(&dq->top, __ATOMIC_ACQUIRE);
        if (bottom - top < ZZ_DEQUE_CAP) {
            zz_deque_push(dq, task);
            // A parked worker never sees a local push (it sleeps on the
            // injector condvar) — wake one so the task can't strand on
            // this deque while its owner is parked. Skipped when nobody
            // sleeps (the hot steady state): one atomic load. The signal
            // goes out under `inj_lock` so it is airtight against the
            // park path's verify-then-wait (same discipline as the
            // injector push below): publish happens-before the lock, and
            // the parker verifies every sibling deque while holding it.
            if (__atomic_load_n(&zz_executor.sleepers, __ATOMIC_ACQUIRE)
                > 0) {
                pthread_mutex_lock(&zz_executor.inj_lock);
                pthread_cond_signal(&zz_executor.inj_cond);
                pthread_mutex_unlock(&zz_executor.inj_lock);
            }
            return;
        }
        // Full: spill to the injector (rare — 4096 deep per worker).
    }
    zz_injector_push(task);
}

// One steal round over founding-worker siblings only (lock-free CAS;
// never touches the injector lock — safe to call while holding
// `inj_lock`, unlike the full round below).
static int zz_steal_siblings(zz_task_t *out) {
    zz_executor_t *ex = &zz_executor;
    size_t n = ex->nworkers;
    if (n == 0) return 0;
    size_t start = (size_t)(zz_next_rand() % n);
    for (size_t k = 0; k < n; k++) {
        if (zz_deque_steal(&ex->deques[(start + k) % n], out)) return 1;
    }
    return 0;
}

// One steal round: random-victim siblings first (cache-hot
// continuations stay local), then the injector (fresh bursts).
// Takes `inj_lock` via the injector — NEVER call while holding it.
static int zz_steal_round(zz_task_t *out) {
    if (zz_steal_siblings(out)) return 1;
    return zz_injector_steal(out);
}

// Run one task: call the closure (Unit-seated, like the VM worker
// setup), publish the outcome, free the context.
// Trampoline: run one task to completion OR to its first suspend point.
// Green closures execute with the task frame on TLS; a suspend returns
// through the closure (frame queued in a channel/join waiter list) and
// the worker immediately continues with other work — no thread parked,
// no top-up needed. Completion publishes the result (serving green join
// waiters by direct handoff first), then frees the closure snapshot and
// the frame.
void zz_task_join_complete(zz_task_join *join, zz_value result);

static void zz_run_task(zz_task_t task) {
    zz_value unit_arg = zz_unit();
    zz_value args[1] = {unit_arg};
    int green =
        task.fn.tag == ZZ_NATIVE && zz_closure_is_green(task.fn);
    if (!green) {
        // Hygiene: a stale task frame must not leak into sync-called
        // green closures reachable from this non-green frame (they are
        // invoked through `zz_call_closure`, which clears TLS — this
        // belt-and-braces NULL keeps the raw trampoline path identical).
        zz_task_frame *saved_fr = zz_current_frame;
        zz_task_t *saved_task = zz_current_task;
        zz_current_frame = NULL;
        zz_current_task = NULL;
        zz_value result = zz_call_closure_raw(task.fn, args, 1);
        zz_current_frame = saved_fr;
        zz_current_task = saved_task;
        zz_task_join_complete(task.join, result);
        // Free the dup'd closure snapshot (owned by this execution).
        if (task.fn.tag == ZZ_NATIVE) {
            zz_closure_release_rep(task.fn);
        } else {
            zz_release(&task.fn);
        }
        return;
    }
    if (!task.frame) task.frame = zz_frame_new();
    zz_task_frame *fr = task.frame;
    fr->is_task = 1;
    zz_task_frame *saved_fr = zz_current_frame;
    zz_task_t *saved_task = zz_current_task;
    zz_current_frame = fr;
    zz_current_task = &task;
    zz_value result = zz_call_closure_raw(task.fn, args, 1);
    zz_current_frame = saved_fr;
    zz_current_task = saved_task;
    // Verdict is thread-local: a resuming run on another thread cannot
    // corrupt this run's unwind that it legitimately overlaps (the
    // waiter queue owns the continuation from handoff on).
    if (zz_green_suspended()) {
        // Suspended: the waiter queue holds a task copy (closure + join
        // + frame) that resumes with the handed-off value. This copy
        // owns nothing — the resuming run completes and frees.
        return;
    }
    zz_task_join_complete(task.join, result);
    if (task.fn.tag == ZZ_NATIVE) {
        zz_closure_release_rep(task.fn);
    } else {
        zz_release(&task.fn);
    }
    zz_frame_free(task.frame);
}

// Publish a task result and serve green join waiters by direct handoff
// (FIFO, shallow value copies — same convention as the blocking path's
// multi-recv aliasing). Old-style condvar sleepers get one signal.
void zz_task_join_complete(zz_task_join *join, zz_value result) {
    pthread_mutex_lock(&join->lock);
    join->result = result;
    join->completed = 1;
    zz_green_waiter *w = join->gwait_head;
    join->gwait_head = NULL;
    join->gwait_tail = NULL;
    __atomic_store_n(&join->green_waiters, 0, __ATOMIC_RELEASE);
    // Recycle waiter nodes now (lock-protected pool); requeue + wake
    // after unlock to keep the critical section short.
    zz_task_t *tasks = NULL;
    size_t ntasks = 0;
    size_t cap = 0;
    while (w) {
        zz_green_waiter *next = w->next;
        w->frame->value = result;
        w->frame->has_value = 1;
        if (w->is_task) {
            if (ntasks == cap) {
                size_t ncap = cap == 0 ? 4 : cap * 2;
                zz_task_t *nb =
                    (zz_task_t *)realloc(tasks, ncap * sizeof(zz_task_t));
                if (!nb) {
                    fprintf(stderr, "zz: out of memory (join handoff)\n");
                    exit(1);
                }
                tasks = nb;
                cap = ncap;
            }
            tasks[ntasks].fn = w->fn;
            tasks[ntasks].join = w->join;
            tasks[ntasks].frame = w->frame;
            ntasks++;
        }
        if (join->gwait_pool_n < ZZ_GWAIT_POOL_CAP) {
            w->next = join->gwait_pool;
            join->gwait_pool = w;
            join->gwait_pool_n++;
        } else {
            free(w);
        }
        w = next;
    }
    pthread_cond_signal(&join->cond);
    pthread_mutex_unlock(&join->lock);
    for (size_t i = 0; i < ntasks; i++) zz_enqueue_task(tasks[i]);
    free(tasks);
    // Thread-parked green waiters (sync frames) sleep on this same
    // condvar with predicate `fr->has_value`: one signal may not reach
    // them all — broadcast to be exact, but only when any exist
    // (completion is one-shot, so this runs once per join handle).
    if (__atomic_load_n(&join->gparked, __ATOMIC_ACQUIRE) > 0) {
        pthread_mutex_lock(&join->lock);
        pthread_cond_broadcast(&join->cond);
        pthread_mutex_unlock(&join->lock);
    }
}

// Park more executor capacity: a replacement worker with a private
// deque. Used when a worker must block (channel recv, task join) so
// throughput never collapses. The top-up deque is never stolen from —
// its work is always its own — but it steals from everyone else.
//
// Two gates keep this bounded (unbounded top-up = one immortal
// 1MB-stack thread per blocking wait; the 100k-round-trip latency
// probe alone spawned ~100k threads / ~440MB RSS):
//   1. Parked workers absorb first: if any executor worker is already
//      parked on the injector, it will steal the stranded deque's work
//      — no replacement needed.
//   2. Hard cap: extra workers beyond 4x CPU (clamped to [32, 256])
//      are refused; the blocker then just parks and throughput degrades
//      gracefully instead of OOMing. B3 state machines remove the need
//      for top-up entirely (tasks suspend instead of parking threads).
static size_t zz_topup_count; // __atomic: replacement workers spawned
static size_t zz_topup_cap;   // set once at executor init
static void *zz_worker_loop(void *arg);

static size_t zz_executor_parked(void) {
    return __atomic_load_n(&zz_executor.sleepers, __ATOMIC_ACQUIRE);
}

static void zz_executor_top_up(void) {
    if (zz_executor_parked() > 0) {
        return;
    }
    size_t had =
        __atomic_fetch_add(&zz_topup_count, 1, __ATOMIC_ACQ_REL);
    if (had >= zz_topup_cap) {
        __atomic_sub_fetch(&zz_topup_count, 1, __ATOMIC_ACQ_REL);
        return;
    }
    zz_deque_t *dq = (zz_deque_t *)calloc(1, sizeof(zz_deque_t));
    if (!dq) {
        __atomic_sub_fetch(&zz_topup_count, 1, __ATOMIC_ACQ_REL);
        return;
    }
    dq->buf = (zz_task_t *)calloc(ZZ_DEQUE_CAP, sizeof(zz_task_t));
    if (!dq->buf) {
        free(dq);
        __atomic_sub_fetch(&zz_topup_count, 1, __ATOMIC_ACQ_REL);
        return;
    }
    pthread_t thread;
    pthread_attr_t attr;
    pthread_attr_init(&attr);
    pthread_attr_setstacksize(&attr, 1024 * 1024);
    if (pthread_create(&thread, &attr, zz_worker_loop, dq) == 0) {
        pthread_detach(thread);
    } else {
        free(dq->buf);
        free(dq);
        __atomic_sub_fetch(&zz_topup_count, 1, __ATOMIC_ACQ_REL);
    }
    pthread_attr_destroy(&attr);
}

static void *zz_worker_loop(void *arg) {
    zz_deque_t *dq = (zz_deque_t *)arg;
    zz_executor_t *ex = &zz_executor;
    zz_local_deque = dq;
    zz_is_worker = 1;
    zz_steal_seed = (unsigned long long)(uintptr_t)dq ^ 0x9E3779B97F4A7C15ULL;
    for (;;) {
        zz_task_t task;
        // Hot spin while work flows: local pop every iteration; sibling
        // + injector steals every 16th. A full steal round hammers shared
        // atomics (and the injector mutex); doing it per PAUSE starves
        // the very threads whose progress we are waiting for. Bounded
        // (~4096 iterations) then park — no CPU burn at rest.
        int found = 0;
        for (int spin = 0; spin < 4096; spin++) {
            if (zz_deque_pop(dq, &task)) {
                found = 1;
                break;
            }
            // Full steal round (incl. injector mutex) every 16th
            // iteration; sibling-only steals between. A full round per
            // PAUSE hammers shared atomics and starves the very threads
            // whose progress we are waiting for.
            int got = ((spin & 15) == 0) ? zz_steal_round(&task)
                                         : zz_steal_siblings(&task);
            if (got) {
                found = 1;
                break;
            }
#if defined(__x86_64__) || defined(__i386__)
            __asm__ volatile("pause");
#elif defined(__aarch64__)
            __asm__ volatile("yield");
#else
            sched_yield();
#endif
        }
        if (found) {
            zz_run_task(task);
            continue;
        }
        // Cold: announce sleep, re-verify, then sleep on the condvar.
        // Lock ordering: the re-verify below NEVER takes `inj_lock`
        // (sibling steals are pure CAS; the injector is read directly
        // under the held lock). The old code called the full steal round
        // while holding `inj_lock` — self-deadlock on first idle.
        pthread_mutex_lock(&ex->inj_lock);
        __atomic_add_fetch(&ex->sleepers, 1, __ATOMIC_ACQ_REL);
        for (;;) {
            if (zz_deque_pop(dq, &task) || zz_steal_siblings(&task)) {
                break;
            }
            if (ex->inj_len > 0) {
                task = ex->inj_buf[ex->inj_head];
                ex->inj_head = (ex->inj_head + 1) % ex->inj_cap;
                ex->inj_len--;
                break;
            }
            // Announce-then-verify: a push landing between the last steal
            // and this park signals the condvar (sleepers > 0), so no
            // wakeup is lost by construction.
            pthread_cond_wait(&ex->inj_cond, &ex->inj_lock);
        }
        pthread_mutex_unlock(&ex->inj_lock);
        __atomic_sub_fetch(&ex->sleepers, 1, __ATOMIC_ACQ_REL);
        zz_run_task(task);
    }
    return NULL;
}

static void zz_executor_init(void);
static int get_cpu_count(void);

static void zz_executor_init(void) {
    zz_executor_t *ex = &zz_executor;
#ifdef ZZ_OS_WINDOWS
    size_t n = 4;
#else
    int cpus = get_cpu_count();
    size_t n = cpus > 0 ? (size_t)cpus : 4;
#endif
    if (n < 2) n = 2;
    ex->nworkers = n;
    ex->deques = (zz_deque_t *)calloc(n, sizeof(zz_deque_t));
    if (!ex->deques) {
        fprintf(stderr, "zz: out of memory (executor)\n");
        exit(1);
    }
    for (size_t i = 0; i < n; i++) {
        ex->deques[i].buf =
            (zz_task_t *)calloc(ZZ_DEQUE_CAP, sizeof(zz_task_t));
        if (!ex->deques[i].buf) {
            fprintf(stderr, "zz: out of memory (executor)\n");
            exit(1);
        }
    }
    pthread_mutex_init(&ex->inj_lock, NULL);
    pthread_cond_init(&ex->inj_cond, NULL);
    ex->inj_buf = NULL;
    ex->inj_len = 0;
    ex->inj_cap = 0;
    ex->inj_head = 0;
    __atomic_store_n(&ex->sleepers, 0, __ATOMIC_RELAXED);
    // Top-up cap: 4x founding workers, clamped to [32, 256].
    __atomic_store_n(&zz_topup_count, 0, __ATOMIC_RELAXED);
    size_t cap = n * 4;
    if (cap < 32) cap = 32;
    if (cap > 256) cap = 256;
    __atomic_store_n(&zz_topup_cap, cap, __ATOMIC_RELAXED);
    pthread_attr_t attr;
    pthread_attr_init(&attr);
    pthread_attr_setstacksize(&attr, 1024 * 1024);
    for (size_t i = 0; i < n; i++) {
        pthread_t thread;
        if (pthread_create(&thread, &attr, zz_worker_loop,
                           &ex->deques[i]) != 0) {
            fprintf(stderr, "zz: out of memory (executor thread)\n");
            exit(1);
        }
        pthread_detach(thread);
    }
    pthread_attr_destroy(&attr);
}

zz_value zz_spawn(zz_value fn, int *err) {
    // Closures lower to ZZ_NATIVE (zz_closure_make); ZZ_FUNC is the
    // legacy named-fn box. Accept both.
    if (fn.tag != ZZ_FUNC && fn.tag != ZZ_NATIVE) { *err = 1; return zz_unit(); }
    // M:N executor (Phase B2): enqueue instead of creating a pthread.
    // Lazily initialized once; workers are detached and live for the
    // process (same lifetime discipline as the old detached threads).
    pthread_once(&zz_executor_once, zz_executor_init);
    // Create task join handle.
    zz_task_join *join = (zz_task_join *)malloc(sizeof(zz_task_join));
    if (!join) {
        fprintf(stderr, "zz: out of memory (task join)\n");
        exit(1);
    }
    pthread_mutex_init(&join->lock, NULL);
    pthread_cond_init(&join->cond, NULL);
    join->result = zz_unit();
    join->completed = 0;
    join->consumed = 0;
    join->gwait_head = NULL;
    join->gwait_tail = NULL;
    join->gwait_pool = NULL;
    join->gwait_pool_n = 0;
    __atomic_store_n(&join->green_waiters, 0, __ATOMIC_RELAXED);
    __atomic_store_n(&join->gparked, 0, __ATOMIC_RELAXED);
    zz_task_t task;
    task.fn = zz_value_dup(fn);  // Independent copy for the worker.
    task.join = join;
    task.frame = NULL;  // Lazily allocated by the trampoline (green only).
    zz_enqueue_task(task);
    zz_value v;
    v.tag = ZZ_TASK_JOIN;
    v.task = join;
    return v;
}

zz_value zz_task_join_recv_green(zz_value join_val, zz_task_frame *fr, int resume_id, int *err) {
    // No frame (sync call outside any task): degrade to the blocking
    // call — the thread parks, which is always correct.
    if (!fr) {
        zz_value r = zz_task_join_recv(join_val, err);
        zz_suspend_verdict = 0;
        return r;
    }
    if (join_val.tag != ZZ_TASK_JOIN) { *err = 1; return zz_unit(); }
    zz_task_join *join = join_val.task;
    pthread_mutex_lock(&join->lock);
    if (join->completed) {
        zz_value result = join->result;
        pthread_mutex_unlock(&join->lock);
        *err = 0;
        zz_suspend_verdict = 0;
        return result;
    }
    // Miss: register as green waiter (checkout from the lock-protected
    // pool, FIFO append, counted).
    zz_green_waiter *w = join->gwait_pool;
    if (w) {
        join->gwait_pool = w->next;
        join->gwait_pool_n--;
    } else {
        w = (zz_green_waiter *)malloc(sizeof(zz_green_waiter));
        if (!w) {
            fprintf(stderr, "zz: out of memory (green waiter)\n");
            exit(1);
        }
    }
    w->next = NULL;
    w->frame = fr;
    w->join = NULL;
    w->is_task = 0;
    if (fr->is_task && zz_current_task) {
        w->fn = zz_current_task->fn;
        w->join = zz_current_task->join;
        w->is_task = 1;
    }
    if (join->gwait_tail) {
        join->gwait_tail->next = w;
    } else {
        join->gwait_head = w;
    }
    join->gwait_tail = w;
    __atomic_add_fetch(&join->green_waiters, 1, __ATOMIC_ACQ_REL);
    fr->resume = resume_id;
    if (w->is_task) {
        pthread_mutex_unlock(&join->lock);
        *err = 0;
        zz_suspend_verdict = 1;
        return zz_unit();
    }
    __atomic_add_fetch(&join->gparked, 1, __ATOMIC_ACQ_REL);
    while (!fr->has_value) {
        pthread_cond_wait(&join->cond, &join->lock);
    }
    __atomic_sub_fetch(&join->gparked, 1, __ATOMIC_ACQ_REL);
    fr->has_value = 0;
    zz_value result = fr->value;
    pthread_mutex_unlock(&join->lock);
    *err = 0;
    zz_suspend_verdict = 0;
    return result;
}

zz_value zz_task_join_recv(zz_value join_val, int *err) {
    if (join_val.tag != ZZ_TASK_JOIN) { *err = 1; return zz_unit(); }
    zz_task_join *join = join_val.task;
    pthread_mutex_lock(&join->lock);
    // Same top-up discipline as channel recv: only when genuinely about
    // to park (!completed), at most once; the parked-worker gate + hard
    // cap inside zz_executor_top_up bound the total.
    if (!join->completed && zz_is_worker) {
        zz_executor_top_up();
    }
    while (!join->completed) {
        pthread_cond_wait(&join->cond, &join->lock);
    }
    zz_value result = join->result;
    pthread_mutex_unlock(&join->lock);
    // Note: join handle is intentionally not freed here to allow multiple recv.
    // The handle is leaked at process exit (acceptable for now).
    *err = 0;
    return result;
}

// `task.try_join(handle)` — non-blocking check: `.some(result)` when the
// task finished (consuming, like the VM), `.none` while it is still running.
// Never blocks, never touches `err` (the call shims drop it).
zz_value zz_task_try_join(zz_value join_val, int *err) {
    (void)err;
    if (join_val.tag != ZZ_TASK_JOIN) {
        return (zz_value){ZZ_OPTION_NONE, {.payload = NULL}};
    }
    zz_task_join *join = join_val.task;
    pthread_mutex_lock(&join->lock);
    if (!join->completed || join->consumed) {
        pthread_mutex_unlock(&join->lock);
        return (zz_value){ZZ_OPTION_NONE, {.payload = NULL}};
    }
    join->consumed = 1;
    zz_value result = zz_clone(join->result);
    pthread_mutex_unlock(&join->lock);
    return zz_variant_some(result);
}

// ---- cooperative safepoints ----------------------------------------------
// Called at AOT loop tops. Budget-guarded: one thread-local counter
// decrement per iteration, clock read once per 1024. On quantum expiry
// (1ms of looping without blocking) yields the OS thread so sibling task
// threads get scheduled — the AOT answer to the VM executor's Timeslice
// requeue (C frames cannot suspend, so this is a courtesy yield, not a
// task switch). Steady-state cost ~0.02ns/iter.
#ifndef ZZ_OS_WINDOWS
#include <sched.h>
#endif

static __thread unsigned zz_sp_budget = 1024;
static __thread long long zz_sp_start_ns = 0;

void zz_safepoint(void) {
    if (zz_sp_budget != 0) { zz_sp_budget--; return; }
    zz_sp_budget = 1024;
    struct timespec now;
    clock_gettime(CLOCK_MONOTONIC, &now);
    long long now_ns = (long long)now.tv_sec * 1000000000LL + now.tv_nsec;
    if (zz_sp_start_ns == 0) { zz_sp_start_ns = now_ns; return; }
    if (now_ns - zz_sp_start_ns >= 1000000LL) {
        zz_sp_start_ns = now_ns;
#ifdef ZZ_OS_WINDOWS
        SwitchToThread();
#else
        sched_yield();
#endif
    }
}

// ---- http AOT server -------------------------------------------------------

// Minimal HTTP server for AOT mode: thread-per-connection, fixed "OK" response.
// Route handlers are not supported in AOT (no interpreter to call closures).

#include <pthread.h>
// platform.h is concatenated before this TU and defines ZZ_OS_WINDOWS on
// Windows targets, where Winsock replaces the POSIX socket headers.
// Linux/macOS keep the existing POSIX set (epoll is Linux-only; other
// targets use the poll/select paths below).
#ifdef ZZ_OS_WINDOWS
#include <winsock2.h>
#include <ws2tcpip.h>
#include <windows.h>
#else
#include <sys/socket.h>
#include <netinet/in.h>
#include <netinet/tcp.h>
#include <arpa/inet.h>
#include <unistd.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <strings.h>
#include <errno.h>
#include <signal.h>
#ifdef __linux__
#include <sys/epoll.h>
#include <sys/syscall.h>
#include <sched.h>
#endif
#include <fcntl.h>
#include <sys/types.h>
#include <sys/wait.h>
#include <poll.h>
#endif

// One-time network startup (WSAStartup on Windows, no-op on POSIX).
// Called lazily from the TCP natives so binaries pay nothing when the
// net module is unused.
#ifdef ZZ_OS_WINDOWS
static int zz_net_started = 0;
void zz_net_init(void) {
    if (!zz_net_started) {
        WSADATA wsa;
        if (WSAStartup(MAKEWORD(2, 2), &wsa) == 0) zz_net_started = 1;
    }
}
#endif

struct zz_tcp {
    int fd;
    int closed;
};

// Resolve "host:port" to a sockaddr_in. Returns 0 on success.
static int tcp_resolve(const char *hostport, struct sockaddr_in *out) {
    const char *colon = strrchr(hostport, ':');
    if (!colon) return -1;
    char host[256];
    size_t hl = (size_t)(colon - hostport);
    if (hl >= sizeof host) return -1;
    memcpy(host, hostport, hl);
    host[hl] = '\0';
    int port = atoi(colon + 1);
    if (port <= 0 || port > 65535) return -1;
    if (strcmp(host, "localhost") == 0) {
        strcpy(host, "127.0.0.1");
    }
    memset(out, 0, sizeof *out);
    out->sin_family = AF_INET;
    out->sin_port = htons((uint16_t)port);
    return inet_pton(AF_INET, host, &out->sin_addr) == 1 ? 0 : -1;
}

static zz_tcp *tcp_alloc(int fd) {
    zz_tcp *t = (zz_tcp *)malloc(sizeof(zz_tcp));
    t->fd = fd;
    t->closed = 0;
    return t;
}

// net.tcp_listen(addr) → Result<Ok(listener), Err(msg)>
zz_value zz_tcp_listen(zz_value addr, int *err) {
    (void)err;
    if (addr.tag != ZZ_STR) return zz_variant_err(zz_str_static("tcp_listen: expected a string"));
    struct sockaddr_in sa;
    if (tcp_resolve(zz_str_cptr(addr.s), &sa) != 0) {
        char buf[192];
        int n = snprintf(buf, sizeof buf, "tcp_listen failed: invalid address `%s`", zz_str_cptr(addr.s));
        return zz_variant_err(zz_str_owned(copy_cstr(buf, (size_t)n)));
    }
    int fd = socket(AF_INET, SOCK_STREAM, 0);
    if (fd < 0) return zz_variant_err(zz_str_static("tcp_listen failed: socket"));
    int one = 1;
    setsockopt(fd, SOL_SOCKET, SO_REUSEADDR, &one, sizeof one);
    if (bind(fd, (struct sockaddr *)&sa, sizeof sa) != 0) {
        char buf[192];
        int n = snprintf(buf, sizeof buf, "tcp_listen failed: %s", strerror(errno));
        close(fd);
        return zz_variant_err(zz_str_owned(copy_cstr(buf, (size_t)n)));
    }
    if (listen(fd, 16) != 0) {
        close(fd);
        return zz_variant_err(zz_str_static("tcp_listen failed: listen"));
    }
    return zz_variant_ok((zz_value){ZZ_TCP_LISTENER, {.net = tcp_alloc(fd)}});
}

// net.tcp_connect(addr, timeout_ms) → Result<Ok(stream), Err(msg)>
zz_value zz_tcp_connect(zz_value addr, zz_value timeout_ms, int *err) {
    (void)err;
    if (addr.tag != ZZ_STR) return zz_variant_err(zz_str_static("tcp_connect: expected a string"));
    struct sockaddr_in sa;
    if (tcp_resolve(zz_str_cptr(addr.s), &sa) != 0) {
        char buf[192];
        int n = snprintf(buf, sizeof buf, "invalid address: `%s`", zz_str_cptr(addr.s));
        return zz_variant_err(zz_str_owned(copy_cstr(buf, (size_t)n)));
    }
    int fd = socket(AF_INET, SOCK_STREAM, 0);
    if (fd < 0) return zz_variant_err(zz_str_static("tcp_connect failed: socket"));
    long toms = timeout_ms.tag == ZZ_INT ? (long)timeout_ms.i : 5000;
    // Non-blocking connect so the timeout is honored.
    int flags = fcntl(fd, F_GETFL, 0);
    fcntl(fd, F_SETFL, flags | O_NONBLOCK);
    int rc = connect(fd, (struct sockaddr *)&sa, sizeof sa);
    if (rc != 0 && errno != EINPROGRESS) {
        char buf[192];
        int n = snprintf(buf, sizeof buf, "tcp_connect failed: %s", strerror(errno));
        close(fd);
        return zz_variant_err(zz_str_owned(copy_cstr(buf, (size_t)n)));
    }
    if (rc != 0) {
        struct pollfd pfd = { fd, POLLOUT, 0 };
        int pr = poll(&pfd, 1, (int)toms);
        if (pr <= 0) {
            close(fd);
            return zz_variant_err(zz_str_static("tcp_connect failed: timed out"));
        }
        int soerr = 0;
        socklen_t slen = sizeof soerr;
        getsockopt(fd, SOL_SOCKET, SO_ERROR, &soerr, &slen);
        if (soerr != 0) {
            char buf[192];
            int n = snprintf(buf, sizeof buf, "tcp_connect failed: %s", strerror(soerr));
            close(fd);
            return zz_variant_err(zz_str_owned(copy_cstr(buf, (size_t)n)));
        }
    }
    fcntl(fd, F_SETFL, flags);
    return zz_variant_ok((zz_value){ZZ_TCP_STREAM, {.net = tcp_alloc(fd)}});
}

// net.tcp_accept(listener) → Result<Ok(stream), Err(msg)>
zz_value zz_tcp_accept(zz_value listener, int *err) {
    (void)err;
    if (listener.tag != ZZ_TCP_LISTENER || !listener.net || listener.net->closed) {
        return zz_variant_err(zz_str_static("tcp_accept failed: not a listener"));
    }
    int cfd = accept(listener.net->fd, NULL, NULL);
    if (cfd < 0) {
        char buf[192];
        int n = snprintf(buf, sizeof buf, "tcp_accept failed: %s", strerror(errno));
        return zz_variant_err(zz_str_owned(copy_cstr(buf, (size_t)n)));
    }
    return zz_variant_ok((zz_value){ZZ_TCP_STREAM, {.net = tcp_alloc(cfd)}});
}

// net.tcp_write(stream, data) → Result<Ok(byte_count), Err(msg)>
zz_value zz_tcp_write(zz_value stream, zz_value data, int *err) {
    (void)err;
    if (stream.tag != ZZ_TCP_STREAM || !stream.net || stream.net->closed) {
        return zz_variant_err(zz_str_static("tcp_write failed: not a stream"));
    }
    if (data.tag != ZZ_STR) return zz_variant_err(zz_str_static("tcp_write failed: expected a string"));
    size_t total = 0;
    while (total < data.s->len) {
        ssize_t w = send(stream.net->fd, zz_str_cptr(data.s) + total, data.s->len - total, 0);
        if (w <= 0) {
            char buf[192];
            int n = snprintf(buf, sizeof buf, "tcp_write failed: %s", strerror(errno));
            return zz_variant_err(zz_str_owned(copy_cstr(buf, (size_t)n)));
        }
        total += (size_t)w;
    }
    return zz_variant_ok((zz_value){ZZ_INT, {.i = (int64_t)total}});
}

// net.tcp_read(stream, max_bytes) → Result<Ok(str), Err(msg)>
zz_value zz_tcp_read(zz_value stream, zz_value max_bytes, int *err) {
    (void)err;
    if (stream.tag != ZZ_TCP_STREAM || !stream.net || stream.net->closed) {
        return zz_variant_err(zz_str_static("tcp_read failed: not a stream"));
    }
    size_t cap = max_bytes.tag == ZZ_INT && max_bytes.i > 0 ? (size_t)max_bytes.i : 1024;
    char *buf = (char *)malloc(cap);
    ssize_t n = recv(stream.net->fd, buf, cap, 0);
    if (n < 0) {
        free(buf);
        if (errno == EAGAIN || errno == EWOULDBLOCK) {
            return zz_variant_err(zz_str_static("tcp_read failed: timed out"));
        }
        char msg[192];
        int m = snprintf(msg, sizeof msg, "tcp_read failed: %s", strerror(errno));
        return zz_variant_err(zz_str_owned(copy_cstr(msg, (size_t)m)));
    }
    if (n == 0) {
        free(buf);
        return zz_variant_err(zz_str_static("tcp_read failed: connection closed"));
    }
    return zz_variant_ok(zz_str_owned(copy_cstr(buf, (size_t)n)));
}

// net.tcp_readline(stream) → Result<Ok(line without '\n'), Err(msg)>
zz_value zz_tcp_readline(zz_value stream, int *err) {
    (void)err;
    if (stream.tag != ZZ_TCP_STREAM || !stream.net || stream.net->closed) {
        return zz_variant_err(zz_str_static("tcp_readline failed: not a stream"));
    }
    SB sb = {0};
    char b;
    for (;;) {
        ssize_t n = recv(stream.net->fd, &b, 1, 0);
        if (n <= 0) {
            if (sb.len > 0) break;  // EOF after partial line
            char *msg = sb_take(&sb);
            free(msg);
            if (n < 0) return zz_variant_err(zz_str_static("tcp_readline failed"));
            return zz_variant_err(zz_str_static("connection closed"));
        }
        if (b == '\n') break;
        sb_str(&sb, &b, 1);
    }
    return zz_variant_ok(zz_str_owned(sb_take(&sb)));
}

// net.tcp_close(stream) → Result<Ok(true), Err(msg)> (idempotent)
zz_value zz_tcp_close(zz_value stream, int *err) {
    (void)err;
    if ((stream.tag == ZZ_TCP_STREAM || stream.tag == ZZ_TCP_LISTENER) && stream.net && !stream.net->closed) {
        close(stream.net->fd);
        stream.net->closed = 1;
    }
    return zz_variant_ok((zz_value){ZZ_BOOL, {.b = true}});
}

static zz_value tcp_addr(zz_value stream, int peer, int *err) {
    (void)err;
    if (stream.tag != ZZ_TCP_STREAM || !stream.net || stream.net->closed) {
        return zz_variant_err(zz_str_static("addr_failed"));
    }
    struct sockaddr_in sa;
    socklen_t slen = sizeof sa;
    if (peer ? getpeername(stream.net->fd, (struct sockaddr *)&sa, &slen) != 0
             : getsockname(stream.net->fd, (struct sockaddr *)&sa, &slen) != 0) {
        char msg[192];
        int m = snprintf(msg, sizeof msg, "%s failed: %s", peer ? "peer_addr" : "local_addr", strerror(errno));
        return zz_variant_err(zz_str_owned(copy_cstr(msg, (size_t)m)));
    }
    char ip[INET_ADDRSTRLEN];
    inet_ntop(AF_INET, &sa.sin_addr, ip, sizeof ip);
    char buf[64];
    int n = snprintf(buf, sizeof buf, "%s:%d", ip, ntohs(sa.sin_port));
    return zz_variant_ok(zz_str_owned(copy_cstr(buf, (size_t)n)));
}

zz_value zz_tcp_peer_addr(zz_value stream, int *err) { return tcp_addr(stream, 1, err); }
zz_value zz_tcp_local_addr(zz_value stream, int *err) { return tcp_addr(stream, 0, err); }

// net.set_read_timeout / set_write_timeout → Result<Ok(true), Err(msg)>
zz_value zz_tcp_set_read_timeout(zz_value stream, zz_value ms, int *err) {
    if (stream.tag != ZZ_TCP_STREAM || !stream.net || stream.net->closed) {
        return zz_variant_err(zz_str_static("set_read_timeout failed"));
    }
    struct timeval tv;
    tv.tv_sec = (ms.tag == ZZ_INT ? ms.i : 0) / 1000;
    tv.tv_usec = (ms.tag == ZZ_INT ? ms.i : 0) % 1000 * 1000;
    if (setsockopt(stream.net->fd, SOL_SOCKET, SO_RCVTIMEO, &tv, sizeof tv) != 0) {
        return zz_variant_err(zz_str_static("set_read_timeout failed"));
    }
    return zz_variant_ok((zz_value){ZZ_BOOL, {.b = true}});
}

zz_value zz_tcp_set_write_timeout(zz_value stream, zz_value ms, int *err) {
    if (stream.tag != ZZ_TCP_STREAM || !stream.net || stream.net->closed) {
        return zz_variant_err(zz_str_static("set_write_timeout failed"));
    }
    struct timeval tv;
    tv.tv_sec = (ms.tag == ZZ_INT ? ms.i : 0) / 1000;
    tv.tv_usec = (ms.tag == ZZ_INT ? ms.i : 0) % 1000 * 1000;
    if (setsockopt(stream.net->fd, SOL_SOCKET, SO_SNDTIMEO, &tv, sizeof tv) != 0) {
        return zz_variant_err(zz_str_static("set_write_timeout failed"));
    }
    return zz_variant_ok((zz_value){ZZ_BOOL, {.b = true}});
}

// =====================================================================
//  std.db SQLite — prepared-statement FFI (zero-alloc binding path)
// =====================================================================
//
//  Contract (mirrors the rusqlite natives in zz_stdlib):
//    - `zz_db_open(path)` → ZZ_DB handle (sqlite3* boxed, :memory: ok).
//    - `zz_db_exec(db, sql, binds, n)` → int rows-changed. Uses
//      sqlite3_prepare_v2 + sqlite3_bind_* + sqlite3_step; the SQL
//      template is a static C string with ?N placeholders, bound values
//      arrive as zz_values — never string-concatenated.
//    - `zz_db_query(db, sql, binds, n)` → array of row dicts
//      (positional c0..cN keys; struct mapping happens at the ZZ
//      layer via field order).
//    - `zz_db_close(db)` → unit (also closed at process exit).
#ifdef ZZ_HAS_SQLITE3
#include <sqlite3.h>
#endif

zz_value zz_db_open(zz_value path, int *err) {
    (void)err;
    if (path.tag != ZZ_STR || !path.s) return (zz_value){ZZ_DB, {.db = NULL}};
    const char *p = zz_str_cptr(path.s);
#ifdef ZZ_HAS_SQLITE3
    sqlite3 *conn = NULL;
    int rc;
    if (strcmp(p, ":memory:") == 0) rc = sqlite3_open(":memory:", &conn);
    else {
        /* Strip optional `sqlite://` prefix (scheme routing). */
        const char *sp = (strncmp(p, "sqlite://", 9) == 0) ? p + 9 : p;
        if (sp[0] == '\0') rc = sqlite3_open(":memory:", &conn);
        else rc = sqlite3_open(sp, &conn);
    }
    if (rc != SQLITE_OK) {
        if (conn) sqlite3_close(conn);
        return (zz_value){ZZ_DB, {.db = NULL}};
    }
    return (zz_value){ZZ_DB, {.db = (void *)conn}};
#else
    (void)p;
    return (zz_value){ZZ_DB, {.db = NULL}};
#endif
}

static int zz_db_bind_all(
#ifdef ZZ_HAS_SQLITE3
    sqlite3_stmt *st,
#endif
    zz_value *binds, size_t nbinds, int *err) {
    (void)err;
    for (size_t i = 0; i < nbinds; i++) {
        zz_value v = binds[i];
#ifdef ZZ_HAS_SQLITE3
        int idx = (int)i + 1;
        int rc = SQLITE_OK;
        switch (v.tag) {
        case ZZ_INT: rc = sqlite3_bind_int64(st, idx, v.i); break;
        case ZZ_FLOAT: rc = sqlite3_bind_double(st, idx, v.f); break;
        case ZZ_BOOL: rc = sqlite3_bind_int(st, idx, v.b ? 1 : 0); break;
        case ZZ_STR:
            rc = v.s ? sqlite3_bind_text(st, idx, zz_str_cptr(v.s), (int)v.s->len, SQLITE_TRANSIENT) : sqlite3_bind_null(st, idx);
            break;
        case ZZ_OPTION_NONE: rc = sqlite3_bind_null(st, idx); break;
        case ZZ_OPTION_SOME:
            if (v.payload) {
                zz_value inner = *v.payload;
                if (inner.tag == ZZ_INT) rc = sqlite3_bind_int64(st, idx, inner.i);
                else if (inner.tag == ZZ_FLOAT) rc = sqlite3_bind_double(st, idx, inner.f);
                else if (inner.tag == ZZ_BOOL) rc = sqlite3_bind_int(st, idx, inner.b ? 1 : 0);
                else if (inner.tag == ZZ_STR && inner.s) rc = sqlite3_bind_text(st, idx, zz_str_cptr(inner.s), (int)inner.s->len, SQLITE_TRANSIENT);
                else rc = sqlite3_bind_null(st, idx);
            } else rc = sqlite3_bind_null(st, idx);
            break;
        default: rc = sqlite3_bind_null(st, idx); break;
        }
        if (rc != SQLITE_OK) return rc;
#else
        (void)v;
#endif
    }
    return 0;
}

// ---- transaction error flag --------------------------------------------
static int _zz_tx_error = 0;
void zz_tx_set_error(void)   { _zz_tx_error = 1; }
void zz_tx_reset_error(void) { _zz_tx_error = 0; }
int  zz_tx_has_error(void)   { return _zz_tx_error; }

zz_value zz_db_exec_raw(zz_value db, const char *sql, zz_value *binds, size_t nbinds, int *err) {
    (void)err;
    if (db.tag != ZZ_DB || !db.db || !sql) return zz_int(0);
#ifdef ZZ_HAS_SQLITE3
    sqlite3 *conn = (sqlite3 *)db.db;
    sqlite3_stmt *st = NULL;
    if (sqlite3_prepare_v2(conn, sql, -1, &st, NULL) != SQLITE_OK) return zz_int(0);
    if (zz_db_bind_all(st, binds, nbinds, err) != SQLITE_OK) { sqlite3_finalize(st); return zz_int(0); }
    int rc = sqlite3_step(st);
    int changed = sqlite3_changes(conn);
    sqlite3_finalize(st);
    if (rc != SQLITE_DONE && rc != SQLITE_ROW) { zz_tx_set_error(); return zz_int(0); }
    return zz_int((int64_t)changed);
#else
    (void)binds; (void)nbinds;
    return zz_int(0);
#endif
}

zz_value zz_db_query_raw(zz_value db, const char *sql, zz_value *binds, size_t nbinds, int *err) {
    (void)err;
    zz_value out = zz_array_new();
    if (db.tag != ZZ_DB || !db.db || !sql) return out;
#ifdef ZZ_HAS_SQLITE3
    sqlite3 *conn = (sqlite3 *)db.db;
    sqlite3_stmt *st = NULL;
    if (sqlite3_prepare_v2(conn, sql, -1, &st, NULL) != SQLITE_OK) return out;
    if (zz_db_bind_all(st, binds, nbinds, err) != SQLITE_OK) { sqlite3_finalize(st); return out; }
    int ncol = sqlite3_column_count(st);
    int rc;
    while ((rc = sqlite3_step(st)) == SQLITE_ROW) {
        zz_value row = zz_dict_new();
        for (int i = 0; i < ncol; i++) {
            /* Use the real SQL column name so ZZ struct field access
               (e.g. users[0].id) works in AOT mode.  Fall back to
               the positional "cN" form if the name is unavailable. */
            const char *cname = sqlite3_column_name(st, i);
            char fallback[32];
            if (!cname || !cname[0]) {
                snprintf(fallback, sizeof fallback, "c%d", i);
                cname = fallback;
            }
            zz_value val;
            switch (sqlite3_column_type(st, i)) {
            case SQLITE_INTEGER: val = zz_int(sqlite3_column_int64(st, i)); break;
            case SQLITE_FLOAT: val = zz_float(sqlite3_column_double(st, i)); break;
            case SQLITE_TEXT: {
                const unsigned char *t = sqlite3_column_text(st, i);
                int n = sqlite3_column_bytes(st, i);
                val = zz_str_owned(copy_cstr((const char *)t, (size_t)(n < 0 ? 0 : n)));
                break;
            }
            case SQLITE_NULL: val = (zz_value){ZZ_OPTION_NONE, {.payload = NULL}}; break;
            default: {
                const unsigned char *t = sqlite3_column_text(st, i);
                int n = sqlite3_column_bytes(st, i);
                if (t) val = zz_str_owned(copy_cstr((const char *)t, (size_t)(n < 0 ? 0 : n)));
                else val = (zz_value){ZZ_OPTION_NONE, {.payload = NULL}};
                break;
            }
            }
            int derr = 0;
            zz_value k = zz_str_owned(copy_cstr(cname, strlen(cname)));
            zz_index_set(row, k, val, &derr);
            (void)derr;
        }
        int aerr = 0;
        out = zz_vec_push(out, row, &aerr);
        (void)aerr;
    }
    sqlite3_finalize(st);
#else
    (void)binds; (void)nbinds;
#endif
    return out;
}

zz_value zz_db_close(zz_value db, int *err) {
    (void)err;
#ifdef ZZ_HAS_SQLITE3
    if (db.tag == ZZ_DB && db.db) sqlite3_close((sqlite3 *)db.db);
#else
    (void)db;
#endif
    return zz_unit();
}

// Native-convention wrappers: (db, sql_str, binds_array, err).
// Unpack the binds array into a C slice, then delegate to the raw FFI.
zz_value zz_db_exec(zz_value db, zz_value sql, zz_value binds, int *err) {
    const char *tmpl = (sql.tag == ZZ_STR && sql.s) ? zz_str_cptr(sql.s) : "";
    zz_value *items = NULL;
    size_t n = 0;
    if (binds.tag == ZZ_ARRAY && binds.arr) {
        items = binds.arr->items;
        n = binds.arr->len;
    }
    return zz_db_exec_raw(db, tmpl, items, n, err);
}

zz_value zz_db_query(zz_value db, zz_value sql, zz_value binds, int *err) {
    const char *tmpl = (sql.tag == ZZ_STR && sql.s) ? zz_str_cptr(sql.s) : "";
    zz_value *items = NULL;
    size_t n = 0;
    if (binds.tag == ZZ_ARRAY && binds.arr) {
        items = binds.arr->items;
        n = binds.arr->len;
    }
    return zz_db_query_raw(db, tmpl, items, n, err);
}

// =====================================================================
//  Epoll HTTP Server — SO_REUSEPORT Multi-Core Event Loop
// =====================================================================
//
//  Architecture:
//    - SO_REUSEPORT: multiple forked workers share the same port
//    - Each worker: own epoll_create1() + epoll_wait() loop
//    - Level-triggered EPOLLIN/EPOLLOUT (not edge-triggered)
//    - Pre-allocated Connection array — no malloc per request
//
//  Connection state machine:
//    CONN_CONNECTED → CONN_READING → CONN_PARSED → CONN_WRITING → CONN_KEEP_ALIVE/CONN_CLOSED

#define MAX_CONNECTIONS 1024
#define READ_BUF_SIZE   8192
#define WRITE_BUF_SIZE  16384
#define MAX_EVENTS      64

typedef enum {
    CONN_CONNECTED  = 0,
    CONN_READING    = 1,
    CONN_PARSED     = 2,
    CONN_WRITING    = 3,
    CONN_KEEP_ALIVE = 4,
    CONN_CLOSED     = 5,
} ConnState;

typedef struct {
    int     fd;
    int     state;
    char    read_buf[READ_BUF_SIZE];
    char    write_buf[WRITE_BUF_SIZE];
    int     read_pos;
    int     write_pos;
    int     response_len;
    int     keep_alive;
} Connection;

static Connection g_connections[MAX_CONNECTIONS];
static int g_http_server_running = 0;

// ---- Connection slot management (no malloc) ----

static int find_free_slot(void) {
    for (int i = 0; i < MAX_CONNECTIONS; i++) {
        if (g_connections[i].fd == -1) return i;
    }
    return -1;
}

static Connection* alloc_connection(int fd) {
    int idx = find_free_slot();
    if (idx < 0) return NULL;
    Connection *c = &g_connections[idx];
    c->fd = fd;
    c->state = CONN_READING;  // Start in READING state
    c->read_pos = 0;
    c->write_pos = 0;
    c->response_len = 0;
    c->keep_alive = 0;
    return c;
}

static void free_connection(Connection *c) {
    if (c->fd >= 0) {
        // fprintf(stderr, "closing fd=%d\n", c->fd);
        close(c->fd);
        c->fd = -1;
    }
    c->state = CONN_CLOSED;
    c->read_pos = 0;
    c->write_pos = 0;
    c->response_len = 0;
}

static void init_connections(void) {
    for (int i = 0; i < MAX_CONNECTIONS; i++) {
        g_connections[i].fd = -1;
        g_connections[i].state = CONN_CLOSED;
        g_connections[i].read_pos = 0;
        g_connections[i].write_pos = 0;
        g_connections[i].response_len = 0;
        g_connections[i].keep_alive = 0;
    }
}

// ---- Non-blocking helpers ----

static void set_nonblock(int fd) {
    int flags = fcntl(fd, F_GETFL, 0);
    if (flags >= 0) fcntl(fd, F_SETFL, flags | O_NONBLOCK);
}

// ---- HTTP parsing ----

static int parse_request_headers(Connection *c) {
    // Find \r\n\r\n — end of headers
    if (c->read_pos < 4) return 0;
    void *end = memmem(c->read_buf, c->read_pos, "\r\n\r\n", 4);
    if (!end) return 0;

    // HTTP/1.1 defaults to keep-alive, only disable if "close" is present
    c->keep_alive = 1;

    // Find "Connection:" header and check value
    char *headers_end = (char *)end;
    for (char *p = c->read_buf; p < headers_end - 12; p++) {
        // Look for start of a header line (preceded by \r\n)
        if (p > c->read_buf && p[-1] == '\n' && p[0] == '\r') {
            p++; // skip the \r, now at start of header name
            // Skip leading whitespace
            while (*p == ' ' || *p == '\t') p++;
            // Check for "Connection:" (case-insensitive)
            if (strncasecmp(p, "connection:", 11) == 0) {
                p += 11; // skip "connection:"
                // Skip whitespace
                while (*p == ' ' || *p == '\t') p++;
                // Check if value starts with "close"
                if (strncasecmp(p, "close", 5) == 0) {
                    char *after = p + 5;
                    // Must be at end or followed by \r\n or whitespace
                    if (*after == '\r' || *after == '\n' || *after == ' ' || *after == '\0' || *after == ';') {
                        c->keep_alive = 0;
                    }
                }
            }
        }
    }
    return 1;
}

static int parse_request_line(Connection *c) {
    // Request line: "METHOD URI HTTP/1.1\r\n"
    // Find first \r\n
    if (c->read_pos < 2) return 0;
    int crlf_pos = -1;
    for (int i = 0; i <= c->read_pos - 2; i++) {
        if (c->read_buf[i] == '\r' && c->read_buf[i+1] == '\n') {
            crlf_pos = i;
            break;
        }
    }
    if (crlf_pos < 0) return 0;

    // Look for " HTTP/" before the crlf
    for (int j = 0; j < crlf_pos - 6; j++) {
        if (memcmp(c->read_buf + j, " HTTP/", 6) == 0) {
            // Found " HTTP/" at position j
            // The space before "HTTP/" is at position j
            // Find the space that separates METHOD from URI (search backward from j)
            int space_pos = -1;
            for (int sp = j - 1; sp >= 0; sp--) {
                if (c->read_buf[sp] == ' ') {
                    space_pos = sp;
                    break;
                }
            }
            if (space_pos < 0) continue;
            // Validate method (everything before space_pos)
            int valid = 1;
            for (int k = 0; k < space_pos; k++) {
                if (c->read_buf[k] < 'A' || c->read_buf[k] > 'Z') {
                    valid = 0;
                    break;
                }
            }
            if (valid) return 1;
        }
    }
    return 0;
}

// ---- Response builder ----

static void build_response(Connection *c, int status, const char *body, int body_len) {
    const char *status_line;
    if (status == 200) status_line = "200 OK";
    else if (status == 404) status_line = "404 Not Found";
    else if (status == 400) status_line = "400 Bad Request";
    else status_line = "500 Internal Server Error";

    char headers[512];
    int hl = snprintf(headers, sizeof(headers),
        "HTTP/1.1 %s\r\n"
        "Content-Type: text/plain\r\n"
        "Content-Length: %d\r\n"
        "Connection: %s\r\n"
        "\r\n",
        status_line, body_len,
        c->keep_alive ? "keep-alive" : "close");

    memcpy(c->write_buf, headers, hl);
    if (body && body_len > 0) {
        memcpy(c->write_buf + hl, body, body_len);
    }
    c->write_pos = hl + body_len;
    c->response_len = c->write_pos;
    c->write_pos = 0; // reset write position for actual send
}

// ---- Connection state machine ----

static void connection_to_reading(Connection *c) {
    c->state = CONN_READING;
}

static void connection_to_parsed(Connection *c) {
    c->state = CONN_PARSED;
    build_response(c, 200, "OK", 2);
}

static void connection_to_writing(Connection *c) {
    c->state = CONN_WRITING;
}

static void connection_to_keep_alive(Connection *c) {
    c->state = CONN_KEEP_ALIVE;
    c->read_pos = 0;
    c->write_pos = 0;
}

static void connection_to_closed(Connection *c) {
    free_connection(c);
}

// ---- Process connection in current state ----

static void process_connection(Connection *c) {
    switch (c->state) {
        case CONN_READING: {
            if (parse_request_headers(c)) {
                if (parse_request_line(c)) {
                    connection_to_parsed(c);
                } else {
                    build_response(c, 400, "Bad Request", 11);
                    connection_to_writing(c);
                }
            }
            break;
        }
        case CONN_WRITING:
        case CONN_KEEP_ALIVE:
            // Handled in main loop write phase
            break;
        default:
            break;
    }
}

// ---- Read from socket ----

static int read_from_socket(Connection *c) {
    if (c->read_pos >= READ_BUF_SIZE - 1) return 0; // buffer full

    ssize_t n = read(c->fd, c->read_buf + c->read_pos, READ_BUF_SIZE - c->read_pos - 1);
    if (n > 0) {
        c->read_pos += (int)n;
        c->read_buf[c->read_pos] = '\0';
        return 1;
    } else if (n == 0) {
        // Client closed
        return 0;
    } else {
        // EAGAIN / EWOULDBLOCK — no more data
        if (errno == EAGAIN || errno == EWOULDBLOCK) return 1;
        return 0;
    }
}

// ---- Write to socket ----

static int write_to_socket(Connection *c) {
    int remaining = c->response_len - c->write_pos;
    if (remaining <= 0) return 1;
    ssize_t n = write(c->fd, c->write_buf + c->write_pos, remaining);
    if (n > 0) {
        c->write_pos += (int)n;
        return 1;
    } else if (n == 0) {
        return 0;
    } else {
        if (errno == EAGAIN || errno == EWOULDBLOCK) return 1;
        return 0;
    }
}

// ---- Get CPU core count ----

static int get_cpu_count(void) {
    long n = sysconf(_SC_NPROCESSORS_ONLN);
    return (n > 0) ? (int)n : 4;
}

// ---- Epoll worker loop (runs in each forked process) ----

static void worker_loop(int listen_fd, int worker_id) {
    int epfd = epoll_create1(EPOLL_CLOEXEC);
    if (epfd < 0) {
        fprintf(stderr, "worker %d: epoll_create1 failed: %s\n", worker_id, strerror(errno));
        return;
    }

    // Add listen_fd to epoll
    struct epoll_event ev;
    ev.events = EPOLLIN | EPOLLET;
    ev.data.fd = listen_fd;
    if (epoll_ctl(epfd, EPOLL_CTL_ADD, listen_fd, &ev) < 0) {
        fprintf(stderr, "worker %d: epoll_ctl ADD listen_fd failed: %s\n", worker_id, strerror(errno));
        close(epfd);
        return;
    }

    struct epoll_event events[MAX_EVENTS];

    while (g_http_server_running) {
        int nfds = epoll_wait(epfd, events, MAX_EVENTS, -1);
        if (nfds < 0) {
            if (errno == EINTR) continue;
            break;
        }

        for (int i = 0; i < nfds; i++) {
            int fd = events[i].data.fd;
            uint32_t revents = events[i].events;

            if (fd == listen_fd) {
                // Accept all pending connections
                while (1) {
                    struct sockaddr_in client_addr;
                    socklen_t client_len = sizeof(client_addr);
                    int client_fd = accept(listen_fd, (struct sockaddr *)&client_addr, &client_len);
                    if (client_fd < 0) break;

                    // Disable Nagle + enable keep-alive
                    int flag = 1;
                    setsockopt(client_fd, IPPROTO_TCP, TCP_NODELAY, &flag, sizeof(flag));
                    setsockopt(client_fd, SOL_SOCKET, SO_KEEPALIVE, &flag, sizeof(flag));

                    Connection *c = alloc_connection(client_fd);
                    if (!c) {
                        close(client_fd);
                        continue;
                    }

                    set_nonblock(client_fd);
                    struct epoll_event cev;
                    cev.events = EPOLLIN | EPOLLET;
                    cev.data.fd = client_fd;
                    if (epoll_ctl(epfd, EPOLL_CTL_ADD, client_fd, &cev) < 0) {
                        free_connection(c);
                        continue;
                    }

                }
            } else {
                // Client socket event
                Connection *c = NULL;
                for (int j = 0; j < MAX_CONNECTIONS; j++) {
                    if (g_connections[j].fd == fd) {
                        c = &g_connections[j];
                        break;
                    }
                }
                if (!c) continue;

                if (revents & (EPOLLERR | EPOLLHUP)) {
                    connection_to_closed(c);
                    epoll_ctl(epfd, EPOLL_CTL_DEL, fd, NULL);
                    continue;
                }

                if (revents & EPOLLIN) {
                    // Edge-triggered: read all available data until EAGAIN
                    while (read_from_socket(c)) {
                        // Transition from keep-alive to reading when new data arrives
                        if (c->state == CONN_KEEP_ALIVE) {
                            connection_to_reading(c);
                        }
                        process_connection(c);
                        if (c->state != CONN_READING && c->state != CONN_KEEP_ALIVE) {
                            break;
                        }
                    }
                    // If socket closed or error
                    if (c->fd < 0) {
                        continue;
                    }
                }

                if (c->state == CONN_PARSED) {

                    connection_to_writing(c);
                    struct epoll_event cev;
                    cev.events = EPOLLOUT | EPOLLET;
                    cev.data.fd = fd;
                    epoll_ctl(epfd, EPOLL_CTL_MOD, fd, &cev);
                } else if (c->state == CONN_WRITING) {
                    // Edge-triggered: write all data until EAGAIN
                    while (c->write_pos < c->response_len) {
                        int written = write_to_socket(c);
                        if (!written) {
                            connection_to_closed(c);
                            epoll_ctl(epfd, EPOLL_CTL_DEL, fd, NULL);
                            break;
                        }
                    }
                    // Check if write complete
                    if (c->fd >= 0 && c->write_pos >= c->response_len) {

                        if (!c->keep_alive) {
                            connection_to_closed(c);
                            epoll_ctl(epfd, EPOLL_CTL_DEL, fd, NULL);
                        } else {
                            // Reset for keep-alive
                            c->state = CONN_KEEP_ALIVE;
                            c->read_pos = 0;
                            c->write_pos = 0;
                            c->response_len = 0;
                            struct epoll_event cev;
                            cev.events = EPOLLIN | EPOLLET;
                            cev.data.fd = fd;
                            epoll_ctl(epfd, EPOLL_CTL_MOD, fd, &cev);
                        }
                    }
                }
            }
        }
    }

    close(epfd);
}

// ---- Main server startup with SO_REUSEPORT + fork ----

static int spawn_workers(int listen_fd, int port) {
    int workers = get_cpu_count();

    for (int w = 0; w < workers; w++) {
        pid_t pid = fork();
        if (pid < 0) {
            return -1;
        }
        if (pid == 0) {
            // Child worker
            worker_loop(listen_fd, w);
            close(listen_fd);
            exit(0);
        }
        // Parent continues forking
    }

    // Parent waits for children
    while (g_http_server_running) {
        sleep(1);
    }

    // Reap children
    while (wait(NULL) > 0) {}

    return 0;
}

// ---- HTTP AOT stub implementations ----

// Max number of route patterns we track (for debug/future use)
#define MAX_ROUTES 32
static char *g_http_routes[MAX_ROUTES];
static int g_http_route_count = 0;

// zz_http_server(unused, err) — creates an HTTP server handle (AOT stub)
zz_value zz_http_server(zz_value unused, int *err) {
    (void)unused;
    *err = 0;
    return zz_int(0);
}

// zz_http_route_get(server, path, handler, err) — tracks route pattern; handler ignored in AOT
zz_value zz_http_route_get(zz_value server, zz_value path, zz_value handler, int *err) {
    *err = 0;
    (void)server; (void)handler;
    if (g_http_route_count < MAX_ROUTES - 1 && path.tag == ZZ_STR) {
        g_http_routes[g_http_route_count++] = strndup(zz_str_cptr(path.s), path.s->len);
    }
    return zz_int(0);
}

// zz_http_log(server, enabled, err) — AOT stub: no-op
zz_value zz_http_log(zz_value server, zz_value enabled, int *err) {
    *err = 0;
    (void)server; (void)enabled;
    return zz_unit();
}

// zz_http_listen(server, port, err) — starts HTTP server, blocks forever
zz_value zz_http_listen(zz_value server, zz_value port, int *err) {
    (void)server;
    *err = 0;

    int p = (port.tag == ZZ_INT) ? (int)port.i : 8080;
    if (p <= 0 || p > 65535) p = 8080;

    // Ignore SIGPIPE to avoid crash on closed connections
    signal(SIGPIPE, SIG_IGN);

    // Initialize connection pool
    init_connections();

    int listen_fd = socket(AF_INET, SOCK_STREAM, 0);
    if (listen_fd < 0) {
        fprintf(stderr, "zz_http_listen: socket() failed: %s\n", strerror(errno));
        return zz_unit();
    }

    int opt = 1;
    setsockopt(listen_fd, SOL_SOCKET, SO_REUSEADDR, &opt, sizeof(opt));

    // SO_REUSEPORT for multi-core scaling
    int reuseport = 1;
    setsockopt(listen_fd, SOL_SOCKET, SO_REUSEPORT, &reuseport, sizeof(reuseport));

    struct sockaddr_in addr;
    memset(&addr, 0, sizeof(addr));
    addr.sin_family = AF_INET;
    addr.sin_addr.s_addr = INADDR_ANY;
    addr.sin_port = htons((unsigned short)p);

    if (bind(listen_fd, (struct sockaddr *)&addr, sizeof(addr)) < 0) {
        fprintf(stderr, "zz_http_listen: bind() failed on port %d: %s\n", p, strerror(errno));
        close(listen_fd);
        return zz_unit();
    }

    if (listen(listen_fd, 128) < 0) {
        fprintf(stderr, "zz_http_listen: listen() failed: %s\n", strerror(errno));
        close(listen_fd);
        return zz_unit();
    }

    // Set listen_fd non-blocking
    set_nonblock(listen_fd);

    // Print SERVER_READY so benchmark runners know the port is open
    fprintf(stdout, "SERVER_READY\n");
    fflush(stdout);

    g_http_server_running = 1;

    // Spawn workers and wait
    spawn_workers(listen_fd, p);

    g_http_server_running = 0;
    close(listen_fd);
    return zz_unit();
}

// zz_http_handle(server, method, path, body, err) — AOT stub: returns "OK"
zz_value zz_http_handle(zz_value server, zz_value method, zz_value path, zz_value body, int *err) {
    *err = 0;
    (void)server; (void)method; (void)path; (void)body;
    return zz_unit();
}

// ---- entry --------------------------------------------------------------
// Process argv for env.args()/args.get_raw(): argv[0] is the binary,
// everything after is script args (mirrors the VM's interp.args).
static int zz_g_argc = 0;
static char **zz_g_argv = NULL;

int zz_run(void) {
    zz_main();
    int main_err = 0;
    if (zz_call_main())
        main_err = 1;
    return main_err;
}

int main(int argc, char **argv) {
    zz_g_argc = argc;
    zz_g_argv = argv;
    return zz_run();
}
// ---- codegen shims ------------------------------------------------------
zz_value zz_call_native1(zz_value (*f)(zz_value, int *), zz_value a) {
    int err = 0;
    zz_value r = f(a, &err);
    return r;
}

zz_value zz_call_native0(zz_value (*f)(zz_value, int *)) {
    int err = 0;
    zz_value r = f(zz_unit(), &err);
    return r;
}

zz_value zz_call_native2(zz_value (*f)(zz_value, zz_value, int *), zz_value a, zz_value b) {
    int err = 0;
    zz_value r = f(a, b, &err);
    return r;
}

zz_value zz_call_native3(zz_value (*f)(zz_value, zz_value, zz_value, int *), zz_value a, zz_value b, zz_value c) {
    int err = 0;
    zz_value r = f(a, b, c, &err);
    return r;
}
// typeof(v) — return type name as string.
zz_value zz_typeof(zz_value v, int *err) {
    (void)err;
    const char *name;
    switch (v.tag) {
        case ZZ_UNIT: name = "unit"; break;
        case ZZ_INT: name = "int"; break;
        case ZZ_FLOAT: name = "float"; break;
        case ZZ_BOOL: name = "bool"; break;
        case ZZ_STR: name = "str"; break;
        case ZZ_ARRAY: name = "array"; break;
        case ZZ_DICT: name = "dict"; break;
        case ZZ_FUNC: name = "func"; break;
        case ZZ_NATIVE: name = "native"; break;
        case ZZ_OPTION_SOME: name = "option"; break;
        case ZZ_OPTION_NONE: name = "option"; break;
        case ZZ_RESULT_OK: name = "result"; break;
        case ZZ_RESULT_ERR: name = "result"; break;
        case ZZ_RANGE: name = "range"; break;
        case ZZ_JSON: name = "json"; break;
        case ZZ_TCP_STREAM: name = "tcp.stream"; break;
        case ZZ_TCP_LISTENER: name = "tcp.listener"; break;
        case ZZ_TUPLE: name = "tuple"; break;
        default: name = "unknown"; break;
    }
    return zz_str_static(name);
}

// int(v) — cast to int.
zz_value zz_int_cast(zz_value v, int *err) {
    (void)err;
    switch (v.tag) {
        case ZZ_INT: return v;
        case ZZ_FLOAT: return (zz_value){ZZ_INT, {.i = (int64_t)v.f}};
        case ZZ_BOOL: return (zz_value){ZZ_INT, {.i = v.b ? 1 : 0}};
        case ZZ_STR: {
            char *end;
            int64_t n = strtoll(zz_str_cptr(v.s), &end, 10);
            if (end == zz_str_cptr(v.s)) return (zz_value){ZZ_INT, {.i = 0}};
            return (zz_value){ZZ_INT, {.i = n}};
        }
        default: return (zz_value){ZZ_INT, {.i = 0}};
    }
}

// float(v) — cast to float.
zz_value zz_float_cast(zz_value v, int *err) {
    (void)err;
    switch (v.tag) {
        case ZZ_FLOAT: return v;
        case ZZ_INT: return (zz_value){ZZ_FLOAT, {.f = (double)v.i}};
        case ZZ_BOOL: return (zz_value){ZZ_FLOAT, {.f = v.b ? 1.0 : 0.0}};
        case ZZ_STR: {
            char *end;
            double n = strtod(zz_str_cptr(v.s), &end);
            if (end == zz_str_cptr(v.s)) return (zz_value){ZZ_FLOAT, {.f = 0.0}};
            return (zz_value){ZZ_FLOAT, {.f = n}};
        }
        default: return (zz_value){ZZ_FLOAT, {.f = 0.0}};
    }
}

// bool(v) — cast to bool.
zz_value zz_bool_cast(zz_value v, int *err) {
    (void)err;
    switch (v.tag) {
        case ZZ_BOOL: return v;
        case ZZ_INT: return (zz_value){ZZ_BOOL, {.b = v.i != 0}};
        case ZZ_FLOAT: return (zz_value){ZZ_BOOL, {.b = v.f != 0.0}};
        case ZZ_STR: return (zz_value){ZZ_BOOL, {.b = v.s->len > 0}};
        case ZZ_ARRAY: return (zz_value){ZZ_BOOL, {.b = v.arr->len > 0}};
        case ZZ_DICT: return (zz_value){ZZ_BOOL, {.b = v.dict->len > 0}};
        default: return (zz_value){ZZ_BOOL, {.b = false}};
    }
}

// zz_str(v) — cast to string.
// math.abs(v)
zz_value zz_math_abs(zz_value v, int *err) {
    (void)err;
    if (v.tag == ZZ_INT) return (zz_value){ZZ_INT, {.i = v.i < 0 ? -v.i : v.i}};
    if (v.tag == ZZ_FLOAT) return (zz_value){ZZ_FLOAT, {.f = fabs(v.f)}};
    return v;
}

// math.sqrt(v)
zz_value zz_math_sqrt(zz_value v, int *err) {
    (void)err;
    double d = v.tag == ZZ_FLOAT ? v.f : (v.tag == ZZ_INT ? (double)v.i : 0.0);
    return (zz_value){ZZ_FLOAT, {.f = sqrt(d)}};
}

// math.floor(v) → int
zz_value zz_math_floor(zz_value v, int *err) {
    (void)err;
    double d = v.tag == ZZ_FLOAT ? v.f : (v.tag == ZZ_INT ? (double)v.i : 0.0);
    return (zz_value){ZZ_INT, {.i = (int64_t)floor(d)}};
}

// math.ceil(v) → int
zz_value zz_math_ceil(zz_value v, int *err) {
    (void)err;
    double d = v.tag == ZZ_FLOAT ? v.f : (v.tag == ZZ_INT ? (double)v.i : 0.0);
    return (zz_value){ZZ_INT, {.i = (int64_t)ceil(d)}};
}

// math.round(v) → float
zz_value zz_math_round(zz_value v, int *err) {
    (void)err;
    double d = v.tag == ZZ_FLOAT ? v.f : (v.tag == ZZ_INT ? (double)v.i : 0.0);
    return (zz_value){ZZ_FLOAT, {.f = round(d)}};
}

// math.trunc(v) → float
zz_value zz_math_trunc(zz_value v, int *err) {
    (void)err;
    double d = v.tag == ZZ_FLOAT ? v.f : (v.tag == ZZ_INT ? (double)v.i : 0.0);
    return (zz_value){ZZ_FLOAT, {.f = trunc(d)}};
}

// math.signum(v) → same type
zz_value zz_math_signum(zz_value v, int *err) {
    (void)err;
    if (v.tag == ZZ_INT) {
        int64_t s = (v.i > 0) - (v.i < 0);
        return (zz_value){ZZ_INT, {.i = s}};
    }
    if (v.tag == ZZ_FLOAT) {
        double s = (v.f > 0.0) - (v.f < 0.0);
        return (zz_value){ZZ_FLOAT, {.f = s}};
    }
    return v;
}

// math.hypot(x, y) → float
zz_value zz_math_hypot(zz_value x, zz_value y, int *err) {
    (void)err;
    double dx = x.tag == ZZ_FLOAT ? x.f : (x.tag == ZZ_INT ? (double)x.i : 0.0);
    double dy = y.tag == ZZ_FLOAT ? y.f : (y.tag == ZZ_INT ? (double)y.i : 0.0);
    return (zz_value){ZZ_FLOAT, {.f = hypot(dx, dy)}};
}

// math.clamp(val, min, max) → float
zz_value zz_math_clamp(zz_value val, zz_value min, zz_value max, int *err) {
    (void)err;
    double dv = val.tag == ZZ_FLOAT ? val.f : (val.tag == ZZ_INT ? (double)val.i : 0.0);
    double dmin = min.tag == ZZ_FLOAT ? min.f : (min.tag == ZZ_INT ? (double)min.i : 0.0);
    double dmax = max.tag == ZZ_FLOAT ? max.f : (max.tag == ZZ_INT ? (double)max.i : 0.0);
    if (dv < dmin) dv = dmin;
    if (dv > dmax) dv = dmax;
    return (zz_value){ZZ_FLOAT, {.f = dv}};
}

// math.root(x, n) → float (nth root)
zz_value zz_math_root(zz_value x, zz_value n, int *err) {
    (void)err;
    double dx = x.tag == ZZ_FLOAT ? x.f : (x.tag == ZZ_INT ? (double)x.i : 0.0);
    double dn = n.tag == ZZ_FLOAT ? n.f : (n.tag == ZZ_INT ? (double)n.i : 0.0);
    if (dn == 0.0) return (zz_value){ZZ_FLOAT, {.f = 1.0}};
    return (zz_value){ZZ_FLOAT, {.f = pow(dx, 1.0/dn)}};
}

// math.factorial(n) → .ok(int) or .err(str)
zz_value zz_math_factorial(zz_value n, int *err) {
    (void)err;
    if (n.tag != ZZ_INT || n.i < 0 || n.i > 20)
        return zz_variant_err(zz_str_static("factorial: input out of range"));
    int64_t r = 1;
    for (int64_t i = 2; i <= n.i; i++) r *= i;
    return zz_variant_ok((zz_value){ZZ_INT, {.i = r}});
}

// math.gcd(a, b) → int
zz_value zz_math_gcd(zz_value a, zz_value b, int *err) {
    (void)err;
    int64_t x = a.tag == ZZ_INT ? a.i : 0;
    int64_t y = b.tag == ZZ_INT ? b.i : 0;
    while (y != 0) { int64_t t = y; y = x % y; x = t; }
    return (zz_value){ZZ_INT, {.i = x < 0 ? -x : x}};
}

// math.lcm(a, b) → int
zz_value zz_math_lcm(zz_value a, zz_value b, int *err) {
    (void)err;
    int64_t x = a.tag == ZZ_INT ? a.i : 0;
    int64_t y = b.tag == ZZ_INT ? b.i : 0;
    if (x == 0 || y == 0) return (zz_value){ZZ_INT, {.i = 0}};
    int64_t g = x; int64_t t = y;
    while (t != 0) { int64_t tmp = t; t = g % t; g = tmp; }
    return (zz_value){ZZ_INT, {.i = (x / g) * y < 0 ? -((x / g) * y) : (x / g) * y}};
}

// math.sin(x) → float
zz_value zz_math_sin(zz_value x, int *err) {
    (void)err;
    double d = x.tag == ZZ_FLOAT ? x.f : (x.tag == ZZ_INT ? (double)x.i : 0.0);
    return (zz_value){ZZ_FLOAT, {.f = sin(d)}};
}

// math.cos(x) → float
zz_value zz_math_cos(zz_value x, int *err) {
    (void)err;
    double d = x.tag == ZZ_FLOAT ? x.f : (x.tag == ZZ_INT ? (double)x.i : 0.0);
    return (zz_value){ZZ_FLOAT, {.f = cos(d)}};
}

// math.tan(x) → float
zz_value zz_math_tan(zz_value x, int *err) {
    (void)err;
    double d = x.tag == ZZ_FLOAT ? x.f : (x.tag == ZZ_INT ? (double)x.i : 0.0);
    return (zz_value){ZZ_FLOAT, {.f = tan(d)}};
}

// math.asin(x) → float
zz_value zz_math_asin(zz_value x, int *err) {
    (void)err;
    double d = x.tag == ZZ_FLOAT ? x.f : (x.tag == ZZ_INT ? (double)x.i : 0.0);
    return (zz_value){ZZ_FLOAT, {.f = asin(d)}};
}

// math.acos(x) → float
zz_value zz_math_acos(zz_value x, int *err) {
    (void)err;
    double d = x.tag == ZZ_FLOAT ? x.f : (x.tag == ZZ_INT ? (double)x.i : 0.0);
    return (zz_value){ZZ_FLOAT, {.f = acos(d)}};
}

// math.atan(x) → float
zz_value zz_math_atan(zz_value x, int *err) {
    (void)err;
    double d = x.tag == ZZ_FLOAT ? x.f : (x.tag == ZZ_INT ? (double)x.i : 0.0);
    return (zz_value){ZZ_FLOAT, {.f = atan(d)}};
}

// math.sin_deg(x) → float
zz_value zz_math_sin_deg(zz_value x, int *err) {
    (void)err;
    double d = x.tag == ZZ_FLOAT ? x.f : (x.tag == ZZ_INT ? (double)x.i : 0.0);
    return (zz_value){ZZ_FLOAT, {.f = sin(d * M_PI / 180.0)}};
}

// math.cos_deg(x) → float
zz_value zz_math_cos_deg(zz_value x, int *err) {
    (void)err;
    double d = x.tag == ZZ_FLOAT ? x.f : (x.tag == ZZ_INT ? (double)x.i : 0.0);
    return (zz_value){ZZ_FLOAT, {.f = cos(d * M_PI / 180.0)}};
}

// math.tan_deg(x) → float
zz_value zz_math_tan_deg(zz_value x, int *err) {
    (void)err;
    double d = x.tag == ZZ_FLOAT ? x.f : (x.tag == ZZ_INT ? (double)x.i : 0.0);
    return (zz_value){ZZ_FLOAT, {.f = tan(d * M_PI / 180.0)}};
}

// math.to_radians(deg) → float
zz_value zz_math_to_radians(zz_value deg, int *err) {
    (void)err;
    double d = deg.tag == ZZ_FLOAT ? deg.f : (deg.tag == ZZ_INT ? (double)deg.i : 0.0);
    return (zz_value){ZZ_FLOAT, {.f = d * M_PI / 180.0}};
}

// math.to_degrees(rad) → float
zz_value zz_math_to_degrees(zz_value rad, int *err) {
    (void)err;
    double d = rad.tag == ZZ_FLOAT ? rad.f : (rad.tag == ZZ_INT ? (double)rad.i : 0.0);
    return (zz_value){ZZ_FLOAT, {.f = d * 180.0 / M_PI}};
}

// math.log(x) → float (natural log)
zz_value zz_math_log(zz_value x, int *err) {
    (void)err;
    double d = x.tag == ZZ_FLOAT ? x.f : (x.tag == ZZ_INT ? (double)x.i : 0.0);
    return (zz_value){ZZ_FLOAT, {.f = log(d)}};
}

// math.log10(x) → float
zz_value zz_math_log10(zz_value x, int *err) {
    (void)err;
    double d = x.tag == ZZ_FLOAT ? x.f : (x.tag == ZZ_INT ? (double)x.i : 0.0);
    return (zz_value){ZZ_FLOAT, {.f = log10(d)}};
}

// math.exp(x) → float
zz_value zz_math_exp(zz_value x, int *err) {
    (void)err;
    double d = x.tag == ZZ_FLOAT ? x.f : (x.tag == ZZ_INT ? (double)x.i : 0.0);
    return (zz_value){ZZ_FLOAT, {.f = exp(d)}};
}

// math.random() → float in [0, 1)
zz_value zz_math_random(zz_value unused, int *err) {
    (void)unused; (void)err;
    return (zz_value){ZZ_FLOAT, {.f = (double)rand() / (double)RAND_MAX}};
}

// math.is_nan(x) → bool
zz_value zz_math_is_nan(zz_value x, int *err) {
    (void)err;
    double d = x.tag == ZZ_FLOAT ? x.f : 0.0;
    return (zz_value){ZZ_BOOL, {.b = d != d}};
}

// math.is_inf(x) → bool
zz_value zz_math_is_inf(zz_value x, int *err) {
    (void)err;
    double d = x.tag == ZZ_FLOAT ? x.f : 0.0;
    return (zz_value){ZZ_BOOL, {.b = d == 1.0/0.0 || d == -1.0/0.0}};
}

// math constants
zz_value zz_math_pi(zz_value unused, int *err) {
    (void)unused; (void)err;
    return (zz_value){ZZ_FLOAT, {.f = M_PI}};
}
zz_value zz_math_e(zz_value unused, int *err) {
    (void)unused; (void)err;
    return (zz_value){ZZ_FLOAT, {.f = M_E}};
}
zz_value zz_math_tau(zz_value unused, int *err) {
    (void)unused; (void)err;
    return (zz_value){ZZ_FLOAT, {.f = 2.0 * M_PI}};
}
zz_value zz_math_inf(zz_value unused, int *err) {
    (void)unused; (void)err;
    return (zz_value){ZZ_FLOAT, {.f = 1.0/0.0}};
}
zz_value zz_math_nan(zz_value unused, int *err) {
    (void)unused; (void)err;
    return (zz_value){ZZ_FLOAT, {.f = 0.0/0.0}};
}

// math.isqrt(n) → .ok(int) or .err(str)
zz_value zz_math_isqrt(zz_value n, int *err) {
    (void)err;
    if (n.tag != ZZ_INT || n.i < 0)
        return zz_variant_err(zz_str_static("isqrt: non-negative integer required"));
    int64_t x = n.i, y = (x + 1) / 2;
    while (y < x) { x = y; y = (x + n.i / x) / 2; }
    return zz_variant_ok((zz_value){ZZ_INT, {.i = x}});
}

// math.mean(list) → .ok(float) or .err(str)
zz_value zz_math_mean(zz_value list, int *err) {
    (void)err;
    if (list.tag != ZZ_ARRAY || !list.arr || list.arr->len == 0)
        return zz_variant_err(zz_str_static("mean: empty list"));
    double sum = 0.0;
    for (size_t i = 0; i < list.arr->len; i++) {
        zz_value v = list.arr->items[i];
        sum += v.tag == ZZ_FLOAT ? v.f : (v.tag == ZZ_INT ? (double)v.i : 0.0);
    }
    return zz_variant_ok((zz_value){ZZ_FLOAT, {.f = sum / (double)list.arr->len}});
}

// math.median(list) → .ok(float) or .err(str)
zz_value zz_math_median(zz_value list, int *err) {
    (void)err;
    if (list.tag != ZZ_ARRAY || !list.arr || list.arr->len == 0)
        return zz_variant_err(zz_str_static("median: empty list"));
    // Copy to temp array, sort, find median.
    size_t n = list.arr->len;
    double *vals = (double *)malloc(n * sizeof(double));
    for (size_t i = 0; i < n; i++) {
        zz_value v = list.arr->items[i];
        vals[i] = v.tag == ZZ_FLOAT ? v.f : (v.tag == ZZ_INT ? (double)v.i : 0.0);
    }
    // Simple insertion sort (median lists are typically small).
    for (size_t i = 1; i < n; i++) {
        double key = vals[i];
        size_t j = i;
        while (j > 0 && vals[j-1] > key) { vals[j] = vals[j-1]; j--; }
        vals[j] = key;
    }
    double result;
    if (n % 2 == 1)
        result = vals[n / 2];
    else
        result = (vals[n/2 - 1] + vals[n/2]) / 2.0;
    free(vals);
    return zz_variant_ok((zz_value){ZZ_FLOAT, {.f = result}});
}

// math.rand_range(min, max) → float
zz_value zz_math_rand_range(zz_value min, zz_value max, int *err) {
    (void)err;
    double lo = min.tag == ZZ_FLOAT ? min.f : (min.tag == ZZ_INT ? (double)min.i : 0.0);
    double hi = max.tag == ZZ_FLOAT ? max.f : (max.tag == ZZ_INT ? (double)max.i : 0.0);
    double r = (double)rand() / (double)RAND_MAX;
    return (zz_value){ZZ_FLOAT, {.f = lo + r * (hi - lo)}};
}

// math.dot_product(v1, v2) → .ok(float) or .err(str)
zz_value zz_math_dot_product(zz_value v1, zz_value v2, int *err) {
    (void)err;
    if (v1.tag != ZZ_ARRAY || v2.tag != ZZ_ARRAY || !v1.arr || !v2.arr)
        return zz_variant_err(zz_str_static("dot_product: expected two arrays"));
    if (v1.arr->len != v2.arr->len)
        return zz_variant_err(zz_str_static("dot_product: arrays must have same length"));
    double sum = 0.0;
    for (size_t i = 0; i < v1.arr->len; i++) {
        double a = v1.arr->items[i].tag == ZZ_FLOAT ? v1.arr->items[i].f :
                   (v1.arr->items[i].tag == ZZ_INT ? (double)v1.arr->items[i].i : 0.0);
        double b = v2.arr->items[i].tag == ZZ_FLOAT ? v2.arr->items[i].f :
                   (v2.arr->items[i].tag == ZZ_INT ? (double)v2.arr->items[i].i : 0.0);
        sum += a * b;
    }
    return zz_variant_ok((zz_value){ZZ_FLOAT, {.f = sum}});
}

// math.magnitude(v) → float
zz_value zz_math_magnitude(zz_value v, int *err) {
    (void)err;
    if (v.tag != ZZ_ARRAY || !v.arr) return (zz_value){ZZ_FLOAT, {.f = 0.0}};
    double sum = 0.0;
    for (size_t i = 0; i < v.arr->len; i++) {
        double d = v.arr->items[i].tag == ZZ_FLOAT ? v.arr->items[i].f :
                   (v.arr->items[i].tag == ZZ_INT ? (double)v.arr->items[i].i : 0.0);
        sum += d * d;
    }
    return (zz_value){ZZ_FLOAT, {.f = sqrt(sum)}};
}

// math.matrix_mul(m1, m2) → .ok(array) or .err(str)
zz_value zz_math_matrix_mul(zz_value m1, zz_value m2, int *err) {
    (void)err;
    (void)m1; (void)m2;
    return zz_variant_err(zz_str_static("matrix_mul: not yet implemented in native"));
}

// env.get(name) — returns .some(val) or .none
zz_value zz_env_get(zz_value name, int *err) {
    (void)err;
    if (name.tag != ZZ_STR) return (zz_value){ZZ_OPTION_NONE, {0}};
    const char *val = getenv(zz_str_cptr(name.s));
    if (!val) return (zz_value){ZZ_OPTION_NONE, {0}};
    return zz_variant_some(zz_str_static(val));
}

// env.var(name) — returns .ok(val) or .err(msg)
zz_value zz_env_var(zz_value name, int *err) {
    (void)err;
    if (name.tag != ZZ_STR) return zz_variant_err(zz_str_static("env.var: expected string name"));
    const char *val = getenv(zz_str_cptr(name.s));
    if (!val) {
        // Build error message: "environment variable `NAME` not set"
        size_t nlen = name.s->len;
        const char *prefix = "environment variable `";
        const char *suffix = "` not set";
        size_t total = strlen(prefix) + nlen + strlen(suffix);
        char *msg = (char *)malloc(total + 1);
        memcpy(msg, prefix, strlen(prefix));
        memcpy(msg + strlen(prefix), zz_str_cptr(name.s), nlen);
        memcpy(msg + strlen(prefix) + nlen, suffix, strlen(suffix));
        msg[total] = '\0';
        return zz_variant_err(zz_str_owned(msg));
    }
    return zz_variant_ok(zz_str_static(val));
}

// env.args() — returns command-line arguments (excludes argv[0] binary name)
zz_value zz_env_args(zz_value unused, int *err) {
    (void)unused; (void)err;
    zz_value out = zz_array_new();
    for (int i = 1; i < zz_g_argc; i++) {
        zz_array_push(
            out.arr,
            zz_str_new(zz_g_argv[i], strlen(zz_g_argv[i]))
        );
    }
    return out;
}

// dict.len(d)
// fs.read(path)
zz_value zz_fs_read(zz_value path, int *err) {
    if (path.tag != ZZ_STR) { *err = 1; return zz_unit(); }
    FILE *f = fopen(zz_str_cptr(path.s), "rb");
    if (!f) {
        // Match the VM: `.err(io error string)`
        return zz_variant_err(zz_str_static("No such file or directory"));
    }
    fseek(f, 0, SEEK_END);
    long sz = ftell(f);
    fseek(f, 0, SEEK_SET);
    if (sz < 0) sz = 0;
    zz_str *out = str_alloc(sz);
    size_t n = fread(zz_str_ptr(out), 1, sz, f);
    fclose(f);
    zz_str_ptr(out)[n] = '\0';
    out->len = n;
    return zz_variant_ok((zz_value){ZZ_STR, {.s = out}});
}

// fs.write(path, data)
zz_value zz_fs_write(zz_value path, zz_value data, int *err) {
    if (path.tag != ZZ_STR || data.tag != ZZ_STR) { *err = 1; return zz_unit(); }
    FILE *f = fopen(zz_str_cptr(path.s), "wb");
    if (!f) {
        return zz_variant_err(zz_str_static("cannot open file for write"));
    }
    size_t w = fwrite(zz_str_cptr(data.s), 1, data.s->len, f);
    int close_ok = (fclose(f) == 0);
    if (w != data.s->len || !close_ok) {
        return zz_variant_err(zz_str_static("write failed"));
    }
    return zz_variant_ok(zz_unit());
}

// fs.exists(path)
zz_value zz_fs_exists(zz_value path, int *err) {
    (void)err;
    if (path.tag != ZZ_STR) return (zz_value){ZZ_BOOL, {.b = false}};
    FILE *f = fopen(zz_str_cptr(path.s), "rb");
    if (!f) return (zz_value){ZZ_BOOL, {.b = false}};
    fclose(f);
    return (zz_value){ZZ_BOOL, {.b = true}};
}

// fs.remove(path)
zz_value zz_fs_remove(zz_value path, int *err) {
    if (path.tag != ZZ_STR) { *err = 1; return zz_unit(); }
    int r = remove(zz_str_cptr(path.s));
    if (r != 0) {
        return zz_variant_err(zz_str_static("cannot remove file"));
    }
    return zz_variant_ok(zz_unit());
}

// fs.mkdir(path)
zz_value zz_fs_mkdir(zz_value path, int *err) {
    if (path.tag != ZZ_STR) { *err = 1; return zz_unit(); }
    int r = mkdir(zz_str_cptr(path.s), 0755);
    if (r != 0) { *err = 1; return zz_unit(); }
    return zz_unit();
}

// fs.readdir(path) — return array of filenames.
zz_value zz_fs_readdir(zz_value path, int *err) {
    (void)err;
    if (path.tag != ZZ_STR) return zz_array_new();
    // Not implemented fully — return empty array.
    return zz_array_new();
}

// encoding.url_encode(s)
zz_value zz_encoding_url_encode(zz_value s, int *err) {
    (void)err;
    if (s.tag != ZZ_STR) return s;
    const char *src = zz_str_cptr(s.s);
    size_t len = s.s->len;
    // Worst case: every byte becomes %XX.
    zz_str *out = str_alloc(len * 3);
    size_t pos = 0;
    for (size_t i = 0; i < len; i++) {
        unsigned char c = (unsigned char)src[i];
        if ((c >= 'A' && c <= 'Z') || (c >= 'a' && c <= 'z') || (c >= '0' && c <= '9') || c == '-' || c == '_' || c == '.' || c == '~') {
            zz_str_ptr(out)[pos++] = c;
        } else {
            snprintf(zz_str_ptr(out) + pos, 4, "%%%02X", c);
            pos += 3;
        }
    }
    zz_str_ptr(out)[pos] = '\0';
    out->len = pos;
    return (zz_value){ZZ_STR, {.s = out}};
}

// encoding.url_decode(s) → Result<str>
zz_value zz_encoding_url_decode(zz_value s, int *err) {
    (void)err;
    if (s.tag != ZZ_STR)
        return zz_variant_err(zz_str_static("URL decode error: expected string"));
    const char *src = zz_str_cptr(s.s);
    size_t len = s.s->len;
    zz_str *out = str_alloc(len);
    size_t pos = 0;
    for (size_t i = 0; i < len; i++) {
        if (src[i] == '%' && i + 2 < len) {
            char hex[3] = {src[i+1], src[i+2], '\0'};
            zz_str_ptr(out)[pos++] = (char)strtol(hex, NULL, 16);
            i += 2;
        } else if (src[i] == '+') {
            zz_str_ptr(out)[pos++] = ' ';
        } else {
            zz_str_ptr(out)[pos++] = src[i];
        }
    }
    zz_str_ptr(out)[pos] = '\0';
    out->len = pos;
    return zz_variant_ok((zz_value){ZZ_STR, {.s = out}});
}

// encoding.base64_encode(s)
zz_value zz_encoding_base64_encode(zz_value s, int *err) {
    (void)err;
    if (s.tag != ZZ_STR) return s;
    static const char tbl[] = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    const unsigned char *src = (const unsigned char *)zz_str_cptr(s.s);
    size_t len = s.s->len;
    size_t out_len = 4 * ((len + 2) / 3);
    zz_str *out = str_alloc(out_len);
    size_t j = 0;
    for (size_t i = 0; i < len; i += 3) {
        unsigned int a = src[i];
        unsigned int b = (i+1 < len) ? src[i+1] : 0;
        unsigned int c = (i+2 < len) ? src[i+2] : 0;
        unsigned int triple = (a << 16) | (b << 8) | c;
        zz_str_ptr(out)[j++] = tbl[(triple >> 18) & 0x3F];
        zz_str_ptr(out)[j++] = tbl[(triple >> 12) & 0x3F];
        zz_str_ptr(out)[j++] = (i+1 < len) ? tbl[(triple >> 6) & 0x3F] : '=';
        zz_str_ptr(out)[j++] = (i+2 < len) ? tbl[triple & 0x3F] : '=';
    }
    zz_str_ptr(out)[j] = '\0';
    out->len = j;
    return (zz_value){ZZ_STR, {.s = out}};
}

// encoding.base64_decode(s) → Result<str>
zz_value zz_encoding_base64_decode(zz_value s, int *err) {
    if (s.tag != ZZ_STR)
        return zz_variant_err(zz_str_static("base64 decode error: expected string"));
    static const unsigned char tbl[256] = {
        ['A']=0,1,2,3,4,5,6,7,8,9,10,11,12,13,14,15,16,17,18,19,20,21,22,23,24,25,
        ['a']=26,27,28,29,30,31,32,33,34,35,36,37,38,39,40,41,42,43,44,45,46,47,48,49,50,51,
        ['0']=52,53,54,55,56,57,58,59,60,61,
        ['+']=62, ['/']=63
    };
    const char *src = zz_str_cptr(s.s);
    size_t len = s.s->len;
    // Remove padding.
    while (len > 0 && src[len-1] == '=') len--;
    // Validate length: base64 (without padding) length must be multiple of 4
    // or the last group may be shorter (2 or 3 chars for 1 or 2 output bytes).
    if (len % 4 != 0 && len % 4 != 2 && len % 4 != 3)
        return zz_variant_err(zz_str_static("base64 decode error: Invalid padding"));
    // Validate characters.
    for (size_t i = 0; i < len; i++) {
        unsigned char c = (unsigned char)src[i];
        if (tbl[c] == 0 && c != 'A')
            return zz_variant_err(zz_str_static("base64 decode error: invalid character"));
    }
    size_t out_len = len * 3 / 4;
    zz_str *out = str_alloc(out_len);
    size_t j = 0;
    for (size_t i = 0; i < len; i += 4) {
        unsigned int a = tbl[(unsigned char)src[i]];
        unsigned int b = (i+1 < len) ? tbl[(unsigned char)src[i+1]] : 0;
        unsigned int c = (i+2 < len) ? tbl[(unsigned char)src[i+2]] : 0;
        unsigned int d = (i+3 < len) ? tbl[(unsigned char)src[i+3]] : 0;
        unsigned int triple = (a << 18) | (b << 12) | (c << 6) | d;
        if (j < out_len) zz_str_ptr(out)[j++] = (triple >> 16) & 0xFF;
        if (j < out_len) zz_str_ptr(out)[j++] = (triple >> 8) & 0xFF;
        if (j < out_len) zz_str_ptr(out)[j++] = triple & 0xFF;
    }
    zz_str_ptr(out)[j] = '\0';
    out->len = j;
    return zz_variant_ok((zz_value){ZZ_STR, {.s = out}});
}

// encoding.hex_encode(data) → hex string
zz_value zz_encoding_hex_encode(zz_value s, int *err) {
    (void)err;
    if (s.tag != ZZ_STR) return zz_str_static("");
    const unsigned char *d = (const unsigned char *)zz_str_cptr(s.s);
    size_t len = s.s->len;
    char *hex = (char *)malloc(len * 2 + 1);
    for (size_t i = 0; i < len; i++) {
        snprintf(hex + i*2, 3, "%02x", d[i]);
    }
    hex[len * 2] = '\0';
    return zz_str_owned(hex);
}

// encoding.hex_decode(hex_str) → .ok(data) or .err(str)
zz_value zz_encoding_hex_decode(zz_value s, int *err) {
    (void)err;
    if (s.tag != ZZ_STR)
        return zz_variant_err(zz_str_static("expected string"));
    size_t len = s.s->len;
    if (len % 2 != 0)
        return zz_variant_err(zz_str_static("odd-length hex string"));
    char *out = (char *)malloc(len / 2 + 1);
    for (size_t i = 0; i < len; i += 2) {
        char byte_str[3] = { zz_str_cptr(s.s)[i], zz_str_cptr(s.s)[i+1], '\0' };
        char *endptr;
        unsigned long val = strtoul(byte_str, &endptr, 16);
        if (endptr != byte_str + 2)
            return zz_variant_err(zz_str_static("hex decode error: invalid digit found in string"));
        out[i/2] = (char)val;
    }
    out[len/2] = '\0';
    return zz_variant_ok(zz_str_new(out, len / 2));
}

// =====================================================================
//  HTTP client — libcurl-based implementation
// =====================================================================

// Helper struct for curl body accumulation (avoids flexible-array-member issues)
typedef struct {
    char *data;
    size_t cap;
    size_t len;
} curl_buf;

// Callback for curl to write response body into a growing memory buffer.
static size_t curl_write_cb(void *data, size_t size, size_t nmemb, void *userp) {
    size_t realsize = size * nmemb;
    curl_buf *buf = (curl_buf *)userp;
    size_t new_len = buf->len + realsize;
    if (buf->cap <= new_len) {
        size_t new_cap = buf->cap == 0 ? 256 : buf->cap * 2;
        while (new_cap < new_len + 1) new_cap *= 2;
        buf->data = (char *)realloc(buf->data, new_cap);
        buf->cap = new_cap;
    }
    memcpy(buf->data + buf->len, data, realsize);
    buf->len = new_len;
    buf->data[buf->len] = '\0';
    return realsize;
}

// Callback for curl to read headers into a dict.
static size_t curl_header_cb(void *data, size_t size, size_t nmemb, void *userp) {
    size_t realsize = size * nmemb;
    const char *line = (const char *)data;
    const char *colon = strchr(line, ':');
    if (!colon || colon >= line + realsize) return realsize;

    size_t key_len = colon - line;
    const char *val = colon + 1;
    while (*val == ' ' || *val == '\t') val++;
    size_t val_len = realsize - (val - line);
    if (val_len > 0 && val[val_len-1] == '\r') val_len--;
    if (val_len > 0 && val[val_len-1] == '\n') val_len--;

    zz_value *hdrs_val = (zz_value *)userp;
    if (hdrs_val->tag != ZZ_DICT) return realsize;

    zz_str *key = str_alloc(key_len);
    memcpy(zz_str_ptr(key), line, key_len);
    zz_str_ptr(key)[key_len] = '\0';
    key->len = key_len;

    zz_str *value_str = str_alloc(val_len);
    memcpy(zz_str_ptr(value_str), val, val_len);
    zz_str_ptr(value_str)[val_len] = '\0';
    value_str->len = val_len;

    zz_dict_set(hdrs_val->dict, (zz_value){ZZ_STR, {.s = key}}, (zz_value){ZZ_STR, {.s = value_str}});
    return realsize;
}

// http.get(url, headers) → .ok(HttpResponse) or .err(str)
zz_value zz_http_get(zz_value url, zz_value headers, int *err) {
    if (url.tag != ZZ_STR) { *err = 1; return zz_variant_err(zz_str_static("http.get: url must be string")); }
    *err = 0;

    CURL *curl = curl_easy_init();
    if (!curl) { *err = 1; return zz_variant_err(zz_str_static("http.get: curl_easy_init failed")); }

    curl_buf body_buf = {0};
    zz_value headers_dict = zz_dict_new();

    curl_easy_setopt(curl, CURLOPT_URL, (char *)zz_str_cptr(url.s));
    curl_easy_setopt(curl, CURLOPT_WRITEFUNCTION, curl_write_cb);
    curl_easy_setopt(curl, CURLOPT_WRITEDATA, &body_buf);
    curl_easy_setopt(curl, CURLOPT_HEADERFUNCTION, curl_header_cb);
    curl_easy_setopt(curl, CURLOPT_HEADERDATA, &headers_dict);
    curl_easy_setopt(curl, CURLOPT_FOLLOWLOCATION, 1L);
    curl_easy_setopt(curl, CURLOPT_TIMEOUT, 30L);
    curl_easy_setopt(curl, CURLOPT_NOSIGNAL, 1L);

    struct curl_slist *header_list = NULL;
    if (headers.tag == ZZ_DICT && headers.dict && headers.dict->len > 0) {
        for (size_t i = 0; i < headers.dict->len; i++) {
            zz_str *k = headers.dict->entries[i].key;
            zz_value *v = &headers.dict->entries[i].val;
            if (k && v->tag == ZZ_STR) {
                // NOTE: no trailing CRLF — curl treats a trailing CRLF as an
                // empty header line, which terminates the header block early
                // and swallows the request body into the headers.
                size_t hlen = k->len + 2 + v->s->len;
                char *h = (char *)malloc(hlen + 1);
                memcpy(h, zz_str_cptr(k), k->len);
                h[k->len] = ':';
                h[k->len + 1] = ' ';
                memcpy(h + k->len + 2, zz_str_cptr(v->s), v->s->len);
                h[k->len + 2 + v->s->len] = '\0';
                header_list = curl_slist_append(header_list, h);
                free(h);
            }
        }
        if (header_list) curl_easy_setopt(curl, CURLOPT_HTTPHEADER, header_list);
    }

    CURLcode res = curl_easy_perform(curl);
    if (header_list) curl_slist_free_all(header_list);

    if (res != CURLE_OK) {
        char errbuf[256];
        snprintf(errbuf, sizeof(errbuf), "http.get: %s", curl_easy_strerror(res));
        curl_easy_cleanup(curl);
        if (body_buf.data) free(body_buf.data);
        zz_release(&headers_dict);
        *err = 1;
        return zz_variant_err(zz_str_static(errbuf));
    }

    long http_code = 0;
    curl_easy_getinfo(curl, CURLINFO_RESPONSE_CODE, &http_code);
    curl_easy_cleanup(curl);

    // Adopt body_buf into a proper zz_str
    zz_str *body_str;
    if (body_buf.data && body_buf.len > 0) {
        body_str = str_alloc(body_buf.len);
        memcpy(zz_str_ptr(body_str), body_buf.data, body_buf.len);
        zz_str_ptr(body_str)[body_buf.len] = '\0';
        free(body_buf.data);
    } else {
        body_str = str_alloc(0);
    }

    // Build response object
    const char *field_names_str[] = {"status", "body", "headers", "text", "json"};
    zz_value field_names[5];
    for (int i = 0; i < 5; i++) {
        field_names[i] = zz_str_static(field_names_str[i]);
    }
    zz_value resp_val = zz_object_new("http.response", field_names, 5);
    zz_object_set_field(&resp_val, "status", (zz_value){ZZ_INT, {.i = http_code}});
    zz_object_set_field(&resp_val, "body", (zz_value){ZZ_STR, {.s = body_str}});
    zz_object_set_field(&resp_val, "text", (zz_value){ZZ_STR, {.s = body_str}});
    zz_object_set_field(&resp_val, "headers", zz_clone(headers_dict));

    zz_value json_val = zz_unit();
    if (body_str->len > 0) {
        int jerr = 0;
        json_val = zz_json_parse((zz_value){ZZ_STR, {.s = body_str}}, &jerr);
        if (jerr) json_val = zz_unit();
    }
    zz_object_set_field(&resp_val, "json", json_val);

    zz_release(&headers_dict);
    return zz_variant_ok(resp_val);
}

// http.post(url, body, headers) → .ok(HttpResponse) or .err(str)
zz_value zz_http_post(zz_value url, zz_value body, zz_value headers, int *err) {
    if (url.tag != ZZ_STR) { *err = 1; return zz_variant_err(zz_str_static("http.post: url must be string")); }
    *err = 0;

    CURL *curl = curl_easy_init();
    if (!curl) { *err = 1; return zz_variant_err(zz_str_static("http.post: curl_easy_init failed")); }

    curl_buf body_buf = {0};
    zz_value headers_dict = zz_dict_new();

    curl_easy_setopt(curl, CURLOPT_URL, (char *)zz_str_cptr(url.s));
    curl_easy_setopt(curl, CURLOPT_POST, 1L);
    curl_easy_setopt(curl, CURLOPT_WRITEFUNCTION, curl_write_cb);
    curl_easy_setopt(curl, CURLOPT_WRITEDATA, &body_buf);
    curl_easy_setopt(curl, CURLOPT_HEADERFUNCTION, curl_header_cb);
    curl_easy_setopt(curl, CURLOPT_HEADERDATA, &headers_dict);
    curl_easy_setopt(curl, CURLOPT_FOLLOWLOCATION, 1L);
    curl_easy_setopt(curl, CURLOPT_TIMEOUT, 30L);
    curl_easy_setopt(curl, CURLOPT_NOSIGNAL, 1L);

    if (body.tag == ZZ_STR && body.s && body.s->len > 0) {
        curl_easy_setopt(curl, CURLOPT_POSTFIELDS, (char *)zz_str_cptr(body.s));
        // Explicit size: CURLOPT_POSTFIELDS alone uses strlen(), which is
        // wrong if the payload ever contains NUL bytes.
        curl_easy_setopt(curl, CURLOPT_POSTFIELDSIZE, (long)body.s->len);
    }

    struct curl_slist *header_list = NULL;
    if (headers.tag == ZZ_DICT && headers.dict && headers.dict->len > 0) {
        for (size_t i = 0; i < headers.dict->len; i++) {
            zz_str *k = headers.dict->entries[i].key;
            zz_value *v = &headers.dict->entries[i].val;
            if (k && v->tag == ZZ_STR) {
                // NOTE: no trailing CRLF — curl treats a trailing CRLF as an
                // empty header line, which terminates the header block early
                // and swallows the request body into the headers.
                size_t hlen = k->len + 2 + v->s->len;
                char *h = (char *)malloc(hlen + 1);
                memcpy(h, zz_str_cptr(k), k->len);
                h[k->len] = ':';
                h[k->len + 1] = ' ';
                memcpy(h + k->len + 2, zz_str_cptr(v->s), v->s->len);
                h[k->len + 2 + v->s->len] = '\0';
                header_list = curl_slist_append(header_list, h);
                free(h);
            }
        }
        if (header_list) curl_easy_setopt(curl, CURLOPT_HTTPHEADER, header_list);
    }

    CURLcode res = curl_easy_perform(curl);
    if (header_list) curl_slist_free_all(header_list);

    if (res != CURLE_OK) {
        char errbuf[256];
        snprintf(errbuf, sizeof(errbuf), "http.post: %s", curl_easy_strerror(res));
        curl_easy_cleanup(curl);
        if (body_buf.data) free(body_buf.data);
        zz_release(&headers_dict);
        *err = 1;
        return zz_variant_err(zz_str_static(errbuf));
    }

    long http_code = 0;
    curl_easy_getinfo(curl, CURLINFO_RESPONSE_CODE, &http_code);
    curl_easy_cleanup(curl);

    zz_str *body_str;
    if (body_buf.data && body_buf.len > 0) {
        body_str = str_alloc(body_buf.len);
        memcpy(zz_str_ptr(body_str), body_buf.data, body_buf.len);
        zz_str_ptr(body_str)[body_buf.len] = '\0';
        free(body_buf.data);
    } else {
        body_str = str_alloc(0);
    }

    const char *field_names_str[] = {"status", "body", "headers", "text", "json"};
    zz_value field_names[5];
    for (int i = 0; i < 5; i++) {
        field_names[i] = zz_str_static(field_names_str[i]);
    }
    zz_value resp_val = zz_object_new("http.response", field_names, 5);
    zz_object_set_field(&resp_val, "status", (zz_value){ZZ_INT, {.i = http_code}});
    zz_object_set_field(&resp_val, "body", (zz_value){ZZ_STR, {.s = body_str}});
    zz_object_set_field(&resp_val, "text", (zz_value){ZZ_STR, {.s = body_str}});
    zz_object_set_field(&resp_val, "headers", zz_clone(headers_dict));

    zz_value json_val = zz_unit();
    if (body_str->len > 0) {
        int jerr = 0;
        json_val = zz_json_parse((zz_value){ZZ_STR, {.s = body_str}}, &jerr);
        if (jerr) json_val = zz_unit();
    }
    zz_object_set_field(&resp_val, "json", json_val);

    zz_release(&headers_dict);
    return zz_variant_ok(resp_val);
}

// http.response.status(response) → int
zz_value zz_http_response_status(zz_value resp, int *err) {
    (void)err;
    return zz_object_get_field(&resp, "status");
}

// http.response.text(response) → str
zz_value zz_http_response_text(zz_value resp, int *err) {
    (void)err;
    return zz_object_get_field(&resp, "text");
}

// http.response.json(response) → json
zz_value zz_http_response_json(zz_value resp, int *err) {
    (void)err;
    return zz_object_get_field(&resp, "json");
}

// http.response.headers(response) → dict
zz_value zz_http_response_headers(zz_value resp, int *err) {
    (void)err;
    return zz_object_get_field(&resp, "headers");
}

