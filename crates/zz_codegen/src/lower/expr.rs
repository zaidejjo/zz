//! Expression lowering: literals, binary ops, function calls, field access,
//! collections, variants, closures, and match expressions.

use zz_frontend::ast::{Block, Decorator, Expr, FmtPart, MatchArm, Param, Pattern};

use super::green::GreenCtx;
use super::*;

// ── Route-handler request-use analysis ───────────────────────────────────
// True when a 1-param closure never references its parameter, letting the
// socket fast path skip request-dict construction. Deliberately
// over-approximate: ANY same-name Ident counts as a use (shadowing
// ignored), non-literal handlers never qualify. Misses only lose the
// optimization; a wrong `true` would miscompile, so every AST node type
// is matched explicitly — no wildcards (new variants fail loudly here).
fn closure_ignores_param(params: &[Param], body: &Expr) -> bool {
    let [p] = params else { return false };
    !mentions_ident(body, &p.name.name)
}

fn mentions_ident(e: &Expr, name: &str) -> bool {
    match e {
        Expr::Int { .. }
        | Expr::Float { .. }
        | Expr::Str { .. }
        | Expr::Bool { .. }
        | Expr::Break { .. }
        | Expr::Continue { .. } => false,
        Expr::Ident { name: n, .. } => n == name,
        // Dotted chains: any segment match counts (over-approx, safe).
        Expr::Path { parts, .. } => parts.iter().any(|p| p == name),
        Expr::Fmt { parts, .. } => parts.iter().any(|pt| match pt {
            FmtPart::Text(_) => false,
            FmtPart::Expr(inner, _) => mentions_ident(inner, name),
        }),
        Expr::Paren { expr, .. } => mentions_ident(expr, name),
        Expr::Tuple { items, .. } => items.iter().any(|i| mentions_ident(i, name)),
        Expr::Unary { expr, .. } => mentions_ident(expr, name),
        Expr::Binary { left, right, .. } => {
            mentions_ident(left, name) || mentions_ident(right, name)
        }
        Expr::Call {
            callee,
            args,
            named,
            ..
        } => {
            mentions_ident(callee, name)
                || args.iter().any(|a| mentions_ident(a, name))
                || named.iter().any(|(_, v)| mentions_ident(v, name))
        }
        Expr::Closure { params, body, .. } => {
            params.iter().any(|p| mentions_default(&p.default, name)) || mentions_ident(body, name)
        }
        Expr::If {
            cond, then, els, ..
        } => {
            mentions_ident(cond, name)
                || mentions_block(then, name)
                || els.as_ref().is_some_and(|b| mentions_ident(b, name))
        }
        Expr::While { cond, body, .. } => mentions_ident(cond, name) || mentions_block(body, name),
        Expr::Match {
            scrutinee, arms, ..
        } => mentions_ident(scrutinee, name) || arms.iter().any(|a| mentions_match_arm(a, name)),
        Expr::IfLet {
            pat,
            value,
            then,
            els,
            ..
        } => {
            mentions_pat(pat, name)
                || mentions_ident(value, name)
                || mentions_block(then, name)
                || els.as_ref().is_some_and(|b| mentions_ident(b, name))
        }
        Expr::Try { expr, .. } => mentions_ident(expr, name),
        Expr::Block(b) => mentions_block(b, name),
        Expr::Variant { arg, .. } => arg.as_ref().is_some_and(|a| mentions_ident(a, name)),
        Expr::Array { elems, .. } => elems.iter().any(|el| mentions_ident(el, name)),
        Expr::Dict { entries, .. } => entries
            .iter()
            .any(|(k, v)| mentions_ident(k, name) || mentions_ident(v, name)),
        // Field name is a static member, never a variable use.
        Expr::Field { obj, .. } => mentions_ident(obj, name),
        Expr::Range { start, end, .. } => mentions_ident(start, name) || mentions_ident(end, name),
        Expr::StructInit { fields, .. } => fields.iter().any(|(_, v)| mentions_ident(v, name)),
        Expr::Index { obj, index, .. } => mentions_ident(obj, name) || mentions_ident(index, name),
        Expr::Slice {
            obj, start, end, ..
        } => {
            mentions_ident(obj, name)
                || start.as_ref().is_some_and(|s| mentions_ident(s, name))
                || end.as_ref().is_some_and(|e| mentions_ident(e, name))
        }
        Expr::ListComp {
            body, iter, filter, ..
        } => {
            mentions_ident(body, name)
                || mentions_ident(iter, name)
                || filter.as_ref().is_some_and(|f| mentions_ident(f, name))
        }
    }
}

fn mentions_default(default: &Option<Box<Expr>>, name: &str) -> bool {
    default.as_ref().is_some_and(|d| mentions_ident(d, name))
}

fn mentions_block(b: &Block, name: &str) -> bool {
    b.stmts.iter().any(|s| mentions_stmt(s, name))
}

fn mentions_match_arm(a: &MatchArm, name: &str) -> bool {
    mentions_pat(&a.pat, name)
        || a.guard.as_ref().is_some_and(|g| mentions_ident(g, name))
        || mentions_ident(&a.body, name)
}

// Pattern bindings declare names; references in guards/bodies are walked
// separately (counting a binding as a use would only ever miss the opt).
fn mentions_pat(p: &Pattern, _name: &str) -> bool {
    // Bindings declare names; a use in a guard/body is walked separately.
    // Always false here (over-approx would only ever miss the opt).
    match p {
        Pattern::Wildcard { .. } | Pattern::Binding { .. } | Pattern::Literal { .. } => false,
        Pattern::Variant { arg, .. } => arg.as_ref().is_some_and(|a| mentions_pat(a, _name)),
        Pattern::Tuple { pats, .. } | Pattern::Or { pats, .. } => {
            pats.iter().any(|q| mentions_pat(q, _name))
        }
    }
}

fn mentions_stmt(s: &Stmt, name: &str) -> bool {
    match s {
        Stmt::Decl { value, .. } => mentions_ident(value, name),
        Stmt::Import { .. } | Stmt::Break { .. } | Stmt::Continue { .. } | Stmt::Link { .. } => {
            false
        }
        Stmt::Func {
            params,
            body,
            decorators,
            ..
        } => {
            params.iter().any(|p| mentions_default(&p.default, name))
                || mentions_block(body, name)
                || decorators.iter().any(|d| mentions_decorator(d, name))
        }
        Stmt::Return { value, .. } => value.as_ref().is_some_and(|v| mentions_ident(v, name)),
        // Struct shapes and aliases carry types only.
        Stmt::Struct { .. } | Stmt::TypeAlias { .. } => false,
        Stmt::Impl { methods, .. } => methods.iter().any(|m| mentions_stmt(m, name)),
        Stmt::For { iter, body, .. } => mentions_ident(iter, name) || mentions_block(body, name),
        Stmt::Defer { expr, .. } => mentions_ident(expr, name),
        Stmt::Assign { target, value, .. } => {
            mentions_ident(target, name) || mentions_ident(value, name)
        }
        Stmt::CompoundAssign { target, value, .. } => {
            mentions_ident(target, name) || mentions_ident(value, name)
        }
        Stmt::Destructure { pat, value, .. } => {
            mentions_pat(pat, name) || mentions_ident(value, name)
        }
        Stmt::ExternBlock { items, .. } => items
            .iter()
            .any(|f| f.params.iter().any(|p| mentions_default(&p.default, name))),
        Stmt::Expr(e) => mentions_ident(e, name),
    }
}

fn mentions_decorator(d: &Decorator, name: &str) -> bool {
    d.args.iter().any(|a| mentions_ident(a, name))
        || d.named.iter().any(|(_, v)| mentions_ident(v, name))
}

impl Lowerer {
    /// Emit a block in value position, returning a C expression string
    /// for its tail value. Shared by value-position blocks and if-branch
    /// bodies so both agree on what a branch yields. A trailing `if`
    /// tail recurses through value-position if lowering, so elif chains
    /// yield branch values instead of unit.
    pub(super) fn emit_block_value(
        &self,
        b: &Block,
        names: &mut NameCtx,
        out: &mut String,
    ) -> String {
        names.clear_array_lens();
        // Lexical scope: declarations inside the block (including
        // shadowing re-declarations) must not leak into the enclosing
        // NameCtx — the C declarations live inside braces, so a stale
        // entry would resolve to an out-of-scope identifier (or a shadowed
        // one). Mirrors the for-loop body's push/pop_scope discipline.
        names.push_scope();
        let tail_saved = names.stack.get("__tail").map(|v| v.len()).unwrap_or(0);
        let n = b.stmts.len();
        for (i, stmt) in b.stmts.iter().enumerate() {
            self.emit_stmt(stmt, names, out, i == n - 1);
        }
        let tail_tmp = names.stack.get_mut("__tail").and_then(|v| {
            if v.len() > tail_saved {
                v.pop().map(|(t, _)| t)
            } else {
                None
            }
        });
        let result = if let Some(tmp) = tail_tmp {
            tmp
        } else if let Some(Stmt::Expr(e)) = b.stmts.last() {
            if matches!(e, Expr::If { .. }) {
                self.emit_expr(e, names, out)
            } else {
                self.emit_tail_value(e, names, out)
            }
        } else if let Some(Stmt::Decl { name, .. }) = b.stmts.last() {
            // Trailing `:=`: the declared value is the block's value
            // (VM slot semantics). Return the local, boxed.
            let n = name.name.clone();
            self.decl_tail_value(&n, names, out)
                .unwrap_or_else(|| "zz_unit()".to_string())
        } else {
            "zz_unit()".to_string()
        };
        names.pop_scope();
        result
    }

    /// Return expression for a trailing-`Decl` tail: the declared local,
    /// boxed to a `zz_value` (scalars via constructors, refcounted via
    /// clone, unboxed structs via the object boxer). `None` when the
    /// local is unknown (caller falls back to unit).
    pub(super) fn decl_tail_value(
        &self,
        name: &str,
        names: &mut NameCtx,
        out: &mut String,
    ) -> Option<String> {
        let cid = names.lookup(name)?.to_string();
        let ctype = names.lookup_type(name).unwrap_or("zz_value").to_string();
        match ctype.as_str() {
            "int64_t" => Some(format!("zz_int({cid})")),
            "double" => Some(format!("zz_float({cid})")),
            "bool" => Some(format!("zz_bool({cid})")),
            t if t.starts_with("zz_struct_") => {
                let sname = match names.checker_types.get(name) {
                    Some(zz_checker::Type::Struct(s, _)) => s.clone(),
                    _ => return None,
                };
                let ident = Expr::Ident {
                    name: name.to_string(),
                    span: zz_frontend::span::Span::new(0, 0),
                };
                Some(self.emit_boxed_value(&sname, &ident, names, out))
            }
            _ => Some(format!("zz_clone({cid})")),
        }
    }

    pub(super) fn emit_expr(&self, e: &Expr, names: &mut NameCtx, out: &mut String) -> String {
        // Statement-direct flag: true when this expression is the
        // outermost value of a statement-level position (set by the
        // statement lowerer). Consumed here so only the direct child
        // observes it — nested expressions (tuple elements, call
        // arguments, block contents) always see false.
        let stmt_direct = self.stmt_direct.replace(false);
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
                        FmtPart::Expr(inner, spec) => {
                            let v = self.emit_expr(inner, names, out);
                            // Unboxed structs render through their generated
                            // `debug_string` (a raw C struct is not a
                            // `zz_value`). Format specs on structs keep the
                            // old path.
                            if spec.is_none() {
                                if let Some(sname) = self.unboxed_struct_of_expr(inner, names) {
                                    let s = self.stringify_struct_value(&sname, v, names, out);
                                    acc = format!("zz_binop_cat_str({acc}, {s})");
                                    continue;
                                }
                            }
                            let boxed_v = box_scalar_operand(inner, names, &v);
                            // If format spec is present, use zz_to_str_fmt
                            if let Some(ref s) = spec {
                                let spec_str =
                                    format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""));
                                acc = format!("zz_binop_cat({acc}, zz_str_owned(zz_to_str_fmt({boxed_v}, {spec_str})))");
                            } else {
                                acc = format!("zz_binop_cat_str({acc}, {boxed_v})");
                            }
                        }
                    }
                }
                acc
            }
            Expr::Ident { name, .. } => match names.lookup(name) {
                Some(cid) => {
                    // Check if the variable is a scalar type that can be used directly
                    if let Some(ctype) = names.lookup_type(name) {
                        // Moved-from temporaries (`x = f(x)` fast path):
                        // the slot was taken, so pass the temp through
                        // without a `zz_clone` bump.
                        if super::move_elide::is_moved_type(Some(ctype)) {
                            return cid.to_string();
                        }
                        match ctype {
                            "int64_t" | "double" | "bool" => cid.to_string(),
                            // Raw C structs are Copy (not refcounted) — pass
                            // through; cloning would also be a type error
                            // (zz_clone takes zz_value).
                            t if t.starts_with("zz_struct_") => cid.to_string(),
                            _ => format!("zz_clone({cid})"),
                        }
                    } else {
                        // Fallback to safe behavior if type unknown
                        format!("zz_clone({cid})")
                    }
                }
                None => {
                    // First-class reference to a named function
                    // (`f := add`): box the static function when it
                    // resolves (selective-import canonical first, then
                    // the bare spelling). Anything else unknown stays
                    // unit (the checker rejects it upstream).
                    if let Some(v) = self.static_func_value_ident(name) {
                        return v;
                    }
                    "zz_unit()".to_string()
                }
            },
            Expr::Path { parts, .. } => {
                // Handle struct field access (e.g., p.x or r.origin.x)
                if parts.len() == 2 {
                    if let Some(base_name) = names.lookup(&parts[0]) {
                        if let Some(base_type) = names.lookup_type(&parts[0]) {
                            // Check if base is a struct type
                            if self.is_struct_type_str(base_type) {
                                let field_name = &parts[1];
                                // Wrap scalar fields in the correct boxing
                                // so the result is always a `zz_value`.
                                // The `auto_box` idempotency guard prevents
                                // double-boxing when callers also call auto_box.
                                if let Some(field_c_type) =
                                    self.field_type_from_struct(base_type, field_name)
                                {
                                    let raw = format!("({base_name}).{field_name}");
                                    match field_c_type {
                                        "int64_t" => return format!("zz_int({raw})"),
                                        "double" => return format!("zz_float({raw})"),
                                        "bool" => return format!("zz_bool({raw})"),
                                        // Non-scalar field (boxed): clone.
                                        _ => return format!("zz_clone({raw})"),
                                    }
                                }
                                // Promoted field through an embedded struct
                                // (`u.id` → `(u).Base.id`).
                                if let Some(root) = self.unmangled_struct_name(base_type) {
                                    if let Some((chain, leaf)) =
                                        self.resolve_access_chain(&root, &parts[1..])
                                    {
                                        let mut raw = format!("({base_name})");
                                        for p in &chain {
                                            raw = format!("({raw}).{p}");
                                        }
                                        return auto_box(&raw, Some(leaf.as_str()));
                                    }
                                }
                                // Unknown field on a struct: fall through to
                                // the generic lookup below (the checker
                                // rejects unknown fields, so this is
                                // unreachable for valid programs).
                            } else if base_type == "zz_value" {
                                // Boxed object field access: u.name where u is a boxed struct
                                // Use runtime function to get the field value.
                                let field_name = &parts[1];
                                return format!(
                                    "zz_object_get_field(&{base_name}, \"{field_name}\")"
                                );
                            }
                        }
                    }
                }

                // Handle nested struct field access (e.g., r.origin.x = parts[0..2] + parts[2])
                if parts.len() >= 3 {
                    // Try to find the base variable
                    if let Some(base_name) = names.lookup(&parts[0]).map(str::to_string) {
                        if let Some(base_type) = names.lookup_type(&parts[0]).map(str::to_string) {
                            if self.is_struct_type_str(&base_type) {
                                // General promotion-aware resolution first:
                                // covers direct chains of any depth plus
                                // embedded hops (e.g. `o.Mid.Inner.x`).
                                if let Some(root) = self.unmangled_struct_name(&base_type) {
                                    if let Some((chain, leaf)) =
                                        self.resolve_access_chain(&root, &parts[1..])
                                    {
                                        let mut praw = format!("({base_name})");
                                        for p in &chain {
                                            praw = format!("({praw}).{p}");
                                        }
                                        return auto_box(&praw, Some(leaf.as_str()));
                                    }
                                }
                            } else if base_type == "zz_value" {
                                // Boxed base (boxed struct or dict): chain
                                // runtime field reads. Each level resolves
                                // embedded fields recursively in C, so both
                                // `u.Base.name` and promoted chains work.
                                let mut acc =
                                    format!("zz_object_get_field(&{base_name}, \"{}\")", parts[1]);
                                for part in &parts[2..] {
                                    let tmp = names.fresh("_field_obj");
                                    out.push_str(&format!("    zz_value {tmp} = {acc};\n"));
                                    acc = format!("zz_object_get_field(&{tmp}, \"{part}\")");
                                }
                                return acc;
                            }
                            // For now, only handle 2-level deep fields
                            if parts.len() == 3 && self.is_struct_type_str(&base_type) {
                                let field1 = &parts[1];
                                let field2 = &parts[2];
                                let raw = format!("(({base_name}).{field1}).{field2}");
                                // Walk the chain: base → field1 (struct) → field2 (scalar/struct)
                                // to derive the final field's C type for auto-boxing.
                                let field2_ctype =
                                    self.field_type_from_struct(&base_type, field1).and_then(
                                        |f1_type| self.field_type_from_struct(f1_type, field2),
                                    );
                                return auto_box(&raw, field2_ctype);
                            }
                        }
                    }
                }

                let joined = parts.join(".");
                if let Some(cid) = names.lookup(&joined) {
                    // Check if the variable is a scalar type that can be used directly
                    if let Some(ctype) = names.lookup_type(&joined) {
                        match ctype {
                            "int64_t" | "double" | "bool" => cid.to_string(),
                            _ if self.is_struct_type_str(ctype) => cid.to_string(),
                            _ => format!("zz_clone({cid})"),
                        }
                    } else {
                        // Fallback to safe behavior if type unknown
                        format!("zz_clone({cid})")
                    }
                } else {
                    // Math constants (`math.PI`, `std.math.PI`) lower to
                    // float literals — the checker rejects calls, so value
                    // position is the only valid use. Named function
                    // references (`f := path.join`) box the static
                    // function the same way. Anything else unknown stays
                    // unit (the checker rejects it upstream).
                    if let Some(v) = self.static_func_value_path(parts) {
                        return v;
                    }
                    super::math_const_c_literal(&joined).unwrap_or_else(|| "zz_unit()".to_string())
                }
            }
            Expr::Paren { expr, .. } => self.emit_expr(expr, names, out),
            Expr::Unary { op, expr, .. } => {
                let v = self.emit_expr(expr, names, out);
                match op {
                    // `zz_neg` / `zz_not` take `zz_value`: box raw scalars
                    // (e.g. a `bool`/`int` local lowers to its C type).
                    // `Pos` is identity — keep the raw form so scalar
                    // arithmetic fast-paths still recognize it.
                    zz_frontend::ast::UnOp::Neg => {
                        format!("zz_neg({})", box_scalar_operand(expr, names, &v))
                    }
                    zz_frontend::ast::UnOp::Pos => v,
                    zz_frontend::ast::UnOp::Not => {
                        format!("zz_not({})", box_scalar_operand(expr, names, &v))
                    }
                    zz_frontend::ast::UnOp::BitNot => {
                        format!("zz_bitnot({})", box_scalar_operand(expr, names, &v))
                    }
                }
            }
            Expr::Binary {
                op,
                left,
                right,
                span,
            } => {
                // Strength reduction: `x ** 2` → `x * x`, `x ** 3` →
                // `x * x * x`. The generic pow path boxes both operands
                // and routes through double-precision `dpow` — pure
                // overhead for small literal exponents in tight loops.
                // Only fires for duplication-safe bases (no side effects);
                // variable/complex exponents keep the `dpow` path.
                if matches!(op, zz_frontend::ast::BinOp::Pow) {
                    if let Expr::Int { value, .. } = right.as_ref() {
                        if (*value == 2 || *value == 3) && is_dup_safe(left) {
                            let mk_mul = |a: Expr, b: Expr| Expr::Binary {
                                op: zz_frontend::ast::BinOp::Mul,
                                left: Box::new(a),
                                right: Box::new(b),
                                span: *span,
                            };
                            let base = (**left).clone();
                            let reduced = if *value == 2 {
                                mk_mul(base.clone(), base)
                            } else {
                                mk_mul(base.clone(), mk_mul(base.clone(), base))
                            };
                            return self.emit_expr(&reduced, names, out);
                        }
                    }
                }
                let l = self.emit_expr(left, names, out);
                let r = self.emit_expr(right, names, out);
                match op {
                    zz_frontend::ast::BinOp::And => {
                        // `zz_truthy` takes `zz_value`: box raw-scalar
                        // operands (e.g. `bool` locals lower to C `bool`)
                        // and raw unboxed structs (e.g. `p && q`).
                        let raw_l = l.clone();
                        let raw_r = r.clone();
                        let l = box_scalar_operand(left, names, &l);
                        let l = self.box_struct_operand(left, l, &raw_l, names, out);
                        let r = box_scalar_operand(right, names, &r);
                        let r = self.box_struct_operand(right, r, &raw_r, names, out);
                        format!("zz_bool(zz_truthy({l}) && zz_truthy({r}))")
                    }
                    zz_frontend::ast::BinOp::Or => {
                        let raw_l = l.clone();
                        let raw_r = r.clone();
                        let l = box_scalar_operand(left, names, &l);
                        let l = self.box_struct_operand(left, l, &raw_l, names, out);
                        let r = box_scalar_operand(right, names, &r);
                        let r = self.box_struct_operand(right, r, &raw_r, names, out);
                        format!("zz_bool(zz_truthy({l}) || zz_truthy({r}))")
                    }
                    zz_frontend::ast::BinOp::Elvis => {
                        // Evaluate the left side once and store in a temp to avoid
                        // double-evaluation (which would call side-effecting natives
                        // like `input()` twice). Box: the temp is `zz_value`
                        // but a raw-scalar operand lowers to its C type.
                        let raw_l = l.clone();
                        let raw_r = r.clone();
                        let l = box_scalar_operand(left, names, &l);
                        let l = self.box_struct_operand(left, l, &raw_l, names, out);
                        let r = box_scalar_operand(right, names, &r);
                        let r = self.box_struct_operand(right, r, &raw_r, names, out);
                        let tmp = names.fresh("elvis");
                        out.push_str(&format!("    zz_value {tmp} = {l};\n"));
                        format!("zz_elvis({tmp}, {r})")
                    }
                    _ => {
                        let cop = binop_runtime_op(op);
                        // Check if either operand is a scalar. We treat both scalar-typed locals
                        // AND Int/Float literals as scalar operands so that patterns like
                        // `i + 1` or `count + n` unbox to raw C arithmetic instead of routing
                        // through the boxed `zz_binop` path.
                        let left_type = scalar_operand_type(left, names);
                        let right_type = scalar_operand_type(right, names);

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
                                | zz_frontend::ast::BinOp::BitAnd
                                | zz_frontend::ast::BinOp::BitOr
                                | zz_frontend::ast::BinOp::BitXor
                        );
                        // NOTE: `Shl`/`Shr` deliberately stay out of the raw
                        // path — raw C `<<` on signed overflow / shifts >=
                        // width is UB. They route through `zz_binop`
                        // (`ZZOP_SHL`/`ZZOP_SHR`) which masks `& 63` and
                        // rejects negative counts.
                        if is_arith {
                            // Arena string concatenation: when Add is used on
                            // strings inside a loop, allocate the result on
                            // the arena to avoid heap malloc per iteration.
                            if matches!(op, zz_frontend::ast::BinOp::Add)
                                && (self.is_string_expr(left, names)
                                    || self.is_string_expr(right, names))
                                && self.loop_arenas.borrow().last().is_some()
                            {
                                let arena = self.loop_arenas.borrow().last().unwrap().clone();
                                let boxed_l = box_scalar_operand(left, names, &l);
                                let boxed_r = box_scalar_operand(right, names, &r);
                                format!("zz_binop_cat_arena({boxed_l}, {boxed_r}, &{arena})")
                            } else if left_type == Some("int64_t") && right_type == Some("int64_t")
                            {
                                // Both sides are int64 — emit raw C arith.
                                let c_op = match op {
                                    zz_frontend::ast::BinOp::Add => "+",
                                    zz_frontend::ast::BinOp::Sub => "-",
                                    zz_frontend::ast::BinOp::Mul => "*",
                                    zz_frontend::ast::BinOp::Div => "/",
                                    zz_frontend::ast::BinOp::Rem => "%",
                                    zz_frontend::ast::BinOp::BitAnd => "&",
                                    zz_frontend::ast::BinOp::BitOr => "|",
                                    zz_frontend::ast::BinOp::BitXor => "^",
                                    _ => "+",
                                };
                                let lc = scalar_operand_c(left, names).unwrap_or_else(|| l.clone());
                                let rc =
                                    scalar_operand_c(right, names).unwrap_or_else(|| r.clone());
                                // Literal-zero divisor guard: a `*int` local
                                // can't be statically proven nonzero, but a
                                // literal zero would divide-by-zero at -O3.
                                if matches!(
                                    op,
                                    zz_frontend::ast::BinOp::Div | zz_frontend::ast::BinOp::Rem
                                ) && matches!(right.as_ref(), Expr::Int { value: 0, .. })
                                {
                                    format!("zz_binop({cop}, {l}, {r})")
                                } else {
                                    format!("(int64_t)({lc} {c_op} {rc})")
                                }
                            } else if left_type == Some("double") && right_type == Some("double") {
                                let c_op = match op {
                                    zz_frontend::ast::BinOp::Add => "+",
                                    zz_frontend::ast::BinOp::Sub => "-",
                                    zz_frontend::ast::BinOp::Mul => "*",
                                    zz_frontend::ast::BinOp::Div => "/",
                                    _ => "+",
                                };
                                let lc = scalar_operand_c(left, names).unwrap_or_else(|| l.clone());
                                let rc =
                                    scalar_operand_c(right, names).unwrap_or_else(|| r.clone());
                                if matches!(op, zz_frontend::ast::BinOp::Rem) {
                                    format!("(double)(fmod({l}, {r}))")
                                } else {
                                    format!("(double)({lc} {c_op} {rc})")
                                }
                            } else if (left_type == Some("int64_t")
                                || right_type == Some("int64_t"))
                                || (left_type == Some("double") || right_type == Some("double"))
                            {
                                // Mixed: one scalar, one boxed. Box both
                                // sides and dispatch through zz_binop.
                                // Struct operands (never scalar-typed) box
                                // from raw structs to runtime objects here.
                                let boxed_l = box_scalar_operand(left, names, &l);
                                let boxed_l =
                                    self.box_struct_operand(left, boxed_l, &l, names, out);
                                let boxed_r = box_scalar_operand(right, names, &r);
                                let boxed_r =
                                    self.box_struct_operand(right, boxed_r, &r, names, out);
                                format!("zz_binop({cop}, {boxed_l}, {boxed_r})")
                            } else {
                                // Neither operand has a known scalar type in
                                // NameCtx, but struct field accesses like
                                // `(v0).width` are raw C scalars that need
                                // boxing for `zz_binop`. Use `box_scalar_operand`
                                // which recognizes the cast pattern. Raw
                                // unboxed structs box to objects here.
                                let boxed_l = box_scalar_operand(left, names, &l);
                                let boxed_l =
                                    self.box_struct_operand(left, boxed_l, &l, names, out);
                                let boxed_r = box_scalar_operand(right, names, &r);
                                let boxed_r =
                                    self.box_struct_operand(right, boxed_r, &r, names, out);
                                format!("zz_binop({cop}, {boxed_l}, {boxed_r})")
                            }
                        } else {
                            // Comparisons / pow / etc: use the boxed path
                            // (result must be zz_value). Also handles
                            // struct field accesses that emit as raw C
                            // scalars, plus raw unboxed structs (e.g.
                            // `Pt{...} == p`, `p == q`) which box to
                            // runtime objects for `zz_binop`.
                            let boxed_l = box_scalar_operand(left, names, &l);
                            let boxed_l = self.box_struct_operand(left, boxed_l, &l, names, out);
                            let boxed_r = box_scalar_operand(right, names, &r);
                            let boxed_r = self.box_struct_operand(right, boxed_r, &r, names, out);
                            format!("zz_binop({cop}, {boxed_l}, {boxed_r})")
                        }
                    }
                }
            }
            Expr::Call {
                callee,
                args,
                named,
                ..
            } => self.emit_call(callee, args, named, names, out, stmt_direct),
            Expr::While {
                cond, body, span, ..
            } => {
                // Loops whose body contains non-escaping allocations get a
                // loop-scoped sub-arena, exactly like `for` loops: the buffer
                // is reset at the end of every iteration so per-iteration
                // allocations are reused instead of growing the heap.
                // Skipped in green closures (stack arenas cannot survive
                // a suspend; heap-only there).
                let loop_arena: Option<String> =
                    if !self.green_active() && self.escape.loop_spans.contains(span) {
                        let ac = names.bump_counter();
                        let name = format!("_loop_arena{ac}");
                        let alloc_count = count_allocating_exprs(body);
                        let arena_size = (alloc_count * 128).max(65536);
                        out.push_str(&format!("    zz_arena {name};\n"));
                        out.push_str(&format!("    zz_arena_init(&{name}, {arena_size});\n"));
                        Some(name)
                    } else {
                        None
                    };
                if let Some(ref name) = loop_arena {
                    self.loop_arenas.borrow_mut().push(name.clone());
                    *self.current_loop_arena.borrow_mut() = Some(name.clone());
                }
                out.push_str("    while (1) {\n");
                // Cooperative safepoint at the loop top (mirrors the VM's
                // `Op::Safepoint`): budget-guarded `zz_safepoint()` yields
                // the OS thread on quantum expiry so sibling AOT task
                // threads get scheduled.
                out.push_str("        zz_safepoint();\n");
                let c = self.emit_expr(cond, names, out);
                let c = box_scalar_operand(cond, names, &c);
                out.push_str(&format!("        if (!zz_truthy({c})) break;\n"));
                // Lexical scope for the body (same push/pop_scope
                // discipline as for-loop bodies and value blocks):
                // shadowing declarations must not leak past the braces.
                // The marker additionally releases body-declared heap
                // locals at the bottom of every iteration.
                let while_scope = self.loop_scope_begin(names);
                // Loop body is never a function tail.
                for bstmt in &body.stmts {
                    self.emit_stmt(bstmt, names, out, false);
                }
                self.loop_scope_end(names, out, while_scope);
                if let Some(ref name) = loop_arena {
                    self.loop_arenas.borrow_mut().pop();
                    *self.current_loop_arena.borrow_mut() =
                        self.loop_arenas.borrow().last().cloned();
                    out.push_str(&format!("        zz_arena_reset(&{name});\n"));
                }
                out.push_str("    }\n");
                if let Some(ref name) = loop_arena {
                    out.push_str(&format!("    zz_arena_destroy(&{name});\n"));
                }
                "zz_unit()".to_string()
            }
            Expr::If {
                cond, then, els, ..
            } => {
                // Value-position if: every branch assigns its tail value
                // into a shared temp (statement position discards it).
                // Chained `else if` arrives as a bare If — recurse so
                // nested branches emit instead of collapsing to unit.
                let c = self.emit_expr(cond, names, out);
                let c = box_scalar_operand(cond, names, &c);
                let tmp = names.fresh("_ifv");
                out.push_str(&format!("    zz_value {tmp} = zz_unit();\n"));
                out.push_str(&format!("    if (zz_truthy({c})) {{\n"));
                let tv = self.emit_block_value(then, names, out);
                out.push_str(&format!("        {tmp} = {tv};\n"));
                if let Some(el) = els {
                    out.push_str("    } else {\n");
                    let ev = match el.as_ref() {
                        Expr::Block(b) => self.emit_block_value(b, names, out),
                        other => self.emit_expr(other, names, out),
                    };
                    out.push_str(&format!("        {tmp} = {ev};\n"));
                    out.push_str("    }\n");
                } else {
                    out.push_str("    }\n");
                }
                tmp
            }
            Expr::Block(b) => {
                // Value-position block: evaluate statements, yield the tail
                // (see emit_block_value; statement-position emit_block
                // truncates `__tail` so inner temps never leak there).
                self.emit_block_value(b, names, out)
            }
            Expr::Range { start, end, .. } => {
                let s = self.emit_expr(start, names, out);
                let en = self.emit_expr(end, names, out);
                format!("zz_range_build({s}, {en})")
            }
            Expr::Index { obj, index, .. } => {
                // `obj[idx]` — runtime-dispatched read (arrays/dicts).
                // Two fast-paths over the naive
                // `zz_call_native2(zz_index_get, zz_clone(o), boxed_i)`:
                //   1. Plain Ident receivers pass borrowed: zz_index_get
                //      never releases or stores its object argument, so
                //      the atomic retain per load is pure overhead.
                //   2. Direct `zz_index_get` call (now static inline in
                //      collections.h) instead of the native-call shim.
                // Error behavior is unchanged: the shim ignored *err, and
                // the temp err here is likewise unread.
                let o = match obj.as_ref() {
                    Expr::Ident { name, .. } => names
                        .lookup(name)
                        .map(|s| s.to_string())
                        .unwrap_or_else(|| self.emit_expr(obj, names, out)),
                    _ => self.emit_expr(obj, names, out),
                };
                let i = self.emit_expr(index, names, out);
                // Box a scalar index (ident/raw-arith) to a zz_value.
                let i_boxed = self.box_index_arg(index, i, names);
                let e = names.fresh("_idxe");
                out.push_str(&format!("    int {e} = 0;\n"));
                format!("zz_index_get({o}, {i_boxed}, &{e})")
            }
            Expr::Slice {
                obj, start, end, ..
            } => {
                // `obj[a:b]` — array/string slicing. Missing bounds lower to
                // unit (the C runtime interprets unit as "from 0" / "to end").
                // Scalar bounds (int idents, raw arithmetic) must be boxed
                // like index args, or C rejects int64_t as zz_value.
                let o = self.emit_expr(obj, names, out);
                let s = match start {
                    Some(e) => {
                        let emitted = self.emit_expr(e, names, out);
                        self.box_index_arg(e, emitted, names)
                    }
                    None => "zz_unit()".to_string(),
                };
                let e = match end {
                    Some(e) => {
                        let emitted = self.emit_expr(e, names, out);
                        self.box_index_arg(e, emitted, names)
                    }
                    None => "zz_unit()".to_string(),
                };
                format!("zz_call_native3(zz_slice_value, {o}, {s}, {e})")
            }
            Expr::StructInit { name, fields, .. } => {
                self.emit_struct_init(name, fields, names, out)
            }
            Expr::Field { obj, name, .. } => {
                // Determine if the object is boxed (zz_value) or unboxed (C struct).
                // For unboxed: emit C struct field access (e.g., "obj.field").
                // For boxed: emit zz_object_get_field(&obj, "field").
                let obj_val = self.emit_expr(obj, names, out);
                // Try to determine if the object type is an unboxed struct.
                // Index receivers (`arr[i].field`) are always boxed
                // `zz_value`s at runtime — array elements (including DB
                // row dicts standing in for structs) never inhabit raw
                // C structs — so they must use the runtime accessor even
                // when the checker type is an unboxed struct. Call results
                // are also always boxed: every function returns `zz_value`
                // (struct returns are boxed via `emit_boxed_value`), so
                // `make_pt().x` and `id_pt(q).x` must use the runtime
                // accessor, never raw `(call).field`.
                let is_unboxed = if matches!(obj.as_ref(), Expr::Index { .. }) {
                    false
                } else if let Expr::Ident { name: obj_name, .. } = obj.as_ref() {
                    names
                        .lookup_type(obj_name)
                        .map(|t| t.starts_with("zz_struct_"))
                        .unwrap_or(false)
                } else if let Expr::StructInit { name: sname, .. } = obj.as_ref() {
                    // A struct literal of unboxed type lowers to a raw C
                    // struct value, so `Pt{...}.x` can use direct access.
                    self.is_unboxed_struct(sname)
                } else if matches!(
                    obj.as_ref(),
                    Expr::Field { .. } | Expr::Path { .. } | Expr::Paren { .. }
                ) {
                    // Field/Path chains lower raw only when rooted at a raw
                    // unboxed value (an unboxed local or literal). A chain
                    // rooted at a call/index (`make_pt().a.b`) is boxed at
                    // the first step, so every outer step stays boxed even
                    // though the checker type is still a struct.
                    let root_is_raw = {
                        let mut cur = obj.as_ref();
                        loop {
                            match cur {
                                Expr::Paren { expr, .. } => cur = expr.as_ref(),
                                Expr::Field { obj: inner, .. } => cur = inner.as_ref(),
                                Expr::Path { parts, .. } => {
                                    break parts
                                        .first()
                                        .and_then(|b| names.lookup_type(b))
                                        .map(|t| t.starts_with("zz_struct_"))
                                        .unwrap_or(false);
                                }
                                Expr::Ident { name: b, .. } => {
                                    break names
                                        .lookup_type(b)
                                        .map(|t| t.starts_with("zz_struct_"))
                                        .unwrap_or(false);
                                }
                                Expr::StructInit { name: s, .. } => {
                                    break self.is_unboxed_struct(s);
                                }
                                _ => break false,
                            }
                        }
                    };
                    if root_is_raw {
                        if let Some(zz_checker::Type::Struct(sname, _)) =
                            self.ty_at(names, obj.span())
                        {
                            self.is_unboxed_struct(sname)
                        } else {
                            false
                        }
                    } else {
                        false
                    }
                } else {
                    // Calls, arrays, dicts, blocks, if/match, etc. all lower
                    // to boxed `zz_value` — never raw structs.
                    false
                };
                if is_unboxed {
                    // Unboxed struct: direct C field access, then auto-box
                    // scalar fields so the result is always a zz_value.
                    // Derive the field C type from the parent object's struct type.
                    let field_ctype = self.ty_at(names, obj.span()).and_then(|ot| {
                        if let zz_checker::Type::Struct(sname, _) = ot {
                            if let Some(sig) = self.tp.structs.get(sname) {
                                if let Some((_, ft)) = sig.fields.iter().find(|(n, _)| n == name) {
                                    return Some(self.type_to_c(ft));
                                }
                            }
                            // Promoted field through an embedded struct.
                            if let Some((chain, _)) =
                                self.resolve_access_chain(sname, std::slice::from_ref(name))
                            {
                                if let Some(leaf) = self.promoted_leaf_ctype(sname, &chain) {
                                    return Some(leaf);
                                }
                            }
                        }
                        None
                    });
                    // The raw access must follow the embedded chain when the
                    // field is promoted (`m().id` → `(tmp).Base.id`).
                    let raw = self
                        .ty_at(names, obj.span())
                        .and_then(|ot| match ot {
                            zz_checker::Type::Struct(sname, _) => self
                                .resolve_access_chain(sname, std::slice::from_ref(name))
                                .map(|(chain, _)| {
                                    let mut acc = format!("({obj_val})");
                                    for p in &chain {
                                        acc = format!("({acc}).{p}");
                                    }
                                    acc
                                }),
                            _ => None,
                        })
                        .unwrap_or_else(|| format!("({obj_val}).{name}"));
                    auto_box(&raw, field_ctype.as_deref())
                } else {
                    // Boxed object: use runtime function.
                    // `zz_object_get_field` takes a pointer, so we need
                    // an lvalue.  A simple Ident usually produces a C
                    // variable name (lvalue), but emission can also yield
                    // an rvalue for one (e.g. `zz_clone(v0)`) — so check
                    // the emitted form, not just the AST shape. Anything
                    // else (Index, Call, Field chain, …) is an rvalue —
                    // hoist to a temp.
                    if matches!(obj.as_ref(), Expr::Ident { .. }) && is_simple_ident(&obj_val) {
                        format!("zz_object_get_field(&{obj_val}, \"{name}\")")
                    } else {
                        let tmp = names.fresh("_field_obj");
                        out.push_str(&format!("    zz_value {tmp} = {obj_val};\n"));
                        format!("zz_object_get_field(&{tmp}, \"{name}\")")
                    }
                }
            }
            Expr::Array { elems, span, .. } => {
                // Fast path: stack-promote when the literal is provably
                // non-escaping AND every element is a scalar that lowers
                // to a raw C scalar expression. This eliminates both the
                // header bump-alloc and the per-iteration `realloc` of
                // the items buffer, achieving Rust-level throughput on
                // tight array-literal-in-loop benchmarks.
                if let Some(v) = self.try_emit_stack_array(elems, *span, names, out) {
                    return v;
                }
                // Fallback: arena-aware or heap-allocated construction.
                let arr_var = format!("__arr{}", names.counter);
                names.counter += 1;
                // Literal-literal path: pre-allocate the exact capacity —
                // `zz_array_new_lit` bump-allocates the header AND the items
                // buffer on the arena (or one heap block when escaping), so
                // element stores never realloc and never touch atomic
                // refcounts. Only applicable when every element lowers to a
                // plain scalar (int/float/bool): scalars carry no refcount,
                // so the clone-free direct store is perfectly safe.
                let mut scalar_inits: Vec<String> = Vec::with_capacity(elems.len());
                let mut all_scalar = true;
                for item in elems {
                    let mut scratch = String::new();
                    match self.emit_scalar_init(item, names, &mut scratch) {
                        Some(init) if scratch.is_empty() => scalar_inits.push(init),
                        _ => {
                            all_scalar = false;
                            break;
                        }
                    }
                }
                if all_scalar {
                    let arena_arg = match self.arena_for(*span) {
                        Some(arena) => format!("&{arena}"),
                        None => "NULL".to_string(),
                    };
                    out.push_str(&format!(
                        "    zz_value {arr_var} = zz_array_new_lit({arena_arg}, {});\n",
                        elems.len()
                    ));
                    for init in &scalar_inits {
                        // init = `{.tag=ZZ_INT, {.i=42}}`; reuse its innards
                        // inside the compound-literal wrapper.
                        let inner = &init[1..init.len() - 1];
                        out.push_str(&format!(
                            "    zz_array_push_lit({arr_var}.arr, (zz_value){{{inner}}});\n"
                        ));
                    }
                    return arr_var;
                }
                // Non-scalar fallback: force arena allocation inside a loop body with
                // an arena reset or for a provably non-escaping literal. This
                // is safe because the per-iteration reset frees the object
                // before any function call could observe it. Function calls
                // that receive arena-allocated objects must not retain them.
                let ctor = match self.arena_for(*span) {
                    Some(arena) => format!("zz_array_new_arena_sized(&{arena}, {})", elems.len()),
                    None => "zz_array_new()".to_string(),
                };
                out.push_str(&format!("    zz_value {arr_var} = {ctor};\n"));
                for item in elems {
                    self.append_container_item(&arr_var, item, names, out);
                }
                arr_var
            }
            Expr::Tuple { items, .. } => {
                // Tuples are represented as arrays. The empty tuple `()` lowers
                // to an empty array so `stringify(())` is `[]` (matches VM).
                let arr_var = names.fresh("__tup");
                let ctor = if items.is_empty() {
                    "zz_array_new()".to_string()
                } else {
                    match self.arena_for(zz_frontend::span::Span::new(0, 0)) {
                        Some(arena) => {
                            format!("zz_array_new_arena_sized(&{arena}, {})", items.len())
                        }
                        None => "zz_array_new()".to_string(),
                    }
                };
                out.push_str(&format!("    zz_value {arr_var} = {ctor};\n"));
                if !items.is_empty() {
                    for item in items {
                        self.append_container_item(&arr_var, item, names, out);
                    }
                }
                arr_var
            }
            Expr::Dict { entries, span, .. } => {
                // Create a temp to hold the dict, populate entries, return the temp.
                let dv = names.fresh("__dict");
                let n = entries.len();
                let arena_code = match self.arena_for(*span) {
                    Some(arena) => format!("zz_dict_new_arena_sized(&{arena}, {n})"),
                    // Heap path is also pre-sized: avoids the calloc +
                    // realloc cascade when the literal escapes the arena.
                    None => format!("zz_dict_new_sized({n})"),
                };
                out.push_str(&format!("    zz_value {dv} = {arena_code};\n"));
                for (k, v) in entries {
                    let raw_key = self.emit_expr(k, names, out);
                    let raw_val = self.emit_expr(v, names, out);
                    // Box raw-scalar keys/values (loop vars, arithmetic)
                    // so every `zz_index_set` argument is a real zz_value.
                    let key = box_scalar_operand(k, names, &raw_key);
                    let val = box_scalar_operand(v, names, &raw_val);
                    out.push_str(&format!(
                        "    {{ int _de = 0; zz_index_set({dv}, {key}, {val}, &_de); }}\n"
                    ));
                }
                dv
            }
            #[allow(unreachable_patterns)]
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
                        zz_frontend::ast::FmtPart::Expr(expr, spec) => {
                            let val = self.emit_expr(expr, names, out);
                            // Unboxed structs render through their generated
                            // `debug_string`. (`zz_to_str` needs a `zz_value`.)
                            let val = if spec.is_none() {
                                if let Some(sname) = self.unboxed_struct_of_expr(expr, names) {
                                    self.stringify_struct_value(&sname, val, names, out)
                                } else {
                                    val
                                }
                            } else {
                                val
                            };
                            if let Some(ref s) = spec {
                                let spec_str =
                                    format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""));
                                out.push_str(&format!("    {{ zz_value _r = zz_str_owned(zz_to_str_fmt({val}, {spec_str})); {fvar} = zz_binop_cat({fvar}, _r); }}\n"));
                            } else {
                                out.push_str(&format!("    {{ zz_value _r = zz_to_str({val}, &(int){{0}}); {fvar} = zz_binop_cat({fvar}, _r); }}\n"));
                            }
                        }
                    }
                }
                fvar
            }
            Expr::Variant { name, arg, .. } => {
                // `.ok(x)`, `.err(e)`, `.some(x)`, `.none`
                // Payloads arrive as `zz_value`: box raw-scalar args
                // (unboxed int/float/bool locals, literals, scalar
                // arithmetic) so `zz_variant_ok(v1)` never receives a
                // bare `int64_t`.
                match name.as_str() {
                    "some" => {
                        if let Some(a) = arg {
                            let inner = self.emit_expr(a, names, out);
                            let boxed = box_scalar_operand(a, names, &inner);
                            format!("zz_variant_some({boxed})")
                        } else {
                            "(zz_value){ZZ_OPTION_NONE, {0}}".to_string()
                        }
                    }
                    "none" => "(zz_value){ZZ_OPTION_NONE, {0}}".to_string(),
                    "ok" => {
                        if let Some(a) = arg {
                            let inner = self.emit_expr(a, names, out);
                            let boxed = box_scalar_operand(a, names, &inner);
                            format!("zz_variant_ok({boxed})")
                        } else {
                            "(zz_value){ZZ_RESULT_OK, {0}}".to_string()
                        }
                    }
                    "err" => {
                        if let Some(a) = arg {
                            let inner = self.emit_expr(a, names, out);
                            let boxed = box_scalar_operand(a, names, &inner);
                            format!("zz_variant_err({boxed})")
                        } else {
                            "(zz_value){ZZ_RESULT_ERR, {0}}".to_string()
                        }
                    }
                    _ => "zz_unit()".to_string(),
                }
            }
            Expr::IfLet {
                pat,
                value,
                then,
                els,
                ..
            } => {
                // Desugar `if let pat = val { body } else { alt }`
                // into `match val { pat => body, _ => alt }`
                let wildcard_body = match els {
                    Some(e) => (**e).clone(),
                    None => Expr::Bool {
                        value: false,
                        span: zz_frontend::span::Span::default(),
                    },
                };
                let arms = vec![
                    MatchArm {
                        pat: pat.clone(),
                        guard: None,
                        body: Expr::Block(then.clone()),
                        span: zz_frontend::span::Span::default(),
                    },
                    MatchArm {
                        pat: Pattern::Wildcard {
                            span: zz_frontend::span::Span::default(),
                        },
                        guard: None,
                        body: wildcard_body,
                        span: zz_frontend::span::Span::default(),
                    },
                ];
                self.emit_match(value, &arms, names, out)
            }
            Expr::Match {
                scrutinee, arms, ..
            } => self.emit_match(scrutinee, arms, names, out),
            Expr::ListComp {
                body,
                var,
                iter,
                filter,
                ..
            } => {
                let comp = names.fresh("__comp");
                out.push_str(&format!("    zz_value {comp} = zz_array_new();\n"));
                // Static bounds when the iterable is `range(a, b)` or `a..b`.
                let bounds: Option<(String, String)> = match iter.as_ref() {
                    Expr::Call { callee, args, .. } => {
                        let is_range = matches!(
                            callee.as_ref(),
                            Expr::Ident { name, .. } if name == "range"
                        );
                        if is_range && args.len() == 2 {
                            let a = self.emit_expr(&args[0], names, out);
                            let a = box_scalar_operand(&args[0], names, &a);
                            let b = self.emit_expr(&args[1], names, out);
                            let b = box_scalar_operand(&args[1], names, &b);
                            Some((a, b))
                        } else {
                            None
                        }
                    }
                    Expr::Range { start, end, .. } => {
                        let a = self.emit_expr(start, names, out);
                        let a = box_scalar_operand(start, names, &a);
                        let b = self.emit_expr(end, names, out);
                        let b = box_scalar_operand(end, names, &b);
                        Some((a, b))
                    }
                    _ => None,
                };
                if let Some((a, b)) = bounds {
                    out.push_str(&format!(
                        "    for (int64_t __ci = ({a}).i; __ci < ({b}).i; __ci++) {{\n"
                    ));
                    // Lexical scope for the comprehension variable
                    // (shadowing must not leak past the braces).
                    names.push_scope();
                    let xcid = names.enter(&var.name);
                    out.push_str(&format!("        zz_value {xcid} = zz_int(__ci);\n"));
                    let mut body_scratch = String::new();
                    let bv = self.emit_expr(body, names, &mut body_scratch);
                    let bv = box_scalar_operand(body, names, &bv);
                    if let Some(c) = filter {
                        let mut cond_scratch = String::new();
                        let cv = self.emit_expr(c, names, &mut cond_scratch);
                        let cv = box_scalar_operand(c, names, &cv);
                        out.push_str(&format!("        if (zz_truthy({cv})) {{\n"));
                        out.push_str(&cond_scratch);
                        out.push_str(&body_scratch);
                        out.push_str(&format!(
                            "            {{ int _e = 0; zz_vec_append({comp}, {bv}, &_e); }}\n"
                        ));
                        out.push_str("        }\n");
                    } else {
                        out.push_str(&body_scratch);
                        out.push_str(&format!(
                            "        {{ int _e = 0; zz_vec_append({comp}, {bv}, &_e); }}\n"
                        ));
                    }
                    out.push_str("    }\n");
                    names.pop_scope();
                } else {
                    out.push_str("    // unsupported comprehension iterable\n");
                }
                comp
            }
            Expr::Break { .. } => {
                out.push_str("    break;\n");
                "zz_unit()".to_string()
            }
            Expr::Continue { .. } => {
                out.push_str("    continue;\n");
                "zz_unit()".to_string()
            }
            Expr::Closure { params, body, .. } => {
                let (fname, cap_arr, kind_arr, size_arr, n, green) =
                    self.emit_closure_parts(params, body, names, out);
                if n == 0 {
                    if green {
                        format!("zz_closure_make_green({fname})")
                    } else {
                        format!("zz_closure_make({fname})")
                    }
                } else {
                    format!(
                        "zz_closure_make_ex_typed{green}({fname}, {cap_arr}, {kind_arr}, {size_arr}, {n})",
                        green = if green { "_green" } else { "" },
                    )
                }
            }
            Expr::Try { expr: inner, span } => self.emit_try(inner, *span, names, out),
        }
    }

    /// Lower a closure literal's body definition + capture arrays, returning
    /// the pieces a construction call needs:
    /// (fn name, cap array, kind array, size array, ncaps, green).
    /// Array exprs are `"NULL"` when the closure captures nothing (no arrays
    /// emitted). Shared by the general `make` path and the `task.spawn`
    /// fast path (`zz_spawn_ex`, which skips the intermediate rep).
    pub(super) fn emit_closure_parts(
        &self,
        params: &[Param],
        body: &Expr,
        names: &mut NameCtx,
        out: &mut String,
    ) -> (String, String, String, String, usize, bool) {
        // Allocate the id up front (not from `closure_defs.len()`):
        // lowering the body re-enters closure lowering for nested literals
        // while no borrow is held, so ids stay unique.
        let cid = {
            let n = self.closure_seq.get();
            self.closure_seq.set(n + 1);
            n
        };
        // Free variables bound outside the closure become shared
        // heap cells in the environment (match VM by-reference
        // capture). Names resolving to nothing here (plain function
        // names, namespaces) are skipped.
        let param_names: Vec<String> = params.iter().map(|p| p.name.name.clone()).collect();
        let globals = self.global_name_set();
        let mut caps: Vec<(String, String, String, Option<zz_checker::Type>)> = Vec::new();
        for fv in zz_hir::closure_free_vars(&param_names, body, &globals) {
            let Some(ptr) = names.cell_ptr(&fv) else {
                continue;
            };
            let ctype = names.lookup_type(&fv).unwrap_or("zz_value").to_string();
            let checker = names.checker_types.get(&fv).cloned();
            caps.push((fv, ptr, ctype, checker));
        }
        let body_c = self.emit_closure(params, body, cid, &caps, names, out);
        self.closure_defs.borrow_mut().push(body_c);
        // Emit a forward declaration so the closure is visible to
        // call sites that appear before the closure definition.
        self.closure_forward_decls.borrow_mut().push(format!(
            "static zz_value zz_closure_{cid}(zz_value *args, size_t argc, void **env, size_t nenv);\n"
        ));
        // Green closures (B3 suspendable frames): eligible bodies
        // lower to a state machine and suspend instead of parking.
        let green = super::green::closure_green_eligible(self, body);
        let fname = format!("zz_closure_{cid}");
        if caps.is_empty() {
            (
                fname,
                "NULL".to_string(),
                "NULL".to_string(),
                "NULL".to_string(),
                0,
                green,
            )
        } else {
            let cap_arr = names.fresh("_cap");
            let ptrs: Vec<String> = caps.iter().map(|(_, p, _, _)| p.clone()).collect();
            out.push_str(&format!(
                "    void *{cap_arr}[] = {{{}}};\n",
                ptrs.join(", ")
            ));
            // Cell layout for the rep: boxed `zz_value` cells
            // deep-copy on spawn; anything else (unboxed int /
            // double / bool / struct cells) copies byte-wise.
            // `zz_value_dup` reads this — without it every cell
            // is misread as `zz_value*` (spawn segfault class).
            let kind_arr = names.fresh("_capkind");
            let kinds: Vec<String> = caps
                .iter()
                .map(|(_, _, ctype, _)| if ctype == "zz_value" { "0" } else { "1" }.to_string())
                .collect();
            out.push_str(&format!(
                "    unsigned char {kind_arr}[] = {{{}}};\n",
                kinds.join(", ")
            ));
            let size_arr = names.fresh("_capsz");
            let sizes: Vec<String> = caps
                .iter()
                .map(|(_, _, ctype, _)| format!("sizeof({ctype})"))
                .collect();
            out.push_str(&format!(
                "    size_t {size_arr}[] = {{{}}};\n",
                sizes.join(", ")
            ));
            (fname, cap_arr, kind_arr, size_arr, caps.len(), green)
        }
    }

    /// Emit a C static function for a closure literal `|p1, p2| body` and
    /// return the closure's C body text. Param values arrive boxed in `args[]`;
    /// captured variables arrive as shared cells in `env[]` (see the creation
    /// site, which passes the same `caps` layout). The body expression is
    /// lowered against params + captures and returned.
    pub(super) fn emit_closure(
        &self,
        params: &[Param],
        body: &Expr,
        cid: usize,
        caps: &[(String, String, String, Option<zz_checker::Type>)],
        outer_names: &mut NameCtx,
        _out: &mut String,
    ) -> String {
        // Green transform (B3): suspendable bodies lower every local to a
        // task-frame cell and split at blocking calls into resume labels.
        let green = super::green::closure_green_eligible(self, body);
        if green {
            self.green.borrow_mut().replace(GreenCtx::new());
        }
        // Loop arenas cannot survive a suspend (stack buffers): force heap
        // allocation throughout green bodies. Save/restore the outer state
        // (closures may be created inside outer loop bodies).
        let saved_arena = self.current_loop_arena.borrow().clone();
        if green {
            *self.current_loop_arena.borrow_mut() = None;
        }
        let scope = outer_names.current_scope.clone();
        let mut o = self.emit_closure_inner(params, body, cid, caps, green, &scope);
        if green {
            self.green_finish(&mut o);
            *self.current_loop_arena.borrow_mut() = saved_arena;
        }
        o
    }

    fn emit_closure_inner(
        &self,
        params: &[Param],
        body: &Expr,
        cid: usize,
        caps: &[(String, String, String, Option<zz_checker::Type>)],
        green: bool,
        scope: &str,
    ) -> String {
        let mut names = NameCtx::new();
        // Closure bodies check under the enclosing function: inherit its
        // scope so typed lookups hit the right entries.
        names.current_scope = scope.to_string();
        self.seed_globals(&mut names);
        for (i, (name, _, ctype, checker)) in caps.iter().enumerate() {
            let ptr = format!("env[{i}]");
            let deref = NameCtx::cap_deref_of(&ptr, ctype);
            names.insert_capture(name, &ptr, &deref, ctype, checker.clone());
        }
        // Bindings of THIS body captured by a deeper closure literal.
        {
            let param_names: Vec<String> = params.iter().map(|p| p.name.name.clone()).collect();
            names.capture_set = self.expr_capture_set(&param_names, body);
        }
        let mut o = String::new();
        o.push_str(&format!(
            "static zz_value zz_closure_{cid}(zz_value *args, size_t argc, void **env, size_t nenv) {{\n"
        ));
        o.push_str("    (void)argc;\n");
        o.push_str("    (void)nenv;\n");
        if caps.is_empty() {
            o.push_str("    (void)env;\n");
        }
        if green {
            // Suspendable prologue: fetch the task frame (trampoline-set)
            // or stand up a stack-backed sync frame (sync calls: NULL
            // TLS). The cleanup attribute frees sync cell contents at
            // every exit; task frames are freed by the trampoline on
            // completion and survive suspend-returns. `_gcell` pointers
            // restore from frame slots (a resume jumps over their
            // allocation sites), then resume dispatches to the yield
            // label. Placeholders expand in `green_finish`.
            o.push_str("    zz_task_frame *zz_fr = zz_green_frame();\n");
            o.push_str(
                "    __attribute__((cleanup(zz_sync_frame_cleanup))) zz_task_frame zz_sync_fr;\n",
            );
            o.push_str("    void *_sync_cells[/*GREEN_NSLOTS*/];\n");
            o.push_str("    unsigned char _sync_kind[/*GREEN_NSLOTS*/];\n");
            o.push_str("    size_t _sync_size[/*GREEN_NSLOTS*/];\n");
            o.push_str("    zz_task_frame_init(&zz_sync_fr);\n");
            o.push_str("    if (!zz_fr) {\n");
            o.push_str("        memset(_sync_cells, 0, sizeof(_sync_cells));\n");
            o.push_str("        memset(_sync_kind, 0, sizeof(_sync_kind));\n");
            o.push_str("        memset(_sync_size, 0, sizeof(_sync_size));\n");
            o.push_str("        zz_sync_fr.cells = _sync_cells;\n");
            o.push_str("        zz_sync_fr.cell_kind = _sync_kind;\n");
            o.push_str("        zz_sync_fr.cell_size = _sync_size;\n");
            o.push_str("        zz_sync_fr.ncells = /*GREEN_NSLOTS*/;\n");
            o.push_str("        zz_fr = &zz_sync_fr;\n");
            o.push_str("    }\n");
            o.push_str("    if (!zz_fr->cells) {\n");
            o.push_str("        zz_fr->cells = (void**)calloc(/*GREEN_NSLOTS*/, sizeof(void*));\n");
            o.push_str("        zz_fr->cell_kind = (unsigned char*)calloc(/*GREEN_NSLOTS*/, 1);\n");
            o.push_str(
                "        zz_fr->cell_size = (size_t*)calloc(/*GREEN_NSLOTS*/, sizeof(size_t));\n",
            );
            o.push_str("        if (!zz_fr->cells || !zz_fr->cell_kind || !zz_fr->cell_size) { fprintf(stderr, \"zz: out of memory (frame cells)\\n\"); exit(1); }\n");
            o.push_str("        zz_fr->ncells = /*GREEN_NSLOTS*/;\n");
            o.push_str("        zz_fr->owns_cells = 1;\n");
            o.push_str("    }\n");
            o.push_str("/*GREEN_DECLS*/");
            o.push_str("/*GREEN_RESTORE*/");
            o.push_str("    if (zz_fr->resume != 0) {\n");
            o.push_str("        switch (zz_fr->resume) {\n");
            o.push_str("/*GREEN_DISPATCH*/");
            o.push_str("        default: break;\n");
            o.push_str("        }\n");
            o.push_str("        return zz_unit();\n");
            o.push_str("    }\n");
        } else {
            // No function arena: nothing lowers allocations into it
            // (loop bodies use their own sub-arenas, everything else is
            // heap). A per-call 64KB init here used to leak on every
            // closure invocation (epilogue unreachable past `return`).
        }
        o.push_str("    int __defers[32];\n");
        o.push_str("    int __defer_n = 0;\n");
        for (i, p) in params.iter().enumerate() {
            if green {
                // Params must survive a suspend: frame cells, assigned
                // from the re-passed `args[]` on first entry (resume
                // skips straight to the yield label).
                let (ptr, deref, n) = self.green_cell(&mut names, "zz_value", true, &mut o);
                o.push_str(&format!("    {deref} = args[{i}];\n"));
                names.enter_cell(&p.name.name, &ptr, &deref, "zz_value", n);
                continue;
            }
            if names.capture_set.contains(&p.name.name) {
                // Captured param: heap cell shared with nested closures.
                let n = names.bump_counter();
                let ptr = format!("_cell{n}");
                let deref = NameCtx::owner_deref(&ptr);
                o.push_str(&format!(
                    "    zz_value *{ptr} = (zz_value*)malloc(sizeof(zz_value));\n"
                ));
                o.push_str(&format!("    {deref} = args[{i}];\n"));
                names.enter_cell(&p.name.name, &ptr, &deref, "zz_value", n);
            } else {
                let cid_enter = names.enter(&p.name.name);
                o.push_str(&format!("    zz_value {cid_enter} = args[{i}];\n"));
            }
        }
        let mut body_out = String::new();
        // Block bodies use function-style tail handling so a trailing value
        // expression (e.g. `{ count = count + d; count }`) becomes the return
        // value instead of unit. Bare-expression bodies evaluate inline.
        let block_body: Option<zz_frontend::ast::Block> = match body {
            Expr::Block(b) => Some(b.clone()),
            _ => None,
        };
        let val = match &block_body {
            Some(b) => {
                self.emit_func_block(b, &mut names, &mut body_out);
                o.push_str(&body_out);
                // Defer runner lands before the tail returns below.
                String::new()
            }
            None => {
                // A bare body that IS a suspendable call (`|_| recv(c)`)
                // is a statement-level yield: suspend instead of parking.
                if green {
                    self.stmt_direct.set(true);
                }
                let v = self.emit_expr(body, &mut names, &mut body_out);
                o.push_str(&body_out);
                box_scalar_operand(body, &names, &v)
            }
        };
        {
            let mut slots = self.defer_slots.borrow_mut();
            if !slots.is_empty() {
                o.push_str("    for (int __dk = __defer_n - 1; __dk >= 0; __dk--) {\n");
                o.push_str("        switch (__defers[__dk]) {\n");
                let snap: Vec<String> = std::mem::take(&mut *slots);
                for (idx, snippet) in snap.iter().enumerate() {
                    o.push_str(&format!("        case {idx}:\n"));
                    o.push_str(snippet);
                    o.push_str("\n            break;\n");
                }
                o.push_str("        default: break;\n");
                o.push_str("        }\n");
                o.push_str("    }\n");
            }
        }
        if let Some(b) = &block_body {
            if self.last_stmt_value(b, &mut names, &mut o).is_none() {
                o.push_str("    return zz_unit();\n");
            }
        } else {
            o.push_str(&format!("    return {val};\n"));
        }
        o.push_str("}\n");
        o
    }

    /// Lower `try inner` / `inner?`: unwrap Option/Result or early-return.
    ///
    /// Emission strategy (zero-overhead, mirrors the VM `TryOp` logic):
    /// the inner value is hoisted into a `zz_value` temp; on the error
    /// arm we `return` the converted error, otherwise the expression
    /// evaluates to the unwrapped payload (`zz_match_some` / `zz_match_ok`).
    ///
    /// Conversion (`try_resolutions` with different source/target error
    /// types) calls the checker's `convert_to_` impl as a direct C call:
    /// impl methods go through the struct-pointer convention
    /// (`zz_fn_T__convert_to_U(&err, NULL, 0)`), plain functions through
    /// the args-array convention.
    pub(super) fn emit_try(
        &self,
        inner: &Expr,
        span: zz_frontend::span::Span,
        names: &mut NameCtx,
        out: &mut String,
    ) -> String {
        let inner_val = self.emit_expr(inner, names, out);
        let inner_boxed = box_scalar_operand(inner, names, &inner_val);
        let tmp = names.fresh("_try");
        out.push_str(&format!("    zz_value {tmp} = {inner_boxed};\n"));

        // Conversion target for this `try` site (`None` = identity).
        let convert: Option<String> = self.tp.try_converts.get(&span).cloned().unwrap_or(None);

        // Emit the early-return for a `Result` error payload held in `tmp`.
        let emit_result_err_return = |out: &mut String, names: &mut NameCtx| {
            if let Some(fname) = convert.clone() {
                let err_tmp = names.fresh("_try_err");
                let conv_tmp = names.fresh("_try_conv");
                let cf = format!("zz_fn_{}", mangle(&fname));
                let is_impl = self
                    .tp
                    .funcs
                    .get(&fname)
                    .and_then(|sig| sig.params.first().map(|(_, t)| t.clone()))
                    .map(|t| matches!(&t, zz_checker::Type::Struct(_, _)))
                    .unwrap_or(false);
                out.push_str(&format!(
                    "        zz_value {err_tmp} = zz_match_err({tmp});\n"
                ));
                if is_impl {
                    out.push_str(&format!(
                        "        zz_value {conv_tmp} = {cf}(&{err_tmp}, NULL, 0);\n"
                    ));
                } else {
                    out.push_str(&format!(
                        "        zz_value {conv_tmp} = {cf}((zz_value[]){{ {err_tmp} }}, 1);\n"
                    ));
                }
                out.push_str(&format!("        return zz_variant_err({conv_tmp});\n"));
            } else {
                out.push_str(&format!("        return {tmp};\n"));
            }
        };

        match self.ty_at(names, inner.span()) {
            Some(zz_checker::Type::Option(_)) => {
                out.push_str(&format!("    if ({tmp}.tag == ZZ_OPTION_NONE) {{\n"));
                out.push_str("        return (zz_value){ZZ_OPTION_NONE, {0}};\n");
                out.push_str("    }\n");
                format!("zz_match_some({tmp})")
            }
            Some(zz_checker::Type::Result(_, _)) => {
                out.push_str(&format!("    if ({tmp}.tag == ZZ_RESULT_ERR) {{\n"));
                emit_result_err_return(out, names);
                out.push_str("    }\n");
                format!("zz_match_ok({tmp})")
            }
            // Unresolved/dynamic operand: guard both tags. The error arm
            // applies the conversion when one was resolved; otherwise the
            // value is already the correct error variant to return as-is.
            // The happy path dispatches on the runtime tag into a result
            // temp (the checker rejects `?` on uninferred types, so this
            // arm is defensive-only).
            _ => {
                out.push_str(&format!("    if ({tmp}.tag == ZZ_RESULT_ERR) {{\n"));
                emit_result_err_return(out, names);
                out.push_str("    }\n");
                out.push_str(&format!("    if ({tmp}.tag == ZZ_OPTION_NONE) {{\n"));
                out.push_str("        return (zz_value){ZZ_OPTION_NONE, {0}};\n");
                out.push_str("    }\n");
                let res_tmp = names.fresh("_try_ok");
                out.push_str(&format!("    zz_value {res_tmp};\n"));
                out.push_str(&format!(
                    "    if ({tmp}.tag == ZZ_OPTION_SOME) {{ {res_tmp} = zz_match_some({tmp}); }} else {{ {res_tmp} = zz_match_ok({tmp}); }}\n"
                ));
                res_tmp
            }
        }
    }

    pub(super) fn emit_call(
        &self,
        callee: &Expr,
        args: &[Expr],
        named: &[(String, Expr)],
        names: &mut NameCtx,
        out: &mut String,
        stmt_direct: bool,
    ) -> String {
        // `stmt_direct` arrives from `emit_expr` (single funnel above):
        // only a suspendable call sitting directly in statement position
        // qualifies for the green yield sequence; nested calls observed
        // `false` and keep the blocking path.
        // Resolve callee name — handle method dispatch for Path/Field expressions.
        // Returns (cname, method_receiver) where method_receiver is the owned Expr
        // to insert as the first argument for method calls like `x.push(4)`.
        let (cname, method_receiver): (String, Option<Expr>) = match callee {
            Expr::Ident { name, .. } => {
                // Selective-import aliases (`rts` from
                // `import std.fs(read_to_string as rts)`) resolve to their
                // canonical native — but never shadow a real local binding.
                let resolved = if names.lookup(name).is_none() {
                    self.import_fn_aliases
                        .get(name)
                        .cloned()
                        .unwrap_or_else(|| name.clone())
                } else {
                    name.clone()
                };
                (resolved, None)
            }
            Expr::Path { parts, span, .. } if parts.len() == 2 => {
                // Module-namespace collision: the loader qualifies same-file
                // calls (`greet_user` in `server.zz` -> `server.greet_user`),
                // which collides with a local of the same name as the file
                // stem (`server := http.server()`). An exact function match
                // is a direct call, never a method on the local.
                let joined = parts.join(".");
                if self.reachable_funcs.contains(&joined) || self.tp.funcs.contains_key(&joined) {
                    // Namespace-alias copies (`f.write` from
                    // `import std.fs as f`) match here because the loader
                    // registers them in funcs — but they have no native of
                    // their own, so the call would lower to a bodyless
                    // `zz_fn_f__write` stub (silent no-op). Resolve to the
                    // canonical name (`std.fs.write`) when only it has a
                    // native impl. User-function aliases are untouched
                    // (neither spelling has a native), as are shadowed
                    // locals (exact-match rule above still wins for them).
                    let obj_name = &parts[0];
                    let method = &parts[1];
                    let cname = if names.lookup(obj_name).is_none() {
                        match self.import_ns_aliases.get(obj_name) {
                            Some(head) => {
                                let resolved = format!("{head}.{method}");
                                // Bodies live under canonical names (seed
                                // copies under the alias have none): when
                                // the alias spelling has neither a native
                                // impl nor a definition but a canonical
                                // spelling does, call the canonical one.
                                // The `std.`-stripped form covers stdlib
                                // definitions (`std.path.join` is defined
                                // as `path.join`).
                                let mut cands = vec![resolved.clone()];
                                if let Some(stripped) = resolved.strip_prefix("std.") {
                                    cands.push(stripped.to_string());
                                }
                                let joined_ok = native_supported(&joined)
                                    || self.find_func_def(&joined).is_some();
                                if !joined_ok {
                                    if let Some(hit) = cands.into_iter().find(|c| {
                                        native_supported(c) || self.find_func_def(c).is_some()
                                    }) {
                                        hit
                                    } else {
                                        joined
                                    }
                                } else {
                                    joined
                                }
                            }
                            None => joined,
                        }
                    } else {
                        joined
                    };
                    (cname, None)
                } else {
                    let obj_name = &parts[0];
                    let method = &parts[1];
                    if names.lookup(obj_name).is_some() {
                        // obj_name is a LOCAL variable — this is a method call.
                        let first_ident_end = span.start + obj_name.len() as u32;
                        let first_ident_span =
                            zz_frontend::span::Span::new(span.start, first_ident_end);

                        // Struct method dispatch: if the local's C type is a
                        // struct (e.g. `zz_struct_mod__Rectangle`), look up
                        // `<StructType>.<method>` in `reachable_funcs` (impl
                        // methods are stored as `Type.method` using the
                        // un-mangled struct name like `mod.Rectangle`). This
                        // handles `rect.area()` regardless of whether `rect`
                        // is bare or module-prefixed. Boxed structs (C type
                        // `zz_value`, e.g. containing strings) resolve through
                        // the checker's type map instead; embedded promotion
                        // (`u.area()` → `Base.area`) applies to both shapes.
                        let struct_dispatch: Option<(String, Expr)> = self
                            .dispatch_struct_name(
                                names,
                                names.lookup_type(obj_name),
                                obj_name,
                                first_ident_span,
                            )
                            .and_then(|unmangled| {
                                self.struct_method_target(&unmangled, method).map(
                                    |(impl_name, path)| {
                                        let recv = if path.is_empty() {
                                            Expr::Ident {
                                                name: obj_name.clone(),
                                                span: first_ident_span,
                                            }
                                        } else {
                                            let mut parts = vec![obj_name.clone()];
                                            parts.extend(path);
                                            Expr::Path {
                                                parts,
                                                span: first_ident_span,
                                            }
                                        };
                                        (impl_name, recv)
                                    },
                                )
                            });
                        if let Some((c, r)) = struct_dispatch {
                            (c, Some(r))
                        } else {
                            // Use the receiver's type from the type checker to
                            // select the correct namespace. Without this, the
                            // generic loop picks "vec" before "str" for methods
                            // like `.contains()` that exist on multiple types.
                            let mut found_ns = "";
                            // Try type-based dispatch: check NameCtx's checker_types
                            // (populated at Decl) for the receiver variable's resolved
                            // type, then map to the matching namespace.
                            let recv_type_ns =
                                names.checker_types.get(obj_name).and_then(|ty| match ty {
                                    zz_checker::Type::Str => Some("str"),
                                    zz_checker::Type::Array(_) => Some("vec"),
                                    zz_checker::Type::Dict(_, _) => Some("dict"),
                                    zz_checker::Type::Option(_) => Some("option"),
                                    zz_checker::Type::Result(_, _) => Some("result"),
                                    zz_checker::Type::Db => Some("sqlz"),
                                    zz_checker::Type::TcpStream | zz_checker::Type::TcpListener => {
                                        Some("net")
                                    }
                                    zz_checker::Type::HttpServer
                                    | zz_checker::Type::Response
                                    | zz_checker::Type::HttpRequest => Some("http"),
                                    zz_checker::Type::Json => Some("json"),
                                    zz_checker::Type::Bytes => Some("bytes"),
                                    zz_checker::Type::Chan => Some("chan"),
                                    // Opaque handles dispatch on their module tag.
                                    // Tags are dynamic, so leak once per tag —
                                    // same pattern as struct namespaces in the VM.
                                    zz_checker::Type::Opaque(tag) => {
                                        Some(Box::leak(tag.clone().into_boxed_str()) as &str)
                                    }
                                    _ => None,
                                });
                            // Also check the type checker's span_types map
                            // using the receiver's source span.
                            let span_type_ns =
                                if let Some(zzty) = self.ty_at(names, first_ident_span) {
                                    match zzty {
                                        zz_checker::Type::Str => Some("str"),
                                        zz_checker::Type::Array(_) => Some("vec"),
                                        zz_checker::Type::Dict(_, _) => Some("dict"),
                                        zz_checker::Type::Option(_) => Some("option"),
                                        zz_checker::Type::Result(_, _) => Some("result"),
                                        zz_checker::Type::Db => Some("sqlz"),
                                        zz_checker::Type::TcpStream
                                        | zz_checker::Type::TcpListener => Some("net"),
                                        zz_checker::Type::HttpServer
                                        | zz_checker::Type::Response
                                        | zz_checker::Type::HttpRequest => Some("http"),
                                        zz_checker::Type::Json => Some("json"),
                                        zz_checker::Type::Bytes => Some("bytes"),
                                        zz_checker::Type::Chan => Some("chan"),
                                        zz_checker::Type::Opaque(tag) => {
                                            Some(Box::leak(tag.clone().into_boxed_str()) as &str)
                                        }
                                        _ => None,
                                    }
                                } else {
                                    None
                                };
                            let type_ns = recv_type_ns.or(span_type_ns).unwrap_or("");
                            if !type_ns.is_empty() {
                                let candidate = format!("{type_ns}.{method}");
                                let std_candidate = format!("std.{type_ns}.{method}");
                                if self.reachable_natives.contains(&candidate)
                                    || self.reachable_natives.contains(&std_candidate)
                                    || native_supported(&candidate)
                                {
                                    found_ns = type_ns;
                                }
                            }
                            // Fallback: generic namespace search (untyped
                            // receivers, e.g. variables without type annotations).
                            if found_ns.is_empty() {
                                let namespaces = [
                                    "vec", "str", "dict", "option", "result", "http", "sqlz", "db",
                                    "file",
                                ];
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
                            }
                            if found_ns.is_empty() {
                                // Reachable-based dynamic scan FIRST (FFI-module
                                // namespaces like regexp, uuid, file, … plus any
                                // embedded namespace): any reachable
                                // `<ns>.<method>` wins, sorted for determinism.
                                // This must precede the `native_supported`
                                // fallback below: global support without
                                // reachability misroutes (e.g. `f.close()` on a
                                // file handle would pick `sqlz.close`, which is
                                // always "supported" but not reachable here).
                                let suffix = format!(".{method}");
                                let mut cands: Vec<&str> = self
                                    .reachable_natives
                                    .iter()
                                    .filter_map(|n| {
                                        n.strip_suffix(suffix.as_str())
                                            .map(|ns| ns.strip_prefix("std.").unwrap_or(ns))
                                    })
                                    .collect();
                                cands.sort_unstable();
                                cands.dedup();
                                if let Some(ns) = cands.into_iter().next() {
                                    found_ns = ns;
                                }
                                // Last resort: match by native_impl — checks if
                                // there's a C runtime function registered for
                                // this method under any namespace, even when
                                // reachability missed it.
                                if found_ns.is_empty() {
                                    let namespaces = [
                                        "vec", "str", "dict", "option", "result", "http", "sqlz",
                                        "db", "file",
                                    ];
                                    for ns in &namespaces {
                                        let candidate = format!("{ns}.{method}");
                                        if native_supported(&candidate) {
                                            found_ns = ns;
                                            break;
                                        }
                                    }
                                }
                                if found_ns.is_empty() {
                                    // No namespace resolved — the fallthrough
                                    // below emits a bare call and lets later
                                    // stages (checker/runtime) report it.
                                }
                            }
                            if found_ns.is_empty() {
                                // Free-function / bare-native method syntax
                                // (mirrors the VM's `lookup_method_recv`
                                // bare-callable-first rule and the checker's
                                // Path-method `funcs.get(method)` branch):
                                // `p.bump()` where `bump` is a free function
                                // resolves to `bump(p)` — the receiver becomes
                                // the first argument. Without this the call
                                // lowers to `bump()` with the receiver dropped
                                // (arity mismatch: the callee reads `args[0]`
                                // out of bounds — Bug 8's garbage/hang/segfault).
                                //
                                // Name resolution has three shapes because the
                                // loader namespaces top-level items by file
                                // stem (`bump` → `m1.bump`) while the method
                                // tail of a true receiver call stays bare
                                // (locals shadow the namespace rewrite):
                                //   1. bare `method` (harness/REPL programs),
                                //   2. `{struct-ns}.{method}` (same-module
                                //      free function — mirrors the checker's
                                //      `Struct(sname)` → `{ns}.{method}`
                                //      fallback),
                                //   3. unique `*.{method}` suffix (mirrors the
                                //      callgraph candidates + the bare-Ident
                                //      `owned_fallback` below).
                                let receiver = Expr::Ident {
                                    name: obj_name.clone(),
                                    span: first_ident_span,
                                };
                                let mut target: Option<String> = None;
                                if self.reachable_funcs.contains(method)
                                    || self.tp.funcs.contains_key(method)
                                {
                                    target = Some(method.clone());
                                }
                                if target.is_none() {
                                    if let Some(unmangled) = self.dispatch_struct_name(
                                        names,
                                        names.lookup_type(obj_name),
                                        obj_name,
                                        first_ident_span,
                                    ) {
                                        if let Some((ns, _)) = unmangled.rsplit_once('.') {
                                            let cand = format!("{ns}.{method}");
                                            if self.reachable_funcs.contains(&cand)
                                                || self.tp.funcs.contains_key(&cand)
                                            {
                                                target = Some(cand);
                                            }
                                        }
                                    }
                                }
                                if target.is_none() {
                                    let suffix = format!(".{method}");
                                    let mut hits: Vec<&String> = self
                                        .reachable_funcs
                                        .iter()
                                        .filter(|f| f.ends_with(&suffix))
                                        .collect();
                                    for k in self.tp.funcs.keys() {
                                        if k.ends_with(&suffix) && !hits.contains(&k) {
                                            hits.push(k);
                                        }
                                    }
                                    hits.sort_unstable();
                                    hits.dedup();
                                    if hits.len() == 1 {
                                        target = Some(hits[0].clone());
                                    }
                                }
                                if target.is_none()
                                    && (self.reachable_natives.contains(method)
                                        || native_supported(method))
                                {
                                    // Bare native with an untyped receiver
                                    // (`xs.len()` missing type info → `len(xs)`).
                                    target = Some(method.clone());
                                }
                                match target {
                                    Some(t) => (t, Some(receiver)),
                                    None => {
                                        // Unknown — fall through to unit below.
                                        (method.clone(), None)
                                    }
                                }
                            } else {
                                let receiver = Expr::Ident {
                                    name: obj_name.clone(),
                                    span: first_ident_span,
                                };
                                (format!("{found_ns}.{method}"), Some(receiver))
                            }
                        }
                    } else {
                        // obj_name is NOT a local — it's a namespace like `vec`, `io`,
                        // or a module head alias (`f` from `import std.fs as f`).
                        let head = self
                            .import_ns_aliases
                            .get(obj_name)
                            .cloned()
                            .unwrap_or_else(|| obj_name.clone());
                        (format!("{head}.{method}"), None)
                    }
                }
            }
            Expr::Path { parts, .. } => {
                // Multi-segment Path call. Try to resolve as a method
                // dispatch on a struct/typed receiver. Examples that hit
                // this branch:
                //   - `mod.rect.area()` — receiver is the namespaced local
                //     `mod.rect`; the AOT walks the path from the end
                //     (longest matching local first) to find the local
                //     and uses its struct type to look up
                //     `<StructType>.<method>` in funcs.
                if parts.len() >= 2 {
                    // Same collision rule as the 2-part arm above: an exact
                    // user-function match is a direct call even when a local
                    // shares the head segment.
                    let joined = parts.join(".");
                    if self.reachable_funcs.contains(&joined) || self.tp.funcs.contains_key(&joined)
                    {
                        (joined, None)
                    } else if let Some((recv_cname, recv_expr)) =
                        self.resolve_path_receiver(parts, names)
                    {
                        (recv_cname, Some(recv_expr))
                    } else if names.lookup(&parts[0]).is_some() {
                        // Head is a local but no impl target matched
                        // (`o.inner.bump()` with free `bump`): the tail is
                        // a free-function call on the head chain. Resolve
                        // it and keep the chain as receiver instead of
                        // emitting a bogus `o.inner.bump` direct call
                        // (which lowers to unit / reads OOB).
                        let method = parts.last().cloned().unwrap_or_default();
                        let recv_expr = Expr::Path {
                            parts: parts[..parts.len() - 1].to_vec(),
                            span: callee.span(),
                        };
                        match self.field_free_target(&method, None) {
                            Some(t) => (t, Some(recv_expr)),
                            None => {
                                let mut fixed = parts.clone();
                                if let Some(head) = self.import_ns_aliases.get(&parts[0]) {
                                    fixed[0] = head.clone();
                                }
                                (fixed.join("."), None)
                            }
                        }
                    } else {
                        // Module head aliases (`pg` from
                        // `import std.sqlz.postgres as pg`) rewrite the head.
                        let mut fixed = parts.clone();
                        if let Some(head) = self.import_ns_aliases.get(&parts[0]) {
                            fixed[0] = head.clone();
                        }
                        (fixed.join("."), None)
                    }
                } else {
                    (parts.join("."), None)
                }
            }
            Expr::Field {
                obj, name: method, ..
            } => {
                // Expr::Field callee — rare since parser consumes ident chains as Path.
                // Look up receiver type and dispatch.
                if let Some(zzty) = self.ty_at(names, obj.span()) {
                    match zzty {
                        zz_checker::Type::Struct(sname, _) => {
                            // Impl methods keep the direct-form convention
                            // (struct-pointer receiver); promotion included
                            // for embedded methods. The receiver is kept so
                            // the impl call site can pass `&recv` — including
                            // non-Ident receivers like `arr[0]` (boxed at
                            // runtime, unboxed into a temp at the call site).
                            if let Some((target, _)) = self.struct_method_target(sname, method) {
                                (target, Some(*obj.clone()))
                            } else {
                                // Free function on a non-Ident receiver
                                // (`origin().bump()`, `arr[0].bump()`,
                                // `Pt{..}.bump()`): resolve and KEEP the
                                // receiver as first arg. Dropping it makes
                                // the callee read args[0] out of bounds
                                // (silent zeroed structs/segfaults).
                                let receiver = *obj.clone();
                                match self.field_free_target(method, Some(sname.as_str())) {
                                    Some(t) => (t, Some(receiver)),
                                    None => (method.clone(), None),
                                }
                            }
                        }
                        // Canonical `sqlz.*`; `db.*` alias resolves to the
                        // same runtime fn via native_impl.
                        zz_checker::Type::Db => (format!("sqlz.{method}"), Some(*obj.clone())),
                        _ => {
                            let ns: &str = match zzty {
                                zz_checker::Type::Array(_) => "vec",
                                zz_checker::Type::Str => "str",
                                zz_checker::Type::Dict(_, _) => "dict",
                                zz_checker::Type::Option(_) => "option",
                                zz_checker::Type::Result(_, _) => "result",
                                zz_checker::Type::Opaque(tag) => {
                                    Box::leak(tag.clone().into_boxed_str()) as &str
                                }
                                _ => "",
                            };
                            if !ns.is_empty() {
                                (format!("{ns}.{method}"), Some(*obj.clone()))
                            } else {
                                // Unknown-type receiver (e.g. a span the
                                // checker never recorded): still resolve a
                                // free function and keep the receiver —
                                // mirrors the VM's bare-callable rule.
                                let receiver = *obj.clone();
                                match self.field_free_target(method, None) {
                                    Some(t) => (t, Some(receiver)),
                                    None => (method.clone(), None),
                                }
                            }
                        }
                    }
                } else {
                    // No recorded receiver type at all: same free-function
                    // fallback with the receiver kept.
                    let receiver = *obj.clone();
                    match self.field_free_target(method, None) {
                        Some(t) => (t, Some(receiver)),
                        None => (method.clone(), None),
                    }
                }
            }
            _ => return "zz_unit()".to_string(),
        };

        // Peephole: `len(x)` where `x` is a variable bound to an array
        // literal folds to the literal's arity (straight-line code only).
        // This bypasses the runtime native call AND the boxed arithmetic,
        // so `sum = sum + len(arr)` lowers to `sum += 3` — matching what
        // Rust does for `[i, i+1, i+2].len()`. The literal construction is
        // still emitted (side effects preserved); pure scalar stores are
        // dead-code-eliminated by the C compiler.
        if args.len() == 1 {
            let is_len = matches!(
                cname.as_str(),
                "len" | "vec.len" | "std.vec.len" | "std.len"
            );
            if is_len {
                match &args[0] {
                    // `len([a(), 1])`: emit the literal construction so
                    // element side effects run; only the arity is constant.
                    Expr::Array { elems, .. } => {
                        let _ = self.emit_expr(&args[0], names, out);
                        return format!("zz_int({})", elems.len());
                    }
                    // `len(arr)`: reading a variable is side-effect free.
                    Expr::Ident { name, .. } => {
                        if let Some(n) = names.array_len(name.as_str()) {
                            return format!("zz_int({n})");
                        }
                    }
                    _ => {}
                }
            }
        }
        // Any other call may mutate or retain its array arguments — drop
        // literal-length knowledge for every variable passed in (including
        // the method receiver, e.g. `arr.push(4)`).
        if let Some(Expr::Ident { name, .. }) = &method_receiver {
            names.invalidate_array_len(name);
        }
        for a in args {
            match a {
                Expr::Ident { name, .. } => names.invalidate_array_len(name),
                Expr::Path { parts, .. } => {
                    let joined = parts.join(".");
                    names.invalidate_array_len(&joined);
                }
                _ => {}
            }
        }

        // Build the ordered argument list, handling named args by reordering
        // them to match the function's parameter positions (mirrors the VM's
        // compile_reordered_args). When named args are present, we look up
        // the FuncSig and reorder all args accordingly.
        let ordered_args: Vec<&Expr> = {
            // Look up the function signature for slot-based arg ordering.
            // This handles both named args AND default values.
            let sig_info = self.tp.funcs.get(&cname);
            let has_named_or_defaults =
                !named.is_empty() || sig_info.is_some_and(|s| s.has_default.iter().any(|&d| d));
            if has_named_or_defaults {
                if let Some(sig) = sig_info {
                    let n = sig.params.len();
                    let mut slots: Vec<Option<&Expr>> = vec![None; n];
                    // Fill positional args by index
                    for (i, arg) in args.iter().enumerate() {
                        if i < n {
                            slots[i] = Some(arg);
                        }
                    }
                    // Fill named args by param name
                    for (name, val) in named {
                        if let Some(idx) = sig.params.iter().position(|(pn, _)| pn == name) {
                            if slots[idx].is_none() {
                                slots[idx] = Some(val);
                            }
                        }
                    }
                    // Fill in default values for any remaining empty slots.
                    if sig.has_default.iter().any(|&d| d) {
                        if let Some(func_def) = self.find_func_def(&cname) {
                            for (i, slot) in slots.iter_mut().enumerate() {
                                if slot.is_none() {
                                    if let Some(param) = func_def.get(i) {
                                        if let Some(ref default_val) = param.default {
                                            *slot = Some(default_val.as_ref());
                                        }
                                    }
                                }
                            }
                        }
                    }
                    // Collect non-None slots in order
                    slots.into_iter().flatten().collect()
                } else {
                    args.iter().chain(named.iter().map(|(_, v)| v)).collect()
                }
            } else {
                args.iter().collect()
            }
        };

        // Plugin `extern "C"` call: direct C invocation with unboxed
        // scalars, bypassing the zz_value calling convention. Only for
        // bare-name calls (externs have no receivers); anything
        // unsupported degrades to `zz_unit()` inside.
        if method_receiver.is_none() {
            if let Some(sig) = self.tp.funcs.get(&cname).cloned() {
                if sig.is_extern {
                    return self.emit_extern_call(&cname, &sig, &ordered_args, names, out);
                }
            }
        }

        let mut arg_items: Vec<String> = Vec::new();
        // `len(s.f)` without the getter's retain (see `zz_len_field`):
        // runs before arg emission so the field is never cloned.
        if cname == "len"
            && method_receiver.is_none()
            && named.is_empty()
            && ordered_args.len() == 1
        {
            if let Some(len_c) = self.try_emit_len_field(ordered_args[0], names) {
                return len_c;
            }
        }
        // sqlz early-out: sqlz.query/sqlz.exec (+ db.* alias, + pg.* free
        // form) lower via emit_db_call, which needs the raw Exprs (Fmt
        // split into template + binds). Runs BEFORE the generic
        // receiver/arg loops below, which would otherwise concatenate
        // the Fmt into a single string.
        // NOTE: ordered_args holds ONLY user args ([sql] in method form);
        // the receiver is separate in method_receiver. Free-form static
        // calls (`sqlz.query(db, sql)`, `pg.query(db, sql)`) carry the
        // handle as the first user arg — emit_db_call splits it off.
        // Both are passed explicitly.
        if matches!(
            cname.as_str(),
            "sqlz.query"
                | "std.sqlz.query"
                | "db.query"
                | "std.db.query"
                | "pg.query"
                | "std.sqlz.postgres.query"
        ) || matches!(
            cname.as_str(),
            "sqlz.exec"
                | "std.sqlz.exec"
                | "db.exec"
                | "std.db.exec"
                | "pg.exec"
                | "std.sqlz.postgres.exec"
        ) {
            return self.emit_db_call(&cname, method_receiver.as_ref(), ordered_args, names, out);
        }
        // ── sqlz.transaction / db.transaction inlining ────────────────────
        // The closure-based transaction cannot go through the normal native
        // path (closures have no C function pointer).  Instead we inline the
        // closure body between BEGIN / COMMIT / ROLLBACK directly, binding
        // the closure parameter (typically `tx`) to the same db handle.
        if matches!(
            cname.as_str(),
            "sqlz.transaction" | "std.sqlz.transaction" | "db.transaction" | "std.db.transaction"
        ) {
            if let Some(Expr::Closure { params, body, .. }) = args.last() {
                // Emit the db handle — for the method form
                // `db.transaction(|tx| {...})` the receiver IS the db;
                // for the static form `sqlz.transaction(db, |tx| {...})`
                // it's the first user arg.
                let db_val = if let Some(ref recv) = method_receiver {
                    self.emit_expr(recv, names, out)
                } else if let Some(first) = ordered_args.first() {
                    self.emit_expr(first, names, out)
                } else {
                    return "zz_unit()".to_string();
                };
                // Bind the closure parameter (e.g. `tx`) to the db handle.
                if let Some(param) = params.first() {
                    let param_c = names.enter(&param.name.name);
                    out.push_str(&format!("    zz_value {param_c} = {db_val};\n"));
                }
                // BEGIN.
                out.push_str(&format!(
                    "    zz_tx_reset_error();\n\
                     {{ int _txerr = 0; \
                     zz_db_exec_raw({db_val}, \"BEGIN\", NULL, 0, &_txerr); }}\n"
                ));
                // Inline the closure body.  Use emit_func_block for Block
                // bodies so that __tail is preserved (emit_block truncates it).
                let mut body_out = String::new();
                let body_val = if let Expr::Block(b) = body.as_ref() {
                    self.emit_func_block(b, names, &mut body_out);
                    if let Some((tmp, _)) = names.stack.get("__tail").and_then(|s| s.last()) {
                        tmp.clone()
                    } else {
                        // Leaf tail (string literal, ident, etc.) — emit_expr
                        // was skipped by emit_func_block; evaluate it directly.
                        if let Some(Stmt::Expr(e)) = b.stmts.last() {
                            let v = self.emit_expr(e, names, &mut body_out);
                            box_scalar_operand(e, names, &v)
                        } else {
                            "zz_unit()".to_string()
                        }
                    }
                } else {
                    self.emit_expr(body, names, &mut body_out)
                };
                out.push_str(&body_out);
                let body_val = box_scalar_operand(body, names, &body_val);
                // COMMIT or ROLLBACK based on the error flag.
                let tx_result = names.fresh("_tx_result");
                out.push_str(&format!(
                    "    zz_value {tx_result};\n\
                     if (zz_tx_has_error()) {{\n\
                     {{ int _txerr = 0; \
                     zz_db_exec_raw({db_val}, \"ROLLBACK\", NULL, 0, &_txerr); }}\n\
                     {tx_result} = zz_variant_err(zz_str_static(\"transaction failed\"));\n\
                     }} else {{\n\
                     {{ int _txerr = 0; \
                     zz_db_exec_raw({db_val}, \"COMMIT\", NULL, 0, &_txerr); }}\n\
                     {tx_result} = zz_variant_ok({body_val});\n\
                     }}\n"
                ));
                return tx_result;
            }
            // Fallback: not a closure argument — let it fall through to zz_unit().
        }
        // ── task.spawn closure-literal fast path ─────────────────────────
        // `task.spawn(|...| ...)` builds the worker-owned closure rep
        // directly from the capture arrays (`zz_spawn_ex`), skipping the
        // intermediate call-site rep (one malloc + a full make/dup layer
        // per spawn — the allocator traffic that dominated spawn bursts).
        // Gated on the real stdlib spawn: receiver-free, arity 1, resolving
        // to the `zz_spawn` runtime fn. User methods named `spawn`, named
        // args, and non-literal closures keep the general path (identical
        // isolation semantics either way).
        if method_receiver.is_none()
            && named.is_empty()
            && args.len() == 1
            && matches!(native_impl(&cname), Some("zz_spawn"))
        {
            if let Some(Expr::Closure { params, body, .. }) = args.last() {
                let (fname, cap_arr, kind_arr, size_arr, n, green) =
                    self.emit_closure_parts(params, body, names, out);
                let g = if green { "1" } else { "0" };
                return format!(
                    "zz_call_native_spawn({fname}, {cap_arr}, {kind_arr}, {size_arr}, {n}, {g})"
                );
            }
        }
        // Clone the method receiver up front; we may need it again in the
        // impl-method call-site branch (which needs the original Expr
        // to emit the unboxed-struct address).
        let method_receiver_for_call = method_receiver.clone();
        // If this is a method call, emit and insert the receiver as first arg.
        // For impl methods, the receiver is the unboxed struct (passed by
        // pointer to the callee), so we DO NOT box it. For other method
        // calls (e.g. vec.push, str.contains), the receiver is a
        // refcounted `zz_value` and IS boxed via `zz_clone`.
        // Detect struct receivers by checking the local's C type in NameCtx.
        let recv_is_struct = match method_receiver.as_ref() {
            Some(Expr::Ident { name, .. }) => names
                .lookup_type(name)
                .map(|t| t.starts_with("zz_struct_"))
                .unwrap_or(false),
            // Promoted receiver (`u.Base` for `u.area()`): a path whose
            // leaf resolves to an unboxed struct type.
            Some(Expr::Path { parts, .. }) if parts.len() >= 2 => names
                .lookup_type(&parts[0])
                .and_then(|bt| {
                    self.unmangled_struct_name(bt).and_then(|root| {
                        self.resolve_access_chain(&root, &parts[1..])
                            .map(|(_, leaf)| leaf.starts_with("zz_struct_"))
                    })
                })
                .unwrap_or(false),
            _ => false,
        };
        if let Some(ref recv) = method_receiver {
            // Borrowed-receiver fast path: zz_vec_push/zz_vec_append
            // never retain, release, or store the receiver array itself
            // (push dups elements internally; append mutates in place),
            // so the atomic retain that Ident emission adds is pure
            // overhead in tight push loops. Pass plain Ident receivers
            // borrowed; complex receivers already come out uncloned.
            let borrow_recv = matches!(
                native_impl(&cname),
                Some("zz_vec_push") | Some("zz_vec_append")
            ) && matches!(recv, Expr::Ident { .. });
            let recv_val = if borrow_recv {
                if let Expr::Ident { name, .. } = recv {
                    names
                        .lookup(name)
                        .map(|s| s.to_string())
                        .unwrap_or_else(|| self.emit_expr(recv, names, out))
                } else {
                    self.emit_expr(recv, names, out)
                }
            } else {
                self.emit_expr(recv, names, out)
            };
            if recv_is_struct && self.is_impl_method(&cname) {
                arg_items.push(recv_val);
            } else if let Some(sname) = self.struct_box_name(recv, names) {
                // Free-function / native method syntax on an unboxed struct
                // (`p.bump()` → `bump(p)`): regular funcs and natives take
                // boxed `zz_value` args, so the raw C struct must be boxed
                // into a runtime object — exactly like `struct_box_for_arg`
                // does for positional struct params. `box_raw_struct` reuses
                // the already-emitted raw value (pure for Ident/Path/Field
                // roots) with no re-emission.
                arg_items.push(self.box_raw_struct(&sname, &recv_val, names, out));
            } else if recv_is_struct {
                // Defensive: unboxed receiver where `struct_box_name` missed
                // (should not happen for Ident/Path). Push raw so a shape
                // mismatch surfaces as a loud C type error, never a silent
                // arg drop.
                arg_items.push(recv_val);
            } else {
                let boxed = auto_box(&recv_val, None); // receiver is always zz_value
                arg_items.push(boxed);
            }
        }
        // Save and clear void_context while emitting arguments — argument
        // return values are always consumed, so the optimization that swaps
        // vec.push → vec.append (in-place mutation) must not apply here.
        let saved_void = *self.void_context.borrow();
        *self.void_context.borrow_mut() = false;
        // Unboxed-struct params of the callee (free functions taking a
        // struct first are NOT impl methods): the emitted arg is a raw C
        // struct and must be boxed into a runtime object. Positional: slot
        // i of ordered_args matches sig param i (named args already
        // reordered above); the method receiver (if any) lives outside
        // `ordered_args`. Whenever a receiver is present, explicit arg `i`
        // maps to sig param `i+1` (param 0 is the receiver) — for impl
        // methods and non-impl callees alike, since the impl emission path
        // also passes `self` separately (`skip(1)` below).
        let recv_shift = usize::from(method_receiver.is_some());
        let struct_box_for_arg: Vec<Option<String>> = match self.tp.funcs.get(&cname) {
            Some(sig) => ordered_args
                .iter()
                .enumerate()
                .map(|(i, _)| {
                    sig.params.get(i + recv_shift).and_then(|(_, t)| match t {
                        zz_checker::Type::Struct(s, _) if self.is_unboxed_struct(s) => {
                            Some(s.clone())
                        }
                        _ => None,
                    })
                })
                .collect(),
            None => vec![None; ordered_args.len()],
        };
        for (arg_idx, a) in ordered_args.iter().enumerate() {
            if let Some(sname) = struct_box_for_arg.get(arg_idx).and_then(|o| o.clone()) {
                arg_items.push(self.emit_boxed_value(&sname, a, names, out));
                continue;
            }
            let emitted = self.emit_expr(a, names, out);
            // Display builtins (`println`, `print`, `str`, ...) render
            // unboxed structs through their generated `debug_string`
            // function (a raw C struct is not a `zz_value`).
            if Self::is_display_builtin(&cname) {
                if let Some(sname) = self.unboxed_struct_of_expr(a, names) {
                    arg_items.push(self.stringify_struct_value(&sname, emitted, names, out));
                    continue;
                }
            }
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
            } else if let Expr::Paren { expr, .. } = a {
                // Handle parentheses by looking at the inner expression.
                let inner = expr.as_ref();
                let emitted_inner = self.emit_expr(inner, names, out);
                let boxed = if let Expr::Ident { name, .. } = inner {
                    auto_box(&emitted_inner, names.lookup_type(name))
                } else if let Expr::Path { parts, .. } = inner {
                    let joined = parts.join(".");
                    let direct_type = names.lookup_type(&joined);
                    if direct_type.is_some() {
                        auto_box(&emitted_inner, direct_type)
                    } else if parts.len() == 2 {
                        if let Some(base_type) = names.lookup_type(&parts[0]) {
                            if let Some(field_type) =
                                self.field_type_from_struct(base_type, &parts[1])
                            {
                                auto_box(&emitted_inner, Some(field_type))
                            } else {
                                emitted_inner
                            }
                        } else {
                            emitted_inner
                        }
                    } else if parts.len() >= 3 {
                        self.auto_box_nested_field(parts, names, &emitted_inner)
                    } else {
                        emitted_inner
                    }
                } else if let Expr::Binary { .. } = inner {
                    let val_is_unboxed = emitted_inner.starts_with("(int64_t)(")
                        || emitted_inner.starts_with("(double)(")
                        || emitted_inner.starts_with("(bool)(");
                    let boxed = if val_is_unboxed {
                        // Determine the box type from the emitted cast.
                        if emitted_inner.starts_with("(double)(") {
                            format!("zz_float({emitted_inner})")
                        } else {
                            format!("zz_int({emitted_inner})")
                        }
                    } else {
                        // Already a zz_value — but we may need to clone it
                        // if it's a reference-counted type.
                        if let Expr::Ident { name, .. } = inner {
                            // If the variable is a refcounted type, clone it.
                            if let Some(_ty) = names.lookup_type(name) {
                                format!("zz_clone({emitted_inner})")
                            } else {
                                emitted_inner
                            }
                        } else {
                            emitted_inner
                        }
                    };
                    boxed
                } else {
                    emitted_inner
                };
                boxed
            } else if let Expr::Binary { .. } = a {
                // Binary op: result is unboxed only if the emitted C
                // expression starts with `(int64_t)(` or `(double)(`.
                // Otherwise it's already a zz_value.
                let val_is_unboxed = emitted.starts_with("(int64_t)(")
                    || emitted.starts_with("(double)(")
                    || emitted.starts_with("(bool)(");
                let boxed = if val_is_unboxed {
                    // Determine the box type from the emitted cast.
                    if emitted.starts_with("(double)(") {
                        format!("zz_float({emitted})")
                    } else {
                        format!("zz_int({emitted})")
                    }
                } else {
                    // Already a zz_value — but we may need to clone it
                    // if it's a reference-counted type.
                    if let Expr::Ident { name, .. } = a {
                        // If the variable is a refcounted type, clone it.
                        if let Some(_ty) = names.lookup_type(name) {
                            format!("zz_clone({emitted})")
                        } else {
                            emitted
                        }
                    } else {
                        emitted
                    }
                };
                boxed
            } else if let Expr::Field { obj, name, .. } = a {
                // Nested field access (e.g., r.origin.x) produces a raw C
                // scalar that must be boxed for function calls.
                // Derive field type from the parent object's struct type.
                let ctype = self.ty_at(names, obj.span()).and_then(|ot| {
                    if let zz_checker::Type::Struct(sname, _) = ot {
                        if let Some(sig) = self.tp.structs.get(sname) {
                            if let Some((_, ft)) = sig.fields.iter().find(|(n, _)| n == name) {
                                return Some(self.type_to_c(ft));
                            }
                        }
                    }
                    None
                });
                auto_box(&emitted, ctype.as_deref())
            } else {
                emitted
            };
            arg_items.push(boxed);
        }
        // Restore void_context after argument emission.
        *self.void_context.borrow_mut() = saved_void;

        // Only lower natives that survived DCE reachability AND have a C
        // runtime implementation.
        let cname_for_native = cname.clone();

        // `range(...)` has variable arity (1..=3); pad to 3 args for the
        // fixed-arity runtime constructor and materialize to an int array.
        if cname_for_native == "range" {
            let mut intof = |e: &Expr| {
                let v = self.emit_expr(e, names, out);
                box_scalar_operand(e, names, &v)
            };
            return match args.len() {
                1 => format!(
                    "zz_call_native3(zz_range3, zz_int(0), {}, zz_int(1))",
                    intof(&args[0])
                ),
                2 => format!(
                    "zz_call_native3(zz_range3, {}, {}, zz_int(1))",
                    intof(&args[0]),
                    intof(&args[1])
                ),
                3 => format!(
                    "zz_call_native3(zz_range3, {}, {}, {})",
                    intof(&args[0]),
                    intof(&args[1]),
                    intof(&args[2])
                ),
                _ => "zz_array_new()".to_string(),
            };
        }
        // Recognize namespace-qualified names from method dispatch
        // (e.g. `vec.map`) that have a C runtime impl even though
        // `reachable_natives` only tracks the bare name (`map`).
        let std_name = format!("std.{cname_for_native}");
        let is_native = self.reachable_natives.contains(&cname_for_native)
            || self.reachable_natives.contains(&std_name)
            || native_supported(&cname_for_native);
        let native_rt = if is_native {
            // Embedded C runtime first, Rust staticlib second (Phase 1+).
            native_impl(&cname_for_native).or_else(|| crate::ffi_impl(&cname_for_native))
        } else {
            None
        };
        if let Some(impl_name) = native_rt {
            // In void context (result discarded), use in-place mutation
            // instead of copy-on-write for vec.push.
            let effective_name = if *self.void_context.borrow() && impl_name == "zz_vec_push" {
                "zz_vec_append"
            } else {
                impl_name
            };
            // Route-handler fast path: a 1-param closure literal that
            // never references its parameter registers under the `_fast`
            // twin, letting the socket server skip request-dict
            // construction. The handler is always the last user arg
            // (receiver excluded) across all five spellings.
            let effective_name: &str = match effective_name {
                "zz_http_route_get"
                | "zz_http_route_post"
                | "zz_http_route_put"
                | "zz_http_route_delete"
                | "zz_http_route" => {
                    let ignores = ordered_args.last().is_some_and(|h| match *h {
                        Expr::Closure {
                            ref params,
                            ref body,
                            ..
                        } => closure_ignores_param(params, body),
                        _ => false,
                    });
                    if ignores {
                        match effective_name {
                            "zz_http_route_get" => "zz_http_route_get_fast",
                            "zz_http_route_post" => "zz_http_route_post_fast",
                            "zz_http_route_put" => "zz_http_route_put_fast",
                            "zz_http_route_delete" => "zz_http_route_delete_fast",
                            _ => "zz_http_route_fast",
                        }
                    } else {
                        effective_name
                    }
                }
                _ => effective_name,
            };
            // Arena-aware str_cast inside loops: allocate result on arena
            // to avoid heap malloc for intermediate string conversions.
            if effective_name == "zz_str_cast" {
                if let Some(ref arena) = *self.current_loop_arena.borrow() {
                    let a = &arg_items[0];
                    return format!("zz_str_cast_arena({a}, &(int){{0}}, &{arena})");
                }
            }
            // `http.get/post/put/delete/respond/post_json` accept an omitted
            // trailing `headers` (checker `has_default`); `http.fetch`
            // accepts up to four omitted trailing options
            // (method/headers/body/timeout_ms). Pad like `range` above.
            if matches!(
                effective_name,
                "zz_http_get"
                    | "zz_http_post"
                    | "zz_http_put"
                    | "zz_http_delete"
                    | "zz_http_respond"
                    | "zz_http_post_json"
            ) {
                let full_arity = match effective_name {
                    "zz_http_get" | "zz_http_delete" => 2,
                    _ => 3,
                };
                if arg_items.len() + 1 == full_arity {
                    arg_items.push("zz_dict_new()".to_string());
                }
            }
            if effective_name == "zz_http_fetch" {
                // Signature: (url, method, headers, body, timeout_ms).
                while arg_items.len() < 5 {
                    let pad = match 5 - arg_items.len() {
                        4 => "zz_str_static(\"GET\")".to_string(),
                        3 => "zz_dict_new()".to_string(),
                        2 => "zz_str_static(\"\")".to_string(),
                        _ => "zz_int(30000)".to_string(),
                    };
                    arg_items.push(pad);
                }
            }
            // Borrowed-arg fast path for proven read-only natives: `zz_len`
            // only inspects its input's tag/length and `zz_fs_write` /
            // `zz_fs_append` only read the data bytes during the call, so
            // passing the live local directly avoids a `zz_clone` temporary
            // that nothing would release (one leaked share per call — a
            // full 1MB per `len(big)` / `write(big)`). Mirrors the
            // index-receiver borrowed fast path above. Only exact
            // `zz_clone(barecid)` shapes are stripped (live C locals and
            // globals, which outlive the synchronous call); complex
            // temporaries keep their owned form.
            if matches!(effective_name, "zz_len" | "zz_fs_write" | "zz_fs_append") {
                for item in arg_items.iter_mut() {
                    if let Some(inner) = item
                        .strip_prefix("zz_clone(")
                        .and_then(|s| s.strip_suffix(')'))
                    {
                        if !inner.is_empty()
                            && inner
                                .bytes()
                                .all(|b| b.is_ascii_alphanumeric() || b == b'_')
                            && !inner.starts_with("zz_")
                        {
                            *item = inner.to_string();
                        }
                    }
                }
            }
            return match arg_items.len() {
                1 => {
                    let a = &arg_items[0];
                    // Green yield (B3): a suspendable call in direct
                    // statement position inside a green closure suspends
                    // the task instead of parking the thread. The value
                    // travels through a frame temp cell so both the fresh
                    // path (stores the call result) and the resume path
                    // (loads the handed-off value at the label) converge
                    // on the same value string for the enclosing
                    // statement. Non-statement positions keep the
                    // blocking call (thread parks; always correct).
                    if stmt_direct
                        && self.green_active()
                        && (effective_name == "zz_chan_recv"
                            || effective_name == "zz_task_join_recv")
                    {
                        let green_fn = if effective_name == "zz_chan_recv" {
                            "zz_chan_recv_green"
                        } else {
                            "zz_task_join_recv_green"
                        };
                        let tmp = self.green_temp(names, out);
                        let k = self.green_resume_id();
                        let yv = names.fresh("_yv");
                        out.push_str(&format!(
                            "    {{ int _e = 0; zz_value {yv} = {green_fn}({a}, zz_fr, {k}, &_e);\n"
                        ));
                        out.push_str("      if (zz_green_suspended()) { return zz_unit(); }\n");
                        out.push_str(&format!("      {tmp} = {yv}; goto green_done_{k}; }}\n"));
                        out.push_str(&format!(
                            "    green_L_{k}: zz_green_resumed(); {tmp} = zz_fr->value; zz_fr->has_value = 0;\n"
                        ));
                        out.push_str(&format!("    green_done_{k}: ;\n"));
                        return tmp;
                    }
                    format!("zz_call_native1({effective_name}, {a})")
                }
                0 => format!("zz_call_native0({effective_name})"),
                2 => {
                    let a = &arg_items[0];
                    let b = &arg_items[1];
                    format!("zz_call_native2({effective_name}, {a}, {b})")
                }
                3 => {
                    let a = &arg_items[0];
                    let b = &arg_items[1];
                    let c = &arg_items[2];
                    format!("zz_call_native3({effective_name}, {a}, {b}, {c})")
                }
                4 => {
                    let a = &arg_items[0];
                    let b = &arg_items[1];
                    let c = &arg_items[2];
                    let d = &arg_items[3];
                    format!("zz_call_native4({effective_name}, {a}, {b}, {c}, {d})")
                }
                5 => {
                    let a = &arg_items[0];
                    let b = &arg_items[1];
                    let c = &arg_items[2];
                    let d = &arg_items[3];
                    let e = &arg_items[4];
                    format!("zz_call_native5({effective_name}, {a}, {b}, {c}, {d}, {e})")
                }
                _ => "zz_unit()".to_string(),
            };
        }

        // Reachable native without a C runtime impl (VM-only surface,
        // e.g. Phase 3 HTTP): abort loudly with the missing name. The old
        // silent unit wedged programs — a retry loop matching on the
        // result never fires when the value is unit (http_tls_p3 spun
        // forever). The name is an internal dotted key (safe charset).
        if is_native {
            return format!("zz_unimplemented_native(\"{cname_for_native}\")");
        }

        // Bare-name fallback: the loader qualifies same-file calls, but a
        // bare `greet_user` can still arrive here (generated code, REPL).
        // A unique `*.name` reachable function is that same-file target.
        let owned_fallback: Option<String>;
        let cname_ref: &str = if !cname_for_native.contains('.')
            && !self.reachable_funcs.contains(&cname_for_native)
            && !self.tp.funcs.contains_key(&cname_for_native)
        {
            let suffix = format!(".{cname_for_native}");
            let mut hits: Vec<&String> = self
                .reachable_funcs
                .iter()
                .filter(|f| f.ends_with(suffix.as_str()))
                .collect();
            hits.sort_unstable();
            hits.dedup();
            if hits.len() == 1 {
                owned_fallback = Some(hits[0].clone());
                owned_fallback.as_deref().unwrap()
            } else {
                owned_fallback = None;
                cname_for_native.as_str()
            }
        } else {
            owned_fallback = None;
            cname_for_native.as_str()
        };
        // Shadow `cname_for_native` with the resolved name for the rest of
        // this function (avoids touching every use below).
        let cname_for_native: String = cname_ref.to_string();
        let _ = &owned_fallback;

        if self.reachable_funcs.contains(&cname_for_native)
            && (self.tp.funcs.contains_key(&cname_for_native)
                || self.is_impl_method(&cname_for_native))
        {
            let cf = format!("zz_fn_{}", mangle(&cname_for_native));
            // Impl methods: callee signature is
            // `zz_fn_X(<struct>* self, zz_value* args, size_t argc)`.
            // The receiver is passed as a pointer; the remaining args
            // are boxed zz_values in the args array. Name-shape checked
            // (a free function taking a struct first is NOT a method).
            let is_impl_method = self.is_impl_method(&cname_for_native);
            if is_impl_method {
                if let Some(method_receiver) = method_receiver_for_call.as_ref() {
                    // Struct name from `ns.Type.method` (e.g. `main.Box`
                    // from `main.Box.inc`).
                    let sname: Option<String> = cname_for_native
                        .rsplit_once('.')
                        .map(|(s, _)| s.to_string());
                    let unboxed = sname.as_deref().is_some_and(|s| self.is_unboxed_struct(s));
                    // Unbox a boxed `zz_value` temp into a raw struct temp
                    // for unboxed-receiver methods. Boxed-receiver methods
                    // take `zz_value *self`, so the boxed temp passes
                    // directly.
                    let unbox_tmp = |boxed: &str, names: &mut NameCtx, out: &mut String| {
                        if unboxed {
                            let sname = sname.clone().unwrap_or_default();
                            let raw = names.fresh("_recv_raw");
                            let ctype = format!("zz_struct_{}", mangle(&sname));
                            out.push_str(&format!("    {ctype} {raw};\n"));
                            self.emit_unbox_struct(&sname, boxed, &raw, names, out);
                            raw
                        } else {
                            boxed.to_string()
                        }
                    };
                    let recv_val = if let Expr::Ident { name, .. } = method_receiver {
                        match names.lookup(name) {
                            Some(cid) => {
                                let cid = cid.to_string();
                                // Raw struct local: pass its address
                                // directly. Boxed local (e.g. bound to a
                                // call/index result): unbox into a temp.
                                let is_raw = names
                                    .lookup_type(name)
                                    .is_some_and(|t| t.starts_with("zz_struct_"));
                                if is_raw {
                                    cid
                                } else {
                                    unbox_tmp(&cid, names, out)
                                }
                            }
                            None => self.emit_expr(method_receiver, names, out),
                        }
                    } else if let Expr::Path { parts, .. } = method_receiver {
                        // Promoted struct receiver (`u.Base`): emit the raw
                        // lvalue chain so `&(...)` takes the embedded
                        // object's address. Boxed receivers are not lvalues,
                        // so materialize them into a temp instead.
                        if let Some(lval) = self.emit_struct_lvalue(parts, names) {
                            lval
                        } else {
                            let rv = self.emit_expr(method_receiver, names, out);
                            let tmp = names.fresh("_recv");
                            out.push_str(&format!("    zz_value {tmp} = {rv};\n"));
                            unbox_tmp(&tmp, names, out)
                        }
                    } else {
                        // Arbitrary receiver (`arr[0]`, `make_pt()`, ...):
                        // always a boxed `zz_value` at runtime (array
                        // elements and call results never inhabit raw C
                        // structs). Materialize into a temp, then unbox
                        // into a raw struct temp when the method expects
                        // an unboxed receiver.
                        let rv = self.emit_expr(method_receiver, names, out);
                        let boxed = names.fresh("_recv_box");
                        out.push_str(&format!("    zz_value {boxed} = {rv};\n"));
                        unbox_tmp(&boxed, names, out)
                    };
                    let rest_args: Vec<String> = arg_items.into_iter().skip(1).collect();
                    let rest_n = rest_args.len();
                    if rest_n == 0 {
                        return format!("{cf}(&{recv_val}, NULL, 0)");
                    }
                    return format!(
                        "{cf}(&{recv_val}, (zz_value[]){{ {joined} }}, {n})",
                        joined = rest_args.join(", "),
                        n = rest_n
                    );
                }
                // No receiver (shouldn't happen for impl methods but
                // fall through to the regular path defensively).
            }
            if arg_items.is_empty() {
                return format!("{cf}(NULL, 0)");
            }
            return format!(
                "({cf}((zz_value[]){{ {joined} }}, {n}))",
                joined = arg_items.join(", "),
                n = arg_items.len()
            );
        }

        // Indirect closure call: `f(args)` where `f` is a local/global
        // holding a closure value, not a statically-known func. Previously
        // this fell through to `zz_unit()`, so any first-class closure
        // invocation (`f := make_adder(); f(5)`) silently returned unit.
        // Dispatch through the null-safe `zz_call_closure` helper: genuine
        // closures get called, non-closure values keep the old unit result.
        if method_receiver.is_none() {
            // Resolve first (immutable borrow ends), then emit (mutable borrow).
            enum Indirect {
                Known,
                Other,
                No,
            }
            let kind = match callee {
                Expr::Ident { name, .. } => {
                    if names.lookup(name).is_some() {
                        Indirect::Known
                    } else {
                        Indirect::No
                    }
                }
                Expr::Path { parts, .. } => {
                    let joined = parts.join(".");
                    if names.lookup(&joined).is_some() {
                        Indirect::Known
                    } else if parts
                        .last()
                        .is_some_and(|last| names.lookup(last).is_some())
                    {
                        // Qualified closure var (`function_types.transform`):
                        // globals are seeded bare, so accept the leaf.
                        Indirect::Known
                    } else {
                        Indirect::No
                    }
                }
                // Arbitrary callee expression (e.g. `make_adder()(5)`):
                // evaluate it, then dispatch. `Field` callees are method-ish
                // and keep the old unit fallback.
                _ if !matches!(callee, Expr::Field { .. }) => Indirect::Other,
                _ => Indirect::No,
            };
            let closure_val: Option<String> = match kind {
                Indirect::No => None,
                Indirect::Other => Some(self.emit_expr(callee, names, out)),
                Indirect::Known => {
                    // Qualified closure var lowers via its bare global
                    // (`function_types.transform` → `transform` global);
                    // `emit_expr` on the qualified path would yield unit.
                    if let Expr::Path { parts, .. } = callee {
                        let joined = parts.join(".");
                        let leaf_hit = names.lookup(&joined).is_none()
                            && parts
                                .last()
                                .is_some_and(|last| names.lookup(last).is_some());
                        if leaf_hit {
                            let leaf = Expr::Ident {
                                name: parts.last().cloned().unwrap_or_default(),
                                span: callee.span(),
                            };
                            Some(self.emit_expr(&leaf, names, out))
                        } else {
                            Some(self.emit_expr(callee, names, out))
                        }
                    } else {
                        Some(self.emit_expr(callee, names, out))
                    }
                }
            };
            if let Some(cv) = closure_val {
                if arg_items.is_empty() {
                    return format!("zz_call_closure({cv}, NULL, 0)");
                }
                return format!(
                    "zz_call_closure({cv}, (zz_value[]){{ {joined} }}, {n})",
                    joined = arg_items.join(", "),
                    n = arg_items.len()
                );
            }
        }

        "zz_unit()".to_string()
    }

    /// Emit an expression in tail/return position as a `zz_value`.
    ///
    /// Every generated function returns `zz_value`, but unboxed structs
    /// lower to raw C structs. A tail that is (or names) an unboxed
    /// struct must be boxed into a runtime object first — otherwise C
    /// rejects `return <raw struct>` / `tmp = <raw struct>`. Scalars
    /// keep the existing `box_scalar_operand` path; anything already
    /// boxed passes through unchanged.
    pub(super) fn emit_tail_value(
        &self,
        e: &Expr,
        names: &mut NameCtx,
        out: &mut String,
    ) -> String {
        // Check BEFORE emitting so side-effecting values emit exactly once.
        if let Expr::StructInit { name, .. } = e {
            if self.is_unboxed_struct(name) {
                return self.emit_boxed_value(name, e, names, out);
            }
        }
        if let Some(sname) = self.unboxed_struct_of_expr(e, names) {
            return self.emit_boxed_value(&sname, e, names, out);
        }
        let v = self.emit_expr(e, names, out);
        box_scalar_operand(e, names, &v)
    }

    /// Lower a struct literal, distributing flattened (promoted) fields
    /// into embedded sub-objects (`User{id: 1, age: 2}` fills `Base.id`).
    /// Literals without flattened fields keep the historical emission
    /// exactly (evaluation order included).
    pub(super) fn emit_struct_init(
        &self,
        sname: &str,
        fields: &[(String, Expr)],
        names: &mut NameCtx,
        out: &mut String,
    ) -> String {
        // Canonicalize selective-import aliases (`Product` →
        // `product.Product`) so the emitted C type matches the
        // HIR-declared slot type (the checker canonicalizes the same
        // way). Miss-only with a structs membership guard, so local
        // structs and func-name collisions are unaffected.
        let resolved;
        let sname: &str = if let Some(q) = self.import_fn_aliases.get(sname) {
            if self.tp.structs.contains_key(q) {
                resolved = q.clone();
                &resolved
            } else {
                sname
            }
        } else {
            sname
        };
        let all_direct = self.tp.structs.get(sname).is_some_and(|sig| {
            fields
                .iter()
                .all(|(fname, _)| sig.fields.iter().any(|(n, _)| n == fname))
        });
        if all_direct {
            return self.emit_struct_init_direct(sname, fields, names, out);
        }
        let entries: Vec<(Vec<String>, &Expr)> = fields
            .iter()
            .map(|(fname, fexpr)| (self.literal_field_path(sname, fname), fexpr))
            .collect();
        self.emit_struct_lit(sname, &entries, names, out, false)
    }

    /// Historical struct-literal emission for literals whose fields are all
    /// direct (no flattening): unboxed C literals or boxed runtime objects.
    fn emit_struct_init_direct(
        &self,
        name: &str,
        fields: &[(String, Expr)],
        names: &mut NameCtx,
        out: &mut String,
    ) -> String {
        // Check if this struct is unboxed
        if self.is_unboxed_struct(name) {
            let c_type = self.struct_c_type(name);
            let mut field_inits = Vec::new();
            for (field_name, field_expr) in fields {
                let field_val = self.emit_expr(field_expr, names, out);
                // Check if the field type is scalar
                if let Some(sig) = self.tp.structs.get(name) {
                    if let Some((_, field_type)) = sig.fields.iter().find(|(n, _)| n == field_name)
                    {
                        // Scalar fields arrive boxed (`zz_int(1)`, call
                        // results) or raw (locals, arithmetic) depending on
                        // the producer — extract `.i`/`.f`/`.b` only from
                        // boxed forms, otherwise use the raw value as-is.
                        // (Unconditional extraction miscompiles
                        // `Box{v: i}` for raw `i` as `(i).i`.)
                        let ident = match field_expr {
                            Expr::Ident { name, .. } => Some(name.as_str()),
                            _ => None,
                        };
                        let raw =
                            crate::lower::context::emitted_is_raw_scalar(&field_val, names, ident);
                        let final_val = match field_type {
                            zz_checker::Type::Int if !raw => format!("({field_val}).i"),
                            zz_checker::Type::Float if !raw => format!("({field_val}).f"),
                            zz_checker::Type::Bool if !raw => format!("({field_val}).b"),
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
            // Boxed struct: use zz_object_new + zz_object_set_field
            let sig = match self.tp.structs.get(name) {
                Some(s) => s,
                None => return "zz_unit()".to_string(),
            };
            let n = sig.fields.len();
            let obj_tmp = names.fresh("__obj");
            // Build field_names array: interned strings for each field name
            let names_arr_tmp = names.fresh("__field_names");
            out.push_str(&format!("    zz_value {names_arr_tmp}[{n}];\n"));
            for (i, (fname, _)) in sig.fields.iter().enumerate() {
                out.push_str(&format!(
                    "    {names_arr_tmp}[{i}] = zz_str_static(\"{fname}\");\n",
                ));
            }
            // Create the object
            out.push_str(&format!(
                "    zz_value {obj_tmp} = zz_object_new(\"{name}\", {names_arr_tmp}, {n});\n",
            ));
            // Set each field from the StructInit's fields list
            for (fname, fexpr) in fields {
                // An unboxed struct value stored in a boxed parent must be
                // boxed into a runtime object first (a raw C struct is not
                // a `zz_value`).
                let for_boxing = sig
                    .fields
                    .iter()
                    .find(|(n, _)| n == fname)
                    .and_then(|(_, ft)| match ft {
                        zz_checker::Type::Struct(inner, _) if self.is_unboxed_struct(inner) => {
                            Some(inner.clone())
                        }
                        _ => None,
                    });
                if let Some(inner) = for_boxing {
                    let child = self.emit_boxed_value(&inner, fexpr, names, out);
                    out.push_str(&format!(
                        "    zz_object_set_field(&{obj_tmp}, \"{fname}\", {child});\n",
                    ));
                    continue;
                }
                let fval = self.emit_expr(fexpr, names, out);
                // Box the field value if it's a scalar type. The boxer
                // sees the source expression (not just the emitted text)
                // so already-boxed values (params, calls) pass through
                // instead of being re-wrapped (C type error).
                let boxed_fval =
                    if let Some((_, field_type)) = sig.fields.iter().find(|(n, _)| n == fname) {
                        Self::box_struct_field_expr(fexpr, fval, field_type, names)
                    } else {
                        fval
                    };
                out.push_str(&format!(
                    "    zz_object_set_field(&{obj_tmp}, \"{fname}\", {boxed_fval});\n",
                ));
            }
            obj_tmp
        }
    }

    /// Box an unboxed-struct-typed value into a runtime `ZZ_OBJECT` so it
    /// can be stored inside a boxed parent (e.g. an embedded all-scalar
    /// struct inside a struct holding strings). Literals lower
    /// field-by-field; any other expression is read member-wise through
    /// synthesized field accesses.
    pub(super) fn emit_boxed_value(
        &self,
        sname: &str,
        value: &Expr,
        names: &mut NameCtx,
        out: &mut String,
    ) -> String {
        if let Expr::StructInit { fields, .. } = value {
            let entries: Vec<(Vec<String>, &Expr)> = fields
                .iter()
                .map(|(fname, fexpr)| (self.literal_field_path(sname, fname), fexpr))
                .collect();
            return self.emit_struct_lit(sname, &entries, names, out, true);
        }
        let sig_fields: Vec<(String, zz_checker::Type)> = self
            .tp
            .structs
            .get(sname)
            .map(|s| s.fields.clone())
            .unwrap_or_default();
        let n = sig_fields.len();
        let obj_tmp = names.fresh("__obj");
        let names_arr_tmp = names.fresh("__field_names");
        out.push_str(&format!("    zz_value {names_arr_tmp}[{n}];\n"));
        for (i, (fname, _)) in sig_fields.iter().enumerate() {
            out.push_str(&format!(
                "    {names_arr_tmp}[{i}] = zz_str_static(\"{fname}\");\n",
            ));
        }
        out.push_str(&format!(
            "    zz_value {obj_tmp} = zz_object_new(\"{sname}\", {names_arr_tmp}, {n});\n",
        ));
        for (fname, fty) in &sig_fields {
            let access = Expr::Field {
                obj: Box::new(value.clone()),
                name: fname.clone(),
                span: value.span(),
            };
            if let zz_checker::Type::Struct(inner, _) = fty {
                if self.is_unboxed_struct(inner) {
                    let child = self.emit_boxed_value(inner, &access, names, out);
                    out.push_str(&format!(
                        "    zz_object_set_field(&{obj_tmp}, \"{fname}\", {child});\n",
                    ));
                    continue;
                }
            }
            let fval = self.emit_expr(&access, names, out);
            let boxed = Self::box_struct_field_expr(&access, fval, fty, names);
            out.push_str(&format!(
                "    zz_object_set_field(&{obj_tmp}, \"{fname}\", {boxed});\n",
            ));
        }
        obj_tmp
    }

    /// Recursive struct-literal emission over concrete paths. `entries` maps
    /// a concrete path (`[Base, id]`) to its value expression. Only used
    /// for literals with flattened fields; the checker guarantees every
    /// entry resolves and every required leaf is covered.
    fn emit_struct_lit(
        &self,
        sname: &str,
        entries: &[(Vec<String>, &Expr)],
        names: &mut NameCtx,
        out: &mut String,
        force_boxed: bool,
    ) -> String {
        let sig_fields: Vec<(String, zz_checker::Type)> = self
            .tp
            .structs
            .get(sname)
            .map(|s| s.fields.clone())
            .unwrap_or_default();
        if sig_fields.is_empty() {
            return "zz_unit()".to_string();
        }
        // `force_boxed` renders an unboxed struct as a runtime object so it
        // can be stored inside a boxed parent (see `emit_boxed_value`).
        if self.is_unboxed_struct(sname) && !force_boxed {
            let c_type = self.struct_c_type(sname);
            let mut field_inits = Vec::new();
            for (fname, fty) in &sig_fields {
                if let Some((_, fexpr)) =
                    entries.iter().find(|(p, _)| p.len() == 1 && &p[0] == fname)
                {
                    let field_val = self.emit_expr(fexpr, names, out);
                    let final_val = match fty {
                        zz_checker::Type::Int => format!("({field_val}).i"),
                        zz_checker::Type::Float => format!("({field_val}).f"),
                        zz_checker::Type::Bool => format!("({field_val}).b"),
                        _ => field_val,
                    };
                    field_inits.push(format!(".{fname} = {final_val}"));
                } else if let zz_checker::Type::Struct(inner, _) = fty {
                    let child: Vec<(Vec<String>, &Expr)> = entries
                        .iter()
                        .filter(|(p, _)| p.len() > 1 && &p[0] == fname)
                        .map(|(p, e)| (p[1..].to_vec(), *e))
                        .collect();
                    if !child.is_empty() {
                        let child_val = self.emit_struct_lit(inner, &child, names, out, false);
                        field_inits.push(format!(".{fname} = {child_val}"));
                    }
                    // Otherwise missing (rejected by the checker) — C
                    // designated initializers zero-fill the rest.
                }
            }
            format!("({c_type}){{ {}}}", field_inits.join(", "))
        } else {
            // Boxed struct: nested objects are built into temps, then set.
            let n = sig_fields.len();
            let obj_tmp = names.fresh("__obj");
            let names_arr_tmp = names.fresh("__field_names");
            out.push_str(&format!("    zz_value {names_arr_tmp}[{n}];\n"));
            for (i, (fname, _)) in sig_fields.iter().enumerate() {
                out.push_str(&format!(
                    "    {names_arr_tmp}[{i}] = zz_str_static(\"{fname}\");\n",
                ));
            }
            out.push_str(&format!(
                "    zz_value {obj_tmp} = zz_object_new(\"{sname}\", {names_arr_tmp}, {n});\n",
            ));
            for (fname, fty) in &sig_fields {
                if let Some((_, fexpr)) =
                    entries.iter().find(|(p, _)| p.len() == 1 && &p[0] == fname)
                {
                    // An unboxed struct value stored in a boxed parent must
                    // be boxed into a runtime object first.
                    if let zz_checker::Type::Struct(inner, _) = fty {
                        if self.is_unboxed_struct(inner) {
                            let child = self.emit_boxed_value(inner, fexpr, names, out);
                            out.push_str(&format!(
                                "    zz_object_set_field(&{obj_tmp}, \"{fname}\", {child});\n",
                            ));
                            continue;
                        }
                    }
                    let fval = self.emit_expr(fexpr, names, out);
                    let boxed_fval = Self::box_struct_field_expr(fexpr, fval, fty, names);
                    out.push_str(&format!(
                        "    zz_object_set_field(&{obj_tmp}, \"{fname}\", {boxed_fval});\n",
                    ));
                } else if let zz_checker::Type::Struct(inner, _) = fty {
                    let child: Vec<(Vec<String>, &Expr)> = entries
                        .iter()
                        .filter(|(p, _)| p.len() > 1 && &p[0] == fname)
                        .map(|(p, e)| (p[1..].to_vec(), *e))
                        .collect();
                    if !child.is_empty() {
                        let child_val = self.emit_struct_lit(inner, &child, names, out, true);
                        out.push_str(&format!(
                            "    zz_object_set_field(&{obj_tmp}, \"{fname}\", {child_val});\n",
                        ));
                    }
                }
            }
            obj_tmp
        }
    }

    /// Resolve a free function for `Field`-callee method syntax
    /// (`recv.method()` where the receiver is not a bare local).
    /// Mirrors the checker's `Struct(sname)` → `{ns}.{method}` fallback
    /// order: bare name, `{ns}.{method}` (when the struct name is
    /// namespaced), unique `*.{method}` suffix, bare native. Returns
    /// `None` when nothing resolves — callers then keep the old
    /// `(method, None)` fallthrough.
    fn field_free_target(&self, method: &str, sname: Option<&str>) -> Option<String> {
        // Bare names count only with a real definition: reachable_funcs
        // can hold phantom short names (the callgraph records
        // Field-callee method tails unqualified), and resolving to one
        // drops the call to unit downstream where tp.funcs/native_impl
        // know nothing under that spelling.
        if self.tp.funcs.contains_key(method) {
            return Some(method.to_string());
        }
        if let Some(s) = sname {
            if let Some((ns, _)) = s.rsplit_once('.') {
                let cand = format!("{ns}.{method}");
                if self.reachable_funcs.contains(&cand) || self.tp.funcs.contains_key(&cand) {
                    return Some(cand);
                }
            }
        }
        let suffix = format!(".{method}");
        let mut hits: Vec<&String> = self
            .reachable_funcs
            .iter()
            .filter(|f| f.ends_with(&suffix))
            .collect();
        for k in self.tp.funcs.keys() {
            if k.ends_with(&suffix) && !hits.contains(&k) {
                hits.push(k);
            }
        }
        hits.sort_unstable();
        hits.dedup();
        if hits.len() == 1 {
            return Some(hits[0].clone());
        }
        if self.reachable_natives.contains(method) || native_supported(method) {
            return Some(method.to_string());
        }
        None
    }

    /// Box a resolved plain function as a first-class value, or `None`.
    /// Only non-generic, non-extern, non-method functions with an emitted
    /// definition qualify: generics are rejected by the checker, externs
    /// need a C ABI and methods need a receiver, neither of which a value
    /// can carry (both keep their existing behavior).
    fn static_func_value(&self, name: &str) -> Option<String> {
        let sig = self.tp.funcs.get(name)?;
        if !sig.generics.is_empty() || sig.is_extern {
            return None;
        }
        if self.is_impl_method(name) {
            return None;
        }
        self.find_func_def(name)?;
        Some(format!("zz_func_of_static(&zz_fn_{})", mangle(name)))
    }

    /// Resolve a bare function reference (`f := join`): selective-import
    /// canonical first (plus its `std.`-stripped form, which is where
    /// stdlib bodies live), then the bare spelling itself. First
    /// body-backed hit wins.
    fn static_func_value_ident(&self, name: &str) -> Option<String> {
        let mut cands = Vec::new();
        if let Some(canon) = self.import_fn_aliases.get(name) {
            cands.push(canon.clone());
            if let Some(stripped) = canon.strip_prefix("std.") {
                cands.push(stripped.to_string());
            }
        }
        cands.push(name.to_string());
        cands.into_iter().find_map(|c| self.static_func_value(&c))
    }

    /// Resolve a path function reference (`f := ns.func`): the spelling
    /// itself, then import-alias and `std.`-stripped canonicals — the
    /// same candidate rule as call lowering (values have no receiver,
    /// so no method dispatch applies). First body-backed hit wins.
    fn static_func_value_path(&self, parts: &[String]) -> Option<String> {
        let joined = parts.join(".");
        let mut cands = vec![joined.clone()];
        if parts.len() >= 2 {
            if let Some(head) = self.import_ns_aliases.get(&parts[0]) {
                let resolved = format!("{head}.{}", parts[1..].join("."));
                cands.push(resolved.clone());
                if let Some(stripped) = resolved.strip_prefix("std.") {
                    cands.push(stripped.to_string());
                }
            }
            if let Some(stripped) = joined.strip_prefix("std.") {
                cands.push(stripped.to_string());
            }
        }
        cands.into_iter().find_map(|c| self.static_func_value(&c))
    }

    /// Box a struct field value for `zz_object_set_field` given the field
    /// expression, its emitted form, and its declared type. Scalar-typed
    /// fields route through [`box_scalar_operand`], which boxes raw C
    /// scalars (`zz_int/float/bool(...)`) but passes already-boxed values
    /// (params, calls, boxed locals) through untouched. The old
    /// string-sniffing boxer re-wrapped boxed values (`zz_bool(zz_clone(v))`
    /// — a C type error) whenever the value wasn't literally prefixed.
    fn box_struct_field_expr(
        fexpr: &Expr,
        fval: String,
        fty: &zz_checker::Type,
        names: &NameCtx,
    ) -> String {
        match fty {
            zz_checker::Type::Int | zz_checker::Type::Float | zz_checker::Type::Bool => {
                box_scalar_operand(fexpr, names, &fval)
            }
            _ => fval,
        }
    }

    /// Un-mangled struct name when `e` lowers to a raw (unboxed) C struct
    /// value: struct literals of unboxed type plus Ident/Path/Field shapes
    /// rooted at raw values (see `unboxed_struct_of_expr`). Calls, indexes,
    /// arrays, blocks, etc. lower boxed and yield `None`.
    fn struct_box_name(&self, e: &Expr, names: &NameCtx) -> Option<String> {
        if let Expr::StructInit { name, .. } = e {
            if self.is_unboxed_struct(name) {
                return Some(name.clone());
            }
            return None;
        }
        self.unboxed_struct_of_expr(e, names)
    }

    /// Box an already-emitted raw struct expression (`(v0)`, `(lit)`, …)
    /// into a runtime `ZZ_OBJECT` so `zz_binop`/`zz_truthy` receive a
    /// `zz_value`. Raw producers (locals, literals, field reads) are pure,
    /// so reusing the emitted text per field is side-effect free. The
    /// emitted operand itself is left untouched (no re-emission).
    fn box_raw_struct(
        &self,
        sname: &str,
        raw: &str,
        names: &mut NameCtx,
        out: &mut String,
    ) -> String {
        let sig_fields: Vec<(String, zz_checker::Type)> = self
            .tp
            .structs
            .get(sname)
            .map(|s| s.fields.clone())
            .unwrap_or_default();
        if sig_fields.is_empty() {
            return "zz_unit()".to_string();
        }
        let n = sig_fields.len();
        let obj_tmp = names.fresh("__eqobj");
        let names_arr_tmp = names.fresh("__eqfields");
        out.push_str(&format!("    zz_value {names_arr_tmp}[{n}];\n"));
        for (i, (fname, _)) in sig_fields.iter().enumerate() {
            out.push_str(&format!(
                "    {names_arr_tmp}[{i}] = zz_str_static(\"{fname}\");\n",
            ));
        }
        out.push_str(&format!(
            "    zz_value {obj_tmp} = zz_object_new(\"{sname}\", {names_arr_tmp}, {n});\n",
        ));
        for (fname, fty) in &sig_fields {
            // Follow embedded promotion when the field lives in a base
            // struct (`u.id` → `(u).Base.id`).
            let chain = self
                .resolve_access_chain(sname, std::slice::from_ref(fname))
                .map(|(c, _)| c)
                .unwrap_or_else(|| vec![fname.clone()]);
            let mut acc = format!("({raw})");
            for p in &chain {
                acc = format!("({acc}).{p}");
            }
            let boxed = match fty {
                zz_checker::Type::Int => format!("zz_int({acc})"),
                zz_checker::Type::Float => format!("zz_float({acc})"),
                zz_checker::Type::Bool => format!("zz_bool({acc})"),
                zz_checker::Type::Struct(inner, _) if self.is_unboxed_struct(inner) => {
                    self.box_raw_struct(inner, &acc, names, out)
                }
                _ => format!("zz_clone({acc})"),
            };
            out.push_str(&format!(
                "    zz_object_set_field(&{obj_tmp}, \"{fname}\", {boxed});\n",
            ));
        }
        obj_tmp
    }

    /// Unbox a boxed struct `zz_value` (`boxed`, a `zz_object`) into a raw
    /// unboxed C struct lvalue (`raw`, e.g. `zz_struct_ns__Box`). Emits one
    /// field extraction per declared field: `zz_object_get_field` yields an
    /// owned `zz_value`, whose scalar payload (`.i`/`.f`/`.b`) fills the raw
    /// field. Nested unboxed structs recurse through a field temp. Only
    /// called for unboxed struct types (all fields scalar or nested
    /// unboxed), so every field has a known scalar extraction.
    fn emit_unbox_struct(
        &self,
        sname: &str,
        boxed: &str,
        raw: &str,
        names: &mut NameCtx,
        out: &mut String,
    ) {
        let fields: Vec<(String, zz_checker::Type)> = self
            .tp
            .structs
            .get(sname)
            .map(|s| s.fields.clone())
            .unwrap_or_default();
        for (fname, fty) in &fields {
            // Follow embedded promotion when the field lives in a base
            // struct (`u.id` → `(u).Base.id`).
            let chain = self
                .resolve_access_chain(sname, std::slice::from_ref(fname))
                .map(|(c, _)| c)
                .unwrap_or_else(|| vec![fname.clone()]);
            let mut acc = format!("({raw})");
            for p in &chain {
                acc = format!("({acc}).{p}");
            }
            match fty {
                zz_checker::Type::Int => {
                    out.push_str(&format!(
                        "    {acc} = zz_object_get_field(&{boxed}, \"{fname}\").i;\n"
                    ));
                }
                zz_checker::Type::Float => {
                    out.push_str(&format!(
                        "    {acc} = zz_object_get_field(&{boxed}, \"{fname}\").f;\n"
                    ));
                }
                zz_checker::Type::Bool => {
                    out.push_str(&format!(
                        "    {acc} = zz_object_get_field(&{boxed}, \"{fname}\").b;\n"
                    ));
                }
                zz_checker::Type::Struct(inner, _) if self.is_unboxed_struct(inner) => {
                    let ftmp = names.fresh("_unbox_f");
                    out.push_str(&format!(
                        "    zz_value {ftmp} = zz_object_get_field(&{boxed}, \"{fname}\");\n"
                    ));
                    // Nested boxed object → raw nested struct temp, then copy.
                    let ntmp = names.fresh("_unbox_n");
                    let nctype = format!("zz_struct_{}", mangle(inner));
                    out.push_str(&format!("    {nctype} {ntmp};\n"));
                    self.emit_unbox_struct(inner, &ftmp, &ntmp, names, out);
                    out.push_str(&format!("    {acc} = {ntmp};\n"));
                }
                _ => {}
            }
        }
    }

    /// Box a binary operand that lowers to a raw unboxed struct into a
    /// runtime object; pass through everything else unchanged. `emitted`
    /// is the already scalar-boxed operand text (identity for structs).
    fn box_struct_operand(
        &self,
        e: &Expr,
        emitted: String,
        raw: &str,
        names: &mut NameCtx,
        out: &mut String,
    ) -> String {
        if let Some(sname) = self.struct_box_name(e, names) {
            self.box_raw_struct(&sname, raw, names, out)
        } else {
            emitted
        }
    }

    /// Resolve a multi-segment `Path` callee into a method-dispatch pair
    /// `(impl_method_full_name, receiver_expr)`. Returns `None` when the
    /// path is not a recognized struct-method call (e.g. it's a static
    /// helper like `mod.helper()` — those still go through the regular
    /// `reachable_funcs` path).
    ///
    /// Algorithm:
    ///   1. Walk the path from the end to find the longest prefix that
    ///      names a registered local variable. This handles cases like
    ///      `mod.rect.area()` where the local is `rect` (registered
    ///      under the bare name, not the module-prefixed form).
    ///   2. Once the local is found, look up its C type in `NameCtx`. If
    ///      the type is a struct, build the impl-method name
    ///      `<StructType>.<tail_method>` and look it up in
    ///      `reachable_funcs`.
    ///   3. As a fallback, if no local is found but the second-to-last
    ///      path segment matches a struct type name (e.g. `mod.Rectangle
    ///      .area()`), try `<StructType>.<method>`. This is uncommon
    ///      but lets the static-form `Rectangle.area(...)` work in
    ///      fixtures where the type name is used directly.
    pub(super) fn resolve_path_receiver(
        &self,
        parts: &[String],
        names: &NameCtx,
    ) -> Option<(String, Expr)> {
        if parts.len() < 2 {
            return None;
        }
        let method = parts.last().unwrap().clone();

        // Strategy 1: longest-prefix local match.
        // parts = ["mod", "rect", "area"] → try "mod.rect" (no), then
        // "rect" (yes, since locals are registered under the bare name
        // and not the module-prefixed form). The matched lookup key is
        // either the joined prefix or the last segment of the prefix
        // — whichever hits a registered local.
        for split in (1..parts.len() - 1).rev() {
            let recv_parts = &parts[..split];
            let recv_joined = recv_parts.join(".");
            let recv_tail = recv_parts.last().unwrap().clone();
            let (matched_key, matched_tail) = if names.lookup(&recv_joined).is_some() {
                (recv_joined, recv_tail)
            } else if names.lookup(&recv_tail).is_some() {
                (recv_tail.clone(), recv_tail)
            } else {
                continue;
            };
            let recv_type = names.lookup_type(&matched_key)?;
            let unmangled = self
                .struct_name_from_c_type(recv_type)
                .and_then(|mangled| {
                    self.tp
                        .structs
                        .keys()
                        .find(|k| mangle(k) == *mangled)
                        .cloned()
                })
                .or_else(|| {
                    // Boxed receiver: resolve through the checker type.
                    let span = zz_frontend::span::Span::new(0, 0);
                    self.checker_struct_of(names, &matched_tail, Some(span))
                })?;
            if let Some((impl_name, path)) = self.struct_method_target(&unmangled, &method) {
                let span = zz_frontend::span::Span::new(0, 0);
                let recv_expr = if path.is_empty() {
                    Expr::Ident {
                        name: matched_tail,
                        span,
                    }
                } else {
                    let mut rparts = vec![matched_tail];
                    rparts.extend(path);
                    Expr::Path {
                        parts: rparts,
                        span,
                    }
                };
                return Some((impl_name, recv_expr));
            }
        }

        // Strategy 2: type-name-as-receiver (static form).
        // parts = ["mod", "Rectangle", "area"] → try "mod.Rectangle.area"
        // and "Rectangle.area" against `reachable_funcs` directly. This
        // is uncommon but lets the static-form `Rectangle.area(...)`
        // work in fixtures where the type name is used directly.
        for split in 1..parts.len() - 1 {
            let recv_joined = parts[..split].join(".");
            let impl_name = format!("{recv_joined}.{method}");
            if self.reachable_funcs.contains(&impl_name) {
                let span = zz_frontend::span::Span::new(0, 0);
                let recv_expr = Expr::Ident {
                    name: "self".to_string(),
                    span,
                };
                return Some((impl_name, recv_expr));
            }
        }

        None
    }

    /// Append one array/tuple element, boxing it into a `zz_value` first.
    /// Unboxed structs (raw C values: literals, locals, field reads) route
    /// through `emit_boxed_value` instead of emitting; raw scalars box via
    /// the operand helper; already-boxed expressions pass through unchanged.
    /// Without this, `(int, UnboxedStruct)` tuples and `[Point{...}]`
    /// arrays hand a raw C struct to `zz_vec_append(zz_value)` and the C
    /// build fails.
    ///
    /// The boxed object is freshly constructed per element, and
    /// `zz_vec_append` clones for store — so the temp is released right
    /// after the append statement. Without the release, every iteration
    /// of a loop building such tuples retains one object (~0.5KB/draw
    /// for a 4-int struct). The release only fires for temp names
    /// `emit_boxed_value` created (inline fallbacks like `zz_unit()` are
    /// appended bare, and borrowed locals never reach this branch).
    pub(super) fn append_container_item(
        &self,
        arr_var: &str,
        item: &Expr,
        names: &mut NameCtx,
        out: &mut String,
    ) {
        let unboxed: Option<String> = match self.ty_at(names, item.span()) {
            Some(zz_checker::Type::Struct(s, _)) if self.is_unboxed_struct(s) => Some(s.clone()),
            _ => None,
        };
        if let Some(sname) = unboxed {
            let tmp = self.emit_boxed_value(&sname, item, names, out);
            if is_simple_ident(&tmp) {
                out.push_str(&format!(
                    "    {{ int _e = 0; zz_vec_append({arr_var}, {tmp}, &_e); zz_release(&{tmp}); }}\n"
                ));
            } else {
                out.push_str(&format!(
                    "    {{ int _e = 0; zz_vec_append({arr_var}, {tmp}, &_e); }}\n"
                ));
            }
            return;
        }
        let item_val = self.emit_expr(item, names, out);
        // Auto-box if needed (historical Ident fast path, preserved).
        let boxed = if let Expr::Ident { name: n, .. } = item {
            auto_box(&item_val, names.lookup_type(n))
        } else {
            box_scalar_operand(item, names, &item_val)
        };
        out.push_str(&format!(
            "    {{ int _e = 0; zz_vec_append({arr_var}, {boxed}, &_e); }}\n"
        ));
    }

    /// Box an index expression to a `zz_value` for `idx` arguments. Scalar
    /// locals (int64_t/double/bool) and raw arithmetic need an explicit box;
    /// already-boxed expressions pass through unchanged.
    pub(super) fn box_index_arg(&self, e: &Expr, emitted: String, names: &NameCtx) -> String {
        if let Expr::Ident { name, .. } = e {
            return auto_box(&emitted, names.lookup_type(name));
        }
        if let Expr::Path { parts, .. } = e {
            let joined = parts.join(".");
            if let Some(ty) = names.lookup_type(&joined) {
                return auto_box(&emitted, Some(ty));
            }
        }
        if emitted.starts_with("(int64_t)(") || emitted.starts_with("(int64_t)") {
            return format!("zz_int({emitted})");
        }
        if emitted.starts_with("(double)(") {
            return format!("zz_float({emitted})");
        }
        if emitted.starts_with("(bool)(") {
            return format!("zz_bool({emitted})");
        }
        emitted
    }

    /// Box a raw-scalar index-store RHS to a `zz_value` before storing.
    /// Extracted from the `obj[idx] = v` lowering so compound assignment
    /// (`obj[idx] OP= v`) boxes identically.
    pub(super) fn box_index_store_value(
        &self,
        value: &Expr,
        val: String,
        names: &NameCtx,
    ) -> String {
        let ast_says_scalar = super::expr_emits_raw_scalar(value);
        let ident = match value {
            Expr::Ident { name, .. } => Some(name.as_str()),
            _ => None,
        };
        let val_is_actually_scalar =
            crate::lower::context::emitted_is_raw_scalar(&val, names, ident)
                || (ast_says_scalar && !val.starts_with("zz_"));
        let value_is_scalar = ast_says_scalar || val_is_actually_scalar;
        if value_is_scalar {
            if let Expr::Ident { name, .. } = value {
                let name_str = name.clone();
                auto_box(&val, names.lookup_type(&name_str))
            } else if let Expr::Path { parts, .. } = value {
                let joined = parts.join(".");
                auto_box(&val, names.lookup_type(&joined))
            } else if val.starts_with("(double)(") {
                format!("zz_float({val})")
            } else if val.starts_with("(bool)(") {
                format!("zz_bool({val})")
            } else if val_is_actually_scalar {
                // Genuinely raw scalar (int ident/arithmetic):
                // box it. (String concats over string idents
                // trip the AST-only check above but lower to
                // a zz_value — wrapping one in zz_int is a C
                // type error.)
                format!("zz_int({val})")
            } else {
                val
            }
        } else {
            val
        }
    }

    pub(super) fn emit_str_literal(&self, s: &str) -> String {
        // NUL-containing literals cannot use `zz_str_static`: it measures
        // with `strlen`, truncating at the first NUL (so `"\x00"` became
        // `""` and `str.contains(x, "\x00")` was always true). Emit a
        // length-aware `zz_str_new` instead, with 3-digit octal escapes
        // (unambiguous in C no matter what follows) and the true length.
        if s.contains('\0') {
            let mut o = String::from("\"");
            for c in s.chars() {
                match c {
                    '"' => o.push_str("\\\""),
                    '\\' => o.push_str("\\\\"),
                    '\n' => o.push_str("\\n"),
                    '\r' => o.push_str("\\r"),
                    '\t' => o.push_str("\\t"),
                    c if (c as u32) < 32 => o.push_str(&format!("\\{:03o}", c as u32)),
                    c => o.push(c),
                }
            }
            o.push('"');
            return format!("zz_str_new({o}, {})", s.len());
        }
        let mut o = String::from("\"");
        let mut it = s.chars().peekable();
        while let Some(c) = it.next() {
            match c {
                '"' => o.push_str("\\\""),
                '\\' => o.push_str("\\\\"),
                '\n' => o.push_str("\\n"),
                '\r' => o.push_str("\\r"),
                '\t' => o.push_str("\\t"),
                c if (c as u32) < 32 => {
                    // C `\x` escapes consume ALL following hex digits, so
                    // `\x1f` + `1` would compile as `\x1f1` (out of range).
                    // Close + reopen the literal when a hex digit follows;
                    // adjacent literals concatenate in C.
                    o.push_str(&format!("\\x{:02x}", c as u32));
                    if matches!(it.peek(), Some(n) if n.is_ascii_hexdigit()) {
                        o.push_str("\" \"");
                    }
                }
                c => o.push(c),
            }
        }
        o.push('"');
        format!("zz_str_static({o})")
    }

    /// Escape a raw string for embedding as a C string literal body
    /// (without the `zz_str_static` wrapper).
    fn c_escape(s: &str) -> String {
        let mut o = String::new();
        let mut it = s.chars().peekable();
        while let Some(c) = it.next() {
            match c {
                '"' => o.push_str("\\\""),
                '\\' => o.push_str("\\\\"),
                '\n' => o.push_str("\\n"),
                '\r' => o.push_str("\\r"),
                '\t' => o.push_str("\\t"),
                c if (c as u32) < 32 => {
                    // Same greedy-`\x` split as emit_str_literal above.
                    o.push_str(&format!("\\x{:02x}", c as u32));
                    if matches!(it.peek(), Some(n) if n.is_ascii_hexdigit()) {
                        o.push_str("\" \"");
                    }
                }
                c => o.push(c),
            }
        }
        o
    }

    /// Lower `sqlz.query(sql)` / `sqlz.exec(sql)` (+ `db.*` alias) to
    /// zero-alloc C FFI: static template with `{expr}` → `?N`, bound exprs
    /// collected into a binds array, then the native-convention
    /// `zz_db_query` / `zz_db_exec` (db, sql_str, binds_array) which call
    /// `sqlite3_prepare_v2` + `sqlite3_bind_*` + `sqlite3_step`.
    /// The receiver arrives separately (`method_receiver`); `sql_args`
    /// holds the user args ([sql]).
    fn emit_db_call(
        &self,
        cname: &str,
        method_receiver: Option<&Expr>,
        sql_args: Vec<&Expr>,
        names: &mut NameCtx,
        out: &mut String,
    ) -> String {
        // Free-form static calls carry the handle first (`sqlz.query(db,
        // sql)`, `pg.exec(db, sql)`); method form carries it as the
        // receiver. A single bare SQL arg keeps the old unit-receiver
        // shape (invalid at runtime — the handle tags mismatch — exactly
        // like the VM's expect_db error, minus the message).
        let (recv_expr, sql_args): (Option<Expr>, Vec<&Expr>) = match method_receiver {
            Some(_) => (None, sql_args),
            None if sql_args.len() >= 2 => {
                let mut it = sql_args.into_iter();
                let db = it.next().map(|e| (*e).clone());
                (db, it.collect())
            }
            None => (None, sql_args),
        };
        let recv_c = match (method_receiver, recv_expr.as_ref()) {
            (Some(r), _) => {
                let raw = self.emit_expr(r, names, out);
                match r {
                    Expr::Ident { name, .. } => auto_box(&raw, names.lookup_type(name)),
                    _ => raw,
                }
            }
            (None, Some(db)) => {
                let raw = self.emit_expr(db, names, out);
                match db {
                    Expr::Ident { name, .. } => auto_box(&raw, names.lookup_type(name)),
                    _ => raw,
                }
            }
            (None, None) => "zz_unit()".to_string(),
        };
        // Split SQL into template + bound exprs.
        let (template, bound): (String, Vec<Expr>) = match sql_args.first() {
            Some(Expr::Fmt { parts, .. }) => {
                let mut t = String::new();
                let mut b = Vec::new();
                let mut n = 0u32;
                for p in parts.iter() {
                    match p {
                        FmtPart::Text(s) => t.push_str(s),
                        FmtPart::Expr(e, _) => {
                            n += 1;
                            t.push_str(&format!("?{n}"));
                            b.push((**e).clone());
                        }
                    }
                }
                (t, b)
            }
            Some(Expr::Paren { expr, .. }) => match expr.as_ref() {
                Expr::Fmt { parts, .. } => {
                    let mut t = String::new();
                    let mut b = Vec::new();
                    let mut n = 0u32;
                    for p in parts.iter() {
                        match p {
                            FmtPart::Text(s) => t.push_str(s),
                            FmtPart::Expr(e, _) => {
                                n += 1;
                                t.push_str(&format!("?{n}"));
                                b.push((**e).clone());
                            }
                        }
                    }
                    (t, b)
                }
                other => (self.sql_text_of(other), Vec::new()),
            },
            Some(other) => (self.sql_text_of(other), Vec::new()),
            None => (String::new(), Vec::new()),
        };
        let n = bound.len();
        // Static template as a zz_value string (avoids raw char* in the
        // native call convention) + binds collected into a zz array.
        let tvar = names.fresh("zz_sql");
        out.push_str(&format!(
            "    zz_value {tvar} = zz_str_static(\"{}\");\n",
            Self::c_escape(&template)
        ));
        // Emit bound values as zz_value temporaries.
        let mut bvars: Vec<String> = Vec::with_capacity(n);
        for (i, b) in bound.iter().enumerate() {
            let raw = self.emit_expr(b, names, out);
            let boxed = match b {
                Expr::Ident { name, .. } => auto_box(&raw, names.lookup_type(name)),
                Expr::Path { parts, .. } => {
                    let joined = parts.join(".");
                    auto_box(&raw, names.lookup_type(&joined))
                }
                _ => box_scalar_operand(b, names, &raw),
            };
            let bv = names.fresh("zz_bind");
            out.push_str(&format!("    zz_value {bv} = {boxed};\n"));
            let _ = i;
            bvars.push(bv);
        }
        let arr = names.fresh("zz_binds");
        if n == 0 {
            out.push_str(&format!("    zz_value {arr} = zz_array_new();\n"));
        } else {
            // Build the binds array via zz_vec_push chain (copy-on-write
            // safe: each push returns the new array).
            out.push_str(&format!("    zz_value {arr} = zz_array_new();\n"));
            for bv in &bvars {
                out.push_str(&format!(
                    "    {{ int _e = 0; {arr} = zz_vec_push({arr}, {bv}, &_e); }}\n"
                ));
            }
        }
        let is_query = matches!(
            cname,
            "sqlz.query"
                | "std.sqlz.query"
                | "db.query"
                | "std.db.query"
                | "pg.query"
                | "std.sqlz.postgres.query"
        );
        let rt = if is_query {
            "zz_db_query"
        } else {
            "zz_db_exec"
        };
        // Native convention (db, sql_str, binds_array) → route through
        // zz_call_native3 so err plumbing matches every other native.
        format!("zz_call_native3({rt}, {recv_c}, {tvar}, {arr})")
    }

    /// Best-effort static text of a non-Fmt SQL arg (plain string literal
    /// or anything else — the latter lowers to "" and the runtime reports
    /// the SQLite error verbatim).
    fn sql_text_of(&self, e: &Expr) -> String {
        match e {
            Expr::Str { value, .. } => value.clone(),
            _ => String::new(),
        }
    }

    /// Emit a `match scrutinee { ... }` as an if/else chain on the tag,
    /// binding the payload in each arm. Shared by `Expr::Match` and
    /// desugared `Expr::IfLet`.
    /// Recursively bind a pattern to a scrutinee value. Used for nested
    /// variant patterns like `.some(.some(v))` where each inner variant
    /// needs tag-checking and payload extraction.
    /// Returns the number of open if-blocks that must be closed AFTER the arm body.
    pub(super) fn emit_pattern_bind(
        &self,
        pat: &Pattern,
        scrut: &str,
        names: &mut NameCtx,
        out: &mut String,
    ) -> usize {
        match pat {
            Pattern::Binding { name } => {
                // Green: arm bindings live in frame cells (a yield in
                // the arm body resumes past this declaration).
                if self.green_active() {
                    let (ptr, deref, n) = self.green_cell(names, "zz_value", true, out);
                    out.push_str(&format!("        {deref} = {scrut};\n"));
                    names.enter_cell(&name.name, &ptr, &deref, "zz_value", n);
                } else {
                    let cid = names.enter(&name.name);
                    out.push_str(&format!("        zz_value {cid} = {scrut};\n"));
                }
                0
            }
            Pattern::Variant { name, arg, .. } => {
                let tag_check = match name.as_str() {
                    "ok" => "ZZ_RESULT_OK",
                    "err" => "ZZ_RESULT_ERR",
                    "some" => "ZZ_OPTION_SOME",
                    "none" => "ZZ_OPTION_NONE",
                    _ => return 0,
                };
                let extractor = match name.as_str() {
                    "ok" => "zz_match_ok",
                    "err" => "zz_match_err",
                    "some" => "zz_match_some",
                    _ => return 0,
                };
                out.push_str(&format!("        if ({scrut}.tag == {tag_check}) {{\n"));
                let inner_open = if let Some(inner) = arg {
                    let payload_tmp: String = if self.green_active() {
                        let (_, deref, _) = self.green_cell(names, "zz_value", false, out);
                        out.push_str(&format!("            {deref} = {extractor}({scrut});\n"));
                        deref
                    } else {
                        let payload_tmp = names.fresh("_payload");
                        out.push_str(&format!(
                            "            zz_value {payload_tmp} = {extractor}({scrut});\n"
                        ));
                        payload_tmp
                    };
                    self.emit_pattern_bind(inner, &payload_tmp, names, out)
                } else {
                    0
                };
                // Don't close this block yet — the arm body must be inside it.
                1 + inner_open
            }
            _ => 0,
        }
    }

    pub(super) fn emit_match(
        &self,
        scrutinee: &Expr,
        arms: &[MatchArm],
        names: &mut NameCtx,
        out: &mut String,
    ) -> String {
        let scrut_val = self.emit_expr(scrutinee, names, out);
        // Green: scrutinee/result cross arm yields (resume may land
        // inside an arm); plain path keeps stack temps.
        let green_match = self.green_active();
        let scrut_tmp: String = if green_match {
            let (_, deref, _) = self.green_cell(names, "zz_value", false, out);
            deref
        } else {
            let scrut_tmp = names.fresh("_match");
            out.push_str(&format!("    zz_value {scrut_tmp} = zz_unit();\n"));
            scrut_tmp
        };
        let boxed = box_scalar_operand(scrutinee, names, &scrut_val);
        out.push_str(&format!("    {scrut_tmp} = {boxed};\n"));

        let scrut_type = scalar_operand_type(scrutinee, names);
        let scrut_raw = scalar_operand_c(scrutinee, names).unwrap_or_else(|| scrut_val.clone());

        let result_tmp: String = if green_match {
            let (_, deref, _) = self.green_cell(names, "zz_value", false, out);
            out.push_str(&format!("    {deref} = zz_unit();\n"));
            deref
        } else {
            let result_tmp = names.fresh("_mresult");
            out.push_str(&format!("    zz_value {result_tmp} = zz_unit();\n"));
            result_tmp
        };

        // Desugar or-patterns (`a | b`) into separate arms sharing the
        // same body/guard, expanding nested ors (variant args, tuples).
        fn expand_pat(pat: &Pattern) -> Vec<Pattern> {
            match pat {
                Pattern::Or { pats, .. } => pats.iter().flat_map(expand_pat).collect(),
                Pattern::Variant {
                    name,
                    arg: Some(a),
                    span,
                } => {
                    let inner = expand_pat(a);
                    if inner.len() <= 1 {
                        vec![pat.clone()]
                    } else {
                        inner
                            .into_iter()
                            .map(|e| Pattern::Variant {
                                name: name.clone(),
                                arg: Some(Box::new(e)),
                                span: *span,
                            })
                            .collect()
                    }
                }
                Pattern::Variant { .. } => vec![pat.clone()],
                Pattern::Tuple { pats, span } => {
                    let mut acc: Vec<Vec<Pattern>> = vec![Vec::new()];
                    for p in pats {
                        let exp = expand_pat(p);
                        let mut next = Vec::new();
                        for prefix in &acc {
                            for e in &exp {
                                let mut v = prefix.clone();
                                v.push(e.clone());
                                next.push(v);
                            }
                        }
                        acc = next;
                    }
                    if acc.len() <= 1 {
                        vec![pat.clone()]
                    } else {
                        acc.into_iter()
                            .map(|v| Pattern::Tuple {
                                pats: v,
                                span: *span,
                            })
                            .collect()
                    }
                }
                _ => vec![pat.clone()],
            }
        }
        let mut flat: Vec<MatchArm> = Vec::new();
        for arm in arms {
            for p in expand_pat(&arm.pat) {
                flat.push(MatchArm {
                    pat: p,
                    guard: arm.guard.clone(),
                    body: arm.body.clone(),
                    span: arm.span,
                });
            }
        }

        for (i, arm) in flat.iter().enumerate() {
            let is_last_arm = i == flat.len() - 1;
            let arm_needs_else_prefix = i > 0;
            let arm_closes_block = match &arm.pat {
                Pattern::Binding { .. } | Pattern::Wildcard { .. } | Pattern::Variant { .. } => {
                    arm.guard.is_none() && is_last_arm
                }
                _ => false,
            };

            match &arm.pat {
                Pattern::Variant { name, arg, .. } => {
                    // Lexical scope for arm bindings (same push/pop_scope
                    // discipline as value blocks).
                    names.push_scope();
                    let tag_check = match name.as_str() {
                        "ok" => "ZZ_RESULT_OK",
                        "err" => "ZZ_RESULT_ERR",
                        "some" => "ZZ_OPTION_SOME",
                        "none" => "ZZ_OPTION_NONE",
                        _ => continue,
                    };
                    let extractor = match name.as_str() {
                        "ok" => "zz_match_ok",
                        "err" => "zz_match_err",
                        "some" => "zz_match_some",
                        _ => "",
                    };

                    let cond = format!("{scrut_tmp}.tag == {tag_check}");
                    let full_cond = if let Some(guard_expr) = &arm.guard {
                        let guard_c = emit_guard_expr(guard_expr, names, &scrut_raw, scrut_type);
                        format!("{cond} && zz_truthy({guard_c})")
                    } else {
                        cond
                    };

                    if arm_needs_else_prefix {
                        out.push_str(&format!("    }} else if ({full_cond}) {{\n"));
                    } else {
                        out.push_str(&format!("    if ({full_cond}) {{\n"));
                    }

                    // Extract the payload from this variant arm, then
                    // recursively handle the inner pattern (which may be
                    // another variant pattern for nested matching).
                    let mut inner_open = 0;
                    if let Some(arg_pat) = arg {
                        let payload_tmp: String = if self.green_active() {
                            let (_, deref, _) = self.green_cell(names, "zz_value", false, out);
                            out.push_str(&format!("        {deref} = {extractor}({scrut_tmp});\n"));
                            deref
                        } else {
                            let payload_tmp = names.fresh("_payload");
                            out.push_str(&format!(
                                "        zz_value {payload_tmp} = {extractor}({scrut_tmp});\n"
                            ));
                            payload_tmp
                        };
                        inner_open = self.emit_pattern_bind(arg_pat, &payload_tmp, names, out);
                    }

                    let arm_val = self.emit_tail_value(&arm.body, names, out);
                    out.push_str(&format!("        {result_tmp} = {arm_val};\n"));
                    // Close any nested pattern if-blocks.
                    for _ in 0..inner_open {
                        out.push_str("        }\n");
                    }
                    names.pop_scope();
                    if arm_closes_block {
                        out.push_str("    }\n");
                    }
                }
                Pattern::Binding { name } => {
                    // Lexical scope for the arm binding (same
                    // push/pop_scope discipline as value blocks).
                    names.push_scope();
                    // Green: the binding outlives a suspend in the arm body
                    // (resume jumps over this declaration) — frame cell,
                    // mirroring `emit_pattern_bind`.
                    if self.green_active() {
                        let (ptr, deref, n) = self.green_cell(names, "zz_value", true, out);
                        out.push_str(&format!("        {deref} = {scrut_tmp};\n"));
                        names.enter_cell(&name.name, &ptr, &deref, "zz_value", n);
                    } else {
                        let cid = names.enter(&name.name);
                        out.push_str(&format!("        zz_value {cid} = {scrut_tmp};\n"));
                    }

                    if let Some(guard_expr) = &arm.guard {
                        let guard_c = emit_guard_expr(guard_expr, names, &scrut_raw, scrut_type);
                        if arm_needs_else_prefix {
                            out.push_str(&format!("    }} else if ({guard_c}) {{\n"));
                        } else {
                            out.push_str(&format!("    if ({guard_c}) {{\n"));
                        }
                    } else {
                        if arm_needs_else_prefix {
                            out.push_str("    } else {\n");
                        } else {
                            out.push_str("    {\n");
                        }
                    }
                    let arm_val = self.emit_tail_value(&arm.body, names, out);
                    out.push_str(&format!("        {result_tmp} = {arm_val};\n"));
                    names.pop_scope();
                    if arm_closes_block {
                        out.push_str("    }\n");
                    }
                }
                Pattern::Wildcard { .. } => {
                    // Lexical scope for declarations in the arm body.
                    names.push_scope();
                    if let Some(guard_expr) = &arm.guard {
                        let guard_c = emit_guard_expr(guard_expr, names, &scrut_raw, scrut_type);
                        if arm_needs_else_prefix {
                            out.push_str(&format!("    }} else if ({guard_c}) {{\n"));
                        } else {
                            out.push_str(&format!("    if ({guard_c}) {{\n"));
                        }
                    } else {
                        if arm_needs_else_prefix {
                            out.push_str("    } else {\n");
                        } else {
                            out.push_str("    {\n");
                        }
                    }
                    let arm_val = self.emit_tail_value(&arm.body, names, out);
                    out.push_str(&format!("        {result_tmp} = {arm_val};\n"));
                    names.pop_scope();
                    if arm_closes_block {
                        out.push_str("    }\n");
                    }
                }
                Pattern::Literal { value, .. } => {
                    // Lexical scope for declarations in the arm body.
                    names.push_scope();
                    let lit_c = match value {
                        zz_frontend::ast::Lit::Int(v) => format!("zz_int({v})"),
                        zz_frontend::ast::Lit::Float(v) => {
                            let s = if *v == v.floor() && v.abs() < 1e15 {
                                format!("{v:.1}")
                            } else {
                                format!("{v}")
                            };
                            format!("zz_float({s})")
                        }
                        zz_frontend::ast::Lit::Bool(v) => {
                            format!("zz_bool({})", if *v { "true" } else { "false" })
                        }
                        zz_frontend::ast::Lit::Str(v) => self.emit_str_literal(v),
                    };
                    let cond = format!("zz_truthy(zz_binop(ZZOP_EQ, {scrut_tmp}, {lit_c}))");
                    let full_cond = if let Some(guard_expr) = &arm.guard {
                        let guard_c = emit_guard_expr(guard_expr, names, &scrut_raw, scrut_type);
                        format!("{cond} && zz_truthy({guard_c})")
                    } else {
                        cond
                    };
                    if arm_needs_else_prefix {
                        out.push_str(&format!("    }} else if ({full_cond}) {{\n"));
                    } else {
                        out.push_str(&format!("    if ({full_cond}) {{\n"));
                    }
                    let arm_val = self.emit_tail_value(&arm.body, names, out);
                    out.push_str(&format!("        {result_tmp} = {arm_val};\n"));
                    names.pop_scope();
                    if arm_closes_block {
                        out.push_str("    }\n");
                    }
                }
                Pattern::Tuple { .. } => {
                    out.push_str("    // unsupported match pattern (tuple)\n");
                }
                Pattern::Or { .. } => {
                    // Unreachable: or-patterns are expanded into separate
                    // arms above.
                    out.push_str("    // unsupported match pattern (or)\n");
                }
            }
        }
        result_tmp
    }
}
