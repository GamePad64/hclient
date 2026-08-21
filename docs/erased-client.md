# `hclient::Client` names no type parameters

Done. `Client` is one concrete type, `Clone` is an `Arc` bump, and a
library takes `&Client` with no `where` clause where it used to write five.

This document is the record of what it cost, because two of the three costs
were **not** in the plan this was executed from, and one of them contradicts
what that plan asserted.

## What it buys, measured

`crates/hclient/examples/portable.rs` is the case the whole thing is for — a
consumer written against this client, generic over nothing it cares about.
Before:

```rust
pub async fn fetch<T, S>(client: &Client<T>, args: FetchArgs, ctx: &mut S)
    -> Result<(), ComponentError>
where
    T: Transport,
    T::Error: Send + Sync + 'static,
    T::Body: http_body::Body<Data = Bytes> + Unpin,
    <T::Body as http_body::Body>::Error: StdError + Send + Sync + 'static,
    S: ContentSink,
```

After:

```rust
pub async fn fetch<S>(client: &Client, args: FetchArgs, ctx: &mut S)
    -> Result<(), ComponentError>
where
    S: ContentSink,
```

Four bounds gone, and none of them was a bound this function had an opinion
about — a generic function has to restate its callee's where-clause, and
that is the tax erasure removes. The forked `Client` declaration
(`#[cfg(feature = "default-transport")]`, with and without a default type
parameter) collapsed into one, and `Clone` became a derive: erasure put the
transport and the clock behind the `Arc` in `Inner`, so there is no
`T: Clone` for a derive to demand.

`Client::execute`'s future grew from 4,344 to **4,960 bytes**, against the
6 KiB ceiling `tests/future_size.rs` already had. Four `send-bound-exception:
amendment-C1` markers **left** `client.rs`, because `Transport::to_error` is
now called where `Self` is concrete, at the blanket impl.

## How it works

`hclient_core::unversioned::erased` holds `BoxedTransport` and `BoxedTimer`,
each with a blanket impl over every `Transport`/`Timer`. Two walls were
named against this and both were cleared:

- **`Transport::execute` is an RPITIT**, so `dyn Transport` would need return
  type notation — `E0658` on rustc 1.98. Cleared by not asking: the boxed
  future declares no `Send`, so there is nothing to prove and the blanket
  impl compiles. A backend author writes nothing.
- **`Timer::Instant` is `Copy + PartialOrd`**, and `Copy` on a trait object
  is not a thing. Cleared by erasing the *question*: `ErasedInstant` answers
  *how long ago was this*, so the instant stays inside the clock that made
  it. `docs/competitive-gaps.md` §G13 calls that permanent; it is not.

`Client` holds `Box<erased::SharedTransport>` and
`Arc<erased::SharedTimer>` — two aliases, each `dyn .. + Send + Sync`, and
the bound lives on those two short lines and nowhere else. That is a rule
rather than a style: `cargo fmt` moves a trailing comment off a line it
reflows and deletes one from a `where` clause, so a `send-bound-exception`
marker cannot survive on a long signature. This workspace has lost one that
way four times, twice during this change.

`Client::transport_as::<T>()` is how a caller asks for the concrete backend
back — a mock's recorded requests, or a `Native` to lend to
`hclient-tungstenite`. The `Option` is honest: nothing checked, when the
client was built, that the backend is the one this caller names.

## What it cost

### 1. The embedded target has no `Client`

The plan said *"`Embassy` is refused there, and already was: it is `RefCell`
throughout, so `Client<Native<Embassy, ..>>` is `!Send` today."* **That was
wrong**, and it is the sentence to learn from: being `!Send` and being
*refused* are different, and today's generic `Client` builds fine over
Embassy — it simply yields a client that cannot cross a thread.

Measured: an erased `Client` boxes its transport as `Send + Sync`, and
`RefCell<embassy_net::Inner>` is not `Sync`, so `Client::builder` refuses.
`hclient-rt-embassy`'s nine live TAP scenarios are written against
`Native`/`Transport` directly now — `tuntap.rs`'s own `get` helper, ~20
lines — and the CI job is unchanged and green. What that job no longer
covers is the `Client` layer above the transport; that layer is
target-independent and is exercised by the rest of the suite on every other
backend.

### 2. Nothing a request produces is `Send`

One `ClientBody` has to serve every backend, and `hclient-fetch`'s body
holds a `dyn Stream` with no auto trait. So a `Send` on the erased body does
not weaken the browser backend, it **excludes** it: with `BoxBody` declared
`Send`, `cargo test -p hclient-fetch --target wasm32-unknown-unknown
--no-run` refuses `Client::builder(Fetch::new())` outright.

That was measured rather than reasoned, and only after the `Send` version
had been written and the whole native suite made green under it — which is
the process note worth keeping: **`cargo nextest run --workspace` does not
build for `wasm32-unknown-unknown`**, so the browser was invisible for a
full round of this work, exactly as `AGENTS.md` records happening for six
merges once before. The cheap check is
`cargo test -p hclient-fetch --target wasm32-unknown-unknown --no-run`.

So the trade is forced, and both halves are named in `shape.rs`:

- **Lost.** `tokio::spawn` of a response body, which worked on
  `hclient-native`. A caller who needs it reaches past the facade with
  `transport_as`.
- **Kept.** Every backend this workspace ships can be a `Client`.

The request future is a separate and cheaper matter: `hclient-native`'s
`execute` future is *already* `!Send`, pinned by a paired doctest on
`Native`, so the only backend erasure takes that from is the mock.

**`Client` itself stays `Send + Sync`**, which is the half that has to — a
client lives in shared application state.

### 3. A `!Send` hook cannot be watched through `Client`

`hclient-fetch`'s P13 test — *a single-threaded runtime can watch* — used to
run through `hclient::Client` with a `Recorder` holding an `Rc`. An erased
`Client` refuses that at `builder`. The property is a fact about the hooks
seam rather than about `Client`, so the test moved down one layer to
`Transport` and still asserts it. What is genuinely lost is watching a
`!Send`-hooked browser transport *through the facade*: cookies, redirects
and the response cache are not available to a caller who wants that.

## What is deliberately not done

**A `#[cfg]` that makes `BoxBody` `Send` off-wasm.** It would give native
callers their spawnable bodies back, and `Send` is a claim about threads
which the wasm targets do not have, so it would not be a capability that
lies. It is refused because a cfg-alias hides the symptom rather than
removing the cause, and because the resulting `ClientBody` would be a type
whose auto traits depend on the target — a thing a portable library cannot
reason about.

**A second, generic client kept beside the erased one.** It loses nothing
and duplicates ~2,000 lines of facade that would drift.

## Checks

`fmt-check`, `lint`, `invariants`, `graph`, `docs`, `test-doc`,
`test-no-default`, `features`, `packaging`, `package-build`,
`build-three-targets`, `test-wasi`, `test-embassy-live`, `fuzz-smoke`, and
`cargo test -p hclient-fetch --target wasm32-unknown-unknown --no-run`.
1,745 tests and 22 doctests.

One trap met on the way, in `package-build`: it verifies each packaged crate
against the shared `target/debug/deps`, so a stale `rmeta` for an unchanged
version number makes it fail — or, worse, pass — misleadingly.
`cargo clean -p <crate>` is the fix, and the failing direction is the one to
remember, because a gate that passes over a broken tarball is this file's
own rule about a check that cannot fail.
