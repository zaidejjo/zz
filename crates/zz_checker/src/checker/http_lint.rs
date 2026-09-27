//! Compile-time lint for `std.http` route registration (Phase 2.2).
//!
//! Go reports route mistakes at runtime (404s, silent shadowing). ZZ rejects
//! them at `zz check`: malformed paths, unknown methods, wrong handler
//! arity, duplicate routes, and `param("typo")` hints.
//!
//! Design notes:
//! - Only plain string literals are validated. Interpolated (`Fmt`) or
//!   computed paths skip silently — no false positives on dynamic routes.
//! - Route tables are tracked per server-variable root ident with
//!   inheritance across `:=` / `=` chains (`s2 := s.route_get(...)`),
//!   mirroring how fixtures build servers. Anything dynamic degrades to
//!   unscoped checks (shape only), never to noise.
//! - `param()` hints consult the union of params seen so far (single pass,
//!   best effort) and only fire as warnings.

use std::collections::{HashMap, HashSet};

use zz_frontend::ast::Expr;
use zz_frontend::diag::{error_at, warning_at};
use zz_frontend::levenshtein::suggest_all;
use zz_frontend::span::Span;

use crate::checker::Checker;
use crate::type_::Type;

/// One validated registration, kept for dup/shape checks.
#[derive(Debug, Clone)]
pub(crate) struct HttpRoute {
    pub(crate) method: String,
    pub(crate) pattern: String,
    pub(crate) span: Span,
}

/// Which route-registering call was seen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RouteCall {
    /// `route_get/post/put/delete` — method fixed, path is first user arg.
    PerMethod { method: &'static str },
    /// `route` — method is a string-literal user arg.
    Generic,
}

/// Classify a qualified callee name (`http.*` and `std.http.*` spellings).
pub(crate) fn route_call_kind(name: &str) -> Option<RouteCall> {
    let (_, short) = name
        .rsplit_once('.')
        .map_or((None, name), |(n, s)| (Some(n), s));
    match short {
        "route_get" => Some(RouteCall::PerMethod { method: "GET" }),
        "route_post" => Some(RouteCall::PerMethod { method: "POST" }),
        "route_put" => Some(RouteCall::PerMethod { method: "PUT" }),
        "route_delete" => Some(RouteCall::PerMethod { method: "DELETE" }),
        "route" => Some(RouteCall::Generic),
        _ => None,
    }
}

/// Bare method name on an `HttpServer` receiver (`s.route_get(...)`).
pub(crate) fn is_route_method(method: &str) -> bool {
    matches!(
        method,
        "route_get" | "route_post" | "route_put" | "route_delete" | "route"
    )
}

/// Is this a `param` lookup (`http.param` / `std.http.param` / `req.param`)?
/// (Method-form receivers are checked by call-site type; this covers the
/// qualified spellings.)
pub(crate) fn is_param_call(qualified: &str, method: &str) -> bool {
    method == "param"
        && (qualified == "http.param" || qualified == "std.http.param" || qualified == "param")
}

/// Valid HTTP methods for `http.route(server, method, ...)`.
pub(crate) const ROUTE_METHODS: &[&str] =
    &["GET", "POST", "PUT", "DELETE", "PATCH", "HEAD", "OPTIONS"];

fn valid_param_name(name: &str) -> bool {
    let mut chars = name.chars();
    match chars.next() {
        Some(c) if c.is_ascii_alphabetic() || c == '_' => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// Validate a route pattern. Returns captured param names.
///
/// Mirrors `zz_stdlib/.../http/router.rs::compile_pattern` — the two must
/// agree, or the checker accepts what the runtime rejects.
pub(crate) fn validate_pattern(pattern: &str) -> Result<Vec<String>, String> {
    if pattern == "*" {
        return Ok(Vec::new());
    }
    if pattern.contains('{') || pattern.contains('}') {
        return Err(format!(
            "route path `{pattern}` uses `{{...}}`, which interpolates in ZZ strings; \
             use `:name` instead (e.g. `/users/:id`)"
        ));
    }
    if !pattern.starts_with('/') {
        return Err(format!(
            "route path `{pattern}` must start with `/` (or be exactly `*`)"
        ));
    }
    let trimmed = pattern.trim_matches('/');
    if trimmed.is_empty() {
        return Ok(Vec::new()); // root `/`
    }
    let segs: Vec<&str> = trimmed.split('/').collect();
    if segs.iter().any(|s| s.is_empty()) {
        return Err(format!(
            "route path `{pattern}` has an empty segment (`//`)"
        ));
    }
    let mut params = Vec::new();
    for (i, seg) in segs.iter().enumerate() {
        if *seg == "*" {
            return Err(format!(
                "route path `{pattern}`: `*` is only valid as the whole pattern; \
                 use `:name...` for a greedy tail"
            ));
        }
        if let Some(rest) = seg.strip_prefix(':') {
            if let Some(name) = rest.strip_suffix("...") {
                if i != segs.len() - 1 {
                    return Err(format!(
                        "route path `{pattern}`: greedy `:name...` must be the last segment"
                    ));
                }
                if !valid_param_name(name) {
                    return Err(format!(
                        "route path `{pattern}`: bad param name `:{name}...` \
                         (expected [A-Za-z_][A-Za-z0-9_]*)"
                    ));
                }
                params.push(name.to_string());
            } else {
                if !valid_param_name(rest) {
                    return Err(format!(
                        "route path `{pattern}`: bad param name `:{rest}` \
                         (expected [A-Za-z_][A-Za-z0-9_]*)"
                    ));
                }
                params.push(rest.to_string());
            }
        }
    }
    // At most one greedy tail is implied (it must be last, so >1 impossible
    // unless two `...` segments exist — the loop above already rejects a
    // non-final one; a second final one cannot exist).
    Ok(params)
}

/// Segment shape for collision comparison.
#[derive(Debug, Clone, PartialEq)]
enum Seg {
    Lit(String),
    Param,
    /// Greedy `:name...` tail (or whole-pattern `*`, handled separately).
    Wild,
}

fn parse_segments(pattern: &str) -> Option<Vec<Seg>> {
    if pattern.contains('{') || pattern.contains('}') {
        return None;
    }
    let trimmed = pattern.trim_matches('/');
    if trimmed.is_empty() {
        return Some(Vec::new());
    }
    let mut out = Vec::new();
    for seg in trimmed.split('/') {
        if seg.is_empty() || seg == "*" {
            return None;
        }
        if let Some(rest) = seg.strip_prefix(':') {
            if rest.strip_suffix("...").is_some() {
                out.push(Seg::Wild);
            } else {
                out.push(Seg::Param);
            }
        } else {
            out.push(Seg::Lit(seg.to_string()));
        }
    }
    Some(out)
}

/// Do two validated patterns potentially match the same request path?
/// Literals must agree per position; params match any single segment;
/// a greedy tail absorbs the rest. Whole-pattern `*` hits everything.
fn shapes_collide(a: &str, b: &str) -> bool {
    if a == "*" || b == "*" {
        return true;
    }
    let (Some(pa), Some(pb)) = (parse_segments(a), parse_segments(b)) else {
        return false;
    };
    let n = pa.len().max(pb.len());
    for i in 0..n {
        match (pa.get(i), pb.get(i)) {
            (Some(Seg::Lit(x)), Some(Seg::Lit(y))) => {
                if x != y {
                    return false;
                }
            }
            // Validated patterns keep `Wild` last, so it absorbs the rest.
            (Some(Seg::Wild), _) | (_, Some(Seg::Wild)) => return true,
            // Arity differs with no tail to absorb it.
            (None, _) | (_, None) => return false,
            // Param matches any single segment.
            _ => {}
        }
    }
    true
}

/// State kept on the `Checker` (fields live in `checker/mod.rs`).
#[derive(Debug, Default)]
pub(crate) struct HttpLintState {
    /// Server-var root ident → registrations (insertion order).
    pub(crate) routes: HashMap<String, Vec<HttpRoute>>,
    /// Union of param names seen so far (for `param()` typo hints).
    pub(crate) params: HashSet<String>,
}

impl HttpLintState {
    /// Record a validated route; emits dup/shape diagnostics.
    /// Returns the param names for union bookkeeping.
    pub(crate) fn record(
        &mut self,
        root: Option<String>,
        method: &str,
        pattern: &str,
        params: Vec<String>,
        span: Span,
        push_diag: &mut impl FnMut(zz_frontend::diag::RawDiag),
    ) {
        for p in &params {
            self.params.insert(p.clone());
        }
        let Some(root) = root else { return };
        let entry = self.routes.entry(root).or_default();
        for prev in entry.iter() {
            if prev.method == method && prev.pattern == pattern {
                push_diag(
                    error_at(format!("duplicate route `{method} {pattern}`"), span).with_secondary(
                        zz_frontend::diag::SecondaryLabel {
                            span: prev.span,
                            message: "first registered here".to_string(),
                        },
                    ),
                );
                return;
            }
            // Same colliding shape, different patterns (`/:a` vs `/:b`):
            // the first wins every request, so the second is dead — warn.
            if prev.method == method
                && prev.pattern != pattern
                && shapes_collide(&prev.pattern, pattern)
            {
                push_diag(
                    warning_at(
                        format!(
                            "route `{method} {pattern}` has the same shape as `{method} {}` \
                             — the first registration wins, this one never matches",
                            prev.pattern
                        ),
                        span,
                    )
                    .with_secondary(zz_frontend::diag::SecondaryLabel {
                        span: prev.span,
                        message: "earlier same-shape route".to_string(),
                    }),
                );
                break;
            }
        }
        entry.push(HttpRoute {
            method: method.to_string(),
            pattern: pattern.to_string(),
            span,
        });
    }
}

// ── Checker hooks ────────────────────────────────────────────────────────

/// Plain string literal text. Interpolated (`Fmt`) and computed paths
/// return `None` — dynamic routes skip the lint silently.
fn str_lit(expr: &Expr) -> Option<&str> {
    match expr {
        Expr::Str { value, .. } => Some(value.as_str()),
        _ => None,
    }
}

impl Checker {
    /// Validate one route registration and record it for dup/shape checks.
    /// `path_expr`/`handler_expr` positions depend on the call shape;
    /// non-literal paths and non-closure handlers degrade gracefully.
    fn lint_route(
        &mut self,
        root: Option<String>,
        method: &str,
        path_expr: Option<&Expr>,
        handler_expr: Option<&Expr>,
        span: Span,
    ) {
        if let Some(handler) = handler_expr {
            if let Expr::Closure { params, .. } = handler {
                if params.len() != 1 {
                    self.errors.push(error_at(
                        format!(
                            "route handler must take exactly 1 argument (the request), found {}",
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
    pub(crate) fn lint_http_route_method(
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
            self.lint_route(root, method, args.get(1), args.get(2), span);
        } else if let Some(RouteCall::PerMethod { method }) = route_call_kind(method_name) {
            self.lint_route(root, method, args.first(), args.get(1), span);
        }
    }

    /// Direct form: `http.route_get(s, path, h)` / `std.http.route(s, m, p, h)`.
    pub(crate) fn lint_http_route_direct(&mut self, kind: RouteCall, args: &[Expr], span: Span) {
        let root = match args.first() {
            Some(Expr::Ident { name, .. }) => Some(name.clone()),
            _ => None,
        };
        match kind {
            RouteCall::PerMethod { method } => {
                self.lint_route(root, method, args.get(1), args.get(2), span);
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
                self.lint_route(root, method, args.get(2), args.get(3), span);
            }
        }
    }

    /// Receiver-implicit form: `s.route_get(path, h)` / `s.route(m, p, h)`.
    /// Same validation as direct, but the server is the receiver (arg
    /// positions shift left by one).
    pub(crate) fn lint_http_route_receiver(
        &mut self,
        short: &str,
        root: String,
        args: &[Expr],
        span: Span,
    ) {
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
            self.lint_route(Some(root), method, args.get(1), args.get(2), span);
        } else if let Some(RouteCall::PerMethod { method }) = route_call_kind(short) {
            self.lint_route(Some(root), method, args.first(), args.get(1), span);
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
    pub(crate) fn lint_http_path_call(&mut self, parts: &[String], args: &[Expr], span: Span) {
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
    pub(crate) fn lint_http_param(&mut self, name_expr: Option<&Expr>) {
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
    pub(crate) fn propagate_http_routes(&mut self, target: &str, value: &Expr) {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn valid_patterns_capture_params() {
        assert_eq!(validate_pattern("/").unwrap(), Vec::<String>::new());
        assert_eq!(validate_pattern("*").unwrap(), Vec::<String>::new());
        assert_eq!(validate_pattern("/users/:id").unwrap(), vec!["id"]);
        assert_eq!(
            validate_pattern("/posts/:pid/comments/:cid").unwrap(),
            vec!["pid", "cid"]
        );
        assert_eq!(validate_pattern("/files/:path...").unwrap(), vec!["path"]);
    }

    #[test]
    fn invalid_patterns_rejected() {
        for bad in [
            "no-slash",
            "/a//b",
            "/users/{id}",
            "/users/:",
            "/users/:9bad",
            "/users/:a.../x",
            "/a/*/b",
        ] {
            assert!(validate_pattern(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn brace_hint_mentions_colon_form() {
        let err = validate_pattern("/users/{id}").unwrap_err();
        assert!(err.contains(":id") || err.contains(":name"), "{err}");
    }

    #[test]
    fn router_agrees_with_runtime() {
        // Every pattern the checker accepts must compile in the runtime
        // router (checked structurally: same `:`, `...`, `*` rules).
        for ok in ["/", "*", "/a", "/a/:b", "/a/:b...", "/a/:b/c/:d"] {
            assert!(validate_pattern(ok).is_ok(), "{ok}");
        }
    }

    #[test]
    fn collision_matches_dispatch() {
        // Same request shape → collide.
        assert!(shapes_collide("/a/:x", "/a/:y"));
        assert!(shapes_collide("/a/:x", "/a/:z"));
        assert!(shapes_collide("/files/:p...", "/files/a/b"));
        assert!(shapes_collide("*", "/anything"));
        // Different literals / arities → independent.
        assert!(!shapes_collide("/a/:x", "/b/:x"));
        assert!(!shapes_collide("/a/:x", "/a/b/c"));
        assert!(!shapes_collide("/", "/a"));
        assert!(!shapes_collide("/a/:x", "/a"));
    }

    #[test]
    fn route_methods_classified() {
        assert!(route_call_kind("http.route_get").is_some());
        assert!(route_call_kind("std.http.route").is_some());
        assert!(route_call_kind("s.route").is_some());
        assert!(route_call_kind("http.param").is_none());
        assert!(is_route_method("route_get"));
        assert!(is_route_method("route"));
        assert!(!is_route_method("test"));
        assert!(ROUTE_METHODS.contains(&"PATCH"));
        assert!(!ROUTE_METHODS.contains(&"FROB"));
    }
}
