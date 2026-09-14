//! `std.time` high-resolution extension for the unified native runtime.
//!
//! One implementation serves both engines: the VM calls the safe API
//! ([`now_nanos`], [`now_micros`], [`monotonic_nanos`], [`sleep_micros`])
//! through thin `zz_stdlib` adapters, while AOT binaries call the
//! `extern "C"` `zz_time_*` functions (same `zz_value` convention).
//! The pre-existing `now_ms`/`sleep_ms` stay where they are (working
//! code is not churned); everything new is unified from birth.
//!
//! Clocks:
//! - `now_*`: system clock (UNIX epoch). Subject to NTP adjustments —
//!   good for timestamps, wrong for measuring intervals.
//! - `monotonic_nanos`: `Instant`-based process uptime. Never goes
//!   backwards — the right clock for benchmarks.

use std::sync::LazyLock;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::cabi::CValue;

/// Process start, for the monotonic clock.
static START: LazyLock<Instant> = LazyLock::new(Instant::now);

fn system_nanos() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0)
}

/// Nanoseconds since the UNIX epoch (system clock), saturating at `i64::MAX`
/// (reached in year 2262 — practically, always exact).
pub fn now_nanos() -> i64 {
    system_nanos().min(i64::MAX as u128) as i64
}

/// Microseconds since the UNIX epoch (system clock).
pub fn now_micros() -> i64 {
    (system_nanos() / 1_000).min(i64::MAX as u128) as i64
}

/// Nanoseconds since process start (monotonic clock). Never decreases.
pub fn monotonic_nanos() -> i64 {
    START.elapsed().as_nanos().min(i64::MAX as u128) as i64
}

/// Sleep `micros` microseconds. Zero/negative sleeps return immediately.
pub fn sleep_micros(micros: i64) {
    if micros > 0 {
        std::thread::sleep(Duration::from_micros(micros as u64));
    }
}

fn set_err(err: *mut std::ffi::c_int) {
    if !err.is_null() {
        // SAFETY: codegen always passes a valid out-param; guard anyway.
        unsafe {
            *err = 1;
        }
    }
}

/// `time.now_nanos() -> int`.
#[no_mangle]
pub extern "C" fn zz_time_now_nanos(_unit: CValue, err: *mut std::ffi::c_int) -> CValue {
    let _ = err;
    CValue::int(now_nanos())
}

/// `time.now_micros() -> int`.
#[no_mangle]
pub extern "C" fn zz_time_now_micros(_unit: CValue, err: *mut std::ffi::c_int) -> CValue {
    let _ = err;
    CValue::int(now_micros())
}

/// `time.monotonic_nanos() -> int`.
#[no_mangle]
pub extern "C" fn zz_time_monotonic_nanos(_unit: CValue, err: *mut std::ffi::c_int) -> CValue {
    let _ = err;
    CValue::int(monotonic_nanos())
}

/// `time.sleep_micros(n: int) -> unit`.
#[no_mangle]
pub extern "C" fn zz_time_sleep_micros(n: CValue, err: *mut std::ffi::c_int) -> CValue {
    match n.as_i64() {
        Some(micros) => {
            sleep_micros(micros);
            CValue::unit()
        }
        None => {
            set_err(err);
            CValue::unit()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clocks_advance_and_agree() {
        let a = now_nanos();
        let b = now_nanos();
        assert!(b >= a, "system clock went backwards");
        let micros = now_micros();
        // nanos/1000 and micros sampled back-to-back agree within 1ms.
        assert!((b / 1_000 - micros).abs() < 1_000, "nanos/micros disagree");
        assert!(now_nanos() > 1_700_000_000_000_000_000, "sane epoch");
    }

    #[test]
    fn monotonic_never_decreases() {
        let a = monotonic_nanos();
        let b = monotonic_nanos();
        assert!(b >= a);
        assert!(a >= 0);
    }

    #[test]
    fn sleep_micros_sleeps() {
        let t0 = monotonic_nanos();
        sleep_micros(20_000);
        let dt = monotonic_nanos() - t0;
        assert!(dt >= 20_000, "slept only {dt}ns");
        assert!(dt < 20_000_000_000, "slept absurdly long: {dt}ns");
        // Zero/negative are no-ops (must not hang or error).
        sleep_micros(0);
        sleep_micros(-5);
    }
}
