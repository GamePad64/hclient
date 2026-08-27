# The two invariants, and every exception to them

Two rules hold across this workspace, and both are enforced by CI rather
than by review:

- **No crate declares `Send` or `Sync`** on a seam a backend implements.
  Checked by `scripts/no-send-or-sync-in-the-core-surface.sh`, which
  scans `crates/*/src`.
- **No crate writes `unsafe`.** Every `src/lib.rs` carries
  `#![forbid(unsafe_code)]`, and `scripts/unsafe-code-policy.sh` is the
  backstop for the case where that line goes missing.

Neither rule is absolute, and the exceptions are the point of this
document. Each one is numbered, and **the code must cite its number**:

```rust
pub type SharedTransport = dyn BoxedTransport + Send + Sync; // send-bound-exception: amendment-C12
```

The two families never mix. A `Send` exception cites
`send-bound-exception: amendment-CN`; an `unsafe` one cites
`unsafe-code-exception: amendment-CN`, and is additionally pinned to the
file paths its amendment names — a file drifting into `unsafe` fails the
script rather than inheriting a neighbour's permission.

## Why the rule exists

A `Send` bound on a seam is a demand on every implementor, including ones
this workspace has never seen. `hclient-rt-embassy`'s `connect` future
holds a `RefCell` because embassy's executor is single-threaded, and
always will; a bound on `TcpConnect` would exclude that backend rather
than inconvenience it. The same argument reaches the browser:
`hclient-fetch`'s response body holds a `dyn Stream` with no auto trait.

So a bound is permitted where it is a claim about *a value this
workspace owns* — never where it is a demand on an implementor.

## `Send`/`Sync` exceptions

**C1 — `Error` requires `Send + Sync` from its source.** Erasure into a
`dyn Trait` does not let auto traits through unless the trait object is
bounded, so `Arc<dyn Error>` is `!Send` even when the concrete error is
`Send + Sync`. A library error type that cannot cross a thread is not
usable, so the bound is on `Error::new`'s source. The cost is stated
where it lands: a transport with a genuinely `!Send` error still
implements `Transport`, it simply cannot call the defaulted `to_error`.

**C2 — `RequestBody` bounds its own trait objects.** C1 one level over:
the rewind factory (`Arc<dyn Fn() -> BodyStream>`) and the streaming arm
(`Box<dyn Body>`) are erasures too, so without the bound
`http::Request<RequestBody>` is `!Send` and nothing can spawn a request.

**C3 — `Send`/`Sync` assertions live in `tests/`, not `src/`.** A bare
`fn assert_send<T: Send>() {}` is exactly what the guard's pattern
matches, so an assertion inside `src` needs an exception marker of its
own. Outside `src` it needs none, which keeps the guard's blind spot as
small as the lines that genuinely are exceptions.

**C5 — `hclient_rt::Blocking` declares `Send` in the trait.** Unlike
C1/C2 this is a bound in a declaration rather than one discovered at an
erasure. `Blocking::run` is a bridge to a blocking thread pool —
`getaddrinfo`, file IO — and a thread pool that cannot be handed work
from another thread is not one. It is a runtime *capability* trait, not
a core seam: a runtime without threads does not implement it.

**C10 — the HTTP/3 stack declares `Send` because `quinn::Runtime` does.**
The bound is a third party's, and it lives in `hclient-native`'s `h3`
and `quinn` modules — it was `hclient-h3` and `hclient-quinn` until those
folded in. quinn declares
`Runtime: Send + Sync + Debug + 'static` and takes its driver as
`Pin<Box<dyn Future<Output = ()> + Send>>`; a crate implementing that
trait from outside quinn either satisfies those conditions or does not
implement it. This is the class C10 exists to name — neither erasure nor
a trait of ours.

**C12 — `hclient` declares `Send` on opt-in setters.** A bound this
crate *chooses*, so a caller's own cache store and public suffix list
reach `Client` by erasure rather than by a type parameter. A type
parameter would put `S` on the public `ClientBody` alias, so the arity
of a public alias would change with a feature — and Cargo unifies
features. The bound sits on the opt-in call and nowhere else, so no
signature a caller already writes gains one.

**C14 — the erased response body, sleep and instant declare `Send`.**
C1/C2's class again — a `dyn` lets no auto trait through unless the
object is bounded — applied to `erased::{BoxBody, BoxSleep, BoxInstant}`,
so that a `Response<ClientBody>` handed back by the erased `Client` can
cross a thread. That was lost when `Client` stopped carrying its
transport as a type parameter, and this is what gives it back:
`tokio::spawn` of a response body works again.

**What makes it payable is that every backend here satisfies it**, which
was not true until the browser's body stopped holding a `js_sys::JsFuture`
— measured on all three targets, including `wasm32-unknown-unknown` under
`-Ctarget-feature=+atomics`, where `body::pump` and `timer::Elapsed` hold
no JS handle at all and so need no claim about how many threads there
are. A backend added later must produce a `Send` body and a `Send`
sleep, and that is the cost: it is a promise the seam now makes on every
implementor's behalf.

**It is deliberately not the bound the first erasure attempt was
abandoned over.** That one put `Send` on the boxed *future*, which
propagates down seven seam methods and excludes `hclient-rt-embassy`,
whose `connect` future holds a `RefCell` because its executor is
single-threaded. `BoxExchange` is still unbounded here, so
`Transport::execute` is untouched and `Client::execute`'s future stays
`!Send` — what crosses a thread is what a request *produced*, not the
act of making it.

**Where C12's bound is written matters.** `cargo fmt` moves a trailing
comment off a line it reflows and deletes one from a `where` clause
outright, so a marker cannot survive on a long signature. Bounds of this
kind go on a short named alias — `erased::SharedTransport`,
`erased::SharedTimer` — which fmt has no reason to touch, and every use
site then writes `Box<SharedTransport>` and carries no marker at all.

**C12 is not reusable by gesture.** Citing it means the same argument
applies: a value the caller owns, reaching a facade by erasure, bounded
at the opt-in call. Where a bound is demanded by *someone else's* trait,
the question is which external contract is being satisfied, and that is
C10's shape rather than C12's.

**C15 — an implementor names the auto traits of its own future.** The
seams whose futures a consumer must *prove* `Send` — `TcpConnect`,
`TlsConnect`, `Resolve` — carry an **associated type** rather than an
RPITIT, so the answer is written by each implementor: `Tokio` and `Smol`
write `Pin<Box<dyn Future<..> + Send + 'a>>`, `hclient-rt-embassy` writes
the same without the `Send`, and both satisfy the same trait.

This is the opposite of C1/C2's shape and has to be read as such. There
the bound is *discovered* at an erasure and lands on everybody; here it is
a statement one implementor makes about itself, and the seam demands
nothing — which is the entire reason the associated type exists rather
than a `+ Send` in the trait. A marker is still required at each site,
because the scanner cannot tell the two apart by looking, and telling them
apart is the decision worth recording.

The test fixtures in these crates are the proof that it costs nothing:
`connect.rs`'s `FakeRuntime` keeps a `RefCell` and writes a plain box, and
it is a `TcpConnect` like any other.

## `unsafe` exceptions

The rule: `unsafe` is permitted where Rust has no other way to reach a
platform API this project needs, and only there. Not for performance,
not for convenience, not to avoid a bounds check. The crate swaps
`#![forbid(unsafe_code)]` for `#![deny(..)]`, joins
`scripts/unsafe-code-policy.sh`'s exempt list, and **every** `unsafe`
line in it carries the marker.

**C7 — `hclient-fetch`'s one `unsafe impl`.**
`wasm_bindgen_futures::JsFuture` holds an `Rc<RefCell<..>>` and is
therefore `!Send` — an implementation choice, not a platform property.
`wasm32-unknown-unknown` has no threads, so the impl asserts something
that cannot be observed false there.

**C8 — a foreign-function boundary**, in
`hclient-dns-system/src/sys/res_query.rs` and `.../sys/windows.rs`: one
file per platform backend, and no `unsafe` anywhere else in the crate.

**C9 — the platform's UTS 46**, in `hclient-idn/src/icu/windows.rs` and
nothing else. Not `icu/mod.rs` above it, not `lib.rs`, not a directory.

**C11 — Apple's Objective-C boundary**, in
`hclient-urlsession/src/{delegate,session}.rs`. Every call into
`URLSession` goes through `objc2`'s message send, where the selector,
the argument types and the ownership convention are checked by nothing
the compiler can see — `unsafe` is the medium rather than an
optimisation, so a crate-wide `forbid` would mean no backend. `body.rs`
has none at all, which is the check the file naming exists to make
possible.

**C13 — Core Foundation types an array's elements as raw pointers**, in
`hclient-proxy/src/system/read.rs`. macOS returns the proxy exceptions
list as a `CFArray` of `CFString`, and `core-foundation` implements
`ConcreteCFType` for `CFArray<*const c_void>` alone — so the value can be
downcast to an untyped array and to no other, and its elements arrive as
pointers with no safe way to read one. Checked in 0.9 and 0.10;
`objc2-core-foundation` has the same wall one level up, at the dictionary.

Two things make this narrower than it looks. **The class is checked, not
assumed**: the pointer is borrowed as a `CFType`, whose `downcast` compares
the type id, so an array of something else yields nothing rather than a
string read out of the wrong object. And **skipping the list was weighed
and is worse** — every Mac ships `*.local` and `169.254/16` as defaults, so
a reader that dropped it would silently proxy the traffic its owner
excluded, which is the failure this whole module refuses everywhere else.

The crate carries `#![deny(unsafe_code)]` rather than losing the attribute:
one site, marked, in a file this script names.

**C7 covers two sites now, and the second one added no `unsafe`.**
`SingleThreaded<T>` was written for `promise.rs`'s pair of `Closure`s;
`websocket.rs`'s three ride the same wrapper rather than a claim of their
own, which is what keeps this crate at exactly one `unsafe impl`. The
argument is unchanged and so is its scope — it is about `!atomics`
meaning one thread, and the `cfg` strips it under wasm threads for both
sites at once.

The alternative was checked rather than assumed: `Closure` cannot be
given a `Send` inner `dyn`, because `WasmClosure` is implemented for
`dyn FnMut(..) -> R + 'a` and no other shape (wasm-bindgen 0.2.126,
`convert/closures.rs`). `Closure<dyn FnMut() + Send>` is a type that
exists, satisfies `Send` by auto-derivation, and cannot be constructed.

## One that is neither

**C6 — a `#[non_exhaustive]` type can only be checked for completeness
inside the crate that defines it.** An exhaustive destructure without
`..` is a compile error naming any field added later, and the attribute
forbids that expression from outside the defining crate. So a test that
must fail when a field is added lives in the defining crate; one written
outside it silently keeps passing. `Capabilities` relies on this in two
places, which is why a new field is a compile error twice over until
somebody decides whether it is a gate or a report.
