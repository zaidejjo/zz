//! Struct registration, checking, and embedding (anonymous fields).
//!
//! Embedding (Go-style composition): `struct User { Base, age: int }`
//! declares an *embedded* field whose name defaults to the type's last
//! segment (`Base`). Field and method lookup on `User` falls through to
//! embedded structs transitively: `u.id` resolves as `u.Base.id` and
//! `u.area()` dispatches to `Base.area` with the embedded value as the
//! receiver.
//!
//! A field counts as embedded when its type is `Struct(s)` and the field
//! name equals the last segment of `s`. This covers both the anonymous
//! syntax (`Base,`) and the explicit-but-equivalent form (`Base: Base`),
//! so no AST flag is needed and all downstream passes keep working.

use zz_checker_core::{Checker, FuncSig};
use zz_diag_core::{error_at, FixIt};
use zz_infer::InferOps;
use zz_span_ast::ast::Stmt;
use zz_span_ast::span::Span;
use zz_suggest::suggest_all;
use zz_types::Type;

pub trait StructOps {
    fn collect_struct(&mut self, stmt: &Stmt);

    /// True when a struct field is an embedded (anonymous) field: its type
    /// is a struct whose last name segment equals the field name.
    fn is_embedded_field(fname: &str, fty: &Type) -> bool;

    /// Substitution map from a struct's generic parameters to the
    /// concrete arguments at this use site. Empty when arities differ
    /// (already reported elsewhere) — lookups then keep `Named` as-is.
    fn struct_arg_map(&self, sname: &str, args: &[Type])
        -> std::collections::HashMap<String, Type>;

    /// Direct field type with the use-site arguments substituted
    /// (`Box<int>.v` → `int`). Returns `None` for unknown structs,
    /// unknown fields, or arity mismatch details (handled by callers).
    fn direct_field_type(&self, sname: &str, args: &[Type], field: &str) -> Option<Type>;

    /// Like [`Checker::direct_field_type`], but also returns the path of
    /// embedded field names leading to the field (empty = direct).
    /// Generic arguments thread through embedded levels: stepping into
    /// `Base` via a field of type `Base[int]` continues with
    /// `args=[int]`. Cycle-safe.
    fn resolve_struct_field_generic(
        &self,
        sname: &str,
        args: &[Type],
        field: &str,
    ) -> Option<(Vec<String>, Type)>;

    /// Concrete type of the embedded value defining `method` when called
    /// on `(sname, args)`: BFS over embedded fields carrying each level's
    /// substituted arguments, returning the first value whose struct base
    /// matches `defining` (mirrors [`Checker::find_struct_method`).
    /// The runtime passes this embedded value as the receiver, so method
    /// calls unify it (not the outer type) against the method's `self`.
    fn promoted_method_receiver(&self, sname: &str, args: &[Type], defining: &str) -> Option<Type>;

    /// First required leaf path under struct `sname` (reached via `prefix`)
    /// that is not covered by the given concrete literal paths. An explicit
    /// value at a path covers its whole subtree; otherwise embedded
    /// subtrees must be fully covered leaf-by-leaf. `None` = fully covered.
    fn first_uncovered_leaf(
        &self,
        sname: &str,
        prefix: &[String],
        given: &[Vec<String>],
        depth: usize,
    ) -> Option<Vec<String>>;

    /// All field names visible on a struct: direct fields plus transitively
    /// promoted fields (nearest embedding wins on shadowing). Used for
    /// "did you mean" suggestions.
    fn all_visible_fields(&self, sname: &str) -> Vec<String>;

    /// Find a method for a struct, searching embedded structs transitively
    /// when the outer struct defines no such method. Returns the defining
    /// struct's name and signature. Direct methods always win; breadth-first
    /// order keeps the nearest embedding's method on conflicts.
    fn find_struct_method(&self, sname: &str, method: &str) -> Option<(String, FuncSig)>;

    /// Resolve a field read on struct `sname` (with generic `args`),
    /// including embedded fields. Reports unknown structs/fields and
    /// returns `Unit` so checking can continue.
    fn struct_field_access(&mut self, sname: &str, args: &[Type], name: &str, span: Span) -> Type;
}

impl StructOps for Checker {
    fn collect_struct(&mut self, stmt: &Stmt) {
        let (name, generics, fields) = match stmt {
            Stmt::Struct {
                name,
                generics,
                fields,
                ..
            } => (name, generics, fields),
            _ => {
                self.errors.push(error_at(
                    "internal: collect_struct called with a non-Struct statement",
                    stmt.span(),
                ));
                return;
            }
        };
        // Reject duplicate type parameters (`struct P[T, T]`).
        let mut seen = std::collections::HashSet::new();
        for g in generics {
            if !seen.insert(g.name.clone()) {
                self.errors.push(error_at(
                    format!(
                        "duplicate type parameter `{}` in struct `{}`",
                        g.name,
                        name.join(".")
                    ),
                    g.span,
                ));
            }
        }
        let gen_names: Vec<String> = generics.iter().map(|g| g.name.clone()).collect();
        let sig_fields = fields
            .iter()
            .map(|(fname, fty)| (fname.name.clone(), self.ast_to_type(fty, &gen_names)))
            .collect();
        let full_name = name.join(".");
        // Recursive fields (direct or behind arrays/options) are sound:
        // struct values are heap objects held by reference, so nesting
        // is always finite at construction. The AOT unbox analysis
        // treats any cycle as boxed (see is_unboxed_struct).
        // Shadowing a builtin amid generics is still a collision; the
        // plain-struct path reports it elsewhere, so only check arity here.
        self.structs.insert(
            full_name,
            zz_checker_core::StructSig {
                generics: gen_names,
                fields: sig_fields,
            },
        );
    }

    /// True when a struct field is an embedded (anonymous) field: its type
    /// is a struct whose last name segment equals the field name.
    fn is_embedded_field(fname: &str, fty: &Type) -> bool {
        match fty {
            Type::Struct(s, _) => s.rsplit('.').next().unwrap_or(s) == fname,
            _ => false,
        }
    }

    /// Substitution map from a struct's generic parameters to the
    /// concrete arguments at this use site. Empty when arities differ
    /// (already reported elsewhere) — lookups then keep `Named` as-is.
    fn struct_arg_map(
        &self,
        sname: &str,
        args: &[Type],
    ) -> std::collections::HashMap<String, Type> {
        match self.structs.get(sname) {
            Some(sig) if sig.generics.len() == args.len() => sig
                .generics
                .iter()
                .cloned()
                .zip(args.iter().cloned())
                .collect(),
            _ => std::collections::HashMap::new(),
        }
    }

    /// Direct field type with the use-site arguments substituted
    /// (`Box<int>.v` → `int`). Returns `None` for unknown structs,
    /// unknown fields, or arity mismatch details (handled by callers).
    fn direct_field_type(&self, sname: &str, args: &[Type], field: &str) -> Option<Type> {
        let sig = self.structs.get(sname)?;
        let (_, ft) = sig.fields.iter().find(|(n, _)| n == field)?;
        let map = self.struct_arg_map(sname, args);
        Some(zz_infer::subst(ft, &map))
    }

    /// Like [`Checker::direct_field_type`], but also returns the path of
    /// embedded field names leading to the field (empty = direct).
    /// Generic arguments thread through embedded levels: stepping into
    /// `Base` via a field of type `Base[int]` continues with
    /// `args=[int]`. Cycle-safe.
    fn resolve_struct_field_generic(
        &self,
        sname: &str,
        args: &[Type],
        field: &str,
    ) -> Option<(Vec<String>, Type)> {
        let mut visited = vec![sname.to_string()];
        let mut queue: Vec<(String, Vec<Type>, Vec<String>)> =
            vec![(sname.to_string(), args.to_vec(), Vec::new())];
        while let Some((cur, cur_args, path)) = queue.first().cloned() {
            queue.remove(0);
            let sig = self.structs.get(&cur)?;
            let map = self.struct_arg_map(&cur, &cur_args);
            if let Some((_, ft)) = sig.fields.iter().find(|(n, _)| n == field) {
                return Some((path, zz_infer::subst(ft, &map)));
            }
            for (fname, fty) in &sig.fields {
                let concrete = zz_infer::subst(fty, &map);
                if let Type::Struct(inner, inner_args) = &concrete {
                    if Self::is_embedded_field(fname, &concrete) && !visited.contains(inner) {
                        visited.push(inner.clone());
                        let mut next_path = path.clone();
                        next_path.push(fname.clone());
                        queue.push((inner.clone(), inner_args.clone(), next_path));
                    }
                }
            }
        }
        None
    }

    /// Concrete type of the embedded value defining `method` when called
    /// on `(sname, args)`: BFS over embedded fields carrying each level's
    /// substituted arguments, returning the first value whose struct base
    /// matches `defining` (mirrors [`Checker::find_struct_method`).
    /// The runtime passes this embedded value as the receiver, so method
    /// calls unify it (not the outer type) against the method's `self`.
    fn promoted_method_receiver(&self, sname: &str, args: &[Type], defining: &str) -> Option<Type> {
        let mut visited = vec![sname.to_string()];
        let mut queue: Vec<(String, Vec<Type>)> = vec![(sname.to_string(), args.to_vec())];
        while let Some((cur, cur_args)) = queue.first().cloned() {
            queue.remove(0);
            let sig = self.structs.get(&cur)?;
            let map = self.struct_arg_map(&cur, &cur_args);
            for (fname, fty) in &sig.fields {
                let concrete = zz_infer::subst(fty, &map);
                if let Type::Struct(inner, inner_args) = &concrete {
                    if inner == defining {
                        return Some(concrete.clone());
                    }
                    if Self::is_embedded_field(fname, &concrete) && !visited.contains(inner) {
                        visited.push(inner.clone());
                        queue.push((inner.clone(), inner_args.clone()));
                    }
                }
            }
        }
        None
    }
    /// First required leaf path under struct `sname` (reached via `prefix`)
    /// that is not covered by the given concrete literal paths. An explicit
    /// value at a path covers its whole subtree; otherwise embedded
    /// subtrees must be fully covered leaf-by-leaf. `None` = fully covered.
    fn first_uncovered_leaf(
        &self,
        sname: &str,
        prefix: &[String],
        given: &[Vec<String>],
        depth: usize,
    ) -> Option<Vec<String>> {
        if depth > 32 {
            return None;
        }
        let sig = self.structs.get(sname)?;
        for (fname, fty) in &sig.fields {
            let mut path = prefix.to_vec();
            path.push(fname.clone());
            if given.iter().any(|g| g == &path) {
                continue;
            }
            match fty {
                Type::Struct(inner, _) if Self::is_embedded_field(fname, fty) => {
                    if let Some(leaf) = self.first_uncovered_leaf(inner, &path, given, depth + 1) {
                        return Some(leaf);
                    }
                }
                _ => return Some(path),
            }
        }
        None
    }

    /// All field names visible on a struct: direct fields plus transitively
    /// promoted fields (nearest embedding wins on shadowing). Used for
    /// "did you mean" suggestions.
    fn all_visible_fields(&self, sname: &str) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        let mut visited = vec![sname.to_string()];
        let mut queue: Vec<String> = vec![sname.to_string()];
        while let Some(cur) = queue.first().cloned() {
            queue.remove(0);
            let Some(sig) = self.structs.get(&cur) else {
                continue;
            };
            for (fname, fty) in &sig.fields {
                if !out.contains(fname) {
                    out.push(fname.clone());
                }
                if let Type::Struct(inner, _) = fty {
                    if Self::is_embedded_field(fname, fty) && !visited.contains(inner) {
                        visited.push(inner.clone());
                        queue.push(inner.clone());
                    }
                }
            }
        }
        out
    }

    /// Find a method for a struct, searching embedded structs transitively
    /// when the outer struct defines no such method. Returns the defining
    /// struct's name and signature. Direct methods always win; breadth-first
    /// order keeps the nearest embedding's method on conflicts.
    fn find_struct_method(&self, sname: &str, method: &str) -> Option<(String, FuncSig)> {
        let mut visited = vec![sname.to_string()];
        let mut queue: Vec<String> = vec![sname.to_string()];
        while let Some(cur) = queue.first().cloned() {
            queue.remove(0);
            if let Some(sig) = self.funcs.get(&format!("{cur}.{method}")) {
                return Some((cur, sig.clone()));
            }
            // Cross-module fallback: `ns.method` for `ns.Type`.
            if let Some((ns, _)) = cur.rsplit_once('.') {
                if let Some(sig) = self.funcs.get(&format!("{ns}.{method}")) {
                    return Some((cur, sig.clone()));
                }
            }
            let Some(sig) = self.structs.get(&cur) else {
                continue;
            };
            for (fname, fty) in &sig.fields {
                if let Type::Struct(inner, _) = fty {
                    if Self::is_embedded_field(fname, fty) && !visited.contains(inner) {
                        visited.push(inner.clone());
                        queue.push(inner.clone());
                    }
                }
            }
        }
        None
    }

    fn struct_field_access(&mut self, sname: &str, args: &[Type], name: &str, span: Span) -> Type {
        match self.structs.get(sname).cloned() {
            Some(_) => match self.direct_field_type(sname, args, name) {
                Some(ft) => ft,
                None => match self.resolve_struct_field_generic(sname, args, name) {
                    // Promoted through an embedded struct.
                    Some((_, ft)) => ft,
                    None => {
                        let visible = self.all_visible_fields(sname);
                        let field_names: Vec<&str> = visible.iter().map(|n| n.as_str()).collect();
                        let mut diag =
                            error_at(format!("struct `{sname}` has no field `{name}`"), span);
                        let all = suggest_all(name, &field_names);
                        if let Some((suggestion, _)) = all.first() {
                            diag = diag.with_note(format!("did you mean field `{suggestion}`?"));
                            let field_span = Span::new(span.end - name.len() as u32, span.end);
                            let alts: Vec<String> =
                                all.iter().map(|(s, _)| s.to_string()).collect();
                            let fixit = if all.len() == 1 {
                                FixIt::safe(field_span, suggestion.to_string(), "replace field")
                            } else {
                                FixIt::ambiguous(
                                    field_span,
                                    suggestion.to_string(),
                                    "replace field",
                                    alts,
                                )
                            };
                            diag = diag.with_fixit(fixit);
                        }
                        self.errors.push(diag);
                        Type::Unit
                    }
                },
            },
            None => {
                self.errors
                    .push(error_at(format!("unknown struct `{sname}`"), span));
                Type::Unit
            }
        }
    }
}
