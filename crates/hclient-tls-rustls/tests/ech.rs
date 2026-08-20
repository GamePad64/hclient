//! ECH is refused, and the refusal is measured on the wire.
//!
//! # Why the observer is a socket rather than an error
//!
//! `assert!(connect(..).is_err())` would pass for a backend that sent a
//! ClientHello, leaked the server name, and *then* returned an error —
//! which is the failure this refusal exists to prevent, not the behaviour
//! it asks for. So the assertion below belongs to the peer: a plain
//! `TcpListener` that records every byte it is sent. With `ech: Some(_)`
//! it must receive nothing at all.
//!
//! # Its control is the leak itself
//!
//! A test that only checks "no bytes arrived" would also pass against a
//! listener nobody ever connected to, or against an observer that cannot
//! see anything. So the same fixture runs a second time with `ech: None`,
//! and then the server MUST see the server name in the clear, inside the
//! plaintext ClientHello. That is the thing ECH is for; showing that this
//! observer can see it is what makes its silence in the first test worth
//! something.

use hclient_core::ErrorKind;
use hclient_rt::TcpConnect;
use hclient_rt_tokio::Tokio;
use hclient_tls::{TlsConnect, TlsRequest};
use hclient_tls_rustls::Rustls;
use std::io::Read;
use std::net::SocketAddr;
use std::sync::mpsc;
use std::time::Duration;

/// The name the client asks to have protected. Long and unmistakable, so
/// that finding it inside a ClientHello is not a coincidence.
const SERVER_NAME: &str = "secret-name-that-must-not-leak.example.com";

/// How long the observer waits for bytes that should not come. Short: it
/// bounds only the negative test, and a ClientHello on loopback is written
/// in microseconds.
const QUIET_WINDOW: Duration = Duration::from_millis(500);

/// A peer that never answers, and reports what it was sent.
///
/// It deliberately does NOT complete a TLS handshake: everything asserted
/// here happens in the client's first flight, and a server that answered
/// would only add a way for the test to fail for an unrelated reason.
fn recording_peer() -> (SocketAddr, mpsc::Receiver<Vec<u8>>) {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = listener.local_addr().expect("local_addr");
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let Ok((mut sock, _)) = listener.accept() else {
            return;
        };
        sock.set_read_timeout(Some(QUIET_WINDOW))
            .expect("read timeout");
        let mut seen = Vec::new();
        let mut chunk = [0u8; 4096];
        // One read is enough for a ClientHello on loopback; the loop is
        // for the case where it arrives split across segments.
        while let Ok(n) = sock.read(&mut chunk) {
            if n == 0 {
                break;
            }
            seen.extend_from_slice(&chunk[..n]);
            if seen.len() > 64 {
                break;
            }
        }
        let _ = tx.send(seen);
    });
    (addr, rx)
}

async fn connect_with(ech: Option<&[u8]>, addr: SocketAddr) -> Result<(), hclient_core::Error> {
    let tls = Rustls::with_webpki_roots();
    let tcp = Tokio
        .connect(addr, &hclient_rt::TcpOpts::default())
        .await
        .expect("loopback connect");
    tls.connect(
        tcp,
        TlsRequest {
            server_name: SERVER_NAME,
            alpn: &[b"h2"],
            ech,
            early_data: None,
        },
    )
    .await
    .map(|_| ())
}

/// An `EchConfigList` is opaque to this test: the refusal must not depend
/// on it parsing, because a caller who passed a malformed one is asking
/// for the same protection as one who passed a good one.
const SOME_ECH_CONFIG: &[u8] = &[0x00, 0x41, 0xfe, 0x0d];

#[tokio::test]
async fn an_ech_request_puts_nothing_on_the_wire() {
    let (addr, seen) = recording_peer();

    let err = connect_with(Some(SOME_ECH_CONFIG), addr)
        .await
        .expect_err("ECH must be refused, not ignored");

    // The wire first, deliberately: it is the claim, and the error's shape
    // is only how the caller learns of it. Asserting the message before the
    // bytes would let a backend that leaked the name and then complained
    // fail this test on the wrong line.
    let bytes = seen.recv().expect("the observer thread must report");
    assert!(
        bytes.is_empty(),
        "the refusal must come before the ClientHello: the peer received {} bytes \
         ({:?}…), so the name the caller asked to encrypt was already on the wire",
        bytes.len(),
        &bytes[..bytes.len().min(16)]
    );

    assert_eq!(*err.kind(), ErrorKind::Tls, "{err}");
    assert!(
        err.to_string().to_lowercase().contains("ech"),
        "the error must name what was refused, so a caller can tell it from \
         a handshake failure: {err}"
    );
}

/// The control: the same request without ECH reaches the wire, and what
/// reaches it contains the server name in plaintext.
///
/// Two things at once. It proves the refusal above is conditional on
/// `ech` rather than on something incidental about the fixture, and it
/// exhibits the leak — so "no bytes at all" in the test above is a
/// measurement by an observer that has been shown capable of seeing one.
#[tokio::test]
async fn without_ech_the_name_goes_out_in_the_clear() {
    let (addr, seen) = recording_peer();

    // The handshake cannot complete — the peer never answers — so the
    // result is uninteresting. Only what the peer saw matters.
    let _ = tokio::time::timeout(Duration::from_secs(5), connect_with(None, addr)).await;

    let bytes = seen.recv().expect("the observer thread must report");
    assert!(
        !bytes.is_empty(),
        "the control must actually send a ClientHello, or the test above is \
         measuring an observer that sees nothing either way"
    );
    let needle = SERVER_NAME.as_bytes();
    assert!(
        bytes.windows(needle.len()).any(|w| w == needle),
        "the plaintext ClientHello must carry the server name — that is the \
         leak ECH exists to prevent, and the reason the request above is \
         refused rather than downgraded to this"
    );
}
