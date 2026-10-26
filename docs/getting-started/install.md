# Install

## Recommended: release script

```bash
curl -fsSL https://zz-lang.pages.dev/install.sh | sh
zz --version
zz setup    # PATH + completions for bash/zsh/fish, run once
```

Windows:

```powershell
irm https://zz-lang.pages.dev/install.ps1 | iex
```

## From source

Needs stable Rust + `clang` 18+ (or `zig cc`) for AOT builds:

```bash
git clone https://github.com/zaidejjo/zz
cd zz
cargo install --path crates/zz_cli
zz --version
```

VM-only work (`zz run`, `zz check`, `zz test`) needs no C toolchain.
`zz build` / `zz run --native` / `zz test --engine native` need one.

## No toolchain? Use the managed Zig backend

```bash
zz toolchain install
zz toolchain status
zz build -p --cc zig main.zz
```

Pin per project with `[native]`, relocate with `ZZ_TOOLCHAIN_ROOT` for hermetic CI.

## Health check

```bash
zz doctor          # binary, clang, shell, git, registry
zz doctor --fix
zz upgrade         # self-update
zz cache clean     # reclaim ~/.zz/cache when disk is low
```

Next: [Quick start](quick-start.md).
