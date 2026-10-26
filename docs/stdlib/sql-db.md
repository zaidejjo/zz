# Databases — `std.sqlz` (SQLite) + Postgres + MySQL

Canonical module is `std.sqlz`. There is no `std.db`/`db.*` — `sqlz` only.

## SQLite foundation

```zz
import std.sqlz

func main() -> Result<int, str> {
    db := sqlz.open("app.db")
    sqlz.exec(db, "CREATE TABLE IF NOT EXISTS users (id INTEGER, name TEXT)")
    sqlz.exec(db, "INSERT INTO users VALUES (1, 'Ada')")
    rows := sqlz.query(db, "SELECT id, name FROM users")
    println(rows)
    sqlz.close(db)
    return .ok(0)
}
```

| Function | Notes |
|---|---|
| `open(path)` | `Result<db, str>` |
| `exec(db, sql)` | DDL/DML |
| `query(db, sql)` | rows (JSON-shaped values) |
| `close(db)` | release |
| `transaction(db, fn)` | runs `fn` atomically |

Method dispatch works after `import std.sqlz`: `db.query(...)`, `db.exec(...)`.
See `examples/sqlz_test.zz` and `examples/sqlz_universal.zz`.

## Postgres — `std.sqlz.postgres`

```zz
import std.sqlz.postgres as pg

func main() -> Result<int, str> {
    db := pg.connect("host=127.0.0.1 user=app dbname=app")
    pg.exec(db, "CREATE TABLE IF NOT EXISTS t (id INTEGER)")
    rows := pg.query(db, "SELECT * FROM t")
    println(rows)
    pg.close(db)
    return .ok(0)
}
```

`connect`, `exec`, `query`, `close`. Handles are `db`, so method syntax applies.

## MySQL — `std.sqlz.mysql`

```zz
import std.sqlz.mysql as my

func main() -> Result<int, str> {
    db := my.connect("host=127.0.0.1 port=3306 user=app password=secret dbname=appdb")
    my.exec(db, "CREATE TABLE IF NOT EXISTS t (id INTEGER)")
    rows := my.query(db, "SELECT * FROM t")
    println(rows)
    my.close(db)
    return .ok(0)
}
```

Binary prepared protocol under the hood. Same `connect/exec/query/close` shape.

## Practices

- `?` propagates; top-level `match` prints human errors.
- Keep schema in `exec` migrations, reads in `query`.
- Wrap multi-statement writes in `transaction`.
- Never interpolate user input into SQL — use placeholders per driver; log the
  statement and row count at `debug`.
