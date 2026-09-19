//! Closing the session sends `close_notify`, and the peer is what says so.
//!
//! # The distinction, and why only the peer can see it
//!
//! `src/hyper_io.rs` already carries a test built entirely on this
//! difference — *"a peer cannot tell a bare FIN from a truncation
//! attack"* — but it checks it one layer below TLS, where all it can ask is
//! whether `close` reached `poll_shutdown` rather than `poll_flush`. The
//! layer where the alert is actually produced is
//! `TlsStream::poll_close`, which calls `native_tls::TlsStream::shutdown`,
//! and **that** was a live mutation survivor: replaced by `Poll::from(Ok
//! (()))` it skips the alert, returns success, and every other test in this
//! crate passes, because a client that has already read its bytes cannot
//! tell whether it said goodbye.
//!
//! So the assertion is made from the other end. A TLS peer reading to the
//! end of a session distinguishes the two outcomes itself: a `close_notify`
//! surfaces as a clean end of stream, a bare FIN as
//! `UnexpectedEof` — which is precisely the signal that exists so a
//! truncated response cannot be passed off as a complete one. The fixture
//! reports which it saw and the test asserts the first.
//!
//! Measured while writing: with `poll_close` intact the peer reports a
//! clean end; with the mutation applied it does not, and this file fails
//! while the rest of the suite stays green.

use hclient_rt::Shutdown as _;
use hclient_rt::TcpConnect;
use hclient_rt_tokio::Tokio;
use hclient_tls::{TlsConnect, TlsRequest};
use hclient_tls_native_tls::NativeTls;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::time::Duration;

const OP_TIMEOUT: Duration = Duration::from_secs(10);

/// What the server made of the end of the session.
///
/// A `String` rather than an enum so that an unexpected outcome names
/// itself in the failure message instead of arriving as a wildcard — the
/// interesting failures here are the ones nobody predicted.
type Outcome = Arc<Mutex<Option<String>>>;

/// A TLS server that accepts one connection, reads to the end of the
/// session, and records how it ended.
fn spawn_tls_server(outcome: Outcome) -> (std::net::SocketAddr, Vec<u8>) {
    let cert = rcgen::generate_simple_self_signed(vec!["localhost".into()])
        .expect("self-signed certificate");
    let der = cert.cert.der().to_vec();
    let cfg = rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(
            vec![der.clone().into()],
            rustls_pki_types::PrivateKeyDer::Pkcs8(cert.signing_key.serialize_der().into()),
        )
        .expect("server config");
    let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(cfg));

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
            if let Ok((tcp, _)) = listener.accept().await
                && let Ok(mut tls) = acceptor.accept(tcp).await
            {
                use tokio::io::AsyncReadExt as _;
                let mut sink = [0u8; 64];
                // The whole of the measurement. `rustls` maps a
                // `close_notify` to an ordinary end of stream and a
                // FIN without one to `UnexpectedEof`, which is the
                // distinction TLS defines the alert for.
                let seen = match tls.read(&mut sink).await {
                    Ok(0) => "clean end of stream".to_owned(),
                    Ok(n) => format!("{n} bytes of data, which this test sends none of"),
                    Err(e) => format!("error: {:?}", e.kind()),
                };
                *outcome.lock().expect("outcome") = Some(seen);
            }
        });
    });
    (addr, der)
}

/// **A closed session ends with `close_notify`, and the peer sees a clean
/// end of stream rather than a truncation.**
///
/// The client writes nothing: the only thing crossing the wire after the
/// handshake is the alert, so the peer's answer is about that and nothing
/// else. With `TlsStream::poll_close` replaced by `Ok(())` the peer reports
/// an `UnexpectedEof` instead and this line fails.
#[tokio::test]
async fn closing_the_session_sends_close_notify_and_the_peer_sees_a_clean_end() {
    let outcome: Outcome = Arc::new(Mutex::new(None));
    let (addr, der) = spawn_tls_server(Arc::clone(&outcome));
    let root = native_tls::Certificate::from_der(&der).expect("the fixture's own DER is a root");

    let tcp = Tokio
        .connect(addr, &hclient_rt::TcpOpts::default())
        .await
        .expect("tcp");
    let (mut stream, _) = tokio::time::timeout(
        OP_TIMEOUT,
        NativeTls::new().add_root_certificate(root).connect(
            tcp,
            TlsRequest {
                identity: None,
                server_name: "localhost",
                alpn: &[],
                ech: None,
                early_data: None,
            },
        ),
    )
    .await
    .expect("handshake within the bound")
    .expect("a root the client was given must verify");

    tokio::time::timeout(
        OP_TIMEOUT,
        std::future::poll_fn(|cx| Pin::new(&mut stream).poll_shutdown(cx)),
    )
    .await
    .expect("close within the bound")
    .expect("close");
    // The TCP FIN follows the alert, and the peer needs both to answer.
    drop(stream);

    // Polling rather than one sleep: the fixture is on its own runtime in
    // its own thread, so the answer arrives when it arrives, and a fixed
    // wait is either flaky or slow.
    let deadline = std::time::Instant::now() + OP_TIMEOUT;
    let seen = loop {
        if let Some(seen) = outcome.lock().expect("outcome").clone() {
            break seen;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the server never reached the end of the session within {OP_TIMEOUT:?}"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    };

    assert_eq!(
        seen, "clean end of stream",
        "the peer must see a close_notify. Anything else means the alert was \
         not sent, and a peer cannot tell a bare FIN from a truncation attack \
         — which is the entire reason TLS has the alert"
    );
}
