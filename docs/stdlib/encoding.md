# Encoding — `std.encoding`

Base64, hex, and URL percent-encoding codecs. `*_bytes` variants exist because binary payloads cannot round-trip through UTF-8 `str`. Decode failures are `Result.err` — never panics.

```zz
import std.encoding
```

## Purpose

`std.encoding` encodes/decodes base64, hex, and URL percent-encoding. Text variants operate on `str`; `_bytes` variants operate on `bytes` for binary-safe round-trips. Use for data transport, embedding binary in text, and URL query building.

## Quickstart

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

## Exhaustive reference

| Function | Signature | Description | Example |
|----------|-----------|-------------|---------|
| `encoding.base64_encode` | `encoding.base64_encode(data: str) -> str` | Base64 encode | `base64_encode("hi")` → `"aGk="` |
| `encoding.base64_decode` | `encoding.base64_decode(encoded: str) -> Result<str, str>` | Base64 decode to str | `base64_decode("aGk=")` → `.ok("hi")` |
| `encoding.base64_decode_bytes` | `encoding.base64_decode_bytes(encoded: str) -> Result<bytes, str>` | Base64 decode to bytes | `base64_decode_bytes("aGk=")` → `.ok(bytes)` |
| `encoding.hex_encode` | `encoding.hex_encode(data: str) -> str` | Hex encode | `hex_encode("hi")` → `"6869"` |
| `encoding.hex_decode` | `encoding.hex_decode(encoded: str) -> Result<str, str>` | Hex decode (odd-length → `.err`) | |
| `encoding.url_encode` | `encoding.url_encode(data: str) -> str` | Percent-encode | `url_encode("a b?c")` |
| `encoding.url_decode` | `encoding.url_decode(encoded: str) -> Result<str, str>` | Percent-decode | `url_decode("a%20b")` → `.ok("a b")` |

## Error model

- All decode ops return `Result<_, str>` — invalid input is `.err`, **never panics**.
- `hex_decode` rejects odd-length input with `.err`.
- Text variants require valid UTF-8; use `_bytes` variants for arbitrary binary.

## Gotchas

- **`base64_decode` returns `str`** — use `base64_decode_bytes` when the payload is binary (ciphertext, images).
- **`hex_decode` odd-length → `.err`.**
- **URL encoding uses standard percent-encoding** (space → `%20`, not `+`).
- Pair `crypto.sha256_bytes` with `base64_decode_bytes` for bytes-input hashing.

## Production patterns

```zz
import std.encoding
import std.crypto

func main() -> Result<int, str> {
    // Encode binary ciphertext for text transport
    ciphertext_b64 := "aGk="
    match encoding.base64_decode_bytes(ciphertext_b64) {
        .ok(raw) => println(crypto.sha256_bytes(raw)),
        .err(e) => println(e),
    }

    // Build a URL query
    query := encoding.url_encode("q=hello world&lang=en")
    println(query)
    return .ok(0)
}
```

