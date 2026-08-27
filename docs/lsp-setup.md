# ZZ LSP — Editor Configuration

The ZZ language server (`zz-lsp`) provides diagnostics, completion,
hover, go-to-definition, find-references, rename, code actions,
formatting, inlay hints, semantic tokens, and folding ranges.

## Building

```bash
cargo build --release -p zz_lsp
# Binary: target/release/zz-lsp
```

## VS Code / VSCodium

Install the **ZZ Language** extension, or configure manually in
`.vscode/settings.json`:

```jsonc
{
  // Point to the zz-lsp binary.
  "zz.languageServer.path": "target/release/zz-lsp",

  // Optional: enable format-on-save.
  "[zz]": {
    "editor.defaultFormatter": "your-publisher.zz-lang",
    "editor.formatOnSave": true
  }
}
```

If using a custom build path, set `zz.languageServer.path` to the
absolute path of your `zz-lsp` binary.

## Neovim (nvim-lspconfig)

```lua
-- In your LSP setup (e.g. after/lsp/zz.lua or init.lua):
local lspconfig = require("lspconfig")
local configs = require("lspconfig.configs")

-- Register the config if not already present.
if not configs.zz_lsp then
  configs.zz_lsp = {
    default_config = {
      cmd = { "zz-lsp" },
      filetypes = { "zz" },
      root_dir = lspconfig.util.root_pattern(".git", "*.zz"),
    },
  }
end

-- Start the server.
lspconfig.zz_lsp.setup({})

-- Optional: format on save.
vim.api.nvim_create_autocmd("BufWritePre", {
  pattern = "*.zz",
  callback = function()
    vim.lsp.buf.format({ async = false })
  end,
})
```

Add the file type detection:

```lua
vim.filetype.add({ extension = { zz = "zz" } })
```

## Helix

Add to `~/.config/helix/languages.toml`:

```toml
[language-server.zz-lsp]
command = "zz-lsp"

[[language]]
name = "zz"
language-servers = ["zz-lsp"]
auto-format = true
```

## Features

| Feature | Description |
|---|---|
| Diagnostics | Real-time error and warning reporting |
| Completion | Context-aware completions for variables, functions, fields |
| Hover | Type and documentation on hover |
| Go-to-definition | Navigate to symbol definitions (cross-file) |
| Find references | Find all uses of a symbol (cross-file) |
| Rename | Rename a symbol across all files |
| Code actions | Quick fixes for diagnostics (unused vars, typos) |
| Formatting | `zz fmt` style via `textDocument/formatting` |
| Inlay hints | Parameter names at call sites |
| Semantic tokens | Syntax highlighting from the AST |
| Folding ranges | Collapse function bodies, structs, loops |
| Workspace symbols | Search symbols across the workspace |
| Document symbols | Outline view for a single file |

## Logging

The server uses the `log` crate with `env_logger`.  Set the `RUST_LOG`
environment variable to enable debug output:

```bash
RUST_LOG=debug zz-lsp            # verbose logging
RUST_LOG=zz_lsp=trace zz-lsp     # trace-level for zz_lsp only
```

When using an editor, set the environment variable in your editor
configuration so that `zz-lsp` inherits it.

## CLI Formatter

Format files from the command line:

```bash
zz fmt .              # format all .zz files in-place
zz fmt -c src/        # check formatting without writing (exit 1 if changed)
zz fmt hello.zz       # format a single file
```
