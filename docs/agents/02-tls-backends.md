# The TLS backends

> Part of the [AGENTS.md](../../AGENTS.md) record, split 2026-10-05. The root
> file keeps the operational rules and the index of parts. Sections below
> keep their original headings, and a "this file" in the text named the
> whole record when it was written.

### The wrapper was the limitation, and removing it took the workspace's second `unsafe`

`C16` made `hclient::Client` require `SendTransport`, and this backend
could not implement it: its handshake was
`async_native_tls::TlsConnector::connect`, a `pub async fn` whose future
has no name. So there was **no `Client` over the platform TLS stack at
all** — not a missing `Send` future, which is what the commit that landed
C16 said, but the cookie jar, redirects, the cache, decompression, digest
auth and SSE, all out of reach. It took measuring from outside the
workspace to see, because nothing in-tree depends on this crate.

**The fix was to stop being a wrapper.** Driving `native-tls`'s own
handshake was not enough on its own — `async_native_tls::TlsStream::new`
is `pub(crate)` and its adapter private, so the stream had to be owned
too. `crates/hclient-tls-native-tls/src/stream.rs` is that: a named
handshake future whose `Send` follows from `S`, and a `TlsStream` over
`native_tls`'s.

**It costs one `unsafe` and that is amendment C17.** `native-tls` is not
sans-io — it fronts SChannel, Security.framework and OpenSSL through a
synchronous `Read`/`Write` — so bridging it means handing the synchronous
side a way to reach the current task's waker. That is a raw `Context`
pointer, set immediately before a call and cleared by a `Guard` whose
`Drop` runs on unwind, asserted non-null rather than trusted. It is the
difference between the two TLS backends rather than a difference in care:
rustls is sans-io, so its handshake is a loop over buffers this workspace
owns.

**What it bought is two things, and the second was not the point.**
`Client` works over the platform stack again — which is the whole reason
this crate exists, since an organisation with MDM roots cannot use
rustls. And `reports_alpn` is `true`:
`native_tls::TlsStream::negotiated_alpn` is public, and only the wrapper's
absence of a re-export had hidden it. This crate's own doc called that
limitation concrete for two verticals while naming its cause correctly and
never acting on it.

**Measured: the graph fell from 66 crates to 32**, because
`async-native-tls` left with its dependencies.

**A second TLS seam, for QUIC, and it is not a widening of the first.**
`hclient-tls`'s `QuicTlsConnect` exists because the intersection of
`TlsConnect`'s four methods with `quinn_proto::crypto::Session`'s eleven is
**empty** — QUIC wants key schedules per encryption level and CRYPTO-frame
payloads, `TlsConnect` can only hand back a wrapped byte stream — and the
failure mode is worse than a compile error: an adapter between them
type-checks *with an empty body*. `hclient-tls-rustls` implements it behind
a `quic` feature; `hclient-tls-native-tls` implements nothing and using it
for HTTP/3 is a compile error, which is honest rather than harsh, because
`native-tls` binds no QUIC API at any level.

**It was a separate crate for two verticals, on the argument that Cargo
unifies features — and that was reversed in `169dbdd` after the cost was
measured rather than assumed.** The argument is still literally true: a
neighbour switching the feature on does put `quinn-proto` in every graph
that has any TLS. What was wrong is that this cost had **already been
accepted one crate over** — enabling `hclient-native/http3` puts
`quinn-proto` and `ring` into `Native<Embassy, NoTls, IpLiteralOnly>`'s
graph, and the commit that did that called it *dead code in the graph, not
a broken one*. Refusing the identical cost here was the inconsistency
rather than the caution. Who newly pays is narrower still: of the five
crates depending on `hclient-tls`, two gain an edge, and both are only
ever in a graph beside a transport.

`TlsConnect` and `QuicTlsConnect` share `TlsIdentity`, so a connector has
one configuration identity rather than two.

### A client certificate is chosen by a name, and the name is the only portable thing

mTLS with **one** identity has always worked: `Rustls::from_config` takes a
`rustls::ClientConfig` built with `.with_client_auth_cert(chain, key)`, and
every connection presents it. What did not exist is choosing **per
request** — a tenant per certificate, a smartcard beside a software key —
and that is now `hclient_core::ClientIdentity` in the request's extensions,
`TlsRequest::identity` on the seam, and `Rustls::with_identity(name, cfg)`
on the backend.

**What travels is a label the caller invented, and that is the whole
design.** Not a certificate — a key in a smartcard cannot be handed over as
bytes. Not a store query — `CERT_FIND_SUBJECT_STR` means nothing to
PKCS#11, and a `SecIdentityRef` means nothing to Windows. A name is the
only value that means the same thing on Windows, macOS, PKCS#11 and
Android at once, because it means nothing on any of them until a backend
resolves it. `docs/mtls-design.md` §3.1.

**The seam is `TlsIdentity::config_id_for(name) -> Option<TlsConfigId>`,
defaulted to `None`** — `reports_alpn`'s and `applies_ech`'s shape, a
constant defaulted to the understating value, read by the layer above to
decide whether to *ask*. A backend that never heard of labels refuses every
name, and `hclient-native` turns that into an error naming the label
**before it opens a socket**. The alternative — connecting with the default
identity — is how one tenant's certificate reaches another tenant's server,
which is the *silently ignored setting* defect where the setting is a
credential.

**Isolation is by construction rather than by a check**, and it cost one
expression: `TlsConfigId` was already a component of `hclient-native`'s
pool key, so resolving the label into that key is the whole of it —
`Security::Tls(identity_id.unwrap_or_else(|| self.tls.config_id()))`. Two
labels to one origin therefore cannot share a connection. That is the
load-bearing property and it is mutation-verified: dropping the
`unwrap_or_else` back to the connector's own id fails exactly
`two_identities_to_one_origin_cannot_share_a_connection` and nothing else.
The control is the same label twice, which must share one.

**The label is `Cow<'static, str>` rather than an `Arc<str>`**, because it
is almost always a literal: `Cow::Borrowed` costs nothing where
`Arc::from(&str)` allocates on every request. A computed label pays a
`String` clone per hop, bounded by the redirect limit and a few bytes wide.

**The QUIC half was written by the compiler and one line was not.**
`QuicTlsRequest` carries the same field, and threading it through
`hclient-h3` was mechanical — except that `quic_config_for` accepted the
identity and ignored it, which the type checker cannot see. The design
document had warned about exactly this shape one section earlier and the
warning was realised inside its own implementation: **the compiler catches
the transport and never the backend.**

**The observation half is built, and it landed one commit later than the
selection half on purpose.** `ClientCertRequest` was written with the
seam, **removed before that commit** because it had no producer and no
reader — `UpgradeSupport`'s shape — and put back with both. A caller who
wants to *choose* interactively needs what the server asked for, and
rustls hands the `CertificateRequest`'s contents to a
`ResolvesClientCert` and to nothing else: there is no getter on
`ClientConnection` afterwards, so the only way to see them is to **be**
the resolver.

**So the record belongs to a connection and the resolver belongs to a
config, and those are different lifetimes.** A `ClientConfig` is shared —
cloning one is rustls' most expensive operation and this crate caches
per-ALPN copies precisely to avoid it — so the resolver cannot own the
slot it writes into. What it can do is write into whichever slot is
installed *for the duration of the call*, and `resolve` is reachable only
from inside `ClientConnection::process_new_packets`, which is a
**synchronous call this crate makes**. A guard scopes the slot around the
poll the way a lock scopes a critical section. That is
`hclient-tls-native-tls`'s waker trick with the sign flipped: there it
costs the workspace's second `unsafe`, and here it costs none, because
rustls is sans-io and the value being scoped is an `Arc` rather than a
borrowed `Context`.

**The wrap is unconditional and cannot change a handshake.** Every
constructor goes through it, `from_config` included — the caller who
builds their own config is the caller doing mTLS — and `Recording`
forwards `resolve`, `has_certs` and `only_raw_public_keys` verbatim. It
costs one config clone per *construction*, never per connection.

**`answered` is the field that earns the payload type.** Without it a
caller cannot tell *403 because I sent no certificate* from *403 because
I am not authorised*, so a picker fires on the wrong responses. It is
discriminated by exactly one of the three tests, which is what says it is
carrying a fact rather than decorating one.

**And the answer is three-state, because two would be a lie by
omission.** `Option<ClientCertRequest>` was the shape that shipped for
one commit, and it collapses *the server did not ask* into *this backend
cannot see whether it did*. Both sides are reachable in one program —
rustls observes by being the resolver, `native-tls` exposes no hook — and
**`hc --backend` chooses between them at run time**, so a caller writing
a picker would see nothing on one backend and conclude that no server
ever asks. `ClientCertAsk::{Unobserved, NotAsked, Asked(..)}` is
`Discovered::NoRecord`/`NotConsulted` a third time, and the default is
`Unobserved` — the understating value, asserted rather than described, so
a backend that forgets the field can never be read as having watched.

Only `hclient-tls-rustls` may say `NotAsked`, and it may because its
recording resolver sits in **every** config it hands out: an empty slot
after a completed handshake is an answer. `hc -v` prints all three —
nothing for `NotAsked`, the authorities and whether one was sent for
`Asked`, and *this TLS backend does not report whether one was requested*
for `Unobserved`, which is the same distinction the SSL line above it
already draws for the version and the suite. The QUIC path reports
`Unobserved`: quinn drives that handshake, so no slot of ours is
installed.

The enum is **not** `#[non_exhaustive]` where its payload is, and the
split is this file's own rule: the payload is handed back and only read,
where the enum is *branched on*, so exhaustiveness is the mechanism and a
fourth state must be a compile error at every reader.

It reaches a caller on `Connected`, by the path `tls_version`,
`tls_cipher` and `alpn` already take, and through the same **one** setter
for the same reason: a backend either read the handshake's outcome or it
did not.

**Two more things the seam owed and did not have.** `Rustls::config_for`
fell back to the **default config** for a label it did not recognise,
under a comment observing that the transport refuses first — a silent
substitution guarded by an argument about unreachability, which is the
exact failure the design exists to prevent, and the QUIC half had the
same fallback written a second way. Both refuse now, naming the label,
and the obligation is written on the seam: *a backend that answers `Some`
from `config_id_for` owes that identity at connect time, and owes a
refusal rather than a substitution if it cannot serve it.* The layer
above cannot catch a breach — it resolved the label, put the id in its
pool key, and has no way to learn the handshake used another. And
`ClientIdentity`'s field is private: it was an `Arc<str>` for a day, so
the representation is exactly the thing not to promise, and `Clone` is
what a pool key needs — which is why the h3 key holds the type rather
than the string inside it.

What is still not here is the picker itself, and `docs/mtls-design.md`
§3.5 is why it is the caller's: rustls has no handshake pause, so an
interactive choice is observe, abandon, choose, redial — one extra
handshake per origin per session, and a second failed mTLS attempt in
whatever the server counts.

**And this workspace was wrong twice about rustls and about smartcards.**
It was said here that a server sending a DN list lets rustls choose:
`SingleCertAndKey::resolve` takes `_root_hint_subjects` and `_sigschemes`
and **discards both by name**, so rustls ships no resolver that chooses at
all — nothing in this workspace narrows a certificate set, and the DN list
is a thing to hand a caller rather than a thing anything acts on. The
smartcard claim is corrected where it was made: `native_tls::Identity`
builds from PKCS#12 or PKCS#8 **bytes** only, so the platform backend
cannot reach an OS-held key either. Reaching one needs a backend bound to
the keystore — `rustls-cng` on Windows — and that is the next step rather
than a thing already there.

`NoTls` in `hclient-tls` is the third choice: no TLS at all, for a build
that has no room for a stack. `https://` then fails at connect with a typed
error, and `Capabilities::tls_config` reads `TlsSupport::None` rather than
claiming otherwise.

**`std` is required, and that is not our decision to reverse.** `http` 1.x
forbids `no_std` outright — `src/lib.rs` carries a commented-out
`#![cfg_attr(not(feature = "std"), no_std)]` next to
`compile_error!("`std` feature currently required, support for `no_std` may
be added later")` — and `http::{Request, Response, HeaderMap, Uri, Method}`
appear in the public API of ten crates here, including the sans-io
`hclient-proto`. `bytes` is a genuine `no_std` + `alloc` crate, and `url` is
gone from the graph entirely (see below), so the remaining obstacle is
`http` itself; a feature flag that claimed otherwise would not build.

For constrained targets that *do* have `std` — static musl binaries, small
containers, embedded Linux — see `NoTls` and `IpLiteralOnly`, and
`crates/hclient-native/examples/minimal.rs`.

**Bare-metal microcontrollers are not reachable today. A device with
`std` is — and the sentence that used to say so named the wrong runtime
for it.** It read that an esp-idf target is reachable *because*
`hclient-rt-embassy` implements `TcpConnect` over `embassy-net`, and the
pairing is wrong in both directions. Measured: `esp-idf-svc` 0.51.0
integrates `embassy-sync`, `embassy-time-driver` and `embassy-futures`
and **not `embassy-net`** — it gives you ESP-IDF's own lwIP through
`std::net`, so the runtime there is `hclient-rt-smol` or
`hclient-rt-tokio`, which have needed nothing special all along.
`embassy-net` is the `no_std` stack, and `no_std` is exactly what is out.

So a std device is reachable, and `Native<Embassy, ..>` is not how. What
`hclient-rt-embassy` is for is [the section on its real
value](06-send-and-embassy.md#the-embassy-runtime-is-the-workspaces-only-send-counterexample);
what is still out is `no_std`, and the obstacle there is a dependency
rather than a design:

- **`http` 1.x, external.** The `compile_error!` above.
- **`url`, ours — and now removed.** `hclient-proto` used it at exactly one
  functional site — `Url::parse().join()` for RFC 3986 reference
  resolution — and that one call pulled `idna` -> `icu_normalizer` +
  `icu_properties`: measured at 1.9 MB, 1004 KB, 820 KB and 452 KB of
  vendored source, almost all Unicode tables for internationalised domain
  names. On a part with 256-512 KB of flash that is the entire budget, for
  a feature such a device rarely needs.

  `crates/hclient-proto/src/uri.rs` now implements RFC 3986 §5.2 directly,
  and `url` has moved to `[dev-dependencies]`, where it is the oracle for
  `tests/uri_resolution.rs` — a 96-pair differential corpus (all 42 RFC
  3986 §5.4 reference examples, plus the forms a client actually meets)
  that pins both implementations' answers and enumerates every place they
  deliberately differ. IDN survives as the `idn` feature of
  `hclient-proto`, forwarded by `hclient` and **on by default**, so a plain
  build behaves as before; `--no-default-features` removes the whole IDN
  implementation and turns a non-ASCII host into a typed
  `UriError::NonAsciiHost` naming the A-label to send instead. The
  `idn-feature-is-real` CI job checks all of that, in both directions, and
  runs the feature-off test suite that `--all-features` cannot reach.

  **The feature no longer names `idna`; it names `hclient-idn`**, which
  chooses the implementation by target in its own `build.rs`. `uri.rs`
  calls `hclient_idn::domain_to_ascii` and maps two error variants, and
  nothing in `hclient-proto` mentions `idna` any more. What that changes
  is where the Unicode tables are, not what a host converts to: on Linux
  and the other ELF unixes, and on wasm, the backend *is*
  `idna::domain_to_ascii_cow(…, AsciiDenyList::URL)` — the same call
  `uri.rs` used to make itself, with the same arguments. Measured before
  believing: the 96-pair corpus is unchanged and green in both feature
  settings, and `hclient_idn::domain_to_ascii` against `idna` directly
  over 9,739 inputs on this host gave 0 differences.

  So the crate count is now a fact about the target rather than one
  number. Measured `cargo tree -e normal`, unique crates, this tree:

  | build of `hclient-proto` | crates | what supplies UTS 46 |
  |---|---|---|
  | default (`idn`), x86-64 Linux | **55** | `idna` + the ICU data crates |
  | default (`idn`), `--target x86_64-pc-windows-msvc` | **20** | `icuuc.dll`, through `windows-sys` |
  | default (`idn`), `--target aarch64-apple-darwin` | **55** | `idna`, the same as Linux |
  | `--no-default-features` | **16** | nothing — `NonAsciiHost` |

  **The Apple row moved for a reason of this workspace's own and the rest
  moved with the graph.** It read **22** and `Foundation, through
  objc2-foundation` until the differential corpus was run against that
  backend: `NSURL` converts an IDN host as a side effect of parsing a URL,
  so it does not case-fold ASCII and does not validate an ACE label, and
  eight rows came back as themselves. Apple takes the bundled tables now,
  which is what the row costs. Windows is the only target left where the
  saving is real, and Android — not in this table, because
  `hclient-proto` is not built for it here — is the other.

  The Linux row was the old **36** plus `hclient-idn` itself and nothing
  else: `thiserror` was already there, and no new Unicode crate arrives.
  There is no `url` in any of them.

  Every row is three higher than that since `encode.rs` and `uri.rs`
  stopped carrying their own encoders — `base64`, then `form_urlencoded`
  and `percent-encoding` — and the Linux row is one higher again from
  upstream churn, the same drift the dependency-graph section in the root
file measures. That the
  crate's own changes moved **all four** rows while churn moved only the
  one with a large third-party subtree is the clearest statement of what
  separates them.

  **All four moved again, by exactly one, when `head.rs` arrived** —
  `winnow`, for RFC 9112 §4's response head, which is what let
  `CONNECT` stop needing an HTTP client. The rule above demonstrated
  itself: a change of this crate's own moves every row, and the +1 is
  the same +1 on Windows and macOS as on Linux because `winnow` has no
  Unicode tables and no platform half. It is **not** behind a feature,
  which was the first shape and was withdrawn: gating it would have
  bought one crate back at the floor and cost every consumer a feature
  to remember.

  **There is still no `url` in any of them, and that is the point of the
  last two.** `form_urlencoded` is its own crate over `percent-encoding`
  alone. This file and `encode.rs` both said for two verticals that
  taking it *"would bring `url` straight back"*, and the claim was never
  measured; it is two crates, no `idna`, no ICU, no build script, and
  both wasm targets. What the measurement does rule out is the near
  neighbour: `urlencoding` computes a different function at both sites.

**Name resolution has a third backend, and its problem was never the wire
format.** `hclient-dns-doh` (v0.3) puts DNS-over-HTTPS behind the same
`Resolve` seam, and the two questions worth knowing the answers to are
both about bootstrapping rather than about parsing. What makes the
request is a `Transport`, **never an `hclient::Client`** — a cookie jar,
a redirect policy and `Authorization` belong to `Client`, which
`Transport` has never heard of, so "a resolver's client is not the
user's client" is a thing that does not typecheck rather than a thing
that is discouraged; the cost is that there is no `total` bound, because
that is `Client`'s. And what resolves the DoH server's own name is
stated by which constructor compiles: `Doh::pinned` takes an IP literal
and refuses a name, `Doh::bootstrapped` takes a name and refuses a
literal. Failing closed is the default and failing open is visible in
the type — `Doh<C>` is `Doh<C, NoFallback>` — so it travels into every
transport that holds it rather than hiding in a builder call.

SVCB parsing moved from `hclient-dns-system` up into `hclient-dns`
behind a `codec` feature, so an `IpLiteralOnly` build carries no DNS
decoder: 13 crates without it, 16 with, and the DoH crate itself is 22
with no `tokio`, `hyper` or `h2`.

**The feature is gone and the property it bought is now unconditional**,
which is the section on taking `domain` off that crate's public surface,
in [Releases and versions](01-releases-and-versions.md).
What moved up was two things wearing one name: RFC 9460's *client rules*,
which name no decoder and stayed, and one *conversion from a decoded
record*, which could not be written without naming one and has gone to
its only caller. So `hclient-dns` is **14 crates with every feature there
is** rather than 14-or-22 depending on a flag, and no `--features` line
can put a DNS codec in its graph. The numbers here are the older
measurement and are kept for the shape rather than the figure.

**HTTPS/SVCB records are consulted before every new connection** on a
resolver that says it can ask (v0.3 W2), for `https://` at the default
port only — RFC 9460 §9.5, since the record fetched for a bare name is
the default-port one, and `http://` would mean upgrading the scheme,
which a connector must not do silently. The record's port, address
hints and ALPN offer are used; **its `ech_config_list` is passed on only
to a TLS backend that says it applies one** (`TlsConnect::applies_ech`,
defaulted `false` beside `reports_alpn`), and no backend in this
workspace does. That is not caution: `hclient-tls-rustls` *refuses* a
non-`None` `ech`, so a connector that filled the field from every record
would make every ECH-publishing origin unreachable. Measured before it
was decided — zero bytes on the wire. The privacy cost of the gate is
stated where a caller will find it, including a test asserting the
origin's name goes out in the clear.

The record and the addresses are asked **at once**, which took a second
commit: the first put discovery in front of the address lookups and
roughly doubled cold DNS on the default path. Measured after: floor
396 → 322 ms, median 456 → 340 ms on a DNS-dominated request; a record
cost 404.6 ms of extra DNS time and now costs 0.8 ms.

Runtimes exercised in CI: tokio and smol. Connection reuse landed in v0.2
(W2) and `Native::new` now pools by default; **HTTP/2 landed in v0.2 (W3)**,
behind `hclient-native`'s `http2` feature, off by default; **HTTP/3 landed
in v0.3**, over QUIC — in its own crate then, and `hclient_native::H3` since `f4dfe48`. **WebSocket landed in
v0.3 (W4)**, and in v0.4 became a crate of its own,
`hclient-tungstenite` — and not as a method on `Transport`: it is its
own trait pair,
`WebSocketConnect` (what a backend implements — a backend is not a
connection) and `WebSocket` (the message channel), so a transport that
cannot do it is a **compile error** rather than a runtime `Unsupported`.
`Capabilities::upgrade` is gone with it: four variants, and nothing ever
branched on any of them.

The seam is message oriented on purpose. "Hand back the socket after the
101" is implementable by exactly one of the four backends here, and the
three it shuts out include the browser — where `WebSocket` is a separate
global that a `fetch`-shaped `Transport` cannot reach at all. That the
browser then fitted the trait **unchanged** is the evidence the shape was
right, and it settled two things the design could only argue: the seam's
`!Send` allowance has a real subject (`FetchWebSocket` is `Rc<RefCell<..>>`
plus three `Closure`s and needed no `unsafe impl Send`, because no `Client`
sits between this seam and its caller), and `Message` has no `Ping`/`Pong`
because a browser has neither `send(ping)` nor `onping` — the variant would
have had no honest right-hand side.

Framing on native is `tungstenite`, driven by us rather than through an
async wrapper, and the reason is not taste: `WebSocketContext` takes the
stream as a *parameter*, so the shim can borrow the poll `Context` for one
call — where `tokio-tungstenite`'s `AllowStd`, owning its stream across
calls, has to smuggle a `*mut Context`. `.notes/w4-upgrade-seam.md` has the
measurements and the decisions.

**It is `hclient-tungstenite`, its own crate, and until v0.4 it was a
`websocket` feature of `hclient-native` — the one pluggable thing here
that was not its own crate** (`.notes/w4-upgrade-seam.md` §8). Features are
additive, so that feature put `tungstenite` into every build in any graph
that switched it on: the argument that kept `hclient-h3` out of
`hclient-native` and the QUIC TLS seam out of `hclient-tls` — the second
of those has since dissolved, because the seam stopped carrying
`quinn-proto` at all — applied to the one place it was not. A dependency in the other direction cannot be
switched on from outside, and `graph-no-framing-in-the-transport` checks
it with `--all-features` on the transport rather than asserting it.

**The seam between them is "an upgraded byte stream, plus the `read_buf`
hyper had already read past" — the shape §2 rejects as the public seam.**
Both hold at once and they are about different levels: as the public seam
it excludes three of four backends, the browser among them; between a
transport and a framing crate it is only ever asked of the one backend
that can answer it. `hclient_native::Upgrading` is that seam, and it is
two-step on purpose — it lends out the `101`'s head and is dismantled only
by a separate `finish`, so the checks that decide whether this is *your*
`101` cannot run after the connection has been taken apart.

**What implements `WebSocketConnect` is `Tungstenite<'_, R, T, D, H>`, a
connector that borrows a `Native`**, and the losing option is worth its
line: `Native` keeping the impl and delegating the framing costs a caller
**zero** lines, against one dependency and one expression
(`Tungstenite::new(client.transport()).websocket(req)`) — and leaves the
defect exactly where it was, since the impl needs `tungstenite` and would
put the feature straight back on the transport. It borrows rather than
owns, unlike the QUIC arm, because `Native` is not `Clone` and
`Client::builder` takes its transport by value: owning would cost either a
second transport with a second pool or a `Transport` impl on a type that
sends no requests. `hclient-fetch` is untouched by all of this and needs
no connector — a browser hands back *messages* — which is the asymmetry
that says the seam is in the right place.

**An open WebSocket is bounded by liveness, not by `Timeouts`, and the
knob is on the connector rather than on the seam.** `total` is
meaningless for a connection meant to outlive its exchange and
`between_bytes` would be actively wrong, since silence is a WebSocket's
normal state — so the bound is RFC 6455's ping/pong, off by default,
because a default that pings sends traffic nobody asked for. It is not on
the trait because a browser has neither `send(ping)` nor `onping`, the same
fact that keeps `Ping`/`Pong` out of `Message`; asking `hclient-fetch` for
it does not compile.

**Two clocks, answering to different events**, which is the part that took
measuring rather than deciding. The interval measures *silence*, so any
inbound frame restarts it — which is what makes the feature free on a busy
connection. The deadline measures *an unanswered probe*, and only a `Pong`
carrying that ping's own payload clears it — matched on payload rather than
opcode, because §5.5.3 allows unsolicited pongs, and letting any frame
clear it would turn the probe back into the gap bound that was rejected.
That distinction was found by a mutation that **survived a test asserting
the right error**: with any frame clearing the probe, the stream still
failed with the same `PongNotReceived`, the same kind and the same bound —
a second ping had simply died 100 ms later. Same error, different fact. The
fixture now counts pings and the test asserts exactly one.

The missed-pong error is `ErrorKind::Body` with a public `PongNotReceived`
source, deliberately agreeing with `hclient-fetch`'s treatment of a
`wasClean == false` close rather than inventing a second vocabulary, and
deliberately not `ErrorKind::Timeout`, since no `Timeouts` field is in
force. Nothing is spawned: the caller's `poll_next` is the only thing
driving the socket, **so a caller that stops polling gets no keep-alive**.
That is the mirror of HTTP/3, where a spawned driver turned out to be
necessary and not sufficient; here there is no driver at all.

See [`.notes/v01-acceptance.md`](.notes/v01-acceptance.md) for what v0.1
deliberately does not do,
[`.notes/v03-acceptance.md`](.notes/v03-acceptance.md) for what v0.3 does,
does not, and has not checked, and
[`.notes/v04-acceptance.md`](.notes/v04-acceptance.md) for v0.4 — which is
the shortest of the four on purpose, because v0.4's arguments were
written down one document per topic as they were made, and it indexes
them rather than copying them. What it does carry, because no per-topic
document can, is the *deliberately not done* and *not checked* lists:
each of those knows only its own half.
