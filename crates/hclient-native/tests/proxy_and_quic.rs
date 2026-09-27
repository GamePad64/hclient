//! A proxy or a Unix socket decides **where a request goes**, and the QUIC
//! arm is not allowed to decide otherwise.
//!
//! Both carry a byte stream. QUIC is datagrams, so a request routed to the
//! QUIC arm leaves over UDP, from this host, to the origin — past the proxy
//! the machine's owner configured, or onto the network where the caller
//! asked for a local socket. And the fast tier's HTTPS-record lookup names
//! the origin to the local resolver, which is the leak a proxy user is
//! often there to avoid.
//!
//! Every test runs both servers of a [`Pair`], so a request reaching one of
//! them is a choice. The proxy here tunnels to the pair's TCP half whatever
//! it is asked for and counts the tunnels, so *"went through the proxy"* is
//! read off the proxy rather than inferred from the answer.
#![cfg(all(
    feature = "http3",
    feature = "proxy",
    unix,
    not(target_family = "wasm")
))]

mod fakedns;
mod servers;

use fakedns::{FakeDns, service_record};
use hclient_core::body::RequestBody;
use hclient_core::error::ErrorKind;
use hclient_core::req::RequireVersion;
use hclient_core::transport::Transport;
use hclient_native::proxy::{HttpConnect, Proxy};
use hclient_native::{H3, Native};
use hclient_rt_tokio::TokioHandle;
use http_body_util::BodyExt;
use servers::{ORIGIN, Pair};
use std::io::Write as _;
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

/// Never an assertion — it turns a mutation that hangs into a red test.
const BOUND: Duration = Duration::from_secs(10);

/// The proxy's name. `FakeDns` answers every name with loopback, so this
/// reaches the proxy below; it is a name rather than a literal so that the
/// resolver's log can tell the proxy's lookups from the origin's.
const PROXY: &str = "proxy.test";

/// An HTTP `CONNECT` proxy that tunnels to `upstream`, whatever authority
/// it is asked for, and counts the tunnels it opened.
fn tunnelling_proxy(upstream: SocketAddr) -> (u16, Arc<AtomicUsize>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().expect("addr").port();
    let tunnels = Arc::new(AtomicUsize::new(0));
    let counted = tunnels.clone();
    std::thread::spawn(move || {
        for conn in listener.incoming() {
            let Ok(mut client) = conn else { break };
            // The head is small and arrives in one read on loopback; the
            // client sends nothing after it until the `200`.
            let mut head = Vec::new();
            let mut byte = [0u8; 1];
            while !head.ends_with(b"\r\n\r\n") {
                match std::io::Read::read(&mut client, &mut byte) {
                    Ok(1) => head.push(byte[0]),
                    _ => break,
                }
            }
            let Ok(upstream) = TcpStream::connect(upstream) else {
                continue;
            };
            if client
                .write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
                .is_err()
            {
                continue;
            }
            counted.fetch_add(1, Ordering::SeqCst);
            splice(client, upstream);
        }
    });
    (port, tunnels)
}

/// Bytes both ways between two sockets, each direction on its own thread.
fn splice<A, B>(a: A, b: B)
where
    A: std::io::Read + std::io::Write + TryClone + Send + 'static,
    B: std::io::Read + std::io::Write + TryClone + Send + 'static,
{
    let (mut a2, mut b2) = (a.try_clone_(), b.try_clone_());
    let (mut a, mut b) = (a, b);
    std::thread::spawn(move || {
        let _ = std::io::copy(&mut a2, &mut b2);
    });
    std::thread::spawn(move || {
        let _ = std::io::copy(&mut b, &mut a);
    });
}

trait TryClone: Sized {
    fn try_clone_(&self) -> Self;
}
impl TryClone for TcpStream {
    fn try_clone_(&self) -> Self {
        self.try_clone().expect("clone")
    }
}
impl TryClone for std::os::unix::net::UnixStream {
    fn try_clone_(&self) -> Self {
        self.try_clone().expect("clone")
    }
}

type Proxied =
    Native<TokioHandle, hclient_tls_rustls::Rustls, FakeDns, hclient_core::hooks::NoHooks>;

fn quic(pair: &Pair, dns: &FakeDns) -> H3<TokioHandle, hclient_tls_rustls::Rustls, FakeDns> {
    let rt = TokioHandle::current().expect("inside #[tokio::test]");
    H3::new(rt, servers::client_tls(&pair.cert_der), dns.clone()).expect("H3::new does no I/O")
}

fn tcp(pair: &Pair, dns: &FakeDns) -> Native<TokioHandle, hclient_tls_rustls::Rustls, FakeDns> {
    let rt = TokioHandle::current().expect("inside #[tokio::test]");
    Native::new(rt, servers::client_tls(&pair.cert_der), dns.clone())
}

/// The proxy, then the QUIC arm — the order a caller building by hand
/// would likely write.
fn proxy_then_h3(pair: &Pair, dns: &FakeDns, proxy: Proxy<HttpConnect>) -> Proxied {
    tcp(pair, dns)
        .proxy(proxy)
        .http3(quic(pair, dns))
        .expect("the two paths agree")
}

/// The QUIC arm, then the proxy — the order `hclient::Client::new` builds
/// in, since the proxies come from the machine's settings afterwards.
fn h3_then_proxy(pair: &Pair, dns: &FakeDns, proxy: Proxy<HttpConnect>) -> Proxied {
    tcp(pair, dns)
        .http3(quic(pair, dns))
        .expect("the two paths agree")
        .proxy(proxy)
}

fn uri(pair: &Pair) -> String {
    format!("https://{ORIGIN}:{}/hello", pair.port)
}

fn request(pair: &Pair) -> http::Request<RequestBody> {
    http::Request::builder()
        .uri(uri(pair))
        .body(RequestBody::Empty)
        .expect("a well-formed request")
}

/// One request, and which server answered it.
async fn send<T: Transport<Error = hclient_core::error::Error>>(t: &T, pair: &Pair) -> String
where
    T::Body: http_body::Body<Data = bytes::Bytes>,
    <T::Body as http_body::Body>::Error: std::fmt::Debug,
{
    let resp = tokio::time::timeout(BOUND, t.execute(request(pair)))
        .await
        .expect("the request finished inside the bound")
        .expect("a server answered");
    assert_eq!(resp.status(), 200);
    let body = resp
        .into_body()
        .collect()
        .await
        .expect("a complete body")
        .to_bytes();
    String::from_utf8(body.to_vec()).expect("utf-8")
}

/// The origin's name was never handed to the local resolver for an HTTPS
/// record — the proxy's may have been, and is not the question.
fn origin_record_never_asked(dns: &FakeDns) {
    assert!(
        dns.svcb_names().iter().all(|n| !n.contains(ORIGIN)),
        "the origin's HTTPS record was looked up locally: {:?}",
        dns.svcb_names()
    );
}

// --- the slow tier: an advertisement heard through the proxy ------------

/// **The defect.** Request 1 goes through the proxy and its answer carries
/// `Alt-Svc: h3`. Request 2 must go through the proxy too: the proxy is
/// where this machine's traffic is meant to go, and an origin's
/// advertisement is no instruction to leave it.
async fn an_advertisement_does_not_move_a_proxied_request(
    build: fn(&Pair, &FakeDns, Proxy<HttpConnect>) -> Proxied,
) {
    let pair = servers::start();
    pair.set_alt_svc(Some(&pair.h3_here("; ma=86400")));
    let (port, tunnels) = tunnelling_proxy(pair.addr());
    let dns = FakeDns::new();
    let t = build(&pair, &dns, Proxy::new(HttpConnect::new(), PROXY, port));

    assert_eq!(send(&t, &pair).await, "h1");
    assert_eq!(
        send(&t, &pair).await,
        "h1",
        "the second request left the proxy"
    );
    assert_eq!(
        tunnels.load(Ordering::SeqCst),
        2,
        "both requests went through the proxy"
    );
    assert_eq!(
        pair.quic_attempted(),
        0,
        "a QUIC handshake reached the origin"
    );
    origin_record_never_asked(&dns);
}

#[tokio::test(flavor = "multi_thread")]
async fn an_advertisement_does_not_move_a_proxied_request_proxy_first() {
    an_advertisement_does_not_move_a_proxied_request(proxy_then_h3).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn an_advertisement_does_not_move_a_proxied_request_quic_arm_first() {
    an_advertisement_does_not_move_a_proxied_request(h3_then_proxy).await;
}

// --- the fast tier: a record offering h3 --------------------------------

/// A record offering `h3` would choose QUIC on the first request of a
/// direct transport (`tests/alt_svc.rs`). Through a proxy it is not even
/// asked for: the lookup itself would name the origin to the local
/// resolver.
#[tokio::test(flavor = "multi_thread")]
async fn a_proxied_request_asks_for_no_record_and_takes_the_proxy() {
    let pair = servers::start();
    let (port, tunnels) = tunnelling_proxy(pair.addr());
    let dns = FakeDns::with_records(vec![service_record(1, &[b"h3"])]);
    let t = h3_then_proxy(&pair, &dns, Proxy::new(HttpConnect::new(), PROXY, port));

    assert_eq!(send(&t, &pair).await, "h1");
    assert_eq!(tunnels.load(Ordering::SeqCst), 1);
    assert_eq!(pair.quic_attempted(), 0);
    origin_record_never_asked(&dns);
}

/// **The control**, and without it every test above would pass for a
/// transport that simply stopped choosing QUIC once a proxy was installed.
/// An origin the proxy bypasses is a direct request, and a direct request
/// is the QUIC arm's to take.
#[tokio::test(flavor = "multi_thread")]
async fn a_bypassed_origin_is_direct_and_may_still_take_quic() {
    let pair = servers::start();
    let (port, tunnels) = tunnelling_proxy(pair.addr());
    let dns = FakeDns::with_records(vec![service_record(1, &[b"h3"])]);
    let t = h3_then_proxy(
        &pair,
        &dns,
        Proxy::new(HttpConnect::new(), PROXY, port).bypass([ORIGIN]),
    );

    assert_eq!(send(&t, &pair).await, "h3");
    assert_eq!(tunnels.load(Ordering::SeqCst), 0);
}

// --- a demand -----------------------------------------------------------

/// `RequireVersion(HTTP_3)` through a proxy is refused, and refused before
/// anything is dialled: the caller asked for something this path cannot
/// carry, and sending it direct instead would be answering a different
/// question than the one the machine's owner configured.
#[tokio::test(flavor = "multi_thread")]
async fn demanding_http_3_through_a_proxy_is_refused_by_name() {
    let pair = servers::start();
    let (port, tunnels) = tunnelling_proxy(pair.addr());
    let dns = FakeDns::new();
    let t = h3_then_proxy(&pair, &dns, Proxy::new(HttpConnect::new(), PROXY, port));

    let mut req = request(&pair);
    req.extensions_mut()
        .insert(RequireVersion(http::Version::HTTP_3));
    let err = tokio::time::timeout(BOUND, t.execute(req))
        .await
        .expect("inside the bound")
        .expect_err("HTTP/3 cannot go through a proxy");
    assert_eq!(*err.kind(), ErrorKind::Unsupported);
    let refusal = std::error::Error::source(&err)
        .and_then(|s| s.downcast_ref::<hclient_native::error::NoDatagramPath>())
        .unwrap_or_else(|| panic!("a typed refusal: {err}"));
    assert_eq!(&*refusal.via, format!("{PROXY}:{port}"), "names the proxy");
    assert_eq!(tunnels.load(Ordering::SeqCst), 0);
    assert_eq!(pair.quic_attempted(), 0);
}

// --- a Unix socket is the same question ---------------------------------

/// A Unix socket answers *where does this connection go* exactly as a proxy
/// does, and a record offering `h3` must not send the request onto the
/// network instead. The socket here forwards to the pair's TCP half, so
/// the answer is `h1` only if the request really took it.
#[tokio::test(flavor = "multi_thread")]
async fn a_unix_socket_is_not_left_for_quic() {
    let pair = servers::start();
    let dir = std::env::temp_dir().join(format!("hc-pq-{}-{:p}", std::process::id(), &pair));
    std::fs::create_dir_all(&dir).expect("temp dir");
    let path = dir.join("s");
    let listener = std::os::unix::net::UnixListener::bind(&path).expect("bind");
    let upstream = pair.addr();
    let forwarded = Arc::new(AtomicUsize::new(0));
    let counted = forwarded.clone();
    std::thread::spawn(move || {
        for conn in listener.incoming() {
            let Ok(local) = conn else { break };
            let Ok(remote) = TcpStream::connect(upstream) else {
                continue;
            };
            counted.fetch_add(1, Ordering::SeqCst);
            splice(local, remote);
        }
    });

    let dns = FakeDns::with_records(vec![service_record(1, &[b"h3"])]);
    let t = tcp(&pair, &dns)
        .http3(quic(&pair, &dns))
        .expect("the two paths agree")
        .unix_socket(&path)
        .expect("no proxy is configured");

    assert_eq!(send(&t, &pair).await, "h1");
    assert_eq!(forwarded.load(Ordering::SeqCst), 1);
    assert_eq!(pair.quic_attempted(), 0);
    origin_record_never_asked(&dns);
    let _ = std::fs::remove_dir_all(&dir);
}

// --- what the transport reports -----------------------------------------

/// `capabilities().proxy` is whether a proxy is configured, whichever order
/// the transport was built in. It read `false` with the proxy installed
/// first — the QUIC arm's `false` taken as a disagreement — and `true` the
/// other way round, which was the one of the two that lied while the arm
/// could leave the proxy.
#[tokio::test(flavor = "multi_thread")]
async fn the_proxy_capability_does_not_depend_on_the_order_of_construction() {
    let pair = servers::start();
    let dns = FakeDns::new();
    let p = || Proxy::new(HttpConnect::new(), PROXY, 9);
    assert!(proxy_then_h3(&pair, &dns, p()).capabilities().proxy);
    assert!(h3_then_proxy(&pair, &dns, p()).capabilities().proxy);
}
