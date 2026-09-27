//! What one egress filter makes possible that the old proxy parameter did
//! not, watched from the fixtures' side of the wire: rules of different
//! protocols on one transport, a SOCKS5 proxy listening on a Unix socket,
//! a tunnel reused from the pool, and the caller's connect bound holding
//! through a proxy that never answers.
//!
//! Every fixture here is loopback and every proxy is addressed by an IP
//! literal, so `IpLiteralOnly` suffices and nothing resolves an origin's
//! name locally — which is itself part of what a proxy promises.
#![cfg(all(feature = "proxy", unix, not(target_family = "wasm")))]

use hclient_core::body::RequestBody;
use hclient_core::error::ErrorKind;
use hclient_core::transport::Transport;
use hclient_dns::IpLiteralOnly;
use hclient_native::Native;
use hclient_native::proxy::{HttpConnect, Proxy, Socks5};
use hclient_rt_tokio::Tokio;
use hclient_tls::NoTls;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc;
use std::time::Duration;

/// Never an assertion — it turns a mutation that hangs into a red test.
const BOUND: Duration = Duration::from_secs(10);

/// Reads a request head off `s`, up to and including the blank line.
fn read_head<S: Read>(s: &mut S) -> String {
    let mut head = Vec::new();
    let mut byte = [0u8; 1];
    while !head.ends_with(b"\r\n\r\n") {
        match s.read(&mut byte) {
            Ok(0) | Err(_) => break,
            Ok(_) => head.push(byte[0]),
        }
    }
    String::from_utf8_lossy(&head).into_owned()
}

/// A keep-alive answer: two bytes, and the connection stays open.
const KEEP_ALIVE: &[u8] = b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nhi";

/// Serve RFC 1928 §3–§4 on `s`, report the name it was asked for, then
/// answer every request that follows on the same connection — so a pooled
/// tunnel can be seen being reused.
fn serve_socks5<S: Read + Write>(mut s: S, asked: &mpsc::Sender<String>) {
    let mut hdr = [0u8; 2];
    if s.read_exact(&mut hdr).is_err() {
        return;
    }
    let mut methods = vec![0u8; usize::from(hdr[1])];
    let _ = s.read_exact(&mut methods);
    let _ = s.write_all(&[0x05, 0x00]);
    let mut req = [0u8; 4];
    if s.read_exact(&mut req).is_err() {
        return;
    }
    let mut len = [0u8; 1];
    let _ = s.read_exact(&mut len);
    let mut host = vec![0u8; usize::from(len[0])];
    let _ = s.read_exact(&mut host);
    let mut port = [0u8; 2];
    let _ = s.read_exact(&mut port);
    let _ = asked.send(String::from_utf8_lossy(&host).into_owned());
    let _ = s.write_all(&[0x05, 0x00, 0x00, 0x01, 0, 0, 0, 0, 0, 0]);
    loop {
        let head = read_head(&mut s);
        if head.is_empty() {
            return;
        }
        if s.write_all(KEEP_ALIVE).and_then(|()| s.flush()).is_err() {
            return;
        }
    }
}

/// A SOCKS5 proxy on TCP that answers HTTP itself; counts connections.
fn socks5_tcp() -> (SocketAddr, mpsc::Receiver<String>, Arc<AtomicUsize>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = listener.local_addr().expect("addr");
    let (tx, rx) = mpsc::channel();
    let accepted = Arc::new(AtomicUsize::new(0));
    let count = accepted.clone();
    std::thread::spawn(move || {
        for conn in listener.incoming() {
            let Ok(s) = conn else { break };
            count.fetch_add(1, Ordering::SeqCst);
            let tx = tx.clone();
            std::thread::spawn(move || serve_socks5(s, &tx));
        }
    });
    (addr, rx, accepted)
}

/// An HTTP proxy answering absolute-form requests itself; reports each
/// request head.
fn http_proxy() -> (SocketAddr, mpsc::Receiver<String>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = listener.local_addr().expect("addr");
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        for conn in listener.incoming() {
            let Ok(mut s) = conn else { break };
            let head = read_head(&mut s);
            let _ = tx.send(head);
            let _ =
                s.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nhi");
            let _ = s.flush();
        }
    });
    (addr, rx)
}

fn rt() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .expect("runtime")
}

fn get(
    t: &Native<Tokio, NoTls, IpLiteralOnly>,
    rt: &tokio::runtime::Runtime,
    uri: &str,
) -> Result<u16, hclient_core::error::Error> {
    get_with(t, rt, uri, None)
}

fn get_with(
    t: &Native<Tokio, NoTls, IpLiteralOnly>,
    rt: &tokio::runtime::Runtime,
    uri: &str,
    connect: Option<Duration>,
) -> Result<u16, hclient_core::error::Error> {
    let mut req = http::Request::builder()
        .uri(uri)
        .body(RequestBody::Empty)
        .expect("request");
    if let Some(c) = connect {
        req.extensions_mut()
            .insert(hclient_core::req::Timeouts::new().with_connect(c));
    }
    rt.block_on(async {
        let resp = tokio::time::timeout(BOUND, t.execute(req))
            .await
            .expect("must not hang")?;
        let status = resp.status().as_u16();
        // Drained, so a keep-alive connection goes back to the pool.
        let _ = http_body_util::BodyExt::collect(resp.into_body()).await;
        Ok(status)
    })
}

/// **An HTTP proxy and a SOCKS5 proxy on one transport**, which the old
/// single proxy type parameter could not express: the first rule serves
/// every host but the one it bypasses, and that one falls through to the
/// second rule. Each fixture's log is the witness.
#[test]
fn http_and_socks5_rules_serve_one_transport() {
    let (http_addr, http_seen) = http_proxy();
    let (socks_addr, socks_seen, _) = socks5_tcp();
    let t = Native::new(Tokio, NoTls, IpLiteralOnly)
        .proxy(
            Proxy::new(
                HttpConnect::new(),
                http_addr.ip().to_string(),
                http_addr.port(),
            )
            .bypass(["b.test"])
            .unwrap(),
        )
        .and_proxy(Proxy::new(
            Socks5::new(),
            socks_addr.ip().to_string(),
            socks_addr.port(),
        ));
    let rt = rt();

    assert_eq!(
        get(&t, &rt, "http://a.test/one").expect("via the HTTP proxy"),
        200
    );
    assert_eq!(
        http_seen
            .recv_timeout(BOUND)
            .expect("the HTTP proxy saw it")
            .lines()
            .next()
            .unwrap_or_default(),
        "GET http://a.test/one HTTP/1.1",
        "absolute-form, to the HTTP proxy"
    );

    assert_eq!(get(&t, &rt, "http://b.test/two").expect("via SOCKS5"), 200);
    assert_eq!(
        socks_seen
            .recv_timeout(BOUND)
            .expect("the SOCKS5 proxy saw it"),
        "b.test",
        "the origin's name, carried to the proxy rather than resolved here"
    );
    assert!(
        http_seen.try_recv().is_err(),
        "the bypassed host never reached the HTTP proxy"
    );
}

/// **A SOCKS5 proxy listening on a Unix socket** — Tor's
/// `SocksPort unix:/path` — reached through `proxy_over_ipc`. The old
/// transport could not say this: a proxy and a socket were exclusive.
#[test]
fn socks5_on_a_unix_socket_reaches_the_origin() {
    let dir = std::env::temp_dir().join(format!(
        "hc-eg-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    std::fs::create_dir_all(&dir).expect("temp dir");
    let path = dir.join("s");
    let _ = std::fs::remove_file(&path);
    let listener = std::os::unix::net::UnixListener::bind(&path).expect("bind");
    let (tx, asked) = mpsc::channel();
    std::thread::spawn(move || {
        for conn in listener.incoming() {
            let Ok(s) = conn else { break };
            let tx = tx.clone();
            std::thread::spawn(move || serve_socks5(s, &tx));
        }
    });

    let t = Native::new(Tokio, NoTls, IpLiteralOnly)
        .proxy_over_ipc(hclient_native::proxy::IpcProxy::new(
            Socks5::new(),
            hclient_rt::IpcAddr::unix(&path),
        ))
        .expect("tokio on unix opens same-machine connections");
    let rt = rt();

    assert_eq!(
        get(&t, &rt, "http://over-a-socket.test/").expect("through the socket"),
        200
    );
    assert_eq!(
        asked.recv_timeout(BOUND).expect("asked"),
        "over-a-socket.test"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// **A tunnel is pooled and reused through the same proxy**: two requests
/// to one origin through one SOCKS5 rule open one connection to the proxy.
///
/// The other half — two *different* proxies never sharing a connection —
/// is not observable inside one transport, because the route is a pure
/// function of the scheme, host and port the pool key already carries; it
/// is pinned where the key is made, in `hclient-proxy`'s `Rules` tests.
#[test]
fn a_tunnel_is_reused_through_the_same_proxy() {
    let (socks_addr, _asked, accepted) = socks5_tcp();
    let t = Native::new(Tokio, NoTls, IpLiteralOnly).proxy(Proxy::new(
        Socks5::new(),
        socks_addr.ip().to_string(),
        socks_addr.port(),
    ));
    let rt = rt();

    assert_eq!(get(&t, &rt, "http://pooled.test/1").expect("first"), 200);
    assert_eq!(get(&t, &rt, "http://pooled.test/2").expect("second"), 200);
    assert_eq!(accepted.load(Ordering::SeqCst), 1, "one tunnel, reused");
}

/// **The caller's connect bound holds through a proxy that never
/// answers.** The proxy accepts and then says nothing, so the SOCKS5
/// greeting waits for ever; the request must end at the caller's bound as
/// a timeout, and the hang guard around it must not be what ends it.
#[test]
fn a_black_holed_proxy_times_out_at_the_callers_bound() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = listener.local_addr().expect("addr");
    std::thread::spawn(move || {
        let mut held = Vec::new();
        // Held open and never answered.
        for s in listener.incoming().flatten() {
            held.push(s);
        }
    });
    let t = Native::new(Tokio, NoTls, IpLiteralOnly).proxy(Proxy::new(
        Socks5::new(),
        addr.ip().to_string(),
        addr.port(),
    ));
    let rt = rt();

    let began = std::time::Instant::now();
    let err = get_with(
        &t,
        &rt,
        "http://silent.test/",
        Some(Duration::from_millis(300)),
    )
    .expect_err("the proxy never answers");
    assert!(
        matches!(
            *err.kind(),
            ErrorKind::Timeout(hclient_core::error::Phase::Connect)
        ),
        "a connect timeout: {err:?}"
    );
    assert!(
        began.elapsed() < Duration::from_secs(5),
        "ended by the caller's bound"
    );
}

// --- a filter written outside this crate ---------------------------------

mod xor {
    //! A filter this crate did not write: it reaches a relay by name
    //! through the transport's own connect path and XORs every byte on the
    //! way, so the stream it hands back is a *different type* from the
    //! runtime's — the case `Opened::Wrapped` exists for.

    use futures_io::{AsyncRead, AsyncWrite};
    use hclient_proxy::{
        Attempt, BoxDial, BoxIo, BoxOpening, Decision, Dial, EgressFilter, FilterSupport, Io,
        Opened, RequestForm, Route, SendEgressFilter, Target,
    };
    use std::pin::Pin;
    use std::task::{Context, Poll};

    pub struct XorStream<S> {
        inner: S,
        key: u8,
    }
    impl<S: AsyncRead + Unpin> AsyncRead for XorStream<S> {
        fn poll_read(
            mut self: Pin<&mut Self>,
            cx: &mut Context<'_>,
            buf: &mut [u8],
        ) -> Poll<std::io::Result<usize>> {
            let key = self.key;
            let r = Pin::new(&mut self.inner).poll_read(cx, buf);
            if let Poll::Ready(Ok(n)) = r {
                for b in &mut buf[..n] {
                    *b ^= key;
                }
            }
            r
        }
    }
    impl<S: AsyncWrite + Unpin> AsyncWrite for XorStream<S> {
        fn poll_write(
            mut self: Pin<&mut Self>,
            cx: &mut Context<'_>,
            buf: &[u8],
        ) -> Poll<std::io::Result<usize>> {
            // One byte at a time keeps a short write from desynchronising
            // the key stream, which is all a test double needs.
            let Some(&first) = buf.first() else {
                return Poll::Ready(Ok(0));
            };
            let key = self.key;
            Pin::new(&mut self.inner).poll_write(cx, &[first ^ key])
        }
        fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
            Pin::new(&mut self.inner).poll_flush(cx)
        }
        fn poll_close(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
            Pin::new(&mut self.inner).poll_close(cx)
        }
    }
    impl<S: hclient_rt::Shutdown + Unpin> hclient_rt::Shutdown for XorStream<S> {
        fn poll_shutdown(
            mut self: Pin<&mut Self>,
            cx: &mut Context<'_>,
        ) -> Poll<std::io::Result<()>> {
            Pin::new(&mut self.inner).poll_shutdown(cx)
        }
    }

    /// Carries requests for `only` through an XOR relay; everything else is
    /// direct.
    pub struct Xor {
        pub relay: (String, u16),
        pub key: u8,
        pub only: &'static str,
    }

    impl EgressFilter for Xor {
        type Wrapped<S: Io> = XorStream<S>;

        fn route(&self, t: &Target<'_>) -> Decision<'_> {
            if t.host == self.only {
                Decision::Filtered(Route::new(
                    FilterSupport::STREAM,
                    format!("xor:{}:{}", self.relay.0, self.relay.1),
                    RequestForm::Origin,
                ))
            } else {
                Decision::Direct
            }
        }

        async fn open_stream<'a, C: Dial + 'a>(
            &'a self,
            _t: Target<'a>,
            ctx: &'a C,
        ) -> Result<Opened<C::Stream, XorStream<C::Stream>>, Attempt>
        where
            Self: Sized,
        {
            let inner = ctx
                .connect(&self.relay.0, self.relay.1)
                .await
                .map_err(Attempt::Failed)?;
            Ok(Opened::Wrapped(XorStream {
                inner,
                key: self.key,
            }))
        }
    }

    impl SendEgressFilter for Xor {
        fn open_stream_send<'a>(&'a self, t: Target<'a>, ctx: &'a BoxDial<'a>) -> BoxOpening<'a> {
            Box::pin(async move { self.open_stream(t, ctx).await.map(hclient_proxy::erase) })
        }
    }

    #[allow(dead_code, reason = "names the type the filter hands back")]
    type _Check = BoxIo;
}

/// An origin that answers every request, over a connection whose bytes
/// are `XOR`ed with `key` in both directions.
fn xor_origin(key: u8) -> (SocketAddr, Arc<AtomicUsize>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = listener.local_addr().expect("addr");
    let served = Arc::new(AtomicUsize::new(0));
    let count = served.clone();
    std::thread::spawn(move || {
        for mut s in listener.incoming().flatten() {
            let count = count.clone();
            std::thread::spawn(move || {
                let mut head = Vec::new();
                let mut byte = [0u8; 1];
                while !head.ends_with(b"\r\n\r\n") {
                    match s.read(&mut byte) {
                        Ok(1) => head.push(byte[0] ^ key),
                        _ => return,
                    }
                }
                count.fetch_add(1, Ordering::SeqCst);
                let reply: Vec<u8> = KEEP_ALIVE.iter().map(|b| b ^ key).collect();
                let _ = s.write_all(&reply);
            });
        }
    });
    (addr, served)
}

/// **A filter from outside the crate wraps the stream**, and the transport
/// speaks HTTP through the wrapper: the origin only understands `XOR`ed
/// bytes, so a `200` is the proof the wrapper carried the request.
#[test]
fn an_external_filter_may_wrap_the_stream() {
    let key = 0x5a;
    let (relay, served) = xor_origin(key);
    let t = Native::new(Tokio, NoTls, IpLiteralOnly).egress(xor::Xor {
        relay: (relay.ip().to_string(), relay.port()),
        key,
        only: "wrapped.test",
    });
    let rt = rt();
    assert_eq!(
        get(&t, &rt, "http://wrapped.test/").expect("through the XOR relay"),
        200
    );
    assert_eq!(served.load(Ordering::SeqCst), 1);
}

/// **What an external filter declines goes to the built-in rules**, and
/// only what both decline is direct: the filter serves one host, a SOCKS5
/// rule serves the rest.
#[test]
fn an_external_filter_is_asked_before_the_built_in_rules() {
    let key = 0x33;
    let (relay, served) = xor_origin(key);
    let (socks_addr, socks_seen, _) = socks5_tcp();
    let t = Native::new(Tokio, NoTls, IpLiteralOnly)
        .proxy(Proxy::new(
            Socks5::new(),
            socks_addr.ip().to_string(),
            socks_addr.port(),
        ))
        .egress(xor::Xor {
            relay: (relay.ip().to_string(), relay.port()),
            key,
            only: "wrapped.test",
        });
    let rt = rt();
    assert_eq!(
        get(&t, &rt, "http://wrapped.test/").expect("the filter's"),
        200
    );
    assert_eq!(served.load(Ordering::SeqCst), 1);
    assert_eq!(get(&t, &rt, "http://other.test/").expect("the rules'"), 200);
    assert_eq!(
        socks_seen.recv_timeout(BOUND).expect("SOCKS5 saw it"),
        "other.test"
    );
    assert!(
        socks_seen.try_recv().is_err(),
        "the filtered host never reached SOCKS5"
    );
}

mod deny {
    //! A filter that carries its target by *refusing* it: it answers
    //! `Filtered` with no support at all, the natural shape of a deny list.

    use hclient_proxy::{
        Attempt, BoxDial, BoxOpening, Decision, Dial, EgressFilter, FilterSupport, Io, Opened,
        RequestForm, Route, SendEgressFilter, Target,
    };
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    pub struct Deny {
        pub opened: Arc<AtomicUsize>,
    }

    impl EgressFilter for Deny {
        type Wrapped<S: Io> = S;

        fn route(&self, _t: &Target<'_>) -> Decision<'_> {
            Decision::Filtered(Route::new(FilterSupport::NONE, "deny", RequestForm::Origin))
        }

        fn open_stream<'a, C: Dial + 'a>(
            &'a self,
            _t: Target<'a>,
            _ctx: &'a C,
        ) -> impl std::future::Future<Output = Result<Opened<C::Stream, C::Stream>, Attempt>> + 'a
        where
            Self: Sized,
        {
            self.opened.fetch_add(1, Ordering::SeqCst);
            std::future::ready(Err(Attempt::Failed(hclient_core::error::Error::new(
                hclient_core::error::ErrorKind::Connect,
                std::io::Error::other("should never have been asked"),
            ))))
        }
    }

    impl SendEgressFilter for Deny {
        fn open_stream_send<'a>(&'a self, t: Target<'a>, ctx: &'a BoxDial<'a>) -> BoxOpening<'a> {
            Box::pin(async move { self.open_stream(t, ctx).await.map(hclient_proxy::erase) })
        }
    }
}

/// **A filter that declares no stream is refused before it is asked to
/// open one**, with a typed error naming it — the capability is read, so a
/// deny list is a thing a filter can say rather than a thing it must
/// implement by failing.
#[test]
fn a_filter_that_declares_no_stream_is_refused_before_dialling() {
    let opened = Arc::new(AtomicUsize::new(0));
    let t = Native::new(Tokio, NoTls, IpLiteralOnly).egress(deny::Deny {
        opened: opened.clone(),
    });
    let rt = rt();
    let err = get(&t, &rt, "http://denied.test/").expect_err("the filter carries no stream");
    assert_eq!(*err.kind(), ErrorKind::Unsupported, "{err:?}");
    let refusal = std::error::Error::source(&err)
        .and_then(|s| s.downcast_ref::<hclient_native::error::NoStreamPath>())
        .unwrap_or_else(|| panic!("a typed refusal: {err:?}"));
    assert_eq!(&*refusal.via, "deny");
    assert_eq!(
        opened.load(Ordering::SeqCst),
        0,
        "open_stream was never asked"
    );
}

// --- the external path, beyond wrapping ----------------------------------
//
// Copied from `tests/proxy.rs`: integration tests share no modules here.

fn ok_response() -> &'static [u8] {
    b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nhi"
}

use rustls::pki_types::{CertificateDer, PrivateKeyDer};

fn identity() -> (CertificateDer<'static>, PrivateKeyDer<'static>) {
    let cert = rcgen::generate_simple_self_signed(vec!["localhost".into()])
        .expect("rcgen can always make a self-signed cert");
    (
        CertificateDer::from(cert.cert.der().to_vec()),
        PrivateKeyDer::try_from(cert.signing_key.serialize_der()).expect("pkcs8 from rcgen"),
    )
}

/// A TLS origin that reports **the server name it was greeted with**.
///
/// That is the whole assertion: over a tunnel the certificate is still the
/// origin's, so a client that sent the proxy's name would fail the
/// handshake — but it would also fail if it sent nothing, and the two are
/// different defects. The name is read off the accepted connection rather
/// than inferred from the request succeeding.
fn tls_origin() -> (SocketAddr, CertificateDer<'static>, mpsc::Receiver<String>) {
    let (cert_der, key_der) = identity();
    let mut cfg = rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(vec![cert_der.clone()], key_der)
        .expect("the cert and key were made together");
    cfg.alpn_protocols = vec![b"http/1.1".to_vec()];
    let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(cfg));

    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = listener.local_addr().expect("addr");
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");
        rt.block_on(async move {
            listener.set_nonblocking(true).expect("nonblocking");
            let listener = tokio::net::TcpListener::from_std(listener).expect("adopt");
            while let Ok((tcp, _)) = listener.accept().await {
                let acceptor = acceptor.clone();
                let tx = tx.clone();
                tokio::spawn(async move {
                    use tokio::io::{AsyncReadExt, AsyncWriteExt};

                    let Ok(mut tls) = acceptor.accept(tcp).await else {
                        return;
                    };
                    let sni = tls.get_ref().1.server_name().unwrap_or("<none>").to_owned();
                    let _ = tx.send(sni);
                    let mut buf = [0u8; 1024];
                    let _ = tls.read(&mut buf).await;
                    let _ = tls.write_all(ok_response()).await;
                    let _ = tls.flush().await;
                });
            }
        });
    });
    (addr, cert_der, rx)
}

/// An HTTP proxy that tunnels: `200`, then bytes both ways, and it reports
/// the authority it was asked for.
fn tunnelling_proxy(origin: SocketAddr) -> (SocketAddr, mpsc::Receiver<String>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = listener.local_addr().expect("addr");
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        for conn in listener.incoming() {
            let Ok(mut client) = conn else { break };
            let head = read_head(&mut client);
            let target = head
                .lines()
                .next()
                .and_then(|l| l.split_whitespace().nth(1))
                .unwrap_or_default()
                .to_owned();
            let _ = tx.send(target);
            let Ok(mut upstream) = TcpStream::connect(origin) else {
                continue;
            };
            if client
                .write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
                .is_err()
            {
                continue;
            }
            let (mut c2, mut u2) = (
                client.try_clone().expect("clone"),
                upstream.try_clone().expect("clone"),
            );
            std::thread::spawn(move || {
                let _ = std::io::copy(&mut c2, &mut u2);
            });
            std::thread::spawn(move || {
                let _ = std::io::copy(&mut upstream, &mut client);
            });
        }
    });
    (addr, rx)
}

mod tunnel {
    //! An external filter that reaches an HTTP proxy through the lent
    //! connect path and opens a `CONNECT` tunnel — the built-in behaviour,
    //! written from outside, so the external path is exercised on its own.

    use hclient_proxy::{
        Attempt, BoxDial, BoxOpening, Decision, Dial, EgressFilter, FilterSupport, HttpConnect, Io,
        Opened, RequestForm, Route, SendEgressFilter, Target,
    };

    pub struct Tunnel {
        pub proxy: (String, u16),
        /// When set, `http://` requests are written absolute-form with this
        /// header instead of tunnelled.
        pub absolute: Option<http::HeaderValue>,
    }

    impl EgressFilter for Tunnel {
        type Wrapped<S: Io> = S;

        fn route(&self, t: &Target<'_>) -> Decision<'_> {
            let form = match (&self.absolute, t.use_tls) {
                (Some(auth), false) => RequestForm::absolute(Some(auth.clone())),
                _ => RequestForm::Origin,
            };
            Decision::Filtered(Route::new(
                FilterSupport::STREAM,
                format!("tunnel:{}:{}", self.proxy.0, self.proxy.1),
                form,
            ))
        }

        async fn open_stream<'a, C: Dial + 'a>(
            &'a self,
            t: Target<'a>,
            ctx: &'a C,
        ) -> Result<Opened<C::Stream, C::Stream>, Attempt>
        where
            Self: Sized,
        {
            let mut s = ctx
                .connect(&self.proxy.0, self.proxy.1)
                .await
                .map_err(Attempt::Failed)?;
            if self.absolute.is_some() && !t.use_tls {
                return Ok(Opened::Raw(s));
            }
            hclient_proxy::drive_exact(&mut s, &mut HttpConnect::new(), t.host, t.port)
                .await
                .map_err(Attempt::Failed)?;
            Ok(Opened::Raw(s))
        }
    }

    impl SendEgressFilter for Tunnel {
        fn open_stream_send<'a>(&'a self, t: Target<'a>, ctx: &'a BoxDial<'a>) -> BoxOpening<'a> {
            Box::pin(async move { self.open_stream(t, ctx).await.map(hclient_proxy::erase) })
        }
    }
}

/// **TLS to the origin over an external filter's stream**, with the
/// origin's own name as SNI: the certificate is `localhost`'s and the
/// proxy is `127.0.0.1`, so a handshake that used the proxy's name, or
/// none, fails.
#[test]
fn an_external_filter_carries_https() {
    let (origin, cert, sni) = tls_origin();
    let (proxy, asked) = tunnelling_proxy(origin);
    let mut roots = rustls::RootCertStore::empty();
    roots.add(cert).expect("a DER certificate");
    let tls = hclient_tls_rustls::Rustls::from_config(Arc::new(
        rustls::ClientConfig::builder()
            .with_root_certificates(roots)
            .with_no_client_auth(),
    ));
    let t = Native::new(Tokio, tls, IpLiteralOnly).egress(tunnel::Tunnel {
        proxy: (proxy.ip().to_string(), proxy.port()),
        absolute: None,
    });
    let rt = rt();
    let status = rt.block_on(async {
        tokio::time::timeout(
            BOUND,
            t.execute(
                http::Request::builder()
                    .uri(format!("https://localhost:{}/x", origin.port()))
                    .body(RequestBody::Empty)
                    .expect("request"),
            ),
        )
        .await
        .expect("must not hang")
        .expect("the tunnelled https request")
        .status()
    });
    assert_eq!(status, 200);
    assert_eq!(sni.recv_timeout(BOUND).expect("greeted"), "localhost");
    assert_eq!(
        asked.recv_timeout(BOUND).expect("asked"),
        format!("localhost:{}", origin.port())
    );
}

/// A proxy that grants the tunnel and speaks first: the `200` and some
/// bytes of its own arrive in one write.
fn chatty_proxy() -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = listener.local_addr().expect("addr");
    std::thread::spawn(move || {
        let mut held = Vec::new();
        for conn in listener.incoming() {
            let Ok(mut client) = conn else { break };
            let _ = read_head(&mut client);
            let _ = client.write_all(b"HTTP/1.1 200 Connection Established\r\n\r\nextra");
            held.push(client);
        }
    });
    addr
}

/// **Bytes a proxy sends past its handshake are refused through an
/// external filter too** — `drive_exact` names them rather than handing
/// them to HTTP as if the origin had sent them.
#[test]
fn an_external_filter_refuses_bytes_past_the_handshake() {
    let proxy = chatty_proxy();
    let t = Native::new(Tokio, NoTls, IpLiteralOnly).egress(tunnel::Tunnel {
        proxy: (proxy.ip().to_string(), proxy.port()),
        absolute: None,
    });
    let rt = rt();
    let err = get(&t, &rt, "http://chatty.test/").expect_err("the proxy spoke first");
    assert_eq!(*err.kind(), ErrorKind::Connect, "{err:?}");
    let mut source = std::error::Error::source(&err);
    let mut spoke = None;
    while let Some(s) = source {
        if let Some(p) = s.downcast_ref::<hclient_proxy::ProxySpokeFirst>() {
            spoke = Some(p.bytes);
        }
        source = s.source();
    }
    assert_eq!(spoke, Some(5), "{err:?}");
}

#[derive(Clone, Default)]
struct Remotes(Arc<std::sync::Mutex<Vec<Option<SocketAddr>>>>);

impl hclient_core::hooks::Hooks for Remotes {
    fn on(&self, event: &hclient_core::hooks::Event<'_>) {
        if let hclient_core::hooks::Event::Connected(c) = event {
            self.0.lock().expect("hook log").push(c.remote);
        }
    }
}

/// **A hook sees the address an external filter dialled** — the relay,
/// the first hop — whatever the filter wrapped around it.
#[test]
fn a_hook_sees_the_first_hop_of_an_external_filter() {
    let key = 0x21;
    let (relay, _served) = xor_origin(key);
    let hooks = Remotes::default();
    let t = Native::new(Tokio, NoTls, IpLiteralOnly)
        .hooks(hooks.clone())
        .egress(xor::Xor {
            relay: (relay.ip().to_string(), relay.port()),
            key,
            only: "wrapped.test",
        });
    let rt = rt();
    let status = rt.block_on(async {
        tokio::time::timeout(
            BOUND,
            t.execute(
                http::Request::builder()
                    .uri("http://wrapped.test/")
                    .body(RequestBody::Empty)
                    .expect("request"),
            ),
        )
        .await
        .expect("must not hang")
        .expect("through the relay")
        .status()
    });
    assert_eq!(status, 200);
    assert_eq!(*hooks.0.lock().expect("hook log"), [Some(relay)]);
}

/// **An external filter may ask for absolute-form**, and the request line
/// and its `Proxy-Authorization` reach the proxy as the filter said.
#[test]
fn an_external_filter_may_answer_absolute_form() {
    let (proxy, seen) = http_proxy();
    let t = Native::new(Tokio, NoTls, IpLiteralOnly).egress(tunnel::Tunnel {
        proxy: (proxy.ip().to_string(), proxy.port()),
        absolute: Some(http::HeaderValue::from_static("Basic dTpw")),
    });
    let rt = rt();
    assert_eq!(
        get(&t, &rt, "http://abs.test/p").expect("via the proxy"),
        200
    );
    let head = seen.recv_timeout(BOUND).expect("seen");
    assert_eq!(
        head.lines().next().unwrap_or_default(),
        "GET http://abs.test/p HTTP/1.1"
    );
    assert!(
        head.to_ascii_lowercase()
            .contains("proxy-authorization: basic dtpw"),
        "{head}"
    );
}

/// **The caller's connect bound holds through an external filter whose
/// proxy never answers**: its `CONNECT` waits for a reply that never comes,
/// inside the connect phase.
#[test]
fn a_black_holed_external_relay_times_out_at_the_callers_bound() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = listener.local_addr().expect("addr");
    std::thread::spawn(move || {
        let mut held = Vec::new();
        // Held open and never answered.
        for s in listener.incoming().flatten() {
            held.push(s);
        }
    });
    let t = Native::new(Tokio, NoTls, IpLiteralOnly).egress(tunnel::Tunnel {
        proxy: (addr.ip().to_string(), addr.port()),
        absolute: None,
    });
    let rt = rt();
    let began = std::time::Instant::now();
    let err = get_with(
        &t,
        &rt,
        "http://silent.test/",
        Some(Duration::from_millis(300)),
    )
    .expect_err("the relay never answers");
    assert!(
        matches!(
            *err.kind(),
            ErrorKind::Timeout(hclient_core::error::Phase::Connect)
        ),
        "{err:?}"
    );
    assert!(began.elapsed() < Duration::from_secs(5));
}
