# Setup

The crate is experimental and Git-only. No crates.io release has been made.

```toml
[dependencies]
otelprofile = { git = "https://github.com/akshatagarwl/otelprofile-rs", features = ["opentelemetry"] }

[profile.release]
debug = 1
```

`start(Config::default())` reads the environment. Keep the returned `Profiler` alive for the application lifetime. Drop it or call `stop()` at shutdown. One profiler may run per process. Initialize a `log` subscriber to see backend, export, and dropped-sample warnings.

## CPU

CPU profiling emits both `cpu` nanoseconds and `samples` counts at a nominal 100 Hz. It uses `SIGPROF` and `ITIMER_PROF` inside the process. No eBPF, privileges, or external agent is required.

The interrupted thread is unwound in the signal handler. Addresses are symbolized off-thread. Unwind metadata and retained symbols improve the result; stripped binaries may contain address-only frames. Frame pointers can help native tooling but are not required by this unwinder.

Do not combine CPU sampling with another `SIGPROF` or `ITIMER_PROF` owner. The profiler rejects a pre-existing custom signal handler, leaves its own handler installed after stopping, and disables its timer. Restarting CPU profiling reuses that handler. Test signal interactions in your application.

**This is not a signal-safety guarantee.** The implementation warms the native unwinder and avoids unwinding known libunwind/libgcc regions. Native unwinding from an asynchronous signal still has reentrancy risks. Those regions appear as `[library]` frames. Unsupported native libraries and dynamic loading need application-specific validation.

### Known macOS timer accuracy limit

The macOS Intel GitHub runner currently fails the absolute CPU-time test. Three runs recorded roughly 0.56–0.60 times the burner's measured thread CPU. A standalone C probe with only a counting signal handler reproduced the undercount: 70 signals over 1.277 CPU seconds at requested 100 Hz, ratio 0.548. All signals reached the worker, so this result needs neither wrong-thread delivery nor our unwinder to occur. The ARM runner and local M4 also showed smaller native delivery deficits.

This isolates a mismatch between native timer delivery and native CPU-clock accounting below our unwinder. It does not identify which kernel/VM accounting mechanism is responsible or diagnose every Intel Mac. Nominal sample periods can understate actual CPU time when signals are missed or delayed. Absolute CPU-accuracy CI remains failing on that Intel runner; the acceptance bound was not weakened. [Probe records and exact CI revision](benchmarks/timer-delivery.json) are public. Do not treat these profiles as exact CPU accounting.

## Heap with any allocator

```rust
#[global_allocator]
static ALLOC: otelprofile::alloc::Sampled<std::alloc::System> =
    otelprofile::alloc::Sampled::new(std::alloc::System);
```

`Sampled<A>` forwards to any `GlobalAlloc`. The default mean sample interval is 512 KiB. `Sampled::with_rate(allocator, bytes)` changes it. Smaller intervals increase overhead and precision.

Each sampled allocation records its stack and active span. Samples are weighted by their sampling probability. `inuse_objects` and `inuse_space` estimate live memory. `alloc_objects` and `alloc_space` estimate allocations during each export window. These are statistical estimates, not allocator accounting totals.

The wrapper remains active even when no profiler is running. It keeps live allocations but not allocation windows in that state. Its live table has 65,536 slots and a bounded probe search. Overflow loses samples and is logged during collection. Distinct stack metadata is retained; high-cardinality workloads need longer memory tests than the included short soak.

## jemalloc

```toml
[dependencies]
otelprofile = { git = "https://github.com/akshatagarwl/otelprofile-rs", features = ["jemalloc", "opentelemetry"] }
tikv-jemallocator = { version = "0.7", features = ["profiling"] }
```

```rust
#[global_allocator]
static ALLOC: tikv_jemallocator::Jemalloc = tikv_jemallocator::Jemalloc;
otelprofile::jemalloc_profiling!();
```

The macro configures `prof:true,prof_active:true,prof_accum:true,lg_prof_sample:19` before allocator startup. No `MALLOC_CONF` is needed. The environment can override this configuration. When both backends are available, an installed `Sampled<A>` takes priority.

Every export interval, jemalloc writes a temporary `heap_v2` dump. The crate parses the already weighted `@` counters, symbolizes stacks, and removes that dump. In-use counters are snapshots; `alloc_*` is the difference between cumulative counters from successive dumps. The first dump establishes an allocation baseline. `prof_accum:true` retains lifetime allocation histories and can consume memory as distinct stacks accumulate. Heap dumps have **no span links** because jemalloc does not record them.

For CPU-only operation, `profile_types: vec![otelprofile::ProfileType::Cpu]` avoids the heap backend. If no heap backend is installed, the default configuration still produces CPU profiles and logs that heap profiling is unavailable.

## Spans and labels

Enable `opentelemetry` and use OpenTelemetry 0.33 contexts or `tracing-opentelemetry` 0.34. Start the profiler before entering spans. It registers an experimental global context observer. Another observer can prevent automatic linking.

Entering and leaving a context updates the sample link. Async spans should be instrumented per poll through `tracing`, not held as thread-local guards across `.await`. The crate does not export traces. Configure your existing trace SDK separately so the receiver can resolve the link.

For another tracer, `span::enter(SpanLink { trace_id, span_id })` creates a thread-local guard. The guard is neither `Send` nor `Sync`; it cannot move to another thread. Use it only within synchronous code on the current thread. Drop guards in reverse order. `tag_wrapper(&[("route", "/checkout")], || work())` adds sample labels; `tag_future` applies labels during each future poll. Labels are independent of resource `Config::tags`.

## Configuration

Explicit nonempty `Config` fields take precedence over the environment. Signal-specific OTLP variables take precedence over general ones.

| Config field | Environment | Default |
|---|---|---|
| `service_name` | `OTEL_SERVICE_NAME` | `unknown_service:<executable>` |
| `endpoint` | `OTEL_EXPORTER_OTLP_PROFILES_ENDPOINT`, then `OTEL_EXPORTER_OTLP_ENDPOINT` | `http://localhost:4318` |
| `headers` | `OTEL_EXPORTER_OTLP_PROFILES_HEADERS`, then `OTEL_EXPORTER_OTLP_HEADERS` | none |
| `tags` | `OTEL_RESOURCE_ATTRIBUTES` | host, process, OS, SDK attributes |
| `profile_types` | `OTEL_RUST_X_PROFILES_TYPES=cpu,heap` or `all` | CPU + heap |
| `interval` | `OTEL_RUST_X_PROFILES_INTERVAL=15s` | 15 seconds |

Additional environment settings:

- `OTEL_SDK_DISABLED=true` makes `start` return a no-op guard. It does not remove a global allocator wrapper.
- `OTEL_EXPORTER_OTLP_[PROFILES_]TIMEOUT` is milliseconds; default 10000.
- `OTEL_EXPORTER_OTLP_[PROFILES_]COMPRESSION` accepts `none` or `gzip`.
- Header and resource lists use comma-separated `key=value` pairs with percent encoding.

Transport is HTTP/protobuf, including HTTPS with Rustls. There is no gRPC exporter. A base endpoint gets `/v1development/profiles` appended. The profiles-specific endpoint accepts a full path. Standard `OTEL_EXPORTER_OTLP_PROTOCOL` is not consumed.

The exporter has a two-window queue, bounded requests, transient-error retries, and partial-success logging. A slow or unavailable receiver can cause windows to be dropped; profiling does not terminate the application. Shutdown joins the sampler, then waits at most five seconds for the final upload. This upload timeout is not a five-second bound on the whole shutdown, which also includes final collection and symbolization.

See [receiver checks](receivers.md) for compatible versions and [overhead measurements](benchmarks.md) before choosing a deployment configuration.
