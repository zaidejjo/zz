//! `net.tls_connect` — rustls TLS client shared by VM and AOT.
//!
//! One driver serves both engines (PG `pg_conn` precedent):
//! - the VM (`zz_stdlib`) calls the safe API ([`connect`], [`with`]) through
//!   thin adapters returning `Value::Opaque("tls")`;
//! - AOT binaries call the `extern "C"` `zz_tls_*` functions (same `zz_value`
//!   convention as embedded-C natives, invoked via `zz_call_nativeN`).
//!
//! Security contract (plan §3 T1):
//! - verification ON by default (webpki roots), hostname verification ON,
//!   SNI sent, min TLS 1.2 (ring provider, TLS 1.3 + 1.2);
//! - `ca_file` adds one PEM trust root (self-signed test certs);
//! - `insecure` skips verification entirely (explicit opt-in only);
//! - timeouts bound dial + handshake + IO (no unbounded hang);
//! - errors carry a reason class: `handshake_failed | cert_untrusted |
//!   cert_hostname | refused_tls | timeout` (prefixed `tls:<reason>:`).
//!
//! Connections live in the handle pool under tag `"tls"`; across the FFI
//! they are `ZZ_INT` ids. Never panics on hostile input.

use std::io::{Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::cabi::{cvalue_bytes, cvalue_str, cvalue_to_bytes, cvalue_to_string, CValue};
use crate::{alloc, payload, Handle};

/// Pool tag for TLS client connections. Selects the `tls.*` method namespace.
pub const TAG: &str = "tls";
/// Pool tag for TLS listeners (`tls_listener.*`: `accept`, `close`).
pub const LISTENER_TAG: &str = "tls_listener";

/// Transport under a live TLS connection: client or server side.
/// `Read`/`Write` delegate so framing above is side-blind.
pub enum TlsStream {
    Client(rustls::StreamOwned<rustls::ClientConnection, TcpStream>),
    Server(rustls::StreamOwned<rustls::ServerConnection, TcpStream>),
}

impl std::io::Read for TlsStream {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        match self {
            Self::Client(s) => s.read(buf),
            Self::Server(s) => s.read(buf),
        }
    }
}

impl std::io::Write for TlsStream {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        match self {
            Self::Client(s) => s.write(buf),
            Self::Server(s) => s.write(buf),
        }
    }
    fn flush(&mut self) -> std::io::Result<()> {
        match self {
            Self::Client(s) => s.flush(),
            Self::Server(s) => s.flush(),
        }
    }
}

impl TlsStream {
    pub fn sock(&self) -> &TcpStream {
        match self {
            Self::Client(s) => &s.sock,
            Self::Server(s) => &s.sock,
        }
    }
}

/// A live TLS connection (client or accepted server side).
pub struct TlsConn {
    pub stream: TlsStream,
    pub peer: String,
    #[allow(dead_code)]
    pub server_name: String,
}

impl TlsConn {
    pub fn set_read_timeout(&self, d: Option<Duration>) -> std::io::Result<()> {
        self.stream.sock().set_read_timeout(d)
    }
    pub fn set_write_timeout(&self, d: Option<Duration>) -> std::io::Result<()> {
        self.stream.sock().set_write_timeout(d)
    }
    pub fn shutdown(&self) -> std::io::Result<()> {
        self.stream.sock().shutdown(std::net::Shutdown::Both)
    }
}

/// Map a dial/handshake/IO failure into a `tls:<reason>:` message.
fn classify(host: &str, e: &str) -> String {
    let low = e.to_lowercase();
    if low.contains("timed out")
        || low.contains("timeout")
        || low.contains("wouldblock")
        || low.contains("would block")
    {
        return format!("tls:timeout: handshake/IO with `{host}` timed out ({e})");
    }
    if low.contains("notvalidforname")
        || low.contains("not valid for name")
        || low.contains("hostname")
        || low.contains("invalid server name")
        || low.contains("invalid dns")
    {
        return format!("tls:cert_hostname: `{host}` does not match the server certificate ({e})");
    }
    if low.contains("unknownissuer")
        || low.contains("unknown issuer")
        || low.contains("untrusted")
        || low.contains("expired")
        || low.contains("notvalidyet")
        || low.contains("revoked")
        || low.contains("badcertificate")
        || low.contains("certificate")
    {
        return format!("tls:cert_untrusted: server certificate for `{host}` is not trusted ({e})");
    }
    if low.contains("invalidmessage")
        || low.contains("invalid message")
        || low.contains("peer sent")
        || low.contains("alert")
        || low.contains("unexpectedmessage")
        || low.contains("plaintext")
        || low.contains("record layer")
    {
        return format!("tls:refused_tls: peer for `{host}` is not speaking TLS (plaintext on a TLS port?) ({e})");
    }
    if low.contains("refused") || low.contains("unreachable") || low.contains("no addresses") {
        return format!("tls:handshake_failed: cannot reach `{host}` ({e})");
    }
    format!("tls:handshake_failed: TLS handshake with `{host}` failed ({e})")
}

fn base_config(
) -> Result<rustls::ConfigBuilder<rustls::ClientConfig, rustls::WantsVerifier>, String> {
    let provider = rustls::crypto::ring::default_provider();
    rustls::ClientConfig::builder_with_provider(provider.into())
        .with_protocol_versions(&[&rustls::version::TLS13, &rustls::version::TLS12])
        .map_err(|e| format!("tls:handshake_failed: unsupported protocol versions ({e})"))
}

fn verified_config(ca_file: Option<&str>) -> Result<Arc<rustls::ClientConfig>, String> {
    let mut roots = rustls::RootCertStore::empty();
    roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    if let Some(path) = ca_file {
        let file = std::fs::File::open(path)
            .map_err(|e| format!("tls:handshake_failed: CA file `{path}` unreadable ({e})"))?;
        let mut reader = std::io::BufReader::new(file);
        let certs: Vec<_> = rustls_pemfile::certs(&mut reader).collect();
        let mut loaded = 0usize;
        for c in certs {
            match c {
                Ok(der) => {
                    roots.add(der).map_err(|e| {
                        format!("tls:handshake_failed: CA file `{path}` has an invalid cert ({e})")
                    })?;
                    loaded += 1;
                }
                Err(e) => {
                    return Err(format!(
                        "tls:handshake_failed: CA file `{path}` is not valid PEM ({e})"
                    ))
                }
            }
        }
        if loaded == 0 {
            return Err(format!(
                "tls:handshake_failed: CA file `{path}` contains no certificates"
            ));
        }
    }
    let config = base_config()?
        .with_root_certificates(roots)
        .with_no_client_auth();
    Ok(Arc::new(config))
}

/// Verifier that trusts everything (explicit `insecure` opt-in only).
#[derive(Debug)]
struct NoVerifier {
    provider: Arc<rustls::crypto::CryptoProvider>,
}

impl NoVerifier {
    fn new() -> Self {
        Self {
            provider: Arc::new(rustls::crypto::ring::default_provider()),
        }
    }
}

impl rustls::client::danger::ServerCertVerifier for NoVerifier {
    fn verify_server_cert(
        &self,
        _end_entity: &rustls_pki_types::CertificateDer<'_>,
        _intermediates: &[rustls_pki_types::CertificateDer<'_>],
        _server_name: &rustls_pki_types::ServerName<'_>,
        _ocsp_response: &[u8],
        _now: rustls_pki_types::UnixTime,
    ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        Ok(rustls::client::danger::ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &rustls_pki_types::CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &rustls_pki_types::CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        self.provider
            .signature_verification_algorithms
            .supported_schemes()
    }
}

fn insecure_config() -> Result<Arc<rustls::ClientConfig>, String> {
    let config = base_config()?
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(NoVerifier::new()))
        .with_no_client_auth();
    Ok(Arc::new(config))
}

fn dial(addr: &str, timeout: Duration) -> Result<TcpStream, String> {
    // System resolution, every address tried in order (#425, #426).
    crate::dial::dial_multi(addr, timeout)
        .map_err(|e| format!("tls:handshake_failed: tcp connect to `{addr}` failed ({e})"))
}

/// Parse `server_name` as DNS or IP (redis connects by IP all the time;
/// rustls `ServerName::try_from` rejects IP literals, so try `IpAddr` first).
fn parse_server_name(server_name: &str) -> Result<rustls_pki_types::ServerName<'static>, String> {
    if let Ok(ip) = server_name.parse::<std::net::IpAddr>() {
        return Ok(rustls_pki_types::ServerName::IpAddress(
            rustls_pki_types::IpAddr::try_from(ip.to_string().as_str())
                .map_err(|_| format!("tls:cert_hostname: invalid server name `{server_name}`"))?,
        ));
    }
    rustls_pki_types::ServerName::try_from(server_name.to_string())
        .map_err(|_| format!("tls:cert_hostname: invalid server name `{server_name}`"))
}

/// Connect timeout: `0` means "no explicit timeout" — use a bounded 30 s
/// default so dial + handshake can never hang forever (plan §10.3: handshake
/// bounded by connect timeout).
fn connect_timeout(ms: u64) -> Duration {
    if ms == 0 {
        Duration::from_secs(30)
    } else {
        Duration::from_millis(ms)
    }
}

/// Connect + handshake. `ca_file`: extra PEM trust root. `insecure`: skip verification.
pub fn connect(
    addr: &str,
    server_name: &str,
    timeout_ms: u64,
    ca_file: Option<&str>,
    insecure: bool,
) -> Result<Handle, String> {
    if server_name.is_empty() {
        return Err(
            "tls:cert_hostname: empty server name (SNI/hostname verification needs one)"
                .to_string(),
        );
    }
    // ServerName check up front so the error names the problem, not the handshake.
    parse_server_name(server_name)?;
    let timeout = connect_timeout(timeout_ms);
    let config = if insecure {
        insecure_config()?
    } else {
        verified_config(ca_file)?
    };
    let stream = dial(addr, timeout)?;
    // Bound the handshake: socket timeouts apply to the TLS handshake reads/writes.
    let _ = stream.set_read_timeout(Some(timeout));
    let _ = stream.set_write_timeout(Some(timeout));
    let peer = stream
        .peer_addr()
        .map(|a| a.to_string())
        .unwrap_or_default();
    let name = parse_server_name(server_name)?;
    let conn = rustls::ClientConnection::new(config, name)
        .map_err(|e| classify(server_name, &e.to_string()))?;
    let mut tls = rustls::StreamOwned::new(conn, stream);
    tls.flush()
        .map_err(|e| classify(server_name, &e.to_string()))?;
    let handle = alloc(
        TAG,
        Arc::new(Mutex::new(TlsConn {
            stream: TlsStream::Client(tls),
            peer,
            server_name: server_name.to_string(),
        })),
    );
    Ok(handle)
}

/// Run `f` on the connection for `id`. `None` for unknown/dropped ids and
/// for handles owned by another module (tag mismatch).
pub fn with<T>(id: u64, f: impl FnOnce(&mut TlsConn) -> T) -> Option<T> {
    payload::<Mutex<TlsConn>>(id, TAG).map(|m| {
        let mut lock = m.lock().unwrap_or_else(|p| p.into_inner());
        f(&mut lock)
    })
}

/// A TLS listener (TCP accept + server-side handshake per connection).
pub struct TlsListener {
    listener: std::net::TcpListener,
    config: Arc<rustls::ServerConfig>,
}

fn load_server_config(
    cert_path: &str,
    key_path: &str,
) -> Result<Arc<rustls::ServerConfig>, String> {
    let cert_file = std::fs::File::open(cert_path)
        .map_err(|e| format!("tls:handshake_failed: cert `{cert_path}` unreadable ({e})"))?;
    let mut reader = std::io::BufReader::new(cert_file);
    let certs: Vec<rustls_pki_types::CertificateDer<'static>> = rustls_pemfile::certs(&mut reader)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| format!("tls:handshake_failed: cert `{cert_path}` is not valid PEM ({e})"))?;
    if certs.is_empty() {
        return Err(format!(
            "tls:handshake_failed: cert `{cert_path}` contains no certificates"
        ));
    }
    let key_file = std::fs::File::open(key_path)
        .map_err(|e| format!("tls:handshake_failed: key `{key_path}` unreadable ({e})"))?;
    let mut kreader = std::io::BufReader::new(key_file);
    let key = rustls_pemfile::pkcs8_private_keys(&mut kreader)
        .next()
        .transpose()
        .map_err(|e| format!("tls:handshake_failed: key `{key_path}` is not valid PEM ({e})"))?
        .map(rustls_pki_types::PrivateKeyDer::Pkcs8)
        .or_else(|| {
            let f = std::fs::File::open(key_path).ok()?;
            let mut r = std::io::BufReader::new(f);
            let k = rustls_pemfile::rsa_private_keys(&mut r).next()?.ok()?;
            Some(rustls_pki_types::PrivateKeyDer::Pkcs1(k))
        })
        .or_else(|| {
            let f = std::fs::File::open(key_path).ok()?;
            let mut r = std::io::BufReader::new(f);
            let k = rustls_pemfile::ec_private_keys(&mut r).next()?.ok()?;
            Some(rustls_pki_types::PrivateKeyDer::Sec1(k))
        })
        .ok_or_else(|| {
            format!("tls:handshake_failed: key `{key_path}` has no PKCS#8, PKCS#1, or SEC1 key")
        })?;
    let provider = rustls::crypto::ring::default_provider();
    let mut config = rustls::ServerConfig::builder_with_provider(provider.into())
        .with_protocol_versions(&[&rustls::version::TLS13, &rustls::version::TLS12])
        .map_err(|e| format!("tls:handshake_failed: unsupported protocol versions ({e})"))?
        .with_no_client_auth()
        .with_single_cert(certs, key)
        .map_err(|e| format!("tls:handshake_failed: cert/key mismatch ({e})"))?;
    config.alpn_protocols = Vec::new();
    Ok(Arc::new(config))
}

/// Bind a TLS listener. `SO_REUSEADDR` set so watch-restarts rebind.
pub fn listen(addr: &str, cert_path: &str, key_path: &str) -> Result<Handle, String> {
    let config = load_server_config(cert_path, key_path)?;
    let socket_addr: std::net::SocketAddr = addr
        .to_socket_addrs()
        .map_err(|e| format!("tls:handshake_failed: invalid listen address `{addr}` ({e})"))?
        .next()
        .ok_or_else(|| format!("tls:handshake_failed: no addresses for `{addr}`"))?;
    let domain = if socket_addr.is_ipv4() {
        socket2::Socket::new(socket2::Domain::IPV4, socket2::Type::STREAM, None)
            .map_err(|e| format!("tls:io: socket failed ({e})"))?
    } else {
        socket2::Socket::new(socket2::Domain::IPV6, socket2::Type::STREAM, None)
            .map_err(|e| format!("tls:io: socket failed ({e})"))?
    };
    domain
        .set_reuse_address(true)
        .map_err(|e| format!("tls:io: socket option failed ({e})"))?;
    domain
        .bind(&socket_addr.into())
        .map_err(|e| format!("tls:io: bind `{addr}` failed ({e})"))?;
    domain
        .listen(128)
        .map_err(|e| format!("tls:io: listen failed ({e})"))?;
    let listener: std::net::TcpListener = domain.into();
    Ok(alloc(
        LISTENER_TAG,
        Arc::new(Mutex::new(TlsListener { listener, config })),
    ))
}

/// Accept one connection + complete the server-side handshake.
pub fn accept(id: u64) -> Result<Handle, String> {
    // Clone what the blocking accept needs under one lock acquisition
    // (no TOCTOU if the listener is closed concurrently), then drop the
    // guard before blocking in `accept`.
    let (listener, config) = payload::<Mutex<TlsListener>>(id, LISTENER_TAG)
        .map(|m| {
            let guard = m.lock().unwrap_or_else(|p| p.into_inner());
            (
                guard
                    .listener
                    .try_clone()
                    .map_err(|e| format!("tls:io: accept failed ({e})")),
                guard.config.clone(),
            )
        })
        .ok_or_else(|| "tls:io: not a tls listener".to_string())?;
    let listener = listener?;
    let (sock, peer) = listener
        .accept()
        .map(|(s, a)| (s, a.to_string()))
        .map_err(|e| format!("tls:io: accept failed ({e})"))?;
    // Bound the server-side handshake like the client side (no unbounded hang).
    let _ = sock.set_read_timeout(Some(Duration::from_secs(10)));
    let _ = sock.set_write_timeout(Some(Duration::from_secs(10)));
    let conn = rustls::ServerConnection::new(config)
        .map_err(|e| format!("tls:handshake_failed: cannot start server handshake ({e})"))?;
    let mut tls = rustls::StreamOwned::new(conn, sock);
    tls.flush().map_err(|e| classify("peer", &e.to_string()))?;
    Ok(alloc(
        TAG,
        Arc::new(Mutex::new(TlsConn {
            stream: TlsStream::Server(tls),
            peer,
            server_name: String::new(),
        })),
    ))
}

/// The pool id for a VM handle (tag-checked).
pub fn id_of(handle: &Handle) -> Option<u64> {
    (handle.tag == TAG).then_some(handle.id)
}

fn set_err(err: *mut std::ffi::c_int) {
    if !err.is_null() {
        unsafe {
            *err = 1;
        }
    }
}

fn ok_str(s: &[u8]) -> CValue {
    unsafe { crate::cabi::zz_variant_ok(cvalue_str(s)) }
}

fn err_str(msg: String) -> CValue {
    unsafe { crate::cabi::zz_variant_err(cvalue_str(msg.as_bytes())) }
}

fn read_timeout_err(host: &str) -> String {
    format!("tls:timeout: read from `{host}` timed out")
}

/// `net.tls_connect(addr, server_name, timeout_ms) -> Result<int, str>` (AOT: handle id).
#[no_mangle]
pub extern "C" fn zz_tls_connect(
    addr: CValue,
    name: CValue,
    timeout: CValue,
    err: *mut std::ffi::c_int,
) -> CValue {
    let (Some(a), Some(n)) = (cvalue_to_string(addr), cvalue_to_string(name)) else {
        set_err(err);
        return err_str(
            "tls:handshake_failed: expected (addr: str, server_name: str, timeout_ms: int)"
                .to_string(),
        );
    };
    let ms = timeout.as_i64().unwrap_or(5000).max(0) as u64;
    match connect(&a, &n, ms, None, false) {
        Ok(h) => unsafe { crate::cabi::zz_variant_ok(CValue::int(h.id as i64)) },
        Err(msg) => err_str(msg),
    }
}

/// `net.tls_connect_with_ca(addr, server_name, ca_file, timeout_ms)`.
#[no_mangle]
pub extern "C" fn zz_tls_connect_with_ca(
    addr: CValue,
    name: CValue,
    ca: CValue,
    timeout: CValue,
    err: *mut std::ffi::c_int,
) -> CValue {
    let (Some(a), Some(n), Some(c)) = (
        cvalue_to_string(addr),
        cvalue_to_string(name),
        cvalue_to_string(ca),
    ) else {
        set_err(err);
        return err_str(
            "tls:handshake_failed: expected (addr: str, server_name: str, ca_file: str, timeout_ms: int)"
                .to_string(),
        );
    };
    let ms = timeout.as_i64().unwrap_or(5000).max(0) as u64;
    match connect(&a, &n, ms, Some(c.as_str()), false) {
        Ok(h) => unsafe { crate::cabi::zz_variant_ok(CValue::int(h.id as i64)) },
        Err(msg) => err_str(msg),
    }
}

/// `net.tls_connect_insecure(addr, server_name, timeout_ms)` — verification OFF.
#[no_mangle]
pub extern "C" fn zz_tls_connect_insecure(
    addr: CValue,
    name: CValue,
    timeout: CValue,
    err: *mut std::ffi::c_int,
) -> CValue {
    let (Some(a), Some(n)) = (cvalue_to_string(addr), cvalue_to_string(name)) else {
        set_err(err);
        return err_str(
            "tls:handshake_failed: expected (addr: str, server_name: str, timeout_ms: int)"
                .to_string(),
        );
    };
    let ms = timeout.as_i64().unwrap_or(5000).max(0) as u64;
    match connect(&a, &n, ms, None, true) {
        Ok(h) => unsafe { crate::cabi::zz_variant_ok(CValue::int(h.id as i64)) },
        Err(msg) => err_str(msg),
    }
}

/// `tls.write(id, data: str) -> Result<int, str>`.
#[no_mangle]
pub extern "C" fn zz_tls_write(id: CValue, data: CValue, err: *mut std::ffi::c_int) -> CValue {
    let (Some(n), Some(text)) = (id.as_i64(), cvalue_to_string(data)) else {
        set_err(err);
        return err_str("tls:io: expected (tls, data: str)".to_string());
    };
    match with(n as u64, |c| c.stream.write_all(text.as_bytes())) {
        Some(Ok(())) => unsafe { crate::cabi::zz_variant_ok(CValue::int(text.len() as i64)) },
        Some(Err(e)) => err_str(classify("peer", &e.to_string())),
        None => {
            set_err(err);
            err_str("tls:io: not a tls connection".to_string())
        }
    }
}

/// `tls.write_bytes(id, data: bytes) -> Result<int, str>`.
#[no_mangle]
pub extern "C" fn zz_tls_write_bytes(
    id: CValue,
    data: CValue,
    err: *mut std::ffi::c_int,
) -> CValue {
    let (Some(n), Some(bytes)) = (id.as_i64(), cvalue_to_bytes(data)) else {
        set_err(err);
        return err_str("tls:io: expected (tls, data: bytes)".to_string());
    };
    let len = bytes.len();
    match with(n as u64, |c| c.stream.write_all(&bytes)) {
        Some(Ok(())) => unsafe { crate::cabi::zz_variant_ok(CValue::int(len as i64)) },
        Some(Err(e)) => err_str(classify("peer", &e.to_string())),
        None => {
            set_err(err);
            err_str("tls:io: not a tls connection".to_string())
        }
    }
}

/// `tls.read(id, max_bytes) -> Result<str, str>` (single read; lossy on binary by construction).
#[no_mangle]
pub extern "C" fn zz_tls_read(id: CValue, max: CValue, err: *mut std::ffi::c_int) -> CValue {
    let (Some(n), Some(cap)) = (id.as_i64(), max.as_i64()) else {
        set_err(err);
        return err_str("tls:io: expected (tls, max_bytes: int)".to_string());
    };
    let cap = cap.clamp(1, 8 * 1024 * 1024) as usize;
    let mut buf = vec![0u8; cap];
    match with(n as u64, |c| c.stream.read(&mut buf)) {
        Some(Ok(0)) => err_str("tls:io: connection closed".to_string()),
        Some(Ok(got)) => {
            buf.truncate(got);
            let s = String::from_utf8_lossy(&buf).into_owned();
            ok_str(s.as_bytes())
        }
        Some(Err(e)) => err_str(classify("peer", &e.to_string())),
        None => {
            set_err(err);
            err_str("tls:io: not a tls connection".to_string())
        }
    }
}

/// `tls.read_bytes(id, max_bytes) -> Result<bytes, str>` (single read, binary-safe).
#[no_mangle]
pub extern "C" fn zz_tls_read_bytes(id: CValue, max: CValue, err: *mut std::ffi::c_int) -> CValue {
    let (Some(n), Some(cap)) = (id.as_i64(), max.as_i64()) else {
        set_err(err);
        return err_str("tls:io: expected (tls, max_bytes: int)".to_string());
    };
    let cap = cap.clamp(1, 8 * 1024 * 1024) as usize;
    let mut buf = vec![0u8; cap];
    match with(n as u64, |c| c.stream.read(&mut buf)) {
        Some(Ok(0)) => err_str("tls:io: connection closed".to_string()),
        Some(Ok(got)) => {
            buf.truncate(got);
            unsafe { crate::cabi::zz_variant_ok(cvalue_bytes(&buf)) }
        }
        Some(Err(e)) => err_str(classify("peer", &e.to_string())),
        None => {
            set_err(err);
            err_str("tls:io: not a tls connection".to_string())
        }
    }
}

/// `tls.read_exact(id, n) -> Result<bytes, str>` (loops until `n` bytes).
#[no_mangle]
pub extern "C" fn zz_tls_read_exact(id: CValue, want: CValue, err: *mut std::ffi::c_int) -> CValue {
    let (Some(n), Some(k)) = (id.as_i64(), want.as_i64()) else {
        set_err(err);
        return err_str("tls:io: expected (tls, n: int)".to_string());
    };
    if k <= 0 || k > 64 * 1024 * 1024 {
        set_err(err);
        return err_str(format!("tls:io: bad read length {k} (want 1..67108864)"));
    }
    let k = k as usize;
    let mut out = vec![0u8; k];
    let res = with(n as u64, |c| c.stream.read_exact(&mut out));
    match res {
        Some(Ok(())) => unsafe { crate::cabi::zz_variant_ok(cvalue_bytes(&out)) },
        Some(Err(e)) => err_str(classify("peer", &e.to_string())),
        None => {
            set_err(err);
            err_str("tls:io: not a tls connection".to_string())
        }
    }
}

/// `tls.read_line(id) -> Result<str, str>` (until `\n`, exclusive).
#[no_mangle]
pub extern "C" fn zz_tls_read_line(id: CValue, err: *mut std::ffi::c_int) -> CValue {
    let Some(n) = id.as_i64() else {
        set_err(err);
        return err_str("tls:io: expected (tls)".to_string());
    };
    let mut line: Vec<u8> = Vec::new();
    let mut one = [0u8; 1];
    loop {
        let r = with(n as u64, |c| c.stream.read_exact(&mut one));
        match r {
            Some(Ok(())) => {
                if one[0] == b'\n' {
                    break;
                }
                line.push(one[0]);
                if line.len() > 64 * 1024 {
                    return err_str("tls:io: line too long (max 64 KiB)".to_string());
                }
            }
            Some(Err(e)) => {
                if line.is_empty() {
                    return err_str(classify("peer", &e.to_string()));
                }
                break;
            }
            None => {
                set_err(err);
                return err_str("tls:io: not a tls connection".to_string());
            }
        }
    }
    if !line.is_empty() && line[line.len() - 1] == b'\r' {
        line.pop();
    }
    let s = String::from_utf8_lossy(&line).into_owned();
    ok_str(s.as_bytes())
}

/// `tls.set_read_timeout(id, ms) / tls.set_write_timeout(id, ms)`.
#[no_mangle]
pub extern "C" fn zz_tls_set_read_timeout(
    id: CValue,
    ms: CValue,
    err: *mut std::ffi::c_int,
) -> CValue {
    let (Some(n), Some(v)) = (id.as_i64(), ms.as_i64()) else {
        set_err(err);
        return err_str("tls:io: expected (tls, ms: int)".to_string());
    };
    let dur = if v <= 0 {
        None
    } else {
        Some(Duration::from_millis(v as u64))
    };
    // Read current write timeout so we don't clobber it: rustls has no
    // getter, so track via socket calls (write timeout preserved by only
    // setting the read side through the OS handle).
    match with(n as u64, |c| c.stream.sock().set_read_timeout(dur)) {
        Some(Ok(())) => unsafe { crate::cabi::zz_variant_ok(CValue::boolean(true)) },
        Some(Err(e)) => err_str(format!("tls:io: set_read_timeout failed ({e})")),
        None => {
            set_err(err);
            err_str("tls:io: not a tls connection".to_string())
        }
    }
}

/// `tls.set_write_timeout(id, ms)`.
#[no_mangle]
pub extern "C" fn zz_tls_set_write_timeout(
    id: CValue,
    ms: CValue,
    err: *mut std::ffi::c_int,
) -> CValue {
    let (Some(n), Some(v)) = (id.as_i64(), ms.as_i64()) else {
        set_err(err);
        return err_str("tls:io: expected (tls, ms: int)".to_string());
    };
    let dur = if v <= 0 {
        None
    } else {
        Some(Duration::from_millis(v as u64))
    };
    match with(n as u64, |c| c.stream.sock().set_write_timeout(dur)) {
        Some(Ok(())) => unsafe { crate::cabi::zz_variant_ok(CValue::boolean(true)) },
        Some(Err(e)) => err_str(format!("tls:io: set_write_timeout failed ({e})")),
        None => {
            set_err(err);
            err_str("tls:io: not a tls connection".to_string())
        }
    }
}

/// `tls.close(id) / tls.shutdown(id)` — shutdown + drop the handle.
#[no_mangle]
pub extern "C" fn zz_tls_close(id: CValue, err: *mut std::ffi::c_int) -> CValue {
    let Some(n) = id.as_i64() else {
        set_err(err);
        return err_str("tls:io: expected (tls)".to_string());
    };
    with(n as u64, |c| {
        let _ = c.stream.sock().shutdown(std::net::Shutdown::Both);
    });
    crate::drop_handle(n as u64);
    unsafe { crate::cabi::zz_variant_ok(CValue::unit()) }
}

/// `tls.peer_addr(id) -> Result<str, str>`.
#[no_mangle]
pub extern "C" fn zz_tls_peer_addr(id: CValue, err: *mut std::ffi::c_int) -> CValue {
    let Some(n) = id.as_i64() else {
        set_err(err);
        return err_str("tls:io: expected (tls)".to_string());
    };
    match with(n as u64, |c| c.peer.clone()) {
        Some(p) => ok_str(p.as_bytes()),
        None => {
            set_err(err);
            err_str("tls:io: not a tls connection".to_string())
        }
    }
}

#[allow(dead_code)]
fn unused_timeout_helper(host: &str) -> String {
    read_timeout_err(host)
}

/// `net.tls_listen(addr, cert, key) -> Result<int, str>` (AOT: listener id).
#[no_mangle]
pub extern "C" fn zz_tls_listen(
    addr: CValue,
    cert: CValue,
    key: CValue,
    err: *mut std::ffi::c_int,
) -> CValue {
    let (Some(a), Some(c), Some(k)) = (
        cvalue_to_string(addr),
        cvalue_to_string(cert),
        cvalue_to_string(key),
    ) else {
        set_err(err);
        return err_str(
            "tls:handshake_failed: expected (addr: str, cert_file: str, key_file: str)".to_string(),
        );
    };
    match listen(&a, &c, &k) {
        Ok(h) => unsafe { crate::cabi::zz_variant_ok(CValue::int(h.id as i64)) },
        Err(msg) => err_str(msg),
    }
}

/// `tls_listener.accept(id) -> Result<int, str>`.
#[no_mangle]
pub extern "C" fn zz_tls_accept(id: CValue, err: *mut std::ffi::c_int) -> CValue {
    let Some(n) = id.as_i64() else {
        set_err(err);
        return err_str("tls:io: expected (tls_listener)".to_string());
    };
    match accept(n as u64) {
        Ok(h) => unsafe { crate::cabi::zz_variant_ok(CValue::int(h.id as i64)) },
        Err(msg) => err_str(msg),
    }
}

/// `tls_listener.close(id)`.
#[no_mangle]
pub extern "C" fn zz_tls_listener_close(id: CValue, err: *mut std::ffi::c_int) -> CValue {
    let Some(n) = id.as_i64() else {
        set_err(err);
        return err_str("tls:io: expected (tls_listener)".to_string());
    };
    crate::drop_handle(n as u64);
    unsafe { crate::cabi::zz_variant_ok(CValue::boolean(true)) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;

    fn rcgen_pair() -> (String, String) {
        let key = rcgen::generate_simple_self_signed(vec!["localhost".to_string()]).unwrap();
        let dir = std::env::temp_dir();
        let cert_path = dir.join(format!("zz_tls_test_cert_{}.pem", std::process::id()));
        let key_path = dir.join(format!("zz_tls_test_key_{}.pem", std::process::id()));
        std::fs::write(&cert_path, key.cert.pem()).unwrap();
        std::fs::write(&key_path, key.key_pair.serialize_pem()).unwrap();
        (
            cert_path.to_string_lossy().into_owned(),
            key_path.to_string_lossy().into_owned(),
        )
    }

    #[test]
    fn classify_names_reasons() {
        assert!(classify("h", "certificate unknown issuer").starts_with("tls:cert_untrusted:"));
        assert!(classify("h", "peer not valid for name").starts_with("tls:cert_hostname:"));
        assert!(classify("h", "timed out").starts_with("tls:timeout:"));
        assert!(classify("h", "invalid message").starts_with("tls:refused_tls:"));
        assert!(classify("h", "boom").starts_with("tls:handshake_failed:"));
    }

    #[test]
    fn rejects_empty_server_name() {
        let r = connect("127.0.0.1:1", "", 200, None, false);
        assert!(r.unwrap_err().starts_with("tls:cert_hostname:"));
    }

    #[test]
    fn full_handshake_against_loopback() {
        let (cert_path, key_path) = rcgen_pair();
        // Build a real loopback TLS server in this thread (blocks).
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let cert_file = std::fs::File::open(&cert_path).unwrap();
            let mut reader = std::io::BufReader::new(cert_file);
            let certs: Vec<_> = rustls_pemfile::certs(&mut reader)
                .collect::<Result<Vec<_>, _>>()
                .unwrap();
            let key_file = std::fs::File::open(&key_path).unwrap();
            let mut kreader = std::io::BufReader::new(key_file);
            let pkcs8: Vec<_> = rustls_pemfile::pkcs8_private_keys(&mut kreader)
                .collect::<Result<Vec<_>, _>>()
                .unwrap();
            let provider = rustls::crypto::ring::default_provider();
            let cfg = rustls::ServerConfig::builder_with_provider(provider.into())
                .with_protocol_versions(&[&rustls::version::TLS13, &rustls::version::TLS12])
                .unwrap()
                .with_no_client_auth()
                .with_single_cert(
                    certs,
                    rustls_pki_types::PrivateKeyDer::Pkcs8(pkcs8[0].clone_key()),
                )
                .unwrap();
            let cfg = Arc::new(cfg);
            tx.send(()).unwrap();
            // Serve up to 3 handshakes: valid-CA echo, untrusted (client
            // aborts server-side), insecure echo. Handshake failures are
            // expected — never panic the server thread.
            for _ in 0..3 {
                let Ok((sock, _)) = listener.accept() else {
                    break;
                };
                let _ = sock.set_read_timeout(Some(std::time::Duration::from_secs(5)));
                let conn = rustls::ServerConnection::new(cfg.clone()).unwrap();
                let mut tls = rustls::StreamOwned::new(conn, sock);
                // Complete the handshake; a client that rejects our cert
                // fails here — that is the test passing, not a server bug.
                let mut buf = [0u8; 5];
                if tls.read_exact(&mut buf).is_err() {
                    continue;
                }
                // Only the "hello" probe gets the "world" reply; anything
                // else is echoed back so insecure round-trips stay testable.
                if &buf == b"hello" {
                    let _ = tls.write_all(b"world");
                } else {
                    let _ = tls.write_all(&buf);
                }
                let _ = tls.flush();
            }
        });
        rx.recv().unwrap();
        // Trust the rcgen cert via ca_file.
        let dir = std::env::temp_dir();
        let ca = dir.join(format!("zz_tls_test_cert_{}.pem", std::process::id()));
        let h = connect(
            &addr.to_string(),
            "localhost",
            5000,
            Some(ca.to_string_lossy().as_ref()),
            false,
        )
        .expect("handshake with valid CA must succeed");
        let id = h.id;
        with(id, |c| c.stream.write_all(b"hello")).unwrap().unwrap();
        with(id, |c| {
            let mut buf = [0u8; 5];
            c.stream.read_exact(&mut buf).unwrap();
            assert_eq!(&buf, b"world");
        });
        // Without the CA it must fail closed (untrusted, not plaintext fallback).
        let err = connect(&addr.to_string(), "localhost", 3000, None, false).unwrap_err();
        assert!(
            err.starts_with("tls:cert_untrusted:") || err.starts_with("tls:handshake_failed:"),
            "{err}"
        );
        // insecure=true connects regardless of trust (still TLS, hostname
        // still parsed but not verified against roots).
        let h2 = connect(&addr.to_string(), "localhost", 5000, None, true)
            .expect("insecure must connect");
        let id2 = h2.id;
        with(id2, |c| c.stream.write_all(b"hello"))
            .unwrap()
            .unwrap();
        with(id2, |c| {
            let mut buf = [0u8; 5];
            c.stream.read_exact(&mut buf).unwrap();
            assert_eq!(&buf, b"world");
        });
        crate::drop_handle(id);
        crate::drop_handle(id2);
    }
}
