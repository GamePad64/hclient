# OpenTelemetry for `hclient`: a transport decorator, and why not hooks

**Status: design. Nothing here is built.**

The want is ordinary: a span per request, `traceparent` and `baggage`
injected into the outgoing headers, and the result reaching whatever
collector the application already runs. What makes it worth writing down
is that the obvious home for it — the `Hooks` seam — cannot do the job,
and the reason is structural rather than a missing feature.

## 1. `Hooks` is the wrong seam, and it is not close

```rust
fn on(&self, event: Event<'_>);
```

An immutable event, `&self`, no return value. **Nothing reachable from a
hook can put a header into an outgoing request.** That is not an
oversight to be patched: the seam exists so that a backend can announce
what happened without a hook being able to change what happens, and
`AGENTS.md` records the care taken to keep the bounds off every signature
a hook-less build meets. Widening it to permit mutation would make every
backend's emission sites a place where a caller's code can rewrite a
request mid-flight.

Two smaller facts point the same way, both measured in this tree:

- **There is no request-start event.** `Event` has exactly five variants —
  `Connected`, `Reused`, `Head`, `Closed`, `Informational` — which is the
  life of a *connection* plus the arrival of a response head. A span needs
  a beginning, and there is nothing to hang one on.
- **`Hooks` is not universal.** `fn hooks` is declared on four backends:
  `Native`, `H3`, `Fetch`, `WasiHttp`. It is absent from
  `hclient-urlsession`, `hclient-winhttp`, `hclient-tower` and
  `hclient-mock`, and `ClientBuilder` has no hooks at all — the single
  occurrence of the word in `client.rs` is a sentence in a comment.

The first fact alone settles it. The other two say that even a mutating
hook would have reached two thirds of the backends.

## 2. The shape that works: a transport decorator

```rust
fn execute(
    &self,
    req: http::Request<RequestBody>,
) -> impl Future<Output = Result<http::Response<Self::Body>, Self::Error>>;
```

The request arrives **by value**, so a decorator may edit its headers
freely, and `Self::Body` is an associated type, so a decorator may wrap
the response body. Both halves of what tracing needs are already in the
seam.

```rust
let client = Client::builder(Instrumented::new(native)).build()?;
```

One line at the call site, and **nothing leaks downward**, because
`hclient::Client` names no type parameters. That is the erasure paying for
itself a second time, after `hc --backend` — see `AGENTS.md`.

**It is expressible today, and that is worth saying out loud.**
`hclient-tower` carries `TransportService` and `ServiceTransport`, so
`tower-http`'s `TraceLayer` can already be wrapped around this client:

```rust
Client::builder(ServiceTransport::new(layer.layer(TransportService::new(native))))
```

What that route does not give is connection-level events, and what it
costs is exactly the generic ceremony this workspace removed from
`Client`. A dedicated crate is the ergonomic answer, not the only one.

## 3. Where the context comes from — three answers, and the third is a rule

1. **Ambient.** `opentelemetry::Context::current()`, read at the top of
   `execute`. This works, and it works for a reason rather than by luck:
   this client does not spawn the request future, so `execute` is polled
   on the caller's own task. Zero API surface.
2. **Explicit, per request.** `RequestBuilder::extension(OtelContext(cx))`,
   for a caller who has themselves crossed a task or channel boundary and
   whose ambient context is now somebody else's.
3. **The rule:** the extension is read first, the ambient context is the
   fallback. Said once, at the setter.

An extension rather than a `Client` setting, for the reason
`AllowEarlyData` and `ClientIdentity` are extensions: a trace context is a
property of *a request*, not of the client that sends it.

## 4. The span's life

Start before delegating. **End at the end of the body, not at the head** —
otherwise `http.client.request.duration` is wrong for every streaming
response, which is most of the responses anyone instruments a client to
watch. So `type Body = SpanBody<T::Body>`.

The rule to write down: **the span closes on whichever comes first, the
end of the body or `Drop`**, and `Drop` is the backstop rather than the
path. A caller who abandons a body must not leave a span open. That pair
needs two tests, neither covering the other — the precedent is
`hclient-fetch`, where cancellation is pinned by exactly such a pair
because a pump watching only the channel passes one and fails the other.

## 5. Two things the decorator cannot know

Both are honest absences, and this workspace's rule is that an attribute
whose value would be a guess is **omitted**, not guessed.

**`http.request.resend_count` is not computable from below.** In
`Client::run` the outer loop is hops — redirects and authentication legs —
and the inner loop is retries of one hop. Both arrive at the transport
identically: a fresh `execute` with a cloned request. Hop 2 of a redirect
chain and attempt 2 of a retry are indistinguishable from where the
decorator stands.

*Closable cheaply:* `Client::run` puts `Attempt { hop: u8, resend: u8 }`
into the request's extensions. Two lines, readable by anything below, and
it fits the existing habit — `client.rs` already inserts and removes
extensions per hop.

**Connection attributes cannot be attached to a span.**
`network.peer.address`, the TLS version, whether the connection was
reused: all of it exists only in `Hooks`, and `Head` carries a
`ConnectionId` rather than a request identity. On a multiplexed h2
connection that id is not the address of *this* request. So even on
`Native` there is nothing a decorator can correlate.

*Closable two ways:* a request identity in the events, or hooks at the
`Client` level. The second is larger and probably the more valuable —
`Client` is currently not observable in any portable way at all.

Neither is required for a first version. A first version works without
them and **under-reports**, which is the direction this workspace
consistently chooses.

## 6. Baggage crosses an origin, and we cannot stop it here

Baggage lives in the same `Context`, so one propagator covers both it and
`traceparent`. But baggage is caller-invented key/value pairs, and tenant
identifiers routinely end up in it.

`Client` knows how to strip `Authorization` and `AllowEarlyData` on a
cross-origin hop, because it has `Follow::strip_sensitive` and knows where
the chain started. **A decorator does not**: it sees each hop as a
separate `execute` and has no memory of the first origin. So a redirect to
a third party carries the trace context and the baggage there.

The proposal: inject everywhere by default, as every SDK does; offer
`propagate_when(fn(&Uri) -> bool)` for an allow-list; and say the redirect
consequence plainly at the setter. The correct fix is a layer above, and
naming it as absent is better than implying it exists.

## 7. Where the telemetry goes

**The crate does not own a pipeline.** Spans go to
`opentelemetry::global::tracer()` or to a `Tracer` handed in; the SDK and
the OTLP exporter are the application's to configure. A library that
decides where a process's telemetry is shipped has drawn the boundary in
the wrong place.

Two fronts, and the second is nearly free:

- feature `tracing` — emit `tracing` spans, which is what applications
  actually have, and anyone with `tracing-opentelemetry` gets OTel for
  nothing;
- feature `otel` — a `Tracer` directly.

`opentelemetry` depends on `tracing` already, so the second front adds no
graph.

**And a bootstrap loop that must be named.** If the OTLP exporter makes
its requests through an instrumented `Client`, exporting produces spans
which produce exports. The rule is already written in this workspace for
DNS — *a resolver's client is not the user's client* — and it applies
verbatim: the exporter's client must be a **plain** `Client`. That belongs
at the constructor, not in a README.

## 8. Cost, measured

Measured in a scratch crate outside this workspace:

| | crates |
|---|---|
| `opentelemetry` alone | **13** |
| `tracing` alone | 10 |
| `opentelemetry` + `-http` + `tracing` + `tracing-opentelemetry` | 30 |

`opentelemetry`'s graph is `futures-core`, `futures-sink`,
`pin-project-lite`, `thiserror`, `tracing`/`tracing-core` and a proc-macro.
**No build script, no `-sys` crate, no C**, and it type-checks for
`wasm32-unknown-unknown`. For this workspace that is a cheap guest.

`opentelemetry-http` is not worth taking: what it offers here is a
`HeaderInjector` over `HeaderMap`, which is fifteen obvious lines. This
workspace's rule for taking a dependency is whether a wrong answer would
be *silent* — which is why `base64` and the hashes were taken and the
percent-encoder was not — and a header injector fails loudly.

A separate crate, `hclient-otel`, by the local test for a crate boundary:
it holds a dependency that a feature would otherwise spread into every
graph in the workspace.

## 9. What a first version leaves out

- **Metrics** (`http.client.request.duration` and friends) — the same
  data, a separate surface. After spans.
- **Enrichment from `Hooks`** — wrong until a request identity exists in
  the events; see §5.
- **`resend_count`** — absent until `Attempt` is in the extensions.

A first version is: the decorator, injection from the context with an
extension override, a span from `execute` to the end of the body, request
and response attributes, `propagate_when`, and the rule about the
exporter's client.
