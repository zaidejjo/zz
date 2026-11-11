//! Conservative per-parameter escape analysis for call-argument ownership.
//!
//! The AOT backend passes heap-typed call arguments as retained shares
//! (`zz_clone`), and nothing releases the caller share after the call —
//! roughly 300B retained per call in struct-heavy loops (#188). Releasing
//! those shares (or borrowing instead of cloning) is only sound when the
//! callee provably never lets the argument outlive the call: no stores of
//! it, no returns of it, no forwarding to unknown code, no captures, and
//! no rebinding its slot (which would release the borrowed storage early).
//!
//! This analysis computes, per user function and parameter, whether that
//! holds. It is *conservative in the safe direction*: anything it cannot
//! prove read-only stays escaping, so callers keep today's clone
//! behavior. Only proven read-only parameters unlock borrow-passing.
//!
//! Soundness-critical invariants (do not weaken without a proof):
//! - Reads through cloning accessors (field/index/len on a head name,
//!   match scrutinees, conditions) yield FRESH values: the runtime clones
//!   on extraction, so the result never aliases the source. Bare mentions
//!   (`a` itself) DO alias.
//! - Stores clone for store, but plain assignment MOVES (shares storage)
//!   and returns/args borrow — so any stored/returned/forwarded alias
//!   counts as escaped. Rebinding a borrowed slot releases caller storage,
//!   so rebinding a tainted parameter disqualifies it outright.
//! - Captures (closures, nested functions, defers) outlive the call:
//!   any outer-tainted mention inside one escapes, even in read position.
//! - Fixpoint starts optimistic (all read-only) and only removes on
//!   evidence, iterating to stability. Self-recursive read-only claims
//!   are sound: infinite regress diverges (no observable aliasing), and
//!   any finite path through them reads only.

use std::collections::{HashMap, HashSet};

use zz_checker::Type;
use zz_frontend::ast::{Block, Expr, FmtPart, MatchArm, Pattern, Program, Stmt};
use zz_frontend::move_elide::count_refs;

/// Outcome of evaluating an expression: whether tainted storage escapes
/// through it (calls into unknown code, captures, deferred runs), and
/// whether the resulting value may alias tainted storage.
#[derive(Clone, Copy, Default)]
struct Flow {
    escaped: bool,
    aliased: bool,
}

impl Flow {
    fn clean() -> Self {
        Flow {
            escaped: false,
            aliased: false,
        }
    }
    fn or(self, other: Flow) -> Self {
        Flow {
            escaped: self.escaped || other.escaped,
            aliased: self.aliased || other.aliased,
        }
    }
    fn seq(fs: &[Flow]) -> Self {
        let mut out = Flow::clean();
        for f in fs {
            out = out.or(*f);
        }
        out
    }
}

/// Inline (Copy, heap-free) types can never alias heap storage, so
/// parameters of these types are read-only without walking anything.
fn type_is_inline(ty: &Type, structs: &HashMap<String, zz_checker::StructSig>) -> bool {
    match ty {
        Type::Int | Type::Float | Type::Bool | Type::Unit | Type::Void => true,
        Type::Struct(name, _) => {
            // Unboxed (all-scalar, transitively) structs lower to raw C
            // values with no heap refs. Anything else is heap.
            fn unboxed(
                name: &str,
                structs: &HashMap<String, zz_checker::StructSig>,
                seen: &mut HashSet<String>,
            ) -> bool {
                if !seen.insert(name.to_string()) {
                    return true;
                }
                match structs.get(name) {
                    Some(sig) => sig.fields.iter().all(|(_, t)| match t {
                        Type::Int | Type::Float | Type::Bool | Type::Unit | Type::Void => true,
                        Type::Struct(inner, _) => unboxed(inner, structs, seen),
                        _ => false,
                    }),
                    None => false,
                }
            }
            unboxed(name, structs, &mut HashSet::new())
        }
        Type::Str
        | Type::Tuple(_)
        | Type::Option(_)
        | Type::Result(_, _)
        | Type::Func(_, _)
        | Type::Array(_)
        | Type::Bytes
        | Type::Dict(_, _)
        | Type::Union(_)
        | Type::Json
        | Type::Db
        | Type::HttpServer
        | Type::TcpStream
        | Type::TcpListener
        | Type::Response
        | Type::HttpRequest
        | Type::Opaque(_)
        | Type::Chan
        | Type::TaskJoin
        | Type::Enum(_, _)
        | Type::Range(_)
        | Type::Var(_)
        | Type::Named(_)
        | Type::Error
        | Type::Never
        | Type::Ptr { .. } => false,
    }
}

/// Callee classification for interprocedural forwarding.
enum Callee {
    /// `len` family: reads only, fresh int result. Nested argument
    /// escapes still propagate (e.g. `len(f(r))`).
    Len,
    /// User function with a verdict: forwarding an argument to its i-th
    /// parameter is safe exactly when that parameter is read-only.
    Known(Vec<bool>),
    /// Everything else (natives, methods, unknown, arity-mismatched,
    /// named args): any tainted involvement escapes, results may alias.
    Unknown,
}

/// The walker: scope stack for taint plus the callee verdict table.
struct Walker<'a> {
    scopes: Vec<HashMap<String, bool>>,
    /// Inside a closure body, nested function, or deferred expression,
    /// even reads of outer tainted names escape via capture. Depth of
    /// such barriers.
    barriers: usize,
    /// Current read-only verdicts (optimistic start, pruned to fixpoint).
    masks: &'a HashMap<String, Vec<bool>>,
}

impl<'a> Walker<'a> {
    fn new(masks: &'a HashMap<String, Vec<bool>>) -> Self {
        Walker {
            scopes: vec![HashMap::new()],
            barriers: 0,
            masks,
        }
    }

    fn push(&mut self) {
        self.scopes.push(HashMap::new());
    }

    fn pop(&mut self) {
        self.scopes.pop();
    }

    fn define(&mut self, name: &str, tainted: bool) {
        if let Some(scope) = self.scopes.last_mut() {
            scope.insert(name.to_string(), tainted);
        }
    }

    /// Innermost binding wins. Returns (tainted, captured-from-outer).
    /// Names bound nowhere (globals, imports, builtins) are untainted.
    /// Capture is purely barrier-driven: same-frame nesting (blocks,
    /// branches) shares lifetimes, so only closure/defer boundaries
    /// capture.
    fn resolve(&self, name: &str) -> (bool, bool) {
        for scope in self.scopes.iter().rev() {
            if let Some(t) = scope.get(name) {
                return (*t, self.barriers > 0);
            }
        }
        (false, false)
    }

    /// Flow of a bare name use: direct mentions alias; mentions captured
    /// across a barrier escape.
    fn flow_name(&self, name: &str) -> Flow {
        let (tainted, captured) = self.resolve(name);
        Flow {
            escaped: tainted && captured,
            aliased: tainted,
        }
    }

    /// True when an expression position touches tainted storage at all
    /// (for mutation targets, where even reads matter).
    fn mentions_tainted(&self, e: &Expr) -> bool {
        match e {
            Expr::Ident { name, .. } => self.resolve(name).0,
            Expr::Path { parts, .. } => parts.first().is_some_and(|h| self.resolve(h).0),
            Expr::Paren { expr, .. } | Expr::Unary { expr, .. } => self.mentions_tainted(expr),
            Expr::Binary { left, right, .. } => {
                self.mentions_tainted(left) || self.mentions_tainted(right)
            }
            Expr::Call {
                callee,
                args,
                named,
                ..
            } => {
                self.mentions_tainted(callee)
                    || args.iter().any(|a| self.mentions_tainted(a))
                    || named.iter().any(|(_, v)| self.mentions_tainted(v))
            }
            Expr::Closure { params, body, .. } => {
                let shadowed: HashSet<&str> = params.iter().map(|p| p.name.name.as_str()).collect();
                self.mentions_tainted_scoped(body, &shadowed)
            }
            Expr::If {
                cond, then, els, ..
            } => {
                self.mentions_tainted(cond)
                    || self.mentions_tainted_block(then)
                    || els.as_ref().is_some_and(|x| self.mentions_tainted(x))
            }
            Expr::While { cond, body, .. } => {
                self.mentions_tainted(cond) || self.mentions_tainted_block(body)
            }
            Expr::Match {
                scrutinee, arms, ..
            } => {
                self.mentions_tainted(scrutinee)
                    || arms.iter().any(|a| {
                        self.mentions_tainted_pat(&a.pat)
                            || a.guard.as_ref().is_some_and(|g| self.mentions_tainted(g))
                            || self.mentions_tainted(&a.body)
                    })
            }
            Expr::IfLet {
                pat,
                value,
                then,
                els,
                ..
            } => {
                self.mentions_tainted(value)
                    || self.mentions_tainted_pat(pat)
                    || self.mentions_tainted_block(then)
                    || els.as_ref().is_some_and(|x| self.mentions_tainted(x))
            }
            Expr::Try { expr, .. } => self.mentions_tainted(expr),
            Expr::Block(b) => self.mentions_tainted_block(b),
            Expr::Variant { arg, .. } => arg.as_ref().is_some_and(|a| self.mentions_tainted(a)),
            Expr::Array { elems, .. } | Expr::Tuple { items: elems, .. } => {
                elems.iter().any(|x| self.mentions_tainted(x))
            }
            Expr::Dict { entries, .. } => entries
                .iter()
                .any(|(k, v)| self.mentions_tainted(k) || self.mentions_tainted(v)),
            Expr::Field { obj, .. } => self.mentions_tainted(obj),
            Expr::Range { start, end, .. } => {
                self.mentions_tainted(start) || self.mentions_tainted(end)
            }
            Expr::StructInit { fields, .. } => fields.iter().any(|(_, v)| self.mentions_tainted(v)),
            Expr::Index { obj, index, .. } => {
                self.mentions_tainted(obj) || self.mentions_tainted(index)
            }
            Expr::Slice {
                obj, start, end, ..
            } => {
                self.mentions_tainted(obj)
                    || start.as_ref().is_some_and(|s| self.mentions_tainted(s))
                    || end.as_ref().is_some_and(|e| self.mentions_tainted(e))
            }
            Expr::ListComp {
                body,
                var,
                iter,
                filter,
                ..
            } => {
                self.mentions_tainted(iter)
                    || self.mentions_tainted_scoped(
                        body,
                        &std::iter::once(var.name.as_str()).collect(),
                    )
                    || filter.as_ref().is_some_and(|f| {
                        self.mentions_tainted_scoped(
                            f,
                            &std::iter::once(var.name.as_str()).collect(),
                        )
                    })
            }
            Expr::Int { .. }
            | Expr::Float { .. }
            | Expr::Str { .. }
            | Expr::Bool { .. }
            | Expr::Break { .. }
            | Expr::Continue { .. } => false,
            Expr::Fmt { parts, .. } => parts.iter().any(|p| match p {
                FmtPart::Text(_) => false,
                FmtPart::Expr(inner, _) => self.mentions_tainted(inner),
            }),
        }
    }

    /// Like [`Walker::mentions_tainted`], but names in `shadowed` resolve
    /// to fresh inner bindings instead of possibly-tainted outers.
    fn mentions_tainted_scoped(&self, e: &Expr, shadowed: &HashSet<&str>) -> bool {
        match e {
            Expr::Ident { name, .. } => !shadowed.contains(name.as_str()) && self.resolve(name).0,
            Expr::Path { parts, .. } => parts
                .first()
                .is_some_and(|h| !shadowed.contains(h.as_str()) && self.resolve(h).0),
            Expr::Closure { params, body, .. } => {
                let mut shadowed = shadowed.clone();
                for p in params {
                    shadowed.insert(p.name.name.as_str());
                }
                // Capture check needs barrier semantics: build a scoped
                // variant inline by temporarily noting depth is
                // unnecessary here — any outer mention escapes.
                self.mentions_tainted_scoped_barrier(body, &shadowed)
            }
            _ => self.mentions_tainted_closure_fallback(e, shadowed),
        }
    }

    /// Closure bodies capture everything they mention.
    fn mentions_tainted_scoped_barrier(&self, e: &Expr, shadowed: &HashSet<&str>) -> bool {
        match e {
            Expr::Ident { name, .. } => !shadowed.contains(name.as_str()) && self.resolve(name).0,
            Expr::Path { parts, .. } => parts
                .first()
                .is_some_and(|h| !shadowed.contains(h.as_str()) && self.resolve(h).0),
            Expr::Closure { params, body, .. } => {
                let mut shadowed = shadowed.clone();
                for p in params {
                    shadowed.insert(p.name.name.as_str());
                }
                self.mentions_tainted_scoped_barrier(body, &shadowed)
            }
            _ => self.mentions_tainted_closure_fallback(e, shadowed),
        }
    }

    /// Structural recursion shared by the scoped mention walkers.
    fn mentions_tainted_closure_fallback(&self, e: &Expr, shadowed: &HashSet<&str>) -> bool {
        match e {
            Expr::Ident { .. } | Expr::Path { .. } => self.mentions_tainted_scoped(e, shadowed),
            Expr::Paren { expr, .. } | Expr::Unary { expr, .. } => {
                self.mentions_tainted_closure_fallback(expr, shadowed)
            }
            Expr::Binary { left, right, .. } => {
                self.mentions_tainted_closure_fallback(left, shadowed)
                    || self.mentions_tainted_closure_fallback(right, shadowed)
            }
            Expr::Call {
                callee,
                args,
                named,
                ..
            } => {
                self.mentions_tainted_closure_fallback(callee, shadowed)
                    || args
                        .iter()
                        .any(|a| self.mentions_tainted_closure_fallback(a, shadowed))
                    || named
                        .iter()
                        .any(|(_, v)| self.mentions_tainted_closure_fallback(v, shadowed))
            }
            Expr::Closure { .. } => self.mentions_tainted_scoped(e, shadowed),
            Expr::If {
                cond, then, els, ..
            } => {
                self.mentions_tainted_closure_fallback(cond, shadowed)
                    || self.mentions_tainted_block(then)
                    || els
                        .as_ref()
                        .is_some_and(|x| self.mentions_tainted_closure_fallback(x, shadowed))
            }
            Expr::While { cond, body, .. } => {
                self.mentions_tainted_closure_fallback(cond, shadowed)
                    || self.mentions_tainted_block(body)
            }
            Expr::Match {
                scrutinee, arms, ..
            } => {
                self.mentions_tainted_closure_fallback(scrutinee, shadowed)
                    || arms.iter().any(|a| {
                        self.mentions_tainted_pat(&a.pat)
                            || a.guard.as_ref().is_some_and(|g| {
                                self.mentions_tainted_closure_fallback(g, shadowed)
                            })
                            || self.mentions_tainted_closure_fallback(&a.body, shadowed)
                    })
            }
            Expr::IfLet {
                pat,
                value,
                then,
                els,
                ..
            } => {
                self.mentions_tainted_closure_fallback(value, shadowed)
                    || self.mentions_tainted_pat(pat)
                    || self.mentions_tainted_block(then)
                    || els
                        .as_ref()
                        .is_some_and(|x| self.mentions_tainted_closure_fallback(x, shadowed))
            }
            Expr::Try { expr, .. } => self.mentions_tainted_closure_fallback(expr, shadowed),
            Expr::Block(b) => self.mentions_tainted_block(b),
            Expr::Variant { arg, .. } => arg
                .as_ref()
                .is_some_and(|a| self.mentions_tainted_closure_fallback(a, shadowed)),
            Expr::Array { elems, .. } | Expr::Tuple { items: elems, .. } => elems
                .iter()
                .any(|x| self.mentions_tainted_closure_fallback(x, shadowed)),
            Expr::Dict { entries, .. } => entries.iter().any(|(k, v)| {
                self.mentions_tainted_closure_fallback(k, shadowed)
                    || self.mentions_tainted_closure_fallback(v, shadowed)
            }),
            Expr::Field { obj, .. } => self.mentions_tainted_closure_fallback(obj, shadowed),
            Expr::Range { start, end, .. } => {
                self.mentions_tainted_closure_fallback(start, shadowed)
                    || self.mentions_tainted_closure_fallback(end, shadowed)
            }
            Expr::StructInit { fields, .. } => fields
                .iter()
                .any(|(_, v)| self.mentions_tainted_closure_fallback(v, shadowed)),
            Expr::Index { obj, index, .. } => {
                self.mentions_tainted_closure_fallback(obj, shadowed)
                    || self.mentions_tainted_closure_fallback(index, shadowed)
            }
            Expr::Slice {
                obj, start, end, ..
            } => {
                self.mentions_tainted_closure_fallback(obj, shadowed)
                    || start
                        .as_ref()
                        .is_some_and(|s| self.mentions_tainted_closure_fallback(s, shadowed))
                    || end
                        .as_ref()
                        .is_some_and(|e| self.mentions_tainted_closure_fallback(e, shadowed))
            }
            Expr::ListComp {
                body,
                var,
                iter,
                filter,
                ..
            } => {
                let mut shadowed = shadowed.clone();
                shadowed.insert(var.name.as_str());
                self.mentions_tainted_closure_fallback(iter, &shadowed)
                    || self.mentions_tainted_closure_fallback(body, &shadowed)
                    || filter
                        .as_ref()
                        .is_some_and(|f| self.mentions_tainted_closure_fallback(f, &shadowed))
            }
            Expr::Fmt { parts, .. } => parts.iter().any(|p| match p {
                FmtPart::Text(_) => false,
                FmtPart::Expr(inner, _) => self.mentions_tainted_closure_fallback(inner, shadowed),
            }),
            Expr::Int { .. }
            | Expr::Float { .. }
            | Expr::Str { .. }
            | Expr::Bool { .. }
            | Expr::Break { .. }
            | Expr::Continue { .. } => false,
        }
    }

    fn mentions_tainted_block(&self, b: &Block) -> bool {
        b.stmts.iter().any(|s| self.mentions_tainted_stmt(s))
    }

    fn mentions_tainted_stmt(&self, s: &Stmt) -> bool {
        match s {
            Stmt::Decl { value, .. } => self.mentions_tainted(value),
            // A nested definition in a scanned region conservatively
            // counts as mentioning: its body may capture outer tainted
            // storage (checked precisely by the main walk).
            Stmt::Func { .. } => true,
            Stmt::Return { value, .. } => value.as_ref().is_some_and(|v| self.mentions_tainted(v)),
            Stmt::Expr(e) => self.mentions_tainted(e),
            Stmt::Defer { expr: e, .. } => self.mentions_tainted(e),
            Stmt::Assign { target, value, .. } | Stmt::CompoundAssign { target, value, .. } => {
                self.mentions_tainted(target) || self.mentions_tainted(value)
            }
            Stmt::Destructure { value, .. } => self.mentions_tainted(value),
            Stmt::For { iter, body, .. } => {
                self.mentions_tainted(iter) || self.mentions_tainted_block(body)
            }
            Stmt::Break { .. } | Stmt::Continue { .. } => false,
            Stmt::Import { .. }
            | Stmt::Struct { .. }
            | Stmt::TypeAlias { .. }
            | Stmt::Enum { .. }
            | Stmt::Impl { .. }
            | Stmt::ExternBlock { .. }
            | Stmt::Link { .. } => false,
        }
    }

    fn mentions_tainted_pat(&self, pat: &Pattern) -> bool {
        match pat {
            Pattern::Wildcard { .. } => false,
            // A bare binding mentions nothing by itself; the taint comes
            // from the matched value, tracked by the caller.
            Pattern::Binding { .. } => false,
            Pattern::Literal { .. } => false,
            Pattern::Variant { arg: Some(a), .. } => self.mentions_tainted_pat(a),
            Pattern::Variant { arg: None, .. } => false,
            Pattern::Tuple { pats, .. } | Pattern::Or { pats, .. } => {
                pats.iter().any(|p| self.mentions_tainted_pat(p))
            }
        }
    }

    fn classify_callee(&self, callee: &Expr, args: &[Expr], named_empty: bool) -> Callee {
        let cname: Option<String> = match callee {
            Expr::Ident { name, .. } => Some(name.clone()),
            Expr::Path { parts, .. } => Some(parts.join(".")),
            _ => None,
        };
        let Some(cname) = cname else {
            return Callee::Unknown;
        };
        if matches!(
            cname.as_str(),
            "len" | "vec.len" | "std.vec.len" | "std.len"
        ) {
            return Callee::Len;
        }
        match self.masks.get(&cname) {
            Some(mask) if named_empty && args.len() <= mask.len() => Callee::Known(mask.clone()),
            _ => Callee::Unknown,
        }
    }

    /// A method-style callee (`obj.method`) hands the receiver to unknown
    /// code: escape when the receiver touches tainted storage. Module
    /// heads resolve nowhere, so plain module calls never trip this.
    fn receiver_escapes(&self, callee: &Expr) -> bool {
        match callee {
            Expr::Field { obj, .. } => self.mentions_tainted(obj),
            Expr::Path { parts, .. } => parts.first().is_some_and(|h| self.resolve(h).0),
            _ => false,
        }
    }

    fn flow_expr(&mut self, e: &Expr) -> Flow {
        match e {
            Expr::Int { .. }
            | Expr::Float { .. }
            | Expr::Str { .. }
            | Expr::Bool { .. }
            | Expr::Break { .. }
            | Expr::Continue { .. } => Flow::clean(),
            Expr::Ident { name, .. } => self.flow_name(name),
            Expr::Path { parts, .. } => match parts.first() {
                // Reads through accessors yield fresh clones; only a
                // captured read escapes.
                Some(head) => {
                    let (tainted, captured) = self.resolve(head);
                    Flow {
                        escaped: tainted && captured,
                        aliased: false,
                    }
                }
                None => Flow::clean(),
            },
            Expr::Fmt { parts, .. } => {
                let mut out = Flow::clean();
                for part in parts {
                    if let FmtPart::Expr(inner, _) = part {
                        out = out.or(self.flow_read(inner));
                    }
                }
                out
            }
            Expr::Paren { expr, .. } | Expr::Unary { expr, .. } => self.flow_expr(expr),
            Expr::Binary { left, right, .. } => self.flow_expr(left).or(self.flow_expr(right)),
            Expr::Call {
                callee,
                args,
                named,
                ..
            } => {
                if self.receiver_escapes(callee) {
                    return Flow {
                        escaped: true,
                        aliased: false,
                    };
                }
                let named_empty = named.is_empty();
                match self.classify_callee(callee, args, named_empty) {
                    Callee::Len => {
                        let mut out = Flow::clean();
                        for a in args {
                            out = out.or(self.flow_read(a));
                        }
                        out
                    }
                    Callee::Known(mask) => {
                        let mut out = Flow::clean();
                        for (i, a) in args.iter().enumerate() {
                            let f = self.flow_expr(a);
                            out.escaped |= f.escaped;
                            // Forwarding to a read-only parameter is safe;
                            // anything else escapes with the argument.
                            if !mask.get(i).copied().unwrap_or(false) {
                                out.escaped |= f.escaped || f.aliased;
                                out.aliased |= f.aliased;
                            }
                        }
                        debug_assert!(named.is_empty());
                        out
                    }
                    Callee::Unknown => {
                        let mut out = self.flow_expr(callee);
                        for a in args {
                            let f = self.flow_expr(a);
                            out.escaped |= f.escaped || f.aliased;
                            out.aliased |= f.aliased;
                        }
                        for (_, v) in named {
                            let f = self.flow_expr(v);
                            out.escaped |= f.escaped || f.aliased;
                            out.aliased |= f.aliased;
                        }
                        out
                    }
                }
            }
            Expr::Closure { params, body, .. } => {
                // Captures may outlive the call: any outer-tainted mention
                // escapes, even in read positions.
                self.push();
                for p in params {
                    self.define(&p.name.name, false);
                }
                self.barriers += 1;
                let f = self.flow_expr(body);
                self.barriers -= 1;
                self.pop();
                // The closure value itself is fresh; only the capture matters.
                Flow {
                    escaped: f.escaped || f.aliased,
                    aliased: false,
                }
            }
            Expr::If {
                cond, then, els, ..
            } => {
                let mut out = self.flow_read(cond);
                out = out.or(self.flow_block(then));
                if let Some(e) = els {
                    out = out.or(self.flow_expr(e));
                }
                out
            }
            Expr::While { cond, body, .. } => {
                let mut out = self.flow_read(cond);
                out = out.or(self.flow_block(body));
                out
            }
            Expr::Match {
                scrutinee, arms, ..
            } => {
                let mut out = self.flow_read(scrutinee);
                // Arm payloads extract by clone: fresh unless the whole
                // value binds (bare `x =>` arm aliases the scrutinee).
                let scrut_aliases = self.value_aliases(scrutinee);
                for arm in arms {
                    out = out.or(self.flow_arm(arm, scrut_aliases));
                }
                out
            }
            Expr::IfLet {
                pat,
                value,
                then,
                els,
                ..
            } => {
                let mut out = self.flow_read(value);
                let tainted = self.value_aliases(value);
                self.push();
                bind_pattern_whole(pat, tainted, &mut self.scopes);
                out = out.or(self.flow_block(then));
                self.pop();
                if let Some(e) = els {
                    out = out.or(self.flow_expr(e));
                }
                out
            }
            Expr::Try { expr, .. } => self.flow_expr(expr),
            Expr::Block(b) => self.flow_block(b),
            Expr::Variant { arg, .. } => match arg {
                // A payload value is stored into the constructed variant.
                Some(a) => self.flow_expr(a),
                None => Flow::clean(),
            },
            Expr::Array { elems, .. } | Expr::Tuple { items: elems, .. } => {
                Flow::seq(&elems.iter().map(|e| self.flow_expr(e)).collect::<Vec<_>>())
            }
            Expr::Dict { entries, .. } => {
                let mut out = Flow::clean();
                for (k, v) in entries {
                    out = out.or(self.flow_expr(k));
                    out = out.or(self.flow_expr(v));
                }
                out
            }
            Expr::Field { obj, .. } => {
                // Clone boundary: field extraction retains independently.
                let f = self.flow_read(obj);
                Flow {
                    escaped: f.escaped,
                    aliased: false,
                }
            }
            Expr::Range { start, end, .. } => self.flow_expr(start).or(self.flow_expr(end)),
            Expr::StructInit { fields, .. } => {
                let mut out = Flow::clean();
                for (_, v) in fields {
                    out = out.or(self.flow_expr(v));
                }
                out
            }
            Expr::Index { obj, index, .. } => {
                // Clone boundary like Field. Index operands evaluate
                // without storing.
                let o = self.flow_read(obj);
                let i = self.flow_read(index);
                Flow {
                    escaped: o.escaped || i.escaped,
                    aliased: false,
                }
            }
            Expr::Slice {
                obj, start, end, ..
            } => {
                let mut out = self.flow_read(obj);
                if let Some(s) = start {
                    out = out.or(self.flow_read(s));
                }
                if let Some(e) = end {
                    out = out.or(self.flow_read(e));
                }
                // Slices may share the buffer: keep the alias unless the
                // read itself was clean of tainted mentions.
                let mentions = self.mentions_tainted(obj)
                    || start.as_ref().is_some_and(|s| self.mentions_tainted(s))
                    || end.as_ref().is_some_and(|e| self.mentions_tainted(e));
                out.aliased |= mentions;
                out
            }
            Expr::ListComp {
                body,
                var,
                iter,
                filter,
                ..
            } => {
                let mut out = self.flow_read(iter);
                self.push();
                // Elements extract by clone; over-taint when the source
                // may alias so dict/bytes shapes stay sound.
                let tainted = self.value_aliases(iter);
                self.define(&var.name, tainted);
                out = out.or(self.flow_expr(body));
                if let Some(f) = filter {
                    out = out.or(self.flow_read(f));
                }
                self.pop();
                // The comprehension builds a fresh array, but elements may
                // alias tainted storage.
                out
            }
        }
    }

    /// Like [`Walker::flow_expr`], but a direct tainted mention is just a
    /// read (fresh clone): conditions, interpolation, `len` args, match
    /// scrutinees. Nested escapes still propagate; captures still escape.
    fn flow_read(&mut self, e: &Expr) -> Flow {
        match e {
            Expr::Ident { name, .. } => {
                let (tainted, captured) = self.resolve(name);
                Flow {
                    escaped: tainted && captured,
                    aliased: false,
                }
            }
            Expr::Path { parts, .. } => match parts.first() {
                Some(head) => {
                    let (tainted, captured) = self.resolve(head);
                    Flow {
                        escaped: tainted && captured,
                        aliased: false,
                    }
                }
                None => Flow::clean(),
            },
            _ => self.flow_expr(e),
        }
    }

    /// Whether evaluating `e` as a value could hand out storage aliasing
    /// tainted bindings (for pattern/loop-variable taint).
    fn value_aliases(&mut self, e: &Expr) -> bool {
        match e {
            Expr::Ident { name, .. } => self.resolve(name).0,
            Expr::Path { parts, .. } => parts.first().is_some_and(|h| self.resolve(h).0),
            _ => self.flow_expr(e).aliased,
        }
    }

    fn flow_arm(&mut self, arm: &MatchArm, scrut_aliased: bool) -> Flow {
        self.push();
        // Payloads extract by clone (fresh); a bare whole-value binding
        // aliases the scrutinee.
        bind_pattern_whole(&arm.pat, scrut_aliased, &mut self.scopes);
        let mut out = Flow::clean();
        if let Some(g) = &arm.guard {
            // Guards evaluate without storing.
            let f = self.flow_expr(g);
            out.escaped |= f.escaped;
        }
        out = out.or(self.flow_expr(&arm.body));
        self.pop();
        out
    }

    fn flow_block(&mut self, b: &Block) -> Flow {
        self.push();
        let mut out = Flow::clean();
        for s in &b.stmts {
            out = out.or(self.flow_stmt(s));
            if out.escaped {
                break;
            }
        }
        self.pop();
        out
    }

    /// Returns true when the statement lets tainted storage escape the
    /// function. Plain bindings update taint and continue.
    fn flow_stmt(&mut self, s: &Stmt) -> Flow {
        match s {
            Stmt::Decl { name, value, .. } => {
                let f = self.flow_expr(value);
                if f.escaped {
                    return Flow {
                        escaped: true,
                        aliased: false,
                    };
                }
                self.define(&name.name, f.aliased);
                Flow::clean()
            }
            Stmt::Func { params, body, .. } => {
                // Nested function: captures may outlive the call.
                self.push();
                for p in params {
                    self.define(&p.name.name, false);
                }
                self.barriers += 1;
                let f = self.flow_block(body);
                self.barriers -= 1;
                self.pop();
                if f.escaped || f.aliased {
                    return Flow {
                        escaped: true,
                        aliased: false,
                    };
                }
                Flow::clean()
            }
            Stmt::Return { value, .. } => match value {
                Some(v) => {
                    let f = self.flow_expr(v);
                    // Returned values outlive the call by definition.
                    Flow {
                        escaped: f.escaped || f.aliased,
                        aliased: false,
                    }
                }
                None => Flow::clean(),
            },
            Stmt::Expr(e) => {
                let f = self.flow_expr(e);
                // Evaluated and discarded: only nested escapes matter.
                Flow {
                    escaped: f.escaped,
                    aliased: false,
                }
            }
            Stmt::Assign { target, value, .. } => self.flow_assign(target, value),
            Stmt::CompoundAssign { target, value, .. } => {
                // `x OP= v` reads x, computes, and stores back. A store
                // through tainted storage keeps it observable to a
                // borrow-passing caller (clone-passing detaches on write),
                // so it escapes.
                if self.mentions_tainted(target) {
                    return Flow {
                        escaped: true,
                        aliased: false,
                    };
                }
                let v = self.flow_expr(value);
                if v.escaped {
                    return Flow {
                        escaped: true,
                        aliased: false,
                    };
                }
                // Rebind plain locals like Assign (never untaints).
                if let Expr::Ident { name, .. } = target {
                    if v.aliased {
                        self.define(name, true);
                    }
                }
                Flow::clean()
            }
            Stmt::Destructure { pat, value, .. } => {
                let f = self.flow_expr(value);
                if f.escaped {
                    return Flow {
                        escaped: true,
                        aliased: false,
                    };
                }
                // Elements extract by clone; a bare whole-value binding
                // aliases the source.
                bind_pattern_whole(pat, f.aliased, &mut self.scopes);
                Flow::clean()
            }
            Stmt::For {
                vars, iter, body, ..
            } => {
                let it = self.flow_expr(iter);
                if it.escaped {
                    return Flow {
                        escaped: true,
                        aliased: false,
                    };
                }
                self.push();
                for v in vars {
                    self.define(&v.name, it.aliased);
                }
                let out = self.flow_block(body);
                self.pop();
                out
            }
            Stmt::Defer { expr, .. } => {
                // Runs at scope exit: any tainted involvement escapes.
                let f = self.flow_expr(expr);
                Flow {
                    escaped: f.escaped || f.aliased,
                    aliased: false,
                }
            }
            Stmt::Break { .. } | Stmt::Continue { .. } => Flow::clean(),
            Stmt::Import { .. }
            | Stmt::Struct { .. }
            | Stmt::TypeAlias { .. }
            | Stmt::Enum { .. }
            | Stmt::Impl { .. }
            | Stmt::ExternBlock { .. }
            | Stmt::Link { .. } => Flow::clean(),
        }
    }

    /// `target = value`: plain locals rebind (taint follows the value,
    /// including untainting on fresh rebinding — rebinding a tainted
    /// slot releases its old storage, which is exactly what
    /// borrow-passing forbids, so rebinding tainted disqualifies).
    /// Stores through heap/global paths escape when they touch tainted
    /// storage or store aliasing values.
    fn flow_assign(&mut self, target: &Expr, value: &Expr) -> Flow {
        let v = self.flow_expr(value);
        if v.escaped {
            return Flow {
                escaped: true,
                aliased: false,
            };
        }
        match target {
            Expr::Ident { name, .. } => match self.resolve(name) {
                // Unbound: a global/store target in some positions;
                // conservatively treat an aliasing store as escaping.
                // Bound fresh: rebind (taint follows the value).
                // Bound tainted: rebinding releases borrowed storage.
                (false, _) => {
                    self.define(name, v.aliased);
                    Flow::clean()
                }
                (true, _) => Flow {
                    escaped: true,
                    aliased: false,
                },
            },
            _ => {
                if self.mentions_tainted(target) || v.aliased {
                    return Flow {
                        escaped: true,
                        aliased: false,
                    };
                }
                Flow::clean()
            }
        }
    }
}

/// Bind with whole-value semantics: `Binding` aliases the scrutinee;
/// payload positions (`Variant` args, tuple elements, `Or` alternatives
/// under a payload) extract by clone and stay fresh... except `Or`,
/// which preserves whole-ness: one alternative may be the bare value.
fn bind_pattern_whole(pat: &Pattern, tainted: bool, scopes: &mut Vec<HashMap<String, bool>>) {
    match pat {
        Pattern::Wildcard { .. } => {}
        Pattern::Binding { name } => {
            if let Some(scope) = scopes.last_mut() {
                scope.insert(name.name.clone(), tainted);
            }
        }
        Pattern::Literal { .. } => {}
        Pattern::Variant { arg: Some(a), .. } => {
            bind_pattern_fresh(a, scopes);
        }
        Pattern::Variant { arg: None, .. } => {}
        Pattern::Tuple { pats, .. } => {
            for p in pats {
                bind_pattern_fresh(p, scopes);
            }
        }
        Pattern::Or { pats, .. } => {
            for p in pats {
                bind_pattern_whole(p, tainted, scopes);
            }
        }
    }
}

/// Bind everything under a payload position as fresh: extraction clones.
fn bind_pattern_fresh(pat: &Pattern, scopes: &mut Vec<HashMap<String, bool>>) {
    match pat {
        Pattern::Wildcard { .. } => {}
        Pattern::Binding { name } => {
            if let Some(scope) = scopes.last_mut() {
                scope.insert(name.name.clone(), false);
            }
        }
        Pattern::Literal { .. } => {}
        Pattern::Variant { arg: Some(a), .. } => bind_pattern_fresh(a, scopes),
        Pattern::Variant { arg: None, .. } => {}
        Pattern::Tuple { pats, .. } | Pattern::Or { pats, .. } => {
            for p in pats {
                bind_pattern_fresh(p, scopes);
            }
        }
    }
}

/// Walk one function body with a single tainted parameter. Returns true
/// when the parameter provably never escapes (read-only).
fn param_is_readonly(
    params: &[(String, Type)],
    idx: usize,
    body: &Block,
    masks: &HashMap<String, Vec<bool>>,
) -> bool {
    let mut w = Walker::new(masks);
    for (name, _) in params {
        w.define(name, false);
    }
    w.define(&params[idx].0, true);
    let mut out = Flow::clean();
    for s in &body.stmts {
        out = out.or(w.flow_stmt(s));
        if out.escaped {
            return false;
        }
    }
    true
}

/// Compute the set of locals in `block` that may be lowered as flat
/// (unboxed) arrays: declared as `name: [scalar|unboxed-struct] = []` and
/// mentioned *only* in positions the flat layout implements.
///
/// Supported mentions (everything else disqualifies the name, which then
/// keeps the boxed `zz_value` layout — always correct, just slower):
/// - `arr.push(x)` (one positional argument)
/// - `arr[i]` reads with a non-slice index (scalar elements only; a struct
///   element has no boxed form, so `arr[i].field` is the supported read)
/// - `len(arr)`
/// - `for x in arr`
///
/// Any other mention (call argument, `return arr`, whole-variable
/// assignment, rebinding, `arr[i] = v`, slicing, `.map`/`.pop`/…,
/// closure/defer capture, shadowing) means the buffer would have to be
/// boxed at that point, so the name stays boxed everywhere.
pub(crate) fn flat_array_locals(
    block: &Block,
    is_unboxed_struct: &dyn Fn(&str) -> bool,
) -> HashSet<String> {
    use zz_frontend::ast::TyKind;

    // Pass 1: candidate declarations (`name: [T] = []` with a flat element).
    // Value = true when the element is a scalar (bare `arr[i]` reads are
    // supported); struct elements only support `arr[i].field`.
    let mut candidates: HashMap<String, bool> = HashMap::new();
    fn collect(block: &Block, out: &mut HashMap<String, bool>, is_unboxed: &dyn Fn(&str) -> bool) {
        for stmt in &block.stmts {
            match stmt {
                Stmt::Decl {
                    ty, name, value, ..
                } => {
                    if matches!(value, Expr::Array { elems, .. } if elems.is_empty()) {
                        if let Some(ty) = ty {
                            if let TyKind::Array(inner) = &ty.kind {
                                let flat = match &inner.kind {
                                    TyKind::Int | TyKind::Float | TyKind::Bool => Some(true),
                                    TyKind::Named(n, _) if is_unboxed(n) => Some(false),
                                    _ => None,
                                };
                                if let Some(scalar) = flat {
                                    out.insert(name.name.clone(), scalar);
                                }
                            }
                        }
                    }
                }
                Stmt::For { body, .. } => collect(body, out, is_unboxed),
                Stmt::Expr(Expr::Block(b)) => collect(b, out, is_unboxed),
                Stmt::Expr(Expr::If { then, els, .. }) => {
                    collect(then, out, is_unboxed);
                    if let Some(Expr::Block(b)) = els.as_deref() {
                        collect(b, out, is_unboxed);
                    }
                }
                Stmt::Expr(Expr::While { body, .. }) => collect(body, out, is_unboxed),
                _ => {}
            }
        }
    }
    collect(block, &mut candidates, is_unboxed_struct);
    if candidates.is_empty() {
        return HashSet::new();
    }

    // Pass 2: disqualify any candidate with an unsupported mention.
    let mut bad: HashSet<String> = HashSet::new();
    {
        let mut scan = FlatUseScan {
            cand: &candidates,
            bad: &mut bad,
            escaping: false,
            inferred_layout: false,
            row_cand: None,
            rows: None,
            declared_rows: vec![HashSet::new()],
            row_ok: None,
        };
        scan.block(block);
    }
    candidates.retain(|n, _| !bad.contains(n));
    candidates.into_keys().collect()
}

/// Use scan for arrays inferred from usage (untyped `name := []` declarations
/// whose pushes reveal a flat element type).
///
/// This layout has no boxed fallback at any mention, so the scan is stricter
/// than the typed contract in one direction (any plain mention disqualifies)
/// and more permissive in another: the inferred layout implements
/// `arr[i] = v` stores and the self-append assignment
/// (`arr = vec.push(arr, x)` / `arr = arr.push(x)`), so those shapes are
/// allowed here. `cand` maps each candidate to whether its element is a
/// scalar (struct elements cannot be index-stored).
///
/// Returns the candidates with an unsupported mention; callers keep those
/// boxed.
pub(crate) fn untyped_flat_uses_ok(block: &Block, cand: &HashMap<String, bool>) -> HashSet<String> {
    let mut bad: HashSet<String> = HashSet::new();
    {
        let mut scan = FlatUseScan {
            cand,
            bad: &mut bad,
            escaping: false,
            inferred_layout: true,
            row_cand: None,
            rows: None,
            declared_rows: vec![HashSet::new()],
            row_ok: None,
        };
        scan.block(block);
    }
    bad
}

/// Use scan for arrays inferred to hold *flat scalar rows* — the unboxed
/// nested-array layout (`a := []` whose every push appends a flat scalar
/// array, lowered to `zz_flat_row *`).
///
/// `rows` maps this body's phase-1 flat scalar locals to their element C
/// type; `cand` is the set of still-unclassified `name := []` candidates.
/// Only shapes the nested lowering implements are allowed:
///
/// - the candidate's own `a := []`,
/// - `a = vec.push(a, row)` / `a = a.push(row)` / the statement forms, where
///   `row` is a flat scalar local declared before the push,
/// - `a[i][j]` reads and `a[i][j] = v` stores,
/// - `len(a)`.
///
/// Every other mention disqualifies the candidate, because the row layout has
/// no boxed fallback. Returns the supported candidates mapped to the inner
/// row element C type.
pub(crate) fn untyped_flat_rows_uses_ok(
    block: &Block,
    rows: &HashMap<String, String>,
    cand: &HashSet<String>,
) -> HashMap<String, String> {
    let scalar_cand: HashMap<String, bool> = HashMap::new();
    let mut bad: HashSet<String> = HashSet::new();
    let mut row_ok: HashMap<String, String> = HashMap::new();
    {
        let mut scan = FlatUseScan {
            cand: &scalar_cand,
            bad: &mut bad,
            escaping: false,
            inferred_layout: true,
            row_cand: Some(cand),
            rows: Some(rows),
            declared_rows: vec![HashSet::new()],
            row_ok: Some(&mut row_ok),
        };
        scan.block(block);
    }
    row_ok.retain(|n, _| !bad.contains(n));
    row_ok
}

/// Element expression of a self-append assignment
/// `var = vec.push(var, e)` / `var = std.vec.push(var, e)` /
/// `var = var.push(e)`.
///
/// Mirrors `zz_frontend::move_elide::classify_self_assign` so the scan never
/// allows a shape the lowering cannot emit: no named arguments and exactly one
/// reference to `var` (the receiver), which keeps the element from observing
/// the moved-from slot.
fn self_push_elem<'e>(var: &str, rhs: &'e Expr) -> Option<&'e Expr> {
    let Expr::Call {
        callee,
        args,
        named,
        ..
    } = rhs
    else {
        return None;
    };
    if !named.is_empty() {
        return None;
    }
    // Exactly one reference to the array (the receiver): the lowering's
    // take-before-store window is only sound when the element cannot
    // observe the moved-from slot.
    if count_refs(rhs, var) != 1 {
        return None;
    }
    let fn_push = match callee.as_ref() {
        Expr::Ident { name, .. } => name == "vec.push" || name == "std.vec.push",
        Expr::Path { parts, .. } => {
            matches!(parts.as_slice(), [m, p] if m == "vec" && p == "push")
                || matches!(parts.as_slice(), [s, m, p] if s == "std" && m == "vec" && p == "push")
        }
        _ => false,
    };
    if fn_push && args.len() == 2 {
        return match &args[0] {
            Expr::Ident { name, .. } if name == var => Some(&args[1]),
            _ => None,
        };
    }
    let method_push = match callee.as_ref() {
        Expr::Path { parts, .. } => parts.len() == 2 && parts[0] == var && parts[1] == "push",
        Expr::Field { obj, name, .. } => {
            name == "push" && matches!(obj.as_ref(), Expr::Ident { name: n, .. } if n == var)
        }
        _ => false,
    };
    if method_push && args.len() == 1 {
        return Some(&args[0]);
    }
    None
}

/// Receiver name and element argument of any push spelling: the function
/// forms `vec.push(recv, x)` / `std.vec.push(recv, x)` / `ns.push(recv, x)`,
/// and the method forms `recv.push(x)` / `recv.push` as a field call. Used by
/// the row scan to validate statement-form appends, where the element must be
/// a specific row local (unlike the scalar scan, which only looks at types).
fn push_recv_arg(rhs: &Expr) -> Option<(&str, &Expr)> {
    let Expr::Call {
        callee,
        args,
        named,
        ..
    } = rhs
    else {
        return None;
    };
    if !named.is_empty() {
        return None;
    }
    let fn_push = match callee.as_ref() {
        Expr::Ident { name, .. } => name == "vec.push" || name == "std.vec.push",
        Expr::Path { parts, .. } => {
            matches!(parts.as_slice(), [m, p] if m == "vec" && p == "push")
                || matches!(parts.as_slice(), [s, m, p] if s == "std" && m == "vec" && p == "push")
        }
        _ => false,
    };
    if fn_push && args.len() == 2 {
        return match &args[0] {
            Expr::Ident { name, .. } => Some((name.as_str(), &args[1])),
            _ => None,
        };
    }
    let method_recv = match callee.as_ref() {
        Expr::Path { parts, .. } if parts.len() == 2 && parts[1] == "push" && args.len() == 1 => {
            Some(parts[0].as_str())
        }
        Expr::Field { obj, name, .. } if name == "push" && args.len() == 1 => match obj.as_ref() {
            Expr::Ident { name, .. } => Some(name.as_str()),
            _ => None,
        },
        _ => None,
    };
    method_recv.map(|r| (r, &args[0]))
}

/// Program-wide version of [`flat_array_locals`] for module-level arrays:
/// a top-level `name: [scalar|unboxed-struct] = []` may lower to the flat
/// layout only when *every* mention of that name in the whole program is an
/// operation the flat layout implements (`push`, integer index reads,
/// `len`, `for x in arr`, `arr[i].field`).
///
/// Globals have no flat → `zz_value` materialization at boxed boundaries
/// (call arguments, returns, slice containers, whole rebinding, printing),
/// so unlike locals — which fall back to boxing per mention — one
/// unsupported mention keeps the global boxed for the entire program. That
/// conservative direction is always correct, just slower.
///
/// Only bare (non-namespaced) declarations are considered: qualified
/// `mod.arr` globals are reached through `Path` nodes whose leaf spelling
/// can collide with a bare name, so they stay boxed.
pub(crate) fn flat_global_arrays(
    program: &Program,
    is_unboxed_struct: &dyn Fn(&str) -> bool,
) -> HashSet<String> {
    use zz_frontend::ast::TyKind;

    // Pass 1: candidate top-level declarations.
    let mut candidates: HashMap<String, bool> = HashMap::new();
    for stmt in &program.stmts {
        let Stmt::Decl {
            ty, name, value, ..
        } = stmt
        else {
            continue;
        };
        if name.name.contains('.') {
            continue;
        }
        if !matches!(value, Expr::Array { elems, .. } if elems.is_empty()) {
            continue;
        }
        let Some(ty) = ty else { continue };
        let TyKind::Array(inner) = &ty.kind else {
            continue;
        };
        let flat = match &inner.kind {
            TyKind::Int | TyKind::Float | TyKind::Bool => Some(true),
            TyKind::Named(n, _) if is_unboxed_struct(n) => Some(false),
            _ => None,
        };
        if let Some(scalar) = flat {
            candidates.insert(name.name.clone(), scalar);
        }
    }
    if candidates.is_empty() {
        return HashSet::new();
    }

    // Pass 2: disqualify candidates used anywhere in an unsupported shape.
    // Function/method bodies are scanned with `escaping == false` (a global
    // is addressable from any frame, unlike a captured local); closures and
    // nested functions inside them still raise the capture barrier.
    let mut bad: HashSet<String> = HashSet::new();
    {
        let mut scan = FlatUseScan {
            cand: &candidates,
            bad: &mut bad,
            escaping: false,
            inferred_layout: false,
            row_cand: None,
            rows: None,
            declared_rows: vec![HashSet::new()],
            row_ok: None,
        };
        for stmt in &program.stmts {
            match stmt {
                Stmt::Func { body, .. } => scan.block(body),
                Stmt::Impl { methods, .. } => {
                    for m in methods {
                        if let Stmt::Func { body, .. } = m {
                            scan.block(body);
                        }
                    }
                }
                other => scan.stmt(other),
            }
        }
    }
    candidates.retain(|n, _| !bad.contains(n));
    candidates.into_keys().collect()
}

/// Walks a body marking candidate flat arrays that appear in a position the
/// flat layout cannot serve. `escaping` is set inside closures and `defer`
/// bodies, where any mention (even `push`) is a capture.
struct FlatUseScan<'a> {
    cand: &'a HashMap<String, bool>,
    bad: &'a mut HashSet<String>,
    escaping: bool,
    /// Enable the shapes only the inferred layout implements: `arr[i] = v`
    /// stores (scalar elements only) and the self-append assignment. The
    /// typed [`flat_array_locals`] contract keeps this off; see
    /// [`untyped_flat_uses_ok`].
    inferred_layout: bool,
    /// Flat-row candidates (`zz_flat_row *`). Populated only by
    /// [`untyped_flat_rows_uses_ok`]; `cand` is empty in that scan so none of
    /// the scalar shapes can match a row name by accident.
    row_cand: Option<&'a HashSet<String>>,
    /// Flat scalar locals of the body. Row pushes are only sound when the
    /// pushed element is one of these (a flat row buffer with a known length).
    rows: Option<&'a HashMap<String, String>>,
    /// Names declared so far in the enclosing blocks. A row (or row-array)
    /// push before the declaration would lower against an identifier that is
    /// not in C scope, so order is part of the contract.
    declared_rows: Vec<HashSet<String>>,
    /// Accepted row candidates: name -> inner row element C type.
    row_ok: Option<&'a mut HashMap<String, String>>,
}

impl<'a> FlatUseScan<'a> {
    fn is_cand(&self, name: &str) -> bool {
        self.cand.contains_key(name)
    }

    /// True when the candidate's element is a scalar (bare `arr[i]` reads
    /// are supported). Struct elements have no boxed element form here.
    fn is_scalar_cand(&self, name: &str) -> bool {
        self.cand.get(name).copied().unwrap_or(false)
    }

    /// True when `name` is a flat-row candidate of the current scan.
    fn is_row_cand(&self, name: &str) -> bool {
        self.row_cand.map_or(false, |c| c.contains(name))
    }

    /// True when `name` was already declared in an enclosing block.
    fn declared_in_scope(&self, name: &str) -> bool {
        self.declared_rows.iter().any(|s| s.contains(name))
    }

    /// Validate one `a = vec.push(a, row)` / `a = a.push(row)` append on a
    /// flat-row candidate: the element must be a flat scalar local declared
    /// before the push (so the lowering can read its buffer and length), and
    /// every append must agree on the inner row element type.
    fn record_row_push(&mut self, outer: &str, arg: &Expr) {
        if !self.declared_in_scope(outer) {
            self.mention(outer);
            return;
        }
        let Expr::Ident { name: row, .. } = arg else {
            self.mention(outer);
            return;
        };
        let Some(inner) = self.rows.and_then(|r| r.get(row)).cloned() else {
            self.mention(outer);
            return;
        };
        match self.row_ok.as_ref().and_then(|m| m.get(outer)).cloned() {
            Some(prev) if prev != inner => self.mention(outer),
            Some(_) => {}
            None => {
                if let Some(map) = self.row_ok.as_mut() {
                    map.insert(outer.to_string(), inner);
                } else {
                    self.mention(outer);
                }
            }
        }
    }

    fn mention(&mut self, name: &str) {
        if self.is_cand(name) || self.is_row_cand(name) {
            self.bad.insert(name.to_string());
        }
    }

    /// Any expression mentioning a candidate disqualifies it (lvalue,
    /// closure capture, deferred body, comprehension source, …).
    fn poison_expr(&mut self, e: &Expr) {
        match e {
            Expr::Ident { name, .. } => self.mention(name),
            Expr::Index { obj, .. } | Expr::Field { obj, .. } | Expr::Slice { obj, .. } => {
                self.poison_expr(obj);
            }
            Expr::Path { parts, .. } => {
                for p in parts {
                    self.mention(p);
                }
            }
            _ => self.expr(e),
        }
    }

    fn poison_block(&mut self, b: &Block) {
        let saved = self.escaping;
        self.escaping = true;
        self.block(b);
        self.escaping = saved;
    }

    fn poison_pattern(&mut self, p: &Pattern) {
        match p {
            Pattern::Binding { name } => self.mention(&name.name),
            Pattern::Tuple { pats, .. } | Pattern::Or { pats, .. } => {
                for p in pats {
                    self.poison_pattern(p);
                }
            }
            Pattern::Variant { arg, .. } => {
                if let Some(p) = arg {
                    self.poison_pattern(p);
                }
            }
            Pattern::Wildcard { .. } | Pattern::Literal { .. } => {}
        }
    }

    fn block(&mut self, b: &Block) {
        self.declared_rows.push(HashSet::new());
        for stmt in &b.stmts {
            self.stmt(stmt);
        }
        self.declared_rows.pop();
    }

    fn stmt(&mut self, stmt: &Stmt) {
        match stmt {
            Stmt::Decl { name, value, .. } => {
                // A redeclaration shadows the candidate: its later uses are
                // not the array, so keep the array boxed (conservative). The
                // candidate's own `arr: [T] = []` declaration is exempt.
                let own_flat_decl = (self.is_cand(&name.name) || self.is_row_cand(&name.name))
                    && matches!(value, Expr::Array { elems, .. } if elems.is_empty());
                if !own_flat_decl {
                    self.mention(&name.name);
                }
                self.expr(value);
                // Row buffers must be declared before they are pushed (the
                // lowering reads `row` + `row_len` at the push site).
                if self.rows.map_or(false, |r| r.contains_key(&name.name)) {
                    if let Some(top) = self.declared_rows.last_mut() {
                        top.insert(name.name.clone());
                    }
                }
                // Row-array candidates: `a := []` is what later pushes bind to.
                if own_flat_decl && self.is_row_cand(&name.name) {
                    if let Some(top) = self.declared_rows.last_mut() {
                        top.insert(name.name.clone());
                    }
                }
            }
            Stmt::For {
                vars, iter, body, ..
            } => {
                // Iterating the array itself is the one supported use.
                match iter.as_ref() {
                    Expr::Ident { name, .. } if self.is_cand(name) && !self.escaping => {}
                    _ => self.expr(iter),
                }
                for v in vars {
                    self.mention(&v.name);
                }
                self.block(body);
            }
            Stmt::Return { value, .. } => {
                if let Some(v) = value {
                    self.poison_expr(v);
                }
            }
            Stmt::Assign { target, value, .. } => {
                // Self-append (`arr = vec.push(arr, x)` / `arr = arr.push(x)`):
                // implemented only by the inferred layout, where the whole
                // assignment lowers inline — scan just the element (it must
                // not mention the array; the take precedes the store).
                if self.inferred_layout && !self.escaping {
                    if let Expr::Ident { name, .. } = target {
                        if self.is_row_cand(name) {
                            if let Some(elem) = self_push_elem(name, value) {
                                self.record_row_push(name, elem);
                                return;
                            }
                        }
                        if self.is_cand(name) {
                            if let Some(elem) = self_push_elem(name, value) {
                                self.expr(elem);
                                return;
                            }
                        }
                    }
                }
                // `rows[i][j] = v` on a flat-row outer: the row buffer is
                // reached directly, so scan the outer index, the inner index
                // and the value; the receiver is consumed.
                if !self.escaping {
                    if let Expr::Index { obj, index, .. } = target {
                        if let Expr::Index {
                            obj: root, index: i, ..
                        } = obj.as_ref()
                        {
                            if let Expr::Ident { name, .. } = root.as_ref() {
                                if self.is_row_cand(name) {
                                    self.expr(i);
                                    self.expr(index);
                                    self.expr(value);
                                    return;
                                }
                            }
                        }
                    }
                }
                // `arr[i] = v` (scalar flat elements): implemented as a direct
                // buffer store, so the base is consumed and only the index and
                // the value need scanning. Struct elements have no flat store
                // yet, hence the scalar check.
                if self.inferred_layout && !self.escaping {
                    if let Expr::Index { obj, index, .. } = target {
                        if let Expr::Ident { name, .. } = obj.as_ref() {
                            if self.is_scalar_cand(name) {
                                self.expr(index);
                                self.expr(value);
                                return;
                            }
                        }
                    }
                }
                // Any other store (`b = arr`, `arr = other`, `arr += x`, …) is
                // not implemented on the flat layout: the target is a poison
                // position.
                self.poison_expr(target);
                self.expr(value);
            }
            Stmt::CompoundAssign { target, value, .. } => {
                // Compounds are not implemented on the flat layout.
                self.poison_expr(target);
                self.expr(value);
            }
            Stmt::Destructure { pat, value, .. } => {
                self.poison_pattern(pat);
                self.expr(value);
            }
            Stmt::Defer { expr, .. } => {
                let saved = self.escaping;
                self.escaping = true;
                self.expr(expr);
                self.escaping = saved;
            }
            Stmt::Expr(e) => {
                // Statement-form row append (`a.push(row)`,
                // `vec.push(a, row)`, …): an in-place mutation, so the
                // receiver is consumed. Validate the element; a value-position
                // push (e.g. `if a.push(row)`) falls through to `expr` and
                // disqualifies the candidate, because that path boxes a
                // `zz_value` result the row layout cannot produce.
                if !self.escaping {
                    if let Some((recv, arg)) = push_recv_arg(e) {
                        if self.is_row_cand(recv) {
                            self.record_row_push(recv, arg);
                            return;
                        }
                    }
                }
                self.expr(e)
            }
            Stmt::Func { body, .. } => self.poison_block(body),
            Stmt::Impl { methods, .. } => {
                for m in methods {
                    if let Stmt::Func { body, .. } = m {
                        self.poison_block(body);
                    }
                }
            }
            Stmt::Break { .. }
            | Stmt::Continue { .. }
            | Stmt::Import { .. }
            | Stmt::Struct { .. }
            | Stmt::TypeAlias { .. }
            | Stmt::Enum { .. }
            | Stmt::ExternBlock { .. }
            | Stmt::Link { .. } => {}
        }
    }

    fn expr(&mut self, e: &Expr) {
        match e {
            Expr::Ident { name, .. } => self.mention(name),
            Expr::Call {
                callee,
                args,
                named,
                ..
            } => {
                // `len(arr)` consumes its argument as a length read, so the
                // generic argument scan below must skip it.
                let mut args_consumed = false;
                match callee.as_ref() {
                    // `arr.push(x)` — the supported append form.
                    Expr::Path { parts, .. }
                        if parts.len() == 2 && self.is_cand(&parts[0]) && !self.escaping =>
                    {
                        if parts[1] != "push" || args.len() != 1 || !named.is_empty() {
                            self.mention(&parts[0]);
                        }
                    }
                    Expr::Field { obj, name, .. } => match obj.as_ref() {
                        Expr::Ident { name: recv, .. } if self.is_cand(recv) && !self.escaping => {
                            if name != "push" || args.len() != 1 || !named.is_empty() {
                                self.mention(recv);
                            }
                        }
                        _ => self.expr(obj),
                    },
                    // `len(arr)` — a pure read of the length counter.
                    Expr::Ident {
                        name: callee_name, ..
                    } if callee_name == "len" && args.len() == 1 => match &args[0] {
                        Expr::Ident { name, .. }
                            if self.is_cand(name) || self.is_row_cand(name) =>
                        {
                            args_consumed = true;
                        }
                        other => self.expr(other),
                    },
                    other => self.expr(other),
                }
                if !args_consumed {
                    for a in args {
                        self.expr(a);
                    }
                }
                for (_, v) in named {
                    self.expr(v);
                }
            }
            // `arr[i]` reads only; the index expression itself is scanned.
            // Bare struct elements have no boxed form, so only scalar
            // element arrays may be indexed directly (struct arrays are
            // read through `arr[i].field`, handled above).
            Expr::Index { obj, index, .. } => {
                // `rows[i][j]` on a flat-row outer: the row handle is reached
                // directly and both indices are scanned; the receiver is
                // consumed (no boxed `rows[i]` value is ever materialized).
                if !self.escaping {
                    if let Expr::Index {
                        obj: root, index: i, ..
                    } = obj.as_ref()
                    {
                        if let Expr::Ident { name, .. } = root.as_ref() {
                            if self.is_row_cand(name) {
                                self.expr(i);
                                self.expr(index);
                                return;
                            }
                        }
                    }
                }
                match obj.as_ref() {
                    Expr::Ident { name, .. } if self.is_scalar_cand(name) && !self.escaping => {}
                    _ => self.expr(obj),
                }
                self.expr(index);
            }
            Expr::Slice {
                obj, start, end, ..
            } => {
                self.poison_expr(obj);
                if let Some(s) = start {
                    self.expr(s);
                }
                if let Some(s) = end {
                    self.expr(s);
                }
            }
            Expr::Closure { params, body, .. } => {
                for p in params {
                    self.mention(&p.name.name);
                }
                let saved = self.escaping;
                self.escaping = true;
                self.expr(body);
                self.escaping = saved;
            }
            Expr::Block(b) => self.block(b),
            Expr::If {
                cond, then, els, ..
            } => {
                self.expr(cond);
                self.block(then);
                if let Some(e) = els {
                    self.expr(e);
                }
            }
            Expr::While { cond, body, .. } => {
                self.expr(cond);
                self.block(body);
            }
            Expr::IfLet {
                pat,
                value,
                then,
                els,
                ..
            } => {
                self.poison_pattern(pat);
                self.expr(value);
                self.block(then);
                if let Some(e) = els {
                    self.expr(e);
                }
            }
            Expr::Match {
                scrutinee, arms, ..
            } => {
                self.expr(scrutinee);
                for arm in arms {
                    self.poison_pattern(&arm.pat);
                    if let Some(g) = &arm.guard {
                        self.expr(g);
                    }
                    self.expr(&arm.body);
                }
            }
            Expr::ListComp {
                body,
                var,
                iter,
                filter,
                ..
            } => {
                // The comprehension iterates through the boxed protocol.
                self.poison_expr(iter);
                self.mention(&var.name);
                self.expr(body);
                if let Some(f) = filter {
                    self.expr(f);
                }
            }
            Expr::Fmt { parts, .. } => {
                for p in parts {
                    if let FmtPart::Expr(e, _) = p {
                        self.expr(e);
                    }
                }
            }
            Expr::Paren { expr, .. }
            | Expr::Unary { expr, .. }
            | Expr::Try { expr, .. }
            | Expr::Variant {
                arg: Some(expr), ..
            } => self.expr(expr),
            Expr::Binary { left, right, .. } => {
                self.expr(left);
                self.expr(right);
            }
            Expr::Tuple { items, .. } => {
                for i in items {
                    self.expr(i);
                }
            }
            Expr::Array { elems, .. } => {
                for i in elems {
                    self.expr(i);
                }
            }
            Expr::Dict { entries, .. } => {
                for (k, v) in entries {
                    self.expr(k);
                    self.expr(v);
                }
            }
            // `arr[i].field` — a field read off a flat element (the struct
            // fast path indexes the buffer directly).
            Expr::Field { obj, .. } => match obj.as_ref() {
                Expr::Index {
                    obj: base, index, ..
                } => match base.as_ref() {
                    Expr::Ident { name, .. } if self.is_cand(name) && !self.escaping => {
                        self.expr(index);
                    }
                    _ => self.poison_expr(obj),
                },
                _ => self.poison_expr(obj),
            },
            Expr::Range { start, end, .. } => {
                self.expr(start);
                self.expr(end);
            }
            Expr::StructInit { fields, .. } => {
                for (_, v) in fields {
                    self.expr(v);
                }
            }
            Expr::Path { parts, .. } => {
                for p in parts {
                    self.mention(p);
                }
            }
            Expr::Int { .. }
            | Expr::Float { .. }
            | Expr::Str { .. }
            | Expr::Bool { .. }
            | Expr::Variant { arg: None, .. }
            | Expr::Break { .. }
            | Expr::Continue { .. } => {}
        }
    }
}

/// Compute per-parameter read-only masks for every top-level function:
/// name → one bool per signature parameter, in order.
///
/// Needs checker signatures (for parameter types) alongside AST bodies.
/// Optimistic start (all read-only), pruned to fixpoint: each round
/// re-walks every function against current callee verdicts. Monotone
/// shrinking guarantees termination; the final round with no changes is
/// self-consistent, which makes mutual-recursion verdicts sound.
pub(crate) fn analyze(
    program: &Program,
    funcs: &HashMap<String, zz_checker::FuncSig>,
    structs: &HashMap<String, zz_checker::StructSig>,
) -> HashMap<String, Vec<bool>> {
    // Bodies by qualified name.
    let mut bodies: HashMap<String, &Block> = HashMap::new();
    for stmt in &program.stmts {
        if let Stmt::Func { name, body, .. } = stmt {
            bodies.insert(name.join("."), body);
        }
    }
    // Signatures present in the seed. Only functions with BOTH body and
    // signature get masks; everything else (natives, methods, externs,
    // missing) keeps today's clone behavior. Optimistic start (all
    // read-only): the fixpoint below prunes on escape evidence.
    let mut masks: HashMap<String, Vec<bool>> = HashMap::new();
    for (name, sig) in funcs {
        if !bodies.contains_key(name) {
            continue;
        }
        masks.insert(name.clone(), vec![true; sig.params.len()]);
    }
    // Fixpoint: re-walk every not-yet-read-only heap parameter until
    // no verdict changes. Cap is just a backstop; monotone shrinking
    // converges in at most (functions × params) rounds.
    let cap = masks.len() * 4 + 4;
    for _ in 0..cap {
        let mut changed = false;
        // Snapshot callee verdicts for this round.
        let round = masks.clone();
        for (name, mask) in masks.iter_mut() {
            let Some(body) = bodies.get(name) else {
                continue;
            };
            let Some(sig) = funcs.get(name) else {
                continue;
            };
            for (i, (_, ty)) in sig.params.iter().enumerate() {
                if !mask[i] || type_is_inline(ty, structs) {
                    continue;
                }
                let params: Vec<(String, Type)> = sig
                    .params
                    .iter()
                    .map(|(n, t)| (n.clone(), t.clone()))
                    .collect();
                if !param_is_readonly(&params, i, body, &round) {
                    mask[i] = false;
                    changed = true;
                }
            }
        }
        if !changed {
            break;
        }
    }
    masks
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Analyze `src` with explicit (name → params) signatures. Bodies come
    /// from parsing; types from the spec.
    fn analyze_src(src: &str, sigs: &[(&str, Vec<(&str, Type)>)]) -> HashMap<String, Vec<bool>> {
        let parsed = zz_frontend::parse(src);
        assert!(
            parsed.errors.is_empty(),
            "parse errors: {:?}",
            parsed.errors
        );
        let mut funcs = HashMap::new();
        for (name, params) in sigs {
            funcs.insert(
                name.to_string(),
                zz_checker::FuncSig {
                    is_extern: false,
                    readonly: false,
                    extern_c_symbol: None,
                    generics: vec![],
                    bounds: vec![],
                    params: params
                        .iter()
                        .map(|(n, t)| (n.to_string(), t.clone()))
                        .collect(),
                    has_default: vec![],
                    ret: Type::Int,
                },
            );
        }
        analyze(&parsed.program, &funcs, &HashMap::new())
    }

    fn rng() -> Type {
        Type::Struct("Rng".to_string(), vec![])
    }

    /// `flat_array_locals` over the parsed `main` body of `src`.
    fn flat_names_with(src: &str, is_unboxed: &dyn Fn(&str) -> bool) -> HashSet<String> {
        let parsed = zz_frontend::parse(src);
        assert!(
            parsed.errors.is_empty(),
            "parse errors: {:?}",
            parsed.errors
        );
        let body = parsed
            .program
            .stmts
            .iter()
            .find_map(|s| match s {
                Stmt::Func { name, body, .. } if name.join(".") == "main" => Some(body),
                _ => None,
            })
            .expect("main body");
        flat_array_locals(body, is_unboxed)
    }

    fn flat_names(src: &str) -> HashSet<String> {
        flat_names_with(src, &|_| false)
    }

    /// `untyped_flat_uses_ok` over the parsed `main` body of `src`, with
    /// `cand` naming the inferred candidates (name, is-scalar-element).
    fn untyped_bad(src: &str, cand: &[(&str, bool)]) -> HashSet<String> {
        let parsed = zz_frontend::parse(src);
        assert!(
            parsed.errors.is_empty(),
            "parse errors: {:?}",
            parsed.errors
        );
        let body = parsed
            .program
            .stmts
            .iter()
            .find_map(|s| match s {
                Stmt::Func { name, body, .. } if name.join(".") == "main" => Some(body),
                _ => None,
            })
            .expect("main body");
        let map: HashMap<String, bool> = cand
            .iter()
            .map(|(name, scalar)| (name.to_string(), *scalar))
            .collect();
        untyped_flat_uses_ok(body, &map)
    }

    /// `flat_global_arrays` over a parsed program (bare globals only).
    fn flat_global_names(src: &str) -> HashSet<String> {
        let parsed = zz_frontend::parse(src);
        assert!(
            parsed.errors.is_empty(),
            "parse errors: {:?}",
            parsed.errors
        );
        flat_global_arrays(&parsed.program, &|_| false)
    }

    #[test]
    fn flat_global_supported_uses_are_eligible() {
        let names = flat_global_names(
            "arr: [int] = []
             func main() {
                for i in 0..3 { arr.push(i) }
                total := 0
                for x in arr { total = total + x }
                println(total)
                println(len(arr))
                println(arr[1])
             }",
        );
        assert_eq!(names, HashSet::from(["arr".to_string()]));
    }

    #[test]
    fn flat_global_call_argument_disqualifies() {
        let names = flat_global_names(
            "arr: [int] = []
             func main() { arr.push(1)\nprintln(arr) }",
        );
        assert!(names.is_empty(), "expected boxed, got {names:?}");
    }

    #[test]
    fn flat_global_index_store_disqualifies() {
        let names = flat_global_names(
            "arr: [int] = []
             func main() { arr.push(1)\narr[0] = 2 }",
        );
        assert!(names.is_empty(), "expected boxed, got {names:?}");
    }

    #[test]
    fn flat_global_untyped_decl_is_not_a_candidate() {
        let names = flat_global_names(
            "arr := []
             func main() { arr.push(1) }",
        );
        assert!(names.is_empty(), "expected boxed, got {names:?}");
    }

    #[test]
    fn flat_array_supported_uses_are_eligible() {
        let names = flat_names(
            "func main() {
                arr: [int] = []
                for i in 0..3 { arr.push(i) }
                total := 0
                for x in arr { total = total + x }
                println(total)
                println(len(arr))
                println(arr[1])
            }",
        );
        assert!(names.contains("arr"), "expected flat, got {names:?}");
    }

    #[test]
    fn flat_array_index_store_disqualifies() {
        let names = flat_names("func main() { arr: [int] = []\narr.push(1)\narr[0] = 2 }");
        assert!(names.is_empty(), "expected boxed, got {names:?}");
    }

    #[test]
    fn flat_array_call_argument_disqualifies() {
        let names =
            flat_names("func main() { arr: [int] = []\narr.push(1)\nprintln(arr)\nlet_sum(arr) }");
        assert!(names.is_empty(), "expected boxed, got {names:?}");
    }

    #[test]
    fn flat_array_return_disqualifies() {
        let names = flat_names("func main() { arr: [int] = []\narr.push(1)\nreturn arr }");
        assert!(names.is_empty(), "expected boxed, got {names:?}");
    }

    #[test]
    fn flat_array_slice_disqualifies() {
        let names = flat_names("func main() { arr: [int] = []\narr.push(1)\nprintln(arr[0:1]) }");
        assert!(names.is_empty(), "expected boxed, got {names:?}");
    }

    #[test]
    fn flat_array_other_method_disqualifies() {
        let names = flat_names("func main() { arr: [int] = []\narr.push(1)\narr.pop() }");
        assert!(names.is_empty(), "expected boxed, got {names:?}");
    }

    #[test]
    fn flat_array_closure_capture_disqualifies() {
        let names = flat_names(
            "func main() { arr: [int] = []\narr.push(1)\nf := || arr[0]\nprintln(f()) }",
        );
        assert!(names.is_empty(), "expected boxed, got {names:?}");
    }

    #[test]
    fn flat_array_untyped_empty_stays_boxed() {
        let names = flat_names("func main() { arr := []\narr.push(1)\nprintln(len(arr)) }");
        assert!(names.is_empty(), "expected boxed, got {names:?}");
    }

    #[test]
    fn flat_struct_array_eligible_when_element_is_unboxed() {
        let src = "func main() {
            pts: [Pt] = []
            pts.push(Pt{ x: 1, y: 2 })
            for p in pts { println(p.x) }
            println(pts[0].x)
        }";
        let unboxed = flat_names_with(src, &|n| n == "Pt");
        assert!(unboxed.contains("pts"), "expected flat, got {unboxed:?}");
        // Boxed element type: no flat lowering.
        assert!(flat_names(src).is_empty());
    }

    #[test]
    fn flat_array_redeclaration_disqualifies() {
        let names = flat_names(
            "func main() { arr: [int] = []\narr.push(1)\nif true { arr := [1, 2]\nprintln(len(arr)) } }",
        );
        assert!(names.is_empty(), "expected boxed, got {names:?}");
    }

    #[test]
    fn untyped_self_push_and_index_store_are_eligible() {
        let bad = untyped_bad(
            "func main() {
                flags := []
                for i in 0..3 { flags = vec.push(flags, true) }
                flags[0] = false
                for x in flags { println(x) }
                println(len(flags))
                println(flags[1])
            }",
            &[("flags", true)],
        );
        assert!(bad.is_empty(), "expected flat, got {bad:?}");
    }

    #[test]
    fn untyped_method_self_push_is_eligible() {
        let bad = untyped_bad(
            "func main() {
                arr := []
                for i in 0..3 { arr = arr.push(i) }
                println(len(arr))
            }",
            &[("arr", true)],
        );
        assert!(bad.is_empty(), "expected flat, got {bad:?}");
    }

    #[test]
    fn untyped_struct_index_store_disqualifies() {
        // Struct elements have no flat index store yet.
        let bad = untyped_bad(
            "func main() {
                pts := []
                for i in 0..3 { pts = vec.push(pts, Pt{ x: 1, y: 2 }) }
                pts[0] = Pt{ x: 3, y: 4 }
            }",
            &[("pts", false)],
        );
        assert!(bad.contains("pts"), "expected boxed, got {bad:?}");
    }

    #[test]
    fn untyped_standalone_push_call_disqualifies() {
        // Only the self-append *assignment* lowers; a bare push statement
        // would reach the boxed native.
        let bad = untyped_bad(
            "func main() {
                arr := []
                vec.push(arr, 1)
            }",
            &[("arr", true)],
        );
        assert!(bad.contains("arr"), "expected boxed, got {bad:?}");
    }

    #[test]
    fn untyped_whole_assignment_disqualifies() {
        let bad = untyped_bad(
            "func main() {
                arr := []
                other := []
                arr = other
            }",
            &[("arr", true)],
        );
        assert!(bad.contains("arr"), "expected boxed, got {bad:?}");
    }

    #[test]
    fn untyped_element_mentioning_array_disqualifies() {
        // `arr = vec.push(arr, len(arr))` would read a moved-from slot; the
        // single-reference check in `self_push_elem` catches it.
        let bad = untyped_bad(
            "func main() {
                arr := []
                arr = vec.push(arr, len(arr))
            }",
            &[("arr", true)],
        );
        assert!(bad.contains("arr"), "expected boxed, got {bad:?}");
    }

    #[test]
    fn untyped_compound_index_store_disqualifies() {
        let bad = untyped_bad(
            "func main() {
                arr := []
                arr[0] += 1
            }",
            &[("arr", true)],
        );
        assert!(bad.contains("arr"), "expected boxed, got {bad:?}");
    }

    #[test]
    fn untyped_plain_mention_disqualifies() {
        let bad = untyped_bad(
            "func main() {
                arr := []
                println(arr)
            }",
            &[("arr", true)],
        );
        assert!(bad.contains("arr"), "expected boxed, got {bad:?}");
    }

    #[test]
    fn pure_reads_are_readonly() {
        // The issue shape: field reads + int arithmetic only.
        let m = analyze_src(
            "func next(r: Rng) -> (int, Rng) { return (r.s0 + 1, Rng{ s0: r.s0 + 1 }) }",
            &[("next", vec![("r", rng())])],
        );
        assert_eq!(m["next"], vec![true]);
    }

    #[test]
    fn returned_param_escapes() {
        let m = analyze_src(
            "func id(r: Rng) -> Rng { return r }",
            &[("id", vec![("r", rng())])],
        );
        assert_eq!(m["id"], vec![false]);
    }

    #[test]
    fn rebound_param_escapes() {
        // Rebinding releases borrowed storage: incompatible with borrows.
        let m = analyze_src(
            "func f(r: Rng) -> int { r = g()\nreturn 0 }",
            &[("f", vec![("r", rng())])],
        );
        assert_eq!(m["f"], vec![false]);
    }

    #[test]
    fn stored_param_escapes() {
        // Storing through the parameter keeps it observable.
        let m = analyze_src(
            "func f(r: Rng) -> int { r.s0 = 1\nreturn 0 }",
            &[("f", vec![("r", rng())])],
        );
        assert_eq!(m["f"], vec![false]);
    }

    #[test]
    fn forwarded_to_unknown_escapes() {
        let m = analyze_src(
            "func fwd(r: Rng) -> int { return g(r) }",
            &[("fwd", vec![("r", rng())])],
        );
        assert_eq!(m["fwd"], vec![false]);
    }

    #[test]
    fn forwarded_to_readonly_stays_readonly() {
        // Fixpoint: mid forwards to proven-read-only next.
        let m = analyze_src(
            "func next(r: Rng) -> int { return r.s0 }\nfunc mid(r: Rng) -> int { return next(r) }",
            &[("next", vec![("r", rng())]), ("mid", vec![("r", rng())])],
        );
        assert_eq!(m["next"], vec![true]);
        assert_eq!(m["mid"], vec![true]);
    }

    #[test]
    fn captured_param_escapes() {
        let m = analyze_src(
            "func f(r: Rng) -> int { return g(| | r.s0) }",
            &[("f", vec![("r", rng())])],
        );
        assert_eq!(m["f"], vec![false]);
    }

    #[test]
    fn per_param_precision() {
        // Only the escaping parameter loses its verdict.
        let m = analyze_src(
            "func f(a: Rng, b: int) -> int { g(a)\nreturn b }",
            &[("f", vec![("a", rng()), ("b", Type::Int)])],
        );
        assert_eq!(m["f"], vec![false, true]);
    }

    #[test]
    fn scalar_params_always_readonly() {
        let m = analyze_src(
            "func add(a: int, b: int) -> int { return a + b }",
            &[("add", vec![("a", Type::Int), ("b", Type::Int)])],
        );
        assert_eq!(m["add"], vec![true, true]);
    }

    #[test]
    fn aliased_return_escapes() {
        // `y = r` aliases; returning the alias escapes.
        let m = analyze_src(
            "func f(r: Rng) -> Rng { y := r\nreturn y }",
            &[("f", vec![("r", rng())])],
        );
        assert_eq!(m["f"], vec![false]);
    }

    #[test]
    fn fresh_rebind_keeps_local_clean_but_param_stays() {
        // `x := r.s0` binds fresh (clone read); the parameter itself is
        // untouched and stays read-only.
        let m = analyze_src(
            "func f(r: Rng) -> int { x := r.s0\nreturn x + 1 }",
            &[("f", vec![("r", rng())])],
        );
        assert_eq!(m["f"], vec![true]);
    }

    #[test]
    fn len_arg_is_a_read() {
        let m = analyze_src(
            "func f(r: Rng) -> int { return len(r) }",
            &[("f", vec![("r", rng())])],
        );
        // len() only reads: still read-only.
        assert_eq!(m["f"], vec![true]);
    }

    #[test]
    fn unknown_callee_with_tainted_arg_escapes() {
        let m = analyze_src(
            "func g(arr: [int]) -> int { vec.push(arr, 1)\nreturn 0 }",
            &[("g", vec![("arr", Type::Array(Box::new(Type::Int)))])],
        );
        assert_eq!(m["g"], vec![false]);
    }

    #[test]
    fn stored_in_local_stays_readonly() {
        // Storing into a frame-local that never escapes is fine: the
        // alias dies with the frame.
        let m = analyze_src(
            "func f(r: Rng) -> int { xs := [r]\nreturn len(xs) }",
            &[("f", vec![("r", rng())])],
        );
        assert_eq!(m["f"], vec![true]);
    }

    #[test]
    fn match_on_param_is_a_read() {
        let m = analyze_src(
            "func f(r: Rng) -> int { return match r.s0 { 0 => 0, _ => 1 } }",
            &[("f", vec![("r", rng())])],
        );
        assert_eq!(m["f"], vec![true]);
    }
}
