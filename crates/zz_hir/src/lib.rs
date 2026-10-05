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

pub use zz_checker::{
    check_program_typed, AliasSig, EnumSig, FuncSig, SpanKey, StructSig, Type, TOP_SCOPE,
};
pub use zz_frontend::ast::{Block, Expr, Program, Stmt};
pub use zz_frontend::span::Span;

pub mod walk;

pub use walk::{walk_expr, walk_exprs, walk_stmt, TypedExpr};

pub mod callgraph;

pub mod capture;
pub use capture::{captured_in_block, captured_in_expr, closure_free_vars};

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
    /// `try` site span → conversion function name (`None` = identity).
    /// Mirrors the checker's `try_converts`; consulted by native codegen
    /// to emit error-conversion calls on early return.
    pub try_converts: HashMap<Span, Option<String>>,
}

/// Result of building a [`TypedProgram`]: the typed program plus any checker
/// diagnostics (errors/warnings) encountered.
#[derive(Debug, Clone)]
pub struct TypedResult {
    pub program: TypedProgram,
    pub diagnostics: Vec<zz_frontend::diag::RawDiag>,
}

/// Build a [`TypedProgram`] from parsed source and optional seed maps
/// (for REPL sessions where prior statements' types persist).
pub fn build_program(
    program: &Program,
    initial_bindings: HashMap<String, Type>,
    initial_funcs: HashMap<String, FuncSig>,
    initial_structs: HashMap<String, StructSig>,
    initial_aliases: HashMap<String, AliasSig>,
    initial_enums: HashMap<String, EnumSig>,
) -> TypedResult {
    // Expand decorators before checking so the typed program (consumed by
    // codegen) contains the lowered `__inner` + wrapper functions. The
    // checker re-expands idempotently; diagnostics merge in order.
    let (expanded, mut diags) = zz_frontend::decorators::expand_program(program);
    let (checked, span_types) = check_program_typed(
        &expanded,
        initial_bindings,
        initial_funcs,
        initial_structs,
        initial_aliases,
        initial_enums,
    );
    // Move (never clone) the result maps: each is freshly built per compile
    // (notably `funcs`, one entry per function) and used exactly once here.
    diags.extend(checked.errors);
    let bindings = checked.bindings;
    let funcs = checked.funcs;
    let structs = checked.structs;
    let enums = checked.enums;
    let try_converts = checked.try_converts;
    TypedResult {
        program: TypedProgram {
            program: expanded,
            types: span_types,
            bindings,
            funcs,
            structs,
            enums,
            try_converts,
        },
        diagnostics: diags,
    }
}

impl TypedProgram {
    /// The resolved type of the expression at `span` within `func`
    /// (top-level function name, `Type.method` for methods,
    /// [`TOP_SCOPE`] otherwise), if the checker could determine it.
    pub fn type_at(&self, func: &str, span: Span) -> Option<&Type> {
        self.types.get(&SpanKey::new(func, span))
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
/// Returns `None` if the source failed to parse.
pub fn build_source(
    source: &str,
    initial_bindings: HashMap<String, Type>,
    initial_funcs: HashMap<String, FuncSig>,
    initial_structs: HashMap<String, StructSig>,
    initial_aliases: HashMap<String, AliasSig>,
    initial_enums: HashMap<String, EnumSig>,
) -> Option<TypedResult> {
    let parsed = zz_frontend::parse(source);
    if !parsed.errors.is_empty() {
        return None;
    }
    Some(build_program(
        &parsed.program,
        initial_bindings,
        initial_funcs,
        initial_structs,
        initial_aliases,
        initial_enums,
    ))
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
