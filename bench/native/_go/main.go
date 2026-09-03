package main
import (
  "fmt"
  "os"
)
func loop() { var s int64; for i := int64(0); i < 10000000; i++ { s += i }; fmt.Println(s) }
func fib(n int) int { if n <= 1 { return n }; return fib(n-1)+fib(n-2) }
func strBench() {
  s := ""
  for i := 0; i < 20000; i++ { s += "a" }
  fmt.Println(len(s))
}
func main() {
  switch os.Args[1] {
  case "loop": loop()
  case "fib": fmt.Println(fib(30))
  case "str": strBench()
  }
}
