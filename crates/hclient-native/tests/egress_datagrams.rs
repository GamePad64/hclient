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
