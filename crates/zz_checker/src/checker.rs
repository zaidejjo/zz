//! Type checker: HM-lite inference, generics, patterns, exhaustiveness.
//!
//! Design (Phase 1 scope):
//! - Inference is unification-based; every unresolved inference variable left
//!   in a top-level binding is an error (monomorphic, no generalization).
//! - Generic parameters are explicit (`func id<T>(...)`); at call sites they
//!   are instantiated with fresh variables.
//! - `Option`/`Result` are built-in; constructors are `.ok/.err/.some/.none`,
//!   and `?` propagates through enclosing functions returning the same shape.
//! - `match`/`if let` patterns are checked against the scrutinee type with
//!   exhaustiveness enforcement for Option/Result/bool.

use std::collections::HashMap;

use zz_frontend::ast::{
    BinOp, Block, Expr, FmtPart, Lit, MatchArm, Param, Pattern, Program, Stmt, Ty, TyKind, UnOp,
};
use zz_frontend::diag::{error_at, warning_at, FixIt, RawDiag};
use zz_frontend::levenshtein::suggest_all;
use zz_frontend::span::Span;

use crate::type_::Type;
use crate::unify::{Unifier, UnifyError};

/// A registered function signature.
#[derive(Debug, Clone)]
pub struct FuncSig {
    pub generics: Vec<String>,
    pub params: Vec<(String, Type)>,
    pub has_default: Vec<bool>,
    pub ret: Type,
}

/// A registered struct definition: field names and their types.
#[derive(Debug, Clone)]
pub struct StructSig {
    pub fields: Vec<(String, Type)>,
}

#[derive(Debug, Clone)]
pub struct CheckResult {
    pub errors: Vec<RawDiag>,
    /// Top-level `let` bindings and their types (fully resolved).
    pub bindings: HashMap<String, Type>,
    /// Top-level function signatures.
    pub funcs: HashMap<String, FuncSig>,
    /// Top-level struct definitions.
    pub structs: HashMap<String, StructSig>,
}

/// Type-check a whole program, seeded with bindings/funcs/structs from prior
/// REPL evals. Errors are collected (not fatal); the program should not run
/// if any are present.
pub fn check_program(
    program: &Program,
    initial_bindings: HashMap<String, Type>,
    initial_funcs: HashMap<String, FuncSig>,
    initial_structs: HashMap<String, StructSig>,
) -> CheckResult {
    let mut checker = Checker::new(initial_bindings, initial_funcs, initial_structs);

    // Pass 1: register struct definitions (fields are resolved against the
    // struct registry, so structs may reference earlier structs). Structs
    // must be registered before functions so `func f(p: Point)` resolves.
    let mut seen_structs = HashMap::new();
    for stmt in &program.stmts {
        if let Stmt::Struct { name, span, .. } = stmt {
            let full_name = name.join(".");
            if let Some(prev) = seen_structs.insert(full_name.clone(), *span) {
                checker.errors.push(error_at(
                    format!("duplicate definition of struct `{}`", full_name),
                    *span,
                ));
                checker
                    .errors
                    .push(error_at("previous definition here", prev));
            }
            checker.collect_struct(stmt);
        }
    }

    // Pass 1b: register all function signatures so recursion and mutual
    // recursion resolve.
    let mut seen = HashMap::new();
    for stmt in &program.stmts {
        if let Stmt::Func { name, span, .. } = stmt {
            let full_name = name.join(".");
            if let Some(prev) = seen.insert(full_name.clone(), *span) {
                checker.errors.push(error_at(
                    format!("duplicate definition of function `{}`", full_name),
                    *span,
                ));
                checker
                    .errors
                    .push(error_at("previous definition here", prev));
            }
            checker.collect_func(stmt);
        }
    }

    // Pass 2: check top-level statements in order.
    for stmt in &program.stmts {
        checker.check_stmt(stmt);
    }

    // Finalize: bindings that still contain inference variables were already
    // reported inline (see check_stmt `Let`); skip them so the session never
    // seeds an unresolved type.
    let mut bindings = HashMap::new();
    for (name, ty) in &checker.new_bindings {
        let rt = checker.unifier.resolve_deep(ty);
        if !contains_var(&rt) {
            bindings.insert(name.clone(), rt.clone());
        }
    }

    // Emit unused variable warnings for the global scope (the top scope
    // is never popped, so pop_scope's check never fires for it).
    checker.emit_global_unused_warnings();

    CheckResult {
        errors: checker.errors,
        bindings,
        funcs: checker.funcs,
        structs: checker.structs,
    }
}

fn contains_var(t: &Type) -> bool {
    match t {
        Type::Var(_) => true,
        Type::Tuple(ts) => ts.iter().any(contains_var),
        Type::Option(x) => contains_var(x),
        Type::Result(a, b) => contains_var(a) || contains_var(b),
        Type::Func(ps, r) => ps.iter().any(contains_var) || contains_var(r),
        Type::Array(x) => contains_var(x),
        Type::Dict(k, v) => contains_var(k) || contains_var(v),
        Type::Union(ts) => ts.iter().any(contains_var),
        Type::Range(x) => contains_var(x),
        _ => false,
    }
}

/// Replace unresolved type variables inside `Option`/`Result` with `unit`
/// so `.none`/`.ok`/`.err` bindings type-check without annotations.
fn default_variant_vars(t: &mut Type) {
    match t {
        Type::Option(inner) => {
            if contains_var(inner) {
                default_variant_vars(inner);
                if contains_var(inner) {
                    **inner = Type::Unit;
                }
            }
        }
        Type::Result(ok, err) => {
            if contains_var(ok) {
                default_variant_vars(ok);
                if contains_var(ok) {
                    **ok = Type::Unit;
                }
            }
            if contains_var(err) {
                default_variant_vars(err);
                if contains_var(err) {
                    **err = Type::Unit;
                }
            }
        }
        _ => {}
    }
}

struct Checker {
    unifier: Unifier,
    errors: Vec<RawDiag>,
    funcs: HashMap<String, FuncSig>,
    structs: HashMap<String, StructSig>,
    env: Vec<HashMap<String, Type>>,
    /// Top-level let bindings discovered this run: name → (type, span).
    new_bindings: HashMap<String, Type>,
    current_ret: Option<Type>,
    current_generics: Vec<String>,
    /// Nesting depth of `for`/`while` loops (for `break`/`continue`).
    loop_depth: usize,
    /// Names that were used (looked up) — for unused-variable warnings.
    used_names: std::collections::HashSet<String>,
    /// Names defined in the current scope with their spans — for unused
    /// variable warnings. Each scope level has its own map.
    defined_names: Vec<HashMap<String, Span>>,
    /// Tracks whether the most recent `lookup()` produced an "undefined
    /// variable" error.  Used by `check_call` to suppress the secondary
    /// "cannot call a value of type unit" cascading error.
    had_undefined_var: bool,
    /// Imported namespaces: (alias, span). Used to detect unused imports.
    imports: Vec<(String, Span)>,
}

impl Checker {
    fn new(
        initial_bindings: HashMap<String, Type>,
        funcs: HashMap<String, FuncSig>,
        structs: HashMap<String, StructSig>,
    ) -> Self {
        let env = vec![initial_bindings];
        Checker {
            unifier: Unifier::new(),
            errors: Vec::new(),
            funcs,
            structs,
            env,
            new_bindings: HashMap::new(),
            current_ret: None,
            current_generics: Vec::new(),
            loop_depth: 0,
            used_names: std::collections::HashSet::new(),
            defined_names: vec![HashMap::new()],
            had_undefined_var: false,
            imports: Vec::new(),
        }
    }

    // --- environments -----------------------------------------------------

    fn push_scope(&mut self) {
        self.env.push(HashMap::new());
        self.defined_names.push(HashMap::new());
    }

    /// Strip the module prefix from a name for display.
    /// `"diag-typo.area"` → `"area"`, `"area"` → `"area"`.
    fn display_name(name: &str) -> &str {
        match name.rfind('.') {
            Some(pos) => &name[pos + 1..],
            None => name,
        }
    }

    fn pop_scope(&mut self) {
        // Warn about unused variables in this scope (skip the global scope).
        if let Some(defined) = self.defined_names.pop() {
            for (name, span) in &defined {
                let display = Self::display_name(name);
                if !self.used_names.contains(name) && !display.starts_with('_') {
                    let fixit_name = format!("_{display}");
                    self.errors.push(
                        warning_at(
                            format!(
                                "unused variable `{display}`, consider prefixing with `_{display}`"
                            ),
                            *span,
                        )
                        .with_note("variable is never read")
                        .with_fixit(FixIt::safe(
                            *span,
                            fixit_name,
                            "rename to",
                        )),
                    );
                }
            }
        }
        self.env.pop();
    }

    fn define(&mut self, name: &str, ty: Type) {
        self.define_at(name, ty, Span::new(0, 0));
    }

    fn define_at(&mut self, name: &str, ty: Type, span: Span) {
        self.env.last_mut().unwrap().insert(name.to_string(), ty);
        // Record the definition span for unused-variable warnings.
        if let Some(scope) = self.defined_names.last_mut() {
            scope.insert(name.to_string(), span);
        }
    }

    /// Emit unused-variable warnings for the global scope (scope index 0).
    /// The global scope is never popped, so `pop_scope`'s check does not
    /// fire for top-level definitions. Also emits unused-import warnings.
    fn emit_global_unused_warnings(&mut self) {
        // --- Unused variables ---
        if let Some(defined) = self.defined_names.first() {
            // Snapshot defined names to avoid borrow issues.
            let entries: Vec<(String, Span)> =
                defined.iter().map(|(k, &v)| (k.clone(), v)).collect();
            for (name, span) in &entries {
                let display = Self::display_name(name);
                if !self.used_names.contains(name) && !display.starts_with('_') {
                    let fixit_name = format!("_{display}");
                    self.errors.push(
                        warning_at(
                            format!(
                                "unused variable `{display}`, consider prefixing with `_{display}`"
                            ),
                            *span,
                        )
                        .with_note("variable is never read")
                        .with_fixit(FixIt::safe(
                            *span,
                            fixit_name,
                            "rename to",
                        )),
                    );
                }
            }
        }

        // --- Unused imports ---
        let imports: Vec<(String, Span)> = self.imports.clone();
        for (alias, span) in &imports {
            let prefix = format!("{alias}.");
            let used = self
                .used_names
                .iter()
                .any(|n| n == alias || n.starts_with(&prefix));
            if !used {
                self.errors.push(
                    warning_at(format!("unused import `{alias}`"), *span)
                        .with_note("remove this import or use it in the program"),
                );
            }
        }
    }

    fn lookup(&mut self, name: &str, span: Span) -> Type {
        match self.lookup_opt(name) {
            Some(t) => {
                self.used_names.insert(name.to_string());
                t
            }
            None => {
                if let Some(sig) = self.funcs.get(name) {
                    if !sig.generics.is_empty() {
                        self.errors.push(error_at(
                            format!(
                                "cannot use generic function `{name}` as a value; call it with arguments"
                            ),
                            span,
                        ));
                        return Type::Error;
                    }
                }
                // Build candidate list for typo suggestions.
                let mut candidates: Vec<&str> = Vec::new();
                for scope in self.env.iter().rev() {
                    for key in scope.keys() {
                        candidates.push(key);
                    }
                }
                for key in self.funcs.keys() {
                    candidates.push(key);
                }
                for key in self.structs.keys() {
                    candidates.push(key);
                }
                // Also add bare-name versions for module-qualified candidates.
                // e.g. "diag-typo.height" → also add "height" so Levenshtein
                // can match "hieght" → "height".
                let mut extras: Vec<String> = Vec::new();
                for c in &candidates {
                    if let Some(bare) = c.rsplit('.').next() {
                        if bare != *c {
                            extras.push(bare.to_string());
                        }
                    }
                }
                for e in &extras {
                    candidates.push(e);
                }
                let mut diag = error_at(format!("undefined variable `{name}`"), span);
                let all = suggest_all(name, &candidates);
                if let Some((suggestion, _dist)) = all.first() {
                    diag = diag.with_note(format!("did you mean `{suggestion}`?"));
                    let fixit = if all.len() == 1 {
                        FixIt::safe(span, suggestion.to_string(), "replace variable")
                    } else {
                        let alts: Vec<String> = all.iter().map(|(s, _)| s.to_string()).collect();
                        FixIt::ambiguous(span, suggestion.to_string(), "replace variable", alts)
                    };
                    diag = diag.with_fixit(fixit);
                }

                // If the name looks like `module.func` and matches a known
                // stdlib pattern, suggest adding the import.
                if let Some((module, _func)) = name.split_once('.') {
                    let std_module = match module {
                        "io" | "str" | "vec" | "json" | "http" | "fs" | "env" | "math" | "time" => {
                            Some(module)
                        }
                        _ => None,
                    };
                    if let Some(mod_name) = std_module {
                        let import_stmt = format!("import std.{mod_name}");
                        diag = diag.with_note(format!(
                            "add `{import_stmt}` at the top of the file to use `{name}`"
                        ));
                        // Create an "add import" fixit that inserts at the top.
                        diag = diag.with_fixit(FixIt::safe(
                            Span::new(0, 0),
                            format!("{import_stmt}\n"),
                            "add import",
                        ));
                    }
                }
                self.errors.push(diag);
                self.had_undefined_var = true;
                Type::Error
            }
        }
    }

    /// Like [`Checker::lookup`] but without the error: returns `None` when
    /// the name is not bound anywhere.
    fn lookup_opt(&mut self, name: &str) -> Option<Type> {
        for scope in self.env.iter().rev() {
            if let Some(t) = scope.get(name) {
                self.used_names.insert(name.to_string());
                return Some(t.clone());
            }
        }
        if let Some(sig) = self.funcs.get(name) {
            self.used_names.insert(name.to_string());
            if !sig.generics.is_empty() {
                return None;
            }
            // Function used as a value: give its (uninstantiated) type. Call
            // sites handle generic instantiation via the Named path below.
            return Some(Type::Named(name.to_string()));
        }
        None
    }

    /// Resolve a dotted path: first as a single qualified name (module
    /// bindings/functions), then as a struct-field walk from the first part.
    fn lookup_path(&mut self, parts: &[String], span: Span) -> Type {
        let joined = parts.join(".");
        if let Some(t) = self.lookup_opt(&joined) {
            return t;
        }
        let Some(root) = self.lookup_opt(&parts[0]) else {
            // Suggest typo corrections for the root name.
            let mut candidates: Vec<&str> = Vec::new();
            for scope in self.env.iter().rev() {
                for key in scope.keys() {
                    candidates.push(key);
                }
            }
            for key in self.funcs.keys() {
                candidates.push(key);
            }
            for key in self.structs.keys() {
                candidates.push(key);
            }
            let mut diag = error_at(format!("undefined variable `{joined}`"), span);
            let all = suggest_all(&parts[0], &candidates);
            if let Some((suggestion, _)) = all.first() {
                diag = diag.with_note(format!("did you mean `{suggestion}`?"));
                let root_span = Span::new(span.start, span.start + parts[0].len() as u32);
                let fixit = if all.len() == 1 {
                    FixIt::safe(root_span, suggestion.to_string(), "replace variable")
                } else {
                    let alts: Vec<String> = all.iter().map(|(s, _)| s.to_string()).collect();
                    FixIt::ambiguous(root_span, suggestion.to_string(), "replace variable", alts)
                };
                diag = diag.with_fixit(fixit);
            }
            self.errors.push(diag);
            self.had_undefined_var = true;
            return Type::Error;
        };
        let mut ty = root;
        for field in &parts[1..] {
            match self.unifier.resolve(&ty) {
                Type::Struct(name) => match self.structs.get(&name) {
                    Some(sig) => match sig.fields.iter().find(|(n, _)| n == field) {
                        Some((_, ft)) => ty = ft.clone(),
                        None => {
                            // Suggest closest field name.
                            let field_names: Vec<&str> =
                                sig.fields.iter().map(|(n, _)| n.as_str()).collect();
                            let mut diag =
                                error_at(format!("struct `{name}` has no field `{field}`"), span);
                            let all = suggest_all(field, &field_names);
                            if let Some((suggestion, _)) = all.first() {
                                diag =
                                    diag.with_note(format!("did you mean field `{suggestion}`?"));
                                let field_span = Span::new(span.end - field.len() as u32, span.end);
                                let fixit = if all.len() == 1 {
                                    FixIt::safe(field_span, suggestion.to_string(), "replace field")
                                } else {
                                    let alts: Vec<String> =
                                        all.iter().map(|(s, _)| s.to_string()).collect();
                                    FixIt::ambiguous(
                                        field_span,
                                        suggestion.to_string(),
                                        "replace field",
                                        alts,
                                    )
                                };
                                diag = diag.with_fixit(fixit);
                            }
                            self.errors.push(diag);
                            return Type::Error;
                        }
                    },
                    None => {
                        self.errors
                            .push(error_at(format!("unknown struct `{name}`"), span));
                        return Type::Error;
                    }
                },
                other => {
                    self.errors.push(error_at(
                        format!("cannot access field `{field}` on a value of type `{other}`"),
                        span,
                    ));
                    return Type::Error;
                }
            }
        }
        ty
    }

    // --- statements -------------------------------------------------------

    fn check_stmt(&mut self, stmt: &Stmt) -> Type {
        self.had_undefined_var = false;
        match stmt {
            Stmt::Decl {
                name,
                ty,
                value,
                span: _,
            } => {
                let vt = self.check_expr(value);
                if let Some(ann) = ty {
                    let gens = self.current_generics.clone();
                    let at = self.ast_to_type(ann, &gens);
                    if let Err(e) = self.unifier.unify(&vt, &at) {
                        self.report_mismatch(e, ann.span);
                    }
                }
                let rt = self.unifier.resolve_deep(&vt);
                if contains_var(&rt) {
                    // `.none`/`.ok`/`.err` bindings have an unconstrained
                    // variant parameter; default it to `unit` so they can be
                    // stored without an annotation.
                    let mut d = rt.clone();
                    default_variant_vars(&mut d);
                    if contains_var(&d) {
                        self.errors.push(error_at(
                            format!(
                                "cannot infer the type of `{}`; add a type annotation",
                                name.name
                            ),
                            name.span,
                        ));
                    } else {
                        self.define_at(&name.name, d.clone(), name.span);
                        if self.env.len() == 1 {
                            self.new_bindings.insert(name.name.clone(), d.clone());
                        }
                        return d;
                    }
                }
                if self.env.len() == 1 {
                    self.new_bindings.insert(name.name.clone(), vt.clone());
                }
                self.define_at(&name.name, rt.clone(), name.span);
                rt
            }
            Stmt::Import { path, alias, span } => {
                // Track the import namespace for unused-import detection.
                // The alias (if present) or the last segment of the path
                // becomes the namespace prefix used in the program.
                let ns = alias
                    .as_ref()
                    .cloned()
                    .or_else(|| path.last().cloned())
                    .unwrap_or_default();
                self.imports.push((ns, *span));
                Type::Unit
            }
            Stmt::Func { .. } => {
                // Signature registered in pass 1; check the body against it.
                let sig = self.funcs.get(&func_name(stmt)).unwrap().clone();
                self.check_func_body(stmt, &sig);
                Type::Unit
            }
            Stmt::Return { value, span } => {
                let ret = match self.current_ret.clone() {
                    Some(r) => r,
                    None => {
                        self.errors
                            .push(error_at("`return` outside of a function", *span));
                        Type::Unit
                    }
                };
                match value {
                    Some(v) => {
                        let vt = self.check_expr(v);
                        if let Err(e) = self.unifier.unify(&vt, &ret) {
                            self.report_mismatch(e, v.span());
                        }
                        vt
                    }
                    None => {
                        if let Err(e) = self.unifier.unify(&Type::Unit, &ret) {
                            self.report_mismatch(e, *span);
                        }
                        Type::Unit
                    }
                }
            }
            Stmt::Expr(e) => self.check_expr(e),
            Stmt::Struct { .. } => {
                // Registered in pass 1b; nothing to check in the body.
                Type::Unit
            }
            Stmt::For {
                var,
                iter,
                body,
                span,
            } => {
                let it = self.check_expr(iter);
                let it = self.unifier.resolve(&it);
                let elem = match it {
                    Type::Array(elem) => *elem,
                    Type::Range(elem) => *elem,
                    Type::Var(_) => {
                        self.errors.push(error_at(
                            "cannot iterate a value whose type could not be inferred",
                            *span,
                        ));
                        Type::Unit
                    }
                    other => {
                        self.errors.push(error_at(
                            format!("cannot iterate a value of type `{other}`"),
                            *span,
                        ));
                        Type::Unit
                    }
                };
                self.push_scope();
                self.define(&var.name, elem);
                self.loop_depth += 1;
                self.check_block(body);
                self.loop_depth -= 1;
                self.pop_scope();
                Type::Unit
            }
            Stmt::Break { span } => {
                if self.loop_depth == 0 {
                    self.errors
                        .push(error_at("`break` outside of a loop", *span));
                }
                Type::Unit
            }
            Stmt::Continue { span } => {
                if self.loop_depth == 0 {
                    self.errors
                        .push(error_at("`continue` outside of a loop", *span));
                }
                Type::Unit
            }
            Stmt::Defer { expr, span } => {
                if self.current_ret.is_none() {
                    self.errors
                        .push(error_at("`defer` outside of a function", *span));
                }
                self.check_expr(expr);
                Type::Unit
            }
            Stmt::Assign {
                target,
                value,
                span,
            } => {
                let errors_before = self.errors.len();
                let tt = self.check_assign_target(target);
                let vt = self.check_expr(value);
                // Skip the unify when the target itself was rejected — the
                // target error is the real diagnosis; don't pile on a type
                // mismatch against the placeholder `unit`.
                if self.errors.len() == errors_before {
                    if let Err(e) = self.unifier.unify(&vt, &tt) {
                        self.report_mismatch(e, *span);
                    }
                }
                Type::Unit
            }
        }
    }

    /// Type of an assignment target: a variable, a qualified name, or a
    /// struct field path.
    fn check_assign_target(&mut self, target: &Expr) -> Type {
        match target {
            Expr::Ident { name, span } => self.lookup(name, *span),
            Expr::Path { parts, span } => self.lookup_path(parts, *span),
            Expr::Field { obj, name, span } => {
                let ot = self.check_expr(obj);
                let ot = self.unifier.resolve(&ot);
                match ot {
                    Type::Struct(sname) => match self.structs.get(&sname) {
                        Some(sig) => match sig.fields.iter().find(|(n, _)| n == name) {
                            Some((_, ft)) => ft.clone(),
                            None => {
                                let field_names: Vec<&str> =
                                    sig.fields.iter().map(|(n, _)| n.as_str()).collect();
                                let mut diag = error_at(
                                    format!("struct `{sname}` has no field `{name}`"),
                                    *span,
                                );
                                let all = suggest_all(name, &field_names);
                                if let Some((suggestion, _)) = all.first() {
                                    diag = diag
                                        .with_note(format!("did you mean field `{suggestion}`?"));
                                    let field_span =
                                        Span::new(span.end - name.len() as u32, span.end);
                                    let alts: Vec<String> =
                                        all.iter().map(|(s, _)| s.to_string()).collect();
                                    let fixit = if all.len() == 1 {
                                        FixIt::safe(
                                            field_span,
                                            suggestion.to_string(),
                                            "replace field",
                                        )
                                    } else {
                                        FixIt::ambiguous(
                                            field_span,
                                            suggestion.to_string(),
                                            "replace field",
                                            alts,
                                        )
                                    };
                                    diag = diag.with_fixit(fixit);
                                }
                                self.errors.push(diag);
                                Type::Unit
                            }
                        },
                        None => {
                            self.errors
                                .push(error_at(format!("unknown struct `{sname}`"), *span));
                            Type::Unit
                        }
                    },
                    other => {
                        self.errors.push(error_at(
                            format!("cannot assign to field `{name}` of a value of type `{other}`"),
                            *span,
                        ));
                        Type::Unit
                    }
                }
            }
            Expr::Index { obj, index, span } => {
                let ot = self.check_expr(obj);
                let ot = self.unifier.resolve(&ot);
                let it = self.check_expr(index);
                match ot {
                    Type::Array(elem) => {
                        self.ensure_int(it, index.span());
                        *elem
                    }
                    Type::Dict(k, v) => {
                        if let Err(e) = self.unifier.unify(&it, &k) {
                            self.report_mismatch(e, index.span());
                        }
                        *v
                    }
                    Type::Str => {
                        self.errors
                            .push(error_at("cannot assign to an index of a string", *span));
                        Type::Unit
                    }
                    Type::Var(_) => {
                        self.errors.push(error_at(
                            "cannot assign to an index of a value whose type could not be inferred",
                            *span,
                        ));
                        Type::Unit
                    }
                    other => {
                        self.errors.push(error_at(
                            format!("cannot assign to an index of a value of type `{other}`"),
                            *span,
                        ));
                        Type::Unit
                    }
                }
            }
            other => {
                self.errors.push(error_at(
                    "cannot assign to this expression".to_string(),
                    other.span(),
                ));
                Type::Unit
            }
        }
    }

    fn check_func_body(&mut self, stmt: &Stmt, sig: &FuncSig) {
        let (name, body) = match stmt {
            Stmt::Func { name, body, .. } => (name, body),
            _ => unreachable!(),
        };
        self.push_scope();
        for (pname, pty) in &sig.params {
            self.define(pname, pty.clone());
        }
        let prev_ret = self.current_ret.replace(sig.ret.clone());
        let prev_gen = std::mem::replace(&mut self.current_generics, sig.generics.clone());
        let body_t = self.check_block(body);
        self.current_ret = prev_ret;
        self.current_generics = prev_gen;
        self.pop_scope();
        let _ = name;
        if let Err(e) = self.unifier.unify(&body_t, &sig.ret) {
            self.report_mismatch(e, body.span);
        }
    }

    fn check_block(&mut self, block: &Block) -> Type {
        self.push_scope();
        let mut result = Type::Unit;
        for stmt in &block.stmts {
            result = self.check_stmt(stmt);
        }
        self.pop_scope();
        result
    }

    fn collect_struct(&mut self, stmt: &Stmt) {
        let (name, fields) = match stmt {
            Stmt::Struct { name, fields, .. } => (name, fields),
            _ => unreachable!(),
        };
        let gens = self.current_generics.clone();
        let sig_fields = fields
            .iter()
            .map(|(fname, fty)| (fname.name.clone(), self.ast_to_type(fty, &gens)))
            .collect();
        let full_name = name.join(".");
        self.structs
            .insert(full_name, StructSig { fields: sig_fields });
    }

    fn collect_func(&mut self, stmt: &Stmt) {
        let (name, generics, params, ret) = match stmt {
            Stmt::Func {
                name,
                generics,
                params,
                ret,
                ..
            } => (name, generics, params, ret),
            _ => unreachable!(),
        };
        let gen_names: Vec<String> = generics.iter().map(|g| g.name.clone()).collect();
        let sig_params: Vec<(String, Type)> = params
            .iter()
            .map(|p| {
                let ty = match &p.ty {
                    Some(t) => self.ast_to_type(t, &gen_names),
                    None => self.unifier.fresh_var(),
                };
                (p.name.name.clone(), ty)
            })
            .collect();
        let has_default: Vec<bool> = params.iter().map(|p| p.default.is_some()).collect();
        let sig_ret = match ret {
            Some(t) => self.ast_to_type(t, &gen_names),
            None => self.unifier.fresh_var(),
        };
        let full_name = name.join(".");
        self.funcs.insert(
            full_name,
            FuncSig {
                generics: gen_names,
                params: sig_params,
                has_default,
                ret: sig_ret,
            },
        );
    }

    // --- expressions ------------------------------------------------------

    /// Merge element types into a single type: identical types collapse to
    /// one; differing types form a union.
    fn merge_types(&mut self, types: Vec<Type>) -> Type {
        if types.is_empty() {
            return self.unifier.fresh_var();
        }
        let mut distinct: Vec<Type> = Vec::new();
        for t in types {
            let rt = self.unifier.resolve(&t);
            if let Some(existing) = distinct.iter().find(|d| self.unifier.resolve(d) == rt) {
                let _ = self.unifier.unify(existing, &t);
            } else {
                distinct.push(t);
            }
        }
        if distinct.len() == 1 {
            distinct.pop().unwrap()
        } else {
            Type::Union(distinct)
        }
    }

    fn check_expr(&mut self, e: &Expr) -> Type {
        match e {
            Expr::Int { .. } => Type::Int,
            Expr::Float { .. } => Type::Float,
            Expr::Str { .. } => Type::Str,
            Expr::Bool { .. } => Type::Bool,
            Expr::Ident { name, span } => self.lookup(name, *span),
            Expr::Path { parts, span } => self.lookup_path(parts, *span),
            Expr::Field { obj, name, span } => {
                let ot = self.check_expr(obj);
                let ot = self.unifier.resolve(&ot);
                match ot {
                    Type::Struct(sname) => match self.structs.get(&sname) {
                        Some(sig) => match sig.fields.iter().find(|(n, _)| n == name) {
                            Some((_, ft)) => ft.clone(),
                            None => {
                                let field_names: Vec<&str> =
                                    sig.fields.iter().map(|(n, _)| n.as_str()).collect();
                                let mut diag = error_at(
                                    format!("struct `{sname}` has no field `{name}`"),
                                    *span,
                                );
                                let all = suggest_all(name, &field_names);
                                if let Some((suggestion, _)) = all.first() {
                                    diag = diag
                                        .with_note(format!("did you mean field `{suggestion}`?"));
                                    let field_span =
                                        Span::new(span.end - name.len() as u32, span.end);
                                    let alts: Vec<String> =
                                        all.iter().map(|(s, _)| s.to_string()).collect();
                                    let fixit = if all.len() == 1 {
                                        FixIt::safe(
                                            field_span,
                                            suggestion.to_string(),
                                            "replace field",
                                        )
                                    } else {
                                        FixIt::ambiguous(
                                            field_span,
                                            suggestion.to_string(),
                                            "replace field",
                                            alts,
                                        )
                                    };
                                    diag = diag.with_fixit(fixit);
                                }
                                self.errors.push(diag);
                                Type::Unit
                            }
                        },
                        None => {
                            self.errors
                                .push(error_at(format!("unknown struct `{sname}`"), *span));
                            Type::Unit
                        }
                    },
                    other => {
                        // Try method dispatch for non-struct types (Option,
                        // Result, str, vec) — same lookup as check_call's
                        // Expr::Field callee path.
                        let method = name.clone();
                        let ns = match &other {
                            Type::Str => Some("str"),
                            Type::Array(_) => Some("vec"),
                            Type::Option(_) => Some("option"),
                            Type::Result(_, _) => Some("result"),
                            _ => None,
                        };
                        if let Some(ns) = ns {
                            if let Some(sig) = self.funcs.get(&format!("{ns}.{method}")).cloned() {
                                let (ps, ret) = self.instantiate(&sig);
                                if !ps.is_empty() {
                                    if let Err(e) = self.unifier.unify(&other, &ps[0]) {
                                        self.report_mismatch(e, *span);
                                    }
                                }
                                return ret;
                            }
                        }
                        self.errors.push(error_at(
                            format!("cannot access field `{name}` on a value of type `{other}`"),
                            *span,
                        ));
                        Type::Unit
                    }
                }
            }
            Expr::Range { start, end, .. } => {
                let st = self.check_expr(start);
                let et = self.check_expr(end);
                for (t, s) in [(st, start.span()), (et, end.span())] {
                    match self.unifier.resolve(&t) {
                        Type::Int => {}
                        Type::Var(id) => {
                            self.unifier.bind(id, Type::Int);
                        }
                        other => {
                            self.errors.push(error_at(
                                format!("range bounds must be `int`, found `{other}`"),
                                s,
                            ));
                        }
                    }
                }
                Type::Range(Box::new(Type::Int))
            }
            Expr::StructInit { name, fields, span } => {
                let Some(sig) = self.structs.get(name).cloned() else {
                    self.errors
                        .push(error_at(format!("unknown struct `{name}`"), *span));
                    return Type::Unit;
                };
                for (fname, fval) in fields {
                    let Some((_, ft)) = sig.fields.iter().find(|(n, _)| n == fname) else {
                        self.errors.push(error_at(
                            format!("struct `{name}` has no field `{fname}`"),
                            fval.span(),
                        ));
                        continue;
                    };
                    let vt = self.check_expr(fval);
                    if let Err(e) = self.unifier.unify(&vt, ft) {
                        self.report_mismatch(e, fval.span());
                    }
                }
                Type::Struct(name.clone())
            }
            Expr::Index { obj, index, span } => {
                let ot = self.check_expr(obj);
                let ot = self.unifier.resolve(&ot);
                let it = self.check_expr(index);
                match ot {
                    Type::Array(elem) => {
                        self.ensure_int(it, index.span());
                        *elem
                    }
                    Type::Dict(k, v) => {
                        if let Err(e) = self.unifier.unify(&it, &k) {
                            self.report_mismatch(e, index.span());
                        }
                        *v
                    }
                    Type::Str => {
                        self.ensure_int(it, index.span());
                        Type::Str
                    }
                    Type::Var(_) => {
                        self.errors.push(error_at(
                            "cannot index a value whose type could not be inferred",
                            *span,
                        ));
                        Type::Unit
                    }
                    other => {
                        self.errors.push(error_at(
                            format!("cannot index a value of type `{other}`"),
                            *span,
                        ));
                        Type::Unit
                    }
                }
            }
            Expr::Slice {
                obj,
                start,
                end,
                span,
            } => {
                let ot = self.check_expr(obj);
                let ot = self.unifier.resolve(&ot);
                for bound in [start.as_deref(), end.as_deref()].into_iter().flatten() {
                    let bt = self.check_expr(bound);
                    self.ensure_int(bt, bound.span());
                }
                match ot {
                    Type::Array(elem) => Type::Array(elem),
                    Type::Str => Type::Str,
                    Type::Var(_) => {
                        self.errors.push(error_at(
                            "cannot slice a value whose type could not be inferred",
                            *span,
                        ));
                        Type::Unit
                    }
                    other => {
                        self.errors.push(error_at(
                            format!("cannot slice a value of type `{other}`"),
                            *span,
                        ));
                        Type::Unit
                    }
                }
            }
            Expr::Fmt { parts, .. } => {
                // Interpolated strings are `str`; embedded expressions are
                // checked for validity (their Display form is used at runtime).
                for part in parts {
                    if let FmtPart::Expr(e, _) = part {
                        let _ = self.check_expr(e);
                    }
                }
                Type::Str
            }
            Expr::Paren { expr, .. } => self.check_expr(expr),
            Expr::Unary { op, expr, span } => self.check_unary(*op, expr, *span),
            Expr::Binary {
                op,
                left,
                right,
                span,
            } => self.check_binary(*op, left, right, *span),
            Expr::Call {
                callee,
                args,
                named,
                span,
            } => self.check_call(callee, args, named, *span),
            Expr::Closure { params, body, span } => self.check_closure(params, body, *span),
            Expr::If {
                cond,
                then,
                els,
                span,
            } => {
                let ct = self.check_expr(cond);
                self.ensure_bool(ct, cond.span());
                let tt = self.check_block(then);
                match els {
                    Some(e) => {
                        let et = self.check_expr(e);
                        if let Err(err) = self.unifier.unify(&et, &tt) {
                            self.report_mismatch(err, e.span());
                        }
                    }
                    None => {
                        if let Err(err) = self.unifier.unify(&Type::Unit, &tt) {
                            self.report_mismatch(err, *span);
                        }
                    }
                }
                tt
            }
            Expr::While { cond, body, .. } => {
                let ct = self.check_expr(cond);
                self.ensure_bool(ct, cond.span());
                self.loop_depth += 1;
                self.check_block(body);
                self.loop_depth -= 1;
                Type::Unit
            }
            Expr::Match {
                scrutinee,
                arms,
                span,
            } => self.check_match(scrutinee, arms, *span),
            Expr::IfLet {
                pat,
                value,
                then,
                els,
                span,
            } => {
                let vt = self.check_expr(value);
                self.push_scope();
                self.bind_pattern(pat, &vt);
                let tt = self.check_block(then);
                self.pop_scope();
                match els {
                    Some(e) => {
                        let et = self.check_expr(e);
                        if let Err(err) = self.unifier.unify(&et, &tt) {
                            self.report_mismatch(err, e.span());
                        }
                    }
                    None => {
                        if let Err(err) = self.unifier.unify(&Type::Unit, &tt) {
                            self.report_mismatch(err, *span);
                        }
                    }
                }
                tt
            }
            Expr::Try { expr, span } => self.check_try(expr, *span),
            Expr::Block(b) => self.check_block(b),
            Expr::Array { elems, span: _ } => {
                let types: Vec<Type> = elems.iter().map(|e| self.check_expr(e)).collect();
                let elem_t = self.merge_types(types);
                Type::Array(Box::new(elem_t))
            }
            Expr::ListComp {
                body,
                var,
                iter,
                filter,
                span,
            } => {
                let it = self.check_expr(iter);
                let it = self.unifier.resolve(&it);
                let elem = match it {
                    Type::Array(elem) => *elem,
                    Type::Range(elem) => *elem,
                    Type::Var(_) => {
                        self.errors.push(error_at(
                            "cannot iterate a value whose type could not be inferred",
                            *span,
                        ));
                        Type::Unit
                    }
                    other => {
                        self.errors.push(error_at(
                            format!("cannot iterate a value of type `{other}`"),
                            *span,
                        ));
                        Type::Unit
                    }
                };
                self.push_scope();
                self.define(&var.name, elem);
                if let Some(f) = filter {
                    let ft = self.check_expr(f);
                    let ft = self.unifier.resolve(&ft);
                    if let Err(e) = self.unifier.unify(&ft, &Type::Bool) {
                        self.report_mismatch(e, f.span());
                    }
                }
                let body_t = self.check_expr(body);
                self.pop_scope();
                Type::Array(Box::new(body_t))
            }
            Expr::Dict { entries, span: _ } => {
                let mut key_types = Vec::new();
                let mut val_types = Vec::new();
                for (k, v) in entries {
                    key_types.push(self.check_expr(k));
                    val_types.push(self.check_expr(v));
                }
                let key_t = self.merge_types(key_types);
                let val_t = self.merge_types(val_types);
                Type::Dict(Box::new(key_t), Box::new(val_t))
            }
            Expr::Variant { name, arg, span } => {
                let arg_t = arg.as_ref().map(|a| self.check_expr(a));
                match (name.as_str(), arg_t) {
                    ("ok", Some(t)) => {
                        Type::Result(Box::new(t), Box::new(self.unifier.fresh_var()))
                    }
                    ("ok", None) => {
                        self.errors
                            .push(error_at("`.ok` requires an argument", *span));
                        Type::Result(Box::new(Type::Unit), Box::new(self.unifier.fresh_var()))
                    }
                    ("err", Some(e)) => {
                        Type::Result(Box::new(self.unifier.fresh_var()), Box::new(e))
                    }
                    ("err", None) => {
                        self.errors
                            .push(error_at("`.err` requires an argument", *span));
                        Type::Result(Box::new(self.unifier.fresh_var()), Box::new(Type::Unit))
                    }
                    ("some", Some(t)) => Type::Option(Box::new(t)),
                    ("some", None) => {
                        self.errors
                            .push(error_at("`.some` requires an argument", *span));
                        Type::Option(Box::new(Type::Unit))
                    }
                    ("none", None) => Type::Option(Box::new(self.unifier.fresh_var())),
                    ("none", Some(_)) => {
                        self.errors
                            .push(error_at("`.none` takes no argument", *span));
                        Type::Option(Box::new(self.unifier.fresh_var()))
                    }
                    (other, _) => {
                        self.errors
                            .push(error_at(format!("unknown variant `.{other}`"), *span));
                        Type::Unit
                    }
                }
            }
        }
    }

    fn check_unary(&mut self, op: UnOp, expr: &Expr, span: Span) -> Type {
        let t = self.check_expr(expr);

        let t = self.unifier.resolve(&t);
        match op {
            UnOp::Not => match t {
                Type::Bool => Type::Bool,
                Type::Var(id) => {
                    self.unifier.bind(id, Type::Bool);
                    Type::Bool
                }
                other => {
                    self.errors
                        .push(error_at(format!("expected `bool`, found `{other}`"), span));
                    Type::Bool
                }
            },
            UnOp::Pos | UnOp::Neg => match t {
                Type::Int => Type::Int,
                Type::Float => Type::Float,
                Type::Var(id) => {
                    self.unifier.bind(id, Type::Int);
                    Type::Int
                }
                other => {
                    self.errors.push(error_at(
                        format!("cannot negate a value of type `{other}`"),
                        span,
                    ));
                    Type::Int
                }
            },
        }
    }

    fn check_binary(&mut self, op: BinOp, left: &Expr, right: &Expr, span: Span) -> Type {
        match op {
            BinOp::And | BinOp::Or => {
                let lt = self.check_expr(left);
                self.ensure_bool(lt, left.span());
                let rt = self.check_expr(right);
                self.ensure_bool(rt, right.span());
                Type::Bool
            }
            BinOp::Elvis => {
                let lt = self.check_expr(left);
                let lt_resolved = self.unifier.resolve(&lt);
                let rt = self.check_expr(right);
                let rt_resolved = self.unifier.resolve(&rt);
                match lt_resolved {
                    Type::Option(inner) => {
                        if let Err(e) = self.unifier.unify(&*inner, &rt_resolved) {
                            self.report_mismatch(e, span);
                        }
                        *inner
                    }
                    _ => {
                        // Non-Option left: pass through (allows chaining).
                        if let Err(e) = self.unifier.unify(&lt, &rt) {
                            self.report_mismatch(e, span);
                        }
                        lt
                    }
                }
            }
            BinOp::Eq | BinOp::Ne | BinOp::Lt | BinOp::Gt | BinOp::Le | BinOp::Ge => {
                let lt = self.check_expr(left);
                let rt = self.check_expr(right);
                if let Err(e) = self.unifier.unify(&rt, &lt) {
                    self.report_mismatch(e, span);
                }
                Type::Bool
            }
            BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div | BinOp::Rem | BinOp::Pow => {
                self.check_arith(op, left, right, span)
            }
        }
    }

    fn check_arith(&mut self, op: BinOp, left: &Expr, right: &Expr, span: Span) -> Type {
        let lt = self.check_expr(left);

        let lt = self.unifier.resolve(&lt);
        let rt = self.check_expr(right);

        let rt = self.unifier.resolve(&rt);
        let result = match (&lt, &rt) {
            (Type::Int, Type::Int) => Type::Int,
            (Type::Str, Type::Str) if op == BinOp::Add => Type::Str,
            (Type::Int, Type::Float) | (Type::Float, Type::Int) | (Type::Float, Type::Float) => {
                Type::Float
            }
            (Type::Var(_), t) => {
                self.unifier.bind_var(&lt, t.clone());
                t.clone()
            }
            (t, Type::Var(_)) => {
                self.unifier.bind_var(&rt, t.clone());
                t.clone()
            }
            (a, b) => {
                if !matches!((&a, &b), (Type::Error, _) | (_, Type::Error)) {
                    self.errors.push(error_at(
                        format!("cannot apply `{}` to `{}` and `{}`", op.symbol(), a, b),
                        span,
                    ));
                }
                Type::Error
            }
        };
        result
    }

    fn check_call(
        &mut self,
        callee: &Expr,
        args: &[Expr],
        named: &[(String, Expr)],
        span: Span,
    ) -> Type {
        // Direct call of a named function: bypass `lookup` so generic
        // functions are instantiated here rather than rejected as values.
        let direct_name = match callee {
            Expr::Ident { name, .. } => Some(name.clone()),
            Expr::Path { parts, .. } => Some(parts.join(".")),
            Expr::Field { obj, name, .. } => {
                // Method call: `obj.method()` — resolve as method on obj's type
                let recv_t = self.check_expr(obj);
                let recv_t = self.unifier.resolve(&recv_t);
                let method = name.clone();
                let mut sig = self.funcs.get(&method).cloned();
                if sig.is_none() {
                    match &self.unifier.resolve(&recv_t) {
                        Type::Str => sig = self.funcs.get(&format!("str.{method}")).cloned(),
                        Type::Array(_) => sig = self.funcs.get(&format!("vec.{method}")).cloned(),
                        Type::Option(_) => {
                            sig = self.funcs.get(&format!("option.{method}")).cloned()
                        }
                        Type::Result(_, _) => {
                            sig = self.funcs.get(&format!("result.{method}")).cloned()
                        }
                        Type::Struct(sname) => {
                            if let Some((ns, _)) = sname.rsplit_once('.') {
                                sig = self.funcs.get(&format!("{ns}.{method}")).cloned();
                            }
                        }
                        _ => {}
                    }
                }
                if let Some(sig) = sig {
                    let (ps, ret) = self.instantiate(&sig);
                    if ps.is_empty() {
                        self.errors.push(error_at(
                            format!("method `{method}` takes no arguments"),
                            span,
                        ));
                        return Type::Unit;
                    }
                    if let Err(e) = self.unifier.unify(&recv_t, &ps[0]) {
                        self.report_mismatch(e, span);
                    }
                    self.check_args_against(
                        &sig.params[1..]
                            .iter()
                            .map(|(n, _)| n.clone())
                            .collect::<Vec<_>>(),
                        &ps[1..],
                        &[],
                        args,
                        named,
                        span,
                    );
                    return ret;
                }
                // Fall through to general call handling if no method found
                None
            }
            _ => None,
        };
        if let Some(name) = &direct_name {
            if let Some(sig) = self.funcs.get(name).cloned() {
                self.used_names.insert(name.clone());
                let (ps, ret) = self.instantiate(&sig);
                // Special case: `input` accepts 0 or 1 string argument (optional prompt)
                if name == "input" {
                    if args.len() + named.len() > 1 {
                        self.errors.push(error_at(
                            format!(
                                "expected 0 or 1 arguments, found {}",
                                args.len() + named.len()
                            ),
                            span,
                        ));
                    } else if args.len() + named.len() == 1 {
                        let arg_expr = if !args.is_empty() {
                            &args[0]
                        } else {
                            &named[0].1
                        };
                        let at = self.check_expr(arg_expr);
                        if let Err(e) = self.unifier.unify(&at, &Type::Str) {
                            self.report_mismatch(e, arg_expr.span());
                        }
                    }
                    return ret;
                }
                // Special case: `range` accepts 1, 2, or 3 int arguments
                if name == "range" {
                    let total = args.len() + named.len();
                    if total == 0 || total > 3 {
                        self.errors.push(error_at(
                            format!("range expects 1, 2, or 3 arguments, found {total}"),
                            span,
                        ));
                    } else {
                        for arg in args {
                            let at = self.check_expr(arg);
                            if let Err(e) = self.unifier.unify(&at, &Type::Int) {
                                self.report_mismatch(e, arg.span());
                            }
                        }
                        for (_, val) in named {
                            let at = self.check_expr(val);
                            if let Err(e) = self.unifier.unify(&at, &Type::Int) {
                                self.report_mismatch(e, val.span());
                            }
                        }
                    }
                    return ret;
                }
                let pnames: Vec<String> = sig.params.iter().map(|(n, _)| n.clone()).collect();
                self.check_args_against(&pnames, &ps, &sig.has_default, args, named, span);
                return ret;
            }
        }
        // Method call: `p.dist()` resolves to `dist(p, ...)` when the full
        // path isn't a known function.
        if let Expr::Path { parts, span: pspan } = callee {
            if parts.len() >= 2 {
                let method = parts.last().unwrap();
                let recv_t = self.lookup_path(&parts[..parts.len() - 1], *pspan);
                let mut sig = self.funcs.get(method).cloned();
                if sig.is_none() {
                    let recv_t_resolved = self.unifier.resolve(&recv_t);
                    match &recv_t_resolved {
                        Type::Str => {
                            sig = self.funcs.get(&format!("str.{method}")).cloned();
                        }
                        Type::Array(_) => {
                            sig = self.funcs.get(&format!("vec.{method}")).cloned();
                        }
                        Type::Option(_) => {
                            sig = self.funcs.get(&format!("option.{method}")).cloned();
                        }
                        Type::Result(_, _) => {
                            sig = self.funcs.get(&format!("result.{method}")).cloned();
                        }
                        Type::Struct(sname) => {
                            if let Some((ns, _)) = sname.rsplit_once('.') {
                                sig = self.funcs.get(&format!("{ns}.{method}")).cloned();
                            }
                        }
                        _ => {}
                    }
                }
                if let Some(sig) = sig {
                    let (ps, ret) = self.instantiate(&sig);
                    if ps.is_empty() {
                        self.errors.push(error_at(
                            format!("method `{method}` takes no arguments"),
                            span,
                        ));
                        return Type::Unit;
                    }
                    if let Err(e) = self.unifier.unify(&recv_t, &ps[0]) {
                        self.report_mismatch(e, *pspan);
                    }
                    self.check_args_against(
                        &sig.params[1..]
                            .iter()
                            .map(|(n, _)| n.clone())
                            .collect::<Vec<_>>(),
                        &ps[1..],
                        &[],
                        args,
                        named,
                        span,
                    );
                    return ret;
                }
            }
        }
        let callee_t = self.check_expr(callee);

        let callee_t = self.unifier.resolve(&callee_t);
        match callee_t {
            Type::Func(ps, ret) => {
                // No param names for anonymous function types; positional-only.
                let pnames: Vec<String> = (0..ps.len()).map(|i| format!("_{i}")).collect();
                self.check_args_against(&pnames, &ps, &[], args, named, span);
                *ret
            }
            Type::Named(name) => match self.funcs.get(&name).cloned() {
                Some(sig) => {
                    let (ps, ret) = self.instantiate(&sig);
                    let param_names: Vec<String> =
                        sig.params.iter().map(|(n, _)| n.clone()).collect();
                    self.check_args_against(&param_names, &ps, &sig.has_default, args, named, span);
                    ret
                }
                None => {
                    self.errors
                        .push(error_at(format!("unknown function `{name}`"), span));
                    Type::Unit
                }
            },
            Type::Var(_) => {
                // Suppress cascading error if the callee was already an
                // undefined variable — the primary error is sufficient.
                if !self.had_undefined_var {
                    self.errors.push(error_at(
                        "cannot call a value whose type could not be inferred",
                        span,
                    ));
                }
                Type::Error
            }
            Type::Error => Type::Error,
            other => {
                // Suppress cascading "cannot call unit" when the callee
                // was already flagged as an undefined variable.
                if !self.had_undefined_var {
                    self.errors.push(error_at(
                        format!("cannot call a value of type `{other}`"),
                        span,
                    ));
                }
                Type::Error
            }
        }
    }

    /// Check that the given positional and named arguments match the parameter
    /// types.  `has_default` indicates which trailing parameters have defaults;
    /// callers may omit those.
    fn check_args_against(
        &mut self,
        param_names: &[String],
        ps: &[Type],
        has_default: &[bool],
        args: &[Expr],
        named: &[(String, Expr)],
        span: Span,
    ) {
        let total_provided = args.len() + named.len();
        let total_params = ps.len();
        let allowed_min = total_params - has_default.iter().filter(|&&d| d).count();

        if total_provided < allowed_min || total_provided > total_params {
            self.errors.push(error_at(
                format!(
                    "expected {} to {} arguments, found {}",
                    allowed_min, total_params, total_provided
                ),
                span,
            ));
            return;
        }

        // Build a slot for each param: None = use default.
        let mut slots: Vec<Option<&Expr>> = vec![None; total_params];

        // Place positional args.
        for (i, arg) in args.iter().enumerate() {
            if i >= total_params {
                self.errors.push(error_at(
                    format!("too many positional arguments (max {})", total_params),
                    arg.span(),
                ));
                return;
            }
            if slots[i].is_some() {
                self.errors.push(error_at(
                    format!("positional argument `{}` conflicts with named argument", i),
                    arg.span(),
                ));
                return;
            }
            slots[i] = Some(arg);
        }

        // Place named args.
        for (name, val) in named {
            let pos = param_names.iter().position(|pn| pn == name);
            match pos {
                Some(i) => {
                    if slots[i].is_some() {
                        self.errors.push(error_at(
                            format!("argument `{name}` already provided positionally"),
                            val.span(),
                        ));
                        return;
                    }
                    slots[i] = Some(val);
                }
                None => {
                    self.errors
                        .push(error_at(format!("unknown parameter `{name}`"), val.span()));
                    return;
                }
            }
        }

        // Type-check each provided argument.
        for (i, slot) in slots.iter().enumerate() {
            if let Some(arg) = slot {
                let at = self.check_expr(arg);
                if let Err(e) = self.unifier.unify(&at, &ps[i]) {
                    self.report_mismatch(e, arg.span());
                }
            }
        }
    }

    fn check_closure(&mut self, params: &[Param], body: &Expr, _span: Span) -> Type {
        self.push_scope();
        let mut ptypes = Vec::new();
        for p in params {
            let ty = match &p.ty {
                Some(t) => {
                    let gens = self.current_generics.clone();
                    self.ast_to_type(t, &gens)
                }
                None => self.unifier.fresh_var(),
            };
            self.define(&p.name.name, ty.clone());
            ptypes.push(ty);
        }
        let bt = self.check_expr(body);
        self.pop_scope();
        Type::Func(ptypes, Box::new(bt))
    }

    fn check_match(&mut self, scrutinee: &Expr, arms: &[MatchArm], span: Span) -> Type {
        let st = self.check_expr(scrutinee);

        let st = self.unifier.resolve(&st);
        self.check_exhaustive(&st, arms, span);
        let mut result: Option<Type> = None;
        for arm in arms {
            self.push_scope();
            self.bind_pattern(&arm.pat, &st);
            let bt = self.check_expr(&arm.body);
            self.pop_scope();
            match &result {
                Some(r) => {
                    if let Err(e) = self.unifier.unify(&bt, r) {
                        self.report_mismatch(e, arm.body.span());
                    }
                }
                None => result = Some(bt),
            }
        }
        result.unwrap_or(Type::Unit)
    }

    fn check_try(&mut self, expr: &Expr, span: Span) -> Type {
        let ot = self.check_expr(expr);

        let ot = self.unifier.resolve(&ot);
        let ret = match &self.current_ret {
            Some(r) => self.unifier.resolve(r),
            None => {
                self.errors.push(error_at(
                    "`?` can only be used inside a function returning `Result` or `Option`",
                    span,
                ));
                return Type::Unit;
            }
        };
        match ot {
            Type::Option(t) => match &ret {
                Type::Option(_) => *t,
                Type::Var(id) => {
                    self.unifier.bind(*id, Type::Option(t.clone()));
                    *t
                }
                other => {
                    self.errors.push(error_at(
                        format!("`?` on `Option` cannot propagate through a function returning `{other}`"),
                        span,
                    ));
                    *t
                }
            },
            Type::Result(t, e) => match &ret {
                Type::Result(_, ret_e) => {
                    if let Err(err) = self.unifier.unify(&e, ret_e) {
                        self.report_mismatch(err, span);
                    }
                    *t
                }
                Type::Var(id) => {
                    self.unifier.bind(*id, Type::Result(t.clone(), e.clone()));
                    *t
                }
                other => {
                    self.errors.push(error_at(
                        format!("`?` on `Result` cannot propagate through a function returning `{other}`"),
                        span,
                    ));
                    *t
                }
            },
            Type::Var(_) => {
                self.errors.push(error_at(
                    "cannot use `?` on a value whose type could not be inferred",
                    span,
                ));
                Type::Unit
            }
            other => {
                self.errors.push(error_at(
                    format!("cannot use `?` on a value of type `{other}`"),
                    span,
                ));
                Type::Unit
            }
        }
    }

    // --- patterns ---------------------------------------------------------

    fn bind_pattern(&mut self, pat: &Pattern, ty: &Type) {
        match pat {
            Pattern::Wildcard { .. } => {}
            Pattern::Binding { name } => {
                self.define(&name.name, ty.clone());
            }
            Pattern::Literal { value, span } => {
                let lit_t = match value {
                    Lit::Int(_) => Type::Int,
                    Lit::Float(_) => Type::Float,
                    Lit::Str(_) => Type::Str,
                    Lit::Bool(_) => Type::Bool,
                };
                if let Err(e) = self.unifier.unify(&lit_t, ty) {
                    self.report_mismatch(e, *span);
                }
            }
            Pattern::Variant { name, arg, span } => {
                let rt = self.unifier.resolve(ty);
                let inner = match (&rt, name.as_str()) {
                    (Type::Option(inner), "some") => match arg {
                        Some(p) => Some((p.as_ref().clone(), (**inner).clone())),
                        None => {
                            self.errors
                                .push(error_at("`.some` pattern requires an argument", *span));
                            None
                        }
                    },
                    (Type::Option(_), "none") => {
                        if arg.is_some() {
                            self.errors
                                .push(error_at("`.none` pattern takes no argument", *span));
                        }
                        None
                    }
                    (Type::Result(t, _), "ok") => {
                        arg.as_ref().map(|p| (p.as_ref().clone(), (**t).clone()))
                    }
                    (Type::Result(_, e), "err") => {
                        arg.as_ref().map(|p| (p.as_ref().clone(), (**e).clone()))
                    }
                    (Type::Var(_), _) => {
                        // Unknown scrutinee type: bind optimistically.
                        arg.as_ref()
                            .map(|p| (p.as_ref().clone(), self.unifier.fresh_var()))
                    }
                    (other, vname) => {
                        self.errors.push(error_at(
                            format!("pattern `.{vname}` does not match a value of type `{other}`"),
                            *span,
                        ));
                        None
                    }
                };
                if let Some((p, inner)) = inner {
                    self.bind_pattern(&p, &inner);
                }
            }
        }
    }

    fn check_exhaustive(&mut self, st: &Type, arms: &[MatchArm], span: Span) {
        if arms
            .iter()
            .any(|a| matches!(a.pat, Pattern::Wildcard { .. }))
        {
            return;
        }
        let needs: Option<Vec<&str>> = match st {
            Type::Option(_) => Some(vec!["some", "none"]),
            Type::Result(_, _) => Some(vec!["ok", "err"]),
            Type::Bool => Some(vec!["true", "false"]),
            Type::Int | Type::Float | Type::Str | Type::Unit => {
                self.errors.push(error_at(
                    format!("match on `{st}` requires a `_` wildcard arm"),
                    span,
                ));
                return;
            }
            _ => return, // Var, Named, Tuple, Func: skip
        };
        let Some(needs) = needs else { return };
        let have: Vec<String> = arms
            .iter()
            .filter_map(|a| match &a.pat {
                Pattern::Variant { name, .. } => Some(name.clone()),
                Pattern::Literal {
                    value: Lit::Bool(b),
                    ..
                } => Some(if *b { "true" } else { "false" }.to_string()),
                _ => None,
            })
            .collect();
        let missing: Vec<&str> = needs
            .iter()
            .filter(|n| !have.iter().any(|h| h == *n))
            .copied()
            .collect();
        if !missing.is_empty() {
            let missing = missing
                .iter()
                .map(|m| format!("`.{m}`"))
                .collect::<Vec<_>>()
                .join(" or ");
            self.errors.push(error_at(
                format!("non-exhaustive match: missing {missing} (or add a `_` arm)"),
                span,
            ));
        }
    }

    // --- generics ---------------------------------------------------------

    fn instantiate(&mut self, sig: &FuncSig) -> (Vec<Type>, Type) {
        let subs: HashMap<String, Type> = sig
            .generics
            .iter()
            .map(|g| (g.clone(), self.unifier.fresh_var()))
            .collect();
        let params = sig.params.iter().map(|(_, t)| subst(t, &subs)).collect();
        let ret = subst(&sig.ret, &subs);
        (params, ret)
    }

    // --- type annotations -------------------------------------------------

    fn ast_to_type(&mut self, ty: &Ty, generics: &[String]) -> Type {
        self.ast_to_type_inner(ty, generics)
    }

    fn ast_to_type_inner(&mut self, ty: &Ty, generics: &[String]) -> Type {
        match &ty.kind {
            TyKind::Int => Type::Int,
            TyKind::Float => Type::Float,
            TyKind::Bool => Type::Bool,
            TyKind::Str => Type::Str,
            TyKind::Unit => Type::Unit,
            TyKind::Tuple(ts) => Type::Tuple(
                ts.iter()
                    .map(|t| self.ast_to_type_inner(t, generics))
                    .collect(),
            ),
            TyKind::Option(t) => Type::Option(Box::new(self.ast_to_type_inner(t, generics))),
            TyKind::Result(t, e) => Type::Result(
                Box::new(self.ast_to_type_inner(t, generics)),
                Box::new(self.ast_to_type_inner(e, generics)),
            ),
            TyKind::Func(ps, r) => Type::Func(
                ps.iter()
                    .map(|t| self.ast_to_type_inner(t, generics))
                    .collect(),
                Box::new(self.ast_to_type_inner(r, generics)),
            ),
            TyKind::Array(t) => Type::Array(Box::new(self.ast_to_type_inner(t, generics))),
            TyKind::Dict(k, v) => Type::Dict(
                Box::new(self.ast_to_type_inner(k, generics)),
                Box::new(self.ast_to_type_inner(v, generics)),
            ),
            TyKind::Union(ts) => Type::Union(
                ts.iter()
                    .map(|t| self.ast_to_type_inner(t, generics))
                    .collect(),
            ),
            TyKind::Named(name, args) => {
                if generics.iter().any(|g| g == name) {
                    if !args.is_empty() {
                        self.errors.push(error_at(
                            format!("generic parameter `{name}` does not take type arguments"),
                            ty.span,
                        ));
                    }
                    Type::Named(name.clone())
                } else if self.structs.contains_key(name) {
                    if !args.is_empty() {
                        self.errors.push(error_at(
                            format!("struct `{name}` does not take type arguments"),
                            ty.span,
                        ));
                    }
                    Type::Struct(name.clone())
                } else {
                    self.errors
                        .push(error_at(format!("unknown type `{name}`"), ty.span));
                    Type::Unit
                }
            }
        }
    }

    // --- errors -----------------------------------------------------------

    fn report_mismatch(&mut self, err: UnifyError, span: Span) {
        let msg = match err.message.as_str() {
            "type mismatch" => {
                format!(
                    "type mismatch: expected `{}`, found `{}`",
                    err.right, err.left
                )
            }
            "function arity mismatch" => "function arity mismatch".to_string(),
            "tuple arity mismatch" => "tuple arity mismatch".to_string(),
            other => other.to_string(),
        };
        self.errors.push(error_at(msg, span));
    }

    fn ensure_bool(&mut self, t: Type, span: Span) {
        match self.unifier.resolve(&t) {
            Type::Bool => {}
            Type::Var(id) => {
                self.unifier.bind(id, Type::Bool);
            }
            other => {
                self.errors
                    .push(error_at(format!("expected `bool`, found `{other}`"), span));
            }
        }
    }

    fn ensure_int(&mut self, t: Type, span: Span) {
        match self.unifier.resolve(&t) {
            Type::Int => {}
            Type::Var(id) => {
                self.unifier.bind(id, Type::Int);
            }
            other => {
                self.errors.push(error_at(
                    format!("index must be `int`, found `{other}`"),
                    span,
                ));
            }
        }
    }
}

fn func_name(stmt: &Stmt) -> String {
    match stmt {
        Stmt::Func { name, .. } => name.join("."),
        _ => unreachable!(),
    }
}

fn subst(t: &Type, subs: &HashMap<String, Type>) -> Type {
    match t {
        Type::Named(name) => subs
            .get(name)
            .cloned()
            .unwrap_or_else(|| Type::Named(name.clone())),
        Type::Tuple(ts) => Type::Tuple(ts.iter().map(|x| subst(x, subs)).collect()),
        Type::Option(x) => Type::Option(Box::new(subst(x, subs))),
        Type::Result(a, b) => Type::Result(Box::new(subst(a, subs)), Box::new(subst(b, subs))),
        Type::Func(ps, r) => Type::Func(
            ps.iter().map(|x| subst(x, subs)).collect(),
            Box::new(subst(r, subs)),
        ),
        Type::Array(x) => Type::Array(Box::new(subst(x, subs))),
        Type::Dict(k, v) => Type::Dict(Box::new(subst(k, subs)), Box::new(subst(v, subs))),
        Type::Union(ts) => Type::Union(ts.iter().map(|x| subst(x, subs)).collect()),
        Type::Range(x) => Type::Range(Box::new(subst(x, subs))),
        other => other.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use zz_frontend::parse;

    fn check_src(src: &str) -> CheckResult {
        let parsed = parse(src);
        assert!(
            parsed.errors.is_empty(),
            "parse errors: {:?}",
            parsed.errors
        );
        check_program(
            &parsed.program,
            HashMap::new(),
            HashMap::new(),
            HashMap::new(),
        )
    }

    fn errors_of(src: &str) -> Vec<String> {
        check_src(src)
            .errors
            .iter()
            .filter(|e| e.severity == zz_frontend::diag::Severity::Error)
            .map(|e| e.message.clone())
            .collect()
    }

    fn errors_contain(src: &str, needle: &str) {
        let errs = errors_of(src);
        assert!(
            errs.iter().any(|e| e.contains(needle)),
            "expected error containing `{needle}`, got: {errs:?}"
        );
    }

    /// True if `CheckResult` has any errors (severity=Error), ignoring warnings.
    fn has_errors(r: &CheckResult) -> bool {
        r.errors
            .iter()
            .any(|e| e.severity == zz_frontend::diag::Severity::Error)
    }

    /// Check with a seeded function map (e.g. a generic builtin like `typeof`).
    fn check_src_with_funcs(src: &str, funcs: HashMap<String, FuncSig>) -> CheckResult {
        let parsed = parse(src);
        assert!(
            parsed.errors.is_empty(),
            "parse errors: {:?}",
            parsed.errors
        );
        check_program(&parsed.program, HashMap::new(), funcs, HashMap::new())
    }

    /// Check with seeded functions and structs (e.g. a namespaced struct
    /// that only exists through module registration).
    fn check_src_with_funcs_and_structs(
        src: &str,
        funcs: HashMap<String, FuncSig>,
        structs: HashMap<String, StructSig>,
    ) -> CheckResult {
        let parsed = parse(src);
        assert!(
            parsed.errors.is_empty(),
            "parse errors: {:?}",
            parsed.errors
        );
        check_program(&parsed.program, HashMap::new(), funcs, structs)
    }

    #[test]
    fn infers_int_from_literal() {
        let r = check_src("x := 1");
        assert!(!has_errors(&r), "errors: {:?}", r.errors);
        assert_eq!(r.bindings["x"], Type::Int);
    }

    #[test]
    fn infers_float_from_promotion() {
        let r = check_src("x := 1 + 2.5");
        assert!(!has_errors(&r), "errors: {:?}", r.errors);
        assert_eq!(r.bindings["x"], Type::Float);
    }

    #[test]
    fn annotation_unifies() {
        let r = check_src("x: float = 1 + 2.5");
        assert!(!has_errors(&r), "errors: {:?}", r.errors);
        assert_eq!(r.bindings["x"], Type::Float);
    }

    #[test]
    fn annotation_mismatch_errors() {
        errors_contain("x: str = 1 + 2", "type mismatch");
    }

    #[test]
    fn type_mismatch_arithmetic() {
        errors_contain("1 + \"a\"", "cannot apply `+`");
    }

    #[test]
    fn bool_ops_require_bool() {
        errors_contain("1 && true", "expected `bool`, found `int`");
    }

    #[test]
    fn comparison_requires_same_type() {
        errors_contain("1 == \"a\"", "type mismatch");
    }

    #[test]
    fn undefined_variable_errors() {
        errors_contain("nope + 1", "undefined variable `nope`");
    }

    #[test]
    fn func_signature_and_body() {
        let r = check_src("func add(a: int, b: int) -> int { return a + b }\nz := add(1, 2)");
        assert!(!has_errors(&r), "errors: {:?}", r.errors);
        assert_eq!(r.bindings["z"], Type::Int);
    }

    #[test]
    fn func_return_type_inferred() {
        let r = check_src("func five() { return 5 }\nz := five()");
        assert!(!has_errors(&r), "errors: {:?}", r.errors);
        assert_eq!(r.bindings["z"], Type::Int);
    }

    #[test]
    fn func_wrong_return_type_errors() {
        errors_contain("func f() -> int { return \"a\" }", "type mismatch");
    }

    #[test]
    fn wrong_arg_count_errors() {
        errors_contain(
            "func f(a: int) -> int { a }\nf(1, 2)",
            "expected 1 to 1 arguments, found 2",
        );
    }

    #[test]
    fn wrong_arg_type_errors() {
        errors_contain("func f(a: int) -> int { a }\nf(\"x\")", "type mismatch");
    }

    #[test]
    fn generic_func_instantiates() {
        let r = check_src("func id<T>(x: T) -> T { return x }\na := id(1)\nb := id(\"s\")");
        assert!(!has_errors(&r), "errors: {:?}", r.errors);
        assert_eq!(r.bindings["a"], Type::Int);
        assert_eq!(r.bindings["b"], Type::Str);
    }

    #[test]
    fn generic_func_monomorphic_fail() {
        errors_contain(
            "func id<T>(x: T) -> T { x }\nf := id",
            "cannot use generic function `id` as a value",
        );
    }

    #[test]
    fn recursion_works() {
        let r = check_src("func fact(n: int) -> int { if n <= 1 { 1 } else { n * fact(n - 1) } }");
        assert!(!has_errors(&r), "errors: {:?}", r.errors);
    }

    // --- structs -----------------------------------------------------------

    #[test]
    fn struct_def_and_init() {
        let r = check_src("struct Point { x: int, y: int }\np := Point{ x: 1, y: 2 }");
        assert!(!has_errors(&r), "errors: {:?}", r.errors);
        assert_eq!(r.bindings["p"], Type::Struct("Point".into()));
        assert_eq!(r.structs["Point"].fields.len(), 2);
    }

    #[test]
    fn struct_field_access() {
        let r = check_src("struct Point { x: int, y: int }\np := Point{ x: 1, y: 2 }\nz := p.x");
        assert!(!has_errors(&r), "errors: {:?}", r.errors);
        assert_eq!(r.bindings["z"], Type::Int);
    }

    #[test]
    fn struct_field_mutation() {
        let r = check_src("struct Point { x: int, y: int }\np := Point{ x: 1, y: 2 }\np.x = 10");
        assert!(!has_errors(&r), "errors: {:?}", r.errors);
    }

    #[test]
    fn struct_field_mutation_type_mismatch_errors() {
        errors_contain(
            "struct Point { x: int, y: int }\np := Point{ x: 1, y: 2 }\np.x = \"a\"",
            "type mismatch",
        );
    }

    #[test]
    fn struct_unknown_field_errors() {
        errors_contain(
            "struct Point { x: int, y: int }\np := Point{ x: 1, y: 2 }\np.z",
            "has no field `z`",
        );
    }

    #[test]
    fn struct_unknown_field_in_init_errors() {
        errors_contain(
            "struct Point { x: int, y: int }\np := Point{ x: 1, z: 2 }",
            "has no field `z`",
        );
    }

    #[test]
    fn struct_unknown_type_errors() {
        errors_contain("p := Nope{ x: 1 }", "unknown struct `Nope`");
    }

    #[test]
    fn struct_in_func_signature() {
        let r = check_src(
            "struct Point { x: int, y: int }\nfunc dist(p: Point) -> int { p.x + p.y }\nz := dist(Point{ x: 1, y: 2 })",
        );
        assert!(!has_errors(&r), "errors: {:?}", r.errors);
        assert_eq!(r.bindings["z"], Type::Int);
    }

    #[test]
    fn struct_wrong_arg_type_errors() {
        errors_contain(
            "struct Point { x: int, y: int }\nfunc dist(p: Point) -> int { p.x }\ndist(5)",
            "type mismatch",
        );
    }

    #[test]
    fn struct_field_on_non_struct_errors() {
        errors_contain("x := 5\nx.y", "cannot access field `y`");
    }

    #[test]
    fn struct_duplicate_definition_errors() {
        errors_contain(
            "struct A { x: int }\nstruct A { y: int }",
            "duplicate definition of struct `A`",
        );
    }

    #[test]
    fn struct_type_annotation_resolves() {
        let r = check_src("struct Point { x: int, y: int }\np: Point = Point{ x: 1, y: 2 }");
        assert!(!has_errors(&r), "errors: {:?}", r.errors);
        assert_eq!(r.bindings["p"], Type::Struct("Point".into()));
    }

    // --- for loops ---------------------------------------------------------

    #[test]
    fn for_over_range() {
        let r = check_src("sum := 0\nfor i in 0..5 { sum = sum + i }");
        assert!(!has_errors(&r), "errors: {:?}", r.errors);
    }

    #[test]
    fn for_over_array() {
        let r = check_src("total := 0\nfor n in [10, 20, 30] { total = total + n }");
        assert!(!has_errors(&r), "errors: {:?}", r.errors);
    }

    #[test]
    fn for_loop_var_typed() {
        let r = check_src("for i in 0..5 { i }");
        assert!(!has_errors(&r), "errors: {:?}", r.errors);
    }

    #[test]
    fn for_over_non_iterable_errors() {
        errors_contain("for i in 5 { i }", "cannot iterate a value of type `int`");
    }

    #[test]
    fn for_loop_var_scope_does_not_leak() {
        errors_contain("for i in 0..5 { i }\ni", "undefined variable `i`");
    }

    #[test]
    fn break_outside_loop_errors() {
        errors_contain("break", "`break` outside of a loop");
    }

    #[test]
    fn continue_outside_loop_errors() {
        errors_contain("continue", "`continue` outside of a loop");
    }

    #[test]
    fn break_inside_loop_ok() {
        let r = check_src("for i in 0..5 { if i == 2 { break } }");
        assert!(!has_errors(&r), "errors: {:?}", r.errors);
    }

    #[test]
    fn break_inside_while_ok() {
        let r = check_src("x := 0\nwhile x < 5 { x = x + 1; break }");
        assert!(!has_errors(&r), "errors: {:?}", r.errors);
    }

    #[test]
    fn range_bounds_must_be_int() {
        errors_contain("for i in 0.5..2.5 { i }", "range bounds must be `int`");
    }

    #[test]
    fn assignment_to_undefined_errors() {
        errors_contain("nope = 5", "undefined variable `nope`");
    }

    #[test]
    fn closure_inference() {
        let r = check_src("f := |x: int| x + 1");
        assert!(!has_errors(&r), "errors: {:?}", r.errors);
        assert_eq!(
            r.bindings["f"],
            Type::Func(vec![Type::Int], Box::new(Type::Int))
        );
    }

    #[test]
    fn closure_ambiguous_errors() {
        errors_contain("f := |x| x", "cannot infer the type of `f`");
    }

    #[test]
    fn calling_closure() {
        let r = check_src("f := |x: int| x + 1\ny := f(5)");
        assert!(!has_errors(&r), "errors: {:?}", r.errors);
        assert_eq!(r.bindings["y"], Type::Int);
    }

    #[test]
    fn match_option() {
        let r = check_src("v := .some(1)\nx := match v { .some(n) => n, .none => 0 }");
        assert!(!has_errors(&r), "errors: {:?}", r.errors);
        assert_eq!(r.bindings["x"], Type::Int);
    }

    #[test]
    fn match_result() {
        let r =
            check_src("v: Result<int, str> = .ok(1)\nx := match v { .ok(n) => n, .err(_) => 0 }");
        assert!(!has_errors(&r), "errors: {:?}", r.errors);
        assert_eq!(r.bindings["x"], Type::Int);
    }

    #[test]
    fn match_nonexhaustive_errors() {
        errors_contain("v := .some(1)\nmatch v { .some(n) => n }", "non-exhaustive");
    }

    #[test]
    fn match_on_int_requires_wildcard() {
        errors_contain("match 5 { 1 => 1 }", "requires a `_` wildcard arm");
    }

    #[test]
    fn match_arm_type_mismatch_errors() {
        errors_contain(
            "v := .some(1)\nmatch v { .some(n) => n, .none => \"x\" }",
            "type mismatch",
        );
    }

    #[test]
    fn if_let_binds() {
        let r = check_src("v := .some(5)\nx := if let .some(n) = v { n } else { 0 }");
        assert!(!has_errors(&r), "errors: {:?}", r.errors);
        assert_eq!(r.bindings["x"], Type::Int);
    }

    #[test]
    fn try_propagates_result() {
        let r = check_src(
            "func div(a: int, b: int) -> Result<int, str> { if b == 0 { .err(\"z\") } else { .ok(a / b) } }\nfunc f(a: int, b: int) -> Result<int, str> { q := div(a, b)?; .ok(q) }",
        );
        assert!(!has_errors(&r), "errors: {:?}", r.errors);
    }

    #[test]
    fn try_on_option() {
        let r = check_src("func f() -> Option<int> { x := .some(1)?; .some(x) }");
        assert!(!has_errors(&r), "errors: {:?}", r.errors);
    }

    #[test]
    fn try_outside_function_errors() {
        errors_contain(".ok(1)?", "only be used inside a function");
    }

    #[test]
    fn try_on_plain_int_errors() {
        errors_contain(
            "func f() -> Result<int, str> { x := 5?; .ok(x) }",
            "cannot use `?` on a value of type `int`",
        );
    }

    #[test]
    fn try_error_type_mismatch() {
        errors_contain(
            "func a() -> Result<int, str> { .ok(1) }\nfunc b() -> Result<int, int> { x := a()?; .ok(x) }",
            "type mismatch",
        );
    }

    #[test]
    fn variant_type_inference() {
        // `.none`/`.ok`/`.err` default their unknown variant parameter to
        // `unit`; `.some`/`.ok` with a concrete argument infer fully.
        let r = check_src("a := .ok(1)\nb := .none\nc := .err(\"boom\")");
        assert!(!has_errors(&r), "expected no errors, got {:?}", r.errors);
        // A binding whose type still has a var after defaulting still errors.
        errors_contain("f := |x| x", "cannot infer the type of `f`");
    }

    #[test]
    fn return_outside_function_errors() {
        errors_contain("return 5", "`return` outside of a function");
    }

    #[test]
    fn if_else_type_unify() {
        let r = check_src("x := if true { 1 } else { 2 }");
        assert!(!has_errors(&r), "errors: {:?}", r.errors);
        assert_eq!(r.bindings["x"], Type::Int);
    }

    #[test]
    fn if_else_mismatch_errors() {
        errors_contain("x := if true { 1 } else { \"a\" }", "type mismatch");
    }

    #[test]
    fn if_condition_must_be_bool() {
        errors_contain("if 1 { 1 } else { 2 }", "expected `bool`");
    }

    #[test]
    fn while_condition_must_be_bool() {
        errors_contain("while 1 { f() }", "expected `bool`");
    }

    #[test]
    fn str_concat() {
        let r = check_src("s := \"a\" + \"b\"");
        assert!(!has_errors(&r), "errors: {:?}", r.errors);
        assert_eq!(r.bindings["s"], Type::Str);
    }

    #[test]
    fn str_plus_int_errors() {
        errors_contain("s := \"a\" + 1", "cannot apply `+`");
    }

    #[test]
    fn shadowing_allowed() {
        let r = check_src("x := 1\nx := x + 1");
        assert!(!has_errors(&r), "errors: {:?}", r.errors);
        assert_eq!(r.bindings["x"], Type::Int);
    }

    #[test]
    fn duplicate_func_errors() {
        errors_contain("func f() { 1 }\nfunc f() { 2 }", "duplicate definition");
    }

    #[test]
    fn array_literal_infers() {
        let r = check_src("scores := [10, 20, 30]");
        assert!(!has_errors(&r), "errors: {:?}", r.errors);
        assert_eq!(r.bindings["scores"], Type::Array(Box::new(Type::Int)));
    }

    #[test]
    fn array_explicit_decl() {
        let r = check_src("scores: [int] = [10, 20, 30]");
        assert!(!has_errors(&r), "errors: {:?}", r.errors);
        assert_eq!(r.bindings["scores"], Type::Array(Box::new(Type::Int)));
    }

    #[test]
    fn array_mixed_types_form_union() {
        let r = check_src("v := [1, \"a\"]");
        assert!(!has_errors(&r), "errors: {:?}", r.errors);
        assert_eq!(
            r.bindings["v"],
            Type::Array(Box::new(Type::Union(vec![Type::Int, Type::Str])))
        );
    }

    #[test]
    fn array_annotation_unifies_with_union() {
        let r = check_src("v: [int] = [1, 2]");
        assert!(!has_errors(&r), "errors: {:?}", r.errors);
        assert_eq!(r.bindings["v"], Type::Array(Box::new(Type::Int)));
    }

    #[test]
    fn array_type_mismatch_errors() {
        errors_contain("v: [int] = [\"a\"]", "type mismatch");
    }

    #[test]
    fn array_union_member_accepted() {
        // Union semantics: a value matches a declared type if any member
        // matches. `[1, "a"]` has element type `int | str`, which contains
        // `int`, so the annotation is accepted.
        let r = check_src("v: [int] = [1, \"a\"]");
        assert!(!has_errors(&r), "errors: {:?}", r.errors);
    }

    #[test]
    fn empty_array_ambiguous() {
        errors_contain("v := []", "cannot infer the type of `v`");
    }

    #[test]
    fn dict_literal_infers() {
        let r = check_src("ages := {\"Zaid\": 20}");
        assert!(!has_errors(&r), "errors: {:?}", r.errors);
        assert_eq!(
            r.bindings["ages"],
            Type::Dict(Box::new(Type::Str), Box::new(Type::Int))
        );
    }

    #[test]
    fn dict_explicit_decl() {
        let r = check_src("ages: {str: int} = {\"a\": 1}");
        assert!(!has_errors(&r), "errors: {:?}", r.errors);
        assert_eq!(
            r.bindings["ages"],
            Type::Dict(Box::new(Type::Str), Box::new(Type::Int))
        );
    }

    #[test]
    fn dict_union_value_type() {
        let r = check_src("user: {str: str | int} = {\"name\": \"Zaid\", \"age\": 20}");
        assert!(!has_errors(&r), "errors: {:?}", r.errors);
        assert_eq!(
            r.bindings["user"],
            Type::Dict(
                Box::new(Type::Str),
                Box::new(Type::Union(vec![Type::Str, Type::Int]))
            )
        );
    }

    #[test]
    fn dict_key_mismatch_errors() {
        errors_contain("m: {str: int} = {1: 2}", "type mismatch");
    }

    #[test]
    fn empty_dict_ambiguous() {
        errors_contain("m := {}", "cannot infer the type of `m`");
    }

    #[test]
    fn import_is_noop() {
        let r = check_src("import std.io\nx := 1");
        assert!(!has_errors(&r), "errors: {:?}", r.errors);
        assert_eq!(r.bindings["x"], Type::Int);
    }

    #[test]
    fn union_annotation_accepts_member() {
        let r = check_src("v: str | int = 5");
        assert!(!has_errors(&r), "errors: {:?}", r.errors);
        // Binding stores the value type (int), which unifies with the union.
        assert_eq!(r.bindings["v"], Type::Int);
    }

    #[test]
    fn union_mismatch_errors() {
        errors_contain("v: str | int = true", "type mismatch");
    }

    // --- indexing & slicing -------------------------------------------------

    #[test]
    fn array_index_type() {
        let r = check_src("scores := [10, 20]\nx := scores[0]");
        assert!(!has_errors(&r), "errors: {:?}", r.errors);
        assert_eq!(r.bindings["x"], Type::Int);
    }

    #[test]
    fn dict_index_type() {
        let r = check_src("ages := {\"a\": 1}\nx := ages[\"a\"]");
        assert!(!has_errors(&r), "errors: {:?}", r.errors);
        assert_eq!(r.bindings["x"], Type::Int);
    }

    #[test]
    fn str_index_type() {
        let r = check_src("x := \"hello\"[1]");
        assert!(!has_errors(&r), "errors: {:?}", r.errors);
        assert_eq!(r.bindings["x"], Type::Str);
    }

    #[test]
    fn array_slice_type() {
        let r = check_src("scores := [10, 20, 30]\nx := scores[1:3]");
        assert!(!has_errors(&r), "errors: {:?}", r.errors);
        assert_eq!(r.bindings["x"], Type::Array(Box::new(Type::Int)));
    }

    #[test]
    fn str_slice_type() {
        let r = check_src("x := \"hello\"[1:3]");
        assert!(!has_errors(&r), "errors: {:?}", r.errors);
        assert_eq!(r.bindings["x"], Type::Str);
    }

    #[test]
    fn index_non_indexable_errors() {
        errors_contain("x := 5\nx[0]", "cannot index a value of type `int`");
    }

    #[test]
    fn index_non_int_errors() {
        errors_contain("scores := [1, 2]\nscores[\"a\"]", "index must be `int`");
    }

    #[test]
    fn slice_non_sliceable_errors() {
        errors_contain("x := 5\nx[1:2]", "cannot slice a value of type `int`");
    }

    #[test]
    fn index_assign_type_checked() {
        let r = check_src("scores := [1, 2]\nscores[0] = 5");
        assert!(!has_errors(&r), "errors: {:?}", r.errors);
    }

    #[test]
    fn index_assign_wrong_type_errors() {
        errors_contain("scores := [1, 2]\nscores[0] = \"x\"", "type mismatch");
    }

    #[test]
    fn str_index_assign_errors() {
        errors_contain(
            "s := \"abc\"\ns[0] = \"x\"",
            "cannot assign to an index of a string",
        );
    }

    #[test]
    fn dict_index_assign_ok() {
        let r = check_src("ages := {\"a\": 1}\nages[\"b\"] = 2");
        assert!(!has_errors(&r), "errors: {:?}", r.errors);
    }

    // --- pipeline -----------------------------------------------------------

    #[test]
    fn pipe_type_checks() {
        let r = check_src("func dbl(a: int, b: int) -> int { a * b }\nx := 5 |> dbl(3)");
        assert!(!has_errors(&r), "errors: {:?}", r.errors);
        assert_eq!(r.bindings["x"], Type::Int);
    }

    #[test]
    fn pipe_bare_name_type_checks() {
        let r = check_src("func inc(n: int) -> int { n + 1 }\nx := 5 |> inc");
        assert!(!has_errors(&r), "errors: {:?}", r.errors);
        assert_eq!(r.bindings["x"], Type::Int);
    }

    #[test]
    fn pipe_type_mismatch_errors() {
        errors_contain(
            "func dbl(a: int, b: int) -> int { a * b }\nx := \"s\" |> dbl(3)",
            "type mismatch",
        );
    }

    #[test]
    fn pipe_chain_type_checks() {
        let r = check_src(
            "func inc(n: int) -> int { n + 1 }\nfunc dbl(n: int) -> int { n * 2 }\nx := 5 |> inc |> dbl",
        );
        assert!(!has_errors(&r), "errors: {:?}", r.errors);
        assert_eq!(r.bindings["x"], Type::Int);
    }

    // --- typeof -------------------------------------------------------------

    #[test]
    fn typeof_any_value() {
        // `typeof` is a generic builtin: `typeof(v: T) -> str`.
        let mut funcs = HashMap::new();
        funcs.insert(
            "typeof".to_string(),
            FuncSig {
                generics: vec!["T".to_string()],
                params: vec![("v".to_string(), Type::Named("T".to_string()))],
                has_default: vec![],
                ret: Type::Str,
            },
        );
        for src in [
            "x := typeof(1)",
            "x := typeof(\"s\")",
            "x := typeof([1, 2])",
            "x := typeof({\"a\": 1})",
            "x := typeof(.some(1))",
        ] {
            let r = check_src_with_funcs(src, funcs.clone());
            assert!(!has_errors(&r), "errors for `{src}`: {:?}", r.errors);
            assert_eq!(r.bindings["x"], Type::Str, "for `{src}`");
        }
    }

    // --- method calls -------------------------------------------------------

    fn method_funcs() -> HashMap<String, FuncSig> {
        let mut funcs = HashMap::new();
        funcs.insert(
            "dist".to_string(),
            FuncSig {
                generics: Vec::new(),
                params: vec![
                    ("p".to_string(), Type::Struct("Point".to_string())),
                    ("scale".to_string(), Type::Int),
                ],
                has_default: vec![],
                ret: Type::Int,
            },
        );
        funcs
    }

    #[test]
    fn method_call_type_checks() {
        let r = check_src_with_funcs(
            "struct Point { x: int }\np := Point{ x: 3 }\nz := p.dist(2)",
            method_funcs(),
        );
        assert!(!has_errors(&r), "errors: {:?}", r.errors);
        assert_eq!(r.bindings["z"], Type::Int);
    }

    #[test]
    fn method_call_receiver_mismatch_errors() {
        let r = check_src_with_funcs(
            "struct Point { x: int }\nstruct Line { a: int }\nl := Line{ a: 1 }\nz := l.dist(2)",
            method_funcs(),
        );
        assert!(
            r.errors.iter().any(|e| e.message.contains("type mismatch")),
            "errors: {:?}",
            r.errors
        );
    }

    #[test]
    fn method_call_arg_mismatch_errors() {
        let r = check_src_with_funcs(
            "struct Point { x: int }\np := Point{ x: 3 }\nz := p.dist(\"s\")",
            method_funcs(),
        );
        assert!(
            r.errors.iter().any(|e| e.message.contains("type mismatch")),
            "errors: {:?}",
            r.errors
        );
    }

    #[test]
    fn method_call_unknown_method_errors() {
        let r = check_src_with_funcs(
            "struct Point { x: int }\np := Point{ x: 3 }\nz := p.nope()",
            method_funcs(),
        );
        assert!(
            r.errors
                .iter()
                .any(|e| e.message.contains("no field `nope`")),
            "errors: {:?}",
            r.errors
        );
    }

    #[test]
    fn method_call_namespaced_by_struct_type() {
        // `shapes.Point` receiver resolves `dist` → `shapes.dist`. The
        // namespaced struct exists only through module registration, so it
        // is seeded rather than defined in source.
        let mut funcs = HashMap::new();
        funcs.insert(
            "shapes.dist".to_string(),
            FuncSig {
                generics: Vec::new(),
                params: vec![("p".to_string(), Type::Struct("shapes.Point".to_string()))],
                has_default: vec![],
                ret: Type::Int,
            },
        );
        let mut structs = HashMap::new();
        structs.insert(
            "shapes.Point".to_string(),
            StructSig {
                fields: vec![("x".to_string(), Type::Int)],
            },
        );
        let r = check_src_with_funcs_and_structs(
            "p := shapes.Point{ x: 3 }\nz := p.dist()",
            funcs,
            structs,
        );
        assert!(!has_errors(&r), "errors: {:?}", r.errors);
        assert_eq!(r.bindings["z"], Type::Int);
    }

    // --- conversions --------------------------------------------------------

    fn conv_funcs() -> HashMap<String, FuncSig> {
        let t = Type::Named("T".to_string());
        let mut funcs = HashMap::new();
        funcs.insert(
            "str".to_string(),
            FuncSig {
                generics: vec!["T".to_string()],
                params: vec![("v".to_string(), t.clone())],
                has_default: vec![],
                ret: Type::Str,
            },
        );
        funcs.insert(
            "int".to_string(),
            FuncSig {
                generics: vec!["T".to_string()],
                params: vec![("v".to_string(), t.clone())],
                has_default: vec![],
                ret: Type::Option(Box::new(Type::Int)),
            },
        );
        funcs.insert(
            "float".to_string(),
            FuncSig {
                generics: vec!["T".to_string()],
                params: vec![("v".to_string(), t.clone())],
                has_default: vec![],
                ret: Type::Option(Box::new(Type::Float)),
            },
        );
        funcs
    }

    #[test]
    fn conversion_sigs() {
        let r = check_src_with_funcs("a := str(1)\nb := int(\"42\")\nc := float(3)", conv_funcs());
        assert!(!has_errors(&r), "errors: {:?}", r.errors);
        assert_eq!(r.bindings["a"], Type::Str);
        assert_eq!(r.bindings["b"], Type::Option(Box::new(Type::Int)));
        assert_eq!(r.bindings["c"], Type::Option(Box::new(Type::Float)));
    }

    #[test]
    fn conversion_any_value() {
        for src in ["a := str([1, 2])", "a := int(3.7)", "a := float(\"2.5\")"] {
            let r = check_src_with_funcs(src, conv_funcs());
            assert!(!has_errors(&r), "errors for `{src}`: {:?}", r.errors);
        }
    }

    // --- smart diagnostics tests -------------------------------------------

    #[test]
    fn unused_variable_warning() {
        let r = check_src("x := 1");
        let msgs: Vec<_> = r.errors.iter().map(|e| e.message.as_str()).collect();
        assert!(
            msgs.iter().any(|m| m.contains("unused variable")),
            "expected unused variable warning, got: {msgs:?}"
        );
    }

    #[test]
    fn underscore_prefixed_no_warning() {
        let r = check_src("_x := 1");
        assert!(
            r.errors
                .iter()
                .all(|e| !e.message.contains("unused variable")),
            "underscore-prefixed should not warn: {:?}",
            r.errors
        );
    }

    #[test]
    fn used_variable_no_warning() {
        // x is used in y's expression, so no warning for x.
        let r = check_src("x := 1\ny := x + 1");
        let warns: Vec<String> = r
            .errors
            .iter()
            .filter(|e| e.severity == zz_frontend::diag::Severity::Warning)
            .map(|e| e.message.clone())
            .collect();
        assert!(
            !warns.iter().any(|m| m.contains("unused variable `x`")),
            "x should not be unused: {warns:?}"
        );
    }

    #[test]
    fn typo_suggestion_variable() {
        // Register a function "println" so the typo engine has a candidate.
        let mut funcs = HashMap::new();
        funcs.insert(
            "println".to_string(),
            FuncSig {
                generics: vec![],
                params: vec![("msg".to_string(), Type::Str)],
                has_default: vec![false],
                ret: Type::Unit,
            },
        );
        let parsed = parse("prntlnn");
        assert!(
            parsed.errors.is_empty(),
            "parse errors: {:?}",
            parsed.errors
        );
        let r = check_program(&parsed.program, HashMap::new(), funcs, HashMap::new());
        let notes: Vec<String> = r.errors.iter().flat_map(|e| e.notes.clone()).collect();
        let msgs: Vec<_> = r.errors.iter().map(|e| e.message.as_str()).collect();
        assert!(
            msgs.iter().any(|m| m.contains("undefined")),
            "expected undefined variable error, got: {msgs:?}"
        );
        assert!(
            notes.iter().any(|n| n.contains("did you mean")),
            "expected typo suggestion, got: {notes:?}"
        );
    }

    #[test]
    fn typo_suggestion_struct_field() {
        let r = check_src_with_funcs_and_structs(
            "struct Point { x: int, y: int }\np := Point{ x: 1, y: 2 }\nq := p.xz",
            HashMap::new(),
            {
                let mut s = HashMap::new();
                s.insert(
                    "Point".to_string(),
                    StructSig {
                        fields: vec![("x".to_string(), Type::Int), ("y".to_string(), Type::Int)],
                    },
                );
                s
            },
        );
        let notes: Vec<String> = r.errors.iter().flat_map(|e| e.notes.clone()).collect();
        assert!(
            notes.iter().any(|n| n.contains("did you mean")),
            "expected field suggestion, got: {notes:?}"
        );
    }

    #[test]
    fn unclosed_paren_in_parser() {
        let parsed = parse("func add(a: int, b: int) -> int {\n    a +\n");
        assert!(
            parsed.errors.iter().any(|e| e.message.contains("unclosed")),
            "expected unclosed delimiter error, got: {:?}",
            parsed.errors
        );
    }

    #[test]
    fn mismatched_delimiter_in_parser() {
        let parsed = parse("(1 + 2]");
        let msgs: Vec<_> = parsed.errors.iter().map(|e| e.message.as_str()).collect();
        assert!(
            msgs.iter()
                .any(|m| m.contains("unexpected") || m.contains("unclosed")),
            "expected mismatched delimiter error, got: {msgs:?}"
        );
    }

    #[test]
    fn fixit_structure_is_populated() {
        use zz_frontend::diag::FixIt;
        let fixit = FixIt::safe(Span::new(0, 5), "_x", "rename to");
        assert_eq!(fixit.replacement, "_x");
        assert_eq!(fixit.message, "rename to");
    }
}
