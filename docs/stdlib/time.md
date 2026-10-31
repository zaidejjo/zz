# `std.time` — clocks, sleeps, and calendar dates

Monotonic and wall-clock time, sleeps, and UTC RFC3339 calendar dates.
Dates are dicts `{year, month, day, hour, min, sec}`.

```zz
import std.time

start := time.now_ms()
time.sleep_ms(1000)
elapsed := time.now_ms() - start
println("slept {elapsed}ms")

d := time.parse_rfc3339("2024-02-29T12:30:45Z")
println(time.format_rfc3339(d))          // 2024-02-29T12:30:45Z
println(time.add_days(d, 1)["day"])      // 1
println(time.diff_days(d, time.make_date(2024, 2, 28, 0, 0, 0))) // 1
```

## Types

| Type | Notes |
|---|---|
| `Date` | Dict `{year, month, day, hour, min, sec}` as `{str: int}`. Produced by `make_date`/`parse_rfc3339`. |

## Variables

None.

## Functions

| Function | Signature | Notes |
|---|---|---|
| `now_ms()` | `now_ms() -> int` | Wall-clock ms since epoch. |
| `now_micros()` | `now_micros() -> int` | Wall-clock micros since epoch. |
| `now_nanos()` | `now_nanos() -> int` | Wall-clock nanos since epoch. |
| `monotonic_nanos()` | `monotonic_nanos() -> int` | Monotonic clock nanos. |
| `sleep_ms(ms)` | `sleep_ms(ms: int) -> unit` | Cooperative sleep (yields green threads). |
| `sleep_micros(micros)` | `sleep_micros(micros: int) -> unit` | Cooperative sleep in micros. |
| `parse_rfc3339(s)` | `parse_rfc3339(s: str) -> {str: int}` | Parse UTC RFC3339; invalid → epoch fallback. |
| `format_rfc3339(d)` | `format_rfc3339(d: {str: int}) -> str` | Format a date dict as RFC3339. |
| `make_date(y, mo, d, h, mi, se)` | `(...) -> {str: int}` | Build a date dict. |
| `date_valid(y, m, d)` | `(y: int, m: int, d: int) -> bool` | Calendar validity. |
| `is_leap(y)` | `is_leap(y: int) -> bool` | Leap-year check. |
| `days_in_month(y, m)` | `(y: int, m: int) -> int` | Days in month. |
| `add_days(d, n)` | `(d: {str: int}, n: int) -> {str: int}` | Add days (leap-correct). |
| `diff_days(a, b)` | `(a: {str: int}, b: {str: int}) -> int` | Whole days between two dates. |
| `to_epoch_days(d)` | `to_epoch_days(d: {str: int}) -> int` | Days since epoch. |
| `from_epoch_days(z)` | `from_epoch_days(z: int) -> {str: int}` | Days-since-epoch to date. |
| `civil_from_days(z)` | `civil_from_days(z: int) -> {str: int}` | Civil date from epoch days. |
| `days_from_civil(y, m, d)` | `(y: int, m: int, d: int) -> int` | Days from epoch to civil date. |
| `pad2(n)` | `pad2(n: int) -> str` | Zero-pad to 2 digits. |
| `epoch_fallback()` | `epoch_fallback() -> {str: int}` | Epoch date dict on parse failure. |
