// Paired experiment: does entering the kernel change native profiler delivery?
#define _DARWIN_C_SOURCE
#include <assert.h>
#include <inttypes.h>
#include <mach/mach.h>
#include <pthread.h>
#include <sched.h>
#include <signal.h>
#include <stdatomic.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/resource.h>
#include <sys/time.h>
#include <time.h>
#include <unistd.h>

_Static_assert(ATOMIC_LONG_LOCK_FREE == 2, "lock-free signal counters required");
static _Atomic unsigned long all_samples, worker_samples;
static _Atomic int ready, running;
static _Thread_local volatile sig_atomic_t worker_thread;
static double used_cpu, mach_cpu, ru_cpu;
static unsigned long loops;
static const char *mode;

static double seconds(clockid_t clock) {
    struct timespec ts;
    assert(clock_gettime(clock, &ts) == 0);
    return (double)ts.tv_sec + (double)ts.tv_nsec / 1e9;
}

static double mach_seconds(void) {
    thread_basic_info_data_t info;
    mach_msg_type_number_t count = THREAD_BASIC_INFO_COUNT;
    const mach_port_t port = mach_thread_self();
    assert(thread_info(port, THREAD_BASIC_INFO, (thread_info_t)&info, &count) == KERN_SUCCESS);
    assert(mach_port_deallocate(mach_task_self(), port) == KERN_SUCCESS);
    return info.user_time.seconds + info.system_time.seconds +
        (info.user_time.microseconds + info.system_time.microseconds) / 1e6;
}

static double rusage_seconds(void) {
    struct rusage ru;
    assert(getrusage(RUSAGE_SELF, &ru) == 0);
    return ru.ru_utime.tv_sec + ru.ru_stime.tv_sec +
        (ru.ru_utime.tv_usec + ru.ru_stime.tv_usec) / 1e6;
}

static void sample(int signal) {
    (void)signal;
    atomic_fetch_add_explicit(&all_samples, 1, memory_order_relaxed);
    if (worker_thread) atomic_fetch_add_explicit(&worker_samples, 1, memory_order_relaxed);
    if (strcmp(mode, "handler-syscall") == 0) {
        struct rusage ru;
        (void)getrusage(RUSAGE_SELF, &ru);
    }
}

static void *burn(void *unused) {
    (void)unused;
    worker_thread = 1;
    atomic_store(&ready, 1);
    while (!atomic_load(&running)) sched_yield();
    const double before = seconds(CLOCK_THREAD_CPUTIME_ID);
    const double mach_before = mach_seconds();
    const double ru_before = rusage_seconds();
    const double until = seconds(CLOCK_MONOTONIC) + 1.5;
    volatile uint64_t x = 1;
    do {
        for (unsigned i = 0; i < 1000; ++i) x = x * UINT64_C(6364136223846793005) + 1;
        if (strcmp(mode, "syscall") == 0) {
            struct rusage ru;
            assert(getrusage(RUSAGE_SELF, &ru) == 0);
        } else if (strcmp(mode, "mixed") == 0 && loops % 200 == 0) {
            struct timespec ts = {0, 1000000};
            nanosleep(&ts, NULL);
        }
        loops++;
    } while (seconds(CLOCK_MONOTONIC) < until);
    used_cpu = seconds(CLOCK_THREAD_CPUTIME_ID) - before;
    mach_cpu = mach_seconds() - mach_before;
    ru_cpu = rusage_seconds() - ru_before;
    return NULL;
}

int main(int argc, char **argv) {
    assert(argc == 3);
    mode = argv[1];
    const unsigned long hz = strtoul(argv[2], NULL, 10);
    assert(hz > 0 && hz <= 10000);
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
    assert(setitimer(ITIMER_PROF, &timer, NULL) == 0);
    atomic_store(&running, 1);
    assert(pthread_join(worker, NULL) == 0);
    struct itimerval stopped = {{0, 0}, {0, 0}};
    assert(setitimer(ITIMER_PROF, &stopped, NULL) == 0);
    printf("{\"mode\":\"%s\",\"requested_hz\":%lu,\"all_samples\":%lu,\"worker_samples\":%lu,"
           "\"thread_cpu_s\":%.9f,\"mach_cpu_s\":%.9f,\"rusage_cpu_s\":%.9f,\"ratio\":%.6f,\"loops\":%lu}\n",
           mode, hz, atomic_load(&all_samples), atomic_load(&worker_samples), used_cpu, mach_cpu, ru_cpu,
           (double)atomic_load(&worker_samples) / hz / used_cpu, loops);
}
