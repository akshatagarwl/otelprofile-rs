# Check profiles in a receiver

Run the checkout demo against a receiver, then read its profiles back. The check
requires CPU and heap frames from the demo, not just a successful HTTP response.

The 2026-11-09 readback summaries are saved in [receivers/](receivers).
The Collector command below completed in 49 seconds with a warm local Cargo cache
and an already-running Collector. Only newly appended requests were checked;
all six kinds and positive span links were present. This is not a cold-install
timing. See `quickstart-timing.json` and `quickstart-proof.json` in that directory.

## OpenTelemetry Collector 0.165.0

From the repository root, start a Collector with profiles enabled.

```sh
mkdir -p /tmp/rust-profiles-out
chmod 777 /tmp/rust-profiles-out
docker run --rm --name rust-profiles-collector \
  -p 127.0.0.1:14318:4318 \
  -v "$PWD/scripts/collector.yaml:/etc/otelcol-contrib/config.yaml:ro" \
  -v /tmp/rust-profiles-out:/out \
  otel/opentelemetry-collector-contrib:0.165.0 \
  --config=/etc/otelcol-contrib/config.yaml \
  --feature-gates=service.profilesSupport
```

In a second terminal, send the demo's traces and profiles.

```sh
OTEL_EXPORTER_OTLP_ENDPOINT=http://localhost:14318 \
  cargo run -p otelprofile-demo -- 35
python3 scripts/check-receiver.py collector /tmp/rust-profiles-out/profiles.json
```

The script prints all six profile types and a positive `linked_samples` count.
The Collector's file output uses its own dictionary layout. The crate's wire
format also passes upstream `profcheck` in CI.

## Grafana Pyroscope 2.4.3

```sh
docker run --rm --name rust-profiles-pyroscope \
  -p 127.0.0.1:14440:4040 grafana/pyroscope:2.4.3
```

Wait for `curl -f localhost:14440/ready` to succeed. This can take several minutes.
Pyroscope does not accept traces, so keep the Collector running for those.

```sh
OTEL_EXPORTER_OTLP_ENDPOINT=http://localhost:14440 \
OTEL_EXPORTER_OTLP_TRACES_ENDPOINT=http://localhost:14318/v1/traces \
  cargo run -p otelprofile-demo -- 35
python3 scripts/check-receiver.py pyroscope http://localhost:14440
```

Pyroscope returns CPU nanoseconds and all four heap flamegraphs with demo frames.
In the tested version, it lists the `samples` type but returns no count flamegraph
for that type. The check reports this limitation separately. The Collector and
sigiro return the count profile too. A profile export succeeding does not prove
that a receiver preserves every profile type.

## Sigiro

Use a build with OTLP Profiles v1.10 decoding. Sigiro commit `3c801fb` was tested.
The older `0.1.0` release binary rejects the current `Sample.link_index` wire type.
The tested scratch build uses its embedded dashboard, with no server changes.

```sh
sigiro serve --data-dir /tmp/rust-profiles-sigiro \
  --http-port 19998 --otlp-http-port 14338 --otlp-grpc-port 14337
```

```sh
OTEL_EXPORTER_OTLP_ENDPOINT=http://localhost:14338 \
  cargo run -p otelprofile-demo -- 45
python3 scripts/check-receiver.py sigiro http://localhost:19998
```

Sigiro flushes asynchronously. Retry the check after a few seconds if its tables
are still empty. The check requires all six types and sample links that resolve
to exported spans by both trace ID and span ID. Open
`http://localhost:19998/dashboard/profiles/` to inspect the flamegraphs.

## Jemalloc

Repeat the demo command with `--features otelprofile-demo/jemalloc`. The demo
installs `tikv-jemallocator` with profiling enabled and uses
`otelprofile::jemalloc_profiling!()`. Heap samples from jemalloc have no span links.
CPU samples still link to the active span.
