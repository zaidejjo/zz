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
| `edge_float_nan_display` (`NaN` vs `nan`) | OPEN, tiny — one C float-printer branch; unscoped, candidate micro-PR or M6 |
| `concurrency_panic_test` (err through task closures) | OPEN (a-M) |
| `encoding_test` messages | OPEN polish (a-S) |
| `math_extended_test` precision | BLOCKED on canonical float formatting decision |
| `struct_impl`, `struct_embedding`, `decorators`, `extension_methods`, selective-import group | DEFERRED to HIR→IR (noted in harness) |
| P3 HTTP `vm-only` set (6) | runtime track |
| Unicode `lower/upper/trim`, float→string ownership | runtime track (float half blocked on format decision) |
| Call/operand evaluation order | OPEN audit (no divergence observed; probes in M1 impl) |
| OOM diagnostics parity | OPEN small |
| Message-text convergence | ongoing polish, non-gating by spec §10 |
| `try`-conversion table | M1 impl (representational) |
| Negative indices | CLOSED — `edge_negative_index` strict |

## Spec freeze

`docs/ir-spec-draft.md`: FROZEN except §4 canonical float formatting,
which awaits the pending decision (blocks only `math_extended_test`
precision + the `zzrt` formatting ownership in M3). Closure capture
semantics (§1.6–1.8) are locked per the provided decision.
