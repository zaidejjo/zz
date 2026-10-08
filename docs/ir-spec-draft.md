# ZZ bytecode IR spec — DRAFT for review (M1)

Status: FROZEN (float formatting §4 locked 2026-10-05 with recorded
VM ground truth + conformance fixtures). Normative keywords: MUST /
MUST NOT / SHOULD per RFC 2119.
This spec is the single source of truth for program behavior. Where it
conflicts with current engine behavior, the engines change (migration
table in §11), not the spec.

Decisions already locked (pre-M1 review): wrap for `+ - *`; trap for
`MIN/-1`, `MIN%-1`, negative `**` exponent, and OOB; write-through
chained stores; canonical NaN/inf formatting; canonical error struct;
bool-only jumps (checker guarantees); Unicode/float-format ops owned by
the Rust core.

## 1. Value model (decided first — everything else hangs off this)

1.1. ZZ is a **value language**. Assignment, argument passing, and
return **copy**. After `y = x`, mutating through `y` MUST NOT be
observable through `x` — for scalars, strings, arrays, dicts, tuples,
structs, options, and results. The VM already behaves this way
(`move_append_struct_copy` prints the unaliased result); the AOT
backend's header-only struct clone is a bug under this rule.

1.2. Sharing behind the scenes is allowed **iff unobservable**: COW,
move elision, arena allocation, and refcount sharing MUST preserve 1.1.
Concretely: a write through one binding MUST NOT leak into another
binding's storage. The existing `heal-on-escape` (arena → heap copy on
retaining stores) is the reference mechanism, generalized: any value
that outlives its allocating scope MUST be independently owned.

1.3. Indexing yields a **copy**. `m[0]` evaluates to an owned value;
mutating it mutates nothing until stored back.

1.4. Stores write through to the root binding. `m[0][0] = v` updates
`m` (the VM temp-clone drop is fixed; the AOT behavior is correct).
`Field`-under-`Index` (`s[0].v = x`) also writes through — the native
backend still drops these (tracked native bug, same class).

1.5. Strings and bytes are immutable values. No operation mutates a
string in place; concatenation builds a new value.

1.6. Closures capture **by reference to the variable**, not by value.
A closure and its defining scope observe each other's assignments to a
captured mutable variable. Each loop iteration creates a fresh binding
of the loop variable; closures created in different iterations capture
different variables.

1.7. Representation is unspecified but observably equivalent to cells:
captured `const` or never-reassigned variables MAY be copied by value;
captured, reassigned variables of non-escaping closures MAY live in a
stack slot accessed by reference; captured, reassigned variables of
escaping closures use a shared reference-counted cell. Annotations
selecting a representation are hints — removing them MUST NOT change
program output (quad runs with `--strip-annotations` in M1 impl).

1.8. `spawn` moves or copies captured variables into the new task at
spawn time. No mutable cell is implicitly shared across tasks;
cross-task sharing requires an explicit shared type (channels and
future shared handles, §1.7 of the pre-M1 report scope).

1.7. Explicitly shared handles (channels, task joins, DB connections,
opaque native handles) are the ONLY aliasing exceptions. Their sharing
semantics live with their owning module spec, not here.

## 2. Types and ops

2.1. Scalar kinds: `I64`, `F64`, `BOOL`, `UNIT`. Composite values
(str, array, dict, tuple, struct, option, result, func, opaque) travel
through generic ops that dispatch to `zzrt`.

2.2. Core ops are **typed**: `ADD_I64`, `SUB_I64`, `MUL_I64`,
`DIV_I64`, `REM_I64`, `NEG_I64`, `SHL_I64`, `SHR_I64` (arithmetic
shift), `AND_I64`/`OR_I64`/`XOR_I64`/`NOT_I64`, `ADD_F64`, …,
`CMP_I64` (lt/eq…), `CMP_F64`, `JMP_TRUE`/`JMP_FALSE` (bool only),
`CONV_F64_I64`, `CONV_I64_F64`, `CONV_STR_I64`, … The verifier knows
each op's stack type signature (§8).

2.3. No `TRUTHY` op exists. Conditional jumps take `BOOL` only. The
checker guarantees bool conditions; the verifier rejects a
statically-non-bool condition; a dynamically-non-bool condition traps
`Type` (defensive — unreachable on checker-sound programs).

## 3. Integer semantics (MUST)

- `ADD_I64`/`SUB_I64`/`MUL_I64`/`NEG_I64`: two's-complement **wrap**.
  (`MIN` negation yields `MIN`.) This retires the debug-build traps.
- `DIV_I64`: trap `DivZero` on divisor 0; trap `Overflow` on
  `MIN / -1`. Otherwise truncating division.
- `REM_I64`: trap `DivZero` on 0; trap `Overflow` on `MIN % -1`.
- `SHL_I64`: count `& 63`; negative count traps `Bounds`. Logical
  (modulo-2⁶⁴) shift. `SHR_I64`: arithmetic (sign-extending) shift,
  same count rule.
- `AND/OR/XOR/NOT_I64`: total, bitwise.
- `POW_I64`: repeated multiplication (wrapping); negative exponent
  traps `Domain`. (`0 ** 0` is 1.)

## 4. Float semantics (MUST, FROZEN)

Arithmetic: IEEE-754 binary64. NO fast-math in parity builds: NaN quiet,
`1.0/0.0 = inf`, `0.0/0.0 = NaN`, signed-zero arithmetic per IEEE.
`REM_F64` is `fmod`. `POW_F64` is `pow` (with the integer fast path
only when it agrees bit-for-bit — else call `pow`). Mixed `I64`/`F64`
arithmetic promotes to `F64` (exactness loss accepted). Mixed
comparisons in checked programs are rejected by the checker; the IR
still defines promotion for completeness.

### 4.1 Float formatting — canonical (MUST, FROZEN)

Output is the shortest decimal string that round-trips to the same
`f64` (Rust `Display` semantics), produced by the Rust runtime core
only. No backend formats floats itself: the VM computes it inline and
the AOT backend calls `zz_float_format_raw` (in `zz_native_rt`
`float_fmt`) through the C ABI. C MUST NOT use `printf %g` (or any C
float printer) for user-visible floats.

The rule, arm-for-arm with the VM (`zz_runtime::value` float Display):

- finite + integral (`fract() == 0`, includes `±0`): `{x:.1}`.
- anything else: `{x}` (shortest round-trip).
- special values fall out of Rust `Display`: `NaN`, `inf`, `-inf`.

Recorded VM ground truth (2026-10-05; ZZ has no exponent literals,
so extremes are built via `math.pow` / `float("…")` parsing, which
accepts exponents):

| value | prints |
|---|---|
| `0.1` | `0.1` |
| `0.1 + 0.2` | `0.30000000000000004` |
| `1.0` | `1.0` (integrals keep `.0`) |
| `-0.0` (literal) | `-0.0` |
| `0.0 - 0.0` (computes `+0`) | `0.0` |
| `math.pow(10.0, 21.0)` (= 1e21) | `1000000000000000000000.0` (full expansion, never exponent) |
| `math.pow(10.0, -7.0)` (= 1e-7) | `0.0000001` (positional, never exponent) |
| `2.5`, `100.0`, `123456789.0` | as written (`.0` kept) |
| `math.NAN` | `NaN` |
| `math.INF` | `inf` |
| `0.0 - math.INF` | `-inf` |
| `f64::MAX` | 309 digits + `.0` (311 chars, no exponent) |
| `5e-324` (min subnormal) | `0.` + 322 chars ending in `5` (326 chars, no exponent) |

Conformance: `edge_float_format` pins all of the above in-fixture on
both engines (exact strings, including the 300+ char expansions) plus
strict quad parity; `edge_float_nan_display` pins `NaN` via `0.0/0.0`.
The Rust-core unit tests (`float_fmt`) pin the same vectors. Do not
change VM output: any VM Display change must break these pins loudly.

Out of scope for §4 (separate paths, unchanged): JSON stringify keeps
its own number rendering on both engines; float→int saturation is §5.

## 5. Conversions (MUST)

- `CONV_F64_I64` (int(f)): truncate toward zero; saturate
  (`NaN → 0`, `+∞ → MAX`, `-∞ → MIN`). Both engines already agree —
  pinned by `edge_cast_float_int`.
- `CONV_I64_F64`: nearest representable (hardware cast).
- `CONV_STR_I64` (int(s)): trim ASCII whitespace, optional sign,
  full-consume decimal digits; overflow or malformed yields `none`.
  (Agreed today — pinned by `edge_cast_str_int`.)
- `CONV_*_BOOL`: int/float nonzero, nonempty string/array/dict.
- `CONV_*_STR` for display goes through `zzrt` formatting (§4).

## 6. Composite semantics (MUST)

- Index OOB (array/tuple/str-by-char) traps `Bounds`. The AOT's silent
  unit is a bug. Negative indices normalize (`len + i`); still-OOB
  traps. String indexing is by Unicode scalar, not byte.
- Slice ends clamp to `[0, len]` (pinned by `edge_slice_clamp`); start
  past end yields empty. Slicing a string slices by chars.
- Dict missing-key read traps `Bounds`; dict index-store inserts.
- String `+` concatenates; `len(str)` counts chars; `lower`/`upper`/
  `trim` are Unicode-aware and owned by the Rust core (no C
  reimplementation — same rule as float formatting).
- Struct field read of an unknown field is a checker error; field
  write on a missing value traps `Type`.

## 7. Evaluation order and control flow (MUST)

- Operands evaluate left-to-right. Call: callee, then args
  left-to-right. `&&`/`||` short-circuit; Elvis/`??`/`?` evaluate the
  RHS at most once, exactly once iff needed. Probed and agreed on both
  engines (`edge_eval_order` strict).
- Stores evaluate source order: base, index, value — each side exactly
  once (compound stores read-then-write through one index evaluation,
  `edge_compound_index_eval` strict). Chained plain stores re-evaluate
  the outer base/index on write-back (documented double-evaluation
  contract, pinned by `eval_order_store`); every individual store still
  orders base, index, value. Both engines agree (`edge_index_store_order`
  and `eval_order_store` strict).
- Code is basic blocks with explicit `JMP`/`JMP_TRUE`/`JMP_FALSE`.
  Every block records its entry stack depth; every join MUST agree
  (verifier-enforced). Loops are blocks + back-edges with a `SAFEPOINT`
  at the header (cooperative yield point, stack-neutral).
- Calls push a frame; `RET` returns the top of stack. `defer` runs LIFO
  at scope exit (existing semantics preserved verbatim).

## 8. The `.zzc` format (M1 implementation target)

- Little-endian. Magic `ZZC1`, `u32` version (this spec = 2),
  section table: `TYPES`, `STRINGS` (interned names), `CONSTS` (value pool),
  `FUNCS` (name, arity, signature, locals type table, entry block),
  `CODE` (blocks + typed ops),
  `SPANS` (op → source span), `ANNOT` (performance hints, §9).
- v2 adds the per-function locals table: one type id per frame slot
  (params seeded from the signature; conflicts widen to the "unknown"
  top). Types are mandatory semantics, NOT annotations — the verifier
  checks every store/call/return against them, and `dis` shows them.
  The v2 decoder rejects v1 files (re-emit with `zz build --emit-ir`).
- Constants pool holds all literals (including string-literal
  markers for `print` — the M2 subset needs no string machinery
  beyond this).
- The format MUST make stack→register lowering possible later:
  single-assignment-friendly op stream, explicit block params instead
  of cross-block stack juggling, no implicit stack effects.
  (v1 status: flat stack code with verifier-enforced join depths —
  join agreement is checked, block params are future work for the
  register-lowering slice.)
- `zz dis` disassembles `.zzc` to stable text (golden-testable).
- Serializer + deserializer round-trip bit-identically; the
  deserializer is fuzzed (malformed inputs MUST be rejected, never
  crash).

## 9. Annotations: HIR analyses ride along, never decide

Escape classes (`Escapes` / `ArgLocal` / `ReturnFresh`), move-elision
homes, arena pre-size hints, and unboxed-scalar facts travel in `ANNOT`
so the M5 5% gate stays reachable without re-inference in backends.
RULE: dropping every annotation MUST NOT change program behavior —
annotations are performance-only. The verifier ignores them; a
`--strip-annotations` mode in tooling proves it by differential test.

## 10. Errors (MUST)

Canonical trap record: `{ kind, message, span, exit_code }`.
`kind` is a closed enum: `Overflow`, `DivZero`, `Domain`, `Bounds`,
`Type`, `Arity`, `Undefined`, `OOM`, `Native`, `Panic`, `Assert`.
`exit_code` is 1 for every trap (0 success; 2 CLI usage — unchanged).
`message` text is NOT gated for parity (kind + exit are); message
convergence is a SHOULD. Traps print `error: {message}` + span to
stderr (VM shape today; AOT adopts it, replacing bare
`zz error: ...` lines).

## 11. Migration: spec-vs-today deltas

| Behavior | Today | Spec | Fixture flip |
|---|---|---|---|
| `+ - *` overflow | VM-debug traps / VM-release + native wrap | wrap | `edge_int_overflow_*`, `edge_int_neg_min` become strict (non-error) |
| `MIN/-1`, `MIN%-1` | VM-debug traps / others wrap-or-UB | trap `Overflow` | stay error fixtures; native gains checks |
| neg `**` exp | VM errors / native 0 | trap `Domain` | stays error; native gains check |
| OOB index | VM traps / native unit+exit 0 | trap `Bounds` | stays error; native gains check |
| chained stores | VM dropped (fixed) / native through | through | `edge_chained_store` strict (done) |
| field-of-index store | both drop (fixed this batch) | through | write-through family strict |
| plain index-store order | VM value-first / native source-order | source order | `edge_index_store_order` strict since zzc-codec; `eval_order_store` conformance |
| compound index eval | VM once / native twice (regression) | exactly once | `edge_compound_index_eval` strict after AOT fix |
| NaN display | `NaN` vs `nan` | `NaN` (+owned formatting) | `edge_float_nan_display` strict after C fix |
| int(NaN) under -O3 | 0 or MIN (UB) | saturate → 0 | `edge_cast_float_nan` strict after `-ffast-math` removal |
| conditions | truthy soup | bool-only | checker already guarantees; verifier enforces |

## 12. Open questions for M1 implementation (not blockers for this draft)

Closure-value copy cell sharing (§1.6); negative-index parity proof;
call/operand order proof (§7); OOM diagnostics parity in C; message-text
convergence scope; `try`-conversion table encoding in IR.
