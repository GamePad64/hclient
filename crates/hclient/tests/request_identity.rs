//! One `RequestId` per operation, and hop and resend counters beside it.
//!
//! The identity exists so that a transport decorator and a connection hook
//! can join on a key, and so that `http.request.resend_count` is a field
//! something reads rather than a number nothing can compute. Both of those
//! rest on one property: **every send of one operation carries the same id,
//! and the pair `(hop, resend)` says which send it is.**
//!
//! These tests read the extension at the transport boundary, which is the
//! only place the answer is observable — exactly as the four `AllowEarlyData`
//! tests do, and for the same reason.

#![cfg(feature = "test-util")]

use hclient::mock::MockTransport;
use hclient_core::unversioned::Attempt;

/// Every attempt this transport was handed, in order.
fn attempts(t: &MockTransport) -> Vec<Attempt> {
    t.requests()
        .iter()
        .map(|r| {
            *r.extensions
                .get::<Attempt>()
                .expect("every send carries the identity")
        })
        .collect()
}

fn client() -> hclient::Client {
    hclient::Client::builder(MockTransport::new())
        .build()
        .expect("mock supports the default config")
}

/// The plain case, and the two facts it pins are the ones everything else
/// rests on: a send carries an identity at all, and it starts at the first
/// hop and the first send.
#[test]
fn one_send_carries_the_first_hop_and_the_first_send() {
    let c = client();
    c.transport_as::<MockTransport>()
        .expect("the mock")
        .push_response(http::Response::builder().status(200).body("ok").unwrap());

    let _ = futures_executor::block_on(c.get("https://a/").send()).expect("response");

    let seen = attempts(c.transport_as::<MockTransport>().expect("the mock"));
    assert_eq!(seen.len(), 1);
    assert_eq!((seen[0].hop, seen[0].resend), (0, 0));
    assert_ne!(
        seen[0].id,
        hclient_core::unversioned::RequestId::UNIDENTIFIED,
        "a request that went through `Client` is identified"
    );
}

/// **A redirect chain is one operation.** The id must survive the hop, and
/// `hop` must move — the pair is what makes a span joinable across a chain
/// rather than three unrelated spans.
#[test]
fn a_redirect_chain_keeps_one_id_and_counts_the_hops() {
    let c = hclient::Client::builder(MockTransport::new())
        .redirect(hclient::redirect::Limit::new(10))
        .build()
        .expect("mock supports the default config");
    let m = c.transport_as::<MockTransport>().expect("the mock");
    m.push_response(
        http::Response::builder()
            .status(302)
            .header("location", "https://a/two")
            .body("")
            .unwrap(),
    );
    m.push_response(
        http::Response::builder()
            .status(302)
            .header("location", "https://a/three")
            .body("")
            .unwrap(),
    );
    m.push_response(http::Response::builder().status(200).body("ok").unwrap());

    let _ = futures_executor::block_on(c.get("https://a/one").send()).expect("response");

    let seen = attempts(c.transport_as::<MockTransport>().expect("the mock"));
    assert_eq!(seen.len(), 3, "three hops");
    assert!(
        seen.iter().all(|a| a.id == seen[0].id),
        "one operation, one id: {seen:?}"
    );
    assert_eq!(
        seen.iter().map(|a| a.hop).collect::<Vec<_>>(),
        vec![0, 1, 2],
        "the hop counter moves with the chain"
    );
    assert!(
        seen.iter().all(|a| a.resend == 0),
        "no hop was sent twice, so nothing resent: {seen:?}"
    );
}

/// **The distinction the whole design exists for.** `otel-design.md` says
/// hop 2 of a chain and attempt 2 of a retry are indistinguishable from
/// below; this is the assertion that they are not, now. A `425` replay is
/// a second send of the *same* hop, so `resend` moves and `hop` does not —
/// the exact opposite of the test above, over the same transport.
#[test]
fn a_replay_moves_the_resend_counter_and_not_the_hop() {
    let c = client();
    let m = c.transport_as::<MockTransport>().expect("the mock");
    m.push_response(
        http::Response::builder()
            .status(425)
            .body("too early")
            .unwrap(),
    );
    m.push_response(http::Response::builder().status(200).body("ok").unwrap());

    let _ = futures_executor::block_on(c.get("https://a/").send()).expect("response");

    let seen = attempts(c.transport_as::<MockTransport>().expect("the mock"));
    assert_eq!(seen.len(), 2, "sent once, replayed once");
    assert_eq!(seen[0].id, seen[1].id, "one operation");
    assert_eq!(
        (seen[0].hop, seen[0].resend),
        (0, 0),
        "the first send of the first hop"
    );
    assert_eq!(
        (seen[1].hop, seen[1].resend),
        (0, 1),
        "the same hop, sent again — not hop 1"
    );
}

/// Two operations through one client do not share an id, which is what
/// makes the id a key rather than a constant.
#[test]
fn two_operations_get_two_ids() {
    let c = client();
    let m = c.transport_as::<MockTransport>().expect("the mock");
    m.push_response(http::Response::builder().status(200).body("a").unwrap());
    m.push_response(http::Response::builder().status(200).body("b").unwrap());

    let _ = futures_executor::block_on(c.get("https://a/").send()).expect("first");
    let _ = futures_executor::block_on(c.get("https://a/").send()).expect("second");

    let seen = attempts(c.transport_as::<MockTransport>().expect("the mock"));
    assert_eq!(seen.len(), 2);
    assert_ne!(seen[0].id, seen[1].id, "two operations, two identities");
}

/// **An authentication leg is a resend too**, and it is the third send
/// site — the retry loop, the `425` replay and this one, each building its
/// own `HopParts`. The mutation that removed the identity from this branch
/// alone survived the four tests above, which is what says the branch
/// needs its own.
#[cfg(feature = "digest-auth")]
#[test]
fn an_authentication_leg_moves_the_resend_counter_and_not_the_hop() {
    let c = client();
    let m = c.transport_as::<MockTransport>().expect("the mock");
    m.push_response(
        http::Response::builder()
            .status(401)
            .header(
                "www-authenticate",
                "Digest realm=\"t\", nonce=\"n\", qop=\"auth\", algorithm=SHA-256",
            )
            .body("")
            .unwrap(),
    );
    m.push_response(http::Response::builder().status(200).body("ok").unwrap());

    let _ = futures_executor::block_on(c.get("https://a/").digest_auth("u", "p").send())
        .expect("the second request succeeds");

    let seen = attempts(c.transport_as::<MockTransport>().expect("the mock"));
    assert_eq!(seen.len(), 2, "challenged once, answered once");
    assert_eq!(seen[0].id, seen[1].id, "one operation");
    assert_eq!((seen[0].hop, seen[0].resend), (0, 0));
    assert_eq!(
        (seen[1].hop, seen[1].resend),
        (0, 1),
        "the same hop, sent again with credentials — not a redirect"
    );
}
