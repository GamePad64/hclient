//! CONNECT and extended CONNECT (RFC 8441, RFC 9220) to a proxy over
//! HTTP/2 and HTTP/3, as a filter installed with `Native::egress` is lent
//! them.
//!
//! Every claim is read off a real `h2::server` behind `tokio-rustls`, or a
//! real `h3::server` over quinn: what it decoded (`:authority`, `:path`,
//! `:protocol`), what it echoed, and — over HTTP/3 — which quarter stream
//! id each datagram carried and what each connection announced.
#![cfg(all(feature = "http2", not(target_family = "wasm")))]

#[path = "h2_proxy.rs"]
mod h2_proxy;
#[cfg(feature = "http3")]
#[path = "h3_proxy.rs"]
mod h3_proxy;

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

/// A TLS proxy that negotiates no `h2` is refused before a byte of HTTP/2
/// is written: speaking the preface into a connection whose peer chose
/// otherwise is a protocol error the proxy would see, not a tunnel.
#[tokio::test]
async fn a_tls_proxy_that_does_not_select_h2_is_refused_before_the_preface() {
    let proxy = h2_proxy(H2Proxy::NoAlpn);
    let native = native_with_egress_trusting(&proxy);
    let dial = hclient_native::testing::dial_for(&native);
    let err = bounded(dial.connect_tunnel(req(proxy.port(), "origin.test:443")))
        .await
        .unwrap_err();
    assert!(matches!(err.kind(), ErrorKind::Unsupported), "{err:?}");
    assert!(format!("{err}{err:?}").contains("no ALPN"), "{err:?}");
    drop(dial);
    drop(native);
    // The proxy reads until the client closes (or a preface arrives), so
    // what it holds afterwards is everything that was sent.
    let got = bounded(async {
        loop {
            if let Some(got) = proxy.received() {
                return got;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await;
    assert!(
        !got.starts_with(&h2_proxy::PREFACE[..4]),
        "the proxy saw {got:?}"
    );
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

/// A reader parked in one task must be woken by what arrives after a
/// writer in another task polled the connection last and went idle — the
/// shape a datagram path over this stream has, where receiving and sending
/// happen on different tasks.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_reader_in_one_task_wakes_after_a_writer_in_another_goes_idle() {
    let proxy = h2_proxy(H2Proxy::Echo { extended: true });
    let native = native_with_egress_trusting(&proxy);
    let dial = hclient_native::testing::dial_for(&native);
    let t = bounded(dial.connect_tunnel(req(proxy.port(), "origin.test:443")))
        .await
        .unwrap();
    let (mut r, mut w) = t.stream.split();
    let reader = tokio::spawn(async move {
        let mut got = [0u8; 4];
        r.read_exact(&mut got).await.map(|()| got)
    });
    // Long enough for the reader to have parked in its read.
    tokio::time::sleep(Duration::from_millis(200)).await;
    tokio::spawn(async move {
        w.write_all(b"ping").await.unwrap();
        w.flush().await.unwrap();
        // Idle from here on, holding the half so the stream stays open.
        std::future::pending::<()>().await;
    });
    let got = tokio::time::timeout(Duration::from_secs(5), reader)
        .await
        .expect("the reader was woken by the echo")
        .unwrap()
        .unwrap();
    assert_eq!(&got, b"ping");
}

/// Tunnels over HTTP/3, on connections of their own, carrying the
/// stream's HTTP datagrams.
#[cfg(feature = "http3")]
mod over_h3 {
    use super::{bounded, h2_proxy};
    use crate::h3_proxy::{self, Announced, H3Proxy, h3_proxy};
    use futures_util::{AsyncReadExt as _, AsyncWriteExt as _};
    use hclient_core::error::ErrorKind;
    use hclient_native::{H3, Native};
    use hclient_proxy::{
        DatagramPath as _, Dial as _, ProxyTls, Tunnel, TunnelRequest, TunnelVersion,
    };
    use hclient_rt_tokio::TokioHandle;
    use std::time::Duration;

    type Tls = hclient_tls_rustls::Rustls;
    type Transport = Native<TokioHandle, Tls, hclient_dns::IpLiteralOnly>;

    const ECHO: H3Proxy = H3Proxy::Echo {
        extended: true,
        datagrams: true,
    };

    fn quic(tls: &Tls) -> H3<TokioHandle, Tls, hclient_dns::IpLiteralOnly> {
        H3::new(
            TokioHandle::current().expect("inside #[tokio::test]"),
            tls.clone(),
            hclient_dns::IpLiteralOnly,
        )
        .expect("H3::new does no I/O")
    }

    /// A transport with an HTTP/3 arm and a filter installed, trusting
    /// `tls`'s roots — the two opt-ins that lend a filter HTTP/3 tunnels.
    fn native(tls: &Tls) -> Transport {
        Native::new(
            TokioHandle::current().expect("inside #[tokio::test]"),
            tls.clone(),
            hclient_dns::IpLiteralOnly,
        )
        .http3(quic(tls))
        .expect("the two stacks agree")
        .egress(hclient_proxy::Rules::new())
    }

    fn udp(port: u16) -> TunnelRequest<'static> {
        TunnelRequest::new("127.0.0.1", port, ProxyTls::new("localhost"), "localhost")
            .path(Some("/.well-known/masque/udp/o/443/"))
            .protocol(Some("connect-udp"))
            .version(TunnelVersion::Http3)
    }

    async fn recv(d: &hclient_proxy::BoxPath) -> Vec<u8> {
        let mut buf = [0u8; 2048];
        let n = bounded(std::future::poll_fn(|cx| d.poll_recv(cx, &mut buf)))
            .await
            .unwrap();
        buf[..n].to_vec()
    }

    async fn echoes_both_ways(t: Tunnel, proxy: &h3_proxy::Proxy) {
        assert_eq!(t.response.status, 200);
        let d = t.datagrams.expect("h3 tunnels carry datagrams");
        assert!(d.max_datagram_size() >= 1200, "{}", d.max_datagram_size());
        d.try_send(b"dgram").unwrap();
        // The proxy sends a decoy for another quarter stream id first; only
        // this stream's datagram is handed over.
        assert_eq!(recv(&d).await, b"dgram");
        let seen = proxy.last_request();
        assert_eq!(proxy.quarters(), vec![seen.stream_id / 4]);
        let mut s = t.stream;
        bounded(s.write_all(b"ping")).await.unwrap();
        let mut got = [0u8; 4];
        bounded(s.read_exact(&mut got)).await.unwrap();
        assert_eq!(&got, b"ping");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn an_extended_connect_over_h3_carries_bytes_and_datagrams() {
        let proxy = h3_proxy(ECHO);
        let native = native(&h3_proxy::client_tls(&proxy));
        let dial = hclient_native::testing::dial_for(&native);
        let t = bounded(dial.connect_tunnel(udp(proxy.port())))
            .await
            .unwrap();
        echoes_both_ways(t, &proxy).await;
        let seen = proxy.last_request();
        assert_eq!(seen.method, "CONNECT");
        assert_eq!(seen.protocol.as_deref(), Some("connect-udp"));
        assert_eq!(seen.path, "/.well-known/masque/udp/o/443/");
        assert_eq!(seen.authority, "localhost");
    }

    /// The same tunnel through the erased context an installed filter is
    /// actually lent.
    #[tokio::test(flavor = "multi_thread")]
    async fn an_erased_context_lends_the_same_h3_tunnel() {
        let proxy = h3_proxy(ECHO);
        let native = native(&h3_proxy::client_tls(&proxy));
        let dial = hclient_native::testing::dial_for(&native);
        let shared: &hclient_proxy::SharedDial<'_> = &dial;
        let boxed = hclient_proxy::BoxDial::new(shared);
        let t = bounded(boxed.connect_tunnel(udp(proxy.port())))
            .await
            .unwrap();
        echoes_both_ways(t, &proxy).await;
    }

    /// `egress` before `http3`: the filter is lent the arm installed after
    /// it, so neither order of the two opt-ins loses the tunnels.
    #[tokio::test(flavor = "multi_thread")]
    async fn the_order_of_egress_and_http3_does_not_matter() {
        let proxy = h3_proxy(ECHO);
        let tls = h3_proxy::client_tls(&proxy);
        let native = Native::new(
            TokioHandle::current().unwrap(),
            tls.clone(),
            hclient_dns::IpLiteralOnly,
        )
        .egress(hclient_proxy::Rules::new())
        .http3(quic(&tls))
        .unwrap();
        let dial = hclient_native::testing::dial_for(&native);
        let t = bounded(dial.connect_tunnel(udp(proxy.port())))
            .await
            .unwrap();
        echoes_both_ways(t, &proxy).await;
    }

    /// Past the stream's first window, and ended by a half-close: the
    /// echo completes only if the reads keep the window open and the
    /// proxy sees the stream's end.
    #[tokio::test(flavor = "multi_thread")]
    async fn an_h3_tunnel_carries_a_long_stream_to_its_end() {
        let proxy = h3_proxy(ECHO);
        let native = native(&h3_proxy::client_tls(&proxy));
        let dial = hclient_native::testing::dial_for(&native);
        let t = bounded(dial.connect_tunnel(udp(proxy.port())))
            .await
            .unwrap();
        let (mut r, mut w) = t.stream.split();
        let payload: Vec<u8> = (0..2_000_000u32)
            .map(|i| u8::try_from(i % 251).unwrap())
            .collect();
        let expected = payload.clone();
        let writer = tokio::spawn(async move {
            w.write_all(&payload).await.unwrap();
            w.close().await.unwrap();
            w
        });
        let mut got = Vec::new();
        bounded(r.read_to_end(&mut got)).await.unwrap();
        let _w = bounded(writer).await.unwrap();
        assert_eq!(got.len(), expected.len());
        assert_eq!(got, expected);
    }

    /// A tunnel's connection announces extended CONNECT and datagrams, and
    /// an ordinary request's connection to the same server announces
    /// neither: the tunnel's settings are its own connection's.
    #[tokio::test(flavor = "multi_thread")]
    async fn an_h3_tunnel_does_not_change_what_ordinary_h3_connections_announce() {
        use hclient_core::transport::Transport as _;
        let proxy = h3_proxy(ECHO);
        let native = native(&h3_proxy::client_tls(&proxy));
        let mut req = http::Request::get(format!("https://127.0.0.1:{}/", proxy.port()))
            .body(hclient_core::body::RequestBody::Empty)
            .unwrap();
        req.extensions_mut()
            .insert(hclient_core::req::RequireVersion(http::Version::HTTP_3));
        let resp = bounded(native.execute(req)).await.unwrap();
        assert_eq!(resp.status(), 200);
        assert_eq!(resp.version(), http::Version::HTTP_3);
        let dial = hclient_native::testing::dial_for(&native);
        let _t = bounded(dial.connect_tunnel(udp(proxy.port())))
            .await
            .unwrap();
        assert_eq!(
            proxy.announced(),
            vec![
                Announced {
                    extended_connect: false,
                    datagrams: false,
                },
                Announced {
                    extended_connect: true,
                    datagrams: true,
                },
            ],
            "the ordinary connection first, then the tunnel's own"
        );
    }

    /// An h3 proxy that announces no extended CONNECT is `Unsupported` over
    /// HTTP/3 alone, once its SETTINGS have said so.
    #[tokio::test(flavor = "multi_thread")]
    async fn an_h3_proxy_without_extended_connect_is_unsupported() {
        let proxy = h3_proxy(H3Proxy::Echo {
            extended: false,
            datagrams: true,
        });
        let native = native(&h3_proxy::client_tls(&proxy));
        let dial = hclient_native::testing::dial_for(&native);
        let e = bounded(dial.connect_tunnel(udp(proxy.port())))
            .await
            .unwrap_err();
        assert_eq!(*e.kind(), ErrorKind::Unsupported, "{e:?}");
        assert_eq!(proxy.accepted(), 1, "refused on what it announced");
    }

    /// Nor does one that announces no HTTP datagrams: an HTTP/3 tunnel is
    /// asked for its datagrams.
    #[tokio::test(flavor = "multi_thread")]
    async fn an_h3_proxy_without_datagrams_is_unsupported() {
        let proxy = h3_proxy(H3Proxy::Echo {
            extended: true,
            datagrams: false,
        });
        let native = native(&h3_proxy::client_tls(&proxy));
        let dial = hclient_native::testing::dial_for(&native);
        let e = bounded(dial.connect_tunnel(udp(proxy.port())))
            .await
            .unwrap_err();
        assert_eq!(*e.kind(), ErrorKind::Unsupported, "{e:?}");
        assert_eq!(proxy.accepted(), 1);
    }

    /// A plain CONNECT and a `:protocol` the HTTP/3 stack cannot write are
    /// refused over HTTP/3 alone before a packet is sent.
    #[tokio::test(flavor = "multi_thread")]
    async fn what_h3_cannot_express_is_refused_before_a_packet() {
        let proxy = h3_proxy(ECHO);
        let native = native(&h3_proxy::client_tls(&proxy));
        let dial = hclient_native::testing::dial_for(&native);
        for req in [
            udp(proxy.port()).protocol(None),
            udp(proxy.port()).protocol(Some("bespoke")),
        ] {
            let e = bounded(dial.connect_tunnel(req)).await.unwrap_err();
            assert_eq!(*e.kind(), ErrorKind::Unsupported, "{e:?}");
        }
        assert_eq!(proxy.accepted(), 0);
    }

    /// The default version is HTTP/3 then HTTP/2, and a plain CONNECT —
    /// which HTTP/3 here cannot write — goes straight to HTTP/2, with no
    /// QUIC packet sent: a UDP socket on the proxy's port hears nothing.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_plain_connect_goes_straight_to_h2() {
        let proxy = h2_proxy::h2_proxy(h2_proxy::H2Proxy::Echo { extended: true });
        let udp_ear = std::net::UdpSocket::bind(("127.0.0.1", proxy.port())).ok();
        let native = native(&h3_proxy::trusting(&[proxy.cert()]));
        let dial = hclient_native::testing::dial_for(&native);
        let t = bounded(dial.connect_tunnel(TunnelRequest::new(
            "127.0.0.1",
            proxy.port(),
            ProxyTls::new("localhost"),
            "o:443",
        )))
        .await
        .unwrap();
        assert!(t.datagrams.is_none(), "the tunnel came over h2");
        assert_eq!(proxy.last_request().authority, "o:443");
        if let Some(ear) = udp_ear {
            ear.set_nonblocking(true).unwrap();
            let e = ear.recv(&mut [0u8; 2048]).unwrap_err();
            assert_eq!(e.kind(), std::io::ErrorKind::WouldBlock, "QUIC was dialled");
        }
    }

    /// An extended CONNECT with HTTP/3 then HTTP/2, to a proxy whose UDP
    /// port swallows every packet: HTTP/3 gets a bounded share, and the
    /// tunnel comes up over HTTP/2 in seconds rather than after QUIC's idle
    /// timeout.
    #[tokio::test(flavor = "multi_thread")]
    async fn an_h3_attempt_into_silence_leaves_time_for_h2() {
        let proxy = h2_proxy::h2_proxy(h2_proxy::H2Proxy::Echo { extended: true });
        let Ok(hole) = std::net::UdpSocket::bind(("127.0.0.1", proxy.port())) else {
            return;
        };
        let native = native(&h3_proxy::trusting(&[proxy.cert()]));
        let dial = hclient_native::testing::dial_for(&native);
        let t = tokio::time::timeout(
            Duration::from_secs(5),
            dial.connect_tunnel(udp(proxy.port()).version(TunnelVersion::Http3ThenHttp2)),
        )
        .await
        .expect("HTTP/2 was reached within seconds")
        .unwrap();
        assert!(t.datagrams.is_none(), "the tunnel came over h2");
        assert_eq!(
            proxy.last_request().protocol.as_deref(),
            Some("connect-udp")
        );
        hole.set_nonblocking(true).unwrap();
        assert!(
            hole.recv(&mut [0u8; 2048]).is_ok(),
            "HTTP/3 was tried first"
        );
    }

    /// Two proxies behind one authority, an HTTP/3 one on UDP that refuses
    /// extended CONNECT and an HTTP/2 one on TCP that takes it — or `None`
    /// where the UDP port is taken.
    fn pair() -> Option<(h2_proxy::Proxy, h3_proxy::Proxy)> {
        let h2 = h2_proxy::h2_proxy(h2_proxy::H2Proxy::Echo { extended: true });
        let h3 = h3_proxy::h3_proxy_on(
            H3Proxy::Echo {
                extended: false,
                datagrams: true,
            },
            h2.port(),
        )?;
        Some((h2, h3))
    }

    /// An extended CONNECT that HTTP/3 was asked for first and refused goes
    /// to the same proxy over HTTP/2.
    #[tokio::test(flavor = "multi_thread")]
    async fn an_h3_refusal_falls_to_h2_where_both_were_asked_for() {
        let Some((h2, h3)) = pair() else { return };
        let native = native(&h3_proxy::trusting(&[h2.cert(), h3.cert()]));
        let dial = hclient_native::testing::dial_for(&native);
        let t = bounded(dial.connect_tunnel(udp(h2.port()).version(TunnelVersion::Http3ThenHttp2)))
            .await
            .unwrap();
        assert!(t.datagrams.is_none(), "the tunnel came over h2");
        assert_eq!(h3.accepted(), 1, "HTTP/3 was tried first");
        assert_eq!(h2.last_request().protocol.as_deref(), Some("connect-udp"));
    }

    /// The control: HTTP/3 alone keeps the refusal, and HTTP/2 is never
    /// dialled.
    #[tokio::test(flavor = "multi_thread")]
    async fn an_h3_refusal_stands_where_only_h3_was_asked_for() {
        let Some((h2, h3)) = pair() else { return };
        let native = native(&h3_proxy::trusting(&[h2.cert(), h3.cert()]));
        let dial = hclient_native::testing::dial_for(&native);
        let e = bounded(dial.connect_tunnel(udp(h2.port())))
            .await
            .unwrap_err();
        assert_eq!(*e.kind(), ErrorKind::Unsupported, "{e:?}");
        assert_eq!(h3.accepted(), 1);
        assert_eq!(h2.accepted(), 0);
    }

    /// A transport without an HTTP/3 arm lends no HTTP/3 tunnels, and says
    /// so before a packet is sent.
    #[tokio::test(flavor = "multi_thread")]
    async fn without_http3_there_are_no_h3_tunnels() {
        let proxy = h3_proxy(ECHO);
        let native = Native::new(
            TokioHandle::current().unwrap(),
            h3_proxy::client_tls(&proxy),
            hclient_dns::IpLiteralOnly,
        )
        .egress(hclient_proxy::Rules::new());
        let dial = hclient_native::testing::dial_for(&native);
        let e = bounded(dial.connect_tunnel(udp(proxy.port())))
            .await
            .unwrap_err();
        assert_eq!(*e.kind(), ErrorKind::Unsupported, "{e:?}");
        assert_eq!(proxy.accepted(), 0);
    }

    /// A reader parked in one task is woken by the echo of what a writer in
    /// another task sent and then went idle.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn an_h3_reader_in_one_task_wakes_after_a_writer_in_another_goes_idle() {
        let proxy = h3_proxy(ECHO);
        let native = native(&h3_proxy::client_tls(&proxy));
        let dial = hclient_native::testing::dial_for(&native);
        let t = bounded(dial.connect_tunnel(udp(proxy.port())))
            .await
            .unwrap();
        let (mut r, mut w) = t.stream.split();
        let reader = tokio::spawn(async move {
            let mut got = [0u8; 4];
            r.read_exact(&mut got).await.map(|()| got)
        });
        tokio::time::sleep(Duration::from_millis(200)).await;
        tokio::spawn(async move {
            w.write_all(b"ping").await.unwrap();
            w.flush().await.unwrap();
            std::future::pending::<()>().await;
        });
        let got = tokio::time::timeout(Duration::from_secs(5), reader)
            .await
            .expect("the reader was woken by the echo")
            .unwrap()
            .unwrap();
        assert_eq!(&got, b"ping");
    }

    /// A datagram receiver parked in one task is woken by the echo of what
    /// another task sent — quinn's endpoint driver receives while its
    /// connection driver sends.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_datagram_receiver_in_one_task_wakes_after_a_send_in_another() {
        let proxy = h3_proxy(ECHO);
        let native = native(&h3_proxy::client_tls(&proxy));
        let dial = hclient_native::testing::dial_for(&native);
        let t = bounded(dial.connect_tunnel(udp(proxy.port())))
            .await
            .unwrap();
        let d = std::sync::Arc::new(t.datagrams.expect("h3 tunnels carry datagrams"));
        let rd = std::sync::Arc::clone(&d);
        let receiver = tokio::spawn(async move {
            let mut buf = [0u8; 64];
            let n = std::future::poll_fn(|cx| rd.poll_recv(cx, &mut buf)).await?;
            Ok::<_, std::io::Error>(buf[..n].to_vec())
        });
        tokio::time::sleep(Duration::from_millis(200)).await;
        let sd = std::sync::Arc::clone(&d);
        tokio::spawn(async move { sd.try_send(b"hello").unwrap() })
            .await
            .unwrap();
        let got = tokio::time::timeout(Duration::from_secs(5), receiver)
            .await
            .expect("the receiver was woken by the echo")
            .unwrap()
            .unwrap();
        assert_eq!(got, b"hello");
    }

    /// A receiver parked in one task still wakes after another task polled
    /// the path once and walked away: nothing depends on having been the
    /// last to poll.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_parked_receiver_survives_another_task_polling_and_leaving() {
        let proxy = h3_proxy(ECHO);
        let native = native(&h3_proxy::client_tls(&proxy));
        let dial = hclient_native::testing::dial_for(&native);
        let t = bounded(dial.connect_tunnel(udp(proxy.port())))
            .await
            .unwrap();
        let d = std::sync::Arc::new(t.datagrams.expect("h3 tunnels carry datagrams"));
        let rd = std::sync::Arc::clone(&d);
        let receiver = tokio::spawn(async move {
            let mut buf = [0u8; 64];
            let n = std::future::poll_fn(|cx| rd.poll_recv(cx, &mut buf)).await?;
            Ok::<_, std::io::Error>(buf[..n].to_vec())
        });
        tokio::time::sleep(Duration::from_millis(200)).await;
        let bd = std::sync::Arc::clone(&d);
        tokio::spawn(async move {
            let mut buf = [0u8; 64];
            std::future::poll_fn(|cx| {
                assert!(bd.poll_recv(cx, &mut buf).is_pending());
                std::task::Poll::Ready(())
            })
            .await;
        })
        .await
        .unwrap();
        d.try_send(b"still").unwrap();
        let got = tokio::time::timeout(Duration::from_secs(5), receiver)
            .await
            .expect("the first receiver was still woken")
            .unwrap()
            .unwrap();
        assert_eq!(got, b"still");
    }

    /// A datagram larger than the path carries is refused, never cut.
    #[tokio::test(flavor = "multi_thread")]
    async fn an_oversized_datagram_is_invalid_input() {
        let proxy = h3_proxy(ECHO);
        let native = native(&h3_proxy::client_tls(&proxy));
        let dial = hclient_native::testing::dial_for(&native);
        let t = bounded(dial.connect_tunnel(udp(proxy.port())))
            .await
            .unwrap();
        let d = t.datagrams.unwrap();
        let big = vec![0u8; d.max_datagram_size() + 1];
        let e = d.try_send(&big).unwrap_err();
        assert_eq!(e.kind(), std::io::ErrorKind::InvalidInput);
        // Accepted; whether the proxy can send one this size back depends
        // on its own path, which is not this client's to promise.
        d.try_send(&vec![7u8; d.max_datagram_size()]).unwrap();
    }
}
