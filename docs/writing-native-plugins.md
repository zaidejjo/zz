# Writing Native Plugins: End-to-End Tutorial

Build a real native plugin from scratch: `fnv`, a pure-Rust FNV-1a hash
exposed to ZZ on **both** engines (`zz run` via dlopen, `zz build` via
static link). No C, no system libraries — every step runs on Linux and
macOS with just `zz`, `cargo`, and a C toolchain.

This is the task companion to the reference
[`plugin-author-guide.md`](plugin-author-guide.md) (interface format,
type rules, platform matrix). When this tutorial says "why", the guide
section linked next to it has the full rule.

Prereqs: `zz` on PATH, `cargo`, `clang`/`cc` (AOT verification only).

---

## 1. Scaffold the package

```
fnv/
├── plugin.zzi      interface declarations (the contract)
├── zz.toml         package manifest with [native]
├── build.sh        build hook → build/*.so + *.a + flags
├── native/         Rust crate (VM glue + AOT staticlib)
│   ├── Cargo.toml
│   └── src/lib.rs
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
// Rustc: <your `rustc --version`, e.g. 1.97.1>
// Plugin-version: 0.1.0

extern "C" {
    func fnv_hash(s: str) -> int
    func fnv_seed() -> int
}
```

The `Rustc` stamp records the building toolchain for diagnostics; the
refusal the loader actually enforces at dlopen is the `ZZ_PLUGIN_ABI_VERSION`
stamp (mismatch → clear `ABI version mismatch` error, no registration,
no crash — verified adversarially, see §7).

`str` is param-only, never a return (guide §7). `int` crosses as
`i64`/`int64_t` on both engines. Keep the `// Rustc:` stamp accurate:
the loader refuses ABI mismatches (guide §11).

## 3. Write the native crate (`native/`)

```toml
# native/Cargo.toml
[package]
name = "fnv_native"
version = "0.1.0"
edition = "2021"

[lib]
crate-type = ["cdylib", "staticlib"]
# cdylib  → build/libfnv_native.so  (VM dlopen target)
# staticlib → build/libfnv_native.a (AOT link target)

[dependencies]
# Git dependency on the zz toolchain repo (zz_runtime is a workspace
# crate, unpublished on crates.io — a version requirement cannot
# resolve). Pinned to a tag so anyone cloning this tutorial reproduces
# the exact tested tree; no local directory layout assumed.
zz_runtime = { git = "https://github.com/zaidejjo/zz", tag = "tutorial/fnv-pin" }
```

```rust
// native/src/lib.rs
use std::ffi::{CStr, CString};

/// Must match `CURRENT_ABI_VERSION` in `zz_plugin::loader`.
#[no_mangle]
pub static ZZ_PLUGIN_ABI_VERSION: u32 = 1;

fn fnv1a(bytes: &[u8]) -> i64 {
    let mut h: u64 = 0xcbf29ce484222325;
    for b in bytes {
        h ^= *b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    h as i64
}

// Raw C-ABI layer. AOT links these symbols straight from the staticlib,
// so they must exist with exactly the `plugin.zzi` C names (`fnv_hash`,
// not `native_fnv_hash`) — without them the AOT link fails with
// `undefined reference`. The VM glue below calls the same functions:
// one implementation, both engines.

/// Hash a NUL-terminated string. Never retains the pointer.
#[no_mangle]
pub extern "C" fn fnv_hash(s: *const std::ffi::c_char) -> i64 {
    if s.is_null() {
        return 0;
    }
    let s = unsafe { CStr::from_ptr(s).to_string_lossy() };
    fnv1a(s.as_bytes())
}

/// The FNV offset basis (lets ZZ code seed its own pipelines).
#[no_mangle]
pub extern "C" fn fnv_seed() -> i64 {
    0xcbf29ce484222325u64 as i64
}

// VM glue: convert Value <-> C types, then call the raw layer above.

type Interp = zz_runtime::eval::Interp;
type Value = zz_runtime::Value;

fn native_fnv_hash(
    _interp: &mut Interp,
    args: &mut Vec<Value>,
    span: zz_runtime::Span,
) -> Result<Value, zz_runtime::EvalError> {
    let err = |msg: String| zz_runtime::EvalError::new(msg, span);
    let s = match &args[0] {
        // Value::Str is Box<String>; borrow, never move, out of args.
        Value::Str(s) => s.as_str(),
        other => return Err(err(format!("expected str, got {other:?}"))),
    };
    // Rust strings are not NUL-terminated: copy through CString and
    // reject interior NULs rather than truncating silently.
    let cs = CString::new(s).map_err(|_| err("interior NUL".to_string()))?;
    Ok(Value::Int(fnv_hash(cs.as_ptr())))
}

fn native_fnv_seed(
    _interp: &mut Interp,
    _args: &mut Vec<Value>,
    _span: zz_runtime::Span,
) -> Result<Value, zz_runtime::EvalError> {
    Ok(Value::Int(fnv_seed()))
}

// The allow belongs on BOTH items: the warning fires on the alias
// definition as well as the function using it.
#[allow(improper_ctypes_definitions)] // NativeFn is an extern-C fn pointer; safe here
type RegisterCallback = extern "C" fn(name: *const i8, arity: usize, f: zz_runtime::NativeFn);

#[no_mangle]
#[allow(improper_ctypes_definitions)]
pub extern "C" fn zz_plugin_register(callback: RegisterCallback) {
    let reg = |name: &str, arity: usize, f: zz_runtime::NativeFn| {
        let name = CString::new(name).unwrap();
        callback(name.as_ptr(), arity, f);
    };
    // Names AND arities must match plugin.zzi exactly.
    reg("fnv_hash", 1, native_fnv_hash);
    reg("fnv_seed", 0, native_fnv_seed);
}
```

`Value` is not `Copy`: match by reference (guide §14). Returning
`Value::Int`/`Float`/`Bool` is free; constructing `Value::Str` needs
`Box::new`.

## 4. Write the build hook (`build.sh`)

```sh
#!/bin/sh
# build.sh — artifacts the CLI consumes (guide §4).
set -e
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
BUILD_DIR="$SCRIPT_DIR/build"
mkdir -p "$BUILD_DIR"

cd "$SCRIPT_DIR/native"
# cdylib with plugin link discipline: localize archive symbols so dead
# host-runtime references GC away; lazy bind since the host never
# provides C runtime symbols. Real missing deps still fail via DT_NEEDED.
cargo rustc --release --lib --crate-type cdylib -- \
    -C link-args=-Wl,--exclude-libs,ALL \
    -C link-args=-Wl,-z,lazy
SO=$(find target/release/deps -maxdepth 1 -name 'libfnv_native.so' \
    -newer Cargo.toml | head -1)
cp "$SO" "$BUILD_DIR/"
cargo build --release
cp target/release/libfnv_native.a "$BUILD_DIR/"

# No system deps: files must exist, contents may be empty.
: > "$BUILD_DIR/cflags.txt"
: > "$BUILD_DIR/ldflags.txt"
echo "Static: build/libfnv_native.a (AOT)  Shared: build/libfnv_native.so (VM)"
```

Cargo does **not** see C sources changed outside its fingerprint: if you
later add a `csrc/` dir, force the crate rebuild when C files are newer
than compiled objects (`touch native/build.rs` in `build.sh` — the
staleness guard; without it you will debug a stale `.so` for an hour).

Checkpoint: `./build.sh` exits 0 and `build/` holds `.so`, `.a`,
`cflags.txt`, `ldflags.txt`.

## 5. Manifest + ZZ entry

```toml
# zz.toml
[package]
name = "fnv"
version = "0.1.0"

[native]
build = "build.sh"
```

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
zz install                                # links vendor/, runs build.sh
```

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
zz build src/main.zz && ./src/bin/main   # AOT: static link
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

(`zz run` additionally logs `zz: loaded plugin 'fnv'` on stderr.)
Assert final-state invariants in the consumer (handles at zero, files
on disk correct) the way `zimg` asserts `live_handles() == 0`: the
3-call smoke test is not enough for real chains; exercise your longest
realistic pipeline before publishing.

Adversarial check (do this once per plugin): rebuild the cdylib with a
wrong `ZZ_PLUGIN_ABI_VERSION`, swap it into `build/`, and `zz run`.
Expect exit 1 with `ABI version mismatch (expected 1, got …)` and no
registration — never a crash. Then rebuild clean and re-verify.

---

## Graduating: adding C / system libraries (the `zimg` map)

When pure Rust isn't enough (libvips, sqlite, …), the shape stays the
same; only the native crate grows a `csrc/` + `build-dependencies`:

- `native/build.rs` compiles `csrc/*.c` with `cc` + `pkg-config` flags;
  keep the staleness guard from §4 — it exists because of this case.
- Rust declares `extern "C"` fns matching C symbols; VM glue converts
  `Value ↔ C types` (handles as `Value::Int` ↔ `void*`, `str` via
  `CString`, never retained past the call).
- Single-owner discipline for handles: each producing call overwrites
  one result slot — consume via `get_result()` exactly once, release
  the previous handle on success, never touch the slot on failure.
  Ship a `live_handles()` counter and assert zero in tests; run the
  longest chain under ASan/LSan, not just the smoke test (zimg's 6-op
  chain caught a same-process stale-cache bug the 3-call test hid:
  libvips caches loaders by filename, so same-second overwrites
  re-load stale pixels — disabled via `vips_cache_set_max(0)`).
- `zz build` links `.a` (+ `ldflags.txt` system libs); `zz run`
  dlopens `.so`, checks the ABI stamp, calls `zz_plugin_register`.

## Troubleshooting

| Symptom | Cause | Pointer |
|---|---|---|
| `cannot read imported file …/fnv.zz` | skipped `zz add`/`zz install`; no `vendor/` link | §6 |
| VM: `unknown native fnv_hash` | name/arity mismatch in `reg(...)` vs `.zzi` | §3, guide §1 |
| AOT link: `undefined reference to fnv_hash` | `.a` stale or `build.sh` didn't rerun; C symbol ≠ ZZ name | §4 |
| `method call` error on `fnv.raw.hash` | raw names must be flat; 3-part paths are method-only | §2 |
| Old binary after rebuilding the plugin's `.so`/`.a` | covered automatically: the cache key hashes linked artifact bytes + flags content, so a real rebuild busts; `rm -rf ~/.zz/cache` only needed if you suspect staleness anyway | §4 |
| Old binary after editing compiler sources | covered automatically: key includes compiler-source mtimes (verified: a `touch` forces a new entry); same-second edits are the residual gap — clear cache to be sure | — |
| Works VM, fails AOT (or reverse) | engine-specific lowering/dispatch; minimize to one call, compare | §7 |
| Windows: VM won't load | dlopen path unimplemented; ship AOT only | guide §12 |

## Pre-publish checklist

- [ ] `build.sh` green from a clean checkout (`rm -rf build native/target`)
- [ ] Throwaway consumer passes on **both** engines with identical output
- [ ] Longest realistic chain exercised (not just one call), resources at zero
- [ ] `plugin.zzi` header stamps (`Version`, `Rustc`, `Plugin-version`) current
- [ ] `plugin-author-guide.md` §14 gotchas re-read once more
