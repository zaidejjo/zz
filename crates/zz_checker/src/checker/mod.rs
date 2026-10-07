//! Type checker: HM-lite inference, generics, patterns, exhaustiveness.

pub mod aliases;
pub mod diagnostics;
pub mod enums;
pub mod funcs;
pub mod http_lint;
pub mod inference;
pub mod scope;
pub mod structs;
pub mod type_check;

use std::collections::HashMap;
use std::sync::Arc;

use zz_frontend::ast::{Program, Stmt, Ty};
use zz_frontend::diag::RawDiag;
use zz_frontend::span::Span;

use crate::type_::Type;

/// Pseudo-scope owning all top-level (non-function) statements. Mirrors
/// `zz_hir::callgraph::TOP`: expression spans are only unique *within* one
/// function body (every module restarts offsets at 0), so span-keyed maps
/// must always pair the span with its enclosing scope.
pub const TOP_SCOPE: &str = "<top>";

/// Scope-qualified expression key for the typed AST view. Plain `Span`
/// keys collide across modules (and across stdlib files): two different
/// nodes at the same offsets would overwrite each other in
/// `span_types`, and native codegen would lower one with the other's
/// type (e.g. a string accumulator lowered as `bool`, printing "false").
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SpanKey {
    /// Enclosing top-level function (`Type.method` for methods,
    /// [`TOP_SCOPE`] otherwise). Shared (`Arc`, not `String`): one key is
    /// built per checked expression, and (atomic) refcounting kills an allocation
    /// per node with zero hashing cost change.
    pub func: Arc<str>,
    pub span: Span,
}

impl SpanKey {
    pub fn new(func: impl Into<Arc<str>>, span: Span) -> Self {
        SpanKey {
            func: func.into(),
            span,
        }
    }
}

/// A registered function signature.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
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

/// A registered struct definition: type parameters and field types
/// (which may reference the parameters as `Type::Named`).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct StructSig {
    pub generics: Vec<String>,
    pub fields: Vec<(String, Type)>,
}

/// A registered type alias: type parameters and the target type with
/// parameters as `Type::Named` (e.g. `type Pair[T] = (T, T)` stores
/// `Tuple([Named("T"), Named("T")])`). Uses resolve to the target, so
/// aliases never appear at runtime.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct AliasSig {
    pub generics: Vec<String>,
    pub target: Type,
}
/// A registered user enum: type parameters and variant names with
/// optional payload types (`enum Box[T] { V(T), E }` stores
/// `generics: ["T"]`, `variants: [("V", Some(Named("T"))),
/// ("E", None)]`). Construction (`Box.V(1)`) and patterns (`.V(v)`)
/// resolve against this table; values erase to `Object`s at runtime.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct EnumSig {
    pub generics: Vec<String>,
    pub variants: Vec<(String, Option<Type>)>,
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
    /// Top-level type aliases (`type Tokens = [Token]`), resolved targets.
    pub aliases: HashMap<String, AliasSig>,
    /// Top-level user enums (`enum Token { ... }`).
    pub enums: HashMap<String, EnumSig>,
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
    /// Only `pub` type aliases (for cross-module export).
    pub pub_aliases: HashMap<String, AliasSig>,
    /// Only `pub` user enums (for cross-module export).
    pub pub_enums: HashMap<String, EnumSig>,
}

/// Type-check a whole program, seeded with bindings/funcs/structs from prior
/// REPL evals. Errors are collected (not fatal); the program should not run
/// if any are present.
pub fn check_program(
    program: &Program,
    initial_bindings: HashMap<String, Type>,
    initial_funcs: HashMap<String, FuncSig>,
    initial_structs: HashMap<String, StructSig>,
    initial_aliases: HashMap<String, AliasSig>,
    initial_enums: HashMap<String, EnumSig>,
) -> CheckResult {
    check_program_impl(
        program,
        initial_bindings,
        initial_funcs,
        initial_structs,
        initial_aliases,
        initial_enums,
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
    initial_aliases: HashMap<String, AliasSig>,
    initial_enums: HashMap<String, EnumSig>,
    initial_consts: HashMap<String, Span>,
) -> CheckResult {
    check_program_impl(
        program,
        initial_bindings,
        initial_funcs,
        initial_structs,
        initial_aliases,
        initial_enums,
        initial_consts,
    )
    .result
}

/// Like [`check_program`], but also returns a deep-resolved type annotation
/// map keyed by scoped expression key. The map is the typed view of the AST used
/// by the HIR builder for native codegen.
pub fn check_program_typed(
    program: &Program,
    initial_bindings: HashMap<String, Type>,
    initial_funcs: HashMap<String, FuncSig>,
    initial_structs: HashMap<String, StructSig>,
    initial_aliases: HashMap<String, AliasSig>,
    initial_enums: HashMap<String, EnumSig>,
) -> (CheckResult, std::collections::HashMap<SpanKey, Type>) {
    let out = check_program_impl(
        program,
        initial_bindings,
        initial_funcs,
        initial_structs,
        initial_aliases,
        initial_enums,
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
    initial_aliases: HashMap<String, AliasSig>,
    initial_enums: HashMap<String, EnumSig>,
    initial_consts: HashMap<String, Span>,
) -> CheckerOutcome {
    // Resolve explicit decorators first: `@dec func f` becomes `f__inner` +
    // a same-signature wrapper, so all downstream passes see ordinary
    // functions and calls. Idempotent — already-expanded programs pass
    // through unchanged.
    //
    // Skip fast path: decorator-free programs (the common case) check
    // in place with zero cloning. HIR already expanded once upstream, so
    // without this every compile paid a second full-program deep clone
    // for a no-op re-expansion.
    let mut owned: Option<Program> = None;
    let mut decorator_errors = Vec::new();
    if zz_frontend::decorators::has_any_decorators(program) {
        let (expanded, errs) = zz_frontend::decorators::expand_program(program);
        decorator_errors = errs;
        owned = Some(expanded);
    }
    let program: &Program = owned.as_ref().map_or(program, |o| o);
    let mut checker = Checker::new(
        initial_bindings,
        initial_funcs,
        initial_structs,
        initial_aliases,
        initial_enums,
        initial_consts,
    );
    // Offset fresh-var ids above any `Var(id)` carried in seed signatures
    // from already-checked modules. Those ids were allocated by a previous
    // checker's unifier; reusing them here would unify unrelated types
    // across the module boundary.
    {
        let mut max_seen: Option<u32> = None;
        let bump = |max_seen: &mut Option<u32>, t: &Type| {
            if let Some(id) = inference::max_var_id(t) {
                *max_seen = Some(max_seen.map_or(id, |m| m.max(id)));
            }
        };
        for ty in checker.env.iter().flat_map(|s| s.values()) {
            bump(&mut max_seen, ty);
        }
        for sig in checker.funcs.values() {
            for (_, pt) in &sig.params {
                bump(&mut max_seen, pt);
            }
            bump(&mut max_seen, &sig.ret);
        }
        for sig in checker.structs.values() {
            for (_, ft) in &sig.fields {
                bump(&mut max_seen, ft);
            }
        }
        if let Some(m) = max_seen {
            checker.unifier.reserve_vars_above(m.saturating_add(1));
        }
    }
    checker.errors.append(&mut decorator_errors);

    // Track which items are pub (for cross-module export).
    let mut pub_bindings_set: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut pub_funcs_set: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut pub_structs_set: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut pub_aliases_set: std::collections::HashSet<String> = std::collections::HashSet::new();

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

    // Pass 1a: register type aliases. Raw targets are recorded first so
    // aliases may reference structs regardless of order; conversion to
    // resolved types happens after (nested aliases expand recursively
    // with cycle detection in `convert_alias`).
    let mut seen_aliases = HashMap::new();
    for stmt in &program.stmts {
        if let Stmt::TypeAlias {
            name,
            generics,
            target,
            span,
            pub_,
        } = stmt
        {
            let full_name = name.join(".");
            if checker.structs.contains_key(&full_name) {
                checker.errors.push(zz_frontend::diag::error_at(
                    format!(
                        "duplicate definition of `{}` (a struct with the same name exists)",
                        full_name
                    ),
                    *span,
                ));
            }
            if let Some(prev) = seen_aliases.insert(full_name.clone(), *span) {
                checker.errors.push(zz_frontend::diag::error_at(
                    format!("duplicate definition of type alias `{}`", full_name),
                    *span,
                ));
                checker.errors.push(zz_frontend::diag::error_at(
                    "previous definition here",
                    prev,
                ));
            }
            // Reject duplicate type parameters (`type P[T, T] = ...`).
            let mut seen = std::collections::HashSet::new();
            let mut gen_names = Vec::new();
            for g in generics {
                if !seen.insert(g.name.clone()) {
                    checker.errors.push(zz_frontend::diag::error_at(
                        format!(
                            "duplicate type parameter `{}` in alias `{}`",
                            g.name, full_name
                        ),
                        g.span,
                    ));
                } else {
                    gen_names.push(g.name.clone());
                }
            }
            checker
                .alias_asts
                .insert(full_name.clone(), (gen_names, target.clone()));
            if *pub_ {
                pub_aliases_set.insert(full_name);
            }
        }
    }
    // Convert raw targets now that every struct and alias name is known.
    // (Moved after enum collection below: alias targets may name enums,
    // and enum payloads may name aliases — both tables must be complete
    // before either resolves. Conversion is lazy-safe regardless.)
    // Pass 1b: register user enums. Payload types resolve against the
    // struct and alias tables (already collected); alias conversion
    // runs after so targets may also reference enums.
    let mut seen_enums = HashMap::new();
    let mut pub_enums_set: std::collections::HashSet<String> = std::collections::HashSet::new();
    for stmt in &program.stmts {
        if let Stmt::Enum {
            name,
            generics,
            variants,
            span,
            pub_,
        } = stmt
        {
            let full_name = name.join(".");
            if checker.structs.contains_key(&full_name) {
                checker.errors.push(zz_frontend::diag::error_at(
                    format!(
                        "duplicate definition of `{}` (a struct with the same name exists)",
                        full_name
                    ),
                    *span,
                ));
            }
            if checker.alias_asts.contains_key(&full_name) {
                checker.errors.push(zz_frontend::diag::error_at(
                    format!(
                        "duplicate definition of `{}` (a type alias with the same name exists)",
                        full_name
                    ),
                    *span,
                ));
            }
            if let Some(prev) = seen_enums.insert(full_name.clone(), *span) {
                checker.errors.push(zz_frontend::diag::error_at(
                    format!("duplicate definition of enum `{}`", full_name),
                    *span,
                ));
                checker.errors.push(zz_frontend::diag::error_at(
                    "previous definition here",
                    prev,
                ));
            }
            // Reject duplicate type parameters (`enum P[T, T]`).
            let mut seen_params = std::collections::HashSet::new();
            let mut gen_names = Vec::new();
            for g in generics {
                if !seen_params.insert(g.name.clone()) {
                    checker.errors.push(zz_frontend::diag::error_at(
                        format!(
                            "duplicate type parameter `{}` in enum `{}`",
                            g.name, full_name
                        ),
                        g.span,
                    ));
                } else {
                    gen_names.push(g.name.clone());
                }
            }
            let mut resolved = Vec::new();
            for (vname, payload) in variants {
                let pty = payload
                    .as_ref()
                    .map(|t| checker.ast_to_type_inner(t, &gen_names));
                resolved.push((vname.name.clone(), pty));
            }
            checker.enums.insert(
                full_name.clone(),
                EnumSig {
                    generics: gen_names,
                    variants: resolved,
                },
            );
            if *pub_ {
                pub_enums_set.insert(full_name);
            }
        }
    }
    let alias_names: Vec<String> = checker.alias_asts.keys().cloned().collect();
    for alias_name in alias_names {
        checker.convert_alias(&alias_name);
    }

    // Pass 1b: register impl method signatures so method calls resolve.
    // Inherent (`impl KnownStruct`) → `funcs[Type.method]` (existing).
    // Extension (`impl` on builtins or unknown/cross-module types) →
    // `ext_methods[(TypeKey, method)]`, merged into `funcs` when no conflict.
    // Priority downstream: inherent → extension → stdlib. Builtin-wins and
    // orphan duplicates are compile errors here, not last-wins.
    let mut seen = HashMap::new();
    for stmt in &program.stmts {
        if let Stmt::Impl {
            name,
            generics: impl_generics,
            methods,
            ..
        } = stmt
        {
            let type_name = name.join(".");
            let is_known_struct = checker.structs.contains_key(&type_name);
            // `impl` on a known enum is inherent, exactly like structs:
            // methods land in `funcs[Enum.method]` with historical
            // last-wins (re-checks of the same program stay idempotent
            // instead of tripping the extension orphan rule).
            let is_known_enum = checker.enums.contains_key(&type_name);
            let builtin_key = Checker::builtin_ext_key(&type_name);
            let is_extension = builtin_key.is_some() || !(is_known_struct || is_known_enum);
            let type_key = builtin_key.unwrap_or_else(|| type_name.clone());
            // Generic structs need a matching generic impl (`impl Box[T]`);
            // the parameters scope over every method below. Generic
            // enums follow the identical rule.
            let struct_generics: Vec<String> = checker
                .structs
                .get(&type_name)
                .map(|s| s.generics.clone())
                .unwrap_or_else(|| {
                    checker
                        .enums
                        .get(&type_name)
                        .map(|s| s.generics.clone())
                        .unwrap_or_default()
                });
            let impl_gen_names: Vec<String> =
                impl_generics.iter().map(|g| g.name.clone()).collect();
            if is_extension {
                if !impl_gen_names.is_empty() {
                    checker.errors.push(zz_frontend::diag::error_at(
                        format!(
                            "generic `impl` requires a generic struct (`{type_name}` is not one)"
                        ),
                        stmt.span(),
                    ));
                }
            } else if struct_generics.len() != impl_gen_names.len() {
                // Name the item kind correctly (struct vs enum).
                let kind = if checker.enums.contains_key(&type_name) {
                    "enum"
                } else {
                    "struct"
                };
                if struct_generics.is_empty() {
                    checker.errors.push(zz_frontend::diag::error_at(
                        format!(
                            "{kind} `{type_name}` is not generic (expected `impl {type_name}` without type parameters)"
                        ),
                        stmt.span(),
                    ));
                } else {
                    checker.errors.push(zz_frontend::diag::error_at(
                        format!(
                            "generic {kind} `{type_name}` takes {} type parameter{} (expected `impl {type_name}[{}]`)",
                            struct_generics.len(),
                            if struct_generics.len() == 1 { "" } else { "s" },
                            struct_generics.join(", "),
                        ),
                        stmt.span(),
                    ));
                }
            } else {
                // Duplicate parameter names (`impl Box[T, T]`) shadow each
                // other in substitution maps; reject rather than guess.
                let mut seen_gen = std::collections::HashSet::new();
                for g in &impl_gen_names {
                    if !seen_gen.insert(g.clone()) {
                        checker.errors.push(zz_frontend::diag::error_at(
                            format!("duplicate type parameter `{g}` in `impl {type_name}`"),
                            stmt.span(),
                        ));
                    }
                }
            }
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
                    let method_gens: Vec<String> =
                        generics.iter().map(|g| g.name.name.clone()).collect();
                    // Impl parameters scope over every method (so the
                    // receiver's `Box[int]` unifies `T := int` at call
                    // sites through the existing instantiation path).
                    // A method parameter shadowing an impl parameter
                    // would collapse two distinct variables; reject it.
                    for g in generics {
                        if impl_gen_names.contains(&g.name.name) {
                            checker.errors.push(zz_frontend::diag::error_at(
                                format!(
                                    "type parameter `{}` shadows an `impl {}` parameter (rename one)",
                                    g.name.name, type_name
                                ),
                                g.name.span,
                            ));
                        }
                    }
                    let gen_names: Vec<String> = impl_gen_names
                        .iter()
                        .cloned()
                        .chain(method_gens.iter().cloned())
                        .collect();
                    let gen_bounds: Vec<(String, Vec<zz_frontend::ast::TraitBound>)> = generics
                        .iter()
                        .map(|g| (g.name.name.clone(), g.bounds.clone()))
                        .collect();
                    // Build params, replacing `self` with the receiver type
                    // (builtin mapped, else struct by name — generic structs
                    // keep their parameters as `Named` so calls instantiate
                    // them from the receiver). Enums erase to objects but
                    // keep their identity: `self` is `Type::Enum`, generic
                    // enums keeping their parameters as `Named` too.
                    let self_ty = if let Some(esig) = checker.enums.get(&type_name) {
                        Type::Enum(
                            type_name.clone(),
                            esig.generics
                                .iter()
                                .map(|g| Type::Named(g.clone()))
                                .collect(),
                        )
                    } else {
                        Checker::self_type_for_impl(
                            &type_name,
                            &type_key,
                            &impl_gen_names,
                            &mut checker.unifier,
                        )
                    };
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

    // Pass 2: check top-level statements in order. Non-function items
    // record expression types under TOP_SCOPE (function bodies scope
    // themselves in check_stmt).
    for stmt in &program.stmts {
        // Track pub on Decl before checking.
        if let Stmt::Decl { name, pub_, .. } = stmt {
            if *pub_ {
                pub_bindings_set.insert(name.name.clone());
            }
        }
        let scoped = !matches!(stmt, Stmt::Func { .. } | Stmt::Impl { .. });
        if scoped {
            checker.scope.push(Arc::from(TOP_SCOPE));
        }
        checker.check_stmt(stmt);
        if scoped {
            checker.scope.pop();
        }
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
        .chain(pub_aliases_set.iter())
        .chain(pub_enums_set.iter())
        .cloned()
        .collect();

    // Emit unused variable warnings for the global scope (the top scope
    // is never popped, so pop_scope's check never fires for it).
    checker.emit_global_unused_warnings();

    // Build pub-only maps for cross-module export. Signature types are
    // deep-resolved so inferred returns (e.g. `ping()` with no annotation
    // unifies its ret var to `unit` during body checking) export as
    // concrete types, never as the defining checker's `Var(id)` — a bare
    // id would collide with the importer's own fresh vars.
    let pub_bindings: HashMap<String, Type> = bindings
        .iter()
        .filter(|(k, _)| pub_bindings_set.contains(k.as_str()))
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    let pub_funcs: HashMap<String, FuncSig> = checker
        .funcs
        .iter()
        .filter(|(k, _)| pub_funcs_set.contains(k.as_str()))
        .map(|(k, v)| {
            let mut sig = v.clone();
            sig.params = sig
                .params
                .iter()
                .map(|(n, t)| (n.clone(), checker.unifier.resolve_deep(t)))
                .collect();
            sig.ret = checker.unifier.resolve_deep(&sig.ret);
            (k.clone(), sig)
        })
        .collect();
    let pub_structs: HashMap<String, StructSig> = checker
        .structs
        .iter()
        .filter(|(k, _)| pub_structs_set.contains(k.as_str()))
        .map(|(k, v)| {
            let mut sig = v.clone();
            sig.fields = sig
                .fields
                .iter()
                .map(|(n, t)| (n.clone(), checker.unifier.resolve_deep(t)))
                .collect();
            (k.clone(), sig)
        })
        .collect();
    let pub_aliases: HashMap<String, AliasSig> = checker
        .aliases
        .iter()
        .filter(|(k, _)| pub_aliases_set.contains(k.as_str()))
        .map(|(k, v)| {
            let mut sig = v.clone();
            sig.target = checker.unifier.resolve_deep(&sig.target);
            (k.clone(), sig)
        })
        .collect();

    // Deep-resolve the recorded span types now that all unification is done.
    // Skip any that still contain inference variables (unresolvable at
    // compile time — the node lowers dynamically).
    let mut span_types: std::collections::HashMap<SpanKey, Type> = std::collections::HashMap::new();
    for (key, ty) in &checker.span_types {
        let rt = checker.unifier.resolve_deep(ty);
        if !inference::contains_var(&rt) {
            span_types.insert(key.clone(), rt);
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

    // Enum payloads are resolved concrete at collection (no inference
    // variables can appear), so `pub` enums export as-is.
    let pub_enums: HashMap<String, EnumSig> = pub_enums_set
        .iter()
        .filter_map(|k| checker.enums.get(k).cloned().map(|v| (k.clone(), v)))
        .collect();

    CheckerOutcome {
        result: CheckResult {
            errors: checker.errors,
            bindings,
            funcs: checker.funcs,
            structs: checker.structs,
            aliases: checker.aliases,
            enums: checker.enums,
            try_resolutions: checker.try_resolutions,
            try_converts,
            link_libs: checker.link_libs,
            const_bindings,
            pub_bindings,
            pub_funcs,
            pub_structs,
            pub_aliases,
            pub_enums,
        },
        span_types,
    }
}

/// Result of the check pass: the public [`CheckResult`] plus the typed AST
/// view (span → resolved type) for codegen.
struct CheckerOutcome {
    result: CheckResult,
    span_types: std::collections::HashMap<SpanKey, Type>,
}

pub(crate) struct Checker {
    pub(crate) unifier: crate::unify::Unifier,
    pub(crate) errors: Vec<zz_frontend::diag::RawDiag>,
    pub(crate) funcs: HashMap<String, FuncSig>,
    pub(crate) structs: HashMap<String, StructSig>,
    /// Type aliases (`type Tokens = [Token]`): exported, resolved targets.
    pub(crate) aliases: HashMap<String, AliasSig>,
    /// Raw alias targets (this program only), converted to `aliases`
    /// after all structs are collected so targets may reference any
    /// struct regardless of order. Not exported.
    pub(crate) alias_asts: HashMap<String, (Vec<String>, Ty)>,
    /// Alias expansion stack for cycle detection (`type A = B`,
    /// `type B = A` reports instead of recursing forever).
    pub(crate) alias_expanding: Vec<String>,
    /// User enums (`enum Token { ... }`): exported, resolved payloads.
    pub(crate) enums: HashMap<String, EnumSig>,
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
    /// Module-head aliases: `m` → `std.math` from `import std.math as m`.
    /// Lets const/call diagnostics resolve aliased namespaces.
    pub(crate) module_aliases: HashMap<String, String>,
    /// Resolved type per scoped expression key, recorded during the type walk.
    /// Used by the HIR builder to attach a resolved `Type` to every AST node.
    /// Keyed by [`SpanKey`] (function + span): bare spans collide across
    /// modules since every file restarts offsets at 0.
    pub(crate) span_types: std::collections::HashMap<SpanKey, Type>,
    /// Native libraries requested via `@link`, in source order, deduped.
    pub(crate) link_libs: Vec<String>,
    /// `std.http` route registrations per server-var root (Phase 2.2 lint).
    pub(crate) http_lint: http_lint::HttpLintState,
    /// Enclosing-item scope stack for [`SpanKey`] recording. Holds the
    /// top-level function (or `Type.method`) whose body is being checked;
    /// empty at top level (keys then use [`TOP_SCOPE`]).
    pub(crate) scope: Vec<Arc<str>>,
}

impl Checker {
    /// Scope-qualified key for an expression span under the item currently
    /// being checked.
    pub(crate) fn scope_key(&self, span: Span) -> SpanKey {
        SpanKey::new(
            self.scope
                .last()
                .cloned()
                .unwrap_or_else(|| Arc::from(TOP_SCOPE)),
            span,
        )
    }
    pub(crate) fn new(
        initial_bindings: HashMap<String, Type>,
        funcs: HashMap<String, FuncSig>,
        structs: HashMap<String, StructSig>,
        aliases: HashMap<String, AliasSig>,
        enums: HashMap<String, EnumSig>,
        initial_consts: HashMap<String, Span>,
    ) -> Self {
        let env = vec![initial_bindings];
        Checker {
            unifier: crate::unify::Unifier::new(),
            errors: Vec::new(),
            funcs,
            structs,
            aliases,
            alias_asts: HashMap::new(),
            alias_expanding: Vec::new(),
            enums,
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
            module_aliases: HashMap::new(),
            span_types: std::collections::HashMap::new(),
            scope: Vec::new(),
            link_libs: Vec::new(),
            http_lint: http_lint::HttpLintState::default(),
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
        impl_generics: &[String],
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
            _ => Type::Struct(
                type_name.to_string(),
                impl_generics
                    .iter()
                    .map(|g| Type::Named(g.clone()))
                    .collect(),
            ),
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

#[cfg(test)]
mod span_scope_tests {
    use super::*;

    // Two separately-parsed "modules" (offsets restart at 0, exactly like
    // the loader merging multi-file programs): the `1234` and `true`
    // initializers share span (28, 32) but have different types. A bare
    // span-keyed map collapses them and native codegen lowers one with
    // the other's type; scoped keys keep both.
    #[test]
    fn same_span_different_functions_keep_types() {
        let a = zz_frontend::parse("func f() -> int {\n    v :=  1234\n    v\n}\n");
        let b = zz_frontend::parse("func g() -> bool {\n    v := true\n    v\n}\n");
        assert!(a.errors.is_empty(), "parse a: {:?}", a.errors);
        assert!(b.errors.is_empty(), "parse b: {:?}", b.errors);
        let mut stmts = Vec::new();
        stmts.extend(a.program.stmts.iter().cloned());
        stmts.extend(b.program.stmts.iter().cloned());
        let merged = Program {
            stmts,
            span: Span::new(0, 0),
        };
        let (res, types) = check_program_typed(
            &merged,
            HashMap::new(),
            HashMap::new(),
            HashMap::new(),
            HashMap::new(),
            HashMap::new(),
        );
        assert!(res.errors.is_empty(), "check: {:?}", res.errors);
        let key = Span::new(28, 32);
        assert_eq!(types.get(&SpanKey::new("f", key)), Some(&Type::Int));
        assert_eq!(types.get(&SpanKey::new("g", key)), Some(&Type::Bool));
    }
}

#[cfg(test)]
mod cache_serde_tests {
    // S1 arena-scale: pub signatures must survive a JSON roundtrip exactly
    // (module check-cache stores these on disk).
    use super::*;
    use crate::type_::Type;

    #[test]
    fn func_sig_roundtrips() {
        let sig = FuncSig {
            generics: vec!["T".to_string()],
            bounds: vec![("T".to_string(), vec![zz_frontend::ast::TraitBound::Display])],
            params: vec![
                ("x".to_string(), Type::Int),
                (
                    "ys".to_string(),
                    Type::Array(Box::new(Type::Option(Box::new(Type::Named(
                        "T".to_string(),
                    ))))),
                ),
            ],
            has_default: vec![false, true],
            ret: Type::Result(Box::new(Type::Named("T".to_string())), Box::new(Type::Str)),
            is_extern: false,
            extern_c_symbol: None,
        };
        let json = serde_json::to_string(&sig).expect("serialize FuncSig");
        let back: FuncSig = serde_json::from_str(&json).expect("deserialize FuncSig");
        assert_eq!(format!("{sig:?}"), format!("{back:?}"));
    }

    #[test]
    fn struct_sig_and_type_roundtrip() {
        let sig = StructSig {
            generics: vec![],
            fields: vec![
                ("x".to_string(), Type::Int),
                (
                    "f".to_string(),
                    Type::Func(vec![Type::Int], Box::new(Type::Bool)),
                ),
            ],
        };
        let json = serde_json::to_string(&sig).expect("serialize StructSig");
        let back: StructSig = serde_json::from_str(&json).expect("deserialize StructSig");
        assert_eq!(format!("{sig:?}"), format!("{back:?}"));
    }
}
