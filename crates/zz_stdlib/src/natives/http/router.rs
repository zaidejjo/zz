//! Radix-style router for `std.http` (Phase 1: Minimal Core).
//!
//! Single syntax only: `:id` matches one segment, `:name...` matches the
//! greedy tail (must be the last segment). `*` is a legacy unnamed catch-all.
//! No `{id}` — braces collide with ZZ string interpolation (`"Hello, {name}"`).
//!
//! Matching precedence: exact > param > wildcard. First-registered wins ties.
//! Method mismatches are reported so dispatch can return 405 instead of 404.

/// One compiled segment of a route pattern.
#[derive(Debug, Clone, PartialEq)]
pub enum Segment {
    /// Static text, e.g. `users`.
    Literal(String),
    /// `:id` — captures exactly one path segment.
    Param(String),
    /// `:rest...` — captures the remaining tail (possibly empty), joined by `/`.
    Wildcard(String),
}

/// A compiled route pattern.
#[derive(Debug, Clone, PartialEq)]
pub struct RoutePattern {
    pub segments: Vec<Segment>,
    /// True for the legacy `*` catch-all (matches every path).
    pub catch_all: bool,
}

/// Parse `"users"` / `":id"` / `":rest..."` segment text.
fn parse_segment(seg: &str) -> Segment {
    if let Some(rest) = seg.strip_prefix(':') {
        if let Some(name) = rest.strip_suffix("...") {
            Segment::Wildcard(name.to_string())
        } else {
            Segment::Param(rest.to_string())
        }
    } else if seg == "*" {
        Segment::Wildcard(String::new())
    } else {
        Segment::Literal(seg.to_string())
    }
}

/// Compile a route pattern string into segments.
///
/// Returns `None` when the pattern is malformed:
/// - empty param name (`/users/:/x`, `/users/:...`)
/// - bad param name (not `[A-Za-z_][A-Za-z0-9_]*`)
/// - greedy `:name...` not in last position
/// - `*` mixed with other segments (`/a/*/b`)
pub fn compile_pattern(pattern: &str) -> Option<RoutePattern> {
    if pattern == "*" {
        return Some(RoutePattern {
            segments: vec![Segment::Wildcard(String::new())],
            catch_all: true,
        });
    }
    if pattern.contains('{') || pattern.contains('}') {
        return None;
    }
    let trimmed = pattern.trim_matches('/');
    if trimmed.is_empty() {
        return Some(RoutePattern {
            segments: Vec::new(),
            catch_all: false,
        });
    }
    let raw: Vec<&str> = trimmed.split('/').collect();
    if raw.iter().any(|s| s.is_empty()) {
        return None; // `//` in pattern
    }
    let mut segments = Vec::with_capacity(raw.len());
    for (i, seg) in raw.iter().enumerate() {
        let parsed = parse_segment(seg);
        match &parsed {
            Segment::Wildcard(name) => {
                if *seg == "*" {
                    return None; // `*` only valid as the whole pattern
                }
                if i != raw.len() - 1 {
                    return None; // greedy tail must be last
                }
                if name.is_empty() || !valid_param_name(name) {
                    return None;
                }
            }
            Segment::Param(name) => {
                if name.is_empty() || !valid_param_name(name) {
                    return None;
                }
            }
            Segment::Literal(_) => {}
        }
        segments.push(parsed);
    }
    Some(RoutePattern {
        segments,
        catch_all: false,
    })
}

fn valid_param_name(name: &str) -> bool {
    let mut chars = name.chars();
    match chars.next() {
        Some(c) if c.is_ascii_alphabetic() || c == '_' => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// Match a compiled pattern against an actual request path.
/// Returns captured `(name, value)` pairs on success.
pub fn match_pattern(pattern: &RoutePattern, actual: &str) -> Option<Vec<(String, String)>> {
    if pattern.catch_all {
        return Some(Vec::new());
    }
    let trimmed = actual.trim_matches('/');
    let actual_segs: Vec<&str> = if trimmed.is_empty() {
        Vec::new()
    } else {
        trimmed.split('/').collect()
    };
    // Greedy tail absorbs the remainder; otherwise arity must match exactly.
    let has_wildcard = matches!(pattern.segments.last(), Some(Segment::Wildcard(_)));
    if has_wildcard {
        if actual_segs.len() < pattern.segments.len() - 1 {
            return None;
        }
    } else if actual_segs.len() != pattern.segments.len() {
        return None;
    }
    let mut params = Vec::new();
    for (i, seg) in pattern.segments.iter().enumerate() {
        match seg {
            Segment::Literal(lit) => {
                if actual_segs.get(i) != Some(&lit.as_str()) {
                    return None;
                }
            }
            Segment::Param(name) => {
                let val = *actual_segs.get(i)?;
                params.push((name.clone(), val.to_string()));
            }
            Segment::Wildcard(name) => {
                let tail = actual_segs[i..].join("/");
                if !name.is_empty() {
                    params.push((name.clone(), tail));
                }
                break;
            }
        }
    }
    Some(params)
}

/// A scored candidate: handler index, captured params, precedence score.
type ScoredMatch = (usize, Vec<(String, String)>, (usize, usize, usize));

/// Score a match for precedence: more literals wins, params beat wildcards.
fn match_score(pattern: &RoutePattern) -> (usize, usize, usize) {
    let mut literals = 0;
    let mut params = 0;
    let mut wildcards = 0;
    for seg in &pattern.segments {
        match seg {
            Segment::Literal(_) => literals += 1,
            Segment::Param(_) => params += 1,
            Segment::Wildcard(_) => wildcards += 1,
        }
    }
    if pattern.catch_all {
        wildcards += 1;
    }
    (literals, params, std::cmp::Reverse(wildcards).0)
}

/// Match `path` against every `(pattern, handler_idx)` route for one method.
///
/// Returns the best `(handler_idx, params)`: highest
/// `(literals, params, -wildcards)` score, first-registered wins ties.
/// Malformed patterns never match (checker lint in P2 rejects them statically).
pub fn best_match(
    routes: &[(String, String)],
    method: &str,
    path: &str,
) -> Option<(usize, Vec<(String, String)>)> {
    let mut best: Option<ScoredMatch> = None;
    for (idx, (m, pattern_str)) in routes.iter().enumerate() {
        if m != method {
            continue;
        }
        if pattern_str == path {
            // Exact string equality short-circuits scoring entirely.
            return Some((idx, Vec::new()));
        }
        let Some(compiled) = compile_pattern(pattern_str) else {
            continue;
        };
        let Some(params) = match_pattern(&compiled, path) else {
            continue;
        };
        let score = match_score(&compiled);
        let better = match &best {
            None => true,
            Some((_, _, s)) => score > *s,
        };
        if better {
            best = Some((idx, params, score));
        }
    }
    best.map(|(idx, params, _)| (idx, params))
}

/// True when `path` matches at least one route registered for a *different*
/// method — lets dispatch return 405 instead of 404.
pub fn matches_other_method(routes: &[(String, String)], method: &str, path: &str) -> bool {
    for (m, pattern_str) in routes {
        if m == method {
            continue;
        }
        if pattern_str == path {
            return true;
        }
        if let Some(compiled) = compile_pattern(pattern_str) {
            if match_pattern(&compiled, path).is_some() {
                return true;
            }
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pat(s: &str) -> RoutePattern {
        compile_pattern(s).expect("valid pattern")
    }

    #[test]
    fn exact_beats_param() {
        let routes = vec![
            ("GET".to_string(), "/users/:id".to_string()),
            ("GET".to_string(), "/users/new".to_string()),
        ];
        let (idx, params) = best_match(&routes, "GET", "/users/new").unwrap();
        assert_eq!(idx, 1);
        assert!(params.is_empty());
    }

    #[test]
    fn param_captures_single_segment() {
        let (idx, params) = best_match(
            &[("GET".to_string(), "/users/:id".to_string())],
            "GET",
            "/users/42",
        )
        .unwrap();
        assert_eq!(idx, 0);
        assert_eq!(params, vec![("id".to_string(), "42".to_string())]);
    }

    #[test]
    fn param_does_not_span_slash() {
        let routes = vec![("GET".to_string(), "/users/:id".to_string())];
        assert!(best_match(&routes, "GET", "/users/42/posts").is_none());
        assert!(best_match(&routes, "GET", "/users").is_none());
    }

    #[test]
    fn greedy_tail_captures_rest() {
        let routes = vec![("GET".to_string(), "/files/:path...".to_string())];
        let (idx, params) = best_match(&routes, "GET", "/files/a/b/c").unwrap();
        assert_eq!(idx, 0);
        assert_eq!(params, vec![("path".to_string(), "a/b/c".to_string())]);
    }

    #[test]
    fn greedy_tail_matches_empty() {
        let routes = vec![("GET".to_string(), "/files/:path...".to_string())];
        let (_, params) = best_match(&routes, "GET", "/files").unwrap();
        assert_eq!(params, vec![("path".to_string(), String::new())]);
    }

    #[test]
    fn catch_all_matches_everything() {
        let routes = vec![("GET".to_string(), "*".to_string())];
        assert!(best_match(&routes, "GET", "/anything/at/all").is_some());
    }

    #[test]
    fn root_matches_root() {
        let routes = vec![("GET".to_string(), "/".to_string())];
        let (idx, params) = best_match(&routes, "GET", "/").unwrap();
        assert_eq!(idx, 0);
        assert!(params.is_empty());
    }

    #[test]
    fn method_mismatch_detected() {
        let routes = vec![("GET".to_string(), "/users/:id".to_string())];
        assert!(best_match(&routes, "POST", "/users/42").is_none());
        assert!(matches_other_method(&routes, "POST", "/users/42"));
        assert!(!matches_other_method(&routes, "POST", "/nope"));
    }

    #[test]
    fn malformed_patterns_rejected() {
        assert!(compile_pattern("/users/{id}").is_none());
        assert!(compile_pattern("/users/:").is_none());
        assert!(compile_pattern("/users/:...").is_none());
        assert!(compile_pattern("/users/:a.../x").is_none());
        assert!(compile_pattern("/a//b").is_none());
        assert!(compile_pattern("/a/*/b").is_none());
        assert!(compile_pattern("/users/:9bad").is_none());
    }

    #[test]
    fn multi_param_route() {
        let p = pat("/posts/:pid/comments/:cid");
        let params = match_pattern(&p, "/posts/5/comments/99").unwrap();
        assert_eq!(
            params,
            vec![
                ("pid".to_string(), "5".to_string()),
                ("cid".to_string(), "99".to_string()),
            ]
        );
    }

    #[test]
    fn first_registered_wins_ties() {
        let routes = vec![
            ("GET".to_string(), "/a/:x".to_string()),
            ("GET".to_string(), "/a/:y".to_string()),
        ];
        let (idx, params) = best_match(&routes, "GET", "/a/1").unwrap();
        assert_eq!(idx, 0);
        assert_eq!(params, vec![("x".to_string(), "1".to_string())]);
    }

    #[test]
    fn trailing_slash_tolerant() {
        let routes = vec![("GET".to_string(), "/users/:id".to_string())];
        let (_, params) = best_match(&routes, "GET", "/users/42/").unwrap();
        assert_eq!(params, vec![("id".to_string(), "42".to_string())]);
    }
}
