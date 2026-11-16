//! `zz_stdlib_native` — precompiles ZZ stdlib `.zz` sources to native objects
//! and archives them into `libzz_stdlib.a`, embedded in the `zz` compiler
//! binary and linked into every AOT build.
//!
//! Pipeline per module:
//!   parse .zz → type-check against stdlib signature table → lower to C
//!   (zz_lower) → clang -O3 → .o → ar → libzz_stdlib.a
//!
//! Usage:
//!   cargo run -p zz_stdlib_native [--release] [--out <dir>] [--check]
//!
//! With `--check`, verifies that the archive is up to date (CI drift gate).
//! Without it, writes `libzz_stdlib.a` and `zz_stdlib.h` to `--out`
//! (default: `crates/zz_codegen_runtime/src/runtime/lib/`).

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::process::Command;

use zz_codegen_cc::{clang_flags, detect_clang, BuildOptions};
use zz_codegen_ffi::ffi_impl;
use zz_frontend::parse;
use zz_hir::build_program;
use zz_lower::Lowerer;
use zz_stdlib::funcs::stdlib_funcs_cached;

fn std_lib_dir() -> PathBuf {
    Path::new(option_env!("CARGO_MANIFEST_DIR").unwrap_or("crates/zz_stdlib_native"))
        .join("../zz_stdlib/zz")
}
fn default_out() -> PathBuf {
    Path::new(option_env!("CARGO_MANIFEST_DIR").unwrap_or("crates/zz_stdlib_native"))
        .parent()
        .unwrap_or(Path::new(""))
        .join(DEFAULT_OUT_REL)
}

const DEFAULT_OUT_REL: &str = "zz_codegen_runtime/src/runtime/lib";
const MANIFEST_NAME: &str = "zz_stdlib.manifest";

struct ModuleOutput {
    name: String,
    c_path: PathBuf,
    object: PathBuf,
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let mut out_dir = None;
    let mut check_only = false;
    let mut release = false;

    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--out" => {
                i += 1;
                out_dir = Some(args[i].clone());
            }
            "--check" => check_only = true,
            "--release" => release = true,
            _ => {
                eprintln!("unknown argument: {}", args[i]);
                std::process::exit(2);
            }
        }
        i += 1;
    }

    let out = out_dir.unwrap_or_else(|| default_out().to_string_lossy().to_string());
    let out_path = PathBuf::from(&out);

    if check_only {
        if let Err(e) = check_archive(&out_path) {
            eprintln!("pnsa: {e}");
            std::process::exit(1);
        }
        return;
    }

    if let Err(e) = build_archive(&out_path, release) {
        eprintln!("pnsa: {e}");
        std::process::exit(1);
    }
}

/// `--check`: verify the precompiled archive is fresh for the sources on disk.
///
/// Recomputes a content hash for every discovered stdlib source and compares
/// it to the stored manifest. Exits non-zero if the archive is missing or any
/// source drifted — CI's drift gate for `libzz_stdlib.a`.
fn check_archive(out_path: &Path) -> Result<(), String> {
    let archive = out_path.join("libzz_stdlib.a");
    let manifest = out_path.join(MANIFEST_NAME);
    let sources = discover_sources()?;
    if !archive.is_file() {
        eprintln!("pnsa: archive missing: {}", archive.display());
        std::process::exit(1);
    }
    let stored = std::fs::read_to_string(&manifest)
        .map_err(|e| format!("cannot read manifest {}: {e}", manifest.display()))?;
    let stored = parse_manifest(&stored);
    let current: std::collections::HashMap<String, u64> = sources
        .iter()
        .map(|(n, s)| (n.clone(), content_hash(s.as_bytes())))
        .collect();
    if stored != current {
        eprintln!(
            "pnsa: archive is stale (stdlib sources drifted); re-run `cargo run -p zz_stdlib_native` without --check"
        );
        std::process::exit(1);
    }
    eprintln!("pnsa: archive up to date: {}", archive.display());
    Ok(())
}

fn build_archive(out_path: &Path, release: bool) -> Result<(), String> {
    std::fs::create_dir_all(out_path)
        .map_err(|e| format!("cannot create {}: {e}", out_path.display()))?;

    // 1. Discover stdlib modules (single source of truth: disk, synced with
    //    `zz_stdlib::ZZ_SOURCES`).
    let sources = discover_sources()?;
    if sources.is_empty() {
        return Err("no stdlib .zz sources found".into());
    }

    // 2. Build the signature table for type-checking
    let sigs = stdlib_funcs_cached().clone();

    // 3. Parse + type-check + lower each module to C
    let mut modules = Vec::new();
    let tmpdir = std::env::temp_dir().join(format!("zz-stdlib-native-{}", std::process::id()));
    std::fs::create_dir_all(&tmpdir).map_err(|e| format!("cannot create tmpdir: {e}"))?;

    for (name, source) in &sources {
        let parsed = parse(source);
        if !parsed.errors.is_empty() {
            return Err(format!("parse errors in {name}: {:?}", parsed.errors));
        }
        let tp = build_program(
            &parsed.program,
            HashMap::new(),
            sigs.clone(),
            HashMap::new(),
            HashMap::new(),
            HashMap::new(),
            HashMap::new(),
            HashMap::new(),
        );
        if !tp.diagnostics.is_empty() {
            for d in &tp.diagnostics {
                if d.severity == zz_frontend::diag::Severity::Error {
                    eprintln!("pnsa: {name}: {}", d.message);
                }
            }
            let has_errors = tp
                .diagnostics
                .iter()
                .any(|d| d.severity == zz_frontend::diag::Severity::Error);
            if has_errors {
                return Err(format!("type-check errors in {name}"));
            }
        }

        // Collect all FFI native names so `zz_lower` emits their `extern`
        // declarations. C runtime symbols come from RUNTIME_C/RUNTIME_H.
        let ffi_natives: HashSet<String> = stdlib_funcs_cached()
            .keys()
            .filter(|k| ffi_impl(k).is_some())
            .cloned()
            .collect();

        // Only this module's functions are "local"; cross-module calls become
        // external C symbol calls resolved at link time from the archive.
        let local_funcs: HashSet<String> = tp.program.funcs.keys().cloned().collect();
        let mut lowerer = Lowerer::new(
            local_funcs,
            ffi_natives,
            format!("stdlib.{}", artifact_id(name)),
            tp.program.clone(),
        );
        // External linkage: stdlib functions must be callable from user
        // programs and other stdlib modules (cross-module calls resolve
        // at link time from the archive).
        lowerer.set_unit_mode(true);
        let lowered = lowerer.lower();

        let c_path = tmpdir.join(format!("{name}.c"));
        std::fs::write(&c_path, &lowered.source)
            .map_err(|e| format!("cannot write {}: {e}", c_path.display()))?;

        modules.push(ModuleOutput {
            name: name.clone(),
            c_path,
            object: PathBuf::new(),
        });
    }

    // 4. Compile each C module to .o
    let clang = detect_clang().ok_or("no clang available".to_string())?;
    let mut opts = if release {
        BuildOptions::release()
    } else {
        BuildOptions::dev()
    };
    // Stdlib modules are compiled WITHOUT ThinLTO: LTO internalization
    // happens at final link time (user binary), not per-module. Compiling
    // with -flto=thin here would internalize all stdlib functions (none
    // are called from each module's main), making them invisible to the
    // archive linker. -O3 alone is enough; cross-module inlining happens
    // when the user binary links with -flto=thin against this archive.
    opts.thin_lto = false;

    for module in &mut modules {
        let o_path = tmpdir.join(format!("{}.o", artifact_id(&module.name)));

        let mut cmd = Command::new(&clang.path);
        if clang.zig {
            cmd.arg("cc");
        }
        for flag in &clang_flags(&opts, None) {
            cmd.arg(flag);
        }
        cmd.arg("-c").arg(&module.c_path).arg("-o").arg(&o_path);

        let status = cmd
            .status()
            .map_err(|e| format!("clang spawn failed for {}: {e}", module.name))?;
        if !status.success() {
            return Err(format!("clang failed for {}", module.name));
        }
        module.object = o_path;
    }

    // 5. Archive into libzz_stdlib.a
    let archive = out_path.join("libzz_stdlib.a");
    // Start from a clean archive so stale members from a previous (differently
    // named) build can't linger and cause duplicate-symbol linkage.
    let _ = std::fs::remove_file(&archive);
    let mut ar_cmd = Command::new("ar");
    ar_cmd.arg("rcs").arg(&archive);
    for module in &modules {
        ar_cmd.arg(&module.object);
    }
    let status = ar_cmd
        .status()
        .map_err(|e| format!("ar spawn failed: {e}"))?;
    if !status.success() {
        return Err("ar failed".into());
    }

    // 6. Generate header file (+ content-hash manifest for `--check`)
    generate_header(out_path, &modules)?;
    let manifest = out_path.join(MANIFEST_NAME);
    std::fs::write(&manifest, manifest_text(&sources))
        .map_err(|e| format!("cannot write manifest: {e}"))?;

    eprintln!(
        "pnsa: wrote {} ({} modules)",
        archive.display(),
        modules.len()
    );
    Ok(())
}

/// Discover every pure-ZZ stdlib module from disk, keyed like `ZZ_SOURCES`
/// (`std:<path relative to `crates/zz_stdlib/zz/`>`).
///
/// This is the source of truth for what goes into `libzz_stdlib.a`. It must
/// stay in sync with [`crate::ZZ_SOURCES`] — see
/// [`discover_sources_matches_embedded_sources`].
fn discover_sources() -> Result<Vec<(String, String)>, String> {
    let root = std_lib_dir();
    let files = walk_zz(&root)?;
    let mut out = Vec::new();
    for path in files {
        let rel = path
            .strip_prefix(&root)
            .map_err(|e| format!("cannot relativize {}: {e}", path.display()))?
            .to_string_lossy()
            .replace('\\', "/");
        // Canonical module key matches `ZZ_SOURCES` spelling (`std:X.mod.zz`,
        // `std:collections.map.zz`) — slashes become dots so the discovered
        // set can be compared 1:1 with the embedded list.
        let name = format!("std:{}", rel.replace('/', "."));
        let source = std::fs::read_to_string(&path)
            .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
        out.push((name, source));
    }
    out.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(out)
}

/// Recursively collect every `.zz` file under `root`, sorted by path.
fn walk_zz(root: &Path) -> Result<Vec<PathBuf>, String> {
    let mut out = Vec::new();
    fn rec(dir: &Path, out: &mut Vec<PathBuf>) -> Result<(), String> {
        for entry in
            std::fs::read_dir(dir).map_err(|e| format!("cannot read {}: {e}", dir.display()))?
        {
            let entry = entry.map_err(|e| format!("dir entry error: {e}"))?;
            let path = entry.path();
            if path.is_dir() {
                rec(&path, out)?;
            } else if path.extension().map(|e| e == "zz").unwrap_or(false) {
                out.push(path);
            }
        }
        Ok(())
    }
    rec(root, &mut out)?;
    out.sort();
    Ok(out)
}

/// FNV-1a 64-bit content hash (deterministic across runs — safe for a stored
/// manifest used by `--check`).
fn content_hash(data: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325;
    for &b in data {
        h ^= b as u64;
        h = h.wrapping_mul(0x0100000001b3);
    }
    h
}

fn manifest_text(sources: &[(String, String)]) -> String {
    let mut lines: Vec<String> = sources
        .iter()
        .map(|(name, source)| format!("{:016x} {}", content_hash(source.as_bytes()), name))
        .collect();
    lines.sort();
    lines.join("\n") + "\n"
}

fn parse_manifest(text: &str) -> std::collections::HashMap<String, u64> {
    text.lines()
        .filter_map(|line| {
            let mut parts = line.splitn(2, ' ');
            let hash = u64::from_str_radix(parts.next()?, 16).ok()?;
            let name = parts.next()?;
            Some((name.to_string(), hash))
        })
        .collect()
}

/// Sanitize a canonical module key (`std:collections.map.zz`) into a
/// cwd-safe identifier for filesystem artifacts (`.o` path, entry namespace).
/// Colons are not portable in filenames; dots/underscores are fine.
fn artifact_id(name: &str) -> String {
    name.replace(":", "_")
}

fn generate_header(out_path: &Path, modules: &[ModuleOutput]) -> Result<(), String> {
    use std::collections::{BTreeMap, BTreeSet};
    use zz_codegen_types::mangle;

    let header_path = out_path.join("zz_stdlib.h");
    let mut content = String::from(
        "// GENERATED by zz_stdlib_native — do not edit.\n\
         #ifndef ZZ_STDLIB_H\n\
         #define ZZ_STDLIB_H\n\n\
         // Pure-ZZ stdlib functions, resolved from `libzz_stdlib.a` at link time.\n\
         // The AOT lowerer emits each call as `zz_fn_<mangled(name)>(\n\
         //   zz_value *args, size_t argc)` and the linker resolves it from\n\
         // the archive. FFI natives (with a C twin in the runtime) are not\n\
         // listed here.\n\n",
    );

    for module in modules {
        content.push_str(&format!("// {}\n", module.name));
    }

    // Extern declarations for every pure-ZZ stdlib function — exactly the
    // functions the archive defines (one `zz_fn_*` symbol each). The AOT
    // lowerer emits calls to `zz_fn_<mangle(name)>` and the linker resolves
    // them from `libzz_stdlib.a`. No `native_impl()` hand-edit is needed per
    // function; FFI natives keep their C twins in the runtime.
    let mut by_module: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for program in zz_stdlib::zz_stdlib_programs() {
        let tp = match program {
            Ok(tp) => tp,
            Err(_) => continue,
        };
        for stmt in &tp.program.stmts {
            if let zz_frontend::ast::Stmt::Func { name, .. } = stmt {
                let key = name.join(".");
                let module = key.split('.').next().unwrap_or(&key).to_string();
                by_module.entry(module).or_default().insert(key);
            }
        }
    }
    for (module, names) in &by_module {
        content.push_str(&format!("\n// {module}\n"));
        for name in names {
            content.push_str(&format!(
                "zz_value zz_fn_{}(zz_value *args, size_t argc);\n",
                mangle(name)
            ));
        }
    }

    content.push_str("\n#endif // ZZ_STDLIB_H\n");
    std::fs::write(&header_path, &content)
        .map_err(|e| format!("cannot write {}: {e}", header_path.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `discover_sources()` (disk scan) must stay in sync with the embedded
    /// `ZZ_SOURCES` list — both name set and source content. This is the
    /// guarantee that `libzz_stdlib.a` precompiles every stdlib module.
    #[test]
    fn discover_sources_matches_embedded_sources() {
        let discovered = discover_sources().expect("discover_sources should run");
        let embedded = zz_stdlib::zz_std::ZZ_SOURCES;
        let embedded_names: std::collections::HashSet<&str> =
            embedded.iter().map(|(n, _)| *n).collect();
        let discovered_names: std::collections::HashSet<&str> =
            discovered.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(
            discovered_names, embedded_names,
            "discover_sources names must equal ZZ_SOURCES names; add/remove \
             a .zz file under crates/zz_stdlib/zz/ and update both"
        );
        // Source content must match the embedded `include_str!` too, so a
        // disk edit can't silently diverge from what `zz_stdlib` compiles.
        let embedded_by_name: std::collections::HashMap<&str, &str> =
            embedded.iter().cloned().collect();
        for (name, source) in &discovered {
            let embedded_source = embedded_by_name
                .get(name.as_str())
                .unwrap_or_else(|| panic!("discovered {name} not in ZZ_SOURCES"));
            assert_eq!(
                source, *embedded_source,
                "source content for {name} diverged from ZZ_SOURCES"
            );
        }
        assert!(
            !discovered.is_empty(),
            "expected at least one stdlib module"
        );
    }
}
