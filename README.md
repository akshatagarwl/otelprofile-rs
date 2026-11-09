# otelprofile

Continuous profiling for Rust that pushes standard **OTLP Profiles**. One call
starts it, and any OTLP profiles receiver can store the result. It is the Rust
sibling of [otelprofile-go](https://github.com/akshatagarwl/otelprofile-go).

> **Status: under construction, experimental.** Nothing is published yet. The
> OTLP Profiles signal is still in development (`/v1development/profiles`), so
> the wire format and this API will change.

The plan is in [issue #1](https://github.com/akshatagarwl/otelprofile-rs/issues/1).

## License

Apache License 2.0; see `LICENSE`.
