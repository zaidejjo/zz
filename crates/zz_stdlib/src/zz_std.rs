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
];

/// Compiled pure-ZZ stdlib programs, computed once.
static COMPILED: OnceLock<Vec<TypedProgram>> = OnceLock::new();

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
    let initial_funcs = crate::funcs::stdlib_funcs();
    let initial_structs: HashMap<String, StructSig> = HashMap::new();
    let initial_bindings: HashMap<String, Type> = HashMap::new();

    let mut programs = Vec::new();
    for (name, source) in ZZ_SOURCES {
        match compile_one(
            name,
            source,
            &initial_bindings,
            &initial_funcs,
            &initial_structs,
        ) {
            Ok(tp) => programs.push(tp),
            Err(e) => {
                // During development, panic on compile errors so they are
                // surfaced immediately rather than silently swallowed.
                panic!("pure-ZZ stdlib compilation failed: {e}");
            }
        }
    }
    programs
}

/// Get the compiled pure-ZZ stdlib programs.
///
/// Returns a reference to a `Vec<TypedProgram>` that is computed exactly once.
/// Each element corresponds to one embedded `.zz` file, in compilation order.
pub fn zz_stdlib_programs() -> &'static [TypedProgram] {
    COMPILED.get_or_init(compile_all)
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
    let sigs = crate::funcs::stdlib_funcs();
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
        // Should have compiled ten modules (str, math, collections/vec,
        // json, regexp, time, args, colors, path, http).
        assert_eq!(programs.len(), 10, "expected 10 pure-ZZ stdlib modules");
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
}
