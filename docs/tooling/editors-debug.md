# Editors and debugging — LSP + DAP

## LSP (`zz-lsp`)

Diagnostics, completion, hover, go-to-def, references, rename, code actions,
formatting, inlay hints, semantic tokens, folding. Editor truth is a CI property:
`zz check` set must equal `publishDiagnostics` set
(`cargo test -p zz_lsp --test check_parity`). Divergences fix the analysis gap
or land in `tests/fixtures/lsp_parity_allowlist.toml` with a reason; `FIXED!`
entries are removed same day.

Build:

```bash
cargo build --release -p zz_lsp
# target/release/zz-lsp  (speaks stdio; OpenCode `lsp.zz` entry is forward-compatible)
```

VS Code / VSCodium (`.vscode/settings.json`):

```jsonc
{
  "zz.languageServer.path": "target/release/zz-lsp",
  "[zz]": {
    "editor.defaultFormatter": "your-publisher.zz-lang",
    "editor.formatOnSave": true
  }
}
```

Neovim (`nvim-lspconfig`, `after/lsp/zz.lua`):

```lua
local lspconfig = require("dap")
-- cmd = { "zz-lsp" }, filetypes = { "zz" }
-- root_pattern(".git", "*.zz")
-- triggers: `.`, `(`, `{`, `:` for members/imports/literals/annotations
```

Full: [lsp-setup.md](../lsp-setup.md).

## DAP (`zz-dap`)

Speaks Debug Adapter Protocol over stdio:

```bash
cargo build -p zz_dap
# target/debug/zz-dap
```

VS Code `.vscode/launch.json`:

```jsonc
{
  "version": "0.2.0",
  "configurations": [
    { "type": "zz", "request": "launch", "name": "Debug ZZ file", "program": "${file}" }
  ]
}
```

Neovim (`nvim-dap`):

```lua
local dap = require("dap")
dap.adapters.zz = { type = "executable", command = "zz-dap" }
dap.configurations.zz = {
  { type = "zz", request = "launch", name = "Debug file", program = "${file}" },
}
```

Implemented: `initialize`, `launch {program}`, `setBreakpoints`,
`configurationDone`, `threads`, `stackTrace`, `scopes`, `variables`
(structs/dicts/options by field, arrays/tuples page `[0..99]`), `continue`,
`next`, `stepIn`, `stepOut`, `disconnect`. Else answers `success: false`.

Limits: single file (imports run unlabeled); `input()` steals stdio framing —
don't use under DAP; >100-item sequences page; some fn params absent from Locals.

Full: [debugging.md](../debugging.md).
