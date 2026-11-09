# Overhead measurements

A short soak, not a production capacity claim. Measurements below come from an Apple M4 on Darwin 27.0.0, release builds, 2026-11-09. [Raw records, receiver evidence, binary hashes, and compiler version](benchmarks/2026-11-09-m4).

## Method

Five rounds, eight cases per round, 20 seconds of work per case. Cases run sequentially and their order reverses on even rounds. Both profilers sample CPU at 100 Hz and export every 15 seconds. Heap sampling uses a 512 KiB mean interval. No build ran during the measured rounds. Local receivers and unrelated desktop processes shared the machine.

Each operation performs 20,000 black-boxed integer updates, formats and allocates a receipt, and keeps the most recent 512 receipts alive. Results include completed operations, wall time, process user+system CPU, process peak RSS, a checksum, and profiler shutdown time. CPU and wall counters cover the work interval, **excluding startup and final flush**. Peak RSS includes startup. The CPU metric includes profiler threads during the work interval. This is predominantly a CPU workload, not an allocator stress test.

The comparator is `pyroscope` Rust 2.1.5, using its in-process pprof CPU backend and jemalloc backend. It sends to Pyroscope 2.4.3. otelprofile sends standard OTLP to sigiro `3c801fb`. The backends have different encoding and receiver costs; this is not an isolated wire-encoding comparison.

**The heap cases do not perform identical work.** otelprofile retains jemalloc cumulative allocation counters and exports all four heap kinds. Pyroscope's jemalloc conversion panicked in the pilot with `prof_accum:true` (`pprof_util` 0.8.2, “no exabyte heap sizes”). Its heap case therefore uses `prof_accum:false`. It measures live heap without comparable cumulative allocation deltas. The `sampled-system` case wraps `System` with `Sampled<A>`; its `off` mode still incurs that wrapper's live-tracking cost.

All 40 runs completed with nonzero work and no matched error, panic, or dropped-window log lines. Receiver readback confirmed positive CPU data and workload frames for every profiled run. Every otelprofile heap run delivered all six CPU/heap types. Pyroscope heap runs delivered positive in-use byte graphs with workload frames. See `receiver-proof.json`; a successful process exit alone was not accepted as proof.

## Results

| Allocator | Mode | n | Ops/s median [min, max] | CPU µs/op median | Peak RSS MiB median | Stop ms median |
|---|---|---:|---:|---:|---:|---:|
| jemalloc | off | 5 | 19,470 [19,226, 19,558] | 51.113 | 9.44 | 0.13 |
| jemalloc | otel-all | 5 | 18,779 [12,713, 19,547] | 51.381 | 16.55 | 3.43 |
| jemalloc | otel-cpu | 5 | 19,412 [19,068, 19,538] | 51.206 | 15.48 | 2.48 |
| jemalloc | pyroscope-all | 5 | 19,307 [17,180, 19,567] | 51.258 | 41.08 | 4959.01 |
| jemalloc | pyroscope-cpu | 5 | 18,179 [16,557, 19,566] | 51.058 | 39.19 | 7059.16 |
| sampled-system | off | 5 | 19,396 [18,464, 19,426] | 51.465 | 12.86 | 0.12 |
| sampled-system | otel-all | 5 | 19,258 [16,100, 19,402] | 51.836 | 17.64 | 3.61 |
| system | off | 5 | 19,531 [18,975, 19,584] | 51.020 | 9.52 | 1.34 |

Throughput varies substantially, particularly in the early rounds. Process CPU per operation is much steadier. That is consistent with scheduling interference, but the host was not instrumented well enough to identify its source. These observations **do not establish a throughput winner**. The small CPU differences also do not establish statistical significance.

Pyroscope shutdown includes upload-window coordination and, for the heap case, stopping two agents concurrently. It is not a comparison of sampler stop latency alone. The RSS figures are high-water marks, not retained-memory or leak measurements. Longer runs with many distinct allocation stacks, more threads, allocator-heavy workloads, and a controlled Linux host remain necessary.

## jemalloc dump cost

A separate 75-second run retained 64 MiB at one allocation site while repeatedly allocating and freeing 16 KiB blocks. It called `prof.dump` every 15 seconds with cumulative counters enabled. Five 6,511-byte dumps took **0.324–0.542 ms**, median **0.473 ms**. See `dump-cost.json`.

This measures the synchronous native dump call only, not parsing, symbolization, encoding, or export. Few allocation sites produce a small dump. Do not extrapolate it to a large, stack-diverse service. A native dump has no hard timeout.

## Reproduce

Start the receivers as described in [receiver verification](receivers.md). Keep at least 10 GiB free. Build three separate executables so the global allocator cannot change between cases:

```sh
mkdir -p /tmp/otelprofile-soak-bin
cargo build --release -p otelprofile-soak
cp target/release/otelprofile-soak /tmp/otelprofile-soak-bin/soak-jemalloc
cargo build --release -p otelprofile-soak --no-default-features --features sampled
cp target/release/otelprofile-soak /tmp/otelprofile-soak-bin/soak-sampled
cargo build --release -p otelprofile-soak --no-default-features
cp target/release/otelprofile-soak /tmp/otelprofile-soak-bin/soak-system

for round in 1 2 3 4 5; do
  python3 scripts/soak.py --binaries /tmp/otelprofile-soak-bin \
    --output /tmp/otelprofile-soak-results --round "$round"
done
python3 scripts/check-soak.py /tmp/otelprofile-soak-results
python3 scripts/summarize-soak.py /tmp/otelprofile-soak-results

cargo build --release -p otelprofile-soak --example dump-cost
_RJEM_MALLOC_CONF=prof:true,prof_active:true,prof_accum:true,lg_prof_sample:19 \
  target/release/examples/dump-cost
```

The default receiver ports are sigiro HTTP `19998`, OTLP HTTP `14338`, and Pyroscope HTTP `14440`. The scripts accept overrides. The runner refuses to overwrite an existing case, uses one unique service name per process, and checks the disk floor before each case. Set `--seconds` to a longer duration for a sustained soak. Regenerate this table with `scripts/summarize-soak.py docs/benchmarks/2026-11-09-m4/results.json`.
