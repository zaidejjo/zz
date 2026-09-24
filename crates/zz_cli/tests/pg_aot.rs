//! Black-box Postgres tests for the AOT PG backend (Step 3).
//!
//! Spins an ephemeral Postgres cluster (`initdb` + `pg_ctl`, no docker)
//! and, for each case, runs the same `.zz` source on the VM (`zz run`)
//! and on AOT (one `zz build` binary), asserting byte-identical stdout.
//! A final AOT-only case covers leniency paths where the VM raises
//! (bad SQL, rolled-back transactions, use-after-close, refused
//! connect): those assert exact AOT markers instead of VM parity.
//!
//! Skipped when `initdb`/`pg_ctl` are missing or `ZZ_SKIP_NATIVE=1`.

use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::Duration;

fn require_pg() -> bool {
    if std::env::var("ZZ_SKIP_NATIVE").is_ok() {
        eprintln!("skip: native backend unsupported (ZZ_SKIP_NATIVE=1)");
        return false;
    }
    for bin in ["initdb", "pg_ctl", "psql"] {
        if which(bin).is_none() {
            eprintln!("skip: {bin} not on PATH (no local postgres)");
            return false;
        }
    }
    true
}

fn which(bin: &str) -> Option<PathBuf> {
    std::env::var_os("PATH").and_then(|paths| {
        std::env::split_paths(&paths)
            .map(|d| d.join(bin))
            .find(|p| p.is_file())
    })
}

/// Ephemeral cluster. Each test builds its own (a few seconds): the
/// `Drop` stops postgres and removes the datadir, so no postmaster or
/// `/tmp` state outlives the test that created it.
struct Cluster {
    dir: PathBuf,
    port: u16,
}

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .expect("bind probe")
        .local_addr()
        .expect("probe addr")
        .port()
}

fn cluster() -> Cluster {
    // Nanos suffix: bare PIDs recycle and would resurrect stale data
    // dirs (and their old clusters) from earlier test processes.
    let dir = std::env::temp_dir().join(format!(
        "zz_pg_aot_{}_{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).expect("tempdir");
    let port = free_port();
    let run = |prog: &str, args: &[&str]| {
        let out = Command::new(prog)
            .args(args)
            .output()
            .unwrap_or_else(|_| panic!("spawn {prog}"));
        assert!(
            out.status.success(),
            "{prog} {args:?} failed:\n{}",
            String::from_utf8_lossy(&out.stderr)
        );
    };
    run(
        "initdb",
        &[
            "-D",
            &dir.join("data").to_string_lossy(),
            "-U",
            "postgres",
            "--auth=trust",
        ],
    );
    run(
        "pg_ctl",
        &[
            "-D",
            &dir.join("data").to_string_lossy(),
            "-o",
            &format!("-p {port} -k {}", dir.display()),
            "-l",
            &dir.join("pg.log").to_string_lossy(),
            "start",
        ],
    );
    // Wait for readiness (poll a trivial connection).
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    loop {
        let probe = Command::new("psql")
            .args([
                "-h",
                "127.0.0.1",
                "-p",
                &port.to_string(),
                "-U",
                "postgres",
                "-c",
                "SELECT 1",
            ])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
        if probe.is_ok_and(|s| s.success()) {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "postgres never became ready"
        );
        std::thread::sleep(Duration::from_millis(200));
    }
    run(
        "psql",
        &[
            "-h",
            "127.0.0.1",
            "-p",
            &port.to_string(),
            "-U",
            "postgres",
            "-c",
            "CREATE DATABASE pgtest",
        ],
    );
    Cluster { dir, port }
}

impl Drop for Cluster {
    fn drop(&mut self) {
        let data = self.dir.join("data");
        let _ = Command::new("pg_ctl")
            .args(["-D", &data.to_string_lossy(), "stop", "-m", "fast"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
        // Best-effort cleanup (a stopped cluster is harmless in tmp).
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn zz_bin() -> PathBuf {
    let exe = std::env::current_exe().unwrap();
    let deps = exe.parent().unwrap();
    let debug = deps.parent().unwrap();
    debug.join("zz")
}

fn pg_url(port: u16) -> String {
    format!("postgres://postgres@127.0.0.1:{port}/pgtest?connect_timeout=2")
}

/// Write a case source, run it on the VM, build + run it AOT, and assert
/// identical stdout (both exit 0).
fn assert_vm_aot_parity(cl: &Cluster, name: &str, src: &str) {
    let dir = cl.dir.clone();
    let zz_path = dir.join(format!("{name}.zz"));
    std::fs::write(&zz_path, src).expect("write case");
    let url = pg_url(cl.port);
    let run_vm = Command::new(zz_bin())
        .arg("run")
        .arg(&zz_path)
        .env("ZZ_PG_URL", &url)
        .output()
        .expect("spawn zz run");
    assert!(
        run_vm.status.success(),
        "VM {name} failed:\n{}",
        String::from_utf8_lossy(&run_vm.stderr)
    );
    let build = Command::new(zz_bin())
        .arg("build")
        .arg(&zz_path)
        .output()
        .expect("spawn zz build");
    assert!(
        build.status.success(),
        "AOT build {name} failed:\n{}",
        String::from_utf8_lossy(&build.stderr)
    );
    let bin = dir.join("bin").join(name);
    let run_aot = Command::new(&bin)
        .env("ZZ_PG_URL", &url)
        .output()
        .expect("spawn AOT case");
    assert!(
        run_aot.status.success(),
        "AOT {name} failed:\n{}",
        String::from_utf8_lossy(&run_aot.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&run_aot.stdout),
        String::from_utf8_lossy(&run_vm.stdout),
        "VM/AOT stdout differ for {name}"
    );
}

/// Run an AOT-only case (VM would raise): assert exit 0 + exact stdout.
fn assert_aot_only(cl: &Cluster, name: &str, src: &str, expected: &str) {
    let dir = cl.dir.clone();
    let zz_path = dir.join(format!("{name}.zz"));
    std::fs::write(&zz_path, src).expect("write case");
    let build = Command::new(zz_bin())
        .arg("build")
        .arg(&zz_path)
        .output()
        .expect("spawn zz build");
    assert!(
        build.status.success(),
        "AOT build {name} failed:\n{}",
        String::from_utf8_lossy(&build.stderr)
    );
    let bin = dir.join("bin").join(name);
    let run_aot = Command::new(&bin)
        .env("ZZ_PG_URL", pg_url(cl.port))
        .output()
        .expect("spawn AOT case");
    assert!(
        run_aot.status.success(),
        "AOT {name} failed:\n{}",
        String::from_utf8_lossy(&run_aot.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&run_aot.stdout),
        expected,
        "AOT {name} output mismatch"
    );
}

// The URL comes from the environment so one source serves any cluster.
const URL_EXPR: &str = r#"env.var("ZZ_PG_URL") ?? "postgres://postgres@127.0.0.1:1/pgtest""#;

#[test]
fn pg_crud_parity() {
    if !require_pg() {
        return;
    }
    let cl = cluster();
    let src = format!(
        r#"import std.sqlz
import std.sqlz.postgres as pg
import std.env
struct Item {{ id: int, name: str, price: float, active: bool }}
struct N {{ name: str }}
struct C {{ c: int }}
func main() {{
    url := {url}
    db := sqlz.open(url)
    id := 1
    name := "alice"
    bob := "bob"
    sqlz.exec(db, "DELETE FROM crud_items")
    empty: [C] = sqlz.query(db, "SELECT COUNT(*) AS c FROM crud_items")
    println("empty:{{empty[0].c}}")
    println("ins:{{sqlz.exec(db, "INSERT INTO crud_items VALUES ({{id}}, {{name}}, 9.5, true)")}}")
    rows: [Item] = sqlz.query(db, "SELECT id, name, price, active FROM crud_items WHERE id = {{id}}")
    println("n:{{len(rows)}}")
    println("id:{{rows[0].id}}")
    println("name:{{rows[0].name}}")
    println("price:{{rows[0].price}}")
    println("active:{{rows[0].active}}")
    mdb := pg.connect("host=127.0.0.1 port={port} dbname=pgtest user=postgres connect_timeout=2")
    println("pins:{{pg.exec(mdb, "INSERT INTO crud_items VALUES (2, {{bob}}, 1.25, false)")}}")
    r2: [N] = pg.query(mdb, "SELECT name FROM crud_items WHERE id = 2")
    println("q2:{{r2[0].name}}")
    r3: [C] = mdb.query("SELECT COUNT(*) AS c FROM crud_items")
    println("count:{{r3[0].c}}")
    sqlz.close(db)
    pg.close(mdb)
    println("pg_crud_ok")
}}
"#,
        url = URL_EXPR,
        port = cl.port
    );
    // Schema first (DROP + CREATE: deterministic empty start no matter
    // what earlier runs — or PID-recycled tmpdirs — left behind).
    let setup = Command::new("psql")
        .args([
            "-h", "127.0.0.1", "-p", &cl.port.to_string(), "-U", "postgres",
            "-d", "pgtest", "-c",
            "DROP TABLE IF EXISTS crud_items",
            "-c",
            "CREATE TABLE crud_items (id INTEGER PRIMARY KEY, name TEXT, price DOUBLE PRECISION, active BOOLEAN)",
        ])
        .output()
        .expect("psql setup");
    assert!(setup.status.success(), "setup failed");
    assert_vm_aot_parity(&cl, "pgcrud", &src);
}

#[test]
fn pg_edge_parity() {
    if !require_pg() {
        return;
    }
    let cl = cluster();
    let src = format!(
        r#"import std.sqlz
import std.env
struct N {{ name: str }}
func main() {{
    url := {url}
    db := sqlz.open(url)
    b := "b"
    sqlz.exec(db, "DELETE FROM edge_items")
    sqlz.exec(db, "INSERT INTO edge_items VALUES (1, NULL, NULL, NULL)")
    rows: [N] = sqlz.query(db, "SELECT name FROM edge_items WHERE id = 1")
    println("nullname:{{rows[0].name ?? "was-null"}}")
    txok := sqlz.transaction(db, |tx| {{ tx.exec("INSERT INTO edge_items VALUES (2, {{b}}, 1.0, true)"); .ok("done") }})
    println("txok:{{txok}}")
    c: [N] = sqlz.query(db, "SELECT name FROM edge_items WHERE id = 2")
    println("txrows:{{len(c)}}:{{c[0].name ?? "was-null"}}")
    sqlz.close(db)
    println("pg_edge_ok")
}}
"#,
        url = URL_EXPR
    );
    let setup = Command::new("psql")
        .args([
            "-h", "127.0.0.1", "-p", &cl.port.to_string(), "-U", "postgres",
            "-d", "pgtest", "-c",
            "DROP TABLE IF EXISTS edge_items",
            "-c",
            "CREATE TABLE edge_items (id INTEGER PRIMARY KEY, name TEXT, price DOUBLE PRECISION, active BOOLEAN)",
        ])
        .output()
        .expect("psql setup");
    assert!(setup.status.success(), "setup failed");
    assert_vm_aot_parity(&cl, "pgedge", &src);
}

#[test]
fn pg_aot_leniency() {
    if !require_pg() {
        return;
    }
    let cl = cluster();
    let src = format!(
        r#"import std.sqlz
import std.sqlz.postgres as pg
import std.env
struct N {{ name: str }}
func main() {{
    url := {url}
    db := sqlz.open(url)
    sqlz.exec(db, "DELETE FROM aot_items")
    println("badexec:{{sqlz.exec(db, "INSERT INTO nope VALUES (1)")}}")
    println("badquery:{{len(sqlz.query(db, "SELECT * FROM nope"))}}")
    txbad := sqlz.transaction(db, |tx| {{ tx.exec("INSERT INTO aot_items VALUES (9, 'x', 1.0, true)"); tx.exec("INSERT INTO nope VALUES (1)"); .ok("done") }})
    println("txbad:{{txbad}}")
    c: [N] = sqlz.query(db, "SELECT name FROM aot_items")
    println("txrows:{{len(c)}}")
    sqlz.close(db)
    println("afterclose:{{len(sqlz.query(db, "SELECT 1"))}}")
    refused := pg.connect("host=127.0.0.1 port=1 dbname=x user=u connect_timeout=1")
    println("refused:{{len(pg.query(refused, "SELECT 1"))}}")
    println("pg_aot_only_ok")
}}
"#,
        url = URL_EXPR
    );
    let setup = Command::new("psql")
        .args([
            "-h", "127.0.0.1", "-p", &cl.port.to_string(), "-U", "postgres",
            "-d", "pgtest", "-c",
            "DROP TABLE IF EXISTS aot_items",
            "-c",
            "CREATE TABLE aot_items (id INTEGER PRIMARY KEY, name TEXT, price DOUBLE PRECISION, active BOOLEAN)",
        ])
        .output()
        .expect("psql setup");
    assert!(setup.status.success(), "setup failed");
    // The good insert rolls back with the bad one: txrows 0.
    let expected = "badexec:0\nbadquery:0\ntxbad:.err(transaction failed)\ntxrows:0\nafterclose:0\nrefused:0\npg_aot_only_ok\n";
    assert_aot_only(&cl, "pgaot", &src, expected);
}
