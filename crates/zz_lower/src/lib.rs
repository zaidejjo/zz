//! HIR → C lowering for the native AOT backend.
//!
//! Consumes the DCE-pruned [`TypedProgram`] and [`ReachableSet`] from
//! zz_hir and produces a single self-contained C translation unit (plus the
//! embedded runtime). A `cc` invocation compiles it to a native binary.
//!
//! The lowering logic is split across domain modules:
//!   - [`context`]: codegen state, scopes, scalar classification
//!   - [`expr`]: expression lowering (literals, binops, calls, fields)
//!   - [`stmt`]: statement lowering (decls, assigns, if/else, match, loops)
//!   - [`fn_decl`]: function generation, signatures, struct declarations
//!   - [`param_escape`]: per-parameter escape analysis for call ownership

mod context;
mod expr;
mod extern_call;
mod fn_decl;
mod green;
mod move_elide;
mod param_escape;
mod stmt;
pub mod units;

use zz_codegen_ffi::native_impl;
use zz_codegen_runtime::{RUNTIME_C, RUNTIME_H};
use zz_frontend::ast::{Expr, Pattern, Stmt};

pub use context::{Lowerer, NameCtx};

// Internal helpers shared across the lowering submodules.
pub(crate) use context::{
    auto_box, binop_runtime_op, box_scalar_operand, emit_guard_expr, is_dup_safe, is_simple_ident,
    scalar_operand_c, scalar_operand_type,
};

/// Result of lowering.
#[derive(Debug)]
pub struct LoweredC {
    /// The full generated C source (runtime + user code + main glue).
    pub source: String,
    /// True when the program calls natives provided by the Rust static
    /// library; the build must link `libzz_native_rt.a`.
    pub needs_native_rt: bool,
    /// True when the program can reach the Postgres backend (any sqlz /
    /// pg spelling): the link must force-extract the PG objects from the
    /// archive (`-u`), since the C dispatcher references them weakly and
    /// weak refs alone never pull archive members.
    pub needs_pg_link: bool,
    /// True when any typed node in the program can hold a float: float
    /// Display (`println`, interpolation, `str()`) routes through the
    /// Rust core (`zz_float_format_raw`), so the link must force-extract
    /// that object (`-u`). The type scan is sound — there is no `Any` /
    /// dynamic type, and JSON values print through their own (already
    /// agreeing) stringify path, never float Display.
    pub needs_float_fmt: bool,
    /// True when reachable natives lower to curl-backed C client calls:
    /// the link must add `-lcurl`. Server/route-only programs skip it.
    pub needs_curl: bool,
    /// True when reachable natives lower to sqlite-backed C calls: the
    /// link must add `-lsqlite3`. Programs that never query skip it
    /// (previously every binary carried the dependency via the
    /// single-TU archive + `--as-needed` ordering).
    pub needs_sqlite: bool,
}

impl LoweredC {
    /// Fold the link gates discovered while lowering into `opts`.
    ///
    /// Callers never clear a user-requested flag, so this only ORs in what
    /// the program actually needs. Every build path (single TU, per-unit,
    /// chunk backend) funnels through this, so adding a future gate (OpenMP,
    /// sanitizer runtime, ...) is one edit here instead of a copy in each
    /// caller.
    pub fn apply_link_needs(&self, opts: &mut zz_codegen_types::BuildOptions) {
        merge_link_needs(
            opts,
            self.needs_native_rt,
            self.needs_pg_link,
            self.needs_float_fmt,
            self.needs_curl,
            self.needs_sqlite,
        );
    }
}

/// Shared implementation for [`LoweredC`] and [`units::LoweredUnits`].
fn merge_link_needs(
    opts: &mut zz_codegen_types::BuildOptions,
    native_rt: bool,
    pg_link: bool,
    float_fmt: bool,
    curl: bool,
    sqlite: bool,
) {
    opts.native_rt |= native_rt;
    opts.pg_link |= pg_link;
    opts.float_link |= float_fmt;
    opts.curl_link |= curl;
    opts.sqlite_link |= sqlite;
}

/// True when a resolved type can carry an `f64` to a Display site.
/// Recursive over every compound shape (no `Any` exists, so a whole-
/// program scan of `TypedProgram` types is a sound float-format gate).
pub(crate) fn type_has_float(t: &zz_checker::Type) -> bool {
    match t {
        zz_checker::Type::Float => true,
        zz_checker::Type::Tuple(ts) | zz_checker::Type::Union(ts) => ts.iter().any(type_has_float),
        zz_checker::Type::Option(b) | zz_checker::Type::Array(b) | zz_checker::Type::Range(b) => {
            type_has_float(b)
        }
        zz_checker::Type::Result(a, b) | zz_checker::Type::Dict(a, b) => {
            type_has_float(a) || type_has_float(b)
        }
        zz_checker::Type::Func(params, ret) => {
            params.iter().any(type_has_float) || type_has_float(ret)
        }
        zz_checker::Type::Struct(_, args) | zz_checker::Type::Enum(_, args) => {
            args.iter().any(type_has_float)
        }
        zz_checker::Type::Ptr { inner, .. } => type_has_float(inner),
        _ => false,
    }
}

/// Mangle a zz qualified name to a C identifier.
pub use zz_codegen_types::mangle;

/// Lower a `std.math` numeric constant path (`std.math.PI`, `math.PI`,
/// or bare `PI` from `import std.math(PI)`) to a `zz_float(...)` literal.
/// Constants are true values — the checker rejects calls like `math.PI()`,
/// so value position is the only valid use.
/// Bare single-component names consult the same table because the checker
/// only lets selective-import consts reach lowering unresolved this way
/// (locals/globals resolve earlier; anything else errors upstream).
/// Dotted non-`std.math` paths (user modules) never match.
/// Returns `None` for anything that is not a known constant spelling.
pub(crate) fn math_const_c_literal(joined: &str) -> Option<String> {
    let leaf = joined
        .strip_prefix("std.math.")
        .or_else(|| joined.strip_prefix("math."))
        .unwrap_or(joined);
    if leaf.contains('.') {
        return None;
    }
    // Shortest round-trip decimals, matching Rust's `to_string()` output.
    let num = match leaf {
        "PI" => std::f64::consts::PI.to_string(),
        "E" => std::f64::consts::E.to_string(),
        "TAU" => std::f64::consts::TAU.to_string(),
        "SQRT_2" => std::f64::consts::SQRT_2.to_string(),
        "SQRT_1_2" => std::f64::consts::FRAC_1_SQRT_2.to_string(),
        "LN_2" => std::f64::consts::LN_2.to_string(),
        "LN_10" => std::f64::consts::LN_10.to_string(),
        "LOG10_E" => std::f64::consts::LOG10_E.to_string(),
        "LOG2_E" => std::f64::consts::LOG2_E.to_string(),
        // No decimal literal for non-finite values: use the same
        // expressions as the C runtime (`zz_math_inf` / `zz_math_nan`).
        "INF" => "(1.0/0.0)".to_string(),
        "NAN" => "(0.0/0.0)".to_string(),
        _ => return None,
    };
    Some(format!("zz_float({num})"))
}

/// Extract the raw C string literal body from a `zz_str_static("...")`
/// expression emitted by `emit_str_literal`. The wrapper is
/// `zz_str_static( <literal> )` — we strip just the function-call syntax
/// and leave the literal (including its surrounding double quotes) intact.
fn extract_c_literal(emit: &str) -> &str {
    const PREFIX: &str = "zz_str_static(";
    const SUFFIX: &str = ")";
    if let Some(rest) = emit.strip_prefix(PREFIX) {
        if let Some(body) = rest.strip_suffix(SUFFIX) {
            return body;
        }
    }
    // Fallback: emit an empty literal; the runtime will no-op.
    "\"\""
}

/// Expressions that are pure leaves (no side effects, no captured value).
fn is_leaf_expr(e: &Expr) -> bool {
    matches!(
        e,
        Expr::Int { .. }
            | Expr::Float { .. }
            | Expr::Str { .. }
            | Expr::Bool { .. }
            | Expr::Ident { .. }
            | Expr::Path { .. }
            | Expr::Binary { .. }
            | Expr::Paren { .. }
    )
}

/// Expressions that must be captured into a temp before return.
fn needs_temp(e: &Expr) -> bool {
    matches!(
        e,
        Expr::Unary { .. }
            | Expr::If { .. }
            | Expr::Block(_)
            | Expr::Closure { .. }
            | Expr::Array { .. }
            | Expr::Range { .. }
            | Expr::Fmt { .. }
            | Expr::Tuple { .. }
            | Expr::Dict { .. }
            | Expr::Variant { .. }
            | Expr::Field { .. }
            | Expr::Index { .. }
            | Expr::Slice { .. }
            | Expr::Match { .. }
    )
}

/// Count the number of heap-allocating expressions in a block.
/// Used to estimate arena pre-size for loop bodies.
fn count_allocating_exprs(block: &zz_hir::Block) -> usize {
    let mut count = 0;
    for stmt in &block.stmts {
        match stmt {
            zz_hir::Stmt::Decl { value, .. } => {
                count += count_allocating_in_expr(value);
            }
            zz_hir::Stmt::Expr(e) => {
                count += count_allocating_in_expr(e);
            }
            zz_hir::Stmt::For { body, .. } => {
                count += count_allocating_exprs(body);
            }
            _ => {}
        }
    }
    count
}

fn count_allocating_in_expr(e: &zz_hir::Expr) -> usize {
    match e {
        zz_hir::Expr::Array { elems, .. } => {
            1 + elems.iter().map(count_allocating_in_expr).sum::<usize>()
        }
        zz_hir::Expr::Dict { entries, .. } => {
            1 + entries
                .iter()
                .map(|(k, v)| count_allocating_in_expr(k) + count_allocating_in_expr(v))
                .sum::<usize>()
        }
        zz_hir::Expr::Str { .. } => 1,
        zz_hir::Expr::Fmt { parts, .. } => {
            1 + parts
                .iter()
                .filter_map(|p| match p {
                    zz_frontend::ast::FmtPart::Expr(inner, _) => {
                        Some(count_allocating_in_expr(inner))
                    }
                    _ => None,
                })
                .sum::<usize>()
        }
        zz_hir::Expr::Block(b) => count_allocating_exprs(b),
        zz_hir::Expr::Binary { left, right, .. } => {
            count_allocating_in_expr(left) + count_allocating_in_expr(right)
        }
        zz_hir::Expr::Call { args, .. } => {
            1 + args.iter().map(count_allocating_in_expr).sum::<usize>()
        }
        _ => 0,
    }
}

impl Lowerer {
    /// True only for real `impl` methods (`{Type}.{method}` where the first
    /// param is exactly that struct type). A free function that merely
    /// TAKES a struct first (e.g. `mint_token(gu: GhUser)`) must use the
    /// plain `(args, argc)` shape — the old first-param-is-struct heuristic
    /// gave it a `(self, args, argc)` declaration with a 2-argument call.
    pub(crate) fn is_impl_method(&self, fname: &str) -> bool {
        let Some(sig) = self.tp.funcs.get(fname) else {
            return false;
        };
        let Some((_, first)) = sig.params.first() else {
            return false;
        };
        let zz_checker::Type::Struct(sname, _) = first else {
            return false;
        };
        let method = fname.rsplit('.').next().unwrap_or(fname);
        fname == format!("{sname}.{method}")
    }

    pub fn lower(&self) -> LoweredC {
        let mut funcs = String::new();
        let mut body = String::new();
        // Module-level vars become C globals so every function can see them.
        // `collect_globals` gathers all top-level Decl names with checker types.
        let globals = self.collect_globals();
        let mut globals_decl = String::new();
        for (_, cid, ctype, _) in &globals {
            globals_decl.push_str(&format!("static {ctype} {cid};\n"));
        }
        // One NameCtx shared across ALL top-level statements, pre-seeded with
        // globals. Top-level Decl assigns into its global (no local redecl).
        let mut names = NameCtx::new();
        self.seed_globals(&mut names);
        // `zz_main` is a C frame too: a closure created in top-level code
        // cannot read its C locals. Bindings that live only in that frame —
        // loop variables and block locals — must be heap cells when a nested
        // closure uses them, exactly like function locals. Module globals are
        // excluded by the walker (closures read those directly), so this set
        // only ever names inline-scope bindings.
        names.capture_set = zz_hir::captured_in_stmts(&[], self.tp.stmts(), &self.global_set);
        self.seed_scalar_fns(&mut names);
        let mut global_init_done: std::collections::HashSet<String> =
            std::collections::HashSet::new();

        // Per-function bump arena for zz_main (top-level code). Non-static,
        // non-escaping top-level allocations bump-allocate here and are
        // reclaimed in O(1) when zz_main returns via cleanup attribute.
        body.push_str("    zz_arena _func_arena __attribute__((cleanup(zz_arena_destroy)));\n");
        body.push_str("    zz_arena_init(&_func_arena, 65536);\n");

        for stmt in self.tp.stmts() {
            match stmt {
                Stmt::Func {
                    name,
                    params,
                    body: b,
                    ..
                } => {
                    let fname = name.join(".");
                    if !self.reachable_funcs.contains(&fname) {
                        continue;
                    }
                    funcs.push_str(&self.emit_function(&fname, params, b));
                }
                Stmt::Impl { name, methods, .. } => {
                    // `impl T { func m(...) ... }` — methods are stored
                    // in `tp.funcs` under `<T>.m` (mirroring the
                    // checker/funcmap registration). Emit each reachable
                    // method as a C function with the same
                    // `<T>__m` mangling that other funcs get.
                    let tname = name.join(".");
                    for m in methods {
                        if let Stmt::Func {
                            name: mname,
                            params,
                            body: b,
                            ..
                        } = m
                        {
                            let fname = format!("{tname}.{}", mname.join("."));
                            if !self.reachable_funcs.contains(&fname) {
                                continue;
                            }
                            funcs.push_str(&self.emit_function(&fname, params, b));
                        }
                    }
                }
                Stmt::Struct { .. }
                | Stmt::TypeAlias { .. }
                | Stmt::Enum { .. }
                | Stmt::Import { .. } => {}
                Stmt::Decl { name, value, .. } => {
                    // Top-level `x := <rhs>` → assign into `zz_global_*`.
                    // The global is pre-declared; zz_main only initializes it.
                    let zz_name = &name.name;
                    let Some((gid, gtype)) = names
                        .globals
                        .get(zz_name)
                        .map(|(a, b)| (a.clone(), b.clone()))
                    else {
                        // Not a tracked global (should not happen): fall back.
                        let mut out = String::new();
                        self.emit_stmt(stmt, &mut names, &mut out, false);
                        body.push_str(&out);
                        continue;
                    };
                    let mut out = String::new();
                    // Struct-init fast path mirrors emit_stmt Decl.
                    if let Expr::StructInit {
                        name: struct_name, ..
                    } = value
                    {
                        if self.is_unboxed_struct(struct_name) {
                            // Globals are boxed (see collect_globals):
                            // emit the literal directly as a boxed
                            // object — no unboxed intermediate.
                            let boxed =
                                self.emit_boxed_value(struct_name, value, &mut names, &mut out);
                            out.push_str(&format!("    {gid} = {boxed};\n"));
                            body.push_str(&out);
                            global_init_done.insert(zz_name.clone());
                            continue;
                        }
                    }
                    let val = self.emit_expr(value, &mut names, &mut out);
                    // Raw-scalar detection: cast-prefixed expressions plus
                    // bare scalar globals (`zz_global_x` for int64_t /
                    // double / bool globals emit raw — no cast prefix —
                    // and must not gain a `.i` / `.f` / `.b` suffix).
                    let val_is_unboxed = val.starts_with("(int64_t)(")
                        || val.starts_with("(double)(")
                        || val.starts_with("(bool)(")
                        || names.globals.values().any(|(gid, gtype)| {
                            val == *gid && matches!(gtype.as_str(), "int64_t" | "double" | "bool")
                        });
                    let final_val = match gtype.as_str() {
                        "int64_t" if !val_is_unboxed => format!("({val}).i"),
                        "double" if !val_is_unboxed => format!("({val}).f"),
                        "bool" if !val_is_unboxed => format!("({val}).b"),
                        _ => val,
                    };
                    let first_init = !global_init_done.contains(zz_name);
                    if first_init {
                        out.push_str(&format!("    {gid} = {final_val};\n"));
                        global_init_done.insert(zz_name.clone());
                    } else if matches!(gtype.as_str(), "int64_t" | "double" | "bool") {
                        out.push_str(&format!("    {gid} = {final_val};\n"));
                    } else {
                        out.push_str(&format!("    zz_assign(&{gid}, {final_val});\n"));
                    }
                    if let Expr::Array { elems, .. } = value {
                        names.set_array_len(zz_name, elems.len());
                    }
                    body.push_str(&out);
                }
                Stmt::Destructure { pat, value, .. } => {
                    // Top-level `(a, b) := rhs` binds module globals
                    // (see `collect_globals`): assign each element into
                    // its `zz_global_*` so functions and closures
                    // compiled with a fresh scope can see the names.
                    let mut out = String::new();
                    self.emit_destructure_global(
                        pat,
                        value,
                        &mut names,
                        &mut out,
                        &mut global_init_done,
                    );
                    body.push_str(&out);
                }
                other => {
                    let mut out = String::new();
                    self.emit_stmt(other, &mut names, &mut out, false);
                    body.push_str(&out);
                }
            }
        }
        // Selective-import bare aliases (`double` for
        // `math_utils.double`): the callgraph edges the bare name while
        // the body is emitted under the canonical — forward the bare
        // symbol so the linker sees it. Skipped when the alias has its
        // own body or the canonical is an impl method (different ABI).
        let mut alias_pairs: Vec<(&String, &String)> = self.import_fn_aliases.iter().collect();
        alias_pairs.sort();
        for (alias, canonical) in alias_pairs {
            if !self.reachable_funcs.contains(alias) {
                continue;
            }
            if self.find_func_def(alias).is_some() {
                continue;
            }
            if self.find_func_def(canonical).is_none() {
                continue;
            }
            if self.is_impl_method(canonical) {
                continue;
            }
            funcs.push_str(&format!(
                "static zz_value zz_fn_{}(zz_value *args, size_t argc) {{ return zz_fn_{}(args, argc); }}\n",
                mangle(alias),
                mangle(canonical)
            ));
        }

        let main_decl = if self.reachable_funcs.contains(&self.entry_main) {
            // main exists: its return value decides the exit code
            // (`zz_main_result_code`: `.err` prints + exits 1).
            "return zz_call_into_main();".to_string()
        } else {
            String::new()
        };

        // Generate struct typedefs preamble
        let struct_preamble = self.lower_structs_preamble();

        // Auto-generated struct debug_string functions (VM-identical
        // display for println/f-strings/str()). Empty when the program
        // defines no unboxed structs.
        let struct_debug_fns = self.lower_struct_debug_fns();

        // Forward declarations: every reachable user-defined function (incl.
        // `impl` methods) is `static` in the emitted C, so the order of
        // definitions in `funcs` decides which callers see which callees.
        // Emitting one prototype per reachable function up front lets any
        // user fn call any other without forcing a topological sort of
        // `funcs` (which would otherwise be needed when f_a() is defined
        // before f_b() but calls into it). C is happy to take a redundant
        // prototype for a same-TU `static` function.
        //
        // Impl methods have a different signature: they take a struct
        // pointer as the first arg (the `self` receiver). Name-shape
        // checked (a free function taking a struct first is NOT a method).
        let mut forward_decls = String::new();
        let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
        for fname in &self.reachable_funcs {
            if !seen.insert(fname.clone()) {
                continue;
            }
            // Plugin `extern "C"` functions have no ZZ body: they are
            // declared (with real C types) in the extern prelude below
            // and called directly, never wrapped as `zz_fn_*`.
            if self
                .tp
                .funcs
                .get(fname)
                .map(|sig| sig.is_extern)
                .unwrap_or(false)
            {
                continue;
            }
            let first_struct_c = self
                .tp
                .funcs
                .get(fname)
                .and_then(|sig| sig.params.first().map(|(_, t)| t.clone()))
                .filter(|t| matches!(t, zz_checker::Type::Struct(_, _)))
                .filter(|_| self.is_impl_method(fname))
                .map(|t| self.type_to_c(&t));
            let proto = match first_struct_c {
                Some(sct) => format!(
                    "static zz_value zz_fn_{}({sct} *self, zz_value *args, size_t argc);\n",
                    mangle(fname)
                ),
                None => format!(
                    "static zz_value zz_fn_{}(zz_value *args, size_t argc);\n",
                    mangle(fname)
                ),
            };
            forward_decls.push_str(&proto);
            // Scalar-specialized functions also get an unboxed prototype
            // so callers earlier in the TU can route to `_u` directly.
            if self.specialized.contains(fname) {
                if let Some(sig) = self.tp.funcs.get(fname) {
                    let ret = Self::scalar_ctype(&sig.ret).unwrap_or("zz_value");
                    let params: Vec<String> = sig
                        .params
                        .iter()
                        .map(|(_, t)| Self::scalar_ctype(t).unwrap_or("zz_value").to_string())
                        .collect();
                    let psig = if params.is_empty() {
                        "void".to_string()
                    } else {
                        params.join(", ")
                    };
                    forward_decls.push_str(&format!(
                        "static {ret} zz_fn_{}_u({psig});\n",
                        mangle(fname)
                    ));
                }
            }
        }

        let closure_fwd = self.closure_forward_decls.borrow().join("");
        // Plugin extern prelude: real C declarations for reachable
        // `extern "C"` functions. Empty when none, keeping generated C
        // for existing programs byte-identical.
        let extern_pre = self.extern_prelude();
        let extern_section = if extern_pre.is_empty() {
            String::new()
        } else {
            format!("\n// ---- plugin externs ----\n{extern_pre}\n")
        };
        // FFI prelude: `extern` declarations for Rust-staticlib natives used
        // by this program. Empty when none, keeping generated C for existing
        // programs byte-identical.
        //
        // Selective-import bare names (`pid` from `import std.process(pid)`)
        // are recorded under their bare spelling by the callgraph, but
        // declarations and staticlib linkage are keyed by canonical name
        // (`std.process.pid`). Expand the set through the import alias map
        // so both spellings resolve; without this the call emits but its
        // `extern` decl (and link gate below) silently go missing.
        let expanded_natives: std::collections::HashSet<String> = {
            let mut set = self.reachable_natives.clone();
            for (alias, canonical) in &self.import_fn_aliases {
                if set.contains(alias) {
                    set.insert(canonical.clone());
                }
            }
            set
        };
        let ffi_pre = zz_codegen_ffi::ffi_prelude(&expanded_natives);
        let ffi_section = if ffi_pre.is_empty() {
            String::new()
        } else {
            format!("\n// ---- native-runtime FFI ----\n{ffi_pre}\n")
        };
        let needs_native_rt = zz_codegen_ffi::needs_native_rt(&expanded_natives);
        let runtime_c = if self.precompiled { "" } else { RUNTIME_C };
        let main_tail = if self.reachable_funcs.contains(&self.entry_main) {
            // main() exists: its stub returns the exit code already.
            String::new()
        } else {
            "    return 0;".to_string()
        };
        let source = format!(
            "{runtime_h}\n{runtime_c}\n{ffi_section}\n{extern_section}// ---- struct definitions ----\n{struct_preamble}\n{struct_debug_fns}\n// ---- module globals ----\n{globals_decl}\n// ---- forward declarations ----\n{forward_decls}{closure_fwd}\n// ---- generated code ----\n{funcs}\n// ---- closures ----\n{closure_defs}\nvoid zz_main(void) {{\n{body}}}\n\nint zz_call_main(void) {{\n    {main_decl}\n{main_tail}\n}}\n",
            runtime_h = RUNTIME_H,
            runtime_c = runtime_c,
            struct_preamble = struct_preamble,
            struct_debug_fns = struct_debug_fns,
            globals_decl = globals_decl,
            funcs = funcs,
            closure_defs = self.closure_defs.borrow().join("\n"),
            body = body,
            main_decl = main_decl,
        );
        // The runtime is split across modular headers/sources that
        // `#include` each other; since everything is inlined into a single
        // translation unit, strip every quoted include directive (system
        // includes like <math.h> stay).
        let source = zz_codegen_runtime::strip_quoted_includes(&source);

        // If main is reachable, append a stub that calls it (runtime does
        // not know the symbol; we emit a forward decl + call here).
        let with_main = if self.reachable_funcs.contains(&self.entry_main) {
            let m = format!("zz_fn_{}", mangle(&self.entry_main));
            // `main` with exactly one param (e.g. `main(cli_args: [str])`)
            // receives the process argv (argv[1..], mirroring the VM's
            // cli_args and `env.args()` — the runtime already stashes
            // argv in globals for `zz_env_args`, so reuse it). Anything
            // else keeps the old (NULL, 0) call so generated C for
            // existing programs stays byte-identical.
            let takes_argv = self
                .tp
                .funcs
                .get(&self.entry_main)
                .map(|sig| sig.params.len() == 1)
                .unwrap_or(false);
            if takes_argv {
                format!(
                    "\nstatic int zz_call_into_main(void);\nstatic int zz_call_into_main(void) {{ int _e = 0; zz_value _cli = zz_env_args(zz_unit(), &_e); zz_value _r = {m}(&_cli, 1); return zz_main_result_code(_r); }}\n"
                )
            } else {
                format!(
                    "\nstatic int zz_call_into_main(void);\nstatic int zz_call_into_main(void) {{ zz_value _r = {m}(NULL, 0); return zz_main_result_code(_r); }}\n"
                )
            }
        } else {
            String::new()
        };
        // Insert before the `int zz_call_main` body.
        let source = source.replacen(
            "int zz_call_main(void) {",
            &format!("{with_main}\nint zz_call_main(void) {{"),
            1,
        );

        LoweredC {
            source,
            needs_native_rt,
            needs_pg_link: zz_codegen_ffi::needs_pg_link(&expanded_natives),
            needs_float_fmt: self.tp.types.values().any(type_has_float)
                || self.tp.bindings.values().any(type_has_float)
                || self.tp.funcs.values().any(|s| {
                    s.params.iter().any(|(_, ty)| type_has_float(ty)) || type_has_float(&s.ret)
                }),
            needs_curl: zz_codegen_ffi::needs_curl_link(&expanded_natives),
            needs_sqlite: zz_codegen_ffi::needs_sqlite_link(&expanded_natives),
        }
    }

    /// Lower a top-level `(a, b) := rhs` destructuring declaration.
    /// Each bound name is a module global (see `collect_globals`):
    /// evaluate the RHS once into a temp, then assign each element
    /// into its `zz_global_*` — mirroring top-level `Decl` assignment
    /// (scalar extraction, `zz_assign` for refcounted values, init
    /// tracking). Called only for direct children of the program block;
    /// nested/function-body destructuring goes through `emit_stmt` and
    /// binds scope-locally instead.
    pub(crate) fn emit_destructure_global(
        &self,
        pat: &zz_frontend::ast::Pattern,
        value: &Expr,
        names: &mut NameCtx,
        out: &mut String,
        global_init_done: &mut std::collections::HashSet<String>,
    ) {
        let rhs = self.emit_expr(value, names, out);
        let tmp = names.fresh("__dtup");
        out.push_str(&format!("    zz_value {tmp} = {rhs};\n"));
        // Record checker types for method dispatch (mirrors Decl).
        for (name, ty) in self.destructure_binding_types(pat) {
            names.checker_types.insert(name, ty);
        }
        self.emit_destructure_global_pat(pat, &tmp, names, out, global_init_done);
    }

    /// Checker types of a destructuring pattern's bindings, from
    /// `tp.bindings` (populated by the checker for top-level names).
    fn destructure_binding_types(
        &self,
        pat: &zz_frontend::ast::Pattern,
    ) -> Vec<(String, zz_checker::Type)> {
        fn leaf(out: &mut Vec<(String, zz_checker::Type)>, lowerer: &Lowerer, p: &Pattern) {
            if let Pattern::Binding { name } = p {
                if let Some(ty) = lowerer.tp.bindings.get(&name.name) {
                    out.push((name.name.clone(), ty.clone()));
                }
            }
        }
        let mut types = Vec::new();
        match pat {
            Pattern::Tuple { pats, .. } => {
                for p in pats {
                    if matches!(p, Pattern::Tuple { .. }) {
                        types.extend(self.destructure_binding_types(p));
                    } else {
                        leaf(&mut types, self, p);
                    }
                }
            }
            _ => leaf(&mut types, self, pat),
        }
        types
    }

    /// Bind a destructuring pattern's leaves into module globals.
    /// `src` names a `zz_value` temp holding the tuple value; element
    /// `i` is `zz_index_get(src, zz_int(i))` (tuples are runtime arrays).
    /// Wildcards and match-only patterns bind nothing (mirroring the VM,
    /// which pops them). A binding with no tracked global falls back to
    /// a scope-local bind (defensive; unreachable when the checker
    /// recorded every top-level name).
    fn emit_destructure_global_pat(
        &self,
        pat: &zz_frontend::ast::Pattern,
        src: &str,
        names: &mut NameCtx,
        out: &mut String,
        global_init_done: &mut std::collections::HashSet<String>,
    ) {
        match pat {
            Pattern::Tuple { pats, .. } => {
                for (i, p) in pats.iter().enumerate() {
                    match p {
                        Pattern::Wildcard { .. } => {}
                        Pattern::Tuple { .. } => {
                            let e = names.fresh("__de");
                            let err = names.fresh("_dee");
                            out.push_str(&format!("    int {err} = 0;\n"));
                            out.push_str(&format!(
                                "    zz_value {e} = zz_index_get({src}, zz_int({i}), &{err});\n"
                            ));
                            self.emit_destructure_global_pat(p, &e, names, out, global_init_done);
                        }
                        Pattern::Binding { name } => {
                            let e = names.fresh("__de");
                            let err = names.fresh("_dee");
                            out.push_str(&format!("    int {err} = 0;\n"));
                            out.push_str(&format!(
                                "    zz_value {e} = zz_index_get({src}, zz_int({i}), &{err});\n"
                            ));
                            if let Some((gid, gtype)) = names
                                .globals
                                .get(&name.name)
                                .map(|(a, b)| (a.clone(), b.clone()))
                            {
                                // Mirror top-level Decl assignment.
                                let final_val = match gtype.as_str() {
                                    "int64_t" => format!("({e}).i"),
                                    "double" => format!("({e}).f"),
                                    "bool" => format!("({e}).b"),
                                    _ => e.clone(),
                                };
                                let first_init = !global_init_done.contains(&name.name);
                                if first_init {
                                    out.push_str(&format!("    {gid} = {final_val};\n"));
                                    global_init_done.insert(name.name.clone());
                                } else if matches!(gtype.as_str(), "int64_t" | "double" | "bool") {
                                    out.push_str(&format!("    {gid} = {final_val};\n"));
                                } else {
                                    out.push_str(&format!("    zz_assign(&{gid}, {final_val});\n"));
                                }
                            } else {
                                self.emit_pattern_bind(p, &e, names, out);
                            }
                        }
                        // Literal/variant/or cannot appear here from the
                        // parser (only flat bindings/wildcards); skip
                        // without binding, matching the VM's Pop behavior.
                        _ => {}
                    }
                }
            }
            // Non-tuple root (unreachable from the parser): bind directly.
            Pattern::Binding { name } => {
                if let Some((gid, gtype)) = names
                    .globals
                    .get(&name.name)
                    .map(|(a, b)| (a.clone(), b.clone()))
                {
                    let final_val = match gtype.as_str() {
                        "int64_t" => format!("({src}).i"),
                        "double" => format!("({src}).f"),
                        "bool" => format!("({src}).b"),
                        _ => src.to_string(),
                    };
                    out.push_str(&format!("    {gid} = {final_val};\n"));
                    global_init_done.insert(name.name.clone());
                }
            }
            _ => {}
        }
    }
}

/// Whether an AST expression was (or will be) lowered to a raw scalar
/// C expression (int64_t / double / bool) rather than a wrapped zz_value.
/// Used by Stmt::Assign to decide whether to emit a plain `cid = val;`
/// or a `.i`/`.f`/`.b` field extraction on a boxed value.
/// Returns true if `emit_expr` produces a raw C scalar (int64_t/double/bool)
/// rather than a zz_value.  Used by assignment to decide whether to extract
/// `.i`/`.f`/`.b` or assign directly.
fn expr_emits_raw_scalar(e: &Expr) -> bool {
    match e {
        // Literals always emit zz_int()/zz_float()/zz_bool() → zz_value.
        Expr::Int { .. } | Expr::Float { .. } | Expr::Bool { .. } => false,
        Expr::Ident { .. } | Expr::Path { .. } => true,
        Expr::Paren { expr, .. } => expr_emits_raw_scalar(expr),
        Expr::Unary { expr, .. } => expr_emits_raw_scalar(expr),
        Expr::Binary {
            left, right, op, ..
        } => {
            // Pure scalar binary if both sides are scalar AND it's an
            // arithmetic op (comparisons produce a boxed bool).
            matches!(
                op,
                zz_frontend::ast::BinOp::Add
                    | zz_frontend::ast::BinOp::Sub
                    | zz_frontend::ast::BinOp::Mul
                    | zz_frontend::ast::BinOp::Div
                    | zz_frontend::ast::BinOp::Rem
            ) && expr_emits_raw_scalar(left)
                && expr_emits_raw_scalar(right)
        }
        _ => false,
    }
}

/// Convert a zz_type to a C type string for variable declarations.
fn ty_to_ctype(ty: &zz_hir::Type) -> String {
    match ty {
        zz_hir::Type::Int => "int64_t".to_string(),
        zz_hir::Type::Float => "double".to_string(),
        zz_hir::Type::Bool => "bool".to_string(),
        zz_hir::Type::Unit => "zz_value".to_string(), // Unit is represented as zz_value
        zz_hir::Type::Str => "zz_value".to_string(),  // Strings are heap-allocated
        zz_hir::Type::Tuple(_) => "zz_value".to_string(), // Tuples are heap-allocated
        zz_hir::Type::Option(_) => "zz_value".to_string(), // Options are heap-allocated
        zz_hir::Type::Result(_, _) => "zz_value".to_string(), // Results are heap-allocated
        zz_hir::Type::Func(_, _) => "zz_value".to_string(), // Functions are heap-allocated
        zz_hir::Type::Array(_) => "zz_value".to_string(), // Arrays are heap-allocated
        zz_hir::Type::Dict(_, _) => "zz_value".to_string(), // Dicts are heap-allocated
        zz_hir::Type::Union(_) => "zz_value".to_string(), // Unions are heap-allocated
        zz_hir::Type::Json => "zz_value".to_string(), // JSON is heap-allocated
        zz_hir::Type::HttpServer => "zz_value".to_string(), // HTTP server is heap-allocated
        zz_hir::Type::TcpStream => "zz_value".to_string(), // TcpStream is heap-allocated
        zz_hir::Type::TcpListener => "zz_value".to_string(), // TcpListener is heap-allocated
        zz_hir::Type::Response => "zz_value".to_string(), // Response is heap-allocated
        zz_hir::Type::HttpRequest => "zz_value".to_string(), // Request is heap-allocated
        _ => "zz_value".to_string(),                  // All other types are heap-allocated
    }
}

pub use zz_codegen_ffi::native_supported;

/// Required value-count for a stdlib native (params minus trailing
/// defaults), from the checker's lockstep signature table. Used to
/// disambiguate untyped-receiver method dispatch: the call passed
/// checking with N values, so among same-named natives in different
/// namespaces the arity-compatible one is the checker's choice
/// (e.g. 2-value `ph.find(x)` is `regexp.find`, not 3-value
/// `str.find`). `None` when the name isn't a known native.
pub(crate) fn native_arity(name: &str) -> Option<usize> {
    let funcs = zz_stdlib::funcs::stdlib_funcs_cached();
    let sig = funcs
        .get(name)
        .or_else(|| funcs.get(&format!("std.{name}")))?;
    let mut required = sig.params.len();
    for defaulted in sig.has_default.iter().rev() {
        if *defaulted {
            required -= 1;
        } else {
            break;
        }
    }
    Some(required)
}

/// Choose a method namespace from an ordered candidate list, preferring
/// the candidate whose required arity matches the call's value count
/// (receiver + args). Falls back to the first supported candidate, so
/// behavior is unchanged when nothing matches by arity. Only ever
/// *changes* selection away from a candidate that could not have
/// compiled (C natives take a fixed value count).
pub(crate) fn pick_method_ns<'a, I>(namespaces: I, method: &str, nvalues: usize) -> Option<String>
where
    I: IntoIterator<Item = &'a str>,
{
    let mut fallback: Option<String> = None;
    for ns in namespaces {
        let candidate = format!("{ns}.{method}");
        if !native_supported(&candidate) {
            continue;
        }
        if fallback.is_none() {
            fallback = Some(ns.to_string());
        }
        if native_arity(&candidate) == Some(nvalues) {
            return Some(ns.to_string());
        }
    }
    fallback
}

/// Prefer the arity-matching candidate; else the first with unknown
/// arity (can't judge — old behavior); else `None`: every candidate is
/// provably wrong, so leave resolution to a later stage (another
/// fallback site, or a loud lowering error) instead of emitting a
/// guaranteed C type error.
pub(crate) fn pick_arity<'a, I>(namespaces: I, method: &str, nvalues: usize) -> Option<&'a str>
where
    I: IntoIterator<Item = &'a str>,
{
    let mut unknown: Option<&'a str> = None;
    for ns in namespaces {
        match native_arity(&format!("{ns}.{method}")) {
            Some(a) if a == nvalues => return Some(ns),
            None if unknown.is_none() => unknown = Some(ns),
            _ => {}
        }
    }
    unknown
}

#[cfg(test)]
mod link_need_tests {
    use super::*;

    fn lowered(needs: [bool; 5]) -> LoweredC {
        LoweredC {
            source: String::new(),
            needs_native_rt: needs[0],
            needs_pg_link: needs[1],
            needs_float_fmt: needs[2],
            needs_curl: needs[3],
            needs_sqlite: needs[4],
        }
    }

    #[test]
    fn every_gate_maps_to_its_option() {
        let mut opts = zz_codegen_types::BuildOptions::default();
        lowered([true; 5]).apply_link_needs(&mut opts);
        assert!(opts.native_rt);
        assert!(opts.pg_link);
        assert!(opts.float_link);
        assert!(opts.curl_link);
        assert!(opts.sqlite_link);
    }

    #[test]
    fn explicit_flags_survive_a_quiet_program() {
        let mut opts = zz_codegen_types::BuildOptions {
            native_rt: true,
            curl_link: true,
            ..Default::default()
        };
        lowered([false; 5]).apply_link_needs(&mut opts);
        assert!(opts.native_rt, "user-requested link must not be cleared");
        assert!(opts.curl_link, "user-requested link must not be cleared");
        assert!(!opts.pg_link);
        assert!(!opts.float_link);
        assert!(!opts.sqlite_link);
    }

    #[test]
    fn units_gates_match_single_tu_gates() {
        let units = units::LoweredUnits {
            header: String::new(),
            units: Vec::new(),
            entry_ns: "main".to_string(),
            needs_native_rt: true,
            needs_pg_link: true,
            needs_float_fmt: true,
            needs_curl: true,
            needs_sqlite: true,
        };
        let mut opts = zz_codegen_types::BuildOptions::default();
        units.apply_link_needs(&mut opts);
        let mut single = zz_codegen_types::BuildOptions::default();
        lowered([true; 5]).apply_link_needs(&mut single);
        assert_eq!(
            (
                opts.native_rt,
                opts.pg_link,
                opts.float_link,
                opts.curl_link,
                opts.sqlite_link
            ),
            (
                single.native_rt,
                single.pg_link,
                single.float_link,
                single.curl_link,
                single.sqlite_link
            ),
        );
    }
}
