//! Two of the three failure modes brief item C asked the original task to
//! cover (fix round 1 review, Verdict B #5): only
//! `rejects_an_untrusted_certificate` (`tests/handshake.rs`) shipped in
//! round 1. Both below were independently re-verified in this tree - PASS
//! against the fixed code, same as before round 1 (this is a coverage gap,
//! not a behaviour defect the review found) - adapted from the reviewer's
//! saved `review-task-9-error-path-coverage.rs`.
use http_ng_rt::TcpConnect;
use http_ng_rt_tokio::Tokio;
use http_ng_tls::{TlsConnect, TlsRequest};
use http_ng_tls_rustls::Rustls;
use std::sync::Arc;
use std::time::Duration;

mod server; // crates/http-ng-tls-rustls/tests/server.rs -- spawn_tls_echo()

#[tokio::test]
async fn name_mismatch_is_reported_as_tls_with_a_distinguishing_source() {
    let (addr, ca_der) = server::spawn_tls_echo(); // cert is issued for "localhost"
    let mut roots = rustls::RootCertStore::empty();
    roots.add(ca_der.into()).unwrap();
    let cfg = rustls::ClientConfig::builder()
        .with_root_certificates(roots)
        .with_no_client_auth();
    let tls = Rustls::from_config(Arc::new(cfg));
    let tcp = Tokio
        .connect(addr, &http_ng_rt::TcpOpts::default())
        .await
        .unwrap();

    let result = tokio::time::timeout(
        Duration::from_secs(5),
        tls.connect(
            tcp,
            TlsRequest {
                server_name: "not-localhost.invalid", // trusted CA, wrong hostname
                alpn: &[],
                ech: None,
            },
        ),
    )
    .await
    .expect("must not hang");

    let err = result.expect_err("hostname mismatch must fail");
    assert!(matches!(err.kind(), http_ng_core::ErrorKind::Tls), "{err}");
    // `kind()` alone doesn't distinguish this from other TLS failures - a
    // single flat `Tls` category is Task 8's established design, not
    // something this crate introduced or could change - but the wrapped
    // source's `Display` does, which is what a caller needing to tell
    // "wrong host" from "untrusted cert" apart would inspect.
    let msg = err.to_string();
    assert!(
        msg.contains("not-localhost.invalid") || msg.contains("not valid for name"),
        "expected the source error to name the mismatch, got: {msg}"
    );
}
/// Walk the source chain for an `io::Error`.
///
/// `http_ng_core::Error` is deliberately erased (`Arc<dyn Error>`), so the
/// io kind is reachable only from the chain — and it is the only
/// platform-neutral way to say WHICH way the peer went away. Matching on
/// `Display` text instead pins English strings that differ per OS:
/// "Connection reset by peer" on Linux and macOS, "An established
/// connection was aborted by the software in your host machine." on
/// Windows. The previous version of this test matched on text, and that is
/// half of why it flaked.
fn io_kind(err: &(dyn std::error::Error + 'static)) -> Option<std::io::ErrorKind> {
    let mut cur = Some(err);
    while let Some(e) = cur {
        if let Some(io) = e.downcast_ref::<std::io::Error>() {
            return Some(io.kind());
        }
        cur = e.source();
    }
    None
}

/// A TCP listener on loopback, plus the client-side `TlsConnect` attempt
/// against it. `serve` runs on its OWN thread and its own runtime,
/// deliberately: this test's runtime is single-threaded, so a
/// `tokio::spawn`ed accept would only run when the client task yields, and
/// the close would land at a different point in the handshake than the one
/// under test. Measured, not assumed — switching to `tokio::spawn` made the
/// EOF assertion fail every time.
///
/// The `ready` channel closes the one race that shape has: building a
/// runtime takes time, and the client below waits only 5 seconds. Under
/// load — several test binaries and a build sharing the machine — that race
/// was lost rarely, and the test then failed on its own timeout and
/// reported a hang, which is exactly the failure it is named for and
/// exactly what had not happened.
async fn handshake_against<F>(serve: F) -> http_ng_core::Error
where
    F: FnOnce(
            tokio::net::TcpStream,
        ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>>
        + Send
        + 'static,
{
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    listener.set_nonblocking(true).unwrap();

    let (ready_tx, ready_rx) = std::sync::mpsc::channel::<()>();
    std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async move {
            let listener = tokio::net::TcpListener::from_std(listener).unwrap();
            // Ready means "the reactor is up and this listener is
            // registered with it", not "the thread started".
            ready_tx.send(()).expect("the test is still waiting");
            let (tcp, _) = listener.accept().await.unwrap();
            serve(tcp).await;
        });
    });
    ready_rx
        .recv_timeout(Duration::from_secs(30))
        .expect("the server thread must reach its accept loop");

    let roots = rustls::RootCertStore::empty();
    let cfg = rustls::ClientConfig::builder()
        .with_root_certificates(roots)
        .with_no_client_auth();
    let tls = Rustls::from_config(Arc::new(cfg));
    let tcp = Tokio
        .connect(addr, &http_ng_rt::TcpOpts::default())
        .await
        .unwrap();

    let result = tokio::time::timeout(
        Duration::from_secs(5),
        tls.connect(
            tcp,
            TlsRequest {
                server_name: "localhost",
                alpn: &[],
                ech: None,
            },
        ),
    )
    .await
    .expect("must not hang");

    result
        .map(|_| ())
        .expect_err("a peer that goes away mid-handshake must fail, not succeed")
}

/// The graceful half: the peer sends FIN in the middle of the handshake.
///
/// The server DRAINS the ClientHello before closing, and that is the whole
/// point of this version. Closing a socket that still has unread data
/// queued makes the OS send RST instead of FIN — always on macOS and
/// Windows, and on Linux depending on whether the ClientHello had landed
/// yet. The old single test did not drain, so which of the two error
/// shapes it got was a race: it failed on `macos-latest` with `Connection
/// reset by peer (os error 54)` and on `windows-latest` with `os error
/// 10053`, while passing on Linux almost always. Both outcomes were
/// correct behaviour; the test simply demanded one of them.
#[tokio::test]
async fn peer_sending_fin_mid_handshake_is_reported_as_tls_not_a_hang() {
    let err = handshake_against(|mut tcp| {
        Box::pin(async move {
            use tokio::io::{AsyncReadExt, AsyncWriteExt};
            let mut buf = [0u8; 4096];
            // Drain the ClientHello, so the close below is a clean FIN.
            let _ = tcp.read(&mut buf).await;
            tcp.shutdown().await.unwrap();
            // Stay alive until the client goes away: dropping the socket
            // while something new arrived would turn the FIN into a reset
            // after the fact.
            let _ = tcp.read(&mut buf).await;
        })
    })
    .await;

    assert!(matches!(err.kind(), http_ng_core::ErrorKind::Tls), "{err}");
    // The `!more` branch in `Rustls::connect`'s handshake `poll_fn` is what
    // is expected to fire here — it constructs exactly this io kind.
    assert_eq!(
        io_kind(&err),
        Some(std::io::ErrorKind::UnexpectedEof),
        "expected an EOF-shaped source error, got: {err}"
    );
}

/// The abortive half: the peer resets the connection mid-handshake.
///
/// `SO_LINGER(0)` + close sends RST regardless of what is or isn't queued,
/// so unlike the FIN case this one does not depend on timing at all — on
/// any platform.
#[tokio::test]
async fn peer_resetting_mid_handshake_is_reported_as_tls_not_a_hang() {
    let err = handshake_against(|tcp| {
        Box::pin(async move {
            let std = tcp.into_std().unwrap();
            socket2::Socket::from(std)
                .set_linger(Some(Duration::ZERO))
                .unwrap();
            // dropped here: SO_LINGER(0) makes close send RST, not FIN.
        })
    })
    .await;

    assert!(matches!(err.kind(), http_ng_core::ErrorKind::Tls), "{err}");
    // Both kinds mean "the peer tore the connection down": Linux and macOS
    // report the received RST as `ConnectionReset`, Windows can surface the
    // same exchange as `ConnectionAborted` (WSAECONNABORTED, 10053). What
    // must NOT happen is `UnexpectedEof` — that would mean an abortive
    // close was reported as a graceful one, which is the confusion the
    // sibling test above exists to keep separate.
    let kind = io_kind(&err);
    assert!(
        matches!(
            kind,
            Some(std::io::ErrorKind::ConnectionReset | std::io::ErrorKind::ConnectionAborted)
        ),
        "expected a reset-shaped source error, got {kind:?} from: {err}"
    );
}
