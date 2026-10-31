# `std.process` — spawn and run external processes

Spawn and run external commands. Results cross as `[status, stdout,
stderr]` inside `Result`. Children are opaque `process` handles.

```zz
import std.process

match process.run("ls", ["-la"]) {
    .ok(out)  => println("status {out[0]}"),
    .err(e)   => println(e),
}
match process.run_with_env("make", ["test"], ["RUST_BACKTRACE=1"]) {
    .ok(out)  => println(out[1]),   // stdout
    .err(e)   => println(e),
}

// Non-blocking: spawn then wait
match process.spawn("sleep", ["2"]) {
    .ok(child) => {
        match process.wait(child) {
            .ok(out) => println("done: {out[0]}"),
            .err(e)  => println(e),
        }
    },
    .err(e) => println(e),
}
process.exit(0)
```

## Types

| Type | Notes |
|---|---|
| `process` | Opaque child-process handle from `spawn`. Reap with `process.wait`; read `pid`. |

## Variables

None.

## Functions

| Function | Signature | Notes |
|---|---|---|
| `run(cmd, argv)` | `run(cmd: str, argv: [str]) -> Result<[int \| str \| str], str>` | Run to completion; `[exit, stdout, stderr]`. |
| `run_with_env(cmd, argv, env)` | `run_with_env(cmd: str, argv: [str], env: [str]) -> Result<[int \| str \| str], str>` | Run with `K=V` env entries. |
| `spawn(cmd, argv)` | `spawn(cmd: str, argv: [str]) -> Result<process, str>` | Non-blocking spawn; returns a handle. |
| `wait(child)` | `wait(child: process) -> Result<[int \| str \| str], str>` | Reap a spawned child; `[exit, stdout, stderr]`. |
| `pid()` | `pid() -> int` | Current process ID. |
| `exit(code)` | `exit(code: int) -> unit` | Terminate the process with `code`. |
