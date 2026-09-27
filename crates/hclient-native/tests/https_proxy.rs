//! A proxy reached over TLS: the connection to the proxy is encrypted,
//! checked with the transport's own trust, and everything a plain proxy
//! does happens inside it.

#![cfg(all(feature = "proxy", not(target_family = "wasm")))]

use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpListener};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc;
use std::time::Duration;

use hclient_core::body::RequestBody;
use hclient_core::error::{ErrorKind, Phase};
use hclient_core::req::Timeouts;
use hclient_core::transport::Transport;
use hclient_dns::{IpLiteralOnly, Overrides};
use hclient_native::Native;
use hclient_native::proxy::{HttpConnect, Proxy};
use hclient_rt_tokio::Tokio;
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

const BOUND: Duration = Duration::from_secs(10);
const PROXY: &str = "proxy.test";

fn identity(name: &str) -> (CertificateDer<'static>, PrivateKeyDer<'static>) {
    let cert = rcgen::generate_simple_self_signed(vec![name.into()]).expect("rcgen");
    (
        CertificateDer::from(cert.cert.der().to_vec()),
        PrivateKeyDer::try_from(cert.signing_key.serialize_der()).expect("pkcs8"),
    )
}

fn acceptor(name: &str) -> (tokio_rustls::TlsAcceptor, CertificateDer<'static>) {
    let (cert, key) = identity(name);
    let mut cfg = rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(vec![cert.clone()], key)
        .expect("cert and key match");
    cfg.alpn_protocols = vec![b"http/1.1".to_vec()];
    (tokio_rustls::TlsAcceptor::from(Arc::new(cfg)), cert)
}

/// What the TLS proxy saw on one connection: the SNI it was greeted with,
/// and the head of the first request inside the tunnel.
#[derive(Debug)]
struct Seen {
    sni: String,
    head: String,
}

/// An HTTP proxy behind TLS. `CONNECT` tunnels to `origin`; anything else
/// is answered `200 hi` as an origin would be, absolute-form. Reports what
/// it saw and how many TCP connections it accepted.
fn tls_proxy(
    origin: Option<SocketAddr>,
) -> (
    SocketAddr,
    CertificateDer<'static>,
    mpsc::Receiver<Seen>,
    Arc<AtomicUsize>,
) {
    let (acceptor, cert) = acceptor(PROXY);
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = listener.local_addr().expect("addr");
    let (tx, rx) = mpsc::channel();
    let accepted = Arc::new(AtomicUsize::new(0));
    let count = accepted.clone();
    std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("rt");
        rt.block_on(async move {
            listener.set_nonblocking(true).expect("nonblocking");
            let listener = tokio::net::TcpListener::from_std(listener).expect("adopt");
            while let Ok((tcp, _)) = listener.accept().await {
                count.fetch_add(1, Ordering::SeqCst);
                let acceptor = acceptor.clone();
                let tx = tx.clone();
                tokio::spawn(async move {
                    let Ok(mut tls) = acceptor.accept(tcp).await else {
                        return;
                    };
                    let sni = tls.get_ref().1.server_name().unwrap_or("<none>").to_owned();
                    loop {
                        let head = read_head_async(&mut tls).await;
                        if head.is_empty() {
                            return;
                        }
                        let _ = tx.send(Seen {
                            sni: sni.clone(),
                            head: head.clone(),
                        });
                        if head.starts_with("CONNECT ") {
                            let Some(origin) = origin else { return };
                            let Ok(mut up) = tokio::net::TcpStream::connect(origin).await else {
                                return;
                            };
                            let _ = tls
                                .write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
                                .await;
                            let _ = tokio::io::copy_bidirectional(&mut tls, &mut up).await;
                            return;
                        }
                        let _ = tls
                            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nhi")
                            .await;
                    }
                });
            }
        });
    });
    (addr, cert, rx, accepted)
}

async fn read_head_async<S: tokio::io::AsyncRead + Unpin>(s: &mut S) -> String {
    let mut buf = Vec::new();
    let mut b = [0u8; 1];
    while !buf.ends_with(b"\r\n\r\n") {
        match s.read(&mut b).await {
            Ok(1) => buf.push(b[0]),
            _ => return String::new(),
        }
    }
    String::from_utf8_lossy(&buf).into_owned()
}

/// A TLS origin that reports the SNI it was greeted with.
fn tls_origin() -> (SocketAddr, CertificateDer<'static>, mpsc::Receiver<String>) {
    let (acceptor, cert) = acceptor("localhost");
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = listener.local_addr().expect("addr");
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("rt");
        rt.block_on(async move {
            listener.set_nonblocking(true).expect("nonblocking");
            let listener = tokio::net::TcpListener::from_std(listener).expect("adopt");
            while let Ok((tcp, _)) = listener.accept().await {
                let acceptor = acceptor.clone();
                let tx = tx.clone();
                tokio::spawn(async move {
                    let Ok(mut tls) = acceptor.accept(tcp).await else {
                        return;
                    };
                    let _ = tx.send(tls.get_ref().1.server_name().unwrap_or("<none>").to_owned());
                    let _ = read_head_async(&mut tls).await;
                    let _ = tls
                        .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nhi")
                        .await;
                    let _ = tls.flush().await;
                });
            }
        });
    });
    (addr, cert, rx)
}

fn rustls_trusting(certs: &[CertificateDer<'static>]) -> hclient_tls_rustls::Rustls {
    let mut roots = rustls::RootCertStore::empty();
    for c in certs {
        roots.add(c.clone()).expect("a DER certificate");
    }
    hclient_tls_rustls::Rustls::from_config(Arc::new(
        rustls::ClientConfig::builder()
            .with_root_certificates(roots)
            .with_no_client_auth(),
    ))
}

fn dns() -> Overrides<IpLiteralOnly> {
    Overrides::new(IpLiteralOnly).host(PROXY, [IpAddr::V4(Ipv4Addr::LOCALHOST)])
}

fn rt() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("rt")
}

fn get<T: Transport<Error = hclient_core::error::Error>>(
    t: &T,
    rt: &tokio::runtime::Runtime,
    uri: &str,
) -> Result<http::StatusCode, hclient_core::error::Error> {
    rt.block_on(async {
        let resp = tokio::time::timeout(
            BOUND,
            t.execute(
                http::Request::builder()
                    .uri(uri)
                    .body(RequestBody::Empty)
                    .expect("request"),
            ),
        )
        .await
        .expect("must not hang")?;
        let status = resp.status();
        // Drained, so a keep-alive connection goes back to the pool — and
        // read, because every fixture here answers `hi`, and a status with
        // a lost body is not a response.
        let body = http_body_util::BodyExt::collect(resp.into_body())
            .await
            .unwrap_or_else(|_| panic!("the body reads to its end"))
            .to_bytes();
        assert_eq!(&body[..], b"hi");
        Ok(status)
    })
}

#[test]
fn plain_http_through_an_https_proxy_is_absolute_form_inside_tls() {
    let (proxy, cert, seen, _) = tls_proxy(None);
    let t = Native::new(Tokio, rustls_trusting(&[cert]), dns())
        .proxy(Proxy::new(HttpConnect::new(), PROXY, proxy.port()).tls());
    assert_eq!(
        get(&t, &rt(), "http://abs.test/p").expect("via the proxy"),
        200
    );
    let s = seen.recv_timeout(BOUND).expect("seen");
    assert_eq!(s.sni, PROXY);
    assert_eq!(
        s.head.lines().next().unwrap_or_default(),
        "GET http://abs.test/p HTTP/1.1"
    );
}

#[test]
fn https_through_an_https_proxy_is_tls_in_tls() {
    let (origin, origin_cert, origin_sni) = tls_origin();
    let (proxy, proxy_cert, seen, _) = tls_proxy(Some(origin));
    let t = Native::new(Tokio, rustls_trusting(&[proxy_cert, origin_cert]), dns())
        .proxy(Proxy::new(HttpConnect::new(), PROXY, proxy.port()).tls());
    let uri = format!("https://localhost:{}/x", origin.port());
    assert_eq!(get(&t, &rt(), &uri).expect("tunnelled"), 200);
    let s = seen.recv_timeout(BOUND).expect("seen");
    assert_eq!(s.sni, PROXY);
    assert_eq!(
        s.head.lines().next().unwrap_or_default(),
        format!("CONNECT localhost:{} HTTP/1.1", origin.port())
    );
    assert_eq!(
        origin_sni.recv_timeout(BOUND).expect("greeted"),
        "localhost"
    );
}

#[test]
fn an_untrusted_proxy_certificate_is_a_tls_error_and_reaches_no_origin() {
    let (origin, origin_cert, origin_sni) = tls_origin();
    let (proxy, _untrusted, _, _) = tls_proxy(Some(origin));
    let t = Native::new(Tokio, rustls_trusting(&[origin_cert]), dns())
        .proxy(Proxy::new(HttpConnect::new(), PROXY, proxy.port()).tls());
    let uri = format!("https://localhost:{}/x", origin.port());
    let err = get(&t, &rt(), &uri).expect_err("the proxy is not trusted");
    assert_eq!(*err.kind(), ErrorKind::Tls, "{err:?}");
    assert!(
        origin_sni.recv_timeout(Duration::from_millis(300)).is_err(),
        "nothing reached the origin"
    );
}

#[test]
fn two_requests_through_one_tls_proxy_share_its_connection() {
    let (proxy, cert, _seen, accepted) = tls_proxy(None);
    let t = Native::new(Tokio, rustls_trusting(&[cert]), dns())
        .proxy(Proxy::new(HttpConnect::new(), PROXY, proxy.port()).tls());
    let rt = rt();
    assert_eq!(get(&t, &rt, "http://a.test/").expect("first"), 200);
    assert_eq!(get(&t, &rt, "http://a.test/").expect("second"), 200);
    assert_eq!(accepted.load(Ordering::SeqCst), 1);
}

#[test]
fn a_silent_tls_proxy_times_out_at_the_callers_bound() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = listener.local_addr().expect("addr");
    std::thread::spawn(move || {
        let mut held = Vec::new();
        for s in listener.incoming().flatten() {
            held.push(s); // accepted, never answered
        }
    });
    let (_, cert) = acceptor(PROXY);
    let t = Native::new(Tokio, rustls_trusting(&[cert]), dns())
        .proxy(Proxy::new(HttpConnect::new(), PROXY, addr.port()).tls());
    let rt = rt();
    let began = std::time::Instant::now();
    let err = rt
        .block_on(async {
            let mut req = http::Request::builder()
                .uri("http://silent.test/")
                .body(RequestBody::Empty)
                .expect("request");
            req.extensions_mut()
                .insert(Timeouts::new().with_connect(Duration::from_millis(300)));
            tokio::time::timeout(BOUND, t.execute(req))
                .await
                .expect("must not hang")
        })
        .expect_err("the proxy never answers the ClientHello");
    assert!(
        matches!(*err.kind(), ErrorKind::Timeout(Phase::Connect)),
        "{err:?}"
    );
    assert!(began.elapsed() < Duration::from_secs(5));
}

mod erased {
    //! An external filter that asks the transport for TLS to its proxy
    //! through the erased context.

    use hclient_proxy::{
        Attempt, BoxDial, BoxOpening, Decision, Dial, EgressFilter, FilterSupport, HttpConnect, Io,
        Opened, ProxyTls, RequestForm, Route, SendEgressFilter, Target,
    };

    pub struct TlsTunnel {
        pub proxy: (String, u16),
    }

    impl EgressFilter for TlsTunnel {
        type Wrapped<S: Io> = S;

        fn route(&self, _: &Target<'_>) -> Decision<'_> {
            Decision::Filtered(Route::new(
                FilterSupport::STREAM,
                format!("tls-tunnel:{}:{}", self.proxy.0, self.proxy.1),
                RequestForm::Origin,
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
            let s = ctx
                .connect(&self.proxy.0, self.proxy.1)
                .await
                .map_err(Attempt::Failed)?;
            let mut s = ctx
                .connect_tls(s, ProxyTls::new(&self.proxy.0))
                .await
                .map_err(Attempt::Failed)?;
            hclient_proxy::drive_exact(&mut s, &mut HttpConnect::new(), t.host, t.port)
                .await
                .map_err(Attempt::Failed)?;
            Ok(Opened::Raw(s))
        }
    }

    impl SendEgressFilter for TlsTunnel {
        fn open_stream_send<'a>(&'a self, t: Target<'a>, ctx: &'a BoxDial<'a>) -> BoxOpening<'a> {
            Box::pin(async move { self.open_stream(t, ctx).await.map(hclient_proxy::erase) })
        }
    }
}

#[test]
fn an_external_filter_gets_tls_to_its_proxy_through_the_erased_context() {
    let (origin, origin_cert, origin_sni) = tls_origin();
    let (proxy, proxy_cert, seen, _) = tls_proxy(Some(origin));
    let t = Native::new(Tokio, rustls_trusting(&[proxy_cert, origin_cert]), dns()).egress(
        erased::TlsTunnel {
            proxy: (PROXY.to_owned(), proxy.port()),
        },
    );
    let uri = format!("https://localhost:{}/x", origin.port());
    assert_eq!(get(&t, &rt(), &uri).expect("tunnelled"), 200);
    assert_eq!(seen.recv_timeout(BOUND).expect("seen").sni, PROXY);
    assert_eq!(
        origin_sni.recv_timeout(BOUND).expect("greeted"),
        "localhost"
    );
}
