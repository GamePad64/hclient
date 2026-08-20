//! The adapter is a `Service`, it carries the backend's error
//! classification through, and it is honest about `Send`.

use hclient_core::{ErrorKind, RequestBody, unversioned::Transport};
use hclient_mock::MockTransport;
use hclient_tower::TransportService;
use http_body_util::BodyExt;
use tower::ServiceExt;

fn req() -> http::Request<RequestBody> {
    http::Request::builder()
        .uri("https://example.com/")
        .body(RequestBody::Empty)
        .unwrap()
}

#[test]
fn a_transport_becomes_a_service_and_the_response_arrives_intact() {
    let m = MockTransport::new();
    m.push_response(
        http::Response::builder()
            .status(203)
            .header("x-marker", "kept")
            .body("payload")
            .unwrap(),
    );
    let svc = TransportService::new(m);

    let resp = futures_executor::block_on(svc.oneshot(req())).expect("the mock answers");
    assert_eq!(resp.status(), 203);
    assert_eq!(
        resp.headers().get("x-marker").map(|v| v.to_str().unwrap()),
        Some("kept"),
        "headers must survive the adapter, not just the status"
    );
    let body = futures_executor::block_on(resp.into_body().collect())
        .expect("body collects")
        .to_bytes();
    assert_eq!(&body[..], b"payload");
}

/// The point of `Transport::to_error`. Wrapping the backend's error afresh
/// here would repeat branch finding B2, where a backend's whole taxonomy
/// was discarded one layer up and every `is_*` predicate answered `false`.
#[test]
fn the_backends_error_classification_survives_the_adapter() {
    let m = MockTransport::new();
    m.push_transport_error(hclient_core::Error::new(
        ErrorKind::Tls,
        std::io::Error::other("handshake refused"),
    ));
    let svc = TransportService::new(m);

    let err = futures_executor::block_on(svc.oneshot(req())).expect_err("the mock fails");
    assert_eq!(
        *err.kind(),
        ErrorKind::Tls,
        "the adapter must not flatten the backend's kind to Other: {err}"
    );
    assert!(err.to_string().contains("handshake refused"), "{err}");
}

/// Cloning shares one transport rather than duplicating it — `Service::call`
/// takes `&mut self`, so a tower stack clones freely, and a clone that
/// forgot the queued responses would make the mock useless and a real
/// connection pool worse.
#[test]
fn clones_share_one_transport() {
    let m = MockTransport::new();
    m.push_response(http::Response::builder().status(200).body("first").unwrap());
    m.push_response(
        http::Response::builder()
            .status(201)
            .body("second")
            .unwrap(),
    );

    let svc = TransportService::new(m);
    let other = svc.clone();

    let a = futures_executor::block_on(svc.clone().oneshot(req())).unwrap();
    let b = futures_executor::block_on(other.oneshot(req())).unwrap();
    assert_eq!((a.status().as_u16(), b.status().as_u16()), (200, 201));
    assert_eq!(
        svc.get_ref().requests().len(),
        2,
        "both calls must land on the same transport"
    );
}

/// The capabilities are reachable through the adapter, which is what a
/// guard around a middleware layer has to consult: `tower-http`'s
/// decompression, for one, corrupts a response from a backend that already
/// decompressed, and only the capability says which that is.
#[test]
fn capabilities_are_reachable_through_the_adapter() {
    let m = MockTransport::new();
    let expected_redirects = m.capabilities().redirects;
    let svc = TransportService::new(m);
    assert_eq!(
        svc.capabilities().redirects,
        expected_redirects,
        "the adapter must forward the backend's own capabilities, not a default"
    );
}
