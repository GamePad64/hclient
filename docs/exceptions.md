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

**C10 covers a second site**, and naming it here rather than minting a
new amendment is C7's precedent: the argument is identical and only the
foreign crate differs. `hclient`'s `RequestBuilder::extension` declares
`T: Clone + Send + Sync + 'static` because that is what
`http::Extensions::insert` demands — a caller either satisfies it or
cannot put a value into an `http::Request` by any route, including the
one they already have through `http::request::Builder::extension`. The
bound is on one opt-in method and reaches no signature a caller already
writes.

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

**C16 — `SendTransport`, and the bound that finally reaches `Client`.**
`erased::BoxExchange` declares `Send`, so `hclient::Client`'s request
future crosses a thread. Whoever boxes into it must *prove* it, and for a
generic transport that means naming every future the transport awaits —
which is why C15's associated types had to come first.

**It is a separate trait rather than a bound on `Transport`, and the
difference is where the bounds may live.** An impl may carry bounds its
trait does not, so `hclient-native` implements this for every `Native`
whose runtime, TLS backend and resolver name `Send` futures, and for no
other. `Native` over `hclient-rt-embassy` remains a `Transport`; it is not
a `SendTransport`. Nothing left the seam.

**The two that cannot make the claim are named where a caller meets
them**, and neither is a shortcoming of this workspace's code:
`hclient-dns-doh` resolves through a generic `C: Transport`, whose
`execute` is an RPITIT with no name.

**What that loses is `hclient::Client` itself, not a spawnable future**,
because `Client::builder` requires this trait — so the cookie jar,
redirects, the cache, decompression, digest auth and SSE go with it.
`Transport` is untouched: `Native::execute` works, and so does everything
built directly on a transport. Measured from outside the workspace, since
nothing in-tree depends on that crate.

**`hclient-tls-native-tls` was on this list and is not any more**, which
is the more useful half of the record: it was excluded for the same
reason — `async_native_tls::TlsConnector::connect` is a `pub async fn` —
and the exclusion was paid off rather than accepted, by owning the
handshake and the stream. See amendment C17.

At a concrete type — which is what a backend is — the body is
`Box::pin(self.execute(req))` and `Send` is *inferred*. Proof is only ever
owed by generic code, which is the asymmetry the whole design rests on.

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
`system-resolver/src/sys/{res_query,android,apple}.rs` and
`system-resolver/src/sys/windows/{raw,parsed}.rs`: one file per platform
call, and no `unsafe` anywhere else in the crate. Android is a backend of
its own rather than a `cfg` on the first, because bionic's `res_*` family
is not in the NDK's stable ABI — `android_res_nquery` is, and it is what
the platform's own resolver work goes through.

**Apple is a backend of its own for a measured reason rather than a
declared one.** It shared `res_query.rs` until that call was run on a Mac:
the same query answers 64/64 serially and 12/64 from eight threads, so the
resolver state there is shared rather than per-thread, and a caller on a
blocking pool was most of the way to failing. `apple.rs` calls
`DNSServiceQueryRecord` instead — which also reaches the resolvers a VPN
installs, where `res_9_query` does not.

**Windows is two files rather than one**, and the split is what makes the
naming rule pay here: `raw.rs` reaches `DnsQueryRaw` and `parsed.rs`
reaches `DnsQuery_UTF8`, chosen at run time by `windows/mod.rs` — which
has **no** `unsafe` in it at all and is not on this list, so the choice
between two foreign calls is itself ordinary safe code.

**This lived in `hclient-dns-system` until the platform calls moved.**
That crate is an adapter now — records in, endpoints out — and carries no
`unsafe` and no `deny`; it is back to the workspace's `forbid`. An
amendment naming a file that has moved is the shape this project treats
as worse than no amendment at all, which is why the exempt list is paths
rather than crates.

**C9 — the platform's UTS 46**, in `hclient-idn/src/icu/windows.rs` and
nothing else. Not `icu/mod.rs` above it, not `lib.rs`, not a directory.

**C11 — Apple's Objective-C boundary**, in
`hclient-urlsession/src/{delegate,session,websocket}.rs`. Every call into
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

**C19 — Android keeps two things behind a JVM**, and the amendment
covers both files that reach for them: `hclient-proxy/src/system/jvm.rs`
and `hclient-idn/src/android.rs`.

The second is UTS 46. `android.icu.text.IDNA` has shipped with the
platform since API 24 and is the same ICU `hclient-idn` calls through
`icuuc.dll` on Windows — the same option bits, the same error names — but
the NDK exposes no C entry point for it, so JNI is the only way in. The
`unsafe` is the same single line as the proxy reader's,
`JavaVM::from_raw` over the `ndk_context` pointer, null-checked rather
than trusted, and every other line is `jni`'s safe API. The split is the
same too: the file holds no decision, because the one decision — which of
ICU's errors this crate forgives — is `IGNORED` beside a test that pins
it against the Windows backend's bit mask on any host. **What it does not
have is a run**: no runner in this project is an Android device, so the
file type-checks for `aarch64-linux-android` in `just check-targets` and
has never been executed, which is stated in its own module doc rather
than left to be discovered.

The first is the proxy, in
`hclient-proxy/src/system/jvm.rs` and in one function. Android has no
environment variable for a proxy and no registry: what it has is
`System.getProperty("http.proxyHost")` and four neighbours, which the
framework fills in from the active network and which `java.net`'s own
`DefaultProxySelector` reads — so reading them is what makes this client
agree with every other one in the process. Reaching them means calling
into managed code, and `JavaVM::from_raw` over the pointer the
application registered with `ndk_context` is the whole of the `unsafe`;
the pointer is null-checked rather than trusted, and every other line in
the file is `jni`'s safe API. **The file holds no rules** — which key is
which scheme, how `nonProxyHosts` splits, what a missing port means are
all in `read.rs`'s `from_jvm_properties`, a pure function tested on this
workspace's own Linux hosts. That is C8's and C13's split kept for a
third platform.

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

**C17 — `hclient-tls-native-tls` bridges a synchronous TLS stack.**
`native-tls` fronts SChannel, Security.framework and OpenSSL through one
**synchronous** `Read`/`Write` interface and answers
`HandshakeError::WouldBlock` when the transport underneath is not ready.
Bridging that to a poll-based world means handing the synchronous side a
`Read`/`Write` that can reach the current task's waker, which is
`stream.rs`'s `StdAdapter`: a raw `*mut ()` holding the `Context` for the
length of one call, plus the two `unsafe impl`s that say the pointer does
not make the type less `Send`/`Sync` than the stream it wraps.

**Bounded three ways, and each is checkable.** The pointer is set
immediately before a call into `native-tls` and cleared immediately after
— by a `Guard` whose `Drop` runs on the unwinding path too, so a panic out
of the platform stack cannot leave a dangling one. `with_context` asserts
it is non-null rather than trusting the invariant, which turns a violation
into a panic instead of a use-after-free. And it is null at every instant
a value could be observed from anywhere but the call that set it, which is
what the two `unsafe impl`s rest on.

**Why the sans-io shape was not available.** `hclient-tls-rustls` needs
none of this: rustls is sans-io, so its handshake is a loop over buffers
this workspace owns and there is no `Context` to smuggle. That is the
difference between the two TLS backends, not a difference in care.

**What it replaced and what it bought.** The crate was a wrapper over
`async-native-tls`, whose connector is a `pub async fn` — a future with no
name, which `TlsConnect::Handshake` being an associated type (C15) made
unusable, and which cost this backend `hclient::Client` entirely. Driving
the handshake by hand was not enough on its own, because
`async_native_tls::TlsStream::new` is `pub(crate)` and its own adapter is
private, so the stream had to be owned too. Owning it gave back `Client`
**and** `reports_alpn`, since `native_tls::TlsStream::negotiated_alpn` is
public where the wrapper's was not — a limitation this crate documented as
concrete for two verticals and which turned out to be the wrapper's.
Measured: the graph fell from 66 crates to 32.

**C18 — WinHTTP's asynchronous model is C callbacks and a lent
buffer**, in `hclient-winhttp/src/sys.rs` and nowhere else in the crate.
`session.rs`, `body.rs` and `lib.rs` have none at all, which is the check
the file naming exists to make possible — the split `hclient-dns-system`
already draws between `sys/` and its parsers.

Three obligations WinHTTP places on a caller, none of which Rust can
express, and each made structural rather than disciplinary where it
could be:

**The read buffer belongs to WinHTTP between `WinHttpReadData` and
`READ_COMPLETE`.** The buffer is an enum, `Home(BytesMut)` or
`Loaned { held: BytesMut }`, and the hand-off is a pointer into the
buffer's **spare capacity** that `read` computes, gives to WinHTTP and
keeps none of. Safe code above this file cannot read bytes WinHTTP is
still writing because `take_read` refuses the `Loaned` arm, and the
allocation cannot move or be reallocated because nothing between the two
touches `held`.

**This paragraph used to describe a `Box<[u8]>` given away with
`Box::into_raw` and taken back with `Box::from_raw`**, and the argument
was that the value safe code would read through did not exist. That was
stronger, and it was paid for with a copy of every chunk into a fresh
`Bytes` — a `Box` cannot be split. The `BytesMut` version removes the
copy and, with it, **two `unsafe impl`s**: `Exchange` needed
`Send`/`Sync` by hand only because `Inner` held a raw pointer, and it
holds none now, so the auto impls apply. `reclaim` lost its `unsafe`
too. What arrived in exchange is one `advance_mut`, asserting that
WinHTTP wrote the `n` bytes it reported — bounded by what was lent
rather than trusted. Fewer `unsafe` sites in a crate not one line of
which has ever been executed, which is where they were least
affordable.

**The context is a `usize` the callback is handed back.** It is
`Arc::into_raw` of the shared state, installed with
`WinHttpSetOption(WINHTTP_OPTION_CONTEXT_VALUE)` **immediately after
`WinHttpOpenRequest`** rather than passed to `WinHttpSendRequest`, so
every failure path afterwards still reaches `HANDLE_CLOSING` with a
reference to release. That is the second obligation: `HANDLE_CLOSING` is
the last callback a handle ever receives, and it is where the `Arc` is
reclaimed — and a still-lent buffer with it, so the design leaks nothing
whether or not a cancelled read reports an error first.

**The request body pointer must outlive the send.** It is a `Bytes` held
in the shared state until `SENDREQUEST_COMPLETE`: heap-stable and
immutable, which is the whole of what that needs. Headers avoid the
question entirely — `WinHttpAddRequestHeaders` is synchronous and copies,
so `WinHttpSendRequest` is passed a null header pointer.

**What is not verified.** No line of this crate has been run: there is no
Windows machine here, so the three obligations above are read from
WinHTTP's documentation rather than observed. They are stated at the
sites that depend on them for exactly that reason. `cargo check --target
x86_64-pc-windows-msvc --all-targets` is the whole of what this
workspace can say about it today.

## One that is neither

**C6 — a `#[non_exhaustive]` type can only be checked for completeness
inside the crate that defines it.** An exhaustive destructure without
`..` is a compile error naming any field added later, and the attribute
forbids that expression from outside the defining crate. So a test that
must fail when a field is added lives in the defining crate; one written
outside it silently keeps passing. `Capabilities` relies on this in two
places, which is why a new field is a compile error twice over until
somebody decides whether it is a gate or a report.
