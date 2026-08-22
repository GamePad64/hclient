//! `Native::http3` — a QUIC arm on the TCP transport, watched from the
//! servers' side of the wire.
//!
//! It lives in this crate's suite rather than `hclient-native`'s because
//! the live QUIC/TCP pair (`tests/servers.rs`) is here, and one test
//! travelling to a fixture is cheaper than a 200-line fixture travelling
//! to a crate. It moves when the two crates do.
//!
//! What is asserted is that the request **reached the QUIC server** —
//! counted by a peer, not inferred from a type. A `Native` that stored the
//! arm and went on using TCP would answer the caller identically, since
//! both servers here serve the same authority.
#![cfg(all(feature = "http3", not(target_family = "wasm")))]

mod fakedns;
mod servers;

use fakedns::FakeDns;
use hclient_core::unversioned::Transport as _;
use hclient_core::{RequestBody, RequireVersion};
use hclient_native::H3;
use hclient_native::Native;
use hclient_rt_tokio::TokioHandle;
use servers::{ORIGIN, Pair};
use std::error::Error as _;

fn armed(pair: &Pair, dns: FakeDns) -> Native<TokioHandle, hclient_tls_rustls::Rustls, FakeDns> {
    let rt = TokioHandle::current().expect("inside #[tokio::test]");
    let quic = H3::new(rt.clone(), servers::client_tls(&pair.cert_der), dns.clone())
        .expect("H3::new does no I/O");
    Native::new(rt, servers::client_tls(&pair.cert_der), dns)
        .http3(quic)
        .expect("the two paths agree")
}

fn demanding(pair: &Pair, v: http::Version) -> http::Request<RequestBody> {
    let mut req = http::Request::builder()
        .uri(format!("https://{ORIGIN}:{}/hello", pair.port))
        .body(RequestBody::Empty)
        .expect("a well-formed request");
    req.extensions_mut().insert(RequireVersion(v));
    req
}

/// The claim: with an arm installed, a request demanding HTTP/3 is served
/// by the QUIC server and the TCP one never hears about it.
#[tokio::test(flavor = "multi_thread")]
async fn a_demand_for_http3_reaches_the_quic_server() {
    let pair = servers::start();
    let dns = FakeDns::new();
    let t = armed(&pair, dns);

    let resp = t
        .execute(demanding(&pair, http::Version::HTTP_3))
        .await
        .expect("the QUIC server answers");

    assert_eq!(resp.status(), 200);
    assert_eq!(
        pair.quic_answered(),
        1,
        "the demand must be served over QUIC, by the server that counts it"
    );
    assert_eq!(
        pair.tcp_accepted(),
        0,
        "and the TCP stack must not have been touched — both servers stand \
         behind one authority, so a wrong route would answer identically"
    );
}

/// The control: the same transport with no arm refuses the same demand,
/// and does so with the type a connection that negotiated the wrong
/// protocol would raise rather than a second spelling of it.
#[tokio::test(flavor = "multi_thread")]
async fn without_an_arm_the_same_demand_is_refused() {
    let pair = servers::start();
    let rt = TokioHandle::current().expect("inside #[tokio::test]");
    let t = Native::new(rt, servers::client_tls(&pair.cert_der), FakeDns::new());

    let err = t
        .execute(demanding(&pair, http::Version::HTTP_3))
        .await
        .expect_err("there is no QUIC arm on this transport");

    assert!(
        err.source()
            .and_then(|s| s.downcast_ref::<hclient_core::VersionNotAvailable>())
            .is_some(),
        "the refusal must be `VersionNotAvailable`, not a bare Unsupported: {err}"
    );
    assert_eq!(pair.quic_answered(), 0);
    assert_eq!(
        pair.tcp_accepted(),
        0,
        "and nothing was dialled to find out"
    );
}
