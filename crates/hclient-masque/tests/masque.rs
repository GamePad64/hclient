//! `Masque` end to end: a real `hclient::Client` over a real `Native`,
//! through MASQUE proxies that really relay, to origins that really answer.
#![cfg(not(target_family = "wasm"))]

mod common;

use common::bounded;
use hclient_masque::Masque;
use hclient_proxy::TunnelVersion;

#[tokio::test(flavor = "multi_thread")]
async fn h3_in_h3_through_a_masque_proxy() {
    let origin = common::h3_origin().await;
    let proxy = common::masque_h3().await;
    let client = common::client_through(Masque::new("localhost", proxy.port()), &[&origin, &proxy]);
    let r = bounded(
        client
            .get(origin.url_ip())
            .require_version(http::Version::HTTP_3)
            .send(),
    )
    .await
    .unwrap();
    assert_eq!(r.status(), 200);
    assert_eq!(r.version(), http::Version::HTTP_3);
    assert!(proxy.datagrams_forwarded() > 0);
    assert_eq!(origin.h3_answered(), 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn h3_through_a_masque_proxy_spoken_to_over_h2_capsules() {
    let origin = common::h3_origin().await;
    let proxy = common::masque_h2().await;
    let client = common::client_through(
        Masque::new("localhost", proxy.port()).version(TunnelVersion::Http2),
        &[&origin, &proxy],
    );
    let r = bounded(
        client
            .get(origin.url_ip())
            .require_version(http::Version::HTTP_3)
            .send(),
    )
    .await
    .unwrap();
    assert_eq!(r.version(), http::Version::HTTP_3);
    assert!(proxy.capsules_forwarded() > 0);
    assert_eq!(origin.h3_answered(), 1);
}

/// A plain CONNECT goes over HTTP/2, asked for alone or after HTTP/3.
#[tokio::test(flavor = "multi_thread")]
async fn connect_tcp_over_h2() {
    let origin = common::h1_origin().await;
    for v in [TunnelVersion::Http2, TunnelVersion::Http3ThenHttp2] {
        let proxy = common::masque_h2().await;
        let client =
            common::client_through(Masque::new("localhost", proxy.port()).version(v), &[&proxy]);
        let r = bounded(client.get(origin.url_ip_http()).send())
            .await
            .unwrap();
        assert_eq!(r.status(), 200, "{v:?}");
        assert_eq!(proxy.tcp_tunnels(), 1, "{v:?}");
    }
    assert_eq!(origin.answered(), 2);
}

/// A plain CONNECT is not lent over HTTP/3: the HTTP/3 stack writes
/// `:scheme` and `:path` on every request, and RFC 9114 §4.4 forbids both
/// on a CONNECT. So a byte stream through a proxy asked for over HTTP/3
/// alone is refused as unsupported — before a QUIC connection to the
/// proxy is even attempted.
#[tokio::test(flavor = "multi_thread")]
async fn connect_tcp_over_h3_alone_is_unsupported_and_dials_nothing() {
    let origin = common::h1_origin().await;
    let proxy = common::masque_h3().await;
    let client = common::client_through(
        Masque::new("localhost", proxy.port()).version(TunnelVersion::Http3),
        &[&proxy],
    );
    let e = bounded(client.get(origin.url_ip_http()).send())
        .await
        .unwrap_err();
    assert_eq!(
        *e.kind(),
        hclient_core::error::ErrorKind::Unsupported,
        "{e:?}"
    );
    assert_eq!(proxy.accepted(), 0, "no QUIC connection reached the proxy");
    assert_eq!(proxy.tcp_tunnels(), 0);
    assert_eq!(origin.answered(), 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_proxy_that_refuses_connect_udp_falls_back_to_a_tcp_tunnel() {
    let origin = common::h3_and_h1_origin().await;
    let proxy = common::masque_h2_without_udp().await;
    let client = common::client_through(
        Masque::new("localhost", proxy.port()).version(TunnelVersion::Http2),
        &[&origin, &proxy],
    );
    common::seed_alt_svc(&client, &origin).await;
    let r = bounded(client.get(origin.url_ip()).send()).await.unwrap();
    assert_eq!(r.version(), http::Version::HTTP_11);
    assert_eq!(proxy.udp_refusals(), 1);
    // Remembered: a third request does not ask for UDP again.
    let r = bounded(client.get(origin.url_ip()).send()).await.unwrap();
    assert_eq!(r.version(), http::Version::HTTP_11);
    assert_eq!(proxy.udp_refusals(), 1);
    assert_eq!(origin.h3_answered(), 0);
}
