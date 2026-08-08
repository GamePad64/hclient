# HTTP/3 — research. No implementation.

Research only. Nothing in `crates/` changed. Every "it builds" below is a
command and its output; every "it cannot" is the compiler's own words;
everything that could not be checked here is marked **unverified** together
with what would settle it.

Spikes live in `spikes/` (untracked, outside `crates/`): `q1-quic`,
`q1-seam`, `q2-udp`, `q2-embassy`, `q5-graph`.

Environment: `rustc 1.97.1 (8bab26f4f 2026-07-14)`, x86-64 Linux 7.0.0.
Base commit `5f9d82f` (`origin/main`), i.e. after W1, W2 and W4. Crate
versions under test: `quinn 0.11.11`, `quinn-proto 0.11.16`,
`quinn-udp 0.5.15`, `h3 0.0.8`, `h3-quinn 0.0.10`, `rustls 0.23.43`.

---

## Summary

| question | answer | how |
|---|---|---|
| Q1 sealed? | **Nothing is sealed.** All four `quinn` runtime traits and all five `h3::quic` traits are implemented from outside their crates in one file that compiles. The h2 disaster does not repeat. | §1.1 |
| Q1 no-spawn? | **Yes, measured end to end.** A real HTTP/3 GET over real QUIC on loopback, driven by one `futures_executor::block_on(poll_fn(..))`, with a `quinn::Runtime` whose `spawn` only queues. `wall=2.9ms`. Not a busy-spin: `wall 608.9ms / cpu 10ms` against a server that answers after 600 ms. | §1.2, §1.3 |
| Q1 `Send` leaks | **Three, all quotable, none in the protocol layer.** `quinn::Runtime`, `AsyncTimer` and `AsyncUdpSocket` are each `Send + Sync + Debug + 'static`; `http_ng_rt`'s `Timer` and `TcpConnect` promise none of that. The h3 layer itself demands nothing. | §1.4 |
| **not asked, and it is the real blocker** | **An idle QUIC connection nobody polls dies.** Same gap, same config: unpolled → request 2 fails; driven → request 2 succeeds. Without `Spawn` (which does not compile on this seam) h3 cannot be pooled the way W2 pools h1. | §1.5 |
| Q2 the trait | Unconnected, batch, caller-owned buffers, per-datagram ECN + segment size. Signatures in §2.3. GSO/GRO/ECN are three separate capabilities, not one. | §2 |
| Q2 measured | GSO 64, GRO 64, ECN round-trips (`Ect0` in, `Some(Ect0)` out), `may_fragment=false` — one `sendmsg`, 3 datagrams, one `recvmmsg`. tokio and smol both expose the descriptor; embassy-net has none, quoted. | §2.1, §2.4 |
| Q3 the TLS seam | `TlsConnect` cannot carry QUIC, and it is not close. A **second trait**, rustls-only, is the only honest option. `native-tls` is out — not "reports less", out. | §3.1, §3.6 |
| **Q3, 0-RTT** | **Works, measured, and it breaks W3's reserved `TlsInfo::early_data_accepted`.** The acceptance answer is a *future* that resolved at `8.6ms`, after the response arrived at `8.5ms`. On rejection the request fails with `ZeroRttRejected` and must be replayed. | §3.2, §3.3 |
| Q3, replay policy | **`RetryKind` covers the wrong half.** It answers "can I resend this", which 0-RTT also needs; it does not answer "may an attacker resend this", which is method safety — a notion this codebase deliberately does not have (quoted). And early data fails in **three** places, not one: no key material, `ZeroRttRejected`, and HTTP `425 Too Early`. | §3.5 |
| Q4 discovery | **Alt-Svc is not the first cut, and need not be.** `http_ng_dns::SvcbEndpoint` already carries `alpn` and `port`, and real HTTPS/SVCB lookup already ships. That is a discovery path with a TTL owned by the resolver, not a cache we invent. | §4 |
| Q5 CI | **Not compile-only.** The whole h3 client + server ran on loopback in this environment in single-digit milliseconds with an `rcgen` cert. The uncertain part is GSO/GRO/ECN on a GitHub runner, which is a capability report, not a pass/fail. | §5 |

---

## 1. Q1 — the executor question, asked first

### 1.1 Nothing is sealed

The failure being guarded against: `hyper::rt::bounds::Http2ClientConnExec`
(`hyper-1.11.0/src/rt/bounds.rs:51`) has a private supertrait, so an
executor that queues futures for our own poll loop cannot be written at
any price, and v0.2's design doc proposed a feature that was never an
option.

`spikes/q1-quic/src/bin/unsealed.rs` writes, from outside those crates,
an impl of every trait between us and a driven HTTP/3 exchange:
`quinn::{Runtime, AsyncTimer, AsyncUdpSocket, UdpPoller}` and
`h3::quic::{Connection, OpenStreams, SendStream, RecvStream, BidiStream}`.

```
$ cargo run --bin unsealed
UNSEALED: quinn::{Runtime, AsyncTimer, AsyncUdpSocket, UdpPoller} and
          h3::quic::{Connection, OpenStreams, SendStream, RecvStream, BidiStream}
          all implemented from OUTSIDE their crates. Nothing is sealed.
```

Corroborated by grep, in both directions:

```
$ grep -rn "sealed\|Sealed\|private::" quinn-0.11.11/src/ h3-0.0.8/src/ h3-quinn-0.0.10/src/
(no matches)
$ grep -rn "sealed\|Sealed" quinn-proto-0.11.16/src/
quinn-proto-0.11.16/src/crypto.rs:203:    /// Method for opening a sealed message `data`
quinn-proto-0.11.16/src/token.rs:252,257,258,500   (local variables named `sealed_token`)
```

`quinn_proto::crypto::{Session, ClientConfig, ServerConfig}` — the
TLS-for-QUIC interface — are public and unsealed too (`crypto.rs:28`,
`:113`, `:124`). That matters for §3: a QUIC TLS seam of ours is
*allowed* to exist.

### 1.2 A QUIC connection driven from one `poll_fn`, no spawn

`quinn` does require a spawner: `Endpoint::new_with_abstract_socket`
(`endpoint.rs:153`) and `Connection::new` (`connection.rs:66`) each call
`runtime.spawn(Box::pin(..))`. But `Runtime` is ours to implement, so
`spawn` can mean "put it in a queue and let our own loop poll it" — the
same thing `h1::exchange` does with `hyper::client::conn::http1::
Connection` today.

`spikes/q1-quic/src/queue_rt.rs` is that runtime, 180 lines:
`spawn` pushes into an `Arc<Mutex<VecDeque<Pin<Box<dyn Future + Send>>>>>`,
timers are `async_io::Timer`, the socket is `async_io::Async<UdpSocket>`
plus `quinn_udp::UdpSocketState` — i.e. the same reactor
`http-ng-rt-smol` already uses, so "no spawn" does not secretly mean "no
reactor, therefore a busy-spin".

The client in `nospawn.rs` is one `futures_executor::block_on`, no tokio,
no task; the server is an ordinary quinn + h3 server on its own thread
with its own tokio runtime (only the client is under test).

```
$ cargo run --bin nospawn
udp caps on this host: gso_segments=64 gro_segments=64 may_fragment=false
quinn futures queued by Endpoint::new (never spawned): 1
quinn futures our loop is driving at the end: 2
total polls of quinn-queued futures: 155
status=200 OK body="hello over h3, no spawn"
wall=2.947613ms cpu=10ms
OK: HTTP/3 over QUIC on futures_executor::block_on, no spawn, no tokio on the client
```

Two numbers in there are the answer to the brief's question. **One**
future is queued by `Endpoint::new` (the endpoint driver) and a second
appears when the connection is created; **two** are still being driven
when the request finishes. So the shape is not "quinn wants a background
task as a convenience" — it is "quinn has exactly two long-lived drivers,
and somebody must poll them for as long as the connection exists". Who
that somebody is, is §1.5.

### 1.3 It is not a busy-spin

The measurement AGENTS.md records for Task 12, repeated: CPU time from
`/proc/self/stat` around a request the server answers after 600 ms.

```
$ cargo run --bin cpu_measure
server answers after 600ms: status=200 OK body="slow" wall=608.910435ms cpu=10ms polls=1067
```

`wall 608.9ms, cpu 10ms` — one scheduler tick, i.e. the loop actually
parks. (`http_ng_native::testing::blocking_io`, the honest busy-spin, is
`wall 600.4ms / cpu 600ms` on the same measurement.)

### 1.4 The three bounds that do leak

Not into the protocol layer — `h3`'s five traits declare no `Send`
anywhere, and its `client::builder().build(quic)` requires only
`C: quic::Connection<B>`, no spawner, no `'static`, no `Send`. The leak
is entirely at `quinn`'s runtime seam, and `spikes/q1-seam` isolates each
one behind its own cargo feature so its error is quotable alone.

**(a) The runtime value.** `impl<R: Timer + Debug> quinn::Runtime for OurRt<R>`:

```
$ cargo check --features t_runtime
error[E0277]: `R` cannot be shared between threads safely
  --> src/lib.rs:34:47
   |
34 |     impl<R: Timer + Debug> quinn::Runtime for OurRt<R> {
   |                                               ^^^^^^^^ `R` cannot be shared between threads safely
   |
note: required by a bound in `Runtime`
  --> quinn-0.11.11/src/runtime.rs:16:27
   |
16 | pub trait Runtime: Send + Sync + Debug + 'static {
   |                           ^^^^ required by this bound in `Runtime`

error[E0277]: `R` cannot be sent between threads safely
   ... same site, `Send` instead of `Sync`
```

**(b) The timer future.** `http_ng_core::unversioned::Timer::sleep`
returns `impl Future<Output = ()>` with no `Send`, no `'static`, no
`Debug`. `quinn::AsyncTimer` wants all three:

```
$ cargo check --features t_timer
error[E0277]: `SleepTimer<F>` doesn't implement `Debug`
note: required by a bound in `AsyncTimer`
  --> quinn-0.11.11/src/runtime.rs:34:30
   |
34 | pub trait AsyncTimer: Send + Debug + 'static {
   |                              ^^^^^ required by this bound in `AsyncTimer`

error[E0277]: `F` cannot be sent between threads safely
   ... note: required because it appears within the type `SleepTimer<F>`
   ... note: required by a bound in `AsyncTimer`

error: lifetime may not live long enough
  --> src/lib.rs:83:9
   |
82 |       pub fn make<R: Timer>(r: &R, d: Duration) -> Pin<Box<dyn quinn::AsyncTimer>> {
   |                                - let's call the lifetime of this reference `'1`
83 | /         Box::pin(SleepTimer {
84 | |             fut: Box::pin(r.sleep(d)),
85 | |         })
   | |__________^ coercion requires that `'1` must outlive `'static`
```

**And a fourth problem in the same place that no bound fixes.**
`AsyncTimer::reset(self: Pin<&mut Self>, i: std::time::Instant)` re-arms
an existing timer to an *absolute* `std::time::Instant`. Our seam has
`type Instant: Copy + PartialOrd` and `fn sleep(&self, d: Duration)` — a
one-shot relative sleep, with an opaque instant type that has no
conversion to `std::time::Instant` in either direction (`Tokio`'s is
`tokio::time::Instant`, a wrapper; `Smol`'s is `std::time::Instant`, and
`two_runtimes.rs` exists partly to pin that they differ). So a
`quinn::Runtime` over `R: Timer` needs *either* a new method on `Timer`
(`sleep_until`, plus a conversion) *or* to bypass `Timer` and use the
runtime's own clock crate. This is the same class of finding as W7's
`TcpOpts`: the seam has no way to express what is being asked.

**(c) The UDP socket.** A UDP capability shaped exactly like
`TcpConnect` — an associated socket type carrying only what the protocol
layer needs — does not satisfy `quinn::AsyncUdpSocket`:

```
$ cargo check --features t_socket
error[E0277]: `Adapter<S>` doesn't implement `Debug`
error[E0277]: `S` cannot be shared between threads safely
error[E0277]: `S` cannot be sent between threads safely
   ...
note: required by a bound in `AsyncUdpSocket`
  --> quinn-0.11.11/src/runtime.rs:42:27
   |
 42 | pub trait AsyncUdpSocket: Send + Sync + Debug + 'static {
   |                           ^^^^ required by this bound in `AsyncUdpSocket`
```

The control — the same three impls with the bounds written in — compiles:

```
$ cargo check --features t_ok
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 0.08s
```

**What this costs, stated plainly.** `TcpConnect::Stream` is
`hyper::rt::Read + Write + Unpin` and nothing else, and
`http-ng-native/src/connect.rs:666`'s `FakeStream` holds an `Rc<()>` for
the sole purpose of proving no path needs `Send`. A UDP capability that
feeds quinn cannot keep that property: its socket must be
`Send + Sync + 'static`. That is not fatal — a socket being `Send` is
normal, unlike a *future* being forced `Send` — but it is a real
asymmetry between the two capabilities, and it must be written into the
trait rather than discovered by the first implementer.

### 1.5 Not asked, and it decides whether h3 is worth having

An idle HTTP/1 socket in W2's pool needs nobody: the kernel holds it, and
`checkout`'s poll is enough. **A QUIC connection is a state machine with
timers, and it only advances while something polls it.** The PING that
resets the peer's idle timer is emitted by quinn's connection driver —
one of the two futures §1.2 counted — not by the kernel.

`spikes/q1-quic/src/bin/idle.rs`: one request, then a 1500 ms gap, then a
second request on the same connection. `max_idle_timeout = 1000ms`,
`keep_alive_interval = 300ms` — the configuration a pool *wants*. The
only difference between A and B is whether anything polls during the gap.
The observer is the request itself, and B is the control that stops
"nothing works" from being the explanation.

```
$ cargo run --bin idle
A. connection left unpolled across the gap:
  request 1: Ok((200, "pooled?"))
  gap 1500ms (NOT POLLED), idle_timeout 1000ms -> request 2: FAILED: Connection error: Remote error: Error undefined by h3: reset by peer
B. control, same gap, connection driven:
  request 1: Ok((200, "pooled?"))
  gap 1501ms (DRIVEN), idle_timeout 1000ms -> request 2: OK 200 OK "pooled?"
```

Now put that next to the two facts v0.2 already established:

- `http_ng_rt::Spawn<F>`'s implementations require `F: Send + 'static`,
  and "a pool driven by a spawned background task does not compile on
  this seam at all" (`docs/v02-acceptance.md`).
- W2's pool therefore has no reaper, by the same argument.

For h1 that was a cosmetic loss — an idle socket held a few minutes too
long. For h3 it is the whole feature: **an h3 connection that is not
being polled is not idle, it is dying**, and the multiplexing that
justifies h3 in the first place only pays off across requests, i.e.
exactly across the gaps nobody polls.

Three ways out, and none of them is free:

1. **One connection per request.** Compiles today, measured working
   (§1.2), and buys nearly nothing over h1 — a full QUIC+TLS handshake
   per request, recovered only partly by 0-RTT (§3.2). Honest, cheap,
   and a poor advertisement for HTTP/3.
2. **A driver handle the caller must poll.** The connection's two
   futures move into a value the application owns and polls (or drops).
   This is `wait_idle`'s shape, and h3's own examples spawn it. It works
   with no `Spawn`, and it changes `Client`'s contract: a client that
   must be polled between requests is not the `Client` this project
   ships.
3. **`Spawn` becomes usable.** Not by relaxing `F: Send` — quinn's
   futures *are* `Send`, so this specific case would work — but the
   seam's implementations also require `'static`, and `Spawn` is
   currently implemented by exactly two runtimes and used by none of the
   library code. Making the h3 backend the first consumer of `Spawn`
   makes `Spawn` mandatory for any runtime that wants h3, which is the
   opposite of the direction W7 established for embassy.

I would take (2) for a first cut and say so in the type system, because
it is the only one that is both honest and cheap. But it is a change to
what `Client` means, and it belongs in the design doc, not in a backend.

### 1.6 `quinn` vs `quinn-proto`

`quinn-proto` is genuinely sans-io — the same shape as `http-ng-proto` —
and its driving surface is public (`Endpoint`, `Connection`,
`ConnectionHandle`, `Transmit`, `EndpointEvent`, `ConnectionEvent`,
`Event`, `StreamEvent`, `Dir`, `StreamId`, all asserted nameable in
`unsealed.rs`). Nothing about it demands `Send`, a spawner or a reactor.

**It is not needed, and I would not use it.** `quinn` is what stands
between `quinn-proto` and a socket, and §1.2 shows that layer is already
compatible with our constraints once its `Runtime` is ours. Re-writing it
would be a second QUIC event loop in this workspace with its own
congestion-control bugs, its own timer handling and its own loss
detection — the same argument W7 used to reject a second HTTP/1
implementation, with higher stakes. `quinn-proto` is the fallback if the
`Send + Sync + 'static` bounds of §1.4 ever become intolerable, and that
is a measurement nobody has needed to make.

`h3` is sans-io over the `h3::quic` traits and assumes no spawner: its
`client::Connection` exposes `poll_close(&mut self, cx)`, which §1.2
drives inside the same `select` as the request. Its doc examples spawn;
its code does not require it.

---

## 2. Q2 — the UDP capability

### 2.1 What is actually available, measured

`spikes/q2-udp` asks `quinn-udp` about a plain `std::net::UdpSocket` and
then puts all three features through a real loopback exchange.

```
$ cargo run          # spikes/q2-udp
== capabilities reported for a plain std::net::UdpSocket ==
  max_gso_segments (send batch) : 64
  gro_segments     (recv batch) : 64
  may_fragment                  : false
  BATCH_SIZE (recvmmsg slots)   : 32

sent 3600 bytes in ONE send() with segment_size=1200 and ecn=Ect0
  recv round 0: len=3600 stride=1200 -> 3 datagram(s), ecn=Some(Ect0), from=127.0.0.1:51757
total bytes 3600 of 3600, datagrams 3 of 3

tokio::net::UdpSocket    -> gso=64 gro=64 (AsFd, so quinn-udp works verbatim)
async_io::Async<UdpSocket> -> gso=64 gro=64 (AsFd, so quinn-udp works verbatim)
```

Three datagrams from one `sendmsg`, three coalesced into one `recvmmsg`
with `stride=1200`, and the ECN codepoint survived the round trip. This
is the only place in the report where a number is a *ceiling* rather than
a fact about our code: 64 is this kernel's `UDP_MAX_SEGMENTS`.

### 2.2 Where each one comes from, and why it is a capability

All three are `cmsg` on a descriptor, applied and read by `quinn-udp`
(`quinn-udp-0.5.15/src/unix.rs`): `UDP_SEGMENT` for GSO, `UDP_GRO` for
GRO, `IP_TOS`/`IPV6_TCLASS` + `IP_RECVTOS`/`IPV6_RECVTCLASS` for ECN.
The crate is explicit that they are not universal — *"Some features are
unavailable in some environments… When support is unavailable,
functionality will gracefully degrade"* (`lib.rs:22-23`) — and it ships a
`fallback.rs` for anything that is neither unix nor windows, headed
`// No ECN support`.

That is exactly the shape this project refuses to leave implicit.
"Gracefully degrade" is a fine default for a QUIC library and a defect
for us, because the degradation is invisible: a client without ECN is a
client whose congestion controller cannot tell congestion from loss, and
a client without GSO does one syscall per 1200 bytes. Neither fails.
Both must be reportable.

They are **three capabilities, not one**, because they degrade
independently and quinn already treats them so:
`max_transmit_segments()`, `max_receive_segments()` and the per-datagram
`ecn` field are three separate answers on `AsyncUdpSocket`, and
`may_fragment()` is a fourth. Collapsing them into one boolean would be
the `TcpOptsSupport` mistake in a new place.

### 2.3 The trait, in signatures

Written against what quinn actually calls, not against what a UDP socket
usually looks like:

```rust
/// Bind, not connect. QUIC's socket is UNCONNECTED: one endpoint socket
/// serves every connection it opens, the peer's address can change under
/// migration, and `connect(2)` would make both impossible. This is the
/// first place the UDP capability is not "TcpConnect with a different
/// letter".
pub trait UdpBind {
    /// `Send + Sync + 'static` is a finding, not a preference: quinn
    /// stores this behind `Arc<dyn AsyncUdpSocket>` and its trait is
    /// declared `Send + Sync + Debug + 'static` (§1.4c).
    type Socket: UdpDatagrams + Send + Sync + 'static; // send-bound-exception: see §1.4

    fn bind(&self, local: SocketAddr) -> impl Future<Output = io::Result<Self::Socket>>;
}

pub trait UdpDatagrams: std::fmt::Debug {
    /// One call, N datagrams. `segment_size: Some(n)` is GSO; `None` is
    /// a single datagram. A backend without GSO must either loop or
    /// refuse — never silently send the whole buffer as one datagram.
    fn poll_send(&self, cx: &mut Context<'_>, t: &Datagrams<'_>) -> Poll<io::Result<()>>;

    /// Caller-owned buffers, plural, with one metadata slot each —
    /// NOT `recv_from(&mut [u8]) -> (usize, SocketAddr)`. Two reasons,
    /// both measured above: a GRO read returns several datagrams in one
    /// buffer and needs `stride` to split them, and each read needs its
    /// own `ecn` and its own destination address. A `recv_from` shape
    /// cannot carry either, and a capability that cannot carry ECN is a
    /// capability that silently drops it.
    fn poll_recv(
        &self,
        cx: &mut Context<'_>,
        bufs: &mut [IoSliceMut<'_>],
        meta: &mut [RecvMeta],
    ) -> Poll<io::Result<usize>>;

    fn local_addr(&self) -> io::Result<SocketAddr>;

    /// The three answers, separately, defaulting to the weakest — the
    /// `TcpOptsSupport::NONE` rule. A runtime that does not override
    /// these under-claims; it never promises an offload it drops.
    fn caps(&self) -> UdpCaps { UdpCaps::NONE }
}

pub struct Datagrams<'a> {
    pub destination: SocketAddr,
    pub src_ip: Option<IpAddr>,   // needed when bound to a wildcard v6 address
    pub ecn: Option<EcnCodepoint>,
    pub segment_size: Option<usize>,
    pub contents: &'a [u8],
}

pub struct RecvMeta {
    pub addr: SocketAddr,
    pub len: usize,
    pub stride: usize,            // 0 or >= len means "one datagram"
    pub ecn: Option<EcnCodepoint>,
    pub dst_ip: Option<IpAddr>,
}

pub struct UdpCaps {
    pub max_send_segments: usize, // 1 == no GSO
    pub max_recv_segments: usize, // 1 == no GRO
    pub ecn: bool,
    pub may_fragment: bool,       // true == PMTU discovery is unreliable
}
```

Deliberately **not** in it: `connect`/`send`/`recv` (a connected socket
cannot serve a QUIC endpoint), a `recv_from` returning a fresh `Vec` (an
allocation per datagram at 1200 bytes a time), and any method that
returns `io::Result<()>` for a feature that was silently not applied.

Two of these types (`RecvMeta`, `EcnCodepoint`) already exist in
`quinn-udp` with these exact fields. Whether to re-declare them or
re-export is an implementation choice; re-declaring keeps `http-ng-rt`
free of a QUIC dependency, which is worth more than the conversion costs.

### 2.4 What each runtime can actually provide

| runtime | GSO | GRO | ECN | how |
|---|---|---|---|---|
| tokio | 64 | 64 | yes | `tokio::net::UdpSocket: AsFd`, measured §2.1 |
| smol / `async-io` | 64 | 64 | yes | `async_io::Async<UdpSocket>` derefs to the std socket, measured §2.1 |
| embassy-net | **no** | **no** | **no** | there is no descriptor at all — quoted below |

The embassy answer is not "embassy-net has not implemented them". It is
that the question cannot be asked: `quinn_udp::UdpSockRef: From<&T>
where T: AsFd`, and all three features are cmsg on a descriptor.

```
$ cargo check --features t_asfd      # spikes/q2-embassy
error[E0277]: the trait bound `embassy_net::udp::UdpSocket<'_>: AsFd` is not satisfied
  --> src/lib.rs:12:36
   |
12 |     quinn_udp::UdpSocketState::new(quinn_udp::UdpSockRef::from(sock))
   |                                    ^^^^^^^^^^^^^^^^^^^^^ the trait `AsFd` is not implemented for `embassy_net::udp::UdpSocket<'_>`
   |
   = note: required for `UdpSockRef<'_>` to implement `From<&embassy_net::udp::UdpSocket<'_>>`
```

Reading `embassy-net-0.9.1/src/udp.rs` confirms the API has no room for
them either: `poll_recv_from(&self, buf: &mut [u8], cx) -> Poll<Result<
(usize, UdpMetadata), RecvError>>` is one datagram into one buffer, and
`UdpMetadata` (smoltcp 0.13) carries `endpoint`, `local_address` and
`meta` — no ECN, no stride. The only IP-layer knob exposed is
`set_hop_limit`. So an embassy-net UDP backend would report
`UdpCaps::NONE` and be correct.

**Unverified: macOS and Windows.** `quinn-udp` has a `windows.rs` and
per-platform code in `unix.rs` (including *"mac and ios do not support
IP_RECVTOS on dual-stack sockets"*, `unix.rs:114`), so the numbers will
differ and the `UdpCaps` values will not be `64/64/true` everywhere.
What would settle it: running `spikes/q2-udp` on the existing
`macos-latest` and `windows-latest` CI runners — it is a 100-line binary
with two dependencies, and it prints exactly the table above.

---

## 3. Q3 — what happens to the TLS seam

**This is the section that decides the shape**, and the 0-RTT
requirement is what makes it so rather than the datagram question.

### 3.1 `TlsConnect` cannot carry QUIC, and not by a small margin

`TlsConnect::connect<S>(&self, io: S, req: TlsRequest<'_>) ->
impl Future<Output = Result<(Self::Stream<S>, TlsInfo), Error>>` where
`S: hyper::rt::Read + Write + Unpin`. Bytes in, bytes out, over an
already-established stream.

What QUIC needs instead, from `quinn-proto-0.11.16/src/crypto.rs:28`:

```rust
pub trait Session: Send + Sync + 'static {
    fn initial_keys(&self, dst_cid: &ConnectionId, side: Side) -> Keys;
    fn early_crypto(&self) -> Option<(Box<dyn HeaderKey>, Box<dyn PacketKey>)>;
    fn early_data_accepted(&self) -> Option<bool>;
    fn read_handshake(&mut self, buf: &[u8]) -> Result<bool, TransportError>;
    fn write_handshake(&mut self, buf: &mut Vec<u8>) -> Option<Keys>;
    fn next_1rtt_keys(&mut self) -> Option<KeyPair<Box<dyn PacketKey>>>;
    fn transport_parameters(&self) -> Result<Option<TransportParameters>, TransportError>;
    fn is_valid_retry(&self, orig_dst_cid: &ConnectionId, header: &[u8], payload: &[u8]) -> bool;
    // … plus handshake_data, peer_identity, is_handshaking, export_keying_material
}
```

Eleven methods, and the important thing is what *kind* they are:
`initial_keys`, `next_1rtt_keys` and `early_crypto` hand out **key
schedules per encryption level**; `read_handshake`/`write_handshake`
move CRYPTO-frame payloads that the QUIC layer, not TLS, is responsible
for framing and retransmitting; `transport_parameters` returns a
**QUIC-specific TLS extension** that has no counterpart over TCP.

**This is not a compile error, and that is worth saying, because it is
worse than one.** An adapter
`impl<T: TlsConnect> quinn_proto::crypto::ClientConfig for Quic<T>`
type-checks fine — with an empty body. `TlsConnect` has four methods
(`connect`, `tls_support`, `reports_alpn`, `config_id`) and the only one
that produces anything produces a wrapped byte stream. There is no
expression in `TlsConnect`'s vocabulary whose value can become a `Keys`.
The intersection of what `TlsConnect` offers and what `Session` requires
is empty, so an adaptation layer would have to *contain a second TLS
implementation*, at which point it is not an adaptation.

Two further facts in the same direction: `Session` is `Send + Sync +
'static` and `crypto::ClientConfig` is `Send + Sync`, neither of which
`TlsConnect` requires; and `TlsRequest` carries `ech: Option<&[u8]>`,
which for QUIC belongs to the same `Session` construction rather than to
a wrapper.

### 3.2 0-RTT works — measured, through h3, on our no-spawn loop

`spikes/q1-quic/src/bin/zero_rtt.rs`. Two connections to the same server
through **one** `rustls::ClientConfig` — which is where the session store
lives, and which is exactly what `http_ng_tls_rustls::Rustls::from_config`
already holds one of. Three scenarios; the third arranges a *rejection*
on purpose by pointing the second connection at a different server
instance with the same certificate and its own ticketer.

```
$ cargo run --bin zero_rtt
== server sets max_early_data_size = u32::MAX ==
  conn 1 (1-RTT): handshake 3.98727ms, request Ok(200)
  conn 2: into_0rtt() gave a Connection after 1.27366ms
  conn 2: h3 layer up at 1.680785ms, response at 8.580084ms (Ok(200)), ZeroRttAccepted resolved to true at 8.629475ms

== server leaves max_early_data_size = 0 (the default) ==
  conn 1 (1-RTT): handshake 5.221425ms, request Ok(200)
  conn 2: into_0rtt() REFUSED after 712.971µs — no usable 0-RTT key material

== 0-RTT offered to a DIFFERENT server instance (same cert, other ticketer) ==
  conn 1 (1-RTT): handshake 3.386553ms, request Ok(200)
  conn 2: into_0rtt() gave a Connection after 1.423476ms
  conn 2: h3 layer up at 5.733898ms, response at 12.957021ms (Err(Undefined(ZeroRttRejected))), ZeroRttAccepted resolved to false at 13.005752ms
```

Read the three lines of scenario 1 in order, because the order is the
finding:

- the connection exists at **1.27 ms**, before any handshake round trip;
- the h3 layer (SETTINGS on the control stream, the request stream) is up
  at **1.68 ms**, still before the handshake completed;
- the response arrives at **8.58 ms**;
- and only at **8.63 ms** — *after* the response — does the client learn
  whether its early data was accepted.

### 3.3 Therefore `TlsInfo::early_data_accepted` cannot carry this

W3 has reserved, in `TlsRequest`/`TlsInfo`, exactly the two slots this
would want (I read them in the W3 working tree; they are not on `main`
at `5f9d82f`):

```rust
pub early_data: Option<usize>,          // TlsRequest
pub early_data_accepted: Option<bool>,  // TlsInfo
```

The reservation is right and the reasoning attached to it is right. The
placement does not survive contact with QUIC, for four separate reasons,
each of which is visible in the run above:

1. **`TlsInfo` is returned by `connect`, and in the 0-RTT case there is
   no handshake result at `connect` time.** `into_0rtt()` returns at
   1.27 ms with the answer genuinely unknown; `ZeroRttAccepted` is a
   `Future<Output = bool>` (`quinn-0.11.11/src/connection.rs:219`) that
   resolves when the handshake finishes. A `TlsInfo` field can only hold
   it by making `connect` wait for the handshake — which is precisely
   the round trip 0-RTT exists to skip. **The answer is a future, and
   whatever seam we build must carry it as one.**
2. **The rejection surfaces on the request, not on the connection.**
   Scenario 3's response is `Err(Undefined(ZeroRttRejected))`, at the h3
   stream layer, while the connection itself is fine and the 1-RTT
   handshake succeeded. quinn documents this: *"If it was rejected, the
   existence of streams opened and other application data sent prior to
   the handshake completing will not be conveyed to the remote
   application, and local operations on them will return `ZeroRttRejected`
   errors"* (`connection.rs:99`). So the transport must **replay the
   request on the same connection**, after the handshake, and the caller
   should never see `ZeroRttRejected`.
3. **`TlsRequest::early_data` is per-connect; rustls's switch is
   per-config.** `rustls::ClientConfig` has `pub enable_early_data: bool`
   (`client_conn.rs:227`). Our `Rustls::config_for` already caches one
   cloned `ClientConfig` per ALPN set; honouring a per-connect
   `early_data` would add a second cache dimension, i.e. two
   `ClientConfig`s per ALPN set. Cheap, but it is a change, not a
   read of an existing field.
4. **The `usize` has no client-side meaning.** `max_early_data_size` is a
   *server* field in rustls; the client's budget comes from the
   remembered ticket. `Option<usize>` should probably be a plain
   `bool`-shaped opt-in on the QUIC side — worth settling before the slot
   ships, since changing it later is the breaking change the slot exists
   to avoid.

### 3.4 The session store — what carries over, and what does not

The coordinator's note says part of the machinery assembled itself as a
side effect of W2. **Checked, and it is half true, which is the useful
half to know.**

**True.** `http_ng_tls_rustls::Rustls` holds `base: Arc<rustls::ClientConfig>`
(`lib.rs:27`), and rustls keeps resumption there:
`ClientConfig` is `#[derive(Clone)]` (`client_conn.rs:163`) and
`Resumption { store: Arc<dyn ClientSessionStore>, .. }`
(`client_conn.rs:480-483`). So `config_for`'s per-ALPN clones share one
store by construction — one `Rustls` value is one ticket cache, across
every ALPN set it hands out. And `TlsConfigId`, drawn once per `Rustls`
and already in W2's `PoolKey`, is exactly the identity that store belongs
to: two clients with different roots must not share tickets for the same
reason they must not share sockets. That is a real, unplanned fit, and
nothing about it needs redesigning.

**Not true, in three places.**

- **The store does not distinguish QUIC from TCP.**
  `ClientSessionStore`'s methods are keyed by `ServerName` alone
  (`client_conn.rs:44-74`), while a TLS 1.3 ticket carries
  `quic_params` (`msgs/persist.rs:81`) that rustls sets only on QUIC
  connections (`client/tls13.rs:1513`) and reads back unconditionally
  when resuming a QUIC connection (`client/hs.rs:1146-1151`). So one
  `Rustls` serving both an h1/h2 TCP path and an h3 QUIC path has one
  slot per host for two kinds of ticket. **Unverified: what actually
  happens** when a TCP-issued ticket (empty `quic_params`) is offered to
  a QUIC handshake. What would settle it: one server process serving
  both TLS-over-TCP and QUIC with a shared ticketer, one shared
  `ClientSessionStore`, a TCP handshake followed by a QUIC 0-RTT
  attempt. Until then the safe design is **a separate store for the
  QUIC path**, which costs one `Arc` and removes the question.
- **0-RTT reuse is not pool reuse.** W2's `PoolKey` governs handing back
  a live connection. 0-RTT builds a *new* connection from a stored
  ticket, so it is reachable when the pool is empty — which is the case
  h3 is in most often (§1.5). The key is right; the code path is not the
  pool's.
- **`enable_early_data` is off by default** in rustls and our `Rustls`
  never sets it, so today's `http-ng` stores tickets and never offers
  early data. That is the correct default and should stay the default.

### 3.5 Replay is policy — and `RetryKind` answers the other half

`RequestBody::retry_kind()` answers **"can I send this body again?"** —
`Free`, `ViaFactory`, `Impossible`. 0-RTT needs that answer, for §3.3(2):
a request that went into early data and was rejected must be replayed,
and a `Streaming` body cannot be. So `RetryKind` is a hard precondition:
`RetryKind::Impossible` must never enter early data, because the
transport could not recover it.

But it does not answer the question that decides exposure. **"Can I
resend this" and "may an attacker resend this" are different
questions**, and a body that is trivially replayable is often exactly the
one that must not be replayed: `POST /transfer` with
`RequestBody::Full(bytes)` is `RetryKind::Free` and is precisely what
0-RTT must refuse. quinn says the same in one line: *"this enables
transmission of 0-RTT data, which is vulnerable to replay attacks, and
should therefore never invoke non-idempotent operations"*
(`connection.rs:120`).

**The notion that answers it — method safety/idempotency — does not
exist in this codebase, and its absence is deliberate and written down.**
`http-ng-native/src/lib.rs:400-411`, W2's one retry:

> *"…it hands the request back only when not a byte of it reached the
> wire (`h1::Failed`), so what is resent below is the original request
> object, its body untouched at its first byte. No clone, no rewind, and
> **nothing to decide about idempotency: this is not a second request, it
> is the first one, which never left.**"*

That reasoning is exact, and it is exactly why it does not extend: 0-RTT
data *does* reach the wire, and can reach it twice from an attacker. So
0-RTT is the first feature in this project that needs an idempotency
judgement.

The standard says where the default has to sit, and it is the
conservative side. RFC 8470 §2: *"Absent other information, clients MAY
send requests with safe HTTP methods (RFC 7231, Section 4.2.1) in early
data when it is available and MUST NOT send unsafe methods (or methods
whose safety is not known) in early data."* Note "absent other
information" — the method table is the floor, not the answer, because
"GET is safe" is untrue of plenty of real APIs and only the caller knows.
So the judgement must be a **caller-visible decision**, with a method
check as the guard beneath it, not a table hidden in a transport.

**And there is a third failure path the same RFC defines, which neither
the coordinator's note nor §3.3 covers.** RFC 8470 §5.2 gives servers
`425 (Too Early)` — *"the server is unwilling to risk processing a
request that might be replayed"* — and requires that *"A user agent
SHOULD retry automatically, but any retries MUST NOT be sent in early
data."* So a request placed in early data can fail in **three** distinct
ways, at three different layers, and a design that handles one is
incomplete:

| failure | where it appears | when | what the transport must do |
|---|---|---|---|
| no usable key material | `into_0rtt()` returns `Err(Connecting)` | before a byte is sent | fall back to 1-RTT silently; nothing was risked |
| the server rejected the 0-RTT keys | `ZeroRttRejected` on the h3 stream (measured, §3.2 scenario 3) | after sending, before/around the response | replay on the same connection once the handshake completes |
| the server processed nothing on purpose | HTTP status `425` | a full round trip later | replay, and **not** in early data |

Only the first is free. The other two need `RetryKind` to be at least
`ViaFactory`, which is why this section's first paragraph is a hard
precondition rather than a nicety — and the third needs a status-code
branch in the client, i.e. it is not purely a transport concern.

The shape that fits the rules already in force (floor rule; a default
never stronger than the truth; over-claiming must not be the silent
option):

- `Capabilities` gains **one** field with a conservative default —
  `EarlyDataSupport::{None, Supported}`, `None` in
  `Capabilities::none()` and `None` for every backend that ships today.
  It reports the *floor*: `Supported` means only "this transport can
  offer early data", never "this request went in early data".
- Admission is **per request, opt-in, and refuses** — the same move W3
  chose for h2's duplex: an extension on the request
  (`ReplaySafe`/`AllowEarlyData`) that the caller sets, and which yields
  a typed `UnsupportedCapability` against a transport that declares
  `None`. Absent the extension, the request waits for 1-RTT. There is no
  configuration in which a request the caller did not mark ends up in
  early data.
- Acceptance is **observed after the fact, and mostly not by the
  caller**: the transport replays a rejected request itself (§3.3(2))
  and the caller sees a normal response. If the outcome is worth
  surfacing at all it belongs in `http::Extensions` on the response, next
  to where the negotiated version already lives — not in `TlsInfo`.

### 3.6 The options, costed

| option | what it costs | verdict |
|---|---|---|
| **A second trait beside `TlsConnect`**, for QUIC key schedules, implemented only by backends that can | one new trait in `http-ng-tls` (or its own crate); `http-ng-tls-rustls` gains an impl that is mostly a wrapper over `quinn_proto::crypto::rustls::QuicClientConfig`; `http-ng-tls-native-tls` gains **nothing** and implements **nothing** — it is a compile error to use it for h3, which is the honest outcome | **this one** |
| The h3 path depends on `rustls` directly, and "two TLS backends behind one seam" is declared not to extend to it | cheapest today, and wrong later for the same reason `TlsConnect` exists: it forecloses SChannel and Security.framework, both of which *do* support QUIC natively, and it hard-codes a crypto library into a transport | no |
| Widen `TlsConnect` with QUIC methods, defaulted | every existing implementation silently gains a QUIC story it cannot honour, and `NoTls`'s uninhabited `Stream<S>` — currently a nice piece of type-level honesty — becomes meaningless | no |

**What A costs the two existing TLS crates, concretely.**
`http-ng-tls-rustls`: one impl, plus the `enable_early_data` cache
dimension of §3.3(3), plus a decision about a separate session store
(§3.4). `http-ng-tls-native-tls`: **nothing at all** — and the reason is
stronger than the ALPN one already documented. It is not that
`async-native-tls` fails to expose the negotiated protocol; it is that
SChannel's and Security.framework's QUIC support is a *different API
surface* that `native-tls` does not bind at any level, so there is no
partial implementation to write. `AGENTS.md`'s "two TLS backends, both
behind the same `TlsConnect` seam" stays true, and h3 sits beside that
sentence rather than inside it — the same way `NoTls` is a third choice
rather than a weaker second.

`TlsConnect::reports_alpn()` (W3, read in that branch) is the shape to
copy for every capability here, and its argument transfers verbatim: a
default that over-claims turns a lost optimisation into a protocol error.
For 0-RTT the same default turns a lost optimisation into replay
exposure, so the default is `None` and the method has no `true` default
anywhere.

**ALPN over QUIC needs no new question.** It is `h3` (RFC 9114 §3.2 —
*"During connection establishment, HTTP/3 support is indicated by
selecting the ALPN token 'h3' in the TLS handshake"*), it
is mandatory, and rustls reports it — the spikes here set
`alpn_protocols = vec![b"h3".to_vec()]` on both ends and every run
above succeeded, which would not have happened had negotiation failed.
A QUIC connection whose ALPN is not `h3` is an error, not a fallback,
so the `reports_alpn() == false` case simply cannot arise on this path:
a backend that cannot report ALPN cannot implement the QUIC trait at all.

---

## 4. Q4 — discovery

**Alt-Svc is not the first cut, and — this is the part the design doc
did not have — it need not be, because a second discovery mechanism
already ships in this repository.**

`http_ng_dns::SvcbEndpoint` (`crates/http-ng-dns/src/lib.rs:85`) is:

```rust
pub struct SvcbEndpoint {
    pub priority: u16,
    pub target: String,
    pub alpn: Vec<Vec<u8>>,      // <- "h3" arrives here
    pub port: Option<u16>,
    pub ipv4hint: Vec<Ipv4Addr>,
    pub ipv6hint: Vec<Ipv6Addr>,
    pub ech_config_list: Option<Bytes>,
}
```

and v0.2 already shipped "real SVCB/HTTPS through the system resolver"
plus the hickory resolver, with `Resolve::supports_svcb()` as the
capability that says whether a given resolver can answer. An HTTPS RR
advertising `alpn="h3"` is how browsers avoid the first-request
downgrade, and it has the property Alt-Svc lacks: **the cache with a
lifetime already exists and is not ours** — it is the DNS resolver's, with
the record's TTL, maintained by code this project does not write.

Today `connect.rs` uses none of it (`grep` finds `lookup_svcb` only in
that file's module doc, explaining that Task 6 built the plumbing and
Task 7 did not use it). So the first cut has three tiers, in increasing
cost:

1. **Explicit opt-in.** `Client::builder(http_ng_h3::H3::new(..))`, or a
   scheme/extension that names h3. No discovery, no cache, no state. This
   is what a first h3 backend ships with, and it is honest: h3 is a
   deployment decision the caller makes.
2. **HTTPS/SVCB.** Consume the `alpn` field that already exists, on the
   resolvers that already answer, gated by `supports_svcb()`. No new
   persistence. This is where h3 becomes automatic without inventing
   anything, and it is a task, not a vertical.
3. **Alt-Svc.** Still deferred, and the design doc's reason still holds.

**Scope of the smallest honest Alt-Svc, since the brief asks for a scope
and not a design.** Four things it must have, and each is a reason it is
not this work item:

- **A store with an eviction policy**, keyed by origin, holding
  `(alpn, host, port, expiry)` from `ma=`. Where it lives is the
  question the design doc flagged: per-`Client` means a fresh process
  learns nothing, and anything wider is shared mutable state across
  clients with different trust configurations — the defect
  `TlsConfigId` exists to prevent, in a new place.
- **A negative cache**, or the first failed h3 attempt is repeated for
  every request for `ma` seconds.
- **A rule for `clear`** and for `Alt-Svc` on an h3 response itself.
- **A statement about what it does to `Capabilities`.** A client whose
  protocol is decided by a header it has not received yet cannot answer
  `full_duplex` at construction — which is the *same* question W3 just
  settled by the floor rule, so the answer exists, but it has to be
  applied deliberately rather than inherited.

None of that is protocol work, all of it is policy work, and it is
strictly cheaper after tier 2 exists than before.

---

## 5. Q5 — what CI could actually run

**Not compile-only.** Everything in §1, §2 and §3 ran here, on loopback,
against a real quinn + h3 server, with a self-signed certificate from
`rcgen` (already a dev-dependency of `http-ng-tls-rustls`). The whole
`nospawn` exchange is `wall=2.9ms`; the 0-RTT scenario set, including two
server instances and two 200 ms ticket waits, is under a second.

What a job would contain, in order of what it proves:

| check | proves | cost |
|---|---|---|
| an `h3` request against a `quinn` + `h3` server on `127.0.0.1`, driven with no spawn | the whole stack, the way `two_runtimes.rs` proves the h1 one | milliseconds |
| the idle pair of §1.5, unpolled vs driven | that the pooling limitation is a fact and stays one | ~3 s |
| the 0-RTT triple of §3.2 (accepted / refused / rejected) | that early data is offered only where it is safe, and that a rejection is replayed rather than surfaced | ~1 s |
| printing `UdpCaps` on all three runners | that GSO/GRO/ECN are reported, not assumed — and it is the only way we would ever notice a runner where they vanish | milliseconds |

Three things about the environment, stated as what they are:

- **UDP on loopback works here**, in this sandbox, unprivileged, no
  namespace tricks — unlike W7's TAP device. Nothing about a QUIC test
  needs elevated privileges: it is two `UdpSocket::bind("127.0.0.1:0")`.
- **Certificates are already solved.** `rcgen` +
  `rustls::ClientConfig::with_root_certificates`, exactly as
  `crates/http-ng-tls-rustls/tests/server.rs` does for TCP. QUIC adds
  only `QuicServerConfig::try_from` and `alpn_protocols = ["h3"]`, and
  requires TLS 1.3 (`builder_with_protocol_versions(&[&TLS13])`).
- **Unverified: GSO/GRO/ECN on GitHub runners.** Measured `64/64/ECN
  round-trips` on this machine; a virtualised runner may report `1/1` and
  no ECN, and macOS/Windows will differ regardless (§2.4). This is not a
  reason to gate the test — quinn works without all three — but a job
  that *asserts* `gso == 64` would be flaky by construction. What would
  settle it: one throwaway workflow running `spikes/q2-udp` on the three
  runners already in the matrix. The right assertion in CI is that the
  numbers are **reported**, not what they are.

One measurement for the dependency table, since this project keeps one.
An `http-ng-h3` would take `quinn` with **no** `runtime-*` feature (the
runtime is ours) — which builds:

```
$ cargo check                       # spikes/q5-graph
    Checking h3-quinn v0.0.10
    Checking q5-graph v0.0.0
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 4.74s

$ cargo tree -e normal --prefix none | sort -u | wc -l
55

$ cargo tree -e normal -i tokio -f "{p} {f}"
tokio v1.53.1 bytes,default,io-util,sync
├── h3 v0.0.8
├── h3-quinn v0.0.10
├── quinn v0.11.11 futures-io,ring,rustls-ring
└── tokio-util v0.7.19 default
    └── h3-quinn v0.0.10
```

**Tokio does not go away, and it is bigger than hyper's leaf.** `quinn`
declares `tokio = { version = "1.28.1", features = ["sync"] }`
unconditionally (`quinn/Cargo.toml:187`), exactly as hyper does, so the
`AGENTS.md` argument transfers word for word. `h3` adds its own
`tokio = { features = ["sync"] }` and uses `tokio::io::ReadBuf` on the
client path (`h3/src/stream.rs:10`); `h3-quinn` adds `tokio-util`
(`ReusableBoxFuture`, `lib.rs:29`) which brings `io-util`, and the whole
`futures` facade. So the honest row is `[bytes, default, io-util, sync]`
plus `tokio-util`, against `[sync]` for hyper's h1 — and 55 crates for
the h3 stack alone, which is why it belongs behind a feature or in its
own crate whatever else is decided.

---

## Out of scope, confirmed rather than assumed

The browser and WASI. `fetch` and `wasi:http` use HTTP/3 if the host
does; there is nothing to implement and no capability changes, exactly as
for h2. Worth one line beyond the brief's: `quinn-udp` carries a
`wasm_browser` cfg and `quinn` a `#[cfg(not(wasm_browser))]` on
`wrap_udp_socket`, i.e. upstream has begun a WebTransport-shaped browser
story. That is not our h3, and reaching for it would put a QUIC stack
inside a target whose whole claim is that it has none.

---

## Recommendation

**The shape I would build:** `http-ng-h3`, its own crate, on `quinn` +
`h3` + `h3-quinn` with a `quinn::Runtime` of ours whose `spawn` queues
into the exchange's own poll loop (measured working, no spawn, no tokio
on the client, not a busy-spin) — not `quinn-proto`, which would be a
second QUIC event loop for no measured gain; its own crate rather than a
feature of `http-ng-native`, because its `Transport` impl needs
`R: UdpBind` and a QUIC TLS trait that `Native`'s bounds do not have and
cargo's additive features would make unconditional; and with the QUIC
connection's two driver futures owned by a value the caller polls,
because §1.5 shows an unpolled connection dies and this seam has no
`Spawn` to hide that in.

**The seam changes it needs:** three, and one of them is not the one the
design doc names. (1) `http-ng-rt` gains `UdpBind`/`UdpDatagrams` —
unconnected, batched, caller-owned buffers, with GSO/GRO/ECN as three
separately-defaulted capability values and a socket type that must be
`Send + Sync + 'static` where `TcpConnect::Stream` need not be. (2)
`Timer` gains an absolute-deadline sleep and a `Send + 'static` timer
future, or the h3 backend does not use `Timer` at all — `AsyncTimer::
reset(std::time::Instant)` cannot be built from `sleep(Duration)` with an
opaque `Instant`. (3) A **second TLS trait beside `TlsConnect`**, for
QUIC key schedules, implemented by rustls and by nothing else — and with
it, the 0-RTT answer modelled as a *future plus a transport-internal
replay*, never as a `TlsInfo` field, because the acceptance verdict
arrived 50 µs after the response body; plus one conservative
`Capabilities` value and a per-request opt-in that refuses, since
over-claiming this one costs replay exposure rather than a buffered copy.

**The one thing that would kill it:** having nobody to poll a connection
that is not currently serving a request. Measured, with its control: same
gap, same keep-alive, unpolled connection dead, driven connection fine.
Everything h3 is *for* — multiplexing, no head-of-line blocking, 0-RTT on
the second visit — pays off across requests, and this seam has no `Spawn`
that compiles and no reaper for the pool it already has. If the answer is
"one connection per request", h3 buys a handshake it did not need and the
crate is not worth building; if the answer is "the caller polls a driver
handle", that is a change to what `Client` means, and it belongs in the
design doc before a line of the backend is written.
