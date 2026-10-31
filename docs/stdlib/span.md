# `std.span` — span type

`span` is an opaque handle type used by `std.log.span_begin` to mark timed
or logged regions of execution.

## Types

| Type | Notes |
|---|---|
| `span` | Opaque span handle from `log.span_begin`. |

## Variables

None.

## Functions

| Function | Signature | Notes |
|---|---|---|
| `end(sp)` | `end(sp: span) -> int` | Elapsed nanos/ticks for a span (log-internal). |
