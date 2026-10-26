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

use std::collections::{HashMap, HashSet};
use std::ffi::{c_char, c_void, CString};
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
    /// Borrowed byte view: one ZZ `bytes` arg expands to TWO C params
    /// (`*const u8` + `usize`, no copy, no NUL). Stamps pair it with a
    /// same-index `Len` slot (see below); both error loudly on non-bytes.
    Bytes,
    /// Opaque integer-handle (#234): crosses C as `i64` (`int64_t`). The
    /// runtime tracks live handle ids in a validity registry so
    /// use-after-free surfaces as a clean ZZ error instead of UB.
    Handle,
    /// Length half of a [`CParam::Bytes`] pair: always follows its `Bytes`
    /// at the same arg index. Never constructed from a real parameter.
    Len,
}

/// A C-callable return kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CRet {
    Int,
    Float,
    Void,
    /// Opaque integer-handle return (#234): crosses C as `i64`.
    Handle,
    /// Owned `bytes` return (#234): C fills a caller-provided
    /// `zz_bytes*` out-slot and returns `int` status; the ZZ side owns
    /// the result and frees it via `zz_release_bytes`.
    Bytes,
    /// Owned `str` return (#234): C fills a caller-provided
    /// `zz_str*` out-slot and returns `int` status; the ZZ side owns
    /// the result and frees it via `zz_release`.
    Str,
    /// `Result<int, str>` return (#234): C returns `int` status and writes
    /// the error message into a caller-provided `char **` message slot.
    /// The ZZ side maps status 0 → `Ok(value)`, nonzero → `Err(msg)`.
    ResultIntStr,
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

/// Live opaque-handle ids created by C plugins (#234). Plugins register a
/// handle id when they create one and release it when they free it; the ZZ
/// side checks validity on every handle argument so use-after-free surfaces
/// as a clean ZZ error instead of UB.
///
/// Bounded to prevent unbounded growth (a plugin that creates handles in a
/// loop without releasing them would otherwise leak forever). When the cap
/// is hit, the oldest entry is evicted. This keeps RSS bounded even under
/// pathological plugin behavior.
static HANDLES: OnceLock<RwLock<HashSet<i64>>> = OnceLock::new();

/// Maximum number of live handle ids tracked at once.
const MAX_HANDLES: usize = 10_000;

fn handles() -> &'static RwLock<HashSet<i64>> {
    HANDLES.get_or_init(|| RwLock::new(HashSet::new()))
}

/// Register a handle id as live. Returns false if already registered.
/// If the registry is full, the oldest entry is evicted to make room.
pub fn register_handle(id: i64) -> bool {
    let mut h = handles().write().expect("handle registry lock");
    if h.len() >= MAX_HANDLES {
        // Evict the oldest entry (HashSet has no order, so remove any one).
        if let Some(first) = h.iter().next().copied() {
            h.remove(&first);
        }
    }
    h.insert(id)
}

/// Release a handle id, marking it no longer valid. Returns false if it was
/// not registered. Plugins must call this when freeing a handle; the ZZ side
/// cannot auto-release because `Value::Int` carries no type tag distinguishing
/// a handle id from a plain integer.
pub fn release_handle(id: i64) -> bool {
    handles().write().expect("handle registry lock").remove(&id)
}

/// True when `id` is a live handle.
pub fn is_handle_valid(id: i64) -> bool {
    handles()
        .read()
        .expect("handle registry lock")
        .contains(&id)
}

fn registry() -> &'static RwLock<HashMap<String, CEntry>> {
    REGISTRY.get_or_init(|| RwLock::new(HashMap::new()))
}

/// Shape key for adapter selection: e.g. `[Int, Float] -> Int` is `"IF>i"`.
fn shape_key(sig: &CSig) -> String {
    let mut k = String::with_capacity(sig.params.len() + 3);
    for p in &sig.params {
        // One ZZ arg is one key letter: a bytes view still counts as a
        // single `B` (its `(ptr, len)` expansion lives in the stamp).
        k.push(match p {
            CParam::Int => 'I',
            CParam::Float => 'F',
            CParam::Str => 'S',
            CParam::Bytes => 'B',
            CParam::Handle => 'H',
            // `Len` is a stamp-level pairing device, never a real param.
            CParam::Len => continue,
        });
    }
    k.push('>');
    k.push(match sig.ret {
        CRet::Int => 'i',
        CRet::Float => 'f',
        CRet::Void => 'v',
        CRet::Handle => 'h',
        CRet::Bytes => 'b',
        CRet::Str => 's',
        CRet::ResultIntStr => 'r',
    });
    k
}

/// Adapter implementations, stamped per supported shape. Each takes the raw
/// symbol pointer plus already-arity-checked args. Split by return kind so
/// no nested macro dispatch is needed.
macro_rules! adapters_i {
    ($( $name:ident ( $( $idx:tt : $t:ident ),* ) ; )*) => {
        $(#[allow(unused_variables)]
        #[allow(unused_mut)]
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
    (@ctype S) => { *const c_char };
    (@ctype B) => { *const u8 };
    (@ctype H) => { i64 };
    (@ctype L) => { usize };
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
    (@extract $args:ident $span:ident $fname:ident $idx:tt B $held:ident) => {{
        // Borrowed view: pointer into the Arc-backed buffer, no copy,
        // no NUL termination. The `args` slice outlives the call, so no
        // `_held` pinning is needed — but the C side must not retain it
        // (same contract as borrowed `str`, without even the NUL).
        match &$args[$idx] {
            Value::Bytes(b) => b.as_slice().as_ptr(),
            other => {
                return Err(EvalError::new(
                    format!("`{}`: expected bytes, got `{other}`", $fname),
                    $span,
                ));
            }
        }
    }};
    (@extract $args:ident $span:ident $fname:ident $idx:tt H $held:ident) => {{
        // Opaque handle id crosses as i64; validity is checked by the
        // handle registry at call time (use-after-free → clean error).
        match &$args[$idx] {
            Value::Int(i) => {
                if !is_handle_valid(*i) {
                    return Err(EvalError::new(
                        format!("`{}`: handle `{i}` is not live (use-after-free)", $fname),
                        $span,
                    ));
                }
                *i
            }
            other => {
                return Err(EvalError::new(
                    format!("`{}`: expected handle, got `{other}`", $fname),
                    $span,
                ));
            }
        }
    }};
    (@extract $args:ident $span:ident $fname:ident $idx:tt L $held:ident) => {{
        match &$args[$idx] {
            Value::Bytes(b) => b.as_slice().len(),
            other => {
                return Err(EvalError::new(
                    format!("`{}`: expected bytes, got `{other}`", $fname),
                    $span,
                ));
            }
        }
    }};
}

macro_rules! adapters_v {
    ($( $name:ident ( $( $idx:tt : $t:ident ),* ) ; )*) => {
        $(#[allow(unused_variables)]
        #[allow(unused_mut)]
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
    (@ctype S) => { *const c_char };
    (@ctype B) => { *const u8 };
    (@ctype H) => { i64 };
    (@ctype L) => { usize };
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
    (@extract $args:ident $span:ident $fname:ident $idx:tt B $held:ident) => {{
        // Borrowed view: pointer into the Arc-backed buffer, no copy,
        // no NUL termination. The `args` slice outlives the call, so no
        // `_held` pinning is needed — but the C side must not retain it
        // (same contract as borrowed `str`, without even the NUL).
        match &$args[$idx] {
            Value::Bytes(b) => b.as_slice().as_ptr(),
            other => {
                return Err(EvalError::new(
                    format!("`{}`: expected bytes, got `{other}`", $fname),
                    $span,
                ));
            }
        }
    }};
    (@extract $args:ident $span:ident $fname:ident $idx:tt H $held:ident) => {{
        // Opaque handle id crosses as i64; validity is checked by the
        // handle registry at call time (use-after-free → clean error).
        match &$args[$idx] {
            Value::Int(i) => {
                if !is_handle_valid(*i) {
                    return Err(EvalError::new(
                        format!("`{}`: handle `{i}` is not live (use-after-free)", $fname),
                        $span,
                    ));
                }
                *i
            }
            other => {
                return Err(EvalError::new(
                    format!("`{}`: expected handle, got `{other}`", $fname),
                    $span,
                ));
            }
        }
    }};
    (@extract $args:ident $span:ident $fname:ident $idx:tt L $held:ident) => {{
        match &$args[$idx] {
            Value::Bytes(b) => b.as_slice().len(),
            other => {
                return Err(EvalError::new(
                    format!("`{}`: expected bytes, got `{other}`", $fname),
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
    // Borrowed bytes view: one ZZ arg, two C params (ptr + len).
    c1_b(0: B, 0: L);
    // Opaque integer-handle: one ZZ arg, one C i64 param.
    c1_h(0: H);
}

adapters_v! {
    c0_v0_void();
    c1_i_void(0: I);
    c1_h_void(0: H);
}

macro_rules! adapters_f {
    ($( $name:ident ( $( $idx:tt : $t:ident ),* ) ; )*) => {
        $(#[allow(unused_variables)]
        #[allow(unused_mut)]
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
    (@ctype S) => { *const c_char };
    (@ctype B) => { *const u8 };
    (@ctype H) => { i64 };
    (@ctype L) => { usize };
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
    (@extract $args:ident $span:ident $fname:ident $idx:tt B $held:ident) => {{
        // Borrowed view: pointer into the Arc-backed buffer, no copy,
        // no NUL termination. The `args` slice outlives the call, so no
        // `_held` pinning is needed — but the C side must not retain it
        // (same contract as borrowed `str`, without even the NUL).
        match &$args[$idx] {
            Value::Bytes(b) => b.as_slice().as_ptr(),
            other => {
                return Err(EvalError::new(
                    format!("`{}`: expected bytes, got `{other}`", $fname),
                    $span,
                ));
            }
        }
    }};
    (@extract $args:ident $span:ident $fname:ident $idx:tt H $held:ident) => {{
        // Opaque handle id crosses as i64; validity is checked by the
        // handle registry at call time (use-after-free → clean error).
        match &$args[$idx] {
            Value::Int(i) => {
                if !is_handle_valid(*i) {
                    return Err(EvalError::new(
                        format!("`{}`: handle `{i}` is not live (use-after-free)", $fname),
                        $span,
                    ));
                }
                *i
            }
            other => {
                return Err(EvalError::new(
                    format!("`{}`: expected handle, got `{other}`", $fname),
                    $span,
                ));
            }
        }
    }};
    (@extract $args:ident $span:ident $fname:ident $idx:tt L $held:ident) => {{
        match &$args[$idx] {
            Value::Bytes(b) => b.as_slice().len(),
            other => {
                return Err(EvalError::new(
                    format!("`{}`: expected bytes, got `{other}`", $fname),
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
        "B>i" => c1_b,
        "H>i" => c1_h,
        "H>v" => c1_h_void,
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
    extern "C" fn t_greet(name: *const c_char) -> i64 {
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
    // Borrowed bytes view: pointer + length, no copy, no NUL.
    extern "C" fn t_sum_bytes(ptr: *const u8, len: usize) -> i64 {
        if ptr.is_null() {
            return -1;
        }
        let slice = unsafe { std::slice::from_raw_parts(ptr, len) };
        slice.iter().map(|b| *b as i64).sum()
    }
    extern "C" fn t_save(h: i64, path: *const c_char, q: i64) -> i64 {
        if path.is_null() {
            return -1;
        }
        h + q
    }
    // Opaque handle: t_use_handle consumes a live handle id.
    extern "C" fn t_use_handle(h: i64) -> i64 {
        h * 10
    }
    extern "C" fn t_use_handle_void(h: i64) {
        let _ = h;
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
            vec![Value::Str(std::sync::Arc::new("hello".to_string()))],
            5,
        );
    }

    #[test]
    fn bytes_param_crosses_as_ptr_and_len() {
        reg(
            "cabi_t_sum_bytes",
            t_sum_bytes as *mut c_void,
            vec![CParam::Bytes],
            CRet::Int,
        );
        expect_int(
            "cabi_t_sum_bytes",
            vec![Value::Bytes(Box::new(crate::value::BytesData::new(vec![
                1, 2, 3, 250,
            ])))],
            256,
        );
        // Arity stays ZZ-level: one arg, not two.
        assert_eq!(arity_of("cabi_t_sum_bytes"), Some(1));
        // Mismatched values fail loudly, naming the function.
        let mut args = vec![Value::Int(7)];
        match call("cabi_t_sum_bytes", &mut args, span()) {
            Some(Err(e)) => assert!(e.message.contains("expected bytes"), "{e:?}"),
            other => panic!("expected bytes error, got {other:?}"),
        }
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
                Value::Str(std::sync::Arc::new("a.png".to_string())),
                Value::Int(5),
            ],
            105,
        );
    }

    #[test]
    fn type_mismatch_is_call_site_error() {
        // Self-sufficient: the global registry persists across tests in
        // one process but libtest runs tests in parallel — never rely on
        // another test's registration (same for native_value_carries_arity
        // below). Re-registering is an idempotent overwrite.
        reg(
            "cabi_t_add",
            t_add as *mut c_void,
            vec![CParam::Int, CParam::Int],
            CRet::Int,
        );
        let mut args = vec![
            Value::Str(std::sync::Arc::new("x".to_string())),
            Value::Int(2),
        ];
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
        // Self-sufficient registration (see type_mismatch_is_call_site_error).
        reg(
            "cabi_t_add",
            t_add as *mut c_void,
            vec![CParam::Int, CParam::Int],
            CRet::Int,
        );
        match native_value("cabi_t_add") {
            Some(Value::Native(nf)) => assert_eq!(nf.arity, 2),
            other => panic!("expected native, got {other:?}"),
        }
    }

    #[test]
    fn handle_param_crosses_as_i64() {
        reg(
            "cabi_t_use_handle",
            t_use_handle as *mut c_void,
            vec![CParam::Handle],
            CRet::Int,
        );
        // Register the id as live first (as a plugin would when creating it).
        assert!(register_handle(42));
        expect_int("cabi_t_use_handle", vec![Value::Int(42)], 420);
        // Arity stays ZZ-level: one arg.
        assert_eq!(arity_of("cabi_t_use_handle"), Some(1));
        // Release the handle so the test doesn't pollute the global registry.
        assert!(release_handle(42));
    }

    #[test]
    fn handle_param_use_after_free_errors() {
        reg(
            "cabi_t_use_handle",
            t_use_handle as *mut c_void,
            vec![CParam::Handle],
            CRet::Int,
        );
        // Do NOT register 99 → simulating a freed/never-created handle.
        assert!(!is_handle_valid(99));
        let mut args = vec![Value::Int(99)];
        match call("cabi_t_use_handle", &mut args, span()) {
            Some(Err(e)) => assert!(
                e.message.contains("not live"),
                "expected use-after-free error, got {e:?}"
            ),
            other => panic!("expected use-after-free error, got {other:?}"),
        }
    }

    #[test]
    fn handle_void_return() {
        reg(
            "cabi_t_use_handle_void",
            t_use_handle_void as *mut c_void,
            vec![CParam::Handle],
            CRet::Void,
        );
        assert!(register_handle(7));
        let mut args = vec![Value::Int(7)];
        match call("cabi_t_use_handle_void", &mut args, span()) {
            Some(Ok(Value::Unit)) => {}
            other => panic!("expected Unit, got {other:?}"),
        }
        assert!(release_handle(7));
    }
}
