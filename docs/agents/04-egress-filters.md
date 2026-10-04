# Egress filters

> Part of the [AGENTS.md](../../AGENTS.md) record, split 2026-10-05. The root
> file keeps the operational rules and the index of parts. Sections below
> keep their original headings, and a "this file" in the text named the
> whole record when it was written.

### The QUIC arm could leave the proxy, and the fix was one decision rather than a check

With a proxy **and** the HTTP/3 arm on one `Native`, the routing between
the stacks never asked where the request was meant to go. An origin whose
`Alt-Svc` arrived through the proxy had its **next** request sent over
UDP from this host, straight to the origin; a record offering `h3` did
the same on the first request; and choosing at all meant asking the
**local** resolver for the origin's HTTPS record, which names it to the
very resolver a proxy user is often there to avoid. A Unix socket had the
identical hole. `capabilities().proxy` said `false` in one order of
construction and `true` in the other — the second while the leak was
live. `hclient::Client::new` builds in that second order, so the
`http3` feature plus an `HTTPS_PROXY` on the machine was the whole
recipe. Reproduced before anything changed: six of seven tests red, the
bypass control green.

**The repair was `connect::egress`, asked by both stacks** — a function
that no longer exists: the section after this one made the same decision
a seam, `EgressFilter`, and moved the proxies out of `Native`. It answered
*direct, through this proxy, or over the socket* for one request, and the
connector already made exactly that decision inline — unix, then
`Proxy::choose`, then resolve. Moving it into a function and asking it
first in `route` means the QUIC arm is reachable only from `Direct`, and
the record lookup and the `Alt-Svc` cache are never consulted for a
request that is not direct. `RequireVersion(HTTP_3)` for such a request
is refused as `Http3NotDirect` under `ErrorKind::Unsupported`, before
anything is dialled: sending it direct would answer a question nobody
asked. In `caps::combine`, `proxy` is now the TCP member's value rather
than the conjunction, because that member holds the list the routing
consults.

**Tearing the runtime seams apart was proposed and declined.**
`TcpConnect` and `UdpBind` already *are* the stream seam and the datagram
seam, named for their protocols; renaming them is a major version of the
stable `hclient-rt` for nothing but names, and a seqpacket seam has no
consumer anywhere in this stack. What was missing was never a seam below
the transport — it was one decision inside it. The datagram half of
proxying, when it comes, is a `UdpBind` wrapper (SOCKS5 UDP ASSOCIATE
fits; MASQUE does not, since it needs an HTTP client to the proxy) and a
fourth `Egress` variant that the QUIC arm may take. *(It came as neither:
the datagram half is a `DatagramPath` a filter opens, not a `UdpBind`
wrapper, and MASQUE got its HTTP client by the transport lending the
filter `Dial::connect_tunnel` — see "HTTP/3 leaves through a filter"
below.)*

Five mutations, five kills, in `tests/proxy_and_quic.rs`: the check
removed (five tests), a Unix socket treated as direct (one), the demand
sent over TCP (one), the conjunction restored (one), and QUIC refused for
any configured proxy regardless of bypass — which only the control kills,
and is the reason it exists.

### Where a connection goes is one filter, and the proxies left `Native`

The section above made *where does this request go* one function inside
`Native`. This one made it a seam, and moved everything that answers it
out of the transport. `hclient-proxy` carries the seam — `Dial`, what a
transport lends; `EgressFilter`, which routes a target and opens it
through that loan; `SendEgressFilter`, its erasable sibling — and the
default filter, `Rules`: the proxy list and the Unix-socket policy,
first match wins. `Native` keeps the question and the machinery to act
on the answer: Happy Eyeballs behind `Dial::connect`, the routing between
the stacks, the pool key. `P` is gone from `Native`, and with it the
mutual exclusion of a proxy and a Unix socket, the panic in
`with_proxies`, and `NoProxy`, which existed only to be `P`'s default.
`.notes/superpowers/specs/2026-09-27-egress-filters-design.md` is the
design, and its rulings ledger is the record of what the plan got wrong.

**The seam returns `impl Future`, and erasure goes through a concrete
type.** The built-in filter is called concretely with `NativeDial`, so
its futures' auto traits are inferred — the default path is unboxed, and
`Native<Embassy, ..>` is asked for nothing. An external filter is erased
at `Native::egress`, where the runtime's, resolver's and TLS backend's
futures can be proven `Send`, into a function pointer (`src/external.rs`)
— the arrangement the IPC dialler and `SpawnH2` already had — and is lent
the transport's connect path as a concrete `BoxDial`. So
`open_stream_send` is written where every type is concrete, which is
`SendTransport`'s shape one seam over, and nothing else in the transport
names those bounds.

**A filter that wraps the stream names its wrapper**, `type Wrapped<S>`,
and that was found by executing the plan rather than by reading it: a
generic `open_stream<C: Dial>` cannot box `C::Stream` into a `Send` box,
and putting `Send` on `Dial::Stream` would have taken the proxies away
from embassy. An associated type lets each filter answer for its own
wrapper — amendment C15's principle, met from the side where the thing
being named is a stream rather than a future. `hclient_proxy::erase` boxes
it at the filter's own concrete impl. The GAT and `open_stream` carry
`where Self: Sized`, which is what keeps `dyn SendEgressFilter` possible.

**An external filter is asked first, and what it declines goes to the
built-in rules.** Replacing them would have been a silent drop of
settings made on the same transport, and refusing the pair a return of
the mutual exclusion this change removed. Every setter appends for the
same reason: `proxy` is `and_proxy`, and a `unix_socket` rule serves
everything and shadows the rules after it, which is ordinary list
semantics where the old code refused.

**What an outside crate can now write, checked from outside.**
`.notes/witnesses/egress-preproxy` is curl's `--preproxy` — SOCKS5, then
an HTTP `CONNECT` through it — as an `EgressFilter` in a crate that is
not a workspace member, driven through `hclient::Client` against a
forwarding SOCKS5 fixture, a tunnelling proxy and an origin. It passed on
its first build with no edit under `crates/`, and fails with its second
hop removed. `cd .notes/witnesses/egress-preproxy && cargo nextest run`.
In-tree, `tests/egress.rs` pins what the old parameter could not say:
HTTP and SOCKS5 rules on one transport, SOCKS5 on a Unix socket
(`Native::proxy_over_ipc`, Tor's `SocksPort unix:` shape), a pooled
tunnel reused, the connect bound through a black-holed proxy, and an
external filter that wraps the stream (XOR) and is asked before the
rules. Every one was killed by its own mutation.

**Two things the first external filter would have tripped on are now
refused by construction.** A proxy reached over a socket is `IpcProxy`,
which only `Native::proxy_over_ipc` takes — installed through `proxy()`
it used to be accepted and fail every request, against the
refuse-at-configuration rule `unix_socket` and `tcp_opts` keep; it is a
compile error now, pinned by a `compile_fail` doctest whose error was
read under `no_run` to be the right one (E0308, `Proxy` expected). And
`hclient_proxy::drive_exact` refuses bytes a proxy sends past its
handshake, the check every filter owes and the witness had written by
hand. Writing its test found the old one had never pinned the
classification: `bytes_past_the_handshake_are_refused` read only the
error's source, so leftover bytes turning `Unreachable` survived it.
`Conn` is `!Sync` since the erased side arrived, which nothing needs
and is said on the type.

**What it cost, measured.** Stripped release binaries against the commit
before the work: `hc` 6,241,128 → 6,293,616 bytes (+52 KiB, +0.84%);
`hclient-native`'s `minimal` example 518,776 → 525,304 (+6.4 KiB,
+1.26%). The `minimal` figure is a program that builds a transport and
sends nothing, and a symbol diff says what it is: about 0.6 KiB of new
drop glue (`Rules`, `Arc<Proxy<BoxHandshake>>`, against the old
`Vec<Proxy<NoProxy>>`), and the rest LLVM moving hyper's and tokio's
drop glue in both directions. `Native::execute`'s future grew by 96 bytes
(20,240 → 20,336 with every feature, 13,104 → 13,200 with none), under
its 24 KiB ceiling. The old `P = NoProxy` was an empty enum, which let
the compiler delete the proxy path from a direct build outright; a list
decided at run time cannot be deleted, and that is the whole of the
trade.

**What is deliberately not done.** Datagrams through a filter:
`FilterSupport::datagrams` is in the seam and honoured by nothing, so a
filtered request never uses HTTP/3 and `RequireVersion(HTTP_3)` for one
is `NoDatagramPath { via }`. The system-proxy translation still installs
HTTP proxies only and refuses a machine naming SOCKS as well; its stated
reason (*a transport holds one `P`*) was corrected, and installing SOCKS
entries is now possible and an owner's decision. TLS to a proxy is the
next section. *(The datagram half is done: a filter declaring `datagrams`
opens a path and HTTP/3 goes over it, and `NoDatagramPath` is now the
answer only for a filter that declares none — see "HTTP/3 leaves through
a filter" below. The system-proxy refusal of SOCKS entries is unchanged.)*

### A filter can ask the transport for TLS, and the stream it gets back is the one it gave

`Dial::connect_tls` runs TLS over one of the transport's own streams,
with the transport's backend and trust, and hands back the same type.
That one choice is the whole design: a filter never names a TLS stream,
so `EgressFilter::Wrapped<S>` stays a function of `S`, and layers
compose — TLS to a proxy, a tunnel, TLS to the origin, and a chain such
as SOCKS5 → TLS → `CONNECT` written outside the workspace
(`.notes/witnesses/egress-tls-chain`), which passed on its first build
with no edit under `crates/` and fails with the `connect_tls` line
removed.

**`hclient-native` lends a recursive `DialStream<S, L>`** — the socket,
or TLS over another `DialStream` — and the `Box` in it exists for the
recursion rather than for erasure, so no `Send` is demanded and
`Native<Embassy, ..>` is still a `Transport`. The two rejected shapes
are why: an erased `connect_tls(BoxIo) -> BoxIo` needs `Send` and would
have cut the `!Send` runtimes off; a GAT `type Tls<S>` forces
`Wrapped` to take the dial as a parameter. The recursion was probed on
a scratch crate before the plan relied on it: it compiles once the
struct carries `S: Io`, and a stream holding an `Rc` makes it `!Send`.
The cost it did bring is a `T: 'static` beside every bound that named
`T::Stream<R::Stream>: 'static`, because a type is `'static` only if its
parameters are.

**The proxy's certificate is checked like an origin's.** One backend,
one trust store — an MDM-pushed root works because the store is the
system's. A separate configuration for proxies (curl's
`--proxy-cacert`) is a field of the `#[non_exhaustive]` `ProxyTls` when
somebody needs it, not a new method. An unknown client-identity label is
refused by the backend, naming it, which `TlsIdentity`'s contract
already owes; an up-front check in `connect_tls` was written and
removed, because deleting it left every test green.

**`https://` in `HTTPS_PROXY` is a TLS proxy now**, not a refusal:
`ParseError::TlsToProxyUnsupported` is gone, the default port for the
scheme is 443, and a TLS proxy's pool key is prefixed `tls:` so it never
shares a connection with a plaintext one at the same address. A TLS
failure to the proxy is `Attempt::Failed` with the backend's
`ErrorKind::Tls`. `connect_tls` is defaulted, so a third-party engine
with no TLS implements `Dial` as before and refuses honestly.

**Its acceptance test found a defect that predates it.**
`http1::exchange` polls hyper's connection before the request, and a
TLS server that closes without `close_notify` right behind a complete
`Content-Length` response fails that connection in the same poll that
delivered the response — so `UnexpectedEof` replaced a response already
read. TLS in TLS made it land four runs in five, because the proxy's
`close_notify` arrives in the same read as the origin's last record;
direct TLS reaches it only when the FIN is that prompt. The exchange
now asks the request once before surfacing the connection's error, and
`a_response_already_read_wins_over_the_connection_failing_behind_it`
reproduces it with a scripted stream, poll by poll.

**What it cost, measured.** `Native::execute`'s future is unchanged at
20,336 bytes with every feature and 13,200 with none. Stripped release
binaries: `minimal` 525,304 → 525,432 (+128 bytes), `hc` 6,308,424 →
6,377,416 (+68,992, +1.1%) — the TLS backend monomorphised over
`DialStream` beside the socket.

**Still not done**: h2 or ALPN to a proxy, and MASQUE; a separate trust
store for proxies; a client-certificate setter on `Proxy` (the field is
in `ProxyTls`); TLS over a filter's own wrapper. The `Dial` blocker on
stabilising `hclient-proxy` is removed; the stabilisation itself is a
separate decision. *(h2 to a proxy and MASQUE have since arrived, as
tunnels a filter is lent rather than as `Proxy` rules — the next
section. `Dial` grew three methods doing it, all defaulted, which is the
shape a stable seam can grow in.)*

### HTTP/3 leaves through a filter, and the path is quinn's own shape

A request an egress filter carries can now go over HTTP/3. The filter
opens a **datagram path** to the origin — through a SOCKS5 relay, or
through a MASQUE proxy over HTTP/3 or HTTP/2 — and the QUIC arm runs its
handshake over that path instead of a socket. Before this, a filtered
request was TCP-only by construction. The design is
`.notes/superpowers/specs/2026-09-28-egress-datagrams-design.md`. The
ledger of what the plan got wrong is
`.superpowers/sdd/2026-09-28-egress-datagrams/progress.md`, in the
working copy only.

**`DatagramPath` is `Send + Sync`, and the reason is its only consumer.**
The seam demands `Send` of nothing a runtime lends unless something here
needs it. quinn's `AsyncUdpSocket` demands both of every socket, and a
path exists only to be one. It costs embassy nothing: embassy cannot run
HTTP/3 at all.

The trait copies quinn's shape: `&self` methods, `try_send` with
`WouldBlock` as a normal answer, `poll_writable`, `poll_recv`,
`max_datagram_size`. Adapting in either direction is then a forward.
The sketch in the design conversation had a `poll_send`, and it was
amended while planning for exactly this reason. A path is **connected**:
one peer, whole datagrams, no GSO, no ECN, no addresses. That is what a
proxy can honestly carry.

**`Dial` lends three more things, and each refuses by default.**

- `bind_udp` — the runtime's UDP, erased as `BoxUdp`.
- `resolve` — **the proxy's name only**. The origin's name is never
  resolved locally on a filtered path. That rule is the reason a SOCKS5
  relay reply of `0.0.0.0` resolves the *proxy's* name, and why nothing
  else here resolves a name.
- `connect_tunnel` — CONNECT and extended CONNECT to a proxy, over
  HTTP/2 or HTTP/3.

Each default is an `ErrorKind::Unsupported` refusal, so a third-party
`Dial` compiled against the old trait still builds. `EgressFilter` gains
`open_datagrams`, refusing with `Attempt::Unsupported` by default, and it
requires `C::Stream: Send + 'static` — a SOCKS5 path holds its control
connection for the path's whole life, inside a `Send + Sync` value.
`TunnelRequest` is `#[non_exhaustive]` with a builder, by the
three-answer rule: the filter builds it and the transport reads it.
Its `Debug` is written by hand, because the derived one printed `headers`
whole and a filter's `Proxy-Authorization` is what it adds there: a value
marked sensitive, or under either authorization name, prints as
`<redacted>` whether or not the filter remembered the flag.
`FilterSupport::datagrams` was a flag nothing read. It is now the switch
the routing asks.

**In `Native`, the path is a per-connection endpoint over a stand-in
peer.** `PathSocket` wraps a `BoxPath` as a `quinn::AsyncUdpSocket`,
declaring one segment, no ECN and `may_fragment = false`. Each
connection gets its own `quinn::Endpoint`, and its peer is
`192.0.2.1:<port>`. quinn refuses port 0 and unspecified addresses, and
the path ignores the address anyway. TEST-NET-1 can never be a real peer,
which is what makes it safe to report every datagram as coming from it.
A path under 1200 bytes is `Unsupported`, since QUIC cannot run on it.

**A connection over a path starts at 1200 bytes and discovers upwards,
with the path's `max_datagram_size` as the upper bound.** It first
started *at* that size with discovery off, on the argument that only the
proxy knows the ceiling. That is true of a capsule path and false of a
SOCKS relay, whose figure is a guess at a 1500-byte link. Behind a VPN
or PPPoE the link is narrower and an oversized datagram is lost without
a word. The review that found this said such a connection would hang
until its idle timeout; **measured, it does not** — quinn's black-hole
detection runs with discovery off too. What it did was lose 23 of 24
full-size packets before falling back to 1200, and then stay at 1200.
Starting at the floor loses 3–4 overshooting probes instead.
`a_path_narrower_than_it_claims_still_carries_a_large_upload` counts
the drops rather than timing the upload, because the upload completes
either way on loopback.

**What a filtered HTTP/3 connection does differently:**

- The pool key carries the filter's key as `via`, so a direct and a
  filtered connection to one origin never share.
- The origin's HTTPS record is **not** looked up. Looking it up is a
  local query naming the origin. QUIC is chosen through a filter only on
  a `RequireVersion(HTTP_3)` demand or an `Alt-Svc` heard through that
  filter.
- `Connected::remote` is `None`. The stand-in is not an address, and
  reporting it would be the `0.0.0.0:0` answer `Head::version` already
  refused.

**Opening the path and the QUIC handshake spend one `Timeouts::connect`,
and the first build spent it twice.** Review caught it. The new test
`opening_the_path_and_the_quic_handshake_spend_one_connect_bound`
measured **1.80 s against a 1 s bound** before the fix. The path's
opening now runs under what is left of the bound, and the handshake under
what is left after that — the stream path's rule. **A switch to the
stream after that paid the path's time twice**, because the request came
back still carrying the narrowed bound and the switch took everything
since the start off it again; it is handed the caller's bound back now,
and `a_fallback_to_the_stream_after_quic_over_a_slow_path_gets_what_is_left_once`
failed with `ConnectTimedOut(696ms)` before that. That test was itself
a two-sided wall-clock race with about 200 ms of margin, and it reads the
bound now instead: the fixture filter records what each lent context's
`remaining()` said and when, and the stream's figure must equal the
path's less the time the filter saw pass. Spent once, they agree to
microseconds; spent twice, the stream is short by the path's whole
opening, a 400 ms sleep — deterministic in both directions.

**The switch to a stream is remembered, and it reuses `over_quic`'s TCP
tail.** Three things switch the same unsent request to the filter's
stream and record the origin in `H3Failures` with its TTL:

- `Attempt::Unsupported` from `open_datagrams`;
- a QUIC connect over the path that fails;
- a path too small for QUIC.

The switch goes through `after_quic_failed`, the function the direct
fallback and the race already share. So there is one spelling of the
budget rule, not three. Nothing was sent, so this is not a retry in
`RetryKind`'s sense. `RequireVersion(HTTP_3)` does not switch. **A
demand for any other version takes the stream before the advertisement
is asked**, as on the direct path; it used to be routed onto QUIC by an
`Alt-Svc`, refused by the arm, and recorded against the origin in
`H3Failures`, so one `HTTP_11` demand turned HTTP/3 off for every request
after it.
`Attempt::Failed` is final: a proxy that could not be reached will not be
reached by asking for a stream instead.

The memory is keyed by origin alone, where the spec wrote `(filter key,
origin)`. That is equivalent, because `route` is a pure function of the
target, so an origin always takes the same filter. The seam's contract
already forbids the case where it would not be.

**The built-in `Rules` reach their path through the erased route, and
that put sixteen predicates on `Native::http3`.** The plan had `Native`
call `Rules::open_datagrams` concretely with `NativeDial`. That needs
`DialStream<R::Stream, T>: Send + 'static`, proven inside `Native`'s
generic route impl. A generic impl cannot prove it (ruling P1). So the
rules go the way an external filter goes: through `BoxDial` and
`SendEgressFilter::open_datagrams_send`, behind an `open_path` function
pointer that `http3()` installs. `http3()` therefore restates what
`egress()` already carried, plus what the arm's staging needs of the
concrete `H3`:

- `R: H3Runtime`, `R::Socket: Send + Sync + 'static + Debug`,
  `R::Sleep: Send + 'static`, `R::Instant: Send + Sync`;
- `T: QuicTlsConnect<Session = Arc<dyn quinn_proto::crypto::ClientConfig>>`;
- `D: Resolve`, `for<'a> D::Records<'a>: Send`;
- `Staged<R, NoHooks>: Send`;
- `R::Stream: Send + 'static`, `for<'a> R::Connecting<'a>: Send`;
- `T::Stream<BoxIo>: Send + 'static`,
  `for<'a> T::Handshake<'a, BoxIo>: Send`.

Every combination this workspace ships already satisfied `egress()`'s
set, so nothing that built stopped building. The cost is a public
signature that is noisier than what it needs to say.

**SOCKS5 UDP ASSOCIATE is opt-in per proxy, with `Socks5::new().with_udp()`.**
`hclient_proxy::system` never turns it on. A rule without it declares
`STREAM` and behaves exactly as before. A rule learns of UDP through
`Handshake::associate`, defaulting to `None`, so a foreign protocol owes
nothing. **The association opens its own path, and that is what makes the
rules protocol-blind.** The first build had `rules.rs::open_datagrams`
name `drive_associate`, `RelayAddr`, `header_for` and `Socks5Path` — the
§4 exchange, the relay's address, §7's header and the path that frames
with it, all SOCKS5's, sitting in the shared filter. `Association::open_path`
is the whole of it moved behind the value the seam already hands around:
the rules dial the control connection and call the association, and name
none of SOCKS5. **`Associate` is SOCKS5's alone, and sealed** — a foreign
`Handshake` answers `None` or forwards a wrapped SOCKS5 one, a
`compile_fail` doctest pins the seal, and the reason is now that the
opening is this crate's plumbing rather than that the rules would frame
wrongly: the framing moved inside, so a protocol whose association
framed differently could only exist if a constructor were promised, and
none is until one is needed. The gate is
`socks5-udp-stays-in-its-module`, `quinn-stays-in-its-module`'s shape:
nothing outside `socks5_udp.rs` names the four internals, checked in the
failing direction against the `rules.rs` that named them. The sweep after
the move: **633 mutants, 429 caught, 169 unviable, 4 timeouts, 31
missed** — `open_path`'s four new mutants all caught, four former misses
left with the code that moved, and every remaining miss is in the
classes the third audit classified: the platform readers under `system/`
this host never compiles, six `Debug` impls, a test double's `Drop`, and
the known equivalents. The exchange
is sans-io like the rest of the crate: greeting,
RFC 1929 auth, then `CMD=0x03`.

- `REP=0x07` (command not supported) is `Attempt::Unsupported`, which
  switches to TCP and is remembered. Other codes are `Failed`.
- A UDP socket that will not bind is `Failed` as well. Only an
  `ErrorKind::Unsupported` bind — a runtime that lends no UDP — is a
  refusal to switch on; out of descriptors or a port taken is this host
  failing now, and is not remembered against the origin.
- An unspecified relay address in the reply means *the proxy's own*. It
  resolves the proxy's name through `Dial::resolve`; a proxy given as a
  literal skips the lookup.
- End of file on the control connection ends the path. `poll_recv` polls
  the control stream beside the socket, because a SOCKS5 proxy ends an
  association by closing it, and without that a request would wait on a
  dead association until its bound. A test pins this against a real
  relay: the request **errors** within 10 s of the close rather than
  hanging.
- A GRO receive is split by its stride. The runtime's UDP may hand over
  several datagrams as one, noticed while wiring the first path — and the
  `egress-datagram` witness's own path still ignores it, which a
  loopback run does not show.
- A datagram not from the relay's address is dropped. So is one with
  `FRAG != 0`, since fragmentation is deliberately not done. One poll
  drops at most 64 before it wakes itself and answers `Pending`, so a
  flood of off-path datagrams cannot hold the executor's thread.

Against a real relay fixture and a quinn origin, the origin sees only the
relay, and the relay is handed the origin **by name**. The test's
resolver is never asked for the origin at all.

**HTTP/2 tunnels are driven inline, and one waker was not enough.** The
plan had `multiplexed()` spawn the tunnel's h2 connection. Its spawner
is typed to the request driver, `H2Driver<NativeIo<R, T>, H, R>`, and
cannot spawn anything else (ruling T11). So the tunnel's stream owns its
`h2::client::Connection` and polls it from every read and write. The
opener is installed by `egress()`, whose where-clause already proves what
it needs, so **no new bounds**.

That first build parked the connection's socket waker on whichever task
polled last. A reader in one task then stalled for good once a writer in
another went idle — and quinn reads and writes a capsule path from
different tasks, which is exactly that shape. Review found it. The new
test hung at **5 s** before the fix and passes in 0.26 s after. The fix
is a `Fanout` waker that holds the last reader's and the last writer's
wakers and wakes both.

Waiting for the proxy's SETTINGS is a PING. In h2 0.4.19, `poll_ready`
resolves before SETTINGS arrive, so it would read extended CONNECT as
off. A PONG follows the peer's SETTINGS, which makes the wait causal.

**An HTTP/2 tunnel is refused unless the proxy's TLS selected `h2`.**
Offering it in ALPN is not the proxy choosing it: an HTTP/1.1 proxy that
ignores ALPN completes the handshake all the same, and the first build
wrote an HTTP/2 preface into that connection and failed as an opaque
`Connect`. It is `ErrorKind::Unsupported` now, naming what was
negotiated, before a byte of HTTP/2 — and a fixture proxy that reads
until the client closes asserts no preface reached it.

**HTTP/3 tunnels get QUIC connections of their own, so ordinary
SETTINGS do not change.** Each tunnel dials a fresh connection with its
own builder, announcing extended CONNECT and HTTP datagrams, and waits
for the proxy's SETTINGS before sending. Ordinary connections announce
neither, and
`an_h3_tunnel_does_not_change_what_ordinary_h3_connections_announce`
pins that against the same server.

Reading the peer's SETTINGS took `h3`'s
`i-implement-a-third-party-backend-and-opt-into-breaking-changes`
feature. The ordinary client API exposes neither the frame's arrival nor
what it said. In 0.0.8 the feature changes what can be named, relaxes
`non_exhaustive` on two error enums, and turns on no branch. The
dependency is pinned to `0.0.8`, so the breaking changes it opts into
cannot arrive by drift.

**A plain CONNECT over HTTP/3 is impossible with `h3` 0.0.8**, and it is
refused rather than worked around (ruling T14). `ext::Protocol` accepts
only `webtransport` and `connect-udp`. The client also always writes
`:scheme` and `:path`, which RFC 9114 forbids on a plain CONNECT. So a
CONNECT-TCP tunnel is HTTP/2 only. `Http3` alone is `Unsupported` before
a packet is sent, and `Http3ThenHttp2` goes straight to HTTP/2.

**When HTTP/2 can take over, the HTTP/3 attempt is capped at 1.5 s, and
the first build starved the fallback.** Review found an `Http3ThenHttp2`
tunnel to a proxy with no UDP listener spending quinn's whole 30 s idle
timeout on QUIC, or the whole connect bound, which left HTTP/2 nothing.
The cap is `H3_TUNNEL_BEFORE_H2`: about one first PTO off QUIC's guessed
333 ms initial RTT, plus room for one retransmission. Under a connect
bound it is `min(1.5 s, remaining / 2)`. The new test against a
black-holed UDP port failed at its **5 s** guard before the fix and
passes in **1.57 s** after — **30 s to about 1.5 s**. An HTTP/3-only
request still gets everything that is left.

**And that cap is paid once per proxy, not once per tunnel.** A failed
HTTP/3 tunnel is remembered by the proxy's authority, with `H3Failures`'
shape and TTL, on the installed filter; while it is, a tunnel that will
take HTTP/2 starts no QUIC connection, and `Native::network_changed()`
forgets it. A tunnel that will take HTTP/3 alone never reads the memory.
The test counts QUIC connection attempts at a black-holed UDP port by
the destination connection id of their Initials, so a late
retransmission or close from an abandoned attempt is not mistaken for a
new one.

**`hclient-masque` is an experiment, and it exists to shape the seams
rather than to ship.** It is `publish = false` and nothing published
names it. A `Masque` filter speaks RFC 9298 CONNECT-UDP (URI template,
context-id 0 on QUIC datagrams, DATAGRAM capsules on an HTTP/2 stream)
and a plain CONNECT for streams, using only `Dial::connect_tunnel`. The
codecs — template, varint, capsules — are its own. It does not reuse
`hclient-webtransport`'s. Proven end to end through `hclient::Client`,
against real h3 and h2 proxy fixtures:

- HTTP/3 inside HTTP/3;
- HTTP/3 over HTTP/2 capsules;
- CONNECT-TCP over HTTP/2, under both `Http2` and the default
  `Http3ThenHttp2`;
- a proxy refusing CONNECT-UDP falls back to a TCP tunnel, and the
  fallback is remembered.

CONNECT-TCP over HTTP/3 alone is asserted to be `Unsupported` with **0**
QUIC connections accepted, for the `h3` reason above. **No defect turned
up in `hclient-native` or `hclient-proxy`** — every path passed against
the transport unchanged. That is the result the experiment was for.

**One seam gap it did surface: `hclient_proxy::ProxyRefused` cannot be
built outside its crate.** It is `#[non_exhaustive]` with no constructor,
so a third-party filter that wants to report a proxy's status has to mint
its own error, which `hclient-masque`'s `Refused { status }` does. A
constructor would be the three-answer rule applied once more. It is not
added here.

**Two `CapsulePath` liveness bugs were invisible to an echoing
fixture.** A capsule path writes DATAGRAM capsules into a stream that may
take them only in part. What is left is kept as a tail, and somebody has
to own writing it.

1. The first build drained the tail only on send. quinn asks for
   writability only when it has more to send, so the last capsule of a
   burst — often a final ACK — sat there forever. A cross-task test
   caught it as "sent 300 recv 299".
2. The fix registered the receiver only when it happened to see a tail.
   A receiver parked while the tail was still empty then owned nothing,
   and a later partial send was stranded until an unrelated wake. Review
   found it. The cross-task test could not, because its far end
   **echoed**, and the returning bytes woke the receiver every time.
   `the_last_capsule_of_a_burst_reaches_a_proxy_that_only_reads` has a
   far end that never writes back. It failed with the capsule stranded,
   and the fix — the receiver always owns the tail's write interest —
   passes.

The rule to carry: **an echoing fixture hides a stranded write**, because
every reply is a wake nobody earned.

**An outside witness wrote a datagram filter with no edit under
`crates/`.** `.notes/witnesses/egress-datagram` sends a request that
demands HTTP/3 through its own `Forward` filter. With `open_datagrams`
removed, it fails with the seam's default refusal: `Unsupported`, *this
filter opens no datagram path*. It is not `NoDatagramPath`, because the
filter still declares datagrams, and that distinction is exactly the one
the two answers draw. All three egress witnesses pass on the finished
tree.

**The mutation sweep found twenty-nine gaps, all in the new code's edges
rather than in its routing.** Each one was closed by a test, and each
kill was re-applied by hand before it was believed.

- `just mutants hclient-proxy`: 618 tested, 393 caught, 167 unviable, 6
  timeouts, 52 missed.
  - 19 are the platform readers under `system/`, never type-checked on
    this host. On Linux, `detect_platform` already *is* the default.
  - 11 are equivalent: six `Debug` impls, the test double's
    `poll_writable`, a `<=` where no reply is ever five bytes long, a
    short-datagram `<=` that the next check subsumes, and two mutants that
    only make the receive buffer larger.
  - 22 were gaps. Neither `BoxPath` nor `BoxUdp` was ever asked to
    forward a poll: nothing in the crate exercised its own erasure, the
    finding the second audit made about `BoxHandshake`. Nothing pinned
    `Socks5::udp`. The association's refusal of `0xFF` came back under
    the wrong name, and a method it never offered reached an `expect`.
  - The rest were boundaries: a 255-byte name, a path's exact
    `max_datagram_size`, a reserved byte set alone, a datagram shorter
    than a header, which the `==` mutant indexed past. One was the
    receive buffer sized for the longest §7 header in front of a full
    1452-byte payload: five mutants shrank it and truncated that
    datagram, and nothing noticed.
  - The one gap older than this work: `10.0.0.0/+8` and `example.com:+80`
    were refused only by `digits`, since `str::parse` accepts a sign.
  - The timeouts are hangs: `read_some` answering without reading, and
    `percent_decode`'s index running backwards. The two `BoxUdp` ones now
    fail fast on a direct test.
- `just mutants hclient-masque`: 117 tested, 63 caught, 28 unviable, 10
  timeouts, 16 missed.
  - 9 are equivalent: four varint `|` to `^` over disjoint bits, two
    `Debug` impls, a resize `<=`, a size check whose inner path refuses
    the same datagram with the same kind, and a waker slot that clones
    when it need not.
  - 7 were gaps:
    - `accepted` returning `Ok` for a `501`, which end to end is
      indistinguishable, because a capsule path over a refused stream
      fails its QUIC handshake and switches anyway;
    - a bare IPv6 literal left unbracketed;
    - a transport's `Unsupported` read as final;
    - both paths refusing their exact size;
    - `ContextPath` not forwarding writability;
    - a waiter from a second task never replacing the first's.
  - All 10 timeouts are in `CapsulePath`'s liveness code, and each hangs
    a test against its own guard. Those are the hangs the section above
    was about.

**What it cost, measured.** Almost nothing, and the absences are the
point:

- `Native::execute`'s future is **unchanged** at 20,336 bytes with every
  feature and 13,200 with none. The whole datagram route sits behind the
  arm's erasure and a function pointer.
- `hclient-proxy`'s graph is **30 crates** before and after, and its
  manifest did not change.
- `hclient-native`'s is **50** by default and **90** with every feature,
  before and after. The `h3` feature adds code, not crates.
- `hclient-masque` is 32, and it is in no published graph.

(Before-counts were taken from `git archive 11e09836`, never a checkout.)

**Deliberately not done**, from the design:

- a pool of proxy connections for tunnels — one connection per tunnel;
- chaining filters;
- CONNECT-IP;
- UDP through a SOCKS proxy reached over a Unix socket;
- SOCKS5 fragmentation.

**And the real limits that remain, found and not closed:**

- An unspecified relay behind a named proxy resolves the name fresh, and
  may land on a different address from the one the control connection
  reached.
- Tunnel setup is bounded only by what the filter reads off
  `remaining()`, as the other `Dial` methods are.
- `ConnectTiming::dns` reports about zero for a path connection that did
  no lookup.
