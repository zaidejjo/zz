//! ZZ native codegen backend (Phase 3).
//!
//! Consumes the DCE-pruned typed program from [`zz_hir`], lowers it to C,
//! embeds the C runtime, and invokes a C compiler to produce a standalone
//! binary. `cc`/`clang`/`gcc` are auto-detected; NO manual tooling install
//! is required on standard systems (they ship with the OS toolchain).

pub mod cache;
pub mod chunk;
pub mod compile;
pub mod ffi;
pub mod lower;

pub use chunk::{build_module as build_chunk_module, coverage as chunk_coverage, ChunkError};
pub use compile::{
    compile_and_run, compile_and_run_for_target, detect_clang, detect_clang_with,
    emit_c_plus_script, host_triple, is_macos_target, is_windows_target, validate, BuildError,
    BuildOptions, Clang, ClangProvider, EmbedAsset, PgoMode,
};
pub use ffi::{ffi_impl, FfiError, FFI_VERSION};
pub use lower::{mangle, native_supported, units, LoweredC, Lowerer};

/// The embedded C runtime header. The runtime is split across modular
/// sub-headers under `src/runtime/`; the umbrella `runtime.h` includes them
/// in dependency order, and they are concatenated here into one string.
pub const RUNTIME_H: &str = concat!(
    include_str!("runtime/runtime.h"),
    include_str!("runtime/platform.h"),
    include_str!("runtime/core.h"),
    include_str!("runtime/memory.h"),
    include_str!("runtime/strings.h"),
    include_str!("runtime/collections.h"),
    include_str!("runtime/json.h"),
);

/// The embedded C runtime implementation. The modular `.c` files are
/// concatenated in dependency order into a single translation unit.
pub const RUNTIME_C: &str = concat!(
    include_str!("runtime/memory.c"),
    include_str!("runtime/strings.c"),
    include_str!("runtime/collections.c"),
    include_str!("runtime/json.c"),
    include_str!("runtime/core.c"),
);

/// Version marker for generated binaries.
pub const C_BUILD_VERSION: &str = "0.1.0";

/// Lower a pruned typed program to C without compiling.
///
/// Used by dev builds (`zz build` default): the C is dumped to `bin/app.c`
/// for inspection while execution stays on the VM — no toolchain involved.
pub fn lower_only(
    tp: &zz_hir::TypedProgram,
    reach: &zz_hir::ReachableSet,
    entry_main: &str,
) -> LoweredC {
    // Only emit reachable natives for which we have a C impl; the rest are
    // pruned by DCE anyway. If a reachable native lacks a C impl, lower to
    // unit (documented limitation of the MVP runtime).
    let lowerer = Lowerer::new(
        reach.funcs.clone(),
        reach.natives.clone(),
        entry_main.to_string(),
        tp.clone(),
    );
    lowerer.lower()
}

/// Build a native binary from a pruned typed program.
///
/// `target` is the `--target=<triple>` cross triple, or `None` for a
/// native host build. Returns the generated C source (for
/// tests/inspection) and the path to the compiled binary.
pub fn build_native(
    tp: &zz_hir::TypedProgram,
    reach: &zz_hir::ReachableSet,
    entry_main: &str,
    opts: BuildOptions,
    target: Option<&str>,
    out_path: &std::path::Path,
) -> Result<LoweredC, BuildError> {
    let verbose = opts.verbose;
    let t_lower = std::time::Instant::now();
    let mut lowerer = Lowerer::new(
        reach.funcs.clone(),
        reach.natives.clone(),
        entry_main.to_string(),
        tp.clone(),
    );
    lowerer.set_precompiled(true);
    let lowered = lowerer.lower();
    if verbose {
        eprintln!("zz: timing lower={}ms", t_lower.elapsed().as_millis());
    }
    // Programs calling Rust-staticlib natives need the native runtime link
    // even when the caller did not opt in explicitly.
    let mut opts = opts;
    opts.native_rt = opts.native_rt || lowered.needs_native_rt;
    opts.pg_link = opts.pg_link || lowered.needs_pg_link;
    opts.float_link = opts.float_link || lowered.needs_float_fmt;
    opts.curl_link = opts.curl_link || lowered.needs_curl;
    opts.sqlite_link = opts.sqlite_link || lowered.needs_sqlite;
    let t_cc = std::time::Instant::now();
    compile::build(&lowered.source, out_path, opts, target)?;
    if verbose {
        eprintln!("zz: timing clang_link={}ms", t_cc.elapsed().as_millis());
    }
    Ok(lowered)
}

/// Per-module native build: lower to namespaced translation units and
/// compile them (serially for now) against the precompiled runtime archive.
/// `--embed` tables ride in the entry unit (emitted once, never in the
/// shared header). Returns the entry-unit index for callers that need it.
pub fn build_native_units_with(
    tp: &zz_hir::TypedProgram,
    reach: &zz_hir::ReachableSet,
    entry_main: &str,
    opts: BuildOptions,
    target: Option<&str>,
    clang: &compile::Clang,
    out_path: &std::path::Path,
) -> Result<units::LoweredUnits, compile::BuildError> {
    // Entry namespace derives from the dotted main key (`main.main` ->
    // `main`); bare test mains land in the prelude bucket with bare fns.
    let entry_ns = units::ns_of(entry_main).to_string();
    let verbose = opts.verbose;
    let t_lower = std::time::Instant::now();
    let mut lowerer = Lowerer::new(
        reach.funcs.clone(),
        reach.natives.clone(),
        entry_main.to_string(),
        tp.clone(),
    );
    lowerer.set_precompiled(true);
    let mut lowered = lowerer.lower_units(&entry_ns);
    if verbose {
        eprintln!("zz: timing lower_units={}ms", t_lower.elapsed().as_millis());
    }
    let mut opts = opts;
    opts.native_rt = opts.native_rt || lowered.needs_native_rt;
    opts.pg_link = opts.pg_link || lowered.needs_pg_link;
    opts.float_link = opts.float_link || lowered.needs_float_fmt;
    opts.curl_link = opts.curl_link || lowered.needs_curl;
    opts.sqlite_link = opts.sqlite_link || lowered.needs_sqlite;
    if !opts.embed_assets.is_empty() {
        let tables = compile::embed_c(&opts.embed_assets);
        match lowered.units.iter_mut().find(|u| u.ns == lowered.entry_ns) {
            Some(u) => {
                u.code.push_str(&tables);
            }
            None => {
                lowered.units.push(units::CodeUnit {
                    ns: lowered.entry_ns.clone(),
                    code: tables,
                });
            }
        }
    }
    let pairs: Vec<(String, String)> = lowered
        .units
        .iter()
        .map(|u| (u.ns.clone(), u.code.clone()))
        .collect();
    let t_cc = std::time::Instant::now();
    compile::compile_units(&lowered.header, &pairs, out_path, &opts, target, clang)?;
    if verbose {
        eprintln!("zz: timing units_cc_link={}ms", t_cc.elapsed().as_millis());
    }
    Ok(lowered)
}

/// [`build_native`] with an explicit Clang provider (honors `--cc`).
pub fn build_native_with(
    tp: &zz_hir::TypedProgram,
    reach: &zz_hir::ReachableSet,
    entry_main: &str,
    opts: BuildOptions,
    target: Option<&str>,
    clang: &Clang,
    out_path: &std::path::Path,
) -> Result<LoweredC, BuildError> {
    let verbose = opts.verbose;
    let t_lower = std::time::Instant::now();
    let mut lowerer = Lowerer::new(
        reach.funcs.clone(),
        reach.natives.clone(),
        entry_main.to_string(),
        tp.clone(),
    );
    lowerer.set_precompiled(true);
    let lowered = lowerer.lower();
    if verbose {
        eprintln!("zz: timing lower={}ms", t_lower.elapsed().as_millis());
    }
    let mut opts = opts;
    opts.native_rt = opts.native_rt || lowered.needs_native_rt;
    opts.pg_link = opts.pg_link || lowered.needs_pg_link;
    opts.float_link = opts.float_link || lowered.needs_float_fmt;
    opts.curl_link = opts.curl_link || lowered.needs_curl;
    opts.sqlite_link = opts.sqlite_link || lowered.needs_sqlite;
    let t_cc = std::time::Instant::now();
    compile::build_with(&lowered.source, out_path, opts, target, clang)?;
    if verbose {
        eprintln!("zz: timing clang_link={}ms", t_cc.elapsed().as_millis());
    }
    Ok(lowered)
}

#[cfg(test)]
mod tests;
