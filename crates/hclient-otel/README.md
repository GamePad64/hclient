# hclient-otel

**A span per request, `traceparent` on the wire — as a `Transport`
decorator.**

Its own crate by this workspace's one test for a crate boundary: it holds
a dependency a feature would otherwise spread into every graph here.
Measured with `cargo tree -e normal` on this tree — `hclient-core` alone
is **13** crates, this crate is **16** with `otel`, **18** with `tracing`
and **19** with both. A feature of `hclient` would put `opentelemetry` in
the graph of every build in any tree that switched it on, which is the
same argument that keeps `hclient-tls-quic` out of `hclient-tls` and the
WebSocket framing out of the transport.

```rust
let client = hclient::Client::builder(Instrumented::otel(transport)).build()?;
```

One line, and **nothing leaks downward**: `hclient::Client` names no type
parameters, so every signature below it is unchanged.

## Why a decorator and not a hook

The `Hooks` seam is the obvious home and cannot do the job.
`fn on(&self, event: &Event<'_>)` takes an immutable event, `&self`, and
returns nothing — so **nothing reachable from a hook can put a header into
an outgoing request**, which is what that seam is for. There is no
request-start event either: `Event`'s variants are the life of a
*connection*, the arrival of a head and octets moving, and a span needs a
beginning.

`Transport::execute` already gives both halves away. The request arrives
**by value**, so a decorator may edit its headers, and `Self::Body` is an
associated type, so a decorator may wrap the response body — which is what
makes the duration the exchange's rather than the time to first byte.

## Two fronts, and the constructor picks one

`Instrumented::otel` records an OpenTelemetry span through the global
provider and injects. `Instrumented::tracing` records a `tracing` span,
which is what most applications already have, and with
`tracing-opentelemetry` installed it becomes an OTel span for nothing.

**A feature decides which constructors exist; it never decides what a
built decorator does.** Cargo unifies features across a graph, so the
other arrangement would let a neighbour's build add a second span per
request to this one.

**The `tracing` front emits and does not inject**, and that is structural:
a `tracing` span's identity is a `tracing::span::Id` handed out by
whatever subscriber is installed, so there is no W3C trace-id for a
propagator to write. Saying so is better than shipping a header that is
silently absent.

## What it sets, and what it will not

Every attribute the OpenTelemetry HTTP client span conventions ask for,
with three decisions worth knowing before you read a span:

- **`url.full` is redacted.** `https://REDACTED:REDACTED@host/`, and a
  presigned URL's signature becomes `X-Amz-Signature=REDACTED`. A span is
  a place credentials travel to a collector.
- **The span's name is the method alone.** A client has no route
  template, so a URL in the name is one distinct name per distinct URL.
  An unknown method becomes `_OTHER`, with the original in
  `http.request.method_original`, which is what keeps the name to ten
  values for ever.
- **`http.request.resend_count` is `hop + resend`.** The registry counts a
  redirect as a resend; `hclient_core::unversioned::Attempt` splits the
  same total on a line OTel does not draw. Reading `resend` alone is the
  mapping the field names invite and it reports nothing for the third hop
  of a redirect chain. Both halves travel beside it as `hclient.hop` and
  `hclient.resend`, because *third send, first hop* and *first send,
  third hop* are different failures.

`network.peer.address` and its port are **not set**: they live in a
`Hooks` event a decorator cannot read, and an attribute whose value would
be a guess is omitted. Both that event and this span carry a `RequestId`,
so a caller who wants them can install a hook and join on it.

## The pipeline is the application's

Spans go to `opentelemetry::global`'s provider or to a `tracing`
subscriber. The SDK, the sampler, the propagator and the exporter are
yours — a library that decides where a process's telemetry is shipped has
drawn the boundary in the wrong place. A process that installs no
propagator gets no `traceparent`, which is correct.

**The exporter's own client must be a plain `Client`.** If OTLP exports
through an instrumented one, exporting produces spans which produce
exports — the rule this workspace already writes for DNS, where *a
resolver's client is not the user's client*.

Part of [hclient](https://github.com/GamePad64/hclient) — an HTTP client
complete enough to build a new curl on, or a browser. See the repository
for the whole shape, `docs/otel-design.md` for the design and its
corrections, and `AGENTS.md` for why this piece is its own crate.

## Licence

MIT or Apache-2.0, at your option.
