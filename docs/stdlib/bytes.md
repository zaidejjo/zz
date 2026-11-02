# Bytes — `std.bytes`

Contiguous, immutable byte buffers (~1 byte RSS per byte; slices share the store with zero copies). Prints like an int array (`[104, 105]`). JSON-serializes as int arrays. Deep `==` equality. The binary-safe counterpart to `std.str`.

```zz
import std.bytes
```

## Purpose

`std.bytes` holds arbitrary binary data safely — no UTF-8 validation, no lossy decoding. Produced by `std.fs.read_bytes`, `fs.read_chunk_bytes`, `fs.read_bytes_at`, and `bytes.finish`. Use for file uploads, ciphertext, binary protocols, and any payload that isn't valid UTF-8.

`bytes` is immutable (`b[i] = x` is an error). Slices (`b[1:4]`) share the underlying store with zero copies.

## Quickstart

```zz
import std.bytes
import std.fs

match fs.read_bytes("data.bin") {
    .ok(b) => {
        println(len(b))      // or b.len()
        println(b[0])        // u8 as int (negatives wrap)
        println(b[1:4])      // zero-copy slice -> bytes
        println(typeof(b))   // bytes
        for x in b { println(x) }   // iterate ints
    }
    .err(e) => println(e),
}

// Build binary incrementally
buf := bytes.builder()
buf = bytes.push_byte(buf, 0x48)  // 'H'
buf = bytes.push_byte(buf, 0x65)  // 'e'
match bytes.finish(buf) {
    .ok(b) => println(b),         // "He"
    .err(e) => println(e),
}
```

## Method syntax

| Method | Signature | Description |
|--------|-----------|-------------|
| `b.len()` | `bytes.len(b: bytes) -> int` | Buffer length |
| `b.to_ints()` | `bytes.to_ints(b: bytes) -> [int]` | Opaque buffer as plain ints |

```zz
import std.bytes

func main() -> Result<int, str> {
    b := bytes.finish([72, 101, 108, 108, 111])?   // "Hello"
    println(b.len())   // 5
    println(bytes.to_ints(b)) // [72, 101, 108, 108, 111]
    return .ok(0)
}
```

## Exhaustive reference

### Construction & conversion

| Function | Signature | Description | Example |
|----------|-----------|-------------|---------|
| `bytes.to_ints` | `bytes.to_ints(b: bytes) -> [int]` | Opaque buffer as plain ints | `bytes.to_ints(b)[0]` |
| `bytes.to_str` | `bytes.to_str(vs: [int]) -> Result<str, str>` | Strict UTF-8 decode from ints | `bytes.to_str([104,105])` → `.ok("hi")` |
| `bytes.finish` | `bytes.finish(vs: [int]) -> Result<bytes, str>` | Build `bytes` from ints (range-checked) | `bytes.finish([104,105])` → `.ok(bytes)` |
| `bytes.from_ints` | `bytes.from_ints(vs: [int]) -> [int]` | Normalize int array | `bytes.from_ints([1,2])` |

> `bytes.finish` is fallible (range-checked 0–255) — unlike `str.finish` — because binary assembly must never silently clamp.

### Builder (incremental assembly)

| Function | Signature | Description | Example |
|----------|-----------|-------------|---------|
| `bytes.builder` | `bytes.builder() -> [int]` | Empty byte builder handle | `bytes.builder()` |
| `bytes.push_byte` | `bytes.push_byte(b: [int], v: int) -> [int]` | Append byte (mod 256, negatives wrap) | `bytes.push_byte(b, 300)` → 44 |
| `bytes.extend` | `bytes.extend(b: [int], vs: [int]) -> [int]` | Append many ints | `bytes.extend(b, [1,2,3])` |
| `bytes.len_of` | `bytes.len_of(vs: [int]) -> int` | Total size without joining | `bytes.len_of(b)` |

### Info

| Function | Signature | Description | Example |
|----------|-----------|-------------|---------|
| `bytes.len` | `bytes.len(b: bytes) -> int` | Buffer length | `len(b)` / `b.len()` |

## Edge cases & gotchas

- **Immutable:** `b[i] = x` is an error. Use `bytes.builder` / `push_byte` / `finish` to mutate, then re-bind.
- **`push_byte` wraps mod 256.** `push_byte(b, 300)` stores `44`; negatives wrap too.
- **`finish` range-checks.** Values outside `0–255` yield `.err`.
- **Slices share storage.** `b[1:4]` is zero-copy — mutating the original through a builder invalidates nothing (immutable), but be aware slices keep the whole buffer alive.
- **`bytes` prints as `[104, 105]`** — not as text. Use `bytes.to_str` for text interpretation after validating UTF-8.
- **Deep `==`:** `b1 == b2` compares byte-by-byte on both VM and AOT.

## Performance notes

- Slices are zero-copy (share the store).
- `bytes.builder` + `push_byte` + `finish` is O(n); never concatenate binary in a loop.
- `read_bytes` caches content in the process-wide FS cache (see [`std.fs`](fs.md)).
- Immutable — safe to share across tasks without locking.

## Production patterns

```zz
import std.bytes
import std.crypto
import std.encoding

// Hash a binary file
match fs.read_bytes("payload.bin") {
    .ok(b) => {
        println(crypto.sha256_bytes(b))   // hex digest over raw bytes
    }
    .err(e) => println(e),
}

// Base64 round-trip preserving binary
match encoding.base64_decode_bytes("aGk=") {
    .ok(raw) => println(bytes.to_ints(raw)[0]),  // first byte as int
    .err(e) => println(e),
}
```

