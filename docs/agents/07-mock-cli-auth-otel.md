# Mock, CLI, auth, OpenTelemetry

> Part of the [AGENTS.md](../../AGENTS.md) record, split 2026-10-05. The root
> file keeps the operational rules and the index of parts. Sections below
> keep their original headings, and a "this file" in the text named the
> whole record when it was written.

### `hc` gained `--sse` and `--ws`, and found two defects it had shipped all along

Both features are ordinary; the two things it turned up on the way are
not.

**`-L` was a flag that did nothing, and that is the *silently ignored
setting* defect inverted.** `Client` falls back to `Limit::default()` —
ten hops — when no policy is set, so `hc` followed redirects with or
without `-L`: measured against the built binary, same body and same exit
code either way. The usual defect is a setting the code ignores; this was
a setting that was silently already **on**, which no amount of reading
the flag's own code would show. It is `Forbid` without the flag now and
`Limit(--max-redirects)` with it — `Forbid` rather than `Limit::new(0)`,
because one is an answer and the other an error. **This changes `hc`'s
default**, to what curl, httpie and xh all do. The rule it earns: a CLI
must state *both* arms of a boolean flag, never only the `true` one,
because the library's default is not the tool's.

**`hc` corrupted every binary body on a pipe, for the CLI's whole life.**
Everything went through one `anstream::AutoStream`, and with colour off
that stream is an **ANSI parser**: it deletes bytes it cannot read as
text. Measured on the built binary — a PNG's `89 50 4e 47 0d 0a 1a 0a`
arrived as `50 4e 47 0d 0a 0a`; `ColorChoice::Always` is byte-exact and
`Never` is not.

That is the *wrapper was the limitation* shape a fourth time, after
`native-tls`'s ALPN, `hclient-fetch`'s `!Send` body and `TlsInfo` in
`hclient-native` — **and it is the worst of the four**, because the other
three were silent absences where this one silently broke a promise
`output.rs`'s own opening paragraph makes about bytes surviving. Payload
bytes bypass the filter now.

**A mutation that hangs is worse than one that fails**, and a suite that
drives a subprocess needs a watchdog for exactly the mutations whose
defect is non-termination. Making `--sse` reconnect unconditionally
produces *no answer* rather than a wrong one, so four of that mutation's
eight kills land on a 20-second watchdog rather than on an assertion.
`hclient-tungstenite`'s `BOUND` is the precedent; this is the rule behind
both rather than two local decisions.

**The `-j96` flake instrument earned its keep a fourth time.** A real
race in the new `--ws` code reproduced 7 times in 12 there and never at
`-j16` or `-j32`: `tungstenite`'s `read` can lift a data frame *and* the
close behind it off the socket in one go, so a `start_send` at stdin EOF
answers *"Sending after closing is not allowed"* — exit 4 on a session
that delivered every byte. The fix is two guards, and they were
discriminated properly: removing either alone gives 0 failures in 12,
removing **both** gives 7. So the suite pins their conjunction and
nothing pins either half, which is written where they are so that
neither is deleted as redundant.

**Two library facts the CLI work established**, both reasons rather than
defects. `SseStream` owns its `Response` and exposes neither `status()`
nor `headers()`, and `SseBuilder` carries a URL and headers and nothing
else — no body, no query, no redirect policy, no `require_version`. That
is why `--sse` refuses six flags by name instead of accepting them into
silence, and it is what would have to change for `--print h` over a
stream. And `hclient_proto::sse` strips one leading space after a
comment's colon, so a faithful re-serialisation is `": {text}"` — the
kind of thing only a round-trip test finds.

**The `--ws` obstacle was the one predicted and the fix is the better of
two.** `Tungstenite` borrows a `Native` and `backend::build` returned an
erased `Client`, so construction is split into *make the transport* and
*wrap it*. `transport_as::<Native<..>>()` was the alternative and is
worse: it returns an `Option` because nothing checked at `build()` which
backend is inside, so it answers a question at run time that splitting
removes. Both paths still go through `backend::choose`, so `--backend`'s
refusal is intact — checked by making `--ws` bypass it and watching the
exit code go from 3 to 4.

### Authentication became a seam, and the measurement says that is where the demand is

Digest was a hard-coded `401` branch in `Client::run`. It is now
`hclient::auth`: [`Auth`] is the configuration and [`AuthFlow`] is one
exchange's state, made fresh per hop. `Digest` is the one scheme this
crate ships, written on the seam like any other.

**The reason is a measurement rather than a taste for abstraction.** NTLM
and Negotiate need a platform's own security provider, and those crates
have real users — `libgssapi` at **565,785** downloads a month, `sspi` at
**561,893**, `cross-krb5` at **496,315**. What does not exist on
crates.io is any HTTP glue over them: there is no `reqwest-ntlm` and no
`hyper-ntlm`, because those clients have nowhere to put one. So this
crate does not grow a Kerberos dependency; it grows the two traits
somebody else needs to write one in their own crate.

**Two traits, for the reason the redirect and retry policies have one
each and a client-owned counter.** The scheme is shared by every clone of
the client; the *flow* is one hop's state, because a scheme with three
legs has to remember which leg it is on and a shared value cannot. It is
httpx's generator-based `Auth` — `response = yield request` — written as
a state machine, which is what Rust has instead.

**Three rules the client enforces and a flow cannot override**: a body
that cannot be replayed ends it (`retry_kind()`, asked before every extra
leg exactly as for `425` and a retry); credentials do not cross an origin
(digest's rule, now every scheme's); and `MAX_LEGS` bounds a flow that
never says `Done`, which would otherwise be an infinite exchange against
a server that keeps challenging.

**The first shape had two flows and the tests killed it in one run.** One
was made before the send and one after the response, so the second had
never seen the request — and digest hashes the method and the
request-target into `A2`, so it had nothing to hash. One flow per hop,
made before the first send, and it sits *outside* the retry loop: a retry
re-sends the same request, and a flow counting legs must not count an
attempt that failed for the network's reasons.

**`on_response` is given only the response, deliberately**, and the
consequence is that a flow stashes what it needs in `authorize` — which
the trait's contract makes safe, since `authorize` runs before every
send. `DigestFlow` does exactly that. The alternative, passing the method
and target to both methods, duplicates what the flow has already seen.

**And `cargo fmt` deletes a marker rather than moving it, which is new.**
This file records that `cargo fmt` moves a trailing comment off a line it
reflows, so a `send-bound-exception` marker on a `pub trait X: Send {`
line is lost. The obvious repair — a trait-level `where Self: Send,` —
is **worse**: `cargo fmt` removes that comment outright. Reproduced in
isolation before it was believed.

What that forced is better than what it broke: **neither trait declares
an auto trait at all.** `Send` is demanded where the facade *stores* the
value — `BoxedFlow` and `SharedAuth`, one line each — which is this
workspace's own rule for a seam, arrived at from a formatter rather than
from the argument.

### OpenTelemetry landed as a transport decorator, and the seam it wanted was the one it could not use

`hclient-otel`: `Instrumented::otel(transport)` or
`Instrumented::tracing(transport)`, one line at `Client::builder`, a span
per request with the OTel HTTP client attribute set, and `traceparent`
and `baggage` on the wire. `docs/otel-design.md` is the design and now
also the record of six places it was wrong.

**The obvious home was `Hooks` and it is not close.** `fn on(&self, event:
&Event<'_>)` takes an immutable event, `&self`, and returns nothing, so
**nothing reachable from a hook can put a header into an outgoing
request** — which is what that seam is *for*. Two smaller facts point the
same way: there is no request-start event, `Event` being the life of a
connection plus a head plus octets, and `fn hooks` is declared on four
backends of six. `Transport::execute` gives both halves away already: the
request arrives by value, so headers are editable, and `Self::Body` is an
associated type, so the body is wrappable — which is what makes the
duration the exchange's rather than the time to first byte.

**Its own crate by the local test, measured rather than asserted.**
`cargo tree -e normal`: `hclient-core` alone is 13 crates, `hclient-otel`
is **16** with `otel`, **18** with `tracing`, **19** with both, and **29**
on `wasm32-unknown-unknown`, where `opentelemetry`'s clock reaches for
`js_sys::Date::now`. A feature of `hclient` would put `opentelemetry` into
every graph in any tree that switched it on.

**And that measurement reversed the design's own arithmetic.** §8 said
`opentelemetry` depends on `tracing` already, so the second front adds no
graph. It depends on it only through the default `internal-logs` feature,
which this crate switches off — so the fronts are additive, and the one
that *propagates* is the **smaller** by two, because `futures-core`,
`futures-sink`, `pin-project-lite` and `thiserror` are already in the
client's graph and `tracing` brings `tracing-core` and `once_cell` that
are not. `otel` is therefore the default feature and `tracing` is not,
which is the opposite of what was planned.

**The two fronts are chosen at the constructor and never by a feature.**
Cargo unifies features, so a feature deciding what a *built* decorator
does would let a neighbour's build add a second span per request to this
one — `Collected::text`'s rule one crate over, that a call must not change
meaning with a feature. A feature decides which constructors exist.

**`tracing` emits and cannot inject, and that is structural.** A
`tracing` span's identity is a `tracing::span::Id` from whatever
subscriber is installed; `traceparent` is a W3C trace-id, and there is no
value to write. The tempting repair — with `opentelemetry` also compiled
in, inject `Context::current()` — is worse than the absence, and the
first draft of this paragraph was **wrong about why**, which is the whole
reason it is worth a paragraph. It said that context is empty under
`tracing-opentelemetry`. Measured against 0.33 on a scratch consumer:
that bridge has `with_context_activation`, on by default, and the current
context inside `execute` names the **enclosing** span — the caller's —
because activation happens on span *entry* and this crate never enters
the span it opens. So the repair writes the **wrong parent**: the
server's span comes out a sibling of the client span rather than its
child. Silently. Said at the constructor instead.

**The same run paid for itself twice.** It confirms the front's actual
value — under the bridge this span exports as an OTel span named `GET`,
kind `Client`, parented on the caller's span and sharing its trace, so
*"OTel for nothing"* is true of everything but the header. And it found a
defect no reading would have: `tracing-opentelemetry` maps a `u16` or a
`u64` field to a **string** and only an `i64` to an integer, so
`server.port`, `http.response.status_code` and `http.request.resend_count`
— all `int` in the registry — were arriving at collectors quoted. Every
number the `tracing` front records is an `i64` now, and the test reads
the `Visit` method each field arrived through rather than its rendering,
because a plain subscriber renders the two identically.

**Three attribute decisions the specification makes and the design got
wrong.** `http.request.method_original` was recorded as *absent, and that
is an answer*; normalising an unknown method to `_OTHER` is a **MUST**,
and it is also what bounds the span name — without it a caller who invents
a method per request puts it in the name, which is the cardinality
blow-up the `{method}` rule exists to prevent. `error.type` had one arm
and has two: a status that indicates an error is reported as the **status
number as a string**, without which a span for a `500` carries an `Error`
status and nothing an aggregation can group by. And `url.full`'s redaction
is userinfo *and* seven named query-string keys — a presigned URL carries
its signature in the query, which is the commoner case by far.

**`resend_count` is `hop + resend` and the field names invite the wrong
mapping**, which is the one thing the design got most emphatically right:
`Attempt` splits the total on a line OTel does not draw, and reading
`resend` alone reports nothing for the third hop of a redirect chain —
exactly the case the attribute exists for. Both halves travel beside it
as `hclient.hop` and `hclient.resend`, because *third send, first hop* and
*first send, third hop* are different failures.

**One mutation survived on purpose, and the measurement is why it is
kept.** Emptying `Recorder`'s `Drop` leaves all 39 tests green, because
both fronts close themselves — `opentelemetry_sdk::trace::Span` has an
`impl Drop` and a dropped `tracing::Span` fires the registry's
`on_close`. The impl stays because `opentelemetry::trace::Span` is a
**trait with no `Drop` requirement** and the API crate carries no `impl
Drop` on any span type: a provider whose spans do not self-end is
conforming, and this crate hands its spans to whatever the application
installed. Leaving the close to the SDK's convenience would make the
promise the SDK's. The killable half of the same rule is
`SpanBody::poll_frame` calling `end` at end-of-stream, and its test is
written with the body **still alive** — a body read to its end and then
dropped looks the same under either mutation.

**And a chain of redirects is flat rather than nested**, which OTel's
model chooses: one client span per request, and a redirect is a resend.
With no ambient span each hop is the root of its own trace; with one, all
three are children of the caller's span and none of each other, which is
what the test asserts rather than merely asserting one trace id.
