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
// Synchronous-handoff nesting depth (direct worker handoff): each inline
// resume nests `zz_run_task` + closure + channel-op frames on this
// thread's C stack. Capped (see ZZ_HANDOFF_MAX_DEPTH) so waiter chains
// (A→B→C→…) degrade to queueing instead of growing the stack.
static __thread int zz_handoff_depth = 0;
// Helping nesting depth (blocking waits that run other tasks instead of
// parking — audit CRITICAL-1 fix). Capped (see ZZ_HELP_MAX_DEPTH) so
// pathological blocking-nesting depths degrade to parking instead of
// growing the C stack without bound.
static __thread int zz_help_depth = 0;

int zz_green_suspended(void) {
    return zz_suspend_verdict;
}

void zz_green_resumed(void) {
    zz_suspend_verdict = 0;
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
    // Fused-block discipline (B4-lite slab): `embedded` reps keep
    // kinds/sizes inside the same malloc block as the header (never freed
    // separately); `inline_vals` reps (spawn-dup built, worker-owned) also
    // store every cell inline — VALUE cells in the values section, RAW
    // bytes in the raw section — so the whole closure is one malloc/one
    // free. Adopted reps (call-site transfer) keep heap cells and only
    // embed the kinds/sizes.
    int embedded;
    int inline_vals;
    // Total heap block bytes (meaningful when embedded): drives the
    // size-classed block pool below (checkout reuses by class, checkin
    // returns by this size — no size recomputation on free).
    size_t block_size;
    void *env[];
} zz_closure_rep;

// Fused rep block layout: [header][env ptrs][kinds][sizes][inline values]
// [inline raw bytes]. One malloc per closure instead of 3 + per-cell
// mallocs — the spawn burst is allocator-bound (glibc malloc ~450ns/op
// under cross-thread free traffic; tcmalloc proved 4x), so halving
// allocator traffic is worth more than any queue tuning.
static size_t zz_rep_block_size(size_t nenv, size_t nval, size_t nraw) {
    size_t n = sizeof(zz_closure_rep) + nenv * sizeof(void *);
    n += nenv * sizeof(unsigned char);
    n = (n + 7) & ~(size_t)7;
    n += nenv * sizeof(size_t);
    n = (n + 15) & ~(size_t)15;
    n += nval * sizeof(zz_value);
    // Raw slots are 16-aligned (user struct captures may need it; the old
    // per-cell malloc guaranteed it) — the pre-scan, layout, and fill
    // below must use this same stride.
    n = (n + 15) & ~(size_t)15;
    n += nraw;
    return n;
}

// Split a fused block into its sections. Valid only for embedded reps;
// `nval` is the inline VALUE-cell count the block was sized with (known
// at build time from the kinds pre-scan).
static void zz_rep_block_layout(zz_closure_rep *rep, size_t nval,
                                unsigned char **kinds_out,
                                size_t **sizes_out, zz_value **vals_out,
                                char **raw_out) {
    char *p = (char *)rep + sizeof(zz_closure_rep) + rep->nenv * sizeof(void *);
    *kinds_out = (unsigned char *)p;
    p += rep->nenv * sizeof(unsigned char);
    p = (char *)(((uintptr_t)p + 7) & ~(uintptr_t)7);
    *sizes_out = (size_t *)p;
    p += rep->nenv * sizeof(size_t);
    p = (char *)(((uintptr_t)p + 15) & ~(uintptr_t)15);
    *vals_out = (zz_value *)p;
    p += nval * sizeof(zz_value);
    p = (char *)(((uintptr_t)p + 15) & ~(uintptr_t)15);
    *raw_out = p;
}

zz_value zz_closure_make(zz_dispatch_fn f) {
    zz_closure_rep *rep =
        (zz_closure_rep *)malloc(sizeof(zz_closure_rep));
    if (!rep) return zz_unit();
    rep->fn = f;
    rep->nenv = 0;
    rep->cell_kind = NULL;
    rep->cell_size = NULL;
    rep->is_green = 0;
    rep->embedded = 0;
    rep->inline_vals = 0;
    rep->block_size = 0;
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
    // Adopted cells (call-site transfer): VALUE cell pointers stay heap
    // (the caller keeps no reference — generated code never frees _cap
    // cells), kinds/sizes ride inline. One malloc total.
    size_t total = zz_rep_block_size(nenv, 0, 0);
    zz_closure_rep *rep = (zz_closure_rep *)malloc(total);
    if (!rep) return zz_unit();
    rep->fn = f;
    rep->nenv = nenv;
    rep->is_green = 0;
    rep->embedded = 1;
    rep->inline_vals = 0;
    rep->block_size = total;
    if (nenv) {
        unsigned char *rk;
        size_t *rs;
        zz_value *rv;
        char *rr;
        zz_rep_block_layout(rep, 0, &rk, &rs, &rv, &rr);
        (void)rv;
        (void)rr;
        rep->cell_kind = rk;
        rep->cell_size = rs;
        for (size_t i = 0; i < nenv; i++) {
            rep->cell_kind[i] = kinds ? kinds[i] : ZZ_CELL_VALUE;
            rep->cell_size[i] =
                sizes ? sizes[i] : sizeof(zz_value);
        }
        for (size_t i = 0; i < nenv; i++) rep->env[i] = cells[i];
    } else {
        rep->cell_kind = NULL;
        rep->cell_size = NULL;
    }
    zz_value v;
    v.tag = ZZ_NATIVE;
    v.payload = (zz_value *)rep;
    return v;
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
// Pooled inline blocks rejoin the rep-block pool instead of freeing
// (forward: pool block lives below with the dup path).
static void zz_rep_block_checkin(void *block, size_t size);
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
        // Inline cells (fused spawn-dup reps) die with the block; adopted
        // heap cells (call-site transfer, legacy reps) are freed here.
        if (!rep->inline_vals) free(rep->env[i]);
    }
    // Embedded kinds/sizes ride inside the block; legacy reps free them.
    // Pooled inline blocks (spawn-dup built) rejoin their size class;
    // everything else frees normally.
    if (rep->inline_vals && rep->block_size > 0) {
        zz_rep_block_checkin(rep, rep->block_size);
    } else {
        if (!rep->embedded) {
            free(rep->cell_kind);
            free(rep->cell_size);
        }
        free(rep);
    }
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

// Forward: renders a printed `.err` as a readable, hinted diagnostic and
// aborts. Defined after the socket include block (needs isatty/fileno).
static void zz_throw_printed_err(const zz_value *payload);

zz_value zz_io_println(zz_value v, int *err) {
    (void)err;
    // Unwrap consecutive `Result::Ok` / `Option::Some` layers for stdout
    // presentation (mirrors the VM's `for_stdout`): `println(.ok(x))`
    // prints `x`, `println(.some(x))` prints `x`, bare `.none` prints
    // `none`. Nested Options inside containers unwrap via the display
    // printer below; only `zz_dbg` / `:?` keep wrappers. A printed `.err`
    // throws a readable, hinted diagnostic on stderr instead of a raw
    // `.err(...)` line. A bare function value is always a missing `()`
    // — abort with a hint instead of rendering `<func>` / `<value>`.
    for (;;) {
        if (v.tag == ZZ_RESULT_OK && v.payload) {
            v = *v.payload;
        } else if (v.tag == ZZ_OPTION_SOME && v.payload) {
            v = *v.payload;
        } else {
            break;
        }
    }
    if (v.tag == ZZ_OPTION_NONE) {
        fputs("none\n", stdout);
        fflush(stdout);
        return zz_unit();
    }
    if (v.tag == ZZ_NATIVE || v.tag == ZZ_FUNC) {
        int tty;
#ifdef ZZ_OS_WINDOWS
        tty = _isatty(_fileno(stderr));
#else
        tty = isatty(fileno(stderr));
#endif
        if (tty) {
            fprintf(stderr, "\033[1;31merror\033[0m: cannot print a function value: call it with arguments\n");
            fprintf(stderr, "    \033[1;36m=\033[0m \033[36mhint: a bare function name is the function itself, not its result\033[0m\n");
        } else {
            fprintf(stderr, "error: cannot print a function value: call it with arguments\n");
            fprintf(stderr, "    = hint: a bare function name is the function itself, not its result\n");
        }
        fprintf(stderr, "zz: program failed\n");
        fflush(stderr);
        exit(1);
    }
    if (v.tag == ZZ_RESULT_ERR && v.payload) {
        zz_throw_printed_err(v.payload);
        // Unreachable: `zz_throw_printed_err` exits the process.
        return zz_unit();
    }
    zz_print_value_display(stdout, &v);
    fputc('\n', stdout);
    fflush(stdout);
    return zz_unit();
}

zz_value zz_dbg(zz_value v, int *err) {
    (void)err;
    // Debug path: preserves explicit `.some(v)` / `.none` wrappers,
    // unlike interpolation / println / str() which unwrap for display.
    char *s = zz_value_to_string(&v);
    fprintf(stderr, "[dbg] %s\n", s);
    fflush(stderr);
    free(s);
    return zz_clone(v);
}

zz_value zz_io_print(zz_value v, int *err) {
    (void)err;
    for (;;) {
        if (v.tag == ZZ_RESULT_OK && v.payload) {
            v = *v.payload;
        } else if (v.tag == ZZ_OPTION_SOME && v.payload) {
            v = *v.payload;
        } else {
            break;
        }
    }
    if (v.tag == ZZ_OPTION_NONE) {
        fputs("none", stdout);
        fflush(stdout);
        return zz_unit();
    }
    if (v.tag == ZZ_NATIVE || v.tag == ZZ_FUNC) {
        int tty;
#ifdef ZZ_OS_WINDOWS
        tty = _isatty(_fileno(stderr));
#else
        tty = isatty(fileno(stderr));
#endif
        if (tty) {
            fprintf(stderr, "\033[1;31merror\033[0m: cannot print a function value: call it with arguments\n");
            fprintf(stderr, "    \033[1;36m=\033[0m \033[36mhint: a bare function name is the function itself, not its result\033[0m\n");
        } else {
            fprintf(stderr, "error: cannot print a function value: call it with arguments\n");
            fprintf(stderr, "    = hint: a bare function name is the function itself, not its result\n");
        }
        fprintf(stderr, "zz: program failed\n");
        fflush(stderr);
        exit(1);
    }
    if (v.tag == ZZ_RESULT_ERR && v.payload) {
        zz_throw_printed_err(v.payload);
        return zz_unit();
    }
    zz_print_value_display(stdout, &v);
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
    int owned; // 1 once `items` is heap (must be freed by the owner)
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

// Small-copy fast path: the first entries ride in a caller-provided inline
// store (spawn closures typically memoize 1–2 entries), so the common dup
// pays zero allocator traffic for bookkeeping.
#define ZZ_DUP_INLINE_CAP 8

static void zz_dup_memo_put(zz_dup_memo *m, const void *src, zz_value dst) {
    if (m->len == m->cap) {
        size_t nc = m->cap * 2;
        zz_dup_entry *nb =
            (zz_dup_entry *)malloc(nc * sizeof(zz_dup_entry));
        if (!nb) return; // memo is best-effort dedup; the copy stays correct
        for (size_t i = 0; i < m->len; i++) nb[i] = m->items[i];
        if (m->owned) free(m->items);
        m->items = nb;
        m->cap = nc;
        m->owned = 1;
    }
    m->items[m->len].src = src;
    m->items[m->len].dst = dst;
    m->len++;
}

// ---- closure rep block pool (B4-lite slab) -------------------------------
// Spawn bursts allocate + free one fused rep block per task across threads
// (main allocates, worker frees) — exactly the traffic glibc malloc
// handles worst (~450ns/op with cross-thread frees; tcmalloc proved 4x).
// This pool caches freed blocks by size class behind tiny critical
// sections (~10 instructions vs malloc's hundreds), so burst traffic
// recycles without touching the allocator. Bounded (cap per class),
// overflow-agnostic (unknown sizes malloc/free normally — always correct).
#define ZZ_REP_POOL_CLASSES 7
static const size_t zz_rep_pool_buckets[ZZ_REP_POOL_CLASSES] = {
    128, 256, 512, 1024, 2048, 4096, 8192
};
#define ZZ_REP_POOL_CAP 256 // max cached blocks per class

typedef struct {
    pthread_mutex_t lock;
    void *head;   // stack of free blocks (next pointer in first word)
    size_t count;
} zz_rep_pool_t;

static zz_rep_pool_t zz_rep_pools[ZZ_REP_POOL_CLASSES];
static pthread_once_t zz_rep_pool_once = PTHREAD_ONCE_INIT;

static void zz_rep_pool_init(void) {
    for (int i = 0; i < ZZ_REP_POOL_CLASSES; i++) {
        pthread_mutex_init(&zz_rep_pools[i].lock, NULL);
        zz_rep_pools[i].head = NULL;
        zz_rep_pools[i].count = 0;
    }
}

// Smallest class holding `need` bytes, or -1 past the largest bucket.
static int zz_rep_pool_class(size_t need) {
    for (int i = 0; i < ZZ_REP_POOL_CLASSES; i++) {
        if (zz_rep_pool_buckets[i] >= need) return i;
    }
    return -1;
}

// Checkout a block of at least `need` bytes: pooled bucket when one fits
// (the caller lays out by needed dims — a bigger bucket's prefix is
// identical), exact malloc on overflow. NULL only when malloc fails.
static void *zz_rep_block_checkout(size_t need) {
    pthread_once(&zz_rep_pool_once, zz_rep_pool_init);
    int c = zz_rep_pool_class(need);
    if (c < 0) return malloc(need);
    zz_rep_pool_t *p = &zz_rep_pools[c];
    pthread_mutex_lock(&p->lock);
    void *b = p->head;
    if (b) {
        p->head = *(void **)b;
        p->count--;
    }
    pthread_mutex_unlock(&p->lock);
    if (b) return b;
    return malloc(zz_rep_pool_buckets[c]);
}

// Return a block of recorded checkout size `size`. Only exact bucket
// sizes rejoin their class (checkout stores bucket sizes; overflow
// exact-mallocs and foreign blocks fall through to free). Full pools fall
// through the same way — always correct, just uncached.
static void zz_rep_block_checkin(void *block, size_t size) {
    int c = zz_rep_pool_class(size);
    if (c < 0 || zz_rep_pool_buckets[c] != size) {
        free(block);
        return;
    }
    zz_rep_pool_t *p = &zz_rep_pools[c];
    pthread_mutex_lock(&p->lock);
    if (p->count < ZZ_REP_POOL_CAP) {
        *(void **)block = p->head;
        p->head = block;
        p->count++;
        block = NULL;
    }
    pthread_mutex_unlock(&p->lock);
    if (block) free(block);
}

static zz_value zz_value_dup_inner(zz_value v, zz_dup_memo *m);

// Build a worker-owned fused closure rep directly from capture cells:
// one block holds header + env ptrs + kinds + sizes + inline VALUE cells
// (deep-copied via the memo) + inline RAW bytes (memcpy). Zero per-cell
// mallocs. Shared by the spawn-dup path and the `zz_spawn_ex` fast path
// below (which skips the intermediate call-site rep entirely).
//
// `kinds`/`sizes` may be NULL (all VALUE, default sizes — the untyped
// make convention). On OOM returns a null-payload closure (calls on it
// evaluate to unit); callers must not memoize that as a copy.
static zz_value zz_closure_dup_build(zz_dispatch_fn fn, void **env,
                                     const unsigned char *kinds,
                                     const size_t *sizes, size_t nenv,
                                     int is_green, zz_dup_memo *m) {
    size_t nval = 0;
    size_t nraw = 0;
    for (size_t i = 0; i < nenv; i++) {
        unsigned char kind = kinds ? kinds[i] : ZZ_CELL_VALUE;
        size_t size = sizes ? sizes[i] : sizeof(zz_value);
        if (kind != ZZ_CELL_RAW) {
            nval++;
        } else {
            nraw = (nraw + 15) & ~(size_t)15;
            nraw += size;
        }
    }
    zz_closure_rep *nr;
    size_t block_size;
    {
        size_t need = zz_rep_block_size(nenv, nval, nraw);
        int c = zz_rep_pool_class(need);
        // Pooled checkout (bucket-sized, recorded for exact checkin) or
        // exact malloc on overflow. The layout below addresses by needed
        // dims only, so a roomier bucket's prefix is identical.
        nr = (zz_closure_rep *)zz_rep_block_checkout(need);
        block_size = c < 0 ? need : zz_rep_pool_buckets[c];
    }
    if (!nr) {
        // OOM degradation: null-payload closure (calls evaluate to unit,
        // spawns report via err). Callers never memoize this as a copy.
        zz_value oom;
        oom.tag = ZZ_NATIVE;
        oom.payload = NULL;
        return oom;
    }
    nr->fn = fn;
    nr->nenv = nenv;
    nr->is_green = is_green;
    nr->embedded = 1;
    nr->inline_vals = 1;
    nr->block_size = block_size;
    unsigned char *rk;
    size_t *rs;
    zz_value *rv;
    char *rr;
    zz_rep_block_layout(nr, nval, &rk, &rs, &rv, &rr);
    nr->cell_kind = nenv ? rk : NULL;
    nr->cell_size = nenv ? rs : NULL;
    size_t vi = 0;
    for (size_t i = 0; i < nenv; i++) {
        unsigned char kind = kinds ? kinds[i] : ZZ_CELL_VALUE;
        size_t size = sizes ? sizes[i] : sizeof(zz_value);
        rk[i] = kind;
        rs[i] = size;
        if (kind == ZZ_CELL_RAW) {
            // Unboxed cell: bitwise copy into the inline raw section.
            // (Interior pointers, e.g. refcounted strings inside unboxed
            // struct cells, are shared — documented limitation; never
            // misread as zz_value, which segfaulted.)
            uintptr_t aligned = ((uintptr_t)rr + 15) & ~(uintptr_t)15;
            char *slot = (char *)aligned;
            if (env[i] && size) memcpy(slot, env[i], size);
            nr->env[i] = slot;
            rr = slot + size;
        } else {
            nr->env[i] = &rv[vi++];
            rv[vi - 1] = env[i] ? zz_value_dup_inner(*(zz_value *)env[i], m)
                                : zz_unit();
        }
    }
    zz_value out;
    out.tag = ZZ_NATIVE;
    out.payload = (zz_value *)nr;
    return out;
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
    case ZZ_BYTES: {
        // Buffers are immutable: share the store (retain), never copy.
        if (!v.bytes) return v;
        zz_retain_bytes(v.bytes);
        return v;
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
        int is_green = v.tag == ZZ_NATIVE && v.payload
            && ((zz_closure_rep *)(void *)v.payload)->is_green;
        // Forward the source rep's own kinds/sizes (NULL when untyped —
        // the helper applies the same defaults `cell_info` would).
        const unsigned char *sk = NULL;
        const size_t *ss = NULL;
        if (v.tag == ZZ_NATIVE && v.payload) {
            zz_closure_rep *sr = (zz_closure_rep *)(void *)v.payload;
            sk = sr->cell_kind;
            ss = sr->cell_size;
        }
        zz_value out = zz_closure_dup_build(fn, env, sk, ss, nenv,
                                            is_green, m);
        // `zz_closure_dup_build` degrades to the shared source on OOM;
        // only memoize real copies (the source never aliases a copy).
        if (out.payload != v.payload) {
            if (v.payload) zz_dup_memo_put(m, v.payload, out);
        }
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
    zz_dup_entry inline_store[ZZ_DUP_INLINE_CAP];
    zz_dup_memo m = {inline_store, 0, ZZ_DUP_INLINE_CAP, 0};
    zz_value out = zz_value_dup_inner(v, &m);
    if (m.owned) free(m.items);
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
// Run one task to completion-or-suspend (executor trampoline, defined
// below; forward because the channel handoff path can invoke it inline).
static void zz_run_task(zz_task_t task);
// Run one pending task instead of parking in a blocking wait (helping —
// audit CRITICAL-1 fix; defined after the trampoline). Returns 1 when a
// task was run (caller rechecks its predicate), 0 when idle or past the
// nesting cap (caller parks normally).
static int zz_worker_help_once(void);
// Run a handed-off task synchronously on the sender's thread when the
// nesting budget allows (defined after the trampoline). Returns 1 when
// the task was run inline (caller must NOT enqueue), 0 when the budget
// is exhausted (caller falls back to queueing).
static int zz_handoff_run_inline(zz_task_t task);
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
            // Direct worker handoff: run a suspended task waiter
            // synchronously on this thread instead of routing it through
            // the executor queue. In the rendezvous steady state (ping-pong)
            // the waiter runs NOW — its reply lands in the ring before this
            // sender even reaches `recv` — so the round trip pays zero
            // queue hops, zero thread hops, and zero futex park/wake pairs.
            // Falls back to queueing past the nesting budget.
            if (wis_task && !zz_handoff_run_inline(wt)) zz_enqueue_task(wt);
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
    // Counted wake: the spill path serves no green waiters (handoff runs
    // first under the same lock), so with no classic sleepers parked the
    // signal is a pure futex waste. Gated like the ring path — the
    // announce-then-verify discipline on both counters keeps it airtight
    // (a sleeper registering after this read sees the published value and
    // never parks; one registered before is seen and woken).
    int wspill = __atomic_load_n(&ch->sleepers, __ATOMIC_ACQUIRE) > 0
        || __atomic_load_n(&ch->gparked, __ATOMIC_ACQUIRE) > 0;
    if (wspill) pthread_cond_signal(&ch->cond);
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
    // arrived while the ring was full. Also gated on zero queued green
    // waiters (same discipline as the green fast path): a registered
    // waiter must observe every value.
    zz_value v;
    if (__atomic_load_n(&ch->green_waiters, __ATOMIC_ACQUIRE) == 0
        && __atomic_load_n(&ch->spill_count, __ATOMIC_ACQUIRE) == 0
        && zz_ring_try_dequeue(ch, &v)) {
        return v;
    }
    // Adaptive spin: a rendezvous landing within microseconds (the
    // ping-pong steady state) costs PAUSEs instead of a futex pair.
    // Lock-free ring only, and only while BOTH tiers look empty: when
    // the spill is non-empty the value waits under the mutex — spinning
    // would burn ~10µs before taking the lock (measured 170x slowdown
    // draining a stocked buffer). Bounded (~1024) so a genuinely empty
    // channel still parks promptly. Breaks immediately when a green
    // waiter registers (its takes must observe every value — see the
    // fast-path gate above).
    if (__atomic_load_n(&ch->spill_count, __ATOMIC_ACQUIRE) == 0) {
        for (int spin = 0; spin < 1024; spin++) {
            if (__atomic_load_n(&ch->green_waiters, __ATOMIC_ACQUIRE) != 0) {
                break;
            }
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
    // Slow loop: spill first (older than anything that arrived while the
    // ring was full), then the ring. A ring miss with both tiers
    // nominally non-empty is transient (a publisher mid-claim) — loop
    // and re-check rather than sleeping on a value already on its way.
    // Counted sleepers: sends skip the condvar notify when this is zero,
    // so waiter-free traffic pays no futex wake. Re-checked after every
    // wake (spurious wakeups and racing consumers just loop).
    //
    // The lock is taken per iteration (not held across the loop): helping
    // runs tasks that may use this same channel.
    // Top-up discipline: only when genuinely about to park (value
    // confirmed absent above — never on mere slow-path entry), at most
    // once per call, and only when no executor worker is already parked
    // to absorb the stranded deque (see zz_executor_top_up's own cap).
    // Unconditional top-up here spawned one immortal 1MB-stack thread
    // per blocking recv — 100k latency round-trips = 100k threads.
    int topped_up = 0;
    for (;;) {
        pthread_mutex_lock(&ch->lock);
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
        // Helping (audit CRITICAL-1): a worker parks only when truly
        // idle. Otherwise it runs stranded deque/steal work and rechecks
        // the tiers — the lock is released across the run (the helped
        // task may itself use this channel) and announce-then-verify
        // below is unchanged, so no wakeup can be lost: we only sleep
        // while announced, and any sender in between either enqueued
        // (seen on recheck) or signaled (sleepers > 0).
        pthread_mutex_unlock(&ch->lock);
        if (zz_worker_help_once()) continue;
        pthread_mutex_lock(&ch->lock);
        if (ch->len > 0 || zz_ring_len_estimate(ch) > 0) {
            pthread_mutex_unlock(&ch->lock);
            continue;
        }
        __atomic_add_fetch(&ch->sleepers, 1, __ATOMIC_ACQ_REL);
        while (ch->len == 0 && zz_ring_len_estimate(ch) == 0) {
            pthread_cond_wait(&ch->cond, &ch->lock);
        }
        __atomic_sub_fetch(&ch->sleepers, 1, __ATOMIC_ACQ_REL);
        pthread_mutex_unlock(&ch->lock);
    }
}

zz_value zz_chan_try_recv(zz_value chan, int *err) {
    if (chan.tag != ZZ_CHAN) { *err = 1; return zz_unit(); }
    zz_chan *ch = chan.chan;
    zz_value v;
    // Fast path: zero spill count means every queued value sits in the
    // ring in FIFO order — a lock-free pop is exactly ordered. Gated on
    // zero green waiters like the other fast paths (a registered waiter
    // must observe every value).
    if (__atomic_load_n(&ch->green_waiters, __ATOMIC_ACQUIRE) == 0
        && __atomic_load_n(&ch->spill_count, __ATOMIC_ACQUIRE) == 0
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

// Remove a just-registered waiter that was never exposed to any sender:
// the caller holds `ch->lock` continuously since registration, so the
// node is still queued and no handoff could have touched it. Matched by
// frame (the caller only kept `fr`). Used by the register-then-recheck
// path when a value turns up in the tiers after all (no suspend after
// all — frame state untouched).
static void zz_gwait_unregister_tail(zz_chan *ch, zz_task_frame *fr) {
    // Find our node by frame identity (defensive walk: the tail invariant
    // above should always hold, but position is never trusted).
    zz_green_waiter *prev = NULL;
    zz_green_waiter *w = ch->gwait_head;
    while (w && w->frame != fr) {
        prev = w;
        w = w->next;
    }
    if (!w) {
        // Not queued (should be impossible — see above). Bail without
        // touching counts: the waiter still owns the frame's future.
        return;
    }
    if (prev) {
        prev->next = w->next;
        if (ch->gwait_tail == w) ch->gwait_tail = prev;
    } else {
        ch->gwait_head = w->next;
        if (ch->gwait_tail == w) ch->gwait_tail = NULL;
    }
    __atomic_sub_fetch(&ch->green_waiters, 1, __ATOMIC_RELEASE);
    zz_gwait_recycle(ch, w);
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
    // Fast path first (same exactness rule as the blocking call). Gated
    // on zero queued green waiters: a registered waiter must observe every
    // value, so lock-free takes stop while one exists — otherwise a racing
    // take can strand values behind a waiter at sender exhaustion (lost
    // wakeup — a waiter suspended with values sitting in the ring that no
    // future send will ever hand off).
    if (__atomic_load_n(&ch->green_waiters, __ATOMIC_ACQUIRE) == 0
        && __atomic_load_n(&ch->spill_count, __ATOMIC_ACQUIRE) == 0
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
    // Register-then-recheck under this same lock: a lock-free racing
    // consumer may have beaten our ring CAS above while values remained,
    // so re-examine both tiers AFTER registering. Either we take a value
    // (unregister, no suspend) or both tiers are truly empty — and any
    // LATER send then sees our waiter under this same lock and hands off.
    // This closes the lost-wakeup hole where a waiter slept behind values
    // stranded in the ring at sender exhaustion.
    fr->resume = resume_id;
    if (zz_chan_wait_register(ch, fr)) {
        if (ch->len > 0) {
            v = ch->queue[ch->head];
            ch->head = (ch->head + 1) % ch->cap;
            ch->len--;
            __atomic_sub_fetch(&ch->spill_count, 1, __ATOMIC_RELEASE);
            zz_gwait_unregister_tail(ch, fr);
            pthread_mutex_unlock(&ch->lock);
            *err = 0;
            zz_suspend_verdict = 0;
            return v;
        }
        if (zz_ring_try_dequeue(ch, &v)) {
            zz_gwait_unregister_tail(ch, fr);
            pthread_mutex_unlock(&ch->lock);
            *err = 0;
            zz_suspend_verdict = 0;
            return v;
        }
        pthread_mutex_unlock(&ch->lock);
        *err = 0;
        zz_suspend_verdict = 1;
        return zz_unit();
    }
    // Sync frame: park the thread until a send hands off (value lands in
    // `fr` under this same lock). Same register-then-recheck discipline as
    // the task branch above: re-examine both tiers now that our own
    // registration gates every lock-free taker, so the park below can only
    // begin with both tiers truly empty. Helping applies after that (audit
    // CRITICAL-1): a worker parked with owned deque work strands it, so
    // help first and only sleep while announced. A handoff landing
    // between recheck and announce is still exact — the value sits in
    // `fr->has_value`, which the wait predicate rechecks under the lock.
    if (ch->len > 0) {
        v = ch->queue[ch->head];
        ch->head = (ch->head + 1) % ch->cap;
        ch->len--;
        __atomic_sub_fetch(&ch->spill_count, 1, __ATOMIC_RELEASE);
        zz_gwait_unregister_tail(ch, fr);
        pthread_mutex_unlock(&ch->lock);
        *err = 0;
        zz_suspend_verdict = 0;
        return v;
    }
    if (zz_ring_try_dequeue(ch, &v)) {
        zz_gwait_unregister_tail(ch, fr);
        pthread_mutex_unlock(&ch->lock);
        *err = 0;
        zz_suspend_verdict = 0;
        return v;
    }
    for (;;) {
        if (fr->has_value) break;
        pthread_mutex_unlock(&ch->lock);
        if (zz_worker_help_once()) {
            pthread_mutex_lock(&ch->lock);
            continue;
        }
        pthread_mutex_lock(&ch->lock);
        if (fr->has_value) break;
        __atomic_add_fetch(&ch->gparked, 1, __ATOMIC_ACQ_REL);
        while (!fr->has_value) {
            pthread_cond_wait(&ch->cond, &ch->lock);
        }
        __atomic_sub_fetch(&ch->gparked, 1, __ATOMIC_ACQ_REL);
        break;
    }
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
    size_t inj_len; // __atomic: stealers pre-check this lock-free (see below)
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
//
// `bottom` writes are atomic stores (relaxed): thieves read `bottom`
// atomically, and mixing plain writes with atomic reads is a formal data
// race (TSan-flagged) even though the protocol's real ordering comes from
// the fence + top CAS below. Relaxed stores compile to the same single
// instruction on every target — zero cost, fully defined.
static int zz_deque_pop(zz_deque_t *dq, zz_task_t *out) {
    size_t bottom = __atomic_load_n(&dq->bottom, __ATOMIC_RELAXED);
    if (bottom == 0) return 0;
    bottom--;
    __atomic_store_n(&dq->bottom, bottom, __ATOMIC_RELAXED);
    __atomic_thread_fence(__ATOMIC_SEQ_CST);
    size_t top = __atomic_load_n(&dq->top, __ATOMIC_RELAXED);
    if (top > bottom) {
        // Empty (a thief took the last one): restore and report empty.
        __atomic_store_n(&dq->bottom, bottom + 1, __ATOMIC_RELAXED);
        return 0;
    }
    *out = dq->buf[bottom & ZZ_DEQUE_MASK];
    if (top == bottom) {
        // Single element: race thieves for it via CAS.
        size_t expect = top;
        if (!__atomic_compare_exchange_n(&dq->top, &expect, top + 1, 0,
                                         __ATOMIC_ACQ_REL, __ATOMIC_RELAXED)) {
            // Lost: a thief won it. Restore and report empty.
            __atomic_store_n(&dq->bottom, bottom + 1, __ATOMIC_RELAXED);
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
    size_t ilen = __atomic_load_n(&ex->inj_len, __ATOMIC_RELAXED);
    if (ilen == ex->inj_cap) {
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
                (zz_task_t *)malloc(ilen * sizeof(zz_task_t));
            if (!tmp) {
                fprintf(stderr, "zz: out of memory (injector)\n");
                exit(1);
            }
            for (size_t i = 0; i < ilen; i++) {
                tmp[i] = nb[(ex->inj_head + i) % ex->inj_cap];
            }
            for (size_t i = 0; i < ilen; i++) nb[i] = tmp[i];
            free(tmp);
        }
        ex->inj_buf = nb;
        ex->inj_cap = new_cap;
        ex->inj_head = 0;
    }
    ex->inj_buf[(ex->inj_head + ilen) % ex->inj_cap] = task;
    __atomic_store_n(&ex->inj_len, ilen + 1, __ATOMIC_RELEASE);
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
//
// Lock-free empty pre-check: workers probe the injector on every 16th
// spin iteration, and a mutex around every probe collapses under a spawn
// burst (the pusher's lock ping-pongs between 5 cores, inflating each
// spawn by microseconds). The length is atomic: zero means provably empty
// (a push publishes the task before bumping the count, so a zero read
// cannot miss a task), non-zero falls through to the locked pop which
// re-verifies. A stale-zero read only delays discovery by one spin
// iteration — progress is never lost.
static int zz_injector_steal(zz_task_t *out) {
    zz_executor_t *ex = &zz_executor;
    if (__atomic_load_n(&ex->inj_len, __ATOMIC_ACQUIRE) == 0) return 0;
    int got = 0;
    pthread_mutex_lock(&ex->inj_lock);
    size_t ilen = __atomic_load_n(&ex->inj_len, __ATOMIC_RELAXED);
    if (ilen > 0) {
        *out = ex->inj_buf[ex->inj_head];
        ex->inj_head = (ex->inj_head + 1) % ex->inj_cap;
        __atomic_store_n(&ex->inj_len, ilen - 1, __ATOMIC_RELAXED);
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
    // Every run starts live: a recycled worker thread may carry a stale
    // suspend verdict from an earlier task. Only an actual suspend during
    // THIS run may set it (green entries do so before returning).
    zz_suspend_verdict = 0;
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

// Direct worker handoff: the synchronous-resume half of a channel/join
// handoff. Bypasses the executor queue (no deque push, no steal, no
// thread hop) by running the waiter NOW on the sender's thread.
//
// Why this is safe:
// - `zz_run_task` saves/restores the TLS frame+task around the run, so a
//   sender that is itself a running task resumes undisturbed.
// - Every green entry sets the thread-local suspend verdict before
//   returning, and the inline runner saves/restores it, so the inline run
//   cannot clobber the sender's own verdict: the sender's next blocking
//   call re-establishes it before anything reads it (`send` itself never
//   touches the verdict).
// - No locks are held at any call site (handoff runs after unlock), so no
//   lock ordering is introduced.
// - Stack growth is bounded by ZZ_HANDOFF_MAX_DEPTH: each inline level
//   runs to its next suspend or to completion before returning, and past
//   the cap the caller queues instead (0 = queue it).
#define ZZ_HANDOFF_MAX_DEPTH 8

static int zz_handoff_run_inline(zz_task_t task) {
    if (zz_handoff_depth >= ZZ_HANDOFF_MAX_DEPTH) return 0;
    zz_handoff_depth++;
    // Verdict save/restore (audit MAJOR-1): an inlined waiter that
    // re-suspends leaves verdict=1 on this thread's TLS. Without restoring,
    // a green sender that returns without another green entry would look
    // suspended to the trampoline, which would drop its completion (lost
    // join result + leaked frame/rep). The sender's own verdict is
    // meaningless mid-`send` — only the trampoline reads it.
    int saved_verdict = zz_suspend_verdict;
    zz_run_task(task);
    zz_suspend_verdict = saved_verdict;
    zz_handoff_depth--;
    return 1;
}

// Helping (audit CRITICAL-1 fix): a worker about to park in a blocking
// wait runs one pending task from its OWN deque instead and reports 1 so
// the caller rechecks its predicate. Parked owners strand deque work no
// thief can reach (top-up deques are never stolen from by design);
// draining it here keeps that work moving while the waiter still polls
// its own predicate every round. Returns 0 when the deque is empty
// (caller parks normally) or past the nesting cap (caller parks —
// graceful degradation for absurd blocking-nesting depths, documented in
// threading.md).
//
// Own-deque ONLY — never steals (audit follow-up): stealing while holding
// a blocked task buries dependency order under the run. A total-order
// chain (au4) distributed across nested stacks provably stalls that way:
// satisfiable predicates rot mid-stack while tops wait on the
// unsatisfiable, and per-join broadcasts only revisit tops. Own-chain
// nesting is instead inherently forward (a parent buried under its own
// child is revisited when the child completes and unwinds into it), and
// every steal happens from the hold-nothing worker loop, so every parked
// wait is either a revisited top or unwind-reachable. All stealing stays
// in `zz_worker_loop`; helping never touches another deque.
//
// Runs WITHOUT any channel/join lock (callers unlock first): the helped
// task may itself wait on the same channel/join, which re-enters helping
// one level deeper (bounded by ZZ_HELP_MAX_DEPTH).
#define ZZ_HELP_MAX_DEPTH 64

static int zz_worker_help_once(void) {
    if (!zz_is_worker || !zz_local_deque) return 0;
    if (zz_help_depth >= ZZ_HELP_MAX_DEPTH) return 0;
    // Owner pop only (single owner = this thread; racing thieves use the
    // top CAS — the standard Chase–Lev pairing, same as the worker loop).
    zz_task_t task;
    if (!zz_deque_pop(zz_local_deque, &task)) return 0;
    // Verdict save/restore (same class as MAJOR-1): the helped task may
    // suspend (verdict=1), but the waiter runs a *blocking* call that
    // never reads the verdict — only the trampoline does, after later
    // green entries re-establish whatever this run needs.
    int saved_verdict = zz_suspend_verdict;
    zz_help_depth++;
    zz_run_task(task);
    zz_help_depth--;
    zz_suspend_verdict = saved_verdict;
    return 1;
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
    // Counted wake: waiter-free completions (the fan-in steady state —
    // no joiner registered) skip the broadcast entirely, so each
    // completion pays no futex wake. Airtight by announce-then-verify: a
    // blocking waiter increments `sleepers` before predicating on
    // `completed` under this same lock. Broadcast (not signal): handles
    // are multi-recv by design, so N parked joiners must ALL wake
    // (audit MAJOR-2: a single signal stranded every joiner but one).
    if (__atomic_load_n(&join->sleepers, __ATOMIC_ACQUIRE) > 0) {
        pthread_cond_broadcast(&join->cond);
    }
    pthread_mutex_unlock(&join->lock);
    for (size_t i = 0; i < ntasks; i++) {
        // Direct handoff applies to join waiters too: a completion that
        // releases a suspended joiner runs it inline instead of queueing.
        if (!zz_handoff_run_inline(tasks[i])) zz_enqueue_task(tasks[i]);
    }
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
            if (__atomic_load_n(&ex->inj_len, __ATOMIC_RELAXED) > 0) {
                task = ex->inj_buf[ex->inj_head];
                ex->inj_head = (ex->inj_head + 1) % ex->inj_cap;
                __atomic_sub_fetch(&ex->inj_len, 1, __ATOMIC_RELAXED);
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
    __atomic_store_n(&ex->inj_len, 0, __ATOMIC_RELAXED);
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
    // Static initialization (assignment, no init call): handles are never
    // destroyed (intentionally leaked, like before), so dynamic init buys
    // nothing — this skips two libc calls per spawn on the hot path.
    join->lock = (pthread_mutex_t)PTHREAD_MUTEX_INITIALIZER;
    join->cond = (pthread_cond_t)PTHREAD_COND_INITIALIZER;
    join->result = zz_unit();
    join->completed = 0;
    join->consumed = 0;
    join->gwait_head = NULL;
    join->gwait_tail = NULL;
    join->gwait_pool = NULL;
    join->gwait_pool_n = 0;
    __atomic_store_n(&join->green_waiters, 0, __ATOMIC_RELAXED);
    __atomic_store_n(&join->gparked, 0, __ATOMIC_RELAXED);
    __atomic_store_n(&join->sleepers, 0, __ATOMIC_RELAXED);
    zz_task_t task;
    task.fn = zz_value_dup(fn);  // Independent copy for the worker.
    if (task.fn.tag == ZZ_NATIVE && !task.fn.payload) {
        // Dup OOM: null-payload closure. Drop the task (reporting via
        // err) rather than enqueueing a no-op that still costs a join.
        *err = 1;
        return zz_unit();
    }
    task.join = join;
    task.frame = NULL;  // Lazily allocated by the trampoline (green only).
    zz_enqueue_task(task);
    zz_value v;
    v.tag = ZZ_TASK_JOIN;
    v.task = join;
    return v;
}

// Spawn fast path for `task.spawn(|...| ...)` closure literals: build the
// worker-owned closure rep directly from the call-site capture arrays and
// enqueue, skipping the intermediate call-site rep (one malloc plus a full
// make/dup layer) that `zz_spawn(zz_closure_make_ex_typed(...))` burns.
// Snapshot isolation is identical (VALUE cells deep-copied, RAW bytes
// copied); only the redundant middle copy disappears.
//
// `is_green` marks suspendable bodies (the lowerer decides eligibility).
// The capture arrays are borrowed for the call (stack `_cap` arrays) —
// every byte the worker needs is copied before return.
zz_value zz_spawn_ex(zz_dispatch_fn fn, void **cells,
                     const unsigned char *kinds, const size_t *sizes,
                     size_t nenv, int is_green, int *err) {
    if (!fn) { *err = 1; return zz_unit(); }
    pthread_once(&zz_executor_once, zz_executor_init);
    zz_dup_entry inline_store[ZZ_DUP_INLINE_CAP];
    zz_dup_memo m = {inline_store, 0, ZZ_DUP_INLINE_CAP, 0};
    zz_value owned =
        zz_closure_dup_build(fn, cells, kinds, sizes, nenv, is_green, &m);
    if (m.owned) free(m.items);
    if (owned.tag != ZZ_NATIVE || !owned.payload) {
        *err = 1;
        return zz_unit();
    }
    zz_task_join *join = (zz_task_join *)malloc(sizeof(zz_task_join));
    if (!join) {
        fprintf(stderr, "zz: out of memory (task join)\n");
        exit(1);
    }
    // Static initialization (see `zz_spawn`): assignment, no libc call.
    join->lock = (pthread_mutex_t)PTHREAD_MUTEX_INITIALIZER;
    join->cond = (pthread_cond_t)PTHREAD_COND_INITIALIZER;
    join->result = zz_unit();
    join->completed = 0;
    join->consumed = 0;
    join->gwait_head = NULL;
    join->gwait_tail = NULL;
    join->gwait_pool = NULL;
    join->gwait_pool_n = 0;
    __atomic_store_n(&join->green_waiters, 0, __ATOMIC_RELAXED);
    __atomic_store_n(&join->gparked, 0, __ATOMIC_RELAXED);
    __atomic_store_n(&join->sleepers, 0, __ATOMIC_RELAXED);
    zz_task_t task;
    task.fn = owned;
    task.join = join;
    task.frame = NULL;
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
    // Sync frame: same helping discipline as the channel green path —
    // help first, sleep only while announced on `gparked`.
    for (;;) {
        if (fr->has_value) break;
        pthread_mutex_unlock(&join->lock);
        if (zz_worker_help_once()) {
            pthread_mutex_lock(&join->lock);
            continue;
        }
        pthread_mutex_lock(&join->lock);
        if (fr->has_value) break;
        __atomic_add_fetch(&join->gparked, 1, __ATOMIC_ACQ_REL);
        while (!fr->has_value) {
            pthread_cond_wait(&join->cond, &join->lock);
        }
        __atomic_sub_fetch(&join->gparked, 1, __ATOMIC_ACQ_REL);
        break;
    }
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
    for (;;) {
        if (join->completed) break;
        // Helping (audit CRITICAL-1): same discipline as channel recv —
        // run stranded work instead of parking; the lock is released
        // across the run. Announce-then-verify below is unchanged.
        pthread_mutex_unlock(&join->lock);
        if (zz_worker_help_once()) {
            pthread_mutex_lock(&join->lock);
            continue;
        }
        pthread_mutex_lock(&join->lock);
        if (join->completed) break;
        __atomic_add_fetch(&join->sleepers, 1, __ATOMIC_ACQ_REL);
        while (!join->completed) {
            pthread_cond_wait(&join->cond, &join->lock);
        }
        __atomic_sub_fetch(&join->sleepers, 1, __ATOMIC_ACQ_REL);
        break;
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
#include <io.h>
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
#ifdef __linux__
#include <malloc.h>
#endif
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

// net.tcp_read_bytes(stream, max_bytes) → Result<Ok(bytes), Err(msg)>
// Binary-safe read: raw bytes with no UTF-8 decoding (unlike zz_tcp_read,
// which decodes lossy by construction).
zz_value zz_tcp_read_bytes(zz_value stream, zz_value max_bytes, int *err) {
    (void)err;
    if (stream.tag != ZZ_TCP_STREAM || !stream.net || stream.net->closed) {
        return zz_variant_err(zz_str_static("tcp_read_bytes failed: not a stream"));
    }
    size_t cap = max_bytes.tag == ZZ_INT && max_bytes.i > 0 ? (size_t)max_bytes.i : 1024;
    char *buf = (char *)malloc(cap);
    if (!buf) return zz_variant_err(zz_str_static("tcp_read_bytes failed: out of memory"));
    ssize_t n = recv(stream.net->fd, buf, cap, 0);
    if (n < 0) {
        free(buf);
        if (errno == EAGAIN || errno == EWOULDBLOCK) {
            return zz_variant_err(zz_str_static("tcp_read_bytes failed: timed out"));
        }
        char msg[192];
        int m = snprintf(msg, sizeof msg, "tcp_read_bytes failed: %s", strerror(errno));
        return zz_variant_err(zz_str_owned(copy_cstr(msg, (size_t)m)));
    }
    if (n == 0) {
        free(buf);
        return zz_variant_err(zz_str_static("tcp_read_bytes failed: connection closed"));
    }
    zz_value out = zz_bytes_new((const unsigned char *)buf, (size_t)n);
    free(buf);
    if (out.tag != ZZ_BYTES) {
        return zz_variant_err(zz_str_static("tcp_read_bytes failed: out of memory"));
    }
    return zz_variant_ok(out);
}

// net.tcp_write_bytes(stream, bytes) → Result<Ok(int), Err(msg)>
zz_value zz_tcp_write_bytes(zz_value stream, zz_value data, int *err) {
    (void)err;
    if (stream.tag != ZZ_TCP_STREAM || !stream.net || stream.net->closed) {
        return zz_variant_err(zz_str_static("tcp_write_bytes failed: not a stream"));
    }
    if (data.tag != ZZ_BYTES || !data.bytes) {
        return zz_variant_err(zz_str_static("tcp_write_bytes failed: expected bytes"));
    }
    const unsigned char *p = data.bytes->buf->data + data.bytes->off;
    size_t len = data.bytes->len;
    size_t total = 0;
    while (total < len) {
        ssize_t w = send(stream.net->fd, p + total, len - total, 0);
        if (w <= 0) {
            char buf[192];
            int n = snprintf(buf, sizeof buf, "tcp_write_bytes failed: %s", strerror(errno));
            return zz_variant_err(zz_str_owned(copy_cstr(buf, (size_t)n)));
        }
        total += (size_t)w;
    }
    return zz_variant_ok((zz_value){ZZ_INT, {.i = (int64_t)total}});
}

// net.tcp_shutdown(stream) → Result<Ok(unit), Err(msg)>
// Real shutdown: both directions stop. The fd stays valid for
// address queries; later I/O fails naturally at the syscall layer.
zz_value zz_tcp_shutdown(zz_value stream, int *err) {
    (void)err;
    if (stream.tag != ZZ_TCP_STREAM || !stream.net || stream.net->closed) {
        return zz_variant_err(zz_str_static("tcp_shutdown failed: not a stream"));
    }
#ifdef ZZ_OS_WINDOWS
    int how = SD_BOTH;
#else
    int how = SHUT_RDWR;
#endif
    if (shutdown(stream.net->fd, how) != 0 && errno != ENOTCONN) {
        char msg[192];
        int m = snprintf(msg, sizeof msg, "tcp_shutdown failed: %s", strerror(errno));
        return zz_variant_err(zz_str_owned(copy_cstr(msg, (size_t)m)));
    }
    return zz_variant_ok(zz_unit());
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

// ---- SQL backends (sqlite embedded, postgres via staticlib) ----
//
// `ZZ_DB` values point at a heap handle recording the backend, so
// `sqlz.*`/`pg.*` query/exec/close dispatch at runtime (the URL scheme
// is only sniffed once, at open). Postgres symbols come from
// `libzz_native_rt.a`, which the build links whenever any sqlz/pg
// native is reachable (see `needs_native_rt`).
typedef enum { ZZDB_CLOSED = -1, ZZDB_SQLITE = 0, ZZDB_PG = 1 } zz_db_backend;
typedef struct {
    int backend;
    union {
        void *sqlite;   // sqlite3* (ZZDB_SQLITE)
        uint64_t pg_id; // staticlib pool id (ZZDB_PG)
    };
} zz_db_handle;

// Postgres backend (Rust staticlib). Weak imports: programs that never
// touch PG link WITHOUT the staticlib (see `needs_native_rt`), so these
// resolve to NULL there; any binary that can actually hold a PG handle
// links the staticlib (the gate keys off sqlz/pg reachability) and sees
// them. Guards live at the two creation sites below — a live PG handle
// proves linkage, so use sites call directly.
#if defined(__APPLE__)
#define ZZ_WEAK_IMPORT __attribute__((weak_import))
#else
#define ZZ_WEAK_IMPORT __attribute__((weak))
#endif
uint64_t zz_pg_connect_raw(const char *info, size_t len) ZZ_WEAK_IMPORT;
int64_t zz_pg_exec_raw(uint64_t id, const char *sql, size_t len, const zz_value *binds, size_t nbinds) ZZ_WEAK_IMPORT;
zz_value zz_pg_query_raw(uint64_t id, const char *sql, size_t len, const zz_value *binds, size_t nbinds) ZZ_WEAK_IMPORT;
void zz_pg_close_raw(uint64_t id) ZZ_WEAK_IMPORT;

// `pg.connect(conninfo)` — URL or keyword form (the driver parses both);
// `ZZ_DB`-NULL on failure, mirroring the SQLite open leniency.
zz_value zz_pg_connect(zz_value info, int *err) {
    (void)err;
    if (info.tag != ZZ_STR || !info.s) return (zz_value){ZZ_DB, {.db = NULL}};
    if (!zz_pg_connect_raw) return (zz_value){ZZ_DB, {.db = NULL}};
    uint64_t id = zz_pg_connect_raw(zz_str_cptr(info.s), info.s->len);
    if (id == 0) return (zz_value){ZZ_DB, {.db = NULL}};
    zz_db_handle *h = (zz_db_handle *)malloc(sizeof(zz_db_handle));
    if (!h) {
        zz_pg_close_raw(id);
        return (zz_value){ZZ_DB, {.db = NULL}};
    }
    h->backend = ZZDB_PG;
    h->pg_id = id;
    return (zz_value){ZZ_DB, {.db = h}};
}

zz_value zz_db_open(zz_value path, int *err) {
    (void)err;
    if (path.tag != ZZ_STR || !path.s) return (zz_value){ZZ_DB, {.db = NULL}};
    const char *p = zz_str_cptr(path.s);
    // Postgres URLs route to the Rust wire driver (staticlib); the
    // handle enum below records the backend so query/exec/close
    // dispatch without re-sniffing.
    if (strncmp(p, "postgres://", 11) == 0 || strncmp(p, "postgresql://", 13) == 0) {
        if (!zz_pg_connect_raw) return (zz_value){ZZ_DB, {.db = NULL}};
        uint64_t id = zz_pg_connect_raw(p, path.s->len);
        if (id == 0) return (zz_value){ZZ_DB, {.db = NULL}};
        zz_db_handle *h = (zz_db_handle *)malloc(sizeof(zz_db_handle));
        if (!h) return (zz_value){ZZ_DB, {.db = NULL}};
        h->backend = ZZDB_PG;
        h->pg_id = id;
        return (zz_value){ZZ_DB, {.db = h}};
    }
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
    zz_db_handle *h = (zz_db_handle *)malloc(sizeof(zz_db_handle));
    if (!h) {
        sqlite3_close(conn);
        return (zz_value){ZZ_DB, {.db = NULL}};
    }
    h->backend = ZZDB_SQLITE;
    h->sqlite = conn;
    return (zz_value){ZZ_DB, {.db = h}};
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
    zz_db_handle *h = (zz_db_handle *)db.db;
    // Closed tombstones (and NULL handles above) degrade to empty
    // results — never touch a freed backend through a stale value.
    if (h->backend != ZZDB_SQLITE && h->backend != ZZDB_PG) {
        return zz_int(0);
    }
    if (h->backend == ZZDB_PG) {
        int64_t n = zz_pg_exec_raw(h->pg_id, sql, strlen(sql), binds, nbinds);
        // The Rust side reports failure as -1 (a bare 0 is a legitimate
        // "no rows changed"); flag it exactly like the SQLite path so
        // inlined transactions roll back.
        if (n < 0) {
            zz_tx_set_error();
            n = 0;
        }
        return zz_int(n);
    }
#ifdef ZZ_HAS_SQLITE3
    sqlite3 *conn = (sqlite3 *)h->sqlite;
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
    zz_db_handle *h = (zz_db_handle *)db.db;
    // Closed tombstones (and NULL handles above) degrade to empty
    // results — never touch a freed backend through a stale value.
    if (h->backend != ZZDB_SQLITE && h->backend != ZZDB_PG) {
        return out;
    }
    if (h->backend == ZZDB_PG) {
        zz_release(&out);
        return zz_pg_query_raw(h->pg_id, sql, strlen(sql), binds, nbinds);
    }
#ifdef ZZ_HAS_SQLITE3
    sqlite3 *conn = (sqlite3 *)h->sqlite;
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
    if (db.tag == ZZ_DB && db.db) {
        zz_db_handle *h = (zz_db_handle *)db.db;
        if (h->backend == ZZDB_PG) {
            zz_pg_close_raw(h->pg_id);
        }
#ifdef ZZ_HAS_SQLITE3
        else if (h->backend == ZZDB_SQLITE) {
            sqlite3_close((sqlite3 *)h->sqlite);
        }
#else
        (void)0;
#endif
        // Tombstone, don't free: the ZZ value still points here and a
        // use-after-close must degrade to empty results (like the VM's
        // error), never a dangling read. One 16-byte shell per close.
        h->backend = ZZDB_CLOSED;
        h->sqlite = NULL;
    }
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
//    CONN_READING → CONN_READING_BODY → dispatch → CONN_WRITING → CONN_CLOSED
//    (always-Connection: close, mirroring the VM; the KEEP_ALIVE state
//    is retained for future use but currently unreachable).

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
    CONN_READING_BODY = 6,
} ConnState;

// Head/body ceilings mirror the VM (Registry V2 G1: multi-MB publishes;
// anything larger is rejected before buffering).
#define HTTP_MAX_HEAD_BYTES (64 * 1024)
#define HTTP_MAX_BODY_BYTES (50 * 1024 * 1024)
// Responses larger than this after a big request return heap to the OS
// (same 8 MB tripwire the VM uses after large publishes).
#define HTTP_TRIM_AFTER_BYTES (8 * 1024 * 1024)

typedef struct {
    int     fd;
    int     state;
    char    read_buf[READ_BUF_SIZE];
    char    write_buf[WRITE_BUF_SIZE];
    int     read_pos;
    int     write_pos;
    int     response_len;
    int     keep_alive;
    // Step-2 dispatch state: grown head buffer when headers exceed
    // read_buf (NULL = using read_buf), exact Content-Length body
    // buffer, and the malloc'd response. All freed on free/reset.
    char   *head_dyn;
    size_t  head_len;
    size_t  head_cap;
    size_t  head_end;   // offset just past \r\n\r\n once found (0 = not yet)
    char   *body_buf;
    size_t  body_len;
    size_t  body_need;
    char   *wbuf;
    size_t  wlen;
    size_t  wpos;
    int     head_too_large; // sticky: headers exceeded the 64 KB cap
    int     body_truncated; // sticky: EOF before the full body arrived
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
    c->head_dyn = NULL;
    c->head_len = 0;
    c->head_cap = 0;
    c->head_end = 0;
    c->body_buf = NULL;
    c->body_len = 0;
    c->body_need = 0;
    c->wbuf = NULL;
    c->wlen = 0;
    c->wpos = 0;
    c->head_too_large = 0;
    c->body_truncated = 0;
    return c;
}

// Release per-request dynamic buffers (idempotent: safe on reset paths).
static void http_conn_free_dyn(Connection *c) {
    if (c->head_dyn) { free(c->head_dyn); c->head_dyn = NULL; }
    if (c->body_buf) { free(c->body_buf); c->body_buf = NULL; }
    if (c->wbuf) { free(c->wbuf); c->wbuf = NULL; }
    c->head_len = 0;
    c->head_cap = 0;
    c->head_end = 0;
    c->body_len = 0;
    c->body_need = 0;
    c->wlen = 0;
    c->wpos = 0;
    c->head_too_large = 0;
    c->body_truncated = 0;
}

static void free_connection(Connection *c) {
    if (c->fd >= 0) {
        // fprintf(stderr, "closing fd=%d\n", c->fd);
        close(c->fd);
        c->fd = -1;
    }
    http_conn_free_dyn(c);
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

// ---- HTTP head/body buffering (Step-2 true-AOT dispatch) ----
//
// The static read_buf serves heads up to 8 KB on the fast path; larger
// header blocks grow a dynamic buffer up to HTTP_MAX_HEAD_BYTES (431
// beyond, mirroring the VM). Bodies buffer exactly Content-Length bytes
// up to HTTP_MAX_BODY_BYTES (413 beyond). Truncated bodies (EOF first)
// reply 400. Request parsing + ZZ dispatch live after the http natives
// below (they need the route table); the entry points are fwd-declared.
static void http_on_head(Connection *c);
static void http_run_handler(Connection *c);
static void http_reply(Connection *c, long status, const char *body, size_t body_len);

// Active head bytes: dynamic buffer when grown, else read_buf/read_pos.
static const char *http_head_buf(Connection *c) {
    return c->head_dyn ? c->head_dyn : c->read_buf;
}

static size_t http_head_len(Connection *c) {
    return c->head_dyn ? c->head_len : (size_t)c->read_pos;
}

// Offset just past the first \r\n\r\n, or 0 when incomplete.
static size_t http_head_end(const char *buf, size_t len) {
    if (len < 4) return 0;
    void *end = memmem(buf, len, "\r\n\r\n", 4);
    if (!end) return 0;
    return (size_t)((const char *)end - buf) + 4;
}

// Append fresh socket bytes to the head buffer, growing past read_buf up
// to the cap. Returns 1 with head_end set when the head is complete,
// 0 while incomplete, -1 when the cap is exceeded (caller replies 431).
static int http_head_append(Connection *c, const char *data, size_t n) {
    size_t cur = http_head_len(c);
    if (cur + n > HTTP_MAX_HEAD_BYTES) {
        c->head_too_large = 1;
        return -1;
    }
    if (!c->head_dyn && cur + n <= READ_BUF_SIZE - 1) {
        memcpy(c->read_buf + cur, data, n);
        c->read_pos = (int)(cur + n);
        c->read_buf[c->read_pos] = '\0';
    } else {
        size_t need = cur + n + 1;
        size_t cap = c->head_cap ? c->head_cap : 16384;
        while (cap < need) cap *= 2;
        if (cap > HTTP_MAX_HEAD_BYTES + 1) cap = HTTP_MAX_HEAD_BYTES + 1;
        char *nb = (char *)realloc(c->head_dyn, cap);
        if (!nb) {
            c->head_too_large = 1;
            return -1;
        }
        if (!c->head_dyn) memcpy(nb, c->read_buf, cur);
        memcpy(nb + cur, data, n);
        c->head_dyn = nb;
        c->head_cap = cap;
        c->head_len = cur + n;
        c->head_dyn[c->head_len] = '\0';
    }
    size_t end = http_head_end(http_head_buf(c), http_head_len(c));
    if (end > 0) {
        c->head_end = end;
        return 1;
    }
    return 0;
}

// Parse "METHOD TARGET HTTP/x" from the head. Returns 1 with borrowed
// method/target spans (method must be A-Z alpha, like the old static
// server's validation; the VM is looser but every real client sends a
// token here and garbage gets a clean 400).
static int http_parse_request_line(const char *head, size_t head_end,
                                   const char **method, size_t *method_len,
                                   const char **target, size_t *target_len) {
    size_t eol = 0;
    while (eol + 1 < head_end && !(head[eol] == '\r' && head[eol + 1] == '\n')) eol++;
    if (eol + 1 >= head_end) return 0;
    // " HTTP/" marker before EOL, method/target split on spaces.
    size_t hs = 0;
    int found = 0;
    for (size_t j = 0; j + 6 <= eol; j++) {
        if (memcmp(head + j, " HTTP/", 6) == 0) { hs = j; found = 1; break; }
    }
    // " HTTP/" marker before EOL: the space AT hs ends the target, an
    // earlier space ends the method.
    if (!found || hs == 0) return 0;
    size_t sp = hs;
    while (sp > 0 && head[sp - 1] != ' ') sp--;
    // Method is [0, sp-1), target is [sp, hs); both non-empty.
    if (sp < 2 || sp >= hs) return 0;
    for (size_t k = 0; k < sp - 1; k++) {
        if (head[k] < 'A' || head[k] > 'Z') return 0;
    }
    *method = head;
    *method_len = sp - 1;
    *target = head + sp;
    *target_len = hs - sp;
    return 1;
}

// ---- Connection state machine ----

static void connection_to_reading(Connection *c) {
    c->state = CONN_READING;
}

static void connection_to_closed(Connection *c) {
    free_connection(c);
}

// ---- Process connection in current state ----

static void process_connection(Connection *c) {
    switch (c->state) {
        case CONN_READING:
            // Head bytes accumulate in read_from_socket; when the
            // terminator lands, http_on_head parses + transitions
            // (direct dispatch, body wait, or error reply).
            if (c->head_end > 0) {
                http_on_head(c);
            } else if (c->head_too_large) {
                http_reply(c, 431, "headers too large", 17);
            }
            break;
        case CONN_READING_BODY:
            if (c->body_truncated) {
                http_reply(c, 400, "truncated body", 14);
            } else if (c->body_len >= c->body_need) {
                http_run_handler(c);
            }
            break;
        case CONN_WRITING:
        case CONN_KEEP_ALIVE:
            // Handled in main loop write phase
            break;
        default:
            break;
    }
}

// ---- Read from socket (state-aware) ----

// Returns 1 while the connection is alive (more reads may follow),
// 0 when it died and the caller should drop it.
static int read_from_socket(Connection *c) {
    if (c->state == CONN_READING_BODY) {
        // Append straight into the exact-size body buffer.
        size_t room = c->body_need - c->body_len;
        if (room == 0) return 1;
        ssize_t n = read(c->fd, c->body_buf + c->body_len, room);
        if (n > 0) {
            c->body_len += (size_t)n;
            return 1;
        }
        if (n == 0) {
            // EOF before the full body: sticky 400 (but only if the
            // body is actually incomplete — a pipelined close after a
            // complete body still dispatches).
            if (c->body_len < c->body_need) c->body_truncated = 1;
            return 1;
        }
        if (errno == EAGAIN || errno == EWOULDBLOCK) return 1;
        return 0;
    }
    // Head phase: fill the static buffer, growing past it when needed.
    char tmp[8192];
    ssize_t n = read(c->fd, tmp, sizeof tmp);
    if (n > 0) {
        int rc = http_head_append(c, tmp, (size_t)n);
        if (rc < 0) return 1; // cap exceeded; process_connection replies 431
        return 1;
    }
    if (n == 0) return 0; // client closed
    if (errno == EAGAIN || errno == EWOULDBLOCK) return 1;
    return 0;
}

// ---- Write to socket ----

// True while response bytes remain unsent (dynamic wbuf when the
// dispatcher built one, else the legacy static buffer).
static int http_write_pending(Connection *c) {
    if (c->wbuf) return c->wpos < c->wlen;
    return c->write_pos < c->response_len;
}

static int write_to_socket(Connection *c) {
    if (c->wbuf) {
        if (c->wpos >= c->wlen) return 1;
        ssize_t n = write(c->fd, c->wbuf + c->wpos, c->wlen - c->wpos);
        if (n > 0) {
            c->wpos += (size_t)n;
            return 1;
        }
        if (n == 0) return 0;
        if (errno == EAGAIN || errno == EWOULDBLOCK) return 1;
        return 0;
    }
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
// Linux-only: epoll + fork exist nowhere else. Other targets (macOS, Windows)
// get a clear runtime error instead of a compile wall — the AOT HTTP server
// never supported them; plain `zz run` (VM) remains the portable path.
#ifdef __linux__
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
                        if (c->state != CONN_READING && c->state != CONN_KEEP_ALIVE
                            && c->state != CONN_READING_BODY) {
                            break;
                        }
                    }
                    // If socket closed or error
                    if (c->fd < 0) {
                        continue;
                    }
                }

                if (c->state == CONN_WRITING) {
                    // Edge-triggered: write all data until EAGAIN
                    while (http_write_pending(c)) {
                        int written = write_to_socket(c);
                        if (!written) {
                            connection_to_closed(c);
                            epoll_ctl(epfd, EPOLL_CTL_DEL, fd, NULL);
                            break;
                        }
                    }
                    // Check if write complete
                    if (c->fd >= 0 && !http_write_pending(c)) {

                        if (!c->keep_alive) {
                            connection_to_closed(c);
                            epoll_ctl(epfd, EPOLL_CTL_DEL, fd, NULL);
                        } else {
                            // Reset for keep-alive
                            c->state = CONN_KEEP_ALIVE;
                            c->read_pos = 0;
                            c->write_pos = 0;
                            c->response_len = 0;
                            http_conn_free_dyn(c);
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
#else
// Non-Linux fallback: no fork/epoll here. Emits a clear error so a native
// binary fails loudly at `listen` time instead of not compiling at all.
// Uses only stdio so it builds on every target (incl. Windows/MSVC).
static int spawn_workers(int listen_fd, int port) {
    (void)listen_fd;
    (void)port;
    fprintf(stderr,
        "zz_http_listen: the AOT HTTP server requires Linux (fork+epoll); "
        "use `zz run` (VM) on this platform\n");
    return -1;
}
#endif

// ---- HTTP routes + request/response natives (AOT) ----
//
// Route handlers ARE first-class here: AOT lowers closures to C function
// pointers (`zz_closure_make_ex`), so `http.test` (in-process dispatch)
// matches routes and calls them exactly like the VM does. The epoll
// workers (`zz_http_listen`) dispatch socket requests through the same
// table (true-AOT C handlers — no VM involved).

// Route table: method + pattern (+ `:param` segments / "*" wildcard) with
// the ZZ closure value (retained). Single global table: one server per
// process, same as the VM's single-interp discipline for `zz run`.
#define HTTP_MAX_ROUTES 64
#define HTTP_MAX_MIDDLEWARE 16
typedef struct {
    char *method;
    char *pattern;
    zz_value handler;
} http_route_entry;
static http_route_entry g_http_route_table[HTTP_MAX_ROUTES];
static int g_http_route_count = 0;
static zz_value g_http_middlewares[HTTP_MAX_MIDDLEWARE];
static int g_http_middleware_count = 0;
// Access-log flag for the socket serve path (http.log).
static int g_http_log_enabled = 0;

// zz_http_server(unused, err) — creates an HTTP server handle (int 0;
// routes live in the global table above).
zz_value zz_http_server(zz_value unused, int *err) {
    (void)unused;
    *err = 0;
    return zz_int(0);
}

// Shared route registration for GET/POST/PUT/DELETE (the codegen maps all
// four spellings here; the method is recorded per entry).
static zz_value http_route_add(const char *method, zz_value path, zz_value handler, int *err) {
    *err = 0;
    if (path.tag != ZZ_STR || !path.s) return zz_int(0);
    if (g_http_route_count >= HTTP_MAX_ROUTES - 1) return zz_int(0);
    http_route_entry *e = &g_http_route_table[g_http_route_count++];
    e->method = strndup(method, strlen(method));
    e->pattern = strndup(zz_str_cptr(path.s), path.s->len);
    e->handler = zz_clone(handler);
    return zz_int(0);
}

// zz_http_route(server, method, path, handler, err) — validated
// single-entry routing. Unknown methods are a loud error (not a silently
// dropped route).
zz_value zz_http_route(zz_value server, zz_value method, zz_value path, zz_value handler, int *err) {
    (void)server;
    if (method.tag != ZZ_STR || !method.s) {
        *err = 1;
        return zz_variant_err(zz_str_static("std.http.route: method must be a string"));
    }
    const char *m = zz_str_cptr(method.s);
    size_t mlen = method.s->len;
    // Upper-copy so `route(s, "get", …)` behaves like the VM (which
    // uppercases before matching). Methods are a few bytes; stack is fine.
    char up[16];
    const char *known = NULL;
    if (mlen < sizeof up) {
        for (size_t i = 0; i < mlen; i++) {
            char c = m[i];
            up[i] = (c >= 'a' && c <= 'z') ? (char)(c - 32) : c;
        }
        up[mlen] = '\0';
        if (mlen == 3 && memcmp(up, "GET", 3) == 0) known = "GET";
        else if (mlen == 4 && memcmp(up, "POST", 4) == 0) known = "POST";
        else if (mlen == 3 && memcmp(up, "PUT", 3) == 0) known = "PUT";
        else if (mlen == 6 && memcmp(up, "DELETE", 6) == 0) known = "DELETE";
    }
    if (!known) {
        *err = 1;
        return zz_variant_err(zz_str_static("std.http.route: unknown method (expected GET, POST, PUT or DELETE)"));
    }
    return http_route_add(known, path, handler, err);
}

// zz_http_route_get(server, path, handler, err) — records method+pattern+closure.
zz_value zz_http_route_get(zz_value server, zz_value path, zz_value handler, int *err) {
    (void)server;
    return http_route_add("GET", path, handler, err);
}

// zz_http_route_post/put/delete — same table, own method.
zz_value zz_http_route_post(zz_value server, zz_value path, zz_value handler, int *err) {
    (void)server;
    return http_route_add("POST", path, handler, err);
}

zz_value zz_http_route_put(zz_value server, zz_value path, zz_value handler, int *err) {
    (void)server;
    return http_route_add("PUT", path, handler, err);
}

zz_value zz_http_route_delete(zz_value server, zz_value path, zz_value handler, int *err) {
    (void)server;
    return http_route_add("DELETE", path, handler, err);
}

// ---- shared request/response helpers ----

// Forward declarations (http_response_new precedes the parse helpers).
static zz_value http_json_field(zz_str *body);

// Response object constructor shared by respond/test paths.
// `body` is shared (retained under both "body" and "text", same as the
// client builder); `headers` is retained.
static zz_value http_response_new(long status, zz_str *body, zz_value headers) {
    zz_value names[5];
    names[0] = zz_str_static("status");
    names[1] = zz_str_static("body");
    names[2] = zz_str_static("headers");
    names[3] = zz_str_static("text");
    names[4] = zz_str_static("json");
    zz_value resp = zz_object_new("http.response", names, 5);
    zz_object_set_field(&resp, "status", (zz_value){ZZ_INT, {.i = (long long)status}});
    zz_object_set_field(&resp, "body", (zz_value){ZZ_STR, {.s = body}});
    zz_object_set_field(&resp, "text", (zz_value){ZZ_STR, {.s = body}});
    zz_object_set_field(&resp, "headers", zz_clone(headers));
    zz_object_set_field(&resp, "json", http_json_field(body));
    return resp;
}

// "field" sub-value of a dict (unit when absent/non-dict). Returns an
// OWNED clone (zz_object_get_field retains) — callers must release.
static zz_value http_dict_field(zz_value dict, const char *field) {
    if (dict.tag != ZZ_DICT || !dict.dict) return zz_unit();
    return zz_object_get_field(&dict, field);
}

// Find a str entry by exact key; returns 1 + sets *out (borrowed).
static int http_dict_find_str(zz_value dict, const char *key, zz_value *out) {
    if (dict.tag != ZZ_DICT || !dict.dict) return 0;
    size_t n = dict.dict->len;
    for (size_t i = 0; i < n; i++) {
        zz_str *k = dict.dict->entries[i].key;
        zz_value v = dict.dict->entries[i].val;
        if (k && v.tag == ZZ_STR && v.s
            && strlen(key) == k->len && memcmp(key, zz_str_cptr(k), k->len) == 0) {
            *out = v;
            return 1;
        }
    }
    return 0;
}

// Case-insensitive key lookup over a dict; any value type (mirrors the VM
// header fallback, which returns the value as-is). Returns 1 + *out.
static int http_dict_find_ci(zz_value dict, const char *key, zz_value *out) {
    if (dict.tag != ZZ_DICT || !dict.dict) return 0;
    size_t n = dict.dict->len;
    for (size_t i = 0; i < n; i++) {
        zz_str *k = dict.dict->entries[i].key;
        if (k && strlen(key) == k->len
            && strcasecmp(key, zz_str_cptr(k)) == 0) {
            *out = dict.dict->entries[i].val;
            return 1;
        }
    }
    return 0;
}

// Split "a=1&b=&c" into a str->str dict. No percent-decoding and no pair
// filtering — mirrors the VM's parse_query_string exactly (every &-piece
// yields a pair; missing '=' means empty value).
static zz_value http_parse_query(const char *qs, size_t len) {
    zz_value d = zz_dict_new();
    if (len == 0) return d;
    size_t start = 0;
    for (size_t i = 0; i <= len; i++) {
        if (i == len || qs[i] == '&') {
            size_t piece_len = i - start;
            const char *eq = memchr(qs + start, '=', piece_len);
            size_t klen, vlen;
            const char *vstart;
            if (eq) {
                klen = (size_t)(eq - (qs + start));
                vstart = eq + 1;
                vlen = piece_len - klen - 1;
            } else {
                klen = piece_len;
                vstart = qs + start + piece_len;
                vlen = 0;
            }
            zz_str *k = str_alloc(klen);
            memcpy(zz_str_ptr(k), qs + start, klen);
            zz_str_ptr(k)[klen] = '\0';
            k->len = klen;
            zz_str *v = str_alloc(vlen);
            memcpy(zz_str_ptr(v), vstart, vlen);
            zz_str_ptr(v)[vlen] = '\0';
            v->len = vlen;
            zz_dict_set(d.dict, (zz_value){ZZ_STR, {.s = k}}, (zz_value){ZZ_STR, {.s = v}});
            start = i + 1;
        }
    }
    return d;
}

// Match "/users/:id" against "/users/42" (length-aware, no copying).
// Returns 1 + fills params dict, or 0. Mirrors the VM's match_route_pattern
// (trim '/', equal segment counts, ':x' captures, '*' matches one segment).
static int http_match_pattern(const char *pat, size_t patlen, const char *path, size_t pathlen,
                              zz_value params) {
    while (patlen > 0 && pat[0] == '/') { pat++; patlen--; }
    while (patlen > 0 && pat[patlen - 1] == '/') patlen--;
    while (pathlen > 0 && path[0] == '/') { path++; pathlen--; }
    while (pathlen > 0 && path[pathlen - 1] == '/') pathlen--;
    // Segment counts (Rust "".split('/') yields one empty segment).
    size_t pn = 1, an = 1;
    for (size_t i = 0; i < patlen; i++) if (pat[i] == '/') pn++;
    for (size_t i = 0; i < pathlen; i++) if (path[i] == '/') an++;
    if (pn != an) return 0;
    size_t pi = 0, ai = 0;
    for (size_t s = 0; s < pn; s++) {
        size_t pj = pi, aj = ai;
        while (pj < patlen && pat[pj] != '/') pj++;
        while (aj < pathlen && path[aj] != '/') aj++;
        size_t plen = pj - pi, alen = aj - ai;
        if (plen > 0 && pat[pi] == ':') {
            zz_str *k = str_alloc(plen - 1);
            memcpy(zz_str_ptr(k), pat + pi + 1, plen - 1);
            zz_str_ptr(k)[plen - 1] = '\0';
            k->len = plen - 1;
            zz_str *v = str_alloc(alen);
            memcpy(zz_str_ptr(v), path + ai, alen);
            zz_str_ptr(v)[alen] = '\0';
            v->len = alen;
            zz_dict_set(params.dict, (zz_value){ZZ_STR, {.s = k}}, (zz_value){ZZ_STR, {.s = v}});
        } else if (!((plen == alen && memcmp(pat + pi, path + ai, plen) == 0)
                     || (plen == 1 && pat[pi] == '*'))) {
            return 0;
        }
        pi = pj + 1;
        ai = aj + 1;
    }
    return 1;
}

// Build an .err(str) variant with a formatted message.
static zz_value http_errf(const char *fmt, const char *arg) {
    char buf[512];
    snprintf(buf, sizeof buf, fmt, arg);
    size_t n = strlen(buf);
    return zz_variant_err(zz_str_new(buf, n));
}

// Move the inner value out of a Result/Option variant: frees the variant
// box only; ownership of the inner's refs transfers to the caller.
// (zz_clone + zz_release is WRONG here — the clone shares the variant
// box and the release frees it, leaving a dangling payload pointer.)
static zz_value http_variant_take(zz_value r) {
    zz_value out = *r.payload;
    free(r.payload);
    return out;
}

// Parse a body string the way the VM's response/body json accessors do:
// always a Result — `.ok(json)` on success (so `res.json()?` works),
// `.err("JSON parse error")` otherwise. (zz_json_parse itself returns a
// wrapped Result; the payload is taken out and re-wrapped to keep one
// uniform shape.)
static zz_value http_parse_body_json(zz_value body) {
    zz_str *s = (body.tag == ZZ_STR && body.s) ? body.s : NULL;
    // Mirror the VM: an empty/missing body fails parsing like any
    // invalid JSON (extract_dict_field_str yields "" for missing).
    zz_value bs = s ? (zz_value){ZZ_STR, {.s = s}} : zz_str_static("");
    int jerr = 0;
    zz_value r = zz_json_parse(bs, &jerr);
    if (r.tag == ZZ_RESULT_OK && r.payload) {
        // Always a Result (like the VM): take the payload out (frees the
        // parse wrapper; ownership transfers) and re-wrap as .ok so
        // `res.json()?` works on both engines. No release: take consumed it.
        zz_value inner = http_variant_take(r);
        return zz_variant_ok(inner);
    }
    zz_release(&r);
    return zz_variant_err(zz_str_static("JSON parse error"));
}

// Stored "json" field for a fresh response object: bare json on success,
// unit when the body isn't valid JSON (the .json() METHOD re-parses and
// yields .err there — see zz_http_response_json).
static zz_value http_json_field(zz_str *body) {
    if (!body || body->len == 0) return zz_unit();
    int jerr = 0;
    zz_value r = zz_json_parse((zz_value){ZZ_STR, {.s = body}}, &jerr);
    if (r.tag == ZZ_RESULT_OK && r.payload) {
        return http_variant_take(r);
    }
    zz_release(&r);
    return zz_unit();
}

// zz_http_respond(status, body, headers) → HttpResponse object.
// Mirrors the VM: int status 100-599, str body, str->str headers dict
// (non-str pairs dropped). Arg violations raise (*err=1).
zz_value zz_http_respond(zz_value status, zz_value body, zz_value headers, int *err) {
    if (status.tag != ZZ_INT || status.i < 100 || status.i > 599) {
        *err = 1;
        return zz_variant_err(zz_str_static("std.http.respond: status out of range (100-599)"));
    }
    if (body.tag != ZZ_STR || !body.s) {
        *err = 1;
        return zz_variant_err(zz_str_static("std.http.respond: body must be a string"));
    }
    if (headers.tag != ZZ_DICT || !headers.dict) {
        *err = 1;
        return zz_variant_err(zz_str_static("std.http.respond: headers must be a dict"));
    }
    *err = 0;
    zz_value hdrs = zz_dict_new();
    size_t n = headers.dict->len;
    for (size_t i = 0; i < n; i++) {
        zz_str *k = headers.dict->entries[i].key;
        zz_value v = headers.dict->entries[i].val;
        if (k && v.tag == ZZ_STR && v.s) {
            zz_dict_set(hdrs.dict, (zz_value){ZZ_STR, {.s = k}}, zz_clone(v));
        }
    }
    zz_value resp = http_response_new((long)status.i, body.s, hdrs);
    zz_release(&hdrs);
    return resp;
}

// zz_http_param(req, name) → .ok(str) | .err("param `x` not found").
zz_value zz_http_param(zz_value req, zz_value name, int *err) {
    if (req.tag != ZZ_DICT || !req.dict) {
        *err = 1;
        return zz_variant_err(zz_str_static("std.http.param: expected a dict"));
    }
    if (name.tag != ZZ_STR || !name.s) {
        *err = 1;
        return zz_variant_err(zz_str_static("std.http.param: expected a str name"));
    }
    *err = 0;
    zz_value params = http_dict_field(req, "params");
    zz_value out;
    int found = http_dict_find_str(params, zz_str_cptr(name.s), &out);
    zz_value ret = found ? zz_variant_ok(zz_clone(out))
                         : http_errf("param `%s` not found", zz_str_cptr(name.s));
    zz_release(&params);
    return ret;
}

// zz_http_query(req) → query dict (empty when absent — mirrors the VM).
zz_value zz_http_query(zz_value req, int *err) {
    if (req.tag != ZZ_DICT || !req.dict) {
        *err = 1;
        return zz_variant_err(zz_str_static("std.http.query: expected a dict"));
    }
    *err = 0;
    zz_value q = http_dict_field(req, "query");
    zz_value ret = (q.tag == ZZ_DICT && q.dict) ? zz_clone(q) : zz_dict_new();
    zz_release(&q);
    return ret;
}

// zz_http_header(req, name) → .ok(value) | .err("header `x` not found").
// Nested "headers" dict first (case-insensitive), then a top-level
// case-insensitive scan — mirrors the VM fallback exactly.
zz_value zz_http_header(zz_value req, zz_value name, int *err) {
    if (req.tag != ZZ_DICT || !req.dict) {
        *err = 1;
        return zz_variant_err(zz_str_static("std.http.header: expected a dict"));
    }
    if (name.tag != ZZ_STR || !name.s) {
        *err = 1;
        return zz_variant_err(zz_str_static("std.http.header: expected a str name"));
    }
    *err = 0;
    const char *nname = zz_str_cptr(name.s);
    zz_value hdrs = http_dict_field(req, "headers");
    zz_value out;
    zz_value ret;
    if (http_dict_find_ci(hdrs, nname, &out)) {
        ret = zz_variant_ok(zz_clone(out));
    } else if (http_dict_find_ci(req, nname, &out)) {
        ret = zz_variant_ok(zz_clone(out));
    } else {
        ret = http_errf("header `%s` not found", nname);
    }
    zz_release(&hdrs);
    return ret;
}

// zz_http_body_json(req) → parsed JSON, or .err on parse failure.
// Missing/non-str body behaves as "" (mirrors extract_dict_field_str).
zz_value zz_http_body_json(zz_value req, int *err) {
    if (req.tag != ZZ_DICT || !req.dict) {
        *err = 1;
        return zz_variant_err(zz_str_static("std.http.body_json: expected a dict"));
    }
    *err = 0;
    zz_value b = http_dict_field(req, "body");
    zz_value ret = http_parse_body_json(b);
    zz_release(&b);
    return ret;
}

// zz_http_body_form(req) → form-decoded dict of the body string.
zz_value zz_http_body_form(zz_value req, int *err) {
    if (req.tag != ZZ_DICT || !req.dict) {
        *err = 1;
        return zz_variant_err(zz_str_static("std.http.body_form: expected a dict"));
    }
    *err = 0;
    zz_value b = http_dict_field(req, "body");
    zz_value ret = zz_dict_new();
    if (b.tag == ZZ_STR && b.s) {
        zz_release(&ret);
        ret = http_parse_query(zz_str_cptr(b.s), b.s->len);
    }
    zz_release(&b);
    return ret;
}

// ---- in-process dispatch (http.test / http.handle) ----

// Find the first route for (method, path): exact match, then :param
// patterns, then per-method "*". Mirrors the VM's find order. Fills
// params (possibly empty). Returns handler or unit when none matched.
// Pattern attempts fill a scratch dict merged only on success — a failed
// attempt must not leak partial captures into a later route's params
// (the VM discards its per-route vec the same way).
static zz_value http_match_route(const char *method, const char *path, size_t pathlen,
                                 zz_value params) {
    for (int i = 0; i < g_http_route_count; i++) {
        http_route_entry *e = &g_http_route_table[i];
        if (strcmp(e->method, method) != 0) continue;
        size_t plen = strlen(e->pattern);
        if (plen == pathlen && memcmp(e->pattern, path, pathlen) == 0) return e->handler;
        zz_value scratch = zz_dict_new();
        int hit = http_match_pattern(e->pattern, plen, path, pathlen, scratch);
        if (hit) {
            for (size_t k = 0; k < scratch.dict->len; k++) {
                zz_str *ek = scratch.dict->entries[k].key;
                zz_value ev = scratch.dict->entries[k].val;
                zz_dict_set(params.dict, (zz_value){ZZ_STR, {.s = ek}}, zz_clone(ev));
            }
            zz_release(&scratch);
            return e->handler;
        }
        zz_release(&scratch);
    }
    for (int i = 0; i < g_http_route_count; i++) {
        http_route_entry *e = &g_http_route_table[i];
        if (strcmp(e->method, method) == 0 && strcmp(e->pattern, "*") == 0) return e->handler;
    }
    return zz_unit();
}

// Build the request dict the VM dispatcher builds: method/path/body
// strings, headers, parsed query, matched params. `headers` is moved
// (adopted into the dict).
static zz_value http_test_request(const char *method, const char *path, size_t pathlen,
                                  zz_value body, zz_value headers, zz_value query, zz_value params) {
    zz_value req = zz_dict_new();
    zz_dict_set(req.dict, zz_str_static("method"), zz_str_new(method, strlen(method)));
    zz_dict_set(req.dict, zz_str_static("path"), zz_str_new(path, pathlen));
    zz_dict_set(req.dict, zz_str_static("body"), zz_clone(body));
    zz_dict_set(req.dict, zz_str_static("headers"), headers);
    zz_dict_set(req.dict, zz_str_static("query"), query);
    zz_dict_set(req.dict, zz_str_static("params"), params);
    return req;
}

// 500 response object with a text message (dispatch-level failures, which
// the VM wraps into a 500 Response rather than raising).
static zz_value http_500(const char *msg, size_t len) {
    zz_str *b = str_alloc(len);
    memcpy(zz_str_ptr(b), msg, len);
    zz_str_ptr(b)[len] = '\0';
    b->len = len;
    zz_value hdrs = zz_dict_new();
    zz_value r = http_response_new(500, b, hdrs);
    zz_release(&hdrs);
    zz_value owned = {ZZ_STR, {.s = b}};
    zz_release(&owned);
    return r;
}

// Wrap a handler return the way the VM dispatcher does: str → 200 text,
// response object → passthrough, other scalars → 200 Display (unit
// renders empty, exactly like the VM), containers → 500 (the VM
// serializes dict/array to JSON here; no value→JSON helper exists in
// the C runtime yet — Step-2 remainder, now socket-visible too).
// Fresh {"Content-Type": "text/plain; charset=utf-8"} dict (the VM's
// default for str/scalar handler results).
static zz_value http_text_plain_hdrs(void) {
    static const char ct_text[] = "text/plain; charset=utf-8";
    zz_value hdrs = zz_dict_new();
    zz_str *ct = str_alloc(sizeof(ct_text) - 1);
    memcpy(zz_str_ptr(ct), ct_text, sizeof(ct_text) - 1);
    zz_str_ptr(ct)[sizeof(ct_text) - 1] = '\0';
    ct->len = sizeof(ct_text) - 1;
    zz_dict_set(hdrs.dict, zz_str_static("Content-Type"), (zz_value){ZZ_STR, {.s = ct}});
    return hdrs;
}

static zz_value http_wrap_result(zz_value r) {
    if (r.tag == ZZ_STR && r.s) {
        zz_value hdrs = http_text_plain_hdrs();
        zz_str *b = str_alloc(r.s->len);
        memcpy(zz_str_ptr(b), zz_str_cptr(r.s), r.s->len);
        zz_str_ptr(b)[r.s->len] = '\0';
        b->len = r.s->len;
        zz_value resp = http_response_new(200, b, hdrs);
        zz_release(&hdrs);
        // http_response_new retains the body under both fields; drop the
        // fresh allocation's own ref so the object owns exactly two.
        zz_value owned = {ZZ_STR, {.s = b}};
        zz_release(&owned);
        return resp;
    }
    if (r.tag == ZZ_OBJECT) {
        zz_value st = zz_object_get_field(&r, "status");
        int is_resp = (st.tag == ZZ_INT);
        zz_release(&st);
        if (is_resp) return zz_clone(r);
    }
    if (r.tag != ZZ_ARRAY && r.tag != ZZ_DICT && r.tag != ZZ_JSON) {
        // Other scalars (int/float/bool/unit/...) → 200 Display, exactly
        // like the VM's `format!("{other}")` fallback.
        char *disp = zz_value_to_string(&r);
        size_t dl = strlen(disp);
        zz_str *b = str_alloc(dl);
        memcpy(zz_str_ptr(b), disp, dl);
        zz_str_ptr(b)[dl] = '\0';
        b->len = dl;
        free(disp);
        zz_value hdrs = http_text_plain_hdrs();
        zz_value resp = http_response_new(200, b, hdrs);
        zz_release(&hdrs);
        zz_value owned = {ZZ_STR, {.s = b}};
        zz_release(&owned);
        return resp;
    }
    const char *msg = "http.test: unsupported handler return type (Step-2: dict/array JSON)";
    return http_500(msg, strlen(msg));
}

// Shared dispatch core: match, middleware chain, handler call. Returns an
// http.response object in all cases (VM http.test semantics). Sets
// *failed when the 500 came from the dispatcher itself (no route,
// middleware violation) as opposed to the handler — http.handle maps the
// former to .err and the latter to .ok, exactly like the VM.
static zz_value http_dispatch(const char *method, const char *path, size_t pathlen,
                              zz_value body, zz_value headers, int *failed) {
    *failed = 0;
    // Split off '?query' (first '?' wins, like the VM's split_once).
    const char *qm = memchr(path, '?', pathlen);
    size_t plen = qm ? (size_t)(qm - path) : pathlen;
    zz_value query = (qm && (size_t)(qm - path) < pathlen)
        ? http_parse_query(qm + 1, pathlen - (plen + 1))
        : zz_dict_new();
    zz_value params = zz_dict_new();
    zz_value handler = http_match_route(method, path, plen, params);
    if (handler.tag == ZZ_UNIT) {
        char msg[256];
        snprintf(msg, sizeof msg, "std.http: no route for %s %.*s", method, (int)plen, path);
        zz_release(&query);
        zz_release(&params);
        *failed = 1;
        return http_500(msg, strlen(msg));
    }
    zz_value req = http_test_request(method, path, plen, body, headers, query, params);
    // Middleware chain in registration order.
    for (int i = 0; i < g_http_middleware_count; i++) {
        zz_value r = zz_call_closure(g_http_middlewares[i], &req, 1);
        if (r.tag == ZZ_RESULT_OK && r.payload) {
            zz_value inner = *r.payload;
            if (inner.tag == ZZ_DICT && inner.dict) {
                zz_release(&req);
                req = zz_clone(inner);
                zz_release(&r);
                continue;
            }
            char *disp = zz_value_to_string(&inner);
            char msg[256];
            snprintf(msg, sizeof msg,
                     "std.http: middleware returned `%.200s`, expected .ok or .err", disp);
            free(disp);
            zz_release(&req);
            zz_release(&r);
            *failed = 1;
            return http_500(msg, strlen(msg));
        }
        if (r.tag == ZZ_RESULT_ERR && r.payload) {
            zz_value inner = *r.payload;
            if (inner.tag == ZZ_OBJECT) {
                zz_value st = zz_object_get_field(&inner, "status");
                int is_resp = (st.tag == ZZ_INT);
                zz_release(&st);
                if (is_resp) {
                    zz_release(&req);
                    zz_value out = zz_clone(inner);
                    zz_release(&r);
                    return out;
                }
            }
            char *disp = zz_value_to_string(&inner);
            // 401 with the displayed value as the body.
            size_t dl = strlen(disp);
            zz_str *bb = str_alloc(dl);
            memcpy(zz_str_ptr(bb), disp, dl);
            zz_str_ptr(bb)[dl] = '\0';
            bb->len = dl;
            free(disp);
            zz_value hdrs = zz_dict_new();
            zz_value resp = http_response_new(401, bb, hdrs);
            zz_release(&hdrs);
            zz_release(&req);
            zz_release(&r);
            return resp;
        }
        // Non-Result middleware return.
        {
            char *disp = zz_value_to_string(&r);
            char msg[256];
            snprintf(msg, sizeof msg,
                     "std.http: middleware returned `%.200s`, expected .ok or .err", disp);
            free(disp);
            zz_release(&req);
            zz_release(&r);
            *failed = 1;
            return http_500(msg, strlen(msg));
        }
    }
    zz_value r = zz_call_closure(handler, &req, 1);
    zz_release(&req);
    zz_value out = http_wrap_result(r);
    zz_release(&r);
    return out;
}

// ---- socket serve path (Step-2 true-AOT dispatch) ----
//
// The epoll workers parse real requests and run them through the same
// route table + middleware + ZZ closures as http.test (http_dispatch).
// Semantics mirror the VM's handle_connection_thread: 64 KB head cap
// (431), exact Content-Length bodies up to 50 MB (413), 100-continue,
// dispatch errors as 500, always-Connection: close, colored log lines,
// malloc_trim after >8 MB requests.

// Reason phrases — identical table to the VM's http_reason.
static const char *http_reason_phrase(long status) {
    switch (status) {
        case 100: return "Continue";
        case 200: return "OK";
        case 201: return "Created";
        case 204: return "No Content";
        case 301: return "Moved Permanently";
        case 304: return "Not Modified";
        case 400: return "Bad Request";
        case 401: return "Unauthorized";
        case 403: return "Forbidden";
        case 404: return "Not Found";
        case 405: return "Method Not Allowed";
        case 413: return "Payload Too Large";
        case 431: return "Headers Too Large";
        case 500: return "Internal Server Error";
        default: return "Error";
    }
}

// Trim ASCII spaces/tabs (plus the \r left by \n-splitting) in place.
static void http_trim_span(const char **s, size_t *n) {
    while (*n > 0 && (**s == ' ' || **s == '\t' || **s == '\r')) { (*s)++; (*n)--; }
    while (*n > 0 && ((*s)[*n - 1] == ' ' || (*s)[*n - 1] == '\t' || (*s)[*n - 1] == '\r')) (*n)--;
}

// Scan the head for Content-Length (last wins; garbage → 0, like the
// VM's parse().unwrap_or(0)) and Expect: 100-continue. The request
// line itself is skipped so an absolute-form target (which contains
// ':') can never pollute the scan.
static void http_scan_head(const char *head, size_t head_end,
                           size_t *content_length, int *expect_continue) {
    *content_length = 0;
    *expect_continue = 0;
    size_t pos = 0;
    while (pos + 1 < head_end && !(head[pos] == '\r' && head[pos + 1] == '\n')) pos++;
    if (pos + 1 < head_end) pos += 2;
    while (pos + 1 < head_end) {
        size_t eol = pos;
        while (eol + 1 < head_end && !(head[eol] == '\r' && head[eol + 1] == '\n')) eol++;
        if (eol == pos) break;
        const char *colon = memchr(head + pos, ':', eol - pos);
        if (colon) {
            const char *ns = head + pos;
            size_t nn = (size_t)(colon - (head + pos));
            const char *vs = colon + 1;
            size_t vn = eol - (size_t)(colon + 1 - head);
            if (nn == 14 && strncasecmp(ns, "content-length", 14) == 0) {
                http_trim_span(&vs, &vn);
                size_t v = 0;
                int ok = vn > 0;
                for (size_t k = 0; k < vn && ok; k++) {
                    if (vs[k] < '0' || vs[k] > '9') ok = 0;
                    else v = v * 10 + (size_t)(vs[k] - '0');
                }
                *content_length = ok ? v : 0;
            } else if (nn == 6 && strncasecmp(ns, "expect", 6) == 0) {
                http_trim_span(&vs, &vn);
                if (vn == 12 && strncasecmp(vs, "100-continue", 12) == 0) *expect_continue = 1;
            }
        }
        pos = eol + 2;
    }
}

// Build the headers dict from the head (first-':' split, trimmed
// pairs, colon-less lines skipped — the VM's extract_headers exactly).
static zz_value http_build_headers_dict(const char *head, size_t head_end) {
    zz_value d = zz_dict_new();
    size_t pos = 0;
    while (pos + 1 < head_end && !(head[pos] == '\r' && head[pos + 1] == '\n')) pos++;
    if (pos + 1 < head_end) pos += 2;
    while (pos + 1 < head_end) {
        size_t eol = pos;
        while (eol + 1 < head_end && !(head[eol] == '\r' && head[eol + 1] == '\n')) eol++;
        if (eol == pos) break;
        const char *colon = memchr(head + pos, ':', eol - pos);
        if (colon) {
            const char *ns = head + pos;
            size_t nn = (size_t)(colon - (head + pos));
            const char *vs = colon + 1;
            size_t vn = eol - (size_t)(colon + 1 - head);
            http_trim_span(&ns, &nn);
            http_trim_span(&vs, &vn);
            zz_str *k = str_alloc(nn);
            memcpy(zz_str_ptr(k), ns, nn);
            zz_str_ptr(k)[nn] = '\0';
            k->len = nn;
            zz_str *v = str_alloc(vn);
            memcpy(zz_str_ptr(v), vs, vn);
            zz_str_ptr(v)[vn] = '\0';
            v->len = vn;
            zz_dict_set(d.dict, (zz_value){ZZ_STR, {.s = k}}, (zz_value){ZZ_STR, {.s = v}});
        }
        pos = eol + 2;
    }
    return d;
}

// Serialize a dispatch response object to wire bytes (malloc'd, *out_len
// set). Views the body in place — the caller must keep resp alive until
// the copy completes (it does: release happens after).
static char *http_serialize_response(zz_value *resp, size_t *out_len) {
    long status = 500;
    zz_value st = zz_object_get_field(resp, "status");
    if (st.tag == ZZ_INT) status = (long)st.i;
    zz_release(&st);
    const char *body = "";
    size_t body_len = 0;
    zz_value t = zz_object_get_field(resp, "body");
    if (t.tag == ZZ_STR && t.s) {
        body = zz_str_cptr(t.s);
        body_len = t.s->len;
    }
    const char *reason = http_reason_phrase(status);
    zz_value h = zz_object_get_field(resp, "headers");
    size_t xlen = 0;
    if (h.tag == ZZ_DICT && h.dict) {
        for (size_t i = 0; i < h.dict->len; i++) {
            zz_str *k = h.dict->entries[i].key;
            zz_value v = h.dict->entries[i].val;
            if (k && v.tag == ZZ_STR && v.s) {
                xlen += k->len + 2 + v.s->len + 2;
            }
        }
    }
    char hs[256];
    int hlen = snprintf(hs, sizeof hs,
        "HTTP/1.1 %ld %s\r\nContent-Length: %zu\r\nConnection: close\r\n",
        status, reason, body_len);
    if (hlen < 0 || (size_t)hlen >= sizeof hs) {
        zz_release(&t);
        zz_release(&h);
        return NULL;
    }
    size_t total = (size_t)hlen + xlen + 2 + body_len;
    char *out = (char *)malloc(total + 1);
    if (!out) {
        zz_release(&t);
        zz_release(&h);
        return NULL;
    }
    memcpy(out, hs, (size_t)hlen);
    size_t off = (size_t)hlen;
    if (h.tag == ZZ_DICT && h.dict) {
        for (size_t i = 0; i < h.dict->len; i++) {
            zz_str *k = h.dict->entries[i].key;
            zz_value v = h.dict->entries[i].val;
            if (k && v.tag == ZZ_STR && v.s) {
                memcpy(out + off, zz_str_cptr(k), k->len);
                off += k->len;
                memcpy(out + off, ": ", 2);
                off += 2;
                memcpy(out + off, zz_str_cptr(v.s), v.s->len);
                off += v.s->len;
                memcpy(out + off, "\r\n", 2);
                off += 2;
            }
        }
    }
    memcpy(out + off, "\r\n", 2);
    off += 2;
    if (body_len > 0) memcpy(out + off, body, body_len);
    off += body_len;
    out[off] = '\0';
    zz_release(&t);
    zz_release(&h);
    *out_len = total;
    return out;
}

// Plain-text error reply (400/413/431/500 paths). Drops any buffered
// request bytes so peak memory stays bounded, then arms the writer.
static void http_reply(Connection *c, long status, const char *body, size_t body_len) {
    const char *reason = http_reason_phrase(status);
    char hs[256];
    int hlen = snprintf(hs, sizeof hs,
        "HTTP/1.1 %ld %s\r\nContent-Type: text/plain\r\nContent-Length: %zu\r\nConnection: close\r\n\r\n",
        status, reason, body_len);
    if (hlen < 0 || (size_t)hlen >= sizeof hs) {
        free_connection(c);
        return;
    }
    char *out = (char *)malloc((size_t)hlen + body_len + 1);
    if (!out) {
        free_connection(c);
        return;
    }
    memcpy(out, hs, (size_t)hlen);
    if (body_len > 0) memcpy(out + (size_t)hlen, body, body_len);
    out[(size_t)hlen + body_len] = '\0';
    if (c->head_dyn) { free(c->head_dyn); c->head_dyn = NULL; }
    if (c->body_buf) { free(c->body_buf); c->body_buf = NULL; }
    c->head_len = 0;
    c->head_cap = 0;
    c->head_end = 0;
    c->body_len = 0;
    c->body_need = 0;
    c->wbuf = out;
    c->wlen = (size_t)hlen + body_len;
    c->wpos = 0;
    c->keep_alive = 0;
    c->state = CONN_WRITING;
}

// Head complete: validate the request line, size the body, then run or
// wait for it.
static void http_on_head(Connection *c) {
    const char *head = http_head_buf(c);
    const char *m;
    const char *tg;
    size_t ml;
    size_t tl;
    if (!http_parse_request_line(head, c->head_end, &m, &ml, &tg, &tl)) {
        http_reply(c, 400, "bad request", 11);
        return;
    }
    size_t cl = 0;
    int expect = 0;
    http_scan_head(head, c->head_end, &cl, &expect);
    if (cl > HTTP_MAX_BODY_BYTES) {
        http_reply(c, 413, "payload too large", 17);
        return;
    }
    if (expect && cl > 0) {
        // Best-effort interim: a dead client surfaces at body-read time.
        const char *cont = "HTTP/1.1 100 Continue\r\n\r\n";
        size_t off = 0;
        while (off < 25) {
            ssize_t n = write(c->fd, cont + off, 25 - off);
            if (n <= 0) break;
            off += (size_t)n;
        }
    }
    if (cl == 0) {
        http_run_handler(c);
        return;
    }
    char *bb = (char *)malloc(cl);
    if (!bb) {
        http_reply(c, 500, "internal error", 14);
        return;
    }
    size_t total = http_head_len(c);
    size_t tail = (c->head_end < total) ? total - c->head_end : 0;
    if (tail > cl) tail = cl;
    if (tail > 0) memcpy(bb, head + c->head_end, tail);
    c->body_buf = bb;
    c->body_len = tail;
    c->body_need = cl;
    if (tail >= cl) {
        http_run_handler(c);
    } else {
        c->state = CONN_READING_BODY;
    }
}

// Full dispatch for a buffered request: ZZ handler runs inline in the
// worker, the response object serializes to wbuf, always-Connection:
// close (mirroring the VM, which never keeps socket clients alive).
static void http_run_handler(Connection *c) {
    struct timespec t0;
    clock_gettime(CLOCK_MONOTONIC, &t0);
    size_t req_bytes = c->body_need;
    const char *head = http_head_buf(c);
    const char *m;
    const char *tg;
    size_t ml;
    size_t tl;
    if (!http_parse_request_line(head, c->head_end, &m, &ml, &tg, &tl)) {
        http_reply(c, 500, "internal error", 14);
        return;
    }
    char *method = (char *)malloc(ml + 1);
    if (!method) {
        http_reply(c, 500, "internal error", 14);
        return;
    }
    memcpy(method, m, ml);
    method[ml] = '\0';
    zz_value headers = http_build_headers_dict(head, c->head_end);
    zz_str *bs;
    if (c->body_need > 0 && c->body_buf) {
        bs = str_alloc(c->body_need);
        memcpy(zz_str_ptr(bs), c->body_buf, c->body_need);
        zz_str_ptr(bs)[c->body_need] = '\0';
        bs->len = c->body_need;
    } else {
        bs = str_alloc(0);
    }
    zz_value bodyval = {ZZ_STR, {.s = bs}};
    int failed = 0;
    zz_value resp = http_dispatch(method, tg, tl, bodyval, headers, &failed);
    zz_release(&bodyval);
    // The response owns its copies now — drop request bytes before
    // serializing so peak stays at one body, not two.
    if (c->head_dyn) { free(c->head_dyn); c->head_dyn = NULL; }
    if (c->body_buf) { free(c->body_buf); c->body_buf = NULL; }
    c->head_len = 0;
    c->head_cap = 0;
    c->head_end = 0;
    c->body_len = 0;
    c->body_need = 0;
    size_t wlen = 0;
    char *wbuf = http_serialize_response(&resp, &wlen);
    long status = 500;
    zz_value st = zz_object_get_field(&resp, "status");
    if (st.tag == ZZ_INT) status = (long)st.i;
    zz_release(&st);
    zz_release(&resp);
    if (!wbuf) {
        http_reply(c, 500, "internal error", 14);
        return;
    }
    c->wbuf = wbuf;
    c->wlen = wlen;
    c->wpos = 0;
    c->keep_alive = 0;
    c->state = CONN_WRITING;
    if (g_http_log_enabled) {
        struct timespec t1;
        clock_gettime(CLOCK_MONOTONIC, &t1);
        double ms = (double)(t1.tv_sec - t0.tv_sec) * 1000.0
            + (double)(t1.tv_nsec - t0.tv_nsec) / 1000000.0;
        const char *cs = "\x1b[0m";
        if (status >= 200 && status < 300) cs = "\x1b[32m";
        else if (status >= 300 && status < 400) cs = "\x1b[33m";
        else if (status >= 400 && status < 500) cs = "\x1b[31m";
        else if (status >= 500 && status < 600) cs = "\x1b[35m";
        const char *qm = memchr(tg, '?', tl);
        size_t plen = qm ? (size_t)(qm - tg) : tl;
        fprintf(stderr, "%s[%ld] %s %.*s %s\x1b[0m %.2fms\n",
            cs, status, method, (int)plen, tg,
            http_reason_phrase(status), ms);
    }
    free(method);
    if (req_bytes > HTTP_TRIM_AFTER_BYTES) {
        // Same tripwire as the VM: large requests churn tens of MB
        // through this worker's heap; hand the freed pages back so a
        // few big publishes don't ratchet RSS forever.
#ifdef __linux__
        malloc_trim(0);
#endif
    }
}

// zz_http_test(server, method, path, body) → HttpResponse object.
// Mirrors the VM: query split off '?', middleware + handler dispatch,
// every outcome wrapped as a response (even "no route" → 500).
zz_value zz_http_test(zz_value server, zz_value method, zz_value path, zz_value body, int *err) {
    (void)server;
    if (method.tag != ZZ_STR || !method.s || path.tag != ZZ_STR || !path.s
        || body.tag != ZZ_STR || !body.s) {
        *err = 1;
        return zz_variant_err(zz_str_static("std.http.test: expected (server, method, path, body) strings"));
    }
    int failed = 0;
    zz_value out = http_dispatch(zz_str_cptr(method.s), zz_str_cptr(path.s), path.s->len, body, zz_dict_new(), &failed);
    *err = 0;
    return out;
}

// zz_http_handle(server, method, path, body) → .ok(body str) | .err(msg).
// Mirrors the VM: dispatcher failures (no route, middleware violation)
// are .err, while a handler-produced response (any status) is .ok(text).
zz_value zz_http_handle(zz_value server, zz_value method, zz_value path, zz_value body, int *err) {
    (void)server;
    if (method.tag != ZZ_STR || !method.s || path.tag != ZZ_STR || !path.s
        || body.tag != ZZ_STR || !body.s) {
        *err = 1;
        return zz_variant_err(zz_str_static("std.http.handle: expected (server, method, path, body) strings"));
    }
    int failed = 0;
    zz_value resp = http_dispatch(zz_str_cptr(method.s), zz_str_cptr(path.s), path.s->len, body, zz_dict_new(), &failed);
    *err = 0;
    zz_value text = zz_object_get_field(&resp, "text");
    zz_value out;
    if (failed) {
        out = zz_variant_err(text.tag == ZZ_STR ? zz_clone(text) : zz_str_static("request failed"));
    } else {
        out = zz_variant_ok(text.tag == ZZ_STR ? zz_clone(text) : zz_str_static(""));
    }
    zz_release(&resp);
    return out;
}


// zz_http_pipe(server, middleware, err) — appends to the middleware chain
// (run in order by the test dispatcher; epoll workers in Step 2).
zz_value zz_http_pipe(zz_value server, zz_value middleware, int *err) {
    *err = 0;
    if (g_http_middleware_count < HTTP_MAX_MIDDLEWARE - 1) {
        g_http_middlewares[g_http_middleware_count++] = zz_clone(middleware);
    }
    return server;
}

// zz_http_log(server, enabled, err) — arms the per-request access log
// (same colored format as the VM) and returns the server for chaining.
zz_value zz_http_log(zz_value server, zz_value enabled, int *err) {
    *err = 0;
    g_http_log_enabled = zz_truthy(enabled) ? 1 : 0;
    return server;
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

    // Print SERVER_READY to stderr so benchmark runners know the port
    // is open (matches the VM, and keeps program stdout clean).
    fprintf(stderr, "SERVER_READY\n");
    fflush(stderr);

    g_http_server_running = 1;

    // Spawn workers and wait
    spawn_workers(listen_fd, p);

    g_http_server_running = 0;
    close(listen_fd);
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
// 4-arg natives (e.g. http.test/server+method+path+body). Same err
// discipline as the other shims: the callee signals arg violations
// through *err and the generated code checks it after the call.
zz_value zz_call_native4(zz_value (*f)(zz_value, zz_value, zz_value, zz_value, int *), zz_value a, zz_value b, zz_value c, zz_value d) {
    int err = 0;
    zz_value r = f(a, b, c, d, &err);
    return r;
}
zz_value zz_call_native5(zz_value (*f)(zz_value, zz_value, zz_value, zz_value, zz_value, int *), zz_value a, zz_value b, zz_value c, zz_value d, zz_value e) {
    int err = 0;
    zz_value r = f(a, b, c, d, e, &err);
    return r;
}
// Spawn-closure-literal fuse (`task.spawn(|...| ...)`): the lowerer passes
// the capture arrays straight through instead of building an intermediate
// closure value first. Same err discipline as the other shims (ignored:
// OOM degrades to a unit handle, exactly like a failed `make` would).
zz_value zz_call_native_spawn(zz_dispatch_fn fn, void **cells,
                              const unsigned char *kinds, const size_t *sizes,
                              size_t nenv, int is_green) {
    int err = 0;
    return zz_spawn_ex(fn, cells, kinds, sizes, nenv, is_green, &err);
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
        case ZZ_FILE: name = "file"; break;
        case ZZ_BYTES: name = "bytes"; break;
        case ZZ_VFS: name = "fs"; break;
        case ZZ_TUPLE: name = "tuple"; break;
        default: name = "unknown"; break;
    }
    return zz_str_static(name);
}

// int(v) — parse to Option[int] (mirrors the VM `conv_int`).
// INT/FLOAT wrap in `.some`; STR trims ASCII whitespace then requires an
// optional sign + all-digits (Rust `parse::<i64>` semantics); everything
// else (BOOL, arrays, NONE, ...) yields `.none`. Strict full-consumption:
// "12abc"/"3.9"/"" all fail, matching `s.trim().parse::<i64>().ok()`.
zz_value zz_int_cast(zz_value v, int *err) {
    (void)err;
    switch (v.tag) {
        case ZZ_INT:
            return zz_variant_some(v);
        case ZZ_FLOAT: {
            double f = v.f;
            int64_t n;
            if (f != f) n = 0; // NaN saturates to 0 (Rust `as` semantics)
            else if (f >= (double)INT64_MAX) n = INT64_MAX;
            else if (f <= (double)INT64_MIN) n = INT64_MIN;
            else n = (int64_t)f;
            return zz_variant_some((zz_value){ZZ_INT, {.i = n}});
        }
        case ZZ_STR: {
            if (!v.s) return (zz_value){ZZ_OPTION_NONE, {.payload = NULL}};
            const char *d = zz_str_cptr(v.s);
            size_t len = v.s->len;
            size_t start = 0, end = len;
            while (start < end) {
                char c = d[start];
                if (c == ' ' || c == '\t' || c == '\n' || c == '\r' || c == '\v' || c == '\f') start++;
                else break;
            }
            while (end > start) {
                char c = d[end - 1];
                if (c == ' ' || c == '\t' || c == '\n' || c == '\r' || c == '\v' || c == '\f') end--;
                else break;
            }
            if (start >= end) return (zz_value){ZZ_OPTION_NONE, {.payload = NULL}};
            int neg = 0;
            size_t pos = start;
            if (d[pos] == '+' || d[pos] == '-') {
                neg = (d[pos] == '-');
                pos++;
                if (pos >= end) return (zz_value){ZZ_OPTION_NONE, {.payload = NULL}};
            }
            uint64_t limit = neg ? ((uint64_t)INT64_MAX) + 1u : (uint64_t)INT64_MAX;
            uint64_t acc = 0;
            for (; pos < end; pos++) {
                char c = d[pos];
                if (c < '0' || c > '9') return (zz_value){ZZ_OPTION_NONE, {.payload = NULL}};
                unsigned digit = (unsigned)(c - '0');
                if (acc > (limit - digit) / 10u) return (zz_value){ZZ_OPTION_NONE, {.payload = NULL}};
                acc = acc * 10u + digit;
            }
            int64_t n = neg ? (acc == limit ? INT64_MIN : -(int64_t)acc) : (int64_t)acc;
            return zz_variant_some((zz_value){ZZ_INT, {.i = n}});
        }
        default: return (zz_value){ZZ_OPTION_NONE, {.payload = NULL}};
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
    // Copy: the process environment may move under set/unset.
    return zz_variant_some(zz_str_new(val, strlen(val)));
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

// ---- cross-platform environment + OS identity (mirrors natives/env) --------
//
// POSIX on Unix-likes (`setenv`/`unsetenv`, `getcwd`/`chdir`,
// `readlink /proc/self/exe`, `_NSGetExecutablePath` on macOS, `environ`)
// and CRT/Win32 on Windows (`_putenv`, `GetCurrentDirectoryA`,
// `SetCurrentDirectoryA`, `GetModuleFileNameA`, `GetTempPathA`,
// `GetEnvironmentStringsA`). All returned strings are copies owned by
// the runtime (never aliases into `environ`/CRT buffers).

static int zz_env_key_valid(const char *k) {
    if (!k || !*k) return 0;
    for (const char *p = k; *p; p++) {
        if (*p == '=') return 0;
    }
    return 1;
}

// env.set(key, val) → Result<unit>
zz_value zz_env_set(zz_value name, zz_value val, int *err) {
    (void)err;
    if (name.tag != ZZ_STR || val.tag != ZZ_STR) { *err = 1; return zz_unit(); }
    const char *k = zz_str_cptr(name.s);
    const char *v = zz_str_cptr(val.s);
    if (!zz_env_key_valid(k)) {
        size_t n = strlen("invalid environment variable name ``") + name.s->len + 1;
        char *msg = (char *)malloc(n);
        if (!msg) return zz_variant_err(zz_str_static("env.set: out of memory"));
        snprintf(msg, n, "invalid environment variable name `%s`", k);
        return zz_variant_err(zz_str_owned(msg));
    }
#ifdef ZZ_OS_WINDOWS
    size_t n = name.s->len + 1 + val.s->len + 1;
    char *kv = (char *)malloc(n);
    if (!kv) return zz_variant_err(zz_str_static("env.set: out of memory"));
    memcpy(kv, k, name.s->len);
    kv[name.s->len] = '=';
    memcpy(kv + name.s->len + 1, v, val.s->len + 1);
    int rc = _putenv(kv);
    free(kv);
    if (rc != 0) return zz_variant_err(zz_str_static("env.set: failed"));
#else
    // setenv copies both strings; lengths come from NUL-terminated
    // getters over length-tagged values — pass explicit slices via a
    // temporary NUL-terminated copy to honor embedded lengths exactly.
    char *kc = copy_cstr(k, name.s->len);
    char *vc = copy_cstr(v, val.s->len);
    int rc = -1;
    if (kc && vc) rc = setenv(kc, vc, 1);
    free(kc);
    free(vc);
    if (rc != 0) return zz_variant_err(zz_str_static("env.set: failed"));
#endif
    return zz_variant_ok(zz_unit());
}

// env.remove(key) / env.unset(key) — total, missing keys are no-ops.
zz_value zz_env_remove(zz_value name, int *err) {
    (void)err;
    if (name.tag != ZZ_STR) { *err = 1; return zz_unit(); }
    const char *k = zz_str_cptr(name.s);
#ifdef ZZ_OS_WINDOWS
    size_t n = name.s->len + 2;
    char *kv = (char *)malloc(n);
    if (kv) {
        memcpy(kv, k, name.s->len);
        kv[name.s->len] = '=';
        kv[name.s->len + 1] = '\0';
        _putenv(kv); // `NAME=` deletes the variable; ignore errors
        free(kv);
    }
#else
    char *kc = copy_cstr(k, name.s->len);
    if (kc) {
        unsetenv(kc); // no-op when absent; no failure mode worth surfacing
        free(kc);
    }
#endif
    return zz_unit();
}

// env.vars() → Dict<str, str> (sorted by key, like the VM).
zz_value zz_env_vars(zz_value unused, int *err) {
    (void)unused; (void)err;
    zz_value out = zz_dict_new();
#ifdef ZZ_OS_WINDOWS
    char *block = GetEnvironmentStringsA();
    if (block) {
        for (char *p = block; *p; p += strlen(p) + 1) {
            // Skip the per-drive hidden entries (`=C:=C:\...`).
            if (*p == '=') continue;
            char *eq = strchr(p, '=');
            if (!eq) continue;
            zz_value k = zz_str_new(p, (size_t)(eq - p));
            zz_value v = zz_str_new(eq + 1, strlen(eq + 1));
            zz_dict_set(out.dict, k, v);
            // dict_set retains the key but takes ownership of the value.
            zz_release(&k);
        }
        FreeEnvironmentStringsA(block);
    }
#else
    extern char **environ;
    for (char **e = environ; e && *e; e++) {
        char *eq = strchr(*e, '=');
        if (!eq) continue;
        zz_value k = zz_str_new(*e, (size_t)(eq - *e));
        zz_value v = zz_str_new(eq + 1, strlen(eq + 1));
        zz_dict_set(out.dict, k, v);
        // dict_set retains the key but takes ownership of the value.
        zz_release(&k);
    }
#endif
    // Insertion order differs by platform (environ order vs block order);
    // the VM sorts by key, so sort here too for byte-exact parity. Dicts
    // preserve insertion order — rebuild sorted by MOVING entry structs
    // (ownership transfers; no refcount traffic, no double frees).
    size_t n = out.dict ? out.dict->len : 0;
    if (n > 1) {
        size_t *idx = (size_t *)malloc(n * sizeof(size_t));
        if (idx) {
            for (size_t i = 0; i < n; i++) idx[i] = i;
            // Insertion sort on key bytes (env blocks are small).
            for (size_t i = 1; i < n; i++) {
                size_t j = i;
                while (j > 0) {
                    zz_str *a = out.dict->entries[idx[j - 1]].key;
                    zz_str *b = out.dict->entries[idx[j]].key;
                    size_t m = a->len < b->len ? a->len : b->len;
                    int c = memcmp(zz_str_cptr(a), zz_str_cptr(b), m);
                    if (c == 0) c = (a->len < b->len) ? -1 : (a->len > b->len);
                    if (c <= 0) break;
                    size_t t = idx[j - 1];
                    idx[j - 1] = idx[j];
                    idx[j] = t;
                    j--;
                }
            }
            zz_dict_entry *nen =
                (zz_dict_entry *)malloc(n * sizeof(zz_dict_entry));
            if (nen) {
                for (size_t i = 0; i < n; i++) nen[i] = out.dict->entries[idx[i]];
                free(out.dict->entries);
                out.dict->entries = nen;
                out.dict->cap = n;
            }
            free(idx);
        }
    }
    return out;
}

// env.cwd() → Result<str>
zz_value zz_env_cwd(zz_value unused, int *err) {
    (void)unused; (void)err;
#ifdef ZZ_OS_WINDOWS
    char buf[4096];
    DWORD n = GetCurrentDirectoryA((DWORD)sizeof buf, buf);
    if (n == 0 || n >= sizeof buf) {
        return zz_variant_err(zz_str_static("cannot read working directory"));
    }
    return zz_variant_ok(zz_str_new(buf, strlen(buf)));
#else
    char *p = getcwd(NULL, 0);
    if (!p) return zz_variant_err(zz_str_static("cannot read working directory"));
    zz_value v = zz_variant_ok(zz_str_new(p, strlen(p)));
    free(p);
    return v;
#endif
}

// env.set_cwd(path) → Result<unit>
zz_value zz_env_set_cwd(zz_value path, int *err) {
    (void)err;
    if (path.tag != ZZ_STR || !path.s) { *err = 1; return zz_unit(); }
    const char *p = zz_str_cptr(path.s);
#ifdef ZZ_OS_WINDOWS
    if (!SetCurrentDirectoryA(p)) {
        size_t n = strlen("fs:set_cwd:io_error: ") + strlen(p) + 1;
        char *msg = (char *)malloc(n);
        if (!msg) return zz_variant_err(zz_str_static("fs:set_cwd:io_error"));
        snprintf(msg, n, "fs:set_cwd:io_error: %s", p);
        return zz_variant_err(zz_str_owned(msg));
    }
#else
    if (chdir(p) != 0) {
        size_t n = strlen("fs:set_cwd:io_error: ") + strlen(p)
            + 2 + strlen(strerror(errno)) + 1;
        char *msg = (char *)malloc(n);
        if (!msg) return zz_variant_err(zz_str_static("fs:set_cwd:io_error"));
        snprintf(msg, n, "fs:set_cwd:io_error: %s (%s)", p, strerror(errno));
        return zz_variant_err(zz_str_owned(msg));
    }
#endif
    return zz_variant_ok(zz_unit());
}

// env.exe_path() → Result<str>
zz_value zz_env_exe_path(zz_value unused, int *err) {
    (void)unused; (void)err;
#ifdef ZZ_OS_WINDOWS
    char buf[32768];
    DWORD n = GetModuleFileNameA(NULL, buf, (DWORD)sizeof buf);
    if (n == 0 || n >= sizeof buf) {
        return zz_variant_err(zz_str_static("cannot read executable path"));
    }
    return zz_variant_ok(zz_str_new(buf, n));
#elif defined(__APPLE__)
    uint32_t size = 0;
    _NSGetExecutablePath(NULL, &size);
    char *buf = (char *)malloc(size > 0 ? size : 4096);
    if (!buf) return zz_variant_err(zz_str_static("cannot read executable path"));
    if (_NSGetExecutablePath(buf, &size) != 0) {
        free(buf);
        return zz_variant_err(zz_str_static("cannot read executable path"));
    }
    zz_value v = zz_variant_ok(zz_str_new(buf, strlen(buf)));
    free(buf);
    return v;
#else
    // Linux (and other /proc hosts): readlink sizes the exact buffer.
    char buf[4096];
    ssize_t n = readlink("/proc/self/exe", buf, sizeof buf - 1);
    if (n < 0) return zz_variant_err(zz_str_static("cannot read executable path"));
    buf[n] = '\0';
    return zz_variant_ok(zz_str_new(buf, (size_t)n));
#endif
}

// env.home_dir() → Option<str>
zz_value zz_env_home_dir(zz_value unused, int *err) {
    (void)unused; (void)err;
#ifdef ZZ_OS_WINDOWS
    const char *v = getenv("USERPROFILE");
#else
    const char *v = getenv("HOME");
#endif
    if (!v || !*v) return (zz_value){ZZ_OPTION_NONE, {0}};
    return zz_variant_some(zz_str_new(v, strlen(v)));
}

// env.temp_dir() → str (total: always has a fallback).
zz_value zz_env_temp_dir(zz_value unused, int *err) {
    (void)unused; (void)err;
#ifdef ZZ_OS_WINDOWS
    char buf[32768];
    DWORD n = GetTempPathA((DWORD)sizeof buf, buf);
    if (n == 0 || n >= sizeof buf) return zz_str_static("C:\\Windows\\Temp");
    // GetTempPathA ends with a backslash; strip it for a stable shape.
    while (n > 1 && (buf[n - 1] == '\\' || buf[n - 1] == '/')) buf[--n] = '\0';
    return zz_str_new(buf, n);
#else
    const char *v = getenv("TMPDIR");
    if (v && *v) return zz_str_new(v, strlen(v));
    return zz_str_static("/tmp");
#endif
}

// env.user() → Option<str>
zz_value zz_env_user(zz_value unused, int *err) {
    (void)unused; (void)err;
#ifdef ZZ_OS_WINDOWS
    const char *v = getenv("USERNAME");
    if (!v || !*v) return (zz_value){ZZ_OPTION_NONE, {0}};
    return zz_variant_some(zz_str_new(v, strlen(v)));
#else
    const char *v = getenv("USER");
    if (!v || !*v) v = getenv("LOGNAME");
    if (!v || !*v) return (zz_value){ZZ_OPTION_NONE, {0}};
    return zz_variant_some(zz_str_new(v, strlen(v)));
#endif
}

// env.os() → str (target platform, matching `sys.os()` values).
zz_value zz_env_os(zz_value unused, int *err) {
    (void)unused; (void)err;
#ifdef ZZ_OS_WINDOWS
    return zz_str_static("windows");
#elif defined(__APPLE__)
    return zz_str_static("macos");
#elif defined(__linux__)
    return zz_str_static("linux");
#else
    return zz_str_static("unknown");
#endif
}

// env.arch() → str (target CPU, matching `sys.arch()` values).
zz_value zz_env_arch(zz_value unused, int *err) {
    (void)unused; (void)err;
#if defined(__x86_64__) || defined(_M_X64)
    return zz_str_static("x86_64");
#elif defined(__i386__) || defined(_M_IX86)
    return zz_str_static("x86");
#elif defined(__aarch64__) || defined(_M_ARM64)
    return zz_str_static("aarch64");
#else
    return zz_str_static("unknown");
#endif
}

// Render a printed `.err(payload)` as a readable, hinted diagnostic and
// abort (exit 1). Structured `fs:<op>:<code>: <detail>` payloads become
// `cannot <verb> '<detail>': <reason>` + `hint: ...`; anything else
// surfaces verbatim with a generic recovery hint. Colors apply when
// stderr is a terminal, matching the CLI diagnostic renderer.
// (Defined here — after the socket include block — for isatty/fileno.)
static void zz_throw_printed_err(const zz_value *payload) {
    char text[1024];
    if (payload->tag == ZZ_STR && payload->s) {
        size_t n = payload->s->len < sizeof(text) - 1 ? payload->s->len : sizeof(text) - 1;
        memcpy(text, zz_str_cptr(payload->s), n);
        text[n] = '\0';
    } else {
        snprintf(text, sizeof text, "<non-string error>");
    }
    // Split `fs:<op>:<code>: <detail>`.
    const char *verb = NULL;
    const char *reason = NULL;
    const char *hint = NULL;
    char detail[768] = "";
    if (strncmp(text, "fs:", 3) == 0) {
        char *op_end = strchr(text + 3, ':');
        if (op_end) {
            char *code_end = strchr(op_end + 1, ':');
            if (code_end) {
                *op_end = '\0';
                *code_end = '\0';
                const char *op = text + 3;
                const char *code = op_end + 1;
                snprintf(detail, sizeof detail, "%s", code_end + 1);
                // Trim one leading space from "<code>: <detail>".
                if (detail[0] == ' ') memmove(detail, detail + 1, strlen(detail));
                if (strcmp(op, "read") == 0 || strcmp(op, "read_bytes") == 0) verb = "read";
                else if (strcmp(op, "write") == 0) verb = "write to";
                else if (strcmp(op, "append") == 0) verb = "append to";
                else if (strcmp(op, "copy") == 0) verb = "copy";
                else if (strcmp(op, "move") == 0 || strcmp(op, "rename") == 0) verb = "move";
                else if (strcmp(op, "remove") == 0 || strcmp(op, "remove_file") == 0) verb = "remove";
                else if (strcmp(op, "mkdir") == 0 || strcmp(op, "mkdir_all") == 0) verb = "create directory";
                else if (strcmp(op, "read_dir") == 0 || strcmp(op, "readdir") == 0) verb = "list directory";
                else if (strcmp(op, "remove_dir_all") == 0) verb = "remove directory";
                else if (strcmp(op, "walk_dir") == 0) verb = "walk directory";
                else if (strcmp(op, "stat") == 0) verb = "stat";
                else if (strcmp(op, "open") == 0) verb = "open";
                else if (strcmp(op, "read_chunk") == 0) verb = "read from file";
                else if (strcmp(op, "write_chunk") == 0) verb = "write to file";
                else if (strcmp(op, "seek") == 0) verb = "seek in file";
                else if (strcmp(op, "flush") == 0) verb = "flush file";
                else if (strcmp(op, "close") == 0) verb = "close file";
                if (strcmp(code, "not_found") == 0) {
                    reason = "no such file or directory";
                    hint = "check that the path is correct — relative paths resolve from the current working directory";
                } else if (strcmp(code, "permission_denied") == 0) {
                    reason = "permission denied";
                    hint = "check read/write permissions for the current user";
                } else if (strcmp(code, "already_exists") == 0) {
                    reason = "file already exists";
                    hint = "remove the existing file first, or pick another path";
                } else if (strcmp(code, "invalid_input") == 0) {
                    reason = "invalid argument";
                    hint = "check the arguments (e.g. File.open mode must be one of r, w, a)";
                } else if (strcmp(code, "not_empty") == 0) {
                    reason = "directory is not empty";
                    hint = "remove the contents first, or use fs.remove_dir_all";
                } else if (strcmp(code, "closed") == 0) {
                    reason = "file handle is closed";
                    hint = "the handle was already closed — open it again with File.open";
                } else {
                    reason = "input/output error";
                    hint = "check disk health and available space";
                }
            }
        }
    }
    int tty;
#ifdef ZZ_OS_WINDOWS
    tty = _isatty(_fileno(stderr));
#else
    tty = isatty(fileno(stderr));
#endif
    if (verb && reason) {
        // Trim leading whitespace from detail (payloads use ": <detail>").
        const char *d = detail;
        while (*d == ' ' || *d == '\t') d++;
        if (tty) {
            fprintf(stderr, "\033[1;31merror\033[0m: cannot %s '%s': %s\n", verb, d, reason);
            fprintf(stderr, "    \033[1;36m=\033[0m \033[36mhint: %s\033[0m\n", hint);
        } else {
            fprintf(stderr, "error: cannot %s '%s': %s\n", verb, d, reason);
            fprintf(stderr, "    = hint: %s\n", hint);
        }
    } else {
        if (tty) {
            fprintf(stderr, "\033[1;31merror\033[0m: error value printed: %s\n", text);
            fprintf(stderr, "    \033[1;36m=\033[0m \033[36mhint: handle .err explicitly with match to recover instead of aborting\033[0m\n");
        } else {
            fprintf(stderr, "error: error value printed: %s\n", text);
            fprintf(stderr, "    = hint: handle .err explicitly with match to recover instead of aborting\n");
        }
    }
    fprintf(stderr, "zz: program failed\n");
    fflush(stderr);
    exit(1);
}
// =====================================================================
//  std.fs — comprehensive non-blocking filesystem
//
//  Every fallible op returns `Result<_, str>` with a unified diagnostic
//  `fs:<op>:<code>: <path>` (never raw `strerror` text), mirroring the VM
//  (`zz_stdlib/src/natives/fs/mod.rs`) byte-for-byte. `<code>` is one of
//  `not_found`, `permission_denied`, `already_exists`, `invalid_input`,
//  `not_empty`, `closed`, or `io_error`.
//
//  Non-blocking discipline: each blocking syscall first tops up the
//  executor when running on a worker thread (`zz_is_worker`), so the
//  scheduler never stalls waiting for disk. The public API stays
//  synchronous-looking — `fs.read_to_string(path)` — exactly like the VM.
// =====================================================================
#ifndef ZZ_OS_WINDOWS
#include <dirent.h>
#endif

// errno → stable code (mirrors the VM's ErrorKind mapping).
static const char *zz_fs_code(int e) {
    switch (e) {
    case ENOENT:
#ifdef ENODATA
    case ENODATA:
#endif
        return "not_found";
    case EACCES:
    case EPERM:
        return "permission_denied";
    case EEXIST:
        return "already_exists";
    case EINVAL:
    case EISDIR:
    case ENOTDIR:
        return "invalid_input";
#ifdef ENOTEMPTY
    case ENOTEMPTY:
#endif
        return "not_empty";
    default:
        return "io_error";
    }
}

// `fs:<op>:<code>: <path>` as an `.err(str)`.
static zz_value zz_fs_err1(const char *op, const char *path, int e) {
    const char *code = zz_fs_code(e);
    size_t n = strlen("fs:") + strlen(op) + 1 + strlen(code) + 2 + strlen(path) + 1;
    char *msg = (char *)malloc(n);
    if (!msg) return zz_variant_err(zz_str_static("fs:io_error"));
    snprintf(msg, n, "fs:%s:%s: %s", op, code, path);
    return zz_variant_err(zz_str_owned(msg));
}

// `fs:<op>:<code>: <src> -> <dst>` as an `.err(str)`.
static zz_value zz_fs_err2(const char *op, const char *src, const char *dst, int e) {
    const char *code = zz_fs_code(e);
    size_t n = strlen("fs:") + strlen(op) + 1 + strlen(code) + 2
        + strlen(src) + 4 + strlen(dst) + 1;
    char *msg = (char *)malloc(n);
    if (!msg) return zz_variant_err(zz_str_static("fs:io_error"));
    snprintf(msg, n, "fs:%s:%s: %s -> %s", op, code, src, dst);
    return zz_variant_err(zz_str_owned(msg));
}

// Scheduler courtesy: blocking disk I/O must never stall the executor.
// When this runs on a worker thread, park a replacement first (same
// discipline as channel/task blocking waits).
static inline void zz_fs_top_up(void) {
    if (zz_is_worker) zz_executor_top_up();
}

static const char *zz_fs_cstr(zz_value v) {
    if (v.tag != ZZ_STR || !v.s) return NULL;
    return zz_str_cptr(v.s);
}

// fs.read_to_string(path) → Result<str>
zz_value zz_fs_read(zz_value path, int *err) {
    (void)err;
    const char *p = zz_fs_cstr(path);
    if (!p) { *err = 1; return zz_unit(); }
    zz_fs_top_up();
    FILE *f = fopen(p, "rb");
    if (!f) return zz_fs_err1("read", p, errno);
    fseek(f, 0, SEEK_END);
    long sz = ftell(f);
    fseek(f, 0, SEEK_SET);
    if (sz < 0) sz = 0;
    zz_str *out = str_alloc((size_t)sz);
    size_t n = fread(zz_str_ptr(out), 1, (size_t)sz, f);
    int ferr = ferror(f);
    fclose(f);
    if (ferr) {
        zz_value leak = (zz_value){ZZ_STR, {.s = out}};
        zz_release(&leak);
        return zz_fs_err1("read", p, EIO);
    }
    zz_str_ptr(out)[n] = '\0';
    out->len = n;
    return zz_variant_ok((zz_value){ZZ_STR, {.s = out}});
}

// fs.read_bytes(path) → Result<bytes> (contiguous buffer, ~1x RSS).
zz_value zz_fs_read_bytes(zz_value path, int *err) {
    (void)err;
    const char *p = zz_fs_cstr(path);
    if (!p) { *err = 1; return zz_unit(); }
    zz_fs_top_up();
    FILE *f = fopen(p, "rb");
    if (!f) return zz_fs_err1("read_bytes", p, errno);
    fseek(f, 0, SEEK_END);
    long sz = ftell(f);
    fseek(f, 0, SEEK_SET);
    if (sz < 0) sz = 0;
    if (sz > 0) {
        // Regular files: read straight into the bytes store — one
        // allocation, zero copies (~1x RSS).
        zz_bytes_buf *store = zz_bytes_buf_new((size_t)sz);
        if (!store) {
            fclose(f);
            return zz_fs_err1("read_bytes", p, ENOMEM);
        }
        size_t n = fread(store->data, 1, (size_t)sz, f);
        int ferr = ferror(f);
        fclose(f);
        if (ferr) {
            if (__atomic_sub_fetch(&store->refs, 1, __ATOMIC_ACQ_REL) == 0) free(store);
            return zz_fs_err1("read_bytes", p, EIO);
        }
        // Short reads (races/truncation) shrink the window, not the store.
        return zz_variant_ok(zz_bytes_wrap(store, 0, n));
    } else {
        // Pipes / procfs / sysfs report size 0: grow to EOF (same shape
        // as the old chunked reader, then one shrink).
        size_t cap = 65536;
        zz_bytes_buf *store = zz_bytes_buf_new(cap);
        if (!store) {
            fclose(f);
            return zz_fs_err1("read_bytes", p, ENOMEM);
        }
        size_t n = 0;
        size_t got;
        int ferr = 0;
        while ((got = fread(store->data + n, 1, cap - n, f)) > 0) {
            n += got;
            if (n == cap) {
                size_t ncap = cap * 2;
                zz_bytes_buf *ns =
                    (zz_bytes_buf *)realloc(store, sizeof(zz_bytes_buf) + ncap);
                if (!ns) {
                    if (__atomic_sub_fetch(&store->refs, 1, __ATOMIC_ACQ_REL) == 0) {
                        free(store);
                    }
                    fclose(f);
                    return zz_fs_err1("read_bytes", p, ENOMEM);
                }
                store = ns;
                store->len = ncap;
                cap = ncap;
            }
        }
        ferr = ferror(f);
        fclose(f);
        if (ferr) {
            if (__atomic_sub_fetch(&store->refs, 1, __ATOMIC_ACQ_REL) == 0) free(store);
            return zz_fs_err1("read_bytes", p, EIO);
        }
        return zz_variant_ok(zz_bytes_wrap(store, 0, n));
    }
}

// fs.write(path, data) → Result<unit>
zz_value zz_fs_write(zz_value path, zz_value data, int *err) {
    (void)err;
    const char *p = zz_fs_cstr(path);
    const char *d = zz_fs_cstr(data);
    if (!p || data.tag != ZZ_STR) { *err = 1; return zz_unit(); }
    zz_fs_top_up();
    FILE *f = fopen(p, "wb");
    if (!f) return zz_fs_err1("write", p, errno);
    size_t w = fwrite(d, 1, data.s->len, f);
    int close_ok = (fclose(f) == 0);
    if (w != data.s->len || !close_ok) return zz_fs_err1("write", p, EIO);
    return zz_variant_ok(zz_unit());
}

// fs.append(path, data) → Result<unit>
zz_value zz_fs_append(zz_value path, zz_value data, int *err) {
    (void)err;
    const char *p = zz_fs_cstr(path);
    const char *d = zz_fs_cstr(data);
    if (!p || data.tag != ZZ_STR) { *err = 1; return zz_unit(); }
    zz_fs_top_up();
    FILE *f = fopen(p, "ab");
    if (!f) return zz_fs_err1("append", p, errno);
    size_t w = fwrite(d, 1, data.s->len, f);
    int close_ok = (fclose(f) == 0);
    if (w != data.s->len || !close_ok) return zz_fs_err1("append", p, EIO);
    return zz_variant_ok(zz_unit());
}

// fs.copy(src, dst) → Result<unit>
zz_value zz_fs_copy(zz_value src, zz_value dst, int *err) {
    (void)err;
    const char *s = zz_fs_cstr(src);
    const char *d = zz_fs_cstr(dst);
    if (!s || !d) { *err = 1; return zz_unit(); }
    zz_fs_top_up();
    FILE *in = fopen(s, "rb");
    if (!in) return zz_fs_err2("copy", s, d, errno);
    FILE *out = fopen(d, "wb");
    if (!out) {
        int e = errno;
        fclose(in);
        return zz_fs_err2("copy", s, d, e);
    }
    char buf[65536];
    size_t n;
    int rwerr = 0;
    while ((n = fread(buf, 1, sizeof buf, in)) > 0) {
        if (fwrite(buf, 1, n, out) != n) { rwerr = EIO; break; }
    }
    if (!rwerr && ferror(in)) rwerr = EIO;
    fclose(in);
    if (fclose(out) != 0 && !rwerr) rwerr = EIO;
    if (rwerr) return zz_fs_err2("copy", s, d, rwerr);
    return zz_variant_ok(zz_unit());
}

// fs.move(src, dst) → Result<unit>
zz_value zz_fs_move(zz_value src, zz_value dst, int *err) {
    (void)err;
    const char *s = zz_fs_cstr(src);
    const char *d = zz_fs_cstr(dst);
    if (!s || !d) { *err = 1; return zz_unit(); }
    zz_fs_top_up();
    if (rename(s, d) != 0) return zz_fs_err2("move", s, d, errno);
    return zz_variant_ok(zz_unit());
}

// fs.exists(path) → bool (stat-based: true for files AND dirs).
zz_value zz_fs_exists(zz_value path, int *err) {
    (void)err;
    const char *p = zz_fs_cstr(path);
    if (!p) return (zz_value){ZZ_BOOL, {.b = false}};
    struct stat st;
    return (zz_value){ZZ_BOOL, {.b = stat(p, &st) == 0}};
}

// fs.is_file(path) → bool
zz_value zz_fs_is_file(zz_value path, int *err) {
    (void)err;
    const char *p = zz_fs_cstr(path);
    if (!p) return (zz_value){ZZ_BOOL, {.b = false}};
    struct stat st;
    if (stat(p, &st) != 0) return (zz_value){ZZ_BOOL, {.b = false}};
#ifdef ZZ_OS_WINDOWS
    return (zz_value){ZZ_BOOL, {.b = (st.st_mode & _S_IFMT) == _S_IFREG}};
#else
    return (zz_value){ZZ_BOOL, {.b = S_ISREG(st.st_mode)}};
#endif
}

// fs.is_dir(path) → bool
zz_value zz_fs_is_dir(zz_value path, int *err) {
    (void)err;
    const char *p = zz_fs_cstr(path);
    if (!p) return (zz_value){ZZ_BOOL, {.b = false}};
    struct stat st;
    if (stat(p, &st) != 0) return (zz_value){ZZ_BOOL, {.b = false}};
#ifdef ZZ_OS_WINDOWS
    return (zz_value){ZZ_BOOL, {.b = (st.st_mode & _S_IFMT) == _S_IFDIR}};
#else
    return (zz_value){ZZ_BOOL, {.b = S_ISDIR(st.st_mode)}};
#endif
}

// fs.remove(path) → Result<unit>
zz_value zz_fs_remove(zz_value path, int *err) {
    (void)err;
    const char *p = zz_fs_cstr(path);
    if (!p) { *err = 1; return zz_unit(); }
    zz_fs_top_up();
    if (remove(p) != 0) return zz_fs_err1("remove_file", p, errno);
    return zz_variant_ok(zz_unit());
}

// fs.mkdir(path) → Result<unit>
zz_value zz_fs_mkdir(zz_value path, int *err) {
    (void)err;
    const char *p = zz_fs_cstr(path);
    if (!p) { *err = 1; return zz_unit(); }
    zz_fs_top_up();
#ifdef ZZ_OS_WINDOWS
    int r = _mkdir(p);
#else
    int r = mkdir(p, 0755);
#endif
    if (r != 0) return zz_fs_err1("mkdir", p, errno);
    return zz_variant_ok(zz_unit());
}

// fs.mkdir_all(path) → Result<unit> (recursive, EEXIST-tolerant).
zz_value zz_fs_mkdir_all(zz_value path, int *err) {
    (void)err;
    const char *p = zz_fs_cstr(path);
    if (!p) { *err = 1; return zz_unit(); }
    zz_fs_top_up();
    // Walk components, creating each level. Absolute + relative paths.
    size_t len = strlen(p);
    char *tmp = (char *)malloc(len + 1);
    if (!tmp) return zz_fs_err1("mkdir_all", p, ENOMEM);
    memcpy(tmp, p, len + 1);
    for (size_t i = 0; i <= len; i++) {
        if (tmp[i] == '/' || tmp[i] == '\\' || tmp[i] == '\0') {
            char save = tmp[i];
            // Skip leading separators and `.` (never mkdir "" or ".").
            int is_edge = (i == 0);
            tmp[i] = '\0';
            if (!is_edge && strlen(tmp) > 0 && strcmp(tmp, ".") != 0) {
#ifdef ZZ_OS_WINDOWS
                if (_mkdir(tmp) != 0 && errno != EEXIST) {
#else
                if (mkdir(tmp, 0755) != 0 && errno != EEXIST) {
#endif
                    int e = errno;
                    // A non-directory at this prefix is a hard error even
                    // when some other errno claims success.
                    struct stat st;
                    if (stat(tmp, &st) != 0
#ifdef ZZ_OS_WINDOWS
                        || (st.st_mode & _S_IFMT) != _S_IFDIR) {
#else
                        || !S_ISDIR(st.st_mode)) {
#endif
                        tmp[i] = save;
                        zz_value r = zz_fs_err1("mkdir_all", p, e);
                        free(tmp);
                        return r;
                    }
                }
            }
            tmp[i] = save;
        }
    }
    free(tmp);
    return zz_variant_ok(zz_unit());
}

// Collect sorted entry names (no `.`/`..`). Returns NULL on error with
// errno preserved; `*n_out` holds the count.
static char **zz_fs_list_names(const char *p, size_t *n_out) {
    *n_out = 0;
#ifdef ZZ_OS_WINDOWS
    size_t plen = strlen(p);
    char *pat = (char *)malloc(plen + 3);
    if (!pat) return NULL;
    memcpy(pat, p, plen);
    pat[plen] = '\\';
    pat[plen + 1] = '*';
    pat[plen + 2] = '\0';
    WIN32_FIND_DATAA fd;
    HANDLE h = FindFirstFileA(pat, &fd);
    free(pat);
    if (h == INVALID_HANDLE_VALUE) return NULL;
    size_t cap = 16, n = 0;
    char **names = (char **)malloc(cap * sizeof(char *));
    if (!names) { FindClose(h); return NULL; }
    do {
        if (strcmp(fd.cFileName, ".") == 0 || strcmp(fd.cFileName, "..") == 0) continue;
        if (n == cap) {
            cap *= 2;
            char **nb = (char **)realloc(names, cap * sizeof(char *));
            if (!nb) break;
            names = nb;
        }
        names[n++] = copy_cstr(fd.cFileName, strlen(fd.cFileName));
    } while (FindNextFileA(h, &fd));
    FindClose(h);
#else
    DIR *d = opendir(p);
    if (!d) return NULL;
    size_t cap = 16, n = 0;
    char **names = (char **)malloc(cap * sizeof(char *));
    if (!names) { closedir(d); return NULL; }
    struct dirent *e;
    while ((e = readdir(d)) != NULL) {
        if (strcmp(e->d_name, ".") == 0 || strcmp(e->d_name, "..") == 0) continue;
        if (n == cap) {
            cap *= 2;
            char **nb = (char **)realloc(names, cap * sizeof(char *));
            if (!nb) break;
            names = nb;
        }
        names[n++] = copy_cstr(e->d_name, strlen(e->d_name));
    }
    closedir(d);
#endif
    // Insertion sort (directories are small; avoids qsort fn-pointer casts).
    for (size_t i = 1; i < n; i++) {
        char *t = names[i];
        size_t j = i;
        while (j > 0 && strcmp(names[j - 1], t) > 0) {
            names[j] = names[j - 1];
            j--;
        }
        names[j] = t;
    }
    *n_out = n;
    return names;
}

// fs.read_dir(path) → Result<[str]> (sorted basenames).
zz_value zz_fs_read_dir(zz_value path, int *err) {
    (void)err;
    const char *p = zz_fs_cstr(path);
    if (!p) { *err = 1; return zz_unit(); }
    zz_fs_top_up();
    size_t n = 0;
    char **names = zz_fs_list_names(p, &n);
    if (!names && errno != 0) {
        // Distinguish "not a directory" from generic I/O errors while
        // keeping the unified code shape.
        struct stat st;
        if (stat(p, &st) != 0) return zz_fs_err1("read_dir", p, errno);
#ifdef ZZ_OS_WINDOWS
        if ((st.st_mode & _S_IFMT) != _S_IFDIR) return zz_fs_err1("read_dir", p, ENOTDIR);
#else
        if (!S_ISDIR(st.st_mode)) return zz_fs_err1("read_dir", p, ENOTDIR);
#endif
        return zz_fs_err1("read_dir", p, EIO);
    }
    zz_value out = zz_array_new();
    for (size_t i = 0; i < n; i++) {
        zz_array_push(out.arr, zz_str_new(names[i], strlen(names[i])));
        free(names[i]);
    }
    free(names);
    return zz_variant_ok(out);
}

// Legacy alias: fs.readdir → read_dir.
zz_value zz_fs_readdir(zz_value path, int *err) {
    return zz_fs_read_dir(path, err);
}

// Recursive remove helper. Returns 0 or an errno.
static int zz_fs_rm_recursive(const char *p) {
#ifdef ZZ_OS_WINDOWS
    DWORD attrs = GetFileAttributesA(p);
    if (attrs == INVALID_FILE_ATTRIBUTES) return ENOENT;
    if (!(attrs & FILE_ATTRIBUTE_DIRECTORY)) {
        return DeleteFileA(p) ? 0 : EACCES;
    }
    size_t plen = strlen(p);
    char *pat = (char *)malloc(plen + 3);
    if (!pat) return ENOMEM;
    memcpy(pat, p, plen);
    pat[plen] = '\\';
    pat[plen + 1] = '*';
    pat[plen + 2] = '\0';
    WIN32_FIND_DATAA fd;
    HANDLE h = FindFirstFileA(pat, &fd);
    free(pat);
    int rc = 0;
    if (h != INVALID_HANDLE_VALUE) {
        do {
            if (strcmp(fd.cFileName, ".") == 0 || strcmp(fd.cFileName, "..") == 0) continue;
            size_t cl = strlen(p) + 1 + strlen(fd.cFileName) + 1;
            char *child = (char *)malloc(cl);
            if (!child) { rc = ENOMEM; break; }
            snprintf(child, cl, "%s\\%s", p, fd.cFileName);
            rc = zz_fs_rm_recursive(child);
            free(child);
            if (rc) break;
        } while (FindNextFileA(h, &fd));
        FindClose(h);
    }
    if (!rc && !RemoveDirectoryA(p)) rc = EACCES;
    return rc;
#else
    struct stat st;
    if (lstat(p, &st) != 0) return errno;
    if (!S_ISDIR(st.st_mode)) {
        return unlink(p) == 0 ? 0 : errno;
    }
    DIR *d = opendir(p);
    if (!d) return errno;
    int rc = 0;
    struct dirent *e;
    while ((e = readdir(d)) != NULL) {
        if (strcmp(e->d_name, ".") == 0 || strcmp(e->d_name, "..") == 0) continue;
        size_t cl = strlen(p) + 1 + strlen(e->d_name) + 1;
        char *child = (char *)malloc(cl);
        if (!child) { rc = ENOMEM; break; }
        snprintf(child, cl, "%s/%s", p, e->d_name);
        rc = zz_fs_rm_recursive(child);
        free(child);
        if (rc) break;
    }
    closedir(d);
    if (!rc && rmdir(p) != 0) rc = errno;
    return rc;
#endif
}

// fs.remove_dir_all(path) → Result<unit>
zz_value zz_fs_remove_dir_all(zz_value path, int *err) {
    (void)err;
    const char *p = zz_fs_cstr(path);
    if (!p) { *err = 1; return zz_unit(); }
    zz_fs_top_up();
    int rc = zz_fs_rm_recursive(p);
    if (rc != 0) return zz_fs_err1("remove_dir_all", p, rc);
    return zz_variant_ok(zz_unit());
}

// Recursive walk helper: appends full paths (files AND dirs, no root
// itself) to `out` (a C array with cap tracking).
static int zz_fs_walk_into(const char *dir, char ***out, size_t *len, size_t *cap) {
    size_t n = 0;
    char **names = zz_fs_list_names(dir, &n);
    if (!names) return errno != 0 ? errno : EIO;
    int rc = 0;
    for (size_t i = 0; i < n && !rc; i++) {
        size_t cl;
#ifdef ZZ_OS_WINDOWS
        cl = strlen(dir) + 1 + strlen(names[i]) + 1;
#else
        cl = strlen(dir) + 1 + strlen(names[i]) + 1;
#endif
        char *child = (char *)malloc(cl);
        if (!child) { rc = ENOMEM; break; }
#ifdef ZZ_OS_WINDOWS
        snprintf(child, cl, "%s\\%s", dir, names[i]);
#else
        snprintf(child, cl, "%s/%s", dir, names[i]);
#endif
        if (*len == *cap) {
            size_t nc = (*cap == 0) ? 16 : *cap * 2;
            char **nb = (char **)realloc(*out, nc * sizeof(char *));
            if (!nb) { free(child); rc = ENOMEM; break; }
            *out = nb;
            *cap = nc;
        }
        (*out)[(*len)++] = child;
        // lstat (never stat): symlinked dirs are recorded but never
        // descended — matching the VM (`DirEntry::file_type` does not
        // traverse symlinks) and immune to symlink cycles.
#ifdef ZZ_OS_WINDOWS
        struct stat st;
        int is_dir = stat(child, &st) == 0
            && (st.st_mode & _S_IFMT) == _S_IFDIR
            && !(GetFileAttributesA(child) & FILE_ATTRIBUTE_REPARSE_POINT);
#else
        struct stat st;
        int is_dir = lstat(child, &st) == 0 && S_ISDIR(st.st_mode);
#endif
        if (is_dir) {
            rc = zz_fs_walk_into(child, out, len, cap);
        }
    }
    for (size_t i = 0; i < n; i++) free(names[i]);
    free(names);
    return rc;
}

// fs.walk_dir(path) → Result<[str]> (sorted full paths, recursive).
zz_value zz_fs_walk_dir(zz_value path, int *err) {
    (void)err;
    const char *p = zz_fs_cstr(path);
    if (!p) { *err = 1; return zz_unit(); }
    zz_fs_top_up();
    struct stat st;
    if (stat(p, &st) != 0) return zz_fs_err1("walk_dir", p, errno);
#ifdef ZZ_OS_WINDOWS
    if ((st.st_mode & _S_IFMT) != _S_IFDIR) return zz_fs_err1("walk_dir", p, ENOTDIR);
#else
    if (!S_ISDIR(st.st_mode)) return zz_fs_err1("walk_dir", p, ENOTDIR);
#endif
    char **paths = NULL;
    size_t len = 0, cap = 0;
    int rc = zz_fs_walk_into(p, &paths, &len, &cap);
    if (rc != 0) {
        for (size_t i = 0; i < len; i++) free(paths[i]);
        free(paths);
        return zz_fs_err1("walk_dir", p, rc);
    }
    // Final global sort so VM and AOT agree byte-for-byte.
    for (size_t i = 1; i < len; i++) {
        char *t = paths[i];
        size_t j = i;
        while (j > 0 && strcmp(paths[j - 1], t) > 0) {
            paths[j] = paths[j - 1];
            j--;
        }
        paths[j] = t;
    }
    zz_value out = zz_array_new();
    for (size_t i = 0; i < len; i++) {
        zz_array_push(out.arr, zz_str_new(paths[i], strlen(paths[i])));
        free(paths[i]);
    }
    free(paths);
    return zz_variant_ok(out);
}

// fs.stat(path) → Result<dict<str,str>> with size, modified_ms,
// created_ms, is_file, is_dir, readonly (all strings — Dict<Str,Str>
// keeps the checker + codegen uniform across engines).
zz_value zz_fs_stat(zz_value path, int *err) {
    (void)err;
    const char *p = zz_fs_cstr(path);
    if (!p) { *err = 1; return zz_unit(); }
    struct stat st;
    if (stat(p, &st) != 0) return zz_fs_err1("stat", p, errno);
    char sizeb[32], modb[32], creb[32];
    snprintf(sizeb, sizeof sizeb, "%lld", (long long)st.st_size);
#if defined(ZZ_OS_MACOS)
    long long modms = (long long)st.st_mtimespec.tv_sec * 1000
        + st.st_mtimespec.tv_nsec / 1000000;
#ifdef st_birthtime
    long long crems = (long long)st.st_birthtimespec.tv_sec * 1000
        + st.st_birthtimespec.tv_nsec / 1000000;
#else
    long long crems = 0;
#endif
#elif defined(ZZ_OS_WINDOWS)
    long long modms = (long long)st.st_mtime * 1000;
    long long crems = (long long)st.st_ctime * 1000;
#else
    long long modms = (long long)st.st_mtim.tv_sec * 1000
        + st.st_mtim.tv_nsec / 1000000;
    long long crems = 0;
#endif
    snprintf(modb, sizeof modb, "%lld", modms);
    snprintf(creb, sizeof creb, "%lld", crems);
#ifdef ZZ_OS_WINDOWS
    int is_file = (st.st_mode & _S_IFMT) == _S_IFREG;
    int is_dir = (st.st_mode & _S_IFMT) == _S_IFDIR;
    int readonly = (st.st_mode & _S_IWRITE) == 0;
#else
    int is_file = S_ISREG(st.st_mode);
    int is_dir = S_ISDIR(st.st_mode);
    int readonly = (st.st_mode & 0222) == 0;
#endif
    zz_value d = zz_dict_new();
    zz_dict_set(d.dict, zz_str_static("size"), zz_str_new(sizeb, strlen(sizeb)));
    zz_dict_set(d.dict, zz_str_static("modified_ms"), zz_str_new(modb, strlen(modb)));
    zz_dict_set(d.dict, zz_str_static("created_ms"), zz_str_new(creb, strlen(creb)));
    zz_dict_set(d.dict, zz_str_static("is_file"),
                zz_str_new(is_file ? "true" : "false", is_file ? 4 : 5));
    zz_dict_set(d.dict, zz_str_static("is_dir"),
                zz_str_new(is_dir ? "true" : "false", is_dir ? 4 : 5));
    zz_dict_set(d.dict, zz_str_static("readonly"),
                zz_str_new(readonly ? "true" : "false", readonly ? 4 : 5));
    return zz_variant_ok(d);
}

// ---- streaming handles (ZZ_FILE) ----------------------------------------

struct zz_file {
    FILE *fp;
    char *path;   // owned copy for diagnostics
    int closed;
};

static struct zz_file *zz_file_alloc(FILE *fp, const char *path) {
    struct zz_file *f = (struct zz_file *)malloc(sizeof(struct zz_file));
    if (!f) return NULL;
    f->fp = fp;
    f->path = copy_cstr(path, strlen(path));
    f->closed = 0;
    return f;
}

static zz_value zz_file_closed_err(const char *op) {
    size_t n = strlen("fs:") + strlen(op) + strlen(":closed") + 1;
    char *msg = (char *)malloc(n);
    if (!msg) return zz_variant_err(zz_str_static("fs:closed"));
    snprintf(msg, n, "fs:%s:closed", op);
    return zz_variant_err(zz_str_owned(msg));
}

// fs.open(path, mode) → Result<file> where mode ∈ {r, w, a}.
zz_value zz_fs_open(zz_value path, zz_value mode, int *err) {
    (void)err;
    const char *p = zz_fs_cstr(path);
    const char *m = zz_fs_cstr(mode);
    if (!p || !m) { *err = 1; return zz_unit(); }
    const char *fmode = NULL;
    if (strcmp(m, "r") == 0) fmode = "rb";
    else if (strcmp(m, "w") == 0) fmode = "wb";
    else if (strcmp(m, "a") == 0) fmode = "ab";
    if (!fmode) {
        size_t n = strlen("fs:open:invalid_input: ") + strlen(p)
            + strlen(" (mode must be one of r, w, a)") + 1;
        char *msg = (char *)malloc(n);
        if (!msg) return zz_variant_err(zz_str_static("fs:open:invalid_input"));
        snprintf(msg, n, "fs:open:invalid_input: %s (mode must be one of r, w, a)", p);
        return zz_variant_err(zz_str_owned(msg));
    }
    zz_fs_top_up();
    FILE *fp = fopen(p, fmode);
    if (!fp) return zz_fs_err1("open", p, errno);
    struct zz_file *f = zz_file_alloc(fp, p);
    if (!f) {
        fclose(fp);
        return zz_variant_err(zz_str_static("fs:open:io_error"));
    }
    return zz_variant_ok((zz_value){ZZ_FILE, {.file = f}});
}

// file.read_chunk(f, n) → Result<str> ("" at EOF).
zz_value zz_fs_read_chunk(zz_value f, zz_value n, int *err) {
    (void)err;
    if (f.tag != ZZ_FILE || !f.file || f.file->closed || !f.file->fp) {
        return zz_file_closed_err("read_chunk");
    }
    if (n.tag != ZZ_INT || n.i < 0) { *err = 1; return zz_unit(); }
    size_t want = (size_t)n.i;
    if (want > 8 * 1024 * 1024) want = 8 * 1024 * 1024;
    zz_fs_top_up();
    // Heap temp + copy (disk-bound anyway): keeps error paths leak-free
    // without reaching into the string allocator's ownership rules.
    char *tmp = (char *)malloc(want > 0 ? want : 1);
    if (!tmp) return zz_fs_err1("read_chunk", f.file->path ? f.file->path : "?", ENOMEM);
    size_t got = want > 0 ? fread(tmp, 1, want, f.file->fp) : 0;
    if (got == 0 && ferror(f.file->fp)) {
        free(tmp);
        return zz_fs_err1("read_chunk", f.file->path ? f.file->path : "?", EIO);
    }
    zz_value s = zz_str_new(tmp, got);
    free(tmp);
    return zz_variant_ok(s);
}

// file.write_chunk(f, data) → Result<int> (bytes written).
zz_value zz_fs_write_chunk(zz_value f, zz_value data, int *err) {
    (void)err;
    if (f.tag != ZZ_FILE || !f.file || f.file->closed || !f.file->fp) {
        return zz_file_closed_err("write_chunk");
    }
    if (data.tag != ZZ_STR) { *err = 1; return zz_unit(); }
    zz_fs_top_up();
    size_t w = fwrite(zz_str_cptr(data.s), 1, data.s->len, f.file->fp);
    if (w != data.s->len) {
        return zz_fs_err1("write_chunk", f.file->path ? f.file->path : "?", EIO);
    }
    return zz_variant_ok((zz_value){ZZ_INT, {.i = (int64_t)w}});
}

// file.seek(f, pos) → Result<int> (absolute from start).
zz_value zz_fs_seek(zz_value f, zz_value pos, int *err) {
    (void)err;
    if (f.tag != ZZ_FILE || !f.file || f.file->closed || !f.file->fp) {
        return zz_file_closed_err("seek");
    }
    if (pos.tag != ZZ_INT || pos.i < 0) { *err = 1; return zz_unit(); }
    zz_fs_top_up();
    if (fseek(f.file->fp, (long)pos.i, SEEK_SET) != 0) {
        return zz_fs_err1("seek", f.file->path ? f.file->path : "?", errno);
    }
    long at = ftell(f.file->fp);
    return zz_variant_ok((zz_value){ZZ_INT, {.i = (int64_t)(at < 0 ? 0 : at)}});
}

// file.flush(f) → Result<unit>
zz_value zz_fs_flush(zz_value f, int *err) {
    (void)err;
    if (f.tag != ZZ_FILE || !f.file || f.file->closed || !f.file->fp) {
        return zz_file_closed_err("flush");
    }
    zz_fs_top_up();
    if (fflush(f.file->fp) != 0) {
        return zz_fs_err1("flush", f.file->path ? f.file->path : "?", errno);
    }
    return zz_variant_ok(zz_unit());
}

// file.close(f) → Result<unit> (idempotent).
zz_value zz_fs_close(zz_value f, int *err) {
    (void)err;
    if (f.tag != ZZ_FILE || !f.file) return zz_variant_ok(zz_unit());
    if (!f.file->closed && f.file->fp) {
        fflush(f.file->fp);
        fclose(f.file->fp);
        f.file->fp = NULL;
    }
    f.file->closed = 1;
    free(f.file->path);
    f.file->path = NULL;
    return zz_variant_ok(zz_unit());
}

// file.read_chunk_bytes(f, n) → Result<bytes> (binary-safe; empty at EOF).
zz_value zz_fs_read_chunk_bytes(zz_value f, zz_value n, int *err) {
    (void)err;
    if (f.tag != ZZ_FILE || !f.file || f.file->closed || !f.file->fp) {
        return zz_file_closed_err("read_chunk_bytes");
    }
    if (n.tag != ZZ_INT || n.i < 0) { *err = 1; return zz_unit(); }
    size_t want = (size_t)n.i;
    if (want > 8 * 1024 * 1024) want = 8 * 1024 * 1024;
    zz_fs_top_up();
    unsigned char *tmp = (unsigned char *)malloc(want > 0 ? want : 1);
    if (!tmp) return zz_fs_err1("read_chunk_bytes", f.file->path ? f.file->path : "?", ENOMEM);
    size_t got = want > 0 ? fread(tmp, 1, want, f.file->fp) : 0;
    if (got == 0 && ferror(f.file->fp)) {
        free(tmp);
        return zz_fs_err1("read_chunk_bytes", f.file->path ? f.file->path : "?", EIO);
    }
    return zz_variant_ok(zz_bytes_take(tmp, got));
}

// ---- cross-platform path lexing (mirrors natives/fs/path.rs) ---------------
//
// Style follows the binary's target (`ZZ_OS_WINDOWS` → `\`, else `/`),
// matching the VM side, which resolves `sys.os()` at call time. Both
// engines accept both separators as input.
#ifdef ZZ_OS_WINDOWS
#define ZZ_PATH_SEP '\\'
#define ZZ_PATH_WIN 1
#else
#define ZZ_PATH_SEP '/'
#define ZZ_PATH_WIN 0
#endif

static int zz_path_is_sep(char c) { return c == '/' || c == '\\'; }

// Growable byte buffer owned by the caller (freed on every path).
struct zz_path_buf { char *p; size_t len; size_t cap; };
static void zz_path_push(struct zz_path_buf *b, const char *s, size_t n) {
    if (b->len + n + 1 > b->cap) {
        size_t cap = b->cap ? b->cap * 2 : 64;
        while (cap < b->len + n + 1) cap *= 2;
        char *np = (char *)realloc(b->p, cap);
        if (!np) return; // OOM: caller falls back to a static string
        b->p = np;
        b->cap = cap;
    }
    memcpy(b->p + b->len, s, n);
    b->len += n;
    b->p[b->len] = '\0';
}
static void zz_path_push1(struct zz_path_buf *b, char c) { zz_path_push(b, &c, 1); }

// Normalize `p` (NUL-terminated) into a fresh `zz_str`. Never fails from
// ZZ's perspective: OOM falls back to "." (total, like the VM's infallible
// string ops).
static zz_value zz_path_normalize_str(const char *p) {
    struct zz_path_buf out = {NULL, 0, 0};
    size_t n = strlen(p);
    if (n == 0) return zz_str_static(".");
    // Prefix end (exclusive) + whether `..` clamps (rooted, non-drive-relative).
    size_t pre_end = 0;
    int rooted = 0;
    if (ZZ_PATH_WIN) {
        if (n >= 2 && zz_path_is_sep(p[0]) && zz_path_is_sep(p[1])) {
            // UNC: `\\server\share`.
            size_t i = 2;
            for (int k = 0; k < 2; k++) {
                while (i < n && zz_path_is_sep(p[i])) i++;
                size_t s = i;
                while (i < n && !zz_path_is_sep(p[i])) i++;
                if (s == i) break;
                if (k == 1) pre_end = i;
            }
            if (pre_end == 0) pre_end = i;
            zz_path_push1(&out, ZZ_PATH_SEP);
            zz_path_push1(&out, ZZ_PATH_SEP);
            // Re-emit `server/share` canonically (skip separators).
            size_t j = 2;
            int first = 1;
            while (j < pre_end) {
                while (j < pre_end && zz_path_is_sep(p[j])) j++;
                size_t s = j;
                while (j < pre_end && !zz_path_is_sep(p[j])) j++;
                if (j > s) {
                    if (!first) zz_path_push1(&out, ZZ_PATH_SEP);
                    zz_path_push(&out, p + s, j - s);
                    first = 0;
                }
            }
            rooted = 1;
        } else if (n >= 2 && ((p[0] >= 'A' && p[0] <= 'Z') || (p[0] >= 'a' && p[0] <= 'z'))
                   && p[1] == ':') {
            zz_path_push(&out, p, 1);
            zz_path_push1(&out, ':');
            pre_end = 2;
            if (n > 2 && zz_path_is_sep(p[2])) {
                zz_path_push1(&out, ZZ_PATH_SEP);
                pre_end = 3;
                rooted = 1;
            }
        } else if (n >= 1 && zz_path_is_sep(p[0])) {
            // Rooted on the current drive (`\x`).
            zz_path_push1(&out, ZZ_PATH_SEP);
            pre_end = 1;
            rooted = 1;
        }
    } else {
        if (zz_path_is_sep(p[0])) {
            zz_path_push1(&out, '/');
            pre_end = 1;
            rooted = 1;
        }
    }
    // Segment stack: offsets into a kept-segment list (index/len pairs).
    size_t *idx = (size_t *)malloc(sizeof(size_t) * (n + 1));
    size_t *slen = (size_t *)malloc(sizeof(size_t) * (n + 1));
    size_t kept = 0;
    if (!idx || !slen) {
        free(idx);
        free(slen);
        free(out.p);
        return zz_str_static(".");
    }
    size_t i = pre_end;
    // Collapse separators right after a rooted prefix (`C://x` → `C:\x`).
    if (pre_end > 0) {
        while (i < n && zz_path_is_sep(p[i])) i++;
    }
    while (i < n) {
        while (i < n && zz_path_is_sep(p[i])) i++;
        if (i >= n) break;
        size_t s = i;
        while (i < n && !zz_path_is_sep(p[i])) i++;
        size_t e = i; // e > s: separators were skipped above
        if (e - s == 1 && p[s] == '.') continue;
        if (e - s == 2 && p[s] == '.' && p[s + 1] == '.') {
            // Pop a real segment only; `..` never cancels `..`.
            if (kept > 0) {
                size_t li = idx[kept - 1];
                size_t ll = slen[kept - 1];
                if (!(ll == 2 && p[li] == '.' && p[li + 1] == '.')) {
                    kept--;
                    continue;
                }
            }
            if (!rooted) {
                idx[kept] = s;
                slen[kept] = 2;
                kept++;
            }
            continue;
        }
        idx[kept] = s;
        slen[kept] = e - s;
        kept++;
    }
    for (size_t k = 0; k < kept; k++) {
        if (out.len > 0 && out.p[out.len - 1] != ZZ_PATH_SEP) zz_path_push1(&out, ZZ_PATH_SEP);
        zz_path_push(&out, p + idx[k], slen[k]);
    }
    free(idx);
    free(slen);
    if (out.len == 0) {
        free(out.p);
        return zz_str_static(".");
    }
    zz_value v = zz_str_new(out.p, out.len);
    free(out.p);
    return v;
}

// Last separator position in a NUL string, or -1.
static long zz_path_rsep(const char *p) {
    long at = -1;
    for (long i = 0; p[i]; i++) {
        if (zz_path_is_sep(p[i])) at = i;
    }
    return at;
}

zz_value zz_fs_normalize(zz_value path, int *err) {
    (void)err;
    const char *p = zz_fs_cstr(path);
    if (!p) { *err = 1; return zz_unit(); }
    return zz_path_normalize_str(p);
}

zz_value zz_fs_join(zz_value a, zz_value b, int *err) {
    (void)err;
    const char *pa = zz_fs_cstr(a);
    const char *pb = zz_fs_cstr(b);
    if (!pa || !pb) { *err = 1; return zz_unit(); }
    if (pb[0] == '\0') return zz_path_normalize_str(pa);
    // `b` absolute → wins (same rule as `fs.is_absolute` on raw input).
    int b_abs;
    if (ZZ_PATH_WIN) {
        b_abs = zz_path_is_sep(pb[0])  // UNC `\\s\..` or rooted `\x`
            || (pb[0] && pb[1] == ':' && ((pb[0] >= 'A' && pb[0] <= 'Z') || (pb[0] >= 'a' && pb[0] <= 'z'))
                && pb[2] && zz_path_is_sep(pb[2]));  // drive-absolute `C:\`
    } else {
        b_abs = zz_path_is_sep(pb[0]);
    }
    if (b_abs) return zz_path_normalize_str(pb);
    if (pa[0] == '\0') return zz_path_normalize_str(pb);
    size_t na = strlen(pa), nbb = strlen(pb);
    char *cat = (char *)malloc(na + nbb + 2);
    if (!cat) return zz_str_static(".");
    memcpy(cat, pa, na);
    cat[na] = '/';
    memcpy(cat + na + 1, pb, nbb + 1);
    zz_value v = zz_path_normalize_str(cat);
    free(cat);
    return v;
}

zz_value zz_fs_basename(zz_value path, int *err) {
    (void)err;
    const char *p = zz_fs_cstr(path);
    if (!p) { *err = 1; return zz_unit(); }
    zz_value n = zz_path_normalize_str(p);
    const char *q = zz_str_cptr(n.s);
    size_t len = n.s->len;
    if (len == 1 && q[0] == ZZ_PATH_SEP) return n; // root
    if (ZZ_PATH_WIN && len == 3 && q[1] == ':' && q[2] == ZZ_PATH_SEP) return n; // drive root
    if (len == 1 && q[0] == '.') return n;
    long at = zz_path_rsep(q);
    if (at < 0) return n;
    zz_value v = zz_str_new(q + at + 1, len - (size_t)at - 1);
    zz_release(&n);
    return v;
}

zz_value zz_fs_dirname(zz_value path, int *err) {
    (void)err;
    const char *p = zz_fs_cstr(path);
    if (!p) { *err = 1; return zz_unit(); }
    zz_value n = zz_path_normalize_str(p);
    const char *q = zz_str_cptr(n.s);
    size_t len = n.s->len;
    long at = zz_path_rsep(q);
    if (at < 0) {
        zz_release(&n);
        return zz_str_static(".");
    }
    if (at == 0) {
        zz_release(&n);
        return zz_str_new(&((char){ZZ_PATH_SEP}), 1);
    }
    if (ZZ_PATH_WIN && at == 2 && q[1] == ':') { // `C:\x` → `C:\`
        zz_value v = zz_str_new(q, 3);
        zz_release(&n);
        return v;
    }
    zz_value v = zz_str_new(q, (size_t)at);
    zz_release(&n);
    (void)len;
    return v;
}

zz_value zz_fs_is_absolute(zz_value path, int *err) {
    (void)err;
    const char *p = zz_fs_cstr(path);
    if (!p) { *err = 1; return zz_unit(); }
    int abs = 0;
    if (ZZ_PATH_WIN) {
        if (p[0] && zz_path_is_sep(p[0]) && p[1] && zz_path_is_sep(p[1])) {
            abs = 1; // UNC
        } else if (p[0] && p[1] == ':' && ((p[0] >= 'A' && p[0] <= 'Z')
                                           || (p[0] >= 'a' && p[0] <= 'z'))) {
            abs = (p[2] != '\0' && zz_path_is_sep(p[2]));
        }
    } else {
        abs = zz_path_is_sep(p[0]);
    }
    return (zz_value){ZZ_BOOL, {.b = abs}};
}

zz_value zz_fs_extension(zz_value path, int *err) {
    int e = 0;
    zz_value b = zz_fs_basename(path, &e);
    if (e) { *err = e; return zz_unit(); }
    const char *q = zz_str_cptr(b.s);
    size_t len = b.s->len;
    if (len == 0 || (len == 1 && q[0] == '.') || (len == 2 && q[0] == '.' && q[1] == '.')) {
        return b;
    }
    long dot = -1;
    for (long k = 0; q[k]; k++) {
        if (q[k] == '.') dot = k;
    }
    if (dot <= 0) {
        zz_release(&b);
        return zz_str_static("");
    }
    zz_value v = zz_str_new(q + dot + 1, len - (size_t)dot - 1);
    zz_release(&b);
    return v;
}

// ---- virtual filesystem providers (mirrors `natives/fs/vfs.rs`) ------------
//
// Handle = process-lifetime `zz_vfs` (never freed — same discipline as
// `zz_file`). Tree providers keep an O(n) entry list; right for test,
// cache, and asset scale (not a database).

struct zz_vfs_entry {
    char *path;              // canonical `/`-rooted key (owned)
    unsigned char *data;     // file bytes (NULL for dirs)
    size_t len;
    int is_dir;
    struct zz_vfs_entry *next;
};

struct zz_vfs {
    int kind;                // 0 = os, 1 = mem, 2 = tar, 3 = embed
    struct zz_vfs_entry *entries;
};

static struct zz_vfs_entry *zz_vfs_find(struct zz_vfs *v, const char *key) {
    for (struct zz_vfs_entry *e = v->entries; e; e = e->next) {
        if (strcmp(e->path, key) == 0) return e;
    }
    return NULL;
}

// Canonical virtual key: normalize, root at `/`, force `/` separators.
static char *zz_vfs_key(const char *raw) {
    zz_value n = zz_path_normalize_str(raw);
    const char *q = zz_str_cptr(n.s);
    size_t len = n.s->len;
    char *key;
    if ((len == 1 && q[0] == '.') || len == 0) {
        key = copy_cstr("/", 1);
    } else if (q[0] == '/' || q[0] == '\\') {
        key = (char *)malloc(len + 1);
        if (key) {
            for (size_t i = 0; i <= len; i++) key[i] = q[i] == '\\' ? '/' : q[i];
        }
    } else if ((len == 2 && q[0] == '.' && q[1] == '.')
               || (len > 3 && q[0] == '.' && q[1] == '.' && (q[2] == '/' || q[2] == '\\'))) {
        key = copy_cstr("/", 1); // `..` above the virtual root clamps
    } else {
        key = (char *)malloc(len + 2);
        if (key) {
            key[0] = '/';
            for (size_t i = 0; i <= len; i++) key[i + 1] = q[i] == '\\' ? '/' : q[i];
        }
    }
    zz_release(&n);
    return key;
}

static int zz_vfs_is_dir_key(struct zz_vfs *v, const char *key) {
    if (strcmp(key, "/") == 0) return 1;
    struct zz_vfs_entry *e = zz_vfs_find(v, key);
    return e && e->is_dir;
}

// Create implied parents of `key` (all but the last segment).
static int zz_vfs_mkdir_parents(struct zz_vfs *v, const char *key) {
    // Collect segment boundaries first so insertion can't disturb parsing.
    const char *seps[256];
    size_t nsep = 0;
    for (const char *p = key; *p && nsep < 256; p++) {
        if (*p == '/') seps[nsep++] = p;
    }
    // Parents = prefixes ending before each `/` after the first, excluding
    // the full key itself (last segment is the file/dir being created).
    for (size_t k = 1; k < nsep; k++) {
        size_t plen = (size_t)(seps[k] - key);
        if (plen == 0) continue;
        char *parent = (char *)malloc(plen + 1);
        if (!parent) return -1;
        memcpy(parent, key, plen);
        parent[plen] = '\0';
        struct zz_vfs_entry *e = zz_vfs_find(v, parent);
        if (!e) {
            e = (struct zz_vfs_entry *)malloc(sizeof(struct zz_vfs_entry));
            if (!e) {
                free(parent);
                return -1;
            }
            e->path = parent;
            e->data = NULL;
            e->len = 0;
            e->is_dir = 1;
            e->next = v->entries;
            v->entries = e;
        } else {
            free(parent);
            if (!e->is_dir) return -1; // file blocks the path
        }
    }
    return 0;
}

static zz_value zz_vfs_at_closed(const char *op) {
    size_t n = strlen("fs:") + strlen(op) + strlen(":closed") + 1;
    char *msg = (char *)malloc(n);
    if (!msg) return zz_variant_err(zz_str_static("fs:closed"));
    snprintf(msg, n, "fs:%s:closed", op);
    return zz_variant_err(zz_str_owned(msg));
}

static zz_value zz_vfs_code_err(const char *op, const char *path, const char *code) {
    size_t n = strlen("fs:") + strlen(op) + 1 + strlen(code) + 2 + strlen(path) + 1;
    char *msg = (char *)malloc(n);
    if (!msg) return zz_variant_err(zz_str_static("fs:io_error"));
    snprintf(msg, n, "fs:%s:%s: %s", op, code, path);
    return zz_variant_err(zz_str_owned(msg));
}

static zz_value zz_vfs_readonly_err(const char *op, const char *path) {
    size_t n = strlen("fs:") + strlen(op)
        + strlen(":invalid_input: ") + strlen(path) + strlen(" (read-only filesystem)") + 1;
    char *msg = (char *)malloc(n);
    if (!msg) return zz_variant_err(zz_str_static("fs:invalid_input"));
    snprintf(msg, n, "fs:%s:invalid_input: %s (read-only filesystem)", op, path);
    return zz_variant_err(zz_str_owned(msg));
}

static int zz_vfs_check(zz_value fsys, struct zz_vfs **out) {
    if (fsys.tag != ZZ_VFS || !fsys.vfs) return 0;
    *out = fsys.vfs;
    return 1;
}

// Parse a plain (uncompressed) tar image into `v`. Regular files and
// dirs only; GNU long names (`L`) apply to the next entry. Returns NULL
// on success or a malloc'd reason string.
static char *zz_vfs_parse_tar(struct zz_vfs *v, const unsigned char *bytes, size_t total) {
    size_t i = 0;
    char *longname = NULL;
    while (i + 512 <= total) {
        const unsigned char *hdr = bytes + i;
        int allzero = 1;
        for (size_t k = 0; k < 512; k++) {
            if (hdr[k]) {
                allzero = 0;
                break;
            }
        }
        if (allzero) break;
        char name[512];
        size_t nlen = 0;
        while (nlen < 100 && hdr[nlen]) nlen++;
        size_t plen = 0;
        while (plen < 155 && hdr[345 + plen]) plen++;
        if (plen > 0) {
            size_t cp = plen < 200 ? plen : 200;
            memcpy(name, hdr + 345, cp);
            name[cp] = '/';
            size_t cn = nlen < 300 ? nlen : 300;
            if (cp + 1 + cn > sizeof(name) - 1) cn = sizeof(name) - 1 - cp - 1;
            memcpy(name + cp + 1, hdr, cn);
            nlen = cp + 1 + cn;
        } else {
            size_t cn = nlen < sizeof(name) - 1 ? nlen : sizeof(name) - 1;
            memcpy(name, hdr, cn);
            nlen = cn;
        }
        name[nlen] = '\0';
        char sizef[13];
        memcpy(sizef, hdr + 124, 12);
        sizef[12] = '\0';
        char *end = NULL;
        unsigned long size = strtoul(sizef, &end, 8);
        if (end == sizef) {
            free(longname);
            char *m = (char *)malloc(128);
            if (m) snprintf(m, 128, "malformed size field for entry `%.64s`", name);
            return m;
        }
        unsigned char typeflag = hdr[156];
        i += 512;
        if (size > total || i > total - size) {
            free(longname);
            char *m = (char *)malloc(128);
            if (m) snprintf(m, 128, "truncated data for entry `%.64s`", name);
            return m;
        }
        const unsigned char *data = bytes + i;
        i += (size + 511) / 512 * 512;
        if (typeflag == 'L') {
            free(longname);
            longname = (char *)malloc(size + 1);
            if (!longname) return copy_cstr("out of memory", 13);
            memcpy(longname, data, size);
            longname[size] = '\0';
            // Strip trailing NULs.
            size_t ll = size;
            while (ll > 0 && longname[ll - 1] == '\0') ll--;
            longname[ll] = '\0';
        } else if (typeflag == '0' || typeflag == '\0' || typeflag == '5') {
            const char *entry_name = longname ? longname : name;
            char *key = zz_vfs_key(entry_name);
            free(longname);
            longname = NULL;
            if (!key) return copy_cstr("out of memory", 13);
            if (typeflag == '5') {
                if (zz_vfs_mkdir_parents(v, key) != 0) {
                    free(key);
                    return copy_cstr("conflicting entry", 18);
                }
                if (!zz_vfs_find(v, key)) {
                    struct zz_vfs_entry *e = (struct zz_vfs_entry *)malloc(sizeof(*e));
                    if (!e) {
                        free(key);
                        return copy_cstr("out of memory", 13);
                    }
                    e->path = key;
                    e->data = NULL;
                    e->len = 0;
                    e->is_dir = 1;
                    e->next = v->entries;
                    v->entries = e;
                } else {
                    free(key);
                }
            } else {
                struct zz_vfs_entry *e = zz_vfs_find(v, key);
                if (e && e->is_dir) {
                    free(key);
                    return copy_cstr("conflicting entry", 18);
                }
                if (zz_vfs_mkdir_parents(v, key) != 0) {
                    free(key);
                    return copy_cstr("conflicting entry", 18);
                }
                if (!e) {
                    e = (struct zz_vfs_entry *)malloc(sizeof(*e));
                    if (!e) {
                        free(key);
                        return copy_cstr("out of memory", 13);
                    }
                    e->path = key;
                    e->data = NULL;
                    e->next = v->entries;
                    v->entries = e;
                } else {
                    free(key);
                }
                free(e->data);
                e->data = size ? (unsigned char *)malloc(size ? size : 1) : NULL;
                if (size && !e->data) return copy_cstr("out of memory", 13);
                if (size) memcpy(e->data, data, size);
                e->len = size;
                e->is_dir = 0;
            }
        } else {
            free(longname);
            longname = NULL;
        }
    }
    return NULL;
}

static struct zz_vfs *zz_vfs_alloc(int kind) {
    struct zz_vfs *v = (struct zz_vfs *)malloc(sizeof(struct zz_vfs));
    if (!v) return NULL;
    v->kind = kind;
    v->entries = NULL;
    return v;
}

zz_value zz_fs_osfs(zz_value unused, int *err) {
    (void)err;
    (void)unused;
    struct zz_vfs *v = zz_vfs_alloc(0);
    if (!v) return zz_variant_err(zz_str_static("fs:osfs:io_error"));
    return zz_variant_ok((zz_value){ZZ_VFS, {.vfs = v}});
}

zz_value zz_fs_memfs(zz_value unused, int *err) {
    (void)err;
    (void)unused;
    struct zz_vfs *v = zz_vfs_alloc(1);
    if (!v) return zz_variant_err(zz_str_static("fs:memfs:io_error"));
    return zz_variant_ok((zz_value){ZZ_VFS, {.vfs = v}});
}

zz_value zz_fs_tarfs(zz_value path, int *err) {
    (void)err;
    const char *p = zz_fs_cstr(path);
    if (!p) { *err = 1; return zz_unit(); }
    zz_fs_top_up();
    FILE *f = fopen(p, "rb");
    if (!f) return zz_fs_err1("tarfs", p, errno);
    fseek(f, 0, SEEK_END);
    long sz = ftell(f);
    fseek(f, 0, SEEK_SET);
    if (sz < 0) sz = 0;
    unsigned char *buf = (unsigned char *)malloc((size_t)sz > 0 ? (size_t)sz : 1);
    if (!buf) {
        fclose(f);
        return zz_fs_err1("tarfs", p, ENOMEM);
    }
    size_t n = fread(buf, 1, (size_t)sz, f);
    int ferr = ferror(f);
    fclose(f);
    if (ferr) {
        free(buf);
        return zz_fs_err1("tarfs", p, EIO);
    }
    struct zz_vfs *v = zz_vfs_alloc(2);
    if (!v) {
        free(buf);
        return zz_fs_err1("tarfs", p, ENOMEM);
    }
    char *reason = zz_vfs_parse_tar(v, buf, n);
    free(buf);
    if (reason) {
        // Free any entries parsed before the failure.
        struct zz_vfs_entry *e = v->entries;
        while (e) {
            struct zz_vfs_entry *nx = e->next;
            free(e->path);
            free(e->data);
            free(e);
            e = nx;
        }
        free(v);
        size_t mlen = strlen("fs:tarfs:invalid_input: ") + strlen(p) + 3 + strlen(reason) + 1;
        char *msg = (char *)malloc(mlen);
        if (!msg) {
            free(reason);
            return zz_variant_err(zz_str_static("fs:tarfs:invalid_input"));
        }
        snprintf(msg, mlen, "fs:tarfs:invalid_input: %s (%s)", p, reason);
        free(reason);
        return zz_variant_err(zz_str_owned(msg));
    }
    return zz_variant_ok((zz_value){ZZ_VFS, {.vfs = v}});
}

// Process-wide embed table (populated by `zz_embed_register`).
static struct zz_vfs_entry *zz_embed_entries = NULL;

void zz_embed_register(const char *name, const unsigned char *data, size_t len) {
    char *key = zz_vfs_key(name);
    if (!key) return;
    for (struct zz_vfs_entry *e = zz_embed_entries; e; e = e->next) {
        if (strcmp(e->path, key) == 0) {
            free(key);
            return; // first registration wins
        }
    }
    struct zz_vfs_entry *e = (struct zz_vfs_entry *)malloc(sizeof(*e));
    if (!e) {
        free(key);
        return;
    }
    e->path = key;
    e->len = len;
    e->is_dir = 0;
    e->data = len ? (unsigned char *)malloc(len) : NULL;
    if (len && !e->data) {
        free(key);
        free(e);
        return;
    }
    if (len) memcpy(e->data, data, len);
    e->next = zz_embed_entries;
    zz_embed_entries = e;
    // Implied parent dirs are resolved at lookup time (prefix scan), so no
    // separate dir entries are needed.
}

zz_value zz_fs_embedfs(zz_value unused, int *err) {
    (void)err;
    (void)unused;
    struct zz_vfs *v = zz_vfs_alloc(3);
    if (!v) return zz_variant_err(zz_str_static("fs:embedfs:io_error"));
    // Snapshot shares entry pointers (embed data is process-lifetime).
    for (struct zz_vfs_entry *e = zz_embed_entries; e; e = e->next) {
        struct zz_vfs_entry *c = (struct zz_vfs_entry *)malloc(sizeof(*c));
        if (!c) break;
        c->path = copy_cstr(e->path, strlen(e->path));
        if (!c->path) {
            free(c);
            break;
        }
        c->data = e->data;
        c->len = e->len;
        c->is_dir = 0;
        c->next = v->entries;
        v->entries = c;
    }
    return zz_variant_ok((zz_value){ZZ_VFS, {.vfs = v}});
}

// Lookup shared by tree providers (mem/tar/embed). Embed dirs resolve by
// prefix scan since only file entries are registered.
static struct zz_vfs_entry *zz_vfs_tree_find(struct zz_vfs *v, const char *key) {
    struct zz_vfs_entry *e = zz_vfs_find(v, key);
    if (e || v->kind != 3 || strcmp(key, "/") == 0) return e;
    // Embed dir? Any entry under `key/`.
    size_t klen = strlen(key);
    for (struct zz_vfs_entry *c = v->entries; c; c = c->next) {
        if (strncmp(c->path, key, klen) == 0 && c->path[klen] == '/') return c;
    }
    return NULL;
}

static int zz_vfs_tree_is_dir(struct zz_vfs *v, const char *key) {
    if (strcmp(key, "/") == 0) return 1;
    struct zz_vfs_entry *e = zz_vfs_find(v, key);
    if (e) return e->is_dir;
    if (v->kind != 3) return 0;
    size_t klen = strlen(key);
    for (struct zz_vfs_entry *c = v->entries; c; c = c->next) {
        if (strncmp(c->path, key, klen) == 0 && c->path[klen] == '/') return 1;
    }
    return 0;
}

static int zz_vfs_str_is_utf8(const unsigned char *d, size_t n) {
    size_t i = 0;
    while (i < n) {
        unsigned char c = d[i];
        size_t need;
        if (c < 0x80) need = 1;
        else if ((c & 0xE0) == 0xC0) need = 2;
        else if ((c & 0xF0) == 0xE0) need = 3;
        else if ((c & 0xF8) == 0xF0) need = 4;
        else return 0;
        if (i + need > n) return 0;
        for (size_t k = 1; k < need; k++) {
            if ((d[i + k] & 0xC0) != 0x80) return 0;
        }
        i += need;
    }
    return 1;
}

zz_value zz_fs_read_to_string_at(zz_value fsys, zz_value path, int *err) {
    (void)err;
    struct zz_vfs *v;
    if (!zz_vfs_check(fsys, &v)) return zz_vfs_at_closed("read_to_string_at");
    const char *p = zz_fs_cstr(path);
    if (!p || path.tag != ZZ_STR) { *err = 1; return zz_unit(); }
    if (v->kind == 0) return zz_fs_read(path, err);
    char *key = zz_vfs_key(p);
    if (!key) return zz_vfs_code_err("read", p, "io_error");
    // Exact match only: a child entry must never stand in for its dir.
    struct zz_vfs_entry *e = zz_vfs_find(v, key);
    zz_value r;
    if (!e) {
        r = zz_vfs_code_err("read", p, zz_vfs_tree_is_dir(v, key) ? "io_error" : "not_found");
    } else if (e->is_dir) {
        r = zz_vfs_code_err("read", p, "io_error");
    } else if (!zz_vfs_str_is_utf8(e->data ? e->data : (unsigned char *)"", e->len)) {
        r = zz_vfs_code_err("read", p, "invalid_input");
    } else {
        r = zz_variant_ok(zz_str_new(e->data ? (char *)e->data : "", e->len));
    }
    free(key);
    return r;
}

zz_value zz_fs_read_bytes_at(zz_value fsys, zz_value path, int *err) {
    (void)err;
    struct zz_vfs *v;
    if (!zz_vfs_check(fsys, &v)) return zz_vfs_at_closed("read_bytes_at");
    const char *p = zz_fs_cstr(path);
    if (!p || path.tag != ZZ_STR) { *err = 1; return zz_unit(); }
    if (v->kind == 0) return zz_fs_read_bytes(path, err);
    char *key = zz_vfs_key(p);
    if (!key) return zz_vfs_code_err("read_bytes", p, "io_error");
    struct zz_vfs_entry *e = zz_vfs_find(v, key);
    zz_value r;
    if (!e) {
        r = zz_vfs_code_err("read_bytes", p, zz_vfs_tree_is_dir(v, key) ? "io_error" : "not_found");
    } else if (e->is_dir) {
        r = zz_vfs_code_err("read_bytes", p, "io_error");
    } else {
        r = zz_variant_ok(zz_bytes_new(e->data, e->len));
    }
    free(key);
    return r;
}

zz_value zz_fs_write_at(zz_value fsys, zz_value path, zz_value data, int *err) {
    (void)err;
    struct zz_vfs *v;
    if (!zz_vfs_check(fsys, &v)) return zz_vfs_at_closed("write_at");
    const char *p = zz_fs_cstr(path);
    if (!p || path.tag != ZZ_STR || data.tag != ZZ_STR) { *err = 1; return zz_unit(); }
    if (v->kind == 0) return zz_fs_write(path, data, err);
    if (v->kind != 1) return zz_vfs_readonly_err("write", p);
    const char *d = zz_str_cptr(data.s);
    size_t dlen = data.s->len;
    char *key = zz_vfs_key(p);
    if (!key) return zz_vfs_code_err("write", p, "io_error");
    struct zz_vfs_entry *e = zz_vfs_find(v, key);
    zz_value r;
    if (e && e->is_dir) {
        r = zz_vfs_code_err("write", p, "io_error");
    } else if (zz_vfs_mkdir_parents(v, key) != 0) {
        r = zz_vfs_code_err("write", p, "io_error");
    } else {
        if (!e) {
            e = (struct zz_vfs_entry *)malloc(sizeof(*e));
            if (!e) {
                free(key);
                return zz_vfs_code_err("write", p, "io_error");
            }
            e->path = key;
            key = NULL;
            e->data = NULL;
            e->next = v->entries;
            v->entries = e;
        }
        free(e->data);
        e->data = dlen ? (unsigned char *)malloc(dlen) : NULL;
        if (dlen && !e->data) {
            r = zz_vfs_code_err("write", p, "io_error");
        } else {
            if (dlen) memcpy(e->data, d, dlen);
            e->len = dlen;
            e->is_dir = 0;
            r = zz_variant_ok(zz_unit());
        }
    }
    free(key);
    return r;
}

zz_value zz_fs_append_at(zz_value fsys, zz_value path, zz_value data, int *err) {
    (void)err;
    struct zz_vfs *v;
    if (!zz_vfs_check(fsys, &v)) return zz_vfs_at_closed("append_at");
    const char *p = zz_fs_cstr(path);
    if (!p || path.tag != ZZ_STR || data.tag != ZZ_STR) { *err = 1; return zz_unit(); }
    if (v->kind == 0) return zz_fs_append(path, data, err);
    if (v->kind != 1) return zz_vfs_readonly_err("append", p);
    const char *d = zz_str_cptr(data.s);
    size_t dlen = data.s->len;
    char *key = zz_vfs_key(p);
    if (!key) return zz_vfs_code_err("append", p, "io_error");
    struct zz_vfs_entry *e = zz_vfs_find(v, key);
    zz_value r;
    if (e && e->is_dir) {
        r = zz_vfs_code_err("append", p, "io_error");
    } else if (zz_vfs_mkdir_parents(v, key) != 0) {
        r = zz_vfs_code_err("append", p, "io_error");
    } else {
        if (!e) {
            e = (struct zz_vfs_entry *)malloc(sizeof(*e));
            if (!e) {
                free(key);
                return zz_vfs_code_err("append", p, "io_error");
            }
            e->path = key;
            key = NULL;
            e->data = NULL;
            e->len = 0;
            e->is_dir = 0;
            e->next = v->entries;
            v->entries = e;
        }
        unsigned char *nd = (unsigned char *)malloc(e->len + dlen + 1);
        if (!nd && e->len + dlen > 0) {
            r = zz_vfs_code_err("append", p, "io_error");
        } else {
            if (e->len) memcpy(nd, e->data, e->len);
            if (dlen) memcpy(nd + e->len, d, dlen);
            free(e->data);
            e->data = nd;
            e->len += dlen;
            r = zz_variant_ok(zz_unit());
        }
    }
    free(key);
    return r;
}

static zz_value zz_vfs_pred_at(zz_value fsys, zz_value path, int *err, const char *op, int want) {
    // want: 0 = exists, 1 = is_file, 2 = is_dir.
    (void)err;
    struct zz_vfs *v;
    if (!zz_vfs_check(fsys, &v)) {
        // Predicates never fail the program (mirror `exists` totality).
        return (zz_value){ZZ_BOOL, {.b = false}};
    }
    const char *p = zz_fs_cstr(path);
    if (!p || path.tag != ZZ_STR) { *err = 1; return zz_unit(); }
    if (v->kind == 0) {
        if (want == 0) return zz_fs_exists(path, err);
        if (want == 1) return zz_fs_is_file(path, err);
        return zz_fs_is_dir(path, err);
    }
    char *key = zz_vfs_key(p);
    if (!key) return (zz_value){ZZ_BOOL, {.b = false}};
    // Exact match only: a child entry must never stand in for its dir
    // (embed registers files alone; dirs resolve by prefix scan).
    struct zz_vfs_entry *e = zz_vfs_find(v, key);
    int isdir = e ? e->is_dir : zz_vfs_tree_is_dir(v, key);
    free(key);
    (void)op;
    if (want == 0) return (zz_value){ZZ_BOOL, {.b = e != NULL || isdir}};
    if (want == 1) return (zz_value){ZZ_BOOL, {.b = e != NULL && !e->is_dir}};
    return (zz_value){ZZ_BOOL, {.b = isdir}};
}

zz_value zz_fs_exists_at(zz_value fsys, zz_value path, int *err) {
    return zz_vfs_pred_at(fsys, path, err, "exists_at", 0);
}

zz_value zz_fs_is_file_at(zz_value fsys, zz_value path, int *err) {
    return zz_vfs_pred_at(fsys, path, err, "is_file_at", 1);
}

zz_value zz_fs_is_dir_at(zz_value fsys, zz_value path, int *err) {
    return zz_vfs_pred_at(fsys, path, err, "is_dir_at", 2);
}

static int zz_vfs_str_cmp(const void *a, const void *b) {
    return strcmp(*(char *const *)a, *(char *const *)b);
}

zz_value zz_fs_read_dir_at(zz_value fsys, zz_value path, int *err) {
    (void)err;
    struct zz_vfs *v;
    if (!zz_vfs_check(fsys, &v)) return zz_vfs_at_closed("read_dir_at");
    const char *p = zz_fs_cstr(path);
    if (!p || path.tag != ZZ_STR) { *err = 1; return zz_unit(); }
    if (v->kind == 0) return zz_fs_read_dir(path, err);
    char *key = zz_vfs_key(p);
    if (!key) return zz_vfs_code_err("read_dir", p, "io_error");
    // Exact match only (see read_to_string_at): dir-ness of implied
    // (embed) dirs resolves by prefix scan below.
    struct zz_vfs_entry *self = zz_vfs_find(v, key);
    int isdir = self ? self->is_dir : zz_vfs_tree_is_dir(v, key);
    if ((self && !self->is_dir) || (!self && !isdir)) {
        zz_value r = zz_vfs_code_err("read_dir", p,
                                     self ? "io_error" : "not_found");
        free(key);
        return r;
    }
    // Collect immediate children (dedup by linear scan — dir scale).
    char **names = NULL;
    size_t nnames = 0, cap = 0;
    size_t klen = strlen(key);
    for (struct zz_vfs_entry *e = v->entries; e; e = e->next) {
        const char *rel;
        if (strcmp(key, "/") == 0) {
            if (e->path[0] != '/' || e->path[1] == '\0') continue;
            rel = e->path + 1;
        } else {
            if (strncmp(e->path, key, klen) != 0 || e->path[klen] != '/') continue;
            rel = e->path + klen + 1;
        }
        if (*rel == '\0' || strchr(rel, '/')) continue;
        int dup = 0;
        for (size_t k = 0; k < nnames; k++) {
            if (strcmp(names[k], rel) == 0) {
                dup = 1;
                break;
            }
        }
        if (dup) continue;
        if (nnames == cap) {
            size_t ncap = cap ? cap * 2 : 16;
            char **nn = (char **)realloc(names, ncap * sizeof(char *));
            if (!nn) break;
            names = nn;
            cap = ncap;
        }
        names[nnames++] = e->path + (rel - e->path);
    }
    qsort(names, nnames, sizeof(char *), zz_vfs_str_cmp);
    zz_value out = zz_array_new();
    for (size_t k = 0; k < nnames; k++) {
        zz_array_push(out.arr, zz_str_new(names[k], strlen(names[k])));
    }
    free(names);
    free(key);
    return zz_variant_ok(out);
}

zz_value zz_fs_mkdir_all_at(zz_value fsys, zz_value path, int *err) {
    (void)err;
    struct zz_vfs *v;
    if (!zz_vfs_check(fsys, &v)) return zz_vfs_at_closed("mkdir_all_at");
    const char *p = zz_fs_cstr(path);
    if (!p || path.tag != ZZ_STR) { *err = 1; return zz_unit(); }
    if (v->kind == 0) return zz_fs_mkdir_all(path, err);
    if (v->kind != 1) return zz_vfs_readonly_err("mkdir_all", p);
    char *key = zz_vfs_key(p);
    if (!key) return zz_vfs_code_err("mkdir_all", p, "io_error");
    struct zz_vfs_entry *e = zz_vfs_find(v, key);
    zz_value r;
    if (e && !e->is_dir) {
        r = zz_vfs_code_err("mkdir_all", p, "already_exists");
    } else if (zz_vfs_mkdir_parents(v, key) != 0) {
        r = zz_vfs_code_err("mkdir_all", p, "io_error");
    } else if (!e && strcmp(key, "/") != 0) {
        e = (struct zz_vfs_entry *)malloc(sizeof(*e));
        if (!e) {
            r = zz_vfs_code_err("mkdir_all", p, "io_error");
        } else {
            e->path = key;
            key = NULL;
            e->data = NULL;
            e->len = 0;
            e->is_dir = 1;
            e->next = v->entries;
            v->entries = e;
            r = zz_variant_ok(zz_unit());
        }
    } else {
        r = zz_variant_ok(zz_unit());
    }
    free(key);
    return r;
}

zz_value zz_fs_remove_file_at(zz_value fsys, zz_value path, int *err) {
    (void)err;
    struct zz_vfs *v;
    if (!zz_vfs_check(fsys, &v)) return zz_vfs_at_closed("remove_file_at");
    const char *p = zz_fs_cstr(path);
    if (!p || path.tag != ZZ_STR) { *err = 1; return zz_unit(); }
    if (v->kind == 0) return zz_fs_remove(path, err);
    if (v->kind != 1) return zz_vfs_readonly_err("remove_file", p);
    char *key = zz_vfs_key(p);
    if (!key) return zz_vfs_code_err("remove_file", p, "io_error");
    struct zz_vfs_entry **link = &v->entries;
    zz_value r = zz_vfs_code_err("remove_file", p, "not_found");
    while (*link) {
        struct zz_vfs_entry *e = *link;
        if (strcmp(e->path, key) == 0) {
            if (e->is_dir) {
                r = zz_vfs_code_err("remove_file", p, "io_error");
                break;
            }
            *link = e->next;
            free(e->path);
            free(e->data);
            free(e);
            r = zz_variant_ok(zz_unit());
            break;
        }
        link = &e->next;
    }
    free(key);
    return r;
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

// Shared client perform for fetch/post_json: method already validated,
// timeout_ms > 0. `body`/`body_len` may be NULL/0 (no request body).
// Returns a Result of the 5-field response object (see http_response_new).
// Forward: defined with the other verb clients below.
zz_value zz_http_post(zz_value url, zz_value body, zz_value headers, int *err);
static zz_value http_client_perform(const char *method, const char *url, size_t url_len,
        zz_value headers, const unsigned char *body, size_t body_len,
        long timeout_ms, const char *label, int *err) {
    CURL *curl = curl_easy_init();
    if (!curl) { *err = 1; return zz_variant_err(zz_str_static("http client: curl_easy_init failed")); }

    curl_buf body_buf = {0};
    zz_value headers_dict = zz_dict_new();

    char *url_c = (char *)malloc(url_len + 1);
    if (!url_c) {
        curl_easy_cleanup(curl);
        zz_release(&headers_dict);
        *err = 1;
        return zz_variant_err(zz_str_static("http client: out of memory"));
    }
    memcpy(url_c, url, url_len);
    url_c[url_len] = '\0';
    curl_easy_setopt(curl, CURLOPT_URL, url_c);
    // Custom request covers every verb uniformly (GET/POST/PUT/DELETE/PATCH).
    curl_easy_setopt(curl, CURLOPT_CUSTOMREQUEST, method);
    curl_easy_setopt(curl, CURLOPT_WRITEFUNCTION, curl_write_cb);
    curl_easy_setopt(curl, CURLOPT_WRITEDATA, &body_buf);
    curl_easy_setopt(curl, CURLOPT_HEADERFUNCTION, curl_header_cb);
    curl_easy_setopt(curl, CURLOPT_HEADERDATA, &headers_dict);
    curl_easy_setopt(curl, CURLOPT_FOLLOWLOCATION, 1L);
    curl_easy_setopt(curl, CURLOPT_TIMEOUT_MS, timeout_ms);
    curl_easy_setopt(curl, CURLOPT_NOSIGNAL, 1L);

    if (body && body_len > 0) {
        curl_easy_setopt(curl, CURLOPT_POSTFIELDS, (const char *)body);
        curl_easy_setopt(curl, CURLOPT_POSTFIELDSIZE, (long)body_len);
    }

    struct curl_slist *header_list = NULL;
    if (headers.tag == ZZ_DICT && headers.dict && headers.dict->len > 0) {
        for (size_t i = 0; i < headers.dict->len; i++) {
            zz_str *k = headers.dict->entries[i].key;
            zz_value *v = &headers.dict->entries[i].val;
            if (k && v->tag == ZZ_STR) {
                size_t hlen = k->len + 2 + v->s->len;
                char *h = (char *)malloc(hlen + 1);
                if (!h) continue;
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
    free(url_c);
    if (header_list) curl_slist_free_all(header_list);

    if (res != CURLE_OK) {
        char errbuf[256];
        snprintf(errbuf, sizeof errbuf, "%s: %s", label, curl_easy_strerror(res));
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
        if (!body_str) {
            free(body_buf.data);
            zz_release(&headers_dict);
            *err = 1;
            return zz_variant_err(zz_str_static("http client: out of memory"));
        }
        memcpy(zz_str_ptr(body_str), body_buf.data, body_buf.len);
        zz_str_ptr(body_str)[body_buf.len] = '\0';
        free(body_buf.data);
    } else {
        body_str = str_alloc(0);
        if (!body_str) {
            zz_release(&headers_dict);
            *err = 1;
            return zz_variant_err(zz_str_static("http client: out of memory"));
        }
    }

    zz_value resp = http_response_new(http_code, body_str, headers_dict);
    zz_release(&headers_dict);
    return zz_variant_ok(resp);
}

// http.fetch(url, method, headers, body, timeout_ms) → .ok(HttpResponse) or .err(str)
zz_value zz_http_fetch(zz_value url, zz_value method, zz_value headers, zz_value body, zz_value timeout_ms, int *err) {
    *err = 0;
    if (url.tag != ZZ_STR || !url.s) { *err = 1; return zz_variant_err(zz_str_static("http.fetch: url must be a string")); }
    if (method.tag != ZZ_STR || !method.s) { *err = 1; return zz_variant_err(zz_str_static("http.fetch: method must be a string")); }
    const char *m = zz_str_cptr(method.s);
    size_t mlen = method.s->len;
    char up[16];
    const char *verb = NULL;
    if (mlen < sizeof up) {
        for (size_t i = 0; i < mlen; i++) {
            char c = m[i];
            up[i] = (c >= 'a' && c <= 'z') ? (char)(c - 32) : c;
        }
        up[mlen] = '\0';
        if ((mlen == 3 && memcmp(up, "GET", 3) == 0) ||
            (mlen == 4 && memcmp(up, "POST", 4) == 0) ||
            (mlen == 3 && memcmp(up, "PUT", 3) == 0) ||
            (mlen == 6 && memcmp(up, "DELETE", 6) == 0) ||
            (mlen == 5 && memcmp(up, "PATCH", 5) == 0)) {
            verb = up;
        }
    }
    if (!verb) {
        *err = 1;
        return zz_variant_err(zz_str_static("http.fetch: unknown method (expected GET, POST, PUT, DELETE or PATCH)"));
    }
    long toms = (timeout_ms.tag == ZZ_INT && timeout_ms.i > 0) ? (long)timeout_ms.i : -1;
    if (toms < 0) {
        *err = 1;
        return zz_variant_err(zz_str_static("http.fetch: timeout_ms must be a positive int"));
    }
    const unsigned char *bdata = NULL;
    size_t blen = 0;
    if (body.tag == ZZ_STR && body.s) {
        bdata = (const unsigned char *)zz_str_cptr(body.s);
        blen = body.s->len;
    } else if (body.tag == ZZ_BYTES && body.bytes) {
        bdata = body.bytes->buf->data + body.bytes->off;
        blen = body.bytes->len;
    } else if (body.tag != ZZ_STR) {
        *err = 1;
        return zz_variant_err(zz_str_static("http.fetch: body must be a string or bytes"));
    }
    // `verb` aliases the stack buffer `up` — copy it: perform only reads it.
    char verb_copy[16];
    memcpy(verb_copy, verb, mlen + 1);
    return http_client_perform(verb_copy, zz_str_cptr(url.s), url.s->len,
        headers, bdata, blen, toms, "http.fetch", err);
}

// http.post_json(url, body, headers) → .ok(HttpResponse) or .err(str)
// Serializes any value to JSON, defaults Content-Type when absent, POSTs it.
zz_value zz_http_post_json(zz_value url, zz_value body, zz_value headers, int *err) {
    char *text = json_to_cstr(body);
    if (!text) {
        *err = 1;
        return zz_variant_err(zz_str_static("http.post_json: could not serialize body"));
    }
    zz_value hdrs = zz_dict_new();
    if (headers.tag == ZZ_DICT && headers.dict) {
        for (size_t i = 0; i < headers.dict->len; i++) {
            zz_str *k = headers.dict->entries[i].key;
            zz_value v = headers.dict->entries[i].val;
            if (k && v.tag == ZZ_STR) {
                zz_dict_set(hdrs.dict, (zz_value){ZZ_STR, {.s = k}}, zz_clone(v));
            }
        }
    }
    int has_ct = 0;
    if (hdrs.tag == ZZ_DICT && hdrs.dict) {
        for (size_t i = 0; i < hdrs.dict->len; i++) {
            zz_str *k = hdrs.dict->entries[i].key;
            if (k && k->len == 12) {
                char low[12];
                for (size_t j = 0; j < 12; j++) {
                    char c = zz_str_cptr(k)[j];
                    low[j] = (c >= 'A' && c <= 'Z') ? (char)(c + 32) : c;
                }
                if (memcmp(low, "content-type", 12) == 0) { has_ct = 1; break; }
            }
        }
    }
    if (!has_ct) {
        zz_str *ck = str_alloc(12);
        zz_str *cv = str_alloc(16);
        if (!ck || !cv) {
            free(text);
            zz_release(&hdrs);
            *err = 1;
            return zz_variant_err(zz_str_static("http.post_json: out of memory"));
        }
        memcpy(zz_str_ptr(ck), "Content-Type", 12);
        zz_str_ptr(ck)[12] = '\0';
        memcpy(zz_str_ptr(cv), "application/json", 16);
        zz_str_ptr(cv)[16] = '\0';
        zz_dict_set(hdrs.dict, (zz_value){ZZ_STR, {.s = ck}}, (zz_value){ZZ_STR, {.s = cv}});
    }
    zz_str *bs = str_alloc(strlen(text));
    if (!bs) {
        free(text);
        zz_release(&hdrs);
        *err = 1;
        return zz_variant_err(zz_str_static("http.post_json: out of memory"));
    }
    memcpy(zz_str_ptr(bs), text, strlen(text));
    zz_str_ptr(bs)[strlen(text)] = '\0';
    free(text);
    zz_value out = zz_http_post(url, (zz_value){ZZ_STR, {.s = bs}}, hdrs, err);
    zz_release(&hdrs);
    return out;
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

    zz_object_set_field(&resp_val, "json", http_json_field(body_str));

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

    zz_object_set_field(&resp_val, "json", http_json_field(body_str));

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

// http.response.json(response) → parsed body JSON, or .err on parse
// failure. Re-parses the body at call time exactly like the VM's
// http_response_json (the stored "json" field is bare-or-unit).
zz_value zz_http_response_json(zz_value resp, int *err) {
    (void)err;
    zz_value b = zz_object_get_field(&resp, "body");
    zz_value ret = http_parse_body_json(b);
    zz_release(&b);
    return ret;
}

// http.response.headers(response) → dict
zz_value zz_http_response_headers(zz_value resp, int *err) {
    (void)err;
    return zz_object_get_field(&resp, "headers");
}

