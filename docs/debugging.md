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
(structs/dicts/options expand by field, arrays/tuples page in
`[0..99]` windows), `continue`, `next` (step-over), `stepIn`,
`stepOut`, `disconnect`. Anything else answers `success: false`
instead of hanging.

## Limitations (current)

- **Single file**: breakpoints resolve in the launched file; imports
  execute but carry no source labels yet.
- **`input()` corrupts the session**: the debuggee shares stdio with
  the protocol — reading stdin steals framing bytes. Don't.
- **Long sequences page**: arrays/tuples over 100 items expand as
  `[0..99]`, `[100..199]`, … windows.
- **Function params** may not appear in Locals (slots + env only).
