# v0.3 design — the rest of the vertical

**Theme: the two things around a request that are not the request.** How a
client works out where to connect and what to speak (W2, W4), and what a
connection becomes once the exchange is over (W5). Plus the work that
defends claims already made (W1), the debts the first half of this vertical
wrote down (W6), and one external answer that has not changed (W7).

v0.3 did not begin with this document. **HTTP/3 shipped first**, planned by
[`docs/h3-research.md`](h3-research.md) and accounted for by
[`docs/v03-acceptance.md`](v03-acceptance.md); `http-ng-h3` is in the tree,
over QUIC, multiplexed, with a caller-owned 0-RTT gate. This document plans
what is left, and is written at `38f29d2`.

---

## How to read the premises in this document

**Written first, because it is the reason this document exists at all.**

`docs/v02-design.md` was refuted three times inside one vertical, and a
fourth section was stale for its whole length:

- **§W3 said HTTP/2 had to go through `hyper/http2`.** It cannot: the
  executor bound `Http2ClientConnExec` is *sealed*
  (`hyper-1.11.0/src/rt/bounds.rs:51-52`), so an executor of our own does
  not exist to be written. The `h2` crate is driven directly instead. The
  correction is in the same section, and in `docs/v02-acceptance.md`'s
  "Three things that were found rather than designed".
- **§W7 said embassy's `'static` problem sat in the `TcpConnect` seam.** It
  did not: `embassy_net::tcp::TcpSocket` holds no buffers at all
  (`docs/w7-embassy-research.md` §1.1), and the `'static` was
  `http-ng-native`'s own — `R::Stream: 'static` at
  `crates/http-ng-native/src/lib.rs:166-171`, there because `NativeBody`
  boxes hyper's `Connection` as a `dyn Future` (`h1.rs:130`).
- **§W2 said `Spawn` does not compile on this seam, because `Spawn<F>`
  requires `F: Send + 'static` and the native IO is not `Send`.** Both
  halves were false. `Spawn<F>` declares no bounds; the auto traits come
  from the `Tokio` and `Smol` impls. What actually blocks it is that the
  future is a type parameter of the *trait*, so a bound must **name** it —
  and an `async` block has no name. `Native::with_reaper`
  (`crates/http-ng-native/src/lib.rs:361`) is the shipped counter-example.
- **§W6 said native buffers a streaming request body and WASI takes only
  `Full`.** Neither was true when it was written, and the item stayed
  blocked on that for the whole vertical. The real content was one backend,
  `http-ng-fetch`, and one browser difference nobody had measured.

Those four are not embarrassing because they were wrong — a plan is allowed
to be wrong, and this project's whole review technique is built on finding
out. They are expensive because **they were written as facts**. A premise
stated as "X is impossible" ends the enquiry. A premise stated as "X looks
impossible, and *this* would show otherwise" invites it, and three times in
one vertical the invitation paid for the document.

So, the rule for everything below:

> **Every premise is a claim plus the thing that would refute it.** Where
> the check has been run, the result is given with a file and a line. Where
> it has not, the premise says **unverified** and names what would settle
> it. No work item here rests on an unrun check without saying so out loud.

Each `## W` section therefore carries a **Premises** table: the claim, how
to check it, and what is known today. A row marked *measured here* was run
against this tree while writing; a row marked *unverified* was not.

---

## What is already known, and must not be re-measured

This section exists because three of the refutations above cost a full
re-derivation of something the tree already knew. Each row below is settled;
disagreeing with one is allowed, re-discovering it is waste.

| the question | the answer | where it was settled |
|---|---|---|
| Can library code `spawn` on this seam? | Yes, with a **named** future type. `Spawn<F>` declares no bounds; `Send + 'static` belong to the `Tokio`/`Smol` impls. An `async` block cannot be spawned because it cannot be named — that, not auto traits, is the wall. | `docs/v02-design.md` §W2 correction; `crates/http-ng-native/src/lib.rs:341-375` (`with_reaper`, `Reaper`) |
| Does `'static` in the seam shut out embassy? | No. The `'static` is `http-ng-native`'s (`lib.rs:166-171`), and embassy's own `TcpClient`/`TcpClientState` is the bounded buffer pool that satisfies it. Four variants compiled, one runs over a TAP device. | `docs/w7-embassy-research.md` §1.1–1.3 |
| Can hyper's HTTP/2 client be used without a spawner? | No, and not for reasons of cost: the trait is sealed. Use `h2` directly. | `hyper-1.11.0/src/rt/bounds.rs:51-52`; `crates/http-ng-native/src/http2.rs` |
| Which UTS-46 option word does this project need? | `0x3C` — non-transitional both ways, plus the two context checks. `UIDNA_DEFAULT` is `0`, which is *transitional*, agrees with IDNA2003, and therefore disagrees with us on `straße.de`. Apple's `swift-foundation` uses the same word. | `docs/v02-design.md` "Platform IDN"; `docs/icu-ecosystem-survey.md`; the `OPTIONS` unit test in `crates/http-ng-idn/src/lib.rs` |
| Does `Timer` need an absolute-deadline sleep for QUIC? | No. quinn's `Instant` *is* `std::time::Instant` (`quinn-0.11.11/src/lib.rs:56`), so `reset(i)` is `sleep(i - now)`. The recommendation was measured away rather than implemented. | `docs/v03-acceptance.md` "The seam changes" |
| Can `tokio` leave a `hyper` build? | No, and it never could: `hyper` depends on it unconditionally. What is checkable is that the `tokio` present is the inert `sync` leaf, and CI checks exactly that. | `AGENTS.md` "What's in the dependency graph"; hyper#3428, hyper#3767; `.github/deny/smol-path.toml` per `docs/ci.md` |
| Does `tower`'s concurrency limit bound sockets? | No — the permit is released at the response **head**, so a streaming body holds its connection outside the limit. | `docs/v02-design.md` §W4 correction; `crates/http-ng-tower/tests/concurrency.rs` |
| How is a browser's request-stream support detected? | Behaviourally, never by `'duplex' in Request.prototype`. Firefox 153 does not refuse a stream body — it **replaces** it with the 23 bytes `[object ReadableStream]`, and nothing inside the page can see that. | `docs/v02-design.md` §W6; `docs/measurements/w6-request-streams/` |
| Does dropping a `wasi:http` exchange cancel it? | Yes — the Component Model subtask is cancelled, measured on a live wasmtime host with the server's own socket as the observer. All three v0.1 backends cancel. | `docs/v02-design.md` §W1; `crates/http-ng-wasi/tests/live_roundtrip.rs` |
| Can the 0-RTT acceptance verdict be a field on a handshake result? | No. On QUIC it resolves *after* the response — 8.63 ms against 8.58 ms — so a field could only hold it by waiting for the handshake, which is the round trip 0-RTT skips. | `docs/h3-research.md` §3.2–3.3 |
| Is `full_duplex` allowed to be optimistic? | No, and it is the one field where that is a category difference: over-claiming `streaming_request_body` costs a buffered copy, over-claiming `full_duplex` costs a **deadlock**. Capabilities report the floor. | `docs/v02-design.md` §W3 "DECIDED" |
| Does a variant of a capability enum earn its place? | Only if a caller decision turns on it. `CancelSupport` and `ReuseSupport` each shipped with two values for that reason; `RedirectSupport::Internal` earns a third because `check_supported` *refuses* on it. | `docs/v02-acceptance.md` "The rule this vertical kept applying" |

Two further things are settled and are easy to re-open by accident, so they
are stated as prohibitions rather than facts:

- **Do not reach for a system IDN library on Linux.** An ELF backend was
  built, worked, and was deleted: `dlopen`ing `libicuuc.so.NN` returns
  whatever Unicode version that machine happens to carry, and for IDN a
  Unicode difference is a different host. The rule that survived is narrower
  than "the platform offers the same standard": *static linkage against an
  ABI the OS versions for us*. Windows and Apple qualify; Linux does not.
  (`docs/v02-design.md`, "Superseded on two counts".)
- **Do not add an MSRV job, a CHANGELOG, or a version bump** as tidying.
  Both have written reasons in `AGENTS.md`, and both have been proposed
  before.
