# Observing `hclient::Client`: identity first, and a small second seam

**Status: design. Nothing here is built.** What is measured is marked as
measured; every measurement was taken in this tree on 2026-08-30.

`docs/otel-design.md` reached this from one direction and named two things
it could not fix from below: `http.request.resend_count` is not computable,
because a redirect hop and a retry arrive at the transport identically, and
connection facts cannot be attached to a request, because `Head` carries a
`ConnectionId` and no request identity. This is the other half.

It comes out smaller than expected, and the reason is the shape of the
finding: **most of what `Client` does is already visible through seams the
caller supplies, and none of it is attributable.** So the load-bearing piece
is an identity, not an event list — and once the identity exists, the event
list that survives this workspace's own bar is three variants.

## 1. What was measured first

**`fn hooks` is declared on four backends and nowhere else.**
`hclient-native` (`src/lib.rs:1400`), its QUIC arm `H3`
(`src/http3/mod.rs:391`), `hclient-fetch` (`src/lib.rs:139`) and
`hclient-wasi` (`src/lib.rs:98`). The string `Hooks` does not occur at all in
`hclient-urlsession/src`, `hclient-winhttp/src`, `hclient-tower/src` or
`hclient-mock/src`. In `crates/hclient/src/client.rs` the word occurs
**once**, in a doc comment saying that a cache hit means "hooks see nothing
for that hop, because there was no exchange."

**Three of `Client`'s stages are already observable, because the caller
supplies them.** `RedirectPolicy::follow` is handed a `ProposedRedirect`,
which has a public constructor (`hclient-proto/src/redirect.rs:144`);
`RetryPolicy::retry` is handed a `ProposedRetry` carrying the attempt
number, the outcome and whether the body is replayable
(`hclient-proto/src/retry.rs:375`); and `AuthFlow` *is* one hop's state, so
a scheme sees every leg with the method, the URI, the headers and the
status. A `CacheStore` sees `get` and `put`. So a caller who wants to watch
redirects, retries or authentication can already do it by writing a policy
that logs and returns the verdict it was going to return anyway.

**None of them can say which request they belong to.** A policy lives inside
the `Client`'s `Arc` and is shared by every clone and every request in
flight — the same argument that keeps the hop counter out of `Limit` and in
`run`'s local. Two concurrent requests interleave their proposals in one
stream with nothing to separate them. That is the whole defect, and it is
one defect rather than four.

**And using a policy as an observer corrupts a property those seams are
built on.** `hclient-proto/src/redirect.rs:1` opens *"A pure function: no
I/O, no time"*; `retry.rs:4` says `Standard::decide` is *"a pure function of
the rule and one outcome"*, and `jitter` was moved onto the proposal
specifically so that it stayed one. A logging policy has side effects, and
`.and(..)` composes policies as a meet on a lattice whose order is
deliberately unobservable. So the route exists and taking it turns a
decision seam into a middleware seam, which is the confusion `redirect`'s
own doc exists to prevent.

**One shipped feature is already wrong for exactly this reason.**
`hc -w '%{num_redirects}'` is computed as `heads - 1`
(`hclient-cli/src/timings.rs:263`) — the number of `Event::Head`s minus one
— because there is no redirect event to count. `hc` supports digest auth
(`run.rs:471`, behind `--digest`), and a digest exchange is two heads and no
redirect, so `hc -w '%{num_redirects}' --auth u:p --digest` against a
challenging server reports **1**. A retry that got a response would add another, and a hop served from
the cache would subtract one. curl's `%{time_redirect}` and
`%{redirect_url}` are absent from `KNOWN` (`timings.rs:161`) because nothing
can answer them. This is a named, in-tree reader with a named missing fact,
which is the bar this workspace sets before a seam is worth building.

**`docs/competitive-gaps.md:296` says the same thing from outside.** It
records `isahc`'s `Metrics` and urllib3-future's `response.conn_info` as
attribute accesses on a response, against *"a caller who wants one request's
numbers must write a hook and correlate by `ConnectionId`"* here.

**Two statements in `docs/otel-design.md` are already stale**, and both are
this week's parallel work rather than anything wrong when written: `Event`
has **six** variants, not five (`Progress` landed), and `Hooks::on` takes
`&Event<'_>`, not `Event<'_>`. §1's conclusion is unaffected — a hook still
returns `()` and still cannot write a header — and there is still no
request-start event.

## 2. The load-bearing half is an identity, and it is not a seam

The proposal is two things and only the first is load-bearing:

1. **`RequestId`**, minted once per operation by `Client`, travelling in
   `http::Extensions`, and carried on every `Event` payload.
2. A **small** client-level seam, `ClientHooks`/`ClientEvent`, for the facts
   nothing else can report.

The order matters because the first closes both of `otel-design.md`'s
absences **without the second existing at all**:

- A transport decorator receives the request by value before the inner
  transport does, so it can read the identity and the hop counters and put
  them on its span. `http.request.resend_count` becomes a field it reads
  rather than a number it cannot compute.
- A `Hooks` impl watching connections sees a `RequestId` on `Connected`,
  `Reused`, `Head`, `Informational` and `Progress`, so a span opened by the
  decorator and a handshake observed by the hook join on a key. On a
  multiplexed h2 connection the `ConnectionId` is not the address of *this*
  request, and now something is.

So if only one of the two is built, build the identity.

## 3. Where the identity is minted, and why nothing below `Client` can

**`Client::execute_with`, before `run`.** One id per *operation* — not per
hop, not per attempt — and the hop and resend counters travel beside it:

```rust
/// Which request. Process-wide, monotonic, minted per operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct RequestId(u64);

/// The identity plus where in the operation this attempt sits.
#[derive(Debug, Clone, Copy)]
#[non_exhaustive]
pub struct Attempt {
    pub id: RequestId,
    /// Redirect hops taken so far. `0` on the first.
    pub hop: u8,
    /// Sends of *this* hop before this one: a retry, a `425` replay, an
    /// authentication leg. `0` on the first.
    pub resend: u16,
}
```

`#[non_exhaustive]` on `Attempt` and a `new` beside it, because the type
lives in `hclient-core` and the crate that *builds* it is `hclient` — the
same pair, and the same reason, as `ClientCertRequest`: the attribute stops
a reader breaking on a field, and the constructor stops the attribute
locking the builder out. `RequestId` carries no attribute at all: it is a
newtype over a private counter, with `get()` for a log line, exactly as
`ConnectionId` is.

**Nothing below `Client` can mint the operation identity**, and that is the
argument for putting it there rather than in a decorator. A decorator sees
one `execute` and can mint one id per call — which is exactly the state
`otel-design.md` §5 describes as insufficient: hop 2 of a redirect chain and
attempt 2 of a retry are two `execute` calls and it cannot tell them apart,
let alone group them. The operation exists only in `Client::run`'s loop, so
the identity has to be minted where the loop is.

**One type rather than two, and one insert per hop.** `RequestId` alone is
what an event carries; `Attempt` is what goes in the extensions, because the
three facts are never wanted apart — a hop number with no request is
meaningless — and one insert per hop is cheaper than two.

**It travels in `http::Extensions`**, which is the channel `AllowEarlyData`,
`RequireVersion` and `ClientIdentity` use, and their own reason applies
verbatim: a per-request value that **the transport reads**. `ClientIdentity`
also states the limit — *"extensions reach `Transport::execute` and are
readable by any transport, including one this workspace did not write, which
is why digest's password travels as an argument instead"*. A counter is not
a credential, so it passes that test. The rule it earns is the negative one:
**the identity is a number and never a trace context.** A `traceparent` in
the extensions would hand a cross-service correlator to every transport in
the graph; propagation is the decorator's, `otel-design.md` §3.

The alternative — a parameter on `Transport::execute` — is refused because
it changes the one seam every backend implements, for a value four of six
backends have nothing to do with.

**It crosses an origin, and `AllowEarlyData` does not.** `next_hop` clones
the extensions and strips exactly one type, only when the host or scheme
changed (`stages/redirect.rs:76-84`). The identity stays on that list's far
side deliberately: a redirect chain is *one operation*, and the mark that is
stripped is a judgement about a particular server, where this is a name for
work the caller asked for. Stripping it would make the chain unjoinable at
precisely the hop where joining it matters.

**The caller cannot supply one instead**, which was checked before the
identity was designed rather than assumed: `RequestBuilder` has fourteen
public setters (`request.rs`) and not one of them writes an arbitrary
extension. There are three named extension setters — `timeouts`, `redirect`,
`require_version` — and no general one. So a correlation token has to come
from the library or from nowhere.

## 4. The identity on the events: a setter, never a `new` parameter

Every payload in `hooks.rs` is `#[non_exhaustive]` **and** built by a
transport, and the constructors' own comment states the split: *"what a
value cannot be without goes in `new`, and what a backend may not know goes
in a setter. So a new field that a backend may not know is additive; one it
must supply is a break, and no attribute has ever protected against that."*

A request identity is exactly a thing a backend may not know — a transport
used directly, with no `Client` above it, has no extension to read — so it
arrives as `Head::request(id)` and its four siblings, defaulting to
`RequestId::UNIDENTIFIED`. That is `ConnectionId::UNWATCHED`'s shape one
field over, with one producer instead of two: *this event names no request*.

The duty is on the seam and cannot be enforced by a type: a backend that
forgets the setter reports `UNIDENTIFIED`, which is the **understating**
value, `reports_alpn`'s and `applies_ech`'s rule. Under-reporting costs a
hook a join it cannot make; over-reporting would attach one request's
handshake to another's span, and there is no third answer.

## 5. Is this a second seam or a lifted one — and it is a second one

Three routes were weighed. The loser costs are stated because two of the
three are genuinely attractive.

**Route A: `Client` becomes generic over `H: Hooks`.** Rejected outright,
and the reason is not taste: it undoes the erasure that is this crate's
headline property. `Client` names no type parameters, which is what lets
`hc --backend` be an ordinary `match` and what took four `where` lines off
`examples/portable.rs`'s `fetch`. Buying observability with that is the most
expensive line in this document.

**Route B: one trait and one enum — `Hooks` lifted, with `Event` growing
client variants.** Attractive: a caller writes one `impl`, installs it at
both levels, and reads one ordered stream. Two facts stand against it,
and only the second kills it.

*The first is mechanical and surmountable.* `Hooks` has an associated const,
so it is not dyn-compatible — `HooksExt`'s own doc says so, *"it was never
object-safe and never could be"* — and `Client` must store its hook erased.
That is fixable: read `H::WATCHING` at the setter and store `None` when it
is `false`, so the const survives erasure as the presence of an `Option`,
and box the rest behind a private object-safe shim. Worth writing down,
because it is the mechanism §9 uses anyway.

*The second is not.* `Event` lives in `hclient-core::unversioned`, its own
doc line is **"What a transport reports"**, and its primary reader is a
backend author. Adding `Redirect`, `Retry` or a cache disposition to it puts
variants in front of every backend author that no backend may ever emit —
and this enum has already had a variant **deleted** for that shape: the doc
records that a "request queued" variant was dropped because
`hclient-native` had nothing to put in it, and *"a variant no code can emit
is a capability that lies"*. Reusing `Event` would add three of them on
purpose.

**Route C, which is what this proposes: a second seam, mirroring the first
line for line, in `hclient-core::unversioned`.**

```rust
pub trait ClientHooks {
    const WATCHING: bool = true;
    fn on(&self, event: &ClientEvent<'_>);
}
```

Same `WATCHING` const and same defaulting argument, same `Arc`/`&`/`Rc`
forwarding impls, same `ClientHooksExt::and` with the same `||` on
`WATCHING` and the same *"`self` runs first"* promise, same
`#[non_exhaustive]` resolution. **Deliberately the same spelling**, because
`HooksExt`'s doc already states the rule: *"a third spelling for the same
idea is a reader stopping to work out whether the difference means
something."*

**In `hclient-core::unversioned` rather than in `hclient`**, for two
reasons. The vocabulary is brand new and unvalidated, and the quarantine is
where such a thing belongs — outside it, a breaking change to an event set
nobody has used yet costs a major version. And core already names
client-level concepts it has no code for: `Capabilities::owns_cookie_jar`,
`owns_cache` and `RedirectSupport::Internal` are all about `Client`'s
behaviour, defined in core and read in `hclient`. `RequestId` has to be
there regardless, because transports stamp events with it.

**The measured cost of two traits, and it is one line at one kind of call
site.** A type implementing both gets `error[E0034]: multiple applicable
items in scope` on `h.on(&ev)` — measured on a two-trait reproduction,
rustc does not disambiguate by argument type. Library code never meets it,
because there the bound picks the trait; a caller's own test does, and
writes `ClientHooks::on(&h, &ev)`. `hclient-cli`'s `timings.rs:452` is the
shape that would have to change. That is the whole bill for the duplication,
and it is cheaper than either loser.

## 6. Which events, and what was cut

The bar applied to each candidate: **name the reader, and show the fact is
not recoverable from outside.** Four candidates failed it and are cut.

### The three that survive

```rust
#[non_exhaustive]
pub enum ClientEvent<'a> {
    /// An operation began. Binds the identity to a method and a URI.
    Started(Started<'a>),
    /// One attempt at one hop ended.
    Hop(Hop<'a>),
    /// The operation ended, at the head.
    Completed(Completed<'a>),
}
```

**`Started { id, method, uri }`.** The reader is any hook at all: the first
transport event a hook sees carries a `RequestId` it has never met, and
nothing else in the stream ever states the method. Not recoverable — the id
exists nowhere else, and there is no channel for a caller to supply one
(§3).

**`Hop { id, hop, resend, cause, method, uri, served, retry_in }`**, emitted
once per attempt, **at its end**. `cause: HopCause` says why this attempt
exists — `Initial`, `Redirect { status }`, `Retry`, `TooEarly`, `Auth`;
`served: Served` says what answered it — `Origin(StatusCode)`,
`Cache(StatusCode)`, `Revalidated(StatusCode)`, `Failed(&Error)`;
`retry_in: Option<Duration>` is the delay about to be slept, which is known
only here and is the difference between a hook that can say *"retrying in
4 s"* and one that goes silent for four seconds.

Readers: `hc -w '%{num_redirects}'`, which is wrong today and would become a
count of `HopCause::Redirect`; `%{time_redirect}` and `%{redirect_url}`,
which have no answer today; and any decorator computing `resend_count` for a
hop the transport never saw. Not recoverable: the policy seams see
*proposals*, before the fact and unattributed, and reading them corrupts
their purity (§1).

**At the end rather than at the start**, which costs something and is worth
naming: a hook learns that a hop is happening only when it is over, so a
long connect is silent at this level. It is not silent overall —
`Connected`, `Informational` and `Progress` fire underneath during exactly
that window — and one event per attempt rather than two is what keeps the
cost in §9 honest. Attribution works by ordering: everything between two
boundaries in one `RequestId` belongs to the hop the second one names.

**`Completed { id, hops, resends, elapsed, outcome }`.** The weakest of the
three and kept for one reason: **a failed operation produces no `Head` at
all.** A redirect refused by a policy, `TooManyLegs`, a `total` timeout, a
body that vanished before a retry — none of these reaches a transport, so a
hook that saw `Started` sees silence and cannot tell a failure from a hang.
`elapsed` is free: `execute_with` already reads the clock unconditionally
(`let started = self.inner.timer.now_boxed();`) because the deadline needs
it. Weak because the *caller* has the outcome at the call site; kept because
a hook is installed once for an application and the alternative is wrapping
every call site.

**It is emitted at the head, not at the end of the body**, and that is
deliberate rather than an oversight. `Client::execute` returns at the head;
the body is `Limited<Decompressed<Deadline<Cached<B>>>>` and is the caller's
after that. Ending a span at the end of the body — which
`otel-design.md` §4 rightly requires — is the decorator's `SpanBody`, where
the wrapper already exists. Putting a hook reference into every response
body to emit one more event would duplicate that machinery for one event.

**And an abandoned operation emits `Started` and no `Completed`**, because
the module doc's rule is that no hook is ever called from a `Drop` — a panic
there, during an unwind, aborts the process. That is the same hole `Closed`
already has for a connection the caller dropped, and it is written down for
the same reason.

### The four that were cut

**Cookies applied and stored.** `Client::cookies()` hands the caller a
guard over the jar, and `cookie_header` is a pure function of the jar, the
URI and a `now` that a caller can re-run. No reader could be named beyond a
log line. Recoverable, and cut.

**Decompression.** The decoded octet count is the caller's own — they are
holding the frames, which is `Progress`'s own argument for counting the
encoded ones. Which coding was reversed is genuinely invisible, because the
`Content-Encoding` header is stripped from the response; but that is a
missing accessor on `Response`, not an event, and it should be built as one.

**Authentication legs as their own variant.** `AuthFlow` is already a
per-hop observer with full fidelity — it is handed the method, the URI, the
headers and the status, and it is *made fresh per hop*, so it already knows
which hop it is. What it lacks is only the identity, which §3 gives it if
`Auth::start` is ever widened. A leg is a `Hop` with
`cause: HopCause::Auth`, and one variant is enough.

**A client-level `Progress` over decoded octets.** Cut on cost, and this is
the one that shapes §9: it would be the only client event on a per-frame
path, where the runtime `Option` test that erasure forces would be paid per
frame instead of per hop. The transport's `Progress` already reports the
encoded count with a `WATCHING` const gating it properly, and the decoded
count is the caller's.

**And this is the honest summary of the cut list:** three of the four are
cut because the fact is already reachable, and the reason none of them felt
reachable before is that none of them was *attributable*. Fixing that is §3,
not an event.

## 7. Exhaustiveness: what a new variant costs, and to whom

`Event` is `#[non_exhaustive]` today and buys the compile error back in one
place — `every_event_is_accounted_for` in `hooks.rs`'s own test module,
verified by reading it: a `match` with no `_` arm, legal because the
attribute is inert inside the defining crate. Adding a variant is one
compile error in one known file and no break for anybody outside.

`ClientEvent` copies that exactly, with its own
`every_client_event_is_accounted_for`. Adding a variant costs one compile
error in `hclient-core` and nothing to an out-of-tree hook.

**`HopCause` and `Served` are also `#[non_exhaustive]`, and that is where
this differs from `Direction` beside it.** `Direction` is exhaustive because
a hook that draws an upload bar and a download bar *branches*, and a third
direction is inconceivable — the same rule `ClientCertAsk` states, where
three states are the complete answer to a closed question. `HopCause` is the
opposite: every stage `Client` grows is a new cause, and the list has grown
three times in this workspace's own history (the `425` replay, the retry
loop, the auth flow). A growing enum that out-of-tree hooks branch on must
not break them every release. So the split is by *whether the set is
closed*, not by whether it is branched on, and both halves of that rule now
have an example.

Adding a **field** to a payload costs nothing in either direction, provided
it arrives as a setter (§4).

## 8. `Send`: the client seam cannot have the transport seam's answer

`Hooks` declares no `Send`, and P13 — settled by construction in
`hclient-core/tests/shape.rs` — is that a hook holding an `Rc` still works,
because the response body *holds* the hook and a bound anywhere on that path
would shut every single-threaded runtime out of observability.

**`ClientHooks` cannot keep that, and the reason is structural.** `Client`
is `Send + Sync` — asserted in `hclient/tests/shape.rs` — because `Inner`
lives in an `Arc` and stores `Box<SharedTransport>`, which is
`dyn BoxedTransport + Send + Sync`. Anything else stored beside it must be
`Send + Sync` too. So the setter reads:

```rust
pub fn hooks<H>(mut self, hooks: H) -> Self
where
    H: ClientHooks + Send + Sync + 'static, // send-bound-exception: amendment-C12
```

which is one more site of amendment C12 — measured, there are 38 markers in
`crates/*/src` today — and `ClientBuilder::cookie_jar`'s shape verbatim:
`P: PublicSuffixList + Send + 'static`, with the marker on the same line,
because `cargo fmt` moves a trailing comment off a line it reflows. **The trait declares nothing; the bound is where the facade
stores the value**, which is this workspace's rule for a seam and the one
`hclient::auth` arrived at from a formatter rather than from an argument.

**Nothing is lost that was not lost already.** `hclient-fetch`'s
`a_non_send_hook_works_all_the_way_through_the_transport` already moved down
to the `Transport` layer when `Client` was erased, and its doc says exactly
what remains true here: *"What is genuinely lost is watching a `!Send`-hooked
browser transport through the facade."* A single-threaded runtime can still
watch — at the transport, where the property lives — and a client-level hook
on that target is refused at the line that asked for it rather than silently
downgraded.

**Two measurements that contradict what the tree says about itself**, taken
because this section depends on them. Compiled from a scratch crate outside
the workspace with a path dependency:

- `Client::execute`'s future **is `Send`**, and `hclient::body::ClientBody`
  **is `Send`**. `crates/hclient/tests/shape.rs` says both are not, in prose
  and in two `compile_fail` doc blocks — and **those blocks never run**:
  `cargo test --doc -p hclient --all-features` reports 13 doctests, every
  one of them from `src/`, because rustdoc only collects doctests from
  library targets. A `compile_fail` in an integration test is a check that
  cannot fail.
- `erased::BoxExchange` carries `+ Send` (amendment C16) and `BoxBody`
  carries `+ Send` (amendment C14), which is why. `BoxSleep`'s doc comment
  says *"Not `Send`"* one line above the declaration that says otherwise.

None of that changes the design — a client hook needs `Send + Sync` because
`Client` is `Sync`, not because its future is anything — but the stale
claims are why this section states its premise from the declarations rather
than from the prose around them.

## 9. Cost when nobody is watching

**`Hooks::WATCHING` cannot survive erasure as a const, and it does not have
to.** It is read at the setter:

```rust
if !H::WATCHING { return self; }   // the hook is never stored
```

so a `NoHooks`-shaped client hook leaves an `Option::None` behind, and the
const's whole promise — *a build with no hook does not read a clock and does
not take an id from a counter* — becomes the emptiness of an `Option`. What
is lost against the transport seam is that the branch is a runtime test
rather than one a monomorphised `NoHooks` deletes.

**That is affordable here and would not be one layer down, and the
difference is the number of event sites.** The transport seam's const gates
work on a per-frame path — `Meter`, `Counting`, `Reporting` — where a
predictable branch per frame is a real cost and where `hclient-fetch` has
already had a mutation survive its whole suite by leaving one gate out. The
client's event sites are **per hop**: at most one `Started`, one
`Completed`, and one `Hop` per attempt, bounded by the redirect limit times
the retry budget. That is the rule §6 cut the client-level `Progress` on:
**no client event may be emitted on a per-frame path.**

Measured, single-threaded, `--release`, on this host:

| | ns |
|---|---|
| `Option::<Arc<_>>::as_ref()` on a `None`, per event site | 0.00, optimised away |
| relaxed `AtomicU64::fetch_add`, uncontended | 4.70 |
| relaxed `AtomicU64::fetch_add`, 8 threads on one counter | 12.68 wall per op |
| `Box::new(u64)` | 5.65 |
| a second `http::Extensions::insert` into a map that already has one | 11.08 |

**So minting the identity costs about 16 ns per request and is
unconditional**, which is a decision rather than an oversight. Gating it on
a client hook being installed would deny it to the two readers that motivated
it — a transport-level `Hooks` impl and an OTel decorator — neither of which
the client can see. And the comparison that settles the size of it is
in-tree: `execute_with` already calls `now_boxed()` unconditionally, which is
at least the `Box::new` row, and `run` already does
`hp.extensions.insert(effective)` unconditionally for `Timeouts`, which is
the row above it. The identity is a second insert beside an existing one and
an atomic beside an existing allocation, against a network round trip three
to five orders of magnitude larger.

A `Capabilities` field reporting whether the transport is watching was
considered as a gate and rejected: it cannot see a decorator, so it would
gate off exactly the configuration `otel-design.md` proposes.

## 10. What this does not do

**Injection is settled and is not revisited.** A hook returns `()`, the seam
exists so that a backend can announce what happened without a caller being
able to change what happens, and `execute` takes the request by value so a
decorator can. `docs/otel-design.md` §1 and §2 have the argument; nothing
here weakens it, and `ClientHooks::on` returns `()` for the same reason
`Hooks::on` does.

**It does not put the facts on the response**, which is the shape
`competitive-gaps.md:296` records as isahc's and urllib3-future's, and which
would serve most of §6's readers with no seam at all: `Response::hops()`,
`Response::resends()`, `Response::request_id()`, a `Metrics` accessor. That
is a real and separate proposal, it is tracked in that row already, and the
two do not compete — a return value serves facts that arrive **with** the
answer, and a hook serves facts that arrive **before** it. Building the
identity first makes both cheaper, because it is what a caller would join
on.

**It does not touch the transport seam's vocabulary**, beyond one setter per
payload. The `Event` enum keeps meaning *what a transport reports*.

## 11. Deliberately not done, each with what it needs

- **A span that ends with the body.** Needs the decorator's `SpanBody`
  (`otel-design.md` §4), not a client event.
- **`Auth::start` seeing the identity.** One parameter, when somebody wants
  it; the flow is already the per-hop observer.
- **A `!Send` client hook.** Needs `Client` to stop being `Sync`, which is
  not a trade anybody should take. The transport layer is the answer and
  already works.
- **Which content coding was reversed.** An accessor on `Response`, because
  the header is stripped and nothing else can say.
- **Per-frame client events.** Needs the runtime `Option` to become a const
  again, which needs `Client` to stop being erased.

## 12. The order to build it in

1. `RequestId` and `Attempt` in `hclient-core`; minted in
   `Client::execute_with`, updated per hop and per resend in `run`.
2. The `request(..)` setter on the five `Event` payloads, and each in-tree
   backend reading `Attempt` from the extensions. Four backends, one line
   each; a test per backend that the id on the event is the id on the
   request, because a forgotten setter is silent.
3. Stop there and check: at this point `otel-design.md`'s two absences are
   closed and `hclient-otel` is writable. Everything after this is worth
   less than everything before it.
4. `ClientHooks`, `ClientEvent` and the three variants, with
   `every_client_event_is_accounted_for` and the `WATCHING`-at-the-setter
   gate.
5. `hc -w`: `%{num_redirects}` off `HopCause::Redirect` instead of
   `heads - 1`, plus `%{time_redirect}` and `%{redirect_url}`. That is the
   first reader, it is in this repository, and it is currently wrong.
