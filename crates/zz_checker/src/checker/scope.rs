//! Scope management and variable lookup.

use crate::checker::Checker;
use crate::type_::Type;
use zz_frontend::diag::{error_at, warning_at, FixIt};
use zz_frontend::levenshtein::suggest_all;
use zz_frontend::span::Span;

impl Checker {
    // --- environments -----------------------------------------------------

    pub(crate) fn push_scope(&mut self) {
        self.env.push(std::collections::HashMap::new());
        self.defined_names.push(std::collections::HashMap::new());
    }

    /// Strip the module prefix from a name for display.
    /// `"diag-typo.area"` → `"area"`, `"area"` → `"area"`.
    pub(crate) fn display_name(name: &str) -> &str {
        match name.rfind('.') {
            Some(pos) => &name[pos + 1..],
            None => name,
        }
    }

    pub(crate) fn pop_scope(&mut self) {
        // Warn about unused variables in this scope (skip the global scope).
        if let Some(defined) = self.defined_names.pop() {
            for (name, span) in &defined {
                let display = Self::display_name(name);
                if !self.used_names.contains(name)
                    && !self.pub_names.contains(name)
                    && !display.starts_with('_')
                {
                    let fixit_name = format!("_{display}");
                    self.errors.push(
                        warning_at(
                            format!(
                                "unused variable `{display}`, consider prefixing with `_{display}`"
                            ),
                            *span,
                        )
                        .with_note("variable is never read")
                        .with_fixit(FixIt::safe(
                            *span,
                            fixit_name,
                            "rename to",
                        )),
                    );
                }
            }
        }
        self.env.pop();
    }

    pub(crate) fn define(&mut self, name: &str, ty: Type) {
        self.define_at(name, ty, Span::new(0, 0));
    }

    pub(crate) fn define_at(&mut self, name: &str, ty: Type, span: Span) {
        self.env.last_mut().unwrap().insert(name.to_string(), ty);
        // Record the definition span for unused-variable warnings.
        if let Some(scope) = self.defined_names.last_mut() {
            scope.insert(name.to_string(), span);
        }
    }

    /// Emit unused-variable warnings for the global scope (scope index 0).
    /// The global scope is never popped, so `pop_scope`'s check does not
    /// fire for top-level definitions. Also emits unused-import warnings.
    pub(crate) fn emit_global_unused_warnings(&mut self) {
        // --- Unused variables ---
        if let Some(defined) = self.defined_names.first() {
            // Snapshot defined names to avoid borrow issues.
            let entries: Vec<(String, Span)> =
                defined.iter().map(|(k, &v)| (k.clone(), v)).collect();
            for (name, span) in &entries {
                let display = Self::display_name(name);
                if !self.used_names.contains(name)
                    && !self.pub_names.contains(name)
                    && !display.starts_with('_')
                {
                    let fixit_name = format!("_{display}");
                    self.errors.push(
                        warning_at(
                            format!(
                                "unused variable `{display}`, consider prefixing with `_{display}`"
                            ),
                            *span,
                        )
                        .with_note("variable is never read")
                        .with_fixit(FixIt::safe(
                            *span,
                            fixit_name,
                            "rename to",
                        )),
                    );
                }
            }
        }

        // --- Unused imports ---
        let imports: Vec<(String, Span)> = self.imports.clone();
        for (alias, span) in &imports {
            let prefix = format!("{alias}.");
            let used = self
                .used_names
                .iter()
                .any(|n| n == alias || n.starts_with(&prefix));
            if !used {
                self.errors.push(
                    warning_at(format!("unused import `{alias}`"), *span)
                        .with_note("remove this import or use it in the program"),
                );
            }
        }
    }

    pub(crate) fn lookup(&mut self, name: &str, span: Span) -> Type {
        match self.lookup_opt(name) {
            Some(t) => {
                self.used_names.insert(name.to_string());
                t
            }
            None => {
                if let Some(sig) = self.funcs.get(name) {
                    if !sig.generics.is_empty() {
                        self.errors.push(error_at(
                            format!(
                                "cannot use generic function `{name}` as a value; call it with arguments"
                            ),
                            span,
                        ));
                        return Type::Error;
                    }
                }
                // Build candidate list for typo suggestions.
                let mut candidates: Vec<&str> = Vec::new();
                for scope in self.env.iter().rev() {
                    for key in scope.keys() {
                        candidates.push(key);
                    }
                }
                for key in self.funcs.keys() {
                    candidates.push(key);
                }
                for key in self.structs.keys() {
                    candidates.push(key);
                }
                // Also add bare-name versions for module-qualified candidates.
                // e.g. "diag-typo.height" → also add "height" so Levenshtein
                // can match "hieght" → "height".
                let mut extras: Vec<String> = Vec::new();
                for c in &candidates {
                    if let Some(bare) = c.rsplit('.').next() {
                        if bare != *c {
                            extras.push(bare.to_string());
                        }
                    }
                }
                for e in &extras {
                    candidates.push(e);
                }
                let mut diag = error_at(format!("undefined variable `{name}`"), span);
                let all = suggest_all(name, &candidates);
                if let Some((suggestion, _dist)) = all.first() {
                    diag = diag.with_note(format!("did you mean `{suggestion}`?"));
                    let fixit = if all.len() == 1 {
                        FixIt::safe(span, suggestion.to_string(), "replace variable")
                    } else {
                        let alts: Vec<String> = all.iter().map(|(s, _)| s.to_string()).collect();
                        FixIt::ambiguous(span, suggestion.to_string(), "replace variable", alts)
                    };
                    diag = diag.with_fixit(fixit);
                }

                // If the name looks like `module.func` and matches a known
                // stdlib pattern, suggest adding the import.
                if let Some((module, _func)) = name.split_once('.') {
                    let std_module = match module {
                        "io" | "str" | "vec" | "json" | "http" | "fs" | "env" | "math" | "time" => {
                            Some(module)
                        }
                        _ => None,
                    };
                    if let Some(mod_name) = std_module {
                        let import_stmt = format!("import std.{mod_name}");
                        diag = diag.with_note(format!(
                            "add `{import_stmt}` at the top of the file to use `{name}`"
                        ));
                        // Create an "add import" fixit that inserts at the top.
                        diag = diag.with_fixit(FixIt::safe(
                            Span::new(0, 0),
                            format!("{import_stmt}\n"),
                            "add import",
                        ));
                    }
                }
                self.errors.push(diag);
                self.had_undefined_var = true;
                Type::Error
            }
        }
    }

    /// Like [`Checker::lookup`] but without the error: returns `None` when
    /// the name is not bound anywhere.
    pub(crate) fn lookup_opt(&mut self, name: &str) -> Option<Type> {
        for scope in self.env.iter().rev() {
            if let Some(t) = scope.get(name) {
                self.used_names.insert(name.to_string());
                return Some(t.clone());
            }
        }
        if let Some(sig) = self.funcs.get(name) {
            self.used_names.insert(name.to_string());
            if !sig.generics.is_empty() {
                return None;
            }
            // Function used as a value: give its (uninstantiated) type. Call
            // sites handle generic instantiation via the Named path below.
            return Some(Type::Named(name.to_string()));
        }
        None
    }

    /// Resolve a dotted path: first as a single qualified name (module
    /// bindings/functions), then as a struct-field walk from the first part.
    pub(crate) fn lookup_path(&mut self, parts: &[String], span: Span) -> Type {
        let joined = parts.join(".");
        if let Some(t) = self.lookup_opt(&joined) {
            return t;
        }
        let Some(root) = self.lookup_opt(&parts[0]) else {
            // Suggest typo corrections for the root name.
            let mut candidates: Vec<&str> = Vec::new();
            for scope in self.env.iter().rev() {
                for key in scope.keys() {
                    candidates.push(key);
                }
            }
            for key in self.funcs.keys() {
                candidates.push(key);
            }
            for key in self.structs.keys() {
                candidates.push(key);
            }
            let mut diag = error_at(format!("undefined variable `{joined}`"), span);
            let all = suggest_all(&parts[0], &candidates);
            if let Some((suggestion, _)) = all.first() {
                diag = diag.with_note(format!("did you mean `{suggestion}`?"));
                let root_span = Span::new(span.start, span.start + parts[0].len() as u32);
                let fixit = if all.len() == 1 {
                    FixIt::safe(root_span, suggestion.to_string(), "replace variable")
                } else {
                    let alts: Vec<String> = all.iter().map(|(s, _)| s.to_string()).collect();
                    FixIt::ambiguous(root_span, suggestion.to_string(), "replace variable", alts)
                };
                diag = diag.with_fixit(fixit);
            }
            self.errors.push(diag);
            self.had_undefined_var = true;
            return Type::Error;
        };
        let mut ty = root;
        for field in &parts[1..] {
            match self.unifier.resolve(&ty) {
                Type::Struct(name) => match self.structs.get(&name) {
                    Some(sig) => match sig.fields.iter().find(|(n, _)| n == field) {
                        Some((_, ft)) => ty = ft.clone(),
                        None => {
                            // Suggest closest field name.
                            let field_names: Vec<&str> =
                                sig.fields.iter().map(|(n, _)| n.as_str()).collect();
                            let mut diag =
                                error_at(format!("struct `{name}` has no field `{field}`"), span);
                            let all = suggest_all(field, &field_names);
                            if let Some((suggestion, _)) = all.first() {
                                diag =
                                    diag.with_note(format!("did you mean field `{suggestion}`?"));
                                let field_span = Span::new(span.end - field.len() as u32, span.end);
                                let fixit = if all.len() == 1 {
                                    FixIt::safe(field_span, suggestion.to_string(), "replace field")
                                } else {
                                    let alts: Vec<String> =
                                        all.iter().map(|(s, _)| s.to_string()).collect();
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
                            return Type::Error;
                        }
                    },
                    None => {
                        self.errors
                            .push(error_at(format!("unknown struct `{name}`"), span));
                        return Type::Error;
                    }
                },
                Type::Dict(k, v) => {
                    // Dict field access: req.body returns the value type
                    if let Err(e) = self.unifier.unify(&Type::Str, &k) {
                        self.report_mismatch(e, span);
                    }
                    ty = *v;
                }
                Type::Var(_) => {
                    // Inference variable — not yet resolved (e.g. untyped closure param).
                    // Return a fresh var; unification will catch real mismatches later.
                    ty = self.unifier.fresh_var();
                }
                other => {
                    self.errors.push(error_at(
                        format!("cannot access field `{field}` on a value of type `{other}`"),
                        span,
                    ));
                    return Type::Error;
                }
            }
        }
        ty
    }
}
