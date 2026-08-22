//! What Nagle's algorithm costs a `Native` connection, measured from both
//! sides of the wire.
//!
//! **A measurement, not a feature.** Everything here is `#[ignore]`d and
//! prints rather than asserts, which is the rule this workspace already
//! follows for timing: three timing assertions here turned out to be
//! flakes and one was hiding a real defect, so a harness whose output is
//! *numbers* may be timing-based where a harness whose output is a
//! *verdict* may not. What the numbers below decided is asserted
//! elsewhere — `tests/tcp_opts.rs`, which contains no clock at all.
//!
//! ```text
//! cargo nextest run -p hclient-native --test nagle_cost --run-ignored all \
//!     --no-capture -j1
//! ```
//!
//! `-j1` because every arm binds a port and times things; `--no-capture`
//! because the output is the point.
//!
//! # Why the server keeps a log of its reads
//!
//! An earlier measurement found 41 ms on the head of every `Native` TLS
//! exchange and named Nagle from the outside: unchanged by an
//! IP literal, so not resolution; unchanged between debug and release, so
//! not crypto. That is an argument by elimination, and it cannot tell
//! *which direction* waited — a server that took 40 ms to answer and a
//! request that took 40 ms to arrive look identical from the client.
//!
//! So the observer is on the wire, at the server. [`Recorded`] wraps the
//! accepted `TcpStream` and stamps every read that returns bytes, before
//! TLS sees them, against a clock the client shares. A Nagle stall on the
//! request has one shape and nothing else does: the handshake flights
//! arrive within a millisecond of each other and then **the next read is
//! ~40 ms later** — the client's second flight sitting in its own kernel
//! waiting for an ACK the peer's delayed-ACK timer has not sent yet.
//!
//! # The five arms, and what each one rules out
//!
//! - [`shipped_default`] — `Native::new` and nothing else, which is what a
//!   caller actually gets. The arm the fix is judged by.
//! - [`nagle_on`] — the same exchange with `TcpOpts::default()` stated
//!   explicitly, which turns the transport's own `nodelay` back off (see
//!   `Native::tcp_opts` — a caller's set is the whole set). The number
//!   under investigation, and what `shipped_default` used to be.
//! - [`nodelay_on`] — the same exchange with `TcpOpts { nodelay: true }`.
//!   The difference between these two is the whole finding.
//! - [`server_side_nodelay`] — `TCP_NODELAY` set on the **server's**
//!   socket, the client left at the default. `TCP_NODELAY` and the
//!   delayed-ACK timer are different mechanisms on different hosts, so if
//!   the stall is the client's held write this arm must still stall. It is
//!   the arm that says which side is holding.
//! - [`plaintext_no_tls`] — `http://` over `NoTls`, so the client writes
//!   the request head as its *first* write on the connection. Nagle never
//!   holds a first write (there is nothing unacknowledged), so a stall
//!   here would mean the diagnosis is wrong.
#![cfg(not(target_family = "wasm"))]

use hclient_core::RequestBody;
use hclient_core::unversioned::Transport;
use hclient_dns::IpLiteralOnly;
use hclient_native::Native;
use hclient_rt::TcpOpts;
use hclient_rt_tokio::Tokio;
use hclient_tls::NoTls;
use hclient_tls_rustls::Rustls;
use http_body_util::BodyExt;
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};
use std::time::{Duration, Instant};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, ReadBuf};

/// How many exchanges each arm makes. Each one is a fresh `Native` and
/// therefore a fresh connection: the number under investigation is a cold
/// connection's, and a pooled second request never handshakes at all.
const SAMPLES: usize = 10;

// --- the wire log --------------------------------------------------------

/// One read that returned bytes: which connection, when (on the shared
/// clock), and how many.
#[derive(Debug, Clone, Copy)]
struct Flight {
    conn: usize,
    at: Duration,
    len: usize,
}

/// The server's view of the wire, shared with the test.
#[derive(Debug, Clone)]
struct Wire {
    log: Arc<Mutex<Vec<Flight>>>,
    /// The origin every timestamp in this file is measured from — the
    /// client's `execute` marks too, so the two sides are on one timeline.
    origin: Instant,
    next_conn: Arc<AtomicUsize>,
}

impl Wire {
    fn new() -> Self {
        Self {
            log: Arc::new(Mutex::new(Vec::new())),
            origin: Instant::now(),
            next_conn: Arc::new(AtomicUsize::new(0)),
        }
    }

    fn flights_of(&self, conn: usize) -> Vec<Flight> {
        self.log
            .lock()
            .expect("no test here panics while holding this")
            .iter()
            .filter(|f| f.conn == conn)
            .copied()
            .collect()
    }
}

/// A `TcpStream` that stamps every read returning bytes, before anything
/// above it (TLS, hyper) sees them.
#[derive(Debug)]
struct Recorded {
    inner: tokio::net::TcpStream,
    wire: Wire,
    conn: usize,
}

impl AsyncRead for Recorded {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        let me = self.get_mut();
        let before = buf.filled().len();
        let r = Pin::new(&mut me.inner).poll_read(cx, buf);
        if matches!(r, Poll::Ready(Ok(()))) {
            let n = buf.filled().len() - before;
            if n > 0 {
                me.wire
                    .log
                    .lock()
                    .expect("no test here panics while holding this")
                    .push(Flight {
                        conn: me.conn,
                        at: me.wire.origin.elapsed(),
                        len: n,
                    });
            }
        }
        r
    }
}

impl AsyncWrite for Recorded {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        Pin::new(&mut self.get_mut().inner).poll_write(cx, buf)
    }
    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.get_mut().inner).poll_flush(cx)
    }
    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.get_mut().inner).poll_shutdown(cx)
    }
}

// --- the server ----------------------------------------------------------

/// One self-signed certificate covering the loopback literal every arm
/// dials — an IP SAN, because that is what `https://127.0.0.1:port/`
/// checks against.
fn identity() -> (CertificateDer<'static>, PrivateKeyDer<'static>) {
    let cert = rcgen::generate_simple_self_signed(vec!["127.0.0.1".into()])
        .expect("rcgen can always make a self-signed cert");
    (
        CertificateDer::from(cert.cert.der().to_vec()),
        PrivateKeyDer::try_from(cert.signing_key.serialize_der()).expect("pkcs8 from rcgen"),
    )
}

/// Read to the end of the request head, answer `200 ok`, close cleanly.
///
/// The response is written and flushed in one go, so nothing in the answer
/// can contribute a Nagle stall of its own — the arms below are about the
/// request.
async fn serve<S: AsyncRead + AsyncWrite + Unpin>(mut s: S) {
    let mut head = Vec::new();
    let mut byte = [0u8; 1];
    while s.read_exact(&mut byte).await.is_ok() {
        head.push(byte[0]);
        if head.ends_with(b"\r\n\r\n") {
            break;
        }
    }
    let _ = s
        .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok")
        .await;
    let _ = s.flush().await;
    // `close_notify` rather than a bare drop: rustls reports a truncated
    // stream as an error, and this crate's `truncation_detection.rs` is
    // why.
    let _ = s.shutdown().await;
}

/// A loopback server that records its reads, optionally behind TLS,
/// optionally with `TCP_NODELAY` on its own accepted sockets.
fn spawn(tls: bool, server_nodelay: bool) -> (SocketAddr, Wire, Option<CertificateDer<'static>>) {
    let (cert_der, key_der) = identity();
    let acceptor = tls.then(|| {
        let mut cfg = rustls::ServerConfig::builder()
            .with_no_client_auth()
            .with_single_cert(vec![cert_der.clone()], key_der)
            .expect("the cert and key were made together");
        cfg.alpn_protocols = vec![b"http/1.1".to_vec()];
        tokio_rustls::TlsAcceptor::from(Arc::new(cfg))
    });

    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("v4 loopback");
    let addr = listener.local_addr().expect("local_addr");
    listener.set_nonblocking(true).expect("nonblocking");

    let wire = Wire::new();
    let theirs = wire.clone();
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
                if server_nodelay {
                    tcp.set_nodelay(true)
                        .expect("TCP_NODELAY on an accepted socket");
                }
                let conn = theirs.next_conn.fetch_add(1, Ordering::SeqCst);
                let rec = Recorded {
                    inner: tcp,
                    wire: theirs.clone(),
                    conn,
                };
                let acceptor = acceptor.clone();
                tokio::spawn(async move {
                    match acceptor {
                        Some(a) => {
                            let Ok(s) = a.accept(rec).await else { return };
                            serve(s).await;
                        }
                        None => serve(rec).await,
                    }
                });
            }
        });
    });

    (addr, wire, tls.then_some(cert_der))
}

/// A `Rustls` trusting exactly this certificate, so the handshake is about
/// the wire rather than about the machine's trust store.
fn client_tls(cert: &CertificateDer<'static>) -> Rustls {
    let mut roots = rustls::RootCertStore::empty();
    roots.add(cert.clone()).expect("a DER certificate");
    let cfg = rustls::ClientConfig::builder()
        .with_root_certificates(roots)
        .with_no_client_auth();
    Rustls::from_config(Arc::new(cfg))
}

// --- one exchange --------------------------------------------------------

/// What one request cost, on the shared clock.
#[derive(Debug, Clone, Copy)]
struct Sample {
    /// `execute` called.
    start: Duration,
    /// `execute` returned a head.
    head: Duration,
    /// the body ended.
    end: Duration,
}

async fn one<T>(t: &T, uri: &str, origin: Instant) -> Sample
where
    T: Transport,
    <T::Body as http_body::Body>::Error: std::fmt::Debug,
{
    let req = http::Request::builder()
        .uri(uri)
        .body(RequestBody::Empty)
        .expect("a well-formed request");
    let start = origin.elapsed();
    let resp = t
        .execute(req)
        .await
        .unwrap_or_else(|e| panic!("{uri}: the exchange had to complete, got {e}"));
    let head = origin.elapsed();
    assert_eq!(resp.status(), 200);
    let body = resp.into_body().collect().await.expect("a two-byte body");
    let end = origin.elapsed();
    assert_eq!(body.to_bytes().as_ref(), b"ok");
    Sample { start, head, end }
}

// --- reporting -----------------------------------------------------------

fn ms(d: Duration) -> f64 {
    d.as_secs_f64() * 1e3
}

fn median(mut v: Vec<f64>) -> f64 {
    v.sort_by(f64::total_cmp);
    v[v.len() / 2]
}

/// The head cost of every sample, plus the wire log of the first one.
fn report(arm: &str, samples: &[Sample], wire: &Wire) {
    let heads: Vec<f64> = samples.iter().map(|s| ms(s.head - s.start)).collect();
    let bodies: Vec<f64> = samples.iter().map(|s| ms(s.end - s.head)).collect();
    let lo = heads.iter().copied().fold(f64::INFINITY, f64::min);
    let hi = heads.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    println!("\n== {arm} ==");
    println!(
        "  execute -> head: median {:.3} ms  (min {:.3}, max {:.3}, n={})",
        median(heads.clone()),
        lo,
        hi,
        heads.len()
    );
    println!("  head -> end of body: median {:.4} ms", median(bodies));
    println!("  every sample's head, ms: {heads:?}");

    // The wire, for one connection: when each read that returned bytes
    // returned, relative to the moment `execute` was called on that same
    // sample. The gap between the last two rows is the finding.
    let s = samples[samples.len() / 2];
    let conn = samples.len() / 2;
    println!("  the wire, for sample {conn} (server-side reads, from its own execute):");
    let mut prev = s.start;
    for f in wire.flights_of(conn) {
        println!(
            "    +{:8.3} ms  {:5} bytes   (+{:.3} ms since the previous read)",
            ms(f.at - s.start),
            f.len,
            ms(f.at - prev)
        );
        prev = f.at;
    }
    println!(
        "    +{:8.3} ms  response head at the client",
        ms(s.head - s.start)
    );
}

// --- the arms ------------------------------------------------------------

/// n cold exchanges against one server, each on its own connection.
///
/// `opts` is `None` for the arm that measures what a caller actually gets:
/// `Native::new` and nothing else. Every other arm states its options,
/// which **replaces** the set — including the `nodelay` `new` asks for.
async fn tls_arm(opts: Option<TcpOpts>, server_nodelay: bool) -> (Vec<Sample>, Wire) {
    let (addr, wire, cert) = spawn(true, server_nodelay);
    let cert = cert.expect("a TLS arm has a certificate");
    let uri = format!("https://{addr}/");
    let mut samples = Vec::new();
    for _ in 0..SAMPLES {
        // A fresh transport per sample: `Native::new` pools, and a pooled
        // second request never handshakes, which is not the exchange under
        // investigation.
        let t = Native::new(Tokio, client_tls(&cert), IpLiteralOnly);
        let t = match &opts {
            Some(o) => t.tcp_opts(o.clone()).expect("Tokio applies every option"),
            None => t,
        };
        samples.push(one(&t, &uri, wire.origin).await);
    }
    (samples, wire)
}

/// **What a caller gets today**: `Native::new`, no `tcp_opts` call. The
/// arm the fix is judged by, and the one whose number the acceptance
/// document quotes.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "a measurement, not a verdict — see the module doc"]
async fn shipped_default() {
    let (samples, wire) = tls_arm(None, false).await;
    report("TLS 1.3, Native::new and nothing else", &samples, &wire);
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "a measurement, not a verdict — see the module doc"]
async fn nagle_on() {
    let (samples, wire) = tls_arm(Some(TcpOpts::default()), false).await;
    report("TLS 1.3, client TcpOpts::default()", &samples, &wire);
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "a measurement, not a verdict — see the module doc"]
async fn nodelay_on() {
    let opts = TcpOpts {
        nodelay: true,
        ..TcpOpts::default()
    };
    let (samples, wire) = tls_arm(Some(opts), false).await;
    report("TLS 1.3, client TcpOpts { nodelay: true }", &samples, &wire);
}

/// The arm that says which side is holding: `TCP_NODELAY` on the server,
/// the client left at the default.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "a measurement, not a verdict — see the module doc"]
async fn server_side_nodelay() {
    let (samples, wire) = tls_arm(Some(TcpOpts::default()), true).await;
    report(
        "TLS 1.3, client default, TCP_NODELAY on the SERVER",
        &samples,
        &wire,
    );
}

/// `http://` over `NoTls`: the request head is the client's first write on
/// the connection, and Nagle never holds a first write.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "a measurement, not a verdict — see the module doc"]
async fn plaintext_no_tls() {
    let (addr, wire, _) = spawn(false, false);
    let uri = format!("http://{addr}/");
    let mut samples = Vec::new();
    for _ in 0..SAMPLES {
        let t = Native::new(Tokio, NoTls, IpLiteralOnly)
            .tcp_opts(TcpOpts::default())
            .expect("Tokio applies every option");
        samples.push(one(&t, &uri, wire.origin).await);
    }
    report(
        "plaintext http://, client TcpOpts::default()",
        &samples,
        &wire,
    );
}
