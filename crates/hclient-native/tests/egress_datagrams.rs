//! QUIC over a filter's datagram path, asserted at the QUIC arm.
//!
//! The origin is a real HTTP/3 server on loopback and the path is a real
//! UDP socket aimed at it, so a response that arrives is a QUIC handshake
//! and an h3 exchange that really crossed the path — and the resolver is
//! one that counts, because the one thing a filtered connection must never
//! do is name its origin to the local resolver.
#![cfg(all(feature = "http3", not(target_family = "wasm")))]

#[path = "egress_fixtures.rs"]
mod fixtures;
#[path = "h3_server.rs"]
mod server;
#[path = "servers.rs"]
mod servers;

use futures_util::stream;
use hclient_core::body::RequestBody;
use hclient_core::error::Error;
use hclient_dns::{RData, Record, Resolve, rtype};
use hclient_native::H3;
use hclient_native::testing::ViaOutcome;
use hclient_rt_tokio::TokioHandle;
use server::Behaviour;
use std::net::{IpAddr, Ipv4Addr};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

/// A resolver that answers every name with `127.0.0.1` and counts how often
/// it was asked — so the direct half of a test can reach `localhost`, and
/// the filtered half can be caught asking.
#[derive(Debug, Clone, Default)]
struct Counting(Arc<AtomicUsize>);

impl Resolve for Counting {
    type Records<'a>
        = std::pin::Pin<Box<dyn futures_core::Stream<Item = Result<Record, Error>> + Send + 'a>>
    where
        Self: 'a;

    fn supports(&self, rtype: u16) -> bool {
        matches!(rtype, rtype::A | rtype::AAAA)
    }

    fn lookup<'a>(&'a self, _name: &str, rtype: u16) -> Self::Records<'a> {
        self.0.fetch_add(1, Ordering::SeqCst);
        match rtype {
            rtype::A => Box::pin(stream::iter(vec![Ok(Record::new(RData::from(
                IpAddr::V4(Ipv4Addr::LOCALHOST),
            )))])),
            _ => Box::pin(stream::empty()),
        }
    }
}

type Transport = H3<TokioHandle, hclient_tls_rustls::Rustls, Counting>;

fn h3(s: &server::Server) -> (Transport, Arc<AtomicUsize>) {
    let dns = Counting::default();
    let lookups = dns.0.clone();
    let t = H3::new(
        TokioHandle::current().expect("inside #[tokio::test]"),
        server::client_tls(&s.cert_der),
        dns,
    )
    .expect("H3::new does no I/O");
    (t, lookups)
}

/// `localhost` rather than the literal: a name is what a filtered request
/// must not resolve, and the certificate carries it.
fn get(s: &server::Server) -> http::Request<RequestBody> {
    http::Request::get(format!("https://localhost:{}/", s.addr.port()))
        .body(RequestBody::Empty)
        .unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn a_pool_miss_without_a_path_asks_for_one_and_does_no_io() {
    let s = server::start(Behaviour::Echo);
    let (t, lookups) = h3(&s);
    let got = t.stage_via_for_test(get(&s), "via-a", None).await;
    assert!(matches!(got, Err(ViaOutcome::NeedsPath)), "{got:?}");
    assert_eq!(s.dialled(), 0);
    assert_eq!(lookups.load(Ordering::SeqCst), 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_request_over_a_path_reaches_the_origin_and_resolves_nothing() {
    let s = server::start(Behaviour::Echo);
    let (t, lookups) = h3(&s);
    let resp = t
        .execute_via_for_test(get(&s), "via-a", fixtures::udp_bridge(s.addr))
        .await
        .expect("a request over the path");
    assert_eq!(resp.status(), 200);
    assert_eq!(s.requests(), 1);
    assert_eq!(lookups.load(Ordering::SeqCst), 0);
}

/// A path that claims more than its link carries loses almost nothing. A
/// SOCKS relay's `max_datagram_size` is a guess at a 1500-byte link;
/// behind a VPN or `PPPoE` the link is narrower, and a datagram over it is
/// lost without a word. Starting at the claim loses a burst of full-size
/// packets before quinn's black-hole detection falls back to 1200 —
/// measured, 23 of them for this upload, each a retransmission timeout on
/// a real network — and then stays at 1200. Starting at QUIC's floor and
/// discovering upwards loses only the probes that overshoot.
///
/// A count rather than a clock: the upload completes either way on
/// loopback, so what separates the two is how many datagrams died.
#[tokio::test(flavor = "multi_thread")]
async fn a_path_narrower_than_it_claims_still_carries_a_large_upload() {
    const BODY: usize = 256 * 1024;
    let s = server::start(Behaviour::CountBody);
    let (t, _) = h3(&s);
    let req = http::Request::post(format!("https://localhost:{}/", s.addr.port()))
        .body(RequestBody::Full(bytes::Bytes::from(vec![7u8; BODY])))
        .unwrap();
    let (path, dropped) = fixtures::udp_bridge_narrower_than_it_claims(s.addr, 1300);
    let resp = tokio::time::timeout(
        std::time::Duration::from_secs(10),
        t.execute_via_for_test(req, "via-a", path),
    )
    .await
    .expect("the upload completes rather than stalling on lost datagrams")
    .expect("a request over the path");
    assert_eq!(resp.status(), 200);
    let body = http_body_util::BodyExt::collect(resp.into_body())
        .await
        .expect("the count arrives")
        .to_bytes();
    assert_eq!(body, format!("{BODY} bytes"));
    let dropped = dropped.load(Ordering::SeqCst);
    assert!(
        dropped <= 8,
        "{dropped} datagrams lost to a link narrower than the path claimed"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_direct_and_a_via_connection_are_pooled_apart() {
    let s = server::start(Behaviour::Echo);
    let (t, _) = h3(&s);
    t.execute_for_test(get(&s)).await.expect("direct");
    t.execute_via_for_test(get(&s), "via-a", fixtures::udp_bridge(s.addr))
        .await
        .expect("over the path");
    assert_eq!(s.dialled(), 2);
    // And the path's connection is pooled under its filter's key: a second
    // request through the same filter needs no path at all.
    let again = t.stage_via_for_test(get(&s), "via-a", None).await;
    assert!(again.is_ok(), "{again:?}");
    // Nor does a different filter find it.
    let other = t.stage_via_for_test(get(&s), "via-b", None).await;
    assert!(matches!(other, Err(ViaOutcome::NeedsPath)), "{other:?}");
    assert_eq!(s.dialled(), 2);
}

/// Every `Connected` event's `remote`, in order.
#[derive(Debug, Clone, Default)]
struct Remotes(Arc<std::sync::Mutex<Vec<Option<std::net::SocketAddr>>>>);

impl hclient_core::hooks::Hooks for Remotes {
    fn on(&self, event: &hclient_core::hooks::Event<'_>) {
        if let hclient_core::hooks::Event::Connected(c) = event {
            self.0.lock().unwrap().push(c.remote);
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_connection_over_a_path_reports_no_remote_address() {
    // The address quinn holds for a path's peer is a stand-in it needed and
    // never sent to; reporting it would be a wrong answer where the absence
    // is a missing one. The direct connection beside it is the control.
    let s = server::start(Behaviour::Echo);
    let (t, _) = h3(&s);
    let seen = Remotes::default();
    let t = t.hooks(seen.clone());
    t.execute_for_test(get(&s)).await.expect("direct");
    t.execute_via_for_test(get(&s), "via-a", fixtures::udp_bridge(s.addr))
        .await
        .expect("over the path");
    assert_eq!(*seen.0.lock().unwrap(), vec![Some(s.addr), None]);
}

// --- `Native`: which requests a filter's datagram path carries ----------
//
// A [`servers::Pair`] is an HTTP/3 server and an HTTP/1.1 one on the same
// port, so the version of a response says which stack carried it — and the
// filter forwards whatever it opens to that pair, so a request that arrived
// at all came through the filter.

use fixtures::{ForwardFilter, Mode, NameLog};
use hclient_core::req::RequireVersion;
use hclient_core::transport::Transport as _;
use hclient_native::Native;

/// Never an assertion — it turns a mutation that hangs into a red test.
const BOUND: std::time::Duration = std::time::Duration::from_secs(10);

type Filtered = Native<TokioHandle, hclient_tls_rustls::Rustls, NameLog>;

/// Both stacks over one resolver, and `filter` in front of them.
fn native(pair: &servers::Pair, dns: &NameLog, filter: ForwardFilter) -> Filtered {
    let rt = TokioHandle::current().expect("inside #[tokio::test]");
    let quic = H3::new(rt.clone(), servers::client_tls(&pair.cert_der), dns.clone())
        .expect("H3::new does no I/O");
    Native::new(rt, servers::client_tls(&pair.cert_der), dns.clone())
        .http3(quic)
        .expect("the two stacks agree")
        .egress(filter)
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
async fn send(t: &Filtered, authority: &str, demand_h3: bool) -> Result<http::Version, Error> {
    let resp = tokio::time::timeout(BOUND, t.execute(request(authority, demand_h3)))
        .await
        .expect("the request finished inside the bound")?;
    assert_eq!(resp.status(), 200);
    Ok(resp.version())
}

fn literal(pair: &servers::Pair) -> String {
    format!("127.0.0.1:{}", pair.port)
}

/// Request one goes over the filter's stream and carries `Alt-Svc: h3` back,
/// so the next request has a signal to act on.
async fn seed_alt_svc_h3(t: &Filtered, pair: &servers::Pair) {
    pair.set_alt_svc(Some(&pair.h3_here("; ma=86400")));
    let v = send(t, &literal(pair), false)
        .await
        .expect("the seed request");
    assert_eq!(v, http::Version::HTTP_11, "no signal yet, so the stream");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_filtered_request_with_a_demand_for_h3_goes_over_the_path() {
    let pair = servers::start();
    let filter = ForwardFilter::new(Mode::DatagramsOnly, pair.addr());
    let t = native(&pair, &NameLog::default(), filter.clone());
    let v = send(&t, &literal(&pair), true)
        .await
        .expect("over the path");
    assert_eq!(v, http::Version::HTTP_3);
    assert_eq!(pair.quic_answered(), 1);
    assert_eq!(pair.tcp_accepted(), 0);
    assert_eq!(filter.datagram_attempts(), 1);
    // The connection is pooled under the filter's key: a second request
    // needs no second path.
    let v = send(&t, &literal(&pair), true).await.expect("pooled");
    assert_eq!(v, http::Version::HTTP_3);
    assert_eq!(filter.datagram_attempts(), 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_filtered_h3_request_never_resolves_the_origin() {
    let pair = servers::start();
    let dns = NameLog::default();
    let t = native(
        &pair,
        &dns,
        ForwardFilter::new(Mode::DatagramsOnly, pair.addr()),
    );
    let authority = format!("{}:{}", servers::ORIGIN, pair.port);
    let v = send(&t, &authority, true).await.expect("over the path");
    assert_eq!(v, http::Version::HTTP_3);
    assert!(
        dns.names().iter().all(|n| !n.contains(servers::ORIGIN)),
        "the origin was named to the local resolver: {:?}",
        dns.names()
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn an_advertisement_heard_through_a_filter_moves_the_next_request_onto_its_path() {
    let pair = servers::start();
    let filter = ForwardFilter::new(Mode::RefusingDatagramsWithStream, pair.addr());
    let t = native(&pair, &NameLog::default(), filter.clone());
    seed_alt_svc_h3(&t, &pair).await;
    // No signal on the first request, so the path was never asked for.
    assert_eq!(filter.datagram_attempts(), 0);
    let v = send(&t, &literal(&pair), false).await.expect("request two");
    assert_eq!(
        v,
        http::Version::HTTP_11,
        "the refusal switched to the stream"
    );
    assert_eq!(
        filter.datagram_attempts(),
        1,
        "the advertisement was acted on"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_filter_that_refuses_datagrams_sends_the_request_over_its_stream_and_remembers() {
    let pair = servers::start();
    let filter = ForwardFilter::new(Mode::RefusingDatagramsWithStream, pair.addr());
    let t = native(&pair, &NameLog::default(), filter.clone());
    seed_alt_svc_h3(&t, &pair).await;
    let r1 = send(&t, &literal(&pair), false).await.expect("request two");
    assert_eq!(r1, http::Version::HTTP_11);
    let r2 = send(&t, &literal(&pair), false)
        .await
        .expect("request three");
    assert_eq!(r2, http::Version::HTTP_11);
    assert_eq!(
        filter.datagram_attempts(),
        1,
        "the second request must not ask again"
    );
    assert_eq!(pair.quic_attempted(), 0, "nothing reached the QUIC server");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_path_too_small_for_quic_falls_back_to_the_stream_and_is_remembered() {
    let pair = servers::start();
    let filter = ForwardFilter::new(Mode::SmallPathWithStream, pair.addr());
    let t = native(&pair, &NameLog::default(), filter.clone());
    seed_alt_svc_h3(&t, &pair).await;
    let r1 = send(&t, &literal(&pair), false).await.expect("request two");
    assert_eq!(r1, http::Version::HTTP_11, "a 1199-byte path is refused");
    let r2 = send(&t, &literal(&pair), false)
        .await
        .expect("request three");
    assert_eq!(r2, http::Version::HTTP_11);
    assert_eq!(filter.datagram_attempts(), 1, "remembered");
    assert_eq!(pair.quic_attempted(), 0, "no packet was sent over it");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_path_too_small_for_quic_is_unsupported_when_h3_was_demanded() {
    let pair = servers::start();
    let filter = ForwardFilter::new(Mode::SmallPathWithStream, pair.addr());
    let t = native(&pair, &NameLog::default(), filter.clone());
    let e = send(&t, &literal(&pair), true).await.unwrap_err();
    assert_eq!(
        *e.kind(),
        hclient_core::error::ErrorKind::Unsupported,
        "{e:?}"
    );
    assert_eq!(filter.stream_opens(), 0, "a demand does not switch");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_demand_for_h3_through_a_filter_without_datagrams_is_no_datagram_path() {
    let pair = servers::start();
    let t = native(
        &pair,
        &NameLog::default(),
        ForwardFilter::new(Mode::StreamOnly, pair.addr()),
    );
    let e = send(&t, &literal(&pair), true).await.unwrap_err();
    assert!(
        std::error::Error::source(&e).is_some_and(|s| s
            .downcast_ref::<hclient_native::error::NoDatagramPath>()
            .is_some()),
        "{e:?}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_failed_proxy_is_final_and_does_not_switch() {
    let pair = servers::start();
    let filter = ForwardFilter::new(Mode::Failing, pair.addr());
    let t = native(&pair, &NameLog::default(), filter.clone());
    seed_alt_svc_h3(&t, &pair).await;
    let streams = filter.stream_opens();
    let e = send(&t, &literal(&pair), false).await.unwrap_err();
    assert_eq!(*e.kind(), hclient_core::error::ErrorKind::Connect, "{e:?}");
    assert_eq!(filter.datagram_attempts(), 1);
    assert_eq!(filter.stream_opens(), streams, "no switch to the stream");
}

#[tokio::test(flavor = "multi_thread")]
async fn opening_the_path_and_the_quic_handshake_spend_one_connect_bound() {
    // A path that takes most of the bound to open, to a peer that never
    // answers: the QUIC handshake gets only what is left, so the request
    // ends inside one bound rather than after the path's time plus a
    // whole second bound.
    const BOUND_C: std::time::Duration = std::time::Duration::from_millis(1000);
    let pair = servers::start();
    let hole = std::net::UdpSocket::bind("127.0.0.1:0").expect("bind a silent socket");
    let filter = ForwardFilter::new(
        Mode::Slow(BOUND_C * 8 / 10),
        hole.local_addr().expect("addr"),
    );
    let t = native(&pair, &NameLog::default(), filter);
    let mut req = request(&literal(&pair), true);
    req.extensions_mut()
        .insert(hclient_core::req::Timeouts::new().with_connect(BOUND_C));
    let began = std::time::Instant::now();
    let e = tokio::time::timeout(BOUND, t.execute(req))
        .await
        .expect("the request finished inside the guard")
        .expect_err("nothing answers over the path");
    let took = began.elapsed();
    assert_eq!(
        *e.kind(),
        hclient_core::error::ErrorKind::Timeout(hclient_core::error::Phase::Connect),
        "{e:?}"
    );
    assert!(
        took < BOUND_C * 3 / 2,
        "one connect bound of {BOUND_C:?} took {took:?}"
    );
    drop(hole);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_fallback_to_the_stream_after_quic_over_a_slow_path_gets_what_is_left_once() {
    // The path takes `OPEN` to open and is too small for QUIC, so QUIC over
    // it fails at once and the request switches to the stream. What the
    // stream's context says is left of the bound is read against what the
    // path's context said and the time the filter itself saw pass between
    // the two: spent once, the two agree; spent twice, the stream is short
    // by the path's whole opening, which is a sleep of `OPEN` and so never
    // less. No step here races a clock against the bound.
    const BOUND_C: std::time::Duration = std::time::Duration::from_millis(5000);
    const OPEN: std::time::Duration = std::time::Duration::from_millis(400);
    let pair = servers::start();
    let filter = ForwardFilter::new(
        Mode::SlowSmallPathSlowStream(OPEN, std::time::Duration::ZERO),
        pair.addr(),
    );
    let t = native(&pair, &NameLog::default(), filter.clone());
    seed_alt_svc_h3(&t, &pair).await;
    let mut req = request(&literal(&pair), false);
    req.extensions_mut()
        .insert(hclient_core::req::Timeouts::new().with_connect(BOUND_C));
    let resp = tokio::time::timeout(BOUND, t.execute(req))
        .await
        .expect("the request finished inside the guard")
        .expect("the stream carries the request");
    assert_eq!(resp.version(), http::Version::HTTP_11);
    assert_eq!(filter.datagram_attempts(), 1, "the path was tried first");

    // The seeding request's stream comes first; this request's path and
    // stream are the last two.
    let lent = filter.lent();
    let [.., path, stream] = lent.as_slice() else {
        panic!("a path and a stream were asked for: {lent:?}")
    };
    assert!(path.datagrams && !stream.datagrams, "{lent:?}");
    let (Some(at_path), Some(at_stream)) = (path.remaining, stream.remaining) else {
        panic!("both contexts carry the caller's bound: {lent:?}")
    };
    assert!(at_path <= BOUND_C, "{lent:?}");
    // What the stream should have been handed, by the filter's own clock.
    let expected = at_path.saturating_sub(stream.at - path.at);
    let short = expected.saturating_sub(at_stream);
    assert!(
        short < OPEN / 2,
        "the stream was handed {at_stream:?} where {expected:?} was left: \
         the path's {OPEN:?} was taken off the bound twice"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_filtered_demand_for_http_1_1_takes_the_stream_and_teaches_the_memory_nothing() {
    let pair = servers::start();
    let filter = ForwardFilter::new(Mode::Both, pair.addr());
    let t = native(&pair, &NameLog::default(), filter.clone());
    seed_alt_svc_h3(&t, &pair).await;
    let mut req = request(&literal(&pair), false);
    req.extensions_mut()
        .insert(RequireVersion(http::Version::HTTP_11));
    let resp = tokio::time::timeout(BOUND, t.execute(req))
        .await
        .expect("the request finished inside the guard")
        .expect("served over the stream");
    assert_eq!(resp.version(), http::Version::HTTP_11);
    assert_eq!(filter.datagram_attempts(), 0, "no QUIC attempt");
    assert_eq!(pair.quic_attempted(), 0);
    // Nothing was learned about the origin: the advertisement still moves
    // an unconstrained request onto the path.
    let v = send(&t, &literal(&pair), false)
        .await
        .expect("request three");
    assert_eq!(v, http::Version::HTTP_3);
    assert_eq!(filter.datagram_attempts(), 1);
}
