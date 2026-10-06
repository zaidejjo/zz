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

    /// Sherlock: returning an unwrapped `Result` where a plain value is
    /// expected (or vice versa) is the most common ZZ beginner error.
    /// Point at the value with the three actionable fixes (#245).
    /// Returns true when the hint was emitted (caller skips the generic
    /// return-type-span error to avoid a double report).
    pub(crate) fn report_result_return_hint(
        &mut self,
        value_ty: &Type,
        ret_ty: &Type,
        span: Span,
    ) -> bool {
        let v = self.unifier.resolve(value_ty);
        let r = self.unifier.resolve(ret_ty);
        // Unwrapped Result where a plain value is expected.
        if let Type::Result(inner, err) = &v {
            let payload = self.unifier.resolve(inner);
            if payload == r {
                let mut diag = error_at(
                    format!(
                        "returning unwrapped `Result<{payload}, {err}>` where `{r}` is expected"
                    ),
                    span,
                );
                diag = diag.with_note(format!(
                    "use `?` to propagate (needs `-> Result<{payload}, {err}>`)"
                ));
                diag = diag.with_note("or `match` on the value".to_string());
                diag = diag.with_note(format!(
                    "or change the return type to `Result<{payload}, {err}>`"
                ));
                self.errors.push(diag);
                return true;
            }
        }
        // Plain value where a Result is expected.
        if let Type::Result(inner, err) = &r {
            let payload = self.unifier.resolve(inner);
            if payload == v {
                let mut diag = error_at(
                    format!("returning plain `{v}` where `Result<{payload}, {err}>` is expected"),
                    span,
                );
                diag = diag.with_note("wrap the value: `.ok(value)`".to_string());
                diag = diag.with_note("or propagate a fallible call with `?`".to_string());
                self.errors.push(diag);
                return true;
            }
        }
        false
    }

    pub(crate) fn ensure_bool(&mut self, t: Type, span: Span) {
        match self.unifier.resolve(&t) {
            Type::Bool => {}
            Type::Var(id) => {
                self.unifier.bind(id, Type::Bool);
            }
            // Divergent condition: unreachable, vacuously fine.
            Type::Never => {}
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
            // Divergent index: unreachable, vacuously fine.
            Type::Never => {}
            other => {
                self.errors.push(error_at(
                    format!("index must be `int`, found `{other}`"),
                    span,
                ));
            }
        }
    }
}
