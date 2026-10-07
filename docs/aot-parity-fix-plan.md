# AOT parity fix plan — nested `[[str]]` corruption + UTF-8 string measure

Branch: `fix/aot-nested-array-utf8-parity` (branched from `dev` @ `8604635`)
Status: FIXED + VERIFIED — see §7. Implementation committed on this branch.
Toolchain verified: `zz 0.1.6`, `clang 22.1.8`, Linux x86_64, repo `/home/zaid/Projects/zz_lang`.

## 0. Repro status on this branch (2026-10-03)

| repro | VM (`zz run`) | native (`zz run --native` / `build -p` binary) |
|---|---|---|
| `mini1.zz` (struct `T{rows:[[str]]}` + 2× `vec.push`) | `Alice/25/Bob/30/mini1_ok` | **PASSES here** at `-O0` and `-O3 -flto=thin` (both local `target/debug/zz` and installed `~/.cargo/bin/zz`) |
| `mini2.zz` (plain local `[[str]]` + 2× `vec.push`) | `Alice/25/30/mini2_ok` | **PASSES here** at `-O0` and `-O3` |
| `stress.zz` (struct + `mkrow()` + `for range(0,5)` push) | passes | passes |
| `mini4.zz` (`len("╭─╮")`, `str.length`, `s[0]`, `s[1]`) | `3/3/╭/─` | **FAILS here**: `9/9/�/�` — bug 2 CONFIRMED |

Conclusion: bug 2 is a hard AOT/VM divergence, reproducible with a 4-liner.
Bug 1 does **not** reproduce with the minimal 2-push fixtures on current `dev`
HEAD — the recent `move-append-elision` (`386ada7`, `175bd72`, `8604635`) and
`aot-container-temp-release` (`2ed5032`/`cbdc780`) merges are the prime
suspects and the table-pkg demo (`rounded + title/footer + std.colors +
markdown`, row-major `[[str]]` + widen + re-stride) is the real trigger.
Phase 0 below fetches that trigger before touching codegen.

Repro files used (kept in `/tmp/repro/`, will become fixtures in Phase 0):

```zz
// mini1.zz
import std.vec
struct T { rows: [[str]] }
func main() {
  t := T{ rows: [] }
  t.rows = vec.push(t.rows, ["Alice", "25"])
  t.rows = vec.push(t.rows, ["Bob", "30"])
  println(t.rows[0][0]) // native bug report: <value>, expect Alice
  println(t.rows[0][1])
  println(t.rows[1][0])
  println(t.rows[1][1])
  println("mini1_ok")
}
```

```zz
// mini4.zz
import std.str
func main() {
  println(len("╭─╮"))         // VM 3, native 9
  println(str.length("╭─╮"))  // VM 3, native 9
  s := "╭─╮"
  println(s[0])  // VM ╭, native �
  println(s[1])  // VM ─, native �
}
```

## 1. Bug 2 (major, confirmed) — AOT counts bytes, VM counts chars

### 1.1 Contract (VM is source of truth)

- `crates/zz_stdlib/src/natives/iterators/mod.rs::len`: `Value::Str(s) → s.chars().count()`.
- `crates/zz_stdlib/src/natives/str_mod/mod.rs::str_length`: `s.chars().count()`.
- `crates/zz_runtime/src/runtime/ops.rs::get_index`: `s.chars().collect()[idx]` (char-indexed, negative-aware via `normalize_index`).
- `crates/zz_runtime/src/runtime/ops.rs::slice_value`: char-sliced (`chars[a..b]`).
- Docs say `str` is UTF-8; `len`/`str.length`/indexing must agree across engines.
- Pure ASCII unaffected (bytes == chars), which is why this hid until box-drawing/CJK.

### 1.2 Current AOT behavior (all byte-based)

| location | behavior |
|---|---|
| `crates/zz_codegen/src/runtime/strings.h::zz_str_get` (inline) | `s->len` as count, `zz_str_new(ptr+i, 1)` — byte offset, 1-byte slice. Comment even admits "established engine difference". |
| `crates/zz_codegen/src/runtime/strings.c::zz_str_length` | returns `s->len` (bytes). Comment says "in bytes". |
| `crates/zz_codegen/src/runtime/collections.c::zz_len` + `zz_len_field` + `zz_vec_len` | `s->len` bytes for `ZZ_STR`. |
| `crates/zz_codegen/src/runtime/collections.c::zz_slice_value` (`ZZ_STR` arm) | byte `si/ei` + `zz_str_new(ptr+si, ei-si)` — slices mid-codepoint. |
| `zz_str_split` empty-sep arm | `for i in 0..len → 1-byte items` — must become per-char. |

### 1.3 Fix design (C runtime only, no codegen shape change)

Add UTF-8 helpers in `strings.h`/`strings.c` (static inline + tested):

```c
// returns Unicode scalar count (chars), not bytes.
size_t zz_str_chars(const zz_str *s);
// byte offset of char index `i` (negative already normalized by caller).
// returns (size_t)-1 when out of range.
size_t zz_str_byte_off(const zz_str *s, int64_t char_idx);
// byte length of the codepoint starting at byte offset `off`.
size_t zz_utf8_seq_len(unsigned char lead);
```

Rules:
- Count with the standard UTF-8 lead-byte table (`0xxxxxxx→1`, `110xxxxx→2`, `1110xxxx→3`, `11110xxx→4`), continuation bytes `10xxxxxx` never start a char.
- Invalid bytes (lone continuation, truncated sequence, overlong — keep it simple): treat as 1 single-byte char (never crash, never loop forever; matches Rust `chars()` lossy-ish fallback closely enough for parity tests, document in comment).
- `zz_str_get`: normalize negative against **char** count, map char idx → byte off → seq len → `zz_str_new(ptr+off, seq_len)`.
- `zz_str_length` / `zz_len` / `zz_len_field` (`ZZ_STR` arms): return `zz_str_chars`.
- `zz_slice_value` (`ZZ_STR` arm): normalize `si/ei` against char count, map both to byte offsets (`ei==nchars → s->len`), then `zz_str_new`.
- `zz_str_split` empty-sep: iterate by codepoint, not byte.
- Audit-only (no change, add comment): `contains`/`replace`/`startswith`/`endswith`/`join`/`split(sep≠"")` are byte-`memcmp` — correct for valid UTF-8 (substring byte match ⟺ char-boundary match when both sides are valid UTF-8). `trim`/`lower`/`upper` are ASCII-only today on **both** engines for these paths (VM `trim` is Unicode-aware — check `str_trim` parity separately; `lower`/`upper` VM uses `to_lowercase`/`to_uppercase` which is Unicode-aware vs AOT ASCII-only — file as follow-up, out of scope for this fix except docs).
- `zz_str_view`/FFI stays byte-based (bytes + len pair, no semantic change).

Performance: char counting is O(n). `len()` in a loop over a large string becomes O(n²) worst-case — same as VM today (`s.chars().count()`), so parity first, optimize later (cache char count in header as follow-up, not this branch).

Security: all new loops bound by `s->len`, no NUL reliance (`sso`/`heap` may embed NUL — use `len`, never `strlen`). No new `malloc` failure paths except existing `zz_str_new` (already exits on OOM).

### 1.4 Bug 2 tests

- New e2e fixtures (register with `e2e_success_test!` in `crates/zz_cli/tests/e2e.rs`, follow `tests/fixtures/{syntax,types,stdlib}/` conventions — each prints trailing success marker):
  - `tests/fixtures/stdlib/str_utf8_len.zz` — `len` + `str.length` on `"╭─╮"`, `"héllo"`, `"日本語"`, `""`, ASCII control; asserts `3/5/3/0`.
  - `tests/fixtures/stdlib/str_utf8_index_slice.zz` — `s[0]/s[1]/s[-1]`, `s[0:2]`, OOB → error path, negative slice.
  - `tests/fixtures/stdlib/str_utf8_split.zz` — `str.split(s, "")` yields per-char array (VM parity).
- New Rust unit tests:
  - `crates/zz_codegen/src/tests.rs` (or new `utf8` module): lower `mini4` and assert emitted C calls char helpers (snapshot the `zz_str_get`/`zz_len` call shape, not full C).
  - C-level: if repo has a C harness pattern, add `zz_str_chars` table test (`╭` = 3 bytes/1 char, `─` = 3 bytes, mixed ASCII+CJK, invalid byte `0xFF` → 1 char, empty).
- Dual-engine parity: extend `crates/zz_cli/tests/dual_engine_parity.rs` with the utf8 fixtures (VM leg + `--native` leg must both print `3/3/╭/─`).

## 2. Bug 1 (critical, needs trigger) — nested `[[str]]` first-element corruption + SIGSEGV

### 2.1 What we know

- VM tree-walker correct; AOT wrong. Flat `[str]` struct fields verified clean on both engines → not `vec.push` alone, not struct layout alone; points at **inner-array value handling** (box/unbox, slice header, retain/store).
- Downstream: first-row cells garbage (`Alice→A`, `25→G`) then `signal 11`. `<value>` on `println(t.rows[0][0])` means the loaded `zz_value.tag` hit the `default:` printer arm — a corrupted tag, i.e. use-after-free / unretained temp / buffer overwrite, not a wrong-string bug.
- Minimal 2-push repro passes on `dev` HEAD at `-O0` and `-O3 -flto=thin` → the corruption needs more pressure (loop arena resets, `refs==1` in-place take, dup sharing, or struct-field path). Recent merges touched exactly this:
  - `386ada7`/`175bd72`/`8604635` move-on-self-reassign (`zz_vec_push_take`, `zz_object_push_field_take`, `MOVED_TYPE`, `takeable_slot`).
  - `cbdc780` container-temp release (`append_container_item` releases `emit_boxed_value` temps).
  - `0795339` unboxed-struct boxing in array/tuple literals.

### 2.2 Suspect list (in priority order, with files/lines)

1. `zz_vec_push_take` / `zz_object_push_field_take` in `collections.c:899-960` + `lower/move_elide.rs:172-302`:
   - In-place gate is `arr->refs == 1`. But struct clones (`zz_clone`/`zz_retain_object`) bump only the **object header**, never field buffers (comment in code admits this), while `zz_release_object` frees field buffers. So a cloned struct can share a field array at `refs==1` → in-place `zz_array_push` mutates a buffer still aliased by the sibling → first element overwritten / double-free → SIGSEGV. The code comment already flags "tracked as follow-up (see move_append_struct_copy known-failure)". Nested `[[str]]` widens this: outer dup is shallow (`zz_array_dup` clones `zz_value`s by bumping inner array `refs`, but inner `zz_array` headers stay shared) — verify the inner `refs` accounting on the `take` path.
   - `zz_object_push_field_take` fallback does `get+push+release(slot)+*slot=n` — check missing `zz_release(&cur)`/double-release ordering when `cur` aliases `slot`.
2. Array literal construction in `lower/expr.rs:900-964` + `append_container_item:3316-3350` + `context.rs:try_emit_stack_array/emit_scalar_init`:
   - `["Alice","25"]` is a non-scalar literal → `zz_array_new[_arena_sized]` + per-item `zz_vec_append` with `zz_clone_for_store`. If the literal was arena-allocated (`arena_for(span)` non-NULL inside a loop/function with arena reset) and then stored into an escaping outer array without `zz_str_heal_arena`/`zz_clone_for_store` on the **array value itself** (healing today covers strings only), the next `zz_arena_reset` reuses the inner header/items → first-element garbage. Check: does `zz_clone_for_store` handle `ZZ_ARRAY` with arena sentinel `refs==0`/`LIT_MAGIC`/`ARENA_MAGIC`? Today it only heals `ZZ_STR`.
   - `zz_array_push_lit` stores without clone — only valid for scalar inits; confirm no string/inner-array literal ever takes that path (guard is `all_scalar`, but re-verify after `emit_scalar_init` changes).
   - `Index` lowering `expr.rs:765-789`: `Ident` receivers pass **borrowed** (no clone) to `zz_index_get`, which returns `zz_clone(item)`. For `rows[0][0]` the outer `zz_index_get(rows,0)` result is an rvalue temp; the inner `[0]` then borrows that temp — lifetime is the full expression, OK, but check the generated temp scoping when chained as `t.rows[0][0]` (field-get temp + two index temps + `println` arg). Dump the C.
3. Retain/release in `core.c` (`zz_clone`/`zz_release`/`zz_assign`, `zz_release_array`) + `memory.c` arena (`zz_arena_reset/destroy`):
   - `zz_array_dup` shallow-clone bumps inner refs — confirm every bump has a matching release on overwrite (`zz_array_set`, `zz_object_set_field`, `zz_vec_pop/remove/insert`).
   - `zz_release_array` must skip `STACK_MAGIC`/`LIT_MAGIC`/arena headers but still release items appropriately — a missed item-release leaks; a double item-release corrupts the first element first (lowest address, first freed).
4. Struct field paths: `emit_struct_init`, boxed `zz_object_new/set/get_field` (`collections.c:610-729`), unboxed `zz_struct_*` in `fn_decl.rs:272+`, and `zz_len_field` (它 borrows without retain — confirm no path stores that borrowed pointer).

### 2.3 Phase 0 — get the failing trigger (do NOT skip)

1. Fetch the table pkg demo from the bug report (`table/examples/demo.zz`: rounded + title/footer + `std.colors` + markdown) into `/tmp/table-demo/` (not into the repo).
2. `zz run` vs `zz run --native` (and `zz build -p` + binary) — confirm `Alice→A / 25→G` + SIGSEGV natively.
3. `git bisect` between `v0.1.6` tag and `dev` HEAD if demo fails on one but not the other; else reduce: `creduce`-by-hand — delete styles/colors/markdown/widen until minimal failing file (goal: ≤30 lines, still `[[str]]` + struct + loop or function boundary).
4. Dump the failing C: `zz build --verbose` keeps the TU (see `crates/zz_cli/src/build.rs:966` `lower_only` + `emit_c_plus_script`); capture the `t.rows = …push…` + `rows[0][0]` region and attach to the fix PR.
5. Only then choose the fix below (A/B/C) — do not "fix" the minimal passing repro.

### 2.4 Fix candidates (pick after Phase 0, in this order)

- **A (most likely): heal/clone arrays on retaining stores.** Extend `zz_clone_for_store` (or add `zz_clone_array_for_store`) to dup arena/stack/lit arrays on store into escaping containers (`zz_vec_append/push`, `zz_dict_set`, `zz_object_set_field`, `zz_array_set`), mirroring the existing string-heal. Guard: only when `refs` is a sentinel (`0`, `STACK/LIT/ARENA_MAGIC`); heap `refs>=1` keeps sharing + bump.
- **B: tighten the `refs==1` in-place gate.** Make field-buffer sharing exact: either deep-bump field arrays on struct clone (costly — measure), or add COW on field write (check `slot->arr->refs` **and** object alias count), or disable `zz_object_push_field_take` in-place when the base object `refs > 1`. Smallest safe change: fall back to `get+push+set` whenever the base object is shared.
- **C: fix temp lifetime in chained `a[i][j]` / `t.f[i][j]` lowering.** Hoist each `zz_index_get` into a named temp with explicit `zz_release` after use (the `Index` fast path currently emits nested calls inline — confirm the inner temp outlives the outer under `-O3 -flto=thin` + `__atomic` refcounts).

All three keep the `vec.push` copy-on-write contract (VM: input untouched) — add an assertion test for it.

### 2.5 Bug 1 tests

- Reduced fixture(s) from Phase 0 as `tests/fixtures/stdlib/vec_nested_str_push.zz` (+ struct variant `struct_nested_str_push.zz`), each printing all cells + trailing `_ok` marker, registered in `e2e.rs`.
- `dual_engine_parity.rs`: add nested-array cases (2-push, loop-push × N, struct-field, function-returned inner array, `x = vec.push(x, …)` self-reassign spelling + `x.push(…)` method spelling).
- ASan/UBSan gate (local, not CI): `clang -fsanitize=address,undefined` build of the reduced fixture must be clean; run the binary under `valgrind --tool=memcheck` if ASan is inconclusive.
- Existing known-failure: `move_append_struct_copy` — re-run; this fix should close it or document why not.

## 3. Execution phases

- **Phase 0 — harness (0.5d):** table demo fetched, native failure confirmed, reduced fixture, C dump attached, `ZZ_PARITY_VM_ONLY=1` baseline green.
- **Phase 1 — Bug 2 runtime (1d):** UTF-8 helpers + `zz_len`/`zz_str_length`/`zz_str_get`/`zz_slice_value`/`zz_str_split("")` + unit tests. No codegen changes. Verify `mini4` → `3/3/╭/─` on `--native`.
- **Phase 2 — Bug 1 root-cause + fix (2-3d):** Phase 0 reduction → pick A/B/C → implement in `collections.c` (+ `move_elide.rs`/`expr.rs` if lowering) → ASan clean → `mini1/mini2` + reduced fixture green at `-O0` and `-p`.
- **Phase 3 — parity suite (0.5d):** new fixtures registered, `dual_engine_parity` extended, full demo `zz run` vs `zz run --native` byte-identical, exit 0, no SIGSEGV.
- **Phase 4 — gate (0.5d):** `./scripts/test-fast.sh`, then `cargo test --all` if time allows, plus `cargo clippy --all-targets -- -D warnings` and `cargo fmt --check`. Update `docs/stdlib.md` (`len`/`str.length`/indexing = chars) and remove/refresh the "established engine difference" comments in `strings.h`.

## 4. Suggested verification (from the bug report — Definition of Done)

1. `mini1`/`mini2` print all four cells correctly with `--native`, no `<value>`.
2. `mini4` prints `3 / 3 / ╭ / ─` with `--native`.
3. Full demo (`table/examples/demo.zz`: rounded + title/footer + `std.colors` + markdown) renders identically under `zz run` and `zz run --native`, exit 0, no SIGSEGV.
4. Suite stays green: `./scripts/test-fast.sh` (or `cargo test --all` if time allows) + `cargo clippy --all-targets -- -D warnings`, `cargo fmt --check`.

## 5. Files to touch (expected)

- `crates/zz_codegen/src/runtime/strings.h` — UTF-8 helpers + `zz_str_get` char-indexed.
- `crates/zz_codegen/src/runtime/strings.c` — `zz_str_length`, `zz_str_split("")`, helpers.
- `crates/zz_codegen/src/runtime/collections.c` — `zz_len`, `zz_len_field`, `zz_slice_value` (`ZZ_STR` arms); Bug 1 fix in `zz_vec_push_take`/`zz_object_push_field_take`/`zz_clone_for_store` (+ array-store healing).
- `crates/zz_codegen/src/lower/expr.rs` — only if Phase 0 shows a lowering lifetime bug (`Index` chain temps, `append_container_item` arena healing).
- `crates/zz_codegen/src/lower/move_elide.rs` — only if Phase 0 shows the `refs==1` gate is unsound (gate tightening).
- Tests: `crates/zz_cli/tests/e2e.rs` + `tests/fixtures/stdlib/str_utf8_*.zz` + `vec_nested_str_push.zz`, `crates/zz_cli/tests/dual_engine_parity.rs`, `crates/zz_codegen/src/tests.rs`, `crates/zz_stdlib/src/natives/tests.rs::str_length_counts_chars` (extend with UTF-8).
- Docs: `docs/stdlib.md`, comments in `strings.h`/`strings.c`/`collections.c`.

## 6. Non-goals / follow-ups

- Unicode `lower`/`upper`/`trim` parity (VM Unicode-aware vs AOT ASCII-only) — document, fix separately.
- Char-count caching in `zz_str` header (perf) — only if benchmarks regress.
- Table-pkg flat workaround (`cells:[str]` row-major + `nrows/ncols`) stays valid regardless; this branch removes the need for it on native.
- Chained index *stores* (`t.rows[0][0] = x`): VM drops the write into a temp
  clone (read-back shows the old value), native writes through. Pre-existing
  lowering-level divergence, untouched by this fix; new fixtures deliberately
  cover reads only. Fix separately (either engine converging needs a
  semantics decision + its own parity tests).

## 7. Fix record (implemented on this branch)

### Bug 2 — UTF-8 measure (VM parity restored)
- `runtime/strings.h`: new `zz_utf8_seq_len` / `zz_str_char_len` /
  `zz_str_char_byte_off` helpers; `zz_str_get` rewritten char-indexed
  (negative-aware, OOB → unit + err). Invalid bytes count as one
  single-byte char each (never crash/loop).
- `runtime/strings.c`: `zz_str_length` returns chars; `zz_str_split` empty
  separator emits per-char items AND matches Rust `split("")` exactly
  (`"ab"` → `["", "a", "b", ""]`, `""` → `["", ""]`).
- `runtime/collections.c`: `zz_len` / `zz_len_field` (`ZZ_STR` arms) return
  chars; `zz_slice_value` (`ZZ_STR` arm) normalizes + maps char bounds to
  byte offsets. Byte-internal ops (concat/compare/print/hash) untouched.
- Verified: `mini4` → `3/3/╭/─` on `--native` (`-O0` and `-p -O3`);
  extended edge fixture (negative index, slices, empty/ASCII, split empties)
  byte-identical VM vs native.

### Bug 1 — nested `[[str]]` arena aliasing (SIGSEGV fixed)
- Reproduced on `dev` HEAD: struct-field + loop push (`loopfield.zz`)
  SIGSEGVs natively while VM passes. Root cause: inner literals use
  `zz_array_new_arena_sized` (header + items on the loop arena); retaining
  stores only healed strings, so the outer heap array aliased arena memory
  and `zz_arena_reset` overwrote it (first-element garbage → tag corruption
  → `<value>` → SIGSEGV). Minimal 2-push repros pass because straight-line
  temps are never released/reset.
- `runtime/collections.{h,c}`: `zz_array_is_arena` / `zz_dict_is_arena`
  predicates; `zz_heal_for_move` (move convention: copy arena incl. deep
  nested + wrapper lookthrough, adopt heap) and extended
  `zz_clone_for_store` (share convention: copy arena, `zz_clone` heap);
  depth-capped at 32. Applied at every ingress: `zz_array_push`,
  `zz_vec_append`/`push`/`insert`, `zz_dict_set`, `zz_object_set_field`,
  `zz_assign` (skips the retain when it healed — fresh-owned needs none).
  Ownership conventions per site preserved (share sites still bump, move
  sites still adopt — no new leaks).
- Hardening in the same area: `zz_array_push` + `zz_vec_insert` growth now
  migrates `ZZ_ARRAY_ARENA_MAGIC` buffers to malloc before realloc
  (`zz_vec_append` already did; realloc-on-arena is heap corruption).
- Verified: `loopfield` + `tablelike` (10-row struct loop) + `mini1/mini2`
  identical VM vs native at `-O0` and `-p`; ASan+UBSan clean (no UAF/UB;
  only pre-existing exit-time leaks, same as baseline).

### Tests added
- `tests/fixtures/stdlib/str_utf8_parity.zz` + `vec_nested_str_parity.zz`
  (plain + struct + loop shapes, read-only), registered strict in both
  `crates/zz_cli/tests/e2e.rs` and `dual_engine_parity.rs`.
- Gates: `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`
  clean; `./scripts/test-fast.sh` green (incl. full dual-engine parity).
  Note: `zz_lsp cross_file::parse_file_entry_performance` flaked once
  under parallel clang load (66ms vs threshold) and passes in isolation
  (0.02s) — unrelated crate, no shared code with this fix.
