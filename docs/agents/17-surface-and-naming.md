# Surface, naming, audits

> Part of the [AGENTS.md](../../AGENTS.md) record, split 2026-10-05. The root
> file keeps the operational rules and the index of parts. Sections below
> keep their original headings, and a "this file" in the text named the
> whole record when it was written.

### One crate was named after a family the dependency rule forbids

Asked before publishing whether any crate is redundant or misnamed, both
questions were measured rather than eyeballed, and they came out
differently.

**Nothing is redundant.** The test is this workspace's own: a crate exists
to hold a dependency a feature would otherwise spread to every graph.

**The example this used to give has since inverted, and the inversion is
worth more than the example.** It read: the likeliest suspect passes the
test most sharply — `hclient-tls-quic` is 153 lines, the smallest here,
and it carries `quinn-proto`, which is exactly the dependency the
argument is about. That crate folded into `hclient-tls` at `169dbdd` and
became a `quic` feature; the feature then had nothing left to gate,
because the seam stopped *carrying* `Arc<dyn
quinn_proto::crypto::ClientConfig>` and started answering a declarative
`QuicCryptoConfig` with an opaque `Session`. So the dependency the whole
argument turned on is gone: `hclient-tls` is 21 crates under
`--all-features` where the seam cost 38, with `chacha20`, `rand_core`
and `ring` among the difference.

The rule survives its example, which is the point. A boundary is worth
keeping for a dependency it holds — and the way to stop paying for one is
to **stop holding the dependency**, which is a change to what crosses the
seam rather than to where the seam is drawn. `hclient-tungstenite`
carries `tungstenite` and is kept for exactly that reason, unchanged. `hclient-quinn` has one in-tree
consumer and an external reason (41 crates against `hclient-h3`'s 56 for a
caller who wants bare QUIC), enforced by a `just` recipe — **and it was
misnamed, which this pass checked it for redundancy and missed.** It was
`hclient-rt-quinn`, and the arrow points the other way from the rest of
that family: `hclient-rt-tokio` and its siblings implement *our* seam using
someone else's runtime, where this implements *quinn's* — `quinn::Runtime`,
`AsyncTimer`, `AsyncUdpSocket`, `UdpPoller` — using ours. Its own first line
had always said so. It is not a fourth runtime; it is what lets quinn run on
whichever runtime the caller already chose, and the family name made the
wrong reading the default. `hclient-tower`'s bare-foreign-name shape is the
one it takes now, and `.notes/quinn-adapter-extraction.md` went with it. Eleven crates have
no in-workspace consumer at all and are terminal by design: a user picks the
backend.

**`hclient-rt-pair-check` is the one that looks superfluous and is not.** A
5-line lib whose own doc says *deliberately empty*, 500 lines of tests, an
empty `[dependencies]`, `publish = false` — and it must depend on
`hclient-rt-tokio` **and** `hclient-rt-smol` at once, which no shipped
crate may do. Its name sits in the runtime-implementation
namespace while being a test harness, and it is left alone deliberately: the
name never reaches crates.io, and it is cited as evidence in seventeen doc
comments across the workspace, so it has become a landmark whose renaming
costs prose and buys nothing outside the repository.

**One name was wrong, and the interesting part is that the missing crate it
implied should not exist.** Three families follow `hclient-<seam>-<impl>`
and each has a seam crate: `hclient-rt` (which carries **hyper**),
`hclient-tls` (**hyper** again) and `hclient-dns` (`dns-message-parser`
behind `codec`). Each head exists to hold something `hclient-core` must
not. `hclient-ws-tungstenite` was the fourth `-<impl>` name and had no head
— and it should not have one: the `WebSocketConnect`/`WebSocket` pair is
**161 lines over `futures_core` and `futures_sink`**, nothing `hclient-core`
does not already have, so an `hclient-ws` would be a crate with nothing to
carry. The name promised a crate the dependency rule forbids.

It is **`hclient-tungstenite`** now, which keeps the framing library in the
name — that choice is argued in `.notes/w4-upgrade-seam.md`, `tungstenite`
over `tokio-tungstenite` because `WebSocketContext` takes the stream as a
parameter and needs no `*mut Context` — and promises no family. Renaming
before the first publish is free and after it is not, which is why the
question was worth asking at this exact moment. The reason lives in the
crate's own README, where a reader looking for `hclient-ws` will be.

One softer observation, recorded and not acted on. `hclient-idn` is not about HTTP at all — it is a UTS 46 crate that picks its
implementation by target, worth more to the ecosystem than the prefix lets
anyone find. It is the owner's call and it is not wrong.

All 29 names were checked against crates.io and all are free. **The first
run of that check answered `403` for every name including its control**,
`reqwest` — crates.io refusing a request with no `User-Agent` — which is
this file's rule about a check whose answers cannot differ, met one more
time. With a `User-Agent` the controls separate: `reqwest` 200, a nonsense
name 404.

### `hclient-native`'s root held 61 names, and a third of them were one kind of thing

The same exercise one crate down, measured the same way — rustdoc's JSON
rather than a read of `lib.rs`, because re-exports behind five features
do not add up by eye. **61 names and three modules at the root, now 19
and five**, and the move answers one question per module rather than
tidying: *what does a caller do with this?*

- **`error::`, 19 payloads** — everything `Error::source` can hand back.
  Seventeen were at the root beside `Native`, the proxy two were also
  under `proxy::`, and `H3ConnectTimedOut` carried a prefix because the
  root could hold one `ConnectTimedOut`.
- **`staged::` and `staged::h3::`** — each stack's `StagedConnect`,
  `Staged` and `Refused` under its own module and its own name. The QUIC
  trait had been renamed `H3StagedConnect` for exactly the root's reason,
  and `Staged`/`Refused` beside it went out as `H3Staged`/`H3Refused`.
  A module is what a name prefix was standing in for.
- **`task::`** — `H2Driver`, `Reaper`, `QuinnTask`: public only because a
  `Spawn` bound names them.
- **`proxy::`** was already public and every proxy type was *also* at the
  root. One path each now.

**Eleven items left the public API**, each named by no public signature:
`hyperio::HyperIo`, the failure memory `H3Failures` and both TTLs, and
the `Alt-Svc` parser and rules — `AltSvcCache`, `FieldValue`,
`Alternative`, `parse`, `DEFAULT_MAX_AGE`, `BoxAltSvcStore`, `InMemory`.
`altsvc::` keeps what a store author writes against: `AltSvcStore`,
`Entry`, `Origin`, `KvStore`. The tests that exercise the rest reach it
through `#[doc(hidden)] testing`, which is this crate's existing door
for exactly that.

**The move found a payload a caller could not name.** The TCP
`ConnectTimedOut` was `pub(crate)` while `ResolveTimedOut`,
`FirstByteTimedOut` and `BetweenBytesElapsed` beside it were public —
under a module doc arguing that the four are four types *so that a
caller can tell them apart by `downcast_ref`*. The commonest timeout was
the one that argument did not reach. It is public in `error::` now, and
`NoQuicArm`, raised by the routing and reachable by no path, is public
beside it. **A module with one job is what made both visible**: scattered
across the root, a missing payload is an absence nobody reads; listed in
the one place payloads live, it is a gap in a list.

**A second pass took four more, and found what the first had walked
past.** `Conn`, the connection `NativeIo` names, was a `pub enum` with
both variants public; it is a struct over a private enum now, `TokioIo`'s
shape one layer up, and stays generic over the two *streams* rather than
over `R` and `T` — the first attempt made it `NativeIo<R, T>` and asked
`R: 'static` of every pooled connection, where only the stream has to be.
`Prefetch` went private with `Discovered`, because its one outside
caller was `hclient-select` and that routing is `route.rs` now; its
second method, `execute_prepared`, had **no caller at all**, the router
calling `run` directly. And `Prepared` left the public API with them:
`StagedConnect::connect` took one, read only the request out of it, and
every caller built it with `Prepared::new` — a type in a public signature
whose other half nothing read. It takes the request now, as the QUIC
trait always did.

**The owner then took the last three off the root.** `endpoint` — the
bare-QUIC entry point that was `hclient-quinn`'s whole surface — had no
caller anywhere, and `DEFAULT_HEAD_START` and `DEFAULT_KEEP_ALIVE` were
read only by tests. The two numbers are stated where a caller meets them
now, on `Native::hedging` (250 ms) and `H3::keep_alive_interval` (5 s),
and the tests reach the head start through `testing`.

**And the rendered page had lost `Native`'s own documentation.** Its
whole doc — the `Send` argument and both doctest fences — sat on the
private `Versions` struct below it, and five methods' docs were shifted
the same way: `hooks`' onto `proxy`, `watching_1xx`'s onto
`expect_continue`, `multiplexed`'s onto `now` and `h2_keep_alive`,
`network_changed`'s onto `alt_svc_store`, and the QUIC
`StagedConnect::connect`'s onto an associated type. `new` had none at all. Each is a method inserted between a doc block and the item
it described. **`just docs` cannot see it** — the links resolve, the text
simply belongs to a neighbour — and the doctests kept running, because
rustdoc tests private items too. What sees it is `missing_docs`, which
finds the item left with nothing; it is on in `hclient-native` now, where
`just lint`'s `-D warnings` makes it a gate. Across the workspace it
reports 270 more, and those are for another pass.

### Doc comments are for docs.rs, and the argument moved beside them

The rendered documentation was **154 thousand words**, written in this
file's voice: history, rejected alternatives, measurements, test names,
`.notes/` paths, amendment numbers and commit hashes — none of which a
reader who installed a crate can follow. Every public doc was split: the
reference stays in `///`/`//!`, and everything else moved **verbatim** into
a `// Maintainer notes (not rendered):` block directly above the item, so
the argument is still in git and still next to the code it explains.
Rendered text is 127 thousand words after, crate front pages 18.5 thousand
to 11.3.

Nothing was deleted, and that was checked rather than promised: every
comment line removed from a doc had to reappear in the added text, and the
only exceptions are sentences cut in two and a handful of bridging words.
`just doc-comments-speak-to-readers` (in `invariants`, with a CI step)
refuses a doc comment naming `.notes/`, AGENTS.md, an amendment, a commit
hash, a work-item label or a task number — the half of the rule a pattern
can decide. Whether a history sentence helps a caller is review's.

**One process defect worth carrying forward.** The split ran as parallel
agents on disjoint crates in one working tree, and one of them ran `git
stash` to compare against `HEAD` — which stashes everybody's work. Both
stash commits were compared with the tree afterwards and nothing was lost,
but that was luck of timing. Agents sharing a checkout must never run
`stash`, `reset` or `checkout`; a comparison against `HEAD` is `git diff`
or `git show HEAD:<path>`.

### The front page listed 73 items and twelve of them were for the caller

`hclient`'s rendered index was one flat alphabetical list, so **`AnyList` —
a type nobody writes — sat above `Client`**. Counted rather than eyeballed:
73 entries, of which about twelve are what a caller reaches for. The rest
divided into four groups that each wanted a door.

- **14 error payloads**, reached only through `Error::source` — now
  `error::`, with `Error` and `ErrorKind` kept at the root because they are
  on every signature in the crate and a door in front of them is one every
  caller walks through immediately.
- **11 hooks types** re-exported from the core for a seam most callers never
  touch — now `hooks::`.
- **6 capability reports** — now `caps::`.
- **5 response-body wrappers** that are public for one reason only: the
  alias `ClientBody<B, Tm> = Limited<Decompressed<Deadline<Cached<B>, Tm>>>`
  names them, and an alias cannot name a private type. Now `body::`, with
  the alias, so the four wrappers sit beside the thing that needs them.

Plus `sse::`, `redirect::` and `erased::` — the last for `AnyList` and
`AnyStore`, which a caller meets only as `CookieJar<AnyList>`.

**Nothing about a type changed. What changed is the path a reader types**,
and rustdoc renders modules before items, so the page is now 16 names and
12 doors. Free before the first publish and a major version after it, which
is the same window the crate renames used.

**Two orphaned `#[cfg]` attributes came out of the edit, and the second one
is the instructive one.** Deleting a re-export left its attribute behind,
where it silently attached to the *next* item: `#[cfg(feature = "charset")]`
landed on `pub use response::{Collected, Response};`, so without that
feature the two most-used types in the crate did not exist. `cargo check`
with default features never saw it — `charset` is off by default, but
`--all-features` is what the suite runs. **`test-no-default` is what
caught it**, which is the recipe this file records as having once printed
`error:` and exited zero. It earns its place here.

### `hclient-proxy` was audited for a stable number, and the gate had a blind side

Asked what stood between `hclient-native` and a stable version, the
answer began with what `native` exposes — and the first finding was
about the instrument. **`just exposed-majors` read an impl's trait and
its items and never its where-clause**, so `impl Transport for H3 where
T: QuicTlsConnect<Session = Arc<dyn quinn_proto::crypto::ClientConfig>>`
reached no report: on `native` the scan answered eight crates and the
truth is ten, `quinn_proto` and `hclient_dns` among the missing. The
eight published stable crates carry no such bound and all still pass,
so nothing shipped wrong — what the hole would have done is let
`native` itself through with a clean bill. A path through a `__private`
module (`pin-project-lite`'s generated `Unpin` impl) is skipped, since a
macro's plumbing is not a promise.

**`native` exposes `hclient-proxy`**, a pre-release, so that crate goes
first — `hclient-tls` waiting on `hclient-rt` a second time. Its audit,
by the instruments this file trusts:

- **An outside consumer** wrote a third protocol against `Handshake`
  alone — `HELLO host:port`, `OK`, tunnel — and drove a real request
  through `Native::proxy` and `Client`. It compiled and passed on the
  first try, which is the seam's best evidence.
- **It leaked a pre-release of its own**: `ConnectError::Malformed`
  carried `hclient_proto::head::HeadError` with a `#[from]`. It carries
  `MalformedHead` now, an opaque newtype whose `source()` is the
  parser's error, and the public API reaches `bytes`, `http` and
  `hclient-core` and nothing else. `hclient-proto` is still a normal
  dependency — the head parser and base64 — so a stable `proxy` would
  have a pre-release in its graph without one in its surface.
- **`ProxyRefused(pub StatusCode)`** was a tuple struct with a public
  field and no `#[non_exhaustive]`: handed back and only read, so answer
  three. It is `ProxyRefused { status }`, non-exhaustive, with room for
  the `Proxy-Authenticate` challenge a proxy-auth flow will want.
- **One concept had two types**: `system::Scheme` and the root's
  `ProxyScheme`, the same two variants, mapped arm by arm in
  `translate.rs`. `ProxyScheme` is the one left.
- **Three items served nobody outside**: `Proxy::key` (the pool-key
  string, which `native` now builds from `host` and `port` where the pool
  is), `Proxy::protocol_mut` (no caller anywhere, and a doc contradicting
  its own signature) and `drive_for_test`, a `#[doc(hidden)] pub` used
  only by the crate's own unit tests and `#[cfg(test)]` now.
- **Mutation: 334 mutants, 254 caught, 53 unviable, 3 timeouts, 24
  missed**, and 23 of the 24 are artefacts of a Linux host — the
  Windows, Apple and Android readers are never compiled here, the Linux
  `platform()` already *is* `Raw::default()`, and `NoProxy::begin` has an
  uninhabited receiver. **The twenty-fourth was a real defect under a
  note calling it equivalent.** `parse_prefix` re-read an address
  `IpAddr` had refused through `u8::from_str`, which takes `010` as ten
  and `+10` as ten, so a bypass `010.0.0.0/8` covered `10.0.0.0/8` —
  where `inet_aton`, and so the platform that wrote the pattern, reads
  `010` as eight. The fallback takes digits without a leading zero now,
  and a pattern in no accepted shape matches nothing, as everywhere else
  in the matcher. The note was right that `>= 4` and `> 4` agree, for a
  premise that was false until the fix made it true.

**`H3` stays in `hclient-native`, and the owner's call is to pay for it
in the version rather than in a crate.** With the gate reading impl
where-clauses, `native`'s public API reaches `quinn_proto` through
`impl Transport for H3 where T: QuicTlsConnect<Session = Arc<dyn
quinn_proto::crypto::ClientConfig>>`. Moving `H3` into a crate of its
own, with the staged-connect trait left in `native` and `Native::http3`
generic over it, was measured as cheap — `lib.rs` names the concrete
`H3` in that one signature, and everything else already goes through
`http3::arm`'s erasure — and declined for now. So a stable `native`
promises `quinn-proto` 0.11 behind `http3`, as `hclient-tls-rustls`
does behind `quic`, and `quinn` 0.12 is a major version of `native`.
A seam for HTTP/3 may come later; this paragraph is where the
measurement for it lives.

**`hclient-proto` is to go stable ahead of `proxy`**, with frequent
0.x minor steps as the honest label for its churn: no stable crate
exposes its types, so a step costs its dependents a requirement bump
and a patch release, and `just exposed-majors` fails the day one of
them starts exposing it.

### `hclient-proxy` was audited a second time, and three of its distinctions had no reader

The first audit predates the egress seam, so the surface it cleared was
half of what a stable number would now promise. The second one used the
same instruments — a mutation sweep, the rustdoc surface, the two
outside witnesses — and the defects it found were small; the surface
changes were not.

**Two defects, both a wrong answer where an honest one was cheap.**
`HttpConnect::basic_auth` took a `:` in the username, which `hclient`'s
own `basic_auth` refuses (RFC 7617 §2: `a:b`/`c` and `a`/`b:c` are the
same bytes). And a SOCKS5 reply naming an address type RFC 1928 does not
define came back as `Socks5Refused { rep: atyp }` — for `ATYP=5`,
*connection refused*, a reason no proxy gave. It is
`Socks5HandshakeError::BadAddressType` now.

**The sweep: 434 mutants, 36 missed, and the real gaps were the
erasures.** Most survivors are the platform readers this host never
compiles, or equivalents (`<` for `<=` where the next check subsumes it;
`MalformedHead::source`, whose parser error never has one — the impl is
gone). What was real is that nothing in this crate exercised its own
erased path: `BoxHandshake` could drop `Proxy-Authorization` from an
absolute-form request, and `BoxIo` could forward nothing, with the whole
suite green. `hclient-native`'s tests reach both, and a crate whose own
suite does not is one `just mutants` cannot vouch for. After the
repair the same sweep is 427 mutants and 25 missed, every one of them a
platform reader this host does not compile, an equivalent, or a `Debug`
impl.

**Three distinctions were taken out because nothing read them — the
`UpgradeSupport` rule applied before a freeze rather than after.**

- `Attempt::{Unreachable, Refused}` is one `Failed`. Neither permits a
  switch, the transport reads only `into_error`, and the rules could tell
  a refusal from an outage only for their own three protocols, so a
  third-party handshake's refusal arrived as `Unreachable`. Why an
  attempt failed is the error's source, for every protocol alike.
- `Reach` is private, with `Proxy::reach`: since `IpcProxy` a public
  `Proxy` is always TCP, and `host()` answered `""` for a value no caller
  could hold.
- `Proxy::choose` is gone and `serves` is crate-private: first-match-wins
  is `Rules`' rule, and the translation's tests had been routing through
  a chooser no transport called.

**And the rest was the three-answer rule, type by type.** `Target` is
built by the transport and read by a filter, so it is
`#[non_exhaustive]` with `Target::new` — which a filter's own tests
need. `Decision::Filtered` carries a `#[non_exhaustive]` `Route`, and
`FilterSupport` is `#[non_exhaustive]` with `with_datagrams`: both are
built by filters, but from a constructor or a constant, so the attribute
costs nobody anything and the datagram path's next field stops being a
break of every filter. `ProxySpokeFirst { bytes }` took `ProxyRefused`'s
repair. The setters now fail the same way — `userid`,
`password_auth` and `basic_auth` all answer `hclient_core::Error`, of
kind `Other`, with the specific error as its source.

**`system::rules` replaces `http_proxies`**, returning the `Rules` a
transport installs. The old name and its `Vec<Proxy<HttpConnect>>` would
have frozen *HTTP only* into the signature, so installing a machine's
SOCKS entry — still refused, and still the owner's decision — would have
needed new functions; behind `Rules` it is a refusal fewer.

**A third pass read every public item off rustdoc's JSON rather than off
the modules, and found what the first two had walked past.** Forty-nine
items, and five findings:

- **`Socks5`'s derived `Debug` printed the password** — RFC 1929
  credentials sit in it as plain strings, so a `{:?}` of a
  `Proxy<Socks5>` carried them into a log. `Credentials` and
  `HttpConnect`'s sensitive header had each been guarded; the one
  protocol whose credential is not a header had not.
- **`Proxy::bypass` kept a pattern it could never match**, so
  `*.corp.com` — the spelling a person types — silently left a caller's
  exclusion unapplied, while the system reader reported the same
  pattern. `bypass` is fallible now, naming the pattern; `*.x` reads as
  `.x`; the setter and the reader share one `normalize_bypass`, so the
  two dialects cannot drift; and `BypassReason::Malformed` makes the
  reader report `10.0.0.0/33` where it used to install it.
- **`Dial::connect` and `connect_ipc` could not borrow their
  arguments** — `+ '_` on `&self` alone — so both in-tree implementors
  copied the host; `connect_tls` beside them already took one `'a`.
- **`RequestForm::Absolute` was a closed variant** where a hop's other
  headers would go; it is `#[non_exhaustive]` with
  `RequestForm::absolute`.
- **`Route::pool_key` is a `Cow` borrowed from the filter**, the
  owner's suggestion over an `Arc<str>`: `route` is asked several times
  per request, and `Rules` now computes each key once and lends it —
  no allocation and no reference count — at the cost of a lifetime on
  `Decision`.

`Proxy::handshake` went too, being `protocol().clone()`, and `IpcProxy`
gained the `scheme()` its sibling had.

**And `Io` moved to `hclient-rt`, beside the `Shutdown` it names.** It is
`AsyncRead + AsyncWrite + Shutdown + Unpin` under one name with a blanket
impl, and the proxy crate had minted it only because `Wrapped<S: Io>`
needed a word — while the same four bounds were spelled out 29 times in
seven crates. Not `hclient-core`: that would have meant moving `Shutdown`
and `futures-io` there, which is the move declined when `hclient-rt` went
stable first. For the stable crate it is an addition (`cargo
semver-checks`: no update required); `hclient_proxy::Io` stays as a
re-export so a filter author needs one dependency. The stable seams'
own bounds keep their spelled-out form — equivalent, and not worth a
diff in a frozen surface; the internal ones in `native` and the TLS
backends say `Io`.

### `hclient-proxy` was audited a third time, and the sharpest finding was a default

The datagram seam arrived after the second audit, so a third one read it
with the same instruments — a consumer written outside the workspace,
the rustdoc surface item by item, and a mutation sweep — and ended with
the crate set to **`0.1.0`**, every requirement naming it moved to match.
**The owner has since chosen otherwise, and the number came back**: on
2026-10-05 the crate released as `0.1.0-alpha.16`, and the freeze stays a
plan — the audits stand as its preparation, not as its record. The
reversal was free for the reason the sentence below it always stated: the
stable number in the manifest was an intention, and the index is where it
becomes a promise. The index never saw one.

**`SendEgressFilter::open_datagrams_send` had a default, and the default
was a trap.** It refused, as `EgressFilter::open_datagrams`' default
does, so a filter that implemented `open_datagrams` and forgot the
one-line forward compiled without a word — and the erased path, which is
the only one an external filter is ever called through, never reached
the method the filter had written. The request went over a stream
instead, and the failure was recorded in `H3Failures`, so HTTP/3 was off
for that origin until the memory expired. The outside consumer did
exactly that, on purpose, and nothing warned. **A default that answers
differently from the method it mirrors is a defect a type checker cannot
see**, so there is no default: every implementation writes
`Box::pin(self.open_datagrams(t, ctx))`, and a `compile_fail` doctest pins
that leaving it out is `E0046` — which it did not, before, and the
doctest failing on the old tree is the red line.

**The sealed `Associate` was four public items for one value.** A
public trait nobody outside could implement, a public step enum, a
public relay address and a public error — each nameable and none of them
usable, because the only implementation was ours. What a foreign
`Handshake` actually needs is to carry the association of a SOCKS5
handshake it wraps, so `Handshake::associate` answers
`Option<Association>`, one opaque struct with no public constructor and
no public method, and the four are crate-private. The seal went with the
trait, and its intent survives as a `compile_fail` doctest: a foreign
crate cannot build an `Association`. A test outside the crate wraps
`Socks5`, forwards the value, and watches `Rules` declare datagrams for
it.

**The refusals a filter reports had no constructors.** `ProxyRefused`,
`Socks4Refused`, `Socks5Refused` and `ProxySpokeFirst` are
`#[non_exhaustive]` with public fields — handed back and read, which is
the rule's third answer — and that left a filter outside the crate able
to read one and never build one, so a third-party proxy's refusal had to
be a second vocabulary. The masque experiment had minted exactly that,
`Refused { status }`, and the second audit named the gap and did not
close it. Each has a `const fn new` now. `Socks5HandshakeError` took the
`PartialEq` its SOCKS4 sibling already had.

**A plain CONNECT defaults to HTTP/2.** `TunnelRequest::new` asked over
HTTP/3 then HTTP/2 whatever the request was, and a plain CONNECT cannot
go over the HTTP/3 client this family is built on, so the default named
a version the transport would skip. It is HTTP/2 for a plain CONNECT and
HTTP/3 then HTTP/2 once a `protocol` makes it extended — which is what
an HTTP/3 tunnel is for. **The field is `Option<TunnelVersion>` and the
answer is `effective_version()`**, and the shape was chosen over a
first one that kept `version` a plain public value and rewrote it from
`protocol()` unless a private flag said a version had been chosen. That
one was exact through the builder and wrong through the field: the
struct's fields are public to read *and to assign*, so a filter writing
`req.version = Http3` and then calling `.protocol(..)` had its choice
overwritten without a word. With `None` meaning *by the kind of
CONNECT*, nothing rewrites anything, and
`a_version_assigned_to_the_field_survives_a_later_protocol` pins it.

**The four enums a filter builds and a transport matches took one
rule**, and it is the one this file already states for errors crossing a
seam: `#[non_exhaustive]` exactly where the wildcard arm has an honest
right-hand side. `TunnelVersion` has one — refuse, as a transport lending
no tunnels does, which a filter already reads as *try another way* — so
it keeps the attribute, and `hclient-native`'s arm that read an unknown
version as HTTP/3 then HTTP/2 is a refusal now. `Decision`, `Opened` and
`RequestForm` have none: a wildcard could only send a filtered request
direct, carry a stream it cannot read, or write a head the proxy reads
differently, so a new variant must be a compile error in every
transport. Each type says which, and why. `Attempt` took the attribute
for a different reason — a transport reads it only through
`permits_switch` and `into_error`, which answer for every variant.

**The rest was the surface narrowing to what is used.** `DynHandshake`
and `BoxHandshake` are crate-private, nothing outside naming either; the
two `#[doc(hidden)] testing` modules are behind a `test-util` feature
that `hclient-native` and the masque experiment enable in their
dev-dependencies, so the stable number promises nothing about them;
`UnsupportedBypass` and `BypassReason` have one path, the root, where
they had two. `DatagramPath` states what it always did and never said —
a short buffer truncates, silently, as a socket does, and
`max_datagram_size` is a ceiling a transport discovers up to rather than
a size every datagram below it survives, which is the fix
`hclient-native` made to its QUIC-over-a-path start just before this
audit, written on the seam. Every `Dial` method and
its erased mirror has an `# Errors` section, and three doc sentences
that had gone stale — a subnet as an unrepresentable bypass, a bypass
report that is never an `Err`, and three ways an `Attempt` can fail —
say what the code does.

**The sweep: 629 mutants, 423 caught, 167 unviable, 4 timeouts, 35
missed.** Thirty-three of the misses are what they were last time — the
platform readers this host never compiles, equivalents, `Debug` impls
and a test double's own methods. **The other two were one gap**:
`Socks5Path::poll_recv` discards a datagram two ways, one from a stranger
and one from the relay carrying `FRAG != 0`, and only the first was
tested against the per-poll bound, so `discarded *= 1` and
`discarded -= 1` on the second both survived. Sixty-five relay fragments
now make the first poll yield, and both mutants were applied by hand and
died.

### `hclient-proxy` was audited a fourth time, and the question was the width of the promise

The owner opened it with the freeze worry: *the surface is too big, and
after the number is in the index nothing will be removable*. That turned
the audit from *what leaked* into *who names what* — every public item
off rustdoc's JSON, grepped against every consumer (`hclient-native`,
the `hclient` facade, `hclient-masque`, the three egress witnesses), the
compiler as referee. **85 public items; 83 named by a consumer or
reachable through one; two named by nobody, and both left.**

- **`drive`'s only caller was `drive_exact`, its own wrapper.** The
  leftover-bytes variant — the one that returns what the proxy sent past
  its handshake instead of refusing it — had no consumer shape at all:
  the preproxy witness, the exact configuration that would want bytes
  passed on, used `drive_exact`. It is `pub(crate)` now; `drive_exact`
  keeps the door, and its doc says a caller carrying a server-speaks-first
  protocol drives the handshake itself. The module's maintainer notes
  carry the split and its reason.
- **`ParseError` was public and produced by no public function.** Its
  instances are dropped into the `ignored` list as strings during
  `detect()`; no caller could ever hold one. It is the same leak
  `MalformedHead` was built to close, still standing in `system` — closed
  the same way. The caller-visible report of a malformed entry is
  `SystemProxyRefused` and the `ignored` list, and the enum's own doc now
  says so.

**What was measured and deliberately kept, because it looks like the same
class and is not.** The `SystemProxies` accessors — `entries`, `bypass`,
`pac`, `ignored`, `unsupported_bypass` and their siblings — have no
outside caller either. But `SystemProxyRefused::UnrepresentableBypass`'s
own error text tells the caller to *"read `SystemProxies` yourself and
decide"*, and the facade doc redirects the PAC case to
`SystemProxies::detect()`. **They are the reader half the errors name**;
narrowing them would make the error's advice a lie. One has a production
reader (`names_a_proxy` — urlsession); `is_empty`'s only caller is a
native test. **`Proxy::protocol` is the flagged one**: no consumer names
it either — the in-crate rules read it, and outside that its only reader
is the front-page doctest, which teaches the sans-io composition from a
configured `Proxy`. Left public on purpose; the owner's call in this same
window, since after the publish it is a major.

**The sweep: 633 mutants, 427 caught, 169 unviable, 4 timeouts, 33
missed.** The 33 are the third audit's classes unchanged — 20 platform
readers under `system/` this host never compiles (or is equivalent on:
`detect_platform -> Default::default()` answers what a Linux reader with
no settings answers), six `Debug` impls, the test double's
`poll_writable`, three boundary `<`→`<=` and two receive-buffer
`+`→`*`. The classes were believed twice already; this time one
representative was re-applied by hand — `from_peer.len() < 5` → `<= 5`
in `socks5.rs`, suite green — confirming the equivalence: the mutant only
moves which poll answers `NeedMore`. The four timeouts are the known
hangs (`read_some` answering without reading, `percent_decode`'s index
running backwards).

Gates after the narrowing: `lint`, `docs`, `test-doc`, `packaging`,
`versions-agree`, `semver` (1764 checks across 9 stable crates),
`exposed-majors`, `internal-crates-stay-internal`,
`socks5-udp-stays-in-its-module` — all green. All three egress witnesses
pass unchanged on the narrowed tree, which is the narrowing checked from
outside. **And `just docs` earned its keep against its own author**: the
first build after `drive` went private failed on `drive_exact`'s doc
still linking the now-private item three times — the gate catching the
exact defect this section's change introduced.

### `hclient-proto` is internal, and what it held for `hclient` moved into `hclient`

The owner's rule: **`hclient-proto` is an internal crate, and nothing is
re-exported from it.** It is published because its dependents need it on
crates.io, promises no stable interface, and moves its minor version
whenever they need it to. That is safe exactly as long as no crate hands a
caller one of its types, and at the time of the rule three did.

**"Internal" means *no stable interface*, not *not for use*** — the
owner's clarification, and the `windows-sys` model. The crate follows
semver, so an outside crate may depend on it at a pinned minor and take a
breaking release when it chooses; a third-party HTTP engine reusing the
RFC 8305 scheduler is the expected case. What stays forbidden is an
hclient crate *exposing* its types, because that would turn every
breaking release of it into a breaking release of the exposer.

**`hclient` re-exported it in seven places** — the redirect and retry
policy seams, `Backoff`, `Link`/`Links`, `SseEvent` and its size default,
and `UriError`. A trait cannot be hidden behind a re-export, and
`redirect::decide` takes `&dyn RedirectPolicy`, so the policy and its
mechanism had to live together. The crate split cleanly by consumer:
`head`, `encode` and `happy_eyeballs` serve `hclient-native`,
`hclient-proxy` and `hclient-winhttp`; `redirect`, `retry`, `backoff`,
`link`, `lines`, `sse`, `field` and `uri` served `hclient` alone, about
5,850 lines against 1,400. The second half moved into `hclient` as the
private module `sansio`, whole and with its unit tests, and the public
`redirect`, `retry`, `link` and `sse` modules re-export from it — which
makes them this crate's own types. No caller's `use` line moved.

What travelled with it: the `idn` feature and `hclient-idn` (the URI
parser was the reason for both), `percent-encoding`, `winnow` becoming
unconditional (it already was in every default build), the RFC 3986
differential corpus and the retry-policy tests, and the SSE fuzz targets
— the fuzz crate is `crates/hclient/fuzz` now, reaching the decoder
through a `#[doc(hidden)] testing` door, `hclient-native`'s shape. The
graph gates followed their subjects: `graph-no-url` and
`graph-idn-feature` ask `hclient`, and the latter now also asserts that
`hclient-proto` carries no IDN at all.

**And it leaves the pre-release series at `0.1.0`, for the gate rather
than for anybody's confidence.** Inside a pre-release `cargo semver-checks`
runs none of its lints, so an internal crate that breaks often is exactly
the one that most needs a stable number: each break is then a 0.x minor
step the tool can see, and it costs the three dependents a requirement
bump and a patch release, because none of them exposes it. Its graph holds
no pre-release now that `hclient-idn` left with the URI parser.

**It is held by a gate rather than by this paragraph.**
`just internal-crates-stay-internal` reads a crate's
`[package.metadata.hclient] internal = true`, documents every other
publishable library as rustdoc JSON with `exposed-majors`' scanner, and
fails on any path into an internal crate. **Its first run found a leak the
grep had missed**: `hclient-winhttp`'s `WinHttpError::Head` carried
`HeadError` through a `#[from]`, which is `hclient-proxy`'s defect a second
time and got the same repair — an opaque `MalformedHead` newtype whose
`source()` is the parser's error.

**The move surfaced types a caller could reach and not name**, found by
running `-W unnameable_types` rather than by reading.
`ProposedRetry::new` takes an `Outcome` and `Standard::decide` answers a
`Decision` carrying a `StopReason`, and `hclient::retry` exported none of
the three, so a policy author using only `hclient` could not build the
value its own unit test needed. They are exported now. `hclient-winhttp`'s
`Win32Error` was the same shape and is exported too. The lint also names
`Step`/`Plan` enums in `hclient`'s `cache::kv` and `cookie::kv` and in
`hclient-native`'s `altsvc_cache::kv`, which predate this and are left for
their own look.

Two smaller things fell out. `sansio::field::quoted_string_raw` has one
caller, behind `charset`, and was invisible as dead code while it was
another crate's `pub` item; it carries an allow naming that caller now.
And two cookie test files carried five clippy findings that no gate could
see, because they build only under `--no-default-features --features
cookies`; `test-no-default` clippies that combination now.
