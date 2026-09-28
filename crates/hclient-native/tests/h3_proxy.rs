//! An HTTP/3 proxy that answers extended CONNECT and echoes its stream and
//! its datagrams, over real QUIC.
//!
//! An `h3::server` over quinn with an `rcgen` certificate naming
//! `localhost` and `127.0.0.1`. Every CONNECT is answered `200`, its
//! request body is echoed back on the response body, and every HTTP
//! datagram (RFC 9297) that arrives is echoed back with the same quarter
//! stream id — after a decoy carrying a different one, which a client that
//! did not filter by quarter stream id would hand over first. A `GET` is
//! answered `200` with a short body, so an ordinary request can reach the
//! same server.
//!
//! What the server read off each client's SETTINGS frame is kept, so a
//! test asserts what a connection announced rather than what the client
//! meant to.
#![cfg(all(feature = "http3", not(target_family = "wasm")))]
#![allow(
    dead_code,
    reason = "a fixture module included by `#[path]`; each including test file uses its own subset"
)]

use std::net::SocketAddr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use bytes::{Buf as _, Bytes};
use h3::ConnectionState as _;

/// What the proxy announces.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum H3Proxy {
    /// Answer every CONNECT `200` and echo it; `extended` and `datagrams`
    /// decide whether RFC 9220's and RFC 9297's settings are announced.
    Echo {
        /// `SETTINGS_ENABLE_CONNECT_PROTOCOL`.
        extended: bool,
        /// `SETTINGS_H3_DATAGRAM`.
        datagrams: bool,
    },
}

/// A request as the proxy decoded it.
#[derive(Debug, Clone, Default)]
pub struct Seen {
    /// `:method`.
    pub method: String,
    /// `:authority`.
    pub authority: String,
    /// `:path`.
    pub path: String,
    /// `:protocol`, `None` where none was sent.
    pub protocol: Option<String>,
    /// The request stream's id.
    pub stream_id: u64,
}

/// What one client connection announced in its SETTINGS frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Announced {
    /// `SETTINGS_ENABLE_CONNECT_PROTOCOL`.
    pub extended_connect: bool,
    /// `SETTINGS_H3_DATAGRAM`.
    pub datagrams: bool,
}

/// A running proxy.
#[derive(Debug)]
pub struct Proxy {
    addr: SocketAddr,
    cert: rustls::pki_types::CertificateDer<'static>,
    seen: Arc<Mutex<Option<Seen>>>,
    announced: Arc<Mutex<Vec<Announced>>>,
    quarters: Arc<Mutex<Vec<u64>>>,
    accepted: Arc<AtomicUsize>,
}

impl Proxy {
    /// The port it listens on, on `127.0.0.1`.
    pub fn port(&self) -> u16 {
        self.addr.port()
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

    /// What each accepted connection announced, in order of arrival.
    ///
    /// # Panics
    ///
    /// If the fixture's thread panicked holding the lock.
    pub fn announced(&self) -> Vec<Announced> {
        self.announced.lock().unwrap().clone()
    }

    /// The quarter stream id of every datagram that arrived.
    ///
    /// # Panics
    ///
    /// If the fixture's thread panicked holding the lock.
    pub fn quarters(&self) -> Vec<u64> {
        self.quarters.lock().unwrap().clone()
    }

    /// QUIC connections accepted.
    pub fn accepted(&self) -> usize {
        self.accepted.load(Ordering::SeqCst)
    }
}

/// Start a proxy on an ephemeral port. Returns once it is bound.
///
/// # Panics
///
/// If no port can be bound on loopback.
pub fn h3_proxy(mode: H3Proxy) -> Proxy {
    h3_proxy_on(mode, 0).expect("an ephemeral UDP port on loopback")
}

/// Start a proxy on `127.0.0.1:port`, or `None` where that UDP port is
/// taken — for standing it beside a TCP server on the same port number.
///
/// # Panics
///
/// If a certificate cannot be made, or the proxy's thread dies before it
/// answers.
pub fn h3_proxy_on(mode: H3Proxy, port: u16) -> Option<Proxy> {
    let H3Proxy::Echo {
        extended,
        datagrams,
    } = mode;
    let cert = rcgen::generate_simple_self_signed(vec!["localhost".into(), "127.0.0.1".into()])
        .expect("rcgen can always make a self-signed cert");
    let cert_der = rustls::pki_types::CertificateDer::from(cert.cert.der().to_vec());
    let key_der = rustls::pki_types::PrivateKeyDer::try_from(cert.signing_key.serialize_der())
        .expect("a key rcgen just produced");
    let mut tls = rustls::ServerConfig::builder_with_protocol_versions(&[&rustls::version::TLS13])
        .with_no_client_auth()
        .with_single_cert(vec![cert_der.clone()], key_der)
        .expect("the cert and key were made together");
    tls.alpn_protocols = vec![b"h3".to_vec()];
    let quic_tls = quinn::crypto::rustls::QuicServerConfig::try_from(tls)
        .expect("TLS 1.3 with a ring provider always has the initial suite");
    let cfg = quinn::ServerConfig::with_crypto(Arc::new(quic_tls));

    let seen: Arc<Mutex<Option<Seen>>> = Arc::new(Mutex::new(None));
    let announced: Arc<Mutex<Vec<Announced>>> = Arc::new(Mutex::new(Vec::new()));
    let quarters: Arc<Mutex<Vec<u64>>> = Arc::new(Mutex::new(Vec::new()));
    let accepted = Arc::new(AtomicUsize::new(0));
    let (seen_t, announced_t, quarters_t, accepted_t) = (
        Arc::clone(&seen),
        Arc::clone(&announced),
        Arc::clone(&quarters),
        Arc::clone(&accepted),
    );
    let (tx, rx) = std::sync::mpsc::channel();

    std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("a current-thread runtime");
        rt.block_on(async move {
            let Ok(endpoint) =
                quinn::Endpoint::server(cfg, SocketAddr::from(([127, 0, 0, 1], port)))
            else {
                let _ = tx.send(None);
                return;
            };
            tx.send(Some(endpoint.local_addr().unwrap())).unwrap();
            while let Some(incoming) = endpoint.accept().await {
                let (seen, announced, quarters, accepted) = (
                    Arc::clone(&seen_t),
                    Arc::clone(&announced_t),
                    Arc::clone(&quarters_t),
                    Arc::clone(&accepted_t),
                );
                tokio::spawn(async move {
                    let Ok(conn) = incoming.await else { return };
                    accepted.fetch_add(1, Ordering::SeqCst);
                    let quic = conn.clone();
                    let Ok(mut h3) = h3::server::builder()
                        .enable_extended_connect(extended)
                        .enable_datagram(datagrams)
                        .build::<_, Bytes>(h3_quinn::Connection::new(conn))
                        .await
                    else {
                        return;
                    };
                    // The client's SETTINGS are the first frame on its
                    // control stream; wait for them, so what is recorded is
                    // what the client announced rather than the default a
                    // connection reads before the frame arrives.
                    if std::future::poll_fn(|cx| h3.inner.poll_control(cx))
                        .await
                        .is_err()
                    {
                        return;
                    }
                    let s = h3.settings();
                    announced.lock().unwrap().push(Announced {
                        extended_connect: s.enable_extended_connect(),
                        datagrams: s.enable_datagram(),
                    });
                    tokio::spawn(echo_datagrams(quic, quarters));
                    while let Ok(Some(resolver)) = h3.accept().await {
                        let seen = Arc::clone(&seen);
                        tokio::spawn(async move {
                            let Ok((req, stream)) = resolver.resolve_request().await else {
                                return;
                            };
                            answer(req, stream, seen).await;
                        });
                    }
                });
            }
        });
    });

    let addr = rx.recv().expect("the proxy thread answers")?;
    Some(Proxy {
        addr,
        cert: cert_der,
        seen,
        announced,
        quarters,
        accepted,
    })
}

type ServerStream = h3::server::RequestStream<h3_quinn::BidiStream<Bytes>, Bytes>;

/// Record the request and answer it: a CONNECT is echoed, anything else
/// gets a short body.
async fn answer(req: http::Request<()>, mut stream: ServerStream, seen: Arc<Mutex<Option<Seen>>>) {
    let (parts, ()) = req.into_parts();
    *seen.lock().unwrap() = Some(Seen {
        method: parts.method.to_string(),
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
            .get::<h3::ext::Protocol>()
            .map(|p| p.as_str().to_owned()),
        stream_id: stream.id().into_inner(),
    });
    let resp = http::Response::builder().status(200).body(()).unwrap();
    if stream.send_response(resp).await.is_err() {
        return;
    }
    if parts.method != http::Method::CONNECT {
        let _ = stream.send_data(Bytes::from_static(b"ok")).await;
        let _ = stream.finish().await;
        return;
    }
    while let Ok(Some(mut chunk)) = stream.recv_data().await {
        let data = chunk.copy_to_bytes(chunk.remaining());
        if stream.send_data(data).await.is_err() {
            return;
        }
    }
    let _ = stream.finish().await;
}

/// Echo every datagram, after a decoy addressed to the next quarter
/// stream id.
async fn echo_datagrams(conn: quinn::Connection, quarters: Arc<Mutex<Vec<u64>>>) {
    while let Ok(frame) = conn.read_datagram().await {
        let Some((quarter, header)) = get_varint(&frame) else {
            continue;
        };
        quarters.lock().unwrap().push(quarter);
        let mut decoy = Vec::new();
        put_varint(&mut decoy, quarter + 1);
        decoy.extend_from_slice(b"decoy");
        let _ = conn.send_datagram(Bytes::from(decoy));
        let mut echo = Vec::new();
        put_varint(&mut echo, quarter);
        echo.extend_from_slice(&frame[header..]);
        let _ = conn.send_datagram(Bytes::from(echo));
    }
}

/// RFC 9000 §16, written out so the fixture does not share the client's
/// encoder: a client and a server agreeing because they are one function
/// would prove nothing.
fn put_varint(buf: &mut Vec<u8>, v: u64) {
    let bytes = v.to_be_bytes();
    if v < 1 << 6 {
        buf.push(bytes[7]);
    } else if v < 1 << 14 {
        buf.push(0x40 | bytes[6]);
        buf.push(bytes[7]);
    } else if v < 1 << 30 {
        buf.push(0x80 | bytes[4]);
        buf.extend_from_slice(&bytes[5..]);
    } else {
        buf.push(0xc0 | bytes[0]);
        buf.extend_from_slice(&bytes[1..]);
    }
}

fn get_varint(buf: &[u8]) -> Option<(u64, usize)> {
    let first = *buf.first()?;
    let len = 1usize << (first >> 6);
    if buf.len() < len {
        return None;
    }
    let mut v = u64::from(first & 0x3f);
    for b in &buf[1..len] {
        v = (v << 8) | u64::from(*b);
    }
    Some((v, len))
}

/// A client TLS backend trusting exactly this proxy's certificate.
///
/// # Panics
///
/// If the certificate is malformed — never expected of this module's own.
pub fn client_tls(proxy: &Proxy) -> hclient_tls_rustls::Rustls {
    trusting(&[proxy.cert()])
}

/// A client TLS backend trusting exactly these certificates.
///
/// # Panics
///
/// If one is malformed.
pub fn trusting(
    certs: &[&rustls::pki_types::CertificateDer<'static>],
) -> hclient_tls_rustls::Rustls {
    let mut roots = rustls::RootCertStore::empty();
    for c in certs {
        roots.add((*c).clone()).expect("a DER certificate");
    }
    let cfg = rustls::ClientConfig::builder_with_protocol_versions(&[&rustls::version::TLS13])
        .with_root_certificates(roots)
        .with_no_client_auth();
    hclient_tls_rustls::Rustls::from_config(Arc::new(cfg))
}
