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
    fn test_register_symbol_null_terminated() {
        assert_eq!(REGISTER_SYMBOL.last(), Some(&0u8));
    }

    #[test]
    fn test_version_symbol_null_terminated() {
        assert_eq!(VERSION_SYMBOL.last(), Some(&0u8));
    }
}
