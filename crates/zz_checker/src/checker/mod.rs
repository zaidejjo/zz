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
    check_program_impl(program, initial_bindings, initial_funcs, initial_structs).result
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
    let out = check_program_impl(program, initial_bindings, initial_funcs, initial_structs);
    (out.result, out.span_types)
}

/// Core pass shared by [`check_program`] and [`check_program_typed`].
fn check_program_impl(
    program: &Program,
    initial_bindings: HashMap<String, Type>,
    initial_funcs: HashMap<String, FuncSig>,
    initial_structs: HashMap<String, StructSig>,
) -> CheckerOutcome {
    let mut checker = Checker::new(initial_bindings, initial_funcs, initial_structs);

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
    // Impl methods are registered as `TypeName.method_name` functions with
    // `self` typed as the struct type.
    let mut seen = HashMap::new();
    for stmt in &program.stmts {
        if let Stmt::Impl {
            name,
            methods,
            pub_,
            ..
        } = stmt
        {
            let type_name = name.join(".");
            for method in methods {
                if let Stmt::Func {
                    name: mname,
                    params,
                    ret,
                    generics,
                    ..
                } = method
                {
                    let full_name = format!("{}.{}", type_name, mname.join("."));
                    let gen_names: Vec<String> = generics.iter().map(|g| g.name.clone()).collect();
                    // Build params, replacing `self` with the struct type
                    let sig_params: Vec<(String, Type)> = params
                        .iter()
                        .enumerate()
                        .map(|(i, p)| {
                            let ty = if i == 0 && p.name.name == "self" {
                                Type::Struct(type_name.clone())
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
                    if let Some(prev) = seen.insert(full_name.clone(), method.span()) {
                        checker.errors.push(zz_frontend::diag::error_at(
                            format!("duplicate definition of method `{}`", full_name),
                            method.span(),
                        ));
                        checker.errors.push(zz_frontend::diag::error_at(
                            "previous definition here",
                            prev,
                        ));
                    }
                    checker.funcs.insert(
                        full_name.clone(),
                        crate::checker::FuncSig {
                            generics: gen_names,
                            params: sig_params,
                            has_default,
                            ret: sig_ret,
                        },
                    );
                    if *pub_ {
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

    CheckerOutcome {
        result: CheckResult {
            errors: checker.errors,
            bindings,
            funcs: checker.funcs,
            structs: checker.structs,
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
    pub(crate) env: Vec<HashMap<String, Type>>,
    /// Top-level let bindings discovered this run: name → type.
    pub(crate) new_bindings: HashMap<String, Type>,
    pub(crate) current_ret: Option<Type>,
    pub(crate) current_generics: Vec<String>,
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
    /// Resolved type per expression span, recorded during the type walk.
    /// Used by the HIR builder to attach a resolved `Type` to every AST node.
    pub(crate) span_types: std::collections::HashMap<zz_frontend::span::Span, Type>,
}

impl Checker {
    pub(crate) fn new(
        initial_bindings: HashMap<String, Type>,
        funcs: HashMap<String, FuncSig>,
        structs: HashMap<String, StructSig>,
    ) -> Self {
        let env = vec![initial_bindings];
        Checker {
            unifier: crate::unify::Unifier::new(),
            errors: Vec::new(),
            funcs,
            structs,
            env,
            new_bindings: HashMap::new(),
            current_ret: None,
            current_generics: Vec::new(),
            loop_depth: 0,
            used_names: std::collections::HashSet::new(),
            pub_names: std::collections::HashSet::new(),
            defined_names: vec![HashMap::new()],
            had_undefined_var: false,
            imports: Vec::new(),
            span_types: std::collections::HashMap::new(),
        }
    }

    /// Get the full name of a function statement.
    pub(crate) fn func_name(stmt: &Stmt) -> String {
        match stmt {
            Stmt::Func { name, .. } => name.join("."),
            _ => unreachable!(),
        }
    }
}
