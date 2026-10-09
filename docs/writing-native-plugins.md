# Writing Native Plugins: End-to-End Tutorial

Build a real native plugin from scratch: `fnv`, a pure-C FNV-1a hash
exposed to ZZ on **both** engines (`zz run` via dlopen, `zz build` via
static link). No scripts, no Rust, no system libraries — `zz` compiles
the C itself from a declarative manifest. Every step runs on Linux and
macOS with just `zz` and a C toolchain.

This is the task companion to the reference
[`plugin-author-guide.md`](plugin-author-guide.md) (interface format,
type rules, platform matrix). When this tutorial says "why", the guide
section linked next to it has the full rule.

Prereqs: `zz` on PATH, `cc`/`clang`.

---

## 1. Scaffold the package

```
fnv/
├── plugin.zzi      interface declarations (the contract)
├── zz.toml         package manifest with [native.build-cc]
├── csrc/
│   └── fnv.c        C implementation (compiled by zz, never by you)
└── src/
    └── fnv.zz      ergonomic ZZ entry (`import fnv`)
```

`import fnv` loads `src/fnv.zz` as a module **and** merges the
`plugin.zzi` signatures, so `fnv.hash(...)` resolves alongside the raw
declarations (guide §2).

## 2. Declare the interface (`plugin.zzi`)

Raw names stay **flat** — free-function calls resolve one- and two-part
paths only; a three-part path is always a method call. So the raw layer
is `fnv_hash`, never `fnv.raw.hash`:

```
// Version: 1
// C-ABI: 1
// Plugin-version: 0.1.0

extern "C" {
    func fnv_hash(s: str) -> int
    func fnv_seed() -> int
}
```

Pure-C plugins carry the `// C-ABI: 1` header (no `// Rustc:` line —
there is no Rust toolchain involved). The loader resolves symbols
directly with dlsym; the refusal it enforces is the
`ZZ_C_PLUGIN_ABI_VERSION` stamp you export from C (mismatch → clear
error, no registration, no crash).

`str` is param-only, never a return (guide §7). `int` crosses as
`i64`/`int64_t` on both engines. AOT lowers `str` params to borrowed
`const char *` — never retain the pointer.

## 3. Write the C source (`csrc/fnv.c`)

```c
// csrc/fnv.c — one implementation, both engines. AOT links these
// symbols straight from the static archive; the VM dlsyms the same
// names from the shared lib. C names must equal the plugin.zzi names
// with `.` replaced by `_` (or an explicit `= "..."` override).

// ABI stamp — the loader refuses to register without it.
const unsigned int ZZ_C_PLUGIN_ABI_VERSION = 1;

static long long fnv1a(const char *s) {
    unsigned long long h = 0xcbf29ce484222325ULL;
    if (s) {
        while (*s) {
            h ^= (unsigned char)*s++;
            h *= 0x100000001b3ULL;
        }
    }
    return (long long)h;
}

/// Hash a NUL-terminated string. Never retains the pointer.
long long fnv_hash(const char *s) { return fnv1a(s); }

/// The FNV offset basis (lets ZZ code seed its own pipelines).
long long fnv_seed(void) { return (long long)0xcbf29ce484222325ULL; }
```

`Value::Str` arrives as `Value::Str(Box<String>)` on the VM side and
as `zz_str_cptr(v.s)` (borrowed `const char *`) on the AOT side — the
glue is generated, not hand-written. Constructing `Value::Str` in
hand-written glue needs `Box::new` (guide §14).

## 4. Declare the declarative manifest (`zz.toml`)

No `build.sh`. `zz` compiles `csrc/` itself (guide §4):

```toml
# zz.toml
[package]
name = "fnv"
version = "0.1.0"

[native]
backend = "cc"
manifest = "plugin.zzi"

[native.build-cc]
sources = ["csrc/fnv.c"]
include_dirs = ["csrc"]
defines = ["NDEBUG"]
cflags = ["-O2", "-Wall", "-Wextra", "-fPIC"]
libs = []
pkg_config = []
targets = ["linux-x86_64-gnu-glibc2.28", "macos-arm64-min11.0"]
```

`cflags` come from the explicit allowlist only (guide §4) — anything
else is a manifest error, not a warning. Paths stay inside the
package; no globs in v1.

Checkpoint: `zz install` in a consumer compiles the plugin and both
verifications pass — every `.zzi` symbol in the archive (AOT) and in
the shared lib (VM), plus the ABI stamp. Delete a function from the C
file and the build refuses loudly (`symbol ... missing`); that refusal
is the adversarial check from the old hook days, now enforced at
build time instead of dlopen.

## 5. ZZ entry

```rust
// src/fnv.zz — the only surface consumers learn.
pub func hash(s: str) -> int {
    fnv_hash(s)
}

pub func seed() -> int {
    fnv_seed()
}

/// Pure-ZZ value-add over raw FFI: combine two hashes (no native code).
/// Folded mod 1000 first: raw FNV values overflow i64 when scaled
/// (ZZ ints trap on overflow — `hash * 31` fails at runtime).
pub func combine(a: str, b: str) -> int {
    (fnv_hash(a) % 1000) * 31 + (fnv_hash(b) % 1000)
}
```

Keep fallible native calls behind `Result` here (`.ok`/`.err`,
`??`, `.expect`) so consumers never touch status codes.

## 6. Consume it (throwaway acceptance project)

```bash
mkdir -p /tmp/fnvcheck/src && cd /tmp/fnvcheck
printf '[package]\nname = "fnvcheck"\nversion = "0.1.0"\n' > zz.toml
zz registry add fnv --path /path/to/fnv   # once per machine
zz add fnv                                # resolves via registry alias
zz install                                # links vendor/, declarative cc build
```

Direct-dependency source builds just work. Transitive consumers of a
source-only plugin need `zz install --allow-source-builds` until the
maintainer ships prebuilts (guide §3.1); legacy-hook plugins need
`--allow-hooks` (direct only).

```rust
// src/main.zz
import fnv

func main() {
    println(fnv.hash("hello"))
    println(fnv.seed())
    println(fnv.combine("a", "b"))
    println("fnv_ok")
}
```

## 7. Verify both engines (required, not optional)

```bash
zz build src/main.zz && ./bin/main   # AOT: static link (project root bin/)
zz run src/main.zz                        # VM: dlopen + register
```

Both must print **identical** output. Any divergence is a bug — in
your glue (arity/name mismatch,calling convention) or, rarely, the
compiler. Expected output (both engines):

```
-6615550055289275125
-3750763034362895579
-20207
fnv_ok
```

(`zz run` additionally logs `zz: loaded plugin 'fnv'` on stderr under
`ZZ_VERBOSE`.)
Assert final-state invariants in the consumer (handles at zero, files
on disk correct) the way `zimg` asserts `live_handles() == 0`: the
3-call smoke test is not enough for real chains; exercise your longest
realistic pipeline before publishing.

Adversarial check (do this once per plugin): delete one function from
`csrc/fnv.c` and `zz install` in the consumer. Expect a hard
`static/shared verification failed ... symbol ... missing` error and no
artifacts accepted — never a silent half-build. Restore and re-verify.

---

## Graduating: system libraries (the `zimg` map)

When pure C isn't enough (libvips, sqlite, …), the shape stays the
same; only the manifest grows a `pkg_config` line:

```toml
[native.build-cc]
sources = ["csrc/zimg_wrapper.c"]
include_dirs = ["csrc"]
defines = ["NDEBUG"]
cflags = ["-O2", "-Wall", "-Wextra", "-fPIC"]
pkg_config = ["vips"]
targets = ["linux-x86_64-gnu-glibc2.28", "macos-arm64-min11.0"]
```

- `zz` resolves `pkg-config --cflags/--libs`, validates every token
  (guide §4), and records versions in `zz.lock` (drift busts the cache).
- Handles as `Value::Int` ↔ `void*`, `str` borrowed (never retained).
- Single-owner discipline for handles: each producing call overwrites
  one result slot — consume via `get_result()` exactly once, release
  the previous handle on success, never touch the slot on failure.
  Ship a `live_handles()` counter and assert zero in tests; run the
  longest chain under ASan/LSan, not just the smoke test (zimg's 6-op
  chain caught a same-process stale-cache bug the 3-call test hid:
  libvips caches loaders by filename, so same-second overwrites
  re-load stale pixels — disabled via `vips_cache_set_max(0)`).
- Never bake absolute paths into the build (no
  `-DZIMG_VIPS_HOME="/home/…"`) — it busts CAS sharing. Resolve data
  paths at runtime (env vars, dlopen search).
- When the plugin works everywhere, ship prebuilts:
  `zz publish --artifact-dir <dir>` verifies each per-tag `.tgz` and
  prints the `[native.prebuilt]` stanza (guide §3.2).
- `zz build` links `.a` (+ `ldflags.txt` system libs); `zz run`
  dlopens `.so`, checks the ABI stamp, dispatches.

Rust-crate plugins stay on the legacy hook until the v2 cargo-vendor
design lands (guide §5).

## Troubleshooting

| Symptom | Cause | Pointer |
|---|---|---|
| `cannot read imported file …/fnv.zz` | skipped `zz add`/`zz install`; no `vendor/` link | §6 |
| VM: `unknown native fnv_hash` | C symbol ≠ ZZ dotted name (dots → underscores) | §3, guide §1 |
| `static/shared verification failed … symbol … missing` | C file doesn't implement every `.zzi` func, or ABI stamp missing | §4 |
| `invalid cflag …` | flag outside the explicit allowlist; use `include_dirs`/`libs`/`defines` | §4, guide §4 |
| `method call` error on `fnv.raw.hash` | raw names must be flat; 3-part paths are method-only | §2 |
| Old binary after rebuilding the plugin's `.so`/`.a` | covered automatically: the cache key hashes linked artifact bytes + flags content, so a real rebuild busts; `rm -rf ~/.zz/cache` only needed if you suspect staleness anyway | §4 |
| Old binary after editing compiler sources | covered automatically: key includes compiler-source mtimes (verified: a `touch` forces a new entry); same-second edits are the residual gap — clear cache to be sure | — |
| Works VM, fails AOT (or reverse) | engine-specific lowering/dispatch; minimize to one call, compare | §7 |
| Windows: VM won't load | dlopen path unimplemented; ship AOT only | guide §12 |

## Pre-publish checklist

- [ ] Declarative manifest validates (`zz install` green from a clean checkout, `rm -rf build`)
- [ ] Throwaway consumer passes on **both** engines with identical output
- [ ] Longest realistic chain exercised (not just one call), resources at zero
- [ ] `plugin.zzi` header stamps (`Version`, `C-ABI`, `Plugin-version`) current
- [ ] No absolute paths baked into flags (CAS-safe); runtime data via env/dlopen
- [ ] Prebuilt tarballs verified (`zz publish --artifact-dir`), stanza pasted into `zz.toml`
- [ ] `plugin-author-guide.md` §14 gotchas + threat model (§4) re-read once more
