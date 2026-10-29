//! Type checker: HM-lite inference, generics, patterns, exhaustiveness.

pub mod http_lint;
pub mod std_gate;
pub mod type_check;
pub mod value_lint;

// State and shared helper types live in `zz_checker_core`.
pub use zz_checker_core::*;

use std::collections::HashMap;
use std::sync::Arc;
use zz_scope::ScopeOps;

use zz_span_ast::ast::{Program, Stmt};
use zz_span_ast::span::Span;

use crate::checker::type_check::TypeCheckOps;
use zz_infer::InferOps;
use zz_sigs::funcs::FuncOps;
use zz_structs::StructOps;
use zz_types::Type;

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
        "",
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
        "",
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
        "",
    );
    (out.result, out.span_types)
}

/// Like [`check_program`], but names the module under check (`fen` for
/// `fen.zz`) so bindings shadowing the module's own namespace (or an
/// imported one) warn instead of failing only at runtime (#376, #361).
/// The loader passes each module's namespace; merged programs and REPL
/// snippets pass `""` (no single owner).
pub fn check_program_with_module_ns(
    program: &Program,
    initial_bindings: HashMap<String, Type>,
    initial_funcs: HashMap<String, FuncSig>,
    initial_structs: HashMap<String, StructSig>,
    initial_aliases: HashMap<String, AliasSig>,
    initial_enums: HashMap<String, EnumSig>,
    module_ns: &str,
) -> CheckResult {
    check_program_impl(
        program,
        initial_bindings,
        initial_funcs,
        initial_structs,
        initial_aliases,
        initial_enums,
        HashMap::new(),
        module_ns,
    )
    .result
}

/// Core pass shared by [`check_program`], [`check_program_with_consts`],
/// [`check_program_with_module_ns`], and [`check_program_typed`].
// Eight params is the honest shape here (six seed maps + consts +
// module owner); a struct would churn every call site for no benefit.
#[allow(clippy::too_many_arguments)]
fn check_program_impl(
    program: &Program,
    initial_bindings: HashMap<String, Type>,
    initial_funcs: HashMap<String, FuncSig>,
    initial_structs: HashMap<String, StructSig>,
    initial_aliases: HashMap<String, AliasSig>,
    initial_enums: HashMap<String, EnumSig>,
    initial_consts: HashMap<String, Span>,
    module_ns: &str,
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
            if let Some(id) = zz_infer::max_var_id(t) {
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
    // Import hygiene for fully-qualified std paths (#227): only enforced
    // for loader-checked user modules (non-empty owner). Stdlib internals,
    // the REPL, and merged HIR programs pass "" and skip it.
    if !module_ns.is_empty() {
        checker
            .errors
            .extend(std_gate::lint_std_imports(program, module_ns));
    }

    // Track which items are pub (for cross-module export).
    let mut pub_bindings_set: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut pub_funcs_set: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut pub_structs_set: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut pub_aliases_set: std::collections::HashSet<String> = std::collections::HashSet::new();

    // Pass 0: pre-register struct/enum names with empty signatures so
    // fields and payloads may reference their own type (or each other,
    // in either order). Direct self-embedding (`kid: Node` — no array,
    // option, or other indirection) stays an error, reported when the
    // real signature lands in Pass 1/1b. The placeholders are always
    // overwritten below; a duplicate definition still errors there.
    for stmt in &program.stmts {
        match stmt {
            Stmt::Struct { name, generics, .. } => {
                checker.structs.entry(name.join(".")).or_insert(StructSig {
                    generics: generics.iter().map(|g| g.name.clone()).collect(),
                    fields: Vec::new(),
                });
            }
            Stmt::Enum { name, generics, .. } => {
                checker.enums.entry(name.join(".")).or_insert(EnumSig {
                    generics: generics.iter().map(|g| g.name.clone()).collect(),
                    variants: Vec::new(),
                });
            }
            _ => {}
        }
    }

    // Pass 1a: register type aliases. Raw targets are recorded first so
    // aliases may reference structs regardless of order; conversion to
    // resolved types happens after (nested aliases expand recursively
    // with cycle detection in `convert_alias`). Registered before struct
    // collection so struct fields can reference aliases (e.g. `run: F`
    // where `type F = func(int) -> int`).
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
                checker.errors.push(zz_diag_core::error_at(
                    format!(
                        "duplicate definition of `{}` (a struct with the same name exists)",
                        full_name
                    ),
                    *span,
                ));
            }
            if let Some(prev) = seen_aliases.insert(full_name.clone(), *span) {
                checker.errors.push(zz_diag_core::error_at(
                    format!("duplicate definition of type alias `{}`", full_name),
                    *span,
                ));
                checker
                    .errors
                    .push(zz_diag_core::error_at("previous definition here", prev));
            }
            // Reject duplicate type parameters (`type P[T, T] = ...`).
            let mut seen = std::collections::HashSet::new();
            let mut gen_names = Vec::new();
            for g in generics {
                if !seen.insert(g.name.clone()) {
                    checker.errors.push(zz_diag_core::error_at(
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

    // Pass 1: register struct definitions (fields are resolved against the
    // struct registry, so structs may reference earlier structs). Structs
    // must be registered before functions so `func f(p: Point)` resolves.
    // Aliases are already registered (Pass 1a) so struct fields may
    // reference alias types.
    let mut seen_structs = HashMap::new();
    for stmt in &program.stmts {
        if let Stmt::Struct {
            name, span, pub_, ..
        } = stmt
        {
            let full_name = name.join(".");
            if let Some(prev) = seen_structs.insert(full_name.clone(), *span) {
                checker.errors.push(zz_diag_core::error_at(
                    format!("duplicate definition of struct `{}`", full_name),
                    *span,
                ));
                checker
                    .errors
                    .push(zz_diag_core::error_at("previous definition here", prev));
            }
            checker.collect_struct(stmt);
            if *pub_ {
                pub_structs_set.insert(full_name);
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
                checker.errors.push(zz_diag_core::error_at(
                    format!(
                        "duplicate definition of `{}` (a struct with the same name exists)",
                        full_name
                    ),
                    *span,
                ));
            }
            if checker.alias_asts.contains_key(&full_name) {
                checker.errors.push(zz_diag_core::error_at(
                    format!(
                        "duplicate definition of `{}` (a type alias with the same name exists)",
                        full_name
                    ),
                    *span,
                ));
            }
            if let Some(prev) = seen_enums.insert(full_name.clone(), *span) {
                checker.errors.push(zz_diag_core::error_at(
                    format!("duplicate definition of enum `{}`", full_name),
                    *span,
                ));
                checker
                    .errors
                    .push(zz_diag_core::error_at("previous definition here", prev));
            }
            // Reject duplicate type parameters (`enum P[T, T]`).
            let mut seen_params = std::collections::HashSet::new();
            let mut gen_names = Vec::new();
            for g in generics {
                if !seen_params.insert(g.name.clone()) {
                    checker.errors.push(zz_diag_core::error_at(
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
    let mut alias_names: Vec<String> = checker.alias_asts.keys().cloned().collect();
    // Deterministic order: HashMap iteration is random per process, and
    // whichever alias converts first names the cycle (`A -> B` vs
    // `B -> A`). Sorting pins one spelling across runs, processes, and
    // consumers (CLI, LSP, build cache) — the parity gate (#264) caught
    // this flapping between runs.
    alias_names.sort();
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
            // Generic structs need a matching generic impl (`impl Box<T>`);
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
                    checker.errors.push(zz_diag_core::error_at(
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
                    checker.errors.push(zz_diag_core::error_at(
                        format!(
                            "{kind} `{type_name}` is not generic (expected `impl {type_name}` without type parameters)"
                        ),
                        stmt.span(),
                    ));
                } else {
                    checker.errors.push(zz_diag_core::error_at(
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
                // Duplicate parameter names (`impl Box<T, T>`) shadow each
                // other in substitution maps; reject rather than guess.
                let mut seen_gen = std::collections::HashSet::new();
                for g in &impl_gen_names {
                    if !seen_gen.insert(g.clone()) {
                        checker.errors.push(zz_diag_core::error_at(
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
                    // receiver's `Box<int>` unifies `T := int` at call
                    // sites through the existing instantiation path).
                    // A method parameter shadowing an impl parameter
                    // would collapse two distinct variables; reject it.
                    for g in generics {
                        if impl_gen_names.contains(&g.name.name) {
                            checker.errors.push(zz_diag_core::error_at(
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
                    let gen_bounds: Vec<(String, Vec<zz_span_ast::ast::TraitBound>)> = generics
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
                            checker.errors.push(zz_diag_core::error_at(
                                "`convert_to_` must name a target type (e.g. `convert_to_MyErr`)",
                                method.span(),
                            ));
                        } else if matches!(sig_ret, Type::Var(_)) {
                            checker.errors.push(zz_diag_core::error_at(
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
                            checker.errors.push(zz_diag_core::error_at(
                                format!(
                                    "ambiguous conversion from `{}`: already converts to `{}`",
                                    type_key, prev.to_name
                                ),
                                method.span(),
                            ));
                            checker.errors.push(zz_diag_core::error_at(
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
                        checker.errors.push(zz_diag_core::error_at(
                            format!("duplicate definition of method `{}`", full_name),
                            method.span(),
                        ));
                        checker
                            .errors
                            .push(zz_diag_core::error_at("previous definition here", prev));
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
                        // Seeds are canonical (`std.*` only), so check the
                        // canonical spelling too (`str.length` collides with
                        // `std.str.length`).
                        if checker.funcs.contains_key(&full_name)
                            || canonical_std_name(&full_name)
                                .is_some_and(|c| checker.funcs.contains_key(&c))
                        {
                            let origin = if Checker::is_stdlib_method(&full_name) {
                                "defined in the standard library"
                            } else {
                                "already defined in another module (orphan rule: same (Type, method) in two modules)"
                            };
                            checker.errors.push(zz_diag_core::error_at(
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
                            checker.errors.push(zz_diag_core::error_at(
                                format!("duplicate extension method `{}` (orphan rule)", full_name),
                                method.span(),
                            ));
                            checker.errors.push(zz_diag_core::error_at(
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
                checker.errors.push(zz_diag_core::error_at(
                    format!("duplicate definition of function `{}`", full_name),
                    *span,
                ));
                checker
                    .errors
                    .push(zz_diag_core::error_at("previous definition here", prev));
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
                    checker.errors.push(zz_diag_core::error_at(
                        format!("duplicate definition of function `{}`", item.name.name),
                        item.span,
                    ));
                    checker
                        .errors
                        .push(zz_diag_core::error_at("previous definition here", prev));
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
        if !zz_infer::contains_var(&rt) {
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
    //
    // Variant constructors are the exception: default their unconstrained
    // payload parameters to `unit` first, the same rule that validates
    // annotated declarations (`.ok(42)` is `Result<int, unit>`, `.none` is
    // `Option<unit>`). The outer constructor is what method dispatch needs —
    // `.ok(42).unwrap_or(0)` has to resolve `result.unwrap_or`, not the
    // same-named `option` twin — and dropping the entry because the *payload*
    // position was unconstrained lost exactly that: the AOT fell back to its
    // arity-based namespace guess (`option` precedes `result`) and called
    // `option.unwrap_or` on a `Result` value, while the VM dispatched on the
    // value and printed the answer (#505). Variant payloads erase to
    // `zz_value`, so defaulting matches the runtime representation. Arrays and
    // dicts keep the drop: their element types select the unboxed
    // representation, which cannot be guessed.
    let mut span_types: std::collections::HashMap<SpanKey, Type> = std::collections::HashMap::new();
    for (key, ty) in &checker.span_types {
        let mut rt = checker.unifier.resolve_deep(ty);
        if zz_infer::contains_var(&rt)
            && matches!(rt, Type::Option(_) | Type::Result(_, _) | Type::Enum(_, _))
        {
            zz_infer::default_variant_vars(&mut rt);
        }
        if !zz_infer::contains_var(&rt) {
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
        let a = zz_frontend::parse("func f() -> int {\n    v :=  1234\n    return v\n}\n");
        let b = zz_frontend::parse("func g() -> bool {\n    v := true\n    return v\n}\n");
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
    use zz_types::Type;

    #[test]
    fn func_sig_roundtrips() {
        let sig = FuncSig {
            generics: vec!["T".to_string()],
            bounds: vec![("T".to_string(), vec![zz_span_ast::ast::TraitBound::Display])],
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

/// Regression tests for #652: the checker must never `unreachable!()`/panic
/// when a statement variant reaches a collector/checker that expects a
/// different variant. These paths are defensive ("should never happen") but
/// must degrade to a recorded `RawDiag` instead of aborting the compiler.
#[cfg(test)]
mod unreachable_guard_tests {
    use super::*;
    use zz_frontend::parse;

    fn checker() -> Checker {
        Checker::new(
            HashMap::new(),
            HashMap::new(),
            HashMap::new(),
            HashMap::new(),
            HashMap::new(),
            HashMap::new(),
        )
    }

    fn first_stmt(src: &str) -> Stmt {
        let parsed = parse(src);
        assert!(
            parsed.errors.is_empty(),
            "parse errors: {:?}",
            parsed.errors
        );
        parsed
            .program
            .stmts
            .into_iter()
            .next()
            .expect("source should contain a statement")
    }

    #[test]
    fn func_name_returns_unknown_for_non_func() {
        let stmt = first_stmt("struct Point { x: int }");
        assert_eq!(Checker::func_name(&stmt), "<unknown>");
    }

    #[test]
    fn collect_func_non_func_records_error_instead_of_panicking() {
        let stmt = first_stmt("struct Point { x: int }");
        let mut c = checker();
        c.collect_func(&stmt);
        assert!(
            !c.errors.is_empty(),
            "expected collect_func to record an error, got none"
        );
        assert!(
            c.errors.iter().any(|e| e.message.contains("collect_func")),
            "expected a collect_func error, got: {:?}",
            c.errors
        );
    }

    #[test]
    fn check_func_body_non_func_records_error_instead_of_panicking() {
        let stmt = first_stmt("struct Point { x: int }");
        let mut c = checker();
        let sig = FuncSig {
            generics: vec![],
            bounds: vec![],
            params: vec![],
            has_default: vec![],
            ret: Type::Unit,
            is_extern: false,
            extern_c_symbol: None,
        };
        c.check_func_body(&stmt, &sig);
        assert!(
            !c.errors.is_empty(),
            "expected check_func_body to record an error, got none"
        );
        assert!(
            c.errors
                .iter()
                .any(|e| e.message.contains("check_func_body")),
            "expected a check_func_body error, got: {:?}",
            c.errors
        );
    }

    #[test]
    fn collect_struct_non_struct_records_error_instead_of_panicking() {
        let stmt = first_stmt("func f() { }");
        let mut c = checker();
        c.collect_struct(&stmt);
        assert!(
            !c.errors.is_empty(),
            "expected collect_struct to record an error, got none"
        );
        assert!(
            c.errors
                .iter()
                .any(|e| e.message.contains("collect_struct")),
            "expected a collect_struct error, got: {:?}",
            c.errors
        );
    }
}
