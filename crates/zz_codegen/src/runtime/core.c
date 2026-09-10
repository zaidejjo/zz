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
            memcpy(out->data, a.s->data, a.s->len);
            memcpy(out->data + a.s->len, b.s->data, b.s->len);
            zz_value v;
            v.tag = ZZ_STR;
            v.s = out;
            return v;
        }
        int cmp = memcmp(a.s->data, b.s->data,
                         a.s->len < b.s->len ? a.s->len : b.s->len);
        // If common prefix matches, shorter string is "less than"
        if (cmp == 0 && a.s->len != b.s->len) {
            cmp = (a.s->len < b.s->len) ? -1 : 1;
        }
        switch (op) {
        case ZZOP_EQ:
            return zz_bool(a.s->len == b.s->len &&
                           memcmp(a.s->data, b.s->data, a.s->len) == 0);
        case ZZOP_NE:
            return zz_bool(!(a.s->len == b.s->len &&
                             memcmp(a.s->data, b.s->data, a.s->len) == 0));
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
// A closure value is a ZZ_NATIVE whose payload points to a heap slot holding
// a generated `zz_dispatch_fn` pointer. Not refcounted; released as a no-op.
zz_value zz_closure_make(zz_dispatch_fn f) {
    zz_dispatch_fn *slot = (zz_dispatch_fn *)malloc(sizeof(zz_dispatch_fn));
    *slot = f;
    zz_value v;
    v.tag = ZZ_NATIVE;
    v.payload = (zz_value *)slot;
    return v;
}

zz_dispatch_fn zz_closure_target(zz_value v) {
    if (v.tag != ZZ_NATIVE || !v.payload) return NULL;
    return *(zz_dispatch_fn *)(void *)v.payload;
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
        fwrite(prompt.s->data, 1, prompt.s->len, stdout);
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
//  Thread-safe channels (pthread-based)
// =====================================================================

zz_value zz_chan_new(int *err) {
    (void)err;
    zz_chan *ch = (zz_chan *)malloc(sizeof(zz_chan));
    if (!ch) {
        fprintf(stderr, "zz: out of memory (channel)\n");
        exit(1);
    }
    pthread_mutex_init(&ch->lock, NULL);
    pthread_cond_init(&ch->cond, NULL);
    ch->len = 0;
    ch->cap = 16;
    ch->queue = (zz_value *)malloc(sizeof(zz_value) * ch->cap);
    if (!ch->queue) {
        fprintf(stderr, "zz: out of memory (channel buffer)\n");
        exit(1);
    }
    zz_value v;
    v.tag = ZZ_CHAN;
    v.chan = ch;
    return v;
}

zz_value zz_chan_send(zz_value chan, zz_value val, int *err) {
    if (chan.tag != ZZ_CHAN) { *err = 1; return zz_unit(); }
    zz_chan *ch = chan.chan;
    pthread_mutex_lock(&ch->lock);
    // Grow if needed.
    if (ch->len == ch->cap) {
        size_t new_cap = ch->cap * 2;
        zz_value *new_queue = (zz_value *)realloc(ch->queue, sizeof(zz_value) * new_cap);
        if (!new_queue) {
            pthread_mutex_unlock(&ch->lock);
            *err = 1;
            return zz_unit();
        }
        ch->queue = new_queue;
        ch->cap = new_cap;
    }
    ch->queue[ch->len++] = zz_clone(val);
    pthread_cond_signal(&ch->cond);
    pthread_mutex_unlock(&ch->lock);
    return zz_unit();
}

zz_value zz_chan_recv(zz_value chan, int *err) {
    (void)err;
    if (chan.tag != ZZ_CHAN) { *err = 1; return zz_unit(); }
    zz_chan *ch = chan.chan;
    pthread_mutex_lock(&ch->lock);
    while (ch->len == 0) {
        pthread_cond_wait(&ch->cond, &ch->lock);
    }
    zz_value v = ch->queue[0];
    // Shift remaining items left.
    for (size_t i = 0; i < ch->len - 1; i++) {
        ch->queue[i] = ch->queue[i + 1];
    }
    ch->len--;
    pthread_mutex_unlock(&ch->lock);
    return v;
}

zz_value zz_chan_try_recv(zz_value chan, int *err) {
    if (chan.tag != ZZ_CHAN) { *err = 1; return zz_unit(); }
    zz_chan *ch = chan.chan;
    pthread_mutex_lock(&ch->lock);
    if (ch->len == 0) {
        pthread_mutex_unlock(&ch->lock);
        *err = 1;  // No message available.
        return zz_unit();
    }
    zz_value v = ch->queue[0];
    for (size_t i = 0; i < ch->len - 1; i++) {
        ch->queue[i] = ch->queue[i + 1];
    }
    ch->len--;
    pthread_mutex_unlock(&ch->lock);
    *err = 0;
    return v;
}

// =====================================================================
//  Spawn / task join (pthread-based)
// =====================================================================

// Thread trampoline: calls zz_call on the function and stores the result.
typedef struct {
    zz_value fn;
    zz_task_join *join;
} zz_spawn_ctx;

static void *zz_spawn_trampoline(void *arg) {
    zz_spawn_ctx *ctx = (zz_spawn_ctx *)arg;
    zz_task_join *join = ctx->join;
    // Call the function (zero args for now).
    int err = 0;
    zz_value result = zz_call(ctx->fn, NULL, 0, &err);
    // Store result and signal completion.
    pthread_mutex_lock(&join->lock);
    join->result = result;
    join->completed = 1;
    pthread_cond_signal(&join->cond);
    pthread_mutex_unlock(&join->lock);
    // Free the context (fn was cloned into join->result via zz_clone at spawn time).
    free(ctx);
    return NULL;
}

zz_value zz_spawn(zz_value fn, int *err) {
    if (fn.tag != ZZ_FUNC) { *err = 1; return zz_unit(); }
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
    // Create spawn context passed to trampoline.
    zz_spawn_ctx *ctx = (zz_spawn_ctx *)malloc(sizeof(zz_spawn_ctx));
    if (!ctx) {
        fprintf(stderr, "zz: out of memory (spawn context)\n");
        exit(1);
    }
    ctx->fn = zz_clone(fn);  // Keep a ref for the thread.
    ctx->join = join;
    // Create the thread.
    if (pthread_create(&join->thread, NULL, zz_spawn_trampoline, ctx) != 0) {
        free(ctx);
        free(join);
        *err = 1;
        return zz_unit();
    }
    // Detach: thread frees its own resources.
    pthread_detach(join->thread);
    zz_value v;
    v.tag = ZZ_TASK_JOIN;
    v.task = join;
    return v;
}

zz_value zz_task_join_recv(zz_value join_val, int *err) {
    if (join_val.tag != ZZ_TASK_JOIN) { *err = 1; return zz_unit(); }
    zz_task_join *join = join_val.task;
    pthread_mutex_lock(&join->lock);
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

// ---- http AOT server -------------------------------------------------------

// Minimal HTTP server for AOT mode: thread-per-connection, fixed "OK" response.
// Route handlers are not supported in AOT (no interpreter to call closures).

#include <pthread.h>
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
#include <sys/epoll.h>
#include <fcntl.h>
#include <sys/types.h>
#include <sys/wait.h>
#include <sys/syscall.h>
#include <sched.h>
#include <poll.h>

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
    if (tcp_resolve(addr.s->data, &sa) != 0) {
        char buf[192];
        int n = snprintf(buf, sizeof buf, "tcp_listen failed: invalid address `%s`", addr.s->data);
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
    if (tcp_resolve(addr.s->data, &sa) != 0) {
        char buf[192];
        int n = snprintf(buf, sizeof buf, "invalid address: `%s`", addr.s->data);
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
        ssize_t w = send(stream.net->fd, data.s->data + total, data.s->len - total, 0);
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
        g_http_routes[g_http_route_count++] = strndup(path.s->data, path.s->len);
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
int zz_run(void) {
    zz_main();
    int main_err = 0;
    if (zz_call_main())
        main_err = 1;
    return main_err;
}

int main(void) {
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
            int64_t n = strtoll(v.s->data, &end, 10);
            if (end == v.s->data) return (zz_value){ZZ_INT, {.i = 0}};
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
            double n = strtod(v.s->data, &end);
            if (end == v.s->data) return (zz_value){ZZ_FLOAT, {.f = 0.0}};
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
    const char *val = getenv(name.s->data);
    if (!val) return (zz_value){ZZ_OPTION_NONE, {0}};
    return zz_variant_some(zz_str_static(val));
}

// env.var(name) — returns .ok(val) or .err(msg)
zz_value zz_env_var(zz_value name, int *err) {
    (void)err;
    if (name.tag != ZZ_STR) return zz_variant_err(zz_str_static("env.var: expected string name"));
    const char *val = getenv(name.s->data);
    if (!val) {
        // Build error message: "environment variable `NAME` not set"
        size_t nlen = name.s->len;
        const char *prefix = "environment variable `";
        const char *suffix = "` not set";
        size_t total = strlen(prefix) + nlen + strlen(suffix);
        char *msg = (char *)malloc(total + 1);
        memcpy(msg, prefix, strlen(prefix));
        memcpy(msg + strlen(prefix), name.s->data, nlen);
        memcpy(msg + strlen(prefix) + nlen, suffix, strlen(suffix));
        msg[total] = '\0';
        return zz_variant_err(zz_str_owned(msg));
    }
    return zz_variant_ok(zz_str_static(val));
}

// env.args() — returns command-line arguments (excludes argv[0] binary name)
zz_value zz_env_args(zz_value unused, int *err) {
    (void)unused; (void)err;
    // C main() in generated code doesn't receive argc/argv yet.
    // Return an empty array for now.
    return zz_array_new();
}

// dict.len(d)
// fs.read(path)
zz_value zz_fs_read(zz_value path, int *err) {
    if (path.tag != ZZ_STR) { *err = 1; return zz_unit(); }
    FILE *f = fopen(path.s->data, "rb");
    if (!f) {
        // Match the VM: `.err(io error string)`
        return zz_variant_err(zz_str_static("No such file or directory"));
    }
    fseek(f, 0, SEEK_END);
    long sz = ftell(f);
    fseek(f, 0, SEEK_SET);
    if (sz < 0) sz = 0;
    zz_str *out = str_alloc(sz);
    size_t n = fread(out->data, 1, sz, f);
    fclose(f);
    out->data[n] = '\0';
    out->len = n;
    return zz_variant_ok((zz_value){ZZ_STR, {.s = out}});
}

// fs.write(path, data)
zz_value zz_fs_write(zz_value path, zz_value data, int *err) {
    if (path.tag != ZZ_STR || data.tag != ZZ_STR) { *err = 1; return zz_unit(); }
    FILE *f = fopen(path.s->data, "wb");
    if (!f) {
        return zz_variant_err(zz_str_static("cannot open file for write"));
    }
    size_t w = fwrite(data.s->data, 1, data.s->len, f);
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
    FILE *f = fopen(path.s->data, "rb");
    if (!f) return (zz_value){ZZ_BOOL, {.b = false}};
    fclose(f);
    return (zz_value){ZZ_BOOL, {.b = true}};
}

// fs.remove(path)
zz_value zz_fs_remove(zz_value path, int *err) {
    if (path.tag != ZZ_STR) { *err = 1; return zz_unit(); }
    int r = remove(path.s->data);
    if (r != 0) {
        return zz_variant_err(zz_str_static("cannot remove file"));
    }
    return zz_variant_ok(zz_unit());
}

// fs.mkdir(path)
zz_value zz_fs_mkdir(zz_value path, int *err) {
    if (path.tag != ZZ_STR) { *err = 1; return zz_unit(); }
    int r = mkdir(path.s->data, 0755);
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
    const char *src = s.s->data;
    size_t len = s.s->len;
    // Worst case: every byte becomes %XX.
    zz_str *out = str_alloc(len * 3);
    size_t pos = 0;
    for (size_t i = 0; i < len; i++) {
        unsigned char c = (unsigned char)src[i];
        if ((c >= 'A' && c <= 'Z') || (c >= 'a' && c <= 'z') || (c >= '0' && c <= '9') || c == '-' || c == '_' || c == '.' || c == '~') {
            out->data[pos++] = c;
        } else {
            snprintf(out->data + pos, 4, "%%%02X", c);
            pos += 3;
        }
    }
    out->data[pos] = '\0';
    out->len = pos;
    return (zz_value){ZZ_STR, {.s = out}};
}

// encoding.url_decode(s) → Result<str>
zz_value zz_encoding_url_decode(zz_value s, int *err) {
    (void)err;
    if (s.tag != ZZ_STR)
        return zz_variant_err(zz_str_static("URL decode error: expected string"));
    const char *src = s.s->data;
    size_t len = s.s->len;
    zz_str *out = str_alloc(len);
    size_t pos = 0;
    for (size_t i = 0; i < len; i++) {
        if (src[i] == '%' && i + 2 < len) {
            char hex[3] = {src[i+1], src[i+2], '\0'};
            out->data[pos++] = (char)strtol(hex, NULL, 16);
            i += 2;
        } else if (src[i] == '+') {
            out->data[pos++] = ' ';
        } else {
            out->data[pos++] = src[i];
        }
    }
    out->data[pos] = '\0';
    out->len = pos;
    return zz_variant_ok((zz_value){ZZ_STR, {.s = out}});
}

// encoding.base64_encode(s)
zz_value zz_encoding_base64_encode(zz_value s, int *err) {
    (void)err;
    if (s.tag != ZZ_STR) return s;
    static const char tbl[] = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    const unsigned char *src = (const unsigned char *)s.s->data;
    size_t len = s.s->len;
    size_t out_len = 4 * ((len + 2) / 3);
    zz_str *out = str_alloc(out_len);
    size_t j = 0;
    for (size_t i = 0; i < len; i += 3) {
        unsigned int a = src[i];
        unsigned int b = (i+1 < len) ? src[i+1] : 0;
        unsigned int c = (i+2 < len) ? src[i+2] : 0;
        unsigned int triple = (a << 16) | (b << 8) | c;
        out->data[j++] = tbl[(triple >> 18) & 0x3F];
        out->data[j++] = tbl[(triple >> 12) & 0x3F];
        out->data[j++] = (i+1 < len) ? tbl[(triple >> 6) & 0x3F] : '=';
        out->data[j++] = (i+2 < len) ? tbl[triple & 0x3F] : '=';
    }
    out->data[j] = '\0';
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
    const char *src = s.s->data;
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
        if (j < out_len) out->data[j++] = (triple >> 16) & 0xFF;
        if (j < out_len) out->data[j++] = (triple >> 8) & 0xFF;
        if (j < out_len) out->data[j++] = triple & 0xFF;
    }
    out->data[j] = '\0';
    out->len = j;
    return zz_variant_ok((zz_value){ZZ_STR, {.s = out}});
}

// encoding.hex_encode(data) → hex string
zz_value zz_encoding_hex_encode(zz_value s, int *err) {
    (void)err;
    if (s.tag != ZZ_STR) return zz_str_static("");
    const unsigned char *d = (const unsigned char *)s.s->data;
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
        char byte_str[3] = { s.s->data[i], s.s->data[i+1], '\0' };
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
    memcpy(key->data, line, key_len);
    key->data[key_len] = '\0';
    key->len = key_len;

    zz_str *value_str = str_alloc(val_len);
    memcpy(value_str->data, val, val_len);
    value_str->data[val_len] = '\0';
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

    curl_easy_setopt(curl, CURLOPT_URL, (char *)url.s->data);
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
                memcpy(h, k->data, k->len);
                h[k->len] = ':';
                h[k->len + 1] = ' ';
                memcpy(h + k->len + 2, v->s->data, v->s->len);
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

    // Adopt body_buf into a proper zz_str (flexible array requires full struct alloc)
    zz_str *body_str;
    if (body_buf.data && body_buf.len > 0) {
        body_str = (zz_str *)malloc(sizeof(zz_str) + body_buf.cap + 1);
        body_str->refs = 1;
        body_str->interned = 0;
        body_str->cap = body_buf.cap;
        body_str->len = body_buf.len;
        memcpy(body_str->data, body_buf.data, body_buf.len + 1);
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

    curl_easy_setopt(curl, CURLOPT_URL, (char *)url.s->data);
    curl_easy_setopt(curl, CURLOPT_POST, 1L);
    curl_easy_setopt(curl, CURLOPT_WRITEFUNCTION, curl_write_cb);
    curl_easy_setopt(curl, CURLOPT_WRITEDATA, &body_buf);
    curl_easy_setopt(curl, CURLOPT_HEADERFUNCTION, curl_header_cb);
    curl_easy_setopt(curl, CURLOPT_HEADERDATA, &headers_dict);
    curl_easy_setopt(curl, CURLOPT_FOLLOWLOCATION, 1L);
    curl_easy_setopt(curl, CURLOPT_TIMEOUT, 30L);
    curl_easy_setopt(curl, CURLOPT_NOSIGNAL, 1L);

    if (body.tag == ZZ_STR && body.s && body.s->len > 0) {
        curl_easy_setopt(curl, CURLOPT_POSTFIELDS, (char *)body.s->data);
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
                memcpy(h, k->data, k->len);
                h[k->len] = ':';
                h[k->len + 1] = ' ';
                memcpy(h + k->len + 2, v->s->data, v->s->len);
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
        body_str = (zz_str *)malloc(sizeof(zz_str) + body_buf.cap + 1);
        body_str->refs = 1;
        body_str->interned = 0;
        body_str->cap = body_buf.cap;
        body_str->len = body_buf.len;
        memcpy(body_str->data, body_buf.data, body_buf.len + 1);
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

