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
    /// Send the response **head**, wait, and then tear the whole QUIC
    /// connection down — so the failure arrives while the caller is reading
    /// the body rather than while it is waiting for a head.
    ///
    /// The two are different observers in `tests/hooks.rs`:
    /// [`Behaviour::DieAfterHead`] makes `H3::execute` the one that meets
    /// the connection's death, and this makes `H3Body::poll_frame` the one.
    /// A transport that reported a close from only one of them would pass
    /// the other's test.
    ///
    /// The wait is not a bound on anything and nothing is asserted about
    /// it: it only orders the head in front of the close, on a loopback
    /// where the head takes microseconds. If it ever lost that race the
    /// test would fail loudly — no `Head` event at all — rather than
    /// quietly passing.
    HeadThenDie,
    /// Tear the whole QUIC connection down the moment the request head has
    /// arrived, without answering.
    ///
    /// The opposite pole to [`Behaviour::Echo`] for a client still writing
    /// its request body: `Echo` stops reading and the response survives,
    /// this kills the connection and nothing can. A client that told the
    /// two apart by tolerating everything would pass one and hang on the
    /// other.
    DieAfterHead,
    /// Read the request body to its end, **then** answer `200` with the
    /// byte count as the response body.
    ///
    /// The ordinary shape of an upload, and the only one that can say
    /// whether a whole streamed body arrived: the count comes from the
    /// server's own reading, so a client that sent one frame and stopped
    /// cannot produce it.
    CountBody,
    /// Answer `200` **before reading a single byte** of the request body,
    /// then read it to the end and send the byte count as the response
    /// body.
    ///
    /// This is the server half of duplex. Paired with a client whose body
    /// does not produce its last chunk until the head has been seen, it
    /// makes the exchange impossible to complete without duplex rather
    /// than merely slower — see `tests/streaming.rs`.
    HeadThenRead,
    /// [`Behaviour::CountBody`], but pausing between DATA frames.
    ///
    /// Not a benchmark and not padding: it is what makes "the client is
    /// still writing" a fact rather than a race. A reader this slow lets
    /// the peer's flow-control window fill and stay full, so a client with
    /// a body larger than the window **cannot** have finished, whatever
    /// the machine is doing. `tests/streaming.rs`'s buffered-cancellation
    /// test needs exactly that.
    ReadSlowly(std::time::Duration),
    /// Send the whole response — head, body, end — **without ever reading
    /// the request body**, and then hold the request stream open for `d`
    /// rather than dropping it.
    ///
    /// The holding is the point, and it is what tells this apart from
    /// [`Behaviour::Echo`]. Dropping an unread `RecvStream` sends
    /// `STOP_SENDING`, which lets a blocked client stop writing; keeping it
    /// alive leaves the client's flow-control window full with no way out.
    /// So the client is certainly still writing while the response is
    /// there to be read — which is the one arrangement in which a response
    /// body that waited on the request body would hang instead of merely
    /// being slow.
    AnswerWithoutReading(std::time::Duration),
}

/// What one request's body looked like **from the server**, with the
/// server's own clock, measured from the moment the request head resolved.
///
/// The rule every timing test in this workspace follows: the client is the
/// thing under test, so the observer is the other end of the wire.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct BodyReport {
    /// Request-body bytes read before the stream ended or failed.
    pub bytes: usize,
    /// How many DATA frames those bytes arrived in.
    pub frames: usize,
    /// `true` if the request stream ended cleanly, `false` if it failed —
    /// which on this path means the client reset it.
    pub complete: bool,
    /// When the response head was sent. `None` for behaviours that send it
    /// after reading.
    pub head_sent: Option<std::time::Duration>,
    /// When the last request-body byte arrived.
    pub last_byte: Option<std::time::Duration>,
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
    /// One entry per request whose body this server read — see
    /// [`BodyReport`]. Empty unless the behaviour is [`Behaviour::
    /// CountBody`] or [`Behaviour::HeadThenRead`].
    pub bodies: Arc<std::sync::Mutex<Vec<BodyReport>>>,
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
    /// One [`BodyReport`] per request whose body was read, in the order the
    /// reads finished.
    pub fn bodies(&self) -> Vec<BodyReport> {
        self.bodies.lock().unwrap().clone()
    }
    /// Wait for the server to have finished reading `n` request bodies.
    ///
    /// A poll rather than a sleep: the thing being waited for is the
    /// server's own record, so the test never has to guess how long a
    /// reset takes to arrive. Returns `false` if it did not happen inside
    /// `within`, so a caller can fail with its own message.
    pub async fn wait_for_bodies(&self, n: usize, within: std::time::Duration) -> bool {
        let deadline = std::time::Instant::now() + within;
        while std::time::Instant::now() < deadline {
            if self.bodies.lock().unwrap().len() >= n {
                return true;
            }
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
        self.bodies.lock().unwrap().len() >= n
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
    // All three names: the tests dial the literal `127.0.0.1` (so the
    // resolver is not a second thing under test), and rcgen turns an
    // IP-shaped SAN into an IP SAN, which is what rustls checks a literal
    // against. `::1` is `tests/quic_server_name.rs`'s — the only authority
    // a URI writes in brackets — and `localhost` is that file's control
    // that the brackets come off a bracketed host and nothing else.
    let cert = rcgen::generate_simple_self_signed(vec![
        "localhost".into(),
        "127.0.0.1".into(),
        "::1".into(),
    ])
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

/// The pair above, with a **flow-control window too small to hold h3's
/// SETTINGS frame** — which is how a client's control-stream write is held
/// open across the instant a server refuses its early data.
///
/// `quinn::TransportConfig::stream_receive_window` is what this endpoint
/// sends as `initial_max_stream_data_uni`
/// (`quinn-proto-0.11.16/src/transport_parameters.rs:160`), and a client
/// resuming with 0-RTT writes against the value it remembered from the
/// ticket — so setting it on **both** servers is what puts the window in
/// force on the resumed connection as well as on the one that issued the
/// ticket.
///
/// Nothing else changes. On a connection whose handshake is ordinary the
/// peer's h3 layer reads the control stream, `MAX_STREAM_DATA` comes back
/// and the write finishes; the ticket-issuing exchange below is that case
/// and it is unremarkable. It is only on a connection whose early data is
/// **discarded** that the credit never arrives, because the write itself
/// was discarded with it — and the write is then parked in exactly the gap
/// `docs/v04-h3-0rtt-control-stream.md` §2.1 measures, instead of having to
/// be caught there by luck.
pub fn start_two_sharing_a_certificate_and_a_tiny_window(
    behaviour: Behaviour,
    window: u64,
) -> (Server, Server) {
    let id = identity();
    (
        start_windowed(behaviour, id.clone(), window),
        start_windowed(behaviour, id, window),
    )
}

fn start_windowed(behaviour: Behaviour, id: Identity, window: u64) -> Server {
    start_inner(behaviour, None, id, false, v4(), Some(window)).expect("a v4 loopback bind")
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
    start_inner(behaviour, None, identity(), true, v4(), None).expect("a v4 loopback bind")
}

pub fn start_full(behaviour: Behaviour, idle: Option<std::time::Duration>, id: Identity) -> Server {
    start_inner(behaviour, idle, id, false, v4(), None).expect("a v4 loopback bind")
}

/// The address every other server in this suite binds.
fn v4() -> SocketAddr {
    "127.0.0.1:0".parse().unwrap()
}

/// A server on the **IPv6** loopback, or `None` on a host that has none.
///
/// The one thing a v4 server cannot say anything about: an IPv6 authority
/// is the only one a URI writes in brackets, and the brackets are what
/// `tests/quic_server_name.rs` is about. `None` rather than a panic for the
/// same reason `an_endpoint_is_bound_in_the_peers_address_family` skips its
/// v6 half in `src/lib.rs` — a host without IPv6 is a fact about the host.
pub fn start_on_v6(behaviour: Behaviour) -> Option<Server> {
    start_inner(
        behaviour,
        None,
        identity(),
        false,
        "[::1]:0".parse().unwrap(),
        None,
    )
}

fn start_inner(
    behaviour: Behaviour,
    idle: Option<std::time::Duration>,
    id: Identity,
    watch_early_data: bool,
    bind: SocketAddr,
    // See `start_two_sharing_a_certificate_and_a_tiny_window`. `None`
    // leaves quinn's default, which is what every other server here uses.
    stream_window: Option<u64>,
) -> Option<Server> {
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
    if let Some(w) = stream_window {
        let mut t = quinn::TransportConfig::default();
        t.stream_receive_window(w.try_into().expect("a window that fits a varint"));
        cfg.transport_config(Arc::new(t));
    }

    let (tx, rx) = std::sync::mpsc::channel();
    let accepted = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let requests = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let timings: Arc<std::sync::Mutex<Vec<Arc<ConnTiming>>>> =
        Arc::new(std::sync::Mutex::new(Vec::new()));
    let bodies: Arc<std::sync::Mutex<Vec<BodyReport>>> =
        Arc::new(std::sync::Mutex::new(Vec::new()));
    let (a, r, ts, bs) = (
        accepted.clone(),
        requests.clone(),
        timings.clone(),
        bodies.clone(),
    );

    let thread = std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async move {
            let endpoint = match quinn::Endpoint::server(cfg, bind) {
                Ok(e) => e,
                // The only expected failure is "this host has no IPv6", and
                // the caller has to be able to tell it apart from a server
                // that bound and then went quiet.
                Err(_) => {
                    let _ = tx.send(None);
                    return;
                }
            };
            tx.send(Some(endpoint.local_addr().unwrap())).unwrap();
            while let Some(incoming) = endpoint.accept().await {
                let (a, r, ts, bs) = (a.clone(), r.clone(), ts.clone(), bs.clone());
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
                    // Kept beside the h3 layer, for `DieAfterHead`: closing
                    // the QUIC connection is not something the h3 API
                    // exposes, and it is the point of that behaviour.
                    let quic = conn.clone();
                    let Ok(mut h3) =
                        h3::server::Connection::new(h3_quinn::Connection::new(conn)).await
                    else {
                        return;
                    };
                    while let Ok(Some(resolver)) = h3.accept().await {
                        let (r, timing, quic) = (r.clone(), timing.clone(), quic.clone());
                        let bodies = bs.clone();
                        tokio::spawn(async move {
                            let Ok((_req, mut stream)) = resolver.resolve_request().await else {
                                return;
                            };
                            let t0 = std::time::Instant::now();
                            timing
                                .first_request
                                .lock()
                                .unwrap()
                                .get_or_insert(started.elapsed());
                            r.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                            if behaviour == Behaviour::DieAfterHead {
                                quic.close(1u32.into(), b"dying on purpose");
                                return;
                            }
                            if behaviour == Behaviour::HeadThenDie {
                                let resp = http::Response::builder()
                                    .status(http::StatusCode::OK)
                                    .body(())
                                    .unwrap();
                                if stream.send_response(resp).await.is_err() {
                                    return;
                                }
                                tokio::time::sleep(std::time::Duration::from_millis(150)).await;
                                quic.close(1u32.into(), b"dying after the head");
                                return;
                            }
                            if let Behaviour::AnswerWithoutReading(d) = behaviour {
                                let resp = http::Response::builder()
                                    .status(http::StatusCode::OK)
                                    .body(())
                                    .unwrap();
                                if stream.send_response(resp).await.is_err() {
                                    return;
                                }
                                let _ =
                                    stream.send_data(Bytes::from_static(b"hello over h3")).await;
                                let _ = stream.finish().await;
                                // Holding `stream`, and therefore its unread
                                // receive half, so no `STOP_SENDING` goes out.
                                tokio::time::sleep(d).await;
                                return;
                            }
                            if matches!(
                                behaviour,
                                Behaviour::CountBody
                                    | Behaviour::HeadThenRead
                                    | Behaviour::ReadSlowly(_)
                            ) {
                                let mut report = BodyReport::default();
                                if behaviour == Behaviour::HeadThenRead {
                                    // Before a single byte of the request
                                    // body has been read, which is the
                                    // whole point of this behaviour.
                                    let resp = http::Response::builder()
                                        .status(http::StatusCode::OK)
                                        .body(())
                                        .unwrap();
                                    if stream.send_response(resp).await.is_err() {
                                        return;
                                    }
                                    report.head_sent = Some(t0.elapsed());
                                }
                                loop {
                                    match stream.recv_data().await {
                                        Ok(Some(buf)) => {
                                            report.bytes += bytes::Buf::remaining(&buf);
                                            report.frames += 1;
                                            report.last_byte = Some(t0.elapsed());
                                            if let Behaviour::ReadSlowly(d) = behaviour {
                                                tokio::time::sleep(d).await;
                                            }
                                        }
                                        Ok(None) => {
                                            report.complete = true;
                                            break;
                                        }
                                        // The client reset the stream, or
                                        // the connection went. Either way
                                        // what arrived is not the whole
                                        // request, and saying so is the
                                        // point of the field.
                                        Err(_) => break,
                                    }
                                }
                                // Recorded before the tail is sent, so a
                                // test waiting on a cancelled upload is not
                                // waiting on a write to a peer that has
                                // gone.
                                bodies.lock().unwrap().push(report);
                                if behaviour != Behaviour::HeadThenRead {
                                    let resp = http::Response::builder()
                                        .status(http::StatusCode::OK)
                                        .body(())
                                        .unwrap();
                                    if stream.send_response(resp).await.is_err() {
                                        return;
                                    }
                                }
                                let _ = stream
                                    .send_data(Bytes::from(format!("{} bytes", report.bytes)))
                                    .await;
                                let _ = stream.finish().await;
                                return;
                            }
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

    Some(Server {
        addr: rx
            .recv()
            .expect("the server thread binds before it answers")?,
        cert_der,
        accepted,
        requests,
        timings,
        bodies,
        _thread: thread,
    })
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
