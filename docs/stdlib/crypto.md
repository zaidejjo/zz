# Crypto — `std.crypto`

Digests (lowercase hex), HMAC, CSPRNG, constant-time compare, password hashing (Argon2id + bcrypt), and asymmetric signatures/JWT (ed25519 / RSA). All digests are lowercase hex strings.

```zz
import std.crypto
```

## Purpose

`std.crypto` covers hashing, MACs, random bytes, password hashing, public-key signatures, and JWT. Digests return lowercase hex; `random_bytes` returns hex of `n` CSPRNG bytes. Compare secrets with `ct_eq`, never `==`. Password hashes are PHC strings; verification is total (malformed input verifies as false).

## Conceptual model

- **Digests/MAC:** `sha256`, `sha512`, `hmac_sha256` → lowercase hex `str`.
- **Bytes-input variants:** `sha256_bytes`, `hmac_sha256` accept `bytes` directly.
- **Random:** `random_bytes(n)` → hex of `n` OS CSPRNG bytes (length `2*n`).
- **Password hashing:** Argon2id / bcrypt. Hashes are PHC strings; `*_verify` is total.
- **Asymmetric:** ed25519 / RSA keypairs, sign, verify. Signing is fallible (malformed keys); verification is total.
- **JWT:** HS256 (`jwt_encode`/`jwt_decode`) and EdDSA (`jwt_encode_ed`/`jwt_decode_ed`).

## Quickstart

```zz
import std.crypto
import std.encoding

func main() -> Result<int, str> {
    // Digests
    crypto.sha256("hi")                 // lowercase hex
    crypto.sha512("hi")
    crypto.hmac_sha256("key", "msg")

    // Bytes input (binary-safe)
    match encoding.base64_decode_bytes("aGk=") {
        .ok(b)  => println(crypto.sha256_bytes(b)),   // same hex out
        .err(e) => println(e),
    }

    // Random + constant-time compare
    crypto.random_bytes(16)             // hex of 16 CSPRNG bytes (32 chars)
    println(crypto.ct_eq("s1", "s1"))   // true — never use == for secrets

    // Passwords
    h := crypto.argon2_hash("pw")
    crypto.argon2_verify(h, "pw")       // true
    crypto.bcrypt_hash("pw")
    crypto.bcrypt_verify(h, "pw")

    // Asymmetric signatures
    kp := crypto.ed25519_keypair()          // [secret, public]
    sig := crypto.ed25519_sign(kp[0], "msg")?
    crypto.ed25519_verify(kp[1], "msg", sig)

    // JWT
    tok := crypto.jwt_encode(`{"sub":"1"}`, "secret")
    _ := crypto.jwt_decode(tok, "secret")?   // .ok(payload) / .err
    _ := crypto.jwt_encode_ed(kp[0], `{"sub":"1"}`)?
    _ := crypto.jwt_decode_ed(kp[1], tok)?

    return .ok(0)
}
```

## Exhaustive reference

### Digests & MAC

| Function | Signature | Description | Example |
|----------|-----------|-------------|---------|
| `crypto.sha256` | `crypto.sha256(s: str) -> str` | SHA-256 lowercase hex | |
| `crypto.sha256_bytes` | `crypto.sha256_bytes(b: bytes) -> str` | SHA-256 over raw bytes | |
| `crypto.sha512` | `crypto.sha512(s: str) -> str` | SHA-512 lowercase hex | |
| `crypto.hmac_sha256` | `crypto.hmac_sha256(key: str, msg: str) -> str` | HMAC-SHA256 lowercase hex | |

### CSPRNG & compare

| Function | Signature | Description | Example |
|----------|-----------|-------------|---------|
| `crypto.random_bytes` | `crypto.random_bytes(n: int) -> str` | Hex of `n` OS CSPRNG bytes | `random_bytes(16)` → 32 hex chars |
| `crypto.ct_eq` | `crypto.ct_eq(a: str, b: str) -> bool` | Constant-time compare | `ct_eq("s1","s1")` → true |

### Password hashing

| Function | Signature | Description | Example |
|----------|-----------|-------------|---------|
| `crypto.argon2_hash` | `crypto.argon2_hash(pw: str) -> str` | Argon2id PHC hash | |
| `crypto.argon2_verify` | `crypto.argon2_verify(hash: str, pw: str) -> bool` | Verify (total, malformed → false) | |
| `crypto.bcrypt_hash` | `crypto.bcrypt_hash(pw: str) -> str` | bcrypt hash | |
| `crypto.bcrypt_verify` | `crypto.bcrypt_verify(hash: str, pw: str) -> bool` | Verify (total) | |

### Asymmetric signatures

| Function | Signature | Description |
|----------|-----------|-------------|
| `crypto.ed25519_keypair` | `crypto.ed25519_keypair() -> [str]` | `[secret, public]` parts |
| `crypto.ed25519_sign` | `crypto.ed25519_sign(sk: str, msg: str) -> Result<str, str>` | Sign (`.err` on bad key) |
| `crypto.ed25519_verify` | `crypto.ed25519_verify(pk: str, msg: str, sig: str) -> bool` | Verify (total) |
| `crypto.rsa_keypair` | `crypto.rsa_keypair() -> Result<[str], str>` | RSA keypair |
| `crypto.rsa_sign` | `crypto.rsa_sign(sk: str, msg: str) -> Result<str, str>` | RSA sign |
| `crypto.rsa_verify` | `crypto.rsa_verify(pk: str, msg: str, sig: str) -> bool` | RSA verify (total) |

### JWT

| Function | Signature | Description |
|----------|-----------|-------------|
| `crypto.jwt_encode` | `crypto.jwt_encode(payload: str, secret: str) -> str` | HS256 JWT |
| `crypto.jwt_decode` | `crypto.jwt_decode(token: str, secret: str) -> Result<str, str>` | Decode HS256 |
| `crypto.jwt_encode_ed` | `crypto.jwt_encode_ed(payload: str, sk: str) -> Result<str, str>` | EdDSA JWT |
| `crypto.jwt_decode_ed` | `crypto.jwt_decode_ed(token: str, pk: str) -> Result<str, str>` | Decode EdDSA |

## Error model

- **Signing** (`ed25519_sign`, `rsa_sign`, `jwt_encode_ed`) is fallible — malformed keys → `.err`.
- **Verification** (`ed25519_verify`, `rsa_verify`, `jwt_decode`) is total — returns `bool`.
- **Password verification** is total — malformed input verifies as false.
- **Digests/HMAC/random/ct_eq** are total.

## Gotchas

- **Never compare secrets with `==`** — use `ct_eq` to avoid timing side-channels.
- **`random_bytes` hex length is `2*n`.**
- **Keypairs cross as `[secret, public]` string arrays.**
- **JWT payload/secret are raw strings** — the caller is responsible for JSON shaping the payload.
- **Verification is total** — a `.err` token decodes to false, never crashes.

## Production patterns

```zz
import std.crypto

func main() -> Result<int, str> {
    // Constant-time API key check
    expected := "sk_live_..."
    client_key := "sk_live_..."
    if crypto.ct_eq(client_key, expected) {
        println("authorized")
    }

    // Password reset token
    token := crypto.jwt_encode(`{"uid":123}`, "reset-secret")
    println(token)
    return .ok(0)
}
```

## Security notes

- Argon2id is the recommended default for new password storage.
- `ct_eq` compares full strings (no early exit) — timing-independent.
- JWT `jwt_decode` does not validate `exp`/`nbf` — the caller inspects the payload JSON.

