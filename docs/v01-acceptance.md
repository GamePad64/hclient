# v0.1 acceptance

The four claims from the design spec, and what proves each one. Every file
named here was checked to exist and to prove what the row says, at the commit
this document was written — the rows are not carried forward on trust.

| claim | proof |
|---|---|
| The runtime seam is real | `crates/http-ng/tests/two_runtimes.rs` — one generic function, `fetch_once<R>`, bounded only by `http_ng_rt::{TcpConnect, Timer, Blocking} + Clone`, drives a real HTTP/1.1 exchange over loopback TCP under both `Tokio` and `Smol`. No `#[cfg]` in the test code; the file's single conditional is the gate excluding it from wasm targets, where its native dev-dependencies do not build. Plus `crates/http-ng-native/tests/h1.rs`, which completes an exchange on a bare `futures` executor with no spawn and no timer |
| The delegation seam is real | `http-ng-wasi` on `wasi:http` 0.3, where there is no socket to own at all — the transport delegates the whole exchange to the host |
| The capability model degrades honestly | See below — the row the plan wrote for this is no longer true, and the truth is stronger |
| The `Transport` shape was guessed correctly | `crates/http-ng/examples/portable.rs` builds for native, `wasm32-wasip2` and `wasm32-unknown-unknown` with **zero** `cfg` of any form in the example, verified by a CI job that both builds all three and scans the file. It is a line-by-line port of `act/components/http-client`, a consumer written before this library existed |

## The capability model, stated accurately

The plan's evidence for this claim was that `streaming_request_body` differs
between Chrome and Firefox in one binary. That stopped being true, and the
reason it stopped is better evidence than the claim it replaced.

`http-ng-fetch` hardcodes `streaming_request_body = false` and
`full_duplex = false` for every browser. Chrome genuinely supports a streamed
request body through `duplex: "half"`, and `supports_duplex()` detects it
correctly — but nothing in the crate builds that `ReadableStream` yet, and
`convert::resolve_body` rejects every `RequestBody::Streaming`
unconditionally. Reporting `true` in Chrome, which the crate did until that
task was reopened, meant a caller who branched on
`capabilities().streaming_request_body` — the entire reason the registry
exists — would see `true`, expect a streamed send, and get
`ErrorKind::Unsupported` at `execute()`.

So the rule the model actually demonstrates is sharper than "capabilities
vary by environment":

> A capability describes **this transport's behaviour**, not the
> environment's features.

What proves it is that the same defect was caught three times, in three
independently written backends, each time before release:

- `http-ng-wasi`'s `full_duplex` and `request_trailers` (vertical 1)
- `http-ng-native`'s `timeouts.connect` (vertical 2)
- `http-ng-fetch`'s `streaming_request_body`, `full_duplex` and `redirects`
  (vertical 3)

And that a capability a backend cannot honour is a typed error rather than a
silent no-op: `build_rejects_timeouts_fetch_cannot_express`
(`crates/http-ng-fetch/tests/transport.rs`), and a `RedirectPolicy` against a
`RedirectSupport::Internal` backend refused at `build()` and again on the
merged per-request value (`crates/http-ng/tests/redirect.rs`,
`crates/http-ng/tests/wasm_default.rs`).

`RedirectSupport` carrying a distinct `Transparent` variant is the same fix
applied to the enum itself: without it, "the field was not filled in"
(`None`, what `Capabilities::none()` returns) could not be told apart from a
backend's substantive claim that redirects are impossible.

**What the two-browser matrix still proves**, stated as what is true rather
than what the plan hoped: 65 tests run against two independent engine
implementations of the Fetch, Promise and `setTimeout` surface this crate
depends on, and one genuinely browser-varying runtime probe survives —
`crates/http-ng-fetch/tests/caps.rs`'s
`supports_duplex_reflects_the_prototype_not_a_hardcoded_constant`, which
compares against whatever `Request.prototype.duplex` the running browser
actually has, then flips it to prove the answer is neither cached nor
constant. It passes on both engines because it is written against the live
browser rather than against an expected value.

## Deliberately not done in v0.1

Ten items were listed here. ~~Connection pooling; HTTP/2 and HTTP/3;
streaming request bodies; `first_byte` and `between_bytes` timeouts on native
(declared unsupported, not silently unimplemented); h1 upgrade and WebSocket;
hickory and DoH; Alt-Svc~~ — **seven are built**, an eighth was answered
rather than built, and **two are open**. The two are below, with what they
still need.

> Where the seven landed. Pooling, v0.2 W2 — `Native::new` pools by default
> and `without_pool()` restores the v0.1 behaviour. HTTP/2, v0.2 W3, behind
> `http-ng-native`'s `http2` feature; HTTP/3, v0.3, in its own crate
> `http-ng-h3`, which could not have been that feature because it is bounded
> on `R: UdpBind + Spawn` and `T: QuicTlsConnect`. Streaming request bodies,
> v0.2 W6 on native and v0.3 on h3, where they arrive with real full duplex.
> `first_byte` and `between_bytes`, v0.2 W4, declared and enforced in one
> commit and measured against three misbehaving servers. h1 upgrade and
> WebSocket, v0.3 W4, and in v0.4 a crate of its own — now
> `http-ng-tungstenite`, on its own trait pair rather than a method on
> `Transport`, so a backend that cannot do it is a compile error. hickory and
> DoH, as `http-ng-dns-hickory` and `http-ng-dns-doh`. Alt-Svc, v0.4 W1, as
> `http-ng-select`'s slow tier — the HTTPS record is the fast one, and the
> order between them is a rule rather than an accident.

**Middleware was answered rather than built, and `http-ng-tower` is the
answer.** It goes both ways: `TransportService` makes a `Transport` into a
`tower::Service` so `tower-http`'s stack applies, and `ServiceTransport`
makes any `tower::Service` into a `Transport`. Nothing native is planned,
and that is decision D6 rather than an omission — *"all the machinery was a
consequence of type erasure in middleware; remove the erasure from the
built-in stages and the machinery disappears entirely"*. The built-ins here
are stages on data, not layers, which is why the core carries no `BoxFuture`
alias and no proc macro. `docs/competitive-gaps.md` §D6.

**Two `getaddrinfo` calls for one name, and it is still two.** `lookup_ipv4`
and `lookup_ipv6` do not share a resolution attempt, while
`std::net::ToSocketAddrs` resolves both families in a single system call — so
a Happy Eyeballs consumer calling both, which the scheduler is required to
do, triggers two full dual-family lookups and throws half of each away.
Measured with a counter around `Blocking::run` rather than assumed, and
written where the code is (`crates/http-ng-dns-system/src/lib.rs`). Open, and
the only item on this list that is a plain inefficiency rather than a missing
capability.

**`http-ng-rmcp` does not exist**, and it is named in three design documents
as the first consumer of a streaming request body. That role has since been
filled by tests rather than by a crate, so what it would prove now is
narrower than what it was listed for.

No total-deadline timeout. `Timeouts` is `connect` / `first_byte` /
`between_bytes`, so a response that starts promptly and then dribbles just
under the `between_bytes` threshold runs unbounded. `wasi-fetch` had the same
three and no whole-request deadline, so the migration documented in
[`porting-wasi-fetch.md`](porting-wasi-fetch.md) loses nothing — but it is a
gap, not a design position.

> Closed in v0.2 W4, and not as a fourth `Timeouts` field: the bound lives
> in `Client`, because only the client knows where the operation begins and
> ends (its redirect loop is inside it), and it is set together with the
> clock that measures it — `ClientBuilder::total_timeout(clock, d)`, or
> `Client::total_timeout(d)` on a client already carrying the target's
> default clock. Expiry is `ErrorKind::Timeout(Phase::Total)` and drops the
> exchange. The dribbling response above is the test that pins it
> (`crates/http-ng/tests/deadline.rs`); the one case it does not cover — a
> body that falls completely silent after the head — is `between_bytes`,
> still `false` on native. See `docs/v02-design.md` §W4.

## What remains unverified

`RequestBody::Streaming` does not pass through any transport: native buffers
it, fetch rejects it via `Capabilities`, wasi accepts only `Full`. The replay
contract is covered by unit tests and by no end-to-end scenario. The first
real consumer is `http-ng-rmcp`, in v0.2.

> Closed, and by tests rather than by the consumer this paragraph was
> waiting for. Native streams since v0.2 W6 and `http-ng-h3` since v0.3,
> where the request stream is split and the body is written from a future
> polled *beside* `recv_response` — genuinely full duplex, and pinned
> **causally** rather than by a clock: in `crates/http-ng-h3/tests/streaming.rs`
> the caller's body has no second chunk until `execute` has returned a head,
> so a transport that read the head only after finishing the body cannot
> complete the exchange at any speed. Two defects came out of building it,
> both predating it: a cancelled upload used to poison the shared connection
> — `quinn::SendStream::drop` finishes rather than resets, so a request
> dropped mid-DATA-frame terminated *cleanly* carrying a truncated frame,
> which RFC 9114 §7.1 makes a **connection** error — and a
> `RequestBody::Rewindable` whose factory returned a `Streaming` sent nothing
> at all: no bytes, no error, a `200` for a body that never existed.
>
> None of the three backends this paragraph names still behaves as it says.
> `http-ng-wasi` declares `streaming_request_body = true` and hands a
> `Streaming` body straight to the host; native no longer buffers; and
> `http-ng-fetch`'s answer is behavioural rather than a flat refusal —
> whatwg/fetch#1470, so it depends on the browser. The backend that reports
> `false` today is `http-ng-urlsession`, which arrived after this list was
> written and says so in the error a `Streaming` body gets.

Cross-backend cancellation is asymmetric. Only `http-ng-fetch` cancels the
in-flight exchange when the future is dropped, and `Transport::execute`'s
documentation says nothing about drop-cancellation for any backend. A caller
who drops a future gets different behaviour per target, undocumented.

> Closed in v0.2 W1, and the asymmetry turned out to be in the
> documentation rather than in the behaviour: native and WASI cancel too —
> native because the future owns the socket, WASI because dropping the
> future cancels the Component Model subtask. What none of them had was a
> stated duty or a measurement. `Transport::execute` now requires
> cancellation, `Capabilities::cancel_on_drop` is how a backend that cannot
> says so, and each backend has a pair of tests whose observer is outside
> the client: `crates/http-ng-native/tests/cancel.rs`,
> `crates/http-ng-wasi/tests/live_roundtrip.rs`,
> `crates/http-ng-fetch/tests/transport.rs`.
