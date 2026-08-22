//! The hedge: two connects at once, one request afterwards.
//!
//! The race is a **third** thing, after the two discovery tiers — applied
//! *after* the choice, as a hedge against a network that blocks UDP/443,
//! and not as a way of choosing. So nothing here decides which stack an
//! origin speaks; [`Native::route`](crate::Native) has already
//! decided QUIC by the time any of this runs, and all this adds is *"and
//! do not wait thirty seconds to find out that it cannot be reached"*.
//!
//! # The premise this was blocked on has changed, and that is why it is here
//!
//! A race made of two `Transport::execute` calls was measured, and the
//! measurement is what stopped it:
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
//! The staged connect landed since, and `hclient_native::StagedConnect` and
//! `hclient_h3::StagedConnect` are what this is built out of. Neither
//! `connect` writes a request — the property is structural rather than
//! promised, since the request is not handed to a stream at all until
//! `exchange`/`finish` — so **a losing arm now sends nothing at any head
//! start, zero included**. The head start stops being a safety mechanism
//! and becomes a cost knob; §1 below is what that changed and what it did
//! not.
//!
//! # 1. The head start
//!
//! 250 ms — `hclient_proto::happy_eyeballs::HeConfig::default()`'s
//! `attempt_delay`,
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
//!   floor moved by an order of magnitude once `Native::new` began asking
//!   for `TCP_NODELAY`: a cold TCP-by-name exchange on this host is
//!   **1.4–2.6 ms** where it had been 41.8–42.5 ms, against QUIC's
//!   2.5–7.8 ms. On
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
//! default at all in the sense that matters: a `Native` does not race
//! until [`Native::hedging`](crate::Native::hedging) is called.
//!
//! # 2. The budget
//!
//! `Timeouts::connect` is **one** deadline, `C`, for one request, and
//! handing each arm a copy of it is how a caller who wrote `Some(C)` is
//! made to wait `2C` — `hclient::Client`'s `425` rule, and the sequential
//! fallback's `spend_connect_budget`, one shape simpler.
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
//! bound to bound this one is a coupling nothing states. The mutation is
//! recorded as survived, with this reason.
//!
//! # 3. The losing arm
//!
//! Both arms are dropped the instant the other answers, and neither drop
//! needs anything from this crate: `hclient_native::Staged`'s own `Drop`
//! checks its connection back into the pool, and `hclient_h3::Staged` is a
//! claim on a connection the pool already holds. §7.6 measured the QUIC
//! side of a mid-handshake drop — one padded goodbye datagram 1.3–2.4 ms
//! later and then silence — and that is unchanged here.
//!
//! Two things about that are decisions rather than mechanics.
//!
//! **"Warm" is the only disposal the seam offers, and it is the right one
//! anyway.** A dropped `hclient_native::Staged` checks in whenever reuse is
//! on, and reuse is always on under a `Native`: `connection_reuse` is one
//! of [`combine`](crate::caps::combine)'s *same-or-refuse* fields, so
//! `Native::without_pool()` against an `H3` does not construct. So this
//! module could not close a TCP connection it made even if it wanted to —
//! and it does not want to. Nothing was spoken on that connection; it is
//! indistinguishable from the one an ordinary TCP request would have left
//! behind, and the pool's own idle policy governs it from there. The QUIC
//! side has the cost `hclient_h3::staged` already wrote down and said was
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
//! `futures_util::future::select` — the shape `hclient-native`'s
//! `alongside_address_lookups` already uses to race discovery against the
//! address lookups. `hclient-select` has no `Spawn` bound and gains none
//! here.

use crate::Native;
use crate::altsvc::Origin;
use crate::route::spend_connect_budget;
use crate::{Prepared, StagedConnect as TcpConnectStaged};
use bytes::Bytes;
use futures_util::future::{Either, select};
use hclient_core::{Error, RequestBody, RetryKind, Timeouts};
use hclient_dns::Resolve;
use hclient_rt::{TcpConnect, Timer};
use hclient_tls::TlsConnect;
use std::pin::pin;
use std::task::{Context, Poll};
use std::time::Duration;

/// How long the QUIC arm runs alone before the hedge is started, when a
/// caller has no reason to name a different number.
///
/// 250 ms, and it is not a new constant: it is
/// `hclient_proto::happy_eyeballs::HeConfig::default()`'s `attempt_delay`,
/// which is RFC 8305 §5's Connection Attempt Delay, and which is already
/// this codebase's answer to *"how long do I give the preferred option
/// before trying the other"* one layer down. The module doc says what the
/// staged connect changed about why this number is right, and what it did
/// not change.
///
/// It is not applied by default anywhere. A [`Native`] does not race
/// until [`Native::hedging`] is called, and that call names the number.
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
/// because `hclient_h3`'s early-data admission reads
/// `RequestBody::retry_kind()` — *"a body we could not put back on the wire
/// after a rejection would strand the request"* — and that answer is part
/// of `hclient-h3`'s `PoolKey`. A probe whose retry kind disagreed with the
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

/// The request a connect is made for: everything of this one that reaches a
/// connector, and nothing else.
///
/// # This is the shape of the finding, and it is worth reading twice
///
/// Both staged pairs take the request **by value** and hand it back only
/// through `Refused` — that is, only when the connect *fails*. A race has to
/// be able to abandon an arm that has neither failed nor finished, and an
/// arm holding the caller's request cannot be abandoned, because the request
/// would go with it. So the race cannot give either arm the real request,
/// and what it gives them instead is this: the same method, URI, version,
/// headers and extensions, and a body that is empty and agrees about
/// `retry_kind`.
///
/// Written field by field rather than through `http::request::Parts` so that
/// what a probe copies is a list a reader can check against what a connector
/// reads: `key_parts` and `admit` take the URI, `protocol_admissible`,
/// `check_version` and the early-data gate take the extensions, and neither
/// member looks at a header or a body byte before `exchange`.
///
/// What follows: the race's product is two warm connections and a decision,
/// the request is sent afterwards through the ordinary routing, and the
/// hand-off from the winning arm to the request goes through the **pool**
/// rather than through the handle.
fn probe(req: &http::Request<RequestBody>) -> http::Request<RequestBody> {
    let mut probe = http::Request::new(probe_body(req.body()));
    *probe.method_mut() = req.method().clone();
    *probe.uri_mut() = req.uri().clone();
    *probe.version_mut() = req.version();
    *probe.headers_mut() = req.headers().clone();
    *probe.extensions_mut() = req.extensions().clone();
    probe
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
        resolve: None,
        connect: Some(connect),
        ..timeouts
    });
}

/// What the caller's `Timeouts::connect` leaves for a second connect started
/// `head_start` late.
#[derive(Debug, PartialEq, Eq)]
enum Room {
    /// No `Timeouts::connect` was set, so neither arm is bounded and neither
    /// has anything to subtract from. That is what a caller who set no bound
    /// asked for, and against a black hole it is quinn's 30 s
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
/// clauses the methods below carry.
fn room(budget: Option<Duration>, head_start: Duration) -> Room {
    let Some(connect) = budget else {
        return Room::Unbounded;
    };
    match connect.checked_sub(head_start) {
        Some(left) if !left.is_zero() => Room::Left(left),
        _ => Room::None,
    }
}

/// Which stack now holds a connection for this request — the race's whole
/// output, and deliberately not a response.
///
/// A decision rather than an answer is what keeps the routing in one place:
/// [`Native::serve_quic`] sends the request the same way whether a race
/// happened or not, and there is exactly one call site for each of the two
/// members. That is not only tidiness. `execute` is an `async fn`, so every
/// future it may await is a field of one state machine; an earlier draft in
/// which the race did its own routing put a second copy of the whole QUIC arm
/// inside `execute` and **overflowed the stack** of two `hclient::Client`
/// tests in a debug build.
enum Raced {
    /// Send it over HTTP/3 — either because the QUIC arm won, or because no
    /// race was run at all and this is the ordinary sequential path.
    Quic,
    /// Send it over TCP: the hedge won, or the QUIC connect failed and there
    /// is bound left to try TCP with.
    Tcp,
    /// Neither. The QUIC connect failed and the caller's whole connect bound
    /// went with it, so the honest answer is that failure rather than a
    /// second attempt there is no room for.
    Failed(Error),
}

impl<R, T, D, H, P> Native<R, T, D, H, P>
where
    R: TcpConnect + Timer + Clone,
    R::Stream: 'static,
    T: TlsConnect,
    T::Stream<R::Stream>: 'static,
    D: Resolve,
    H: hclient_core::unversioned::Hooks + Clone + Unpin,
    P: crate::proxy::ProxyProtocol,
{
    /// Everything the QUIC arm of `Transport::execute` does, hedged or not.
    ///
    /// The hedge runs when one was asked for **and** this request may go over
    /// TCP at all. Both are conditions rather than one: a
    /// `RequireVersion(HTTP_3)` demand arrives with `fallback: false`, and a
    /// TCP connection opened for a request that could never be sent on it is
    /// a connection opened for nothing.
    ///
    /// What the race hands back is a decision and a request whose
    /// `Timeouts::connect` has been charged for it. The two ways out of here
    /// — [`Self::over_quic`] and the TCP member's `execute_prepared` — are
    /// each written once, which is what stops a raced request and an unraced
    /// one from becoming two transports, and is also why this function exists
    /// at all rather than the race doing its own routing (see [`Raced`]).
    pub(crate) async fn serve_quic(
        &self,
        mut req: http::Request<RequestBody>,
        fallback: bool,
        origin: Option<&Origin>,
    ) -> Result<http::Response<crate::NativeBody<R, T, H>>, Error> {
        if let Some(head_start) = self.hedge.filter(|_| fallback) {
            match self.race_connects(&mut req, origin, head_start).await {
                Raced::Quic => {}
                Raced::Tcp => {
                    return self.run(Prepared::new(req)).await;
                }
                Raced::Failed(e) => return Err(e),
            }
        }
        self.over_quic(req, fallback, origin).await
    }

    /// Two connects, one head start between them, and no request on either.
    ///
    /// The QUIC arm is started first and the hedge sleeps for `head_start`
    /// before it connects, so a QUIC handshake that is going to succeed
    /// normally does so before a TCP socket is opened at all. Whichever arm
    /// produces a connection first ends the race; the other is dropped, which
    /// cancels it, and whatever connection it may have left behind is warm in
    /// its own member's pool rather than closed.
    ///
    /// `req` is **borrowed and not sent**: what this writes back into it is
    /// what is left of `Timeouts::connect` after the race, because the race
    /// *is* the connect phase and is charged for once. See the module doc's
    /// §2 for the arithmetic and §3 for what a losing arm costs.
    async fn race_connects(
        &self,
        req: &mut http::Request<RequestBody>,
        origin: Option<&Origin>,
        head_start: Duration,
    ) -> Raced {
        let began = self.now();
        let budget = req.extensions().get::<Timeouts>().and_then(|t| t.connect);
        let hedge_bound = match room(budget, head_start) {
            Room::Unbounded => None,
            Room::Left(left) => Some(left),
            // No room for two connects inside one bound. Nothing is raced,
            // nothing is charged, and the caller gets the sequential fallback
            // unchanged — the module doc's §2.
            Room::None => return Raced::Quic,
        };

        let mut hedge_probe = probe(req);
        if let Some(left) = hedge_bound {
            set_connect(&mut hedge_probe, left);
        }
        let quic_probe = probe(req);

        let refused = {
            let Some(arm) = self.h3.as_ref().filter(|_| self.versions.h3) else {
                return Raced::Quic;
            };
            let quic = pin!(arm.connect_boxed(quic_probe));
            let hedge = pin!(async {
                self.rt.sleep(head_start).await;
                TcpConnectStaged::connect(self, Prepared::new(hedge_probe)).await
            });
            // Every arm below drops the loser by letting it fall out of
            // scope, which is what cancels it. The handles go the same way,
            // and that is where the connections go: back to their own
            // member's pool, warm.
            match select(quic, hedge).await {
                Either::Left((Ok(_quic_connection), _hedge)) => return Raced::Quic,
                Either::Left((Err(refused), _hedge)) => refused.into_error(),
                Either::Right((Ok(_tcp_connection), _quic)) => {
                    // The QUIC arm was abandoned rather than beaten, and that
                    // is what the memory is told — module doc §3.
                    self.note_h3_failure(origin);
                    self.charge(req, began);
                    return Raced::Tcp;
                }
                // The hedge failed. That is not an answer to the question the
                // race is asking — the QUIC arm is still the request's best
                // chance and is still running — so it is awaited alone, under
                // the bound it has carried all along.
                Either::Right((Err(_), quic)) => match quic.await {
                    Ok(_quic_connection) => return Raced::Quic,
                    Err(refused) => refused.into_error(),
                },
            }
        };

        self.note_h3_failure(origin);
        if self.charge(req, began) {
            Raced::Tcp
        } else {
            Raced::Failed(refused)
        }
    }

    /// Take what the race spent off the request's `Timeouts::connect`, and
    /// say whether there is anything left to make a *fresh* connect with.
    ///
    /// The winner does not need the answer — it has a connection — and the
    /// refusal does: see `crate::spend_connect_budget`.
    fn charge(&self, req: &mut http::Request<RequestBody>, began: Duration) -> bool {
        spend_connect_budget(req, self.now().saturating_sub(began))
    }

    /// One line, written once, because a memory written at two of the three
    /// exits and not at the third is the shape of mutation this crate has
    /// already been bitten by.
    fn note_h3_failure(&self, origin: Option<&Origin>) {
        if let Some(origin) = origin {
            self.h3_failures.note(origin, self.now());
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
    fn a_probe_body_agrees_with_the_callers_about_retrying_and_nothing_else() {
        for like in [
            RequestBody::Empty,
            RequestBody::Full(Bytes::from_static(b"hello")),
            RequestBody::rewindable(|| RequestBody::Full(Bytes::from_static(b"hello"))),
            RequestBody::Streaming(Box::new(NoBody)),
        ] {
            let body = probe_body(&like);
            assert_eq!(
                body.retry_kind(),
                like.retry_kind(),
                "hclient-h3's pool key is computed from this"
            );
            // And carries none of the caller's bytes.
            assert!(matches!(body.size_hint(), Some(0) | None));
        }
    }

    /// A probe is the request a connector would have been handed, minus the
    /// body — so everything a connector reads has to survive the copy.
    #[test]
    fn a_probe_carries_everything_a_connector_reads() {
        let mut req = http::Request::builder()
            .method(http::Method::POST)
            .uri("https://example.test:8443/path?q=1")
            .body(RequestBody::Full(Bytes::from_static(b"hello")))
            .expect("a well-formed request");
        req.extensions_mut()
            .insert(hclient_core::RequireVersion(http::Version::HTTP_3));
        req.extensions_mut().insert(Timeouts {
            resolve: None,
            connect: Some(Duration::from_millis(300)),
            ..Default::default()
        });

        let probe = probe(&req);

        assert_eq!(probe.method(), req.method());
        assert_eq!(probe.uri(), req.uri());
        assert_eq!(probe.version(), req.version());
        assert_eq!(
            probe
                .extensions()
                .get::<hclient_core::RequireVersion>()
                .map(|v| v.0),
            Some(http::Version::HTTP_3),
            "the version demand decides whether a connect is attempted at all"
        );
        assert_eq!(
            probe.extensions().get::<Timeouts>().and_then(|t| t.connect),
            Some(Duration::from_millis(300)),
            "and the bound the arm is spending"
        );
        assert_eq!(
            probe.body().size_hint(),
            Some(0),
            "but not one byte of the caller's body"
        );
    }
}
