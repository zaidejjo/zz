# Append-elision report: move-on-self-reassign (native + VM)

Branch: `perf/move-append-elision` · Date: 2026-10-02 · Machine: `x86_64`
Corpus manifest sha: `645532fc56366eaa` (deterministic, seed 7)

## 1. Objective

Eliminate the quadratic append pattern (`x = vec.push(x, e)` and
shape-alikes) on both engines via move-on-self-reassign: take the array
out of its home, push in place, store back — zero clones on the steady
path.

## 2. Mechanism

**Native** (`crates/zz_codegen/{lower/move_elide.rs,runtime/collections.c}`):
`zz_vec_push_take` / `zz_object_push_field_take` plus `zz_len_field`
(reads a field length without the getter's retaining clone). A take
fires only when the buffer counter proves unique ownership
(`arr->refs == 1`); every miss falls back to the generic
clone-push-store, which is always value-correct. `x = f(x, …)` threads
the argument through by borrow plus `refs == 1`.

**VM** (`crates/zz_runtime/src/vm/{compiler,runtime}.rs`, `env.rs`,
`capture.rs`): fused `VecPush` / `VecPushField` / `VecPushMethod` ops
with `TakeSlot` / `TakeVar` homes, stdlib arg-slot reuse, true `Env`
moves (`EnvLink::take` / `try_assign` — same owning-scope and
frozen-detach discipline as `assign`, but moving instead of cloning),
a moved (never cloned) `unwind_frame` return payload, and tail-take:
a function ending in `return x` / bare `x` moves the frame local out
instead of deep-cloning it (NRVO-equivalent; outer bindings keep the
cloning load). Top-level statements no longer force environment
promotion (`nested` capture flag); callee-position references stay
promoted because `CallPath` resolves through the environment.

## 3. Refcount / take trace: `doc = push_node(doc, x)` (11-array struct)

Per key on the steady path (verified with temporary `FUSE` emission
and runtime hit/miss probes, since removed):

1. `TakeSlot(thread_doc.doc)` — the module-global load moves instead
   of cloning (path-thread take; slot home proves no callee can name
   the transient `Unit`).
2. Call args move into param slots (no clone on entry).
3. 5× `VecPushField(home=Slot(0))` — all direct in-place hits
   (`refs == 1`); zero fallback clones.
4. Tail `TakeSlot(doc)` — the returned struct moves out, no O(n) deep
   clone (this was the last quadratic: `Value::Array`/`Object` clones
   are deep, so one missed return clone per key is O(n²)).
5. `unwind_frame` moves the payload; `StoreSlot` restores the global.

Native mirrors it: borrow + `refs == 1` takes, struct returns are
shallow retains (O(1)), misses use the healing fallback.

## 4. Before / after (release, time ms / peak RSS KiB)

BEFORE = main (`zz 0.1.4`, `/tmp/before_main_release.md`);
AFTER = this branch (`zz 0.1.6`, `/tmp/after_final_release.md`).
Method: `bench/move_append/run.py` (`--zz` selects the binary), wall
clock + per-child `wait4` `ru_maxrss`.

### Microbenchmarks

| case | VM before | VM after | native before | native after |
|------|-----------|----------|---------------|--------------|
| `push_int N=10000` | 7678 | 103 | 5089 | 102 |
| `push_str N=5000` | 15858 | 153 | 2129 | 105 |
| `field_push N=10000` | 18596 | 102 | 4349 | 52 |
| `thread_doc N=2000` | 9446 | 153 | 1269 | 54 |

RSS stays flat: VM ~18 MB, native ~12 MB (before: VM grew to 30 MB
on corpus cases; native flat_1m timed out at 590 s, now 253 ms).

### Corpus proxy (line-pattern appends through `doc = push_*(doc, …)`)

| case | VM before | VM after | native before | native after |
|------|-----------|----------|---------------|--------------|
| `proxy/flat_1k` | 422 | 102 | 154 | 52 |
| `proxy/mixed_1k` | 205 | 102 | 53 | 51 |
| `proxy/flat_8k` | 3315 | 153 | 209 | 51 |
| `proxy/mixed_8k` | 2076 | 160 | 203 | 55 |
| `proxy/flat_32k` | 67738 | 153 | 1316 | 56 |
| `proxy/mixed_32k` | 17279 | 406 | 307 | 51 |
| `proxy/flat_128k` | skipped | skipped | 13046 | 52 |
| `proxy/mixed_128k` | skipped | skipped | 3250 | 51 |
| `proxy/flat_1m` | skipped | skipped | timeout:590s | 253 |
| `proxy/mixed_1m` | skipped | skipped | 186673 | 155 |

### 11-array struct threading at scale (release, best of 2, startup ~60 ms incl.)

| N appends | VM | native |
|-----------|----|--------|
| 1000 | 137 | — |
| 2000 | 98 | 54 (`thread_doc` micro) |
| 4000 | 95 | — |
| 8000 | 130 | 167 |
| 16000 | 226 | 96 |
| 32000 | 312 | 178 |
| 64000 | 314 | 142 |

Flat on both engines (residual slope is per-key constant work: 5 field
pushes + call/return; N=0 startup is ~60–190 ms depending on load).

## 5. Real `toml.parse` re-measurement (no changes to `../toml`)

A path-dep bench project (`/home/zaid/toml_bench`, own `zz.lock` +
`vendor/` link; `../toml` untouched — `git status` there is clean)
runs real `toml.parse` on the harness corpus with the new release
compiler. Before-numbers via the old compiler are unavailable: the
0.1.4 binary cannot compile the current toml source (checker drift —
`expected Result<…>, found unit` in `parser.zz`), so the proxy table
above stands as the before-baseline.

| corpus | VM after (release) |
|--------|--------------------|
| flat_8k (486 nodes) | ~14–17 s |
| flat_32k (1943 nodes) | ~249–286 s |
| flat_128k | >590 s (timeout) |
| mixed_8k (29 nodes) | ~3.4 s |
| mixed_32k (111 nodes) | ~37–115 s (wide: measured under full test-suite load) |
| mixed_128k | >590 s (timeout) |

**Analysis — appends are linear, parsing is not:** every parsed key
does 3× sorted-position `vec.insert` into the `x_*` lookup index
(`value.zz`: `table_insert` → `xsearch` + insert). Each insert is an
O(n) memmove plus an *unfused* whole-array clone (`insert` has no take
form), so per-key cost is O(n) regardless of push elision — ~17× time
for 4× keys on flat files. Fusing `x = vec.insert(x, i, v)` would only
shrink the constant (the shift remains); true linearity needs a
parser-side index change (append + sort once, or a hash index). That
is a follow-up in the toml package, explicitly out of scope here.

## 6. Correctness

- **Aliasing fixtures** (`tests/fixtures/regression/move_append_{alias,early_exit,spawn,field,shapes,append}.zz`):
  live shares (`b = a`), self-push, double-read elems, dict slots,
  closure captures, spawn snapshots, `?` early exit, `break` windows —
  all byte-identical VM vs native. Values print as `name=value`
  (never bare numbers): the parity harness strips purely-numeric
  lines, so bare-number fixtures compare vacuously — caught live when
  the struct-copy known-failure reported a false "FIXED!".
- **Fuzz** (`tests/fuzz/gen.py --shapes v3`, 9 families: alias,
  self-push, method/append loops, field loops, threading, closure
  capture, dict slots, early exit): 1000 cases, VM exit 0 × 1000,
  native exit 0 × 1000, **0 divergences**.
- **ASan** (temporary `ZZ_ASAN_OLEVEL` hook, reverted after): all
  e2e fixtures (202: syntax/types/stdlib/regression/errors) at -O1
  and -O3, plus the 60-case v3 screen at both levels, with
  `detect_leaks=0`. Result: the **only** error is
  `extension_methods.zz` (null+8 SEGV), which reproduces on the
  main-branch binary — a pre-existing known native failure, not from
  takes. Exit-time leaks exist but are pre-existing (the generic
  `vec.push` path leaks identically; the runtime never frees
  exit-live values).
- **Suites**: `scripts/test-fast.sh` green (fmt, clippy `-D warnings`,
  all unit tests incl. 165 `zz_runtime` + fused-op emission tests,
  e2e VM, parity VM-leg 111/111); full dual-engine parity 110/111
  with the single miss being the vacuous-comparison artifact above,
  fixed by labeled prints and re-verified 7/7 on both engines.
- **Free-push misrouting regression**: `vm_fused_free_push_emits_vec_push_not_method`
  pins that 2-arg `vec.push` fuses to `VecPush`, not the method path.
- **Tail-take regression**: `vm_tail_take_moves_param_and_spares_outer`
  pins param moves and outer-binding safety for functions and closures.

## 7. `vec.append` resolution (was: tracked VM-vs-native divergence)

Correct behavior per `docs/stdlib.md` is the VM/docs behavior:
`vec.append(v, x)` returns the new array (alias for `vec.push`).
The native backend lowered it to an in-place unit mutator instead.
Fix: map `vec.append | std.vec.append` to `zz_vec_push` (value form);
statement position still mutates in place via the existing
void-context swap, same as `vec.push`. The `move_append_append`
fixture is now strict parity on both engines, and `docs/stdlib.md`
documents the alias row.

## 8. Open design decisions / follow-ups

1. **Native struct-clone sharing** (`move_append_struct_copy`,
   `parity_known_failure`): `zz_clone` bumps only the object header
   while `zz_release_object` frees field buffers — a cloned struct
   shares field arrays at `arr->refs == 1`. Options: (a) deep-bump
   retain (clone bumps field counters; touches every struct clone);
   (b) COW field write (check-and-dup on set; touches every field
   store); (c) keep value-divergence documented (status quo). The
   take gate on the buffer counter is observably equivalent to the
   generic path either way, so takes are safe regardless.
2. **Frozen-chain take/assign below a frozen level** drops the
   intermediate frozen vars (same pre-existing shape as `assign_rec`);
   takes only fire for frame-local homes so the path is nearly
   unreachable — shares the fix with `assign` if ever addressed.
3. **Real-toml linearity** needs the parser-side index change (§5).
4. **`vec.insert` take fusion** (`x = vec.insert(x, i, v)`): small,
   constant-factor only; worth doing with (3) or if insert-heavy
   workloads appear.
5. Full-e2e ASan ran with `detect_leaks=0`; a leak-free-exit pass
   would need runtime teardown work first (pre-existing).

## 9. Assumptions

- Previous numbers = `docs/perf/baseline.md` + `/tmp/before_main_release.md`
  (main, release); fixed ~160 ms startup overhead excluded from
  scaling claims; some after-numbers measured under concurrent build
  load (flat speedups of 10–1000× make load noise immaterial, and
  key scalings were re-verified quiet).
- No merge performed; branch `perf/move-append-elision` only.

