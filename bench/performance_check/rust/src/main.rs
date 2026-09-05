// =====================================================================
// Multi-language stress benchmark — Rust implementation.
//
// Equivalent work to ../zz/*.zz and ../go/main.go — same workloads,
// same totals, same stdout protocol.
//
//   bench_memory_leak    -> 50M short-lived allocs across 2 passes
//   bench_cpu_intensive  -> 10M accum + 1M powmod + 1M array ops
//   bench_string_concats -> string concat rounds (50x5k + 20x2k + 10k)
//
// Select a benchmark via argv[1]:
//   cargo run --release -- memory_leak
//   cargo run --release -- cpu_intensive
//   cargo run --release -- string_concats
// =====================================================================

use std::env;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

// ---------------------------------------------------------------------
// bench_memory_leak — mirror of bench_memory_leak.zz
// ---------------------------------------------------------------------
fn bench_memory_leak() {
    let mut sum: i64 = 0;
    let start = now_ms();

    for outer in 0..5 {
        for middle in 0..1000 {
            for inner in 0..100 {
                let a = vec![outer, middle, inner];
                let b = vec![outer + 1, middle + 1, inner + 1];
                let c = vec![outer + 2, middle + 2, inner + 2];
                let d = vec![outer + 3, middle + 3, inner + 3];
                let e = vec![outer + 4, middle + 4, inner + 4];
                let f = vec![outer + 5, middle + 5, inner + 5];
                let g = vec![outer + 6, middle + 6, inner + 6];
                let h = vec![outer + 7, middle + 7, inner + 7];
                let i = vec![outer + 8, middle + 8, inner + 8];
                let j = vec![outer + 9, middle + 9, inner + 9];
                // small dict equivalent
                let mut kv = std::collections::HashMap::with_capacity(3);
                kv.insert(outer, middle);
                kv.insert(middle, inner);
                kv.insert(inner, outer);

                sum += (a.len()
                    + b.len()
                    + c.len()
                    + d.len()
                    + e.len()
                    + f.len()
                    + g.len()
                    + h.len()
                    + i.len()
                    + j.len()
                    + kv.len()) as i64;
            }
        }
    }

    let mid = now_ms();

    for outer in 0..5 {
        for middle in 0..1000 {
            for inner in 0..100 {
                let a = vec![outer, middle, inner];
                let b = vec![outer + 1, middle + 1, inner + 1];
                let c = vec![outer + 2, middle + 2, inner + 2];
                let d = vec![outer + 3, middle + 3, inner + 3];
                let e = vec![outer + 4, middle + 4, inner + 4];
                let f = vec![outer + 5, middle + 5, inner + 5];
                let g = vec![outer + 6, middle + 6, inner + 6];
                let h = vec![outer + 7, middle + 7, inner + 7];
                let i = vec![outer + 8, middle + 8, inner + 8];
                let j = vec![outer + 9, middle + 9, inner + 9];
                let mut kv = std::collections::HashMap::with_capacity(3);
                kv.insert(outer, middle);
                kv.insert(middle, inner);
                kv.insert(inner, outer);

                sum += (a.len()
                    + b.len()
                    + c.len()
                    + d.len()
                    + e.len()
                    + f.len()
                    + g.len()
                    + h.len()
                    + i.len()
                    + j.len()
                    + kv.len()) as i64;
            }
        }
    }

    let end = now_ms();
    println!("pass1_ms: {}", mid - start);
    println!("pass2_ms: {}", end - mid);
    println!("sum: {}", sum);
    println!("bench_memory_leak_ok");
}

// ---------------------------------------------------------------------
// bench_cpu_intensive — mirror of bench_cpu_intensive.zz
// ---------------------------------------------------------------------
fn bench_cpu_intensive() {
    let mut sum: i64 = 0;
    let t0 = now_ms();

    for i in 0..10_000_000 {
        sum += i;
    }
    let t1 = now_ms();
    println!("accum_10M_ms: {}", t1 - t0);

    let mut acc: i64 = 0;
    for i in 0..1_000_000 {
        acc += (i as i64).pow(2) % 97;
    }
    let t2 = now_ms();
    println!("powmod_1M_ms: {}", t2 - t1);
    println!("powmod_sum: {}", acc);

    let mut arr: Vec<i64> = Vec::with_capacity(1_000_000);
    for i in 0..1_000_000 {
        arr.push(i as i64);
    }
    let t3 = now_ms();
    println!("fill_1M_ms: {}", t3 - t2);

    let mut s: i64 = 0;
    for v in &arr {
        s += *v;
    }
    let t4 = now_ms();
    println!("sum_1M_ms: {}", t4 - t3);
    println!("arr_sum: {}", s);

    let t5 = now_ms();
    println!("total_ms: {}", t5 - t0);
    println!("signature_sum: {}", sum + acc + s);
    println!("bench_cpu_intensive_ok");
}

// ---------------------------------------------------------------------
// bench_string_concats — mirror of bench_string_concats.zz
// ---------------------------------------------------------------------
fn bench_string_concats() {
    let t0 = now_ms();

    let mut round_sum: usize = 0;
    for _r in 0..50 {
        let mut s = String::new();
        for _i in 0..5000 {
            s.push('a');
        }
        round_sum += s.len();
    }
    let t1 = now_ms();
    println!("round1_50x5000_ms: {}", t1 - t0);
    println!("round1_chars: {}", round_sum);

    let mut round_sum2: usize = 0;
    let chunk = "hello-";
    for _r in 0..20 {
        let mut s = String::new();
        for _i in 0..2000 {
            s.push_str(chunk);
        }
        round_sum2 += s.len();
    }
    let t2 = now_ms();
    println!("round2_20x2000_ms: {}", t2 - t1);
    println!("round2_chars: {}", round_sum2);

    let mut s = String::new();
    for _i in 0..10000 {
        s.push('x');
    }
    let t3 = now_ms();
    println!("round3_10000_ms: {}", t3 - t2);
    println!("round3_chars: {}", s.len());

    let t4 = now_ms();
    println!("total_ms: {}", t4 - t0);
    println!("signature_total_chars: {}", (round_sum * 2) + s.len());
    println!("bench_string_concats_ok");
}

fn main() {
    let args: Vec<String> = env::args().collect();
    let which = args.get(1).map(String::as_str).unwrap_or("");
    let _ = Instant::now(); // warm up time machinery; kept for future precise timing
    match which {
        "memory_leak" => bench_memory_leak(),
        "cpu_intensive" => bench_cpu_intensive(),
        "string_concats" => bench_string_concats(),
        _ => {
            eprintln!(
                "usage: {} <memory_leak|cpu_intensive|string_concats>",
                args.get(0).map(String::as_str).unwrap_or("bench")
            );
            std::process::exit(2);
        }
    }
}
