/*
 * Link-time fallbacks for the AOT C runtime symbols called by this crate.
 *
 * `zz_native_rt` is built both as an rlib (linked into other crates' test
 * binaries) and as a staticlib (linked into AOT programs together with the
 * C runtime, which *defines* `zz_str_new` & friends). The Rust FFI entry
 * points here call those symbols, so a test binary that pulls one of them
 * has no definition to resolve against. ELF/macOS linkers happen to drop
 * the unreferenced entry points; MSVC keeps them and fails the link with
 * LNK2019 for every C constructor.
 *
 * This object therefore maps each runtime symbol to an aborting stub
 * *only when the real definition is missing*:
 *   - MSVC/lld-link: `/alternatename` fallback (no symbol definition, so a
 *     real runtime definition always wins),
 *   - GNU/ELF toolchains: a weak definition (overridable by a strong one).
 *
 * A test binary never calls these functions; landing in the stub means a
 * Rust-side FFI entry point was reached without the AOT runtime linked,
 * which is a programming error, hence the abort.
 *
 * Names are undecorated: ZZ only targets x86_64 and aarch64 Windows.
 * `build.rs` compiles this file for Windows targets only.
 */

/* Header-free on purpose: the shim must compile in cross builds where the
 * target's SDK headers are not on the include path yet. */
void abort(void);

#if defined(_MSC_VER)

void zz_cabi_missing(void);
void zz_cabi_missing(void) { abort(); }

#pragma comment(linker, "/alternatename:zz_str_new=zz_cabi_missing")
#pragma comment(linker, "/alternatename:zz_array_new=zz_cabi_missing")
#pragma comment(linker, "/alternatename:zz_array_push=zz_cabi_missing")
#pragma comment(linker, "/alternatename:zz_array_len=zz_cabi_missing")
#pragma comment(linker, "/alternatename:zz_array_at=zz_cabi_missing")
#pragma comment(linker, "/alternatename:zz_variant_some=zz_cabi_missing")
#pragma comment(linker, "/alternatename:zz_variant_ok=zz_cabi_missing")
#pragma comment(linker, "/alternatename:zz_variant_err=zz_cabi_missing")
#pragma comment(linker, "/alternatename:zz_str_view=zz_cabi_missing")
#pragma comment(linker, "/alternatename:zz_bytes_new=zz_cabi_missing")
#pragma comment(linker, "/alternatename:zz_bytes_view=zz_cabi_missing")
#pragma comment(linker, "/alternatename:zz_value_release=zz_cabi_missing")
#pragma comment(linker, "/alternatename:zz_dict_new=zz_cabi_missing")
#pragma comment(linker, "/alternatename:zz_index_set=zz_cabi_missing")

#else /* GNU-compatible Windows toolchains (weak definitions) */

#define ZZ_CABI_FALLBACK(name)                                                 \
    void name(void) __attribute__((weak));                                     \
    void name(void) { abort(); }

ZZ_CABI_FALLBACK(zz_str_new)
ZZ_CABI_FALLBACK(zz_array_new)
ZZ_CABI_FALLBACK(zz_array_push)
ZZ_CABI_FALLBACK(zz_array_len)
ZZ_CABI_FALLBACK(zz_array_at)
ZZ_CABI_FALLBACK(zz_variant_some)
ZZ_CABI_FALLBACK(zz_variant_ok)
ZZ_CABI_FALLBACK(zz_variant_err)
ZZ_CABI_FALLBACK(zz_str_view)
ZZ_CABI_FALLBACK(zz_bytes_new)
ZZ_CABI_FALLBACK(zz_bytes_view)
ZZ_CABI_FALLBACK(zz_value_release)
ZZ_CABI_FALLBACK(zz_dict_new)
ZZ_CABI_FALLBACK(zz_index_set)

#endif
