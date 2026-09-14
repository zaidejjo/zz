//! `std.args` CLI argument parsing for the unified native runtime.
//!
//! One implementation serves both engines: the VM calls the safe API
//! through thin `zz_stdlib` adapters, while AOT binaries call the
//! `extern "C"` `zz_args_*` functions (same `zz_value` convention).
//!
//! Two layers:
//! - Raw access: the caller passes its argv array explicitly
//!   (`p.parse(args.get_raw())`). The VM reads `interp.args`; AOT reads
//!   the C globals via the fixed `zz_env_args` (same symbol backs
//!   `args.get_raw`, so both spellings share one source of truth).
//! - `Parser`: a pool handle (tag `"args"`, interior-mutated through
//!   `Mutex`) configured with `str/int/bool` flags, then `parse`d
//!   against an explicit array and queried with `get_*`.
//!
//! Flag grammar: `--name value`, `--name=value`, bare `--flag` (bool
//! only, means true), `--flag=true|false|1|0`, `--` terminator (rest is
//! positional), `--help`/`-h` (parse fails soft with empty error — the
//! caller shows `help()`), first positional doubles as `subcommand()`.
//! Unknown flags, missing values, and bad ints fail with a message in
//! `error()`. Single-dash shorts are not supported (except `-h`).

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use crate::cabi::{cvalue_str, cvalue_to_str_vec, cvalue_to_string, CValue};
use crate::{alloc, payload, Handle};

/// Pool tag for parsers. Selects the `args.*` method namespace.
pub const TAG: &str = "args";

/// A configured flag parser (interior-mutated; results live here too).
#[derive(Debug, Default)]
pub struct Parser {
    str_flags: Vec<(String, String)>,
    int_flags: Vec<(String, i64)>,
    bool_flags: Vec<String>,
    parsed_str: HashMap<String, String>,
    parsed_int: HashMap<String, i64>,
    parsed_bool: HashMap<String, bool>,
    positionals: Vec<String>,
    error: String,
    help_requested: bool,
}

fn find_str(p: &Parser, name: &str) -> Option<usize> {
    p.str_flags.iter().position(|(n, _)| n == name)
}

fn find_int(p: &Parser, name: &str) -> Option<usize> {
    p.int_flags.iter().position(|(n, _)| n == name)
}

fn find_bool(p: &Parser, name: &str) -> Option<usize> {
    p.bool_flags.iter().position(|n| n == name)
}

fn with_parser<T>(id: u64, f: impl FnOnce(&mut Parser) -> T) -> Option<T> {
    payload::<Mutex<Parser>>(id, TAG).map(|p| f(&mut p.lock().unwrap_or_else(|e| e.into_inner())))
}

/// Allocate a fresh parser handle.
pub fn parser_new() -> Handle {
    alloc(TAG, Arc::new(Mutex::new(Parser::default())))
}

/// Register `name` with `default` (re-registering overwrites the default).
pub fn str_flag(id: u64, name: &str, default: &str) -> bool {
    with_parser(id, |p| match find_str(p, name) {
        Some(i) => p.str_flags[i].1 = default.to_string(),
        None => p.str_flags.push((name.to_string(), default.to_string())),
    })
    .is_some()
}

/// Register `name` with `default` (re-registering overwrites the default).
pub fn int_flag(id: u64, name: &str, default: i64) -> bool {
    with_parser(id, |p| match find_int(p, name) {
        Some(i) => p.int_flags[i].1 = default,
        None => p.int_flags.push((name.to_string(), default)),
    })
    .is_some()
}

/// Register boolean `name` (default false).
pub fn bool_flag(id: u64, name: &str) -> bool {
    with_parser(id, |p| {
        if find_bool(p, name).is_none() {
            p.bool_flags.push(name.to_string());
        }
    })
    .is_some()
}

fn fail(p: &mut Parser, msg: String) -> bool {
    p.error = msg;
    false
}

/// Parse `args` into the parser. `true` on success; `false` leaves a
/// message in [`error_of`] (empty when `--help`/`-h` was passed — the
/// caller should show [`help_of`]).
pub fn parse(id: u64, args: &[String]) -> Option<bool> {
    with_parser(id, |p| {
        p.parsed_str.clear();
        p.parsed_int.clear();
        p.parsed_bool.clear();
        p.positionals.clear();
        p.error.clear();
        p.help_requested = false;
        let mut i = 0;
        let mut only_positional = false;
        while i < args.len() {
            let a = &args[i];
            if only_positional {
                p.positionals.push(a.clone());
            } else if a == "--" {
                only_positional = true;
            } else if a == "--help" || a == "-h" {
                p.help_requested = true;
                return false;
            } else if let Some(rest) = a.strip_prefix("--") {
                let (name, inline) = match rest.split_once('=') {
                    Some((n, v)) => (n, Some(v)),
                    None => (rest, None),
                };
                if name.is_empty() {
                    return fail(p, "empty flag name `--`".to_string());
                }
                if find_bool(p, name).is_some() {
                    let v = match inline {
                        None => true,
                        Some("true") | Some("1") => true,
                        Some("false") | Some("0") => false,
                        Some(other) => {
                            return fail(
                                p,
                                format!("flag --{name}: want true/false, got `{other}`"),
                            );
                        }
                    };
                    p.parsed_bool.insert(name.to_string(), v);
                } else if find_str(p, name).is_some() {
                    let v = match inline {
                        Some(v) => v.to_string(),
                        None => {
                            i += 1;
                            match args.get(i) {
                                Some(v) => v.clone(),
                                None => {
                                    return fail(p, format!("flag --{name} needs a value"));
                                }
                            }
                        }
                    };
                    p.parsed_str.insert(name.to_string(), v);
                } else if find_int(p, name).is_some() {
                    let raw = match inline {
                        Some(v) => v.to_string(),
                        None => {
                            i += 1;
                            match args.get(i) {
                                Some(v) => v.clone(),
                                None => {
                                    return fail(p, format!("flag --{name} needs a value"));
                                }
                            }
                        }
                    };
                    match raw.parse::<i64>() {
                        Ok(n) => {
                            p.parsed_int.insert(name.to_string(), n);
                        }
                        Err(_) => {
                            return fail(p, format!("flag --{name}: want int, got `{raw}`"));
                        }
                    }
                } else {
                    return fail(p, format!("unknown flag --{name}"));
                }
            } else if a.starts_with('-') && a.len() > 1 {
                return fail(p, format!("unknown flag {a} (only --long flags supported)"));
            } else {
                p.positionals.push(a.clone());
            }
            i += 1;
        }
        true
    })
}

/// Parsed `name`, falling back to its registered default (`""` unknown).
pub fn get_str(id: u64, name: &str) -> Option<String> {
    with_parser(id, |p| {
        p.parsed_str
            .get(name)
            .cloned()
            .or_else(|| find_str(p, name).map(|i| p.str_flags[i].1.clone()))
            .unwrap_or_default()
    })
}

/// Parsed `name`, falling back to its registered default (`0` unknown).
pub fn get_int(id: u64, name: &str) -> Option<i64> {
    with_parser(id, |p| {
        p.parsed_int
            .get(name)
            .copied()
            .or_else(|| find_int(p, name).map(|i| p.int_flags[i].1))
            .unwrap_or(0)
    })
}

/// Parsed `name` (`false` when absent or unknown).
pub fn get_bool(id: u64, name: &str) -> Option<bool> {
    with_parser(id, |p| p.parsed_bool.get(name).copied().unwrap_or(false))
}

/// Positional `i` (`None` when out of range).
pub fn positional(id: u64, i: usize) -> Option<Option<String>> {
    with_parser(id, |p| p.positionals.get(i).cloned())
}

/// First positional, or `""` when none.
pub fn subcommand(id: u64) -> Option<String> {
    with_parser(id, |p| p.positionals.first().cloned().unwrap_or_default())
}

/// Last parse error (`""` when the last parse succeeded, or when `--help`
/// was passed — show [`help_of`] then).
pub fn error_of(id: u64) -> Option<String> {
    with_parser(id, |p| p.error.clone())
}

/// Whether the last parse saw `--help`/`-h`.
pub fn was_help(id: u64) -> Option<bool> {
    with_parser(id, |p| p.help_requested)
}

/// Auto-generated help for `prog`.
pub fn help_of(id: u64, prog: &str) -> Option<String> {
    with_parser(id, |p| {
        let mut out = format!("Usage: {prog} [flags] [command]\n\nFlags:\n");
        for (name, default) in &p.str_flags {
            out.push_str(&format!("  --{name} <str> (default: {default})\n"));
        }
        for (name, default) in &p.int_flags {
            out.push_str(&format!("  --{name} <int> (default: {default})\n"));
        }
        for name in &p.bool_flags {
            out.push_str(&format!("  --{name}\n"));
        }
        out.push_str("  --help, -h\n");
        out
    })
}

fn set_err(err: *mut std::ffi::c_int) {
    if !err.is_null() {
        // SAFETY: codegen always passes a valid out-param; guard anyway.
        unsafe {
            *err = 1;
        }
    }
}

fn handle_of(h: &Handle) -> CValue {
    CValue::int(h.id as i64)
}

/// `args.parser() -> int` (parser handle id).
#[no_mangle]
pub extern "C" fn zz_args_parser(_unit: CValue, err: *mut std::ffi::c_int) -> CValue {
    let _ = err;
    handle_of(&parser_new())
}

fn parser_id(h: CValue) -> Option<u64> {
    h.as_i64().and_then(|n| (n > 0).then_some(n as u64))
}

/// `args.str_flag(h: int, name: str, default: str) -> unit`.
#[no_mangle]
pub extern "C" fn zz_args_str_flag(
    h: CValue,
    name: CValue,
    default: CValue,
    err: *mut std::ffi::c_int,
) -> CValue {
    match (
        parser_id(h),
        cvalue_to_string(name),
        cvalue_to_string(default),
    ) {
        (Some(id), Some(n), Some(d)) => {
            if !str_flag(id, &n, &d) {
                set_err(err);
            }
            CValue::unit()
        }
        _ => {
            set_err(err);
            CValue::unit()
        }
    }
}

/// `args.int_flag(h: int, name: str, default: int) -> unit`.
#[no_mangle]
pub extern "C" fn zz_args_int_flag(
    h: CValue,
    name: CValue,
    default: CValue,
    err: *mut std::ffi::c_int,
) -> CValue {
    match (parser_id(h), cvalue_to_string(name), default.as_i64()) {
        (Some(id), Some(n), Some(d)) => {
            if !int_flag(id, &n, d) {
                set_err(err);
            }
            CValue::unit()
        }
        _ => {
            set_err(err);
            CValue::unit()
        }
    }
}

/// `args.bool_flag(h: int, name: str) -> unit`.
#[no_mangle]
pub extern "C" fn zz_args_bool_flag(h: CValue, name: CValue, err: *mut std::ffi::c_int) -> CValue {
    match (parser_id(h), cvalue_to_string(name)) {
        (Some(id), Some(n)) => {
            if !bool_flag(id, &n) {
                set_err(err);
            }
            CValue::unit()
        }
        _ => {
            set_err(err);
            CValue::unit()
        }
    }
}

/// `args.parse(h: int, argv: [str]) -> bool`.
#[no_mangle]
pub extern "C" fn zz_args_parse(h: CValue, argv: CValue, err: *mut std::ffi::c_int) -> CValue {
    match (parser_id(h), cvalue_to_str_vec(argv)) {
        (Some(id), Some(args)) => match parse(id, &args) {
            Some(ok) => CValue::boolean(ok),
            None => {
                set_err(err);
                CValue::boolean(false)
            }
        },
        _ => {
            set_err(err);
            CValue::boolean(false)
        }
    }
}

/// `args.get_str(h: int, name: str) -> str`.
#[no_mangle]
pub extern "C" fn zz_args_get_str(h: CValue, name: CValue, err: *mut std::ffi::c_int) -> CValue {
    match (parser_id(h), cvalue_to_string(name)) {
        (Some(id), Some(n)) => match get_str(id, &n) {
            Some(s) => cvalue_str(s.as_bytes()),
            None => {
                set_err(err);
                cvalue_str(b"")
            }
        },
        _ => {
            set_err(err);
            cvalue_str(b"")
        }
    }
}

/// `args.get_int(h: int, name: str) -> int`.
#[no_mangle]
pub extern "C" fn zz_args_get_int(h: CValue, name: CValue, err: *mut std::ffi::c_int) -> CValue {
    match (parser_id(h), cvalue_to_string(name)) {
        (Some(id), Some(n)) => match get_int(id, &n) {
            Some(v) => CValue::int(v),
            None => {
                set_err(err);
                CValue::int(0)
            }
        },
        _ => {
            set_err(err);
            CValue::int(0)
        }
    }
}

/// `args.get_bool(h: int, name: str) -> bool`.
#[no_mangle]
pub extern "C" fn zz_args_get_bool(h: CValue, name: CValue, err: *mut std::ffi::c_int) -> CValue {
    match (parser_id(h), cvalue_to_string(name)) {
        (Some(id), Some(n)) => match get_bool(id, &n) {
            Some(v) => CValue::boolean(v),
            None => {
                set_err(err);
                CValue::boolean(false)
            }
        },
        _ => {
            set_err(err);
            CValue::boolean(false)
        }
    }
}

/// `args.positional(h: int, i: int) -> Option<str>`.
#[no_mangle]
pub extern "C" fn zz_args_positional(h: CValue, i: CValue, err: *mut std::ffi::c_int) -> CValue {
    match (parser_id(h), i.as_i64()) {
        (Some(id), Some(n)) if n >= 0 => match positional(id, n as usize) {
            // SAFETY: constructors are provided by the linked AOT program.
            Some(Some(s)) => unsafe { crate::cabi::zz_variant_some(cvalue_str(s.as_bytes())) },
            _ => CValue::none(),
        },
        _ => {
            set_err(err);
            CValue::none()
        }
    }
}

/// `args.subcommand(h: int) -> str`.
#[no_mangle]
pub extern "C" fn zz_args_subcommand(h: CValue, err: *mut std::ffi::c_int) -> CValue {
    match parser_id(h) {
        Some(id) => match subcommand(id) {
            Some(s) => cvalue_str(s.as_bytes()),
            None => {
                set_err(err);
                cvalue_str(b"")
            }
        },
        _ => {
            set_err(err);
            cvalue_str(b"")
        }
    }
}

/// `args.help(h: int, prog: str) -> str`.
#[no_mangle]
pub extern "C" fn zz_args_help(h: CValue, prog: CValue, err: *mut std::ffi::c_int) -> CValue {
    match (parser_id(h), cvalue_to_string(prog)) {
        (Some(id), Some(p)) => match help_of(id, &p) {
            Some(s) => cvalue_str(s.as_bytes()),
            None => {
                set_err(err);
                cvalue_str(b"")
            }
        },
        _ => {
            set_err(err);
            cvalue_str(b"")
        }
    }
}

/// `args.error(h: int) -> str`.
#[no_mangle]
pub extern "C" fn zz_args_error(h: CValue, err: *mut std::ffi::c_int) -> CValue {
    match parser_id(h) {
        Some(id) => match error_of(id) {
            Some(s) => cvalue_str(s.as_bytes()),
            None => {
                set_err(err);
                cvalue_str(b"")
            }
        },
        _ => {
            set_err(err);
            cvalue_str(b"")
        }
    }
}

/// `args.was_help(h: int) -> bool`.
#[no_mangle]
pub extern "C" fn zz_args_was_help(h: CValue, err: *mut std::ffi::c_int) -> CValue {
    match parser_id(h) {
        Some(id) => match was_help(id) {
            Some(v) => CValue::boolean(v),
            None => {
                set_err(err);
                CValue::boolean(false)
            }
        },
        _ => {
            set_err(err);
            CValue::boolean(false)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn configured() -> Handle {
        let h = parser_new();
        assert!(str_flag(h.id, "output", "a.out"));
        assert!(int_flag(h.id, "count", 1));
        assert!(bool_flag(h.id, "verbose"));
        h
    }

    fn args(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn long_space_and_equals_forms() {
        let h = configured();
        assert_eq!(
            parse(
                h.id,
                &args(&["--output", "b.out", "--count=3", "--verbose"])
            ),
            Some(true)
        );
        assert_eq!(get_str(h.id, "output"), Some("b.out".to_string()));
        assert_eq!(get_int(h.id, "count"), Some(3));
        assert_eq!(get_bool(h.id, "verbose"), Some(true));
        assert_eq!(subcommand(h.id), Some(String::new()));
        crate::drop_handle(h.id);
    }

    #[test]
    fn defaults_and_missing() {
        let h = configured();
        assert_eq!(parse(h.id, &args(&[])), Some(true));
        assert_eq!(get_str(h.id, "output"), Some("a.out".to_string()));
        assert_eq!(get_int(h.id, "count"), Some(1));
        assert_eq!(get_bool(h.id, "verbose"), Some(false));
        assert_eq!(get_str(h.id, "nope"), Some(String::new()));
        assert_eq!(positional(h.id, 0), Some(None));
        crate::drop_handle(h.id);
    }

    #[test]
    fn positionals_terminator_and_subcommand() {
        let h = configured();
        assert_eq!(
            parse(h.id, &args(&["serve", "--", "--verbose", "pos"])),
            Some(true)
        );
        // --verbose after -- is positional, not a flag.
        assert_eq!(get_bool(h.id, "verbose"), Some(false));
        assert_eq!(subcommand(h.id), Some("serve".to_string()));
        assert_eq!(positional(h.id, 0), Some(Some("serve".to_string())));
        assert_eq!(positional(h.id, 2), Some(Some("pos".to_string())));
        crate::drop_handle(h.id);
    }

    #[test]
    fn errors_and_help() {
        let h = configured();
        assert_eq!(parse(h.id, &args(&["--nope"])), Some(false));
        assert_eq!(error_of(h.id), Some("unknown flag --nope".to_string()));
        assert_eq!(parse(h.id, &args(&["--count"])), Some(false));
        assert!(error_of(h.id).unwrap().contains("needs a value"));
        assert_eq!(parse(h.id, &args(&["--count", "x"])), Some(false));
        assert!(error_of(h.id).unwrap().contains("want int"));
        assert_eq!(parse(h.id, &args(&["--help"])), Some(false));
        assert_eq!(error_of(h.id), Some(String::new()));
        assert_eq!(was_help(h.id), Some(true));
        let help = help_of(h.id, "myapp").unwrap();
        assert!(help.contains("Usage: myapp"), "got: {help}");
        assert!(help.contains("--output <str> (default: a.out)"));
        assert!(help.contains("--count <int> (default: 1)"));
        assert!(help.contains("--verbose"));
        crate::drop_handle(h.id);
    }

    #[test]
    fn bool_equals_forms() {
        let h = configured();
        assert_eq!(parse(h.id, &args(&["--verbose=false"])), Some(true));
        assert_eq!(get_bool(h.id, "verbose"), Some(false));
        assert_eq!(parse(h.id, &args(&["--verbose=1"])), Some(true));
        assert_eq!(get_bool(h.id, "verbose"), Some(true));
        assert_eq!(parse(h.id, &args(&["--verbose=maybe"])), Some(false));
        crate::drop_handle(h.id);
    }

    #[test]
    fn foreign_handle_rejected() {
        assert!(!str_flag(999_999_999, "x", "y"));
        assert_eq!(parse(999_999_999, &[]), None);
    }
}
