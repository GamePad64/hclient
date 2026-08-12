//! What a pooled connection costs against a server that closes after
//! every response — the race `h1.rs` calls residual, measured.
//!
//! **A measurement, not a verdict**, and `#[ignore]`d for the reason
//! `tests/nagle_cost.rs` is: the output is a *rate*, and a rate asserted
//! against a threshold is a flake waiting to be filed. What it is here to
//! answer is one question that the `nodelay` change raised and could not
//! answer from outside:
//!
//! `Native::new` pools. A server that answers and then closes — no
//! `Connection: close`, which is a server's right and what several
//! fixtures in this workspace do — leaves the pool holding a connection
//! that is about to die. `pool.rs` polls it at checkout and `h1.rs` polls
//! it once more before handing the request over, and between them they
//! close the race whenever the peer's `FIN` has already arrived. When it
//! has not, `h1.rs` says so in its own words: *"the residual race the
//! checkout poll cannot close"*.
//!
//! **Turning `nodelay` on made that residual race routine**, at least on
//! loopback: `http-ng-select`'s `alt_svc` suite went from 0 failing runs
//! in 20 to 9, and every failure was the same
//! `Connect / hyper::Error(Shutdown, BrokenPipe)`. Nothing about the
//! error path changed; what changed is that the client is 40 ms faster at
//! coming back for the second request, and 40 ms is how long the peer's
//! close took to become visible.
//!
//! # What this file failed to reproduce, which is the finding
//!
//! **Both arms below fail 0 of 40 requests**, with `nodelay` and without.
//! Forty consecutive requests through one pooled transport against a
//! server that closes after every one of them, and the pool's checkout
//! poll plus `h1.rs`'s pre-poll catch the close every single time. So the
//! race is **not** "a pooled connection against a server that closes";
//! that much this crate handles.
//!
//! What the `alt_svc` failures have and this does not is **contention**:
//! measured at `-j16` on a 28-core machine under load, 9 failing runs in
//! 20; at `-j1`, 0 in 20, on the identical binary. A starved client is one
//! whose reactor has not yet delivered the peer's `FIN` when `h1.rs` takes
//! its one non-suspending look, and `h1.rs` names that window itself. The
//! 41 ms was covering it.
//!
//! It is recorded here rather than fixed, and `docs/nagle-and-nodelay.md`
//! §6 says why, including the fix that was tried and reverted: past that
//! window the request is inside `try_send_request`, hyper reports
//! `message: None` for it, and `Failed::Sent` is not retryable.
//!
//! ```text
//! cargo nextest run -p http-ng-native --test stale_reuse --run-ignored all \
//!     --no-capture -j1
//! ```
#![cfg(not(target_family = "wasm"))]

use http_body_util::BodyExt;
use http_ng_core::RequestBody;
use http_ng_core::unversioned::Transport;
use http_ng_dns::IpLiteralOnly;
use http_ng_native::Native;
use http_ng_rt::TcpOpts;
use http_ng_rt_tokio::Tokio;
use http_ng_tls_rustls::Rustls;
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use std::net::SocketAddr;
use std::sync::Arc;

/// Requests per arm. Every one after the first meets a pooled connection
/// the server has already closed, so this is the number of times the race
/// is run rather than a sample size for a timing.
const REQUESTS: usize = 40;

fn identity() -> (CertificateDer<'static>, PrivateKeyDer<'static>) {
    let cert = rcgen::generate_simple_self_signed(vec!["127.0.0.1".into()])
        .expect("rcgen can always make a self-signed cert");
    (
        CertificateDer::from(cert.cert.der().to_vec()),
        PrivateKeyDer::try_from(cert.signing_key.serialize_der()).expect("pkcs8 from rcgen"),
    )
}

/// A TLS server that answers exactly one request per connection and then
/// closes it — `close_notify` first, so the client's failure is about the
/// close rather than about a truncated stream.
///
/// The shape is `http-ng-select`'s `tests/servers.rs` and
/// `pool.rs`'s `responses_before_close: Some(1)`, deliberately: this file
/// exists to explain a failure those two produce.
fn spawn() -> (SocketAddr, CertificateDer<'static>) {
    let (cert_der, key_der) = identity();
    let mut cfg = rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(vec![cert_der.clone()], key_der)
        .expect("the cert and key were made together");
    cfg.alpn_protocols = vec![b"http/1.1".to_vec()];
    let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(cfg));

    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("v4 loopback");
    let addr = listener.local_addr().expect("local_addr");
    listener.set_nonblocking(true).expect("nonblocking");

    std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("a current-thread runtime");
        rt.block_on(async move {
            let listener = tokio::net::TcpListener::from_std(listener).expect("from_std");
            loop {
                let Ok((tcp, _)) = listener.accept().await else {
                    continue;
                };
                let acceptor = acceptor.clone();
                tokio::spawn(async move {
                    use tokio::io::{AsyncReadExt, AsyncWriteExt};
                    let Ok(mut tls) = acceptor.accept(tcp).await else {
                        return;
                    };
                    let mut head = Vec::new();
                    let mut byte = [0u8; 1];
                    while !head.ends_with(b"\r\n\r\n") {
                        match tls.read(&mut byte).await {
                            Ok(1) => head.push(byte[0]),
                            _ => return,
                        }
                    }
                    let _ = tls
                        .write_all(b"HTTP/1.1 200 OK\r\ncontent-length: 2\r\n\r\nok")
                        .await;
                    let _ = tls.flush().await;
                    let _ = tls.shutdown().await;
                });
            }
        });
    });

    (addr, cert_der)
}

fn client_tls(cert: &CertificateDer<'static>) -> Rustls {
    let mut roots = rustls::RootCertStore::empty();
    roots.add(cert.clone()).expect("a DER certificate");
    let cfg = rustls::ClientConfig::builder()
        .with_root_certificates(roots)
        .with_no_client_auth();
    Rustls::from_config(Arc::new(cfg))
}

/// `REQUESTS` sequential requests through one pooled transport, and the
/// errors they collected.
async fn run(opts: Option<TcpOpts>) -> Vec<String> {
    let (addr, cert) = spawn();
    let t = Native::new(Tokio, client_tls(&cert), IpLiteralOnly);
    let t = match opts {
        Some(o) => t.tcp_opts(o).expect("Tokio applies every option"),
        None => t,
    };
    let uri = format!("https://{addr}/");
    let mut errors = Vec::new();
    for _ in 0..REQUESTS {
        let req = http::Request::builder()
            .uri(&uri)
            .body(RequestBody::Empty)
            .expect("a well-formed request");
        match t.execute(req).await {
            Ok(resp) => {
                // Drain it, or the connection never returns to the pool
                // and the next request meets nothing to race against.
                let _ = resp.into_body().collect().await;
            }
            Err(e) => errors.push(format!("{e:?}")),
        }
    }
    errors
}

fn report(arm: &str, errors: &[String]) {
    println!(
        "\n== {arm} ==\n  {} of {REQUESTS} requests failed",
        errors.len()
    );
    for e in errors.iter().take(3) {
        println!("    {e}");
    }
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "a measurement, not a verdict — see the module doc"]
async fn with_the_transports_own_nodelay() {
    report("Native::new and nothing else", &run(None).await);
}

/// The control, and the arm that says the difference is the 40 ms rather
/// than anything about the pool: `TcpOpts::default()` replaces the whole
/// set, so this is the transport as it was before the `nodelay` change.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "a measurement, not a verdict — see the module doc"]
async fn with_nagle_left_on() {
    report(
        "tcp_opts(TcpOpts::default()) — Nagle on, as before",
        &run(Some(TcpOpts::default())).await,
    );
}
