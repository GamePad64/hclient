# v0.3 design — the rest of the vertical

**Theme: the two things around a request that are not the request.** How a
client works out where to connect and what to speak (W2, W4), and what a
connection becomes once the exchange is over (W5). Plus the work that
defends claims already made (W1), the debts the first half of this vertical
wrote down (W6), and one external answer that has not changed (W7).

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
  `http-ng-native`'s own — `R::Stream: 'static` at
  `crates/http-ng-native/src/lib.rs:166-171`, there because `NativeBody`
  boxes hyper's `Connection` as a `dyn Future` (`h1.rs:130`).
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
| Does `'static` in the seam shut out embassy? | No. The `'static` is `http-ng-native`'s (`lib.rs:166-171`), and embassy's own `TcpClient`/`TcpClientState` is the bounded buffer pool that satisfies it. Four variants compiled, one runs over a TAP device. | `docs/w7-embassy-research.md` §1.1–1.3 |
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
| The separated `try_send`/`poll_writable` shape fits a second runtime | It was separated because a QUIC endpoint must be able to wait *without a datagram in hand*, which is a claim about quinn, not about `async-io`. The check is whether `async-io`'s readiness API can express it without a dummy datagram | **unverified** |

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

1. **The ECH slot stops being a silent drop.** Of the three TLS backends,
   two refuse a non-`None` `TlsRequest::ech` by name and say why
   (`crates/http-ng-tls-native-tls/src/lib.rs:128`,
   `crates/http-ng-tls-rustls/src/quic.rs:76`). The third — the rustls TCP
   path, which is the default backend — **neither refuses nor honours it:
   it never reads the field.** `Rustls::connect`
   (`crates/http-ng-tls-rustls/src/lib.rs:189-198`) uses `req.server_name`
   and `req.alpn`, and nothing else. That is harmless only for as long as
   `connect.rs` passes `None`, which is exactly what part 2 changes. So
   part 1 comes first, and its content is a decision — refuse, or honour —
   rather than code that can follow afterwards.
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
| A negative cache is Alt-Svc's problem | **False.** UDP/443 is blocked on ~2–5% of networks, which is why the original spec made the broken-backoff mandatory; an HTTPS record advertising `h3` on such a network costs something on every request, whoever produced the record. The negative cache is h3's, and tier 2 inherits it | argued from `docs/superpowers/specs/2026-08-05-http-ng-design.md` §5.6; the size of the cost is **unverified** — a firewall rule dropping UDP/443 and ten timed requests, with and without a race, would settle it |
| What Alt-Svc adds over SVCB is only a positive cache | Partly: also a host/port *change* for an origin, and coverage of origins whose DNS carries no HTTPS RR. How many origins that is, nobody here has counted | **unverified**; one sweep over a list of origins, comparing HTTPS RR presence against the `Alt-Svc` header, settles it |
| An ECH config from DNS would reach a backend that uses it | `grep -n ech crates/http-ng-tls-rustls/src/lib.rs` → no matches | measured here |

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
