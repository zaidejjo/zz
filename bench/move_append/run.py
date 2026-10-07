#!/usr/bin/env python3
"""Append-elision benchmark harness: wall time + peak RSS per case.

Engines: VM (`zz run`) and native (prebuilt `zz build` binaries).
Peak RSS comes from `wait4` per-child `ru_maxrss` (same kernel counter
`/usr/bin/time -v` reports as "Maximum resident set size"; GNU time is
not installed on this machine, so the harness reads the counter directly
instead of parsing `time -v` output).

Usage:
    python3 bench/move_append/run.py [--out docs/perf/baseline.md]
"""

import argparse
import datetime
import hashlib
import os
import resource
import signal
import subprocess
import sys
import time

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(os.path.dirname(HERE))
DEFAULT_ZZ = os.path.join(ROOT, "target", "release", "zz")
if not os.path.exists(DEFAULT_ZZ):
    DEFAULT_ZZ = os.path.join(ROOT, "target", "debug", "zz")
# Overridden by --zz below (before/after comparisons across worktrees).
ZZ = DEFAULT_ZZ
BIN = os.path.join(HERE, "bin")
CORPUS = os.path.join(HERE, "corpus")
DEFAULT_OUT = os.path.join(ROOT, "docs", "perf", "baseline.md")

TIMEOUT_S = 590

MICRO = [
    ("push_int", ["10000"]),
    ("push_str", ["5000"]),
    ("field_push", ["10000"]),
    ("thread_doc", ["2000"]),
]

CORPUS_FILES = [
    "flat_1k",
    "mixed_1k",
    "flat_8k",
    "mixed_8k",
    "flat_32k",
    "mixed_32k",
    "flat_128k",
    "mixed_128k",
    "flat_1m",
    "mixed_1m",
]

# VM cannot finish the two largest corpora in any reasonable time
# (superlinear); skip instead of burning two timeout windows.
VM_SKIP = {"flat_128k", "mixed_128k", "flat_1m", "mixed_1m"}


def run_measured(argv, timeout=TIMEOUT_S):
    """Run argv, return (status, wall_ms, peak_rss_kib).

    status is "ok" or "timeout:<ms>" / "error:<code>".
    """
    t0 = time.monotonic()
    pid = os.fork()
    if pid == 0:
        try:
            os.execv(argv[0], argv)
        except Exception:
            os._exit(127)
    deadline = t0 + timeout
    while True:
        try:
            done_pid, status, rusage = os.wait4(pid, os.WNOHANG)
        except ChildProcessError:
            return (f"error:gone", int((time.monotonic() - t0) * 1000), 0)
        if done_pid == pid:
            wall_ms = int((time.monotonic() - t0) * 1000)
            if os.WIFEXITED(status) and os.WEXITSTATUS(status) == 0:
                return ("ok", wall_ms, rusage.ru_maxrss)
            code = os.WEXITSTATUS(status) if os.WIFEXITED(status) else -1
            return (f"error:{code}", wall_ms, rusage.ru_maxrss)
        if time.monotonic() > deadline:
            try:
                os.kill(pid, signal.SIGKILL)
            except ProcessLookupError:
                pass
            _, status, rusage = os.wait4(pid, 0)
            wall_ms = int((time.monotonic() - t0) * 1000)
            return (f"timeout:{wall_ms}ms", wall_ms, rusage.ru_maxrss)
        time.sleep(0.05)


def build_native(drivers):
    os.makedirs(BIN, exist_ok=True)
    bins = {}
    for name in drivers:
        src = os.path.join(HERE, name + ".zz")
        r = subprocess.run(
            [ZZ, "build", src], capture_output=True, text=True, timeout=590
        )
        # `zz build x.zz` drops the binary in <dir>/bin/<stem>.
        cand = os.path.join(HERE, "bin", name)
        if os.path.isfile(cand) and os.access(cand, os.X_OK):
            bins[name] = cand
            print(f"built {name} -> {cand}", flush=True)
        else:
            print(f"BUILD FAIL {name}: {r.stderr[-500:]}", flush=True)
    return bins


def corpus_sha():
    h = hashlib.sha256()
    with open(os.path.join(CORPUS, "MANIFEST.txt"), "rb") as f:
        h.update(f.read())
    return h.hexdigest()[:16]


def zz_version():
    try:
        r = subprocess.run(
            [ZZ, "--version"], capture_output=True, text=True, timeout=30
        )
        return (r.stdout.strip() or r.stderr.strip())[:40]
    except Exception:
        return "unknown"


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--out", default=DEFAULT_OUT)
    ap.add_argument("--skip-build", action="store_true")
    ap.add_argument("--zz", default=DEFAULT_ZZ)
    ap.add_argument("--title", default="Append-elision baseline (pre-optimization)")
    args = ap.parse_args()
    global ZZ
    ZZ = args.zz

    drivers = [m[0] for m in MICRO] + ["parse_proxy"]
    bins = {} if args.skip_build else build_native(drivers)
    if args.skip_build:
        for name in drivers:
            cand = os.path.join(BIN, name)
            if os.path.isfile(cand):
                bins[name] = cand

    rows = []  # (case, engine, status, wall_ms, rss_kib)

    def measure(case, engine, argv):
        print(f"[{engine}] {case} ...", flush=True)
        status, wall_ms, rss = run_measured(argv)
        print(f"  {status} {wall_ms}ms rss={rss}KiB", flush=True)
        rows.append((case, engine, status, wall_ms, rss))

    for name, bench_args in MICRO:
        src = os.path.join(HERE, name + ".zz")
        measure(f"{name} N={bench_args[0]}", "vm", [ZZ, "run", src] + bench_args)
        if name in bins:
            measure(f"{name} N={bench_args[0]}", "native", [bins[name]] + bench_args)

    for stem in CORPUS_FILES:
        path = os.path.join(CORPUS, stem + ".toml")
        if stem not in VM_SKIP:
            measure(
                f"proxy/{stem}",
                "vm",
                [ZZ, "run", os.path.join(HERE, "parse_proxy.zz"), path],
            )
        else:
            rows.append((f"proxy/{stem}", "vm", "skipped-infeasible", 0, 0))
            print(f"[vm] proxy/{stem} ... skipped-infeasible", flush=True)
        if "parse_proxy" in bins:
            measure(f"proxy/{stem}", "native", [bins["parse_proxy"], path])

    commit = subprocess.run(
        ["git", "rev-parse", "--short", "HEAD"],
        capture_output=True,
        text=True,
        cwd=ROOT,
    ).stdout.strip()
    machine = subprocess.run(
        ["uname", "-m"], capture_output=True, text=True
    ).stdout.strip()
    date = datetime.datetime.now(datetime.timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")

    def cell(status, wall_ms, rss):
        if status != "ok":
            return (status, status)
        return (f"{wall_ms}", f"{rss}")

    lines = []
    lines.append(f"# {args.title}")
    lines.append("")
    lines.append(f"Date: {date} · Machine: `{machine}` · zz: `{zz_version()}`")
    lines.append(f"Commit: `{commit}` · Corpus manifest sha: `{corpus_sha()}`")
    lines.append(f"ZZ binary: `{os.path.relpath(ZZ, ROOT)}` · Timeout: {TIMEOUT_S}s")
    lines.append("")
    lines.append("Method: each case runs once per engine; wall time via")
    lines.append("monotonic clock, peak RSS via per-child `wait4` `ru_maxrss`")
    lines.append("(same kernel counter as GNU `time -v`; GNU time is not")
    lines.append("installed on this machine). VM cases over 32 KB are skipped")
    lines.append("above 32 KB (superlinear — infeasible); native 1 MB may hit")
    lines.append("the timeout, which is itself the baseline signal.")
    lines.append("")
    lines.append("## Microbenchmarks (wall ms / peak RSS KiB)")
    lines.append("")
    lines.append("| case | VM | VM RSS | native | native RSS |")
    lines.append("|------|----|--------|--------|------------|")
    seen = []
    for name, bench_args in MICRO:
        case = f"{name} N={bench_args[0]}"
        vm = next(r for r in rows if r[0] == case and r[1] == "vm")
        nat = next(r for r in rows if r[0] == case and r[1] == "native")
        vm_t, vm_r = cell(vm[2], vm[3], vm[4])
        n_t, n_r = cell(nat[2], nat[3], nat[4])
        lines.append(f"| `{case}` | {vm_t} | {vm_r} | {n_t} | {n_r} |")
        seen.append(case)
    lines.append("")
    lines.append("## Corpus proxy (wall ms / peak RSS KiB)")
    lines.append("")
    lines.append("| case | VM | VM RSS | native | native RSS |")
    lines.append("|------|----|--------|--------|------------|")
    for stem in CORPUS_FILES:
        case = f"proxy/{stem}"
        vm = next(r for r in rows if r[0] == case and r[1] == "vm")
        nat = next(r for r in rows if r[0] == case and r[1] == "native")
        vm_t, vm_r = cell(vm[2], vm[3], vm[4])
        n_t, n_r = cell(nat[2], nat[3], nat[4])
        lines.append(f"| `{case}` | {vm_t} | {vm_r} | {n_t} | {n_r} |")
    lines.append("")

    os.makedirs(os.path.dirname(args.out), exist_ok=True)
    with open(args.out, "w") as f:
        f.write("\n".join(lines))
    print(f"wrote {args.out}", flush=True)


if __name__ == "__main__":
    sys.exit(main())
