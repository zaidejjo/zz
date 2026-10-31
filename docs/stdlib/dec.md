# `std.dec` — fixed-point decimal arithmetic

Exact decimal math on scaled integers. A decimal is stored as a string
representing an integer scaled by `10^scale`, avoiding binary floating
point error for money and measurements.

```zz
import std.dec

match dec.add("10.50", "3.25") {
    .ok(v) => println(v),        // not applicable: dec functions are total
    .err(e) => println(e),
}
// dec.add returns str directly (total): 
sum := dec.add("10.50", "3.25") // "13.75"
diff := dec.sub("10.50", "3.25") // "7.25"
prod := dec.mul("2.5", "4.0")   // "10.00"
cmp := dec.cmp("1.5", "1.49")   // 1 (greater)
```

## Types

None (decimal values are `str`).

## Variables

None.

## Functions

| Function | Signature | Notes |
|---|---|---|
| `add(a, b)` | `add(a: str, b: str) -> str` | Exact sum. |
| `sub(a, b)` | `sub(a: str, b: str) -> str` | Exact difference. |
| `mul(a, b)` | `mul(a: str, b: str) -> str` | Exact product. |
| `pow10(n)` | `pow10(n: int) -> int` | `10^n` as an integer. |
| `cmp(a, b)` | `cmp(a: str, b: str) -> int` | `-1`/`0`/`1` ordering comparison. |
| `eq` / `gt` / `lt` | `(a: str, b: str) -> bool` | Equality / greater-than / less-than. |
| `scale_of(s)` | `scale_of(s: str) -> int` | Number of fractional digits. |
| `to_scaled(s, scale)` | `to_scaled(s: str, scale: int) -> int` | Integer value scaled to `scale`. |
| `from_scaled(v, scale)` | `from_scaled(v: int, scale: int) -> str` | Integer `v` back to a decimal string. |
| `format(s, places)` | `format(s: str, places: int) -> str` | Format to `places` decimal digits. |
| `trim_zeros(s)` | `trim_zeros(s: str) -> str` | Remove trailing fractional zeros. |
| `is_valid(s)` | `is_valid(s: str) -> bool` | Whether `s` is a valid decimal. |
