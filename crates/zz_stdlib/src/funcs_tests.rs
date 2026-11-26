//! Tests for the stdlib signature table, kept out of the generated shim.

use zz_checker::Type;

use crate::funcs::stdlib_funcs;
use crate::stdlib_consts;

#[test]
fn has_all_modules() {
    let funcs = stdlib_funcs();
    assert!(funcs.contains_key("println"));
    assert!(funcs.contains_key("print"));
    assert!(funcs.contains_key("input"));
    assert!(funcs.contains_key("std.str.length"));
    assert!(funcs.contains_key("std.vec.push"));
    assert!(funcs.contains_key("std.json.parse"));
    assert!(funcs.contains_key("std.json.stringify"));
    assert!(funcs.contains_key("std.http.server"));
    assert!(funcs.contains_key("std.http.handle"));
    assert!(funcs.contains_key("std.http.listen"));
    assert!(funcs.contains_key("std.http.log"));
    assert!(funcs.contains_key("std.http.pipe"));
    assert!(funcs.contains_key("std.http.serve_dir"));
    assert!(funcs.contains_key("std.http.test"));
    assert!(funcs.contains_key("std.http.param"));
    assert!(funcs.contains_key("std.http.query"));
    assert!(funcs.contains_key("std.http.header"));
    assert!(funcs.contains_key("std.http.body_json"));
    assert!(funcs.contains_key("std.http.body_form"));
    assert!(funcs.contains_key("std.http.body_bytes"));
    assert!(funcs.contains_key("std.http.respond"));
    assert!(funcs.contains_key("std.http.test_req"));
    assert!(funcs.contains_key("std.http.serve_dir_at"));
    assert!(funcs.contains_key("std.http.pipe_post"));
    assert!(funcs.contains_key("std.http.rate_limit"));
    assert!(funcs.contains_key("std.http.with_headers"));
    assert!(funcs.contains_key("std.http.listen_cfg"));
    assert!(funcs.contains_key("std.http.cors"));
    assert!(funcs.contains_key("std.http.secure_headers"));
    assert!(funcs.contains_key("std.encoding.base64_decode_bytes"));
    assert!(funcs.contains_key("std.fs.read_bytes"));
    assert!(funcs.contains_key("std.fs.write"));
    assert!(funcs.contains_key("std.fs.append"));
    assert!(funcs.contains_key("std.fs.copy"));
    assert!(funcs.contains_key("std.fs.move"));
    assert!(!funcs.contains_key("std.fs.rename"));
    assert!(funcs.contains_key("std.fs.is_file"));
    assert!(funcs.contains_key("std.fs.is_dir"));
    assert!(funcs.contains_key("std.fs.mkdir"));
    assert!(funcs.contains_key("std.fs.mkdir_all"));
    assert!(funcs.contains_key("std.fs.read_dir"));
    assert!(funcs.contains_key("std.fs.remove_dir_all"));
    assert!(funcs.contains_key("std.fs.walk_dir"));
    assert!(funcs.contains_key("std.fs.stat"));
    assert!(funcs.contains_key("std.fs.open"));
    assert!(funcs.contains_key("std.fs.read_chunk"));
    assert!(funcs.contains_key("std.fs.write_chunk"));
    assert!(funcs.contains_key("std.fs.seek"));
    assert!(funcs.contains_key("std.fs.flush"));
    assert!(funcs.contains_key("std.fs.close"));
    assert!(funcs.contains_key("File.open"));
    assert!(funcs.contains_key("file.read_chunk"));
    assert!(funcs.contains_key("file.write_chunk"));
    assert!(funcs.contains_key("file.seek"));
    assert!(funcs.contains_key("file.flush"));
    assert!(funcs.contains_key("file.close"));
    assert!(funcs.contains_key("std.env.var"));
    assert!(funcs.contains_key("std.env.args"));
    assert!(funcs.contains_key("std.math.abs"));
    assert!(funcs.contains_key("std.math.random"));
    assert!(funcs.contains_key("std.math.round"));
    assert!(funcs.contains_key("std.math.trunc"));
    assert!(funcs.contains_key("std.math.clamp"));
    assert!(funcs.contains_key("std.math.isqrt"));
    assert!(funcs.contains_key("std.math.factorial"));
    assert!(funcs.contains_key("std.math.gcd"));
    assert!(funcs.contains_key("std.math.lcm"));
    assert!(funcs.contains_key("std.math.sin"));
    assert!(funcs.contains_key("std.math.cos"));
    assert!(funcs.contains_key("std.math.tan"));
    assert!(funcs.contains_key("std.math.asin"));
    assert!(funcs.contains_key("std.math.acos"));
    assert!(funcs.contains_key("std.math.atan"));
    assert!(funcs.contains_key("std.math.sin_deg"));
    assert!(funcs.contains_key("std.math.cos_deg"));
    assert!(funcs.contains_key("std.math.tan_deg"));
    assert!(funcs.contains_key("std.math.to_radians"));
    assert!(funcs.contains_key("std.math.to_degrees"));
    assert!(funcs.contains_key("std.math.log"));
    assert!(funcs.contains_key("std.math.log10"));
    assert!(funcs.contains_key("std.math.exp"));
    assert!(funcs.contains_key("std.math.dot_product"));
    assert!(funcs.contains_key("std.math.magnitude"));
    assert!(funcs.contains_key("std.math.matrix_mul"));
    assert!(funcs.contains_key("std.math.mean"));
    assert!(funcs.contains_key("std.math.median"));
    assert!(funcs.contains_key("std.math.rand_range"));
    assert!(funcs.contains_key("std.time.now_ms"));
    assert!(funcs.contains_key("typeof"));
    assert!(funcs.contains_key("str"));
    assert!(funcs.contains_key("int"));
    assert!(funcs.contains_key("float"));
    assert!(funcs.contains_key("clone"));
    assert!(funcs.contains_key("std.task.spawn"));
    assert!(funcs.contains_key("std.task.join"));
    assert!(funcs.contains_key("std.task.try_join"));
    assert!(funcs.contains_key("std.sqlz.open"));
    assert!(funcs.contains_key("std.sqlz.exec"));
    assert!(funcs.contains_key("std.sqlz.query"));
    assert!(funcs.contains_key("std.sqlz.close"));
    assert!(funcs.contains_key("std.sqlz.postgres.connect"));
    assert!(funcs.contains_key("std.sqlz.postgres.exec"));
    assert!(funcs.contains_key("std.sqlz.postgres.query"));
    assert!(funcs.contains_key("std.sqlz.postgres.close"));
    assert!(funcs.contains_key("std.sqlz.mysql.connect"));
    assert!(funcs.contains_key("std.sqlz.mysql.exec"));
    assert!(funcs.contains_key("std.sqlz.mysql.query"));
    assert!(funcs.contains_key("std.sqlz.mysql.close"));
    assert!(funcs.contains_key("std.sqlz.transaction"));
    // Math constants are static values, not zero-arg functions.
    let consts = stdlib_consts();
    assert!(consts.contains_key("std.math.PI"));
    assert!(consts.contains_key("std.math.E"));
    assert!(consts.contains_key("std.math.TAU"));
    // ... and carry checker signatures so `std.math.PI` resolves
    // import-free (like every other `std.*` name).
    assert!(funcs.contains_key("std.math.PI"));
    assert!(funcs.contains_key("std.math.E"));
    assert!(funcs.contains_key("std.math.INF"));
    assert!(funcs.contains_key("std.math.NAN"));
    // vec.enumerate: method spelling of bare `enumerate`.
    assert!(funcs.contains_key("std.vec.enumerate"));
    // path helpers (pure-ZZ).
    assert!(funcs.contains_key("std.path.join"));
    assert!(funcs.contains_key("std.path.join_all"));
    assert!(funcs.contains_key("std.path.normalize"));
    assert!(funcs.contains_key("std.path.basename"));
    assert!(funcs.contains_key("std.path.dirname"));
    assert!(funcs.contains_key("std.path.is_absolute"));
    assert!(funcs.contains_key("std.path.extension"));
    assert!(funcs.contains_key("std.colors.red"));
    assert!(funcs.contains_key("std.colors.rgb"));
    assert!(funcs.contains_key("std.colors.hex"));
    assert!(funcs.contains_key("std.colors.bold"));
    assert!(funcs.contains_key("std.colors.strip"));
    assert!(funcs.contains_key("std.colors.reverse"));
    assert!(funcs.contains_key("std.colors.strikethrough"));
    assert!(funcs.contains_key("std.colors.blink"));
    assert!(funcs.contains_key("std.colors.bg_bright_red"));
    assert!(funcs.contains_key("std.colors.bg_bright_white"));
    assert!(funcs.contains_key("std.colors.color256"));
    assert!(funcs.contains_key("std.colors.bg_256"));
    assert!(funcs.contains_key("dbg"));
    // net method aliases + binary transfers + shutdown.
    for name in [
        "std.net.tcp_read_bytes",
        "std.net.tcp_write_bytes",
        "std.net.tcp_shutdown",
        "std.net.shutdown",
        "std.net.accept",
        "std.net.read",
        "std.net.read_line",
        "std.net.read_bytes",
        "std.net.write",
        "std.net.write_bytes",
        "std.net.peer_addr",
        "std.net.local_addr",
        "std.net.set_read_timeout",
        "std.net.set_write_timeout",
        "std.net.tcp_read_exact",
        "std.net.tls_connect",
        "std.net.tls_connect_with_ca",
        "std.net.tls_connect_insecure",
        "std.net.tls_listen",
        "std.net.tls_accept",
    ] {
        assert!(funcs.contains_key(name), "missing {name}");
    }
    // http helpers (pure-ZZ, compiled from zz/http/mod.zz).
    for name in [
        "std.http.route",
        "std.http.ok",
        "std.http.created",
        "std.http.not_found",
        "std.http.redirect",
    ] {
        assert!(funcs.contains_key(name), "missing {name}");
    }
    // Unified client + JSON client.
    for name in ["std.http.fetch", "std.http.post_json"] {
        assert!(funcs.contains_key(name), "missing {name}");
    }
    // 9 canonical `std.str.*` twins of the `str.*` method entries
    // (trim, to_upper, to_lower, replace, starts_with, ends_with,
    // join, trim_start, trim_end).
    for name in [
        "std.str.trim",
        "std.str.to_upper",
        "std.str.to_lower",
        "std.str.replace",
        "std.str.starts_with",
        "std.str.ends_with",
        "std.str.join",
        "std.str.trim_start",
        "std.str.trim_end",
    ] {
        assert!(funcs.contains_key(name), "missing {name}");
    }
    // std.term — raw mode, single-key reads, size (both spellings).
    for name in [
        "std.term.enable_raw",
        "std.term.disable_raw",
        "std.term.read_key",
        "std.term.poll",
        "std.term.get_size",
        "std.term.is_tty",
        "std.term.flush",
    ] {
        assert!(funcs.contains_key(name), "missing {name}");
    }
    // std.pty — pseudo-terminals.
    for name in [
        "std.pty.open",
        "std.pty.read",
        "std.pty.write",
        "std.pty.ptsname",
        "std.pty.grantpt",
        "std.pty.unlockpt",
        "std.pty.set_winsz",
        "std.pty.isatty",
    ] {
        assert!(funcs.contains_key(name), "missing {name}");
    }
    // std.signal — yield-point signal delivery.
    assert!(
        funcs.contains_key("std.signal.install"),
        "missing std.signal.install"
    );
    // std.process.spawn_pty.
    assert!(
        funcs.contains_key("std.process.spawn_pty"),
        "missing std.process.spawn_pty"
    );
    // New pure-ZZ modules (map, set, dec, bytes, csv, time dates).
    for name in [
        "std.map.has",
        "std.map.get_or",
        "std.map.merge",
        "std.map.keys",
        "std.set.has",
        "std.set.union",
        "std.set.intersect",
        "std.dec.add",
        "std.dec.cmp",
        "std.dec.format",
        "std.str.builder",
        "std.str.finish",
        "std.bytes.builder",
        "std.bytes.finish",
        "std.cell.new",
        "std.cell.get",
        "std.cell.set",
        "std.cell.drop",
        "std.net.unix_connect",
        "std.net.unix_listen",
        "std.net.unix_accept",
        "std.net.unix_listener_close",
        "std.net.unix_write",
        "std.net.unix_write_bytes",
        "std.net.unix_read",
        "std.net.unix_read_bytes",
        "std.net.unix_read_exact",
        "std.net.unix_readline",
        "std.net.unix_set_read_timeout",
        "std.net.unix_set_write_timeout",
        "std.net.unix_close",
        "std.net.unix_shutdown",
        "std.unix_listener.accept",
        "std.unix_listener.close",
        "std.net.tls_readline",
        "std.net.tls_shutdown",
        "std.bytes.push_byte",
        "std.csv.parse",
        "std.csv.stringify",
        "std.csv.to_json",
        "std.property.seed",
        "std.property.int_in",
        "std.property.check",
        "std.property.for_all_int",
        "std.time.parse_rfc3339",
        "std.time.format_rfc3339",
        "std.time.add_days",
        "std.str.starts_with_at",
        "std.str.ends_with_at",
        "std.str.trim_span",
        "std.str.find_in",
        "std.str.rfind_in",
        "std.str.count_in",
        "std.str.bytes",
        "std.str.byte_len",
        "std.bytes.to_str",
        "std.bytes.to_ints",
        "std.str.classify",
        "std.fs.scan_counts",
        "std.fs.is_generated",
        "std.str.find",
        "std.str.rfind",
        "ord",
        "std.str.ord",
        "chr",
        "std.str.chr",
        "assert_contains",
        "std.test.assert_contains",
        "assert_starts_with",
        "std.test.assert_starts_with",
        "assert_ends_with",
        "std.test.assert_ends_with",
        "assert_len",
        "std.test.assert_len",
        "assert_gt",
        "std.test.assert_gt",
        "assert_lt",
        "std.test.assert_lt",
        "assert_is_some",
        "std.test.assert_is_some",
        "assert_is_none",
        "std.test.assert_is_none",
        "assert_is_ok",
        "std.test.assert_is_ok",
        "assert_is_err",
        "std.test.assert_is_err",
        "temp_dir",
        "std.test.temp_dir",
        "free_port",
        "std.test.free_port",
        "assert_snapshot",
        "std.test.assert_snapshot",
        "black_box",
        "std.test.black_box",
    ] {
        assert!(funcs.contains_key(name), "missing {name}");
    }
    assert_eq!(funcs.len(), 699);
}

#[test]
fn no_bare_twin_seeds() {
    // B invariant: seeds hold canonical `std.*` names only. A bare
    // `module.func` key may exist only when no `std.module.func` seed
    // exists - never as a twin. Namespaces without an importable
    // module behind them (`span.end`, ...) keep their bare seeds:
    // the test derives the rule from `canonical_std_name` itself
    // instead of a hardcoded list. Bare callers resolve via that
    // same fallback.
    let funcs = stdlib_funcs();
    let mut twins = Vec::new();
    for k in funcs.keys() {
        if k.contains('.')
            && !k.starts_with("std.")
            && funcs.contains_key(&format!("std.{k}"))
            && zz_checker::canonical_std_name(k).is_some()
        {
            twins.push(k.clone());
        }
    }
    assert!(
        twins.is_empty(),
        "bare twin seeds (use std.* only): {twins:?}"
    );
}

#[test]
fn vec_funcs_are_generic() {
    let funcs = stdlib_funcs();
    assert_eq!(funcs["std.vec.push"].generics, vec!["T"]);
    assert_eq!(funcs["std.vec.push"].params[1].1, Type::Named("T".into()));
}
