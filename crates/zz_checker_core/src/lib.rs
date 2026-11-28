//! Shared type-checker state.
//!
//! [`Checker`] owns every table and cursor the six-pass checker walks
//! (signatures, scopes, diagnostics, span types). Behavior lives in
//! per-domain crates that implement extension traits for it; this crate
//! defines the state, its constructors, and the low-level helpers several
//! domains share.

use std::any::{Any, TypeId};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use zz_arena::Interner;
use zz_diag_core::{error_at, ErrorCode, RawDiag, SecondaryLabel};
use zz_span_ast::ast::{Stmt, Ty};
use zz_span_ast::span::Span;
use zz_types::Type;
use zz_unify::{Unifier, UnifyError};

/// Usage context for type inference hints: tracks how unbound variables
/// are used (arithmetic, comparison, index, etc.) to suggest type names
/// when inference fails.
#[derive(Debug, Clone, Default)]
pub struct UsageContext {
    /// Used with arithmetic operators (+, -, *, /, %)
    pub used_arithmetic: bool,
    /// Used with comparison operators (==, !=, <, >, <=, >=)
    pub used_comparison: bool,
    /// Used as array/tuple index
    pub used_as_index: bool,
    /// Used with string methods (.len(), .trim(), etc.)
    pub used_as_string: bool,
    /// Unified with a concrete type
    pub unified_with: Option<Type>,
}

/// How a variable name was used, for type inference hints.
#[derive(Debug, Clone)]
pub enum UsageUsage {
    /// Used with arithmetic operators (+, -, *, /, %)
    Arithmetic,
    /// Used with comparison operators (==, !=, <, >, <=, >=)
    Comparison,
    /// Used as array/tuple index
    Index,
    /// Used with string methods (.len(), .trim(), etc.)
    String,
    /// Unified with a concrete type
    UnifiedWith(Type),
}

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
    /// [`TOP_SCOPE`] otherwise). Stored as an interned `Symbol` (u32)
    /// rather than `Arc<str>` so all keys for the same function share one
    /// allocation — reduces per-key size from 24 bytes to 8 bytes and
    /// kills millions of small heap allocations on large modules.
    pub func: zz_arena::Symbol,
    pub span: Span,
}

impl SpanKey {
    pub fn new(func: zz_arena::Symbol, span: Span) -> Self {
        SpanKey { func, span }
    }
}

/// Manual serde for SpanKey: `Symbol` is not serde-serializable by itself,
/// so we round-trip through a `String` key (`id@start..end`). The symbol
/// table is carried separately in [`CachedModule`] so Symbol ids can be
/// reconstructed on deserialize. The `id` is the raw u32 from
/// [`zz_arena::Symbol`].
impl serde::Serialize for SpanKey {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        format!("{}@{}..{}", self.func.id(), self.span.start, self.span.end).serialize(serializer)
    }
}

impl<'de> serde::Deserialize<'de> for SpanKey {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let s = String::deserialize(deserializer)?;
        let (id_str, rest) = s
            .split_once('@')
            .ok_or_else(|| serde::de::Error::custom("invalid SpanKey: missing @"))?;
        let (start_str, end_str) = rest
            .split_once("..")
            .ok_or_else(|| serde::de::Error::custom("invalid SpanKey: missing .."))?;
        let id: u32 = id_str.parse().map_err(serde::de::Error::custom)?;
        let start: u32 = start_str.parse().map_err(serde::de::Error::custom)?;
        let end: u32 = end_str.parse().map_err(serde::de::Error::custom)?;
        Ok(SpanKey {
            func: zz_arena::Symbol(id),
            span: Span::new(start, end),
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

/// Placeholder receiver type for a trait method declaration's `self`.
///
/// Trait signatures are stored once, at declaration time, before any
/// implementor is known, so `self` is `Type::Named(SELF_TYPE_PARAM)`. The
/// resolver substitutes the receiver's real type at every use site. Trait
/// method signatures never take generics in v1, so the name cannot collide
/// with a user parameter.
pub const SELF_TYPE_PARAM: &str = "Self";

/// A user `trait` declaration: named, signature-only methods.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct TraitSig {
    /// Method name → signature. `params[0]` is the receiver, typed
    /// [`SELF_TYPE_PARAM`]; later params and the return type may mention
    /// it too. Deterministic codegen sorts keys.
    pub methods: HashMap<String, FuncSig>,
    /// Optional deprecated span. If set, using this trait as a bound
    /// emits a deprecation warning.
    pub deprecated: Option<Span>,
}

impl TraitSig {
    /// Method names in stable (sorted) order — codegen and diagnostics.
    pub fn method_names(&self) -> Vec<&String> {
        let mut names: Vec<&String> = self.methods.keys().collect();
        names.sort();
        names
    }
}

/// Canonical registry key for the concrete type a bound is checked
/// against: the plain name for structs/enums, the builtin alias
/// (`str`, `int`, `float`, `bool`, `vec`, `option`, `result`) for
/// builtins, and `None` for types that cannot implement a trait
/// (unions, functions, opaque handles, inference variables).
///
/// Shared by bound checking and codegen so they cannot drift.
pub fn trait_type_key(ty: &Type) -> Option<String> {
    match ty {
        Type::Struct(name, _) | Type::Enum(name, _) => Some(name.clone()),
        Type::Int => Some("int".to_string()),
        Type::Float => Some("float".to_string()),
        Type::Bool => Some("bool".to_string()),
        Type::Str => Some("str".to_string()),
        Type::Array(_) => Some("vec".to_string()),
        Type::Option(_) => Some("option".to_string()),
        Type::Result(_, _) => Some("result".to_string()),
        _ => None,
    }
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
    /// Top-level user trait declarations.
    pub traits: HashMap<String, TraitSig>,
    /// Trait name → implementing type keys (all impls in this program).
    pub trait_impls: HashMap<String, Vec<String>>,
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
    /// Only `pub` trait declarations (for cross-module export).
    pub pub_traits: HashMap<String, TraitSig>,
    /// Reverse map: func name → Symbol id. Enables external consumers
    /// (HIR, runtime) to resolve a func name to its interned `Symbol` for
    /// [`SpanKey`] construction.
    pub symbol_map: HashMap<String, u32>,
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
    /// User trait declarations (`trait Draw { ... }`): name → signatures.
    pub traits: HashMap<String, TraitSig>,
    /// Trait implementations: trait name → implementing type keys (see
    /// [`trait_type_key`]). Bound checking consults this; codegen reads it
    /// to build method dispatch tables. Appended in declaration order and
    /// deduplicated by the checker.
    pub trait_impls: HashMap<String, Vec<String>>,
    /// Extension methods (separate table): (TypeName, method) → (sig, def span).
    /// Holds `impl` on builtins and cross-type extensions. Lookup priority:
    /// inherent (`funcs`) → extension (here) → stdlib namespace.
    pub ext_methods: HashMap<(String, String), (FuncSig, Span)>,
    /// Minimal `convert_to_` registry for `try` error conversion.
    pub convert_impls: HashMap<String, ConvertImpl>,
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
    /// Top-level declarations whose type could not be inferred during
    /// declaration checking because a callee's return-type variable was
    /// still unresolved. Re-checked after all function bodies are checked;
    /// an error is emitted only if the type is still unresolved then.
    pub deferred_type_errors: Vec<(String, Span)>,
    /// Top-level names marked `pub` — should not emit unused warnings.
    pub pub_names: std::collections::HashSet<String>,
    /// Functions annotated with `@test`, `@bench`, `@setup`, or `@teardown`.
    /// These are invoked by the test/bench harness at runtime, not by direct
    /// calls in source, so they must not trigger unused-func warnings.
    pub test_funcs: std::collections::HashSet<String>,
    /// Functions defined in the current program (not stdlib-seeded).
    /// Used to avoid warning on unused stdlib functions that happen to
    /// not appear in `used_names`.
    pub local_funcs: std::collections::HashSet<String>,
    /// Structs defined in the current program (not import-seeded).
    /// Used to scope unused-field warnings to locally-owned structs;
    /// imported pub structs may be used externally.
    pub local_structs: std::collections::BTreeSet<String>,
    /// Enums defined in the current program (not import-seeded).
    /// Used to scope unused-enum warnings to locally-owned enums.
    pub local_enums: std::collections::BTreeSet<String>,
    /// Traits defined in the current program (not import-seeded).
    /// Used to scope unused-trait warnings to locally-owned traits.
    pub local_traits: std::collections::BTreeSet<String>,
    /// Type aliases defined in the current program (not import-seeded).
    /// Used to scope unused-alias warnings to locally-owned aliases.
    pub local_aliases: std::collections::HashSet<String>,
    /// Const declarations defined in the current program (not import-seeded).
    /// Used to scope unused-const warnings to locally-owned consts.
    pub local_consts: std::collections::HashSet<String>,
    /// Struct names referenced (used as types) — for unused-struct warnings.
    pub used_struct_names: std::collections::HashSet<String>,
    /// Enum names referenced (used as types) — for unused-enum warnings.
    pub used_enum_names: std::collections::HashSet<String>,
    /// Trait names referenced (used as bounds) — for unused-trait warnings.
    pub used_trait_names: std::collections::HashSet<String>,
    /// Const names read — for unused-const warnings.
    pub used_const_names: std::collections::HashSet<String>,
    /// Impl methods declared: (type_name, method_name) → span of the `fn` keyword.
    /// Used to detect unused impl methods.
    pub used_impl_methods: std::collections::HashMap<(String, String), Span>,
    /// Usage context for type inference hints: tracks how unbound variables
    /// are used (arithmetic, comparison, index, etc.) to suggest type names
    /// when inference fails.
    pub usage_context: HashMap<String, UsageContext>,
    /// Enums defined in the current program (not import-seeded).
    /// Used to scope unused-enum warnings to locally-owned enums.
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
    /// Extern `extern "C"` symbols declared this run: C symbol → defining span.
    /// Used to detect duplicate extern symbol declarations at check time so
    /// they surface as `zz check` errors instead of linker errors. Full
    /// symbol-existence verification (does the symbol actually exist in a
    /// linked library) requires the C toolchain and is intentionally a link-
    /// time concern, not a check-time one — emitting symbol-not-found errors
    /// here would risk false positives in environments without the library.
    pub extern_symbols: std::collections::HashMap<String, Span>,
    /// Per-domain state (route lints, future lints), keyed by type.
    pub domain: DomainState,
    /// Enclosing-item scope stack for [`SpanKey`] recording. Holds the
    /// top-level function (or `Type.method`) whose body is being checked;
    /// empty at top level (keys then use [`TOP_SCOPE`]).
    pub scope: Vec<Arc<str>>,
    /// Interns the `func` string of every [`SpanKey`] so all keys for the
    /// same enclosing function share one heap allocation instead of one
    /// `Arc<str>` per checked expression. Reduces checker peak RSS on
    /// large modules (Phase 1 of the checker improvement plan).
    pub func_interner: Interner,
    /// Reverse map: func name → Symbol id. Populated by [`scope_key`] so
    /// external consumers (HIR, runtime) can resolve a func name to its
    /// interned `Symbol` for [`SpanKey`] construction.
    pub symbol_map: std::collections::HashMap<String, u32>,
    /// Flow-sensitive type narrowing: one map per scope depth, parallel to
    /// [`env`]. Holds refined types for names narrowed by `if x is T`
    /// (then-branch) and match-arm bindings. `lookup_opt` consults the map
    /// for the current depth before falling back to the scope walk, so a
    /// narrowed type is visible only inside the scope that introduced it.
    pub narrowing: Vec<std::collections::HashMap<String, Type>>,
    /// Used enum variants: `(enum_name, variant_name)` pairs that are
    /// constructed or pattern-matched. Used to detect dead variants.
    pub used_enum_variants: std::collections::HashSet<(String, String)>,
    /// Used struct fields: `(struct_name, field_name)` pairs that are
    /// read via field access. Used to detect dead fields.
    pub used_struct_fields: std::collections::HashSet<(String, String)>,
    /// Used type aliases: names resolved in type positions. Used to
    /// detect dead aliases.
    pub used_aliases: std::collections::HashSet<String>,
    /// Shadow cache: innermost binding per name → (scope depth, type).
    /// Makes variable lookup O(1) amortized instead of O(depth). Entries are
    /// inserted on `define` and evicted for the popped depth on `pop_scope`;
    /// shadowed outer values are restored lazily by a single linear re-walk
    /// on the next lookup, keeping correctness for shadowing.
    pub scope_cache: std::collections::HashMap<String, (usize, Type)>,
    /// Names defined at each scope depth, parallel to `env`; used to evict
    /// exactly the cache entries belonging to a popped scope.
    pub names_at_depth: Vec<Vec<String>>,
    /// Memo cache for `Ty -> Type` conversion, keyed by
    /// `(span.start, span.end, generics_hash)`. Avoids re-parsing the same
    /// type annotation repeatedly within one check run. Also deduplicates
    /// the errors such conversions emit (unknown type, arity mismatch).
    pub type_annotation_cache: std::collections::HashMap<(u64, u64), Type>,
    /// Names of module-level `state` bindings (D1). Since `state` is
    /// module-level only in v1, no scoping is needed — these live at
    /// scope index 0 for the whole check run.
    pub state_names: std::collections::HashSet<String>,
    /// Nesting depth of `atomic { }` blocks being checked. >0 means we're
    /// inside an atomic block (D2/D3 rules relax accordingly).
    pub atomic_depth: usize,
}

impl Checker {
    /// Does `ty` carry a registered implementation of the user trait
    /// `trait_name`? Bound checking for `<T: Draw>` funnels through here so
    /// every call site agrees with codegen about what counts as an impl.
    pub fn implements_trait(&self, ty: &Type, trait_name: &str) -> bool {
        match trait_type_key(ty) {
            Some(key) => self
                .trait_impls
                .get(trait_name)
                .is_some_and(|types| types.iter().any(|t| t == &key)),
            None => false,
        }
    }

    /// Scope-qualified key for an expression span under the item currently
    /// being checked. Interns the enclosing function name so all keys in
    /// the same function share one `Symbol` allocation.
    pub fn scope_key(&mut self, span: Span) -> SpanKey {
        let func = self.scope.last().map(|s| s.as_ref()).unwrap_or(TOP_SCOPE);
        let sym = self.func_interner.intern(func);
        self.symbol_map.insert(func.to_string(), sym.id());
        SpanKey::new(sym, span)
    }
    /// Record how a variable name is used, for type inference hints.
    pub fn record_usage(&mut self, name: &str, usage: UsageUsage) {
        let ctx = self
            .usage_context
            .entry(name.to_string())
            .or_insert_with(UsageContext::default);
        match usage {
            UsageUsage::Arithmetic => ctx.used_arithmetic = true,
            UsageUsage::Comparison => ctx.used_comparison = true,
            UsageUsage::Index => ctx.used_as_index = true,
            UsageUsage::String => ctx.used_as_string = true,
            UsageUsage::UnifiedWith(ty) => ctx.unified_with = Some(ty),
        }
    }

    /// Create a checker seeded with prior state. `initial_traits`/`initial_trait_impls`
    /// carry cross-module pub traits and impls for bound checking.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        initial_bindings: HashMap<String, Type>,
        funcs: HashMap<String, FuncSig>,
        structs: HashMap<String, StructSig>,
        aliases: HashMap<String, AliasSig>,
        enums: HashMap<String, EnumSig>,
        initial_traits: HashMap<String, TraitSig>,
        initial_trait_impls: HashMap<String, Vec<String>>,
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
            traits: initial_traits,
            trait_impls: initial_trait_impls,
            ext_methods: HashMap::new(),
            convert_impls: HashMap::new(),
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
            deferred_type_errors: Vec::new(),
            pub_names: std::collections::HashSet::new(),
            local_funcs: std::collections::HashSet::new(),
            local_structs: std::collections::BTreeSet::new(),
            local_enums: std::collections::BTreeSet::new(),
            local_traits: std::collections::BTreeSet::new(),
            local_aliases: std::collections::HashSet::new(),
            local_consts: std::collections::HashSet::new(),
            test_funcs: std::collections::HashSet::new(),
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
            func_interner: Interner::new(),
            narrowing: vec![std::collections::HashMap::new()],
            symbol_map: std::collections::HashMap::new(),
            usage_context: HashMap::new(),
            used_enum_variants: std::collections::HashSet::new(),
            used_struct_fields: std::collections::HashSet::new(),
            used_aliases: std::collections::HashSet::new(),
            used_struct_names: std::collections::HashSet::new(),
            used_enum_names: std::collections::HashSet::new(),
            used_trait_names: std::collections::HashSet::new(),
            used_const_names: std::collections::HashSet::new(),
            used_impl_methods: std::collections::HashMap::new(),
            scope_cache: std::collections::HashMap::new(),
            names_at_depth: vec![Vec::new()],
            link_libs: Vec::new(),
            extern_symbols: std::collections::HashMap::new(),
            domain: DomainState::default(),
            type_annotation_cache: std::collections::HashMap::new(),
            state_names: std::collections::HashSet::new(),
            atomic_depth: 0,
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

    /// Like `report_mismatch` but with a context string describing what the
    /// expected type is for (e.g. parameter name, field name, return type).
    /// Renders as "expected `{right}` for `{context}`, found `{left}`".
    pub fn report_mismatch_with_context(
        &mut self,
        mut err: UnifyError,
        span: Span,
        context: String,
    ) {
        err.context = Some(context);
        self.report_mismatch_with_notes(err, span, Vec::new());
    }

    /// `report_mismatch` plus caller-supplied Sherlock notes. Call sites
    /// that know the expected shape (parameter names, annotations) use
    /// this so hints stay typed — never string-parsed.
    pub fn report_mismatch_with_notes(&mut self, err: UnifyError, span: Span, notes: Vec<String>) {
        let msg = match err.message.as_str() {
            "type mismatch" => match &err.context {
                Some(c) => format!(
                    "type mismatch: expected `{}` for `{c}`, found `{}`",
                    err.right, err.left
                ),
                None => format!(
                    "type mismatch: expected `{}`, found `{}`",
                    err.right, err.left
                ),
            },
            "function arity mismatch" => "function arity mismatch".to_string(),
            "tuple arity mismatch" => "tuple arity mismatch".to_string(),
            other => other.to_string(),
        };
        let mut diag = error_at(msg, span);
        match err.message.as_str() {
            "type mismatch" => {
                diag = diag.with_code(ErrorCode(2));
            }
            "function arity mismatch" | "tuple arity mismatch" => {
                diag = diag.with_code(ErrorCode(21));
            }
            _ => {}
        }
        // Add secondary labels showing where each type came from, when we
        // have span info (e.g. "expected `int` because of this annotation
        // here", "found `str` at this variable declaration").
        if let Some(left_span) = err.left_span {
            diag = diag.with_secondary(SecondaryLabel {
                span: left_span,
                message: format!("found `{}` here", err.left),
            });
        }
        if let Some(right_span) = err.right_span {
            diag = diag.with_secondary(SecondaryLabel {
                span: right_span,
                message: format!("expected `{}` here", err.right),
            });
        }
        for note in notes {
            diag = diag.with_note(note);
        }
        // Type-mismatch diff for generics (e.g. Box<int> vs Box<str>).
        if err.message == "type mismatch" {
            if let Some(diff) = Self::type_diff_note(&err.left_ty, &err.right_ty) {
                diag = diag.with_note(diff);
            }
        }
        self.errors.push(diag);
    }

    /// Sherlock: returning an unwrapped `Result` where a plain value is
    /// expected (or vice versa) is the most common ZZ beginner error.
    /// Point at the value with the three actionable fixes (#245).
    /// Returns true when the hint was emitted (caller skips the generic
    /// return-type-span error to avoid a double report).
    /// Produce a structural diff note when two types share an outer
    /// constructor but differ at an inner level (e.g. `Box<int>` vs
    /// `Box<str>` → "differ at inner: expected `int`, found `str`").
    /// Returns `None` when there's no useful diff (different constructors,
    /// primitives, or one side is a var/named type).
    fn type_diff_note(left: &Type, right: &Type) -> Option<String> {
        let diff = Self::diff_types(left, right, 0)?;
        Some(format!("structural diff: {diff}"))
    }

    /// Recursively compare two types; returns a description of the first
    /// point of difference, or `None` when the types are structurally equal
    /// or incomparable.
    fn diff_types(left: &Type, right: &Type, depth: usize) -> Option<String> {
        if depth > 6 {
            return None;
        }
        match (left, right) {
            // Same constructor → recurse into inner types.
            (Type::Option(a), Type::Option(b)) => {
                Self::diff_types(a, b, depth + 1).map(|d| format!("Option inner: {d}"))
            }
            (Type::Array(a), Type::Array(b)) => {
                Self::diff_types(a, b, depth + 1).map(|d| format!("array element: {d}"))
            }
            (Type::Dict(ka, va), Type::Dict(kb, vb)) => Self::diff_types(ka, kb, depth + 1)
                .or_else(|| Self::diff_types(va, vb, depth + 1))
                .map(|d| format!("dict: {d}")),
            (Type::Result(a, ea), Type::Result(b, eb)) => Self::diff_types(a, b, depth + 1)
                .or_else(|| Self::diff_types(ea, eb, depth + 1))
                .map(|d| format!("Result: {d}")),
            (Type::Func(pa, ra), Type::Func(pb, rb)) => {
                let p_diff = pa
                    .iter()
                    .zip(pb)
                    .find_map(|(a, b)| Self::diff_types(a, b, depth + 1));
                let r_diff = Self::diff_types(ra, rb, depth + 1);
                match (p_diff, r_diff) {
                    (Some(d), None) => Some(format!("param {d}")),
                    (None, Some(d)) => Some(format!("return {d}")),
                    (Some(dp), Some(dr)) => Some(format!("params: {dp}; return: {dr}")),
                    (None, None) => None,
                }
            }
            (Type::Tuple(a), Type::Tuple(b)) if a.len() == b.len() => a
                .iter()
                .zip(b)
                .find_map(|(x, y)| Self::diff_types(x, y, depth + 1))
                .map(|d| format!("tuple: {d}")),
            (Type::Range(a), Type::Range(b)) => {
                Self::diff_types(a, b, depth + 1).map(|d| format!("range: {d}"))
            }
            (
                Type::Ptr {
                    mutable: ma,
                    inner: ia,
                },
                Type::Ptr {
                    mutable: mb,
                    inner: ib,
                },
            ) => {
                if ma != mb {
                    return Some(format!(
                        "mutability: expected `*{}`, found `*{}`",
                        if *ma { "mut" } else { "const" },
                        if *mb { "mut" } else { "const" }
                    ));
                }
                Self::diff_types(ia, ib, depth + 1).map(|d| format!("pointer inner: {d}"))
            }
            // Primitives / named / var / error / opaque — no useful inner diff.
            _ => None,
        }
        .or_else(|| {
            // If both are the same enum/struct name but different args, diff args.
            match (left, right) {
                (Type::Struct(n1, args1), Type::Struct(n2, args2))
                    if n1 == n2 && args1.len() == args2.len() =>
                {
                    args1
                        .iter()
                        .zip(args2)
                        .find_map(|(a, b)| Self::diff_types(a, b, depth + 1))
                        .map(|d| format!("struct `{n1}` arg: {d}"))
                }
                (Type::Enum(n1, args1), Type::Enum(n2, args2))
                    if n1 == n2 && args1.len() == args2.len() =>
                {
                    args1
                        .iter()
                        .zip(args2)
                        .find_map(|(a, b)| Self::diff_types(a, b, depth + 1))
                        .map(|d| format!("enum `{n1}` arg: {d}"))
                }
                _ => None,
            }
        })
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
                )
                .with_code(ErrorCode(2));
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
                )
                .with_code(ErrorCode(2));
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
                let mut diag = error_at(format!("expected `bool`, found `{other}`"), span)
                    .with_code(ErrorCode(15));
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
                self.errors.push(
                    error_at(format!("index must be `int`, found `{other}`"), span)
                        .with_code(ErrorCode(11)),
                );
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
    "pty",
    "signal",
    "uuid",
    "sqlz",
    "sqlz.postgres",
    "sqlz.mysql",
    "colors",
    "term",
    "test",
    "debug",
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

/// Set of stdlib native names that are safe to call inside auto-atomic
/// statements (D2): pure, non-blocking, never calls back into user code,
/// no I/O. Natives taking closures (map/filter/sort_by/...) are NOT
/// state_safe. Derived by auditing every std.vec / std.dict / std.math /
/// std.str / std.json / std.sys / std.log / std.crypto / std.encoding /
/// std.regexp / std.uuid / std.args native.
///
/// This is separate from `FuncSig.readonly` (which governs ARC retain
/// elision) because the two concepts differ: e.g. `std.str.trim` is readonly
/// but returns a new string (fine in auto-atomic), while `std.vec.push` is
/// not readonly but mutates its receiver (NOT fine in auto-atomic).
pub fn state_safe_natives() -> HashSet<String> {
    let mut s = HashSet::new();

    // Builtins (no import required).
    s.insert("range".into());
    s.insert("len".into());
    s.insert("ord".into());
    s.insert("chr".into());

    // std.str — pure string queries.
    s.insert("std.str.length".into());
    s.insert("std.str.contains".into());
    s.insert("std.str.find".into());
    s.insert("std.str.rfind".into());
    s.insert("std.str.byte_len".into());
    s.insert("std.str.ord".into());
    s.insert("std.str.starts_with".into());
    s.insert("std.str.ends_with".into());
    s.insert("std.str.starts_with_at".into());
    s.insert("std.str.ends_with_at".into());
    s.insert("std.str.is_empty".into());
    s.insert("std.str.count".into());
    s.insert("std.str.count_in".into());
    s.insert("std.str.find_in".into());
    s.insert("std.str.rfind_in".into());

    // std.str builders — linear string builders. Non-closure, non-blocking,
    // no-I/O, never call back into user code (they mutate a builder buffer
    // directly), so they are safe inside D2 auto-atomic statements.
    s.insert("std.str.builder".into());
    s.insert("std.str.push_part".into());
    s.insert("std.str.finish".into());
    s.insert("std.str.builder_len".into());
    s.insert("std.str.join_parts".into());
    s.insert("std.str.push_byte".into());
    s.insert("std.str.extend".into());

    // std.math — pure arithmetic queries.
    s.insert("std.math.abs".into());
    s.insert("std.math.signum".into());
    s.insert("std.math.clamp".into());
    s.insert("std.math.floor".into());
    s.insert("std.math.ceil".into());
    s.insert("std.math.round".into());
    s.insert("std.math.min".into());
    s.insert("std.math.max".into());
    s.insert("std.math.pow".into());
    s.insert("std.math.sqrt".into());

    // std.vec — pure query natives plus the non-closure, non-blocking,
    // no-I/O mutating natives (push/insert/remove/pop/sort). These are
    // safe inside D2 auto-atomic statements: they take a plain value
    // receiver, never call back into user code, and do no I/O or blocking.
    // Closure-taking forms (sort_by, sort_by_desc, sort_key) stay OUT.
    s.insert("std.vec.len".into());
    s.insert("std.vec.contains".into());
    s.insert("std.vec.push".into());
    s.insert("std.vec.insert".into());
    s.insert("std.vec.remove".into());
    s.insert("std.vec.pop".into());
    s.insert("std.vec.sort".into());

    // std.dict — dicts use index syntax (`d[k] = v`, `d[k]`) which lowers
    // to a store op (allowed under D2 as field/index access), so there are
    // no `dict.set`/`dict.insert`/`dict.remove` natives to whitelist. Pure
    // query natives only.
    s.insert("std.dict.len".into());
    s.insert("std.dict.contains".into());
    s.insert("std.dict.get".into());
    s.insert("std.dict.keys".into());
    s.insert("std.dict.values".into());

    // std.json — pure query natives only (NOT parse/stringify/merge).
    s.insert("std.json.len".into());
    s.insert("std.json.has".into());
    s.insert("std.json.type".into());
    s.insert("std.json.as_str".into());
    s.insert("std.json.as_int".into());
    s.insert("std.json.as_float".into());
    s.insert("std.json.as_bool".into());
    s.insert("std.json.is_null".into());
    s.insert("std.json.is_bool".into());
    s.insert("std.json.is_number".into());
    s.insert("std.json.is_string".into());
    s.insert("std.json.is_array".into());
    s.insert("std.json.is_object".into());
    s.insert("std.json.is_empty".into());
    s.insert("std.json.deep_get".into());
    s.insert("std.json.path_exists".into());
    s.insert("std.json.path_get_or".into());

    // std.sys — pure system info queries.
    s.insert("std.sys.os".into());
    s.insert("std.sys.arch".into());
    s.insert("std.sys.cpu_count".into());
    s.insert("std.sys.hostname".into());
    s.insert("std.sys.total_mem".into());
    s.insert("std.sys.avail_mem".into());

    // std.log — pure query only (NOT set_level/set_format/to_file).
    s.insert("std.log.get_level".into());

    // std.crypto — pure query/hash natives only (NOT keygen/sign/verify/Argon2/bcrypt).
    s.insert("std.crypto.sha256".into());
    s.insert("std.crypto.sha256_bytes".into());
    s.insert("std.crypto.sha512".into());
    s.insert("std.crypto.hmac_sha256".into());
    s.insert("std.crypto.random_bytes".into());
    s.insert("std.crypto.ct_eq".into());

    // std.encoding — pure encoding/decoding.
    s.insert("std.encoding.base64_encode".into());
    s.insert("std.encoding.base64_decode".into());
    s.insert("std.encoding.base64_decode_bytes".into());
    s.insert("std.encoding.hex_encode".into());
    s.insert("std.encoding.hex_decode".into());
    s.insert("std.encoding.url_encode".into());
    s.insert("std.encoding.url_decode".into());

    // std.regexp — pure query only (NOT compile).
    s.insert("std.regexp.is_match".into());
    s.insert("std.regexp.find".into());
    s.insert("std.regexp.captures".into());
    s.insert("std.regexp.is_email".into());

    // std.uuid — pure validation only (NOT v4/v7/parse).
    s.insert("std.uuid.is_valid".into());

    // std.args — pure getters only (NOT parser/flag setters).
    s.insert("std.args.get_str".into());
    s.insert("std.args.get_int".into());
    s.insert("std.args.get_bool".into());
    s.insert("std.args.positional".into());
    s.insert("std.args.was_help".into());

    s
}
