# SQL — `std.sqlz`

SQLite foundation (canonical module name `sqlz`), plus PostgreSQL and MySQL wire-protocol drivers, and unified closure transactions for all backends.

```zz
import std.sqlz
```

## Purpose

`std.sqlz` is the only SQLite module. `open(path) -> db`, `exec(db, sql) -> int` (rows changed), `query(db, sql) -> [T]` (rows mapped to structs), `close(db)`. The SQL param is checked by `verify_sql_params` (Fmt segments become `?N` bound params); `query`'s return unifies with the caller's annotation (`let users: [User] = sqlz.query(...)`) so the runtime can map columns positionally into that struct. Handle variables named `db` (`db := sqlz.open(..); db.exec(..)`) are method calls.

Postgres: `import std.sqlz.postgres as pg` — `pg.connect`, `pg.exec`, `pg.query`, `pg.close`. MySQL: `import std.sqlz.mysql as my` — same shape. Both wire-protocol drivers; MySQL uses binary prepared statements.

Transactions: `sqlz.transaction(db, fn(tx) { ... })` — BEGIN, run closure, COMMIT on `.ok(value)`, ROLLBACK on `.err(message)`.

## Quickstart

```zz
import std.sqlz

struct User { id: int, name: str }

func main() -> Result<int, str> {
    db := sqlz.open("app.db")
    sqlz.exec(db, "CREATE TABLE IF NOT EXISTS users (id INTEGER, name TEXT)")
    sqlz.exec(db, "INSERT INTO users VALUES (1, 'Ada')")

    users := sqlz.query(db, "SELECT id, name FROM users")
    for u in users { println(u.name) }

    sqlz.close(db)
    return .ok(0)
}
```

## Transactions

```zz
import std.sqlz

func main() -> Result<int, str> {
    db := sqlz.open("app.db")

    result := sqlz.transaction(db, |tx| -> int {
        sqlz.exec(tx, "UPDATE users SET name = 'Bob' WHERE id = 1")
        return 42
    })
    // COMMIT on .ok, ROLLBACK on .err
    return .ok(0)
}
```

## Exhaustive reference

### SQLite

| Function | Signature | Description | Example |
|----------|-----------|-------------|---------|
| `sqlz.open` | `sqlz.open(path: str) -> db` | Open SQLite DB | `sqlz.open("app.db")` |
| `sqlz.exec` | `sqlz.exec(db: db, sql: str) -> int` | Execute, rows changed | `sqlz.exec(db, "...")` |
| `sqlz.query` | `sqlz.query(db: db, sql: str) -> [T]` | Query rows mapped to structs | `sqlz.query(db, "...")` |
| `sqlz.close` | `sqlz.close(db: db) -> unit` | Close DB | `sqlz.close(db)` |
| `sqlz.transaction` | `sqlz.transaction(db, fn) -> Result[T, str]` | BEGIN/commit/rollback closure | |

### PostgreSQL (`std.sqlz.postgres`)

| Function | Signature | Description |
|----------|-----------|-------------|
| `sqlz.postgres.connect` | `connect(conninfo: str) -> db` | URL or keyword form |
| `sqlz.postgres.exec` | `exec(db, sql) -> int` | Rows changed |
| `sqlz.postgres.query` | `query(db, sql) -> [T]` | Rows mapped to structs |
| `sqlz.postgres.close` | `close(db) -> unit` | Close |

### MySQL (`std.sqlz.mysql`)

| Function | Signature | Description |
|----------|-----------|-------------|
| `sqlz.mysql.connect` | `connect(conninfo: str) -> db` | URL or keyword form |
| `sqlz.mysql.exec` | `exec(db, sql) -> int` | Rows changed |
| `sqlz.mysql.query` | `query(db, sql) -> [T]` | Rows mapped to structs |
| `sqlz.mysql.close` | `close(db) -> unit` | Close |

## Method syntax

```zz
import std.sqlz

func main() -> Result<int, str> {
    db := sqlz.open("app.db")
    _ = db.exec("CREATE TABLE ...")
    _rows := db.query("SELECT * FROM users")
    db.close()
    return .ok(0)
}
```

## Gotchas

- **`sqlz` is the only SQLite module** — there is no `std.sqlite`.
- **SQL is checked at compile time** via `verify_sql_params`; Fmt segments become bound params.
- **`query` return type unifies with the caller's annotation** for positional column mapping — annotate your struct.
- **No `db.transaction`** — use `sqlz.transaction` only.
- **Postgres/MySQL are wire-protocol drivers** — not embedded; require a running server.

## Production patterns

```zz
import std.sqlz

func main() -> Result<int, str> {
    db := sqlz.open("app.db")

    // Transaction with rollback on error
    result := sqlz.transaction(db, |tx| -> int {
        sqlz.exec(tx, "INSERT INTO ledger ...")
        sqlz.exec(tx, "UPDATE balance ...")
        return 0
    })
    match result {
        .ok(_) => println("committed"),
        .err(e) => println("rolled back: {e}"),
    }
    return .ok(0)
}
```

