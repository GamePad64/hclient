//! Runs `examples/portable.rs` — the port of `act`'s `http-client`
//! component — against `MockTransport`.
//!
//! The stop criterion for Task 10 is a *compile-time* one: three targets,
//! no `#[cfg]` in the example. That property is real but narrow — an
//! example that never streams, never sets a redirect limit and drops a
//! timeout would satisfy it just as well. These tests are what makes the
//! three green builds mean something: they pin the behaviours the example
//! claims to have ported, so the example cannot be quietly simplified into
//! something that still builds for three targets and no longer mirrors
//! anything.
//!
//! The example is pulled in with `#[path]` rather than copied. A second
//! copy of the logic would be a test of the copy.

// `http_ng::mock` lives behind the `test-util` feature (see `mock.rs`) —
// the same crate-level gate every other test in this directory carries.
#![cfg(feature = "test-util")]

// `dead_code`: the example's `main` has no caller here, and `Body::Raw`/
// `Body::Text` are exercised by the component's real callers rather than
// by these tests. The allow lives on this side of the `#[path]`, never in
// the example — nothing in `examples/portable.rs` may be shaped by the
// needs of a test.
#[path = "../examples/portable.rs"]
#[allow(dead_code)]
mod portable;

use http_ng::mock::MockTransport;
use http_ng::{Capabilities, Client, Error, ErrorKind, RedirectSupport, TimeoutSupport, Timeouts};
use portable::{Body, ComponentError, ContentSink, FetchArgs, fetch};
use std::collections::HashMap;
use std::time::Duration;

/// One `ActContext::send_content` call, recorded whole.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Sent {
    data: Vec<u8>,
    content_type: Option<String>,
    metadata: Vec<(String, Vec<u8>)>,
}

#[derive(Debug, Default)]
struct Recorder {
    sent: Vec<Sent>,
}

impl ContentSink for Recorder {
    fn send_content(
        &mut self,
        data: Vec<u8>,
        content_type: Option<String>,
        metadata: Vec<(String, Vec<u8>)>,
    ) {
        self.sent.push(Sent {
            data,
            content_type,
            metadata,
        });
    }
}

/// The component's defaults: GET, no headers, no body, no timeout, follow
/// redirects. Each test overrides exactly the field it is about.
fn args(url: &str) -> FetchArgs {
    FetchArgs {
        url: url.to_string(),
        method: http::Method::GET,
        headers: HashMap::new(),
        body: None,
        timeout_ms: None,
        follow_redirects: true,
    }
}

/// A backend that hands 3xx responses to us, the way `wasi:http` does —
/// `Capabilities::none()` reports `RedirectSupport::None`, which is not
/// the same claim.
fn transparent_mock() -> MockTransport {
    let mut caps = Capabilities::none();
    caps.redirects = RedirectSupport::Transparent;
    MockTransport::new().with_capabilities(caps)
}

/// `Transparent` plus all three timeout phases, for the tests that set a
/// timeout: without it `check_timeouts_supported` rejects the request
/// before the property under test is reached.
fn full_mock() -> MockTransport {
    let mut caps = Capabilities::none();
    caps.redirects = RedirectSupport::Transparent;
    caps.timeouts = TimeoutSupport {
        connect: true,
        first_byte: true,
        between_bytes: true,
    };
    MockTransport::new().with_capabilities(caps)
}

/// A browser-shaped backend: it follows redirects itself and lets nobody
/// see or control it.
fn internal_mock() -> MockTransport {
    let mut caps = Capabilities::none();
    caps.redirects = RedirectSupport::Internal;
    MockTransport::new().with_capabilities(caps)
}

fn run<T>(c: &Client<T>, a: FetchArgs, r: &mut Recorder) -> Result<(), ComponentError>
where
    T: http_ng_core::unversioned::Transport<Body = http_ng::mock::MockBody, Error = Error>,
{
    futures_executor::block_on(fetch(c, a, r))
}

fn meta_value<'a>(sent: &'a Sent, key: &str) -> Option<&'a [u8]> {
    sent.metadata
        .iter()
        .find(|(k, _)| k == key)
        .map(|(_, v)| v.as_slice())
}

// ── streaming ────────────────────────────────────────────────────────

/// The fidelity item the addendum puts first. The component forwards each
/// chunk as it arrives, and attaches status and headers to the **first**
/// one only; a port built on `collect()` would deliver one call carrying
/// the whole body and would still build for all three targets.
///
/// Three assertions, each of which fails on its own mutation: the call
/// count (`collect()` instead of `chunk()` → 1), the payload split (a
/// concatenating port → one 15-byte call), and the metadata placement
/// (dropping `first_chunk` → metadata on all three).
#[test]
fn every_chunk_is_forwarded_as_it_arrives_and_only_the_first_carries_metadata() {
    let m = transparent_mock();
    m.push_response_frames(
        http::Response::builder()
            .status(200)
            .header("content-type", "text/plain")
            .body(vec!["first-", "second-", "third"])
            .unwrap(),
    );

    let c = Client::builder(m).build().unwrap();
    let mut rec = Recorder::default();
    run(&c, args("https://a/x"), &mut rec).unwrap();

    assert_eq!(
        rec.sent.len(),
        3,
        "one send_content per body frame, not one for the whole body"
    );
    assert_eq!(rec.sent[0].data, b"first-");
    assert_eq!(rec.sent[1].data, b"second-");
    assert_eq!(rec.sent[2].data, b"third");

    assert_eq!(
        rec.sent[0].metadata.len(),
        2,
        "the first chunk carries status and headers"
    );
    assert_eq!(
        meta_value(&rec.sent[0], "http-client:status"),
        Some(b"200".as_slice())
    );
    assert!(
        String::from_utf8_lossy(meta_value(&rec.sent[0], "http-client:headers").unwrap())
            .contains("content-type: text/plain"),
        "the headers metadata must actually carry the headers"
    );
    assert!(
        rec.sent[1].metadata.is_empty() && rec.sent[2].metadata.is_empty(),
        "metadata goes on the first chunk only, not on every one"
    );

    // The content type rides along with every chunk, first or not.
    for s in &rec.sent {
        assert_eq!(s.content_type.as_deref(), Some("text/plain"));
    }
}

/// The `if first_chunk` fallback (the original's lines 146-152): a response
/// whose body produces no frames at all must still deliver the status and
/// headers, or a 204 would reach the caller as nothing whatsoever.
///
/// `push_response_frames` with an empty `Vec`, deliberately, not
/// `push_response` with `""` — the latter queues one zero-length **data**
/// frame, which drives the loop once and never reaches the fallback.
#[test]
fn a_body_with_no_frames_still_delivers_status_and_headers_once() {
    let m = transparent_mock();
    m.push_response_frames(
        http::Response::builder()
            .status(204)
            .body(Vec::<&'static str>::new())
            .unwrap(),
    );

    let c = Client::builder(m).build().unwrap();
    let mut rec = Recorder::default();
    run(&c, args("https://a/x"), &mut rec).unwrap();

    assert_eq!(
        rec.sent.len(),
        1,
        "exactly one call — the fallback, and not a second one from the loop"
    );
    assert!(rec.sent[0].data.is_empty());
    assert_eq!(
        meta_value(&rec.sent[0], "http-client:status"),
        Some(b"204".as_slice())
    );
}

/// `wasi-fetch::Body::chunk` returned `Option<Bytes>` and mapped a
/// mid-stream failure onto `None` (`wasi-fetch/src/body.rs`: `Some(Err(_)) |
/// None => { self.inner = Done; return None }`), so a truncated download
/// was indistinguishable from a complete one — the component would emit
/// the partial body and return `Ok`. `Response::chunk` returns
/// `Option<Result<Bytes, Error>>`, and the port propagates it.
#[test]
fn a_body_that_breaks_mid_stream_is_an_error_not_a_short_read() {
    let m = transparent_mock();
    m.push_response_frames_then_error(
        http::Response::builder()
            .status(200)
            .body(vec!["half"])
            .unwrap(),
        Error::new(ErrorKind::Body, std::io::Error::other("connection reset")),
    );

    let c = Client::builder(m).build().unwrap();
    let mut rec = Recorder::default();
    let err = run(&c, args("https://a/x"), &mut rec).unwrap_err();

    assert!(
        matches!(err, ComponentError::Internal(ref m) if m.contains("connection reset")),
        "the body error must reach the caller: {err:?}"
    );
    assert_eq!(
        rec.sent.len(),
        1,
        "the chunk that did arrive was already forwarded — it is the tail that is missing"
    );
}

// ── timeouts ─────────────────────────────────────────────────────────

/// The addendum's second fidelity item. `wasi-fetch::RequestBuilder::
/// timeout` set the wasip3 `connect` **and** `first_byte` options from its
/// single `Duration` (`wasi-fetch/src/request.rs`: `set_connect_timeout`
/// and `set_first_byte_timeout`, same `ns`). A port that maps `timeout_ms`
/// to `first_byte` alone silently drops the connect timeout the component
/// has today.
///
/// `between_bytes` is asserted `None` on purpose: `Timeouts::default()` is
/// all-`None`, so without this line a port that set nothing at all would
/// only be caught by the two positive assertions, and a port that set all
/// three would not be caught at all.
#[test]
fn one_timeout_ms_becomes_both_connect_and_first_byte() {
    let m = full_mock();
    m.push_response(http::Response::builder().status(200).body("ok").unwrap());

    let c = Client::builder(m).build().unwrap();
    let mut rec = Recorder::default();
    let mut a = args("https://a/x");
    a.timeout_ms = Some(1500);
    run(&c, a, &mut rec).unwrap();

    let seen = c.transport().requests();
    let t = seen[0]
        .extensions
        .get::<Timeouts>()
        .expect("the timeouts must reach the transport");
    assert_eq!(t.connect, Some(Duration::from_millis(1500)));
    assert_eq!(t.first_byte, Some(Duration::from_millis(1500)));
    assert_eq!(
        t.between_bytes, None,
        "`between_bytes_timeout` was a separate setter and has no source here"
    );
}

/// The other half: no `timeout_ms` means no phase timeout at all. Without
/// it, a port that always set some default would pass the test above.
#[test]
fn no_timeout_ms_sets_no_phase_at_all() {
    let m = full_mock();
    m.push_response(http::Response::builder().status(200).body("ok").unwrap());

    let c = Client::builder(m).build().unwrap();
    let mut rec = Recorder::default();
    run(&c, args("https://a/x"), &mut rec).unwrap();

    let seen = c.transport().requests();
    let t = seen[0]
        .extensions
        .get::<Timeouts>()
        .copied()
        .unwrap_or_default();
    assert_eq!(t, Timeouts::default(), "nothing was asked for, nothing set");
}

// ── the per-request redirect limit ───────────────────────────────────

/// `follow_redirects: true` → limit 10, per request, and the hop is
/// actually taken. Pairs with the test below: without both, a port that
/// hard-coded one of the two branches would pass one of them.
#[test]
fn follow_redirects_true_takes_the_hop() {
    let m = transparent_mock();
    m.push_response(
        http::Response::builder()
            .status(302)
            .header("location", "https://a/second")
            .body("")
            .unwrap(),
    );
    m.push_response(http::Response::builder().status(200).body("done").unwrap());

    let c = Client::builder(m).build().unwrap();
    let mut rec = Recorder::default();
    run(&c, args("https://a/first"), &mut rec).unwrap();

    assert_eq!(
        c.transport().requests().len(),
        2,
        "the original plus one hop"
    );
    assert_eq!(rec.sent[0].data, b"done");
    assert_eq!(
        meta_value(&rec.sent[0], "http-client:status"),
        Some(b"200".as_slice()),
        "the status reported is the final one, not the 302"
    );
}

/// `follow_redirects: false` → `RedirectPolicy::None`, and the port is now
/// behaviour-for-behaviour with the original.
///
/// This is the test that had to change when `RedirectPolicy` became an enum,
/// and it failed loudly when it did rather than quietly passing for a new
/// reason — which is why it was named for what actually happened rather than
/// for what the port wanted.
///
/// `wasi-fetch` with `redirect_limit == 0` short-circuits on
/// `if redirect_limit > 0 && status.is_redirection()` (`request.rs:135`) and
/// returns the 3xx **as an ordinary response**, so the component forwards
/// status 302 and the `Location` header to its caller. `RedirectPolicy::None`
/// now does the same: `decide` returns `Stop` before any hop counting, and
/// the response reaches the caller intact.
///
/// `Limited(0)` still errors, and the test below pins that — the two are
/// different intents and collapsing them is the defect this enum exists to
/// prevent.
#[test]
fn follow_redirects_false_hands_the_3xx_to_the_caller_like_the_original() {
    let m = transparent_mock();
    m.push_response(
        http::Response::builder()
            .status(302)
            .header("location", "https://a/second")
            .body("")
            .unwrap(),
    );

    let c = Client::builder(m).build().unwrap();
    let mut rec = Recorder::default();
    let mut a = args("https://a/first");
    a.follow_redirects = false;
    run(&c, a, &mut rec).expect("the 302 is the answer, not a failure");

    assert_eq!(
        c.transport().requests().len(),
        1,
        "not a single hop was taken"
    );
    assert_eq!(
        meta_value(&rec.sent[0], "http-client:status"),
        Some(b"302".as_slice()),
        "the component forwards the 3xx upward, exactly as it did on wasi-fetch"
    );
    let headers = String::from_utf8_lossy(
        meta_value(&rec.sent[0], "http-client:headers").expect("headers metadata"),
    )
    .to_string();
    assert!(
        headers.contains("https://a/second"),
        "Location must reach the caller — a status without it is not usable: {headers}"
    );
}

/// `Limited(0)` is NOT `None`: follow zero hops, so the first 3xx carrying a
/// `Location` is an error. Task 8 pinned this when `limit: 0` was the only
/// way to spell either intent; it survives the enum unchanged, and it must,
/// or the two intents have quietly re-merged into one.
#[test]
fn limited_zero_still_errors_and_is_not_the_same_as_none() {
    let m = transparent_mock();
    m.push_response(
        http::Response::builder()
            .status(302)
            .header("location", "https://a/second")
            .body("")
            .unwrap(),
    );

    let c = Client::builder(m)
        .redirect(http_ng::RedirectPolicy::Limited(0))
        .build()
        .unwrap();
    let err = futures_executor::block_on(c.get("https://a/first").send())
        .expect_err("Limited(0) must not hand the 302 back");
    assert_eq!(*err.kind(), ErrorKind::Redirect, "{err}");
}

/// The browser-shaped backend refuses both branches, because the component
/// states a redirect intent on **every** call and `RedirectSupport::
/// Internal` can honour neither of them. An `Unsupported` error out of
/// `send()`, not a setting that quietly does nothing — the whole reason
/// `Config::redirect` is an `Option` and this check reads the merged value.
#[test]
fn a_browser_shaped_backend_refuses_the_per_request_policy_either_way() {
    for follow in [true, false] {
        let m = internal_mock();
        m.push_response(http::Response::builder().status(200).body("done").unwrap());

        let c = Client::builder(m)
            .build()
            .expect("the client configures nothing");
        let mut rec = Recorder::default();
        let mut a = args("https://a/x");
        a.follow_redirects = follow;
        let err = run(&c, a, &mut rec).unwrap_err();

        assert!(
            matches!(err, ComponentError::Internal(ref m) if m.contains("redirect_policy")),
            "follow_redirects={follow}: {err:?}"
        );
        assert!(
            c.transport().requests().is_empty(),
            "follow_redirects={follow}: refused before anything went out"
        );
    }
}

// ── headers and body ─────────────────────────────────────────────────

/// A JSON body gets `content-type: application/json` — but only when the
/// caller did not name one, and the check is case-insensitive over the
/// caller's own key, which is a plain `String` and not a `HeaderName`.
///
/// Both halves in one test on purpose: they are the two branches of one
/// `if`, and the second is the one that goes red when the
/// `eq_ignore_ascii_case` guard is dropped — the default is applied after
/// the caller's headers and `RequestBuilder::header` inserts rather than
/// appends, so without the guard it wins.
#[test]
fn a_json_body_defaults_the_content_type_unless_the_caller_set_one() {
    let m = transparent_mock();
    m.push_response(http::Response::builder().status(200).body("ok").unwrap());
    m.push_response(http::Response::builder().status(200).body("ok").unwrap());

    let c = Client::builder(m).build().unwrap();
    let mut rec = Recorder::default();

    let mut a = args("https://a/x");
    a.method = http::Method::POST;
    a.body = Some(Body::Json("{\"k\":1}".to_string()));
    run(&c, a, &mut rec).unwrap();

    let mut b = args("https://a/y");
    b.method = http::Method::POST;
    b.body = Some(Body::Json("{\"k\":1}".to_string()));
    b.headers.insert(
        "Content-Type".to_string(),
        "application/vnd.custom+json".to_string(),
    );
    run(&c, b, &mut rec).unwrap();

    let seen = c.transport().requests();
    assert_eq!(
        seen[0].headers.get("content-type").unwrap(),
        "application/json",
        "no content-type from the caller — the JSON default applies"
    );
    assert_eq!(
        seen[1].headers.get("content-type").unwrap(),
        "application/vnd.custom+json",
        "the caller's `Content-Type` must survive, differently cased key and all"
    );
    assert_eq!(seen[0].method, http::Method::POST);
    assert_eq!(seen[0].body_size_hint, Some(7));
}

/// A `Text` body carries no content type of its own, and the caller's
/// headers travel unchanged. The negative half of the test above: without
/// it, a port that set `application/json` for every body would still pass
/// there.
#[test]
fn a_non_json_body_gets_no_content_type_and_the_callers_headers_go_out() {
    let m = transparent_mock();
    m.push_response(http::Response::builder().status(200).body("ok").unwrap());

    let c = Client::builder(m).build().unwrap();
    let mut rec = Recorder::default();
    let mut a = args("https://a/x");
    a.method = http::Method::PUT;
    a.body = Some(Body::Text("plain".to_string()));
    a.headers
        .insert("x-trace".to_string(), "abc123".to_string());
    run(&c, a, &mut rec).unwrap();

    let seen = c.transport().requests();
    assert!(
        seen[0].headers.get("content-type").is_none(),
        "only a JSON body defaults the content type"
    );
    assert_eq!(seen[0].headers.get("x-trace").unwrap(), "abc123");
    assert_eq!(seen[0].method, http::Method::PUT);
}

// ── error classification ─────────────────────────────────────────────

/// `wasi-fetch` had `Error::Url(String)`, and the component mapped exactly
/// that variant to `ActError::invalid_args`. `http-ng` reports an
/// unparseable URL as `ErrorKind::Other` carrying `http::uri::InvalidUri`,
/// so the split survives — through `source().is::<..>()` rather than
/// through `kind()`.
#[test]
fn an_unparseable_url_is_invalid_args() {
    let m = transparent_mock();
    let c = Client::builder(m).build().unwrap();
    let mut rec = Recorder::default();

    let err = run(&c, args("not a url"), &mut rec).unwrap_err();
    assert!(
        matches!(err, ComponentError::InvalidArgs(_)),
        "the caller's typo is the caller's fault: {err:?}"
    );
    assert!(
        c.transport().requests().is_empty(),
        "rejected before anything was sent"
    );
}

/// And its counterpart: a transport failure is `internal`, not
/// `invalid_args`. Without this, a `classify` that answered
/// `InvalidArgs` unconditionally would pass the test above.
#[test]
fn a_transport_failure_is_internal() {
    let m = transparent_mock();
    m.push_transport_error(Error::new(
        ErrorKind::Connect,
        std::io::Error::other("no route to host"),
    ));

    let c = Client::builder(m).build().unwrap();
    let mut rec = Recorder::default();
    let err = run(&c, args("https://a/x"), &mut rec).unwrap_err();

    assert!(
        matches!(err, ComponentError::Internal(ref m) if m.contains("no route to host")),
        "{err:?}"
    );
}
