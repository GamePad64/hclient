//! The hedge: two connects at once, one request afterwards.
//!
//! `docs/v04-design.md` §W1 deliverable 5, and it is the last piece of that
//! vertical. P12 is emphatic that the race is a **third** thing — applied
//! *after* the choice, as a hedge against a network that blocks UDP/443,
//! and not as a way of choosing. So nothing here decides which stack an
//! origin speaks; [`Selecting::route`](crate::Selecting) has already
//! decided QUIC by the time any of this runs, and all this adds is *"and
//! do not wait thirty seconds to find out that it cannot be reached"*.
//!
//! # The premise this was blocked on has changed, and that is why it is here
//!
//! `docs/v04-w1-acceptance.md` §7.6 measured a race made of two
//! `Transport::execute` calls and found the thing that stopped it:
//!
//! > **A race built out of two `Transport::execute` calls races requests,
//! > not connections.** Browsers race *connections* and send the request on
//! > the winner. `Transport` has no connect-only entry point … so any race
//! > assembled from the members as they stand duplicates the request at the
//! > origin whenever the loser gets far enough.
//!
//! With no head start the losing arm delivered a complete, well-formed
//! request to the origin in five of six arms, which made the head start a
//! **safety** mechanism: it had to exceed one QUIC handshake, on a path the
//! client cannot see, or the hedge stopped being a hedge and became a coin
//! toss that sent the request twice.
//!
//! The staged connect landed since, and `http_ng_native::StagedConnect` and
//! `http_ng_h3::StagedConnect` are what this is built out of. Neither
//! `connect` writes a request — the property is structural rather than
//! promised, since the request is not handed to a stream at all until
//! `exchange`/`finish` — so **a losing arm now sends nothing at any head
//! start, zero included**. The head start stops being a safety mechanism
//! and becomes a cost knob; §1 below is what that changed and what it did
//! not.
//!
//! # 1. The head start
//!
//! `docs/v04-w1-acceptance.md` §7.4 proposed 250 ms —
//! `http_ng_proto::happy_eyeballs::HeConfig::default()`'s `attempt_delay`,
//! RFC 8305 §5's Connection Attempt Delay, and this codebase's answer to
//! the structurally identical question one layer down. That value stands.
//! **What changed is its floor and its justification, not its number.**
//!
//! - **The floor moved to zero.** A head start below one QUIC handshake
//!   used to mean a duplicated request at the origin — a correctness cliff
//!   at a place the client cannot locate. It now means one TCP connect and
//!   TLS handshake that the request will probably not use, and which is
//!   **checked into the pool warm** rather than thrown away. So
//!   [`Duration::ZERO`] is a setting rather than a bug, and it is the right
//!   one for a caller who knows their network blocks UDP/443.
//! - **The cost of being too generous is now bounded by something that did
//!   not exist when 250 ms was chosen.** §7.7 item 4 warned that without a
//!   failure memory *"the head start is paid again on every request to a
//!   blocked origin — 250 ms × every request"*. [`crate::failures`] is that
//!   memory, and §3 below feeds it from the race, so the head start is paid
//!   **once per origin per [`crate::H3_FAILURE_TTL`]** and not once per
//!   request.
//! - **The reason to keep it is no longer safety but the chooser.** The TCP
//!   floor was re-measured for this work, because
//!   `docs/nagle-and-nodelay.md` landed after §7.3 was written and
//!   `Native::new` now asks for `TCP_NODELAY`. It moved by an order of
//!   magnitude: a cold TCP-by-name exchange on this host is **1.4–2.6 ms**
//!   where §7.3 measured 41.8–42.5 ms, against QUIC's 2.5–7.8 ms. On
//!   loopback — which has no round trip, so it measures CPU and nothing
//!   else — **TCP is now the faster of the two**, and a race with no head
//!   start is won by TCP. On a real path the order reverses, because QUIC's
//!   handshake is one round trip and TCP-plus-TLS-1.3 is two; but the
//!   client cannot see the path, and a head start is what keeps the hedge
//!   from quietly overruling the choice on the paths where it would.
//!
//! The honest derived form is still §7.4's — an origin-keyed RTT
//! observation — and there is still nowhere in this crate to keep one. So
//! the head start is a constant the caller names, [`DEFAULT_HEAD_START`] is
//! the one to name when there is no reason to name another, and there is no
//! default at all in the sense that matters: a `Selecting` does not race
//! until [`Selecting::hedging`](crate::Selecting::hedging) is called.
//!
//! # 2. The budget
//!
//! `Timeouts::connect` is **one** deadline, `C`, for one request, and
//! handing each arm a copy of it is how a caller who wrote `Some(C)` is
//! made to wait `2C` — `http_ng::Client`'s `425` rule, and
//! [`crate::spend_connect_budget`]'s, one shape simpler.
//!
//! Three statements, in the order they are made here:
//!
//! - **The QUIC arm gets `C`**, unchanged, because it starts when the
//!   request does.
//! - **The hedge arm starts at `H` and gets `C − H`**, so both arms carry
//!   the same deadline measured from the same instant.
//! - **The routed request gets `C −` what the race spent.** The race *is*
//!   the connect phase, so it is charged for once; what follows it is a
//!   pooled connection being spent, and where the race consumed the whole
//!   bound there is nothing left and the answer is the caller's own
//!   `Timeout(Connect)`.
//!
//! **`H < C` is a precondition and it is met by not racing**, which is the
//! honest half of §7.5's *"that must be refused or documented"*. A caller
//! who sets `connect: Some(100ms)` against a 250 ms head start has set a
//! bound with no room for two connects in it; the hedge does not run, and
//! what happens instead is exactly what happened before this module
//! existed — the sequential fallback, whose behaviour and tests are
//! unchanged. Refusing the *request* would be worse than the thing it
//! replaces, and this is not a silent degradation: it is pinned, in
//! `tests/race.rs`.
//!
//! **One line here is not observable and is written anyway**, which is said
//! rather than hidden. The hedge arm's own `C − H` cannot be told apart
//! from leaving the probe's bound at `C`, because the QUIC arm carries `C`
//! from an earlier start and therefore always reaches its deadline first —
//! so the race ends, and the hedge is dropped, before a hedge bound of `C`
//! could be exceeded. It is written because relying on the *other* arm's
//! bound to bound this one is a coupling nothing states, and because
//! `docs/v04-w1-acceptance.md` §7.5 is where the rule lives. The mutation
//! run in `docs/v04-race.md` records it as survived, with this reason.
//!
//! # 3. The losing arm
//!
//! Both arms are dropped the instant the other answers, and neither drop
//! needs anything from this crate: `http_ng_native::Staged`'s own `Drop`
//! checks its connection back into the pool, and `http_ng_h3::Staged` is a
//! claim on a connection the pool already holds. §7.6 measured the QUIC
//! side of a mid-handshake drop — one padded goodbye datagram 1.3–2.4 ms
//! later and then silence — and that is unchanged here.
//!
//! Two things about that are decisions rather than mechanics.
//!
//! **"Warm" is the only disposal the seam offers, and it is the right one
//! anyway.** A dropped `http_ng_native::Staged` checks in whenever reuse is
//! on, and reuse is always on under a `Selecting`: `connection_reuse` is one
//! of [`combine`](crate::combine)'s *same-or-refuse* fields, so
//! `Native::without_pool()` against an `H3` does not construct. So this
//! module could not close a TCP connection it made even if it wanted to —
//! and it does not want to. Nothing was spoken on that connection; it is
//! indistinguishable from the one an ordinary TCP request would have left
//! behind, and the pool's own idle policy governs it from there. The QUIC
//! side has the cost `http_ng_h3::staged` already wrote down and said was
//! the race's to inherit: *"a declined QUIC connection goes on sending its
//! `DEFAULT_KEEP_ALIVE` PING every five seconds for as long as the
//! transport lives"*. It is inherited, and it is only ever paid on a
//! connection that **succeeded** — which is a connection to an origin that
//! does speak HTTP/3 and that the next request there will want.
//!
//! **A QUIC arm that loses the race teaches the failure memory.** This is
//! the decision, and it widens what [`crate::failures`] means by a
//! deliberate hair: the memory used to hold *"an `H3::connect` failed"* and
//! now holds *"HTTP/3 did not produce a connection in time to be worth
//! using"*. The reason is §7.7 item 4 — without it the head start is paid
//! on every request to a blocked origin, which is the cost that made the
//! race not worth building. The reason it is not an over-reach is the
//! arithmetic: the QUIC arm is only abandoned when a TCP connect that
//! started `H` **later** finished first, and QUIC's handshake is one round
//! trip against TCP-plus-TLS-1.3's two, so an arm that loses by that margin
//! is not a slow HTTP/3, it is one that is not getting through. The cost of
//! being wrong is the one the memory's own doc names: HTTP/3 held off at
//! that origin for [`crate::H3_FAILURE_TTL`], after which the next request
//! races again with the record and the advertisement exactly as they were.
//!
//! # What is not raced
//!
//! A `RequireVersion(HTTP_3)` demand. It reaches the QUIC arm with
//! `fallback: false`, and a hedge for a request that can never be sent over
//! TCP would be a TCP connection opened for nothing at all. That is the
//! same rule the fallback already follows, in the same place.
//!
//! # Nothing is spawned
//!
//! The two arms are two branches of one future, joined by
//! `futures_util::future::select` — the shape `http-ng-native`'s
//! `alongside_address_lookups` already uses to race discovery against the
//! address lookups. `http-ng-select` has no `Spawn` bound and gains none
//! here.

use crate::altsvc::Origin;
use crate::{NativeBodyOf, QuicBodyOf, SelectedBody, Selecting, spend_connect_budget};
use bytes::Bytes;
use futures_util::future::{Either, select};
use http_ng_core::unversioned::Transport;
use http_ng_core::{Error, RequestBody, RetryKind, Timeouts};
use http_ng_dns::Resolve;
use http_ng_h3::{H3, StagedConnect as QuicConnect};
use http_ng_native::{Native, Prefetch, Prepared, StagedConnect as TcpConnectStaged};
use http_ng_rt::{TcpConnect, Timer};
use http_ng_tls::TlsConnect;
use std::pin::pin;
use std::task::{Context, Poll};
use std::time::Duration;

/// How long the QUIC arm runs alone before the hedge is started, when a
/// caller has no reason to name a different number.
///
/// 250 ms, and it is not a new constant: it is
/// `http_ng_proto::happy_eyeballs::HeConfig::default()`'s `attempt_delay`,
/// which is RFC 8305 §5's Connection Attempt Delay, and which is already
/// this codebase's answer to *"how long do I give the preferred option
/// before trying the other"* one layer down. The module doc says what the
/// staged connect changed about why this number is right, and what it did
/// not change.
///
/// It is not applied by default anywhere. A [`Selecting`] does not race
/// until [`Selecting::hedging`] is called, and that call names the number.
pub const DEFAULT_HEAD_START: Duration = Duration::from_millis(250);

/// A body that is over before it starts, for the one thing a probe needs a
/// body for: [`RetryKind::Impossible`].
///
/// See [`probe_body`]. Written here rather than borrowed from
/// `http-body-util` because that crate is a dev-dependency of this one and
/// this is six lines.
struct NoBody;

impl http_body::Body for NoBody {
    type Data = Bytes;
    type Error = Error;

    fn poll_frame(
        self: std::pin::Pin<&mut Self>,
        _: &mut Context<'_>,
    ) -> Poll<Option<Result<http_body::Frame<Bytes>, Error>>> {
        Poll::Ready(None)
    }
}

/// A body with no bytes and the same [`RetryKind`] as `like`.
///
/// # Why a probe has a body at all
///
/// It does not need one to connect: neither member writes a request byte
/// before `exchange`, and the body is not read until then. It needs one
/// because `http_ng_h3`'s early-data admission reads
/// `RequestBody::retry_kind()` — *"a body we could not put back on the wire
/// after a rejection would strand the request"* — and that answer is part
/// of `http-ng-h3`'s `PoolKey`. A probe whose retry kind disagreed with the
/// request's would connect under one pool key and the request would look
/// under another, so the connection the race made would be missed and a
/// second one dialled.
///
/// So this copies the one property of the caller's body that reaches a
/// connect, and copies nothing else — in particular it never clones the
/// caller's bytes and never calls a `Rewindable` factory.
fn probe_body(like: &RequestBody) -> RequestBody {
    match like.retry_kind() {
        RetryKind::Free => RequestBody::Empty,
        RetryKind::ViaFactory => RequestBody::rewindable(|| RequestBody::Empty),
        RetryKind::Impossible => RequestBody::Streaming(Box::new(NoBody)),
    }
}

/// The request a connect is made for: this one's head, and a body that
/// answers the one question a connect asks of one.
///
/// # This is the shape of the finding, and it is worth reading twice
///
/// Both staged pairs take the request **by value** and hand it back only
/// through `Refused` — that is, only when the connect *fails*. A race has
/// to be able to abandon an arm that has neither failed nor finished, and
/// an arm holding the caller's request cannot be abandoned, because the
/// request would go with it. So the race cannot give either arm the real
/// request, and what it gives them instead is this: the same URI, the same
/// extensions, and a body that is empty and agrees about `retry_kind`.
///
/// What follows from that is the whole of §4 in `docs/v04-race.md`: the
/// race's product is two warm connections and a decision, the request is
/// sent afterwards through the ordinary routing, and the hand-off from the
/// winning arm to the request goes through the **pool** rather than through
/// the handle. `exchange` is therefore reached on the QUIC side (through
/// [`Selecting::over_quic`]) and not on the TCP one.
fn probe(parts: &http::request::Parts, body: &RequestBody) -> http::Request<RequestBody> {
    http::Request::from_parts(parts.clone(), probe_body(body))
}

/// Give this probe a connect bound of its own, replacing whatever the
/// caller's request carried.
fn set_connect(req: &mut http::Request<RequestBody>, connect: Duration) {
    let timeouts = req
        .extensions()
        .get::<Timeouts>()
        .copied()
        .unwrap_or_default();
    req.extensions_mut().insert(Timeouts {
        connect: Some(connect),
        ..timeouts
    });
}

/// What the caller's `Timeouts::connect` leaves for a second connect
/// started `head_start` late.
#[derive(Debug, PartialEq, Eq)]
enum Room {
    /// No `Timeouts::connect` was set, so neither arm is bounded and
    /// neither has anything to subtract from. That is what a caller who set
    /// no bound asked for, and against a black hole it is quinn's 30 s
    /// `max_idle_timeout` on the QUIC arm — which is the number the hedge
    /// exists to stop anybody waiting for.
    Unbounded,
    /// `C − H`, and never zero.
    Left(Duration),
    /// `H >= C`: the caller's bound has no room for a second connect, so
    /// there is no hedge and the request takes the sequential path.
    None,
}

/// The rule, as a pure function, so it can be read without the six `where`
/// clauses the method below carries.
fn room(budget: Option<Duration>, head_start: Duration) -> Room {
    let Some(connect) = budget else {
        return Room::Unbounded;
    };
    match connect.checked_sub(head_start) {
        Some(left) if !left.is_zero() => Room::Left(left),
        _ => Room::None,
    }
}

/// Which arm produced the connection the request will be sent on.
enum Won {
    /// The QUIC arm, and the request goes over HTTP/3.
    Quic,
    /// The hedge, and the QUIC arm was abandoned rather than beaten — see
    /// the module doc's §3 for why that teaches the failure memory.
    Tcp,
    /// Neither: the QUIC connect failed outright, and this is the sequential
    /// fallback's own case arriving through the race.
    QuicRefused(Error),
}

impl<R, T, D> Selecting<R, T, D>
where
    R: TcpConnect + Timer + Clone,
    T: TlsConnect,
    Native<R, T, D>: Prefetch + Transport<Error = Error> + TcpConnectStaged<Error = Error>,
    H3<R, T, D>: QuicConnect<Error = Error>,
    <Native<R, T, D> as Transport>::Body: http_body::Body<Data = Bytes, Error = Error> + Unpin,
    <H3<R, T, D> as Transport>::Body: http_body::Body<Data = Bytes, Error = Error> + Unpin,
    D: Resolve,
{
    /// Two connects, one request, and the head start between them.
    ///
    /// Reached only from [`Transport::execute`]'s QUIC arm, only when
    /// [`Selecting::hedging`] has been called, and only for a request that
    /// is allowed to fall back — see the module doc.
    ///
    /// The QUIC arm is started first and the hedge sleeps for `head_start`
    /// before it connects, so a QUIC handshake that is going to succeed
    /// normally does so before a TCP socket is opened at all. Whichever arm
    /// produces a connection first ends the race; the other is dropped,
    /// which cancels it, and the connection it may have left behind is warm
    /// in its own member's pool rather than closed.
    ///
    /// Then the caller's request — which never entered the race, and could
    /// not have, see [`probe`] — is routed over the winning stack through
    /// the ordinary path, with what is left of `Timeouts::connect`.
    pub(crate) async fn raced(
        &self,
        req: http::Request<RequestBody>,
        origin: Option<&Origin>,
        head_start: Duration,
    ) -> Result<http::Response<SelectedBody<NativeBodyOf<R, T, D>, QuicBodyOf<R, T, D>>>, Error>
    {
        let began = self.now();
        let budget = req.extensions().get::<Timeouts>().and_then(|t| t.connect);
        let hedge_bound = match room(budget, head_start) {
            Room::Unbounded => None,
            Room::Left(left) => Some(left),
            // No room for two connects inside one bound. The hedge does not
            // run and this is the sequential fallback, unchanged — see the
            // module doc's §2.
            Room::None => return self.over_quic(req, true, origin).await,
        };

        // The caller's request is taken apart here and put back together
        // below. Neither arm ever holds it: see [`probe`], and
        // `docs/v04-race.md` §4 for what follows from that.
        let (parts, body) = req.into_parts();
        let mut hedge_probe = probe(&parts, &body);
        if let Some(left) = hedge_bound {
            set_connect(&mut hedge_probe, left);
        }

        let won = {
            let quic = pin!(self.quic.connect(probe(&parts, &body)));
            let hedge = pin!(async {
                self.rt.sleep(head_start).await;
                self.tcp.connect(Prepared::new(hedge_probe)).await
            });
            // Every arm below drops the loser by letting it fall out of
            // scope, which is what cancels it. The handles are dropped the
            // same way, and that is where the connections go: back to their
            // own member's pool, warm.
            match select(quic, hedge).await {
                Either::Left((Ok(_quic_connection), _hedge)) => Won::Quic,
                Either::Left((Err(refused), _hedge)) => Won::QuicRefused(refused.into_error()),
                Either::Right((Ok(_tcp_connection), _quic)) => Won::Tcp,
                // The hedge failed. That is not an answer to the question
                // the race is asking — the QUIC arm is still the request's
                // best chance and is still running — so it is awaited
                // alone, under the bound it has carried all along.
                Either::Right((Err(_), quic)) => match quic.await {
                    Ok(_quic_connection) => Won::Quic,
                    Err(refused) => Won::QuicRefused(refused.into_error()),
                },
            }
        };

        // Read once, after the race and before anything else, for
        // `over_quic`'s reason: how much of the bound is gone and when a
        // failure expires are one instant.
        let now = self.now();
        let spent = now.saturating_sub(began);
        let mut req = http::Request::from_parts(parts, body);
        match won {
            Won::Quic => {
                // The connection is in `H3`'s pool, so `over_quic`'s connect
                // finds it rather than dialling. The budget is charged for
                // the race whether or not it does.
                let _ = spend_connect_budget(&mut req, spent);
                self.over_quic(req, true, origin).await
            }
            Won::Tcp => {
                if let Some(origin) = origin {
                    self.h3_failures.note(origin, now);
                }
                let _ = spend_connect_budget(&mut req, spent);
                self.tcp
                    .execute_prepared(Prepared::new(req))
                    .await
                    .map(|r| r.map(SelectedBody::Tcp))
            }
            Won::QuicRefused(error) => {
                if let Some(origin) = origin {
                    self.h3_failures.note(origin, now);
                }
                self.after_quic_failed(req, error, spent).await
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_unset_bound_leaves_both_arms_unbounded() {
        assert_eq!(room(None, Duration::from_millis(250)), Room::Unbounded);
    }

    #[test]
    fn the_hedge_gets_the_bound_less_the_head_start() {
        assert_eq!(
            room(Some(Duration::from_millis(300)), Duration::from_millis(50)),
            Room::Left(Duration::from_millis(250))
        );
    }

    #[test]
    fn a_head_start_that_does_not_fit_inside_the_bound_leaves_no_room() {
        assert_eq!(
            room(Some(Duration::from_millis(100)), Duration::from_millis(250)),
            Room::None
        );
        // Exactly equal is also no room: a hedge that starts at the instant
        // the bound expires has nothing to connect with.
        assert_eq!(
            room(Some(Duration::from_millis(250)), Duration::from_millis(250)),
            Room::None
        );
    }

    #[test]
    fn a_probe_body_agrees_with_the_caller_s_about_retrying_and_about_nothing_else() {
        for like in [
            RequestBody::Empty,
            RequestBody::Full(Bytes::from_static(b"hello")),
            RequestBody::rewindable(|| RequestBody::Full(Bytes::from_static(b"hello"))),
            RequestBody::Streaming(Box::new(NoBody)),
        ] {
            let probe = probe_body(&like);
            assert_eq!(
                probe.retry_kind(),
                like.retry_kind(),
                "the pool key is computed from this"
            );
            // And carries none of the caller's bytes.
            assert!(matches!(probe.size_hint(), Some(0) | None));
        }
    }
}
