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
use http_ng_core::ErrorKind;
use http_ng_webtransport::{
    DatagramTooLarge, DatagramsUnavailable, NotHttps, NotSupportedByPeer, Session, SessionRefused,
};
use server::Options;

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

fn source_of<T: std::error::Error + 'static>(e: &http_ng_core::Error) -> &T {
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
/// `docs/w4-upgrade-seam.md` §4 asserted this was reachable from `h3`
/// 0.0.8's client API by reading three files. This is the reading,
/// executed: the value the server reports is the one its own QPACK decoder
/// took off the wire, not a type the two sides share by construction.
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
    // a stale paragraph in a document — `docs/v04-w2-webtransport.md` §3.
    assert!(
        !announced.webtransport,
        "h3 0.0.8's client cannot announce SETTINGS_ENABLE_WEBTRANSPORT; if this fails, it can now"
    );
    // The one the client *can* make, and the reason datagrams exist in
    // this crate at all: `h3::client::Builder::enable_datagram` is the
    // setter §3 of `docs/v04-w2-webtransport.md` found missing one feature
    // over. RFC 9297 §2.1 makes it the peer's licence to send us any HTTP
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
