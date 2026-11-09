# Scorecard

Evidence is from 2026-11-09. This follows the Go sibling's row-and-evidence method, not its performance verdicts. Rust runtime profiling has different constraints. Nothing is published, tagged, or released.

| Row | Status | Evidence | Comparison with Pyroscope Rust 2.1.5 |
|---|---|---|---|
| One-call setup and environment configuration | Verified; cold quickstart unmeasured | `Config`/environment unit tests. [Collector demo](docs/receivers.md) completed in 49 seconds with warm Cargo caches and a running receiver; fresh appended requests passed readback. | Both provide a start/stop API. |
| CPU, count, and all four heap profile kinds | Verified | [Receiver summaries](docs/receivers), `tests/sampled.rs`, real jemalloc tests in `src/heap/jemalloc.rs` and `src/profiler.rs`. | Both have CPU and jemalloc backends. Pyroscope's cumulative-only heap stacks triggered a comparator panic; [setup difference disclosed](docs/benchmarks.md). |
| Any-allocator heap sampling | Verified on `System`; other allocators not exercised | `tests/sampled.rs` checks retained/freed bytes, allocation deltas, object counts, and span links through the actual exporter. | Pyroscope's tested heap backend is jemalloc-specific. |
| Labels and span-to-profile navigation | Verified | Context and label tests; compile-fail regression prevents moving a manual span guard between threads. [Real trace view](docs/assets/sigiro-trace.png) and [clicked span profile](docs/assets/sigiro-span.png). | Our CPU and `Sampled<A>` samples carry standard trace/span links. jemalloc cannot attach them. |
| Standard OTLP and request dictionaries | Verified | Upstream protobuf oracle, golden fixture, `profcheck` with duplicate/orphan/shape checks, and [three receiver readbacks](docs/receivers.md). The deliberately duplicated dictionary is rejected. | The comparator uses Pyroscope's native ingestion API. |
| Export errors, queue pressure, and shutdown | Covered by tests; not a universal shutdown bound | Mock HTTP tests cover gzip, headers, retries, permanent errors, partial success, and request limits. Final upload wait is bounded; final collection/symbolization is not. | Stop times are measured but have different upload-window semantics. |
| Native platform matrix | CI | Linux x86-64/ARM64 and macOS ARM64 passed before documentation work. The final workflow also runs macOS Intel. Check the [CI runs](https://github.com/akshatagarwl/otelprofile-rs/actions/workflows/ci.yml) for the exact revision. | No Linux-vs-macOS performance equivalence is claimed. |
| Accuracy | Unit evidence; no complete comparative path-share study | CPU burner attribution and sampled/jemalloc retained-allocation tests exercise ground truth. Real receiver profiles contain workload frames. | No Rust top-20-path A/B tolerance verdict is claimed. |
| Overhead and dump latency | Measured short soak; long-duration characterization open | [Five interleaved rounds, all raw records and caveats](docs/benchmarks.md). Separate five-dump experiment at 15-second intervals. | CPU costs are of the same order on this workload. No throughput winner or statistical significance claim. |
| Documentation on GitHub | Verified in Chrome | All seven README images loaded, SVG intrinsic widths are 960 px, and two captures prove the flow animation changes. [Saved result](docs/assets/github-readme-proof.json); rerun `npm run check-readme` under `docs/assets/src`. | Real receiver screenshots, not UI mockups. |
| Release preparation | Local checks pass; publication gated | fmt, clippy, MSRV 1.88, default/all-feature tests, profcheck, rustdoc, `cargo package`, and `cargo publish --dry-run`. | Publication, tags, and releases require explicit approval. |

## Limits that remain

- No long-duration, stack-diverse, multithreaded allocation soak or controlled Linux performance run.
- No cold-cache quickstart timing or comprehensive path-share accuracy A/B study.
- Native unwinding inside a signal handler is experimental, not an async-signal-safety guarantee.
- Pyroscope 2.4.3 lists the OTLP `samples` type but returned no count flamegraph in our check.
- Sigiro's older 0.1.0 binary cannot decode the current link representation; the separately built `3c801fb` receiver can.

See [setup](docs/setup.md) before enabling profiling in a service. “Verified” means the named check ran, not that every application or receiver is compatible.
