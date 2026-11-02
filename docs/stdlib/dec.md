# Decimal — `std.dec`

Exact decimal arithmetic on strings. No floating-point drift: `dec.add("0.1", "0.2") == "0.3"`. Scale capped at 18 places. Decimals are plain `str` in `[-]digits[.digits]` form.

```zz
import std.dec
```

## Purpose

`std.dec` does exact base-10 arithmetic on decimal strings, avoiding the binary floating-point drift that makes `0.1 + 0.2 != 0.3` in `float`. All ops take and return `str`. Use for money, prices, taxes, and any domain where exact decimal semantics are required.

## Conceptual model

A decimal is a `str` matching `[-]digits[.digits]`. Internally operations work on scaled integers. Scale (fractional digits) is capped at 18. Results are normalized (no trailing zeros after the point unless significant).

## Quickstart

```zz
import std.dec

dec.add("0.1", "0.2")      // "0.3"      (not "0.30000000000000004")
dec.sub("1.0", "0.3")      // "0.7"
dec.mul("1.5", "2")        // "3"
dec.format("3.14159", 2)   // "3.14"
dec.cmp("1.5", "2")        // -1
dec.eq("0.30", "0.3")      // true  (normalized comparison)
```

## Exhaustive reference

| Function | Signature | Description | Example |
|----------|-----------|-------------|---------|
| `dec.is_valid` | `dec.is_valid(s: str) -> bool` | Shape check (`[-]digits[.digits]`) | `dec.is_valid("1.5")` → true |
| `dec.scale_of` | `dec.scale_of(s: str) -> int` | Number of fractional digits | `dec.scale_of("1.23")` → 2 |
| `dec.pow10` | `dec.pow10(n: int) -> int` | 10^n | `dec.pow10(3)` → 1000 |
| `dec.to_scaled` | `dec.to_scaled(s: str, scale: int) -> int` | Scale to integer | |
| `dec.from_scaled` | `dec.from_scaled(v: int, scale: int) -> str` | Integer to scaled string | |
| `dec.trim_zeros` | `dec.trim_zeros(s: str) -> str` | Trim trailing zeros | `dec.trim_zeros("1.500")` → "1.5" |
| `dec.add` | `dec.add(a: str, b: str) -> str` | Exact addition | `dec.add("0.1","0.2")` → `"0.3"` |
| `dec.sub` | `dec.sub(a: str, b: str) -> str` | Exact subtraction | `dec.sub("1.0","0.3")` → `"0.7"` |
| `dec.mul` | `dec.mul(a: str, b: str) -> str` | Exact multiplication | `dec.mul("1.5","2")` → `"3"` |
| `dec.cmp` | `dec.cmp(a: str, b: str) -> int` | -1 / 0 / 1 | `dec.cmp("1.5","2")` → -1 |
| `dec.eq` | `dec.eq(a: str, b: str) -> bool` | Equality (normalized) | `dec.eq("0.30","0.3")` → true |
| `dec.lt` | `dec.lt(a: str, b: str) -> bool` | Less than | |
| `dec.gt` | `dec.gt(a: str, b: str) -> bool` | Greater than | |
| `dec.format` | `dec.format(s: str, places: int) -> str` | Round/truncate to N places | `dec.format("3.14159",2)` → `"3.14"` |

## Edge cases & gotchas

- **Scale capped at 18 places.** Results beyond 18 fractional digits are truncated (not rounded) — verify against your domain's precision needs.
- **Input validation:** `is_valid` checks shape; arithmetic ops assume valid input — invalid strings are runtime errors.
- **No `dec.div`** — division can produce non-terminating decimals; scale via `to_scaled`/`from_scaled` or use `float` when approximate is acceptable.
- **`format` rounds/truncates** to `places` — not banker's rounding by default; test boundary cases (e.g. `0.005`).
- **Normalized comparison:** `dec.eq("0.30", "0.3")` → true (trailing zeros don't matter for equality).

## Performance notes

- All ops allocate new strings; O(n) in digit count.
- Fine for transactional money math; not for high-throughput numeric kernels (use `float`/`int` there).
- Pure ZZ — identical results on VM and AOT.

## Production patterns

```zz
import std.dec

// Money: price * qty, format to 2 places
price := dec.mul("19.99", "2")          // "39.98"
display := dec.format(price, 2)         // "39.98"

// Exact comparison
subtotal := dec.add("10.00", "5.00")    // "15.00"
tax := dec.mul(subtotal, "0.1")         // "1.5"
tax_total := dec.add(subtotal, tax)     // "16.5"
if dec.eq(tax_total, dec.add(subtotal, tax)) {
    println("ok")
}
```

