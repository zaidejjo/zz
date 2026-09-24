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

mod context;
mod expr;
mod extern_call;
mod fn_decl;
mod green;
mod stmt;

use zz_frontend::ast::{Block, Expr, Stmt};

pub use context::{Lowerer, NameCtx};

// Internal helpers shared across the lowering submodules.
pub(crate) use context::{
    auto_box, box_scalar_operand, emit_guard_expr, is_dup_safe, scalar_operand_c,
    scalar_operand_type,
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
}

/// Mangle a zz qualified name to a C identifier.
pub fn mangle(name: &str) -> String {
    name.replace('.', "__")
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect()
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
    pub(super) fn is_impl_method(&self, fname: &str) -> bool {
        let Some(sig) = self.tp.funcs.get(fname) else {
            return false;
        };
        let Some((_, first)) = sig.params.first() else {
            return false;
        };
        let zz_checker::Type::Struct(sname) = first else {
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
        let mut global_init_done: std::collections::HashSet<String> =
            std::collections::HashSet::new();

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
                Stmt::Struct { .. } | Stmt::Import { .. } => {}
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
                            let val = self.emit_expr(value, &mut names, &mut out);
                            out.push_str(&format!("    {gid} = {val};\n"));
                            body.push_str(&out);
                            global_init_done.insert(zz_name.clone());
                            continue;
                        }
                    }
                    let val = self.emit_expr(value, &mut names, &mut out);
                    let val_is_unboxed = val.starts_with("(int64_t)(")
                        || val.starts_with("(double)(")
                        || val.starts_with("(bool)(");
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
                other => {
                    let mut out = String::new();
                    self.emit_stmt(other, &mut names, &mut out, false);
                    body.push_str(&out);
                }
            }
        }

        let main_decl = if self.reachable_funcs.contains(&self.entry_main) {
            // main exists: call its stub from zz_call_main.
            "zz_call_into_main();".to_string()
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
                .filter(|t| matches!(t, zz_checker::Type::Struct(_)))
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
        let ffi_pre = crate::ffi::ffi_prelude(&self.reachable_natives);
        let ffi_section = if ffi_pre.is_empty() {
            String::new()
        } else {
            format!("\n// ---- native-runtime FFI ----\n{ffi_pre}\n")
        };
        let needs_native_rt = crate::ffi::needs_native_rt(&self.reachable_natives);
        let runtime_c = if self.precompiled {
            ""
        } else {
            crate::RUNTIME_C
        };
        let source = format!(
            "{runtime_h}\n{runtime_c}\n{ffi_section}\n{extern_section}// ---- struct definitions ----\n{struct_preamble}\n{struct_debug_fns}\n// ---- module globals ----\n{globals_decl}\n// ---- forward declarations ----\n{forward_decls}{closure_fwd}\n// ---- generated code ----\n{funcs}\n// ---- closures ----\n{closure_defs}\nvoid zz_main(void) {{\n    zz_arena _arena;\n    zz_arena_init(&_arena, 65536);\n{body}    zz_arena_reset_trim(&_arena);\n}}\n\nint zz_call_main(void) {{\n    {main_decl}\n    return 0;\n}}\n",
            runtime_h = crate::RUNTIME_H,
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
        let source = strip_quoted_includes(&source);

        // If main is reachable, append a stub that calls it (runtime does
        // not know the symbol; we emit a forward decl + call here).
        let with_main = if self.reachable_funcs.contains(&self.entry_main) {
            let m = format!("zz_fn_{}", mangle(&self.entry_main));
            format!(
                "\nstatic void zz_call_into_main(void);\nstatic void zz_call_into_main(void) {{ zz_value _r = {m}(NULL, 0); (void)_r; }}\n"
            )
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
            needs_pg_link: crate::ffi::needs_pg_link(&self.reachable_natives),
        }
    }
}

/// Remove every `#include "..."` directive from the assembled C source.
///
/// The AOT backend concatenates the modular runtime headers and sources
/// into one translation unit, so quoted includes (which reference files
/// that do not exist at compile time) must be dropped. System includes
/// (`<...>`) are preserved.
pub(crate) fn strip_quoted_includes(src: &str) -> String {
    src.lines()
        .filter(|line| {
            let t = line.trim_start();
            !(t.starts_with("#include \"") && t.ends_with('"'))
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Get a Block from an else-expression (or empty block).
fn get_block(e: &Expr) -> &Block {
    match e {
        Expr::Block(b) => b,
        _ => {
            // Wrap expression in an implicit block reference is not possible;
            // return a static empty block via leak — MVPs accept unit.
            static EMPTY: Block = Block {
                stmts: Vec::new(),
                span: zz_frontend::span::Span { start: 0, end: 0 },
            };
            &EMPTY
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
        _ => "zz_value".to_string(),                  // All other types are heap-allocated
    }
}

/// Map a zz native qualified name to its C runtime implementation name.
fn native_impl(name: &str) -> Option<&'static str> {
    match name {
        // Builtin console I/O (no import, no `std.io` module).
        "println" => Some("zz_io_println"),
        "print" => Some("zz_io_print"),
        "input" => Some("zz_io_input"),
        "len" => Some("zz_len"),
        "map" | "vec.map" | "std.vec.map" => Some("zz_iter_map"),
        "filter" | "vec.filter" | "std.vec.filter" => Some("zz_iter_filter"),
        "enumerate" | "vec.enumerate" | "std.vec.enumerate" => Some("zz_iter_enumerate"),
        "zip" | "vec.zip" | "std.vec.zip" => Some("zz_iter_zip"),
        "append" => Some("zz_vec_push"),
        "range" => Some("zz_range3"),
        "typeof" => Some("zz_typeof"),
        "int" => Some("zz_int_cast"),
        "float" => Some("zz_float_cast"),
        "bool" => Some("zz_bool_cast"),
        "str" => Some("zz_str_cast"),
        // vec methods — bare names for method dispatch
        "vec.len" | "std.vec.len" | "vec_len" => Some("zz_vec_len"),
        "bytes.len" | "std.bytes.len" => Some("zz_len"),
        "vec.append" | "std.vec.append" => Some("zz_vec_append"),
        "vec.push" | "std.vec.push" => Some("zz_vec_push"),
        "vec.pop" | "std.vec.pop" => Some("zz_vec_pop"),
        "vec.remove" | "std.vec.remove" => Some("zz_vec_remove"),
        "vec.insert" | "std.vec.insert" => Some("zz_vec_insert"),
        "vec.join" | "std.vec.join" => Some("zz_str_join"), // join is a str function called on arrays
        "vec.contains" | "std.vec.contains" => Some("zz_vec_contains"),
        "vec.sort" | "std.vec.sort" => Some("zz_vec_sort"),
        "vec.reverse" | "std.vec.reverse" => Some("zz_vec_reverse"),
        // str
        "str.length" | "std.str.length" => Some("zz_str_length"),
        "str.to_lower" | "std.str.to_lower" | "str.lower" | "std.str.lower" => Some("zz_str_lower"),
        "str.to_upper" | "std.str.to_upper" | "str.upper" | "std.str.upper" => Some("zz_str_upper"),
        "str.replace" | "std.str.replace" => Some("zz_str_replace"),
        "str.contains" | "std.str.contains" => Some("zz_str_contains"),
        "str.starts_with" | "std.str.starts_with" | "str.startswith" | "std.str.startswith" => {
            Some("zz_str_startswith")
        }
        "str.ends_with" | "std.str.ends_with" | "str.endswith" | "std.str.endswith" => {
            Some("zz_str_endswith")
        }
        "str.trim" | "std.str.trim" => Some("zz_str_trim"),
        "str.trim_start" | "std.str.trim_start" => Some("zz_str_trim_start"),
        "str.trim_end" | "std.str.trim_end" => Some("zz_str_trim_end"),
        "str.join" | "std.str.join" => Some("zz_str_join"),
        "str.split" | "std.str.split" => Some("zz_str_split"),
        // math
        "math.abs" | "std.math.abs" => Some("zz_math_abs"),
        "math.sqrt" | "std.math.sqrt" => Some("zz_math_sqrt"),
        "math.pow" | "std.math.pow" => Some("zz_math_pow"),
        "math.floor" | "std.math.floor" => Some("zz_math_floor"),
        "math.ceil" | "std.math.ceil" => Some("zz_math_ceil"),
        "math.round" | "std.math.round" => Some("zz_math_round"),
        "math.trunc" | "std.math.trunc" => Some("zz_math_trunc"),
        "math.signum" | "std.math.signum" => Some("zz_math_signum"),
        "math.hypot" | "std.math.hypot" => Some("zz_math_hypot"),
        "math.clamp" | "std.math.clamp" => Some("zz_math_clamp"),
        "math.root" | "std.math.root" => Some("zz_math_root"),
        "math.factorial" | "std.math.factorial" => Some("zz_math_factorial"),
        "math.gcd" | "std.math.gcd" => Some("zz_math_gcd"),
        "math.lcm" | "std.math.lcm" => Some("zz_math_lcm"),
        "math.sin" | "std.math.sin" => Some("zz_math_sin"),
        "math.cos" | "std.math.cos" => Some("zz_math_cos"),
        "math.tan" | "std.math.tan" => Some("zz_math_tan"),
        "math.asin" | "std.math.asin" => Some("zz_math_asin"),
        "math.acos" | "std.math.acos" => Some("zz_math_acos"),
        "math.atan" | "std.math.atan" => Some("zz_math_atan"),
        "math.sin_deg" | "std.math.sin_deg" => Some("zz_math_sin_deg"),
        "math.cos_deg" | "std.math.cos_deg" => Some("zz_math_cos_deg"),
        "math.tan_deg" | "std.math.tan_deg" => Some("zz_math_tan_deg"),
        "math.to_radians" | "std.math.to_radians" => Some("zz_math_to_radians"),
        "math.to_degrees" | "std.math.to_degrees" => Some("zz_math_to_degrees"),
        "math.log" | "std.math.log" => Some("zz_math_log"),
        "math.log10" | "std.math.log10" => Some("zz_math_log10"),
        "math.exp" | "std.math.exp" => Some("zz_math_exp"),
        "math.random" | "std.math.random" => Some("zz_math_random"),
        "math.is_nan" | "std.math.is_nan" => Some("zz_math_is_nan"),
        "math.is_inf" | "std.math.is_inf" => Some("zz_math_is_inf"),
        "math.PI" | "std.math.PI" => Some("zz_math_pi"),
        "math.E" | "std.math.E" => Some("zz_math_e"),
        "math.TAU" | "std.math.TAU" => Some("zz_math_tau"),
        "math.INF" | "std.math.INF" => Some("zz_math_inf"),
        "math.NAN" | "std.math.NAN" => Some("zz_math_nan"),
        "math.isqrt" | "std.math.isqrt" => Some("zz_math_isqrt"),
        "math.mean" | "std.math.mean" => Some("zz_math_mean"),
        "math.median" | "std.math.median" => Some("zz_math_median"),
        "math.rand_range" | "std.math.rand_range" => Some("zz_math_rand_range"),
        "math.dot_product" | "std.math.dot_product" => Some("zz_math_dot_product"),
        "math.magnitude" | "std.math.magnitude" => Some("zz_math_magnitude"),
        // json
        "json.parse" | "std.json.parse" => Some("zz_json_parse"),
        "json.stringify" | "std.json.stringify" => Some("zz_json_stringify"),
        "json.null" | "std.json.null" => Some("zz_json_null"),
        "json.get" | "std.json.get" => Some("zz_json_get"),
        "json.as_str" | "std.json.as_str" => Some("zz_json_as_str"),
        "json.as_int" | "std.json.as_int" => Some("zz_json_as_int"),
        "json.as_float" | "std.json.as_float" => Some("zz_json_as_float"),
        "json.as_bool" | "std.json.as_bool" => Some("zz_json_as_bool"),
        "json.type" | "std.json.type" => Some("zz_json_type"),
        "json.len" | "std.json.len" => Some("zz_json_len"),
        "json.keys" | "std.json.keys" => Some("zz_json_keys"),
        "json.has" | "std.json.has" => Some("zz_json_has"),
        "json.pretty" | "std.json.pretty" => Some("zz_json_pretty"),
        "json.merge" | "std.json.merge" => Some("zz_json_merge"),
        "json.deep_get" | "std.json.deep_get" => Some("zz_json_deep_get"),
        "json.array_push" | "std.json.array_push" => Some("zz_json_array_push"),
        // env
        "env.get" | "std.env.get" | "envmod.get" | "std.envmod.get" => Some("zz_env_get"),
        "env.get_var" | "std.env.get_var" | "envmod.get_var" | "std.envmod.get_var" => {
            Some("zz_env_get")
        }
        "env.var" | "std.env.var" | "envmod.var" | "std.envmod.var" => Some("zz_env_var"),
        "env.set" | "std.env.set" => Some("zz_env_set"),
        "env.remove" | "std.env.remove" | "env.unset" | "std.env.unset" => Some("zz_env_remove"),
        "env.vars" | "std.env.vars" => Some("zz_env_vars"),
        "env.cwd" | "std.env.cwd" => Some("zz_env_cwd"),
        "env.set_cwd" | "std.env.set_cwd" => Some("zz_env_set_cwd"),
        "env.exe_path" | "std.env.exe_path" => Some("zz_env_exe_path"),
        "env.home_dir" | "std.env.home_dir" => Some("zz_env_home_dir"),
        "env.temp_dir" | "std.env.temp_dir" => Some("zz_env_temp_dir"),
        "env.user" | "std.env.user" => Some("zz_env_user"),
        "env.os" | "std.env.os" => Some("zz_env_os"),
        "env.arch" | "std.env.arch" => Some("zz_env_arch"),
        "env.args" | "std.env.args" | "envmod.args" | "std.envmod.args" => Some("zz_env_args"),
        // dict
        "dict.len" => Some("zz_dict_len_val"),
        "dict.keys" => Some("zz_dict_keys"),
        "dict.has" => Some("zz_dict_has"),
        // option / result
        "option.expect" | "std.option.expect" => Some("zz_option_expect"),
        "result.expect" | "std.result.expect" => Some("zz_result_expect"),
        // fs — comprehensive non-blocking filesystem (C runtime in
        // core.c; unified `fs:<op>:<code>` diagnostics match the VM).
        "fs.read" | "std.fs.read" => Some("zz_fs_read"),
        "fs.read_file" | "std.fs.read_file" => Some("zz_fs_read"),
        "fs.read_to_string" | "std.fs.read_to_string" => Some("zz_fs_read"),
        "fs.read_bytes" | "std.fs.read_bytes" => Some("zz_fs_read_bytes"),
        "fs.write" | "std.fs.write" => Some("zz_fs_write"),
        "fs.write_file" | "std.fs.write_file" => Some("zz_fs_write"),
        "fs.append" | "std.fs.append" => Some("zz_fs_append"),
        "fs.copy" | "std.fs.copy" => Some("zz_fs_copy"),
        "fs.move" | "std.fs.move" => Some("zz_fs_move"),
        "fs.rename" | "std.fs.rename" => Some("zz_fs_move"),
        "fs.exists" | "std.fs.exists" => Some("zz_fs_exists"),
        "fs.is_file" | "std.fs.is_file" => Some("zz_fs_is_file"),
        "fs.is_dir" | "std.fs.is_dir" => Some("zz_fs_is_dir"),
        "fs.remove" | "std.fs.remove" | "fs.remove_file" | "std.fs.remove_file" => {
            Some("zz_fs_remove")
        }
        "fs.mkdir" | "std.fs.mkdir" => Some("zz_fs_mkdir"),
        "fs.mkdir_all" | "std.fs.mkdir_all" => Some("zz_fs_mkdir_all"),
        "fs.readdir" | "std.fs.readdir" => Some("zz_fs_readdir"),
        "fs.read_dir" | "std.fs.read_dir" => Some("zz_fs_read_dir"),
        "fs.remove_dir_all" | "std.fs.remove_dir_all" => Some("zz_fs_remove_dir_all"),
        "fs.walk_dir" | "std.fs.walk_dir" => Some("zz_fs_walk_dir"),
        "fs.stat" | "std.fs.stat" => Some("zz_fs_stat"),
        "fs.open" | "std.fs.open" | "File.open" => Some("zz_fs_open"),
        "fs.read_chunk" | "std.fs.read_chunk" | "file.read_chunk" => Some("zz_fs_read_chunk"),
        "fs.read_chunk_bytes" | "std.fs.read_chunk_bytes" | "file.read_chunk_bytes" => {
            Some("zz_fs_read_chunk_bytes")
        }
        "fs.write_chunk" | "std.fs.write_chunk" | "file.write_chunk" => Some("zz_fs_write_chunk"),
        "fs.seek" | "std.fs.seek" | "file.seek" => Some("zz_fs_seek"),
        "fs.flush" | "std.fs.flush" | "file.flush" => Some("zz_fs_flush"),
        "fs.close" | "std.fs.close" | "file.close" => Some("zz_fs_close"),
        "fs.normalize" | "std.fs.normalize" => Some("zz_fs_normalize"),
        "fs.join" | "std.fs.join" => Some("zz_fs_join"),
        "fs.basename" | "std.fs.basename" => Some("zz_fs_basename"),
        "fs.dirname" | "std.fs.dirname" => Some("zz_fs_dirname"),
        "fs.is_absolute" | "std.fs.is_absolute" => Some("zz_fs_is_absolute"),
        "fs.extension" | "std.fs.extension" => Some("zz_fs_extension"),
        "fs.osfs" | "std.fs.osfs" => Some("zz_fs_osfs"),
        "fs.memfs" | "std.fs.memfs" => Some("zz_fs_memfs"),
        "fs.tarfs" | "std.fs.tarfs" => Some("zz_fs_tarfs"),
        "fs.embedfs" | "std.fs.embedfs" => Some("zz_fs_embedfs"),
        "fs.read_to_string_at" | "std.fs.read_to_string_at" => Some("zz_fs_read_to_string_at"),
        "fs.read_bytes_at" | "std.fs.read_bytes_at" => Some("zz_fs_read_bytes_at"),
        "fs.write_at" | "std.fs.write_at" => Some("zz_fs_write_at"),
        "fs.append_at" | "std.fs.append_at" => Some("zz_fs_append_at"),
        "fs.exists_at" | "std.fs.exists_at" => Some("zz_fs_exists_at"),
        "fs.is_file_at" | "std.fs.is_file_at" => Some("zz_fs_is_file_at"),
        "fs.is_dir_at" | "std.fs.is_dir_at" => Some("zz_fs_is_dir_at"),
        "fs.read_dir_at" | "std.fs.read_dir_at" => Some("zz_fs_read_dir_at"),
        "fs.mkdir_all_at" | "std.fs.mkdir_all_at" => Some("zz_fs_mkdir_all_at"),
        "fs.remove_file_at" | "std.fs.remove_file_at" => Some("zz_fs_remove_file_at"),
        // encoding
        "encoding.url_encode" | "std.encoding.url_encode" => Some("zz_encoding_url_encode"),
        "encoding.url_decode" | "std.encoding.url_decode" => Some("zz_encoding_url_decode"),
        "encoding.base64_encode" | "std.encoding.base64_encode" => {
            Some("zz_encoding_base64_encode")
        }
        "encoding.base64_decode" | "std.encoding.base64_decode" => {
            Some("zz_encoding_base64_decode")
        }
        "encoding.hex_encode" | "std.encoding.hex_encode" => Some("zz_encoding_hex_encode"),
        "encoding.hex_decode" | "std.encoding.hex_decode" => Some("zz_encoding_hex_decode"),
        // time
        "time.now_ms" | "std.time.now_ms" => Some("zz_time_now_ms"),
        "time.sleep_ms" | "std.time.sleep_ms" => Some("zz_time_sleep_ms"),
        // net tcp
        "net.tcp_connect" | "std.net.tcp_connect" => Some("zz_tcp_connect"),
        "net.tcp_listen" | "std.net.tcp_listen" => Some("zz_tcp_listen"),
        "net.tcp_accept" | "std.net.tcp_accept" => Some("zz_tcp_accept"),
        "net.tcp_write" | "std.net.tcp_write" => Some("zz_tcp_write"),
        "net.tcp_read" | "std.net.tcp_read" => Some("zz_tcp_read"),
        "net.tcp_readline" | "std.net.tcp_readline" => Some("zz_tcp_readline"),
        "net.tcp_close" | "std.net.tcp_close" => Some("zz_tcp_close"),
        "net.peer_addr" | "std.net.peer_addr" => Some("zz_tcp_peer_addr"),
        "net.local_addr" | "std.net.local_addr" => Some("zz_tcp_local_addr"),
        "net.set_read_timeout" | "std.net.set_read_timeout" => Some("zz_tcp_set_read_timeout"),
        "net.set_write_timeout" | "std.net.set_write_timeout" => Some("zz_tcp_set_write_timeout"),
        // sqlz (AOT: sqlite3 prepared-statement C impl + postgres via
        // the staticlib; the C handle records the backend).
        // Canonical `sqlz.*` / `std.sqlz.*`; `db.*` / `std.db.*` are
        // zero-overhead aliases; `pg.*` / `std.sqlz.postgres.*` are the
        // explicit PG spellings (same handle, same dispatch).
        "sqlz.open" | "std.sqlz.open" | "db.open" | "std.db.open" => Some("zz_db_open"),
        "pg.connect" | "std.sqlz.postgres.connect" => Some("zz_pg_connect"),
        "sqlz.exec" | "std.sqlz.exec" | "db.exec" | "std.db.exec" => Some("zz_db_exec"),
        "pg.exec" | "std.sqlz.postgres.exec" => Some("zz_db_exec"),
        "sqlz.query" | "std.sqlz.query" | "db.query" | "std.db.query" => Some("zz_db_query"),
        "pg.query" | "std.sqlz.postgres.query" => Some("zz_db_query"),
        "sqlz.close" | "std.sqlz.close" | "db.close" | "std.db.close" => Some("zz_db_close"),
        "pg.close" | "std.sqlz.postgres.close" => Some("zz_db_close"),
        // channels
        "chan" | "std.chan" => Some("zz_chan_new"),
        "chan.send" | "std.chan.send" => Some("zz_chan_send"),
        "chan.recv" | "std.chan.recv" => Some("zz_chan_recv"),
        "chan.try_recv" | "std.chan.try_recv" => Some("zz_chan_try_recv"),
        // spawn / task join
        "spawn" | "std.spawn" | "task.spawn" | "std.task.spawn" => Some("zz_spawn"),
        "task.recv" | "std.task.recv" | "task.join" | "std.task.join" => Some("zz_task_join_recv"),
        "task.try_join" | "std.task.try_join" => Some("zz_task_try_join"),
        // http (AOT: route table with closure handlers; http.test
        // dispatches in-process; epoll workers serve static OK until Step 2)
        "http.server" | "std.http.server" => Some("zz_http_server"),
        "http.route_get" | "std.http.route_get" => Some("zz_http_route_get"),
        "http.route_post" | "std.http.route_post" => Some("zz_http_route_post"),
        "http.route_put" | "std.http.route_put" => Some("zz_http_route_put"),
        "http.route_delete" | "std.http.route_delete" => Some("zz_http_route_delete"),
        "http.log" | "std.http.log" => Some("zz_http_log"),
        "http.pipe" | "std.http.pipe" => Some("zz_http_pipe"),
        "http.listen" | "std.http.listen" => Some("zz_http_listen"),
        "http.test" | "std.http.test" => Some("zz_http_test"),
        "http.handle" | "std.http.handle" => Some("zz_http_handle"),
        "http.respond" | "std.http.respond" => Some("zz_http_respond"),
        "http.param" | "std.http.param" => Some("zz_http_param"),
        "http.query" | "std.http.query" => Some("zz_http_query"),
        "http.header" | "std.http.header" => Some("zz_http_header"),
        "http.body_json" | "std.http.body_json" => Some("zz_http_body_json"),
        "http.body_form" | "std.http.body_form" => Some("zz_http_body_form"),
        // http Response methods
        "http.status" | "std.http.status" => Some("zz_http_response_status"),
        "http.text" | "std.http.text" => Some("zz_http_response_text"),
        "http.json" | "std.http.json" => Some("zz_http_response_json"),
        "http.headers" | "std.http.headers" => Some("zz_http_response_headers"),
        // http request functions
        "http.get" | "std.http.get" => Some("zz_http_get"),
        "http.post" | "std.http.post" => Some("zz_http_post"),
        _ => None,
    }
}

/// Whether a reachable native has a callable implementation.
///
/// Embedded-C natives resolve through [`native_impl`]; Rust-staticlib
/// natives (regex, crypto, … from Phase 1 on) through [`crate::ffi_impl`].
/// Anything else lowers to unit (documented MVP limitation).
pub fn native_supported(name: &str) -> bool {
    native_impl(name).is_some() || crate::ffi_impl(name).is_some()
}
