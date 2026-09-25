//! VM-based plugin loader using dlopen.
//!
//! Loads `.so` (Linux) / `.dylib` (macOS) plugin libraries at runtime,
//! validates version stamps, and calls the well-known `zz_plugin_register`
//! entrypoint to populate the VM's native function dispatch table.

use std::collections::HashMap;
use std::path::Path;

use zz_runtime::{NativeEntry, NativeFn};

/// FFI-compatible callback for plugin registration.
///
/// The plugin's `zz_plugin_register` function calls this for each native
/// function it wants to register.
///
/// Note: `NativeFn` is a Rust `fn(...)` type passed through `extern "C"`.
/// This works in practice because `NativeFn` uses the C calling convention
/// for its actual function body. The warning is suppressed at the call site.
#[allow(improper_ctypes_definitions)]
pub type RegisterCallback = extern "C" fn(name: *const i8, arity: usize, f: NativeFn);

/// Well-known symbol name for the plugin registration entrypoint.
const REGISTER_SYMBOL: &[u8] = b"zz_plugin_register\0";

/// Well-known symbol name for the ABI version stamp.
const VERSION_SYMBOL: &[u8] = b"ZZ_PLUGIN_ABI_VERSION\0";

/// Well-known symbol name for the C-plugin ABI version stamp.
const C_VERSION_SYMBOL: &[u8] = b"ZZ_C_PLUGIN_ABI_VERSION\0";

/// Current C-plugin ABI version — bump when the marshaling contract changes.
pub const CURRENT_C_ABI_VERSION: u32 = zz_runtime::c_abi::C_ABI_VERSION;

/// Map a manifest signature to its C-ABI descriptor. Only the scalar
/// subset travels directly (`int`/`float`/`str` params, `int`/`float`/`void`
/// returns); anything else is an actionable load error, never UB.
fn c_sig_of(
    params: &[(String, zz_checker::Type)],
    ret: &zz_checker::Type,
) -> Result<zz_runtime::c_abi::CSig, String> {
    use zz_checker::Type;
    let mut out = Vec::with_capacity(params.len());
    for (name, ty) in params {
        out.push(match ty {
            Type::Int => zz_runtime::c_abi::CParam::Int,
            Type::Float => zz_runtime::c_abi::CParam::Float,
            Type::Str => zz_runtime::c_abi::CParam::Str,
            other => {
                return Err(format!(
                    "parameter `{name}` has non-C type `{other}` (C plugins take int/float/str only)"
                ));
            }
        });
    }
    let ret = match ret {
        Type::Int => zz_runtime::c_abi::CRet::Int,
        Type::Float => zz_runtime::c_abi::CRet::Float,
        Type::Void | Type::Unit => zz_runtime::c_abi::CRet::Void,
        other => {
            return Err(format!(
                "return type `{other}` is not C-callable (C plugins return int/float/void only)"
            ));
        }
    };
    Ok(zz_runtime::c_abi::CSig { params: out, ret })
}

/// Load a pure-C plugin: resolve every manifest function directly with
/// `dlsym` and register it in the runtime C-ABI registry.
///
/// The library must define `ZZ_C_PLUGIN_ABI_VERSION` (a `u32` data symbol)
/// and one C symbol per manifest function (override-aware via
/// [`zz_checker::FuncSig::c_symbol`]). Registration order is sorted by
/// ZZ-visible name so multi-function failures read deterministically.
pub fn load_c_plugin(
    path: &Path,
    funcs: &HashMap<String, zz_checker::FuncSig>,
) -> Result<PluginLib, LoadError> {
    use std::os::raw::c_void;

    let path_str = path.display().to_string();

    let lib = unsafe { libloading::Library::new(path) }.map_err(|e| LoadError::Dlopen {
        path: path_str.clone(),
        source: e,
    })?;

    // Validate C ABI version stamp.
    unsafe {
        let version_sym =
            lib.get::<*const u32>(C_VERSION_SYMBOL)
                .map_err(|_| LoadError::MissingSymbol {
                    path: path_str.clone(),
                    symbol: "ZZ_C_PLUGIN_ABI_VERSION".to_string(),
                })?;
        let got = **version_sym;
        if got != CURRENT_C_ABI_VERSION {
            return Err(LoadError::VersionMismatch {
                path: path_str,
                expected: CURRENT_C_ABI_VERSION,
                got,
            });
        }
    }

    let mut names: Vec<&String> = funcs.keys().collect();
    names.sort();
    for zz_name in names {
        let sig = &funcs[zz_name];
        let sym = sig.c_symbol(zz_name);
        // libloading takes NUL-terminated symbol bytes.
        let mut sym_nul = sym.into_bytes();
        sym_nul.push(0);
        let ptr: *mut c_void = unsafe {
            *lib.get::<*mut c_void>(&sym_nul)
                .map_err(|_| LoadError::MissingSymbol {
                    path: path_str.clone(),
                    symbol: sig.c_symbol(zz_name),
                })?
        };
        let csig = c_sig_of(&sig.params, &sig.ret).map_err(|detail| LoadError::UnsupportedSig {
            path: path_str.clone(),
            func: zz_name.clone(),
            detail,
        })?;
        zz_runtime::c_abi::register(zz_name, ptr, csig).map_err(|detail| {
            LoadError::UnsupportedSig {
                path: path_str.clone(),
                func: zz_name.clone(),
                detail,
            }
        })?;
    }

    Ok(PluginLib { _lib: lib })
}

/// Current ABI version — bump on breaking changes.
pub const CURRENT_ABI_VERSION: u32 = 1;

/// Errors that can occur when loading a plugin.
#[derive(Debug, thiserror::Error)]
pub enum LoadError {
    #[error("failed to load plugin `{path}`: {source}")]
    Dlopen {
        path: String,
        source: libloading::Error,
    },

    #[error("plugin `{path}`: missing symbol `{symbol}`")]
    MissingSymbol { path: String, symbol: String },

    #[error("plugin `{path}`: ABI version mismatch (expected {expected}, got {got})")]
    VersionMismatch {
        path: String,
        expected: u32,
        got: u32,
    },

    #[error("plugin `{path}`: C function `{func}` has unsupported signature: {detail}")]
    UnsupportedSig {
        path: String,
        func: String,
        detail: String,
    },
}

/// A loaded plugin library. Keeps the dlopen handle alive for the
/// lifetime of the process (plugins are never unloaded in v1).
pub struct PluginLib {
    #[allow(dead_code)]
    _lib: libloading::Library,
}

// Thread-local for passing the natives map through the C callback boundary.
thread_local! {
    static REGISTER_CTX: std::cell::RefCell<Option<HashMap<String, NativeEntry>>> =
        const { std::cell::RefCell::new(None) };
}

/// C-compatible callback invoked by the plugin for each native function.
#[allow(improper_ctypes_definitions)]
extern "C" fn register_callback(name: *const i8, arity: usize, f: NativeFn) {
    let name = unsafe {
        std::ffi::CStr::from_ptr(name)
            .to_str()
            .unwrap_or("invalid_utf8")
            .to_string()
    };
    REGISTER_CTX.with(|ctx| {
        if let Some(ref mut map) = *ctx.borrow_mut() {
            map.insert(name, NativeEntry { arity, f });
        }
    });
}

/// Load a plugin shared library and register its native functions.
///
/// Returns a `PluginLib` handle that must be kept alive (dropping unloads
/// the library — UB if any function pointers are still in use).
pub fn load_plugin(
    path: &Path,
    natives: &mut HashMap<String, NativeEntry>,
) -> Result<PluginLib, LoadError> {
    let path_str = path.display().to_string();

    let lib = unsafe { libloading::Library::new(path) }.map_err(|e| LoadError::Dlopen {
        path: path_str.clone(),
        source: e,
    })?;

    // Validate ABI version stamp.
    unsafe {
        let version_bytes: &[u8] = VERSION_SYMBOL;
        let version_sym =
            lib.get::<*const u32>(version_bytes)
                .map_err(|_| LoadError::MissingSymbol {
                    path: path_str.clone(),
                    symbol: "ZZ_PLUGIN_ABI_VERSION".to_string(),
                })?;
        let got = **version_sym;
        if got != CURRENT_ABI_VERSION {
            return Err(LoadError::VersionMismatch {
                path: path_str,
                expected: CURRENT_ABI_VERSION,
                got,
            });
        }
    }

    // Get the registration function.
    let register_fn: libloading::Symbol<unsafe extern "C" fn(RegisterCallback)> = unsafe {
        lib.get(REGISTER_SYMBOL)
            .map_err(|_| LoadError::MissingSymbol {
                path: path_str.clone(),
                symbol: "zz_plugin_register".to_string(),
            })?
    };

    // Set up context and call registration.
    REGISTER_CTX.with(|ctx| {
        *ctx.borrow_mut() = Some(natives.clone());
    });

    unsafe {
        register_fn(register_callback);
    }

    // Extract updated natives.
    REGISTER_CTX.with(|ctx| {
        if let Some(updated) = ctx.borrow_mut().take() {
            *natives = updated;
        }
    });

    Ok(PluginLib { _lib: lib })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_abi_version_constant() {
        assert_eq!(CURRENT_ABI_VERSION, 1);
    }

    #[test]
    fn test_c_abi_version_matches_runtime() {
        assert_eq!(CURRENT_C_ABI_VERSION, zz_runtime::c_abi::C_ABI_VERSION);
    }

    fn sig(
        params: Vec<(&str, zz_checker::Type)>,
        ret: zz_checker::Type,
    ) -> (Vec<(String, zz_checker::Type)>, zz_checker::Type) {
        (
            params
                .into_iter()
                .map(|(n, t)| (n.to_string(), t))
                .collect(),
            ret,
        )
    }

    #[test]
    fn test_c_sig_scalar_subset() {
        use zz_checker::Type;
        let (params, ret) = sig(
            vec![("h", Type::Int), ("s", Type::Float), ("p", Type::Str)],
            Type::Int,
        );
        let csig = c_sig_of(&params, &ret).unwrap();
        assert_eq!(
            csig.params,
            vec![
                zz_runtime::c_abi::CParam::Int,
                zz_runtime::c_abi::CParam::Float,
                zz_runtime::c_abi::CParam::Str,
            ]
        );
        assert_eq!(csig.ret, zz_runtime::c_abi::CRet::Int);
    }

    #[test]
    fn test_c_sig_void_and_unit_return() {
        use zz_checker::Type;
        let (params, ret) = sig(vec![], Type::Void);
        assert_eq!(
            c_sig_of(&params, &ret).unwrap().ret,
            zz_runtime::c_abi::CRet::Void
        );
        let (params, ret) = sig(vec![], Type::Unit);
        assert_eq!(
            c_sig_of(&params, &ret).unwrap().ret,
            zz_runtime::c_abi::CRet::Void
        );
    }

    #[test]
    fn test_c_sig_rejects_bool_param() {
        use zz_checker::Type;
        let (params, ret) = sig(vec![("b", Type::Bool)], Type::Int);
        let err = c_sig_of(&params, &ret).unwrap_err();
        assert!(err.contains("non-C type"), "{err}");
    }

    #[test]
    fn test_c_sig_rejects_str_return() {
        use zz_checker::Type;
        let (params, ret) = sig(vec![], Type::Str);
        let err = c_sig_of(&params, &ret).unwrap_err();
        assert!(err.contains("not C-callable"), "{err}");
    }

    #[test]
    fn test_register_symbol_null_terminated() {
        assert_eq!(REGISTER_SYMBOL.last(), Some(&0u8));
    }

    #[test]
    fn test_version_symbol_null_terminated() {
        assert_eq!(VERSION_SYMBOL.last(), Some(&0u8));
    }
}
