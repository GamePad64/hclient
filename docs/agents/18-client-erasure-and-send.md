# Client erasure and Send

> Part of the [AGENTS.md](../../AGENTS.md) record, split 2026-10-05. The root
> file keeps the operational rules and the index of parts. Sections below
> keep their original headings, and a "this file" in the text named the
> whole record when it was written.

### `hclient::Client` names no type parameters, and the browser decided what that costs

`Client` is one concrete type. `Clone` is an `Arc` bump, and a library takes
`&Client` where it used to write five `where` lines — measured on this
workspace's own consumer, `examples/portable.rs`, whose `fetch` went from
`fetch<T, S>` with four transport bounds to `fetch<S>` with none. A generic
function has to restate its callee's where-clause, and that is the tax
erasure removes.

**Two recorded blockers were cleared by not asking the question.**
`docs/competitive-gaps.md` §G13 said `Transport`'s RPITIT needs return type
notation (`E0658` on 1.98, true) and that `Timer::Instant: Copy` is
permanent (also true). Both are irrelevant: the boxed future declares no
`Send`, so there is nothing to prove and `DynTransport` gets a **blanket
impl** over every `Transport` — a backend author writes nothing — and
`DynInstant` answers *how long ago was this*, so the instant never
crosses the boundary and `Copy` is asked of nothing.

**The first attempt at this was abandoned, and the reason it was abandoned
is the reason this one works.** That version put `Send` on the boxed future
so a request could be spawned, and following the bound down needs it on
seven seam methods — at which point `hclient-rt-embassy`'s `connect` future,
which holds `RefCell<embassy_net::Inner>` because embassy's executor is
single-threaded, is excluded. Dropping the bound removes the whole chain.

**What it costs is three things, and only one of them was in the plan.**

**The embedded target has no `Client`.** The plan asserted *"`Embassy` is
refused there, and already was"* — wrong, and the distinction is the lesson:
being `!Send` and being *refused* are different, and the generic `Client`
built over Embassy perfectly well, it merely could not cross a thread. An
erased `Client` boxes its transport `Send + Sync`, and `RefCell<embassy_net::
Inner>` is not `Sync`. The nine live TAP scenarios are written against
`Native`/`Transport` directly now, ~20 lines of helper, and the CI job is
unchanged and green; what it no longer covers is the target-independent
`Client` layer, exercised by the rest of the suite on every other backend.

**Nothing a request produces is `Send`, and the browser is why.** One
`ClientBody` serves every backend, and `hclient-fetch`'s body holds a `dyn
Stream` with no auto trait — so `Send` on the erased body does not weaken
the browser backend, it **excludes** it: `Client::builder(Fetch::new())`
stops compiling. Measured, and only after the `Send` version had been
written and the entire native suite made green under it, because **`cargo
nextest run --workspace` does not build for `wasm32-unknown-unknown`** —
this file records the same blind spot hiding a broken browser suite for six
merges once before. The cheap check is `cargo test -p hclient-fetch --target
wasm32-unknown-unknown --no-run`. So `tokio::spawn` of a response body is
gone, which worked on `hclient-native`; a caller who needs it reaches past
the facade with `Client::transport_as::<Native<..>>()`. The request future
was cheaper to lose, because `Native`'s was *already* `!Send` — and that
half has since been fixed at its cause, so the sentence now cuts the
other way: see the section below. **`Client` itself stays `Send +
Sync`**, which is the half that has to.

**True, and not for the reason it reads as — measured later, and the
difference is what makes it fixable.** *The browser* is not a category
that is `!Send`. `wasm32-unknown-unknown` without atomics is one thread,
and wasm-bindgen marks its handles accordingly: asked directly on that
target, `JsValue`, `js_sys::Promise` and
`web_sys::ReadableStreamDefaultReader` are all **`Send`**, and so is
`Fetch::execute`'s own future. `hclient-wasi` is `Send` **throughout** —
transport, future and body — so it was never part of this question at
all. What is `!Send` is exactly one third-party type: `js_sys::JsFuture`
holds `Rc<RefCell<Inner<T>>>` for its two promise callbacks (0.3.104,
still the latest), and it arrives in the body through
`wasm_streams::readable::IntoStream`. `NativeBody` is `Send` as well, so
the erased `ClientBody` is **one crate's read loop** away from being
`Send` on every backend here.

**And the adapter that closes it already exists in this crate**, which
the paragraph above got wrong within a day of being written.
`promise::SendJsFuture` is exactly the `Arc<Mutex<..>>`-where-`JsFuture`-
has-`Rc<RefCell<..>>` shape, with `SingleThreaded<T>` carrying the one
`unsafe impl Send` this workspace allows (amendment C7, and the only
`unsafe` in the crate). `Timer::sleep` and the WebSocket are built on it.
The body is not: `from_response` reaches for
`wasm_streams::ReadableStream::into_stream()`, and `IntoStream` holds a
`js_sys::JsFuture`. So the remaining work is **using the adapter in one
more place**, not writing one — a hand-rolled loop over
`ReadableStreamDefaultReader::read()`, whose cost is the reader
lifecycle and cancel-on-drop that `wasm_streams` is doing today.

**The actor is what landed, and it is the more expensive of the two on
purpose.** `hclient_fetch::Body` is `Send` now: `body::pump` owns the
`IntoStream` on the thread that made it and hands `Bytes` — already
`Send` — over a `futures_channel::mpsc` of capacity zero, so no JS handle
crosses to the caller's side and nothing about the property depends on
how many threads there are. The adapter would have been fewer lines and
would have died under `+atomics`, which is the configuration this was
chosen against.

Three things it cost, each answered rather than waived. **A spawn**,
which this workspace refuses everywhere else — the refusal is about work
continuing behind a caller who walked away, and the pump is bounded one
chunk ahead by the channel and ends when the `Body` drops. **One crate**,
`futures-channel` (33 to 34; `wasm-bindgen-futures` was already there
through `wasm-streams`), bought rather than written because a hand-rolled
single-slot channel is waker code and its defects are silent hangs.
**And cancellation, which had to be built rather than inherited**: a drop
used to reach `IntoStream` synchronously and fire `wasm-streams`'
`cancel_on_drop`, where now it reaches the pump. Closing the channel is
noticed at the send and is enough for a body that is producing; a pump
parked on a `read()` a quiet peer will never answer never gets there. So
the `Body` also holds a `oneshot::Sender` it never sends on, selected
against every read. The pair of tests is the assertion — a pump watching
only the channel passes
`dropping_a_pending_body_cancels_the_underlying_reader` and fails
`dropping_a_body_whose_read_will_never_answer_still_cancels`, verified by
applying exactly that mutation.

**And the declaration it was for has followed.**
`erased::{BoxBody, BoxSleep, BoxInstant}` carry `Send` now — amendment
C14 — so a `Response<ClientBody>` from the erased `Client` crosses a
thread again, which it stopped doing when `Client` gave up its type
parameter. `crates/hclient/tests/spawnable_body.rs` collects one on
another thread.

**The request future is deliberately still `!Send`.** `BoxExchange` is
unbounded, so `Transport::execute` is untouched and
`hclient-rt-embassy`'s `RefCell`-holding `connect` future is not
excluded — the bound that abandoned the first erasure attempt is exactly
the one not taken here. What crosses a thread is what a request
*produced*, not the act of making it; a caller who needs the second
still reaches past the facade.

**Checked where it would have hurt most.** `hclient` with
`default-transport` builds for `wasm32-unknown-unknown` under
`-Ctarget-feature=+atomics` — the browser keeps `Client` under wasm
threads. That took one more repair of the same kind: `BrowserClock::Sleep`
was `Discard<SendJsFuture>`, whose `Send` is the `unsafe impl` the
atomics `cfg` strips, so it is `timer::Elapsed` now — a
`oneshot::Receiver<()>` the spawned waiter fires, holding no JS and
claiming nothing about threads. The timer still starts when `sleep` is
called, because the promise is built before the spawn.

`fetch-must-fail-under-atomics` still rejects, which is the check working
rather than a leftover: `SendJsFuture` is what the WebSocket and the
sleep's own waiter still run on, and its `Send` must still disappear
under threads.

**A sweep for what is still `!Send` found one more of the same, and it
was a feature away rather than a target away.** `http2::On1xx` was
`&'a dyn Fn(StatusCode, &HeaderMap)`, held across an await, and its doc
said the callback "neither outlives this call nor crosses a thread" —
true, and it was the erasure rather than the callback that settled the
second half. So every build with `http2` on had a `!Send` future,
including one whose hook is an ordinary `Send` type. It is a type
parameter now, and the property is inferred: an `Rc`-holding hook still
yields a `!Send` future and nothing else does.

That one had no gate, because the workspace run is `--all-features`,
where `http3` switches `tests/send_future.rs` off. `just test-no-default`
runs this crate's suite under `--features http2`, which is where it is
checked now — 282 tests.

**What is left, re-measured from outside the workspace — and this table
has now gone stale twice, the second time within hours of being
corrected.** The first correction fixed three of four rows that C15 and
C16 had quietly made true. The DoH row survived it, said `!Send`, and was
fixed by `8eeb9e6` **the same day** — after which the row went on saying
`!Send` for another week, and was quoted back into a design discussion as
though it were a fact. A claim is as perishable as the thing it
describes, and this file's own example of the rule is this paragraph.

| what | measured | |
|---|---|---|
| `Client`, its request future, `Response`, `ClientBody`, `Collected`, `Error` | **all `Send`** | asserted from a scratch crate depending on `hclient` by path |
| `Native::execute`, plain | **`Send`** | |
| `Native::execute` **with the `http3` arm installed** | **`Send`** | the bounds live on the opt-in `Native::http3`, amendment C15 — the row that used to say the blanket impl could not prove them |
| `hclient-tower`'s `Service::call` future | **`Send`** | its `type Future` has declared `Send` since C16; the row saying it needs return type notation outlived the fix |
| a transport whose resolver is `hclient-dns-doh::Doh` | **`Send`** | since `8eeb9e6`: `execute_send` names its future, so the streams are declared `Send` — and `tests/send.rs` asserts it |
| `hclient-fetch` | `!Send` under `+atomics` by nature | a JS `WebSocket` belongs to the realm that made it |

**So nothing is left, and the paragraph that stood here proposed a
repair that had already been made.** It read: DoH resolves through a
generic `C: Transport` whose `execute` is an RPITIT, so its streams
cannot be declared `Send`; `SendTransport` would fix it at the cost of
narrowing the bound; *measured, not done*. Every word was true when
written and none of it after `8eeb9e6`, which made exactly that change —
`C: SendTransport`, `execute_send`'s named `BoxSendExchange`, and two
`Send` stream aliases.

**The cost of leaving it was not the sentence, it was the decision it
shaped.** Asked which seams this workspace still owes, this row was read
off the table and reported as an open item — twice, in the same
conversation, before anyone opened `tests/send.rs` and found its first
line saying *this **was** the last `!Send` in the workspace*. A table is
read where a test is not, which is an argument for the table naming the
test rather than restating it.

Re-measured from a scratch crate outside the workspace: `Client` and its
request future, `Response`, `ClientBody`, `Collected`, `Error`,
`Native::execute` with and without the h3 arm, `hclient-tower`'s
`Service::call`, and `Doh`'s `lookup` streams and the
resolver itself — **all `Send`**. The only remaining row is
`hclient-fetch` under `+atomics`, which is not a defect but a fact about
a JS realm.

Everything else that grep finds is a trait object whose trait already
declares `Send` (quinn's `AsyncTimer`, `AsyncUdpSocket`, rustls'
`ClientSessionStore`) or a `dyn Any`/`dyn Error` that never crosses an
await.

**The fourth row is closed, and it cost no `unsafe` and no actor.**
`FetchWebSocket` is `Send`: the state cell is `Arc<Mutex<..>>` like
`promise::State` beside it, and the three `Closure`s ride
`promise::SingleThreaded`, which already carries this crate's one
`unsafe impl Send`. Its own module doc had read *"`Rc<RefCell<..>>`, not
`Arc<Mutex<..>>` — so no `unsafe`"* for a vertical, and the second half
did not follow from the first: `Arc<Mutex<..>>` needs no `unsafe` either,
and the wrapper the closures needed was already written.

**Giving the closures a `Send` inner `dyn` was tried first and is
impossible**, which is worth knowing before someone tries it again:
`WasmClosure` is implemented for `dyn FnMut(..) -> R + 'a` and no other
shape (wasm-bindgen 0.2.126, `convert/closures.rs`), so
`Closure<dyn FnMut() + Send>` is a type that exists, satisfies `Send` by
auto-derivation, and cannot be constructed. That is also the reason
`promise.rs`'s `unsafe` cannot be deleted.

**And the actor was refused here, having been chosen one module over.**
The difference is what each buys. `body::pump` feeds `ClientBody`, an
erased type shared by every backend, which must be `Send` for everyone or
for no one — so paying a spawn there bought the property for the whole
facade. `WebSocketConnect`/`WebSocket` declare no `Send` at all and
nothing erases them, so an actor here would buy a property nothing reads
— and it would cost a real one, because `Sink::start_send`'s refusal and
`poll_close` are **synchronous** today and a channel would make both
asynchronous. So this is `Send` exactly as far as `JsValue` is, and it
disappears under `+atomics`, which is honest: a JS `WebSocket` belongs to
the realm that made it.

**Three of those four are one blocker, and it was taken all the way to a
working build before being reverted.** They are not four walls: they are
the same wall, that a `dyn` declaring `Send` obliges whoever boxes into it
to *prove* it, and proving it for a generic parameter means naming an
RPITIT future. Return type notation is the language feature for naming
one, so the whole thing was built on nightly under `--cfg rtn_probe` to
see what it actually costs.

**It works.** `T: Transport<execute(..): Send> + Sync` on
`DynTransport`'s blanket impl, `Send` on `BoxExchange`, the same
treatment for `http3::arm`'s three boxes with
`StagedConnect<connect(..): Send, exchange(..): Send>` and its bounds on
the opt-in `Native::http3`, and `Send` on `hclient-tower`'s `type Future`
— the whole workspace compiles, and
`assert_send(client.get(u).send())` passes. **`Client::execute`'s future
is `Send` under RTN**, which is the property this file has recorded as
lost since erasure. Neither `hclient-dns-doh` nor `hclient-rt-embassy` is
touched: nothing moves to a seam, so nothing has to be satisfied by a
backend that cannot.

**And then the bill arrives somewhere else, which is the finding.** Two
consumers in this workspace stop compiling, and both are the same shape:

- `crates/hclient/tests/two_runtimes.rs` is generic — `fetch_once<R>` over
  `R: TcpConnect + Timer + Blocking`. Under RTN, `Client::builder(t)`
  demands `execute(..): Send` of a *type parameter*, so the caller must
  restate the whole chain: `TcpConnect<connect(..): Send>`,
  `Blocking<run(..): Send>`, `Resolve<lookup(..): Send>`. That is
  **the seven-seam cascade this file already records — moved out of the
  seam and into every generic consumer**, which is the exact tax erasure
  was introduced to remove.
- `crates/hclient-tower/tests/round_trip.rs` returns `impl Transport`, and
  an opaque type does **not** leak an RPITIT bound, so it has to be
  restated there too — and **it cannot be**, because an opaque type has no
  name to hang a bound on. Re-measured on 2026-08-28 with a minimal
  two-crate reproduction: a `fn make() -> impl Seam` gives its caller a
  clean `E0277` and no way to say what would fix it; only the *producer*
  writing `-> impl Seam<go(..): Send>` does, and that is a foreign crate's
  signature. **RTN does not travel through `impl Trait`**, which is the
  durable statement.

  The sentence here used to say that restating it *ICEs*, with the
  `DefId(.. Transport::execute::{anon_assoc#0}) does not have a "type_of"`
  from 1.100.0-nightly (f7d782a3b, 2026-08-19). That was seen, in this
  workspace's real code, and it does **not** reproduce minimally on the
  same nightly — so it is one manifestation rather than the rule, and the
  rule above is what a reader should act on. A crash observed once is
  weaker evidence than a limitation reproduced on demand.

So the answer to *can we fix all of it* is: **yes for a concrete
transport, and the generic case pays what the seam would have paid.**
Everything above was reverted; what is kept is the measurement, because
the next person to ask will otherwise re-derive it. `hclient-tower`'s own
module doc says the fix is one bound when #109417 lands — true of that
crate, and this is the rest of the bill.

### A sweep for stale limitations, and what separates the two kinds

Two documented "cannot"s turned out to be false in one week —
`hclient-tls-native-tls`'s ALPN and `hclient-fetch`'s `!Send` body — and
both had **named their own cause correctly** while nobody acted on it. So
the rest were swept for the same shape. Six more were false and are
fixed; four were checked and stand.

**What was false**, all of it about `Send` and all of it made obsolete by
the associated-future work rather than by upstream drift:
`hclient-core`'s `erased` module doc (*nothing boxed here declares
`Send`*, above four aliases that now do, and *a backend author writes
nothing*, which is one method now); `BoxBody`'s own doc (*Not `Send`*,
directly above the line declaring it); `hclient`'s crate doc (*what a
request produces is not `Send`*); `hclient-mock`'s (*`Client::execute`'s
future is `!Send` whatever is underneath it*); `hclient-tower`'s (*today
that cannot be fixed here*, plus *when #109417 lands, the fix is one
bound*); and `hclient-native`'s long block on why the QUIC arm could not
be `Send`.

**One of them was hiding an unrun test.** `hclient-native`'s
`tests/send_future.rs` was gated `not(feature = "http3")`, which was
honest when the arm was `!Send` — and left the property out of the
workspace's own `--all-features` run once it stopped being. The gate is
gone and the file's positive doctest fence is back, having been removed
for the same reason.

**What was checked and stands**, recorded so the next sweep does not
re-derive it: `native-tls` really reports no protocol version and no
cipher suite — its `Protocol` type is a setter pair, not a getter;
`h2::client::Connection` really reports no traffic, so an h2 keep-alive
cannot measure silence the way the WebSocket one does; every third-party
version a doc comment cites is the version in the graph; and `h3` 0.0.8's
client really has no `enable_webtransport` — though that line now says
which setting it means, because the crate *does* announce
`enable_extended_connect` and `enable_datagram` three lines away and the
compressed phrasing read as announcing nothing.

**The rule the sweep produced.** A claim about a third party goes stale
in two ways, and only one of them is upstream's doing. Version drift is
the obvious one and was absent here. The other is a claim about a
**wrapper** — *this crate does not expose X* — where the layer beneath
does, and which stays true of the wrapper for ever while the conclusion
drawn from it quietly stops being. Both of this week's findings were that
shape, and `negotiated_alpn` is the sharpest: the doc named the wrapper as
the cause, correctly, in the same paragraph that called the limitation
concrete.

### `Client`'s request future is `Send`, and RTN was never needed

**`tokio::spawn(client.get(u).send())` compiles.** The route is not the
one above and does not wait on anything: the four seams whose futures
`Native::execute` awaits — `Blocking`, `TcpConnect`, `TlsConnect`,
`Resolve` — carry **associated future types** instead of RPITITs, so a
consumer can *name* them, and `SendTransport` (amendment C16) is a
separate trait whose impl carries the bounds `Transport` does not.

**Naming is not requiring, and that is the whole of why this works where
`Resolve → BoxStream` did not.** A fixed `Send` box in a seam excludes
whoever cannot satisfy it; an associated type lets each implementor
answer for itself. Measured in-tree, three ways:

- `Tokio` and `Smol` box `Connecting` `Send`; `hclient-rt-embassy` boxes
  it plain, because `embassy_net::Stack` is `&'d RefCell<Inner>`. Both
  are `TcpConnect`s.
- `hclient-dns-doh` boxes its streams plain, because it resolves through
  a generic `C: Transport` whose `execute` is an RPITIT. It is a
  `Resolve` like any other; what it loses is one layer up.
- `hclient-tls-native-tls` boxes its handshake plain, because
  `async_native_tls::TlsConnector::connect` is a `pub async fn` and its
  future has no name.

**One seam needed a named type rather than a box, and the reason
generalises.** `TlsConnect::connect` is generic over `S`, so its future is
`Send` exactly when `S` is — a box would have to pick one answer for
every `S`, and both answers are wrong (`+ Send` excludes embassy's IO,
which would take embassy out of `Native` entirely; without it every
handshake is `!Send` for everybody). `hclient-tls-rustls` writes
`Handshaking<S>`, two states and a poll loop, and the answer is derived.
The sync preparation moved into `connect`, which is what made that a
dozen lines instead of a state machine.

**What a generic consumer pays is real and, unlike under RTN, payable.**
`two_runtimes.rs` and `reaper.rs` restate `for<'a> R::Connecting<'a>:
Send` and its neighbours — ordinary bounds, three lines each. The same
restatement expressed as return type notation is unstable and, across a
crate boundary, ICEs.

**What it costs at runtime** is one allocation per connect, resolve,
handshake and blocking call. `hclient-tls-rustls`'s handshake allocates
none: it is a named type, not a box.

**And what it costs a backend** is one method, whose body at a concrete
type is `Box::pin(self.execute(req))` — `Send` inferred, not proved.
Proof is only ever owed by generic code, which is the asymmetry the whole
design rests on.

**Three backends owed that method and did not have it for a day**, and
the workspace was green over all three: `hclient-fetch`, `hclient-wasi`
and `hclient-urlsession` build for targets `cargo nextest run --workspace`
does not — `wasm32-unknown-unknown`, `wasm32-wasip2` and Apple — so
`Client::builder(Fetch::new())` and its two siblings stopped compiling and
nothing said so. This file already records that blind spot twice; this is
the third, and the cheap checks that catch it are the ones already
written: `wasm-pack test --headless --firefox`, `cargo check -p
hclient-wasi --target wasm32-wasip2 --all-targets`, and `cargo check -p
hclient-urlsession --target aarch64-apple-darwin --all-targets`, which is
clean on a Linux host.

**The two are not interchangeable, and which one is right depends on a
configuration nobody here builds yet.** The adapter's `Send` is a claim
about there being one thread, so it is stripped under `+atomics` by the
same `cfg` that strips wasm-bindgen's own — under wasm threads it cannot
help, and nothing holding a `JsValue` can be `Send` there by any means.
The actor can: it keeps every JS handle on the thread that owns it and
hands `Bytes` — already `Send` — across a channel, so the type crossing
the boundary holds no JS at all. So the adapter is the cheap answer for
the single-threaded target this workspace ships for today, and the actor
is the only answer that survives wasm threads. Worth knowing before the
cheap one is taken as the answer to both.


**A `!Send` hook cannot be watched through `Client`.** `hclient-fetch`'s P13
test — *a single-threaded runtime can watch* — ran through `Client` with a
hook holding an `Rc`. It asserts the same property at the `Transport` layer
now, which is where the property actually lives; what is lost is watching a
`!Send`-hooked browser transport *through the facade*, so cookies, redirects
and the cache are unavailable to a caller who wants that.

**A `#[cfg]` making `BoxBody` `Send` off-wasm is refused**, though it would
give native callers their spawnable bodies back and would not be a
capability that lies — `Send` is a claim about threads and the wasm targets
have none. It is refused because a cfg-alias hides the symptom rather than
removing the cause, and because a `ClientBody` whose auto traits depend on
the target is a thing a portable library cannot reason about.

**The `send-bound-exception` markers live on two short aliases now**, and
that is the same rule this file has recorded three times from the other
side: `cargo fmt` moves a trailing comment off a line it reflows, and a
marker lost that way was lost twice more during this change.
`erased::SharedTransport` and `erased::SharedTimer` are
`dyn .. + Send + Sync` on one line each, every use site writes
`Box<SharedTransport>`, and `cargo fmt` and `just invariants` now pass
*together*.

**The rule is narrower than it was stated, which is worth knowing before
paying for it again.** "Deletes one from a `where` clause" is false as a
general claim — measured with `rustfmt --edition 2024` on both shapes: a
short predicate keeps its trailing marker untouched, and a predicate long
enough to wrap keeps it too, because the marker travels with the
continuation line that carries the `Send`. Since `no-send-or-sync` scans
for a `Send`/`Sync` line and a marker on that same line, both survive the
check. `Native::http3`'s four bounds are written that way and `cargo fmt`
and `just invariants` pass together over them.

So the shape that actually loses a marker is the one the first sentence
names — a comment on a line fmt has to reflow *around*, not one on a
`where` predicate. Believing the wider version cost three helper traits
and two public re-exports in `hclient-native` before it was measured. Four `amendment-C1` markers **left** `client.rs` with
the type parameter, because `Transport::to_error` is now called where `Self`
is concrete.

`.notes/erased-client.md` has the measurements, including the
`package-build` trap met on the way: it verifies against the shared
`target/debug/deps`, so a stale `rmeta` for an unchanged version makes it
fail — or, worse, pass — misleadingly. **It is closed now, and it took CI failing
on a real push to close it**: renaming `hclient-rt`'s seam without moving
its number made `hclient-rt-smol` verify against the previous push's
compiled `hclient-rt`, restored from CI's cache, because cargo fingerprints
a registry crate by version and never rebuilds it for a changed source.
Clearing `target/package` was tried first and did nothing — the stale
thing was the artifact, not the tarball. `just package-build` verifies in a
scratch build directory of its own now, at about two minutes a run.

### A `dyn` that declares no auto traits does not hide `Send` — it removes it

`Native::execute`'s future is `Send` now, and the change is one field of
one private struct: `connect.rs`'s `Answers` held the resolver's stream as
`Pin<Box<dyn Stream<..> + 'a>>` and holds it as `Pin<Box<S>>`. Same
allocation, same absence of `unsafe` — the box is there for pin
projection, which is what its comment always said — and the concrete type
is simply no longer thrown away. **No bound, no `send-bound-exception`
marker, no dependency**: the property is *inferred* per instantiation,
so a `!Send` resolver still works and still yields a `!Send` future.

`tokio::spawn` of a request works, which is what the consumer asking for
this needed. `tests/send_future.rs` pins it twice — once on the type, once
by actually spawning one against a server — and was checked in the failing
direction, where the old `dyn` names `Answers` as the type that contains
it.

**This crate's own doc comment had named the box as the single cause for
two verticals and drawn the opposite conclusion**, because it weighed
exactly one repair: declaring `+ Send` on the `dyn`. That does oblige the
seam — `Resolve::lookup` returns `impl Stream`, unnameable, so
unbounded, still `E0658` on 1.98 — and the argument was right about its
own question. *Removing* the `dyn` was never asked.

**The alternative was built and measured before this one was believed.**
Converting `Resolve` to `BoxStream` and `Blocking::run` to `BoxFuture`
(both from `futures-core`, already in every graph here; one feature,
`alloc`, no crate added) works, makes `Resolve` object-safe, and takes 71
call sites across 17 files. It costs **`hclient-dns-doh` entirely**: DoH
resolves through a generic `C: Transport`, whose RPITIT future is equally
unnameable, and no consumer can supply the impl, because both the trait
and the type are foreign to them. One crate, no fix inside the design.

**The rule that came out of it is the part worth keeping**, and it
explains the erased-`Client` section above from the other side:

- at a **concrete** type `Send` is *inferred* — nothing has to be named;
- in a **generic** impl `Send` must be *proven* — every RPITIT future in
  the chain has to be named, and `impl Future` has no name.

So the six RPITIT seams here — `Transport`, `Resolve`, `TcpConnect`,
`TlsConnect`, `Blocking`, `WebSocketConnect` — block a *declaration* and
not an *instantiation*. Which is why the cheap repairs are the places a
`dyn` discards a property the concrete type already had, and the
expensive ones are the places something must promise it in advance.

**The `http3` arm is still `!Send`, and it is the same decision rather
than a second one.** `H3` itself is clean — `H3::resolve` boxes the
concrete stream, and `UdpBind::bind` and
`QuicTlsConnect::quic_client_config` are **synchronous**, so nothing
there loses the property. What loses it is `http3::arm`'s deliberate
erasure, `Box<dyn BoxedStaged<'_>>` and `Staging<'a>`, which exists to
keep `H3`'s bounds off `Native`'s `Transport` impl. Declaring `Send`
there obliges that **blanket** impl to prove `StagedConnect::connect`'s
RPITIT future `Send` for a generic `T`, and behind it `Resolve` again.
So the QUIC arm and the DoH resolver are one question with one answer.

**And the doctest gate did not catch the claim going stale**, which is
this file's recurring rule met from a new direction. The `compile_fail`
fence asserting the old `!Send` had to start failing, and it did — in the
**default** build. `just test-doc` runs `--all-features`, where `http3`
keeps the future `!Send`, so the fence still passed there. A doctest
cannot be gated on `not(feature = "http3")`, so the positive half lives
in `tests/send_future.rs` and the fence that stays is the one true in
every configuration. Worth knowing before adding another: **a doc fence
can only assert what holds under `--all-features`.**


### A default is not a default when Cargo unifies features — it is a floor

`cargo add hclient` gives a client that does not compile: `Client::new()`
needs `default-transport`, and `default` is `["idn"]`. That cost is real and
lands in the five minutes where someone decides whether to keep reading, so
the feature **was** moved into `default` — and moved back out one commit
later, which is the part worth keeping.

**Cargo unifies features across a graph, so a default here is a floor.**
Measured on a scratch workspace rather than argued: `lean` depending on
`hclient` with `default-features = false`, `fat` depending on it with
defaults, and cargo builds **one** `hclient` — `default,default-transport,idn`
— which `lean` links. `lean` alone resolves zero tokio, rustls or hyper; in
the shared graph it gets all three. **The party who wanted the small graph
is not the party who decides.**

That is the same argument that keeps the WebSocket framing in a crate of
its own. It kept the QUIC TLS seam out of `hclient-tls` too, until that
seam stopped carrying `quinn-proto` and there was nothing left for a
boundary to hold. **It is not
what keeps `hclient-h3` out of `hclient-native`** — that reason was
measured and is wrong, see the HTTP/3 section. Applying it to the two it
fits and not to a feature list would have been the inconsistency.

**The audience it protects is narrower than "every constrained build",
which is worth knowing before the next time this is raised.**
`hclient-native/examples/minimal.rs` reaches `Transport` directly and never
names `hclient`, so a 512 KB target is unaffected either way. The one who
pays is the caller who wants `Client` over a transport of their own —
`Native<Smol, NativeTls, Hickory>` — and would carry tokio, rustls and the
system resolver because something else in the graph was careless.

So the cost is paid in text: both READMEs and the crate docs now lead with
`cargo add hclient --features default-transport`. A flag someone reads
before compiling is cheaper than a graph they cannot get out of afterwards.

**One real defect came out of the round trip and is kept.** `DefaultClock`
had three `#[cfg]` arms — native-with-feature, browser-with-feature, and
`not(feature)` — and `wasm32-wasip2` **with** the feature matched none of
them, so the crate did not compile there at all. Its doc comment called that
"the same deliberate compile error as `DefaultTransport`", and the two are
not the same: `DefaultTransport` is named only by someone asking for it,
where `DefaultClock` is the default type parameter of `ClientBuilder`,
`RequestBuilder` and both forks of `Client`. And by the same unification
above, the trigger was never that user's own choice. `Client`'s forked
declaration had the identical gap for the identical reason — it keyed on
*the feature* where the question is *does `DefaultTransport` exist*. Both
now use one pair of conditions, negations of each other, so the arms are
exhaustive and non-overlapping by construction.
