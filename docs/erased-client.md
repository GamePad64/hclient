# Making `hclient::Client` non-generic

Where the work stands, what is already settled, and the order to finish it
in. The seam it needs is built and green
(`hclient_core::unversioned::erased`); what remains is converting the
facade onto it.

`erased-client-wip.patch` beside this file is an attempt at that conversion,
kept because the decisions in it were made and measured rather than guessed.
It does **not** compile — see [What is left](#what-is-left).

## Why this is possible at all, and what it is not

Three walls were named against it and two were cleared:

- **`Transport::execute` is an RPITIT**, so `dyn Transport` needs return
  type notation — `E0658` on rustc 1.98.0, re-measured 2026-08-20. Cleared
  by not asking: the boxed future is **not** `Send`, so there is nothing to
  prove and `BoxedTransport` has a blanket impl over every `Transport`.
- **`Timer::Instant` is `Copy + PartialOrd`**, and `Copy` on a trait object
  is not a thing. Cleared by erasing the *question* rather than the type:
  `ErasedInstant` answers *how long ago was this*, so the instant stays
  inside the clock that made it.
- **The third is not cleared and does not need to be.** `Native::execute`'s
  future is `!Send` — one box around the resolver's stream in `connect.rs`
  — so a request cannot be spawned. Recovering that would need the bound on
  seven seam methods and would exclude `hclient-rt-embassy`, whose `connect`
  future holds `RefCell<embassy_net::Inner>`. The erased client therefore
  has **exactly today's `Send` semantics**: the client is `Send + Sync`, its
  request futures are not.

So this is erasure for arity, not for spawning. Nothing that works today
stops working.

## Decisions already made

- **The erased type becomes `Client`.** The forked declaration
  (`#[cfg(feature = "default-transport")]` with and without a default type
  parameter) collapses into one, which is the single largest simplification
  in the change.
- **`Send + Sync` is said at the use site**, `Box<dyn BoxedTransport + Send
  + Sync>`, never on the trait. A backend that cannot satisfy it is refused
  at the constructor. `Embassy` is refused there, and already was: it is
  `RefCell` throughout, so `Client<Native<Embassy, ..>>` is `!Send` today.
  Those sites cite amendment C12 — a bound this crate chooses so a caller's
  value reaches `Client` by erasure rather than by a type parameter — which
  is C12's own stated criterion.
- **The timer is `Arc`, not `Box`.** `Deadline` holds one too, and shares it
  with the client. Found by the compiler.
- **The backend's type name is captured at construction.**
  `backend_name::<T>()` has four call sites in capability refusals, and
  erasure loses the type; `ClientBuilder` stores `std::any::type_name::<T>()`
  instead, so the refusals still name the backend.
- **`ClientBody` becomes concrete**:
  `Limited<Decompressed<Deadline<Cached<BoxBody>>>>`. `Response` loses its
  parameter with it.

## What is left

Roughly fifty compile errors in `crates/hclient/src`, then the tests.

**Do it bottom-up.** The attempt in the patch went top-down — declarations
first — and the error count oscillated between 40 and 65 for three rounds,
because each structural fix exposed the next layer of the same 2,000-line
file. The dependency order is:

1. `deadline.rs` — `Deadline<B>` over the `Arc`'d erased timer.
2. `lib.rs` — `ClientBody` concrete.
3. `response.rs` — `Response` over it.
4. `request.rs` — `RequestBuilder<'a>`.
5. `sse.rs` — `SseBuilder<'a>`, `ReconnectingSse*<'a>`.
6. `client.rs` last, and it is most of the work: `Inner`, `Client`,
   `ClientBuilder`, and the method bodies that reach through `T`.

Then **48 test and example files** name `Client<..>` or `Response<..>` and
lose those arguments. That part is mechanical.

## What to check when it is green

- `just invariants` — the `Send` markers are in `hclient`, and `cargo fmt`
  moves a trailing comment off a line it reflows. Run it every time.
- The library-ergonomics claim this is all for: a crate depending on
  `hclient` with `default-features = false` should write `&Client` with no
  `where` clause at all, where today it writes five.
- `tests/future_size.rs`'s ceilings, which will move.
