//! `std.sys` natives (VM side).
//!
//! Thin adapters between [`Value`] and the shared implementation in
//! `zz_native_rt::sys` (which also backs the AOT `zz_sys_*` FFI). All
//! queries are infallible from ZZ's perspective (empty string / fallback
//! values instead of errors).

use zz_runtime::{EvalError, Interp, Span, Value};
macro_rules! str_native {
    ($name:ident, $func:ident) => {
        pub(crate) fn $name(
            _interp: &mut Interp,
            _args: &mut Vec<Value>,
            _span: Span,
        ) -> Result<Value, EvalError> {
            Ok(Value::Str(Box::new(zz_native_rt::sys::$func().to_string())))
        }
    };
}

macro_rules! int_native {
    ($name:ident, $func:ident) => {
        pub(crate) fn $name(
            _interp: &mut Interp,
            _args: &mut Vec<Value>,
            _span: Span,
        ) -> Result<Value, EvalError> {
            Ok(Value::Int(zz_native_rt::sys::$func()))
        }
    };
}

str_native!(sys_os, os);
str_native!(sys_arch, arch);
int_native!(sys_cpu_count, cpu_count);
str_native!(sys_hostname, hostname);
int_native!(sys_total_mem, total_mem);
int_native!(sys_avail_mem, avail_mem);
