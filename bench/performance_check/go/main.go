// =====================================================================
// Multi-language stress benchmark — Go implementation.
//
// Equivalent work to ../zz/*.zz — same workloads, same totals, same
// stdout protocol so the runner can compare apples-to-apples.
//
//	bench_memory_leak    -> 50M short-lived allocs across 2 passes
//	bench_cpu_intensive  -> 10M accum + 1M powmod + 1M array ops
//	bench_string_concats -> string concat rounds (50x5k + 20x2k + 10k)
//
// Select a benchmark via argv[1]:
//
//	go run main.go memory_leak
//	go run main.go cpu_intensive
//	go run main.go string_concats
//
// =====================================================================
package main

import (
	"fmt"
	"math"
	"os"
	"strconv"
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
// dispatch
// ---------------------------------------------------------------------
func main() {
	if len(os.Args) < 2 {
		fmt.Fprintln(os.Stderr, "usage: main <memory_leak|cpu_intensive|string_concats>")
		os.Exit(2)
	}
	switch os.Args[1] {
	case "memory_leak":
		benchMemoryLeak()
	case "cpu_intensive":
		benchCpuIntensive()
	case "string_concats":
		benchStringConcats()
	default:
		fmt.Fprintf(os.Stderr, "unknown benchmark: %s\n", os.Args[1])
		os.Exit(2)
	}
	_ = strconv.Itoa // keep import if future extensions need it
}
