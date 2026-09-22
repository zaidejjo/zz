package main

import (
	"fmt"
	"time"
)

func main() {
	const n = 20000
	c1 := make(chan int)
	c2 := make(chan int)
	go func() {
		for i := 0; i < n; i++ {
			v := <-c1
			c2 <- v + 1
		}
	}()
	t0 := time.Now().UnixNano()
	last := 0
	for i := 0; i < n; i++ {
		c1 <- i
		last = <-c2
	}
	t1 := time.Now().UnixNano()
	el := t1 - t0
	fmt.Printf("pingpong elapsed_ns=%d last=%d ns_per_rt=%d\n", el, last, el/n)

	const m = 10000
	c := make(chan int)
	t2 := time.Now().UnixNano()
	for i := 0; i < m; i++ {
		go func(v int) { c <- v }(i)
	}
	sum := 0
	for i := 0; i < m; i++ {
		sum += <-c
	}
	t3 := time.Now().UnixNano()
	fspan := t3 - t2
	fmt.Printf("fanin elapsed_ns=%d sum=%d tasks_per_s=%d\n", fspan, sum, m*1_000_000_000/fspan)
	fmt.Println("bench_handoff_ok")
}
