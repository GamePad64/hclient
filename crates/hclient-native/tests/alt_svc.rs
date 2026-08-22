//! The slow tier, watched from the servers' side of the wire.
//!
//! `tests/choice.rs` shows the fast tier choosing between two real servers
//! on the strength of a DNS record. This file shows the *other* tier, and
//! its whole shape is in the first test: **request 1 goes over TCP and
//! request 2 goes over QUIC**, because the advertisement that changed the
//! answer arrived in the response to request 1. A test that did not show
//! both requests would not have tested the feature — an `Alt-Svc` field is
//! a response header, so the first request to an unknown origin is TCP no
//! matter what the origin has to say.
//!
//! The resolver here answers **no** HTTPS record, on purpose. That is the
//! majority of the web, and it is exactly the population the fast tier
//! cannot serve; where a record does exist, it decides and this tier is
//! not consulted (`a_record_that_does_not_offer_h3_is_not_overruled_by_an_
//! advertisement`).
//!
//! Every assertion is causal, and none of them waits for a duration. Both
//! servers are alive throughout, on the same port number and behind the
//! same certificate, so a request reaching one of them is a choice: the
//! other was reachable and was not chosen. The `ma` timescales that cannot
//! be reached this way — twenty-four hours — are in
//! `tests/altsvc_cache.rs`, where the clock is a parameter.
#![cfg(all(feature = "http3", not(target_family = "wasm")))]

mod fakedns;
mod servers;

use fakedns::{FakeDns, service_record};
use hclient_core::RequestBody;
use hclient_core::unversioned::Transport;
use hclient_h3::H3;
use hclient_native::Native;
use hclient_rt_tokio::TokioHandle;
use http_body_util::BodyExt;
use servers::{ORIGIN, Pair};
use std::time::Duration;

/// Never an assertion — it turns a mutation that hangs into a red test
/// rather than an eternal one.
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

/// One request to the pair, and which server answered it — read off the
/// body, which is each server's own word for itself, and cross-checked
/// against the counters by [`hop`].
async fn send(t: &Selector, uri: &str) -> String {
    let req = http::Request::builder()
        .uri(uri)
        .body(RequestBody::Empty)
        .expect("a well-formed request");
    let resp = tokio::time::timeout(BOUND, t.execute(req))
        .await
        .expect("the request finished inside the bound")
        .expect("one of the two servers answered");
    assert_eq!(resp.status(), 200);
    let body = resp
        .into_body()
        .collect()
        .await
        .expect("a complete body")
        .to_bytes();
    String::from_utf8(body.to_vec()).expect("utf-8")
}

/// One request, and the **delta** on both counters across it.
///
/// Deltas rather than totals because these tests make several requests to
/// servers that outlive them all, and a total is satisfied by an earlier
/// hop. Both counters are read every time: "the QUIC server answered" and
/// "the TCP server did not" are two claims, and neither counter can make
/// both.
async fn hop(t: &Selector, pair: &Pair, uri: &str, expect: &str) {
    let (tcp, quic) = (pair.tcp_answered(), pair.quic_answered());
    let body = send(t, uri).await;
    let (tcp, quic) = (pair.tcp_answered() - tcp, pair.quic_answered() - quic);
    assert_eq!(
        (body.as_str(), tcp, quic),
        (
            expect,
            usize::from(expect == "h1"),
            usize::from(expect == "h3")
        ),
        "expected this hop on the {expect} server and on nothing else"
    );
}

fn uri(pair: &Pair) -> String {
    format!("https://{ORIGIN}:{}/hello", pair.port)
}

// --- the deliverable ----------------------------------------------------

/// **The whole feature.** Request 1 goes over TCP — the origin publishes
/// no HTTPS record and this client has never heard of it — and its answer
/// carries `Alt-Svc: h3=":<port>"`. Request 2, to the same origin through
/// the same transport, goes over QUIC.
///
/// Both servers are up for both requests, so neither hop is the only thing
/// that could have happened.
#[tokio::test(flavor = "multi_thread")]
async fn the_first_request_is_tcp_and_the_second_is_quic() {
    let pair = servers::start();
    pair.set_alt_svc(Some(&pair.h3_here("; ma=86400")));
    let dns = FakeDns::new();
    let t = selector(&pair, dns.clone());

    hop(&t, &pair, &uri(&pair), "h1").await;
    hop(&t, &pair, &uri(&pair), "h3").await;

    // …and nothing new was asked of DNS to get there: one type-65 query
    // per request, which is what an origin with no record already paid
    // before this tier existed.
    assert_eq!(dns.svcb_lookups(), 2);
}

/// The control, and without it the test above would pass for a transport
/// that simply sent every request after the first over QUIC.
///
/// The same two requests to the same servers, with the origin advertising
/// nothing at all.
#[tokio::test(flavor = "multi_thread")]
async fn an_origin_that_advertises_nothing_never_moves_off_tcp() {
    let pair = servers::start();
    let dns = FakeDns::new();
    let t = selector(&pair, dns);

    hop(&t, &pair, &uri(&pair), "h1").await;
    hop(&t, &pair, &uri(&pair), "h1").await;
    hop(&t, &pair, &uri(&pair), "h1").await;
}

/// A second control of a different kind: the advertisement is remembered
/// by the **transport**, not by the process. A fresh `Selecting` over the
/// same servers starts again on TCP.
///
/// This is the scope decision made visible — nothing is persisted, so a
/// caller that drops its client has forgotten everything it heard.
#[tokio::test(flavor = "multi_thread")]
async fn a_new_transport_has_heard_nothing_and_starts_on_tcp() {
    let pair = servers::start();
    pair.set_alt_svc(Some(&pair.h3_here("")));
    let dns = FakeDns::new();

    let first = selector(&pair, dns.clone());
    hop(&first, &pair, &uri(&pair), "h1").await;
    hop(&first, &pair, &uri(&pair), "h3").await;

    let second = selector(&pair, dns);
    hop(&second, &pair, &uri(&pair), "h1").await;
}

/// One `hclient::Client`, two requests, two protocols — the same thing a
/// browser does, from where a caller stands.
#[tokio::test(flavor = "multi_thread")]
async fn one_client_moves_itself_to_http_3_on_the_second_request() {
    let pair = servers::start();
    pair.set_alt_svc(Some(&pair.h3_here("; ma=86400")));
    let client = hclient::Client::builder(selector(&pair, FakeDns::new()))
        .build()
        .expect("the capabilities this transport reports are all honourable");

    for (expected, version) in [
        ("h1", http::Version::HTTP_11),
        ("h3", http::Version::HTTP_3),
        ("h3", http::Version::HTTP_3),
    ] {
        let resp = tokio::time::timeout(BOUND, client.get(&uri(&pair)).send())
            .await
            .expect("inside the bound")
            .expect("a response");
        assert_eq!(resp.version(), version);
        let text = resp
            .collect()
            .await
            .expect("a complete body")
            .text()
            .expect("utf-8");
        assert_eq!(text, expected);
    }
    assert_eq!(pair.tcp_answered(), 1);
    assert_eq!(pair.quic_answered(), 2);
}

// --- the fast tier is not overruled, and is not made to pay -------------

/// **The ordering, and it is the rule that keeps the two tiers apart.** An
/// origin that publishes a first-ranked record without `h3` has asked for
/// TCP — RFC 9460 §2.4.2 makes priority the operator's preference order —
/// and an advertisement heard on an earlier request does not overrule a
/// record fetched for this one.
///
/// The cache is populated first, over an origin with no record, and only
/// then does the resolver start answering. A transport that consulted the
/// cache before the record would send the second request over QUIC.
#[tokio::test(flavor = "multi_thread")]
async fn a_record_that_does_not_offer_h3_is_not_overruled_by_an_advertisement() {
    let pair = servers::start();
    pair.set_alt_svc(Some(&pair.h3_here("; ma=86400")));

    // Two resolvers, one transport: the first has no record to give, so
    // the advertisement is what decides; the second publishes one that
    // ranks HTTP/1.1 first.
    let silent = FakeDns::new();
    let t = selector(&pair, silent);
    hop(&t, &pair, &uri(&pair), "h1").await;
    hop(&t, &pair, &uri(&pair), "h3").await;

    let publishing = FakeDns::with_records(vec![service_record(1, &[b"http/1.1"])]);
    let t2 = selector(&pair, publishing);
    // The second transport has heard nothing yet, so this hop populates
    // *its* cache while the record sends it to TCP…
    hop(&t2, &pair, &uri(&pair), "h1").await;
    // …and this one is the assertion: the entry is there, the record says
    // otherwise, and the record wins.
    hop(&t2, &pair, &uri(&pair), "h1").await;
}

/// The same shape from the other side: a record that *does* offer `h3`
/// chooses QUIC on the very first request, with no advertisement anywhere
/// — the fast tier is untouched.
#[tokio::test(flavor = "multi_thread")]
async fn a_record_offering_h3_still_chooses_quic_on_the_first_request() {
    let pair = servers::start();
    let dns = FakeDns::with_records(vec![service_record(1, &[b"h3"])]);
    let t = selector(&pair, dns.clone());

    hop(&t, &pair, &uri(&pair), "h3").await;
    assert_eq!(dns.svcb_lookups(), 1, "one query, as before this tier");
}

// --- what the origin can take back --------------------------------------

/// RFC 7838 §3's `clear`, over the connection it moved the client onto.
/// Request 3 is back on TCP.
///
/// The withdrawal is sent by the **QUIC** server, which is the only place
/// it can come from once a client has moved: an origin able to withdraw an
/// advertisement only over TCP could not withdraw one at all.
#[tokio::test(flavor = "multi_thread")]
async fn clear_over_quic_sends_the_origin_back_to_tcp() {
    let pair = servers::start();
    pair.set_alt_svc(Some(&pair.h3_here("; ma=86400")));
    let t = selector(&pair, FakeDns::new());

    hop(&t, &pair, &uri(&pair), "h1").await;
    pair.set_alt_svc(Some("clear"));
    hop(&t, &pair, &uri(&pair), "h3").await;
    hop(&t, &pair, &uri(&pair), "h1").await;
}

/// `ma=0` is the same withdrawal by a different route, and the route
/// matters: `clear` travels through the parser's `Clear` arm and this
/// travels through an entry whose window closes at the instant it opens.
#[tokio::test(flavor = "multi_thread")]
async fn ma_zero_sends_the_origin_back_to_tcp() {
    let pair = servers::start();
    pair.set_alt_svc(Some(&pair.h3_here("; ma=86400")));
    let t = selector(&pair, FakeDns::new());

    hop(&t, &pair, &uri(&pair), "h1").await;
    pair.set_alt_svc(Some(&pair.h3_here("; ma=0")));
    hop(&t, &pair, &uri(&pair), "h3").await;
    hop(&t, &pair, &uri(&pair), "h1").await;
}

/// `clear` in a field that also advertises `h3` still clears — RFC 7838
/// §3 decides this exact case: *"including those specified in the same
/// response, in case of an invalid reply containing both 'clear' and
/// alternative services"*.
///
/// This is the one arm where `clear` is behaviourally distinguishable from
/// a field that merely offers no `h3`: on its own it removes the entry,
/// and so does an empty list, so only a reply carrying both can tell a
/// parser that honours `clear` from one that drops it as a member it could
/// not read.
#[tokio::test(flavor = "multi_thread")]
async fn clear_beside_an_advertisement_still_clears() {
    let pair = servers::start();
    pair.set_alt_svc(Some(&pair.h3_here("; ma=86400")));
    let t = selector(&pair, FakeDns::new());

    hop(&t, &pair, &uri(&pair), "h1").await;
    pair.set_alt_svc(Some(&format!("{}, clear", pair.h3_here("; ma=86400"))));
    hop(&t, &pair, &uri(&pair), "h3").await;
    hop(&t, &pair, &uri(&pair), "h1").await;
}

/// RFC 9110 §5.3 makes several `Alt-Svc` field lines one comma-joined
/// list, so a client that read only the first would miss what the second
/// said.
///
/// Two lines, and the `h3` is in the **second** one, so a client that read
/// only the first would stay on TCP. The advertisement is withdrawn before
/// the QUIC hop, because a repeated field line is an HTTP/1.1 wire shape
/// this fixture writes by hand and there is no such thing to write over
/// HTTP/3 — the second hop's job here is only to say which stack the first
/// hop's field sent it to.
#[tokio::test(flavor = "multi_thread")]
async fn every_repeated_field_line_is_read() {
    let pair = servers::start();
    let t = selector(&pair, FakeDns::new());

    pair.set_alt_svc(Some(&format!(
        "h2=\":443\"\r\nalt-svc: {}",
        pair.h3_here("; ma=86400")
    )));
    hop(&t, &pair, &uri(&pair), "h1").await;
    pair.set_alt_svc(None);
    hop(&t, &pair, &uri(&pair), "h3").await;
}

/// …and a `clear` on one line beats an `h3` on another, which is §3's rule
/// applied across lines rather than within one.
#[tokio::test(flavor = "multi_thread")]
async fn a_clear_on_one_line_beats_an_advertisement_on_another() {
    let pair = servers::start();
    pair.set_alt_svc(Some(&pair.h3_here("; ma=86400")));
    let t = selector(&pair, FakeDns::new());

    hop(&t, &pair, &uri(&pair), "h1").await;
    // Back to TCP first, so the line below is heard over the wire that can
    // carry two of them.
    pair.set_alt_svc(Some("clear"));
    hop(&t, &pair, &uri(&pair), "h3").await;
    hop(&t, &pair, &uri(&pair), "h1").await;

    pair.set_alt_svc(Some(&format!(
        "clear\r\nalt-svc: {}",
        pair.h3_here("; ma=86400")
    )));
    hop(&t, &pair, &uri(&pair), "h1").await;
    hop(&t, &pair, &uri(&pair), "h1").await;
}

/// RFC 7838 §3: *"its value invalidates and replaces all cached
/// alternative services for that origin."* A field that advertises
/// something else takes the `h3` entry with it, without saying `clear`.
#[tokio::test(flavor = "multi_thread")]
async fn a_later_field_that_offers_no_h3_replaces_the_entry() {
    let pair = servers::start();
    pair.set_alt_svc(Some(&pair.h3_here("; ma=86400")));
    let t = selector(&pair, FakeDns::new());

    hop(&t, &pair, &uri(&pair), "h1").await;
    pair.set_alt_svc(Some(r#"h2=":443"; ma=86400"#));
    hop(&t, &pair, &uri(&pair), "h3").await;
    hop(&t, &pair, &uri(&pair), "h1").await;
}

/// A response with **no** field at all is not an instruction and changes
/// nothing — which is the pair to the test above, and the reason `note` is
/// not called when the header is missing.
#[tokio::test(flavor = "multi_thread")]
async fn a_response_with_no_field_leaves_the_entry_alone() {
    let pair = servers::start();
    pair.set_alt_svc(Some(&pair.h3_here("; ma=86400")));
    let t = selector(&pair, FakeDns::new());

    hop(&t, &pair, &uri(&pair), "h1").await;
    pair.set_alt_svc(None);
    hop(&t, &pair, &uri(&pair), "h3").await;
    hop(&t, &pair, &uri(&pair), "h3").await;
}

// --- what cannot be acted on --------------------------------------------

/// An alternative at another authority is understood and not acted on.
///
/// RFC 7838 §2 keeps the `Host` header the origin's, so honouring this
/// would mean connecting to one authority while the request names another
/// — and `Transport::execute` has nowhere to say that. The advertisement
/// here names a port that is *almost* this origin's, so the only thing
/// separating it from an actionable one is the rule under test.
#[tokio::test(flavor = "multi_thread")]
async fn an_alternative_at_another_authority_is_not_acted_on() {
    let pair = servers::start();
    let elsewhere = pair.port.wrapping_add(1).max(1);
    let t = selector(&pair, FakeDns::new());

    for field in [
        format!("h3=\":{elsewhere}\"; ma=86400"),
        format!("h3=\"other.example:{}\"; ma=86400", pair.port),
    ] {
        pair.set_alt_svc(Some(&field));
        hop(&t, &pair, &uri(&pair), "h1").await;
        hop(&t, &pair, &uri(&pair), "h1").await;
    }
}

/// A field nobody can parse leaves the origin where it was. The parser has
/// its own file; this is the one arm that says a garbled field cannot move
/// a request onto QUIC.
#[tokio::test(flavor = "multi_thread")]
async fn a_field_that_does_not_parse_moves_nothing() {
    let pair = servers::start();
    let t = selector(&pair, FakeDns::new());

    for field in ["h3", "h3=", r#"h3=":0""#, "!!!", "h3=:443", "clear=1"] {
        pair.set_alt_svc(Some(field));
        hop(&t, &pair, &uri(&pair), "h1").await;
        hop(&t, &pair, &uri(&pair), "h1").await;
    }
}

/// An advertisement for a protocol this transport does not choose between
/// is not an advertisement for `h3`.
#[tokio::test(flavor = "multi_thread")]
async fn an_advertisement_for_another_protocol_moves_nothing() {
    let pair = servers::start();
    let t = selector(&pair, FakeDns::new());

    pair.set_alt_svc(Some(&format!(
        "h2=\":{}\"; ma=86400, h3-29=\":{}\"; ma=86400",
        pair.port, pair.port
    )));
    hop(&t, &pair, &uri(&pair), "h1").await;
    hop(&t, &pair, &uri(&pair), "h1").await;
}

// --- an IP literal, which has an origin but no name ----------------------

/// An IP literal is skipped by the **fast** tier because it has no name to
/// look up, and that reason is about DNS rather than about QUIC — so the
/// slow tier does serve it, and costs no query to do so.
#[tokio::test(flavor = "multi_thread")]
async fn an_ip_literal_has_no_record_to_ask_for_but_can_still_be_advertised_to() {
    let pair = servers::start();
    pair.set_alt_svc(Some(&pair.h3_here("; ma=86400")));
    let dns = FakeDns::with_records(vec![service_record(1, &[b"h3"])]);
    let t = selector(&pair, dns.clone());
    let literal = format!("https://127.0.0.1:{}/hello", pair.port);

    hop(&t, &pair, &literal, "h1").await;
    hop(&t, &pair, &literal, "h3").await;
    assert_eq!(
        dns.svcb_lookups(),
        0,
        "a literal is still not a name to ask about, on either hop"
    );
}

// --- the network change, which this crate cannot see for itself ---------

/// RFC 7838 §2.2, and the scope decision at the type: the caller reports
/// the event, because a `Transport` cannot see it.
#[tokio::test(flavor = "multi_thread")]
async fn a_reported_network_change_sends_the_origin_back_to_tcp() {
    let pair = servers::start();
    pair.set_alt_svc(Some(&pair.h3_here("; ma=86400")));
    let t = selector(&pair, FakeDns::new());

    hop(&t, &pair, &uri(&pair), "h1").await;
    // Stop advertising, so the hop below cannot quietly re-learn what the
    // network change is supposed to have taken away.
    pair.set_alt_svc(None);
    hop(&t, &pair, &uri(&pair), "h3").await;

    t.network_changed();
    hop(&t, &pair, &uri(&pair), "h1").await;
}

/// …and `persist=1` is the origin's hint that this one is not specific to
/// the access network, so it survives. Without this arm,
/// `network_changed` clearing everything unconditionally would look
/// correct.
#[tokio::test(flavor = "multi_thread")]
async fn a_persistent_advertisement_survives_a_reported_network_change() {
    let pair = servers::start();
    pair.set_alt_svc(Some(&pair.h3_here("; ma=86400; persist=1")));
    let t = selector(&pair, FakeDns::new());

    hop(&t, &pair, &uri(&pair), "h1").await;
    pair.set_alt_svc(None);
    hop(&t, &pair, &uri(&pair), "h3").await;

    t.network_changed();
    hop(&t, &pair, &uri(&pair), "h3").await;
}

// --- and the field is not eaten on the way past -------------------------

/// The response reaches the caller with its `Alt-Svc` field intact. This
/// transport reads the header; it does not consume it, and a caller
/// inspecting its own response is entitled to see what the origin sent.
#[tokio::test(flavor = "multi_thread")]
async fn the_field_is_read_and_not_removed() {
    let pair = servers::start();
    let field = pair.h3_here("; ma=86400");
    pair.set_alt_svc(Some(&field));
    let t = selector(&pair, FakeDns::new());

    let req = http::Request::builder()
        .uri(uri(&pair))
        .body(RequestBody::Empty)
        .expect("a well-formed request");
    let resp = tokio::time::timeout(BOUND, t.execute(req))
        .await
        .expect("inside the bound")
        .expect("the TCP server answered");

    assert_eq!(
        resp.headers().get("alt-svc").and_then(|v| v.to_str().ok()),
        Some(field.as_str())
    );
}
