//! ZZ HIR (High-level Intermediate Representation).
//!
//! The typed view of the parsed AST: every expression is paired with its
//! fully resolved checker type. This is the shared substrate for both the
//! bytecode VM (which re-derives what it needs cheaply) and the native AOT
//! codegen backend (which needs resolved types for C lowering).
//!
//! The HIR is *additive*: the `Program` AST remains the source of truth for
//! formatting/editing; `TypedProgram` adds the type lattice on top, keyed by
//! scope-qualified expression key (spans repeat across modules since every
//! file restarts offsets at 0 — see `zz_checker::SpanKey`).

use std::collections::HashMap;

use zz_arena::Symbol;
pub use zz_checker::{
    check_program_typed, AliasSig, EnumSig, FuncSig, SpanKey, StructSig, TraitSig, Type, TOP_SCOPE,
};
pub use zz_frontend::ast::{Block, Expr, Program, Stmt};
pub use zz_frontend::span::Span;

pub mod walk;

pub use walk::{walk_expr, walk_exprs, walk_stmt, TypedExpr};

pub mod callgraph;

pub mod capture;
pub use capture::{captured_in_block, captured_in_expr, captured_in_stmts, closure_free_vars};

pub mod escape;
pub use callgraph::{dce, prune_program, reachable, reachable_from, CallGraph, ReachableSet, TOP};
pub use escape::{analyze as escape_analyze, AllocClass, EscapeResult};

/// The typed program: the original AST plus resolved types per span.
///
/// Types are deep-resolved (no inference variables); nodes whose type could
/// not be resolved (e.g. fully-dynamic closures) are simply absent from the
/// map and lower through the dynamic path in codegen.
#[derive(Debug, Clone)]
pub struct TypedProgram {
    pub program: Program,
    /// Resolved type keyed by scope-qualified expression key.
    pub types: HashMap<SpanKey, Type>,
    /// Top-level bindings (name → resolved type) produced by the checker.
    pub bindings: HashMap<String, Type>,
    /// Top-level function signatures.
    pub funcs: HashMap<String, FuncSig>,
    /// Top-level struct signatures.
    pub structs: HashMap<String, StructSig>,
    /// Top-level user enum signatures (for VM construction detection).
    pub enums: HashMap<String, EnumSig>,
    /// Top-level user trait declarations.
    pub traits: HashMap<String, TraitSig>,
    /// Trait name → implementing type keys (all impls in this program).
    pub trait_impls: HashMap<String, Vec<String>>,
    /// `try` site span → conversion function name (`None` = identity).
    /// Mirrors the checker's `try_converts`; consulted by native codegen
    /// to emit error-conversion calls on early return.
    pub try_converts: HashMap<Span, Option<String>>,
    /// Module namespace → origin: `"project"` for first-party source the
    /// consumer owns, `"pkg"` for an external ZZ package dependency.
    /// Absent namespaces (stdlib bodies, bare `_prelude`) are treated as
    /// stdlib by the codegen's stdlib scan; this map only needs to record
    /// the package-vs-project boundary. Populated by `zz_build` from the
    /// loader's module-origin map and propagated through
    /// [`merge_typed_programs`].
    pub module_origins: HashMap<String, String>,
    /// Reverse map: func name → Symbol id. Enables [`type_at`] to resolve
    /// a func name to its interned `Symbol` for [`SpanKey`] construction.
    pub symbol_map: HashMap<String, u32>,
}

/// Result of building a [`TypedProgram`]: the typed program plus any checker
/// diagnostics (errors/warnings) encountered.
#[derive(Debug, Clone)]
pub struct TypedResult {
    pub program: TypedProgram,
    pub diagnostics: Vec<zz_frontend::diag::RawDiag>,
}

/// Remove seed signatures for `impl <builtin>` methods the program declares
/// itself.
///
/// Pipelines type-check each module once to build the shared `funcs` table,
/// then re-check the combined program with that table as the seed (both the
/// native AOT and bytecode paths do this). Re-seeding the program's own
/// builtin extension methods makes the second pass report them as
/// extension/orphan conflicts with a "previous definition" that is really
/// the same declaration. The loader's authoritative per-module check already
/// rejected genuine collisions, so strip only the exact keys this program
/// re-registers; duplicates *within* the program still trip the checker's
/// `seen` guard, and real stdlib seeds (`std.str.length`) are untouched.
fn strip_own_builtin_impls(program: &Program, funcs: &mut HashMap<String, FuncSig>) {
    for stmt in &program.stmts {
        let Stmt::Impl {
            name,
            methods,
            trait_name,
            ..
        } = stmt
        else {
            continue;
        };
        if trait_name.is_some() {
            continue;
        }
        let Some(type_key) = zz_checker::checker::Checker::builtin_ext_key(&name.join(".")) else {
            continue;
        };
        for method in methods {
            if let Stmt::Func { name: mname, .. } = method {
                funcs.remove(&format!("{}.{}", type_key, mname.join(".")));
            }
        }
    }
}

/// Build a [`TypedProgram`] from parsed source and optional seed maps
/// (for REPL sessions where prior statements' types persist).
#[allow(clippy::too_many_arguments)]
pub fn build_program(
    program: &Program,
    initial_bindings: HashMap<String, Type>,
    mut initial_funcs: HashMap<String, FuncSig>,
    initial_structs: HashMap<String, StructSig>,
    initial_aliases: HashMap<String, AliasSig>,
    initial_enums: HashMap<String, EnumSig>,
    initial_traits: HashMap<String, TraitSig>,
    initial_trait_impls: HashMap<String, Vec<String>>,
) -> TypedResult {
    // Expand decorators before checking so the typed program (consumed by
    // codegen) contains the lowered `__inner` + wrapper functions. The
    // checker re-expands idempotently; diagnostics merge in order.
    let (expanded, mut diags) = zz_frontend::decorators::expand_program(program);
    // Re-checks seed `funcs` with this program's own registrations; drop
    // them so builtin extension methods do not self-conflict.
    strip_own_builtin_impls(&expanded, &mut initial_funcs);
    let (checked, span_types) = check_program_typed(
        &expanded,
        initial_bindings,
        initial_funcs,
        initial_structs,
        initial_aliases,
        initial_enums,
        initial_traits,
        initial_trait_impls,
    );
    // Move (never clone) the result maps: each is freshly built per compile
    // (notably `funcs`, one entry per function) and used exactly once here.
    diags.extend(checked.errors);
    let bindings = checked.bindings;
    let funcs = checked.funcs;
    let structs = checked.structs;
    let enums = checked.enums;
    let traits = checked.traits.clone();
    let trait_impls = checked.trait_impls.clone();
    let try_converts = checked.try_converts;
    TypedResult {
        program: TypedProgram {
            program: expanded,
            types: span_types,
            bindings,
            funcs,
            structs,
            enums,
            traits,
            trait_impls,
            try_converts,
            module_origins: HashMap::new(),
            symbol_map: checked.symbol_map,
        },
        diagnostics: diags,
    }
}

impl TypedProgram {
    /// The resolved type of the expression at `span` within `func`
    /// (top-level function name, `Type.method` for methods,
    /// [`TOP_SCOPE`] otherwise), if the checker could determine it.
    pub fn type_at(&self, func: &str, span: Span) -> Option<&Type> {
        let sym_id = self.symbol_map.get(func)?;
        self.types.get(&SpanKey::new(Symbol(*sym_id), span))
    }

    /// Iterate the top-level statements.
    pub fn stmts(&self) -> &[Stmt] {
        &self.program.stmts
    }

    /// Whether the program has any checker errors (severity Error).
    pub fn has_errors(&self) -> bool {
        // Recompute via diagnostics is wasteful; callers should track this
        // from `TypedResult`. Kept for convenience during construction.
        false
    }
}

/// Convenience: parse + type-check + HIR-build in one call.
///
/// Returns `None` if the source failed to parse with a hard error.
/// Warnings (e.g. redundant semicolon) do not block HIR construction.
pub fn build_source(
    source: &str,
    initial_bindings: HashMap<String, Type>,
    initial_funcs: HashMap<String, FuncSig>,
    initial_structs: HashMap<String, StructSig>,
    initial_aliases: HashMap<String, AliasSig>,
    initial_enums: HashMap<String, EnumSig>,
) -> Option<TypedResult> {
    let parsed = zz_frontend::parse(source);
    if parsed
        .errors
        .iter()
        .any(|d| d.severity == zz_frontend::diag::Severity::Error)
    {
        return None;
    }
    Some(build_program(
        &parsed.program,
        initial_bindings,
        initial_funcs,
        initial_structs,
        initial_aliases,
        initial_enums,
        HashMap::new(),
        HashMap::new(),
    ))
}

/// Merge multiple [`TypedProgram`]s into one without re-type-checking.
///
/// Used to combine pre-compiled stdlib [`TypedProgram`]s (cached via
/// [`zz_stdlib::zz_stdlib_programs`]) with a user [`TypedProgram`] so the
/// checker does not re-process stdlib bodies on every build. Statements are
/// concatenated (stdlib first, mirroring VM load order) and signature maps
/// are merged with later programs taking precedence on key collisions.
/// Span keys never collide across modules because every file restarts span
/// offsets at 0 and `SpanKey` includes the qualifying name.
pub fn merge_typed_programs(programs: Vec<TypedProgram>) -> TypedProgram {
    let mut stmts: Vec<Stmt> = Vec::new();
    let mut types: HashMap<SpanKey, Type> = HashMap::new();
    let mut bindings: HashMap<String, Type> = HashMap::new();
    let mut funcs: HashMap<String, FuncSig> = HashMap::new();
    let mut structs: HashMap<String, StructSig> = HashMap::new();
    let mut enums: HashMap<String, EnumSig> = HashMap::new();
    let mut traits: HashMap<String, TraitSig> = HashMap::new();
    let mut trait_impls: HashMap<String, Vec<String>> = HashMap::new();
    let mut try_converts: HashMap<Span, Option<String>> = HashMap::new();
    let mut module_origins: HashMap<String, String> = HashMap::new();
    let mut span: Span = Span::new(0, 0);
    // Span types are keyed by `(interned scope, span)` and every program's
    // interner restarts at 0, so local ids collide across programs. Re-intern
    // by *scope name*: build a global name→id table and remap each program's
    // keys to the global ids. Without this, `symbol_map` came out empty and
    // every `type_at` lookup in the merged (AOT) program missed — scalar
    // specialization then declared raw `int64_t`/`bool` locals as `zz_value`
    // (`zz_value v1 = (int64_t)(v0 / 2);`) and clang rejected the C.
    //
    // `<top>` is deliberately excluded: every program shares that scope name
    // but their top-level spans overlap (offsets restart per file), so merging
    // it would let one module's type answer for another's. Top-level lowering
    // already falls back safely when a type is unknown; a *wrong* type would
    // miscompile, so drop those keys instead of merging them.
    let mut symbol_map: HashMap<String, u32> = HashMap::new();
    let mut next_symbol: u32 = 0;
    for tp in programs {
        stmts.extend(tp.program.stmts);
        // program-local symbol id → global id (named, non-`<top>` scopes)
        let mut remap: HashMap<u32, u32> = HashMap::new();
        for (name, local_id) in &tp.symbol_map {
            if name == TOP_SCOPE {
                continue;
            }
            let global = *symbol_map.entry(name.clone()).or_insert_with(|| {
                let id = next_symbol;
                next_symbol += 1;
                id
            });
            remap.entry(*local_id).or_insert(global);
        }
        for (key, ty) in tp.types {
            // Unmapped scopes are `<top>` or a scope the checker never
            // registered; drop rather than guess (matches pre-merge
            // behavior for those keys).
            if let Some(&global) = remap.get(&key.func.id()) {
                types.insert(SpanKey::new(Symbol(global), key.span), ty);
            }
        }
        bindings.extend(tp.bindings);
        funcs.extend(tp.funcs);
        structs.extend(tp.structs);
        enums.extend(tp.enums);
        traits.extend(tp.traits);
        for (tname, types) in &tp.trait_impls {
            let entry = trait_impls.entry(tname.clone()).or_default();
            for t in types {
                if !entry.contains(t) {
                    entry.push(t.clone());
                }
            }
        }
        try_converts.extend(tp.try_converts);
        module_origins.extend(tp.module_origins);
        if tp.program.span.end > span.end {
            span = tp.program.span;
        }
    }
    TypedProgram {
        program: Program { stmts, span },
        types,
        bindings,
        funcs,
        structs,
        enums,
        traits,
        trait_impls,
        try_converts,
        module_origins,
        symbol_map,
    }
}

/// Does the resolved type require the dynamic (`zz_value`) codegen fallback?
/// True for types that cannot lower to a static C type: unions, opaque
/// handles (json/http/server/streams), dictionaries, and functions.
pub fn is_dynamic(ty: &Type) -> bool {
    matches!(
        ty,
        Type::Union(_)
            | Type::Json
            | Type::Db
            | Type::HttpServer
            | Type::TcpStream
            | Type::TcpListener
            | Type::Response
            | Type::HttpRequest
            | Type::Opaque(_)
            | Type::Dict(_, _)
            | Type::Func(_, _)
    )
}

/// Whether the type is a plain integer (fast C `int64_t` lowering).
pub fn is_int(ty: &Type) -> bool {
    matches!(ty, Type::Int)
}

#[cfg(test)]
mod tests;
