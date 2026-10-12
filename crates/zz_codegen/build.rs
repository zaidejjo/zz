//! Re-embed the C runtime when its sources change.
//!
//! The native backend bakes `src/runtime/*.c|h` into the binary via
//! `include_str!`, which cargo does NOT track: editing C without touching
//! Rust silently shipped stale natives (fresh `zz run --native` linked
//! the old runtime with no diagnostic — observed: probes of edited C
//! kept showing old behavior). `rerun-if-changed` alone cannot fix this
//! (it only gates script re-execution, not crate recompilation), so the
//! script also folds a content hash of the runtime sources into
//! `ZZ_RUNTIME_HASH`: any edit changes the env, which changes the crate
//! fingerprint, forcing a rebuild and a fresh embed.
fn main() {
    let files = [
        "src/runtime/core.c",
        "src/runtime/core.h",
        "src/runtime/strings.c",
        "src/runtime/strings.h",
        "src/runtime/collections.c",
        "src/runtime/collections.h",
        "src/runtime/json.c",
        "src/runtime/json.h",
        "src/runtime/memory.c",
        "src/runtime/memory.h",
        "src/runtime/runtime.h",
        "src/runtime/platform.h",
    ];
    for f in files {
        println!("cargo:rerun-if-changed={f}");
    }
    // FNV-1a over the raw bytes (no extra deps in a build script).
    let mut hash: u64 = 0xcbf29ce484222325;
    let mut any = false;
    for f in files {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(f);
        if let Ok(bytes) = std::fs::read(&path) {
            any = true;
            for b in bytes {
                hash ^= b as u64;
                hash = hash.wrapping_mul(0x100000001b3);
            }
        }
    }
    if any {
        println!("cargo:rustc-env=ZZ_RUNTIME_HASH={hash:016x}");
    }
}
