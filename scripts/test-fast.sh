#!/bin/bash
# Fast iteration suite: everything except the slow native/plugin legs.
#
# Skips:
# - dual_engine_parity native runs (`zz run --native` per fixture; use
#   ZZ_PARITY_VM_ONLY=1 to run the VM leg only, or run the file explicitly
#   for full dual-engine checks)
# - plugin_e2e (builds temp cargo/cdylib projects; ~minutes)
#
# Full gate (CI equivalent): cargo test --all
set -euo pipefail

cd "$(dirname "$0")/.."

# CI runs fmt/clippy once in the `lint` job; the per-platform legs set
# ZZ_CI_SKIP_LINT=1 to avoid repeating the same source-only checks six
# times. Local runs keep both.
if [ -z "${ZZ_CI_SKIP_LINT:-}" ]; then
	echo "== fmt =="
	cargo fmt --check

	echo "== clippy (-D warnings) =="
	cargo clippy --all-targets -- -D warnings
fi

echo "== unit tests (all crates) =="
cargo test --workspace --exclude zz_cli --lib

echo "== zz_cli unit tests =="
cargo test -p zz_cli --lib

echo "== e2e (VM) =="
cargo test -p zz_cli --test e2e

echo "== fast integration targets =="
cargo test -p zz_cli \
	--test bug_hunter \
	--test build_e2e \
	--test chunk_aot_diff \
	--test collections_e2e \
	--test concurrency_audit_regression \
	--test native_build \
	--test performance_check_regression

echo "== hermetic Rust plugin (both engines) =="
cargo test -p zz_cli --test rust_plugin_e2e

echo "== parity (VM leg only; native leg needs --native builds) =="
ZZ_PARITY_VM_ONLY=1 cargo test -p zz_cli --test dual_engine_parity

echo "== bytecode (.zzc load vs source run) parity =="
# Compares `run <source>` against `run --bytecode <compiled .zzc>` on the
# bytecode_file_roundtrip fixtures. PRs skip `cargo test --all`, so this must
# run here too or a stderr divergence (e.g. a frontend warning that the
# bytecode leg bypasses) only shows on push (#630 / #bytecode_file_roundtrip).
cargo test -p zz_cli --test bytecode_parity

echo
echo "FAST SUITE GREEN."
echo "For full dual-engine + plugin coverage: cargo test --all"
