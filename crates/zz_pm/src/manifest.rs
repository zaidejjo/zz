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
//! ```
//!
//! The `authors`, `description`, `license`, and `repository` keys are
//! optional: manifests written before they existed still parse, and
//! `save()` omits them while empty so diffs stay minimal.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

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
            },
            dependencies: HashMap::new(),
            native: None,
        }
    }
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
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum NativeBackend {
    /// Declarative C build (default when `[native.build-cc]` is present).
    Cc,
    /// Legacy shell hook (default when `build = "..."` is present).
    #[default]
    Hook,
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
        if self.build_cc.is_some() {
            return NativeBackend::Cc;
        }
        NativeBackend::Hook
    }

    /// Validate all declarative fields against `pkg_root`.
    ///
    /// Checks: mixed-form rejection, backend/shape agreement, path
    /// containment (hook, manifest, sources, include_dirs), explicit
    /// cflag allowlist + rejected classes, `libs` shape, platform tags,
    /// prebuilt URL/sha shape, legacy/new `pkg_config` exclusivity.
    /// Returns `Ok(())` for a legacy hook-only spec (nothing to check
    /// beyond containment of the hook path).
    pub fn validate(&self, pkg_root: &Path) -> Result<(), String> {
        // Mixed form is always an error.
        if self.build.is_some() && self.build_cc.is_some() {
            return Err(
                "invalid [native]: `build` (hook) and `[native.build-cc]` are mutually exclusive\n\
                 hint: remove `build = \"build.sh\"` to use the declarative backend"
                    .to_string(),
            );
        }
        if self.pkg_config.is_some()
            && self
                .build_cc
                .as_ref()
                .is_some_and(|b| !b.pkg_config.is_empty())
        {
            return Err("invalid [native]: legacy `pkg_config` (string) and `[native.build-cc] pkg_config` (list) are mutually exclusive\n\
                 hint: keep only the `[native.build-cc]` list form"
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
                NativeBackend::Hook if self.build.is_none() => {
                    return Err(
                        "invalid [native]: `backend = \"hook\"` requires `build = \"...\"`\n\
                         hint: add `build = \"build.sh\"` or switch to `backend = \"cc\"`"
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

/// Reject paths escaping the package root (`..`, absolute, empty).
fn check_contained(pkg_root: &Path, rel: &str, field: &str) -> Result<(), String> {
    if rel.is_empty() {
        return Err(format!("invalid [native]: `{field}` must not be empty"));
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
/// `windows-x86_64-msvc`. Anything else is a manifest error.
pub fn validate_platform_tag(tag: &str) -> Result<(), String> {
    fn bad(tag: &str) -> String {
        format!(
            "invalid platform tag `{tag}`\n\
             hint: e.g. linux-x86_64-gnu-glibc2.28, linux-x86_64-musl, macos-arm64-min11.0, windows-x86_64-msvc"
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
        ["windows", "x86_64", "msvc"] => Ok(()),
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
    /// Simple version range: `"^1.2.0"`.
    Version(String),
    /// Git dependency with version constraint.
    Git(GitDep),
    /// Path dependency (local filesystem).
    // TODO(workspace): cross-source version conflicts (same package name available
    // as both a registry version and a path dep, e.g. once [workspace] ships)
    // are currently undefined behavior — not resolved now, just flagged so it
    // isn't silently assumed away later.
    Path(PathDep),
}

/// Git dependency spec.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct GitDep {
    pub version: String,
    pub git: String,
    pub rev: String,
}

/// Path dependency spec.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PathDep {
    pub path: String,
}

impl Manifest {
    /// Load a manifest from a `zz.toml` file.
    pub fn load(path: &Path) -> Result<Self, String> {
        let content = std::fs::read_to_string(path)
            .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
        Self::parse(&content)
    }

    /// Parse a manifest from a string.
    pub fn parse(s: &str) -> Result<Self, String> {
        toml::from_str(s).map_err(|e| format!("invalid zz.toml: {e}"))
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
            },
            dependencies: HashMap::new(),
            native: None,
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

        // Write starter source
        let main_content = match template {
            Some("lib") => {
                "/// Add one to a number.\npub func add_one(n: int) -> int {\n    n + 1\n}\n"
            }
            Some("web") => {
                "import std.http\n\nfunc main() {\n    s := http.server()\n    s2 := http.route_get(s, \"/\", |req| \"Hello, ZZ!\")\n    http.listen(s2, 8080) ?? println(\"failed to start server\")\n}\n"
            }
            _ => {
                "import std.http\n\nfunc main() {\n    println(\"Hello, ZZ!\")\n}\n"
            }
        };
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
            },
            dependencies: {
                let mut d = HashMap::new();
                d.insert("foo".into(), DepSpec::Version("^1.2.0".into()));
                d.insert(
                    "bar".into(),
                    DepSpec::Git(GitDep {
                        version: "2.0".into(),
                        git: "https://github.com/user/repo".into(),
                        rev: "main".into(),
                    }),
                );
                d.insert(
                    "baz".into(),
                    DepSpec::Path(PathDep {
                        path: "../baz".into(),
                    }),
                );
                d
            },
            native: None,
        };

        let d = tmp_dir("round_trip");
        let path = d.join("zz.toml");
        m.save(&path).unwrap();
        let loaded = Manifest::load(&path).unwrap();
        assert_eq!(m, loaded);
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

    #[test]
    fn create_new_cli_template() {
        let d = tmp_dir("new_cli");
        let project = Manifest::create_new(&d, "myapp", None).unwrap();
        assert!(project.join("zz.toml").exists());
        assert!(project.join("src/main.zz").exists());
        let src = fs::read_to_string(project.join("src/main.zz")).unwrap();
        assert!(src.contains("Hello, ZZ!"));
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
            },
            dependencies: HashMap::new(),
            native: None,
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
            prebuilt: Some(PrebuiltSpec { target: targets }),
            pkg_config: None,
        };
        let err = n.validate(Path::new("/pkg")).unwrap_err();
        assert!(err.contains("https:"), "{err}");
    }
}
