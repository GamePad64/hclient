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
//! port (RFC 9460 §2.3), and `hclient-native` deliberately applies no
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
use hclient_core::unversioned::Transport;
use hclient_core::{RequestBody, Timeouts};
use hclient_dns::SvcbEndpoint;
use hclient_h3::H3;
use hclient_native::Native;
use hclient_rt_tokio::TokioHandle;
use hclient_select::Selecting;
use http_body_util::BodyExt;
use servers::ORIGIN;
use std::time::Duration;

/// Short, because the one request here that is meant to fail should fail
/// quickly; never an assertion.
const CONNECT: Duration = Duration::from_millis(300);

/// Generous, and never the assertion: it turns a mutation that hangs into
/// a red test rather than a stuck one.
const BOUND: Duration = Duration::from_secs(10);

type Selector = Selecting<TokioHandle, hclient_tls_rustls::Rustls, FakeDns>;

fn selector(dns: FakeDns, tls: impl Fn() -> hclient_tls_rustls::Rustls) -> Selector {
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
        resolve: None,
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

/// An empty trust store: for the arms where nothing is meant to complete a
/// handshake, and the observation is a resolver's log.
fn untrusting_tls() -> hclient_tls_rustls::Rustls {
    let cfg = rustls::ClientConfig::builder_with_protocol_versions(&[&rustls::version::TLS13])
        .with_root_certificates(rustls::RootCertStore::empty())
        .with_no_client_auth();
    hclient_tls_rustls::Rustls::from_config(std::sync::Arc::new(cfg))
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
    let t = selector(dns.clone(), untrusting_tls);

    let _ = tokio::time::timeout(BOUND, t.execute(get(format!("https://{ORIGIN}/"))))
        .await
        .expect("the request finished inside the bound");

    assert_eq!(
        dns.svcb_names(),
        [ORIGIN],
        "the connector was told there is none, and did not go and ask"
    );
}

/// A member that cannot ask has not answered, so the question is still
/// open — and a transport whose own resolver can ask still asks it.
///
/// The third thing `Discovered` distinguishes, and it only shows up when
/// the members do not share a resolver, which
/// [`Selecting::new`](hclient_select::Selecting::new) explicitly allows
/// since it takes one of its own. The members here are built on a resolver
/// reporting `supports_svcb() == false`; this transport's can ask and
/// publishes `h3`. "The connector could not ask" is a fact about the
/// resolver, not about the origin, so it must arrive as
/// `Discovered::NotConsulted` — and collapsed into `NoRecord`, which is
/// where it used to be folded, this origin would have gone to TCP with a
/// perfectly capable resolver sitting unused.
///
/// **At the origin's default port, deliberately, and this is the only arm
/// where that is the point rather than a nuisance.** Discovery has three
/// gates in order — the port, the negative cache, then the capability —
/// and an arm at any other port never reaches the third. The first version
/// of this test named a port, connected to a real server, asserted the
/// right thing and *passed for the wrong reason*: the mutation that reads
/// "cannot ask" as "no record" survived it untouched (M54,
/// `docs/v04-w1-acceptance.md` §3.3).
///
/// What is observed is therefore the **question**, on two resolver logs,
/// and not the connection: nothing can listen on 443 here, so the request
/// fails, and its failure is not the observation. That the `h3` this
/// transport then reads chooses QUIC is `choice.rs`'s claim, and it is not
/// re-made here.
#[tokio::test(flavor = "multi_thread")]
async fn a_member_that_cannot_ask_has_not_answered_and_this_transport_still_asks() {
    let members = FakeDns::cannot_ask_but_would_have_said(vec![record_at(443)]);
    let ours = FakeDns::with_records(vec![service_record(1, &[b"h3"])]);
    let rt = TokioHandle::current().expect("inside #[tokio::test]");
    let t = Selecting::new(
        rt.clone(),
        Native::new(rt.clone(), untrusting_tls(), members.clone()),
        H3::new(rt, untrusting_tls(), members.clone()).expect("H3::new does no I/O"),
        ours.clone(),
    )
    .expect("the two stacks agree");

    let _ = tokio::time::timeout(BOUND, t.execute(get(format!("https://{ORIGIN}/"))))
        .await
        .expect("the request finished inside the bound");

    assert_eq!(
        members.svcb_lookups(),
        0,
        "a resolver that says it cannot ask is not asked"
    );
    assert_eq!(
        ours.svcb_names(),
        [ORIGIN],
        "and the question was still put, by the transport whose resolver can"
    );
}

/// A record fetched under the prefixed name stays out of the connector.
///
/// The URI names its own port, so RFC 9460 §2.3 puts its record under
/// `_<port>._https.<host>` — which is what this transport asks for, and
/// which is a record about **that** service. `hclient-native` applies no
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
