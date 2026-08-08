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

Connection pooling; HTTP/2 and HTTP/3; streaming request bodies; `first_byte`
and `between_bytes` timeouts on native (declared unsupported, not silently
unimplemented); two `getaddrinfo` slots instead of one; h1 upgrade and
WebSocket; hickory and DoH; Alt-Svc; middleware and `http-ng-tower`;
`http-ng-rmcp`.

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
