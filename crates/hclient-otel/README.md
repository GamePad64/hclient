# hclient-otel

OpenTelemetry and `tracing` instrumentation for `hclient`, as a transport
decorator.

```rust
let client = Client::builder(Instrumented::otel(transport)).build()?;
```

One span per request with the OTel HTTP client attributes set, and
`traceparent` and `baggage` on the wire. It wraps a `Transport` rather
than using the hooks seam, because a hook is handed an immutable event and
so cannot put a header into an outgoing request, which is the main thing
this needs to do.

Two fronts, chosen at the constructor rather than by a feature:
`Instrumented::otel` exports spans through `opentelemetry` and injects the
header; `Instrumented::tracing` emits `tracing` spans and cannot inject,
because a `tracing` span's id is not a W3C trace id and there is no value
to write. Choosing by feature would let an unrelated crate in the graph
change what your decorator does.

Its own crate because `opentelemetry` is 16 crates on native and 29 on
`wasm32-unknown-unknown`, and a feature of `hclient` would put that in
every graph that enabled it.

Part of [hclient](https://github.com/GamePad64/hclient), a cross-platform
HTTP client for native, browser and WASI targets.

## Licence

MIT or Apache-2.0, at your option.
