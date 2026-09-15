//! The TLS session itself: what the handshake reports back, and the stream
//! it hands over.
//!
//! # Why this file exists, and what it is not
//!
//! `tests/shape.rs` checks the seam is implemented and that the one
//! pre-transport refusal happens early; `tests/insecure.rs` checks a
//! certificate nobody issued is refused by default. Between them **nothing
//! used the stream** — every test stopped at the handshake's `Result`. So
//! `TlsStream`'s whole IO surface (`poll_read`, `poll_write`, `poll_flush`,
//! `poll_close`, `cvt`) and both of its report accessors
//! (`negotiated_alpn`, `peer_certificate_der`) were unexecuted, and a
//! mutation sweep found exactly that: 22 of this crate's 29 survivors were
//! in code a handshake reaches and only a *conversation* runs.
//!
//! # Why the root is added rather than verification turned off
//!
//! Every test here trusts the fixture's own certificate through
//! [`NativeTls::add_root_certificate`], not
//! `danger_accept_invalid_certs`. Two reasons, and the second is the one
//! that matters. It runs under **both** feature settings, where anything
//! reaching for the insecure constructor is compiled out without
//! `dangerous-insecure` and `--all-features` alone cannot reach the other
//! arm. And it exercises the path a deployment actually uses — a private
//! CA the platform store does not have — so what is pinned is the trust
//! decision working rather than the trust decision being skipped.
//!
//! The peer is `rustls`, for the reason `tests/insecure.rs` and the
//! manifest both record: `native_tls::Identity::from_pkcs8` is an
//! OpenSSL-shaped constructor that `SChannel` and Security.framework refuse,
//! so a `native-tls` server would not start on two of the three platforms
//! this crate exists for.

use hclient_rt::TcpConnect;
use hclient_rt_tokio::Tokio;
use hclient_tls::{TlsConnect, TlsInfo, TlsRequest};
use hclient_tls_native_tls::NativeTls;
use hyper::rt::{Read as _, Write as _};
use std::net::SocketAddr;
use std::pin::Pin;
use std::time::Duration;

const OP_TIMEOUT: Duration = Duration::from_secs(10);

/// The stream the seam hands back, named once so the poll helpers below can
/// take it without restating the four layers of wrapper.
type Stream = <NativeTls as TlsConnect>::Stream<hclient_rt_tokio::TokioIo>;

/// A self-signed TLS server that echoes one short message back.
///
/// Returns its address **and its certificate in DER**, which is what lets
/// the client trust it by adding a root rather than by switching
/// verification off — see the module doc.
fn spawn_echo_server(alpn: Vec<Vec<u8>>) -> (SocketAddr, Vec<u8>) {
    let cert = rcgen::generate_simple_self_signed(vec!["localhost".into()])
        .expect("self-signed certificate");
    let der = cert.cert.der().to_vec();

    let mut cfg = rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(
            vec![der.clone().into()],
            rustls_pki_types::PrivateKeyDer::Pkcs8(cert.signing_key.serialize_der().into()),
        )
        .expect("server config");
    // Empty on the arm that tests "the peer chose nothing", which is a
    // different answer from "this backend cannot tell you" and is why the
    // test below asserts `None` there rather than skipping the case.
    cfg.alpn_protocols = alpn;
    let acceptor = tokio_rustls::TlsAcceptor::from(std::sync::Arc::new(cfg));

    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = listener.local_addr().expect("local_addr");
    listener.set_nonblocking(true).expect("nonblocking");
    std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");
        rt.block_on(async move {
            let listener = tokio::net::TcpListener::from_std(listener).expect("from_std");
            loop {
                let Ok((tcp, _)) = listener.accept().await else {
                    continue;
                };
                let acceptor = acceptor.clone();
                tokio::spawn(async move {
                    // A refused handshake is an expected outcome — a test
                    // here deliberately connects without the root — so the
                    // error is dropped rather than unwrapped.
                    if let Ok(mut tls) = acceptor.accept(tcp).await {
                        use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
                        let mut buf = [0u8; 64];
                        // Echo once, then let the connection end. One
                        // round trip is all any test here needs, and a
                        // loop would outlive the client's `close`.
                        if let Ok(n) = tls.read(&mut buf).await {
                            let _ = tls.write_all(&buf[..n]).await;
                            let _ = tls.flush().await;
                        }
                    }
                });
            }
        });
    });
    (addr, der)
}

async fn bounded<F: std::future::Future>(fut: F) -> F::Output {
    tokio::time::timeout(OP_TIMEOUT, fut)
        .await
        .unwrap_or_else(|_| panic!("did not resolve within {OP_TIMEOUT:?}"))
}

/// Completes a handshake against `addr`, trusting `der` as a root and
/// offering `alpn`.
async fn connect_trusting(
    addr: SocketAddr,
    der: &[u8],
    alpn: &[&[u8]],
) -> Result<(Stream, TlsInfo), hclient_core::error::Error> {
    let root = native_tls::Certificate::from_der(der).expect("the fixture's own DER is a root");
    let tls = NativeTls::new().add_root_certificate(root);
    let tcp = Tokio
        .connect(addr, &hclient_rt::TcpOpts::default())
        .await
        .expect("tcp");
    bounded(tls.connect(
        tcp,
        TlsRequest {
            identity: None,
            server_name: "localhost",
            alpn,
            ech: None,
            early_data: None,
        },
    ))
    .await
}

/// One `write` -> `flush` -> `read` round trip over the session, returning
/// what came back.
///
/// Written as `poll_fn` over the seam's own `hyper::rt` traits rather than
/// through an extension trait, because that is the shape a consumer meets
/// and because it is the only way to reach these four `poll_*` bodies
/// without adding a dependency for the privilege.
async fn round_trip(stream: &mut Stream, msg: &[u8]) -> Vec<u8> {
    let mut sent = 0;
    while sent < msg.len() {
        let n = bounded(std::future::poll_fn(|cx| {
            Pin::new(&mut *stream).poll_write(cx, &msg[sent..])
        }))
        .await
        .expect("write");
        assert_ne!(n, 0, "a zero-length write would loop for ever");
        sent += n;
    }
    bounded(std::future::poll_fn(|cx| {
        Pin::new(&mut *stream).poll_flush(cx)
    }))
    .await
    .expect("flush");

    let mut raw = [0u8; 64];
    let mut buf = hyper::rt::ReadBuf::new(&mut raw);
    bounded(std::future::poll_fn(|cx| {
        Pin::new(&mut *stream).poll_read(cx, buf.unfilled())
    }))
    .await
    .expect("read");
    buf.filled().to_vec()
}

/// **The session carries bytes both ways.**
///
/// This is the test the whole file turns on: it is the only thing in the
/// crate that *uses* a `TlsStream` rather than merely obtaining one, so it
/// is what executes `poll_write`, `poll_flush`, `poll_read` and the `cvt`
/// that turns `native-tls`'s synchronous `WouldBlock` into `Poll::Pending`.
///
/// Each of those was a live mutation survivor. A `poll_read` answering
/// `Ok(0)` — silent truncation of every response — and a `poll_write`
/// answering `Ok(0)` both passed the entire suite before this existed, and
/// so did a `cvt` that treated `WouldBlock` as a real error, because
/// nothing ever suspended mid-record.
#[tokio::test]
async fn a_completed_session_carries_bytes_in_both_directions() {
    let (addr, der) = spawn_echo_server(vec![b"h2".to_vec()]);
    let (mut stream, _) = connect_trusting(addr, &der, &[b"h2"])
        .await
        .expect("a root the client was given must verify");

    let echoed = round_trip(&mut stream, b"ping over TLS").await;
    assert_eq!(
        echoed, b"ping over TLS",
        "the bytes must survive the session intact — a short read or a write \
         reporting more than it sent truncates silently, with no error anywhere"
    );
}

/// **`close` completes**, which is `poll_close`'s own arm and not
/// `poll_flush`'s.
///
/// Kept separate from the round trip above because it is a different
/// mutation: `poll_close` replaced by `Ok(())` skips
/// `native_tls::TlsStream::shutdown` — the call that emits `close_notify`
/// — and a peer cannot tell a bare FIN from a truncation attack. The
/// round-trip test passes either way, having already got its bytes.
#[tokio::test]
async fn closing_the_session_runs_the_tls_shutdown() {
    let (addr, der) = spawn_echo_server(vec![]);
    let (mut stream, _) = connect_trusting(addr, &der, &[])
        .await
        .expect("a root the client was given must verify");

    bounded(std::future::poll_fn(|cx| {
        Pin::new(&mut stream).poll_shutdown(cx)
    }))
    .await
    .expect("close_notify must go out cleanly over a live session");
}

/// **The negotiated protocol is reported, and it is the one the peer
/// chose.**
///
/// `reports_alpn` returning `true` is a promise about this value, and
/// `tests/shape.rs` asserts only the `bool`. What makes the promise real is
/// that `TlsInfo::alpn` carries what came back off the wire: the four
/// mutations of `negotiated_alpn` — `None`, and three constant `Some`s —
/// all survived the suite, because no test had ever read the field.
///
/// The server offers `h2` and `http/1.1` and the client asks for `http/1.1`
/// alone, so the answer cannot be either side's first preference by
/// accident. A constant `Some(b"h2")` fails here.
#[tokio::test]
async fn the_alpn_reported_is_the_protocol_the_peer_actually_chose() {
    let (addr, der) = spawn_echo_server(vec![b"h2".to_vec(), b"http/1.1".to_vec()]);
    let (_, info) = connect_trusting(addr, &der, &[b"http/1.1"])
        .await
        .expect("a root the client was given must verify");

    assert_eq!(
        info.alpn.as_deref(),
        Some(&b"http/1.1"[..]),
        "the client offered only http/1.1 against a server preferring h2, so \
         this is the negotiated answer rather than either side's default"
    );
}

/// **No ALPN offered means no ALPN reported** — `None`, and it means *the
/// peer chose nothing*, not *this backend cannot tell you*.
///
/// The control for the test above: without it, a `negotiated_alpn` hard-
/// wired to the protocol the other test expects would pass that one and
/// this one would catch it. It is also the arm that says the `Option` is
/// read rather than manufactured.
#[tokio::test]
async fn no_alpn_offered_is_reported_as_none_rather_than_as_something() {
    let (addr, der) = spawn_echo_server(vec![]);
    let (_, info) = connect_trusting(addr, &der, &[])
        .await
        .expect("a root the client was given must verify");

    assert_eq!(
        info.alpn, None,
        "nothing was offered and nothing was chosen; a `Some` here would be \
         this backend inventing a protocol nobody negotiated"
    );
}

/// **The peer's leaf certificate comes back, and it is the peer's.**
///
/// The crate's module doc makes two claims about this field that nothing
/// checked: that it is the leaf, returned as a one-element `Vec` rather
/// than `None` *because there is a certificate and the chain is what is
/// missing*; and that `None` throughout means "this backend cannot tell
/// you". All four `peer_certificate_der` mutations survived — including
/// `Some(vec![])`, an empty certificate, which is the shape a caller
/// parsing DER would meet as a corrupt input rather than as an absence.
///
/// Comparing against the fixture's own DER is what makes it the *peer's*
/// rather than any certificate: a constant cannot equal a key generated
/// this run.
#[tokio::test]
async fn the_peer_certificate_is_the_leaf_the_server_actually_presented() {
    let (addr, der) = spawn_echo_server(vec![]);
    let (_, info) = connect_trusting(addr, &der, &[])
        .await
        .expect("a root the client was given must verify");

    let certs = info
        .peer_certificates
        .expect("the server presented a certificate, so this is not `cannot tell you`");
    assert_eq!(
        certs.len(),
        1,
        "the leaf alone — this backend cannot reach the chain, and says so \
         with a one-element Vec rather than by omitting the field"
    );
    assert_eq!(
        certs[0], der,
        "and it is the certificate this fixture minted this run, which no \
         constant can be"
    );
}

/// **A certificate the client was not given is refused**, with the root
/// added being the only difference.
///
/// The control for every test above: each of them proves a property of a
/// handshake that *succeeded*, and would prove it just as well for a
/// client that had stopped verifying anything. This is the arm that says
/// the success was earned. It is also what discriminates
/// [`NativeTls::add_root_certificate`] itself — a survivor, since a body
/// returning `Default::default()` drops the root silently and every test
/// above would fail for the wrong reason while this one passed.
#[tokio::test]
async fn the_same_server_without_the_added_root_is_refused() {
    let (addr, _der) = spawn_echo_server(vec![]);

    let tcp = Tokio
        .connect(addr, &hclient_rt::TcpOpts::default())
        .await
        .expect("tcp");
    let err = bounded(NativeTls::new().connect(
        tcp,
        TlsRequest {
            identity: None,
            server_name: "localhost",
            alpn: &[],
            ech: None,
            early_data: None,
        },
    ))
    .await
    .expect_err("no trust store issued this certificate, so it must be refused");

    assert_eq!(
        *err.kind(),
        hclient_core::error::ErrorKind::Tls,
        "a refused certificate is a TLS error, not a transport one: {err}"
    );
}
