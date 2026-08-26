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

**`hclient-core` and `hclient-proto` compile for bare metal today**, once
`http` does. That was the uncertain half and it is now measured rather
than hoped:

```
cargo check -p hclient-core -p hclient-proto \
    --no-default-features --target riscv32imac-unknown-none-elf
    Finished `dev` profile
```

Both crates are `#![no_std]` on `main`'s successor branch as of this
document, their 173 tests still pass on the host, and
`cargo check --workspace --all-features` is unchanged. The one thing that
build needs and cannot have from crates.io is a `no_std` `http`, supplied
in the spike by a patched checkout (see [Reproducing](#reproducing)).

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
3. **`hclient-core` and `hclient-proto`.** Done, in the branch this
   document ships on. `portable-atomic` for the counter, `round()` for
   the float, mechanical renames for the rest.
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
