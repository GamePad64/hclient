# The verticals

> Part of the [AGENTS.md](../../AGENTS.md) record, split 2026-10-05. The root
> file keeps the operational rules and the index of parts. Sections below
> keep their original headings, and a "this file" in the text named the
> whole record when it was written.

### Vertical 2 (native): what's proven

**The runtime seam is real, not decorative.** The same generic code
(`fetch_once<R>` in `crates/hclient/tests/two_runtimes.rs`, bounded by
`hclient_rt::{TcpConnect, Timer, Blocking} + Clone`, with no `#[cfg]` anywhere
in the test code — the file's only conditional is the `#![cfg(not(target_family
= "wasm"))]` gate excluding it from wasm targets, where its native
dev-dependencies do not build) actually drives an HTTP/1.1 request over real TCP to a real
server on loopback — once under `hclient_rt_tokio::Tokio` inside a
`tokio::runtime::Runtime`, once under `hclient_rt_smol::Smol` on a bare
`futures_executor::block_on`. The property is confirmed by more than a green
run: adding `R::Instant: PartialEq<std::time::Instant>` to `fetch_once`'s
bound (the same mutation trick `hclient-rt-pair-check`'s `pair_property.rs`
already applied to runtime capabilities individually) breaks instantiation on
`Tokio` (`Instant = tokio::time::Instant`, a wrapper, `E0277: can't compare
tokio::time::Instant with std::time::Instant`) and does not break `Smol`
(`Instant = std::time::Instant` directly) — the test is sensitive to a
regression of the seam, not just to whether the file compiles at all.

**The HTTP/1 exchange runs without spawn and without a reactor where there
isn't one.** `hclient-native/tests/h1.rs`'s
`works_on_a_bare_futures_executor_with_no_spawn` checks this on IO with no
reactor at all (Task 12); `two_runtimes.rs` above checks the same property of
the transport (`Native`), now under real runtime backends, not just under the
test busy-spin.

**`DefaultTransport`/`Client::new()`** (the `Client<T = DefaultTransport>`
this line named for two verticals is gone — `Client` names no parameters) — the
`default-transport` feature, **not** in `hclient`'s `default`, as for every
crate in the vertical — it was moved in for one commit and back out, and
the section on features as a floor is why. On any non-wasm target it
resolves to
`Native<Tokio, Rustls, SystemDns<Tokio>>` with the system trust store
(`rustls-platform-verifier`, not `webpki-roots` — a client that "just works",
not one with explicitly chosen roots). On `wasm32-unknown-unknown` it resolves
to `hclient_fetch::Fetch`, and `Client::new()` there returns `Self` rather than
a `Result`, because fetch's constructor cannot fail. Without the feature, or on
`wasm32-wasip2` (`target_os = "wasi"`), the type doesn't exist at all — a
compile error, not a silently weaker transport, and since the
features-as-a-floor section in [Language and packaging](19-language-and-packaging.md)
one that names which of the two reasons it is; on wasip2/wasip1 there's deliberately no branch that reuses the
already-built `hclient_wasi::WasiHttp` through this mechanism — `hclient`
doesn't depend on `hclient-wasi` (an invariant recorded in
`hclient-wasi/Cargo.toml`), and adding that dependency here would mean a path
that no CI job in this repository builds (the `wasip2` job runs `hclient-wasi`
directly). The direct path on WASI remains `Client::builder(hclient_wasi::
WasiHttp::new())`, same as before this task. Resolution details are in the
`DefaultTransport` doc comment in `crates/hclient/src/lib.rs`.

**`TCP_NODELAY` is asked for when the runtime says it can apply it, and the
41 ms it saves is the head of every connection.** Measured from the server's
side of the wire, with every read stamped before TLS sees it: with Nagle on,
the client's `Finished` and its `GET` arrive **coalesced as one 137-byte
write, 41.6 ms late**; with `nodelay` they arrive separately at 0.25 ms.
Four independent confirmations that it is the *request* that waits — the gap
is inbound at the server, the byte counts show the coalescing (137 = 74 +
63), `TCP_NODELAY` on the *server* changes nothing, and plaintext never
stalls because there the request head is the connection's first write.

**The default did not change, and that is the decision.** `TcpOpts` is a
socket seam that cannot know its caller writes request/response, and in this
workspace a *set* option is a **refusal**: `nodelay: true` in the seam's
default would turn every connect on a backend that left `TcpConnect::TCP_SUPPORT`
at its understating `NONE` into an `Unsupported` error for an option nobody
asked for. So `Native::new` asks for exactly what the runtime declares —
`nodelay: <R as TcpConnect>::TCP_SUPPORT.nodelay` — which is `applies_ech`'s and
`reports_alpn`'s shape one seam over: a constant defaulted to the
understating value, read by the layer above to decide whether to *ask*.
Silence now costs a slow connection rather than a refused one. The cost is
that `tcp_opts` replaces the whole set, so a caller setting only `keepalive`
turns `nodelay` back off — pinned by a test.

It also made a latent defect visible, which is worth more than the
milliseconds: the Alt-Svc fixture answered once and closed
**without `Connection: close`**, which RFC 9112 §9.6 makes a MUST. The
client pooled a connection the peer had already closed and the next request
raced the FIN — `hclient-native`'s pooled-reuse window, recorded in `h1.rs`
as residual and still there. Nagle's 41 ms had been padding the gap; with it
gone the suite failed 7 runs in 12 under `-j16`. The fixture now announces
its close, and the library's race is one look narrower than it was — see
the pooled-reuse section in [Proxies](15-proxies.md) on the point in it
that was ours.

**Checked against gRPC as an external yardstick, and gRPC itself is out of
scope.** The goal is a client powerful enough that someone else could build
gRPC on it, so `grpc/doc/PROTOCOL-HTTP2.md` was used the way Autobahn was
used for WebSocket: 21 requirements, 15 tests, every claim read off what a
real `h2::server` decoded. **No library code changed** — the client already
did all of them, including `te: trailers` reaching the wire, a Trailers-Only
response (HEADERS with END_STREAM and no DATA) arriving as a complete
response, a message split across DATA frames arriving as the caller sent it,
the empty end-of-stream DATA frame, back-pressure in both directions, and
sixteen rounds of bidirectional streaming on one stream — which is the first
consumer-shaped exercise of the duplex h2 landed in v0.4.
`.notes/grpc-yardstick.md` is the row-by-row report.

**Three limitations, none new, two of them one — and all three closed on
request in v0.4.** By default there is **no multiplexing**: an h2
connection is checked out exclusively, so two concurrent calls cost two
connections and two handshakes. That is v0.2 W3's decision, and its reason
is still live for the *default* — without `Spawn` there is nobody to drive
a shared connection but the in-flight request futures, so a caller that
stopped polling would stall its neighbours. **Cancellation therefore closes
the connection rather than sending `RST_STREAM(CANCEL)`**: the pump's
`Drop` does queue the reset, but the `Connection` is dropped in the same
breath. And a `PING` on a pooled connection is answered by the next call
rather than promptly. The first costs a handshake per concurrent RPC; none
of the three costs a failed call.

**`Native::multiplexed()` closes all three, and the second and third cost
no code of their own** — which is what `.notes/grpc-yardstick.md` predicted
when it classified them as downstream of the first. It spawns the h2
connection's driver, so the connection outlives the stream (the queued
`RST_STREAM(CANCEL)` reaches the wire) and outlives the request (a `PING`
is answered while idle), and concurrent requests share it: eight
concurrent calls, **one** accept, eight streams open at once by the
server's own count.

**The bound sits on that constructor and nowhere else, which is the whole
of the design.** `hclient_rt::Spawn` declares zero bounds, so
`<R as Spawn<F>>::spawn` coerces to `fn(&R, F)` and lives in a field that
demands nothing of `R` — no signature a `Spawn`-less runtime meets
changes, and `two_runtimes.rs` still runs `Native` on a bare
`futures_executor::block_on`. A runtime with no `Spawn` gets `E0277` where
it wrote `multiplexed()`, and so does a hook holding an `Rc`, because the
driver carries `H` so that a shared connection's `Closed` has an emitter
at all — the collision `hclient-h3` met from the other side and could not
close.

**Three prices, and each is said where a caller meets them.** A spawner
nobody drives turns "sockets stay open" into "requests **hang**", which is
worse than the reaper's version of the same mistake and is cut only by
`Timeouts::first_byte`. Beyond the peer's `MAX_CONCURRENT_STREAMS`
requests **queue** — no second connection is opened, because
`SendRequest::poll_ready` is a liveness check and not a capacity one, so
the threshold would have to be ours and depends on a handshake cost that
is a network property rather than a loopback one. And `.hooks(..)` must
come **before** `.multiplexed()`: the spawner's type names the hook, so
the other order compiles and shares nothing.

Measured through the real transport in both arms, 480 requests at a
concurrency of 8: **480** TCP accepts and 480 TLS+h2 handshakes exclusive
against **60** shared, for ~3× the CPU. In steady state — a warm pool,
loopback, no handshake left to save — sharing costs *more* CPU and saves
the sockets, which is the honest shape of the trade.
`.notes/h2-multiplexing.md` §11.

Also worth knowing before reading `capabilities()`: with `http2` on,
`full_duplex` and `response_trailers` still report the HTTP/1.1 **floor**, so
a caller cannot ask the capability whether duplex and trailers will work.
The honest route is `RequireVersion(HTTP_2)` before the head and
`Response::version()` after it — the floor rule behaving as designed rather
than a contradiction.

**HTTP/2 is negotiated and spoken, not merely compiled in (v0.2 W3).** The
`http2` feature is off by default and, when on, changes nothing a caller can
observe except speed and `Response::version()`: `capabilities()` still report
the **floor** — the value that holds on the worst protocol the transport
might negotiate — because over-claiming `full_duplex` costs a caller a
deadlock rather than a degradation, and because Cargo unifies features across
a graph, so a library can never know whether some other crate turned h2 on.
`crates/hclient-native/tests/http2.rs` pins both halves: an `h2::server` on a
real socket answers the client (an HTTP/1.1 request would get nothing at all
from it) and `Response::version()` reads `HTTP_2`, while
`capabilities_report_the_floor_with_the_feature_on` asserts `full_duplex ==
false` with the feature compiled in.

Two things behind that are worth knowing before reading the code.
**`hyper/http2` is unusable here** — its executor bound
`Http2ClientConnExec` is a sealed trait and the executor is handed the h2
connection itself, so a crate with no `Spawn` cannot supply one; the `h2`
crate underneath it is used directly instead, its `Connection` polled by hand
exactly as hyper's HTTP/1 one already is (`src/http2.rs`'s module doc, and
the correction in `.notes/v02-design.md` §W3). And **h2 is offered only over a
TLS backend that can report the negotiated ALPN** — `TlsConnect::reports_alpn`,
defaulting to `false`, overridden to `true` by `hclient-tls-rustls`: a backend
that sends the ALPN list and cannot read the answer back (which is exactly
`hclient-tls-native-tls`) would otherwise leave the client speaking HTTP/1
into a connection the server had switched to HTTP/2.

An h2 connection is **checked out of the pool exclusively, one stream at a
time**: without `Spawn` there is nobody to drive a shared connection but the
in-flight request futures, so a caller that stopped polling would stall its
neighbours. W1's "cancelling one stream must not tear down the others" then
holds because there are no others — a property of that pool policy, not of
the h2 code, and written down in both places so that lifting the exclusivity
does not lose it silently.

**What's still unverified live and carries over into vertical 3** (a boundary
from the vertical's brief, not narrowed by this task): the `Capabilities`
runtime model for fetch with its Chrome/Safari difference; `SseStream`
reconnection; `act` acceptance.

**Deliberately not done in v0.1** (recorded, not hidden): connection pooling
(one connection per request — **since done in v0.2 W2**, and `Native::new`
pools by default; `Native::without_pool()` restores this v0.1 behaviour);
streaming request bodies (**since done — v0.2 W6 on `hclient-native`, v0.3
on `hclient-h3`, where they arrive with real full duplex**); `first_byte`/
`between_bytes` timeouts (declared unsupported via `Capabilities`, rather than
silently unimplemented — **since done in v0.2 W4**, declared and enforced in
one commit, and measured against servers that answer never, fall silent after
the head, and stall mid-body: `crates/hclient-native/tests/timeouts.rs`); a
single `getaddrinfo` call for both address families
instead of separate v4/v6 slots; h1 upgrade.

### Vertical 1 (WASI): what's proven

**Proven.** The `Transport` shape actually works against an ambient backend
with no socket of its own on the guest side — not in theory, but under a real
`wasmtime` host (`crates/hclient-wasi/tests/live_roundtrip.rs`). A setting the
transport doesn't support becomes a typed `UnsupportedCapability` error
already at `ClientBuilder::build()`, rather than being silently ignored; the
same holds one level down — the `wasi:http` host rejecting a request-option
value (timeout, method, scheme) also becomes an error rather than being
dropped, and this isn't only verified by hand during implementation — it's
held in place by static analysis in CI (the `no-discarded-wasi-setter-result`
ast-grep rule, with the corpus it was accepted against next to it in
`scripts/ast-grep/rule-tests`) on every push.

**`full_duplex` is declared `false` — and that's about the `hclient-wasi`
implementation, not about the shape of the seam.** The `wasi:http` 0.3
protocol itself supports duplex request bodies: body data can flow while the
host hasn't yet returned a response. The shipped `WasiHttp::execute` doesn't
give you that — `convert::race_send_with_body` waits for both `send` and the
full body write (except on an early `send` failure). Measured on a live
`wasmtime` host (host-specific behavior, not pinned down by `wasi:http`): the
response already existed on the server at t≈0.10s, but the caller saw it only
at t≈2.00s, once the body finished writing; for a body with no end, it would
never see it.

The limitation is lifted **inside `hclient-wasi`, without touching
`Transport`.** `Transport::execute` returns `http::Response<Self::Body>`, and
`Self::Body` is `hclient_wasi::Body`, a type from that same crate: the
unfinished write future is carried into it and polled further from
`poll_frame`, and a transfer failure becomes a terminal body error. The
branch's final review implemented this as a proof of concept — around forty
lines, one new `Inner` variant, the `Transport::execute` signature untouched —
and measured it on the same host and server: the branch as it stands hangs
until killed at 25s; the variant with the future in `Body` delivers
`RESPONSE_HEAD_RECEIVED status=200 OK` in 0.094s. The technique isn't new: the
same `convert::resolve_send` doc comment proposes exactly this for a
*different* discarded future (`transmitted`).

Deferred not because of the seam, but because of three real costs it would
have to pay: (1) the guard against undeclared trailers can't run before
`execute` returns — trailer names are only known once the body has ended, so
the guard moves into `Body` and becomes a terminal body error; (2)
`resolve_send`'s policy that "a response arriving on top of a failed body
write is not a success" moves from an `execute`-level error to a body-level
error, i.e. gets weaker; (3) a caller that never reads the response body never
finishes writing the request body either — that's inherent to duplex without
`spawn` and needs documenting. Vertical 2's work, entirely inside
`hclient-wasi`.

**`wasm32-wasip3` is checked too, by its own job.** The target finally
names the `wasi:http` 0.3 this crate already speaks, and the guest is the
same source: `just test-wasip3` runs the unit suite under `wasm32-wasip3`
and the whole live file with `HCLIENT_WASI_GUEST_TARGET=wasm32-wasip3`,
which asserts the artifact cargo built is the target asked for — so a job
that lost its variable cannot come back green on the `wasip2` guest. It
is nightly (tier 3) and needs **wasmtime 49 or later**: under 47.0.3 the
wasip3 guest traps on `out of bounds memory access` or hangs, measured on
the live suite, and the recipe refuses an older host by name.

**Wasmtime 49 also found a defect on `wasip2`, and the local runs hid
it for a day.** 49 moved the connection task's abort-on-drop handle into
the transmission future `request.new` hands back, and `execute` dropped
that future — so the host closed the connection under a body still being
read and the trailers behind the data never arrived. `Body` holds it now
until the stream ends. It "passed locally" because `find_wasmtime`
preferred `~/.cargo/bin` (47) over a `PATH` naming 49; it prefers `PATH`
now. One fact about 49 reads like a defect and is not: it strips
`transfer-encoding` from every response it hands a guest
(`DEFAULT_FORBIDDEN_HEADERS`).

The two invariants CI enforces and every exception to them:
[`docs/exceptions.md`](docs/exceptions.md).
