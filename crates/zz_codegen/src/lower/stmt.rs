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
                        if self.green_active() {
                            self.stmt_direct.set(true);
                        }
                        let val = self.emit_expr(value, names, out);
                        if self.green_active() {
                            let captured = names.capture_set.contains(&name.name);
                            let (ptr, deref, n) = self.green_cell(names, &c_type, captured, out);
                            out.push_str(&format!("    {deref} = {val};\n"));
                            names.enter_cell(&name.name, &ptr, &deref, &c_type, n);
                        } else if names.capture_set.contains(&name.name) {
                            // Captured struct: heap cell holding the unboxed value.
                            let n = names.bump_counter();
                            let ptr = format!("_cell{n}");
                            let deref = NameCtx::owner_deref(&ptr);
                            self.emit_cell_alloc(&ptr, &c_type, &val, out);
                            names.enter_cell(&name.name, &ptr, &deref, &c_type, n);
                        } else {
                            let cid = names.enter_with_type(&name.name, &c_type);
                            out.push_str(&format!("    {c_type} {cid} = {val};\n"));
                        }
                        return;
                    }
                }

                // Emit the RHS expression FIRST, while the variable name still
                // resolves to the PREVIOUS scope entry (if any). This is critical
                // for redeclarations like `total := total + item` inside loop
                // bodies: the RHS must reference the old `total`, not the new one.
                //
                // In a green closure the RHS may be a statement-level
                // suspendable call: flag it so the call lowerer suspends
                // instead of parking.
                if self.green_active() {
                    self.stmt_direct.set(true);
                }
                let val = self.emit_expr(value, names, out);

                // NOW enter the new scope entry with the correct C type.
                // For scalars, use enter_with_type so that any subsequent code
                // (e.g. the loop body in a for-loop) sees the correct type
                // during scalar_operand_type lookups.
                //
                // For scalar types, extract the underlying value from the zz_value
                // UNLESS the emitted expression is already unboxed (a binary
                // op on unboxed scalars produces a raw int64_t, not a zz_value;
                // a raw scalar variable likewise needs no extraction).
                let ident = match value {
                    Expr::Ident { name, .. } => Some(name.as_str()),
                    _ => None,
                };
                let val_is_unboxed =
                    crate::lower::context::emitted_is_raw_scalar(&val, names, ident);
                let final_val = match ctype.as_str() {
                    "int64_t" if !val_is_unboxed => format!("({val}).i"),
                    "double" if !val_is_unboxed => format!("({val}).f"),
                    "bool" if !val_is_unboxed => format!("({val}).b"),
                    _ => val,
                };
                // Captured bindings become shared heap cells instead of plain
                // locals: the stack entry holds the deref expr so every use
                // site transparently reads/writes the shared cell.
                // Green closures put EVERY binding in a task-frame cell
                // (locals must survive a suspend): same transparency,
                // frame-backed lifetime.
                if self.green_active() {
                    let captured = names.capture_set.contains(&name.name);
                    let (ptr, deref, n) = self.green_cell(names, &ctype, captured, out);
                    out.push_str(&format!("    {deref} = {final_val};\n"));
                    names.enter_cell(&name.name, &ptr, &deref, &ctype, n);
                } else if names.capture_set.contains(&name.name) {
                    let n = names.bump_counter();
                    let ptr = format!("_cell{n}");
                    let deref = NameCtx::owner_deref(&ptr);
                    self.emit_cell_alloc(&ptr, &ctype, &final_val, out);
                    names.enter_cell(&name.name, &ptr, &deref, &ctype, n);
                } else {
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
                    out.push_str(&format!("    {ctype} {cid} = {final_val};\n"));
                }
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

                // Green closures: a statement-level suspendable RHS
                // suspends instead of parking (pre-scan rejected exotic
                // targets, so the assignment after the label is plain).
                if self.green_active() {
                    self.stmt_direct.set(true);
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
                let ident = match value {
                    Expr::Ident { name, .. } => Some(name.as_str()),
                    _ => None,
                };
                let val_is_actually_scalar =
                    crate::lower::context::emitted_is_raw_scalar(&val, names, ident)
                        || (ast_says_scalar && !val.starts_with("zz_"));
                let value_is_scalar = ast_says_scalar || val_is_actually_scalar;
                let value_needs_unbox = !val_is_actually_scalar;
                match target {
                    Expr::Ident { name, .. } => {
                        if let Some(cid) = names.lookup(name.as_str()).map(|s| s.to_string()) {
                            // Check if the target variable is a scalar type
                            if let Some(ctype) =
                                names.lookup_type(name.as_str()).map(|s| s.to_string())
                            {
                                match ctype.as_str() {
                                    "int64_t" | "double" | "bool" => {
                                        if value_is_scalar && !value_needs_unbox {
                                            out.push_str(&format!("    {cid} = {val};\n"));
                                        } else {
                                            let field = match ctype.as_str() {
                                                "int64_t" => ".i",
                                                "double" => ".f",
                                                "bool" => ".b",
                                                _ => "",
                                            };
                                            out.push_str(&format!("    {cid} = ({val}){field};\n"));
                                        }
                                    }
                                    ct if ct.starts_with("zz_struct_")
                                        && names.cell_ptrs.contains_key(name.as_str()) =>
                                    {
                                        // Struct cell: `zz_assign` expects `zz_value *` but
                                        // struct cells are `zz_struct_X *`.  Use a named
                                        // temporary + memcpy to avoid type mismatch.
                                        let tmp = names.fresh("_sval");
                                        out.push_str(&format!(
                                            "    {ct} {tmp} = {val};\n\
                                             memcpy(&{cid}, &{tmp}, sizeof({ct}));\n"
                                        ));
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
                                        // Promoted assignment through an
                                        // embedded struct: `u.id = 1` writes
                                        // `(u).Base.id`.
                                        if let Some(root) = self.unmangled_struct_name(base_type) {
                                            if let Some((chain, leaf)) =
                                                self.resolve_access_chain(&root, &parts[1..])
                                            {
                                                let final_val = match leaf.as_str() {
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
                                                let mut lhs = format!("({base_cid})");
                                                for p in &chain {
                                                    lhs = format!("({lhs}).{p}");
                                                }
                                                out.push_str(&format!(
                                                    "    {lhs} = {final_val};\n"
                                                ));
                                                return;
                                            }
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
                                        // General promotion-aware resolution
                                        // first (covers direct chains and
                                        // embedded hops of any depth).
                                        if let Some(root) = self.unmangled_struct_name(base_type) {
                                            if let Some((chain, leaf)) =
                                                self.resolve_access_chain(&root, &parts[1..])
                                            {
                                                let final_val = match leaf.as_str() {
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
                                                let mut lhs = format!("({base_cid})");
                                                for p in &chain {
                                                    lhs = format!("({lhs}).{p}");
                                                }
                                                out.push_str(&format!(
                                                    "    {lhs} = {final_val};\n"
                                                ));
                                                return;
                                            }
                                        }
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

                        // Deep chains (`a.b.c.d = v`): promotion-aware
                        // resolution for unboxed structs.
                        if parts.len() > 3 {
                            if let Some(base_cid) = names.lookup(&parts[0]) {
                                if let Some(base_type) = names.lookup_type(&parts[0]) {
                                    if self.is_struct_type_str(base_type) {
                                        if let Some(root) = self.unmangled_struct_name(base_type) {
                                            if let Some((chain, leaf)) =
                                                self.resolve_access_chain(&root, &parts[1..])
                                            {
                                                let final_val = match leaf.as_str() {
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
                                                let mut lhs = format!("({base_cid})");
                                                for p in &chain {
                                                    lhs = format!("({lhs}).{p}");
                                                }
                                                out.push_str(&format!(
                                                    "    {lhs} = {final_val};\n"
                                                ));
                                                return;
                                            }
                                        }
                                    }
                                }
                            }
                        }

                        // Boxed struct field assignment, direct or promoted
                        // (`u.id = 1`): lowers to runtime
                        // `zz_object_set_field`, which promotes through
                        // embedded structs in C. Only fires for checker-known
                        // structs so dict/dot targets keep their old path.
                        if parts.len() >= 2 {
                            if let Some(base_cid) = names.lookup(&parts[0]).map(str::to_string) {
                                if let Some(zz_checker::Type::Struct(sname)) =
                                    names.checker_types.get(&parts[0]).cloned()
                                {
                                    if self.type_to_c(&zz_checker::Type::Struct(sname.clone()))
                                        == "zz_value"
                                    {
                                        if let Some((chain, leaf)) =
                                            self.resolve_access_chain(&sname, &parts[1..])
                                        {
                                            let boxed = Self::box_struct_field_ctype(val, &leaf);
                                            if chain.len() == 1 {
                                                out.push_str(&format!(
                                                    "    zz_object_set_field(&{base_cid}, \"{}\", {boxed});\n",
                                                    chain[0]
                                                ));
                                            } else {
                                                // Walk down with temps, set
                                                // the leaf, write back up
                                                // (value semantics, like
                                                // the VM's assign_path).
                                                let mut tmps = vec![base_cid];
                                                for f in &chain[..chain.len() - 1] {
                                                    let tmp = names.fresh("_sobj");
                                                    let parent = tmps.last().cloned().unwrap();
                                                    out.push_str(&format!(
                                                        "    zz_value {tmp} = zz_object_get_field(&{parent}, \"{f}\");\n"
                                                    ));
                                                    tmps.push(tmp);
                                                }
                                                let leaf_parent = tmps.last().cloned().unwrap();
                                                out.push_str(&format!(
                                                    "    zz_object_set_field(&{leaf_parent}, \"{}\", {boxed});\n",
                                                    chain.last().unwrap()
                                                ));
                                                for (i, f) in chain[..chain.len() - 1]
                                                    .iter()
                                                    .enumerate()
                                                    .rev()
                                                {
                                                    out.push_str(&format!(
                                                        "    zz_object_set_field(&{}, \"{f}\", zz_clone({}));\n",
                                                        tmps[i], tmps[i + 1]
                                                    ));
                                                }
                                            }
                                            return;
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
                        if self.green_active() {
                            self.stmt_direct.set(true);
                        }
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
                    // Mutating-method write-back (mirrors the VM compiler and
                    // tree-walker): `arr.sort()` as a statement must store
                    // the returned array back, since AOT natives take the
                    // array by value and return a new one. `push`/`append`
                    // are already in-place (`zz_vec_append`) and excluded.
                    if let Some((obj, method)) = mutating_method_target(e, names) {
                        // NB: `push`/`append` lower in void context to
                        // in-place `zz_vec_append`, which returns Unit —
                        // writing that back would destroy the array.
                        const WRITEBACK_METHODS: &[&str] =
                            &["pop", "insert", "remove", "reverse", "sort"];
                        let is_struct = names
                            .lookup_type(&obj)
                            .is_some_and(|t| t.starts_with("zz_struct_"));
                        if !is_struct && WRITEBACK_METHODS.contains(&method.as_str()) {
                            if let Some(cid) = names.lookup(&obj).map(|s| s.to_string()) {
                                *self.void_context.borrow_mut() = true;
                                let val = self.emit_expr(e, names, out);
                                *self.void_context.borrow_mut() = false;
                                out.push_str(&format!("    zz_assign(&{cid}, {val});\n"));
                                return;
                            }
                        }
                    }
                    *self.void_context.borrow_mut() = true;
                    if self.green_active() {
                        self.stmt_direct.set(true);
                    }
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
            // Top-level only: emitted in the preamble by `Lowerer::lower`.
            Stmt::ExternBlock { .. } | Stmt::Link { .. } => {}
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
            // Skipped in green closures: stack arenas cannot survive a
            // suspend (heap-only there).
            let loop_arena: Option<String> =
                if !self.green_active() && self.escape.loop_spans.contains(&span) {
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
            let green = self.green_active();
            // Green closures: the loop variable lives in a frame cell
            // (a resume may land inside the body). Plain path keeps the
            // unboxed stack local.
            let cid: String = if green {
                let (ptr, deref, n) = self.green_cell(names, "int64_t", true, out);
                names.enter_cell(v, &ptr, &deref, "int64_t", n);
                deref
            } else {
                let cid = names.enter(v);
                // Update the type in NameCtx to int64_t since the loop variable is unboxed
                if let Some(vec) = names.stack.get_mut(v) {
                    if let Some((_, existing_ty)) = vec.last_mut() {
                        *existing_ty = "int64_t".to_string();
                    }
                }
                cid
            };

            // Fast path: if end is an unboxed scalar, use unboxed C loop variables
            if end_is_scalar {
                // Determine the actual unboxed value.
                // Scalar-typed ends (plain local, param, or captured raw
                // cell) are already unboxed — use as-is and NEVER append
                // `.i`: a raw cell deref like `((*(int64_t*)env[i]))` is an
                // int, not a struct (appending `.i` breaks C compilation
                // for loops over captured int bounds, green or plain).
                let is_simple_ident = |s: &str| -> bool {
                    !s.is_empty()
                        && s.chars().all(|c| c.is_alphanumeric() || c == '_')
                        && !s.starts_with("zz_")
                };

                let end_is_ident_scalar = end_is_scalar;
                let ev_unboxed = if end_is_ident_scalar {
                    if (ev.starts_with("zz_int(")
                        || ev.starts_with("zz_float(")
                        || ev.starts_with("zz_bool("))
                        && ev.ends_with(')')
                    {
                        // Redundant boxing around a scalar (some passes add
                        // it): strip to the inner expression. NOTE: prefix
                        // lengths differ (`zz_float(` is 9 chars), so match
                        // each prefix explicitly instead of slicing [7..].
                        ["zz_int(", "zz_float(", "zz_bool("]
                            .iter()
                            .find_map(|p| ev.strip_prefix(p).and_then(|s| s.strip_suffix(')')))
                            .unwrap_or(&ev)
                            .to_string()
                    } else {
                        ev.clone()
                    }
                } else if is_simple_ident(&ev) {
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

                // For start, similar logic: scalar-typed starts are already
                // unboxed (same raw-deref rule as the end bound above).
                let start_name_opt: Option<String> = match &start_expr {
                    Expr::Ident { name, .. } => Some(name.clone()),
                    Expr::Path { parts, .. } => Some(parts.join(".")),
                    _ => None,
                };
                let start_is_scalar = start_name_opt
                    .as_ref()
                    .and_then(|n| names.lookup_type(n))
                    .map(|t| t == "int64_t" || t == "double" || t == "bool")
                    .unwrap_or(false);
                let sv_unboxed = if start_is_scalar {
                    if (sv.starts_with("zz_int(")
                        || sv.starts_with("zz_float(")
                        || sv.starts_with("zz_bool("))
                        && sv.ends_with(')')
                    {
                        // Same prefix-length discipline as the end bound.
                        ["zz_int(", "zz_float(", "zz_bool("]
                            .iter()
                            .find_map(|p| sv.strip_prefix(p).and_then(|s| s.strip_suffix(')')))
                            .unwrap_or(&sv)
                            .to_string()
                    } else {
                        sv.clone()
                    }
                } else if sv.starts_with("zz_int(") {
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

                let s = if green {
                    // Resume-safety: the end bound is re-read every
                    // iteration, including iterations after a resume jumped
                    // over the bound's evaluation. A non-literal bound
                    // (variable, temp, call result) would read a dead C
                    // stack slot — indeterminate trip count (silent hangs /
                    // early exits under O3). Spill it into a frame cell once
                    // at loop entry. Only pure literals stay inline (a
                    // `zz_int(...)` wrapper around a temp is NOT a literal).
                    let end_ref: String = ["zz_int(", "zz_float(", "zz_bool("]
                        .iter()
                        .find_map(|p| {
                            if ev.starts_with(p) && ev.ends_with(')') {
                                let inner = &ev[p.len()..ev.len() - 1];
                                let numeric = !inner.is_empty()
                                    && inner.chars().all(|c| {
                                        c.is_ascii_digit()
                                            || c == '.'
                                            || c == '-'
                                            || c == '+'
                                            || c == 'e'
                                            || c == 'E'
                                    });
                                if numeric || inner == "true" || inner == "false" {
                                    Some(ev_unboxed.clone())
                                } else {
                                    None
                                }
                            } else {
                                None
                            }
                        })
                        .unwrap_or_else(|| {
                            let end_ctype: String = end_name_opt
                                .as_ref()
                                .and_then(|n| names.lookup_type(n))
                                .filter(|t| *t == "int64_t" || *t == "double" || *t == "bool")
                                .unwrap_or("int64_t")
                                .to_string();
                            let (_, ederef, _) = self.green_cell(names, &end_ctype, false, out);
                            out.push_str(&format!("    {ederef} = {ev_unboxed};\n"));
                            ederef
                        });
                    format!("for ({cid} = {sv_unboxed}; {cid} < {end_ref}; {cid}++) {{\n")
                } else {
                    format!(
                        "for (int64_t {cid} = {sv_unboxed}; {cid} < {ev_unboxed}; {cid}++) {{\n"
                    )
                };
                out.push_str(&s);
                // Loop-top safepoint (mirrors the VM's `Op::Safepoint`).
                out.push_str("    zz_safepoint();\n");
            } else {
                // Slow path: both bounds are general expressions, use boxed loop
                let sv_boxed = sv;
                let ev_boxed = auto_box(&ev, if end_is_scalar { Some("int64_t") } else { None });
                if green {
                    // Green: the end bound is re-read every iteration,
                    // including iterations after a resume jumped over its
                    // evaluation — a C-local `_e` would be indeterminate
                    // (silent hangs / early exits under O3). Spill it into
                    // a frame cell once at loop entry. `_s` stays a local:
                    // it is read only at driver init + guard, both of which
                    // run strictly before any suspend.
                    let (_, driver, _) = self.green_cell(names, "int64_t", true, out);
                    let (_, ederef, _) = self.green_cell(names, "zz_value", false, out);
                    out.push_str(&format!(
                        "{{ zz_value _s = {sv_boxed};\n    \
                         {ederef} = {ev_boxed};\n    \
                         if (_s.tag == ZZ_INT && ({ederef}).tag == ZZ_INT) {{\n        \
                         for ({driver} = _s.i; {driver} < ({ederef}).i; {driver}++) {{\n            \
                         {cid} = {driver};\n            \
                         zz_safepoint();\n"
                    ));
                } else {
                    let s = format!(
                        "{{ zz_value _s = {sv_boxed}; zz_value _e = {ev_boxed};\n    \
                     if (_s.tag == ZZ_INT && _e.tag == ZZ_INT) {{\n        \
                     for (int64_t {cid}_i = _s.i; {cid}_i < _e.i; {cid}_i++) {{\n            \
                     int64_t {cid} = {cid}_i;\n            \
                     zz_safepoint();\n"
                    );
                    out.push_str(&s);
                }
            }
            // Loop body is never a function tail.
            // A captured loop variable gets a per-iteration shared cell so
            // closures created inside the loop each see their own iteration.
            // Body writes go to the cell; the raw loop variable keeps driving
            // iteration. Green closures mirror this with frame cells: the
            // driver stays put while a fresh per-iteration copy cell shadows
            // it for the body (spawned closures dup at creation, so later
            // iterations cannot disturb them — same isolation as below).
            let loop_cell: Option<String> = if names.capture_set.contains(v) {
                if green {
                    let (ptr2, deref2, n2) = self.green_cell(names, "int64_t", true, out);
                    out.push_str(&format!("    {deref2} = {cid};\n"));
                    names.enter_cell(v, &ptr2, &deref2, "int64_t", n2);
                    Some(v.clone())
                } else {
                    let n = names.bump_counter();
                    let ptr = format!("_cell{n}");
                    let deref = NameCtx::owner_deref(&ptr);
                    out.push_str(&format!(
                        "    int64_t *{ptr} = (int64_t*)malloc(sizeof(int64_t));\n"
                    ));
                    out.push_str(&format!("    {deref} = {cid};\n"));
                    names.enter_cell(v, &ptr, &deref, "int64_t", n);
                    Some(v.clone())
                }
            } else {
                None
            };
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
            if let Some(cell_name) = loop_cell {
                // Pop the per-iteration cell shadow, then the raw loop var.
                names.leave(&cell_name);
                names.pop_cell(&cell_name);
            }
            if green {
                // Green driver is itself a frame cell: retire its records.
                names.pop_cell(v);
            }
            names.leave(v);
        } else {
            // For-in over an array or dict: evaluate the iterable, then
            // dispatch based on the runtime tag.
            let iter_val = self.emit_expr(iter, names, out);
            let green_iter = self.green_active();
            // Green: the iterable temp must survive a suspend (a resume
            // may land inside the loop).
            let iter_tmp: String = if green_iter {
                let (_, deref, _) = self.green_cell(names, "zz_value", false, out);
                out.push_str(&format!("    {deref} = {iter_val};\n"));
                deref
            } else {
                let iter_tmp = names.fresh("_iter");
                out.push_str(&format!("    zz_value {iter_tmp} = {iter_val};\n"));
                iter_tmp
            };

            if vars.len() == 1 {
                // for x in <array|dict>: iterate array elements or dict keys.
                // Enter a scope so redeclarations inside the body (e.g.
                // `total := total + item`) don't leak past the loop boundary.
                names.push_scope();
                let v = &vars[0].name;
                // Green: per-iteration item is a frame cell (fresh when a
                // nested closure may capture it — same isolation as the
                // blocking path's per-iteration cells — else reused).
                let cid: String = if green_iter {
                    let captured = names.capture_set.contains(v);
                    let (ptr, deref, n) = self.green_cell(names, "zz_value", captured, out);
                    names.enter_cell(v, &ptr, &deref, "zz_value", n);
                    deref
                } else {
                    let cid = names.enter(v);
                    // Update type to zz_value (array elements / dict keys are boxed)
                    if let Some(vec) = names.stack.get_mut(v) {
                        if let Some((_, existing_ty)) = vec.last_mut() {
                            *existing_ty = "zz_value".to_string();
                        }
                    }
                    cid
                };
                // Green: loop drivers live in frame cells (a resume may
                // land inside the loop); plain path keeps stack counters.
                let (idx, len): (String, String) = if green_iter {
                    let (_, ideref, _) = self.green_cell(names, "int64_t", false, out);
                    let (_, lderef, _) = self.green_cell(names, "int64_t", false, out);
                    out.push_str(&format!("    {ideref} = 0;\n"));
                    out.push_str(&format!(
                        "    {lderef} = ({iter_tmp}.tag == ZZ_ARRAY) ? (int64_t){iter_tmp}.arr->len\n\
                         : ({iter_tmp}.tag == ZZ_DICT) ? (int64_t){iter_tmp}.dict->len : 0;\n"
                    ));
                    (ideref, lderef)
                } else {
                    let idx = names.fresh("_idx");
                    let len = names.fresh("_len");
                    out.push_str(&format!("    int64_t {idx} = 0;\n"));
                    out.push_str(&format!(
                        "    int64_t {len} = ({iter_tmp}.tag == ZZ_ARRAY) ? (int64_t){iter_tmp}.arr->len\n\
                         : ({iter_tmp}.tag == ZZ_DICT) ? (int64_t){iter_tmp}.dict->len : 0;\n"
                    ));
                    (idx, len)
                };
                out.push_str(&format!("    for (; {idx} < {len}; {idx}++) {{\n"));
                out.push_str("    zz_safepoint();\n");
                // Green: the item is a frame cell (assigned, never
                // declared); plain path declares the per-iteration local.
                if green_iter {
                    out.push_str(&format!(
                        "        {cid} = ({iter_tmp}.tag == ZZ_ARRAY)\n\
                         ? zz_clone({iter_tmp}.arr->items[{idx}])\n\
                         : (zz_value){{ZZ_STR, {{.s = {iter_tmp}.dict->entries[{idx}].key}}}};\n"
                    ));
                } else {
                    out.push_str(&format!(
                        "        zz_value {cid} = ({iter_tmp}.tag == ZZ_ARRAY)\n\
                         ? zz_clone({iter_tmp}.arr->items[{idx}])\n\
                         : (zz_value){{ZZ_STR, {{.s = {iter_tmp}.dict->entries[{idx}].key}}}};\n"
                    ));
                }
                // Captured iteration variable: per-iteration shared cell.
                // Green skips this: the item is already a frame cell
                // (fresh per iteration when captured).
                if !green_iter && names.capture_set.contains(v) {
                    let n = names.bump_counter();
                    let ptr = format!("_cell{n}");
                    let deref = NameCtx::owner_deref(&ptr);
                    out.push_str(&format!(
                        "        zz_value *{ptr} = (zz_value*)malloc(sizeof(zz_value));\n"
                    ));
                    out.push_str(&format!("        {deref} = {cid};\n"));
                    names.enter_cell(v, &ptr, &deref, "zz_value", n);
                }
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
                // Green: per-iteration items are frame cells (fresh when
                // capturable, reused otherwise); drivers likewise.
                let (k_cid, v_cid): (String, String) = if green_iter {
                    let k_cap = names.capture_set.contains(k_name);
                    let (kptr, kderef, kn) = self.green_cell(names, "zz_value", k_cap, out);
                    names.enter_cell(k_name, &kptr, &kderef, "zz_value", kn);
                    let v_cap = names.capture_set.contains(v_name);
                    let (vptr, vderef, vn) = self.green_cell(names, "zz_value", v_cap, out);
                    names.enter_cell(v_name, &vptr, &vderef, "zz_value", vn);
                    (kderef, vderef)
                } else {
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
                    (k_cid, v_cid)
                };
                // Green: drivers are frame cells (a resume may land inside
                // the loop); plain path keeps stack counters.
                if green_iter {
                    let (_, ideref, _) = self.green_cell(names, "int64_t", false, out);
                    let (_, lderef, _) = self.green_cell(names, "int64_t", false, out);
                    out.push_str(&format!("    {ideref} = 0;\n"));
                    out.push_str(&format!(
                        "    {lderef} = ({iter_tmp}.tag == ZZ_DICT) ? (int64_t){iter_tmp}.dict->len : 0;\n"
                    ));
                    out.push_str(&format!("    for (; {ideref} < {lderef}; {ideref}++) {{\n"));
                    out.push_str("    zz_safepoint();\n");
                    out.push_str(&format!(
                        "        {k_cid} = (zz_value){{ZZ_STR, {{.s = {iter_tmp}.dict->entries[{ideref}].key}}}};\n"
                    ));
                    out.push_str(&format!(
                        "        {v_cid} = zz_clone({iter_tmp}.dict->entries[{ideref}].val);\n"
                    ));
                } else {
                    let idx = names.fresh("_idx");
                    let len = names.fresh("_len");
                    out.push_str(&format!("    int64_t {idx} = 0;\n"));
                    out.push_str(&format!(
                        "    int64_t {len} = ({iter_tmp}.tag == ZZ_DICT) ? (int64_t){iter_tmp}.dict->len : 0;\n"
                    ));
                    out.push_str(&format!("    for (; {idx} < {len}; {idx}++) {{\n"));
                    out.push_str("    zz_safepoint();\n");
                    out.push_str(&format!(
                        "        zz_value {k_cid} = (zz_value){{ZZ_STR, {{.s = {iter_tmp}.dict->entries[{idx}].key}}}};\n"
                    ));
                    out.push_str(&format!(
                        "        zz_value {v_cid} = zz_clone({iter_tmp}.dict->entries[{idx}].val);\n"
                    ));
                }
                // Captured iteration variables: per-iteration shared cells.
                // Green skips this: items are already frame cells (fresh
                // per iteration when captured).
                if !green_iter {
                    for (lname, raw) in [
                        (k_name.as_str(), k_cid.as_str()),
                        (v_name.as_str(), v_cid.as_str()),
                    ] {
                        if names.capture_set.contains(lname) {
                            let n = names.bump_counter();
                            let ptr = format!("_cell{n}");
                            let deref = NameCtx::owner_deref(&ptr);
                            out.push_str(&format!(
                                "        zz_value *{ptr} = (zz_value*)malloc(sizeof(zz_value));\n"
                            ));
                            out.push_str(&format!("        {deref} = {raw};\n"));
                            names.enter_cell(lname, &ptr, &deref, "zz_value", n);
                        }
                    }
                }
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

/// Detect `obj.method(args)` calls on a plain local variable, mirroring the
/// VM compiler's mutating-method interception (which also handles the
/// `Field{obj: Ident}` shape). Returns `(object_name, method_name)`.
fn mutating_method_target(e: &Expr, names: &NameCtx) -> Option<(String, String)> {
    let Expr::Call { callee, .. } = e else {
        return None;
    };
    match callee.as_ref() {
        // `arr.push(x)` parsed as a two-part path where the head is a local.
        Expr::Path { parts, .. } if parts.len() == 2 => {
            let (obj, method) = (&parts[0], &parts[1]);
            names.lookup(obj).map(|_| (obj.clone(), method.clone()))
        }
        // `arr.push(x)` parsed as a field access on an identifier.
        Expr::Field { obj, name, .. } => match obj.as_ref() {
            Expr::Ident { name: obj_name, .. } => names
                .lookup(obj_name)
                .map(|_| (obj_name.clone(), name.clone())),
            _ => None,
        },
        _ => None,
    }
}
