//! `std.sys` system information for the unified native runtime.
//!
//! One implementation serves both engines: the VM calls the safe API
//! through thin `zz_stdlib` adapters, while AOT binaries call the
//! `extern "C"` `zz_sys_*` functions (same `zz_value` convention).
//!
//! Sources: OS/arch come from compile-time std constants (zero cost,
//! identical in both engines); CPU count from `available_parallelism`;
//! hostname and memory from `sysinfo` (one fresh `System` per call —
//! millisecond-scale, never cached, so readings stay current).

use crate::cabi::{cvalue_str, CValue};

/// OS type (`"linux"`, `"macos"`, `"windows"`, …).
pub fn os() -> &'static str {
    std::env::consts::OS
}

/// CPU architecture (`"x86_64"`, `"aarch64"`, …).
pub fn arch() -> &'static str {
    std::env::consts::ARCH
}

/// Logical CPU count. Falls back to 1 when the platform gives no answer.
pub fn cpu_count() -> i64 {
    std::thread::available_parallelism()
        .map(|n| n.get() as i64)
        .unwrap_or(1)
}

/// Machine hostname (`""` when undiscoverable — never fails the call).
pub fn hostname() -> String {
    sysinfo::System::host_name().unwrap_or_default()
}

/// Total physical memory, bytes.
pub fn total_mem() -> i64 {
    let sys = sysinfo::System::new_all();
    sys.total_memory().min(i64::MAX as u64) as i64
}

/// Available physical memory, bytes.
pub fn avail_mem() -> i64 {
    let sys = sysinfo::System::new_all();
    sys.available_memory().min(i64::MAX as u64) as i64
}

/// `sys.avail_mem() -> int` (bytes).
#[no_mangle]
pub extern "C" fn zz_sys_avail_mem(_unit: CValue, err: *mut std::ffi::c_int) -> CValue {
    let _ = err;
    CValue::int(avail_mem())
}

/// `sys.os() -> str`.
#[no_mangle]
pub extern "C" fn zz_sys_os(_unit: CValue, err: *mut std::ffi::c_int) -> CValue {
    let _ = err;
    cvalue_str(os().as_bytes())
}

/// `sys.arch() -> str`.
#[no_mangle]
pub extern "C" fn zz_sys_arch(_unit: CValue, err: *mut std::ffi::c_int) -> CValue {
    let _ = err;
    cvalue_str(arch().as_bytes())
}

/// `sys.cpu_count() -> int`.
#[no_mangle]
pub extern "C" fn zz_sys_cpu_count(_unit: CValue, err: *mut std::ffi::c_int) -> CValue {
    let _ = err;
    CValue::int(cpu_count())
}

/// `sys.hostname() -> str`.
#[no_mangle]
pub extern "C" fn zz_sys_hostname(_unit: CValue, err: *mut std::ffi::c_int) -> CValue {
    let _ = err;
    cvalue_str(hostname().as_bytes())
}

/// `sys.total_mem() -> int` (bytes).
#[no_mangle]
pub extern "C" fn zz_sys_total_mem(_unit: CValue, err: *mut std::ffi::c_int) -> CValue {
    let _ = err;
    CValue::int(total_mem())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn consts_match_host() {
        assert_eq!(os(), std::env::consts::OS);
        assert_eq!(arch(), std::env::consts::ARCH);
        assert!(!os().is_empty());
        assert!(!arch().is_empty());
    }

    #[test]
    fn cpu_and_mem_sane() {
        assert!(cpu_count() >= 1);
        let total = total_mem();
        let avail = avail_mem();
        assert!(total > 0, "total memory must be positive");
        assert!(avail > 0, "available memory must be positive");
        assert!(avail <= total, "available ({avail}) <= total ({total})");
    }

    #[test]
    fn hostname_present() {
        // CI containers always have a hostname; even so, only assert the
        // call path works (empty is allowed by contract).
        let _ = hostname();
    }
}
