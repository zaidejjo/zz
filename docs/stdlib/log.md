# `std.log` — leveled logging with levels and spans

Leveled logging (`trace`, `debug`, `info`, `warn`, `error`), span handles
for timed operations, and output sinks (stderr or a file).

```zz
import std.log

log.set_level("info")
log.info("server ready")
log.warn("retrying attempt {n}")
log.error("boom: {e}")

sp := log.span_begin("load_config")
// ... work ...
println(log.get_level())  // "info"
log.to_file("app.log")    // redirect sink to a file
log.to_stderr()           // back to stderr
```

## Types

| Type | Notes |
|---|---|
| `span` | Opaque span handle from `span_begin`; marks timed/logged regions. |

## Variables

None.

## Functions

| Function | Signature | Notes |
|---|---|---|
| `trace(msg)` | `trace(msg: str) -> unit` | Trace-level log. |
| `debug(msg)` | `debug(msg: str) -> unit` | Debug-level log. |
| `info(msg)` | `info(msg: str) -> unit` | Info-level log. |
| `warn(msg)` | `warn(msg: str) -> unit` | Warning-level log. |
| `error(msg)` | `error(msg: str) -> unit` | Error-level log. |
| `set_level(name)` | `set_level(name: str) -> bool` | Minimum level (`trace`/`debug`/`info`/`warn`/`error`). |
| `get_level()` | `get_level() -> str` | Current level name. |
| `set_format(name)` | `set_format(name: str) -> bool` | Output format (`text`/`json`). |
| `span_begin(name)` | `span_begin(name: str) -> span` | Begin a named timed span. |
| `to_file(path)` | `to_file(path: str) -> bool` | Redirect log output to `path`. |
| `to_stderr()` | `to_stderr() -> unit` | Redirect log output to stderr. |
