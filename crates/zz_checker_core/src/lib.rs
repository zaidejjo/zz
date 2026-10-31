//! Shared type-checker state.
//!
//! [`Checker`] owns every table and cursor the six-pass checker walks
//! (signatures, scopes, diagnostics, span types). Behavior lives in
//! per-domain crates that implement extension traits for it; this crate
//! defines the state, its constructors, and the low-level helpers several
//! domains share.

use std::any::{Any, TypeId};
use std::collections::HashMap;
use std::sync::Arc;

use zz_diag_core::{error_at, RawDiag};
use zz_span_ast::ast::{Stmt, Ty};
use zz_span_ast::span::Span;
use zz_types::Type;
use zz_unify::{Unifier, UnifyError};

/// Pseudo-scope owning all top-level (non-function) statements. Mirrors
/// `zz_hir::callgraph::TOP`: expression spans are only unique *within* one
/// function body (every module restarts offsets at 0), so span-keyed maps
/// must always pair the span with its enclosing scope.
pub const TOP_SCOPE: &str = "<top>";

/// Scope-qualified expression key for the typed AST view. Plain `Span`
/// keys collide across modules (and across stdlib files): two different
/// nodes at the same offsets would overwrite each other in
/// `span_types`, and native codegen would lower one with the other's
/// type (e.g. a string accumulator lowered as `bool`, printing "false").
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SpanKey {
    /// Enclosing top-level function (`Type.method` for methods,
    /// [`TOP_SCOPE`] otherwise). Shared (`Arc`, not `String`): one key is
    /// built per checked expression, and (atomic) refcounting kills an allocation
    /// per node with zero hashing cost change.
    pub func: Arc<str>,
    pub span: Span,
}

impl SpanKey {
    pub fn new(func: impl Into<Arc<str>>, span: Span) -> Self {
        SpanKey {
            func: func.into(),
            span,
        }
    }
}

/// Manual serde for SpanKey: `Arc<str>` is not serde-serializable, so we
/// round-trip through `String`.
impl serde::Serialize for SpanKey {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        (self.func.as_ref(), self.span).serialize(serializer)
    }
}

impl<'de> serde::Deserialize<'de> for SpanKey {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let (func, span) = <(String, Span)>::deserialize(deserializer)?;
        Ok(SpanKey {
            func: func.into(),
            span,
        })
    }
}

/// A registered function signature.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct FuncSig {
    pub generics: Vec<String>,
    /// Trait bounds per generic parameter name (e.g. `T` → `[Num, Ord]`).
    pub bounds: Vec<(String, Vec<zz_span_ast::ast::TraitBound>)>,
    pub params: Vec<(String, Type)>,
    pub has_default: Vec<bool>,
    pub ret: Type,
    /// True for `extern "C"` declarations — no ZZ body, linked natively.
    pub is_extern: bool,
    /// True for functions that never retain their arguments — arguments
    /// passed to a `readonly` call do not escape through the callee, so
    /// they can stay arena-allocated instead of being forced to ARC heap.
    /// Set by the `#[readonly]` / `#[pure]` decorator on function declarations.
    pub readonly: bool,
    /// Explicit C symbol override from the manifest (`= "..."`). `None`
    /// means "derive from the ZZ-visible name" via [`FuncSig::c_symbol`].
    pub extern_c_symbol: Option<String>,
}

impl FuncSig {
    /// Resolve the underlying C symbol for an extern signature keyed by
    /// its ZZ-visible name: the explicit override when present, otherwise
    /// the ZZ name with `.` replaced by `_` (`zimg.resize` → `zimg_resize`,
    /// flat `add` → `add`).
    pub fn c_symbol(&self, zz_name: &str) -> String {
        self.extern_c_symbol
            .clone()
            .unwrap_or_else(|| zz_name.replace('.', "_"))
    }
}

/// A registered struct definition: type parameters and field types
/// (which may reference the parameters as `Type::Named`).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct StructSig {
    pub generics: Vec<String>,
    pub fields: Vec<(String, Type)>,
}

/// A registered type alias: type parameters and the target type with
/// parameters as `Type::Named` (e.g. `type Pair<T> = (T, T)` stores
/// `Tuple([Named("T"), Named("T")])`). Uses resolve to the target, so
/// aliases never appear at runtime.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct AliasSig {
    pub generics: Vec<String>,
    pub target: Type,
}
/// A registered user enum: type parameters and variant names with
/// optional payload types (`enum Box<T> { V(T), E }` stores
/// `generics: ["T"]`, `variants: [("V", Some(Named("T"))),
/// ("E", None)]`). Construction (`Box.V(1)`) and patterns (`.V(v)`)
/// resolve against this table; values erase to `Object`s at runtime.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct EnumSig {
    pub generics: Vec<String>,
    pub variants: Vec<(String, Option<Type>)>,
}
/// Minimal error-conversion registration (V1, error-only — not a general trait system).
/// Declared via `impl From { func convert_to_To(self) -> To { ... } }`.
/// V1 rule: at most one conversion per source type (keeps runtime dispatch sound).
#[derive(Debug, Clone)]
pub struct ConvertImpl {
    pub from: Type,
    pub from_name: String,
    pub to: Type,
    pub to_name: String,
    pub func_name: String,
    pub span: Span,
}

/// A `:=` declaration whose initializer is being checked, while probing
/// whether it reads the binding the declaration shadows (#518).
#[derive(Debug, Clone)]
pub struct ShadowProbe {
    /// Declared name.
    pub name: String,
    /// Where the declaration binds (`x` in `x := …`) — the warning points here.
    pub span: Span,
    /// Index of the scope the declaration binds into. A read resolving to a
    /// *shallower* scope hit the shadowed outer binding; one at the same or
    /// deeper index is a binder inside the initializer (closure parameter,
    /// nested block), which is not shadowing.
    pub depth: usize,
    /// Set once such an outer read is seen.
    pub referenced: bool,
}

#[derive(Debug, Clone)]
pub struct CheckResult {
    pub errors: Vec<RawDiag>,
    /// Top-level `let` bindings and their types (fully resolved).
    pub bindings: HashMap<String, Type>,
    /// Top-level function signatures.
    pub funcs: HashMap<String, FuncSig>,
    /// Top-level struct definitions.
    pub structs: HashMap<String, StructSig>,
    /// Top-level type aliases (`type Tokens = [Token]`), resolved targets.
    pub aliases: HashMap<String, AliasSig>,
    /// Top-level user enums (`enum Token { ... }`).
    pub enums: HashMap<String, EnumSig>,
    /// `try` site span → conversion impl span (`None` = identity).
    pub try_resolutions: HashMap<Span, Option<Span>>,
    /// `try` site span → conversion function name (`None` = identity).
    /// Resolved from `try_resolutions` + the `convert_to_` registry so
    /// downstream passes (HIR, native codegen) can emit the conversion
    /// call without re-deriving it. Mirrors `try_resolutions` 1:1.
    pub try_converts: HashMap<Span, Option<String>>,
    /// Native libraries requested via `@link("lib")`, in source order, deduped.
    pub link_libs: Vec<String>,
    /// Top-level `const` bindings and their declaration spans. Used to seed
    /// later REPL snippets so immutable names stay immutable across evals.
    pub const_bindings: HashMap<String, Span>,
    /// Only `pub` bindings (for cross-module export).
    pub pub_bindings: HashMap<String, Type>,
    /// Only `pub` functions (for cross-module export).
    pub pub_funcs: HashMap<String, FuncSig>,
    /// Only `pub` structs (for cross-module export).
    pub pub_structs: HashMap<String, StructSig>,
    /// Only `pub` type aliases (for cross-module export).
    pub pub_aliases: HashMap<String, AliasSig>,
    /// Only `pub` user enums (for cross-module export).
    pub pub_enums: HashMap<String, EnumSig>,
}

/// One binding recorded for unused-variable warnings.
///
/// `warn_unused` is `false` for bindings whose existence is dictated by
/// an external signature rather than by the author's choice — closure
/// parameters, which the callee calls positionally and which cannot be
/// removed without changing the closure's arity.
#[derive(Clone, Copy, Debug)]
pub struct DefinedBinding {
    pub span: zz_span_ast::span::Span,
    pub warn_unused: bool,
}

/// Type-erased storage for state owned by checker *domains*.
///
/// Domain crates (route lints today, others later) need to keep state across
/// the expression walk, but adding a field per domain made this crate depend
/// on every domain crate — a cycle risk and a compile-time tax on anything
/// that only wants the type tables. Domains store their state here instead
/// and fetch it back by type; `zz_checker_core` stays dependent on types,
/// spans and diagnostics only.
#[derive(Default)]
pub struct DomainState {
    entries: HashMap<TypeId, Box<dyn Any>>,
}

impl DomainState {
    /// Mutable state for `T`, created with [`Default`] on first use.
    ///
    /// The `TypeId` key is derived from the concrete type, so the downcast
    /// below cannot fail in practice; it still returns `expect` rather than
    /// `unwrap` to make a (theoretical) collision a clear panic.
    pub fn get_or_default<T: Any + Default>(&mut self) -> &mut T {
        self.entries
            .entry(TypeId::of::<T>())
            .or_insert_with(|| Box::<T>::default())
            .downcast_mut::<T>()
            .expect("DomainState entries are keyed by TypeId")
    }

    /// Shared state for `T`, if the owning domain has stored any.
    pub fn get<T: Any>(&self) -> Option<&T> {
        self.entries.get(&TypeId::of::<T>())?.downcast_ref::<T>()
    }

    /// Number of domains that have stored state (diagnostics/tests).
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// True when no domain has stored state.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

impl std::fmt::Debug for DomainState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // The boxes are opaque by construction; exposing the type count
        // keeps `Checker` readable without unsafe downcasts.
        f.debug_struct("DomainState")
            .field("domains", &self.entries.len())
            .finish()
    }
}

pub struct Checker {
    pub unifier: Unifier,
    pub errors: Vec<zz_diag_core::RawDiag>,
    pub funcs: HashMap<String, FuncSig>,
    pub structs: HashMap<String, StructSig>,
    /// Type aliases (`type Tokens = [Token]`): exported, resolved targets.
    pub aliases: HashMap<String, AliasSig>,
    /// Raw alias targets (this program only), converted to `aliases`
    /// after all structs are collected so targets may reference any
    /// struct regardless of order. Not exported.
    pub alias_asts: HashMap<String, (Vec<String>, Ty)>,
    /// Alias expansion stack for cycle detection (`type A = B`,
    /// `type B = A` reports instead of recursing forever).
    pub alias_expanding: Vec<String>,
    /// User enums (`enum Token { ... }`): exported, resolved payloads.
    pub enums: HashMap<String, EnumSig>,
    /// Extension methods (separate table): (TypeName, method) → (sig, def span).
    /// Holds `impl` on builtins and cross-type extensions. Lookup priority:
    /// inherent (`funcs`) → extension (here) → stdlib namespace.
    pub ext_methods: HashMap<(String, String), (FuncSig, Span)>,
    /// Minimal `convert_to_` registry for `try` error conversion.
    pub convert_impls: Vec<ConvertImpl>,
    /// `try` site span → convert impl span (`None` = identity, no conversion).
    /// Consumed by LSP hover to show the resolved conversion.
    pub try_resolutions: HashMap<Span, Option<Span>>,
    pub env: Vec<HashMap<String, Type>>,
    /// Names declared `const` in each scope, mapped to their declaration
    /// span (for the "defined as immutable here" secondary label). Parallel
    /// to `env` — a name is immutable iff the scope that binds it also
    /// contains it here.
    pub const_env: Vec<HashMap<String, zz_span_ast::span::Span>>,
    /// Top-level let bindings discovered this run: name → type.
    pub new_bindings: HashMap<String, Type>,
    pub current_ret: Option<Type>,
    pub current_generics: Vec<String>,
    /// Trait bounds for the generic parameters currently in scope (set while
    /// checking a generic function body).
    pub current_bounds: HashMap<String, Vec<zz_span_ast::ast::TraitBound>>,
    /// Nesting depth of `for`/`while` loops (for `break`/`continue`).
    pub loop_depth: usize,
    /// Names that were used (looked up) — for unused-import warnings and
    /// module/type usage. Binding-level usage lives in `used_bindings`.
    pub used_names: std::collections::HashSet<String>,
    /// Bindings actually read, keyed by `(scope index, name)`.
    ///
    /// Name-only tracking was too coarse: an unused `x` in one function was
    /// silenced whenever any other function read a *different* `x`, and a
    /// shadowed binding inherited its shadow's reads. The scope index (the
    /// position in `env`) is stable for a scope's lifetime, so a use marked
    /// while the scope is live is seen by that scope's `pop_scope`.
    pub used_bindings: std::collections::HashSet<(usize, String)>,
    /// Declarations whose initializer is being checked right now, probing
    /// whether it reads the very binding the declaration shadows from an
    /// enclosing scope (#518). Innermost last — see `begin_shadow_probe`.
    pub shadow_probes: Vec<ShadowProbe>,
    /// Top-level names marked `pub` — should not emit unused warnings.
    pub pub_names: std::collections::HashSet<String>,
    /// Names defined in the current scope with their spans — for unused
    /// variable warnings. Each scope level has its own map.
    pub defined_names: Vec<HashMap<String, DefinedBinding>>,
    /// Tracks whether the most recent `lookup()` produced an "undefined
    /// variable" error.  Used by `check_call` to suppress the secondary
    /// "cannot call a value of type unit" cascading error.
    pub had_undefined_var: bool,
    /// Whether a block tail in the current position is the block's
    /// *value* (implicit return) rather than a discard. `false` at the
    /// top level and inside loop bodies (their values are discarded),
    /// `true` inside function/closure bodies. `check_block` ANDs this
    /// with tail position, so `-> Result` tails never warn while loop
    /// tails that drop errors do. Saved/restored across nested blocks.
    pub tail_is_value: bool,
    /// Span and resolved type of the last statement of the most recently
    /// checked block. `check_block` sets it *after* any nested block has
    /// been checked, so after a named function body it holds that body's
    /// tail. The missing-return diagnostic uses it to avoid an insertion
    /// fixit before a tail that yields `unit`.
    pub last_block_tail: Option<(Span, Type)>,
    /// Whether the statement currently being checked directly discards
    /// a `match` value (`match ...` as a bare statement/defer, not the
    /// tail). `check_match` uses this to warn on bare `Result`/`Option`
    /// arm bodies, which never pass through `Stmt::Expr` themselves.
    /// Block arm bodies are covered by the normal statement path.
    pub direct_discard: bool,
    /// Imported namespaces: (alias, span). Used to detect unused imports.
    pub imports: Vec<(String, zz_span_ast::span::Span)>,
    /// Selective-import aliases: bare name → qualified `ns.sym`, from
    /// `import m(x)` / `import m(x as y)` statements (kept intact by the
    /// loader). Used ONLY on total miss: locals, seed entries and value
    /// Decls all take precedence. This is how bare calls to *generic*
    /// functions resolve — generics have no value type, so no binding
    /// is ever created for them.
    pub import_aliases: HashMap<String, String>,
    /// Module-head aliases: `m` → `std.math` from `import std.math as m`.
    /// Lets const/call diagnostics resolve aliased namespaces.
    pub module_aliases: HashMap<String, String>,
    /// Resolved type per scoped expression key, recorded during the type walk.
    /// Used by the HIR builder to attach a resolved `Type` to every AST node.
    /// Keyed by [`SpanKey`] (function + span): bare spans collide across
    /// modules since every file restarts offsets at 0.
    pub span_types: std::collections::HashMap<SpanKey, Type>,
    /// Native libraries requested via `@link`, in source order, deduped.
    pub link_libs: Vec<String>,
    /// Per-domain state (route lints, future lints), keyed by type.
    pub domain: DomainState,
    /// Enclosing-item scope stack for [`SpanKey`] recording. Holds the
    /// top-level function (or `Type.method`) whose body is being checked;
    /// empty at top level (keys then use [`TOP_SCOPE`]).
    pub scope: Vec<Arc<str>>,
}

impl Checker {
    /// Scope-qualified key for an expression span under the item currently
    /// being checked.
    pub fn scope_key(&self, span: Span) -> SpanKey {
        SpanKey::new(
            self.scope
                .last()
                .cloned()
                .unwrap_or_else(|| Arc::from(TOP_SCOPE)),
            span,
        )
    }
    pub fn new(
        initial_bindings: HashMap<String, Type>,
        funcs: HashMap<String, FuncSig>,
        structs: HashMap<String, StructSig>,
        aliases: HashMap<String, AliasSig>,
        enums: HashMap<String, EnumSig>,
        initial_consts: HashMap<String, Span>,
    ) -> Self {
        let env = vec![initial_bindings];
        Checker {
            unifier: Unifier::new(),
            errors: Vec::new(),
            funcs,
            structs,
            aliases,
            alias_asts: HashMap::new(),
            alias_expanding: Vec::new(),
            enums,
            ext_methods: HashMap::new(),
            convert_impls: Vec::new(),
            try_resolutions: HashMap::new(),
            env,
            const_env: vec![initial_consts],
            new_bindings: HashMap::new(),
            current_ret: None,
            current_generics: Vec::new(),
            current_bounds: HashMap::new(),
            loop_depth: 0,
            used_names: std::collections::HashSet::new(),
            used_bindings: std::collections::HashSet::new(),
            shadow_probes: Vec::new(),
            pub_names: std::collections::HashSet::new(),
            defined_names: vec![HashMap::new()],
            had_undefined_var: false,
            tail_is_value: false,
            last_block_tail: None,
            direct_discard: false,
            imports: Vec::new(),
            import_aliases: HashMap::new(),
            module_aliases: HashMap::new(),
            span_types: std::collections::HashMap::new(),
            scope: Vec::new(),
            link_libs: Vec::new(),
            domain: DomainState::default(),
        }
    }

    /// Get the full name of a function statement.
    pub fn func_name(stmt: &Stmt) -> String {
        match stmt {
            Stmt::Func { name, .. } => name.join("."),
            _ => "<unknown>".to_string(),
        }
    }

    /// Canonical extension key for builtin receiver names.
    /// `str`→`str`, `Array`/`vec`→`vec`, `Option`/`option`→`option`,
    /// `Result`/`result`→`result`, scalars map to themselves.
    pub fn builtin_ext_key(type_name: &str) -> Option<String> {
        match type_name {
            "str" => Some("str".to_string()),
            "int" => Some("int".to_string()),
            "float" => Some("float".to_string()),
            "bool" => Some("bool".to_string()),
            "vec" | "Array" => Some("vec".to_string()),
            "option" | "Option" => Some("option".to_string()),
            "result" | "Result" => Some("result".to_string()),
            _ => None,
        }
    }

    /// Receiver type for `self` in an `impl` block.
    pub fn self_type_for_impl(
        type_name: &str,
        type_key: &str,
        impl_generics: &[String],
        unifier: &mut Unifier,
    ) -> Type {
        match type_key {
            "str" => Type::Str,
            "int" => Type::Int,
            "float" => Type::Float,
            "bool" => Type::Bool,
            "vec" => Type::Array(Box::new(unifier.fresh_var())),
            "option" => Type::Option(Box::new(unifier.fresh_var())),
            "result" => Type::Result(Box::new(unifier.fresh_var()), Box::new(unifier.fresh_var())),
            _ => Type::Struct(
                type_name.to_string(),
                impl_generics
                    .iter()
                    .map(|g| Type::Named(g.clone()))
                    .collect(),
            ),
        }
    }

    /// True when `Type.method` is a stdlib-provided builtin (builtin-wins).
    /// The namespace list mirrors `STDLIB_MODULES` in `zz_stdlib/src/lib.rs`
    /// plus the method-dispatch namespaces used by `check_call`
    /// (`option`/`result`/`net`/`db`) — i.e. every namespace whose `ns.method`
    /// keys can arrive via the seeded `funcs` map. Collision *coverage* does
    /// not depend on this list (any seeded `funcs` key collides); this only
    /// decides the message wording (stdlib vs another module).
    pub fn is_stdlib_method(full_name: &str) -> bool {
        if let Some((ns, _)) = full_name.rsplit_once('.') {
            // Strip a leading `std.` (`std.str.length` → `str`) and compare
            // the first segment (`sqlz.postgres.query` → `sqlz`).
            let short = ns.strip_prefix("std.").unwrap_or(ns);
            let head = short.split('.').next().unwrap_or(short);
            let head = head.to_lowercase();
            matches!(
                head.as_str(),
                "io" | "str"
                    | "vec"
                    | "json"
                    | "http"
                    | "fs"
                    | "env"
                    | "math"
                    | "time"
                    | "encoding"
                    | "net"
                    | "chan"
                    | "task"
                    | "regexp"
                    | "regex"
                    | "crypto"
                    | "log"
                    | "sys"
                    | "args"
                    | "process"
                    | "uuid"
                    | "sqlz"
                    | "option"
                    | "result"
            )
        } else {
            false
        }
    }
}

impl Checker {
    // --- errors -----------------------------------------------------------

    pub fn report_mismatch(&mut self, err: UnifyError, span: Span) {
        self.report_mismatch_with_notes(err, span, Vec::new());
    }

    /// `report_mismatch` plus caller-supplied Sherlock notes. Call sites
    /// that know the expected shape (parameter names, annotations) use
    /// this so hints stay typed — never string-parsed.
    pub fn report_mismatch_with_notes(&mut self, err: UnifyError, span: Span, notes: Vec<String>) {
        let msg = match err.message.as_str() {
            "type mismatch" => {
                format!(
                    "type mismatch: expected `{}`, found `{}`",
                    err.right, err.left
                )
            }
            "function arity mismatch" => "function arity mismatch".to_string(),
            "tuple arity mismatch" => "tuple arity mismatch".to_string(),
            other => other.to_string(),
        };
        let mut diag = error_at(msg, span);
        for note in notes {
            diag = diag.with_note(note);
        }
        self.errors.push(diag);
    }

    /// Sherlock: returning an unwrapped `Result` where a plain value is
    /// expected (or vice versa) is the most common ZZ beginner error.
    /// Point at the value with the three actionable fixes (#245).
    /// Returns true when the hint was emitted (caller skips the generic
    /// return-type-span error to avoid a double report).
    pub fn report_result_return_hint(
        &mut self,
        value_ty: &Type,
        ret_ty: &Type,
        span: Span,
    ) -> bool {
        let v = self.unifier.resolve(value_ty);
        let r = self.unifier.resolve(ret_ty);
        // Unwrapped Result where a plain value is expected.
        if let Type::Result(inner, err) = &v {
            let payload = self.unifier.resolve(inner);
            if payload == r {
                let mut diag = error_at(
                    format!(
                        "returning unwrapped `Result<{payload}, {err}>` where `{r}` is expected"
                    ),
                    span,
                );
                diag = diag.with_note(format!(
                    "use `?` to propagate (needs `-> Result<{payload}, {err}>`)"
                ));
                diag = diag.with_note("or `match` on the value".to_string());
                diag = diag.with_note(format!(
                    "or change the return type to `Result<{payload}, {err}>`"
                ));
                self.errors.push(diag);
                return true;
            }
        }
        // Plain value where a Result is expected.
        if let Type::Result(inner, err) = &r {
            let payload = self.unifier.resolve(inner);
            if payload == v {
                let mut diag = error_at(
                    format!("returning plain `{v}` where `Result<{payload}, {err}>` is expected"),
                    span,
                );
                diag = diag.with_note("wrap the value: `.ok(value)`".to_string());
                diag = diag.with_note("or propagate a fallible call with `?`".to_string());
                self.errors.push(diag);
                return true;
            }
        }
        false
    }

    pub fn ensure_bool(&mut self, t: Type, span: Span, glob_hint: Option<&str>) {
        match self.unifier.resolve(&t) {
            Type::Bool => {}
            Type::Var(id) => {
                self.unifier.bind(id, Type::Bool);
            }
            // Divergent condition: unreachable, vacuously fine.
            Type::Never => {}
            other => {
                let mut diag = error_at(format!("expected `bool`, found `{other}`"), span);
                // A `str` that looks like a GLOB/pattern is a common mistake
                // where the author meant a truthy check, not a boolean.
                if matches!(other, Type::Str) {
                    if let Some(s) = glob_hint {
                        if Self::looks_like_glob(s) {
                            diag = diag.with_note(format!(
                                "expected a boolean condition; `{}` looks like a GLOB/pattern string \
                                 — use a truthy check (e.g. `{}` != \"\") or pass a `bool`",
                                s, s
                            ));
                        }
                    }
                }
                self.errors.push(diag);
            }
        }
    }

    /// True when `s` contains characters typical of a GLOB/pattern:
    /// `*`, `?`, `[`, `]`, `{`, `}`.
    pub fn looks_like_glob(s: &str) -> bool {
        s.chars()
            .any(|c| matches!(c, '*' | '?' | '[' | ']' | '{' | '}'))
    }

    pub fn ensure_int(&mut self, t: Type, span: Span) {
        match self.unifier.resolve(&t) {
            Type::Int => {}
            Type::Var(id) => {
                self.unifier.bind(id, Type::Int);
            }
            // Divergent index: unreachable, vacuously fine.
            Type::Never => {}
            // Cascade root already reported: an index on an `<error>` value
            // must not spawn a new "index must be `int`" diagnostic (#711).
            Type::Error => {}
            other => {
                self.errors.push(error_at(
                    format!("index must be `int`, found `{other}`"),
                    span,
                ));
            }
        }
    }
}

/// Canonical std module list (dotted; nested modules included).
/// SINGLE SOURCE for all module-name tables: `zz_stdlib::STDLIB_MODULES`
/// re-exports this, and the helpers below derive from it — there is no
/// second list to drift. Method-only namespaces (`option`, `result`,
/// `file`, …) and type constructors (`ArgsParser`, `Regexp`) are absent
/// on purpose — they were never duplicated and stay bare.
pub const STD_MODULES: &[&str] = &[
    "str",
    "vec",
    "json",
    "http",
    "fs",
    "path",
    "env",
    "math",
    "time",
    "encoding",
    "net",
    "chan",
    "task",
    "regexp",
    "crypto",
    "log",
    "sys",
    "args",
    "process",
    "uuid",
    "sqlz",
    "sqlz.postgres",
    "sqlz.mysql",
    "colors",
    "term",
    "test",
    "map",
    "set",
    "dec",
    "bytes",
    "csv",
    "property",
    "cell",
    "unix",
    "tls",
    "tls_listener",
    "unix_listener",
];

/// True when `head` is a top-level std module (`sqlz` in
/// `sqlz.postgres.query`). Derives from [`STD_MODULES`] — no parallel
/// list. Used by import suggestions and canonical resolution alike.
pub fn is_std_module_head(head: &str) -> bool {
    STD_MODULES
        .iter()
        .any(|m| m.split('.').next() == Some(head))
}

/// Look up a stdlib registry entry by either spelling: the name as-written
/// first (covers orphans like `option.unwrap` and import-copied `ns.*`
/// keys, which have no canonical form), then the canonical `std.*`
/// spelling. Single choke point for the canonical-seeds-only tables —
/// callers must use this instead of raw `HashMap::get` for stdlib names.
pub fn lookup_std<'a, V>(
    map: &'a std::collections::HashMap<String, V>,
    name: &str,
) -> Option<&'a V> {
    map.get(name)
        .or_else(|| canonical_std_name(name).and_then(|c| map.get(&c)))
}

/// Membership twin of [`lookup_std`] for precomputed name sets
/// (`native_names`, seed-key sets, completion snapshots).
pub fn contains_std(set: &std::collections::HashSet<String>, name: &str) -> bool {
    set.contains(name) || canonical_std_name(name).is_some_and(|c| set.contains(&c))
}

/// Resolve a bare `module.func` name to its canonical `std.module.func`
/// spelling: the single source of truth for the std alias (replaces the
/// old per-crate inline copies). Returns `None` for already-canonical
/// `std.*` names, bare builtins, unknown heads, and non-module
/// namespaces. Nested modules compose (`sqlz.postgres.query` →
/// `std.sqlz.postgres.query`). Callers gate on module position (a local
/// binding with the same head always wins) before applying this.
pub fn canonical_std_name(name: &str) -> Option<String> {
    if name.starts_with("std.") || !name.contains('.') {
        return None;
    }
    let head = name.split('.').next().unwrap_or(name);
    if is_std_module_head(head) {
        return Some(format!("std.{name}"));
    }
    None
}
