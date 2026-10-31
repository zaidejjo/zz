# `std.chan` — message passing channels

A `chan` is a single-producer, single-consumer message channel for passing
values between concurrent green threads (see `std.task`). Channels are
typed: a `chan` carries one element type `T`.

```zz
import std.chan
import std.task

ch := chan.create()        // chan (opaque handle)
task.spawn(|| {
    chan.send(ch, "ping")?
})
match chan.recv(ch) {
    .ok(msg) => println(msg) // "ping"
    .err(e)  => println(e),
}
```

## Types

| Type | Notes |
|---|---|
| `chan` | Opaque channel handle. Created with `chan.create`; passed by reference to `send`/`recv`/`try_recv`. |

## Variables

None.

## Functions

| Function | Signature | Notes |
|---|---|---|
| `create()` | `create() -> chan` | Create a new channel. |
| `send(ch, v)` | `send(ch: chan, v: T) -> unit` | Send value `v` of type `T` into `ch`. Blocks until received. |
| `recv(ch)` | `recv(ch: chan) -> T` | Receive the next value from `ch`. Blocks until available. |
| `try_recv(ch)` | `try_recv(ch: chan) -> Option<T>` | Non-blocking receive: `.some(v)` if a message is waiting, `.none` otherwise. |
