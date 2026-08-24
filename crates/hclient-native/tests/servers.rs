//! Two real servers behind **one authority**, so that "which stack was
//! chosen" is a question a peer answers.
//!
//! An HTTP/3 server on UDP and an HTTP/1.1 server on TCP, on the **same
//! port number** — which is possible because the two are separate port
//! spaces, and necessary because otherwise the origin the client dials
//! would differ between the two arms and the record would no longer be the
//! only thing that changed. Both present the same certificate and both are
//! alive throughout every test, so a request reaching either one is a
//! choice rather than the only thing that could have happened.
//!
//! Neither server is a mock. `crates/hclient-h3/tests/server.rs` (a `quinn`
//! endpoint with an `rcgen` certificate) and
//! `crates/hclient-native/tests/tls_server_name.rs` (`tokio-rustls` over a
//! real `TcpListener`) are where the two halves come from; what is new here
//! is the pairing and the counting.
//!
//! Each server counts what it *answered*, and the tests assert on both
//! counters at once: "the QUIC one answered and the TCP one did not" is a
//! claim neither counter can make alone.
#![cfg(not(target_family = "wasm"))]
#![allow(dead_code)]

use bytes::Bytes;
use std::fmt::Debug;
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

/// The authority every test dials. A name rather than a literal, because
/// an HTTPS record is looked up under a name and this is the string the
/// resolver is handed.
pub const ORIGIN: &str = "both-stacks.test";

/// What both servers put in an `Alt-Svc` response header field, or `None`
/// for a response that carries none.
///
/// Shared by the two servers and settable **between** requests, which is
/// what makes the slow tier testable at all: an origin advertises `h3` in
/// the answer to request 1, and the test can then have it withdraw the
/// advertisement, or change its `ma`, before request 2 — the same origin,
/// the same connection story, a different thing said.
type AltSvc = Arc<Mutex<Option<String>>>;

/// What the QUIC half of the pair does.
///
/// The failure modes are here rather than in a test file because they are
/// *server* behaviours, and because the choice between them is the one
/// decision the negative-half tests had to make: §9.3 recorded the
/// black hole as the blocker (*"the arm under test would be a multi-second
/// handshake timeout — a clock-driven assertion"*), and
/// [`Quic::Rejecting`] is the way round it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Quic {
    /// A real HTTP/3 server that answers.
    Working,
    /// A real QUIC server that **refuses the handshake**, by offering an
    /// ALPN this client will not accept.
    ///
    /// The point is that it fails *causally and at once* — one round trip
    /// on loopback, a TLS `no_application_protocol` alert — where a black
    /// hole costs quinn's 30 s `max_idle_timeout` on every path. What the
    /// failure memory remembers is *"the connect failed"*, and it does not
    /// read the reason, so this produces the fact under test without
    /// producing a clock-driven test.
    Rejecting,
    /// The UDP port is **bound and silent**: no ICMP, no reply, nothing —
    /// a `DROP` rather than a `REJECT`, which is what a network blocking
    /// UDP/443 actually does.
    ///
    /// Holding the socket is the whole mechanism: an *unbound* port makes
    /// the kernel answer ICMP port
    /// unreachable. Used only where a test needs the QUIC arm to spend a
    /// `Timeouts::connect` bound rather than to fail.
    BlackHole,
}

/// What the TCP half of the pair does.
///
/// The second mode exists for the same reason [`Quic::Rejecting`] does, one
/// protocol over: a test of what happens when an arm **fails** needs it to
/// fail causally and at once, or the assertion becomes a clock.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tcp {
    /// A real HTTP/1.1-over-TLS server that answers.
    Working,
    /// Accepts the connection and closes it without a byte, so the client's
    /// TLS handshake fails on EOF in one round trip.
    ///
    /// It still counts the accept, which is what makes *"the request did not
    /// try TCP a second time"* a number rather than an absence.
    Rejecting,
}

/// Two servers, one port number, one certificate.
pub struct Pair {
    pub port: u16,
    pub cert_der: rustls::pki_types::CertificateDer<'static>,
    alt_svc: AltSvc,
    /// HTTP/1.1 requests answered over TLS on TCP.
    tcp_answered: Arc<AtomicUsize>,
    /// TCP connections accepted, whether or not a request was ever read —
    /// the only thing an `http://` arm can observe, since a cleartext
    /// request never gets past the handshake.
    tcp_accepted: Arc<AtomicUsize>,
    /// HTTP/3 requests answered over QUIC on UDP.
    quic_answered: Arc<AtomicUsize>,
    /// QUIC connection attempts that reached the endpoint, answered or
    /// not — the counter a *failed* handshake still moves, and therefore
    /// the only one that can tell "this request did not try QUIC" from
    /// "this request tried QUIC and the handshake was refused".
    ///
    /// Always zero under [`Quic::BlackHole`], which has no endpoint to
    /// accept anything.
    quic_attempted: Arc<AtomicUsize>,
    /// UDP datagrams that arrived at a [`Quic::BlackHole`], and nothing
    /// else — the working and rejecting servers hand their socket to
    /// `quinn`, which does its own reading.
    ///
    /// This is the only thing a black hole can be observed by, and it is
    /// what makes *"this request did not try QUIC"* an assertion where
    /// [`Pair::quic_attempted`] cannot be: a hole has no endpoint, so it
    /// accepts nothing however many `ClientHello`s it swallows.
    quic_datagrams: Arc<AtomicUsize>,
    _threads: (std::thread::JoinHandle<()>, std::thread::JoinHandle<()>),
}

impl Debug for Pair {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Pair")
            .field("port", &self.port)
            .field("tcp_answered", &self.tcp_answered())
            .field("quic_answered", &self.quic_answered())
            .finish()
    }
}

impl Pair {
    pub fn tcp_answered(&self) -> usize {
        self.tcp_answered.load(Ordering::SeqCst)
    }
    pub fn tcp_accepted(&self) -> usize {
        self.tcp_accepted.load(Ordering::SeqCst)
    }
    pub fn quic_answered(&self) -> usize {
        self.quic_answered.load(Ordering::SeqCst)
    }
    pub fn quic_attempted(&self) -> usize {
        self.quic_attempted.load(Ordering::SeqCst)
    }
    pub fn quic_datagrams(&self) -> usize {
        self.quic_datagrams.load(Ordering::SeqCst)
    }
    pub fn addr(&self) -> SocketAddr {
        SocketAddr::from(([127, 0, 0, 1], self.port))
    }

    /// What both servers advertise from now on. `None` sends no field at
    /// all, which is not the same instruction as an empty one.
    pub fn set_alt_svc(&self, value: Option<&str>) {
        *self.alt_svc.lock().expect("alt-svc fixture") = value.map(str::to_owned);
    }

    /// The advertisement a server built on this pair's own port —
    /// `h3=":<port>"`, which is the only alt-authority this transport can
    /// act on, since the request keeps the origin's authority whatever the
    /// field names.
    pub fn h3_here(&self, params: &str) -> String {
        format!("h3=\":{}\"{params}", self.port)
    }
}

/// A port free on **both** TCP and UDP, with the two sockets already held
/// so nothing can take them in between.
///
/// The retry is the whole mechanism: `bind(0)` picks a port free on *one*
/// protocol and says nothing about the other, so the pair is found by
/// asking and trying again.
///
/// **Both directions are tried, alternately, and that is not belt and
/// braces.** The original asked TCP first, always, with a comment reading
/// *"sixty-four attempts against an ephemeral range of ~28000 ports is not
/// a bound anyone reaches"* — which is Linux's range and Linux's
/// allocator. It was reached on `windows-latest`, by three of these tests
/// at once, because nextest runs them in parallel and they compete for the
/// same pairs. Windows also reserves whole port blocks (Hyper-V does this
/// on the hosted runners) and the reservations need not be the same for
/// the two protocols — so a TCP port whose UDP twin is reserved is not bad
/// luck that another draw fixes, it is a region the TCP allocator keeps
/// handing out. Asking UDP first on alternate attempts escapes a
/// reservation that is one-sided in either direction.
///
/// The count is raised with it, and the failure now carries the OS error:
/// a bound that is reached should say *why* rather than only that it was.
fn bind_pair() -> (std::net::TcpListener, std::net::UdpSocket) {
    let mut last = None;
    for attempt in 0..256 {
        if attempt % 2 == 0 {
            let tcp = std::net::TcpListener::bind("127.0.0.1:0").expect("a loopback TCP bind");
            let port = tcp.local_addr().expect("local_addr").port();
            match std::net::UdpSocket::bind(("127.0.0.1", port)) {
                Ok(udp) => return (tcp, udp),
                Err(e) => last = Some(("udp", port, e)),
            }
        } else {
            let udp = std::net::UdpSocket::bind("127.0.0.1:0").expect("a loopback UDP bind");
            let port = udp.local_addr().expect("local_addr").port();
            match std::net::TcpListener::bind(("127.0.0.1", port)) {
                Ok(tcp) => return (tcp, udp),
                Err(e) => last = Some(("tcp", port, e)),
            }
        }
    }
    match last {
        Some((which, port, e)) => panic!(
            "no port was free on both TCP and UDP in 256 attempts, alternating which \
             protocol chose it; the last failure was the {which} bind on port {port}: {e}"
        ),
        None => panic!("no port was free on both TCP and UDP in 256 attempts"),
    }
}

/// One self-signed certificate naming [`ORIGIN`] and the loopback literal,
/// presented by both servers — so the two arms differ in the protocol and
/// in nothing else, the identity included.
fn identity() -> (
    rustls::pki_types::CertificateDer<'static>,
    rustls::pki_types::PrivateKeyDer<'static>,
) {
    let cert = rcgen::generate_simple_self_signed(vec![ORIGIN.into(), "127.0.0.1".into()])
        .expect("rcgen can always make a self-signed cert");
    (
        rustls::pki_types::CertificateDer::from(cert.cert.der().to_vec()),
        rustls::pki_types::PrivateKeyDer::try_from(cert.signing_key.serialize_der())
            .expect("a key rcgen just produced"),
    )
}

/// Start both, with a QUIC server that works.
pub fn start() -> Pair {
    start_with_quic(Quic::Working)
}

/// Start both, varying only the QUIC half.
pub fn start_with_quic(quic: Quic) -> Pair {
    start_with(quic, Tcp::Working)
}

/// Start both. Returns once both are bound, so no test races them.
pub fn start_with(quic: Quic, tcp: Tcp) -> Pair {
    let (tcp_sock, udp_sock) = bind_pair();
    let port = tcp_sock.local_addr().expect("local_addr").port();
    let (cert_der, key_der) = identity();

    let tcp_answered = Arc::new(AtomicUsize::new(0));
    let tcp_accepted = Arc::new(AtomicUsize::new(0));
    let quic_answered = Arc::new(AtomicUsize::new(0));
    let quic_attempted = Arc::new(AtomicUsize::new(0));
    let quic_datagrams = Arc::new(AtomicUsize::new(0));
    let alt_svc: AltSvc = Arc::default();

    let tcp_thread = start_tcp(
        tcp_sock,
        cert_der.clone(),
        key_der.clone_key(),
        tcp_answered.clone(),
        tcp_accepted.clone(),
        alt_svc.clone(),
        tcp,
    );
    let quic_thread = match quic {
        // The socket is held by a thread that answers nothing, so the port
        // stays bound and nothing is ever replied to. Holding rather than
        // dropping: a dropped socket is an *unbound* port, which is the
        // other failure and costs the same 30 s for a different reason.
        //
        // It **reads**, and counting what it read is the only observation
        // a hole can offer. Reading changes nothing a client can see —
        // nothing is ever sent back either way — and it is what turns
        // *"that hop did not try QUIC"* from an absence into a delta.
        Quic::BlackHole => {
            let seen = quic_datagrams.clone();
            std::thread::spawn(move || {
                let mut buf = [0u8; 2048];
                loop {
                    match udp_sock.recv_from(&mut buf) {
                        Ok(_) => {
                            seen.fetch_add(1, Ordering::SeqCst);
                        }
                        Err(_) => return,
                    }
                }
            })
        }
        _ => start_quic(
            udp_sock,
            cert_der.clone(),
            key_der,
            quic_answered.clone(),
            quic_attempted.clone(),
            alt_svc.clone(),
            quic,
        ),
    };

    Pair {
        port,
        cert_der,
        alt_svc,
        tcp_answered,
        tcp_accepted,
        quic_answered,
        quic_attempted,
        quic_datagrams,
        _threads: (tcp_thread, quic_thread),
    }
}

/// HTTP/1.1 over TLS, by hand.
///
/// `alpn_protocols` is `http/1.1` alone on purpose: with
/// `hclient-native`'s `http2` feature on — which `--all-features` turns on
/// — the client offers `h2` first, and a server that accepted it would
/// make this arm's protocol depend on a feature flag rather than on the
/// choice under test. The request line is read and discarded; what is
/// counted is that a well-formed request arrived over a completed
/// handshake.
fn start_tcp(
    listener: std::net::TcpListener,
    cert: rustls::pki_types::CertificateDer<'static>,
    key: rustls::pki_types::PrivateKeyDer<'static>,
    answered: Arc<AtomicUsize>,
    accepted: Arc<AtomicUsize>,
    alt_svc: AltSvc,
    mode: Tcp,
) -> std::thread::JoinHandle<()> {
    let mut tls = rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(vec![cert], key)
        .expect("the cert and key were made together");
    tls.alpn_protocols = vec![b"http/1.1".to_vec()];
    let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(tls));

    std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("a current-thread runtime");
        rt.block_on(async move {
            listener.set_nonblocking(true).expect("nonblocking");
            let listener = tokio::net::TcpListener::from_std(listener).expect("from_std");
            loop {
                let Ok((sock, _)) = listener.accept().await else {
                    return;
                };
                accepted.fetch_add(1, Ordering::SeqCst);
                // Counted first, then dropped: a refusal a test cannot see
                // is not a fixture. The drop closes the socket before a
                // single TLS byte is written, which the client meets as an
                // EOF in the handshake — causal, and one round trip.
                if mode == Tcp::Rejecting {
                    drop(sock);
                    continue;
                }
                let (acceptor, answered, alt_svc) =
                    (acceptor.clone(), answered.clone(), alt_svc.clone());
                tokio::spawn(async move {
                    use tokio::io::{AsyncReadExt, AsyncWriteExt};
                    let Ok(mut stream) = acceptor.accept(sock).await else {
                        return;
                    };
                    let mut head = Vec::new();
                    let mut byte = [0u8; 1];
                    while !head.ends_with(b"\r\n\r\n") {
                        match stream.read(&mut byte).await {
                            Ok(1) => head.push(byte[0]),
                            _ => return,
                        }
                    }
                    // Counted before the write, so a test that sees the
                    // response has certainly seen this increment.
                    answered.fetch_add(1, Ordering::SeqCst);
                    // Read at answer time rather than at start time, so a
                    // test can change what this origin says between two
                    // requests — which is the whole shape of the slow
                    // tier.
                    let advertisement = match &*alt_svc.lock().expect("alt-svc fixture") {
                        Some(v) => format!("alt-svc: {v}\r\n"),
                        None => String::new(),
                    };
                    // **`connection: close`, and leaving it out was the
                    // fixture's bug rather than a scenario.** This server
                    // answers once and shuts down, and RFC 9112 §9.6 makes
                    // announcing that a MUST for any sender that will not
                    // continue. Without the header the client pools a
                    // connection the peer has already closed and the next
                    // request races the FIN — which is a real defect of
                    // `hclient-native`'s pooled-reuse window, recorded in
                    // `h1.rs`, and not the thing any test in this file is
                    // about. It stayed invisible while Nagle put 41 ms
                    // between the two requests; `nodelay` removed the pad
                    // and it appeared at 7 runs in 12 under `-j16`.
                    let _ = stream
                        .write_all(
                            format!(
                                "HTTP/1.1 200 OK\r\ncontent-length: 2\r\nconnection: close\r\n{advertisement}\r\nh1"
                            )
                            .as_bytes(),
                        )
                        .await;
                    let _ = stream.flush().await;
                    // `shutdown` rather than a drop: it writes TLS
                    // `close_notify` first, and rustls on the client side
                    // reports a socket that vanished without one as an
                    // error — which would make every TCP arm here fail for
                    // a reason that has nothing to do with the choice
                    // under test. Measured: without this line the five
                    // TCP-arm tests fail with `peer closed connection
                    // without sending TLS close_notify`.
                    let _ = stream.shutdown().await;
                });
            }
        });
    })
}

/// HTTP/3 over QUIC, on the UDP socket bound to the same port number.
///
/// `Endpoint::new` rather than `Endpoint::server` because the socket was
/// bound outside — it had to be, since finding a port free on both
/// protocols means holding both while trying.
fn start_quic(
    socket: std::net::UdpSocket,
    cert: rustls::pki_types::CertificateDer<'static>,
    key: rustls::pki_types::PrivateKeyDer<'static>,
    answered: Arc<AtomicUsize>,
    attempted: Arc<AtomicUsize>,
    alt_svc: AltSvc,
    mode: Quic,
) -> std::thread::JoinHandle<()> {
    let mut tls = rustls::ServerConfig::builder_with_protocol_versions(&[&rustls::version::TLS13])
        .with_no_client_auth()
        .with_single_cert(vec![cert], key)
        .expect("the cert and key were made together");
    // The refusal is one byte of configuration and one round trip on the
    // wire: the client offers `h3` alone (RFC 9114 §3.2 makes it mandatory
    // rather than a preference), this server offers something else, and
    // rustls sends `no_application_protocol`. Everything else about the
    // server — the certificate, the port, the address — is identical, so a
    // test using it differs from one using the working server in exactly
    // the fact under test.
    tls.alpn_protocols = match mode {
        Quic::Working => vec![b"h3".to_vec()],
        Quic::Rejecting | Quic::BlackHole => vec![b"h3-nothing-speaks-this".to_vec()],
    };
    let quic_tls = quinn::crypto::rustls::QuicServerConfig::try_from(tls)
        .expect("TLS 1.3 with a ring provider always has the initial suite");
    let server_config = quinn::ServerConfig::with_crypto(Arc::new(quic_tls));

    std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("a current-thread runtime");
        rt.block_on(async move {
            let endpoint = quinn::Endpoint::new(
                quinn::EndpointConfig::default(),
                Some(server_config),
                socket,
                quinn::default_runtime().expect("the runtime-tokio feature is on"),
            )
            .expect("the socket is already bound");
            while let Some(incoming) = endpoint.accept().await {
                // Bumped before the handshake is awaited, so a connection
                // this server is about to refuse still counts as an
                // attempt — which is the whole reason this counter exists
                // beside `quic_answered`.
                attempted.fetch_add(1, Ordering::SeqCst);
                let (answered, alt_svc) = (answered.clone(), alt_svc.clone());
                tokio::spawn(async move {
                    let Ok(conn) = incoming.await else { return };
                    let Ok(mut h3) =
                        h3::server::Connection::new(h3_quinn::Connection::new(conn)).await
                    else {
                        return;
                    };
                    while let Ok(Some(resolver)) = h3.accept().await {
                        let (answered, alt_svc) = (answered.clone(), alt_svc.clone());
                        tokio::spawn(async move {
                            let Ok((_req, mut stream)) = resolver.resolve_request().await else {
                                return;
                            };
                            answered.fetch_add(1, Ordering::SeqCst);
                            // The QUIC server advertises too. An origin
                            // that could only withdraw an advertisement
                            // over TCP could never withdraw one at all
                            // once a client had moved to HTTP/3.
                            let advertisement = alt_svc.lock().expect("alt-svc fixture").clone();
                            let mut resp = http::Response::builder().status(http::StatusCode::OK);
                            // `from_maybe_shared` rather than `header(..)`
                            // so that a value only the TCP arm can write —
                            // a repeated field line, which is two `\r\n`
                            // separated lines on that wire and nothing at
                            // all on this one — leaves this server sending
                            // no field instead of failing to build a
                            // response.
                            if let Some(v) = advertisement
                                .and_then(|v| http::HeaderValue::from_maybe_shared(v).ok())
                            {
                                resp = resp.header("alt-svc", v);
                            }
                            let resp = resp.body(()).expect("a 200 with no body");
                            if stream.send_response(resp).await.is_err() {
                                return;
                            }
                            let _ = stream.send_data(Bytes::from_static(b"h3")).await;
                            let _ = stream.finish().await;
                        });
                    }
                });
            }
        });
    })
}

/// A `Rustls` trusting exactly this pair's certificate and nothing else —
/// used for both members, so neither arm has an identity advantage.
pub fn client_tls(cert: &rustls::pki_types::CertificateDer<'static>) -> hclient_tls_rustls::Rustls {
    let mut roots = rustls::RootCertStore::empty();
    roots.add(cert.clone()).expect("a DER certificate");
    let cfg = rustls::ClientConfig::builder_with_protocol_versions(&[&rustls::version::TLS13])
        .with_root_certificates(roots)
        .with_no_client_auth();
    hclient_tls_rustls::Rustls::from_config(Arc::new(cfg))
}
