# v0.2 acceptance

What v0.2 claims, what proves each claim, and — at the same length, because
it is worth as much — what it deliberately does not do and what nobody has
checked. Written as the work lands rather than at the end, so the reasoning
is recorded while it is still the reasoning and not a reconstruction.

Every file named here was checked to exist and to prove what the row says at
the commit this document was written. Rows are not carried forward on trust.
Sections for W3 (HTTP/2), W5 (compression) and W7 (embassy) arrive when they
do.

| claim | proof |
|---|---|
| Dropping an in-flight request stops the exchange | `crates/http-ng-native/tests/cancel.rs`, `crates/http-ng-wasi/tests/live_roundtrip.rs`, `crates/http-ng-fetch/tests/transport.rs` — one per backend, and in every case the **observer is outside the client**: a server reporting the socket closed, a wasmtime guest that outlives its own drop, and the browser rejecting its own promise with `AbortError`, which our side cannot synthesise |
| Connections are reused | `crates/http-ng-native/tests/pool.rs` — a server counting *accepted* connections, not a counter we also wrote. Two requests, one accept with the pool on; two accepts with `PoolConfig::disabled()` |
| Two clients with different trust cannot share a socket | `crates/http-ng-tls-rustls/tests/config_id.rs` — fails a `TypeId`-shaped or per-call `config_id`, which is what a naive implementation would reach for |
| An operation as a whole can be bounded | `crates/http-ng/tests/deadline.rs` — a server that answers in milliseconds and then drips one byte every 20 ms for ever. The test cannot pass without the bound |
| Adding a bound does not change the client's type | `crates/http-ng/tests/deadline_client_type.rs` — `struct App { http: Client }` with `total_timeout` applied. The `: Client` annotations are the assertion; the `assert_eq!` beside them is what stops the file passing if `total_timeout` stored nothing |

## The rule this vertical kept applying

Three capabilities were added in v0.2 — `cancel_on_drop`, `connection_reuse`,
and (pending) the ALPN and socket-option facts — and the same question was
asked each time, because v0.1 got it wrong four separate times in the other
direction:

**A variant exists only if a caller decision turns on it.** `CancelSupport`
started as three values, splitting `Supported` by *who* performs the
cancellation — the transport tearing down its own socket versus asking an
ambient host. Nobody could name a caller decision that turned on the
difference, so it is two values and the distinction lives in a doc comment.
`ReuseSupport` was proposed as three by the design doc and shipped as two
for the same reason, with the condition for a third written down: it arrives
when a portable client-level pool setting exists that a host-pooled backend
would have to refuse, together with the `check_supported` branch that
refuses it. That is exactly how `RedirectSupport::Internal` earns its
variant — not because ownership differs, but because a setting exists that
it must reject.

**A default must never be stronger than the truth.** This is the M2 lesson
from `RedirectSupport::Transparent`, and it decided three separate questions
this vertical: `Capabilities::none()` reports `CancelSupport::None`, because
silence and "cannot cancel" mean the same thing to a caller; `TcpOptsSupport`
defaults to `NONE` rather than `ALL`, so a runtime that forgets to declare
what it applies under-claims instead of promising socket options it silently
drops; and `TlsConnect::reports_alpn()` defaults to `false`, so a TLS backend
that cannot report the negotiated protocol is not assumed to be able to —
the cost of that particular over-claim is a `PROTOCOL_ERROR` on every
request.

## Three things that were found rather than designed

**`Spawn` was never usable here, so the pool never had a choice.**
`http_ng_rt::Spawn<F>` requires `F: Send + 'static`, and the native IO is
deliberately not `Send` — `connect.rs`'s `FakeStream` holds an `Rc<()>` for
the sole purpose of proving it. So "a pool driven by a spawned background
task" does not compile on this seam at all; the design doc's framing of it
as a trade-off was wrong, and there was one option, not two. What exists
instead is a poll at checkout plus a second look while the request is still
ours.

**A hang in hyper's dispatcher that would otherwise have shipped.** Reading
it to implement retry turned up a window where a keep-alive connection
ending gracefully with a request already queued strands the callback inside
the `Connection` we hold. It is a typed error now, pinned by a poll-by-poll
unit test. Nothing in the plan predicted this; it came out of reading the
dependency rather than trusting it.

**hyper's HTTP/2 client cannot be used without a spawner, and not for
reasons of cost.** `Http2ClientConnExec` is a **sealed** trait
(`hyper-1.11.0/src/rt/bounds.rs:51`), so an executor that queues futures for
our own `poll_frame` to drive cannot be written at all. v0.2 therefore
drives the `h2` crate directly, exactly as it drives `http1::Connection`
today. The design doc's `http2 = ["hyper/http2"]` was not merely a worse
option; it was not an option.

## Deliberately not done in v0.2

Recorded, not hidden — and each with the reason, because a bare list invites
someone to "fix" an item whose absence is the decision.

- **No reaper for idle sockets.** The idle timeout is a filter applied at
  checkout, not a background task that closes what has gone stale. Without
  `Spawn` — see above — there is nobody to run one. A client that goes quiet
  for an hour holds its sockets until the next request or until `Drop`.
- **No pool shared between clients.** Each `Native` owns its own, which is
  why the TLS configuration identity in `PoolKey` is a constant within any
  one pool today. The field is not decoration: it must be in the key before
  a shared pool exists, because that is the moment its absence becomes a
  security defect rather than a redundancy.
- **A nanosecond race survives.** A server can close a connection between
  our checkout poll and our write. Every HTTP/1 pool has this, hyper and
  reqwest included; the retry is what makes it recoverable rather than
  visible.
- **`total` does not cut a body that goes completely silent after the head.**
  `Timer::sleep` is an RPITIT, so its future cannot be stored in a struct
  field, and boxing it would make *every* response body `!Send`. The body
  wrapper therefore checks elapsed time on each `poll_frame`, which catches
  a dribbling body on its next byte and never wakes for one that stops
  entirely. That is `between_bytes`' job, and `between_bytes` is still
  honestly declared `false`.
- **The concurrency limit bounds requests, not sockets.** `tower` releases
  its permit when the `call` future completes — at the response head — so a
  streaming body holds its connection outside the limit. The design doc
  claimed otherwise and has been corrected.
- **No per-request `total` override.** The client-level form has no invalid
  state to reject: the clock arrives with the bound, so "a total without a
  clock" is unrepresentable. A per-request setter would need either a
  runtime flag existing only to be refused, or negative reasoning coherence
  does not offer. A per-handle bound costs one `Arc` bump and covers most of
  the same ground.
- **`MockTransport` still reports `Capabilities::none()`**, so
  `cancel_on_drop: None`. That is honest rather than lazy: its `execute`
  completes synchronously and there is nothing to cancel.
- **A `tower::buffer::Buffer` in the stack breaks the cancellation
  contract**, because it spawns a worker and the request outlives the
  dropped future. Such a stack must declare `None` even when the transport
  underneath can cancel. Written down in `http-ng-tower`.

## What remains unverified

- **fetch and WASI declare `ReuseSupport::Supported` with no external
  observer.** Browsers and `wasi:http` hosts do keep connections alive, and
  declaring `None` would be a lie in the other direction — but from inside
  the sandbox we cannot watch sockets, and CI does not check it. This is the
  one declaration in the set that no test stands behind, and it says so
  where the value is set.
- **Cancellation on a naive embassy backend does not work**, measured: the
  server sees nothing for two seconds, because `TcpSocket::drop` removes the
  socket from smoltcp before the queued FIN can become a packet. W7 must
  either declare `CancelSupport::None` or own a closing list the stack task
  drains. Recorded in `docs/w7-embassy-research.md`.
- **`http-ng-tls-native-tls` sends a proposed ALPN and cannot report the
  selected one.** Harmless while `http/1.1` is the only proposal; the moment
  h2 is proposed it becomes a `PROTOCOL_ERROR` per request. `reports_alpn()`
  is the fix and it is in W3's scope, not yet landed.
- **Two things about `http-ng-idn` that no test reaches.** The gate that
  refuses to trust an ICU until it has answered `straße.de` correctly has
  covered *content* and uncovered *presence*: deleting the filter leaves
  every test green, because killing that mutation needs a machine where ICU
  is present but wrong, and no runner is one. And on Windows the absence of
  `icuuc.dll` is a `STATUS_DLL_NOT_FOUND` at process start with no mention
  of IDN at all — the 1703 floor the crate's docs state is checked by
  nothing, at build time or at run time. Both are written where the code
  is, not only here.
- **The platform IDN question is open on one measurement, not two.**
  macOS is settled from Apple's own source: `swift-foundation`'s
  `UIDNAHookICU` calls `uidna_openUTS46` with `0x3C`, i.e. non-transitional,
  which agrees with `idna` — so the *flavour* question is answered and only
  the live confirmation is missing. Chrome was measured directly and agrees
  too. What still needs a runner is whether `CoInitializeEx` must precede
  `uidna_openUTS46` on Windows. All of it is in
  `docs/icu-ecosystem-survey.md`, including the one divergence no
  pre-filter fixes: browsers accept invalid punycode (`xn--a.de`) where
  `idna` refuses.
