//! Which stack answered, asked of the servers rather than of the client.
//!
//! Two real servers behind one authority (`tests/servers.rs`), both alive
//! in every test, and the only thing that changes between the arms is the
//! HTTPS record the resolver hands back. So a request reaching one of them
//! is a **choice**: the other was reachable and was not chosen.
//!
//! Nothing here waits for a duration and then asserts. Three timing
//! assertions in this workspace turned out to be flakes and one was hiding
//! a real defect; the observations below are all of the form "this server
//! answered and that one did not", which is settled by the time the
//! response is in hand.
//!
//! Each request also carries a wall-clock bound of its own — not as an
//! assertion, but so a mutation that turns a choice into a hang is red
//! rather than eternal.
#![cfg(all(feature = "http3", not(target_family = "wasm")))]

mod fakedns;
mod servers;

use fakedns::{FakeDns, alias_record, service_record};
use hclient_core::unversioned::Transport;
use hclient_core::{RequestBody, RequireVersion};
use hclient_native::H3;
use hclient_native::Native;
use hclient_rt_tokio::TokioHandle;
use http_body_util::BodyExt;
use servers::{ORIGIN, Pair};
use std::time::Duration;

/// Bounds every request, so that a mutation which turns a choice into a
/// hang fails instead of running out the harness's own patience.
const BOUND: Duration = Duration::from_secs(10);

type Selector = Native<TokioHandle, hclient_tls_rustls::Rustls, FakeDns>;

fn selector(pair: &Pair, dns: FakeDns) -> Selector {
    let rt = TokioHandle::current().expect("inside #[tokio::test]");
    let tls = servers::client_tls(&pair.cert_der);
    let quic = H3::new(rt.clone(), tls.clone(), dns.clone()).expect("H3::new does no I/O");
    Native::new(rt, tls, dns)
        .http3(quic)
        .expect("the two paths agree")
}

fn get(pair: &Pair, scheme: &str) -> http::Request<RequestBody> {
    http::Request::builder()
        .uri(format!("{scheme}://{ORIGIN}:{}/hello", pair.port))
        .body(RequestBody::Empty)
        .expect("a well-formed request")
}

/// The response status, version and body, or the error — whichever the
/// exchange produced, inside [`BOUND`].
async fn send(t: &Selector, req: http::Request<RequestBody>) -> (http::Version, String) {
    let resp = tokio::time::timeout(BOUND, t.execute(req))
        .await
        .expect("the request finished inside the bound")
        .expect("one of the two servers answered");
    assert_eq!(resp.status(), 200);
    let version = resp.version();
    let body = resp
        .into_body()
        .collect()
        .await
        .expect("a complete body")
        .to_bytes();
    (version, String::from_utf8(body.to_vec()).expect("utf-8"))
}

// --- the record decides -------------------------------------------------

/// The deliverable, in one test: a record that lists `h3` puts the request
/// on the QUIC server, and the TCP server — reachable, on the same
/// authority, at the same port number — gets nothing.
#[tokio::test(flavor = "multi_thread")]
async fn a_record_offering_h3_puts_the_request_on_the_quic_server() {
    let pair = servers::start();
    let dns = FakeDns::with_records(vec![service_record(1, &[b"h3"])]);
    let t = selector(&pair, dns.clone());

    let (version, body) = send(&t, get(&pair, "https")).await;

    assert_eq!(body, "h3", "the QUIC server's own answer");
    assert_eq!(version, http::Version::HTTP_3);
    assert_eq!(pair.quic_answered(), 1);
    assert_eq!(pair.tcp_answered(), 0, "the TCP server was not chosen");
    // …and the record really was consulted, under the name RFC 9460 §2.3
    // puts a non-default port's record at.
    assert_eq!(
        dns.svcb_names(),
        [format!("_{}._https.{ORIGIN}", pair.port)]
    );
}

/// The other half of the same claim, and the reason it is a separate test:
/// a transport that always chose QUIC would pass the one above.
#[tokio::test(flavor = "multi_thread")]
async fn a_record_that_does_not_offer_h3_puts_the_request_on_the_tcp_server() {
    let pair = servers::start();
    let dns = FakeDns::with_records(vec![service_record(1, &[b"h2", b"http/1.1"])]);
    let t = selector(&pair, dns.clone());

    let (version, body) = send(&t, get(&pair, "https")).await;

    assert_eq!(body, "h1", "the TCP server's own answer");
    assert_eq!(version, http::Version::HTTP_11);
    assert_eq!(pair.tcp_answered(), 1);
    assert_eq!(pair.quic_answered(), 0, "the QUIC server was not chosen");
}

/// An origin that publishes no record at all is TCP: browsers do not race
/// on first contact, and an unknown origin gets TCP — the default this
/// whole mechanism is an exception to.
#[tokio::test(flavor = "multi_thread")]
async fn an_origin_with_no_record_is_served_over_tcp() {
    let pair = servers::start();
    let dns = FakeDns::new();
    let t = selector(&pair, dns.clone());

    let (_, body) = send(&t, get(&pair, "https")).await;

    assert_eq!(body, "h1");
    assert_eq!(pair.quic_answered(), 0);
    // The resolver *was* asked — "no record" is an answer, not a skipped
    // question, and the two are different states this transport must not
    // conflate.
    assert_eq!(dns.svcb_lookups(), 1);
}

/// A resolver that cannot do SVCB is asked whether it can, not whether its
/// stream was empty.
///
/// It is holding a record that offers `h3`, so a transport that read the
/// stream without first reading the capability would choose QUIC here.
/// `Resolve::supports_svcb` exists precisely to keep "cannot ask" and
/// "asked and found nothing" apart, and this is the arm where the
/// difference is visible.
#[tokio::test(flavor = "multi_thread")]
async fn a_resolver_that_cannot_do_svcb_never_chooses_quic() {
    let pair = servers::start();
    let dns = FakeDns::cannot_ask_but_would_have_said(vec![service_record(1, &[b"h3"])]);
    let t = selector(&pair, dns.clone());

    let (_, body) = send(&t, get(&pair, "https")).await;

    assert_eq!(body, "h1");
    assert_eq!(pair.quic_answered(), 0);
    assert_eq!(
        dns.svcb_lookups(),
        0,
        "a resolver that says it cannot ask is not asked"
    );
}

/// An AliasMode record does not hide the ServiceMode one behind it.
///
/// `priority: 0` with every parameter empty sorts *below* every real
/// record, so a selection that ranked by priority without skipping these
/// would land on the one endpoint whose ALPN list is empty and would never
/// choose QUIC — discovery wired up and doing nothing, which is the shape
/// `hclient-native`'s own selection wrote the skip down for.
#[tokio::test(flavor = "multi_thread")]
async fn an_alias_record_does_not_out_rank_the_service_record_behind_it() {
    let pair = servers::start();
    let dns = FakeDns::with_records(vec![alias_record(), service_record(1, &[b"h3"])]);
    let t = selector(&pair, dns);

    let (_, body) = send(&t, get(&pair, "https")).await;

    assert_eq!(body, "h3");
    assert_eq!(pair.quic_answered(), 1);
}

/// The origin's own ranking is honoured: the **first-ranked** record
/// decides, not any record that happens to mention `h3`.
///
/// RFC 9460 §2.4.2 makes priority the operator's preference order, so an
/// origin whose best endpoint is HTTP/2-only has asked for HTTP/2 even
/// though it also publishes an `h3` alternative.
#[tokio::test(flavor = "multi_thread")]
async fn the_first_ranked_record_decides_and_a_lower_one_does_not_override_it() {
    let pair = servers::start();
    let dns = FakeDns::with_records(vec![
        service_record(2, &[b"h3"]),
        service_record(1, &[b"http/1.1"]),
    ]);
    let t = selector(&pair, dns);

    let (_, body) = send(&t, get(&pair, "https")).await;

    assert_eq!(
        body, "h1",
        "priority 1 says http/1.1; the h3 record at priority 2 is the alternative"
    );
    assert_eq!(pair.quic_answered(), 0);
}

/// …and the same two records the other way round, which is what makes the
/// test above about *ranking* rather than about list order.
#[tokio::test(flavor = "multi_thread")]
async fn the_first_ranked_record_decides_when_it_is_the_h3_one() {
    let pair = servers::start();
    let dns = FakeDns::with_records(vec![
        service_record(2, &[b"http/1.1"]),
        service_record(1, &[b"h3"]),
    ]);
    let t = selector(&pair, dns);

    let (_, body) = send(&t, get(&pair, "https")).await;

    assert_eq!(body, "h3");
    assert_eq!(pair.quic_answered(), 1);
}

/// A cleartext origin is never QUIC, and the resolver is not even asked.
///
/// HTTP/3 has no cleartext form, and an HTTPS record against an `http://`
/// origin means something this transport will not do on a caller's behalf
/// (RFC 9460 §9.5 makes it an instruction to upgrade the scheme). The
/// request goes to the TCP stack and fails there against a TLS listener,
/// which is the honest outcome and not the observation: what is asserted
/// is that the QUIC server saw nothing and the resolver was never asked.
#[tokio::test(flavor = "multi_thread")]
async fn a_cleartext_origin_is_not_offered_to_quic_and_costs_no_lookup() {
    let pair = servers::start();
    let dns = FakeDns::with_records(vec![service_record(1, &[b"h3"])]);
    let t = selector(&pair, dns.clone());

    // Not `send`: this one is expected to fail, because the peer on TCP
    // speaks TLS and this request does not.
    let _ = tokio::time::timeout(BOUND, t.execute(get(&pair, "http")))
        .await
        .expect("the request finished inside the bound");

    assert!(
        pair.tcp_accepted() >= 1,
        "the TCP stack was the one that tried"
    );
    assert_eq!(pair.quic_answered(), 0);
    assert_eq!(dns.svcb_lookups(), 0, "no record is fetched for http://");
}

/// An IP literal is served over TCP, and no record is fetched for it.
///
/// A literal has no name to look up: `_443._https.127.0.0.1` is a query
/// with no answer, and a transport that built it would make every
/// literal-addressed request pay for one. The resolver here holds a record
/// offering `h3` and says it can answer, so a transport that skipped the
/// literal check would choose QUIC.
#[tokio::test(flavor = "multi_thread")]
async fn an_ip_literal_has_no_record_to_look_up_and_is_served_over_tcp() {
    let pair = servers::start();
    let dns = FakeDns::with_records(vec![service_record(1, &[b"h3"])]);
    let t = selector(&pair, dns.clone());

    let req = http::Request::builder()
        .uri(format!("https://127.0.0.1:{}/hello", pair.port))
        .body(RequestBody::Empty)
        .expect("a well-formed request");
    let (_, body) = send(&t, req).await;

    assert_eq!(body, "h1");
    assert_eq!(pair.quic_answered(), 0);
    assert_eq!(
        dns.svcb_lookups(),
        0,
        "a literal is not a name to ask about"
    );
}

// --- a demand outranks the record --------------------------------------

/// `RequireVersion(HTTP_3)` reaches the QUIC stack at an origin that
/// publishes nothing, and asks the resolver nothing.
///
/// Both members report `version_select: true`, so the composite does too,
/// and a transport reporting it owes an answer. Routing by the record and
/// leaving the demand to whichever member won would fail this request with
/// `VersionNotAvailable` from a transport that owns an HTTP/3 stack.
#[tokio::test(flavor = "multi_thread")]
async fn a_demand_for_http_3_is_served_over_quic_without_a_record_or_a_lookup() {
    let pair = servers::start();
    let dns = FakeDns::new();
    let t = selector(&pair, dns.clone());

    let mut req = get(&pair, "https");
    req.extensions_mut()
        .insert(RequireVersion(http::Version::HTTP_3));
    let (version, body) = send(&t, req).await;

    assert_eq!(body, "h3");
    assert_eq!(version, http::Version::HTTP_3);
    assert_eq!(pair.quic_answered(), 1);
    assert_eq!(dns.svcb_lookups(), 0, "a demand is answered before DNS is");
}

/// …and the opposite demand takes the TCP stack even where the record says
/// `h3`, which is the pair that makes the rule a rule.
#[tokio::test(flavor = "multi_thread")]
async fn a_demand_for_http_1_1_is_served_over_tcp_although_the_record_offers_h3() {
    let pair = servers::start();
    let dns = FakeDns::with_records(vec![service_record(1, &[b"h3"])]);
    let t = selector(&pair, dns.clone());

    let mut req = get(&pair, "https");
    req.extensions_mut()
        .insert(RequireVersion(http::Version::HTTP_11));
    let (version, body) = send(&t, req).await;

    assert_eq!(body, "h1");
    assert_eq!(version, http::Version::HTTP_11);
    assert_eq!(pair.tcp_answered(), 1);
    assert_eq!(pair.quic_answered(), 0);
    assert_eq!(dns.svcb_lookups(), 0);
}

// --- and through a real `Client` ---------------------------------------

/// The whole point of the crate, from where a caller stands: one
/// `hclient::Client`, two origins' worth of behaviour, no `#[cfg]` and no
/// second client.
///
/// The counters are read as **deltas** across the two arms rather than as
/// totals, because the servers outlive both clients — a total would be
/// satisfied by the first arm alone.
#[tokio::test(flavor = "multi_thread")]
async fn one_client_reaches_both_servers_depending_only_on_the_record() {
    let pair = servers::start();

    for (record, expected, quic_delta, tcp_delta) in [
        (service_record(1, &[b"h3"]), "h3", 1, 0),
        (service_record(1, &[b"http/1.1"]), "h1", 0, 1),
    ] {
        let (quic_before, tcp_before) = (pair.quic_answered(), pair.tcp_answered());
        let dns = FakeDns::with_records(vec![record]);
        let client = hclient::Client::builder(selector(&pair, dns))
            .build()
            .expect("the capabilities this transport reports are all honourable");
        let text = tokio::time::timeout(
            BOUND,
            client
                .get(&format!("https://{ORIGIN}:{}/hello", pair.port))
                .send(),
        )
        .await
        .expect("inside the bound")
        .expect("a response")
        .collect()
        .await
        .expect("a complete body")
        .text()
        .expect("utf-8");

        assert_eq!(text, expected);
        assert_eq!(pair.quic_answered() - quic_before, quic_delta);
        assert_eq!(pair.tcp_answered() - tcp_before, tcp_delta);
    }
}
