//! Which phase a connect's time is attributed to, pinned causally.
//!
//! # Not by magnitude, by ordering
//!
//! Three timing assertions in this workspace turned out to be flakes in
//! one week, so nothing here says "dns was about 300 ms". Each test makes
//! **one** phase slow by construction and then asserts an *ordering
//! between the phases the transport itself reported*: with a resolver
//! that takes 300 ms and a loopback socket that connects in microseconds,
//! `dns > tcp` is not a stopwatch reading, it is a fact about which of
//! two numbers the code put the wait into. The margin is three orders of
//! magnitude, and the assertion would survive a machine a hundred times
//! slower.
//!
//! # Why two tests and not one
//!
//! A single slow phase cannot distinguish "measured correctly" from
//! "everything is measured from the start of the connect": under a slow
//! DNS alone, a `tls` figure that wrongly began at the connect's start
//! would also be large — and would then be *larger* than `dns`, which the
//! first test catches. Under a slow handshake the ordering is the other
//! way round and `dns` must be the small one. Together the pair says the
//! phases move independently, which is the only thing that makes them
//! worth reporting.
//!
//! # The doubles
//!
//! [`SlowDns`] and [`SlowTls`] are test doubles for exactly one reason
//! each: a resolver that waits before answering, and a `TlsConnect` that
//! waits and then hands the plaintext stream back unchanged. The second
//! is not a TLS implementation and does not pretend to be — it reports
//! `http/1.1` as the negotiated ALPN and the fixture server speaks
//! ordinary HTTP/1.1, so what is exercised is this crate's timing of the
//! handshake seam, not anybody's cryptography.
#![cfg(not(target_family = "wasm"))]

use futures_core::Stream;
use http_ng_core::RequestBody;
use http_ng_core::unversioned::{Event, Hooks, Transport};
use http_ng_dns::{Resolve, ResolvedAddr};
use http_ng_native::Native;
use http_ng_rt_tokio::Tokio;
use http_ng_tls::{TlsConfigId, TlsConnect, TlsIdentity, TlsInfo, TlsRequest};
use std::io::{Read, Write};
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// The wait each double introduces. Big enough that a loopback connect
/// (microseconds) cannot be confused with it under any load this test
/// will meet, and small enough not to slow the suite down.
const SLOW: Duration = Duration::from_millis(300);

// ── what the hook wrote down ────────────────────────────────────────────

#[derive(Debug, Clone, Copy, Default)]
struct Phases {
    dns: Duration,
    tcp: Duration,
    tls: Option<Duration>,
    total: Duration,
}

#[derive(Clone, Default)]
struct FirstConnect(Arc<Mutex<Option<Phases>>>);

impl FirstConnect {
    fn get(&self) -> Phases {
        self.0
            .lock()
            .expect("recorder")
            .expect("a connect happened")
    }
}

impl Hooks for FirstConnect {
    fn on(&self, event: Event<'_>) {
        if let Event::Connected(e) = event {
            let mut slot = self.0.lock().expect("recorder");
            if slot.is_none() {
                *slot = Some(Phases {
                    dns: e.timing.dns,
                    tcp: e.timing.tcp,
                    tls: e.timing.tls,
                    total: e.timing.total,
                });
            }
        }
    }
}

// ── the doubles ─────────────────────────────────────────────────────────

/// A resolver that answers `127.0.0.1` after a wait.
///
/// The wait is `tokio::time::sleep` rather than a thread sleep so the
/// runtime stays free to make progress — a blocking resolver would be
/// measuring the executor rather than the phase.
#[derive(Clone, Copy)]
struct SlowDns(Duration);

impl Resolve for SlowDns {
    fn lookup_ipv4(
        &self,
        _name: &str,
    ) -> impl Stream<Item = Result<ResolvedAddr, http_ng_core::Error>> {
        let d = self.0;
        futures_util::stream::once(async move {
            tokio::time::sleep(d).await;
            Ok(ResolvedAddr {
                addr: IpAddr::V4(Ipv4Addr::LOCALHOST),
                ttl: None,
            })
        })
    }
    /// Empty, not slow: RFC 8305 races the families, and a v6 answer that
    /// never comes is what a v4-only host looks like. Making *both* slow
    /// would leave the scheduler's Resolution Delay in the measurement.
    fn lookup_ipv6(
        &self,
        _name: &str,
    ) -> impl Stream<Item = Result<ResolvedAddr, http_ng_core::Error>> {
        futures_util::stream::empty()
    }
}

/// A `TlsConnect` that waits and then hands the stream back as it is.
///
/// Not a TLS backend: it wraps nothing and encrypts nothing, which is
/// what lets an ordinary HTTP/1.1 fixture server sit behind it. What it
/// reproduces faithfully is the *shape* — an `async fn` between the
/// connected socket and the first byte of HTTP — which is the only thing
/// the `tls` phase is a measurement of.
#[derive(Clone, Copy)]
struct SlowTls(Duration);

impl TlsIdentity for SlowTls {
    fn config_id(&self) -> TlsConfigId {
        TlsConfigId::no_tls()
    }
}

impl TlsConnect for SlowTls {
    type Stream<S>
        = S
    where
        S: hyper::rt::Read + hyper::rt::Write + Unpin;

    async fn connect<S>(
        &self,
        stream: S,
        _req: TlsRequest<'_>,
    ) -> Result<(Self::Stream<S>, TlsInfo), http_ng_core::Error>
    where
        S: hyper::rt::Read + hyper::rt::Write + Unpin,
    {
        tokio::time::sleep(self.0).await;
        Ok((
            stream,
            TlsInfo {
                alpn: Some(b"http/1.1".to_vec()),
                ..TlsInfo::default()
            },
        ))
    }

    /// So that `Native` believes the ALPN above — without it the
    /// transport would never have offered a protocol list and the answer
    /// would be ignored. It changes nothing about the timing; it is what
    /// makes this double usable at all.
    fn reports_alpn(&self) -> bool {
        true
    }
}

// ── the fixture ─────────────────────────────────────────────────────────

fn server() -> SocketAddr {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = listener.local_addr().expect("local_addr");
    std::thread::spawn(move || {
        for sock in listener.incoming() {
            let Ok(mut sock) = sock else { continue };
            std::thread::spawn(move || {
                let mut buf: Vec<u8> = Vec::new();
                let mut chunk = [0u8; 1024];
                loop {
                    let head_end = loop {
                        if let Some(i) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                            break i + 4;
                        }
                        match sock.read(&mut chunk) {
                            Ok(0) | Err(_) => return,
                            Ok(n) => buf.extend_from_slice(&chunk[..n]),
                        }
                    };
                    buf.drain(..head_end);
                    if sock
                        .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok")
                        .is_err()
                    {
                        return;
                    }
                }
            });
        }
    });
    addr
}

async fn get(t: &impl Transport, uri: String) {
    let req = http::Request::builder()
        .uri(uri)
        .body(RequestBody::Empty)
        .unwrap();
    let resp = t.execute(req).await.map_err(|_| ()).expect("request");
    assert_eq!(resp.status(), 200);
    let _ = http_body_util::BodyExt::collect(resp.into_body())
        .await
        .map_err(|_| ())
        .expect("body");
}

// ── the two directions ──────────────────────────────────────────────────

/// A slow name lookup lands in `dns` and in nothing else.
///
/// The name is a name rather than a literal, which is what makes the
/// resolver run at all; the address it answers with is loopback, so the
/// TCP attempt is microseconds and the handshake does not exist.
#[tokio::test]
async fn a_slow_resolver_shows_up_as_dns_and_not_as_tcp() {
    let addr = server();
    let rec = FirstConnect::default();
    let t = Native::new(Tokio, http_ng_tls::NoTls, SlowDns(SLOW)).hooks(rec.clone());

    get(&t, format!("http://slow.example:{}/", addr.port())).await;

    let p = rec.get();
    assert!(
        p.dns > p.tcp,
        "the wait was the resolver's: dns {:?} must exceed tcp {:?}",
        p.dns,
        p.tcp
    );
    assert_eq!(p.tls, None, "`http://` has no handshake");
    assert!(
        p.dns <= p.total,
        "and it must fit inside the connect: dns {:?}, total {:?}",
        p.dns,
        p.total
    );
}

/// A slow handshake lands in `tls`, and — the half that matters — leaves
/// `dns` small.
///
/// This is the test a `tls` measured from the start of the connect would
/// still pass, and a `dns` measured to the *end* of the connect would
/// fail: the resolver here is `IpLiteralOnly`, so there is genuinely no
/// name to look up, and any `dns` figure that swallowed the handshake
/// would be caught by the comparison below.
#[tokio::test]
async fn a_slow_handshake_shows_up_as_tls_and_leaves_dns_small() {
    let addr = server();
    let rec = FirstConnect::default();
    let t = Native::new(Tokio, SlowTls(SLOW), http_ng_dns::IpLiteralOnly).hooks(rec.clone());

    get(&t, format!("https://127.0.0.1:{}/", addr.port())).await;

    let p = rec.get();
    let tls = p.tls.expect("an `https://` connection has a handshake");
    assert!(
        tls > p.dns,
        "the wait was the handshake's: tls {tls:?} must exceed dns {:?}",
        p.dns
    );
    assert!(
        tls > p.tcp,
        "and it must exceed the loopback connect: tls {tls:?}, tcp {:?}",
        p.tcp
    );
    assert!(
        tls <= p.total,
        "and fit inside the connect: tls {tls:?}, total {:?}",
        p.total
    );
}

/// The invariant both runs share, stated once: three disjoint intervals
/// inside one. A phase stamped from an instant earlier than its own start
/// breaks this without any test having to know how long anything took.
#[tokio::test]
async fn the_phases_never_add_up_to_more_than_the_connect() {
    let addr = server();

    let slow_dns = FirstConnect::default();
    let t = Native::new(Tokio, http_ng_tls::NoTls, SlowDns(SLOW)).hooks(slow_dns.clone());
    get(&t, format!("http://slow.example:{}/", addr.port())).await;

    let slow_tls = FirstConnect::default();
    let t = Native::new(Tokio, SlowTls(SLOW), http_ng_dns::IpLiteralOnly).hooks(slow_tls.clone());
    get(&t, format!("https://127.0.0.1:{}/", addr.port())).await;

    for p in [slow_dns.get(), slow_tls.get()] {
        let sum = p.dns + p.tcp + p.tls.unwrap_or(Duration::ZERO);
        assert!(
            sum <= p.total,
            "dns {:?} + tcp {:?} + tls {:?} = {sum:?} must fit inside {:?}",
            p.dns,
            p.tcp,
            p.tls,
            p.total
        );
    }
}
