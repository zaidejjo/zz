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
