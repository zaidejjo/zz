//! zz.toml manifest parse/emit.
//!
//! Schema:
//! ```toml
//! [package]
//! name = "my_app"
//! version = "0.1.0"
//! authors = ["Alice"]
//! description = "Does things"
//! license = "MIT"
//! repository = "https://github.com/user/my_app"
//!
//! [dependencies]
//! foo = "^1.2.0"
//! bar = { version = "2.0", git = "https://github.com/user/repo", rev = "main" }
//! baz = { path = "../baz" }
//!
//! [collect.commands]
//! dir = "commands"
//! sig = "func(args: [str]) -> int"
//! locales-ar = { version = "1.0", optional = true }
//!
//! [features]
//! default = ["std"]
//! std = ["locales-ar"]
//! locales-ar = { modules = ["locales/ar"], deps = ["locales-ar"] }
//! ```
//!
//! The `authors`, `description`, `license`, and `repository` keys are
//! optional: manifests written before they existed still parse, and
//! `save()` omits them while empty so diffs stay minimal.
//!
//! `[features]` is optional too. A manifest without it behaves exactly
//! as before: every dependency is active and no module is gated.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};

use crate::pathsafe::escapes_root;

use serde::{Deserialize, Serialize};

use crate::hash;

/// Check a `[package] zz = "<req>"` minimum-compiler requirement against
/// the running compiler version (e.g. `(">=0.1.5", "0.1.6")`). Opt-in:
/// callers skip entirely when no requirement is set. Errors carry an
/// upgrade hint.
pub fn check_compiler_req(req: &str, compiler_version: &str) -> Result<(), String> {
    let req_parsed = semver::VersionReq::parse(req)
        .map_err(|e| format!("invalid zz version requirement `{req}`: {e}"))?;
    let ver = semver::Version::parse(compiler_version)
        .map_err(|e| format!("invalid compiler version `{compiler_version}`: {e}"))?;
    if req_parsed.matches(&ver) {
        Ok(())
    } else {
        Err(format!(
            "zz {req} is required, but this is zz {compiler_version}\n\n\
             hint: upgrade the compiler (see `zz setup`) or loosen the \
             package's `[package] zz` requirement"
        ))
    }
}

/// Top-level manifest.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Manifest {
    pub package: PackageSpec,
    #[serde(default)]
    pub dependencies: HashMap<String, DepSpec>,
    /// Native build configuration for packages that provide C/Rust extensions.
    #[serde(default)]
    pub native: Option<NativeSpec>,
    /// Directory-based typed registries (`[collect.<name>]`). Sorted map
    /// keeps manifest emission deterministic. Empty (the common case)
    /// omits the key entirely.
    #[serde(
        default,
        rename = "collect",
        skip_serializing_if = "BTreeMap::is_empty"
    )]
    pub collections: BTreeMap<String, CollectionSpec>,
    /// Feature gates (#488). Absent means today's behavior: no optional
    /// dependencies and no gated modules.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub features: BTreeMap<String, FeatureSpec>,
}

impl Default for Manifest {
    fn default() -> Self {
        Self {
            package: PackageSpec {
                name: "untitled".to_string(),
                version: "0.1.0".to_string(),
                authors: Vec::new(),
                description: None,
                license: None,
                repository: None,
                category: None,
                keywords: Vec::new(),
                zz: None,
                entry: None,
            },
            dependencies: HashMap::new(),
            native: None,
            collections: BTreeMap::new(),
            features: BTreeMap::new(),
        }
    }
}

/// A directory-based typed registry declared as `[collect.<name>]`.
///
/// Every `*.zz` file directly inside `dir` is a member. The compiler
/// synthesizes a module with `get(name)` / `all()` accessors; members must
/// export `pub <export>` (default `run`) with the signature in `sig`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CollectionSpec {
    /// Member directory, relative to the package root.
    pub dir: String,
    /// Required member signature: a function type (`func([str]) -> int`)
    /// or a function declaration shape (`func run(args: [str]) -> int`).
    pub sig: String,
    /// Member naming strategy. v1 supports `"file"` only (file stem,
    /// with `-` normalized to `_`).
    #[serde(default = "default_collection_name")]
    pub name: String,
    /// Required exported function name (default `run`).
    #[serde(default = "default_collection_export")]
    pub export: String,
}

fn default_collection_name() -> String {
    "file".to_string()
}

fn default_collection_export() -> String {
    "run".to_string()
}

/// A discovered collection member.
#[derive(Debug, Clone, PartialEq)]
pub struct CollectionMember {
    /// Identifier the member is registered under (`get("<name>")`).
    pub name: String,
    /// Path to the member source file.
    pub path: PathBuf,
    /// File stem as written on disk (for diagnostics; `foo-bar.zz`).
    pub file: String,
}

impl CollectionSpec {
    /// Validate the spec against a package root: `dir` containment/existence,
    /// supported `name` mode, identifier-shaped `export`, and a `sig` that
    /// parses through the real frontend grammar into a function type.
    pub fn validate(&self, pkg_root: &Path) -> Result<(), String> {
        if self.name != "file" {
            return Err(format!(
                "invalid [collect]: `name = \"{}\"` is not supported\n\
                 hint: v1 supports only `name = \"file\"` (member name = file stem)",
                self.name
            ));
        }
        if !is_identifier(&self.export) {
            return Err(format!(
                "invalid [collect]: `export = \"{}\"` is not a valid identifier",
                self.export
            ));
        }
        let rel = Path::new(&self.dir);
        if self.dir.is_empty() || escapes_root(rel) {
            return Err(format!(
                "invalid [collect]: `dir = \"{}\"` must be relative and stay inside the package",
                self.dir
            ));
        }
        let dir = pkg_root.join(rel);
        if !dir.is_dir() {
            return Err(format!(
                "invalid [collect]: `dir = \"{}\"` does not exist or is not a directory\n\
                 hint: create the directory or fix the path in zz.toml",
                self.dir
            ));
        }
        // Symlink escape: the directory itself must stay inside the package.
        let root_canon = pkg_root
            .canonicalize()
            .unwrap_or_else(|_| pkg_root.to_path_buf());
        if let Ok(dir_canon) = dir.canonicalize() {
            if !dir_canon.starts_with(&root_canon) {
                return Err(format!(
                    "invalid [collect]: `dir = \"{}\"` escapes the package (symlink)",
                    self.dir
                ));
            }
        }
        self.parsed_signature().map(|_| ())
    }

    /// Parse `sig` through the frontend parser and return the function type.
    ///
    /// Both spellings are accepted:
    /// - type form: `func([str]) -> int`
    /// - declaration form: `func run(args: [str]) -> int` (also the
    ///   issue's `func(args: [str]) -> int` spelling, where `args` is a
    ///   parameter name).
    pub fn parsed_signature(&self) -> Result<zz_frontend::ast::Ty, String> {
        parse_collection_signature(&self.sig)
    }

    /// Like [`Self::parsed_signature`], but also returns a canonical source
    /// spelling of the function type (`func([str]) -> int`) for the
    /// synthesized module. Declaration-form signatures are normalized so
    /// the generated AST is exactly what the checker sees.
    pub fn parsed_signature_with_source(&self) -> Result<(zz_frontend::ast::Ty, String), String> {
        parse_collection_signature_full(&self.sig)
    }

    /// Discover and validate members: `*.zz` files directly in `dir`,
    /// sorted by file stem. Empty collections are valid.
    pub fn discover(&self, pkg_root: &Path) -> Result<Vec<CollectionMember>, String> {
        let dir = pkg_root.join(&self.dir);
        let mut members = Vec::new();
        let entries = std::fs::read_dir(&dir)
            .map_err(|e| format!("cannot read collection directory `{}`: {e}", dir.display()))?;
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_file() || path.extension().and_then(|e| e.to_str()) != Some("zz") {
                continue;
            }
            let stem = path
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or_default()
                .to_string();
            let name = stem.replace('-', "_");
            if !is_identifier(&name) {
                return Err(format!(
                    "collection `{}`: file `{}` does not yield a valid identifier\n\
                     hint: rename the file to start with a letter/underscore and use only letters, digits, and underscores",
                    self.collection_label(),
                    path.file_name()
                        .and_then(|n| n.to_str())
                        .unwrap_or(&stem)
                ));
            }
            let file = path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or(&stem)
                .to_string();
            members.push(CollectionMember { name, path, file });
        }
        members.sort_by(|a, b| a.name.cmp(&b.name).then_with(|| a.path.cmp(&b.path)));
        // Normalized-name collisions (`foo-bar.zz` + `foo_bar.zz`) are loud:
        // silently dropping one member would make registration order decide.
        for pair in members.windows(2) {
            if pair[0].name == pair[1].name {
                return Err(format!(
                    "collection `{}`: files `{}` and `{}` both map to member `{}`\n\
                     hint: rename one file so each member name is unique",
                    self.collection_label(),
                    pair[0].file,
                    pair[1].file,
                    pair[0].name
                ));
            }
        }
        Ok(members)
    }

    /// Human-readable collection name used in diagnostics. Callers usually
    /// have the manifest key; `dir` is the best fallback for spec-local
    /// errors.
    fn collection_label(&self) -> &str {
        &self.dir
    }
}

/// Parse a collection signature through the real frontend parser.
///
/// Returns the parsed `Ty` (always `TyKind::Func`). This is intentionally
/// grammar-driven: no hand-rolled signature parser can drift from the
/// language.
pub fn parse_collection_signature(sig: &str) -> Result<zz_frontend::ast::Ty, String> {
    parse_collection_signature_full(sig).map(|(ty, _)| ty)
}

/// Full signature parse: the `Ty` plus its canonical source spelling.
pub fn parse_collection_signature_full(
    sig: &str,
) -> Result<(zz_frontend::ast::Ty, String), String> {
    use zz_frontend::ast::{Stmt, Ty, TyKind};
    let src = sig.trim();
    if src.is_empty() {
        return Err("invalid [collect]: `sig` must not be empty".to_string());
    }
    // Declaration forms. `func(...)` is ambiguous with the type form
    // (`func([str]) -> int`), so a failed declaration parse falls through
    // to the type parser instead of erroring immediately.
    let decl_src = if let Some(rest) = src.strip_prefix("func ") {
        Some((format!("pub func {rest} {{ }}"), true))
    } else {
        src.strip_prefix("func(")
            .map(|rest| (format!("pub func __collection_sig({rest} {{ }}"), false))
    };
    if let Some((source, strict)) = decl_src {
        let parsed = zz_frontend::parse(&source);
        if first_parse_error(&parsed.errors).is_none() {
            let func = parsed.program.stmts.iter().find_map(|s| match s {
                Stmt::Func {
                    generics,
                    params,
                    ret,
                    span,
                    ..
                } => Some((generics, params, ret, *span)),
                _ => None,
            });
            let Some((generics, params, ret, span)) = func else {
                return Err(format!(
                    "invalid [collect]: `sig = \"{sig}\"` is not a function signature"
                ));
            };
            if !generics.is_empty() {
                return Err(format!(
                    "invalid [collect]: `sig = \"{sig}\"` must not be generic\n\
                     hint: collection members are concrete functions"
                ));
            }
            let mut tys = Vec::new();
            for p in params {
                let Some(ty) = &p.ty else {
                    return Err(format!(
                        "invalid [collect]: parameter `{}` in `sig = \"{sig}\"` has no type",
                        p.name.name
                    ));
                };
                tys.push(ty.clone());
            }
            let ret_ty = ret.clone().unwrap_or(Ty {
                kind: TyKind::Unit,
                span,
            });
            let params_src: Vec<String> = tys
                .iter()
                .map(|t| source[t.span.start as usize..t.span.end as usize].to_string())
                .collect();
            let ret_src = ret
                .as_ref()
                .map(|t| source[t.span.start as usize..t.span.end as usize].to_string())
                .unwrap_or_else(|| "unit".to_string());
            let ty_source = format!("func({}) -> {ret_src}", params_src.join(", "));
            return Ok((
                Ty {
                    kind: TyKind::Func(tys, Box::new(ret_ty)),
                    span,
                },
                ty_source,
            ));
        }
        if strict {
            let err = first_parse_error(&parsed.errors).unwrap_or_default();
            return Err(format!(
                "invalid [collect]: `sig = \"{sig}\"` does not parse: {err}\n\
                 hint: use e.g. `sig = \"func(args: [str]) -> int\"` or `sig = \"func([str]) -> int\"`"
            ));
        }
    }
    // Type form.
    let source = format!("type __CollectionSig = {src}");
    let parsed = zz_frontend::parse(&source);
    if let Some(err) = first_parse_error(&parsed.errors) {
        return Err(format!(
            "invalid [collect]: `sig = \"{sig}\"` does not parse: {err}\n\
             hint: use a function type, e.g. `sig = \"func([str]) -> int\"`"
        ));
    }
    let ty = parsed.program.stmts.iter().find_map(|s| match s {
        Stmt::TypeAlias { target, .. } => Some(target.clone()),
        _ => None,
    });
    let Some(ty) = ty else {
        return Err(format!(
            "invalid [collect]: `sig = \"{sig}\"` is not a type"
        ));
    };
    if !matches!(ty.kind, TyKind::Func(..)) {
        return Err(format!(
            "invalid [collect]: `sig = \"{sig}\"` is not a function type\n\
             hint: collection members export a function, e.g. `sig = \"func([str]) -> int\"`"
        ));
    }
    let ty_source = source[ty.span.start as usize..ty.span.end as usize].to_string();
    Ok((ty, ty_source))
}

fn first_parse_error(errors: &[zz_frontend::diag::RawDiag]) -> Option<String> {
    errors
        .iter()
        .find(|e| e.severity == zz_frontend::diag::Severity::Error)
        .map(|e| e.message.clone())
}

/// True when `name` is usable as a ZZ identifier: leading letter or `_`,
/// then letters/digits/underscores.
pub fn is_identifier(name: &str) -> bool {
    let mut chars = name.chars();
    match chars.next() {
        Some(c) if c.is_ascii_alphabetic() || c == '_' => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// One `[features]` entry. Two shapes are accepted:
///
/// ```toml
/// default = ["std", "locales-ar"]              # bare list
/// locales-ar = { modules = ["locales/ar"], deps = ["faker_locales_ar"] }
/// ```
///
/// The list form enables other features (transitively) and may also name
/// an optional dependency directly. The table form gates module files
/// (`modules`) and pulls in optional dependencies (`deps`).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(untagged)]
pub enum FeatureSpec {
    /// `name = ["other_feature", ...]`
    List(Vec<String>),
    /// `name = { modules = [...], deps = [...] }`
    Table(FeatureTable),
}

/// Table form of a feature gate.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct FeatureTable {
    /// Module paths (relative to the package source root, no `.zz`, no
    /// `..`) gated by this feature. e.g. `["locales/ar"]`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub modules: Vec<String>,
    /// Optional dependency names enabled by this feature. Every entry
    /// must name a declared `optional = true` dependency.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub deps: Vec<String>,
}

impl FeatureSpec {
    /// Feature names this entry enables transitively (list form only).
    pub fn feature_refs(&self) -> &[String] {
        match self {
            FeatureSpec::List(items) => items,
            FeatureSpec::Table(_) => &[],
        }
    }

    /// Optional dependency names enabled by this entry.
    pub fn dep_refs(&self) -> &[String] {
        match self {
            FeatureSpec::List(_) => &[],
            FeatureSpec::Table(table) => &table.deps,
        }
    }

    /// Gated module paths (table form only).
    pub fn modules(&self) -> &[String] {
        match self {
            FeatureSpec::List(_) => &[],
            FeatureSpec::Table(table) => &table.modules,
        }
    }
}

/// Feature selection from the command line: `--features a,b` plus
/// `--all-features`. The implicit `default` set is always included.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FeatureSelection {
    /// Explicitly requested names (`--features`, repeatable).
    pub requested: BTreeSet<String>,
    /// `--all-features`: enable every declared feature.
    pub all: bool,
}

impl FeatureSelection {
    /// Build a selection from explicit names.
    pub fn new<I, S>(requested: I, all: bool) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        Self {
            requested: requested.into_iter().map(Into::into).collect(),
            all,
        }
    }

    /// No explicit flags: only the manifest's `default` set.
    pub fn default_set() -> Self {
        Self::default()
    }

    /// Explicit names as a sorted slice-like iterator.
    pub fn names(&self) -> Vec<String> {
        self.requested.iter().cloned().collect()
    }

    /// True when no feature flags were given (default selection).
    pub fn is_default(&self) -> bool {
        !self.all && self.requested.is_empty()
    }

    /// Stable fingerprint of the explicit selection. Empty for the default
    /// selection so lockfiles, run caches, and build caches keep their
    /// existing keys when no feature flags are passed.
    pub fn fingerprint(&self) -> String {
        if self.is_default() {
            return String::new();
        }
        let mut salt = String::new();
        if self.all {
            salt.push_str("all;");
        }
        salt.push_str(&self.names().join(","));
        crate::hash::hash_bytes(salt.as_bytes())
    }
}

/// Parse `--features` values: repeatable flags plus comma-separated names.
/// `--all-features` is handled by the caller (it is a boolean flag).
pub fn parse_feature_list(values: &[String]) -> BTreeSet<String> {
    values
        .iter()
        .flat_map(|v| v.split(','))
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(str::to_string)
        .collect()
}

/// Normalize a module path for `[features] modules` comparisons:
/// backslashes become `/`, a leading `./` and a trailing `.zz` are
/// stripped. Callers compare import paths through this.
pub fn normalize_module_path(path: &str) -> String {
    let mut p = path.replace('\\', "/");
    while let Some(rest) = p.strip_prefix("./") {
        p = rest.to_string();
    }
    if let Some(rest) = p.strip_suffix(".zz") {
        p = rest.to_string();
    }
    p
}

/// Feature names are identifiers/kebab strings so they survive import
/// diagnostics and CLI flag parsing without quoting.
fn is_valid_feature_name(name: &str) -> bool {
    let mut chars = name.chars();
    match chars.next() {
        Some(c) if c.is_ascii_alphabetic() => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// Module paths in `[features]` are relative to the package source root
/// and may not escape it.
fn validate_feature_module(feature: &str, module: &str) -> Result<(), String> {
    let bad = |why: &str| {
        format!(
            "invalid module `{module}` in feature `{feature}`: {why}\n\
             hint: use a relative path like \"locales/ar\" (no `..`, no leading `/`)"
        )
    };
    if module.trim().is_empty() {
        return Err(bad("must not be empty"));
    }
    let p = Path::new(module);
    if escapes_root(p) {
        return Err(bad("must be relative and stay inside the package"));
    }
    Ok(())
}

/// Package metadata.
///
/// `authors`, `description`, `license`, and `repository` are optional so
/// pre-enrichment manifests keep parsing; `save()` skips them while empty.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PackageSpec {
    pub name: String,
    pub version: String,
    /// e.g. `authors = ["Alice <alice@example.com>"]`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub authors: Vec<String>,
    /// Short human-readable summary (sent as `description` on publish).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// SPDX identifier (e.g. `"MIT"`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub license: Option<String>,
    /// Source URL (sent as `repo` on publish).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repository: Option<String>,
    /// Fixed-vocabulary category (sent as `category` on publish).
    /// Canonical values: backend, cli, frameworks, math, gui, utilities.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub category: Option<String>,
    /// Free-form discovery keywords (sent as `keywords` on publish).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub keywords: Vec<String>,
    /// Minimum compiler version, as a semver requirement
    /// (e.g. `zz = ">=0.1.5"`). Opt-in; absent means any compiler.
    /// NOTE: parsers without this field (<= 0.1.5) *ignore* unknown keys,
    /// so an old compiler never sees the requirement — enforcement only
    /// protects compilers new enough to know the field. The field still
    /// documents intent for humans and registries.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub zz: Option<String>,
    /// Entry-point override, relative to the package root
    /// (e.g. `entry = "src/cli.zz"`). Opt-in; absent means conventional
    /// discovery (`src/main.zz`, then `main.zz`). Same unknown-key note
    /// as `zz` above: old compilers ignore it and build the
    /// conventional entry instead.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entry: Option<String>,
}

/// Options for scaffolding a new manifest (`zz init` / `zz new` flags).
#[derive(Debug, Clone, Default)]
pub struct InitOptions {
    /// `--author` (repeatable, comma-separated values also split).
    pub authors: Vec<String>,
    /// `--description`.
    pub description: Option<String>,
    /// `--license` (e.g. `MIT`).
    pub license: Option<String>,
    /// `--repo`.
    pub repository: Option<String>,
}

/// Backend selector for native plugin builds.
///
/// - `Hook`: legacy `build = "build.sh"` script (deprecated, warns, gated
///   behind `--allow-hooks` for direct deps / error for transitive deps).
/// - `Cc`: declarative C build performed by `zz` itself
///   (`[native.build-cc]`), no shell, no script, no network.
/// - `Rust`: hermetic `cargo build --offline --locked` from a vendored
///   closure (`[native.build-rust]`), no package scripts.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum NativeBackend {
    /// Declarative C build (default when `[native.build-cc]` is present).
    Cc,
    /// Hermetic Rust crate build (default when `[native.build-rust]` is present).
    Rust,
    /// Legacy shell hook (default when `build = "..."` is present).
    #[default]
    Hook,
}

/// Default rustc compatibility range for `[native.build-rust]`.
///
/// A range (not an exact pin) is sound for the C-ABI plugin contract:
/// the link-time `ZZ_*_ABI_VERSION` stamp — not the rustc patch level —
/// is what refuses incompatible plugins. Authors with a genuine ABI
/// requirement can narrow it explicitly.
pub const DEFAULT_RUSTC_RANGE: &str = ">=1.75, <2";

/// Cargo profile accepted by `[native.build-rust]`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RustProfile {
    Release,
    Debug,
}

impl RustProfile {
    /// Cargo `--profile` directory name.
    pub fn dir_name(self) -> &'static str {
        match self {
            RustProfile::Release => "release",
            RustProfile::Debug => "debug",
        }
    }

    /// Cargo profile flag (`--release` for release, `--profile dev` for debug).
    pub fn cargo_args(self) -> &'static [&'static str] {
        match self {
            RustProfile::Release => &["--release"],
            RustProfile::Debug => &["--profile", "dev"],
        }
    }
}

/// Native build configuration for packages that provide C extensions.
///
/// Two shapes (explicit `backend` tag preferred, inferred when absent):
///
/// ```toml
/// # Legacy (deprecated):
/// [native]
/// build = "build.sh"
///
/// # Declarative v1:
/// [native]
/// backend = "cc"
/// manifest = "plugin.zzi"
/// [native.build-cc]
/// sources = ["csrc/wrapper.c"]
/// include_dirs = ["csrc"]
/// defines = ["NDEBUG"]
/// cflags = ["-O2", "-Wall", "-Wextra", "-fPIC"]
/// libs = []
/// pkg_config = ["vips"]
/// targets = ["linux-x86_64-gnu-glibc2.28"]
/// [native.prebuilt.target."linux-x86_64-gnu-glibc2.28"]
/// url = "https://example.com/zimg-0.3.0-linux-x86_64-gnu-glibc2.28.tgz"
/// sha256 = "9f2c…"
/// ```
///
/// Prebuilt table keys are platform tags (see [`validate_platform_tag`]).
/// Tags contain dots (`glibc2.28`), so they MUST be quoted in TOML.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct NativeSpec {
    /// Backend selector. Inferred when absent: `cc` if `[native.build-cc]`
    /// is present, else `hook`. When set explicitly it must agree with the
    /// shape present (mismatch = error).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub backend: Option<NativeBackend>,
    /// Interface file, relative to package root. Default `plugin.zzi`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub manifest: Option<String>,
    /// Legacy hook script, relative to package root (`build = "build.sh"`).
    /// Mutually exclusive with `build_cc`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub build: Option<String>,
    /// Declarative C build spec (`[native.build-cc]`).
    #[serde(default, rename = "build-cc", skip_serializing_if = "Option::is_none")]
    pub build_cc: Option<BuildCcSpec>,
    /// Hermetic Rust crate build spec (`[native.build-rust]`).
    #[serde(
        default,
        rename = "build-rust",
        skip_serializing_if = "Option::is_none"
    )]
    pub build_rust: Option<BuildRustSpec>,
    /// Per-platform prebuilt artifacts (`[native.prebuilt]`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prebuilt: Option<PrebuiltSpec>,
    /// Legacy system-dep query (string form, e.g. `pkg_config = "vips"`).
    /// The declarative form uses `BuildCcSpec::pkg_config` (list) instead;
    /// setting both is an error.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pkg_config: Option<String>,
}

/// Declarative C build spec: everything `zz` needs to compile the plugin
/// without running any package-supplied code.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct BuildCcSpec {
    /// C sources, relative to package root, explicit list (no globs v1).
    /// e.g. `["csrc/zimg_wrapper.c"]`.
    pub sources: Vec<String>,
    /// `-I` dirs, relative to package root. Only way to add includes.
    #[serde(default)]
    pub include_dirs: Vec<String>,
    /// `-D` defines without the `-D` prefix. e.g. `["NDEBUG"]`,
    /// `["_POSIX_C_SOURCE=200809L"]`.
    #[serde(default)]
    pub defines: Vec<String>,
    /// Extra C flags from the explicit allowlist (see [`validate_cflag`]).
    #[serde(default)]
    pub cflags: Vec<String>,
    /// Direct `-l` libs (bare names, no `-l` prefix). e.g. `["vips"]`.
    /// Prefer `pkg_config` below.
    #[serde(default)]
    pub libs: Vec<String>,
    /// pkg-config modules. e.g. `["vips"]`. Resolved by `zz`, recorded
    /// in `zz.lock` (drift busts the cache).
    #[serde(default)]
    pub pkg_config: Vec<String>,
    /// Supported platform tags (see [`validate_platform_tag`]). A missing
    /// entry means "no prebuilt, source fallback".
    #[serde(default)]
    pub targets: Vec<String>,
}

/// Declarative Rust crate build spec (`[native.build-rust]`).
///
/// `zz` vendors the locked crate closure into CAS, then drives
/// `cargo build --offline --locked` itself under a hermetic environment.
/// The consumed package has no `build.sh` and no `build.rs`; cargo is a
/// build tool `zz` drives, never a script the package runs.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct BuildRustSpec {
    /// Crate directory, relative to the package root (e.g. `"native/"`).
    /// Must contain `Cargo.toml` and a committed `Cargo.lock`.
    #[serde(rename = "crate")]
    pub crate_path: String,
    /// Cargo features to enable (`--features a,b`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub cargo_features: Vec<String>,
    /// Explicit Rust target triple override. Absent derives the triple
    /// from the platform tag (`native_platform_tag()` mapping).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,
    /// rustc compatibility range (semver requirement). Default
    /// [`DEFAULT_RUSTC_RANGE`] — never an exact pin unless the author
    /// deliberately narrows it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rustc_range: Option<String>,
    /// Cargo profile: `release` (default) or `debug`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile: Option<String>,
}

impl BuildRustSpec {
    /// Effective rustc requirement (default compatibility range).
    pub fn rustc_req(&self) -> &str {
        self.rustc_range.as_deref().unwrap_or(DEFAULT_RUSTC_RANGE)
    }

    /// Effective cargo profile (`release` default).
    pub fn rust_profile(&self) -> Result<RustProfile, String> {
        match self.profile.as_deref().unwrap_or("release") {
            "release" => Ok(RustProfile::Release),
            "debug" | "dev" => Ok(RustProfile::Debug),
            other => Err(format!(
                "invalid [native.build-rust] profile `{other}`\n\
                 hint: use \"release\" (default) or \"debug\""
            )),
        }
    }
}

/// Per-platform prebuilt artifacts: `target.<tag> = { url, sha256 }`.
/// v1: `https:` URLs only (GitHub releases convention); the `registry:`
/// scheme is deferred to v2.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct PrebuiltSpec {
    /// Map from platform tag → artifact.
    #[serde(default)]
    pub target: std::collections::HashMap<String, PrebuiltArtifact>,
}

/// One per-platform prebuilt tarball (full `build/` layout: `*.o`, `*.a`,
/// shared lib, `ldflags.txt`/`cflags.txt` — never partial).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PrebuiltArtifact {
    /// `https:` URL (GitHub releases convention for v1).
    pub url: String,
    /// Expected sha256 of the tarball bytes (fail closed on mismatch).
    pub sha256: String,
}

impl NativeSpec {
    /// Manifest-interface path (default `plugin.zzi`).
    pub fn manifest_path(&self) -> &str {
        self.manifest.as_deref().unwrap_or("plugin.zzi")
    }

    /// Resolved backend (explicit tag wins; else inferred from shape).
    pub fn resolved_backend(&self) -> NativeBackend {
        if let Some(b) = self.backend {
            return b;
        }
        if self.build_rust.is_some() {
            return NativeBackend::Rust;
        }
        if self.build_cc.is_some() {
            return NativeBackend::Cc;
        }
        NativeBackend::Hook
    }

    /// Validate all declarative fields against `pkg_root`.
    ///
    /// Checks: mixed-form rejection, backend/shape agreement, path
    /// containment (hook, manifest, crate, sources, include_dirs),
    /// explicit cflag allowlist + rejected classes, `libs` shape, platform
    /// tags, prebuilt URL/sha shape, legacy/new `pkg_config` exclusivity,
    /// and for `[native.build-rust]`: committed Cargo.lock, both
    /// `crate-type`s, no package `build.rs`, pinned lockfile closure.
    /// Returns `Ok(())` for a legacy hook-only spec (nothing to check
    /// beyond containment of the hook path).
    pub fn validate(&self, pkg_root: &Path) -> Result<(), String> {
        // Mixed form is always an error.
        let declarative = [
            self.build.is_some(),
            self.build_cc.is_some(),
            self.build_rust.is_some(),
        ]
        .iter()
        .filter(|present| **present)
        .count();
        if declarative > 1 {
            return Err(
                "invalid [native]: `build` (hook), `[native.build-cc]`, and `[native.build-rust]` are mutually exclusive\n\
                 hint: keep exactly one backend; remove `build = \"build.sh\"` when using a declarative table"
                    .to_string(),
            );
        }
        let legacy_pkg_config_conflicts = self.pkg_config.is_some()
            && (self.build_rust.is_some()
                || self
                    .build_cc
                    .as_ref()
                    .is_some_and(|b| !b.pkg_config.is_empty()));
        if legacy_pkg_config_conflicts {
            return Err("invalid [native]: legacy `pkg_config` (string) is mutually exclusive with declarative backends\n\
                 hint: remove the legacy `pkg_config` string; use `[native.build-cc] pkg_config` (list) or `[native.build-rust]`"
                .to_string());
        }
        // Explicit backend must agree with the shape present.
        if let Some(b) = self.backend {
            match b {
                NativeBackend::Cc if self.build_cc.is_none() => {
                    return Err(
                        "invalid [native]: `backend = \"cc\"` requires `[native.build-cc]`\n\
                         hint: add a `[native.build-cc]` table or drop the backend tag"
                            .to_string(),
                    );
                }
                NativeBackend::Rust if self.build_rust.is_none() => {
                    return Err(
                        "invalid [native]: `backend = \"rust\"` requires `[native.build-rust]`\n\
                         hint: add a `[native.build-rust]` table (crate = \"native/\") or drop the backend tag"
                            .to_string(),
                    );
                }
                NativeBackend::Hook if self.build.is_none() => {
                    return Err(
                        "invalid [native]: `backend = \"hook\"` requires `build = \"...\"`\n\
                         hint: add `build = \"build.sh\"` or switch to `backend = \"cc\"`/`\"rust\"`"
                            .to_string(),
                    );
                }
                _ => {}
            }
        }
        // Containment for hook + manifest paths.
        if let Some(hook) = &self.build {
            check_contained(pkg_root, hook, "build")?;
        }
        if let Some(m) = &self.manifest {
            check_contained(pkg_root, m, "manifest")?;
        }
        if let Some(rust) = &self.build_rust {
            return validate_rust_spec(pkg_root, rust);
        }
        let Some(cc) = &self.build_cc else {
            return Ok(());
        };
        if cc.sources.is_empty() {
            return Err(
                "invalid [native.build-cc]: `sources` must list at least one file\n\
                 hint: e.g. sources = [\"csrc/wrapper.c\"]"
                    .to_string(),
            );
        }
        for s in &cc.sources {
            check_contained(pkg_root, s, "sources")?;
            if !s.ends_with(".c") {
                return Err(format!(
                    "invalid [native.build-cc]: source `{s}` must be a `.c` file\n\
                     hint: declarative v1 covers C sources only"
                ));
            }
        }
        for d in &cc.include_dirs {
            check_contained(pkg_root, d, "include_dirs")?;
        }
        for def in &cc.defines {
            validate_define(def)?;
        }
        for f in &cc.cflags {
            validate_cflag(f)?;
        }
        for lib in &cc.libs {
            if lib.is_empty() || lib.starts_with('-') || lib.contains(['/', '\\', ' ', ',', '@']) {
                return Err(format!(
                    "invalid [native.build-cc]: lib `{lib}` must be a bare name (no `-l`, no path)\n\
                     hint: e.g. libs = [\"vips\"]"
                ));
            }
        }
        for pc in &cc.pkg_config {
            if pc.is_empty() || pc.contains([' ', ',', '@', '/']) {
                return Err(format!(
                    "invalid [native.build-cc]: pkg_config entry `{pc}` must be a bare module name\n\
                     hint: e.g. pkg_config = [\"vips\"]"
                ));
            }
        }
        for t in &cc.targets {
            validate_platform_tag(t)?;
        }
        if let Some(pre) = &self.prebuilt {
            for (tag, art) in &pre.target {
                validate_platform_tag(tag)?;
                if !(art.url.starts_with("https://") || art.url.starts_with("http://")) {
                    return Err(format!(
                        "invalid [native.prebuilt]: url for `{tag}` must be https: (registry: is deferred to v2)\n\
                         hint: host the tarball on GitHub releases"
                    ));
                }
                if art.sha256.len() != 64 || !art.sha256.chars().all(|c| c.is_ascii_hexdigit()) {
                    return Err(format!(
                        "invalid [native.prebuilt]: sha256 for `{tag}` must be 64 hex chars"
                    ));
                }
            }
        }
        Ok(())
    }
}

/// Validate a `[native.build-rust]` spec against the package root.
///
/// Structural checks happen here (paths, ranges, features, target triple,
/// crate types, committed lock, banned scripts, pinned closure). Network
/// and cargo probing stay in the build/install path.
fn validate_rust_spec(pkg_root: &Path, spec: &BuildRustSpec) -> Result<(), String> {
    check_contained(pkg_root, &spec.crate_path, "build-rust.crate")?;
    let crate_dir = crate::rust_lock::crate_dir_for(pkg_root, &spec.crate_path)?;
    if !crate_dir.is_dir() {
        return Err(format!(
            "invalid [native.build-rust]: crate `{}` is not a directory\n\
             hint: crate = \"native/\" relative to the package root",
            spec.crate_path
        ));
    }

    // Profile + rustc range are manifest-level contracts.
    spec.rust_profile()?;
    semver::VersionReq::parse(spec.rustc_req()).map_err(|e| {
        format!(
            "invalid [native.build-rust] rustc_range `{}`: {e}\n\
             hint: use a semver range, e.g. \">=1.75, <2\"",
            spec.rustc_req()
        )
    })?;
    for feat in &spec.cargo_features {
        if feat.is_empty()
            || feat.starts_with('-')
            || feat.contains(|c: char| c.is_whitespace() || c == ',')
        {
            return Err(format!(
                "invalid [native.build-rust] cargo_features entry `{feat}`\n\
                 hint: bare feature names only (no commas/spaces/leading dashes)"
            ));
        }
    }
    if let Some(target) = &spec.target {
        if target.is_empty()
            || !target
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
        {
            return Err(format!(
                "invalid [native.build-rust] target `{target}`: must be a Rust target triple\n\
                 hint: e.g. x86_64-unknown-linux-gnu"
            ));
        }
    }

    let manifest = crate::rust_lock::validate_crate(&crate_dir)?;
    crate::rust_lock::read_closure(&crate_dir, &manifest)?;
    Ok(())
}

/// Reject paths escaping the package root (`..`, absolute, empty).
fn check_contained(pkg_root: &Path, rel: &str, field: &str) -> Result<(), String> {
    if rel.is_empty() {
        return Err(format!("invalid [native]: `{field}` must not be empty"));
    }
    let p = Path::new(rel);
    if escapes_root(p) {
        return Err(format!(
            "invalid [native]: `{field}` entry `{rel}` must be relative and stay inside the package\n\
             hint: use a path like \"csrc/wrapper.c\""
        ));
    }
    // Symlink escape is checked at build time (canonicalize + strip_prefix).
    let _ = pkg_root;
    Ok(())
}

/// Validate one `-D` define body (no `-D` prefix).
fn validate_define(def: &str) -> Result<(), String> {
    if def.is_empty() || def.starts_with('-') || def.contains([' ', ',', '@', '"', '\'', '(', ')'])
    {
        return Err(format!(
            "invalid [native.build-cc]: define `{def}` must look like NAME or NAME=value (no spaces, commas, quotes, parens)\n\
             hint: e.g. defines = [\"NDEBUG\"]"
        ));
    }
    Ok(())
}

/// Explicit allowlist for `[native.build-cc] cflags`.
///
/// Allowed: `-O0 -O1 -O2 -O3 -Os -Oz`, `-g`, `-fPIC -fpic -fPIE`,
/// `-Wall -Wextra -Werror -Wno-<name>`, `-std=c11 -std=c17`,
/// `-pthread`, `-march=x86-64 -march=armv8-a`.
///
/// Rejected classes (each fail closed):
/// ',' anywhere, leading '@', `-Wl,*`, `-Wp,*`, `-Xlinker`,
/// `-Xpreprocessor`, bare `-I` (use `include_dirs`), bare `-L`/`-l`
/// (use `libs`), `--target=` (toolchain sets it). Same validator gates
/// `pkg-config` output tokens (plus `-I/-L/-l` allowed from that source —
/// see [`validate_pkg_config_token`]).
pub fn validate_cflag(flag: &str) -> Result<(), String> {
    // Rejected classes first (fail closed before consulting allowlist).
    if flag.contains(',') {
        return Err(format!(
            "invalid cflag `{flag}`: commas are forbidden (blocks -Wl,/-Wp, smuggling)"
        ));
    }
    if flag.starts_with('@') {
        return Err(format!(
            "invalid cflag `{flag}`: @file indirection is forbidden"
        ));
    }
    if flag.starts_with("-Wl,")
        || flag.starts_with("-Wp,")
        || flag == "-Xlinker"
        || flag == "-Xpreprocessor"
        || flag.starts_with("--target=")
    {
        return Err(format!(
            "invalid cflag `{flag}`: linker/compiler-plugin escapes are forbidden"
        ));
    }
    if flag.starts_with("-I") {
        return Err(format!(
            "invalid cflag `{flag}`: bare -I is forbidden; use include_dirs instead"
        ));
    }
    if flag.starts_with("-L") || flag.starts_with("-l") {
        return Err(format!(
            "invalid cflag `{flag}`: bare -L/-l is forbidden; use libs instead"
        ));
    }
    const ALLOWED: &[&str] = &[
        "-O0",
        "-O1",
        "-O2",
        "-O3",
        "-Os",
        "-Oz",
        "-g",
        "-fPIC",
        "-fpic",
        "-fPIE",
        "-Wall",
        "-Wextra",
        "-Werror",
        "-pthread",
        "-std=c11",
        "-std=c17",
        "-march=x86-64",
        "-march=armv8-a",
    ];
    if ALLOWED.contains(&flag) || flag.starts_with("-Wno-") {
        return Ok(());
    }
    Err(format!(
        "invalid cflag `{flag}`: not on the explicit allowlist\n\
         hint: allowed: -O0..-Oz, -g, -fPIC/-fpic/-fPIE, -Wall/-Wextra/-Werror/-Wno-*, -std=c11/c17, -pthread, -march=x86-64/armv8-a"
    ))
}

/// Validate one whitespace-split token from `pkg-config --cflags/--libs`.
///
/// Same rejected classes as [`validate_cflag`], plus the pkg-config-only
/// allowances `-I<dir>`, `-L<dir>`, `-l<name>` (system paths are legitimate
/// here — the manifest form still bans them so only the resolver can add
/// them). `-D<...>` tokens must pass [`validate_define`] on their body.
pub fn validate_pkg_config_token(tok: &str) -> Result<(), String> {
    if tok.contains(',') {
        return Err(format!(
            "pkg-config emitted forbidden token `{tok}`: commas are forbidden"
        ));
    }
    if tok.starts_with('@') {
        return Err(format!(
            "pkg-config emitted forbidden token `{tok}`: @file indirection is forbidden"
        ));
    }
    if tok.starts_with("-Wl,")
        || tok.starts_with("-Wp,")
        || tok == "-Xlinker"
        || tok == "-Xpreprocessor"
    {
        return Err(format!(
            "pkg-config emitted forbidden token `{tok}`: linker/compiler-plugin escapes are forbidden"
        ));
    }
    if let Some(dir) = tok.strip_prefix("-I").or(tok.strip_prefix("-L")) {
        if dir.is_empty() || dir.contains('@') {
            return Err(format!(
                "pkg-config emitted forbidden token `{tok}`: empty or @-path"
            ));
        }
        return Ok(());
    }
    if let Some(name) = tok.strip_prefix("-l") {
        if name.is_empty() || name.contains(['/', ' ']) {
            return Err(format!("pkg-config emitted forbidden token `{tok}`"));
        }
        return Ok(());
    }
    if let Some(body) = tok.strip_prefix("-D") {
        return validate_define(body)
            .map_err(|_| format!("pkg-config emitted forbidden token `{tok}`: bad -D shape"));
    }
    // Anything else must be on the plain cflag allowlist.
    validate_cflag(tok).map_err(|_| format!("pkg-config emitted forbidden token `{tok}`"))
}

/// Validate a platform tag: `<os>-<arch>-<abi>-<floor>`.
///
/// Accepted: `linux-{x86_64,aarch64}-gnu-glibc<major>.<minor>`
/// (e.g. `linux-x86_64-gnu-glibc2.28`), `linux-{x86_64,aarch64}-musl`,
/// `macos-{x86_64,arm64}-min<major>.<minor>` (e.g. `macos-arm64-min11.0`),
/// `windows-{x86_64,aarch64}-msvc`. Anything else is a manifest error.
pub fn validate_platform_tag(tag: &str) -> Result<(), String> {
    fn bad(tag: &str) -> String {
        format!(
            "invalid platform tag `{tag}`\n\
             hint: e.g. linux-x86_64-gnu-glibc2.28, linux-x86_64-musl, macos-arm64-min11.0, windows-x86_64-msvc, windows-aarch64-msvc"
        )
    }
    let parts: Vec<&str> = tag.split('-').collect();
    match parts.as_slice() {
        ["linux", arch @ ("x86_64" | "aarch64"), "gnu", floor] => {
            parse_glibc_floor(floor).ok_or_else(|| bad(tag))?;
            let _ = arch;
            Ok(())
        }
        ["linux", "x86_64" | "aarch64", "musl"] => Ok(()),
        ["macos", "x86_64" | "arm64", floor] if floor.starts_with("min") => {
            parse_dotted(&floor[3..]).ok_or_else(|| bad(tag))?;
            Ok(())
        }
        // Both Windows architectures are first-class: `host_tag()` emits
        // `windows-aarch64-msvc` on ARM64 and the release pipeline builds
        // it, so rejecting the tag made native packages unpublishable
        // there.
        ["windows", "x86_64" | "aarch64", "msvc"] => Ok(()),
        _ => Err(bad(tag)),
    }
}

/// Parse `glibc<major>.<minor>` → `(major, minor)`.
fn parse_glibc_floor(s: &str) -> Option<(u32, u32)> {
    let rest = s.strip_prefix("glibc")?;
    parse_dotted(rest)
}

/// Parse `<major>.<minor>` → `(major, minor)`.
fn parse_dotted(s: &str) -> Option<(u32, u32)> {
    let (a, b) = s.split_once('.')?;
    Some((a.parse().ok()?, b.parse().ok()?))
}

/// Dependency specification — three variants.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(untagged)]
pub enum DepSpec {
    /// Registry version range: `"^1.2.0"`, or the table form
    /// `{ version = "^1.2.0", optional = true, features = ["x"] }`
    /// (#419: `[dependencies.x]` tables and Cargo-style inline tables
    /// spell the same registry dep).
    Version(VersionDep),
    /// Git dependency with version constraint.
    Git(GitDep),
    /// Path dependency (local filesystem).
    // Cross-source pins (same name locked from another source, e.g. a
    // registry entry left over from before the manifest flipped to a path
    // dep) are stale by definition: the manifest declaration always wins
    // (see resolve.rs notes, #266). True same-name-different-source
    // declarations across workspace members stay future work until
    // [workspace] ships.
    Path(PathDep),
}

impl DepSpec {
    /// `optional = true` on any variant. Optional deps are active only
    /// when an enabled feature names them in `deps` (or, for the list
    /// form, directly).
    pub fn optional(&self) -> bool {
        match self {
            DepSpec::Version(v) => v.optional,
            DepSpec::Git(g) => g.optional,
            DepSpec::Path(p) => p.optional,
        }
    }

    /// Features requested from this dependency's own `[features]` section.
    pub fn features(&self) -> &[String] {
        match self {
            DepSpec::Version(v) => &v.features,
            DepSpec::Git(g) => &g.features,
            DepSpec::Path(p) => &p.features,
        }
    }

    /// Version requirement for registry/git deps.
    pub fn version_req(&self) -> Option<&str> {
        match self {
            DepSpec::Version(v) => Some(&v.version),
            DepSpec::Git(g) => Some(&g.version),
            DepSpec::Path(_) => None,
        }
    }
}

/// Registry dependency: bare version string, or a table adding optional
/// dependency flags. Serialization stays canonical — a dep with no flags
/// emits a plain string, so old manifests round-trip byte-for-byte.
#[derive(Debug, Clone, PartialEq)]
pub struct VersionDep {
    pub version: String,
    pub optional: bool,
    pub features: Vec<String>,
}

impl VersionDep {
    /// Plain (non-optional, no requested features) version dep.
    pub fn new(version: impl Into<String>) -> Self {
        Self {
            version: version.into(),
            optional: false,
            features: Vec::new(),
        }
    }
}

impl From<&str> for VersionDep {
    fn from(v: &str) -> Self {
        Self::new(v)
    }
}

impl From<String> for VersionDep {
    fn from(v: String) -> Self {
        Self::new(v)
    }
}

impl std::fmt::Display for VersionDep {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.version)
    }
}

impl std::ops::Deref for VersionDep {
    type Target = str;
    fn deref(&self) -> &str {
        &self.version
    }
}

fn bool_is_false(v: &bool) -> bool {
    !*v
}

impl Serialize for VersionDep {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        if !self.optional && self.features.is_empty() {
            return serializer.serialize_str(&self.version);
        }
        #[derive(Serialize)]
        struct Table<'a> {
            version: &'a str,
            #[serde(skip_serializing_if = "bool_is_false")]
            optional: bool,
            #[serde(skip_serializing_if = "Vec::is_empty")]
            features: &'a Vec<String>,
        }
        Table {
            version: &self.version,
            optional: self.optional,
            features: &self.features,
        }
        .serialize(serializer)
    }
}

/// Table form of a registry dep with unknown keys denied: extras (a `git`
/// without `rev`, a typo) must stay loud errors rather than silently
/// degrading to a registry dep.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct VersionTable {
    version: String,
    #[serde(default)]
    optional: bool,
    #[serde(default)]
    features: Vec<String>,
}

impl<'de> Deserialize<'de> for VersionDep {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        struct VersionOrTable;
        impl<'de> serde::de::Visitor<'de> for VersionOrTable {
            type Value = VersionDep;

            fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("a version string or a table with `version`, `optional`, `features`")
            }

            fn visit_str<E>(self, v: &str) -> Result<VersionDep, E>
            where
                E: serde::de::Error,
            {
                Ok(VersionDep::new(v))
            }

            fn visit_string<E>(self, v: String) -> Result<VersionDep, E>
            where
                E: serde::de::Error,
            {
                Ok(VersionDep::new(v))
            }

            fn visit_map<A>(self, map: A) -> Result<VersionDep, A::Error>
            where
                A: serde::de::MapAccess<'de>,
            {
                let table =
                    VersionTable::deserialize(serde::de::value::MapAccessDeserializer::new(map))?;
                Ok(VersionDep {
                    version: table.version,
                    optional: table.optional,
                    features: table.features,
                })
            }
        }
        deserializer.deserialize_any(VersionOrTable)
    }
}

/// Git dependency spec.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct GitDep {
    pub version: String,
    pub git: String,
    pub rev: String,
    /// Optional dependency, enabled by a feature's `deps` list.
    #[serde(default, skip_serializing_if = "bool_is_false")]
    pub optional: bool,
    /// Features requested from the dependency's own `[features]`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub features: Vec<String>,
}

/// Path dependency spec.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PathDep {
    pub path: String,
    /// Optional dependency, enabled by a feature's `deps` list.
    #[serde(default, skip_serializing_if = "bool_is_false")]
    pub optional: bool,
    /// Features requested from the dependency's own `[features]`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub features: Vec<String>,
}

impl PathDep {
    /// Plain non-optional path dep.
    pub fn new(path: impl Into<String>) -> Self {
        Self {
            path: path.into(),
            optional: false,
            features: Vec::new(),
        }
    }
}

impl GitDep {
    /// Plain non-optional git dep.
    pub fn new(version: impl Into<String>, git: impl Into<String>, rev: impl Into<String>) -> Self {
        Self {
            version: version.into(),
            git: git.into(),
            rev: rev.into(),
            optional: false,
            features: Vec::new(),
        }
    }
}

impl Manifest {
    /// Load a manifest from a `zz.toml` file.
    pub fn load(path: &Path) -> Result<Self, String> {
        let content = std::fs::read_to_string(path)
            .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
        Self::parse(&content)
    }

    /// Parse a manifest from a string.
    ///
    /// Feature cross-references are validated here, so every caller
    /// (including dependency manifests loaded transitively) gets the same
    /// loud errors. A manifest without `[features]` is untouched.
    pub fn parse(s: &str) -> Result<Self, String> {
        let manifest: Manifest = toml::from_str(s).map_err(|e| format!("invalid zz.toml: {e}"))?;
        manifest.validate_features()?;
        Ok(manifest)
    }

    /// Save the manifest to a file.
    pub fn save(&self, path: &Path) -> Result<(), String> {
        let content =
            toml::to_string_pretty(self).map_err(|e| format!("cannot serialize manifest: {e}"))?;
        std::fs::write(path, content).map_err(|e| format!("cannot write {}: {e}", path.display()))
    }

    /// Hash of the `[dependencies]` section — for lockfile short-circuit.
    /// Stable: same deps produce same hash regardless of TOML whitespace/order.
    pub fn deps_hash(&self) -> String {
        // Sort keys for determinism
        let mut sorted: Vec<_> = self.dependencies.iter().collect();
        sorted.sort_by_key(|(k, _)| (*k).clone());
        let serialized = serde_json::to_string(&sorted).unwrap_or_default();
        hash::hash_bytes(serialized.as_bytes())
    }

    /// Check if this manifest has any path dependencies.
    pub fn has_path_deps(&self) -> bool {
        self.dependencies
            .values()
            .any(|d| matches!(d, DepSpec::Path(_)))
    }

    /// Enforce this manifest's minimum-compiler requirement, if any.
    /// `compiler_version` is the running `zz` version (e.g. `"0.1.6"`).
    /// Returns `Ok` when no requirement is set or it is satisfied.
    pub fn check_zz_version(&self, compiler_version: &str) -> Result<(), String> {
        match &self.package.zz {
            None => Ok(()),
            Some(req) => check_compiler_req(req, compiler_version)
                .map_err(|e| format!("{} requires {e}", self.package.name)),
        }
    }

    /// Validate `[features]`: names, module paths, and cross-references.
    ///
    /// - Feature names are identifiers/kebab strings (`^[A-Za-z][A-Za-z0-9_-]*$`).
    /// - Module paths are relative, non-empty, and never escape the root (`..`).
    /// - Every `deps` entry names a declared `optional = true` dependency.
    /// - Every `optional = true` dependency is referenced by at least one
    ///   feature (otherwise it can never be enabled).
    /// - Bare-list entries name a feature or an optional dependency.
    ///
    /// Cycles are detected by [`Manifest::enabled_features`], not here:
    /// an unreachable cycle is harmless until someone selects it.
    pub fn validate_features(&self) -> Result<(), String> {
        for (name, spec) in &self.features {
            if !is_valid_feature_name(name) {
                return Err(format!(
                    "invalid feature name `{name}` in [features]\n\
                     hint: use letters, digits, `-`, or `_`, starting with a letter (e.g. `locales-ar`)"
                ));
            }
            for module in spec.modules() {
                validate_feature_module(name, module)?;
            }
            for dep in spec.dep_refs() {
                if !self.dependencies.contains_key(dep) {
                    return Err(format!(
                        "feature `{name}` enables undeclared dependency `{dep}`\n\
                         hint: declare it under [dependencies] (e.g. `{dep} = {{ version = \"1.0\", optional = true }}`)"
                    ));
                }
            }
            for item in spec.feature_refs() {
                let is_feature = self.features.contains_key(item);
                let is_optional_dep = self
                    .dependencies
                    .get(item)
                    .is_some_and(|dep| dep.optional());
                if !is_feature && !is_optional_dep {
                    let hint = if self.dependencies.contains_key(item) {
                        format!(
                            "`{item}` is a non-optional dependency (always active); \
                             remove it from the feature list"
                        )
                    } else {
                        let mut available: Vec<&str> =
                            self.features.keys().map(String::as_str).collect();
                        available.sort();
                        format!("available features: {}", available.join(", "))
                    };
                    return Err(format!(
                        "feature `{name}` references unknown feature `{item}`\n\
                         hint: {hint}"
                    ));
                }
            }
        }
        // Optional deps must be reachable from at least one feature. A
        // dep that no feature enables can never be installed: loud error.
        let mut enabled_by_feature: BTreeSet<String> = BTreeSet::new();
        for (feature, spec) in &self.features {
            for dep in spec.dep_refs() {
                enabled_by_feature.insert(dep.clone());
            }
            for item in spec.feature_refs() {
                if self
                    .dependencies
                    .get(item)
                    .is_some_and(|dep| dep.optional())
                {
                    enabled_by_feature.insert(item.clone());
                }
            }
            let _ = feature;
        }
        for (dep, spec) in &self.dependencies {
            if spec.optional() && !enabled_by_feature.contains(dep) {
                return Err(format!(
                    "optional dependency `{dep}` is never enabled by any feature\n\
                     hint: add `deps = [\"{dep}\"]` to a [features] entry (e.g. `{dep} = {{ deps = [\"{dep}\"] }}`) \
                     or drop `optional = true`"
                ));
            }
        }
        Ok(())
    }

    /// Resolve the enabled feature set for `requested` (already-parsed
    /// `--features` names) or `all` (`--all-features`).
    ///
    /// The implicit `default` feature (when declared) is always enabled.
    /// Feature-to-feature references are followed transitively; unknown
    /// names and cycles are loud errors naming the offending feature.
    pub fn enabled_features(
        &self,
        requested: &[String],
        all: bool,
    ) -> Result<BTreeSet<String>, String> {
        self.validate_features()?;
        let mut enabled: BTreeSet<String> = BTreeSet::new();
        let mut seeds: Vec<String> = Vec::new();
        if all {
            seeds.extend(self.features.keys().cloned());
        } else {
            if self.features.contains_key("default") {
                seeds.push("default".to_string());
            }
            for name in requested {
                if !self.features.contains_key(name) {
                    let mut available: Vec<&str> =
                        self.features.keys().map(String::as_str).collect();
                    available.sort();
                    return Err(format!(
                        "unknown feature `{name}`\n\
                         hint: available features: {}\n\
                         hint: pass one or more with --features <name>[,<name>...] or use --all-features",
                        if available.is_empty() {
                            "(none declared)".to_string()
                        } else {
                            available.join(", ")
                        }
                    ));
                }
                seeds.push(name.clone());
            }
        }
        // DFS with an explicit stack; `path` detects cycles among
        // feature-to-feature references.
        fn visit(
            manifest: &Manifest,
            name: &str,
            path: &mut Vec<String>,
            done: &mut BTreeSet<String>,
            enabled: &mut BTreeSet<String>,
        ) -> Result<(), String> {
            if done.contains(name) {
                return Ok(());
            }
            if let Some(pos) = path.iter().position(|p| p == name) {
                let mut cycle = path[pos..].to_vec();
                cycle.push(name.to_string());
                return Err(format!(
                    "feature cycle: {}\n\
                     hint: feature lists must not reference each other in a loop",
                    cycle.join(" -> ")
                ));
            }
            enabled.insert(name.to_string());
            path.push(name.to_string());
            if let Some(spec) = manifest.features.get(name) {
                for item in spec.feature_refs() {
                    if manifest.features.contains_key(item) {
                        visit(manifest, item, path, done, enabled)?;
                    }
                }
            }
            path.pop();
            done.insert(name.to_string());
            Ok(())
        }
        let mut done = BTreeSet::new();
        for seed in seeds {
            let mut path = Vec::new();
            visit(self, &seed, &mut path, &mut done, &mut enabled)?;
        }
        Ok(enabled)
    }

    /// Resolve a [`FeatureSelection`] against this manifest.
    pub fn resolve_features(
        &self,
        selection: &FeatureSelection,
    ) -> Result<BTreeSet<String>, String> {
        self.enabled_features(&selection.names(), selection.all)
    }

    /// Dependencies active for an enabled feature set: every non-optional
    /// dependency, plus optional deps named by an enabled feature's `deps`
    /// (or directly by a bare-list entry).
    pub fn enabled_dependencies(
        &self,
        enabled: &BTreeSet<String>,
    ) -> Result<BTreeSet<String>, String> {
        self.validate_features()?;
        let mut active: BTreeSet<String> = self
            .dependencies
            .iter()
            .filter(|(_, spec)| !spec.optional())
            .map(|(name, _)| name.clone())
            .collect();
        for feature in enabled {
            let Some(spec) = self.features.get(feature) else {
                return Err(format!(
                    "internal: unknown enabled feature `{feature}`\n\
                     hint: pass feature names to Manifest::enabled_features"
                ));
            };
            for dep in spec.dep_refs() {
                // validate_features guarantees the dep exists.
                active.insert(dep.clone());
            }
            for item in spec.feature_refs() {
                if self
                    .dependencies
                    .get(item)
                    .is_some_and(|dep| dep.optional())
                {
                    active.insert(item.clone());
                }
            }
        }
        Ok(active)
    }

    /// Convenience: selection → active dependency names.
    pub fn active_dependency_names(
        &self,
        selection: &FeatureSelection,
    ) -> Result<BTreeSet<String>, String> {
        let enabled = self.resolve_features(selection)?;
        self.enabled_dependencies(&enabled)
    }

    /// Module path → feature names that gate it. Used by the loader to
    /// turn a disabled-module import into a fix-it diagnostic.
    pub fn module_gates(&self) -> BTreeMap<String, Vec<String>> {
        let mut gates: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for (feature, spec) in &self.features {
            for module in spec.modules() {
                gates
                    .entry(normalize_module_path(module))
                    .or_default()
                    .push(feature.clone());
            }
        }
        for features in gates.values_mut() {
            features.sort();
            features.dedup();
        }
        gates
    }

    /// Declared feature names, sorted (`zz info` / diagnostics).
    pub fn feature_names(&self) -> Vec<String> {
        self.features.keys().cloned().collect()
    }

    /// Configured entry override (`[package] entry = "src/cli.zz"`).
    ///
    /// Returns `None` when unset (callers fall back to conventional
    /// discovery). A configured entry is an explicit promise: it must be
    /// relative, stay inside the package (symlinks included), name a
    /// `.zz` file, and exist — any violation is a loud error, never a
    /// silent fallback to the conventional entry.
    pub fn entry_path(&self, pkg_root: &Path) -> Result<Option<PathBuf>, String> {
        let Some(rel) = self.package.entry.as_deref() else {
            return Ok(None);
        };
        let bad = |why: &str| {
            format!(
                "invalid [package] entry `{rel}`: {why}\n\
                 hint: use a relative .zz path inside the package, e.g. entry = \"src/cli.zz\""
            )
        };
        if rel.is_empty() {
            return Err(bad("must not be empty"));
        }
        let p = Path::new(rel);
        if escapes_root(p) {
            return Err(bad("must be relative and stay inside the package"));
        }
        if p.extension().and_then(|e| e.to_str()) != Some("zz") {
            return Err(bad("must name a .zz file"));
        }
        let joined = pkg_root.join(p);
        let canon = joined
            .canonicalize()
            .map_err(|_| bad("file does not exist"))?;
        let root_canon = pkg_root
            .canonicalize()
            .unwrap_or_else(|_| pkg_root.to_path_buf());
        if !canon.starts_with(&root_canon) {
            return Err(bad("must stay inside the package (symlink escape)"));
        }
        if !canon.is_file() {
            return Err(bad("is not a file"));
        }
        Ok(Some(joined))
    }

    /// Resolve a path dep relative to the manifest directory.
    pub fn resolve_path_dep(&self, manifest_dir: &Path, dep_name: &str) -> Option<PathBuf> {
        match self.dependencies.get(dep_name)? {
            DepSpec::Path(p) => Some(manifest_dir.join(&p.path)),
            _ => None,
        }
    }

    /// Create a minimal init manifest in the current directory.
    pub fn create_init(dir: &Path, name: &str) -> Result<Self, String> {
        Self::create_init_opts(dir, name, &InitOptions::default())
    }

    /// Create an init manifest with optional enriched metadata.
    pub fn create_init_opts(dir: &Path, name: &str, opts: &InitOptions) -> Result<Self, String> {
        let manifest = Manifest {
            package: PackageSpec {
                name: name.to_string(),
                version: "0.1.0".to_string(),
                authors: opts.authors.clone(),
                description: opts.description.clone(),
                license: opts.license.clone(),
                repository: opts.repository.clone(),
                category: None,
                keywords: Vec::new(),
                zz: None,
                entry: None,
            },
            dependencies: HashMap::new(),
            native: None,
            collections: BTreeMap::new(),
            features: BTreeMap::new(),
        };
        let path = dir.join("zz.toml");
        manifest.save(&path)?;
        Self::ensure_gitignore(dir)?;
        Ok(manifest)
    }

    /// Entries every ZZ project gitignores: fetched deps, native build
    /// outputs, and compiled binaries. `zz.toml` and `zz.lock` are
    /// deliberately absent — both are committed (lockfile = reproducibility
    /// source of truth, same convention as Cargo).
    /// `src/bin/` is a legacy entry from when builds published next to the
    /// source file; builds now publish to `<root>/bin/`. It stays until
    /// existing checkouts are migrated, then gets removed.
    const GITIGNORE_ENTRIES: &[&str] = &["vendor/", "build/", "bin/", "src/bin/"];

    /// Create `.gitignore` if absent, or append missing ZZ entries if
    /// present. Existing content is never removed or reordered.
    pub fn ensure_gitignore(dir: &Path) -> Result<(), String> {
        let path = dir.join(".gitignore");
        let existing = std::fs::read_to_string(&path).unwrap_or_default();
        let present: std::collections::HashSet<&str> = existing.lines().map(str::trim).collect();
        let missing: Vec<&&str> = Self::GITIGNORE_ENTRIES
            .iter()
            .filter(|e| !present.contains(**e))
            .collect();
        if missing.is_empty() {
            return Ok(());
        }
        let mut out = existing;
        if !out.is_empty() && !out.ends_with('\n') {
            out.push('\n');
        }
        out.push_str("\n# zz: fetched deps, native build outputs, compiled binaries\n");
        for entry in missing {
            out.push_str(entry);
            out.push('\n');
        }
        std::fs::write(&path, out).map_err(|e| format!("cannot write .gitignore: {e}"))?;
        Ok(())
    }

    /// Starter `src/main.zz` for a template. Single source of truth shared
    /// by `zz new` (below) and `zz init` (in `zz_cli::pm`): the default is
    /// a bare `main` with no imports, so a fresh project checks warning-free.
    pub fn starter_content(template: Option<&str>) -> &'static str {
        match template {
            Some("lib") => {
                "/// Add one to a number.\npub func add_one(n: int) -> int {\n    return n + 1\n}\n"
            }
            Some("web") => {
                "import std.http\n\nfunc main() {\n    s := http.server()\n    s2 := http.route_get(s, \"/\", |req| \"Hello, ZZ!\")\n    http.listen(s2, 8080) ?? println(\"failed to start server\")\n}\n"
            }
            _ => "func main() {\n    println(\"Hello, ZZ!\")\n}\n",
        }
    }

    /// Create a new project directory with manifest and starter code.
    pub fn create_new(
        parent_dir: &Path,
        name: &str,
        template: Option<&str>,
    ) -> Result<PathBuf, String> {
        Self::create_new_opts(parent_dir, name, template, &InitOptions::default())
    }

    /// Create a new project directory with enriched manifest metadata.
    pub fn create_new_opts(
        parent_dir: &Path,
        name: &str,
        template: Option<&str>,
        opts: &InitOptions,
    ) -> Result<PathBuf, String> {
        let project_dir = parent_dir.join(name);
        std::fs::create_dir_all(&project_dir)
            .map_err(|e| format!("cannot create {}: {e}", project_dir.display()))?;
        std::fs::create_dir_all(project_dir.join("src"))
            .map_err(|e| format!("cannot create src/: {e}"))?;

        // Write manifest
        Self::create_init_opts(&project_dir, name, opts)?;

        // Write starter source (single-sourced: see `starter_content`).
        let main_content = Self::starter_content(template);
        std::fs::write(project_dir.join("src/main.zz"), main_content)
            .map_err(|e| format!("cannot write src/main.zz: {e}"))?;

        Ok(project_dir)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn tmp_dir(name: &str) -> PathBuf {
        let d =
            std::env::temp_dir().join(format!("zz_manifest_test_{name}_{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn round_trip() {
        let m = Manifest {
            package: PackageSpec {
                name: "test_pkg".into(),
                version: "1.0.0".into(),
                authors: Vec::new(),
                description: None,
                license: None,
                repository: None,
                category: None,
                keywords: Vec::new(),
                zz: None,
                entry: None,
            },
            collections: BTreeMap::new(),
            dependencies: {
                let mut d = HashMap::new();
                d.insert("foo".into(), DepSpec::Version("^1.2.0".into()));
                d.insert(
                    "bar".into(),
                    DepSpec::Git(GitDep {
                        version: "2.0".into(),
                        git: "https://github.com/user/repo".into(),
                        rev: "main".into(),
                        optional: false,
                        features: Vec::new(),
                    }),
                );
                d.insert(
                    "baz".into(),
                    DepSpec::Path(PathDep {
                        path: "../baz".into(),
                        optional: false,
                        features: Vec::new(),
                    }),
                );
                d
            },
            native: None,
            features: BTreeMap::new(),
        };

        let d = tmp_dir("round_trip");
        let path = d.join("zz.toml");
        m.save(&path).unwrap();
        let loaded = Manifest::load(&path).unwrap();
        assert_eq!(m, loaded);
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn version_table_forms_parse_as_version() {
        // #419: `[dependencies.x]` tables, Cargo-style inline tables, and
        // bare strings all spell a registry dep — including mixed in one
        // manifest (the issue's exact shape).
        let m = Manifest::parse(
            r#"
[package]
name = "faker-tests"
version = "0.1.0"

[dependencies.faker]
path = ".."

[dependencies.rand]
version = "^0.4.1"

[dependencies]
inline_bare = "^1.0.0"
inline_table = { version = "^2.0.0" }
"#,
        )
        .expect("mixed dependency spellings must parse");
        assert_eq!(
            m.dependencies.get("rand"),
            Some(&DepSpec::Version("^0.4.1".into()))
        );
        assert_eq!(
            m.dependencies.get("inline_bare"),
            Some(&DepSpec::Version("^1.0.0".into()))
        );
        assert_eq!(
            m.dependencies.get("inline_table"),
            Some(&DepSpec::Version("^2.0.0".into()))
        );
        assert!(matches!(
            m.dependencies.get("faker"),
            Some(DepSpec::Path(_))
        ));
    }

    #[test]
    fn version_table_extras_stay_loud() {
        // Unknown keys must not silently degrade to a registry dep: a
        // `git` without `rev` (or any stray key) is still an error.
        // `optional` and `features` are the only widened keys (#488).
        for bad in [
            "[package]\nname = \"x\"\nversion = \"0.1.0\"\n[dependencies]\ndep = { version = \"^1.0\", git = \"https://example.com/r\" }\n",
            "[package]\nname = \"x\"\nversion = \"0.1.0\"\n[dependencies]\ndep = { version = \"^1.0\", rev = \"main\" }\n",
        ] {
            assert!(
                Manifest::parse(bad).is_err(),
                "extra keys must not parse silently: {bad}"
            );
        }
    }

    const FEATURE_MANIFEST: &str = r#"
[package]
name = "faker"
version = "1.0.0"

[dependencies]
faker_locales_ar = { path = "../locales/ar", optional = true }
faker_locales_fr = { path = "../locales/fr", optional = true }
rand = "^0.4"

[features]
default = ["std"]
std = []
locales-ar = { modules = ["locales/ar"], deps = ["faker_locales_ar"] }
locales-fr = { modules = ["locales/fr", "locales/fr/extra"], deps = ["faker_locales_fr"] }
all-locales = ["locales-ar", "locales-fr"]
"#;

    #[test]
    fn feature_shapes_parse_and_round_trip() {
        let m = Manifest::parse(FEATURE_MANIFEST).expect("both feature shapes parse");
        assert_eq!(
            m.features.get("default"),
            Some(&FeatureSpec::List(vec!["std".to_string()]))
        );
        assert_eq!(
            m.features.get("locales-ar"),
            Some(&FeatureSpec::Table(FeatureTable {
                modules: vec!["locales/ar".to_string()],
                deps: vec!["faker_locales_ar".to_string()],
            }))
        );
        assert!(matches!(
            m.dependencies.get("faker_locales_ar"),
            Some(DepSpec::Path(p)) if p.optional && p.features.is_empty()
        ));
        // No features in the value → canonical bare string on save.
        let d = tmp_dir("features_round_trip");
        let path = d.join("zz.toml");
        m.save(&path).unwrap();
        let text = fs::read_to_string(&path).unwrap();
        assert!(text.contains("rand = \"^0.4\""), "{text}");
        assert!(text.contains("[features]"), "{text}");
        assert!(text.contains("optional = true"), "{text}");
        let loaded = Manifest::load(&path).unwrap();
        assert_eq!(m, loaded);
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn feature_closure_is_transitive() {
        let m = Manifest::parse(FEATURE_MANIFEST).unwrap();
        let enabled = m.enabled_features(&[], false).unwrap();
        assert!(enabled.contains("default"));
        assert!(enabled.contains("std"));
        assert!(!enabled.contains("locales-ar"));

        let enabled = m
            .enabled_features(&["all-locales".to_string()], false)
            .unwrap();
        assert!(enabled.contains("all-locales"));
        assert!(enabled.contains("locales-ar"));
        assert!(enabled.contains("locales-fr"));
        // default stays implicit even when other features are requested.
        assert!(enabled.contains("default"));

        let all = m.enabled_features(&[], true).unwrap();
        assert_eq!(all, m.features.keys().cloned().collect());

        let active = m.enabled_dependencies(&enabled).unwrap();
        assert!(active.contains("rand"), "non-optional always active");
        assert!(active.contains("faker_locales_ar"));
        assert!(active.contains("faker_locales_fr"));
        // Default selection excludes optional locale deps.
        let default_active = m
            .enabled_dependencies(&m.enabled_features(&[], false).unwrap())
            .unwrap();
        assert!(default_active.contains("rand"));
        assert!(!default_active.contains("faker_locales_ar"));
    }

    #[test]
    fn feature_unknown_name_is_loud() {
        let m = Manifest::parse(FEATURE_MANIFEST).unwrap();
        let err = m
            .enabled_features(&["locales-de".to_string()], false)
            .unwrap_err();
        assert!(err.contains("unknown feature `locales-de`"), "{err}");
        assert!(err.contains("locales-ar"), "lists available: {err}");
    }

    #[test]
    fn feature_cycle_is_loud() {
        let m = Manifest::parse(FEATURE_MANIFEST).unwrap();
        let mut cyclic = m.clone();
        cyclic.features.insert(
            "loop-a".to_string(),
            FeatureSpec::List(vec!["loop-b".to_string()]),
        );
        cyclic.features.insert(
            "loop-b".to_string(),
            FeatureSpec::List(vec!["loop-a".to_string()]),
        );
        let err = cyclic
            .enabled_features(&["loop-a".to_string()], false)
            .unwrap_err();
        assert!(err.contains("feature cycle"), "{err}");
        assert!(err.contains("loop-a") && err.contains("loop-b"), "{err}");
    }

    #[test]
    fn feature_validation_rejects_bad_manifests() {
        // Optional dep no feature can enable.
        let err = Manifest::parse(
            r#"
[package]
name = "x"
version = "0.1.0"
[dependencies]
lonely = { version = "1.0", optional = true }
[features]
default = []
"#,
        )
        .unwrap_err();
        assert!(err.contains("never enabled"), "{err}");

        // Dep referenced by a feature but not declared.
        let err = Manifest::parse(
            r#"
[package]
name = "x"
version = "0.1.0"
[features]
gone = { deps = ["missing"] }
"#,
        )
        .unwrap_err();
        assert!(err.contains("undeclared dependency `missing`"), "{err}");

        // Module paths may not escape the package root.
        for evil in ["../secrets", "/abs/path", ""] {
            let err = Manifest::parse(&format!(
                "[package]\nname = \"x\"\nversion = \"0.1.0\"\n[features]\nf = {{ modules = [\"{evil}\"] }}\n"
            ))
            .unwrap_err();
            assert!(err.contains("invalid module"), "`{evil}`: {err}");
        }

        // Feature names must be identifier/kebab-ish.
        let err = Manifest::parse(
            "[package]\nname = \"x\"\nversion = \"0.1.0\"\n[features]\n\"bad name\" = []\n",
        )
        .unwrap_err();
        assert!(err.contains("invalid feature name"), "{err}");
    }

    #[test]
    fn dep_flags_round_trip_canonically() {
        let m = Manifest::parse(
            r#"
[package]
name = "consumer"
version = "0.1.0"

[dependencies]
plain = "^1.0"
flagged = { version = "^2.0", optional = true, features = ["ar", "fr"] }
g = { git = "https://example.com/g.git", rev = "main", version = "*", optional = true, features = ["x"] }
p = { path = "../p", optional = true }
[features]
f = { deps = ["flagged", "g", "p"] }
"#,
        )
        .unwrap();
        let d = tmp_dir("dep_flags_rt");
        let path = d.join("zz.toml");
        m.save(&path).unwrap();
        let text = fs::read_to_string(&path).unwrap();
        assert!(text.contains("plain = \"^1.0\""), "{text}");
        assert_eq!(Manifest::load(&path).unwrap(), m);
        let loaded = Manifest::parse(&text).unwrap();
        assert_eq!(loaded.dependencies["flagged"].features(), ["ar", "fr"]);
        assert!(loaded.dependencies["flagged"].optional());
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn manifest_without_features_is_unchanged() {
        // Backward compatibility: adding `[features]` support must not
        // change the serialized form of an old manifest.
        let original = r#"[package]
name = "legacy"
version = "1.0.0"
authors = ["Alice"]

[dependencies]
foo = "^1.0"
bar = { version = "2.0", git = "https://example.com/r", rev = "main" }
baz = { path = "../baz" }
"#;
        let m = Manifest::parse(original).unwrap();
        let d = tmp_dir("legacy_unchanged");
        let path = d.join("zz.toml");
        m.save(&path).unwrap();
        let saved = fs::read_to_string(&path).unwrap();
        assert!(!saved.contains("[features]"), "{saved}");
        assert!(!saved.contains("optional"), "{saved}");
        assert!(!saved.contains("features ="), "{saved}");
        assert_eq!(Manifest::parse(&saved).unwrap(), m);
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn deps_hash_stable() {
        let m1 = Manifest::parse(
            r#"
[package]
name = "test"
version = "0.1.0"

[dependencies]
foo = "^1.0"
bar = "^2.0"
"#,
        )
        .unwrap();

        let m2 = Manifest::parse(
            r#"
[package]
name = "test"
version = "0.1.0"

[dependencies]
bar = "^2.0"
foo = "^1.0"
"#,
        )
        .unwrap();

        // Different TOML ordering, same deps → same hash
        assert_eq!(m1.deps_hash(), m2.deps_hash());
    }

    #[test]
    fn deps_hash_empty() {
        let m = Manifest::parse(
            r#"
[package]
name = "test"
version = "0.1.0"
"#,
        )
        .unwrap();
        let h = m.deps_hash();
        assert!(!h.is_empty());
    }

    #[test]
    fn has_path_deps_true() {
        let m = Manifest::parse(
            r#"
[package]
name = "test"
version = "0.1.0"

[dependencies]
foo = { path = "../foo" }
"#,
        )
        .unwrap();
        assert!(m.has_path_deps());
    }

    #[test]
    fn has_path_deps_false() {
        let m = Manifest::parse(
            r#"
[package]
name = "test"
version = "0.1.0"

[dependencies]
foo = "^1.0"
"#,
        )
        .unwrap();
        assert!(!m.has_path_deps());
    }

    fn entry_project(tag: &str, manifest_toml: &str, files: &[&str]) -> PathBuf {
        let d = tmp_dir(tag);
        fs::write(d.join("zz.toml"), manifest_toml).unwrap();
        for f in files {
            let p = d.join(f);
            fs::create_dir_all(p.parent().unwrap()).unwrap();
            fs::write(&p, "println(\"hi\")\n").unwrap();
        }
        d
    }

    #[test]
    fn entry_path_unset_is_none() {
        let d = entry_project(
            "entry_none",
            "[package]\nname = \"a\"\nversion = \"0.1.0\"\n",
            &["src/main.zz"],
        );
        let m = Manifest::load(&d.join("zz.toml")).unwrap();
        assert_eq!(m.entry_path(&d).unwrap(), None);
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn entry_path_resolves_configured_file() {
        let d = entry_project(
            "entry_ok",
            "[package]\nname = \"a\"\nversion = \"0.1.0\"\nentry = \"src/cli.zz\"\n",
            &["src/cli.zz"],
        );
        let m = Manifest::load(&d.join("zz.toml")).unwrap();
        assert_eq!(m.entry_path(&d).unwrap(), Some(d.join("src/cli.zz")));
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn entry_path_rejects_bad_values() {
        // (manifest entry value, files present, expected error fragment)
        let cases: &[(&str, &[&str], &str)] = &[
            ("../evil.zz", &[], "stay inside"),
            ("/abs.zz", &[], "relative"),
            ("", &[], "empty"),
            ("src/cli.txt", &["src/cli.txt"], ".zz file"),
            ("src/missing.zz", &[], "does not exist"),
        ];
        for (i, (entry, files, fragment)) in cases.iter().enumerate() {
            let d = entry_project(
                &format!("entry_bad_{i}"),
                &format!("[package]\nname = \"a\"\nversion = \"0.1.0\"\nentry = \"{entry}\"\n"),
                files,
            );
            let m = Manifest::load(&d.join("zz.toml")).unwrap();
            let err = m.entry_path(&d).expect_err("bad entry must fail");
            assert!(
                err.contains(fragment),
                "entry `{entry}`: expected `{fragment}` in:\n{err}"
            );
            let _ = fs::remove_dir_all(&d);
        }
    }

    #[test]
    fn create_new_cli_template() {
        let d = tmp_dir("new_cli");
        let project = Manifest::create_new(&d, "myapp", None).unwrap();
        assert!(project.join("zz.toml").exists());
        assert!(project.join("src/main.zz").exists());
        let src = fs::read_to_string(project.join("src/main.zz")).unwrap();
        assert!(src.contains("Hello, ZZ!"));
        // No unused imports: a fresh project must check warning-free.
        assert!(!src.contains("import"));
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn create_new_lib_template() {
        let d = tmp_dir("new_lib");
        let project = Manifest::create_new(&d, "mylib", Some("lib")).unwrap();
        let src = fs::read_to_string(project.join("src/main.zz")).unwrap();
        assert!(src.contains("add_one"));
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn create_new_web_template() {
        let d = tmp_dir("new_web");
        let project = Manifest::create_new(&d, "myweb", Some("web")).unwrap();
        let src = fs::read_to_string(project.join("src/main.zz")).unwrap();
        assert!(src.contains("http.server"));
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn gitignore_created_when_absent() {
        let d = tmp_dir("gi_new");
        Manifest::ensure_gitignore(&d).unwrap();
        let content = fs::read_to_string(d.join(".gitignore")).unwrap();
        assert!(content.contains("vendor/"));
        assert!(content.contains("build/"));
        assert!(content.lines().any(|l| l.trim() == "bin/"));
        assert!(content.contains("src/bin/"));
        assert!(!content.contains("zz.toml"));
        assert!(!content.contains("zz.lock"));
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn gitignore_appends_without_clobbering() {
        let d = tmp_dir("gi_append");
        fs::write(d.join(".gitignore"), "target/\nvendor/\n").unwrap();
        Manifest::ensure_gitignore(&d).unwrap();
        let content = fs::read_to_string(d.join(".gitignore")).unwrap();
        assert!(content.contains("target/"));
        assert_eq!(content.matches("vendor/").count(), 1);
        assert!(content.contains("build/"));
        assert!(content.lines().any(|l| l.trim() == "bin/"));
        assert!(content.contains("src/bin/"));
        // Idempotent: second run changes nothing.
        Manifest::ensure_gitignore(&d).unwrap();
        let again = fs::read_to_string(d.join(".gitignore")).unwrap();
        assert_eq!(content, again);
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn create_init_writes_gitignore() {
        let d = tmp_dir("init_gi");
        Manifest::create_init(&d, "myapp").unwrap();
        assert!(d.join(".gitignore").exists());
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn metadata_round_trip() {
        let m = Manifest {
            package: PackageSpec {
                name: "enriched".into(),
                version: "2.3.4".into(),
                authors: vec!["Alice <alice@example.com>".into()],
                description: Some("Does things".into()),
                license: Some("MIT".into()),
                repository: Some("https://github.com/user/enriched".into()),
                category: Some("cli".into()),
                keywords: vec!["tool".into()],
                zz: None,
                entry: Some("src/cli.zz".into()),
            },
            dependencies: HashMap::new(),
            native: None,
            collections: BTreeMap::new(),
            features: BTreeMap::new(),
        };

        let d = tmp_dir("meta_rt");
        let path = d.join("zz.toml");
        m.save(&path).unwrap();
        let loaded = Manifest::load(&path).unwrap();
        assert_eq!(m, loaded);
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn legacy_manifest_without_metadata_parses() {
        // Manifests written before enrichment have no new keys.
        let m = Manifest::parse(
            r#"
[package]
name = "legacy"
version = "0.1.0"

[dependencies]
foo = "^1.0"
"#,
        )
        .unwrap();
        assert_eq!(m.package.name, "legacy");
        assert!(m.package.authors.is_empty());
        assert_eq!(m.package.description, None);
        assert_eq!(m.package.license, None);
        assert_eq!(m.package.repository, None);
    }

    #[test]
    fn save_omits_empty_metadata() {
        // Empty metadata stays out of the TOML so old diffs stay minimal.
        let m = Manifest::default();
        let d = tmp_dir("meta_omit");
        let path = d.join("zz.toml");
        m.save(&path).unwrap();
        let content = fs::read_to_string(&path).unwrap();
        assert!(!content.contains("authors"));
        assert!(!content.contains("description"));
        assert!(!content.contains("license"));
        assert!(!content.contains("repository"));
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn create_init_opts_writes_metadata() {
        let d = tmp_dir("init_opts");
        let opts = InitOptions {
            authors: vec!["Bob".into()],
            description: Some("A tool".into()),
            license: Some("MIT".into()),
            repository: Some("https://example.com/bob/tool".into()),
        };
        let m = Manifest::create_init_opts(&d, "tool", &opts).unwrap();
        assert_eq!(m.package.authors, vec!["Bob".to_string()]);
        assert_eq!(m.package.description.as_deref(), Some("A tool"));
        let reloaded = Manifest::load(&d.join("zz.toml")).unwrap();
        assert_eq!(m, reloaded);
        let _ = fs::remove_dir_all(&d);
    }

    fn cc_manifest(toml_native: &str) -> Manifest {
        Manifest::parse(&format!(
            "[package]\nname = \"t\"\nversion = \"0.1.0\"\n{toml_native}"
        ))
        .expect("test manifest parses")
    }

    fn cc_spec() -> BuildCcSpec {
        BuildCcSpec {
            sources: vec!["csrc/wrapper.c".to_string()],
            include_dirs: vec!["csrc".to_string()],
            defines: vec!["NDEBUG".to_string()],
            cflags: vec!["-O2".to_string(), "-Wall".to_string(), "-fPIC".to_string()],
            libs: vec![],
            pkg_config: vec!["vips".to_string()],
            targets: vec!["linux-x86_64-gnu-glibc2.28".to_string()],
        }
    }

    fn validate_cc(spec: &BuildCcSpec) -> Result<(), String> {
        NativeSpec {
            backend: Some(NativeBackend::Cc),
            manifest: None,
            build: None,
            build_cc: Some(spec.clone()),
            build_rust: None,
            prebuilt: None,
            pkg_config: None,
        }
        .validate(Path::new("/pkg"))
    }

    #[test]
    fn legacy_hook_parses_to_hook_backend() {
        let m = cc_manifest("[native]\nbuild = \"build.sh\"\n");
        let n = m.native.expect("native present");
        assert_eq!(n.build.as_deref(), Some("build.sh"));
        assert_eq!(n.resolved_backend(), NativeBackend::Hook);
        assert!(n.validate(Path::new("/pkg")).is_ok());
    }

    #[test]
    fn declarative_shape_resolves_cc() {
        let m = cc_manifest(
            "[native]\nbackend = \"cc\"\n[native.build-cc]\nsources = [\"csrc/wrapper.c\"]\n",
        );
        let n = m.native.expect("native present");
        assert_eq!(n.resolved_backend(), NativeBackend::Cc);
        assert!(n.manifest_path() == "plugin.zzi");
    }

    #[test]
    fn mixed_hook_and_cc_rejected() {
        let m = cc_manifest(
            "[native]\nbuild = \"build.sh\"\n[native.build-cc]\nsources = [\"csrc/wrapper.c\"]\n",
        );
        let err = m
            .native
            .expect("native present")
            .validate(Path::new("/pkg"))
            .unwrap_err();
        assert!(err.contains("mutually exclusive"), "{err}");
    }

    fn rust_pkg(name: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!(
            "zz_manifest_rust_{name}_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(d.join("native/src")).unwrap();
        fs::write(
            d.join("native/Cargo.toml"),
            "[package]\nname=\"rplug\"\nversion=\"0.1.0\"\nlicense=\"MIT\"\n\n[lib]\ncrate-type=[\"cdylib\",\"staticlib\"]\n",
        )
        .unwrap();
        fs::write(
            d.join("native/Cargo.lock"),
            "version = 3\n[[package]]\nname=\"rplug\"\nversion=\"0.1.0\"\n",
        )
        .unwrap();
        fs::write(d.join("native/src/lib.rs"), "").unwrap();
        d
    }

    fn rust_native(spec_extra: &str) -> NativeSpec {
        cc_manifest(&format!(
            "[native]\n[native.build-rust]\ncrate = \"native\"\n{spec_extra}"
        ))
        .native
        .expect("native present")
    }

    #[test]
    fn rust_shape_resolves_rust_and_default_range() {
        let n = rust_native("");
        assert_eq!(n.resolved_backend(), NativeBackend::Rust);
        let spec = n.build_rust.as_ref().unwrap();
        assert_eq!(spec.rustc_req(), DEFAULT_RUSTC_RANGE);
        assert_eq!(spec.rust_profile().unwrap(), RustProfile::Release);
    }

    #[test]
    fn mixed_rust_and_cc_rejected() {
        let m = cc_manifest(
            "[native]\n[native.build-cc]\nsources = [\"csrc/wrapper.c\"]\n[native.build-rust]\ncrate = \"native\"\n",
        );
        let err = m
            .native
            .expect("native present")
            .validate(Path::new("/pkg"))
            .unwrap_err();
        assert!(err.contains("mutually exclusive"), "{err}");
    }

    #[test]
    fn rust_backend_without_table_rejected() {
        let n = cc_manifest("[native]\nbackend = \"rust\"\n")
            .native
            .expect("native present");
        let err = n.validate(Path::new("/pkg")).unwrap_err();
        assert!(err.contains("requires `[native.build-rust]`"), "{err}");
    }

    #[test]
    fn rust_crate_validates_lock_and_crate_types() {
        let d = rust_pkg("ok");
        let n = rust_native("");
        assert!(n.validate(&d).is_ok());

        // Missing committed lock.
        fs::remove_file(d.join("native/Cargo.lock")).unwrap();
        let err = n.validate(&d).unwrap_err();
        assert!(err.contains("Cargo.lock"), "{err}");
        assert!(err.contains("commit"), "{err}");
        fs::write(
            d.join("native/Cargo.lock"),
            "version = 3\n[[package]]\nname=\"rplug\"\nversion=\"0.1.0\"\n",
        )
        .unwrap();

        // Missing staticlib.
        fs::write(
            d.join("native/Cargo.toml"),
            "[package]\nname=\"rplug\"\nversion=\"0.1.0\"\n\n[lib]\ncrate-type=[\"cdylib\"]\n",
        )
        .unwrap();
        let err = n.validate(&d).unwrap_err();
        assert!(err.contains("staticlib"), "{err}");

        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn rust_build_rs_rejected() {
        let d = rust_pkg("buildrs");
        fs::write(d.join("native/build.rs"), "fn main() {}\n").unwrap();
        let err = rust_native("").validate(&d).unwrap_err();
        assert!(err.contains("build.rs"), "{err}");
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn rust_path_escape_rejected() {
        let d = rust_pkg("escape");
        let n = cc_manifest("[native]\n[native.build-rust]\ncrate = \"../outside\"\n")
            .native
            .expect("native present");
        let err = n.validate(&d).unwrap_err();
        assert!(
            err.contains("must be relative and stay inside the package"),
            "{err}"
        );
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn rust_bad_profile_range_features_and_target_rejected() {
        let d = rust_pkg("bad_fields");
        for extra in [
            "profile = \"fast\"\n",
            "rustc_range = \"not-a-range\"\n",
            "cargo_features = [\"a,b\"]\n",
            "target = \"../x86\"\n",
        ] {
            // Re-parse so the table body exercises serde + validation.
            let parsed = cc_manifest(&format!(
                "[native]\n[native.build-rust]\ncrate = \"native\"\n{extra}"
            ))
            .native
            .expect("native present");
            assert!(parsed.validate(&d).is_err(), "`{extra}` must be rejected");
        }
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn backend_shape_mismatch_rejected() {
        // backend=cc without the table.
        let m = cc_manifest("[native]\nbackend = \"cc\"\n");
        let err = m
            .native
            .expect("native present")
            .validate(Path::new("/pkg"))
            .unwrap_err();
        assert!(err.contains("requires `[native.build-cc]`"), "{err}");
        // backend=hook without a script.
        let m = cc_manifest("[native]\nbackend = \"hook\"\n");
        let err = m
            .native
            .expect("native present")
            .validate(Path::new("/pkg"))
            .unwrap_err();
        assert!(err.contains("requires `build"), "{err}");
    }

    #[test]
    fn dual_pkg_config_rejected() {
        let mut spec = cc_spec();
        spec.pkg_config = vec!["vips".to_string()];
        let n = NativeSpec {
            backend: Some(NativeBackend::Cc),
            manifest: None,
            build: None,
            build_cc: Some(spec),
            build_rust: None,
            prebuilt: None,
            pkg_config: Some("vips".to_string()),
        };
        let err = n.validate(Path::new("/pkg")).unwrap_err();
        assert!(err.contains("mutually exclusive"), "{err}");
    }

    #[test]
    fn path_escape_rejected() {
        for evil in ["../evil.c", "/abs.c", "../x.h"] {
            let mut spec = cc_spec();
            spec.sources = vec![evil.to_string()];
            assert!(
                validate_cc(&spec).is_err(),
                "source `{evil}` must be rejected"
            );
        }
        let mut spec = cc_spec();
        spec.sources = vec!["csrc/ok.c".to_string()];
        spec.include_dirs = vec!["../inc".to_string()];
        assert!(validate_cc(&spec).is_err());
    }

    /// Every spelling a Windows or Unix host could call absolute must be
    /// refused on *both* — a manifest accepted on Linux must not become an
    /// escape when the same package is built on Windows.
    #[test]
    fn path_escape_spellings_rejected_everywhere() {
        for evil in [
            "/abs.c",
            "\\abs.c",
            "C:\\abs.c",
            "C:/abs.c",
            "C:abs.c",
            "\\\\server\\share\\evil.c",
            "../x.c",
            "a/../../x.c",
        ] {
            let mut spec = cc_spec();
            spec.sources = vec![evil.to_string()];
            let err = validate_cc(&spec).expect_err(evil);
            assert!(
                err.contains("must be relative and stay inside the package"),
                "`{evil}` rejected for the wrong reason: {err}"
            );
            // include_dirs take the same guard.
            let mut spec = cc_spec();
            spec.include_dirs = vec![evil.to_string()];
            assert!(validate_cc(&spec).is_err(), "include_dir `{evil}`");
        }
    }

    #[test]
    fn non_c_source_rejected() {
        let mut spec = cc_spec();
        spec.sources = vec!["csrc/main.cpp".to_string()];
        let err = validate_cc(&spec).unwrap_err();
        assert!(err.contains(".c"), "{err}");
    }

    #[test]
    fn empty_sources_rejected() {
        let mut spec = cc_spec();
        spec.sources = vec![];
        assert!(validate_cc(&spec).is_err());
    }

    // Rejected cflag classes — one assertion per class from the plan.
    #[test]
    fn cflag_comma_rejected() {
        assert!(validate_cflag("-Wl,-rpath,/x").is_err());
        assert!(validate_cflag("-O2,-g").is_err());
    }

    #[test]
    fn cflag_atfile_rejected() {
        assert!(validate_cflag("@args.txt").is_err());
    }

    #[test]
    fn cflag_linker_escape_rejected() {
        for f in [
            "-Wl,--exclude-libs,ALL",
            "-Wp,-MD",
            "-Xlinker",
            "-Xpreprocessor",
        ] {
            assert!(validate_cflag(f).is_err(), "`{f}` must be rejected");
        }
    }

    #[test]
    fn cflag_bare_include_rejected() {
        assert!(validate_cflag("-Icsrc").is_err());
        assert!(validate_cflag("-I").is_err());
    }

    #[test]
    fn cflag_bare_lib_rejected() {
        assert!(validate_cflag("-lvips").is_err());
        assert!(validate_cflag("-L/usr/lib").is_err());
    }

    #[test]
    fn cflag_target_rejected() {
        assert!(validate_cflag("--target=x86_64-unknown-linux-gnu").is_err());
    }

    #[test]
    fn cflag_unknown_rejected() {
        assert!(validate_cflag("-funroll-loops").is_err());
        assert!(validate_cflag("-march=native").is_err());
    }

    #[test]
    fn cflag_allowlist_accepts() {
        for f in [
            "-O2",
            "-Os",
            "-g",
            "-fPIC",
            "-Wall",
            "-Wextra",
            "-Werror",
            "-Wno-unused",
            "-std=c11",
            "-pthread",
            "-march=x86-64",
        ] {
            assert!(validate_cflag(f).is_ok(), "`{f}` must be allowed");
        }
    }

    #[test]
    fn pkg_config_tokens_validated() {
        // Allowed from resolver output.
        for t in ["-I/usr/include/vips", "-L/usr/lib", "-lvips", "-pthread"] {
            assert!(validate_pkg_config_token(t).is_ok(), "`{t}` allowed");
        }
        // Same rejected classes apply to resolver output.
        for t in ["-Wl,-rpath", "@flags", "-Wp,-v", "-Xlinker"] {
            assert!(
                validate_pkg_config_token(t).is_err(),
                "`{t}` must be rejected"
            );
        }
    }

    #[test]
    fn zz_req_round_trip() {
        let m = Manifest::parse(
            r#"
[package]
name = "needs-new"
version = "1.0.0"
zz = ">=0.1.5"
"#,
        )
        .unwrap();
        assert_eq!(m.package.zz.as_deref(), Some(">=0.1.5"));
        let d = tmp_dir("zz_req");
        let path = d.join("zz.toml");
        m.save(&path).unwrap();
        let loaded = Manifest::load(&path).unwrap();
        assert_eq!(loaded, m);
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn zz_req_absent_by_default() {
        let m = Manifest::parse(
            r#"
[package]
name = "old"
version = "0.1.0"
"#,
        )
        .unwrap();
        assert_eq!(m.package.zz, None);
    }

    #[test]
    fn unknown_fields_ignored() {
        // Documents a protection limit: parsers without the `zz` field
        // (<= 0.1.5) cannot be protected by it — serde ignores unknown
        // keys, so an old compiler reads the manifest fine and fails
        // later (if at all). Enforcement only guards new compilers.
        let m = Manifest::parse(
            r#"
[package]
name = "future"
version = "9.9.9"
zz = ">=9.9.9"
some_future_key = "ignored"
"#,
        )
        .unwrap();
        assert_eq!(m.package.zz.as_deref(), Some(">=9.9.9"));
    }

    #[test]
    fn check_compiler_req_table() {
        assert!(check_compiler_req(">=0.1.5", "0.1.6").is_ok());
        assert!(check_compiler_req(">=0.1.5", "0.1.5").is_ok());
        assert!(check_compiler_req("^0.1.0", "0.1.6").is_ok());
        assert!(check_compiler_req("=0.1.6", "0.1.6").is_ok());
        let err = check_compiler_req(">=0.2.0", "0.1.6").unwrap_err();
        assert!(err.contains("upgrade"), "{err}");
        let err = check_compiler_req("=0.1.5", "0.1.6").unwrap_err();
        assert!(err.contains("0.1.6"), "{err}");
        assert!(check_compiler_req("not-a-req!!!", "0.1.6").is_err());
    }

    #[test]
    fn check_zz_version_none_passes() {
        let m = Manifest::parse(
            r#"
[package]
name = "any"
version = "0.1.0"
"#,
        )
        .unwrap();
        assert!(m.check_zz_version("0.1.0").is_ok());
    }

    #[test]
    fn platform_tags_accepted() {
        for t in [
            "linux-x86_64-gnu-glibc2.28",
            "linux-aarch64-gnu-glibc2.17",
            "linux-x86_64-musl",
            "macos-arm64-min11.0",
            "macos-x86_64-min12.0",
            "windows-x86_64-msvc",
            "windows-aarch64-msvc",
        ] {
            assert!(validate_platform_tag(t).is_ok(), "`{t}` allowed");
        }
    }

    #[test]
    fn platform_tags_rejected() {
        // Bare triples (no libc floor) are gone.
        for t in [
            "x86_64-unknown-linux-gnu",
            "linux-x86_64",
            "macos-arm64",
            "linux-x86_64-gnu",
            "linux-x86_64-gnu-glibc",
            "windows-x86_64",
            "windows-aarch64",
            "windows-aarch64-gnu",
            "freebsd-x86_64-gnu-glibc2.28",
        ] {
            assert!(validate_platform_tag(t).is_err(), "`{t}` must be rejected");
        }
    }

    #[test]
    fn prebuilt_registry_scheme_deferred() {
        let mut spec = cc_spec();
        spec.targets = vec!["linux-x86_64-gnu-glibc2.28".to_string()];
        let mut targets = std::collections::HashMap::new();
        targets.insert(
            "linux-x86_64-gnu-glibc2.28".to_string(),
            PrebuiltArtifact {
                url: "registry:zimg/0.3.0/x.tgz".to_string(),
                sha256: "9f2c9f2c9f2c9f2c9f2c9f2c9f2c9f2c9f2c9f2c9f2c9f2c9f2c9f2c9f2c9f2c"
                    .to_string(),
            },
        );
        let n = NativeSpec {
            backend: Some(NativeBackend::Cc),
            manifest: None,
            build: None,
            build_cc: Some(spec),
            build_rust: None,
            prebuilt: Some(PrebuiltSpec { target: targets }),
            pkg_config: None,
        };
        let err = n.validate(Path::new("/pkg")).unwrap_err();
        assert!(err.contains("https:"), "{err}");
    }

    // ── Collections ───────────────────────────────────────────────────

    fn collection_manifest(spec: &str) -> Manifest {
        Manifest::parse(&format!(
            "[package]\nname = \"c\"\nversion = \"0.1.0\"\n[collect.commands]\n{spec}"
        ))
        .expect("collection manifest parses")
    }

    fn collection_project(tag: &str, files: &[(&str, &str)]) -> PathBuf {
        let d = tmp_dir(tag);
        for (rel, contents) in files {
            let p = d.join(rel);
            fs::create_dir_all(p.parent().unwrap()).unwrap();
            fs::write(p, contents).unwrap();
        }
        fs::create_dir_all(d.join("commands")).unwrap();
        d
    }

    #[test]
    fn collect_parses_defaults() {
        let m = collection_manifest("dir = \"commands\"\nsig = \"func([str]) -> int\"\n");
        let spec = m.collections.get("commands").expect("collection parsed");
        assert_eq!(spec.dir, "commands");
        assert_eq!(spec.name, "file");
        assert_eq!(spec.export, "run");
        assert!(m.collections.contains_key("commands"));
    }

    #[test]
    fn collect_round_trips() {
        let m = collection_manifest(
            "dir = \"commands\"\nsig = \"func(args: [str]) -> int\"\nexport = \"main\"\n",
        );
        let d = tmp_dir("collect_rt");
        m.save(&d.join("zz.toml")).unwrap();
        let loaded = Manifest::load(&d.join("zz.toml")).unwrap();
        assert_eq!(loaded, m);
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn collect_absent_is_empty_and_backcompat() {
        let m = Manifest::parse("[package]\nname = \"old\"\nversion = \"0.1.0\"\n").unwrap();
        assert!(m.collections.is_empty());
    }

    #[test]
    fn collect_validates_dir_and_sig() {
        let d = collection_project("collect_ok", &[]);
        let ok = collection_manifest("dir = \"commands\"\nsig = \"func([str]) -> int\"\n");
        ok.collections["commands"].validate(&d).unwrap();
        // Issue spelling with named parameters also parses.
        let named = collection_manifest("dir = \"commands\"\nsig = \"func(args: [str]) -> int\"\n");
        named.collections["commands"].validate(&d).unwrap();
        let _ = fs::remove_dir_all(&d);

        // Missing dir.
        let d = collection_project("collect_missing", &[]);
        let missing = collection_manifest("dir = \"nope\"\nsig = \"func([str]) -> int\"\n");
        let err = missing.collections["commands"].validate(&d).unwrap_err();
        assert!(err.contains("does not exist"), "{err}");
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn collect_rejects_escaping_paths() {
        let d = collection_project("collect_escape", &[]);
        for dir in ["../outside", "/abs/path", ""] {
            let m =
                collection_manifest(&format!("dir = \"{dir}\"\nsig = \"func([str]) -> int\"\n"));
            let err = m.collections["commands"].validate(&d).unwrap_err();
            assert!(
                err.contains("relative") || err.contains("empty") || err.contains("invalid"),
                "dir `{dir}`: {err}"
            );
        }
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn collect_rejects_bad_signatures() {
        let d = collection_project("collect_badsig", &[]);
        for sig in ["", "int", "func([str] -> int", "func() ->"] {
            let m = collection_manifest(&format!("dir = \"commands\"\nsig = \"{sig}\"\n"));
            let err = m.collections["commands"]
                .validate(&d)
                .expect_err("bad sig must fail");
            assert!(
                err.contains("sig") || err.contains("parse") || err.contains("function"),
                "sig `{sig}`: {err}"
            );
        }
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn collect_rejects_bad_name_mode_and_export() {
        let d = collection_project("collect_badname", &[]);
        let m = collection_manifest(
            "dir = \"commands\"\nsig = \"func([str]) -> int\"\nname = \"numbered\"\n",
        );
        let err = m.collections["commands"].validate(&d).unwrap_err();
        assert!(err.contains("name ="), "{err}");
        let m = collection_manifest(
            "dir = \"commands\"\nsig = \"func([str]) -> int\"\nexport = \"not valid\"\n",
        );
        let err = m.collections["commands"].validate(&d).unwrap_err();
        assert!(err.contains("identifier"), "{err}");
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn collect_discovery_orders_and_skips_non_zz() {
        let d = collection_project(
            "collect_discover",
            &[
                ("commands/zeta.zz", ""),
                ("commands/alpha.zz", ""),
                ("commands/readme.md", ""),
                ("commands/nested/inner.zz", ""),
            ],
        );
        let m = collection_manifest("dir = \"commands\"\nsig = \"func([str]) -> int\"\n");
        let members = m.collections["commands"].discover(&d).unwrap();
        let names: Vec<&str> = members.iter().map(|m| m.name.as_str()).collect();
        assert_eq!(names, ["alpha", "zeta"], "single-level, sorted");
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn collect_empty_dir_is_valid() {
        let d = collection_project("collect_empty", &[]);
        let m = collection_manifest("dir = \"commands\"\nsig = \"func([str]) -> int\"\n");
        assert!(m.collections["commands"].discover(&d).unwrap().is_empty());
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn collect_name_collisions_are_loud() {
        let d = collection_project(
            "collect_collision",
            &[("commands/foo-bar.zz", ""), ("commands/foo_bar.zz", "")],
        );
        let m = collection_manifest("dir = \"commands\"\nsig = \"func([str]) -> int\"\n");
        let err = m.collections["commands"].discover(&d).unwrap_err();
        assert!(err.contains("foo-bar.zz"), "{err}");
        assert!(err.contains("foo_bar.zz"), "{err}");
        assert!(err.contains("both map"), "{err}");
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn collect_invalid_identifier_is_loud() {
        let d = collection_project("collect_badident", &[("commands/9lives.zz", "")]);
        let m = collection_manifest("dir = \"commands\"\nsig = \"func([str]) -> int\"\n");
        let err = m.collections["commands"].discover(&d).unwrap_err();
        assert!(err.contains("valid identifier"), "{err}");
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn collect_identifier_rules() {
        assert!(is_identifier("run"));
        assert!(is_identifier("_private1"));
        assert!(!is_identifier("9lives"));
        assert!(!is_identifier(""));
        assert!(!is_identifier("has-dash"));
    }
}
