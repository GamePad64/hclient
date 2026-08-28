# A blocking API: design

`docs/competitive-gaps.md` §G3 records this as **refused**, and the
refusal has aged: `ureq` exists entirely to serve blocking callers and
reqwest ships `blocking` for them, so it is the largest population in the
comparison. The refusal's own reason — *a blocking facade needs a runtime
to block on, and picking one is exactly the choice `hclient-rt` exists to
avoid* — is the constraint this design has to satisfy, not a wall.

**The requirement is one sentence: every line of logic stays async, and
the synchronous surface is a wrapper around a one-shot executor.** No
second redirect path, no second cookie path, no second cache path. If a
blocking method does anything except drive an async one, the design has
failed.

## The measurement that decides the shape

A bare `block_on` parks a thread and polls one future. It provides **no
reactor** — nothing turns OS readiness into a wakeup. So whether it can
drive a transport is a fact about that transport's runtime, and the two
shipped runtimes answer differently. Measured on 2026-08-29, same request,
same server, same `futures_executor::block_on`:

| runtime under a bare `block_on` | result |
|---|---|
| `Native<Smol, ..>` | **reaches the network** — `async-io` starts its own reactor thread on first use |
| `Native<Tokio, ..>` | **panics**: *"there is no reactor running, must be called from the context of a Tokio 1.x runtime"* |

That is the sharpest fact here, and it rules out the obvious design. A
`blocking::Client` that wrapped any `hclient::Client` and blocked on it
would turn a configuration mistake into a **panic** — and `Client` is
erased, so it cannot look at its transport and refuse.

## The shape

**The facade owns no executor; it takes one.**

```rust
pub trait Blocker {
    fn block_on<F: Future>(&self, f: F) -> F::Output;
}

pub struct Client<B: Blocker> { inner: crate::Client, block: B }
```

Every method is one line:

```rust
pub fn send(&self, req: RequestBuilder<'_>) -> Result<Response, Error> {
    self.block.block_on(req.send())
}
```

That satisfies the requirement structurally rather than by discipline:
there is nowhere for a second implementation to live.

It also puts the runtime choice back with the caller, which is
`hclient-rt`'s whole reason to exist. A tokio user passes
`Runtime::block_on` and it works; a caller with no runtime passes
`futures_executor::block_on` and it works **for a transport whose runtime
drives itself**. Nobody is handed a panic by a default they did not
choose.

**Cost: no new dependency.** The trait and the wrapper need no executor at
all. Only the convenience constructor below does.

## The convenience constructor, and why it must not use tokio

`ureq`'s appeal is one line, so the facade needs one:

```rust
let client = hclient::blocking::Client::new()?;   // `blocking` feature
```

The transport it builds **must be the smol one**, not
`DefaultTransport`'s tokio one — that is the table above, and it is the
difference between working and panicking. So `blocking::Client::new()`
assembles `Native<Smol, Rustls, SystemDns<Smol>>` and pairs it with a
minimal `block_on`.

This is the same class of decision `Client::new()` already makes — it
imposes tokio+rustls+system DNS — and the honest framing is that the
blocking default imposes *the runtime that works without an ambient
executor*, which is a property rather than a preference.

Two candidates for the `block_on` itself, both measured for weight:
`pollster` 1.0.1 (28.5M downloads, **no dependencies**) and
`futures-executor` 0.3 (702M downloads, already a dev-dependency across
this workspace). Either is one crate; `pollster` is the smaller surface
and exists for exactly this.

## What the caller sees

`Response` and `Collected` need blocking mirrors, and only two methods
have anything to do:

- `Collected` is already a value — `bytes()`, `text()`, `json()`,
  `error_for_status()` are synchronous today and need no wrapper at all.
- `Response::collect()` and `Response::chunk()` are the two that await,
  so they become `block_on` one-liners.

Nothing else in the facade is async, which is worth knowing before
estimating this: the blocking surface is **two methods plus a
constructor**, not a parallel API.

## The hazard, and whether it can be detected

Calling a blocking method from inside an async context is the mistake
every blocking facade invites. The outcomes differ and neither is good:
`tokio::Runtime::block_on` **panics** with *"Cannot start a runtime from
within a runtime"*, and `futures_executor::block_on` simply blocks the
executor's thread — a deadlock if that executor is single-threaded, which
is the common case.

reqwest documents this and does not detect it. Whether this facade can do
better is **unmeasured** and worth one experiment before building:
`tokio::runtime::Handle::try_current().is_ok()` detects the tokio case
cheaply, but there is no equivalent for a bare `block_on`, so a check
would cover one executor and not the other — which is the
*silently-ignored-setting* shape this workspace refuses elsewhere. The
alternative is a documented refusal in the one place a caller reads.

## What this design deliberately does not do

- **No thread pool, no background runtime.** reqwest's `blocking` spawns a
  tokio runtime on a background thread and channels requests to it. That
  is a second design with its own failure modes, and it re-imposes the
  runtime choice this one hands back.
- **No blocking `Transport` seam.** The seam stays async; blocking exists
  strictly above `Client`.
- **No blocking WebSocket or SSE.** Both are streams whose whole value is
  incremental, and a blocking `next()` on them is a different product.

## Open questions for the owner

1. **Feature or crate?** A `blocking` feature on `hclient` costs no
   dependency for the trait-and-wrapper half, and one crate for the
   convenience constructor. This workspace's rule — *a crate exists to
   hold a dependency a feature would otherwise spread* — points at a
   feature, since one dependency-free crate is not a spread worth a
   boundary. Recorded, not decided.
2. **Does `blocking::Client::new()` earn imposing smol?** It is the only
   assembly that works without an ambient executor, but it means the
   blocking and async defaults differ in their runtime, which is a
   surprise worth stating loudly if it ships.
3. ~~**Detect the nested-executor mistake, or document it?**~~ —
   **measured, and it is document, without it being close.** See the
   section below.


## The nested-executor mistake: measured, and detection is the wrong answer

Calling a blocking method from inside an async task is the mistake every
blocking facade has to have a position on. It was recorded here as a
judgement call between detecting it and documenting it. Measured on
2026-08-29 — a two-worker `tokio` runtime, the call made from inside
`rt.block_on`:

| what is nested | result |
|---|---|
| `futures_executor::block_on` on a smol-backed future | **succeeds**, 120.1 ms for a 120 ms timer |
| `tokio::runtime::Handle::current().block_on` | **panics**, and tokio writes the message |
| `tokio::runtime::Handle::try_current()` | `true` inside, `false` outside |

**The three rows together say that detection would fire on the wrong
case.** The one this facade could detect is row 1 — and row 1 *works*: a
bare `block_on` inside tokio parks one worker thread and the future
completes, because `async-io` drives its own reactor on a thread of its
own. Refusing it would refuse something that succeeds, and the real cost
there is a starved worker under load or a deadlock on a `current_thread`
runtime, neither of which a `try_current()` check can tell apart from the
healthy case.

Row 2 is the case that genuinely fails, and **tokio already detects it and
panics with a better message than this crate would write** — it names
`block_on` and explains the thread it is on. A check of ours would arrive
first and say less.

And row 3 is the reason detection could not be general even if it were
wanted: `try_current` is tokio's, and there is no equivalent question to
ask of smol, of `futures_executor`, or of an executor this workspace has
never heard of. So a detector would be a **tokio-shaped check inside a
runtime-agnostic facade** — the exact coupling `hclient-rt` exists to
avoid, bought for a case that works.

**It also settles question 2 in the other direction from how it was
posed.** Imposing smol on `blocking::Client::new()` was recorded as a
surprise to be stated loudly. It is that, and it is *also* the more
forgiving default: row 1 is a smol-backed stack surviving the nested
mistake, where a tokio-backed one under the same bare `block_on` panics
before it reaches the network at all. The runtime that works without an
ambient reactor is the runtime a blocking caller has, which is what makes
the asymmetry a reason rather than an accident.

## The fourth option, which the three questions above did not consider

Questions 1 and 2 both turn on a dependency: `blocking::Client::new()`
needs an executor that works with no ambient reactor, the design named
smol, and smol is what a `blocking` feature would spread to every graph in
which any crate switched it on. That is the floor argument, and it is the
argument for a crate.

**It has no subject if the facade owns a `current_thread` tokio runtime
instead.** A blocking caller who wants `new()` at all is asking for
`default-transport`, which is tokio — so the runtime is already in that
graph, and entering it with `Runtime::block_on` supplies the reactor a
bare `block_on` does not. Measured on 2026-08-29, over the real network
through `hclient::Client`:

```
own current_thread tokio -> Ok("200 OK 559 bytes")
  reuse 0 -> Ok(559)
  reuse 1 -> Ok(559)
```

The reuse rows are the half worth measuring rather than assuming: a
`current_thread` runtime drives nothing between calls, so the question was
whether a `Client` outlives one `block_on` and can be used from the next.
It can.

**What that buys, against the smol shape:**

- **no new dependency at all** — so the feature-versus-crate question
  loses the fact it turned on, and a `blocking` feature on `hclient` costs
  a graph that already has `default-transport` nothing;
- **no runtime asymmetry to state loudly** — the blocking and async
  defaults are the same runtime, where imposing smol meant explaining why
  they differ;
- **the nested mistake fails loudly** — tokio's own *"Cannot start a
  runtime from within a runtime"*, which the section above establishes is
  a better message than one of ours, where the smol shape's nested case
  silently parks a worker.

**What it costs**, and it is the honest half: a blocking caller on a
runtime that is not tokio pays a tokio they do not use. That is exactly
the caller the `Blocker` trait is for — they pass their own
`block_on` and never call `new()` — so the cost falls on the convenience
constructor rather than on the facade, which is where a convenience's
costs belong.
