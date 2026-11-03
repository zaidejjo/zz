//! Type inference helpers: unification, type variables, generics.

use zz_checker_core::{AliasSig, Checker};
use zz_diag_core::error_at;
use zz_span_ast::ast::Ty;
use zz_span_ast::span::Span;
use zz_suggest::suggest;
use zz_types::Type;
use zz_unify::Unifier;

pub trait InferOps {
    /// Merge element types into a single type: identical types collapse to
    /// one; differing types form a union. Divergent (`Never`) elements
    /// vanish from the join — an array whose every element diverges is
    /// itself `Never`.
    fn merge_types(&mut self, types: Vec<Type>) -> Type;

    /// Does the generic parameter currently in scope carry the given bound?
    fn has_bound(&self, name: &str, bound: zz_span_ast::ast::TraitBound) -> bool;

    /// Does a concrete type satisfy a trait bound?
    fn satisfies_bound(&self, ty: &Type, bound: zz_span_ast::ast::TraitBound) -> bool;

    /// Validate that every generic parameter with a bound was instantiated
    /// with a concrete type satisfying that bound. Called at call sites after
    /// argument unification. Unresolved generics (never pinned by the call)
    /// are skipped — the body's operator checks already validated them.
    fn validate_bounds(
        &mut self,
        sig: &zz_checker_core::FuncSig,
        subs: &std::collections::HashMap<String, Type>,
        span: Span,
    );

    fn instantiate(
        &mut self,
        sig: &zz_checker_core::FuncSig,
    ) -> (Vec<Type>, Type, std::collections::HashMap<String, Type>);

    fn ast_to_type(&mut self, ty: &Ty, generics: &[String]) -> Type;

    fn ast_to_type_inner(&mut self, ty: &Ty, generics: &[String]) -> Type;

    /// Suggest the closest known type name for an unknown type, or a
    /// builtin synonym (`string` → `str`) that falls outside the
    /// Levenshtein threshold. Returns `None` when there is nothing close
    /// enough to suggest, in which case the plain "unknown type" message
    /// stands alone.
    fn suggest_type_name(&self, name: &str) -> Option<String>;

    /// Canonical struct name: a selectively-imported bare name resolves to
    /// its qualified form (`Product` → `product.Product`), exactly as if
    /// written qualified — methods, conversions and type identity all key
    /// off the qualified form. Miss-only: locals and seed entries (which
    /// have no alias mapping) keep their name. Display still prints the
    /// bare form; only types canonicalize.
    fn canonical_struct_name(&self, name: &str) -> String;

    /// Canonical enum name, mirroring [`Self::canonical_struct_name`]: a
    /// selectively-imported bare name resolves to its qualified form.
    fn canonical_enum_name(&self, name: &str) -> String;

    /// Convert one raw alias target to a resolved [`AliasSig`].
    /// Idempotent: already-converted names return immediately (also
    /// breaks re-entrant conversion during cycle reporting).
    fn convert_alias(&mut self, name: &str);

    /// Canonical alias name, mirroring [`Self::canonical_struct_name`]: a
    /// selectively-imported bare name resolves to its qualified form.
    fn canonical_alias_name(&self, name: &str) -> String;

    /// Expand an alias use `Name[args]` to its resolved [`Type`].
    /// Arity is checked like generic structs; mismatches report and
    /// resolve with the provided prefix (missing parameters stay
    /// `Named`, which surfaces as a generic — never a silent hole).
    fn expand_alias(
        &mut self,
        name: &str,
        args: &[Ty],
        outer_generics: &[String],
        span: Span,
    ) -> Type;
}

impl InferOps for Checker {
    /// Merge element types into a single type: identical types collapse to
    /// one; differing types form a union. Divergent (`Never`) elements
    /// vanish from the join — an array whose every element diverges is
    /// itself `Never`.
    fn merge_types(&mut self, types: Vec<Type>) -> Type {
        if types.is_empty() {
            return self.unifier.fresh_var();
        }
        // Filter divergent elements first (resolve to see through vars).
        let mut live: Vec<Type> = Vec::with_capacity(types.len());
        for t in types {
            if !matches!(self.unifier.resolve(&t), Type::Never) {
                live.push(t);
            }
        }
        if live.is_empty() {
            return Type::Never;
        }
        // When every element resolves to a concrete (var-free) type,
        // structural hashing dedups in O(n) instead of O(n²). If any element
        // still carries an unbound inference variable, fall back to the
        // linear all-pairs eq_resolved so unbound-variable merging (e.g.
        // `[.some(1), .none]`) keeps its exact original semantics.
        if live.iter().all(|t| !contains_var(&self.unifier.resolve(t))) {
            merge_types_hash(&mut self.unifier, live)
        } else {
            merge_types_linear(&mut self.unifier, live)
        }
    }

    // --- generics ---------------------------------------------------------

    /// Does the generic parameter currently in scope carry the given bound?
    fn has_bound(&self, name: &str, bound: zz_span_ast::ast::TraitBound) -> bool {
        self.current_bounds
            .get(name)
            .is_some_and(|bs| bs.contains(&bound))
    }

    /// Does a concrete type satisfy a trait bound?
    fn satisfies_bound(&self, ty: &Type, bound: zz_span_ast::ast::TraitBound) -> bool {
        use zz_span_ast::ast::TraitBound as B;
        match ty {
            Type::Union(ms) => ms.iter().all(|m| self.satisfies_bound(m, bound)),
            Type::Var(_) | Type::Named(_) | Type::Error | Type::Never => false,
            Type::Func(..) => false,
            Type::Unit | Type::Void => false,
            _ => match bound {
                B::Num => matches!(ty, Type::Int | Type::Float),
                B::Ord => matches!(ty, Type::Int | Type::Float | Type::Str),
                B::Eq => true,
                B::Display => true,
            },
        }
    }

    /// Validate that every generic parameter with a bound was instantiated
    /// with a concrete type satisfying that bound. Called at call sites after
    /// argument unification. Unresolved generics (never pinned by the call)
    /// are skipped — the body's operator checks already validated them.
    fn validate_bounds(
        &mut self,
        sig: &zz_checker_core::FuncSig,
        subs: &std::collections::HashMap<String, Type>,
        span: Span,
    ) {
        for (gen, bounds) in &sig.bounds {
            let Some(sub) = subs.get(gen) else { continue };
            let rt = self.unifier.resolve(sub);
            if matches!(rt, Type::Var(_)) {
                continue;
            }
            for b in bounds {
                if !self.satisfies_bound(&rt, *b) {
                    self.errors.push(error_at(
                        format!(
                            "type `{rt}` does not satisfy bound `{}` for generic parameter `{gen}`",
                            b.name()
                        ),
                        span,
                    ));
                }
            }
        }
    }

    fn instantiate(
        &mut self,
        sig: &zz_checker_core::FuncSig,
    ) -> (Vec<Type>, Type, std::collections::HashMap<String, Type>) {
        let subs: std::collections::HashMap<String, Type> = sig
            .generics
            .iter()
            .map(|g| (g.clone(), self.unifier.fresh_var()))
            .collect();
        let params = sig.params.iter().map(|(_, t)| subst(t, &subs)).collect();
        let ret = subst(&sig.ret, &subs);
        (params, ret, subs)
    }

    // --- type annotations -------------------------------------------------

    fn ast_to_type(&mut self, ty: &Ty, generics: &[String]) -> Type {
        self.ast_to_type_inner(ty, generics)
    }

    fn ast_to_type_inner(&mut self, ty: &Ty, generics: &[String]) -> Type {
        use zz_span_ast::ast::TyKind;
        let key = (hash_ty(ty), hash_generics(generics));
        // Cache hit: return a cheap clone, skipping re-parse and side effects
        // (used_names marking, error emission). Re-emitting them would only
        // duplicate diagnostics for the same annotation.
        if let Some(cached) = self.type_annotation_cache.get(&key) {
            return cached.clone();
        }
        let result = match &ty.kind {
            TyKind::Int => Type::Int,
            TyKind::Float => Type::Float,
            TyKind::Bool => Type::Bool,
            TyKind::Str => Type::Str,
            TyKind::Unit => Type::Unit,
            TyKind::Void => Type::Void,
            TyKind::Ptr { mutable, inner } => Type::Ptr {
                mutable: *mutable,
                inner: Box::new(self.ast_to_type_inner(inner, generics)),
            },
            TyKind::Tuple(ts) => Type::Tuple(
                ts.iter()
                    .map(|t| self.ast_to_type_inner(t, generics))
                    .collect(),
            ),
            TyKind::Option(t) => Type::Option(Box::new(self.ast_to_type_inner(t, generics))),
            TyKind::Result(t, e) => Type::Result(
                Box::new(self.ast_to_type_inner(t, generics)),
                Box::new(self.ast_to_type_inner(e, generics)),
            ),
            TyKind::Func(ps, r) => Type::Func(
                ps.iter()
                    .map(|t| self.ast_to_type_inner(t, generics))
                    .collect(),
                Box::new(self.ast_to_type_inner(r, generics)),
            ),
            TyKind::Array(t) => Type::Array(Box::new(self.ast_to_type_inner(t, generics))),
            TyKind::Dict(k, v) => Type::Dict(
                Box::new(self.ast_to_type_inner(k, generics)),
                Box::new(self.ast_to_type_inner(v, generics)),
            ),
            TyKind::Union(ts) => Type::Union(
                ts.iter()
                    .map(|t| self.ast_to_type_inner(t, generics))
                    .collect(),
            ),
            TyKind::Named(name, args) => {
                if generics.iter().any(|g| g == name) {
                    if !args.is_empty() {
                        self.errors.push(error_at(
                            format!("generic parameter `{name}` does not take type arguments"),
                            ty.span,
                        ));
                    }
                    Type::Named(name.clone())
                } else {
                    // Type-position uses count for unused-import tracking
                    // (`import shapes` + `t: shapes.Tokens` marks `shapes`
                    // used). Bare names are skipped: a type sharing a
                    // variable's name must not silence its warning.
                    if name.contains('.') {
                        self.used_names.insert(name.clone());
                    }
                    if self.alias_asts.contains_key(name)
                        || self.aliases.contains_key(name)
                        || self
                            .import_aliases
                            .get(name)
                            .is_some_and(|q| self.aliases.contains_key(q))
                    {
                        // Type aliases erase: uses resolve to the target type.
                        // Unconverted program-local aliases convert on demand
                        // inside `expand_alias` (order-independent).
                        let cname = self.canonical_alias_name(name);
                        // A bare name resolved through a selective import
                        // counts as using that import (mirrors value
                        // lookups through `import_aliases`).
                        if cname != *name {
                            self.used_names.insert(name.clone());
                        }
                        self.expand_alias(&cname, args, generics, ty.span)
                    } else if self.enums.contains_key(name)
                        || self
                            .import_aliases
                            .get(name)
                            .is_some_and(|q| self.enums.contains_key(q))
                    {
                        // User enums name their type directly (`t: Token`);
                        // generic enums name their arguments (`Box<int>`),
                        // exactly like generic structs.
                        let cname = self.canonical_enum_name(name);
                        if cname != *name {
                            self.used_names.insert(name.clone());
                        }
                        let want = self
                            .enums
                            .get(&cname)
                            .map(|s| s.generics.len())
                            .unwrap_or(0);
                        if args.is_empty() && want > 0 {
                            // Mirror generic structs: name the arguments
                            // (`Box<int>`). Inference still flows from
                            // construction (`Box.V(1)` unifies on its own).
                            self.errors.push(error_at(
                                format!(
                                    "enum `{name}` takes {want} type argument{} (e.g. `{name}<{}>`)",
                                    if want == 1 { "" } else { "s" },
                                    vec!["T"; want].join(", "),
                                ),
                                ty.span,
                            ));
                            return Type::Enum(
                                cname,
                                (0..want).map(|_| self.unifier.fresh_var()).collect(),
                            );
                        }
                        if args.len() != want {
                            self.errors.push(error_at(
                                format!(
                                    "enum `{name}` takes {want} type argument{} but {} given",
                                    if want == 1 { "" } else { "s" },
                                    args.len(),
                                ),
                                ty.span,
                            ));
                            return Type::Enum(
                                cname,
                                (0..want).map(|_| self.unifier.fresh_var()).collect(),
                            );
                        }
                        Type::Enum(
                            cname,
                            args.iter()
                                .map(|a| self.ast_to_type_inner(a, generics))
                                .collect(),
                        )
                    } else if self.structs.contains_key(name) {
                        // Canonicalize selective imports, mirroring
                        // StructInit: `Product` → `product.Product`.
                        let cname = self.canonical_struct_name(name);
                        if cname != *name {
                            self.used_names.insert(name.clone());
                        }
                        let want = self
                            .structs
                            .get(&cname)
                            .map(|s| s.generics.len())
                            .unwrap_or(0);
                        if args.is_empty() && want > 0 {
                            self.errors.push(error_at(
                                format!(
                                "struct `{name}` takes {want} type argument{} (e.g. `{name}<{}>`)",
                                if want == 1 { "" } else { "s" },
                                vec!["T"; want].join(", "),
                            ),
                                ty.span,
                            ));
                            Type::Struct(cname, Vec::new())
                        } else if args.len() != want {
                            if !(args.is_empty() && want == 0) {
                                self.errors.push(error_at(
                                    format!(
                                        "struct `{name}` takes {want} type argument{} but {} given",
                                        if want == 1 { "" } else { "s" },
                                        args.len(),
                                    ),
                                    ty.span,
                                ));
                            }
                            Type::Struct(cname, Vec::new())
                        } else {
                            Type::Struct(
                                cname,
                                args.iter()
                                    .map(|a| self.ast_to_type_inner(a, generics))
                                    .collect(),
                            )
                        }
                    } else if name == "json" {
                        Type::Json
                    } else if name == "bytes" {
                        Type::Bytes
                    } else if name == "db" || name == "sqlz" {
                        Type::Db
                    } else if name == "chan" {
                        Type::Chan
                    } else if name == "task.join" {
                        Type::TaskJoin
                    } else if name == "http.server" {
                        Type::HttpServer
                    } else if name == "tcp.stream" {
                        Type::TcpStream
                    } else if name == "tcp.listener" {
                        Type::TcpListener
                    } else if name == "http.response" {
                        Type::Response
                    } else if name == "http.request" {
                        Type::HttpRequest
                    } else if let Some(tag) = opaque_handle_tag(name) {
                        // Module handle types (`tls` connections,
                        // `tls_listener`s, `file`s, compiled `regexp`s,
                        // log `span`s, cli `args` parsers, `process`
                        // handles, `zzfs` providers): natives traffic in
                        // `Opaque(tag)` values and dispatch methods on the
                        // tag, but source could never name them — helper
                        // funcs had to leave params untyped (uncallable).
                        // Only consulted when no struct/enum/alias/builtin
                        // matches, so user types always win.
                        Type::Opaque(tag.to_string())
                    } else {
                        let msg = if let Some(sugg) = self.suggest_type_name(name) {
                            format!("unknown type `{name}` — did you mean `{sugg}`?")
                        } else {
                            format!("unknown type `{name}`")
                        };
                        self.errors.push(error_at(msg, ty.span));
                        Type::Unit
                    }
                }
            }
        };
        self.type_annotation_cache.insert(key, result.clone());
        result
    }

    /// Suggest the closest known type name for an unknown type, or a
    /// builtin synonym (`string` → `str`) that falls outside the
    /// Levenshtein threshold. Returns `None` when there is nothing close
    /// enough to suggest, in which case the plain "unknown type" message
    /// stands alone.
    fn suggest_type_name(&self, name: &str) -> Option<String> {
        // Common synonym that Levenshtein can't reach (`string` vs `str`
        // differ by more than the allowed edit distance).
        if name == "string" {
            return Some("str".to_string());
        }
        let mut candidates: Vec<&str> = Vec::new();
        // Builtin type names.
        candidates.extend([
            "int",
            "float",
            "bool",
            "str",
            "unit",
            "void",
            "json",
            "bytes",
            "db",
            "sqlz",
            "chan",
            "task.join",
            "http.server",
            "tcp.stream",
            "tcp.listener",
            "http.response",
            "http.request",
        ]);
        // Opaque handle tags users can name in type position.
        candidates.extend([
            "tls",
            "tls_listener",
            "unix",
            "unix_listener",
            "cell",
            "file",
            "zzfs",
            "regexp",
            "span",
            "args",
            "process",
        ]);
        // User-defined structs, enums, and aliases (canonical + bare).
        for s in self.structs.keys() {
            candidates.push(s.as_str());
        }
        for e in self.enums.keys() {
            candidates.push(e.as_str());
        }
        for a in self.aliases.keys() {
            candidates.push(a.as_str());
        }
        for a in self.alias_asts.keys() {
            candidates.push(a.as_str());
        }
        suggest(name, &candidates).map(|(s, _)| s.to_string())
    }

    fn canonical_struct_name(&self, name: &str) -> String {
        if let Some(qualified) = self.import_aliases.get(name) {
            if self.structs.contains_key(qualified) {
                return qualified.clone();
            }
        }
        name.to_string()
    }

    fn canonical_enum_name(&self, name: &str) -> String {
        if let Some(qualified) = self.import_aliases.get(name) {
            if self.enums.contains_key(qualified) {
                return qualified.clone();
            }
        }
        name.to_string()
    }

    fn convert_alias(&mut self, name: &str) {
        if self.aliases.contains_key(name) {
            return;
        }
        if self.alias_expanding.iter().any(|n| n == name) {
            let span = self
                .alias_asts
                .get(name)
                .map(|(_, t)| t.span)
                .unwrap_or(Span::new(0, 0));
            self.errors.push(error_at(
                format!(
                    "cyclic type alias `{}`: {}",
                    name,
                    self.alias_expanding.join(" -> ")
                ),
                span,
            ));
            // Placeholder so the use site resolves instead of cascading
            // a second "unknown type" error.
            self.aliases.entry(name.to_string()).or_insert(AliasSig {
                generics: Vec::new(),
                target: Type::Unit,
            });
            return;
        }
        let Some((generics, target)) = self.alias_asts.get(name).cloned() else {
            return;
        };
        self.alias_expanding.push(name.to_string());
        let ty = self.ast_to_type_inner(&target, &generics);
        self.alias_expanding.pop();
        // A cycle during conversion leaves no entry: record the
        // (possibly partial) type anyway so uses don't cascade
        // "unknown type" on top of the cycle error.
        self.aliases.entry(name.to_string()).or_insert(AliasSig {
            generics,
            target: ty,
        });
    }

    fn canonical_alias_name(&self, name: &str) -> String {
        if let Some(qualified) = self.import_aliases.get(name) {
            if self.aliases.contains_key(qualified) {
                return qualified.clone();
            }
        }
        name.to_string()
    }

    fn expand_alias(
        &mut self,
        name: &str,
        args: &[Ty],
        outer_generics: &[String],
        span: Span,
    ) -> Type {
        // Lazily convert: seeded aliases (cross-module) arrive converted,
        // same-program aliases convert in pass 1a — but a use inside an
        // alias target converts on demand through `convert_alias`.
        self.convert_alias(name);
        let Some(sig) = self.aliases.get(name).cloned() else {
            self.errors
                .push(error_at(format!("unknown type `{name}`"), span));
            return Type::Unit;
        };
        if args.len() != sig.generics.len() {
            if sig.generics.is_empty() {
                self.errors.push(error_at(
                    format!(
                        "type alias `{name}` takes no type arguments but {} given",
                        args.len(),
                    ),
                    span,
                ));
            } else {
                self.errors.push(error_at(
                    format!(
                        "type alias `{name}` takes {} type argument{} but {} given",
                        sig.generics.len(),
                        if sig.generics.len() == 1 { "" } else { "s" },
                        args.len(),
                    ),
                    span,
                ));
            }
        }
        let mut map = std::collections::HashMap::new();
        for (i, g) in sig.generics.iter().enumerate() {
            if let Some(a) = args.get(i) {
                map.insert(g.clone(), self.ast_to_type_inner(a, outer_generics));
            }
        }
        subst(&sig.target, &map)
    }
}

/// O(n) dedup for concrete element types via structural-hash buckets.
/// Preserves first-appearance order for deterministic output.
fn merge_types_hash(unifier: &mut Unifier, live: Vec<Type>) -> Type {
    let mut distinct: Vec<Type> = Vec::new();
    let mut hash_to_indices: std::collections::HashMap<u64, Vec<usize>> =
        std::collections::HashMap::new();
    for t in live {
        let h = hash_type(&unifier.resolve(&t));
        let mut placed = false;
        if let Some(indices) = hash_to_indices.get(&h) {
            for &idx in indices {
                if unifier.eq_resolved(&distinct[idx], &t) {
                    let _ = unifier.unify(&distinct[idx], &t);
                    placed = true;
                    break;
                }
            }
        }
        if !placed {
            let idx = distinct.len();
            distinct.push(t);
            hash_to_indices.entry(h).or_default().push(idx);
        }
    }
    if distinct.len() == 1 {
        distinct.pop().unwrap()
    } else {
        Type::Union(distinct)
    }
}

/// Original O(n²) all-pairs eq_resolved dedup, kept exact for cases
/// involving unbound inference variables (which hash-consing cannot
/// bucket soundly since an unbound var eq_resolved-matches anything).
fn merge_types_linear(unifier: &mut Unifier, live: Vec<Type>) -> Type {
    let mut distinct: Vec<Type> = Vec::new();
    for t in live {
        if let Some(existing) = distinct.iter().find(|d| unifier.eq_resolved(d, &t)) {
            let _ = unifier.unify(existing, &t);
        } else {
            distinct.push(t);
        }
    }
    if distinct.len() == 1 {
        distinct.pop().unwrap()
    } else {
        Type::Union(distinct)
    }
}

/// Surface name of a native module-handle type (`tls`, `file`, ...).
/// Consulted only when no struct/enum/alias/builtin matches, so user
/// types always win. Keep in sync with the `Opaque(..)` tags in
/// `zz_stdlib::funcs` (a new handle-producing native adds one line).
fn opaque_handle_tag(name: &str) -> Option<&'static str> {
    match name {
        "tls" => Some("tls"),
        "tls_listener" => Some("tls_listener"),
        "unix" => Some("unix"),
        "unix_listener" => Some("unix_listener"),
        "cell" => Some("cell"),
        "file" => Some("file"),
        "zzfs" => Some("zzfs"),
        "regexp" => Some("regexp"),
        "span" => Some("span"),
        "args" => Some("args"),
        "process" => Some("process"),
        _ => None,
    }
}

/// Check if a type contains any unresolved inference variables.
pub fn contains_var(t: &Type) -> bool {
    match t {
        Type::Var(_) => true,
        Type::Void => false,
        Type::Tuple(ts) => ts.iter().any(contains_var),
        Type::Option(x) => contains_var(x),
        Type::Result(a, b) => contains_var(a) || contains_var(b),
        Type::Func(ps, r) => ps.iter().any(contains_var) || contains_var(r),
        Type::Array(x) => contains_var(x),
        Type::Dict(k, v) => contains_var(k) || contains_var(v),
        Type::Struct(_, args) => args.iter().any(contains_var),
        Type::Enum(_, args) => args.iter().any(contains_var),
        Type::Union(ts) => ts.iter().any(contains_var),
        Type::Range(x) => contains_var(x),
        Type::Ptr { inner, .. } => contains_var(inner),
        _ => false,
    }
}

/// Hash the generic parameter list for use in the type-annotation cache key.
/// A fresh `Hasher` per call keeps this deterministic and dependency-free.
fn hash_generics(generics: &[String]) -> u64 {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let mut h = DefaultHasher::new();
    generics.hash(&mut h);
    h.finish()
}

/// Hash the structural shape of a `Ty` annotation (excluding its span), so
/// the same annotation written in different locations shares one cache key.
fn hash_ty(ty: &Ty) -> u64 {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::Hasher;
    let mut h = DefaultHasher::new();
    hash_ty_into(ty, &mut h);
    h.finish()
}

/// Write the structural shape of `ty` into an existing hasher, so nested
/// types contribute their bytes to the parent's hash (not a discarded
/// child hasher).
fn hash_ty_into(ty: &Ty, h: &mut impl std::hash::Hasher) {
    use std::hash::Hash;
    use zz_span_ast::ast::TyKind;
    match &ty.kind {
        TyKind::Int => h.write_u8(0),
        TyKind::Float => h.write_u8(1),
        TyKind::Bool => h.write_u8(2),
        TyKind::Str => h.write_u8(3),
        TyKind::Unit => h.write_u8(4),
        TyKind::Void => h.write_u8(5),
        TyKind::Ptr { mutable, inner } => {
            h.write_u8(6);
            mutable.hash(h);
            hash_ty_into(inner, h);
        }
        TyKind::Tuple(ts) => {
            h.write_u8(7);
            for t in ts {
                hash_ty_into(t, h);
            }
        }
        TyKind::Option(inner) => {
            h.write_u8(8);
            hash_ty_into(inner, h);
        }
        TyKind::Result(ok, err) => {
            h.write_u8(9);
            hash_ty_into(ok, h);
            hash_ty_into(err, h);
        }
        TyKind::Func(ps, r) => {
            h.write_u8(10);
            for p in ps {
                hash_ty_into(p, h);
            }
            hash_ty_into(r, h);
        }
        TyKind::Array(inner) => {
            h.write_u8(11);
            hash_ty_into(inner, h);
        }
        TyKind::Dict(k, v) => {
            h.write_u8(12);
            hash_ty_into(k, h);
            hash_ty_into(v, h);
        }
        TyKind::Union(ts) => {
            h.write_u8(13);
            for t in ts {
                hash_ty_into(t, h);
            }
        }
        TyKind::Named(name, args) => {
            h.write_u8(14);
            name.hash(h);
            for a in args {
                hash_ty_into(a, h);
            }
        }
    }
}

/// Hash the structural shape of a resolved `Type` (excluding inference
/// variable ids beyond their identity), for bucketing in `merge_types`.
fn hash_type(ty: &Type) -> u64 {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::Hasher;
    let mut h = DefaultHasher::new();
    hash_type_into(ty, &mut h);
    h.finish()
}

/// Write the structural shape of `ty` into an existing hasher.
fn hash_type_into(ty: &Type, h: &mut impl std::hash::Hasher) {
    use std::hash::Hash;
    match ty {
        Type::Int => h.write_u8(0),
        Type::Float => h.write_u8(1),
        Type::Bool => h.write_u8(2),
        Type::Str => h.write_u8(3),
        Type::Unit => h.write_u8(4),
        Type::Void => h.write_u8(5),
        Type::Bytes => h.write_u8(6),
        Type::Json => h.write_u8(7),
        Type::Db => h.write_u8(8),
        Type::HttpServer => h.write_u8(9),
        Type::TcpStream => h.write_u8(10),
        Type::TcpListener => h.write_u8(11),
        Type::Response => h.write_u8(12),
        Type::HttpRequest => h.write_u8(13),
        Type::Opaque(tag) => {
            h.write_u8(14);
            tag.hash(h);
        }
        Type::Chan => h.write_u8(15),
        Type::TaskJoin => h.write_u8(16),
        Type::Error => h.write_u8(17),
        Type::Never => h.write_u8(18),
        Type::Ptr { mutable, inner } => {
            h.write_u8(19);
            mutable.hash(h);
            hash_type_into(inner, h);
        }
        Type::Tuple(ts) => {
            h.write_u8(20);
            for t in ts {
                hash_type_into(t, h);
            }
        }
        Type::Option(inner) => {
            h.write_u8(21);
            hash_type_into(inner, h);
        }
        Type::Result(ok, err) => {
            h.write_u8(22);
            hash_type_into(ok, h);
            hash_type_into(err, h);
        }
        Type::Func(ps, r) => {
            h.write_u8(23);
            for p in ps {
                hash_type_into(p, h);
            }
            hash_type_into(r, h);
        }
        Type::Array(inner) => {
            h.write_u8(24);
            hash_type_into(inner, h);
        }
        Type::Dict(k, v) => {
            h.write_u8(25);
            hash_type_into(k, h);
            hash_type_into(v, h);
        }
        Type::Union(ts) => {
            h.write_u8(26);
            for t in ts {
                hash_type_into(t, h);
            }
        }
        Type::Struct(name, args) => {
            h.write_u8(27);
            name.hash(h);
            for a in args {
                hash_type_into(a, h);
            }
        }
        Type::Enum(name, args) => {
            h.write_u8(28);
            name.hash(h);
            for a in args {
                hash_type_into(a, h);
            }
        }
        Type::Range(inner) => {
            h.write_u8(29);
            hash_type_into(inner, h);
        }
        Type::Var(id) => {
            h.write_u8(30);
            id.hash(h);
        }
        Type::Named(name) => {
            h.write_u8(31);
            name.hash(h);
        }
    }
}

/// Largest inference-variable id inside a type (`None` when var-free).
/// Used to offset a fresh checker's unifier above ids imported from
/// already-checked modules (see `Unifier::reserve_vars_above`).
pub fn max_var_id(t: &Type) -> Option<u32> {
    match t {
        Type::Var(id) => Some(*id),
        Type::Void => None,
        Type::Tuple(ts) => ts.iter().filter_map(max_var_id).max(),
        Type::Option(x) => max_var_id(x),
        Type::Result(a, b) => max_var_id(a).into_iter().chain(max_var_id(b)).max(),
        Type::Func(ps, r) => ps.iter().filter_map(max_var_id).chain(max_var_id(r)).max(),
        Type::Array(x) => max_var_id(x),
        Type::Dict(k, v) => max_var_id(k).into_iter().chain(max_var_id(v)).max(),
        Type::Struct(_, args) => args.iter().filter_map(max_var_id).max(),
        Type::Enum(_, args) => args.iter().filter_map(max_var_id).max(),
        Type::Union(ts) => ts.iter().filter_map(max_var_id).max(),
        Type::Range(x) => max_var_id(x),
        Type::Ptr { inner, .. } => max_var_id(inner),
        _ => None,
    }
}

/// Replace unresolved type variables inside `Option`/`Result` with `unit`
/// so `.none`/`.ok`/`.err` bindings type-check without annotations.
pub fn default_variant_vars(t: &mut Type) {
    match t {
        Type::Option(inner) => {
            if contains_var(inner) {
                default_variant_vars(inner);
                if contains_var(inner) {
                    **inner = Type::Unit;
                }
            }
        }
        Type::Result(ok, err) => {
            if contains_var(ok) {
                default_variant_vars(ok);
                if contains_var(ok) {
                    **ok = Type::Unit;
                }
            }
            if contains_var(err) {
                default_variant_vars(err);
                if contains_var(err) {
                    **err = Type::Unit;
                }
            }
        }
        Type::Enum(_, args) => {
            // Unconstrained enum parameters default like `Option`'s
            // (bare `Box.Empty` with nothing to infer from is `Box<unit>`).
            for a in args {
                if contains_var(a) {
                    default_variant_vars(a);
                    if contains_var(a) {
                        *a = Type::Unit;
                    }
                }
            }
        }
        _ => {}
    }
}

/// Substitute generic type parameters in a type.
pub fn subst(t: &Type, subs: &std::collections::HashMap<String, Type>) -> Type {
    match t {
        Type::Named(name) => subs
            .get(name)
            .cloned()
            .unwrap_or_else(|| Type::Named(name.clone())),
        Type::Tuple(ts) => Type::Tuple(ts.iter().map(|x| subst(x, subs)).collect()),
        Type::Option(x) => Type::Option(Box::new(subst(x, subs))),
        Type::Result(a, b) => Type::Result(Box::new(subst(a, subs)), Box::new(subst(b, subs))),
        Type::Func(ps, r) => Type::Func(
            ps.iter().map(|x| subst(x, subs)).collect(),
            Box::new(subst(r, subs)),
        ),
        Type::Array(x) => Type::Array(Box::new(subst(x, subs))),
        Type::Dict(k, v) => Type::Dict(Box::new(subst(k, subs)), Box::new(subst(v, subs))),
        Type::Struct(n, args) => {
            Type::Struct(n.clone(), args.iter().map(|x| subst(x, subs)).collect())
        }
        Type::Enum(n, args) => Type::Enum(n.clone(), args.iter().map(|x| subst(x, subs)).collect()),
        Type::Union(ts) => Type::Union(ts.iter().map(|x| subst(x, subs)).collect()),
        Type::Range(x) => Type::Range(Box::new(subst(x, subs))),
        Type::Ptr { mutable, inner } => Type::Ptr {
            mutable: *mutable,
            inner: Box::new(subst(inner, subs)),
        },
        other => other.clone(),
    }
}

#[cfg(test)]
mod infer_var_tests {
    use super::*;

    /// Builds one instance of every `Type` variant, nesting a `Var(7)` deep
    /// inside each container so a correct walk finds it. Leaf/scalar
    /// variants carry no var.
    fn var_type() -> Type {
        Type::Var(7)
    }

    fn container_with_var() -> Vec<Type> {
        vec![
            Type::Tuple(vec![var_type()]),
            Type::Option(Box::new(var_type())),
            Type::Result(Box::new(var_type()), Box::new(Type::Int)),
            Type::Func(vec![var_type()], Box::new(Type::Int)),
            Type::Array(Box::new(var_type())),
            Type::Dict(Box::new(Type::Int), Box::new(var_type())),
            Type::Struct("S".to_string(), vec![var_type()]),
            Type::Enum("E".to_string(), vec![var_type()]),
            Type::Union(vec![var_type(), Type::Int]),
            Type::Range(Box::new(var_type())),
            Type::Ptr {
                mutable: false,
                inner: Box::new(var_type()),
            },
        ]
    }

    /// Every non-var, non-container variant: must report no var and no max id.
    fn leaf_types() -> Vec<Type> {
        vec![
            Type::Int,
            Type::Float,
            Type::Bool,
            Type::Str,
            Type::Unit,
            Type::Void,
            Type::Bytes,
            Type::Json,
            Type::Db,
            Type::HttpServer,
            Type::TcpStream,
            Type::TcpListener,
            Type::Response,
            Type::HttpRequest,
            Type::Opaque("tag".to_string()),
            Type::Chan,
            Type::TaskJoin,
            Type::Named("T".to_string()),
            Type::Error,
            Type::Never,
        ]
    }

    #[test]
    fn contains_var_find_var_in_every_container() {
        for t in container_with_var() {
            assert!(
                contains_var(&t),
                "contains_var should find Var inside {:?}",
                t
            );
        }
        // A var directly is also a var.
        assert!(contains_var(&var_type()));
        // Nested container still finds it.
        assert!(contains_var(&Type::Option(Box::new(Type::Array(
            Box::new(var_type())
        )))));
    }

    #[test]
    fn contains_var_false_on_leaf_types() {
        for t in leaf_types() {
            assert!(!contains_var(&t), "leaf {:?} should not contain a var", t);
        }
        assert!(!contains_var(&Type::Tuple(vec![])));
        assert!(!contains_var(&Type::Union(vec![])));
    }

    #[test]
    fn max_var_id_returns_id_in_every_container() {
        for t in container_with_var() {
            assert_eq!(
                max_var_id(&t),
                Some(7),
                "max_var_id should find Var(7) inside {:?}",
                t
            );
        }
        assert_eq!(max_var_id(&var_type()), Some(7));
        // Two vars: returns the max.
        let two = Type::Tuple(vec![Type::Var(3), Type::Var(9)]);
        assert_eq!(max_var_id(&two), Some(9));
        // Nested: deepest wins.
        let nested = Type::Option(Box::new(Type::Array(Box::new(Type::Var(42)))));
        assert_eq!(max_var_id(&nested), Some(42));
    }

    #[test]
    fn max_var_id_none_on_leaf_types() {
        for t in leaf_types() {
            assert_eq!(
                max_var_id(&t),
                None,
                "leaf {:?} should have no max var id",
                t
            );
        }
        assert_eq!(max_var_id(&Type::Tuple(vec![])), None);
        assert_eq!(max_var_id(&Type::Union(vec![])), None);
    }
}
