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

**C10 — `hclient-h3` declares `Send` because `quinn::Runtime` does.**
The bound is a third party's. quinn declares
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

## One that is neither

**C6 — a `#[non_exhaustive]` type can only be checked for completeness
inside the crate that defines it.** An exhaustive destructure without
`..` is a compile error naming any field added later, and the attribute
forbids that expression from outside the defining crate. So a test that
must fail when a field is added lives in the defining crate; one written
outside it silently keeps passing. `Capabilities` relies on this in two
places, which is why a new field is a compile error twice over until
somebody decides whether it is a gate or a report.
