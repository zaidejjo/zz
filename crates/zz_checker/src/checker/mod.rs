//! Type checker: HM-lite inference, generics, patterns, exhaustiveness.

pub mod diagnostics;
pub mod funcs;
pub mod inference;
pub mod scope;
pub mod structs;
pub mod type_check;

use std::collections::HashMap;

use zz_frontend::ast::{Program, Stmt};
use zz_frontend::diag::RawDiag;
use zz_frontend::span::Span;

use crate::type_::Type;

/// A registered function signature.
#[derive(Debug, Clone)]
pub struct FuncSig {
    pub generics: Vec<String>,
    /// Trait bounds per generic parameter name (e.g. `T` → `[Num, Ord]`).
    pub bounds: Vec<(String, Vec<zz_frontend::ast::TraitBound>)>,
    pub params: Vec<(String, Type)>,
    pub has_default: Vec<bool>,
    pub ret: Type,
    /// True for `extern "C"` declarations — no ZZ body, linked natively.
    pub is_extern: bool,
    /// Explicit C symbol override from the manifest (`= "..."`). `None`
    /// means "derive from the ZZ-visible name" via [`FuncSig::c_symbol`].
    pub extern_c_symbol: Option<String>,
}

impl FuncSig {
    /// Resolve the underlying C symbol for an extern signature keyed by
    /// its ZZ-visible name: the explicit override when present, otherwise
    /// the ZZ name with `.` replaced by `_` (`zimg.resize` → `zimg_resize`,
    /// flat `add` → `add`).
    pub fn c_symbol(&self, zz_name: &str) -> String {
        self.extern_c_symbol
            .clone()
            .unwrap_or_else(|| zz_name.replace('.', "_"))
    }
}

/// A registered struct definition: field names and their types.
#[derive(Debug, Clone)]
pub struct StructSig {
    pub fields: Vec<(String, Type)>,
}

/// Minimal error-conversion registration (V1, error-only — not a general trait system).
/// Declared via `impl From { func convert_to_To(self) -> To { ... } }`.
/// V1 rule: at most one conversion per source type (keeps runtime dispatch sound).
#[derive(Debug, Clone)]
pub struct ConvertImpl {
    pub from: Type,
    pub from_name: String,
    pub to: Type,
    pub to_name: String,
    pub func_name: String,
    pub span: Span,
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
    /// `try` site span → conversion impl span (`None` = identity).
    pub try_resolutions: HashMap<Span, Option<Span>>,
    /// `try` site span → conversion function name (`None` = identity).
    /// Resolved from `try_resolutions` + the `convert_to_` registry so
    /// downstream passes (HIR, native codegen) can emit the conversion
    /// call without re-deriving it. Mirrors `try_resolutions` 1:1.
    pub try_converts: HashMap<Span, Option<String>>,
    /// Native libraries requested via `@link("lib")`, in source order, deduped.
    pub link_libs: Vec<String>,
    /// Top-level `const` bindings and their declaration spans. Used to seed
    /// later REPL snippets so immutable names stay immutable across evals.
    pub const_bindings: HashMap<String, Span>,
    /// Only `pub` bindings (for cross-module export).
    pub pub_bindings: HashMap<String, Type>,
    /// Only `pub` functions (for cross-module export).
    pub pub_funcs: HashMap<String, FuncSig>,
    /// Only `pub` structs (for cross-module export).
    pub pub_structs: HashMap<String, StructSig>,
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
    check_program_impl(
        program,
        initial_bindings,
        initial_funcs,
        initial_structs,
        HashMap::new(),
    )
    .result
}

/// Like [`check_program`], but seeds the `const` (immutable) bindings from a
/// prior session so REPL snippets remember which names are immutable.
pub fn check_program_with_consts(
    program: &Program,
    initial_bindings: HashMap<String, Type>,
    initial_funcs: HashMap<String, FuncSig>,
    initial_structs: HashMap<String, StructSig>,
    initial_consts: HashMap<String, Span>,
) -> CheckResult {
    check_program_impl(
        program,
        initial_bindings,
        initial_funcs,
        initial_structs,
        initial_consts,
    )
    .result
}

/// Like [`check_program`], but also returns a deep-resolved type annotation
/// map keyed by expression span. The map is the typed view of the AST used
/// by the HIR builder for native codegen.
pub fn check_program_typed(
    program: &Program,
    initial_bindings: HashMap<String, Type>,
    initial_funcs: HashMap<String, FuncSig>,
    initial_structs: HashMap<String, StructSig>,
) -> (CheckResult, std::collections::HashMap<Span, Type>) {
    let out = check_program_impl(
        program,
        initial_bindings,
        initial_funcs,
        initial_structs,
        HashMap::new(),
    );
    (out.result, out.span_types)
}

/// Core pass shared by [`check_program`], [`check_program_with_consts`], and
/// [`check_program_typed`].
fn check_program_impl(
    program: &Program,
    initial_bindings: HashMap<String, Type>,
    initial_funcs: HashMap<String, FuncSig>,
    initial_structs: HashMap<String, StructSig>,
    initial_consts: HashMap<String, Span>,
) -> CheckerOutcome {
    // Resolve explicit decorators first: `@dec func f` becomes `f__inner` +
    // a same-signature wrapper, so all downstream passes see ordinary
    // functions and calls. Idempotent — already-expanded programs pass
    // through unchanged.
    let (expanded, mut decorator_errors) = zz_frontend::decorators::expand_program(program);
    let program = &expanded;
    let mut checker = Checker::new(
        initial_bindings,
        initial_funcs,
        initial_structs,
        initial_consts,
    );
    checker.errors.append(&mut decorator_errors);

    // Track which items are pub (for cross-module export).
    let mut pub_bindings_set: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut pub_funcs_set: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut pub_structs_set: std::collections::HashSet<String> = std::collections::HashSet::new();

    // Pass 1: register struct definitions (fields are resolved against the
    // struct registry, so structs may reference earlier structs). Structs
    // must be registered before functions so `func f(p: Point)` resolves.
    let mut seen_structs = HashMap::new();
    for stmt in &program.stmts {
        if let Stmt::Struct {
            name, span, pub_, ..
        } = stmt
        {
            let full_name = name.join(".");
            if let Some(prev) = seen_structs.insert(full_name.clone(), *span) {
                checker.errors.push(zz_frontend::diag::error_at(
                    format!("duplicate definition of struct `{}`", full_name),
                    *span,
                ));
                checker.errors.push(zz_frontend::diag::error_at(
                    "previous definition here",
                    prev,
                ));
            }
            checker.collect_struct(stmt);
            if *pub_ {
                pub_structs_set.insert(full_name);
            }
        }
    }

    // Pass 1b: register impl method signatures so method calls resolve.
    // Inherent (`impl KnownStruct`) → `funcs[Type.method]` (existing).
    // Extension (`impl` on builtins or unknown/cross-module types) →
    // `ext_methods[(TypeKey, method)]`, merged into `funcs` when no conflict.
    // Priority downstream: inherent → extension → stdlib. Builtin-wins and
    // orphan duplicates are compile errors here, not last-wins.
    let mut seen = HashMap::new();
    for stmt in &program.stmts {
        if let Stmt::Impl { name, methods, .. } = stmt {
            let type_name = name.join(".");
            let is_known_struct = checker.structs.contains_key(&type_name);
            let builtin_key = Checker::builtin_ext_key(&type_name);
            let is_extension = builtin_key.is_some() || !is_known_struct;
            let type_key = builtin_key.unwrap_or_else(|| type_name.clone());
            for method in methods {
                if let Stmt::Func {
                    name: mname,
                    params,
                    ret,
                    generics,
                    pub_: m_pub,
                    ..
                } = method
                {
                    let method_name = mname.join(".");
                    let full_name = format!("{}.{}", type_key, method_name);
                    let gen_names: Vec<String> =
                        generics.iter().map(|g| g.name.name.clone()).collect();
                    let gen_bounds: Vec<(String, Vec<zz_frontend::ast::TraitBound>)> = generics
                        .iter()
                        .map(|g| (g.name.name.clone(), g.bounds.clone()))
                        .collect();
                    // Build params, replacing `self` with the receiver type
                    // (builtin mapped, else struct by name).
                    let self_ty =
                        Checker::self_type_for_impl(&type_name, &type_key, &mut checker.unifier);
                    let sig_params: Vec<(String, Type)> = params
                        .iter()
                        .enumerate()
                        .map(|(i, p)| {
                            let ty = if i == 0 && p.name.name == "self" {
                                self_ty.clone()
                            } else {
                                match &p.ty {
                                    Some(t) => checker.ast_to_type(t, &gen_names),
                                    None => checker.unifier.fresh_var(),
                                }
                            };
                            (p.name.name.clone(), ty)
                        })
                        .collect();
                    let has_default: Vec<bool> =
                        params.iter().map(|p| p.default.is_some()).collect();
                    let sig_ret = match ret {
                        Some(t) => checker.ast_to_type(t, &gen_names),
                        None => checker.unifier.fresh_var(),
                    };
                    let sig = crate::checker::FuncSig {
                        generics: gen_names,
                        bounds: gen_bounds,
                        params: sig_params,
                        has_default,
                        ret: sig_ret.clone(),
                        is_extern: false,
                        extern_c_symbol: None,
                    };
                    // `convert_to_X` methods double as error-conversion impls.
                    if let Some(to_name) = method_name.strip_prefix("convert_to_") {
                        if to_name.is_empty() {
                            checker.errors.push(zz_frontend::diag::error_at(
                                "`convert_to_` must name a target type (e.g. `convert_to_MyErr`)",
                                method.span(),
                            ));
                        } else if matches!(sig_ret, Type::Var(_)) {
                            checker.errors.push(zz_frontend::diag::error_at(
                                "conversion method must declare an explicit return type",
                                method.span(),
                            ));
                        } else if let Some(prev) = checker
                            .convert_impls
                            .iter()
                            .find(|c| c.from_name == type_key)
                        {
                            // V1: one conversion per source type (covers the
                            // spec's same-pair ambiguity and keeps runtime
                            // single-candidate dispatch sound).
                            checker.errors.push(zz_frontend::diag::error_at(
                                format!(
                                    "ambiguous conversion from `{}`: already converts to `{}`",
                                    type_key, prev.to_name
                                ),
                                method.span(),
                            ));
                            checker.errors.push(zz_frontend::diag::error_at(
                                "previous conversion here",
                                prev.span,
                            ));
                        } else {
                            checker.convert_impls.push(ConvertImpl {
                                from: self_ty.clone(),
                                from_name: type_key.clone(),
                                to: sig_ret.clone(),
                                to_name: to_name.to_string(),
                                func_name: full_name.clone(),
                                span: method.span(),
                            });
                        }
                    }
                    if let Some(prev) = seen.insert(full_name.clone(), method.span()) {
                        checker.errors.push(zz_frontend::diag::error_at(
                            format!("duplicate definition of method `{}`", full_name),
                            method.span(),
                        ));
                        checker.errors.push(zz_frontend::diag::error_at(
                            "previous definition here",
                            prev,
                        ));
                        continue;
                    }
                    if !is_extension {
                        // Inherent methods keep historical last-wins across
                        // modules (pure-ZZ stdlib defines e.g. `Regexp.new`
                        // in more than one source); within-module duplicates
                        // are still rejected via `seen` above.
                        checker.funcs.insert(full_name.clone(), sig);
                    } else {
                        // Builtin-wins: an extension colliding with an existing
                        // inherent/stdlib method is an error naming the origin.
                        if checker.funcs.contains_key(&full_name) {
                            let origin = if Checker::is_stdlib_method(&full_name) {
                                "defined in the standard library"
                            } else {
                                "already defined in another module (orphan rule: same (Type, method) in two modules)"
                            };
                            checker.errors.push(zz_frontend::diag::error_at(
                                format!(
                                    "extension method `{}` conflicts with an existing method {}",
                                    full_name, origin
                                ),
                                method.span(),
                            ));
                            continue;
                        }
                        if let Some((_, prev_span)) = checker
                            .ext_methods
                            .get(&(type_key.clone(), method_name.clone()))
                        {
                            checker.errors.push(zz_frontend::diag::error_at(
                                format!("duplicate extension method `{}` (orphan rule)", full_name),
                                method.span(),
                            ));
                            checker.errors.push(zz_frontend::diag::error_at(
                                "previous definition here",
                                *prev_span,
                            ));
                            continue;
                        }
                        checker.ext_methods.insert(
                            (type_key.clone(), method_name.clone()),
                            (sig.clone(), method.span()),
                        );
                        // Merge so HIR/callgraph/codegen/runtime resolve it.
                        checker.funcs.insert(full_name.clone(), sig);
                    }
                    // Only `pub` methods are visible cross-module. `pub impl`
                    // is rejected by the parser, so `pub` goes on the method.
                    if *m_pub {
                        pub_funcs_set.insert(full_name);
                    }
                }
            }
        }
    }

    // Pass 1c: register all function signatures so recursion and mutual
    // recursion resolve.
    for stmt in &program.stmts {
        if let Stmt::Func {
            name, span, pub_, ..
        } = stmt
        {
            let full_name = name.join(".");
            if let Some(prev) = seen.insert(full_name.clone(), *span) {
                checker.errors.push(zz_frontend::diag::error_at(
                    format!("duplicate definition of function `{}`", full_name),
                    *span,
                ));
                checker.errors.push(zz_frontend::diag::error_at(
                    "previous definition here",
                    prev,
                ));
            }
            checker.collect_func(stmt);
            if *pub_ {
                pub_funcs_set.insert(full_name);
            }
        }
        // Pass 1d: register `extern "C"` signatures (no bodies to check).
        if let Stmt::ExternBlock { items, .. } = stmt {
            for item in items {
                if let Some(prev) = seen.insert(item.name.name.clone(), item.span) {
                    checker.errors.push(zz_frontend::diag::error_at(
                        format!("duplicate definition of function `{}`", item.name.name),
                        item.span,
                    ));
                    checker.errors.push(zz_frontend::diag::error_at(
                        "previous definition here",
                        prev,
                    ));
                }
                checker.collect_extern(&item.name, &item.params, &item.ret, item.c_symbol.clone());
            }
        }
        // `@link` is collected in Pass 2 (check_stmt) to preserve order/dedup.
    }

    // Pass 2: check top-level statements in order.
    for stmt in &program.stmts {
        // Track pub on Decl before checking.
        if let Stmt::Decl { name, pub_, .. } = stmt {
            if *pub_ {
                pub_bindings_set.insert(name.name.clone());
            }
        }
        checker.check_stmt(stmt);
    }

    // Finalize: bindings that still contain inference variables were already
    // reported inline (see check_stmt `Let`); skip them so the session never
    // seeds an unresolved type.
    let mut bindings = HashMap::new();
    for (name, ty) in &checker.new_bindings {
        let rt = checker.unifier.resolve_deep(ty);
        if !inference::contains_var(&rt) {
            bindings.insert(name.clone(), rt);
        }
    }

    // Populate pub_names so unused-warning logic can skip pub items.
    checker.pub_names = pub_bindings_set
        .union(&pub_funcs_set)
        .chain(pub_structs_set.iter())
        .cloned()
        .collect();

    // Emit unused variable warnings for the global scope (the top scope
    // is never popped, so pop_scope's check never fires for it).
    checker.emit_global_unused_warnings();

    // Build pub-only maps for cross-module export.
    let pub_bindings: HashMap<String, Type> = bindings
        .iter()
        .filter(|(k, _)| pub_bindings_set.contains(k.as_str()))
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    let pub_funcs: HashMap<String, FuncSig> = checker
        .funcs
        .iter()
        .filter(|(k, _)| pub_funcs_set.contains(k.as_str()))
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    let pub_structs: HashMap<String, StructSig> = checker
        .structs
        .iter()
        .filter(|(k, _)| pub_structs_set.contains(k.as_str()))
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();

    // Deep-resolve the recorded span types now that all unification is done.
    // Skip any that still contain inference variables (unresolvable at
    // compile time — the node lowers dynamically).
    let mut span_types: std::collections::HashMap<Span, Type> = std::collections::HashMap::new();
    for (span, ty) in &checker.span_types {
        let rt = checker.unifier.resolve_deep(ty);
        if !inference::contains_var(&rt) {
            span_types.insert(*span, rt);
        }
    }

    // Top-level `const` bindings (name → declaration span) for session
    // persistence across REPL snippets.
    let const_bindings: HashMap<String, Span> =
        checker.const_env.first().cloned().unwrap_or_default();

    // Resolve each `try` site to its conversion function name so native
    // codegen can emit the call directly. `try_resolutions` holds the impl
    // span; join it against the `convert_to_` registries:
    //   1. inherent `convert_impls` (span match → func_name),
    //   2. extension methods (span match → `Type.method`),
    //   3. anything else → identity (matches the checker's fallbacks).
    let mut try_converts: HashMap<Span, Option<String>> = HashMap::new();
    for (span, impl_span) in &checker.try_resolutions {
        let func_name: Option<String> = match impl_span {
            None => None,
            Some(ispan) => {
                if let Some(c) = checker.convert_impls.iter().find(|c| c.span == *ispan) {
                    Some(c.func_name.clone())
                } else if let Some(((tkey, mname), _)) = checker
                    .ext_methods
                    .iter()
                    .find(|(_, (_, mspan))| *mspan == *ispan)
                {
                    Some(format!("{tkey}.{mname}"))
                } else {
                    None
                }
            }
        };
        try_converts.insert(*span, func_name);
    }

    CheckerOutcome {
        result: CheckResult {
            errors: checker.errors,
            bindings,
            funcs: checker.funcs,
            structs: checker.structs,
            try_resolutions: checker.try_resolutions,
            try_converts,
            link_libs: checker.link_libs,
            const_bindings,
            pub_bindings,
            pub_funcs,
            pub_structs,
        },
        span_types,
    }
}

/// Result of the check pass: the public [`CheckResult`] plus the typed AST
/// view (span → resolved type) for codegen.
struct CheckerOutcome {
    result: CheckResult,
    span_types: std::collections::HashMap<Span, Type>,
}

pub(crate) struct Checker {
    pub(crate) unifier: crate::unify::Unifier,
    pub(crate) errors: Vec<zz_frontend::diag::RawDiag>,
    pub(crate) funcs: HashMap<String, FuncSig>,
    pub(crate) structs: HashMap<String, StructSig>,
    /// Extension methods (separate table): (TypeName, method) → (sig, def span).
    /// Holds `impl` on builtins and cross-type extensions. Lookup priority:
    /// inherent (`funcs`) → extension (here) → stdlib namespace.
    pub(crate) ext_methods: HashMap<(String, String), (FuncSig, Span)>,
    /// Minimal `convert_to_` registry for `try` error conversion.
    pub(crate) convert_impls: Vec<ConvertImpl>,
    /// `try` site span → convert impl span (`None` = identity, no conversion).
    /// Consumed by LSP hover to show the resolved conversion.
    pub(crate) try_resolutions: HashMap<Span, Option<Span>>,
    pub(crate) env: Vec<HashMap<String, Type>>,
    /// Names declared `const` in each scope, mapped to their declaration
    /// span (for the "defined as immutable here" secondary label). Parallel
    /// to `env` — a name is immutable iff the scope that binds it also
    /// contains it here.
    pub(crate) const_env: Vec<HashMap<String, zz_frontend::span::Span>>,
    /// Top-level let bindings discovered this run: name → type.
    pub(crate) new_bindings: HashMap<String, Type>,
    pub(crate) current_ret: Option<Type>,
    pub(crate) current_generics: Vec<String>,
    /// Trait bounds for the generic parameters currently in scope (set while
    /// checking a generic function body).
    pub(crate) current_bounds: HashMap<String, Vec<zz_frontend::ast::TraitBound>>,
    /// Nesting depth of `for`/`while` loops (for `break`/`continue`).
    pub(crate) loop_depth: usize,
    /// Names that were used (looked up) — for unused-variable warnings.
    pub(crate) used_names: std::collections::HashSet<String>,
    /// Top-level names marked `pub` — should not emit unused warnings.
    pub(crate) pub_names: std::collections::HashSet<String>,
    /// Names defined in the current scope with their spans — for unused
    /// variable warnings. Each scope level has its own map.
    pub(crate) defined_names: Vec<HashMap<String, zz_frontend::span::Span>>,
    /// Tracks whether the most recent `lookup()` produced an "undefined
    /// variable" error.  Used by `check_call` to suppress the secondary
    /// "cannot call a value of type unit" cascading error.
    pub(crate) had_undefined_var: bool,
    /// Imported namespaces: (alias, span). Used to detect unused imports.
    pub(crate) imports: Vec<(String, zz_frontend::span::Span)>,
    /// Selective-import aliases: bare name → qualified `ns.sym`, from
    /// `import m(x)` / `import m(x as y)` statements (kept intact by the
    /// loader). Used ONLY on total miss: locals, seed entries and value
    /// Decls all take precedence. This is how bare calls to *generic*
    /// functions resolve — generics have no value type, so no binding
    /// is ever created for them.
    pub(crate) import_aliases: HashMap<String, String>,
    /// Resolved type per expression span, recorded during the type walk.
    /// Used by the HIR builder to attach a resolved `Type` to every AST node.
    pub(crate) span_types: std::collections::HashMap<zz_frontend::span::Span, Type>,
    /// Native libraries requested via `@link`, in source order, deduped.
    pub(crate) link_libs: Vec<String>,
}

impl Checker {
    pub(crate) fn new(
        initial_bindings: HashMap<String, Type>,
        funcs: HashMap<String, FuncSig>,
        structs: HashMap<String, StructSig>,
        initial_consts: HashMap<String, Span>,
    ) -> Self {
        let env = vec![initial_bindings];
        Checker {
            unifier: crate::unify::Unifier::new(),
            errors: Vec::new(),
            funcs,
            structs,
            ext_methods: HashMap::new(),
            convert_impls: Vec::new(),
            try_resolutions: HashMap::new(),
            env,
            const_env: vec![initial_consts],
            new_bindings: HashMap::new(),
            current_ret: None,
            current_generics: Vec::new(),
            current_bounds: HashMap::new(),
            loop_depth: 0,
            used_names: std::collections::HashSet::new(),
            pub_names: std::collections::HashSet::new(),
            defined_names: vec![HashMap::new()],
            had_undefined_var: false,
            imports: Vec::new(),
            import_aliases: HashMap::new(),
            span_types: std::collections::HashMap::new(),
            link_libs: Vec::new(),
        }
    }

    /// Get the full name of a function statement.
    pub(crate) fn func_name(stmt: &Stmt) -> String {
        match stmt {
            Stmt::Func { name, .. } => name.join("."),
            _ => unreachable!(),
        }
    }

    /// Canonical extension key for builtin receiver names.
    /// `str`→`str`, `Array`/`vec`→`vec`, `Option`/`option`→`option`,
    /// `Result`/`result`→`result`, scalars map to themselves.
    pub(crate) fn builtin_ext_key(type_name: &str) -> Option<String> {
        match type_name {
            "str" => Some("str".to_string()),
            "int" => Some("int".to_string()),
            "float" => Some("float".to_string()),
            "bool" => Some("bool".to_string()),
            "vec" | "Array" => Some("vec".to_string()),
            "option" | "Option" => Some("option".to_string()),
            "result" | "Result" => Some("result".to_string()),
            _ => None,
        }
    }

    /// Receiver type for `self` in an `impl` block.
    pub(crate) fn self_type_for_impl(
        type_name: &str,
        type_key: &str,
        unifier: &mut crate::unify::Unifier,
    ) -> Type {
        match type_key {
            "str" => Type::Str,
            "int" => Type::Int,
            "float" => Type::Float,
            "bool" => Type::Bool,
            "vec" => Type::Array(Box::new(unifier.fresh_var())),
            "option" => Type::Option(Box::new(unifier.fresh_var())),
            "result" => Type::Result(Box::new(unifier.fresh_var()), Box::new(unifier.fresh_var())),
            _ => Type::Struct(type_name.to_string()),
        }
    }

    /// True when `Type.method` is a stdlib-provided builtin (builtin-wins).
    /// The namespace list mirrors `STDLIB_MODULES` in `zz_stdlib/src/lib.rs`
    /// plus the method-dispatch namespaces used by `check_call`
    /// (`option`/`result`/`net`/`db`) — i.e. every namespace whose `ns.method`
    /// keys can arrive via the seeded `funcs` map. Collision *coverage* does
    /// not depend on this list (any seeded `funcs` key collides); this only
    /// decides the message wording (stdlib vs another module).
    pub(crate) fn is_stdlib_method(full_name: &str) -> bool {
        if let Some((ns, _)) = full_name.rsplit_once('.') {
            // Strip a leading `std.` (`std.str.length` → `str`) and compare
            // the first segment (`sqlz.postgres.query` → `sqlz`).
            let short = ns.strip_prefix("std.").unwrap_or(ns);
            let head = short.split('.').next().unwrap_or(short);
            let head = head.to_lowercase();
            matches!(
                head.as_str(),
                "io" | "str"
                    | "vec"
                    | "json"
                    | "http"
                    | "fs"
                    | "env"
                    | "math"
                    | "time"
                    | "encoding"
                    | "net"
                    | "chan"
                    | "task"
                    | "regexp"
                    | "regex"
                    | "crypto"
                    | "log"
                    | "sys"
                    | "args"
                    | "process"
                    | "uuid"
                    | "sqlz"
                    | "db"
                    | "option"
                    | "result"
            )
        } else {
            false
        }
    }
}
