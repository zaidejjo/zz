use std::collections::HashMap;
use std::path::Path;
use zz_checker::{FuncSig, Type};
use zz_frontend::ast::{Stmt, TyKind};
use zz_frontend::parser::parse;

/// Errors that can occur when loading a plugin manifest.
#[derive(Debug, thiserror::Error)]
pub enum ManifestError {
    #[error("failed to read manifest `{path}`: {source}")]
    Io {
        path: String,
        source: std::io::Error,
    },

    #[error("manifest `{path}` has parse errors")]
    Parse { path: String, count: usize },

    #[error("manifest `{path}`: missing required metadata field `{field}`")]
    MissingField { path: String, field: String },

    #[error("manifest `{path}`: invalid metadata format for `{field}`: {detail}")]
    InvalidField {
        path: String,
        field: String,
        detail: String,
    },

    #[error("manifest `{path}`: no `extern \"C\"` block found")]
    NoExternBlock { path: String },

    #[error("manifest `{path}`: extern function `{func}` has non-C parameter type `{ty}`")]
    NonCType {
        path: String,
        func: String,
        ty: String,
    },

    #[error("manifest `{path}`: plugin extern function `{func}` returns str, which is not yet supported — return an int status code and use a getter pattern instead")]
    StrReturn { path: String, func: String },
}

/// Parsed metadata from a `.zzi` manifest header.
#[derive(Debug, Clone)]
pub struct ManifestMeta {
    /// Manifest format version (currently 1).
    pub version: u32,
    /// Exact rustc version used to build the plugin. Empty for C-only
    /// plugins (no Rust toolchain involved) — ABI refusal keys off the
    /// link-time `ZZ_*_ABI_VERSION` symbols, never this string.
    pub rustc: String,
    /// Plugin version (semver).
    pub plugin_version: String,
    /// C-plugin ABI version (`// C-ABI: 1` header). `Some` = pure-C plugin
    /// loaded via direct `dlsym` (`load_c_plugin`); `None` = legacy Rust
    /// plugin via `zz_plugin_register`.
    pub c_abi: Option<u32>,
}

/// A loaded plugin manifest with metadata and function signatures.
#[derive(Debug)]
pub struct PluginManifest {
    pub meta: ManifestMeta,
    pub funcs: HashMap<String, FuncSig>,
}

/// Load and validate a `.zzi` plugin manifest.
///
/// Parses the file using the existing ZZ parser, extracts metadata from
/// the header comment block, and converts `extern "C"` function signatures
/// into `FuncSig` entries.
pub fn load_manifest(path: &Path) -> Result<PluginManifest, ManifestError> {
    let source = std::fs::read_to_string(path).map_err(|e| ManifestError::Io {
        path: path.display().to_string(),
        source: e,
    })?;

    let path_str = path.display().to_string();

    // Parse metadata from comment header (lines starting with `//`)
    let meta = parse_metadata(&source, &path_str)?;

    // Parse the full file using the ZZ parser
    let parsed = parse(&source);
    if !parsed.errors.is_empty() {
        return Err(ManifestError::Parse {
            path: path_str,
            count: parsed.errors.len(),
        });
    }

    // Extract extern "C" function signatures
    let mut funcs = HashMap::new();
    for stmt in &parsed.program.stmts {
        if let Stmt::ExternBlock {
            items: ext_funcs, ..
        } = stmt
        {
            for ef in ext_funcs {
                let sig = extern_func_to_sig(ef, &path_str)?;
                funcs.insert(ef.name.name.clone(), sig);
            }
        }
    }

    if funcs.is_empty() {
        return Err(ManifestError::NoExternBlock { path: path_str });
    }

    Ok(PluginManifest { meta, funcs })
}

/// Parse metadata from the comment header of a `.zzi` file.
///
/// Expects lines like:
/// ```text
/// // Version: 1
/// // Rustc: 1.85.0
/// // Plugin-version: 0.1.0
/// ```
///
/// C-only plugins replace the `Rustc` line with `C-ABI`:
/// ```text
/// // Version: 1
/// // C-ABI: 1
/// // Plugin-version: 0.2.0
/// ```
fn parse_metadata(source: &str, path: &str) -> Result<ManifestMeta, ManifestError> {
    let mut version: Option<u32> = None;
    let mut rustc: Option<String> = None;
    let mut plugin_version: Option<String> = None;
    let mut c_abi: Option<u32> = None;

    for line in source.lines() {
        let trimmed = line.trim();
        if !trimmed.starts_with("//") {
            // First non-comment line: stop parsing metadata
            break;
        }
        let content = trimmed[2..].trim();
        if let Some(val) = content.strip_prefix("Version:") {
            let val = val.trim();
            version = Some(val.parse().map_err(|_| ManifestError::InvalidField {
                path: path.to_string(),
                field: "Version".to_string(),
                detail: format!("expected integer, got `{val}`"),
            })?);
        } else if let Some(val) = content.strip_prefix("Rustc:") {
            rustc = Some(val.trim().to_string());
        } else if let Some(val) = content.strip_prefix("C-ABI:") {
            let val = val.trim();
            c_abi = Some(val.parse().map_err(|_| ManifestError::InvalidField {
                path: path.to_string(),
                field: "C-ABI".to_string(),
                detail: format!("expected integer, got `{val}`"),
            })?);
        } else if let Some(val) = content.strip_prefix("Plugin-version:") {
            plugin_version = Some(val.trim().to_string());
        }
    }

    // `Rustc` is required for legacy Rust plugins; C-only plugins
    // (`C-ABI` present) carry no Rust toolchain stamp.
    let rustc = match (rustc, c_abi) {
        (Some(r), _) => r,
        (None, Some(_)) => String::new(),
        (None, None) => {
            return Err(ManifestError::MissingField {
                path: path.to_string(),
                field: "Rustc".to_string(),
            });
        }
    };
    Ok(ManifestMeta {
        version: version.ok_or(ManifestError::MissingField {
            path: path.to_string(),
            field: "Version".to_string(),
        })?,
        rustc,
        plugin_version: plugin_version.ok_or(ManifestError::MissingField {
            path: path.to_string(),
            field: "Plugin-version".to_string(),
        })?,
        c_abi,
    })
}

/// Convert an `ExternFunc` AST node into a `FuncSig`.
///
/// Validates that all parameter and return types are C-ABI-compatible.
fn extern_func_to_sig(
    ef: &zz_frontend::ast::ExternFunc,
    path: &str,
) -> Result<FuncSig, ManifestError> {
    let mut params = Vec::with_capacity(ef.params.len());
    let mut has_default = Vec::with_capacity(ef.params.len());

    for p in &ef.params {
        let ty = match &p.ty {
            Some(t) => ast_to_c_type(t),
            None => Type::Void,
        };
        if !is_c_abi_type(&ty) {
            return Err(ManifestError::NonCType {
                path: path.to_string(),
                func: ef.name.name.clone(),
                ty: format!("{ty}"),
            });
        }
        params.push((p.name.name.clone(), ty));
        has_default.push(false);
    }

    let ret = match &ef.ret {
        Some(t) => {
            let ty = ast_to_c_type(t);
            if matches!(ty, Type::Str) {
                return Err(ManifestError::StrReturn {
                    path: path.to_string(),
                    func: ef.name.name.clone(),
                });
            }
            if !is_c_abi_type(&ty) {
                return Err(ManifestError::NonCType {
                    path: path.to_string(),
                    func: ef.name.name.clone(),
                    ty: format!("{ty}"),
                });
            }
            ty
        }
        None => Type::Void,
    };

    Ok(FuncSig {
        generics: Vec::new(),
        bounds: Vec::new(),
        params,
        has_default,
        ret,
        is_extern: true,
        extern_c_symbol: ef.c_symbol.clone(),
    })
}

/// Convert an AST `Ty` to a checker `Type` for C-ABI-compatible types only.
fn ast_to_c_type(t: &zz_frontend::ast::Ty) -> Type {
    match &t.kind {
        TyKind::Int => Type::Int,
        TyKind::Float => Type::Float,
        TyKind::Bool => Type::Bool,
        TyKind::Void => Type::Void,
        TyKind::Str => Type::Str,
        TyKind::Unit => Type::Unit,
        TyKind::Ptr { mutable, inner } => Type::Ptr {
            mutable: *mutable,
            inner: Box::new(ast_to_c_type(inner)),
        },
        TyKind::Array(inner) => Type::Array(Box::new(ast_to_c_type(inner))),
        _ => Type::Void, // unknown/non-C types map to Void for C ABI check
    }
}

/// Check if a type is C-ABI-compatible (scalar, string, pointer, void, or unit).
fn is_c_abi_type(t: &Type) -> bool {
    matches!(
        t,
        Type::Int
            | Type::Float
            | Type::Bool
            | Type::Str
            | Type::Void
            | Type::Unit
            | Type::Ptr { .. }
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

    fn write_manifest(content: &str) -> NamedTempFile {
        let mut f = NamedTempFile::new().unwrap();
        f.write_all(content.as_bytes()).unwrap();
        f.flush().unwrap();
        f
    }

    #[test]
    fn test_valid_manifest() {
        let f = write_manifest(
            r#"// Version: 1
// Rustc: 1.85.0
// Plugin-version: 0.1.0

extern "C" {
    func add(a: int, b: int) -> int
    func greet(name: *const void) -> void
}
"#,
        );
        let manifest = load_manifest(f.path()).unwrap();
        assert_eq!(manifest.meta.version, 1);
        assert_eq!(manifest.meta.rustc, "1.85.0");
        assert_eq!(manifest.meta.plugin_version, "0.1.0");
        assert_eq!(manifest.funcs.len(), 2);

        let add = manifest.funcs.get("add").unwrap();
        assert_eq!(add.params.len(), 2);
        assert_eq!(add.params[0].0, "a");
        assert!(matches!(add.params[0].1, Type::Int));
        assert!(matches!(add.ret, Type::Int));
        assert!(add.is_extern);

        let greet = manifest.funcs.get("greet").unwrap();
        assert_eq!(greet.params.len(), 1);
        assert!(matches!(greet.ret, Type::Void));
    }

    #[test]
    fn test_missing_version() {
        let f = write_manifest(
            r#"// Rustc: 1.85.0
// Plugin-version: 0.1.0

extern "C" {
    func add(a: int, b: int) -> int
}
"#,
        );
        let err = load_manifest(f.path()).unwrap_err();
        assert!(err.to_string().contains("Version"));
    }

    #[test]
    fn test_missing_rustc() {
        let f = write_manifest(
            r#"// Version: 1
// Plugin-version: 0.1.0

extern "C" {
    func add(a: int, b: int) -> int
}
"#,
        );
        let err = load_manifest(f.path()).unwrap_err();
        assert!(err.to_string().contains("Rustc"));
    }

    #[test]
    fn test_missing_extern_block() {
        let f = write_manifest(
            r#"// Version: 1
// Rustc: 1.85.0
// Plugin-version: 0.1.0

func add(a: int, b: int) -> int {
    return a + b
}
"#,
        );
        let err = load_manifest(f.path()).unwrap_err();
        assert!(err.to_string().contains("no `extern"));
    }

    #[test]
    fn test_non_c_type_rejected() {
        let f = write_manifest(
            r#"// Version: 1
// Rustc: 1.85.0
// Plugin-version: 0.1.0

extern "C" {
    func bad(items: [int]) -> int
}
"#,
        );
        let err = load_manifest(f.path()).unwrap_err();
        assert!(err.to_string().contains("non-C"));
    }

    #[test]
    fn test_pointer_types_accepted() {
        let f = write_manifest(
            r#"// Version: 1
// Rustc: 1.85.0
// Plugin-version: 0.1.0

extern "C" {
    func load(path: *const void) -> *mut void
}
"#,
        );
        let manifest = load_manifest(f.path()).unwrap();
        let load = manifest.funcs.get("load").unwrap();
        assert!(matches!(load.params[0].1, Type::Ptr { mutable: false, .. }));
        assert!(matches!(load.ret, Type::Ptr { mutable: true, .. }));
    }

    #[test]
    fn test_c_abi_manifest_needs_no_rustc() {
        let f = write_manifest(
            r#"// Version: 1
// C-ABI: 1
// Plugin-version: 0.2.0

extern "C" {
    func zimg_load(path: str) -> int
}
"#,
        );
        let manifest = load_manifest(f.path()).unwrap();
        assert_eq!(manifest.meta.version, 1);
        assert_eq!(manifest.meta.c_abi, Some(1));
        assert!(manifest.meta.rustc.is_empty());
        assert!(manifest.funcs.contains_key("zimg_load"));
    }

    #[test]
    fn test_c_abi_and_rustc_coexist() {
        let f = write_manifest(
            r#"// Version: 1
// Rustc: 1.97.1
// C-ABI: 1
// Plugin-version: 0.2.0

extern "C" {
    func ping() -> int
}
"#,
        );
        let manifest = load_manifest(f.path()).unwrap();
        assert_eq!(manifest.meta.c_abi, Some(1));
        assert_eq!(manifest.meta.rustc, "1.97.1");
    }

    #[test]
    fn test_invalid_c_abi_format() {
        let f = write_manifest(
            r#"// Version: 1
// C-ABI: not-a-number
// Plugin-version: 0.2.0

extern "C" {
    func ping() -> int
}
"#,
        );
        let err = load_manifest(f.path()).unwrap_err();
        assert!(err.to_string().contains("C-ABI"));
    }

    #[test]
    fn test_legacy_manifest_has_no_c_abi() {
        let f = write_manifest(
            r#"// Version: 1
// Rustc: 1.85.0
// Plugin-version: 0.1.0

extern "C" {
    func add(a: int, b: int) -> int
}
"#,
        );
        let manifest = load_manifest(f.path()).unwrap();
        assert_eq!(manifest.meta.c_abi, None);
    }

    #[test]
    fn test_invalid_version_format() {
        let f = write_manifest(
            r#"// Version: not-a-number
// Rustc: 1.85.0
// Plugin-version: 0.1.0

extern "C" {
    func add(a: int, b: int) -> int
}
"#,
        );
        let err = load_manifest(f.path()).unwrap_err();
        assert!(err.to_string().contains("expected integer"));
    }
}
