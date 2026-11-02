# Colors — `std.colors`

ANSI styling: foregrounds (30–37, 90–97), backgrounds (40–47, 100–107), text styles (bold/dim/italic/underline/blink/reverse/strikethrough), truecolor (`rgb`/`bg_rgb`), 256-color (`color256`/`bg_256`), hex (`#RRGGBB`), and numeric helpers. Every wrapper takes text first and appends a trailing reset so styles never bleed. Pipeline-friendly — composes with `|>`.

```zz
import std.colors
```

## Purpose

`std.colors` wraps text in ANSI escape sequences for terminal output. All channel wrappers take `(s: str) -> str` and append a reset, so styles never bleed across output. Nesting stacks (`bold(reverse(x))`); `strip` recovers visible text. Designed for CLI tools, test reporters, and logs.

## Quickstart

```zz
import std.colors

println("TEST FAIL" |> colors.red() |> colors.bold())
println(colors.strip(colors.reverse("sel")) == "sel")   // true
println(colors.rgb("hi", 255, 87, 51))                  // truecolor fg
println(colors.hex("hi", "#FF5733"))                    // #RRGGBB / RRGGBB / #RGB
println(colors.bg_rgb("ok", 0, 128, 0))                 // truecolor bg
```

## Exhaustive reference

### Foreground (30–37)

| Function | Signature | Description |
|----------|-----------|-------------|
| `colors.black` … `colors.white` | `colors.<name>(s: str) -> str` | Foreground SGR 30–37 |

### Bright foreground (90–97)

| Function | Signature | Description |
|----------|-----------|-------------|
| `colors.bright_black` … `colors.bright_white` | `colors.<name>(s: str) -> str` | Foreground SGR 90–97 |

### Background (40–47)

| Function | Signature | Description |
|----------|-----------|-------------|
| `colors.bg_black` … `colors.bg_white` | `colors.<name>(s: str) -> str` | Background SGR 40–47 |

### Bright background (100–107)

| Function | Signature | Description |
|----------|-----------|-------------|
| `colors.bg_bright_black` … `colors.bg_bright_white` | `colors.<name>(s: str) -> str` | Background SGR 100–107 |

### Text styles

| Function | Signature | Description | Example |
|----------|-----------|-------------|---------|
| `colors.bold` | `colors.bold(s: str) -> str` | Bold (SGR 1) | |
| `colors.dim` | `colors.dim(s: str) -> str` | Dim (SGR 2) | |
| `colors.italic` | `colors.italic(s: str) -> str` | Italic (SGR 3) | |
| `colors.underline` | `colors.underline(s: str) -> str` | Underline (SGR 4) | |
| `colors.blink` | `colors.blink(s: str) -> str` | Blink (SGR 5) | |
| `colors.reverse` | `colors.reverse(s: str) -> str` | Reverse video (SGR 7) | |
| `colors.strikethrough` | `colors.strikethrough(s: str) -> str` | Strikethrough (SGR 9) | |
| `colors.reset` | `colors.reset(s: str) -> str` | Append reset | |
| `colors.strip` | `colors.strip(s: str) -> str` | Remove all `\e[...m` | `colors.strip(colors.red("x"))` → `"x"` |

### Truecolor

| Function | Signature | Description | Example |
|----------|-----------|-------------|---------|
| `colors.rgb` | `colors.rgb(s: str, r: int, g: int, b: int) -> str` | Truecolor fg (channels clamped) | `colors.rgb("hi",255,87,51)` |
| `colors.bg_rgb` | `colors.bg_rgb(s: str, r: int, g: int, b: int) -> str` | Truecolor bg (channels clamped) | |

### 256-color

| Function | Signature | Description |
|----------|-----------|-------------|
| `colors.color256` | `colors.color256(s: str, n: int) -> str` | 256-color fg (`38;5;n`, clamped) |
| `colors.bg_256` | `colors.bg_256(s: str, n: int) -> str` | 256-color bg (`48;5;n`, clamped) |

### Hex

| Function | Signature | Description | Example |
|----------|-----------|-------------|---------|
| `colors.hex` | `colors.hex(s: str, code: str) -> str` | `#FF5733` / `FF5733` / `#F53`; malformed passes through | `colors.hex("hi","#FF5733")` |
| `colors.hex6` | helper over digit arrays | Six-digit form | |
| `colors.hex3` | helper over digit arrays | Three-digit form | |

### Numeric helpers

| Function | Signature | Description |
|----------|-----------|-------------|
| `colors.hex_val` | `colors.hex_val(c: str) -> int` | Nibble parser (`-1` on malformed) |
| `colors.hex_byte` | `colors.hex_byte(hi: str, lo: str) -> int` | Byte parser (`-1` on malformed) |
| `colors.clamp255` | `colors.clamp255(v: int) -> int` | Clamp to `0–255` |

## Edge cases & gotchas

- **Every wrapper appends a reset** — styles never bleed across output.
- **Malformed hex passes through unchanged** — no error, no styling.
- **Channels are clamped to `0–255`** automatically.
- **Nested styles stack** — `bold(reverse(x))` applies both; inner reset ends its own segment.

## Performance notes

- All ops are O(n) string allocation; fine for CLI output.
- Not intended for high-frequency per-character styling in tight loops.

## Production patterns

```zz
import std.colors

// Status color helper
func status_color(ok: bool, msg: str) -> str {
    if ok { return colors.green(msg) }
    return colors.red(msg) |> colors.bold()
}

// Section header
println(colors.bold(colors.underline("=== Report ===")))
```

