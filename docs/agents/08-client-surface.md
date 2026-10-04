# The client surface

> Part of the [AGENTS.md](../../AGENTS.md) record, split 2026-10-05. The root
> file keeps the operational rules and the index of parts. Sections below
> keep their original headings, and a "this file" in the text named the
> whole record when it was written.

### An axum app is testable in process, and the seam that allows it is the one reqwest has not got

`hclient_tower::app::AppTransport` makes a `tower::Service` a
[`Transport`], so a test drives the **real** `hclient::Client` — redirects,
the jar, the cache, retries, decompression, `.json()` — against a **real**
`axum::Router`, with no socket, no port and nothing spawned. httpx's
`ASGITransport`, in Rust.

**It exists here rather than anywhere else for a structural reason.**
`reqwest` has no `pub trait Connect` — measured in 0.13.4, zero hits — so
testing an axum app against it means binding a port. And what
`tower::ServiceExt::oneshot` already gives is a *service call*, not a
client: the `http::Request` is assembled by hand, no redirect is followed,
no cookie is stored, nothing is decompressed. The difference is the whole
of `Client`, and the test that pins it walks a `302`, stores a
`Set-Cookie` and presents it on the next hop — one `send()`, two hops, no
socket.

**The claim is executed rather than asserted.** `tests/axum_router.rs`
runs a real `axum::Router`, because a claim about a third party is exactly
as perishable as the check behind it — this file's own rule, and the
reason `axum` is a dev-dependency here. It costs 22 crates with
`default-features = false`, carrying neither tokio nor hyper, which is
what made proving it affordable.

**Two pieces were missing and both are boundary work.** `RequestBody` is
not an `http_body::Body` — it is an enum with a factory arm — so
`OutgoingBody` is the view that makes it one, built through
`RequestBody::reduce` rather than by matching: the enum is
`#[non_exhaustive]` now, so a match here needs a wildcard, and a wildcard
is where a new variant goes to be silently mishandled. `Reduced` is
exhaustive, owned by the crate that would add one, and already carries the
factory arm's depth bound.

And the response body needed mapping the other way. `DynTransport`'s
blanket impl requires `<T::Body>::Error: Into<Error>`, which a server-side
body does not satisfy — `Full`'s is `Infallible`, axum's is `axum::Error`.
Without `IncomingBody` an `axum::Router` could be a `Transport` and still
not back a `Client`, which is the whole point. **The transport is the
boundary, so the conversion belongs there** and not in a caller's test.

**The authority is named at construction and any other is refused.** An
in-process service has no origin and a client needs an absolute URI —
there is nowhere to resolve `Location: /other` against otherwise — so
httpx invents `http://testserver` and so does this. What is added is the
refusal: a test that names a real host would otherwise be *answered by
the local router*, and pass while reaching nothing. Checked by mutation:
removing the check kills exactly that test.

**The body mapping is the caller's one line, deliberately.**
`axum::Router` takes `axum::body::Body`, so the call site writes
`app.map_request(|r| r.map(axum::body::Body::new))`. A type parameter and
a stored closure on `AppTransport` would spare that line and would be this
crate reimplementing `ServiceExt::map_request`, which the caller already
has.

### Retry took the same shape, and the asymmetry between the two is the finding

`RetryPolicy` is a trait with one method, `Standard` is the configuring
struct that implements it, and the retry predicate is gone the way the
redirect one went. `SafeMethodsOnly`, `Never`, `RetryFromFn`, `RetryAll`
and `.and(..)` are the rest.

**But the two operations are not symmetrical, and forcing the symmetry
without saying so would have been wrong.** Following a redirect is what
happens *unless* a policy objects, so composing there is a vocabulary of
restrictions and the trait's default is `Follow`. Retrying happens *only
because* a policy permitted it, so the trait's default is `Stop` and
`and` narrows from **one configured permission** rather than from *yes*.

The consequence is a rule that needed writing down: **composing two
permitters gives their intersection, not their union.**
`Standard::default().and(OnStatus(..))` does not mean "unsent errors *or*
these statuses" — it means neither, because each refuses what the other
allows. To retry *more*, widen the one `Standard`; to retry *less*, `and`
a guard onto it. The same `and` reads as pure restriction one module over
because there is nothing to widen there.

`RetryAll`'s empty case is the same asymmetry once more: an empty
`redirect::All` permits everything and an empty `RetryAll` stops, because
the identity of a meet is the operation's default and the two defaults
are opposite.

**`SafeMethodsOnly` is what the whole method-safety argument was owed.**
This workspace states in four places that `RetryKind` answers *can this be
sent again* and nothing answers *may this be repeated* — and until there
was a guard seam, that principled refusal had nowhere for a caller to act
on it. It is now a named type: `GET`, `HEAD`, `PUT`, `DELETE`, `OPTIONS`,
`TRACE` and `QUERY`, the last included because it is safe and idempotent
by its own specification, which is the same reason this workspace does
not group it with `POST` for redirects.

**One capability was deliberately lost.** The old predicate was handed the
settled delay and could refuse a wait it found too long. The verdict now
*is* the delay, and `and` takes the **longer** of two — so a guard can
lengthen a wait and cannot shorten one. That is not an oversight: waiting
less is the less careful answer, and `Standard::max_retry_after` already
caps by **stopping** rather than by waiting less, which is the whole
reason `Retry-After` is refused rather than clamped.

**`jitter` moved onto the proposal**, so the policy stays a pure function
of its inputs — the client owns the entropy exactly as it owns the hop
count one module over, and for `Backoff::delay`'s own stated reason.

`crates/hclient/src/predicate.rs` is deleted. It had held two verdicts,
two proposals and two boxed closures; what survived is one error type,
which now sits beside the other redirect errors in `client.rs`. A module
named after a concept that no longer exists is worse than no module.

### The redirect policy is a trait now, and the predicate stopped being a second concept

`RedirectPolicy` was a `Copy` enum in `hclient-proto` and
`RedirectPredicate` was a boxed closure in `hclient` — two things for one
job, the second existing only because the first could not hold a closure.
Both are gone; there is one trait with **one method**:

```rust
fn follow(&self, hop: &ProposedRedirect<'_>) -> RedirectVerdict
```

`Forbid`, `Limit::new(n)`, `SameOriginOnly`, `HttpsOnly`, `FromFn`, `All`,
and `.and(..)` to compose. One setter where there were two, and one
`check_supported` branch where there were two.

**The line between policy and mechanism was drawn by measurement, not by
taste.** `decide` does seven things and only one was ever policy — the hop
limit. The rest is RFC: which statuses redirect, whether a `Location`
parses, RFC 3986 §5.2 resolution, what an origin is, RFC 9110 §15.4's
method table. The rule that separates them is *two correct clients could
disagree about it*, and it was checked against curl 8.18: there is a flag
for **every** step this rule calls policy — `--max-redirs`, `--post301/2/3`,
`--location-trusted`, `--proto-redir` — and **none** for any step it calls
mechanism. No client exposes a way to resolve a `Location` differently,
because doing it differently is an open redirect rather than a preference.

**So two more things became policy, and they are exactly curl's two.**
`Allow { preserve_method, keep_credentials }` — the 301/302/303 rewrite to
`GET`, and whether credentials survive a cross-origin hop. Granted **per
hop**, which is more than curl's three flags give: a policy can trust one
destination without trusting every future one, and can answer differently
per status without needing a parameter.

**`Allow` carries booleans and never a request, and that is the invariant
the type lives under.** A verdict carrying the rewritten request was
proposed and refused for two reasons. Composition: two policies answering
`Follow(req_a)` and `Follow(req_b)` have no defined meet, where two
`Allow`s meet field-wise. And escape: handing over the request hands over
the `uri`, which is the open-redirect surface the mechanism exists to
keep closed. **The rule is written beside the type — `Allow` may only ever
gain fields that switch a protection off.**

**One rule for all composition: the more conservative answer wins.**
`Follow < Stop < Refuse`, two `Follow`s meet their `Allow`s, and the chain
short-circuits at `Refuse` and **not** at `Stop` — because nothing can
raise a `Refuse`, where a later policy can raise a `Stop`. A pair of tests
pins exactly that: a `Stop` still asks the rest of the chain, a `Refuse`
does not.

**Order is unobservable, which is what separates this from middleware.**
Middleware composes as function composition and can rewrite; policies
compose as a meet on a lattice and can only narrow. That distinction is
why redirect-following is not expressible as a `tower` layer here and is
not exposed as one anywhere else either — OkHttp keeps
`RetryAndFollowUp` in its fixed chain, between the two user positions,
which is where `Client::run` already sits.

**The limit moved from a separate method into `follow`**, because the hop
count is already on the proposal. An `AtomicU8` inside the policy was
proposed and cannot work: the policy lives inside `Client`'s `Arc`, shared
by every clone and every request in flight, so the counter would be
per-client — `Limit::new(10)` would allow ten redirects for the life of
the program. `hops` is a local in `run`, per operation, which is where
per-request state belongs. **The policy holds the rule, the client holds
the state** — the same rule that already keeps a policy from recomputing
what an origin is.

**Four costs, each real.** Two policies are two types, so an `if`/`else`
choosing between them needs an `Arc` where an enum needed nothing —
`examples/portable.rs` shows it. The visited-URI list is now always
collected, where it used to be gated on a predicate being installed,
because one trait cannot be asked whether it reads history — a `Vec<Uri>`
bounded by a `u8` against as many round trips. The limit is consulted
*after* the `Location` is resolved rather than before, so a malformed
`Location` on the hop that would have exceeded it now reports
`InvalidLocation` rather than the limit; no hop goes anywhere different.
And "follow with no limit" became expressible by accident — the default
policy is still `Limit::new(10)`, so only a caller who *replaces* it is
exposed, and `hops: u8` is the structural backstop.

**A gap surfaced on contact, and it is ACT's finding one type over.**
`ProposedRedirect` had no public constructor, so a caller could not unit-
test their own policy — the wall `Response` was found to be. It has one
now.

**And the error got better rather than worse.** `TooMany(u8)` said one
thing and could not say the others; `RedirectRefused` carries the policy's
own reason plus the client's hop count, so *"refused a 302 to X after 10
hops: redirect limit reached"* covers a limit, an origin rule and a
caller's own closure with one shape. The number is the client's fact, not
the policy's, which is why a `&'static str` verdict loses nothing.

### A retry that is safe by construction, and the one gap the ecosystem measures

The competitive analysis in this workspace lists gaps against other
clients. Measured from the other direction — what people **bolt onto**
`reqwest`, in recent monthly downloads — the picture inverts:

| bolt-on | per month | here |
|---|---|---|
| `reqwest-middleware` | 15,551,838 | `hclient-tower` |
| **`reqwest-retry`** | **7,657,595** | **was missing** |
| `reqwest-tracing` | 4,516,360 | hooks, in the box |
| `reqwest-eventsource` | 2,510,406 | `hclient::sse` |
| `http-cache-reqwest` | 859,999 | `hclient::cache` |
| `reqwest-websocket` | 564,728 | `hclient-tungstenite` |
| `reqwest_cookie_store` | 346,932 | `hclient::cookie` |

Over 32 million downloads a month of things that are already inside this
crate, plus one that was not. That is ACT's *the gap is a pointer, not a
feature* at ecosystem scale.

**One candidate was killed by the same measurement**: `rvcr`, record and
replay, is at **437** downloads a month. Nobody in Rust wants a VCR, and
knowing that before proposing it is worth more than the feature would
have been.

**`ClientBuilder::retry` is the gap, and it comes out better in kind
rather than in degree.** A retry crate that wraps a client sits *above*
the transport, so it cannot tell a request that never left from one a
server received and acted on — both are an error — and must therefore
repeat both or neither. Here they are different values:

- `Error::is_unsent()` is a claim a **transport** makes at a site where it
  knows. `hclient-native` marks it where the Happy Eyeballs race ends with
  no connection and where no attempt was launched at all.
- `RequestBody::retry_kind()` answers *can this be sent twice* **before**
  the first attempt. A `Streaming` body is `Impossible` and no policy
  overrides it.

**The trap that makes the first of those necessary was found by reading
rather than by testing.** `ErrorKind::Connect` looks like it means
*nothing was sent* and does not: `hclient-native` classifies a response
head over `H1Opts::max_headers` as `Connect` too, on hyper's reasoning
that nothing usable came off the connection — and that happens **after**
the request went out. A retry deciding from the category would resend a
request the server had processed, which is exactly the guess this whole
design exists not to make. Two tests differing only in the mark, with
identical `ErrorKind`, are the pair that pins it; mutating the decision
back to the category kills one and leaves the other green.

**`Retry-After` that cannot be honoured stops the retry rather than being
rounded down.** A server asking for longer than `max_retry_after`, or
sending the `HTTP-date` form this module deliberately does not parse, ends
the retry — because waiting *less* than a server asked is the one
behaviour the header exists to prevent, and a client that caps the value
and retries anyway has turned a limit into a violation. The date form is
unparsed on purpose: reading it needs a calendar, `hclient-proto` is the
sans-io leaf whose dependency count is guarded, and the narrowing has a
direction.

**And the signature takes a clock because the alternative hangs.** The
first version was `retry(policy)`, using the client's own timer. Without
`default-transport`, `DefaultClock` is `NoClock`, whose `Sleep` is
`std::future::Pending` — so that version compiled everywhere and **hung
for ever** at the first backoff. Found by `just test-no-default`, where
four tests timed out at 300 s rather than failing; a hang is worse than a
panic and much worse than a refusal, so the precondition moved into the
signature, where `total_timeout` already had it. The tests supply a clock
whose sleeps are instant, which also means every assertion in them is
about the *decision* and never about a delay.

The policy itself is in `hclient-proto`: `RetryPolicy::decide` is a pure
function of a rule and one outcome, so every rule is tested with no
socket, no clock and no entropy. It is deliberately **not**
`#[non_exhaustive]`, on `TcpOpts`' argument — its whole use is
`RetryPolicy { statuses: .., ..Default::default() }`, and the attribute
forbids exactly that from outside the crate.

### The handshake was described three lines above where it was thrown away

Asked whether `hc` can show the TLS handshake the way `curl -v` does. It
could not, and the reason was not the CLI's.

`TlsInfo` has carried `protocol_version`, `cipher_suite`, `alpn` and
`peer_certificates` since TLS became a seam here, and
`hclient-tls-rustls` fills every one of them — the version normalised to
the dotted `TLSv1.3` spelling curl prints, the suite as its IANA registry
name. `hclient-native` receives it, binds it to a local, and **reads that
local three lines above the line where it emits `Connected`**, to pick
the protocol. Measured: `TlsInfo` and `tls_info` appear **zero** times in
`hclient-core` and in `hclient`. Nothing above the transport could see
any of it.

**That is the third sighting of one shape**, after `native-tls`'s ALPN
and `hclient-fetch`'s `!Send` body: *the limitation belongs to the
wrapper, and the layer beneath has the thing.* Here the wrapper was our
own transport rather than a third party's.

`Connected` gains `tls_version`, `tls_cipher` and `alpn`, and **the
freeze is what made that free**: the struct became `#[non_exhaustive]`
during the API-stability work, so adding three fields is not a breaking
change. They are borrowed `&'a str`/`&'a [u8]`, because `Connected<'a>`
already had the lifetime and the `TlsInfo` outlives the emission. They
are **strings rather than enums** — the vocabulary is the TLS registry's,
which gains entries, and a backend that meets a version this workspace
has never heard of should be able to say so instead of being pushed
through an `Other` arm.

One setter for all three, not three, because they come from one place: a
backend either read the handshake's outcome or it did not, and three
setters would let a caller set two and forget the third — which is
exactly what `Native::hooks` dropping the `1xx` installer while keeping
its capability already cost this workspace once.

**`None` means two things and the pair separates them.** Over `http://`
there was no handshake, and `timing.tls` is `None` too; over `https://`
it means the backend does not describe it, which is
`hclient-tls-native-tls`, whose own module doc says the platform stacks
expose no getter. So `(timing.tls, tls_version)` answers *no TLS* against
*TLS this backend will not describe*, and `hc -v` prints three different
things accordingly — including a line saying the connection was
encrypted and undescribed, because silence there would read as
plaintext.

`alpn` is deliberately **not** a second statement of `version`:
`version` is what the transport will speak, decided by the same function
that picks the handshake, where `alpn` is what the peer said. They agree
on every connection this workspace makes, and a hook that finds them
disagreeing has found something worth reporting.

Certificates are not exposed, and the reason is a dependency rather than
a decision: `TlsInfo::peer_certificates` is DER, and turning it into
curl's subject/issuer/dates needs an X.509 parser. The field is there for
whoever wants to add one.

### `hclient-fetch` ran zero tests on the main gate, and the fix was not the one that was built

Asked whether the browser body's channel machinery — the pump, the
cancellation, `poll_frame` — was the same pattern as the embassy
converter's. It is, at two granularities: `hclient-fetch` sends **frames**
over an `mpsc` and keeps streaming, where `hclient-actor` sent **one
collected value** over a `oneshot` and did not. So "an actor costs
streaming" was a fact about the granularity chosen, not about actors.

Measured, and the overlap was real: of `hclient-fetch/src/body.rs`'s 133
code lines, **64 mentioned nothing of JS at all** — `pump` had already
been written taking a `Stream` rather than anything of the browser's —
against 69 that are `web_sys` throughout. Nothing like the 20-of-402 the
date parsers turned out to be.

**So the shared half was extracted to `hclient-core`, and then put back,
and the round trip is the finding.** Two things happened in between.

`hclient-actor` was **deleted**, which took the second consumer with it —
and with one consumer the extraction is `futures-channel` costing +1 crate
to `hclient`, `hclient-core` and `hclient-rt-embassy` to serve a backend
that already had it. The workspace's own test for a boundary — *does it
hold a dependency a feature would otherwise spread* — has no subject when
there is nothing to spread to.

The argument that replaced it was that the **tests** would move onto the
main gate, and it was true and did not need the move.
**`hclient-fetch` runs zero tests in `cargo nextest run --workspace`** —
all 13 of its binaries are `#![cfg(target_arch = "wasm32")]`, so every
property it asserts is guarded by `wasm-pack test --headless` alone, the
job this file records silently failing to compile for six merges. But the
crate **builds on the host** — nextest was compiling those 13 binaries and
finding nothing to run — so a `#[cfg(test)] mod tests` inside `body.rs`
runs on every push with no browser, and needs no crate to move anywhere.

Five tests where there were none, and the cancellation pair among them:
mutating the pump to watch only the channel kills
`dropping_a_body_whose_read_will_never_answer_still_cancels` and leaves
`dropping_a_producing_body_stops_the_pump` green, which is what says
neither covers for the other.

**One of them caught a real detail on the first run**, and the assertion
was wrong rather than the code: the pump asks the dead source once more on
its way out, because `select` polls the stream before the cancellation, so
the turn that learns of the drop has already been told `Pending`. The test
now bounds the extra poll instead of forbidding it — an equality there
would have been pinning `futures_util::select`'s internals rather than
this pump's promise, which is that it stops.

**The rule this leaves behind**: *"these two crates share code" and "this
code is untested where it runs" are different problems, and only the first
is answered by moving it.* The extraction was the right instrument aimed
at the wrong defect, and aiming it correctly cost one `mod tests` and no
dependency at all.

### The first consumer reported, and the two costliest findings were things we already had

ACT ported four call sites onto `0.1.0-alpha.2` — a WIT fetch, an OAuth
flow, an OCI blob fetch and a `wasi:http` host — and no crate in that
workspace names `reqwest` directly any more. The report is the first
outside measurement this project has had, and what it measured is not the
API.

**Two of its findings are one finding, and it named the finding itself:**
*the gap is a pointer, not a feature.* It hand-rolled
`url::form_urlencoded::Serializer` twice, six lines each, for want of a
`.form()` — which **exists, behind no feature, in the very version it was
porting against**, verified by extracting the published `.crate`. And it
worked around `Response` having no public constructor before finding
`hclient-mock`, which is the answer.

That is this file's *a consumer is a different instrument from a test*
finding arriving from the other side, and it is sharper: the earlier
sightings were about surfaces that were genuinely awkward or genuinely
missing, where these two were **finished, documented and unfindable**. A
long method list on `RequestBuilder` and 25 crates in the family mean a
reader who does not already know a name does not meet it. The repair is a
*where things are* table on the front page and a signpost on `Response`
turning its missing constructor into the mock rather than into a wall.

**`impl AsRef<str>` is the whole of the `IntoUrl` question.** ACT asked
for something `IntoUrl`-shaped because call sites holding a `url::Url`
were writing `url.as_str()`. `url::Url` implements `AsRef<str>`, so
widening the seven verb methods reaches it **with no dependency on `url`**
— the crate `hclient-proto` removed at a measured 1.9 MB of ICU tables.
A trait of ours would have been worth exactly the conversions it named,
and the one worth naming is already reachable. `url` sits in
`[dev-dependencies]` as the witness, which is the same role it plays for
`uri.rs`'s differential corpus.

**It paid out in a way nobody planned: 90 `needless_borrow` warnings**,
each a `&format!(..)` at a call site in this workspace's own tests that no
longer needs the borrow. A widening whose benefit shows up as ceremony
clippy can now delete is a widening that was real.

**A missing method cannot say why it is missing.**
`Rustls::with_webpki_roots()` without its feature made rustc suggest
`Rustls::from_config` — a correct name and a far bigger detour, because
rustc offers the nearest name it can see and the nearest name is the
general-purpose escape hatch. The stand-in behind
`#[cfg(not(feature = "webpki-roots"))]` carries the message on an
unsatisfiable bound, with the same lifetime trap `Client::new()`'s hit the
night before: a `where Self: Trait` predicate mentioning no generic
parameter is checked where the method is **defined**, so the plain form
fails in this crate rather than at the caller.

**What the report confirms is worth as much as what it corrects**, and it
is listed because a design argument that is never tested from outside is
just an argument. `build()` refusing a configuration the backend cannot
honour read as correct on contact. A redirect predicate against an
internally-redirecting backend being an error at `build()` is the exact
question `.notes/` recorded as unanswerable from inside — ACT's answer is
that a predicate never consulted would have been its worst outcome,
because it would have believed a ceiling was enforced. `RedirectVerdict`'s
third arm is used *because* the other two exist to be contrasted with.
`ErrorKind` being an enum retired three of its tests that had to make real
network requests, because a `reqwest::Error` cannot be constructed. And
`Resolve` handing back A and AAAA as separate streams pushed a
policy-audit record out of their resolver, where neither stream can see
it, and into the request, where it is one event — their own code's comment
had already said that was where it belonged.

**The one thing a consumer cannot get on our side of the fence.**
`reqwest` is still in ACT's graph under `oci-client`, which brings
`hyper-rustls` with `aws-lc-rs` while `hclient-tls-rustls` uses `ring` —
so rustls correctly refuses to pick a provider and ACT installs one
explicitly. Nothing here is this workspace's defect, and it is recorded
because it is the difference between *we migrated* and *we have one HTTP
stack*: the lever is an OCI client that takes a transport, which is
somebody else's crate.

### A CLI, and the mutation that survived is what it is for

`crates/hclient-cli`, binary `hc` — httpie's request-item grammar,
curl's `--insecure` and `--resolve`, and `--backend` chosen at **runtime**.

**The differentiator is real but narrower than it first reads, and the
narrow version is the one to say.** curl supports several TLS backends
chosen at build time; only a `MultiSSL` build honours `CURL_SSL_BACKEND`,
the stock build on most distributions is not one, and curl's own man page
says an unknown name *"makes curl stay with the default"*. So: curl
**can**, in a build almost nobody has; when it cannot it **says
nothing**; and the choice belongs to whoever packaged the binary. `hc`
refuses a backend it has not got, by name, beside the list of what it
carries, with its own exit code so a script can tell that from an
unreachable server.

**It works only because `Client` names no type parameters.** Both arms
return the same `hclient::Client`, so `--backend` is an ordinary `match`.
A generic client would have made the builder function's return type name
a transport, and the arms build different ones — which is the erasure
paying for itself in the first consumer written after it.

**The finding is a mutation that survived.** Replacing the refusal with a
silent fallback — the exact defect the tool exists not to have — passed
all 30 tests, because the default build carries every backend and the
refusing arm is unreachable under the `--all-features` run CI does. The
repair is not another test: the decision is now a pure function of
`(requested, available)` **taking the available list as a parameter**, so
it is testable at any feature setting. This is the same week's third
sighting of one shape — a check that cannot fail in the configuration CI
runs — after the doctest fences and the crate that only built inside the
workspace.

**Two more, both found by building rather than by designing.** `--print H`
printed the caller's headers rather than the ones actually sent, so the
diagnostic lied about the `User-Agent` and `Content-Type` the tool itself
causes. And the item grammar reads `https://example.com` as a header named
`https`, because `:` is a separator — the likeliest mistake a caller can
make, producing a silently wrong request. A scheme followed by `//` is a
named refusal now, while `https:x` stays an ordinary header, so the
refusal is exactly as wide as the mistake.

**What it costs, which the research had listed as unmeasured.** The
default build — both TLS backends, tokio, the system resolver, four
decompressors, cookies, JSON, digest auth — is **144 crates and 5.2 MiB
stripped** on the ordinary release profile, against a two-backend probe
with no argument parsing at 3.30 MiB. Ubuntu's curl is 334 KB **plus 33
shared libraries**, so the honest comparison is one file against
thirty-four rather than 5.2 MiB against 334 KB. Nothing here is tuned:
`opt-level = "z"`, LTO and `panic = "abort"` are untried, and the number
is the default profile's.

**And it found a gap in the library it is written on**, recorded here
because the route to finding it generalises: `--http 2` had been wired to
a header nothing reads. `Capabilities` report the floor, so this file
names `RequireVersion` before the head as *the* honest route to knowing
which protocol will be used — and `RequestBuilder` had no setter for it,
because `RequireVersion` lives in `Extensions` and only `timeouts` and
`redirect` had one. `tests/require_version.rs` did not notice across two
verticals: every test in it builds its request with
`extensions_mut().insert(..)`, so testing the gate by going around the
builder is what let the builder have no route to the gate. **A consumer
written against the facade is a different instrument from a test written
beside it.**

### `curl -k` and `curl --resolve`, and the two land at different seams

Two of the flags a command-line client is expected to have, added as
library capabilities rather than as anything CLI-shaped — the crate is
the infrastructure and the flags are what a consumer needs to build.
**They land in different places, and that is the finding rather than an
implementation detail.**

**`--resolve` is a `Resolve` that wraps another `Resolve`.**
`hclient_dns::Overrides<D>` answers from a table where it has an entry
and hands the question to `D` where it does not, so it composes over the
system resolver, over DoH, over hickory, over anything — and a transport
that never heard of it needs no arm for it. Three decisions came out of
the seam rather than out of curl. The override is **host-wide**, where
curl's is `host:port:addr`: `Resolve` is asked for a name and a family and
carries no port at all, so a per-port table would be keyed on something
this seam cannot see. The **family filter applies to the override too**,
so Happy Eyeballs still races two families against an overridden host
rather than one arm getting everything. And an entry with an **empty
address list answers nothing rather than falling through**, because
otherwise "point this host at nowhere" and "do not override this host"
would be the same table, and the first is how a caller blocks a name.
SVCB passes through untouched: an override is an address, and minting a
record would put a port and an ALPN into the answer that nobody supplied.

**`--insecure` is a constructor on each TLS backend, behind a
`dangerous-insecure` feature.** A feature rather than a plain constructor
**for auditability**: a build either contains a path that skips
certificate verification or provably does not, and `cargo tree -f "{p}
{f}"` answers which. It is in no `default`, for this file's own reason —
Cargo unifies features, so a default would be a floor and one careless
crate in a tree would put the path into every other crate's binary.

**The two backends do not turn off the same amount, and that is the part
worth knowing before relying on either.** rustls keeps **signature**
verification: the custom verifier answers the chain, the expiry and the
name, and delegates `verify_tls12_signature`/`verify_tls13_signature` to
the provider's own, so the handshake still proves the peer holds the key
for what it sent. `native-tls` has no such seam — the platform stacks
verify as one operation — so it is the coarser of the two.

**Whether `native-tls` needs the hostname flag beside the certificate one
is a fact about the platform**, measured in 0.2.18: the OpenSSL backend
implements the certificate flag as `set_verify(NONE)`, which drops the
name check with everything else, while SChannel and Security.framework
forward the two independently. Both are set, or the method would mean
different things on Linux and on Windows — and **the test cannot show
it**: the mutation dropping the second setter survives on this host, and
the comment claiming otherwise was corrected rather than the test being
strengthened past what a Linux run can honestly prove.

**Neither insecure configuration can share a pooled connection with a
verifying one.** Each constructor draws a fresh `TlsConfigId`, which is
part of `hclient-native`'s pool key — asserted rather than assumed,
because the dangerous direction is a connection established without
verification being handed to a client that asked for it.

### The one claim this file marked as unverified was false, and it had shipped

`hclient-dns-system/src/sys/windows.rs` read an HTTPS record as
`DNS_SVCB_DATA` and dereferenced its `pszTargetName`. Its own header
recorded the claim underneath that as **taken on the project owner's word,
not verified here**, and named the exact consequence of it being wrong:
the record's payload would be raw response bytes, and reading it as that
struct would build a `PSTR` out of them.

**Measured on a real Windows 11, and that is what was happening.**
`DnsQuery_UTF8` for `cloudflare.com` type 65 reports `wDataLength = 61`
and the union holds `0001 00 0001 0006 02 68 33 02 68 32 …` — SVCB wire
format. `DNS_SVCB_DATA` is 32 bytes on x64 with `pszTargetName` at offset
8; offset 8 there holds `h3`. The 61 octets are **byte-for-byte** the
RDATA inside the `res_query` answer this repository had captured on Linux
in v0.3 and kept as a test fixture ever since.

**The rule is readable from the Win32 metadata and was confirmed in both
directions.** A type named by `DNS_RECORDA`'s data union arrives parsed
into that member; a type it does not name arrives as its own RDATA, with
`wDataLength` its length. `MX` came back as a structure and `CAA` as
RDATA; `DNS_TYPE_SVCB` is **64** while HTTPS is **65** and has no union
member — and `DNS_TYPE_HTTPS`, `DNS_TYPE_CERT` and `DNS_TYPE_LOC` all
exist as constants with no member, so it is the union that decides and not
the constant list.

**What made this reachable is the shape of the sentence, not the code.**
Every other unverified claim in this workspace is about a third party's
behaviour and is stated where the code depends on it; this one was too.
The difference is that nothing could fail: the module compiled on every
push, `cargo check --target x86_64-pc-windows-msvc` was clean, and the
crate's own tests exercised the Unix path. **A file whose header says it
has never been executed is not covered by any gate this project has** —
which is *a check that cannot fail* met from the one direction that had
never been named, where there is no check to break on purpose because
there is no machine to break it on.

The repair is not a test. It is `crates/system-resolver`: the platform
calls moved out of `hclient-dns-system` into a crate of their own, where
`Support::AnyExcept(&[..])` refuses the forty-three types Windows parses
**by name and before a query**, and where Windows 11's `DnsQueryRaw` —
resolved with `GetProcAddress`, because `windows-sys` emits it as a
`raw-dylib` import and naming one stops a process from starting on Windows
10 — hands over the wire message and makes that platform behave like the
other four. `hclient-dns-system` is an adapter above it and carries no
`unsafe` at all any more.

**And the second half of the fix is the part worth copying.** The two
Windows calls are compared against each other on a machine that has both:
`the_parsed_path_answers_the_same_rdata_as_the_raw_one` runs
`DnsQuery_UTF8` directly on Windows 11, where nothing else ever reaches
it, because the code for the platform this project cannot get hold of
would otherwise be the only code in the crate that never runs. That is the
nearest thing to a Windows 10 gate that exists without a Windows 10.

**The same shape was then found on a second platform, and there the code
had never worked at all.** Apple's arm called `res_9_query`, on this
workspace's word that it was `res_query` under another name, and on a
borrowed Mac every one of the crate's live tests failed in ten
milliseconds. The cause is that `res_query`'s state is per-thread and
Apple's copy is not safe to enter from several at once: 12 successes in 64
from eight threads. So the arm was not merely unverified in the Windows
sense — it could not have served a client, and the whole of the evidence
for it had been a table.

It is `DNSServiceQueryRecord` now, which hands over RDATA per record and
is routed by the daemon, so a Mac's supplemental resolvers — a VPN's
split-DNS, `.local` over mDNS — are answered rather than missed. Two of
its flags cost a wrong implementation before they were measured, and both
are the opposite of what their names suggest: `kDNSServiceFlagsTimeout`
**suppresses** the negative answer it is documented to bound, and
`kDNSServiceFlagsReturnIntermediates` is what makes one arrive — 1.2 ms
against nothing in four seconds. One capability is genuinely lost and is
said on the variant rather than papered over: the daemon reports a missing
name and a missing record type with one code, so `NameDoesNotExist` is
unreachable on Apple, and the live test **branches on the platform**
instead of accepting either answer, so a platform that could distinguish
and quietly stopped still fails a line.

**The acceptance is the whole tree rather than the crate**, because a
crate passing alone says nothing about the adapter above it: `cargo
nextest run --workspace --all-features` on macOS 27 is **2304 passed, 0
failed in 12.4 s**, and `hclient-dns-system`'s own live test walks the
whole path — the daemon's callback, this crate's `Record`, the SVCB
envelope — to an endpoint advertising `h3`.

**And the first attempt at that run failed for a reason that is a third
argument for nextest.** macOS's default `ulimit -n` is **256** against
Linux's 1024, and under `cargo test`, which runs a binary's tests as
threads of one process, the suite died with `TooManyOpenFiles` in
`hclient-native`'s fixtures. Under `cargo nextest run` the same tree
passes **at that same 256**, because each test is its own process and no
test's descriptors outlive it. Raising the limit is not the repair, which
was checked rather than assumed: at 8192 under `cargo test` the
descriptor failures give way to nine `hclient-otel` failures, since those
tests each install a global tracer provider and, sharing one process,
overwrite each other. Two symptoms, one runner. This is invisible on
Linux, which is what makes it worth writing down beside the two reasons
already here.

It is **not** an answer to the macOS CI hang recorded in
[Releases and versions](01-releases-and-versions.md), and the
distinction matters: this is arm64 hardware on macOS 27, where CI runs a
GitHub-hosted runner of another version on another architecture. What it
establishes is that the suite completes on a Mac at all, which nothing
had shown before.
