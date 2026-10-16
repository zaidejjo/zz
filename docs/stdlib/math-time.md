# Math and time — `std.math`, `std.time`

```zz
import std.math
import std.time
```

## `std.math`

```zz
math.abs(-5)     // 5
math.floor(3.7)  // 3
math.ceil(3.2)   // 4
math.sqrt(9.0)   // 3.0
math.pow(2, 10)  // 1024.0
x := math.random()  // 0.0 <= x < 1.0
```

Coverage (see [stdlib.md](../stdlib.md) for exact arities):

- Rounding: `round`, `trunc`, `floor`, `ceil`, `clamp`, `signum`.
- Roots/powers: `sqrt`, `pow`, `root`, `isqrt`, `hypot`, `exp`, `log`, `log10`.
- Trig: `sin/cos/tan`, `asin/acos/atan`, `sin_deg/cos_deg/tan_deg`, `to_radians/to_degrees`.
- Predicates: `is_nan`, `is_inf`, `is_even`.
- Ints: `factorial`, `gcd`, `lcm`, `min`, `max`, `rand_range`.
- Aggregates: `sum`, `product`, `count`, `mean`, `median`, `dot_product`, `magnitude`, `matrix_mul`.

All builds use `-fwrapv` + no `-ffast-math`: VM and native agree bit-for-bit
on ints and floats.

## `std.time`

```zz
start := time.now_ms()
time.sleep_ms(1000)
elapsed := time.now_ms() - start
println("slept {elapsed}ms")
```

| Function | Notes |
|---|---|
| `now_ms()` | ms since epoch |
| `sleep_ms(ms)` | cooperative sleep (yields green threads) |

## Calendar dates — dict `Date`

`{year, month, day, hour, min, sec}` as `{str: int}`. Parse/format are UTC
RFC3339 (`...Z`) only; invalid input falls back to epoch so callers stay total.

```zz
d := time.parse_rfc3339("2024-02-29T12:30:45Z")
println(time.format_rfc3339(d))  // 2024-02-29T12:30:45Z
println(time.add_days(d, 1)["day"])
println(time.diff_days(d, time.make_date(2024, 2, 28, 0, 0, 0)))  // 1
```

`make_date(y, mo, d, h, mi, se)`, `add_days` (leap-correct), `diff_days`,
`to_epoch_days` / `from_epoch_days`, `is_leap`, `days_in_month`, `date_valid`.
