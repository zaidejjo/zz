// =====================================================================
// Multi-language stress benchmark — Rust implementation.
//
// Equivalent work to ../zz/*.zz and ../go/main.go — same workloads,
// same totals, same stdout protocol.
//
//   bench_memory_leak        -> 50M short-lived allocs across 2 passes
//   bench_cpu_intensive      -> 10M accum + 1M powmod + 1M array ops
//   bench_string_concats     -> string concat rounds (50x5k + 20x2k + 10k)
//   bench_concurrency_stress -> 100k spawns + 1M channel messages
//   bench_http_throughput    -> HTTP server with concurrent connections
//   bench_memory_alloc      -> Mass allocation/destruction patterns
//
// Select a benchmark via argv[1]:
//   cargo run --release -- memory_leak
//   cargo run --release -- cpu_intensive
//   cargo run --release -- string_concats
//   cargo run --release -- concurrency_stress
//   cargo run --release -- http_throughput
//   cargo run --release -- memory_alloc
// =====================================================================

use std::env;
use std::io::{Read, Write};
use std::sync::mpsc;
use std::thread;
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

// ---------------------------------------------------------------------
// bench_concurrency_stress — mirror of bench_concurrency_stress.zz
// ---------------------------------------------------------------------
fn worker(id: i64, ch: mpsc::Sender<i64>, count: i64) {
    for i in 0..count {
        let _ = ch.send(id * count + i);
    }
}

fn bench_concurrency_stress() {
    let t0 = now_ms();

    let (tx, rx) = mpsc::channel();
    let num_workers = 1_000;
    let msgs_per_worker = 1_000;
    let expected_total = num_workers * msgs_per_worker;

    let t1 = now_ms();
    let mut handles = Vec::with_capacity(num_workers);
    for i in 0..num_workers as i64 {
        let tx_clone = tx.clone();
        handles.push(thread::spawn(move || {
            worker(i, tx_clone, msgs_per_worker as i64);
        }));
    }
    let t2 = now_ms();
    drop(tx); // Close the channel

    let mut sum: i64 = 0;
    let mut received: i64 = 0;
    for val in rx {
        sum += val;
        received += 1;
    }
    let t3 = now_ms();

    // Wait for all threads to finish
    for h in handles {
        let _ = h.join();
    }

    let spawn_ms = t2 - t1;
    let recv_ms = t3 - t2;
    let total_ms = t3 - t0;

    println!("spawn_100k_ms: {}", spawn_ms);
    println!("recv_1M_ms: {}", recv_ms);
    println!("total_ms: {}", total_ms);
    if recv_ms > 0 {
        println!("msgs_per_sec: {}", (expected_total as i64 * 1000) / recv_ms);
    }
    if spawn_ms > 0 {
        println!("spawns_per_sec: {}", (num_workers as i64 * 1000) / spawn_ms);
    }
    println!("sum: {}", sum);
    println!("bench_concurrency_stress_ok");
}

// ---------------------------------------------------------------------
// bench_http_throughput — mirror of bench_http_throughput.zz
// ---------------------------------------------------------------------
fn bench_http_throughput() {
    use std::io::Write;

    println!("SERVER_READY");

    let listener = std::net::TcpListener::bind("0.0.0.0:8080").unwrap();
    for stream in listener.incoming() {
        let mut stream = stream.unwrap();
        let mut buf = [0u8; 8192];
        let n = match stream.read(&mut buf) {
            Ok(n) => n,
            Err(_) => continue,
        };
        let text = String::from_utf8_lossy(&buf[..n]).to_string();
        let path = text
            .lines()
            .next()
            .unwrap_or("")
            .split_whitespace()
            .nth(1)
            .unwrap_or("/");

        let response = match path {
            "/" | "/ping" => "HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nOK",
            "/json" => {
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 29\r\n\r\n{\"status\":\"ok\",\"data\":[1,2,3]}"
            }
            "/health" => "HTTP/1.1 200 OK\r\nContent-Length: 7\r\n\r\nhealthy",
            _ => "HTTP/1.1 404 Not Found\r\nContent-Length: 9\r\n\r\nNOT_FOUND",
        };

        let _ = stream.write_all(response.as_bytes());
        let _ = stream.flush();
    }
}

// ---------------------------------------------------------------------
// bench_memory_alloc — mirror of bench_memory_alloc.zz
// ---------------------------------------------------------------------
fn allocate_small_arrays(count: usize) -> i64 {
    let mut sum: i64 = 0;
    for i in 0..count {
        let arr = vec![
            i as i64,
            i as i64 + 1,
            i as i64 + 2,
            i as i64 + 3,
            i as i64 + 4,
        ];
        sum += arr[0] + arr[4];
    }
    sum
}

fn allocate_dicts(count: usize) -> i64 {
    let mut sum: i64 = 0;
    for i in 0..count {
        let mut d = std::collections::HashMap::new();
        d.insert("a", i as i64);
        d.insert("b", i as i64 + 1);
        d.insert("c", i as i64 + 2);
        sum += d.get("a").unwrap_or(&0) + d.get("c").unwrap_or(&0);
    }
    sum
}

fn allocate_strings(count: usize) -> i64 {
    let mut sum: i64 = 0;
    for i in 0..count {
        let s = format!("item_{}_data", i);
        sum += s.len() as i64;
    }
    sum
}

fn allocate_nested(count: usize) -> i64 {
    let mut sum: i64 = 0;
    for i in 0..count {
        let outer = vec![i as i64, i as i64 + 1];
        let mut m = std::collections::HashMap::new();
        m.insert("x", outer[0]);
        m.insert("y", outer[1]);
        let arr = vec![
            *m.get("x").unwrap_or(&0),
            *m.get("y").unwrap_or(&0),
            i as i64,
        ];
        sum += arr[0] + arr[2];
    }
    sum
}

fn bench_memory_alloc() {
    let t0 = now_ms();

    // Wave 1: Small arrays (1M iterations)
    let mut sum1: i64 = 0;
    let wave1_start = now_ms();
    for _wave in 0..10 {
        sum1 += allocate_small_arrays(100_000);
    }
    let wave1_end = now_ms();
    println!("wave1_arrays_1M_ms: {}", wave1_end - wave1_start);

    // Wave 2: Dictionaries (500k iterations)
    let mut sum2: i64 = 0;
    let wave2_start = now_ms();
    for _wave in 0..5 {
        sum2 += allocate_dicts(100_000);
    }
    let wave2_end = now_ms();
    println!("wave2_dicts_500k_ms: {}", wave2_end - wave2_start);

    // Wave 3: Strings (500k iterations)
    let mut sum3: i64 = 0;
    let wave3_start = now_ms();
    for _wave in 0..5 {
        sum3 += allocate_strings(100_000);
    }
    let wave3_end = now_ms();
    println!("wave3_strings_500k_ms: {}", wave3_end - wave3_start);

    // Wave 4: Nested structures (200k iterations)
    let mut sum4: i64 = 0;
    let wave4_start = now_ms();
    for _wave in 0..2 {
        sum4 += allocate_nested(100_000);
    }
    let wave4_end = now_ms();
    println!("wave4_nested_200k_ms: {}", wave4_end - wave4_start);

    // Baseline: Keep data alive (arrays only)
    let baseline_start = now_ms();
    let mut big_arr: Vec<Vec<i64>> = Vec::with_capacity(100_000);
    for i in 0..100_000 {
        big_arr.push(vec![i as i64, i as i64 * 2, i as i64 % 1000]);
    }
    let baseline_end = now_ms();
    println!("baseline_arr_len: {}", big_arr.len());
    println!("baseline_alloc_100k_ms: {}", baseline_end - baseline_start);

    // Cleanup
    drop(big_arr);

    let t1 = now_ms();
    println!("total_ms: {}", t1 - t0);
    println!("signature_sum: {}", sum1 + sum2 + sum3 + sum4);
    println!("bench_memory_alloc_ok");
}

fn main() {
    let args: Vec<String> = env::args().collect();
    let which = args.get(1).map(String::as_str).unwrap_or("");
    let _ = Instant::now(); // warm up time machinery; kept for future precise timing
    match which {
        "memory_leak" => bench_memory_leak(),
        "cpu_intensive" => bench_cpu_intensive(),
        "string_concats" => bench_string_concats(),
        "concurrency_stress" => bench_concurrency_stress(),
        "http_throughput" => bench_http_throughput(),
        "memory_alloc" => bench_memory_alloc(),
        _ => {
            eprintln!(
                "usage: {} <memory_leak|cpu_intensive|string_concats|concurrency_stress|http_throughput|memory_alloc>",
                args.get(0).map(String::as_str).unwrap_or("bench")
            );
            std::process::exit(2);
        }
    }
}
