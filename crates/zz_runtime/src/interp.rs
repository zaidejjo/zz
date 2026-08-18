//! Phase 0 tree-walker interpreter.
//!
//! Evaluates the AST directly. Slow by design — this exists to bootstrap the
//! frontend and power the REPL until the bytecode VM lands.

use zz_frontend::ast::{BinOp, Expr, Program, Stmt, UnOp};
use zz_frontend::span::Span;

use crate::env::Env;
use crate::value::Value;

#[derive(Debug)]
pub struct EvalError {
    pub message: String,
    pub span: Span,
}

impl EvalError {
    fn new(message: impl Into<String>, span: Span) -> Self {
        EvalError {
            message: message.into(),
            span,
        }
    }
}

pub struct Interp {
    pub env: Env,
}

impl Default for Interp {
    fn default() -> Self {
        Self::new()
    }
}

impl Interp {
    pub fn new() -> Self {
        Interp { env: Env::new() }
    }

    /// Run a program, returning the value of the last statement.
    pub fn run(&mut self, program: &Program) -> Result<Value, EvalError> {
        let mut result = Value::Unit;
        for stmt in &program.stmts {
            result = self.run_stmt(stmt)?;
        }
        Ok(result)
    }

    pub fn run_stmt(&mut self, stmt: &Stmt) -> Result<Value, EvalError> {
        match stmt {
            Stmt::Let { name, value, span } => {
                let v = self.eval(value)?;
                self.env.define(&name.name, v.clone());
                let _ = span;
                Ok(v)
            }
            Stmt::Expr(e) => self.eval(e),
        }
    }

    pub fn eval(&mut self, expr: &Expr) -> Result<Value, EvalError> {
        match expr {
            Expr::Int { value, .. } => Ok(Value::Int(*value)),
            Expr::Float { value, .. } => Ok(Value::Float(*value)),
            Expr::Ident { name, span } => self
                .env
                .get(name)
                .ok_or_else(|| EvalError::new(format!("undefined variable `{name}`"), *span)),
            Expr::Paren { expr, .. } => self.eval(expr),
            Expr::Unary { op, expr, span } => {
                let v = self.eval(expr)?;
                self.eval_unary(*op, v, *span)
            }
            Expr::Binary {
                op,
                left,
                right,
                span,
            } => {
                let l = self.eval(left)?;
                let r = self.eval(right)?;
                self.eval_binary(*op, l, r, *span)
            }
        }
    }

    fn eval_unary(&mut self, op: UnOp, v: Value, span: Span) -> Result<Value, EvalError> {
        match op {
            UnOp::Pos => Ok(v),
            UnOp::Neg => match v {
                Value::Int(i) => i
                    .checked_neg()
                    .map(Value::Int)
                    .ok_or_else(|| EvalError::new("integer overflow in negation", span)),
                Value::Float(f) => Ok(Value::Float(-f)),
                Value::Unit => Err(EvalError::new("cannot negate unit", span)),
            },
        }
    }

    fn eval_binary(
        &mut self,
        op: BinOp,
        l: Value,
        r: Value,
        span: Span,
    ) -> Result<Value, EvalError> {
        // Mixed int/float arithmetic promotes to float.
        match (l, r) {
            (Value::Int(a), Value::Int(b)) => self.eval_int_binary(op, a, b, span),
            (l, r) => {
                let (a, b) = match (l.to_float(), r.to_float()) {
                    (Some(a), Some(b)) => (a, b),
                    _ => return Err(EvalError::new("arithmetic on non-numeric value", span)),
                };
                let result = match op {
                    BinOp::Add => a + b,
                    BinOp::Sub => a - b,
                    BinOp::Mul => a * b,
                    BinOp::Div => a / b,
                    BinOp::Rem => a % b,
                };
                Ok(Value::Float(result))
            }
        }
    }

    fn eval_int_binary(&self, op: BinOp, a: i64, b: i64, span: Span) -> Result<Value, EvalError> {
        match op {
            BinOp::Add => a
                .checked_add(b)
                .map(Value::Int)
                .ok_or_else(|| EvalError::new("integer overflow in addition", span)),
            BinOp::Sub => a
                .checked_sub(b)
                .map(Value::Int)
                .ok_or_else(|| EvalError::new("integer overflow in subtraction", span)),
            BinOp::Mul => a
                .checked_mul(b)
                .map(Value::Int)
                .ok_or_else(|| EvalError::new("integer overflow in multiplication", span)),
            BinOp::Div => {
                if b == 0 {
                    Err(EvalError::new("division by zero", span))
                } else {
                    a.checked_div(b)
                        .map(Value::Int)
                        .ok_or_else(|| EvalError::new("integer overflow in division", span))
                }
            }
            BinOp::Rem => {
                if b == 0 {
                    Err(EvalError::new("modulo by zero", span))
                } else {
                    a.checked_rem(b)
                        .map(Value::Int)
                        .ok_or_else(|| EvalError::new("integer overflow in modulo", span))
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use zz_frontend::parse;

    fn eval_src(src: &str) -> Result<Value, EvalError> {
        let parsed = parse(src);
        assert!(
            parsed.errors.is_empty(),
            "parse errors: {:?}",
            parsed.errors
        );
        let mut interp = Interp::new();
        interp.run(&parsed.program)
    }

    #[test]
    fn arithmetic_and_precedence() {
        assert_eq!(eval_src("1 + 2 * 3").unwrap(), Value::Int(7));
        assert_eq!(eval_src("(1 + 2) * 3").unwrap(), Value::Int(9));
        assert_eq!(eval_src("10 / 3").unwrap(), Value::Int(3));
        assert_eq!(eval_src("10 % 3").unwrap(), Value::Int(1));
        assert_eq!(eval_src("-5 + 2").unwrap(), Value::Int(-3));
    }

    #[test]
    fn let_binding_evaluates_to_value() {
        assert_eq!(eval_src("let x = 1 + 2").unwrap(), Value::Int(3));
    }

    #[test]
    fn let_references_previous_bindings() {
        assert_eq!(
            eval_src("let a = 10\nlet b = 20\nlet c = a + b\nc").unwrap(),
            Value::Int(30)
        );
    }

    #[test]
    fn shadowing() {
        assert_eq!(
            eval_src("let x = 1\nlet x = x + 1\nx").unwrap(),
            Value::Int(2)
        );
    }

    #[test]
    fn mixed_int_float_promotes() {
        assert_eq!(eval_src("1 + 2.5").unwrap(), Value::Float(3.5));
    }

    #[test]
    fn division_by_zero_errors() {
        let err = eval_src("1 / 0").unwrap_err();
        assert_eq!(err.message, "division by zero");
    }

    #[test]
    fn undefined_variable_errors() {
        let err = eval_src("nope + 1").unwrap_err();
        assert_eq!(err.message, "undefined variable `nope`");
    }

    #[test]
    fn integer_overflow_errors() {
        let err = eval_src("9223372036854775807 + 1").unwrap_err();
        assert_eq!(err.message, "integer overflow in addition");
    }

    #[test]
    fn empty_program_is_unit() {
        assert_eq!(eval_src("").unwrap(), Value::Unit);
    }
}
