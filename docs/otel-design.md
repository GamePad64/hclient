# OpenTelemetry for `hclient`: a transport decorator, and why not hooks

**Status: built, and corrected against the building.** `crates/hclient-otel`
implements §11's steps 1-5. Six claims below were wrong and are struck
through and answered in place — each marked **[corrected]** — because a
design document that survives its implementation unamended is the shape
this workspace records failing three times over about checks: the claim is
exactly as perishable as the thing it describes. §12's list of what a first
version leaves out still holds, and metrics are still not built.

The want is ordinary: a span per request, `traceparent` and `baggage`
injected into the outgoing headers, and the result reaching whatever
collector the application already runs. What makes it worth writing down
is that the obvious home for it — the `Hooks` seam — cannot do the job,
and the reason is structural rather than a missing feature.

## 1. `Hooks` is the wrong seam, and it is not close

```rust
fn on(&self, event: &Event<'_>);
```

An immutable event — by reference since hook composition landed, which
changed nothing here — `&self`, no return value. **Nothing reachable from a
hook can put a header into an outgoing request.** That is not an
oversight to be patched: the seam exists so that a backend can announce
what happened without a hook being able to change what happens, and
`AGENTS.md` records the care taken to keep the bounds off every signature
a hook-less build meets. Widening it to permit mutation would make every
backend's emission sites a place where a caller's code can rewrite a
request mid-flight.

Two smaller facts point the same way, both measured in this tree:

- **There is no request-start event.** `Event` has six variants —
  `Connected`, `Reused`, `Head`, `Closed`, `Informational`, `Progress` —
  which is the life of a *connection*, the arrival of a response head, and
  octets moving. `Progress` arrived after this document was written and
  does not change the conclusion: it is emitted from inside an exchange
  that has already begun, so it is no more a beginning than `Head` is. A span needs
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

## 5. What the decorator can and cannot know — one absence closed

**`http.request.resend_count` is computable now, and the obvious mapping
is wrong.** When this document was written a decorator saw one `execute`
per hop and could not tell hop 2 of a redirect chain from attempt 2 of a
retry. `hclient_core::unversioned::Attempt { id, hop, resend }` now travels
in the request's extensions, minted once per operation in `Client::run` and
updated at all three send sites — the retry loop, the `425` replay and the
authentication leg.

So the decorator reads the value rather than computing it. But **it is
`hop + resend`, not `resend`**, and that is measured rather than guessed.
The registry's own words:

> The ordinal number of request resending attempt (for any reason,
> including redirects).
>
> The resend count SHOULD be updated each time an HTTP request gets resent
> by the client, regardless of what was the cause of the resending (e.g.
> redirection, authorization failure, 503 Server Unavailable, network
> issues, or any other).

Our two counters split the same total on a line OTel does not draw:
`hop` counts redirects and `resend` counts everything else about one hop.
Reading `resend` alone — the mapping the field names invite — reports `0`
for the third hop of a redirect chain, which is exactly the case the
attribute exists for. Both are kept as span fields of our own beside the
standard one, because the split is real information that the sum destroys:
*third send, first hop* and *first send, third hop* are different failures.

**Connection attributes are still not the decorator's, and are now
joinable.** `network.peer.address` and `network.peer.port` live in
`Connected`, which is a `Hooks` event, and `Head` carries a `ConnectionId`
rather than a request identity — that was the second absence. Both events
now carry a `RequestId` as well, so a hook and a span can be joined on a
key. What that does **not** give is a decorator that fills the attributes
itself: it would have to be the hook too. Three options, in increasing
order of what they demand:

1. **Leave them unset.** They are `Recommended`, not required, and this
   workspace's rule is that an attribute whose value would be a guess is
   omitted. A first version does this.
2. **The caller installs a hook and joins.** Costs them the join and gives
   them everything, including the TLS facts `Connected` carries that no
   span field is defined for.
3. **The crate offers a hook that feeds the decorator.** Possible, and it
   is the shape to resist first: `fn hooks` exists on four backends of the
   six types that could carry it, so a decorator that installed one would
   work on some transports and silently not on others — a capability that
   varies by backend, which is what `Capabilities` exists to make visible
   rather than to hide.

**One attribute has a capability to ask.** `network.protocol.version` is
`Recommended`, and whether `Response::version()` means anything is exactly
what `Capabilities::version_reported` answers — `hclient-fetch` and
`hclient-wasi` report `false` because they neither select the version nor
learn it. So the decorator reads the capability and sets the attribute only
where it is true. That is the biconditional `Head::version` already
established one seam over, reused rather than reinvented.

## 5a. The attribute set, against what the seam actually gives

Fetched from the specification rather than recalled. Requirement levels are
OTel's; the right-hand column is ours.

| attribute | level | where it comes from here |
|---|---|---|
| `http.request.method` | Required | the request |
| `server.address`, `server.port` | Required | the URI, port defaulted by scheme |
| `url.full` | Required | the URI, **with userinfo redacted** — the spec requires it and a span is a place credentials travel to a collector. **[corrected]** Userinfo is half of it: the specification also names seven query-string keys whose values are credentials (`X-Amz-Signature`, `X-Amz-Credential`, `X-Amz-Security-Token`, `AWSAccessKeyId`, `Signature`, `sig`, `X-Goog-Signature`), and a presigned URL is the commoner case by far. The key is kept and the value replaced, so a span can still say *this was presigned* |
| `http.response.status_code` | Cond. Required | the response |
| `error.type` | Cond. Required | `ErrorKind`'s variant name, which is why that type is an enum. **[corrected]** That is one of the rule's two arms and this row had only it: where a response *did* arrive with an error status, the specification asks for the **status code as a string**. Without the second arm a span for a `500` carries an `Error` status and no `error.type` at all — and `error.type` is what an aggregation groups by, so the commonest error a client sees would be the one nothing could group. `Timeout` also carries its `Phase` (`Timeout.Connect`), five values, still low cardinality, and the fact a dashboard is built on |
| `http.request.method_original` | Cond. Required | ~~only when we normalise a method, which this client does not — so absent, and that is an answer~~ **[corrected]** Normalising is a **MUST**: an unknown method is reported as `_OTHER` with the original here. It is also what makes the paragraph below true — a caller who invents a method per request would otherwise put it in the span *name*, which is the cardinality blow-up the name rule exists to prevent. So the span name is one of ten values for ever |
| `network.protocol.name` | Cond. Required | absent: it is required only when the protocol is **not** HTTP |
| `http.request.resend_count` | Recommended | `Attempt`, as above |
| `network.protocol.version` | Recommended | `Response::version()`, gated on `version_reported` |
| `user_agent.original` | Recommended | the request headers |
| `network.peer.address`, `.port` | Recommended | **not set** — see §5 |

Span kind `CLIENT`. **Span name is `{method}`**, not `{method} {target}`:
the spec allows the target only when it is low-cardinality, and an HTTP
client has no route template — a URL path would put every distinct URL in
the name and blow up the backend's cardinality, which is the failure the
convention exists to prevent.

**Status is `Error` for 4xx as well as 5xx**, which is a client-span rule
and differs from the server side. It also differs from this crate's own
`error_for_status`, deliberately: that method exists because a `404` is a
normal answer for about half the requests ever made and the caller decides,
where a span records what the exchange *was*. Two different questions, and
the span is not the place to apply the caller's policy.

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
- feature `otel` — ~~a `Tracer` directly~~ **[corrected]** the global
  tracer *provider*, with the instrumentation **scope** as the argument. A
  `Tracer` taken by value is a type parameter, and a type parameter on
  `Instrumented<T>` is one every consumer naming the type has to carry —
  the ceremony erasing `Client` removed. Which provider is a thing the
  application sets, and it is setting one anyway.

~~`opentelemetry` depends on `tracing` already, so the second front adds
no graph.~~ **[corrected], twice over.** It depends on `tracing` only
through its default `internal-logs` feature, which this crate switches
off — so the two fronts are genuinely additive. And measured on the built
crate rather than standalone, the arithmetic inverts: `hclient-otel` is
**16** crates with `otel`, **18** with `tracing`, **19** with both,
against `hclient-core`'s own 13. `opentelemetry` adds exactly itself,
because `futures-core`, `futures-sink`, `pin-project-lite` and
`thiserror` are already in the client's graph; `tracing` adds itself plus
`tracing-core` and `once_cell`. **The front that propagates is the
smaller one**, which is why `otel` is the default feature and `tracing`
is not.

**A third correction the fronts forced, and it is the sharpest thing the
building found.** The two are *not* interchangeable: the `tracing` front
**cannot inject**, at any price. `traceparent` is a W3C trace-id and
span-id; a `tracing` span's identity is a `tracing::span::Id` handed out
by whichever subscriber is installed, meaningful in one process and
nowhere else. Injecting `opentelemetry::Context::current()` instead is the
tempting repair and is worse than the absence — under
`tracing-opentelemetry`, which is the whole reason anybody picks that
front, that context is *empty*, because the bridge keeps the OTel span in
the tracing span's extensions and never pushes it onto the `Context`
stack. The propagator would write nothing at all on a request that looked
instrumented. So the crate says so at the constructor. What would close it
is `tracing_opentelemetry::OpenTelemetrySpanExt`, and taking it means
choosing the caller's bridge crate and its version for them: a third
feature, not a change to these two.

**And the fronts compose at the *constructor*, not through the features.**
`Instrumented::tracing` and `Instrumented::otel`; a feature decides which
exists and never what a built decorator does. Cargo unifies features
across a graph, so the other arrangement lets a neighbour's build add a
second span per request to this one — which is worse than either front
alone. It is `Collected::text`'s rule one crate over: a call must not
change meaning with a feature.

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

**One thing the standalone measurement did not show, and a wasm build
does.** On `wasm32-unknown-unknown` `opentelemetry`'s clock is
`js_sys::Date::now()` — a target-conditional dependency — so
`hclient-otel` is **29** crates there against 19 on `wasm32-wasip2`, the
ten being `js-sys` and `wasm-bindgen`'s tree. Nothing new to a browser
build, which carries them for `hclient-fetch` already, and a fact worth
having written down before somebody counts them.

`opentelemetry-http` is not worth taking: what it offers here is a
`HeaderInjector` over `HeaderMap`, which is fifteen obvious lines. This
workspace's rule for taking a dependency is whether a wrong answer would
be *silent* — which is why `base64` and the hashes were taken and the
percent-encoder was not — and a header injector fails loudly.

A separate crate, `hclient-otel`, by the local test for a crate boundary:
it holds a dependency that a feature would otherwise spread into every
graph in the workspace.

## 9. The crate: what is in it, and what each type owes

```
hclient-otel/
  src/lib.rs        Instrumented<T>, the decorator and its Transport impls
  src/attrs.rs      the request/response attribute set, sans-io
  src/body.rs       SpanBody<B>, which ends the span
  src/context.rs    reading the context, and propagate_when
  src/span.rs       [corrected] a fifth file: the two fronts behind one
                    recorder, so that "end exactly once" is written once
```

**`Instrumented<T>`** wraps any `T: Transport` and implements `Transport`;
it implements `SendTransport` where `T` does, by the one-line
`Box::pin(self.execute(req))` every backend already writes — `Send` is
inferred at a concrete type and proved nowhere.

**`attrs.rs` is sans-io and holds every decision in §5a**, so the whole
attribute set is testable with no socket, no collector and no clock. That
is `hclient-proto`'s bargain applied one crate up, and it is what makes the
`url.full` redaction and the `{method}` span name pinnable by unit tests
rather than by reading a span in a fixture.

**`SpanBody<B>`** ends the span on whichever comes first, the end of the
body or `Drop`. §4 has the rule; what the crate owes is the **pair** of
tests, because a `SpanBody` that ended only on `poll_frame` returning
`None` passes the first and fails the second, and one that ended only on
`Drop` passes the second and reports every duration as the caller's
lifetime rather than the exchange's.

**[corrected] — the pair is right and the second half is not reachable by
a mutation of this crate, which is worth knowing before somebody trusts
it as one.** Both fronts close themselves when dropped:
`opentelemetry_sdk::trace::Span` has an `impl Drop` that ends and
exports, and dropping a `tracing::Span` fires the registry's `on_close`.
So emptying `Recorder`'s own `Drop` leaves the whole suite green —
measured. The impl is kept regardless, and on a measurement rather than
on caution: `opentelemetry::trace::Span` is a **trait with no `Drop`
requirement**, and the API crate carries no `impl Drop` on any span type
at all — the one that exists belongs to the SDK. A conforming provider
whose spans do not self-end is allowed, and this crate hands its spans to
whatever the application installed.

The other half **is** reachable, and only because of how the test is
written: the assertion that the span has closed is made **while the body
is still alive**. A body read to its end and then dropped looks the same
either way.

## 10. What a test can pin without a collector

The thing that makes this crate testable is that neither front needs a
pipeline. With the `tracing` feature, a `tracing_subscriber` layer that
records `(name, fields, close time)` into a `Vec` is thirty lines and gives
every assertion in §5a. With the `otel` feature, `opentelemetry_sdk`'s
in-memory exporter does the same.

Four claims are worth a mutation each, because each would be silent:

- **The span closes at the end of the body**, not at the head — mutate to
  close in `execute` and the recorded duration stops depending on how long
  the body took, which a fixture that delays its second frame detects.
- **`resend_count` is `hop + resend`** — mutate to `resend` and a
  three-hop redirect chain reports `0`. This is the mapping §5 says the
  field names invite and the specification forbids.
- **`url.full` is redacted** — mutate to the raw URI and a request to
  `https://u:p@host/` puts a password in a span.
- **The extension beats the ambient context** — mutate to prefer the
  ambient one and a caller who crossed a task boundary gets the wrong
  parent, which no assertion about *a* span being emitted would catch.

**And one property is not a test but a build:** `hclient-otel` must
compile for `wasm32-unknown-unknown` and `wasm32-wasip2`, because a client
whose whole claim is one API everywhere should not grow an instrumenter
that only exists on native. §8's measurement says it can — `opentelemetry`
has no build script and no `-sys` crate — and `just check-targets` is where
that belongs rather than in a paragraph.

## 11. Build order, and where to stop

1. `Instrumented<T>` with `attrs.rs` and the request-side attributes; span
   opened and closed in `execute`. Wrong duration on purpose, and it is
   the smallest thing that emits a span at all.
2. `SpanBody<B>` and the pair of tests. Now the duration is right.
3. Injection: ambient context, the extension override, `propagate_when`.
4. `resend_count`, `network.protocol.version` and the capability gate.
5. **Stop and check.** At this point the crate is useful and every
   attribute it sets is one it can defend.
6. Metrics, if wanted, as a separate surface — §12.

**Built: 1-5, and stopped at 5.**

## 12. What a first version leaves out

- **Metrics** (`http.client.request.duration` and friends) — the same
  data, a separate surface. After spans, and worth noting that the
  duration a metric wants is the one §4 fixes: to the end of the body.
- **Connection attributes**, `network.peer.address` and its port. §5 has
  the three ways and the reason the tempting one is worst.
- **A span per hop with a parent per operation.** OTel's model is one
  client span per *request*, and a redirect is a resend rather than a
  child — which is what `resend_count` counting redirects means. So the
  chain is flat by the specification's own choice, and grouping it under
  an operation span is a thing a *caller* may want and the convention does
  not describe. `RequestId` is on every hop, so a caller who wants it can
  build it; the crate does not decide for them.

**Two entries left this list because the work under them landed.**
`resend_count` was *absent until `Attempt` is in the extensions* and
`Attempt` is now in the extensions; enrichment from `Hooks` was *wrong
until a request identity exists in the events* and it now does. What
replaced them is narrower and truer: the identity makes both computable,
and only the second still needs a mechanism the crate has not got.

A first version is: the decorator, injection from the context with an
extension override, a span from `execute` to the end of the body, the
attribute set of §5a, `propagate_when`, and the rule about the exporter's
client.
