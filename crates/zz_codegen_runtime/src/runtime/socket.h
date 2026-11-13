// ZZ runtime — POSIX socket ↔ Winsock abstraction.
//
// Same socket vocabulary compiles on POSIX and Windows (clang-cl):
//
//   POSIX   → socket/bind/listen/accept/connect/recv/send/read/write/close,
//             fcntl O_NONBLOCK, poll, errno, <sys/socket.h> et al.
//   Windows → Winsock 2 equivalents (closesocket, ioctlsocket FIONBIO,
//             WSAPoll, WSAGetLastError), SOCKET fd stored as int (-1 =
//             invalid, matching the POSIX sentinel).
//
// The runtime already stores fds as `int` (struct zz_tcp.fd,
// Connection.fd). Winsock SOCKET values are small ints in practice and
// INVALID_SOCKET normalizes to -1 when narrowed to int, so the existing
// `fd < 0` / `fd == -1` / `fd >= 0` sentinel logic keeps working without
// changing the struct layouts.
//
#ifndef ZZ_RUNTIME_SOCKET_H
#define ZZ_RUNTIME_SOCKET_H

#include "platform.h"
#include <stdlib.h>
#include <string.h>

#ifdef ZZ_OS_WINDOWS
#include <winsock2.h>
#include <ws2tcpip.h>
#include <windows.h>
// WSAPoll + struct pollfd live in ws2tcpip.h on Windows; socklen_t/ssize_t
// are not POSIX there.
#ifndef socklen_t
typedef int socklen_t;
#endif
#ifndef ssize_t
typedef SSIZE_T ssize_t;
#endif
typedef int zz_socklen_t;
#else
#include <sys/socket.h>
#include <netinet/in.h>
#include <netinet/tcp.h>
#include <arpa/inet.h>
#include <unistd.h>
#include <fcntl.h>
#include <poll.h>
#include <netdb.h>
#include <errno.h>
#include <strings.h>
typedef socklen_t zz_socklen_t;
#endif

// ---- socket error reporting ---------------------------------------------
// errno on POSIX, WSAGetLastError on Windows.
ZZ_INLINE int zz_socket_errno(void) {
#ifdef ZZ_OS_WINDOWS
    return (int)WSAGetLastError();
#else
    return errno;
#endif
}

// strerror(errno) on POSIX; FormatMessage-based on Windows (WSA codes are
// not meaningful to strerror).
ZZ_INLINE const char *zz_socket_strerror(int err) {
#ifdef ZZ_OS_WINDOWS
    static char buf[256];
    DWORD n = FormatMessageA(FORMAT_MESSAGE_FROM_SYSTEM |
                                 FORMAT_MESSAGE_IGNORE_INSERTS,
                             NULL, (DWORD)err,
                             MAKELANGID(LANG_NEUTRAL, SUBLANG_DEFAULT),
                             buf, (DWORD)sizeof buf, NULL);
    if (n > 0) {
        while (n && (buf[n - 1] == '\n' || buf[n - 1] == '\r')) buf[--n] = '\0';
        return buf;
    }
    return "unknown socket error";
#else
    return strerror(err);
#endif
}

// ---- POSIX error-code names mapped to Winsock ---------------------------
#ifdef ZZ_OS_WINDOWS
#define ZZ_EAGAIN      WSAEWOULDBLOCK
#define ZZ_EWOULDBLOCK WSAEWOULDBLOCK
#define ZZ_EINPROGRESS WSAEINPROGRESS
#define ZZ_EINTR       WSAEINTR
#define ZZ_EPIPE       WSAECONNABORTED
#define ZZ_ECONNRESET  WSAECONNRESET
#define ZZ_ENOTCONN    WSAENOTCONN
// Winsock exposes the poll event bits under the POSIX names (POLLIN/POLLOUT
// are macros in ws2tcpip.h); there are no WSAPOLLIN/WSAPOLLOUT aliases.
#define ZZ_POLLIN      POLLIN
#define ZZ_POLLOUT     POLLOUT
#else
#define ZZ_EAGAIN      EAGAIN
#define ZZ_EWOULDBLOCK EWOULDBLOCK
#define ZZ_EINPROGRESS EINPROGRESS
#define ZZ_EINTR       EINTR
#define ZZ_EPIPE       EPIPE
#define ZZ_ECONNRESET  ECONNRESET
#define ZZ_ENOTCONN    ENOTCONN
#define ZZ_POLLIN      POLLIN
#define ZZ_POLLOUT     POLLOUT
#endif

// ---- socket calls -------------------------------------------------------
// All socket openers return int fds (-1 = invalid). Pass int fds through;
// Winsock calls cast back to SOCKET at the boundary.
ZZ_INLINE int zz_socket_open(int domain, int type, int protocol) {
#ifdef ZZ_OS_WINDOWS
    SOCKET s = socket(domain, type, protocol);
    return (s == INVALID_SOCKET) ? -1 : (int)s;
#else
    return socket(domain, type, protocol);
#endif
}
ZZ_INLINE int zz_accept(int fd, struct sockaddr *addr, socklen_t *len) {
#ifdef ZZ_OS_WINDOWS
    SOCKET s = accept((SOCKET)fd, addr, len);
    return (s == INVALID_SOCKET) ? -1 : (int)s;
#else
    return accept(fd, addr, len);
#endif
}
ZZ_INLINE int zz_bind(int fd, const struct sockaddr *addr, socklen_t len) {
    return bind(fd, addr, len);
}
ZZ_INLINE int zz_listen(int fd, int backlog) {
    return listen(fd, backlog);
}
ZZ_INLINE int zz_connect(int fd, const struct sockaddr *addr, socklen_t len) {
    return connect(fd, addr, len);
}
ZZ_INLINE ssize_t zz_sock_recv(int fd, void *buf, size_t len, int flags) {
#ifdef ZZ_OS_WINDOWS
    return (ssize_t)recv((SOCKET)fd, (char *)buf, (int)len, flags);
#else
    return recv(fd, buf, len, flags);
#endif
}
ZZ_INLINE ssize_t zz_sock_send(int fd, const void *buf, size_t len, int flags) {
#ifdef ZZ_OS_WINDOWS
    return (ssize_t)send((SOCKET)fd, (const char *)buf, (int)len, flags);
#else
    return send(fd, buf, len, flags);
#endif
}
ZZ_INLINE ssize_t zz_sock_read(int fd, void *buf, size_t len) {
#ifdef ZZ_OS_WINDOWS
    return (ssize_t)recv((SOCKET)fd, (char *)buf, (int)len, 0);
#else
    return read(fd, buf, len);
#endif
}
ZZ_INLINE ssize_t zz_sock_write(int fd, const void *buf, size_t len) {
#ifdef ZZ_OS_WINDOWS
    return (ssize_t)send((SOCKET)fd, (const char *)buf, (int)len, 0);
#else
    return write(fd, buf, len);
#endif
}
ZZ_INLINE int zz_close_socket(int fd) {
#ifdef ZZ_OS_WINDOWS
    return closesocket((SOCKET)fd);
#else
    return close(fd);
#endif
}
ZZ_INLINE int zz_set_nonblock(int fd) {
#ifdef ZZ_OS_WINDOWS
    u_long flag = 1;
    return ioctlsocket((SOCKET)fd, FIONBIO, &flag);
#else
    int flags = fcntl(fd, F_GETFL, 0);
    if (flags < 0) return -1;
    return fcntl(fd, F_SETFL, flags | O_NONBLOCK);
#endif
}
ZZ_INLINE int zz_set_blocking(int fd) {
#ifdef ZZ_OS_WINDOWS
    u_long flag = 0;
    return ioctlsocket((SOCKET)fd, FIONBIO, &flag);
#else
    int flags = fcntl(fd, F_GETFL, 0);
    if (flags < 0) return -1;
    return fcntl(fd, F_SETFL, flags & ~O_NONBLOCK);
#endif
}
ZZ_INLINE int zz_poll(struct pollfd *pfds, int nfds, int timeout) {
#ifdef ZZ_OS_WINDOWS
    return WSAPoll(pfds, nfds, timeout);
#else
    return poll(pfds, nfds, timeout);
#endif
}
ZZ_INLINE int zz_setsockopt(int fd, int level, int optname, const void *optval,
                           socklen_t optlen) {
#ifdef ZZ_OS_WINDOWS
    return setsockopt((SOCKET)fd, level, optname, (const char *)optval, (int)optlen);
#else
    return setsockopt(fd, level, optname, optval, optlen);
#endif
}

// int-valued getsockopt (SO_ERROR). Winsock wants `char *` for optval.
ZZ_INLINE int zz_getsockopt_int(int fd, int level, int optname, int *out) {
#ifdef ZZ_OS_WINDOWS
    int len = (int)sizeof *out;
    return getsockopt((SOCKET)fd, level, optname, (char *)out, &len);
#else
    socklen_t len = (socklen_t)sizeof *out;
    return getsockopt(fd, level, optname, out, &len);
#endif
}

// ---- string helpers (POSIX vs Win32) -------------------------------------
// strcasecmp/strncasecmp are POSIX; Windows CRT has _stricmp/_strnicmp.
// memmem is a GNU extension; Windows doesn't have it at all.
#ifdef ZZ_OS_WINDOWS
#define zz_strcasecmp(a, b) _stricmp((a), (b))
#define zz_strncasecmp(a, b, n) _strnicmp((a), (b), (n))
#else
#define zz_strcasecmp(a, b) strcasecmp((a), (b))
#define zz_strncasecmp(a, b, n) strncasecmp((a), (b), (n))
#endif

ZZ_INLINE void *zz_memmem(const void *haystack, size_t haystack_len,
                          const void *needle, size_t needle_len) {
    if (needle_len == 0) return (void *)haystack;
    if (haystack_len < needle_len) return NULL;
    const unsigned char *h = (const unsigned char *)haystack;
    const unsigned char *n = (const unsigned char *)needle;
    size_t limit = haystack_len - needle_len;
    for (size_t i = 0; i <= limit; i++) {
        if (memcmp(h + i, n, needle_len) == 0) return (void *)(h + i);
    }
    return NULL;
}

// strndup is POSIX 2008; the Windows CRT has no equivalent. Always provide
// the portable spelling so the runtime has one call site.
ZZ_INLINE char *zz_strndup(const char *s, size_t n) {
    size_t len = 0;
    while (len < n && s[len] != '\0') len++;
    char *out = (char *)malloc(len + 1);
    if (!out) return NULL;
    memcpy(out, s, len);
    out[len] = '\0';
    return out;
}

#endif // ZZ_RUNTIME_SOCKET_H
