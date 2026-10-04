# Send, and embassy

> Part of the [AGENTS.md](../../AGENTS.md) record, split 2026-10-05. The root
> file keeps the operational rules and the index of parts. Sections below
> keep their original headings, and a "this file" in the text named the
> whole record when it was written.

### The restriction is at `Client`, and the converter was built and then dropped

Asked whether `Client` should simply be restricted to `Send` transports
with a converter for embassy. It already is — `Client::builder` demands
`SendTransport` — and a converter was written, `hclient-actor`: an actor
owning the `!Send` transport, with a `Send` handle in front of it.

**It is deleted, and by this file's own test for a thing's existence.**
Nothing depended on it, in this workspace or outside; it held no
dependency a feature would otherwise spread, its two being
`futures-channel` and `hclient-core`; and the one configuration it served
is itself `publish = false`, on the measurement that `Native<Embassy, ..>`
has no deployment today. A converter with no subject is the shape
`UpgradeSupport`'s spare variants were deleted for.

The layering below is what survives it, and it is worth keeping because
the sharper question the converter was built to answer does not depend on
the converter existing: **if such a boundary can manufacture the promise,
can the `!Send` accommodation come out of the seams?**

**No, and it deletes embassy rather than simplifying it.** Measured:
declaring `TcpConnect::Connecting` as a `Send` box makes
`hclient-rt-embassy` fail to compile at the boxing site, because the
future holds `Stack<'d> = &'d RefCell<Inner>`. So embassy would not be a
`TcpConnect` at all, `Native<Embassy, ..>` would not exist — and the
converter would have **nothing to wrap**, because it operates on a
`Transport` and the seams are below it. A boundary can manufacture a
promise; it cannot manufacture a transport the seams refused to let
exist. That is why deleting the converter costs this argument nothing:
what it establishes is a fact about the seams.

The layering that falls out is worth stating once:

| layer | demands `Send` | why |
|---|---|---|
| `TcpConnect`, `Timer`, `Resolve`, `TlsConnect` | **no** | each implementor names its own future's auto traits (C15) — this is what lets embassy exist |
| `Transport` | **no** | its future is an RPITIT, unnameable, so nothing could ask |
| `SendTransport` | it **is** the demand | an impl may carry bounds the trait does not (C16) |
| `hclient::Client` | **yes** | it boxes its transport `Send + Sync` |
| a converter above it | could manufacture it | and cannot exist without the row above the first |

Each layer restricts exactly where it can, and the accommodation at the
bottom is what any converter at the top would have to work on — which is
the reason the bottom row cannot be tightened, converter or no converter.

**And the converter is a crutch, which is the honest word for it.** It
buys `Client` at the price of streaming: the response is collected before
it crosses, so a body larger than `Limits::max_response` is an error
rather than a stream. On a device that is the trade to think about twice.
What it is not is a workaround for a design mistake — the seams are right,
and buffering at a thread boundary is what a thread boundary costs.

### The embassy runtime is the workspace's only `!Send` counterexample

Asked whether `hclient-rt-embassy` is needed at all, and the workspace's
own test for a crate — *does it hold a dependency a feature would
otherwise spread* — gives the wrong answer here, because the value is not
a dependency.

**As a deployment runtime it has no configuration today, measured.** It is
not `no_std` and cannot be: `http` 1.5.0 still carries
`compile_error!("`std` feature currently required")`. So its
configuration is *std plus `embassy-net`* — and `embassy-net` is the
`no_std` stack. The only device the docs named, esp-idf, integrates
`embassy-sync`, `embassy-time-driver` and `embassy-futures` and **not**
`embassy-net`, handing you lwIP through `std::net` instead; the runtime
there is smol or tokio. The one place `Native<Embassy, ..>` actually runs
is a Linux host over a TAP device, which is this repository's CI.

**As a design counterexample it is now the only one there is**, and that
is load-bearing. Measured across the seam implementors: `hclient-rt-tokio`
boxes three futures `Send`, `hclient-rt-smol` two, `hclient-dns-doh` two
since this week, `hclient-tls-rustls` and `hclient-tls-native-tls` box
none at all — and `hclient-rt-embassy` boxes exactly one, plain, because
`embassy_net::Stack<'d>` is `&'d RefCell<Inner>` and the crate carries no
`unsafe impl Send` anywhere.

Delete it and every test stays green while three decisions quietly lose
their subject: `TcpConnect::Connecting` could become a `Send` box,
`SendTransport` would look like ceremony because every transport would
satisfy it, and `Transport::execute`'s unbounded RPITIT would look like
caution with no case. **That is `UpgradeSupport`'s deletion inverted** —
those four variants went because the distinction had one reachable side,
and embassy is what makes the second side reachable here.

It is pinned rather than described: `tests/seam.rs` asserts that
`Native<Embassy, ..>` **is** a `Transport` and **is not** a
`SendTransport`, with a real negative rather than an `assert_not` that
accepts anything.

**Both things that followed are done.** The crate's own doc leads with
what it is — the `!Send` witness — rather than with embedded reach it does
not deliver. And it is **`publish = false`** as of the first release:
publishing would promise a deployment configuration measurement says does
not exist, and a published surface is one that must not move. Not
publishing is free and reversible; un-publishing is neither.

It follows `hclient-rt-pair-check`'s shape exactly, down to carrying no
licence symlinks and no README — which is also what keeps the hand-check
below (*two per publishable crate, plus one*) true rather than off by two.
Flip it back the day `no_std` becomes reachable: `http` growing it, or
this workspace dropping `http` from its public API. Nothing else changes —
the TAP suite still runs on every push, and the seam test still pins the
counterexample.

### Channels do not transfer to embassy, and the reason is what crosses them

Asked whether the repair that put `hclient-fetch` under wasm threads works
for `hclient-rt-embassy` too. Measured rather than reasoned, and the
answer separates into three options of which the obvious one is the worst.

**The `!Send` is genuine, not a `dyn` erasing a property that was there.**
Measured against `embassy-net` 0.9.1: **zero** `unsafe impl Send` or
`Sync` anywhere in the crate, and `Stack<'d>` is `&'d RefCell<Inner>`. The
plain box in `Embassy::Connecting` is plain *because* the property is
absent — which is the opposite of `connect.rs`'s `Answers` and DoH's
streams, where a `dyn` was throwing away a property the concrete type had.

**What would have to cross is a stream, not a value, and that is the whole
difference.** `hclient-fetch`'s actor hands over one
`http::Response<Body>` per request and is done. What an embassy caller
holds is `EmbassyIo`, which implements the byte-stream seam and is
polled for the length of the exchange. So it would not be an actor, it
would be an **IO proxy**: every `poll_read` a round trip, and — since a
`&mut [u8]` cannot be lent across a channel — **an extra owned buffer and
an extra copy per read**. `EmbassyIo` already carries a 2048-byte scratch
per connection, deliberately a field because a stack local compiled to a
`memset` per call; a proxy adds another buffer and two channels on top, in
the resource a microcontroller has least of. And
`#[embassy_executor::task]`'s `pool_size` is fixed at compile time, so an
actor per connection makes the maximum number of concurrent connections a
constant in the API.

**What it would buy is a property that target cannot use.**
`embassy_executor::Executor` runs `!Send` tasks on one core, and a
`&RefCell` cannot be shared across executors at all — so on a dual-core
part the net stack still lives on one core and the socket cannot leave it.
There is nowhere to send to.

**The second option is real and costs streaming.** An actor one layer up —
at `Transport::execute` rather than at the socket — carries a *value*
again: `http::Request<RequestBody>` in (already `Send`), the response
**collected to `Bytes`** out. One channel per request instead of per read,
and no IO proxy. What it gives up is streaming, which on a device is
exactly the thing you keep when a response is bigger than RAM. It also
needs the transport `'static`, which the `StaticCell` idiom already
provides.

**And the third is the one that answers the actual want.** Nobody wants
`Send` on a single-core microcontroller; they want `hclient::Client` —
redirects, the jar, the cache — and `Send` is only the gate
`SendTransport` puts in front of it. The direct route is a client surface
that does not ask for it: eight `Send` declarations in
`hclient-core`'s erased module are what stand between the two, and a
parallel facade is the cost. That is the "second surface" question,
unchanged, and it is the one to answer rather than this one.

### Every backend is a `SendTransport` now, and the last one took a channel

**Six backends, six impls**, so every one of them can back an
`hclient::Client`: `hclient-native` (including its HTTP/3 arm),
`hclient-fetch`, `hclient-wasi`, `hclient-urlsession`, `hclient-winhttp`
and `hclient-mock`.

**`hclient-native`'s is conditional and that is the design working, not a
gap.** It implements `SendTransport` for every `Native` whose runtime, TLS
backend and resolver name `Send` associated futures, and for no other — so
`Native` over `hclient-rt-embassy` is still a `Transport` and simply not a
`SendTransport`. Nothing is excluded from the seam; something is excluded
from a promise. The one resolver that used to fail that test was
`hclient-dns-doh`, fixed one section up.

**`hclient-fetch` was the last, and it needed a channel.** Its
`execute_send` boxed `execute`'s future, which holds a `js_sys::Promise`
across its one await — fine on a single-threaded target, where
wasm-bindgen marks JS handles `Send` truthfully, and impossible under
`-Ctarget-feature=+atomics`, where it stops. `execute` is now three
pieces: a synchronous `start` needing `&self`, an async `finish` needing
**nothing** of `self` — which is what lets a `spawn_local` task be
`'static` with no `Arc` and no `Clone` bound — and a `report` that emits
the hook where `&self` already is. `Transport::execute` is untouched, so
the spawn is paid for only by a caller who wants `Client`.

**What a spawn puts at risk is the drop-is-cancellation contract**, since
a spawned task does not stop because its spawner went away. `deliver`
races the work against `Sender::cancellation` and is a named function so
that `tests/deliver.rs` can be the pair that pins it — a `deliver`
ignoring cancellation passes one and fails the other, checked by applying
that mutation.

**The check that guarded the old state gained a direction rather than
being deleted.** `fetch-under-wasm-threads` now asserts the library
**builds** under threads *and* that `SingleThreaded<T>`'s `unsafe impl
Send` is still rejected there, `E0277` in `tests/promise.rs`. The second
is what the old recipe was really protecting and is unchanged; the first
would have been false the day before.

**What is left `!Send` is nothing** — measured from outside on the day, and
the one honest asterisk is that a JS `WebSocket` belongs to the realm that
made it, so `hclient-fetch`'s `WebSocketConnect` seam declares no `Send`
and asks for none.

### `Client` in wasm: everything but the constructor is the same source

Asked whether `Client` can be used in wasm without changing code, and
measured on one scratch crate rather than reasoned about. A function that
takes `&Client`, builds a request, sends it, calls `error_for_status`,
`collect` and `json` compiles **unchanged and with no `#[cfg]` at all** on
`x86_64-unknown-linux-gnu`, `wasm32-unknown-unknown` and `wasm32-wasip2`.
It does not even need the `default-transport` feature — a consumer that
takes a `Client` from its caller names no backend.

**Construction is the one place that differs, and it differs three ways:**

| target | how a `Client` is made |
|---|---|
| native | `Client::new()?` — fallible, the OS trust store can fail |
| `wasm32-unknown-unknown` | `Client::new()` — infallible, `Fetch::new()` cannot fail |
| `wasm32-wasip2` | **`Client::new` does not exist**: `DefaultTransport` is undefined there, so it is `Client::builder(WasiHttp::new()).build()?` |

The third is deliberate and recorded far from the other two — `hclient`
does not depend on `hclient-wasi`, so a WASI build names its transport.
Worth stating together, because *"there is no `?` on it — that is the only
difference"* is true of the first two and silent about the third.

So the portable shape is the ordinary one: construct at the top, where the
entry point is target-specific anyway, and pass `&Client` down.
`crates/hclient/examples/portable.rs` is exactly that and is built for all
three targets on every push, so this is a CI gate rather than a claim.

**One wrinkle the measurement exposed, and it is now handled.** Forgetting
the `?` produces `Result<Native<..>, ..>` where a transport was wanted, so
the new `on_unimplemented` note offered to implement `SendTransport` for a
`Result` — sound advice for the wrong problem. It now says first that a
`Result` here means a missing `?`, and why portable code meets exactly
that.

### The mock was built for this workspace's tests and not for anybody else's

`hclient-mock` is used by 35 files here and had **no test of its own and
no doc example**. Asked whether it is good for a library user writing unit
tests, the only honest way to answer was to write one from outside the
workspace — the same instrument that found `require_version` missing. It
took three failed compiles, and the fourth still could not make the
commonest assertion there is.

**Five walls.** `push_response` took `&'static str`, so a body built at
run time did not compile. `MockTransport` was not `Clone`, and
`Client::builder` takes its transport by value, so a test had to hand the
mock over and reach back through `transport_as`. **The request body was
not recorded at all** — only its `size_hint` — so *"my code posted the
right JSON"* could not be written. There was no way to ask whether a
scripted response went unused. And there was no example of any of it.

All five are closed. What is worth carrying forward is **why the obvious
fix to the third one is wrong.** Recording a `Rewindable` body by calling
its factory broke `hclient`'s
`a_rewindable_body_is_replayed_from_the_snapshot_taken_before_the_first_attempt`
inside a minute: that test counts factory calls to pin *one snapshot per
hop, not one per attempt* — a claim about the **client** — and the mock
had become a second caller. Purity is not the question; the contract makes
the *result* the same and says nothing about a caller counting calls. So
the factory is handed to the test as a factory and `snapshot()` is the
opt-in, where the extra call is the test's own choice. This crate's own
doc, applied to itself: *a faithful model of a backend, not something that
masks the defect under test.*

`RecordedBody` therefore has four cases rather than an `Option<Bytes>` —
"no body", "a body this mock will not read for you" and "a body nothing
can read twice" are different facts, and a streaming body is
`NotRecorded` rather than `Empty` because a silent empty would pass a test
that an honest refusal fails. It is `PartialEq` and deliberately not
`Eq`: a closure cannot keep reflexivity.

**Request matching is refused with a reason**, not omitted: the flows this
double exists for — a redirect chain, a `425` replay, a retry — are
**ordered**, and a matcher would let a test pass while the code made its
requests in the wrong order.

**The pattern across three findings this week is now clear enough to
state.** `require_version` was unreachable from the builder while the file
testing it built requests by hand; `hclient-mock` was comfortable for
tests written beside it and awkward for tests written against it; and the
CLI's backend refusal could not fail in the configuration CI runs. In each
case the workspace's own tests were the wrong instrument, because they
share the author's knowledge of where the doors are. **Writing a consumer
is a different measurement from writing a test.**
