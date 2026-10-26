# Encoding and crypto — `std.encoding`, `std.crypto`, `std.uuid`

## `std.encoding`

```zz
import std.encoding

encoding.base64_encode("hi")          // "aGk="
encoding.base64_decode("aGk=")        // .ok("hi")
encoding.base64_decode_bytes("aGk=")  // .ok(bytes) — preserves raw bytes
encoding.hex_encode("hi")             // "6869"
encoding.hex_decode("6869")           // .ok("hi"), odd-length is .err
encoding.url_encode("a b?c")          // percent-encoded
encoding.url_decode("a%20b")          // .ok("a b")
```

`*_bytes` variants exist because binary payloads cannot round-trip through UTF-8
`str`. Decode failures are `Result.err` with a message, never panics.

## `std.crypto` — digests are lowercase hex

```zz
import std.crypto
import std.encoding

crypto.sha256("hi")
crypto.sha512("hi")
crypto.hmac_sha256("key", "msg")
crypto.random_bytes(16)         // hex of n CSPRNG bytes
a := "s1"
println(crypto.ct_eq(a, "s1"))  // true, constant-time compare
match encoding.base64_decode_bytes("aGk=") {
    .ok(b)  => println(crypto.sha256_bytes(b)),  // bytes input, same hex out
    .err(e) => println(e),
}
```

Passwords (argon2/bcrypt) and signatures (ed25519/RSA, JWT):

```zz
h := crypto.argon2_hash("pw")
crypto.argon2_verify(h, "pw")   // true
crypto.bcrypt_hash("pw")
crypto.bcrypt_verify(h, "pw")

kp := crypto.ed25519_keypair()          // [str] handle parts
sig := crypto.ed25519_sign(kp, "msg")
crypto.ed25519_verify(kp, "msg", sig)

tok := crypto.jwt_encode("secret", `{"sub":"1"}`)
crypto.jwt_decode("secret", tok)        // .ok(payload) / .err
crypto.jwt_encode_ed(kp, `{"sub":"1"}`)
crypto.jwt_decode_ed(kp, tok)
```

Random uses OS CSPRNG; `random_bytes` hex length is `2*n`. Compare secrets with
`ct_eq`, never `==`.

## `std.uuid`

```zz
import std.uuid

println(uuid.v4())     // random
println(uuid.v7())     // time-ordered (good for request IDs, DB keys)
s := uuid.v4()
println(uuid.parse(s))   // normalized or .err
println(uuid.is_valid(s))  // bool
```

`v7` sorts roughly by creation time — prefer it for IDs that appear in logs
and indexes. Validate external input with `is_valid` before `parse().unwrap()`.

See [stdlib.md](../stdlib.md) for exact arities.
