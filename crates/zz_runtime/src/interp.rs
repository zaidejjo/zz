//! Phase 1 tree-walker interpreter.
//!
//! Evaluates the AST directly. Slow by design — this exists to bootstrap the
//! frontend and power the REPL until the bytecode VM lands.

use std::collections::HashMap;

use zz_frontend::ast::{BinOp, Block, Expr, Lit, Pattern, Program, Stmt, UnOp};
use zz_frontend::span::Span;

use crate::env::Env;
use crate::value::{FuncValue, Value};

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

/// Result of evaluating an expression or statement. `Return` unwinds the
/// call stack until the enclosing function call catches it.
#[derive(Debug)]
pub(crate) enum Flow {
    Value(Value),
    Return(Value),
}

impl Flow {
    fn into_value(self) -> Result<Value, EvalError> {
        match self {
            Flow::Value(v) => Ok(v),
            Flow::Return(_) => Err(EvalError::new(
                "`return` outside of a function",
                Span::new(0, 0),
            )),
        }
    }
}

pub struct Interp {
    pub env: Env,
    /// Named functions, kept separate from the environment so recursive
    /// bodies can resolve their own name without circular captured envs.
    pub funcs: HashMap<String, FuncValue>,
}

impl Default for Interp {
    fn default() -> Self {
        Self::new()
    }
}

impl Interp {
    pub fn new() -> Self {
        Interp {
            env: Env::new(),
            funcs: HashMap::new(),
        }
    }

    /// Run a program, returning the value of the last statement.
    pub fn run(&mut self, program: &Program) -> Result<Value, EvalError> {
        let mut result = Value::Unit;
        for stmt in &program.stmts {
            result = self.run_stmt(stmt)?.into_value()?;
        }
        Ok(result)
    }

    pub(crate) fn run_stmt(&mut self, stmt: &Stmt) -> Result<Flow, EvalError> {
        match stmt {
            Stmt::Let { name, value, .. } => match self.eval(value)? {
                Flow::Value(v) => {
                    self.env.define(&name.name, v.clone());
                    Ok(Flow::Value(v))
                }
                Flow::Return(v) => Ok(Flow::Return(v)),
            },
            Stmt::Func {
                name, params, body, ..
            } => {
                let fv = FuncValue {
                    params: params.clone(),
                    body: Expr::Block(body.clone()),
                    env: self.env.clone(),
                };
                self.funcs.insert(name.name.clone(), fv.clone());
                self.env.define(&name.name, Value::Func(fv));
                Ok(Flow::Value(Value::Unit))
            }
            Stmt::Return { value, .. } => match value {
                Some(e) => match self.eval(e)? {
                    Flow::Value(v) => Ok(Flow::Return(v)),
                    Flow::Return(v) => Ok(Flow::Return(v)),
                },
                None => Ok(Flow::Return(Value::Unit)),
            },
            Stmt::Expr(e) => self.eval(e),
        }
    }

    pub(crate) fn eval(&mut self, expr: &Expr) -> Result<Flow, EvalError> {
        match expr {
            Expr::Int { value, .. } => Ok(Flow::Value(Value::Int(*value))),
            Expr::Float { value, .. } => Ok(Flow::Value(Value::Float(*value))),
            Expr::Str { value, .. } => Ok(Flow::Value(Value::Str(value.clone()))),
            Expr::Bool { value, .. } => Ok(Flow::Value(Value::Bool(*value))),
            Expr::Ident { name, span } => {
                if let Some(v) = self.env.get(name) {
                    return Ok(Flow::Value(v));
                }
                if let Some(fv) = self.funcs.get(name) {
                    return Ok(Flow::Value(Value::Func(fv.clone())));
                }
                Err(EvalError::new(
                    format!("undefined variable `{name}`"),
                    *span,
                ))
            }
            Expr::Paren { expr, .. } => self.eval(expr),
            Expr::Unary { op, expr, span } => {
                let v = self.eval(expr)?.into_value()?;
                self.eval_unary(*op, v, *span).map(Flow::Value)
            }
            Expr::Binary {
                op,
                left,
                right,
                span,
            } => {
                // Short-circuit && and ||.
                match op {
                    BinOp::And => {
                        let l = self.eval(left)?.into_value()?;
                        if !l.is_truthy() {
                            return Ok(Flow::Value(Value::Bool(false)));
                        }
                        let r = self.eval(right)?.into_value()?;
                        return Ok(Flow::Value(Value::Bool(r.is_truthy())));
                    }
                    BinOp::Or => {
                        let l = self.eval(left)?.into_value()?;
                        if l.is_truthy() {
                            return Ok(Flow::Value(Value::Bool(true)));
                        }
                        let r = self.eval(right)?.into_value()?;
                        return Ok(Flow::Value(Value::Bool(r.is_truthy())));
                    }
                    _ => {}
                }
                let l = self.eval(left)?.into_value()?;
                let r = self.eval(right)?.into_value()?;
                self.eval_binary(*op, l, r, *span).map(Flow::Value)
            }
            Expr::Call { callee, args, span } => {
                let f = self.eval(callee)?.into_value()?;
                let mut arg_vals = Vec::with_capacity(args.len());
                for a in args {
                    arg_vals.push(self.eval(a)?.into_value()?);
                }
                self.call(f, arg_vals, *span).map(Flow::Value)
            }
            Expr::Closure { params, body, .. } => Ok(Flow::Value(Value::Func(FuncValue {
                params: params.clone(),
                body: (**body).clone(),
                env: self.env.clone(),
            }))),
            Expr::If {
                cond,
                then,
                els,
                span,
            } => {
                let c = self.eval(cond)?.into_value()?;
                if !matches!(c, Value::Bool(_)) {
                    return Err(EvalError::new("`if` condition must be a bool", *span));
                }
                if c.is_truthy() {
                    self.eval_block(then)
                } else {
                    match els {
                        Some(e) => self.eval(e),
                        None => Ok(Flow::Value(Value::Unit)),
                    }
                }
            }
            Expr::While { cond, body, span } => {
                let mut result = Value::Unit;
                loop {
                    let c = self.eval(cond)?.into_value()?;
                    if !matches!(c, Value::Bool(_)) {
                        return Err(EvalError::new("`while` condition must be a bool", *span));
                    }
                    if !c.is_truthy() {
                        break;
                    }
                    match self.eval_block(body)? {
                        Flow::Value(v) => result = v,
                        Flow::Return(v) => return Ok(Flow::Return(v)),
                    }
                }
                Ok(Flow::Value(result))
            }
            Expr::Match {
                scrutinee,
                arms,
                span,
            } => {
                let sv = self.eval(scrutinee)?.into_value()?;
                for arm in arms {
                    let mut scope = self.env.child();
                    if self.match_pattern(&arm.pat, &sv, &mut scope) {
                        let prev = std::mem::replace(&mut self.env, scope);
                        let result = self.eval(&arm.body);
                        self.env = prev;
                        return result;
                    }
                }
                Err(EvalError::new(
                    "non-exhaustive match: no arm matched",
                    *span,
                ))
            }
            Expr::IfLet {
                pat,
                value,
                then,
                els,
                span: _,
            } => {
                let v = self.eval(value)?.into_value()?;
                let mut scope = self.env.child();
                if self.match_pattern(pat, &v, &mut scope) {
                    let prev = std::mem::replace(&mut self.env, scope);
                    let result = self.eval_block(then);
                    self.env = prev;
                    result
                } else {
                    match els {
                        Some(e) => self.eval(e),
                        None => Ok(Flow::Value(Value::Unit)),
                    }
                }
            }
            Expr::Try { expr, span } => {
                let v = self.eval(expr)?.into_value()?;
                match v {
                    Value::Option(Some(inner)) => Ok(Flow::Value(*inner)),
                    Value::Option(None) => Ok(Flow::Return(Value::Option(None))),
                    Value::Result(Ok(inner)) => Ok(Flow::Value(*inner)),
                    Value::Result(Err(e)) => Ok(Flow::Return(Value::Result(Err(e)))),
                    other => Err(EvalError::new(
                        format!("cannot use `?` on a value of type `{other}`"),
                        *span,
                    )),
                }
            }
            Expr::Block(b) => self.eval_block(b),
            Expr::Variant { name, arg, span } => {
                let av = match arg {
                    Some(a) => Some(self.eval(a)?.into_value()?),
                    None => None,
                };
                match (name.as_str(), av) {
                    ("ok", Some(v)) => Ok(Flow::Value(Value::Result(Ok(Box::new(v))))),
                    ("ok", None) => Err(EvalError::new("`.ok` requires an argument", *span)),
                    ("err", Some(v)) => Ok(Flow::Value(Value::Result(Err(Box::new(v))))),
                    ("err", None) => Err(EvalError::new("`.err` requires an argument", *span)),
                    ("some", Some(v)) => Ok(Flow::Value(Value::Option(Some(Box::new(v))))),
                    ("some", None) => Err(EvalError::new("`.some` requires an argument", *span)),
                    ("none", None) => Ok(Flow::Value(Value::Option(None))),
                    ("none", Some(_)) => Err(EvalError::new("`.none` takes no argument", *span)),
                    (other, _) => Err(EvalError::new(
                        format!("unknown variant constructor `.{other}`"),
                        *span,
                    )),
                }
            }
        }
    }

    /// Evaluate a block. Returns `Flow::Return` if a `return` unwound.
    fn eval_block(&mut self, block: &Block) -> Result<Flow, EvalError> {
        let scope = self.env.child();
        let prev = std::mem::replace(&mut self.env, scope);
        let mut result = Flow::Value(Value::Unit);
        for stmt in &block.stmts {
            result = self.run_stmt(stmt)?;
            if matches!(result, Flow::Return(_)) {
                break;
            }
        }
        self.env = prev;
        Ok(result)
    }

    /// Bind a pattern against a value in `scope`. Returns whether it matched.
    fn match_pattern(&self, pat: &Pattern, value: &Value, scope: &mut Env) -> bool {
        match pat {
            Pattern::Wildcard { .. } => true,
            Pattern::Binding { name } => {
                scope.define(&name.name, value.clone());
                true
            }
            Pattern::Literal { value: lit, .. } => value_matches_lit(value, lit),
            Pattern::Variant { name, arg, .. } => {
                let inner = match (name.as_str(), value) {
                    ("some", Value::Option(Some(v))) => Some(v.as_ref()),
                    ("none", Value::Option(None)) => None,
                    ("ok", Value::Result(Ok(v))) => Some(v.as_ref()),
                    ("err", Value::Result(Err(e))) => Some(e.as_ref()),
                    _ => return false,
                };
                match (arg.as_deref(), inner) {
                    (Some(p), Some(v)) => self.match_pattern(p, v, scope),
                    (None, None) => true,
                    _ => false,
                }
            }
        }
    }

    fn call(&mut self, f: Value, args: Vec<Value>, span: Span) -> Result<Value, EvalError> {
        let fv = match f {
            Value::Func(fv) => fv,
            other => {
                return Err(EvalError::new(
                    format!("cannot call a value of type `{other}`"),
                    span,
                ))
            }
        };
        if args.len() != fv.params.len() {
            return Err(EvalError::new(
                format!(
                    "expected {} arguments, found {}",
                    fv.params.len(),
                    args.len()
                ),
                span,
            ));
        }
        let mut scope = fv.env.child();
        for (p, v) in fv.params.iter().zip(args) {
            scope.define(&p.name.name, v);
        }
        let prev = std::mem::replace(&mut self.env, scope);
        let result = self.eval(&fv.body);
        self.env = prev;
        match result? {
            Flow::Value(v) => Ok(v),
            Flow::Return(v) => Ok(v),
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
                other => Err(EvalError::new(format!("cannot negate `{other}`"), span)),
            },
            UnOp::Not => match v {
                Value::Bool(b) => Ok(Value::Bool(!b)),
                other => Err(EvalError::new(format!("cannot negate `{other}`"), span)),
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
            (Value::Str(a), Value::Str(b)) if op == BinOp::Add => Ok(Value::Str(format!("{a}{b}"))),
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
                    _ => return Err(EvalError::new("arithmetic on non-numeric value", span)),
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
            BinOp::Eq => Ok(Value::Bool(a == b)),
            BinOp::Ne => Ok(Value::Bool(a != b)),
            BinOp::Lt => Ok(Value::Bool(a < b)),
            BinOp::Gt => Ok(Value::Bool(a > b)),
            BinOp::Le => Ok(Value::Bool(a <= b)),
            BinOp::Ge => Ok(Value::Bool(a >= b)),
            BinOp::And | BinOp::Or => unreachable!("short-circuited in eval"),
        }
    }
}

fn value_matches_lit(value: &Value, lit: &Lit) -> bool {
    match (value, lit) {
        (Value::Int(a), Lit::Int(b)) => a == b,
        (Value::Float(a), Lit::Float(b)) => a == b,
        (Value::Str(a), Lit::Str(b)) => a == b,
        (Value::Bool(a), Lit::Bool(b)) => a == b,
        _ => false,
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

    #[test]
    fn strings_and_concat() {
        assert_eq!(eval_src("\"a\" + \"b\"").unwrap(), Value::Str("ab".into()));
    }

    #[test]
    fn comparisons_and_logic() {
        assert_eq!(eval_src("1 < 2").unwrap(), Value::Bool(true));
        assert_eq!(eval_src("1 == 1").unwrap(), Value::Bool(true));
        assert_eq!(eval_src("true && false").unwrap(), Value::Bool(false));
        assert_eq!(eval_src("true || false").unwrap(), Value::Bool(true));
        assert_eq!(eval_src("!true").unwrap(), Value::Bool(false));
    }

    #[test]
    fn if_expression() {
        assert_eq!(eval_src("if true { 1 } else { 2 }").unwrap(), Value::Int(1));
        assert_eq!(
            eval_src("if false { 1 } else { 2 }").unwrap(),
            Value::Int(2)
        );
    }

    #[test]
    fn closure_and_call() {
        assert_eq!(
            eval_src("let f = |x: int| x + 1\nf(5)").unwrap(),
            Value::Int(6)
        );
    }

    #[test]
    fn closure_captures_env() {
        assert_eq!(
            eval_src("let a = 10\nlet f = |x: int| x + a\nf(5)").unwrap(),
            Value::Int(15)
        );
    }

    #[test]
    fn named_func_and_recursion() {
        assert_eq!(
            eval_src(
                "func fact(n: int) -> int { if n <= 1 { 1 } else { n * fact(n - 1) } }\nfact(5)"
            )
            .unwrap(),
            Value::Int(120)
        );
    }

    #[test]
    fn return_unwinds() {
        assert_eq!(
            eval_src("func f() -> int { if true { return 7 }\n 0 }\nf()").unwrap(),
            Value::Int(7)
        );
    }

    #[test]
    fn match_option() {
        assert_eq!(
            eval_src("let v = .some(1)\nmatch v { .some(n) => n, .none => 0 }").unwrap(),
            Value::Int(1)
        );
        assert_eq!(
            eval_src("let v = .none\nmatch v { .some(n) => n, .none => 0 }").unwrap(),
            Value::Int(0)
        );
    }

    #[test]
    fn match_result() {
        assert_eq!(
            eval_src("let v = .ok(1)\nmatch v { .ok(n) => n, .err(_) => 0 }").unwrap(),
            Value::Int(1)
        );
        assert_eq!(
            eval_src("let v = .err(\"x\")\nmatch v { .ok(n) => n, .err(_) => 0 }").unwrap(),
            Value::Int(0)
        );
    }

    #[test]
    fn if_let() {
        assert_eq!(
            eval_src("let v = .some(3)\nif let .some(n) = v { n } else { 0 }").unwrap(),
            Value::Int(3)
        );
        assert_eq!(
            eval_src("let v = .none\nif let .some(n) = v { n } else { 0 }").unwrap(),
            Value::Int(0)
        );
    }

    #[test]
    fn try_unwraps_option() {
        assert_eq!(
            eval_src("func f() -> Option<int> { let x = .some(1)?; .some(x) }\nf()").unwrap(),
            Value::Option(Some(Box::new(Value::Int(1))))
        );
    }

    #[test]
    fn try_propagates_none() {
        assert_eq!(
            eval_src("func f() -> Option<int> { let x = .none?; .some(x) }\nf()").unwrap(),
            Value::Option(None)
        );
    }

    #[test]
    fn try_propagates_err() {
        assert_eq!(
            eval_src("func f() -> Result<int, str> { let x = .err(\"boom\")?; .ok(x) }\nf()")
                .unwrap(),
            Value::Result(Err(Box::new(Value::Str("boom".into()))))
        );
    }

    #[test]
    fn variant_constructors() {
        assert_eq!(
            eval_src(".ok(1)").unwrap(),
            Value::Result(Ok(Box::new(Value::Int(1))))
        );
        assert_eq!(eval_src(".none").unwrap(), Value::Option(None));
    }
}
