//! Hostname-aware TCP dial with per-address fallback (#425, #426).
//!
//! All three dial sites (VM `std.net.tcp_connect`, VM/AOT TLS via
//! `tls_conn::dial`, AOT `zz_tcp_connect` has its own C twin) must agree:
//! resolve `"host:port"` with system resolution and try every returned
//! address in order. First success wins; otherwise the last error is
//! reported. This fixes both hostnames-never-resolve (#426) and
//! `localhost` hitting IPv6-first `::1` while the listener is IPv4 (#425).

use std::net::{TcpStream, ToSocketAddrs};
use std::time::Duration;

/// Connect to `"host:port"`, trying each resolved address in turn.
///
/// Error strings keep the historical `tcp_connect` shapes (`invalid
/// address: …`, `tcp_connect failed: …`, `no addresses found for …`) so
/// callers can wrap them with their own prefixes.
pub fn dial_multi(addr: &str, timeout: Duration) -> Result<TcpStream, String> {
    let resolved = addr
        .to_socket_addrs()
        .map_err(|e| format!("invalid address: {e}"))?;
    let mut last_err = format!("no addresses found for `{addr}`");
    let mut any = false;
    for socket_addr in resolved {
        any = true;
        match TcpStream::connect_timeout(&socket_addr, timeout) {
            Ok(stream) => return Ok(stream),
            Err(e) => last_err = format!("tcp_connect failed: {e}"),
        }
    }
    if !any {
        return Err(format!("no addresses found for `{addr}`"));
    }
    Err(last_err)
}
