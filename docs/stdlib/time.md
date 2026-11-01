# Time — `std.time`

Clocks, sleeps, and calendar dates. Dates are `{str: int}` dicts (`year`, `month`, `day`, `hour`, `min`, `sec`). Parse/format are UTC RFC3339 (`...Z`) only; invalid input falls back to epoch so callers stay total.

```zz
import std.time
```

## Purpose

`std.time` provides wall-clock and monotonic time, sleeps (cooperative — yield green threads), and calendar-date math on dict-based dates. All parse/format ops are UTC RFC3339; invalid input falls back to epoch (total).

## Quickstart

```zz
import std.time

start := time.now_ms()
time.sleep_ms(1000)
elapsed := time.now_ms() - start
println("slept {elapsed}ms")

d := time.parse_rfc3339("2024-02-29T12:30:45Z")
println(time.format_rfc3339(d))  // 2024-02-29T12:30:45Z
println(time.add_days(d, 1)["day"])
println(time.diff_days(d, time.make_date(2024, 2, 28, 0, 0, 0)))  // 1
```

## Durations (pure-ZZ)

Spans as integer microseconds:

```zz
time.micros(1_000_000)   // 1 second in micros
time.millis(1000)        // 1000 ms in micros
time.secs(1)             // 1 sec in micros
d := time.millis(1000)   // duration span
time.to_micros(d)        // convert duration span to micros
time.to_millis(d)
time.to_secs(d)
time.to_nanos(d)
time.sleep(d)            // sleep a duration span
```

## Exhaustive reference

### Clocks

| Function | Signature | Description |
|----------|-----------|-------------|
| `time.now_ms` | `time.now_ms() -> int` | ms since epoch |
| `time.now_nanos` | `time.now_nanos() -> int` | nanos since epoch |
| `time.now_micros` | `time.now_micros() -> int` | micros since epoch |
| `time.monotonic_nanos` | `time.monotonic_nanos() -> int` | Monotonic clock |

### Sleeps

| Function | Signature | Description |
|----------|-----------|-------------|
| `time.sleep_ms` | `time.sleep_ms(ms: int) -> unit` | Cooperative sleep (yields green threads) |
| `time.sleep_micros` | `time.sleep_micros(micros: int) -> unit` | Sleep micros |
| `time.sleep` | `time.sleep(d: int) -> unit` | Sleep a duration span |

### Duration helpers

| Function | Signature | Description |
|----------|-----------|-------------|
| `time.micros` | `time.micros(us: int) -> int` | Duration span (micros) |
| `time.millis` | `time.millis(ms: int) -> int` | Duration span (millis) |
| `time.secs` | `time.secs(s: int) -> int` | Duration span (secs) |
| `time.to_micros` | `time.to_micros(d: int) -> int` | Convert to micros |
| `time.to_millis` | `time.to_millis(d: int) -> int` | Convert to millis |
| `time.to_secs` | `time.to_secs(d: int) -> int` | Convert to secs |
| `time.to_nanos` | `time.to_nanos(d: int) -> int` | Convert to nanos |

### Calendar dates

| Function | Signature | Description | Example |
|----------|-----------|-------------|---------|
| `time.make_date` | `time.make_date(y, mo, d, h, mi, se) -> {str: int}` | Build date dict | |
| `time.parse_rfc3339` | `time.parse_rfc3339(s: str) -> {str: int}` | Parse UTC RFC3339 (epoch on invalid) | |
| `time.format_rfc3339` | `time.format_rfc3339(d) -> str` | Format as UTC RFC3339 | |
| `time.add_days` | `time.add_days(d, n: int) -> {str: int}` | Shift by N days (leap-correct) | |
| `time.diff_days` | `time.diff_days(a, b) -> int` | `a - b` in days | |
| `time.to_epoch_days` | `time.to_epoch_days(d) -> int` | Days since 1970-01-01 | |
| `time.from_epoch_days` | `time.from_epoch_days(z: int) -> {str: int}` | Inverse of above | |
| `time.is_leap` | `time.is_leap(y: int) -> bool` | Gregorian leap year | |
| `time.days_in_month` | `time.days_in_month(y, m: int) -> int` | Month length | |
| `time.date_valid` | `time.date_valid(y, m, d: int) -> bool` | Range check | |
| `time.epoch_fallback` | `time.epoch_fallback() -> {str: int}` | Epoch date dict | |

## Calendar dates

Dates are `{str: int}` dicts with keys `year`, `month`, `day`, `hour`, `min`, `sec`. Access fields with `d["year"]`, etc.

## Gotchas

- **Parse/format are UTC RFC3339 (`...Z`) only** — no time zones, no locale.
- **Invalid input falls back to epoch** — callers stay total but should validate with `date_valid` first.
- **`sleep_ms`/`sleep` are cooperative** — they yield green threads, never block the OS thread.
- **`to_epoch_days`/`from_epoch_days`** use the civil calendar algorithm (Howard Hinnant).

## Production patterns

```zz
import std.time

// Measure elapsed cooperatively
start := time.monotonic_nanos()
// ... work ...
elapsed_ms := (time.monotonic_nanos() - start) / 1_000_000

// Validate before using
d := time.parse_rfc3339("2024-02-29T12:30:45Z")
if time.date_valid(d["year"], d["month"], d["day"]) {
    // safe to use d
}
```

