# `std.uuid` — UUID generation and validation

Generate UUID v4 (random) and v7 (time-ordered), and validate UUID strings.

```zz
import std.uuid

println(uuid.v4())   // e.g. "f47ac10b-58cc-4372-a567-0e02b2c3d479"
println(uuid.v7())   // time-ordered, sortable
println(uuid.is_valid("f47ac10b-58cc-4372-a567-0e02b2c3d479")) // true
```

## Types

None (UUIDs are `str`).

## Variables

None.

## Functions

| Function | Signature | Notes |
|---|---|---|
| `v4()` | `v4() -> str` | Random UUID v4 string. |
| `v7()` | `v7() -> str` | Time-ordered UUID v7 string. |
| `is_valid(s)` | `is_valid(s: str) -> bool` | Whether `s` is a well-formed UUID. |
| `parse(s)` | `parse(s: str) -> Result<str, str>` | Parse/normalize a UUID string. |
