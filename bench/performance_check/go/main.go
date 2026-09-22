// =====================================================================
// Multi-language stress benchmark — Go implementation.
//
// Equivalent work to ../zz/*.zz — same workloads, same totals, same
// stdout protocol so the runner can compare apples-to-apples.
//
//	bench_memory_leak        -> 50M short-lived allocs across 2 passes
//	bench_cpu_intensive      -> 10M accum + 1M powmod + 1M array ops
//	bench_string_concats     -> string concat rounds (50x5k + 20x2k + 10k)
//	bench_concurrency_stress-> 100k spawns + 1M channel messages
//	bench_http_throughput    -> HTTP server with concurrent connections
//	bench_memory_alloc      -> Mass allocation/destruction patterns
//
// Select a benchmark via argv[1]:
//
//	go run main.go memory_leak
//	go run main.go cpu_intensive
//	go run main.go string_concats
//	go run main.go concurrency_stress
//	go run main.go http_throughput
//	go run main.go memory_alloc
//
// =====================================================================
package main

import (
	"fmt"
	"math"
	"net/http"
	"os"
	"strconv"
	"sync"
	"time"
)

func nowMs() int64 { return time.Now().UnixNano() / 1_000_000 }

// ---------------------------------------------------------------------
// bench_memory_leak — mirror of bench_memory_leak.zz
// ---------------------------------------------------------------------
func benchMemoryLeak() {
	sum := 0
	start := nowMs()

	for outer := 0; outer < 5; outer++ {
		for middle := 0; middle < 1000; middle++ {
			for inner := 0; inner < 100; inner++ {
				a := []int{outer, middle, inner}
				b := []int{outer + 1, middle + 1, inner + 1}
				c := []int{outer + 2, middle + 2, inner + 2}
				d := []int{outer + 3, middle + 3, inner + 3}
				e := []int{outer + 4, middle + 4, inner + 4}
				f := []int{outer + 5, middle + 5, inner + 5}
				g := []int{outer + 6, middle + 6, inner + 6}
				h := []int{outer + 7, middle + 7, inner + 7}
				i := []int{outer + 8, middle + 8, inner + 8}
				j := []int{outer + 9, middle + 9, inner + 9}
				kv := map[int]int{outer: middle, middle: inner, inner: outer}
				sum += len(a) + len(b) + len(c) + len(d) + len(e) +
					len(f) + len(g) + len(h) + len(i) + len(j) +
					len(kv)
			}
		}
	}

	mid := nowMs()

	for outer := 0; outer < 5; outer++ {
		for middle := 0; middle < 1000; middle++ {
			for inner := 0; inner < 100; inner++ {
				a := []int{outer, middle, inner}
				b := []int{outer + 1, middle + 1, inner + 1}
				c := []int{outer + 2, middle + 2, inner + 2}
				d := []int{outer + 3, middle + 3, inner + 3}
				e := []int{outer + 4, middle + 4, inner + 4}
				f := []int{outer + 5, middle + 5, inner + 5}
				g := []int{outer + 6, middle + 6, inner + 6}
				h := []int{outer + 7, middle + 7, inner + 7}
				i := []int{outer + 8, middle + 8, inner + 8}
				j := []int{outer + 9, middle + 9, inner + 9}
				kv := map[int]int{outer: middle, middle: inner, inner: outer}
				sum += len(a) + len(b) + len(c) + len(d) + len(e) +
					len(f) + len(g) + len(h) + len(i) + len(j) +
					len(kv)
			}
		}
	}

	end := nowMs()
	fmt.Printf("pass1_ms: %d\n", mid-start)
	fmt.Printf("pass2_ms: %d\n", end-mid)
	fmt.Printf("sum: %d\n", sum)
	fmt.Println("bench_memory_leak_ok")
}

// ---------------------------------------------------------------------
// bench_cpu_intensive — mirror of bench_cpu_intensive.zz
// ---------------------------------------------------------------------
func benchCpuIntensive() {
	sum := 0
	t0 := nowMs()
	for i := 0; i < 10_000_000; i++ {
		sum += i
	}
	t1 := nowMs()
	fmt.Printf("accum_10M_ms: %d\n", t1-t0)

	acc := 0
	for i := 0; i < 1_000_000; i++ {
		acc += int(math.Pow(float64(i), 2)) % 97
	}
	t2 := nowMs()
	fmt.Printf("powmod_1M_ms: %d\n", t2-t1)
	fmt.Printf("powmod_sum: %d\n", acc)

	arr := make([]int, 0, 1_000_000)
	for i := 0; i < 1_000_000; i++ {
		arr = append(arr, i)
	}
	t3 := nowMs()
	fmt.Printf("fill_1M_ms: %d\n", t3-t2)

	s := 0
	for _, v := range arr {
		s += v
	}
	t4 := nowMs()
	fmt.Printf("sum_1M_ms: %d\n", t4-t3)
	fmt.Printf("arr_sum: %d\n", s)

	t5 := nowMs()
	fmt.Printf("total_ms: %d\n", t5-t0)
	fmt.Printf("signature_sum: %d\n", sum+acc+s)
	fmt.Println("bench_cpu_intensive_ok")
}

// ---------------------------------------------------------------------
// bench_string_concats — mirror of bench_string_concats.zz
// ---------------------------------------------------------------------
func benchStringConcats() {
	t0 := nowMs()
	roundSum := 0
	for r := 0; r < 50; r++ {
		s := ""
		for i := 0; i < 5000; i++ {
			s += "a"
		}
		roundSum += len(s)
	}
	t1 := nowMs()
	fmt.Printf("round1_50x5000_ms: %d\n", t1-t0)
	fmt.Printf("round1_chars: %d\n", roundSum)

	roundSum = 0
	chunk := "hello-"
	for r := 0; r < 20; r++ {
		s := ""
		for i := 0; i < 2000; i++ {
			s += chunk
		}
		roundSum += len(s)
	}
	t2 := nowMs()
	fmt.Printf("round2_20x2000_ms: %d\n", t2-t1)
	fmt.Printf("round2_chars: %d\n", roundSum)

	s := ""
	for i := 0; i < 10000; i++ {
		s += "x"
	}
	t3 := nowMs()
	fmt.Printf("round3_10000_ms: %d\n", t3-t2)
	fmt.Printf("round3_chars: %d\n", len(s))

	t4 := nowMs()
	fmt.Printf("total_ms: %d\n", t4-t0)
	fmt.Printf("signature_total_chars: %d\n", roundSum*2+len(s))
	fmt.Println("bench_string_concats_ok")
}

// ---------------------------------------------------------------------
// bench_concurrency_stress — mirror of bench_concurrency_stress.zz
// ---------------------------------------------------------------------
func worker(id int, ch chan int, count int) {
	for i := 0; i < count; i++ {
		ch <- id*count + i
	}
}

func benchConcurrencyStress() {
	t0 := nowMs()

	ch := make(chan int)
	numWorkers := 1000
	msgsPerWorker := 1000
	expectedTotal := numWorkers * msgsPerWorker

	t1 := nowMs()
	spawnCount := 0
	for i := 0; i < numWorkers; i++ {
		go worker(i, ch, msgsPerWorker)
		spawnCount++
	}
	t2 := nowMs()

	sum := 0
	received := 0
	for received < expectedTotal {
		val := <-ch
		sum += val
		received++
	}
	t3 := nowMs()

	spawnMs := t2 - t1
	recvMs := t3 - t2
	totalMs := t3 - t0

	fmt.Printf("spawn_100k_ms: %d\n", spawnMs)
	fmt.Printf("recv_1M_ms: %d\n", recvMs)
	fmt.Printf("total_ms: %d\n", totalMs)
	if recvMs > 0 {
		fmt.Printf("msgs_per_sec: %d\n", (int64(expectedTotal)*1000)/recvMs)
	}
	if spawnMs > 0 {
		fmt.Printf("spawns_per_sec: %d\n", (spawnCount*1000)/int(spawnMs))
	}
	fmt.Printf("sum: %d\n", sum)
	fmt.Println("bench_concurrency_stress_ok")
}

// ---------------------------------------------------------------------
// bench_http_throughput — mirror of bench_http_throughput.zz
// ---------------------------------------------------------------------
var httpRequestCount int64 = 0

func httpHandler(w http.ResponseWriter, r *http.Request) {
	httpRequestCount++
	switch r.URL.Path {
	case "/", "/ping":
		w.Write([]byte("OK"))
	case "/json":
		w.Header().Set("Content-Type", "application/json")
		w.Write([]byte(`{"status":"ok","data":[1,2,3]}`))
	case "/health":
		w.Write([]byte("healthy"))
	default:
		w.WriteHeader(404)
		w.Write([]byte("NOT_FOUND"))
	}
}

func benchHttpThroughput() {
	fmt.Println("SERVER_READY")

	server := &http.Server{
		Addr:    ":8080",
		Handler: http.HandlerFunc(httpHandler),
	}
	server.ListenAndServe()
}

// ---------------------------------------------------------------------
// bench_memory_alloc — mirror of bench_memory_alloc.zz
// ---------------------------------------------------------------------
func allocateSmallArrays(count int) int {
	sum := 0
	for i := 0; i < count; i++ {
		arr := []int{i, i + 1, i + 2, i + 3, i + 4}
		sum += arr[0] + arr[4]
	}
	return sum
}

func allocateDicts(count int) int {
	sum := 0
	for i := 0; i < count; i++ {
		d := map[string]int{"a": i, "b": i + 1, "c": i + 2}
		sum += d["a"] + d["c"]
	}
	return sum
}

func allocateStrings(count int) int {
	sum := 0
	for i := 0; i < count; i++ {
		s := fmt.Sprintf("item_%d_data", i)
		sum += len(s)
	}
	return sum
}

func allocateNested(count int) int {
	sum := 0
	for i := 0; i < count; i++ {
		outer := []int{i, i + 1}
		m := map[string]int{"x": outer[0], "y": outer[1]}
		arr := []int{m["x"], m["y"], i}
		sum += arr[0] + arr[2]
	}
	return sum
}

func benchMemoryAlloc() {
	t0 := nowMs()

	// Wave 1: Small arrays (1M iterations)
	sum1 := 0
	wave1Start := nowMs()
	for wave := 0; wave < 10; wave++ {
		sum1 += allocateSmallArrays(100000)
	}
	wave1End := nowMs()
	fmt.Printf("wave1_arrays_1M_ms: %d\n", wave1End-wave1Start)

	// Wave 2: Dictionaries (500k iterations)
	sum2 := 0
	wave2Start := nowMs()
	for wave := 0; wave < 5; wave++ {
		sum2 += allocateDicts(100000)
	}
	wave2End := nowMs()
	fmt.Printf("wave2_dicts_500k_ms: %d\n", wave2End-wave2Start)

	// Wave 3: Strings (500k iterations)
	sum3 := 0
	wave3Start := nowMs()
	for wave := 0; wave < 5; wave++ {
		sum3 += allocateStrings(100000)
	}
	wave3End := nowMs()
	fmt.Printf("wave3_strings_500k_ms: %d\n", wave3End-wave3Start)

	// Wave 4: Nested structures (200k iterations)
	sum4 := 0
	wave4Start := nowMs()
	for wave := 0; wave < 2; wave++ {
		sum4 += allocateNested(100000)
	}
	wave4End := nowMs()
	fmt.Printf("wave4_nested_200k_ms: %d\n", wave4End-wave4Start)

	// Baseline: Keep data alive (arrays only)
	baselineStart := nowMs()
	bigArr := make([][]int, 100000)
	for i := 0; i < 100000; i++ {
		bigArr[i] = []int{i, i * 2, i % 1000}
	}
	baselineEnd := nowMs()
	fmt.Printf("baseline_arr_len: %d\n", len(bigArr))
	fmt.Printf("baseline_alloc_100k_ms: %d\n", baselineEnd-baselineStart)

	// Cleanup
	bigArr = nil

	t1 := nowMs()
	fmt.Printf("total_ms: %d\n", t1-t0)
	fmt.Printf("signature_sum: %d\n", sum1+sum2+sum3+sum4)
	fmt.Println("bench_memory_alloc_ok")
}

// ---------------------------------------------------------------------
// dispatch
// ---------------------------------------------------------------------
func main() {
	if len(os.Args) < 2 {
		fmt.Fprintln(os.Stderr, "usage: main <memory_leak|cpu_intensive|string_concats|concurrency_stress|http_throughput|memory_alloc>")
		os.Exit(2)
	}
	switch os.Args[1] {
	case "memory_leak":
		benchMemoryLeak()
	case "cpu_intensive":
		benchCpuIntensive()
	case "string_concats":
		benchStringConcats()
	case "concurrency_stress":
		benchConcurrencyStress()
	case "http_throughput":
		benchHttpThroughput()
	case "memory_alloc":
		benchMemoryAlloc()
	default:
		fmt.Fprintf(os.Stderr, "unknown benchmark: %s\n", os.Args[1])
		os.Exit(2)
	}
	_ = strconv.Itoa // keep import if future extensions need it
	_ = sync.WaitGroup{}
}
