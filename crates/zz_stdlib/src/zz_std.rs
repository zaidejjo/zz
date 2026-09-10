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

/// All embedded source files, in compilation order.
const ZZ_SOURCES: &[(&str, &str)] = &[
    ("std:str.mod.zz", STR_MOD_ZZ),
    ("std:math.mod.zz", MATH_MOD_ZZ),
    ("std:collections.vec.zz", VEC_ZZ),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compile_pure_zz_stdlib() {
        let programs = zz_stdlib_programs();
        // Should have compiled three modules (str, math, collections/vec).
        assert_eq!(programs.len(), 3, "expected 3 pure-ZZ stdlib modules");
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
}
