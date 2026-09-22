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

/// The cache directory (`~/.zz/cache`).
pub fn cache_dir() -> PathBuf {
    let home = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    home.join(".zz").join("cache")
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
    Ok(key.to_slug())
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
    }
}

/// Discover plugin manifests (`.zzi` files) from installed dependencies.
///
/// Reads `zz.lock` and `zz.toml` from the project directory, looks up each
/// dependency's package directory (CAS for git deps, `vendor/<name>/` for path
/// deps), and loads any `plugin.zzi` file found. Returns a flat list of
/// `(function_name, FuncSig)` pairs ready to merge into the checker.
pub(crate) fn discover_plugin_manifests(project_path: &Path) -> Vec<(String, zz_checker::FuncSig)> {
    let mut plugin_funcs = Vec::new();

    // Walk up from the source file to find the project root (has zz.lock)
    let mut dir = project_path.parent().unwrap_or(project_path);
    let project_root = loop {
        if dir.join("zz.lock").exists() {
            break dir.to_path_buf();
        }
        dir = match dir.parent() {
            Some(d) => d,
            None => return plugin_funcs,
        };
    };

    let lock = match zz_pm::lock::Lockfile::load(&project_root.join("zz.lock")) {
        Ok(l) => l,
        Err(_) => return plugin_funcs,
    };

    // Load manifest to check for path deps
    let manifest_path = project_root.join("zz.toml");
    let manifest = zz_pm::manifest::Manifest::load(&manifest_path).ok();

    for dep in &lock.deps {
        // Resolve package directory: path deps use vendor/<name>/, git deps use CAS
        let pkg_dir = if dep.source == "path" {
            if let Some(ref m) = manifest {
                if let Some(zz_pm::manifest::DepSpec::Path(ref path_dep)) =
                    m.dependencies.get(&dep.name)
                {
                    project_root.join(&path_dep.path)
                } else {
                    continue;
                }
            } else {
                continue;
            }
        } else {
            zz_pm::paths::cas_entry(&dep.hash)
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

/// Discover and invoke build hooks for plugin packages with native code.
///
/// Reads `zz.lock` and `zz.toml`, finds dependencies that have a `plugin.zzi`
/// and a `zz.toml` with a `[native]` section, invokes their build hooks, and
/// returns the paths to compiled artifacts (`.o` / `.a` files) plus the raw
/// linker flags from each package's `build/ldflags.txt` (e.g. `-lvips ...`).
fn discover_native_artifacts(project_path: &Path) -> (Vec<PathBuf>, Vec<String>) {
    let mut artifacts = Vec::new();
    let mut link_args: Vec<String> = Vec::new();

    // Walk up from the source file to find the project root (has zz.lock)
    let mut dir = project_path.parent().unwrap_or(project_path);
    let project_root = loop {
        if dir.join("zz.lock").exists() {
            break dir.to_path_buf();
        }
        dir = match dir.parent() {
            Some(d) => d,
            None => return (artifacts, link_args),
        };
    };

    let lock = match zz_pm::lock::Lockfile::load(&project_root.join("zz.lock")) {
        Ok(l) => l,
        Err(_) => return (artifacts, link_args),
    };

    // Load manifest to check for path deps
    let manifest_path = project_root.join("zz.toml");
    let manifest = zz_pm::manifest::Manifest::load(&manifest_path).ok();

    for dep in &lock.deps {
        // Resolve package directory: path deps use the local path, git deps use CAS
        let pkg_dir = if dep.source == "path" {
            if let Some(ref m) = manifest {
                if let Some(zz_pm::manifest::DepSpec::Path(ref path_dep)) =
                    m.dependencies.get(&dep.name)
                {
                    project_root.join(&path_dep.path)
                } else {
                    continue;
                }
            } else {
                continue;
            }
        } else {
            zz_pm::paths::cas_entry(&dep.hash)
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

        // Invoke the build hook
        let build_script = pkg_dir.join(&native.build);
        if !build_script.exists() {
            eprintln!(
                "warning: plugin `{}` build script not found: {}",
                dep.name,
                build_script.display()
            );
            continue;
        }

        eprintln!("zz: building plugin `{}` via {}...", dep.name, native.build);
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
            // Raw linker flags emitted by the build hook (e.g. `-lvips ...`).
            let ldflags_path = build_dir.join("ldflags.txt");
            if let Ok(flags) = std::fs::read_to_string(&ldflags_path) {
                for flag in flags.split_whitespace() {
                    if !link_args.iter().any(|f| f == flag) {
                        link_args.push(flag.to_string());
                    }
                }
            }
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

    (artifacts, link_args)
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
        loaded.funcs.clone(),
        loaded.structs.clone(),
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

/// Directory holding build artifacts: `bin/` next to the source file
/// (or `<cwd>/bin` when the source has no parent).
pub fn bin_dir_for(src: &Path) -> PathBuf {
    src.parent()
        .map(|p| {
            if p.as_os_str().is_empty() {
                PathBuf::from("bin")
            } else {
                p.join("bin")
            }
        })
        .unwrap_or_else(|| PathBuf::from("bin"))
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

/// Native Clang build (cached), published to `bin/` next to the source.
/// Returns the output binary path. `Dev` mode compiles with `-O0 -g`
/// (fast debug binary); `Release`/`Static`/`Pgo` use their option sets.
///
/// When no Clang provider is installed, the generated C + build scripts
/// are still emitted to `bin/` before the error is returned, so the user
/// can build manually on a machine with Clang.
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
    let mut opts = opts_for(mode);
    let target = rel.target_opt();

    // Discover and build plugin native artifacts (compiled .o / .a files
    // plus dependency link flags from each package's ldflags.txt).
    let (plugin_artifacts, plugin_link_args) = discover_native_artifacts(path);
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
    // still leave bin/app.c + scripts behind for manual builds.
    let clang = match zz_codegen::detect_clang_with(rel.provider) {
        Some(c) => c,
        None => {
            let lowered = zz_codegen::lower_only(&pruned, &reach, &main_key);
            let dir = bin_dir_for(path);
            let _ = zz_codegen::emit_c_plus_script(&lowered.source, &dir, target, &opts);
            return Err(zz_codegen::BuildError::NoClang.to_string());
        }
    };
    if rel.verbose {
        eprintln!(
            "zz: {} {}",
            clang.label,
            zz_codegen::compile::clang_flags(&opts, target).join(" ")
        );
    }

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
        dir.join(format!("{key}-{mode:?}-{target_slug}"))
    } else {
        dir.join(format!("{key}-{mode:?}-{target_slug}-{embed_slug}"))
    };

    if is_usable_cache_binary(&cached) {
        // Reuse the cached binary.
        return publish_to_bin(&cached, path, target);
    }
    // Stale artifact (interrupted build, missing exec bit, empty file):
    // drop it so the fresh build below replaces it.
    let _ = std::fs::remove_file(&cached);

    // Build to a unique temp path in the same directory, then atomically
    // rename into place. Concurrent builds of the same key (parallel tests,
    // parallel `zz` invocations) each produce a complete, executable file;
    // observers never see a partially-written or non-executable binary.
    static TMP_COUNTER: AtomicU64 = AtomicU64::new(0);
    let tmp = dir.join(format!(
        "{key}-{mode:?}-{target_slug}.{}.{}.tmp",
        std::process::id(),
        TMP_COUNTER.fetch_add(1, Ordering::SeqCst)
    ));
    if let Err(e) =
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
    publish_to_bin(&cached, path, target)
}

/// Copy a cached binary into `bin/` next to the source with the
/// target-aware name. Returns the `bin/` path.
fn publish_to_bin(cached: &Path, src: &Path, target: Option<&str>) -> Result<PathBuf, String> {
    let stem = src
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "app".to_string());
    let dir = bin_dir_for(src);
    std::fs::create_dir_all(&dir).map_err(|e| format!("cannot create bin dir: {e}"))?;
    let dest = dir.join(bin_name(&stem, target));
    std::fs::copy(cached, &dest).map_err(|e| format!("cannot write binary: {e}"))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Ok(meta) = std::fs::metadata(&dest) {
            let mut perms = meta.permissions();
            perms.set_mode(perms.mode() | 0o111);
            let _ = std::fs::set_permissions(&dest, perms);
        }
    }
    Ok(dest)
}

/// Build a native binary for `path` in release `mode` with default options.
/// Convenience wrapper over [`build_release`] (native host, auto provider).
/// Returns the `bin/` output binary path.
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
    Ok(status.code().unwrap_or(-1))
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
    fn bin_dir_is_pathbuf_joined() {
        // Never string-concatenated separators: always parent + "bin".
        assert_eq!(
            bin_dir_for(Path::new("src/main.zz")),
            PathBuf::from("src/bin")
        );
        assert_eq!(bin_dir_for(Path::new("main.zz")), PathBuf::from("bin"));
    }
}
