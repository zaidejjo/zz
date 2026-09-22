//! Smart SQL error detector for `std.sqlz` (SQLite, Postgres, MySQL).
//!
//! When a statement fails, [`dialect_hints`] scans the SQL text and the
//! backend's error message for cross-dialect mistakes (e.g. MySQL's
//! `AUTO_INCREMENT` sent to SQLite) and returns short, actionable hints.
//! Each hint covers all three dialects so the user can pick the one that
//! matches their database.

/// Database backend a `sqlz` handle is connected to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Backend {
    Sqlite,
    Postgres,
    Mysql,
}

/// Inspect a failed statement and return hint lines (without any `hint:`
/// prefix — callers add that when rendering).
///
/// `sql` is the rendered statement, `db_error` the backend's error text.
/// Returns at most 3 hints, ordered by confidence.
pub(crate) fn dialect_hints(backend: Backend, sql: &str, db_error: &str) -> Vec<String> {
    let upper = sql.to_uppercase();
    let err_lower = db_error.to_lowercase();
    let mut hints = Vec::new();

    // --- auto-increment spelling -----------------------------------------
    if upper.contains("AUTO_INCREMENT") {
        match backend {
            Backend::Sqlite => hints.push(
                "`AUTO_INCREMENT` is MySQL syntax; SQLite needs \
                `INTEGER PRIMARY KEY AUTOINCREMENT`, Postgres needs \
                `GENERATED ALWAYS AS IDENTITY` (or `SERIAL`)"
                    .to_string(),
            ),
            Backend::Postgres => hints.push(
                "`AUTO_INCREMENT` is MySQL syntax; Postgres needs \
                `GENERATED ALWAYS AS IDENTITY` (or `SERIAL`), SQLite needs \
                `INTEGER PRIMARY KEY AUTOINCREMENT`"
                    .to_string(),
            ),
            Backend::Mysql => {}
        }
    }
    if upper.contains("AUTOINCREMENT") && !upper.contains("AUTO_INCREMENT") {
        match backend {
            Backend::Mysql => hints.push(
                "`AUTOINCREMENT` is SQLite syntax; MySQL needs \
                `INT AUTO_INCREMENT PRIMARY KEY`, Postgres needs \
                `GENERATED ALWAYS AS IDENTITY` (or `SERIAL`)"
                    .to_string(),
            ),
            Backend::Postgres => hints.push(
                "`AUTOINCREMENT` is SQLite syntax; Postgres needs \
                `GENERATED ALWAYS AS IDENTITY` (or `SERIAL`), MySQL needs \
                `INT AUTO_INCREMENT PRIMARY KEY`"
                    .to_string(),
            ),
            Backend::Sqlite => {
                if err_lower.contains("autoincrement") {
                    hints.push(
                        "`AUTOINCREMENT` is only allowed on `INTEGER PRIMARY KEY` \
                        in SQLite; declare the column exactly as \
                        `id INTEGER PRIMARY KEY AUTOINCREMENT`"
                            .to_string(),
                    );
                }
            }
        }
    }
    if upper.contains("SERIAL") {
        match backend {
            Backend::Sqlite => hints.push(
                "`SERIAL` is Postgres syntax; SQLite needs \
                `INTEGER PRIMARY KEY AUTOINCREMENT`, MySQL needs \
                `INT AUTO_INCREMENT PRIMARY KEY`"
                    .to_string(),
            ),
            Backend::Mysql => hints.push(
                "`SERIAL` is Postgres syntax; MySQL needs \
                `INT AUTO_INCREMENT PRIMARY KEY`, SQLite needs \
                `INTEGER PRIMARY KEY AUTOINCREMENT`"
                    .to_string(),
            ),
            Backend::Postgres => {}
        }
    }

    // --- identifier quoting ----------------------------------------------
    if backend == Backend::Postgres && sql.contains('`') {
        hints.push(
            "backtick quotes are MySQL/SQLite syntax; Postgres needs \
            double quotes for identifiers: `\"name\"` instead of `` `name` ``"
                .to_string(),
        );
    }

    // --- `==` comparison ---------------------------------------------------
    if matches!(backend, Backend::Postgres | Backend::Mysql)
        && sql.contains("==")
        && (err_lower.contains("syntax error") || err_lower.contains("syntax"))
    {
        hints.push(
            "`==` is SQLite syntax; Postgres and MySQL need a single \
            `=` for comparison"
                .to_string(),
        );
    }

    // --- missing table -----------------------------------------------------
    if hints.is_empty() {
        if let Some(table) = missing_table(&err_lower) {
            hints.push(format!(
                "table `{table}` doesn't exist yet — run your `CREATE TABLE` \
                first (e.g. call the init function before querying)"
            ));
        }
    }

    hints.truncate(3);
    hints
}

/// Extract a missing-table name from a backend error message, if present.
/// - SQLite: `no such table: tasks`
/// - Postgres: `relation "tasks" does not exist`
/// - MySQL: `Table 'mydb.tasks' doesn't exist`
fn missing_table(err_lower: &str) -> Option<String> {
    if let Some(rest) = err_lower.split_once("no such table:") {
        return Some(take_ident(rest.1));
    }
    if let Some(rest) = err_lower.split_once("relation \"") {
        let name: String = rest.1.chars().take_while(|&c| c != '"').collect();
        if !name.is_empty() {
            return Some(name);
        }
    }
    if let Some(rest) = err_lower.split_once("table '") {
        let full: String = rest.1.chars().take_while(|&c| c != '\'').collect();
        if !full.is_empty() {
            // `mydb.tasks` → `tasks`.
            return Some(full.rsplit('.').next().unwrap_or(&full).to_string());
        }
    }
    None
}

/// Take a leading identifier-ish token (`tasks`, `mydb.tasks`, `"tasks"`).
fn take_ident(s: &str) -> String {
    let s = s.trim_start();
    let s = s.strip_prefix('"').unwrap_or(s);
    s.chars()
        .take_while(|c| c.is_alphanumeric() || *c == '_' || *c == '.' || *c == '"')
        .collect::<String>()
        .trim_matches('"')
        .rsplit('.')
        .next()
        .unwrap_or("")
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sqlite_auto_increment_hint() {
        let h = dialect_hints(
            Backend::Sqlite,
            "CREATE TABLE t (id INT PRIMARY KEY AUTO_INCREMENT, name TEXT)",
            "near \"AUTO_INCREMENT\": syntax error",
        );
        assert_eq!(h.len(), 1);
        assert!(h[0].contains("MySQL syntax"));
        assert!(h[0].contains("AUTOINCREMENT"));
        assert!(h[0].contains("IDENTITY"));
    }

    #[test]
    fn mysql_autoincrement_hint() {
        let h = dialect_hints(
            Backend::Mysql,
            "CREATE TABLE t (id INTEGER PRIMARY KEY AUTOINCREMENT)",
            "syntax error near AUTOINCREMENT",
        );
        assert_eq!(h.len(), 1);
        assert!(h[0].contains("SQLite syntax"));
        assert!(h[0].contains("AUTO_INCREMENT"));
    }

    #[test]
    fn postgres_autoincrement_hint() {
        let h = dialect_hints(
            Backend::Postgres,
            "CREATE TABLE t (id INTEGER PRIMARY KEY AUTOINCREMENT)",
            "syntax error at or near \"AUTOINCREMENT\"",
        );
        assert_eq!(h.len(), 1);
        assert!(h[0].contains("SQLite syntax"));
        assert!(h[0].contains("IDENTITY"));
    }

    #[test]
    fn serial_on_sqlite_and_mysql() {
        for backend in [Backend::Sqlite, Backend::Mysql] {
            let h = dialect_hints(
                backend,
                "CREATE TABLE t (id SERIAL PRIMARY KEY)",
                "syntax error",
            );
            assert_eq!(h.len(), 1, "{backend:?}");
            assert!(h[0].contains("Postgres syntax"), "{backend:?}");
        }
    }

    #[test]
    fn no_hint_when_dialect_correct() {
        assert!(dialect_hints(
            Backend::Sqlite,
            "CREATE TABLE t (id INTEGER PRIMARY KEY AUTOINCREMENT)",
            "table t already exists",
        )
        .is_empty());
        assert!(dialect_hints(
            Backend::Mysql,
            "CREATE TABLE t (id INT AUTO_INCREMENT PRIMARY KEY)",
            "table t already exists",
        )
        .is_empty());
    }

    #[test]
    fn postgres_backtick_hint() {
        let h = dialect_hints(
            Backend::Postgres,
            "SELECT `name` FROM t",
            "syntax error at or near \"`\"",
        );
        assert_eq!(h.len(), 1);
        assert!(h[0].contains("double quotes"));
    }

    #[test]
    fn double_equals_hint_on_postgres() {
        let h = dialect_hints(
            Backend::Postgres,
            "DELETE FROM t WHERE id == 1",
            "syntax error at or near \"=\"",
        );
        assert_eq!(h.len(), 1);
        assert!(h[0].contains("single"));
    }

    #[test]
    fn double_equals_ok_on_sqlite() {
        assert!(dialect_hints(
            Backend::Sqlite,
            "DELETE FROM t WHERE id == 1",
            "no such table: t",
        )
        .iter()
        .all(|h| !h.contains("single")));
    }

    #[test]
    fn missing_table_all_backends() {
        let h = dialect_hints(
            Backend::Sqlite,
            "SELECT * FROM tasks",
            "no such table: tasks",
        );
        assert!(h.iter().any(|x| x.contains("`tasks`")), "{h:?}");
        let h = dialect_hints(
            Backend::Postgres,
            "SELECT * FROM tasks",
            "relation \"tasks\" does not exist",
        );
        assert!(h.iter().any(|x| x.contains("`tasks`")), "{h:?}");
        let h = dialect_hints(
            Backend::Mysql,
            "SELECT * FROM tasks",
            "Table 'mydb.tasks' doesn't exist",
        );
        assert!(h.iter().any(|x| x.contains("`tasks`")), "{h:?}");
    }

    #[test]
    fn sqlite_autoincrement_placement_hint() {
        let h = dialect_hints(
            Backend::Sqlite,
            "CREATE TABLE t (id INT AUTOINCREMENT)",
            "AUTOINCREMENT is only allowed on INTEGER PRIMARY KEY",
        );
        assert!(h.iter().any(|x| x.contains("INTEGER PRIMARY KEY")), "{h:?}");
    }
}
