//! CONNECT and extended CONNECT (RFC 8441) to a proxy over HTTP/2, as a
//! filter installed with `Native::egress` is lent them.
//!
//! Every claim is read off a real `h2::server` behind `tokio-rustls`: what
//! it decoded (`:authority`, `:path`, `:protocol`) and what it echoed.
#![cfg(all(feature = "http2", not(target_family = "wasm")))]

#[path = "h2_proxy.rs"]
mod h2_proxy;

use std::time::Duration;

use futures_util::{AsyncReadExt as _, AsyncWriteExt as _};
use hclient_core::error::ErrorKind;
use hclient_native::Native;
use hclient_proxy::{Dial as _, ProxyTls, TunnelRequest, TunnelVersion};
use hclient_rt_tokio::Tokio;

use h2_proxy::{H2Proxy, h2_proxy};

/// Ceiling for anything that must not hang.
const BOUND: Duration = Duration::from_secs(20);

type Tls = hclient_tls_rustls::Rustls;
type Transport = Native<Tokio, Tls, hclient_dns::IpLiteralOnly>;

/// A transport trusting `proxy`, with no filter installed.
fn native_trusting(proxy: &h2_proxy::Proxy) -> Transport {
    Native::new(
        Tokio,
        h2_proxy::client_tls(proxy),
        hclient_dns::IpLiteralOnly,
    )
}

/// A transport trusting `proxy`, with a filter installed — the opt-in that
/// lends a filter tunnels.
fn native_with_egress_trusting(proxy: &h2_proxy::Proxy) -> Transport {
    native_trusting(proxy).egress(hclient_proxy::Rules::new())
}

fn req(port: u16, authority: &str) -> TunnelRequest<'_> {
    TunnelRequest::new("127.0.0.1", port, ProxyTls::new("localhost"), authority)
        .version(TunnelVersion::Http2)
}

async fn bounded<F: Future>(f: F) -> F::Output {
    tokio::time::timeout(BOUND, f).await.expect("did not hang")
}

#[tokio::test]
async fn a_plain_connect_over_h2_carries_bytes_both_ways() {
    let proxy = h2_proxy(H2Proxy::Echo { extended: true });
    let native = native_with_egress_trusting(&proxy);
    let dial = hclient_native::testing::dial_for(&native);
    let t = bounded(dial.connect_tunnel(req(proxy.port(), "origin.test:443")))
        .await
        .unwrap();
    assert_eq!(t.response.status, 200);
    assert!(t.datagrams.is_none());
    let mut s = t.stream;
    bounded(s.write_all(b"ping")).await.unwrap();
    let mut got = [0u8; 4];
    bounded(s.read_exact(&mut got)).await.unwrap();
    assert_eq!(&got, b"ping");
    let seen = proxy.last_request();
    assert_eq!(seen.authority, "origin.test:443");
    assert_eq!(seen.protocol, None);
}

/// Past one flow-control window both ways, and ended by a half-close: the
/// echo completes only if the reader releases capacity, and `read_to_end`
/// returns only if the proxy saw `END_STREAM` and answered with its own.
#[tokio::test]
async fn a_tunnel_carries_more_than_a_window() {
    let proxy = h2_proxy(H2Proxy::Echo { extended: true });
    let native = native_with_egress_trusting(&proxy);
    let dial = hclient_native::testing::dial_for(&native);
    let t = bounded(dial.connect_tunnel(req(proxy.port(), "origin.test:443")))
        .await
        .unwrap();
    let mut s = t.stream;
    let payload: Vec<u8> = (0..300_000u32)
        .map(|i| u8::try_from(i % 251).unwrap())
        .collect();
    bounded(s.write_all(&payload)).await.unwrap();
    bounded(std::future::poll_fn(|cx| {
        hclient_rt::Shutdown::poll_shutdown(std::pin::Pin::new(&mut s), cx)
    }))
    .await
    .unwrap();
    let mut got = Vec::new();
    bounded(s.read_to_end(&mut got)).await.unwrap();
    assert_eq!(got.len(), payload.len());
    assert_eq!(got, payload);
}

/// The same tunnel through the erased context an installed filter is
/// actually lent.
#[tokio::test]
async fn an_erased_context_lends_the_same_tunnel() {
    let proxy = h2_proxy(H2Proxy::Echo { extended: true });
    let native = native_with_egress_trusting(&proxy);
    let dial = hclient_native::testing::dial_for(&native);
    let shared: &hclient_proxy::SharedDial<'_> = &dial;
    let boxed = hclient_proxy::BoxDial::new(shared);
    let t = bounded(boxed.connect_tunnel(req(proxy.port(), "origin.test:443")))
        .await
        .unwrap();
    assert_eq!(t.response.status, 200);
    let mut s = t.stream;
    bounded(s.write_all(b"pong")).await.unwrap();
    let mut got = [0u8; 4];
    bounded(s.read_exact(&mut got)).await.unwrap();
    assert_eq!(&got, b"pong");
}

#[tokio::test]
async fn an_extended_connect_carries_its_protocol_and_path() {
    let proxy = h2_proxy(H2Proxy::Echo { extended: true });
    let native = native_with_egress_trusting(&proxy);
    let dial = hclient_native::testing::dial_for(&native);
    let t = bounded(
        dial.connect_tunnel(
            req(proxy.port(), "localhost")
                .path(Some("/.well-known/masque/udp/o/443/"))
                .protocol(Some("connect-udp")),
        ),
    )
    .await
    .unwrap();
    assert_eq!(t.response.status, 200);
    let seen = proxy.last_request();
    assert_eq!(seen.protocol.as_deref(), Some("connect-udp"));
    assert_eq!(seen.path, "/.well-known/masque/udp/o/443/");
    assert_eq!(seen.authority, "localhost");
}

#[tokio::test]
async fn an_extended_connect_to_a_server_without_it_is_unsupported() {
    let proxy = h2_proxy(H2Proxy::Echo { extended: false });
    let native = native_with_egress_trusting(&proxy);
    let dial = hclient_native::testing::dial_for(&native);
    let e =
        bounded(dial.connect_tunnel(req(proxy.port(), "localhost").protocol(Some("connect-udp"))))
            .await
            .unwrap_err();
    assert_eq!(*e.kind(), ErrorKind::Unsupported, "{e:?}");
}

/// The control for the refusal above: the same server takes a plain
/// CONNECT, so what refused was the missing setting and nothing else.
#[tokio::test]
async fn a_plain_connect_needs_no_setting() {
    let proxy = h2_proxy(H2Proxy::Echo { extended: false });
    let native = native_with_egress_trusting(&proxy);
    let dial = hclient_native::testing::dial_for(&native);
    let t = bounded(dial.connect_tunnel(req(proxy.port(), "origin.test:443")))
        .await
        .unwrap();
    assert_eq!(t.response.status, 200);
}

#[tokio::test]
async fn without_egress_there_are_no_h2_tunnels() {
    let proxy = h2_proxy(H2Proxy::Echo { extended: true });
    let native = native_trusting(&proxy);
    let dial = hclient_native::testing::dial_for(&native);
    let e = bounded(dial.connect_tunnel(req(proxy.port(), "o:443")))
        .await
        .unwrap_err();
    assert_eq!(*e.kind(), ErrorKind::Unsupported, "{e:?}");
    assert_eq!(proxy.accepted(), 0, "refused before a socket was opened");
}

/// HTTP/3 alone is not lent yet, and says so rather than falling back.
#[tokio::test]
async fn http3_alone_is_unsupported() {
    let proxy = h2_proxy(H2Proxy::Echo { extended: true });
    let native = native_with_egress_trusting(&proxy);
    let dial = hclient_native::testing::dial_for(&native);
    let e = bounded(dial.connect_tunnel(req(proxy.port(), "o:443").version(TunnelVersion::Http3)))
        .await
        .unwrap_err();
    assert_eq!(*e.kind(), ErrorKind::Unsupported, "{e:?}");
    assert_eq!(proxy.accepted(), 0);
}

/// HTTP/3-then-HTTP/2 on a transport with no HTTP/3 tunnel is HTTP/2.
#[tokio::test]
async fn http3_then_http2_falls_to_http2() {
    let proxy = h2_proxy(H2Proxy::Echo { extended: true });
    let native = native_with_egress_trusting(&proxy);
    let dial = hclient_native::testing::dial_for(&native);
    let t = bounded(dial.connect_tunnel(
        req(proxy.port(), "origin.test:443").version(TunnelVersion::Http3ThenHttp2),
    ))
    .await
    .unwrap();
    assert_eq!(t.response.status, 200);
}
