# Process — `std.process`

Run and spawn external processes. Results cross as `[status, stdout, stderr]` inside `Result`; spawned children are opaque `"process"` handles.

```zz
import std.process
```

## Purpose

`std.process` runs commands (`run`, `run_with_env`), spawns non-blocking children (`spawn` + `wait`), queries `pid`, and terminates with `exit`. Env entries must be `K=V` — check-time errors name the bad entry.

## Quickstart

```zz
import std.process

match process.run("ls", ["-la"]) {
    .ok(out)  => println("status {out[0]}"),
    .err(e)   => println(e),
}
match process.run_with_env("make", ["test"], ["RUST_BACKTRACE=1"]) {
    .ok(out)  => println(out[1]),
    .err(e)   => println(e),
}

// Non-blocking spawn
func main() -> Result<int, str> {
    child := process.spawn("ls", ["-la"])?
    triple := process.wait(child)?      // [status, stdout, stderr]
    println(process.pid())
    return .ok(0)
}
```

## Exhaustive reference

| Function | Signature | Description | Example |
|----------|-----------|-------------|---------|
| `process.run` | `process.run(cmd: str, argv: [str]) -> Result<[int,str,str], str>` | Run, wait, `[status,stdout,stderr]` | |
| `process.run_with_env` | `process.run_with_env(cmd, argv, env: [str]) -> Result<[int,str,str], str>` | Run with env entries `K=V` | |
| `process.spawn` | `process.spawn(cmd: str, argv: [str]) -> Result<process, str>` | Spawn non-blocking child handle | |
| `process.wait` | `process.wait(child: process) -> Result<[int,str,str], str>` | Wait for child, get triple | |
| `process.exit` | `process.exit(code: int) -> unit` | Terminate current process | |
| `process.pid` | `process.pid() -> int` | Current process ID | |

## Error model

- `run`/`run_with_env` return `.err` on spawn failure; output is captured regardless of exit code.
- `spawn`/`wait` return `Result<_, str>`.
- `exit` is total (`unit`) — terminates the process.
- `pid` is total (`int`).

## Gotchas

- **Env entries must be `K=V` format** — check-time errors name the bad entry.
- **`spawn` is non-blocking** — always `wait` to reap the child and get output (avoid zombies).
- **`[status, stdout, stderr]` are heterogeneous** — index by position: `[0]`=int status, `[1]`=str stdout, `[2]`=str stderr.
- **`exit` does not return** — terminates the current process immediately.

## Production patterns

```zz
import std.process

func run_capture(cmd: str, args: [str]) -> Result<str, str> {
    return match process.run(cmd, args) {
        .ok(out) => {
            if out[0] != 0 {
                return .err(out[2])   // stderr on failure
            }
            .ok(out[1])               // stdout
        }
        .err(e) => .err(e),
    }
}
```

