//! `std.regexp` implementation for the unified native runtime.
//!
//! One implementation serves both engines: the VM calls the safe API
//! ([`compile`], [`with`]) through thin `zz_stdlib` adapters, while AOT
//! binaries call the `extern "C"` `zz_regexp_*` functions (same `zz_value`
//! convention as embedded-C natives, invoked via `zz_call_nativeN`).
//!
//! Compiled patterns live in the handle pool under tag `"regexp"`; across
//! the FFI they are `ZZ_INT` ids. Replacement strings support `$1`/`$name`
//! capture expansion (the `regex` crate default).

use std::sync::Arc;

use crate::cabi::{array_push_str, cvalue_str, cvalue_to_string, CValue};
use crate::{alloc, payload, Handle};

/// Pool tag for compiled patterns. Selects the `regexp.*` method namespace.
pub const TAG: &str = "regexp";

/// Compile `pattern`, storing it in the pool. The `regex` syntax error is
/// returned as a message (the caller wraps it in `Result.err`).
pub fn compile(pattern: &str) -> Result<Handle, String> {
    match regex::Regex::new(pattern) {
        Ok(re) => Ok(alloc(TAG, Arc::new(re))),
        Err(e) => Err(format!("invalid regexp `{pattern}`: {e}")),
    }
}

/// Run `f` on the pattern for `id`. `None` for unknown/dropped ids and for
/// handles owned by another module (tag mismatch).
pub fn with<T>(id: u64, f: impl FnOnce(&regex::Regex) -> T) -> Option<T> {
    payload::<regex::Regex>(id, TAG).map(|re| f(&re))
}

/// The pool id for a VM handle (tag-checked).
pub fn id_of(handle: &Handle) -> Option<u64> {
    (handle.tag == TAG).then_some(handle.id)
}

fn set_err(err: *mut std::ffi::c_int) {
    // SAFETY: codegen always passes a valid out-param; guard anyway.
    if !err.is_null() {
        unsafe {
            *err = 1;
        }
    }
}

/// `regexp.compile(pat: str) -> Result<int, str>` (AOT: handle id).
#[no_mangle]
pub extern "C" fn zz_regexp_compile(pat: CValue, err: *mut std::ffi::c_int) -> CValue {
    let Some(pattern) = cvalue_to_string(pat) else {
        set_err(err);
        return CValue::unit();
    };
    match compile(&pattern) {
        // SAFETY: constructors are provided by the linked AOT program.
        Ok(h) => unsafe { crate::cabi::zz_variant_ok(CValue::int(h.id as i64)) },
        Err(msg) => unsafe { crate::cabi::zz_variant_err(cvalue_str(msg.as_bytes())) },
    }
}

/// `regexp.is_match(re: int, s: str) -> bool`.
#[no_mangle]
pub extern "C" fn zz_regexp_is_match(re: CValue, s: CValue, err: *mut std::ffi::c_int) -> CValue {
    let (Some(id), Some(text)) = (re.as_i64(), cvalue_to_string(s)) else {
        set_err(err);
        return CValue::boolean(false);
    };
    match with(id as u64, |rx| rx.is_match(&text)) {
        Some(m) => CValue::boolean(m),
        None => {
            set_err(err);
            CValue::boolean(false)
        }
    }
}

/// `regexp.find(re: int, s: str) -> Option<str>` (first match).
#[no_mangle]
pub extern "C" fn zz_regexp_find(re: CValue, s: CValue, err: *mut std::ffi::c_int) -> CValue {
    let (Some(id), Some(text)) = (re.as_i64(), cvalue_to_string(s)) else {
        set_err(err);
        return CValue::none();
    };
    match with(id as u64, |rx| {
        rx.find(&text).map(|m| m.as_str().to_string())
    }) {
        // SAFETY: linked-program constructors.
        Some(Some(m)) => unsafe { crate::cabi::zz_variant_some(cvalue_str(m.as_bytes())) },
        Some(None) => CValue::none(),
        None => {
            set_err(err);
            CValue::none()
        }
    }
}

/// `regexp.replace_all(re: int, s: str, rep: str) -> str`.
#[no_mangle]
pub extern "C" fn zz_regexp_replace_all(
    re: CValue,
    s: CValue,
    rep: CValue,
    err: *mut std::ffi::c_int,
) -> CValue {
    let (Some(id), Some(text), Some(repl)) =
        (re.as_i64(), cvalue_to_string(s), cvalue_to_string(rep))
    else {
        set_err(err);
        return cvalue_str(b"");
    };
    match with(id as u64, |rx| {
        rx.replace_all(&text, repl.as_str()).into_owned()
    }) {
        Some(out) => cvalue_str(out.as_bytes()),
        None => {
            set_err(err);
            cvalue_str(b"")
        }
    }
}

/// `regexp.captures(re: int, s: str) -> [str]` (group 0 first; `""` for
/// non-participating groups; `[]` when nothing matches).
#[no_mangle]
pub extern "C" fn zz_regexp_captures(re: CValue, s: CValue, err: *mut std::ffi::c_int) -> CValue {
    // SAFETY: linked-program constructor.
    let arr = unsafe { crate::cabi::zz_array_new() };
    let (Some(id), Some(text)) = (re.as_i64(), cvalue_to_string(s)) else {
        set_err(err);
        return arr;
    };
    match with(id as u64, |rx| {
        rx.captures(&text).map(|caps| {
            caps.iter()
                .map(|g| g.map(|m| m.as_str().to_string()).unwrap_or_default())
                .collect::<Vec<_>>()
        })
    }) {
        Some(Some(groups)) => {
            for g in &groups {
                array_push_str(arr, g);
            }
            arr
        }
        Some(None) => arr,
        None => {
            set_err(err);
            arr
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn compiled(pat: &str) -> Handle {
        compile(pat).expect("test pattern must compile")
    }

    #[test]
    fn rejects_invalid_pattern() {
        assert!(compile("([a-z").is_err());
    }

    #[test]
    fn match_find_replace_captures() {
        let h = compiled(r"(\d+)-(\d+)");
        let id = h.id;
        assert!(with(id, |rx| rx.is_match("ab12-34cd")).unwrap());
        assert!(!with(id, |rx| rx.is_match("none")).unwrap());
        assert_eq!(
            with(id, |rx| rx.find("x12-34y").map(|m| m.as_str().to_string())),
            Some(Some("12-34".to_string()))
        );
        assert_eq!(
            with(id, |rx| rx.replace_all("x12-34y", "[$1/$2]").into_owned()),
            Some("x[12/34]y".to_string())
        );
        assert_eq!(
            with(id, |rx| rx.captures("x12-34y").map(|c| c
                .iter()
                .map(|g| g.map(|m| m.as_str()).unwrap_or("").to_string())
                .collect::<Vec<_>>())),
            Some(Some(vec![
                "12-34".to_string(),
                "12".to_string(),
                "34".to_string()
            ]))
        );
        assert!(crate::drop_handle(id));
        assert!(with(id, |rx| rx.is_match("x")).is_none());
    }

    #[test]
    fn foreign_tag_rejected() {
        let h = crate::alloc("uuid", Arc::new(1u32));
        assert!(with(h.id, |rx| rx.is_match("x")).is_none());
        assert!(crate::drop_handle(h.id));
    }
}
