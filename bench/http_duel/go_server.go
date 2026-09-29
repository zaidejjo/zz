// bench/http_duel/go_server.go — minimal net/http contender.
// Serves / -> "OK" (text/plain), keep-alive by default. Mirrors the
// ZZ bench app's hot route; wrk hammers / only.
package main

import (
	"fmt"
	"net/http"
)

func main() {
	http.HandleFunc("/", func(w http.ResponseWriter, r *http.Request) {
		w.Header().Set("Content-Type", "text/plain; charset=utf-8")
		w.Write([]byte("OK"))
	})
	http.HandleFunc("/ping", func(w http.ResponseWriter, r *http.Request) {
		w.Header().Set("Content-Type", "text/plain; charset=utf-8")
		w.Write([]byte("pong"))
	})
	http.HandleFunc("/json", func(w http.ResponseWriter, r *http.Request) {
		w.Header().Set("Content-Type", "text/plain; charset=utf-8")
		w.Write([]byte(`{"status":"ok"}`))
	})
	fmt.Println("SERVER_READY")
	http.ListenAndServe(":8080", nil)
}
