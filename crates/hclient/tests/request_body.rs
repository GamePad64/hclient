//! The consumer case the conversions were built for: `.body("raw text")`
//! compiles without naming [`RequestBody`](hclient::RequestBody) and
//! without a direct `bytes` dependency, and the literal lands on the
//! wire byte for byte.

#![cfg(feature = "test-util")]

use hclient::mock::MockTransport;
use hclient::{Client, RequestBody};

#[test]
fn a_str_body_reaches_the_wire_byte_for_byte() {
    let m = MockTransport::new();
    m.push_response(http::Response::builder().status(200).body("").unwrap());
    let c = Client::builder(m.clone()).build().unwrap();
    futures_executor::block_on(c.post("https://a/x").body("{\"speed\": 1e999}").send()).unwrap();
    let recorded = m.requests();
    assert_eq!(recorded.len(), 1);
    let body = recorded[0].body.bytes().unwrap();
    assert_eq!(&body[..], b"{\"speed\": 1e999}");
}

#[test]
fn a_non_static_str_is_copied_not_borrowed() {
    let m = MockTransport::new();
    m.push_response(http::Response::builder().status(200).body("").unwrap());
    let owned = String::from("the whole value");
    let slice: &str = &owned[4..9];
    let c = Client::builder(m.clone()).build().unwrap();
    futures_executor::block_on(c.post("https://a/x").body(slice).send()).unwrap();
    let recorded = m.requests();
    assert_eq!(&recorded[0].body.bytes().unwrap()[..], b"whole");
}

#[test]
fn vec_u8_and_bytes_convert_the_same_way() {
    let m = MockTransport::new();
    m.push_response(http::Response::builder().status(200).body("").unwrap());
    let c = Client::builder(m.clone()).build().unwrap();
    futures_executor::block_on(c.post("https://a/x").body(b"vec body".to_vec()).send()).unwrap();
    let recorded = m.requests();
    assert_eq!(&recorded[0].body.bytes().unwrap()[..], b"vec body");
}

#[test]
fn naming_request_body_still_works() {
    // The blanket `impl Into<RequestBody> for RequestBody` keeps every
    // existing call compiling — the widening of `.body` is additive.
    let m = MockTransport::new();
    m.push_response(http::Response::builder().status(200).body("").unwrap());
    let c = Client::builder(m.clone()).build().unwrap();
    futures_executor::block_on(
        c.post("https://a/x")
            .body(RequestBody::Full(bytes::Bytes::from_static(b"named")))
            .send(),
    )
    .unwrap();
    let recorded = m.requests();
    assert_eq!(&recorded[0].body.bytes().unwrap()[..], b"named");
}
