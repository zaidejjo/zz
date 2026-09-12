//! Function generation, signatures, and struct declarations.

use zz_frontend::ast::{Block, Param, Stmt};

use super::*;

impl Lowerer {
    pub(super) fn emit_function(&self, fname: &str, params: &[Param], block: &Block) -> String {
        // Two function shapes:
        //   - Regular funcs: `static zz_value zz_fn_<mangled>(zz_value*, size_t)`
        //   - Impl methods:  `static zz_value zz_fn_<mangled>(<struct>* self, zz_value*, size_t)`
        //     The struct-typed `self` is passed by pointer so unboxed
        //     structs (no `zz_value` overhead) are passed efficiently and
        //     field access in the body lowers to raw C `(self).field`.
        let is_impl_method = self
            .tp
            .funcs
            .get(fname)
            .and_then(|sig| sig.params.first().map(|(_, t)| t.clone()))
            .map(|t| matches!(&t, zz_checker::Type::Struct(_)))
            .unwrap_or(false);
        let cname = format!("zz_fn_{}", mangle(fname));
        let mut o = String::new();
        let first_struct_type = if is_impl_method {
            self.tp
                .funcs
                .get(fname)
                .and_then(|sig| sig.params.first().map(|(_, t)| self.type_to_c(t)))
        } else {
            None
        };
        let signature = if let Some(ref sct) = first_struct_type {
            format!("static zz_value {cname}({sct} *self, zz_value *args, size_t argc) {{\n")
        } else {
            format!("static zz_value {cname}(zz_value *args, size_t argc) {{\n")
        };
        o.push_str(&signature);
        o.push_str("    (void)argc;\n");
        // --- Arena allocator: init on function entry, reset on exit ---
        o.push_str("    zz_arena _arena;\n");
        o.push_str("    zz_arena_init(&_arena, 65536);\n"); // 64KB default
        let mut names = NameCtx::new();
        // Look up the function's parameter types from the type checker.
        // Used to register each param under its actual C type so
        // subsequent expression lowering (field access, binop, ...)
        // produces the right shape.
        let param_types: Vec<zz_checker::Type> = self
            .tp
            .funcs
            .get(fname)
            .map(|sig| sig.params.iter().map(|(_, t)| t.clone()).collect())
            .unwrap_or_else(|| params.iter().map(|_| zz_checker::Type::Unit).collect());
        let arg_offset: usize = if is_impl_method { 1 } else { 0 };
        for (i, p) in params.iter().enumerate() {
            let pt = param_types
                .get(i)
                .cloned()
                .unwrap_or(zz_checker::Type::Unit);
            // The first param of an impl method is the struct receiver
            // (passed as `*self`); the remaining params live in `args`.
            if is_impl_method && i == 0 {
                let ctype = self.type_to_c(&pt);
                let cid = names.enter_with_type(&p.name.name, &ctype);
                o.push_str(&format!("    {ctype} {cid} = *self;\n"));
            } else {
                // Non-self params always live in the `args[]` array
                // which holds `zz_value`s. Keep them as `zz_value`
                // (boxed) — scalar unboxing happens at point of use
                // via `scalar_operand_type` / `box_scalar_operand`.
                let cid = names.enter(&p.name.name);
                o.push_str(&format!(
                    "    zz_value {cid} = args[{idx}];\n",
                    idx = i - arg_offset
                ));
            }
        }
        // Bit of per-function state for `defer` support: a fixed-size array
        // of registered defer-site indices + LIFO counter.
        o.push_str("    int __defers[32];\n");
        o.push_str("    int __defer_n = 0;\n");
        let mut body_out = String::new();
        self.emit_func_block(block, &mut names, &mut body_out);
        o.push_str(&body_out);
        // Defer runner: executes registered deferred expressions in LIFO
        // order at function scope exit (before the implicit return + arena
        // reset below).
        {
            let mut slots = self.defer_slots.borrow_mut();
            if !slots.is_empty() {
                o.push_str("    for (int __dk = __defer_n - 1; __dk >= 0; __dk--) {\n");
                o.push_str("        switch (__defers[__dk]) {\n");
                let snap: Vec<String> = slots.drain(..).collect();
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
        // Implicit return: last statement expression is the function value.
        if self.last_stmt_value(block, &mut names, &mut o).is_none() {
            o.push_str("    return zz_unit();\n");
        }
        // --- Arena reset: O(1) cleanup of all non-escaping allocations ---
        o.push_str("    zz_arena_reset_trim(&_arena);\n");
        o.push_str("}\n\n");
        o
    }

    /// Generate C typedefs for all reachable structs, in dependency order.
    pub(super) fn lower_structs_preamble(&self) -> String {
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
                preamble.push_str("typedef struct {\n");
                for (field_name, field_type) in &sig.fields {
                    let c_type = match field_type {
                        zz_checker::Type::Int => "int64_t".to_string(),
                        zz_checker::Type::Float => "double".to_string(),
                        zz_checker::Type::Bool => "bool".to_string(),
                        zz_checker::Type::Struct(n) if is_unboxed(n) => {
                            format!("zz_struct_{}", mangle(n))
                        }
                        _ => "zz_value".to_string(),
                    };
                    preamble.push_str(&format!("    {} {};\n", c_type, field_name));
                }
                // Mangle the struct's own C typedef name so namespaced
                // structs (e.g. `mod.Rectangle`) emit a single valid
                // identifier (`zz_struct_mod__Rectangle`) instead of
                // `zz_struct_mod.Rectangle` (illegal in C).
                preamble.push_str(&format!("}} zz_struct_{};\n\n", mangle(name)));
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

    /// Find a function's parameter definitions in the AST by name.
    pub(super) fn find_func_def(&self, name: &str) -> Option<&[Param]> {
        for stmt in &self.tp.program.stmts {
            if let Stmt::Func {
                name: func_name,
                params,
                ..
            } = stmt
            {
                if func_name.join(".") == name {
                    return Some(params);
                }
            }
        }
        None
    }
}
