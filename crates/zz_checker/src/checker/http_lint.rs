//! Compile-time lint for `std.http` route registration (Phase 2.2).
//!
//! Route shape validation and shadow detection live in `zz_http_routes`;
//! this module wires them into the checker's expression walk.

use zz_diag_core::{error_at, warning_at};
use zz_http_routes::{
    is_param_call, is_route_method, route_call_kind, validate_pattern, RouteCall, HIJACK_METHOD,
    ROUTE_METHODS,
};
use zz_scope::ScopeOps;
use zz_span_ast::ast::Expr;
use zz_span_ast::span::Span;
use zz_suggest::suggest_all;
use zz_types::Type;

use crate::checker::Checker;

// ── Checker hooks ────────────────────────────────────────────────────────

/// Plain string literal text. Interpolated (`Fmt`) and computed paths
/// return `None` — dynamic routes skip the lint silently.
fn str_lit(expr: &Expr) -> Option<&str> {
    match expr {
        Expr::Str { value, .. } => Some(value.as_str()),
        _ => None,
    }
}

pub trait HttpLintOps {
    /// Validate one route registration and record it for dup/shape checks.
    /// `path_expr`/`handler_expr` positions depend on the call shape;
    /// non-literal paths and non-closure handlers degrade gracefully.
    fn lint_route(
        &mut self,
        root: Option<String>,
        method: &str,
        path_expr: Option<&Expr>,
        handler_expr: Option<&Expr>,
        handler_arity: usize,
        span: Span,
    );

    /// Method form: `s.route_get(path, handler)` / `s.route(m, p, h)`.
    fn lint_http_route_method(
        &mut self,
        root: Option<String>,
        method_name: &str,
        args: &[Expr],
        span: Span,
    );

    /// Direct form: `http.route_get(s, path, h)` / `std.http.route(s, m, p, h)`.
    fn lint_http_route_direct(&mut self, kind: RouteCall, args: &[Expr], span: Span);

    /// Receiver-implicit form: `s.route_get(path, h)` / `s.route(m, p, h)`.
    /// Same validation as direct, but the server is the receiver (arg
    /// positions shift left by one).
    fn lint_http_route_receiver(&mut self, short: &str, root: String, args: &[Expr], span: Span);

    /// Is `parts[0]` a module namespace (vs a value receiver)?
    ///
    /// `http.*` / `std.*` are always module paths; otherwise only an
    /// explicitly imported alias counts. In particular a loader-namespaced
    /// local (`decorators.route` for a same-file `func route`) is NOT a
    /// module call — `lookup_opt` alone cannot tell them apart (neither
    /// root is a value binding), which false-positived on
    /// `syntax/decorators.zz`.
    fn recv_is_module(&self, parts: &[String]) -> bool;

    /// Dispatch a `Path`-callee call (`http.route_get(...)`, `s.route(...)`,
    /// `req.param(...)`) to the right lint. Module calls (`http.*`) carry
    /// the server as `args[0]`; value calls (`s.*`) carry it as receiver.
    fn lint_http_path_call(&mut self, parts: &[String], args: &[Expr], span: Span);

    /// `http.param(req, "name")` / `req.param("name")`: typo hints against
    /// every param registered so far (warning only, best effort).
    fn lint_http_param(&mut self, name_expr: Option<&Expr>);

    /// Inherit route tables across `name := <route call>` / `name = <route
    /// call>` / `name := server_alias` so chained builders stay scoped.
    fn propagate_http_routes(&mut self, target: &str, value: &Expr);
}

impl HttpLintOps for Checker {
    /// Validate one route registration and record it for dup/shape checks.
    /// `path_expr`/`handler_expr` positions depend on the call shape;
    /// non-literal paths and non-closure handlers degrade gracefully.
    fn lint_route(
        &mut self,
        root: Option<String>,
        method: &str,
        path_expr: Option<&Expr>,
        handler_expr: Option<&Expr>,
        handler_arity: usize,
        span: Span,
    ) {
        if let Some(handler) = handler_expr {
            if let Expr::Closure { params, .. } = handler {
                if params.len() != handler_arity {
                    let what = if handler_arity == 1 {
                        "the request"
                    } else {
                        "the request and the socket"
                    };
                    self.errors.push(error_at(
                        format!(
                            "route handler must take exactly {handler_arity} argument(s) ({what}), found {}",
                            params.len()
                        ),
                        handler.span(),
                    ));
                }
            }
        }
        let Some(path_expr) = path_expr else { return };
        let Some(pattern) = str_lit(path_expr) else {
            return; // dynamic path — skip silently
        };
        match validate_pattern(pattern) {
            Err(msg) => {
                self.errors.push(error_at(msg, path_expr.span()));
            }
            Ok(params) => {
                let mut diags = Vec::new();
                self.http_lint
                    .record(root, method, pattern, params, span, &mut |d| diags.push(d));
                self.errors.extend(diags);
            }
        }
    }

    /// Method form: `s.route_get(path, handler)` / `s.route(m, p, h)`.
    fn lint_http_route_method(
        &mut self,
        root: Option<String>,
        method_name: &str,
        args: &[Expr],
        span: Span,
    ) {
        if method_name == "route" {
            let method = args.first().and_then(str_lit).unwrap_or("");
            if !method.is_empty() && !ROUTE_METHODS.contains(&method) {
                self.errors.push(error_at(
                    format!(
                        "`std.http.route`: unknown method `{method}` \
                         (expected GET, POST, PUT, DELETE, PATCH, HEAD or OPTIONS)"
                    ),
                    args[0].span(),
                ));
                return;
            }
            if method.is_empty() {
                return; // dynamic method — normal checking reports arity issues
            }
            self.lint_route(root, method, args.get(1), args.get(2), 1, span);
        } else if let Some(RouteCall::PerMethod { method }) = route_call_kind(method_name) {
            self.lint_route(root, method, args.first(), args.get(1), 1, span);
        } else if route_call_kind(method_name) == Some(RouteCall::Hijack) {
            self.lint_route(root, HIJACK_METHOD, args.first(), args.get(1), 2, span);
        }
    }

    /// Direct form: `http.route_get(s, path, h)` / `std.http.route(s, m, p, h)`.
    fn lint_http_route_direct(&mut self, kind: RouteCall, args: &[Expr], span: Span) {
        let root = match args.first() {
            Some(Expr::Ident { name, .. }) => Some(name.clone()),
            _ => None,
        };
        match kind {
            RouteCall::PerMethod { method } => {
                self.lint_route(root, method, args.get(1), args.get(2), 1, span);
            }
            RouteCall::Hijack => {
                self.lint_route(root, HIJACK_METHOD, args.get(1), args.get(2), 2, span);
            }
            RouteCall::Generic => {
                let method = args.get(1).and_then(str_lit).unwrap_or("");
                if !method.is_empty() && !ROUTE_METHODS.contains(&method) {
                    self.errors.push(error_at(
                        format!(
                            "`std.http.route`: unknown method `{method}` \
                             (expected GET, POST, PUT, DELETE, PATCH, HEAD or OPTIONS)"
                        ),
                        args[1].span(),
                    ));
                    return;
                }
                if method.is_empty() {
                    return;
                }
                self.lint_route(root, method, args.get(2), args.get(3), 1, span);
            }
        }
    }

    /// Receiver-implicit form: `s.route_get(path, h)` / `s.route(m, p, h)`.
    /// Same validation as direct, but the server is the receiver (arg
    /// positions shift left by one).
    fn lint_http_route_receiver(&mut self, short: &str, root: String, args: &[Expr], span: Span) {
        if short == "route" {
            let method = args.first().and_then(str_lit).unwrap_or("");
            if !method.is_empty() && !ROUTE_METHODS.contains(&method) {
                self.errors.push(error_at(
                    format!(
                        "`std.http.route`: unknown method `{method}` \
                         (expected GET, POST, PUT, DELETE, PATCH, HEAD or OPTIONS)"
                    ),
                    args[0].span(),
                ));
                return;
            }
            if method.is_empty() {
                return;
            }
            self.lint_route(Some(root), method, args.get(1), args.get(2), 1, span);
        } else if let Some(RouteCall::PerMethod { method }) = route_call_kind(short) {
            self.lint_route(Some(root), method, args.first(), args.get(1), 1, span);
        } else if route_call_kind(short) == Some(RouteCall::Hijack) {
            self.lint_route(
                Some(root),
                HIJACK_METHOD,
                args.first(),
                args.get(1),
                2,
                span,
            );
        }
    }

    /// Is `parts[0]` a module namespace (vs a value receiver)?
    ///
    /// `http.*` / `std.*` are always module paths; otherwise only an
    /// explicitly imported alias counts. In particular a loader-namespaced
    /// local (`decorators.route` for a same-file `func route`) is NOT a
    /// module call — `lookup_opt` alone cannot tell them apart (neither
    /// root is a value binding), which false-positived on
    /// `syntax/decorators.zz`.
    fn recv_is_module(&self, parts: &[String]) -> bool {
        if parts.is_empty() {
            return false;
        }
        if parts[0] == "http" || parts[0] == "std" {
            return true;
        }
        self.imports.iter().any(|(alias, _)| alias == &parts[0])
    }

    /// Dispatch a `Path`-callee call (`http.route_get(...)`, `s.route(...)`,
    /// `req.param(...)`) to the right lint. Module calls (`http.*`) carry
    /// the server as `args[0]`; value calls (`s.*`) carry it as receiver.
    fn lint_http_path_call(&mut self, parts: &[String], args: &[Expr], span: Span) {
        if parts.len() < 2 {
            return;
        }
        let short = parts.last().unwrap().as_str();
        let recv_is_module = self.recv_is_module(parts);
        if recv_is_module {
            let qualified = parts.join(".");
            if let Some(kind) = route_call_kind(&qualified) {
                self.lint_http_route_direct(kind, args, span);
            } else if is_param_call(&qualified, "param") {
                self.lint_http_param(args.get(1));
            }
            return;
        }
        // Value method: `s.route_get(...)`, `req.param(...)`.
        let root = parts[..parts.len() - 1].join(".");
        if is_route_method(short) {
            self.lint_http_route_receiver(short, root, args, span);
        } else if short == "param" {
            let recv_t = self.lookup_opt(&root);
            if matches!(
                recv_t.map(|t| self.unifier.resolve(&t)),
                Some(Type::HttpRequest)
            ) {
                self.lint_http_param(args.first());
            }
        }
    }

    /// `http.param(req, "name")` / `req.param("name")`: typo hints against
    /// every param registered so far (warning only, best effort).
    fn lint_http_param(&mut self, name_expr: Option<&Expr>) {
        let Some(name_expr) = name_expr else { return };
        let Some(name) = str_lit(name_expr) else {
            return;
        };
        if self.http_lint.params.is_empty() || self.http_lint.params.contains(name) {
            return;
        }
        let mut candidates: Vec<String> = self.http_lint.params.iter().cloned().collect();
        candidates.sort();
        let refs: Vec<&str> = candidates.iter().map(|s| s.as_str()).collect();
        let mut msg = format!("unknown route param `{name}` (no registered route captures it)");
        if let Some((suggestion, _)) = suggest_all(name, &refs).first() {
            msg.push_str(&format!("; did you mean `{suggestion}`?"));
        }
        self.errors.push(warning_at(msg, name_expr.span()));
    }

    /// Inherit route tables across `name := <route call>` / `name = <route
    /// call>` / `name := server_alias` so chained builders stay scoped.
    fn propagate_http_routes(&mut self, target: &str, value: &Expr) {
        let root = match value {
            Expr::Call { callee, args, .. } => match callee.as_ref() {
                Expr::Field { obj, name, .. } if is_route_method(name) => match obj.as_ref() {
                    Expr::Ident { name, .. } => Some(name.clone()),
                    _ => None,
                },
                Expr::Path { parts, .. }
                    if parts.len() >= 2 && route_call_kind(&parts.join(".")).is_some() =>
                {
                    if self.recv_is_module(parts) {
                        // Module form: server is `args[0]`.
                        match args.first() {
                            Some(Expr::Ident { name, .. }) => Some(name.clone()),
                            _ => None,
                        }
                    } else {
                        // Receiver-implicit: root is the receiver path.
                        Some(parts[..parts.len() - 1].join("."))
                    }
                }
                Expr::Ident { name, .. }
                    if route_call_kind(name).is_some()
                        && self
                            .import_aliases
                            .get(name)
                            .is_some_and(|q| route_call_kind(q).is_some()) =>
                {
                    match args.first() {
                        Some(Expr::Ident { name, .. }) => Some(name.clone()),
                        _ => None,
                    }
                }
                _ => None,
            },
            Expr::Ident { name, .. } => Some(name.clone()),
            _ => return,
        };
        if let Some(root) = root {
            if root != target {
                if let Some(routes) = self.http_lint.routes.get(&root).cloned() {
                    self.http_lint.routes.insert(target.to_string(), routes);
                }
            }
        }
    }
}
