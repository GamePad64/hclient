//! WebTransport against a real server on a real socket.
//!
//! Every assertion here is about something that crossed the wire — the
//! `:protocol` pseudo-header as the server's own HTTP/3 decoder read it,
//! the varints at the head of a QUIC stream as an independent decoder read
//! them, the status the server chose. Nothing is asserted about a
//! duration: the two orderings the tests rely on are causal, and both are
//! named where they are used.
#![cfg(not(target_family = "wasm"))]

mod server;

use bytes::Bytes;
use hclient_core::ErrorKind;
use hclient_webtransport::{
    AlreadyClosed, BadCloseCapsule, DatagramTooLarge, DatagramsUnavailable, NotHttps,
    NotSupportedByPeer, Session, SessionClose, SessionRefused,
};
use server::{AfterResponse, Options};

/// The bound on "the peer acts on the CONNECT stream".
///
/// A hang guard, like [`ARRIVAL`], and not a claim about speed — but
/// unlike a datagram, everything it waits for is *ordered and reliable*:
/// a capsule and a FIN travel on one QUIC stream, so what this bound
/// actually catches is a client that never writes the FIN or a server that
/// never reads the capsule, not a slow machine.
const ACTED: std::time::Duration = std::time::Duration::from_secs(10);

/// Await `f`, and fail rather than hang if the session's end never comes.
async fn ended(session: &Session) -> Result<SessionClose, hclient_core::Error> {
    tokio::time::timeout(ACTED, session.closed())
        .await
        .expect("the session ends, one way or the other")
}

/// The bound on "a datagram sent on loopback arrives".
///
/// It is a guard against a hang, not the thing any test asserts: a
/// datagram is unacknowledged, so nothing the client can observe proves
/// one is still in flight, and a `recv_datagram` that never resolves would
/// otherwise take the whole binary down with it. Generous on purpose — the
/// suite's own claims are about *what* arrives, never about when.
const ARRIVAL: std::time::Duration = std::time::Duration::from_secs(10);

/// Take stream 0 out of circulation, so the CONNECT does not land there.
///
/// The reason is the same one `a_bidirectional_stream_carries_bytes_both_ways`
/// gives at length, and it is sharper for datagrams: a session on stream 0
/// has Quarter Stream ID `0 >> 2 == 0`, which is also what *not* shifting
/// gives, and also what a hard-coded zero gives. On stream 4 the quarter
/// is 1 and there is exactly one right answer.
async fn take_stream_zero(conn: &quinn::Connection) {
    let (mut stray, _) = conn
        .open_bi()
        .await
        .expect("stream credit on a fresh connection");
    stray
        .reset(quinn::VarInt::from_u32(0))
        .expect("a freshly opened stream can be reset");
}

fn source_of<T: std::error::Error + 'static>(e: &hclient_core::Error) -> &T {
    std::error::Error::source(e)
        .and_then(|s| s.downcast_ref::<T>())
        .expect("the reason is typed, not a string")
}

fn uri(addr: std::net::SocketAddr, path: &str) -> http::Uri {
    format!("https://{addr}{path}").parse().unwrap()
}

/// **The premise.** An extended CONNECT carrying `:protocol = webtransport`
/// leaves this workspace's HTTP/3 stack and is accepted by a server that
/// speaks it.
///
/// This was asserted reachable from `h3` 0.0.8's client API by reading three
/// files. This is the reading, executed: the value the server reports is the
/// one its own QPACK decoder took off the wire, not a type the two sides
/// share by construction.
#[tokio::test]
async fn an_extended_connect_carries_the_webtransport_protocol_to_the_server() {
    let server = server::start(Options::default());
    let conn = server::dial(&server).await;
    let target = uri(server.addr, "/counter");

    let session = Session::connect(conn, &target)
        .await
        .expect("the server announces WebTransport and answers 200");

    // Causal, not timed: `connect` returned only because the response to
    // the CONNECT arrived, and the server records the request before it
    // answers.
    let seen = server.requests();
    assert_eq!(seen.len(), 1, "exactly one request reached the server");
    assert_eq!(seen[0].method, http::Method::CONNECT);
    assert_eq!(seen[0].protocol.as_deref(), Some("webtransport"));
    assert_eq!(seen[0].uri, target, "the session URI is the request target");
    assert_eq!(
        session.id().value(),
        seen[0].stream_id,
        "the session ID is the CONNECT stream's ID"
    );

    // The other half of the handshake, and the only place it is visible:
    // our own SETTINGS. RFC 9220 §3 says receipt of
    // `SETTINGS_ENABLE_CONNECT_PROTOCOL` by a *server* has no impact, so
    // nothing on the wire forces this — which is exactly why it needs an
    // assertion rather than a comment. What it says is true and cheap, and
    // a client that announces nothing is indistinguishable from one that
    // cannot do this at all.
    let announced = server
        .client_settings()
        .expect("the client's control stream was read before the request resolved");
    assert!(
        announced.extended_connect,
        "the client announced extended CONNECT"
    );
    // And the one it cannot make: `h3` 0.0.8's *client* builder has no
    // setter for `SETTINGS_ENABLE_WEBTRANSPORT` or
    // `SETTINGS_WT_MAX_SESSIONS`, so the announcement the draft requires of
    // clients does not go out. Asserted rather than described, so that a
    // future `h3` which grows the setter fails this line instead of leaving
    // a stale paragraph in a document.
    assert!(
        !announced.webtransport,
        "h3 0.0.8's client cannot announce SETTINGS_ENABLE_WEBTRANSPORT; if this fails, it can now"
    );
    // The one the client *can* make, and the reason datagrams exist in
    // this crate at all: `h3::client::Builder::enable_datagram` is the
    // setter that is missing one feature over, for WebTransport itself.
    // RFC 9297 §2.1 makes it the peer's licence to send us any HTTP
    // Datagram, and — like the line above — nothing on the wire in this
    // suite forces it, because the fixture sends datagrams from raw
    // `quinn` and consults no HTTP/3 setting before doing so. So this
    // assertion is the only thing standing between the announcement and
    // its silent deletion.
    assert!(
        announced.datagram,
        "the client announced SETTINGS_H3_DATAGRAM"
    );
}

/// A bidirectional stream, opened by the client, carrying bytes both ways —
/// and headed the way draft-ietf-webtrans-http3 §4.2 says.
///
/// # Why a stream is opened and reset before the session is established
///
/// The CONNECT would otherwise land on stream ID **0**, where every wrong
/// answer for the session ID is also the right one: `0 >> 2` is `0`, and so
/// is a hard-coded zero. `h3`'s own `From<StreamId> for SessionId` uses
/// `index()` — the ID shifted right by the two type bits — so "the obvious
/// wrong answer" is not hypothetical here, it is what the neighbouring
/// crate does.
///
/// Opening and resetting one bidirectional stream takes ID 0 out of
/// circulation, so the CONNECT lands on **4**, `index()` would say `1`, and
/// the session ID has exactly one correct value. The reset stream reaches
/// the server (QUIC opens lower-numbered streams implicitly, so it would
/// arrive even unwritten) and resolves to a stream error there, which the
/// fixture skips — see its `serve`.
#[tokio::test]
async fn a_bidirectional_stream_carries_bytes_both_ways() {
    let server = server::start(Options::default());
    let conn = server::dial(&server).await;
    let (mut stray, _) = conn
        .open_bi()
        .await
        .expect("stream credit on a fresh connection");
    stray
        .reset(quinn::VarInt::from_u32(0))
        .expect("a freshly opened stream can be reset");

    let session = Session::connect(conn, &uri(server.addr, "/echo"))
        .await
        .expect("the server announces WebTransport and answers 200");
    assert_ne!(
        session.id().value(),
        0,
        "the fixture must not leave the session on stream 0 — see this test's doc"
    );

    let (mut send, mut recv) = session.open_bi().await.expect("a session stream opens");
    send.write_all(b"ping")
        .await
        .expect("the stream carries bytes out");
    send.finish().expect("the write side ends");
    let echoed = recv
        .read_to_end(64 * 1024)
        .await
        .expect("the stream carries bytes back");
    assert_eq!(
        echoed, b"ping",
        "the peer read what was sent and sent it back"
    );

    // Causal: the fixture records the stream before it writes the echo, so
    // having read the echo is having observed the record.
    let seen = server.streams();
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0].signal, 0x41, "the WEBTRANSPORT_STREAM signal value");
    assert_eq!(
        seen[0].session_id,
        session.id().value(),
        "the stream names the session it belongs to"
    );
    assert_eq!(
        seen[0].session_id,
        server.requests()[0].stream_id,
        "and that is the CONNECT stream's ID"
    );
    assert_eq!(seen[0].payload, b"ping");
}

/// A peer whose SETTINGS do not announce WebTransport gets no CONNECT at
/// all — draft-ietf-webtrans-http3 §3.1's *"Clients MUST NOT attempt to
/// establish WebTransport sessions until they have received the settings
/// indicating WebTransport support from the server."*
///
/// The server here would answer `200` to a CONNECT if one arrived, which is
/// what makes the assertion causal rather than a race: a client that
/// skipped the gate — or that read the settings before they arrived, when
/// every flag is still at its `false` default — establishes a session and
/// this test fails on `is_err`, without depending on when the server got
/// round to recording anything.
#[tokio::test]
async fn a_peer_that_does_not_announce_webtransport_is_refused_before_the_connect() {
    let server = server::start(Options {
        announce_webtransport: false,
        ..Options::default()
    });
    let conn = server::dial(&server).await;

    let e = Session::connect(conn, &uri(server.addr, "/nope"))
        .await
        .expect_err("the peer never announced WebTransport");
    assert_eq!(e.kind(), &ErrorKind::Unsupported);
    let refused = std::error::Error::source(&e)
        .and_then(|s| s.downcast_ref::<NotSupportedByPeer>())
        .expect("the reason is typed, not a string");
    assert!(!refused.webtransport);
    assert!(
        refused.extended_connect,
        "the other half of the pair was announced, so this is the flag that decided it"
    );
    assert!(
        server.requests().is_empty(),
        "nothing was asked of a server that never offered"
    );
}

/// The same gate, from the other side: WebTransport announced, extended
/// CONNECT not.
///
/// It exists because a client checking only one of the two settings passes
/// the test above. They are two settings in two documents — RFC 9220's
/// `SETTINGS_ENABLE_CONNECT_PROTOCOL` and the WebTransport draft's own —
/// and a server can honestly be in either half-state: a deployment that
/// offers `connect-udp` (RFC 9298) and no WebTransport announces the first
/// alone, and one that announces the second alone is announcing a session
/// type over a mechanism it never offered.
#[tokio::test]
async fn extended_connect_alone_is_not_webtransport() {
    let server = server::start(Options {
        announce_webtransport: true,
        announce_extended_connect: false,
        ..Options::default()
    });
    let conn = server::dial(&server).await;

    let e = Session::connect(conn, &uri(server.addr, "/nope"))
        .await
        .expect_err("the peer announced no extended CONNECT");
    let refused = std::error::Error::source(&e)
        .and_then(|s| s.downcast_ref::<NotSupportedByPeer>())
        .expect("the reason is typed, not a string");
    assert!(refused.webtransport);
    assert!(!refused.extended_connect);
    assert!(server.requests().is_empty());
}

/// A refusal is the peer's answer, and it survives as one.
///
/// RFC 9220 makes `501` the answer to a `:protocol` a server does not
/// support, which is a different fact from `404`; replacing either with an
/// error of ours would hide a status the caller can act on.
#[tokio::test]
async fn a_refused_session_surfaces_the_peers_status() {
    let server = server::start(Options {
        answer: http::StatusCode::NOT_IMPLEMENTED,
        ..Options::default()
    });
    let conn = server::dial(&server).await;

    let e = Session::connect(conn, &uri(server.addr, "/nope"))
        .await
        .expect_err("the server answered 501");
    assert_eq!(e.kind(), &ErrorKind::Connect);
    let refused = std::error::Error::source(&e)
        .and_then(|s| s.downcast_ref::<SessionRefused>())
        .expect("the status is carried, not stringified");
    assert_eq!(refused.status, http::StatusCode::NOT_IMPLEMENTED);
    // The request did reach the server — this is a refusal, not a gate.
    assert_eq!(server.requests().len(), 1);
    assert_eq!(
        server.requests()[0].protocol.as_deref(),
        Some("webtransport")
    );
}

/// A plaintext session URI is refused rather than quietly promoted.
///
/// `h3`'s `Pseudo::request` defaults `:scheme` to `https` when the URI has
/// none and takes whatever scheme it finds otherwise, so a `http://`
/// session URI would either go out claiming to be TLS or go out claiming
/// to be plaintext over a connection that is not — and neither is a thing
/// to do without saying so.
#[tokio::test]
async fn a_plaintext_session_uri_is_refused_rather_than_rewritten() {
    let server = server::start(Options::default());
    let conn = server::dial(&server).await;
    let plaintext: http::Uri = format!("http://{}/echo", server.addr).parse().unwrap();

    let e = Session::connect(conn, &plaintext)
        .await
        .expect_err("WebTransport has no plaintext form");
    assert_eq!(e.kind(), &ErrorKind::Unsupported);
    let bad = std::error::Error::source(&e)
        .and_then(|s| s.downcast_ref::<NotHttps>())
        .expect("the reason is typed, not a string");
    assert_eq!(bad.scheme, "http");
    assert!(server.requests().is_empty());
}

/// **The datagram premise.** A WebTransport datagram leaves this stack,
/// arrives at a server that decodes RFC 9297's framing with its own
/// decoder, and comes back.
///
/// # What this asserts, and what it does not
///
/// It asserts *what* arrived: the Quarter Stream ID the server read is
/// this session's stream ID divided by four, and the payload is the
/// caller's bytes unchanged. It does **not** assert that a datagram is
/// delivered — nothing does, because nothing may: RFC 9221 datagrams are
/// unreliable by construction and a test claiming otherwise would be
/// claiming a promise the protocol refuses to make. What makes the
/// assertion sound here is loopback with no congestion and one datagram in
/// flight, which is a fact about this fixture rather than about
/// WebTransport.
///
/// The session ID relation is the whole of the wire format's arithmetic,
/// and it is asserted from the server's side: `quarter * 4 == session id`.
/// A client that sent the stream ID unshifted — the value it puts in a
/// *stream* header, three lines away in the same file — fails this line,
/// and then fails again on the echo, which it would address to a session
/// it is not listening for.
#[tokio::test]
async fn a_datagram_carries_bytes_both_ways() {
    let server = server::start(Options::default());
    let conn = server::dial(&server).await;
    take_stream_zero(&conn).await;

    let session = Session::connect(conn, &uri(server.addr, "/datagrams"))
        .await
        .expect("the server announces WebTransport and answers 200");
    assert_ne!(
        session.id().value(),
        0,
        "the fixture must not leave the session on stream 0 — see `take_stream_zero`"
    );

    session
        .send_datagram(Bytes::from_static(b"ping"))
        .expect("the peer announced datagrams and the connection carries them");

    let echoed = tokio::time::timeout(ARRIVAL, session.recv_datagram())
        .await
        .expect("a datagram sent on loopback arrives")
        .expect("the connection is up");
    assert_eq!(&echoed[..], b"ping", "the payload came back unchanged");

    // Causal: the echo exists only because the server read the datagram,
    // and it records it before echoing.
    let seen = server.datagrams();
    assert_eq!(seen.len(), 1, "exactly one datagram reached the server");
    assert_eq!(seen[0].payload, b"ping");
    assert_eq!(
        seen[0].quarter_stream_id * 4,
        session.id().value(),
        "RFC 9297 §2.1: the Quarter Stream ID is the session's stream ID divided by four"
    );
    assert_ne!(
        seen[0].quarter_stream_id,
        session.id().value(),
        "and it is not the stream ID itself, which is what a stream header carries"
    );
}

/// A datagram for another session, and one too short to name any, are both
/// dropped and the wait goes on.
///
/// RFC 9297 §2.1 makes this the receiver's duty rather than a courtesy:
/// a datagram whose Quarter Stream ID names a stream the receiver does not
/// know "SHALL either be dropped silently or buffered temporarily", and
/// handing it to the one session this connection has would be handing a
/// caller someone else's bytes.
///
/// # This one leans on ordering, and says so
///
/// The fixture sends the two rejects immediately before the echo, from one
/// task, so the three DATAGRAM frames are queued together and quinn packs
/// them into a single QUIC packet on loopback. That is what makes "the
/// first datagram to arrive is the echo" reliable here — it is not a
/// promise of the protocol, which orders datagrams not at all. The
/// mutation run is where it was checked rather than assumed: with the
/// mismatch arm removed this test fails, every time, on `b"stray"`.
#[tokio::test]
async fn a_datagram_for_another_session_is_discarded() {
    let server = server::start(Options {
        noise_before_echo: true,
        ..Options::default()
    });
    let conn = server::dial(&server).await;
    take_stream_zero(&conn).await;

    let session = Session::connect(conn, &uri(server.addr, "/noise"))
        .await
        .expect("the server announces WebTransport and answers 200");

    session
        .send_datagram(Bytes::from_static(b"ping"))
        .expect("datagrams are available");

    let got = tokio::time::timeout(ARRIVAL, session.recv_datagram())
        .await
        .expect("a datagram sent on loopback arrives")
        .expect("the connection is up");
    assert_eq!(
        &got[..],
        b"ping",
        "the short frame and the other session's datagram were both dropped"
    );
}

/// A server that announces WebTransport and no datagrams gets a session
/// anyway — and streams on it work.
///
/// This is the decision not to make `SETTINGS_H3_DATAGRAM` a third
/// condition on the establishment gate, pinned rather than described.
/// draft-ietf-webtrans-http3 does ask a server for all three settings, but
/// a server can honestly send two of them, `h3`'s own server builder can be
/// configured into exactly that state, and refusing the session would
/// charge a caller who only ever opens streams for a feature it never
/// used. What the missing setting costs instead is exactly the datagrams:
/// `max_datagram_size` is `None` and a send is a typed refusal that never
/// reaches the wire.
#[tokio::test]
async fn a_peer_without_h3_datagram_gets_a_session_and_refuses_datagrams() {
    let server = server::start(Options {
        announce_datagram: false,
        ..Options::default()
    });
    let conn = server::dial(&server).await;
    let session = Session::connect(conn, &uri(server.addr, "/streams-only"))
        .await
        .expect("WebTransport and extended CONNECT were both announced");

    assert!(
        session.max_datagram_size().is_none(),
        "no budget, because RFC 9297 §2.1 forbids sending this peer a datagram at all"
    );
    let e = session
        .send_datagram(Bytes::from_static(b"ping"))
        .expect_err("the peer never asked for HTTP Datagrams");
    assert_eq!(e.kind(), &ErrorKind::Unsupported);
    assert_eq!(
        source_of::<DatagramsUnavailable>(&e),
        &DatagramsUnavailable::NotAnnouncedByPeer,
        "the peer's HTTP/3 answer, not the QUIC connection under it"
    );

    // A stream still works, which is the whole point of not refusing the
    // session — and it is also what proves the refusal above was a
    // decision rather than a broken connection.
    let (mut send, mut recv) = session.open_bi().await.expect("a session stream opens");
    send.write_all(b"ping").await.expect("bytes go out");
    send.finish().expect("the write side ends");
    assert_eq!(
        recv.read_to_end(64 * 1024).await.expect("bytes come back"),
        b"ping"
    );
    assert!(
        server.datagrams().is_empty(),
        "no datagram reached a server that never asked for one"
    );
}

/// The other half of `None`: the peer's HTTP/3 SETTINGS say yes and its
/// QUIC endpoint offers no datagrams at all.
///
/// Two switches one layer apart — `SETTINGS_H3_DATAGRAM` and RFC 9221's
/// `max_datagram_frame_size` transport parameter — and the error says
/// which. It exists because a client that checked only the HTTP/3 setting
/// passes the test above and then hands quinn a datagram it cannot send;
/// and because a single `Unavailable` variant would tell a caller who owns
/// the endpoint nothing about the one thing they could fix.
#[tokio::test]
async fn a_connection_without_quic_datagrams_is_a_different_refusal() {
    let server = server::start(Options {
        quic_datagrams: false,
        ..Options::default()
    });
    let conn = server::dial(&server).await;
    let session = Session::connect(conn, &uri(server.addr, "/no-quic-datagrams"))
        .await
        .expect("WebTransport and extended CONNECT were both announced");

    assert!(session.max_datagram_size().is_none());
    let e = session
        .send_datagram(Bytes::from_static(b"ping"))
        .expect_err("the connection carries no datagrams");
    assert_eq!(e.kind(), &ErrorKind::Unsupported);
    assert_eq!(
        source_of::<DatagramsUnavailable>(&e),
        &DatagramsUnavailable::NotOnTheConnection,
        "the QUIC connection, not the peer's SETTINGS — which said yes here"
    );
}

/// The budget is the caller's bytes, header already subtracted — and the
/// proof is that exactly the budget fits on the wire.
///
/// A datagram has no smaller form: RFC 9221 puts it in one QUIC packet, so
/// a payload that does not fit is refused rather than split. The
/// interesting half is the other one: `max_datagram_size` must subtract
/// this session's Quarter Stream ID varint from quinn's frame limit, and a
/// budget that forgot to would be one byte too generous — which quinn, not
/// this crate, would then reject. So the success below is the assertion
/// and the refusal above is only its bracket.
#[tokio::test]
async fn the_datagram_budget_is_the_payload_the_wire_accepts() {
    let server = server::start(Options::default());
    let conn = server::dial(&server).await;
    take_stream_zero(&conn).await;
    let session = Session::connect(conn, &uri(server.addr, "/budget"))
        .await
        .expect("the server announces WebTransport and answers 200");

    let budget = session
        .max_datagram_size()
        .expect("the peer announced datagrams and the connection carries them");

    let e = session
        .send_datagram(Bytes::from(vec![0u8; budget + 1]))
        .expect_err("one byte over the budget does not fit");
    assert_eq!(e.kind(), &ErrorKind::Body);
    assert_eq!(
        source_of::<DatagramTooLarge>(&e),
        &DatagramTooLarge {
            payload: budget + 1,
            budget,
        }
    );

    session
        .send_datagram(Bytes::from(vec![7u8; budget]))
        .expect("exactly the budget fits — if this fails, the header was not subtracted");
    assert!(
        server.wait_for_datagrams(1, ARRIVAL).await,
        "and it fits on the wire, not only past this crate's own check"
    );
    let seen = server.datagrams();
    assert_eq!(seen.len(), 1, "the over-budget one never left");
    assert_eq!(seen[0].payload.len(), budget);
    assert_eq!(seen[0].quarter_stream_id * 4, session.id().value());
}

// ---------------------------------------------------------------------------
// The capsule protocol, and the end of a session
// ---------------------------------------------------------------------------

/// **The close premise.** A `CLOSE_WEBTRANSPORT_SESSION` capsule leaves
/// this stack, carrying the caller's application error code and reason, and
/// the CONNECT stream ends behind it.
///
/// The capsule protocol was recorded as not done, with a condition: a real
/// one carries an application error code and a reason string on the CONNECT
/// stream. This is that, executed: the bytes asserted are the ones the
/// server's own varint
/// decoder took off the wire, and the payload is compared **unparsed**, so
/// the two sides cannot agree by sharing a decoder.
///
/// # The ordering is causal
///
/// The fixture reads the capsule, records it, waits for the client's FIN,
/// records that, and only then lets its own half of the stream drop. Bytes
/// on one QUIC stream are ordered, so a client that has seen this side
/// close has causally already had both recorded. Nothing here waits on a
/// duration except the hang guard.
#[tokio::test]
async fn a_close_capsule_carries_the_code_and_the_reason_to_the_peer() {
    let server = server::start(Options::default());
    let conn = server::dial(&server).await;
    let session = Session::connect(conn, &uri(server.addr, "/bye"))
        .await
        .expect("the fixture announces WebTransport");

    session
        .close(0x1234_5678, "so long")
        .await
        .expect("the CONNECT stream is open and the reason is short");

    // The fixture closes its half only after reading ours to its end.
    let end = ended(&session).await.expect("the fixture FINs cleanly");
    assert_eq!(
        end,
        SessionClose {
            code: 0,
            reason: String::new()
        }
    );

    // draft-ietf-webtrans-http3 §5's payload, spelled out: four big-endian
    // bytes of application error code, then the reason as UTF-8.
    let mut expected = vec![0x12, 0x34, 0x56, 0x78];
    expected.extend_from_slice(b"so long");
    assert_eq!(
        server.capsules(),
        vec![server::SeenCapsule {
            kind: 0x2843,
            payload: expected,
        }]
    );
    // The FIN is the draft's second half, and it is a separate line in the
    // client from the capsule.
    assert!(
        server.client_fin(),
        "the CONNECT stream is finished behind the capsule"
    );
}

/// The peer's close capsule is the session's end, with its code and its
/// reason.
#[tokio::test]
async fn the_peers_close_capsule_is_the_sessions_end() {
    let server = server::start(Options {
        after_response: AfterResponse::Close {
            code: 42,
            reason: "server done".into(),
        },
        ..Options::default()
    });
    let conn = server::dial(&server).await;
    let session = Session::connect(conn, &uri(server.addr, "/bye"))
        .await
        .expect("the fixture announces WebTransport");

    let end = ended(&session).await.expect("a capsule is a clean close");
    assert_eq!(
        end,
        SessionClose {
            code: 42,
            reason: "server done".into()
        }
    );
}

/// A CONNECT stream that simply ends is a clean close with zeroes.
///
/// draft-ietf-webtrans-http3 §5: *"Cleanly terminating a CONNECT stream
/// without sending a `CLOSE_WEBTRANSPORT_SESSION` capsule SHALL be
/// semantically equivalent to terminating it with a
/// `CLOSE_WEBTRANSPORT_SESSION` capsule that has an error code of 0 and an
/// empty error string."* So this is `Ok`, not `Err`, and it is the reason
/// the clean/unclean distinction cannot be "did a capsule arrive".
#[tokio::test]
async fn a_bare_fin_is_a_clean_close_with_zeroes() {
    let server = server::start(Options {
        after_response: AfterResponse::Fin,
        ..Options::default()
    });
    let conn = server::dial(&server).await;
    let session = Session::connect(conn, &uri(server.addr, "/bye"))
        .await
        .expect("the fixture announces WebTransport");

    let end = ended(&session).await.expect("a FIN is a clean close");
    assert_eq!(
        end,
        SessionClose {
            code: 0,
            reason: String::new()
        }
    );
}

/// **The distinction.** A reset CONNECT stream is not a clean close.
///
/// This is the test the whole feature exists for. A session that ends
/// because the peer said so and a session that ends because the peer went
/// away are the same event to everything else in this crate — the next
/// `open_bi` fails either way — and telling them apart is what
/// [`Session::closed`] adds. It is `hclient-fetch`'s `wasClean` for a
/// WebSocket, and the error kind agrees with that one on purpose.
///
/// The fixture holds the connection open after the reset, so what is
/// observed here is a *stream* ending abruptly and not a connection
/// disappearing — the two are separate tests because a client could get one
/// right and the other wrong.
#[tokio::test]
async fn a_reset_connect_stream_is_not_a_clean_close() {
    let server = server::start(Options {
        after_response: AfterResponse::Reset,
        ..Options::default()
    });
    let conn = server::dial(&server).await;
    let session = Session::connect(conn, &uri(server.addr, "/bye"))
        .await
        .expect("the fixture announces WebTransport");
    // Causal, not timed: the reset cannot happen before this line, so it
    // cannot destroy the response the line above is still reading.
    server.abandon_the_session();

    let e = ended(&session)
        .await
        .expect_err("a reset stream is not the peer saying it is done");
    assert_eq!(e.kind(), &ErrorKind::Body);
    // And deliberately not a `BadCloseCapsule`: nothing was malformed,
    // there was simply nothing.
    assert!(
        std::error::Error::source(&e)
            .and_then(|s| s.downcast_ref::<BadCloseCapsule>())
            .is_none()
    );
}

/// **The distinction, one layer down.** A connection that vanishes is not a
/// clean close either.
#[tokio::test]
async fn a_connection_that_vanishes_is_not_a_clean_close() {
    let server = server::start(Options {
        after_response: AfterResponse::AbortConnection,
        ..Options::default()
    });
    let conn = server::dial(&server).await;
    let session = Session::connect(conn, &uri(server.addr, "/bye"))
        .await
        .expect("the fixture announces WebTransport");
    server.abandon_the_session();

    let e = ended(&session)
        .await
        .expect_err("a connection closed under the session is not a close");
    assert_eq!(e.kind(), &ErrorKind::Body);
}

/// An unknown capsule is skipped, and the close behind it is still read.
///
/// RFC 9297 §3.2: *"An endpoint that receives a capsule with an unknown
/// Capsule Type MUST silently skip over that capsule."* The fixture sends
/// `DRAIN_WEBTRANSPORT_SESSION` — the draft's other capsule, and the one
/// this client deliberately does not surface — with a **non-empty**
/// payload, so a reader that ignored the length field would take the drain
/// for the close, or the close for part of the drain.
#[tokio::test]
async fn an_unknown_capsule_is_skipped_and_the_close_behind_it_is_read() {
    let server = server::start(Options {
        after_response: AfterResponse::UnknownThenClose {
            code: 7,
            reason: "after the drain".into(),
        },
        ..Options::default()
    });
    let conn = server::dial(&server).await;
    let session = Session::connect(conn, &uri(server.addr, "/bye"))
        .await
        .expect("the fixture announces WebTransport");

    let end = ended(&session)
        .await
        .expect("the second capsule is a close");
    assert_eq!(
        end,
        SessionClose {
            code: 7,
            reason: "after the drain".into()
        }
    );
}

/// A capsule cut across two DATA frames is one capsule.
///
/// RFC 9297 §3.2 puts capsules in the payload of DATA frames and says
/// nothing about aligning the two, so a frame boundary is not a capsule
/// boundary. The cut here is at **two bytes** — inside the capsule's own
/// type field, not merely inside its payload — so a reader that treated
/// each DATA frame as a whole capsule sees a type of `0x68`, a length it
/// never gets, and no close at all.
#[tokio::test]
async fn a_capsule_split_across_two_data_frames_is_one_capsule() {
    let server = server::start(Options {
        after_response: AfterResponse::CloseInTwoFrames {
            code: 9,
            reason: "in two".into(),
            at: 2,
        },
        ..Options::default()
    });
    let conn = server::dial(&server).await;
    let session = Session::connect(conn, &uri(server.addr, "/bye"))
        .await
        .expect("the fixture announces WebTransport");

    let end = ended(&session)
        .await
        .expect("two frames carrying one capsule are one close");
    assert_eq!(
        end,
        SessionClose {
            code: 9,
            reason: "in two".into()
        }
    );
}

/// A close capsule with no room for an error code is not a close.
#[tokio::test]
async fn a_close_capsule_without_an_error_code_is_not_a_clean_close() {
    // Capsule type 0x2843 as a two-byte varint, a length of 2, and two
    // bytes — one short of half the error code the draft requires.
    let server = server::start(Options {
        after_response: AfterResponse::Raw(vec![0x68, 0x43, 0x02, 0x00, 0x2a]),
        ..Options::default()
    });
    let conn = server::dial(&server).await;
    let session = Session::connect(conn, &uri(server.addr, "/bye"))
        .await
        .expect("the fixture announces WebTransport");

    let e = ended(&session).await.expect_err("two bytes are not a code");
    assert_eq!(e.kind(), &ErrorKind::Body);
    assert_eq!(
        source_of::<BadCloseCapsule>(&e),
        &BadCloseCapsule::NoErrorCode { payload: 2 }
    );
}

/// A close reason that is not UTF-8 is not a close.
#[tokio::test]
async fn a_close_reason_that_is_not_utf8_is_not_a_clean_close() {
    let server = server::start(Options {
        // 0x2843, length 6, error code 0, then two bytes no UTF-8 decoder
        // accepts.
        after_response: AfterResponse::Raw(vec![
            0x68, 0x43, 0x06, 0x00, 0x00, 0x00, 0x00, 0xff, 0xfe,
        ]),
        ..Options::default()
    });
    let conn = server::dial(&server).await;
    let session = Session::connect(conn, &uri(server.addr, "/bye"))
        .await
        .expect("the fixture announces WebTransport");

    let e = ended(&session).await.expect_err("0xff 0xfe is not UTF-8");
    assert_eq!(
        source_of::<BadCloseCapsule>(&e),
        &BadCloseCapsule::ReasonNotUtf8
    );
}

/// A close reason over the draft's limit is not a close, in either
/// direction — and the limit is the same number both ways.
///
/// The receiving half is here; the sending half is
/// `a_reason_over_the_limit_is_refused_before_anything_is_sent`. One type,
/// `BadCloseCapsule::ReasonTooLong`, because it is one sentence of the
/// draft.
#[tokio::test]
async fn a_close_reason_over_the_limit_is_not_a_clean_close() {
    let over = BadCloseCapsule::MAX_REASON + 1;
    let mut raw = vec![0x68, 0x43];
    // The capsule length, as a four-byte QUIC varint: 4 + 1025 = 1029.
    raw.extend_from_slice(&((4 + over as u32) | (0b10 << 30)).to_be_bytes());
    raw.extend_from_slice(&0u32.to_be_bytes());
    raw.extend(std::iter::repeat_n(b'x', over));
    let server = server::start(Options {
        after_response: AfterResponse::Raw(raw),
        ..Options::default()
    });
    let conn = server::dial(&server).await;
    let session = Session::connect(conn, &uri(server.addr, "/bye"))
        .await
        .expect("the fixture announces WebTransport");

    let e = ended(&session).await.expect_err("1025 bytes is over 1024");
    assert_eq!(
        source_of::<BadCloseCapsule>(&e),
        &BadCloseCapsule::ReasonTooLong { len: over }
    );
}

/// A stream that ends part way through a capsule is not a clean close.
///
/// The difference from `a_bare_fin_is_a_clean_close_with_zeroes` is the
/// four bytes left over: there the stream ended *at* a capsule boundary,
/// here it ended inside one, and what the peer meant to say went with the
/// rest of it. A reader that answered "clean, code 0" to both would be
/// reporting a truncated close as a deliberate one.
#[tokio::test]
async fn a_stream_that_ends_inside_a_capsule_is_not_a_clean_close() {
    // 0x2843, a length of 8, and only four bytes of payload before the FIN.
    let server = server::start(Options {
        after_response: AfterResponse::Raw(vec![0x68, 0x43, 0x08, 0x00, 0x00, 0x00, 0x01]),
        ..Options::default()
    });
    let conn = server::dial(&server).await;
    let session = Session::connect(conn, &uri(server.addr, "/bye"))
        .await
        .expect("the fixture announces WebTransport");

    let e = ended(&session)
        .await
        .expect_err("half a capsule is not a close");
    assert_eq!(
        source_of::<BadCloseCapsule>(&e),
        &BadCloseCapsule::Truncated { have: 7 }
    );
}

/// The end is remembered, not read again.
///
/// A second `closed()` on a session that ended with code 42 must not answer
/// `{ code: 0 }` — which is exactly what re-reading an already-ended stream
/// gives, because an ended stream is at EOF and EOF is draft §5's bare-FIN
/// close. The same trap applies to an unclean end: read twice, a reset
/// session reports itself clean the second time.
#[tokio::test]
async fn the_end_is_remembered_rather_than_read_again() {
    let server = server::start(Options {
        after_response: AfterResponse::Close {
            code: 42,
            reason: "once".into(),
        },
        ..Options::default()
    });
    let conn = server::dial(&server).await;
    let session = Session::connect(conn, &uri(server.addr, "/bye"))
        .await
        .expect("the fixture announces WebTransport");

    let first = ended(&session).await.expect("a capsule is a clean close");
    let second = ended(&session).await.expect("and it stays one");
    assert_eq!(first, second);
    assert_eq!(
        second,
        SessionClose {
            code: 42,
            reason: "once".into()
        }
    );
}

/// The same, for an end that was not clean: a reset session stays reset.
#[tokio::test]
async fn an_unclean_end_is_remembered_too() {
    let server = server::start(Options {
        after_response: AfterResponse::Reset,
        ..Options::default()
    });
    let conn = server::dial(&server).await;
    let session = Session::connect(conn, &uri(server.addr, "/bye"))
        .await
        .expect("the fixture announces WebTransport");
    server.abandon_the_session();

    ended(&session).await.expect_err("a reset is not clean");
    let again = ended(&session)
        .await
        .expect_err("and asking twice does not make it clean");
    assert_eq!(again.kind(), &ErrorKind::Body);
}

/// A reason over the draft's limit is refused **before anything is sent**,
/// and the session is still open afterwards.
///
/// Refusing rather than truncating is the point: a peer that enforces the
/// limit — `wtransport` 0.7.2 does — treats an over-long reason as a
/// protocol error and closes the *connection*, so a client that sent it
/// would turn a clean close into the one outcome a clean close exists to
/// avoid. That nothing was sent is asserted causally rather than by
/// absence: the session is used afterwards, and the fixture records the
/// stream that use opens.
#[tokio::test]
async fn a_reason_over_the_limit_is_refused_before_anything_is_sent() {
    let server = server::start(Options::default());
    let conn = server::dial(&server).await;
    let session = Session::connect(conn, &uri(server.addr, "/bye"))
        .await
        .expect("the fixture announces WebTransport");

    let over = "x".repeat(BadCloseCapsule::MAX_REASON + 1);
    let e = session
        .close(0, &over)
        .await
        .expect_err("1025 bytes is over the draft's 1024");
    assert_eq!(e.kind(), &ErrorKind::Unsupported);
    assert_eq!(
        source_of::<BadCloseCapsule>(&e),
        &BadCloseCapsule::ReasonTooLong {
            len: BadCloseCapsule::MAX_REASON + 1
        }
    );

    // Exactly the limit is not over it — the boundary, in the direction a
    // `>=` would get wrong.
    session
        .close(0, &"y".repeat(BadCloseCapsule::MAX_REASON))
        .await
        .expect("1024 bytes is the limit, not one past it");
    let _ = ended(&session).await;
    assert_eq!(server.capsules().len(), 1, "the refusal sent nothing");
    assert_eq!(
        server.capsules()[0].payload.len(),
        4 + BadCloseCapsule::MAX_REASON
    );
}

/// Closing twice is refused, rather than silently reporting a code that
/// never left.
///
/// `close` carries an application error code the peer acts on, so an `Ok`
/// to a second call with a different code would be a lie about the wire.
/// There is one capsule per session because the stream it travels on is
/// finished by the first.
#[tokio::test]
async fn closing_twice_is_refused_rather_than_silently_dropped() {
    let server = server::start(Options::default());
    let conn = server::dial(&server).await;
    let session = Session::connect(conn, &uri(server.addr, "/bye"))
        .await
        .expect("the fixture announces WebTransport");

    session.close(1, "first").await.expect("the first close");
    let e = session
        .close(2, "second")
        .await
        .expect_err("there is only one CONNECT stream to finish");
    assert_eq!(e.kind(), &ErrorKind::Unsupported);
    assert_eq!(source_of::<AlreadyClosed>(&e), &AlreadyClosed);

    let _ = ended(&session).await;
    assert_eq!(server.capsules().len(), 1);
    let mut expected = vec![0, 0, 0, 1];
    expected.extend_from_slice(b"first");
    assert_eq!(server.capsules()[0].payload, expected);
}

/// A `Session` can be spawned, and it can be shared.
///
/// `Send` was true before the capsule protocol; it is asserted because the
/// two `Mutex`es the CONNECT stream now sits behind could have taken it
/// away. `Sync` is **new** with them, and it is the property the `&self` on
/// `close` and `closed` exists for: waiting for the peer's close in one
/// task while another opens streams means an `Arc<Session>` in two places,
/// which needs both.
///
/// # Why it is here rather than beside the code
///
/// `scripts/no-send-or-sync-in-the-core-surface.sh` scans `crates/*/src`
/// for a declared `Send` or `Sync` bound and demands a
/// `send-bound-exception: amendment-C…` marker on every one — and that
/// marker names a spec amendment that excuses a **seam** bound. None of
/// them excuses a test, so writing this in `src/` would mean spending a
/// marker on something no amendment covers. A test directory is not the
/// core surface, and this is a property of the public API as a consumer
/// meets it.
#[test]
fn a_session_can_be_spawned_and_shared() {
    fn is_send<T: Send>() {}
    fn is_sync<T: Sync>() {}
    is_send::<Session>();
    is_sync::<Session>();
}
