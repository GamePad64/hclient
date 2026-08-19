//! `GOAWAY` is not observed here, and this file is why — executed rather
//! than described.
//!
//! `docs/v04-w2-webtransport.md` §6 recorded it as deliberately not done
//! with one reason: *"`GOAWAY` arrives on the h3 **control** stream, which
//! the driver owns and nobody polls."* That is true and it is the least of
//! it. Four things stand between an arriving `GOAWAY` and a caller of this
//! crate, and each of them is a separate assertion below, so an `h3` that
//! removes one fails a line instead of leaving a stale paragraph:
//!
//! 1. nothing in this crate polls the driver, so the frame is never
//!    processed at all — [`a_goaway_changes_nothing_a_session_can_observe`];
//! 2. `h3::client::Connection::poll_close` **absorbs** it: it processes the
//!    frame and goes on waiting, so there is no future in `h3` 0.0.8 that
//!    resolves when one arrives —
//!    [`polling_the_driver_across_a_goaway_resolves_nothing`];
//! 3. what it sets is one bit, `ConnectionState::is_closing`, and the
//!    `StreamId` the frame carries — its entire content — is `pub(super)`,
//!    so *"this session is in the rejected range"* and *"this session is
//!    not"* are the same value —
//!    [`two_goaways_that_say_opposite_things_look_identical`];
//! 4. that bit is set by receiving a `GOAWAY` **and** by sending one, so it
//!    is not a fact about the peer —
//!    [`the_one_bit_is_set_by_our_own_goaway_too`].
//!
//! # And the decision, which is separate from the four
//!
//! draft-ietf-webtrans-http3 gives `GOAWAY` two meanings for a WebTransport
//! client. *"A client receiving GOAWAY cannot initiate CONNECT requests for
//! new WebTransport sessions on that HTTP/3 connection"* is the actionable
//! one — and it acquired a subject only when `Session::open_session`
//! landed, because before it a client could not initiate a second CONNECT
//! at all. The other is *"a signal to applications to initiate shutdown"*,
//! which the draft puts in the same sentence as `WT_DRAIN_SESSION` — *"an
//! endpoint MAY continue using the session and MAY open new WebTransport
//! streams"* after either — and which this crate already declines to
//! surface for the reason in `Session::closed`'s own documentation.
//!
//! So there is one duty here, it is unmet, and
//! [`a_session_after_a_goaway_is_rejected_by_the_peer_rather_than_by_us`]
//! is that stated as a test rather than as a regret. **Its cost is a round
//! trip and a typed error, and that is measured rather than assumed**: the
//! peer resets the second CONNECT with `H3_REQUEST_REJECTED`, which
//! RFC 9114 §4.1.1 defines as *"no processing occurred"* and whose remedy
//! is the draft's own — open another connection. So the duty is unmet here
//! and met one layer down, one round trip later.
//!
//! # Why it is not fixed by polling the driver, which would work
//!
//! It would: `h3`'s `SendRequest::send_request` calls
//! `check_peer_connection_closing` and answers `StreamError::RemoteClosing`
//! once the bit is set, so a client that polled the driver would get the
//! MUST NOT enforced for free —
//! [`h3_refuses_a_request_once_the_driver_has_seen_the_goaway`] measures
//! exactly that. Two things stop it, and the second is the decisive one.
//!
//! `poll_close` treats a **server-initiated bidirectional stream** as
//! `H3_STREAM_CREATION_ERROR` unconditionally, which is RFC 9114 §6.1's
//! rule *"unless such an extension has been negotiated"* with the
//! exemption unreachable, because `h3` 0.0.8's client cannot announce
//! WebTransport (§3(a)). Server-initiated bidirectional streams are this
//! crate's other open item and §6 records them as *reachable*; polling the
//! driver would make them a connection error instead.
//!
//! And **there is no causal moment to poll at.** A `GOAWAY` travels on the
//! control stream and a CONNECT on a request stream; QUIC orders bytes
//! within a stream and not across two, so nothing a client can await proves
//! a `GOAWAY` sent before it has arrived. Any test of "the driver was
//! polled after the frame landed" is a test of a duration, and this
//! workspace has three timing assertions on its record that turned out to
//! be flakes.
#![cfg(not(target_family = "wasm"))]

mod server;

use bytes::Bytes;
use h3::ConnectionState as _;
use http_ng_core::ErrorKind;
use http_ng_webtransport::Session;
use server::Options;
use std::future::poll_fn;

/// The bound on "the peer acts": a hang guard, as everywhere else here.
const ACTED: std::time::Duration = std::time::Duration::from_secs(10);

/// How long a `poll_close` that must **not** resolve is given to resolve.
///
/// The only place in this file where a duration is part of a claim, and it
/// is a one-sided one: `poll_close` returning inside it fails the test, and
/// its expiring proves nothing on its own — which is why the assertion
/// beside it is that the bit *did* flip, so the frame demonstrably arrived
/// and was demonstrably processed within the same window.
const ABSORBED: std::time::Duration = std::time::Duration::from_millis(500);

fn uri(addr: std::net::SocketAddr, path: &str) -> http::Uri {
    format!("https://{addr}{path}").parse().unwrap()
}

type Driver = h3::client::Connection<h3_quinn::Connection, Bytes>;
type Sender = h3::client::SendRequest<h3_quinn::OpenStreams, Bytes>;

/// An h3 client built exactly as `Session::connect` builds one, with its
/// driver handed back rather than buried in a `Session`.
///
/// The crate under test exposes no driver — holding it and never polling it
/// is the design — so the four facts about `h3` 0.0.8 below are measured
/// against `h3` directly. That is the honest place for them: they are
/// findings about that crate, and a version of it that fixed one would fail
/// these lines whether or not this crate had changed.
async fn h3_client(conn: quinn::Connection) -> (Driver, Sender) {
    let (mut driver, send) = h3::client::builder()
        .enable_extended_connect(true)
        .enable_datagram(true)
        .build::<h3_quinn::Connection, h3_quinn::OpenStreams, Bytes>(h3_quinn::Connection::new(
            conn,
        ))
        .await
        .expect("the fixture speaks h3");
    // The same wait `Session::connect` makes, for the same reason.
    poll_fn(|cx| driver.inner.poll_control(cx))
        .await
        .expect("the peer's SETTINGS");
    (driver, send)
}

async fn connect_session(send: &mut Sender, at: http::Uri) -> u64 {
    let req = http::Request::builder()
        .method(http::Method::CONNECT)
        .uri(at)
        .extension(h3::ext::Protocol::WEB_TRANSPORT)
        .body(())
        .expect("a CONNECT with a protocol always builds");
    let mut stream = send.send_request(req).await.expect("the CONNECT goes out");
    let resp = stream.recv_response().await.expect("and is answered");
    assert!(resp.status().is_success());
    let id = stream.id().into_inner();
    std::mem::forget(stream);
    id
}

/// A fixture that answers `sessions` CONNECTs and writes a `GOAWAY`
/// immediately after the **first** session's response, naming the
/// `max_requests`-th request stream after the last it accepted.
///
/// The `GOAWAY` is not a gate a test opens, and `server::GoAway`'s own
/// documentation says why: the first version of this file used one and
/// flaked three runs in six, because nothing on the client side means *the
/// frame has landed*. Written where it is, `h3`'s server has set
/// `sent_closing` before it accepts anything else, so what depends on it is
/// decided at the server rather than by which stream's bytes arrive first.
fn server_with_goaway(sessions: usize, max_requests: usize) -> server::Server {
    server::start(Options {
        sessions,
        max_sessions: 4,
        goaway: Some(server::GoAway {
            after: 1,
            max_requests,
        }),
        ..Options::default()
    })
}

/// **(1)** A `GOAWAY` arrives and nothing a `Session` exposes changes.
///
/// The frame is processed only inside `h3::client::Connection::poll_close`,
/// and this crate holds that driver and never polls it — so the session
/// goes on opening streams and sending datagrams exactly as it did, which
/// is what the draft says it MAY do anyway.
///
/// What makes this a test rather than a tautology is the *other* half,
/// below: the same `GOAWAY`, on a driver that **is** polled, flips
/// `is_closing` — so the frame is arriving, and being ignored is a choice
/// this crate is making rather than an accident of the fixture.
#[tokio::test(flavor = "multi_thread")]
async fn a_goaway_changes_nothing_a_session_can_observe() {
    let srv = server_with_goaway(1, 0);
    let conn = server::dial(&srv).await;
    let session = Session::connect(conn.clone(), &uri(srv.addr, "/a"))
        .await
        .expect("a session");

    let (mut send, _recv) = session.open_bi().await.expect("a stream after the GOAWAY");
    send.write_all(b"after the goaway").await.expect("loopback");
    send.finish().expect("a freshly written stream finishes");
    assert!(srv.wait_for_streams(1, ACTED).await);
    assert_eq!(srv.streams()[0].session_id, session.id().value());

    session
        .send_datagram(Bytes::from_static(b"also after"))
        .expect("and a datagram");
    let echoed = tokio::time::timeout(ACTED, session.recv_datagram())
        .await
        .expect("which comes back")
        .expect("without an error");
    assert_eq!(&echoed[..], b"also after");
}

/// **The duty this crate does not meet, and what it actually costs.**
///
/// draft-ietf-webtrans-http3: *"A client receiving GOAWAY cannot initiate
/// CONNECT requests for new WebTransport sessions on that HTTP/3
/// connection; it must open a new HTTP/3 connection."* This client
/// initiates one, because it never sees the frame.
///
/// The `GOAWAY` here names the **first session's own** CONNECT stream, so
/// by RFC 9114 §5.2 every stream at or above it — the second session's
/// included — is one the server has said it will not process. And it does
/// not: `h3`'s server resets the stream with `H3_REQUEST_REJECTED`, which
/// §4.1.1 defines as *"no processing occurred"* and whose remedy is *"the
/// client can retry the request on a different connection"* — the draft's
/// own instruction, arriving from the peer instead of from us.
///
/// **So the cost of not observing `GOAWAY` is a round trip and a typed
/// error, not a session opened against a server's wishes.** That is the
/// whole of the gap, measured; it is smaller than the missing observation
/// looks, and it is why the four blockers in this file's module
/// documentation are not worth the price named there.
///
/// # It is a stream error, so the first session is untouched
///
/// A rejected request resets one stream. The connection carries on —
/// asserted from `quinn`'s own `close_reason`, which is `None` for a live
/// connection whatever h3 thinks — and so does the session on it. A client
/// that lost them would be paying a great deal more than a round trip.
///
/// The stream opened at the end is not read by the fixture, and cannot be:
/// this server is still inside its request-accept loop waiting for a second
/// CONNECT that will never come, which is the same loop that has to be
/// running for the rejection above to happen at all. So the assertion is
/// that the session can still open one, which is a claim about the
/// connection and not about the peer.
///
/// **This test is the tripwire.** A version of this crate that polled the
/// driver would have `h3` refuse the second `send_request` locally, with
/// `RemoteClosing` and no round trip — see
/// [`h3_refuses_a_request_once_the_driver_has_seen_the_goaway`] — and this
/// line would fail on the error's kind, its source, and the server's own
/// count of what reached it.
#[tokio::test(flavor = "multi_thread")]
async fn a_session_after_a_goaway_is_rejected_by_the_peer_rather_than_by_us() {
    let srv = server_with_goaway(2, 0);
    let conn = server::dial(&srv).await;
    let a = Session::connect(conn.clone(), &uri(srv.addr, "/a"))
        .await
        .expect("the first session");

    let refused = tokio::time::timeout(ACTED, a.open_session(&uri(srv.addr, "/b")))
        .await
        .expect("the peer answers")
        .expect_err("with a refusal the client should have made itself");
    assert_eq!(*refused.kind(), ErrorKind::Connect);
    assert!(
        refused.to_string().contains("H3_REQUEST_REJECTED"),
        "RFC 9114 \u{a7}4.1.1's \"no processing occurred\": {refused}"
    );

    assert_eq!(
        srv.requests().len(),
        1,
        "the CONNECT reached the wire and was never resolved into a request"
    );

    assert!(
        conn.close_reason().is_none(),
        "a rejected request is a stream error, not a connection one: {:?}",
        conn.close_reason()
    );
    let (mut send, _recv) = a
        .open_bi()
        .await
        .expect("and the session still opens streams");

    // **The write has two lawful answers and the invariant is the same in
    // both**, which is why this is a `match` rather than an `expect`.
    //
    // On the wire a WebTransport data stream is an ordinary client-initiated
    // bidirectional stream, and `h3` 0.0.8 gives a peer nothing to tell one
    // from a request — so a server already inside `shutdown` may answer it
    // with `STOP_SENDING(H3_REQUEST_REJECTED)`, and this fixture's does,
    // sometimes. It was an `expect`, and it failed 3 runs in 40 under
    // `-j96` across the workspace. Captured rather than reasoned about:
    // `stream=StreamId(8) err=Stopped(267) requests=1 close_reason=None` —
    // the session's own CONNECT is stream 0, the refused sibling is 4, and
    // this stream is 8.
    //
    // Widening a window would not have fixed it and the guess that it might
    // was measured and dropped: a 200 ms pause before `open_bi` produced 0
    // failures in 5, so the rejection is not deterministic in *either*
    // direction. Whether the server's accept loop is still running when
    // stream 8 arrives is the fixture's own race, and nothing this client
    // does can settle it.
    //
    // **What could not be shown is that this change lowers a rate**, and
    // saying so is the point. After it, 60 workspace runs at `-j96` were
    // clean and the `Stopped` arm was never taken — but a **control** run
    // of the original `expect`, on the same host in the same hour, was
    // also 0 in 30. The conditions that produced 3 in 40 that morning were
    // gone, so neither number is evidence about the fix. What the fix
    // rests on is the capture above and the shape of the change: the
    // rejection is a lawful answer and the test now asserts what is true
    // of both answers, which cannot fail for the reason that was caught.
    //
    // The arm is therefore **unexercised in this environment**, and that
    // is recorded in `docs/v04-acceptance.md` rather than left to be
    // rediscovered.
    //
    // What does not vary is the claim this test is named for, and it is
    // asserted on both paths: a rejection here is a **stream** error and
    // the connection is still up. The bytes-arrive half is asserted where
    // it is determined — `sessions.rs`'s
    // `closing_one_session_leaves_the_other_open` writes the same payload
    // on a surviving session against a server that was never shut down.
    match send.write_all(b"a lives").await {
        Ok(()) => send.finish().expect("a freshly written stream finishes"),
        Err(quinn::WriteError::Stopped(code)) => assert_eq!(
            code.into_inner(),
            0x10b,
            "the only refusal in play here is H3_REQUEST_REJECTED"
        ),
        Err(other) => panic!("a stream error was expected, not {other:?}"),
    }
    assert!(
        conn.close_reason().is_none(),
        "and either way the connection survives it: {:?}",
        conn.close_reason()
    );
}

/// **(2)** `poll_close` absorbs a `GOAWAY`: it processes the frame and goes
/// on waiting.
///
/// So there is no future in `h3` 0.0.8's public API that resolves when one
/// arrives, and the only shape available to a client is a flag to look at.
/// The two assertions are one claim: the frame **was** processed inside the
/// window (`is_closing` flipped) and the future **did not** resolve in it.
///
/// An `h3` whose `poll_close` returned on a `GOAWAY` fails the first line.
#[tokio::test(flavor = "multi_thread")]
async fn polling_the_driver_across_a_goaway_resolves_nothing() {
    let srv = server_with_goaway(1, 0);
    let conn = server::dial(&srv).await;
    let (mut driver, mut send) = h3_client(conn.clone()).await;
    connect_session(&mut send, uri(srv.addr, "/a")).await;

    let resolved = tokio::time::timeout(ABSORBED, poll_fn(|cx| driver.poll_close(cx))).await;
    assert!(
        resolved.is_err(),
        "poll_close resolved on a GOAWAY: {:?}",
        resolved.map(|e| e.to_string())
    );
    assert!(
        send.is_closing(),
        "and yet the frame arrived and was processed inside the same window"
    );
}

/// **(3)** Two `GOAWAY`s that say opposite things about *this* session look
/// identical from here.
///
/// RFC 9114 §5.2: the identifier names the first stream the sender will not
/// process, so `max_requests = 0` names this session's own CONNECT stream —
/// *your session is rejected* — and `max_requests = 1` names the next one —
/// *your session stands, open no more*. Those are different instructions.
///
/// `h3` 0.0.8 keeps the `StreamId` in `Connection::recv_closing`, which is
/// `pub(super)`. What a client outside can read is
/// `ConnectionState::is_closing`, a `bool` — so the whole content of the
/// frame is gone and the two arms below are indistinguishable, which is
/// this file's version of the two-state answer to a three-state question
/// `h3`'s `settings()` already gives one frame earlier.
#[tokio::test(flavor = "multi_thread")]
async fn two_goaways_that_say_opposite_things_look_identical() {
    let mut observed = Vec::new();
    for max_requests in [0usize, 1] {
        let srv = server_with_goaway(1, max_requests);
        let conn = server::dial(&srv).await;
        let (mut driver, mut send) = h3_client(conn.clone()).await;
        let session = connect_session(&mut send, uri(srv.addr, "/a")).await;
        let resolved = tokio::time::timeout(ABSORBED, poll_fn(|cx| driver.poll_close(cx))).await;

        observed.push((
            session,
            send.is_closing(),
            resolved.is_ok(),
            // The draft's *"MAY continue using the session and MAY open new
            // WebTransport streams"*, which holds in both arms — including
            // the one the server has said it will not process.
            conn.open_bi().await.is_ok(),
        ));
    }
    let (rejected, spared) = (observed[0], observed[1]);
    assert_eq!(
        rejected.0, spared.0,
        "the two arms put the session on the same stream, so only the frame differs"
    );
    assert_eq!(
        (rejected.1, rejected.2, rejected.3),
        (spared.1, spared.2, spared.3),
        "a GOAWAY that rejects this session and one that spares it are the same observation"
    );
    assert!(rejected.1, "and that observation is the one bit");
}

/// **(4)** The one bit is not a fact about the peer: our own `GOAWAY` sets
/// it too.
///
/// `h3` calls `set_closing` from exactly two places — `process_goaway`,
/// which runs when one arrives, and `shutdown`, which runs when one is
/// sent — and `is_closing` is the only public reader of either. So a client
/// that reported "the peer is going away" from that bit would be reporting
/// its own `shutdown` as the peer's.
///
/// This crate never calls `shutdown`, so the ambiguity is not reachable
/// *through it* — which is the same shape as `ConnectionId::UNWATCHED` one
/// crate over, where a value with one reachable side was left alone. It is
/// asserted here because it is a fact about the bit any future reader of it
/// inherits, and because `h3`'s client `shutdown` is public.
///
/// The peer is never told anything useful either: `Connection::shutdown`
/// discards its `max_push` argument — the parameter is named `_max_push`
/// and the frame always carries `PushId(0)` — and a client-sent `GOAWAY`
/// carries a **push** ID, not a stream ID, so it says nothing about
/// sessions in any case.
#[tokio::test(flavor = "multi_thread")]
async fn the_one_bit_is_set_by_our_own_goaway_too() {
    let srv = server::start(Options {
        sessions: 1,
        max_sessions: 4,
        ..Options::default()
    });
    let conn = server::dial(&srv).await;
    let (mut driver, mut send) = h3_client(conn.clone()).await;
    connect_session(&mut send, uri(srv.addr, "/a")).await;

    assert!(!send.is_closing(), "no GOAWAY in either direction yet");
    driver
        .shutdown(7)
        .await
        .expect("the client's own GOAWAY goes out");
    assert!(
        send.is_closing(),
        "and sets the same bit a peer's GOAWAY would"
    );
}

/// The fix that is not taken, measured so that its cost is a number rather
/// than an argument.
///
/// Once the driver has been polled across a `GOAWAY`, `h3` enforces the
/// draft's MUST NOT by itself: `SendRequest::send_request` calls
/// `check_peer_connection_closing` before it opens a stream and answers
/// `StreamError::RemoteClosing`. So a client that polled the driver would
/// get [`a_session_can_still_be_opened_after_a_goaway`]'s gap closed for
/// free, and locally rather than a round trip later.
///
/// What it would cost is in this file's module documentation: `poll_close`
/// also turns a server-initiated bidirectional stream into
/// `H3_STREAM_CREATION_ERROR`, and there is no causal moment at which to
/// poll. This test exists so that the *first* half — that the enforcement
/// is really there — is a measurement and not a reading.
#[tokio::test(flavor = "multi_thread")]
async fn h3_refuses_a_request_once_the_driver_has_seen_the_goaway() {
    let srv = server_with_goaway(2, 0);
    let conn = server::dial(&srv).await;
    let (mut driver, mut send) = h3_client(conn.clone()).await;
    connect_session(&mut send, uri(srv.addr, "/a")).await;
    let _ = tokio::time::timeout(ABSORBED, poll_fn(|cx| driver.poll_close(cx))).await;
    assert!(send.is_closing(), "the frame arrived and was processed");

    let req = http::Request::builder()
        .method(http::Method::CONNECT)
        .uri(uri(srv.addr, "/b"))
        .extension(h3::ext::Protocol::WEB_TRANSPORT)
        .body(())
        .expect("a CONNECT with a protocol always builds");
    let refused = send
        .send_request(req)
        .await
        .err()
        .expect("h3 refuses a request on a connection the peer is closing");
    assert!(
        matches!(refused, h3::error::StreamError::RemoteClosing),
        "and it says so in its own vocabulary: {refused:?}"
    );
    assert_eq!(
        srv.requests().len(),
        1,
        "the second CONNECT never reached the wire"
    );
}
