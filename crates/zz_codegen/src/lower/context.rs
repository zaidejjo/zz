//! Codegen state, buffer management, and variable lookup scopes.
//!
//! Holds the [`NameCtx`] scope-aware C identifier allocator and the
//! [`Lowerer`] codegen state (reachability sets, typed program, escape
//! analysis, arena/defer/closure buffers). Also contains the scalar
//! classification helpers shared by the expression and statement
//! lowerers.

use std::collections::HashMap;
use std::collections::HashSet;

use zz_frontend::ast::Expr;

use super::mangle;

/// Extract the creation counter from a C identifier for scope tracking.
/// Plain locals are `vN`; owner-cell derefs are `(*_cellN)`. Anything else
/// (temps like `__tail1`, global ids) returns `usize::MAX` so `pop_scope`
/// drops it with the current scope.
fn cid_counter(cid: &str) -> usize {
    if let Some(rest) = cid.strip_prefix('v') {
        if let Ok(n) = rest.parse::<usize>() {
            return n;
        }
    }
    if let Some(rest) = cid.strip_prefix("(*_cell") {
        if let Some(num) = rest.strip_suffix(')') {
            if let Ok(n) = num.parse::<usize>() {
                return n;
            }
        }
    }
    usize::MAX
}

/// Scope-aware C identifier allocator (handles shadowing).
#[derive(Default, Clone)]
pub struct NameCtx {
    /// zz var name → stack of active (C identifier, C type) tuples (innermost last).
    pub(crate) stack: HashMap<String, Vec<(String, String)>>,
    pub(crate) counter: usize,
    /// Module-level globals: zz name → (C global id, C type).
    /// Checked after the local stack, so locals shadow globals.
    /// Never cleared by push/pop_scope.
    pub(crate) globals: HashMap<String, (String, String)>,
    /// Owner-cell pointers for closure-captured locals: zz name → stack of
    /// (creation counter, pointer expr, deref expr). The stack entry pushed
    /// alongside holds the deref expr as its C id, so ordinary reads/writes
    /// go through the shared cell with no special-casing at use sites.
    pub(crate) cell_ptrs: HashMap<String, Vec<(usize, String, String)>>,
    /// Closure environment captures: zz name → (deref expr, C type).
    /// Checked after the local stack (locals shadow captures), before globals.
    /// Never cleared by push/pop_scope: the env outlives inner scopes.
    pub(crate) cap_deref: HashMap<String, (String, String)>,
    /// Closure environment cell pointers: zz name → pointer expr (`env[i]`).
    /// Used when a nested closure captures an outer capture (shared cell).
    pub(crate) cap_ptrs: HashMap<String, String>,
    /// Names in the current body captured by a nested closure literal.
    /// Bindings for these names are heap-cell-allocated so the closure
    /// environment shares them (match VM by-reference capture semantics).
    /// Set once per function/closure body; never modified by push/pop.
    pub(crate) capture_set: HashSet<String>,
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
            globals: HashMap::new(),
            cell_ptrs: HashMap::new(),
            cap_deref: HashMap::new(),
            cap_ptrs: HashMap::new(),
            capture_set: HashSet::new(),
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

    /// Register a module-level global. Locals (stack) always shadow globals.
    pub(super) fn insert_global(
        &mut self,
        name: &str,
        cid: &str,
        ctype: &str,
        checker_ty: Option<zz_checker::Type>,
    ) {
        self.globals
            .insert(name.to_string(), (cid.to_string(), ctype.to_string()));
        if let Some(ct) = checker_ty {
            self.checker_types.insert(name.to_string(), ct);
        }
    }

    /// Look up the variable's C identifier: locals, then closure captures,
    /// then module globals. Cell locals hold their deref expr (`(*_cellN)`)
    /// as the C id so reads/writes transparently go through shared cells.
    pub(super) fn lookup(&self, name: &str) -> Option<&str> {
        if let Some((ident, _)) = self.stack.get(name).and_then(|vec| vec.last()) {
            return Some(ident.as_str());
        }
        if let Some((deref, _)) = self.cap_deref.get(name) {
            return Some(deref.as_str());
        }
        self.globals.get(name).map(|(ident, _)| ident.as_str())
    }

    /// Look up the variable's C type (same fallback chain as [`lookup`]).
    pub(super) fn lookup_type(&self, name: &str) -> Option<&str> {
        if let Some((_, typ)) = self.stack.get(name).and_then(|vec| vec.last()) {
            return Some(typ.as_str());
        }
        if let Some((_, typ)) = self.cap_deref.get(name) {
            return Some(typ.as_str());
        }
        self.globals.get(name).map(|(_, typ)| typ.as_str())
    }

    /// Deref expr for an owner cell pointer of C type `ctype`.
    pub(super) fn owner_deref(ptr: &str) -> String {
        format!("(*{ptr})")
    }

    /// Deref expr for a closure-env slot of C type `ctype`.
    pub(super) fn cap_deref_of(ptr: &str, ctype: &str) -> String {
        format!("(*({ctype}*){ptr})")
    }

    /// Register an owner cell for a captured local: pushes a stack entry
    /// holding the deref expr (transparent at use sites) and records the
    /// pointer for environment building. `ctr` must be the current
    /// [`counter`](Self::counter) so `pop_scope` retires it with its scope.
    pub(super) fn enter_cell(
        &mut self,
        name: &str,
        ptr: &str,
        deref: &str,
        ctype: &str,
        ctr: usize,
    ) {
        self.stack
            .entry(name.to_string())
            .or_default()
            .push((deref.to_string(), ctype.to_string()));
        self.cell_ptrs.entry(name.to_string()).or_default().push((
            ctr,
            ptr.to_string(),
            deref.to_string(),
        ));
    }

    /// Pop one owner-cell record for `name` (mirrors a manual [`leave`](Self::leave)).
    pub(super) fn pop_cell(&mut self, name: &str) {
        if let Some(vec) = self.cell_ptrs.get_mut(name) {
            vec.pop();
            if vec.is_empty() {
                self.cell_ptrs.remove(name);
            }
        }
    }

    /// Whether the active binding for `name` is an owner cell (i.e. the
    /// stack top holds the cell's deref, not a shadowing plain local).
    pub(super) fn is_owner_cell(&self, name: &str) -> bool {
        let top_deref = self
            .stack
            .get(name)
            .and_then(|v| v.last())
            .map(|(cid, _)| cid.as_str());
        let cell_deref = self
            .cell_ptrs
            .get(name)
            .and_then(|v| v.last())
            .map(|(_, _, d)| d.as_str());
        matches!((top_deref, cell_deref), (Some(a), Some(b)) if a == b)
    }

    /// Shared-cell pointer expr for `name`: the owner cell when active,
    /// else an outer closure capture. Used to build nested environments.
    /// Returns `None` for plain locals, globals, and unknowns.
    pub(super) fn cell_ptr(&self, name: &str) -> Option<String> {
        if self.is_owner_cell(name) {
            return self
                .cell_ptrs
                .get(name)
                .and_then(|v| v.last())
                .map(|(_, p, _)| p.clone());
        }
        // A shadowing plain local wins over an outer capture.
        if self.stack.get(name).and_then(|v| v.last()).is_some() {
            return None;
        }
        self.cap_ptrs.get(name).cloned()
    }

    /// Register a closure-environment capture: value reads use `deref`
    /// (transparent via [`lookup`](Self::lookup)); `ptr` is shared into
    /// nested environments.
    pub(super) fn insert_capture(
        &mut self,
        name: &str,
        ptr: &str,
        deref: &str,
        ctype: &str,
        checker_ty: Option<zz_checker::Type>,
    ) {
        self.cap_deref
            .insert(name.to_string(), (deref.to_string(), ctype.to_string()));
        self.cap_ptrs.insert(name.to_string(), ptr.to_string());
        if let Some(ct) = checker_ty {
            self.checker_types.insert(name.to_string(), ct);
        }
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
    /// `enter`/`enter_with_type`/`fresh`/`enter_cell`) will be removed by
    /// `pop_scope`.
    pub(super) fn push_scope(&mut self) {
        self.scope_markers.push(self.counter);
    }

    /// Pop all entries whose C identifier was created at or after the
    /// matching `push_scope` marker. Cell derefs (`(*_cellN)`) carry their
    /// creation counter; owner-cell records retire with the same rule.
    /// Captures (`cap_deref`) and globals survive: environments outlive
    /// inner scopes.
    pub(super) fn pop_scope(&mut self) {
        if let Some(marker) = self.scope_markers.pop() {
            for vec in self.stack.values_mut() {
                vec.retain(|(cid, _)| cid_counter(cid) < marker);
            }
            for vec in self.cell_ptrs.values_mut() {
                vec.retain(|(ctr, _, _)| *ctr < marker);
            }
            self.cell_ptrs.retain(|_, v| !v.is_empty());
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
    /// Monotonic closure id counter. Allocated BEFORE lowering the body
    /// (not derived from `closure_defs.len()`) so nested closure literals
    /// — which re-enter lowering while the outer literal is being built —
    /// still get unique ids.
    pub(crate) closure_seq: std::cell::Cell<usize>,
    /// True when emitting a call expression whose return value is discarded
    /// (statement position). Enables in-place mutations like `zz_vec_append`
    /// instead of copy-on-write `zz_vec_push`.
    pub(crate) void_context: std::cell::RefCell<bool>,
    /// Name of the current loop arena, if any. Used to emit arena-aware
    /// native calls (e.g. str_cast_arena) inside loops.
    pub(crate) current_loop_arena: std::cell::RefCell<Option<String>>,
    /// When true, the generated C omits the embedded runtime sources
    /// (`RUNTIME_C`) and only includes headers (`RUNTIME_H`). The C runtime
    /// is linked from a precompiled static library (`libzz_rt.a`) instead.
    pub(crate) precompiled: bool,
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
            closure_seq: std::cell::Cell::new(0),
            void_context: std::cell::RefCell::new(false),
            current_loop_arena: std::cell::RefCell::new(None),
            precompiled: false,
        }
    }

    /// Enable precompiled runtime mode: the generated C omits `RUNTIME_C`
    /// (the embedded runtime sources) and only includes headers. The
    /// runtime is linked from a precompiled `libzz_rt.a` instead.
    pub fn set_precompiled(&mut self, v: bool) {
        self.precompiled = v;
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
            Expr::Ident { name, .. } => names.lookup_type(name) == Some("string"),
            Expr::Path { parts, .. } => {
                let joined = parts.join(".");
                names.lookup_type(&joined) == Some("string")
            }
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
            zz_checker::Type::Void => "void".to_string(),
            zz_checker::Type::Ptr { mutable, inner } => {
                let base = self.c_abi_base(inner);
                if *mutable {
                    format!("{base} *")
                } else if base == "void" {
                    "const void *".to_string()
                } else {
                    format!("const {base} *")
                }
            }
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

    /// C identifier for a module-level global: `zz_global_<mangled>`.
    pub(super) fn global_cid(zz_name: &str) -> String {
        format!("zz_global_{}", mangle(zz_name))
    }

    /// Names visible as C globals (module-level bindings). Closure free
    /// variables matching these need no environment cell.
    pub(super) fn global_name_set(&self) -> std::collections::HashSet<String> {
        self.tp.bindings.keys().cloned().collect()
    }

    /// Names in `block` captured by a nested closure literal: bindings for
    /// these must be heap-cell-allocated so environments share them.
    pub(super) fn body_capture_set(
        &self,
        params: &[String],
        block: &zz_frontend::ast::Block,
    ) -> std::collections::HashSet<String> {
        let globals = self.global_name_set();
        zz_hir::captured_in_block(params, block, &globals)
    }

    /// Same as [`body_capture_set`](Self::body_capture_set) for closure
    /// bodies that are bare expressions rather than blocks.
    pub(super) fn expr_capture_set(
        &self,
        params: &[String],
        body: &zz_frontend::ast::Expr,
    ) -> std::collections::HashSet<String> {
        let globals = self.global_name_set();
        zz_hir::captured_in_expr(params, body, &globals)
    }

    /// Emit a heap cell allocation + initialization for a captured binding.
    /// `init` is the already-lowered (and scalar-unboxed, if applicable)
    /// initializer value of C type `ctype`.
    pub(super) fn emit_cell_alloc(&self, ptr: &str, ctype: &str, init: &str, out: &mut String) {
        out.push_str(&format!(
            "    {ctype} *{ptr} = ({ctype}*)malloc(sizeof({ctype}));\n"
        ));
        out.push_str(&format!("    *{ptr} = {init};\n"));
    }

    /// Collect module-level globals from top-level `Decl` statements.
    /// Returns sorted (zz_name, C id, C type, checker type) tuples.
    /// Types come from `tp.bindings` (checker-resolved); fallback zz_value.
    pub(super) fn collect_globals(
        &self,
    ) -> Vec<(String, String, String, Option<zz_checker::Type>)> {
        use zz_frontend::ast::Stmt;
        let mut names: Vec<String> = Vec::new();
        for stmt in self.tp.stmts() {
            if let Stmt::Decl { name, .. } = stmt {
                if !names.contains(&name.name) {
                    names.push(name.name.clone());
                }
            }
        }
        names.sort();
        names
            .into_iter()
            .map(|n| {
                let checker_ty = self.tp.bindings.get(&n).cloned();
                let ctype = checker_ty
                    .as_ref()
                    .map(|t| self.type_to_c(t))
                    .unwrap_or_else(|| "zz_value".to_string());
                let cid = Self::global_cid(&n);
                (n, cid, ctype, checker_ty)
            })
            .collect()
    }

    /// Seed a NameCtx with all module-level globals.
    pub(super) fn seed_globals(&self, names: &mut NameCtx) {
        for (zz_name, cid, ctype, checker_ty) in self.collect_globals() {
            names.insert_global(&zz_name, &cid, &ctype, checker_ty);
        }
    }

    /// Base C type for the pointee of a raw pointer (no trailing `*`).
    /// Nested pointers recurse through [`Lowerer::type_to_c`].
    pub(super) fn c_abi_base(&self, ty: &zz_checker::Type) -> String {
        match ty {
            zz_checker::Type::Int => "int64_t".to_string(),
            zz_checker::Type::Float => "double".to_string(),
            zz_checker::Type::Bool => "bool".to_string(),
            zz_checker::Type::Unit | zz_checker::Type::Void => "void".to_string(),
            zz_checker::Type::Ptr { .. } => self.type_to_c(ty),
            _ => "void".to_string(),
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

/// True if an emitted C expression is already a raw scalar (not a boxed
/// `zz_value` needing `.{i,f,b}` extraction): an explicit scalar cast, or
/// a bare variable whose known C type is scalar. `ident` is the ZZ source
/// name when the expression is a plain variable (C ids differ from ZZ
/// names, so the emitted string alone cannot be looked up). Without this
/// check, scalar-to-scalar copies (`s := sx`, `s = sy` between raw
/// locals) miscompile to `(v).f` on a plain `double`.
pub(crate) fn emitted_is_raw_scalar(emitted: &str, names: &NameCtx, ident: Option<&str>) -> bool {
    if emitted.starts_with("(int64_t)(")
        || emitted.starts_with("(double)(")
        || emitted.starts_with("(bool)(")
    {
        return true;
    }
    let mut candidates = Vec::new();
    if let Some(name) = ident {
        candidates.push(name);
    }
    if !emitted.is_empty()
        && emitted
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_')
    {
        candidates.push(emitted);
    }
    candidates.into_iter().any(|key| {
        matches!(
            names.lookup_type(key),
            Some("int64_t") | Some("double") | Some("bool")
        )
    })
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
        || expr.starts_with("zz_object_get_field(")
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

/// True if an expression can be duplicated in generated C without
/// changing program semantics (no side effects, no captured temporaries).
/// Used by pow strength reduction (`x ** 2` → `x * x`), which emits the
/// base expression twice.
pub(crate) fn is_dup_safe(e: &Expr) -> bool {
    match e {
        Expr::Int { .. }
        | Expr::Float { .. }
        | Expr::Bool { .. }
        | Expr::Str { .. }
        | Expr::Ident { .. }
        | Expr::Path { .. } => true,
        Expr::Paren { expr, .. } => is_dup_safe(expr),
        _ => false,
    }
}

/// Classify a binary operand as a recognized scalar shape, returning its C
/// type (`"int64_t"` / `"double"`) if so. Recognized shapes:
///   - Int literal  → `"int64_t"`
///   - Float literal (whole-number form, like `1.0` / `2.5`) → `"double"`
///   - Ident mapped to `int64_t`/`double`/`bool` in NameCtx
///   - Path (e.g. `main.x` global) mapped to scalar in NameCtx
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
        Expr::Path { parts, .. } => {
            // Globals lower as `ns.name`; struct field access (p.x) is not scalar.
            // Only treat as scalar when the joined name resolves to a scalar global
            // and it is NOT a struct-typed base (field access handled elsewhere).
            let joined = parts.join(".");
            // If base resolves to a struct type, this is field access, not a scalar var.
            if parts.len() == 2 {
                if let Some(base_ty) = names.lookup_type(&parts[0]) {
                    if base_ty.starts_with("zz_struct_") {
                        return None;
                    }
                }
            }
            match names.lookup_type(&joined)? {
                "int64_t" => Some("int64_t"),
                "double" => Some("double"),
                "bool" => Some("bool"),
                _ => None,
            }
        }
        Expr::Unary { op, expr, .. } => {
            let inner = scalar_operand_type(expr, names)?;
            match op {
                zz_frontend::ast::UnOp::Neg => Some(inner),
                zz_frontend::ast::UnOp::Pos => Some(inner),
                zz_frontend::ast::UnOp::Not => Some("bool"),
            }
        }
        Expr::Paren { expr, .. } => scalar_operand_type(expr, names),
        Expr::Binary {
            op, left, right, ..
        } => {
            // Fold nested integer/float arithmetic so strength-reduced
            // forms (e.g. `(i * i) % 97`) stay raw: both sides must be
            // the same numeric scalar type. Div is excluded (division
            // keeps boxed runtime error semantics); Rem with a literal
            // zero divisor is excluded (boxed div-by-zero guard).
            use zz_frontend::ast::BinOp::{Add, Mul, Rem, Sub};
            match op {
                Add | Sub | Mul | Rem => {
                    if matches!(op, Rem) && matches!(right.as_ref(), Expr::Int { value: 0, .. }) {
                        return None;
                    }
                    let lt = scalar_operand_type(left, names)?;
                    let rt = scalar_operand_type(right, names)?;
                    if lt == rt && (lt == "int64_t" || lt == "double") {
                        Some(lt)
                    } else {
                        None
                    }
                }
                _ => None,
            }
        }
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
                // identifier is unique, so we scan the scope for any entry
                // whose C identifier matches `emitted` and whose type is
                // a scalar. Cell derefs (`(*_cellN)`, `(*(T*)env[i])`) are
                // matched the same way so captured scalars box correctly.
                if is_simple_ident(emitted)
                    || emitted.starts_with("zz_global_")
                    || emitted.starts_with("(*")
                {
                    for entries in names.stack.values() {
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
                    for (cid, ty) in names.globals.values() {
                        if cid == emitted {
                            match ty.as_str() {
                                "int64_t" => return format!("zz_int({emitted})"),
                                "double" => return format!("zz_float({emitted})"),
                                "bool" => return format!("zz_bool({emitted})"),
                                _ => {}
                            }
                        }
                    }
                    for (cid, ty) in names.cap_deref.values() {
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
            // Look up via stack first, then globals. Scalar globals emit raw.
            if let Some(cid) = names.lookup(name) {
                let ty = names.lookup_type(name);
                match ty {
                    Some("int64_t") | Some("double") | Some("bool") => {
                        // Globals are raw scalars; locals may be boxed zz_value.
                        // If cid is a global (zz_global_), use directly.
                        if cid.starts_with("zz_global_") {
                            cid.to_string()
                        } else if let Some((_, t)) = names.stack.get(name).and_then(|v| v.last()) {
                            match t.as_str() {
                                "int64_t" => format!("({cid}).i"),
                                "double" => format!("({cid}).f"),
                                "bool" => format!("({cid}).b"),
                                _ => scrut_tmp.to_string(),
                            }
                        } else if let Some((_, t)) = names.cap_deref.get(name) {
                            // Closure capture: scalar captures already read raw.
                            match t.as_str() {
                                "int64_t" | "double" | "bool" => cid.to_string(),
                                _ => scrut_tmp.to_string(),
                            }
                        } else {
                            cid.to_string()
                        }
                    }
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
        Expr::Path { parts, .. } => {
            let joined = parts.join(".");
            if let Some(cid) = names.lookup(&joined) {
                let ty = names.lookup_type(&joined);
                match ty {
                    Some("int64_t") | Some("double") | Some("bool") => cid.to_string(),
                    _ => scrut_tmp.to_string(),
                }
            } else {
                scrut_tmp.to_string()
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
        Expr::Path { parts, .. } => {
            let joined = parts.join(".");
            names.lookup(&joined).map(|s| s.to_string())
        }
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
        Expr::Binary {
            op, left, right, ..
        } => {
            // Must agree with scalar_operand_type's Binary fold: only
            // emit raw C when the fold classified this node.
            let t = scalar_operand_type(e, names)?;
            debug_assert!(t == "int64_t" || t == "double");
            let l = scalar_operand_c(left, names)?;
            let r = scalar_operand_c(right, names)?;
            let c_op = match op {
                zz_frontend::ast::BinOp::Add => "+",
                zz_frontend::ast::BinOp::Sub => "-",
                zz_frontend::ast::BinOp::Mul => "*",
                zz_frontend::ast::BinOp::Rem => "%",
                _ => return None,
            };
            Some(format!("({l} {c_op} {r})"))
        }
        _ => None,
    }
}
