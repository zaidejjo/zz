# `std.sys` — system information

Infallible platform queries for worker sizing, diagnostics headers, and
environment introspection.

```zz
import std.sys

println(sys.os())        // "linux"
println(sys.arch())      // "x86_64"
println(sys.hostname())  // machine name
println(sys.cpu_count()) // logical CPUs
println(sys.total_mem()) // bytes
println(sys.avail_mem()) // bytes available
```

## Types

None.

## Variables

None.

## Functions

| Function | Signature | Notes |
|---|---|---|
| `os()` | `os() -> str` | Platform: `linux`, `macos`, or `windows`. |
| `arch()` | `arch() -> str` | CPU architecture. |
| `hostname()` | `hostname() -> str` | Machine hostname. |
| `cpu_count()` | `cpu_count() -> int` | Logical CPU count. |
| `total_mem()` | `total_mem() -> int` | Total RAM in bytes. |
| `avail_mem()` | `avail_mem() -> int` | Available RAM in bytes. |
