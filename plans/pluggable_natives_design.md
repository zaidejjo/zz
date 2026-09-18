# ZZ Pluggable Native Library System — Design Document

**Date:** 2026-09-17
**Status:** Draft — awaiting review before implementation
**Scope:** Compiler/toolchain project inside `zz_lang` monorepo

---

## 0. Problem Statement

ZZ's native-function registration is fully closed. Every native must be hand-registered across 4 hardcoded tables inside the `zz_lang` monorepo:

| Table | File | Purpose |
|-------|------|---------|
| `stdlib_funcs()` | `zz_stdlib/src/funcs.rs` | Checker type signatures |
| `stdlib_natives()` | `zz_stdlib/src/natives/mod.rs` | VM interpreter dispatch |
| `native_impl()` | `zz_codegen/src/lower/mod.rs` | AOT embedded-C symbols |
| `ffi_impl()` | `zz_codegen/src/ffi.rs` | AOT Rust-staticlib symbols |

There is no dlopen, no plugin macro, no external registration path. This blocks any native ZZ library (zimg or otherwise) from being an independently-compiled, separately-distributed package.

---

## 1. §1 Investigation Result: Two Execution Paths, Two Mechanisms

### AOT path (`zz build`): Direct static linking — NO dlopen needed

**Evidence** (`lower/expr.rs:1828`):
```c
// Generated C for io.println(arg):
zz_call_native1(zz_io_println, arg)
```

`zz_call_native1` (`core.c:1571`) receives a function pointer and calls it directly:
```c
zz_value zz_call_native1(zz_value (*f)(zz_value, int *), zz_value a) {
    int err = 0;
    zz_value r = f(a, &err);
    return r;
}
```

The C compiler resolves `zz_io_println` as a symbol reference. The linker resolves it at link time — from the embedded C runtime (for `native_impl` symbols) or from `libzz_native_rt.a` (for `ffi_impl` symbols). There is no runtime dispatch table in the AOT path.

**Implication:** AOT pluggability = (1) checker learns signatures from external manifest, (2) build process links package's compiled native glue. Standard static linking. No dlopen.

### VM interpreter path (`zz run`, REPL): Runtime HashMap dispatch — dlopen needed

**Evidence** (`eval/mod.rs:23`, `eval/tree_walker.rs:1168`):
```rust
pub natives: HashMap<String, NativeEntry>,
// ...
match self.natives.get(&nf.name) {
    Some(entry) => (entry.f)(self, &mut args, span),
```

`NativeEntry` holds a Rust function pointer (`NativeFn = fn(&mut Interp, &mut Vec<Value>, Span) -> Result<Value, EvalError>`). The HashMap is populated at interpreter startup from `stdlib_natives()`. There is no static linking — everything is runtime dispatch.

**Implication:** VM pluggability requires dlopen to load plugin `.so`/`.dylib` and call a registration function that populates the HashMap.

### Design Decision

The system has **two separate mechanisms** that share a common manifest format but have independent implementations:

| Path | Mechanism | Implementation |
|------|-----------|----------------|
| AOT | Static linking + external manifest | Extend checker + build system |
| VM | dlopen + registration callback | New loading infrastructure |

---

## 2. §2a: Plugin Manifest Format

### Decision: `.zzi` interface file (declarative ZZ syntax)

Every native package ships a `plugin.zzi` file containing `extern "C"` declarations that the ZZ checker can parse directly.

**Why `.zzi` over TOML/JSON:**
- The checker already has a parser for `extern "C"` blocks (`parser/stmt.rs:276-362`)
- The checker already validates C ABI types for extern params (`checker/funcs.rs:55-104`)
- No new parser needed — reuse existing infrastructure
- Plugin authors write familiar ZZ syntax, not a data format
- The `.zzi` extension signals "interface file" distinct from `.zz` "implementation file"

**Format:**

```zz
// plugin.zzi — zimg native interface
// Version: 1
// Rustc: 1.85.0
// Plugin-version: 0.1.0

extern "C" {
    func zimg_init() -> int
    func zimg_load(path: *const void) -> int
    func zimg_resize(img: *mut void, scale: float) -> int
    func zimg_blur(img: *mut void, sigma: float) -> int
    func zimg_crop(img: *mut void, left: int, top: int, width: int, height: int) -> int
    func zimg_rot(img: *mut void, angle: int) -> int
    func zimg_flip(img: *mut void, direction: int) -> int
    func zimg_width(img: *mut void) -> int
    func zimg_height(img: *mut void) -> int
    func zimg_save(img: *mut void, path: *const void) -> int
    func zimg_save_jpeg(img: *mut void, path: *const void, quality: int) -> int
    func zimg_save_png(img: *mut void, path: *const void, compression: int) -> int
    func zimg_save_webp(img: *mut void, path: *const void, quality: int) -> int
    func zimg_release(img: *mut void)
    func zimg_get_result() -> *mut void
    func zimg_last_error() -> *const void
}
```

### Metadata header

The first comment block in `.zzi` carries metadata the loader checks:

```
// Version: <manifest-format-version>  (required, currently 1)
// Rustc: <exact rustc version>        (required, for ABI mismatch check)
// Plugin-version: <semver>            (required, for user-facing versioning)
```

The loader parses these from the comment block using a simple key-value scanner (not a full ZZ comment parser — just `// Key: Value` lines at the top of the file). This is intentionally simple and robust.

### Checker integration

The checker gains a new entry point:

```rust
pub fn load_plugin_manifest(path: &Path) -> Result<HashMap<String, FuncSig>, ManifestError>
```

This:
1. Reads the `.zzi` file
2. Parses it using the existing ZZ parser (it's valid ZZ syntax — an `extern "C"` block)
3. Extracts function signatures into `FuncSig` entries
4. Validates metadata header (version stamp, rustc version)
5. Returns the same `HashMap<String, FuncSig>` shape that `stdlib_funcs()` returns

The existing checker loop (`check_program`) is extended to accept additional `FuncSig` maps beyond `stdlib_funcs()`. Plugin manifests are merged into the available function set before type-checking begins.

---

## 3. §2b: AOT Path — Build-Time Discovery + Static Linking

### Architecture

```
zimg/
├── plugin.zzi           Manifest (extern "C" declarations + metadata)
├── csrc/
│   ├── zimg_wrapper.h   C wrapper API
│   └── zimg_wrapper.c   C wrapper implementation
├── src/
│   ├── native.rs        Rust native implementations (called from C wrapper)
│   └── image.zz         ZZ ergonomic layer (calls pre-registered natives)
├── build.sh             Build hook: compile C wrapper + Rust staticlib
└── zz.toml              Package manifest
```

### Build process

When `zz build` encounters a project that depends on a native package:

1. **Discovery:** The build system reads the dependency's `plugin.zzi` and `build.sh` (or `build` hook in `zz.toml`).

2. **Compilation:** The build system invokes the package's `build.sh`, which:
   - Compiles `csrc/zimg_wrapper.c` → `zimg_wrapper.o` (using pkg-config for libvips flags)
   - Compiles the Rust native crate → `libzimg_native.a` (a staticlib)
   - Emits link flags to `build/ldflags.txt`

3. **Linking:** The build system generates the final `prog.c`, then compiles and links it:
   ```bash
   clang prog.c zimg_wrapper.o libzimg_native.a $(cat build/ldflags.txt) -o output
   ```

4. **Symbol resolution:** The linker resolves `zimg_init`, `zimg_load`, etc. from `libzimg_native.a` (or `zimg_wrapper.o`). The C compiler already has the `extern` declarations from the `.zzi` manifest (injected into `prog.c` by the checker/codegen).

### Key insight

The AOT path reuses the **exact same mechanism** the toolchain already uses for `libzz_native_rt.a` — the `ensure_staticlib()` pattern in `ffi.rs`. The difference is: instead of building a single hardcoded `zz_native_rt` crate, the build system builds each dependency's native crate independently and links all of them.

### Integration with `zz pm`

The `zz pm` package manager gains:

1. A `[native]` section in `zz.toml`:
   ```toml
   [native]
   build = "build.sh"           # build hook script
   pkg-config = "vips >= 8.6"   # optional system dependency declaration
   ```

2. After `zz pm install`, the build system knows to invoke the package's build hook.

3. The `zz build` command discovers all installed packages with `[native]` sections, invokes their build hooks, and links the results.

---

## 4. §2c: VM Interpreter Path — dlopen-Based Loading

### Architecture

```
Plugin .so/.dylib
├── Exports: zz_plugin_register (well-known entrypoint)
│             zimg_init, zimg_load, ... (native function symbols)
└── Metadata: version stamp embedded in a known symbol
```

### Loading process

When `zz run` loads a program that imports a native package:

1. **Discovery:** The interpreter reads the package's `plugin.zzi` manifest to know which native functions exist.

2. **dlopen:** The interpreter loads the package's shared library:
   ```rust
   #[cfg(unix)]
   let handle = unsafe { dlopen(path, RTLD_NOW) };
   
   #[cfg(windows)]
   let handle = unsafe { LoadLibraryW(path) };
   ```

3. **Version check:** The loader reads the version stamp symbol (`zz_plugin_version`) from the loaded library and compares it against the expected rustc version + manifest format version. Refuses to load on mismatch with a clear error.

4. **Registration:** The loader calls the well-known entrypoint:
   ```rust
   type RegisterFn = extern "C" fn(natives: &mut HashMap<String, NativeEntry>);
   let register: RegisterFn = unsafe { dlsym(handle, "zz_plugin_register") };
   register(&mut interp.natives);
   ```

5. **Symbol resolution:** Each native function symbol (e.g., `zimg_load`) is resolved via `dlsym` and wrapped in a `NativeEntry` with the correct arity (derived from the `.zzi` manifest).

### Plugin author's Rust code

A plugin's `lib.rs` looks like:

```rust
use zz_runtime::{NativeEntry, NativeFn, Interp, Value, Span, EvalError};

// Well-known entrypoint — called by the VM loader
#[no_mangle]
pub extern "C" fn zz_plugin_register(natives: &mut HashMap<String, NativeEntry>) {
    natives.insert("zimg.init".into(), NativeEntry { arity: 0, f: zimg_init });
    natives.insert("zimg.load".into(), NativeEntry { arity: 1, f: zimg_load });
    // ... etc
}

// Native implementations follow the C-ABI-only rule:
// All args/returns are zz_value (which is a C-union, safe across FFI boundaries)
fn zimg_init(_interp: &mut Interp, _args: &mut Vec<Value>, span: Span) -> Result<Value, EvalError> {
    // Call into C wrapper via the C FFI
    let code = unsafe { zimg_init_c() };
    Ok(Value::Int(code))
}

extern "C" { fn zimg_init_c() -> i32; }
```

### C-ABI-only boundary rule

Every function crossing the plugin boundary must use only C-ABI-safe types:
- `zz_value` (a C-compatible union — the ZZ runtime's universal value representation)
- `int`, `float`, `bool` (scalars)
- `*const void`, `*mut void` (pointers)
- Never: `String`, `Vec<T>`, `Box<T>`, trait objects, Rust structs with non-C layout

This is enforced by the `extern "C"` annotation and validated at plugin-load time via the version stamp.

### Version stamping

The plugin embeds its build environment as a well-known symbol:

```rust
#[no_mangle]
pub static ZZ_PLUGIN_RUSTC_VERSION: &str = env!("RUSTC_BOOTSTRAP");
// or more precisely:
#[no_mangle]
pub static ZZ_PLUGIN_ABI_VERSION: u32 = 1; // bumped on breaking ABI changes
```

The loader checks:
1. `ZZ_PLUGIN_ABI_VERSION` matches the expected version (defense-in-depth)
2. The rustc version matches (or is compatible per a semver check)

---

## 5. §2d: Symbol/Name Collision Avoidance

### Convention: package-name-prefixed C symbols

All plugin-exported C symbols must be prefixed with the package name:

```
zimg_init          → zimg_init       (package "zimg")
zimg_load          → zimg_load
otherpkg_do_thing  → otherpkg_do_thing (package "otherpkg")
```

The `.zzi` manifest declares the ZZ-visible names (e.g., `zimg.init`, `zimg.load`). The build system or plugin author is responsible for ensuring the underlying C symbols match the expected names.

### ZZ-level namespacing

Plugin functions are imported via the standard `import` mechanism:

```zz
import zimg    // loads plugin.zzi, registers native functions under "zimg.*"
import zimg.image  // if the plugin ships ZZ modules too
```

The checker resolves `zimg.load()` against the manifest's `FuncSig` entries. No collision with stdlib names because the package name is part of the qualified name.

### C-symbol collision

If two plugins export the same C symbol name (e.g., both export `init`), the linker will fail with a duplicate symbol error. This is caught at build time, not runtime. The prefix convention (`zimg_init`, `otherpkg_init`) prevents this.

---

## 6. §2e: Cross-Platform Scope

### v1 (this project): Linux + macOS

Both use POSIX `dlopen`/`dlsym`/`dlclose` for the VM path. The AOT path is platform-independent (just static linking).

| Platform | AOT | VM |
|----------|-----|-----|
| Linux | ✅ static linking | ✅ `dlopen`/`dlsym` |
| macOS | ✅ static linking | ✅ `dlopen`/`dlsym` |
| Windows | ✅ static linking | ❌ Deferred — `LoadLibrary`/`GetProcAddress` follow-up |

### Windows follow-up (tracked, not in v1)

Windows uses `LoadLibraryW`/`GetProcAddress` instead of `dlopen`/`dlsym`. The difference is:
- `dlopen` returns a handle; `LoadLibraryW` returns an `HMODULE`
- `dlsym` takes handle + name; `GetProcAddress` takes HMODULE + name
- Error handling: `dlerror()` vs `GetLastError()`

This is a thin platform-abstraction layer (~20 lines of code). It's deferred to keep v1 focused, but tracked as a follow-up issue. The design is platform-agnostic — the abstraction layer hides the differences.

---

## 7. Milestones

### Milestone 1: Investigation & Design ✅ (this document)

### Milestone 2: Manifest format + type-checker extension
- Implement `load_plugin_manifest()` in `zz_checker`
- Parse `.zzi` files using existing parser
- Validate metadata header (version, rustc)
- Unit tests: valid manifest, malformed manifest, version mismatch, missing fields
- **Gate:** Checker can load and validate external function signatures from a `.zzi` file

### Milestone 3: AOT path
- Extend `zz build` to discover dependencies with `[native]` sections
- Invoke package build hooks, compile native glue, link into final binary
- Inject `extern` declarations from `.zzi` into generated C
- E2E test: toy native package (`add(a: int, b: int) -> int`) consumed by a sample ZZ project, produces correctly linked binary, no edits to `zz_lang` source
- **Gate:** Toy package works end-to-end via AOT

### Milestone 4: VM interpreter path
- Implement dlopen-based loader (Linux + macOS)
- Well-known `zz_plugin_register` entrypoint
- Version stamp validation
- C-ABI-only boundary enforcement
- E2E test: same toy package works via `zz run`
- ABI-mismatch test: build plugin with wrong version stamp, confirm loader refuses it
- **Gate:** Toy package works end-to-end via VM, ABI mismatch test passes

### Milestone 5: zimg migration
- Port zimg's C wrapper + Rust native glue onto this system
- zimg becomes an independent repo consumed via `zz pm`
- Full E2E: load → resize → save via both AOT and VM paths
- **Gate:** zimg works as independent package, no monorepo edits

### Milestone 6: Documentation
- Plugin author guide (manifest format, ABI rules, build_hook conventions)
- API reference for the registration/loading mechanisms
- Platform support matrix

---

## 8. Testing Discipline

| Test | Milestone | What it proves |
|------|-----------|----------------|
| Manifest parsing: valid | 2 | Checker loads correct FuncSigs |
| Manifest parsing: malformed | 2 | Clear error, not panic |
| Manifest parsing: version mismatch | 2 | Refuses incompatible manifests |
| AOT toy package | 3 | Static linking of external native works |
| VM toy package | 4 | dlopen loading works |
| ABI mismatch rejection | 4 | Loader refuses incompatible plugins |
| zimg full pipeline | 5 | Real-world native library works |
| `cargo clippy -D warnings` | all | Code quality |
| `cargo fmt --check` | all | Formatting |
| `cargo test --all` | all | No regressions |

---

## 9. Files to Create/Modify

### New files
| File | Purpose |
|------|---------|
| `crates/zz_plugin/` | New crate: manifest parsing, dlopen loader, plugin types |
| `crates/zz_plugin/src/lib.rs` | Public API |
| `crates/zz_plugin/src/manifest.rs` | `.zzi` parser + metadata validation |
| `crates/zz_plugin/src/loader.rs` | dlopen-based VM loader |
| `crates/zz_plugin/src/abi.rs` | ABI version stamps, platform abstraction |
| `docs/plugin-author-guide.md` | Third-party author documentation |

### Modified files
| File | Change |
|------|--------|
| `crates/zz_checker/src/checker/mod.rs` | Accept external `FuncSig` maps from plugin manifests |
| `crates/zz_codegen/src/ffi.rs` | Link multiple native staticlibs (not just `zz_native_rt`) |
| `crates/zz_codegen/src/compile.rs` | Accept extra object files / staticlibs from plugin build hooks |
| `crates/zz_cli/src/build.rs` | Discover and invoke plugin build hooks |
| `crates/zz_cli/src/session.rs` | Load plugins via dlopen for VM path |
| `crates/zz_cli/Cargo.toml` | Add `zz_plugin` dependency |
| Root `Cargo.toml` | Add `zz_plugin` to workspace members |

---

## 10. Risks & Mitigations

| Risk | Severity | Mitigation |
|------|----------|------------|
| dlopen ABI instability across Rust versions | High | Version stamp checks + C-ABI-only boundary rule |
| Plugin build hooks fail silently | Medium | Build hook must emit specific output format; validation on receipt |
| Two plugins export same C symbol | Low | Prefix convention + linker catches at build time |
| Windows dlopen deferred indefinitely | Low | Tracked as explicit follow-up, not forgotten |
| `.zzi` parser bugs | Medium | Reuse existing ZZ parser — minimal new parsing code |
| `zz pm` schema changes conflict with plugin design | Medium | Design coordination in §3 (AOT path section) — define `[native]` schema now, implement later |

---

## 11. Coordination with `zz pm`

The `[native]` section in `zz.toml` should be designed now (even if `zz pm`'s schema change is implemented later) to avoid incompatibility:

```toml
[package]
name = "zimg"
version = "0.1.0"

[native]
build = "build.sh"
manifest = "plugin.zzi"
pkg-config = "vips >= 8.6"    # optional: system dependency declaration
```

The `zz pm install` command gains awareness of `[native]` sections:
- It invokes the build hook during install (or defers to build time)
- It records which packages have native components
- The `zz build` command queries this to know which plugins to link

This coordination ensures the two systems aren't designed independently and end up incompatible.
