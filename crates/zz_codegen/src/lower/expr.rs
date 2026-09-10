//! Expression lowering: literals, binary ops, function calls, field access,
//! collections, variants, closures, and match expressions.

use zz_frontend::ast::{Expr, FmtPart, MatchArm, Param, Pattern};

use super::*;

impl Lowerer {
    pub(super) fn emit_expr(&self, e: &Expr, names: &mut NameCtx, out: &mut String) -> String {
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
                            // If format spec is present, use zz_to_str_fmt
                            if let Some(ref s) = spec {
                                acc = format!("zz_binop_cat({acc}, zz_str_owned(zz_to_str_fmt({boxed_v}, {spec_str})))",
                                    spec_str = format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\"")));
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
                        match ctype {
                            "int64_t" | "double" | "bool" => cid.to_string(),
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
                                let field_name = &parts[1];
                                // Wrap scalar fields in the correct boxing
                                // so the result is always a `zz_value`.
                                // The `auto_box` idempotency guard prevents
                                // double-boxing when callers also call auto_box.
                                let field_c_type = self
                                    .field_type_from_struct(base_type, field_name)
                                    .unwrap_or("zz_value");
                                let raw = format!("({base_name}).{field_name}");
                                match field_c_type {
                                    "int64_t" => return format!("zz_int({raw})"),
                                    "double" => return format!("zz_float({raw})"),
                                    "bool" => return format!("zz_bool({raw})"),
                                    // Non-scalar field (boxed): clone.
                                    _ => return format!("zz_clone({raw})"),
                                }
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
                    if let Some(base_name) = names.lookup(&parts[0]) {
                        if let Some(base_type) = names.lookup_type(&parts[0]) {
                            // For now, only handle 2-level deep fields
                            if parts.len() == 3 && self.is_struct_type_str(base_type) {
                                let field1 = &parts[1];
                                let field2 = &parts[2];
                                let raw = format!("(({base_name}).{field1}).{field2}");
                                // Walk the chain: base → field1 (struct) → field2 (scalar/struct)
                                // to derive the final field's C type for auto-boxing.
                                let field2_ctype =
                                    self.field_type_from_struct(base_type, field1).and_then(
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
                        // Evaluate the left side once and store in a temp to avoid
                        // double-evaluation (which would call side-effecting natives
                        // like `input()` twice).
                        let tmp = names.fresh("elvis");
                        out.push_str(&format!("    zz_value {tmp} = {l};\n"));
                        format!("zz_elvis({tmp}, {r})")
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
                        );
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
                                let boxed_l = box_scalar_operand(left, names, &l);
                                let boxed_r = box_scalar_operand(right, names, &r);
                                format!("zz_binop({cop}, {boxed_l}, {boxed_r})")
                            } else {
                                // Neither operand has a known scalar type in
                                // NameCtx, but struct field accesses like
                                // `(v0).width` are raw C scalars that need
                                // boxing for `zz_binop`. Use `box_scalar_operand`
                                // which recognizes the cast pattern.
                                let boxed_l = box_scalar_operand(left, names, &l);
                                let boxed_r = box_scalar_operand(right, names, &r);
                                format!("zz_binop({cop}, {boxed_l}, {boxed_r})")
                            }
                        } else {
                            // Comparisons / pow / etc: use the boxed path
                            // (result must be zz_value). Also handles
                            // struct field accesses that emit as raw C
                            // scalars.
                            let boxed_l = box_scalar_operand(left, names, &l);
                            let boxed_r = box_scalar_operand(right, names, &r);
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
            } => self.emit_call(callee, args, named, names, out),
            Expr::While {
                cond, body, span, ..
            } => {
                // Loops whose body contains non-escaping allocations get a
                // loop-scoped sub-arena, exactly like `for` loops: the buffer
                // is reset at the end of every iteration so per-iteration
                // allocations are reused instead of growing the heap.
                let loop_arena: Option<String> = if self.escape.loop_spans.contains(span) {
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
                let c = self.emit_expr(cond, names, out);
                let c = box_scalar_operand(cond, names, &c);
                out.push_str(&format!("        if (!zz_truthy({c})) break;\n"));
                // Loop body is never a function tail.
                for bstmt in &body.stmts {
                    self.emit_stmt(bstmt, names, out, false);
                }
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
                let c = self.emit_expr(cond, names, out);
                let c = box_scalar_operand(cond, names, &c);
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
                // If the block's last statement was captured into a __tail
                // temp (e.g. a match expression inside a match arm body),
                // return that value instead of unit.
                if let Some((tmp, _)) = names.stack.get("__tail").and_then(|s| s.last()) {
                    tmp.clone()
                } else {
                    "zz_unit()".to_string()
                }
            }
            Expr::Range { start, end, .. } => {
                let s = self.emit_expr(start, names, out);
                let en = self.emit_expr(end, names, out);
                format!("zz_range_build({s}, {en})")
            }
            Expr::Index { obj, index, .. } => {
                // `obj[idx]` — runtime-dispatched read (arrays/dicts).
                let o = self.emit_expr(obj, names, out);
                let i = self.emit_expr(index, names, out);
                // Box a scalar index (ident/raw-arith) to a zz_value.
                let i_boxed = self.box_index_arg(index, i, names);
                format!("zz_call_native2(zz_index_get, {o}, {i_boxed})")
            }
            Expr::Slice {
                obj, start, end, ..
            } => {
                // `obj[a:b]` — array/string slicing. Missing bounds lower to
                // unit (the C runtime interprets unit as "from 0" / "to end").
                let o = self.emit_expr(obj, names, out);
                let s = match start {
                    Some(e) => self.emit_expr(e, names, out),
                    None => "zz_unit()".to_string(),
                };
                let e = match end {
                    Some(e) => self.emit_expr(e, names, out),
                    None => "zz_unit()".to_string(),
                };
                format!("zz_call_native3(zz_slice_value, {o}, {s}, {e})")
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
                        let fval = self.emit_expr(fexpr, names, out);
                        // Box the field value if it's a scalar type
                        let boxed_fval = if let Some((_, field_type)) =
                            sig.fields.iter().find(|(n, _)| n == fname)
                        {
                            match field_type {
                                zz_checker::Type::Int => format!("zz_int({fval})"),
                                zz_checker::Type::Float => format!("zz_float({fval})"),
                                zz_checker::Type::Bool => format!("zz_bool({fval})"),
                                _ => fval,
                            }
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
            Expr::Field { obj, name, .. } => {
                // Determine if the object is boxed (zz_value) or unboxed (C struct).
                // For unboxed: emit C struct field access (e.g., "obj.field").
                // For boxed: emit zz_object_get_field(&obj, "field").
                let obj_val = self.emit_expr(obj, names, out);
                // Try to determine if the object type is an unboxed struct.
                let is_unboxed = if let Expr::Ident { name: obj_name, .. } = obj.as_ref() {
                    names
                        .lookup_type(obj_name)
                        .map(|t| t.starts_with("zz_struct_"))
                        .unwrap_or(false)
                } else if let Some(obj_span) = self.tp.types.get(&obj.span()) {
                    if let zz_checker::Type::Struct(sname) = obj_span {
                        self.is_unboxed_struct(sname)
                    } else {
                        false
                    }
                } else {
                    false
                };
                if is_unboxed {
                    // Unboxed struct: direct C field access, then auto-box
                    // scalar fields so the result is always a zz_value.
                    let raw = format!("({obj_val}).{name}");
                    // Derive the field C type from the parent object's struct type.
                    let field_ctype = self.tp.types.get(&obj.span()).and_then(|ot| {
                        if let zz_checker::Type::Struct(sname) = ot {
                            if let Some(sig) = self.tp.structs.get(sname) {
                                if let Some((_, ft)) = sig.fields.iter().find(|(n, _)| n == name) {
                                    return Some(self.type_to_c(ft));
                                }
                            }
                        }
                        None
                    });
                    auto_box(&raw, field_ctype.as_deref())
                } else {
                    // Boxed object: use runtime function
                    format!("zz_object_get_field(&{obj_val}, \"{name}\")")
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
                        let item_val = self.emit_expr(item, names, out);
                        let boxed = if let Expr::Ident { name: n, .. } = item {
                            auto_box(&item_val, names.lookup_type(n))
                        } else {
                            item_val
                        };
                        out.push_str(&format!(
                            "    {{ int _e = 0; zz_vec_append({arr_var}, {boxed}, &_e); }}\n"
                        ));
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
                    None => "zz_dict_new()".to_string(),
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
                match name.as_str() {
                    "some" => {
                        if let Some(a) = arg {
                            let inner = self.emit_expr(a, names, out);
                            format!("zz_variant_some({inner})")
                        } else {
                            "(zz_value){ZZ_OPTION_NONE, {0}}".to_string()
                        }
                    }
                    "none" => "(zz_value){ZZ_OPTION_NONE, {0}}".to_string(),
                    "ok" => {
                        if let Some(a) = arg {
                            let inner = self.emit_expr(a, names, out);
                            format!("zz_variant_ok({inner})")
                        } else {
                            "(zz_value){ZZ_RESULT_OK, {0}}".to_string()
                        }
                    }
                    "err" => {
                        if let Some(a) = arg {
                            let inner = self.emit_expr(a, names, out);
                            format!("zz_variant_err({inner})")
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
                } else {
                    out.push_str("    // unsupported comprehension iterable\n");
                }
                comp
            }
            Expr::Closure { params, body, .. } => {
                let mut defs = self.closure_defs.borrow_mut();
                let cid = defs.len().min(1_000_000);
                let body_c = self.emit_closure(params, body, cid, names, out);
                defs.push(body_c);
                // Emit a forward declaration so the closure is visible to
                // call sites that appear before the closure definition.
                self.closure_forward_decls.borrow_mut().push(format!(
                    "static zz_value zz_closure_{cid}(zz_value *args, size_t argc);\n"
                ));
                drop(defs);
                format!("zz_closure_make(zz_closure_{cid})")
            }
            _ => "zz_unit()".to_string(),
        }
    }

    /// Emit a C static function for a closure literal `|p1, p2| body` and
    /// return the closure's C body text. Param values arrive boxed in `args[]`;
    /// the body expression is lowered against them and returned.
    pub(super) fn emit_closure(
        &self,
        params: &[Param],
        body: &Expr,
        cid: usize,
        _outer_names: &mut NameCtx,
        _out: &mut String,
    ) -> String {
        let mut names = NameCtx::new();
        let mut o = String::new();
        o.push_str(&format!(
            "static zz_value zz_closure_{cid}(zz_value *args, size_t argc) {{\n"
        ));
        o.push_str("    (void)argc;\n");
        o.push_str("    zz_arena _arena;\n");
        o.push_str("    zz_arena_init(&_arena, 65536);\n");
        o.push_str("    int __defers[32];\n");
        o.push_str("    int __defer_n = 0;\n");
        for (i, p) in params.iter().enumerate() {
            let cid_enter = names.enter(&p.name.name);
            o.push_str(&format!("    zz_value {cid_enter} = args[{i}];\n"));
        }
        let mut body_out = String::new();
        let val = self.emit_expr(body, &mut names, &mut body_out);
        o.push_str(&body_out);
        let val = box_scalar_operand(body, &mut names, &val);
        {
            let mut slots = self.defer_slots.borrow_mut();
            if !slots.is_empty() {
                o.push_str("    for (int __dk = __defer_n - 1; __dk >= 0; __dk--) {\n");
                o.push_str("        switch (__defers[__dk]) {\n");
                let snap: Vec<String> = slots.drain(..).collect();
                for (idx, snippet) in snap.iter().enumerate() {
                    o.push_str(&format!("        case {idx}:\n"));
                    o.push_str(&snippet);
                    o.push_str("\n            break;\n");
                }
                o.push_str("        default: break;\n");
                o.push_str("        }\n");
                o.push_str("    }\n");
            }
        }
        o.push_str(&format!("    zz_arena_reset_trim(&_arena);\n"));
        o.push_str(&format!("    return {val};\n"));
        o.push_str("}\n");
        o
    }

    pub(super) fn emit_call(
        &self,
        callee: &Expr,
        args: &[Expr],
        named: &[(String, Expr)],
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
                    let first_ident_end = span.start + obj_name.len() as u32;
                    let first_ident_span =
                        zz_frontend::span::Span::new(span.start, first_ident_end);

                    // Struct method dispatch: if the local's C type is a
                    // struct (e.g. `zz_struct_mod__Rectangle`), look up
                    // `<StructType>.<method>` in `reachable_funcs` (impl
                    // methods are stored as `Type.method` using the
                    // un-mangled struct name like `mod.Rectangle`). This
                    // handles `rect.area()` regardless of whether `rect`
                    // is bare or module-prefixed.
                    let struct_dispatch: Option<(String, Expr)> =
                        if let Some(recv_type) = names.lookup_type(obj_name) {
                            self.struct_name_from_c_type(recv_type)
                                .and_then(|mangled_name| {
                                    // Find the un-mangled struct name (the
                                    // key in `tp.structs`) whose mangled C
                                    // form matches `mangled_name`.
                                    self.tp
                                        .structs
                                        .keys()
                                        .find(|k| mangle(k) == *mangled_name)
                                        .cloned()
                                })
                                .and_then(|unmangled| {
                                    let impl_name = format!("{unmangled}.{method}");
                                    if self.reachable_funcs.contains(&impl_name) {
                                        Some((
                                            impl_name,
                                            Expr::Ident {
                                                name: obj_name.clone(),
                                                span: first_ident_span,
                                            },
                                        ))
                                    } else {
                                        None
                                    }
                                })
                        } else {
                            None
                        };
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
                                _ => None,
                            });
                        // Also check the type checker's span_types map
                        // using the receiver's source span.
                        let span_type_ns = if let Some(zzty) = self.tp.types.get(&first_ident_span)
                        {
                            match zzty {
                                zz_checker::Type::Str => Some("str"),
                                zz_checker::Type::Array(_) => Some("vec"),
                                zz_checker::Type::Dict(_, _) => Some("dict"),
                                zz_checker::Type::Option(_) => Some("option"),
                                zz_checker::Type::Result(_, _) => Some("result"),
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
                            {
                                found_ns = type_ns;
                            } else if native_supported(&candidate) {
                                found_ns = type_ns;
                            }
                        }
                        // Fallback: generic namespace search (untyped
                        // receivers, e.g. variables without type annotations).
                        if found_ns.is_empty() {
                            let namespaces = ["vec", "str", "dict", "option", "result", "http"];
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
                            // Also try matching by native_impl — checks if there's
                            // a C runtime function registered for this method under
                            // any namespace.
                            let namespaces = ["vec", "str", "dict", "option", "result", "http"];
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
                    }
                } else {
                    // obj_name is NOT a local — it's a namespace like `vec`, `io`.
                    (parts.join("."), None)
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
                    if let Some((recv_cname, recv_expr)) = self.resolve_path_receiver(parts, names)
                    {
                        (recv_cname, Some(recv_expr))
                    } else {
                        (parts.join("."), None)
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

        let mut arg_items: Vec<String> = Vec::new();
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
            _ => false,
        };
        if let Some(ref recv) = method_receiver {
            let recv_val = self.emit_expr(recv, names, out);
            if recv_is_struct {
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
        for a in ordered_args {
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
                let ctype = self.tp.types.get(&obj.span()).and_then(|ot| {
                    if let zz_checker::Type::Struct(sname) = ot {
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
            // In void context (result discarded), use in-place mutation
            // instead of copy-on-write for vec.push.
            let effective_name = if *self.void_context.borrow() && impl_name == "zz_vec_push" {
                "zz_vec_append"
            } else {
                impl_name
            };
            // Arena-aware str_cast inside loops: allocate result on arena
            // to avoid heap malloc for intermediate string conversions.
            if effective_name == "zz_str_cast" {
                if let Some(ref arena) = *self.current_loop_arena.borrow() {
                    let a = &arg_items[0];
                    return format!("zz_str_cast_arena({a}, &(int){{0}}, &{arena})");
                }
            }
            return match arg_items.len() {
                1 => {
                    let a = &arg_items[0];
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
                _ => "zz_unit()".to_string(),
            };
        }

        // Reachable native without a C runtime impl (e.g. time.now_ms)
        // lowers to Unit.
        if is_native {
            let _ = (cname_for_native.is_empty(),);
            return "zz_unit()".to_string();
        }

        if self.reachable_funcs.contains(&cname_for_native) {
            let cf = format!("zz_fn_{}", mangle(&cname_for_native));
            // Impl methods: callee signature is
            // `zz_fn_X(<struct>* self, zz_value* args, size_t argc)`.
            // The receiver is passed as a pointer; the remaining args
            // are boxed zz_values in the args array.
            let is_impl_method = self
                .tp
                .funcs
                .get(&cname_for_native)
                .and_then(|sig| sig.params.first().map(|(_, t)| t.clone()))
                .map(|t| matches!(&t, zz_checker::Type::Struct(_)))
                .unwrap_or(false);
            if is_impl_method {
                if let Some(method_receiver) = method_receiver_for_call.as_ref() {
                    // For struct receivers, use the raw C variable name
                    // (no clone) so we can take its address for the
                    // struct-pointer parameter. For non-struct receivers,
                    // emit normally.
                    let recv_val = if let Expr::Ident { name, .. } = method_receiver {
                        if let Some(cid) = names.lookup(name) {
                            cid.to_string()
                        } else {
                            self.emit_expr(method_receiver, names, out)
                        }
                    } else {
                        self.emit_expr(method_receiver, names, out)
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

        "zz_unit()".to_string()
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
            let struct_name = self.struct_name_from_c_type(recv_type)?;
            let impl_name = format!("{struct_name}.{method}");
            if self.reachable_funcs.contains(&impl_name) {
                let span = zz_frontend::span::Span::new(0, 0);
                let recv_expr = Expr::Ident {
                    name: matched_tail,
                    span,
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

    pub(super) fn emit_str_literal(&self, s: &str) -> String {
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
                let cid = names.enter(&name.name);
                out.push_str(&format!("        zz_value {cid} = {scrut};\n"));
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
                    let payload_tmp = names.fresh("_payload");
                    out.push_str(&format!(
                        "            zz_value {payload_tmp} = {extractor}({scrut});\n"
                    ));
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
        let scrut_tmp = names.fresh("_match");
        let boxed = box_scalar_operand(scrutinee, names, &scrut_val);
        out.push_str(&format!("    zz_value {scrut_tmp} = {boxed};\n"));

        let scrut_type = scalar_operand_type(scrutinee, names);
        let scrut_raw = scalar_operand_c(scrutinee, names).unwrap_or_else(|| scrut_val.clone());

        let result_tmp = names.fresh("_mresult");
        out.push_str(&format!("    zz_value {result_tmp} = zz_unit();\n"));

        for (i, arm) in arms.iter().enumerate() {
            let is_last_arm = i == arms.len() - 1;
            let arm_needs_else_prefix = i > 0;
            let arm_closes_block = match &arm.pat {
                Pattern::Binding { .. } | Pattern::Wildcard { .. } | Pattern::Variant { .. } => {
                    arm.guard.is_none() && is_last_arm
                }
                _ => false,
            };

            match &arm.pat {
                Pattern::Variant { name, arg, .. } => {
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
                        let payload_tmp = names.fresh("_payload");
                        out.push_str(&format!(
                            "        zz_value {payload_tmp} = {extractor}({scrut_tmp});\n"
                        ));
                        inner_open = self.emit_pattern_bind(arg_pat, &payload_tmp, names, out);
                    }

                    let arm_val = self.emit_expr(&arm.body, names, out);
                    out.push_str(&format!("        {result_tmp} = {arm_val};\n"));
                    // Close any nested pattern if-blocks.
                    for _ in 0..inner_open {
                        out.push_str("        }\n");
                    }
                    if arm_closes_block {
                        out.push_str("    }\n");
                    }
                }
                Pattern::Binding { name } => {
                    let cid = names.enter(&name.name);
                    out.push_str(&format!("        zz_value {cid} = {scrut_tmp};\n"));

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
                    let arm_val = self.emit_expr(&arm.body, names, out);
                    out.push_str(&format!("        {result_tmp} = {arm_val};\n"));
                    if arm_closes_block {
                        out.push_str("    }\n");
                    }
                }
                Pattern::Wildcard { .. } => {
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
                    let arm_val = self.emit_expr(&arm.body, names, out);
                    out.push_str(&format!("        {result_tmp} = {arm_val};\n"));
                    if arm_closes_block {
                        out.push_str("    }\n");
                    }
                }
                Pattern::Literal { value, .. } => {
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
                    let arm_val = self.emit_expr(&arm.body, names, out);
                    out.push_str(&format!("        {result_tmp} = {arm_val};\n"));
                    if arm_closes_block {
                        out.push_str("    }\n");
                    }
                }
                Pattern::Tuple { .. } => {
                    out.push_str("    // unsupported match pattern (tuple)\n");
                }
            }
        }
        result_tmp
    }
}
