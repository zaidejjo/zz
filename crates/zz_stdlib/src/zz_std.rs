//! Pure-ZZ standard library: embedded `.zz` source files compiled once
//! and shared by both the bytecode VM and the native AOT codegen.
//!
//! Each `.zz` file is embedded at compile time via `include_str!` and
//! compiled to a [`TypedProgram`] at first use. The compiled programs
//! are cached in a `OnceLock` so the parse + type-check cost is paid
//! exactly once per process.

use std::collections::HashMap;
use std::sync::OnceLock;

use zz_checker::{FuncSig, StructSig, Type};
use zz_hir::TypedProgram;

/// Embedded pure-ZZ stdlib source files.
const STR_MOD_ZZ: &str = include_str!("../zz/str/mod.zz");
const MATH_MOD_ZZ: &str = include_str!("../zz/math/mod.zz");
const VEC_ZZ: &str = include_str!("../zz/collections/vec.zz");
const JSON_MOD_ZZ: &str = include_str!("../zz/json/mod.zz");
const REGEXP_MOD_ZZ: &str = include_str!("../zz/regexp/mod.zz");
const TIME_MOD_ZZ: &str = include_str!("../zz/time/mod.zz");
const ARGS_MOD_ZZ: &str = include_str!("../zz/args/mod.zz");
const COLORS_MOD_ZZ: &str = include_str!("../zz/colors/mod.zz");
const PATH_MOD_ZZ: &str = include_str!("../zz/path/mod.zz");
const HTTP_MOD_ZZ: &str = include_str!("../zz/http/mod.zz");
const MAP_ZZ: &str = include_str!("../zz/collections/map.zz");
const SET_ZZ: &str = include_str!("../zz/collections/set.zz");
const DEC_MOD_ZZ: &str = include_str!("../zz/dec/mod.zz");
const BYTES_MOD_ZZ: &str = include_str!("../zz/bytes/mod.zz");
const CSV_MOD_ZZ: &str = include_str!("../zz/csv/mod.zz");

/// All embedded source files, in compilation order.
const ZZ_SOURCES: &[(&str, &str)] = &[
    ("std:str.mod.zz", STR_MOD_ZZ),
    ("std:math.mod.zz", MATH_MOD_ZZ),
    ("std:collections.vec.zz", VEC_ZZ),
    ("std:json.mod.zz", JSON_MOD_ZZ),
    ("std:regexp.mod.zz", REGEXP_MOD_ZZ),
    ("std:time.mod.zz", TIME_MOD_ZZ),
    ("std:args.mod.zz", ARGS_MOD_ZZ),
    ("std:colors.mod.zz", COLORS_MOD_ZZ),
    ("std:path.mod.zz", PATH_MOD_ZZ),
    ("std:http.mod.zz", HTTP_MOD_ZZ),
    ("std:collections.map.zz", MAP_ZZ),
    ("std:collections.set.zz", SET_ZZ),
    ("std:dec.mod.zz", DEC_MOD_ZZ),
    ("std:bytes.mod.zz", BYTES_MOD_ZZ),
    ("std:csv.mod.zz", CSV_MOD_ZZ),
];

/// Compiled pure-ZZ stdlib programs, computed once.
static COMPILED: OnceLock<Vec<TypedProgram>> = OnceLock::new();

/// Per-program lazy slots: scoped runs compile only the programs in the
/// import closure instead of all fifteen (each `compile_one` is
/// independent — same global seeds, no cross-program state). Full-access
/// paths (`zz_stdlib_programs`, codegen) still compile everything once.
const fn new_slot() -> OnceLock<TypedProgram> {
    OnceLock::new()
}
static SLOTS: [OnceLock<TypedProgram>; 15] = [
    new_slot(),
    new_slot(),
    new_slot(),
    new_slot(),
    new_slot(),
    new_slot(),
    new_slot(),
    new_slot(),
    new_slot(),
    new_slot(),
    new_slot(),
    new_slot(),
    new_slot(),
    new_slot(),
    new_slot(),
];

const _: () = assert!(
    ZZ_SOURCES.len() == 15,
    "SLOTS must cover every embedded stdlib source"
);

/// Compile (once) and borrow one embedded program by `ZZ_SOURCES` index.
fn program_at(idx: usize) -> &'static TypedProgram {
    let (name, source) = ZZ_SOURCES[idx];
    SLOTS[idx].get_or_init(|| {
        match compile_one(
            name,
            source,
            &HashMap::new(),
            crate::funcs::stdlib_funcs_cached(),
            &HashMap::new(),
        ) {
            Ok(tp) => tp,
            Err(e) => panic!("pure-ZZ stdlib compilation failed: {e}"),
        }
    })
}

/// Parse, type-check, and build a [`TypedProgram`] from embedded `.zz` source.
///
/// `initial_funcs` and `initial_structs` provide the stdlib signatures so the
/// type checker can resolve native function calls within the `.zz` files.
fn compile_one(
    name: &str,
    source: &str,
    initial_bindings: &HashMap<String, Type>,
    initial_funcs: &HashMap<String, FuncSig>,
    initial_structs: &HashMap<String, StructSig>,
) -> Result<TypedProgram, String> {
    let parsed = zz_frontend::parse(source);
    if !parsed.errors.is_empty() {
        return Err(format!(
            "parse errors in pure-ZZ stdlib `{name}`: {:?}",
            parsed.errors
        ));
    }
    let res = zz_hir::build_program(
        &parsed.program,
        initial_bindings.clone(),
        initial_funcs.clone(),
        initial_structs.clone(),
        HashMap::new(),
        HashMap::new(),
    );
    let has_errors = res
        .diagnostics
        .iter()
        .any(|d| d.severity == zz_frontend::diag::Severity::Error);
    if has_errors {
        let msgs: Vec<_> = res
            .diagnostics
            .iter()
            .filter(|d| d.severity == zz_frontend::diag::Severity::Error)
            .map(|d| d.message.clone())
            .collect();
        return Err(format!(
            "type errors in pure-ZZ stdlib `{name}`: {}",
            msgs.join("; ")
        ));
    }
    Ok(res.program)
}

/// Compile all embedded pure-ZZ stdlib files.
///
/// The resulting [`TypedProgram`]s contain the original AST plus the resolved
/// type map. The VM executes them to populate the environment with the
/// compiled functions; AOT codegen lowers them to C.
fn compile_all() -> Vec<TypedProgram> {
    (0..ZZ_SOURCES.len())
        .map(|idx| program_at(idx).clone())
        .collect()
}

/// Get the compiled pure-ZZ stdlib programs.
///
/// Returns a reference to a `Vec<TypedProgram>` that is computed exactly once.
/// Each element corresponds to one embedded `.zz` file, in compilation order.
pub fn zz_stdlib_programs() -> &'static [TypedProgram] {
    COMPILED.get_or_init(compile_all)
}

/// Borrow one compiled program by `ZZ_SOURCES` index (compiles it on
/// first use). Scoped runs resolve indices via [`stdlib_program_closure`]
/// and touch only the needed slots — an import-free program compiles zero
/// stdlib sources.
pub fn zz_stdlib_program_at(idx: usize) -> &'static TypedProgram {
    program_at(idx)
}

/// Number of embedded pure-ZZ sources (indices `0..LEN` are valid).
pub fn zz_stdlib_program_count() -> usize {
    ZZ_SOURCES.len()
}

/// Direct runtime dependencies between pure-ZZ stdlib programs, by index
/// into [`ZZ_SOURCES`]: `PROGRAM_DEPS[P]` lists programs whose runtime
/// definitions P may call (e.g. `bytes` defines `str.*` helpers on top of
/// `vec` natives and `str` helpers).
///
/// Keep in sync with the sources: `stdlib_program_closure_covers_cross_calls`
/// extracts every dotted call from each source and fails on any edge this
/// table misses (over-inclusion is safe, under-inclusion breaks scoped runs).
const PROGRAM_DEPS: &[&[usize]] = &[
    &[],  // 0 str/mod.zz
    &[],  // 1 math/mod.zz
    &[],  // 2 collections/vec.zz
    &[],  // 3 json/mod.zz
    &[],  // 4 regexp/mod.zz
    &[],  // 5 time/mod.zz
    &[],  // 6 args/mod.zz
    &[],  // 7 colors/mod.zz
    &[],  // 8 path/mod.zz
    &[],  // 9 http/mod.zz
    &[],  // 10 collections/map.zz
    &[],  // 11 collections/set.zz
    &[],  // 12 dec/mod.zz
    &[],  // 13 bytes/mod.zz
    &[3], // 14 csv/mod.zz (csv.to_json via json.parse_or_null)
];

/// Loader modules each program serves, by `ZZ_SOURCES` index. Used by
/// [`stdlib_program_closure`] WITHOUT compiling anything — compiling to
/// learn ownership would defeat scoped runs. Almost always the namespaces
/// the file declares (`bytes` also serves `str` builders; `args` serves
/// its module via the `ArgsParser` impl block).
/// `stdlib_program_ownership_table_exact` asserts every declared dotted
/// namespace is served and every served name is a real loader module.
const PROGRAM_MODULES: &[&[&str]] = &[
    &["str"],          // 0 str/mod.zz
    &["math"],         // 1 math/mod.zz
    &["vec"],          // 2 collections/vec.zz
    &["json"],         // 3 json/mod.zz
    &["regexp"],       // 4 regexp/mod.zz
    &["time"],         // 5 time/mod.zz
    &["args"],         // 6 args/mod.zz
    &["colors"],       // 7 colors/mod.zz
    &["path"],         // 8 path/mod.zz
    &["http"],         // 9 http/mod.zz
    &["map"],          // 10 collections/map.zz
    &["set"],          // 11 collections/set.zz
    &["dec"],          // 12 dec/mod.zz
    &["bytes", "str"], // 13 bytes/mod.zz (also defines str builders)
    &["csv"],          // 14 csv/mod.zz
];

/// Dotted short names one program declares (`ns.name`), test oracle for
/// the cross-call audit and the serves-table check.
#[cfg(test)]
fn own_func_names(tp: &TypedProgram) -> Vec<String> {
    tp.program
        .stmts
        .iter()
        .filter_map(|s| match s {
            zz_frontend::ast::Stmt::Func { name, .. } => Some(name.join(".")),
            _ => None,
        })
        .collect()
}

/// Programs that must execute to serve the given loader-module namespaces:
/// every program defining one of the namespaces, plus the [`PROGRAM_DEPS`]
/// fixpoint (a program's runtime callees must run too). Sorted indices.
/// Unknown modules (native-only like `sqlz`) contribute nothing — natives
/// are always registered.
pub fn stdlib_program_closure(modules: &[String]) -> Vec<usize> {
    // Module → defining programs via the static ownership table (no
    // compilation — calling `zz_stdlib_programs()` here would compile all
    // fifteen sources on every run, defeating scoped execution).
    let mut need = vec![false; PROGRAM_MODULES.len()];
    for (idx, namespaces) in PROGRAM_MODULES.iter().enumerate() {
        if namespaces.iter().any(|ns| modules.iter().any(|m| m == ns)) {
            need[idx] = true;
        }
    }
    // Fixpoint over direct deps (15 nodes — trivial loop, no worklist).
    loop {
        let mut changed = false;
        for (idx, deps) in PROGRAM_DEPS.iter().enumerate() {
            if need[idx] {
                for &d in *deps {
                    if d < need.len() && !need[d] {
                        need[d] = true;
                        changed = true;
                    }
                }
            }
        }
        if !changed {
            break;
        }
    }
    need.iter()
        .enumerate()
        .filter_map(|(i, n)| n.then_some(i))
        .collect()
}

/// Define `std.*` canonical aliases for pure-ZZ stdlib functions.
///
/// The embedded sources declare short names (`func json.is_null`), so
/// running them binds only `json.is_null` — but the checker advertises
/// both spellings (`std.json.is_null` + `json.is_null`). Calls to the
/// canonical form type-check yet fail at runtime with
/// "undefined variable". After the programs have run, mirror every dotted
/// env binding `k` to `std.{k}` when the checker knows that signature and
/// nothing is bound there yet. Both the value env and the function table
/// are mirrored (natives live in both maps; keep the same discipline).
/// Idempotent: re-running skips keys that already exist.
pub fn define_canonical_purezz_aliases(
    env: &mut zz_runtime::EnvLink,
    funcs: &mut HashMap<String, zz_runtime::FuncValue>,
) {
    let sigs = crate::funcs::stdlib_funcs_cached();
    let flat = env.flatten();
    for (k, v) in &flat {
        if !k.contains('.') || k.starts_with("std.") {
            continue;
        }
        let canon = format!("std.{k}");
        if !sigs.contains_key(&canon) {
            continue;
        }
        if env.get(&canon).is_some() {
            continue;
        }
        env.define(&canon, v.clone());
        if let zz_runtime::Value::Func(fv) = v {
            funcs.entry(canon).or_insert_with(|| (**fv).clone());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compile_pure_zz_stdlib() {
        let programs = zz_stdlib_programs();
        // Should have compiled fifteen modules (str, math, collections/vec,
        // json, regexp, time, args, colors, path, http, map, set, dec,
        // bytes, csv). TOML lives as an external package (~/Projects/toml),
        // not in the stdlib.
        assert_eq!(programs.len(), 15, "expected 15 pure-ZZ stdlib modules");
        // Each module should have a non-empty types map.
        for (i, tp) in programs.iter().enumerate() {
            assert!(
                !tp.types.is_empty(),
                "module {i} should have resolved types"
            );
        }
    }

    #[test]
    fn pure_zz_str_has_expected_functions() {
        let programs = zz_stdlib_programs();
        let str_prog = &programs[0]; // str/mod.zz
        assert!(
            str_prog.funcs.contains_key("str.repeat"),
            "str.repeat should be defined"
        );
        assert!(
            str_prog.funcs.contains_key("str.count"),
            "str.count should be defined"
        );
        assert!(
            str_prog.funcs.contains_key("str.is_empty"),
            "str.is_empty should be defined"
        );
        assert!(
            str_prog.funcs.contains_key("str.reverse"),
            "str.reverse should be defined"
        );
        assert!(
            str_prog.funcs.contains_key("str.pad_left"),
            "str.pad_left should be defined"
        );
        assert!(
            str_prog.funcs.contains_key("str.pad_right"),
            "str.pad_right should be defined"
        );
    }

    #[test]
    fn pure_zz_math_has_expected_functions() {
        let programs = zz_stdlib_programs();
        let math_prog = &programs[1]; // math/mod.zz
        assert!(
            math_prog.funcs.contains_key("math.sum"),
            "math.sum should be defined"
        );
        assert!(
            math_prog.funcs.contains_key("math.product"),
            "math.product should be defined"
        );
        assert!(
            math_prog.funcs.contains_key("math.count"),
            "math.count should be defined"
        );
        assert!(
            math_prog.funcs.contains_key("math.min"),
            "math.min should be defined"
        );
        assert!(
            math_prog.funcs.contains_key("math.max"),
            "math.max should be defined"
        );
        assert!(
            math_prog.funcs.contains_key("math.is_even"),
            "math.is_even should be defined"
        );
        assert!(
            math_prog.funcs.contains_key("math.is_odd"),
            "math.is_odd should be defined"
        );
        assert!(
            math_prog.funcs.contains_key("math.min_arr"),
            "math.min_arr should be defined"
        );
        assert!(
            math_prog.funcs.contains_key("math.max_arr"),
            "math.max_arr should be defined"
        );
        // Float aggregation
        assert!(
            math_prog.funcs.contains_key("math.sum_f"),
            "math.sum_f should be defined"
        );
        assert!(
            math_prog.funcs.contains_key("math.product_f"),
            "math.product_f should be defined"
        );
        assert!(
            math_prog.funcs.contains_key("math.mean_f"),
            "math.mean_f should be defined"
        );
        assert!(
            math_prog.funcs.contains_key("math.median_f"),
            "math.median_f should be defined"
        );
        // These are implemented in pure ZZ but have the same names as native
        // functions; the native versions are used at runtime.
        assert!(
            math_prog.funcs.contains_key("math.abs"),
            "math.abs should be defined (pure ZZ impl, native runtime)"
        );
        assert!(
            math_prog.funcs.contains_key("math.gcd"),
            "math.gcd should be defined (pure ZZ impl, native runtime)"
        );
        assert!(
            math_prog.funcs.contains_key("math.lcm"),
            "math.lcm should be defined (pure ZZ impl, native runtime)"
        );
        assert!(
            math_prog.funcs.contains_key("math.factorial"),
            "math.factorial should be defined (pure ZZ impl, native runtime)"
        );
        assert!(
            math_prog.funcs.contains_key("math.clamp"),
            "math.clamp should be defined (pure ZZ impl, native runtime)"
        );
        assert!(
            math_prog.funcs.contains_key("math.signum"),
            "math.signum should be defined (pure ZZ impl, native runtime)"
        );
    }

    #[test]
    fn pure_zz_vec_has_expected_functions() {
        let programs = zz_stdlib_programs();
        let vec_prog = &programs[2]; // collections/vec.zz
        assert!(
            vec_prog.funcs.contains_key("vec.fold"),
            "vec.fold should be defined"
        );
        assert!(
            vec_prog.funcs.contains_key("vec.sum"),
            "vec.sum should be defined"
        );
        assert!(
            vec_prog.funcs.contains_key("vec.product"),
            "vec.product should be defined"
        );
        assert!(
            vec_prog.funcs.contains_key("vec.min_val"),
            "vec.min_val should be defined"
        );
        assert!(
            vec_prog.funcs.contains_key("vec.max_val"),
            "vec.max_val should be defined"
        );
        assert!(
            vec_prog.funcs.contains_key("vec.concat"),
            "vec.concat should be defined"
        );
        assert!(
            vec_prog.funcs.contains_key("vec.flatten"),
            "vec.flatten should be defined"
        );
        assert!(
            vec_prog.funcs.contains_key("vec.index_of"),
            "vec.index_of should be defined"
        );
        assert!(
            vec_prog.funcs.contains_key("vec.last_index_of"),
            "vec.last_index_of should be defined"
        );
        // Float aggregation
        assert!(
            vec_prog.funcs.contains_key("vec.sum_f"),
            "vec.sum_f should be defined"
        );
        assert!(
            vec_prog.funcs.contains_key("vec.product_f"),
            "vec.product_f should be defined"
        );
        assert!(
            vec_prog.funcs.contains_key("vec.last_index_of"),
            "vec.last_index_of should be defined"
        );
    }

    #[test]
    fn pure_zz_regexp_has_expected_functions() {
        let programs = zz_stdlib_programs();
        let regexp_prog = &programs[4]; // regexp/mod.zz
        assert!(
            regexp_prog.funcs.contains_key("Regexp.new"),
            "Regexp.new should be defined"
        );
        assert!(
            regexp_prog.funcs.contains_key("regexp.is_email"),
            "regexp.is_email should be defined"
        );
    }

    #[test]
    fn pure_zz_time_has_duration() {
        let programs = zz_stdlib_programs();
        let time_prog = &programs[5]; // time/mod.zz
        for name in [
            "time.micros",
            "time.millis",
            "time.secs",
            "time.to_micros",
            "time.to_millis",
            "time.to_secs",
            "time.to_nanos",
            "time.sleep",
        ] {
            assert!(
                time_prog.funcs.contains_key(name),
                "{name} should be defined"
            );
        }
    }

    #[test]
    fn pure_zz_path_has_helpers() {
        let programs = zz_stdlib_programs();
        let path_prog = &programs[8]; // path/mod.zz
        for name in [
            "path.join",
            "path.join_all",
            "path.normalize",
            "path.basename",
            "path.dirname",
            "path.is_absolute",
            "path.extension",
        ] {
            assert!(
                path_prog.funcs.contains_key(name),
                "{name} should be defined"
            );
        }
    }

    #[test]
    fn pure_zz_http_has_helpers() {
        let programs = zz_stdlib_programs();
        let http_prog = &programs[9]; // http/mod.zz
        for name in [
            "http.use",
            "http.ok",
            "http.created",
            "http.not_found",
            "http.redirect",
            "http.cors",
            "http.secure_headers",
            "http.secure_header_dict",
            "http.csrf_token",
            "http.csrf_check",
            "http.request_id",
        ] {
            assert!(
                http_prog.funcs.contains_key(name),
                "{name} should be defined"
            );
        }
    }

    #[test]
    fn pure_zz_args_has_parser() {
        let programs = zz_stdlib_programs();
        let args_prog = &programs[6]; // args/mod.zz
        assert!(
            args_prog.funcs.contains_key("ArgsParser.new"),
            "ArgsParser.new should be defined"
        );
    }

    #[test]
    fn pure_zz_new_modules_have_expected_functions() {
        let programs = zz_stdlib_programs();
        // map (collections/map.zz, index 10)
        let map_prog = &programs[10];
        for name in [
            "map.has",
            "map.get_or",
            "map.keys",
            "map.values",
            "map.merge",
            "map.remove",
        ] {
            assert!(
                map_prog.funcs.contains_key(name),
                "{name} should be defined"
            );
        }
        // set (collections/set.zz, index 11)
        let set_prog = &programs[11];
        for name in [
            "set.has",
            "set.insert",
            "set.union",
            "set.intersect",
            "set.diff",
        ] {
            assert!(
                set_prog.funcs.contains_key(name),
                "{name} should be defined"
            );
        }
        // dec (dec/mod.zz, index 12)
        let dec_prog = &programs[12];
        for name in ["dec.add", "dec.cmp", "dec.format", "dec.is_valid"] {
            assert!(
                dec_prog.funcs.contains_key(name),
                "{name} should be defined"
            );
        }
        // bytes builders (bytes/mod.zz, index 13)
        let bytes_prog = &programs[13];
        for name in [
            "bytes.builder",
            "bytes.push_byte",
            "str.builder",
            "str.finish",
        ] {
            assert!(
                bytes_prog.funcs.contains_key(name),
                "{name} should be defined"
            );
        }
        // csv (csv/mod.zz, index 14)
        let csv_prog = &programs[14];
        for name in ["csv.parse", "csv.stringify", "csv.to_json", "csv.header"] {
            assert!(
                csv_prog.funcs.contains_key(name),
                "{name} should be defined"
            );
        }
    }

    #[test]
    fn pure_zz_colors_has_palette() {
        let programs = zz_stdlib_programs();
        let colors_prog = &programs[7]; // colors/mod.zz
        for name in [
            "colors.red",
            "colors.green",
            "colors.blue",
            "colors.yellow",
            "colors.magenta",
            "colors.cyan",
            "colors.white",
            "colors.black",
            "colors.bright_red",
            "colors.bg_blue",
            "colors.bold",
            "colors.dim",
            "colors.italic",
            "colors.underline",
            "colors.reset",
            "colors.strip",
            "colors.rgb",
            "colors.bg_rgb",
            "colors.hex",
            "colors.hex6",
            "colors.hex3",
            "colors.clamp255",
        ] {
            assert!(
                colors_prog.funcs.contains_key(name),
                "{name} should be defined"
            );
        }
    }

    /// `ns.name(` call sites in one source (best-effort tokenizer for the
    /// audit below — false positives only over-include, never unsound).
    fn dotted_calls(src: &str) -> Vec<(String, String)> {
        let b = src.as_bytes();
        let mut out = Vec::new();
        let mut i = 0;
        let is_ident = |c: u8| c.is_ascii_alphanumeric() || c == b'_';
        let is_head = |c: u8| c.is_ascii_alphabetic() || c == b'_';
        while i < b.len() {
            if !is_head(b[i]) {
                i += 1;
                continue;
            }
            let mut parts = Vec::new();
            loop {
                let s = i;
                while i < b.len() && is_ident(b[i]) {
                    i += 1;
                }
                parts.push(src[s..i].to_string());
                if i + 1 < b.len() && b[i] == b'.' && is_head(b[i + 1]) {
                    i += 1;
                    continue;
                }
                break;
            }
            let mut k = i;
            while k < b.len() && matches!(b[k], b' ' | b'\t' | b'\n' | b'\r') {
                k += 1;
            }
            // Call position: `(` directly, or generic args `<T>(`.
            let is_call = k < b.len()
                && (b[k] == b'(' || (b[k] == b'<' && k + 1 < b.len() && is_head(b[k + 1])));
            if is_call && parts.len() >= 2 {
                let name = parts.pop().unwrap();
                out.push((parts.join("."), name));
            }
        }
        out
    }

    /// The [`super::PROGRAM_DEPS`] table must cover every pure-ZZ
    /// cross-program call: if program P calls `ns.name` and `ns.name` is
    /// defined by program Q, the closure of {P} must contain Q. Scoped
    /// `zz run` executions rely on this — update the table when stdlib
    /// sources gain cross-module helpers (this test names the missing edge).
    #[test]
    fn stdlib_program_closure_covers_cross_calls() {
        let programs = zz_stdlib_programs();
        // Fully-qualified pure-ZZ func → defining program, from OWN
        // declarations (funcs maps carry the seed — see own_func_names).
        let mut definer: std::collections::HashMap<String, usize> =
            std::collections::HashMap::new();
        for (idx, tp) in programs.iter().enumerate() {
            for key in super::own_func_names(tp) {
                definer.insert(key.clone(), idx);
                definer.insert(format!("std.{key}"), idx);
            }
        }
        // Own namespaces per program (for the closure request below).
        let own_ns: Vec<Vec<String>> = programs
            .iter()
            .map(|tp| {
                let mut v: Vec<String> = super::own_func_names(tp)
                    .iter()
                    .map(|k| k.split('.').next().unwrap_or("").to_string())
                    .collect();
                v.sort();
                v.dedup();
                v
            })
            .collect();
        let mut missing = Vec::new();
        for (idx, (src_name, src)) in ZZ_SOURCES.iter().enumerate() {
            for (ns, name) in dotted_calls(src) {
                let norm = ns.strip_prefix("std.").unwrap_or(&ns);
                let key = format!("{norm}.{name}");
                let Some(&q) = definer.get(&key) else {
                    continue;
                }; // native/unknown
                if q == idx {
                    continue;
                }
                // Value-method lookalikes (`x.len(` where x is a local):
                // conservative — still demand the edge (safe direction).
                let closure = super::stdlib_program_closure(&own_ns[idx]);
                if !closure.contains(&q) {
                    missing.push(format!(
                        "{src_name} calls {key} (program {q}): add it to PROGRAM_DEPS[{idx}]"
                    ));
                }
            }
        }
        missing.sort();
        missing.dedup();
        assert!(
            missing.is_empty(),
            "PROGRAM_DEPS misses {} edge(s):\n  {}",
            missing.len(),
            missing.join("\n  ")
        );
    }

    /// Spot-check the closure shape: requesting one leaf module pulls its
    /// runtime deps but not the world; unknown (native-only) modules pull
    /// nothing.
    #[test]
    fn stdlib_program_closure_shape() {
        let only_str = super::stdlib_program_closure(&["str".to_string()]);
        // str helpers live in str/mod.zz (0) and bytes/mod.zz (13).
        assert_eq!(only_str, vec![0, 13], "str closure, got {only_str:?}");
        let none = super::stdlib_program_closure(&["sqlz".to_string()]);
        assert!(none.is_empty(), "native-only modules need no programs");
        let empty = super::stdlib_program_closure(&[]);
        assert!(empty.is_empty(), "no imports need no programs");
    }

    /// The static [`super::PROGRAM_MODULES`] table must cover every dotted
    /// namespace the sources declare, and name only real loader modules:
    /// the runtime closure never compiles, so a missing entry silently
    /// drops executable code on scoped runs (extra entries only cost time).
    /// Non-dotted declarations (impl methods like `ArgsParser.new`) ride
    /// with their file's module — reachable only through its import.
    #[test]
    fn stdlib_program_ownership_table_exact() {
        let programs = zz_stdlib_programs();
        assert_eq!(
            programs.len(),
            super::PROGRAM_MODULES.len(),
            "SLOTS/table must cover every source"
        );
        let valid: std::collections::HashSet<&str> =
            crate::STDLIB_MODULES.iter().copied().collect();
        for (idx, tp) in programs.iter().enumerate() {
            for key in super::own_func_names(tp) {
                if let Some((ns, _)) = key.split_once('.') {
                    assert!(
                        super::PROGRAM_MODULES[idx].contains(&ns),
                        "program {idx} declares {key} but serves {:?}",
                        super::PROGRAM_MODULES[idx]
                    );
                }
            }
            for served in super::PROGRAM_MODULES[idx] {
                assert!(
                    valid.contains(served),
                    "program {idx} serves unknown module {served}"
                );
            }
        }
    }
}
