//! Blocking MySQL connection over the client/server protocol.
//!
//! Design notes (matches the `net` module pattern and the PG driver):
//! - Plain blocking `TcpStream` with a connect timeout and generous
//!   read/write timeouts. No reactor exists in the ZZ runtime; blocking
//!   natives are safe inside `task.spawn` workers (one OS thread each).
//! - Authentication: `mysql_native_password` and `caching_sha2_password`
//!   (fast path + clear-text full path; RSA-encrypted full auth is
//!   refused with a clear error since this driver is dependency-free).
//!   `sha256_password` degrades to the same clear-text full path.
//! - Statements always run through the binary protocol
//!   (`COM_STMT_PREPARE` / `COM_STMT_EXECUTE`); `{expr}` arrives as `?`
//!   placeholders with typed binary values — never string-concatenated.

use std::io::{Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::time::Duration;

use super::mysql_wire::{
    build_execute, build_handshake_response, build_prepare, build_quit, caching_token, frame,
    is_eof, native_token, parse_auth_switch, parse_binary_row, parse_coldef, parse_err,
    parse_handshake, parse_ok, parse_prepare_ok, read_lenenc, ColDef, MyParam,
};

/// Raw prepared-execute result: columns, raw rows, affected count.
pub type RawResult = (Vec<ColDef>, Vec<Vec<Option<Vec<u8>>>>, u64);

/// Raw query result: column definitions plus raw rows.
pub type RawRows = (Vec<ColDef>, Vec<Vec<Option<Vec<u8>>>>);

/// Read/write timeout once connected.
const IO_TIMEOUT: Duration = Duration::from_secs(30);

/// Connection parameters. Accepts URL form
/// (`mysql://user:pass@host:port/dbname?connect_timeout=5`) and keyword
/// form (`host=.. port=.. dbname=.. user=.. password=..`).
#[derive(Debug, Clone)]
pub struct MyConnInfo {
    pub host: String,
    pub port: u16,
    pub user: String,
    pub password: String,
    pub dbname: String,
    pub connect_timeout: Duration,
}

impl MyConnInfo {
    pub fn parse(s: &str) -> Result<Self, String> {
        let mut info = Self {
            host: "127.0.0.1".to_string(),
            port: 3306,
            user: "root".to_string(),
            password: String::new(),
            dbname: String::new(),
            connect_timeout: Duration::from_secs(10),
        };
        let s = s.trim();
        if s.starts_with("mysql://") {
            info.parse_url(s)?;
        } else {
            info.parse_kv(s)?;
        }
        Ok(info)
    }

    fn parse_url(&mut self, s: &str) -> Result<(), String> {
        let rest = s
            .split_once("://")
            .map(|(_, r)| r)
            .ok_or_else(|| "invalid mysql URL".to_string())?;
        let (authority_path, query) = match rest.split_once('?') {
            Some((a, q)) => (a, Some(q)),
            None => (rest, None),
        };
        let (userinfo, hostpath) = match authority_path.rfind('@') {
            Some(i) => (Some(&authority_path[..i]), &authority_path[i + 1..]),
            None => (None, authority_path),
        };
        if let Some(ui) = userinfo {
            let (user, password) = match ui.split_once(':') {
                Some((u, p)) => (u, Some(p)),
                None => (ui, None),
            };
            self.user = percent_decode(user);
            if let Some(p) = password {
                self.password = percent_decode(p);
            }
        }
        let (hostport, dbname) = match hostpath.split_once('/') {
            Some((h, d)) => (h, Some(d)),
            None => (hostpath, None),
        };
        if !hostport.is_empty() {
            if let Some(stripped) = hostport.strip_prefix('[') {
                let end = stripped
                    .find(']')
                    .ok_or_else(|| "invalid IPv6 host in mysql URL".to_string())?;
                self.host = stripped[..end].to_string();
                if let Some(port) = stripped[end + 1..].strip_prefix(':') {
                    self.port = port
                        .parse()
                        .map_err(|_| "invalid port in mysql URL".to_string())?;
                }
            } else if let Some((h, p)) = hostport.rsplit_once(':') {
                if let Ok(port) = p.parse::<u16>() {
                    self.host = h.to_string();
                    self.port = port;
                } else {
                    self.host = hostport.to_string();
                }
            } else {
                self.host = hostport.to_string();
            }
        }
        if let Some(db) = dbname {
            if !db.is_empty() {
                self.dbname = percent_decode(db);
            }
        }
        if let Some(q) = query {
            for pair in q.split('&') {
                let (k, v) = pair
                    .split_once('=')
                    .ok_or_else(|| format!("invalid query pair `{pair}` in mysql URL"))?;
                if k == "connect_timeout" {
                    let secs: u64 = v
                        .parse()
                        .map_err(|_| "invalid connect_timeout in mysql URL".to_string())?;
                    self.connect_timeout = Duration::from_secs(secs.max(1));
                }
            }
        }
        Ok(())
    }

    fn parse_kv(&mut self, s: &str) -> Result<(), String> {
        for (k, v) in split_kv(s)? {
            match k.as_str() {
                "host" => self.host = v,
                "port" => {
                    self.port = v
                        .parse()
                        .map_err(|_| format!("invalid port `{v}` in mysql conninfo"))?
                }
                "user" => self.user = v,
                "password" => self.password = v,
                "dbname" => self.dbname = v,
                "connect_timeout" => {
                    let secs: u64 = v
                        .parse()
                        .map_err(|_| format!("invalid connect_timeout `{v}`"))?;
                    self.connect_timeout = Duration::from_secs(secs.max(1));
                }
                _ => {}
            }
        }
        Ok(())
    }
}

/// Split `k=v` pairs; values may be `'single-quoted'` with `''` escapes.
fn split_kv(s: &str) -> Result<Vec<(String, String)>, String> {
    let mut out = Vec::new();
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        while i < bytes.len() && bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        if i >= bytes.len() {
            break;
        }
        let ks = i;
        while i < bytes.len() && !bytes[i].is_ascii_whitespace() && bytes[i] != b'=' {
            i += 1;
        }
        let key = s[ks..i].to_string();
        if i >= bytes.len() || bytes[i] != b'=' {
            return Err(format!(
                "invalid conninfo near `{key}` (expected `key=value`)"
            ));
        }
        i += 1;
        let value = if i < bytes.len() && bytes[i] == b'\'' {
            i += 1;
            let mut v = String::new();
            loop {
                if i >= bytes.len() {
                    return Err("unterminated quoted value in conninfo".to_string());
                }
                if bytes[i] == b'\'' {
                    if i + 1 < bytes.len() && bytes[i + 1] == b'\'' {
                        v.push('\'');
                        i += 2;
                    } else {
                        i += 1;
                        break;
                    }
                } else {
                    v.push(bytes[i] as char);
                    i += 1;
                }
            }
            v
        } else {
            let vs = i;
            while i < bytes.len() && !bytes[i].is_ascii_whitespace() {
                i += 1;
            }
            s[vs..i].to_string()
        };
        if key.is_empty() {
            return Err("empty key in conninfo".to_string());
        }
        out.push((key, value));
    }
    Ok(out)
}

/// Minimal `%XX` decoding for URL segments.
fn percent_decode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let (Some(h), Some(l)) = (hex_val(bytes[i + 1]), hex_val(bytes[i + 2])) {
                out.push((h << 4 | l) as char);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i] as char);
        i += 1;
    }
    out
}

fn hex_val(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

/// A live MySQL connection.
#[derive(Debug)]
pub struct MyConn {
    stream: TcpStream,
    seq: u8,
    /// Server version string from the handshake.
    pub server_version: String,
    /// Server connection id from the handshake.
    pub connection_id: u32,
}

impl MyConn {
    /// Connect + authenticate.
    pub fn connect(info: &MyConnInfo) -> Result<Self, String> {
        let addrs: Vec<_> = (info.host.as_str(), info.port)
            .to_socket_addrs()
            .map_err(|e| {
                format!(
                    "my.connect: cannot resolve `{}:{}`: {e}",
                    info.host, info.port
                )
            })?
            .collect();
        if addrs.is_empty() {
            return Err(format!("my.connect: no addresses for `{}`", info.host));
        }
        let mut last_err = String::new();
        for addr in addrs {
            match TcpStream::connect_timeout(&addr, info.connect_timeout) {
                Ok(stream) => {
                    stream
                        .set_read_timeout(Some(IO_TIMEOUT))
                        .and_then(|_| stream.set_write_timeout(Some(IO_TIMEOUT)))
                        .map_err(|e| format!("my.connect: cannot set timeouts: {e}"))?;
                    let mut conn = Self {
                        stream,
                        seq: 0,
                        server_version: String::new(),
                        connection_id: 0,
                    };
                    conn.handshake(info)?;
                    return Ok(conn);
                }
                Err(e) => last_err = e.to_string(),
            }
        }
        Err(format!("my.connect: connection failed: {last_err}"))
    }

    /// Send one packet, bumping the sequence ID.
    fn send(&mut self, body: &[u8]) -> Result<(), String> {
        let pkt = frame(self.seq, body);
        self.seq = self.seq.wrapping_add(1);
        self.stream
            .write_all(&pkt)
            .map_err(|e| format!("my: socket write failed: {e}"))
    }

    /// Read one packet body (length + sequence framing stripped).
    fn read_packet(&mut self) -> Result<Vec<u8>, String> {
        let mut header = [0u8; 4];
        self.stream
            .read_exact(&mut header)
            .map_err(|e| format!("my: socket read failed: {e}"))?;
        let len = (header[0] as usize) | ((header[1] as usize) << 8) | ((header[2] as usize) << 16);
        if len > 64 * 1024 * 1024 {
            return Err("my: packet length out of range".to_string());
        }
        let mut body = vec![0u8; len];
        self.stream
            .read_exact(&mut body)
            .map_err(|e| format!("my: socket read failed: {e}"))?;
        Ok(body)
    }

    /// Handshake + authentication loop.
    fn handshake(&mut self, info: &MyConnInfo) -> Result<(), String> {
        let body = self.read_packet()?;
        let hs = parse_handshake(&body)?;
        self.server_version = hs.server_version.clone();
        self.connection_id = hs.connection_id;
        let mut plugin = if hs.auth_plugin.is_empty() {
            "mysql_native_password".to_string()
        } else {
            hs.auth_plugin.clone()
        };
        let mut seed = hs.seed.clone();
        let mut token = auth_token(&plugin, info.password.as_bytes(), &seed)?;

        let dbname = if info.dbname.is_empty() {
            None
        } else {
            Some(info.dbname.as_str())
        };
        self.seq = 1;
        self.send(&build_handshake_response(
            &info.user, &token, dbname, &plugin,
        ))?;

        loop {
            let reply = self.read_packet()?;
            match reply.first() {
                Some(0x00) if reply.len() == 1 => {
                    // fast_auth_success: OK packet follows.
                }
                Some(0x00) => {
                    parse_ok(&reply)?;
                    return Ok(());
                }
                Some(0xFF) => {
                    let (code, msg) = parse_err(&reply)?;
                    return Err(format!("my.connect: server error {code}: {msg}"));
                }
                Some(0xFE) => {
                    // AuthSwitchRequest: recompute the token for the new
                    // plugin/seed and answer.
                    (plugin, seed) = parse_auth_switch(&reply)?;
                    token = auth_token(&plugin, info.password.as_bytes(), &seed)?;
                    self.send(&token)?;
                }
                Some(0x01) if reply.len() == 1 => {
                    // caching_sha2 full authentication: clear-text password.
                    let mut clear = info.password.clone().into_bytes();
                    clear.push(0);
                    self.send(&clear)?;
                }
                Some(0x01) => {
                    // RSA public-key fast path is out of scope for a
                    // dependency-free driver over plaintext.
                    return Err(
                        "my.connect: server requested RSA-encrypted password exchange, \
                         which this driver does not support (use a TLS proxy or \
                         mysql_native_password)"
                            .to_string(),
                    );
                }
                _ => {
                    return Err("my.connect: unexpected message during handshake".to_string());
                }
            }
        }
    }

    /// Prepare + execute; returns (columns, rows, affected).
    fn prepared(&mut self, sql: &str, params: &[MyParam]) -> Result<RawResult, String> {
        self.seq = 0;
        self.send(&build_prepare(sql))?;
        let prep = self.read_packet()?;
        if prep.first() == Some(&0xFF) {
            let (code, msg) = parse_err(&prep)?;
            return Err(format!("my.query: server error {code}: {msg}"));
        }
        let ok = parse_prepare_ok(&prep)?;
        // Skip parameter definitions + EOF (types ride with EXECUTE).
        for _ in 0..ok.params {
            self.skip_frame()?;
        }
        if ok.params > 0 {
            self.expect_eof()?;
        }
        // Skip column definitions + EOF (fresh defs arrive with EXECUTE).
        for _ in 0..ok.columns {
            self.skip_frame()?;
        }
        if ok.columns > 0 {
            self.expect_eof()?;
        }

        self.seq = 0;
        self.send(&build_execute(ok.stmt_id, params))?;
        let first = self.read_packet()?;
        match first.first() {
            Some(0xFF) => {
                let (code, msg) = parse_err(&first)?;
                return Err(format!("my.query: server error {code}: {msg}"));
            }
            Some(0x00) => {
                let (affected, _) = parse_ok(&first)?;
                return Ok((Vec::new(), Vec::new(), affected));
            }
            _ => {}
        }
        let (ncols, _) = read_lenenc(&first)?;
        let ncols = ncols as usize;
        let mut cols = Vec::with_capacity(ncols);
        for _ in 0..ncols {
            let def = self.read_packet()?;
            cols.push(parse_coldef(&def)?);
        }
        self.expect_eof()?;
        let types: Vec<u8> = cols.iter().map(|c| c.ftype).collect();
        let mut rows = Vec::new();
        loop {
            let pkt = self.read_packet()?;
            if pkt.first() == Some(&0xFF) {
                let (code, msg) = parse_err(&pkt)?;
                return Err(format!("my.query: server error {code}: {msg}"));
            }
            if is_eof(&pkt) {
                break;
            }
            rows.push(parse_binary_row(&pkt, &types)?);
        }
        let n = rows.len() as u64;
        Ok((cols, rows, n))
    }

    fn skip_frame(&mut self) -> Result<(), String> {
        self.read_packet().map(|_| ())
    }

    fn expect_eof(&mut self) -> Result<(), String> {
        let pkt = self.read_packet()?;
        if is_eof(&pkt) {
            Ok(())
        } else {
            Err("my: expected EOF packet".to_string())
        }
    }

    /// Execute a statement; returns affected-row (or row) count.
    pub fn exec(&mut self, sql: &str, params: &[MyParam]) -> Result<i64, String> {
        let (cols, rows, affected) = self.prepared(sql, params)?;
        if cols.is_empty() && rows.is_empty() {
            Ok(affected as i64)
        } else {
            Ok(rows.len() as i64)
        }
    }

    /// Query rows; returns (columns, rows) with decoded cells.
    pub fn query(&mut self, sql: &str, params: &[MyParam]) -> Result<RawRows, String> {
        let (cols, rows, _) = self.prepared(sql, params)?;
        Ok((cols, rows))
    }

    /// Send `COM_QUIT` (best effort); the socket closes on drop anyway.
    pub fn terminate(&mut self) {
        self.seq = 0;
        let _ = self.send(&build_quit());
    }
}

/// Compute the auth token for a plugin, or a clear error for plugins this
/// driver cannot speak.
fn auth_token(plugin: &str, password: &[u8], seed: &[u8]) -> Result<Vec<u8>, String> {
    match plugin {
        "mysql_native_password" => Ok(native_token(password, seed)),
        "caching_sha2_password" => Ok(caching_token(password, seed)),
        // `sha256_password` clear-text fallback (same shape as the
        // caching full-auth path; RSA encryption unsupported).
        "sha256_password" => {
            let mut clear = password.to_vec();
            clear.push(0);
            Ok(clear)
        }
        other => Err(format!(
            "my.connect: auth plugin `{other}` is not supported \
             (only mysql_native_password and caching_sha2_password)"
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::natives::db::mysql_wire;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::mpsc;

    #[test]
    fn conninfo_url_and_kv() {
        let c = MyConnInfo::parse("mysql://u:p%40ss@db.local:3307/app?connect_timeout=5").unwrap();
        assert_eq!(c.host, "db.local");
        assert_eq!(c.port, 3307);
        assert_eq!(c.dbname, "app");
        assert_eq!(c.user, "u");
        assert_eq!(c.password, "p@ss");
        assert_eq!(c.connect_timeout, Duration::from_secs(5));
        let k = MyConnInfo::parse("host=h port=1234 user=bob password='x y'").unwrap();
        assert_eq!(k.host, "h");
        assert_eq!(k.port, 1234);
        assert_eq!(k.password, "x y");
        let d = MyConnInfo::parse("").unwrap();
        assert_eq!((d.port, d.user.as_str()), (3306, "root"));
    }

    // -- Mock-server harness ----------------------------------------------

    fn read_packet(s: &mut TcpStream) -> Vec<u8> {
        let mut hdr = [0u8; 4];
        s.read_exact(&mut hdr).unwrap();
        let len = (hdr[0] as usize) | ((hdr[1] as usize) << 8) | ((hdr[2] as usize) << 16);
        let mut body = vec![0u8; len];
        s.read_exact(&mut body).unwrap();
        body
    }

    fn send_packet(s: &mut TcpStream, seq: u8, body: &[u8]) {
        s.write_all(&mysql_wire::frame(seq, body)).unwrap();
    }

    fn handshake_v10(seed1: &[u8], seed2: &[u8], plugin: &str) -> Vec<u8> {
        let mut b = vec![10u8];
        b.extend_from_slice(b"8.0.36-mock\0");
        b.extend_from_slice(&7u32.to_le_bytes());
        b.extend_from_slice(seed1);
        b.push(0);
        let caps: u32 = 0x0008_8207;
        b.extend_from_slice(&(caps as u16).to_le_bytes());
        b.push(45);
        b.extend_from_slice(&2u16.to_le_bytes());
        b.extend_from_slice(&((caps >> 16) as u16).to_be_bytes());
        // NOTE: handshake packs caps-hi little-endian; fixed below.
        b.pop();
        b.pop();
        b.extend_from_slice(&((caps >> 16) as u16).to_le_bytes());
        b.push(21);
        b.extend_from_slice(&[0u8; 10]);
        b.extend_from_slice(seed2);
        b.push(0);
        b.extend_from_slice(plugin.as_bytes());
        b.push(0);
        b
    }

    fn ok_packet(affected: u64) -> Vec<u8> {
        let mut b = vec![0x00];
        mysql_wire::write_lenenc(affected, &mut b);
        mysql_wire::write_lenenc(0, &mut b);
        b.extend_from_slice(&2u16.to_le_bytes());
        b.extend_from_slice(&0u16.to_le_bytes());
        b
    }

    fn eof_packet() -> Vec<u8> {
        vec![0xFE, 0, 0, 0x02, 0x00]
    }

    fn coldef_packet(name: &str, ty: u8) -> Vec<u8> {
        let mut b = Vec::new();
        for s in ["def", "", "t", ""] {
            let mut l = Vec::new();
            mysql_wire::write_lenenc(s.len() as u64, &mut l);
            b.extend_from_slice(&l);
            b.extend_from_slice(s.as_bytes());
        }
        let mut l = Vec::new();
        mysql_wire::write_lenenc(name.len() as u64, &mut l);
        b.extend_from_slice(&l);
        b.extend_from_slice(name.as_bytes());
        l.clear();
        mysql_wire::write_lenenc(0, &mut l);
        b.extend_from_slice(&l);
        b.push(0x0c);
        b.extend_from_slice(&33u16.to_le_bytes());
        b.extend_from_slice(&256u32.to_le_bytes());
        b.push(ty);
        b.extend_from_slice(&0u16.to_le_bytes());
        b.push(0);
        b.extend_from_slice(&[0, 0]);
        b
    }

    fn bin_row(cells: &[(u8, Option<Vec<u8>>)]) -> Vec<u8> {
        // cells: (type, value); ints passed pre-encoded LE.
        let mut b = vec![0x00u8, 0x00];
        let mut values = Vec::new();
        for (i, (ty, v)) in cells.iter().enumerate() {
            match v {
                None => b[1] |= 1 << ((i + 2) % 8),
                Some(bytes) => {
                    if *ty == mysql_wire::TYPE_LONG {
                        values.extend_from_slice(bytes);
                    } else {
                        let mut l = Vec::new();
                        mysql_wire::write_lenenc(bytes.len() as u64, &mut l);
                        values.extend_from_slice(&l);
                        values.extend_from_slice(bytes);
                    }
                }
            }
        }
        b.extend_from_slice(&values);
        b
    }

    fn mock_server(handler: impl FnOnce(TcpStream) + Send + 'static) -> std::net::SocketAddr {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(10)))
                .unwrap();
            stream
                .set_write_timeout(Some(Duration::from_secs(10)))
                .unwrap();
            handler(stream);
        });
        addr
    }

    fn info_for(addr: std::net::SocketAddr) -> MyConnInfo {
        MyConnInfo {
            host: "127.0.0.1".to_string(),
            port: addr.port(),
            user: "u".to_string(),
            password: String::new(),
            dbname: String::new(),
            connect_timeout: Duration::from_secs(5),
        }
    }

    #[test]
    fn handshake_native_and_prepare_execute() {
        let (tx, rx) = mpsc::channel();
        let seed1 = b"abcdefgh";
        let seed2 = b"ijklmnopqrst";
        let addr = mock_server(move |mut s| {
            // Client starts: no packet first, server speaks handshake.
            send_packet(
                &mut s,
                0,
                &handshake_v10(seed1, seed2, "mysql_native_password"),
            );
            let resp = read_packet(&mut s);
            // caps(4) max(4) charset(1) reserved(23) user\0 token-len token
            assert_eq!(
                &resp[..4],
                &(mysql_wire::CLIENT_CAPS & !mysql_wire::CAP_CONNECT_WITH_DB).to_le_bytes()
            );
            let mut pos = 4 + 4 + 1 + 23;
            assert_eq!(&resp[pos..pos + 2], b"u\0");
            pos += 2;
            let tlen = resp[pos] as usize;
            pos += 1;
            let token = &resp[pos..pos + tlen];
            let mut seed = seed1.to_vec();
            seed.extend_from_slice(seed2);
            assert_eq!(token, &mysql_wire::native_token(b"", &seed)[..0]);
            // Empty password in this test (info.password empty) -> empty token.
            assert!(token.is_empty());
            send_packet(&mut s, 2, &ok_packet(0));
            // COM_STMT_PREPARE
            let prep = read_packet(&mut s);
            assert_eq!(prep[0], mysql_wire::COM_STMT_PREPARE);
            let sql = String::from_utf8(prep[1..].to_vec()).unwrap();
            tx.send(sql).unwrap();
            // stmt 7, 2 cols, 1 param
            let mut po = vec![0x00];
            po.extend_from_slice(&7u32.to_le_bytes());
            po.extend_from_slice(&2u16.to_le_bytes());
            po.extend_from_slice(&1u16.to_le_bytes());
            po.push(0);
            po.extend_from_slice(&0u16.to_le_bytes());
            send_packet(&mut s, 0, &po);
            send_packet(&mut s, 1, &coldef_packet("?", mysql_wire::TYPE_VAR_STRING));
            send_packet(&mut s, 2, &eof_packet());
            send_packet(&mut s, 3, &coldef_packet("id", mysql_wire::TYPE_LONG));
            send_packet(
                &mut s,
                4,
                &coldef_packet("name", mysql_wire::TYPE_VAR_STRING),
            );
            send_packet(&mut s, 5, &eof_packet());
            // COM_STMT_EXECUTE
            let exec = read_packet(&mut s);
            assert_eq!(exec[0], mysql_wire::COM_STMT_EXECUTE);
            assert_eq!(&exec[1..5], &7u32.to_le_bytes());
            // Column count + defs + rows.
            send_packet(&mut s, 0, &[0x02]);
            send_packet(&mut s, 1, &coldef_packet("id", mysql_wire::TYPE_LONG));
            send_packet(
                &mut s,
                2,
                &coldef_packet("name", mysql_wire::TYPE_VAR_STRING),
            );
            send_packet(&mut s, 3, &eof_packet());
            send_packet(
                &mut s,
                4,
                &bin_row(&[
                    (mysql_wire::TYPE_LONG, Some(7i32.to_le_bytes().to_vec())),
                    (mysql_wire::TYPE_VAR_STRING, Some(b"carol".to_vec())),
                ]),
            );
            send_packet(
                &mut s,
                5,
                &bin_row(&[
                    (mysql_wire::TYPE_LONG, None),
                    (mysql_wire::TYPE_VAR_STRING, Some(b"dave".to_vec())),
                ]),
            );
            send_packet(&mut s, 6, &eof_packet());
        });

        let mut conn = MyConn::connect(&info_for(addr)).unwrap();
        assert_eq!(conn.server_version, "8.0.36-mock");
        let (cols, rows) = conn
            .query("SELECT id, name FROM t WHERE id = ?", &[MyParam::Int(7)])
            .unwrap();
        assert_eq!(cols.len(), 2);
        assert_eq!(cols[0].ftype, mysql_wire::TYPE_LONG);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0][0], Some(7i32.to_le_bytes().to_vec()));
        assert_eq!(rows[1][0], None);
        // The prepared SQL carries a bare `?` (rewritten from `?1`).
        assert_eq!(
            rx.recv_timeout(Duration::from_secs(5)).unwrap(),
            "SELECT id, name FROM t WHERE id = ?"
        );
    }

    #[test]
    fn exec_returns_affected_rows() {
        let addr = mock_server(move |mut s| {
            send_packet(
                &mut s,
                0,
                &handshake_v10(b"abcdefgh", b"ijklmnopqrst", "mysql_native_password"),
            );
            let _ = read_packet(&mut s);
            send_packet(&mut s, 2, &ok_packet(0));
            let prep = read_packet(&mut s);
            assert_eq!(prep[0], mysql_wire::COM_STMT_PREPARE);
            let mut po = vec![0x00];
            po.extend_from_slice(&9u32.to_le_bytes());
            po.extend_from_slice(&0u16.to_le_bytes());
            po.extend_from_slice(&0u16.to_le_bytes());
            po.push(0);
            po.extend_from_slice(&0u16.to_le_bytes());
            send_packet(&mut s, 0, &po);
            let exec = read_packet(&mut s);
            assert_eq!(exec[0], mysql_wire::COM_STMT_EXECUTE);
            assert_eq!(&exec[1..5], &9u32.to_le_bytes());
            send_packet(&mut s, 0, &ok_packet(3));
        });
        let mut conn = MyConn::connect(&info_for(addr)).unwrap();
        assert_eq!(conn.exec("INSERT INTO t VALUES (1)", &[]).unwrap(), 3);
    }

    #[test]
    fn caching_sha2_full_auth_flow() {
        let addr = mock_server(move |mut s| {
            send_packet(
                &mut s,
                0,
                &handshake_v10(b"seedpart1", b"seedpart2222", "caching_sha2_password"),
            );
            let _resp = read_packet(&mut s);
            // Fast-auth token first (32 bytes for any password).
            // Ask for full authentication.
            send_packet(&mut s, 2, &[0x01]);
            let clear = read_packet(&mut s);
            assert_eq!(clear.last(), Some(&0));
            assert!(clear.starts_with(b"secret"));
            // fast_auth_success + OK.
            send_packet(&mut s, 3, &[0x00]);
            send_packet(&mut s, 4, &ok_packet(0));
        });
        let mut info = info_for(addr);
        info.password = "secret".to_string();
        let conn = MyConn::connect(&info).unwrap();
        assert_eq!(conn.server_version, "8.0.36-mock");
    }

    #[test]
    fn auth_switch_to_native() {
        let seed1 = b"AAAAAAAA";
        let seed2 = b"BBBBBBBBBBBB";
        let addr = mock_server(move |mut s| {
            send_packet(
                &mut s,
                0,
                &handshake_v10(seed1, seed2, "caching_sha2_password"),
            );
            let _resp = read_packet(&mut s);
            // Switch to mysql_native_password with a fresh seed.
            let mut sw = vec![0xFE];
            sw.extend_from_slice(b"mysql_native_password\0");
            sw.extend_from_slice(b"CCCCCCCCDDDDDDDDDDDD\0");
            send_packet(&mut s, 2, &sw);
            let token_pkt = read_packet(&mut s);
            let seed = b"CCCCCCCCDDDDDDDDDDDD".to_vec();
            assert_eq!(token_pkt, mysql_wire::native_token(b"pw", &seed));
            send_packet(&mut s, 3, &ok_packet(0));
        });
        let mut info = info_for(addr);
        info.password = "pw".to_string();
        MyConn::connect(&info).unwrap();
    }

    #[test]
    fn query_error_propagates() {
        let addr = mock_server(move |mut s| {
            send_packet(
                &mut s,
                0,
                &handshake_v10(b"abcdefgh", b"ijklmnopqrst", "mysql_native_password"),
            );
            let _ = read_packet(&mut s);
            send_packet(&mut s, 2, &ok_packet(0));
            let _ = read_packet(&mut s); // PREPARE
            let mut po = vec![0x00];
            po.extend_from_slice(&1u32.to_le_bytes());
            po.extend_from_slice(&0u16.to_le_bytes());
            po.extend_from_slice(&0u16.to_le_bytes());
            po.push(0);
            po.extend_from_slice(&0u16.to_le_bytes());
            send_packet(&mut s, 0, &po);
            let _ = read_packet(&mut s); // EXECUTE
            let mut err = vec![0xFF, 0x46, 0x04];
            err.extend_from_slice(b"#42000You have an error in your SQL syntax");
            send_packet(&mut s, 0, &err);
        });
        let mut conn = MyConn::connect(&info_for(addr)).unwrap();
        let err = conn.exec("BOGUS", &[]).unwrap_err();
        assert!(err.contains("1062") || err.contains("syntax"), "got: {err}");
    }

    #[test]
    fn connect_refused_errors() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        let mut info = info_for(std::net::SocketAddr::from(([127, 0, 0, 1], port)));
        info.connect_timeout = Duration::from_secs(2);
        let err = MyConn::connect(&info).unwrap_err();
        assert!(err.contains("my.connect"), "got: {err}");
    }

    #[test]
    fn conn_is_send_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<MyConn>();
        assert_send_sync::<super::super::DbConn>();
    }
}
