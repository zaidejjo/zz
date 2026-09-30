# Plugin Author Guide

ZZ supports native plugins — packages that expose Rust/C implementations to ZZ code via the `extern "C"` interface mechanism. Plugins work in both the AOT compiler (`zz build`) and the VM interpreter (`zz run`).

---

## Quick Start

1. Create your package with `plugin.zzi` + `zz.toml` + optional native crate
2. Implement the C wrapper + Rust glue
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

### Metadata Header

The first comment block carries metadata the loader validates:

```
// Version: <manifest-format-version>  (required, currently 1)
// Rustc: <exact rustc version>        (required, for ABI compatibility check)
// Plugin-version: <semver>            (required, user-facing versioning)
```

The `Version` field is the manifest format version. The `Rustc` field records which Rust compiler built the plugin. The loader checks both against expected values before loading.

### Allowed Types

Only C-ABI-safe types are permitted in plugin functions:

| ZZ Type | C Equivalent | Notes |
|---------|--------------|-------|
| `int` | `int64_t` | 64-bit integer (C `int` params work for values fitting i32; compare status codes with `!= 0`, never `== -1`) |
| `float` | `double` | 64-bit float |
| `bool` | `bool` | |
| `str` | `const char *` | Borrowed for the call only (NUL-terminated, same `zz_str_cptr(v.s)` convention stdlib natives use); the C side must not retain it. Not allowed as a return type |
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

Two backends. New plugins use the declarative C backend — no scripts:

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

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `backend` | string | no | `cc` (declarative) or `hook` (legacy); inferred from shape when absent, must agree when set |
| `manifest` | string | no | Path to interface file, default: `plugin.zzi` |
| `build` | string | no | Legacy hook script (deprecated; mutually exclusive with `build-cc`) |
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
`ZZ_PLUGIN_ABI_VERSION`) in the shared lib. Missing either artifact
class is a hard error (except Windows, AOT-only with a loud note).

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

### Legacy build hook (`build = "build.sh"`, deprecated)

One-release deprecation window only. Direct-dependency hooks run
behind `zz install/build --allow-hooks` with a warning on every use;
transitive hooks always error. Then the `bash` path is deleted.
Rust-crate plugins (`native/` + cargo) stay on the hook until a v2
`cargo vendor` design lands — or vendor a C shim and go declarative
today (arbitrary `cargo build` is the same script problem in a
trench coat).

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

## 5. Rust Native Crate (legacy hook path — v2 will vendor it)

Pure-C plugins (§4) are the v1 path. Rust-crate plugins stay on the
deprecated hook until the v2 `cargo vendor` design lands: arbitrary
`cargo build` at install time is the same script problem in a trench
coat, so it keeps the `build = "build.sh"` shape behind
`--allow-hooks` (direct deps only).

### Cargo.toml

```toml
[package]
name = "my_plugin_native"
version = "0.1.0"
edition = "2021"

[lib]
crate-type = ["cdylib", "staticlib"]

[dependencies]
# Path to your local zz repo (for development)
zz_runtime = { path = "/path/to/zz_lang/crates/zz_runtime" }
```

The crate must produce both `cdylib` (`.so`/`.dylib`) and `staticlib` (`.a`).

### `lib.rs` — Registration Entrypoint

```rust
use std::ffi::{CStr, CString};
use std::os::raw::c_char;

/// ABI version stamp — must match CURRENT_ABI_VERSION in zz_plugin::loader.
#[no_mangle]
pub static ZZ_PLUGIN_ABI_VERSION: u32 = 1;

// C wrapper declarations
extern "C" {
    fn my_init() -> i32;
    fn my_process(handle: *mut std::ffi::c_void, data: i64) -> i32;
    fn my_release(handle: *mut std::ffi::c_void);
}

// ZZ-visible native functions
fn native_my_init(
    _interp: &mut zz_runtime::eval::Interp,
    _args: &mut Vec<zz_runtime::Value>,
    _span: zz_runtime::Span,
) -> Result<zz_runtime::Value, zz_runtime::EvalError> {
    let code = unsafe { my_init() };
    Ok(zz_runtime::Value::Int(code as i64))
}

// ... other native functions ...

/// Registration callback type (must match zz_plugin::loader).
type RegisterCallback = extern "C" fn(name: *const i8, arity: usize, f: zz_runtime::NativeFn);

/// Plugin registration entrypoint — called by VM loader.
#[no_mangle]
pub extern "C" fn zz_plugin_register(callback: RegisterCallback) {
    let name = CString::new("my.init").unwrap();
    callback(name.as_ptr(), 0, native_my_init);

    let name = CString::new("my.process").unwrap();
    callback(name.as_ptr(), 2, native_my_process);

    let name = CString::new("my.release").unwrap();
    callback(name.as_ptr(), 1, native_my_release);
}
```

### Key Rules

1. **`ZZ_PLUGIN_ABI_VERSION`** — must be a `pub static u32` with `#[no_mangle]`
2. **`zz_plugin_register`** — must be `extern "C"` with `#[no_mangle]`
3. **Function pointer types** — all native functions must have signature:
   ```rust
   fn(&mut Interp, &mut Vec<Value>, Span) -> Result<Value, EvalError>
   ```
4. **No Rust types across FFI** — use `i64`/`f64`/`i32` for scalars, `*mut void` for opaque handles
5. **Suppress FFI warnings** — add `#[allow(improper_ctypes_definitions)]` to the `RegisterCallback` type and `zz_plugin_register` function (because `NativeFn` is a Rust function pointer, not a C-ABI type, but works in practice)
6. **Register dotted ZZ names** — the same names declared in `plugin.zzi`
   (`my.process`, not `my_process`). `zz run` dispatches on them directly;
   a C-symbol key is aliased automatically when present, but dotted is canonical.
7. **VM handles stay loaded** — the host retains every loaded `.so` for the
   process lifetime, so registration-time function pointers stay valid.

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
| Windows | ✅ Static linking | ❌ Deferred (LoadLibrary) |

The AOT path works everywhere (it's just static linking). The VM path requires dlopen support. Windows support is planned for a future release.

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
