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

impl Checker {
    pub(crate) fn collect_struct(&mut self, stmt: &Stmt) {
        let (name, fields) = match stmt {
            Stmt::Struct { name, fields, .. } => (name, fields),
            _ => unreachable!(),
        };
        let gens = self.current_generics.clone();
        let sig_fields = fields
            .iter()
            .map(|(fname, fty)| (fname.name.clone(), self.ast_to_type(fty, &gens)))
            .collect();
        let full_name = name.join(".");
        self.structs
            .insert(full_name, crate::checker::StructSig { fields: sig_fields });
    }

    /// True when a struct field is an embedded (anonymous) field: its type
    /// is a struct whose last name segment equals the field name.
    pub(crate) fn is_embedded_field(fname: &str, fty: &Type) -> bool {
        match fty {
            Type::Struct(s) => s.rsplit('.').next().unwrap_or(s) == fname,
            _ => false,
        }
    }

    /// Direct field type, or the type promoted through embedded structs
    /// (breadth-first, so the nearest embedding wins). Cycle-safe.
    pub(crate) fn resolve_struct_field(&self, sname: &str, field: &str) -> Option<Type> {
        self.resolve_struct_field_path(sname, field)
            .map(|(_, ty)| ty)
    }

    /// Like [`Checker::resolve_struct_field`], but also returns the path of
    /// embedded field names leading to the field (empty = direct).
    pub(crate) fn resolve_struct_field_path(
        &self,
        sname: &str,
        field: &str,
    ) -> Option<(Vec<String>, Type)> {
        let mut visited = vec![sname.to_string()];
        // Queue of (struct name, path of embedded fields to reach it).
        let mut queue: Vec<(String, Vec<String>)> = vec![(sname.to_string(), Vec::new())];
        while let Some((cur, path)) = queue.first().cloned() {
            queue.remove(0);
            let sig = self.structs.get(&cur)?;
            if let Some((_, ft)) = sig.fields.iter().find(|(n, _)| n == field) {
                // Direct hits win at every level; an embedded field's own
                // name also resolves (so `u.Base` keeps working).
                return Some((path, ft.clone()));
            }
            for (fname, fty) in &sig.fields {
                if let Type::Struct(inner) = fty {
                    if Self::is_embedded_field(fname, fty) && !visited.contains(inner) {
                        visited.push(inner.clone());
                        let mut next_path = path.clone();
                        next_path.push(fname.clone());
                        queue.push((inner.clone(), next_path));
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
                Type::Struct(inner) if Self::is_embedded_field(fname, fty) => {
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
                if let Type::Struct(inner) = fty {
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
                if let Type::Struct(inner) = fty {
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
