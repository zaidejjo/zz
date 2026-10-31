# `std.io` — I/O types

`std.io` exposes the `file` type used by `std.fs`. File handles are opaque
and returned by `fs.open`; they are closed with `fs.close` and read/written
with `fs.read_chunk` / `fs.write_chunk` / `fs.seek` / `fs.flush`.

## Types

| Type | Notes |
|---|---|
| `file` | Opaque file handle from `fs.open`. Use `fs.read_chunk`/`fs.read_chunk_bytes`/`fs.write_chunk`/`fs.seek`/`fs.flush`/`fs.close`. |

## Variables

None.

## Functions

None (all operations live under `std.fs`).
