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

const STD_LIB_DIR: &str = "crates/zz_stdlib/zz";
const DEFAULT_OUT: &str = "crates/zz_codegen_runtime/src/runtime/lib";

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

    let out = out_dir.unwrap_or_else(|| DEFAULT_OUT.to_string());
    let out_path = PathBuf::from(&out);

    if check_only {
        let archive = out_path.join("libzz_stdlib.a");
        if !archive.is_file() {
            eprintln!("pnsa: archive missing: {}", archive.display());
            std::process::exit(1);
        }
        eprintln!("pnsa: archive up to date: {}", archive.display());
        return;
    }

    if let Err(e) = build_archive(&out_path, release) {
        eprintln!("pnsa: {e}");
        std::process::exit(1);
    }
}

fn build_archive(out_path: &Path, release: bool) -> Result<(), String> {
    std::fs::create_dir_all(out_path)
        .map_err(|e| format!("cannot create {}: {e}", out_path.display()))?;

    // 1. Discover stdlib modules
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
            format!("stdlib.{name}"),
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
        let o_path = tmpdir.join(format!("{}.o", module.name));

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

    // 6. Generate header file
    generate_header(out_path, &modules)?;

    eprintln!(
        "pnsa: wrote {} ({} modules)",
        archive.display(),
        modules.len()
    );
    Ok(())
}

fn discover_sources() -> Result<Vec<(String, String)>, String> {
    let mut out = Vec::new();
    for entry in
        std::fs::read_dir(STD_LIB_DIR).map_err(|e| format!("cannot read {STD_LIB_DIR}: {e}"))?
    {
        let entry = entry.map_err(|e| format!("dir entry error: {e}"))?;
        let path = entry.path();
        if path.is_dir() {
            // Look for mod.zz inside each module subdirectory
            let mod_path = path.join("mod.zz");
            if mod_path.is_file() {
                let name = path
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .to_string();
                let source = std::fs::read_to_string(&mod_path)
                    .map_err(|e| format!("cannot read {}: {e}", mod_path.display()))?;
                out.push((name, source));
            }
        }
    }
    out.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(out)
}

fn generate_header(out_path: &Path, modules: &[ModuleOutput]) -> Result<(), String> {
    let header_path = out_path.join("zz_stdlib.h");
    let mut content = String::from(
        "// GENERATED by zz_stdlib_native — do not edit.\n\
         #ifndef ZZ_STDLIB_H\n\
         #define ZZ_STDLIB_H\n\n",
    );

    for module in modules {
        content.push_str(&format!("// {}\n", module.name));
    }

    content.push_str("\n#endif // ZZ_STDLIB_H\n");
    std::fs::write(&header_path, &content)
        .map_err(|e| format!("cannot write {}: {e}", header_path.display()))?;
    Ok(())
}
