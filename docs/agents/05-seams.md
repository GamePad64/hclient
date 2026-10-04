# The seams

> Part of the [AGENTS.md](../../AGENTS.md) record, split 2026-10-05. The root
> file keeps the operational rules and the index of parts. Sections below
> keep their original headings, and a "this file" in the text named the
> whole record when it was written.

### A seam review, and what a scan of thirty-five traits found

Asked to review the architecture and the seams. The inventory is
reassuring and the two findings are both of one kind.

**The shapes are consistent where it matters.** `Transport` keeps an
RPITIT, so nothing can demand `Send` of it and `hclient-rt-embassy` can
exist; every seam beneath it — `Resolve`, `TcpConnect`, `TlsConnect`,
`Blocking`, `Timer` — carries **associated future types**, so each
implementor answers for its own auto traits. The exceptions are the two
newest, `StagedConnect` and `Prefetch` (a private method now), which are
written as RPITITs, and
the cost of that was measured rather than assumed: it is why the QUIC arm
reaches `Native` through `http3::arm`'s erasure, and the erasure is
`Send` anyway.

**Every defaulted constant has the reader it was designed for**, which is
the `UpgradeSupport` question asked of the pattern that replaced it:
`reports_alpn` is read by `may_speak_h2`, `applies_ech` by the connector,
`IpcConnect::IPC_SUPPORT` by `unix_socket`, `presents_client_certs` by both
capability tables. None is a distinction with one reachable side.

**Three of thirty-five traits are named in no test**, and all three are
the erasure traits — `DynTransport`, `DynTimer`, `DynInstant` —
which every `Client` test exercises without naming.

**The first finding was a claim with no check**, and it is the row this
file has gone stale on before: `Native::execute` is `Send` with the h3
arm installed, written down since amendment C15 and asked of the compiler
by nothing, because `send_future.rs` builds the arm-less stack.
`h3_send_future.rs` asks it now, and discriminates seven of the ten
`Send` declarations in `http3/arm.rs`.

**The second was a name.** Two traits called `StagedConnect` lived in one
crate — one per stack, which was one per *crate* until `hclient-h3`
folded in at `f4dfe48`. The root already exported the h3 one as
`H3StagedConnect`, so the surface was unambiguous and the definition was
not; the definition took the exported name. **Both went back to
`StagedConnect` when the pairs left the root** — `staged::StagedConnect`
and `staged::h3::StagedConnect` — because a module tells them apart and
a name prefix was only ever standing in for one.

**And this section's own subject matter was stale in four places.** The
transport that chooses between the stacks was `Selecting` in
`hclient-select` when it was written, and both dissolved into
`hclient-native` with the same commit: `H3Failures` is
`hclient-native`'s own (and crate-private since), `network_changed` is `Native`'s, and there
is no type called `Selecting` anywhere in the workspace. Four sentences
here named one or the other. That is the defect this file records against
itself in every other section, found by asking a scan which traits exist
rather than by reading.

### The resolver seam asks one question, and the record type is the parameter

`Resolve` had three associated types, three `lookup_*` methods and a
`supports_svcb` predicate. It has **one** of each: `type Records<'a>`,
`fn supports(&self, rtype: u16)` and `fn lookup(&self, name, rtype)`, over
a `Record { ttl, rdata }` whose `RData` is `A`, `Aaaa` or `Https`.

**What that buys is that the next record type is additive.** TLSA for
DANE, CAA before issuing, SRV — each is an `RData` variant and changes the
trait not at all. Under the old shape each was a fourth associated type, a
fourth method and a fourth capability constant, which every implementor
outside this workspace would have had to grow to keep compiling. Both new
types are `#[non_exhaustive]` for the reason this file's own rule gives:
they are handed *back* and only read, so exhaustiveness is not the
mechanism.

**`supports` is the same distinction one method further out.**
`supports_svcb` answered one question about one type; the new one takes
the number, which is the shape `system_resolver::Support::allows` next
door already had — `hclient-dns-system`'s answer was literally that
function with `HTTPS` written in, and the generality was being discarded
at this seam. The default is still `false` for everything, still the
understating direction, and still the only thing separating *asked and
found nothing* from *cannot ask*.

**Two types were deleted because the shape removed their subject.**
`NoSvcb` existed so a resolver with no SVCB could name something as its
`Svcb` associated type, and there is no such type any more; `EmptyStream`
existed for `NoSvcb` alone. That is the `UpgradeSupport` test applied
again — a name whose whole purpose was a distinction the code no longer
draws.

**And a TTL that was in two places is now in one.** `SvcbEndpoint::ttl`
was added a week earlier, correctly, because the wire carried a lifetime
that nothing carried up; `Record::ttl` now carries it for every type. Two
copies of one fact is the defect this file records about `Head::version`
and about `Native::hooks`, so the field is gone and
`endpoint_from_binding` hands back a `Record` — a TTL belongs to the
record rather than to what the record says, which is where DNS puts it
and where hickory keeps it. Nothing can fill one copy and forget the
other.

**The cost is that a caller writes the match, and it is not only
cosmetic.** One method cannot say in the type system which variant comes
back, so `RData::addr()` and `RData::https()` are the accessors and the
connector's discovery filter is `let Ok(RData::Https(record)) =
record.map(|r| r.rdata) else`. What the three methods guaranteed by
construction, three call sites now check: the **family** as well as the
type, because a resolver answering `A` to an `AAAA` question would
otherwise put a v4 address into the half of Happy Eyeballs whose whole
job is to race the families apart. `connect` and `H3::resolve` filter
rather than trust — and skip rather than panic, since what produced the
answer is a resolver a caller supplied. It is a real loss, and it is the
price of the additivity above: the alternative is a method per type,
which is what this replaced.

`hclient-dns-hickory` is where the collapse pays visibly — two lookups
that shared a `flat_map` and differed in their `filter_map` became one
`lookup_records(name, RecordType)`, with `wire_type(rtype)` the single
place the seam's numbers meet hickory's, so `supports` and `lookup`
cannot disagree about which types this backend asks about.

**And the mechanical half of the change put the same defect in three
places, which the suite caught and a reader would not have.** The
rewrite generated each `supports` as a constant list of the three types,
which is right for a resolver that answers unconditionally and wrong for
the three that answered from a field: two test fixtures whose whole job
is to be the *control* — records held, capability denied — started
claiming they could ask, and `hclient-dns-system` forwarded
`system_resolver::support()` verbatim, so a Linux build claimed `CAA`
while `lookup` had no arm for it. That last one is the capability lying
in the one method written to stop it, and it is now the rule stated on
all three backends: **`supports` is true only for a type `lookup` really
asks about**, with a test that a type this crate has no `RData` variant
for is refused however capable the platform is.

### The byte-stream seam stopped naming `hyper`, and one crate now does

`TcpConnect::Stream` and `TlsConnect::Stream<S>` are bounded on
`futures_io::AsyncRead + futures_io::AsyncWrite + hclient_rt::Shutdown +
Unpin`. They were `hyper::rt::Read + Write` for four verticals.

**The argument for hyper's traits was real: `hyper::rt` is where every `S`
in this vertical ends up anyway**, since
`hyper::client::conn::http1::handshake` accepts nothing else. What it did
not cover is *whose major version the seam promises*. A public bound naming
`hyper::rt::Read` puts hyper's major in the manifest of every implementor —
nine crates here, plus anybody outside who writes a runtime or a TLS
backend. This workspace has paid that once and repaired it: `hclient-dns`
leaked `domain` through one `pub fn`, recorded as *the leak outlived the
decoder it leaked*. hyper is a dependency this workspace may one day
replace; `http`, `bytes` and `futures-io` are not.

**Somebody else's traits for what they already say, and one of ours for
what they do not.** `futures_io::AsyncWrite` ends a stream with
`poll_close`, which is *close the writer*. An HTTP client needs the
narrower promise — **send FIN and go on reading the response** — which is
what `hyper::rt::Write::poll_shutdown` meant and what `TcpStream::shutdown`
does. Folding the two would lose a distinction this file treats as
load-bearing two sections down, where a half-close is *blocker two* against
an `embedded-nal-async` adapter. So `hclient_rt::Shutdown` carries
`poll_shutdown`. It carried `is_write_vectored` too, for a while — the one
capability `hyper::rt::Write` had and `futures_io::AsyncWrite` does not —
and that left before the freeze: over TLS the record layer copies whatever
it is handed, so a gathered write gains nothing, and on plaintext HTTP/1
the cost of losing it is one copy per request as hyper flattens a head and
a body. A trait named for half-close had no business answering it.

**The uninitialised-buffer machinery went with it and was not being used.**
`hyper::rt::ReadBufCursor` exists to let an implementation fill memory that
was never zeroed, and its only safe entrance is `put_slice`. Measured before
the change rather than assumed: **39 `put_slice` call sites, zero uses of
the cursor's `unsafe` `as_mut`/`advance`.** Every implementation here
already read into a scratch buffer and copied out — so `&mut [u8]` costs
nothing that was being collected, and the scratch buffers it made redundant
are the change's largest practical result:

- **`hclient-rt-embassy`: 2 KiB of application RAM per connection**, on a
  part that may have 256 KiB in total.
- **`hclient-rt-tokio`: a copy per read** on the hot path.
- **`hclient-tls-rustls`: a copy per read**, since rustls now decrypts
  straight into the caller's buffer. Its *ciphertext* scratch stays — that
  one is a real buffer between the transport and rustls rather than an
  artefact of the seam, which is the distinction worth keeping.
- **`hclient-rt::FuturesIo` entirely**, 324 lines plus 727 of tests. It
  existed only to bridge futures-io into hyper's traits; with the seam on
  futures-io it was a pure overhead layer, and its documented per-read copy
  went with it.
- **`hclient-tls-native-tls`'s two wrappers**, 243 lines. That crate
  converted *in* to hyper and *back out* to futures-io around a stack that
  speaks futures-io, to land at the trait the seam now names.

**The conversion lives at the one call that needs it.**
`hclient-native`'s private `hyperio::HyperIo` is the seam → `hyper::rt` adapter, used
at `http1::handshake` and nowhere else, and it is the only place in the
workspace where hyper's IO traits are named in anger. `hclient-native`'s h2
adapter — seam → `tokio::io`, for `h2`'s benefit — got *simpler* rather than
harder: `futures-io` hands over an initialised slice, which is exactly what
`tokio::io::ReadBuf::initialize_unfilled` produces, so the
`hyper::rt::ReadBuf` that used to sit between them is gone.

**`hclient-tls-rustls`'s test suite needs its own copy of that adapter, and
the reason is the dependency graph rather than an oversight.**
`hclient-native` dev-depends on `hclient-tls-rustls`, so depending on it
from there would be a cycle cargo tolerates in a workspace and refuses at
package time — the shape `just package-build` caught once between `hclient`
and its two backends, which would have blocked the whole publication. It is
twenty lines in that crate's shared test module, and what it must stay
faithful to is the one thing a wrong copy would hide: `poll_shutdown` is the
half-close, never `poll_close`.

**One real defect fell out, and the old seam structurally could not show
it.** `hclient_native::testing::blocking_io` called
`std::net::TcpStream::shutdown(Shutdown::Both)` — closing the reading half
too — from inside `hyper::rt::Write::poll_shutdown`. Under one method
meaning both things, the over-broad call was indistinguishable from the
right one. `hclient_rt::Shutdown` names the narrower promise, so it is
`Shutdown::Write` now: send FIN, keep reading the response, which is what
every shipped runtime already did.

**What it is checked by is the suite rather than the change.** 2566 tests
pass, including `hclient-tls-rustls`'s truncation-detection suite over a
**real** `hyper::client::conn::http1` through the new adapter, both of its
adversarial backpressure tests, and `hclient-rt-embassy`'s 19 live TAP
scenarios over a real network stack — which are the ones that would notice
a half-close that stopped being one.

### The QUIC TLS seam stopped naming `quinn-proto`, and the rule found its second subject

`QuicTlsConnect::quic_client_config` answered
`Arc<dyn quinn_proto::crypto::ClientConfig>` and answers a
`QuicCryptoConfig`. Same week, same argument as the byte-stream seam one
section up, and the second application is what says it was a rule rather
than one crate's repair.

**The version is the whole of it.** `quinn` is at `0.11`, a series where
every minor release may break, so a seam spelling quinn's type made a
`quinn-proto` bump a breaking change for **every** implementor rather
than for the two lines that touch the value. `hyper` was the same shape
at a different major.

**The recorded objection was half right and is kept**, because the half
that was right is what rules out the obvious alternative. `quic.rs`
argued that an opaque `type ClientConfig` would carry nothing — the
consumer must bound it back to `Into<Arc<dyn ..>>` before it can do
anything, which is this module's empty-body adapter dressed as
generality. That is still true, and it is why there is no associated
type. What it did not weigh is *whose major version the signature
promises*, and the newtype is neither of the two shapes it compared.

**Measured before choosing, and one plausible answer was rejected on
it.** The door could take a `rustls::ClientConfig` instead, which is
portable and would let any rustls-based backend build one — and it puts
**rustls and ring into `hclient-tls`'s own graph**, which is 33 crates
with `quic` on and zero of them rustls today. That trades a narrow leak
for a heavier one, in the crate that exists to have neither.

**So the two doors are `#[doc(hidden)]`, and the cost is named rather
than glossed.** A QUIC TLS backend written outside this workspace cannot
construct a `QuicCryptoConfig` without reaching a hidden item, so it is
not a supported extension point today. What makes that the right trade
is a measurement rather than a preference:
`quinn_proto::crypto::ClientConfig` has exactly **one** implementation in
practice, `quinn_proto::crypto::rustls::QuicClientConfig`, so the backend
this excludes is a second rustls binding rather than a second QUIC stack.

**That exclusion ended with the newtype, and the paragraph above is kept
for the trade it weighed.** `QuicCryptoConfig` is a public declaration
now, with `QuicCryptoConfig::new` and a builder and nothing hidden, so a
QUIC TLS backend written outside this workspace is a supported extension
point — and `hclient-tls/tests/no_stack.rs` is one, implementing the
trait with no QUIC stack in its graph at all.

**It is not the `bon` hole, and that objection is worth answering rather
than waving at.** `hclient-core`'s `req.rs` records a generated builder
whose *public setters named hidden types*, so a caller met
`SetConnect<S>` in a signature and in a compiler error and could not
write it. Nothing here appears in any public signature — rustdoc's own
words on the rendered page are **"This impl block contains no public
items"** — and a caller who never opens a `QuicCryptoConfig` never meets
quinn at all.

**`QuicTlsRequest` is `#[non_exhaustive]` with a builder**, which is the
input half and the one this workspace's own three-answer rule settles
immediately: it is a type the library *hands to* an implementor, which
reads it and never builds it, so a field added later must not be a
breaking change. The opposite case — `TcpOpts`, built by a caller as
`Struct { one: .., ..Default::default() }` — is why the attribute is
refused there and taken here. `QuicTlsRequest::new(alpn)` takes the one
field with no honest default (RFC 9114 §3.2 makes ALPN mandatory) and
`ech`/`early_data`/`identity` default to the understating answer.

**Checked from outside the workspace in both directions**, which is the
instrument this file records as different from a test written beside the
code. A scratch crate with a path dependency implements `QuicTlsConnect`
and calls the builder with **no `quinn` in its manifest or its source**;
against the parent commit the same file is `E0432`, and a literal
`QuicTlsRequest { .. }` is `E0639`. Two tests pin the builder's own
properties, each killed by its own mutation and neither by the other's —
a default flipped to `true` kills the defaults test alone, a setter made
a no-op kills the setter test alone.

**And then the newtype went too, because it was the defect.** Everything
above is about *whose major version a signature promises*, and the
answer it reached — wrap quinn's trait object in a type of ours — is
right about the signature and wrong about the graph: this crate linked
quinn to **hold** the value. Measured, `--all-features`: **38 crates
with the wrapper, 21 without**, and `chacha20`, `rand_core` and `ring`
among the difference. Cryptography, in the crate whose whole purpose is
that a backend chooses it.

So `QuicCryptoConfig` carries **what was decided** rather than a thing
built from it: the ALPN list, whether early data is offered, an ECH
config list, and the client identity's **label**. Every field is data
that arrived in a `QuicTlsRequest`. A private key is not there and
cannot be — a key in a smartcard is not bytes, and what a label *means*
is implementation-defined, resolved by the backend the caller registered
it with. Trust roots, verifiers and certificate resolvers are absent for
a simpler reason: they are the backend's own, settled at construction
and never per connection.

**Building a stack's session is a second method with an opaque
associated type**, and the crate that drives a stack is the crate that
names one: `hclient-native` writes `T: QuicTlsConnect<Session = Arc<dyn
quinn_proto::crypto::ClientConfig>>` at the three sites that dial, and a
`quiche` transport would write a different bound with nothing in the
seam changed.

**The objection recorded against an associated type was half right, and
the half it missed is the whole of it.** It said such a type carries
nothing, since a consumer must bound it back before it can do anything —
true, and the bound is one line where it belongs. What it did not weigh
is that *carrying* the value means **linking whoever defines its type**.
An abstraction that carries nothing is cheaper than a dependency that
carries cryptography.

The `quic` feature went with it, because it had nothing left to gate:
TCP and QUIC are two seams in one crate, peers, each describing what a
backend must answer. `hclient-tls-rustls` keeps its own `quic` feature
and is the one place a feature still earns its keep — that crate really
does link `quinn-proto`.

Two gates hold it. `just graph-tls-seam-carries-no-stack` checks the
graph under `--all-features`, which is what a feature cannot hide from;
`hclient-tls/tests/no_stack.rs` checks the source, with a backend naming
no QUIC stack — written first **outside the workspace**, against
`hclient-tls` and `hclient-core` alone, which is the instrument this
file records as different from a test written beside the code.
`graph-no-quic`'s control moved with the feature it tested: quinn must
still be one step away, pulled by the *backend*.

**And it found a pre-existing gap that only mutation named.** The
identity label is checked by `quic_client_config` and used by
`quic_session`, and dropping it in between passed the whole suite: every
unknown label refused, every known one silently presenting the
**default** identity — the silent substitution `docs/mtls-design.md`
exists to remove, in the one shape a refusal test cannot see, since both
halves answer `Ok`. It predates the split, where the same omission was a
config built from `self.base`.

**What cannot be given the same treatment is the session store, and that
is rustls' shape rather than a decision.** Read in 0.23.41,
`Tls13ClientSessionValue` holds a `&'static Tls13CipherSuite` over a
`ClientSessionCommon` carrying `Weak<dyn ServerCertVerifier>` and `Weak<dyn
ResolvesClientCert>` — weak references to live objects in this process —
with no codec and a `pub(crate)` constructor. A seam of ours could carry
only an opaque handle, which `Arc<dyn ClientSessionStore>` already is.
So a session store here is an **in-process cache policy** and never a
way to persist resumption across a restart. A test of that was written
and deleted: a mutation that *dropped* the stored value passed it,
because nothing out here can construct one to read back — a check that
cannot fail is not a check, and a claim about a third party that cannot
be checked belongs in prose naming the version it was read at.

### `embedded-nal-async` is the right seam for later and blocked twice now

Asked whether this workspace should implement `TcpConnect` over the
Embedded WG's network abstraction rather than over one stack. Measured,
and the answer is *not yet, and then yes* — with a caveat that does not
expire when the blocker does.

**The breadth argument is real, and it is the reason to want it.**
crates.io reports **27 reverse dependencies**, and about eight of them are
genuine stacks rather than consumers: `embassy-net` itself,
`embassy-nina`, `es-wifi-driver`, `esp8266-at-driver`, `rak811-at-driver`,
`nrf-modem`, `wincwifi`, `riot-wrappers` — AT-command WiFi modules, an
nRF9160 LTE modem, RIOT OS. One adapter reaches all of them where
`hclient-rt-embassy` reaches one. And `reqwless`, the incumbent embedded
HTTP client, is built on exactly this seam, which is evidence about the
niche rather than about us.

**Blocker one is the same `no_std` wall, so nothing changes today.** Every
one of those stacks is a `no_std` device, and `http` 1.5.0 still carries
its `compile_error!`. The only NAL implementations this crate could build
against are the std shims — `std-embedded-nal-async` and its neighbours —
where a caller has `std::net` and would use tokio.

**Blocker two is structural and outlives the first, which is the part
worth writing down.** `embedded_io_async::Write` is `write` and `flush`
and nothing else — read in 0.7.0 — and
`embedded_nal_async::TcpConnect::Connection<'a>` is bounded on
`embedded_io_async::Read + Write` and nothing more. So a NAL connection
**cannot half-close**, by the trait's own definition, and a half-close is
how an HTTP client sends FIN while still reading the response — which is
`hclient_rt::Shutdown`'s whole subject, and was
`hyper::rt::Write::poll_shutdown` while the seam was hyper's.

This crate has already met that and refused it. The W7 spike went through
embassy's own `TcpClient` — its NAL implementation — forwarded
`poll_shutdown` to `flush`, and recorded the result as *"a half-close
hyper believes it performed and did not"*. `hclient-rt-embassy` exists in
the shape it does precisely to avoid that: it owns the
`embassy_net::tcp::TcpSocket` rather than a `Connection`, so `close()` is
available and `poll_shutdown` sends the FIN and waits for it.

**Blocker three is the `Send` story, and it is the hardest of the three
because it is the one this workspace already solved on its own seams and
cannot solve on somebody else's.**

Measured: the auto trait `Send` appears **nowhere** in
`embedded-nal-async` 0.9.0 and **nowhere** in `embedded-io-async` 0.7.0.
The three hits a grep finds in `udp.rs` are the English verb in a doc
comment about sending datagrams. And every method in both crates is an
`async fn` in trait — `TcpConnect::connect`, `Dns::get_host_by_name`,
`Read::read`, `Write::write` — so every future they produce is an RPITIT
with no name.

**The two `TcpConnect` traits are nearly the same shape and differ in
exactly the place that decides this.** Both carry an associated
connection type with a lifetime; ours also names the *future*
(`type Connecting<'a>`) and theirs does not. That one difference is
amendment C15's whole subject: naming is not requiring, so each
implementor answers for itself and a consumer can still write the bound.
With an RPITIT a consumer cannot write it at all.

So an adapter over NAL could only box their future, and boxing decides
the answer for everybody:

- **boxed plain** — `!Send` permanently, for *every* NAL stack, including
  the ones whose connection genuinely is `Send`: a std shim, an
  AT-command driver behind a mutex. That is a `dyn` removing a property
  rather than hiding it, which this workspace has now fixed four times in
  its own code and would here be importing on purpose;
- **boxed `+ Send`** — needs to prove an RPITIT `Send` for a generic
  implementor, which is return type notation: **it works, and it is
  unstable**. Measured on 2026-08-28 against `embedded-nal-async` 0.9.0
  itself on 1.100.0-nightly: `S: TcpConnect<connect(..): Send>` compiles,
  `Box::pin(s.connect(addr))` goes into a `Send` box, `Read::read(..):
  Send` does the same, and a whole `OurTcpConnect for Adapter<S>` with a
  `Send` `Connecting<'a>` builds. So this blocker is the one RTN actually
  removes — see the note below on why that does not make it worth taking;
- **named** — needs `type Connecting<'a> = impl Future`, also unstable.

**And this is coherent on their side rather than a defect.** NAL is
designed for one core and one executor, where `Send` has no subject —
`reqwless`, the client built on it, never needs the property. The
incompatibility is at the seam between a `no_std`-shaped abstraction and
a client that also serves threaded hosts, and it is why
`hclient-rt-embassy` implements *our* `TcpConnect` against
`embassy_net::tcp::TcpSocket` directly: the socket is concrete, so its
`!Send`-ness is a measured fact about one type rather than a property
lost for all of them.

**Neither RTN nor a channel is needed for half the stacks, and that was
measured rather than assumed.** The rule this workspace states everywhere
— *at a concrete type `Send` is inferred, in a generic impl it must be
proven* — is constructive here: move the impl to the concrete type and
inference does the work. A macro is how.

```rust
hclient_rt_nal::adapt!(MyAdapter, my_stack::Stack);
// expands to an `impl TcpConnect for MyAdapter` whose
// `type Connecting<'a> = Pin<Box<dyn Future<..> + Send + 'a>>`
```

At the expansion site `Stack` is concrete, so `Box::pin(s.connect(addr))`
coerces into a **`Send`** box with nothing to prove. Measured against
`embedded-nal-async` 0.9.0 on **stable**: a stack whose connection is
`Send` compiles, and — the control that makes it honest — a stack holding
an `Rc` **fails**, at the boxing site, naming the future. So the macro
does not claim `Send` for everybody; each stack answers for itself, which
is the associated-type principle reached from the other side. A second
macro producing a plain box is the answer for the stacks that fail, the
same split `Transport` and `SendTransport` already are one layer down.

**Where the macro fails is where channels belong, and that is
`embassy-net`.** A `&RefCell` stack cannot be made `Send` by inference,
so the property has to be manufactured: an actor owning the stack, and
proxies holding channel endpoints. What crosses is bytes, so the proxy is
`Send` whatever the stack is. Two things make it cheaper than the earlier
reading of it — **one multiplexer task rather than one per connection**,
which removes the compile-time `pool_size` objection, and a buffer the
caller sizes at run time rather than a constant. `alloc` and atomics are
required by this workspace's construction anyway, so neither is a new
demand.

So the two are complements rather than alternatives: **the macro for
stacks that have the property, a channel actor for stacks that do not.**
And the channel half is what would put `hclient::Client` on embassy —
which is the thing wanted three questions before this one, `Send` having
only ever been the gate in front of it.

**Taking RTN from nightly for this one crate was asked and the answer is
no, on arithmetic rather than on policy.** It removes the third of three
blockers. `no_std` still stands — every one of those eight stacks is a
`no_std` device and `http` 1.5.0 still refuses — so the adapter would
compile only against the std shims, where a caller has `std::net` and
reaches for tokio. The half-close still stands. So the trade is: pin a
published crate to nightly, which makes every consumer nightly, to fix a
third of a problem for nobody who can use the result.

**And the obvious objection — *embedded is all on nightly anyway* — is a
fair recollection of a state that has ended.** It was true for years, and
it is measurably false now:

| measured, 2026-08-28 | |
|---|---|
| `#![feature(..)]` in `embassy-executor` 0.10, `embassy-net` 0.9.1, `embedded-nal-async` 0.9.0, `embedded-io-async` 0.7.0, `embedded-hal-async` 1.0.0 | **none, in any of them** |
| `embassy-executor`'s `nightly` feature | **optional**, not required — this workspace builds it with `platform-std, executor-thread` and nothing else |
| `reqwless` 0.14.0, the incumbent client on this very seam | `rust-version = "1.91"` — **stable** |
| this repository | `rust-toolchain.toml` says `channel = "stable"`, and `hclient-rt-embassy`'s 19 live TAP tests pass on it |

The last row is the sharpest: **this workspace is itself the evidence**,
since the embassy runtime and its live scenarios are green on stable in
CI on every push.

So the objection inverts the conclusion rather than softening it. If the
embedded audience were on nightly, a nightly-pinned adapter would cost
them nothing; because they are on stable, it would exclude precisely the
people it exists for — and `reqwless` shipping at a stable MSRV means a
competitor requiring nightly starts behind for a reason that has nothing
to do with its merits.

The policy cost is real too and points the same way. `rust-toolchain.toml`
says `channel = "stable"`, and this file refuses even an **MSRV job** on
the grounds that a pinned version is a promise that goes stale while
looking maintained — a nightly pin is that argument at its maximum, since
the pin breaks on a schedule somebody else sets.

And nothing is lost by waiting: if RTN stabilises the question becomes a
stable one again, and by then `http`'s `no_std` status may have moved too.
What needed capturing was the measurement, not the crate.

**"If RTN stabilises" had no date behind it and now has one, pointing the
other way.** Measured on 2026-09-06: the stabilisation PR
(rust-lang/rust#138424, opened 2025-03-12) was **closed unmerged on
2025-12-27**, its author saying they are no longer working on Rust and
that they look forward to someone landing it in some capacity; the
tracking issue (#109417) is still `S-tracking-impl-incomplete`, and
nightly **1.100.0 (0ed41eb41, 2026-09-04)** still answers `E0658 — return
type notation is experimental` for `S<get(..): Send>`. That last is the
check rather than a reading, and it is the one to re-run — written out
here rather than made a recipe, because it needs nightly and this
workspace refuses a nightly pin, which is `.notes/android-idn-live.md`'s
shape for the emulator run:

```
printf 'pub trait S { async fn get(&self) -> u8; }\n\
pub fn f<T: S<get(..): Send>>(_: &T) {}\n' > /tmp/rtn.rs
rustc +nightly --edition 2024 --crate-type lib /tmp/rtn.rs -o /dev/null
```

`E0658` means nothing has moved. Anything else means these sections are
stale and the erasure question is open again.

So the sentence above is not wrong and its tense was: waiting costs
nothing *and* is not waiting for something in motion. Every decision in
this file that turns on RTN — the NAL adapter's third blocker, the
generic consumer's bill in the erasure section — should be read as
permanent until somebody re-runs that one-line probe, not as deferred.

It also means blocker three does not lift with blocker one. A half-close
is a method upstream could add; this needs either return type notation to
stabilise and stop ICEing, or NAL to move to associated future types,
which breaks its trait for every implementor.

So the day `no_std` becomes reachable there are three options and none is
free: accept the false half-close **and** a permanently `!Send`
transport, which is what a NAL-based client must do; ask upstream for a
shutdown on `embedded-io-async` and for named futures on both, which is
the correct fix twice over and is somebody else's release schedule; or
keep a crate per stack that owns its socket, which is what exists and is
why the reach is one stack instead of eight.
