# The hooks seam on the two ambient backends (v0.4 W2)

`http-ng-fetch` and `http-ng-wasi` now implement
`http_ng_core::unversioned::Hooks`. They are the third and fourth
implementations, after `http-ng-native` (`db61a06`) and `http-ng-h3`
(`f6d582e`), and they are the first two that own **no connection at all**:
the browser makes, keeps, reuses and closes connections without telling a
page, and `wasi:http`'s client interface is one function with no
connection resource anywhere behind it.

`http-ng-core` is **untouched** — no variant, no field, no bound.

---

## 1. The answer, in one table

| event | `http-ng-native` | `http-ng-h3` | `http-ng-fetch` | `http-ng-wasi` |
|---|---|---|---|---|
| `Connected` | yes | yes | **no emitter** | **no emitter** |
| `Reused` | yes | yes | **no emitter** | **no emitter** |
| `Head` | yes | yes | yes | yes |
| `Closed::Ended` | yes | **no emitter** | **no emitter** | **no emitter** |
| `Closed::Stale` | yes | yes | **no emitter** | **no emitter** |
| `Closed::Failed` | yes | yes | **no emitter** | **no emitter** |

**One event of four**, on both, and it is a result rather than an
omission. It is the same shape the workspace has now recorded three times:
`http-ng-native` refused to add a *request queued* variant it could not
emit, `http-ng-h3` reported that `CloseReason::Ended` has no subject in
QUIC, and this is the third. A variant nothing can emit is a capability
that lies, and an event set is a capability.

### 1.1 What the two backends do **not** agree on

Nothing. That is the point of doing both: they reached the same answer
from unrelated evidence — a browser API that withholds, and a WIT package
that has no such concept — which is what makes it a fact about the *event
set* rather than about either backend.

---

## 2. `http-ng-fetch`: what a browser will tell a page

The Fetch Standard's shape is `fetch(Request) -> Promise<Response>`. There
is no connection object, no connection identity, and no event that fires
when one is made or goes away. `Response` carries `type`, `url`,
`redirected`, `status`, `ok`, `statusText`, `headers`, `body`,
`bodyUsed` — and **no protocol member**.

### 2.1 The `Performance` surface, and why it is not a `Connected`

This is the one thing that looks like a counter-argument, and it deserved
measuring rather than a paragraph. `PerformanceResourceTiming` carries
`domainLookupStart`/`End`, `connectStart`/`End`,
`secureConnectionStart`, `requestStart`, `responseStart` and
`nextHopProtocol` — very nearly `ConnectTiming` plus `Connected::version`.

`crates/http-ng-fetch/tests/hooks_timing.rs` is the answer. Four tests,
green on Chrome 151 and Firefox 153:

1. **The control.** Same-origin, body drained: the entry *is* there, with
   a real `nextHopProtocol` and real timestamps. The browser knows. This
   test exists so the next two read as findings rather than as excuses,
   and so that a browser change that made the surface reachable fails a
   line here.
2. **The entry does not exist when the head arrives.** *Measured:* 0
   entries at the moment `Transport::execute` returns, 1 after the body
   is drained. The browser queues the entry when the *resource* finishes;
   `execute` returns when the *head* does, with the body still an unread
   `ReadableStream`. A `Connected` must precede the `Head` it explains, so
   at the only moment one could be emitted there is nothing to build it
   from.
3. **Nothing on the entry says which request it belongs to.** *Measured:*
   two fetches of one URL give two entries equal on `name`, `entryType`
   and `initiatorType`, and `requestId`, `id`, `connectionId` and
   `transferId` are all `undefined`. A `ConnectionId` exists only so a
   later event can be matched to it; one minted against the wrong entry is
   worse than none.
4. **Cross-origin the numbers are zeroes — read, not measured.** Resource
   Timing Level 2 §4.2 zeroes every phase and blanks `nextHopProtocol`
   without `Timing-Allow-Origin`, which is the general case for an HTTP
   client. **This one is not measured here and the test says so.** A
   `wasm-pack` harness serves one origin and has no network; two ways of
   reaching the same socket under another host string were tried and both
   fail with `TypeError: Failed to fetch` — `localhost` (resolves to
   `::1`, where the harness does not listen) and `[::ffff:127.0.0.1]`
   (Chrome 151). Closing it is a change to `wasm-bindgen-test-runner`, not
   to this crate.

Any one of 2 and 3 is sufficient on its own.

### 2.2 The clock is `performance.now()`, not `BrowserClock`

`BrowserClock` is this crate's `Timer` and reads `Date.now()` —
milliseconds since the epoch, which is what a *sleep* wants and what SSE
reconnect asks it for. `Head::elapsed` is a duration, and a wall clock is
the wrong instrument for one twice over: it can step backwards while a
request is in flight, and its resolution is a whole millisecond, longer
than most of the requests this measurement exists to explain.

---

## 3. `http-ng-wasi`: what the WIT has in it

The whole client interface is

```wit
interface client {
  send: async func(request: request) -> result<response, error-code>;
}
```

and the `response` resource has three accessors: `get-status-code`,
`get-headers`, `consume-body`. There is no connection resource anywhere in
`wasi:http@0.3.0`, no handle a `Connected` could name, and
`request-options` carries three timeouts the *guest* sets and nothing the
host reports back. **Neither `request` nor `response` has a version
accessor**, deliberately: the host decides what to speak and the guest is
version-agnostic.

`error-code`'s eleven `connection-*` variants are the one thing that
sounds like a connection event. They are not: they are how `send` fails,
they already reach the caller classified through `convert::wasi_err`, and
a `CloseReason::Failed` built from one would announce the end of a
connection whose beginning was never announced, under
`ConnectionId::UNWATCHED`, to a caller about to be handed the same error
anyway.

`Reused` has a second reason on top of that one, and the two are checked
together: `Capabilities::connection_reuse` is already `ReuseSupport::None`
on measured grounds (the host opens a connection per request), so the fact
a `Reused` would carry does not exist either.
`the_reuse_the_event_set_cannot_report_is_the_reuse_the_capability_denies`
is the line that makes a future host which pools force a decision here
rather than leave a green suite.

---

## 4. Two fields of `Head` that neither backend observes

`Head` has five fields. `uri`, `status` and `elapsed` are these
transports' own facts. The other two are not, and they are **the same
two** on both backends — which is why this is the sharpest thing the pair
found.

### 4.1 `id`: the seam has no value for "there is no connection"

Both report `ConnectionId::UNWATCHED`. Its own doc says it is "the id of a
connection nobody asked about" and ties it to `Hooks::WATCHING == false`.
Here somebody *is* watching and there is no connection to name. It is the
only value `ConnectionId::next` never returns, so an event carrying it
cannot collide with a real connection from another transport in the same
process — that is the whole of what it buys.

Minting one from the counter instead was considered and is worse: an id
whose `Connected` never arrives and whose `Closed` never arrives is a
number in a caller's log that nothing ever mentions again. Both backends
pin the choice (`the_id_names_no_connection_because_there_is_none_to_name`,
and `id=0` in the WASI transcript), and the mutation that mints one is
killed on both.

### 4.2 `version`: no honest value exists, and `http::Version` has no "unknown"

A browser will not tell a page whether it spoke HTTP/1.1, h2 or h3.
`wasi:http` has no version concept at all. `http::Version` has no variant
meaning "not observed", so **there is no honest value to put in the
field**.

What both backends do is report `resp.version()` — read off the very
response the transport is about to hand back — so the event and the
response cannot disagree. That value is `http`'s builder default,
`HTTP/1.1`, in both cases. It is a placeholder in both places, not a
measurement in either, and the tests say so in their names.

### 4.3 Both are recorded, neither is fixed

This is `http-ng-h3`'s treatment of `ConnectTiming::tls` applied twice:
the defect is in the seam's wording, and editing `http-ng-core` for one
backend's benefit is how an event set stops being a shape. **The task's
own instruction was to stop and report if a new variant were needed; none
is.** What would help is not a variant but two smaller things, and both
are listed in §8 as owed rather than done.

> **Superseded by §9.** Both were taken up, and the two answers came out
> **different**: `id` was never a debt and §8's first bullet was wrong,
> while `version` was one and is now `Option<http::Version>`. §4.1 and
> §4.2 are left as they were written, because §9 is an argument about
> them and an argument needs its subject intact.

---

## 5. Zero cost when nobody is watching

### 5.1 `http-ng-fetch`: a counting clock, and it is the browser's own

`http-ng-native` and `http-ng-h3` count clock reads through a `Timer` they
were handed. This crate has no runtime seam — in a browser the runtime
*is* the browser, the same fact that put `BrowserClock` in this crate
rather than in an `http-ng-rt-browser` that does not exist. So
`tests/hooks_cost.rs` installs the counting clock where the clock lives:
it replaces `Performance.prototype.now` with a tallying wrapper that
delegates to the original.

That is a stronger position than injection rather than a weaker one:
nothing had to be added to the transport for the measurement to be
possible, so what is measured is exactly what ships.

Equalities, not bounds. Identical on Chrome 151 and Firefox 153:

| build | request | `performance.now()` reads |
|---|---|---|
| `NoHooks` | success | **0** |
| a hook | success | **2** — the mark, and the read that closes the interval |
| a hook | connect refused | **1** — the mark; no head, so no interval |

The third row is not decoration: it pins the *shape* of the emission. A
transport that reported a `Head` on the error path would read the clock
twice there.

### 5.2 `http-ng-wasi`: two halves, and the missing third is named

A counting clock is **not available here**, and the reason is the same
fact the crate exists to state: a `wasi:http` guest needs no runtime, so
`WasiHttp` has no runtime seam to inject one through, and the clock it
does read — `std::time::Instant::now()` — is a host call the guest cannot
observe itself making.

Two ways of counting it from outside were tried and **measured** to not
work rather than assumed to:

- **Diffing the component's imports.** If a hookless build read no clock,
  the linker would drop the import. It does not: a component whose entire
  body is `WasiHttp::new()` already imports
  `wasi:clocks/monotonic-clock@0.2.9`, from wasi-libc.
- **Taking the clock away.** `wasmtime run -S cli=n` would make a clock
  read a trap. It refuses before the guest runs — *"component imports
  instance `wasi:io/poll@0.2.9`, but a matching implementation was not
  found in the linker"* (wasmtime 47.0.3) — so it cannot separate "did not
  call the clock" from "could not start".

What is proved instead is exact, in two halves that together cover the
same ground:

1. `mark::<NoHooks>()` is `None` — the clock read is inside a closure that
   is not called. Checked **on `wasm32-wasip2` under a real wasmtime**, not
   only on the host (`hooks::tests::a_hookless_build_takes_no_mark_and_a_watched_one_does`).
2. `mark` and `since` are the **only** clock reads in the crate, checked
   mechanically: `tests/hooks.rs`'s `the_clock_is_read_in_exactly_one_place`
   walks `src/` and fails if `Instant::now` appears anywhere else, or more
   than twice in `hooks.rs`.

A counting clock would collapse those two into one measurement. **What
would make one possible is a clock seam on `WasiHttp`, and that is
declined rather than deferred**: a transport whose whole premise is that
the host owns everything and it needs no runtime would gain a runtime
parameter in order to be tested.

### 5.3 What the hook costs to carry

`NoHooks` is zero-sized on both, so `Fetch<NoHooks>` is the same size as
`Fetch` and `WasiHttp<NoHooks>` the same as `WasiHttp`, and the default
type parameter names the same type rather than a second one.

---

## 6. `Send` — P13, answered again and not undone

**No `Send` bound arrives anywhere.** Both backends bound `H: Hooks` and
nothing else.

That is **one bound fewer than `http-ng-h3`, which was one fewer than
`http-ng-native`**, and the reason is structural rather than luck: the
only event these backends have fires while `execute` still owns
everything, so no response body has to hold a hook. `http-ng-native` needs
`H: Clone + Unpin` because its body reports a `Closed`; `http-ng-h3` needs
`H: Clone`; these need neither.

Both directions are pinned, because either alone reads as an accident:

- **`!Send` works.** `http-ng-fetch`'s recorder is `Rc<RefCell<..>>` and
  runs all the way through `http_ng::Client`, which bounds only
  `T::Error: Send + Sync` (amendment C1). `http-ng-wasi`'s guest recorder
  is the same shape under a real host.
- **`Send` passes through.** A hook holding an `AtomicUsize` leaves
  `execute`'s future `Send` on both. Without this the seam could satisfy
  the first bullet by being unconditionally `!Send`-poisoning — which
  would cost `http-ng-fetch` the property `promise.rs` carries the
  project's one `unsafe impl` for, and `http-ng-wasi` what its
  `send-bound-exception: amendment-C2` marker was spent on.

`impl WebSocketConnect for Fetch<H>` was made generic in the same change,
so switching observability on does not take an unrelated capability away.
No event is emitted for a WebSocket — the vocabulary has no word for one —
which is the call `http-ng-native` already made one seam over.

---

## 7. Mutations

Seventeen applied, **fourteen killed, three survived** — two of them
deliberate controls, one a survivor that caused a change. Anchor counts
were taken before each run and restored after: `http-ng-fetch` browser
suites `hooks` 10, `hooks_cost` 4, `hooks_timing` 4; `http-ng-wasi`
wasip2 52, host `tests/hooks` + `tests/shape` 11. Restores are `git
checkout` plus an explicit `utime` bump — a restore that preserves mtime
leaves cargo using the mutated artifact, which has mis-scored a run in
this workspace before.

### 7.1 `http-ng-fetch` (Chrome 151)

| # | mutation | outcome |
|---|---|---|
| F1 | `hooks::mark`: `H::WATCHING.then(now)` → `Some(now())` | **killed** — `hooks_cost` 1/4 failed (`a_client_with_no_hook_reads_no_clock_at_all`) |
| F2 | the `Head` emission removed entirely | **killed** — `hooks` 7/10, `hooks_cost` 1/4 |
| F3 | `status: out.status()` → `StatusCode::OK` | **killed** — `hooks` 1/10 |
| F4 | `version: out.version()` → `HTTP_2` | **killed** — `hooks` 1/10 |
| F5 | `elapsed` → `Duration::ZERO` | **killed** — `hooks` 1/10, `hooks_cost` 1/4 |
| F6 | `id: ConnectionId::UNWATCHED` → `ConnectionId::next()` | **killed** — `hooks` 1/10 |
| F7 | emit only when `out.status().is_success()` | **killed** — `hooks` 1/10 (the 404 test) |
| F8 | the pre-refactor two-gate form with the `Uri` gate removed | **SURVIVED** — see below |
| F9 | *control:* drop `.max(0.0)` in `since` | **SURVIVED as intended** |

**F8 is the one that changed the code.** `execute` used to read
`H::WATCHING` twice — once for the clock mark, once for the `Uri` clone
`Head::uri` needs, because `to_web_request` consumes the request. Ungating
the second one survives: a `NoHooks` build then clones a `Uri` and calls
`NoHooks::on` for every request, and the cost test still reads 0, because
the clock is only read from the `Some` arm of `since` and **a browser has
no allocator to count**. All 10 behaviour tests and all 4 cost tests were
green with the gate gone.

The fix is one `Option` carrying both — `mark::<H>().map(|at| (at,
uri.clone()))` — so the same mutation now ungates the clock too and F1
kills it. "One gate, once" is the rule `since`'s own doc already stated
for the interval; this is the other half of it.

**F9 is the deliberate control.** `performance.now()` is monotonic by
specification, so the clamp can never fire and no test can distinguish its
presence. It survived all three suites, which is what makes fourteen kills
mean something other than a harness that reports "killed" unconditionally
— the answer `http-ng-h3`'s M18 gave to four mis-scored runs, applied
here as method rather than as one report's footnote.

### 7.2 `http-ng-wasi`

| # | mutation | outcome |
|---|---|---|
| W1 | `mark`: `H::WATCHING.then(Instant::now)` → `Some(Instant::now())` | **killed** — wasip2 1/52 |
| W2 | the `Head` emission removed entirely | **killed** — host 2/23 |
| W3 | `status: out.status()` → `StatusCode::OK` | **killed** — host 1/23 |
| W3b | W3 **plus** the fixture answering 200 instead of 203 | **SURVIVED** — see below |
| W4 | `version: out.version()` → `HTTP_2` | **killed** — host 1/23 |
| W5 | `elapsed` → `Duration::ZERO` | **killed** — host 1/23 |
| W6 | `id: ConnectionId::UNWATCHED` → `ConnectionId::next()` | **killed** — host 1/23 |
| W7 | a stray `Instant::now()` in `execute`, outside the gate | **killed** — host 1/23 (`the_clock_is_read_in_exactly_one_place`) |
| W8 | *control:* `if began.is_some()` → `if H::WATCHING` | **SURVIVED as intended** |

**W3b is why the mock server answers 203.** `Head::status` is read off the
response, and a hard-coded `StatusCode::OK` in the emitter passes every
assertion a 200-answering fixture can make — measured: with the fixture at
200 the whole suite is green with the status invented. The fixture answers
`203 Non-Authoritative Information` now, and the guest's quiet mode
expects 203 too, so it cannot drift back unnoticed.

**W7 is the mutation that validates the test carrying half the zero-cost
claim.** Without it, `the_clock_is_read_in_exactly_one_place` would be a
line nobody had watched fail.

**W8 is the deliberate control.** `began.is_some()` and `H::WATCHING` are
the same constant, so no test can distinguish them; the code uses the
first because it is the one `Option` that already carries the mark.

### 7.3 Where the live WASI tests ended up, and why it is not tidiness

They were written in `crates/http-ng-wasi/tests/hooks.rs` and moved into
`tests/live_roundtrip.rs`. `just test-wasi` names `--test
live_roundtrip`, and it is the only recipe that runs with `wasmtime`
installed — the matrix `test` job runs `cargo nextest run --workspace`
on runners that have none. A live test in a file no recipe names would
have printed its `NOTICE` and reported `ok` on every CI leg, for ever:
exactly the defect `require_wasmtime` exists one level up to stop, and
one this task would have introduced while quoting the guard against it.

What stayed in `tests/hooks.rs` is the half that needs no host — the
source check and the capability check — because those must run on the
legs that have no `wasmtime`, which is most of them.

Every `http-ng-wasi` mutation above was re-run after the move rather than
assumed to still score the same.

### 7.4 A false start in the source check

The source check itself had a false start worth recording: its first
version counted six clock reads and was **measuring prose** — `hooks.rs`
writes about `Instant::now` at length and its own unit tests read a clock.
It strips comments and the `#[cfg(test)]` module now, and the needle lost
its parentheses, because one of the two reads is
`H::WATCHING.then(Instant::now)` — a function reference, still a clock
read, invisible to a `now()` needle.

---

## 8. What is not done, and what it would need

- ~~**A `ConnectionId` value meaning "there is no connection".**
  `UNWATCHED` is being borrowed for it, against its own doc. This is a
  `http-ng-core` change and therefore not this task's; §4.1 is the
  argument for it.~~ **Withdrawn — this was not a debt.** §9.1.
- ~~**`Head::version` as an `Option`,** or an `http::Version`-shaped
  "unknown". Two of four backends have no honest value. Also
  `http-ng-core`'s.~~ **Done.** §9.2.
- **The cross-origin half of §2.1, measured.** Needs a second origin in
  the browser harness — a `wasm-bindgen-test-runner` change.
- **A counting clock for `http-ng-wasi`.** Needs a clock seam on
  `WasiHttp`, declined in §5.2.
- **Anything about a WebSocket.** Neither backend emits an event for one
  and the vocabulary has no word for one; `http-ng-native` made the same
  call.
- **Firefox was run for the behaviour and cost suites and for
  `hooks_timing`; the mutation table was run on Chrome only.** The two
  engines agree on every anchor count and on every assertion, so the
  mutations are expected to score identically, and that expectation is
  untested.
