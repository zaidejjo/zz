# Log — `std.log`

Leveled logging (trace < debug < info < warn < error), pluggable sinks (stderr, file, json/text format), and timed spans. Span handles are opaque `"span"`.

```zz
import std.log
```

## Purpose

`std.log` provides structured, leveled logging for applications and libraries. Keep hot-path logs at `debug` and gate expensive field computation behind level checks. Sinks are process-wide — set once in `main`. Spans nest; `end` is idempotent.

## Quickstart

```zz
import std.log

k := "k1"
n := 1
log.set_level("debug")   // trace < debug < info < warn < error
println(log.get_level())
log.set_format("json")   // or "text"
log.to_stderr()
log.to_file("/tmp/app.log")
log.trace("tick")
log.debug("cache hit {k}")
log.info("listening :8080")
log.warn("retry {n}")
log.error("disk full")

s := log.span_begin("request")   // Opaque("span")
s.end()                          // elapsed micros, idempotent
```

## Span lifecycle

```zz
import std.log

s := log.span_begin("request")   // returns span handle
// ... work ...
micros := s.end()                // elapsed microseconds (int)
s.end()                          // idempotent: second call returns 0
```

## Exhaustive reference

### Level & sink

| Function | Signature | Description | Example |
|----------|-----------|-------------|---------|
| `log.set_level` | `log.set_level(name: str) -> bool` | Set level | `log.set_level("debug")` |
| `log.get_level` | `log.get_level() -> str` | Current level name | → `"info"` |
| `log.set_format` | `log.set_format(name: str) -> bool` | `json` or `text` | `log.set_format("json")` |
| `log.to_file` | `log.to_file(path: str) -> bool` | Sink to file | `log.to_file("/tmp/app.log")` |
| `log.to_stderr` | `log.to_stderr() -> unit` | Sink to stderr | |

### Level ops (total)

| Function | Signature | Description |
|----------|-----------|-------------|
| `log.trace` | `log.trace(msg: str) -> unit` | Trace |
| `log.debug` | `log.debug(msg: str) -> unit` | Debug |
| `log.info` | `log.info(msg: str) -> unit` | Info |
| `log.warn` | `log.warn(msg: str) -> unit` | Warn |
| `log.error` | `log.error(msg: str) -> unit` | Error |

### Spans

| Function | Signature | Description | Example |
|----------|-----------|-------------|---------|
| `log.span_begin` | `log.span_begin(name: str) -> span` | Begin span (opaque handle) | `s := log.span_begin("req")` |
| `span.end` | `span.end(sp: span) -> int` | End span, elapsed micros | `s.end()` |

> `span.end` is the bare seed (method call on span handles); `std.span.end` is the qualified form. Both resolve to the same op.

## Error model

- `set_level`/`set_format`/`to_file` return `bool` (success/failure).
- Level ops (`trace`…`error`) are total — never fallible.
- `span.end` returns elapsed micros (int); idempotent.

## Gotchas

- **Sinks are process-wide** — set once in `main`; changing mid-run affects all logs.
- **Keep hot-path logs at `debug`** — gate expensive field computation behind level checks.
- **Spans nest** — `end` is idempotent (second call returns 0).
- **Interpolation** (`"cache hit {k}"`) is evaluated before the level check — for expensive fields, gate manually.

## Production patterns

```zz
import std.log

func main() {
    log.set_level("info")
    log.set_format("json")
    log.to_stderr()
    log.info("server starting")
    defer log.span_begin("request").end()   // span auto-ends on scope exit
}
```

