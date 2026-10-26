//! Focused Cargo manifest/lockfile model for the hermetic Rust plugin
//! backend (#236).
//!
//! This module deliberately knows only what `zz` needs to make a Rust
//! plugin build auditable:
//!
//! - parse the consumed crate's `Cargo.toml` (`[lib] crate-type` contract),
//! - enumerate the dependency closure from `Cargo.lock`,
//! - reject path deps and accept only checksum-pinned registry deps or
//!   commit-pinned git deps,
//! - content-hash the vendored closure key (lock + manifest + sources),
//! - surface per-crate pins/checksums/licenses for `zz audit`.
//!
//! No network, no cargo invocation, no shell — pure parsing and hashing.

use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::hash;

/// A package entry in `Cargo.lock`.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct CargoLockPackage {
    pub name: String,
    pub version: String,
    /// `registry+https://...` or `git+https://...#<commit>`; absent for
    /// the root package and for path deps.
    #[serde(default)]
    pub source: Option<String>,
    /// Registry package checksum (64 hex chars).
    #[serde(default)]
    pub checksum: Option<String>,
    /// Dependency requirements (`"name"`, `"name version"`, or
    /// `"name version (source)"`).
    #[serde(default)]
    pub dependencies: Vec<String>,
}

/// The subset of `Cargo.lock` we model. Unknown keys (`metadata`, future
/// tables) are ignored so newer cargo lockfile formats keep parsing.
#[derive(Debug, Clone, Deserialize)]
pub struct CargoLock {
    /// Lockfile format version (1..=4 seen in the wild).
    #[serde(default)]
    pub version: u32,
    #[serde(default, rename = "package")]
    pub packages: Vec<CargoLockPackage>,
}

/// Parsed `Cargo.toml` facts the build/audit path needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CrateManifest {
    pub package_name: String,
    pub package_version: String,
    /// `[lib] name` when set, else the package name with `-` → `_`.
    pub lib_name: String,
    /// `[lib] crate-type` entries (lowercased).
    pub crate_types: Vec<String>,
    /// SPDX license expression from `[package] license`.
    pub license: Option<String>,
    /// `[package] license-file` fallback.
    pub license_file: Option<String>,
    /// True when `[package] build = "..."` is set (a custom build script).
    pub custom_build_script: bool,
}

/// A validated lockfile closure: root + non-root dependency packages.
#[derive(Debug, Clone)]
pub struct CargoClosure {
    pub lock: CargoLock,
    pub root: CargoLockPackage,
    pub deps: Vec<CargoLockPackage>,
}

impl CargoClosure {
    /// True when the vendored closure is non-empty (registry/git deps).
    pub fn needs_vendor(&self) -> bool {
        !self.deps.is_empty()
    }
}

/// Read and validate a consumed crate's `Cargo.toml`.
///
/// Contract (v1):
/// - `[lib] crate-type` must contain BOTH `cdylib` (VM dlopen) and
///   `staticlib` (AOT link). Missing either is a loud error naming the
///   exact expected line.
/// - `build.rs` and `[package] build = "..."` are banned: the consumed
///   package may never run package-supplied build code.
pub fn read_crate_manifest(crate_dir: &Path) -> Result<CrateManifest, String> {
    let path = crate_dir.join("Cargo.toml");
    let text = std::fs::read_to_string(&path).map_err(|e| {
        format!(
            "cannot read {}: {e}\n\
             hint: [native.build-rust] crate must contain a Cargo.toml",
            path.display()
        )
    })?;
    parse_crate_manifest(&text).map_err(|e| format!("invalid {}: {e}", path.display()))
}

/// Parse a `Cargo.toml` body (also used for vendored-crate license audit).
pub fn parse_crate_manifest(text: &str) -> Result<CrateManifest, String> {
    let doc: toml::Value = toml::from_str(text).map_err(|e| format!("invalid TOML: {e}"))?;
    let package = doc
        .get("package")
        .and_then(toml::Value::as_table)
        .ok_or_else(|| "missing [package] table".to_string())?;
    let package_name = package
        .get("name")
        .and_then(toml::Value::as_str)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| "missing [package] name".to_string())?
        .to_string();
    let package_version = package
        .get("version")
        .and_then(toml::Value::as_str)
        .unwrap_or("0.0.0")
        .to_string();
    let license = package
        .get("license")
        .and_then(toml::Value::as_str)
        .map(str::to_string)
        .filter(|s| !s.is_empty());
    let license_file = package
        .get("license-file")
        .and_then(toml::Value::as_str)
        .map(str::to_string)
        .filter(|s| !s.is_empty());
    let custom_build_script = package.get("build").is_some();

    let lib = doc.get("lib").and_then(toml::Value::as_table);
    let crate_types: Vec<String> = lib
        .and_then(|l| l.get("crate-type"))
        .and_then(toml::Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(toml::Value::as_str)
                .map(|s| s.to_ascii_lowercase())
                .collect()
        })
        .unwrap_or_default();
    let lib_name = lib
        .and_then(|l| l.get("name"))
        .and_then(toml::Value::as_str)
        .map(str::to_string)
        .unwrap_or_else(|| package_name.replace('-', "_"));

    Ok(CrateManifest {
        package_name,
        package_version,
        lib_name,
        crate_types,
        license,
        license_file,
        custom_build_script,
    })
}

/// Validate the consumed crate directory as a v1 Rust plugin: manifest,
/// both crate types, and no package-supplied build script.
pub fn validate_crate(crate_dir: &Path) -> Result<CrateManifest, String> {
    let manifest = read_crate_manifest(crate_dir)?;
    let has_cdylib = manifest.crate_types.iter().any(|t| t == "cdylib");
    let has_staticlib = manifest.crate_types.iter().any(|t| t == "staticlib");
    if !has_cdylib || !has_staticlib {
        let mut missing = Vec::new();
        if !has_cdylib {
            missing.push("cdylib");
        }
        if !has_staticlib {
            missing.push("staticlib");
        }
        return Err(format!(
            "Rust plugin crate `{}` must build both engines: [lib] crate-type is missing {}\n\
             hint: add this exact line to {}:\n  crate-type = [\"cdylib\", \"staticlib\"]",
            crate_dir.display(),
            missing.join(" and "),
            crate_dir.join("Cargo.toml").display()
        ));
    }
    let build_rs = crate_dir.join("build.rs");
    if build_rs.exists() {
        return Err(format!(
            "Rust plugin crate `{}` contains build.rs, which is banned in v1\n\
             hint: move codegen into committed sources; [native.build-rust] never runs package build scripts",
            crate_dir.display()
        ));
    }
    if manifest.custom_build_script {
        return Err(format!(
            "[package] build is banned in Rust plugin crate `{}`\n\
             hint: [native.build-rust] never runs package build scripts",
            crate_dir.display()
        ));
    }
    Ok(manifest)
}

/// Load and validate `Cargo.lock` for a crate already checked by
/// [`validate_crate`].
///
/// Fail-closed rules:
/// - the crate itself must be the lockfile root (name + version, no source),
/// - every non-root package must be pinned (registry checksum or git commit),
/// - path deps (source-less non-root packages) are rejected,
/// - every dependency reference must resolve to a locked package.
pub fn read_closure(crate_dir: &Path, manifest: &CrateManifest) -> Result<CargoClosure, String> {
    let path = crate_dir.join("Cargo.lock");
    let text = std::fs::read_to_string(&path).map_err(|e| {
        format!(
            "cannot read {}: {e}\n\
             hint: commit Cargo.lock for [native.build-rust] (locked builds are required)",
            path.display()
        )
    })?;
    parse_closure(&text, manifest).map_err(|e| format!("invalid {}: {e}", path.display()))
}

/// Parse a `Cargo.lock` body against a crate manifest.
pub fn parse_closure(text: &str, manifest: &CrateManifest) -> Result<CargoClosure, String> {
    let lock: CargoLock = toml::from_str(text).map_err(|e| format!("invalid TOML: {e}"))?;
    if lock.version > 4 {
        return Err(format!(
            "unsupported Cargo.lock version {} (expected 1..=4)",
            lock.version
        ));
    }
    if lock.packages.is_empty() {
        return Err("Cargo.lock has no [[package]] entries".to_string());
    }

    // Root: same name+version, no source. Reject multiple candidates
    // (a workspace lockfile with several path members is not vendorable
    // in v1 and would make the consumed crate ambiguous).
    let roots: Vec<&CargoLockPackage> = lock
        .packages
        .iter()
        .filter(|p| {
            p.source.is_none()
                && p.name == manifest.package_name
                && p.version == manifest.package_version
        })
        .collect();
    let root = match roots.as_slice() {
        [one] => (*one).clone(),
        [] => {
            return Err(format!(
                "Cargo.lock has no root package `{}@{}` (is the lock committed and in sync with Cargo.toml?)",
                manifest.package_name, manifest.package_version
            ));
        }
        _ => {
            return Err(format!(
                "Cargo.lock has multiple source-less packages named `{}@{}`; workspace/path members are not vendorable in v1",
                manifest.package_name, manifest.package_version
            ));
        }
    };

    let mut deps = Vec::new();
    for p in &lock.packages {
        if p.name == root.name && p.version == root.version && p.source.is_none() {
            continue;
        }
        if p.source.is_none() {
            return Err(format!(
                "path dependency `{}@{}` in Cargo.lock is not allowed in a vendored Rust closure\n\
                 hint: use a registry dependency (checksum-pinned) or a commit-pinned git dependency",
                p.name, p.version
            ));
        }
        check_pin(p)?;
        deps.push(p.clone());
    }

    check_dependency_refs(&root, &deps)?;
    deps.sort_by(|a, b| {
        a.name
            .cmp(&b.name)
            .then_with(|| a.version.cmp(&b.version))
            .then_with(|| a.source.cmp(&b.source))
    });
    Ok(CargoClosure { lock, root, deps })
}

/// A dependency is auditable only when its bytes are pinned:
/// `registry+...` needs a 64-hex checksum; `git+...#<commit>` needs a
/// 40/64-hex commit fragment.
fn check_pin(p: &CargoLockPackage) -> Result<(), String> {
    let source = p.source.as_deref().unwrap_or_default();
    if source.starts_with("registry+") {
        match p.checksum.as_deref() {
            Some(c) if c.len() == 64 && c.chars().all(|ch| ch.is_ascii_hexdigit()) => Ok(()),
            _ => Err(format!(
                "registry dependency `{}@{}` has no valid sha256 checksum in Cargo.lock\n\
                 hint: commit the lockfile produced by cargo; hashless entries are audit failures",
                p.name, p.version
            )),
        }
    } else if source.starts_with("git+") {
        let pinned = source
            .rsplit_once('#')
            .map(|(_, rev)| {
                (rev.len() == 40 || rev.len() == 64) && rev.chars().all(|ch| ch.is_ascii_hexdigit())
            })
            .unwrap_or(false);
        if pinned {
            Ok(())
        } else {
            Err(format!(
                "git dependency `{}@{}` is not pinned by commit hash in Cargo.lock\n\
                 hint: lock it (`cargo update -p {} --precise <rev>`) so the vendored closure is immutable",
                p.name, p.version, p.name
            ))
        }
    } else {
        Err(format!(
            "dependency `{}@{}` uses unsupported source `{source}`\n\
             hint: only registry+ (checksummed) and commit-pinned git+ sources are vendorable",
            p.name, p.version
        ))
    }
}

/// Every dependency string must resolve to exactly one locked package.
fn check_dependency_refs(root: &CargoLockPackage, deps: &[CargoLockPackage]) -> Result<(), String> {
    let mut by_name: BTreeMap<&str, Vec<&CargoLockPackage>> = BTreeMap::new();
    for p in std::iter::once(root).chain(deps.iter()) {
        by_name.entry(&p.name).or_default().push(p);
    }
    for p in std::iter::once(root).chain(deps.iter()) {
        for dep in &p.dependencies {
            let (name, version) = parse_dep_ref(dep);
            let Some(candidates) = by_name.get(name) else {
                return Err(format!(
                    "Cargo.lock package `{}@{}` depends on `{dep}`, which is not locked",
                    p.name, p.version
                ));
            };
            match version {
                Some(v) if !candidates.iter().any(|c| c.version == v) => {
                    return Err(format!(
                        "Cargo.lock package `{}@{}` depends on `{name}@{v}`, which is not locked",
                        p.name, p.version
                    ));
                }
                None if candidates.len() > 1 => {
                    return Err(format!(
                        "ambiguous lockfile dependency `{dep}` from `{}@{}` (multiple `{name}` versions)",
                        p.name, p.version
                    ));
                }
                _ => {}
            }
        }
    }
    Ok(())
}

/// Split a `Cargo.lock` dependency entry into `(name, version?)`.
fn parse_dep_ref(dep: &str) -> (&str, Option<&str>) {
    let mut parts = dep.split_whitespace();
    let name = parts.next().unwrap_or(dep);
    let version = parts
        .next()
        .filter(|v| v.chars().next().is_some_and(|c| c.is_ascii_digit()));
    (name, version)
}

/// Content hash for the vendored closure key: `Cargo.toml` + `Cargo.lock`
/// + every committed crate source file, plus the `[native.build-rust]`
///   spec fingerprint supplied by the caller.
///
/// Deterministic across machines: sorted slash-normalized relative paths,
/// no mtimes, build/target/.git excluded.
pub fn vendor_key(crate_dir: &Path, spec_fingerprint: &str) -> Result<String, String> {
    let mut files: Vec<(String, Vec<u8>)> = Vec::new();
    collect_source_files(crate_dir, crate_dir, &mut files, 0)?;
    files.sort_by(|a, b| a.0.cmp(&b.0));
    let mut buf: Vec<u8> = Vec::new();
    buf.extend_from_slice(b"zz-rust-vendor-v1\0");
    buf.extend_from_slice(spec_fingerprint.as_bytes());
    buf.push(0);
    for (rel, bytes) in &files {
        buf.extend_from_slice(rel.as_bytes());
        buf.push(0);
        buf.extend_from_slice(bytes);
        buf.push(0);
    }
    Ok(hash::hash_bytes(&buf))
}

fn collect_source_files(
    root: &Path,
    dir: &Path,
    out: &mut Vec<(String, Vec<u8>)>,
    depth: usize,
) -> Result<(), String> {
    if depth > 32 {
        return Err(format!(
            "Rust crate source tree too deep under {}",
            root.display()
        ));
    }
    let entries = std::fs::read_dir(dir)
        .map_err(|e| format!("cannot read crate dir {}: {e}", dir.display()))?;
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().into_owned();
        if path.is_dir() {
            if matches!(
                name.as_str(),
                "target" | "build" | ".git" | "node_modules" | "vendor"
            ) || name.starts_with('.')
            {
                continue;
            }
            collect_source_files(root, &path, out, depth + 1)?;
        } else if path.is_file() {
            // Build scripts are rejected by validation, but exclude them
            // from the key too so a stray file cannot silently alter it.
            if name == "build.rs" {
                continue;
            }
            let rel = path
                .strip_prefix(root)
                .unwrap_or(&path)
                .components()
                .map(|c| c.as_os_str().to_string_lossy().into_owned())
                .collect::<Vec<_>>()
                .join("/");
            let bytes =
                std::fs::read(&path).map_err(|e| format!("cannot read {}: {e}", path.display()))?;
            out.push((rel, bytes));
        }
    }
    Ok(())
}

/// One audited crate from the vendored closure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VendoredCrate {
    pub name: String,
    pub version: String,
    /// Registry URL/root or git URL+commit; `local` for the root crate.
    pub source: String,
    /// Registry sha256 or git commit hash.
    pub checksum: Option<String>,
    /// SPDX expression or `license-file` fallback.
    pub license: Option<String>,
    /// True when the entry is immutable (checksum or commit pin).
    pub pinned: bool,
}

/// Result of auditing a vendored Rust closure.
#[derive(Debug, Clone, Default)]
pub struct RustClosureAudit {
    pub crates: Vec<VendoredCrate>,
    pub errors: Vec<String>,
    pub warnings: Vec<String>,
}

/// Audit the vendored closure: pins, hashes, and licenses.
///
/// `vendor_dir` is the CAS entry holding `cargo vendor` output (or `None`
/// when the closure is empty). Missing content is a warning here; the
/// caller turns unpinned/hashless/license-less entries into failures so
/// `zz audit` reports them exactly like registry pins.
pub fn audit_closure(closure: &CargoClosure, vendor_dir: Option<&Path>) -> RustClosureAudit {
    let mut audit = RustClosureAudit::default();
    if !closure.needs_vendor() {
        return audit;
    }
    let Some(vendor_dir) = vendor_dir else {
        audit
            .warnings
            .push("vendored Rust closure is not present locally (run `zz install`)".to_string());
        return audit;
    };
    if !vendor_dir.exists() {
        audit.warnings.push(format!(
            "vendored Rust closure missing at {} (run `zz install`)",
            vendor_dir.display()
        ));
        return audit;
    }

    // Index vendored crate manifests by (name, version). Directory names
    // are cargo-vendor implementation detail (`crate` or `crate-version`),
    // so parse the manifests instead of guessing paths.
    let mut licenses: BTreeMap<(String, String), (Option<String>, Option<String>)> =
        BTreeMap::new();
    if let Ok(entries) = std::fs::read_dir(vendor_dir) {
        for entry in entries.flatten() {
            let manifest_path = entry.path().join("Cargo.toml");
            let Ok(text) = std::fs::read_to_string(&manifest_path) else {
                continue;
            };
            if let Ok(m) = parse_crate_manifest(&text) {
                licenses.insert(
                    (m.package_name, m.package_version),
                    (m.license, m.license_file),
                );
            }
        }
    }

    for p in &closure.deps {
        let pinned = check_pin(p).is_ok();
        if !pinned {
            audit.errors.push(format!(
                "{}@{}: unpinned/hashless Cargo.lock entry",
                p.name, p.version
            ));
        }
        let (license, license_file) = licenses
            .get(&(p.name.clone(), p.version.clone()))
            .cloned()
            .unwrap_or((None, None));
        let license = license.or_else(|| license_file.map(|f| format!("file:{f}")));
        if license.is_none() {
            audit.errors.push(format!(
                "{}@{}: no license metadata in vendored Cargo.toml",
                p.name, p.version
            ));
        }
        audit.crates.push(VendoredCrate {
            name: p.name.clone(),
            version: p.version.clone(),
            source: p.source.clone().unwrap_or_default(),
            checksum: p
                .checksum
                .clone()
                .or_else(|| git_commit(p.source.as_deref().unwrap_or_default())),
            license,
            pinned,
        });
    }
    if licenses.is_empty() {
        audit.warnings.push(
            "vendored Rust closure has no readable crate manifests (run `zz install`)".to_string(),
        );
    }
    audit
}

fn git_commit(source: &str) -> Option<String> {
    source
        .starts_with("git+")
        .then(|| source.rsplit_once('#').map(|(_, rev)| rev.to_string()))
        .flatten()
        .filter(|rev| !rev.is_empty())
}

/// Convenience: validate everything for a crate and return its closure.
pub fn load_validated(crate_dir: &Path) -> Result<(CrateManifest, CargoClosure), String> {
    let manifest = validate_crate(crate_dir)?;
    let closure = read_closure(crate_dir, &manifest)?;
    Ok((manifest, closure))
}

/// Resolve a crate-relative path, rejecting escapes (defensive; the
/// manifest validator already checks containment).
pub fn crate_dir_for(pkg_root: &Path, rel: &str) -> Result<PathBuf, String> {
    if rel.is_empty() {
        return Err("[native.build-rust] `crate` must not be empty".to_string());
    }
    let p = Path::new(rel);
    if p.is_absolute()
        || p.components().any(|c| {
            matches!(
                c,
                std::path::Component::ParentDir | std::path::Component::Prefix(_)
            )
        })
    {
        return Err(format!(
            "[native.build-rust] crate `{rel}` must be a relative directory inside the package"
        ));
    }
    Ok(pkg_root.join(p))
}

/// Recursively copy a directory tree, skipping build outputs and hidden
/// VCS metadata. Used to stage a consumed crate into `build/` so cargo
/// never sees a parent workspace or a read-only CAS tree.
pub fn copy_crate_tree(src: &Path, dst: &Path) -> Result<(), String> {
    std::fs::create_dir_all(dst).map_err(|e| format!("cannot create {}: {e}", dst.display()))?;
    let entries =
        std::fs::read_dir(src).map_err(|e| format!("cannot read {}: {e}", src.display()))?;
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().into_owned();
        let dest = dst.join(&name);
        if path.is_dir() {
            if matches!(
                name.as_str(),
                "target" | "build" | ".git" | "node_modules" | "vendor"
            ) || name.starts_with('.')
            {
                continue;
            }
            copy_crate_tree(&path, &dest)?;
        } else if path.is_file() {
            std::fs::copy(&path, &dest)
                .map_err(|e| format!("cannot copy {}: {e}", path.display()))?;
        }
    }
    Ok(())
}

/// True when `text` already declares a `[workspace]` table.
pub fn has_workspace_table(cargo_toml: &str) -> bool {
    cargo_toml
        .parse::<toml::Value>()
        .ok()
        .and_then(|v| v.get("workspace").cloned())
        .is_some()
}

/// Make a staged crate standalone: append an empty `[workspace]` table
/// when absent so cargo cannot adopt an enclosing workspace.
pub fn detach_workspace(staged_cargo_toml: &Path) -> Result<(), String> {
    let text = std::fs::read_to_string(staged_cargo_toml)
        .map_err(|e| format!("cannot read {}: {e}", staged_cargo_toml.display()))?;
    if has_workspace_table(&text) {
        return Ok(());
    }
    let mut out = text;
    if !out.ends_with('\n') {
        out.push('\n');
    }
    out.push_str("\n[workspace]\n");
    std::fs::write(staged_cargo_toml, out)
        .map_err(|e| format!("cannot write {}: {e}", staged_cargo_toml.display()))
}

/// Directory names in a `cargo vendor` output, for tests/debugging.
pub fn vendored_crate_dirs(vendor_dir: &Path) -> Result<HashSet<String>, String> {
    let entries = std::fs::read_dir(vendor_dir)
        .map_err(|e| format!("cannot read {}: {e}", vendor_dir.display()))?;
    Ok(entries
        .flatten()
        .filter(|e| e.path().is_dir())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!(
            "zz_rust_lock_{name}_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn crate_manifest() -> CrateManifest {
        CrateManifest {
            package_name: "fnv_native".to_string(),
            package_version: "0.1.0".to_string(),
            lib_name: "fnv_native".to_string(),
            crate_types: vec!["cdylib".to_string(), "staticlib".to_string()],
            license: Some("MIT".to_string()),
            license_file: None,
            custom_build_script: false,
        }
    }

    const REGISTRY_LOCK: &str = r#"
version = 3

[[package]]
name = "fnv_native"
version = "0.1.0"
dependencies = [
 "libc",
]

[[package]]
name = "libc"
version = "0.2.155"
source = "registry+https://github.com/rust-lang/crates.io-index"
checksum = "97b3888a4aec0b01a8a6d1a4c7bead3d6b6c4d1c2c3a4b5c6d7e8f9012345678"
"#;

    #[test]
    fn registry_closure_accepted() {
        let closure = parse_closure(REGISTRY_LOCK, &crate_manifest()).unwrap();
        assert_eq!(closure.deps.len(), 1);
        assert_eq!(closure.deps[0].name, "libc");
        assert!(closure.needs_vendor());
    }

    #[test]
    fn root_only_closure_needs_no_vendor() {
        let lock = r#"
version = 3
[[package]]
name = "fnv_native"
version = "0.1.0"
"#;
        let closure = parse_closure(lock, &crate_manifest()).unwrap();
        assert!(closure.deps.is_empty());
        assert!(!closure.needs_vendor());
    }

    #[test]
    fn path_dep_rejected() {
        let lock = r#"
version = 3
[[package]]
name = "fnv_native"
version = "0.1.0"
dependencies = ["helper"]

[[package]]
name = "helper"
version = "0.1.0"
"#;
        let err = parse_closure(lock, &crate_manifest()).unwrap_err();
        assert!(err.contains("path dependency"), "{err}");
    }

    #[test]
    fn hashless_registry_dep_rejected() {
        let lock = REGISTRY_LOCK.replace(
            "checksum = \"97b3888a4aec0b01a8a6d1a4c7bead3d6b6c4d1c2c3a4b5c6d7e8f9012345678\"",
            "",
        );
        let err = parse_closure(&lock, &crate_manifest()).unwrap_err();
        assert!(err.contains("checksum"), "{err}");
    }

    #[test]
    fn git_dep_accepted_when_commit_pinned() {
        let lock = r#"
version = 3
[[package]]
name = "fnv_native"
version = "0.1.0"
dependencies = ["gitdep"]

[[package]]
name = "gitdep"
version = "0.1.0"
source = "git+https://example.com/gitdep?rev=main#0123456789abcdef0123456789abcdef01234567"
"#;
        let closure = parse_closure(lock, &crate_manifest()).unwrap();
        assert_eq!(closure.deps.len(), 1);
    }

    #[test]
    fn git_dep_without_commit_rejected() {
        let lock = r#"
version = 3
[[package]]
name = "fnv_native"
version = "0.1.0"
dependencies = ["gitdep"]

[[package]]
name = "gitdep"
version = "0.1.0"
source = "git+https://example.com/gitdep?rev=main"
"#;
        let err = parse_closure(lock, &crate_manifest()).unwrap_err();
        assert!(err.contains("pinned by commit"), "{err}");
    }

    #[test]
    fn missing_root_rejected() {
        let lock = r#"
version = 3
[[package]]
name = "other"
version = "0.1.0"
"#;
        let err = parse_closure(lock, &crate_manifest()).unwrap_err();
        assert!(err.contains("no root package"), "{err}");
    }

    #[test]
    fn unresolved_dependency_rejected() {
        let lock = r#"
version = 3
[[package]]
name = "fnv_native"
version = "0.1.0"
dependencies = ["ghost"]
"#;
        let err = parse_closure(lock, &crate_manifest()).unwrap_err();
        assert!(err.contains("not locked"), "{err}");
    }

    #[test]
    fn crate_manifest_parses_both_engines() {
        let m = parse_crate_manifest(
            r#"
[package]
name = "my-plugin"
version = "1.2.3"
license = "MIT OR Apache-2.0"

[lib]
name = "my_plugin"
crate-type = ["cdylib", "staticlib"]
"#,
        )
        .unwrap();
        assert_eq!(m.lib_name, "my_plugin");
        assert_eq!(m.license.as_deref(), Some("MIT OR Apache-2.0"));
        assert!(m.crate_types.contains(&"cdylib".to_string()));
        assert!(m.crate_types.contains(&"staticlib".to_string()));
    }

    #[test]
    fn crate_manifest_license_file_fallback() {
        let m = parse_crate_manifest(
            "[package]\nname=\"x\"\nversion=\"0.1.0\"\nlicense-file=\"COPYING\"\n",
        )
        .unwrap();
        assert_eq!(m.license, None);
        assert_eq!(m.license_file.as_deref(), Some("COPYING"));
    }

    #[test]
    fn validate_crate_rejects_missing_crate_type() {
        let d = tmp("missing_crate_type");
        std::fs::write(
            d.join("Cargo.toml"),
            "[package]\nname=\"x\"\nversion=\"0.1.0\"\n\n[lib]\ncrate-type=[\"cdylib\"]\n",
        )
        .unwrap();
        let err = validate_crate(&d).unwrap_err();
        assert!(err.contains("staticlib"), "{err}");
        assert!(
            err.contains("crate-type = [\"cdylib\", \"staticlib\"]"),
            "{err}"
        );
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn validate_crate_rejects_consumed_build_rs() {
        let d = tmp("build_rs");
        std::fs::write(
            d.join("Cargo.toml"),
            "[package]\nname=\"x\"\nversion=\"0.1.0\"\n\n[lib]\ncrate-type=[\"cdylib\",\"staticlib\"]\n",
        )
        .unwrap();
        std::fs::write(d.join("build.rs"), "fn main() {}\n").unwrap();
        let err = validate_crate(&d).unwrap_err();
        assert!(err.contains("build.rs") && err.contains("banned"), "{err}");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn validate_crate_rejects_custom_build_entry() {
        let d = tmp("custom_build");
        std::fs::write(
            d.join("Cargo.toml"),
            "[package]\nname=\"x\"\nversion=\"0.1.0\"\nbuild=\"codegen.rs\"\n\n[lib]\ncrate-type=[\"cdylib\",\"staticlib\"]\n",
        )
        .unwrap();
        let err = validate_crate(&d).unwrap_err();
        assert!(err.contains("[package] build"), "{err}");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn missing_lock_is_loud() {
        let d = tmp("missing_lock");
        std::fs::write(
            d.join("Cargo.toml"),
            "[package]\nname=\"x\"\nversion=\"0.1.0\"\n\n[lib]\ncrate-type=[\"cdylib\",\"staticlib\"]\n",
        )
        .unwrap();
        let manifest = validate_crate(&d).unwrap();
        let err = read_closure(&d, &manifest).unwrap_err();
        assert!(err.contains("Cargo.lock"), "{err}");
        assert!(err.contains("commit"), "{err}");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn vendor_key_is_deterministic_and_source_sensitive() {
        let d = tmp("vendor_key");
        std::fs::create_dir_all(d.join("src")).unwrap();
        std::fs::write(
            d.join("Cargo.toml"),
            "[package]\nname=\"x\"\nversion=\"0.1.0\"\n\n[lib]\ncrate-type=[\"cdylib\",\"staticlib\"]\n",
        )
        .unwrap();
        std::fs::write(
            d.join("Cargo.lock"),
            "version = 3\n[[package]]\nname=\"x\"\nversion=\"0.1.0\"\n",
        )
        .unwrap();
        std::fs::write(d.join("src/lib.rs"), "pub fn a() {}\n").unwrap();
        let k1 = vendor_key(&d, "spec").unwrap();
        let k2 = vendor_key(&d, "spec").unwrap();
        assert_eq!(k1, k2);
        std::fs::write(d.join("src/lib.rs"), "pub fn b() {}\n").unwrap();
        assert_ne!(k1, vendor_key(&d, "spec").unwrap());
        assert_ne!(k1, vendor_key(&d, "other-spec").unwrap());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn audit_reports_pins_hashes_and_licenses() {
        let d = tmp("audit_vendor");
        let dep = d.join("libc-0.2.155");
        std::fs::create_dir_all(&dep).unwrap();
        std::fs::write(
            dep.join("Cargo.toml"),
            "[package]\nname=\"libc\"\nversion=\"0.2.155\"\nlicense=\"MIT\"\n",
        )
        .unwrap();
        let closure = parse_closure(REGISTRY_LOCK, &crate_manifest()).unwrap();
        let audit = audit_closure(&closure, Some(&d));
        assert!(audit.errors.is_empty(), "{:?}", audit.errors);
        assert_eq!(audit.crates.len(), 1);
        assert_eq!(audit.crates[0].license.as_deref(), Some("MIT"));
        assert_eq!(audit.crates[0].checksum.as_deref().map(str::len), Some(64));
        assert!(audit.crates[0].pinned);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn audit_flags_missing_license() {
        let d = tmp("audit_nolicense");
        let dep = d.join("libc");
        std::fs::create_dir_all(&dep).unwrap();
        std::fs::write(
            dep.join("Cargo.toml"),
            "[package]\nname=\"libc\"\nversion=\"0.2.155\"\n",
        )
        .unwrap();
        let closure = parse_closure(REGISTRY_LOCK, &crate_manifest()).unwrap();
        let audit = audit_closure(&closure, Some(&d));
        assert!(
            audit.errors.iter().any(|e| e.contains("no license")),
            "{:?}",
            audit.errors
        );
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn copy_crate_tree_skips_build_outputs() {
        let d = tmp("copy_tree");
        std::fs::create_dir_all(d.join("src")).unwrap();
        std::fs::create_dir_all(d.join("target/debug")).unwrap();
        std::fs::write(d.join("Cargo.toml"), "[package]\n").unwrap();
        std::fs::write(d.join("src/lib.rs"), "x").unwrap();
        std::fs::write(d.join("target/debug/junk"), "junk").unwrap();
        let dst = std::env::temp_dir().join(format!(
            "zz_rust_lock_copy_tree_out_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = std::fs::remove_dir_all(&dst);
        copy_crate_tree(&d, &dst).unwrap();
        assert!(dst.join("Cargo.toml").exists());
        assert!(dst.join("src/lib.rs").exists());
        assert!(!dst.join("target").exists());
        let _ = std::fs::remove_dir_all(&d);
        let _ = std::fs::remove_dir_all(&dst);
    }

    #[test]
    fn detach_workspace_appends_once() {
        let d = tmp("detach_ws");
        let p = d.join("Cargo.toml");
        std::fs::write(&p, "[package]\nname=\"x\"\n").unwrap();
        detach_workspace(&p).unwrap();
        detach_workspace(&p).unwrap();
        let text = std::fs::read_to_string(&p).unwrap();
        assert_eq!(text.matches("[workspace]").count(), 1);
        let _ = std::fs::remove_dir_all(&d);
    }
}
