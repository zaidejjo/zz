//! HIR → C lowering for the native AOT backend.
//!
//! Consumes the DCE-pruned [`TypedProgram`] and [`ReachableSet`] from
//! zz_hir and produces a single self-contained C translation unit (plus the
//! embedded runtime). A `cc` invocation compiles it to a native binary.

use std::collections::HashMap;

use zz_frontend::ast::{Block, Expr, FmtPart, Param, Stmt};

/// Result of lowering.
#[derive(Debug)]
pub struct LoweredC {
    /// The full generated C source (runtime + user code + main glue).
    pub source: String,
}

/// Scope-aware C identifier allocator (handles shadowing).
#[derive(Default)]
pub struct NameCtx {
    /// zz var name → stack of active (C identifier, C type) tuples (innermost last).
    stack: HashMap<String, Vec<(String, String)>>,
    counter: usize,
}

impl NameCtx {
    fn new() -> Self {
        NameCtx {
            stack: HashMap::new(),
            counter: 0,
        }
    }

    /// Enter a new scope for `name`, returning the fresh C identifier.
    fn enter(&mut self, name: &str) -> String {
        let cid = format!("v{}", self.counter);
        self.counter += 1;
        self.stack
            .entry(name.to_string())
            .or_default()
            .push((cid.clone(), "zz_value".to_string()));
        cid
    }

    /// Enter a new scope for `name` with a specific C type.
    fn enter_with_type(&mut self, name: &str, ctype: &str) -> String {
        let cid = format!("v{}", self.counter);
        self.counter += 1;
        self.stack
            .entry(name.to_string())
            .or_default()
            .push((cid.clone(), ctype.to_string()));
        cid
    }

    /// Look up the variable's C identifier.
    fn lookup(&self, name: &str) -> Option<&str> {
        self.stack
            .get(name)
            .and_then(|vec| vec.last())
            .map(|(ident, _)| ident.as_str())
    }

    /// Look up the variable's C type.
    fn lookup_type(&self, name: &str) -> Option<&str> {
        self.stack
            .get(name)
            .and_then(|vec| vec.last())
            .map(|(_, typ)| typ.as_str())
    }

    /// Leave the current scope for `name`.
    fn leave(&mut self, name: &str) {
        if let Some(vec) = self.stack.get_mut(name) {
            vec.pop();
        }
    }
}

/// Auto-box an unboxed scalar expression to a `zz_value` C expression.
/// If the expression is already a `zz_value` (i.e., the variable's C type is
/// `zz_value` or unknown), it is returned as-is. Otherwise, the appropriate
/// boxing helper (`zz_int`, `zz_float`, `zz_bool`) is inserted.
fn auto_box(expr: &str, ctype: Option<&str>) -> String {
    match ctype {
        Some("int64_t") => format!("zz_int({expr})"),
        Some("double") => format!("zz_float({expr})"),
        Some("bool") => format!("zz_bool({expr})"),
        _ => expr.to_string(),
    }
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
    )
}

/// The lowering context.
pub struct Lowerer {
    reachable_funcs: std::collections::HashSet<String>,
    reachable_natives: std::collections::HashSet<String>,
    entry_main: String,
    tp: zz_hir::TypedProgram,
}

impl Lowerer {
    pub fn new(
        reachable_funcs: std::collections::HashSet<String>,
        reachable_natives: std::collections::HashSet<String>,
        entry_main: String,
        tp: zz_hir::TypedProgram,
    ) -> Self {
        Lowerer {
            reachable_funcs,
            reachable_natives,
            entry_main,
            tp,
        }
    }

    /// Check if a struct type is unboxed (all scalar or nested unboxed struct fields).
    fn is_unboxed_struct(&self, name: &str) -> bool {
        if let Some(sig) = self.tp.structs.get(name) {
            sig.fields
                .iter()
                .all(|(_, ty)| self.is_scalar_type(ty) || matches!(ty, zz_checker::Type::Struct(inner) if self.is_unboxed_struct(inner)))
        } else {
            false
        }
    }

    /// Check if a type is scalar (can be unboxed).
    fn is_scalar_type(&self, ty: &zz_checker::Type) -> bool {
        matches!(
            ty,
            zz_checker::Type::Int | zz_checker::Type::Float | zz_checker::Type::Bool
        )
    }

    /// Convert a checker type to C type string.
    fn type_to_c(&self, ty: &zz_checker::Type) -> String {
        match ty {
            zz_checker::Type::Int => "int64_t".to_string(),
            zz_checker::Type::Float => "double".to_string(),
            zz_checker::Type::Bool => "bool".to_string(),
            zz_checker::Type::Struct(name) => {
                if self.is_unboxed_struct(name) {
                    format!("zz_struct_{}", name)
                } else {
                    "zz_value".to_string()
                }
            }
            _ => "zz_value".to_string(),
        }
    }

    /// Get the C type name for a struct.
    fn struct_c_type(&self, name: &str) -> String {
        format!("zz_struct_{}", name)
    }

    /// Check if a type string is a struct type.
    fn is_struct_type_str(&self, type_str: &str) -> bool {
        type_str.starts_with("zz_struct_")
    }

    /// Extract the struct name from a type string like "zz_struct_Point" -> "Point".
    fn struct_name_from_c_type<'a>(&self, c_type: &'a str) -> Option<&'a str> {
        c_type.strip_prefix("zz_struct_")
    }

    /// Get the C type of a field from a struct type string.
    /// Returns the C type string (e.g., "int64_t", "zz_struct_Point") for the named field.
    fn field_type_from_struct(&self, base_c_type: &str, field_name: &str) -> Option<&str> {
        let struct_name = self.struct_name_from_c_type(base_c_type)?;
        let sig = self.tp.structs.get(struct_name)?;
        let (_, field_ty) = sig.fields.iter().find(|(n, _)| n == field_name)?;
        match field_ty {
            zz_checker::Type::Int => Some("int64_t"),
            zz_checker::Type::Float => Some("double"),
            zz_checker::Type::Bool => Some("bool"),
            zz_checker::Type::Struct(name) if self.is_unboxed_struct(name) => {
                // Return a static string — leak the Box for the 'static lifetime.
                // This is fine for codegen: small number of struct types, process exits.
                let s = format!("zz_struct_{}", name);
                Some(Box::leak(s.into_boxed_str()) as &str)
            }
            _ => None, // non-scalar boxed type: caller handles
        }
    }

    /// Auto-box a nested struct field value (e.g., r.origin.x).
    fn auto_box_nested_field(&self, parts: &[String], names: &NameCtx, emitted: &str) -> String {
        if parts.len() >= 2 {
            if let Some(base_type) = names.lookup_type(&parts[0]) {
                // Walk the chain: r (Rect) -> origin (Point) -> x (int)
                let mut current_type = base_type.to_string();
                for i in 1..parts.len() - 1 {
                    if let Some(inner_name) = self.struct_name_from_c_type(&current_type) {
                        if let Some(sig) = self.tp.structs.get(inner_name) {
                            if let Some((_, next_ty)) =
                                sig.fields.iter().find(|(n, _)| n == &parts[i])
                            {
                                current_type = self.type_to_c(next_ty);
                            }
                        }
                    }
                }
                // Now current_type is the type of the second-to-last part.
                // The last part is the leaf field.
                if let Some(leaf_name) = self.struct_name_from_c_type(&current_type) {
                    if let Some(sig) = self.tp.structs.get(leaf_name) {
                        if let Some((_, leaf_ty)) =
                            sig.fields.iter().find(|(n, _)| n == parts.last().unwrap())
                        {
                            let leaf_c = self.type_to_c(leaf_ty);
                            return auto_box(emitted, Some(leaf_c.as_str()));
                        }
                    }
                }
            }
        }
        emitted.to_string()
    }

    /// Generate C typedefs for all reachable structs, in dependency order.
    fn lower_structs_preamble(&self) -> String {
        let mut preamble = String::new();
        let mut emitted = std::collections::HashSet::new();

        // Emit structs in dependency order (topological sort).
        fn emit_struct<'a>(
            name: &'a str,
            tp: &'a zz_hir::TypedProgram,
            is_unboxed: &dyn Fn(&str) -> bool,
            emitted: &mut std::collections::HashSet<String>,
            preamble: &mut String,
        ) {
            if emitted.contains(name) {
                return;
            }
            if let Some(sig) = tp.structs.get(name) {
                // Emit dependencies first.
                for (_, field_type) in &sig.fields {
                    if let zz_checker::Type::Struct(dep_name) = field_type {
                        if is_unboxed(dep_name) {
                            emit_struct(dep_name, tp, is_unboxed, emitted, preamble);
                        }
                    }
                }
                // Emit this struct.
                preamble.push_str(&format!("typedef struct {{\n"));
                for (field_name, field_type) in &sig.fields {
                    let c_type = match field_type {
                        zz_checker::Type::Int => "int64_t".to_string(),
                        zz_checker::Type::Float => "double".to_string(),
                        zz_checker::Type::Bool => "bool".to_string(),
                        zz_checker::Type::Struct(n) if is_unboxed(n) => format!("zz_struct_{}", n),
                        _ => "zz_value".to_string(),
                    };
                    preamble.push_str(&format!("    {} {};\n", c_type, field_name));
                }
                preamble.push_str(&format!("}} zz_struct_{};\n\n", name));
                emitted.insert(name.to_string());
            }
        }

        let names: Vec<String> = self.tp.structs.keys().cloned().collect();
        for name in &names {
            if self.is_unboxed_struct(name) {
                emit_struct(
                    name,
                    &self.tp,
                    &|n| self.is_unboxed_struct(n),
                    &mut emitted,
                    &mut preamble,
                );
            }
        }

        preamble
    }

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
                Stmt::Struct { .. } | Stmt::Impl { .. } | Stmt::Import { .. } => {}
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

        let source = format!(
            "{runtime_h}\n{runtime_c}\n\n// ---- struct definitions ----\n{struct_preamble}\n// ---- generated code ----\n{funcs}\nvoid zz_main(void) {{\n{body}}}\n\nint zz_call_main(void) {{\n    {main_decl}\n    return 0;\n}}\n",
            runtime_h = crate::RUNTIME_H,
            runtime_c = crate::RUNTIME_C,
            struct_preamble = struct_preamble,
            funcs = funcs,
            body = body,
            main_decl = main_decl,
        );
        // runtime.c includes "runtime.h", but it is inlined above; strip the
        // include directive so the single translation unit compiles.
        let source = source.replace("#include \"runtime.h\"\n", "");

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

    fn emit_function(&self, fname: &str, params: &[Param], block: &Block) -> String {
        let cname = format!("zz_fn_{}", mangle(fname));
        let mut o = String::new();
        o.push_str(&format!(
            "static zz_value {cname}(zz_value *args, size_t argc) {{\n"
        ));
        o.push_str("    (void)argc;\n");
        let mut names = NameCtx::new();
        for (i, p) in params.iter().enumerate() {
            let cid = names.enter(&p.name.name);
            o.push_str(&format!("    zz_value {cid} = args[{i}];\n"));
        }
        let mut body_out = String::new();
        self.emit_block(block, &mut names, &mut body_out);
        o.push_str(&body_out);
        // Implicit return: last statement expression is the function value.
        if self.last_stmt_value(block, &mut names, &mut o).is_none() {
            o.push_str("    return zz_unit();\n");
        }
        o.push_str("}\n\n");
        o
    }

    fn emit_block(&self, block: &Block, names: &mut NameCtx, out: &mut String) {
        let n = block.stmts.len();
        for (i, stmt) in block.stmts.iter().enumerate() {
            let is_tail = i == n - 1;
            self.emit_stmt(stmt, names, out, is_tail);
        }
    }

    /// The last statement of a function body — if it's an expression, that
    /// becomes the implicit return value. Handles tail calls, plain
    /// expression tails, and tail if/else (returning from each branch).
    fn last_stmt_value(&self, block: &Block, names: &mut NameCtx, out: &mut String) -> Option<()> {
        if let Some(last) = block.stmts.last() {
            if let Stmt::Expr(e) = last {
                if matches!(e, Expr::If { .. }) {
                    self.emit_tail_expr(e, names, out);
                    return Some(());
                }
                // Tail call/compound was captured into __tail by emit_block.
                if let Some(tmp) = names.stack.get("__tail").and_then(|s| s.last()).cloned() {
                    out.push_str(&format!("    return {};\n", tmp.0));
                    return Some(());
                }
                // Pure leaf tail: emit directly (rarely reached).
                let val = self.emit_expr(e, names, out);
                out.push_str(&format!("    return {val};\n"));
                return Some(());
            }
        }
        None
    }

    /// Emit an expression in return position, emitting `return <val>;`.
    /// If/else tails return from each branch.
    fn emit_tail_expr(&self, e: &Expr, names: &mut NameCtx, out: &mut String) {
        match e {
            Expr::If {
                cond, then, els, ..
            } => {
                let c = self.emit_expr(cond, names, out);
                out.push_str(&format!("    if (zz_truthy({c})) {{\n"));
                if self.last_stmt_value(then, names, out).is_none() {
                    out.push_str("        return zz_unit();\n");
                }
                out.push_str("    } else {\n");
                if let Some(el) = els {
                    match el.as_ref() {
                        Expr::Block(b) => {
                            if self.last_stmt_value(b, names, out).is_none() {
                                out.push_str("        return zz_unit();\n");
                            }
                        }
                        other => self.emit_tail_expr(other, names, out),
                    }
                } else {
                    out.push_str("        return zz_unit();\n");
                }
                out.push_str("    }\n");
            }
            _ => {
                let val = self.emit_expr(e, names, out);
                out.push_str(&format!("    return {val};\n"));
            }
        }
    }

    fn emit_stmt(&self, stmt: &Stmt, names: &mut NameCtx, out: &mut String, is_tail: bool) {
        match stmt {
            Stmt::Decl { name, value, .. } => {
                // Look up the type of the initializer expression
                let ctype = if let Some(ty) = self.tp.types.get(&value.span()) {
                    ty_to_ctype(ty)
                } else {
                    "zz_value".to_string() // fallback
                };

                // Check if this is a struct initialization
                if let Expr::StructInit {
                    name: struct_name, ..
                } = value
                {
                    if self.is_unboxed_struct(struct_name) {
                        let c_type = self.struct_c_type(struct_name);
                        let cid = names.enter_with_type(&name.name, &c_type);
                        let val = self.emit_expr(value, names, out);
                        out.push_str(&format!("    {c_type} {cid} = {val};\n"));
                        return;
                    }
                }

                let cid = names.enter(&name.name);
                let val = self.emit_expr(value, names, out);
                // Update the name ctx with the correct type
                if let Some(vec) = names.stack.get_mut(&name.name) {
                    if let Some((_, existing_ty)) = vec.last_mut() {
                        *existing_ty = ctype.clone();
                    }
                }
                // For scalar types, extract the underlying value from the zz_value
                let final_val = match ctype.as_str() {
                    "int64_t" => format!("({val}).i"),
                    "double" => format!("({val}).f"),
                    "bool" => format!("({val}).b"),
                    _ => val,
                };
                out.push_str(&format!("    {ctype} {cid} = {final_val};\n"));
            }
            Stmt::Assign { target, value, .. } => {
                // Fast-path: `s = s + <rhs>` where `s` is a string
                // (zz_value). Emit an in-place append shim instead of
                // clone+binop+assign so the capacity-aware path in
                // zz_str_append_* can fire. Without this, zz_clone()
                // bumps refs and breaks the refs==1 fast path in the
                // runtime.
                //
                // SAFETY: must only fire when both sides are strings.
                //   - target type must NOT be a scalar (int64_t/double/bool).
                //   - For non-literal RHS, the RHS must itself be string-
                //     typed (else the runtime gets a non-zz_value arg).
                if let Expr::Binary {
                    op: zz_frontend::ast::BinOp::Add,
                    left,
                    right,
                    ..
                } = value
                {
                    let left_ident = match left.as_ref() {
                        Expr::Ident { name, .. } => Some(name.clone()),
                        _ => None,
                    };
                    if let (Some(lname), Expr::Ident { name: tname, .. }) = (&left_ident, target) {
                        if lname == tname {
                            // Skip fast-path for scalar targets: their
                            // storage is the raw type, not a zz_value.
                            let target_is_scalar = names
                                .lookup_type(tname.as_str())
                                .map(|t| matches!(t, "int64_t" | "double" | "bool"))
                                .unwrap_or(false);
                            if !target_is_scalar {
                                if let Some(cid) = names.lookup(tname.as_str()) {
                                    let cid = cid.to_string();
                                    match right.as_ref() {
                                        Expr::Str { value: lit, .. } => {
                                            let lit_c = self.emit_str_literal(lit);
                                            let inner = extract_c_literal(&lit_c);
                                            out.push_str(&format!(
                                                "    zz_str_append_lit(&{cid}, {inner}, sizeof({inner}) - 1);\n"
                                            ));
                                            return;
                                        }
                                        Expr::Ident { name: rname, .. } => {
                                            let rhs_scalar = names
                                                .lookup_type(rname)
                                                .map(|t| matches!(t, "int64_t" | "double" | "bool"))
                                                .unwrap_or(false);
                                            if !rhs_scalar {
                                                if let Some(rcid) = names.lookup(rname) {
                                                    let rcid = rcid.to_string();
                                                    out.push_str(&format!(
                                                        "    zz_str_append_str(&{cid}, {rcid});\n"
                                                    ));
                                                    return;
                                                }
                                            }
                                        }
                                        Expr::Path { parts, .. } => {
                                            let joined = parts.join(".");
                                            let rhs_scalar = names
                                                .lookup_type(&joined)
                                                .map(|t| matches!(t, "int64_t" | "double" | "bool"))
                                                .unwrap_or(false);
                                            if !rhs_scalar {
                                                if let Some(rcid) = names.lookup(&joined) {
                                                    let rcid = rcid.to_string();
                                                    out.push_str(&format!(
                                                        "    zz_str_append_str(&{cid}, {rcid});\n"
                                                    ));
                                                    return;
                                                }
                                            }
                                        }
                                        _ => {}
                                    }
                                }
                            }
                        }
                    }
                }

                let val = self.emit_expr(value, names, out);
                // If the RHS expression was already lowered to a raw
                // scalar (int64_t/double/bool), don't try to extract
                // .i/.f/.b — just assign directly. Otherwise the boxed
                // form `(...).i` is invalid C.
                let value_is_scalar = expr_emits_raw_scalar(value);
                match target {
                    Expr::Ident { name, .. } => {
                        if let Some(cid) = names.lookup(name.as_str()) {
                            // Check if the target variable is a scalar type
                            if let Some(ctype) = names.lookup_type(name.as_str()) {
                                match ctype {
                                    "int64_t" | "double" | "bool" => {
                                        if value_is_scalar {
                                            out.push_str(&format!("    {cid} = {val};\n"));
                                        } else {
                                            let field = match ctype {
                                                "int64_t" => ".i",
                                                "double" => ".f",
                                                "bool" => ".b",
                                                _ => "",
                                            };
                                            out.push_str(&format!("    {cid} = ({val}){field};\n"));
                                        }
                                    }
                                    _ => {
                                        out.push_str(&format!("    zz_assign(&{cid}, {val});\n"));
                                    }
                                }
                            } else {
                                out.push_str(&format!("    zz_assign(&{cid}, {val});\n"));
                            }
                        }
                    }
                    Expr::Path { parts, .. } => {
                        let joined = parts.join(".");
                        // Handle struct field assignment: p.x = 99 or r.origin.x = 42
                        if parts.len() == 2 {
                            if let Some(base_cid) = names.lookup(&parts[0]) {
                                if let Some(base_type) = names.lookup_type(&parts[0]) {
                                    if self.is_struct_type_str(base_type) {
                                        // Scalar field assignment: p.x = <val>
                                        if let Some(field_type) =
                                            self.field_type_from_struct(base_type, &parts[1])
                                        {
                                            let final_val = match field_type {
                                                "int64_t" if !value_is_scalar => {
                                                    format!("({val}).i")
                                                }
                                                "double" if !value_is_scalar => {
                                                    format!("({val}).f")
                                                }
                                                "bool" if !value_is_scalar => format!("({val}).b"),
                                                _ => val,
                                            };
                                            out.push_str(&format!(
                                                "    ({base_cid}).{field_name} = {final_val};\n",
                                                field_name = &parts[1]
                                            ));
                                            return;
                                        }
                                    }
                                }
                            }
                        }
                        if parts.len() == 3 {
                            // Nested field: r.origin.x = 42
                            if let Some(base_cid) = names.lookup(&parts[0]) {
                                if let Some(base_type) = names.lookup_type(&parts[0]) {
                                    if self.is_struct_type_str(base_type) {
                                        if let Some(field1_type) =
                                            self.field_type_from_struct(base_type, &parts[1])
                                        {
                                            if let Some(leaf_type) =
                                                self.field_type_from_struct(field1_type, &parts[2])
                                            {
                                                let final_val = match leaf_type {
                                                    "int64_t" if !value_is_scalar => {
                                                        format!("({val}).i")
                                                    }
                                                    "double" if !value_is_scalar => {
                                                        format!("({val}).f")
                                                    }
                                                    "bool" if !value_is_scalar => {
                                                        format!("({val}).b")
                                                    }
                                                    _ => val,
                                                };
                                                out.push_str(&format!(
                                                    "    (({base_cid}).{f1}).{f2} = {final_val};\n",
                                                    f1 = &parts[1],
                                                    f2 = &parts[2]
                                                ));
                                                return;
                                            }
                                        }
                                    }
                                }
                            }
                        }

                        // Standard variable assignment via path
                        if let Some(cid) = names.lookup(&joined) {
                            // Check if the target variable is a scalar type
                            if let Some(ctype) = names.lookup_type(&joined) {
                                match ctype {
                                    "int64_t" | "double" | "bool" => {
                                        if value_is_scalar {
                                            out.push_str(&format!("    {cid} = {val};\n"));
                                        } else {
                                            let field = match ctype {
                                                "int64_t" => ".i",
                                                "double" => ".f",
                                                "bool" => ".b",
                                                _ => "",
                                            };
                                            out.push_str(&format!("    {cid} = ({val}){field};\n"));
                                        }
                                    }
                                    _ => {
                                        out.push_str(&format!("    zz_assign(&{cid}, {val});\n"));
                                    }
                                }
                            } else {
                                out.push_str(&format!("    zz_assign(&{cid}, {val});\n"));
                            }
                        }
                    }
                    _ => {}
                }
            }
            Stmt::Expr(e) => {
                // Tail expressions in branch/function bodies are captured
                // into a temp so enclosing `return` can reuse the value;
                // the temp assignment still executes side effects.
                if is_tail {
                    if matches!(e, Expr::If { .. }) {
                        // If/else tails are emitted as branches by
                        // last_stmt_value (each arm returns).
                    } else if matches!(e, Expr::Call { .. }) || needs_temp(e) {
                        let tmp = format!("__tail{}", names.counter);
                        names.counter += 1;
                        let val = self.emit_expr(e, names, out);
                        out.push_str(&format!("    zz_value {tmp} = {val};\n"));
                        names
                            .stack
                            .entry("__tail".to_string())
                            .or_default()
                            .push((tmp.clone(), "zz_value".to_string()));
                    }
                    // leaf tails skipped (no side effect)
                } else if matches!(e, Expr::Call { .. }) {
                    let val = self.emit_expr(e, names, out);
                    out.push_str(&format!("    (void)({val});\n"));
                } else if !is_leaf_expr(e) {
                    let _ = self.emit_expr(e, names, out);
                }
            }
            Stmt::Return { value, .. } => match value {
                Some(v) => {
                    let val = self.emit_expr(v, names, out);
                    out.push_str(&format!("    return {val};\n"));
                }
                None => out.push_str("    return zz_unit();\n"),
            },
            Stmt::For {
                vars, iter, body, ..
            } => {
                self.emit_for(vars, iter, body, names, out);
            }
            Stmt::Break { .. } => out.push_str("    break;\n"),
            Stmt::Continue { .. } => out.push_str("    continue;\n"),
            Stmt::Defer { .. } | Stmt::Destructure { .. } => {
                out.push_str("    // unsupported statement skipped\n");
            }
            Stmt::Func { .. } | Stmt::Struct { .. } | Stmt::Impl { .. } | Stmt::Import { .. } => {}
        }
    }

    fn emit_for(
        &self,
        vars: &[zz_frontend::ast::Ident],
        iter: &Expr,
        body: &Block,
        names: &mut NameCtx,
        out: &mut String,
    ) {
        // Resolve the iteration bounds: `a..b` or `range(...)` builtin calls.
        // Returns (start_expr, end_expr) when statically int-rangeable.
        let bounds: Option<(Expr, Expr)> = match iter {
            Expr::Range { start, end, .. } => Some(((**start).clone(), (**end).clone())),
            Expr::Call { callee, args, .. } => {
                let is_range = matches!(
                    callee.as_ref(),
                    Expr::Ident { name, .. } if name == "range"
                );
                if is_range {
                    match args.len() {
                        // range(stop) == 0..stop
                        1 => Some((
                            Expr::Int {
                                value: 0,
                                span: zz_frontend::span::Span { start: 0, end: 0 },
                            },
                            args[0].clone(),
                        )),
                        // range(start, stop)
                        2 => Some((args[0].clone(), args[1].clone())),
                        _ => None,
                    }
                } else {
                    None
                }
            }
            _ => None,
        };

        if let Some((start_expr, end_expr)) = bounds {
            // Check if end is an unboxed scalar variable (for fast path)
            let end_name_opt: Option<String> = match &end_expr {
                Expr::Ident { name, .. } => Some(name.clone()),
                Expr::Path { parts, .. } => Some(parts.join(".")),
                _ => None,
            };
            let end_is_scalar = end_name_opt
                .as_ref()
                .and_then(|n| names.lookup_type(n))
                .map(|t| t == "int64_t" || t == "double" || t == "bool")
                .unwrap_or(false);

            let sv = self.emit_expr(&start_expr, names, out);
            let ev = self.emit_expr(&end_expr, names, out);
            let v = &vars[0].name;
            let cid = names.enter(v);
            // Update the type in NameCtx to int64_t since the loop variable is unboxed
            if let Some(vec) = names.stack.get_mut(v) {
                if let Some((_, existing_ty)) = vec.last_mut() {
                    *existing_ty = "int64_t".to_string();
                }
            }

            // Fast path: if end is an unboxed scalar, use unboxed C loop variables
            if end_is_scalar {
                // Determine the actual unboxed value
                // If ev is a simple C identifier (like "v0"), it's already unboxed
                // If ev is a boxing call like "zz_int(...)", extract the inner expression
                // Otherwise, unbox it with .i
                let is_simple_ident = |s: &str| -> bool {
                    !s.is_empty()
                        && s.chars().all(|c| c.is_alphanumeric() || c == '_')
                        && !s.starts_with("zz_")
                };

                let ev_unboxed = if is_simple_ident(&ev) {
                    // ev is a simple identifier (like v0), it's already unboxed
                    ev.clone()
                } else if ev.starts_with("zz_int(")
                    || ev.starts_with("zz_float(")
                    || ev.starts_with("zz_bool(")
                {
                    // Already a boxing call, extract the inner expression
                    // e.g., "zz_int(v0)" -> "v0"
                    let inner = &ev[7..ev.len() - 1];
                    inner.to_string()
                } else {
                    // Some other expression, unbox it
                    format!("({ev}).i")
                };

                // For start, similar logic
                let sv_unboxed = if sv.starts_with("zz_int(") {
                    // Literal like zz_int(0) -> extract the literal
                    let inner = &sv[7..sv.len() - 1];
                    inner.to_string()
                } else if is_simple_ident(&sv) {
                    // Simple identifier, already unboxed
                    sv.clone()
                } else {
                    // For other expressions, assume they need unboxing
                    format!("({sv}).i")
                };

                let s = format!(
                    "for (int64_t {cid} = {sv_unboxed}; {cid} < {ev_unboxed}; {cid}++) {{\n"
                );
                out.push_str(&s);
            } else {
                // Slow path: both bounds are general expressions, use boxed loop
                let sv_boxed = sv;
                let ev_boxed = auto_box(&ev, if end_is_scalar { Some("int64_t") } else { None });
                let s = format!(
                    "{{ zz_value _s = {sv_boxed}; zz_value _e = {ev_boxed};\n    \
                     if (_s.tag == ZZ_INT && _e.tag == ZZ_INT) {{\n        \
                     for (int64_t {cid}_i = _s.i; {cid}_i < _e.i; {cid}_i++) {{\n            \
                     int64_t {cid} = {cid}_i;\n"
                );
                out.push_str(&s);
            }
            // Loop body is never a function tail.
            for bstmt in &body.stmts {
                self.emit_stmt(bstmt, names, out, false);
            }
            out.push_str("    }\n");
            if !end_is_scalar {
                out.push_str("    }\n}\n");
            }
            names.leave(v);
        } else {
            out.push_str("    // for over non-range unsupported\n");
        }
    }

    fn emit_expr(&self, e: &Expr, names: &mut NameCtx, out: &mut String) -> String {
        match e {
            Expr::Int { value, .. } => format!("zz_int({value})"),
            Expr::Float { value, .. } => {
                let s = if *value == (*value).floor() && (*value).abs() < 1e15 {
                    format!("{value:.1}")
                } else {
                    format!("{value}")
                };
                format!("zz_float({s})")
            }
            Expr::Bool { value, .. } => {
                format!("zz_bool({})", if *value { "true" } else { "false" })
            }
            Expr::Str { value, .. } => self.emit_str_literal(value),
            Expr::Fmt { parts, .. } => {
                // Full fstring: build via runtime interp. MVP: only literal
                // text handled; embedded exprs appended as str(values).
                let mut acc = String::from("zz_str_static(\"\")");
                for part in parts {
                    match part {
                        FmtPart::Text(t) => {
                            let lit = self.emit_str_literal(t);
                            acc = format!("zz_binop_cat({acc}, {lit})");
                        }
                        FmtPart::Expr(inner, _) => {
                            let v = self.emit_expr(inner, names, out);
                            // Auto-box if the embedded expression is an unboxed scalar
                            let boxed_v = if let Expr::Ident { name, .. } = inner.as_ref() {
                                let name_str = name.clone();
                                auto_box(&v, names.lookup_type(&name_str))
                            } else if let Expr::Path { parts, .. } = inner.as_ref() {
                                let joined = parts.join(".");
                                auto_box(&v, names.lookup_type(&joined))
                            } else {
                                v
                            };
                            acc = format!("zz_binop_cat_str({acc}, {boxed_v})");
                        }
                    }
                }
                acc
            }
            Expr::Ident { name, .. } => match names.lookup(name) {
                Some(cid) => {
                    // Check if the variable is a scalar type that can be used directly
                    if let Some(ctype) = names.lookup_type(name) {
                        match ctype {
                            "int64_t" | "double" | "bool" => format!("{cid}"),
                            _ => format!("zz_clone({cid})"),
                        }
                    } else {
                        // Fallback to safe behavior if type unknown
                        format!("zz_clone({cid})")
                    }
                }
                None => "zz_unit()".to_string(),
            },
            Expr::Path { parts, .. } => {
                // Handle struct field access (e.g., p.x or r.origin.x)
                if parts.len() == 2 {
                    if let Some(base_name) = names.lookup(&parts[0]) {
                        if let Some(base_type) = names.lookup_type(&parts[0]) {
                            // Check if base is a struct type
                            if self.is_struct_type_str(base_type) {
                                // Extract field access
                                let field_name = &parts[1];
                                return format!("({base_name}).{field_name}");
                            }
                        }
                    }
                }

                // Handle nested struct field access (e.g., r.origin.x = parts[0..2] + parts[2])
                if parts.len() >= 3 {
                    // Try to find the base variable
                    if let Some(base_name) = names.lookup(&parts[0]) {
                        if let Some(base_type) = names.lookup_type(&parts[0]) {
                            // For now, only handle 2-level deep fields
                            if parts.len() == 3 && self.is_struct_type_str(base_type) {
                                let field1 = &parts[1];
                                let field2 = &parts[2];
                                return format!("(({base_name}).{field1}).{field2}");
                            }
                        }
                    }
                }

                let joined = parts.join(".");
                if let Some(cid) = names.lookup(&joined) {
                    // Check if the variable is a scalar type that can be used directly
                    if let Some(ctype) = names.lookup_type(&joined) {
                        match ctype {
                            "int64_t" | "double" | "bool" => format!("{cid}"),
                            _ if self.is_struct_type_str(ctype) => format!("{cid}"),
                            _ => format!("zz_clone({cid})"),
                        }
                    } else {
                        // Fallback to safe behavior if type unknown
                        format!("zz_clone({cid})")
                    }
                } else {
                    "zz_unit()".to_string()
                }
            }
            Expr::Paren { expr, .. } => self.emit_expr(expr, names, out),
            Expr::Unary { op, expr, .. } => {
                let v = self.emit_expr(expr, names, out);
                match op {
                    zz_frontend::ast::UnOp::Neg => format!("zz_neg({v})"),
                    zz_frontend::ast::UnOp::Pos => v,
                    zz_frontend::ast::UnOp::Not => format!("zz_not({v})"),
                }
            }
            Expr::Binary {
                op, left, right, ..
            } => {
                let l = self.emit_expr(left, names, out);
                let r = self.emit_expr(right, names, out);
                match op {
                    zz_frontend::ast::BinOp::And => {
                        format!("zz_bool(zz_truthy({l}) && zz_truthy({r}))")
                    }
                    zz_frontend::ast::BinOp::Or => {
                        format!("zz_bool(zz_truthy({l}) || zz_truthy({r}))")
                    }
                    zz_frontend::ast::BinOp::Elvis => {
                        format!("(zz_truthy({l}) ? ({l}) : ({r}))")
                    }
                    _ => {
                        let cop = match op {
                            zz_frontend::ast::BinOp::Add => "ZZOP_ADD",
                            zz_frontend::ast::BinOp::Sub => "ZZOP_SUB",
                            zz_frontend::ast::BinOp::Mul => "ZZOP_MUL",
                            zz_frontend::ast::BinOp::Div => "ZZOP_DIV",
                            zz_frontend::ast::BinOp::Rem => "ZZOP_REM",
                            zz_frontend::ast::BinOp::Pow => "ZZOP_POW",
                            zz_frontend::ast::BinOp::Eq => "ZZOP_EQ",
                            zz_frontend::ast::BinOp::Ne => "ZZOP_NE",
                            zz_frontend::ast::BinOp::Lt => "ZZOP_LT",
                            zz_frontend::ast::BinOp::Gt => "ZZOP_GT",
                            zz_frontend::ast::BinOp::Le => "ZZOP_LE",
                            zz_frontend::ast::BinOp::Ge => "ZZOP_GE",
                            _ => "ZZOP_ADD",
                        };
                        // Check if either operand is a scalar variable
                        let left_type = if let Expr::Ident { name, .. } = left.as_ref() {
                            names.lookup_type(name)
                        } else {
                            None
                        };
                        let right_type = if let Expr::Ident { name, .. } = right.as_ref() {
                            names.lookup_type(name)
                        } else {
                            None
                        };

                        // Both operands are scalars of compatible type:
                        // emit a raw C arithmetic op instead of going
                        // through zz_binop (which would box/unbox each
                        // iteration and dominate tight loops). Comparison
                        // ops on scalars stay boxed because we still
                        // need the result wrapped in a zz_value.
                        let is_arith = matches!(
                            op,
                            zz_frontend::ast::BinOp::Add
                                | zz_frontend::ast::BinOp::Sub
                                | zz_frontend::ast::BinOp::Mul
                                | zz_frontend::ast::BinOp::Div
                                | zz_frontend::ast::BinOp::Rem
                        );
                        if is_arith {
                            if left_type == Some("int64_t") && right_type == Some("int64_t") {
                                let c_op = match op {
                                    zz_frontend::ast::BinOp::Add => "+",
                                    zz_frontend::ast::BinOp::Sub => "-",
                                    zz_frontend::ast::BinOp::Mul => "*",
                                    zz_frontend::ast::BinOp::Div => "/",
                                    zz_frontend::ast::BinOp::Rem => "%",
                                    _ => "+",
                                };
                                // Division/modulo by zero still needs a guard.
                                if matches!(
                                    op,
                                    zz_frontend::ast::BinOp::Div | zz_frontend::ast::BinOp::Rem
                                ) && matches!(
                                    right.as_ref(),
                                    Expr::Int { value: 0, .. } | Expr::Ident { .. }
                                ) {
                                    // Only literal-zero check; variable
                                    // divisor would need a runtime guard.
                                    // Fall through to boxed path if divisor
                                    // is a literal 0.
                                    if matches!(right.as_ref(), Expr::Int { value: 0, .. }) {
                                        format!("zz_binop({cop}, {l}, {r})")
                                    } else {
                                        format!("(int64_t)({l} {c_op} {r})")
                                    }
                                } else {
                                    format!("(int64_t)({l} {c_op} {r})")
                                }
                            } else if left_type == Some("double") && right_type == Some("double") {
                                let c_op = match op {
                                    zz_frontend::ast::BinOp::Add => "+",
                                    zz_frontend::ast::BinOp::Sub => "-",
                                    zz_frontend::ast::BinOp::Mul => "*",
                                    zz_frontend::ast::BinOp::Div => "/",
                                    zz_frontend::ast::BinOp::Rem => "fmod",
                                    _ => "+",
                                };
                                if matches!(op, zz_frontend::ast::BinOp::Rem) {
                                    format!("(double)(fmod({l}, {r}))")
                                } else {
                                    format!("(double)({l} {c_op} {r})")
                                }
                            } else if left_type == Some("int64_t") || right_type == Some("int64_t")
                            {
                                // Mixed: one scalar, one boxed. Box both
                                // sides and dispatch through zz_binop.
                                let boxed_l = if left_type == Some("int64_t") {
                                    format!("zz_int({l})")
                                } else {
                                    l
                                };
                                let boxed_r = if right_type == Some("int64_t") {
                                    format!("zz_int({r})")
                                } else {
                                    r
                                };
                                format!("zz_binop({cop}, {boxed_l}, {boxed_r})")
                            } else if left_type == Some("double") || right_type == Some("double") {
                                let boxed_l = if left_type == Some("double") {
                                    format!("zz_float({l})")
                                } else {
                                    l
                                };
                                let boxed_r = if right_type == Some("double") {
                                    format!("zz_float({r})")
                                } else {
                                    r
                                };
                                format!("zz_binop({cop}, {boxed_l}, {boxed_r})")
                            } else {
                                format!("zz_binop({cop}, {l}, {r})")
                            }
                        } else {
                            // Comparisons / pow / etc on scalars still
                            // use the boxed path (result must be zz_value).
                            if left_type == Some("int64_t") || right_type == Some("int64_t") {
                                let boxed_l = if left_type == Some("int64_t") {
                                    format!("zz_int({l})")
                                } else {
                                    l
                                };
                                let boxed_r = if right_type == Some("int64_t") {
                                    format!("zz_int({r})")
                                } else {
                                    r
                                };
                                format!("zz_binop({cop}, {boxed_l}, {boxed_r})")
                            } else if left_type == Some("double") || right_type == Some("double") {
                                let boxed_l = if left_type == Some("double") {
                                    format!("zz_float({l})")
                                } else {
                                    l
                                };
                                let boxed_r = if right_type == Some("double") {
                                    format!("zz_float({r})")
                                } else {
                                    r
                                };
                                format!("zz_binop({cop}, {boxed_l}, {boxed_r})")
                            } else {
                                format!("zz_binop({cop}, {l}, {r})")
                            }
                        }
                    }
                }
            }
            Expr::Call { callee, args, .. } => self.emit_call(callee, args, names, out),
            Expr::If {
                cond, then, els, ..
            } => {
                let c = self.emit_expr(cond, names, out);
                out.push_str(&format!("    if (zz_truthy({c})) {{\n"));
                self.emit_block(then, names, out);
                if let Some(el) = els {
                    out.push_str("    } else {\n");
                    // else branch as expression: emit statements then unit
                    self.emit_block(get_block(el), names, out);
                    out.push_str("    }\n");
                } else {
                    out.push_str("    }\n");
                }
                "zz_unit()".to_string()
            }
            Expr::Block(b) => {
                self.emit_block(b, names, out);
                "zz_unit()".to_string()
            }
            Expr::Range { start, end, .. } => {
                let s = self.emit_expr(start, names, out);
                let en = self.emit_expr(end, names, out);
                format!("zz_range_build({s}, {en})")
            }
            Expr::StructInit { name, fields, .. } => {
                // Check if this struct is unboxed
                if self.is_unboxed_struct(name) {
                    let c_type = self.struct_c_type(name);
                    let mut field_inits = Vec::new();
                    for (field_name, field_expr) in fields {
                        let field_val = self.emit_expr(field_expr, names, out);
                        // Check if the field type is scalar
                        if let Some(sig) = self.tp.structs.get(name) {
                            if let Some((_, field_type)) =
                                sig.fields.iter().find(|(n, _)| n == field_name)
                            {
                                let final_val = match field_type {
                                    zz_checker::Type::Int => format!("({field_val}).i"),
                                    zz_checker::Type::Float => format!("({field_val}).f"),
                                    zz_checker::Type::Bool => format!("({field_val}).b"),
                                    _ => field_val,
                                };
                                field_inits.push(format!(".{field_name} = {final_val}"));
                            } else {
                                field_inits.push(format!(".{field_name} = {field_val}"));
                            }
                        } else {
                            field_inits.push(format!(".{field_name} = {field_val}"));
                        }
                    }
                    format!("({c_type}){{ {}}}", field_inits.join(", "))
                } else {
                    "zz_unit()".to_string()
                }
            }
            Expr::Field { obj, name, .. } => {
                // For now, just emit the object and append the field access
                let obj_val = self.emit_expr(obj, names, out);
                format!("({obj_val}).{name}")
            }
            Expr::Array { elems, .. } => {
                // Emit array literal: zz_array_new() then append each item.
                let arr_var = format!("__arr{}", names.counter);
                names.counter += 1;
                out.push_str(&format!("    zz_value {arr_var} = zz_array_new();\n"));
                for item in elems {
                    let item_val = self.emit_expr(item, names, out);
                    // Auto-box if needed
                    let boxed = if let Expr::Ident { name: n, .. } = item {
                        auto_box(&item_val, names.lookup_type(n))
                    } else {
                        item_val
                    };
                    out.push_str(&format!(
                        "    {{ int _e = 0; zz_vec_append({arr_var}, {boxed}, &_e); }}\n"
                    ));
                }
                arr_var
            }
            Expr::Dict { .. } => "zz_dict_new()".to_string(),
            Expr::Fmt { parts, .. } => {
                // Format string: build by concatenating parts as strings.
                if parts.is_empty() {
                    return "zz_str_static(\"\")".to_string();
                }
                let fvar = format!("__fmt{}", names.counter);
                names.counter += 1;
                out.push_str(&format!("    zz_value {fvar} = zz_str_static(\"\");\n"));
                for part in parts {
                    match part {
                        zz_frontend::ast::FmtPart::Text(value) => {
                            let lit = self.emit_str_literal(value);
                            out.push_str(&format!("    {{ zz_value _r = zz_to_str({lit}, &(int){{0}}); {fvar} = zz_binop_cat({fvar}, _r); }}\n"));
                        }
                        zz_frontend::ast::FmtPart::Expr(expr, _spec) => {
                            let val = self.emit_expr(expr, names, out);
                            out.push_str(&format!("    {{ zz_value _r = zz_to_str({val}, &(int){{0}}); {fvar} = zz_binop_cat({fvar}, _r); }}\n"));
                        }
                    }
                }
                fvar
            }
            _ => "zz_unit()".to_string(),
        }
    }

    fn emit_call(
        &self,
        callee: &Expr,
        args: &[Expr],
        names: &mut NameCtx,
        out: &mut String,
    ) -> String {
        // Resolve callee name — handle method dispatch for Path/Field expressions.
        // Returns (cname, method_receiver) where method_receiver is the owned Expr
        // to insert as the first argument for method calls like `x.push(4)`.
        let (cname, method_receiver): (String, Option<Expr>) = match callee {
            Expr::Ident { name, .. } => (name.clone(), None),
            Expr::Path { parts, span, .. } if parts.len() == 2 => {
                let obj_name = &parts[0];
                let method = &parts[1];
                if names.lookup(obj_name).is_some() {
                    // obj_name is a LOCAL variable — this is a method call.
                    // Try each known method namespace to find a registered native.
                    let first_ident_end = span.start + obj_name.len() as u32;
                    let first_ident_span =
                        zz_frontend::span::Span::new(span.start, first_ident_end);
                    let namespaces = ["vec", "str", "dict", "option", "result"];
                    let mut found_ns = "";
                    for ns in &namespaces {
                        let candidate = format!("{ns}.{method}");
                        let std_candidate = format!("std.{ns}.{method}");
                        if self.reachable_natives.contains(&candidate)
                            || self.reachable_natives.contains(&std_candidate)
                        {
                            found_ns = ns;
                            break;
                        }
                    }
                    if found_ns.is_empty() {
                        // Also try matching by native_impl — checks if there's
                        // a C runtime function registered for this method under
                        // any namespace.
                        for ns in &namespaces {
                            let candidate = format!("{ns}.{method}");
                            if native_supported(&candidate) {
                                found_ns = ns;
                                break;
                            }
                        }
                        if found_ns.is_empty() {
                            eprintln!("[codegen-warn] method {}.{}: no namespace found. reach_natives={:?}", obj_name, method, self.reachable_natives);
                        }
                    }
                    if found_ns.is_empty() {
                        // Also check if the bare method name is a native
                        // (e.g. `len`, `println`).
                        if self.reachable_natives.contains(method) {
                            // Bare builtin — no receiver injection needed.
                            (method.clone(), None)
                        } else {
                            // Unknown — fall through
                            (method.clone(), None)
                        }
                    } else {
                        let receiver = Expr::Ident {
                            name: obj_name.clone(),
                            span: first_ident_span,
                        };
                        (format!("{found_ns}.{method}"), Some(receiver))
                    }
                } else {
                    // obj_name is NOT a local — it's a namespace like `vec`, `io`.
                    (parts.join("."), None)
                }
            }
            Expr::Path { parts, .. } => (parts.join("."), None),
            Expr::Field {
                obj, name: method, ..
            } => {
                // Expr::Field callee — rare since parser consumes ident chains as Path.
                // Look up receiver type and dispatch.
                if let Some(zzty) = self.tp.types.get(&obj.span()) {
                    match zzty {
                        zz_checker::Type::Struct(sname) => (format!("{sname}.{method}"), None),
                        _ => {
                            let ns = match zzty {
                                zz_checker::Type::Array(_) => "vec",
                                zz_checker::Type::Str => "str",
                                zz_checker::Type::Dict(_, _) => "dict",
                                zz_checker::Type::Option(_) => "option",
                                zz_checker::Type::Result(_, _) => "result",
                                _ => "",
                            };
                            if !ns.is_empty() {
                                (format!("{ns}.{method}"), Some(*obj.clone()))
                            } else {
                                (method.clone(), None)
                            }
                        }
                    }
                } else {
                    (method.clone(), None)
                }
            }
            _ => return "zz_unit()".to_string(),
        };

        let mut arg_items: Vec<String> = Vec::new();
        // If this is a method call, emit and insert the receiver as first arg.
        if let Some(recv) = method_receiver {
            let recv_val = self.emit_expr(&recv, names, out);
            let boxed = auto_box(&recv_val, None); // receiver is always zz_value
            arg_items.push(boxed);
        }
        for a in args {
            let emitted = self.emit_expr(a, names, out);
            // Auto-box if this argument is a scalar variable or struct field
            let boxed = if let Expr::Ident { name, .. } = a {
                let name_str = name.clone();
                auto_box(&emitted, names.lookup_type(&name_str))
            } else if let Expr::Path { parts, .. } = a {
                let joined = parts.join(".");
                // First try direct lookup (e.g., a local variable named "p.x")
                let direct_type = names.lookup_type(&joined);
                if direct_type.is_some() {
                    auto_box(&emitted, direct_type)
                } else if parts.len() == 2 {
                    // Struct field access: parts[0] is the base, parts[1] is the field
                    if let Some(base_type) = names.lookup_type(&parts[0]) {
                        if let Some(field_type) = self.field_type_from_struct(base_type, &parts[1])
                        {
                            auto_box(&emitted, Some(field_type))
                        } else {
                            emitted
                        }
                    } else {
                        emitted
                    }
                } else if parts.len() >= 3 {
                    // Nested struct field: e.g., r.origin.x
                    self.auto_box_nested_field(parts, names, &emitted)
                } else {
                    emitted
                }
            } else {
                emitted
            };
            arg_items.push(boxed);
        }

        // Only lower natives that survived DCE reachability AND have a C
        // runtime implementation.
        let cname_for_native = cname.clone();
        // A native may be bound under `std.io.println` (stdlib_funcs) while
        // the source calls `io.println` (namespace-registered). Match either.
        let std_name = format!("std.{cname_for_native}");
        let is_native = self.reachable_natives.contains(&cname_for_native)
            || self.reachable_natives.contains(&std_name);
        let native_rt = if is_native {
            native_impl(&cname_for_native)
        } else {
            None
        };
        if let Some(impl_name) = native_rt {
            return match arg_items.len() {
                1 => {
                    let a = &arg_items[0];
                    format!("zz_call_native1({impl_name}, {a})")
                }
                0 => format!("zz_call_native0({impl_name})"),
                2 => {
                    let a = &arg_items[0];
                    let b = &arg_items[1];
                    format!("zz_call_native2({impl_name}, {a}, {b})")
                }
                3 => {
                    let a = &arg_items[0];
                    let b = &arg_items[1];
                    let c = &arg_items[2];
                    format!("zz_call_native3({impl_name}, {a}, {b}, {c})")
                }
                _ => "zz_unit()".to_string(),
            };
        }

        // Reachable native without a C runtime impl (e.g. time.now_ms)
        // lowers to Unit.
        if is_native {
            let _ = (cname_for_native.is_empty(),);
            return "zz_unit()".to_string();
        }

        if self.reachable_funcs.contains(&cname) {
            let cf = format!("zz_fn_{}", mangle(&cname));
            if arg_items.is_empty() {
                return format!("{cf}(NULL, 0)");
            }
            return format!(
                "({cf}((zz_value[]){{ {joined} }}, {n}))",
                joined = arg_items.join(", "),
                n = args.len()
            );
        }

        "zz_unit()".to_string()
    }

    fn emit_str_literal(&self, s: &str) -> String {
        let mut o = String::from("\"");
        for c in s.chars() {
            match c {
                '"' => o.push_str("\\\""),
                '\\' => o.push_str("\\\\"),
                '\n' => o.push_str("\\n"),
                '\r' => o.push_str("\\r"),
                '\t' => o.push_str("\\t"),
                c if (c as u32) < 32 => o.push_str(&format!("\\x{:02x}", c as u32)),
                c => o.push(c),
            }
        }
        o.push('"');
        format!("zz_str_static({o})")
    }
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
        "input" | "io.read_line" | "main_io.input" => Some("zz_io_input"),
        "len" => Some("zz_len"),
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
        // json
        "json.parse" | "std.json.parse" => Some("zz_json_parse"),
        "json.stringify" | "std.json.stringify" => Some("zz_json_stringify"),
        "json.null" | "std.json.null" => Some("zz_json_null"),
        // env
        "env.get" | "std.env.get" | "envmod.get" | "std.envmod.get" => Some("zz_env_get"),
        "env.args" | "std.env.args" | "envmod.args" | "std.envmod.args" => Some("zz_env_args"),
        // dict
        "dict.len" => Some("zz_dict_len_val"),
        "dict.keys" => Some("zz_dict_keys"),
        "dict.has" => Some("zz_dict_has"),
        // fs
        "fs.read" | "std.fs.read" => Some("zz_fs_read"),
        "fs.write" | "std.fs.write" => Some("zz_fs_write"),
        "fs.exists" | "std.fs.exists" => Some("zz_fs_exists"),
        "fs.remove" | "std.fs.remove" => Some("zz_fs_remove"),
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
        _ => None,
    }
}

/// Whether a reachable native has a C runtime implementation.
pub fn native_supported(name: &str) -> bool {
    native_impl(name).is_some()
}
