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

use zz_diag_core::{error_at, warning_at};
use zz_span_ast::span::Span;

/// One validated registration, kept for dup/shape checks.
#[derive(Debug, Clone)]
pub struct HttpRoute {
    pub method: String,
    pub pattern: String,
    pub span: Span,
}

/// Which route-registering call was seen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RouteCall {
    /// `route_get/post/put/delete` — method fixed, path is first user arg.
    PerMethod { method: &'static str },
    /// `route` — method is a string-literal user arg.
    Generic,
    /// `hijack` — upgrade route, `handler(req, tcp.stream)`, arity 2.
    Hijack,
}

/// Classify a qualified callee name (`http.*` and `std.http.*` spellings).
pub fn route_call_kind(name: &str) -> Option<RouteCall> {
    let (_, short) = name
        .rsplit_once('.')
        .map_or((None, name), |(n, s)| (Some(n), s));
    match short {
        "route_get" => Some(RouteCall::PerMethod { method: "GET" }),
        "route_post" => Some(RouteCall::PerMethod { method: "POST" }),
        "route_put" => Some(RouteCall::PerMethod { method: "PUT" }),
        "route_delete" => Some(RouteCall::PerMethod { method: "DELETE" }),
        "route" => Some(RouteCall::Generic),
        "hijack" => Some(RouteCall::Hijack),
        _ => None,
    }
}

/// Bare method name on an `HttpServer` receiver (`s.route_get(...)`).
pub fn is_route_method(method: &str) -> bool {
    matches!(
        method,
        "route_get" | "route_post" | "route_put" | "route_delete" | "route" | "hijack"
    )
}

/// Lint table tag for hijack routes: separate namespace from `GET` so a
/// normal route and an upgrade on one path coexist without dup errors.
pub const HIJACK_METHOD: &str = "HIJACK";

/// Is this a `param` lookup (`http.param` / `std.http.param` / `req.param`)?
/// (Method-form receivers are checked by call-site type; this covers the
/// qualified spellings.)
pub fn is_param_call(qualified: &str, method: &str) -> bool {
    method == "param"
        && (qualified == "http.param" || qualified == "std.http.param" || qualified == "param")
}

/// Valid HTTP methods for `http.route(server, method, ...)`.
pub const ROUTE_METHODS: &[&str] = &["GET", "POST", "PUT", "DELETE", "PATCH", "HEAD", "OPTIONS"];

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
pub fn validate_pattern(pattern: &str) -> Result<Vec<String>, String> {
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

/// Precedence score mirroring the runtime router
/// (`zz_stdlib/.../http/router.rs::match_score`): more literals wins,
/// params beat wildcards. First-registered wins ties, so an earlier
/// route shadows a later one exactly when every path the later route
/// matches is also matched by the earlier one AND the earlier score is
/// greater or equal.
fn route_score(pattern: &str) -> (usize, usize, usize) {
    if pattern == "*" {
        return (0, 0, 1);
    }
    let Some(segs) = parse_segments(pattern) else {
        // Unvalidated here only if the caller skipped validation; sort
        // lowest so it can never shadow (shapes_collide is false first).
        return (0, 0, 0);
    };
    let mut literals = 0;
    let mut params = 0;
    let mut wildcards = 0;
    for seg in &segs {
        match seg {
            Seg::Lit(_) => literals += 1,
            Seg::Param => params += 1,
            Seg::Wild => wildcards += 1,
        }
    }
    (literals, params, wildcards)
}

/// True when every request path `later` matches is also matched by
/// `earlier` (both validated patterns or whole-pattern `*`).
/// Segment compatibility mirrors `router.rs::match_pattern`: a literal
/// accepts only itself, a param accepts any single segment, a greedy
/// tail (or `*`) accepts anything from its position on.
fn matches_subset(later: &str, earlier: &str) -> bool {
    if earlier == "*" {
        return true;
    }
    if later == "*" {
        return false; // `earlier` is not `*` here
    }
    let (Some(el), Some(ll)) = (parse_segments(earlier), parse_segments(later)) else {
        return false;
    };
    let e_wild = matches!(el.last(), Some(Seg::Wild));
    let l_wild = matches!(ll.last(), Some(Seg::Wild));
    // Length-set inclusion: fixed patterns match one arity, a greedy
    // tail matches its prefix length and above.
    let lens_ok = match (l_wild, e_wild) {
        (false, false) => ll.len() == el.len(),
        (false, true) => ll.len() >= el.len() - 1,
        (true, false) => false,
        (true, true) => ll.len() >= el.len(),
    };
    if !lens_ok {
        return false;
    }
    // Every fixed position of `earlier` must accept `later`'s language
    // there; a greedy tail absorbs the rest unconditionally.
    let fixed = if e_wild { el.len() - 1 } else { el.len() };
    for (i, e) in el.iter().take(fixed).enumerate() {
        match e {
            Seg::Lit(x) => {
                if !matches!(ll.get(i), Some(Seg::Lit(y)) if y == x) {
                    return false;
                }
            }
            Seg::Param => {
                if !matches!(ll.get(i), Some(Seg::Lit(_)) | Some(Seg::Param)) {
                    return false;
                }
            }
            Seg::Wild => {}
        }
    }
    true
}

/// True when `prev` (registered earlier, same method) genuinely shadows
/// `pattern` under runtime dispatch: the shapes overlap, every path
/// `pattern` matches is also matched by `prev`, and `prev` wins or ties
/// there — so `pattern` can never serve a request.
fn shadows(prev: &str, pattern: &str) -> bool {
    shapes_collide(prev, pattern)
        && matches_subset(pattern, prev)
        && route_score(prev) >= route_score(pattern)
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
pub struct HttpLintState {
    /// Server-var root ident → registrations (insertion order).
    pub routes: HashMap<String, Vec<HttpRoute>>,
    /// Union of param names seen so far (for `param()` typo hints).
    pub params: HashSet<String>,
}

impl HttpLintState {
    /// Record a validated route; emits dup/shape diagnostics.
    /// Returns the param names for union bookkeeping.
    pub fn record(
        &mut self,
        root: Option<String>,
        method: &str,
        pattern: &str,
        params: Vec<String>,
        span: Span,
        push_diag: &mut impl FnMut(zz_diag_core::RawDiag),
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
                        zz_diag_core::SecondaryLabel {
                            span: prev.span,
                            message: "first registered here".to_string(),
                        },
                    ),
                );
                return;
            }
            // Same colliding shape, different patterns (`/:a` vs `/:b`):
            // the first wins every request, so the second is dead — warn.
            // Precedence-aware (#490): only when the earlier route
            // genuinely shadows the later one under runtime dispatch
            // (`shadows`), not across exact/param/wildcard classes where
            // both routes stay live.
            if prev.method == method && prev.pattern != pattern && shadows(&prev.pattern, pattern) {
                push_diag(
                    warning_at(
                        format!(
                            "route `{method} {pattern}` has the same shape as `{method} {}` \
                             — the first registration wins, this one never matches",
                            prev.pattern
                        ),
                        span,
                    )
                    .with_secondary(zz_diag_core::SecondaryLabel {
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
    fn shadows_mirrors_runtime_precedence() {
        // True shadowing: same match set, earlier wins ties.
        assert!(shadows("/:a", "/:b"));
        assert!(shadows("/a/:x...", "/a/:y..."));
        assert!(shadows("/a/b", "/a/b")); // dup-shaped (dup itself errors first)
                                          // #490 false positives: exact vs param, either order — the exact
                                          // route wins its path by score, the param route serves the rest.
        assert!(!shadows("/api/pkg/search", "/api/pkg/:name"));
        assert!(!shadows("/api/pkg/:name", "/api/pkg/search"));
        // Param vs greedy tail and catch-alls stay live.
        assert!(!shadows("/files/:p...", "/files/a/b"));
        assert!(!shadows("/files/a/b", "/files/:p..."));
        assert!(!shadows("*", "/a/b"));
        assert!(!shadows("/a/b", "*"));
        assert!(!shadows("/a/:x", "*"));
        // Disjoint shapes never shadow.
        assert!(!shadows("/a/:x", "/b/:x"));
        assert!(!shadows("/a/:x", "/a/b/c"));
        // A longer fixed pattern is not shadowed by a shorter prefix.
        assert!(!shadows("/a", "/a/b"));
    }

    #[test]
    fn record_warns_only_on_true_shadow() {
        fn warnings_for(patterns: &[&str]) -> Vec<String> {
            let mut state = HttpLintState::default();
            let mut out = Vec::new();
            for p in patterns {
                state.record(
                    Some("s".to_string()),
                    "GET",
                    p,
                    Vec::new(),
                    Span::new(0, 0),
                    &mut |d| out.push(d.message),
                );
            }
            out
        }
        // Issue #490 repro: exact, param, catch-all coexist silently.
        assert_eq!(
            warnings_for(&["/api/pkg/search", "/api/pkg/:name", "*"]),
            Vec::<String>::new()
        );
        // Genuine shadow still warns and names both routes.
        let warns = warnings_for(&["/:a", "/:b"]);
        assert_eq!(warns.len(), 1);
        assert!(
            warns[0].contains("/:b") && warns[0].contains("/:a"),
            "{warns:?}"
        );
    }

    #[test]
    fn route_methods_classified() {
        assert!(route_call_kind("http.route_get").is_some());
        assert!(route_call_kind("std.http.route").is_some());
        assert!(route_call_kind("s.route").is_some());
        assert!(route_call_kind("http.param").is_none());
        assert!(is_route_method("route_get"));
        assert!(is_route_method("route"));
        assert!(is_route_method("hijack"));
        assert_eq!(route_call_kind("http.hijack"), Some(RouteCall::Hijack));
        assert!(!is_route_method("test"));
        assert!(ROUTE_METHODS.contains(&"PATCH"));
        assert!(!ROUTE_METHODS.contains(&"FROB"));
    }
}
