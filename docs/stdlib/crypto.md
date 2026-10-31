# `std.crypto` — cryptography primitives

Hashing, symmetric/asymmetric signing, password hashing, JWT, and
constant-time comparison. All operations return `Result` where failure is
possible, and raw strings otherwise.

> ⚠️ Security note: review key sizes, algorithms, and padding for your
> threat model before using in production. `std.crypto` exposes primitives;
> it does not design your protocol.

```zz
import std.crypto

match crypto.sha256("hello") {
    .ok(h) => println("sha256: ", h),
    .err(e) => println(e),
}

match crypto.bcrypt_hash("hunter2") {
    .ok(hash) => println(crypto.bcrypt_verify(hash, "hunter2")), // true
    .err(e) => println(e),
}
```

## Types

None (opaque strings for keys, hashes, signatures, tokens).

## Variables

None.

## Functions

| Function | Signature | Notes |
|---|---|---|
| `sha256(s)` | `sha256(s: str) -> str` | SHA-256 hex digest of `s`. |
| `sha256_bytes(b)` | `sha256_bytes(b: bytes) -> str` | SHA-256 hex digest of raw `bytes`. |
| `sha512(s)` | `sha512(s: str) -> str` | SHA-512 hex digest of `s`. |
| `hmac_sha256(key, msg)` | `hmac_sha256(key: str, msg: str) -> str` | HMAC-SHA256 hex MAC. |
| `random_bytes(n)` | `random_bytes(n: int) -> str` | `n` cryptographically random bytes (as a string). |
| `bcrypt_hash(pw)` | `bcrypt_hash(pw: str) -> str` | bcrypt hash of a password. |
| `bcrypt_verify(hash, pw)` | `bcrypt_verify(hash: str, pw: str) -> bool` | Constant-time password verification. |
| `argon2_hash(pw)` | `argon2_hash(pw: str) -> str` | Argon2id hash of a password. |
| `argon2_verify(hash, pw)` | `argon2_verify(hash: str, pw: str) -> bool` | Argon2id password verification. |
| `ed25519_keypair()` | `ed25519_keypair() -> [str]` | `[public_key, secret_key]` as strings. |
| `ed25519_sign(sk, msg)` | `ed25519_sign(sk: str, msg: str) -> Result<str, str>` | Sign `msg` with secret key `sk`. |
| `ed25519_verify(pk, msg, sig)` | `ed25519_verify(pk: str, msg: str, sig: str) -> bool` | Verify an Ed25519 signature. |
| `rsa_keypair()` | `rsa_keypair() -> Result<[str], str>` | `[public_key, secret_key]` PEM strings. |
| `rsa_sign(sk, msg)` | `rsa_sign(sk: str, msg: str) -> Result<str, str>` | RSA sign `msg`. |
| `rsa_verify(pk, msg, sig)` | `rsa_verify(pk: str, msg: str, sig: str) -> bool` | RSA verify. |
| `jwt_encode(payload, secret)` | `jwt_encode(payload: str, secret: str) -> str` | HS256 JWT. |
| `jwt_encode_ed(payload, sk)` | `jwt_encode_ed(payload: str, sk: str) -> Result<str, str>` | EdDSA JWT. |
| `jwt_decode(token, secret)` | `jwt_decode(token: str, secret: str) -> Result<str, str>` | Decode + verify HS256 JWT. |
| `jwt_decode_ed(token, pk)` | `jwt_decode_ed(token: str, pk: str) -> Result<str, str>` | Decode + verify EdDSA JWT. |
| `ct_eq(a, b)` | `ct_eq(a: str, b: str) -> bool` | Constant-time string comparison (timing-safe). |
