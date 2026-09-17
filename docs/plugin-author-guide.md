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
    func my_init() -> int
    func my_process(handle: *mut void, data: int) -> int
    func my_release(handle: *mut void)
    func my_get_result() -> *mut void
}
```

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
| `int` | `int64_t` / `long long` | 64-bit integer |
| `float` | `double` | 64-bit float |
| `bool` | `int` (0 or 1) | Integer boolean |
| `*const void` | `const void*` | Opaque pointer (read-only) |
| `*mut void` | `void*` | Opaque pointer (mutable) |
| `void` | (no return) | Functions returning nothing |

**Not allowed:** `str`, `String`, `Vec<T>`, arrays, structs, enums, `Option`, `Result`, closures, function pointers.

Strings: `str` is not a C ABI type. Pass string data as `*const void` (pointer to null-terminated C string) or `*mut void` (buffer pointer + separate length parameter).

---

## 2. Package Structure

### Minimal Plugin (declarative only)

```
my-plugin/
├── plugin.zzi          Interface declarations
├── zz.toml             Package manifest
└── src/
    └── main.zz         ZZ code (optional)
```

### Full Plugin (with native crate)

```
my-plugin/
├── plugin.zzi          Interface declarations
├── zz.toml             Package manifest with [native] section
├── build.sh            Build hook (optional)
├── native/             Rust crate producing .so/.a
│   ├── Cargo.toml
│   ├── build.rs        Build script (optional)
│   └── src/
│       └── lib.rs      Native implementations
├── csrc/
│   ├── wrapper.h       C wrapper API (optional)
│   └── wrapper.c       C wrapper implementation (optional)
└── src/
    └── main.zz         ZZ ergonomic layer
```

---

## 3. `zz.toml` Configuration

Add the `[native]` section to declare build hooks and system dependencies:

```toml
[package]
name = "my-plugin"
version = "0.1.0"
rust-version = "1.85.0"

[native]
build = "build.sh"                      # build hook script
manifest = "plugin.zzi"                 # optional, default: plugin.zzi
pkg-config = "libfoo >= 1.0"           # optional: system dependency
```

### Fields

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `build` | string | no | Path to build hook script, relative to package root |
| `manifest` | string | no | Path to interface file, default: `plugin.zzi` |
| `pkg-config` | string | no | pkg-config query for system dependencies |

---

## 4. Build Hook (`build.sh`)

The build hook is invoked by `zz build` before compilation. It must:

1. **Compile** the C wrapper + Rust native crate
2. **Output** artifacts to `build/` directory

### Required Outputs

| File | Description |
|------|-------------|
| `build/*.o` | Compiled C wrapper object files |
| `build/lib*.a` | Static library (for AOT linking) |
| `build/lib*.so` | Shared library (for VM dlopen, Linux) |
| `build/lib*.dylib` | Shared library (for VM dlopen, macOS) |
| `build/ldflags.txt` | Linker flags (one line, space-separated) |
| `build/cflags.txt` | Compiler flags (one line, space-separated) |

### Example `build.sh`

```bash
#!/bin/sh
set -e

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
BUILD_DIR="$SCRIPT_DIR/build"

mkdir -p "$BUILD_DIR"

# Compile C wrapper
CFLAGS=$(pkg-config --cflags vips)
LIBS=$(pkg-config --libs vips)

cc -c csrc/wrapper.c \
    -o build/wrapper.o \
    $CFLAGS -Wall -Wextra -Werror -fPIC

# Compile Rust native crate
cd native
cargo build --release

# Copy artifacts
cp target/release/libmy_plugin.a build/
cp target/release/libmy_plugin.so build/  # or .dylib on macOS

# Save flags
echo "$CFLAGS" > build/cflags.txt
echo "$LIBS" > build/ldflags.txt
```

### Build Hook Contract

- The hook runs from the package root directory
- It must exit 0 on success, non-zero on failure
- stderr output is shown to the user on failure
- stdout output is captured (not displayed)
- `build/` directory is created by the hook (or by `zz build` if it doesn't exist)

---

## 5. Rust Native Crate

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

Strings are not C-ABI-safe. Two approaches:

### Approach A: C-string pointer (null-terminated)

```zz
// plugin.zzi
extern "C" {
    func my_load(path: *const void) -> int
}
```

```rust
// In lib.rs
fn native_my_load(
    _interp: &mut zz_runtime::eval::Interp,
    args: &mut Vec<zz_runtime::Value>,
    _span: zz_runtime::Span,
) -> Result<zz_runtime::Value, zz_runtime::EvalError> {
    // Convert ZZ int handle to pointer
    let path_ptr = match &args[0] {
        zz_runtime::Value::Int(i) => *i as *const i8,
        _ => return Err(zz_runtime::EvalError::new("expected pointer", zz_runtime::Span::new(0, 0))),
    };
    let path = unsafe { CStr::from_ptr(path_ptr) };
    // ... use path ...
    Ok(zz_runtime::Value::Int(0))
}
```

### Approach B: Pointer + length (buffer)

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
import my-plugin

fn main() {
    let handle = my-plugin.init()
    assert handle == 0
    my-plugin.release(handle)
    print("plugin_ok")
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

### `str` is not C-ABI

`str` is a Rust string slice — it cannot cross FFI boundaries. Use `*const void` (pointer to C string) or `*mut void` (buffer pointer + length).

### No `Copy` on `Value`

`Value` does not implement `Copy`. Use `match &value { ... }` (borrow) not `match value { ... }` (move) in native functions.

### `Value::Str` is `Box<String>`

When creating a string value: `Value::Str(Box::new("hello".to_string()))`, not `Value::Str("hello".to_string())`.

### `EvalError` is a struct

`EvalError::new(message, span)` creates an error. There is no `EvalError::TypeError` variant — use `EvalError::new(format!("expected X, got Y"), span)`.

### FFI warnings

The `RegisterCallback` type and `zz_plugin_register` function produce `improper_ctypes_definitions` warnings because `NativeFn` is a Rust function pointer. Suppress with `#[allow(improper_ctypes_definitions)]`. This is safe in practice because `NativeFn` uses C calling convention.

### Plugin libraries are never unloaded

In v1, loaded plugin shared libraries stay in memory for the process lifetime. Dropping a `PluginLib` handle is safe but does not actually unload the library.

---

## 15. Example: Complete Plugin

See the `zimg` package for a complete working example:

- **Repository:** `github.com/user/zimg`
- **Interface:** `plugin.zzi` (11 functions)
- **Native crate:** `native/src/lib.rs` (Rust implementations)
- **C wrapper:** `csrc/zimg_wrapper.c` (libvips bridge)
- **Build hook:** `build.sh` (compiles everything)
- **ZZ wrapper:** `src/zimg.zz` (ergonomic ZZ layer)
