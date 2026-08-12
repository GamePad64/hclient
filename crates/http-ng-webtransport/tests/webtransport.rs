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

use http_ng_core::ErrorKind;
use http_ng_webtransport::{NotHttps, NotSupportedByPeer, Session, SessionRefused};
use server::Options;

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
