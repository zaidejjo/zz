#!/usr/bin/env bash
# =====================================================================
#  Z Z   B E N C H M A R K   R U N N E R
#  Builds benchmarks, runs them, outputs clean JSON.
#  Markdown generation is a separate Python step.
# =====================================================================
set -e
set -u

HERE="$(cd "$(dirname "$0")" && pwd)"
ROOT="$(cd "$HERE/../.." && pwd)"
RUNS="${RUNS:-3}"
SKIP_VM="${SKIP_VM:-false}"

LOG_DIR="$HERE/.log"
RESULTS_JSON="$HERE/results.json"
RESULTS_MD="$HERE/RESULTS.md"
ZZ_BIN_DIR="$HERE/.zzbin"

# Clean slate — remove only artifacts, NOT results.json (Python will overwrite)
rm -rf "$LOG_DIR" "$HERE/.bin" "$ZZ_BIN_DIR"
mkdir -p "$LOG_DIR" "$HERE/.bin" "$ZZ_BIN_DIR"

# Colors
RST=$'\e[0m'
BLD=$'\e[1m'
RED=$'\e[31m'
GRN=$'\e[32m'
YLW=$'\e[33m'
CYN=$'\e[36m'
DIM=$'\e[2m'

# Find zz binary
ZZ="$ROOT/target/release/zz"
[ -x "$ZZ" ] || ZZ="$ROOT/target/debug/zz"
if [ ! -x "$ZZ" ]; then
	echo "${RED}error${RST}: zz binary not found"
	exit 1
fi

# Check tools
HAVE_GO=0
command -v go >/dev/null 2>&1 && HAVE_GO=1
HAVE_RUST=0
command -v rustc >/dev/null 2>&1 && HAVE_RUST=1
HAVE_WRK=0
command -v wrk >/dev/null 2>&1 && HAVE_WRK=1
HAVE_HEY=0
command -v hey >/dev/null 2>&1 && HAVE_HEY=1
[ "$HAVE_GO" -eq 0 ] && echo "${YLW}warn${RST}: go not found"
[ "$HAVE_RUST" -eq 0 ] && echo "${YLW}warn${RST}: rustc not found"
[ "$HAVE_WRK" -eq 0 ] && [ "$HAVE_HEY" -eq 0 ] && echo "${YLW}warn${RST}: wrk/hey not found - HTTP skipped"

# ---------------------------------------------------------------------
# Build
# ---------------------------------------------------------------------
echo "${BLD}${CYN}━━ building ━━${RST}"

GO_BIN="$HERE/.bin/bench_go"
if [ "$HAVE_GO" -eq 1 ]; then
	(cd "$HERE/go" && go build -o "$GO_BIN" .) >"$LOG_DIR/go_build.log" 2>&1 || HAVE_GO=0
	[ ! -x "$GO_BIN" ] && HAVE_GO=0
fi

RUST_BIN="$HERE/.bin/bench_rust"
if [ "$HAVE_RUST" -eq 1 ]; then
	(cd "$HERE/rust" && cargo build --release --quiet) >"$LOG_DIR/rust_build.log" 2>&1
	if [ -x "$HERE/rust/target/release/bench_stress" ]; then
		cp -f "$HERE/rust/target/release/bench_stress" "$RUST_BIN" && chmod +x "$RUST_BIN"
	else
		HAVE_RUST=0
	fi
	[ ! -x "$RUST_BIN" ] && HAVE_RUST=0
fi

declare -A ZZ_AOT_BIN
for bench in cpu_intensive concurrency_stress http_throughput memory_alloc memory_leak string_concats; do
	src="$HERE/zz/bench_${bench}.zz"
	out="$ZZ_BIN_DIR/bench_${bench}_aot"
	rm -f "$out"
	if "$ZZ" build -p "$src" >"$LOG_DIR/zz_build_${bench}.log" 2>&1; then
		srcbin="$HERE/zz/bench_${bench}"
		if [ -x "$srcbin" ]; then
			mv -f "$srcbin" "$out" && chmod +x "$out" && ZZ_AOT_BIN[$bench]="$out"
		else
			echo "${YLW}warn${RST}: no binary for $bench"
		fi
	else
		echo "${YLW}warn${RST}: zz build failed for $bench"
	fi
done

# ---------------------------------------------------------------------
# Run benchmarks via Python — all JSON generation in one place
# ---------------------------------------------------------------------
export HAVE_GO HAVE_RUST HAVE_WRK HAVE_HEY RUNS
export ZZ_BIN_DIR GO_BIN RUST_BIN

python3 - "$HERE" <<'PYEOF'
import subprocess, json, time, os, sys, re

here = sys.argv[1]
os.chdir(here)

runs = int(os.environ.get("RUNS", "3"))
have_go = os.environ.get("HAVE_GO", "0") == "1"
have_rust = os.environ.get("HAVE_RUST", "0") == "1"
can_http = os.environ.get("HAVE_WRK", "0") == "1" or os.environ.get("HAVE_HEY", "0") == "1"
zz_bin_dir = os.environ.get("ZZ_BIN_DIR", ".zzbin")
go_bin = os.environ.get("GO_BIN", ".bin/bench_go")
rust_bin = os.environ.get("RUST_BIN", ".bin/bench_rust")

log_dir = ".log"
os.makedirs(log_dir, exist_ok=True)

def best_ms(cmd, runs=3):
    best = 999999999
    for _ in range(runs):
        t0 = time.time()
        r = subprocess.run(cmd, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        dt = int((time.time() - t0) * 1000)
        if dt < best: best = dt
    return best

def measure_rss_kib(cmd):
    logfile = f"{log_dir}/_rss_{id(cmd)}.log"
    try:
        r = subprocess.run(["/usr/bin/time", "-v"] + list(cmd), capture_output=True, text=True)
        for line in r.stderr.splitlines():
            if "Maximum resident set size" in line:
                return int(line.split()[-1])
    except Exception:
        pass
    # fallback: ps polling
    proc = subprocess.Popen(cmd, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    peak = 0
    try:
        while proc.poll() is None:
            try:
                rss_line = subprocess.check_output(["ps", "-o", "rss=", "-p", str(proc.pid)], text=True)
                rss = int(rss_line.strip())
                if rss > peak: peak = rss
            except:
                pass
            time.sleep(0.05)
    finally:
        try: proc.terminate(); proc.wait(timeout=2)
        except: proc.kill()
    return peak

def run_http(cmd, eng):
    port = 8080
    subprocess.run(["pkill", "-f", f":{port}"], stderr=subprocess.DEVNULL)
    time.sleep(0.5)

    logfile = f"{log_dir}/http_{eng}.log"
    with open(logfile, "w") as f:
        proc = subprocess.Popen(cmd, stdout=f, stderr=subprocess.STDOUT)

    # Wait for server to be ready
    ready = False
    for i in range(30):
        try:
            r = subprocess.run(["curl", "-s", "--connect-timeout", "1", f"http://127.0.0.1:{port}/"],
                            capture_output=True, timeout=1)
            if r.returncode == 0:
                ready = True
                break
        except:
            pass
        time.sleep(0.1)

    if not ready:
        proc.terminate(); proc.wait()
        return 0

    time.sleep(1)
    rps = 0

    if os.environ.get("HAVE_WRK", "0") == "1":
        try:
            out = subprocess.check_output(
                ["wrk", "-t4", "-c100", "-d3s", f"http://127.0.0.1:{port}/"],
                text=True, stderr=subprocess.DEVNULL, timeout=10
            )
            m = re.search(r'Requests/sec:\s+([\d,.]+)', out)
            if m: rps = int(float(m.group(1).replace(",","")))
        except:
            pass
    elif os.environ.get("HAVE_HEY", "0") == "1":
        try:
            out = subprocess.check_output(
                ["hey", "-n", "10000", "-c", "100", "-m", "GET", f"http://127.0.0.1:{port}/"],
                text=True, stderr=subprocess.DEVNULL, timeout=10
            )
            m = re.search(r'Requests/sec:\s+([\d,.]+)', out)
            if m: rps = int(float(m.group(1).replace(",","")))
        except:
            pass

    proc.terminate()
    try: proc.wait(timeout=2)
    except: proc.kill()
    subprocess.run(["pkill", "-f", f":{port}"], stderr=subprocess.DEVNULL)
    time.sleep(0.5)
    return rps

def binary_kib(path):
    if not os.path.exists(path): return 0
    return (os.path.getsize(path) + 1023) // 1024

BENCHES = ["cpu_intensive", "concurrency_stress", "memory_alloc", "memory_leak", "string_concats"]
HTTP_BENCH = "http_throughput"

results = {}

# Non-HTTP benchmarks
for bench in BENCHES:
    zbin = os.path.join(zz_bin_dir, f"bench_{bench}_aot")
    z_exists = os.path.isfile(zbin) and os.access(zbin, os.X_OK)

    print(f"  {bench}:", flush=True)
    row = {}

    for eng, cmd in [
        ("zz_aot_mt", [zbin] if z_exists else None),
        ("zz_aot_st", ["taskset", "-c", "0", zbin] if z_exists else None),
        ("go", [go_bin, bench] if have_go and os.path.exists(go_bin) else None),
        ("rust", [rust_bin, bench] if have_rust and os.path.exists(rust_bin) else None),
    ]:
        if cmd is None:
            row[eng] = {"elapsed_ms": 0, "peak_rss_kib": 0}
            continue
        ms = best_ms(cmd, runs)
        rss = measure_rss_kib(cmd)
        row[eng] = {"elapsed_ms": ms, "peak_rss_kib": rss}
        print(f"    {eng}: {ms}ms rss={rss}KiB", flush=True)

    results[bench] = row

# HTTP benchmark
print(f"  {HTTP_BENCH}:", flush=True)
zbin = os.path.join(zz_bin_dir, f"bench_{HTTP_BENCH}_aot")
z_exists = os.path.isfile(zbin) and os.access(zbin, os.X_OK)
row = {}

for eng, cmd in [
    ("zz_aot_mt", [zbin] if z_exists else None),
    ("zz_aot_st", ["taskset", "-c", "0", zbin] if z_exists else None),
    ("go", [go_bin, HTTP_BENCH] if have_go and os.path.exists(go_bin) else None),
    ("rust", [rust_bin, HTTP_BENCH] if have_rust and os.path.exists(rust_bin) else None),
]:
    if cmd is None:
        row[eng] = {"rps": 0}
        continue
    rps = run_http(cmd, eng) if can_http else 0
    row[eng] = {"rps": rps}
    print(f"    {eng}: {rps} rps", flush=True)

results[HTTP_BENCH] = row

# Build final data structure
data = {
    "runs": runs,
    "machine": subprocess.check_output(["uname", "-m"], text=True).strip(),
    "date": subprocess.check_output(["date", "-u", "+%Y-%m-%dT%H:%M:%SZ"], text=True).strip(),
    "engines": ["zz_aot_mt", "zz_aot_st", "go", "rust"],
    "results": results,
    "binary_kib": {
        "zz_aot": binary_kib(os.path.join(zz_bin_dir, f"bench_{BENCHES[0]}_aot")),
        "go": binary_kib(go_bin),
        "rust": binary_kib(rust_bin),
    }
}

# OVERWRITE results.json completely
with open("results.json", "w") as f:
    json.dump(data, f, indent=2)

print("JSON written", flush=True)
PYEOF

echo
echo "${BLD}${CYN}━━ results ━━${RST}"

# Generate Markdown from clean JSON
python3 - "$RESULTS_JSON" "$RESULTS_MD" <<'PYEOF'
import json, sys

with open(sys.argv[1]) as f:
    data = json.load(f)

BENCHES = ["cpu_intensive","concurrency_stress","memory_alloc","memory_leak","string_concats"]
HTTP_BENCH = "http_throughput"

def fmt(v): return str(v) if v else "0"
def fmt_rps(v): return f"{v:,.0f}" if v else "0"
def fmt_rss(k):
    if not k: return "0 / 0.0"
    return f"{k} / {k/1024:.1f}"

with open(sys.argv[2], "w") as f:
    f.write(f"""# Comprehensive Multi-Scenario Benchmark — ZZ vs Go vs Rust

**Machine:** `{data["machine"]}` · **Date:** {data["date"]} · **Best-of:** {data["runs"]} runs

## Benchmarks

| Benchmark | Description |
|-----------|-------------|
| `bench_cpu_intensive` | 10M integer accum + 1M pow/mod + array ops |
| `bench_concurrency_stress` | 1000 workers × 1000 msgs (1M total) |
| `bench_http_throughput` | HTTP server with concurrent connections |
| `bench_memory_alloc` | Mass allocation/destruction (GC/Arena stress) |
| `bench_memory_leak` | 50M short-lived allocs across 2 passes |
| `bench_string_concats` | String building patterns (50×5k + 20×2k) |

## Execution time — Non-HTTP benchmarks (lower is better, ms)

| Benchmark | ZZ AOT MT | ZZ AOT ST | Go | Rust |
|-----------|----------:|----------:|---:|-----:|
""")
    for bench in BENCHES:
        r = data["results"][bench]
        f.write(f"| `bench_{bench:16s}` | {fmt(r['zz_aot_mt']['elapsed_ms']):>7} | {fmt(r['zz_aot_st']['elapsed_ms']):>7} | {fmt(r['go']['elapsed_ms']):>3} | {fmt(r['rust']['elapsed_ms']):>3} |\n")

    r = data["results"][HTTP_BENCH]
    f.write(f"""
## HTTP Throughput (higher is better, req/sec)

| Benchmark | ZZ AOT MT | ZZ AOT ST | Go | Rust |
|-----------|----------:|----------:|---:|-----:|
| `bench_{HTTP_BENCH:16s}` | {fmt_rps(r['zz_aot_mt']['rps']):>7} | {fmt_rps(r['zz_aot_st']['rps']):>7} | {fmt_rps(r['go']['rps']):>3} | {fmt_rps(r['rust']['rps']):>3} |

## Peak RSS — Non-HTTP benchmarks (lower is better; KiB / MB)

| Benchmark | ZZ AOT MT | ZZ AOT ST | Go | Rust |
|-----------|-----------|-----------|---:|-----:|
""")
    for bench in BENCHES:
        r = data["results"][bench]
        f.write(f"| `bench_{bench:16s}` | {fmt_rss(r['zz_aot_mt']['peak_rss_kib'])} | {fmt_rss(r['zz_aot_st']['peak_rss_kib'])} | {fmt_rss(r['go']['peak_rss_kib'])} | {fmt_rss(r['rust']['peak_rss_kib'])} |\n")

    bk = data["binary_kib"]
    f.write(f"""
## Binary size (KiB)

| Engine | Binary KiB |
|--------|-----------:|
| ZZ AOT | {bk["zz_aot"]} |
| Go | {bk["go"]} |
| Rust | {bk["rust"]} |

## How to reproduce

```bash
cargo build --release
RUNS=5 bash bench/performance_check/run.sh
```

- **ZZ AOT MT**: Multi-threaded execution (all CPU cores)
- **ZZ AOT ST**: Single-threaded execution (1 CPU core, via `taskset -c 0`)
- **Go/Rust**: compiled once into `bench/performance_check/.bin/`
- Peak RSS via `/usr/bin/time -v` (GNU) or `ps` polling fallback
- HTTP benchmark uses `wrk` or `hey` for load testing (if available)

""")

print(f"Markdown written to {sys.argv[2]}")
PYEOF

echo
echo "${BLD}${GRN}done${RST}  →  ${BLD}$RESULTS_MD${RST}"
cat "$RESULTS_MD"
