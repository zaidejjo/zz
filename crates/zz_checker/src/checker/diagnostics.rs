//! Diagnostic helpers for the type checker.

use crate::checker::Checker;
use crate::type_::Type;
use crate::unify::UnifyError;
use zz_frontend::diag::error_at;
use zz_frontend::span::Span;

impl Checker {
    // --- errors -----------------------------------------------------------

    pub(crate) fn report_mismatch(&mut self, err: UnifyError, span: Span) {
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

    pub(crate) fn ensure_bool(&mut self, t: Type, span: Span) {
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

    pub(crate) fn ensure_int(&mut self, t: Type, span: Span) {
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
