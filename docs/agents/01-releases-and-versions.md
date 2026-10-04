# Releases and versions

> Part of the [AGENTS.md](../../AGENTS.md) record, split 2026-10-05. The root
> file keeps the operational rules and the index of parts. Sections below
> keep their original headings, and a "this file" in the text named the
> whole record when it was written.

## Status

v0.1: the core (`hclient-core`, `hclient-proto`, `hclient`) and three backends
— `hclient-wasi` on top of `wasi:http` 0.3 (vertical 1), `hclient-native` on
top of `hyper` + `rustls` + system DNS (vertical 2), and `hclient-fetch` on the
browser's `fetch` (vertical 3).

| target | transport | tokio in the graph |
|---|---|---|
| native | `hclient-native` — TCP + HTTP/1, HTTP/2 behind the `http2` feature, TLS pluggable | yes, on the h1 path |
| native | `hclient_native::H3` — QUIC + HTTP/3, a module rather than a crate since `f4dfe48`, TLS through a second seam | yes |
| WASI | `hclient-wasi` — `wasi:http` 0.3 | **no** |
| browser | `hclient-fetch` — `fetch` | **no** |

Both "no"s are machine-checked on every push rather than asserted here:
`ambient-has-no-tokio` runs `cargo tree` for `hclient-fetch` against
`wasm32-unknown-unknown` and for `hclient-wasi` against `wasm32-wasip2`, and
fails closed if the invocation itself breaks or returns nothing. Measured
while writing: zero matches for `tokio`, `hyper` or `h2` in either wasm
graph, four in the native one.

**The owner has pulled the trigger, and the first published version is
`0.1.0-alpha.1` rather than `0.1.0`.** The reason is not doubt about the
code — 19 CI jobs across three platforms are green — it is that the last
week moved six public surfaces: `Client` from generic to erased,
`Client::transport()` removed, `Client::new`'s error type widened and
`try_new` folded into it, `Response<B>`'s parameter defaulted,
`hclient-idn`'s answer changed three times, and `poll_shutdown`'s treatment
of `ENOTCONN`. Publishing that as `0.1.0` would freeze twenty-nine public
surfaces at the moment they were last seen moving.

A pre-release claims the names and promises nothing, so the next week of
changes costs `-alpha.2` rather than a major version across the family.

**The family shares one pre-release series**, with `hclient-idn` and
`system-resolver` versioned separately for the reason two sections down,
and `hclient-core` stable for the reason one section down. **What those
numbers are is not written here, deliberately.** The index is the
authority; `cargo search`, crates.io, or `just versions-agree` answers
it, and each of them is right by construction where a sentence is right
only until the next release.

**That is a rule this paragraph earned by breaking it twice.** It
carried the figures, went stale, was corrected with a note admitting it
had drifted three alphas and a patch — and then drifted four and two,
including once in the same session that corrected it. A figure in prose
has no gate behind it and nothing forces the reading that would fix it,
so the repair is not a fresher number: it is naming where the number
lives and stopping.

**`hclient-core` has left the pre-release series and is stable in the
index.** The argument for going stable was the gate rather than
confidence: inside a pre-release `cargo semver-checks` executes **0 of
its 254 lints**, because every step out of one is a major step, so the
crate whose types cross every boundary in this family was the one crate
no tool had ever examined. A stable version gives the gate a subject,
since `just semver` selects on *published and not a pre-release*.

**It enrolled itself the moment the index moved**, with no edit to the
recipe: the count `just semver` prints went up by one crate and by that
crate's worth of lints. The count is not written here for the reason the
paragraph above gives — run the recipe, which is the authority and
cannot go stale.

**`hclient-mock` is the third to leave, and choosing it over its
neighbour is the part worth recording.** Both it and `hclient-dns` pass
the structural test — each depends on `hclient-core` alone, which is
already stable, so neither drags a pre-release into a stable graph
(`cargo tree -e normal` finds no `alpha` under either), both have small
surfaces, and both carry `#[non_exhaustive]` where additive growth would
otherwise break. What separated them is the calendar: **`hclient-dns`
was broken twice two days earlier** — `99672f92` and `fda77383`, the
second closing a `domain` leak this file records as having *outlived the
decoder that leaked it*. Freezing a seam two days after it was last
repaired promises exactly what had just stopped being true, which is the
rule about a stable number in the index being a promise rather than an
intention, met from the side where the subject is a seam still moving.

The positive case is narrower than *it looks finished*. `hclient-mock`'s
surface is what other people write **in their own tests**, and its shape
was settled by an outside consumer rather than by an internal seam — so
it is both the surface that hurts most to break and the one a working
`cargo semver-checks` is worth most on.

**And the commit does not move the gate, which is the same lesson a
third time.** After the bump `just semver` still prints its old figure
and still lists `hclient-mock(0.1.0-alpha.11)` among the pre-release
baselines, because the recipe reads the **index** and the index has not
moved. Measured rather than assumed — `cargo search` answers
`0.1.0-alpha.11` on the same tree whose manifest says `0.1.0`. The
enrolment happens on publish, exactly as `hclient-idn`'s did, and
nothing here needs editing for it.

**`hclient-wasi` is the fourth, and it is the cheapest kind there is.**
Two things separate it from every other candidate. It is **terminal** —
nothing in this workspace depends on it, so there is no requirement
naming its version anywhere and the change is one line in one manifest,
against `hclient-mock`'s three. And its surface is **three public
items** over 2,542 lines, last broken on 2026-09-10 by a rename that
swept the whole family rather than by anything of its own.

What makes freezing it worth more than the arithmetic suggests is that
the surface is **executed rather than compiled**: `just test-wasi` runs
18 live tests under a real `wasmtime`, `wasi_transport_round_trips_a_real_response_through_wasmtime`
among them. A stable number on a surface no gate executes would be a
promise about something nobody has run — which is the objection that
keeps `hclient-fetch` in the alpha series, where all 13 test binaries
are `#![cfg(target_arch = "wasm32")]` and the workspace run finds
nothing to execute.

**The rule the four together produce is about blast radius rather than
readiness.** `hclient-core` had to go first because it is the gate's
whole subject; the three since are the ones where being wrong is
survivable — a test double, and a terminal backend a caller chooses.
What stays in the series is everything a *seam*: `hclient-rt` carries
`TcpConnect`, `Timer`, `Blocking`, `Spawn` and `UdpBind` with eleven
in-workspace consumers, and this file records `TCP_SUPPORT`, `type Sleep`
and `UdpBind` itself arriving inside the last few weeks. Those are not
waiting on confidence, they are waiting on the seams to stop growing,
and that is a decision to take deliberately rather than as a companion
to somebody else's bump.

**`hclient-tls` is prepared and is not the sixth, and the reason is a
bound rather than the crate.** Its surface was audited type by type:
mutation leaves two survivors, both equivalent (a default whose body is
already the mutant's value); `TlsRequest` is `#[non_exhaustive]` with
`TlsRequest::new(server_name, alpn)` and setters, `QuicTlsRequest`'s shape,
because the transport builds it and a backend only reads it; the two
**reserved** fields went before the freeze rather than after —
`TlsRequest::early_data: Option<usize>`, read by no backend and with no
client-side meaning (rustls' `max_early_data_size` is a server field), and
its answer `TlsInfo::early_data_accepted` — since a stable field is a
promise about a shape and nobody had designed this one; and each type has
one path, `tcp` being a private module named from the root. Checked from
a crate outside the workspace: the builder compiles, a literal is
`E0639`, `hclient_tls::tcp::..` is `E0603`.

What blocks it is `TlsConnect::Stream<S>`'s bound, which names
`hclient_rt::Shutdown` — and `hclient-rt` is in the series for the reason
the paragraph above gives. A stable crate whose public bound names a
pre-release trait promises that trait's stability without owning it. The
owner's call is **`hclient-rt` first**, over moving `Shutdown` into
`hclient-core`: so `hclient-tls` waits on that crate's seams settling, and
nothing in it needs to change when they do.

**`hclient-rt` has been through the same audit, and one step is left.**
Mutation leaves **no** survivors (89 mutants: 68 caught, 21 unviable);
its graph holds nothing pre-release; the three support reports share one
shape and are checked both on entry and at configuration. The one design
question was `Spawn::spawn`'s `()`, and the owner's answer is that it
stays: **a runtime accepts every future it is handed**, states whatever
that needs on its own type, and a runtime whose spawn can run out —
embassy's fixed task pool — does not implement the trait, losing only
`Native`'s opt-ins. The last check was the one this file trusts most, a
runtime written **from outside the workspace** on plain tokio types and
run under `Native` and `Client` over TCP, a Unix socket and UDP. Every
seam type-checked on the first try; the one place its author had to
guess, `poll_close` on a stream that also has `Shutdown`, is now written
on the trait. What remains is mechanical: the version and the
requirements naming it, then the owner's publish.

The same sweep found `docs/api-stability.md` recommending *a new seam
method arrives defaulted* — which cannot work for an async method, whose
associated future type has no default on stable Rust, and which is why
`connect_unix` became `IpcConnect`. The policy there is a variant for a
new kind of request and a trait for a new capability.

**`hclient-tls`'s blocker is gone with `hclient-rt` 0.1.0, and its audit
moved a type it does not own.** Mutation leaves the same two equivalent
survivors; a TLS backend written from outside the workspace over
`futures-rustls` served HTTPS under `Client` and refused an untrusted
certificate as `ErrorKind::Tls`, and what its author had to work out —
a newtype to carry `Shutdown`, and why forwarding to the library's
`poll_close` is right — is written on `TlsConnect::Stream` now. The
sharper finding was `TlsConnect::tls_support`'s return type,
`hclient_core::caps::TlsSupport`: three undocumented variants, one of
which (`ServerTrustCallbackOnly`) no backend had ever produced, while
`None` was said both by `NoTls` and by the browser — so *`https://` will
fail* and *the platform does it* were one value. The owner chose three
reachable states: `None` for no TLS, **`Platform`** for TLS the OS or the
browser performs and nothing here configures (`fetch`, `wasi`,
`urlsession`, `winhttp`), `Full` for TLS this client configures. It is a
breaking change to `hclient-core`, taken now because freezing
`hclient-tls` would otherwise have frozen the ambiguity with it.

**It ships as `hclient-core` 0.2.1, a deliberate semver break inside the
0.2 series, and the measurement is why.** The honest number was 0.3.0,
and 0.3.0 is not the break it looks like — it is **four**. `hclient-rt`
re-exports core's `Timer`, `hclient-dns`'s `Resolve` returns core's
`Error`, and `hclient-mock` implements core's `Transport`, so each exposes
core types and each would change which core it exposes. A runtime written
against `hclient-core = "0.2"` and `hclient-rt = "0.1"` stops compiling
the moment `hclient-rt` moves to core 0.3 — `E0277`, *multiple different
versions of `hclient_core`* — while the same program against the
published 0.1.0 compiles. And **release-plz proposed only patch bumps
for those three**, because `cargo semver-checks` compares a crate's own
shape and a re-exported trait keeps its shape while becoming another
trait. So the release as planned would have shipped three semver breaks
labelled as patches, to buy one labelled honestly.

The owner's call is the opposite trade: one break, in the one enum it
touches, confined to a variant no backend ever produced
(`ServerTrustCallbackOnly`) and a variant nobody could have matched on
yet (`Platform`), a few days after 0.2.0 reached the index. Under
`[patch.crates-io]` standing in for the publish, the same outside runtime
resolves one core and compiles. The workspace requirement is `0.2.1`
rather than `0.2`, because the four ambient backends name `Platform` —
which also keeps `cargo package -p hclient-tls` refusing until the core
is in the index, so the stable TLS seam cannot be published against
0.2.0's enum by mistake.

**Two consequences to know before touching the release.** `just semver`,
run by hand, now **fails** on `hclient-core` — `enum_variant_added` and
`enum_variant_missing` against 0.2.0 — and that failure is the decision
being reported, not a defect to fix by bumping. And the gap it exposed
is general: **a dependency whose types a stable crate exposes cannot
change major without that crate changing major too**, and nothing here
checks it — not `cargo semver-checks`, not release-plz, not
`versions-agree`. It was written here rather than gated, which this
file's own rule says is the weaker of the two, and **it is gated now**:
`just exposed-majors`, its own CI job, reads each stable crate's public
API out of rustdoc's JSON, keeps the external crates it reaches that are
direct dependencies, and fails when the requirement on one has left the
compatible range of the published release while the crate's own version
has not. Checked in the failing direction by setting core to 0.3.0 and
leaving its dependents alone: it names all five, `hclient-rt`,
`hclient-tls`, `hclient-dns`, `hclient-mock` and `hclient-wasi`, where the
first draft stopped at the first. It needs nightly for the JSON and the
network for the published manifests, which is why it is not in
`invariants`. Its first run on the current tree agreed with the manual
audit it came from, crate by crate: `hclient-core` exposes `http`,
`http-body`, `bytes`, `futures-core` and `futures-sink`, and `hclient-idn`
and `system-resolver` expose nothing.

**The two runtimes and the two TLS backends were audited next, in
parallel, and the first result is a correction to a sentence written two
days earlier.** `hclient_rt::Spawn`'s doc named `TokioHandle` as the
runtime that *carries what it needs and is total everywhere*. A tokio
`Handle` does not keep its `Runtime` alive: drop the runtime and
`TokioHandle`'s `spawn` accepts the future and tokio discards it unrun —
no panic, no error, exactly what the contract forbids. Nothing can
detect it, since tokio has no *is shut down* query on a `Handle`, so it
is a precondition stated on the type and pinned by a test that fails if
tokio's behaviour changes; the `Spawn` doc now names `Smol`, whose
executor is process-wide and never shuts down, as the one that really
carries what it needs. **Carrying a handle is not carrying what it
needs**, and the difference was found by an outside consumer dropping
the runtime and watching.

**All four were weaker than their test counts suggested, and in the same
way: the tests pinned what the author had just changed.** When the seam's
half-close moved to `Shutdown::poll_shutdown`, every test moved with it,
so `hclient-tls-native-tls`'s `poll_close` was left sending
`close_notify` without the FIN, and `hclient-rt-tokio`'s own suite
pinned none of its write path, half-close or Unix path. The rustls
backend's backpressure test never reached backpressure, its QUIC
*label reaches the session* test could not fail, and its test targets
built only under `--all-features`. Survivors fell from 20 to 5 (tokio),
7 to 2 (native-tls) and 32 to 6 (rustls), each remaining one classified.
Two were behaviour defects: native-tls **substituted its own identity**
for a caller's label rather than refusing it — the one failure
`docs/mtls-design.md` exists to prevent, unreached only because
`hclient-native` refuses first — and **re-polled a pending close called
OpenSSL's `SSL_shutdown` again**, which then tried to *read* the peer's
alert and failed.

**Both TLS backends re-export what their constructors take, and the two
answers differ on purpose.** `hclient-tls-rustls` re-exports all of
`rustls` (and `quinn_proto` under `quic`), because a caller builds a whole
`ClientConfig` and a second rustls compiled in beside it can bring a
second crypto provider, which makes `ClientConfig::builder()` panic.
`hclient-tls-native-tls` re-exports `Certificate` and `Identity` and
nothing else, the owner's call: those two are all its constructors take,
and re-exporting the whole crate would make the rest of it part of this
API with nothing gained. Its major version was ours already through those
two signatures; what the re-export removes is the caller having to guess
the version.

**`hclient-rt-smol` names `SmolSleep` where it named `async_io::Timer`**,
for the same reason one layer down: `Timer::Sleep` is public, so the
concrete timer made `async-io`'s major version this crate's too, for a
value a caller only awaits.

**And its connection type is opaque now, which reversed the sentence that
stood here for one commit.** It said `async-net`'s streams stay public
inside `SmolSocket` because reading the socket back is what that type is
for. That is true of the *purpose* and did not need `async-net`: reading
a socket back means reaching the file descriptor, and the standard library
already has a trait for that. `SmolIo` is a struct with a private enum, like
`TokioIo`, and implements `AsFd` on unix and `AsSocket` on Windows, so
`socket2::SockRef::from(&io)` reaches every option the runtime sets. The
crate's own option tests read everything back that way. The old
`SmolSocket::tcp()` returned an `async_net::TcpStream`, which put a second
crate's major version in the API only to serve that read-back.

**A public surface audit of the four found three more, all applied.**
The rustdoc JSON of each crate was walked for every public item and every
foreign type it reaches:

- `hclient-tls-native-tls`'s `TlsStream::negotiated_alpn` and
  `peer_certificate_der` are `pub(crate)` now. Their only callers were in
  the crate: the handshake already hands both to a caller in `TlsInfo`,
  and the rustls `TlsStream` never had them.
- Its builders are `with_client_identity` and `with_root_certificate`. The
  first one was named `identity`, which next door in
  `Rustls::with_identity(name, config)` means a *named* identity that a
  request selects. This backend refuses named identities, so the same word
  meant opposite things in the two backends.
- `TokioHandle` implements `IpcConnect`, delegating to `Tokio` as its TCP
  connect does. Without it `Native::unix_socket` could not be built over a
  handle.
- `hclient-tls-rustls`'s `WebpkiRootsFeature` stub trait and the stub
  `with_webpki_roots` are `#[doc(hidden)]`. The trait exists only
  *without* a feature, so enabling the feature removed a public item.
  `hclient`'s `DefaultTransportFeature` has the same shape and is left as
  it is, because that crate was not part of this audit.

**A polish pass after it made the two runtimes read the same, and it
came from the fix above.** Once `SmolIo` reached its socket through `AsFd`,
`TokioIo` was the odd one out: its `get_ref` and `into_inner` handed back a
`tokio::net::TcpStream` and **panicked on a Unix-domain stream**, a
`# Panics` section documenting an accessor that could not answer every
connection its own runtime makes. Both went, and `TokioIo` implements
`AsFd`/`AsSocket` as `SmolIo` does, so reading an option back is
`socket2::SockRef::from(&io)` over either runtime and panics over neither.
The UDP sockets of both runtimes gained the same impls, documented as
read-only: `UdpDatagrams::support` was measured at bind, so an option
changed through the descriptor afterwards is one the report no longer
describes. `TokioHandle` gained `From<tokio::runtime::Handle>`, and all four
crates open with a compiled example of the type a caller picks, where two
of them had none.

**The runtimes' `udp` feature is gone, and `quic` on the rustls backend
stays, because the same rule gave the two opposite answers.** A feature
earns its place by the dependency it holds. `udp` held one crate,
`quinn-udp` (and `libc` on Windows), whose types never reach either
runtime's API because `hclient_rt::EcnCodepoint` is what crosses the seam.
So it cost nothing to make unconditional: `hclient-rt-tokio` goes from 21
crates to 22 and `hclient-rt-smol` from 40 to 41. And it had cost
something to keep: every consumer had to write `features = ["udp"]`, and
`hclient-native` once did not build on its own for want of that line.
`quic` holds eleven crates, `quinn-proto` among them, and `quinn-proto` is
public through `QuicTlsConnect::Session` and the re-export. It is at 0.11,
where any minor release may break, so without the feature its major would
become part of the promise to every caller who never speaks HTTP/3. The
seams themselves, `UdpBind` and `QuicTlsConnect`, were never behind a
feature in either case.

**`hclient-tls-rustls` 0.1.x lasts exactly as long as rustls 0.23**, and
the migration is a redesign rather than a bump. The owner's call is to stay
on `0.23.45` and ship a breaking release when 0.24 is out. Read in
`0.24.0-dev.1`: `ClientConfig::client_auth_cert_resolver` becomes private
with no setter, and the recording wrapper that lets this backend answer
`ClientCertAsk::NotAsked` re-wraps exactly that field on a config a
caller built — so under 0.24, `from_config` answers `Unobserved` unless
its input changes. `quinn-proto`'s `Arc<dyn ClientConfig>` is public API
through `QuicTlsConnect::Session` even for a caller who never enables
`quic`, because a feature-gated public type is still public.

**`hclient-dns` is the fifth, and the owner's correction is worth more
than the crate.** It had been held back twice on the ground that its
surface was *broken two days ago* — a calendar rule, and the answer to
it was one sentence: **API stability is judged by whether the API is
right, not by how long nobody has touched it.** The calendar is a proxy
that fails in the direction that matters: by it, an abandoned crate with
a bad surface is the ideal candidate, and one repaired yesterday is the
worst.

Asked the right question instead, the crate answers on every count, and
each was read rather than assumed:

- **`Resolve` is the shape that makes the next record type additive** —
  one associated type, `lookup(name, rtype)`, `supports(rtype)`, where
  three of each used to be. TLSA, CAA and SRV are `RData` variants and
  change the trait not at all; under the old shape each was a fourth
  method every outside implementor had to grow.
- **An associated `Records<'a>` rather than an RPITIT**, so each
  implementor answers for its own `Send` — amendment C15, the thing
  that lets `hclient-rt-embassy` exist at all.
- **`#[non_exhaustive]` is decided per type by the three-answer rule**,
  checked on all seven. `Record`, `RData` and `SvcbEndpoint` carry it —
  handed back and only read. `RawParam` and `SvcbRecordError`
  deliberately do **not**, and their reasoning is written where they
  are: `Other(u16)` already absorbs every unmodelled key, so a new
  variant means *this crate now parses that parameter* and must be a
  compile error at every reader; and `SvcbRecordError` crosses a seam
  into two translators, where a `_` arm is a mapping rather than a
  catch-all.
- **No foreign type in the public surface** — verified against the
  *rendered* rustdoc rather than by grep: `bytes::Bytes` for an ECH
  config list and `futures_core::Stream` for the seam itself, and
  nothing else. `domain`, the leak `fda77383` closed, appears nowhere.

So what 2026-09-16 actually records is not a seam still settling but a
surface **cleared for exactly this**: `fda77383` took `domain` off the
public API and nothing has touched the crate since. Reading the diff is
what separates those two, which is the qualifier the calendar rule never
had.

**The episode this replaces is kept, because the distinction it drew is
the durable part.** The number was set to `0.1.0` on 2026-09-10 and
taken back to `0.1.0-alpha.8` days later, and what made that reversal
free is that it had never reached the registry: crates.io's newest was
`0.1.0-alpha.7`, so there was no promise to withdraw, no yank, and no
consumer resolving against a version about to change meaning. **A stable
number in a manifest is an intention; a stable number in the index is
the promise.** Only the second is expensive, which is why the gap
between them is where such a decision belongs — and why getting it wrong
the first time cost nothing.

`alpha.8` rather than back to `alpha.7` at the time, because that number
was taken and the tree had moved a long way past what it held.

**And the gate proved the property in both directions.** When the number
was reverted `just semver` went straight back to `392 checks across 2
crate(s)`, exactly as it had gone to that figure when `hclient-idn`
published `0.2.0`; when the number published it went to 588 across 3.
Neither move edited the recipe. What it costs while a crate is in a
pre-release is stated rather than papered over: that crate is one
`cargo semver-checks` cannot examine until a stable pair exists on both
sides — and `hclient-core` was that crate for six days, during which
**this paragraph went on describing the reversal as the current
state**. A claim about a version is as perishable as the version.

Two things landed before the number moved, and both stand. `bon` left the crate — its generated builders put
`SetConnect<S>` and `IsUnset` into every setter's signature from a
`#[doc(hidden)]` module, so a caller could meet those names and not write
them, which is exactly the hole a freeze must not preserve. Two `const fn`
constructors replaced them, and the graph went from 21 crates to **13** —
`bon` took its whole proc-macro subtree with it, which is more than the
five the plan predicted. And the gate was checked in the failing
direction against a git baseline while the number was stable — the
measurement is what survives the reversal, because **the vacuum follows
the version numbers, not the baseline's source**: `alpha.7 -> alpha.7`
and `alpha.7 -> 0.1.0` both run 0 of 254 lints, and only a stable pair on
both sides makes the tool execute anything (`0.1.0 -> 0.1.0` with a
method narrowed to `pub(crate)` runs 196 checks and fails one, naming it;
the same pair unchanged runs 196 and passes). So a first stable publish
can never be checked against anything, whenever it comes — what it buys
is that every release after it is checkable, which is the argument for
making it and is unaffected by the timing.

**The guard is real and it is not yet in force, which this sentence used
to get wrong.** It read that `cargo add hclient` will not select a
pre-release without being asked. Measured on 2026-08-29, from a fresh
crate against the registry: `cargo add hclient` selected
`0.1.0-alpha.2` — because there is nothing stable for it to prefer. The
moment a stable version exists, `cargo add` takes that and a pre-release
needs asking for. So the protection begins at the first stable release,
not at the first publication — and since `0.1.0` was reverted before it
reached the index, **it has not begun**: `cargo add hclient` still
selects a pre-release today, for want of anything stable to prefer. The
stable number follows when the seams have stopped moving on their own,
and this session moved several.

The version said itself **once**, in `[workspace.package]`, where thirty
copies before it had been thirty chances to drift with no way to see the
drift until a crate published at the wrong number.

**It says itself thirty times again now, and that is deliberate** — see
the release section below. A shared number cannot advance for some
members, so per-crate releases needed per-crate versions; what replaces
the guarantee is `just versions-agree`, which resolves every requirement
to the crate it names and compares against *that* crate's version. The
drift the layout used to forbid is a thing a gate catches instead.

Everything that used to be *do not do this as tidying-up work* is now
either done or the owner's to time. What has **not** changed is the reason
the rule existed, and it is worth reading before the first `cargo publish`
rather than after: publishing is a promise not to break, and this
workspace has been breaking things weekly on purpose.

**Measured rather than recalled, over the last 31 commits alone**: six
public types took a change that would have been a major bump, and **not
one of them is `#[non_exhaustive]`** — `TcpOpts` and `TcpSupport`
(6 fields to 10), `Timeouts` and `TimeoutSupport` (3 to 4, the `resolve`
bound), `Phase` (a fifth variant, so an exhaustive `match` outside this
workspace stops compiling), and `Connected::remote`, which became
`Option<SocketAddr>` for the Unix-socket work. Before that week
`TlsConnect` changed three times in one session (`reports_alpn`, the
`TlsIdentity` extraction, the 0-RTT slots), `Timer` gained `type Sleep`,
`TcpConnect` gained `TCP_SUPPORT`, and `UdpBind` arrived from nothing.

So the freedom that made those changes cheap is what ends here, and naming
it is the point: a change to a public trait has cost a rebase in this
repository and nothing at all outside it, which is why every seam could be
chosen on its merits rather than on what was already promised. After the
first publish it costs a major version, and the honest options are the
ordinary ones — `#[non_exhaustive]` on the structs whose whole use is
`Struct { one: .., ..Default::default() }`, or a `0.x` series where
`0.2.0` is allowed to break. `H2Opts` and `TcpOpts` already carry the
argument for *not* adding the attribute, in their own doc comments, and it
is about ergonomics rather than about semver; the two now have to be
weighed against each other rather than only one of them stated.

What is still missing is enumerated at the end of
`.notes/v01-acceptance.md`, `.notes/v02-acceptance.md`,
`.notes/v03-acceptance.md` and `.notes/v04-acceptance.md`. Those lists are
themselves the thing to check first: several entries on them were built
after they were written.

The mechanics are in place and were measured, not assumed. All **27**
publishable crates carry `description`, `license` and `repository`; inter-crate dependencies
carry `version` beside `path`, without which nothing here could be
published at all. Three crates are `publish = false` —
`hclient-rt-pair-check`, `hclient-rt-nal` and `hclient-rt-embassy` — and
this sentence said there was one for as long as there had been two, which
is the count-in-prose defect this file records about itself elsewhere:
`just packaging` derives the figure from `cargo metadata` and does not go
stale, and a number written here does.
`cargo publish -p hclient-core --dry-run` packages **and verifies** clean,
and `cargo package -p hclient` correctly refuses, because its dependencies
are not in the index — which is what the order is for.

**That order was recorded here as "five waves over 26 crates" and it is
six over 25**, which is the difference between two questions rather than a
miscount. Five is the *normal* dependency graph; `cargo publish` also has
to satisfy **dev-dependencies that carry a version**, of which there are 32
here, and they add three waves. `hclient-core`/`-cache`/`-cookie`/`-idn`
are still first and the terminal backends still last, and the chokepoints
in between are one crate wide: `hclient-tls-rustls`, then
`hclient-native` — two, since `hclient-h3` folded into the second.
**Nothing follows that order by hand any more**: `cargo publish
--workspace` is native since cargo 1.90 and computes it — measured on this
tree, 29 packaged, 29 verified, and its ordering identical to the one
derived here from `cargo metadata` before either tool was consulted. The
table is kept because that agreement is what makes the count a fact about
the graph rather than a guess. What cargo does not do is the **bump** —
each crate's own version, and the literal version requirements naming it,
dozens of them, counted by `just versions-agree` rather than written down
here. Cargo offers no way to write `version.workspace = true` inside a
dependency requirement, so the repetition is forced, and the tool that
rewrites it is `release-plz`.

**The policy was one shared version and every crate published on every
release, and it ended on 2026-09-07.** Its argument was that it removed a
question rather than answering one: selecting means knowing which crates
changed, and knowing means a step that can be forgotten, where publishing
everything cannot. What ended it is a tool that *computes* the set —
`release-plz` replaced `cargo-release`, which has no change detection at
all: measured, with a tag one commit back, a plain `cargo release patch`
still planned all 23 uploads.

**Every crate carries its own literal version now**, and
`[workspace.package].version` is gone. That was the structural half:
`version.workspace = true` made "publish everything" the only expressible
policy, because one number cannot advance for some of 28 crates.

**And it publishes everything anyway today, which is the tool being right
rather than failing.** Asked crate by crate on a pristine tree,
release-plz answers `hclient-otel: already up to date`, and the same for
`hclient-tower` and `hclient-webtransport`. What produces the full sweep
is dependency propagation: `hclient-core` changed, every crate here
depends on it transitively — computed, 8 changed and **30 affected, 0
untouched** — and a dependent must bump so its requirement can name a
version that exists. The saving arrives for a change confined to a leaf
and not for one touching the core.

Two wrong causes were proposed before that measurement — `version.workspace
= true`, and `.cargo_vcs_info.json` differing in every packaged crate.
Both are real differences and neither was the reason; a pristine clone at
the pre-change commit, with the shared version intact, bumped all 27
exactly the same. **A controlled test with one variable changed is what
settled it, and it should have come first.**

**Publishing is `cargo publish --workspace`, not `release-plz release`.**
That command calls `get_git_client(input)?` on the third line of
`release()` — read in `release_plz_core` 0.37.2 — before any per-package
config and before deciding whether to release at all, because it asks the
forge which pull requests are associated with the current commit. So it
needs a forge token on a repository with no PR flow and no GitHub
releases, and neither `git_release_enable = false` nor `release_always =
true` avoids it. `cargo publish --workspace` needs no forge, computes the
upload order itself, and verifies each crate out of its own tarball.

So: **release-plz decides the versions, cargo does the upload.**

**Commit subjects carry a conventional-commit prefix now, and that is
what the level is derived from.** They did not: nought of the twenty-five
subjects before the migration parsed as one, because a subject here is the
record of *why* and reads as a sentence. The prefix goes in front of that
sentence rather than replacing it —
`feat!: dissolve the \`unversioned\` quarantine` — so `feat`, `fix`,
`chore`, `docs`, and a `!` or a `BREAKING CHANGE:` trailer for a major
step. That is a real cost paid for computed release sets and changelogs,
and it is the objection `docs/competitive-gaps.md` had recorded against
release-plz before the tool was taken.

**`just release-pending` is gone with the policy.** It answered which
crates had changed since they last published, anchored on a git tag —
which is what cargo-release could not do for itself. release-plz computes
that set, so the recipe became a second opinion about what to publish, and
it carried an obligation: one tag per crate per release, planted so a
diagnostic could read them back. To see what changed, run `release-plz
update` and read the plan; it edits the working tree and uploads nothing.

**Two crates were already outside the shared version before it ended**,
and the measurement that bought them the exception is the one that still
explains the whole shape. `system-resolver` and `hclient-idn` left because
a shared version cannot leave pre-release while any member still needs to,
and **inside a pre-release `cargo semver-checks` checks nothing**: measured against 0.50.0, both
`0.1.0-alpha.2 -> 0.1.0-alpha.2` and `0.1.0-alpha.2 -> 0.1.0` execute **0
of its 254 lints**, because a major step permits breaking — while `0.1.0
-> 0.1.1` executes 196 and caught a breaking change on the first try. So a
compatibility job over the family would be green for a tree in which every
promise had been broken, which is *a check that cannot fail* with the
subject changed. `just semver` is the gate, and it fails closed on a run
that executed nothing as well as on one that failed, because
cargo-semver-checks prints `0 checks` beside `no semver update required`
and exits zero.

**It was a step in the `lint` job and is not any more**, because
release-plz runs cargo-semver-checks too and does the opposite with the
answer: it bumps the major version and reports, where the recipe *failed*.
Both would mean a breaking change fails CI and is then released anyway.

What that gives up is named rather than glossed, because it is real. The
recipe also failed closed on a run that executed **zero** lints — the
pre-release state most of this family is in, where every step is a major
step and all 254 lints are skipped — and release-plz has no equivalent: a
package it cannot check is one it does not bump for. The recipe still
exists for running by hand.

The thing that made it a gate at all is narrower than *the crate is
stable*: a working tree at the **published** number is `no change; assume
minor` and runs **196** checks, where the pre-release pair above is
`assume major` and runs none. So the vacuum belongs to the pre-release
rather than to equal version numbers. Verified against the published
crate: marking `Error` `#[non_exhaustive]` exits 100 naming the lint and
the item.

**What was the exception is now the rule**, and the cost it carried is
what every crate carries: nothing here notices a bump that was forgotten,
because there is no single number whose absence would show.

`just versions-agree` is what replaces the guarantee, and it needed **no
change** when the shared version went — which is worth knowing, because it
is the one gate that could have gone silently stale. It resolves every
in-workspace requirement to the crate it *names* and compares against that
crate's version, and it has done so since `system-resolver` left the
shared group. The old rule is what this one implies for a family that
happens to share a number; the exception needed no entry in a list, which
is what kept it right when the family stopped being one.

So the drift a shared version made structurally impossible is now a thing
a gate catches rather than a thing the layout forbids. That is the honest
cost of the trade, and it is the reason that gate is not optional.

**What guarantees the set resolves is the requirement, not the matching
numbers** — and that was written here as a reassurance, which is exactly
where it went wrong. The published `hclient` 0.1.0-alpha.2 asks
`^0.1.0-alpha.1` of each neighbour; semver is indeed the mechanism, and
the mechanism was **making a promise nobody had made**. Measured on the
published crates, not reasoned about: a caller who pins `hclient-core =
"=0.1.0-alpha.1"` beside `hclient = "=0.1.0-alpha.2"` gets a resolution
cargo accepts and rustc refuses — *cannot find `Reduced` in
`hclient_core`*, because that type arrived in alpha.2. A pre-release
promises nothing between alphas, which is what this file says a
pre-release is **for**; a requirement spanning two of them says the
opposite.

**The cause was `dependent-version = "fix"`, and it was configured for a
policy this workspace does not have.** `fix` rewrites a requirement only
when the new version stops satisfying it, and `^0.1.0-alpha.1` goes on
satisfying alpha.2 for ever — so after the alpha.2 release every one of
the requirements still read alpha.1, and nothing said so. Its argument is
sound and its subject is `cargo release -p <crate>`: sparing a publish for
a neighbour that did not change. The policy two paragraphs up forbids
exactly that — **every crate is published on every release** — so there was
no publish to spare and only the lie to pay for.

It is `upgrade` now. `^0.1.0-alpha.2` excludes alpha.1 outright, so this
needs no `=` pins, and `just versions-agree` checks the **result** rather
than the setting: a release run from an older checkout, a hand-edited
manifest and a merge all bypass a setting, and none of them bypass a gate.
Two things read as mistakes and are not — an unpublished crate's version
runs ahead of the index, and published versions go sparse per crate.
Publishing everything used to remove both; under release-plz the second is
the intended shape rather than a symptom, and `just versions-agree` is
what says the requirements followed. `docs/publishing.md` has the table, the script that derives it,
and the reason the waves are **not** collapsed back to five — a version-carrying
dev-dependency is what lets a downloaded `.crate` run its own tests, which
distribution packagers do.

**No local check can catch a wrong order**, which is why it is a document
rather than a recipe: `cargo package --workspace` makes every member
available to every other through a local overlay, so `just package-build`
is green for an order that a real sequential publish would refuse. The
refusal is benign — the verify step names the missing crate and nothing is
uploaded. Every publishable crate carries
`[package.metadata.docs.rs] all-features = true`, because docs.rs builds
`default` and `default` here is empty or near-empty in 29 of the 30 —
`hclient` itself is the exception since `default-transport` joined its
default, and even there `json`, `gzip`, `cookies` and `cache` are opt-in.

**Minimum supported Rust: the latest stable release** — currently **1.99**,
declared once in the workspace manifest and shared by every crate. That is the
support policy, not a snapshot: the floor moves with stable, and a release that
needs a newer compiler than the one you have is expected rather than a bug.

The trade is deliberate. There is no window in which this crate builds on an
older toolchain, so if you pin an older Rust, pin an older `hclient` with it.
In exchange nothing here carries version-shim code, and there is no MSRV
matrix to maintain.

**Two crates are outside that too, and the argument is the shared
version's.** `system-resolver` declares `rust-version = "1.96.0"` and
`hclient-idn` `"1.95.0"`, literally. The policy above buys the *family* no
shim code and no matrix, and it costs a crate meant to be used from
outside real compiler reach.

**Both floors have drifted upward since they were set, and the manifests
say why in a way this file did not.** `system-resolver`'s read `1.85.0`
for as long as this paragraph did, bound by **edition 2024** and nothing
else in the crate. It is 1.96 now — `assert_matches!` in its tests
(1.96) over what `cfg_select!` had already cost (1.95) — and its own
manifest calls that *"a trade that has now gone the wrong way twice on
this crate's own argument, which is that it left the family's shared MSRV
to keep reach"*: `cfg-if` and `assert_matches` are each one small crate
with a low floor, and dropping them bought two lines out of the lockfile
for eleven releases of reach.

It is one release stricter than a consumer needs, deliberately:
`assert_matches!` is used only under `#[cfg(test)]`, which cargo does not
build for a dependency, so the crate still compiles on 1.95 for anyone
depending on it. Declaring the higher number is the under-claiming
direction, and it keeps `just msrv` able to run the tests rather than only
check them.

**This paragraph said 1.85 four times over while the manifest said
1.96**, which is this file's own recurring defect — a number in prose does
not move, where `just msrv` reads the floor out of the manifest and
refuses when the two disagree. The gate was right and the sentence was
not.

**And the three-platform promise was not being kept, which is worth
knowing before the next argument leans on it.** For twelve days — every
`ci.yml` run the API still holds, back to 2026-08-09 — `test
(macos-latest)` and `test (windows-latest)` **never finished a single
run**. Each sat until GitHub's default six-hour job limit and was killed
or cancelled, while the Linux leg finished the same suite in two to four
minutes. Nothing said which test was stuck, because **three separate
causes had to line up for that silence**: no per-test bound (there was no
`.config/nextest.toml` at all), no `timeout-minutes` on the job (so the
six-hour default applied), and `just test-workspace` capturing nextest's
output in a shell variable it never lived to print. Remove any two and
the third still gives a six-hour blank.

All three are fixed together — `slow-timeout = { period = "30s",
terminate-after = 10 }`, `timeout-minutes: 60`, and `tee` — with the
numbers measured rather than picked: the slowest test in this workspace
is 7.8 s, so the per-test kill is 38x it and the job bound is 10x the
Linux wall time. **What the fix does is make the next red run say which
test hangs**; it does not fix the hang, which is still unknown and is
nobody's guess worth recording. Both halves were checked in the failing
direction — a red nextest exits non-zero through the new pipe, and a run
printing no `Summary` exits 1 naming itself.

The claim below is stated as it stands, and it is the one to re-read once
that is diagnosed. One data point has since arrived and it is not a
diagnosis: the whole suite runs on a real Mac — macOS 27, arm64 — in
**12.4 s**, so whatever wedges the runner is not something this tree does
on every macOS. See the Apple resolver section for the run and for the
one candidate it produced.

There is also **no MSRV job in CI for the family, deliberately**, and
`rust-toolchain.toml` pins no version — it says `channel = "stable"`. A job
checking a fixed version would be a second statement of the same promise,
staler than the first, and it is the one people would trust: the moment
stable moves past the pin, that job goes on passing while checking a
toolchain nobody supports. The whole test suite already runs on stable on
three platforms, which is the promise, so the pin would add a way to be
wrong and no way to be right.

**And that argument does not reach the one crate with a fixed floor**,
which is why `just msrv` exists and has a job of its own. The objection
above is that a pinned job restates a **moving** promise more staleley
than the manifest does; `system-resolver`'s promise does not move, so the
job is its only statement rather than a second copy of one — and without
it a literal `rust-version` would be a claim with nothing behind it,
which is the defect this file records against itself four times over. The
recipe reads the floor **out of the manifest** rather than carrying a
second copy, and refuses when the two disagree; both halves were checked
in the failing direction, by moving the manifest's number and by adding a
`let`-chain, which needs 1.88.
**Two TLS backends, both behind the same `TlsConnect` seam.**
`hclient-tls-rustls` is the default: memory-safe, and it behaves the same on
every platform. `hclient-tls-native-tls` uses the platform's own stack —
SChannel, Security.framework, OpenSSL — and exists for deployments whose
trust decisions live in the OS store: enterprise roots pushed by policy,
a FIPS-validated provider. That is a fact about an environment, not a
preference.

**Smartcards were on that list for four verticals and are not reachable
through this backend**, which was measured while building the mTLS seam
rather than assumed. `native_tls::Identity` has exactly two constructors
— `from_pkcs12(der, password)` and `from_pkcs8(pem, key)` — and both take
**bytes**, so a key the OS holds and will not export is as unreachable
here as it is through rustls. What the platform stack genuinely buys is
the trust *store* and the provider, which is the rest of the list; a
non-exportable client key needs a backend that can reach the keystore
itself (`rustls-cng` on Windows), and this workspace has none. The claim
was the *wrapper* shape one field over: the platform holds such keys, and
the crate binding the platform does not hand them over. It reports less back, and its own
module doc says exactly what: no protocol version and no cipher suite.
**The ALPN is no longer on that list**, and the story of why is the
section below.
