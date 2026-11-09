# Changelog

## Unreleased

Initial experimental implementation. Not published to crates.io.

- In-process CPU sampling at 100 Hz on Linux and macOS, x86-64 and ARM64.
- Heap profiles through jemalloc `heap_v2` dumps or `alloc::Sampled<A>` over any global allocator.
- OpenTelemetry context observation, manual span guards, synchronous labels, and per-poll async labels.
- Standard OTLP Profiles development messages with request-wide dictionaries, per-sample links, CPU nanoseconds and counts, in-use snapshots, and allocation deltas.
- OTLP/HTTP export with TLS, gzip, standard environment configuration, bounded queues, retry handling, and partial-success reporting.
- Upstream protobuf decoding and `profcheck` conformance tests, a checkout demo exporting profiles and traces, and readback checks against Collector, sigiro, and Pyroscope.
- Reproducible short-soak measurements against Pyroscope Rust 2.1.5, real sigiro screenshots, and animated implementation explainers.

Known limits include native signal-unwinder reentrancy risks, no jemalloc heap span links, no gRPC exporter, evolving OTLP Profiles compatibility, and incomplete long-duration/high-cardinality memory characterization. See [setup](docs/setup.md), [receivers](docs/receivers.md), and [measurements](docs/benchmarks.md).
