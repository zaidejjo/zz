// arena_bench.c — Arena vs malloc/free for ZZ's allocation pattern.
//
// ZZ pattern: each function allocates objects, cleans up at scope exit.
// The arena does this in O(1) per function; malloc/free is O(n) per object.
//
// Compile: gcc -O1 -o arena_bench arena_bench.c -lm
// Run:     ./arena_bench

#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>
#include <stdint.h>

#define N 10000000

typedef struct {
    char *buf;
    size_t cap;
    size_t offset;
} arena_t;

void arena_init(arena_t *a, size_t cap) {
    a->buf = malloc(cap);
    a->cap = cap;
    a->offset = 0;
}
void *arena_alloc(arena_t *a, size_t size, size_t align) {
    size_t aligned = (a->offset + align - 1) & ~(align - 1);
    if (aligned + size <= a->cap) {
        void *p = a->buf + aligned;
        a->offset = aligned + size;
        return p;
    }
    size_t nc = a->cap * 2;
    while (nc < aligned + size) nc *= 2;
    char *nb = malloc(nc);
    memcpy(nb, a->buf, a->offset);
    free(a->buf);
    a->buf = nb;
    a->cap = nc;
    void *p = a->buf + aligned;
    a->offset = aligned + size;
    return p;
}
void arena_reset(arena_t *a) { a->offset = 0; }
void arena_destroy(arena_t *a) { free(a->buf); }

typedef struct { uint64_t refs; size_t len, cap; void *items; } sim_obj;

volatile size_t g_sink = 0;

static double ms(struct timespec s, struct timespec e) {
    return (e.tv_sec - s.tv_sec) * 1000.0 + (e.tv_nsec - s.tv_nsec) / 1e6;
}

int main(void) {
    struct timespec t0, t1;
    const int BATCHES = 1000;
    const int K = N / BATCHES;  // 10K per "function call"
    size_t total;

    printf("=== ZZ allocation pattern: %d batches × %d objects ===\n\n", BATCHES, K);

    // ---- malloc/free (current ZZ approach) ----
    total = 0;
    clock_gettime(CLOCK_MONOTONIC, &t0);
    for (int b = 0; b < BATCHES; b++) {
        sim_obj **objs = malloc(K * sizeof(sim_obj *));
        for (int i = 0; i < K; i++) {
            objs[i] = malloc(sizeof(sim_obj));
            objs[i]->refs = 1;
            objs[i]->len = b * K + i;
            objs[i]->cap = 4;
            objs[i]->items = malloc(32);
            total += objs[i]->len;
        }
        for (int i = 0; i < K; i++) {
            free(objs[i]->items);
            free(objs[i]);
        }
        free(objs);
    }
    clock_gettime(CLOCK_MONOTONIC, &t1);
    g_sink = total;
    printf("malloc/free (current):  %7.1f ms\n", ms(t0, t1));

    // ---- Arena (new ZZ approach) ----
    total = 0;
    clock_gettime(CLOCK_MONOTONIC, &t0);
    {
        arena_t a;
        arena_init(&a, 64 * 1024);  // 64KB per function
        for (int b = 0; b < BATCHES; b++) {
            for (int i = 0; i < K; i++) {
                sim_obj *obj = arena_alloc(&a, sizeof(sim_obj), 8);
                obj->refs = 1;
                obj->len = b * K + i;
                obj->cap = 4;
                obj->items = arena_alloc(&a, 32, 8);
                total += obj->len;
            }
            arena_reset(&a);  // O(1) — frees 10K objects at once
        }
        arena_destroy(&a);
    }
    clock_gettime(CLOCK_MONOTONIC, &t1);
    g_sink = total;
    printf("arena + O(1) reset:     %7.1f ms\n", ms(t0, t1));

    // ---- Per-iteration arena reset (simulates tight loop in ZZ) ----
    total = 0;
    const int LOOP_N = 10000000;
    clock_gettime(CLOCK_MONOTONIC, &t0);
    {
        arena_t a;
        arena_init(&a, 64 * 1024);
        for (int i = 0; i < LOOP_N; i++) {
            sim_obj *obj = arena_alloc(&a, sizeof(sim_obj), 8);
            obj->refs = 1;
            obj->len = (size_t)i;
            obj->cap = 4;
            obj->items = arena_alloc(&a, 32, 8);
            total += obj->len;
            // Reset every 100 iterations (simulates loop body scope)
            if (i % 100 == 0) arena_reset(&a);
        }
        arena_destroy(&a);
    }
    clock_gettime(CLOCK_MONOTONIC, &t1);
    g_sink = total;
    printf("arena loop (10M, reset/100): %4.1f ms\n", ms(t0, t1));

    // ---- Per-iteration malloc/free (same loop pattern) ----
    total = 0;
    clock_gettime(CLOCK_MONOTONIC, &t0);
    {
        for (int i = 0; i < LOOP_N; i++) {
            sim_obj *obj = malloc(sizeof(sim_obj));
            obj->refs = 1;
            obj->len = (size_t)i;
            obj->cap = 4;
            obj->items = malloc(32);
            total += obj->len;
            if (i % 100 == 0) {
                // Can't bulk-free with malloc; just free this one
            }
            free(obj->items);
            free(obj);
        }
    }
    clock_gettime(CLOCK_MONOTONIC, &t1);
    g_sink = total;
    printf("malloc loop (10M):           %4.1f ms\n", ms(t0, t1));

    printf("\n");
    return 0;
}
