//! `danger_accept_invalid_certs` against a server the platform stack has
//! no way to verify.
//!
//! The setting is two forwarded calls into `native-tls`, which is exactly
//! the shape of defect this workspace has closed four times: a setting
//! stored on a builder and never read at the site that applies it. So the
//! test drives a real handshake rather than reading the field back, and it
//! carries the control that makes the result mean something — the same
//! client without the setting must **refuse** the same server.

#![cfg(feature = "dangerous-insecure")]

use hclient_rt::TcpConnect;
use hclient_rt_tokio::Tokio;
use hclient_tls::{TlsConnect, TlsIdentity, TlsRequest};
use hclient_tls_native_tls::NativeTls;
use std::net::SocketAddr;
use std::time::Duration;

const OP_TIMEOUT: Duration = Duration::from_secs(10);

/// A self-signed TLS server on `native-tls` itself. The client side is
/// what this crate ships, so building the server on the other backend
/// would put rustls in the loop and test the pair.
fn spawn_tls_server() -> SocketAddr {
    let cert = rcgen::generate_simple_self_signed(vec!["localhost".into()]).unwrap();
    let identity = native_tls::Identity::from_pkcs8(
        cert.cert.pem().as_bytes(),
        cert.signing_key.serialize_pem().as_bytes(),
    )
    .unwrap();
    let acceptor = native_tls::TlsAcceptor::new(identity).unwrap();

    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    std::thread::spawn(move || {
        // Blocking, in its own thread: the handshake is all this fixture
        // owes, and a client that completed one has proved the point.
        for tcp in listener.incoming().flatten() {
            let acceptor = acceptor.clone();
            std::thread::spawn(move || {
                // A refused handshake is an expected outcome here — it is
                // what the control arm produces — so the error is dropped
                // rather than unwrapped.
                if let Ok(mut tls) = acceptor.accept(tcp) {
                    use std::io::Read as _;
                    let mut sink = [0u8; 64];
                    let _ = tls.read(&mut sink);
                }
            });
        }
    });
    addr
}

async fn bounded<F: std::future::Future>(fut: F) -> F::Output {
    tokio::time::timeout(OP_TIMEOUT, fut)
        .await
        .unwrap_or_else(|_| panic!("did not resolve within {OP_TIMEOUT:?}"))
}

async fn handshake(tls: &NativeTls, addr: SocketAddr) -> Result<(), hclient_core::Error> {
    let tcp = Tokio
        .connect(addr, &hclient_rt::TcpOpts::default())
        .await
        .unwrap();
    bounded(tls.connect(
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
    .map(|_| ())
}

#[tokio::test]
async fn an_unverifiable_server_is_refused_by_default_and_accepted_by_the_insecure_client() {
    let addr = spawn_tls_server();

    // The control. Nothing in this machine's trust store issued that
    // certificate, so the platform stack must refuse it — and without
    // this arm, the assertion below would also pass for a build whose
    // ordinary client had stopped verifying.
    assert!(
        handshake(&NativeTls::new(), addr).await.is_err(),
        "the platform stack must refuse a certificate no trust store issued"
    );

    handshake(&NativeTls::new().danger_accept_invalid_certs(), addr)
        .await
        .expect("danger_accept_invalid_certs must complete a handshake the default one refuses");
}

#[tokio::test]
async fn the_wrong_name_is_accepted_too() {
    // A development certificate is routinely for a name the caller is not
    // using, so this is the outcome the setting promises and it is pinned
    // rather than described.
    //
    // What it is **not** is a discriminator for the second setter line,
    // and that was measured rather than assumed: dropping
    // `danger_accept_invalid_hostnames` leaves this test passing. The
    // reason is a fact about one platform — `native-tls`'s OpenSSL backend
    // implements the certificate flag as `set_verify(SslVerifyMode::NONE)`
    // (`imp/openssl.rs:348`), which switches off the whole verification
    // including the name, so on this host the first flag subsumes the
    // second. On SChannel the two are independent — `accept_invalid_certs`
    // installs a `verify_callback` while `accept_invalid_hostnames` is a
    // separate builder call (`imp/schannel.rs:295-296`) — and
    // Security.framework forwards both separately too
    // (`imp/security_framework.rs:389-390`). That is why both are set, and
    // why no test on a Linux host can show it.
    let addr = spawn_tls_server(); // certificate names `localhost`
    let tls = NativeTls::new().danger_accept_invalid_certs();
    let tcp = Tokio
        .connect(addr, &hclient_rt::TcpOpts::default())
        .await
        .unwrap();
    bounded(tls.connect(
        tcp,
        TlsRequest {
            identity: None,
            server_name: "not-the-name-on-the-certificate.invalid",
            alpn: &[],
            ech: None,
            early_data: None,
        },
    ))
    .await
    .expect("a name mismatch must be accepted too, or the setting is half applied");
}

#[test]
fn an_insecure_config_cannot_share_a_pooled_connection_with_a_verifying_one() {
    // `TlsConfigId` is part of `hclient-native`'s pool key. The dangerous
    // direction is a connection established without verification being
    // handed to a client that asked for it.
    let verifying = NativeTls::new();
    let insecure = NativeTls::new().danger_accept_invalid_certs();
    assert_ne!(verifying.config_id(), insecure.config_id());
}
