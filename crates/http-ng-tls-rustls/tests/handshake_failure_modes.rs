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

#[tokio::test]
async fn peer_closing_mid_handshake_is_reported_as_tls_not_a_hang() {
    // TCP listener that accepts, then immediately closes without sending a
    // single TLS byte.
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    listener.set_nonblocking(true).unwrap();
    std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async move {
            let listener = tokio::net::TcpListener::from_std(listener).unwrap();
            let (tcp, _) = listener.accept().await.unwrap();
            drop(tcp);
        });
    });

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

    let err = result.expect_err("mid-handshake close must fail, not succeed");
    assert!(matches!(err.kind(), http_ng_core::ErrorKind::Tls), "{err}");
    // The `!more` branch in `Rustls::connect`'s handshake `poll_fn`
    // (`ErrorKind::Tls` + `io::ErrorKind::UnexpectedEof`) is what's
    // expected to fire here.
    assert!(
        err.to_string().to_lowercase().contains("eof")
            || err.to_string().to_lowercase().contains("end of file"),
        "expected an EOF-shaped source error, got: {err}"
    );
}
