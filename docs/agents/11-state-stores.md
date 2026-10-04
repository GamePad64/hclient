# The state stores

> Part of the [AGENTS.md](../../AGENTS.md) record, split 2026-10-05. The root
> file keeps the operational rules and the index of parts. Sections below
> keep their original headings, and a "this file" in the text named the
> whole record when it was written.

### A crate was green in the workspace and did not build on its own

`cargo check -p hclient-native --all-features --all-targets` failed with
four errors on `main`, and had for as long as `tests/h3_two_runtimes.rs`
existed: it instantiates the HTTP/3 path under `Smol`, which needs
`hclient-rt-smol/udp`, and this crate's manifest did not ask for it —
`hclient-rt-tokio` one line above does. It compiled because another member
turns the feature on and **Cargo unifies features across a graph**.

**This is the third sighting of one shape**, after the two doctest
examples that compiled only because a neighbour enabled a feature and the
three backends that owed `SendTransport`. All three are one sentence: *a
green `--workspace` run is a claim about the workspace, not about any
crate in it.*

**`just features` could not have caught it, and that is structural.**
`cargo hack --each-feature --no-dev-deps` is blind to a dev-dependency by
construction and builds no test targets — so the defect was invisible to
the gate that looks closest to it. `just check-each-crate` is therefore a
new gate rather than a widened one: every member built alone, with its own
dev-dependencies and its own targets, which is how anybody who downloads
one builds it. Twenty-one crates; five are excluded because their code is
for a target this host is not, and `check-targets` covers those by
**naming the target** rather than by skipping the crate. Both halves were
checked in the failing direction, the second being a loop over nothing —
a green run over zero crates is this file's recurring defect. A sweep over
every other member found no second case.

### A response cache landed, and it is the counterpart `owns_cache` never had

`hclient::cache` — RFC 9111 freshness, validation, `Vary` and the
directives on both sides — **sans-io and clockless**, exactly as
`hclient::cookie` is, and reaching for neither `Client` nor
`hclient-core`. It was the `hclient-cache` crate when it landed; the
section on folding the two in says what moved and what that cost.
`ClientBuilder::cache(HttpCache::new())` switches it on behind
`hclient`'s `cache` feature, off by default. `Client` supplies the
`now` as `SystemTime::now()` for the reason the jar does — `Date`,
`Expires` and `Age` are calendar values and `Timer::Instant` is a
stopwatch with no epoch.

**It is a private cache**, a user agent's rather than a shared one, and
three rules turn on that: `private` is stored, `s-maxage` is not read at
all, and a response to an authenticated request is stored with the
credential in its `Selector` rather than refused — a narrowing of §3.5
that a private cache needs and the RFC does not require.

**`Capabilities::owns_cache` finally has a reader.** It had been a `bool`
set by one backend — `hclient-fetch`, because the browser caches inside
`fetch()` — and branched on nowhere, which is the shape `proxy` and
`client_certs` were found in this same week. A client-side cache against
a transport reporting `true` is now an `UnsupportedCapability` at
`build()`, the arm that field's own doc comment had promised since v0.1.

Four things worth knowing before touching it. `Lookup` has **four**
answers, because *send it*, *send it with these fields added* and *do not
send it at all* are three instructions and an `Option` carries two. **A
validator alone is enough to store on**, and the absence of heuristic
freshness is load-bearing rather than a gap. **A `304` does not relabel
the stored bytes** — `Content-Encoding` is excluded from the update set,
which `hclient`'s decompressor makes concrete. And `stale-while-
revalidate` is deliberately absent: it needs somewhere to run the
revalidation after the response has been handed over, and this client
does not spawn on a caller's behalf — the same sentence the h3 body pump
and the WebSocket keep-alive are written under.

**The wiring's own defect was found by a stack overflow three crates
away.** With the feature on, `hclient-native`'s
`checkout_walks_past_a_dead_connection_to_a_live_one` aborted with
`SIGABRT` — a test that configures no cache at all. Measured: the
`execute` future is 4,232 bytes without the feature and 4,344 with it,
and that test needs between 1 and 2 MiB of stack without and between 2
and 2.5 MiB with, against a 2 MiB default. It is the only place in the
suite holding **two** whole client futures in one frame, through
`tokio::join!`, so it noticed first — and it noticed by aborting rather
than by failing anything. The two futures are boxed there now, and the
bound is stated where a reader will find it: `tests/future_size.rs`
asserts the future stays under 8 KiB, a ceiling with room rather than
today's number, because a test pinned to the current value gets relaxed
without thought. `cached::Cached::recorder` was already boxed for the
same reason with its own measurement recorded; this is that finding one
level up.

### The cache store waits now, and the client stopped holding a lock to ask it

`CacheStore`'s six methods are futures and every one takes `&self`. The
store this crate ships is in memory and answers [`std::future::Ready`], so
the shape costs it no allocation and no suspension; what it buys is the
stores that were unwritable before — on disk, in Redis, in
`moka::future` — for which a synchronous seam offered two options, block
the executor or do not exist.

**`&self` is not a second decision, it follows from the first.** A store
that awaits cannot be held behind a `&mut` across that await, and a store
that is remote is shared by everyone talking to it already. So
synchronisation is the store's own: `MemoryStore` holds a `Mutex` around
its map, which is a *narrower* lock than the one it replaced.

**What went was `Arc<Mutex<HttpCache>>` in `Client`.** The old paragraph
beside it argued the lock was safe because it was never held across an
`.await` — true, and it stopped being true the moment the store could
wait. Rather than an async mutex there is now no lock at that level at
all, so two requests no longer queue behind each other to read a header.
`Client::cache()` hands back a borrow rather than a `MutexGuard`, and the
warning it carried — that holding the guard across an `.await` could
stall a body belonging to another handle — is gone rather than reworded.

**Associated futures, not `async fn`**, for `TcpConnect::Connecting`'s
reason: `Client` boxes its cache `Send + Sync`, and an RPITIT cannot be
bounded. `AnyStore` is the erased half, and it is `DynTransport`'s split
one crate over — a private object-safe trait with a blanket impl, so **a
store author writes nothing** and `Send` is inferred where the type is
still concrete.

**Two costs, both measured.** `Client::execute`'s future went from 4,344
bytes to **5,776** when the two cache hooks became `async fn`, against a
6 KiB ceiling that is meant to have room; boxing those two calls brings it
to **4,784**, at two allocations per hop on a path about to touch a store.
And `AnyStore`'s `Debug` no longer prints a count, because `len` is a
future and a `Debug` cannot await one — a number a remote store might
disagree with is worse than none.

**The write is a state of the body rather than a call inside it.**
`Recorder::commit` was one line in `poll_frame`; it hands back a future
now, which the body drives to completion before it reports the end of the
stream. It is deliberately **not** spawned — a spawned write lets the body
end before the entry exists, so a caller who asks again immediately misses
what it was just told was cached, and this crate does not spawn on a
caller's behalf anywhere.

Setting that field and returning the poll's own answer was the first
shape, and it stored nothing at all: the body had ended, so nothing would
ever poll it again and the future was dropped where it stood. Three cache
tests said so on the first run.

**The suite needed a store that actually waits**, because every store in
this workspace answers `Ready` and a suite built on those would pass for a
seam that had never stopped being synchronous. `SuspendingStore` in
`tests/pluggable_stores.rs` suspends once per operation and counts it; the
test asserts a second request is served from it *and* that it suspended.
Removing the body's wait for the write fails four tests across two files.

### The jar took a store too, and the key could not be the cache's

`hclient::cookie::CookieStore` — `get`, `all`, `put`, `remove`, `touch`,
`len`, `clear`, every one a future and every one taking `&self`, which is
`CacheStore`'s shape verbatim and for its reasons: a store that awaits
cannot be held behind a `&mut` across the await, and a store that is
remote is shared already. `CookieJar<P = BuiltinList, S = MemoryStore>`
is the rules over it, and `MemoryStore` answers `Ready`, so the shape
costs a plain `CookieJar::new()` no allocation and no suspension.

**The key is a domain, and that is the one place this seam could not copy
the cache's.** A cache has an exact key — the method and the target URI —
so `CacheStore::get` is a lookup and every rule is applied to what comes
back. Cookie retrieval has no such key: §5.1.3's domain-match is a suffix
relation and §5.1.4's path-match a prefix one, so a store asked *what
applies to `https://a.b.example.com/x`* would have to implement RFC 6265,
which is the thing the seam exists to keep on this side of it. What **is**
exact is the domain a cookie was stored under, and the set of domains that
can match a host is bounded and computable from the host alone. So the
rule enumerates the candidates and the store answers an exact lookup for
each — `candidate_domains` is `domain_matches` turned inside out, and the
two are asserted against **each other** rather than against a second list,
so a change to the rule that the enumeration did not follow fails a test.
Checked in the failing direction: dropping the IP-literal arm fails both
of its tests.

**A wrong store cannot put a cookie on the wire**, and that is a property
of the code rather than a hope: `matching` re-applies domain-match,
path-match, `Secure` and expiry to whatever `get` answers, so a store that
returned the wrong rows loses them at the filter. That is the half of
`CacheStore`'s safety argument that had to be *built* here rather than
inherited.

**What it bought is the lock.** `Client` held `Option<Mutex<CookieJar>>`,
and `Client::cookies()` handed back a `MutexGuard` — with a paragraph
warning that holding it across an `.await` blocked every other request of
every clone, and a second paragraph about recovering a poisoned lock. Both
are gone rather than reworded, exactly as the cache's were: the accessor
is a borrow, two requests no longer queue behind each other to read a
header, and the synchronisation is the store's own where a store needs
any.

**And the lock was manufacturing a property, which nobody had noticed.**
`Mutex<T>` is `Sync` whenever `T` is `Send`, so `AnyList`'s `Sync` was the
lock's rather than the list's. Removing the lock moved the bound to where
it actually has to hold: `ClientBuilder::cookie_jar` now asks `P: Send +
Sync`. A list that is genuinely `!Sync` was never usable from two threads;
what changed is that it says so at the setter instead of working until
someone shared the client.

**`Limits` split, on the line `crate::cache` already draws.**
`max_name_value_bytes` is a **refusal** — decided before a `Cookie` exists
and never reaching storage — so it stays on the jar. `max_cookies` and
`max_per_domain` are what a store can **hold**, so they are
`cookie::Capacity` on `MemoryStore`, beside `HttpCache`'s `Limits { max_body_bytes }`
and `cache::MemoryStore::with_capacity`. A wrong `Capacity` loses cookies;
a wrong refusal would store one the jar was told not to.

**One thing the seam owed and the cache's still does not pay.**
`CookieStore` traffics in `Cookie`, whose fields are private, so a store
that outlives the process could not have handed one back — a seam naming a
use it could not serve. `Cookie::from_record` closes it: the serialisable
form is `CookieRecord`, which this module already argues is the thing to
write down, and session cookies having no record is not a gap but the
meaning of the word. It deliberately does **not** re-run
`CookieJar::restore`'s checks — the public suffix list, the name prefixes,
the IP-literal rule — because a record on disk is a *claim about scope*
where a store is handing back a cookie the jar gave it, and a second
quieter copy of §5.7 is how the two would drift.
The same hole was open one module over and is closed now too — see the
section below, which is what asking *what else* found.

**`async fn` in traits was tried first, and the three outcomes are the
argument.** It is stable and it is the obvious way to write this, so it
was written on a scratch crate rather than dismissed. `Client` boxes its
jar `Send + Sync`, so something must prove the store's futures `Send`:
with `async fn` on the trait the erasure is `E0277`, because a generic
impl cannot prove a property of a future it cannot name; return type
notation names it and is `E0658` on stable, which this workspace has
already measured the full cost of and declined; and rustc's own
suggestion — `+ Send` on the seam — compiles and **excludes every
single-threaded store**, refusing an implementor that holds an `Rc`
across an await while accepting the same one holding an `Arc`. That is
`Resolve → BoxStream`'s finding with the subject changed: a fixed `Send`
in a seam excludes whoever cannot satisfy it, where an associated type
lets each implementor answer for itself. Naming is not requiring —
amendment C15, arrived at from a fourth direction.

**`#[async_trait]` is the same wall with a macro in front of it and an
allocation behind it**, which is worth writing down because it is the
shape everyone reaches for first and it predates the language feature.
Both halves were built rather than reasoned about: the plain form writes
`Pin<Box<dyn Future + Send>>` into the trait, so the `Rc`-holding store
fails at its own `impl` with the identical diagnostic, and
`#[async_trait(?Send)]` writes a plain box, so the erasure `Client` needs
stops compiling instead — `E0308`, the two box types. The choice is fixed
at the trait rather than per implementor, which is the objection; the
allocation is the surcharge, measured with a counting allocator over
1,000 calls to a store that answers immediately: **1,000 allocations
against 0**, because `MemoryStore` answers `Ready` and the macro boxes
unconditionally.

**`trait_variant` is the one alternative that carries this exact shape,
and finding that out took building the shape rather than the trait.**
rust-lang's own crate for the question writes a *second* trait whose
futures are `Send`, with a blanket impl making every `SendStore` a
`Store`. Against the three-sided constraint this workspace actually has —
a single-threaded store in a bare jar, a threaded store erased through
`ClientBuilder::cookie_jar`, and `CookieJar<AnyList, AnyCookieStore>`
still `Send + Sync` — **all three compile**, and it allocates nothing: 0
per 1,000 calls, level with the shipped shape and unlike `#[async_trait]`.
An earlier reading of it as a non-starter was wrong and was corrected by
the probe.

**What it costs is that the author's choice between the two names is
one-way**, and that is the discriminator rather than any of the
arithmetic. A store whose futures are genuinely `Send`, written against
the trait the seam is *named* after — the obvious choice — works in a
bare jar and is refused at the erasure with `E0277`; and its author
cannot add the second impl beside the first, because the macro's own
blanket impl conflicts, `E0119`. The repair is to delete the impl and
rewrite it against the other name. With associated futures there is one
trait, one impl, and the property is read off the concrete type: the same
`MemoryStore` source is a jar's store and a `Client`'s, and its author
wrote nothing about `Send` at all.

**Two measured costs.** `Client::execute`'s future went from 4,784 bytes
to **6,896** when the two cookie hooks became `async fn` — over
`tests/future_size.rs`'s 6 KiB ceiling, which is the guard doing exactly
what the cache's own boxing measurement predicted it would — and back to
**5,024** with `attach_cookies` and `store_cookies` boxed. Two more
allocations per hop, on a path already about to touch a store. And
`matching` hands back owned `Cookie`s where it lent them, `iter` is
`cookies()`, because a store on the far side of a socket has nothing to
lend.

**The suite needed a store that actually waits**, for the reason the cache
work already recorded: `MemoryStore` answers `Ready`, so a suite built on
it alone would pass for a seam that had never stopped being synchronous.
`SuspendingCookieStore` in `tests/pluggable_stores.rs` suspends once per
operation and counts it, and the test asserts both that the second request
carried the cookie and that the store suspended.

**And a `#[cfg]` was orphaned by the deletion, which is `lib.rs`'s defect
a second time.** Removing `use std::sync::Mutex;` left its
`#[cfg(feature = "cookies")]` behind, where it attached to the import on
the next line and made a build *without* `cookies` fail to find
`SystemTime`. `--all-features` cannot reach that configuration and did not
see it; `hclient-wasi`'s live suite builds `hclient` with its own feature
set and failed seven tests at once.

### The cache seam advertised three stores it could not serve, and a consumer is what found it

Asked what else was worth doing to the jar and the cache, and the answer
was not a feature. `CacheStore`'s own documentation opens *"because the
interesting stores are not in memory — a cache on disk, in Redis, or in
`moka::future`"*, and **not one of them could be written from outside this
crate.** Every field of a `StoredResponse` is readable through an
accessor, so a store could always write an entry *down*; `StoredResponse::
new` was `pub(crate)`, so it could never build one *back*. `Selector` was
worse and in two ways: its constructor was `pub(crate)` too, and the only
public reader was `names()` — the field names without their values, which
is half of a thing whose whole content is `(name, value)` pairs.

**It was found by writing the store, not by reading the code**, from a
scratch crate outside the workspace with a path dependency. That is this
file's own rule about consumers being a different instrument from tests,
and the instrument had already been aimed once: the identical hole in the
cookie seam was found the same day and closed with `Cookie::from_record`.
The cache's was recorded then as *"a real gap, the cache's, recorded here
rather than fixed as a side effect"* — which was the right call at that
moment and is what asking the question again cashed in.

**Both are public now, and the argument for opening them is the seam's
own safety claim rather than convenience.** RFC 9111 is applied *above*
the seam on whatever a store answers — `HttpCache::lookup` filters by
`Selector` and recomputes age and freshness against the request's
directives every time — so a store handing back a wrong entry loses a hit
or keeps rubbish, and cannot make a stale response answer a request that
forbade one. Checked rather than asserted: the filter and the
recomputation are both in `lookup`, read before the visibility changed.
The trust model does not move either, because a store already received
and returned `StoredResponse` values and could always have handed back
the wrong one.

**`Selector::from_fields` re-applies the sort and the dedup rather than
trusting them**, and that is the whole of what it adds over the tuples it
takes: equality is the `Vec`'s, so a selector rebuilt in whatever order a
file or a database row happened to list it would compare unequal to every
selector a request produces, and the entry would be unreachable — a cache
that silently stops hitting. A store cannot get that wrong because it
cannot express it. What is deliberately **not** re-applied is the
`Authorization` rule, and the direction is why: a selector fabricated
without the credential is unequal to every selector a request builds, so
it can only ever miss.

**The evidence is a store that holds nothing of this crate's.**
`ReloadingStore` in `tests/pluggable_stores.rs` writes every entry down to
plain data — a `u16`, strings, byte vectors — and rebuilds one on every
read, so a hit served out of it says the status, the version, the headers,
the body, both §4.2.3 timestamps *and* the selector all survived the
journey. It is `SuspendingCookieStore`'s counterpart and asks a different
question: that one whether the seam really waits, this one whether an
entry can leave the process and come back at all.

**Checked in the failing direction, and the pair is what makes the sort a
claim.** With the disk rows handed back **reversed** the test passes,
because `from_fields` sorts; with the rows reversed *and* the sort removed
it fails. So neither half is decoration. Two unit tests carry the same two
properties at the type — a round trip in any order, and a credential-less
selector that can only miss — because a property named at the type is
found by a reader where one buried in an integration test is not.
### One backend, both seams — measured before anybody writes the sqlite one

The plan is a single sqlite file holding the cache and the jar together,
and no persisting implementation in this workspace. So what was worth
checking now is not *can we persist* but **do the two seams admit one
backend at all** — a question that is free to answer today and expensive
to answer after somebody has written the store.

**They do, and it needed no wrapper.** One type implementing both
`CacheStore` and `CookieStore`, `Clone` over an `Arc` of shared state,
installed into one `Client` through `ClientBuilder::cookie_jar` **and**
`ClientBuilder::cache` — compiles and runs, with both halves reaching the
same handle. The associated types collide in name and not in fact, so
`AnyStore` and `AnyCookieStore` each pick the right ones with no
qualification at the call site. Nothing about the two seams had to change
to allow it, which is the answer the question was asked for.

**Two things did have to change, and neither was visible from inside.**

**A record is not a representation, and the first store outside the
workspace fell into it in five minutes.** `Cookie::to_record` answers
`None` for a session cookie — right for a file, wrong for a store, because
a `CookieStore` is not a mirror of the jar, it *is* where the jar keeps
its cookies for as long as the process runs. A store whose only
representation is `CookieRecord` drops every session cookie and reports
success: a plain `sid=abc` never reached the second request, and the seam
had done exactly what it was asked. Nothing in the signatures says the
round trip is lossy — `put` takes a `Cookie` — so it is said on the trait:
a persisting store holds what it *has* as `Cookie` and writes down the
strict subset that has a record.

**And a response asked the store once per `Set-Cookie` header.** Each
header went through `CookieJar::store`, and each of those read what the
jar already held so §5.7's replacement could keep the old cookie's `seq`
and `creation`. Measured against a store that logs its own statements:
five headers, **six reads and five writes**. `MemoryStore` answers
`Ready`, so through it the difference between one read and six does not
exist; a store on the far side of a file or a socket pays every one.

`store_response` splits `store` into `prepare` — every §5.7 rule, pure,
no store touched — and the insertion, so every refusal is settled before
the store is asked anything, and then one `get` covers every domain the
whole response scopes a cookie to. **Six reads to two**, writes unchanged
at five, because a write per cookie is what storing five cookies is.

The batch still sees itself, which is the half that had to be built rather
than saved: two `Set-Cookie` headers naming one cookie mean the second
replaces the first, so a cookie already placed by *this* response is
looked up in the batch before the snapshot — otherwise the second draws a
fresh `seq` and jumps §5.4's queue against the first.

**Kept by `a_response_full_of_set_cookie_headers_asks_the_store_once`**,
whose bound is *at most two reads* rather than today's number: what must
not come back is growth with the header count, and a test pinned to a
figure gets relaxed rather than read. Checked in the failing direction by
restoring the per-header loop and watching it fire.

**The instrument is the one this file already records.** None of the three
findings is reachable from a test written beside the code: the seams'
compatibility needs two traits on one type, the record's lossiness needs a
store that is not a decorator, and the read count needs a store that is
not `Ready`. All three came from a scratch crate outside the workspace,
and all three are kept by tests inside it — which is the division that
worked for the cache seam one section up and is now three for three.
### And then the four seams got one backend, which is a byte store

The section above asks whether the cookie and cache seams *admit* one
backend; they do, and so do all four. `hclient_core::kv::KeyValueStore`
is that backend's seam — **bytes and nothing else**: a namespace, a
string key, opaque values, eight operations and five associated future
types. `CookieStore`, `HstsStore`, `CacheStore` and `AltSvcStore` are
wrappers over it now, so a store on disk or in Redis is written **once**
rather than four times.

**Bytes rather than a type parameter, and the dependency graph decides
it rather than taste.** `altsvc::Entry` lives in `hclient-native`;
`Cookie`, `hsts::Entry` and `StoredResponse` live in `hclient`; and
`hclient-native` does not depend on `hclient` — the only edge is a
dev-dependency, itself marked as a cycle `cargo package` refuses. So no
crate can name all four value types, and a `KeyValueStore<V>` would mean
one instance per use in every case anyway. The serialisation lands in
the wrappers, which is where the knowledge of what a cookie *is* already
was.

**The clock is an associated type and not `SystemTime`**, which is what
keeps `hclient-core` at 16 crates. Naming a wall clock there would cost
every consumer `web-time` — the parent of `js-sys` and `wasm-bindgen` on
`wasm32-unknown-unknown`, measured at +6 crates for a wasm build with no
transport — and it is not needed, because **a store compares and never
reads**: every calendar arithmetic in this family happens in the rules
above the store. So `Instant` carries `Timer::Instant`'s own bounds and
each wrapper binds it to `web_time::SystemTime` at its own site, where
`no-std-wall-clock-in-the-client` still covers it.

**The seam grew three operations after the fact, each on a measured
need.** It shipped with `get`, `get_many`, `put`, `remove` and `clear`;
`scan`, `scan_many` and `remove_prefix` were added only once three of
the four wrappers had each bent around their absence — a cache cannot
know its `Vary` selectors before reading them, a jar holds many cookies
under one domain and evicts by comparing them, an `Alt-Svc` memory
forgets a whole class at once. Each is *enumerate what is under this
prefix*, and each stays a byte operation, because a prefix is a string.

What `scan` promises is deliberately weak — a snapshot of no particular
instant, in no particular order — because Redis' `SCAN` is a cursor that
may miss a key written during the walk and may repeat one. A wrapper may
choose an eviction victim with it and may **never** conclude from it
that something is absent.

**Two of the four hand-written `MemoryStore`s survive, and the rule that
separates them is what to carry forward.** `cookie::MemoryStore` and
`cache::MemoryStore` hold a capacity and an eviction policy the byte
seam deliberately has not got; `hsts`'s and `altsvc`'s held a map and
nothing else, so each had become a name whose only purpose was a
distinction the code no longer draws. **What survives is a store that
*decides* something the seam cannot; what goes is one that only *held*
what it now holds.**

**Three findings are worth more than the wrappers.**

`persist` left the `Alt-Svc` value and became the **namespace**. RFC
7838 §2.2 asks a client to forget, on a network change, every
advertisement that did not ask to survive one — over a map that is a
`retain` with a predicate, and over a byte store it cannot be, because
the seam does not know what a `persist` flag is and must not grow a
`retain` that takes one. Two namespaces make §2.2 one `clear`. It also
reads as what the RFC says: *the ones that did not ask to survive* is a
statement about a **set**, and this makes the set a thing that exists.

**A `dyn` that declares no auto traits removes `Send` rather than hiding
it**, met for the fifth time. The HSTS wrapper's futures were
`Pin<Box<dyn Future>>` under a comment claiming `Send` would be inferred
from the store; it is not, so `ClientBuilder::hsts` — which asks
`for<'a> S::Get<'a>: Send` — refused `Hsts::new()` although every future
underneath it was `Send`. The repair is `connect.rs`'s: stop erasing.
The wrappers' futures are named types projected with `pin-project-lite`,
and the property is read off the byte store. **What it cost to find is
the line to carry**: `cargo check -p hclient --features hsts` was green
over all of it, because the bound lives at the use site in another file.

And **`Clone` joined `Send` on the list of properties a type can claim
in a derive and fail to have where it is used.** Removing
`altsvc::MemoryStore` found that `hclient_core::kv::MemoryStore` had no
`Clone` impl at all, so every wrapper *declared* `Clone` and none was
clonable — and `Native` clones its `AltSvcCache` into its own routing
half, where both copies must see one memory. Nothing said so, because no
test named the property: the wrappers are constructed and used in place
everywhere the suite looks.

**What is deliberately not on this seam is a cache.** The rule that
admits the four is narrower than *state a crate keeps*: it is **state
whose absence changes an answer** — a cookie that does not arrive, a
policy that lets a request go out in clear text, an advertisement
forgotten, a hit that becomes a miss the server has to answer. Each is a
fact a server told this client. `hclient-tls-rustls`'s `(alpn,
early_data) -> Arc<rustls::ClientConfig>` is not: it holds a value the
client computed from its own settings, losing one costs a config clone
and changes no answer, and its value is not bytes. A memo on a pure
function is a `Mutex<HashMap>` and stays one.

### Two more seams, and the one that took a recorded argument down with it

`hclient::cache` and `hclient::cookie` had stores; the inventory said the
other two memories worth one did not. Both have one now, and the cost of
each is what the inventory predicted plus one thing it did not.

**`hclient_native::altsvc::AltSvcStore`** — `get`, `put`, `remove`,
`retain_persistent`, associated future types, `&self`. `AltSvcCache<S>`
is the RFC 7838 rules over it — §3's *a present field replaces
everything*, the `ma` comparison, §2.2's `persist`, the narrowing to *h3
at this origin's own authority* — applied to whatever a store answers, so
a wrong store forgets an advertisement or keeps a stale one, and a stale
one costs at most a QUIC attempt that falls back. `Native::alt_svc_store`
installs one, **erased** rather than a sixth type parameter, which is
`hclient::Client`'s argument one crate up.

**The expiry had to become a calendar time, and that overturned an
argument this file records.** `Entry::expires_at` was elapsed time on the
transport's own `Timer`, defended in the module doc on the grounds that
*a wall-clock read would disagree with a caller testing under
`tokio::time::pause()`*. Measured before overturning: **`tokio::time::
pause()` appears in five doc comments in this workspace and in zero
tests**, and `tests/svcb.rs` records having *rejected* it. So the
sentence protecting the type was a claim with no check behind it — this
file's own recurring defect, met from the direction where the claim was
guarding a design rather than describing one. What the change costs is
real and is not that sentence: `hclient-native` reads a wall clock now,
where `Timer` was its only clock. What it buys is an entry that means
something to a store outside the process, without which the seam names a
use it cannot serve.

**One behaviour changed with the type, and a test caught it.**
`saturating_add` answered an absurd `ma` with `Duration::MAX`;
`SystemTime` has no `MAX` to saturate towards, so the first version
answered with *now* — an immediate expiry, which silently loses the
advertisement. `MAX_LEASE` is the repair, 400 days, which is
`hclient::cookie::MAX_EXPIRY`'s figure and its argument verbatim: the sum
is never computed, so the hazard has no end to be at.

**`hclient_tls_rustls::Rustls::with_session_store` and
`with_quic_session_store`** — rustls owns this seam, `ClientSessionStore`
is its trait, and what was missing was a way to reach it without building
a whole `ClientConfig`, which every convenience constructor spares you.
**They are two setters and not one**, because `quic.rs` already argued
the stores must be separate: `ClientSessionStore` keys by `ServerName`
alone while a ticket issued over QUIC also carries `quic_params`, so one
store serving both paths would hold two kinds of ticket in one slot.
Handing the same store to both setters puts that back, which is a
caller's decision rather than one made for them. Installing either
redraws `TlsConfigId`, because it must: that id is a component of the
pool key and says *which client may resume whose sessions*, which is
exactly what changed.

**Nothing here persists anything, and that is the shape rather than a
gap.** Neither Chromium nor Firefox persists TLS tickets — Chromium's
`SSLClientSessionCache` is in memory and flushed on memory pressure,
read from its own header — and this crate's default is the same. A
resumption ticket is a credential and 0-RTT data is replayable, so the
seam exists to let a caller make that decision, not to make it for them.

**Every seam is asserted where it is reachable, and finding out where
that was cost the sharpest defect of the day.** The alt-svc store is
asserted twice — the rules running over a substituted store, and
`Native::alt_svc_store` reaching the field `network_changed` reads, which
fires when the installer is made a no-op. The TLS halves are split: the
TCP store is consulted by `rustls::ClientConnection` through a private
`config_for`, so that assertion is a unit test in `lib.rs`; the QUIC
store is fetched by `quic_config_for`, so `quic.rs` pins that the setter
reaches the field it reads. Both fire when their setter is made a no-op.

**The defect is that the TCP test did not exist for twenty minutes while
appearing to.** Appended by a script that inserted before the file's last
`}`, it landed inside a `#[cfg(not(feature = "webpki-roots"))] impl` — a
`#[test] fn` in an `impl` is an associated function, never collected, and
under `--all-features` not even compiled. It compiled, it looked right,
and `cargo nextest` ran 38 tests where it should have run 39. What caught
it was checking the failing direction and reading **zero** where a
failure was due. A test that cannot fail is the thing this file is about,
and the way to find one is still to break its subject on purpose.

**And an orphaned `#[cfg]` for the third time in one week**, the second
of them caused rather than found: inserting `alt_svc_store` above
`network_changed` took that method's `#[cfg(feature = "http3")]` with it,
and `--no-default-features` stopped compiling. `just test-no-default`
caught it, which is twice in two days for the recipe this file records as
having once printed `error:` and exited zero.
### HSTS, the one memory that changes where a request goes

`hclient::hsts` — RFC 6797, behind an `hsts` feature, off by default.
[`Hsts`] over an [`HstsStore`] is the same seam shape as the jar and the
cache, and `ClientBuilder::hsts` installs it erased. It was the last row
of the browser-state inventory and the one flagged there as *the single
line where absence changes not the speed but **where the request goes***.

**It is in `Client` rather than in a transport, which is the opposite
answer from `Alt-Svc` one crate over, and the RFC draws the line
itself.** §8.3 governs what a UA does *"whenever [it] prepares to 'load'
… any 'http' URI (including when following HTTP redirects)"* — the
scheme is settled before any transport is asked, it decides which port
and which handshake happen at all, and a `Location: http://…` is
`Client`'s to resolve. It is equally not in `hclient-proto`, where the
redirect *mechanism* lives: this needs a calendar clock and a store, and
that crate is the sans-io leaf whose dependency count is guarded.

**There is no capability, and that is the decision rather than an
omission.** `owns_cookie_jar` and `owns_cache` exist because a browser
does both internally and doing them *twice* is harmful — two `Cookie`
headers, two stored copies. A browser applies HSTS too, and a second
upgrade is not harmful, because both answers are the same answer:
`https://`. So a capability here would be a gate with nothing to refuse,
which is the *distinction with one reachable side* this workspace deletes
rather than adds. The condition for one arriving is written where the
next reader will look: a transport that an upgrade above it would make
**wrong**, at which point the capability, the check and the refusal come
together — `owns_cookie_jar`'s own rule for its third state.

**§8.3's port rule is the one implementations get wrong, and it is
counter-intuitive on purpose.** No explicit port stays portless; an
explicit `:80` becomes `:443`; **any other explicit port is preserved**,
so `http://a.test:8080/` upgrades to `https://a.test:8080/` and not to
`:443`. The RFC's own NOTE says why — *"these steps ensure that the HSTS
Policy applies to HTTP over any TCP port of an HSTS Host"* — and its next
NOTE warns that such a request is *"reasonably likely"* to fail because a
plain HTTP server is listening there. Failing to reach it is the intended
outcome; reaching it in clear text is not.

**The matching rule is the opposite of the jar's, and a reader carrying
the cookie intuition will get it backwards.** §8.3 step 5: *"if … any
superdomain match with an asserted includeSubDomains directive is found,
or, if no superdomain matches … are found and a congruent match is
found"*. So **any** covering entry upgrades and a more specific entry
cannot veto a less specific one — where RFC 6265 gives the more specific
`Domain` the say. `example.com` with `includeSubDomains` covers
`a.b.example.com` even beside a `b.example.com` entry without the flag.
That was verified against the text rather than taken from a summary, and
then against the errata, because a research pass had twice flagged it as
the most counter-intuitive rule in the document: five errata reported for
RFC 6797, **all rejected**, none touching §8.2 or §8.3.

The enumeration that makes it a store's exact lookup is the jar's
`candidate_domains` move a second time, and for the identical reason:
§8.2 defines matching as a *relation*, so a store asked *what covers this
host* would have to implement §8.2 — which is the thing the seam exists
to keep above it.

**What it is narrower than the RFC in, said where a reader meets it:**
§8.4's *"MUST terminate the connection if there are any errors … with the
underlying secure transport"* is not enforced, because that is a rule
about a handshake and this crate conducts none — `TlsConnect` is the seam
that could. Upgrading the scheme and then accepting a bad certificate
honours half of HSTS, and the half honoured is the half that moves the
bytes onto TLS at all. There is also no preload list: that is a *policy*
— who is on it, how it is updated — and `HstsStore` is exactly where one
would go, needing nothing here to change.

**Seventeen mutations, fifteen killed on the first pass, and the two
survivors were both my tests being true for the wrong reason.** Neither
was a control.

`max-age=0` took **three** attempts to pin, and each wrong version is a
different way to be accidentally green. §6.1.1 makes it a deletion; with
the branch disabled it instead *stores* an entry expiring at `now + 0`.
Asserting *the next request is not upgraded* passes, because expiry
declines it too. Asserting *the store is empty afterwards* also passes,
because `upgrade`'s own §8.1.1 eviction sweeps the entry on the way. Only
reading the store **before** `upgrade` separates deletion from
already-expired — and in memory the two look alike, where in a store that
outlives the process one is a row that never expires out.

The IP-literal test was the same shape one rule over: §8.1.1 forbids
*noting* a literal, and I asserted it through `upgrade`, which refuses
literals on its own account under §8.3 step 3. Green for a client that
noted every literal it ever met. It reads the store now.

**And one guard here could not have seen its own subject.**
`graph-no-cookie-jar` proves a default build carries no jar and no cache
by looking for `public-suffix` and `jiff`. `hsts` costs **no crate at
all** — `winnow` is already in a default build through `hclient-proto`'s
response-head parser, `web-time` through the client's clock — so the two
dependency graphs are byte-identical and no `cargo tree` pattern can
discriminate them. That is a good property of the feature and a blind
spot in the guard. `graph-default-has-no-hsts` asserts the resolved
**feature set** instead, which `-f "{p} {f}"` puts on the package's own
line, and it carries a `present` half so that renaming the feature fails
the check rather than silently emptying it. Both halves were checked in
the failing direction: putting `hsts` in `default` fires the first,
staling the pattern fires the second.

### The jar and the cache became modules, and one feature shape is what made it free

`hclient-cookie` and `hclient-cache` are `hclient::cookie` and
`hclient::cache`. **No consumer's `use` line moved**: both were already
re-exported under exactly those names (`pub use hclient_cookie as
cookie`), so the change is `pub use` to `pub mod` and nothing else on the
public surface.

**What made it defensible is a measurement, not a preference.** This
workspace's test for a crate boundary is whether it holds a dependency a
feature would otherwise spread — `hclient-tungstenite` carries
`tungstenite` and is kept for that. (`hclient-tls-quic` stood beside it
in this sentence and no longer does: it folded in, became a feature, and
the feature lost its subject when the QUIC seam stopped carrying
`quinn-proto`.) `cargo tree -i` named **`hclient` and nothing else** for
each of these two, and their dependencies (`jiff`, `winnow`,
`public-suffix`) are gated just as well by the `cookies` and `cache`
features from inside. The boundary was being kept for a third-party
consumer who did not exist.

**What it cost is one sentence in `docs/competitive-gaps.md` that is now
false and has been corrected**: the jar and the cache are still sans-io
and still clockless, and are no longer *separately usable*. That is the
whole of the loss, and it is worth stating plainly because the crates'
own module docs had argued the boundary made "cookies behave the same on
every backend" **structural** — it does not any more, it is a discipline,
and both module docs now say so in as many words.

**The part that nearly went wrong is a feature, and it is the reason to
read this section before touching the `cookies` feature.**
`hclient-cookie` carried `default = ["public-suffix"]`, and
`tests/without_the_list.rs` — the only thing asserting a no-list build is
*narrower* than a list build rather than quietly wider — ran only under
`-p hclient-cookie --no-default-features`. The obvious spelling of the
merge is `cookies = [.., "public-suffix"]`, and it makes that test
**unreachable**: features are additive, so *the module without the list*
stops being expressible. Measured before it was believed — the old
invocation ran 78 tests, the merged one compiled the file out entirely.

`justfile` had already recorded that deleting that line "would have been
the other direction", so the resolution is a shape rather than a
deletion: **`cookies` pulls the `public-suffix` crate, and a separate
flag of the same name — carried in `default` — gates the code path.** A
plain `--features cookies` behaves exactly as it did; `--no-default-
features --features cookies,test-util` is the no-list build, and it is in
`test-no-default`. The one thing that changed is that the no-list build
still links `public-suffix` as dead code; the test asserts behaviour, not
graph size, and `graph-no-cookie-jar` still pins the crate out of a
default build.

That guard was **weaker than it was** and the weakening was worth naming:
it looked for `hclient-cookie` and `public-suffix`, and there is no crate
name left to look for, so a jar compiled into a default build would show
up there only through its list — and a *cache* compiled in would not show
up at all, which the paragraph did not notice.

**`jiff` is what repaired it**, found by asking the graph rather than by
reading: it is absent from a default build and present with `cookies` or
with `cache`, because both date parsers delegate the calendar to it. So
one pattern covers both modules and covers the jar itself rather than its
list. Checked in all three directions — silent on the default build, and
firing on each feature alone. It also cost the same defect this file
records twice: the backticks in the new message were eaten by the shell
inside the recipe (`sh: 1: cache: Permission denied`), which is a thing to
remember before putting one in a `just` string.

**Two smaller things the move surfaced, both caught by `just docs`
rather than by the compiler.** Doc links in the moved files pointed at
their old crate root, which is a different crate root now; and a `///`
doc on the `pub mod` declaration makes the module's own `//!` links
resolve in the **parent's** scope, so thirteen of them stopped resolving
until that outer comment went. Neither is a compile error, and neither
would have been caught by the test suite.

Counts, measured: 25 publishable crates to 23, `hclient`'s own suite 308
tests to 465, and the workspace unchanged at 1755 — everything moved,
nothing was lost.
