#!/bin/bash
# Batched differential (VM vs native): one native build per K cases.
#
# Usage:
#   tests/fuzz/run_batch.sh [cases_dir] [out_dir]
#   ZZ=path/to/zz tests/fuzz/run_batch.sh            # default /tmp/fuzz layout
#
# Input: `b*.zz` batch files + `manifest.json` (see `gen.py --batch`).
# Each batch runs once per engine; per-case output is demuxed on the
# ZZBEGEN/ZZEND markers and compared with the same normalization as
# run.sh (numeric-only lines stripped, stderr sorted).
#
# A batch that aborts (nonzero exit), loses a marker, or mismatches any
# chunk is BISECTED: its cases are regenerated as singles from the
# manifest (`gen.py --one`, same seeds AND prefixes) and run through
# run.sh. Green batches cost one clang build for K cases; only red
# batches pay per-case builds.
#
# Exit code: 0 iff every batch is green or bisects clean... no: any
# VMFAIL/NATFAIL/DIFF after bisect fails the run (exit 1).
ZZ=${ZZ:-./target/debug/zz}
GEN=${GEN:-tests/fuzz/gen.py}
CASES=${1:-/tmp/fuzz/batches}
OUT=${2:-/tmp/fuzz/batch_results}
mkdir -p "$OUT" "$OUT/singles"
FAILED=0

run_batch_one() {
	b="$1"
	base=$(basename "$b" .zz)
	timeout 120 "$ZZ" run "$b" >"$OUT/$base.vm.out" 2>"$OUT/$base.vm.err"
	vmc=$?
	echo "$vmc" >"$OUT/$base.vm.code"
	timeout 400 "$ZZ" run --native "$b" >"$OUT/$base.nat.out" 2>"$OUT/$base.nat.err"
	natc=$?
	echo "$natc" >"$OUT/$base.nat.code"
	python3 - "$CASES/manifest.json" "$base" "$OUT" "$vmc" "$natc" <<'PYEOF'
import json, re, sys

manifest_path, base, out, vmc, natc = sys.argv[1:6]
vmc, natc = int(vmc), int(natc)
manifest = json.load(open(manifest_path))
cases = manifest[base + ".zz"]

def norm(lines):
    out = []
    for l in lines:
        l = l.rstrip()
        if re.fullmatch(r"[0-9]+", l):
            continue
        out.append(l)
    return out

def chunks(path):
    text = open(path, errors="replace").read().splitlines()
    cur, curkey, res = None, None, {}
    for l in text:
        m = re.match(r"ZZBEGIN (\S+) s(\d+)", l)
        if m:
            curkey, cur = m.group(1), []
            continue
        m = re.match(r"ZZEND (\S+)", l)
        if m:
            if curkey == m.group(1) and cur is not None:
                res[curkey] = cur
            curkey, cur = None, None
            continue
        if cur is not None:
            cur.append(l)
    return res

vm = chunks(f"{out}/{base}.vm.out")
nat = chunks(f"{out}/{base}.nat.out")
bad = []
if vmc != 0 or natc != 0:
    bad = [c["prefix"] for c in cases]
else:
    for c in cases:
        p = c["prefix"]
        if p not in vm or p not in nat:
            bad.append(p)  # lost marker (abort mid-batch)
        elif norm(vm[p]) != norm(nat[p]):
            bad.append(p)
if not bad:
    # stderr must also match sorted (same checker, same file: warnings
    # carry identical batch-relative spans on both engines).
    vmerr = sorted(l.rstrip() for l in open(f"{out}/{base}.vm.err", errors="replace"))
    naterr = sorted(l.rstrip() for l in open(f"{out}/{base}.nat.err", errors="replace"))
    if vmerr != naterr:
        bad = ["stderr"]
if bad:
    print(f"BISECT {base}: {' '.join(bad)}")
else:
    print(f"BATCH-OK {base}")
PYEOF
}

export ZZ GEN CASES OUT
export -f run_batch_one

for b in "$CASES"/b*.zz; do
	[ -e "$b" ] || {
		echo "no batch files in $CASES (see gen.py --batch)"
		exit 2
	}
	run_batch_one "$b"
done | tee "$OUT/batch_summary.txt"

# Bisect red batches into singles (regen from manifest, same seeds AND
# prefixes) and run them through run.sh for exact fault localization.
SCRIPT_DIR=$(cd "$(dirname "$0")" && pwd)
grep "^BISECT" "$OUT/batch_summary.txt" | while read -r _ basecol rest; do
	base="${basecol%:}"
	SUB="$OUT/singles/$base"
	mkdir -p "$SUB"
	python3 - "$CASES/manifest.json" "$base" "$SUB" "$GEN" <<'PYEOF2'
import json, subprocess, sys
manifest_path, base, sub, gen = sys.argv[1:5]
manifest = json.load(open(manifest_path))
for c in manifest[base + ".zz"]:
    out = "%s/%s%s.zz" % (sub, c["prefix"], c["seed"])
    subprocess.run(
        ["python3", gen, "--one", c["shape"], str(c["seed"]), c["prefix"], out],
        check=True,
    )
PYEOF2
	echo "--- bisect $base ---"
	ZZ="$ZZ" "$SCRIPT_DIR/run.sh" "$SUB" "$SUB/results"
done

echo "--- batch counts ---"
grep -c "BATCH-OK" "$OUT/batch_summary.txt" || true
grep -c "BISECT" "$OUT/batch_summary.txt" || true
# Fail if any single, after bisect, still fails/diffs.
if grep -rq "^DIFF\|^VMFAIL\|^NATFAIL" "$OUT"/singles/*/results/summary.txt 2>/dev/null; then
	echo "BATCH RESULT: FAIL (see bisect summaries)"
	exit 1
fi
if grep -q "^BISECT" "$OUT/batch_summary.txt"; then
	# Singles are clean but the batch was not: the batch harness
	# itself lost output (marker collision or abort) — a harness bug,
	# never silently passed.
	echo "BATCH RESULT: FAIL (bisected singles are clean: batch-harness inconsistency)"
	exit 1
fi
echo "BATCH RESULT: OK"
