//! Phase 1 tree-walker interpreter.
//!
//! Evaluates the AST directly. Slow by design — this exists to bootstrap the
//! frontend and power the REPL until the bytecode VM lands.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use zz_frontend::ast::{BinOp, Block, Expr, FmtPart, Lit, Pattern, Program, Stmt, UnOp};
use zz_frontend::span::Span;

use crate::env::Env;
use crate::value::{FuncValue, NativeFunc, Value};

#[derive(Debug)]
pub struct EvalError {
    pub message: String,
    pub span: Span,
}

impl EvalError {
    pub fn new(message: impl Into<String>, span: Span) -> Self {
        EvalError {
            message: message.into(),
            span,
        }
    }
}

/// Result of evaluating an expression or statement. `Return` unwinds the
/// call stack until the enclosing function call catches it; `Break` and
/// `Continue` unwind to the enclosing loop.
#[derive(Debug)]
pub(crate) enum Flow {
    Value(Value),
    Return(Value),
    Break,
    Continue,
}

impl Flow {
    fn into_value(self) -> Result<Value, EvalError> {
        match self {
            Flow::Value(v) => Ok(v),
            Flow::Return(_) => Err(EvalError::new(
                "`return` outside of a function",
                Span::new(0, 0),
            )),
            Flow::Break => Err(EvalError::new("`break` outside of a loop", Span::new(0, 0))),
            Flow::Continue => Err(EvalError::new(
                "`continue` outside of a loop",
                Span::new(0, 0),
            )),
        }
    }
}

/// A native function implementation. Receives the interpreter (so natives
/// can call back into ZZ, e.g. HTTP route handlers) and the argument vector
/// (a `Vec`, not a slice, because `std.vec.push` must grow it).
#[allow(clippy::ptr_arg)]
pub type NativeFn = fn(&mut Interp, &mut Vec<Value>) -> Result<Value, EvalError>;

/// A registered native function: its arity and Rust implementation.
#[derive(Debug, Clone, Copy)]
pub struct NativeEntry {
    pub arity: usize,
    pub f: NativeFn,
}

pub struct Interp {
    pub env: Rc<RefCell<Env>>,
    /// Named functions, kept separate from the environment so recursive
    /// bodies can resolve their own name without circular captured envs.
    pub funcs: HashMap<String, FuncValue>,
    /// Native (Rust-backed) functions, e.g. the standard library.
    pub natives: HashMap<String, NativeEntry>,
    /// Struct definitions: name → ordered field names.
    pub structs: HashMap<String, Vec<String>>,
    /// Command-line arguments passed to the running script (empty in the
    /// REPL). Exposed to scripts via `std.env.args`.
    pub args: Vec<String>,
}

impl Default for Interp {
    fn default() -> Self {
        Self::new()
    }
}

impl Interp {
    pub fn new() -> Self {
        Interp {
            env: Rc::new(RefCell::new(Env::new())),
            funcs: HashMap::new(),
            natives: HashMap::new(),
            structs: HashMap::new(),
            args: Vec::new(),
        }
    }

    /// Create an interpreter with a native function registry.
    pub fn with_natives(natives: HashMap<String, NativeEntry>) -> Self {
        Interp {
            env: Rc::new(RefCell::new(Env::new())),
            funcs: HashMap::new(),
            natives,
            structs: HashMap::new(),
            args: Vec::new(),
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
            Stmt::Decl { name, value, .. } => match self.eval(value)? {
                Flow::Value(v) => {
                    self.env.borrow_mut().define(&name.name, v.clone());
                    Ok(Flow::Value(v))
                }
                Flow::Return(v) => Ok(Flow::Return(v)),
                Flow::Break => Ok(Flow::Break),
                Flow::Continue => Ok(Flow::Continue),
            },
            Stmt::Import { .. } => {
                // Imports are resolved in a later phase; no runtime effect.
                Ok(Flow::Value(Value::Unit))
            }
            Stmt::Func {
                name, params, body, ..
            } => {
                let fv = FuncValue {
                    params: params.clone(),
                    body: Expr::Block(body.clone()),
                    env: Rc::clone(&self.env),
                };
                self.funcs.insert(name.name.clone(), fv.clone());
                self.env.borrow_mut().define(&name.name, Value::Func(fv));
                Ok(Flow::Value(Value::Unit))
            }
            Stmt::Return { value, .. } => match value {
                Some(e) => match self.eval(e)? {
                    Flow::Value(v) => Ok(Flow::Return(v)),
                    Flow::Return(v) => Ok(Flow::Return(v)),
                    Flow::Break => Ok(Flow::Break),
                    Flow::Continue => Ok(Flow::Continue),
                },
                None => Ok(Flow::Return(Value::Unit)),
            },
            Stmt::Struct { name, fields, .. } => {
                self.structs.insert(
                    name.name.clone(),
                    fields.iter().map(|(n, _)| n.name.clone()).collect(),
                );
                Ok(Flow::Value(Value::Unit))
            }
            Stmt::For {
                var, iter, body, ..
            } => {
                let it = self.eval(iter)?.into_value()?;
                match it {
                    Value::Array(items) => {
                        let mut result = Value::Unit;
                        for item in items {
                            let scope = Env::with_parent(&self.env);
                            scope.borrow_mut().define(&var.name, item);
                            let prev = std::mem::replace(&mut self.env, scope);
                            let flow = self.eval_block(body);
                            self.env = prev;
                            match flow? {
                                Flow::Value(v) => result = v,
                                Flow::Return(v) => return Ok(Flow::Return(v)),
                                Flow::Break => break,
                                Flow::Continue => continue,
                            }
                        }
                        Ok(Flow::Value(result))
                    }
                    Value::Range(start, end) => {
                        let mut result = Value::Unit;
                        let mut i = start;
                        while i < end {
                            let scope = Env::with_parent(&self.env);
                            scope.borrow_mut().define(&var.name, Value::Int(i));
                            let prev = std::mem::replace(&mut self.env, scope);
                            let flow = self.eval_block(body);
                            self.env = prev;
                            match flow? {
                                Flow::Value(v) => result = v,
                                Flow::Return(v) => return Ok(Flow::Return(v)),
                                Flow::Break => break,
                                Flow::Continue => {}
                            }
                            i += 1;
                        }
                        Ok(Flow::Value(result))
                    }
                    other => Err(EvalError::new(
                        format!("cannot iterate a value of type `{other}`"),
                        iter.span(),
                    )),
                }
            }
            Stmt::Break { .. } => Ok(Flow::Break),
            Stmt::Continue { .. } => Ok(Flow::Continue),
            Stmt::Assign { target, value, .. } => {
                let v = self.eval(value)?.into_value()?;
                self.assign_target(target, v)?;
                Ok(Flow::Value(Value::Unit))
            }
            Stmt::Expr(e) => self.eval(e),
        }
    }

    /// Assign a value to an assignment target: a variable, a qualified name,
    /// a struct field path, or an index.
    fn assign_target(&mut self, target: &Expr, value: Value) -> Result<(), EvalError> {
        match target {
            Expr::Ident { name, span } => {
                if !self.env.borrow_mut().assign(name, value) {
                    return Err(EvalError::new(
                        format!("undefined variable `{name}`"),
                        *span,
                    ));
                }
                Ok(())
            }
            Expr::Path { parts, span } => {
                let joined = parts.join(".");
                if self.env.borrow().get(&joined).is_some() {
                    self.env.borrow_mut().assign(&joined, value);
                    return Ok(());
                }
                // Struct field walk: `p.x = v` mutates the object bound to
                // `p` and writes it back.
                let root = &parts[0];
                let mut obj = self.env.borrow().get(root).ok_or_else(|| {
                    EvalError::new(format!("undefined variable `{joined}`"), *span)
                })?;
                for field in &parts[1..parts.len() - 1] {
                    obj = Self::object_field(&obj, field, *span)?;
                }
                let last = parts.last().unwrap();
                Self::set_object_field(&mut obj, last, value, *span)?;
                self.env.borrow_mut().assign(root, obj);
                Ok(())
            }
            Expr::Field { obj, name, span } => {
                let mut objv = self.eval(obj)?.into_value()?;
                Self::set_object_field(&mut objv, name, value, *span)?;
                // If the base is a plain variable, write the mutated object
                // back; otherwise the mutation is discarded (temporary).
                if let Expr::Ident { name, .. } = &**obj {
                    self.env.borrow_mut().assign(name, objv);
                }
                Ok(())
            }
            Expr::Index { obj, index, span } => {
                let iv = self.eval(index)?.into_value()?;
                let mut objv = self.eval(obj)?.into_value()?;
                Self::set_index(&mut objv, &iv, value, *span)?;
                self.write_back(obj, objv)
            }
            other => Err(EvalError::new(
                "cannot assign to this expression".to_string(),
                other.span(),
            )),
        }
    }

    /// Write a mutated container back to the variable it came from. Handles
    /// plain variables, qualified names, and struct-field paths; temporary
    /// bases (e.g. `makePoint()`) discard the mutation.
    fn write_back(&mut self, target: &Expr, new_value: Value) -> Result<(), EvalError> {
        match target {
            Expr::Ident { name, span } => {
                if !self.env.borrow_mut().assign(name, new_value) {
                    return Err(EvalError::new(
                        format!("undefined variable `{name}`"),
                        *span,
                    ));
                }
                Ok(())
            }
            Expr::Path { parts, span } => {
                let joined = parts.join(".");
                if self.env.borrow().get(&joined).is_some() {
                    self.env.borrow_mut().assign(&joined, new_value);
                    return Ok(());
                }
                let root = &parts[0];
                let mut obj = self.env.borrow().get(root).ok_or_else(|| {
                    EvalError::new(format!("undefined variable `{joined}`"), *span)
                })?;
                for field in &parts[1..parts.len() - 1] {
                    obj = Self::object_field(&obj, field, *span)?;
                }
                let last = parts.last().unwrap();
                Self::set_object_field(&mut obj, last, new_value, *span)?;
                self.env.borrow_mut().assign(root, obj);
                Ok(())
            }
            Expr::Field { obj, name, span } => {
                let mut objv = self.eval(obj)?.into_value()?;
                Self::set_object_field(&mut objv, name, new_value, *span)?;
                self.write_back(obj, objv)
            }
            _ => Ok(()),
        }
    }

    pub(crate) fn eval(&mut self, expr: &Expr) -> Result<Flow, EvalError> {
        match expr {
            Expr::Int { value, .. } => Ok(Flow::Value(Value::Int(*value))),
            Expr::Float { value, .. } => Ok(Flow::Value(Value::Float(*value))),
            Expr::Str { value, .. } => Ok(Flow::Value(Value::Str(value.clone()))),
            Expr::Bool { value, .. } => Ok(Flow::Value(Value::Bool(*value))),
            Expr::Ident { name, span } => {
                if let Some(v) = self.env.borrow().get(name) {
                    return Ok(Flow::Value(v));
                }
                if let Some(fv) = self.funcs.get(name) {
                    return Ok(Flow::Value(Value::Func(fv.clone())));
                }
                if let Some(entry) = self.natives.get(name) {
                    return Ok(Flow::Value(Value::Native(NativeFunc {
                        name: name.clone(),
                        arity: entry.arity,
                    })));
                }
                Err(EvalError::new(
                    format!("undefined variable `{name}`"),
                    *span,
                ))
            }
            Expr::Fmt { parts, .. } => {
                let mut out = String::new();
                for part in parts {
                    match part {
                        FmtPart::Text(t) => out.push_str(t),
                        FmtPart::Expr(e) => {
                            let v = self.eval(e)?.into_value()?;
                            out.push_str(&v.to_string());
                        }
                    }
                }
                Ok(Flow::Value(Value::Str(out)))
            }
            Expr::Path { parts, span } => {
                let name = parts.join(".");
                if let Some(v) = self.env.borrow().get(&name) {
                    return Ok(Flow::Value(v));
                }
                if let Some(fv) = self.funcs.get(&name) {
                    return Ok(Flow::Value(Value::Func(fv.clone())));
                }
                if let Some(entry) = self.natives.get(&name) {
                    return Ok(Flow::Value(Value::Native(NativeFunc {
                        name,
                        arity: entry.arity,
                    })));
                }
                // Struct field walk: `p.x` where `p` is a struct instance.
                if let Some(mut v) = self.env.borrow().get(&parts[0]) {
                    for field in &parts[1..] {
                        v = Self::object_field(&v, field, *span)?;
                    }
                    return Ok(Flow::Value(v));
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
                env: Rc::clone(&self.env),
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
                        Flow::Break => break,
                        Flow::Continue => {}
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
                    let scope = Env::with_parent(&self.env);
                    if self.match_pattern(&arm.pat, &sv, &scope) {
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
                let scope = Env::with_parent(&self.env);
                if self.match_pattern(pat, &v, &scope) {
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
            Expr::Array { elems, .. } => {
                let mut vs = Vec::with_capacity(elems.len());
                for e in elems {
                    vs.push(self.eval(e)?.into_value()?);
                }
                Ok(Flow::Value(Value::Array(vs)))
            }
            Expr::Dict { entries, .. } => {
                let mut pairs = Vec::with_capacity(entries.len());
                for (k, v) in entries {
                    let kv = self.eval(k)?.into_value()?;
                    let vv = self.eval(v)?.into_value()?;
                    pairs.push((kv, vv));
                }
                Ok(Flow::Value(Value::Dict(pairs)))
            }
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
            Expr::Field { obj, name, span } => {
                let v = self.eval(obj)?.into_value()?;
                Self::object_field(&v, name, *span).map(Flow::Value)
            }
            Expr::Range { start, end, span } => {
                let s = self.eval(start)?.into_value()?;
                let e = self.eval(end)?.into_value()?;
                match (s, e) {
                    (Value::Int(a), Value::Int(b)) => Ok(Flow::Value(Value::Range(a, b))),
                    _ => Err(EvalError::new("range bounds must be integers", *span)),
                }
            }
            Expr::StructInit { name, fields, span } => {
                let Some(field_names) = self.structs.get(name).cloned() else {
                    return Err(EvalError::new(format!("unknown struct `{name}`"), *span));
                };
                let mut out = Vec::with_capacity(field_names.len());
                for fname in &field_names {
                    let Some(fexpr) = fields.iter().find(|(n, _)| n == fname) else {
                        return Err(EvalError::new(
                            format!("missing field `{fname}` in struct literal"),
                            *span,
                        ));
                    };
                    let v = self.eval(&fexpr.1)?.into_value()?;
                    out.push((fname.clone(), v));
                }
                Ok(Flow::Value(Value::Object {
                    name: name.clone(),
                    fields: out,
                }))
            }
            Expr::Index { obj, index, span } => {
                let ov = self.eval(obj)?.into_value()?;
                let iv = self.eval(index)?.into_value()?;
                Self::get_index(&ov, &iv, *span).map(Flow::Value)
            }
            Expr::Slice {
                obj,
                start,
                end,
                span,
            } => {
                let ov = self.eval(obj)?.into_value()?;
                let s = match start {
                    Some(e) => match self.eval(e)?.into_value()? {
                        Value::Int(i) => Some(i),
                        other => {
                            return Err(EvalError::new(
                                format!("slice bound must be `int`, found `{other}`"),
                                e.span(),
                            ))
                        }
                    },
                    None => None,
                };
                let e = match end {
                    Some(e) => match self.eval(e)?.into_value()? {
                        Value::Int(i) => Some(i),
                        other => {
                            return Err(EvalError::new(
                                format!("slice bound must be `int`, found `{other}`"),
                                e.span(),
                            ))
                        }
                    },
                    None => None,
                };
                Self::slice_value(&ov, s, e, *span).map(Flow::Value)
            }
        }
    }

    /// Evaluate a block. Returns `Flow::Return` if a `return` unwound.
    fn eval_block(&mut self, block: &Block) -> Result<Flow, EvalError> {
        let scope = Env::with_parent(&self.env);
        let prev = std::mem::replace(&mut self.env, scope);
        let mut result = Flow::Value(Value::Unit);
        for stmt in &block.stmts {
            result = self.run_stmt(stmt)?;
            if matches!(result, Flow::Return(_) | Flow::Break | Flow::Continue) {
                break;
            }
        }
        self.env = prev;
        Ok(result)
    }

    /// Bind a pattern against a value in `scope`. Returns whether it matched.
    fn match_pattern(&self, pat: &Pattern, value: &Value, scope: &Rc<RefCell<Env>>) -> bool {
        match pat {
            Pattern::Wildcard { .. } => true,
            Pattern::Binding { name } => {
                scope.borrow_mut().define(&name.name, value.clone());
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

    pub fn call(&mut self, f: Value, mut args: Vec<Value>, span: Span) -> Result<Value, EvalError> {
        match f {
            Value::Native(nf) => {
                if args.len() != nf.arity {
                    return Err(EvalError::new(
                        format!("expected {} arguments, found {}", nf.arity, args.len()),
                        span,
                    ));
                }
                match self.natives.get(&nf.name) {
                    Some(entry) => (entry.f)(self, &mut args),
                    None => Err(EvalError::new(
                        format!("unknown native function `{}`", nf.name),
                        span,
                    )),
                }
            }
            Value::Func(fv) => self.call_func(fv, args, span),
            other => Err(EvalError::new(
                format!("cannot call a value of type `{other}`"),
                span,
            )),
        }
    }

    fn call_func(
        &mut self,
        fv: FuncValue,
        args: Vec<Value>,
        span: Span,
    ) -> Result<Value, EvalError> {
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
        let scope = Env::with_parent(&fv.env);
        for (p, v) in fv.params.iter().zip(args) {
            scope.borrow_mut().define(&p.name.name, v);
        }
        let prev = std::mem::replace(&mut self.env, scope);
        let result = self.eval(&fv.body);
        self.env = prev;
        match result? {
            Flow::Value(v) => Ok(v),
            Flow::Return(v) => Ok(v),
            Flow::Break => Err(EvalError::new("`break` outside of a loop", Span::new(0, 0))),
            Flow::Continue => Err(EvalError::new(
                "`continue` outside of a loop",
                Span::new(0, 0),
            )),
        }
    }

    /// Read a field from a struct instance.
    fn object_field(obj: &Value, name: &str, span: Span) -> Result<Value, EvalError> {
        match obj {
            Value::Object {
                name: tname,
                fields,
            } => fields
                .iter()
                .find(|(n, _)| n == name)
                .map(|(_, v)| v.clone())
                .ok_or_else(|| {
                    EvalError::new(format!("struct `{tname}` has no field `{name}`"), span)
                }),
            other => Err(EvalError::new(
                format!("cannot access field `{name}` on a value of type `{other}`"),
                span,
            )),
        }
    }

    /// Write a field into a struct instance (in place).
    fn set_object_field(
        obj: &mut Value,
        name: &str,
        value: Value,
        span: Span,
    ) -> Result<(), EvalError> {
        match obj {
            Value::Object {
                name: tname,
                fields,
            } => {
                if let Some((_, slot)) = fields.iter_mut().find(|(n, _)| n == name) {
                    *slot = value;
                    Ok(())
                } else {
                    Err(EvalError::new(
                        format!("struct `{tname}` has no field `{name}`"),
                        span,
                    ))
                }
            }
            other => Err(EvalError::new(
                format!("cannot assign to field `{name}` of a value of type `{other}`"),
                span,
            )),
        }
    }

    /// Read an element: `arr[i]`, `dict[key]`, `str[i]`. Negative indices
    /// count from the end.
    fn get_index(obj: &Value, index: &Value, span: Span) -> Result<Value, EvalError> {
        match (obj, index) {
            (Value::Array(items), Value::Int(i)) => {
                let idx = Self::normalize_index(*i, items.len(), span)?;
                Ok(items[idx].clone())
            }
            (Value::Dict(entries), key) => entries
                .iter()
                .find(|(k, _)| k == key)
                .map(|(_, v)| v.clone())
                .ok_or_else(|| EvalError::new(format!("key `{key}` not found in dict"), span)),
            (Value::Str(s), Value::Int(i)) => {
                let chars: Vec<char> = s.chars().collect();
                let idx = Self::normalize_index(*i, chars.len(), span)?;
                Ok(Value::Str(chars[idx].to_string()))
            }
            (other, _) => Err(EvalError::new(
                format!("cannot index a value of type `{}`", other.type_name()),
                span,
            )),
        }
    }

    /// Write an element: `arr[i] = v`, `dict[key] = v`. Missing dict keys
    /// are appended; strings are immutable.
    fn set_index(
        obj: &mut Value,
        index: &Value,
        value: Value,
        span: Span,
    ) -> Result<(), EvalError> {
        match (obj, index) {
            (Value::Array(items), Value::Int(i)) => {
                let idx = Self::normalize_index(*i, items.len(), span)?;
                items[idx] = value;
                Ok(())
            }
            (Value::Dict(entries), key) => {
                if let Some((_, slot)) = entries.iter_mut().find(|(k, _)| k == key) {
                    *slot = value;
                } else {
                    entries.push((key.clone(), value));
                }
                Ok(())
            }
            (Value::Str(_), _) => Err(EvalError::new(
                "cannot assign to an index of a string",
                span,
            )),
            (other, _) => Err(EvalError::new(
                format!(
                    "cannot assign to an index of a value of type `{}`",
                    other.type_name()
                ),
                span,
            )),
        }
    }

    /// Slice an array or string: `s[1:3]`, `s[:2]`, `s[1:]`, `s[:]`.
    /// Bounds are clamped; negative bounds count from the end.
    fn slice_value(
        obj: &Value,
        start: Option<i64>,
        end: Option<i64>,
        span: Span,
    ) -> Result<Value, EvalError> {
        match obj {
            Value::Array(items) => {
                let (a, b) = Self::slice_bounds(start, end, items.len());
                Ok(Value::Array(items[a..b].to_vec()))
            }
            Value::Str(s) => {
                let chars: Vec<char> = s.chars().collect();
                let (a, b) = Self::slice_bounds(start, end, chars.len());
                Ok(Value::Str(chars[a..b].iter().collect()))
            }
            other => Err(EvalError::new(
                format!("cannot slice a value of type `{}`", other.type_name()),
                span,
            )),
        }
    }

    /// Normalize an index (negative counts from the end) and bounds-check it.
    fn normalize_index(i: i64, len: usize, span: Span) -> Result<usize, EvalError> {
        let len_i = len as i64;
        let idx = if i < 0 { len_i + i } else { i };
        if idx < 0 || idx >= len_i {
            return Err(EvalError::new(
                format!("index {i} out of bounds for length {len}"),
                span,
            ));
        }
        Ok(idx as usize)
    }

    /// Normalize slice bounds to a clamped `[a, b)` range.
    fn slice_bounds(start: Option<i64>, end: Option<i64>, len: usize) -> (usize, usize) {
        let len_i = len as i64;
        let norm = |i: i64| {
            let v = if i < 0 { len_i + i } else { i };
            v.clamp(0, len_i)
        };
        let a = norm(start.unwrap_or(0));
        let b = norm(end.unwrap_or(len_i));
        if a > b {
            (0, 0)
        } else {
            (a as usize, b as usize)
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
            eval_src("func f() -> Option<int> { x := .none?; .some(x) }\nf()").unwrap(),
            Value::Option(None)
        );
    }

    #[test]
    fn try_propagates_err() {
        assert_eq!(
            eval_src("func f() -> Result<int, str> { x := .err(\"boom\")?; .ok(x) }\nf()").unwrap(),
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

    #[test]
    fn array_literal() {
        assert_eq!(
            eval_src("scores := [10, 20, 30]\nscores").unwrap(),
            Value::Array(vec![Value::Int(10), Value::Int(20), Value::Int(30)])
        );
    }

    #[test]
    fn array_explicit_decl() {
        assert_eq!(
            eval_src("[int] scores = [1, 2]\nscores").unwrap(),
            Value::Array(vec![Value::Int(1), Value::Int(2)])
        );
    }

    #[test]
    fn dict_literal() {
        assert_eq!(
            eval_src("ages := {\"Zaid\": 20}\nages").unwrap(),
            Value::Dict(vec![(Value::Str("Zaid".into()), Value::Int(20))])
        );
    }

    #[test]
    fn dict_explicit_decl() {
        assert_eq!(
            eval_src("{str: int} ages = {\"a\": 1}\nages").unwrap(),
            Value::Dict(vec![(Value::Str("a".into()), Value::Int(1))])
        );
    }

    #[test]
    fn dict_union_value_type() {
        assert_eq!(
            eval_src("{str: str | int} user = {\"name\": \"Zaid\", \"age\": 20}\nuser").unwrap(),
            Value::Dict(vec![
                (Value::Str("name".into()), Value::Str("Zaid".into())),
                (Value::Str("age".into()), Value::Int(20)),
            ])
        );
    }

    #[test]
    fn import_is_noop() {
        assert_eq!(eval_src("import std.io\nx := 1\nx").unwrap(), Value::Int(1));
    }

    #[test]
    fn native_function_dispatches() {
        #[allow(clippy::ptr_arg)]
        fn double(_interp: &mut Interp, args: &mut Vec<Value>) -> Result<Value, EvalError> {
            let n = match args.first() {
                Some(Value::Int(n)) => *n,
                _ => return Err(EvalError::new("expected int", Span::new(0, 0))),
            };
            Ok(Value::Int(n * 2))
        }
        let mut natives = HashMap::new();
        natives.insert(
            "test.double".into(),
            NativeEntry {
                arity: 1,
                f: double,
            },
        );
        let mut interp = Interp::with_natives(natives);
        let parsed = parse("test.double(21)\n");
        assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
        let v = interp.run(&parsed.program).unwrap();
        assert_eq!(v, Value::Int(42));
    }

    #[test]
    fn native_wrong_arity_errors() {
        fn noop(_interp: &mut Interp, _: &mut Vec<Value>) -> Result<Value, EvalError> {
            Ok(Value::Unit)
        }
        let mut natives = HashMap::new();
        natives.insert("test.noop".into(), NativeEntry { arity: 1, f: noop });
        let mut interp = Interp::with_natives(natives);
        let parsed = parse("test.noop()\n");
        assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
        let err = interp.run(&parsed.program).unwrap_err();
        assert!(
            err.message.contains("expected 1 arguments"),
            "{}",
            err.message
        );
    }

    #[test]
    fn unknown_path_errors() {
        let err = eval_src("foo.bar.baz(1)").unwrap_err();
        assert!(
            err.message.contains("undefined variable `foo.bar.baz`"),
            "{}",
            err.message
        );
    }

    // --- structs -----------------------------------------------------------

    #[test]
    fn struct_init_and_field_access() {
        let v = eval_src("struct Point { x: int, y: int }\np := Point{ x: 1, y: 2 }\np.x").unwrap();
        assert_eq!(v, Value::Int(1));
    }

    #[test]
    fn struct_displays_with_name() {
        let v = eval_src("struct Point { x: int, y: int }\nPoint{ x: 1, y: 2 }").unwrap();
        assert_eq!(v.to_string(), "Point{x: 1, y: 2}");
    }

    #[test]
    fn struct_field_mutation() {
        let v =
            eval_src("struct Point { x: int, y: int }\np := Point{ x: 1, y: 2 }\np.x = 10\np.x")
                .unwrap();
        assert_eq!(v, Value::Int(10));
    }

    #[test]
    fn struct_field_mutation_visible_in_object() {
        let v = eval_src("struct Point { x: int, y: int }\np := Point{ x: 1, y: 2 }\np.x = 10\np")
            .unwrap();
        assert_eq!(
            v,
            Value::Object {
                name: "Point".into(),
                fields: vec![("x".into(), Value::Int(10)), ("y".into(), Value::Int(2)),],
            }
        );
    }

    #[test]
    fn struct_nested_field_access() {
        let v = eval_src(
            "struct Point { x: int, y: int }\nstruct Nested { p: Point, z: int }\nn := Nested{ p: Point{ x: 1, y: 2 }, z: 3 }\nn.p.y",
        )
        .unwrap();
        assert_eq!(v, Value::Int(2));
    }

    #[test]
    fn struct_passed_to_func() {
        let v = eval_src(
            "struct Point { x: int, y: int }\nfunc dist(p: Point) -> int { p.x + p.y }\ndist(Point{ x: 3, y: 4 })",
        )
        .unwrap();
        assert_eq!(v, Value::Int(7));
    }

    #[test]
    fn struct_missing_field_errors() {
        let err = eval_src("struct Point { x: int, y: int }\nPoint{ x: 1 }").unwrap_err();
        assert!(err.message.contains("missing field `y`"), "{}", err.message);
    }

    #[test]
    fn struct_unknown_field_errors() {
        let err =
            eval_src("struct Point { x: int, y: int }\np := Point{ x: 1, y: 2 }\np.z").unwrap_err();
        assert!(err.message.contains("has no field `z`"), "{}", err.message);
    }

    #[test]
    fn struct_field_on_non_struct_errors() {
        let err = eval_src("x := 5\nx.y").unwrap_err();
        assert!(
            err.message.contains("cannot access field `y`"),
            "{}",
            err.message
        );
    }

    #[test]
    fn struct_unknown_type_errors() {
        let err = eval_src("Nope{ x: 1 }").unwrap_err();
        assert!(
            err.message.contains("unknown struct `Nope`"),
            "{}",
            err.message
        );
    }

    // --- for loops ---------------------------------------------------------

    #[test]
    fn for_over_range_sums() {
        let v = eval_src("sum := 0\nfor i in 0..5 { sum = sum + i }\nsum").unwrap();
        assert_eq!(v, Value::Int(10));
    }

    #[test]
    fn for_over_array_sums() {
        let v = eval_src("total := 0\nfor n in [10, 20, 30] { total = total + n }\ntotal").unwrap();
        assert_eq!(v, Value::Int(60));
    }

    #[test]
    fn for_break_stops_loop() {
        let v = eval_src("found := 0\nfor i in 0..10 { if i == 3 { found = i; break } }\nfound")
            .unwrap();
        assert_eq!(v, Value::Int(3));
    }

    #[test]
    fn for_continue_skips_iteration() {
        let v = eval_src(
            "count := 0\nfor i in 0..5 { if i == 2 { continue }; count = count + 1 }\ncount",
        )
        .unwrap();
        assert_eq!(v, Value::Int(4));
    }

    #[test]
    fn for_loop_var_does_not_leak() {
        let err = eval_src("for i in 0..5 { i }\ni").unwrap_err();
        assert!(
            err.message.contains("undefined variable `i`"),
            "{}",
            err.message
        );
    }

    #[test]
    fn for_over_non_iterable_errors() {
        let err = eval_src("for i in 5 { i }").unwrap_err();
        assert!(err.message.contains("cannot iterate"), "{}", err.message);
    }

    #[test]
    fn break_outside_loop_errors() {
        let err = eval_src("break").unwrap_err();
        assert!(
            err.message.contains("`break` outside of a loop"),
            "{}",
            err.message
        );
    }

    #[test]
    fn continue_outside_loop_errors() {
        let err = eval_src("continue").unwrap_err();
        assert!(
            err.message.contains("`continue` outside of a loop"),
            "{}",
            err.message
        );
    }

    #[test]
    fn while_loop_with_break() {
        let v = eval_src("x := 0\nwhile x < 10 { x = x + 1; if x == 3 { break } }\nx").unwrap();
        assert_eq!(v, Value::Int(3));
    }

    #[test]
    fn range_value_displays() {
        let v = eval_src("0..5").unwrap();
        assert_eq!(v.to_string(), "0..5");
    }

    #[test]
    fn assignment_to_undefined_errors() {
        let err = eval_src("nope = 5").unwrap_err();
        assert!(
            err.message.contains("undefined variable `nope`"),
            "{}",
            err.message
        );
    }

    #[test]
    fn closure_mutation_propagates() {
        // Shared scopes: a closure can mutate a captured variable.
        let v = eval_src("x := 0\nf := |n: int| { x = x + n }\nf(5)\nf(3)\nx").unwrap();
        assert_eq!(v, Value::Int(8));
    }

    // --- indexing & slicing -------------------------------------------------

    #[test]
    fn array_index() {
        let v = eval_src("scores := [10, 20, 30]\nscores[1]").unwrap();
        assert_eq!(v, Value::Int(20));
    }

    #[test]
    fn array_negative_index() {
        let v = eval_src("scores := [10, 20, 30]\nscores[-1]").unwrap();
        assert_eq!(v, Value::Int(30));
    }

    #[test]
    fn array_index_out_of_bounds_errors() {
        let err = eval_src("scores := [1, 2]\nscores[5]").unwrap_err();
        assert!(
            err.message.contains("index 5 out of bounds for length 2"),
            "{}",
            err.message
        );
    }

    #[test]
    fn array_negative_index_out_of_bounds_errors() {
        let err = eval_src("scores := [1, 2]\nscores[-3]").unwrap_err();
        assert!(
            err.message.contains("index -3 out of bounds for length 2"),
            "{}",
            err.message
        );
    }

    #[test]
    fn dict_index() {
        let v = eval_src("ages := {\"a\": 1, \"b\": 2}\nages[\"b\"]").unwrap();
        assert_eq!(v, Value::Int(2));
    }

    #[test]
    fn dict_missing_key_errors() {
        let err = eval_src("ages := {\"a\": 1}\nages[\"zz\"]").unwrap_err();
        assert!(
            err.message.contains("key `zz` not found in dict"),
            "{}",
            err.message
        );
    }

    #[test]
    fn str_index() {
        let v = eval_src("\"hello\"[1]").unwrap();
        assert_eq!(v, Value::Str("e".to_string()));
    }

    #[test]
    fn index_non_indexable_errors() {
        let err = eval_src("x := 5\nx[0]").unwrap_err();
        assert!(
            err.message.contains("cannot index a value of type `int`"),
            "{}",
            err.message
        );
    }

    #[test]
    fn array_slice() {
        let v = eval_src("scores := [10, 20, 30, 40]\nscores[1:3]").unwrap();
        assert_eq!(v, Value::Array(vec![Value::Int(20), Value::Int(30)]));
    }

    #[test]
    fn slice_open_bounds() {
        assert_eq!(
            eval_src("scores := [10, 20, 30]\nscores[:2]").unwrap(),
            Value::Array(vec![Value::Int(10), Value::Int(20)])
        );
        assert_eq!(
            eval_src("scores := [10, 20, 30]\nscores[1:]").unwrap(),
            Value::Array(vec![Value::Int(20), Value::Int(30)])
        );
        assert_eq!(
            eval_src("scores := [10, 20, 30]\nscores[:]").unwrap(),
            Value::Array(vec![Value::Int(10), Value::Int(20), Value::Int(30)])
        );
    }

    #[test]
    fn slice_negative_bounds() {
        let v = eval_src("\"hello\"[-2:]").unwrap();
        assert_eq!(v, Value::Str("lo".to_string()));
    }

    #[test]
    fn slice_clamps_bounds() {
        // Out-of-range bounds clamp instead of erroring.
        let v = eval_src("scores := [10, 20, 30]\nscores[1:99]").unwrap();
        assert_eq!(v, Value::Array(vec![Value::Int(20), Value::Int(30)]));
    }

    #[test]
    fn str_slice() {
        let v = eval_src("\"hello\"[1:3]").unwrap();
        assert_eq!(v, Value::Str("el".to_string()));
    }

    #[test]
    fn slice_non_sliceable_errors() {
        let err = eval_src("x := 5\nx[1:2]").unwrap_err();
        assert!(
            err.message.contains("cannot slice a value of type `int`"),
            "{}",
            err.message
        );
    }

    #[test]
    fn array_index_assign() {
        let v = eval_src("scores := [10, 20, 30]\nscores[0] = 99\nscores[0]").unwrap();
        assert_eq!(v, Value::Int(99));
    }

    #[test]
    fn array_index_assign_negative() {
        let v = eval_src("scores := [10, 20, 30]\nscores[-1] = 99\nscores[2]").unwrap();
        assert_eq!(v, Value::Int(99));
    }

    #[test]
    fn dict_index_assign_existing() {
        let v = eval_src("ages := {\"a\": 1}\nages[\"a\"] = 5\nages[\"a\"]").unwrap();
        assert_eq!(v, Value::Int(5));
    }

    #[test]
    fn dict_index_assign_new_key() {
        let v = eval_src("ages := {\"a\": 1}\nages[\"b\"] = 2\nages[\"b\"]").unwrap();
        assert_eq!(v, Value::Int(2));
    }

    #[test]
    fn str_index_assign_errors() {
        let err = eval_src("s := \"abc\"\ns[0] = \"x\"").unwrap_err();
        assert!(
            err.message
                .contains("cannot assign to an index of a string"),
            "{}",
            err.message
        );
    }

    #[test]
    fn index_assign_through_field() {
        // `obj.field[i] = v` writes back through the struct field.
        let v = eval_src(
            "struct Box { items: [int] }\nb := Box{ items: [1, 2, 3] }\nb.items[1] = 99\nb.items[1]",
        )
        .unwrap();
        assert_eq!(v, Value::Int(99));
    }

    // --- pipeline -----------------------------------------------------------

    #[test]
    fn pipe_inserts_lhs_as_first_arg() {
        let v = eval_src("func dbl(a: int, b: int) -> int { a * b }\n5 |> dbl(3)").unwrap();
        assert_eq!(v, Value::Int(15));
    }

    #[test]
    fn pipe_bare_name() {
        let v = eval_src("func inc(n: int) -> int { n + 1 }\n5 |> inc").unwrap();
        assert_eq!(v, Value::Int(6));
    }

    #[test]
    fn pipe_chain() {
        let v = eval_src(
            "func inc(n: int) -> int { n + 1 }\nfunc dbl(n: int) -> int { n * 2 }\n5 |> inc |> dbl",
        )
        .unwrap();
        assert_eq!(v, Value::Int(12));
    }
}
