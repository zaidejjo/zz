//! Explicit, compile-time function decorators.
//!
//! A decorator is zero-magic syntactic sugar resolved entirely before type
//! checking. `@dec func foo(params) -> ret { body }` lowers to:
//!
//! ```text
//! func foo__inner(params) -> ret { body }
//! func foo(params) -> ret { return dec(foo__inner)(params...); }
//! ```
//!
//! With arguments, the target is passed explicitly first: `@route("/p")`
//! becomes `route(foo__inner, "/p")`, so decorators stay ordinary functions
//! like `func route(f: func(str) -> str, path: str)`. (A factory returning a
//! decorator also works via a binding: `login := route("/p")`, then
//! `@login func ...`.)
//! Multiple decorators apply bottom-up (`@a` over `@b` becomes `a(b(f))`).
//! The wrapper keeps the original name, signature, and visibility, so call
//! sites, recursion, named arguments, and the C backend (which sees two
//! ordinary functions plus ordinary calls) work unchanged. Type checking
//! happens on the expanded calls, which enforces decorator/target
//! compatibility with standard mismatch diagnostics.

use std::collections::HashSet;

use crate::ast::{Block, Decorator, Expr, Program, Stmt};
use crate::diag::{error_at, RawDiag};
use crate::span::Span;

/// Expand all decorated functions in `program`.
///
/// Idempotent: output functions carry empty `decorators`, so re-running is a
/// no-op. Returns the expanded program plus any expansion diagnostics
/// (e.g. decorators on generic functions, which are rejected).
pub fn expand_program(program: &Program) -> (Program, Vec<RawDiag>) {
    let mut taken = HashSet::new();
    for stmt in &program.stmts {
        match stmt {
            Stmt::Func { name, .. } => {
                taken.insert(name.join("."));
            }
            Stmt::Impl { name, methods, .. } => {
                let t = name.join(".");
                for m in methods {
                    if let Stmt::Func { name: mn, .. } = m {
                        taken.insert(format!("{t}.{}", mn.join(".")));
                    }
                }
            }
            _ => {}
        }
    }
    let mut errors = Vec::new();
    let stmts = expand_stmts(&program.stmts, &mut taken, &mut errors);
    (
        Program {
            stmts,
            span: program.span,
        },
        errors,
    )
}

fn expand_stmts(
    stmts: &[Stmt],
    taken: &mut HashSet<String>,
    errors: &mut Vec<RawDiag>,
) -> Vec<Stmt> {
    let mut out = Vec::with_capacity(stmts.len());
    for stmt in stmts {
        match stmt {
            Stmt::Func {
                name,
                generics,
                params,
                ret,
                body,
                span,
                pub_,
                decorators,
            } => {
                if decorators.is_empty() {
                    out.push(Stmt::Func {
                        name: name.clone(),
                        generics: generics.clone(),
                        params: params.clone(),
                        ret: ret.clone(),
                        body: expand_block(body, taken, errors),
                        span: *span,
                        pub_: *pub_,
                        decorators: Vec::new(),
                    });
                    continue;
                }
                if name.is_empty() || name.iter().all(String::is_empty) {
                    // Recovery stub from parse errors; leave for later passes.
                    out.push(stmt.clone());
                    continue;
                }
                if !generics.is_empty() {
                    errors.push(error_at(
                        "decorators cannot be applied to generic functions \
                         (decorate a concrete wrapper instead)",
                        *span,
                    ));
                    // Keep the original (still decorated) so the checker can
                    // emit a matching error instead of silently unwrapping.
                    out.push(stmt.clone());
                    continue;
                }
                let full = name.join(".");
                let inner_name = unique_inner(taken, &full);
                taken.insert(inner_name.clone());
                let inner_parts: Vec<String> = inner_name.split('.').map(str::to_string).collect();
                let expanded_body = expand_block(body, taken, errors);
                out.push(Stmt::Func {
                    name: inner_parts,
                    generics: Vec::new(),
                    params: params.clone(),
                    ret: ret.clone(),
                    body: expanded_body,
                    span: *span,
                    pub_: false,
                    decorators: Vec::new(),
                });
                out.push(wrapper_func(
                    name.clone(),
                    params,
                    ret,
                    decorators,
                    &inner_name,
                    *span,
                    *pub_,
                ));
            }
            Stmt::Impl {
                name,
                methods,
                span,
                pub_,
            } => {
                let tname = name.join(".");
                let mut sibling: HashSet<String> = methods
                    .iter()
                    .filter_map(|m| match m {
                        Stmt::Func { name: mn, .. } => Some(mn.join(".")),
                        _ => None,
                    })
                    .collect();
                let mut expanded_methods = Vec::with_capacity(methods.len());
                for m in methods {
                    match m {
                        Stmt::Func {
                            name: mn,
                            generics,
                            params,
                            ret,
                            body,
                            span: mspan,
                            pub_: mpub,
                            decorators,
                        } => {
                            if decorators.is_empty() {
                                expanded_methods.push(Stmt::Func {
                                    name: mn.clone(),
                                    generics: generics.clone(),
                                    params: params.clone(),
                                    ret: ret.clone(),
                                    body: expand_block(body, taken, errors),
                                    span: *mspan,
                                    pub_: *mpub,
                                    decorators: Vec::new(),
                                });
                                continue;
                            }
                            if !generics.is_empty() {
                                errors.push(error_at(
                                    "decorators cannot be applied to generic functions \
                                     (decorate a concrete wrapper instead)",
                                    *mspan,
                                ));
                                expanded_methods.push(m.clone());
                                continue;
                            }
                            let short = mn.join(".");
                            let mut inner_short = format!("{short}__inner");
                            let mut n = 2;
                            while sibling.contains(&inner_short) {
                                inner_short = format!("{short}__inner{n}");
                                n += 1;
                            }
                            sibling.insert(inner_short.clone());
                            taken.insert(format!("{tname}.{inner_short}"));
                            expanded_methods.push(Stmt::Func {
                                name: vec![inner_short.clone()],
                                generics: Vec::new(),
                                params: params.clone(),
                                ret: ret.clone(),
                                body: expand_block(body, taken, errors),
                                span: *mspan,
                                pub_: false,
                                decorators: Vec::new(),
                            });
                            // Inside `impl`, methods live under `Type.method`,
                            // so the wrapper references the inner method by
                            // its qualified path (`Point.label__inner`).
                            let qualified = format!("{tname}.{inner_short}");
                            expanded_methods.push(wrapper_func(
                                mn.clone(),
                                params,
                                ret,
                                decorators,
                                &qualified,
                                *mspan,
                                *mpub,
                            ));
                        }
                        other => expanded_methods.push(other.clone()),
                    }
                }
                out.push(Stmt::Impl {
                    name: name.clone(),
                    methods: expanded_methods,
                    span: *span,
                    pub_: *pub_,
                });
            }
            Stmt::For {
                vars,
                iter,
                body,
                span,
            } => out.push(Stmt::For {
                vars: vars.clone(),
                iter: Box::new(expand_expr(iter, taken, errors)),
                body: expand_block(body, taken, errors),
                span: *span,
            }),
            Stmt::Decl {
                ty,
                name,
                value,
                span,
                pub_,
                is_const,
            } => out.push(Stmt::Decl {
                ty: ty.clone(),
                name: name.clone(),
                value: expand_expr(value, taken, errors),
                span: *span,
                pub_: *pub_,
                is_const: *is_const,
            }),
            Stmt::Assign {
                target,
                value,
                span,
            } => out.push(Stmt::Assign {
                target: expand_expr(target, taken, errors),
                value: expand_expr(value, taken, errors),
                span: *span,
            }),
            Stmt::Destructure { pat, value, span } => out.push(Stmt::Destructure {
                pat: pat.clone(),
                value: expand_expr(value, taken, errors),
                span: *span,
            }),
            Stmt::Return { value, span } => out.push(Stmt::Return {
                value: value.as_ref().map(|v| expand_expr(v, taken, errors)),
                span: *span,
            }),
            Stmt::Defer { expr, span } => out.push(Stmt::Defer {
                expr: Box::new(expand_expr(expr, taken, errors)),
                span: *span,
            }),
            Stmt::Expr(e) => out.push(Stmt::Expr(expand_expr(e, taken, errors))),
            other => out.push(other.clone()),
        }
    }
    out
}

fn expand_block(block: &Block, taken: &mut HashSet<String>, errors: &mut Vec<RawDiag>) -> Block {
    Block {
        stmts: expand_stmts(&block.stmts, taken, errors),
        span: block.span,
    }
}

/// Build the wrapper `func <name>(params) -> ret { return <chain>(params...); }`.
fn wrapper_func(
    name: Vec<String>,
    params: &[crate::ast::Param],
    ret: &Option<crate::ast::Ty>,
    decorators: &[Decorator],
    inner_name: &str,
    span: Span,
    pub_: bool,
) -> Stmt {
    let mut chain = path_expr(inner_name, span);
    // Bottom-up: the decorator closest to `func` applies first.
    for dec in decorators.iter().rev() {
        let base = path_expr(&dec.path.join("."), dec.span);
        // Explicit target-first convention: `@d(a, b)` is `d(target, a, b)`.
        // This keeps decorators ordinary typed functions — no factory
        // closures with unparseable nested `func` annotations required.
        let mut call_args = vec![chain];
        call_args.extend(dec.args.iter().cloned());
        let join = dec
            .span
            .join(call_args.last().map(|e| e.span()).unwrap_or(dec.span));
        chain = Expr::Call {
            callee: Box::new(base),
            args: call_args,
            named: dec.named.clone(),
            span: join,
        };
    }
    let forward: Vec<Expr> = params
        .iter()
        .map(|p| Expr::Ident {
            name: p.name.name.clone(),
            span: p.name.span,
        })
        .collect();
    let call_span = chain.span().join(span);
    let call = Expr::Call {
        callee: Box::new(chain),
        args: forward,
        named: Vec::new(),
        span: call_span,
    };
    let body = Block {
        stmts: vec![Stmt::Return {
            value: Some(call),
            span: call_span,
        }],
        span: call_span,
    };
    Stmt::Func {
        name,
        generics: Vec::new(),
        params: params.to_vec(),
        ret: ret.clone(),
        body,
        span,
        pub_,
        decorators: Vec::new(),
    }
}

fn path_expr(dotted: &str, span: Span) -> Expr {
    let parts: Vec<String> = dotted.split('.').map(str::to_string).collect();
    if parts.len() == 1 {
        Expr::Ident {
            name: parts.into_iter().next().unwrap_or_default(),
            span,
        }
    } else {
        Expr::Path { parts, span }
    }
}

fn unique_inner(taken: &HashSet<String>, full: &str) -> String {
    let mut candidate = format!("{full}__inner");
    let mut n = 2;
    while taken.contains(&candidate) {
        candidate = format!("{full}__inner{n}");
        n += 1;
    }
    candidate
}

#[allow(clippy::only_used_in_recursion)]
fn expand_expr(expr: &Expr, taken: &mut HashSet<String>, errors: &mut Vec<RawDiag>) -> Expr {
    match expr {
        Expr::Block(b) => Expr::Block(expand_block(b, taken, errors)),
        Expr::Paren { expr: inner, span } => Expr::Paren {
            expr: Box::new(expand_expr(inner, taken, errors)),
            span: *span,
        },
        Expr::Unary {
            op,
            expr: inner,
            span,
        } => Expr::Unary {
            op: *op,
            expr: Box::new(expand_expr(inner, taken, errors)),
            span: *span,
        },
        Expr::Binary {
            op,
            left,
            right,
            span,
        } => Expr::Binary {
            op: *op,
            left: Box::new(expand_expr(left, taken, errors)),
            right: Box::new(expand_expr(right, taken, errors)),
            span: *span,
        },
        Expr::Call {
            callee,
            args,
            named,
            span,
        } => Expr::Call {
            callee: Box::new(expand_expr(callee, taken, errors)),
            args: args.iter().map(|a| expand_expr(a, taken, errors)).collect(),
            named: named
                .iter()
                .map(|(n, v)| (n.clone(), expand_expr(v, taken, errors)))
                .collect(),
            span: *span,
        },
        Expr::Closure {
            params,
            ret_ty,
            body,
            span,
        } => Expr::Closure {
            params: params.clone(),
            ret_ty: ret_ty.clone(),
            body: Box::new(expand_expr(body, taken, errors)),
            span: *span,
        },
        Expr::If {
            cond,
            then,
            els,
            span,
        } => Expr::If {
            cond: Box::new(expand_expr(cond, taken, errors)),
            then: expand_block(then, taken, errors),
            els: els
                .as_ref()
                .map(|e| Box::new(expand_expr(e, taken, errors))),
            span: *span,
        },
        Expr::While { cond, body, span } => Expr::While {
            cond: Box::new(expand_expr(cond, taken, errors)),
            body: expand_block(body, taken, errors),
            span: *span,
        },
        Expr::Match {
            scrutinee,
            arms,
            span,
        } => Expr::Match {
            scrutinee: Box::new(expand_expr(scrutinee, taken, errors)),
            arms: arms
                .iter()
                .map(|a| crate::ast::MatchArm {
                    pat: a.pat.clone(),
                    guard: a.guard.as_ref().map(|g| expand_expr(g, taken, errors)),
                    body: expand_expr(&a.body, taken, errors),
                    span: a.span,
                })
                .collect(),
            span: *span,
        },
        Expr::IfLet {
            pat,
            value,
            then,
            els,
            span,
        } => Expr::IfLet {
            pat: pat.clone(),
            value: Box::new(expand_expr(value, taken, errors)),
            then: expand_block(then, taken, errors),
            els: els
                .as_ref()
                .map(|e| Box::new(expand_expr(e, taken, errors))),
            span: *span,
        },
        Expr::Try { expr: inner, span } => Expr::Try {
            expr: Box::new(expand_expr(inner, taken, errors)),
            span: *span,
        },
        Expr::Tuple { items, span } => Expr::Tuple {
            items: items
                .iter()
                .map(|i| expand_expr(i, taken, errors))
                .collect(),
            span: *span,
        },
        Expr::Array { elems, span } => Expr::Array {
            elems: elems
                .iter()
                .map(|e| expand_expr(e, taken, errors))
                .collect(),
            span: *span,
        },
        Expr::Dict { entries, span } => Expr::Dict {
            entries: entries
                .iter()
                .map(|(k, v)| (expand_expr(k, taken, errors), expand_expr(v, taken, errors)))
                .collect(),
            span: *span,
        },
        Expr::Fmt { parts, span } => Expr::Fmt {
            parts: parts
                .iter()
                .map(|p| match p {
                    crate::ast::FmtPart::Text(t) => crate::ast::FmtPart::Text(t.clone()),
                    crate::ast::FmtPart::Expr(e, s) => crate::ast::FmtPart::Expr(
                        Box::new(expand_expr(e, taken, errors)),
                        s.clone(),
                    ),
                })
                .collect(),
            span: *span,
        },
        Expr::Field { obj, name, span } => Expr::Field {
            obj: Box::new(expand_expr(obj, taken, errors)),
            name: name.clone(),
            span: *span,
        },
        Expr::Index { obj, index, span } => Expr::Index {
            obj: Box::new(expand_expr(obj, taken, errors)),
            index: Box::new(expand_expr(index, taken, errors)),
            span: *span,
        },
        Expr::Slice {
            obj,
            start,
            end,
            span,
        } => Expr::Slice {
            obj: Box::new(expand_expr(obj, taken, errors)),
            start: start
                .as_ref()
                .map(|s| Box::new(expand_expr(s, taken, errors))),
            end: end
                .as_ref()
                .map(|e| Box::new(expand_expr(e, taken, errors))),
            span: *span,
        },
        Expr::Range { start, end, span } => Expr::Range {
            start: Box::new(expand_expr(start, taken, errors)),
            end: Box::new(expand_expr(end, taken, errors)),
            span: *span,
        },
        Expr::StructInit { name, fields, span } => Expr::StructInit {
            name: name.clone(),
            fields: fields
                .iter()
                .map(|(n, v)| (n.clone(), expand_expr(v, taken, errors)))
                .collect(),
            span: *span,
        },
        Expr::Variant { name, arg, span } => Expr::Variant {
            name: name.clone(),
            arg: arg
                .as_ref()
                .map(|a| Box::new(expand_expr(a, taken, errors))),
            span: *span,
        },
        Expr::ListComp {
            body,
            var,
            iter,
            filter,
            span,
        } => Expr::ListComp {
            body: Box::new(expand_expr(body, taken, errors)),
            var: var.clone(),
            iter: Box::new(expand_expr(iter, taken, errors)),
            filter: filter
                .as_ref()
                .map(|f| Box::new(expand_expr(f, taken, errors))),
            span: *span,
        },
        other => other.clone(),
    }
}
