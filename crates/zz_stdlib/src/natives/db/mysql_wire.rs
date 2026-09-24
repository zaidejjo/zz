//! Pure-Rust MySQL client/server protocol engine — zero dependencies.
//!
//! Covers what the driver needs:
//! - packet framing (3-byte LE length + 1-byte sequence ID),
//! - length-encoded integers / strings,
//! - `HandshakeV10` parsing and `HandshakeResponse41` building,
//! - `mysql_native_password` (SHA1, implemented below) and
//!   `caching_sha2_password` (SHA-256 reused from [`super::pg_wire`])
//!   token math,
//! - `COM_STMT_PREPARE` / `COM_STMT_EXECUTE` builders and the binary
//!   resultset parser (`ColumnDefinition41`, binary rows).
//!
//! Token formulas follow the MySQL 8.0 source (`sql/auth/sha2_password.cc`):
//! native `SHA1(pw) XOR SHA1(seed + SHA1(SHA1(pw)))`, caching
//! `SHA256(pw) XOR SHA256(SHA256(SHA256(pw)), seed)` (always 32 bytes).

use zz_native_rt::pg_wire::sha256;

// ---------------------------------------------------------------------------
// SHA-1 (FIPS 180-4)
// ---------------------------------------------------------------------------

/// Raw SHA-1 digest of `input`.
pub fn sha1(input: &[u8]) -> [u8; 20] {
    let mut msg = input.to_vec();
    let bit_len = (input.len() as u64).wrapping_mul(8);
    msg.push(0x80);
    while msg.len() % 64 != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&bit_len.to_be_bytes());

    let mut h: [u32; 5] = [0x67452301, 0xefcdab89, 0x98badcfe, 0x10325476, 0xc3d2e1f0];
    let mut w = [0u32; 80];
    for chunk in msg.as_chunks::<64>().0 {
        for i in 0..16 {
            w[i] = u32::from_be_bytes([
                chunk[4 * i],
                chunk[4 * i + 1],
                chunk[4 * i + 2],
                chunk[4 * i + 3],
            ]);
        }
        for i in 16..80 {
            w[i] = (w[i - 3] ^ w[i - 8] ^ w[i - 14] ^ w[i - 16]).rotate_left(1);
        }
        let (mut a, mut b, mut c, mut d, mut e) = (h[0], h[1], h[2], h[3], h[4]);
        for (i, &wi) in w.iter().enumerate() {
            let (f, k) = match i {
                0..20 => ((b & c) | ((!b) & d), 0x5a827999),
                20..40 => (b ^ c ^ d, 0x6ed9eba1),
                40..60 => ((b & c) | (b & d) | (c & d), 0x8f1bbcdc),
                _ => (b ^ c ^ d, 0xca62c1d6),
            };
            let tmp = a
                .rotate_left(5)
                .wrapping_add(f)
                .wrapping_add(e)
                .wrapping_add(k)
                .wrapping_add(wi);
            e = d;
            d = c;
            c = b.rotate_left(30);
            b = a;
            a = tmp;
        }
        h[0] = h[0].wrapping_add(a);
        h[1] = h[1].wrapping_add(b);
        h[2] = h[2].wrapping_add(c);
        h[3] = h[3].wrapping_add(d);
        h[4] = h[4].wrapping_add(e);
    }

    let mut out = [0u8; 20];
    for (i, v) in h.iter().enumerate() {
        out[4 * i..4 * i + 4].copy_from_slice(&v.to_be_bytes());
    }
    out
}

/// Hex helper for the token test vectors below.
#[cfg(test)]
fn hex_lower(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push(HEX[(b >> 4) as usize] as char);
        s.push(HEX[(b & 15) as usize] as char);
    }
    s
}

// ---------------------------------------------------------------------------
// Auth token math
// ---------------------------------------------------------------------------

/// `mysql_native_password` token: `SHA1(pw) XOR SHA1(seed + SHA1(SHA1(pw)))`.
/// Empty password yields an empty response.
pub fn native_token(password: &[u8], seed: &[u8]) -> Vec<u8> {
    if password.is_empty() {
        return Vec::new();
    }
    let stage1 = sha1(password);
    let stage2 = sha1(&stage1);
    let mut input = seed.to_vec();
    input.extend_from_slice(&stage2);
    let stage3 = sha1(&input);
    stage1
        .iter()
        .zip(stage3.iter())
        .map(|(a, b)| a ^ b)
        .collect()
}

/// `caching_sha2_password` fast-auth token (always 32 bytes):
/// `SHA256(pw) XOR SHA256(SHA256(SHA256(pw)), seed)`.
/// Empty password yields an empty response.
pub fn caching_token(password: &[u8], seed: &[u8]) -> Vec<u8> {
    if password.is_empty() {
        return Vec::new();
    }
    let stage1 = sha256(password);
    let stage2 = sha256(&stage1);
    let mut input = stage2.to_vec();
    input.extend_from_slice(seed);
    let stage3 = sha256(&input);
    stage1
        .iter()
        .zip(stage3.iter())
        .map(|(a, b)| a ^ b)
        .collect()
}

// ---------------------------------------------------------------------------
// Protocol constants
// ---------------------------------------------------------------------------

pub const CAP_LONG_PASSWORD: u32 = 0x0000_0001;
pub const CAP_LONG_FLAG: u32 = 0x0000_0004;
pub const CAP_CONNECT_WITH_DB: u32 = 0x0000_0008;
pub const CAP_PROTOCOL41: u32 = 0x0000_0200;
pub const CAP_TRANSACTIONS: u32 = 0x0000_2000;
pub const CAP_SECURE_CONNECTION: u32 = 0x0000_8000;
pub const CAP_PLUGIN_AUTH: u32 = 0x0008_0000;

/// Capabilities this driver offers.
pub const CLIENT_CAPS: u32 = CAP_LONG_PASSWORD
    | CAP_LONG_FLAG
    | CAP_CONNECT_WITH_DB
    | CAP_PROTOCOL41
    | CAP_TRANSACTIONS
    | CAP_SECURE_CONNECTION
    | CAP_PLUGIN_AUTH;

/// Commands we send.
pub const COM_QUIT: u8 = 0x01;
pub const COM_STMT_PREPARE: u8 = 0x16;
pub const COM_STMT_EXECUTE: u8 = 0x17;

/// Column types (a subset is enough for mapping).
pub const TYPE_TINY: u8 = 1;
pub const TYPE_SHORT: u8 = 2;
pub const TYPE_LONG: u8 = 3;
pub const TYPE_FLOAT: u8 = 4;
pub const TYPE_DOUBLE: u8 = 5;
pub const TYPE_NULL: u8 = 6;
pub const TYPE_LONGLONG: u8 = 8;
pub const TYPE_INT24: u8 = 9;
pub const TYPE_DATE: u8 = 10;
pub const TYPE_TIME: u8 = 11;
pub const TYPE_DATETIME: u8 = 12;
pub const TYPE_TIMESTAMP: u8 = 7;
pub const TYPE_YEAR: u8 = 13;
pub const TYPE_VARCHAR: u8 = 15;
pub const TYPE_BIT: u8 = 16;
pub const TYPE_NEWDECIMAL: u8 = 246;
pub const TYPE_VAR_STRING: u8 = 253;
pub const TYPE_STRING: u8 = 254;
pub const TYPE_DECIMAL: u8 = 0;

/// Coarse kind of a column type for ZZ value mapping.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MyKind {
    Int,
    Float,
    Text,
}

/// Map a MySQL column type to a [`MyKind`]. Dates, decimals-as-text,
/// blobs and strings all arrive as text.
pub fn col_type_kind(ty: u8) -> MyKind {
    match ty {
        TYPE_TINY | TYPE_SHORT | TYPE_LONG | TYPE_LONGLONG | TYPE_INT24 | TYPE_YEAR => MyKind::Int,
        TYPE_FLOAT | TYPE_DOUBLE => MyKind::Float,
        TYPE_NEWDECIMAL | TYPE_DECIMAL => MyKind::Float,
        TYPE_VARCHAR | TYPE_VAR_STRING | TYPE_STRING | TYPE_BIT => MyKind::Text,
        _ => MyKind::Text,
    }
}

// ---------------------------------------------------------------------------
// Packet framing
// ---------------------------------------------------------------------------

/// Frame a payload: 3-byte LE length + 1-byte sequence ID + body.
pub fn frame(seq: u8, body: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(4 + body.len());
    let len = body.len() as u32;
    out.push((len & 0xff) as u8);
    out.push(((len >> 8) & 0xff) as u8);
    out.push(((len >> 16) & 0xff) as u8);
    out.push(seq);
    out.extend_from_slice(body);
    out
}

/// Split framed messages out of a buffer; returns complete bodies plus
/// trailing incomplete bytes. Sequence IDs are not validated (lenient).
/// (The blocking driver reads exact frames instead; this helper serves
/// buffered transports and tests.)
#[allow(dead_code)]
pub fn split_frames(buf: &[u8]) -> (Vec<Vec<u8>>, Vec<u8>) {
    let mut out = Vec::new();
    let mut pos = 0;
    while pos + 4 <= buf.len() {
        let len =
            (buf[pos] as usize) | ((buf[pos + 1] as usize) << 8) | ((buf[pos + 2] as usize) << 16);
        if buf.len() - pos - 4 < len {
            break;
        }
        out.push(buf[pos + 4..pos + 4 + len].to_vec());
        pos += 4 + len;
    }
    (out, buf[pos..].to_vec())
}

// ---------------------------------------------------------------------------
// Length-encoded integers / strings
// ---------------------------------------------------------------------------

/// Read a length-encoded integer; returns (value, bytes consumed).
/// `None` marks SQL NULL in row contexts (caller interprets 0xFB).
pub fn read_lenenc(data: &[u8]) -> Result<(u64, usize), String> {
    if data.is_empty() {
        return Err("truncated length-encoded integer".to_string());
    }
    match data[0] {
        0xFB => Ok((u64::MAX, 1)), // NULL marker
        0xFC => {
            if data.len() < 3 {
                return Err("truncated length-encoded integer".to_string());
            }
            Ok((u16::from_le_bytes([data[1], data[2]]) as u64, 3))
        }
        0xFD => {
            if data.len() < 4 {
                return Err("truncated length-encoded integer".to_string());
            }
            Ok((
                (data[1] as u64) | ((data[2] as u64) << 8) | ((data[3] as u64) << 16),
                4,
            ))
        }
        0xFE => {
            if data.len() < 9 {
                return Err("truncated length-encoded integer".to_string());
            }
            let mut b = [0u8; 8];
            b.copy_from_slice(&data[1..9]);
            Ok((u64::from_le_bytes(b), 9))
        }
        v => Ok((v as u64, 1)),
    }
}

/// Write a length-encoded integer.
pub fn write_lenenc(v: u64, out: &mut Vec<u8>) {
    if v < 251 {
        out.push(v as u8);
    } else if v < 65536 {
        out.push(0xFC);
        out.extend_from_slice(&(v as u16).to_le_bytes());
    } else if v < 16777216 {
        out.push(0xFD);
        out.push((v & 0xff) as u8);
        out.push(((v >> 8) & 0xff) as u8);
        out.push(((v >> 16) & 0xff) as u8);
    } else {
        out.push(0xFE);
        out.extend_from_slice(&v.to_le_bytes());
    }
}

/// Read a length-encoded byte string; `None` on SQL NULL (0xFB).
pub fn read_lenenc_bytes(data: &[u8]) -> Result<(Option<Vec<u8>>, usize), String> {
    let (len, used) = read_lenenc(data)?;
    if len == u64::MAX {
        return Ok((None, used));
    }
    let len = len as usize;
    if data.len() < used + len {
        return Err("truncated length-encoded string".to_string());
    }
    Ok((Some(data[used..used + len].to_vec()), used + len))
}

/// Read a NUL-terminated string; returns (value, bytes consumed incl. NUL).
pub fn read_cstr(data: &[u8]) -> Result<(Vec<u8>, usize), String> {
    match data.iter().position(|&b| b == 0) {
        Some(i) => Ok((data[..i].to_vec(), i + 1)),
        None => Err("missing NUL terminator".to_string()),
    }
}

// ---------------------------------------------------------------------------
// Handshake
// ---------------------------------------------------------------------------

/// Parsed `HandshakeV10` from the server. All metadata is retained
/// (only seed/plugin/version/id drive the driver today; the rest aids
/// debugging and capability negotiation).
#[allow(dead_code)]
#[derive(Debug)]
pub struct Handshake {
    pub protocol: u8,
    pub server_version: String,
    pub connection_id: u32,
    /// Full auth-plugin seed (part1 + part2, NUL-trimmed).
    pub seed: Vec<u8>,
    pub caps: u32,
    pub charset: u8,
    pub status: u16,
    pub auth_plugin: String,
}

/// Parse a `HandshakeV10` body (packet framing already stripped).
pub fn parse_handshake(body: &[u8]) -> Result<Handshake, String> {
    let mut pos = 0;
    let take = |pos: &mut usize, n: usize| -> Result<&[u8], String> {
        if body.len() < *pos + n {
            return Err("truncated handshake".to_string());
        }
        let s = &body[*pos..*pos + n];
        *pos += n;
        Ok(s)
    };
    let protocol = take(&mut pos, 1)?[0];
    if protocol != 10 {
        return Err(format!("unsupported handshake protocol {protocol}"));
    }
    let (ver, used) = read_cstr(&body[pos..])?;
    pos += used;
    let server_version =
        String::from_utf8(ver).map_err(|_| "handshake version is not UTF-8".to_string())?;
    let connection_id = u32::from_le_bytes(take(&mut pos, 4)?.try_into().unwrap());
    let part1 = take(&mut pos, 8)?.to_vec();
    pos += 1; // filler
    let caps_lo = u16::from_le_bytes(take(&mut pos, 2)?.try_into().unwrap()) as u32;
    let charset = take(&mut pos, 1)?[0];
    let status = u16::from_le_bytes(take(&mut pos, 2)?.try_into().unwrap());
    let caps_hi = u16::from_le_bytes(take(&mut pos, 2)?.try_into().unwrap()) as u32;
    let caps = caps_lo | (caps_hi << 16);
    let plugin_len = if caps & CAP_PLUGIN_AUTH != 0 {
        take(&mut pos, 1)?[0] as usize
    } else {
        0
    };
    pos += 10; // reserved
               // Part 2 is max(13, plugin_len - 8); at minimum read 13 or to end.
    let part2_len = plugin_len
        .saturating_sub(8)
        .max(13)
        .min(body.len().saturating_sub(pos));
    let mut part2 = take(&mut pos, part2_len)?.to_vec();
    // Trim at first NUL (the seed is NUL-terminated).
    if let Some(i) = part2.iter().position(|&b| b == 0) {
        part2.truncate(i);
    }
    let auth_plugin = if caps & CAP_PLUGIN_AUTH != 0 && pos < body.len() {
        let (name, _) = read_cstr(&body[pos..])?;
        String::from_utf8(name).map_err(|_| "auth plugin name is not UTF-8".to_string())?
    } else {
        String::new()
    };
    let mut seed = part1;
    // Part 1 may itself be NUL-padded; keep raw (servers send 8 bytes).
    seed.extend_from_slice(&part2);
    Ok(Handshake {
        protocol,
        server_version,
        connection_id,
        seed,
        caps,
        charset,
        status,
        auth_plugin,
    })
}

/// Build a `HandshakeResponse41` body.
pub fn build_handshake_response(
    user: &str,
    token: &[u8],
    dbname: Option<&str>,
    plugin: &str,
) -> Vec<u8> {
    let mut caps = CLIENT_CAPS;
    if dbname.is_none_or(str::is_empty) {
        caps &= !CAP_CONNECT_WITH_DB;
    }
    let mut out = Vec::new();
    out.extend_from_slice(&caps.to_le_bytes());
    out.extend_from_slice(&0x0100_0000u32.to_le_bytes()); // max packet 16MB
    out.push(45); // utf8mb4
    out.extend_from_slice(&[0u8; 23]);
    out.extend_from_slice(user.as_bytes());
    out.push(0);
    // SECURE_CONNECTION: length-prefixed auth response.
    out.push(token.len() as u8);
    out.extend_from_slice(token);
    if let Some(db) = dbname {
        if !db.is_empty() {
            out.extend_from_slice(db.as_bytes());
            out.push(0);
        }
    }
    out.extend_from_slice(plugin.as_bytes());
    out.push(0);
    out
}

/// Parse an `AuthSwitchRequest` body: `(plugin_name, seed)`.
pub fn parse_auth_switch(body: &[u8]) -> Result<(String, Vec<u8>), String> {
    if body.first() != Some(&0xFE) {
        return Err("not an auth switch request".to_string());
    }
    let (name, used) = read_cstr(&body[1..])?;
    let name = String::from_utf8(name).map_err(|_| "plugin name is not UTF-8".to_string())?;
    let mut seed = body[1 + used..].to_vec();
    // Seed is NUL-terminated; trim it.
    if let Some(i) = seed.iter().position(|&b| b == 0) {
        seed.truncate(i);
    }
    Ok((name, seed))
}

// ---------------------------------------------------------------------------
// OK / ERR / EOF
// ---------------------------------------------------------------------------

/// Parsed OK packet: (affected_rows, last_insert_id).
pub fn parse_ok(body: &[u8]) -> Result<(u64, u64), String> {
    if body.first() != Some(&0x00) {
        return Err("not an OK packet".to_string());
    }
    let (affected, used) = read_lenenc(&body[1..])?;
    let (insert_id, _) = read_lenenc(&body[1 + used..])?;
    Ok((affected, insert_id))
}

/// Parsed ERR packet: (code, message).
pub fn parse_err(body: &[u8]) -> Result<(u16, String), String> {
    if body.first() != Some(&0xFF) {
        return Err("not an ERR packet".to_string());
    }
    if body.len() < 3 {
        return Err("truncated ERR packet".to_string());
    }
    let code = u16::from_le_bytes([body[1], body[2]]);
    // Optional `#sqlstate` marker.
    let msg = if body.get(3) == Some(&b'#') && body.len() >= 9 {
        &body[9..]
    } else {
        &body[3..]
    };
    Ok((code, String::from_utf8_lossy(msg).into_owned()))
}

/// True when the body is an EOF packet (0xFE with length < 9).
pub fn is_eof(body: &[u8]) -> bool {
    body.first() == Some(&0xFE) && body.len() < 9
}

// ---------------------------------------------------------------------------
// COM_* builders
// ---------------------------------------------------------------------------

/// `COM_STMT_PREPARE`.
pub fn build_prepare(sql: &str) -> Vec<u8> {
    let mut body = vec![COM_STMT_PREPARE];
    body.extend_from_slice(sql.as_bytes());
    body
}

/// A bound parameter value for `COM_STMT_EXECUTE`.
#[derive(Debug, Clone)]
pub enum MyParam {
    Int(i64),
    Float(f64),
    Str(Vec<u8>),
    Bool(bool),
    Null,
}

fn param_type(p: &MyParam) -> u8 {
    match p {
        MyParam::Int(_) => TYPE_LONGLONG,
        MyParam::Float(_) => TYPE_DOUBLE,
        MyParam::Str(_) => TYPE_VAR_STRING,
        MyParam::Bool(_) => TYPE_TINY,
        MyParam::Null => TYPE_NULL,
    }
}

/// `COM_STMT_EXECUTE` with typed binary parameter values.
pub fn build_execute(stmt_id: u32, params: &[MyParam]) -> Vec<u8> {
    let mut body = vec![COM_STMT_EXECUTE];
    body.extend_from_slice(&stmt_id.to_le_bytes());
    body.push(0); // CURSOR_TYPE_NO_CURSOR
    body.extend_from_slice(&1u32.to_le_bytes()); // iteration count
    if !params.is_empty() {
        let bitmap_len = params.len().div_ceil(8);
        let mut bitmap = vec![0u8; bitmap_len];
        for (i, p) in params.iter().enumerate() {
            if matches!(p, MyParam::Null) {
                bitmap[i / 8] |= 1 << (i % 8);
            }
        }
        body.extend_from_slice(&bitmap);
        body.push(1); // new-params-bound flag
        for p in params {
            body.push(param_type(p));
            body.push(0); // unsigned flag
        }
        for p in params {
            match p {
                MyParam::Null => {}
                MyParam::Int(i) => body.extend_from_slice(&i.to_le_bytes()),
                MyParam::Float(f) => body.extend_from_slice(&f.to_le_bytes()),
                MyParam::Bool(b) => body.push(u8::from(*b)),
                MyParam::Str(s) => {
                    let mut len = Vec::new();
                    write_lenenc(s.len() as u64, &mut len);
                    body.extend_from_slice(&len);
                    body.extend_from_slice(s);
                }
            }
        }
    }
    body
}

/// `COM_QUIT`.
pub fn build_quit() -> Vec<u8> {
    vec![COM_QUIT]
}

// ---------------------------------------------------------------------------
// Resultset parsing
// ---------------------------------------------------------------------------

/// Parsed `PREPARE` response header.
#[derive(Debug)]
pub struct PrepareOk {
    pub stmt_id: u32,
    pub columns: u16,
    pub params: u16,
}

/// Parse a `COM_STMT_PREPARE` response body.
pub fn parse_prepare_ok(body: &[u8]) -> Result<PrepareOk, String> {
    if body.first() != Some(&0x00) || body.len() < 12 {
        return Err("invalid PREPARE response".to_string());
    }
    Ok(PrepareOk {
        stmt_id: u32::from_le_bytes(body[1..5].try_into().unwrap()),
        columns: u16::from_le_bytes(body[5..7].try_into().unwrap()),
        params: u16::from_le_bytes(body[7..9].try_into().unwrap()),
    })
}

/// One `ColumnDefinition41`. All metadata is retained (only `ftype`
/// drives ZZ mapping today; the rest aids debugging and future
/// field-name matching).
#[allow(dead_code)]
#[derive(Debug, Clone)]
pub struct ColDef {
    pub name: String,
    pub charset: u16,
    pub col_len: u32,
    pub ftype: u8,
    pub flags: u16,
    pub decimals: u8,
}

/// Parse a `ColumnDefinition41` body.
pub fn parse_coldef(body: &[u8]) -> Result<ColDef, String> {
    let mut pos = 0;
    // catalog, schema, table, org_table (skipped).
    for _ in 0..4 {
        let (_, used) = read_lenenc_bytes(&body[pos..])?;
        pos += used;
    }
    let (name_bytes, used) = read_lenenc_bytes(&body[pos..])?;
    pos += used;
    let name = String::from_utf8(name_bytes.unwrap_or_default()).unwrap_or_default();
    let (_, used) = read_lenenc_bytes(&body[pos..])?; // org_name
    pos += used;
    let (fixed_len, used) = read_lenenc(&body[pos..])?;
    pos += used;
    if fixed_len != 0x0c || body.len() < pos + 12 {
        return Err("invalid column definition tail".to_string());
    }
    let charset = u16::from_le_bytes([body[pos], body[pos + 1]]);
    let col_len = u32::from_le_bytes(body[pos + 2..pos + 6].try_into().unwrap());
    let ftype = body[pos + 6];
    let flags = u16::from_le_bytes([body[pos + 7], body[pos + 8]]);
    let decimals = body[pos + 9];
    Ok(ColDef {
        name,
        charset,
        col_len,
        ftype,
        flags,
        decimals,
    })
}

/// A decoded binary-protocol cell: raw bytes plus signedness hint.
/// `None` = SQL NULL.
pub type Cell = Option<Vec<u8>>;

/// Parse one binary-protocol row given the column types.
pub fn parse_binary_row(body: &[u8], types: &[u8]) -> Result<Vec<Cell>, String> {
    if body.first() != Some(&0x00) {
        return Err("not a binary row".to_string());
    }
    let n = types.len();
    let bitmap_len = (n + 9) / 8;
    if body.len() < 1 + bitmap_len {
        return Err("truncated binary row".to_string());
    }
    let bitmap = &body[1..1 + bitmap_len];
    let mut pos = 1 + bitmap_len;
    let mut out = Vec::with_capacity(n);
    for (i, ty) in types.iter().enumerate() {
        if bitmap[(i + 2) / 8] & (1 << ((i + 2) % 8)) != 0 {
            out.push(None);
            continue;
        }
        let (val, used) = parse_binary_value(&body[pos..], *ty)?;
        pos += used;
        out.push(Some(val));
    }
    Ok(out)
}

/// Parse one binary value; returns (raw bytes, consumed).
/// Dates/times decode to `YYYY-MM-DD[ HH:MM:SS[.ffffff]]` text.
fn parse_binary_value(data: &[u8], ty: u8) -> Result<(Vec<u8>, usize), String> {
    let need = |n: usize| -> Result<(), String> {
        if data.len() < n {
            return Err("truncated binary value".to_string());
        }
        Ok(())
    };
    match ty {
        TYPE_TINY => {
            need(1)?;
            Ok((data[..1].to_vec(), 1))
        }
        TYPE_SHORT | TYPE_YEAR => {
            need(2)?;
            Ok((data[..2].to_vec(), 2))
        }
        TYPE_LONG | TYPE_INT24 | TYPE_FLOAT => {
            need(4)?;
            Ok((data[..4].to_vec(), 4))
        }
        TYPE_LONGLONG | TYPE_DOUBLE => {
            need(8)?;
            Ok((data[..8].to_vec(), 8))
        }
        TYPE_DATE | TYPE_DATETIME | TYPE_TIMESTAMP => {
            need(1)?;
            let len = data[0] as usize;
            need(1 + len)?;
            Ok((decode_mysql_datetime(&data[1..1 + len]), 1 + len))
        }
        TYPE_TIME => {
            need(1)?;
            let len = data[0] as usize;
            need(1 + len)?;
            Ok((decode_mysql_time(&data[1..1 + len]), 1 + len))
        }
        _ => {
            // Length-encoded strings, decimals, blobs, bit, geometry...
            let (v, used) = read_lenenc_bytes(data)?;
            Ok((v.unwrap_or_default(), used))
        }
    }
}

fn u16le(b: &[u8]) -> u16 {
    u16::from_le_bytes([b[0], b[1]])
}

fn decode_mysql_datetime(b: &[u8]) -> Vec<u8> {
    // year(2) month day [hour min sec [micro(4)]]; short reads degrade
    // gracefully instead of panicking on malformed servers.
    if b.len() < 4 {
        return b"0000-00-00".to_vec();
    }
    let (y, m, d) = (u16le(&b[0..2]), b[2], b[3]);
    if b.len() < 7 {
        return format!("{y:04}-{m:02}-{d:02}").into_bytes();
    }
    let (hh, mm, ss) = (b[4], b[5], b[6]);
    if b.len() < 11 {
        return format!("{y:04}-{m:02}-{d:02} {hh:02}:{mm:02}:{ss:02}").into_bytes();
    }
    let micro = u32::from_le_bytes([b[7], b[8], b[9], b[10]]);
    format!("{y:04}-{m:02}-{d:02} {hh:02}:{mm:02}:{ss:02}.{micro:06}").into_bytes()
}

fn decode_mysql_time(b: &[u8]) -> Vec<u8> {
    // is_negative(1) days(4) [hh mm ss [micro(4)]]
    if b.len() < 5 {
        return b"00:00:00".to_vec();
    }
    let neg = if b[0] != 0 { "-" } else { "" };
    let days = u32::from_le_bytes([b[1], b[2], b[3], b[4]]);
    if b.len() < 8 {
        return format!("{neg}{days} 00:00:00").into_bytes();
    }
    let (hh, mm, ss) = (b[5], b[6], b[7]);
    if b.len() < 12 {
        return format!("{neg}{days} {hh:02}:{mm:02}:{ss:02}").into_bytes();
    }
    let micro = u32::from_le_bytes([b[8], b[9], b[10], b[11]]);
    format!("{neg}{days} {hh:02}:{mm:02}:{ss:02}.{micro:06}").into_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sha1_vectors() {
        assert_eq!(
            hex_lower(&sha1(b"")),
            "da39a3ee5e6b4b0d3255bfef95601890afd80709"
        );
        assert_eq!(
            hex_lower(&sha1(b"abc")),
            "a9993e364706816aba3e25717850c26c9cd0d89d"
        );
    }

    #[test]
    fn native_password_vector() {
        // Independently computed with Python hashlib.
        let seed: Vec<u8> = (1..=20).collect();
        assert_eq!(
            hex_lower(&native_token(b"password", &seed)),
            "c17d6009a5cb47e59f7483fcf05553bbbf7dd0d6"
        );
        assert!(native_token(b"", &seed).is_empty());
    }

    #[test]
    fn caching_sha2_vector() {
        // Independently computed with Python hashlib; matches the documented
        // XOR(SHA256(pw), SHA256(SHA256(SHA256(pw)), seed)) formula.
        let seed: Vec<u8> = (1..=32).collect();
        assert_eq!(
            hex_lower(&caching_token(b"password", &seed)),
            "de18457639c5c90a828d6815a8d20d8d9bd839d56d9f6a116a5496fd068dccd0"
        );
        assert!(caching_token(b"", &seed).is_empty());
    }

    #[test]
    fn frame_roundtrip() {
        let body = b"hello";
        let f = frame(3, body);
        assert_eq!(&f[..4], &[5, 0, 0, 3]);
        let (msgs, rest) = split_frames(&f);
        assert_eq!(msgs, vec![body.to_vec()]);
        assert!(rest.is_empty());
        // Incomplete tail stays buffered.
        let (msgs2, rest2) = split_frames(&f[..f.len() - 2]);
        assert!(msgs2.is_empty());
        assert_eq!(rest2.len(), f.len() - 2);
    }

    #[test]
    fn lenenc_roundtrip() {
        for v in [0u64, 250, 251, 1000, 65535, 65536, 1 << 40] {
            let mut out = Vec::new();
            write_lenenc(v, &mut out);
            let (back, _) = read_lenenc(&out).unwrap();
            assert_eq!(back, v, "lenenc {v}");
        }
        let (none, _) = read_lenenc_bytes(&[0xFB]).unwrap();
        assert_eq!(none, None);
    }

    #[test]
    fn handshake_parse() {
        // Crafted HandshakeV10: plugin mysql_native_password, seed 8+12.
        let mut body = vec![10u8];
        body.extend_from_slice(b"8.0.36-mock\0");
        body.extend_from_slice(&7u32.to_le_bytes());
        body.extend_from_slice(b"abcdefgh");
        body.push(0);
        let caps: u32 = 0x0008_8207;
        body.extend_from_slice(&(caps as u16).to_le_bytes());
        body.push(45);
        body.extend_from_slice(&2u16.to_le_bytes());
        body.extend_from_slice(&((caps >> 16) as u16).to_le_bytes());
        body.push(21);
        body.extend_from_slice(&[0u8; 10]);
        body.extend_from_slice(b"ijklmnopqrst");
        body.push(0);
        body.extend_from_slice(b"mysql_native_password\0");
        let h = parse_handshake(&body).unwrap();
        assert_eq!(h.protocol, 10);
        assert_eq!(h.server_version, "8.0.36-mock");
        assert_eq!(h.connection_id, 7);
        assert_eq!(h.seed, b"abcdefghijklmnopqrst".to_vec());
        assert_eq!(h.auth_plugin, "mysql_native_password");
    }

    #[test]
    fn handshake_response_shape() {
        let r = build_handshake_response("u", b"token", Some("db"), "mysql_native_password");
        // caps + max + charset + 23 reserved + "u\0" + len + token + "db\0" + plugin\0
        assert_eq!(&r[..4], &(CLIENT_CAPS.to_le_bytes()));
        assert!(r.windows(8).any(|w| w == b"u\0\x05token"));
        assert!(r.ends_with(b"mysql_native_password\0"));
    }

    #[test]
    fn auth_switch_parse() {
        let mut body = vec![0xFE];
        body.extend_from_slice(b"caching_sha2_password\0");
        body.extend_from_slice(b"seedbytes0123456789\0");
        let (plugin, seed) = parse_auth_switch(&body).unwrap();
        assert_eq!(plugin, "caching_sha2_password");
        assert_eq!(seed, b"seedbytes0123456789");
    }

    #[test]
    fn ok_err_eof_packets() {
        let ok = [0x00u8, 0x03, 0x00, 0x02, 0x00, 0x00, 0x00];
        assert_eq!(parse_ok(&ok).unwrap(), (3, 0));
        let mut err = vec![0xFF, 0x28, 0x04];
        err.extend_from_slice(b"#28000Access denied");
        assert_eq!(parse_err(&err).unwrap().0, 0x0428);
        assert!(is_eof(&[0xFE, 0, 0, 0, 0]));
        assert!(!is_eof(&ok));
    }

    #[test]
    fn prepare_ok_parse() {
        let body = [0x00, 7, 0, 0, 0, 2, 0, 1, 0, 0, 0, 0];
        let p = parse_prepare_ok(&body).unwrap();
        assert_eq!((p.stmt_id, p.columns, p.params), (7, 2, 1));
    }

    #[test]
    fn binary_row_parse() {
        // Row (LONG 7, "carol"): header + 1 null-bitmap byte + values.
        let mut body = vec![0x00u8, 0x00];
        body.extend_from_slice(&7i32.to_le_bytes());
        body.push(5);
        body.extend_from_slice(b"carol");
        let row = parse_binary_row(&body, &[TYPE_LONG, TYPE_VAR_STRING]).unwrap();
        assert_eq!(row[0], Some(7i32.to_le_bytes().to_vec()));
        assert_eq!(row[1], Some(b"carol".to_vec()));
        // Row (NULL, "dave"): bit (0+2) set.
        let mut body2 = vec![0x00u8, 0x04];
        body2.push(4);
        body2.extend_from_slice(b"dave");
        let row2 = parse_binary_row(&body2, &[TYPE_LONG, TYPE_VAR_STRING]).unwrap();
        assert_eq!(row2[0], None);
        assert_eq!(row2[1], Some(b"dave".to_vec()));
    }

    #[test]
    fn datetime_decode() {
        // DATETIME 2024-05-06 07:08:09 (payload without length prefix).
        let mut full = vec![7u8];
        full.extend_from_slice(&2024u16.to_le_bytes());
        full.extend_from_slice(&[5, 6, 7, 8, 9]);
        assert_eq!(decode_mysql_datetime(&full[1..]), b"2024-05-06 07:08:09");
        // DATE only.
        let mut d = vec![4u8];
        d.extend_from_slice(&2024u16.to_le_bytes());
        d.extend_from_slice(&[1, 2]);
        assert_eq!(decode_mysql_datetime(&d[1..]), b"2024-01-02");
    }

    #[test]
    fn execute_encoding() {
        let e = build_execute(
            7,
            &[MyParam::Int(7), MyParam::Str(b"x".to_vec()), MyParam::Null],
        );
        assert_eq!(e[0], COM_STMT_EXECUTE);
        assert_eq!(&e[1..5], &7u32.to_le_bytes());
        // Null-bitmap bit 2 set for the third param.
        let bitmap_at = 1 + 4 + 1 + 4;
        assert_eq!(e[bitmap_at], 0x04);
    }
}
