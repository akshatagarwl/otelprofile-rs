// Compare native ITIMER_PROF delivery with thread CPU without invoking an unwinder.
#define _POSIX_C_SOURCE 200809L
#include <assert.h>
#include <inttypes.h>
#include <pthread.h>
#include <sched.h>
#include <signal.h>
#include <stdatomic.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <sys/time.h>
#include <time.h>

_Static_assert(ATOMIC_LONG_LOCK_FREE == 2, "signal counters must be lock-free");
static _Atomic unsigned long all_samples, worker_samples;
static _Atomic int ready, running;
static _Thread_local volatile sig_atomic_t worker_thread;
static double used_cpu;

static double seconds(clockid_t clock) {
    struct timespec ts;
    assert(clock_gettime(clock, &ts) == 0);
    return (double)ts.tv_sec + (double)ts.tv_nsec / 1e9;
}

static void sample(int signal) {
    (void)signal;
    atomic_fetch_add_explicit(&all_samples, 1, memory_order_relaxed);
    if (worker_thread)
        atomic_fetch_add_explicit(&worker_samples, 1, memory_order_relaxed);
}

static void *burn(void *unused) {
    (void)unused;
    // Touch TLS before the timer starts, not for the first time in its handler.
    worker_thread = 1;
    atomic_store(&ready, 1);
    while (!atomic_load(&running)) sched_yield();
    const double before = seconds(CLOCK_THREAD_CPUTIME_ID);
    const double until = seconds(CLOCK_MONOTONIC) + 1.5;
    volatile uint64_t x = 1;
    do {
        for (unsigned i = 0; i < 100000; ++i)
            x = x * UINT64_C(6364136223846793005) + 1;
    } while (seconds(CLOCK_MONOTONIC) < until);
    used_cpu = seconds(CLOCK_THREAD_CPUTIME_ID) - before;
    return NULL;
}

int main(int argc, char **argv) {
    const unsigned long hz = argc == 2 ? strtoul(argv[1], NULL, 10) : 100;
    assert(hz > 0 && hz <= 1000);
    worker_thread = 0;
    struct sigaction action = {0};
    action.sa_handler = sample;
    action.sa_flags = SA_RESTART;
    assert(sigemptyset(&action.sa_mask) == 0);
    assert(sigaction(SIGPROF, &action, NULL) == 0);
    pthread_t worker;
    assert(pthread_create(&worker, NULL, burn, NULL) == 0);
    while (!atomic_load(&ready)) sched_yield();
    const suseconds_t us = (suseconds_t)(1000000 / hz);
    struct itimerval timer = {{0, us}, {0, us}};
    const double before = seconds(CLOCK_PROCESS_CPUTIME_ID);
    assert(setitimer(ITIMER_PROF, &timer, NULL) == 0);
    atomic_store(&running, 1);
    assert(pthread_join(worker, NULL) == 0);
    const double process_cpu = seconds(CLOCK_PROCESS_CPUTIME_ID) - before;
    struct itimerval stopped = {{0, 0}, {0, 0}};
    assert(setitimer(ITIMER_PROF, &stopped, NULL) == 0);
    printf("{\"requested_hz\":%lu,\"all_samples\":%lu,\"worker_samples\":%lu,"
           "\"thread_cpu_s\":%.9f,\"process_cpu_s\":%.9f,\"worker_ratio\":%.6f,\"all_ratio\":%.6f}\n",
           hz, atomic_load(&all_samples), atomic_load(&worker_samples), used_cpu, process_cpu,
           (double)atomic_load(&worker_samples) / hz / used_cpu,
           (double)atomic_load(&all_samples) / hz / process_cpu);
    return 0;
}
