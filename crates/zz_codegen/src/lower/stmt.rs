//! Statement lowering: let bindings, assignments, if/else, match, loops,
//! returns, and defer.

use zz_frontend::ast::{Block, Expr, Stmt};

use super::*;

impl Lowerer {
    pub(super) fn emit_stmt(
        &self,
        stmt: &Stmt,
        names: &mut NameCtx,
        out: &mut String,
        is_tail: bool,
    ) {
        match stmt {
            Stmt::Decl { name, value, .. } => {
                // Look up the type of the initializer expression
                let (ctype, checker_ty) = if let Some(ty) = self.tp.types.get(&value.span()) {
                    (ty_to_ctype(ty), Some(ty.clone()))
                } else {
                    ("zz_value".to_string(), None) // fallback
                };
                // Store the checker type for method dispatch (e.g., to distinguish
                // str.contains from vec.contains when the receiver is a local).
                if let Some(ct) = checker_ty {
                    names.checker_types.insert(name.name.clone(), ct);
                }

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

                // Emit the RHS expression FIRST, while the variable name still
                // resolves to the PREVIOUS scope entry (if any). This is critical
                // for redeclarations like `total := total + item` inside loop
                // bodies: the RHS must reference the old `total`, not the new one.
                let val = self.emit_expr(value, names, out);

                // NOW enter the new scope entry with the correct C type.
                // For scalars, use enter_with_type so that any subsequent code
                // (e.g. the loop body in a for-loop) sees the correct type
                // during scalar_operand_type lookups.
                let cid = if matches!(ctype.as_str(), "int64_t" | "double" | "bool") {
                    names.enter_with_type(&name.name, &ctype)
                } else {
                    names.enter(&name.name)
                };
                // Update the name ctx with the correct type (covers non-scalar
                // types and any type not yet set).
                if let Some(vec) = names.stack.get_mut(&name.name) {
                    if let Some((_, existing_ty)) = vec.last_mut() {
                        *existing_ty = ctype.clone();
                    }
                }
                // For scalar types, extract the underlying value from the zz_value
                // UNLESS the emitted expression is already unboxed (e.g., a binary
                // op on unboxed scalars produces a raw int64_t, not a zz_value).
                let val_is_unboxed = val.starts_with("(int64_t)(")
                    || val.starts_with("(double)(")
                    || val.starts_with("(bool)(");
                let final_val = match ctype.as_str() {
                    "int64_t" if !val_is_unboxed => format!("({val}).i"),
                    "double" if !val_is_unboxed => format!("({val}).f"),
                    "bool" if !val_is_unboxed => format!("({val}).b"),
                    _ => val,
                };
                out.push_str(&format!("    {ctype} {cid} = {final_val};\n"));
                // Track array literals so `len(v)` can fold to the arity.
                if let Expr::Array { elems, .. } = value {
                    names.set_array_len(&name.name, elems.len());
                }
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
                // Any reassignment breaks the literal-length association
                // until proven otherwise (re-inserted below for array
                // literals on the generic path).
                if let Expr::Ident { name, .. } = target {
                    names.invalidate_array_len(name);
                }
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
                //
                // `expr_emits_raw_scalar` only inspects the AST, so it can
                // disagree with the actual emitted form (e.g. when the
                // fast-path is bypassed and `zz_binop` is emitted instead
                // of raw arithmetic). Cross-check against the emitted
                // string to avoid assigning a zz_value to an int64_t.
                let ast_says_scalar = expr_emits_raw_scalar(value);
                let val_is_actually_scalar = val.starts_with("(int64_t)(")
                    || val.starts_with("(double)(")
                    || val.starts_with("(bool)(")
                    || (ast_says_scalar && !val.starts_with("zz_"));
                let value_is_scalar = ast_says_scalar || val_is_actually_scalar;
                let value_needs_unbox = !val_is_actually_scalar;
                match target {
                    Expr::Ident { name, .. } => {
                        if let Some(cid) = names.lookup(name.as_str()) {
                            // Check if the target variable is a scalar type
                            if let Some(ctype) = names.lookup_type(name.as_str()) {
                                match ctype {
                                    "int64_t" | "double" | "bool" => {
                                        if value_is_scalar && !value_needs_unbox {
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
                            // Re-associate the target with a fresh array
                            // literal length when the RHS is one.
                            if let Expr::Array { elems, .. } = value {
                                names.set_array_len(name, elems.len());
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
                                                field_name = parts[1]
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
                                                    f1 = parts[1],
                                                    f2 = parts[2]
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
                                        if value_is_scalar && !value_needs_unbox {
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
                    Expr::Index { obj, index, .. } => {
                        // `obj[idx] = v` — runtime-dispatched write (arrays/dicts).
                        let o = self.emit_expr(obj, names, out);
                        let i = self.emit_expr(index, names, out);
                        // Box a scalar index to a zz_value.
                        let i_boxed = self.box_index_arg(index, i, names);
                        // Box a raw-scalar RHS to a zz_value before storing.
                        let boxed_val = if value_is_scalar {
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
                            } else {
                                format!("zz_int({val})")
                            }
                        } else {
                            val.clone()
                        };
                        out.push_str(&format!(
                            "    {{ int _e = 0; zz_index_set({o}, {i_boxed}, {boxed_val}, &_e); }}\n"
                        ));
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
                    *self.void_context.borrow_mut() = true;
                    let val = self.emit_expr(e, names, out);
                    *self.void_context.borrow_mut() = false;
                    out.push_str(&format!("    (void)({val});\n"));
                } else if !is_leaf_expr(e) {
                    let _ = self.emit_expr(e, names, out);
                }
            }
            Stmt::Return { value, .. } => match value {
                Some(v) => {
                    let val = self.emit_expr(v, names, out);
                    let val = box_scalar_operand(v, names, &val);
                    out.push_str(&format!("    return {val};\n"));
                }
                None => out.push_str("    return zz_unit();\n"),
            },
            Stmt::For {
                vars,
                iter,
                body,
                span,
                ..
            } => {
                self.emit_for(vars, iter, body, *span, names, out);
            }
            Stmt::Break { .. } => out.push_str("    break;\n"),
            Stmt::Continue { .. } => out.push_str("    continue;\n"),
            Stmt::Defer { expr, .. } => {
                // `defer <expr>`: register the deferred expression with the
                // function's LIFO runner. The expression text (incl. any temp
                // setup it needs) is built now but emitted only at function
                // exit, so the deferred side effect runs in reverse order and
                // re-reads the live values at that point.
                let mut scratch = String::new();
                let val = self.emit_expr(expr, names, &mut scratch);
                let val = box_scalar_operand(expr, names, &val);
                let mut slots = self.defer_slots.borrow_mut();
                // Prefix the snippet with whatever setup `emit_block` would
                // have emitted inline (scratch holds the temp declarations).
                let snippet = if scratch.trim().is_empty() {
                    format!("(void)({val});")
                } else {
                    format!("{scratch}        (void)({val});")
                };
                let idx = slots.len();
                slots.push(snippet);
                drop(slots);
                out.push_str(&format!("    __defers[__defer_n++] = {idx};\n"));
            }
            Stmt::Destructure { .. } => {
                out.push_str("    // unsupported statement skipped\n");
            }
            Stmt::Func { .. } | Stmt::Struct { .. } | Stmt::Impl { .. } | Stmt::Import { .. } => {}
        }
    }

    pub(super) fn emit_for(
        &self,
        vars: &[zz_frontend::ast::Ident],
        iter: &Expr,
        body: &Block,
        span: zz_frontend::span::Span,
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
            // Loop-scoped sub-arena: loops whose body contains non-escaping
            // allocations get their own arena so the per-iteration reset can
            // reuse the buffer without touching objects an enclosing scope or
            // an outer loop iteration may have placed on the function arena.
            let loop_arena: Option<String> = if self.escape.loop_spans.contains(&span) {
                let ac = names.bump_counter();
                let name = format!("_loop_arena{ac}");
                // Estimate per-iteration allocation size to pre-size the arena.
                // Count allocating expressions in the body × 128 bytes each.
                let alloc_count = count_allocating_exprs(body);
                let arena_size = (alloc_count * 128).max(65536);
                out.push_str(&format!("    zz_arena {name};\n"));
                out.push_str(&format!("    zz_arena_init(&{name}, {arena_size});\n"));
                Some(name)
            } else {
                None
            };
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
            if let Some(ref name) = loop_arena {
                self.loop_arenas.borrow_mut().push(name.clone());
                *self.current_loop_arena.borrow_mut() = Some(name.clone());
            }
            for bstmt in &body.stmts {
                self.emit_stmt(bstmt, names, out, false);
            }
            if let Some(ref name) = loop_arena {
                self.loop_arenas.borrow_mut().pop();
                *self.current_loop_arena.borrow_mut() = self.loop_arenas.borrow().last().cloned();
                // Per-iteration reset: every arena allocation made inside this
                // iteration is reclaimed in O(1), so the sub-arena's buffer is
                // reused indefinitely without growing the heap footprint.
                out.push_str(&format!("    zz_arena_reset(&{name});\n"));
            }
            out.push_str("    }\n");
            if !end_is_scalar {
                out.push_str("    }\n}\n");
            }
            // Free the sub-arena's buffer once the loop completes.
            if let Some(ref name) = loop_arena {
                out.push_str(&format!("    zz_arena_destroy(&{name});\n"));
            }
            names.leave(v);
        } else {
            // For-in over an array or dict: evaluate the iterable, then
            // dispatch based on the runtime tag.
            let iter_val = self.emit_expr(iter, names, out);
            let iter_tmp = names.fresh("_iter");
            out.push_str(&format!("    zz_value {iter_tmp} = {iter_val};\n"));

            if vars.len() == 1 {
                // for x in <array|dict>: iterate array elements or dict keys.
                // Enter a scope so redeclarations inside the body (e.g.
                // `total := total + item`) don't leak past the loop boundary.
                names.push_scope();
                let v = &vars[0].name;
                let cid = names.enter(v);
                // Update type to zz_value (array elements / dict keys are boxed)
                if let Some(vec) = names.stack.get_mut(v) {
                    if let Some((_, existing_ty)) = vec.last_mut() {
                        *existing_ty = "zz_value".to_string();
                    }
                }
                let idx = names.fresh("_idx");
                let len = names.fresh("_len");
                out.push_str(&format!("    int64_t {idx} = 0;\n"));
                out.push_str(&format!(
                    "    int64_t {len} = ({iter_tmp}.tag == ZZ_ARRAY) ? (int64_t){iter_tmp}.arr->len\n\
                     : ({iter_tmp}.tag == ZZ_DICT) ? (int64_t){iter_tmp}.dict->len : 0;\n"
                ));
                out.push_str(&format!("    for (; {idx} < {len}; {idx}++) {{\n"));
                out.push_str(&format!(
                    "        zz_value {cid} = ({iter_tmp}.tag == ZZ_ARRAY)\n\
                     ? zz_clone({iter_tmp}.arr->items[{idx}])\n\
                     : (zz_value){{ZZ_STR, {{.s = {iter_tmp}.dict->entries[{idx}].key}}}};\n"
                ));
                for bstmt in &body.stmts {
                    self.emit_stmt(bstmt, names, out, false);
                }
                out.push_str("    }\n");
                names.pop_scope();
            } else if vars.len() == 2 {
                // for k, v in <dict>: iterate dict entries (key, value).
                names.push_scope();
                let k_name = &vars[0].name;
                let v_name = &vars[1].name;
                let k_cid = names.enter(k_name);
                let v_cid = names.enter(v_name);
                if let Some(vec) = names.stack.get_mut(k_name) {
                    if let Some((_, ty)) = vec.last_mut() {
                        *ty = "zz_value".to_string();
                    }
                }
                if let Some(vec) = names.stack.get_mut(v_name) {
                    if let Some((_, ty)) = vec.last_mut() {
                        *ty = "zz_value".to_string();
                    }
                }
                let idx = names.fresh("_idx");
                let len = names.fresh("_len");
                out.push_str(&format!("    int64_t {idx} = 0;\n"));
                out.push_str(&format!(
                    "    int64_t {len} = ({iter_tmp}.tag == ZZ_DICT) ? (int64_t){iter_tmp}.dict->len : 0;\n"
                ));
                out.push_str(&format!("    for (; {idx} < {len}; {idx}++) {{\n"));
                out.push_str(&format!(
                    "        zz_value {k_cid} = (zz_value){{ZZ_STR, {{.s = {iter_tmp}.dict->entries[{idx}].key}}}};\n"
                ));
                out.push_str(&format!(
                    "        zz_value {v_cid} = zz_clone({iter_tmp}.dict->entries[{idx}].val);\n"
                ));
                for bstmt in &body.stmts {
                    self.emit_stmt(bstmt, names, out, false);
                }
                out.push_str("    }\n");
                names.pop_scope();
            }
        }
    }

    pub(super) fn emit_block(&self, block: &Block, names: &mut NameCtx, out: &mut String) {
        // Control-flow boundary: drop literal-length knowledge from the
        // enclosing scope so `len(x)` folds never cross a merge point.
        names.clear_array_lens();
        // Scope `__tail` so inner block tail captures don't leak to outer scopes.
        let tail_saved = names.stack.get("__tail").map(|v| v.len()).unwrap_or(0);
        let n = block.stmts.len();
        for (i, stmt) in block.stmts.iter().enumerate() {
            let is_tail = i == n - 1;
            self.emit_stmt(stmt, names, out, is_tail);
        }
        // Restore `__tail` stack to pre-block depth.
        if let Some(v) = names.stack.get_mut("__tail") {
            v.truncate(tail_saved);
        }
    }

    /// Emit the function body block. Unlike `emit_block`, this preserves
    /// `__tail` entries so `last_stmt_value` can use them for implicit returns.
    pub(super) fn emit_func_block(&self, block: &Block, names: &mut NameCtx, out: &mut String) {
        names.clear_array_lens();
        let n = block.stmts.len();
        for (i, stmt) in block.stmts.iter().enumerate() {
            let is_tail = i == n - 1;
            self.emit_stmt(stmt, names, out, is_tail);
        }
    }

    /// The last statement of a function body — if it's an expression, that
    /// becomes the implicit return value. Handles tail calls, plain
    /// expression tails, and tail if/else (returning from each branch).
    pub(super) fn last_stmt_value(
        &self,
        block: &Block,
        names: &mut NameCtx,
        out: &mut String,
    ) -> Option<()> {
        if let Some(Stmt::Expr(e)) = block.stmts.last() {
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
            let val = box_scalar_operand(e, names, &val);
            out.push_str(&format!("    return {val};\n"));
            return Some(());
        }
        None
    }

    /// Emit an expression in return position, emitting `return <val>;`.
    /// If/else tails return from each branch.
    pub(super) fn emit_tail_expr(&self, e: &Expr, names: &mut NameCtx, out: &mut String) {
        match e {
            Expr::If {
                cond, then, els, ..
            } => {
                let c = self.emit_expr(cond, names, out);
                let c = box_scalar_operand(cond, names, &c);
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
}
