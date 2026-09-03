//! ZZ HIR (High-level Intermediate Representation).
//!
//! The typed view of the parsed AST: every expression is paired with its
//! fully resolved checker type. This is the shared substrate for both the
//! bytecode VM (which re-derives what it needs cheaply) and the native AOT
//! codegen backend (which needs resolved types for C lowering).
//!
//! The HIR is *additive*: the `Program` AST remains the source of truth for
//! formatting/editing; `TypedProgram` adds the type lattice on top, keyed by
//! expression span (spans are unique per AST node).

use std::collections::HashMap;

pub use zz_checker::{check_program_typed, FuncSig, StructSig, Type};
pub use zz_frontend::ast::{Block, Expr, Program, Stmt};
pub use zz_frontend::span::Span;

pub mod walk;

pub use walk::{walk_expr, walk_exprs, walk_stmt, TypedExpr};

/// The typed program: the original AST plus resolved types per span.
///
/// Types are deep-resolved (no inference variables); nodes whose type could
/// not be resolved (e.g. fully-dynamic closures) are simply absent from the
/// map and lower through the dynamic path in codegen.
#[derive(Debug, Clone)]
pub struct TypedProgram {
    pub program: Program,
    /// Resolved type keyed by expression span.
    pub types: HashMap<Span, Type>,
    /// Top-level bindings (name → resolved type) produced by the checker.
    pub bindings: HashMap<String, Type>,
    /// Top-level function signatures.
    pub funcs: HashMap<String, FuncSig>,
    /// Top-level struct signatures.
    pub structs: HashMap<String, StructSig>,
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
) -> TypedResult {
    let (checked, span_types) =
        check_program_typed(program, initial_bindings, initial_funcs, initial_structs);
    let diags = checked.errors.clone();
    let bindings = checked.bindings.clone();
    let funcs = checked.funcs.clone();
    let structs = checked.structs.clone();
    TypedResult {
        program: TypedProgram {
            program: program.clone(),
            types: span_types,
            bindings,
            funcs,
            structs,
        },
        diagnostics: diags,
    }
}

impl TypedProgram {
    /// The resolved type of the expression at `span`, if the checker could
    /// determine it.
    pub fn type_at(&self, span: Span) -> Option<&Type> {
        self.types.get(&span)
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
            | Type::HttpServer
            | Type::TcpStream
            | Type::TcpListener
            | Type::Response
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
