//! A body far larger than rustls' plaintext buffer, over **HTTP/3**.
//!
//! This is the h3 counterpart of `http2_over_tls_large_body.rs`, and it
//! is written to pin a **negative**: the rustls backpressure defect that
//! blocked `act`'s blob pulls cannot exist on this path, and the reason
//! is structural rather than a matter of degree.
//!
//! That defect lived in `hclient-tls-rustls`'s `pump_incoming`, which
//! feeds ciphertext to a `rustls::ClientConnection` through `read_tls`
//! and used to treat its backpressure refusal as a failure. Two things
//! had to line up to reach it: a body far past the 64 KiB plaintext
//! buffer, and a reader driven independently of whoever consumes the
//! body — which on TCP is HTTP/2's connection driver, where on HTTP/1.1
//! the body's own reader sets the rhythm and nothing accumulates.
//!
//! **Neither half of that machinery is on the HTTP/3 path.** QUIC does
//! its own TLS through `QuicTlsConnect`, so quinn owns the handshake and
//! the record layer: there is no `TlsStream`, no `pump_incoming` and no
//! `read_tls` anywhere under `hclient-native`'s `http3` module —
//! measured, zero occurrences of either name in that directory. The
//! plaintext buffer whose limit the defect turned on does not exist
//! here, so there is nothing for a large body to overflow.
//!
//! So this file cannot regress *that* fix, and it is not here to. It is
//! here because "structurally impossible" is a claim about the shape of
//! the code, and a claim is exactly as perishable as the check behind it
//! — this workspace's own recurring lesson. A refactor that routed h3
//! through the TCP TLS stream would make the impossibility false, and
//! this test is what would notice. It also covers the ordinary thing no
//! other h3 test does: that a multi-megabyte response arrives whole,
//! where every other body in `h3_live.rs` is thirteen bytes.
#![cfg(all(feature = "http3", not(target_family = "wasm")))]

#[path = "h3_server.rs"]
mod server;

use hclient_core::body::RequestBody;
use hclient_core::transport::Transport;
use hclient_dns::IpLiteralOnly;
use hclient_native::H3;
use hclient_rt_tokio::TokioHandle;
use http_body_util::BodyExt;
use server::Behaviour;

/// 8 MiB — the same figure the h2 test uses, and for the same reason:
/// 128 times rustls' 64 KiB plaintext buffer, so a path that had one
/// would cross it many times over rather than marginally.
const TOTAL: usize = 8 * 1024 * 1024;

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
async fn an_eight_megabyte_body_over_h3_arrives_whole() {
    let s = server::start(Behaviour::PushBlob(TOTAL));
    let t = h3(&s.cert_der);

    let req = http::Request::builder()
        .uri(format!("https://{}/blob", s.addr))
        .body(RequestBody::Empty)
        .expect("a well-formed request");

    let resp = tokio::time::timeout(std::time::Duration::from_secs(90), t.execute(req))
        .await
        .expect("the exchange must not hang")
        .expect("the exchange completes");
    assert_eq!(resp.status(), 200);
    // Without this the file would be a large-body test over whatever the
    // transport happened to negotiate, and the whole point is which
    // stack carried it.
    assert_eq!(
        resp.version(),
        http::Version::HTTP_3,
        "this must be the QUIC stack, or the negative it pins has no subject"
    );

    let body = tokio::time::timeout(
        std::time::Duration::from_secs(90),
        resp.into_body().collect(),
    )
    .await
    .expect("the body must not hang")
    .expect("the body arrives without a TLS error")
    .to_bytes();

    assert_eq!(body.len(), TOTAL, "the whole blob");
    assert!(body.iter().all(|b| *b == b'y'), "byte for byte");
}
