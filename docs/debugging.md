# ZZ Debugging (DAP)

The `zz-dap` binary speaks the Debug Adapter Protocol over stdio:

```bash
cargo build -p zz-dap
# Binary: target/debug/zz-dap
```

## VS Code / VSCodium

`.vscode/launch.json`:

```jsonc
{
  "version": "0.2.0",
  "configurations": [
    {
      "type": "zz",
      "request": "launch",
      "name": "Debug ZZ file",
      "program": "${file}",
      // Point to the zz-dap binary (extension setting or absolute path).
    }
  ]
}
```

Breakpoints, step-over, call stack, and Locals work. Run control:
continue, step-over (`next`), step-into (`stepIn`), step-out
(`stepOut`), disconnect.

## Neovim (nvim-dap)

```lua
local dap = require("dap")
dap.adapters.zz = { type = "executable", command = "zz-dap" }
dap.configurations.zz = {
  { type = "zz", request = "launch", name = "Debug file", program = "${file}" },
}
```

## Protocol subset

Implemented: `initialize`, `launch {program}`, `setBreakpoints`,
`configurationDone`, `threads`, `stackTrace`, `scopes`, `variables`
(leaf values only — structs render as text), `continue`, `next`
(step-over), `disconnect`. Anything else answers `success: false`
instead of hanging.

## Limitations (current)

- **Single file**: breakpoints resolve in the launched file; imports
  execute but carry no source labels yet.
- **`input()` corrupts the session**: the debuggee shares stdio with
  the protocol — reading stdin steals framing bytes. Don't.
- **Leaf-only variables**: no expandable children for structs/arrays.
- **Function params** may not appear in Locals (slots + env only).
