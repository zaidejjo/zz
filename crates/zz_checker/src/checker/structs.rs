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

use crate::checker::{Checker, FuncSig};
use crate::type_::Type;
use zz_frontend::ast::Stmt;
use zz_frontend::diag::error_at;

impl Checker {
    pub(crate) fn collect_struct(&mut self, stmt: &Stmt) {
        let (name, generics, fields) = match stmt {
            Stmt::Struct {
                name,
                generics,
                fields,
                ..
            } => (name, generics, fields),
            _ => unreachable!(),
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
        // Shadowing a builtin amid generics is still a collision; the
        // plain-struct path reports it elsewhere, so only check arity here.
        self.structs.insert(
            full_name,
            crate::checker::StructSig {
                generics: gen_names,
                fields: sig_fields,
            },
        );
    }

    /// True when a struct field is an embedded (anonymous) field: its type
    /// is a struct whose last name segment equals the field name.
    pub(crate) fn is_embedded_field(fname: &str, fty: &Type) -> bool {
        match fty {
            Type::Struct(s, _) => s.rsplit('.').next().unwrap_or(s) == fname,
            _ => false,
        }
    }

    /// Substitution map from a struct's generic parameters to the
    /// concrete arguments at this use site. Empty when arities differ
    /// (already reported elsewhere) — lookups then keep `Named` as-is.
    pub(crate) fn struct_arg_map(
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
    pub(crate) fn direct_field_type(
        &self,
        sname: &str,
        args: &[Type],
        field: &str,
    ) -> Option<Type> {
        let sig = self.structs.get(sname)?;
        let (_, ft) = sig.fields.iter().find(|(n, _)| n == field)?;
        let map = self.struct_arg_map(sname, args);
        Some(crate::checker::inference::subst(ft, &map))
    }

    /// Like [`Checker::direct_field_type`], but also returns the path of
    /// embedded field names leading to the field (empty = direct).
    /// Generic arguments thread through embedded levels: stepping into
    /// `Base` via a field of type `Base[int]` continues with
    /// `args=[int]`. Cycle-safe.
    pub(crate) fn resolve_struct_field_generic(
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
                return Some((path, crate::checker::inference::subst(ft, &map)));
            }
            for (fname, fty) in &sig.fields {
                let concrete = crate::checker::inference::subst(fty, &map);
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
    pub(crate) fn promoted_method_receiver(
        &self,
        sname: &str,
        args: &[Type],
        defining: &str,
    ) -> Option<Type> {
        let mut visited = vec![sname.to_string()];
        let mut queue: Vec<(String, Vec<Type>)> = vec![(sname.to_string(), args.to_vec())];
        while let Some((cur, cur_args)) = queue.first().cloned() {
            queue.remove(0);
            let sig = self.structs.get(&cur)?;
            let map = self.struct_arg_map(&cur, &cur_args);
            for (fname, fty) in &sig.fields {
                let concrete = crate::checker::inference::subst(fty, &map);
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
    pub(crate) fn first_uncovered_leaf(
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
    pub(crate) fn all_visible_fields(&self, sname: &str) -> Vec<String> {
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
    pub(crate) fn find_struct_method(
        &self,
        sname: &str,
        method: &str,
    ) -> Option<(String, FuncSig)> {
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
}
