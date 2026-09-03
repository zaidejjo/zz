package main

import (
	"fmt"
	"math"
	"os"
)

func runLoop() { var s int64; for i := int64(0); i < 10000000; i++ { s += i }; fmt.Println(s) }
func fib(n int) int { if n <= 1 { return n }; return fib(n-1) + fib(n-2) }
func runStr() {
	s := ""
	for i := 0; i < 20000; i++ { s += "a" }
	fmt.Println(len(s))
}
func runMath() {
	acc := int64(0)
	for i := 0; i < 1000000; i++ {
		acc += int64(math.Pow(2, 10)) % 7
	}
	fmt.Println(acc)
}
func main() {
	switch os.Args[1] {
	case "loop": runLoop()
	case "fib": fmt.Println(fib(30))
	case "str": runStr()
	case "math": runMath()
	}
}
