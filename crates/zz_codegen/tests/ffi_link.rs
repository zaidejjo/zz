//! End-to-end proof of the AOT FFI link mechanism.
//!
//! Builds the real static library, compiles a C program that combines the
//! real runtime TU with FFI calls, links it with the real flags, runs it, and
//! checks handle alloc/tag/drop plus a crypto call (which exercises the
//! linkable `zz_str_*` constructors) across the language boundary.

use std::collections::HashSet;
use std::process::Command;

use zz_codegen::ffi::{ffi_prelude, link_args, FFI_VERSION};

/// Build the real static library, compile a C program that combines the
/// real runtime TU (`RUNTIME_C`, exactly as generated programs do) with FFI
/// calls, link with the real flags, run it, and check handle alloc/tag/drop
/// plus a crypto call (which exercises the linkable `zz_str_*` constructors)
/// across the language boundary.
///
/// Gated on Windows: the runtime TU includes `<pthread.h>` and the socket
/// layer is POSIX, so it cannot compile with MSVC there yet. Windows CI still
/// covers the FFI surface through the pure string/flag tests in
/// `zz_codegen_ffi` and the CLI-level VM suites.
#[test]
#[cfg(not(windows))]
fn link_staticlib_from_c() {
    let (mut args, _rt_lib) = match link_args(false) {
        Ok(a) => a,
        Err(e) => panic!("link_args failed: {e}"),
    };
    // Same runtime deps as compile::build (vcpkg names curl's import
    // library `libcurl.lib` on Windows).
    args.push(if cfg!(windows) {
        "-llibcurl".to_string()
    } else {
        "-lcurl".to_string()
    });
    args.push("-lsqlite3".to_string());
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let uniq = COUNTER.fetch_add(1, Ordering::SeqCst);
    let tmp = std::env::temp_dir().join(format!("zz-ffi-test-{}-{uniq}", std::process::id()));
    std::fs::create_dir_all(&tmp).expect("tmpdir");
    let c_path = tmp.join("t.c");
    let bin_path = tmp.join("t");
    let used: HashSet<String> = ["std.crypto.sha256".to_string()].into_iter().collect();
    let prelude = ffi_prelude(&used);
    assert!(
        prelude.contains("zz_crypto_sha256"),
        "prelude must declare used symbols"
    );
    let raw = format!(
        "{headers}\n{runtime}\n{prelude}\n#include <stdlib.h>\nvoid zz_main(void) {{\n    if (zz_rt_version() != {ver}ULL) exit(10);\n    uint64_t live0 = zz_rt_handle_live();\n    uint64_t id = zz_rt_handle_alloc((const uint8_t *)\"regex\", 5);\n    if (id == 0) exit(11);\n    if (!zz_rt_handle_tag_eq(id, (const uint8_t *)\"regex\", 5)) exit(12);\n    if (zz_rt_handle_tag_eq(id, (const uint8_t *)\"uuid\", 4)) exit(13);\n    if (zz_rt_handle_live() != live0 + 1) exit(14);\n    if (!zz_rt_handle_drop(id)) exit(15);\n    if (zz_rt_handle_tag_eq(id, (const uint8_t *)\"regex\", 5)) exit(16);\n    int err = 0;\n    zz_value digest = zz_crypto_sha256(zz_str_new(\"abc\", 3), &err);\n    const char *ptr = NULL; size_t len = 0;\n    zz_str_view(digest, &ptr, &len);\n    if (err != 0 || len != 64 || ptr == NULL) exit(17);\n    if (memcmp(ptr, \"ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad\", 64) != 0) exit(18);\n    printf(\"ffi_link_ok\\n\");\n}}\nint zz_call_main(void) {{ return 0; }}\n",
        headers = zz_codegen_runtime::RUNTIME_H,
        runtime = zz_codegen_runtime::RUNTIME_C,
        ver = FFI_VERSION
    );
    // Same single-TU assembly as generated programs: drop quoted
    // includes (headers are concatenated, not on disk).
    let src = zz_codegen_runtime::strip_quoted_includes(&raw);
    std::fs::write(&c_path, &src).expect("write C");
    let cc = zz_codegen::detect_clang().expect("no C compiler");
    let mut cmd = Command::new(&cc.path);
    if cc.zig {
        cmd.arg("cc");
    }
    cmd.arg("-O1")
        .arg("-DZZ_HAS_CURL=0")
        .arg("-DZZ_HAS_SQLITE3=0")
        .arg("-o")
        .arg(&bin_path)
        .arg(&c_path);
    for a in &args {
        cmd.arg(a);
    }
    let out = cmd.output().expect("run cc");
    assert!(
        out.status.success(),
        "cc link failed:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let run = Command::new(&bin_path).output().expect("run binary");
    assert_eq!(run.status.code(), Some(0), "exit != 0");
    assert_eq!(String::from_utf8_lossy(&run.stdout), "ffi_link_ok\n");
    let _ = std::fs::remove_dir_all(&tmp);
}
