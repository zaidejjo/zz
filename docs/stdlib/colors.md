# `std.colors` — ANSI terminal color & style helpers

`std.colors` builds ANSI escape sequences for coloring and styling terminal
output. Every function takes a string and returns a new string wrapped in
the corresponding escape codes. Compose them by nesting calls.

```zz
import std.colors

println(colors.red("error:"), " something failed")
println(colors.bold(colors.green("ok")))     // bold green
println(colors.hex("#ff5500", "custom"))     // 24-bit color
println(colors.rgb(255, 0, 0, "red"))        // RGB color
println(colors.bg_blue("on blue"))           // background color
```

## Types

None (functions return `str`).

## Variables

None.

## Functions

All functions have the signature `(s: str, ...) -> str` and return an
ANSI-wrapped copy of `s`. Nest to combine styles.

| Function | Signature | Notes |
|---|---|---|
| `black` / `red` / `green` / `yellow` / `blue` / `magenta` / `cyan` / `white` | `(s: str) -> str` | Basic foreground colors. |
| `bright_black` / `bright_red` / `bright_green` / `bright_yellow` / `bright_blue` / `bright_magenta` / `bright_cyan` / `bright_white` | `(s: str) -> str` | Bright (high-intensity) foreground colors. |
| `bg_black` … `bg_white` / `bg_bright_*` | `(s: str) -> str` | Background colors (basic and bright). |
| `bg_rgb(s, r, g, b)` | `(s: str, r: int, g: int, b: int) -> str` | 24-bit background color. |
| `color256(s, n)` / `bg_256(s, n)` | `(s: str, n: int) -> str` | 256-color palette foreground/background. |
| `rgb(s, r, g, b)` | `(s: str, r: int, g: int, b: int) -> str` | 24-bit foreground color. |
| `hex(s, code)` | `(s: str, code: str) -> str` | Hex color string (`#RRGGBB`). |
| `hex3(s, digits)` / `hex6(s, digits)` | `(s: str, digits: [str]) -> str` | Short/long hex digit arrays. |
| `bold` / `dim` / `italic` / `underline` / `blink` / `reverse` / `strikethrough` | `(s: str) -> str` | Text styles. |
| `reset(s)` | `(s: str) -> str` | Reset all styling. |
| `strip(s)` | `(s: str) -> str` | Remove all ANSI escapes from `s`. |
| `clamp255(v)` | `(v: int) -> int` | Clamp `v` to 0–255 (for color math). |
| `hex_byte(hi, lo)` | `(hi: str, lo: str) -> int` | Parse a hex byte from two digit strings. |
| `hex_val(c)` | `(c: str) -> int` | Numeric value of a single hex digit. |
