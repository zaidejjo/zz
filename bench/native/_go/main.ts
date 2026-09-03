function loopBun() { let s: number = 0; for (let i = 0; i < 10000000; i++) s += i; console.log(s); }
function fib(n: number): number { if (n <= 1) return n; return fib(n-1)+fib(n-2); }
function strBun() { let st = ""; for (let i = 0; i < 20000; i++) st += "a"; console.log(st.length); }
switch (process.argv[2]) {
  case "loop": loopBun(); break;
  case "fib": console.log(fib(30)); break;
  case "str": strBun(); break;
}
