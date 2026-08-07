//! `ClientBuilder::base_url()` — resolving a request's relative URIs.
//!
//! Before this round, the value was stored in `Config` and read by NOBODY:
//! `client.get("/v1/things")` reached the transport as the URI
//! `/v1/things`, with no scheme and no authority. The third "saved and
//! ignored" case in the project, the second in this same struct, and one
//! that survived the whole branch's full review, which caught its twin
//! (B1, client timeouts).
//!
//! **The semantics are RFC 3986 §5, the same ones `redirect::decide` uses
//! to resolve `Location:`.** One client shouldn't resolve relative
//! references by two different rules depending on whether they came from
//! the caller or from a response header; the shared implementation is
//! `http_ng_proto::uri::resolve_reference`, which the redirect stage also
//! calls.

// `http_ng::mock` lives behind the `test-util` feature (see `mock.rs`).
#![cfg(feature = "test-util")]

use http_ng::mock::MockTransport;
use http_ng::{Client, ErrorKind, InvalidBaseUrl, RequestBody};

fn client_with_base(base: &str) -> Client<MockTransport> {
    let m = MockTransport::new();
    m.push_response(http::Response::builder().status(200).body("").unwrap());
    Client::builder(m)
        .base_url(base.parse().unwrap())
        .build()
        .unwrap()
}

fn sent_uri(c: &Client<MockTransport>) -> String {
    c.transport().requests()[0].uri.to_string()
}

/// The case this setting exists for, and exactly the one that used to be
/// a silent no-op: a relative reference + a base.
#[test]
fn a_relative_request_uri_is_resolved_against_the_base() {
    let c = client_with_base("https://example.test/api/");
    futures_executor::block_on(c.get("v1/things").send()).unwrap();
    assert_eq!(sent_uri(&c), "https://example.test/api/v1/things");
}

/// An absolute request URI ignores the base — RFC 3986 §5.2.2: if a
/// reference has a scheme, resolution returns it unchanged.
#[test]
fn an_absolute_request_uri_ignores_the_base() {
    let c = client_with_base("https://example.test/api/");
    futures_executor::block_on(c.get("https://other.test/direct").send()).unwrap();
    assert_eq!(sent_uri(&c), "https://other.test/direct");
}

/// Without a base, the URI goes out as-is — including a relative one.
/// Nothing is invented for the caller; a transport that needs
/// absolute-form will reject it itself, with its own type
/// (`WasiHttp::execute` → `scheme_of`).
#[test]
fn without_a_base_the_uri_reaches_the_transport_unchanged() {
    let m = MockTransport::new();
    m.push_response(http::Response::builder().status(200).body("").unwrap());
    let c = Client::builder(m).build().unwrap();
    futures_executor::block_on(c.get("/v1/things").send()).unwrap();
    assert_eq!(sent_uri(&c), "/v1/things");
}

/// The sharp corner of RFC 3986 and the one non-obvious part of the rule:
/// a reference starting with `/` replaces the base's ENTIRE path, rather
/// than appending to it. Pinning both variants in one test so "fixing" one
/// doesn't slip by unnoticed.
#[test]
fn an_absolute_path_replaces_the_bases_path_while_a_relative_one_extends_it() {
    let c = client_with_base("https://example.test/api/");
    futures_executor::block_on(c.get("/v1/things").send()).unwrap();
    assert_eq!(
        sent_uri(&c),
        "https://example.test/v1/things",
        "a leading / replaces the base's path (RFC 3986 §5.2.2), rather than appending to it"
    );

    let c2 = client_with_base("https://example.test/api/");
    futures_executor::block_on(c2.get("v1/things").send()).unwrap();
    assert_eq!(sent_uri(&c2), "https://example.test/api/v1/things");
}

/// The second half of the same sharp corner: a base WITHOUT a trailing
/// slash loses its last segment when resolving a relative reference — this
/// is the merge algorithm from RFC 3986 §5.3, not our own invention.
/// Documented on the setter; the test keeps the documentation honest.
#[test]
fn a_base_without_a_trailing_slash_drops_its_last_segment() {
    let c = client_with_base("https://example.test/api");
    futures_executor::block_on(c.get("v1/things").send()).unwrap();
    assert_eq!(
        sent_uri(&c),
        "https://example.test/v1/things",
        "`/api` with no slash is not a directory: RFC 3986 §5.3 drops the last segment"
    );
}

/// A request with no path at all: the base should reach the transport
/// whole, not collapse into its root.
#[test]
fn an_empty_reference_resolves_to_the_base_itself() {
    let c = client_with_base("https://example.test/api/things");
    futures_executor::block_on(c.get("").send()).unwrap();
    assert_eq!(sent_uri(&c), "https://example.test/api/things");
}

/// A reference's query survives resolution.
#[test]
fn the_query_of_a_relative_reference_survives() {
    let c = client_with_base("https://example.test/api/");
    futures_executor::block_on(c.get("search?q=1&n=2").send()).unwrap();
    assert_eq!(sent_uri(&c), "https://example.test/api/search?q=1&n=2");
}

/// The base itself must be absolute: a relative one has nothing to
/// resolve against. This is a typed error with a nameable source, not a
/// silently ignored setting — the same contract as an unsupported timeout
/// (M3).
#[test]
fn a_relative_base_is_a_typed_error_not_a_silently_ignored_setting() {
    let c = client_with_base("/api/");
    let err = futures_executor::block_on(c.get("v1/things").send())
        .expect_err("a relative base is unfit for use — this must be an error");

    assert_eq!(*err.kind(), ErrorKind::Other, "{err}");
    let src = std::error::Error::source(&err).expect("Error::new always sets a source");
    let bad = src
        .downcast_ref::<InvalidBaseUrl>()
        .expect("the source must name the specific problem, not just be a string");
    assert_eq!(bad.base, "/api/".parse::<http::Uri>().unwrap());
    assert_eq!(bad.requested, "v1/things");
    assert!(
        c.transport().requests().is_empty(),
        "a request with an unfit base must not reach the transport"
    );
}

/// `Response::url()` must name the URL the request actually went out on,
/// not what the caller typed. Otherwise base resolution would be visible
/// to the transport and invisible to the consumer.
#[test]
fn response_url_reports_the_resolved_uri_not_the_relative_one() {
    let c = client_with_base("https://example.test/api/");
    let resp = futures_executor::block_on(c.get("v1/things").send()).unwrap();
    assert_eq!(
        resp.url(),
        &"https://example.test/api/v1/things"
            .parse::<http::Uri>()
            .unwrap()
    );
}

/// `Client::execute` is a public entry point that takes an already-built
/// `http::Request`; the base must apply here too, or the setting only
/// works through `RequestBuilder` and is partial all over again.
///
/// The reference here is `/v1/things`, not `v1/things`, and that's not a
/// choice made by the test: `http::Request::builder().uri("v1/things")`
/// doesn't even build — `http::Uri` can't represent a path-relative
/// reference. Only origin-form and absolute-form are expressible through
/// this entry point, so the base can give such a request a scheme and
/// authority, but not a path. RFC 3986 §5.2.2 would say the exact same
/// thing about any reference with a leading `/`.
#[test]
fn client_execute_resolves_the_base_too_not_only_request_builder() {
    let c = client_with_base("https://example.test/api/");
    let req = http::Request::builder()
        .uri("/v1/things")
        .body(RequestBody::Empty)
        .unwrap();
    futures_executor::block_on(c.execute(req)).unwrap();
    assert_eq!(sent_uri(&c), "https://example.test/v1/things");
}

/// The flip side of the previous test: `http::Uri` can't represent a
/// path-relative reference, so the only way to use one at all is
/// `RequestBuilder`, which resolves the original STRING before parsing.
/// The test pins down that this is exactly the limitation, not "`get`
/// can't do it either".
#[test]
fn a_path_relative_reference_is_expressible_through_the_builder_only() {
    assert!(
        "v1/things".parse::<http::Uri>().is_err(),
        "if http::Uri ever learns this form, resolution could be unified on Uri"
    );
    let c = client_with_base("https://example.test/api/");
    futures_executor::block_on(c.get("v1/things").send()).unwrap();
    assert_eq!(sent_uri(&c), "https://example.test/api/v1/things");
}
