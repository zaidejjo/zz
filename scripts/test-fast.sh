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

echo "== fmt =="
cargo fmt --check

echo "== clippy (-D warnings) =="
cargo clippy --all-targets -- -D warnings

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
	--test concurrency_audit_regression \
	--test native_build \
	--test performance_check_regression

echo "== parity (VM leg only; native leg needs --native builds) =="
ZZ_PARITY_VM_ONLY=1 cargo test -p zz_cli --test dual_engine_parity

echo
echo "FAST SUITE GREEN."
echo "For full dual-engine + plugin coverage: cargo test --all"
