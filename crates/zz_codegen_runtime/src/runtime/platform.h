// ZZ platform abstraction — OS detection and socket/pthread portability.
//
// Single-backend model: Clang (or `zig cc`) compiles every target, so this
// header only branches on the *target OS* (`_WIN32`, `__APPLE__`,
// `__linux__`), never on the compiler. No MSVC-only branches: clang-cl
// consumes the same code uniformly.
//
// The AOT backend concatenates this header first into the single
// translation unit, so the `#include` directives below are stripped at
// assembly time like the rest of the runtime headers.

#ifndef ZZ_RUNTIME_PLATFORM_H
#define ZZ_RUNTIME_PLATFORM_H

#include <stdint.h>

// ---- OS detection ------------------------------------------------------
#if defined(_WIN32) || defined(_WIN64)
#define ZZ_OS_WINDOWS 1
#elif defined(__APPLE__)
#define ZZ_OS_MACOS 1
#elif defined(__linux__)
#define ZZ_OS_LINUX 1
#else
#define ZZ_OS_UNKNOWN 1
#endif

// ---- inline helper (no compiler-specific branches) ---------------------
#define ZZ_INLINE static inline

// ---- threading shim (POSIX pthread ↔ C11 <threads.h>) -------------------
// The runtime is written against one threading vocabulary (`zz_mtx_*`,
// `zz_cnd_*`, `zz_thrd_*`, `zz_once`, `zz_mono_now`, `zz_sleep_ms`) so the
// same C files compile on POSIX and Windows:
//
//   POSIX   → pthread_mutex_t/cond_t/t, pthread_once, clock_gettime,
//             nanosleep (thin wrappers, zero behavior change)
//   Windows → C11 <threads.h> mtx_t/cnd_t/thrd_t; `once` is an atomic-flag
//             + done-flag spin (C11 has no pthread_once equivalent used
//             here); monotonic time is QueryPerformanceCounter; sleep is
//             Win32 Sleep.
//
// Types are exposed as typedefs so struct members in core.h use them.
#ifdef ZZ_OS_WINDOWS
// winsock2.h MUST come before windows.h (otherwise windows.h pulls the
// legacy winsock v1 and the two clash). The socket layer below re-includes
// it harmlessly.
#include <winsock2.h>
#include <windows.h>
#include <stdatomic.h>

// ---- C11 <threads.h> vs Win32 fallback ----------------------------------
// clang-cl / mingw may not ship a conforming <threads.h> (MSVC never did).
// When ZZ_HAS_C11_THREADS is not set explicitly, probe: GCC/clang define
// __STDC_THREADS__ when <threads.h> is usable. Otherwise fall back to
// CRITICAL_SECTION / CONDITION_VARIABLE / CreateThread, present on all
// Windows versions the runtime targets (Vista+).
#ifndef ZZ_HAS_C11_THREADS
#if defined(__STDC_VERSION__) && __STDC_VERSION__ >= 201112L && defined(__STDC_THREADS__)
#define ZZ_HAS_C11_THREADS 1
#else
#define ZZ_HAS_C11_THREADS 0
#endif
#endif

#if ZZ_HAS_C11_THREADS
#include <threads.h>
// C11 mtx_t has no portable static initializer, but the runtime declares a
// few process-wide mutexes at file scope. Wrap mtx_t with a ready flag and
// lazily initialize on first lock, serialized by a per-TU spinlock. Struct
// members (channels, join handles, executor) are explicitly initialized
// with zz_mtx_init and take the ready fast path.
typedef struct {
    mtx_t      m;
    atomic_int ready;
} zz_mtx_t;
typedef cnd_t  zz_cnd_t;
typedef thrd_t zz_thrd_t;
#define ZZ_MTX_INIT {0} // zero-init; the first zz_mtx_lock initializes it
static atomic_flag zz_mtx_init_flag = ATOMIC_FLAG_INIT;
ZZ_INLINE void zz_mtx_init(zz_mtx_t *m) {
    mtx_init(&m->m, mtx_plain);
    atomic_store_explicit(&m->ready, 1, memory_order_release);
}
ZZ_INLINE void zz_mtx_lock(zz_mtx_t *m) {
    if (!atomic_load_explicit(&m->ready, memory_order_acquire)) {
        while (atomic_flag_test_and_set_explicit(&zz_mtx_init_flag,
                                                 memory_order_acquire)) {
            // spin: one-time, short
        }
        if (!atomic_load_explicit(&m->ready, memory_order_acquire)) {
            mtx_init(&m->m, mtx_plain);
            atomic_store_explicit(&m->ready, 1, memory_order_release);
        }
        atomic_flag_clear_explicit(&zz_mtx_init_flag, memory_order_release);
    }
    mtx_lock(&m->m);
}
ZZ_INLINE void zz_mtx_unlock(zz_mtx_t *m) { mtx_unlock(&m->m); }
ZZ_INLINE void zz_mtx_destroy(zz_mtx_t *m) { mtx_destroy(&m->m); }
ZZ_INLINE void zz_cnd_init(zz_cnd_t *c) { cnd_init(c); }
ZZ_INLINE int  zz_cnd_wait(zz_cnd_t *c, zz_mtx_t *m) {
    return cnd_wait(c, &m->m);
}
ZZ_INLINE void zz_cnd_signal(zz_cnd_t *c) { cnd_signal(c); }
ZZ_INLINE void zz_cnd_broadcast(zz_cnd_t *c) { cnd_broadcast(c); }
ZZ_INLINE void zz_cnd_destroy(zz_cnd_t *c) { cnd_destroy(c); }
ZZ_INLINE int zz_thrd_create(zz_thrd_t *t, int (*fn)(void *), void *arg) {
    return thrd_create(t, (thrd_start_t)fn, arg) == thrd_success ? 0 : -1;
}
ZZ_INLINE void zz_thrd_detach(zz_thrd_t t) { thrd_detach(t); }
ZZ_INLINE void zz_thrd_join(zz_thrd_t t) {
    thrd_join(t, NULL);
}
ZZ_INLINE void zz_yield(void) { thrd_yield(); }
#else  // ZZ_HAS_C11_THREADS == 0 — Win32 CRITICAL_SECTION / CONDITION_VARIABLE fallback
typedef struct {
    CRITICAL_SECTION cs;
    atomic_int       ready;
} zz_mtx_t;
typedef CONDITION_VARIABLE zz_cnd_t;
typedef HANDLE       zz_thrd_t;
#define ZZ_MTX_INIT {0} // zero-init; the first zz_mtx_lock initializes it
static atomic_flag zz_mtx_init_flag = ATOMIC_FLAG_INIT;
ZZ_INLINE void zz_mtx_init(zz_mtx_t *m) {
    InitializeCriticalSection(&m->cs);
    atomic_store_explicit(&m->ready, 1, memory_order_release);
}
ZZ_INLINE void zz_mtx_lock(zz_mtx_t *m) {
    if (!atomic_load_explicit(&m->ready, memory_order_acquire)) {
        while (atomic_flag_test_and_set_explicit(&zz_mtx_init_flag,
                                                 memory_order_acquire)) {
            // spin: one-time, short
        }
        if (!atomic_load_explicit(&m->ready, memory_order_acquire)) {
            InitializeCriticalSection(&m->cs);
            atomic_store_explicit(&m->ready, 1, memory_order_release);
        }
        atomic_flag_clear_explicit(&zz_mtx_init_flag, memory_order_release);
    }
    EnterCriticalSection(&m->cs);
}
ZZ_INLINE void zz_mtx_unlock(zz_mtx_t *m) { LeaveCriticalSection(&m->cs); }
ZZ_INLINE void zz_mtx_destroy(zz_mtx_t *m) { DeleteCriticalSection(&m->cs); }
ZZ_INLINE void zz_cnd_init(zz_cnd_t *c) { InitializeConditionVariable(c); }
ZZ_INLINE int zz_cnd_wait(zz_cnd_t *c, zz_mtx_t *m) {
    SleepConditionVariableCS(c, &m->cs, INFINITE);
    return 0;
}
ZZ_INLINE void zz_cnd_signal(zz_cnd_t *c) { WakeConditionVariable(c); }
ZZ_INLINE void zz_cnd_broadcast(zz_cnd_t *c) { WakeAllConditionVariable(c); }
ZZ_INLINE void zz_cnd_destroy(zz_cnd_t *c) { (void)c; } // no resources to free
ZZ_INLINE int zz_thrd_create(zz_thrd_t *t, int (*fn)(void *), void *arg) {
    *t = CreateThread(NULL, 0, (LPTHREAD_START_ROUTINE)fn, arg, 0, NULL);
    return *t != NULL ? 0 : -1;
}
ZZ_INLINE void zz_thrd_detach(zz_thrd_t t) { CloseHandle(t); }
ZZ_INLINE void zz_thrd_join(zz_thrd_t t) {
    WaitForSingleObject(t, INFINITE);
    CloseHandle(t);
}
ZZ_INLINE void zz_yield(void) { SwitchToThread(); }
#endif // ZZ_HAS_C11_THREADS

// One-time init without pthread_once. The static `zz_once_t` values must
// zero-initialize portably, so use an atomic-flag + done-flag spin (the
// initializer body is short: pool/executor/fs-cache setup).
typedef struct {
    atomic_flag lock;
    atomic_int  done;
} zz_once_t;
#define ZZ_ONCE_INIT { ATOMIC_FLAG_INIT, 0 }
ZZ_INLINE void zz_once(zz_once_t *o, void (*f)(void)) {
    if (atomic_load_explicit(&o->done, memory_order_acquire)) return;
    while (atomic_flag_test_and_set_explicit(&o->lock, memory_order_acquire)) {
        // spin until the initializer releases the flag
    }
    if (!atomic_load_explicit(&o->done, memory_order_acquire)) {
        f();
        atomic_store_explicit(&o->done, 1, memory_order_release);
    }
    atomic_flag_clear_explicit(&o->lock, memory_order_release);
}
// Monotonic milliseconds (arbitrary epoch, matching clock_gettime semantics).
ZZ_INLINE int64_t zz_mono_now(void) {
    static volatile int64_t freq = 0;
    if (freq == 0) {
        LARGE_INTEGER f;
        if (QueryPerformanceFrequency(&f)) {
            // Publish atomically: first writer wins; the value is identical
            // either way, so a lost race is harmless.
            (void)_InterlockedCompareExchange64(&freq, f.QuadPart, 0);
        }
    }
    LARGE_INTEGER cnt;
    QueryPerformanceCounter(&cnt);
    return (int64_t)cnt.QuadPart * 1000 / freq;
}
ZZ_INLINE void zz_sleep_ms(int64_t ms) { if (ms > 0) Sleep((DWORD)ms); }
#define ZZ_MTX_SET(m) zz_mtx_init(m)
#define ZZ_CND_SET(c) zz_cnd_init(c)
#else
#include <pthread.h>
#include <time.h>
typedef pthread_mutex_t zz_mtx_t;
typedef pthread_cond_t  zz_cnd_t;
typedef pthread_t       zz_thrd_t;
#define ZZ_MTX_INIT PTHREAD_MUTEX_INITIALIZER
typedef pthread_once_t zz_once_t;
#define ZZ_ONCE_INIT PTHREAD_ONCE_INIT
#define zz_mtx_init(m) pthread_mutex_init((m), NULL)
#define zz_mtx_lock pthread_mutex_lock
#define zz_mtx_unlock pthread_mutex_unlock
#define zz_mtx_destroy pthread_mutex_destroy
#define zz_cnd_init(c) pthread_cond_init((c), NULL)
#define zz_cnd_wait(c, m) pthread_cond_wait((c), (m))
#define zz_cnd_signal(c) pthread_cond_signal(c)
#define zz_cnd_broadcast(c) pthread_cond_broadcast(c)
#define zz_cnd_destroy(c) pthread_cond_destroy(c)
#define zz_thrd_create(t, fn, arg) \
    pthread_create((t), NULL, (void *(*)(void *))(fn), (arg))
#define zz_thrd_detach(t) pthread_detach(t)
#define zz_thrd_join(t) pthread_join((t), NULL)
#define zz_once(o, f) pthread_once((o), (f))
// POSIX keeps the zero-cost static assignment the runtime already used.
#define ZZ_MTX_SET(m) \
    do { *(m) = (pthread_mutex_t)PTHREAD_MUTEX_INITIALIZER; } while (0)
#define ZZ_CND_SET(c) \
    do { *(c) = (pthread_cond_t)PTHREAD_COND_INITIALIZER; } while (0)
#include <sched.h>
ZZ_INLINE void zz_yield(void) { sched_yield(); }
ZZ_INLINE int64_t zz_mono_now(void) {
    struct timespec ts;
    clock_gettime(CLOCK_MONOTONIC, &ts);
    return (int64_t)ts.tv_sec * 1000 + ts.tv_nsec / 1000000;
}
ZZ_INLINE void zz_sleep_ms(int64_t ms) {
    if (ms > 0) {
        struct timespec ts;
        ts.tv_sec = ms / 1000;
        ts.tv_nsec = (ms % 1000) * 1000000;
        nanosleep(&ts, NULL);
    }
}
#endif

// ---- socket layer ------------------------------------------------------
#ifdef ZZ_OS_WINDOWS
#include <winsock2.h>
#include <ws2tcpip.h>
// Windows sockets are SOCKET handles, not int fds; Winsock needs one-time
// process startup before any socket call.
typedef SOCKET zz_fd_t;
#define ZZ_FD_INVALID INVALID_SOCKET
void zz_net_init(void); // WSAStartup once (implemented in core.c)
#else
typedef int zz_fd_t;
#define ZZ_FD_INVALID (-1)
ZZ_INLINE void zz_net_init(void) {} // no-op on POSIX
#endif

// ---- optional native dependencies --------------------------------------
// `ZZ_HAS_CURL` / `ZZ_HAS_SQLITE3` gate the curl/sqlite3 includes and link
// flags. They default to 1 (linked); `-DZZ_HAS_SQLITE3=0` style overrides
// allow minimal builds without those system libraries.
#ifndef ZZ_HAS_CURL
#define ZZ_HAS_CURL 1
#endif
#ifndef ZZ_HAS_SQLITE3
#define ZZ_HAS_SQLITE3 1
#endif

#endif // ZZ_RUNTIME_PLATFORM_H
