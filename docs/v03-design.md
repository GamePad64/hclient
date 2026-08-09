# v0.3 design — the rest of the vertical

**Theme: the two things around a request that are not the request.** How a
client works out where to connect and what to speak (W2, W3), and what a
connection becomes once the exchange is over (W4). Around those: the work
that defends a claim already made rather than adding a feature (W1), a
verdict on every debt the first half of this vertical wrote down (W5), and
one external answer that has not changed (W6).

v0.3 did not begin with this document. **HTTP/3 shipped first**, planned by
[`docs/h3-research.md`](h3-research.md) and accounted for by
[`docs/v03-acceptance.md`](v03-acceptance.md); `http-ng-h3` is in the tree,
over QUIC, multiplexed, with a caller-owned 0-RTT gate. This document plans
what is left, and is written at `38f29d2`.

---

## How to read the premises in this document

**Written first, because it is the reason this document exists at all.**

`docs/v02-design.md` was refuted three times inside one vertical, and a
fourth section was stale for its whole length:

- **§W3 said HTTP/2 had to go through `hyper/http2`.** It cannot: the
  executor bound `Http2ClientConnExec` is *sealed*
  (`hyper-1.11.0/src/rt/bounds.rs:51-52`), so an executor of our own does
  not exist to be written. The `h2` crate is driven directly instead. The
  correction is in the same section, and in `docs/v02-acceptance.md`'s
  "Three things that were found rather than designed".
- **§W7 said embassy's `'static` problem sat in the `TcpConnect` seam.** It
  did not: `embassy_net::tcp::TcpSocket` holds no buffers at all
  (`docs/w7-embassy-research.md` §1.1), and the `'static` was
  `http-ng-native`'s own — `R::Stream: 'static` in `impl Transport for
  Native` (`crates/http-ng-native/src/lib.rs:754-760`), which the research
  traced to `H1Body` storing hyper's `Connection` as a `Pin<Box<dyn
  Future>>`.

  **And that reason has since gone while the bound has stayed**, which is
  worth noticing here rather than being surprised by later: v0.2 W2 replaced
  the box with the concrete `hyper::client::conn::http1::Connection`,
  precisely because `Box<dyn Future>` is never `Send`
  (`crates/http-ng-native/src/h1.rs:29-45`). So what the two `'static`s are
  holding up **today** is *unverified*, and the check is to delete them and
  read the errors. That matters to anyone taking the embassy path further,
  and it is the same shape of staleness this document is written against.
- **§W2 said `Spawn` does not compile on this seam, because `Spawn<F>`
  requires `F: Send + 'static` and the native IO is not `Send`.** Both
  halves were false. `Spawn<F>` declares no bounds; the auto traits come
  from the `Tokio` and `Smol` impls. What actually blocks it is that the
  future is a type parameter of the *trait*, so a bound must **name** it —
  and an `async` block has no name. `Native::with_reaper`
  (`crates/http-ng-native/src/lib.rs:361`) is the shipped counter-example.
- **§W6 said native buffers a streaming request body and WASI takes only
  `Full`.** Neither was true when it was written, and the item stayed
  blocked on that for the whole vertical. The real content was one backend,
  `http-ng-fetch`, and one browser difference nobody had measured.

Those four are not embarrassing because they were wrong — a plan is allowed
to be wrong, and this project's whole review technique is built on finding
out. They are expensive because **they were written as facts**. A premise
stated as "X is impossible" ends the enquiry. A premise stated as "X looks
impossible, and *this* would show otherwise" invites it, and three times in
one vertical the invitation paid for the document.

So, the rule for everything below:

> **Every premise is a claim plus the thing that would refute it.** Where
> the check has been run, the result is given with a file and a line. Where
> it has not, the premise says **unverified** and names what would settle
> it. No work item here rests on an unrun check without saying so out loud.

Each `## W` section therefore carries a **Premises** table: the claim, how
to check it, and what is known today. A row marked *measured here* was run
against this tree while writing; a row marked *unverified* was not.

---

## What is already known, and must not be re-measured

This section exists because three of the refutations above cost a full
re-derivation of something the tree already knew. Each row below is settled;
disagreeing with one is allowed, re-discovering it is waste.

| the question | the answer | where it was settled |
|---|---|---|
| Can library code `spawn` on this seam? | Yes, with a **named** future type. `Spawn<F>` declares no bounds; `Send + 'static` belong to the `Tokio`/`Smol` impls. An `async` block cannot be spawned because it cannot be named — that, not auto traits, is the wall. | `docs/v02-design.md` §W2 correction; `crates/http-ng-native/src/lib.rs:341-375` (`with_reaper`, `Reaper`) |
| Does `'static` in the seam shut out embassy? | No. The `'static` is `http-ng-native`'s (`lib.rs:754-760`), and embassy's own `TcpClient`/`TcpClientState` is the bounded buffer pool that satisfies it. Four variants compiled, one runs over a TAP device. Its *stated cause* is stale — see the bullet above | `docs/w7-embassy-research.md` §1.1–1.3 |
| Can hyper's HTTP/2 client be used without a spawner? | No, and not for reasons of cost: the trait is sealed. Use `h2` directly. | `hyper-1.11.0/src/rt/bounds.rs:51-52`; `crates/http-ng-native/src/http2.rs` |
| Which UTS-46 option word does this project need? | `0x3C` — non-transitional both ways, plus the two context checks. `UIDNA_DEFAULT` is `0`, which is *transitional*, agrees with IDNA2003, and therefore disagrees with us on `straße.de`. Apple's `swift-foundation` uses the same word. | `docs/v02-design.md` "Platform IDN"; `docs/icu-ecosystem-survey.md`; the `OPTIONS` unit test in `crates/http-ng-idn/src/lib.rs` |
| Does `Timer` need an absolute-deadline sleep for QUIC? | No. quinn's `Instant` *is* `std::time::Instant` (`quinn-0.11.11/src/lib.rs:56`), so `reset(i)` is `sleep(i - now)`. The recommendation was measured away rather than implemented. | `docs/v03-acceptance.md` "The seam changes" |
| Can `tokio` leave a `hyper` build? | No, and it never could: `hyper` depends on it unconditionally. What is checkable is that the `tokio` present is the inert `sync` leaf, and CI checks exactly that. | `AGENTS.md` "What's in the dependency graph"; hyper#3428, hyper#3767; `.github/deny/smol-path.toml` per `docs/ci.md` |
| Does `tower`'s concurrency limit bound sockets? | No — the permit is released at the response **head**, so a streaming body holds its connection outside the limit. | `docs/v02-design.md` §W4 correction; `crates/http-ng-tower/tests/concurrency.rs` |
| How is a browser's request-stream support detected? | Behaviourally, never by `'duplex' in Request.prototype`. Firefox 153 does not refuse a stream body — it **replaces** it with the 23 bytes `[object ReadableStream]`, and nothing inside the page can see that. | `docs/v02-design.md` §W6; `docs/measurements/w6-request-streams/` |
| Does dropping a `wasi:http` exchange cancel it? | Yes — the Component Model subtask is cancelled, measured on a live wasmtime host with the server's own socket as the observer. All three v0.1 backends cancel. | `docs/v02-design.md` §W1; `crates/http-ng-wasi/tests/live_roundtrip.rs` |
| Can the 0-RTT acceptance verdict be a field on a handshake result? | No. On QUIC it resolves *after* the response — 8.63 ms against 8.58 ms — so a field could only hold it by waiting for the handshake, which is the round trip 0-RTT skips. | `docs/h3-research.md` §3.2–3.3 |
| Is `full_duplex` allowed to be optimistic? | No, and it is the one field where that is a category difference: over-claiming `streaming_request_body` costs a buffered copy, over-claiming `full_duplex` costs a **deadlock**. Capabilities report the floor. | `docs/v02-design.md` §W3 "DECIDED" |
| Does a variant of a capability enum earn its place? | Only if a caller decision turns on it. `CancelSupport` and `ReuseSupport` each shipped with two values for that reason; `RedirectSupport::Internal` earns a third because `check_supported` *refuses* on it. | `docs/v02-acceptance.md` "The rule this vertical kept applying" |

Two further things are settled and are easy to re-open by accident, so they
are stated as prohibitions rather than facts:

- **Do not reach for a system IDN library on Linux.** An ELF backend was
  built, worked, and was deleted: `dlopen`ing `libicuuc.so.NN` returns
  whatever Unicode version that machine happens to carry, and for IDN a
  Unicode difference is a different host. The rule that survived is narrower
  than "the platform offers the same standard": *static linkage against an
  ABI the OS versions for us*. Windows and Apple qualify; Linux does not.
  (`docs/v02-design.md`, "Superseded on two counts".)
- **Do not add an MSRV job, a CHANGELOG, or a version bump** as tidying.
  Each has a written reason in `AGENTS.md` ("Minimum supported Rust" and
  "Nothing here is published"), and each has been proposed before.

---

## The order, and what it rests on

Stated separately because an order presented as forced is one more premise
written as a fact. **There is exactly one hard dependency in this document**:
inside W2, the ECH decision has to be made before the resolver is allowed to
fill `TlsRequest::ech`, because the moment it does, the default TLS backend
starts dropping an anti-surveillance setting in silence. Everything else
below is preference, and can be reordered by whoever is doing the work.

1. **W1 first** — it is the cheapest item here, it defends an existing claim
   rather than adding a feature, and it reopens the h3 test suite, which is
   where the 0-RTT acceptance gap also lives.
2. **W2 second** — assessed by its own research as a task rather than a
   vertical, and it carries the one ordering constraint above.
3. **W3 next**, because it is the same theme as W2 and consumes one thing W2
   produces: the RFC 9460 client-semantics layer, moved out of
   `http-ng-dns-system` where both resolvers can reach it.
4. **W4 last** — the largest, the only one adding a public seam, and the one
   nothing else depends on, so its slipping costs least. Its first act is a
   measurement that may turn it into a bug fix before it is a feature.
5. **W5 and W6 are not in the sequence.** W5's "a commit" items belong
   wherever their crate is already open; W6 changes documentation and
   touches no code.

---

## W1 — The UDP seam gets its second implementation

**Why first.** It is the cheapest thing in this document and it defends a
claim rather than adding a feature. "The runtime seam is real" is one of the
four things v0.1 set out to prove, and what proves it is
`crates/http-ng/tests/two_runtimes.rs` — the same generic function under two
runtimes, over a real socket. The UDP half of that seam has **one**
implementation (`crates/http-ng-rt-tokio/src/udp.rs`), so `http-ng-h3` has
no two-runtime acceptance and the portability claim for `UdpBind`/
`UdpDatagrams` rests on the crate they were designed against. A seam with
one implementation is a design, not a seam; `docs/v03-acceptance.md` records
this as one of its "deliberately not done" items, and it is the one that
should not stay there.

**Deliverable.** `UdpBind`/`UdpAdoptStd`/`UdpDatagrams` on
`http_ng_rt_smol::Smol`, behind a `udp` feature mirroring
`crates/http-ng-rt-tokio/Cargo.toml:17-19`; plus `http-ng-h3`'s live suite
instantiated under both runtimes the way `two_runtimes.rs` does for HTTP/1,
so that the acceptance is *the same generic code* rather than two similar
test files. The ECN read-back the tokio backend performs
(`crates/http-ng-rt-tokio/src/udp.rs:18-29` — it asks the kernel with
`getsockopt` rather than trusting `quinn-udp`'s documented graceful
degradation) is part of the deliverable, not an extra: a backend that
reports what it *attempted* is exactly the over-claim this seam exists to
prevent.

**Premises.**

| claim | how to refute it | status |
|---|---|---|
| The only thing standing between `Smol` and h3 is the UDP triple | Write `fn assert<R: H3Runtime>()` and instantiate it with `Smol`; the error list must name `UdpBind`, `UdpAdoptStd` and nothing else. `H3Runtime` is `Timer + UdpBind + UdpAdoptStd + Spawn<QuinnTask> + Clone + Send + Sync + 'static` (`crates/http-ng-h3/src/lib.rs:353`), and `Smol` already has `Timer`, `Spawn`, `Blocking`, `TcpConnect`, `TcpAdoptStd` (`crates/http-ng-rt-smol/src/lib.rs:19,46,74,105,154`) | **unverified** — read from the two files, not compiled |
| `async_io::Async<UdpSocket>` gives `quinn-udp` the descriptor it needs | `docs/h3-research.md` §2.4 measured `gso=64 gro=64` through it | measured **in a spike**, not in this crate; the check that transfers it is `crates/http-ng-rt-tokio/tests/udp.rs` run against the new backend |
| It costs no new dependency beyond the one tokio pays | `async-io = "2"` is already a direct dependency (`crates/http-ng-rt-smol/Cargo.toml:12`); only `quinn-udp` arrives, optional, exactly as on tokio | measured here |
| `Spawn` is not a problem for the h3 driver on smol | `QuinnTask` is `Pin<Box<dyn Future<Output = ()> + Send>>` (`crates/http-ng-h3/src/runtime.rs:66`) — a **named** type, so the naming wall does not apply; `Smol`'s impl takes `F: Send + 'static` (`lib.rs:46`) | measured here by reading, not compiled |
| The separated `try_send`/`poll_writable` shape fits a second runtime | The separation was justified by what *quinn* needs — waiting to write with no datagram in hand — and never by what a second runtime can express. Refuted if `async-io`'s readiness API cannot register a write waker without one | **unverified**, and it is the only place this item could turn out to be a seam change rather than a backend |

**Watch for.** Two.

*A second implementation does not kill M14.* `docs/v03-acceptance.md`
records one surviving mutation — `ecn: ecn_is_really_on(&io)` replaced with
`ecn: true` — which is indistinguishable from the truth on a kernel where
ECN works. Two runtimes on the same kernel is still one kernel. What kills
it is the `macos-latest` leg, not a second backend.

*The acceptance must be one function, not two files.* `two_runtimes.rs` is
worth what it is because the same `fetch_once<R>` is instantiated twice, and
its sensitivity was demonstrated by a bound that breaks instantiation on one
runtime and not the other. An h3 pair written as two copies would prove that
both compile and nothing else.

**Fold in here, because it reopens the same suite:** the 0-RTT *acceptance*
path (`docs/v03-acceptance.md`: "0-RTT ACCEPTANCE has not been observed end
to end here; rejection has"). The material exists — the test server already
sets `max_early_data_size = u32::MAX` — and what is missing is the timing
instrumentation the research spike had. It is a test, not a feature, and it
belongs to whoever is already inside `crates/http-ng-h3/tests/live.rs`.

---

## W2 — Discovery, tier 2: the HTTPS record we already fetch

**Why.** `http-ng-h3` chose HTTP/3 by being constructed. That is honest, and
it is tier 1 of `docs/h3-research.md` §4's ladder. Tier 2 — "consume the
`alpn` field that already exists, on the resolvers that already answer" —
was assessed there as *a task rather than a vertical*, and its argument is
the strongest in that research: **the cache with a lifetime already exists
and is not ours.** It is the DNS record's TTL.

The plumbing has been in the tree since v0.2 and is used by nothing:
`SvcbEndpoint` (`crates/http-ng-dns/src/lib.rs:85`) carries `alpn`, `port`,
`ipv4hint`, `ipv6hint` and `ech_config_list`; `Resolve::supports_svcb`
(`:144`) says whether a resolver can ask; the system resolver answers
through `res_query`/`DnsQueryRaw` and the hickory one through its own
queries. `crates/http-ng-native/src/connect.rs:83-91` says in its own module
doc that the plumbing was built and never used, and `:595` passes
`ech: None`.

**Deliverable, in three parts, and the order between them is load-bearing.**

1. **The ECH slot stops being a silent drop. DONE — refuse.** Of the three
   TLS backends, two refused a non-`None` `TlsRequest::ech` by name and
   said why (`crates/http-ng-tls-native-tls/src/lib.rs:129`,
   `crates/http-ng-tls-rustls/src/quic.rs:76`). The third — the rustls TCP
   path, which is the default backend — **neither refused nor honoured it:
   it never read the field.** It does now, and refuses, which unblocks
   part 2: `connect.rs` may fill `TlsRequest::ech` without any backend
   dropping it in silence.

   The decision was taken against the alternative rather than by default,
   and the three obstacles are recorded at the refusal
   (`ech_refused`'s doc comment, `crates/http-ng-tls-rustls/src/lib.rs`),
   measured against rustls 0.23.43: the HPKE suites `EchConfig::new` needs
   exist only in rustls's `aws_lc_rs` provider — `src/crypto/ring/` has no
   `hpke.rs` — and this crate pins `ring` so that it agrees with
   `quinn-proto`'s `rustls-ring`; `ClientConfig::ech_mode` is `pub(super)`
   where `alpn_protocols` is `pub`, so the clone-and-set that makes the
   ALPN cache work has no counterpart and the only entry point is a
   builder running before the verifier is chosen, which `from_config` no
   longer has; and `with_ech` pins TLS 1.3 alone. Honouring ECH therefore
   starts with a second crypto provider and a C toolchain, not with a
   line — which is what W5's "A line, and the reason" predicted, now
   checked rather than assumed.
2. **`connect.rs` consults `lookup_svcb` when `supports_svcb()`**, and uses
   what comes back: the ALPN offer, the port, the address hints as an input
   to Happy Eyeballs, and `ech_config_list` into `TlsRequest::ech`.
3. **A negative cache for a failed attempt** — see the premises below, where
   the assumption that this belongs to Alt-Svc is the one this section
   corrects.

**What is deliberately *not* in the deliverable, and it is the structural
finding of this section.** Tier 2 cannot choose HTTP/3. `SvcbEndpoint::alpn`
containing `h3` is a fact `http-ng-native` can read and cannot act on:
`http-ng-h3` is a different crate with different bounds (`R: UdpBind +
Spawn<QuinnTask>`, `T: QuicTlsConnect`), `Native<R, T, D>` has neither, and
`Client<T>` names exactly one transport type. **There is nowhere in this
codebase for "choose between two protocol stacks" to live.** That is a
transport owning both — a racing transport, with the fallback and the
broken-backoff the original spec sketched (`docs/superpowers/specs/2026-08-05-http-ng-design.md`
§5.6) — and it is a vertical, not a task. So tier 2 delivers everything an
HTTPS record says *except* the protocol choice: hints, port, the ALPN offer
for the TCP stack, and ECH.

That also settles where Alt-Svc goes: it has nowhere to live until that
racing transport exists, which is a better reason for deferring it than the
one v0.2 gave ("a cache with a lifetime, closer to a browser's job").

**Premises.**

| claim | how to refute it | status |
|---|---|---|
| Two shipped resolvers can answer SVCB | `supports_svcb()` is `cfg`-accurate rather than optimistic, and its own test says so (`crates/http-ng-dns-system/src/lib.rs:332`); hickory overrides both methods together (`crates/http-ng-dns-hickory/src/lib.rs:221-225`) | measured here |
| The RFC 9460 client-semantics layer would have to be written | **False, and it is why this is a task.** It exists: `crates/http-ng-dns-system/src/svcb.rs`, 1537 lines with its tests, deciding AliasMode against ServiceMode, `mandatory`, and what "no records" means. It is `pub(crate)` (`:41`), so the deliverable includes moving it where both resolvers and a DoH one can reach it | measured here |
| "The cache with a lifetime is the resolver's, so we need none" | **Half false, and worth checking before relying on it.** `http-ng-dns-hickory` caches and shares one cache across clones (`crates/http-ng-dns-hickory/src/lib.rs:63`). `http-ng-dns-system` caches **nothing** — grep it for `cache`, zero hits — so an SVCB lookup there is a fresh `res_query` per request unless the OS stub resolver happens to cache. What would settle the cost: time two consecutive `lookup_svcb` calls through `SystemDns`, on a machine with `systemd-resolved` and on one without | the absence of a cache in our code is measured here; the cost is **unverified** |
| A TTL is available to cache with | `ResolvedAddr::ttl` exists (`crates/http-ng-dns/src/lib.rs:76`), hickory fills it (`crates/http-ng-dns-hickory/src/lib.rs:105-117`), `getaddrinfo` cannot and leaves it `None` — and **nothing anywhere reads it**: grep `.ttl` outside the resolvers | measured here |
| A negative cache belongs to Alt-Svc, so tier 2 needs none | **False**, and this is what the h3 research's four-item Alt-Svc scope loses when tier 2 is read carefully. UDP/443 is blocked on ~2–5% of networks, which is why the original spec made the broken backoff mandatory — and a record advertising `h3` on such a network produces a failed attempt per request whether the advertisement came from DNS or from a header. The cache of *what failed* is h3's; only the cache of *what was advertised* is Alt-Svc's | argued from `docs/superpowers/specs/2026-08-05-http-ng-design.md` §5.6; the size of the cost is **unverified** — a firewall rule dropping UDP/443 and ten timed requests, with a race and without one, would settle it |
| What Alt-Svc adds over SVCB is only a positive cache | Partly: also a host/port *change* for an origin, and coverage of origins whose DNS carries no HTTPS RR. How many origins that is, nobody here has counted | **unverified**; one sweep over a list of origins, comparing HTTPS RR presence against the `Alt-Svc` header, settles it |
| An ECH config from DNS would reach a backend that uses it | `grep -n ech crates/http-ng-tls-rustls/src/lib.rs` → no matches | measured here, and **since fixed**: the same grep now finds the refusal, and `crates/http-ng-tls-rustls/tests/ech.rs` measures it from the peer's side of a loopback socket — nothing on the wire with `ech: Some(_)`, and, as its control, the server name in the clear with `ech: None` |

**Watch for.** The capability question arrives again, and its answer already
exists: a transport whose ALPN offer is decided per origin, by a record it
has not fetched yet, cannot answer `full_duplex` at construction. That is
v0.2 §W3's floor rule, and it must be *applied* deliberately rather than
inherited by accident (`docs/h3-research.md` §4, fourth bullet).

One trap is specific to this item. An HTTPS record is attacker-influenced
input on the path that decides **which host is contacted** — the same
sentence `http-ng-idn` is written under. `svcb.rs` already refuses a record
that makes an unrecognised key mandatory (`RECOGNISED_KEYS`, `:54`), and
that refusal is the thing to keep rather than relax when the list is
revisited for `dohpath` in W3.

---

## W3 — DoH, and the bootstrap that is the actual problem

**Why here, and why it is not a vertical.** v0.2 deferred DoH in one line:
*"needs an HTTP client to resolve names for an HTTP client. The bootstrap is
the design problem, not the protocol."* That line is the work item. It is
next to W2 because it is the same theme — it is the answer for every
platform whose system resolver cannot ask for an HTTPS record, which is
Windows 10, wasm, and anything behind a stub resolver that drops type 65.
Without it, tier 2 discovery is a Linux-and-macOS-and-Windows-11 feature.

**The bootstrap, in the four shapes it can take.** The decision belongs in
this document; the code does not follow from it automatically, but the wrong
choice is expensive to walk back because it shows in the constructor.

1. **An IP-literal endpoint** — `https://1.1.1.1/dns-query`. No bootstrap at
   all, and `IpLiteralOnly` already exists as the resolver for exactly this
   shape. It needs a certificate with an IP SAN and a verifier that accepts
   one. *Unverified*: whether `rustls-platform-verifier` accepts an IP SAN
   on each of the three platforms in the matrix. One live request settles
   it, per platform.
2. **The system resolver, once, for the DoH host** — then DoH for
   everything else. Cheap, and the failure mode is a cycle (below).
3. **Caller-supplied bootstrap addresses** for the DoH host. Always
   available, always someone else's problem, and the right default for a
   build that has no system resolver at all.
4. **RFC 9461 `dohpath`** — SVCB key 7, which `svcb.rs` deliberately does
   **not** recognise today (`RECOGNISED_KEYS`, `crates/http-ng-dns-system/src/svcb.rs:54`,
   and its comment names `dohpath` as the example of a key that is real,
   registered, and acted on by nothing here). Discovering a DoH endpoint by
   DNS to avoid using DNS is circular for the *first* lookup and useful for
   every one after it; it is not a first cut.

**The cycle, and the claim that it need not be a runtime guard.** A DoH
resolver whose own client resolves through the same DoH resolver is an
infinite regress. The interesting claim is that **the type system already
refuses it**: a `Doh<C>` parameterised by the client it uses makes
`Native<R, T, Doh<Client<Native<R, T, Doh<…>>>>>` an infinitely-sized type,
which is a compile error rather than a stack overflow at run time. This is
*unverified* — nobody has written the type — and the check is to write it
and read the error, which must be a recursion or size error rather than an
accepted definition. It also has a named escape hatch that must be recorded
next to the claim: **any `Arc<dyn Resolve>` erasure reopens the cycle**, so
the guard is a property of not erasing, and a later convenience that boxes
the resolver would remove it silently.

**Deliverable.** `http-ng-dns-doh`: a `Resolve` implementation over any
`Transport`, answering `lookup_ips` and — the point of the crate —
`lookup_svcb` with `supports_svcb() == true`, filling `ResolvedAddr::ttl`
from the answer, with the bootstrap chosen from the list above and stated in
the constructor rather than in prose.

**Premises.**

| claim | how to refute it | status |
|---|---|---|
| The DNS codec would have to be chosen, and hickory-proto is the candidate the spec named | **The codec is already in this tree.** `dns-message-parser = "0.9"` is a normal dependency of `http-ng-dns-system` (`Cargo.toml:18`), decodes HTTPS RR (type 65 — `rr/enums.rs:203`, `rr/draft_ietf_dnsop_svcb_https.rs`), and has an `encode()` on its message type. `svcb.rs`'s module doc records why it was chosen: no `unsafe` in its `src`, and name decompression that terminates by tracking visited offsets | measured here |
| Reaching for `hickory-proto` instead is a small cost | **No.** `cargo tree -p http-ng-dns-hickory -e normal` is **92 unique crates** against **25** for `http-ng-dns-system`, and it brings back `url`, `idna` and the ICU data crates — the graph `http-ng-proto` spent a whole task removing. `hickory-proto`'s `std` feature includes `url/std` | measured here |
| `dns-message-parser` can encode a *query*, not only decode a response | The decode path is what `http-ng-dns-system` uses; the encode path is used by nothing here. Check: build a `Dns` with one question for `A`/`HTTPS`, call `encode()`, and compare the bytes against `dig +qr` | **unverified** |
| A cert with an IP SAN validates through the default verifier | One request to an IP-literal DoH endpoint, on each of the three matrix platforms | **unverified** |
| The cycle is a compile error rather than a runtime guard | Write the recursive type and read the error | **unverified**, and the escape hatch (`dyn Resolve`) is named above |
| DoH over HTTP/1.1 is adequate | RFC 8484 says HTTP/2 SHOULD be used; our h2 is a feature and off by default, and the pool hands an h2 connection out **exclusively** (`crates/http-ng-native/src/pool.rs:155`), so h2 buys nothing for concurrent queries here either. The honest consequence is one connection per outstanding query on the default build | reasoned here from measured facts; the *latency* cost is **unverified** |

**Watch for.** Three.

*A resolver's client is not the user's client.* It must not carry the
caller's cookie jar, redirect policy, or `Authorization`. Whatever
constructor exists has to make the shared-`Client` case the awkward one
rather than the default, or the first bug report is a cookie sent to a DNS
provider.

*The TTL is the first consumer of a field nothing reads.* `ResolvedAddr::ttl`
has been filled by hickory and read by nobody since v0.2 (W2's premises).
DoH is the first place a cache could be ours rather than the OS's, and it is
also the first place where getting it wrong is a stale-address bug rather
than a redundancy.

*The `wasm` case is the one that would justify the whole crate, and it is
the one nobody can test cheaply.* In a browser the transport is `fetch`,
which cannot see DNS at all — so DoH is the only name resolution a wasm
build could ever have. Whether that is useful, given that `fetch` resolves
names itself, is a design question this document does not answer: it would
matter for a wasm build that wants an HTTPS record's `alpn` or ECH, and
`fetch` gives access to neither.

---

## W4 — WebSocket, and the seam it is actually asking for

**Why last.** It is the largest item here, it is the only one that adds a
seam to the public surface, and nothing else in this document depends on it
— so if the vertical runs short, this is the item whose slipping costs
least. It is also the one the original spec called "what nobody else has"
(`docs/superpowers/specs/2026-08-05-http-ng-design.md`, v0.3 roadmap), and
the one whose central decision that spec asked to be taken *before the pool
architecture is frozen*. The pool froze in v0.2 W2 without it. The cost of
that is measurable rather than hypothetical, and it is written below.

`Capabilities::upgrade` exists (`crates/http-ng-core/src/caps.rs:277`, field
at `:492`) and is `UpgradeSupport::None` in every backend and in the default
(`caps.rs:522`, `http-ng-native/src/lib.rs:259`, `http-ng-wasi/src/lib.rs:194`,
`http-ng-fetch/src/caps.rs:407`, `http-ng-h3/src/lib.rs:342`). Four variants,
zero uses: exactly the shape v0.2's rule was written against.

### The finding that decides the seam

**The browser cannot implement "upgrade" at all, and can implement
"WebSocket" perfectly well.** `http-ng-fetch` says so where it sets the
capability: *"`WebSocket` in the browser is a wholly separate global,
unreachable from a `fetch`-shaped `Transport`"* (`crates/http-ng-fetch/src/caps.rs:405-406`).
On Apple platforms the same is true one level down —
`NSURLSessionWebSocketTask` is **message-framed** and hands back no byte
stream. `wasi:http` has no upgrade either: the protocol has an
`HTTP-upgrade-failed` error code and no mechanism.

So a seam shaped as "give me back the socket" is implementable by exactly
one of the four backends here, and among the three it shuts out is the
browser — the target whose inclusion is this project's whole claim.
**The seam to add is a WebSocket seam — message
oriented, `Stream<Item = Result<Message>> + Sink<Message>` — and h1 upgrade
is an implementation detail underneath it on native.** That is the spec's
§5.7 conclusion, and it is worth restating because `UpgradeSupport`'s
existence makes the other shape look like the intended one.

Two consequences follow immediately, and both are decisions rather than
code:

- **The seam expresses itself by being implemented, not by a capability
  field.** This project already has that pattern and wrote it down for
  `TcpAdoptStd`: *"a backend that cannot adopt a `std::net::TcpStream`
  should say so by not implementing the trait, which is already how the seam
  expresses it"* (`docs/w7-embassy-research.md` §1.4). A separate trait, as
  `QuicTlsConnect` is separate from `TlsConnect`, for the same reason given
  there: when the method intersection with the existing trait is empty, an
  adapter between them type-checks with an empty body, which is worse than a
  compile error.
- **`UpgradeSupport` must then justify itself or go.** If WebSocket is a
  trait, no caller decision turns on the field, and v0.2's rule says a
  variant exists only if one does. Either it becomes real for a *different*
  reason — a raw h1 upgrade or `CONNECT` tunnel exposed to callers, which is
  a proxy feature nobody has asked for — or it is deleted. Deciding that is
  in scope for this item; leaving four unused variants in the capability
  registry is not.

### What native needs from hyper, and the trap in it

hyper gives exactly what is required, without a `Send` bound:
`Connection::poll_without_shutdown` and `into_parts`, returning
`Parts { io, read_buf }`, are bounded by `T: Read + Write + Unpin`, `B: Body`
alone (`hyper-1.11.0/src/client/conn/http1.rs:68-113`; `Parts` is
`#[non_exhaustive]` at `:32-45`).

What must **not** be used is `hyper::upgrade::{on, Upgraded}`: `Upgraded`
holds `Rewind<Box<dyn Io + Send>>` (`hyper-1.11.0/src/upgrade.rs:66-67`), so
it puts a `Send` bound on this crate's IO — "single-threaded runtimes shut
out", the thing the crate exists to avoid, and the same objection that
disqualified `hyper/http2` in v0.2 W3.

**The trap, in hyper's own source.** Polling a plain `Connection` as a
`Future` to completion after a 101 returns `Poll::Ready(Ok(()))` and throws
the upgrade away:

```rust
// hyper-1.11.0/src/client/conn/http1.rs:313-320, inside `impl Future for Connection`
proto::Dispatched::Upgrade(pending) => {
    // With no `Send` bound on `I`, we can't try to do
    // upgrades here. …
    pending.manual();
    Poll::Ready(Ok(()))
}
```

"The exchange completed successfully" and "the upgrade was destroyed" are
therefore the *same observation*, and `crates/http-ng-native/src/h1.rs`
polls `Connection` as a `Future` precisely this way (its module doc,
"What happens when `Connection` finishes first, or fails"). A 101 must be
detected by **status**, before the connection is polled to completion.

### The check to run before any code is written — RUN, and the answer is no

Nothing in `http-ng-native` mentions 101 or Switching Protocols — grep the
crate. So today, a server that answers `101` to an ordinary request produces
a response with a body that ends immediately, and `checkin_for`
(`crates/http-ng-native/src/lib.rs:527`) hands the connection back to the
pool when the body ends cleanly. **If that is what happens, the pool is
being poisoned with a connection that is no longer speaking HTTP, and that
is a bug today rather than a WebSocket feature.**

It is not what happens. `crates/http-ng-native/tests/switching_protocols.rs`
is the experiment this paragraph asked for, with the accept count and the
upgraded socket as the observer, in both shapes — a `101` on a fresh
connection and a `101` on one the pool really did hand out. Neither reuses
it, and no HTTP is ever written onto a socket that has stopped speaking it.
**So this item starts with a feature, not with a fix.**

The reason is worth carrying into the work rather than only the verdict,
because it was not the obvious one. hyper decodes a `101` as a zero-length
body with `keep_alive = false` and `wants_upgrade`
(`proto/h1/role.rs:1273`, `:1169-1177`), so its dispatcher finishes inside
the very `Connection::poll` that delivers the head — and `h1::exchange`
polls the connection *before* the request future, so the response arrives
with `conn_done` already `true` and the body is built carrying neither the
connection nor a check-in token. But that is only the first of **five**
places where "this `Connection` has finished" is asked: with it disabled,
the upgraded connection does reach the pool, and the request still does
not, because `is_reusable` polls the connection and then asks
`SendRequest::poll_ready`, which a finished dispatcher answers `Err` to for
ever. Four of the five can be removed together and the outside observation
does not change. The enumeration is in that test file's module doc, and the
mechanism is in `h1.rs`'s.

What this does **not** settle is anything about the upgrade itself. The
paragraph above about `pending.manual()` stands unchanged: the upgrade is
destroyed, and "the exchange finished" and "the upgrade was thrown away"
are the same observation. Detecting a `101` by status before polling
`Connection` to completion is still the requirement for W4 — it is just a
requirement of the feature rather than the fix for a live defect.

### WebSocket over HTTP/2, and why it is out of scope

RFC 8441 extended CONNECT is reachable from where this crate already
stands, and *more* reachable than the original spec thought:

- the `:protocol` pseudo-header is `h2::ext::Protocol` in the request's
  extensions (`h2-0.4.15/src/proto/streams/streams.rs:227`);
- the gating setting is readable from **`SendRequest`**, not only from
  `Connection` — `is_extended_connect_protocol_enabled()` at
  `h2-0.4.15/src/client.rs:547`, in the `impl<B> SendRequest<B>` block at
  `:353`. The spec's first trap ("the flag lives only on `Connection`, and
  hyper-util spawns it into a task and loses it") was a fact about
  hyper-util's architecture, and does not apply to a crate that holds
  `SendRequest` in its own pool;
- and the check is genuinely ours to make: `send_request` removes the
  `Protocol` from the extensions and sends it **without consulting the
  setting** (same file, `:227` onwards), while RFC 8441 §3 forbids sending
  `:protocol` unless the setting was received.

And yet it buys nothing here, for a reason that belongs to the pool rather
than to h2: **an h2 connection is checked out exclusively, one stream at a
time** (`crates/http-ng-native/src/pool.rs:155-172`, which states the policy
and the `Spawn` argument behind it). A WebSocket over h2 would therefore
occupy an entire connection for its lifetime — exactly what it does over
h1, plus framing overhead. RFC 8441 exists to put a tunnel *alongside*
ordinary requests; with exclusive checkout there is no alongside.

Lifting the exclusivity needs a connection driven by somebody who is not a
request future, which is the decision `http-ng-h3` already took and wrote
down (`docs/v03-acceptance.md`, "HTTP/3 requires `R: Spawn`, and connections
are shared"). So: **WebSocket over h1 in v0.3; WebSocket over h2 only
together with a multiplexing pool**, which is its own work item and carries
W1-of-v0.2's cancellation rule with it, because the pool policy is what
currently discharges that rule for free.

This is what the spec's "decide this before the pool architecture is frozen"
was for. The decision was not taken, the pool froze, and the price is one
deferred feature rather than a rewrite — worth recording as the actual size
of that miss.

**Premises.**

| claim | how to refute it | status |
|---|---|---|
| hyper hands back the IO without a `Send` bound | `into_parts`/`poll_without_shutdown` bounds at `hyper-1.11.0/src/client/conn/http1.rs:68-113` | measured here |
| `hyper::upgrade::Upgraded` cannot be used | It is `Rewind<Box<dyn Io + Send>>` (`src/upgrade.rs:66-67`) | measured here |
| Polling `Connection` to completion destroys an upgrade | `src/client/conn/http1.rs:313-320` | measured here |
| A 101 today poisons the pool | The loopback experiment above | **run, and the answer is no** — `crates/http-ng-native/tests/switching_protocols.rs`. The connection is never offered to the pool (`exchange` sees it finished before the response exists), and four further checks would each stop it alone; measured by disabling them |
| RFC 8441 is reachable through the `h2` crate we already drive | `h2-0.4.15/src/client.rs:547`, `proto/streams/streams.rs:227` | measured here by reading; no request has been sent |
| WS-over-h2 is worth having under the current pool | It is not, and the reason is `pool.rs:155-172`, not h2 | argued here |
| A framing library exists that does not force a runtime | `tungstenite` 0.30's `WebSocketContext::read/write` take the stream per call (`tungstenite-0.30.0/src/protocol/mod.rs:449,491`) — a state machine separate from the socket, but typed against `std::io::{Read, Write}`. `async-tungstenite` 0.35 bridges it with `AllowStd` + `AtomicWaker` over `futures_io`, and its only `unsafe` is in `gio.rs` | measured here |
| That bridge can be built here without `unsafe` | `crates/http-ng-rt/src/futures_io.rs` already bridges `futures_io -> hyper::rt` unsafe-free, through a scratch buffer, and documents the one-copy cost. WebSocket needs the **reverse** direction, which that file does not have | **unverified**; writing it under `#![forbid(unsafe_code)]` is the check |
| The handshake needs a dependency | `Sec-WebSocket-Accept` is SHA-1 of the key plus a fixed GUID, base64. `tungstenite`'s `generate_key`/`derive_accept_key` sit behind its `handshake` feature, which pulls `http`, `httparse`, `sha1` and `data-encoding` | measured here; which way to go is a decision, not a fact |

**Watch for.** Four.

*The capability trap, for the fifth time.* "The browser has WebSocket" is a
fact about the environment. What may be declared is what **this transport**
does. v0.1 caught this four times and v0.2 once more.

*No foreign type in the public API.* `Message` must be ours — the spec lists
this as risk 7, with `Upgraded` and `h3::quic::*` as the examples. A
`tungstenite::Message` in the seam would put a framing library in every
downstream crate's public surface, and would be unimplementable by the
browser backend, which has its own message type.

*Reconnection is not this seam's job.* `SseStream` and
`ReconnectingSseStream` are the precedent: a stream, and a separate type
that opens a fresh one (`crates/http-ng/src/sse.rs`).

*Masking, and the `Close` handshake, are the parts a naive implementation
gets wrong quietly.* A client MUST mask; an unmasked frame is a protocol
error the server closes on, and it is invisible to a test that only checks
that a message arrived. Whatever is built needs a server that rejects an
unmasked frame, not one that tolerates it.

---

## W5 — The debts, decided one by one

`docs/v02-acceptance.md` and `docs/v03-acceptance.md` between them record
about two dozen things deliberately not done or not checked. A list like
that decays into a backlog unless somebody says, per item, whether it is
work. This section says so. Three verdicts: **promoted** (it became a work
item above), **a commit** (small enough that it is not an item), and **a
line** (not doing it, and the reason, so that nobody "fixes" an absence that
is the decision).

### Promoted

| debt | where it went |
|---|---|
| No smol UDP backend, so h3 has no two-runtime acceptance | **W1** |
| 0-RTT acceptance never observed end to end (rejection was) | **W1**, same suite |
| No HTTPS/SVCB discovery | **W2**, minus the protocol choice, which has nowhere to live |
| No ECH on the QUIC path | **W2** — but only the half that is dishonest. The QUIC refusal stays a refusal; what gets fixed is the rustls **TCP** path, which neither refuses nor honours |

### A commit, not an item

**`http-ng-h3`'s `connect` timeout.** All three `TimeoutSupport` fields are
honestly `false`; `docs/v03-acceptance.md` calls `connect` "the cheapest to
add and not added, because a declaration and its enforcement belong in the
same change". That rule is v0.2 W4's, it is already written, and the change
is one commit that moves both together.

**WASI's `ReuseSupport::Supported` has an observer after all.**
`docs/v02-acceptance.md` lists it as the one declaration no test stands
behind, on the grounds that *"from inside the sandbox we cannot watch
sockets"*. The observer does not have to be inside the sandbox, and in this
suite it already is not: `crates/http-ng-wasi/tests/live_roundtrip.rs` runs
the mock server as a plain `std::net::TcpListener` on a **host** thread and
the guest as a wasmtime subprocess — that division is the file's whole
design, and it is what made the cancellation measurement possible. Counting
*accepted* connections across two guest requests is the same observer
`crates/http-ng-native/tests/pool.rs` uses. The claim it yields is narrower
than the capability ("this wasmtime host reuses"), and narrower than the
capability is still infinitely more than nothing.

**`http_ng_idn::testing::bundled` is called by nothing — and cannot be
useful as written.** Zero callers: `crates/http-ng-idn/tests/differential.rs`
uses `platform`, `platform_name`, `selected` and `policy_over`, and nothing
else in the workspace names it. The reason is structural rather than an
oversight. `build.rs` sets `idna_backend` only when the target has neither
ICU nor Foundation (`crates/http-ng-idn/build.rs`, the last three `if`s),
and `idna` is a dependency only under
`cfg(not(any(windows, target_vendor = "apple")))` (`Cargo.toml:75-83`). So
on every target where `testing::bundled` exists, it *is* the backend
`domain_to_ascii` already uses — and the crate's own doc says what that
makes a comparison worth (`src/lib.rs:776`: "on a target with the bundled
backend that function *is* `idna::domain_to_ascii_cow`, so comparing it with
`idna` compares `idna` with itself"). Two honest options: delete it, or make
`idna` available alongside the platform backends — which costs the ICU
tables back on Windows and macOS, i.e. exactly the saving the crate exists
for. **Delete it.**

### A line, and the reason

- **No streaming request body and no full duplex on `http-ng-h3`.** HTTP/3
  does both; `execute` writes the whole body and then reads the head, and
  the capabilities describe the implementation. The technique for lifting it
  without touching `Transport` is already written down twice — carry the
  unfinished write future into `Self::Body` and poll it from `poll_frame`,
  with the three costs `AGENTS.md`'s vertical-1 section enumerates. What
  would move it up the list: a consumer that needs duplex, or a wish to give
  the floor rule its first case where `full_duplex` is genuinely `true`
  somewhere.
- **No ECH implementation, on either path.** rustls builds ECH through a
  different builder entry point, so it is a second construction path and a
  third cache dimension. A typed refusal is the honest state, and it now
  **is** the state on all three backends rather than two (W2 part 1, done).
  The estimate was right and one item short: past the builder there is also
  no HPKE at all in the `ring` provider this crate pins, so honouring ECH
  starts by adding `aws-lc-rs`. See `ech_refused` in
  `crates/http-ng-tls-rustls/src/lib.rs`.
- **No reaper for dead pooled h3 connections.** Same shape and same reason
  as v0.2 W2's HTTP/1 pool: a connection the peer closed is dropped at the
  next checkout.
- **`Native::with_reaper` after `pool()` watches a discarded pool.** Already
  documented where the code is, as "Start it last"
  (`crates/http-ng-native/src/lib.rs:341-348`): `pool()`/`without_pool()`
  install a *new* pool, and a reaper started earlier holds a `Weak` to the
  old one and ends quietly. The type-level fix is a typestate builder, which
  is a large change to a constructor for a mis-ordering that has not
  happened, and `with_reaper` already takes the `PoolConfig` itself so that
  `pool()` need not be called beside it. Revisit if anyone hits it.
- **GSO/GRO/ECN are unverified on macOS and Windows, and mutation M14
  survives.** The tests assert the *relationship* between what a socket
  claims and what it delivers, which holds on any kernel; asserting `64`
  would be flaky by construction. M14 is killable on `macos-latest` and has
  not been observed killed. One run of
  `ecn_is_reported_from_the_kernel_on_a_dual_stack_socket_too` there, with
  the mutation applied, settles it.
- **A TLS ticket issued over TCP offered to a QUIC handshake.** Removed by
  construction (a separate session store) rather than answered. What would
  answer it is one server serving both with a shared ticketer.
- **`two_requests_share_one_connection` failed once and was not
  reproduced.** Kept in the record with the observed accept count printed,
  so the next occurrence says whether the connection was replaced.
- **The h3 idle A/B's timings are loose on purpose.** They would rather pass
  on a loaded runner than measure anything precisely.
- **`http-ng-fetch`'s `ReuseSupport::Supported` still has no observer**, and
  unlike WASI's it cannot get one from inside the suite: every line of a
  `wasm-bindgen-test` runs in the browser, so a listener cannot be started
  by the test. What would settle it is a server started by CI outside the
  browser, counting accepts — a job-shaped change, not a test-shaped one.
- **Three `http-ng-idn` gaps stay open**: the ICU acceptance gate covers
  *content* and not *presence* (killing that mutation needs a machine where
  ICU is present but wrong); the Windows 1703 floor is checked by nothing;
  and whether `CoInitializeEx` must precede `uidna_openUTS46` is unverified.
  All three need a runner that no matrix currently has, or a deliberate
  fault injection.
- **The cookie gaps** — no `CookieJar<P>` through `ClientBuilder`, no
  per-request control, `SameSite` parsed and not enforced — keep the reasons
  `docs/v02-acceptance.md` gives. The third is not fixable at all outside a
  browsing context.
- **The concurrency limit bounds requests, not sockets.** Bounding sockets
  needs a limiter that carries its permit into the response body — the shape
  `http_ng::Deadline` and `http-ng-wasi`'s `Body` both use — owned by
  `http-ng-tower` rather than borrowed from `tower`.
- **`MockTransport` reports `Capabilities::none()`**, which is honest: its
  `execute` completes synchronously and there is nothing to cancel.
- **A `tower::buffer::Buffer` in the stack breaks the cancellation
  contract**, and such a stack must declare `None`. Written down where the
  stack is built.
- **No per-request `total` override.** A per-handle bound costs one `Arc`
  bump and covers most of the same ground; a per-request setter would need a
  runtime flag existing only to be refused.
- **Two `getaddrinfo` slots instead of one, and no RFC 6724 §6 sorting.**
  The second is not deferred work but a missing capability: full destination
  ordering needs Source Address Selection, i.e. the routing table, which no
  trait here exposes. A partial implementation would look like compliance
  without being it.

---

## W6 — `no_std`: the answer is still no, and the shape of the no is now measured

**Why it is in this document at all.** `AGENTS.md` states the obstacle as
one external `compile_error!` and moves on. That is a claim about someone
else's crate, made once, and it is exactly the kind of premise this vertical
learned to re-check. It was re-checked while writing, and three things came
out of it — one of which says `AGENTS.md` is wrong today.

**The blocker is unchanged.** `http` 1.5.0 is the newest release
(`cargo info http`), and it still carries the commented-out
`#![cfg_attr(not(feature = "std"), no_std)]` at `src/lib.rs:158` with
`compile_error!("\`std\` feature currently required…")` on `:160`. Two pull
requests are open and neither has been reviewed by a maintainer:
[#749](https://github.com/hyperium/http/pull/749) (opened 2025-01-31, 517
additions and 220 deletions across 22 files, currently conflicted — its only
comment is this project's owner's, dated 2026-08-08) and
[#740](https://github.com/hyperium/http/pull/740) (opened 2025-01-02, the
`core::error` approach, MSRV 1.81 for the `no_std` path only). Check:
`gh api repos/hyperium/http/pulls/749`.

**Correction one: it is ten crates, not seven.** `AGENTS.md` says
`http::{Request, Response, HeaderMap, Uri, Method}` appear in the public API
of seven crates here. `http` is a normal dependency of **ten**: `http-ng`,
`-core`, `-cookie`, `-fetch`, `-h3`, `-mock`, `-native`, `-proto`, `-tower`,
`-wasi`. Check: `grep -rn '^http *=' crates/*/Cargo.toml`. Whether all ten
*expose* it is a narrower question and was not counted — it does not need to
be, because the `no_std` obstacle follows from the dependency and not from
the exposure.

**Correction two: there is a second external blocker, and it is in worse
shape.** `http-body` 1.1.0 is not `no_std` either — its `src/lib.rs` opens
with `use std::convert::Infallible; use std::ops; use std::pin::Pin; use
std::task::{Context, Poll};` and declares no `#![no_std]` — and it is a
dependency of most of the same crates. Unlike `http`, it has **no open
request at all**: 25 open issues and pull requests, not one of them about
`no_std`. Check: `gh api 'repos/hyperium/http-body/issues?state=open&per_page=100'`.
`AGENTS.md` does not mention it.

**Correction three, and it is the useful one: our own code is not the
obstacle.** Measured on this tree:

- `http-ng-proto` — the sans-io crate, the one that would matter for a
  microcontroller — uses exactly four `std::` paths outside its tests:
  `collections::VecDeque`, `net::IpAddr`, `borrow::Cow` and (in a test only)
  `time::Instant`. Three of those are `alloc`/`core` items with a stable
  home (`core::net::IpAddr` since 1.77). Check:
  `grep -rho 'std::[a-z_]*' crates/http-ng-proto/src`.
- `http-ng-core` adds `error::Error` — stable in `core` since 1.81, which
  this project's "latest stable" MSRV comfortably clears — plus `fmt`,
  `task`, `pin`, `future` and `sync::Arc`.

So the honest statement is not "we depend on `std`" but "**two crates in
hyper's own family do, and until they stop, nothing downstream can**". That
is worth having written down precisely, because it changes what the next
person does: not an audit of this workspace, but one look at whether
hyperium has merged #740 or #749.

**Deliverable.** Not a feature, and not a flag — one would not build. Three
corrections to `AGENTS.md`, the measurements above recorded once, and
nothing in `crates/`. Everything else about constrained targets is already
covered by what exists: `NoTls`, `IpLiteralOnly`,
`crates/http-ng-native/examples/minimal.rs`, and — for parts that do have
`std` — `http-ng-rt-embassy`, which is where the microcontroller story
actually lives.

---

## Decisions needed before work starts

Five, each named where it belongs above, gathered here because each one
shows in a signature and is expensive to walk back.

1. ~~**ECH on the rustls TCP path: refuse, or honour.**~~ (W2, part 1.)
   **Decided: refuse**, and done — so `connect.rs` may now pass a real
   `ech_config_list`. Honouring it is not a second construction path and a
   third cache dimension only; it is also a second crypto provider, because
   the `ring` provider this crate pins has no HPKE for `EchConfig::new` to
   use. Measured at the refusal site.
2. **Does `UpgradeSupport` become real, or go?** (W4.) If the WebSocket seam
   is a trait, no caller decision turns on the field, and v0.2's rule says
   it should not exist. The alternative is that it describes something else
   — a raw upgrade or `CONNECT` tunnel exposed to callers — which nobody has
   asked for.
3. **Where the WebSocket seam lives.** The trait has to be somewhere every
   backend can implement it, which argues for `http-ng-core` beside
   `Transport`; the framing implementation has to be somewhere a browser
   build never links, which argues for a leaf crate. The `http-ng-h3`
   precedent (its own crate, because of bounds and because Cargo's features
   are additive) applies to the second half and not to the first.
4. **The DoH bootstrap default.** (W3.) IP-literal endpoint, system
   resolver once, or caller-supplied addresses. It shows in the
   constructor.
5. **Whether a protocol-racing transport is the next vertical.** W2 stops
   short of choosing HTTP/3 because nothing owns both stacks, and Alt-Svc
   has nowhere to live for the same reason. If that transport is imminent,
   W2's shape should anticipate it; if it is not, W2 should not pretend to.

---

## Not in v0.3, and why

**Alt-Svc**, and the question is narrower than it was. `docs/h3-research.md`
§4 measured that it is not needed for the first flight — `SvcbEndpoint::alpn`
already carries `h3`, and the record's TTL is a cache nobody here has to
write. W2 narrows it twice more: the negative cache turns out to belong to
h3 rather than to Alt-Svc, so that is not a reason for it either; and what
is genuinely left — a positive cache keyed by origin, learned from a
response header, covering origins whose DNS carries no HTTPS RR, plus the
host/port change — **has nowhere to live until something owns both protocol
stacks.** That is a better reason to defer it than "closer to a browser's
job", which is what v0.2 said.

**A protocol-racing transport** — QUIC against TCP with a delay, the
fallback, and the broken backoff (the original spec's §5.6). Named here
because W2 and Alt-Svc both stop at its edge. It is a vertical: it owns two
transports, it decides `Capabilities` for a protocol it has not chosen yet,
and it is where a per-origin cache would finally have an owner.

**WebSocket over HTTP/2**, with the multiplexing pool it actually needs.
Deferred together, because RFC 8441 buys nothing while an h2 connection is
handed out exclusively, and because lifting that exclusivity moves v0.2 W1's
cancellation rule from "free" to "owed" (`crates/http-ng-native/src/pool.rs:184-189`
says so where the policy is).

**HTTP/3 streaming request bodies and full duplex** — W5's first line, with
the technique already recorded and the two things that would move it up.

**An ECH implementation** on either path; the typed refusal is the honest
state, and after W2 it is the state everywhere rather than in two places out
of three.

**`no_std` as a feature** — W6. It would not build.

**A compio backend, and event hooks / connection observability.** Both were
on the original spec's v0.3 list. Neither has a consumer asking, and the
evidence a third runtime would buy is arriving more cheaply elsewhere: W1
gives the seam its second UDP implementation, and `http-ng-rt-embassy`
already gave it a third executor model with a different set of things it
cannot do.

**`http-ng-rmcp`, and the `act` acceptance.** The spec put "the second
verification loop" in v0.2 and made both a condition for 1.0; neither
happened, and this document does not schedule them either. Worth saying why
they keep being worth more than they look: a consumer refutes capability
claims in a way a test cannot, because it was not written against them. Of
everything deferred here, this is the item most likely to find something
wrong with a claim the acceptance documents currently call proven.

**Publishing, versions, a CHANGELOG.** `AGENTS.md` says why, and the trigger
is the owner's.
