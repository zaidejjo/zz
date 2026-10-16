# Terminal — `std.term`, `std.colors`

## `std.term`

```zz
import std.term
```

| Function | Description |
|---|---|
| `enable_raw()` | save termios, raw byte-at-a-time, no echo |
| `disable_raw()` | restore (idempotent) |
| `read_key()` | block for one byte `0–255` |
| `poll(ms)` | true when a byte is ready (`<=0` = no wait; works on pipes) |
| `get_size()` | `[cols, rows]` via `TIOCGWINSZ` |
| `is_tty()` | total predicate for graceful degradation |
| `flush()` | flush stdout now |

Raw mode clears `ICANON`/`ECHO` (+ `ISIG`, so Ctrl+C arrives as byte `3` and ZZ
restores via `defer` instead of dying with a raw TTY). Non-TTY stdin fails soft
`.err("std.term.<op>: not a tty")` — pair with `is_tty` (`poll` excepted: works
on pipes; closed stdin reports ready, next read is EOF).

```zz
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

Key bytes (xterm-style): arrows `27,91,65/66/67/68`, Enter `13`, lone Esc `27`,
Space `32`, Backspace `127`, letters ASCII, Ctrl+C `3`. After `27`, `poll(50)` for
the rest: bytes waiting = escape sequence, silence = Esc.

## `std.colors` — ANSI, pipeline-friendly

Every fn takes text first and appends reset so styles never bleed.
Nesting stacks (`bold(reverse(x))`); `strip` recovers visible text.

```zz
import std.colors

println("TEST FAIL" |> colors.red() |> colors.bold())
println(colors.strip(colors.reverse("sel")) == "sel")
println(colors.rgb("hi", 255, 87, 51))
println(colors.hex("hi", "#FF5733"))  // #RRGGBB / RRGGBB / #RGB; malformed passes through
```

Foregrounds `black…white` (30–37), brights (90–97), backgrounds (40–47, 100–107),
`bold/dim/italic/underline/blink/reverse/strikethrough/reset/strip`,
truecolor `rgb/bg_rgb`, 256-color `color256/bg_256`, `hex/hex6/hex3/hex_val/hex_byte`,
`clamp255`.
