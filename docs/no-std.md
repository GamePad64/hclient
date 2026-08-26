# `no_std`: what it would take, measured

`AGENTS.md` has said for four verticals that bare-metal is out and that
the obstacle is `http` 1.x's `compile_error!`. That is true and it is not
the whole answer, because it names one blocker and there are four. This
document is the measurement: every claim below was produced by building
something for `riscv32imac-unknown-none-elf` — a real bare-metal target
with no `std` at all — rather than by reading a manifest.

The constraints the question came with, and they are kept throughout:
**async stays** (async works fine on `no_std`; `embassy` is the proof and
this workspace already has a runtime for it), and **`alloc` stays
mandatory**. Nothing below argues for a `heapless` rewrite.

## The headline

**Four crates compile for bare metal today**, once `http` does — and they
are every crate in this workspace that can, which is measured below rather
than asserted:

```
cargo check -p hclient-core -p hclient-proto -p hclient-dns -p hclient-mock \
    --no-default-features --target riscv32imac-unknown-none-elf
    Finished `dev` profile
```

All four are `#![no_std]`, the workspace's **1755 tests** still pass on the
host, and every `just` gate — `docs`, `invariants`, `test-no-default`,
`packaging`, `features`, `graph`, `test-doc` — is green. The one thing that
build needs and cannot have from crates.io is a `no_std` `http`, supplied
in the spike by a patched checkout (see [Reproducing](#reproducing)).

The first three are what this workspace's own device path names —
`Native<Embassy, NoTls, IpLiteralOnly>`, with `IpLiteralOnly` in
`hclient-dns`. **Everything left over is blocked by `hyper`, nine crates of
it, and by one third-party crate that simply lacks the attribute.** The
section on
[what can carry the attribute today](#what-can-carry-the-attribute-today--and-what-the-attribute-actually-guards)
has the table and, more usefully, what the attribute does and does not
guard.

What made that cheap is a property nobody had written down: **there is
not one genuinely-`std` item in either crate's library code.** No
`HashMap`, no `std::io`, no `Mutex`, no `SystemTime`, no thread. The
single `Instant::now()` in the tree lives inside a `#[cfg(test)]` module
in `sse/lines.rs`. Everything else — `Arc`, `Rc`, `VecDeque`, `Cow`,
`SocketAddr`, `IpAddr`, `Error` — moved to `alloc::` or `core::` with no
change of meaning. `Timer::Instant` being an associated type rather than
`std::time::Instant` is what paid for that, years before this question
was asked.

The dependencies were the same story. Every crate `hclient-proto` names
already builds `no_std + alloc`: `bytes`, `futures-core`, `futures-sink`,
`percent-encoding`, `form_urlencoded`, `base64`, `winnow`, `thiserror` 2.
The whole of the manifest work is `default-features = false` and an
`alloc` feature where one exists.

So the shape of the answer is not "port the client". It is: **fix four
things underneath it, then write one new transport.**

## The four blockers, in order of how hard they are

### 1. `http` 1.x — external, unowned, and the one that gates everything

`http` 1.5 still carries the commented-out `#![cfg_attr(not(feature =
"std"), no_std)]` and the `compile_error!` beside it. `http::{Request,
Response, HeaderMap, Uri, Method}` are in the public API of ten crates
here, `hclient-proto` included, so nothing moves until this does.

**The good news is how little std is actually in there.** Measured across
`http` 1.5's `src/`, the genuinely-`std` surface is three items:

| site | what | the `no_std` answer |
|---|---|---|
| `extensions.rs` | `HashMap<TypeId, …>` with a custom `IdHasher` | `hashbrown` |
| `header/map.rs` | `RandomState`, for the HashDoS `Danger::Red` state | needs a seed with no OS to ask |
| everywhere | `std::error::Error` | `core::error::Error`, stable since 1.81 |

Everything else that spells `std::` in that crate is `core` or `alloc`
under another name.

**The bad news is that upstream has not moved in five years.** Issue
[#551](https://github.com/hyperium/http/issues/551) has been open since
May 2022; there are **five open PRs and one closed** attempting it —
[#472](https://github.com/hyperium/http/pull/472) (Mar 2021, draft),
[#563](https://github.com/hyperium/http/pull/563) (Jul 2022),
[#732](https://github.com/hyperium/http/pull/732) (closed),
[#740](https://github.com/hyperium/http/pull/740) and
[#749](https://github.com/hyperium/http/pull/749) (both Jan 2025). None
is merged. **Waiting is not a plan**, and the honest reading is that this
is the same class of refusal as
[hyper#3428](https://github.com/hyperium/hyper/pull/3428) — a maintainer
being careful about a feature that can never be removed, rather than a
technical objection.

**#749 is the closest thing to a starting point and it does not build.**
It is 22 files, +517/−220, and its approach is right: unconditional
`#![no_std]`, mechanical `std` → `core`/`alloc` renames, `hashbrown` and
`BTreeMap` replacing the `TryFrom<&HashMap>` impls. Building it for
`riscv32imac-unknown-none-elf` took three fixes it does not have:

- `bytes` and `fnv` are declared without `default-features = false`, so
  both drag `std` in regardless;
- `src/extensions.rs:4` still says `use std::hash::Hasher;`;
- its base is January 2025 — 52 commits and one release behind, and this
  workspace already calls `Method::QUERY`, which arrived in `http` 1.3,
  after it. Merging `master` into it conflicts in **5 of 22 files**.

With those three fixed it compiles. That is the measurement: **a rebase
and a day, not a rewrite** — but it is a rebase somebody has to own,
because five people have now written this patch and none of them got it
merged.

**The `RandomState` question is the only design decision in it**, and it
is worth deciding rather than defaulting. On a device there is no
`RandomState::new()` and the HashDoS threat model is different: a header
map on an MCU is fed by a server the device chose to talk to, over a
connection it opened, and the attack `Danger::Red` defends against is a
malicious peer sending thousands of colliding header names. A fixed seed
is the wrong answer to state silently; a seed the application supplies
from the same hardware RNG that feeds TLS is the right one, and it is the
shape this workspace already uses for `critical-section` and for
`getrandom`'s custom backend — a contract the application fills in.

**Three options, and the recommendation is the second.** Land it upstream
(best outcome, unbounded schedule, five failed attempts). **Carry a
patched `http` as a `[patch.crates-io]` in the application's own lock
file, and publish nothing** — which is exactly what `hclient-proto`'s
manifest already documents for `idna_adapter`: *"an application's decision
to make in its own lock file, not something a library can express."* Or
fork and publish, which costs the family a name it does not want and
splits `HeaderMap` in two — a `http`-typed and an `hclient-http`-typed
`Request` do not convert, and every crate here has one in its signature.

That third option is not as bad as it reads here, and it has a section of
its own below: the split can be confined to the target that has no other
`http` in its graph, and it costs **zero source lines** to do it. See
[Re-exporting `http`](#re-exporting-http-real-crate-on-std-fork-otherwise),
which was built rather than argued.

### 2. hyper — and it is not one blocker but two

`hyper` is `std` throughout: 32 uses of `std::io::Error`, and its
`rt::Read`/`rt::Write` traits return `std::io::Result`. It cannot be made
to work here and there is no PR to wait for.

It sits in the way twice, and the two halves cost very differently.

**The seam.** `hclient-rt`'s `TcpConnect::Stream` is declared
`hyper::rt::Read + hyper::rt::Write + Unpin`, and `hclient-tls`'s
`TlsConnect::Stream<S>` says the same. So the IO type of the whole
workspace is hyper's, and `std::io::Result` appears **52 times** across
`hclient-rt`, `hclient-tls` and `hclient-dns`. The natural replacement
already exists and this workspace is already next to it:
`embedded-io-async::{Read, Write}` is what `embassy-net` implements, what
`embedded-tls` speaks, and what `reqwless` is written against — and
`hclient-rt-embassy/src/io.rs` exists **only** to bridge embassy's
`embedded-io-async` socket back into hyper's traits. Its own module doc
records the impedance mismatch (`embedded_io_async::Write` has no
shutdown). A `no_std` build deletes that adapter and uses the underlying
trait directly.

**The engine.** HTTP/1 framing in this workspace *is* hyper —
`hclient-native/src/http1.rs` is 1,424 lines of driving
`hyper::client::conn::http1`, and the crate as a whole is 21,176 lines
with `hyper::` in nine files. None of that is reusable. A `no_std`
transport needs its own HTTP/1 codec over `httparse` (which is `no_std`
already), and the sans-io half of that has a home: `hclient-proto`, whose
whole reason to exist is state machines with no IO. That is the largest
single piece of new code in this whole exercise, and it is also the one
piece that would pay back on the `std` side, because a sans-io HTTP/1
codec is a thing this workspace has never had.

**What such a transport does not need to carry** is most of what
`hclient-native` does: no Happy Eyeballs worth the name on a device with
one interface, no HTTPS/SVCB discovery, no proxy, no HTTP/2, no HTTP/3,
no pool of any size worth the word. `crates/hclient-native/examples/
minimal.rs` already names the target audience for that; this is that
example's logical end point.

### 3. Our own code: two items, both found by the compiler

Neither was visible from reading, and both are the kind of thing that
would have been discovered late.

**`ConnectionId`'s counter is `AtomicU64`** (`hooks.rs:218`), and a
32-bit target has no 64-bit atomics — not on `riscv32imac`, not on any
`thumbv7em`. The fix is `portable-atomic` with `critical-section` **and**
`fallback` (the second is a default feature, so `default-features =
false` silently removes the very thing that makes 64-bit work — measured,
after it failed). `critical-section` is the same `#[no_mangle]`
application-supplied contract `hclient-rt-embassy`'s manifest already
documents at length, so the cost is a dependency and no new concept. The
alternative — narrowing the counter to `AtomicU32` — is cheaper and
changes a public type.

**`f64::round()` is a `std` method** (`backoff.rs:145`). Float maths lives
in `std`, not `core`; `core` has the arithmetic and none of the
functions. One call site, and it is `(x + 0.5) as u128` for a
non-negative value, which is `round` exactly. The general answer is
`libm`; here it is one line and no dependency.

The lesson is worth keeping: *these two were the only surprises in 6,400
lines of "portable" code, and neither was findable by grepping for
`std::`.*

### 4. Everything above the transport — and this one is a scope decision

`hclient::Client` is 13,762 lines and is **not** the `no_std` target, for
reasons that predate the question. `Client` boxes its transport
`Send + Sync`, which is why `AGENTS.md` records that the erased `Client`
already excludes `Embassy` — `RefCell<embassy_net::Inner>` is not `Sync`.
The embedded path in this workspace has been `Transport` directly since
that change, with the nine TAP scenarios written against it, and that
stays true.

If it were ever wanted, the bill is: `SystemTime::now()` at three sites
(the cookie jar's clock, the cache's, and `Date` arithmetic),
`getrandom` 0.4 as a **non-optional** dependency, and `flate2`, `serde`
and `serde_json` pinned to `std` in the manifest. All of them are
feature-shaped rather than structural, and none of them is worth doing
before there is a transport to put underneath.

## TLS: the biggest unknown, and it came out well

This was the part that could have made the whole thing not worth
starting, so it was probed first and directly. **rustls 0.23 is already
`#![no_std]`** — its `std` is a feature, not an assumption — and a real
`ClientConfig` builds for `riscv32imac-unknown-none-elf`:

```rust
rustls::ClientConfig::builder_with_details(
    Arc::new(rustls_rustcrypto::provider()),
    Arc::new(FixedTime),          // no SystemTime; the device supplies one
)
.with_safe_default_protocol_versions()?
.with_root_certificates(roots)    // webpki-roots builds too
.with_no_client_auth()
```

`cargo check --target riscv32imac-unknown-none-elf` — clean. So the
existing `TlsConnect` seam survives, and no second TLS backend is owed;
what changes is the *provider*, not the shape.

Three things that probe established and one caveat it also established.

**`ring` cannot come along.** `hclient-tls-rustls` picks `ring` today,
which needs a C toolchain and supports a fixed target list; bare-metal
RISC-V is not on it. The provider has to be pure Rust, which is the same
argument this workspace already made for `flate2`'s `rust_backend` and
for `ruzstd`.

**Entropy is a first-class problem, not a detail.** The build failed
first on `getrandom`: *"target is not supported"*. On a device there is no
OS to ask, and the answer is `getrandom`'s custom backend — another
`#[no_mangle]` contract the application fills from a hardware RNG. Worth
noting that `hclient` itself already depends on `getrandom` 0.4
unconditionally, for SSE jitter, the multipart boundary and the digest
`cnonce`, and that this file's own rule about degraded values (*a degraded
value is only acceptable when the degradation has a direction*) becomes
much sharper on a device where the RNG may not be ready at boot.

**Certificate validity needs a clock the device owns.** `TimeProvider` is
a caller-supplied trait in rustls, so this is a slot rather than a
problem — but a device with no RTC and no NTP has nothing true to put in
it, and that is a deployment decision rather than a code one.

**The caveat: `rustls-rustcrypto` is `0.0.2-alpha`, last published April
2024.** It works, and its own README says not to use it in production —
only a subset of suites, no formal verification. It also brings **73
crates** and, through `p256`/`p384`/`ed25519`/`num-bigint-dig`, a lot of
flash for a part that will only ever talk to one server. The lean
alternative is `embedded-tls` (0.19, June 2026, 140k downloads): TLS 1.3
only, P-256 only, `no_std` and **no alloc**, and used by `reqwless`. It
would not fit the `TlsConnect` seam without a second backend crate,
which is a real cost against a real saving — that trade needs measuring
in bytes of flash before it is decided, and nothing here has measured it.

## Which device, exactly — because the answer differs

The target matters more than it looks, and one measurement makes the
point. The first probe was built for `riscv32imc-unknown-none-elf` — the
ESP32-C3 — and `bytes` did not compile: `no method named compare_exchange
found for &Atomic<*mut ()>`. **`imc` has no atomic CAS at all.** That is
not a `bytes` bug; it is the chip.

| part | target | atomics |
|---|---|---|
| ESP32-C3, ESP32-C2 | `riscv32imc` | **no CAS** — everything needs `portable-atomic/critical-section` |
| ESP32-C6, ESP32-H2 | `riscv32imac` | CAS yes, 64-bit no |
| ESP32, ESP32-S3 | `xtensa-*` | needs the Espressif rustc fork |
| Cortex-M3/M4/M7 | `thumbv7em` | CAS yes, 64-bit no |
| Cortex-M0/M0+ | `thumbv6m` | **no CAS** |

So `portable-atomic` is not a workaround for one counter; on half the
interesting parts it is a whole-graph requirement, and `bytes` — which is
in every signature here — is one of the crates that needs it. An `esp32`
answer and a `stm32` answer are the same answer; an `esp32-c3` answer has
an extra chapter.

And on any of them, a device with `std` is **already reachable** and owes
nothing: an `esp-idf` target runs `Native<Embassy, NoTls, IpLiteralOnly>`
today, with the TAP scenarios in CI. This whole document is about the
parts that have no `std` at all.

## What it adds up to

In rough order, with what each depends on:

1. **A `no_std` `http`.** Rebase #749 onto 1.5, fix its three build
   defects, decide the `RandomState` seed. Blocks everything. A day of
   work and an unbounded amount of politics.
2. **A `no_std` `http-body`.** Four lines — `#![no_std]`, `extern crate
   alloc`, two prelude imports, `default-features = false` on `bytes` and
   `http`. Measured, in the spike. Upstream would probably take this one.
3. **Every crate here that can be `no_std`** — `hclient-core`,
   `hclient-proto`, `hclient-dns`, `hclient-mock`. **Done**, in the branch
   this document ships on: `portable-atomic` for the counter, `round()`
   for the float, a named error where `io::Error` was boxing a string,
   `spin::Mutex` where `core` has none, mechanical renames for the rest.
   The attribute guards our own source from a host build, so this cannot
   rot while item 1 waits. **It is item 4 that unblocks the next crate,
   not more marking.**
4. **The IO seam.** `TcpConnect`/`TlsConnect` off `hyper::rt` and onto
   `embedded-io-async`, and `std::io::Error` out of 52 sites. This is the
   change with the widest blast radius inside the workspace — seven files
   implement `TcpConnect`, six implement `TlsConnect` — and it is the one
   that needs a decision about whether the `std` backends keep hyper's
   traits behind a feature or move too.
5. **A sans-io HTTP/1 codec in `hclient-proto`,** over `httparse`. The
   largest new piece, and the only one that pays back on the `std` side.
6. **`hclient-embedded`,** a transport over 4 and 5: connect, exchange,
   no pool, no discovery, no proxy. `reqwless` is the shape to read for
   what an embedded HTTP client actually needs.
7. **TLS provider,** `rustls` + a pure-Rust provider, or `embedded-tls`
   behind a second backend. Measure the flash first.

**Items 2, 3 and 7 are measured and green. Item 1 is measured and
blocked. Items 4, 5 and 6 are the work.**

The honest summary is that `no_std` here is not a research question any
more — it is a rebase somebody has to own, a seam change, and one codec.
What it is *not* is a rewrite, and the reason is that the two crates that
would have been hardest to port turned out to need nothing but renames.

## What can carry the attribute today — and what the attribute actually guards

This document's first version said the `#![no_std]` on `hclient-core`
*"has no guard and can rot"*, because the cross build needs a patched
`http` and cannot be a CI job. **That was wrong, and the correction is the
reason to mark crates now rather than later.**

`#![no_std]` removes `std` from **this crate's** extern prelude. A `std::`
path added to a marked crate is `E0433: unresolved module or unlinked
crate` on an ordinary host `cargo check` — no cross-build, no patched
`http`, no new CI job. Checked in the failing direction rather than
assumed: adding `pub fn probe() -> std::collections::HashMap<u8, u8>` to
`hclient-core/src/host.rs` fails, twice, on the plain workspace build.

So the attribute is two claims and only one of them is unguarded:

| claim | guarded today | by what |
|---|---|---|
| *this crate's source reaches for nothing in `std`* | **yes** | rustc, on every host build |
| *this crate's dependencies can all go `no_std`* | no | only the cross build, which `http` blocks |

That split is what makes marking worth doing before `http` moves: the
half that would otherwise rot — a hundred `std::` paths creeping back in
over a year of unrelated work — is exactly the half the compiler already
holds.

### Four crates are marked, and the list is not a preference

`hclient-core`, `hclient-proto`, `hclient-dns` and `hclient-mock`. The
first three are what this workspace's own device path names —
`Native<Embassy, NoTls, IpLiteralOnly>`, where `IpLiteralOnly` lives in
`hclient-dns`. The fourth is there because the measurement said so rather
than because anybody wanted it there; see below.

`hclient-dns` cost one real change beyond renames. `IpLiteralOnly`'s
refusal was built with `std::io::Error::other(format!(..))` — `io::Error`
used as a box for a string, with no I/O anywhere near it and nothing
downstream ever downcasting to it. It is a named `NotALiteral` now, which
is a better error either way and is what lets the crate be `no_std`:
`Error::new` wants `core::error::Error + Send + Sync`, which `io::Error`
is not without `std`.

The `codec` feature is where that crate stops: `dns-message-parser` reaches
`hex`, which is `std`, and the bare-metal build fails there. That is the
documented behaviour rather than a defect — `codec` is the feature a
device turns off, and a build resolving through `IpLiteralOnly` decodes no
DNS messages. Both settings were built for
`riscv32imac-unknown-none-elf`: without `codec`, green; with it, `hex`
fails, loudly.

`hclient-mock` cost a **dependency**, and it is the one judgement call in
this change. Its queue and request log sit behind a mutex, and the module
doc has always said why that is not a `RefCell`: a `RefCell` makes
`MockTransport` `!Sync`, which makes `Client::execute`'s future `!Send`,
and that property is one this crate exists to let tests check. `core` has
no mutex at all, so the choice was `spin::Mutex` or leaving the crate
behind. `spin` with `default-features = false` has **no dependencies of
its own**, and the swap costs **poisoning** — `std`'s mutex refuses the
lock after a panic held it, a spinlock does not. For a test double that
loss is nil: a panic inside one of these critical sections *is* a failed
test, and the harness reports it. The thirteen
`.expect("mock lock poisoned")` calls are gone with it.

### The two that cannot be marked, and it is not discipline

`hclient-rt` and `hclient-tls` are also on the device path, and both are
**structurally** blocked rather than merely unported. `TcpConnect::connect`
returns `std::io::Result<Self::Stream>` and `TcpConnect::Stream` is bounded
on `hyper::rt::Read + hyper::rt::Write`; `TlsConnect::Stream<S>` says the
same. Those are public seam signatures, not implementation details, so no
amount of renaming reaches them.

The attribute would in fact *compile* on them — `#![no_std]` does not
forbid depending on a `std` crate, only writing `std::` — but only with an
`extern crate std;` at the top to get `io::Result` back, which is the
attribute and its own contradiction on the same page. They move when the
IO seam moves, which is item 4 of the plan above.

### Everything else, and the blocker is named rather than guessed

The first sweep measured *distance* — how many `std::` paths and prelude
items a crate's own code would need. That turned out to be the wrong
number, because it says nothing about whether the crate could ever link.
The second one asks the question that decides it: with the `http` overlay
applied, cross-compile every crate for `riscv32imac-unknown-none-elf` and
record what cargo could not build.

| crate | what stops it |
|---|---|
| `hclient-core`, `hclient-proto`, `hclient-dns`, `hclient-mock` | **nothing — they build** |
| `hclient-rt`, `hclient-rt-tokio`, `hclient-rt-smol`, `hclient-rt-embassy`, `hclient-tungstenite` | `hyper`, surfacing as `futures-io`'s `std` feature |
| `hclient-tls`, `hclient-tls-rustls` | `hyper`, surfacing as `bytes` pulled with defaults |
| `hclient-native`, `hclient-tls-native-tls`, `hclient-webtransport` | `hyper`, surfacing as `futures-core`'s `std` feature |
| `hclient-tower` | **`tower-service`** — 0.3.3 has no `#![no_std]` of its own, and that is the whole of it |
| `hclient-wasi` | its wasm bindings, and the target is `wasm32-wasip2` regardless |
| `hclient-fetch` | `wasm-bindgen`, likewise |
| `hclient-idn` | the platform UTS 46 backends — and `idn` is the feature a device turns **off** |
| `hclient-dns-doh`, `hclient-dns-hickory`, `hclient-dns-system` | their transports; a device resolves with `IpLiteralOnly` |
| `hclient-urlsession` | its own source, and it is Apple-only by `#![cfg]` |
| `hclient` | the facade, which is not the device target |

**So the answer to "which crates can be `no_std`" is four, and it is four
because of `hyper` rather than because of us.** Nine of the rows above are
one dependency, and it is the same one. Removing it is item 4 and item 5,
not a marking exercise.

One row of the first sweep was an artifact and is worth recording as one:
`hclient-urlsession` came back **clean with zero edits**, which looked like
a finding and is not — its crate root carries
`#![cfg(target_vendor = "apple")]`, so on the Linux runner the whole crate
compiles to nothing. *A counter that cannot move is not evidence*, and
checking which ones the harness actually feeds is part of reading the
result.

And one gate earned its keep during this change, which is worth a line
because this file argues about gates constantly: `just test-no-default` —
the recipe recorded here as having once printed `error:` and exited zero —
caught `hclient-proto`'s test prelude carrying a `ToOwned` that is live
with `idn` and dead without it. Nothing else in the suite looks at that
combination.

### What is still owed, and cannot be paid today

Nothing notices a **dependency** that cannot go `no_std`. `hclient-core`
gained `portable-atomic` for the counter; if it gained something `std`
tomorrow, the host build would stay green and only the cross build would
object — and the cross build cannot run in CI until `http` moves. That is
one gap, it is named, and inventing a weaker check for it (a `cargo tree`
grep, say) would be the mirror of this file's argument against an MSRV
job: a second, staler statement of a promise, and the one people would
trust.

## Re-exporting `http`: real crate on `std`, fork otherwise

The question asked after the first pass, and it has a better answer than
the one this document opened with. **Both shapes work.** They were built
rather than reasoned about — `spikes/nostd/shim-probe/`, four crates, and
the interesting half is the failure modes.

### Shape A — a feature and a re-export

`hclient-core` depends on two packages under two rename keys and
re-exports one of them:

```toml
[features]
default = ["std"]
std   = ["dep:http-std",   "dep:http-body-std"]
nostd = ["dep:http-nostd", "dep:http-body-nostd"]

[dependencies]
http-std        = { package = "http",              version = "1.5", optional = true }
http-nostd      = { package = "hclient-http",      version = "0.1", optional = true }
http-body-std   = { package = "http-body",         version = "1.1", optional = true }
http-body-nostd = { package = "hclient-http-body", version = "0.1", optional = true }
```

```rust
#[cfg(feature = "std")]
pub use {http_std as http, http_body_std as http_body};
#[cfg(all(feature = "nostd", not(feature = "std")))]
pub use {http_nostd as http, http_body_nostd as http_body};
```

**It compiles both ways, and each arm resolves only its own pair** — a
host build never downloads the fork, a device build never downloads
`http`. Verified with `cargo tree` on both.

**The consumer's source is byte-identical between the arms.** That is the
number that matters: 61 files here name `http::Foo` about 800 times, and
the swap costs **one `use hclient_core::{http, http_body};` per file**, not
800 rewrites — an item imported under the name `http` shadows the extern
prelude crate for everything below it.

**The forked crates need no source edits either.** `http-body` uses
`http::{Request, Response, HeaderMap}`, so the fork is a *family* rather
than a crate; `hclient-http-body` is `http-body` with one line changed in
its manifest —

```toml
[dependencies.http]
package = "hclient-http"
```

— and not a line changed in its code.

The probe crosses the seam where it actually matters rather than at a bare
re-export: `Transport::Body: http_body::Body<Data = Bytes>` with a body of
the consumer's own, and `Frame::trailers(http::HeaderMap::new())`, which
only compiles when the two forks agree about `http`. Green on
`riscv32imac-unknown-none-elf`.

**The hazard is the one this file already has a name for.** Cargo's
features are additive, so `std` and `nostd` are not alternatives — they can
both be on, and then **all four packages land in the graph and `std`
silently wins**. Measured. On a host that is harmless; on a device the
build then dies deep inside `bytes` with `can't find crate for std`, a
message pointing at the wrong crate entirely. *A default is not a default
when Cargo unifies features — it is a floor*, and here the floor is `std`.
So this shape is only honest with a `compile_error!` on the collision,
naming it.

It also needs a discipline rule — *never name `http` directly* — which is
grep-checkable and would join the other `just invariants` scripts.

### Shape B — swap by target, with no feature at all

Better, and it is the rule this workspace already states one level up:
`DefaultTransport` resolves *"by target, not by a feature the user picks."*

```toml
[target.'cfg(not(target_os = "none"))'.dependencies]
http = { workspace = true }

[target.'cfg(target_os = "none")'.dependencies]
http = { package = "hclient-http", version = "0.1" }
```

Two tables, one key, disjoint conditions. **Zero source lines** — not even
the `use`: the name `http` is bound by the manifest, so all ~800 sites are
untouched, in crates that never learn the swap exists. And there is no
feature to unify, so Shape A's hazard has no subject.

**Cargo refuses this when the two sides differ in *source kind*** — which
is exactly what a local spike looks like, and is worth knowing before
concluding the shape is illegal:

```
Dependency 'http' has different source paths depending on the build target.
Each dependency must have a single canonical source path irrespective of
build target.
```

That is a `path` dependency against a registry one. With **both from the
registry — the published world — it is legal**, and resolves per target:
the probe's host build takes `itoa` and its
`riscv32imac-unknown-none-elf` build takes `ryu`, from one unchanged
source file.

Two mechanical notes. `[workspace.dependencies]` inherits **by key**, so
the fork side cannot be inherited under a rename — `workspace = true` with
a `package` key fails with *"`dependency.swapped` was not found in
`workspace.dependencies`"*. The hybrid above is what works: the std side
inherits, the fork side restates one version literal, per crate. Thirteen
manifests, about six lines each. And `cargo package`'s verify step builds
the **host** target only, so the bare-metal arm is never checked at publish
time — that would want a cross-check job of its own.

**The predicate is exact and needs no exceptions.** Measured with
`rustc --print cfg`:

| target | `target_os` | arm |
|---|---|---|
| `riscv32imac-unknown-none-elf`, `thumbv7em-none-eabihf`, `xtensa-esp32-none-elf` | `none` | fork |
| `riscv32imc-esp-espidf` and the other esp-idf targets | `espidf` | real `http` — it has `std` |
| `wasm32-unknown-unknown` | `unknown` | real `http` |
| `wasm32-wasip2` | `wasi` | real `http` |

### What a caller who gets it wrong actually sees

The mistake to price is a downstream crate that writes `http = "1.5"`
itself instead of going through the shim. In the `std` arm it is not a
mistake at all — the shim re-exports that very crate, so the types are
identical, and the probe's `naive` member compiles. In the `nostd` arm
rustc says:

```
expected `http::Response<..>`, found `Response<..>`
= note: `Response<..>` and `http::Response<..>` have similar names, but are
        actually distinct types
note: `Response<..>` is defined in crate `hclient_http`
note: `http::Response<..>` is defined in crate `http`
```

Which is a good message: it names both crates and both files. The split
fails loudly rather than silently, and that is what makes either shape
tolerable.

### The recommendation does not change while upstream is unresolved

**`[patch.crates-io]` in the application's own lock file still wins**, and
the re-export does not replace it — it competes with it:

| | patch | fork + swap |
|---|---|---|
| `http` types in the universe | **one** | two, split by target |
| our published crates | unchanged | 13 manifests, a discipline rule |
| crate to publish and track for ever | none | `hclient-http` + `hclient-http-body` |
| cost to the device user | two lines in their `Cargo.toml` | none |
| the day #749 lands | the patch evaporates | a deprecation to walk |

So the swap is what to build **if** the decision is to publish a
bare-metal story rather than document a patch line — and then Shape B,
because it costs no source and cannot be mis-unified. Shape A stays the
fallback for the one thing a target cfg cannot do: let a caller force the
fork on a `std` target, which is how the fork would be tested at all.

## Reproducing

The spike is under `spikes/nostd/` — gitignored, per this workspace's own
rule that spike code is not carried because *an unbuilt crate rots into a
lie*. If the worktree is gone, it is a clone and three `sed`s:

```
rustup target add riscv32imac-unknown-none-elf

git clone https://github.com/hyperium/http spikes/nostd/http-upstream
cd spikes/nostd/http-upstream
git fetch origin pull/749/head:pr749 && git checkout pr749
#  1. bytes + fnv -> default-features = false
#  2. src/extensions.rs:4  std::hash::Hasher -> core::hash::Hasher
#  3. #![allow(dangerous_implicit_autorefs)]   (its base predates the lint)
#  4. version -> 1.5.0, so [patch.crates-io] satisfies the workspace

#  http-body: #![no_std], extern crate alloc, Box/String imports,
#  core::mem::take, default-features = false on bytes and http

git apply spikes/nostd/workspace-overlay.diff   # root manifest only
cargo check -p hclient-core -p hclient-proto \
    --no-default-features --target riscv32imac-unknown-none-elf
```

`spikes/nostd/tls-probe` is the rustls probe, self-contained with its own
`[workspace]`; `cargo check --target riscv32imac-unknown-none-elf` inside
it is the whole claim.

**What is deliberately not measured here**, and would be next: flash and
RAM for either TLS choice, which is the number that decides item 7;
whether `embedded-io-async` can express `poll_shutdown`'s `ENOTCONN`
treatment that `hclient-rt` documents; and whether a sans-io HTTP/1 codec
can keep `Failed::NotSent`/`Failed::Sent` honest without hyper's
`Envelope::drop`, which is the mechanism `docs/`'s pooled-reuse work
leans on.
