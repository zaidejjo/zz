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
mod fn_decl;
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
    pub fn lower(&self) -> LoweredC {
        let mut funcs = String::new();
        let mut body = String::new();
        // One NameCtx shared across ALL top-level statements: top-level vars
        // remain visible across statements (like zz_main's single frame).
        let mut names = NameCtx::new();

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
        // pointer as the first arg (the `self` receiver), so the prototype
        // shape depends on the first param's type. Detect by inspecting
        // `tp.funcs` for the first param being a `Type::Struct`.
        let mut forward_decls = String::new();
        let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
        for fname in &self.reachable_funcs {
            if !seen.insert(fname.clone()) {
                continue;
            }
            let first_struct_c = self
                .tp
                .funcs
                .get(fname)
                .and_then(|sig| sig.params.first().map(|(_, t)| t.clone()))
                .filter(|t| matches!(t, zz_checker::Type::Struct(_)))
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
        let source = format!(
            "{runtime_h}\n{runtime_c}\n\n// ---- struct definitions ----\n{struct_preamble}\n// ---- forward declarations ----\n{forward_decls}{closure_fwd}\n// ---- generated code ----\n{funcs}\n// ---- closures ----\n{closure_defs}\nvoid zz_main(void) {{\n    zz_arena _arena;\n    zz_arena_init(&_arena, 65536);\n{body}    zz_arena_reset_trim(&_arena);\n}}\n\nint zz_call_main(void) {{\n    {main_decl}\n    return 0;\n}}\n",
            runtime_h = crate::RUNTIME_H,
            runtime_c = crate::RUNTIME_C,
            struct_preamble = struct_preamble,
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

        LoweredC { source }
    }
}

/// Remove every `#include "..."` directive from the assembled C source.
///
/// The AOT backend concatenates the modular runtime headers and sources
/// into one translation unit, so quoted includes (which reference files
/// that do not exist at compile time) must be dropped. System includes
/// (`<...>`) are preserved.
fn strip_quoted_includes(src: &str) -> String {
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
        // Bare builtins (no namespace) registered by stdlib at top level.
        "println" | "io.println" | "std.io.println" => Some("zz_io_println"),
        "print" | "io.print" | "std.io.print" => Some("zz_io_print"),
        "printz" | "io.printz" | "std.io.printz" => Some("zz_io_print"),
        "input" | "io.read_line" | "std.io.read_line" | "main_io.input" => Some("zz_io_input"),
        "len" => Some("zz_len"),
        "map" => Some("zz_iter_map"),
        "filter" => Some("zz_iter_filter"),
        "enumerate" => Some("zz_iter_enumerate"),
        "zip" => Some("zz_iter_zip"),
        "append" => Some("zz_vec_push"),
        "range" => Some("zz_range3"),
        "typeof" => Some("zz_typeof"),
        "int" => Some("zz_int_cast"),
        "float" => Some("zz_float_cast"),
        "bool" => Some("zz_bool_cast"),
        "str" => Some("zz_str_cast"),
        // vec methods — bare names for method dispatch
        "vec.len" | "std.vec.len" | "vec_len" => Some("zz_vec_len"),
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
        "env.args" | "std.env.args" | "envmod.args" | "std.envmod.args" => Some("zz_env_args"),
        // dict
        "dict.len" => Some("zz_dict_len_val"),
        "dict.keys" => Some("zz_dict_keys"),
        "dict.has" => Some("zz_dict_has"),
        // option / result
        "option.expect" | "std.option.expect" => Some("zz_option_expect"),
        "result.expect" | "std.result.expect" => Some("zz_result_expect"),
        // fs
        "fs.read" | "std.fs.read" => Some("zz_fs_read"),
        "fs.read_file" | "std.fs.read_file" => Some("zz_fs_read"),
        "fs.read_to_string" | "std.fs.read_to_string" => Some("zz_fs_read"),
        "fs.write" | "std.fs.write" => Some("zz_fs_write"),
        "fs.write_file" | "std.fs.write_file" => Some("zz_fs_write"),
        "fs.exists" | "std.fs.exists" => Some("zz_fs_exists"),
        "fs.remove" | "std.fs.remove" | "fs.remove_file" | "std.fs.remove_file" => {
            Some("zz_fs_remove")
        }
        "fs.mkdir" | "std.fs.mkdir" => Some("zz_fs_mkdir"),
        "fs.readdir" | "std.fs.readdir" => Some("zz_fs_readdir"),
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
        // channels
        "chan" | "std.chan" => Some("zz_chan_new"),
        "chan.send" | "std.chan.send" => Some("zz_chan_send"),
        "chan.recv" | "std.chan.recv" => Some("zz_chan_recv"),
        "chan.try_recv" | "std.chan.try_recv" => Some("zz_chan_try_recv"),
        // spawn / task join
        "spawn" | "std.spawn" | "std.task.spawn" => Some("zz_spawn"),
        "task.recv" | "std.task.recv" | "task.join" | "std.task.join" => Some("zz_task_join_recv"),
        // http (AOT: minimal thread-per-connection server returning OK)
        "http.server" | "std.http.server" => Some("zz_http_server"),
        "http.route_get" | "std.http.route_get" => Some("zz_http_route_get"),
        "http.route_post" | "std.http.route_post" => Some("zz_http_route_get"),
        "http.route_put" | "std.http.route_put" => Some("zz_http_route_get"),
        "http.route_delete" | "std.http.route_delete" => Some("zz_http_route_get"),
        "http.log" | "std.http.log" => Some("zz_http_log"),
        "http.pipe" | "std.http.pipe" => Some("zz_http_log"),
        "http.listen" | "std.http.listen" => Some("zz_http_listen"),
        "http.handle" | "std.http.handle" => Some("zz_http_handle"),
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

/// Whether a reachable native has a C runtime implementation.
pub fn native_supported(name: &str) -> bool {
    native_impl(name).is_some()
}
