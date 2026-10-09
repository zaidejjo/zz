//! Native AOT build / transient-run integration for the `zz` CLI.
//!
//! Single-backend model (Clang-only), always a native binary:
//! `zz build <file>`            — debug: native Clang `-O0 -g` (fast, no LTO).
//! `zz build -p <file>`         — release: native Clang `-O3 -flto=thin`.
//! `zz build --static`          — static self-contained (ThinLTO, DCE).
//! `zz build --pgo`             — PGO instrumented build (native host only).
//! `zz build --target <triple>` — cross build via `clang --target=`
//!                                (same flags, minus `-march=native`).
//! `zz run --native`            — transient release compile → exec → cleanup.
//!
//! (`zz run` without `--native` is the only VM path.)
//!
//! All artifacts live under `bin/` next to the source file. Release binaries
//! are cached under `~/.zz/cache` keyed by source-hash + build options +
//! target triple, so unchanged files rebuild instantaneously.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};

use zz_codegen::{BuildOptions, ClangProvider};
use zz_frontend::span::Span;
use zz_hir::TypedProgram;
use zz_plugin::load_manifest;

use crate::loader;

/// Build mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BuildMode {
    Dev,
    Release,
    Static,
    Pgo,
    /// PGO phase 2: optimize with collected profile data (`zz profile`).
    /// Same flags as the instrumented build, plus `-fprofile-use`.
    PgoUse,
    /// Max optimization (`zz build --release --full`): release base with
    /// full LTO (deeper cross-TU optimization/elimination than ThinLTO).
    Full,
    /// Max optimization with PGO (`--full -- <train args>`): profile-use
    /// plus full LTO. Instrumented leg reuses plain `Pgo` (same profile
    /// quality, faster instrumented build).
    FullPgo,
}

/// Release-build knobs: cross target, provider selection, verbosity.
#[derive(Debug, Clone, Default)]
pub struct ReleaseOptions {
    /// `--embed=<dir>` static asset directory baked into the binary and
    /// served at runtime through `fs.embedfs()`. `None` = no embedding.
    pub embed: Option<PathBuf>,
    /// `--target=<triple>` cross triple, or `None` for a native host build.
    pub target: Option<String>,
    /// `--cc=` provider preference (clang vs `zig cc`).
    pub provider: ClangProvider,
    /// `--verbose`: print the exact clang command line.
    pub verbose: bool,
    /// `--allow-source-builds`: permit compiling transitive `[native]`
    /// deps from source when no prebuilt covers the host tag.
    pub allow_source_builds: bool,
    /// `--allow-hooks`: permit legacy `build = "..."` hooks (direct
    /// deps only; transitive hooks always error).
    pub allow_hooks: bool,
    /// Internal: static came from the `zz build` default (not an explicit
    /// `--static`), so impossible-static falls back to dynamic with a
    /// note instead of erroring. Set by `build_cmd`; everything else
    /// leaves the default `false` (strict).
    pub allow_static_downgrade: bool,
    /// `-o <name>`: publish the binary under this name instead of
    /// `bin/<stem>`. A bare file name stays inside `bin/`; a value with
    /// a path separator is used as-is relative to the current
    /// directory (go-like). Excluded from the cache key: the same
    /// cached binary is published under any name.
    pub output: Option<PathBuf>,
    /// `--chunk`: lower from the unified IR chunk instead of HIR.
    /// Dual-codegen gate: both paths must agree until full coverage.
    /// Carried into the cache filename so backends never share entries.
    pub chunk: bool,
}

impl ReleaseOptions {
    /// Target as `Option<&str>` for the codegen API.
    pub fn target_opt(&self) -> Option<&str> {
        self.target.as_deref()
    }
}

/// Walk an `--embed` directory into `(virtual-path, bytes)` pairs.
/// Virtual paths are slash-separated, relative to the directory root.
/// Symlinks are not followed; non-regular files are skipped. The total is
/// capped at 256MB with a loud error (binaries are not archives).
pub fn collect_embed(dir: &Path) -> Result<Vec<(String, Vec<u8>)>, String> {
    const MAX_TOTAL: u64 = 256 * 1024 * 1024;
    let mut out: Vec<(String, Vec<u8>)> = Vec::new();
    let mut total: u64 = 0;
    let mut stack: Vec<PathBuf> = vec![dir.to_path_buf()];
    while let Some(cur) = stack.pop() {
        let rd = std::fs::read_dir(&cur)
            .map_err(|e| format!("cannot read --embed dir {}: {e}", cur.display()))?;
        let mut entries: Vec<PathBuf> = Vec::new();
        for e in rd {
            let e = e.map_err(|e| format!("cannot list --embed dir: {e}"))?;
            entries.push(e.path());
        }
        entries.sort();
        for p in entries {
            let ft = std::fs::symlink_metadata(&p)
                .map_err(|e| format!("cannot stat {}: {e}", p.display()))?;
            if ft.is_symlink() {
                continue;
            }
            if ft.is_dir() {
                stack.push(p);
            } else if ft.is_file() {
                let rel = p
                    .strip_prefix(dir)
                    .map_err(|e| format!("bad --embed path: {e}"))?;
                let name = rel
                    .components()
                    .map(|c| c.as_os_str().to_string_lossy().into_owned())
                    .collect::<Vec<_>>()
                    .join("/");
                let bytes =
                    std::fs::read(&p).map_err(|e| format!("cannot read {}: {e}", p.display()))?;
                total += bytes.len() as u64;
                if total > MAX_TOTAL {
                    return Err(format!(
                        "--embed dir exceeds 256MB ({}); hint: embed a smaller asset tree",
                        dir.display()
                    ));
                }
                out.push((name, bytes));
            }
        }
    }
    out.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(out)
}

/// Content signature of an `--embed` tree for cache invalidation
/// (names + sizes + mtimes + a byte hash — asset edits must rebuild).
pub fn embed_sig(dir: &Path) -> String {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    match collect_embed(dir) {
        Err(_) => "embed-err".hash(&mut h),
        Ok(files) => {
            files.len().hash(&mut h);
            for (name, bytes) in &files {
                name.hash(&mut h);
                bytes.hash(&mut h);
            }
        }
    }
    format!("{:016x}", h.finish())
}

/// The build cache directory (`~/.zz/cache`, honoring `ZZ_HOME` like
/// every other ZZ path — see `zz_pm::paths::build_cache_dir`).
pub fn cache_dir() -> PathBuf {
    zz_pm::paths::build_cache_dir()
}

/// Compute a cache key from source + build options + target triple.
/// Uses the new typed CacheKey from zz_pm which includes live path-dep hashing.
///
/// Returns `Err` if `zz.toml` exists but is unparseable — the build must
/// not silently degrade to a key that ignores path-dep content, as that
/// would serve stale cached artifacts (Amendment 2, Round 4).
fn cache_key(
    source_path: &Path,
    src: &str,
    opts: &BuildOptions,
    target: Option<&str>,
) -> Result<String, String> {
    let runtime_mtime = runtime_mtime();
    let build_fingerprint = opts.fingerprint_with(target);

    // Use the new CacheKey which hashes path deps from live disk content
    // (Amendment 2, Round 4: path-dep changes must invalidate the cache).
    // The plugin artifact signature joins it: a native rebuild (fresh
    // .o/.a mtimes, changed flags content) must bust entries linked
    // against the older artifacts.
    let mut key = zz_pm::cache_key::CacheKey::compute(
        source_path,
        src,
        build_fingerprint,
        target,
        runtime_mtime,
    )?;
    let (artifact_hash, artifact_flags) = zz_pm::cache_key::artifact_sig(&opts.plugin_artifacts);
    key.artifact_hash = artifact_hash;
    key.artifact_flags = artifact_flags;
    key.native_build_sig = native_build_sig_for(source_path);
    Ok(key.to_slug())
}

/// Native audit signature for the cache key: one [`native_sig`] per
/// `[native]` dependency (tag + compiler + manifest shape + resolved
/// pkg-config from `zz.lock`), sorted and hashed. A compiler upgrade, a
/// flag edit, or a pkg-config drift busts entries linked against older
/// plugin artifacts. Empty when no native deps exist (stable slug).
fn native_build_sig_for(source_path: &Path) -> String {
    // Walk up from the entry file to the project root holding zz.lock.
    let mut dir = source_path
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."));
    let root = loop {
        if dir.join("zz.lock").exists() {
            break dir;
        }
        if !dir.pop() {
            return String::new();
        }
    };
    let lock = match zz_pm::lock::Lockfile::load(&root.join("zz.lock")) {
        Ok(l) => l,
        Err(_) => return String::new(),
    };
    let manifest = zz_pm::manifest::Manifest::load(&root.join("zz.toml")).ok();
    let host_tag = zz_pm::native_build::host_tag().unwrap_or_default();
    let mut per_dep: Vec<String> = Vec::new();
    for dep in &lock.deps {
        let Some(pkg_dir) = resolve_pkg_dir(&root, dep, manifest.as_ref()) else {
            continue;
        };
        let native_toml = if pkg_dir.join("zz.toml").exists() {
            zz_pm::manifest::Manifest::load(&pkg_dir.join("zz.toml"))
                .ok()
                .and_then(|m| m.native)
                .and_then(|n| toml::to_string(&n).ok())
                .unwrap_or_default()
        } else {
            String::new()
        };
        if native_toml.is_empty() {
            continue;
        }
        let rec = dep.native.as_ref();
        let tag = rec.map(|r| r.tag.as_str()).unwrap_or(host_tag.as_str());
        let compiler = rec.map(|r| r.compiler.as_str()).unwrap_or("");
        let pkg = rec.map(|r| r.pkg_config_resolved.as_str()).unwrap_or("");
        let mut entry = dep.name.clone();
        entry.push('\0');
        entry.push_str(&zz_pm::cache_key::CacheKey::native_sig(
            tag,
            compiler,
            &native_toml,
            pkg,
        ));
        per_dep.push(entry);
    }
    if per_dep.is_empty() {
        return String::new();
    }
    per_dep.sort();
    zz_pm::hash::hash_bytes(per_dep.join("\0").as_bytes())
}

/// Get modification time of every input that affects native output, for
/// cache invalidation: the C runtime + codegen (`zz_codegen`), the Rust
/// native runtime linked into FFI builds (`zz_native_rt`), the pure-ZZ
/// stdlib sources merged into every build (`zz_stdlib/zz/`), and this
/// build module itself.
fn runtime_mtime() -> Option<u64> {
    let crates = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../");
    // Crate dirs and the subdirs to watch inside them (one level deep).
    let watched: &[(&str, &[&str])] = &[
        ("zz_codegen", &["src/lower", "src/runtime", "src"]),
        ("zz_native_rt", &["src"]),
        ("zz_stdlib", &["zz"]),
        ("zz_cli", &["src"]),
    ];
    let mut mtimes: Vec<u64> = Vec::new();
    for (krate, dirs) in watched {
        for dir in *dirs {
            let dir = crates.join(krate).join(dir);
            let Ok(entries) = std::fs::read_dir(&dir) else {
                continue;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                // Recurse one level (covers `zz_stdlib/zz/<mod>/` and
                // `zz_codegen/src/` files alongside the subdirs above).
                let candidates = if path.is_dir() {
                    std::fs::read_dir(&path)
                        .map(|rd| rd.flatten().map(|e| e.path()).collect::<Vec<_>>())
                        .unwrap_or_default()
                } else {
                    vec![path]
                };
                for cand in candidates {
                    if let Ok(meta) = std::fs::metadata(&cand) {
                        if meta.is_file() {
                            if let Ok(m) = meta.modified() {
                                if let Ok(d) = m.duration_since(std::time::UNIX_EPOCH) {
                                    mtimes.push(d.as_secs());
                                }
                            }
                        }
                    }
                }
            }
        }
    }
    if mtimes.is_empty() {
        return None;
    }
    // Combine order-independently: sum of all file mtimes.
    Some(mtimes.iter().fold(0u64, |acc, m| acc.wrapping_add(*m)))
}

fn opts_for(mode: BuildMode) -> BuildOptions {
    match mode {
        BuildMode::Dev => BuildOptions::dev(),
        BuildMode::Release => BuildOptions::release(),
        BuildMode::Static => BuildOptions::static_lto(),
        BuildMode::Pgo => BuildOptions::pgo_generate(),
        BuildMode::PgoUse => BuildOptions::pgo_use(),
        BuildMode::Full => BuildOptions::full(),
        BuildMode::FullPgo => BuildOptions::full_pgo_use(),
    }
}

/// Walk up from a source file to the enclosing project root (has zz.lock),
/// canonicalized to an absolute path.
///
/// Canonicalization matters: a relative root (e.g. `src/main.zz` invoked
/// from the project dir finds root `""`) joined with a `path = "../.."`
/// dep once produced `../../build.sh`, which bash resolved against the
/// hook CWD instead of the package dir. Absolute roots make every
/// downstream join unambiguous.
fn canonical_project_root(project_path: &Path) -> Option<PathBuf> {
    let mut dir = project_path.parent().unwrap_or(project_path);
    if dir.as_os_str().is_empty() {
        dir = Path::new(".");
    }
    loop {
        if dir.join("zz.lock").exists() {
            return Some(std::fs::canonicalize(dir).unwrap_or_else(|_| dir.to_path_buf()));
        }
        dir = dir.parent()?;
    }
}

/// Resolve a locked dependency to its package directory, canonicalized
/// (same `..` rationale as [`canonical_project_root`]).
pub(crate) fn resolve_pkg_dir(
    project_root: &Path,
    dep: &zz_pm::lock::LockedDep,
    manifest: Option<&zz_pm::manifest::Manifest>,
) -> Option<PathBuf> {
    let dir = if dep.source == "path" {
        let m = manifest?;
        match m.dependencies.get(&dep.name)? {
            zz_pm::manifest::DepSpec::Path(path_dep) => project_root.join(&path_dep.path),
            _ => return None,
        }
    } else {
        zz_pm::paths::cas_entry(&dep.hash)
    };
    Some(std::fs::canonicalize(&dir).unwrap_or(dir))
}

/// Discover plugin manifests (`.zzi` files) from installed dependencies.
///
/// Reads `zz.lock` and `zz.toml` from the project directory, looks up each
/// dependency's package directory (CAS for git deps, `vendor/<name>/` for path
/// deps), and loads any `plugin.zzi` file found. Returns a flat list of
/// `(function_name, FuncSig)` pairs ready to merge into the checker.
pub(crate) fn discover_plugin_manifests(project_path: &Path) -> Vec<(String, zz_checker::FuncSig)> {
    let mut plugin_funcs = Vec::new();

    let Some(project_root) = canonical_project_root(project_path) else {
        return plugin_funcs;
    };

    let lock = match zz_pm::lock::Lockfile::load(&project_root.join("zz.lock")) {
        Ok(l) => l,
        Err(_) => return plugin_funcs,
    };

    // Load manifest to check for path deps
    let manifest_path = project_root.join("zz.toml");
    let manifest = zz_pm::manifest::Manifest::load(&manifest_path).ok();

    for dep in &lock.deps {
        let Some(pkg_dir) = resolve_pkg_dir(&project_root, dep, manifest.as_ref()) else {
            continue;
        };

        let manifest_path = pkg_dir.join("plugin.zzi");
        if !manifest_path.exists() {
            continue;
        }
        match load_manifest(&manifest_path) {
            Ok(m) => {
                for (name, sig) in m.funcs {
                    plugin_funcs.push((name, sig));
                }
            }
            Err(e) => {
                eprintln!(
                    "warning: failed to load plugin manifest for `{}`: {e}",
                    dep.name
                );
            }
        }
    }

    plugin_funcs
}

/// Gates for native builds (CLI flags).
#[derive(Debug, Clone, Copy)]
pub(crate) struct NativeBuildOpts {
    /// `--allow-source-builds`: compile transitive `[native]` deps from
    /// source when no prebuilt covers the host tag.
    pub allow_source_builds: bool,
    /// `--allow-hooks`: run legacy `build = "..."` hooks (direct deps
    /// only; transitive hooks always error).
    pub allow_hooks: bool,
}

impl NativeBuildOpts {
    /// Dev-loop behavior for `zz run` (via [`ensure_native_hooks`]):
    /// build what is needed, warn loudly, never fail the run.
    pub(crate) fn permissive() -> Self {
        Self {
            allow_source_builds: true,
            allow_hooks: true,
        }
    }
}

/// Output of [`build_native_deps`]: link inputs plus per-dep audit
/// records for `zz.lock`.
pub(crate) struct NativeBuild {
    pub artifacts: Vec<PathBuf>,
    pub link_args: Vec<String>,
    pub audits: Vec<(String, zz_pm::lock::LockedNative)>,
}

/// Run native builds for every dependency and discard the artifacts.
///
/// `zz run` calls this post-link (permissive: warn-only) so VM dlopen
/// works without a prior `zz build`.
pub(crate) fn ensure_native_hooks(project_root: &Path) {
    // Build_native_deps walks up from its argument's parent looking for
    // zz.lock — anchor inside the canonical root.
    let project_root =
        std::fs::canonicalize(project_root).unwrap_or_else(|_| project_root.to_path_buf());
    let anchor = project_root.join("zz.toml");
    if let Err(e) = build_native_deps(&anchor, NativeBuildOpts::permissive()) {
        eprintln!("warning: native build failed: {e}");
    }
}
///
/// Build native code for plugin packages under the given gates.
///
/// Reads `zz.lock` and `zz.toml`, finds dependencies that have a `plugin.zzi`
/// and a `zz.toml` with a `[native]` section, builds them via the declared
/// backend, and returns compiled artifacts (`.o` / `.a` files), raw linker
/// flags from each package's `build/ldflags.txt`, and audit records.
///
/// Gate behavior:
/// - `cc` without a compatible prebuilt entry: transitive deps error
///   unless `allow_source_builds`; direct deps warn and build.
/// - `cc` failures are hard errors (fail closed — a half-built plugin
///   must never silently link nothing).
/// - `hook` on transitive deps always errors. On direct deps it errors
///   unless `allow_hooks`, and warns on every allowed use. Allowed hook
///   failures keep the historical warn-and-continue behavior.
pub(crate) fn build_native_deps(
    project_path: &Path,
    opts: NativeBuildOpts,
) -> Result<NativeBuild, String> {
    let mut artifacts = Vec::new();
    let mut link_args: Vec<String> = Vec::new();
    let mut audits: Vec<(String, zz_pm::lock::LockedNative)> = Vec::new();

    let empty = NativeBuild {
        artifacts: Vec::new(),
        link_args: Vec::new(),
        audits: Vec::new(),
    };
    let Some(project_root) = canonical_project_root(project_path) else {
        return Ok(empty);
    };

    let lock = match zz_pm::lock::Lockfile::load(&project_root.join("zz.lock")) {
        Ok(l) => l,
        Err(_) => return Ok(empty),
    };

    // Load manifest to check for path deps. Direct deps are the manifest's
    // declared dependencies; anything else locked is transitive.
    let manifest_path = project_root.join("zz.toml");
    let manifest = zz_pm::manifest::Manifest::load(&manifest_path).ok();

    for dep in &lock.deps {
        let direct = manifest
            .as_ref()
            .is_some_and(|m| m.dependencies.contains_key(&dep.name));
        let Some(pkg_dir) = resolve_pkg_dir(&project_root, dep, manifest.as_ref()) else {
            continue;
        };

        // Check for plugin.zzi (this is a native package)
        if !pkg_dir.join("plugin.zzi").exists() {
            continue;
        }

        // Read the package's zz.toml for [native] section
        let dep_manifest_path = pkg_dir.join("zz.toml");
        let dep_manifest = match zz_pm::manifest::Manifest::load(&dep_manifest_path) {
            Ok(m) => m,
            Err(_) => continue,
        };

        let native = match &dep_manifest.native {
            Some(n) => n,
            None => continue,
        };

        // Declarative backend: validated C build owned by zz itself.
        if let Some(cc_spec) = native.build_cc.as_ref() {
            let host = zz_pm::native_build::host_tag().unwrap_or_default();
            let compatible = |t: &String| {
                zz_pm::native_build::tags_compatible(t, &host)
                    || zz_pm::native_build::tags_compatible(&host, t)
                    || *t == host
            };
            let prebuilt_hit = native.prebuilt.as_ref().and_then(|p| {
                p.target
                    .iter()
                    .find(|(t, _)| compatible(t))
                    .map(|(t, a)| (t.clone(), a.clone()))
            });
            let had_entry = prebuilt_hit.is_some();
            // Prebuilt path: verified bytes, no compiler needed.
            if let Some((tag, artifact)) = prebuilt_hit {
                eprintln!("zz: fetching prebuilt plugin `{}` for {tag}...", dep.name);
                match zz_pm::native_build::fetch_prebuilt(&pkg_dir, &dep.name, &tag, &artifact) {
                    Ok(out) => {
                        artifacts.extend(out.objects);
                        artifacts.push(out.archive);
                        collect_link_args(&out.ldflags_path, &mut link_args);
                        audits.push((
                            dep.name.clone(),
                            zz_pm::lock::LockedNative {
                                backend: "prebuilt".to_string(),
                                tag: out.tag,
                                compiler: String::new(),
                                artifact_sha256: artifact.sha256.clone(),
                                pkg_config_resolved: String::new(),
                            },
                        ));
                        continue;
                    }
                    Err(e) if e.contains("cannot download") => {
                        eprintln!("warning: prebuilt unavailable: {e}");
                        // Fall through to source build (gates apply below).
                    }
                    Err(e) => {
                        return Err(format!(
                            "plugin `{}` prebuilt for {tag} failed verification: {e}",
                            dep.name
                        ));
                    }
                }
            }
            if !had_entry {
                if !direct && !opts.allow_source_builds {
                    return Err(format!(
                        "plugin `{}` has no prebuilt for `{host}`; ask the maintainer or re-run with --allow-source-builds",
                        dep.name
                    ));
                }
                if direct {
                    eprintln!(
                        "warning: plugin `{}` source-builds (no prebuilt for `{host}`)",
                        dep.name
                    );
                }
            }
            eprintln!("zz: building plugin `{}` (declarative cc)...", dep.name);
            let out = zz_pm::native_build::ensure_cc(&pkg_dir, &dep.name, cc_spec, None)
                .map_err(|e| format!("plugin `{}` declarative build failed: {e}", dep.name))?;
            artifacts.extend(out.objects);
            artifacts.push(out.archive);
            collect_link_args(&out.ldflags_path, &mut link_args);
            audits.push((
                dep.name.clone(),
                zz_pm::lock::LockedNative {
                    backend: "cc".to_string(),
                    tag: out.tag,
                    compiler: out.compiler,
                    artifact_sha256: String::new(),
                    pkg_config_resolved: zz_pm::native_build::pkg_config_versions(
                        &cc_spec.pkg_config,
                    ),
                },
            ));
            continue;
        }
        let Some(hook_rel) = native.build.as_deref() else {
            continue;
        };

        if !direct {
            return Err(format!(
                "plugin `{}` uses legacy [native] build hook, which is forbidden for transitive dependencies\n\
                 hint: ask the maintainer to migrate to [native.build-cc] (see docs/plugin-author-guide.md §4)",
                dep.name
            ));
        }
        if !opts.allow_hooks {
            return Err(format!(
                "plugin `{}` uses legacy [native] build = \"{hook_rel}\" (deprecated)\n\
                 hint: re-run with --allow-hooks, or ask the maintainer to migrate to [native.build-cc]",
                dep.name
            ));
        }

        // Invoke the build hook
        eprintln!(
            "warning: [native] build = \"{hook_rel}\" is deprecated; migrate to [native.build-cc] (see docs/plugin-author-guide.md §4)"
        );
        let build_script = pkg_dir.join(hook_rel);
        if !build_script.exists() {
            eprintln!(
                "warning: plugin `{}` build script not found: {}",
                dep.name,
                build_script.display()
            );
            continue;
        }

        eprintln!("zz: building plugin `{}` via {hook_rel}...", dep.name);
        let output = std::process::Command::new("bash")
            .arg(&build_script)
            .current_dir(&pkg_dir)
            .output();

        match output {
            Ok(o) => {
                if !o.status.success() {
                    let stderr = String::from_utf8_lossy(&o.stderr);
                    eprintln!("warning: plugin `{}` build failed: {}", dep.name, stderr);
                    continue;
                }
            }
            Err(e) => {
                eprintln!(
                    "warning: failed to run plugin `{}` build hook: {e}",
                    dep.name
                );
                continue;
            }
        }

        // Collect artifacts and linker flags from the build output dir
        let build_dir = pkg_dir.join("build");
        if build_dir.exists() {
            for entry in std::fs::read_dir(&build_dir).into_iter().flatten() {
                let entry = entry.unwrap();
                let path = entry.path();
                if let Some(ext) = path.extension() {
                    if ext == "o" || ext == "a" {
                        artifacts.push(path);
                    }
                }
            }
            collect_link_args(&build_dir.join("ldflags.txt"), &mut link_args);
        }
    }

    // Archives often bundle the same objects that are also emitted loose
    // (e.g. libzimg_native.a contains zimg_wrapper.o alongside
    // build/zimg_wrapper.o). Linking both yields "multiple definition"
    // errors, so thin the archives: copy each conflicting archive next
    // to the original and delete the members shadowed by loose objects.
    // The loose object is authoritative (build hooks recompile it every
    // run, while archives may be cargo-cached and stale).
    thin_shadowed_archive_members(&mut artifacts);

    Ok(NativeBuild {
        artifacts,
        link_args,
        audits,
    })
}

/// Discover hook/cc artifacts for the AOT link under explicit gates.
/// Thin wrapper over [`build_native_deps`] returning the link inputs.
fn discover_native_artifacts(
    project_path: &Path,
    opts: NativeBuildOpts,
) -> Result<(Vec<PathBuf>, Vec<String>), String> {
    let built = build_native_deps(project_path, opts)?;
    Ok((built.artifacts, built.link_args))
}

/// Merge whitespace-split flags from an `ldflags.txt` into `link_args`,
/// deduplicated (both backends share this collection step).
fn collect_link_args(ldflags_path: &Path, link_args: &mut Vec<String>) {
    if let Ok(flags) = std::fs::read_to_string(ldflags_path) {
        for flag in flags.split_whitespace() {
            if !link_args.iter().any(|f| f == flag) {
                link_args.push(flag.to_string());
            }
        }
    }
}

/// Copy archives that duplicate loose `.o` files and delete the shadowed
/// members from the copies, replacing the archive paths in `artifacts`.
/// When `ar` is unavailable the list is left untouched.
fn thin_shadowed_archive_members(artifacts: &mut [PathBuf]) {
    let loose_stems: std::collections::HashSet<String> = artifacts
        .iter()
        .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("o"))
        .filter_map(|p| p.file_stem().and_then(|s| s.to_str()).map(String::from))
        .collect();
    if loose_stems.is_empty() {
        return;
    }
    // replacement per archive.
    let mut replacements: Vec<(usize, PathBuf)> = Vec::new();
    for (i, archive) in artifacts.iter().enumerate() {
        if archive.extension().and_then(|e| e.to_str()) != Some("a") {
            continue;
        }
        let output = match std::process::Command::new("ar")
            .arg("t")
            .arg(archive)
            .output()
        {
            Ok(o) if o.status.success() => o,
            _ => continue,
        };
        let mut shadowed: Vec<String> = Vec::new();
        for member in String::from_utf8_lossy(&output.stdout).lines() {
            let member = member.trim();
            let Some(base) = member.strip_suffix(".o") else {
                continue;
            };
            // Rust archive members are `<hash>-<file>.o`; plain objects
            // are `<file>.o`. Match either the full stem or the suffix
            // after the last `-`.
            let hit = loose_stems.contains(base)
                || base
                    .rsplit('-')
                    .next()
                    .is_some_and(|s| loose_stems.contains(s));
            if hit {
                shadowed.push(member.to_string());
            }
        }
        if shadowed.is_empty() {
            continue;
        }
        let thin_name = archive
            .file_name()
            .and_then(|n| n.to_str())
            .map(|n| format!("{n}.zz-thin.a"))
            .unwrap_or_else(|| "zz-thin.a".to_string());
        let thin_path = archive.with_file_name(&thin_name);
        // Reuse a thin copy that is newer than the archive itself.
        let reuse = std::fs::metadata(&thin_path)
            .and_then(|m| m.modified())
            .ok()
            .zip(std::fs::metadata(archive).and_then(|m| m.modified()).ok())
            .is_some_and(|(thin_mtime, arch_mtime)| thin_mtime >= arch_mtime);
        if !reuse {
            if std::fs::copy(archive, &thin_path).is_err() {
                continue;
            }
            let mut cmd = std::process::Command::new("ar");
            cmd.arg("d").arg(&thin_path);
            for member in &shadowed {
                cmd.arg(member);
            }
            if !cmd.output().is_ok_and(|o| o.status.success()) {
                let _ = std::fs::remove_file(&thin_path);
                continue;
            }
        }
        replacements.push((i, thin_path));
    }
    for (i, thin_path) in replacements {
        artifacts[i] = thin_path;
    }
}

/// Type-check + DCE all modules to a typed program, entry-main name, and
/// reachable set.
fn typed_program_for(
    path: &Path,
    entry_ns: &str,
) -> Result<(TypedProgram, zz_hir::ReachableSet, String), String> {
    // Discover plugin manifests from installed dependencies and merge their
    // function signatures into the checker's function table.
    let plugin_funcs = discover_plugin_manifests(path);
    let loaded = if plugin_funcs.is_empty() {
        loader::load_program(path)?
    } else {
        loader::load_program_with_plugins(path, &plugin_funcs)?
    };
    let mut has_errors = false;
    for e in &loaded.errors {
        let mut files = zz_frontend::diag::Files::new();
        let id = files.add(e.name.clone(), e.source.clone());
        eprint!(
            "{}",
            zz_frontend::diag::render_to_string(&files, id, &e.diags)
        );
        if e.diags
            .iter()
            .any(|d| d.severity == zz_frontend::diag::Severity::Error)
        {
            has_errors = true;
        }
    }
    if has_errors {
        return Err("program failed to type-check".into());
    }

    // Merge all module programs into one for HIR building / DCE. Modules
    // are namespaced by the loader (entry = file stem), so concat is safe.
    // Pure-ZZ stdlib sources come first (mirroring the VM, which executes
    // them before user code): without them AOT binaries silently lower
    // pure-ZZ helpers like `str.repeat` to unit. They contain only function
    // definitions, so merging cannot introduce top-level side effects.
    let mut merged_stmts = Vec::new();
    for zz_prog in zz_stdlib::zz_stdlib_programs() {
        merged_stmts.extend(zz_prog.program.stmts.iter().cloned());
    }
    let merged_span = loaded
        .programs
        .last()
        .map(|p| p.span)
        .unwrap_or(Span::new(0, 0));
    for p in &loaded.programs {
        merged_stmts.extend(p.stmts.iter().cloned());
    }
    let merged = zz_frontend::ast::Program {
        stmts: merged_stmts,
        span: merged_span,
    };

    let res = zz_hir::build_program(
        &merged,
        HashMap::new(),
        loaded.funcs,
        loaded.structs,
        loaded.aliases,
        loaded.enums,
    );
    if !res.diagnostics.is_empty() {
        for d in &res.diagnostics {
            if d.severity == zz_frontend::diag::Severity::Error {
                eprintln!("zz: {}", d.message);
            }
        }
    }
    let main_key = format!("{entry_ns}.main");
    let (pruned, reach) = zz_hir::dce(&res.program, &main_key);
    Ok((pruned, reach, main_key))
}

/// Build the unified IR module for `path` (chunk-backend input): load,
/// check, merge (pure-ZZ stdlib first, mirroring [`typed_program_for`]),
/// VM-compile, then `lower_typed`. Returns the module + dotted main key.
fn chunk_module_for(path: &Path) -> Result<(zz_ir::Module, String), String> {
    let entry_ns = path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let plugin_funcs = discover_plugin_manifests(path);
    let loaded = if plugin_funcs.is_empty() {
        loader::load_program(path)?
    } else {
        loader::load_program_with_plugins(path, &plugin_funcs)?
    };
    let mut has_errors = false;
    for e in &loaded.errors {
        let mut files = zz_frontend::diag::Files::new();
        let id = files.add(e.name.clone(), e.source.clone());
        eprint!(
            "{}",
            zz_frontend::diag::render_to_string(&files, id, &e.diags)
        );
        if e.diags
            .iter()
            .any(|d| d.severity == zz_frontend::diag::Severity::Error)
        {
            has_errors = true;
        }
    }
    if has_errors {
        return Err("program failed to type-check".into());
    }
    // Merged user program. Pure-ZZ stdlib sources are deliberately NOT
    // merged here: their bodies trip the VM compiler's join verifier
    // when compiled outside their home module (latent gap — stdlib only
    // ever runs through the tree-walker today). Calls to pure-ZZ
    // helpers (e.g. `str.repeat`) therefore fail `coverage` with a clean
    // "cannot resolve call target" error instead of miscompiling.
    // Slice-2 (cross-module IR merge, #253) will include them.
    let mut merged_stmts = Vec::new();
    let merged_span = loaded
        .programs
        .last()
        .map(|p| p.span)
        .unwrap_or(Span::new(0, 0));
    for p in &loaded.programs {
        merged_stmts.extend(p.stmts.iter().cloned());
    }
    let merged = zz_frontend::ast::Program {
        stmts: merged_stmts,
        span: merged_span,
    };
    let typed = zz_hir::build_program(
        &merged,
        HashMap::new(),
        loaded.funcs,
        loaded.structs.clone(),
        loaded.aliases,
        loaded.enums.clone(),
    );
    let native_names: std::sync::Arc<std::collections::HashSet<String>> =
        std::sync::Arc::new(loaded.natives.keys().cloned().collect());
    let chunk = zz_runtime::vm::Compiler::compile_program_typed(
        &merged,
        std::sync::Arc::new(typed.program.types),
        loaded.structs,
        loaded.enums,
        native_names,
    );
    let module = zz_ir::lower::lower_typed(&chunk, &typed.program.funcs)
        .map_err(|e| format!("zz: ir lower failed: {e}"))?;
    Ok((module, format!("{entry_ns}.main")))
}

/// Output directory for a build of `src` (authoritative, no duplicates):
/// `<project-root>/bin` when `src` sits under a project (nearest ancestor
/// holding `zz.toml`, resolved from the source path itself so an explicit
/// path builds into its owning project even when invoked from another
/// directory), otherwise the current working directory (standalone builds
/// place `./<stem>` next to the invocation, never `./bin/`).
pub fn output_dir_for(src: &Path) -> PathBuf {
    if let Some(root) = crate::loader::find_project_root(src) {
        return root.join("bin");
    }
    std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
}

/// Conventional entry point of a project root: `src/main.zz`, then
/// `main.zz`. Returns the `src/main.zz` candidate even when neither
/// exists (callers surface a "no entry file" error).
pub fn project_entry(root: &Path) -> PathBuf {
    let src_main = root.join("src").join("main.zz");
    if src_main.is_file() {
        return src_main;
    }
    let top_main = root.join("main.zz");
    if top_main.is_file() {
        return top_main;
    }
    src_main
}

/// Authoritative entry point of a project root: the configured
/// `[package] entry` when present (validated — a bad value is a loud
/// error), else the conventional [`project_entry`], which must exist.
pub fn resolve_entry(root: &Path) -> Result<PathBuf, String> {
    // Configured entries validate loudly; an unloadable manifest falls
    // through to conventional discovery (other layers report it).
    if let Ok(manifest) = zz_pm::manifest::Manifest::load(&root.join("zz.toml")) {
        if let Some(entry) = manifest.entry_path(root)? {
            return Ok(entry);
        }
    }
    let entry = project_entry(root);
    if entry.is_file() {
        return Ok(entry);
    }
    Err(format!(
        "no entry file in `{}`\n\
         hint: expected src/main.zz or main.zz (or set [package] entry)",
        root.display()
    ))
}

/// Binary name configured for a project root: `[package] name` when
/// present and a safe file name, else `None` (callers fall back to the
/// entry file stem).
pub fn package_bin_name(root: &Path) -> Option<String> {
    let manifest = zz_pm::manifest::Manifest::load(&root.join("zz.toml")).ok()?;
    let name = manifest.package.name;
    if name.is_empty() || name.contains('/') || name.contains('\\') || name.contains("..") {
        return None;
    }
    Some(name)
}

/// Same-file comparison via canonicalization (falls back to a plain
/// comparison when either side cannot canonicalize).
fn same_file(a: &Path, b: &Path) -> bool {
    match (a.canonicalize(), b.canonicalize()) {
        (Ok(x), Ok(y)) => x == y,
        _ => a == b,
    }
}

/// Default binary stem for `src`: the package name when `src` is its
/// project's resolved entry point, else the file stem (`"app"`
/// fallback). Never reads CWD: only the source path and its owning
/// project matter.
pub fn default_stem_for(src: &Path) -> String {
    if let Some(root) = crate::loader::find_project_root(src) {
        let entry = resolve_entry(&root)
            .ok()
            .unwrap_or_else(|| project_entry(&root));
        if same_file(src, &entry) {
            if let Some(name) = package_bin_name(&root) {
                return name;
            }
        }
    }
    src.file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "app".to_string())
}

/// Authoritative output destination for a build of `src` (single writer:
/// no legacy duplicates). Precedence:
/// - path-like `-o` (contains a separator): exact destination relative
///   to the current directory (go-like), `+ .exe` on Windows targets.
/// - bare `-o`: resolved stem inside [`output_dir_for`] (project
///   `bin/` or standalone CWD).
/// - none: [`default_stem_for`] inside [`output_dir_for`].
///
/// `target` contributes the cross suffix / `.exe` via [`bin_name`].
pub fn planned_dest_for(src: &Path, output: Option<&Path>, target: Option<&str>) -> PathBuf {
    match output {
        Some(o) if o.components().count() > 1 => {
            let mut dest = o.to_path_buf();
            let windows = match target {
                Some(t) => zz_codegen::is_windows_target(t),
                None => cfg!(windows),
            };
            if windows && dest.extension().is_none() {
                dest.set_extension("exe");
            }
            dest
        }
        Some(o) => {
            let stem = o.to_string_lossy().into_owned();
            output_dir_for(src).join(bin_name(&stem, target))
        }
        None => output_dir_for(src).join(bin_name(&default_stem_for(src), target)),
    }
}

/// Output binary name: `<stem>`, `<stem>-<triple>` for cross builds,
/// plus `.exe` for Windows hosts/targets. Uses path components only —
/// never string-concatenated separators.
pub fn bin_name(stem: &str, target: Option<&str>) -> String {
    let mut name = stem.to_string();
    if let Some(t) = target {
        name.push('-');
        name.push_str(t);
    }
    let windows = match target {
        Some(t) => zz_codegen::is_windows_target(t),
        None => cfg!(windows),
    };
    if windows {
        name.push_str(".exe");
    }
    name
}

/// True when `p` is a file that can be executed: present, non-empty, and
/// (on unix) carrying at least one execute bit. Protects the cache from
/// stale artifacts left by interrupted builds, which clang may leave as
/// a complete-but-non-executable (0644) file.
fn is_usable_cache_binary(p: &Path) -> bool {
    let Ok(meta) = p.metadata() else {
        return false;
    };
    if !meta.is_file() || meta.len() == 0 {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        meta.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}

/// Native Clang build (cached), published to the authoritative output
/// destination ([`planned_dest_for`]: project-root `bin/` or standalone
/// CWD). Returns the output binary path. `Dev` mode compiles with `-O0 -g`
/// (fast debug binary); `Release`/`Static`/`Pgo` use their option sets.
///
/// When no Clang provider is installed, the generated C + build scripts
/// are still emitted next to the planned destination before the error is
/// returned, so the user can build manually on a machine with Clang.
pub fn build_release(
    path: &Path,
    mode: BuildMode,
    rel: &ReleaseOptions,
) -> Result<PathBuf, String> {
    let entry_ns = path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let (pruned, reach, main_key) = typed_program_for(path, &entry_ns)?;
    // Project-owned sources publish under `<root>/bin/` — make sure the
    // root ignores it even when the project predates the redesign (new
    // projects get this from `init`/`new`). Append-only, idempotent,
    // silent; failures (read-only checkouts) never fail the build.
    // Standalone builds leave the invocation dir alone.
    if let Some(root) = crate::loader::find_project_root(path) {
        let _ = zz_pm::manifest::Manifest::ensure_gitignore(&root);
    }
    let mut opts = opts_for(mode);
    opts.allow_static_downgrade = rel.allow_static_downgrade;
    let target = rel.target_opt();
    // Default-static fallback for macOS (static linking is rejected
    // there): downgrade to dynamic with a note instead of failing the
    // default build. Explicit `--static` keeps the hard error in
    // `validate` below. The note prints only on a real build, after the
    // cache check, so cache hits stay silent.
    let mut static_note: Option<&str> = None;
    if opts.static_link && opts.allow_static_downgrade {
        let macos = match target {
            Some(t) => zz_codegen::is_macos_target(t),
            None => cfg!(target_os = "macos"),
        };
        if macos {
            opts.static_link = false;
            static_note = Some("macOS targets cannot statically link; building dynamic");
        }
    }

    // Discover and build plugin native artifacts (compiled .o / .a files
    // plus dependency link flags from each package's ldflags.txt).
    let (plugin_artifacts, plugin_link_args) = discover_native_artifacts(
        path,
        NativeBuildOpts {
            allow_source_builds: rel.allow_source_builds,
            allow_hooks: rel.allow_hooks,
        },
    )?;
    opts.plugin_artifacts = plugin_artifacts;
    opts.plugin_link_args = plugin_link_args;

    // `--embed`: bake the asset tree into the binary (content-hashed into
    // the cache key below so asset edits rebuild).
    let embed_slug = match rel.embed.as_deref() {
        None => String::new(),
        Some(dir) => {
            let files = collect_embed(dir)?;
            opts.embed_assets = files
                .into_iter()
                .map(|(name, bytes)| zz_codegen::EmbedAsset { name, bytes })
                .collect();
            embed_sig(dir)
        }
    };

    // Early validation: exact CLI-contract errors for PGO-cross and
    // static-macOS, before any cache or toolchain work.
    if let Err(e) = zz_codegen::validate(&opts, target) {
        return Err(e.to_string());
    }

    // Resolve the provider now so a missing toolchain fails fast — but
    // still leave app.c + scripts behind for manual builds, next to the
    // planned destination (project-root `bin/` or standalone CWD).
    let clang = match zz_codegen::detect_clang_with(rel.provider) {
        Some(c) => c,
        None => {
            let lowered = zz_codegen::lower_only(&pruned, &reach, &main_key);
            let mut script_opts = opts.clone();
            script_opts.curl_link = script_opts.curl_link || lowered.needs_curl;
            script_opts.sqlite_link = script_opts.sqlite_link || lowered.needs_sqlite;
            let dir = planned_dest_for(path, rel.output.as_deref(), target)
                .parent()
                .map(Path::to_path_buf)
                .filter(|p| !p.as_os_str().is_empty())
                .unwrap_or_else(|| PathBuf::from("."));
            let _ = zz_codegen::emit_c_plus_script(&lowered.source, &dir, target, &script_opts);
            return Err(zz_codegen::BuildError::NoClang.to_string());
        }
    };
    // Seed system-lib needs from reachability before the static-syslibs
    // probe: with conditional linking, programs that neither fetch nor
    // query need no static syslibs, so the probe must not downgrade them.
    // The final `build_native` ORs the same flags again from lowering.
    opts.curl_link = opts.curl_link || zz_codegen::ffi::needs_curl_link(&reach.natives);
    opts.sqlite_link = opts.sqlite_link || zz_codegen::ffi::needs_sqlite_link(&reach.natives);
    // Default-static fallback for missing static system libraries: only
    // the libraries the program actually needs are probed (cached per
    // provider + lib set). Downgrade to dynamic with a note instead of
    // failing the default build. Explicit `--static` skips this
    // (allow_static_downgrade false) and keeps the linker's error.
    if opts.static_link
        && opts.allow_static_downgrade
        && !zz_codegen::compile::static_syslibs_available_for(
            &clang,
            opts.curl_link,
            opts.sqlite_link,
        )
    {
        opts.static_link = false;
        static_note = if opts.curl_link && opts.sqlite_link {
            Some("static system libraries (libcurl.a, libsqlite3.a) not found; building dynamic")
        } else if opts.curl_link {
            Some("static system library (libcurl.a) not found; building dynamic")
        } else {
            Some("static system library (libsqlite3.a) not found; building dynamic")
        };
    }
    if rel.verbose {
        eprintln!(
            "zz: {} {} {}",
            clang.label,
            clang.version(),
            zz_codegen::compile::clang_flags(&opts, target).join(" ")
        );
    }

    // `-o` renames at publish time only (excluded from the cache key:
    // identical source + options reuse one cached binary under any name).
    // Captured before `opts` moves into the clang build below.
    let output = rel.output.clone();
    // Cache: reuse when the same source + build options + target were
    // built before. The chunk backend carries its own slug so the two
    // codegen paths never share entries (dual-codegen gate).
    let kind_slug = if rel.chunk {
        format!("{mode:?}-chunk")
    } else {
        format!("{mode:?}")
    };
    // Cache: reuse when the same source + build options + target were
    // built before.
    let dir = cache_dir();
    std::fs::create_dir_all(&dir).map_err(|e| format!("cannot create cache: {e}"))?;
    let source = std::fs::read_to_string(path).map_err(|e| format!("read: {e}"))?;
    let key = cache_key(path, &source, &opts, target)?;
    let target_slug = target.unwrap_or("host");
    // The embed tree is content-fingerprinted separately (compact slug
    // addition — asset edits must never reuse a non-embed binary).
    let cached = if embed_slug.is_empty() {
        dir.join(format!("{key}-{kind_slug}-{target_slug}"))
    } else {
        dir.join(format!("{key}-{kind_slug}-{target_slug}-{embed_slug}"))
    };

    if is_usable_cache_binary(&cached) {
        // Reuse the cached binary.
        return publish_to_bin(&cached, path, target, output.as_deref());
    }
    // Stale artifact (interrupted build, missing exec bit, empty file):
    // drop it so the fresh build below replaces it.
    let _ = std::fs::remove_file(&cached);
    if let Some(note) = static_note {
        eprintln!("zz: note: {note}");
    }

    // Build to a unique temp path in the same directory, then atomically
    // rename into place. Concurrent builds of the same key (parallel tests,
    // parallel `zz` invocations) each produce a complete, executable file;
    // observers never see a partially-written or non-executable binary.
    static TMP_COUNTER: AtomicU64 = AtomicU64::new(0);
    let tmp = dir.join(format!(
        "{key}-{kind_slug}-{target_slug}.{}.{}.tmp",
        std::process::id(),
        TMP_COUNTER.fetch_add(1, Ordering::SeqCst)
    ));
    // Chunk backend: lower from the unified IR, then compile the
    // emitted source with the same option/flag flow as `build_native`.
    if rel.chunk {
        let (module, main_key) = chunk_module_for(path)?;
        let lowered =
            zz_codegen::build_chunk_module(&module, &main_key).map_err(|e| e.to_string())?;
        opts.native_rt = opts.native_rt || lowered.needs_native_rt;
        opts.pg_link = opts.pg_link || lowered.needs_pg_link;
        opts.float_link = opts.float_link || lowered.needs_float_fmt;
        opts.curl_link = opts.curl_link || lowered.needs_curl;
        opts.sqlite_link = opts.sqlite_link || lowered.needs_sqlite;
        if let Err(e) = zz_codegen::compile::build_with(&lowered.source, &tmp, opts, target, &clang)
        {
            let _ = std::fs::remove_file(&tmp);
            return Err(e.to_string());
        }
    } else if let Err(e) =
        zz_codegen::build_native_with(&pruned, &reach, &main_key, opts, target, &clang, &tmp)
    {
        let _ = std::fs::remove_file(&tmp);
        return Err(e.to_string());
    }
    // Some compilers create the output without the exec bit when writing a
    // fresh file; force it so the rename only ever publishes runnable code.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let res = std::fs::metadata(&tmp)
            .and_then(|m| {
                let mut perms = m.permissions();
                perms.set_mode(perms.mode() | 0o111);
                std::fs::set_permissions(&tmp, perms)
            })
            .map_err(|e| format!("cannot set exec bit: {e}"));
        if let Err(e) = res {
            let _ = std::fs::remove_file(&tmp);
            return Err(e);
        }
    }
    if let Err(e) = std::fs::rename(&tmp, &cached) {
        let _ = std::fs::remove_file(&tmp);
        return Err(format!("cannot publish cache entry: {e}"));
    }
    publish_to_bin(&cached, path, target, output.as_deref())
}

/// Copy a cached binary to the authoritative [`planned_dest_for`]
/// destination. Returns the destination path.
///
/// The copy goes through a unique temp file in the same directory plus an
/// atomic rename: parallel `zz run --native` / `zz build` invocations for
/// the same fixture (e.g. `cargo test --all` running several test binaries
/// at once) must never observe — or execute — a half-written binary.
fn publish_to_bin(
    cached: &Path,
    src: &Path,
    target: Option<&str>,
    output: Option<&Path>,
) -> Result<PathBuf, String> {
    let dest = planned_dest_for(src, output, target);
    if let Some(parent) = dest.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("cannot create output dir: {e}"))?;
        }
    }
    static PUBLISH_COUNTER: AtomicU64 = AtomicU64::new(0);
    let tmp_dir = dest.parent().filter(|p| !p.as_os_str().is_empty());
    let tmp = tmp_dir.unwrap_or(std::path::Path::new(".")).join(format!(
        ".{}.publish-{}.{}.tmp",
        dest.file_name().unwrap_or_default().to_string_lossy(),
        std::process::id(),
        PUBLISH_COUNTER.fetch_add(1, Ordering::SeqCst)
    ));
    let res: Result<(), String> = (|| {
        std::fs::copy(cached, &tmp).map_err(|e| format!("cannot write binary: {e}"))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if let Ok(meta) = std::fs::metadata(&tmp) {
                let mut perms = meta.permissions();
                perms.set_mode(perms.mode() | 0o111);
                let _ = std::fs::set_permissions(&tmp, perms);
            }
        }
        // Windows `rename` cannot replace an existing file — drop the old
        // binary first (a concurrent executor there holds its own handle).
        #[cfg(windows)]
        {
            let _ = std::fs::remove_file(&dest);
        }
        std::fs::rename(&tmp, &dest).map_err(|e| format!("cannot publish binary: {e}"))?;
        Ok(())
    })();
    if res.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    // Native-runtime sidecars (`libstd-*` staged next to the built binary
    // by the codegen link step): carry them next to the published binary
    // so `$ORIGIN` rpath resolves wherever `-o` put it.
    if res.is_ok() {
        if let (Some(from_dir), Some(to_dir)) = (cached.parent(), dest.parent()) {
            if let Ok(entries) = std::fs::read_dir(from_dir) {
                for entry in entries.flatten() {
                    let name = entry.file_name().to_string_lossy().into_owned();
                    let is_sidecar = name.starts_with("libstd-")
                        && (name.ends_with(".so") || name.ends_with(".dylib"));
                    if is_sidecar && !to_dir.join(&name).is_file() {
                        let _ = std::fs::copy(entry.path(), to_dir.join(&name));
                    }
                }
            }
        }
        reap_legacy_twin(src, &dest);
    }
    res.map(|()| dest)
}

/// Migration: drop a byte-identical legacy twin (`src/bin/<name>`, where
/// pre-redesign publishes landed) so upgrades never accumulate
/// duplicates. Only byte-identical files are ever removed — anything
/// else is left alone, and the empty legacy dir goes with it.
fn reap_legacy_twin(src: &Path, dest: &Path) {
    let Some(name) = dest.file_name() else {
        return;
    };
    let Some(parent) = src.parent() else {
        return;
    };
    if parent.as_os_str().is_empty() {
        return;
    }
    let legacy = parent.join("bin").join(name);
    if same_file(&legacy, dest) || !legacy.is_file() {
        return;
    }
    if files_identical(&legacy, dest) {
        let _ = std::fs::remove_file(&legacy);
        if let Some(dir) = legacy.parent() {
            let _ = std::fs::remove_dir(dir);
        }
    }
}

/// Byte-identical files (size fast-path, then full read). Best effort:
/// unreadable sides compare unequal.
fn files_identical(a: &Path, b: &Path) -> bool {
    let meta = match (std::fs::metadata(a), std::fs::metadata(b)) {
        (Ok(x), Ok(y)) => (x, y),
        _ => return false,
    };
    if meta.0.len() != meta.1.len() {
        return false;
    }
    match (std::fs::read(a), std::fs::read(b)) {
        (Ok(x), Ok(y)) => x == y,
        _ => false,
    }
}

/// Stage a published binary for execution under a unique temp path plus
/// neighboring `libstd-*` sidecars (same `$ORIGIN` rule as publishing).
///
/// Concurrent `run --native` invocations for same-named sources share
/// one published destination; executing a private copy means a parallel
/// publish can never swap the binary mid-exec. Returns the staged path
/// and a best-effort cleanup closure.
#[allow(clippy::type_complexity)]
pub fn stage_exec_copy(built: &Path) -> Result<(PathBuf, Box<dyn FnOnce()>), String> {
    static STAGE_COUNTER: AtomicU64 = AtomicU64::new(0);
    let dir = std::env::temp_dir().join(format!(
        "zz-native-exec-{}-{}",
        std::process::id(),
        STAGE_COUNTER.fetch_add(1, Ordering::SeqCst)
    ));
    std::fs::create_dir_all(&dir).map_err(|e| format!("cannot create tmp: {e}"))?;
    let name = built
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "zz_out".to_string());
    let staged = dir.join(&name);
    std::fs::copy(built, &staged).map_err(|e| format!("cannot stage binary: {e}"))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Ok(meta) = std::fs::metadata(&staged) {
            let mut perms = meta.permissions();
            perms.set_mode(perms.mode() | 0o111);
            let _ = std::fs::set_permissions(&staged, perms);
        }
    }
    if let Some(from_dir) = built.parent() {
        if let Ok(entries) = std::fs::read_dir(from_dir) {
            for entry in entries.flatten() {
                let side = entry.file_name().to_string_lossy().into_owned();
                let is_sidecar = side.starts_with("libstd-")
                    && (side.ends_with(".so") || side.ends_with(".dylib"));
                if is_sidecar {
                    let _ = std::fs::copy(entry.path(), dir.join(&side));
                }
            }
        }
    }
    Ok((
        staged.clone(),
        Box::new(move || {
            let _ = std::fs::remove_dir_all(&dir);
        }),
    ))
}

/// Build a native binary for `path` in release `mode` with default options.
/// Convenience wrapper over [`build_release`] (native host, auto provider).
/// Returns the authoritative output binary path.
#[allow(dead_code)]
pub fn build_native(path: &Path, mode: BuildMode) -> Result<PathBuf, String> {
    build_release(path, mode, &ReleaseOptions::default())
}

/// Transient: compile to a temp path (release opts), return (binary path,
/// cleanup fn). Caller must invoke the closure to remove the artifact.
#[allow(dead_code, clippy::type_complexity)]
pub fn transient_build(path: &Path) -> Result<(PathBuf, Box<dyn FnOnce()>), String> {
    let entry_ns = path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let (pruned, reach, main_key) = typed_program_for(path, &entry_ns)?;

    let tmp = std::env::temp_dir().join(format!("zz-native-{}", std::process::id()));
    std::fs::create_dir_all(&tmp).map_err(|e| format!("cannot create tmp: {e}"))?;
    let bin = tmp.join("zz_out");
    zz_codegen::build_native(
        &pruned,
        &reach,
        &main_key,
        BuildOptions::release(),
        None,
        &bin,
    )
    .map_err(|e| format!("{e}"))?;

    let tmp_for_cleanup = tmp.clone();
    let cleanup = Box::new(move || {
        let _ = std::fs::remove_dir_all(&tmp_for_cleanup);
    });
    Ok((bin, cleanup))
}

/// Execute a binary, forwarding args; waits for completion.
pub fn exec_binary(bin: &Path, script_args: &[String]) -> Result<i32, String> {
    let status = Command::new(bin)
        .args(script_args)
        .stdin(Stdio::inherit())
        .status()
        .map_err(|e| format!("cannot run binary: {e}"))?;
    if let Some(code) = status.code() {
        return Ok(code);
    }
    // Killed by signal (Unix): name it. The old bare `-1` hid segfaults
    // (e.g. every `http.listen` native binary died in _dl_fini with no
    // message and surfaced only as "code -1").
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        if let Some(sig) = status.signal() {
            return Err(format!(
                "native program killed by signal {sig} ({})",
                signal_name(sig)
            ));
        }
    }
    Ok(-1)
}

/// Short name for a Unix signal number (common cases; else "unknown").
#[cfg(unix)]
fn signal_name(sig: i32) -> &'static str {
    match sig {
        1 => "SIGHUP",
        2 => "SIGINT",
        3 => "SIGQUIT",
        4 => "SIGILL",
        6 => "SIGABRT",
        8 => "SIGFPE",
        9 => "SIGKILL",
        11 => "SIGSEGV",
        13 => "SIGPIPE",
        14 => "SIGALRM",
        15 => "SIGTERM",
        _ => "unknown signal",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bin_naming_host_and_cross() {
        // Host naming follows the cfg target (this suite runs on unix).
        let host = bin_name("app", None);
        assert_eq!(host, if cfg!(windows) { "app.exe" } else { "app" });
        // Cross builds tag the triple; Windows triples add .exe.
        assert_eq!(
            bin_name("app", Some("aarch64-unknown-linux-gnu")),
            "app-aarch64-unknown-linux-gnu"
        );
        assert_eq!(
            bin_name("app", Some("x86_64-pc-windows-gnu")),
            "app-x86_64-pc-windows-gnu.exe"
        );
        assert_eq!(
            bin_name("demo", Some("x86_64-apple-darwin")),
            "demo-x86_64-apple-darwin"
        );
    }

    #[test]
    fn output_dir_prefers_project_root_bin() {
        // Standalone: a path with no zz.toml ancestor resolves to the
        // invocation dir (never a `bin/` next to the source).
        let base = std::env::temp_dir().join(format!(
            "zz-output-plan-{}-{}",
            std::process::id(),
            "standalone"
        ));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        let src = base.join("no-such-file.zz");
        let cwd = std::env::current_dir().unwrap();
        assert_eq!(output_dir_for(&src), cwd);
        assert_eq!(
            planned_dest_for(&src, None, None),
            cwd.join(bin_name("no-such-file", None))
        );
        // Project-owned source: `<root>/bin`, package-named default stem.
        let root = base.join("proj");
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(
            root.join("zz.toml"),
            "[package]\nname = \"demo-pkg\"\nversion = \"0.1.0\"\n",
        )
        .unwrap();
        let entry = root.join("src").join("main.zz");
        std::fs::write(&entry, "println(\"hi\")\n").unwrap();
        assert_eq!(output_dir_for(&entry), root.join("bin"));
        assert_eq!(default_stem_for(&entry), "demo-pkg");
        assert_eq!(
            planned_dest_for(&entry, None, None),
            root.join("bin").join(bin_name("demo-pkg", None))
        );
        // Explicit non-entry file in a project: project `bin/` + stem.
        let other = root.join("src").join("tool.zz");
        std::fs::write(&other, "println(\"tool\")\n").unwrap();
        assert_eq!(
            planned_dest_for(&other, None, None),
            root.join("bin").join(bin_name("tool", None))
        );
        // Bare `-o` stays in the resolved dir; path-like `-o` is CWD-exact.
        assert_eq!(
            planned_dest_for(&entry, Some(Path::new("server")), None),
            root.join("bin").join(bin_name("server", None))
        );
        assert_eq!(
            planned_dest_for(&entry, Some(Path::new("out/server")), None),
            PathBuf::from("out/server")
        );
        // Cross targets tag the triple (Windows triples add .exe) in
        // every destination shape — exercised on any host.
        let win = Some("x86_64-pc-windows-gnu");
        assert_eq!(
            planned_dest_for(&entry, None, win),
            root.join("bin").join("demo-pkg-x86_64-pc-windows-gnu.exe")
        );
        assert_eq!(
            planned_dest_for(&src, None, win),
            cwd.join("no-such-file-x86_64-pc-windows-gnu.exe")
        );
        assert_eq!(
            planned_dest_for(&entry, Some(Path::new("out/srv")), win),
            PathBuf::from("out/srv.exe")
        );
        // Configured `[package] entry` wins over the conventional one —
        // for resolution, naming, and destination alike.
        let cfg = base.join("cfgproj");
        std::fs::create_dir_all(cfg.join("src")).unwrap();
        std::fs::write(
            cfg.join("zz.toml"),
            "[package]\nname = \"cfg-pkg\"\nversion = \"0.1.0\"\nentry = \"src/cli.zz\"\n",
        )
        .unwrap();
        let cli = cfg.join("src").join("cli.zz");
        std::fs::write(&cli, "println(\"hi\")\n").unwrap();
        assert_eq!(resolve_entry(&cfg).unwrap(), cli);
        assert_eq!(default_stem_for(&cli), "cfg-pkg");
        assert_eq!(
            planned_dest_for(&cli, None, None),
            cfg.join("bin").join(bin_name("cfg-pkg", None))
        );
        // A configured-but-missing entry is a loud error, not a silent
        // conventional fallback.
        std::fs::write(
            cfg.join("zz.toml"),
            "[package]\nname = \"cfg-pkg\"\nversion = \"0.1.0\"\nentry = \"src/gone.zz\"\n",
        )
        .unwrap();
        let err = resolve_entry(&cfg).expect_err("missing configured entry must fail");
        assert!(err.contains("does not exist"), "got:\n{err}");
        let _ = std::fs::remove_dir_all(&base);
    }
}
