//! Scope management and variable lookup.

use zz_checker_core::{Checker, DefinedBinding};
use zz_diag_core::{error_at, warning_at, FixIt};
use zz_span_ast::span::Span;
use zz_structs::StructOps;
use zz_suggest::{suggest_all, suggest_dotted};
use zz_types::Type;

// Import suggestions and canonical resolution derive from the single
// `super::STD_MODULES` source (see `is_std_module_head`) — no parallel
// list to drift.

pub trait ScopeOps {
    fn push_scope(&mut self);

    /// Strip the module prefix from a name for display.
    /// `"diag-typo.area"` → `"area"`, `"area"` → `"area"`.
    fn display_name(name: &str) -> &str;

    /// True when `name` spells a `std.math` numeric constant:
    /// `std.math.PI`, `math.PI` (via `import std.math`), or bare `PI`
    /// (via `import std.math(PI)` / `import std.math(*)`).
    ///
    /// Constants are true `Float` values, not zero-arg functions. Call sites
    /// additionally require the name to be present in `funcs` so import
    /// gating still applies (using `math.PI` without the import stays an
    /// "undefined variable" error).
    fn is_math_const(&self, name: &str) -> bool;

    fn pop_scope(&mut self);

    /// Emit `unused variable` warnings for one scope's recorded bindings,
    /// in the caller-provided (source) order. Bindings marked
    /// `warn_unused: false` (closure parameters) are skipped: their
    /// position comes from the callee's signature, so there is nothing
    /// the author could remove.
    fn emit_unused_binding_warnings(
        &mut self,
        scope_index: usize,
        entries: &[(String, DefinedBinding)],
    );

    fn define(&mut self, name: &str, ty: Type);

    fn define_at(&mut self, name: &str, ty: Type, span: Span);

    /// Define a binding, recording whether its unused-ness is worth
    /// warning about. Closure parameters pass `warn_unused: false`: the
    /// callee dictates the closure's arity, so a parameter cannot be
    /// removed and `_name` would only hide what the API signature says.
    fn define_binding_at(&mut self, name: &str, ty: Type, span: Span, warn_unused: bool);

    /// Define a variable, recording its immutability when `is_const`.
    fn define_var_at(&mut self, name: &str, ty: Type, span: Span, is_const: bool);

    /// Define an immutable (`const`) variable, remembering its declaration
    /// span so reassignment errors can point back at it.
    fn define_const_at(&mut self, name: &str, ty: Type, span: Span);

    /// If `name` resolves to a `const` binding, return its declaration span.
    /// Shadowing-aware: only the scope that actually binds the name is
    /// consulted, so an inner mutable shadow of an outer const is mutable.
    fn lookup_const_span(&mut self, name: &str) -> Option<Span>;

    /// Emit unused-variable warnings for the global scope (scope index 0).
    /// The global scope is never popped, so `pop_scope`'s check does not
    /// fire for top-level definitions. Also emits unused-import warnings.
    fn emit_global_unused_warnings(&mut self);

    fn lookup(&mut self, name: &str, span: Span) -> Type;

    /// Start observing `name` while its `:=` initializer is checked (#518).
    /// Returns the marker for [`Checker::finish_shadow_probes`].
    fn begin_shadow_probe(&mut self, name: String, span: Span) -> usize;

    /// Stop observing and return every probe whose initializer read the
    /// binding it shadows, outermost first.
    fn finish_shadow_probes(&mut self, mark: usize) -> Vec<zz_checker_core::ShadowProbe>;

    /// Like [`Checker::lookup`] but without the error: returns `None` when
    /// the name is not bound anywhere.
    fn lookup_opt(&mut self, name: &str) -> Option<Type>;

    /// Resolve a dotted path: first as a single qualified name (module
    /// bindings/functions), then as a struct-field walk from the first part.
    fn lookup_path(&mut self, parts: &[String], span: Span) -> Type;
}

impl ScopeOps for Checker {
    // --- environments -----------------------------------------------------

    fn push_scope(&mut self) {
        self.env.push(std::collections::HashMap::new());
        self.defined_names.push(std::collections::HashMap::new());
        self.const_env.push(std::collections::HashMap::new());
    }

    /// Strip the module prefix from a name for display.
    /// `"diag-typo.area"` → `"area"`, `"area"` → `"area"`.
    fn display_name(name: &str) -> &str {
        match name.rfind('.') {
            Some(pos) => &name[pos + 1..],
            None => name,
        }
    }

    /// True when `name` spells a `std.math` numeric constant:
    /// `std.math.PI`, `math.PI` (via `import std.math`), or bare `PI`
    /// (via `import std.math(PI)` / `import std.math(*)`).
    ///
    /// Constants are true `Float` values, not zero-arg functions. Call sites
    /// additionally require the name to be present in `funcs` so import
    /// gating still applies (using `math.PI` without the import stays an
    /// "undefined variable" error).
    fn is_math_const(&self, name: &str) -> bool {
        const LEAVES: &[&str] = &[
            "PI", "E", "TAU", "SQRT_2", "SQRT_1_2", "LN_2", "LN_10", "LOG10_E", "LOG2_E", "INF",
            "NAN",
        ];
        // Resolve selective-import aliases first (`pi` from
        // `import std.math(PI as pi)` checks as `math.PI`).
        let resolved = self
            .import_aliases
            .get(name)
            .map(|s| s.as_str())
            .unwrap_or(name);
        let (prefix, leaf) = match resolved.rsplit_once('.') {
            Some((p, l)) => (p, l),
            None => ("", resolved),
        };
        if !LEAVES.contains(&leaf) {
            return false;
        }
        if matches!(prefix, "" | "math" | "std.math") {
            return true;
        }
        // Resolve module-head aliases (`m.TAU` from `import std.math as m`).
        if let Some(module) = self.module_aliases.get(prefix) {
            return matches!(module.as_str(), "math" | "std.math");
        }
        false
    }

    fn pop_scope(&mut self) {
        // Warn about unused variables in this scope (skip the global scope).
        if let Some(defined) = self.defined_names.pop() {
            let scope_index = self.defined_names.len();
            // Sort by (span, name): HashMap iteration order is
            // per-process random, and diagnostics must be byte-identical
            // across runs (the dual-engine parity harness diffs stderr
            // between two separate `zz` processes).
            let mut entries: Vec<(String, DefinedBinding)> = defined.into_iter().collect();
            entries.sort_by(|a, b| {
                (a.1.span.start, a.1.span.end, &a.0).cmp(&(b.1.span.start, b.1.span.end, &b.0))
            });
            self.emit_unused_binding_warnings(scope_index, &entries);
            // Reads of this scope's bindings can no longer be observed.
            self.used_bindings.retain(|(idx, _)| *idx != scope_index);
        }
        self.env.pop();
        self.const_env.pop();
    }

    /// Emit `unused variable` warnings for one scope's recorded bindings,
    /// in the caller-provided (source) order. Bindings marked
    /// `warn_unused: false` (closure parameters) are skipped: their
    /// position comes from the callee's signature, so there is nothing
    /// the author could remove.
    fn emit_unused_binding_warnings(
        &mut self,
        scope_index: usize,
        entries: &[(String, DefinedBinding)],
    ) {
        for (name, binding) in entries {
            if !binding.warn_unused {
                continue;
            }
            let display = Self::display_name(name);
            if !self.used_bindings.contains(&(scope_index, name.clone()))
                && !self.pub_names.contains(name)
                && !display.starts_with('_')
            {
                let fixit_name = format!("_{display}");
                self.errors.push(
                    warning_at(
                        format!(
                            "unused variable `{display}`, consider prefixing with `_{display}`"
                        ),
                        binding.span,
                    )
                    .with_note("variable is never read")
                    .with_fixit(FixIt::safe(
                        binding.span,
                        fixit_name,
                        "rename to",
                    )),
                );
            }
        }
    }

    fn define(&mut self, name: &str, ty: Type) {
        self.define_at(name, ty, Span::new(0, 0));
    }

    fn define_at(&mut self, name: &str, ty: Type, span: Span) {
        self.define_binding_at(name, ty, span, true);
    }

    /// Define a binding, recording whether its unused-ness is worth
    /// warning about. Closure parameters pass `warn_unused: false`: the
    /// callee dictates the closure's arity, so a parameter cannot be
    /// removed and `_name` would only hide what the API signature says.
    fn define_binding_at(&mut self, name: &str, ty: Type, span: Span, warn_unused: bool) {
        self.env.last_mut().unwrap().insert(name.to_string(), ty);
        // Record the definition span for unused-variable warnings.
        if let Some(scope) = self.defined_names.last_mut() {
            scope.insert(name.to_string(), DefinedBinding { span, warn_unused });
        }
    }

    /// Define a variable, recording its immutability when `is_const`.
    fn define_var_at(&mut self, name: &str, ty: Type, span: Span, is_const: bool) {
        if is_const {
            self.define_const_at(name, ty, span);
        } else {
            self.define_at(name, ty, span);
        }
    }

    /// Define an immutable (`const`) variable, remembering its declaration
    /// span so reassignment errors can point back at it.
    fn define_const_at(&mut self, name: &str, ty: Type, span: Span) {
        self.define_at(name, ty, span);
        if let Some(scope) = self.const_env.last_mut() {
            scope.insert(name.to_string(), span);
        }
    }

    /// If `name` resolves to a `const` binding, return its declaration span.
    /// Shadowing-aware: only the scope that actually binds the name is
    /// consulted, so an inner mutable shadow of an outer const is mutable.
    fn lookup_const_span(&mut self, name: &str) -> Option<Span> {
        for (i, scope) in self.env.iter().enumerate().rev() {
            if scope.contains_key(name) {
                if let Some(s) = self.const_env[i].get(name) {
                    return Some(*s);
                }
                return None;
            }
        }
        None
    }

    /// Emit unused-variable warnings for the global scope (scope index 0).
    /// The global scope is never popped, so `pop_scope`'s check does not
    /// fire for top-level definitions. Also emits unused-import warnings.
    fn emit_global_unused_warnings(&mut self) {
        // --- Unused variables ---
        {
            // Snapshot defined names to avoid borrow issues, sorted by
            // (span, name): HashMap iteration order is per-process random
            // (see pop_scope).
            let mut entries: Vec<(String, DefinedBinding)> = self
                .defined_names
                .first()
                .map(|defined| {
                    defined
                        .iter()
                        .map(|(k, &v)| (k.clone(), v))
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            entries.sort_by(|a, b| {
                (a.1.span.start, a.1.span.end, &a.0).cmp(&(b.1.span.start, b.1.span.end, &b.0))
            });
            self.emit_unused_binding_warnings(0, &entries);
        }

        // --- Unused imports ---
        let imports: Vec<(String, Span)> = self.imports.clone();
        for (alias, span) in &imports {
            let prefix = format!("{alias}.");
            let used = self
                .used_names
                .iter()
                .any(|n| n == alias || n.starts_with(&prefix))
                // Selective imports rewritten to canonical paths
                // (`join(...)` → `path.join(...)` by the loader): the
                // import is used when its qualified form was used. The
                // loader canonicalizes `std.*` calls to the `std.`-rooted
                // spelling (`pow(...)` → `std.math.pow(...)`), which never
                // matches the short alias form (`math.pow`) — so the
                // `std.`-rooted spelling counts too, or every selectively
                // imported std call warns falsely.
                || self.import_aliases.get(alias).is_some_and(|q| {
                    let qdot = format!("{q}.");
                    let stdq = format!("std.{q}");
                    let stdqdot = format!("{stdq}.");
                    self.used_names.iter().any(|n| {
                        n == q
                            || n.starts_with(&qdot)
                            || n == &stdq
                            || n.starts_with(&stdqdot)
                    })
                });
            if !used {
                self.errors.push(
                    warning_at(format!("unused import `{alias}`"), *span)
                        .with_note("remove this import or use it in the program"),
                );
            }
        }
    }

    fn lookup(&mut self, name: &str, span: Span) -> Type {
        match self.lookup_opt(name) {
            Some(t) => {
                self.used_names.insert(name.to_string());
                t
            }
            None => {
                if let Some(sig) = zz_checker_core::lookup_std(&self.funcs, name) {
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
                // stdlib pattern, suggest adding the import — but only when
                // the import is not already present (#710). When the module
                // is already imported, the problem is the member name, so
                // suggest the closest member instead.
                if let Some((module, func)) = name.split_once('.') {
                    let std_module = if zz_checker_core::is_std_module_head(module) {
                        Some(module.to_string())
                    } else {
                        // Generalized: any other std module present in the
                        // seed (`std.M.*` keys), not just the table above.
                        self.funcs
                            .keys()
                            .any(|k| k.starts_with(&format!("std.{module}.")))
                            .then(|| module.to_string())
                    };
                    if let Some(mod_name) = std_module {
                        // Suppress when the module namespace is already bound
                        // by an import (full or aliased). Selective imports
                        // bind member names, not the namespace, so they do
                        // not suppress here — `math.sin` with only
                        // `import std.math(sin)` still needs `math`.
                        let import_present = self
                            .imports
                            .iter()
                            .any(|(alias, _)| alias.as_str() == mod_name)
                            || self.module_aliases.contains_key(&mod_name);
                        if !import_present {
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
                        } else {
                            // Module is already imported: the problem is the
                            // member name. Suggest the closest member of the
                            // module (`fs.delete` → `fs.remove`).
                            let prefix = format!("std.{mod_name}.");
                            let members: Vec<&str> = self
                                .funcs
                                .keys()
                                .chain(self.structs.keys())
                                .filter(|k| {
                                    k.starts_with(&prefix) && !k[prefix.len()..].contains('.')
                                })
                                .map(|k| k.strip_prefix(&prefix).unwrap())
                                .collect();
                            if let Some((suggestion, _dist)) = suggest_all(func, &members).first() {
                                diag = diag.with_note(format!(
                                    "`{mod_name}` is imported; did you mean `{mod_name}.{suggestion}`?"
                                ));
                            } else {
                                diag = diag.with_note(format!(
                                    "`{mod_name}` is imported; `{member}` is not a member of `{mod_name}`",
                                    member = func
                                ));
                            }
                        }
                    }
                } else {
                    // Bare name matching exactly one std export (`sin` →
                    // `std.math.sin`): point at the import that provides it.
                    // Capped at 3 modules; more than that is a common word,
                    // not a missed import.
                    let mut providers: Vec<String> = Vec::new();
                    for key in self.funcs.keys().chain(self.structs.keys()) {
                        if let Some(rest) = key.strip_prefix("std.") {
                            if let Some((module, bare)) = rest.split_once('.') {
                                if bare == name && !bare.contains('.') {
                                    let m = module.to_string();
                                    if !providers.contains(&m) {
                                        providers.push(m);
                                    }
                                }
                            }
                        }
                    }
                    providers.sort();
                    providers.truncate(3);
                    if !providers.is_empty() {
                        let imports = providers
                            .iter()
                            .map(|m| format!("`import std.{m}`"))
                            .collect::<Vec<_>>()
                            .join(", ");
                        diag = diag.with_note(format!(
                            "to use `{name}`, add {imports} at the top of the file"
                        ));
                        if providers.len() == 1 {
                            diag = diag.with_fixit(FixIt::safe(
                                Span::new(0, 0),
                                format!("import std.{}\n", providers[0]),
                                "add import",
                            ));
                        }
                    }
                }
                self.errors.push(diag);
                self.had_undefined_var = true;
                Type::Error
            }
        }
    }

    /// Start observing `name` while its `:=` initializer is checked (#518).
    /// Returns the marker for [`Checker::finish_shadow_probes`].
    fn begin_shadow_probe(&mut self, name: String, span: Span) -> usize {
        let mark = self.shadow_probes.len();
        self.shadow_probes.push(zz_checker_core::ShadowProbe {
            name,
            span,
            depth: self.env.len().saturating_sub(1),
            referenced: false,
        });
        mark
    }

    /// Stop observing and return every probe whose initializer read the
    /// binding it shadows, outermost first.
    fn finish_shadow_probes(&mut self, mark: usize) -> Vec<zz_checker_core::ShadowProbe> {
        let mut out = Vec::new();
        while self.shadow_probes.len() > mark {
            let probe = self.shadow_probes.pop().expect("probe above the mark");
            if probe.referenced {
                out.push(probe);
            }
        }
        out
    }

    /// Like [`Checker::lookup`] but without the error: returns `None` when
    /// the name is not bound anywhere.
    fn lookup_opt(&mut self, name: &str) -> Option<Type> {
        for (depth, scope) in self.env.iter().enumerate().rev() {
            if let Some(t) = scope.get(name) {
                let t = t.clone();
                self.used_names.insert(name.to_string());
                // Binding-level usage: an inner shadow must not keep an
                // outer same-named binding alive (and vice versa).
                self.used_bindings.insert((depth, name.to_string()));
                // #518: a read resolving to a *shallower* scope hit the
                // binding the declaration under check would shadow. Fields
                // are borrowed disjointly — `self.env` above is still live.
                for probe in self.shadow_probes.iter_mut() {
                    if probe.name == name && depth < probe.depth {
                        probe.referenced = true;
                    }
                }
                return Some(t);
            }
        }
        if let Some(sig) = zz_checker_core::lookup_std(&self.funcs, name) {
            self.used_names.insert(name.to_string());
            if !sig.generics.is_empty() {
                return None;
            }
            // Math constants (`math.PI`, `std.math.PI`, bare `PI` via a
            // selective import) are true `Float` values, not function
            // values — `println(math.PI)` must not suggest `()`.
            if sig.params.is_empty() && matches!(sig.ret, Type::Float) && self.is_math_const(name) {
                return Some(Type::Float);
            }
            // Function used as a value: give its (uninstantiated) type. Call
            // sites handle generic instantiation via the Named path below.
            return Some(Type::Named(name.to_string()));
        }
        None
    }

    /// Resolve a dotted path: first as a single qualified name (module
    /// bindings/functions), then as a struct-field walk from the first part.
    fn lookup_path(&mut self, parts: &[String], span: Span) -> Type {
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
            // A lone namespace head (`env` in `env.tmp_dir(...)`) is a
            // receiver probe, not a missing value: stay silent here and
            // let the caller's joined-name check report (with its typo
            // hint). Bare `env` as a value still errors via `lookup`.
            if parts.len() == 1 {
                let prefix = format!("{}.", parts[0]);
                if candidates.iter().any(|c| c.starts_with(&prefix)) {
                    return Type::Error;
                }
            }
            let mut diag = error_at(format!("undefined variable `{joined}`"), span);
            // Unimported-but-known std module root (`math.sin` without
            // `import std.math`): suggest the import alongside any typo
            // hint below. Reaching here means the root did not resolve,
            // so a known module name is necessarily unimported. Attached
            // before the typo branches because the dotted branch returns
            // early.
            if parts.len() >= 2 && zz_checker_core::is_std_module_head(&parts[0]) {
                let mod_name = &parts[0];
                // Suppress the import hint when the module is already
                // imported (#710): the real problem is the member name.
                let import_present = self.imports.iter().any(|(a, _)| a == mod_name)
                    || self.module_aliases.contains_key(mod_name);
                if import_present {
                    // Module is imported: point at the closest member
                    // (`fs.delete` → `fs.remove`).
                    let prefix = format!("std.{mod_name}.");
                    let members: Vec<&str> = self
                        .funcs
                        .keys()
                        .chain(self.structs.keys())
                        .filter(|k| k.starts_with(&prefix) && !k[prefix.len()..].contains('.'))
                        .map(|k| k.strip_prefix(&prefix).unwrap())
                        .collect();
                    if let Some((suggestion, _)) = suggest_all(&parts[1], &members).first() {
                        diag = diag.with_note(format!(
                            "`{mod_name}` is imported; did you mean `{mod_name}.{suggestion}`?"
                        ));
                    } else {
                        diag = diag.with_note(format!(
                            "`{mod_name}` is imported; `{member}` is not a member of `{mod_name}`",
                            member = parts[1]
                        ));
                    }
                } else {
                    let import_stmt = format!("import std.{mod_name}");
                    diag = diag.with_note(format!(
                        "add `{import_stmt}` at the top of the file to use `{joined}`"
                    ));
                    diag = diag.with_fixit(FixIt::safe(
                        Span::new(0, 0),
                        format!("{import_stmt}\n"),
                        "add import",
                    ));
                }
            }
            // Dotted paths (`env.tmp_dir`) match segment-wise first:
            // whole-string Levenshtein drowns a 1-char tail typo in the
            // shared prefix. Falls back to root-name suggestions.
            if parts.len() >= 2 {
                if let Some(dotted) = suggest_dotted(&joined, &candidates) {
                    diag = diag.with_note(format!("did you mean `{dotted}`?"));
                    diag =
                        diag.with_fixit(FixIt::safe(span, dotted.to_string(), "replace variable"));
                    self.errors.push(diag);
                    self.had_undefined_var = true;
                    return Type::Error;
                }
            }
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
                Type::Struct(name, args) => {
                    // On failure the helper reports and yields `Unit`;
                    // paths stop here (historical: `Type::Error`) so a
                    // bad middle segment doesn't cascade per segment.
                    let before = self.errors.len();
                    ty = self.struct_field_access(&name, &args, field, span);
                    if self.errors.len() > before {
                        return Type::Error;
                    }
                }
                Type::Dict(k, v) => {
                    // Dict field access: req.body returns the value type
                    if let Err(e) = self.unifier.unify(&Type::Str, &k) {
                        self.report_mismatch(e, span);
                    }
                    ty = *v;
                }
                Type::HttpRequest => {
                    // Typed request field access: `req.method/path/body`
                    // are `str`; `req.headers/query/params` are `{str: str}`.
                    ty = match field.as_str() {
                        "method" | "path" | "body" => Type::Str,
                        "headers" | "query" | "params" => {
                            Type::Dict(Box::new(Type::Str), Box::new(Type::Str))
                        }
                        _ => {
                            self.errors.push(error_at(
                                format!("http.request has no field `{field}`"),
                                span,
                            ));
                            return Type::Error;
                        }
                    };
                }
                Type::Var(_) => {
                    // Inference variable — not yet resolved (e.g. untyped closure param).
                    // Return a fresh var; unification will catch real mismatches later.
                    ty = self.unifier.fresh_var();
                }
                Type::Error => {
                    // Cascade root already reported: a field access on an
                    // `<error>` value must not spawn a new diagnostic
                    // (#711). Keep the error type so downstream accesses
                    // also stay silent.
                    ty = Type::Error;
                }
                other => {
                    let mut diag = error_at(
                        format!("cannot access field `{field}` on a value of type `{other}`"),
                        span,
                    );
                    // Unimported-but-known std module root (`sqlz.open`
                    // without `import std.sqlz`): suggest the import even
                    // when the root name itself is bound to something else
                    // (partial binding / alias).
                    if parts.len() >= 2 && zz_checker_core::is_std_module_head(&parts[0]) {
                        let import_stmt = format!("import std.{}", parts[0]);
                        diag = diag.with_note(format!(
                            "add `{import_stmt}` at the top of the file to use `{joined}`"
                        ));
                        diag = diag.with_fixit(FixIt::safe(
                            Span::new(0, 0),
                            format!("{import_stmt}\n"),
                            "add import",
                        ));
                    }
                    self.errors.push(diag);
                    return Type::Error;
                }
            }
        }
        ty
    }
}
