//! More than one WebTransport session on one QUIC connection.
//!
//! Every assertion here is about something the fixture's own HTTP/3 server
//! decoded — the second CONNECT as its QPACK decoder read it, the session
//! ID at the head of each stream as an independent varint reader read it —
//! or about a refusal that happened before a byte left, which is asserted
//! by counting what the server did *not* see.
//!
//! # What was recorded as the blocker, and what it actually was
//!
//! `docs/v04-w2-webtransport.md` §6: *"here a `Session` owns the h3 client,
//! so there is one."* That was true and it was ours to fix. Two other
//! blockers were recorded elsewhere, and only one of them survived being
//! executed:
//!
//! - **`PoolKey`** — `hclient-h3`'s pool key, and a fact about *sharing a
//!   pooled connection*, which is a different question. Nothing in this
//!   crate has ever touched it.
//! - **`SETTINGS_WT_MAX_SESSIONS` cannot be read** (§3(c)) — **wrong**, and
//!   `the_peers_limit_comes_off_the_settings_frame` is the correction. It
//!   is not on `h3::config::Settings`, which is where §3(c) looked; it is
//!   on the SETTINGS *frame*, which `Session::connect` already awaits.
#![cfg(not(target_family = "wasm"))]

mod server;

use bytes::Bytes;
use hclient_core::ErrorKind;
use hclient_webtransport::{Session, TooManySessions};
use server::Options;

/// The bound on "the peer acts", as in `tests/webtransport.rs`: a hang
/// guard, not a claim about speed.
const ACTED: std::time::Duration = std::time::Duration::from_secs(10);

fn source_of<T: std::error::Error + 'static>(e: &hclient_core::Error) -> &T {
    std::error::Error::source(e)
        .and_then(|s| s.downcast_ref::<T>())
        .expect("the reason is typed, not a string")
}

fn uri(addr: std::net::SocketAddr, path: &str) -> http::Uri {
    format!("https://{addr}{path}").parse().unwrap()
}

/// A server that answers `n` CONNECTs and advertises room for `n`.
fn server_for(n: usize) -> server::Server {
    server::start(Options {
        sessions: n,
        max_sessions: n as u64,
        ..Options::default()
    })
}

/// **The premise.** A second extended CONNECT goes out on the h3 client the
/// first session built, is answered, and the two sessions are peers.
///
/// The two claims that make it a session rather than a second request: the
/// server's own decoder reports `:protocol = webtransport` on **both**, and
/// a stream opened on each carries **that session's** ID in its header —
/// read by the fixture's own varint reader, so a client that headed every
/// stream with the first session's ID fails here rather than agreeing with
/// itself.
#[tokio::test(flavor = "multi_thread")]
async fn a_second_session_is_a_peer_of_the_first() {
    let srv = server_for(2);
    let conn = server::dial(&srv).await;
    let a = Session::connect(conn.clone(), &uri(srv.addr, "/a"))
        .await
        .expect("the first session");
    let b = a
        .open_session(&uri(srv.addr, "/b"))
        .await
        .expect("the second session");

    assert_ne!(a.id(), b.id(), "two sessions are two CONNECT streams");

    let seen = srv.requests();
    assert_eq!(seen.len(), 2, "the server answered two CONNECTs");
    for (request, session) in seen.iter().zip([&a, &b]) {
        assert_eq!(request.method, http::Method::CONNECT);
        assert_eq!(request.protocol.as_deref(), Some("webtransport"));
        assert_eq!(
            request.stream_id,
            session.id().value(),
            "the session ID is its own CONNECT stream's ID"
        );
    }
    assert_eq!(seen[0].uri.path(), "/a");
    assert_eq!(seen[1].uri.path(), "/b");

    for (session, payload) in [(&a, &b"from-a"[..]), (&b, &b"from-b"[..])] {
        let (mut send, _recv) = session.open_bi().await.expect("a stream on either session");
        send.write_all(payload).await.expect("loopback accepts it");
        send.finish().expect("a freshly written stream finishes");
    }
    assert!(srv.wait_for_streams(2, ACTED).await, "both streams arrive");

    let mut streams = srv.streams();
    streams.sort_by_key(|s| s.session_id);
    assert_eq!(
        streams
            .iter()
            .map(|s| (s.signal, s.session_id, s.payload.clone()))
            .collect::<Vec<_>>(),
        vec![
            (0x41, a.id().value(), b"from-a".to_vec()),
            (0x41, b.id().value(), b"from-b".to_vec()),
        ],
        "each stream names the session that opened it"
    );
}

/// **The correction.** The peer's `SETTINGS_WT_MAX_SESSIONS` is read, and a
/// session over it is refused before a byte leaves.
///
/// `docs/v04-w2-webtransport.md` §3(c) recorded this number as unreadable,
/// having looked at `h3::config::Settings`, which has no getter for it.
/// The number is on the SETTINGS **frame** — `h3::proto::frame::Settings`,
/// whose `get` is `pub` — and `Session::connect` was already awaiting that
/// frame and discarding it with a `_`.
///
/// The two arms are the same server with one number changed, so what the
/// assertion turns on is the number and not anything else about the peer.
/// **That nothing was sent is asserted causally**, not by absence: the
/// session is used afterwards and the fixture records the stream that use
/// opens, so a second CONNECT that had gone out would be sitting in
/// `requests()` beside it.
#[tokio::test(flavor = "multi_thread")]
async fn the_peers_limit_comes_off_the_settings_frame() {
    for limit in [1u64, 3] {
        let srv = server::start(Options {
            // Exactly the CONNECTs this arm sends: the refused one never
            // leaves, and the fixture answers a fixed number before it
            // starts reading WebTransport streams.
            sessions: limit as usize,
            max_sessions: limit,
            ..Options::default()
        });
        let conn = server::dial(&srv).await;
        let mut open = vec![
            Session::connect(conn.clone(), &uri(srv.addr, "/0"))
                .await
                .expect("the first session is not checked against the limit"),
        ];
        while (open.len() as u64) < limit {
            let next = open[0]
                .open_session(&uri(srv.addr, "/n"))
                .await
                .expect("a session within the limit");
            open.push(next);
        }

        let refused = open[0]
            .open_session(&uri(srv.addr, "/over"))
            .await
            .expect_err("the peer has no room for another");
        assert_eq!(*refused.kind(), ErrorKind::Unsupported);
        assert_eq!(
            *source_of::<TooManySessions>(&refused),
            TooManySessions { limit, open: limit },
            "the refusal names the peer's number and ours"
        );

        let (mut send, _recv) = open[0].open_bi().await.expect("the session still works");
        send.write_all(b"still here").await.expect("loopback");
        send.finish().expect("a freshly written stream finishes");
        assert!(srv.wait_for_streams(1, ACTED).await);
        assert_eq!(
            srv.requests().len() as u64,
            limit,
            "the refused CONNECT never left"
        );
    }
}

/// A limit of zero refuses every *further* session and not the first.
///
/// The peer here announces WebTransport and offers no sessions, which is
/// contradictory and is exactly what `h3`'s own server builder produces
/// unless `max_webtransport_sessions` is called. Refusing the first session
/// over it would be adding a third condition to the establishment gate, and
/// the reason not to is the one `SETTINGS_H3_DATAGRAM` is kept off it by: a
/// caller who never opens a second session would be charged for a setting
/// it never used.
#[tokio::test(flavor = "multi_thread")]
async fn a_limit_of_zero_still_allows_the_first_session() {
    let srv = server::start(Options {
        sessions: 1,
        max_sessions: 0,
        ..Options::default()
    });
    let conn = server::dial(&srv).await;
    let a = Session::connect(conn.clone(), &uri(srv.addr, "/a"))
        .await
        .expect("the gate is the two flags, not the limit");
    let refused = a
        .open_session(&uri(srv.addr, "/b"))
        .await
        .expect_err("and there is room for nothing more");
    assert_eq!(
        *source_of::<TooManySessions>(&refused),
        TooManySessions { limit: 0, open: 1 }
    );
}

/// Dropping a session gives its slot back, which is what makes the draft's
/// word *simultaneous* mean something.
///
/// It is the handle rather than the wire that counts: a session that has
/// been closed is over at the peer but still the caller's to ask
/// `closed()` of, so a slot that came back at close would be counting
/// something nobody named.
#[tokio::test(flavor = "multi_thread")]
async fn a_dropped_session_gives_its_slot_back() {
    let srv = server::start(Options {
        sessions: 3,
        max_sessions: 2,
        ..Options::default()
    });
    let conn = server::dial(&srv).await;
    let a = Session::connect(conn.clone(), &uri(srv.addr, "/a"))
        .await
        .expect("the first session");
    let b = a
        .open_session(&uri(srv.addr, "/b"))
        .await
        .expect("the second");
    assert!(
        a.open_session(&uri(srv.addr, "/c")).await.is_err(),
        "two of two are open"
    );
    drop(b);
    let c = a
        .open_session(&uri(srv.addr, "/c"))
        .await
        .expect("the dropped session's slot");
    assert_ne!(c.id(), a.id());
    assert_eq!(
        srv.requests().len(),
        3,
        "three CONNECTs, one of them refused twice over"
    );
}

/// **The datagram half, and the defect it closes.** One session reads a
/// datagram addressed to another and hands it over.
///
/// There is one datagram queue per QUIC connection and it is quinn's, so
/// whichever session is being polled reads *everything*. Until v0.4
/// `recv_datagram` discarded what was not its own — right while a
/// connection could hold one session, and silent data loss now.
///
/// # The ordering is causal, and it is the whole test
///
/// `a` is the **only** reader when `b`'s datagram arrives: `b` has not
/// called `recv_datagram` at all yet, so nothing but `a`'s loop can take it
/// off the connection. `b` then asks and gets it. A client that dropped
/// what was not its own leaves `b` waiting for ever, and the second
/// assertion is that `a` — which never stopped waiting — goes on to receive
/// its **own** datagram afterwards, so the hand-over did not cost `a` its
/// place.
#[tokio::test(flavor = "multi_thread")]
async fn a_sibling_reads_a_datagram_and_hands_it_over() {
    let srv = server_for(2);
    let conn = server::dial(&srv).await;
    let a = std::sync::Arc::new(
        Session::connect(conn.clone(), &uri(srv.addr, "/a"))
            .await
            .expect("the first session"),
    );
    let b = a
        .open_session(&uri(srv.addr, "/b"))
        .await
        .expect("the second session");

    let reader = a.clone();
    let a_is_waiting = tokio::spawn(async move { reader.recv_datagram().await });
    // `a`'s task has to be inside `recv_datagram` before `b`'s datagram
    // comes back, or nothing is reading the connection and the test proves
    // only that `b` can read its own. There is no client-side signal for
    // "a future is parked", so this is a sleep — and it is a **precondition
    // of the fixture**, not the assertion: if it were too short, `b` would
    // read its own datagram and the hand-over would never be exercised,
    // which is why the second half asserts `a` was there all along.
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    b.send_datagram(Bytes::from_static(b"for-b"))
        .expect("the fixture's endpoint carries datagrams");
    let collected = tokio::time::timeout(ACTED, b.recv_datagram())
        .await
        .expect("b's datagram reaches b")
        .expect("and is not an error");
    assert_eq!(&collected[..], b"for-b");

    a.send_datagram(Bytes::from_static(b"for-a"))
        .expect("the same endpoint");
    let mine = tokio::time::timeout(ACTED, a_is_waiting)
        .await
        .expect("a was still waiting, so its own arrives")
        .expect("the task did not panic")
        .expect("and is not an error");
    assert_eq!(&mine[..], b"for-a");
}

/// The two sessions end separately: closing one is not closing the other.
///
/// Asserted causally rather than by a clock. The fixture records `b`'s
/// capsule as it reads it, so a `capsules()` containing it means `b`'s
/// close reached the peer; `a` is then used, and the stream that use opens
/// is recorded too. A client whose `close` ended the connection — or the
/// h3 client under it — could satisfy neither.
#[tokio::test(flavor = "multi_thread")]
async fn closing_one_session_leaves_the_other_open() {
    let srv = server_for(2);
    let conn = server::dial(&srv).await;
    let a = Session::connect(conn.clone(), &uri(srv.addr, "/a"))
        .await
        .expect("the first session");
    let b = a
        .open_session(&uri(srv.addr, "/b"))
        .await
        .expect("the second session");

    b.close(7, "b is done").await.expect("the capsule goes out");
    let ended = tokio::time::timeout(ACTED, b.closed())
        .await
        .expect("the peer answers b's FIN with its own")
        .expect("a clean end");
    assert_eq!(ended.code, 0, "the peer closed its half without a capsule");

    let (mut send, _recv) = a.open_bi().await.expect("a is untouched");
    send.write_all(b"a lives").await.expect("loopback");
    send.finish().expect("a freshly written stream finishes");
    assert!(srv.wait_for_streams(1, ACTED).await);
    assert_eq!(srv.streams()[0].session_id, a.id().value());
    assert!(
        srv.capsules().iter().any(|c| c.kind == 0x2843),
        "b's close capsule reached the peer"
    );
}

/// **The blocker, executed.** A second `Session::connect` on the same QUIC
/// connection kills the connection — and takes the first session with it.
///
/// `docs/v04-w2-webtransport.md` §4 predicted the mechanism from RFC 9114
/// §6.2.1 — two h3 clients open two control streams, and a second control
/// stream is a **connection** error — and this is that prediction executed
/// against `h3`'s own server. The peer answers
/// `H3_STREAM_CREATION_ERROR` (`0x103`, read here off `quinn`'s
/// `close_reason` rather than off `h3`'s `Display`, so the assertion is on
/// the code that crossed the wire) with the reason *"got two control
/// streams"*.
///
/// What the prediction did not say is the price: it is the **connection**
/// that dies, so a caller who reached for a second session the obvious way
/// loses the one it already had. That is why
/// [`Session::open_session`](hclient_webtransport::Session::open_session)
/// shares an h3 client rather than building another, and it is the whole
/// reason `Shared` exists.
///
/// # The peer has to be listening, and that is a finding of its own
///
/// This fixture is still polling its `h3::server::Connection` while it
/// waits for the second CONNECT, which is what lets it see the second
/// control stream at all. Measured with a fixture that had stopped polling
/// — `sessions: 1` — the same call simply **hangs**: nobody objects, and
/// the second client never receives the peer's SETTINGS, because a
/// connection has one server control stream and the first client took it.
/// So the failure mode depends on the peer, and neither form is a session.
#[tokio::test(flavor = "multi_thread")]
async fn a_second_h3_client_on_one_connection_is_a_connection_error() {
    let srv = server_for(2);
    let conn = server::dial(&srv).await;
    let a = Session::connect(conn.clone(), &uri(srv.addr, "/a"))
        .await
        .expect("the first session");

    let second = tokio::time::timeout(ACTED, Session::connect(conn.clone(), &uri(srv.addr, "/b")))
        .await
        .expect("the peer objects rather than ignoring it")
        .expect_err("two control streams on one connection is not a session");
    assert_eq!(*second.kind(), ErrorKind::Connect);

    assert!(
        matches!(
            conn.close_reason(),
            Some(quinn::ConnectionError::ApplicationClosed(ref close))
                if close.error_code == quinn::VarInt::from_u32(0x103)
        ),
        "RFC 9114 \u{a7}6.2.1's H3_STREAM_CREATION_ERROR, on the connection: {:?}",
        conn.close_reason()
    );

    a.open_bi()
        .await
        .expect_err("and the first session went down with the connection");
    assert_eq!(
        srv.requests().len(),
        1,
        "the second CONNECT never went out — the SETTINGS gate is before it"
    );
}
