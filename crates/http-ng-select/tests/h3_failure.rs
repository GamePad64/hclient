//! The negative half, watched from the servers' side of the wire.
//!
//! `tests/alt_svc.rs` shows an origin talking this client *into* HTTP/3.
//! This file shows what happens when the client believes it and QUIC does
//! not answer — which before the staged connect could not be built at all,
//! because the fallback would have been a request-level retry with a
//! `RequestBody::retry_kind()` condition on it
//! (`docs/v04-w1-acceptance.md` §9.3, blocker 1).
//!
//! # What the counters have to tell apart
//!
//! *"The second request did not go to QUIC"* and *"QUIC was never chosen"*
//! look identical on a counter of answered requests, because a refused
//! handshake answers nothing either way. So the fixture counts **connection
//! attempts that reached the QUIC endpoint** — `quic_attempted`, bumped
//! before the handshake is awaited — and every test below reads it as a
//! delta across each hop. A hop that tried and failed moves it; a hop the
//! memory held back does not.
//!
//! # The failure is a refusal and not a black hole, deliberately
//!
//! §9.3's second blocker was that *"loopback cannot produce the
//! failure … the arm under test would be a multi-second handshake
//! timeout — a clock-driven assertion, which is the shape three flakes in
//! this workspace already came from."* It is the right worry about the
//! wrong premise: what the memory records is *"the connect failed"*, and it
//! does not read the reason. A real `quinn` server offering an ALPN this
//! client will not accept fails **causally**, in one round trip, and
//! produces exactly that fact. The black hole is still used, once, where a
//! test needs a bound to be *spent* rather than a connect to fail.
//!
//! What that leaves unverified is written down rather than implied: nothing
//! here measures the 30 s a real UDP block costs. That number is
//! `docs/v04-w1-acceptance.md` §7.3's, it is quinn's `max_idle_timeout`
//! rather than anything of ours, and it is a `Timeouts::connect` question.
#![cfg(not(target_family = "wasm"))]

mod fakedns;
mod servers;

use fakedns::{FakeDns, service_record};
use http_body_util::BodyExt;
use http_ng_core::unversioned::Transport;
use http_ng_core::{Error, ErrorKind, Phase, RequestBody, Timeouts};
use http_ng_h3::H3;
use http_ng_native::Native;
use http_ng_rt_tokio::TokioHandle;
use http_ng_select::Selecting;
use servers::{ORIGIN, Pair, Quic};
use std::time::Duration;

/// Never an assertion — it turns a mutation that hangs into a red test
/// rather than an eternal one.
const BOUND: Duration = Duration::from_secs(20);

type Selector = Selecting<TokioHandle, http_ng_tls_rustls::Rustls, FakeDns>;

fn selector(pair: &Pair, dns: FakeDns) -> Selector {
    let rt = TokioHandle::current().expect("inside #[tokio::test]");
    Selecting::new(
        rt.clone(),
        Native::new(rt.clone(), servers::client_tls(&pair.cert_der), dns.clone()),
        H3::new(rt, servers::client_tls(&pair.cert_der), dns.clone()).expect("H3::new does no I/O"),
        dns,
    )
    .expect("the two stacks agree")
}

fn uri(pair: &Pair) -> String {
    format!("https://{ORIGIN}:{}/hello", pair.port)
}

fn request(pair: &Pair, timeouts: Option<Timeouts>) -> http::Request<RequestBody> {
    let mut req = http::Request::builder()
        .uri(uri(pair))
        .body(RequestBody::Empty)
        .expect("a well-formed request");
    if let Some(t) = timeouts {
        req.extensions_mut().insert(t);
    }
    req
}

/// What one hop did, as the **servers** saw it, plus what the caller got.
#[derive(Debug)]
struct Hop {
    /// QUIC connection attempts that reached the endpoint during this hop.
    quic_tried: usize,
    /// HTTP/1.1 requests the TCP server answered during this hop.
    tcp_answered: usize,
    /// TCP connections the TCP server accepted during this hop — the
    /// counter that still moves for a request that arrives and is never
    /// answered, which is what an assertion about *not falling back* needs.
    tcp_accepted: usize,
    /// The body, which is each server's own word for itself, or the error.
    got: Result<String, Error>,
}

impl Hop {
    /// The body, or a panic naming the error — `http_ng_core::Error` is not
    /// `PartialEq`, and a hop that was expected to answer and did not
    /// should say why rather than say `false`.
    fn body(&self) -> &str {
        match &self.got {
            Ok(s) => s,
            Err(e) => panic!("expected one of the two servers to answer, got {e:?}"),
        }
    }
}

async fn hop(t: &Selector, pair: &Pair, req: http::Request<RequestBody>) -> Hop {
    let before = (
        pair.quic_attempted(),
        pair.tcp_answered(),
        pair.tcp_accepted(),
    );
    let got = match tokio::time::timeout(BOUND, t.execute(req))
        .await
        .expect("the request finished inside the bound")
    {
        Ok(resp) => {
            assert_eq!(resp.status(), 200);
            let bytes = resp
                .into_body()
                .collect()
                .await
                .expect("a complete body")
                .to_bytes();
            Ok(String::from_utf8(bytes.to_vec()).expect("utf-8"))
        }
        Err(e) => Err(e),
    };
    Hop {
        quic_tried: pair.quic_attempted() - before.0,
        tcp_answered: pair.tcp_answered() - before.1,
        tcp_accepted: pair.tcp_accepted() - before.2,
        got,
    }
}

/// An advertisement that survives a network change, so a test can clear the
/// *failure* memory without also clearing the thing that sends requests to
/// QUIC in the first place. RFC 7838 §3.1's `persist=1` is the origin's
/// own claim, and this fixture makes it.
fn persistent_h3(pair: &Pair) -> String {
    pair.h3_here("; ma=86400; persist=1")
}

// --- the deliverable ----------------------------------------------------

/// **The whole feature.** An origin advertises HTTP/3, the QUIC connect
/// fails, the request is served over TCP anyway — and the *next* request to
/// the same origin does not try QUIC at all.
///
/// Hop 2 is what makes hop 3 mean something: the QUIC endpoint saw an
/// attempt on hop 2 and none on hop 3, so "did not try" is a delta and not
/// an absence.
#[tokio::test(flavor = "multi_thread")]
async fn an_origin_whose_quic_connect_failed_is_not_tried_again() {
    let pair = servers::start_with_quic(Quic::Rejecting);
    pair.set_alt_svc(Some(&persistent_h3(&pair)));
    let t = selector(&pair, FakeDns::new());

    // 1. Nothing is known yet, so TCP — and the answer advertises h3.
    let one = hop(&t, &pair, request(&pair, None)).await;
    assert_eq!(
        (one.quic_tried, one.tcp_answered, one.body()),
        (0, 1, "h1"),
        "the first request to an unknown origin is TCP whatever the origin has to say"
    );

    // 2. Advertised, so QUIC is chosen — and refused. The request was never
    //    handed to the QUIC stack, so it goes to TCP with nothing to
    //    decide about idempotency.
    let two = hop(&t, &pair, request(&pair, None)).await;
    assert_eq!(
        (two.tcp_answered, two.body()),
        (1, "h1"),
        "a refused QUIC connect must not fail the request"
    );
    assert!(
        two.quic_tried >= 1,
        "this hop must actually have tried QUIC, or hop 3 proves nothing"
    );

    // Nothing more is said from here: a response with no field is not an
    // instruction, so the advertisement stands and hop 3 is still an origin
    // this client believes speaks HTTP/3.
    pair.set_alt_svc(None);

    // 3. The memory.
    let three = hop(&t, &pair, request(&pair, None)).await;
    assert_eq!(
        (three.quic_tried, three.tcp_answered, three.body()),
        (0, 1, "h1"),
        "the origin's QUIC must not be tried again inside the window"
    );
}

/// The veto covers the **fast** tier too, and that is the case it exists
/// for: an origin publishing an HTTPS record that lists `h3`, on a network
/// where QUIC does not get through, is the shape a suppression that only
/// covered `Alt-Svc` would leave paying for a failed connect on every
/// request.
///
/// No advertisement anywhere here — the record is the only thing sending
/// these requests to QUIC.
#[tokio::test(flavor = "multi_thread")]
async fn a_record_that_offers_h3_is_vetoed_by_a_failure_just_the_same() {
    let pair = servers::start_with_quic(Quic::Rejecting);
    let dns = FakeDns::with_records(vec![service_record(1, &[b"h3"])]);
    let t = selector(&pair, dns);

    let one = hop(&t, &pair, request(&pair, None)).await;
    assert_eq!(one.body(), "h1", "served over TCP after all");
    assert!(one.quic_tried >= 1, "the record sent it to QUIC first");

    let two = hop(&t, &pair, request(&pair, None)).await;
    assert_eq!(
        (two.quic_tried, two.tcp_answered, two.body()),
        (0, 1, "h1"),
        "the record still says h3 and the memory still says no"
    );
}

/// The scope decision, and the control for the two tests above: the
/// suppression is not permanent and the only thing that ends it early is
/// the caller saying the network moved.
///
/// It also shows what the veto does **not** do. The advertisement is still
/// there after two suppressed hops — a failure of ours is not evidence
/// against what the origin said — so the moment the memory is cleared the
/// very next request goes to QUIC again with nothing re-advertised in
/// between.
#[tokio::test(flavor = "multi_thread")]
async fn a_reported_network_change_lets_a_failed_origin_be_tried_again() {
    let pair = servers::start_with_quic(Quic::Rejecting);
    pair.set_alt_svc(Some(&persistent_h3(&pair)));
    let t = selector(&pair, FakeDns::new());

    hop(&t, &pair, request(&pair, None)).await; // TCP, hears the advertisement
    let two = hop(&t, &pair, request(&pair, None)).await; // tries QUIC, fails
    assert!(two.quic_tried >= 1);

    pair.set_alt_svc(None);
    let three = hop(&t, &pair, request(&pair, None)).await;
    assert_eq!(three.quic_tried, 0, "suppressed");

    t.network_changed();

    let four = hop(&t, &pair, request(&pair, None)).await;
    assert!(
        four.quic_tried >= 1,
        "a reported network change forgets every failure — and `persist=1` \
         kept the advertisement, so there is still something sending this \
         request to QUIC"
    );
    assert_eq!(
        four.body(),
        "h1",
        "and it fails again, and falls back again"
    );
}

// --- the budget ---------------------------------------------------------

/// **`Timeouts::connect` is one bound for one request, and a sequential
/// fallback is the plainest way to double it.**
///
/// The pair of arms is the assertion, and neither arm asserts on a
/// duration. Same bound, same fallback, same two servers; what differs is
/// how much of the bound the QUIC arm spends:
///
/// - a **refused** handshake spends ~1 ms, so there is a bound left to
///   connect with and the TCP arm runs;
/// - a **black hole** spends all of it, so there is nothing left, and the
///   honest answer is the QUIC arm's own `Timeout(Connect)` rather than a
///   second attempt the caller's bound has no room for.
///
/// A fallback handed a fresh copy of the bound passes the first arm and
/// fails the second, where the TCP server would accept a connection it must
/// not have been given time for.
#[tokio::test(flavor = "multi_thread")]
async fn the_fallback_spends_what_is_left_of_the_connect_bound_and_no_more() {
    let bound = Timeouts {
        connect: Some(Duration::from_millis(300)),
        ..Default::default()
    };

    // Arm A — the QUIC arm fails at once, so the bound has room.
    let pair = servers::start_with_quic(Quic::Rejecting);
    pair.set_alt_svc(Some(&persistent_h3(&pair)));
    let t = selector(&pair, FakeDns::new());
    hop(&t, &pair, request(&pair, None)).await;
    let a = hop(&t, &pair, request(&pair, Some(bound))).await;
    assert!(a.quic_tried >= 1);
    assert_eq!(
        (a.tcp_answered, a.body()),
        (1, "h1"),
        "a connect that failed in a millisecond leaves the bound with room for the fallback"
    );

    // Arm B — the QUIC arm spends the whole bound.
    let pair = servers::start_with_quic(Quic::BlackHole);
    pair.set_alt_svc(Some(&persistent_h3(&pair)));
    let t = selector(&pair, FakeDns::new());
    hop(&t, &pair, request(&pair, None)).await;
    let b = hop(&t, &pair, request(&pair, Some(bound))).await;
    assert_eq!(
        b.tcp_accepted, 0,
        "the caller's whole connect budget went on the QUIC arm, so the TCP \
         arm must not open a connection — a bound the fallback can double is not a bound"
    );
    let e = b.got.expect_err("nothing was left to connect with");
    assert_eq!(
        *e.kind(),
        ErrorKind::Timeout(Phase::Connect),
        "and the answer is the arm that spent the bound, not one of ours"
    );
}

// --- what a demand does -------------------------------------------------

/// A `RequireVersion(HTTP_3)` request is answered before the resolver, the
/// cache and this memory alike, and it does **not** fall back: a caller who
/// demanded HTTP/3 gets the connect error rather than a silent downgrade —
/// which would not even be silent, since `Native` refuses the demand and
/// the caller would read `VersionNotAvailable` in place of the real answer.
///
/// The failure is still recorded, because it is the same fact about the
/// origin however this request came to be making it — checked by the hop
/// after, which is an ordinary advertised request and is held back.
#[tokio::test(flavor = "multi_thread")]
async fn a_demand_for_http_3_does_not_fall_back_but_still_teaches_the_memory() {
    let pair = servers::start_with_quic(Quic::Rejecting);
    pair.set_alt_svc(Some(&persistent_h3(&pair)));
    let t = selector(&pair, FakeDns::new());

    let mut req = request(&pair, None);
    req.extensions_mut()
        .insert(http_ng_core::RequireVersion(http::Version::HTTP_3));
    let demanded = hop(&t, &pair, req).await;
    assert!(demanded.quic_tried >= 1);
    assert_eq!(
        demanded.tcp_accepted, 0,
        "a demand for HTTP/3 must not be answered over TCP"
    );
    assert_eq!(
        *demanded
            .got
            .expect_err("the QUIC connect was refused")
            .kind(),
        ErrorKind::Connect,
    );

    // An ordinary request now, with an advertisement in hand — and it is
    // held back, which is only possible if the demanded request's failure
    // was recorded.
    pair.set_alt_svc(Some(&persistent_h3(&pair)));
    let after = hop(&t, &pair, request(&pair, None)).await;
    assert_eq!(
        (after.quic_tried, after.body()),
        (0, "h1"),
        "the failure a demanded request met is the same fact about the origin"
    );

    // And the demand still outranks the memory, which is the other half of
    // the same rule: a demand is answered before the resolver and the cache
    // are asked, so it is answered before this transport's memory of
    // failures too. Without this hop the pair reads as "a demand happens to
    // go first"; with it, the two directions are one decision.
    let mut req = request(&pair, None);
    req.extensions_mut()
        .insert(http_ng_core::RequireVersion(http::Version::HTTP_3));
    let demanded_again = hop(&t, &pair, req).await;
    assert!(
        demanded_again.quic_tried >= 1,
        "a suppressed origin must still be tried when the caller demands HTTP/3"
    );
}

// --- what it costs ------------------------------------------------------

/// The fallback asks DNS no more than the hop it replaces.
///
/// At this fixture's port — an ephemeral one, because an unprivileged test
/// process cannot bind 443 — `http-ng-native` does no discovery of its own
/// (the record for a service away from the default port lives under a
/// prefixed name only `Selecting` constructs), so the second lookup a
/// fallback would otherwise pay is not made. **At an origin's default port
/// it would be 2 rather than 1**, and that row is inferred rather than
/// measured for the same reason `docs/v04-w1-acceptance.md` §9.6's is: the
/// record travelled with the request into the QUIC arm, and there is
/// deliberately no way to pair a record with a request it was not fetched
/// for. It is paid by the request that discovers the failure and by no
/// other, which is what the memory is for.
#[tokio::test(flavor = "multi_thread")]
async fn the_fallback_asks_no_more_of_dns_than_the_hop_it_replaces() {
    let pair = servers::start_with_quic(Quic::Rejecting);
    pair.set_alt_svc(Some(&persistent_h3(&pair)));
    let dns = FakeDns::new();
    let t = selector(&pair, dns.clone());

    let before = dns.svcb_lookups();
    hop(&t, &pair, request(&pair, None)).await;
    let plain = dns.svcb_lookups() - before;

    let before = dns.svcb_lookups();
    let two = hop(&t, &pair, request(&pair, None)).await;
    assert!(two.quic_tried >= 1, "this hop is the one that falls back");
    assert_eq!(
        dns.svcb_lookups() - before,
        plain,
        "the hop that fell back asked no more than the hop that did not"
    );
}
