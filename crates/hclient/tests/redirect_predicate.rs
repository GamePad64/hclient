//! `ClientBuilder::redirect_predicate`, watched from the server's side.
//!
//! Every assertion here counts **requests a server received**, because the
//! failure mode this feature exists against is a rule that is written down
//! and never consulted — and a predicate that is never called is
//! indistinguishable from one that always answers `Follow` unless
//! something outside the client is counting.
#![cfg(all(feature = "test-util", not(target_family = "wasm")))]

use hclient::Client;
use hclient::error::RedirectRefused;
use hclient::mock::MockTransport;
use hclient::redirect::RedirectVerdict;
use std::sync::{Arc, Mutex};

/// A transport that answers `302` to `next` and then `200`.
fn hop_to(next: &'static str) -> MockTransport {
    let t = MockTransport::new();
    t.push_response(
        http::Response::builder()
            .status(302)
            .header("location", next)
            .body("")
            .unwrap(),
    );
    t.push_response(
        http::Response::builder()
            .status(200)
            .body("arrived")
            .unwrap(),
    );
    t
}

fn go(c: &Client) -> Result<hclient::Collected, hclient_core::Error> {
    futures_executor::block_on(async { c.get("https://a.test/one").send().await?.collect().await })
}

/// **The three verdicts, on one fixture, asserted by what the server
/// saw.** Any two of them would be satisfied by a predicate that was never
/// called: `Follow` looks like no predicate at all, and `Stop` looks like
/// `RedirectPolicy::None`. It is the third that cannot be faked, and it is
/// the triple that says the closure's answer is what decides.
#[test]
fn each_verdict_decides_the_hop_and_the_server_sees_the_difference() {
    // Follow: two requests, the second hop's body is the answer.
    let c = Client::builder(hop_to("https://a.test/two"))
        .redirect_predicate(|_| RedirectVerdict::Follow)
        .build()
        .expect("build");
    assert_eq!(go(&c).expect("follows").text().unwrap(), "arrived");
    assert_eq!(
        c.transport_as::<MockTransport>()
            .expect("the mock")
            .requests()
            .len(),
        2
    );

    // Stop: one request, and the 3xx is the caller's answer.
    let c = Client::builder(hop_to("https://a.test/two"))
        .redirect_predicate(|_| RedirectVerdict::Stop)
        .build()
        .expect("build");
    let got = go(&c).expect("a 3xx is an answer, not a failure");
    assert_eq!(got.status(), 302);
    assert_eq!(
        got.headers()["location"],
        "https://a.test/two",
        "and its Location reaches the caller, who is the one deciding"
    );
    assert_eq!(
        c.transport_as::<MockTransport>()
            .expect("the mock")
            .requests()
            .len(),
        1,
        "the hop was not taken"
    );

    // Refuse: one request, and a typed error naming the hop.
    let c = Client::builder(hop_to("https://a.test/two"))
        .redirect_predicate(|_| RedirectVerdict::Refuse)
        .build()
        .expect("build");
    let err = go(&c).expect_err("a refusal is a failure to reach an answer");
    assert_eq!(*err.kind(), hclient_core::ErrorKind::Redirect, "{err:?}");
    let refused = std::error::Error::source(&err)
        .and_then(|s| s.downcast_ref::<RedirectRefused>())
        .unwrap_or_else(|| panic!("the typed refusal: {err:?}"));
    assert_eq!(refused.to, "https://a.test/two");
    assert_eq!(refused.status, 302);
    assert_eq!(
        c.transport_as::<MockTransport>()
            .expect("the mock")
            .requests()
            .len(),
        1
    );
}

/// **The hop is described as it would go out**, not as the server wrote
/// it: a relative `Location` is already resolved, and `cross_origin` is
/// the same value that drives the credential stripping rather than one
/// computed again beside it.
#[test]
fn the_hop_carries_the_resolved_target_and_the_origin_answer() {
    /// What one question looked like: target, cross-origin, hop count,
    /// status.
    #[derive(Debug, PartialEq, Eq)]
    struct Asked {
        to: String,
        cross_origin: bool,
        hops: u8,
        status: u16,
    }
    let seen: Arc<Mutex<Vec<Asked>>> = Arc::new(Mutex::new(Vec::new()));
    let rec = Arc::clone(&seen);
    // A relative Location, and a same-origin one: both halves are wrong
    // in a different way if the hop were built from the raw header.
    let c = Client::builder(hop_to("/two"))
        .redirect_predicate(move |hop| {
            rec.lock().unwrap().push(Asked {
                to: hop.to().to_string(),
                cross_origin: hop.cross_origin(),
                hops: hop.hops(),
                status: hop.status().as_u16(),
            });
            RedirectVerdict::Follow
        })
        .build()
        .expect("build");
    assert_eq!(go(&c).expect("follows").text().unwrap(), "arrived");

    let seen = seen.lock().unwrap();
    assert_eq!(seen.len(), 1, "one 3xx, one question");
    assert_eq!(
        seen[0].to, "https://a.test/two",
        "resolved against the hop it came from, not handed over as `/two`"
    );
    assert!(
        !seen[0].cross_origin,
        "same host, scheme and port: nothing is stripped"
    );
    assert_eq!(seen[0].hops, 0, "the first 3xx of the chain");
    assert_eq!(seen[0].status, 302);
}

/// **`cross_origin` is the stripping's own answer.** A hop to another host
/// reports `true`, which is what an SSRF or credential guard reads — and
/// it agrees with the `Authorization` that is about to be dropped rather
/// than being a second opinion about what an origin is.
#[test]
fn a_hop_to_another_host_reports_the_origin_change() {
    let seen = Arc::new(Mutex::new(Vec::<bool>::new()));
    let rec = Arc::clone(&seen);
    let c = Client::builder(hop_to("https://b.test/two"))
        .redirect_predicate(move |hop| {
            rec.lock().unwrap().push(hop.cross_origin());
            RedirectVerdict::Follow
        })
        .build()
        .expect("build");
    assert_eq!(go(&c).expect("follows").text().unwrap(), "arrived");
    assert_eq!(*seen.lock().unwrap(), vec![true]);

    // The control, and it is what makes the value mean "origin" rather
    // than "the path changed": a default port written out explicitly is
    // the same origin.
    let seen = Arc::new(Mutex::new(Vec::<bool>::new()));
    let rec = Arc::clone(&seen);
    let c = Client::builder(hop_to("https://a.test:443/two"))
        .redirect_predicate(move |hop| {
            rec.lock().unwrap().push(hop.cross_origin());
            RedirectVerdict::Follow
        })
        .build()
        .expect("build");
    assert_eq!(go(&c).expect("follows").text().unwrap(), "arrived");
    assert_eq!(*seen.lock().unwrap(), vec![false]);
}

/// **Asked on every hop, and the count is what says so.** A predicate
/// consulted once and remembered would pass every test above.
#[test]
fn every_hop_is_asked_and_the_answers_are_independent() {
    let t = MockTransport::new();
    for n in ["/two", "/three"] {
        t.push_response(
            http::Response::builder()
                .status(302)
                .header("location", n)
                .body("")
                .unwrap(),
        );
    }
    t.push_response(
        http::Response::builder()
            .status(200)
            .body("arrived")
            .unwrap(),
    );

    let asked = Arc::new(Mutex::new(Vec::<String>::new()));
    let rec = Arc::clone(&asked);
    let c = Client::builder(t)
        .redirect_predicate(move |hop| {
            rec.lock().unwrap().push(hop.to().path().to_owned());
            // The second hop is refused, so the answers are visibly not
            // one answer reused.
            if hop.to().path() == "/three" {
                RedirectVerdict::Refuse
            } else {
                RedirectVerdict::Follow
            }
        })
        .build()
        .expect("build");
    let err = go(&c).expect_err("the second hop is refused");
    assert_eq!(*err.kind(), hclient_core::ErrorKind::Redirect, "{err:?}");
    assert_eq!(*asked.lock().unwrap(), vec!["/two", "/three"]);
    assert_eq!(
        c.transport_as::<MockTransport>()
            .expect("the mock")
            .requests()
            .len(),
        2,
        "the third request was never sent"
    );
}

/// **A response that is not a followable redirect never reaches the
/// predicate**, so switching one on cannot lengthen a chain or turn a
/// plain answer into a question. `304` is the sharp case: it is a `3xx`
/// that `decide` deliberately does not follow.
#[test]
fn a_response_the_policy_would_not_follow_is_never_asked_about() {
    for (status, location) in [(200u16, None), (304, Some("/two")), (302, None)] {
        let t = MockTransport::new();
        let mut b = http::Response::builder().status(status);
        if let Some(l) = location {
            b = b.header("location", l);
        }
        t.push_response(b.body("body").unwrap());

        let asked = Arc::new(Mutex::new(0usize));
        let rec = Arc::clone(&asked);
        let c = Client::builder(t)
            .redirect_predicate(move |_| {
                *rec.lock().unwrap() += 1;
                RedirectVerdict::Refuse
            })
            .build()
            .expect("build");
        assert_eq!(
            go(&c)
                .unwrap_or_else(|e| panic!("{status}: {e:?}"))
                .status(),
            status,
            "{status}: the response is the answer"
        );
        assert_eq!(
            *asked.lock().unwrap(),
            0,
            "{status}: nothing was going to be followed, so nothing was asked"
        );
    }
}

/// **The policy is asked first**, so a predicate cannot resurrect a hop
/// the policy refused — `RedirectPolicy::None` with a `Follow`-answering
/// predicate still stops.
#[test]
fn the_policy_decides_before_the_predicate_and_cannot_be_overruled() {
    let asked = Arc::new(Mutex::new(0usize));
    let rec = Arc::clone(&asked);
    let c = Client::builder(hop_to("https://a.test/two"))
        .redirect(hclient::redirect::RedirectPolicy::None)
        .redirect_predicate(move |_| {
            *rec.lock().unwrap() += 1;
            RedirectVerdict::Follow
        })
        .build()
        .expect("build");
    assert_eq!(go(&c).expect("stops").status(), 302);
    assert_eq!(
        c.transport_as::<MockTransport>()
            .expect("the mock")
            .requests()
            .len(),
        1
    );
    assert_eq!(*asked.lock().unwrap(), 0, "the policy had already said no");
}

/// **A predicate against a transport that follows redirects itself is
/// refused at `build()`**, naming its own setting rather than
/// `redirect_policy`.
#[test]
fn a_predicate_against_an_internally_redirecting_backend_is_refused() {
    let mut caps = hclient::caps::Capabilities::none();
    caps.redirects = hclient_core::RedirectSupport::Internal;
    let err = Client::builder(MockTransport::new().with_capabilities(caps))
        .redirect_predicate(|_| RedirectVerdict::Follow)
        .build()
        .expect_err("the browser has already followed by the time we see anything");
    assert_eq!(
        err.what, "redirect_predicate",
        "a caller who wrote a predicate must not be told the policy was the problem: {err}"
    );

    // The control: the same builder against a transport that follows
    // nothing itself builds.
    assert!(
        Client::builder(MockTransport::new())
            .redirect_predicate(|_| RedirectVerdict::Follow)
            .build()
            .is_ok()
    );
}

/// The predicate does not cost the client its auto traits — the whole
/// subject of the `Send + Sync` on the setter (amendment C12).
#[test]
fn a_client_with_a_predicate_still_crosses_a_spawn() {
    fn assert_send_sync<T: Send + Sync>(_: &T) {}
    let c = Client::builder(MockTransport::new())
        .redirect_predicate(|_| RedirectVerdict::Follow)
        .build()
        .expect("build");
    assert_send_sync(&c);
}
