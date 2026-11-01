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
    is_macos_target, is_windows_target, validate, BuildError, BuildOptions, Clang, ClangProvider,
    EmbedAsset, PgoMode,
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
    ("std.math.matrix_mul", "no fixture; niche math"),
    // Pure-ZZ stdlib functions — run in VM only, no C codegen
    ("std.math.sum", "pure ZZ; no C codegen"),
    ("std.math.product", "pure ZZ; no C codegen"),
    ("std.math.count", "pure ZZ; no C codegen"),
    ("std.math.min", "pure ZZ; no C codegen"),
    ("std.math.max", "pure ZZ; no C codegen"),
    ("std.math.is_even", "pure ZZ; no C codegen"),
    ("std.math.is_odd", "pure ZZ; no C codegen"),
    ("std.math.min_arr", "pure ZZ; no C codegen"),
    ("std.math.max_arr", "pure ZZ; no C codegen"),
    ("std.math.sum_f", "pure ZZ; no C codegen"),
    ("std.math.product_f", "pure ZZ; no C codegen"),
    ("std.math.mean_f", "pure ZZ; no C codegen"),
    ("std.math.median_f", "pure ZZ; no C codegen"),
    // Math numeric constants without a zero-arg C entry (`PI`/`E`/`TAU`/
    // `INF`/`NAN` have one; these never needed it): value uses lower to
    // float literals, and the checker rejects calls.
    (
        "std.math.SQRT_2",
        "numeric constant; lowers to float literal",
    ),
    (
        "std.math.SQRT_1_2",
        "numeric constant; lowers to float literal",
    ),
    ("std.math.LN_2", "numeric constant; lowers to float literal"),
    (
        "std.math.LN_10",
        "numeric constant; lowers to float literal",
    ),
    (
        "std.math.LOG10_E",
        "numeric constant; lowers to float literal",
    ),
    (
        "std.math.LOG2_E",
        "numeric constant; lowers to float literal",
    ),
    ("std.str.repeat", "pure ZZ; no C codegen"),
    ("std.str.is_empty", "pure ZZ; no C codegen"),
    ("std.str.reverse", "pure ZZ; no C codegen"),
    ("std.str.pad_left", "pure ZZ; no C codegen"),
    ("std.str.pad_right", "pure ZZ; no C codegen"),
    ("std.vec.fold", "pure ZZ; no C codegen"),
    ("std.vec.sum", "pure ZZ; no C codegen"),
    ("std.vec.product", "pure ZZ; no C codegen"),
    ("std.vec.min_val", "pure ZZ; no C codegen"),
    ("std.vec.max_val", "pure ZZ; no C codegen"),
    ("std.vec.sum_f", "pure ZZ; no C codegen"),
    ("std.vec.product_f", "pure ZZ; no C codegen"),
    ("std.vec.concat", "pure ZZ; no C codegen"),
    ("std.vec.flatten", "pure ZZ; no C codegen"),
    ("std.vec.index_of", "pure ZZ; no C codegen"),
    ("std.vec.last_index_of", "pure ZZ; no C codegen"),
    // Pure-ZZ JSON helpers — compiled from zz/json/mod.zz, run in VM only
    ("std.json.validate", "pure ZZ; no C codegen"),
    ("std.json.parse_or", "pure ZZ; no C codegen"),
    ("std.json.parse_or_null", "pure ZZ; no C codegen"),
    ("std.json.path_exists", "pure ZZ; no C codegen"),
    ("std.json.path_get_or", "pure ZZ; no C codegen"),
    ("std.json.is_null", "pure ZZ; no C codegen"),
    ("std.json.is_bool", "pure ZZ; no C codegen"),
    ("std.json.is_number", "pure ZZ; no C codegen"),
    ("std.json.is_string", "pure ZZ; no C codegen"),
    ("std.json.is_array", "pure ZZ; no C codegen"),
    ("std.json.is_object", "pure ZZ; no C codegen"),
    ("std.json.is_empty", "pure ZZ; no C codegen"),
    // Method-dispatch aliases for pure-ZZ functions
    ("math.sum", "pure ZZ alias"),
    ("math.product", "pure ZZ alias"),
    ("math.count", "pure ZZ alias"),
    ("math.min", "pure ZZ alias"),
    ("math.max", "pure ZZ alias"),
    ("math.is_even", "pure ZZ alias"),
    ("math.is_odd", "pure ZZ alias"),
    ("math.min_arr", "pure ZZ alias"),
    ("math.max_arr", "pure ZZ alias"),
    ("math.sum_f", "pure ZZ alias"),
    ("math.product_f", "pure ZZ alias"),
    ("math.mean_f", "pure ZZ alias"),
    ("math.median_f", "pure ZZ alias"),
    ("str.repeat", "pure ZZ alias"),
    ("str.is_empty", "pure ZZ alias"),
    ("str.reverse", "pure ZZ alias"),
    ("str.pad_left", "pure ZZ alias"),
    ("str.pad_right", "pure ZZ alias"),
    ("vec.fold", "pure ZZ alias"),
    ("vec.sum", "pure ZZ alias"),
    ("vec.product", "pure ZZ alias"),
    ("vec.min_val", "pure ZZ alias"),
    ("vec.max_val", "pure ZZ alias"),
    ("vec.sum_f", "pure ZZ alias"),
    ("vec.product_f", "pure ZZ alias"),
    ("vec.concat", "pure ZZ alias"),
    ("vec.flatten", "pure ZZ alias"),
    ("vec.index_of", "pure ZZ alias"),
    ("vec.last_index_of", "pure ZZ alias"),
    // Method-dispatch aliases for pure-ZZ JSON helpers
    ("json.validate", "pure ZZ alias"),
    ("json.parse_or", "pure ZZ alias"),
    ("json.parse_or_null", "pure ZZ alias"),
    ("json.path_exists", "pure ZZ alias"),
    ("json.path_get_or", "pure ZZ alias"),
    ("json.is_null", "pure ZZ alias"),
    ("json.is_bool", "pure ZZ alias"),
    ("json.is_number", "pure ZZ alias"),
    ("json.is_string", "pure ZZ alias"),
    ("json.is_array", "pure ZZ alias"),
    ("json.is_object", "pure ZZ alias"),
    ("json.is_empty", "pure ZZ alias"),
    // Pure-ZZ regexp helpers — compiled from zz/regexp/mod.zz and merged
    // into AOT builds as ZZ functions (no C/FFI native needed).
    ("Regexp.new", "pure ZZ; lowered as ZZ fn"),
    ("ArgsParser.new", "pure ZZ; lowered as ZZ fn"),
    // Pure-ZZ path helpers — compiled from zz/path/mod.zz and merged
    // into AOT builds as ZZ functions (no C native needed).
    ("std.path.join", "pure ZZ; lowered as ZZ fn"),
    ("std.path.join_all", "pure ZZ; lowered as ZZ fn"),
    ("std.path.normalize", "pure ZZ; lowered as ZZ fn"),
    ("std.path.basename", "pure ZZ; lowered as ZZ fn"),
    ("std.path.dirname", "pure ZZ; lowered as ZZ fn"),
    ("std.path.is_absolute", "pure ZZ; lowered as ZZ fn"),
    ("std.path.extension", "pure ZZ; lowered as ZZ fn"),
    ("path.join", "pure ZZ alias"),
    ("path.join_all", "pure ZZ alias"),
    ("path.normalize", "pure ZZ alias"),
    ("path.basename", "pure ZZ alias"),
    ("path.dirname", "pure ZZ alias"),
    ("path.is_absolute", "pure ZZ alias"),
    ("path.extension", "pure ZZ alias"),
    ("std.regexp.is_email", "pure ZZ; lowered as ZZ fn"),
    ("regexp.is_email", "pure ZZ alias"),
    // Pure-ZZ time helpers — compiled from zz/time/mod.zz, same deal.
    ("time.micros", "pure ZZ; lowered as ZZ fn"),
    ("time.millis", "pure ZZ; lowered as ZZ fn"),
    ("time.secs", "pure ZZ; lowered as ZZ fn"),
    ("time.to_micros", "pure ZZ; lowered as ZZ fn"),
    ("time.to_millis", "pure ZZ; lowered as ZZ fn"),
    ("time.to_secs", "pure ZZ; lowered as ZZ fn"),
    ("time.to_nanos", "pure ZZ; lowered as ZZ fn"),
    ("time.sleep", "pure ZZ; lowered as ZZ fn"),
    // Pure-ZZ map/set/dec/bytes/csv/str-builder/time-date batch —
    // compiled from zz/<mod>/mod.zz and merged into AOT builds as ZZ
    // functions (no C/FFI native needed).
    // zz/bytes
    ("std.bytes.builder", "pure ZZ; lowered as ZZ fn"),
    ("std.bytes.extend", "pure ZZ; lowered as ZZ fn"),
    ("std.bytes.from_ints", "pure ZZ; lowered as ZZ fn"),
    ("std.bytes.len_of", "pure ZZ; lowered as ZZ fn"),
    ("std.bytes.push_byte", "pure ZZ; lowered as ZZ fn"),
    ("bytes.builder", "pure ZZ alias"),
    ("bytes.extend", "pure ZZ alias"),
    ("bytes.from_ints", "pure ZZ alias"),
    ("bytes.len_of", "pure ZZ alias"),
    ("bytes.push_byte", "pure ZZ alias"),
    // zz/csv
    ("std.csv.delim_first", "pure ZZ; lowered as ZZ fn"),
    ("std.csv.escape_cell", "pure ZZ; lowered as ZZ fn"),
    ("std.csv.get_cell", "pure ZZ; lowered as ZZ fn"),
    ("std.csv.header", "pure ZZ; lowered as ZZ fn"),
    ("std.csv.json_escape", "pure ZZ; lowered as ZZ fn"),
    ("std.csv.len", "pure ZZ; lowered as ZZ fn"),
    ("std.csv.needs_quote", "pure ZZ; lowered as ZZ fn"),
    ("std.csv.parse", "pure ZZ; lowered as ZZ fn"),
    ("std.csv.parse_delim", "pure ZZ; lowered as ZZ fn"),
    ("std.csv.records", "pure ZZ; lowered as ZZ fn"),
    ("std.csv.stringify", "pure ZZ; lowered as ZZ fn"),
    ("std.csv.stringify_delim", "pure ZZ; lowered as ZZ fn"),
    ("std.csv.to_json", "pure ZZ; lowered as ZZ fn"),
    ("std.csv.validate", "pure ZZ; lowered as ZZ fn"),
    ("csv.delim_first", "pure ZZ alias"),
    ("csv.escape_cell", "pure ZZ alias"),
    ("csv.get_cell", "pure ZZ alias"),
    ("csv.header", "pure ZZ alias"),
    ("csv.json_escape", "pure ZZ alias"),
    ("csv.len", "pure ZZ alias"),
    ("csv.needs_quote", "pure ZZ alias"),
    ("csv.parse", "pure ZZ alias"),
    ("csv.parse_delim", "pure ZZ alias"),
    ("csv.records", "pure ZZ alias"),
    ("csv.stringify", "pure ZZ alias"),
    ("csv.stringify_delim", "pure ZZ alias"),
    ("csv.to_json", "pure ZZ alias"),
    ("csv.validate", "pure ZZ alias"),
    // zz/property
    ("std.property.seed", "pure ZZ; lowered as ZZ fn"),
    ("std.property.norm_state", "pure ZZ; lowered as ZZ fn"),
    ("std.property.step", "pure ZZ; lowered as ZZ fn"),
    ("std.property.stream", "pure ZZ; lowered as ZZ fn"),
    ("std.property.int_in", "pure ZZ; lowered as ZZ fn"),
    ("std.property.pick", "pure ZZ; lowered as ZZ fn"),
    ("std.property.boolean", "pure ZZ; lowered as ZZ fn"),
    ("std.property.alpha_char", "pure ZZ; lowered as ZZ fn"),
    ("std.property.alpha_str", "pure ZZ; lowered as ZZ fn"),
    ("std.property.shrink_int", "pure ZZ; lowered as ZZ fn"),
    ("std.property.shrink_str", "pure ZZ; lowered as ZZ fn"),
    ("std.property.shrink_value", "pure ZZ; lowered as ZZ fn"),
    ("std.property.check", "pure ZZ; lowered as ZZ fn"),
    ("std.property.for_all_int", "pure ZZ; lowered as ZZ fn"),
    ("property.seed", "pure ZZ alias"),
    ("property.norm_state", "pure ZZ alias"),
    ("property.step", "pure ZZ alias"),
    ("property.stream", "pure ZZ alias"),
    ("property.int_in", "pure ZZ alias"),
    ("property.pick", "pure ZZ alias"),
    ("property.boolean", "pure ZZ alias"),
    ("property.alpha_char", "pure ZZ alias"),
    ("property.alpha_str", "pure ZZ alias"),
    ("property.shrink_int", "pure ZZ alias"),
    ("property.shrink_str", "pure ZZ alias"),
    ("property.shrink_value", "pure ZZ alias"),
    ("property.check", "pure ZZ alias"),
    ("property.for_all_int", "pure ZZ alias"),
    // zz/dec
    ("std.dec.add", "pure ZZ; lowered as ZZ fn"),
    ("std.dec.cmp", "pure ZZ; lowered as ZZ fn"),
    ("std.dec.eq", "pure ZZ; lowered as ZZ fn"),
    ("std.dec.format", "pure ZZ; lowered as ZZ fn"),
    ("std.dec.from_scaled", "pure ZZ; lowered as ZZ fn"),
    ("std.dec.gt", "pure ZZ; lowered as ZZ fn"),
    ("std.dec.is_valid", "pure ZZ; lowered as ZZ fn"),
    ("std.dec.lt", "pure ZZ; lowered as ZZ fn"),
    ("std.dec.mul", "pure ZZ; lowered as ZZ fn"),
    ("std.dec.pow10", "pure ZZ; lowered as ZZ fn"),
    ("std.dec.scale_of", "pure ZZ; lowered as ZZ fn"),
    ("std.dec.sub", "pure ZZ; lowered as ZZ fn"),
    ("std.dec.to_scaled", "pure ZZ; lowered as ZZ fn"),
    ("std.dec.trim_zeros", "pure ZZ; lowered as ZZ fn"),
    ("dec.add", "pure ZZ alias"),
    ("dec.cmp", "pure ZZ alias"),
    ("dec.eq", "pure ZZ alias"),
    ("dec.format", "pure ZZ alias"),
    ("dec.from_scaled", "pure ZZ alias"),
    ("dec.gt", "pure ZZ alias"),
    ("dec.is_valid", "pure ZZ alias"),
    ("dec.lt", "pure ZZ alias"),
    ("dec.mul", "pure ZZ alias"),
    ("dec.pow10", "pure ZZ alias"),
    ("dec.scale_of", "pure ZZ alias"),
    ("dec.sub", "pure ZZ alias"),
    ("dec.to_scaled", "pure ZZ alias"),
    ("dec.trim_zeros", "pure ZZ alias"),
    // zz/map
    ("std.map.get_or", "pure ZZ; lowered as ZZ fn"),
    ("std.map.get_str", "pure ZZ; lowered as ZZ fn"),
    ("std.map.has", "pure ZZ; lowered as ZZ fn"),
    ("std.map.is_empty", "pure ZZ; lowered as ZZ fn"),
    ("std.map.keys", "pure ZZ; lowered as ZZ fn"),
    ("std.map.keys_str", "pure ZZ; lowered as ZZ fn"),
    ("std.map.len", "pure ZZ; lowered as ZZ fn"),
    ("std.map.merge", "pure ZZ; lowered as ZZ fn"),
    ("std.map.merge_str", "pure ZZ; lowered as ZZ fn"),
    ("std.map.remove", "pure ZZ; lowered as ZZ fn"),
    ("std.map.values", "pure ZZ; lowered as ZZ fn"),
    ("std.map.values_str", "pure ZZ; lowered as ZZ fn"),
    ("map.get_or", "pure ZZ alias"),
    ("map.get_str", "pure ZZ alias"),
    ("map.has", "pure ZZ alias"),
    ("map.is_empty", "pure ZZ alias"),
    ("map.keys", "pure ZZ alias"),
    ("map.keys_str", "pure ZZ alias"),
    ("map.len", "pure ZZ alias"),
    ("map.merge", "pure ZZ alias"),
    ("map.merge_str", "pure ZZ alias"),
    ("map.remove", "pure ZZ alias"),
    ("map.values", "pure ZZ alias"),
    ("map.values_str", "pure ZZ alias"),
    // zz/set
    ("std.set.diff", "pure ZZ; lowered as ZZ fn"),
    ("std.set.has", "pure ZZ; lowered as ZZ fn"),
    ("std.set.has_int", "pure ZZ; lowered as ZZ fn"),
    ("std.set.insert", "pure ZZ; lowered as ZZ fn"),
    ("std.set.insert_int", "pure ZZ; lowered as ZZ fn"),
    ("std.set.intersect", "pure ZZ; lowered as ZZ fn"),
    ("std.set.intersect_int", "pure ZZ; lowered as ZZ fn"),
    ("std.set.is_empty", "pure ZZ; lowered as ZZ fn"),
    ("std.set.len", "pure ZZ; lowered as ZZ fn"),
    ("std.set.remove", "pure ZZ; lowered as ZZ fn"),
    ("std.set.remove_int", "pure ZZ; lowered as ZZ fn"),
    ("std.set.union", "pure ZZ; lowered as ZZ fn"),
    ("std.set.union_int", "pure ZZ; lowered as ZZ fn"),
    ("set.diff", "pure ZZ alias"),
    ("set.has", "pure ZZ alias"),
    ("set.has_int", "pure ZZ alias"),
    ("set.insert", "pure ZZ alias"),
    ("set.insert_int", "pure ZZ alias"),
    ("set.intersect", "pure ZZ alias"),
    ("set.intersect_int", "pure ZZ alias"),
    ("set.is_empty", "pure ZZ alias"),
    ("set.len", "pure ZZ alias"),
    ("set.remove", "pure ZZ alias"),
    ("set.remove_int", "pure ZZ alias"),
    ("set.union", "pure ZZ alias"),
    ("set.union_int", "pure ZZ alias"),
    // zz/str
    ("std.str.builder", "pure ZZ; lowered as ZZ fn"),
    ("std.str.builder_len", "pure ZZ; lowered as ZZ fn"),
    ("std.str.finish", "pure ZZ; lowered as ZZ fn"),
    ("std.str.join_parts", "pure ZZ; lowered as ZZ fn"),
    ("std.str.push_part", "pure ZZ; lowered as ZZ fn"),
    ("str.builder", "pure ZZ alias"),
    ("str.builder_len", "pure ZZ alias"),
    ("str.finish", "pure ZZ alias"),
    ("str.join_parts", "pure ZZ alias"),
    ("str.push_part", "pure ZZ alias"),
    // zz/time
    ("std.time.add_days", "pure ZZ; lowered as ZZ fn"),
    ("std.time.civil_from_days", "pure ZZ; lowered as ZZ fn"),
    ("std.time.date_valid", "pure ZZ; lowered as ZZ fn"),
    ("std.time.days_from_civil", "pure ZZ; lowered as ZZ fn"),
    ("std.time.days_in_month", "pure ZZ; lowered as ZZ fn"),
    ("std.time.diff_days", "pure ZZ; lowered as ZZ fn"),
    ("std.time.epoch_fallback", "pure ZZ; lowered as ZZ fn"),
    ("std.time.format_rfc3339", "pure ZZ; lowered as ZZ fn"),
    ("std.time.from_epoch_days", "pure ZZ; lowered as ZZ fn"),
    ("std.time.is_leap", "pure ZZ; lowered as ZZ fn"),
    ("std.time.make_date", "pure ZZ; lowered as ZZ fn"),
    ("std.time.pad2", "pure ZZ; lowered as ZZ fn"),
    ("std.time.parse_rfc3339", "pure ZZ; lowered as ZZ fn"),
    ("std.time.to_epoch_days", "pure ZZ; lowered as ZZ fn"),
    ("time.add_days", "pure ZZ alias"),
    ("time.civil_from_days", "pure ZZ alias"),
    ("time.date_valid", "pure ZZ alias"),
    ("time.days_from_civil", "pure ZZ alias"),
    ("time.days_in_month", "pure ZZ alias"),
    ("time.diff_days", "pure ZZ alias"),
    ("time.epoch_fallback", "pure ZZ alias"),
    ("time.format_rfc3339", "pure ZZ alias"),
    ("time.from_epoch_days", "pure ZZ alias"),
    ("time.is_leap", "pure ZZ alias"),
    ("time.make_date", "pure ZZ alias"),
    ("time.pad2", "pure ZZ alias"),
    ("time.parse_rfc3339", "pure ZZ alias"),
    ("time.to_epoch_days", "pure ZZ alias"),
    // Pure-ZZ color helpers — compiled from zz/colors/mod.zz, same deal.
    ("std.colors.black", "pure ZZ; lowered as ZZ fn"),
    ("std.colors.red", "pure ZZ; lowered as ZZ fn"),
    ("std.colors.green", "pure ZZ; lowered as ZZ fn"),
    ("std.colors.yellow", "pure ZZ; lowered as ZZ fn"),
    ("std.colors.blue", "pure ZZ; lowered as ZZ fn"),
    ("std.colors.magenta", "pure ZZ; lowered as ZZ fn"),
    ("std.colors.cyan", "pure ZZ; lowered as ZZ fn"),
    ("std.colors.white", "pure ZZ; lowered as ZZ fn"),
    ("std.colors.bright_black", "pure ZZ; lowered as ZZ fn"),
    ("std.colors.bright_red", "pure ZZ; lowered as ZZ fn"),
    ("std.colors.bright_green", "pure ZZ; lowered as ZZ fn"),
    ("std.colors.bright_yellow", "pure ZZ; lowered as ZZ fn"),
    ("std.colors.bright_blue", "pure ZZ; lowered as ZZ fn"),
    ("std.colors.bright_magenta", "pure ZZ; lowered as ZZ fn"),
    ("std.colors.bright_cyan", "pure ZZ; lowered as ZZ fn"),
    ("std.colors.bright_white", "pure ZZ; lowered as ZZ fn"),
    ("std.colors.bg_black", "pure ZZ; lowered as ZZ fn"),
    ("std.colors.bg_red", "pure ZZ; lowered as ZZ fn"),
    ("std.colors.bg_green", "pure ZZ; lowered as ZZ fn"),
    ("std.colors.bg_yellow", "pure ZZ; lowered as ZZ fn"),
    ("std.colors.bg_blue", "pure ZZ; lowered as ZZ fn"),
    ("std.colors.bg_magenta", "pure ZZ; lowered as ZZ fn"),
    ("std.colors.bg_cyan", "pure ZZ; lowered as ZZ fn"),
    ("std.colors.bg_white", "pure ZZ; lowered as ZZ fn"),
    ("std.colors.bg_bright_black", "pure ZZ; lowered as ZZ fn"),
    ("std.colors.bg_bright_red", "pure ZZ; lowered as ZZ fn"),
    ("std.colors.bg_bright_green", "pure ZZ; lowered as ZZ fn"),
    ("std.colors.bg_bright_yellow", "pure ZZ; lowered as ZZ fn"),
    ("std.colors.bg_bright_blue", "pure ZZ; lowered as ZZ fn"),
    ("std.colors.bg_bright_magenta", "pure ZZ; lowered as ZZ fn"),
    ("std.colors.bg_bright_cyan", "pure ZZ; lowered as ZZ fn"),
    ("std.colors.bg_bright_white", "pure ZZ; lowered as ZZ fn"),
    ("std.colors.bold", "pure ZZ; lowered as ZZ fn"),
    ("std.colors.dim", "pure ZZ; lowered as ZZ fn"),
    ("std.colors.italic", "pure ZZ; lowered as ZZ fn"),
    ("std.colors.underline", "pure ZZ; lowered as ZZ fn"),
    ("std.colors.blink", "pure ZZ; lowered as ZZ fn"),
    ("std.colors.reverse", "pure ZZ; lowered as ZZ fn"),
    ("std.colors.strikethrough", "pure ZZ; lowered as ZZ fn"),
    ("std.colors.reset", "pure ZZ; lowered as ZZ fn"),
    ("std.colors.strip", "pure ZZ; lowered as ZZ fn"),
    ("std.colors.clamp255", "pure ZZ; lowered as ZZ fn"),
    ("std.colors.rgb", "pure ZZ; lowered as ZZ fn"),
    ("std.colors.bg_rgb", "pure ZZ; lowered as ZZ fn"),
    ("std.colors.color256", "pure ZZ; lowered as ZZ fn"),
    ("std.colors.bg_256", "pure ZZ; lowered as ZZ fn"),
    ("std.colors.hex_val", "pure ZZ; lowered as ZZ fn"),
    ("std.colors.hex_byte", "pure ZZ; lowered as ZZ fn"),
    ("std.colors.hex", "pure ZZ; lowered as ZZ fn"),
    ("std.colors.hex6", "pure ZZ; lowered as ZZ fn"),
    ("std.colors.hex3", "pure ZZ; lowered as ZZ fn"),
    ("colors.black", "pure ZZ alias"),
    ("colors.red", "pure ZZ alias"),
    ("colors.green", "pure ZZ alias"),
    ("colors.yellow", "pure ZZ alias"),
    ("colors.blue", "pure ZZ alias"),
    ("colors.magenta", "pure ZZ alias"),
    ("colors.cyan", "pure ZZ alias"),
    ("colors.white", "pure ZZ alias"),
    ("colors.bright_black", "pure ZZ alias"),
    ("colors.bright_red", "pure ZZ alias"),
    ("colors.bright_green", "pure ZZ alias"),
    ("colors.bright_yellow", "pure ZZ alias"),
    ("colors.bright_blue", "pure ZZ alias"),
    ("colors.bright_magenta", "pure ZZ alias"),
    ("colors.bright_cyan", "pure ZZ alias"),
    ("colors.bright_white", "pure ZZ alias"),
    ("colors.bg_black", "pure ZZ alias"),
    ("colors.bg_red", "pure ZZ alias"),
    ("colors.bg_green", "pure ZZ alias"),
    ("colors.bg_yellow", "pure ZZ alias"),
    ("colors.bg_blue", "pure ZZ alias"),
    ("colors.bg_magenta", "pure ZZ alias"),
    ("colors.bg_cyan", "pure ZZ alias"),
    ("colors.bg_white", "pure ZZ alias"),
    ("colors.bg_bright_black", "pure ZZ alias"),
    ("colors.bg_bright_red", "pure ZZ alias"),
    ("colors.bg_bright_green", "pure ZZ alias"),
    ("colors.bg_bright_yellow", "pure ZZ alias"),
    ("colors.bg_bright_blue", "pure ZZ alias"),
    ("colors.bg_bright_magenta", "pure ZZ alias"),
    ("colors.bg_bright_cyan", "pure ZZ alias"),
    ("colors.bg_bright_white", "pure ZZ alias"),
    ("colors.bold", "pure ZZ alias"),
    ("colors.dim", "pure ZZ alias"),
    ("colors.italic", "pure ZZ alias"),
    ("colors.underline", "pure ZZ alias"),
    ("colors.blink", "pure ZZ alias"),
    ("colors.reverse", "pure ZZ alias"),
    ("colors.strikethrough", "pure ZZ alias"),
    ("colors.reset", "pure ZZ alias"),
    ("colors.strip", "pure ZZ alias"),
    ("colors.clamp255", "pure ZZ alias"),
    ("colors.rgb", "pure ZZ alias"),
    ("colors.bg_rgb", "pure ZZ alias"),
    ("colors.color256", "pure ZZ alias"),
    ("colors.bg_256", "pure ZZ alias"),
    ("colors.hex_val", "pure ZZ alias"),
    ("colors.hex_byte", "pure ZZ alias"),
    ("colors.hex", "pure ZZ alias"),
    ("colors.hex6", "pure ZZ alias"),
    ("colors.hex3", "pure ZZ alias"),
    // Parity-skipped modules (non-deterministic output):
    // NOTE: `serve_dir`/`serve_dir_at` used to be listed here (no C
    // implementation); both are implemented in the AOT runtime now, so the
    // stale-gap check requires their removal.
    ("std.http.delete", "http fixtures skipped in parity"),
    ("std.http.put", "http fixtures skipped in parity"),
    // HTTP helpers (pure-ZZ, compiled from zz/http/mod.zz)
    ("std.http.ok", "pure ZZ; no C codegen"),
    ("http.ok", "pure ZZ alias"),
    ("std.http.created", "pure ZZ; no C codegen"),
    ("http.created", "pure ZZ alias"),
    ("std.http.not_found", "pure ZZ; no C codegen"),
    ("http.not_found", "pure ZZ alias"),
    ("std.http.redirect", "pure ZZ; no C codegen"),
    ("http.redirect", "pure ZZ alias"),
    ("std.http.cors", "pure ZZ; no C codegen"),
    ("http.cors", "pure ZZ alias"),
    ("std.http.secure_headers", "pure ZZ; no C codegen"),
    ("http.secure_headers", "pure ZZ alias"),
    ("std.http.secure_header_dict", "pure ZZ; no C codegen"),
    ("http.secure_header_dict", "pure ZZ alias"),
    ("std.http.csrf_token", "pure ZZ; no C codegen"),
    ("http.csrf_token", "pure ZZ alias"),
    ("std.http.csrf_check", "pure ZZ; no C codegen"),
    ("http.csrf_check", "pure ZZ alias"),
    ("std.http.request_id", "pure ZZ; no C codegen"),
    ("http.request_id", "pure ZZ alias"),
    // Phase 2/3 HTTP natives — deliberate AOT exclusions (VM-only). Each is
    // excluded because porting it to the C HTTP runtime needs infrastructure
    // that does not exist in AOT (rustls TLS, WebSocket handshake + TCP
    // handoff, insecure curl client). They abort loudly (`zz_unimplemented_native`)
    // on AOT — never silently lower to unit.
    ("std.http.listen_tls", "AOT exclusion (VM-only): TLS listener needs rustls server config + TLS accept loop in C"),
    ("http.listen_tls", "AOT exclusion (VM-only): TLS listener needs rustls server config + TLS accept loop in C"),
    ("std.http.listen_tls_cfg", "AOT exclusion (VM-only): TLS listener + listen_cfg limits; needs rustls in C"),
    ("http.listen_tls_cfg", "AOT exclusion (VM-only): TLS listener + listen_cfg limits; needs rustls in C"),
    ("std.http.fetch_insecure", "AOT exclusion (VM-only): insecure (self-signed) TLS client needs libcurl with verification disabled; no C twin"),
    ("http.fetch_insecure", "AOT exclusion (VM-only): insecure (self-signed) TLS client needs libcurl with verification disabled; no C twin"),
    ("std.http.hijack", "AOT exclusion (VM-only): WebSocket upgrade handshake + TCP stream handoff; cleartext-only in AOT"),
    ("http.hijack", "AOT exclusion (VM-only): WebSocket upgrade handshake + TCP stream handoff; cleartext-only in AOT"),
    // MySQL wire driver — VM-only (no staticlib backend like PG has;
    // AOT lowers these to Unit like time.now_ms).
    ("std.sqlz.mysql.connect", "VM-only; no C socket driver"),
    ("std.sqlz.mysql.exec", "VM-only; no C socket driver"),
    ("std.sqlz.mysql.query", "VM-only; no C socket driver"),
    ("std.sqlz.mysql.close", "VM-only; no C socket driver"),
    // Closure transactions — inlined by the AOT codegen (BEGIN/COMMIT/ROLLBACK
    // around the closure body); no native C implementation needed.
    ("std.sqlz.transaction", "AOT-inlined; no native C impl"),
    ("sqlz.transaction", "AOT-inlined; no native C impl"),
    // Listener shutdown — VM-only, no C socket listener shutdown twin.
    ("std.tls_listener.shutdown", "VM-only; no C listener shutdown twin"),
    ("std.unix_listener.shutdown", "VM-only; no C listener shutdown twin"),
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
    sink.generated(lowered.units.len());
    let t_cc = std::time::Instant::now();
    compile::compile_units_with_sink(
        &lowered.header,
        &pairs,
        out_path,
        &opts,
        target,
        clang,
        sink,
    )?;
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
