//! Embedded C runtime for the ZZ native (AOT) backend.
//!
//! The runtime is written in C and shipped inside the compiler binary via
//! `include_str!`: the modular headers/sources under `src/runtime/` are
//! concatenated (dependency order) into two constants that codegen splices
//! into every generated translation unit.
//!
//! `build.rs` folds a content hash of the C sources into [`RUNTIME_SRC_HASH`]
//! so editing C without touching Rust still forces a rebuild — cargo does
//! not track `include_str!` inputs on its own.

/// The embedded C runtime header. The runtime is split across modular
/// sub-headers under `src/runtime/`; the umbrella `runtime.h` includes them
/// in dependency order, and they are concatenated here into one string.
pub const RUNTIME_H: &str = concat!(
    include_str!("runtime/runtime.h"),
    include_str!("runtime/platform.h"),
    include_str!("runtime/core.h"),
    include_str!("runtime/memory.h"),
    include_str!("runtime/strings.h"),
    include_str!("runtime/collections.h"),
    include_str!("runtime/json.h"),
);

/// The embedded C runtime implementation. The modular `.c` files are
/// concatenated in dependency order into a single translation unit.
pub const RUNTIME_C: &str = concat!(
    include_str!("runtime/memory.c"),
    include_str!("runtime/strings.c"),
    include_str!("runtime/collections.c"),
    include_str!("runtime/json.c"),
    include_str!("runtime/core.c"),
);

/// Version marker for generated binaries.
pub const C_BUILD_VERSION: &str = "0.1.0";

/// Content hash of the embedded C runtime sources, stamped by build.rs
/// (`ZZ_RUNTIME_HASH`). Referenced so the hash is load-bearing: any runtime
/// edit changes this constant, which changes the crate fingerprint and
/// forces a rebuild + fresh embed. Without it, `include_str!` edits were
/// silently ignored until an unrelated Rust change rebuilt the crate.
pub const RUNTIME_SRC_HASH: &str = match option_env!("ZZ_RUNTIME_HASH") {
    Some(h) => h,
    None => "0000000000000000",
};

/// Remove every `#include "..."` directive from the assembled C source.
///
/// The AOT backend concatenates the modular runtime headers and sources
/// into one translation unit, so quoted includes (which reference files
/// that do not exist at compile time) must be dropped. System includes
/// (`<...>`) are preserved.
pub fn strip_quoted_includes(src: &str) -> String {
    src.lines()
        .filter(|line| {
            let t = line.trim_start();
            !(t.starts_with("#include \"") && t.ends_with('"'))
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runtime_sources_are_embedded() {
        assert!(RUNTIME_H.len() > 1024, "runtime header missing");
        assert!(RUNTIME_C.len() > 1024, "runtime source missing");
        // `build.rs` stamps this; the fallback only fires outside cargo.
        assert_eq!(RUNTIME_SRC_HASH.len(), 16);
    }

    #[test]
    fn strip_quoted_includes_keeps_system_headers() {
        let src = "#include <stdio.h>\n#include \"core.h\"\nint x = 1;\n";
        assert_eq!(strip_quoted_includes(src), "#include <stdio.h>\nint x = 1;");
    }
}
