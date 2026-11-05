# Checker — User-Defined Traits/Interfaces (Phase 2, item 6)

## Current state

ZZ has **built-in trait bounds** only (`Num`, `Ord`, `Eq`, `Decl`), declared on
generic parameters (`func add<T: Num>(x: T, y: T) -> T`) and validated at call
sites via `validate_bounds` (deferred-obligation check, item 2). There are **no
user-defined traits**: no `trait` declaration, no `impl Trait for Type`, no
trait-based method dispatch. `impl` blocks today are **inherent** only
(`impl Point { func dist(self) -> int }`), registered as `Point.dist` functions.

This document specifies a concrete, minimal user-defined trait design that fits
the existing checker/codegen shape, then states what is implemented now versus
deferred.

---

## Design (minimal, concrete)

### 1. Syntax

```zz
// Trait declaration with method signatures (no bodies).
trait Drawable {
    func draw(self) -> str
    func area(self) -> int
}

trait Comparable<T> {
    func cmp(self, other: T) -> int
}

// Implementation: trait for a concrete or generic type.
impl Drawable for Circle {
    func draw(self) -> str { return "circle" }
    func area(self) -> int { return 3 }
}

impl Comparable<int> for Box<int> {
    func cmp(self, other: int) -> int { return self.v - other }
}
```

- `trait Name<Params> { ... }` — declares method signatures. Bounds on trait
  type parameters use the built-in bounds (`T: Num`).
- `impl Trait for Type { ... }` — provides method bodies. `Type` may be a
  concrete struct/enum/alias or a generic instantiation.
- Method call syntax unchanged: `obj.draw()` — resolved first as inherent
  method, then as trait method (see resolution order below).

### 2. Trait table

Added to `Checker`:

```rust
pub traits: HashMap<String, TraitSig>,        // name -> TraitSig
pub trait_impls: HashMap<(String, Type), Vec<TraitImplSig>>, // (trait, type) -> impls
```

```rust
pub struct TraitSig {
    pub name: String,
    pub generics: Vec<String>,
    pub bounds: HashMap<String, Vec<TraitBound>>,   // trait type param bounds
    pub methods: Vec<(String, FuncSig)>,            // method name -> signature
}
pub struct TraitImplSig {
    pub trait_name: String,
    pub trait_args: Vec<Type>,                      // substituted trait type args
    pub for_type: Type,                             // the type being implemented for
    pub methods: Vec<(String, FuncSig)>,            // implemented method bodies
}
```

### 3. Validation rules (check time)

**Trait declaration:**
- Method names unique within the trait.
- Method signatures must be well-formed (types resolve; bounds satisfied).

**Impl (`impl Trait for Type`):**
1. **Trait exists** — the referenced trait is registered.
2. **Arity** — number of type args provided matches trait generics.
3. **Bounds** — trait type args satisfy the trait's parameter bounds.
4. **Orphan rule (coherence, part 1)** — at least one of the trait or the
   `for` type must be defined in the current crate/module. Implementing a
   foreign trait for a foreign type is rejected ("orphan impl").
5. **Coherence (part 2)** — at most one impl per `(trait, for_type)` pair in
   the program. Duplicate impls are rejected.
6. **Signature conformance** — the impl provides every trait method, each with
   a signature that unifies with the trait's declared signature (receiver `self`
   maps to `for_type`). Extra methods beyond the trait are allowed (inherent
   methods).
7. **Method bodies type-check** against the conformed signatures.

**Method dispatch (`obj.method(...)`):**
1. Inherent method (`for_type.method`) wins if present.
2. Else trait method: find the unique impl of any trait the type implements
   that provides `method`. If multiple traits provide `method` (ambiguity),
   report "method `method` is ambiguous; qualify with `<Trait as Trait>::method`".
3. Else error "no method `method` for type `T`".

### 4. Generics

Traits and impls may be generic:

```zz
trait Eq<T> {
    func eq(self, other: T) -> bool
}

impl Eq<int> for Wrapper<int> { ... }   // T=int
impl<T: Num> Eq<T> for Box<T> { ... }   // T bounded by Num
```

Impl generic parameters are independent of trait generic parameters; impl
bounds are checked at the impl site, trait bounds at the use site.

### 5. Codegen / runtime

Trait method dispatch lowers to a direct call of the selected impl method
(monomorphic, like inherent methods today) because ZZ has no runtime vtable or
monomorphization in this phase. A `obj.draw()` that resolves to trait
`Drawable` impl for `Circle` becomes a direct call to
`<Circle as Drawable>::draw`. No dynamic dispatch, no trait objects.

This keeps VM ≠ native identical (both emit the same direct call) and avoids
the complexity of vtable layout, which is a larger feature.

---

## What is implemented now

Nothing for user-defined traits. Only built-in bounds (`Num`/`Ord`/`Eq`/`Decl`)
exist (item 2). `impl` blocks are inherent only.

### Files to touch for a full implementation

| Concern | File(s) |
|---------|---------|
| AST: `Stmt::Trait`, `Stmt::ImplFor` (or extend `Stmt::Impl`) | `crates/zz_span_ast/src/ast/stmt.rs` |
| Parser: `trait` keyword, `impl Trait for Type` | `crates/zz_parser/src/stmt.rs` |
| Checker: trait table, impl table, orphan/coherence, conformance, dispatch | `crates/zz_checker/src/checker/mod.rs` (+ `zz_checker_core`) |
| Codegen: direct-call lowering for trait methods | `crates/zz_lower`, `crates/zz_codegen` |
| Stdlib/natives: none needed | — |

### Decision: implement now or defer?

**Deferred.** A sound trait system with orphan/coherence rules, signature
conformance, and dispatch touches the parser, checker, and all three backends
(VM, native/C, chunk). Doing it soundly in one phase risks regressions in the
existing `impl` (inherent) path and the generic bounds machinery just hardened
in item 2. The design above is the spec; implementation is a follow-up phase
with its own test-first fixtures.

### Related

- `docs/checker/decisions.md` — closure capture, double-check candidate.
- `docs/language/variables-functions.md` — capture-by-reference doc.
- Item 2 (generic bounds) already validates built-in `TraitBound`s; this doc
  extends the concept to user traits.
