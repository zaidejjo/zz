# Term — `std.term`

Raw mode, single-key reads, terminal size, and flush. Fallible ops return `Result` so non-TTY stdin degrades gracefully; `is_tty` is the total predicate.

```zz
import std.term
```

## Purpose

`std.term` controls the terminal: raw mode (byte-at-a-time, no echo), single-key reads, size queries, and flush. Raw mode clears `ICANON`/`ECHO` (plus `ISIG`, so Ctrl+C arrives as byte `3` and ZZ code can restore via `defer` instead of dying with a raw TTY). Non-TTY stdin fails soft with `.err("std.term.<op>: not a tty")` — pair with `is_tty` (`poll` excepted: works on pipes).

## Quickstart

```zz
import std.term

func main() {
    match term.enable_raw() {
        .ok(_)  => println("raw on"),
        .err(e) => println("no tty: {e}"),
    }
    defer term.disable_raw()
    match term.read_key() {
        .ok(k)  => println("key: {k}"),
        .err(e) => println("read failed: {e}"),
    }
}
```

## Key bytes (`read_key` yields raw bytes)

| Key | Bytes |
|-----|-------|
| Up / Down / Right / Left | `27,91,65` / `66` / `67` / `68` |
| Enter | `13` |
| Esc (lone) | `27` |
| Space | `32` |
| Backspace | `127` |
| Letters | ASCII (`a`=`97`, `q`=`113`) |
| Ctrl+C | `3` (raw mode passes through as byte) |

After reading `27` (Esc prefix), `poll(50)` for the rest: bytes waiting = escape sequence; silence = lone Esc.

## Exhaustive reference

| Function | Signature | Description | Example |
|----------|-----------|-------------|---------|
| `term.enable_raw` | `term.enable_raw() -> Result<unit, str>` | Save termios, switch stdin raw | |
| `term.disable_raw` | `term.disable_raw() -> Result<unit, str>` | Restore saved state (idempotent) | |
| `term.read_key` | `term.read_key() -> Result<int, str>` | Block for one byte `0–255` | |
| `term.poll` | `term.poll(ms: int) -> Result<bool, str>` | True when byte ready (`<=0` = no wait; works on pipes) | |
| `term.get_size` | `term.get_size() -> Result<[int, int], str>` | Terminal `[cols, rows]` via `TIOCGWINSZ` | |
| `term.is_tty` | `term.is_tty() -> bool` | Total predicate for graceful degradation | |
| `term.flush` | `term.flush() -> unit` | Flush stdout now | |

## Error model

- `enable_raw`/`disable_raw`/`read_key`/`poll`/`get_size` return `Result<_, str>` — `.err("not a tty")` on non-TTY.
- `is_tty` is total (`bool`).
- `flush` is total (`unit`).

## Gotchas

- **Always `defer term.disable_raw()`** after `enable_raw` — avoids leaving the TTY raw on exit/panic.
- **After reading `27`, `poll(50)`** to distinguish Esc from arrow sequences.
- **`poll` works on pipes** — closed stdin reports ready; next read returns EOF.
- **Non-TTY stdin** (CI, pipes) fails soft — always pair with `is_tty`.

## Production patterns

```zz
import std.term

func read_char() -> Result<int, str> {
    if !term.is_tty() {
        return .err("std.term.read_char: not a tty")
    }
    match term.enable_raw() {
        .ok(_) => (),
        .err(e) => return .err(e),
    }
    defer term.disable_raw()
    return term.read_key()
}
```

