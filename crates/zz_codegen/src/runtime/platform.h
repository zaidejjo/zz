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
