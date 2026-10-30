# dji-common

Shared helpers every other crate may use. No dependencies.

| File | Purpose |
|------|---------|
| `src/lib.rs` | re-exports; `Hex` / `hex` byte formatting for logs and telemetry |
| `src/log.rs` | timestamped line logging: the `logln!` macro and a pluggable sink |

Rule: nothing here knows about DJI, USB or the web. If a helper needs a
protocol type, it belongs in `dji-protocol` instead.
