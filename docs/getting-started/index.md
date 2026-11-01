---
slug: 'getting-started/index'
title: "Get Started — ZZ Programming Language"
description: "Install ZZ, run your first program, and learn the project layout, REPL, and cheatsheet."
editUrl: "https://github.com/zaidejjo/zz/edit/dev/docs/getting-started/index.md"
---

# Get Started

Welcome to ZZ.

## Start here (in order)

1. [Download and Install](install.md) — One script.
2. [Quick start](quick-start.md) — A program in under a minute, including an HTTP server.
3. [Project layout](project-layout.md) — `zz.toml`, entry point, imports, tests, benches, assets.
4. [REPL tour](repl-tour.md) — Interactive stdlib probing.
5. [Cheatsheet](cheatsheet.md) — One-page reference for commands and syntax.

## ZZ in 30 seconds

- **Values, not references.** Variables hold independent values; functions get copies. No borrow checker, no aliasing to reason about.
- **Inferred, then checked.** `:=` infers, `: T =` pins. The checker reports every error in one pass with a fix hint.
- **Pipelines over nesting.** `|>` passes the left value as the first argument; `match` + `?` + `??` keep happy-path code flat.
- **Same on VM and native.** `zz run` (VM) and `zz build` (AOT) agree bit-for-bit on ints and floats.

## Browse by topic

- [Language](/docs/language/) — Values, types, control flow, modules, and the mental model behind ZZ.
- [Standard library](/docs/stdlib/) — Strings, data, HTTP, databases, crypto, concurrency, and more.
- [Tooling](/docs/tooling/) — CLI, formatter, test runner, package manager, editor integration.
- [Guides](/docs/guides/) — Recipes, diagnostics explained, VM vs native parity.
- [Compiler](/docs/compiler/) — Architecture, checker, runtime, and code generation.

## API reference

The complete stdlib signature index lives on one page: [Stdlib reference](../stdlib/reference.md).

Next: [Install](install.md).
