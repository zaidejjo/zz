//! Direct-C plugin ABI: VM dispatch for pure-C native plugins.
//!
//! A C-only plugin is a shared library exporting its `plugin.zzi` functions
//! as plain C symbols plus a `ZZ_C_PLUGIN_ABI_VERSION` data symbol — no Rust
//! shim, no `zz_plugin_register`. [`register`] records each symbol's pointer
//! and signature in a process-global registry; [`call`] marshals a VM call
//! into a typed `extern "C"` invocation; [`native_value`] exposes the arity
//! for lazy `Value::Native` construction at lookup sites.
//!
//! C type contract (mirrors what AOT codegen already assumes in
//! `zz_codegen::lower::extern_call`):
//! - manifest `int` → C `int`, `long`, or `void*` (opaque handles travel as
//!   full 64-bit values; enum codes use the low 32 bits). Safe on System V
//!   AMD64 and AAPCS64: integer args ride in 64-bit registers.
//! - manifest `float` → C `double` (ZZ floats are `f64`; a C `float` param
//!   would misread the register — always declare `double`).
//! - manifest `str` → C `const char*`, NUL-terminated, valid for the call
//!   only. The wrapper must not retain the pointer.
//! - manifest `void`/`unit` returns → C `void`; `int` returns arrive
//!   sign-extended and are read as `i64`.
//!
//! `bool` params and `ptr` params are rejected at registration with an
//! actionable error (extend the shape table below when a plugin needs them).

use std::collections::HashMap;
use std::ffi::{c_void, CString};
use std::sync::{OnceLock, RwLock};

use crate::runtime::EvalError;
use crate::value::{NativeFunc, Value};
use crate::Span;

/// A C-callable parameter kind (subset of the `plugin.zzi` allowlist).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CParam {
    Int,
    Float,
    Str,
}

/// A C-callable return kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CRet {
    Int,
    Float,
    Void,
}

/// A plugin function's C signature: ordered params + return.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CSig {
    pub params: Vec<CParam>,
    pub ret: CRet,
}

/// Current C-plugin ABI version (the `ZZ_C_PLUGIN_ABI_VERSION` symbol).
pub const C_ABI_VERSION: u32 = 1;

struct CEntry {
    ptr: *mut c_void,
    sig: CSig,
}

// Raw fn pointers are safe to share: the loader keeps the dlopen handle
// alive for the process lifetime, so targets are never unmapped.
unsafe impl Send for CEntry {}
unsafe impl Sync for CEntry {}

static REGISTRY: OnceLock<RwLock<HashMap<String, CEntry>>> = OnceLock::new();

fn registry() -> &'static RwLock<HashMap<String, CEntry>> {
    REGISTRY.get_or_init(|| RwLock::new(HashMap::new()))
}

/// Shape key for adapter selection: e.g. `[Int, Float] -> Int` is `"IF>i"`.
fn shape_key(sig: &CSig) -> String {
    let mut k = String::with_capacity(sig.params.len() + 3);
    for p in &sig.params {
        k.push(match p {
            CParam::Int => 'I',
            CParam::Float => 'F',
            CParam::Str => 'S',
        });
    }
    k.push('>');
    k.push(match sig.ret {
        CRet::Int => 'i',
        CRet::Float => 'f',
        CRet::Void => 'v',
    });
    k
}

/// Adapter implementations, stamped per supported shape. Each takes the raw
/// symbol pointer plus already-arity-checked args. Split by return kind so
/// no nested macro dispatch is needed.
macro_rules! adapters_i {
    ($( $name:ident ( $( $idx:tt : $t:ident ),* ) ; )*) => {
        $(#[allow(unused_variables)]
        fn $name(
            ptr: *mut c_void,
            args: &mut [Value],
            span: Span,
            fname: &str,
        ) -> Result<Value, EvalError> {
            let f: extern "C" fn($( adapters_i!(@ctype $t) ),*) -> i64 =
                unsafe { std::mem::transmute(ptr) };
            let mut _held: Vec<CString> = Vec::new();
            let out = f($( adapters_i!(@extract args span fname $idx $t _held) ),*);
            Ok(Value::Int(out))
        })*
    };
    (@ctype I) => { i64 };
    (@ctype F) => { f64 };
    (@ctype S) => { *const i8 };
    (@extract $args:ident $span:ident $fname:ident $idx:tt I $held:ident) => {{
        match &$args[$idx] {
            Value::Int(i) => *i,
            other => {
                return Err(EvalError::new(
                    format!("`{}`: expected int, got `{other}`", $fname),
                    $span,
                ));
            }
        }
    }};
    (@extract $args:ident $span:ident $fname:ident $idx:tt F $held:ident) => {{
        match &$args[$idx] {
            Value::Float(f) => *f,
            // Leniency mirrors the historical Rust shims (`as_float!`):
            // an int where a float is expected widens instead of failing.
            Value::Int(i) => *i as f64,
            other => {
                return Err(EvalError::new(
                    format!("`{}`: expected float, got `{other}`", $fname),
                    $span,
                ));
            }
        }
    }};
    (@extract $args:ident $span:ident $fname:ident $idx:tt S $held:ident) => {{
        match &$args[$idx] {
            Value::Str(s) => match CString::new(s.as_str()) {
                Ok(c) => {
                    let p = c.as_ptr();
                    $held.push(c);
                    p
                }
                Err(_) => {
                    return Err(EvalError::new(
                        format!("`{}`: interior NUL byte in string", $fname),
                        $span,
                    ));
                }
            },
            other => {
                return Err(EvalError::new(
                    format!("`{}`: expected str, got `{other}`", $fname),
                    $span,
                ));
            }
        }
    }};
}

macro_rules! adapters_v {
    ($( $name:ident ( $( $idx:tt : $t:ident ),* ) ; )*) => {
        $(#[allow(unused_variables)]
        fn $name(
            ptr: *mut c_void,
            args: &mut [Value],
            span: Span,
            fname: &str,
        ) -> Result<Value, EvalError> {
            let f: extern "C" fn($( adapters_v!(@ctype $t) ),*) =
                unsafe { std::mem::transmute(ptr) };
            let mut _held: Vec<CString> = Vec::new();
            f($( adapters_v!(@extract args span fname $idx $t _held) ),*);
            Ok(Value::Unit)
        })*
    };
    (@ctype I) => { i64 };
    (@ctype F) => { f64 };
    (@ctype S) => { *const i8 };
    (@extract $args:ident $span:ident $fname:ident $idx:tt I $held:ident) => {{
        match &$args[$idx] {
            Value::Int(i) => *i,
            other => {
                return Err(EvalError::new(
                    format!("`{}`: expected int, got `{other}`", $fname),
                    $span,
                ));
            }
        }
    }};
    (@extract $args:ident $span:ident $fname:ident $idx:tt F $held:ident) => {{
        match &$args[$idx] {
            Value::Float(f) => *f,
            Value::Int(i) => *i as f64,
            other => {
                return Err(EvalError::new(
                    format!("`{}`: expected float, got `{other}`", $fname),
                    $span,
                ));
            }
        }
    }};
    (@extract $args:ident $span:ident $fname:ident $idx:tt S $held:ident) => {{
        match &$args[$idx] {
            Value::Str(s) => match CString::new(s.as_str()) {
                Ok(c) => {
                    let p = c.as_ptr();
                    $held.push(c);
                    p
                }
                Err(_) => {
                    return Err(EvalError::new(
                        format!("`{}`: interior NUL byte in string", $fname),
                        $span,
                    ));
                }
            },
            other => {
                return Err(EvalError::new(
                    format!("`{}`: expected str, got `{other}`", $fname),
                    $span,
                ));
            }
        }
    }};
}

adapters_i! {
    c0_v0();
    c1_s(0: S);
    c1_i(0: I);
    c2_ii(0: I, 1: I);
    c2_if(0: I, 1: F);
    c2_ss(0: S, 1: S);
    c4_iiii(0: I, 1: I, 2: I, 3: I);
    c5_iiiii(0: I, 1: I, 2: I, 3: I, 4: I);
    c3_ifi(0: I, 1: F, 2: I);
    c3_iff(0: I, 1: F, 2: F);
    c6_ifiiii(0: I, 1: F, 2: I, 3: I, 4: I, 5: I);
    c2_is(0: I, 1: S);
    c3_isi(0: I, 1: S, 2: I);
}

adapters_v! {
    c0_v0_void();
    c1_i_void(0: I);
}

macro_rules! adapters_f {
    ($( $name:ident ( $( $idx:tt : $t:ident ),* ) ; )*) => {
        $(#[allow(unused_variables)]
        fn $name(
            ptr: *mut c_void,
            args: &mut [Value],
            span: Span,
            fname: &str,
        ) -> Result<Value, EvalError> {
            let f: extern "C" fn($( adapters_f!(@ctype $t) ),*) -> f64 =
                unsafe { std::mem::transmute(ptr) };
            let mut _held: Vec<CString> = Vec::new();
            let out = f($( adapters_f!(@extract args span fname $idx $t _held) ),*);
            Ok(Value::Float(out))
        })*
    };
    (@ctype I) => { i64 };
    (@ctype F) => { f64 };
    (@ctype S) => { *const i8 };
    (@extract $args:ident $span:ident $fname:ident $idx:tt I $held:ident) => {{
        match &$args[$idx] {
            Value::Int(i) => *i,
            other => {
                return Err(EvalError::new(
                    format!("`{}`: expected int, got `{other}`", $fname),
                    $span,
                ));
            }
        }
    }};
    (@extract $args:ident $span:ident $fname:ident $idx:tt F $held:ident) => {{
        match &$args[$idx] {
            Value::Float(f) => *f,
            Value::Int(i) => *i as f64,
            other => {
                return Err(EvalError::new(
                    format!("`{}`: expected float, got `{other}`", $fname),
                    $span,
                ));
            }
        }
    }};
    (@extract $args:ident $span:ident $fname:ident $idx:tt S $held:ident) => {{
        match &$args[$idx] {
            Value::Str(s) => match CString::new(s.as_str()) {
                Ok(c) => {
                    let p = c.as_ptr();
                    $held.push(c);
                    p
                }
                Err(_) => {
                    return Err(EvalError::new(
                        format!("`{}`: interior NUL byte in string", $fname),
                        $span,
                    ));
                }
            },
            other => {
                return Err(EvalError::new(
                    format!("`{}`: expected str, got `{other}`", $fname),
                    $span,
                ));
            }
        }
    }};
}

adapters_f! {
    c1_i_f(0: I);
}

type Adapter = fn(*mut c_void, &mut [Value], Span, &str) -> Result<Value, EvalError>;

/// Map a signature to its adapter. `None` = valid ABI but no stamped shape
/// yet — the registration error names the shape so the fix is one macro line.
fn adapter_for(sig: &CSig) -> Option<Adapter> {
    Some(match shape_key(sig).as_str() {
        ">i" => c0_v0,
        ">v" => c0_v0_void,
        "S>i" => c1_s,
        "I>i" => c1_i,
        "I>v" => c1_i_void,
        "II>i" => c2_ii,
        "IF>i" => c2_if,
        "SS>i" => c2_ss,
        "IIII>i" => c4_iiii,
        "IIIII>i" => c5_iiiii,
        "IFI>i" => c3_ifi,
        "IFF>i" => c3_iff,
        "IFIIII>i" => c6_ifiiii,
        "IS>i" => c2_is,
        "ISI>i" => c3_isi,
        "I>f" => c1_i_f,
        _ => return None,
    })
}

/// Register a C plugin function. Overwrites any previous registration under
/// `name` (last-writer-wins; tests must use unique names).
pub fn register(name: &str, ptr: *mut c_void, sig: CSig) -> Result<(), String> {
    if ptr.is_null() {
        return Err(format!("C plugin `{name}`: null symbol pointer"));
    }
    let key = shape_key(&sig);
    if adapter_for(&sig).is_none() {
        return Err(format!(
            "C plugin `{name}`: unsupported signature shape `{key}`\n\
             hint: add one adapter line for `{key}` in zz_runtime::c_abi"
        ));
    }
    let arity = sig.params.len();
    registry()
        .write()
        .expect("C plugin registry lock")
        .insert(name.to_string(), CEntry { ptr, sig });
    debug_assert_eq!(
        registry()
            .read()
            .expect("C plugin registry lock")
            .get(name)
            .map(|e| e.sig.params.len()),
        Some(arity)
    );
    Ok(())
}

/// Arity of a registered C function, if present.
pub fn arity_of(name: &str) -> Option<usize> {
    registry()
        .read()
        .expect("C plugin registry lock")
        .get(name)
        .map(|e| e.sig.params.len())
}

/// Lazy native value for lookup sites (mirrors the `natives`-map path).
pub fn native_value(name: &str) -> Option<Value> {
    arity_of(name).map(|arity| {
        Value::Native(Box::new(NativeFunc {
            name: name.to_string(),
            arity,
        }))
    })
}

/// Invoke a registered C function. `None` = not a C plugin name (caller
/// falls through to the Rust registry / unknown-native error).
pub fn call(name: &str, args: &mut [Value], span: Span) -> Option<Result<Value, EvalError>> {
    let guard = registry().read().expect("C plugin registry lock");
    let entry = guard.get(name)?;
    let (ptr, adapter) = (entry.ptr, adapter_for(&entry.sig)?);
    let fname = name.to_string();
    drop(guard);
    if args.len() != arity_of(&fname).unwrap_or(usize::MAX) {
        return Some(Err(EvalError::new(
            format!(
                "`{fname}`: expected {} arguments, found {}",
                arity_of(&fname).unwrap_or(0),
                args.len()
            ),
            span,
        )));
    }
    Some(adapter(ptr, args, span, &fname))
}

#[cfg(test)]
mod tests {
    use super::*;

    // In-process stand-ins for C symbols: real `extern "C"` fns whose
    // addresses register exactly like dlsym results.
    extern "C" fn t_add(a: i64, b: i64) -> i64 {
        a + b
    }
    extern "C" fn t_half(x: i64) -> f64 {
        x as f64 / 2.0
    }
    extern "C" fn t_greet(name: *const i8) -> i64 {
        if name.is_null() {
            return -1;
        }
        let s = unsafe { std::ffi::CStr::from_ptr(name) };
        s.to_bytes().len() as i64
    }
    extern "C" fn t_scale(h: i64, f: f64) -> i64 {
        (h as f64 * f) as i64
    }
    extern "C" fn t_ping() -> i64 {
        7
    }
    extern "C" fn t_sink(h: i64) {
        let _ = h;
    }
    extern "C" fn t_save(h: i64, path: *const i8, q: i64) -> i64 {
        if path.is_null() {
            return -1;
        }
        h + q
    }

    fn reg(name: &str, f: *mut c_void, params: Vec<CParam>, ret: CRet) {
        register(name, f, CSig { params, ret }).expect("test registration");
    }

    fn span() -> Span {
        Span::new(0, 0)
    }

    fn expect_int(name: &str, mut args: Vec<Value>, want: i64) {
        match call(name, &mut args, span()) {
            Some(Ok(Value::Int(got))) => assert_eq!(got, want, "{name}"),
            other => panic!("{name}: expected Int({want}), got {other:?}"),
        }
    }

    #[test]
    fn int_pair_calls_through() {
        reg(
            "cabi_t_add",
            t_add as *mut c_void,
            vec![CParam::Int, CParam::Int],
            CRet::Int,
        );
        expect_int("cabi_t_add", vec![Value::Int(40), Value::Int(2)], 42);
        assert_eq!(arity_of("cabi_t_add"), Some(2));
    }

    #[test]
    fn float_return_and_int_widening() {
        reg(
            "cabi_t_half",
            t_half as *mut c_void,
            vec![CParam::Int],
            CRet::Float,
        );
        let mut args = vec![Value::Int(5)];
        match call("cabi_t_half", &mut args, span()) {
            Some(Ok(Value::Float(got))) => assert!((got - 2.5).abs() < 1e-12),
            other => panic!("expected Float(2.5), got {other:?}"),
        }
    }

    #[test]
    fn str_param_crosses_as_cstring() {
        reg(
            "cabi_t_greet",
            t_greet as *mut c_void,
            vec![CParam::Str],
            CRet::Int,
        );
        expect_int(
            "cabi_t_greet",
            vec![Value::Str(Box::new("hello".to_string()))],
            5,
        );
    }

    #[test]
    fn mixed_int_float_shape() {
        reg(
            "cabi_t_scale",
            t_scale as *mut c_void,
            vec![CParam::Int, CParam::Float],
            CRet::Int,
        );
        // Int widens to float (shim leniency).
        expect_int("cabi_t_scale", vec![Value::Int(21), Value::Int(2)], 42);
    }

    #[test]
    fn void_shapes_yield_unit() {
        reg(
            "cabi_t_sink",
            t_sink as *mut c_void,
            vec![CParam::Int],
            CRet::Void,
        );
        let mut args = vec![Value::Int(1)];
        match call("cabi_t_sink", &mut args, span()) {
            Some(Ok(Value::Unit)) => {}
            other => panic!("expected Unit, got {other:?}"),
        }
        reg("cabi_t_ping", t_ping as *mut c_void, vec![], CRet::Int);
        expect_int("cabi_t_ping", vec![], 7);
    }

    #[test]
    fn int_str_int_shape() {
        reg(
            "cabi_t_save",
            t_save as *mut c_void,
            vec![CParam::Int, CParam::Str, CParam::Int],
            CRet::Int,
        );
        expect_int(
            "cabi_t_save",
            vec![
                Value::Int(100),
                Value::Str(Box::new("a.png".to_string())),
                Value::Int(5),
            ],
            105,
        );
    }

    #[test]
    fn type_mismatch_is_call_site_error() {
        let mut args = vec![Value::Str(Box::new("x".to_string())), Value::Int(2)];
        let out = call("cabi_t_add", &mut args, span()).expect("registered");
        assert!(out.is_err());
    }

    #[test]
    fn unknown_name_falls_through() {
        let mut args = vec![];
        assert!(call("cabi_definitely_missing", &mut args, span()).is_none());
        assert!(arity_of("cabi_definitely_missing").is_none());
        assert!(native_value("cabi_definitely_missing").is_none());
    }

    #[test]
    fn unsupported_shape_rejected_with_hint() {
        // 7-int params: valid ABI, no stamped adapter.
        let err = register(
            "cabi_t_wide",
            t_ping as *mut c_void,
            CSig {
                params: vec![CParam::Int; 7],
                ret: CRet::Int,
            },
        )
        .unwrap_err();
        assert!(err.contains("unsupported signature shape"), "{err}");
        assert!(err.contains("c_abi"), "{err}");
    }

    #[test]
    fn native_value_carries_arity() {
        match native_value("cabi_t_add") {
            Some(Value::Native(nf)) => assert_eq!(nf.arity, 2),
            other => panic!("expected native, got {other:?}"),
        }
    }
}
