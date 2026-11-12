//! ZZ native codegen facade.
//!
//! Consumes the DCE-pruned typed program from [`zz_hir`], lowers it to C,
//! embeds the C runtime, and invokes a C compiler to produce a standalone
//! binary. `cc`/`clang`/`gcc` are auto-detected; NO manual tooling install
//! is required on standard systems (they ship with the OS toolchain).
//!
//! The implementation is split into independently compileable crates and
//! re-exported here so downstream crates keep a single import surface:
//!
//! - [`zz_lower`]: HIR → C lowering
//! - [`zz_chunk`]: unified IR → C lowering (chunk backend)
//! - [`zz_codegen_cc`]: clang/gcc driver, flag matrix, runtime-archive cache
//! - [`zz_codegen_ffi`]: native + staticlib symbol tables, link metadata
//! - [`zz_codegen_runtime`]: embedded C runtime (single TU) + rebuild guard
//! - `zz_codegen_types`: build options, errors, compiler handles
//!
//! This crate keeps the orchestration (lower-only dumps, native/unit
//! builds) and the end-to-end pipeline tests.

pub use zz_chunk as chunk;
pub use zz_codegen_cc as compile;
pub use zz_codegen_cc::cache;
pub use zz_codegen_ffi as ffi;

pub use chunk::{build_module as build_chunk_module, coverage as chunk_coverage, ChunkError};
pub use compile::{
    build_options_fingerprint, build_options_fingerprint_with, compile_and_run,
    compile_and_run_for_target, detect_clang, detect_clang_with, emit_c_plus_script, host_triple,
    is_macos_target, is_windows_target, sanitize_list, validate, BuildError, BuildOptions, Clang,
    ClangProvider, EmbedAsset, PgoMode,
};
pub use ffi::{ffi_decl, ffi_impl, native_impl, native_supported, FfiError, FFI_VERSION};
pub use zz_codegen_runtime::{C_BUILD_VERSION, RUNTIME_C, RUNTIME_H, RUNTIME_SRC_HASH};
pub use zz_lower as lower;
pub use zz_lower::{mangle, units, LoweredC, Lowerer};

/// Progress sinks for the native pipeline (#629). Re-exported from the
/// zero-dependency types crate so `zz_build` (and the CLI) need only this
/// facade.
pub use zz_codegen_types::{CodegenSink, NoopSink};
/// Intentional, tracked VM-only gaps: stdlib funcs the AOT runtime does not
/// implement natively. The preflight in [`build_native_with`] /
/// [`build_native_units_with`] filters these out before failing, so
/// deliberate AOT exclusions (e.g. TLS listeners, the MySQL wire driver)
/// keep aborting at runtime on the VM instead of breaking every build that
/// merely *mentions* them. A gap that is no longer missing (the C impl
/// landed) must be removed here so the list cannot rot; the drift census
/// test [`all_stdlib_funcs_have_c_impls`] enforces this.
pub(crate) const KNOWN_CODEGEN_GAPS: &[(&str, &str)] = &[
    // Non-reachable from parity fixtures (no fixture uses them):
    // Pure-ZZ stdlib functions — run in VM only, no C codegen
    // Pure-ZZ regexp helpers — compiled from zz/regexp/mod.zz and merged
    // into AOT builds as ZZ functions (no C/FFI native needed).
    ("ArgsParser.new", "pure ZZ; lowered as ZZ fn"),
    // Phase 2/3 HTTP natives — deliberate AOT exclusions (VM-only). Each is
    // excluded because porting it to the C HTTP runtime needs infrastructure
    // that does not exist in AOT (rustls TLS, WebSocket handshake + TCP
    // handoff, insecure curl client). They abort loudly (`zz_unimplemented_native`)
    // on AOT — never silently lower to unit.
    // Closure transactions — inlined by the AOT codegen (BEGIN/COMMIT/ROLLBACK
    // around the closure body); no native C implementation needed.
    ("std.sqlz.transaction", "AOT-inlined; no native C impl"),
    ("sqlz.transaction", "AOT-inlined; no native C impl"),
];

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
    // `compile::build` is exactly "detect a provider, then build"; mapping
    // the missing-provider case here keeps the single implementation below.
    let clang = compile::detect_clang().ok_or(BuildError::NoClang)?;
    build_native_with(tp, reach, entry_main, opts, target, &clang, out_path)
}

/// Lower a pruned program in precompiled-runtime mode.
///
/// All native build entry points share this: the lowering mode and the
/// `Lowerer` setup must not drift between the single-TU and per-unit paths.
fn precompiled_lowerer(
    tp: &zz_hir::TypedProgram,
    reach: &zz_hir::ReachableSet,
    entry_main: &str,
) -> Lowerer {
    let mut lowerer = Lowerer::new(
        reach.funcs.clone(),
        reach.natives.clone(),
        entry_main.to_string(),
        tp.clone(),
    );
    lowerer.set_precompiled(true);
    lowerer
}

/// Per-module native build: lower to namespaced translation units and
/// compile them (serially for now) against the precompiled runtime archive.
/// `--embed` tables ride in the entry unit (emitted once, never in the
/// shared header). Returns the entry-unit index for callers that need it.
/// Fail before clang when reachable natives lack a C implementation and are
/// not intentional VM-only gaps (#715). Intentional gaps (TLS listeners,
/// MySQL wire driver, pure-ZZ stdlib functions) are tracked in
/// [`KNOWN_CODEGEN_GAPS`] and deliberately allowed to keep aborting at
/// runtime on the VM instead of breaking every build.
pub(crate) fn reject_unimplemented_natives(names: &[String]) -> Result<(), BuildError> {
    let gaps: std::collections::HashSet<&str> =
        KNOWN_CODEGEN_GAPS.iter().map(|(g, _)| *g).collect();
    let offending: Vec<String> = names
        .iter()
        .filter(|n| !gaps.contains(n.as_str()))
        .cloned()
        .collect();
    if offending.is_empty() {
        Ok(())
    } else {
        Err(BuildError::UnimplementedNative { names: offending })
    }
}

pub fn build_native_units_with(
    tp: &zz_hir::TypedProgram,
    reach: &zz_hir::ReachableSet,
    entry_main: &str,
    opts: BuildOptions,
    target: Option<&str>,
    clang: &compile::Clang,
    out_path: &std::path::Path,
) -> Result<units::LoweredUnits, compile::BuildError> {
    build_native_units_with_sink(
        tp, reach, entry_main, opts, target, clang, out_path, &NoopSink,
    )
}

/// [`build_native_units_with`] with codegen progress notifications (#629):
/// generation → per-unit compilation → link, all reported through `sink`.
// One parameter per pipeline stage (typed program, reach set, entry, opts,
// target, toolchain, output, sink) — same shape as the sink-less entry.
#[allow(clippy::too_many_arguments)]
pub fn build_native_units_with_sink(
    tp: &zz_hir::TypedProgram,
    reach: &zz_hir::ReachableSet,
    entry_main: &str,
    opts: BuildOptions,
    target: Option<&str>,
    clang: &compile::Clang,
    out_path: &std::path::Path,
    sink: &dyn CodegenSink,
) -> Result<units::LoweredUnits, compile::BuildError> {
    // Entry namespace derives from the dotted main key (`main.main` ->
    // `main`); bare test mains land in the prelude bucket with bare fns.
    let entry_ns = units::ns_of(entry_main).to_string();
    let verbose = opts.verbose;
    let t_lower = std::time::Instant::now();
    sink.generating();
    let mut lowered = precompiled_lowerer(tp, reach, entry_main).lower_units(&entry_ns);
    reject_unimplemented_natives(&lowered.unimplemented_natives)?;
    if verbose {
        eprintln!("zz: timing lower_units={}ms", t_lower.elapsed().as_millis());
    }
    let mut opts = opts;
    lowered.apply_link_needs(&mut opts);
    if !opts.embed_assets.is_empty() {
        let tables = compile::embed_c(&opts.embed_assets);
        match lowered.units.iter_mut().find(|u| u.ns == lowered.entry_ns) {
            Some(u) => {
                u.code.push_str(&tables);
            }
            None => {
                let header = lowered.header.clone();
                lowered.units.push(units::CodeUnit {
                    ns: lowered.entry_ns.clone(),
                    header,
                    code: tables,
                });
            }
        }
    }
    let sources: Vec<compile::UnitSource<'_>> = lowered
        .units
        .iter()
        .map(|u| compile::UnitSource {
            ns: &u.ns,
            header: &u.header,
            code: &u.code,
        })
        .collect();
    sink.generated(lowered.units.len());
    let t_cc = std::time::Instant::now();
    compile::compile_units_with_sink(&sources, out_path, &opts, target, clang, sink)?;
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
    build_native_with_sink(
        tp, reach, entry_main, opts, target, clang, out_path, &NoopSink,
    )
}

/// [`build_native_with`] with codegen progress notifications (#629). The
/// single-TU path compiles and links in one clang call, so it reports
/// generation, then one compiled file, then the link boundary.
// Same argument shape as [`build_native_units_with_sink`], plus the sink.
#[allow(clippy::too_many_arguments)]
pub fn build_native_with_sink(
    tp: &zz_hir::TypedProgram,
    reach: &zz_hir::ReachableSet,
    entry_main: &str,
    opts: BuildOptions,
    target: Option<&str>,
    clang: &Clang,
    out_path: &std::path::Path,
    sink: &dyn CodegenSink,
) -> Result<LoweredC, BuildError> {
    let verbose = opts.verbose;
    let t_lower = std::time::Instant::now();
    sink.generating();
    let lowered = precompiled_lowerer(tp, reach, entry_main).lower();
    reject_unimplemented_natives(&lowered.unimplemented_natives)?;
    if verbose {
        eprintln!("zz: timing lower={}ms", t_lower.elapsed().as_millis());
    }
    let mut opts = opts;
    lowered.apply_link_needs(&mut opts);
    sink.generated(1);
    sink.compiling(1, 0);
    let t_cc = std::time::Instant::now();
    compile::build_with(&lowered.source, out_path, opts, target, clang)?;
    sink.file_done(1);
    sink.linking();
    sink.linked();
    if verbose {
        eprintln!("zz: timing clang_link={}ms", t_cc.elapsed().as_millis());
    }
    Ok(lowered)
}

/// Native e2e suite: lowers, compiles with the system C toolchain, runs the
/// binary and diffs stdout against the VM.
///
/// Skipped on Windows: the C runtime is POSIX-thread based (`core.h`
/// includes `<pthread.h>`, the channel/rep-pool code uses mutexes and
/// condition variables) and the socket layer is written against POSIX
/// `fcntl`/`poll`, so the AOT backend cannot build there yet. Windows CI
/// runs the VM suites and the CLI/PM tests; native legs are gated by
/// `ZZ_SKIP_NATIVE` in the integration tests for the same reason.
#[cfg(all(test, not(windows)))]
mod tests;
