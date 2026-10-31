# `std.encoding` — text encodings

Base64 and hex encode/decode, plus URL encoding. All decode operations
return `Result<bytes|str, str>` and fail on invalid input.

```zz
import std.encoding

match encoding.base64_encode("hello") {
    .ok(enc) => println(enc),       // "aGVsbG8="
    .err(e) => println(e),
}
match encoding.base64_decode("aGVsbG8=") {
    .ok(dec) => println(dec),       // "hello"
    .err(e) => println(e),
}
println(encoding.hex_encode("zz"))  // "7a7a"
println(encoding.url_encode("a b")) // "a%20b"
```

## Types

None (works on `str` / `bytes`).

## Variables

None.

## Functions

| Function | Signature | Notes |
|---|---|---|
| `base64_encode(data)` | `base64_encode(data: str) -> str` | Base64-encode a string. |
| `base64_decode(encoded)` | `base64_decode(encoded: str) -> Result<str, str>` | Base64-decode to a string. |
| `base64_decode_bytes(encoded)` | `base64_decode_bytes(encoded: str) -> Result<bytes, str>` | Base64-decode to raw `bytes`. |
| `hex_encode(data)` | `hex_encode(data: str) -> str` | Hex-encode (lowercase). |
| `hex_decode(encoded)` | `hex_decode(encoded: str) -> Result<str, str>` | Hex-decode to a string. |
| `url_encode(data)` | `url_encode(data: str) -> str` | Percent-encode a string. |
| `url_decode(encoded)` | `url_decode(encoded: str) -> Result<str, str>` | Percent-decode a string. |
