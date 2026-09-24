//! Pure-Rust PostgreSQL wire protocol v3.0 engine — zero dependencies.
//!
//! Two halves, both `std`-only:
//! - [`frontend`]: binary builders for frontend packets (`StartupMessage`,
//!   `Parse`, `Bind`, `Describe`, `Execute`, `Sync`, password / SASL
//!   responses, `Terminate`).
//! - [`backend`]: zero-copy parser for backend messages (`Authentication`,
//!   `RowDescription`, `DataRow`, `CommandComplete`, `ReadyForQuery`,
//!   `ErrorResponse`, ...).
//!
//! Crypto needed by auth (MD5, SHA-256, HMAC, PBKDF2, SCRAM-SHA-256) is
//! implemented in [`sasl`] from first principles so the driver stays
//! dependency-free.

use std::collections::HashMap;

// ---------------------------------------------------------------------------
// MD5 (RFC 1321)
// ---------------------------------------------------------------------------

const MD5_S: [u32; 64] = [
    7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 5, 9, 14, 20, 5, 9, 14, 20, 5, 9,
    14, 20, 5, 9, 14, 20, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 6, 10, 15,
    21, 6, 10, 15, 21, 6, 10, 15, 21, 6, 10, 15, 21,
];

const MD5_K: [u32; 64] = [
    0xd76aa478, 0xe8c7b756, 0x242070db, 0xc1bdceee, 0xf57c0faf, 0x4787c62a, 0xa8304613, 0xfd469501,
    0x698098d8, 0x8b44f7af, 0xffff5bb1, 0x895cd7be, 0x6b901122, 0xfd987193, 0xa679438e, 0x49b40821,
    0xf61e2562, 0xc040b340, 0x265e5a51, 0xe9b6c7aa, 0xd62f105d, 0x02441453, 0xd8a1e681, 0xe7d3fbc8,
    0x21e1cde6, 0xc33707d6, 0xf4d50d87, 0x455a14ed, 0xa9e3e905, 0xfcefa3f8, 0x676f02d9, 0x8d2a4c8a,
    0xfffa3942, 0x8771f681, 0x6d9d6122, 0xfde5380c, 0xa4beea44, 0x4bdecfa9, 0xf6bb4b60, 0xbebfbc70,
    0x289b7ec6, 0xeaa127fa, 0xd4ef3085, 0x04881d05, 0xd9d4d039, 0xe6db99e5, 0x1fa27cf8, 0xc4ac5665,
    0xf4292244, 0x432aff97, 0xab9423a7, 0xfc93a039, 0x655b59c3, 0x8f0ccc92, 0xffeff47d, 0x85845dd1,
    0x6fa87e4f, 0xfe2ce6e0, 0xa3014314, 0x4e0811a1, 0xf7537e82, 0xbd3af235, 0x2ad7d2bb, 0xeb86d391,
];

/// Raw MD5 digest of `input`.
pub fn md5(input: &[u8]) -> [u8; 16] {
    let mut msg = input.to_vec();
    let bit_len = (input.len() as u64).wrapping_mul(8);
    msg.push(0x80);
    while msg.len() % 64 != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&bit_len.to_le_bytes());

    let mut a0: u32 = 0x67452301;
    let mut b0: u32 = 0xefcdab89;
    let mut c0: u32 = 0x98badcfe;
    let mut d0: u32 = 0x10325476;

    for chunk in msg.as_chunks::<64>().0 {
        let mut m = [0u32; 16];
        for (i, w) in m.iter_mut().enumerate() {
            *w = u32::from_le_bytes([
                chunk[4 * i],
                chunk[4 * i + 1],
                chunk[4 * i + 2],
                chunk[4 * i + 3],
            ]);
        }
        let (mut a, mut b, mut c, mut d) = (a0, b0, c0, d0);
        for i in 0..64 {
            let (f, g) = match i {
                0..16 => ((b & c) | ((!b) & d), i),
                16..32 => ((d & b) | ((!d) & c), (5 * i + 1) % 16),
                32..48 => (b ^ c ^ d, (3 * i + 5) % 16),
                _ => (c ^ (b | (!d)), (7 * i) % 16),
            };
            let tmp = d;
            d = c;
            c = b;
            b = b.wrapping_add(
                a.wrapping_add(f)
                    .wrapping_add(MD5_K[i])
                    .wrapping_add(m[g])
                    .rotate_left(MD5_S[i]),
            );
            a = tmp;
        }
        a0 = a0.wrapping_add(a);
        b0 = b0.wrapping_add(b);
        c0 = c0.wrapping_add(c);
        d0 = d0.wrapping_add(d);
    }

    let mut out = [0u8; 16];
    out[0..4].copy_from_slice(&a0.to_le_bytes());
    out[4..8].copy_from_slice(&b0.to_le_bytes());
    out[8..12].copy_from_slice(&c0.to_le_bytes());
    out[12..16].copy_from_slice(&d0.to_le_bytes());
    out
}

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
// SHA-256 (FIPS 180-4)
// ---------------------------------------------------------------------------

const SHA256_K: [u32; 64] = [
    0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5,
    0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174,
    0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da,
    0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967,
    0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85,
    0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
    0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
    0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
];

const SHA256_H: [u32; 8] = [
    0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19,
];

/// Raw SHA-256 digest of `input`.
pub fn sha256(input: &[u8]) -> [u8; 32] {
    let mut msg = input.to_vec();
    let bit_len = (input.len() as u64).wrapping_mul(8);
    msg.push(0x80);
    while msg.len() % 64 != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&bit_len.to_be_bytes());

    let mut h = SHA256_H;
    let mut w = [0u32; 64];
    for chunk in msg.as_chunks::<64>().0 {
        for i in 0..16 {
            w[i] = u32::from_be_bytes([
                chunk[4 * i],
                chunk[4 * i + 1],
                chunk[4 * i + 2],
                chunk[4 * i + 3],
            ]);
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16]
                .wrapping_add(s0)
                .wrapping_add(w[i - 7])
                .wrapping_add(s1);
        }
        let (mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut hh) =
            (h[0], h[1], h[2], h[3], h[4], h[5], h[6], h[7]);
        for i in 0..64 {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let ch = (e & f) ^ ((!e) & g);
            let t1 = hh
                .wrapping_add(s1)
                .wrapping_add(ch)
                .wrapping_add(SHA256_K[i])
                .wrapping_add(w[i]);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let maj = (a & b) ^ (a & c) ^ (b & c);
            let t2 = s0.wrapping_add(maj);
            hh = g;
            g = f;
            f = e;
            e = d.wrapping_add(t1);
            d = c;
            c = b;
            b = a;
            a = t1.wrapping_add(t2);
        }
        h[0] = h[0].wrapping_add(a);
        h[1] = h[1].wrapping_add(b);
        h[2] = h[2].wrapping_add(c);
        h[3] = h[3].wrapping_add(d);
        h[4] = h[4].wrapping_add(e);
        h[5] = h[5].wrapping_add(f);
        h[6] = h[6].wrapping_add(g);
        h[7] = h[7].wrapping_add(hh);
    }

    let mut out = [0u8; 32];
    for (i, v) in h.iter().enumerate() {
        out[4 * i..4 * i + 4].copy_from_slice(&v.to_be_bytes());
    }
    out
}

/// HMAC-SHA-256.
pub fn hmac_sha256(key: &[u8], msg: &[u8]) -> [u8; 32] {
    let mut k = [0u8; 64];
    if key.len() > 64 {
        let d = sha256(key);
        k[..32].copy_from_slice(&d);
    } else {
        k[..key.len()].copy_from_slice(key);
    }
    let mut ipad = [0x36u8; 64];
    let mut opad = [0x5cu8; 64];
    for i in 0..64 {
        ipad[i] ^= k[i];
        opad[i] ^= k[i];
    }
    let mut inner = ipad.to_vec();
    inner.extend_from_slice(msg);
    let inner_hash = sha256(&inner);
    let mut outer = opad.to_vec();
    outer.extend_from_slice(&inner_hash);
    sha256(&outer)
}

/// PBKDF2-HMAC-SHA-256, single purpose-built for SCRAM iteration counts.
pub fn pbkdf2_sha256(password: &[u8], salt: &[u8], iterations: u32) -> [u8; 32] {
    // SaltedPassword = U1 ^ U2 ^ ... ^ Uc, U1 = HMAC(pw, salt || 0x00000001).
    let mut block = salt.to_vec();
    block.extend_from_slice(&[0, 0, 0, 1]);
    let mut u = hmac_sha256(password, &block);
    let mut acc = u;
    for _ in 1..iterations {
        u = hmac_sha256(password, &u);
        for i in 0..32 {
            acc[i] ^= u[i];
        }
    }
    acc
}

// ---------------------------------------------------------------------------
// Base64 (hand-rolled, no deps)
// ---------------------------------------------------------------------------

const B64_ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

pub fn b64_encode(input: &[u8]) -> String {
    let mut out = String::with_capacity(input.len().div_ceil(3) * 4);
    for chunk in input.chunks(3) {
        let mut n: u32 = 0;
        for (i, b) in chunk.iter().enumerate() {
            n |= (*b as u32) << (16 - 8 * i);
        }
        let pad = 3 - chunk.len();
        for i in 0..4 - pad {
            out.push(B64_ALPHABET[((n >> (18 - 6 * i)) & 63) as usize] as char);
        }
        for _ in 0..pad {
            out.push('=');
        }
    }
    out
}

pub fn b64_decode(input: &str) -> Result<Vec<u8>, String> {
    let mut vals: Vec<u8> = Vec::with_capacity(input.len());
    let mut pad = 0usize;
    for c in input.bytes() {
        if c == b'=' {
            pad += 1;
            vals.push(0);
            continue;
        }
        let v = B64_ALPHABET
            .iter()
            .position(|&a| a == c)
            .ok_or_else(|| format!("invalid base64 character `{c}`"))?;
        vals.push(v as u8);
    }
    if !vals.len().is_multiple_of(4) {
        return Err("invalid base64 length".to_string());
    }
    let mut out = Vec::with_capacity(vals.len() / 4 * 3);
    for chunk in vals.chunks(4) {
        let n = ((chunk[0] as u32) << 18)
            | ((chunk[1] as u32) << 12)
            | ((chunk[2] as u32) << 6)
            | (chunk[3] as u32);
        out.push((n >> 16) as u8);
        out.push((n >> 8) as u8);
        out.push(n as u8);
    }
    for _ in 0..pad.min(2) {
        out.pop();
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// Auth helpers
// ---------------------------------------------------------------------------

/// PostgreSQL MD5 password response: `md5` + hex(md5(hex(md5(pw+user)) + salt)).
pub fn pg_md5_password(user: &str, password: &str, salt: [u8; 4]) -> String {
    let mut inner_input = Vec::with_capacity(password.len() + user.len());
    inner_input.extend_from_slice(password.as_bytes());
    inner_input.extend_from_slice(user.as_bytes());
    let inner_hex = hex_lower(&md5(&inner_input));
    let mut outer_input = inner_hex.into_bytes();
    outer_input.extend_from_slice(&salt);
    format!("md5{}", hex_lower(&md5(&outer_input)))
}

/// SCRAM-SHA-256 client state machine (RFC 7677, client side only).
pub struct ScramClient {
    pub client_nonce: String,
    client_first_bare: String,
    server_first: String,
    salted_password: Option<[u8; 32]>,
}

impl ScramClient {
    /// Begin auth: returns the `client-first-message` (with `n,,` header).
    pub fn begin(user: &str, nonce: String) -> (Self, String) {
        let bare = format!("n={user},r={nonce}");
        let full = format!("n,,{bare}");
        (
            Self {
                client_nonce: nonce,
                client_first_bare: bare,
                server_first: String::new(),
                salted_password: None,
            },
            full,
        )
    }

    /// Consume the server-first message; returns the `client-final-message`.
    pub fn step_server_first(
        &mut self,
        server_first: &str,
        password: &str,
    ) -> Result<String, String> {
        // `r=<nonce...>,s=<salt>,i=<iters>[,...]`
        let mut nonce = None;
        let mut salt_b64 = None;
        let mut iters = None;
        for part in server_first.split(',') {
            if let Some(v) = part.strip_prefix("r=") {
                nonce = Some(v);
            } else if let Some(v) = part.strip_prefix("s=") {
                salt_b64 = Some(v);
            } else if let Some(v) = part.strip_prefix("i=") {
                iters = Some(v);
            }
        }
        let nonce = nonce.ok_or_else(|| "SCRAM: server-first missing nonce".to_string())?;
        if !nonce.starts_with(&self.client_nonce) {
            return Err("SCRAM: server nonce does not extend client nonce".to_string());
        }
        let salt =
            b64_decode(salt_b64.ok_or_else(|| "SCRAM: server-first missing salt".to_string())?)?;
        let iters: u32 = iters
            .ok_or_else(|| "SCRAM: server-first missing iteration count".to_string())?
            .parse()
            .map_err(|_| "SCRAM: bad iteration count".to_string())?;
        if iters == 0 {
            return Err("SCRAM: iteration count must be > 0".to_string());
        }
        self.server_first = server_first.to_string();
        let salted = pbkdf2_sha256(password.as_bytes(), &salt, iters);
        self.salted_password = Some(salted);

        let client_key = hmac_sha256(&salted, b"Client Key");
        let stored_key = sha256(&client_key);
        // channel binding `n,,` -> `c=biws`
        let cfinal_wo_proof = format!("c=biws,r={nonce}");
        let auth_msg = format!(
            "{},{},{}",
            self.client_first_bare, self.server_first, cfinal_wo_proof
        );
        let sig = hmac_sha256(&stored_key, auth_msg.as_bytes());
        let mut proof = [0u8; 32];
        for i in 0..32 {
            proof[i] = client_key[i] ^ sig[i];
        }
        Ok(format!("{cfinal_wo_proof},p={}", b64_encode(&proof)))
    }

    /// Verify the server-final message (`v=<sig>`); returns Ok on success.
    pub fn verify_server_final(&self, server_final: &str, password: &str) -> Result<(), String> {
        let salted = self
            .salted_password
            .ok_or_else(|| "SCRAM: no handshake in progress".to_string())?;
        // Recompute expected server signature over the same AuthMessage.
        let server_key = hmac_sha256(&salted, b"Server Key");
        // AuthMessage needs the client-final-without-proof; recompute it.
        // server_final = `v=<sig>`; the nonce echo is inside server_first.
        let nonce = self
            .server_first
            .split(',')
            .find_map(|p| p.strip_prefix("r="))
            .ok_or_else(|| "SCRAM: malformed handshake state".to_string())?;
        let cfinal_wo_proof = format!("c=biws,r={nonce}");
        let auth_msg = format!(
            "{},{},{}",
            self.client_first_bare, self.server_first, cfinal_wo_proof
        );
        let expected = hmac_sha256(&server_key, auth_msg.as_bytes());
        let got_b64 = server_final
            .strip_prefix("v=")
            .ok_or_else(|| "SCRAM: expected server signature `v=...`".to_string())?;
        let got = b64_decode(got_b64)?;
        if got.len() == expected.len() && got.iter().zip(expected.iter()).all(|(a, b)| a == b) {
            let _ = password;
            Ok(())
        } else {
            Err("SCRAM: server signature mismatch".to_string())
        }
    }
}

// ---------------------------------------------------------------------------
// Frontend message builders
// ---------------------------------------------------------------------------

fn be_i16(v: i16, out: &mut Vec<u8>) {
    out.extend_from_slice(&v.to_be_bytes());
}

fn be_i32(v: i32, out: &mut Vec<u8>) {
    out.extend_from_slice(&v.to_be_bytes());
}

fn cstr(s: &str, out: &mut Vec<u8>) {
    out.extend_from_slice(s.as_bytes());
    out.push(0);
}

/// Frame a typed frontend message: `type-byte + len(i32 incl. self) + body`.
fn frame(ty: u8, body: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(5 + body.len());
    out.push(ty);
    out.extend_from_slice(&((body.len() + 4) as i32).to_be_bytes());
    out.extend_from_slice(body);
    out
}

/// `StartupMessage`: NO type byte. Protocol 3.0 = 196608.
pub fn build_startup(user: &str, database: &str) -> Vec<u8> {
    let mut body = Vec::new();
    be_i32(196608, &mut body);
    cstr("user", &mut body);
    cstr(user, &mut body);
    cstr("database", &mut body);
    cstr(database, &mut body);
    cstr("application_name", &mut body);
    cstr("zz", &mut body);
    body.push(0);
    let mut out = Vec::with_capacity(4 + body.len());
    be_i32((body.len() + 4) as i32, &mut out);
    out.extend_from_slice(&body);
    out
}

/// Password / SASL-response message (`p`).
pub fn build_password_response(secret: &[u8]) -> Vec<u8> {
    frame(b'p', secret)
}

/// Initial SASL response: `p` + `mechanism\0` + `msglen(i32)` + `msg`.
pub fn build_sasl_initial(mechanism: &str, client_first: &str) -> Vec<u8> {
    let mut body = Vec::new();
    cstr(mechanism, &mut body);
    be_i32(client_first.len() as i32, &mut body);
    body.extend_from_slice(client_first.as_bytes());
    frame(b'p', &body)
}

/// Simple-query message (`Q`). The driver prefers the extended protocol
/// (parameterization + row descriptions); kept for protocol completeness.
#[allow(dead_code)]
pub fn build_simple_query(sql: &str) -> Vec<u8> {
    let mut body = Vec::new();
    cstr(sql, &mut body);
    frame(b'Q', &body)
}

/// `Parse`: unnamed or named prepared statement. Empty `param_oids`
/// lets the server infer parameter types from context.
pub fn build_parse(stmt_name: &str, sql: &str, param_oids: &[u32]) -> Vec<u8> {
    let mut body = Vec::new();
    cstr(stmt_name, &mut body);
    cstr(sql, &mut body);
    be_i16(param_oids.len() as i16, &mut body);
    for oid in param_oids {
        out_oid(*oid, &mut body);
    }
    frame(b'P', &body)
}

fn out_oid(oid: u32, out: &mut Vec<u8>) {
    out.extend_from_slice(&oid.to_be_bytes());
}

/// `Bind`: `None` value = SQL NULL (-1 length). All values sent in text
/// format; results requested in text format.
pub fn build_bind(portal: &str, stmt: &str, values: &[Option<&[u8]>]) -> Vec<u8> {
    let mut body = Vec::new();
    cstr(portal, &mut body);
    cstr(stmt, &mut body);
    // Param formats: 0 entries = all text.
    be_i16(0, &mut body);
    be_i16(values.len() as i16, &mut body);
    for v in values {
        match v {
            None => be_i32(-1, &mut body),
            Some(bytes) => {
                be_i32(bytes.len() as i32, &mut body);
                body.extend_from_slice(bytes);
            }
        }
    }
    // Result formats: 0 entries = all text.
    be_i16(0, &mut body);
    frame(b'B', &body)
}

/// `Describe`: `kind` is `b'S'` (statement) or `b'P'` (portal).
pub fn build_describe(kind: u8, name: &str) -> Vec<u8> {
    let mut body = vec![kind];
    cstr(name, &mut body);
    frame(b'D', &body)
}

/// `Execute`: `max_rows = 0` means unlimited.
pub fn build_execute(portal: &str, max_rows: i32) -> Vec<u8> {
    let mut body = Vec::new();
    cstr(portal, &mut body);
    be_i32(max_rows, &mut body);
    frame(b'E', &body)
}

/// `Sync`.
pub fn build_sync() -> Vec<u8> {
    frame(b'S', &[])
}

/// `Terminate`.
pub fn build_terminate() -> Vec<u8> {
    frame(b'X', &[])
}

// ---------------------------------------------------------------------------
// Backend message parser
// ---------------------------------------------------------------------------

/// PostgreSQL type OIDs we map to ZZ scalars (see `col_oid_kind`).
pub const OID_INT2: u32 = 21;
pub const OID_INT4: u32 = 23;
pub const OID_INT8: u32 = 20;
pub const OID_FLOAT4: u32 = 700;
pub const OID_FLOAT8: u32 = 701;
pub const OID_BOOL: u32 = 16;
pub const OID_NUMERIC: u32 = 1700;

/// Coarse kind of a column type for ZZ value mapping.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColKind {
    Int,
    Float,
    Bool,
    Text,
}

/// Map a PostgreSQL type OID to a [`ColKind`]. Unknown OIDs arrive as text.
pub fn col_oid_kind(oid: u32) -> ColKind {
    match oid {
        OID_INT2 | OID_INT4 | OID_INT8 => ColKind::Int,
        OID_FLOAT4 | OID_FLOAT8 | OID_NUMERIC => ColKind::Float,
        OID_BOOL => ColKind::Bool,
        _ => ColKind::Text,
    }
}

/// One `RowDescription` column. All metadata is retained (only
/// `type_oid` drives ZZ mapping today; the rest aids debugging and
/// future struct-field-name matching).
#[allow(dead_code)]
#[derive(Debug, Clone)]
pub struct ColDesc {
    pub name: String,
    pub table_oid: u32,
    pub attr_no: i16,
    pub type_oid: u32,
    pub type_len: i16,
    pub type_mod: i32,
    pub format: i16,
}

/// Backend messages we act on. Notices / parameter statuses are surfaced
/// as data; everything else needed for the handshake and extended query
/// flow is represented explicitly. Payloads not consumed by the current
/// driver (e.g. `Ready` status byte) are retained for completeness.
#[allow(dead_code)]
#[derive(Debug)]
pub enum BackendMsg {
    AuthOk,
    AuthCleartext,
    AuthMd5([u8; 4]),
    AuthSasl(Vec<String>),
    AuthSaslContinue(Vec<u8>),
    AuthSaslFinal(Vec<u8>),
    ParamStatus(String, String),
    BackendKey(u32, u32),
    Ready(u8),
    ParseComplete,
    BindComplete,
    CloseComplete,
    ParamDesc(Vec<u32>),
    NoData,
    EmptyQuery,
    RowDesc(Vec<ColDesc>),
    DataRow(Vec<Option<Vec<u8>>>),
    CommandComplete(String),
    ErrorMsg(Vec<(u8, String)>),
    Notice(Vec<(u8, String)>),
    NegotiateProto(u32),
}

struct Cursor<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> Cursor<'a> {
    fn new(buf: &'a [u8]) -> Self {
        Self { buf, pos: 0 }
    }

    fn rest_len(&self) -> usize {
        self.buf.len().saturating_sub(self.pos)
    }

    fn take(&mut self, n: usize) -> Result<&'a [u8], String> {
        if self.rest_len() < n {
            return Err(format!("backend message truncated (need {n} bytes)"));
        }
        let s = &self.buf[self.pos..self.pos + n];
        self.pos += n;
        Ok(s)
    }

    fn u8(&mut self) -> Result<u8, String> {
        Ok(self.take(1)?[0])
    }

    fn i16(&mut self) -> Result<i16, String> {
        let b = self.take(2)?;
        Ok(i16::from_be_bytes([b[0], b[1]]))
    }

    fn i32(&mut self) -> Result<i32, String> {
        let b = self.take(4)?;
        Ok(i32::from_be_bytes([b[0], b[1], b[2], b[3]]))
    }

    fn u32(&mut self) -> Result<u32, String> {
        let b = self.take(4)?;
        Ok(u32::from_be_bytes([b[0], b[1], b[2], b[3]]))
    }

    fn cstr(&mut self) -> Result<&'a str, String> {
        let rel = self.buf[self.pos..]
            .iter()
            .position(|&b| b == 0)
            .ok_or_else(|| "backend message missing NUL terminator".to_string())?;
        let s = std::str::from_utf8(&self.buf[self.pos..self.pos + rel])
            .map_err(|_| "backend message has invalid UTF-8".to_string())?;
        self.pos += rel + 1;
        Ok(s)
    }
}

fn parse_fields(cur: &mut Cursor) -> Result<Vec<(u8, String)>, String> {
    let mut out = Vec::new();
    loop {
        let code = cur.u8()?;
        if code == 0 {
            break;
        }
        let val = cur.cstr()?.to_string();
        out.push((code, val));
    }
    Ok(out)
}

/// Human-readable rendering of an `ErrorResponse` / `Notice` field list
/// (message `M`, code `C`, detail `D` when present).
pub fn fields_message(fields: &[(u8, String)]) -> String {
    let get = |code: u8| {
        fields
            .iter()
            .find(|(c, _)| *c == code)
            .map(|(_, v)| v.clone())
    };
    let msg = get(b'M').unwrap_or_else(|| "unknown error".to_string());
    match (get(b'C'), get(b'D')) {
        (Some(code), Some(detail)) => format!("{msg} (code {code}): {detail}"),
        (Some(code), None) => format!("{msg} (code {code})"),
        (None, _) => msg,
    }
}

/// Parse one backend message body (header type byte + length already split).
pub fn parse_backend_msg(ty: u8, body: &[u8]) -> Result<BackendMsg, String> {
    let mut cur = Cursor::new(body);
    let msg = match ty {
        b'R' => {
            let kind = cur.i32()?;
            match kind {
                0 => BackendMsg::AuthOk,
                3 => BackendMsg::AuthCleartext,
                5 => {
                    let s = cur.take(4)?;
                    BackendMsg::AuthMd5([s[0], s[1], s[2], s[3]])
                }
                10 => {
                    let mut mechs = Vec::new();
                    loop {
                        let m = cur.cstr()?.to_string();
                        if m.is_empty() {
                            break;
                        }
                        mechs.push(m);
                    }
                    BackendMsg::AuthSasl(mechs)
                }
                11 => BackendMsg::AuthSaslContinue(cur.buf[cur.pos..].to_vec()),
                12 => BackendMsg::AuthSaslFinal(cur.buf[cur.pos..].to_vec()),
                other => return Err(format!("unsupported authentication type {other}")),
            }
        }
        b'S' => {
            let name = cur.cstr()?.to_string();
            let value = cur.cstr()?.to_string();
            BackendMsg::ParamStatus(name, value)
        }
        b'K' => BackendMsg::BackendKey(cur.u32()?, cur.u32()?),
        b'Z' => BackendMsg::Ready(cur.u8()?),
        b'1' => BackendMsg::ParseComplete,
        b'2' => BackendMsg::BindComplete,
        b'3' => BackendMsg::CloseComplete,
        b't' => {
            let n = cur.i16()?;
            if n < 0 {
                return Err("negative parameter count".to_string());
            }
            let mut oids = Vec::with_capacity(n as usize);
            for _ in 0..n {
                oids.push(cur.u32()?);
            }
            BackendMsg::ParamDesc(oids)
        }
        b'n' => BackendMsg::NoData,
        b'I' => BackendMsg::EmptyQuery,
        b'T' => {
            let n = cur.i16()?;
            if n < 0 {
                return Err("negative column count".to_string());
            }
            let mut cols = Vec::with_capacity(n as usize);
            for _ in 0..n {
                cols.push(ColDesc {
                    name: cur.cstr()?.to_string(),
                    table_oid: cur.u32()?,
                    attr_no: cur.i16()?,
                    type_oid: cur.u32()?,
                    type_len: cur.i16()?,
                    type_mod: cur.i32()?,
                    format: cur.i16()?,
                });
            }
            BackendMsg::RowDesc(cols)
        }
        b'D' => {
            let n = cur.i16()?;
            if n < 0 {
                return Err("negative column count in DataRow".to_string());
            }
            let mut cols = Vec::with_capacity(n as usize);
            for _ in 0..n {
                let len = cur.i32()?;
                if len < 0 {
                    cols.push(None);
                } else {
                    cols.push(Some(cur.take(len as usize)?.to_vec()));
                }
            }
            BackendMsg::DataRow(cols)
        }
        b'C' => BackendMsg::CommandComplete(cur.cstr()?.to_string()),
        b'E' => BackendMsg::ErrorMsg(parse_fields(&mut cur)?),
        b'N' => BackendMsg::Notice(parse_fields(&mut cur)?),
        b'v' => BackendMsg::NegotiateProto(cur.u32()?),
        b'W' | b'H' | b'G' => {
            return Err("COPY protocol is not supported by this driver".to_string());
        }
        other => return Err(format!("unexpected backend message `{other}`")),
    };
    Ok(msg)
}

/// Framed backend messages plus trailing incomplete bytes.
pub type Framed = (Vec<(u8, Vec<u8>)>, Vec<u8>);

/// Split a raw read buffer into framed messages: `(type, body)` pairs.
/// Returns the parsed messages plus any trailing incomplete bytes.
/// (The blocking driver reads exact frames instead; this helper serves
/// buffered transports and the mock-server tests.)
#[allow(dead_code)]
pub fn split_frames(buf: &[u8]) -> Result<Framed, String> {
    let mut out = Vec::new();
    let mut pos = 0;
    while pos < buf.len() {
        if buf.len() - pos < 5 {
            break;
        }
        let ty = buf[pos];
        let len =
            i32::from_be_bytes([buf[pos + 1], buf[pos + 2], buf[pos + 3], buf[pos + 4]]) as usize;
        if len < 4 {
            return Err("backend message length out of range".to_string());
        }
        if buf.len() - pos - 1 < len {
            break;
        }
        out.push((ty, buf[pos + 5..pos + 1 + len].to_vec()));
        pos += 1 + len;
    }
    Ok((out, buf[pos..].to_vec()))
}

/// Parse a `CommandComplete` tag into affected-row count.
/// `INSERT 0 3` → 3, `UPDATE 5` → 5, `SELECT 1` → 1, `CREATE TABLE` → 0.
pub fn command_tag_count(tag: &str) -> i64 {
    tag.split_whitespace()
        .next_back()
        .and_then(|n| n.parse::<i64>().ok())
        .unwrap_or(0)
}

/// Collected server parameters (`ParameterStatus`) by name.
pub type ServerParams = HashMap<String, String>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn md5_vectors() {
        assert_eq!(hex_lower(&md5(b"")), "d41d8cd98f00b204e9800998ecf8427e");
        assert_eq!(hex_lower(&md5(b"abc")), "900150983cd24fb0d6963f7d28e17f72");
        assert_eq!(
            hex_lower(&md5(b"The quick brown fox jumps over the lazy dog")),
            "9e107d9d372bb6826bd81d3542a419d6"
        );
    }

    #[test]
    fn sha256_vectors() {
        assert_eq!(
            hex_lower(&sha256(b"")),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            hex_lower(&sha256(b"abc")),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn hmac_rfc4231_case1() {
        let key = [0x0bu8; 20];
        let mac = hmac_sha256(&key, b"Hi There");
        assert_eq!(
            hex_lower(&mac),
            "b0344c61d8db38535ca8afceaf0bf12b881dc200c9833da726e9376c2e32cff7"
        );
    }

    #[test]
    fn pbkdf2_rfc7914_vector() {
        let dk = pbkdf2_sha256(b"password", b"salt", 1);
        assert_eq!(
            hex_lower(&dk),
            "120fb6cffcf8b32c43e7225256c4f837a86548c92ccc35480805987cb70be17b"
        );
    }

    #[test]
    fn pg_md5_password_vector() {
        // Independently computed with Python hashlib.
        assert_eq!(
            pg_md5_password("zzuser", "zzpass", [0x01, 0x02, 0x03, 0x04]),
            "md549d242afc19a88c62361cd294b85320e"
        );
    }

    #[test]
    fn scram_fixed_vector() {
        // Independently computed with Python hmac/hashlib/pbkdf2_hmac:
        // user=zzuser, password=zzpass, client nonce=testnonce12345678,
        // server nonce=testnonce12345678SERVERPART, salt=bytes(1..=16), i=4096.
        let server_first = "r=testnonce12345678SERVERPART,s=AQIDBAUGBwgJCgsMDQ4PEA==,i=4096";
        let (mut client, first) = ScramClient::begin("zzuser", "testnonce12345678".to_string());
        assert_eq!(first, "n,,n=zzuser,r=testnonce12345678");
        let final_msg = client.step_server_first(server_first, "zzpass").unwrap();
        assert_eq!(
            final_msg,
            "c=biws,r=testnonce12345678SERVERPART,\
             p=J8N61ZJDFQ3z9TuF/OQfI3mqLBxCLodap5j8BfEPF8s="
        );
        client
            .verify_server_final("v=yot0m055PtbtIA4GKLkmYx4tzAo9hK67sESGFAiyG0Q=", "zzpass")
            .unwrap();
        assert!(client
            .verify_server_final("v=AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=", "zzpass")
            .is_err());
    }

    #[test]
    fn scram_rejects_nonce_mismatch() {
        let (mut client, _) = ScramClient::begin("u", "abc".to_string());
        assert!(client
            .step_server_first("r=xyz,s=AQ==,i=4096", "pw")
            .is_err());
    }

    #[test]
    fn b64_roundtrip() {
        for data in [
            &b""[..],
            b"f",
            b"fo",
            b"foo",
            b"foobar",
            &[0, 1, 2, 254, 255],
        ] {
            assert_eq!(b64_decode(&b64_encode(data)).unwrap(), data);
        }
        assert_eq!(b64_encode(b"n,,"), "biws");
        assert!(b64_decode("!!!").is_err());
    }

    #[test]
    fn startup_message_shape() {
        let msg = build_startup("u", "d");
        let len = i32::from_be_bytes([msg[0], msg[1], msg[2], msg[3]]) as usize;
        assert_eq!(len, msg.len());
        assert_eq!(&msg[4..8], &196608i32.to_be_bytes());
        assert!(msg.ends_with(&[0, 0]));
        let body = String::from_utf8_lossy(&msg[8..]);
        assert!(body.contains("user\0u\0"));
        assert!(body.contains("database\0d\0"));
    }

    #[test]
    fn parse_bind_execute_framing() {
        let p = build_parse("", "SELECT $1", &[]);
        assert_eq!(p[0], b'P');
        let len = i32::from_be_bytes([p[1], p[2], p[3], p[4]]) as usize;
        assert_eq!(len + 1, p.len());

        let b = build_bind("", "", &[Some(b"7"), None]);
        assert_eq!(b[0], b'B');
        // NULL param encodes as -1 length.
        assert!(b.windows(4).any(|w| w == (-1i32).to_be_bytes()));

        assert_eq!(build_sync(), vec![b'S', 0, 0, 0, 4]);
        assert_eq!(build_terminate(), vec![b'X', 0, 0, 0, 4]);
    }

    #[test]
    fn parse_auth_ok_and_error() {
        assert!(matches!(
            parse_backend_msg(b'R', &0i32.to_be_bytes()).unwrap(),
            BackendMsg::AuthOk
        ));
        let mut body = 5i32.to_be_bytes().to_vec();
        body.extend_from_slice(&[1, 2, 3, 4]);
        assert!(matches!(
            parse_backend_msg(b'R', &body).unwrap(),
            BackendMsg::AuthMd5([1, 2, 3, 4])
        ));
        // ErrorResponse: fields + NUL terminator.
        let mut e = vec![b'M'];
        e.extend_from_slice(b"oops\0C28000\0\0");
        match parse_backend_msg(b'E', &e).unwrap() {
            BackendMsg::ErrorMsg(fs) => {
                assert_eq!(fields_message(&fs), "oops (code 28000)");
            }
            _ => panic!("expected error"),
        }
    }

    #[test]
    fn parse_row_desc_and_data_row() {
        // RowDescription with (id int4, name text).
        let mut body = 2i16.to_be_bytes().to_vec();
        for (name, oid) in [("id", 23u32), ("name", 25u32)] {
            body.extend_from_slice(name.as_bytes());
            body.push(0);
            body.extend_from_slice(&0u32.to_be_bytes());
            body.extend_from_slice(&0i16.to_be_bytes());
            body.extend_from_slice(&oid.to_be_bytes());
            body.extend_from_slice(&4i16.to_be_bytes());
            body.extend_from_slice(&(-1i32).to_be_bytes());
            body.extend_from_slice(&0i16.to_be_bytes());
        }
        match parse_backend_msg(b'T', &body).unwrap() {
            BackendMsg::RowDesc(cols) => {
                assert_eq!(cols.len(), 2);
                assert_eq!(cols[0].type_oid, 23);
                assert_eq!(col_oid_kind(cols[0].type_oid), ColKind::Int);
                assert_eq!(cols[1].name, "name");
            }
            _ => panic!("expected rowdesc"),
        }
        // DataRow: ("42", NULL).
        let mut d = 2i16.to_be_bytes().to_vec();
        d.extend_from_slice(&2i32.to_be_bytes());
        d.extend_from_slice(b"42");
        d.extend_from_slice(&(-1i32).to_be_bytes());
        match parse_backend_msg(b'D', &d).unwrap() {
            BackendMsg::DataRow(cols) => {
                assert_eq!(cols[0], Some(b"42".to_vec()));
                assert_eq!(cols[1], None);
            }
            _ => panic!("expected datarow"),
        }
    }

    #[test]
    fn command_tag_counts() {
        assert_eq!(command_tag_count("SELECT 1"), 1);
        assert_eq!(command_tag_count("INSERT 0 3"), 3);
        assert_eq!(command_tag_count("UPDATE 5"), 5);
        assert_eq!(command_tag_count("DELETE 12"), 12);
        assert_eq!(command_tag_count("CREATE TABLE"), 0);
        assert_eq!(command_tag_count("BEGIN"), 0);
    }

    #[test]
    fn split_frames_roundtrip() {
        let mut buf = build_sync();
        buf.extend(build_sync());
        buf.extend(build_terminate());
        let (msgs, rest) = split_frames(&buf).unwrap();
        assert_eq!(msgs.len(), 3);
        assert!(rest.is_empty());
        // Incomplete tail stays buffered.
        let partial = &buf[..buf.len() - 2];
        let (msgs2, rest2) = split_frames(partial).unwrap();
        assert_eq!(msgs2.len(), 2);
        assert_eq!(rest2.len(), 3);
    }
}
