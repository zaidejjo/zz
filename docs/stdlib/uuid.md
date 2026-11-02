# UUID — `std.uuid`

v4/v7 generation, parse (normalizing), validation. `v7` is time-ordered — good for request IDs and DB keys.

```zz
import std.uuid
```

## Purpose

`std.uuid` generates UUID v4 (random) and v7 (time-ordered), parses and normalizes UUID strings, and validates shape. v7 sorts roughly by creation time — prefer it for IDs appearing in logs and indexes.

## Quickstart

```zz
import std.uuid

println(uuid.v4())     // random, e.g. "f47ac10b-58cc-4372-a567-0e02b2c3d479"
println(uuid.v7())     // time-ordered (good for request IDs, DB keys)
s := uuid.v4()
println(uuid.parse(s))   // normalized or .err
println(uuid.is_valid(s))  // bool
```

## Exhaustive reference

| Function | Signature | Description | Example |
|----------|-----------|-------------|---------|
| `uuid.v4` | `uuid.v4() -> str` | Random UUID v4 | |
| `uuid.v7` | `uuid.v7() -> str` | Time-ordered UUID v7 | |
| `uuid.parse` | `uuid.parse(s: str) -> Result<str, str>` | Normalize or `.err` | |
| `uuid.is_valid` | `uuid.is_valid(s: str) -> bool` | Shape check | |

## Edge cases & gotchas

- **`v7` sorts roughly by creation time** — prefer for logs/indexes; not cryptographically unguessable like v4.
- **Validate external input** with `is_valid` before `parse().unwrap()`.
- **`parse` normalizes** (case, hyphens) on success.
- **v4 is random** — suitable for unguessable identifiers; v7 is time-ordered — suitable for sorted storage.

## Production patterns

```zz
import std.uuid

func main() -> Result<int, str> {
    // Request ID for tracing
    req_id := uuid.v7()   // time-ordered, sortable in logs
    println(req_id)

    // External ID validation
    external := "f47ac10b-58cc-4372-a567-0e02b2c3d479"
    if uuid.is_valid(external) {
        id := uuid.parse(external)?
        println(id)
    }
    return .ok(0)
}
```

