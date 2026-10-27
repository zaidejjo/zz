<p align="center">
  <img src="https://zz-lang.pages.dev/500-500-logo.png" width="180" alt="ZZ logo">
</p>

<h1 align="center">ZZ</h1>

<p align="center">Fast systems language: native AOT, green threads, zero GC pauses.</p>

<p align="center">
  <a href="https://zz-lang.pages.dev">Website</a> |
  <a href="https://zz-lang.pages.dev/getting-started">Getting started</a> |
  <a href="https://zz-lang.pages.dev/language-reference">Learn</a> |
  <a href="https://zz-lang.pages.dev/stdlib/overview">Documentation</a> |
  <a href="./CONTRIBUTING.md">Contributing</a>
</p>

## Why ZZ?

- Native binaries via `zz build`, fast iteration via `zz run`.
- Green threads and channels for concurrent pipelines.
- Inferred types, pipelines (`|>`), pattern matching.
- Diagnostics that suggest fixes, plus `zz fix` and `zz fmt`.
- Batteries included: HTTP, JSON, fs, math, SQL.

## How is ZZ different from Rust?

ZZ is not syntactic sugar on top of Rust. It is an independent language
with its own frontend, type checker, tree-walk VM, and AOT backend
(ZZ → C → native binary), plus its own standard library.

- **Memory without a borrow checker.** ZZ has value semantics: functions
  receive copies from the programmer's perspective, there are no
  references, no lifetimes, no `&`/`&mut`. The compiler/runtime may elide
  copies internally (copy-on-write, in-place slots), but aliasing is never
  observable. Rust gives you control via ownership/borrows; ZZ trades that
  control for simpler reasoning.
- **Concurrency model.** ZZ ships green threads and channels as the default
  pipeline primitive. No `async`/executors to wire up for the common case.
- **Scope.** ZZ targets services, CLIs, and data pipelines with batteries
  included (HTTP, JSON, fs, SQL). It cannot do everything Rust can: no
  `unsafe`, no zero-cost abstractions, no `no_std`/embedded, no manual
  layout control. For those, use Rust (including as a ZZ native plugin).
- **Interop.** ZZ plugins speak C ABI (`plugin.zzi`); Rust crates can back
  plugins, but the ZZ side never exposes Rust semantics.

## Quick start

Install (main way, latest GitHub release). Re-run to update:

```bash
curl -fsSL https://zz-lang.pages.dev/install.sh | sh
```

Windows:

```powershell
irm https://zz-lang.pages.dev/install.ps1 | iex
```

Verify:

```bash
zz --version
zz setup    # PATH + shell completions for bash/zsh/fish (run once)
```

Hello world (`hello.zz`):

```zz
name := input("What is your name? ")
println("Hello, {name}!")
```

```bash
zz run hello.zz
```

REPL, check, format:

```bash
zz            # REPL (:help, :quit)
zz check src/
zz fmt src/
```

Install a project as a tool (`~/.zz/bin`):

```bash
zz install --path .
```

Keep the toolchain healthy:

```bash
zz upgrade   # self-update zz + zz-lsp (+ bundled runtime/lib) to the latest release
zz doctor    # audit binary, clang, shell, git, registry (add --fix)
```

## Native runtime & build environment

`zz build` and `zz run --native` compile ZZ → C → native binary via Clang
(default) or zig cc (`--cc zig`). The native runtime
(`libzz_native_rt.a`, providing FFI natives like regexp, base64, float
formatting, SQL, crypto) is linked into every AOT binary.

**Source checkout:** no setup needed. `zz build` finds the workspace
root automatically and builds `libzz_native_rt.a` via cargo — no
`ZZ_NATIVE_RT_DIR`, no system C library, no stub shell variables.

**Release zip:** ships a prebuilt `libzz_native_rt.a` (`.lib` on
Windows) plus a bundled Zig toolchain (`zig` + its `lib/`, the std/libc
sources Zig loads from `<exe>/lib`; ~65MB unpacked), so `zz build` works
immediately after download with zero external tools. `zz toolchain
install` stays optional for pinned zig updates.

**Supported platforms:** Linux, macOS, and Windows on both `x86_64` and
`aarch64` (six release zips, same command on all of them).

Two caveats, both tied to optional system libraries rather than the
toolchain:

- AOT programs that reach the HTTP *client* (`http.fetch`, `http.post`)
  or SQL (`sqlz`) link `libcurl`/`sqlite3` and need their dev headers
  (`libcurl4-openssl-dev libsqlite3-dev` on Debian/Ubuntu, `brew install
  curl sqlite` on macOS, `vcpkg install curl sqlite3` on Windows).
  Programs that do not touch those natives compile with neither.
- The AOT HTTP *server* (`http.server`, `http.listen`) is Linux-only
  today: it is a fork+epoll design. On macOS/Windows those calls return a
  clear "requires Linux" error; VM mode serves fine everywhere.
- Loading native plugins in **VM mode** (`zz run` with a `plugin.zzi`
  dependency) is Unix-only: the loader is built on `dlopen`. On Windows
  use `zz build` (AOT), which links the plugin's static archive; the
  declarative `backend = "cc"` / `backend = "rust"` builds produce a
  `.lib` + `.dll` pair there.

**VM mode** (`zz run` without `--native`) needs nothing extra — it's
pure Rust bytecode interpretation, no linking involved.

Verify your environment:

```bash
zz doctor    # reports C backend, native runtime, cargo, and more
```

## Documentation

- [Getting started](https://zz-lang.pages.dev/getting-started)
- [Syntax](https://zz-lang.pages.dev/syntax)
- [Language reference](https://zz-lang.pages.dev/language-reference)
- [Standard library](https://zz-lang.pages.dev/stdlib/overview)
- [Playground](https://zz-lang.pages.dev/playground)

## Contributing

See [CONTRIBUTING.md](./CONTRIBUTING.md) for setup, tests, and PRs.

## License

Apache-2.0 — see [LICENSE](./LICENSE).
