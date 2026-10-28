# Plugin Author Guide

ZZ supports native plugins — packages that expose Rust/C implementations to ZZ code via the `extern "C"` interface mechanism. Plugins work in both the AOT compiler (`zz build`) and the VM interpreter (`zz run`).

---

## Quick Start

1. Create your package with `plugin.zzi` + `zz.toml` + native sources
2. Implement the C-ABI interface (C sources or a hermetic Rust crate)
3. Add `[native]` section to `zz.toml`
4. Users consume via `zz add <your-package>`

---

## 1. Plugin Manifest (`plugin.zzi`)

Every plugin ships a `plugin.zzi` file declaring its native interface. The `.zzi` extension stands for "ZZ interface" and uses the same syntax as `extern "C"` blocks in `.zz` files.

### Format

```zz
// plugin.zzi — my-plugin native interface
// Version: 1
// Rustc: 1.85.0
// Plugin-version: 0.1.0

extern "C" {
    func my.init() -> int
    func my.process(handle: int, data: int) -> int
    func my.release(handle: int)
    func my.get_result() -> int
    // Explicit C symbol override (optional):
    func my.add(a: int, b: int) -> int = "my_add_impl";
}
```

Names are ZZ-visible and should be dotted (`my.process`): consumers call
them qualified after `import my`. The C symbol defaults to the ZZ name
with `.` replaced by `_` (`my.process` → `my_process`); `= "..."` overrides
it. Bare short names are never auto-exposed, so two plugins cannot collide.

Signatures use ZZ's `func` keyword, not Rust's `fn` — `fn` is rejected with
an explicit diagnostic. `extern "C"` blocks are parsed by the ZZ parser,
so the rest of the line is ordinary ZZ syntax too (`-> int`, no `;`).

### Metadata Header

The first comment block carries metadata the loader validates:

```
// Version: <manifest-format-version>  (required, currently 1)
// Rustc: <exact rustc version>        (required, for ABI compatibility check)
// Plugin-version: <semver>            (required, user-facing versioning)
```

The `Version` field is the manifest format version. The `Rustc` field records which Rust compiler built the plugin. The loader checks both against expected values before loading. Pure-C and hermetic Rust plugins (`[native.build-rust]`) use `// C-ABI: 1` instead and export `ZZ_C_PLUGIN_ABI_VERSION`; there is no legacy Rust registration shim in that path.

### Allowed Types

Only C-ABI-safe types are permitted in plugin functions:

| ZZ Type | C Equivalent | Notes |
|---------|--------------|-------|
| `int` | `int64_t` | 64-bit integer (C `int` params work for values fitting i32; compare status codes with `!= 0`, never `== -1`) |
| `float` | `double` | 64-bit float |
| `bool` | `bool` | |
| `str` | `const char *` | Borrowed for the call only (NUL-terminated, same `zz_str_cptr(v.s)` convention stdlib natives use); the C side must not retain it. Not allowed as a return type |
| `bytes` | `const uint8_t *` + `size_t` | Borrowed window into the buffer (no copy, no NUL termination — interior NULs are visible); one ZZ param becomes two C params `(ptr, len)`. The C side must not retain either half. Empty views pass a valid pointer with length 0 |
| `*const void` | `const void*` | Opaque pointer (read-only) |
| `*mut void` | `void*` | Opaque pointer (mutable) |
| `void` | (no return) | Functions returning nothing |

**Not allowed:** arrays, structs, enums, `Option`, `Result`, closures, function pointers, `str` returns.

---

## 2. Package Structure

### Minimal Plugin (declarative only)

```
my-plugin/
├── plugin.zzi          Interface declarations
├── zz.toml             Package manifest ([native] backend = "cc")
├── csrc/
│   └── wrapper.c       C implementation (compiled by zz)
└── src/
    └── main.zz         ZZ code (optional)
```

### Full Plugin (with system dependency)

```
my-plugin/
├── plugin.zzi          Interface declarations
├── zz.toml             Package manifest with [native.build-cc]
├── my.zz               Ergonomic entry (optional; or src/my.zz) — loaded
│                       as a module on `import my`, so `my.resize(...)`
│                       resolves alongside the manifest signatures
├── csrc/
│   ├── wrapper.h       C wrapper API (optional)
│   └── wrapper.c       C wrapper implementation
└── src/
    └── main.zz         ZZ ergonomic layer
```

### Hermetic Rust Plugin (declarative cargo, no scripts)

```
my-plugin/
├── plugin.zzi          Interface declarations (`// C-ABI: 1`)
├── zz.toml             Package manifest with [native.build-rust]
├── native/             Rust crate producing both .a and .so/.dylib
│   ├── Cargo.toml      crate-type = ["cdylib", "staticlib"]
│   ├── Cargo.lock      committed; no path deps
│   └── src/
│       └── lib.rs
└── src/
    └── main.zz         ZZ ergonomic layer
```

See §5. No `build.sh` and no `build.rs` are allowed.

### Legacy Plugin (Rust crate, hook path — deprecated, see §4)

```
my-plugin/
├── plugin.zzi          Interface declarations
├── zz.toml             Package manifest with [native] build = "build.sh"
├── build.sh            Build hook (deprecated, --allow-hooks only)
├── native/             Rust crate producing .so/.a
│   ├── Cargo.toml
│   ├── build.rs        Build script (optional)
│   └── src/
│       └── lib.rs      Native implementations
└── src/
    └── main.zz         ZZ ergonomic layer
```

---

## 3. `zz.toml` Configuration

Three backends. New plugins use a declarative backend — C or Rust —
with no scripts:

```toml
[package]
name = "my-plugin"
version = "0.1.0"

[native]
backend = "cc"                 # declarative C build by zz itself
manifest = "plugin.zzi"        # optional, default: plugin.zzi

[native.build-cc]
sources = ["csrc/wrapper.c"]   # explicit list, no globs
include_dirs = ["csrc"]        # the ONLY way to add -I paths
defines = ["NDEBUG"]           # -D flags without the prefix
cflags = ["-O2", "-Wall", "-Wextra", "-fPIC"]  # explicit allowlist (§4)
libs = []                      # direct -l names; prefer pkg_config
pkg_config = ["vips"]          # optional; resolved by zz, recorded in zz.lock
targets = ["linux-x86_64-gnu-glibc2.28", "macos-arm64-min11.0"]

# Per-platform prebuilt (optional; https: only in v1):
[native.prebuilt.target."linux-x86_64-gnu-glibc2.28"]
url = "https://github.com/<org>/<repo>/releases/download/v0.1.0/my-plugin-0.1.0-linux-x86_64-gnu-glibc2.28.tgz"
sha256 = "9f2c…"
```

Tags contain dots (`glibc2.28`), so quote them. Bare triples
(`x86_64-unknown-linux-gnu`) are rejected — the tag carries the
libc/ABI floor (see §3.1).

A Rust crate uses the same platform tags plus a crate table (§5):

```toml
[native]
backend = "rust"

[native.build-rust]
crate = "native"              # relative to the package root
cargo_features = []           # optional bare feature names
# target = "x86_64-unknown-linux-gnu"  # optional; derived from the tag otherwise
# rustc_range = ">=1.75, <2"  # optional; default compatibility range
# profile = "release"         # optional; release (default) or debug
```

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `backend` | string | no | `cc` (declarative C), `rust` (hermetic cargo), or `hook` (legacy); inferred from shape when absent, must agree when set |
| `manifest` | string | no | Path to interface file, default: `plugin.zzi` |
| `build` | string | no | Legacy hook script (deprecated; mutually exclusive with declarative tables) |
| `pkg_config` | string | no | Legacy system query (deprecated; use the `build-cc` list form) |

### 3.1 Platform tags

Format `<os>-<arch>-<abi>-<floor>`:

| Tag | Meaning |
|---|---|
| `linux-x86_64-gnu-glibc2.28` | glibc ≥ 2.28 |
| `linux-aarch64-gnu-glibc2.28` | glibc ≥ 2.28 |
| `linux-x86_64-musl` | musl (no floor) |
| `macos-arm64-min11.0` | macOS ≥ 11.0 |
| `macos-x86_64-min12.0` | macOS ≥ 12.0 |
| `windows-x86_64-msvc` | AOT-only (VM dlopen deferred) |

Matching: exact tag preferred, else same os+arch+ABI with
`artifact_floor <= host_floor`. An artifact floored above the host is
rejected (`prebuilt requires glibc 2.28, host has 2.17`). glibc and
musl never interchange. Missing entry means "no prebuilt, source
fallback" (gated: transitive source builds need
`--allow-source-builds`).

### 3.2 Prebuilt artifacts (`zz publish --artifact-dir`)

Each per-tag `.tgz` (`<name>-<version>-<tag>.tgz`) contains the FULL
`build/` layout (`*.o`, `*.a`, shared lib, `ldflags.txt`/`cflags.txt`).
Partial tarballs are rejected at publish and at install: a static-only
upload would fix AOT while silently breaking `zz run`.

Ship them from GitHub releases; `zz publish --artifact-dir <dir>`
verifies each tarball (layout + both-engine symbol check) and prints
the `[native.prebuilt]` stanza to paste into `zz.toml`. The sha256 in
the manifest is verified before unpack — a compromised mirror serving
different bytes fails closed. Rotation is a new package version. The
`registry:` URL scheme is deferred to v2.

---

## 4. Declarative C build (`[native.build-cc]`)

`zz` compiles the plugin itself — the only processes ever spawned are
`cc`/`ar`/`pkg-config` with structured arguments under a hermetic
environment (cleared env: `PATH`, `CC`, isolated `TMPDIR`,
`SOURCE_DATE_EPOCH=0`). No shell, no script, no network during the
build. Per source: `cc -c` → `build/<stem>.o`; then
`ar rcs build/lib<name>.a` (AOT input) and
`cc -shared` → `build/lib<name>.so|.dylib|.dll` (VM input); then
`build/cflags.txt` + `build/ldflags.txt`.

### Required outputs (produced by `zz`, not you)

| File | Description |
|------|-------------|
| `build/*.o` | Compiled wrapper objects (AOT link) |
| `build/lib*.a` | Static library (AOT link) |
| `build/lib*.so` | Shared library (VM dlopen, Linux) |
| `build/lib*.dylib` | Shared library (VM dlopen, macOS) |
| `build/lib*.dll` | Shared library (Windows; AOT-only, VM deferred) |
| `build/ldflags.txt` | Linker flags (from `libs` + `pkg_config`) |
| `build/cflags.txt` | Compiler flags (cache-key input) |

### Allowed `cflags` (explicit allowlist)

`-O0 -O1 -O2 -O3 -Os -Oz`, `-g`, `-fPIC -fpic -fPIE`,
`-Wall -Wextra -Werror -Wno-*`, `-std=c11 -std=c17`, `-pthread`,
`-march=x86-64 -march=armv8-a`. Everything else is a manifest error.
Rejected classes (each fail closed): any flag containing `,`
(blocks `-Wl,`/`-Wp,` smuggling), `@file` indirection, `-Wl,*`,
`-Wp,*`, `-Xlinker`, `-Xpreprocessor`, bare `-I` (use
`include_dirs`), bare `-L`/`-l` (use `libs`), `--target=` (the
toolchain sets it from the platform tag).

The same validator gates `pkg-config` output tokens before use
(`-I`/`-L`/`-l` allowed from that source only — the manifest form
still bans them so only the resolver can add them).

### Verification (both engines, required)

Before the result is accepted, `zz` checks every `plugin.zzi`
symbol resolves in the archive (`nm` check, AOT) AND in the shared
lib (VM), plus exactly one ABI stamp (`ZZ_C_PLUGIN_ABI_VERSION` or
`ZZ_PLUGIN_ABI_VERSION`) in both the archive and the shared lib.
Missing either artifact class is a hard error (except Windows,
AOT-only with a loud note).

### Threat model (read this)

The declarative build removes **arbitrary build-time code**: no
`build.sh`, no `curl` at build, no `cargo build` scripts. Manifests
are auditable, flags are allowlisted, the compiler and resolved
`pkg-config` versions land in `zz.lock`, and drift busts the cache.

It does NOT make plugins safe. A native dependency still executes
compiled C inside your process — `dlopen`'d by `zz run`, linked into
your binary by `zz build`. A malicious maintainer gets code execution
regardless of how it was built. sha256 gives **integrity** (bytes
match what the manifest pins), not **authenticity** (who published
them). Signing (Sigstore) is a v2 track.

Docs promise "no arbitrary build scripts" — never "secure plugins".
Treat every `[native]` dependency as code execution with C
privileges: pin versions, review the sources, keep native deps few.

`zz audit` verifies the closure: registry pins (published? hash?
licensed?) plus, per native dep, the locked backend/tag/compiler/
`pkg-config` record against the live machine. Compiler or system
library drift, tampered sources, and legacy hooks fail the audit
(hooks pass only behind `--allow-hooks`); `zz install` re-pins after
any legitimate change.

### Legacy build hook (`build = "build.sh"`, deprecated)

One-release deprecation window only. Direct-dependency hooks run
behind `zz install/build --allow-hooks` with a warning on every use;
transitive hooks always error. Then the `bash` path is deleted.
Rust-crate plugins should migrate to the hermetic cargo backend in
§5 — there is no reason to keep a shell hook for cargo.

<details>
<summary>Legacy <code>build.sh</code> contract (appendix)</summary>

The hook ran from the package root, exited 0 on success, and wrote
`build/*.o`, `build/lib*.a`, `build/lib*.so|.dylib`,
`build/ldflags.txt`, `build/cflags.txt`. Failures warned and the
build continued with whatever artifacts existed. If both
`build/<x>.o` and an archive containing `<x>.o` existed, `zz build`
thinned the archive (loose objects won).
</details>

### Single Result Slot Pattern (recommended)

If producing calls stash their output in one module-static slot consumed
via a getter, guard overwrites in debug builds: track a pending flag set
on produce and cleared on consume, and `abort()` with
`result slot overwritten before consumption` when a produce runs while the
flag is set. Compile the check out with `NDEBUG` for production. See
zimg's `csrc/zimg_wrapper.c` (`ZIMG_SLOT_GUARD`) for the reference
implementation.

---

## 5. Hermetic Rust crate (`[native.build-rust]`)

Rust plugins are built by `zz` from a committed, vendored cargo
closure. There is **no `build.sh` and no `build.rs`**: the package may
never run its own build code. `zz` drives cargo with structured
arguments (`cargo vendor` once to fetch, then
`cargo build --offline --locked`), a scratch `CARGO_HOME`, an isolated
target directory, and an explicit `--target` derived from the platform
tag.

### Package layout

```
my-plugin/
├── plugin.zzi
├── zz.toml
├── native/
│   ├── Cargo.toml          # both crate types
│   ├── Cargo.lock          # committed; no path/hashless deps
│   └── src/lib.rs
└── src/main.zz             # optional
```

`zz.toml`:

```toml
[native]
backend = "rust"

[native.build-rust]
crate = "native"
# cargo_features = ["simd"]
# target = "x86_64-unknown-linux-gnu"
# rustc_range = ">=1.75, <2"
# profile = "release"
```

### `Cargo.toml`

```toml
[package]
name = "my_plugin_native"
version = "0.1.0"
edition = "2021"
license = "MIT"          # required for audit

[lib]
name = "my_plugin_native"
crate-type = ["cdylib", "staticlib"]
```

- **Both crate types are required**: `staticlib` is the AOT link input
  (`.a`), `cdylib` is the VM dlopen input (`.so`/`.dylib`).
- **Commit `Cargo.lock`.** Locked builds are mandatory; `--locked`
  fails rather than resolve a new closure.
- **No path dependencies.** Every dependency is either a registry
  package with a sha256 checksum or a git package pinned by commit
  hash. Anything else fails validation.
- **No `build.rs` / `[package] build = "..."`.** Codegen must be
  committed. This keeps the consumed package from executing arbitrary
  build-time code.
- Do not depend on `zz_runtime` (or any local path). The Hermetic
  backend produces a plain C-ABI library; the VM discovers it through
  the same dlsym path as C plugins.

### `lib.rs` — C-ABI exports

Use `// C-ABI: 1` in `plugin.zzi` and export
`ZZ_C_PLUGIN_ABI_VERSION` (not the legacy Rust registration stamp),
because there is no Rust shim to register natives:

```rust
use std::os::raw::c_char;

/// Exactly one ABI stamp, in both the archive and the shared lib.
#[no_mangle]
pub static ZZ_C_PLUGIN_ABI_VERSION: u32 = 1;

/// Symbol name = ZZ name with `.` → `_` (or an explicit `= "..."`).
///
/// # Safety
/// `s` is a borrowed, NUL-terminated string valid for this call only.
#[no_mangle]
pub unsafe extern "C" fn my_process(s: *const c_char) -> i64 {
    if s.is_null() {
        return 0;
    }
    let bytes = unsafe { std::ffi::CStr::from_ptr(s) }.to_bytes();
    bytes.len() as i64
}
```

`plugin.zzi`:

```
// Version: 1
// C-ABI: 1
// Plugin-version: 0.1.0

extern "C" {
    func my_process(s: str) -> int
}
```

`str` is borrowed for the call; never retain the pointer. `bytes`
crosses as `(ptr, len)`. `int` is 64-bit on both engines.

### What `zz` does

1. Validates `Cargo.toml` (both crate types, licence), `Cargo.lock`
   (root + pinned closure, no path deps), and bans `build.rs`.
2. Stages the crate under `<package>/build/rust-src`, appending an
   empty `[workspace]` when absent so a parent workspace cannot capture
   it.
3. Computes a content key over `Cargo.toml` + `Cargo.lock` + crate
   sources + the `[native.build-rust]` spec. On a cold cache it runs
   `cargo vendor --locked` once and stores the closure in CAS under
   that key. Every later build is offline and reuses the vendored tree.
4. Runs `cargo build --offline --locked --target <triple>
   --target-dir <package>/build/rust-target` with a cleared environment
   (only `PATH`/`HOME`/`RUSTUP_HOME` inherited; scratch `CARGO_HOME`
   and `TMPDIR`, fixed `SOURCE_DATE_EPOCH`).
5. Checks `rustc` against `rustc_range` (default `>=1.75, <2`; the
   link-time ABI stamp, not the rustc patch level, is the real
   compatibility check).
6. Copies `lib<name>.a` + `lib<name>.so|.dylib` into `build/`, writes
   `cflags.txt`/`ldflags.txt`, and verifies both engines: every
   `plugin.zzi` symbol in the archive and shared lib, exactly one ABI
   stamp in each. Windows is AOT-only with a loud note.

Source builds of transitive Rust plugins are gated behind
`--allow-source-builds`; direct path deps build normally. There is no
prebuilt path for Rust in v1.

### Audit

`zz audit` prints `backend=rust`, the platform tag, recorded `rustc`,
and the closure key, then verifies:

- the recomputed closure key matches `zz.lock` (tampered sources or a
  changed spec fail),
- every closure crate still has its checksum/commit pin,
- every vendored crate has a licence,
- the live `rustc` matches the lock and satisfies `rustc_range`.

The reference example is [`examples/rust-fnv/`](../examples/rust-fnv/):
FNV-1a in Rust, no scripts, consumed by a `zz` program on both engines.

### Legacy Rust hook (deprecated)

The pre-#236 `build = "build.sh"` + `zz_runtime` registration path still
exists for one deprecation window behind `--allow-hooks` (direct deps
only). It is not the recommended shape and cannot be used with
`[native.build-rust]`. Migrate by deleting the hook and `build.rs`,
keeping both crate types, committing `Cargo.lock`, and exporting the
C-ABI symbols above.

---

## 6. AOT Path vs VM Path

### AOT Path (`zz build`)

```
plugin.zzi → checker loads FuncSigs → codegen emits extern declarations
                                          ↓
build.sh runs → produces .o + .a files → linker resolves symbols
                                          ↓
                                    Final binary (native symbols embedded)
```

- No dlopen needed — symbols resolved at link time
- All plugin artifacts are statically linked into the final binary
- Best for: production builds, CLI tools, scripts

### VM Path (`zz run`, REPL)

```
plugin.zzi → checker loads FuncSigs → interpreter starts
                                          ↓
dlopen loads .so/.dylib → validates ABI version → calls zz_plugin_register
                                          ↓
                              natives HashMap populated → runtime dispatch
```

- dlopen loads the shared library at runtime
- Version stamp validated before loading
- Best for: development, REPL, interpreted mode

---

## 7. Handling Strings

`str` is a first-class extern parameter type (but never a return type).
AOT lowers it to `const char *` via `zz_str_cptr(v.s)` — borrowed for the
call only; the C side must not retain it. VM glue receives
`Value::Str(String)`; copy through `CString::new` (Rust strings are not
NUL-terminated) and reject interior NULs with an `EvalError`:

```zz
// plugin.zzi
extern "C" {
    func my.starts_with(s: str, prefix: str) -> int
}
```

```rust
// In lib.rs (VM glue)
let cs = CString::new(s).map_err(|_| EvalError::new("interior NUL", span))?;
let r = unsafe { my_starts_with(cs.as_ptr(), cp.as_ptr()) };
```

Legacy alternative: raw `*const void` + length params (still supported).

### Pointer + length (buffers)

```zz
extern "C" {
    func my_write(buf: *const void, len: int) -> int
}
```

Pass buffer as `*const void` + byte count as separate `int` parameter.

---

## 8. Error Handling

Native functions return `Result<Value, EvalError>`. Use `EvalError::new(message, span)` for errors:

```rust
fn native_my_process(
    _interp: &mut zz_runtime::eval::Interp,
    args: &mut Vec<zz_runtime::Value>,
    span: zz_runtime::Span,
) -> Result<zz_runtime::Value, zz_runtime::EvalError> {
    let handle = match &args[0] {
        zz_runtime::Value::Int(i) => *i as *mut std::ffi::c_void,
        other => {
            return Err(zz_runtime::EvalError::new(
                format!("expected handle, got {other:?}"),
                span,
            ));
        }
    };

    // Call C wrapper
    let code = unsafe { my_process_c(handle) };
    if code != 0 {
        let msg = unsafe {
            let ptr = my_last_error();
            if ptr.is_null() {
                "unknown error".to_string()
            } else {
                CStr::from_ptr(ptr).to_string_lossy().into_owned()
            }
        };
        return Err(zz_runtime::EvalError::new(msg, span));
    }

    Ok(zz_runtime::Value::Int(0))
}
```

---

## 9. Opaque Handles Pattern

Most plugins manage C-side state through opaque integer handles:

```zz
// ZZ side
let img = zimg.init()
zimg.resize(img, 0.5)
let w = zimg.width(img)
zimg.release(img)
```

```rust
// Rust side — handles are void pointers cast to/from i64
fn native_zimg_init(...) -> Result<Value, EvalError> {
    let ptr = unsafe { zimg_init_c() };  // returns void*
    Ok(Value::Int(ptr as i64))
}

fn native_zimg_resize(...) -> Result<Value, EvalError> {
    let ptr = handle_to_ptr(&args[0])?;  // i64 → void*
    let scale = extract_float(&args[1])?;
    let code = unsafe { zimg_resize_c(ptr, scale) };
    Ok(Value::Int(code as i64))
}
```

---

## 10. Symbol Naming Convention

All C symbols exported by the plugin must be prefixed with the package name to avoid collisions:

```
Package "zimg":     zimg_init, zimg_load, zimg_resize, ...
Package "mylib":    mylib_init, mylib_process, ...
Package "foo.bar":  foo_bar_init, foo_bar_process, ...
```

The `zz_plugin_register` entrypoint and `ZZ_PLUGIN_ABI_VERSION` symbol are NOT prefixed — they are well-known names shared by all plugins.

### Calling your own natives: C-symbol aliases

Inside the defining package, a dotted self-path (`bytehash.sum(...)`)
reads as a method call and never resolves to the extern. Every extern
is therefore also visible under its C symbol spelling (`bytehash_sum`)
within the defining package (and to importers): a bare name that calls
exactly like the dotted form on both engines. Aliases never override
real names. Generated wrappers (§10b) use them for call-through.

---

## 10b. Generated Wrappers (`zz generate-wrapper`)

`zz generate-wrapper [pkg-dir]` reads `plugin.zzi` and writes
`src/<pkg>.zz`: one `pub` wrapper per manifest function, calling
through the C-symbol alias (§10). The file carries a `GENERATED`
marker; hand-written files are never overwritten. Output is
deterministic, so `zz generate-wrapper --check` fails loudly on drift —
re-run the command after manifest changes, never hand-edit. The
ergonomic layer (slot getters, `Result` mapping) builds on these
wrappers; today they are faithful pass-throughs.

---
## 11. Version Stamping

The loader validates two things before loading a plugin:

1. **ABI Version** — `ZZ_PLUGIN_ABI_VERSION` must equal `1` (currently). Bump on breaking ABI changes.
2. **Rustc Version** — The `// Rustc:` header in `plugin.zzi` records which rustc built the plugin. Loader checks compatibility.

If either check fails, the loader refuses to load the plugin with a clear error message.

---

## 12. Platform Support

| Platform | AOT (`zz build`) | VM (`zz run`) |
|----------|-------------------|----------------|
| Linux | ✅ Static linking | ✅ dlopen/dlsym |
| macOS | ✅ Static linking | ✅ dlopen/dlsym |
| Windows | ⏳ Pending runtime port | ❌ Deferred (LoadLibrary) |

The AOT path is plain static linking on Linux and macOS. It is not
available on Windows yet: the AOT C runtime needs POSIX threads and
sockets (`<pthread.h>`, `fcntl`/`poll`), so it needs a Win32 port before
`zz build` ships there. The VM path needs dlopen/LoadLibrary support;
Windows is planned for a future release. `zz pm` still builds C and Rust
plugin packages on Windows hosts — only linking them into a ZZ binary is
blocked.

---

## 13. Testing Plugins

### Unit test the native crate

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_my_init() {
        let mut interp = /* create test interp */;
        let mut args = vec![];
        let span = zz_runtime::Span::new(0, 0);
        let result = native_my_init(&mut interp, &mut args, span);
        assert!(result.is_ok());
    }
}
```

### E2E test with ZZ code

```zz
// test.zz
import my

func main() {
    code := my.init()
    assert(code == 0)
    println("plugin_ok")
}
```

Run with:
```bash
# AOT
zz build test.zz && ./test

# VM
zz run test.zz
```

---

## 14. Common Gotchas

### `str` crosses as borrowed `const char *`

`str` params lower to `zz_str_cptr(v.s)` (AOT) or arrive as `Value::Str`
(VM glue: copy through `CString::new`, reject interior NULs). Never retain
the pointer; `str` returns are unsupported.

### No `Copy` on `Value`

`Value` does not implement `Copy`. Use `match &value { ... }` (borrow) not `match value { ... }` (move) in native functions.

### `Value::Str` is `Box<String>`

When creating a string value: `Value::Str(Box::new("hello".to_string()))`, not `Value::Str("hello".to_string())`.

### `EvalError` is a struct

`EvalError::new(message, span)` creates an error. There is no `EvalError::TypeError` variant — use `EvalError::new(format!("expected X, got Y"), span)`.

### FFI warnings

The `RegisterCallback` type and `zz_plugin_register` function produce `improper_ctypes_definitions` warnings because `NativeFn` is a Rust function pointer. Suppress with `#[allow(improper_ctypes_definitions)]`. This is safe in practice because `NativeFn` uses C calling convention.

### Plugin libraries are never unloaded

Loaded plugin shared libraries stay in memory for the process lifetime —
the host retains every `PluginLib` handle in a global registry. Dropping a
handle would unmap registered function pointers (use-after-dlclose), so
handles are never released once loaded.

---

## 15. Example: Complete Plugin

See the `zimg` package for a complete working example (declarative v1):

- **Repository:** `github.com/zz-language/zimg`
- **Interface:** `plugin.zzi` (C-ABI)
- **C wrapper:** `csrc/zimg_wrapper.c` (libvips bridge)
- **Manifest:** `[native.build-cc]` with `pkg_config = ["vips"]`
- **Prebuilt:** per-tag `.tgz` on GitHub releases (`https:` + sha256)
- **ZZ wrapper:** `src/zimg.zz` (ergonomic ZZ layer)

No `build.sh`: `zz` compiles `csrc/` itself and verifies both engines.
