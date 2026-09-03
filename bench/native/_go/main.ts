function runLoop() { let s = 0; for (let i = 0; i < 10000000; i++) s += i; console.log(s); }
function fib(n: number): number { if (n <= 1) return n; return fib(n - 1) + fib(n - 2); }
function runStr() { let s = ""; for (let i = 0; i < 20000; i++) s += "a"; console.log(s.length); }
function runMath() { let acc = 0; for (let i = 0; i < 1000000; i++) acc += Math.pow(2, 10) % 7; console.log(acc); }
switch (process.argv[2]) {
  case "loop": runLoop(); break;
  case "fib": console.log(fib(30)); break;
  case "str": runStr(); break;
  case "math": runMath(); break;
}
