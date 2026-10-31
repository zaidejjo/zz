# `std.sqlz` — SQLite database access

Open SQLite databases, run SQL, query rows, and use transactions. Rows are
typed arrays; query results are collected into `[T]`.

```zz
import std.sqlz

match sqlz.open("app.db") {
    .ok(db) => {
        sqlz.exec(db, "CREATE TABLE IF NOT EXISTS users(name TEXT, age INT)")?
        sqlz.exec(db, "INSERT INTO users VALUES ('Ada', 36)")?
        match sqlz.query[ {name: str, age: int} ](db, "SELECT name, age FROM users") {
            .ok(rows) => for r in rows { println(r["name"]) },
            .err(e)   => println(e),
        }
        sqlz.close(db)
    },
    .err(e) => println(e),
}

// Transaction
match sqlz.transaction(db, |d| {
    sqlz.exec(d, "INSERT INTO users VALUES ('Grace', 43)")?
    return .ok(())
}) {
    .ok(_) => println("committed"),
    .err(e) => println(e),
}
```

## Types

| Type | Notes |
|---|---|
| `db` | Opaque database handle from `sqlz.open`/`sqlz.connect`. Pass to `exec`/`query`/`transaction`; close with `sqlz.close`. |

## Variables

None.

## Functions

| Function | Signature | Notes |
|---|---|---|
| `open(path)` | `open(path: str) -> db` | Open/create a SQLite database file. |
| `connect(conninfo)` | `connect(conninfo: str) -> db` | Connect via connection string. |
| `exec(db, sql)` | `exec(db: db, sql: str) -> int` | Execute SQL; returns rows affected. |
| `query[T](db, sql)` | `query[T](db: db, sql: str) -> [T]` | Query rows typed as `T` (struct or scalar). |
| `transaction(db, f)` | `transaction(db: db, f: func(db) -> T) -> Result<T, str>` | Run `f` inside a transaction; commit/rollback. |
| `close(db)` | `close(db: db) -> unit` | Close the database. |
