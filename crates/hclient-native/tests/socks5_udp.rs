//! HTTP/3 through a real SOCKS5 proxy that relays UDP, and every way that
//! proxy can say no.
//!
//! The transport is `Native` with its built-in proxy rules and nothing
//! else — no filter of the test's own — so what carries a request is
//! `Socks5::with_udp`'s association: a control connection, `CMD=0x03`, and
//! datagrams wrapped in RFC 1928's §7 header. The origin is a real HTTP/3
//! server beside a real HTTP/1.1 one on the same port, so the version of a
//! response says which stack carried it, and the relay is a real one, so a
//! response that arrived crossed it.
#![cfg(all(feature = "http3", feature = "proxy", not(target_family = "wasm")))]

#[path = "egress_fixtures.rs"]
mod fixtures;
#[path = "servers.rs"]
mod servers;
#[path = "socks5_relay.rs"]
mod socks5_relay;

use fixtures::NameLog;
use hclient_core::body::RequestBody;
use hclient_core::error::Error;
use hclient_core::req::RequireVersion;
use hclient_core::transport::Transport as _;
use hclient_native::proxy::{Proxy, Socks5};
use hclient_native::{H3, Native};
use hclient_rt_tokio::TokioHandle;
use socks5_relay::{Behaviour, Socks5Udp};

/// Never an assertion — it turns a mutation that hangs into a red test.
const BOUND: std::time::Duration = std::time::Duration::from_secs(20);

type Proxied = Native<TokioHandle, hclient_tls_rustls::Rustls, NameLog>;

/// Both stacks over one resolver, and every request through `socks5` at
/// `relay`.
fn native(pair: &servers::Pair, dns: &NameLog, socks5: Socks5, relay: &Socks5Udp) -> Proxied {
    let rt = TokioHandle::current().expect("inside #[tokio::test]");
    let quic = H3::new(rt.clone(), servers::client_tls(&pair.cert_der), dns.clone())
        .expect("H3::new does no I/O");
    Native::new(rt, servers::client_tls(&pair.cert_der), dns.clone())
        .http3(quic)
        .expect("the two stacks agree")
        .proxy(Proxy::new(socks5, "127.0.0.1", relay.addr().port()))
}

fn request(authority: &str, demand_h3: bool) -> http::Request<RequestBody> {
    let mut req = http::Request::get(format!("https://{authority}/"))
        .body(RequestBody::Empty)
        .unwrap();
    if demand_h3 {
        req.extensions_mut()
            .insert(RequireVersion(http::Version::HTTP_3));
    }
    req
}

/// One request; the response's version, or the error.
async fn send(t: &Proxied, authority: &str, demand_h3: bool) -> Result<http::Version, Error> {
    let resp = tokio::time::timeout(BOUND, t.execute(request(authority, demand_h3)))
        .await
        .expect("the request finished inside the bound")?;
    assert_eq!(resp.status(), 200);
    Ok(resp.version())
}

fn literal(pair: &servers::Pair) -> String {
    format!("127.0.0.1:{}", pair.port)
}

fn named(pair: &servers::Pair) -> String {
    format!("{}:{}", servers::ORIGIN, pair.port)
}

/// Request one goes over the proxy's stream and carries `Alt-Svc: h3`
/// back, so the next request has a signal to act on.
async fn seed_alt_svc_h3(t: &Proxied, pair: &servers::Pair, authority: &str) {
    pair.set_alt_svc(Some(&pair.h3_here("; ma=86400")));
    let v = send(t, authority, false).await.expect("the seed request");
    assert_eq!(v, http::Version::HTTP_11, "no signal yet, so the stream");
}

#[tokio::test(flavor = "multi_thread")]
async fn an_h3_request_goes_through_the_socks5_relay_and_the_origin_sees_only_the_relay() {
    let pair = servers::start();
    let relay = Socks5Udp::start(Behaviour::Relay).await;
    let t = native(&pair, &NameLog::default(), Socks5::new().with_udp(), &relay);
    let v = send(&t, &literal(&pair), true)
        .await
        .expect("through the relay");
    assert_eq!(v, http::Version::HTTP_3);
    assert_eq!(relay.associations(), 1);
    assert!(relay.datagrams_relayed() > 0);
    assert_eq!(pair.quic_peers(), [relay.outbound_addr()]);
    assert_eq!(pair.tcp_accepted(), 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn an_h3_request_names_its_origin_to_the_relay_and_never_to_the_resolver() {
    let pair = servers::start();
    let relay = Socks5Udp::start(Behaviour::Relay).await;
    let dns = NameLog::default();
    let t = native(&pair, &dns, Socks5::new().with_udp(), &relay);
    let v = send(&t, &named(&pair), true)
        .await
        .expect("through the relay");
    assert_eq!(v, http::Version::HTTP_3);
    assert!(
        relay.named_hosts().iter().all(|h| h == servers::ORIGIN),
        "every datagram header names the origin as written: {:?}",
        relay.named_hosts()
    );
    assert!(
        dns.names().iter().all(|n| !n.contains(servers::ORIGIN)),
        "the origin was named to the local resolver: {:?}",
        dns.names()
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn without_with_udp_the_request_goes_over_tcp() {
    let pair = servers::start();
    let relay = Socks5Udp::start(Behaviour::Relay).await;
    let t = native(&pair, &NameLog::default(), Socks5::new(), &relay);
    seed_alt_svc_h3(&t, &pair, &literal(&pair)).await;
    let v = send(&t, &literal(&pair), false).await.expect("request two");
    assert_eq!(v, http::Version::HTTP_11);
    assert_eq!(relay.associations(), 0);
    assert_eq!(relay.connects(), 2);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_relay_without_udp_switches_to_the_stream_and_is_remembered() {
    let pair = servers::start();
    let relay = Socks5Udp::start(Behaviour::RefuseCommand).await;
    let t = native(&pair, &NameLog::default(), Socks5::new().with_udp(), &relay);
    seed_alt_svc_h3(&t, &pair, &literal(&pair)).await;
    let v = send(&t, &literal(&pair), false).await.expect("request two");
    assert_eq!(v, http::Version::HTTP_11, "REP=0x07 switched to the stream");
    let v = send(&t, &literal(&pair), false)
        .await
        .expect("request three");
    assert_eq!(v, http::Version::HTTP_11);
    assert_eq!(relay.associations(), 1, "the refusal was remembered");
    assert_eq!(relay.connects(), 3);
    assert_eq!(pair.quic_attempted(), 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_relay_without_udp_switches_a_named_origin_without_resolving_it() {
    // The advertisement and the fallback both reached with a name: the
    // relay resolves it, on the association and on the `CONNECT` after the
    // refusal, and the local resolver hears it on neither.
    let pair = servers::start();
    let relay = Socks5Udp::start(Behaviour::RefuseCommand).await;
    let dns = NameLog::default();
    let t = native(&pair, &dns, Socks5::new().with_udp(), &relay);
    seed_alt_svc_h3(&t, &pair, &named(&pair)).await;
    let v = send(&t, &named(&pair), false).await.expect("request two");
    assert_eq!(v, http::Version::HTTP_11);
    assert_eq!(relay.associations(), 1, "the advertisement was acted on");
    assert_eq!(relay.connects(), 2);
    assert!(
        dns.names().iter().all(|n| !n.contains(servers::ORIGIN)),
        "the origin was named to the local resolver: {:?}",
        dns.names()
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn an_unspecified_relay_address_means_the_proxys_own() {
    let pair = servers::start();
    let relay = Socks5Udp::start(Behaviour::BindUnspecified).await;
    let t = native(&pair, &NameLog::default(), Socks5::new().with_udp(), &relay);
    let v = send(&t, &literal(&pair), true)
        .await
        .expect("through the relay at the proxy's own address");
    assert_eq!(v, http::Version::HTTP_3);
    assert_eq!(pair.quic_peers(), [relay.outbound_addr()]);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_request_dropped_mid_association_closes_the_control_connection() {
    let pair = servers::start();
    let relay = Socks5Udp::start(Behaviour::Stall).await;
    let t = native(&pair, &NameLog::default(), Socks5::new().with_udp(), &relay);
    let mut fut = Box::pin(t.execute(request("192.0.2.9", true)));
    // Polled until the relay holds the association request it will never
    // answer — so the drop below lands mid-association, not before it.
    tokio::select! {
        r = &mut fut => panic!("a stalled association cannot have answered: {:?}", r.map(|r| r.status())),
        () = relay.wait_stalled(std::time::Duration::from_secs(5)) => {}
    }
    assert_eq!(relay.control_closed(), 0, "the control connection is open");
    drop(fut);
    relay
        .wait_control_closed(std::time::Duration::from_secs(5))
        .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_relay_that_closes_the_control_connection_fails_the_connection_rather_than_hanging() {
    let pair = servers::start();
    let relay = Socks5Udp::start(Behaviour::Relay).await;
    let t = native(&pair, &NameLog::default(), Socks5::new().with_udp(), &relay);
    let v = send(&t, &literal(&pair), true).await.expect("request one");
    assert_eq!(v, http::Version::HTTP_3);
    // Request two goes out on the pooled connection and nothing it sends
    // is relayed, so it waits — on the association, which the relay then
    // ends. What must end it is that, not QUIC's 30 s idle timeout.
    relay.stop_relaying();
    let second = t.execute(request(&literal(&pair), true));
    let end = async {
        // Causal rather than timed: the request is waiting because nothing
        // it sent was relayed, and this sleep only makes it likely that it
        // is waiting on the pooled connection rather than not yet started.
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        relay.close_control();
        std::time::Instant::now()
    };
    let (r, closed_at) = tokio::join!(
        tokio::time::timeout(std::time::Duration::from_secs(25), second),
        end
    );
    let took = closed_at.elapsed();
    let r = r.expect("the request ended before the 25 s guard");
    assert!(
        r.is_err(),
        "nothing was relayed, so nothing answered: {r:?}"
    );
    assert!(
        took < std::time::Duration::from_secs(10),
        "waited {took:?} after the control connection closed: the idle timeout rather than its end"
    );
    assert_eq!(
        relay.associations(),
        1,
        "the in-flight request was not moved"
    );
}
