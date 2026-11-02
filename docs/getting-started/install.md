# Install

## Recommended: release script

```bash
curl -fsSL https://zz-lang.pages.dev/install.sh | sh
zz --version
zz setup    # PATH + completions for bash/zsh/fish/powershell (run once)
```

Windows:

```powershell
irm https://zz-lang.pages.dev/install.ps1 | iex
```

> **Windows native builds:** `zz build` / `zz run --native` / `zz test --engine native`
> are **not supported on Windows yet** — the AOT runtime still needs its Win32
> threads/sockets ported (`<pthread.h>`, `fcntl`, `poll`). VM mode (`zz run`,
> `zz check`, `zz test`), the REPL, `zz-lsp`, and `zz pm` all work on Windows.

## From source

Needs stable Rust + `clang` 18+ (or `zig cc`) for AOT builds:

```bash
git clone https://github.com/zaidejjo/zz
cd zz
cargo install --path crates/zz_cli
zz --version
```

VM-only work (`zz run`, `zz check`, `zz test`) needs **no** C toolchain.
`zz build` / `zz run --native` / `zz test --engine native` need one.

## No C toolchain? Use the managed Zig backend (optional, AOT only)

```bash
zz toolchain install
zz toolchain status
zz build -p --cc zig main.zz
```

Pin per project with `[native]`, relocate with `ZZ_TOOLCHAIN_ROOT` for hermetic CI.

## Health check

```bash
zz doctor          # audit binary, clang, shell, git, registry
zz doctor --fix    # auto-fix what it can
zz upgrade         # self-update zz + zz-lsp from GitHub releases
zz cache clean     # reclaim ~/.zz/cache when disk is low
```

Next: [Quick start](quick-start.md).
