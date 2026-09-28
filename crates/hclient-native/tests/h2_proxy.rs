//! An HTTP/2 proxy that answers CONNECT and extended CONNECT, over real TLS.
//!
//! An `h2::server` behind `tokio-rustls` with an `rcgen` certificate naming
//! `localhost`, so the client's handshake to it is the one a filter would
//! really ask for. Every CONNECT is answered `200` and its request body is
//! echoed back on the response body, which is what makes "the tunnel
//! carries bytes both ways" a claim the peer can settle.
//!
//! What the server decoded — `:authority`, `:path`, `:protocol` — is kept
//! for the last request, so a test asserts what went on the wire rather
//! than what the client meant to send.
#![cfg(all(feature = "http2", not(target_family = "wasm")))]
#![allow(
    dead_code,
    reason = "a fixture module included by `#[path]`; each including test file uses its own subset"
)]

use std::sync::{Arc, Mutex};

use bytes::Bytes;

/// What the proxy does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum H2Proxy {
    /// Answer every CONNECT `200` and echo its body; `extended` decides
    /// whether `SETTINGS_ENABLE_CONNECT_PROTOCOL` is announced.
    Echo {
        /// Whether RFC 8441's setting is sent.
        extended: bool,
    },
}

/// A request as the proxy decoded it.
#[derive(Debug, Clone, Default)]
pub struct Seen {
    /// `:authority`.
    pub authority: String,
    /// `:path`, empty for a plain CONNECT, which has none.
    pub path: String,
    /// `:protocol`, `None` for a plain CONNECT.
    pub protocol: Option<String>,
}

/// A running proxy.
#[derive(Debug)]
pub struct Proxy {
    port: u16,
    cert: rustls::pki_types::CertificateDer<'static>,
    seen: Arc<Mutex<Option<Seen>>>,
    accepted: Arc<std::sync::atomic::AtomicUsize>,
}

impl Proxy {
    /// The port it listens on, on `127.0.0.1`.
    pub fn port(&self) -> u16 {
        self.port
    }

    /// The certificate it presents, for a client to trust.
    pub fn cert(&self) -> &rustls::pki_types::CertificateDer<'static> {
        &self.cert
    }

    /// The last request it decoded.
    ///
    /// # Panics
    ///
    /// If none arrived.
    pub fn last_request(&self) -> Seen {
        self.seen
            .lock()
            .unwrap()
            .clone()
            .expect("a request arrived")
    }

    /// TCP connections accepted.
    pub fn accepted(&self) -> usize {
        self.accepted.load(std::sync::atomic::Ordering::SeqCst)
    }
}

/// Start a proxy. Returns once it is bound.
///
/// # Panics
///
/// If no port can be bound on loopback.
pub fn h2_proxy(mode: H2Proxy) -> Proxy {
    let H2Proxy::Echo { extended } = mode;
    let cert = rcgen::generate_simple_self_signed(vec!["localhost".into()])
        .expect("rcgen can always make a self-signed cert");
    let cert_der = rustls::pki_types::CertificateDer::from(cert.cert.der().to_vec());
    let key_der = rustls::pki_types::PrivateKeyDer::try_from(cert.signing_key.serialize_der())
        .expect("a key rcgen just produced");
    let mut tls = rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(vec![cert_der.clone()], key_der)
        .expect("the cert and key were made together");
    tls.alpn_protocols = vec![b"h2".to_vec()];
    let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(tls));

    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().expect("local_addr").port();
    listener.set_nonblocking(true).expect("nonblocking");
    let seen: Arc<Mutex<Option<Seen>>> = Arc::new(Mutex::new(None));
    let accepted = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let (seen_t, accepted_t) = (Arc::clone(&seen), Arc::clone(&accepted));

    std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("a current-thread runtime");
        rt.block_on(async move {
            let listener = tokio::net::TcpListener::from_std(listener).expect("from_std");
            loop {
                let Ok((sock, _)) = listener.accept().await else {
                    return;
                };
                accepted_t.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                let (acceptor, seen) = (acceptor.clone(), Arc::clone(&seen_t));
                tokio::spawn(async move {
                    let Ok(tls) = acceptor.accept(sock).await else {
                        return;
                    };
                    let mut b = h2::server::Builder::new();
                    if extended {
                        b.enable_connect_protocol();
                    }
                    let Ok(mut conn) = b.handshake::<_, Bytes>(tls).await else {
                        return;
                    };
                    while let Some(Ok((req, respond))) = conn.accept().await {
                        let seen = Arc::clone(&seen);
                        tokio::spawn(echo(req, respond, seen));
                    }
                });
            }
        });
    });

    Proxy {
        port,
        cert: cert_der,
        seen,
        accepted,
    }
}

/// Record the request, answer `200`, and send every byte of its body back.
async fn echo(
    req: http::Request<h2::RecvStream>,
    mut respond: h2::server::SendResponse<Bytes>,
    seen: Arc<Mutex<Option<Seen>>>,
) {
    let (parts, mut body) = req.into_parts();
    *seen.lock().unwrap() = Some(Seen {
        authority: parts
            .uri
            .authority()
            .map(ToString::to_string)
            .unwrap_or_default(),
        path: parts
            .uri
            .path_and_query()
            .map(ToString::to_string)
            .unwrap_or_default(),
        protocol: parts
            .extensions
            .get::<h2::ext::Protocol>()
            .map(|p| p.as_str().to_owned()),
    });
    let resp = http::Response::builder().status(200).body(()).unwrap();
    let Ok(mut send) = respond.send_response(resp, false) else {
        return;
    };
    while let Some(Ok(chunk)) = body.data().await {
        let n = chunk.len();
        let _ = body.flow_control().release_capacity(n);
        if send.send_data(chunk, false).is_err() {
            return;
        }
    }
    let _ = send.send_data(Bytes::new(), true);
}

/// A client TLS backend trusting exactly this proxy's certificate.
///
/// # Panics
///
/// If the certificate is malformed — never expected of this module's own.
pub fn client_tls(proxy: &Proxy) -> hclient_tls_rustls::Rustls {
    let mut roots = rustls::RootCertStore::empty();
    roots.add(proxy.cert().clone()).expect("a DER certificate");
    let cfg = rustls::ClientConfig::builder()
        .with_root_certificates(roots)
        .with_no_client_auth();
    hclient_tls_rustls::Rustls::from_config(Arc::new(cfg))
}
