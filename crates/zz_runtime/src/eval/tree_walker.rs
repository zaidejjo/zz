use std::cell::RefCell;
use std::rc::Rc;

use zz_frontend::ast::{BinOp, Block, Expr, FmtPart, Pattern, Stmt};
use zz_frontend::span::Span;

use crate::env::Env;
use crate::runtime::format::{format_value_with_spec, value_matches_lit};
use crate::runtime::ops::{
    eval_binary, eval_unary, get_index, object_field, set_index, set_object_field, slice_value,
};
use crate::runtime::{EvalError, Flow};
use crate::value::NativeFunc;
use crate::value::{FuncValue, ObjectValue, RangeValue, Value};

use super::Interp;

impl Interp {
    pub(crate) fn run_stmt(&mut self, stmt: &Stmt) -> Result<Flow, EvalError> {
        match stmt {
            Stmt::Decl { name, value, .. } => match self.eval(value)? {
                Flow::Value(v) => {
                    self.env.borrow_mut().define(&name.name, v.clone());
                    Ok(Flow::Value(v))
                }
                Flow::Return(v) => Ok(Flow::Return(v)),
                Flow::Break(span) => Ok(Flow::Break(span)),
                Flow::Continue(span) => Ok(Flow::Continue(span)),
            },
            Stmt::Import { .. } => Ok(Flow::Value(Value::Unit)),
            Stmt::Func {
                name, params, body, ..
            } => {
                let fv = FuncValue {
                    params: params.clone(),
                    body: Expr::Block(body.clone()),
                    env: Rc::clone(&self.env),
                    chunk: None,
                };
                self.funcs.insert(name.join("."), fv.clone());
                self.env
                    .borrow_mut()
                    .define(&name.join("."), Value::Func(Box::new(fv)));
                Ok(Flow::Value(Value::Unit))
            }
            Stmt::Return { value, .. } => match value {
                Some(e) => match self.eval(e)? {
                    Flow::Value(v) => Ok(Flow::Return(v)),
                    Flow::Return(v) => Ok(Flow::Return(v)),
                    Flow::Break(span) => Ok(Flow::Break(span)),
                    Flow::Continue(span) => Ok(Flow::Continue(span)),
                },
                None => Ok(Flow::Return(Value::Unit)),
            },
            Stmt::Struct { name, fields, .. } => {
                self.structs.insert(
                    name.join("."),
                    fields.iter().map(|(n, _)| n.name.clone()).collect(),
                );
                Ok(Flow::Value(Value::Unit))
            }
            Stmt::Impl { name, methods, .. } => {
                let type_name = name.join(".");
                for method in methods {
                    if let Stmt::Func {
                        name: mname,
                        params,
                        body,
                        ..
                    } = method
                    {
                        let full_name = format!("{}.{}", type_name, mname.join("."));
                        let fv = FuncValue {
                            params: params.clone(),
                            body: Expr::Block(body.clone()),
                            env: Rc::clone(&self.env),
                            chunk: None,
                        };
                        self.funcs.insert(full_name.clone(), fv.clone());
                        self.env
                            .borrow_mut()
                            .define(&full_name, Value::Func(Box::new(fv)));
                    }
                }
                Ok(Flow::Value(Value::Unit))
            }
            Stmt::For {
                vars, iter, body, ..
            } => {
                let it = self.eval(iter)?.into_value()?;
                match it {
                    Value::Array(items) => {
                        let mut result = Value::Unit;
                        for item in *items {
                            let scope = Env::with_parent(&self.env);
                            {
                                let mut env = scope.borrow_mut();
                                env.define(&vars[0].name, item);
                            }
                            let prev = std::mem::replace(&mut self.env, scope);
                            let flow = self.eval_block(body);
                            self.env = prev;
                            match flow? {
                                Flow::Value(v) => result = v,
                                Flow::Return(v) => return Ok(Flow::Return(v)),
                                Flow::Break(_) => break,
                                Flow::Continue(_) => {}
                            }
                        }
                        Ok(Flow::Value(result))
                    }
                    Value::Range(r) => {
                        let (start, end, step) = (r.start, r.end, r.step);
                        let mut result = Value::Unit;
                        let mut i = start;
                        if step > 0 {
                            while i < end {
                                let scope = Env::with_parent(&self.env);
                                scope.borrow_mut().define(&vars[0].name, Value::Int(i));
                                let prev = std::mem::replace(&mut self.env, scope);
                                let flow = self.eval_block(body);
                                self.env = prev;
                                match flow? {
                                    Flow::Value(v) => result = v,
                                    Flow::Return(v) => return Ok(Flow::Return(v)),
                                    Flow::Break(_) => break,
                                    Flow::Continue(_) => {}
                                }
                                i += step;
                            }
                        } else {
                            while i > end {
                                let scope = Env::with_parent(&self.env);
                                scope.borrow_mut().define(&vars[0].name, Value::Int(i));
                                let prev = std::mem::replace(&mut self.env, scope);
                                let flow = self.eval_block(body);
                                self.env = prev;
                                match flow? {
                                    Flow::Value(v) => result = v,
                                    Flow::Return(v) => return Ok(Flow::Return(v)),
                                    Flow::Break(_) => break,
                                    Flow::Continue(_) => {}
                                }
                                i += step;
                            }
                        }
                        Ok(Flow::Value(result))
                    }
                    Value::Dict(pairs) => {
                        let mut result = Value::Unit;
                        for (k, v) in *pairs {
                            let scope = Env::with_parent(&self.env);
                            {
                                let mut env = scope.borrow_mut();
                                if vars.len() == 2 {
                                    env.define(&vars[0].name, k);
                                    env.define(&vars[1].name, v);
                                } else {
                                    env.define(&vars[0].name, k);
                                }
                            }
                            let prev = std::mem::replace(&mut self.env, scope);
                            let flow = self.eval_block(body);
                            self.env = prev;
                            match flow? {
                                Flow::Value(v) => result = v,
                                Flow::Return(v) => return Ok(Flow::Return(v)),
                                Flow::Break(_) => break,
                                Flow::Continue(_) => {}
                            }
                        }
                        Ok(Flow::Value(result))
                    }
                    other => Err(EvalError::new(
                        format!("cannot iterate a value of type `{other}`"),
                        iter.span(),
                    )),
                }
            }
            Stmt::Break { span } => Ok(Flow::Break(*span)),
            Stmt::Continue { span } => Ok(Flow::Continue(*span)),
            Stmt::Defer { expr, .. } => {
                let closure = FuncValue {
                    params: vec![],
                    body: expr.as_ref().clone(),
                    env: Rc::clone(&self.env),
                    chunk: None,
                };
                self.defer_stacks
                    .last_mut()
                    .unwrap()
                    .push(Value::Func(Box::new(closure)));
                Ok(Flow::Value(Value::Unit))
            }
            Stmt::Destructure { pat, value, .. } => {
                let v = self.eval(value)?.into_value()?;
                if !self.match_pattern(pat, &v, &self.env) {
                    return Err(EvalError::new(
                        "destructuring pattern does not match value",
                        pat.span(),
                    ));
                }
                Ok(Flow::Value(Value::Unit))
            }
            Stmt::Assign { target, value, .. } => {
                let v = self.eval(value)?.into_value()?;
                self.assign_target(target, v)?;
                Ok(Flow::Value(Value::Unit))
            }
            Stmt::Expr(e) => {
                // Method call write-back: if `obj.method(args)` is called as a
                // statement, the return value (e.g. the new array from push/pop)
                // must be written back to `obj` so the mutation is visible.
                // Only intercept known mutating methods to avoid corrupting
                // non-mutating calls (e.g. `arr.len()` should not write back).
                if let Expr::Call {
                    callee,
                    args,
                    named,
                    ..
                } = e
                {
                    const MUTATING_METHODS: &[&str] = &[
                        "push", "pop", "insert", "remove", "reverse", "sort", "append",
                    ];

                    // Handle `arr.push(x)` — parsed as Path { parts: ["arr", "push"] }
                    if let Expr::Path { parts, span: pspan } = callee.as_ref() {
                        if parts.len() == 2 {
                            let method_name = &parts[1];
                            if MUTATING_METHODS.contains(&method_name.as_str()) {
                                let obj_name = &parts[0];
                                let recv = self.resolve_path_value(parts, *pspan)?;
                                let f = self.lookup_method(&recv, method_name, *pspan)?;
                                let mut arg_vals = vec![recv];
                                for a in args {
                                    arg_vals.push(self.eval(a)?.into_value()?);
                                }
                                for (_, v) in named {
                                    arg_vals.push(self.eval(v)?.into_value()?);
                                }
                                let result = self.call(f, arg_vals, *pspan)?;
                                if !self.env.borrow_mut().assign(obj_name, result) {
                                    return Err(EvalError::new(
                                        format!("undefined variable `{obj_name}`"),
                                        *pspan,
                                    ));
                                }
                                return Ok(Flow::Value(Value::Unit));
                            }
                        }
                    }
                    // Handle `obj.method(x)` — parsed as Field { obj, name }
                    if let Expr::Field {
                        obj: field_obj,
                        name: method_name,
                        ..
                    } = callee.as_ref()
                    {
                        if MUTATING_METHODS.contains(&method_name.as_str()) {
                            if let Expr::Ident { name, span } = field_obj.as_ref() {
                                let recv = self.eval(field_obj)?.into_value()?;
                                let f = self.lookup_method(&recv, method_name, *span)?;
                                let mut arg_vals = vec![recv];
                                for a in args {
                                    arg_vals.push(self.eval(a)?.into_value()?);
                                }
                                for (_, v) in named {
                                    arg_vals.push(self.eval(v)?.into_value()?);
                                }
                                let result = self.call(f, arg_vals, *span)?;
                                if !self.env.borrow_mut().assign(name, result) {
                                    return Err(EvalError::new(
                                        format!("undefined variable `{name}`"),
                                        *span,
                                    ));
                                }
                                return Ok(Flow::Value(Value::Unit));
                            }
                        }
                    }
                    // Built-in `append(arr, val)` write-back.
                    if let Expr::Ident { name: fname, .. } = callee.as_ref() {
                        if fname == "append" && args.len() == 2 && named.is_empty() {
                            if let Expr::Ident {
                                name: arr_name,
                                span,
                            } = &args[0]
                            {
                                let result = self.eval(e)?;
                                if !self.env.borrow_mut().assign(arr_name, result.into_value()?) {
                                    return Err(EvalError::new(
                                        format!("undefined variable `{arr_name}`"),
                                        *span,
                                    ));
                                }
                                return Ok(Flow::Value(Value::Unit));
                            }
                        }
                    }
                }
                self.eval(e)
            }
        }
    }

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
            Expr::Path { parts, span } => self.assign_path(parts, value, *span),
            Expr::Field { obj, name, span } => {
                let mut objv = self.eval(obj)?.into_value()?;
                set_object_field(&mut objv, name, value, *span)?;
                if let Expr::Ident { name, .. } = &**obj {
                    self.env.borrow_mut().assign(name, objv);
                }
                Ok(())
            }
            Expr::Index { obj, index, span } => {
                let iv = self.eval(index)?.into_value()?;
                let mut objv = self.eval(obj)?.into_value()?;
                set_index(&mut objv, &iv, value, *span)?;
                self.write_back(obj, objv)
            }
            other => Err(EvalError::new(
                "cannot assign to this expression".to_string(),
                other.span(),
            )),
        }
    }

    pub(crate) fn assign_path(
        &mut self,
        parts: &[String],
        value: Value,
        span: Span,
    ) -> Result<(), EvalError> {
        let joined = parts.join(".");
        if self.env.borrow().get(&joined).is_some() {
            self.env.borrow_mut().assign(&joined, value);
            return Ok(());
        }
        let root = &parts[0];
        let mut chain = vec![self
            .env
            .borrow()
            .get(root)
            .ok_or_else(|| EvalError::new(format!("undefined variable `{joined}`"), span))?];
        for field in &parts[1..parts.len() - 1] {
            let next = object_field(chain.last().unwrap(), field, span)?;
            chain.push(next);
        }
        let last = parts.last().unwrap();
        set_object_field(chain.last_mut().unwrap(), last, value, span)?;
        for i in (1..chain.len()).rev() {
            let child = chain[i].clone();
            set_object_field(&mut chain[i - 1], &parts[i], child, span)?;
        }
        self.env.borrow_mut().assign(root, chain[0].clone());
        Ok(())
    }

    pub(crate) fn resolve_path_value(
        &self,
        parts: &[String],
        span: Span,
    ) -> Result<Value, EvalError> {
        let name = parts.join(".");
        if let Some(v) = self.env.borrow().get(&name) {
            return Ok(v);
        }
        if let Some(fv) = self.funcs.get(&name) {
            return Ok(Value::Func(Box::new(fv.clone())));
        }
        if let Some(entry) = self.natives.get(&name) {
            return Ok(Value::Native(Box::new(NativeFunc {
                name,
                arity: entry.arity,
            })));
        }
        if let Some(mut v) = self.env.borrow().get(&parts[0]) {
            for field in &parts[1..] {
                v = object_field(&v, field, span)?;
            }
            return Ok(v);
        }
        Err(EvalError::new(format!("undefined variable `{name}`"), span))
    }

    fn lookup_callable(&self, name: &str, span: Span) -> Result<Value, EvalError> {
        if let Some(v) = self.env.borrow().get(name) {
            return Ok(v);
        }
        if let Some(fv) = self.funcs.get(name) {
            return Ok(Value::Func(Box::new(fv.clone())));
        }
        if let Some(entry) = self.natives.get(name) {
            return Ok(Value::Native(Box::new(NativeFunc {
                name: name.to_string(),
                arity: entry.arity,
            })));
        }
        Err(EvalError::new(format!("undefined method `{name}`"), span))
    }

    pub(crate) fn lookup_method(
        &self,
        recv: &Value,
        method: &str,
        span: Span,
    ) -> Result<Value, EvalError> {
        if let Ok(f) = self.lookup_callable(method, span) {
            return Ok(f);
        }
        if let Some(ns) = recv.method_namespace() {
            if let Ok(f) = self.lookup_callable(&format!("{ns}.{method}"), span) {
                return Ok(f);
            }
        }
        if let Value::Object(o) = recv {
            // Try TypeName.method (impl block methods)
            if let Ok(f) = self.lookup_callable(&format!("{}.{}", o.name, method), span) {
                return Ok(f);
            }
            // Try namespace.method (cross-module)
            if let Some((ns, _)) = o.name.rsplit_once('.') {
                if let Ok(f) = self.lookup_callable(&format!("{ns}.{method}"), span) {
                    return Ok(f);
                }
            }
        }
        Err(EvalError::new(format!("undefined method `{method}`"), span))
    }

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
                let mut chain = vec![self.env.borrow().get(root).ok_or_else(|| {
                    EvalError::new(format!("undefined variable `{joined}`"), *span)
                })?];
                for field in &parts[1..parts.len() - 1] {
                    let next = object_field(chain.last().unwrap(), field, *span)?;
                    chain.push(next);
                }
                let last = parts.last().unwrap();
                set_object_field(chain.last_mut().unwrap(), last, new_value, *span)?;
                for i in (1..chain.len()).rev() {
                    let child = chain[i].clone();
                    set_object_field(&mut chain[i - 1], &parts[i], child, *span)?;
                }
                self.env.borrow_mut().assign(root, chain[0].clone());
                Ok(())
            }
            Expr::Field { obj, name, span } => {
                let mut objv = self.eval(obj)?.into_value()?;
                set_object_field(&mut objv, name, new_value, *span)?;
                self.write_back(obj, objv)
            }
            _ => Ok(()),
        }
    }

    pub(crate) fn eval(&mut self, expr: &Expr) -> Result<Flow, EvalError> {
        match expr {
            Expr::Int { value, .. } => Ok(Flow::Value(Value::Int(*value))),
            Expr::Float { value, .. } => Ok(Flow::Value(Value::Float(*value))),
            Expr::Str { value, .. } => Ok(Flow::Value(Value::Str(value.clone().into()))),
            Expr::Bool { value, .. } => Ok(Flow::Value(Value::Bool(*value))),
            Expr::Ident { name, span } => {
                if let Some(v) = self.env.borrow().get(name) {
                    return Ok(Flow::Value(v));
                }
                if let Some(fv) = self.funcs.get(name) {
                    return Ok(Flow::Value(Value::Func(Box::new(fv.clone()))));
                }
                if let Some(entry) = self.natives.get(name) {
                    return Ok(Flow::Value(Value::Native(Box::new(NativeFunc {
                        name: name.clone(),
                        arity: entry.arity,
                    }))));
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
                        FmtPart::Expr(e, fmt) => {
                            let v = self.eval(e)?.into_value()?;
                            match fmt {
                                Some(spec) => out.push_str(&format_value_with_spec(&v, spec)),
                                None => out.push_str(&v.to_string()),
                            }
                        }
                    }
                }
                Ok(Flow::Value(Value::Str(out.into())))
            }
            Expr::Path { parts, span } => self.resolve_path_value(parts, *span).map(Flow::Value),
            Expr::Paren { expr, .. } => self.eval(expr),
            Expr::Unary { op, expr, span } => {
                let v = self.eval(expr)?.into_value()?;
                eval_unary(*op, v, *span).map(Flow::Value)
            }
            Expr::Binary {
                op,
                left,
                right,
                span,
            } => {
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
                    BinOp::Elvis => {
                        let l = self.eval(left)?.into_value()?;
                        match l {
                            Value::Option(Some(v)) => return Ok(Flow::Value(*v)),
                            Value::Option(None) => {}
                            Value::Result(r) => match &*r {
                                Ok(v) => return Ok(Flow::Value(v.clone())),
                                Err(_) => {}
                            },

                            other => {
                                return Ok(Flow::Value(other));
                            }
                        }
                        let r = self.eval(right)?.into_value()?;
                        return Ok(Flow::Value(r));
                    }
                    _ => {}
                }
                let l = self.eval(left)?.into_value()?;
                let r = self.eval(right)?.into_value()?;
                eval_binary(*op, l, r, *span).map(Flow::Value)
            }
            Expr::Call {
                callee,
                args,
                named,
                span,
            } => {
                if let Expr::Path { parts, span: pspan } = callee.as_ref() {
                    if parts.len() >= 2 {
                        let joined = parts.join(".");
                        let is_direct = self.env.borrow().get(&joined).is_some()
                            || self.funcs.contains_key(&joined)
                            || self.natives.contains_key(&joined);
                        if !is_direct && self.resolve_path_value(parts, *pspan).is_err() {
                            let method = parts.last().unwrap();
                            let recv =
                                self.resolve_path_value(&parts[..parts.len() - 1], *pspan)?;
                            let f = self.lookup_method(&recv, method, *pspan)?;
                            let mut arg_vals = vec![recv];
                            for a in args {
                                arg_vals.push(self.eval(a)?.into_value()?);
                            }
                            return self.call(f, arg_vals, *span).map(Flow::Value);
                        }
                    }
                }
                if let Expr::Field {
                    obj,
                    name,
                    span: fspan,
                } = callee.as_ref()
                {
                    let recv = self.eval(obj)?.into_value()?;
                    let f = self.lookup_method(&recv, name, *fspan)?;
                    let mut arg_vals = vec![recv];
                    for a in args {
                        arg_vals.push(self.eval(a)?.into_value()?);
                    }
                    return self.call(f, arg_vals, *span).map(Flow::Value);
                }
                let f = self.eval(callee)?.into_value()?;
                let mut arg_vals: Vec<Value> = Vec::with_capacity(args.len() + named.len());
                for a in args {
                    arg_vals.push(self.eval(a)?.into_value()?);
                }
                let mut named_vals: Vec<(String, Value)> = Vec::with_capacity(named.len());
                for (n, v) in named {
                    named_vals.push((n.clone(), self.eval(v)?.into_value()?));
                }
                if !named_vals.is_empty() {
                    if let Value::Func(fv) = &f {
                        let n = fv.params.len();
                        let mut reordered: Vec<Value> = vec![Value::Unit; n];
                        for (i, v) in arg_vals.iter().enumerate() {
                            if i < n {
                                reordered[i] = v.clone();
                            }
                        }
                        for (name, val) in &named_vals {
                            if let Some(i) = fv.params.iter().position(|p| &p.name.name == name) {
                                reordered[i] = val.clone();
                            }
                        }
                        arg_vals = reordered;
                    }
                }
                self.call(f, arg_vals, *span).map(Flow::Value)
            }
            Expr::Closure { params, body, .. } => {
                Ok(Flow::Value(Value::Func(Box::new(FuncValue {
                    params: params.clone(),
                    body: (**body).clone(),
                    env: Rc::clone(&self.env),
                    chunk: None,
                }))))
            }
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
                        Flow::Break(_) => break,
                        Flow::Continue(_) => {}
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
                        // Check match guard if present
                        if let Some(ref guard) = arm.guard {
                            let guard_val = self.eval(guard)?.into_value()?;
                            match guard_val {
                                Value::Bool(true) => {}
                                _ => {
                                    self.env = prev;
                                    continue;
                                }
                            }
                        }
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
                    Value::Result(r) => match &*r {
                        Ok(inner) => Ok(Flow::Value(inner.clone())),
                        Err(e) => Ok(Flow::Return(Value::Result(Box::new(Err(e.clone()))))),
                    },

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
                Ok(Flow::Value(Value::Array(Box::new(vs))))
            }
            Expr::Tuple { items, .. } => {
                let mut vs = Vec::with_capacity(items.len());
                for e in items {
                    vs.push(self.eval(e)?.into_value()?);
                }
                Ok(Flow::Value(Value::Array(Box::new(vs))))
            }
            Expr::ListComp {
                body,
                var,
                iter,
                filter,
                ..
            } => {
                let it = self.eval(iter)?.into_value()?;
                let mut results = Vec::new();
                match it {
                    Value::Array(items) => {
                        for item in *items {
                            let scope = Env::with_parent(&self.env);
                            scope.borrow_mut().define(&var.name, item);
                            let prev = std::mem::replace(&mut self.env, scope);
                            let dominated = if let Some(f) = filter {
                                let cond = self.eval(f)?.into_value()?;
                                matches!(cond, Value::Bool(true))
                            } else {
                                true
                            };
                            if dominated {
                                let v = self.eval(body)?.into_value()?;
                                results.push(v);
                            }
                            self.env = prev;
                        }
                    }
                    Value::Range(r) => {
                        let (start, end, step) = (r.start, r.end, r.step);
                        let mut i = start;
                        if step > 0 {
                            while i < end {
                                let scope = Env::with_parent(&self.env);
                                scope.borrow_mut().define(&var.name, Value::Int(i));
                                let prev = std::mem::replace(&mut self.env, scope);
                                let dominated = if let Some(f) = filter {
                                    let cond = self.eval(f)?.into_value()?;
                                    matches!(cond, Value::Bool(true))
                                } else {
                                    true
                                };
                                if dominated {
                                    let v = self.eval(body)?.into_value()?;
                                    results.push(v);
                                }
                                self.env = prev;
                                i += step;
                            }
                        } else if step < 0 {
                            while i > end {
                                let scope = Env::with_parent(&self.env);
                                scope.borrow_mut().define(&var.name, Value::Int(i));
                                let prev = std::mem::replace(&mut self.env, scope);
                                let dominated = if let Some(f) = filter {
                                    let cond = self.eval(f)?.into_value()?;
                                    matches!(cond, Value::Bool(true))
                                } else {
                                    true
                                };
                                if dominated {
                                    let v = self.eval(body)?.into_value()?;
                                    results.push(v);
                                }
                                self.env = prev;
                                i += step;
                            }
                        }
                    }
                    other => {
                        return Err(EvalError::new(
                            format!("cannot iterate a value of type `{other}`"),
                            iter.span(),
                        ));
                    }
                }
                Ok(Flow::Value(Value::Array(Box::new(results))))
            }
            Expr::Dict { entries, .. } => {
                let mut pairs = Vec::with_capacity(entries.len());
                for (k, v) in entries {
                    let kv = self.eval(k)?.into_value()?;
                    let vv = self.eval(v)?.into_value()?;
                    pairs.push((kv, vv));
                }
                Ok(Flow::Value(Value::Dict(Box::new(pairs))))
            }
            Expr::Variant { name, arg, span } => {
                let av = match arg {
                    Some(a) => Some(self.eval(a)?.into_value()?),
                    None => None,
                };
                match (name.as_str(), av) {
                    ("ok", Some(v)) => Ok(Flow::Value(Value::Result(Box::new(Ok(v))))),
                    ("ok", None) => Err(EvalError::new("`.ok` requires an argument", *span)),
                    ("err", Some(v)) => Ok(Flow::Value(Value::Result(Box::new(Err(v))))),
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
                object_field(&v, name, *span).map(Flow::Value)
            }
            Expr::Range { start, end, span } => {
                let s = self.eval(start)?.into_value()?;
                let e = self.eval(end)?.into_value()?;
                match (s, e) {
                    (Value::Int(a), Value::Int(b)) => {
                        Ok(Flow::Value(Value::Range(Box::new(RangeValue {
                            start: a,
                            end: b,
                            step: 1,
                        }))))
                    }
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
                Ok(Flow::Value(Value::Object(Box::new(ObjectValue {
                    name: name.clone(),
                    fields: out,
                }))))
            }
            Expr::Index { obj, index, span } => {
                let ov = self.eval(obj)?.into_value()?;
                let iv = self.eval(index)?.into_value()?;
                get_index(&ov, &iv, *span).map(Flow::Value)
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
                slice_value(&ov, s, e, *span).map(Flow::Value)
            }
        }
    }

    pub(crate) fn eval_block(&mut self, block: &Block) -> Result<Flow, EvalError> {
        let scope = Env::with_parent(&self.env);
        let prev = std::mem::replace(&mut self.env, scope);
        let mut result = Flow::Value(Value::Unit);
        for stmt in &block.stmts {
            result = self.run_stmt(stmt)?;
            if matches!(result, Flow::Return(_) | Flow::Break(_) | Flow::Continue(_)) {
                break;
            }
        }
        self.env = prev;
        Ok(result)
    }

    pub(crate) fn match_pattern(
        &self,
        pat: &Pattern,
        value: &Value,
        scope: &Rc<RefCell<Env>>,
    ) -> bool {
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
                    ("ok", Value::Result(r)) => match &**r {
                        Ok(v) => Some(v),
                        Err(_) => None,
                    },
                    ("err", Value::Result(r)) => match &**r {
                        Err(e) => Some(e),
                        Ok(_) => None,
                    },
                    _ => return false,
                };
                match (arg.as_deref(), inner) {
                    (Some(p), Some(v)) => self.match_pattern(p, v, scope),
                    (None, None) => true,
                    _ => false,
                }
            }
            Pattern::Tuple { pats, .. } => {
                if let Value::Array(items) = value {
                    if pats.len() != items.len() {
                        return false;
                    }
                    for (pat, item) in pats.iter().zip(items.iter()) {
                        if !self.match_pattern(pat, item, scope) {
                            return false;
                        }
                    }
                    true
                } else {
                    false
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
                    Some(entry) => (entry.f)(self, &mut args, span),
                    None => Err(EvalError::new(
                        format!("unknown native function `{}`", nf.name),
                        span,
                    )),
                }
            }
            Value::Func(fv) => self.call_func(*fv, args, span),
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
        let result = match &fv.chunk {
            Some(chunk) => {
                let mut vm = crate::vm::Vm::new();
                for p in &fv.params {
                    vm.push(self.env.borrow().get(&p.name.name).unwrap().clone());
                }
                vm.run_chunk_with_base(chunk, self, 0)
            }
            None => {
                self.defer_stacks.push(Vec::new());
                let r = self.eval(&fv.body);
                let defers = self.defer_stacks.pop().unwrap();
                for closure in defers.into_iter().rev() {
                    let _ = self.call(closure, vec![], span)?;
                }
                r
            }
        };
        self.env = prev;
        match result? {
            Flow::Value(v) => Ok(v),
            Flow::Return(v) => Ok(v),
            Flow::Break(span) => Err(EvalError::new("`break` outside of a loop", span)),
            Flow::Continue(span) => Err(EvalError::new("`continue` outside of a loop", span)),
        }
    }
}
