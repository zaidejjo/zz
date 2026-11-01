# Sys — `std.sys`

Infallible machine information: platform identity, CPU count, memory, hostname. All queries return fallback values (empty string / 0) — never `.err`.

```zz
import std.sys
```

## Purpose

`std.sys` reports platform identity and resources. Use `cpu_count` for `task.spawn` fan-out, `total_mem`/`avail_mem` for cache sizing and diagnostics headers.

## Quickstart

```zz
import std.sys

println(sys.os())         // linux / macos / windows
println(sys.arch())       // x86_64 / aarch64 / …
println(sys.cpu_count())  // logical cores — size your worker pool
println(sys.hostname())
println(sys.total_mem())  // bytes
println(sys.avail_mem())  // bytes
```

## Exhaustive reference

| Function | Signature | Description | Example |
|----------|-----------|-------------|---------|
| `sys.os` | `sys.os() -> str` | `"linux"` / `"macos"` / `"windows"` | |
| `sys.arch` | `sys.arch() -> str` | `"x86_64"` / `"aarch64"` / … | |
| `sys.cpu_count` | `sys.cpu_count() -> int` | Logical cores | |
| `sys.hostname` | `sys.hostname() -> str` | Host name | |
| `sys.total_mem` | `sys.total_mem() -> int` | Total RAM, bytes | |
| `sys.avail_mem` | `sys.avail_mem() -> int` | Available RAM, bytes | |

## Error model

All queries are infallible — empty string / 0 on unknown, never `.err`.

## Gotchas

- **`cpu_count` is logical cores** — size worker pools accordingly, not 1:1 with physical cores.
- **Memory values are bytes** — divide by 1024/1024 for MiB.
- **Fallbacks** on unknown platforms — don't assert specific values in tests.

## Production patterns

```zz
import std.sys
import std.task

// Size a worker pool to logical cores
workers := sys.cpu_count()
for i in 0..workers {
    task.spawn(|_| { /* work */ })
}

// Cache sizing from available memory
cache_mb := sys.avail_mem() / (1024 * 1024) / 4   // quarter of avail mem
```

