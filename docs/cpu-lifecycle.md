# CPU sampler lifecycle

## Confirmed stale samples

The static signal ring outlived individual profilers. Final collection drained it **before** symbolization, while the CPU timer stayed active until collection returned. Samples captured during that symbolization could remain queued after stop and appear in a second profiler's export.

A deterministic regression uses two real `Profiler` instances with separate receivers and services. A per-instance, test-only hook raises three real `SIGPROF` signals inside the first instance's final address resolution. Those queued records have its distinctive span link, the sampler thread's name, and symbolization frames. Before the fix, the second receiver exported that old link. After the fix, it exports the fresh span, thread name, and function but no old link. Signal injection makes this independent of timer cadence.

A second regression pauses a producer after it reserves a ring slot. The first sampler stops, the second starts, and a fresh sample queues behind that reserved slot. The old producer then publishes. Removing the generation filter makes the test fail even when startup clearing remains enabled. Clearing the queue alone cannot cover an in-flight producer.

## Narrow fix

Each start receives a unique nonzero generation. Zero means inactive. The handler reads the active generation once and carries it through unwinding and publication. A sampler only accepts records matching its generation. Startup discards already queued records to free capacity; generation filtering also rejects late old publications. Stop makes the active generation zero before disabling the timer.

Sampling frequency, encoded CPU values, the existing CPU accuracy bound, and test enablement are unchanged.

```sh
cargo test --all-features final_symbolization_samples_cannot_leak -- --nocapture
cargo test --all-features delayed_old_producer -- --nocapture
cargo test --workspace --all-features
```

## Separate Intel question

This confirms cross-instance contamination, not its numerical contribution to every earlier Intel accuracy failure. An earlier Rust failure recorded 0.79 seconds attributed to the burner, 1.37 seconds across all threads, and 1.503 seconds measured burner CPU. Stale records, concurrent activity, and signal delivery could not be separated from that log alone.

The [native C probe](benchmarks/timer-delivery.json) independently shows a timer/CPU-clock mismatch without Rust, unwinding, or profiler restarts. That observation is separate from the fixed ownership bug. It is not sufficient reason to calibrate values, redesign timers, or weaken an accuracy gate. Check CI on the exact generation-fix revision before drawing further conclusions about Intel attribution.
