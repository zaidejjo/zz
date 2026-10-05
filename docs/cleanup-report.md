# Post-cleanup divergence report (pre-M1 PR batch)

Scope decided before M2: flags+NaN, #1–#5, #8+family, #9, #10, #15+#17,
#22 now; #16/#18/#19/#20 deferred to HIR→IR; #11/#12/#21 runtime track.
Full dual-engine parity green (177 + negative-index probe).

## Fixed this batch (strict on all legs unless noted)

- Flags (#24–26) + NaN guard (#7): `-ffast-math`/`-march=native` gone,
  `-fwrapv -fno-strict-aliasing` everywhere, `RT_CACHE_VERSION` rt3,
  explicit `isnan` in `zz_int_cast`. `edge_cast_float_nan` strict.
  Bench: perf-neutral within noise (`bench/ir_baseline.md`).
- #1 wrap `+ - *` (incl. neg, pow-overflow, loop counters, slot fused
  paths): `edge_int_overflow_add/mul`, `edge_int_neg_min` moved
  errors/→regression as labeled strict probes.
- #2+#3 `MIN/-1`, `MIN%-1` trap everywhere (VM explicit checks, C
  `zz_binop` checks, scalar Div/Rem routed off raw C `/`/`%` — the old
  literal-zero fallback also fixed a latent raw-operand miscompile).
  New `edge_int_min_rem_neg1` probe. Also hardened: stale-release-driver
  rebuilds via source-stamp gating (the quad caught a stale driver).
- #4 pow-neg traps natively; #5 OOB reads trap (hoisted err check;
  missing dict keys trap too, matching the VM); #8+family: detach-on-write
  `zz_index_set`, struct header COW, temp+write-back triples for computed
  bases, plain/compound Field arms, SROA root invalidation. 5 new strict
  fixtures + struct-path peel. ASan+UBSan clean; 2000-iteration shared-store
  loop leak-free.
- #9 main `.err` propagates (new `zz_main_result_code`); #10 pg.connect
  traps (sqlite leniency untouched); #15+#17 bare scalar globals recognized
  as unboxed (fuzz CLASS 2 FIXED); #22 struct header COW (ASan clean,
  leaks reduced vs baseline).

## Remaining divergences

| Item | Disposition |
|---|---|
| `edge_float_nan_display` (`NaN` vs `nan`) | CLOSED — canonical formatting via Rust core; strict |
| `math_extended_test` precision | CLOSED on precision (exact agreement); remaining diffs are error-message texts + nondeterministic rand (known-failure stays for those) |
| `concurrency_panic_test` (err through task closures) | OPEN (a-M) |
| `encoding_test` messages | OPEN polish (a-S) |
| `struct_impl`, `struct_embedding`, `decorators`, `extension_methods`, selective-import group | DEFERRED to HIR→IR (noted in harness) |
| P3 HTTP `vm-only` set (6) | runtime track |
| Unicode `lower/upper/trim` | runtime track (float→string ownership CLOSED — Rust core) |
| Plain index-store order (VM value-first vs source order) | OPEN, decided: source order canonical; VM changes post-M1 (`edge_index_store_order` quad-split) |
| Closure capture of block/loop locals | OPEN: VM raises runtime undefined-variable, native yields unit (both deviate from §1.6/1.8; probes in /tmp, fixtures after fix decision) |
| Spawn capture sharing | OPEN: VM snapshots (correct per §1.8), native shares cells (deviates) |
| OOM diagnostics parity | CLOSED as documented divergence (below) — live-OOM fixtures are CI-hostile |
| Message-text convergence | ongoing polish, non-gating by spec §10 |
| `try`-conversion table | M1 impl (representational) |
| Negative indices | CLOSED — `edge_negative_index` strict |
| Scalar-global double-unbox (fuzz CLASS 2) | CLOSED + follow-up: function-body site fixed (`emitted_is_raw_scalar` matches global C ids) |

## Closure probe report (spec §1.6–1.8 ground truth)

Probed on both engines; the decided semantics is the reference, engines
are judged against it:

- By-reference both directions (scope→closure, closure→scope): BOTH AGREE.
- Escaping closures (returned from `func`, called later): BOTH AGREE.
- Loop/block-local capture (`for i`, `while`-body `y`, bare-block `z`
  referenced by a closure): BOTH DEVIATE — the VM raises a runtime
  `undefined variable` (check passes; the capture scan in
  `vm/capture.rs` never marks block/loop locals as captured), while
  native captures but yields unit (silent wrong value). Fix size: M on
  each engine (VM: mark closure-referenced block/loop locals captured +
  per-iteration fresh cells; AOT: capture block/loop locals into the
  closure env). Recommended post-M1 alongside the IR escape analysis.
- `spawn` capture: VM snapshots at spawn (CORRECT per §1.8); NATIVE
  SHARES the cell (observes post-spawn mutation — deviates). Fix size:
  S–M on the AOT task env (clone captures at spawn).

## OOM audit close-out

No live-OOM fixture: the only language-level huge-allocation path
(`str.repeat` with a 2⁶² count) hangs instead of failing fast (it is a
ZZ-level loop), and real exhaustion fixtures are nondeterministic and
hostile to CI (cgroup kills, swap storms). Documented contract instead:

- Native: fallible sites print `zz: out of memory (<site>)` /
  `zz: arena out of memory` to stderr and exit 1 (malloc-checked).
- VM: Rust allocation failure aborts via the Rust OOM handler
  (SIGABRT, no ZZ message).
- Unifying these (OOM handler + exit-code parity) is post-M7 work;
  the IR carries no OOM semantics in M1.

## Spec freeze

`docs/ir-spec-draft.md`: FROZEN except §4 canonical float formatting,
which awaits the pending decision (blocks only `math_extended_test`
precision + the `zzrt` formatting ownership in M3). Closure capture
semantics (§1.6–1.8) are locked per the provided decision.
