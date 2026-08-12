//! The record this transport fetched is the record the TCP stack connects
//! under — and the three things that says.
//!
//! `tests/dns_cost.rs` counts queries, which is what the handover was
//! built for. Counting alone cannot tell a shared answer from a lost one:
//! a connector that took the answer and threw it away would ask exactly as
//! few questions as one that used it, and every row in that file would
//! stay where it is. So these arms watch the **connection** instead.
//!
//! | what is handed over | what must happen |
//! |---|---|
//! | a record naming a port | the connection goes to that port — the connector acted on an answer it did not fetch |
//! | *no record*, and the origin publishes none | one query, not two: "there is none" is an answer, and the connector must not re-ask |
//! | a record fetched under a **prefixed** name | it never reaches the connector, so a request that named its own port stays on it |
//!
//! The third is the one that would be a defect rather than a cost. This
//! transport reads a record for `_8443._https.origin` when a URI names a
//! port (RFC 9460 §2.3), and `http-ng-native` deliberately applies no
//! record there at all — *"applying it would be applying one service's
//! parameters to another's"*. Handing that record over would move a
//! connection to a port published for a different service. It cannot
//! happen, and the reason is structural rather than careful:
//! `Native::prepare` is what fetches a record, so the only records that
//! exist inside a `Prepared` are the ones the connector fetched under its
//! own rules. This arm is what makes that claim observable from outside.
#![cfg(not(target_family = "wasm"))]

mod fakedns;
mod servers;

use fakedns::{FakeDns, service_record};
use http_body_util::BodyExt;
use http_ng_core::unversioned::Transport;
use http_ng_core::{RequestBody, Timeouts};
use http_ng_dns::SvcbEndpoint;
use http_ng_h3::H3;
use http_ng_native::Native;
use http_ng_rt_tokio::TokioHandle;
use http_ng_select::Selecting;
use servers::ORIGIN;
use std::time::Duration;

/// Short, because the one request here that is meant to fail should fail
/// quickly; never an assertion.
const CONNECT: Duration = Duration::from_millis(300);

/// Generous, and never the assertion: it turns a mutation that hangs into
/// a red test rather than a stuck one.
const BOUND: Duration = Duration::from_secs(10);

type Selector = Selecting<TokioHandle, http_ng_tls_rustls::Rustls, FakeDns>;

fn selector(dns: FakeDns, tls: impl Fn() -> http_ng_tls_rustls::Rustls) -> Selector {
    let rt = TokioHandle::current().expect("inside #[tokio::test]");
    Selecting::new(
        rt.clone(),
        Native::new(rt.clone(), tls(), dns.clone()),
        H3::new(rt, tls(), dns.clone()).expect("H3::new does no I/O"),
        dns,
    )
    .expect("the two stacks agree")
}

fn get(uri: String) -> http::Request<RequestBody> {
    let mut req = http::Request::builder()
        .uri(uri)
        .body(RequestBody::Empty)
        .expect("a well-formed request");
    req.extensions_mut().insert(Timeouts {
        connect: Some(CONNECT),
        ..Timeouts::default()
    });
    req
}

/// A ServiceMode record that moves a connection: the origin's service is
/// at `port`, and it speaks HTTP/1.1.
fn record_at(port: u16) -> SvcbEndpoint {
    SvcbEndpoint {
        port: Some(port),
        ..service_record(1, &[b"http/1.1"])
    }
}

/// The handover is real: the connection is made under the record this
/// transport fetched, not under a second one and not under none.
///
/// The URI names no port, so the connection would go to 443 — where an
/// unprivileged test process cannot put a listener, and where nothing is
/// listening here. The record names the TCP server's port. So a request
/// that arrives at that server is proof the record reached the connector,
/// and it arrives after **one** type-65 query rather than the two this
/// same exchange used to cost.
///
/// This is the arm that separates "the duplicate is gone" from "discovery
/// is gone". Both would show one query.
#[tokio::test(flavor = "multi_thread")]
async fn the_record_this_transport_fetched_is_the_one_the_connection_is_made_under() {
    let pair = servers::start();
    let dns = FakeDns::with_records(vec![record_at(pair.port)]);
    let t = selector(dns.clone(), || servers::client_tls(&pair.cert_der));

    let resp = tokio::time::timeout(BOUND, t.execute(get(format!("https://{ORIGIN}/hello"))))
        .await
        .expect("inside the bound")
        .expect("the TCP server answered, at the port the record named");
    assert_eq!(resp.status(), 200);
    let body = resp
        .into_body()
        .collect()
        .await
        .expect("a complete body")
        .to_bytes();
    assert_eq!(body.as_ref(), b"h1", "the TCP server's own answer");

    assert_eq!(pair.tcp_answered(), 1);
    assert_eq!(pair.quic_answered(), 0, "the record offered http/1.1");
    assert_eq!(
        dns.svcb_names(),
        [ORIGIN],
        "one query for one record, and the connector connected under it"
    );
}

/// "This origin publishes no record" is an answer, and it travels.
///
/// The half a plain `Option` gets wrong: with "nothing found" and "nobody
/// looked" collapsed into one value, the connector would re-query exactly
/// the origins whose answer cost the most to get — the ones with no record
/// to find. Here the resolver can answer and has nothing to give, and the
/// count says the question was asked once.
///
/// The request itself fails: with no record there is no port to move to,
/// the origin's own endpoint is 443, and nothing is listening there. That
/// failure is never the observation — the resolver's log is.
#[tokio::test(flavor = "multi_thread")]
async fn an_origin_that_publishes_no_record_is_not_asked_about_twice() {
    let dns = FakeDns::new();
    let t = selector(dns.clone(), || {
        // An empty trust store: nothing here completes a handshake.
        let cfg = rustls::ClientConfig::builder_with_protocol_versions(&[&rustls::version::TLS13])
            .with_root_certificates(rustls::RootCertStore::empty())
            .with_no_client_auth();
        http_ng_tls_rustls::Rustls::from_config(std::sync::Arc::new(cfg))
    });

    let _ = tokio::time::timeout(BOUND, t.execute(get(format!("https://{ORIGIN}/"))))
        .await
        .expect("the request finished inside the bound");

    assert_eq!(
        dns.svcb_names(),
        [ORIGIN],
        "the connector was told there is none, and did not go and ask"
    );
}

/// A resolver that cannot ask has not answered, and the transport whose
/// resolver *can* still asks.
///
/// The third thing `Discovered` distinguishes, and the one that only shows
/// up when the members do not share a resolver — which
/// [`Selecting::new`](http_ng_select::Selecting::new) explicitly allows,
/// since it takes one of its own. The TCP member here is built on a
/// resolver that reports `supports_svcb() == false`; this transport's is a
/// resolver that can, and publishes `h3`. "The connector could not ask" is
/// **not** "the origin publishes no record", so the question is still
/// open, and the request must reach the QUIC server.
///
/// Collapsed into `NoRecord` — which is what "the resolver cannot ask"
/// used to be folded into — this origin would have gone to TCP with a
/// perfectly capable resolver sitting unused. The counts on both logs are
/// the other half: the member's resolver was never asked anything (it said
/// it could not), and this transport's was asked once.
#[tokio::test(flavor = "multi_thread")]
async fn a_member_that_cannot_ask_has_not_answered_and_this_transport_still_asks() {
    let pair = servers::start();
    let members = FakeDns::cannot_ask_but_would_have_said(vec![record_at(pair.port)]);
    let ours = FakeDns::with_records(vec![service_record(1, &[b"h3"])]);
    let rt = TokioHandle::current().expect("inside #[tokio::test]");
    let t = Selecting::new(
        rt.clone(),
        Native::new(
            rt.clone(),
            servers::client_tls(&pair.cert_der),
            members.clone(),
        ),
        H3::new(rt, servers::client_tls(&pair.cert_der), members.clone()).expect("no I/O"),
        ours.clone(),
    )
    .expect("the two stacks agree");

    let resp = tokio::time::timeout(
        BOUND,
        t.execute(get(format!("https://{ORIGIN}:{}/hello", pair.port))),
    )
    .await
    .expect("inside the bound")
    .expect("the QUIC server answered");
    assert_eq!(resp.status(), 200);
    let body = resp
        .into_body()
        .collect()
        .await
        .expect("a complete body")
        .to_bytes();
    assert_eq!(body.as_ref(), b"h3", "the QUIC server's own answer");

    assert_eq!(pair.quic_answered(), 1);
    assert_eq!(pair.tcp_answered(), 0);
    assert_eq!(
        members.svcb_lookups(),
        0,
        "a resolver that says it cannot ask is not asked"
    );
    assert_eq!(
        ours.svcb_names(),
        [format!("_{}._https.{ORIGIN}", pair.port)],
        "and the question was still put, by the transport whose resolver can"
    );
}

/// A record fetched under the prefixed name stays out of the connector.
///
/// The URI names its own port, so RFC 9460 §2.3 puts its record under
/// `_<port>._https.<host>` — which is what this transport asks for, and
/// which is a record about **that** service. `http-ng-native` applies no
/// record at a non-default port at all, and the handover must not be a way
/// round that: the resolver here answers every name with a record naming a
/// *different* server's port, and the request must still go to the port
/// its own URI named.
///
/// The second server is alive throughout, so "the request did not go
/// there" is a choice and not the only possibility.
#[tokio::test(flavor = "multi_thread")]
async fn a_record_this_transport_fetched_under_a_prefixed_name_does_not_move_the_connection() {
    let pair = servers::start();
    let elsewhere = servers::start();
    let dns = FakeDns::with_records(vec![record_at(elsewhere.port)]);
    let t = selector(dns.clone(), || servers::client_tls(&pair.cert_der));

    let resp = tokio::time::timeout(
        BOUND,
        t.execute(get(format!("https://{ORIGIN}:{}/hello", pair.port))),
    )
    .await
    .expect("inside the bound")
    .expect("the server at the URI's own port answered");
    assert_eq!(resp.status(), 200);
    let _ = resp.into_body().collect().await.expect("a complete body");

    assert_eq!(
        pair.tcp_answered(),
        1,
        "the URI's own port is where it went"
    );
    assert_eq!(
        elsewhere.tcp_accepted(),
        0,
        "the record's port belongs to another service and was never dialled"
    );
    assert_eq!(
        dns.svcb_names(),
        [format!("_{}._https.{ORIGIN}", pair.port)],
        "asked once, under the prefixed name, by this transport alone"
    );
}
