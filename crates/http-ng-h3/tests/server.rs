//! A real HTTP/3 server on loopback, for the tests to talk to.
//!
//! Not a mock and not a recorded exchange: a `quinn` endpoint with an
//! `rcgen` certificate, speaking QUIC and HTTP/3 to whatever connects. The
//! research this crate is built on measured the whole client-plus-server
//! round trip at `wall=2.9ms` on this kind of setup, which is why these
//! tests run rather than merely compile.
//!
//! It runs on its own tokio runtime on its own thread, with quinn's own
//! `runtime-tokio`. That is deliberate: the *client* is the thing under
//! test, and a server sharing the client's runtime adapter would make a
//! green run ambiguous between "both work" and "both are wrong the same
//! way".
#![cfg(not(target_family = "wasm"))]
#![allow(dead_code)]

use bytes::Bytes;
use std::net::SocketAddr;
use std::sync::Arc;

/// What the server should do with the request it receives.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Behaviour {
    /// Answer `200` with a fixed body, immediately.
    Echo,
    /// Answer `200` after a delay — for measuring that waiting is not
    /// spinning.
    Slow(std::time::Duration),
    /// Answer `425 Too Early`, RFC 8470 §5.2.
    TooEarly,
}

pub struct Server {
    pub addr: SocketAddr,
    pub cert_der: rustls::pki_types::CertificateDer<'static>,
    /// How many QUIC connections the server has accepted. The observer is
    /// the server, never the client's own bookkeeping — the rule every
    /// pooling test in this workspace follows.
    pub accepted: Arc<std::sync::atomic::AtomicUsize>,
    /// How many HTTP/3 requests it has answered.
    pub requests: Arc<std::sync::atomic::AtomicUsize>,
    /// One entry per accepted connection, in accept order — see
    /// [`ConnTiming`]. Empty unless the server was started by
    /// [`start_watching_early_data`].
    pub timings: Arc<std::sync::Mutex<Vec<Arc<ConnTiming>>>>,
    _thread: std::thread::JoinHandle<()>,
}

/// When two things happened on one connection, **as the server saw them**,
/// both measured from the moment the connection reached the application.
///
/// # Why this and not `accepted_0rtt`
///
/// The obvious server-side signal does not exist. `Connecting::into_0rtt`
/// hands back a `ZeroRttAccepted` future which resolves to
/// `quinn_proto::Connection::accepted_0rtt`, and that field is assigned
/// **client-side only** — `quinn-proto-0.11.16/src/connection/mod.rs:2540`
/// guards the whole block with `if self.side.is_client()`, with a
/// `debug_assert!(self.side.is_client())` inside it for good measure. A
/// server therefore always reports `false`, whatever it did with the early
/// data. Measured, after writing a test against the assumption that it
/// meant something: it read `0` on a connection whose 0-RTT packets the
/// wire had just counted going past.
///
/// What a server *can* say is **when** it saw things, and with the client's
/// handshake held back by `wire::Wire::hold_server_flight` the two orders
/// are causally separated rather than merely likely: a request that arrives
/// before this connection's handshake completed cannot have travelled any
/// way but in early data the server chose to process.
#[derive(Debug, Default)]
pub struct ConnTiming {
    /// When the first HTTP/3 request on this connection was resolved.
    pub first_request: std::sync::Mutex<Option<std::time::Duration>>,
    /// When this connection's handshake completed.
    pub handshake_done: std::sync::Mutex<Option<std::time::Duration>>,
}

impl ConnTiming {
    pub fn first_request(&self) -> Option<std::time::Duration> {
        *self.first_request.lock().unwrap()
    }
    pub fn handshake_done(&self) -> Option<std::time::Duration> {
        *self.handshake_done.lock().unwrap()
    }
}

impl std::fmt::Debug for Server {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Server").field("addr", &self.addr).finish()
    }
}

impl Server {
    pub fn accepted(&self) -> usize {
        self.accepted.load(std::sync::atomic::Ordering::SeqCst)
    }
    pub fn requests(&self) -> usize {
        self.requests.load(std::sync::atomic::Ordering::SeqCst)
    }
    /// One [`ConnTiming`] per accepted connection, in accept order.
    pub fn timings(&self) -> Vec<Arc<ConnTiming>> {
        self.timings.lock().unwrap().clone()
    }
}

/// Start a server. Returns once it is bound, so a test never races it.
pub fn start(behaviour: Behaviour) -> Server {
    start_with_idle_timeout(behaviour, None)
}

/// A certificate and key, so that two servers can present one identity.
#[derive(Debug)]
pub struct Identity {
    pub cert_der: rustls::pki_types::CertificateDer<'static>,
    key_der: rustls::pki_types::PrivateKeyDer<'static>,
}

impl Clone for Identity {
    fn clone(&self) -> Self {
        Self {
            cert_der: self.cert_der.clone(),
            key_der: self.key_der.clone_key(),
        }
    }
}

pub fn identity() -> Identity {
    // Both names: the tests dial the literal `127.0.0.1` (so the resolver
    // is not a second thing under test), and rcgen turns an IP-shaped SAN
    // into an IP SAN, which is what rustls checks a literal against.
    let cert = rcgen::generate_simple_self_signed(vec!["localhost".into(), "127.0.0.1".into()])
        .expect("rcgen can always make a self-signed cert");
    Identity {
        cert_der: rustls::pki_types::CertificateDer::from(cert.cert.der().to_vec()),
        key_der: rustls::pki_types::PrivateKeyDer::try_from(cert.signing_key.serialize_der())
            .unwrap(),
    }
}

/// Two servers presenting the same certificate, each with **its own
/// ticketer** — which is what makes the second reject a ticket the first
/// issued, with nothing else about the exchange differing.
///
/// This needs no plumbing: the ticketer is per `rustls::ServerConfig` and
/// each builds its own. It is also what happens in the real world behind a
/// load balancer that does not share ticket keys.
pub fn start_two_sharing_a_certificate(behaviour: Behaviour) -> (Server, Server) {
    let id = identity();
    (
        start_full(behaviour, None, id.clone()),
        start_full(behaviour, None, id),
    )
}

pub fn start_with_idle_timeout(behaviour: Behaviour, idle: Option<std::time::Duration>) -> Server {
    start_full(behaviour, idle, identity())
}

/// A server that hands its connections to the application **before the
/// handshake completes**, and records when each of the two happened — see
/// [`ConnTiming`].
///
/// That is what `Connecting::into_0rtt()` does on the server side: it
/// always succeeds there (`quinn-0.11.11/src/connection.rs:131` —
/// `has_0rtt() || side().is_server()`), so it is not evidence of anything
/// by itself; it is the only way to be handed the connection early enough
/// for "early" to be observable at all, and the only source of the
/// completion signal.
///
/// Opt-in rather than the default, because taking this path would start the
/// h3 layer before the handshake finished for every other test in this
/// suite — changing what they measure for the benefit of one that needs it.
pub fn start_watching_early_data(behaviour: Behaviour) -> Server {
    start_inner(behaviour, None, identity(), true)
}

pub fn start_full(behaviour: Behaviour, idle: Option<std::time::Duration>, id: Identity) -> Server {
    start_inner(behaviour, idle, id, false)
}

fn start_inner(
    behaviour: Behaviour,
    idle: Option<std::time::Duration>,
    id: Identity,
    watch_early_data: bool,
) -> Server {
    let Identity { cert_der, key_der } = id;

    let mut tls = rustls::ServerConfig::builder_with_protocol_versions(&[&rustls::version::TLS13])
        .with_no_client_auth()
        .with_single_cert(vec![cert_der.clone()], key_der)
        .expect("the cert and key were made together");
    tls.alpn_protocols = vec![b"h3".to_vec()];
    // 0-RTT on the server side: without this, `into_0rtt` on the client has
    // no key material to work with and there is nothing to test.
    tls.max_early_data_size = u32::MAX;
    tls.send_half_rtt_data = true;

    let quic_tls = quinn::crypto::rustls::QuicServerConfig::try_from(tls)
        .expect("TLS 1.3 with a ring provider always has the initial suite");
    let mut cfg = quinn::ServerConfig::with_crypto(Arc::new(quic_tls));
    if let Some(d) = idle {
        let mut t = quinn::TransportConfig::default();
        t.max_idle_timeout(Some(d.try_into().unwrap()));
        // Deliberately NOT a keep-alive: the point of the idle test is that
        // something has to drive the connection, and a keep-alive on the
        // server would hide whether the client's driver is running.
        cfg.transport_config(Arc::new(t));
    }

    let (tx, rx) = std::sync::mpsc::channel();
    let accepted = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let requests = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let timings: Arc<std::sync::Mutex<Vec<Arc<ConnTiming>>>> =
        Arc::new(std::sync::Mutex::new(Vec::new()));
    let (a, r, ts) = (accepted.clone(), requests.clone(), timings.clone());

    let thread = std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async move {
            let endpoint = quinn::Endpoint::server(cfg, "127.0.0.1:0".parse().unwrap()).unwrap();
            tx.send(endpoint.local_addr().unwrap()).unwrap();
            while let Some(incoming) = endpoint.accept().await {
                let (a, r, ts) = (a.clone(), r.clone(), ts.clone());
                tokio::spawn(async move {
                    let started = std::time::Instant::now();
                    let timing = Arc::new(ConnTiming::default());
                    let conn = if watch_early_data {
                        let Ok(connecting) = incoming.accept() else {
                            return;
                        };
                        // Always `Ok` on the server side; what it is for is
                        // the connection arriving before the handshake, and
                        // the completion signal that comes with it.
                        let Ok((conn, done)) = connecting.into_0rtt() else {
                            return;
                        };
                        ts.lock().unwrap().push(timing.clone());
                        let t = timing.clone();
                        tokio::spawn(async move {
                            // The `bool` is deliberately discarded: on a
                            // server it is always `false`. See `ConnTiming`.
                            let _ = done.await;
                            *t.handshake_done.lock().unwrap() = Some(started.elapsed());
                        });
                        conn
                    } else {
                        let Ok(conn) = incoming.await else { return };
                        conn
                    };
                    a.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    let Ok(mut h3) =
                        h3::server::Connection::new(h3_quinn::Connection::new(conn)).await
                    else {
                        return;
                    };
                    while let Ok(Some(resolver)) = h3.accept().await {
                        let (r, timing) = (r.clone(), timing.clone());
                        tokio::spawn(async move {
                            let Ok((_req, mut stream)) = resolver.resolve_request().await else {
                                return;
                            };
                            timing
                                .first_request
                                .lock()
                                .unwrap()
                                .get_or_insert(started.elapsed());
                            r.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                            if let Behaviour::Slow(d) = behaviour {
                                tokio::time::sleep(d).await;
                            }
                            let status = match behaviour {
                                Behaviour::TooEarly => http::StatusCode::TOO_EARLY,
                                _ => http::StatusCode::OK,
                            };
                            let resp = http::Response::builder().status(status).body(()).unwrap();
                            if stream.send_response(resp).await.is_err() {
                                return;
                            }
                            let _ = stream.send_data(Bytes::from_static(b"hello over h3")).await;
                            let _ = stream.finish().await;
                        });
                    }
                });
            }
        });
    });

    Server {
        addr: rx
            .recv()
            .expect("the server thread binds before it answers"),
        cert_der,
        accepted,
        requests,
        timings,
        _thread: thread,
    }
}

/// A `Rustls` that trusts exactly this server's certificate, and nothing
/// else.
pub fn client_tls(cert: &rustls::pki_types::CertificateDer<'static>) -> http_ng_tls_rustls::Rustls {
    let mut roots = rustls::RootCertStore::empty();
    roots.add(cert.clone()).unwrap();
    let cfg = rustls::ClientConfig::builder_with_protocol_versions(&[&rustls::version::TLS13])
        .with_root_certificates(roots)
        .with_no_client_auth();
    http_ng_tls_rustls::Rustls::from_config(Arc::new(cfg))
}
