//! A server that answers without reading the request body, and one that
//! dies at the same moment. **The pair is the claim**; either alone reads
//! as an accident, and either alone is passed by a wrong implementation.
//!
//! # What went wrong, and how it was found
//!
//! `live.rs`'s `a_real_request_over_real_quic` failed roughly once in
//! twenty **only under load** — never in 150 isolated runs, four times in
//! 90 runs of six concurrent processes — always with
//! `Error { kind: Body, source: "Remote reset: 0x0" }`, and always on a
//! connection's first request. The chain:
//!
//! 1. the client writes its HEADERS frame and the server resolves it;
//! 2. the test server answers **without reading the request body**, as a
//!    server answering `404`/`401`/`413` does, and its task ends. Dropping
//!    a `quinn::RecvStream` that was not read to the end sends
//!    `STOP_SENDING(0)` (`quinn-0.11.11/src/recv_stream.rs:534`);
//! 3. the client then calls `finish()`, which on a connection's **first**
//!    request writes h3's grease frame (`h3-0.0.8/src/connection.rs:1101`)
//!    — a real write, which fails `Stopped(0)`;
//! 4. `one_attempt` propagated it and returned **before `recv_response`**,
//!    discarding a response that had never been affected, because
//!    `STOP_SENDING` acts on one direction only.
//!
//! RFC 9114 §4.1: *"Clients MUST NOT discard complete responses as a result
//! of having their request terminated abruptly."* So it was a defect in
//! this crate rather than in its fixture, and load only decided who won the
//! race.
//!
//! # Why a four-megabyte body
//!
//! To take the race out. The failure above needed a loaded machine because
//! the client usually finishes its grease frame first. A body far larger
//! than the flow-control window cannot be written before the server has
//! answered and dropped its half, so the `STOP_SENDING` always arrives
//! mid-write and the tests measure the decision rather than the scheduler.
#![cfg(all(feature = "http3", not(target_family = "wasm")))]

#[path = "h3_server.rs"]
mod server;

use hclient_core::RequestBody;
use hclient_core::unversioned::Transport;
use hclient_dns::IpLiteralOnly;
use hclient_native::H3;
use hclient_rt_tokio::TokioHandle;
use http_body_util::BodyExt;
use server::Behaviour;
use std::time::Duration;

/// Larger than quinn's default 1.25 MiB stream flow-control window, so the
/// client is certainly still writing when the answer comes back.
const BODY: usize = 4 * 1024 * 1024;

fn post(addr: std::net::SocketAddr) -> http::Request<RequestBody> {
    http::Request::builder()
        .method("POST")
        .uri(format!("https://{addr}/ignored"))
        .body(RequestBody::Full(bytes::Bytes::from(vec![0u8; BODY])))
        .unwrap()
}

/// The concrete type rather than `impl Transport`: the second test reads
/// `Error::kind()`, and behind the opaque type `Transport::Error` is an
/// associated type with no methods.
fn h3(
    cert: &rustls::pki_types::CertificateDer<'static>,
) -> H3<TokioHandle, hclient_tls_rustls::Rustls, IpLiteralOnly> {
    H3::new(
        TokioHandle::current().expect("inside #[tokio::test]"),
        server::client_tls(cert),
        IpLiteralOnly,
    )
    .expect("H3::new does no I/O")
}

#[tokio::test(flavor = "multi_thread")]
async fn a_server_that_stops_reading_the_body_still_gets_its_response_read() {
    let s = server::start(Behaviour::Echo);
    let t = h3(&s.cert_der);

    let r = t
        .execute(post(s.addr))
        .await
        .expect("the response was never affected: STOP_SENDING acts on one direction");
    assert_eq!(r.status(), 200);
    let bytes = r.into_body().collect().await.unwrap().to_bytes();
    assert_eq!(
        &bytes[..],
        b"hello over h3",
        "and the body too, not just the head"
    );
    assert_eq!(s.requests(), 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_connection_that_dies_mid_body_is_still_an_error() {
    // The other half, and the one that keeps the tolerance narrow. Same
    // moment, same unwritten body, and the only difference is that the peer
    // is gone rather than merely uninterested — so there is no response to
    // wait for and none will come.
    //
    // This does NOT pin the tolerance's narrowness, and saying so is the
    // point. Measured: with the match widened to `Err(_) => Ok(true)` this
    // test still passes — a client that swallowed the write error reaches
    // `recv_response` on a dead connection, and that call fails too, with
    // the same `ErrorKind::Body`. No hang; the two spellings look identical
    // from out here. What this test does pin is that the failure reaches
    // the caller at all, which the test above cannot show because it
    // asserts a success. The argument for the narrow match is written where
    // the match is, in `write_after_head`, and it is a construction
    // argument rather than a measured one.
    let s = server::start(Behaviour::DieAfterHead);
    let t = h3(&s.cert_der);

    let out = tokio::time::timeout(Duration::from_secs(10), t.execute(post(s.addr)))
        .await
        .expect("a dead connection must fail, not hang");
    let err = out.expect_err("the peer closed the connection; there is no response");
    // Not a string match on the QUIC error, which is not a stable
    // interface: what is pinned is that the failure reached the caller at
    // all, and as a transfer failure rather than as a success.
    assert_eq!(
        err.kind(),
        &hclient_core::ErrorKind::Body,
        "the connection was up and the handshake done, so this is a \
         transfer failure: {err}"
    );
    assert_eq!(s.requests(), 1, "the server did read the head before dying");
}
