# Client behaviours

> Part of the [AGENTS.md](../../AGENTS.md) record, split 2026-10-05. The root
> file keeps the operational rules and the index of parts. Sections below
> keep their original headings, and a "this file" in the text named the
> whole record when it was written.

### The browser's own `fetch` members, and the one that is refused

`Fetch::opts(FetchOpts { .. })` — `mode`, `credentials`, `cache` and
`referrerPolicy`, four `Option`s applied to `RequestInit`, `None` meaning
*leave it to the browser*. Until this, `to_web_request` set method,
headers, body, signal and `duplex` and nothing else, so a browser caller
could not send `credentials: "include"` — what a cross-origin
authenticated request needs. Two independent browser clients expose all of
it (reqwest's wasm build and `gloo-net`), which is what makes it an absence
rather than a knob nobody wants.

**On `Fetch`, not on `Transport`**, because three of five backends have no
such concept — the rule that put `WebSocketConnect` in its own trait. And
**not a request extension**, for the reason `Prefetch::prepare` refuses to
take an HTTPS record from a caller: an extension is a channel any code able
to build a request can write to, and `credentials: "include"` is a decision
about which origins receive the user's cookies. Four `web-sys` feature
names and no crate — the graph is 32 either way, measured.

**`redirect` is refused, and it is the interesting one, because it is a
capability that would lie.** `fetch`'s `redirect: "manual"` does not hand
back a `3xx` a caller can act on: a cross-origin response comes back
*opaque-redirect* — status `0`, empty header list, null body, no readable
`Location`. So `Capabilities::redirects` could not honestly move from
`Internal` to `Transparent`, and claiming otherwise would promise `Client`
a policy it could act on for exactly the case where redirects matter.
`hclient-urlsession` is the backend that genuinely reports `Transparent` —
its delegate can refuse a hop — and the asymmetry is why both are worth
having. `redirect: "error"` is a third thing, *fail rather than follow*,
which is `RedirectPolicy::None` with the answer thrown away.

**Asserted without a network, deliberately.** `web_sys::Request` has a
getter for each of the four, so the browser answers whether the member
arrived. What a headless run cannot honestly arrange is what the members
are *for* — `include` against a cross-origin server that sets a cookie,
`no-cors` producing an opaque response — both needing a second origin.
What this crate is responsible for is that the member reaches the request.
The control is the default arm: without it, a test setting `Include` and
reading `Include` back would pass for a transport that hard-coded it.

**And the browser suite had not compiled since 2026-08-16.**
`Event::Informational` landed with the `1xx` work and `hclient-fetch`'s
`tests/hooks.rs` never gained an arm, so every browser binary in that
crate failed to build through six merges that were each green on
`cargo nextest run --workspace --all-features` — which does not build for
`wasm32-unknown-unknown`, where `just test-browsers` is its own CI job.
`Event`'s exhaustiveness is what turned a new variant into a compile
error rather than silence: **the design worked and the running of it did
not.** *(That sentence read `Event` not being `#[non_exhaustive]` for as
long as it has existed, and the type has since taken the attribute —
with `every_event_is_accounted_for`, one in-crate exhaustive match, kept
as the thing that still makes a new variant one compile error in one
known file. The mechanism survived; the sentence naming it did not, which
is this file's own rule met once more.)* The cheap check that would have caught it needs no browser at
all — `cargo test -p hclient-fetch --target wasm32-unknown-unknown
--no-run`.

### `error_for_status`, and the defect it found one line away

`Response::error_for_status()` and `Collected::error_for_status()` —
`Ok(self)` below `400`, an `ErrorKind::Status` error at or above it,
carrying an `UnexpectedStatus` with the status and the URL.

Three decisions. **On both types**, because they are used at different
moments: before the body for a caller who will not read it, after for one
who wants the server's error text and only then decides. **It takes
`self`** where nothing else on these types does — the whole point is that a
caller writing `?` is choosing to stop having a response, and a `&self`
form would leave the failed one in hand. And **`3xx` is `Ok`**, because
reaching one means the redirect policy already decided to hand it back;
`RedirectPolicy::None`'s own doc says a `3xx` is the caller's answer rather
than a failure to reach one, and erroring here would overrule that from two
layers up.

**Not a client setting**, which reqwest, ureq and curl all agree on: `404`
is a normal answer for about half the requests ever made, and a
client-wide *treat every 4xx as an error* would turn a HEAD probe or a
conditional GET into a failure. The caller knows which of their requests
has a status they can act on.

**And it found that `Response::url()` was answering the wrong question.**
It reported the URL the caller *asked for*, not the one that answered —
undocumented, untested, and different exactly when a redirect was
followed. The name reads *where did this come from*; the value said *where
did you send this*, which the caller already knows, having typed it. It is
the last hop now, pinned in `tests/redirect.rs` as well as beside the
error, with the no-redirect control that says the value is not simply *the
second request*. The defect was invisible until an error carried the value
somewhere a caller would read it.

### A separate bound on resolution, and it bounds something that is not a phase

`Timeouts::resolve`, `TimeoutSupport::resolve` and `Phase::Resolve`, in one
change — the rule that kept `connect` off `hclient-h3` until v0.4 W1 and
`first_byte`/`between_bytes` off native until v0.2 W4.

**What it bounds took working out.** Happy Eyeballs interleaves resolution
with connecting on purpose: the resolver is a `Stream`, and `connect`
starts both families at the top precisely so `attempt` can dial the first
address while the rest are still arriving. So there is no instant at which
*resolution finished*, and a bound on one would have nothing to attach to.
What is bounded is the wait for the **first address from either family** —
which is the failure a caller cannot otherwise diagnose, since a resolver
that hangs and an origin that will not answer are the same
`Timeout(Connect)` today.

**Nothing is serialised by it.** `attempt` cannot connect before an address
exists, so the gate waits for what the next line would wait for anyway;
what changes is the error, not the schedule.

Three decisions came out of the shape:

- **It does not apply where the connection does not need the resolver.** An
  HTTPS record with address hints gives the connector somewhere to go with
  no answer at all, so waiting for one would bound a query whose result is
  off the path — the same reasoning that keeps discovery from running for
  an IP literal. That skip was first written as a mutation **control** and
  is a **test**: RFC 9460 §7.3 hints are ordinary, so it was reachable and
  untested, which this file's own rule calls a gap. It is asserted
  causally, on the fixture seeing a connection at all, because after a
  hinted attempt fails the connector legitimately falls back to the
  resolver and the exchange's *ending* is not the subject.
- **It stops waiting when both families are done**, so a name that does not
  exist stays `ErrorKind::Resolve` rather than becoming a timeout —
  `drive`'s `ResolveErrors` has per-family causes this gate does not, and
  replacing a precise diagnosis with a vague one is what the feature exists
  to undo.
- **`false` on both ambient backends, and it is the first field whose
  `TimeoutSupport` bool was ever honestly `false` on arrival.**
  `wasi:http` 0.3's `request-options` carries three timeouts and nothing
  for resolution — the host resolves — and `fetch` collapses everything
  into one `AbortController`. `hclient-dns-doh` sets `None` for a third
  reason: it is the resolver's own client, so a `resolve` bound there would
  bound resolving the name of the thing that resolves names.

**Overlapping budgets are the caller's to reconcile.** Nothing subtracts
`resolve` from `connect`: a resolver that answered in 10 ms has not spent
any of the connect budget in any sense a connector can see, and inventing
an arithmetic between them would make one field's meaning depend on the
other's.

### Digest authentication, and it is the `425` branch with a computed header

`RequestBuilder::digest_auth(user, password)` — RFC 7616, behind the
`digest-auth` feature, off by default. No pure-Rust client ships it;
`xh`, built on reqwest, wrote its own rather than go without, which is the
evidence the absence is felt rather than theoretical.

**The shape was already here.** Digest is a challenge/response over a
`401`, and `Client::run` owns exactly that for `425 Too Early`: a
status-code test, one resend, inside the same `total` budget, gated on
`RequestBody::retry_kind()`. The branch sits beside it and differs by one
thing — the resend carries a header computed from what came back. Nothing
spawns, no clock, no `Send` bound.

**The arithmetic is checked against RFC 7616 §3.9's own printed answers**,
copied out of the document, which is why `digest::answer` takes `cnonce`
as a parameter rather than drawing it: a hash function checked against its
own output is green for any self-consistent mistake about what digest is.
Both of the RFC's examples are there, SHA-256 and MD5 over identical
inputs — the pair is what says the algorithm is *used* rather than echoed
in the header — plus RFC 2617's, for the `qop`-less form §3.4.1 keeps.

**Three decisions the building made that the plan had not asked.**
The credentials **do not cross an origin**, by the rule already stripping
`Authorization`: a password-derived secret must not reach a server the
caller never named, which is a stronger case than the one `AllowEarlyData`
was taken off a hop for. They are **not in `http::Extensions`**, because
extensions reach `Transport::execute` and a password there would be
readable by any transport, including one this workspace did not write — so
they travel as an argument to `execute_with` instead. And what goes in
`uri=` is the **request-target**, not the URL: §3.4.2 hashes what goes on
the request line, and a full URL would give the server a different `A2`
and a second `401` nobody could explain.

**Nine crates, measured, which is why they are taken rather than written.**
`md-5` + `sha2` pull `digest`, `block-buffer`, `crypto-common`,
`hybrid-array`, `typenum`, `cfg-if`, `cpufeatures` — eight net in this
graph. That is a departure from the rule that removed `url` and hand-wrote
base64, and the line between them is whether a wrong answer is *visible*:
base64 is twenty lines and fails loudly everywhere, where a hash is two
hundred whose defects are silent and whose vectors nobody re-derives.
RustCrypto's are audited, `no_std`, build-script-free and build for both
wasm targets — the same shortlist `ruzstd` was chosen from.

**MD5 is here and RFC 7616 §5.2 deprecates it**, which is not an
oversight: a client supporting only SHA-256 would fail against most servers
that speak digest at all. What this does instead is **prefer** the
strongest algorithm offered — across header lines, since a server sending
SHA-256 and MD5 as two `WWW-Authenticate` values is ordinary and a client
taking the first would answer MD5 to a server that offered better. The
client never chooses the algorithm; the server does.

Two absences with their reasons. **`auth-int`** hashes the request body
into `A2`, which cannot be done for a `Streaming` body without buffering
it — so a server offering it *alone* gets a named refusal rather than an
`auth` response it will reject with a `401` nobody can diagnose. And
**there is no nonce cache**, so every request pays one `401` round trip;
removing that needs per-origin state with a lifetime nobody states, which
is the question that made a cache dishonest for SVCB records and honest for
`Alt-Svc`.

**`cnonce` is 128 bits from the OS and its failure path is the opposite of
`sse.rs`'s.** A failed draw there degrades to un-jittered backoff, slower
and safe; here a fixed cnonce is the one value an attacker would choose, so
a failed draw falls back to a heap address — worse entropy, still not a
constant. The rule this file already records: *a degraded value is only
acceptable when the degradation has a direction.*

### HTTP/2's settings frame is tunable, and three of reqwest's eight knobs still are not

`Native::h2_opts(H2Opts { .. })` — the stream window, the connection
window, `max_frame_size` and `max_header_list_size`, four `Option`s
forwarded to `h2::client::Builder`. `None` means *whatever `h2` chooses*,
which is `TcpOpts`' rule: a value set here goes on the wire, so a default
of ours would change what a caller who asked for nothing announces to
every server.

**The window is the one that motivates the rest, and the arithmetic is not
ours.** RFC 9113 §6.9.2 fixes the default at 65 535 bytes and a peer may
have at most a window in flight, so the ceiling is `window / RTT` however
much bandwidth there is — about 5 Mbit/s over a 100 ms round trip. Raising
the stream window alone changes nothing where streams share a connection,
because the connection window is still 65 535; that is a test rather than a
sentence, since a caller reading only the field name would set one and
measure nothing.

**Infallible, unlike `tcp_opts` beside it**, and the difference is who
applies the value: a socket option is applied by a runtime that may not
have it, so `TcpOpts` needs a refusal and a per-field support mirror, where
a `SETTINGS` frame is written by this crate and there is nobody to say no.
Nothing in `Capabilities` moves either — a window size is not a capability.

**Nothing is timed, deliberately.** A throughput measurement on loopback
would say almost nothing: the window bounds bytes *in flight*, and with a
round trip near zero a sender refills it as fast as it drains — which is
exactly why the default only bites on a long fat pipe. What is observable
without a network is the setting at the peer that must obey it, so an
`h2::server` reports what capacity its `SendStream` was granted, and the
frame size is read as *behaviour* — one 512 KiB write cut into DATA frames
at the client's limit — because `h2::server::Connection` exposes no
accessor for it.

Two numbers had to be read rather than guessed. `SendStream::capacity` is
bounded by the sender's own buffer as well as by the peer's window, and
h2's `DEFAULT_MAX_SEND_BUFFER_SIZE` is 400 KiB — a first draft asked for a
megabyte and was told 409 600, which discriminates but does not measure. So
the test asks for 256 KiB, under that buffer, where the window is the only
thing binding. And `max_header_list_size` is enforced by `h2` on **receive**
as well as advertised (`codec/framed_read.rs`), which is what gives the
fourth field a local observable instead of a forwarding line nobody checks.

**Two of reqwest's eight are still absent, and the third landed where it
belongs.** An adaptive window is hyper's, computed from measured RTT;
`h2` has none, so it would be ours to write and a wrong estimator is
worse than an honest constant. And `max_concurrent_streams` governs
streams the *server* opens, i.e. server push, which `h2` does not enable
and RFC 9113 §8.4 deprecates: a knob with no subject.

**Keepalive pings are `Native::h2_keep_alive`, and not an `H2Opts`
field** — this paragraph used to say they need somebody polling an idle
connection, which is true and is the whole shape of the answer: a
`SETTINGS` field is written once at handshake, where this is a *driver*
behaviour, so it belongs on the opt-in constructor that has a driver.
Set without `multiplexed()` it is inert, which is stated where the
setter is.

**The interval measures time, not silence, and that is the one place it
differs from the WebSocket keep-alive it is otherwise modelled on.**
`hclient-tungstenite`'s restarts on any inbound frame, which is what
makes it free on a busy connection; `h2::client::Connection` reports no
traffic at all, and a driver polling it cannot tell a poll that moved
bytes from one that did not. So a busy connection pays one `PING` per
interval — nine bytes, against a feature whose entire purpose is that
the path sees traffic. The second clock is the same in both, and `h2`
makes it easier: `poll_pong` resolves for *our* ping, so there is no
unsolicited pong to mistake for it — the mutation the WebSocket version
had to be taught.

Two tests, and the pair is the assertion. One models a middlebox — an IO
wrapper that cuts the connection after a bound with no inbound bytes,
which is what a NAT flow timer watches — and asserts the server's
**accept count** stays 1 across a pause three times that bound. The
other has the peer answer one request and then stop polling its own
connection while holding the socket open, which is what silence is, and
asserts the close names the probe. Checked by mutation rather than
assumed: suppressing the `send_ping` kills the first and leaves the
second passing, and making the deadline never fire kills the second and
leaves the first — so neither test covers for the other.

`H2Opts` is deliberately **not** `#[non_exhaustive]`, copying `TcpOpts`:
its whole use is `H2Opts { one: Some(n), ..Default::default() }`, which the
attribute forbids from outside the crate, leaving per-field setters that
exist only to work around it.

### A caller gets a say over each redirect hop, after the policy rather than inside it

`ClientBuilder::redirect_predicate(|hop| ..)` -> `RedirectVerdict::{Follow,
Stop, Refuse}`. `RedirectPolicy` answers *how many* and this answers
*whether this one* — no hop to a private address, none to another host,
none off `https`.

**It is not a third `RedirectPolicy` variant, and that was the obvious
shape.** Two things kill it. `RedirectPolicy` lives in `hclient-proto`,
which is sans-io and clockless, and `redirect::decide` is a pure function
of six values — a closure variant makes it *pure except for whatever the
caller passed*. And `RedirectPolicy` is `Copy + PartialEq + Eq`, read out
of a request's extensions with `.copied()`; a boxed closure ends all three.

So `decide` is untouched and the predicate is asked **after** it, only
about a hop it already approved — which is the better order rather than a
concession, because what a predicate wants to see is `decide`'s *output*:
the resolved target (a relative `Location` is already absolute), the
method after any downgrade to `GET`, and `cross_origin`. That last is
`Follow::strip_sensitive` handed over rather than recomputed, so a
predicate refusing cross-origin hops and the client dropping
`Authorization` cannot disagree about what an origin is.

**Three verdicts, because two would lose the distinction at the one place
it matters.** `redirect.rs` already states the rule — *"do not follow" is a
`Stop`, not an error: the 3xx is the caller's answer* — and a predicate
that could only `Stop` would make an SSRF guard hand back a `3xx` the
caller must then remember to check. A caller who forgets gets a silent
success where they asked for a refusal, which is this file's *capability
that lies* one level up.

Two more decisions. **`Fn`, not `FnMut`**: the closure is shared by every
clone of the client and every request in flight, so `FnMut` means a lock
taken on every hop of every request for the sake of predicates that mostly
hold no state. And **no per-request form**, unlike `RedirectPolicy`: a
per-request setting travels in `http::Extensions`, and `AllowEarlyData` is
the type that made *may an extension cross an origin* a live question with
a real answer — where a predicate is a rule about where *this client* may
be sent.

The `Send + Sync` is amendment C12's third site and deliberately not a new
amendment: same argument, a value the caller owns reaching `Client` by
erasure rather than by a type parameter, bound on the opt-in call and
nowhere else. C10's rule against reusing an amendment by gesture is about
bounds demanded by *someone else's* trait, where the argument turns on
which external contract is being satisfied.

### `text()` learned a charset — as a second method, not a smarter first one

`Collected::text_with_charset`, behind the `charset` feature, off by
default: `encoding_rs` is over a megabyte of conversion tables and a
build that only ever meets UTF-8 has no use for them. The name is the one
reqwest and ureq both independently chose.

**What decides the shape is that `text()` must not change meaning with a
feature.** Cargo unifies features across a graph, so a charset-aware
`text()` would answer differently depending on what an unrelated crate
switched on — and the difference is silent: `windows-1251` bytes come
back as plausible mojibake rather than as the error they are today. Same
hazard as the `Capabilities` floor rule, one layer up. So it is a
separate method and the caller says so at the call site.

Four answers and each is a decision. **No `charset` parameter is UTF-8** —
RFC 7231 removed RFC 2616's ISO-8859-1 default, and content sniffing is a
browser's job done against a security model this type does not have. **An
unknown label is a typed error naming it**, never a quiet fall back to
UTF-8, which would turn *the server said something we did not understand*
into mojibake with nothing to show for it. **Malformed bytes are an error
and not U+FFFD**, because `text()` refuses invalid UTF-8 rather than
patching it, and two policies under one name is worse than either; a
caller who wants the lossy answer has `bytes()`. And **a byte order mark
overrides the declared label** — the Encoding Standard's rule, inherited
from `encoding_rs::Encoding::decode` and pinned rather than assumed.

The `charset` parameter is read here rather than by a `mime` crate, for
the reason `url` was removed and base64 is twenty lines in
`hclient-proto`. It splits on `;` **outside quotes**, with backslash
escapes honoured, because `boundary="a;charset=utf-8;b"` is a header a
server can send and a naive `split(';')` reads a parameter out of it. An
escaped quote is reachable from a server too, so it is a test rather than
a mutation control — the rule this file recorded one section up, applied
before it had to be.

### Two more codings, and both premises for refusing them were wrong

`deflate` and `zstd`, behind features of their own, off by default like
`gzip` and `brotli` beside them. `decompress.rs` had refused both in
writing, and the reversal is worth more than the codings.

**`deflate` was refused because "a client must not advertise a coding it
may guess wrong about".** RFC 9110 §8.4.1.2 specifies zlib and its own
Note records that a long tail of servers sends the raw RFC 1951 stream
instead — so the token really is ambiguous. What was wrong is the
assumption that the guess must be made *after* a failure, which is how
curl does it: `lib/content_encoding.c` tries zlib, and on `Z_DATA_ERROR`
calls `inflateReset2(z, -MAX_WBITS)` and replays the buffer — **but only
while no output has been produced yet**, a rule with a window. The
question is answered here **from the first two bytes, before any output
exists**, and it is not probability: RFC 1950's `CM == 8` cannot open a
conformant raw stream, because RFC 1951 §3.2.3 packs `BFINAL` into bit 0
and `BTYPE` into bits 1-2, so a low nibble of 8 is a stored block with the
padding bit §3.2.4 tells encoders to zero. The `% 31` check is a second,
independent one. **It costs no crate at all** — `flate2` was already here
for `gzip`, and RFC 9110's `deflate` is the same RFC 1951 stream under a
different wrapper.

**`zstd` was refused as "a third dependency for a coding no server sends
unasked".** It is two crates, `ruzstd` + `twox-hash`, and it is a decoder
-first pure-Rust crate for the reason `flate2`'s `rust_backend` is chosen
over `zlib-ng`: this crate also builds for `wasm32-unknown-unknown` and
`wasm32-wasip2`, where a C build script is not a dependency but a wall.

Three things about zstd are ours rather than the library's. **The window
is capped at 8 MB** — RFC 8878 §3.1.1.1.2's recommended interoperability
ceiling and the number Chrome settled on, against `ruzstd`'s own 100 MB
default — because a frame declares its own `Window_Size` and `Limited`
structurally cannot reach it: `Limited` counts bytes *yielded* and a
window is allocated before the first byte is yielded. **The frame's XXH64
content checksum is compared**, which `ruzstd` does not do: it exposes
`get_checksum_from_data()` and `get_calculated_checksum()` and compares
them nowhere. And **concatenated frames are one body** (RFC 8878 §3.1),
which neither `ruzstd` entry point crosses on its own.

**The `flate2` writers could not be used, and a wire test is what found
it.** `write::ZlibDecoder::try_finish` calls `zio::finish`, which runs the
decompressor until it stops producing and returns `Ok(())` **without ever
asking whether the stream ended** (flate2 1.1, `src/zio.rs:173`) — so a
truncated body reached the caller as a complete, shorter document, the
exact defect the trailer checks on the other three codings exist against.
`flate2::Decompress` answers `Status::StreamEnd`, and `Decompress::new(
zlib_header)` is the sniff's switch in one type instead of two.

Two method notes, both this file's recurring lessons landing again.
`tests/compression.rs` already warns that a truncation under
`Content-Length` is reported by the *transport*, so the decoder's
end-of-stream check is never reached — the first draft of the new tests
was written that way and passed with `ErrorKind::Body` over a decoder that
had no check at all. And the trailing-bytes refusal was written as a
mutation **control** on the reasoning that no well-formed body has bytes
after the stream; a *server* can send them, so it was reachable and
untested — a gap rather than a control. It has a test, and the control is
now the `Sniffing` arm `push` documents as unreachable, verified by
replacing it with a `panic!` and running the suite.

**One check that could not fail was fixed with them.** `just features`
runs `cargo hack --each-feature`, which builds each feature *alone* — and
`decompress.rs`'s `#[cfg]` shape is about the **combinations**: the
wildcard arm catching "whichever codings this build has no decoder for" is
`not(all(..))` of four and the exhaustive arm is `not(any(..))` of four,
so exactly one of the sixteen sets is each one's boundary. The recipe now
runs the powerset over the four as well, and fails closed on either half.

### Four crates this file did not name, and what each is for

Recorded because `docs/competitive-gaps.md` found them missing from here
while two of them are still listed in `.notes/v01-acceptance.md` as
deliberately not done — a list that was updated for its DoH half and not
for these.

- **`hclient-mock`** — `MockTransport`, behind `hclient`'s `test-util`
  feature. It is how every capability refusal in this workspace is tested,
  because "a jar against a jar-owning backend is refused at `build()`" is
  a fact about a type that never sends anything.
- **`hclient-tower`** — the `tower::Service` adapter, so this client fits
  a stack that already speaks that vocabulary.
- **`hclient-dns-hickory`** — the third `Resolve` backend, beside the
  system resolver and DoH.
- **`hclient-rt-embassy`** — a `TcpConnect`/`Timer` runtime over
  `embassy-net`, which is what makes the embedded target reachable at all;
  see the `std` paragraph in [The TLS backends](02-tls-backends.md).

### A fifth backend: Apple's `URLSession`, and the three things it refuses to take

`hclient-urlsession` puts `URLSession` behind `Transport` — the fourth
**ambient** backend, owning no connection of its own, after `hclient-wasi`
and `hclient-fetch`. It exists for the list a userspace stack cannot reach
on an Apple platform: per-app VPN, the system proxy and its PAC, and
background transfer.

**That list said "enterprise roots pushed by MDM" first, and that was
wrong.** `rustls-platform-verifier` 0.7.0 — which `DefaultTransport`
already uses — reaches them: its Apple path builds the evaluation with
`SecTrust::create_with_certificates` and *deliberately avoids* narrowing
the anchors, calling `set_trust_anchor_certificates_only(false)` after any
extra roots precisely so the system's own are kept
(`src/verification/apple.rs:130-179`, read). So MDM roots were never a
reason to reach for this backend, and the sentence stood for about an
hour before `docs/competitive-gaps.md` caught it. The other three items
stand, and so does the redirect argument below, which is the stronger one
anyway. That is a fact about the
device rather than a preference, which is `hclient-tls-native-tls`'s
argument one seam over.

**What it refuses to take from the OS is the decision worth knowing.**
`URLSession` will keep cookies, a response cache and a redirect policy for
you, and this turns all three off. None of them is in the list above; all
three are portable behaviour this workspace already implements once; and
leaving them on would make this the second backend reporting
`owns_cookie_jar` and `owns_cache`, so a caller porting from
`hclient-native` would lose two features by changing one line.

**Redirects are the sharpest, and this backend is stronger than the
browser one.** `URLSession` lets a delegate refuse a redirect and a
browser does not — so this reports `RedirectSupport::Transparent` where
`hclient-fetch` must report `Internal`, and `Client`'s hop limit and its
`Authorization` stripping across origins work here and cannot there.
Measured rather than read off Apple's documentation: a server that would
have answered a second request receives exactly one.

**Amendment C11 is a new kind of unsafe exemption.** The three before it
each cover a site with its own argument; this crate is an FFI boundary
where `unsafe` is the medium. It follows the policy rather than relaxing
it — a marker on every site, files listed by name — and objc2 0.6 needed
far less than expected: 23 sites became 11, and the body module has none.

**And the reason the Mac mattered.** `cargo check --target
aarch64-apple-darwin` is clean on a Linux host — which is worth knowing,
because it means the shape can be kept honest without Apple hardware — and
every network test hung on a real machine. One delegate per session was
one queue for every task, so `execute` polled a channel nothing would push
to; the signature said so, taking a `_shared` it ignored. A type-check
cannot see an argument that is merely unused. Each task carries its own
delegate now, and the four live tests are green on macOS 27.

### WebTransport: many sessions on one connection, and a `GOAWAY` nobody can see

The two items this crate had recorded as deliberately not done, taken up
together — and they came out opposite ways.

**More than one session per connection works, and the blocker recorded
for it was not the true one.** `PoolKey` is `hclient-h3`'s problem;
`hclient-webtransport` takes its `quinn::Connection` from outside, so
what actually binds is the peer's `SETTINGS_WEBTRANSPORT_MAX_SESSIONS`.
`Session::open_session` opens a sibling on the same connection, the limit
is **read off the SETTINGS frame** rather than assumed, a slot returns
when a session is dropped, and exceeding it is a typed `TooManySessions`.
Siblings are independent: closing one leaves the other open, and a
datagram addressed to one is handed to that one. The hard constraint is
pinned rather than described — `a_second_h3_client_on_one_connection_is_a_connection_error`
asserts the `H3_STREAM_CREATION_ERROR`, so an `h3` that stopped enforcing
it fails a line.

**`GOAWAY` is a measured impossibility**, which is a complete answer
rather than a missing feature. `h3` 0.0.8 gives a client nothing it can
observe: a session's view is unchanged across one, polling the driver
resolves nothing, and — the sharpest of the four —
**two `GOAWAY`s saying opposite things look identical**. So a session
opened after one is refused *by the peer rather than by us*, which is
what the tests assert. No state was invented for it: a variant exists
only if a caller decision turns on it, and there is nothing here for one
to turn on.

### `multipart/form-data`, and a replay contract read off the parts

`RequestBuilder::multipart(Form::new().part(Part::bytes(..).file_name(..)))`.
Its shape is decided by one requirement: **a multipart body must be able
to stream.** Concatenating every part into one `Bytes` is four lines and
is wrong for the case multipart exists for — a file large enough that a
second copy of it is the thing that fails.

**The replay contract is not a setting; it is read off the parts**, and
it is knowable before sending, which is `RetryKind`'s whole promise.
Every part resolved to bytes → a `Rewindable` body, `ViaFactory`,
`Content-Length`. Any part a stream → `Streaming`, `Impossible`, and
`Content-Length` only where every stream's own `size_hint` is exact. The
way to opt into retries is to give the parts bytes; there is no flag,
because a flag would be a promise this module could not keep for a stream
it has already handed to a transport. "Resolved to bytes" is not "written
as bytes": a `Rewindable` part whose factory hands back a `Full` counts,
one whose factory hands back a `Streaming` does not.

**The boundary is 128 bits from the OS, and there is no collision check.**
Not omitted for cost: it cannot be made whole, because a streaming part's
content is unreadable before it is sent, so a scan could only cover the
buffered parts — a guarantee for some inputs, which reads as a guarantee
for all of them and is worse than an honest probability. The probability
is the argument, and it rests on drawing **after** the caller supplied the
content, once per form: an adversary choosing a file cannot choose it to
contain a value that does not exist yet. `getrandom` was already in this
crate's graph for SSE jitter, so nothing is added.

**An entropy failure is an error and never a fixed fallback** — the
opposite resolution from `sse.rs`'s `jitter()` three files over, where a
failed draw becomes `0.0`. The two are consistent: jitter's degenerate
value is un-jittered backoff, slower and safe, where a fixed boundary is
the single string most likely to appear in someone's content and the one
an attacker could plant. **A degraded value is only acceptable when the
degradation has a direction.**

Field and file names go out as UTF-8 with three bytes escaped — LF, CR
and `"` — which is the WHATWG rule all three browser engines moved to,
and every other C0 control is **rejected**. That is a framing property
before an interoperability one: a raw CR LF in caller data would end the
header field and let the rest be read as further part headers. There is
no `filename*`, because RFC 7578 §4.2 forbids it in as many words. The
wart is stated where a caller meets it: `%` is not escaped, so a name
containing the literal text `%22` is indistinguishable from one
containing `"`.

### Request ergonomics: query, forms and auth, with no dependency added

`RequestBuilder::{query, form, basic_auth, bearer_auth}`. Small, and the
interesting part is what they are built on rather than what they do.

**`query` appends and never replaces**, and each call appends again — a
query already in the caller's own URL survives. A replacing setter fails
invisibly from the call site: the `?tenant=acme` the caller wrote is
simply gone.

**The encoding is the WHATWG serialiser, not RFC 3986 percent-encoding**,
and the two are not interchangeable. A space is `+` and only `*-._`
survive as punctuation; `uri.rs`'s `percent_encode_into` is the other set
and a query built with it reaches a form parser as different data — a `+`
sent as itself reads back as a space. Both are now written down beside
each other, in `hclient-proto`'s `encode` module.

**That module is where `base64` moved to**, and it is one function each:
`hclient-native`'s proxy had written its own for
`Proxy-Authorization: Basic`, and `Authorization: Basic` is the same
encoding for the same reason. Encode only, both: nothing here decodes
either, and a decoder is where the sharp edges live.

**All three are the crate now, and the sentence that stood here was the
defect.** It read that neither pulls a crate, because `url` was removed
from this graph at real cost and its `form_urlencoded` *"would bring it
straight back"*, while `base64` is *"a crate for twenty lines"*.
`form_urlencoded` is its own crate over `percent-encoding` alone — two
crates, no `url`, no `idna`, no ICU, no build script, both wasm targets —
and `base64` was already in the graph of any build that resolves DNS with
a codec. **The claim was never measured**, and it was restated once more
after `base64` landed, which is the shape this file records three times
over about checks: nothing forced a re-measurement, so the sentence
outlived the fact. All three crates were checked against the lines they
replace — every padding length for base64, the space/`*`/`~` rules for
the form serialiser, and the delimiters for the URI encoder — and every
one is byte-identical.

What the measurement rules out is the near neighbour rather than the
crates: `urlencoding` disagrees with the WHATWG serialiser on 3 of 11
probed inputs including the space, and one module over it escapes `/`,
`?`, `&` and `#`, turning `/a/b?x=1&y=2` into
`%2Fa%2Fb%3Fx%3D1%26y%3D2`.

**`base64` costs one crate rather than none, and the tense above is
load-bearing.** It shared `dns-message-parser`'s copy at 0.22; taking
0.23 makes it a **second** copy, which `cargo deny` reports as a
duplicate (`multiple-versions = "warn"`). That is the cheap kind by this
workspace's own rule — the two never exchange a `base64` type, since
nothing here does more than call `encode` — and it is worth knowing that
0.23 changes nothing this code touches: its additions are SIMD engines
behind a default-on `simd-unsafe` feature this build switches off,
decode-side error detail, and custom padding. Dropping back to 0.22
removes the duplicate and loses nothing.

**The SIMD engines are refused, and the measurement is the whole
argument.** Switching the feature on changes nothing by itself:
`simd-unsafe` gates the new `Simd`/`Avx2`/`Neon` modules, and `STANDARD`
is `GeneralPurpose`, which the feature does not touch — so SIMD would
have to be named at the call site. Both call sites encode a
`user:password` for `Authorization: Basic`, and at those sizes it is not
faster. Measured on x86-64, encode, nanoseconds per call:

| input | `STANDARD` | `Simd` | |
|---|---|---|---|
| `alice:hunter2`, 13 B | 23.9 | 20.4 | 1.17x |
| 40 B | 23.9 | 25.2 | **0.95x — slower** |
| 120 B | 51.6 | 53.1 | **0.97x — slower** |
| 64 KiB, for contrast | 17784 | 6208 | 2.86x |

The runtime detection and the fallback cost more than the scalar encode
at the sizes this workspace actually has, and the 2.86x needs an input
base64 never sees here. What it would cost is the sharper half:
`Simd` requires base64's **`std`** feature, where `hclient-proto` builds
`alloc`-only for both wasm targets, and none of the three engines exists
on `wasm32` at all — so the engine would be a per-target `#[cfg]` in a
sans-io leaf, which is the machinery this workspace removes rather than
hides. Nanoseconds against that, once per request, beside a network
round trip.

**A JSON request body closes the asymmetry** the response side had left:
`Collected::json` had existed since v0.1 and `RequestBuilder::json` had
not, both behind the same feature and for the same reason — a caller who
streams bytes should not link a serialiser, and on wasm that is download
size. It serialises **in the builder**, so a value that cannot be
serialised is the first build error rather than a failure discovered
after a connection was opened.

**A colon in a Basic username is refused rather than encoded.** RFC 7617
§2 makes it the separator, so `("a:b", "")` and `("a", "b")` would
produce identical bytes and one of the two callers would be silently
wrong. Both credentials are marked sensitive, which is asserted — the
mutation that removes the marking survived every wire-level test, because
a `Debug` is the only place it shows, and an observable property with no
observer is a gap rather than a control.

### `Expect: 100-continue`, and a ceiling that was measured rather than rounded

`Native::expect_continue(after)`. A body carrying the header waits for the
`100` or for `after`, whichever is first — RFC 9110 §10.1.1 makes the
second outcome *send it anyway* rather than an error, so the gate has one
open state and not two. **Both halves are required**: the caller asks by
sending the header, the transport agrees by being configured, and either
alone leaves the body ungated.

hyper's client does **not** do this — `Expect` appears in hyper 1.11 on
the server side only — and two things read from its source are what made
it implementable. `dispatch.rs`'s `poll_loop` calls `poll_read` *before*
`poll_write` every turn, so a body answering `Pending` does not stop the
response from being read; and the `100` arrives through the same
`hyper::ext::on_informational` the `1xx` work already installs. **That
slot holds one closure**, so the gate and the hook cannot each have their
own: which one is installed depends on whether a hook is watching,
because reporting needs `H: Send + Sync + 'static` and opening a gate
needs nothing of `H`.

**The timer could not live in the body**, and that decided the shape: a
concrete `Pin<Box<Tm::Sleep>>` would give `OutgoingBody` a type parameter
a dozen signatures must carry, and a `Box<dyn Future>` drops auto traits
(amendment C1). The body holds a flag and a waker; the clock stays in
`Native`, folded into the `first_byte` race rather than wrapped around it
— written as a wrapper first, and 56 tests in this crate's hook suite
aborted with `SIGABRT` on a stack overflow.

**A default that waited would be a default that hangs**, since a server
ignoring `Expect` sends no `100`. And it is not a `Timeouts` field:
`first_byte` bounds a wait ending in **failure** where this bounds one
ending in **proceeding** — same clock, opposite outcome.

That overflow produced the more general lesson. There are now two future
-size guards, `hclient/tests/future_size.rs` and `hclient-native`'s, and
**neither ceiling is a round number**: `Client::execute`'s future is
4,344 bytes and `Native::execute`'s is 15,480, but the figure that sets
both is what one extra `async fn` layer costs — measured at **1.81×**, so
a ceiling at 2× would be a guard that cannot fire for the defect it
names. They are 6 KiB and 24 KiB, and both are checked in the failing
direction by reintroducing the layer. `.notes/expect-continue.md` §7.
