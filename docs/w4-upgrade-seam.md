# W4 — the upgrade seam, decided

`docs/v03-design.md` §W4 argued the shape. This document takes the
decisions, re-checks the facts they rest on against the versions in this
tree today, and answers a question §W4 did not have: whether the same seam
carries WebTransport.

Nothing here is code. It exists so that the implementation does not have to
re-derive any of it, and so that a later reader can tell which parts were
measured and which were argued.

## 1. The facts, re-verified today

§W4's argument rests on four claims about other people's source. All four
hold, checked against `hyper 1.11.0` and the tree as of this document.

| claim | verified |
|---|---|
| `Connection::poll_without_shutdown` and `into_parts` do not require `Send` on the IO | `src/client/conn/http1.rs`: the `impl<T, B> Connection<T, B>` carrying both is bounded by `T: Read + Write + Unpin`, `B: Body + 'static`, `B::Error: Into<Box<dyn StdError + Send + Sync>>`. `Send` appears only on the *error*, never on `T` |
| `hyper::upgrade::Upgraded` does require it | `src/upgrade.rs:66-67`: `pub struct Upgraded { io: Rewind<Box<dyn Io + Send>> }` |
| polling `Connection` to completion after a 101 destroys the upgrade and reports success | `src/client/conn/http1.rs:310-320`: `proto::Dispatched::Upgrade(pending) => { pending.manual(); Poll::Ready(Ok(())) }`, under hyper's own comment *"With no `Send` bound on `I`, we can't try to do upgrades here"* |
| nothing branches on `Capabilities::upgrade` | four variants (`None`, `H1`, `ExtendedConnect`, `Both`); every backend assigns `None`; the only reads are two assertions and one test fixture that sets `H1` to assert it back. No production code branches on it — the non-test matches for `.upgrade` in this workspace are all `Weak::upgrade` |

The third is the one to keep in mind while implementing: **"the exchange
finished" and "the upgrade was thrown away" are the same observation**, and
`crates/http-ng-native/src/h1.rs` polls `Connection` exactly that way. A
101 must be recognised by *status*, before the connection is polled to
completion. This was probed separately and is not a live defect today — a
101 does not poison the pool, because five independent places ask whether
the connection has finished — but that is an accident of the current code
rather than a guarantee, and it is written down in
`crates/http-ng-native/tests/switching_protocols.rs`.

## 2. Decision 1 — the seam is WebSocket, not "give me the socket"

**Message oriented: `Stream<Item = Result<Message>> + Sink<Message>`.**
A byte-stream seam ("hand back the socket after the 101") is implementable
by exactly one of this project's four backends, and the three it excludes
include the browser — the target whose inclusion is the whole claim.
`http-ng-fetch` says so where it sets the capability: *"`WebSocket` in the
browser is a wholly separate global, unreachable from a `fetch`-shaped
`Transport`"*. On Apple platforms `NSURLSessionWebSocketTask` is
message-framed too and hands back no bytes; `wasi:http` has an
`HTTP-upgrade-failed` error code and no mechanism at all.

So h1 upgrade is an **implementation detail underneath** the seam on
native, not the seam.

**A separate trait, not a method on `Transport`.** The reasoning is the one
already applied twice here. `QuicTlsConnect` is a separate trait from
`TlsConnect` because their method intersection is empty and an adapter
between them would type-check *with an empty body* — a failure mode worse
than a compile error. The same holds here. And `TcpAdoptStd` established
the other half: *"a backend that cannot adopt a `std::net::TcpStream`
should say so by not implementing the trait, which is already how the seam
expresses it"* (`docs/w7-embassy-research.md` §1.4).

**The seam therefore expresses itself by being implemented.** A backend
that can do WebSocket implements the trait; one that cannot does not, and
using it is a compile error rather than a runtime surprise.

## 3. Decision 2 — `UpgradeSupport` goes

Four variants, zero callers branching on them. v0.2's rule is explicit: *a
capability variant exists only if a caller decision turns on it.* With
WebSocket as a trait, no caller decision does.

The alternative was that it becomes real for a *different* reason — a raw
h1 upgrade or a `CONNECT` tunnel exposed to callers. That is a proxy
feature nobody has asked for, and inventing a user for a field in order to
keep the field is the reasoning this project rejects everywhere else.

**So: delete it, in the same change that adds the trait, not before.**
Deleting first would leave a window where a backend can neither declare nor
implement upgrade, and the acceptance documents would have to describe a
state that never shipped.

One consequence to handle rather than discover: `crates/http-ng/tests/
facade.rs` sets `UpgradeSupport::H1` on a test fixture and asserts it back.
That test is pinning the *plumbing* — that a backend's capability reaches
the facade — not upgrade itself. It needs a different field to carry the
same proof, not deletion.

## 4. WebTransport — a separate seam, and the spec's reasons are out of date

`docs/superpowers/specs/2026-08-05-http-ng-design.md` §5.7 concluded "we
don't write WebTransport", on two grounds. Both have moved.

**Ground 1 — "the client crates are server-only."** Still true of
`h3-webtransport` 0.1.2, and the working clients (`wtransport`,
`web-transport-quinn`) still carry their own HTTP/3 and are nailed to
`quinn/runtime-tokio`, which defeats this project's runtime seam. But the
implied conclusion — that WebTransport cannot be built on *our* h3 — does
not follow, because **extended CONNECT is reachable from `h3` 0.0.8's
client API today**: `Pseudo::request` reads `ext.get::<Protocol>()` when
the method is `CONNECT` (`h3-0.0.8/src/proto/headers.rs`, and the client
passes the request's extensions through at `src/client/connection.rs:163`),
`Protocol::WEB_TRANSPORT` is a public constant in `src/ext.rs`,
`SETTINGS_ENABLE_WEBTRANSPORT` is parsed in `src/config.rs`, and `h3-quinn`
has a `datagram` feature. The primitives are present.

**Ground 2 — implicitly, browser availability.** No longer an obstacle:
`WebTransport` shipped in Chrome 97 (2022-01), Firefox 114 (2023-06) and
**Safari 26.4 (2026-03-24)**. All four major engines have it.

**It is still a separate seam, and that is the durable reason.** A
WebSocket seam is one message channel: `Stream + Sink`. A WebTransport
session is a *multiplexer* — bidirectional and unidirectional streams
opened on demand in both directions, plus datagrams. The method
intersection with `Stream + Sink<Message>` is as empty as
`QuicTlsConnect`'s was with `TlsConnect`, and forcing one onto the other
would produce exactly the adapter that type-checks with an empty body.

What *is* shared is the pattern, and this is why the order matters:
WebSocket establishes that a non-request/response capability lives in its
own trait, expressed by implementation. Once that precedent is in the
tree, WebTransport is a second instance of it rather than a second
argument.

**The remaining work for WebTransport is the session layer** — streams
bound to a session id, demultiplexing incoming ones, and the capsule
protocol — and no crate provides it for a client under our runtime seam.
That is a real cost, and it is the honest reason to defer, unlike the two
above.

## 5. Order of work

1. The `WebSocket` trait in `http-ng-core` — shape only, no backend.
2. `http-ng-native`: h1 upgrade underneath it, using
   `poll_without_shutdown` + `into_parts`, never `hyper::upgrade`; the 101
   detected by status before the connection is polled out.
3. `http-ng-fetch`: the browser's `WebSocket` global behind the same trait
   — the proof that the seam is the right shape, since this is the backend
   a byte-stream seam would have excluded.
4. `UpgradeSupport` deleted, `facade.rs`'s plumbing test re-pointed.

Steps 1 and 2 are one change: a trait with no implementation is a shape
nobody has tested. Step 3 is what makes it a seam rather than a native
feature with a trait in front of it.

## What this document does not decide

- **Framing on native.** §5.7 proposed `async-tungstenite` 0.35.0 on
  `futures_io`, runtime-neutral, with
  `tungstenite::handshake::client::{generate_key, derive_accept_key}` for
  the handshake. Not re-verified here; check it against the tree's own
  dependency policy before adopting.
- **Whether the pool must change.** v0.2 W2 froze the pool without
  WebSocket, which the spec had asked to avoid. The cost is unmeasured.
- **Permessage-deflate**, subprotocol negotiation, and close-code
  semantics.
