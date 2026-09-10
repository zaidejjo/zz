//! Codegen state, buffer management, and variable lookup scopes.
//!
//! Holds the [`NameCtx`] scope-aware C identifier allocator and the
//! [`Lowerer`] codegen state (reachability sets, typed program, escape
//! analysis, arena/defer/closure buffers). Also contains the scalar
//! classification helpers shared by the expression and statement
//! lowerers.

use std::collections::HashMap;

use zz_frontend::ast::Expr;

use super::mangle;

/// Scope-aware C identifier allocator (handles shadowing).
#[derive(Default, Clone)]
pub struct NameCtx {
    /// zz var name → stack of active (C identifier, C type) tuples (innermost last).
    pub(crate) stack: HashMap<String, Vec<(String, String)>>,
    pub(crate) counter: usize,
    /// Stack of counter snapshots for scope tracking. `push_scope` records
    /// the current counter; `pop_scope` removes all entries whose C id was
    /// created at or after that counter value.
    pub(crate) scope_markers: Vec<usize>,
    /// zz var name → statically-known length of the array literal it was
    /// most recently bound to (straight-line code only). Used to fold
    /// `len(v)` to a constant so tight loops lower to raw scalar arith.
    pub(crate) array_lens: HashMap<String, usize>,
    /// zz var name → checker type from the type system. Used by method
    /// dispatch to select the correct namespace (e.g., `"str"` for strings
    /// vs `"vec"` for arrays) when multiple natives share a method name.
    pub(crate) checker_types: HashMap<String, zz_checker::Type>,
}

impl NameCtx {
    pub(super) fn new() -> Self {
        NameCtx {
            stack: HashMap::new(),
            counter: 0,
            scope_markers: Vec::new(),
            array_lens: HashMap::new(),
            checker_types: HashMap::new(),
        }
    }

    /// Generate a fresh C identifier without entering a scope.
    /// Used for temporaries that need a unique name but aren't bound to a zz variable.
    pub(super) fn fresh(&mut self, prefix: &str) -> String {
        let cid = format!("{prefix}{}", self.counter);
        self.counter += 1;
        cid
    }

    /// Enter a new scope for `name`, returning the fresh C identifier.
    pub(super) fn enter(&mut self, name: &str) -> String {
        let cid = format!("v{}", self.counter);
        self.counter += 1;
        self.stack
            .entry(name.to_string())
            .or_default()
            .push((cid.clone(), "zz_value".to_string()));
        cid
    }

    /// Enter a new scope for `name` with a specific C type.
    pub(super) fn enter_with_type(&mut self, name: &str, ctype: &str) -> String {
        let cid = format!("v{}", self.counter);
        self.counter += 1;
        self.stack
            .entry(name.to_string())
            .or_default()
            .push((cid.clone(), ctype.to_string()));
        cid
    }

    /// Look up the variable's C identifier.
    pub(super) fn lookup(&self, name: &str) -> Option<&str> {
        self.stack
            .get(name)
            .and_then(|vec| vec.last())
            .map(|(ident, _)| ident.as_str())
    }

    /// Look up the variable's C type.
    pub(super) fn lookup_type(&self, name: &str) -> Option<&str> {
        self.stack
            .get(name)
            .and_then(|vec| vec.last())
            .map(|(_, typ)| typ.as_str())
    }

    /// Advance and return the next fresh counter value. Used by helpers
    /// that emit a family of related identifiers (e.g., stack-promoted
    /// arrays use three identifiers sharing one counter value).
    pub(super) fn bump_counter(&mut self) -> usize {
        let c = self.counter;
        self.counter += 1;
        c
    }

    /// Leave the current scope for `name`.
    pub(super) fn leave(&mut self, name: &str) {
        if let Some(vec) = self.stack.get_mut(name) {
            vec.pop();
        }
    }

    /// Push a scope marker. All entries created after this call (via
    /// `enter`/`enter_with_type`/`fresh`) will be removed by `pop_scope`.
    pub(super) fn push_scope(&mut self) {
        self.scope_markers.push(self.counter);
    }

    /// Pop all entries whose C identifier was created at or after the
    /// matching `push_scope` marker.  This correctly handles redeclarations
    /// inside loop bodies (e.g. `total := total + item` inside a for-loop).
    pub(super) fn pop_scope(&mut self) {
        if let Some(marker) = self.scope_markers.pop() {
            for vec in self.stack.values_mut() {
                vec.retain(|(cid, _)| {
                    // C ids are "vN" where N is the counter at creation time.
                    let n: usize = cid[1..].parse().unwrap_or(usize::MAX);
                    n < marker
                });
            }
        }
    }

    /// Record that `name` currently holds an array literal of length `n`.
    pub(super) fn set_array_len(&mut self, name: &str, n: usize) {
        self.array_lens.insert(name.to_string(), n);
    }

    /// Forget any statically-known literal length for `name`. Called when
    /// `name` is reassigned with a non-literal value or aliased into a call.
    pub(super) fn invalidate_array_len(&mut self, name: &str) {
        self.array_lens.remove(name);
    }

    /// Statically-known length of `name` if it was most recently bound to
    /// an array literal in the current straight-line block.
    pub(super) fn array_len(&self, name: &str) -> Option<usize> {
        self.array_lens.get(name).copied()
    }

    /// Drop all literal-length knowledge. Called at control-flow boundaries
    /// (block/loop/branch entries) so a fold never reads a binding that a
    /// merged path could have changed.
    pub(super) fn clear_array_lens(&mut self) {
        self.array_lens.clear();
    }
}

/// The lowering context.
pub struct Lowerer {
    pub(crate) reachable_funcs: std::collections::HashSet<String>,
    pub(crate) reachable_natives: std::collections::HashSet<String>,
    pub(crate) entry_main: String,
    pub(crate) tp: zz_hir::TypedProgram,
    /// Result of escape analysis: maps spans to allocation classification.
    #[allow(dead_code)]
    pub(crate) escape: zz_hir::EscapeResult,
    /// Stack of C arena identifiers for loop bodies that own a per-iteration
    /// sub-arena (`_loop_arena<N>`). Non-escaping allocations emitted while a
    /// sub-arena is active are routed to it, so the per-iteration reset can
    /// only ever free objects created inside that iteration — never objects
    /// allocated in an enclosing scope or by an outer loop iteration.
    pub(crate) loop_arenas: std::cell::RefCell<Vec<String>>,
    /// Deferred-expression snippet C code for the function currently being
    /// lowered (one entry per `defer` statement site). Flushed into a LIFO
    /// runner at the end of `emit_function`. Index into this list is the
    /// value stored in the per-function `__defers[]` array.
    pub(crate) defer_slots: std::cell::RefCell<Vec<String>>,
    /// Generated C static functions for closure literals (one per `Expr::Closure`).
    pub(crate) closure_defs: std::cell::RefCell<Vec<String>>,
    /// Forward declarations for closure static functions.
    pub(crate) closure_forward_decls: std::cell::RefCell<Vec<String>>,
    /// True when emitting a call expression whose return value is discarded
    /// (statement position). Enables in-place mutations like `zz_vec_append`
    /// instead of copy-on-write `zz_vec_push`.
    pub(crate) void_context: std::cell::RefCell<bool>,
    /// Name of the current loop arena, if any. Used to emit arena-aware
    /// native calls (e.g. str_cast_arena) inside loops.
    pub(crate) current_loop_arena: std::cell::RefCell<Option<String>>,
}

impl Lowerer {
    pub fn new(
        reachable_funcs: std::collections::HashSet<String>,
        reachable_natives: std::collections::HashSet<String>,
        entry_main: String,
        tp: zz_hir::TypedProgram,
    ) -> Self {
        let escape = zz_hir::escape_analyze(&tp);
        Lowerer {
            reachable_funcs,
            reachable_natives,
            entry_main,
            tp,
            escape,
            loop_arenas: std::cell::RefCell::new(Vec::new()),
            defer_slots: std::cell::RefCell::new(Vec::new()),
            closure_defs: std::cell::RefCell::new(Vec::new()),
            closure_forward_decls: std::cell::RefCell::new(Vec::new()),
            void_context: std::cell::RefCell::new(false),
            current_loop_arena: std::cell::RefCell::new(None),
        }
    }

    /// Check if an expression span is classified as non-escaping (arena-safe).
    #[allow(dead_code)]
    pub(super) fn is_non_escaping(&self, span: zz_frontend::span::Span) -> bool {
        self.escape
            .classes
            .get(&span)
            .map(|c| *c == zz_hir::AllocClass::NonEscaping)
            .unwrap_or(false)
    }

    /// Check if an expression span is classified as escaping (needs ARC).
    #[allow(dead_code)]
    pub(super) fn is_escaping(&self, span: zz_frontend::span::Span) -> bool {
        self.escape
            .classes
            .get(&span)
            .map(|c| *c == zz_hir::AllocClass::Escaping)
            .unwrap_or(false)
    }

    /// Check if an expression produces a string value. Used to detect string
    /// concatenation for arena-aware allocation in loops.
    pub(super) fn is_string_expr(&self, expr: &Expr, names: &NameCtx) -> bool {
        match expr {
            Expr::Str { .. } => true,
            Expr::Ident { name, .. } => names.lookup_type(name).map_or(false, |t| t == "string"),
            _ => false,
        }
    }

    /// Choose the arena for an allocation site.
    ///
    /// Allocation strategy, in priority order:
    ///   1. A span explicitly classified as `Escaping` must use the heap
    ///      (ARC) — never an arena, since the object outlives the scope.
    ///   2. Inside a loop body whose sub-arena is reset every iteration,
    ///      non-escaping allocations go to that sub-arena so the buffer is
    ///      reused across iterations with zero heap growth.
    ///   3. A span classified `NonEscaping` goes on the function arena,
    ///      freed in bulk at function exit.
    ///
    /// Returns the C arena identifier (without `&`) or None for heap.
    pub(super) fn arena_for(&self, span: zz_frontend::span::Span) -> Option<String> {
        if self.is_escaping(span) {
            return None;
        }
        if let Some(arena_name) = self.loop_arenas.borrow().last() {
            return Some(arena_name.clone());
        }
        if self.is_non_escaping(span) {
            return Some("_arena".to_string());
        }
        None
    }

    /// Returns the C constructor call for an array, routing through the
    /// arena variant when the expression is classified as non-escaping.
    #[allow(dead_code)] // used by experimental array-construction paths
    pub(super) fn emit_array_new(&self, span: zz_frontend::span::Span) -> String {
        match self.arena_for(span) {
            Some(arena) => format!("zz_array_new_arena_sized(&{arena}, 0)"),
            None => "zz_array_new()".to_string(),
        }
    }

    /// Returns the C constructor call for a dict, routing through the
    /// arena variant when the expression is classified as non-escaping.
    #[allow(dead_code)] // reserved for dict-construction paths
    pub(super) fn emit_dict_new(&self, span: zz_frontend::span::Span) -> String {
        match self.arena_for(span) {
            Some(arena) => format!("zz_dict_new_arena(&{arena})"),
            None => "zz_dict_new()".to_string(),
        }
    }

    /// Returns the C constructor call for a string literal, routing through
    /// the arena variant when the expression is classified as non-escaping.
    #[allow(dead_code)] // reserved for dynamic string allocation (concat results)
    pub(super) fn emit_str_new(
        &self,
        s: &str,
        len: usize,
        span: zz_frontend::span::Span,
    ) -> String {
        match self.arena_for(span) {
            Some(arena) => format!("zz_str_new_arena({s}, {len}, &{arena})"),
            None => format!("zz_str_new({s}, {len})"),
        }
    }

    /// Returns true if the current emission position is inside a loop body
    /// that has an arena reset injected. Used to force arena allocation
    /// for loop-local collections.
    pub(super) fn current_loop_has_arena_reset(&self) -> bool {
        self.loop_arenas.borrow().last().is_some()
    }

    /// Check if a struct type is unboxed (all scalar or nested unboxed struct fields).
    pub(super) fn is_unboxed_struct(&self, name: &str) -> bool {
        if let Some(sig) = self.tp.structs.get(name) {
            sig.fields
                .iter()
                .all(|(_, ty)| self.is_scalar_type(ty) || matches!(ty, zz_checker::Type::Struct(inner) if self.is_unboxed_struct(inner)))
        } else {
            false
        }
    }

    /// Check if a type is scalar (can be unboxed).
    pub(super) fn is_scalar_type(&self, ty: &zz_checker::Type) -> bool {
        matches!(
            ty,
            zz_checker::Type::Int | zz_checker::Type::Float | zz_checker::Type::Bool
        )
    }

    /// Convert a checker type to C type string.
    pub(super) fn type_to_c(&self, ty: &zz_checker::Type) -> String {
        match ty {
            zz_checker::Type::Int => "int64_t".to_string(),
            zz_checker::Type::Float => "double".to_string(),
            zz_checker::Type::Bool => "bool".to_string(),
            zz_checker::Type::Struct(name) => {
                if self.is_unboxed_struct(name) {
                    format!("zz_struct_{}", mangle(name))
                } else {
                    "zz_value".to_string()
                }
            }
            _ => "zz_value".to_string(),
        }
    }

    /// Get the C type name for a struct. Routes through `mangle()` so
    /// namespaced structs (e.g. `mod.Rectangle`) become a single valid
    /// C identifier (`zz_struct_mod__Rectangle`) instead of an invalid
    /// `zz_struct_mod.Rectangle`.
    pub(super) fn struct_c_type(&self, name: &str) -> String {
        format!("zz_struct_{}", mangle(name))
    }

    /// Check if a type string is a struct type.
    pub(super) fn is_struct_type_str(&self, type_str: &str) -> bool {
        type_str.starts_with("zz_struct_")
    }

    /// Extract the struct name from a type string like "zz_struct_Point" -> "Point".
    pub(super) fn struct_name_from_c_type<'a>(&self, c_type: &'a str) -> Option<&'a str> {
        c_type.strip_prefix("zz_struct_")
    }

    /// Get the C type of a field from a struct type string.
    /// Returns the C type string (e.g., "int64_t", "zz_struct_Point") for the named field.
    pub(super) fn field_type_from_struct(
        &self,
        base_c_type: &str,
        field_name: &str,
    ) -> Option<&str> {
        let mangled_suffix = self.struct_name_from_c_type(base_c_type)?;
        // The `tp.structs` keys are un-mangled (e.g. "structs.Rectangle"),
        // but the C type uses mangled names (e.g. "structs__Rectangle").
        // Find the key whose mangled form matches.
        let struct_name = self
            .tp
            .structs
            .keys()
            .find(|k| mangle(k) == mangled_suffix)?;
        let sig = self.tp.structs.get(struct_name)?;
        let (_, field_ty) = sig.fields.iter().find(|(n, _)| n == field_name)?;
        match field_ty {
            zz_checker::Type::Int => Some("int64_t"),
            zz_checker::Type::Float => Some("double"),
            zz_checker::Type::Bool => Some("bool"),
            zz_checker::Type::Struct(name) if self.is_unboxed_struct(name) => {
                // Return a static string — leak the Box for the 'static lifetime.
                // This is fine for codegen: small number of struct types, process exits.
                // Route through `mangle()` so a namespaced struct (e.g. `mod.Rect`)
                // produces a single valid C identifier.
                let s = format!("zz_struct_{}", mangle(name));
                Some(Box::leak(s.into_boxed_str()) as &str)
            }
            _ => None, // non-scalar boxed type: caller handles
        }
    }

    /// Auto-box a nested struct field value (e.g., r.origin.x).
    pub(super) fn auto_box_nested_field(
        &self,
        parts: &[String],
        names: &NameCtx,
        emitted: &str,
    ) -> String {
        if parts.len() >= 2 {
            if let Some(base_type) = names.lookup_type(&parts[0]) {
                // Walk the chain: r (Rect) -> origin (Point) -> x (int)
                let mut current_type = base_type.to_string();
                for part in &parts[1..parts.len() - 1] {
                    if let Some(inner_name) = self.struct_name_from_c_type(&current_type) {
                        if let Some(sig) = self.tp.structs.get(inner_name) {
                            if let Some((_, next_ty)) = sig.fields.iter().find(|(n, _)| n == part) {
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

    /// Try to emit an array literal as a stack-promoted C struct:
    ///   zz_value _items_<N>[K] = { ... };
    ///   zz_array _arr_<N> = { ZZ_ARRAY_STACK_MAGIC, K, K, _items_<N> };
    ///   zz_value _v<N> = (zz_value){ZZ_ARRAY, {.arr = &_arr_<N>}};
    /// Returns Some(var_name) when stack promotion is safe (literal is
    /// non-escaping, length ≤ 8, every element is a scalar literal/ident/
    /// arithmetic op). Returns None to signal the caller to fall back to
    /// the arena-allocated path.
    ///
    /// Stack-promoted arrays carry `refs = ZZ_ARRAY_STACK_MAGIC`; the
    /// runtime recognizes this sentinel and skips `free()` paths so neither
    /// the header nor the items buffer are touched at scope exit.
    pub(super) fn try_emit_stack_array(
        &self,
        elems: &[Expr],
        span: zz_frontend::span::Span,
        names: &mut NameCtx,
        out: &mut String,
    ) -> Option<String> {
        let non_esc = self.is_non_escaping(span);
        let loop_arena = self.current_loop_has_arena_reset();
        // Only promote when the array is provably non-escaping.
        if !non_esc && !loop_arena {
            return None;
        }
        // Skip empty literals — they have no items buffer to skip anyway
        // and the arena path is already O(1).
        if elems.is_empty() {
            return None;
        }
        // Cap at 8 elements: above this, the stack footprint starts to
        // hurt cache behavior and the arena path is already fast enough.
        const MAX_STACK_LEN: usize = 8;
        if elems.len() > MAX_STACK_LEN {
            return None;
        }

        // Classify each element and pre-compute its C scalar initializer.
        // Returns None if any element isn't a recognized scalar form.
        let mut inits: Vec<String> = Vec::with_capacity(elems.len());
        for e in elems {
            let init = self.emit_scalar_init(e, names, out)?;
            inits.push(init);
        }

        // Emit the three declarations.
        let counter = names.bump_counter();
        let items_var = format!("_items_{counter}");
        let arr_var = format!("_arr_{counter}");
        let v_var = format!("_v{counter}");
        let n = inits.len();
        out.push_str(&format!(
            "    zz_value {items_var}[{n}] = {{\n        {},\n    }};\n",
            inits.join(",\n        ")
        ));
        out.push_str(&format!(
            "    zz_array {arr_var} = {{ ZZ_ARRAY_STACK_MAGIC, {n}, {n}, {items_var} }};\n"
        ));
        out.push_str(&format!(
            "    zz_value {v_var} = (zz_value){{ZZ_ARRAY, {{.arr = &{arr_var}}}}};\n"
        ));
        Some(v_var)
    }

    /// Emit a C compound literal initializer for a scalar expression.
    /// Returns `{.tag=ZZ_INT, {.i=<expr>}}` / `{.tag=ZZ_FLOAT, {.f=<expr>}}`
    /// / `{.tag=ZZ_BOOL, {.b=<expr>}}` — or None for unsupported forms.
    ///
    /// Recognized shapes:
    ///   - Int literal: `42`
    ///   - Float literal: `3.5`
    ///   - Bool literal: `true` / `false`
    ///   - Ident mapped to `int64_t`/`double`/`bool` in NameCtx
    ///   - Arithmetic Binary that already lowers to `(int64_t)(l op r)` /
    ///     `(double)(l op r)` (i.e., scalar unboxed arithmetic on locals)
    ///   - Negation of any of the above that yields a recognized scalar
    pub(super) fn emit_scalar_init(
        &self,
        e: &Expr,
        names: &mut NameCtx,
        _out: &mut String,
    ) -> Option<String> {
        match e {
            Expr::Int { value, .. } => Some(format!("{{.tag=ZZ_INT, {{.i={value}}}}}")),
            Expr::Float { value, .. } => {
                let s = if *value == (*value).floor() && (*value).abs() < 1e15 {
                    format!("{value:.1}")
                } else {
                    format!("{value}")
                };
                Some(format!("{{.tag=ZZ_FLOAT, {{.f={s}}}}}"))
            }
            Expr::Bool { value, .. } => {
                let s = if *value { "true" } else { "false" };
                Some(format!("{{.tag=ZZ_BOOL, {{.b={s}}}}}"))
            }
            Expr::Ident { name, .. } => {
                let ty = names.lookup_type(name)?.to_string();
                let cid = names.lookup(name)?.to_string();
                match ty.as_str() {
                    "int64_t" => Some(format!("{{.tag=ZZ_INT, {{.i={cid}}}}}")),
                    "double" => Some(format!("{{.tag=ZZ_FLOAT, {{.f={cid}}}}}")),
                    "bool" => Some(format!("{{.tag=ZZ_BOOL, {{.b={cid}}}}}")),
                    _ => None,
                }
            }
            Expr::Unary { op, expr, .. } => {
                match op {
                    zz_frontend::ast::UnOp::Neg => {
                        // `-x` for any recognized scalar: produce a negated
                        // int of the inner scalar.
                        let inner = self.emit_scalar_init(expr, names, _out)?;
                        let stripped = inner
                            .strip_prefix("{.tag=ZZ_INT, {.i=")
                            .or_else(|| inner.strip_prefix("{.tag=ZZ_FLOAT, {.f="))?;
                        let stripped = stripped.strip_suffix("}}").unwrap_or(stripped);
                        Some(format!("{{.tag=ZZ_INT, {{.i=-({stripped})}}}}"))
                    }
                    zz_frontend::ast::UnOp::Pos => self.emit_scalar_init(expr, names, _out),
                    zz_frontend::ast::UnOp::Not => {
                        let inner_b = self.emit_scalar_bool_init(expr, names)?;
                        Some(format!("{{.tag=ZZ_BOOL, {{.b=!({inner_b})}}}}"))
                    }
                }
            }
            Expr::Paren { expr, .. } => self.emit_scalar_init(expr, names, _out),
            Expr::Binary { .. } => {
                // Binary ops only lower to a raw scalar C expression when
                // both operands are scalar locals. Emit via the regular
                // path and inspect the leading cast. Use a scratch buffer
                // so we don't pollute the caller's C output.
                let mut scratch = String::new();
                let emitted = self.emit_expr(e, names, &mut scratch);
                if emitted.starts_with("(int64_t)(") {
                    let inner = &emitted[10..emitted.len() - 1];
                    return Some(format!("{{.tag=ZZ_INT, {{.i={inner}}}}}"));
                }
                if emitted.starts_with("(double)(") {
                    let inner = &emitted[9..emitted.len() - 1];
                    return Some(format!("{{.tag=ZZ_FLOAT, {{.f={inner}}}}}"));
                }
                if emitted.starts_with("(bool)(") {
                    let inner = &emitted[7..emitted.len() - 1];
                    return Some(format!("{{.tag=ZZ_BOOL, {{.b={inner}}}}}"));
                }
                None
            }
            _ => None,
        }
    }

    /// Like `emit_scalar_init`, but forces a boolean-typed inner expression
    /// (used for `!x` where x is a bool ident/literal).
    pub(super) fn emit_scalar_bool_init(&self, e: &Expr, names: &NameCtx) -> Option<String> {
        match e {
            Expr::Bool { value, .. } => {
                let s = if *value { "true" } else { "false" };
                Some(s.to_string())
            }
            Expr::Ident { name, .. } => {
                let ty = names.lookup_type(name)?;
                if ty == "bool" {
                    return names.lookup(name).map(|cid| cid.to_string());
                }
                None
            }
            _ => None,
        }
    }
}

/// Auto-box an unboxed scalar expression to a `zz_value` C expression.
/// If the expression is already a `zz_value` (i.e., the variable's C type is
/// `zz_value` or unknown), it is returned as-is. Otherwise, the appropriate
/// boxing helper (`zz_int`, `zz_float`, `zz_bool`) is inserted.
pub(crate) fn auto_box(expr: &str, ctype: Option<&str>) -> String {
    // Idempotency guard: if the expression is already a boxed zz_value
    // (e.g. from emit_expr boxing struct field access), do NOT re-wrap.
    if expr.starts_with("zz_int(")
        || expr.starts_with("zz_float(")
        || expr.starts_with("zz_bool(")
        || expr.starts_with("zz_clone(")
        || expr.starts_with("zz_unit()")
    {
        return expr.to_string();
    }
    match ctype {
        Some("int64_t") => format!("zz_int({expr})"),
        Some("double") => format!("zz_float({expr})"),
        Some("bool") => format!("zz_bool({expr})"),
        _ => expr.to_string(),
    }
}

/// Classify a binary operand as a recognized scalar shape, returning its C
/// type (`"int64_t"` / `"double"`) if so. Recognized shapes:
///   - Int literal  → `"int64_t"`
///   - Float literal (whole-number form, like `1.0` / `2.5`) → `"double"`
///   - Ident mapped to `int64_t`/`double`/`bool` in NameCtx
///   - Negation of any of the above
///
///   Used by the Binary emit path to detect when both sides are scalar and
///   emit raw `lhs <op> rhs` instead of routing through the boxed `zz_binop`.
pub(crate) fn scalar_operand_type(e: &Expr, names: &NameCtx) -> Option<&'static str> {
    match e {
        Expr::Int { .. } => Some("int64_t"),
        Expr::Float { value, .. } => {
            if value.is_finite() {
                Some("double")
            } else {
                None
            }
        }
        Expr::Ident { name, .. } => match names.lookup_type(name)? {
            "int64_t" => Some("int64_t"),
            "double" => Some("double"),
            "bool" => Some("bool"),
            _ => None,
        },
        Expr::Unary { op, expr, .. } => {
            let inner = scalar_operand_type(expr, names)?;
            match op {
                zz_frontend::ast::UnOp::Neg => Some(inner),
                zz_frontend::ast::UnOp::Pos => Some(inner),
                zz_frontend::ast::UnOp::Not => Some("bool"),
            }
        }
        Expr::Paren { expr, .. } => scalar_operand_type(expr, names),
        _ => None,
    }
}

/// Return the unboxed C scalar expression for a binary operand. Used
/// after `scalar_operand_type` returns Some to build a raw C arithmetic
/// expression without going through `zz_binop`.
/// Wrap a binary operand — classified as a scalar by `scalar_operand_type`
/// OR emitted as a raw C cast — into its boxed `zz_value` form for the
/// `zz_binop` path. Uses the raw unboxed expression from
/// `scalar_operand_c` (literal body, raw C variable, ...) instead of the
/// already-emitted expression, so literals that emit as `zz_int(1)` never
/// end up double-boxed as `zz_int(zz_int(1))`. Nested binary operands that
/// lower to a raw C cast like `(int64_t)(2 * 3)` are recognized through the
/// cast marker and boxed with the matching constructor.
///
/// `field_hint` optionally provides the C type of a struct field access
/// expression (e.g. `"int64_t"`, `"double"`) when the caller already
/// resolved it from the type system. This avoids the free function needing
/// access to the Lowerer's struct registry.
pub(crate) fn is_simple_ident(s: &str) -> bool {
    !s.is_empty() && s.chars().all(|c| c.is_alphanumeric() || c == '_') && !s.starts_with("zz_")
}

pub(crate) fn box_scalar_operand(e: &Expr, names: &NameCtx, emitted: &str) -> String {
    match scalar_operand_type(e, names) {
        Some("int64_t") => {
            let raw = scalar_operand_c(e, names).unwrap_or_else(|| emitted.to_string());
            format!("zz_int({raw})")
        }
        Some("double") => {
            let raw = scalar_operand_c(e, names).unwrap_or_else(|| emitted.to_string());
            format!("zz_float({raw})")
        }
        Some("bool") => {
            let raw = scalar_operand_c(e, names).unwrap_or_else(|| emitted.to_string());
            format!("zz_bool({raw})")
        }
        _ => {
            // Not a recognized scalar shape directly, but the emitted
            // expression may still be a raw C scalar (e.g., a nested
            // binary op lowered to `(int64_t)(a * b)`).
            if emitted.starts_with("(double)(") {
                format!("zz_float({emitted})")
            } else if emitted.starts_with("(int64_t)(") {
                format!("zz_int({emitted})")
            } else if emitted.starts_with("(bool)(") {
                format!("zz_bool({emitted})")
            } else {
                // Last-resort fallback: if the emitted expression is a
                // simple C identifier (e.g., "v0") whose name maps to a
                // scalar-typed local in NameCtx, box it accordingly.
                // Without this, an int64_t local gets passed unboxed to
                // `zz_binop` and the C compiler rejects the call with
                // "incompatible type for argument".
                //
                // `names.lookup_type` takes the original ZZ name, but we
                // only have the emitted C identifier here. The emitted
                // identifier is unique, so we scan the stack for any entry
                // whose C identifier matches `emitted` and whose type is
                // a scalar.
                if is_simple_ident(emitted) {
                    for (_zzname, entries) in names.stack.iter() {
                        if let Some((cid, ty)) = entries.last() {
                            if cid == emitted {
                                match ty.as_str() {
                                    "int64_t" => return format!("zz_int({emitted})"),
                                    "double" => return format!("zz_float({emitted})"),
                                    "bool" => return format!("zz_bool({emitted})"),
                                    _ => {}
                                }
                            }
                        }
                    }
                }
                emitted.to_string()
            }
        }
    }
}

/// Emit a guard expression using raw C scalar values (no zz_value boxing).
/// Guard comparisons (e.g., `n < 0`) need raw int64_t/double/bool, not
/// boxed zz_value structs. The `scrut_tmp` is the scrutinee's C zz_value
/// identifier — for the primary bound variable (same as pattern name), we
/// unbox scrut_tmp directly instead of looking up the potentially-stale
/// names stack. Other identifiers are looked up normally.
pub(crate) fn emit_guard_expr(
    e: &Expr,
    names: &NameCtx,
    scrut_tmp: &str,
    scrut_type: Option<&str>,
) -> String {
    match e {
        Expr::Ident { name, .. } => {
            // Look up the C identifier in the names stack. If found with a
            // scalar type, use it directly. Otherwise, fall back to unboxing
            // scrut_tmp based on scrut_type.
            if let Some((cid, ty)) = names.stack.get(name).and_then(|v| v.last()) {
                match ty.as_str() {
                    "int64_t" => format!("({cid}).i"),
                    "double" => format!("({cid}).f"),
                    "bool" => format!("({cid}).b"),
                    _ => scrut_tmp.to_string(),
                }
            } else {
                // Fallback: use scrut_tmp's raw form based on its type
                match scrut_type {
                    Some("int64_t") => format!("({scrut_tmp}).i"),
                    Some("double") => format!("({scrut_tmp}).f"),
                    Some("bool") => format!("({scrut_tmp}).b"),
                    _ => format!("({scrut_tmp}).i"), // default int
                }
            }
        }
        Expr::Int { value, .. } => value.to_string(),
        Expr::Float { value, .. } => {
            if *value == value.floor() && value.abs() < 1e15 {
                format!("{value:.1}")
            } else {
                format!("{value}")
            }
        }
        Expr::Bool { value, .. } => if *value { "1" } else { "0" }.to_string(),
        Expr::Binary {
            op, left, right, ..
        } => {
            let l = emit_guard_expr(left, names, scrut_tmp, scrut_type);
            let r = emit_guard_expr(right, names, scrut_tmp, scrut_type);
            let op_str = match op {
                zz_frontend::ast::BinOp::Add => "+",
                zz_frontend::ast::BinOp::Sub => "-",
                zz_frontend::ast::BinOp::Mul => "*",
                zz_frontend::ast::BinOp::Div => "/",
                zz_frontend::ast::BinOp::Rem => "%",
                zz_frontend::ast::BinOp::Lt => "<",
                zz_frontend::ast::BinOp::Le => "<=",
                zz_frontend::ast::BinOp::Gt => ">",
                zz_frontend::ast::BinOp::Ge => ">=",
                zz_frontend::ast::BinOp::Eq => "==",
                zz_frontend::ast::BinOp::Ne => "!=",
                zz_frontend::ast::BinOp::And => "&&",
                zz_frontend::ast::BinOp::Or => "||",
                _ => "??",
            };
            format!("({l} {op_str} {r})")
        }
        Expr::Unary { op, expr, .. } => {
            let inner = emit_guard_expr(expr, names, scrut_tmp, scrut_type);
            match op {
                zz_frontend::ast::UnOp::Neg => format!("(-{inner})"),
                zz_frontend::ast::UnOp::Pos => format!("(+{inner})"),
                zz_frontend::ast::UnOp::Not => format!("(!{inner})"),
            }
        }
        Expr::Paren { expr, .. } => {
            format!("({})", emit_guard_expr(expr, names, scrut_tmp, scrut_type))
        }
        _ => "1".to_string(),
    }
}

/// Return the unboxed C scalar expression for a binary operand. Used
/// after `scalar_operand_type` returns Some to build a raw C arithmetic
/// expression without going through `zz_binop`.
pub(crate) fn scalar_operand_c(e: &Expr, names: &NameCtx) -> Option<String> {
    match e {
        Expr::Int { value, .. } => Some(value.to_string()),
        Expr::Float { value, .. } => {
            if value.is_finite() {
                let s = if *value == (*value).floor() && (*value).abs() < 1e15 {
                    format!("{value:.1}")
                } else {
                    format!("{value}")
                };
                Some(s)
            } else {
                None
            }
        }
        Expr::Ident { name, .. } => names.lookup(name).map(|s| s.to_string()),
        Expr::Unary { op, expr, .. } => {
            let inner = scalar_operand_c(expr, names)?;
            match op {
                zz_frontend::ast::UnOp::Neg => Some(format!("(-{inner})")),
                zz_frontend::ast::UnOp::Pos => Some(inner),
                zz_frontend::ast::UnOp::Not => {
                    if scalar_operand_type(expr, names) == Some("bool") {
                        Some(format!("(!{inner})"))
                    } else {
                        None
                    }
                }
            }
        }
        Expr::Paren { expr, .. } => scalar_operand_c(expr, names),
        _ => None,
    }
}
