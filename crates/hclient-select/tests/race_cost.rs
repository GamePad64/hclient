//! **A measurement, not a feature.** What a race between the two stacks
//! would cost, in numbers, so that the policy is chosen against
//! something.
//!
//! Everything here is `#[ignore]`d and prints rather than asserts. That is
//! deliberate and is the rule this workspace already follows: three timing
//! assertions here turned out to be flakes and one was hiding a real
//! defect, so a harness whose output is *numbers* may be timing-based
//! where a harness whose output is a *verdict* may not. Run it with
//!
//! ```text
//! cargo nextest run -p hclient-select --test race_cost --run-ignored all \
//!     --no-capture -j1
//! ```
//!
//! `-j1` because several of these bind ports and time things; `--no-capture`
//! because the output is the point. The whole file takes about three
//! minutes, almost all of it spent waiting for quinn to give up.
//!
//! # The fixture, and why loopback is enough
//!
//! The race needs a fixture that can actually block UDP/443, and loopback
//! was thought not to manage it. It can.
//! [`black_hole`] binds a UDP socket and answers nothing, **holding** it —
//! so the kernel generates no ICMP port-unreachable, and from the client's
//! side that is a packet filter with a `DROP` target and not a `REJECT`
//! one. There is a case loopback genuinely cannot produce, and it is a
//! different one: a middlebox that *refuses*. [`refusing_hole`] is the
//! nearest reachable approximation — no socket bound at all, so the kernel
//! itself answers ICMP — and §7 of the acceptance document records which
//! of the two each number below came from.
//!
//! What loopback cannot supply at all is a **round trip**: every success
//! figure here is a floor, the cost with the network taken out. That
//! matters less than it looks, and the reason is the finding: the failure
//! costs are not functions of the path at all.
#![cfg(not(target_family = "wasm"))]

mod fakedns;

use bytes::Bytes;
use fakedns::FakeDns;
use hclient_core::unversioned::Transport;
use hclient_core::{RequestBody, Timeouts};
use hclient_h3::H3;
use hclient_native::Native;
use hclient_rt_tokio::TokioHandle;
use http_body_util::BodyExt;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// The authority every request here dials — a name, because that is what a
/// resolver is handed and what the certificate has to carry.
const ORIGIN: &str = "race-cost.test";

type Tls = hclient_tls_rustls::Rustls;
type Quic = H3<TokioHandle, Tls, FakeDns>;
type Tcp = Native<TokioHandle, Tls, FakeDns>;

// --- what the servers are ------------------------------------------------

/// One self-signed certificate naming [`ORIGIN`] and the loopback literal.
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

/// A `Rustls` trusting exactly this certificate — the same one both arms
/// get, so neither has an identity advantage.
fn client_tls(cert: &rustls::pki_types::CertificateDer<'static>) -> Tls {
    let mut roots = rustls::RootCertStore::empty();
    roots.add(cert.clone()).expect("a DER certificate");
    let cfg = rustls::ClientConfig::builder_with_protocol_versions(&[&rustls::version::TLS13])
        .with_root_certificates(roots)
        .with_no_client_auth();
    hclient_tls_rustls::Rustls::from_config(Arc::new(cfg))
}

/// A port free on **both** TCP and UDP, with both sockets held so nothing
/// can take one in between. The same mechanism `tests/servers.rs` uses,
/// and duplicated rather than shared on purpose: this file is a harness
/// with a lifetime of its own, and a fixture two suites share is a fixture
/// neither can change.
fn bind_pair() -> (std::net::TcpListener, std::net::UdpSocket) {
    for _ in 0..64 {
        let tcp = std::net::TcpListener::bind("127.0.0.1:0").expect("a loopback TCP bind");
        let port = tcp.local_addr().expect("local_addr").port();
        if let Ok(udp) = std::net::UdpSocket::bind(("127.0.0.1", port)) {
            return (tcp, udp);
        }
    }
    panic!("no port was free on both TCP and UDP in 64 attempts");
}

/// The log a black hole keeps: when it was last reset, and every datagram
/// since, as an offset from that moment with its length.
#[derive(Debug)]
struct Log {
    since: Instant,
    seen: Vec<(Duration, usize)>,
}

/// A handle onto one of those, shared between the receiving thread and the
/// test.
#[derive(Clone, Debug)]
struct Arrivals(Arc<Mutex<Log>>);

impl Arrivals {
    fn new() -> Self {
        Self(Arc::new(Mutex::new(Log {
            since: Instant::now(),
            seen: Vec::new(),
        })))
    }
    /// Restart the clock and forget everything — so a test can measure a
    /// second phase (after a drop, say) without subtracting a first.
    fn reset(&self) {
        let mut g = self.0.lock().expect("arrivals");
        g.since = Instant::now();
        g.seen.clear();
    }
    fn since_reset(&self) -> Vec<(Duration, usize)> {
        self.0.lock().expect("arrivals").seen.clone()
    }
    fn count(&self) -> usize {
        self.0.lock().expect("arrivals").seen.len()
    }
}

/// A UDP socket that receives and answers nothing, **and stays bound**.
///
/// Staying bound is the whole fixture: an unbound port makes the kernel
/// send ICMP port-unreachable, which is a `REJECT`; a bound socket that
/// never replies is a `DROP`, which is what a firewall blocking UDP/443
/// does. What comes back is the timestamp of every datagram the client
/// sent, which is how the client's own retransmission schedule becomes
/// observable from outside it.
fn black_hole(socket: std::net::UdpSocket) -> Arrivals {
    let arrivals = Arrivals::new();
    let sink = arrivals.clone();
    std::thread::spawn(move || {
        let mut buf = [0u8; 2048];
        loop {
            match socket.recv_from(&mut buf) {
                Ok((n, _)) => {
                    let mut g = sink.0.lock().expect("arrivals");
                    let t = g.since.elapsed();
                    g.seen.push((t, n));
                }
                Err(_) => return,
            }
        }
    });
    arrivals
}

/// A port with a TCP server on it and **nothing at all** bound on UDP, so
/// the kernel answers ICMP port-unreachable: the `REJECT` case, as near as
/// a single host can get to it.
fn refusing_hole(udp: std::net::UdpSocket) {
    drop(udp);
}

/// HTTP/1.1 over TLS on a real TCP socket, counting what it accepted and
/// what it answered.
///
/// Both counters, because the race's cost on the TCP arm is *sockets and
/// handshakes started*, not only requests completed: an arm that opened a
/// connection and was cancelled before its request went out still cost the
/// server a TLS handshake, and only `accepted` sees that.
struct TcpServer {
    accepted: Arc<AtomicUsize>,
    answered: Arc<AtomicUsize>,
    _thread: std::thread::JoinHandle<()>,
}

impl TcpServer {
    fn accepted(&self) -> usize {
        self.accepted.load(Ordering::SeqCst)
    }
    fn answered(&self) -> usize {
        self.answered.load(Ordering::SeqCst)
    }
}

fn start_tcp(
    listener: std::net::TcpListener,
    cert: rustls::pki_types::CertificateDer<'static>,
    key: rustls::pki_types::PrivateKeyDer<'static>,
) -> TcpServer {
    let accepted = Arc::new(AtomicUsize::new(0));
    let answered = Arc::new(AtomicUsize::new(0));
    let (a, b) = (accepted.clone(), answered.clone());

    let mut tls = rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(vec![cert], key)
        .expect("the cert and key were made together");
    // `http/1.1` alone: with `hclient-native`'s `http2` feature on — which
    // `--all-features` turns on — the client offers `h2` first, and this
    // harness is not measuring h2.
    tls.alpn_protocols = vec![b"http/1.1".to_vec()];
    let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(tls));

    let thread = std::thread::spawn(move || {
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
                a.fetch_add(1, Ordering::SeqCst);
                let (acceptor, answered) = (acceptor.clone(), b.clone());
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
                    answered.fetch_add(1, Ordering::SeqCst);
                    let _ = stream
                        .write_all(b"HTTP/1.1 200 OK\r\ncontent-length: 2\r\n\r\nh1")
                        .await;
                    let _ = stream.flush().await;
                    // rustls on the client reports a socket that vanished
                    // without `close_notify` as an error; `shutdown` writes
                    // one.
                    let _ = stream.shutdown().await;
                });
            }
        });
    });

    TcpServer {
        accepted,
        answered,
        _thread: thread,
    }
}

/// A real HTTP/3 server on UDP, for the arms where QUIC is meant to win.
fn start_quic(
    socket: std::net::UdpSocket,
    cert: rustls::pki_types::CertificateDer<'static>,
    key: rustls::pki_types::PrivateKeyDer<'static>,
) -> Arc<AtomicUsize> {
    let answered = Arc::new(AtomicUsize::new(0));
    let counter = answered.clone();

    let mut tls = rustls::ServerConfig::builder_with_protocol_versions(&[&rustls::version::TLS13])
        .with_no_client_auth()
        .with_single_cert(vec![cert], key)
        .expect("the cert and key were made together");
    tls.alpn_protocols = vec![b"h3".to_vec()];
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
                let counter = counter.clone();
                tokio::spawn(async move {
                    let Ok(conn) = incoming.await else { return };
                    let Ok(mut h3) =
                        h3::server::Connection::new(h3_quinn::Connection::new(conn)).await
                    else {
                        return;
                    };
                    while let Ok(Some(resolver)) = h3.accept().await {
                        let counter = counter.clone();
                        tokio::spawn(async move {
                            let Ok((_req, mut stream)) = resolver.resolve_request().await else {
                                return;
                            };
                            counter.fetch_add(1, Ordering::SeqCst);
                            let resp = http::Response::builder()
                                .status(http::StatusCode::OK)
                                .body(())
                                .expect("a 200 with no body");
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
    });

    answered
}

// --- the clients ---------------------------------------------------------

fn quic(cert: &rustls::pki_types::CertificateDer<'static>) -> Quic {
    let rt = TokioHandle::current().expect("inside #[tokio::test]");
    H3::new(rt, client_tls(cert), FakeDns::new()).expect("H3::new does no I/O")
}

fn tcp(cert: &rustls::pki_types::CertificateDer<'static>) -> Tcp {
    let rt = TokioHandle::current().expect("inside #[tokio::test]");
    Native::new(rt, client_tls(cert), FakeDns::new())
}

/// The same TCP transport on the **unit** `Tokio` runtime rather than the
/// handle, with Nagle off or on as asked.
///
/// Two things force this shape, and both are findings rather than
/// convenience:
///
/// 1. `TcpOpts::default()` is all-off — *"the user turns nodelay on, not
///    us"*, `hclient-rt`'s `caps.rs` — so [`tcp`] above connects with
///    Nagle **on**, and the TLS handshake's small writes then meet the
///    peer's delayed ACK.
/// 2. `TokioHandle` cannot be asked for it. Its `TcpConnect::connect`
///    delegates to `Tokio::connect`, which applies every option, but it
///    does **not** restate `APPLIES`, so it inherits the trait's `NONE`
///    and `Native::tcp_opts` refuses `nodelay` with an
///    `UnsupportedTcpOpts` naming it. Measured, not read: the first
///    version of this function used `TokioHandle` and panicked with that
///    error. `Tokio` states `TcpOptsSupport::ALL` and accepts it.
///
/// The second is why this control also has a Nagle-**on** arm on the same
/// runtime: without it the comparison would be confounded by the runtime
/// having changed as well as the option.
fn tcp_on_unit_runtime(
    cert: &rustls::pki_types::CertificateDer<'static>,
    nodelay: bool,
) -> Native<hclient_rt_tokio::Tokio, Tls, FakeDns> {
    Native::new(hclient_rt_tokio::Tokio, client_tls(cert), FakeDns::new())
        .tcp_opts(hclient_rt::TcpOpts {
            nodelay,
            ..Default::default()
        })
        .expect("`Tokio` declares TcpOptsSupport::ALL")
}

fn get(port: u16) -> http::Request<RequestBody> {
    http::Request::builder()
        .uri(format!("https://{ORIGIN}:{port}/hello"))
        .body(RequestBody::Empty)
        .expect("a well-formed request")
}

/// The same request to the same servers with **no name in it** — the
/// certificate carries the loopback literal too.
///
/// This is the control that separates "what the transport's own
/// scheduling costs" from "what a connect costs": an IP literal skips
/// resolution, and with it RFC 8305's `resolution_delay`.
fn get_literal(port: u16) -> http::Request<RequestBody> {
    http::Request::builder()
        .uri(format!("https://127.0.0.1:{port}/hello"))
        .body(RequestBody::Empty)
        .expect("a well-formed request")
}

fn bounded(port: u16, connect: Duration) -> http::Request<RequestBody> {
    let mut req = get(port);
    req.extensions_mut().insert(Timeouts {
        resolve: None,
        connect: Some(connect),
        ..Default::default()
    });
    req
}

/// Run one exchange to completion — head **and** body — and time it.
///
/// `Error = hclient_core::Error` rather than the trait's own
/// `std::error::Error` bound, because the *kind* is what distinguishes a
/// connect timeout from a refusal and that is the whole question here.
/// Both members satisfy it.
async fn exchange<T>(t: &T, req: http::Request<RequestBody>) -> (Duration, String)
where
    T: Transport<Error = hclient_core::Error>,
    <T::Body as http_body::Body>::Error: std::fmt::Display,
{
    let started = Instant::now();
    match t.execute(req).await {
        Ok(r) => {
            let status = r.status();
            let version = r.version();
            let text = match r.into_body().collect().await {
                Ok(b) => String::from_utf8_lossy(&b.to_bytes()).into_owned(),
                Err(e) => format!("<body error: {e}>"),
            };
            (started.elapsed(), format!("{status} {version:?} {text:?}"))
        }
        Err(e) => (started.elapsed(), format!("ERR {:?}: {e}", e.kind())),
    }
}

/// min / median / max of a set of samples.
fn summarise(label: &str, mut xs: Vec<Duration>) {
    xs.sort();
    let n = xs.len();
    println!(
        "  {label:<46} n={n:<3} min {:>10.3?}  median {:>10.3?}  max {:>10.3?}",
        xs[0],
        xs[n / 2],
        xs[n - 1]
    );
}

fn timeline(label: &str, xs: &[(Duration, usize)]) {
    println!("  {label} — {} datagram(s):", xs.len());
    for (t, n) in xs {
        println!("      +{t:>10.3?}  {n:>5} bytes");
    }
}

// =========================================================================
// M1 — what a QUIC connect to a black hole costs today, end to end
// =========================================================================

/// v0.3 measured 30 s for one path — quinn's own idle timeout. This
/// confirms it for *this* shape: `hclient_h3::H3::execute` with no
/// `Timeouts::connect` set, against a held UDP socket that answers
/// nothing, with a live TCP server on the same port number so the origin
/// is reachable by the other stack throughout.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "measurement: takes ~90 s"]
async fn m1_a_quic_connect_to_a_black_hole_with_no_bound() {
    let (tcp_sock, udp_sock) = bind_pair();
    let port = tcp_sock.local_addr().expect("local_addr").port();
    let (cert, key) = identity();
    let _tcp_server = start_tcp(tcp_sock, cert.clone(), key);
    let arrivals = black_hole(udp_sock);

    println!("\nM1 — QUIC connect to a UDP black hole, no `Timeouts::connect`");
    let mut samples = Vec::new();
    for i in 0..3 {
        arrivals.reset();
        // A fresh `H3` each time: the endpoint and the pool are per
        // instance, and reusing one would measure the second attempt
        // through a cache rather than a cold connect.
        let t = quic(&cert);
        let (took, outcome) = exchange(&t, get(port)).await;
        println!("  run {i}: {took:.3?}  {outcome}");
        if i == 0 {
            timeline("    what the hole saw", &arrivals.since_reset());
        }
        samples.push(took);
    }
    summarise("QUIC -> black hole, unbounded", samples);
}

/// The same shape with the bound a caller would actually set, so that the
/// *reduction* is a number rather than a claim. Three bounds, because a
/// policy has to choose one and the shape of the curve is the argument.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "measurement"]
async fn m1b_the_same_black_hole_with_a_connect_bound() {
    let (tcp_sock, udp_sock) = bind_pair();
    let port = tcp_sock.local_addr().expect("local_addr").port();
    let (cert, key) = identity();
    let _tcp_server = start_tcp(tcp_sock, cert.clone(), key);
    let arrivals = black_hole(udp_sock);

    println!("\nM1b — the same black hole, bounded");
    for ms in [100u64, 300, 1000, 3000] {
        arrivals.reset();
        let t = quic(&cert);
        let (took, outcome) = exchange(&t, bounded(port, Duration::from_millis(ms))).await;
        println!(
            "  bound {ms:>5} ms -> {took:>10.3?}  overshoot {:>9.3?}  datagrams {}  {outcome}",
            took.saturating_sub(Duration::from_millis(ms)),
            arrivals.count()
        );
    }
}

// =========================================================================
// M2 — the earliest honest signal
// =========================================================================

/// A QUIC handshake that *will* succeed, and a TCP exchange that will, on
/// the same origin — so the gap between "QUIC is working" and "QUIC is
/// never going to answer" is a number.
///
/// Both are floors: loopback has no round trip, so what is measured is the
/// CPU cost of the handshake and nothing else. The figure that does *not*
/// depend on the path is in M2b.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "measurement"]
async fn m2a_a_handshake_that_succeeds_on_either_stack() {
    let (tcp_sock, udp_sock) = bind_pair();
    let port = tcp_sock.local_addr().expect("local_addr").port();
    let (cert, key) = identity();
    let tcp_server = start_tcp(tcp_sock, cert.clone(), key.clone_key());
    let quic_answered = start_quic(udp_sock, cert.clone(), key);

    println!("\nM2a — a cold exchange that succeeds, both stacks, loopback (floors)");
    let mut q = Vec::new();
    let mut t = Vec::new();
    let mut ql = Vec::new();
    let mut tl = Vec::new();
    let mut tu = Vec::new();
    let mut tn = Vec::new();
    for _ in 0..10 {
        // Fresh transports every iteration: both pool, and a pooled
        // connection is not a connect.
        let (took, _) = exchange(&quic(&cert), get(port)).await;
        q.push(took);
        let (took, _) = exchange(&tcp(&cert), get(port)).await;
        t.push(took);
        let (took, _) = exchange(&quic(&cert), get_literal(port)).await;
        ql.push(took);
        let (took, _) = exchange(&tcp(&cert), get_literal(port)).await;
        tl.push(took);
        let (took, _) = exchange(&tcp_on_unit_runtime(&cert, false), get(port)).await;
        tu.push(took);
        let (took, _) = exchange(&tcp_on_unit_runtime(&cert, true), get(port)).await;
        tn.push(took);
    }
    summarise("QUIC by name (UDP + TLS1.3 + SETTINGS + GET)", q);
    summarise("TCP  by name (SYN + TLS1.3 + GET)", t);
    summarise("QUIC by IP literal (no resolution)", ql);
    summarise("TCP  by IP literal (no resolution)", tl);
    summarise("TCP  on `Tokio`, nodelay off (Nagle on)", tu);
    summarise("TCP  on `Tokio`, nodelay ON", tn);
    println!(
        "  server counters: quic answered {}, tcp accepted {}, tcp answered {}",
        quic_answered.load(Ordering::SeqCst),
        tcp_server.accepted(),
        tcp_server.answered()
    );
}

/// The signal that is **not** a function of the path: quinn's own
/// retransmission schedule against a black hole, read from the hole.
///
/// A client with no RTT sample has to guess one, so the first PTO fires at
/// a *constant* — and that constant is the earliest moment anything in the
/// stack has evidence that something is wrong. Everything before it is the
/// client waiting exactly as it would on a slow but working path.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "measurement: takes ~30 s"]
async fn m2b_when_quinn_itself_first_suspects() {
    let (tcp_sock, udp_sock) = bind_pair();
    let port = tcp_sock.local_addr().expect("local_addr").port();
    let (cert, key) = identity();
    let _tcp_server = start_tcp(tcp_sock, cert.clone(), key);
    let arrivals = black_hole(udp_sock);

    println!("\nM2b — the Initial retransmission ladder, seen from the black hole");
    arrivals.reset();
    let t = quic(&cert);
    let (took, outcome) = exchange(&t, get(port)).await;
    timeline("    what the hole saw", &arrivals.since_reset());
    println!("  gave up after {took:.3?}: {outcome}");
}

/// The TCP arm's *failure* time on the same host, for the other half of
/// the comparison: a connect to a port nothing is listening on.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "measurement"]
async fn m2c_a_tcp_connect_that_is_refused() {
    // A port that was free and is now free again: bound, port read, dropped.
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().expect("local_addr").port();
    drop(listener);
    let (cert, _key) = identity();

    println!("\nM2c — a TCP connect to a closed port (the RST case)");
    let mut xs = Vec::new();
    for _ in 0..10 {
        let (took, _) = exchange(&tcp(&cert), get(port)).await;
        xs.push(took);
    }
    summarise("TCP -> closed port", xs);
}

/// Where the forty milliseconds are, since a headline number that turned
/// out to be an artefact of the fixture would be worse than no number.
///
/// `execute` returning a head and the body arriving after it are timed
/// separately, with Nagle on and off on the same runtime — so the delay is
/// attributed to a phase rather than to the transport in general.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "measurement"]
async fn m2d_which_phase_the_delay_is_in() {
    let (tcp_sock, udp_sock) = bind_pair();
    let port = tcp_sock.local_addr().expect("local_addr").port();
    let (cert, key) = identity();
    let _tcp_server = start_tcp(tcp_sock, cert.clone(), key.clone_key());
    let _quic_answered = start_quic(udp_sock, cert.clone(), key);

    println!("\nM2d — head and body, timed apart");
    for (label, nodelay) in [("Nagle on ", false), ("nodelay  ", true)] {
        let mut heads = Vec::new();
        let mut bodies = Vec::new();
        for _ in 0..10 {
            let t = tcp_on_unit_runtime(&cert, nodelay);
            let started = Instant::now();
            let resp = t.execute(get(port)).await.expect("the TCP server answered");
            let head = started.elapsed();
            let body_started = Instant::now();
            let _ = resp.into_body().collect().await.expect("a complete body");
            heads.push(head);
            bodies.push(body_started.elapsed());
        }
        summarise(&format!("TCP {label} — `execute` to head"), heads);
        summarise(&format!("TCP {label} — head to end of body"), bodies);
    }
    let mut heads = Vec::new();
    for _ in 0..10 {
        let t = quic(&cert);
        let started = Instant::now();
        let resp = t
            .execute(get(port))
            .await
            .expect("the QUIC server answered");
        heads.push(started.elapsed());
        let _ = resp.into_body().collect().await;
    }
    summarise("QUIC — `execute` to head", heads);
}

// =========================================================================
// M3 — what a race costs when QUIC wins
// =========================================================================

/// The hedge's whole justification is that it is nearly free in the common
/// case. This measures what the TCP arm actually costs there: sockets
/// accepted, handshakes completed, requests answered, and how much later
/// the answer arrives than it would with no race at all.
///
/// Three head starts, because the head start is the policy variable.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "measurement"]
async fn m3_a_race_that_quic_wins() {
    let (tcp_sock, udp_sock) = bind_pair();
    let port = tcp_sock.local_addr().expect("local_addr").port();
    let (cert, key) = identity();
    let tcp_server = start_tcp(tcp_sock, cert.clone(), key.clone_key());
    let quic_answered = start_quic(udp_sock, cert.clone(), key);

    println!("\nM3 — a race against a working QUIC origin");
    // Both Nagle settings, because the first run of this measurement
    // found the losing TCP arm delivering a complete request to the
    // origin **after** its future had been dropped, and the obvious
    // suspicion was that the bytes had been sitting in a Nagle-held
    // kernel buffer. With `nodelay` the same thing happens sooner rather
    // than not at all, which is what makes it a finding about the race
    // and not about the option.
    for nodelay in [false, true] {
        println!("  --- TCP arm with nodelay = {nodelay} ---");
        for head_start in [
            Duration::ZERO,
            Duration::from_millis(1),
            Duration::from_millis(50),
            Duration::from_millis(300),
        ] {
            let before = (
                tcp_server.accepted(),
                tcp_server.answered(),
                quic_answered.load(Ordering::SeqCst),
            );
            let q = quic(&cert);
            let n = tcp_on_unit_runtime(&cert, nodelay);
            let started = Instant::now();
            let winner = race(&q, &n, port, head_start).await;
            let took = started.elapsed();
            // The losing arm's future is already dropped — `race` returns by
            // value and the loser goes out of scope inside it. The counters
            // are read TWICE for that reason: what the server had already
            // seen at the instant of the drop, and what it saw in the second
            // after it. The difference between the two readings is the only
            // thing that can distinguish "the loser had got that far" from
            // "the loser kept going".
            let at_drop = (tcp_server.accepted(), tcp_server.answered());
            tokio::time::sleep(Duration::from_secs(1)).await;
            println!(
                "  head start {head_start:>8.3?}: winner {winner:<5} in {took:>9.3?} \
                 | at the drop: tcp accepted +{} answered +{} \
                 | 1 s later: +{} / +{} | quic answered +{}",
                at_drop.0 - before.0,
                at_drop.1 - before.1,
                tcp_server.accepted() - before.0,
                tcp_server.answered() - before.1,
                quic_answered.load(Ordering::SeqCst) - before.2,
            );
        }
    }
}

// =========================================================================
// M4 — what a race costs when QUIC loses
// =========================================================================

/// The black-hole case: nothing ever comes back on UDP.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "measurement"]
async fn m4a_a_race_that_quic_loses_to_a_black_hole() {
    let (tcp_sock, udp_sock) = bind_pair();
    let port = tcp_sock.local_addr().expect("local_addr").port();
    let (cert, key) = identity();
    let tcp_server = start_tcp(tcp_sock, cert.clone(), key);
    let arrivals = black_hole(udp_sock);

    println!("\nM4a — a race against a UDP black hole (DROP)");
    for head_start in [
        Duration::ZERO,
        Duration::from_millis(50),
        Duration::from_millis(300),
        Duration::from_millis(1000),
    ] {
        arrivals.reset();
        let before = (tcp_server.accepted(), tcp_server.answered());
        let q = quic(&cert);
        let n = tcp(&cert);
        let started = Instant::now();
        let winner = race(&q, &n, port, head_start).await;
        let took = started.elapsed();
        let during = arrivals.count();
        // The QUIC arm was dropped when `race` returned. Whether it
        // actually stopped is M5's question; this is the same observation
        // taken here, where the drop follows a *win* rather than a
        // cancellation in the abstract.
        tokio::time::sleep(Duration::from_millis(1500)).await;
        println!(
            "  head start {head_start:>8.3?}: winner {winner:<5} in {took:>9.3?} \
             | tcp accepted +{} answered +{} | udp datagrams {during} during, \
             {} more in the 1.5 s after the drop",
            tcp_server.accepted() - before.0,
            tcp_server.answered() - before.1,
            arrivals.count() - during,
        );
    }
}

/// The other failure, and the task's premise was that it is a different
/// one: an origin that simply has no HTTP/3 server. Nothing is bound on
/// UDP, so the host answers ICMP port-unreachable — a `REJECT`.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "measurement: takes ~35 s if the refusal is not seen"]
async fn m4b_a_race_against_an_origin_with_no_h3_at_all() {
    let (tcp_sock, udp_sock) = bind_pair();
    let port = tcp_sock.local_addr().expect("local_addr").port();
    let (cert, key) = identity();
    let tcp_server = start_tcp(tcp_sock, cert.clone(), key);
    refusing_hole(udp_sock);

    println!("\nM4b — no h3 server at all: nothing bound on UDP (REJECT)");

    // First, the arm on its own — does a refusal arrive faster than a drop?
    let (took, outcome) = exchange(&quic(&cert), get(port)).await;
    println!("  QUIC alone, unbounded: {took:.3?}  {outcome}");

    for head_start in [Duration::ZERO, Duration::from_millis(300)] {
        let before = (tcp_server.accepted(), tcp_server.answered());
        let q = quic(&cert);
        let n = tcp(&cert);
        let started = Instant::now();
        let winner = race(&q, &n, port, head_start).await;
        println!(
            "  head start {head_start:>8.3?}: winner {winner:<5} in {:>9.3?} \
             | tcp accepted +{} answered +{}",
            started.elapsed(),
            tcp_server.accepted() - before.0,
            tcp_server.answered() - before.1,
        );
    }
}

// =========================================================================
// M5 — cancellation: does dropping the loser actually stop it?
// =========================================================================

/// `CancelSupport::Supported` is a duty owed on every dropped future, and
/// `hclient-h3` claims it. What is measured here is whether it holds
/// **during a connect**, which is the only moment a race ever cancels one:
/// the future is dropped mid-handshake and the black hole is watched for
/// further datagrams.
///
/// The interesting half is that HTTP/3 spawns a driver. A connect that had
/// already produced a connection would leave one running; a connect that
/// had not should leave nothing, and this says which.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "measurement"]
async fn m5_dropping_an_in_flight_quic_connect() {
    let (tcp_sock, udp_sock) = bind_pair();
    let port = tcp_sock.local_addr().expect("local_addr").port();
    let (cert, key) = identity();
    let _tcp_server = start_tcp(tcp_sock, cert.clone(), key);
    let arrivals = black_hole(udp_sock);

    println!("\nM5 — dropping an in-flight QUIC connect");
    for after in [
        Duration::from_millis(50),
        Duration::from_millis(1500),
        Duration::from_millis(3500),
    ] {
        arrivals.reset();
        let t = quic(&cert);
        {
            let fut = t.execute(get(port));
            let outcome = tokio::time::timeout(after, fut).await;
            // `timeout` drops the inner future on expiry — the drop is
            // here, at a known instant.
            assert!(outcome.is_err(), "the black hole cannot have answered");
        }
        let at_drop = arrivals.count();
        // The clock the "after the drop" figures are relative to is the
        // bound just expired, not a second reading — `after` is exact and
        // an `Instant::now()` here would add the print path to it.
        let dropped_at = Instant::now();
        tokio::time::sleep(Duration::from_secs(5)).await;
        let after_drop = arrivals.since_reset();
        let late: Vec<_> = after_drop.iter().skip(at_drop).collect();
        println!(
            "  dropped after {after:>8.3?}: {at_drop} datagram(s) sent before the drop, \
             {} in the 5 s after it{}",
            late.len(),
            if late.is_empty() {
                String::new()
            } else {
                // The size matters more than the count. A retransmitted
                // Initial is padded to 1200 bytes (RFC 9000 §14.1); a
                // short one is not a retransmission but a goodbye — an
                // Initial-level CONNECTION_CLOSE, which is what a
                // cancellation *should* look like.
                format!(
                    " — {:?}, first at +{:.3?} ({:.3?} after the drop was issued)",
                    late.iter().map(|(_, n)| *n).collect::<Vec<_>>(),
                    late[0].0,
                    late[0].0.saturating_sub(after),
                )
            }
        );
        let _ = dropped_at;
        // Whether the transport itself is still holding anything: a second
        // request through the SAME `H3` would reuse the endpoint. Dropped
        // here instead, so the next iteration is cold.
        drop(t);
    }
}

/// Whether dropping the whole transport stops it, which is the other half
/// of the same duty: a race's loser is a future, but a `Selecting` that
/// gave up on QUIC for an origin would want the sockets back too.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "measurement"]
async fn m5b_dropping_the_transport_as_well_as_the_future() {
    let (tcp_sock, udp_sock) = bind_pair();
    let port = tcp_sock.local_addr().expect("local_addr").port();
    let (cert, key) = identity();
    let _tcp_server = start_tcp(tcp_sock, cert.clone(), key);
    let arrivals = black_hole(udp_sock);

    println!("\nM5b — dropping the future AND the transport");
    arrivals.reset();
    {
        let t = quic(&cert);
        let _ = tokio::time::timeout(Duration::from_millis(1500), t.execute(get(port))).await;
    }
    let at_drop = arrivals.count();
    tokio::time::sleep(Duration::from_secs(5)).await;
    println!(
        "  {at_drop} datagram(s) before the drop, {} in the 5 s after",
        arrivals.count() - at_drop
    );
}

// --- the race, as a harness rather than as a feature ---------------------

/// Two arms, one head start, first success wins, loser dropped.
///
/// This is **not** what a race in `hclient-select` would look like — there
/// is no shared budget, no capability check and no pool interaction. It is
/// the smallest thing that produces the numbers, and it lives in a test
/// file for exactly that reason.
async fn race<N>(q: &Quic, n: &N, port: u16, head_start: Duration) -> &'static str
where
    N: Transport<Error = hclient_core::Error>,
    <N::Body as http_body::Body>::Error: std::fmt::Display,
{
    let quic_arm = std::pin::pin!(exchange(q, get(port)));
    let tcp_arm = std::pin::pin!(async {
        if !head_start.is_zero() {
            tokio::time::sleep(head_start).await;
        }
        exchange(n, get(port)).await
    });
    let mut quic_arm = quic_arm;
    let mut tcp_arm = tcp_arm;
    let mut quic_done = false;
    let mut tcp_done = false;

    loop {
        tokio::select! {
            (_, outcome) = &mut quic_arm, if !quic_done => {
                quic_done = true;
                if !outcome.starts_with("ERR") {
                    return "quic";
                }
            }
            (_, outcome) = &mut tcp_arm, if !tcp_done => {
                tcp_done = true;
                if !outcome.starts_with("ERR") {
                    return "tcp";
                }
            }
        }
        if quic_done && tcp_done {
            return "none";
        }
    }
}
