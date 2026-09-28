//! MASQUE proxies and origins, all real servers on loopback.
//!
//! The proxies really forward. A CONNECT-UDP tunnel is relayed to a UDP
//! socket aimed at the target the template names — over HTTP/3 as the
//! proxy's own datagrams (quarter stream id, then context id, then the
//! payload), over HTTP/2 as DATAGRAM capsules on the stream — and a plain
//! CONNECT is spliced to a TCP connection to the authority it names. So a
//! response that arrives came from the origin, through the proxy.
//!
//! The wire formats are written out here rather than borrowed from the
//! crate under test: a client and a fixture agreeing because they are one
//! function would prove nothing.
#![allow(
    dead_code,
    reason = "a fixture module shared by several test files, each of which uses its own subset"
)]

#[path = "../../../hclient-native/tests/servers.rs"]
mod servers;

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use bytes::{Buf as _, Bytes};
use hclient_rt_tokio::Tokio;
use rustls::pki_types::CertificateDer;
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::UdpSocket;

/// A server whose certificate a client must trust.
pub trait Trusted {
    /// The certificate it presents.
    fn cert(&self) -> &CertificateDer<'static>;
}

/// An HTTP/3 server and an HTTP/1.1-over-TLS server on one port, one
/// certificate naming `127.0.0.1`.
pub struct Origin(servers::Pair);

impl Origin {
    /// `https://127.0.0.1:<port>/`.
    pub fn url_ip(&self) -> String {
        format!("https://127.0.0.1:{}/", self.0.port)
    }
    /// HTTP/3 requests the origin answered.
    pub fn h3_answered(&self) -> usize {
        self.0.quic_answered()
    }
    /// HTTP/1.1 requests the origin answered.
    pub fn h1_answered(&self) -> usize {
        self.0.tcp_answered()
    }
}

impl Trusted for Origin {
    fn cert(&self) -> &CertificateDer<'static> {
        &self.0.cert_der
    }
}

/// An origin speaking HTTP/3 on UDP.
#[allow(
    clippy::unused_async,
    reason = "every fixture starts behind one `.await`, so a test reads the same whichever kind of server it stands up"
)]
pub async fn h3_origin() -> Origin {
    Origin(servers::start())
}

/// An origin speaking HTTP/3 on UDP and HTTP/1.1 over TLS on TCP, on the
/// same port, so the version of a response says which way it came.
#[allow(
    clippy::unused_async,
    reason = "every fixture starts behind one `.await`, so a test reads the same whichever kind of server it stands up"
)]
pub async fn h3_and_h1_origin() -> Origin {
    Origin(servers::start())
}

/// Have the origin advertise HTTP/3 on its own port, and send one request
/// through `client` to hear it — over a byte stream, since nothing has
/// said HTTP/3 yet.
///
/// # Panics
///
/// If the request fails, or comes back over anything but HTTP/1.1.
pub async fn seed_alt_svc(client: &hclient::Client, origin: &Origin) {
    origin.0.set_alt_svc(Some(&origin.0.h3_here("; ma=86400")));
    let r = bounded(client.get(origin.url_ip()).send())
        .await
        .expect("the seed request");
    assert_eq!(r.version(), http::Version::HTTP_11, "no signal yet");
}

/// A plain-text HTTP/1.1 origin on TCP.
pub struct PlainOrigin {
    port: u16,
    answered: Arc<AtomicUsize>,
}

impl PlainOrigin {
    /// `http://127.0.0.1:<port>/`.
    pub fn url_ip_http(&self) -> String {
        format!("http://127.0.0.1:{}/", self.port)
    }
    /// Requests answered.
    pub fn answered(&self) -> usize {
        self.answered.load(Ordering::SeqCst)
    }
}

/// Start a plain-text HTTP/1.1 origin that answers every request `200`
/// with `h1` and closes.
pub async fn h1_origin() -> PlainOrigin {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let port = listener.local_addr().expect("local_addr").port();
    let answered = Arc::new(AtomicUsize::new(0));
    let counted = Arc::clone(&answered);
    tokio::spawn(async move {
        while let Ok((mut sock, _)) = listener.accept().await {
            let counted = Arc::clone(&counted);
            tokio::spawn(async move {
                let mut head = Vec::new();
                let mut byte = [0u8; 1];
                while !head.ends_with(b"\r\n\r\n") {
                    match sock.read(&mut byte).await {
                        Ok(1) => head.push(byte[0]),
                        _ => return,
                    }
                }
                counted.fetch_add(1, Ordering::SeqCst);
                let _ = sock
                    .write_all(
                        b"HTTP/1.1 200 OK\r\ncontent-length: 2\r\nconnection: close\r\n\r\nh1",
                    )
                    .await;
                let _ = sock.shutdown().await;
            });
        }
    });
    PlainOrigin { port, answered }
}

/// What a MASQUE proxy counted.
#[derive(Default)]
struct Counters {
    /// HTTP/3 datagrams relayed, both ways.
    datagrams: AtomicUsize,
    /// DATAGRAM capsules relayed, both ways.
    capsules: AtomicUsize,
    /// Plain CONNECTs spliced to a TCP target.
    tcp_tunnels: AtomicUsize,
    /// CONNECT-UDP requests refused.
    udp_refusals: AtomicUsize,
    /// Connections accepted — QUIC over HTTP/3, TCP over HTTP/2.
    accepted: AtomicUsize,
}

/// A running MASQUE proxy.
pub struct MasqueProxy {
    port: u16,
    cert: CertificateDer<'static>,
    counters: Arc<Counters>,
}

impl MasqueProxy {
    /// The port it listens on.
    pub fn port(&self) -> u16 {
        self.port
    }
    /// HTTP/3 datagrams relayed, both ways.
    pub fn datagrams_forwarded(&self) -> usize {
        self.counters.datagrams.load(Ordering::SeqCst)
    }
    /// DATAGRAM capsules relayed, both ways.
    pub fn capsules_forwarded(&self) -> usize {
        self.counters.capsules.load(Ordering::SeqCst)
    }
    /// Plain CONNECTs spliced to a TCP target.
    pub fn tcp_tunnels(&self) -> usize {
        self.counters.tcp_tunnels.load(Ordering::SeqCst)
    }
    /// CONNECT-UDP requests refused.
    pub fn udp_refusals(&self) -> usize {
        self.counters.udp_refusals.load(Ordering::SeqCst)
    }
    /// Connections accepted.
    pub fn accepted(&self) -> usize {
        self.counters.accepted.load(Ordering::SeqCst)
    }
}

impl Trusted for MasqueProxy {
    fn cert(&self) -> &CertificateDer<'static> {
        &self.cert
    }
}

/// A certificate naming `localhost` and both loopback literals.
fn identity() -> (
    CertificateDer<'static>,
    rustls::pki_types::PrivateKeyDer<'static>,
) {
    let cert = rcgen::generate_simple_self_signed(vec![
        "localhost".into(),
        "127.0.0.1".into(),
        "::1".into(),
    ])
    .expect("rcgen can always make a self-signed cert");
    (
        CertificateDer::from(cert.cert.der().to_vec()),
        rustls::pki_types::PrivateKeyDer::try_from(cert.signing_key.serialize_der())
            .expect("a key rcgen just produced"),
    )
}

/// Where a proxy listens: every address, both families where the host has
/// IPv6 — `localhost` resolves to `::1` first on most hosts — and the IPv4
/// loopback where it has not.
fn listen_addrs() -> [SocketAddr; 2] {
    [
        SocketAddr::from((std::net::Ipv6Addr::UNSPECIFIED, 0)),
        SocketAddr::from(([127, 0, 0, 1], 0)),
    ]
}

/// RFC 9000 §16.
fn put_varint(buf: &mut Vec<u8>, v: u64) {
    let bytes = v.to_be_bytes();
    if v < 1 << 6 {
        buf.push(bytes[7]);
    } else if v < 1 << 14 {
        buf.push(0x40 | bytes[6]);
        buf.push(bytes[7]);
    } else if v < 1 << 30 {
        buf.push(0x80 | bytes[4]);
        buf.extend_from_slice(&bytes[5..]);
    } else {
        buf.push(0xc0 | bytes[0]);
        buf.extend_from_slice(&bytes[1..]);
    }
}

fn get_varint(buf: &[u8]) -> Option<(u64, usize)> {
    let first = *buf.first()?;
    let len = 1usize << (first >> 6);
    if buf.len() < len {
        return None;
    }
    let mut v = u64::from(first & 0x3f);
    for b in &buf[1..len] {
        v = (v << 8) | u64::from(*b);
    }
    Some((v, len))
}

/// The target of RFC 9298's default template,
/// `/.well-known/masque/udp/{target_host}/{target_port}/`.
fn target_of(path: &str) -> Option<(String, u16)> {
    let rest = path.strip_prefix("/.well-known/masque/udp/")?;
    let mut parts = rest.trim_end_matches('/').split('/');
    let host = parts.next()?.replace("%3A", ":").replace("%3a", ":");
    let port = parts.next()?.parse().ok()?;
    Some((host, port))
}

/// A UDP socket connected to `host:port`.
async fn udp_to(host: &str, port: u16) -> Option<Arc<UdpSocket>> {
    let target = tokio::net::lookup_host((host, port)).await.ok()?.next()?;
    let local: SocketAddr = if target.is_ipv4() {
        SocketAddr::from(([0, 0, 0, 0], 0))
    } else {
        SocketAddr::from((std::net::Ipv6Addr::UNSPECIFIED, 0))
    };
    let sock = UdpSocket::bind(local).await.ok()?;
    sock.connect(target).await.ok()?;
    Some(Arc::new(sock))
}

fn capsule_protocol() -> http::response::Builder {
    http::Response::builder()
        .status(200)
        .header("capsule-protocol", "?1")
}

// --- over HTTP/3 --------------------------------------------------------

/// A MASQUE proxy spoken to over HTTP/3, relaying CONNECT-UDP over its
/// datagrams. A plain CONNECT is answered `501` and counted as a TCP
/// tunnel asked for.
#[allow(
    clippy::unused_async,
    reason = "every fixture starts behind one `.await`, so a test reads the same whichever kind of server it stands up"
)]
pub async fn masque_h3() -> MasqueProxy {
    let (cert, key) = identity();
    let mut tls = rustls::ServerConfig::builder_with_protocol_versions(&[&rustls::version::TLS13])
        .with_no_client_auth()
        .with_single_cert(vec![cert.clone()], key)
        .expect("the cert and key were made together");
    tls.alpn_protocols = vec![b"h3".to_vec()];
    let quic_tls = quinn::crypto::rustls::QuicServerConfig::try_from(tls)
        .expect("TLS 1.3 with a ring provider always has the initial suite");
    let mut cfg = quinn::ServerConfig::with_crypto(Arc::new(quic_tls));
    // Room for an origin's 1200-byte QUIC packet behind a quarter stream id
    // and a context id, from the first packet: quinn starts at 1200 and a
    // relay that could not carry a whole packet back would stall the
    // handshake it is relaying.
    let mut transport = quinn::TransportConfig::default();
    transport.initial_mtu(1400);
    cfg.transport_config(Arc::new(transport));
    let endpoint = listen_addrs()
        .into_iter()
        .find_map(|a| quinn::Endpoint::server(cfg.clone(), a).ok())
        .expect("a UDP port on loopback");
    let port = endpoint.local_addr().expect("local_addr").port();
    let counters = Arc::new(Counters::default());
    let c = Arc::clone(&counters);
    tokio::spawn(async move {
        while let Some(incoming) = endpoint.accept().await {
            let c = Arc::clone(&c);
            tokio::spawn(async move {
                let Ok(conn) = incoming.await else { return };
                c.accepted.fetch_add(1, Ordering::SeqCst);
                serve_h3(conn, c).await;
            });
        }
    });
    MasqueProxy {
        port,
        cert,
        counters,
    }
}

type Routes = Arc<Mutex<HashMap<u64, Arc<UdpSocket>>>>;

async fn serve_h3(conn: quinn::Connection, c: Arc<Counters>) {
    let Ok(mut h3) = h3::server::builder()
        .enable_extended_connect(true)
        .enable_datagram(true)
        .build::<_, Bytes>(h3_quinn::Connection::new(conn.clone()))
        .await
    else {
        return;
    };
    let routes: Routes = Arc::default();
    // Client to target: strip the quarter stream id and the context id,
    // and send the rest to the socket that quarter stream id names.
    let (inbound, rc, cc) = (conn.clone(), Arc::clone(&routes), Arc::clone(&c));
    tokio::spawn(async move {
        while let Ok(frame) = inbound.read_datagram().await {
            let Some((quarter, q)) = get_varint(&frame) else {
                continue;
            };
            let Some((context, x)) = get_varint(&frame[q..]) else {
                continue;
            };
            if context != 0 {
                continue;
            }
            let sock = rc.lock().unwrap().get(&quarter).cloned();
            if let Some(sock) = sock {
                cc.datagrams.fetch_add(1, Ordering::SeqCst);
                let _ = sock.send(&frame[q + x..]).await;
            }
        }
    });
    while let Ok(Some(resolver)) = h3.accept().await {
        let (conn, routes, c) = (conn.clone(), Arc::clone(&routes), Arc::clone(&c));
        tokio::spawn(async move {
            let Ok((req, stream)) = resolver.resolve_request().await else {
                return;
            };
            h3_request(req, stream, conn, routes, c).await;
        });
    }
}

type H3Stream = h3::server::RequestStream<h3_quinn::BidiStream<Bytes>, Bytes>;

async fn h3_request(
    req: http::Request<()>,
    mut stream: H3Stream,
    conn: quinn::Connection,
    routes: Routes,
    c: Arc<Counters>,
) {
    let udp = req.method() == http::Method::CONNECT
        && req
            .extensions()
            .get::<h3::ext::Protocol>()
            .is_some_and(|p| p.as_str() == "connect-udp");
    let target = target_of(req.uri().path());
    let sock = match (udp, target) {
        (true, Some((host, port))) => udp_to(&host, port).await,
        _ => None,
    };
    let Some(sock) = sock else {
        if req.method() == http::Method::CONNECT && !udp {
            c.tcp_tunnels.fetch_add(1, Ordering::SeqCst);
        } else {
            c.udp_refusals.fetch_add(1, Ordering::SeqCst);
        }
        let resp = http::Response::builder().status(501).body(()).unwrap();
        let _ = stream.send_response(resp).await;
        let _ = stream.finish().await;
        return;
    };
    let quarter = stream.id().into_inner() / 4;
    routes.lock().unwrap().insert(quarter, Arc::clone(&sock));
    // Target to client.
    let (back, bs, bc) = (conn.clone(), Arc::clone(&sock), Arc::clone(&c));
    let relay = tokio::spawn(async move {
        let mut buf = vec![0u8; 65_535];
        while let Ok(n) = bs.recv(&mut buf).await {
            let mut frame = Vec::with_capacity(n + 9);
            put_varint(&mut frame, quarter);
            frame.push(0);
            frame.extend_from_slice(&buf[..n]);
            bc.datagrams.fetch_add(1, Ordering::SeqCst);
            let _ = back.send_datagram(Bytes::from(frame));
        }
    });
    if stream
        .send_response(capsule_protocol().body(()).unwrap())
        .await
        .is_ok()
    {
        // The tunnel lives as long as the request stream does.
        while let Ok(Some(_)) = stream.recv_data().await {}
    }
    routes.lock().unwrap().remove(&quarter);
    relay.abort();
}

// --- over HTTP/2 --------------------------------------------------------

/// A MASQUE proxy spoken to over HTTP/2: CONNECT-UDP relayed as DATAGRAM
/// capsules on the stream, plain CONNECT spliced to TCP.
pub async fn masque_h2() -> MasqueProxy {
    h2_proxy(true).await
}

/// The same proxy, refusing every CONNECT-UDP with `501`.
pub async fn masque_h2_without_udp() -> MasqueProxy {
    h2_proxy(false).await
}

async fn h2_proxy(udp: bool) -> MasqueProxy {
    let (cert, key) = identity();
    let mut tls = rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(vec![cert.clone()], key)
        .expect("the cert and key were made together");
    tls.alpn_protocols = vec![b"h2".to_vec()];
    let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(tls));
    let mut listener = None;
    for a in listen_addrs() {
        if let Ok(l) = tokio::net::TcpListener::bind(a).await {
            listener = Some(l);
            break;
        }
    }
    let listener = listener.expect("a TCP port on loopback");
    let port = listener.local_addr().expect("local_addr").port();
    let counters = Arc::new(Counters::default());
    let c = Arc::clone(&counters);
    tokio::spawn(async move {
        while let Ok((sock, _)) = listener.accept().await {
            c.accepted.fetch_add(1, Ordering::SeqCst);
            let (acceptor, c) = (acceptor.clone(), Arc::clone(&c));
            tokio::spawn(async move {
                let Ok(tls) = acceptor.accept(sock).await else {
                    return;
                };
                let mut b = h2::server::Builder::new();
                b.enable_connect_protocol();
                let Ok(mut conn) = b.handshake::<_, Bytes>(tls).await else {
                    return;
                };
                while let Some(Ok((req, respond))) = conn.accept().await {
                    tokio::spawn(h2_request(req, respond, udp, Arc::clone(&c)));
                }
            });
        }
    });
    MasqueProxy {
        port,
        cert,
        counters,
    }
}

async fn h2_request(
    req: http::Request<h2::RecvStream>,
    mut respond: h2::server::SendResponse<Bytes>,
    udp: bool,
    c: Arc<Counters>,
) {
    let (parts, body) = req.into_parts();
    let refuse = |mut respond: h2::server::SendResponse<Bytes>| {
        let resp = http::Response::builder().status(501).body(()).unwrap();
        let _ = respond.send_response(resp, true);
    };
    if parts.method != http::Method::CONNECT {
        return refuse(respond);
    }
    match parts.extensions.get::<h2::ext::Protocol>() {
        None => {
            let Some(authority) = parts.uri.authority().map(ToString::to_string) else {
                return refuse(respond);
            };
            let Ok(tcp) = tokio::net::TcpStream::connect(authority).await else {
                return refuse(respond);
            };
            c.tcp_tunnels.fetch_add(1, Ordering::SeqCst);
            let resp = http::Response::builder().status(200).body(()).unwrap();
            let Ok(send) = respond.send_response(resp, false) else {
                return;
            };
            splice_tcp(tcp, body, send).await;
        }
        Some(p) if p.as_str() == "connect-udp" && udp => {
            let sock = match target_of(parts.uri.path()) {
                Some((host, port)) => udp_to(&host, port).await,
                None => None,
            };
            let Some(sock) = sock else {
                return refuse(respond);
            };
            let Ok(send) = respond.send_response(capsule_protocol().body(()).unwrap(), false)
            else {
                return;
            };
            splice_capsules(sock, body, send, c).await;
        }
        Some(_) => {
            c.udp_refusals.fetch_add(1, Ordering::SeqCst);
            refuse(respond);
        }
    }
}

async fn splice_tcp(
    tcp: tokio::net::TcpStream,
    mut body: h2::RecvStream,
    mut send: h2::SendStream<Bytes>,
) {
    let (mut r, mut w) = tcp.into_split();
    let up = tokio::spawn(async move {
        while let Some(Ok(chunk)) = body.data().await {
            let _ = body.flow_control().release_capacity(chunk.len());
            if w.write_all(&chunk).await.is_err() {
                return;
            }
        }
        let _ = w.shutdown().await;
    });
    let mut buf = vec![0u8; 16_384];
    loop {
        match r.read(&mut buf).await {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                if send
                    .send_data(Bytes::copy_from_slice(&buf[..n]), false)
                    .is_err()
                {
                    break;
                }
            }
        }
    }
    let _ = send.send_data(Bytes::new(), true);
    let _ = up.await;
}

async fn splice_capsules(
    sock: Arc<UdpSocket>,
    mut body: h2::RecvStream,
    mut send: h2::SendStream<Bytes>,
    c: Arc<Counters>,
) {
    let (us, uc) = (Arc::clone(&sock), Arc::clone(&c));
    let up = tokio::spawn(async move {
        let mut pending = bytes::BytesMut::new();
        while let Some(Ok(chunk)) = body.data().await {
            let _ = body.flow_control().release_capacity(chunk.len());
            pending.extend_from_slice(&chunk);
            while let Some((kind, k)) = get_varint(&pending) {
                let Some((len, l)) = get_varint(&pending[k..]) else {
                    break;
                };
                let len = usize::try_from(len).expect("a capsule this fixture can hold");
                if pending.len() < k + l + len {
                    break;
                }
                pending.advance(k + l);
                let value = pending.split_to(len);
                if kind != 0 {
                    continue;
                }
                let Some((context, x)) = get_varint(&value) else {
                    continue;
                };
                if context == 0 {
                    uc.capsules.fetch_add(1, Ordering::SeqCst);
                    let _ = us.send(&value[x..]).await;
                }
            }
        }
    });
    let mut buf = vec![0u8; 65_535];
    while let Ok(n) = sock.recv(&mut buf).await {
        let mut capsule = Vec::with_capacity(n + 10);
        put_varint(&mut capsule, 0);
        put_varint(&mut capsule, n as u64 + 1);
        capsule.push(0);
        capsule.extend_from_slice(&buf[..n]);
        c.capsules.fetch_add(1, Ordering::SeqCst);
        if send.send_data(Bytes::from(capsule), false).is_err() {
            break;
        }
        if up.is_finished() {
            break;
        }
    }
    up.abort();
}

// --- the client ---------------------------------------------------------

/// Never an assertion — it turns a request that hangs into a red test.
pub const BOUND: std::time::Duration = std::time::Duration::from_secs(20);

/// `f`, or a panic once [`BOUND`] has passed.
pub async fn bounded<F: Future>(f: F) -> F::Output {
    tokio::time::timeout(BOUND, f)
        .await
        .expect("finished inside the bound")
}

/// A rustls backend trusting exactly these servers.
pub fn trusting(servers: &[&dyn Trusted]) -> hclient_tls_rustls::Rustls {
    let mut roots = rustls::RootCertStore::empty();
    for s in servers {
        roots.add(s.cert().clone()).expect("a DER certificate");
    }
    let cfg = rustls::ClientConfig::builder_with_protocol_versions(&[&rustls::version::TLS13])
        .with_root_certificates(roots)
        .with_no_client_auth();
    hclient_tls_rustls::Rustls::from_config(Arc::new(cfg))
}

/// [`native_through`]'s transport.
pub type Transport = hclient_native::Native<
    hclient_rt_tokio::Tokio,
    hclient_tls_rustls::Rustls,
    hclient_dns_system::SystemDns<hclient_rt_tokio::Tokio>,
>;

/// The transport every test sends through: both stacks, a multiplexed
/// HTTP/2, the system resolver, and `filter` in front of all of it.
///
/// # Panics
///
/// If the two stacks disagree, which they do not.
pub fn native_through(
    filter: hclient_masque::Masque,
    trusting_these: &[&dyn Trusted],
) -> Transport {
    let tls = trusting(trusting_these);
    let dns = hclient_dns_system::SystemDns::new(Tokio);
    let quic =
        hclient_native::H3::new(Tokio, tls.clone(), dns.clone()).expect("H3::new does no I/O");
    hclient_native::Native::new(Tokio, tls, dns)
        .multiplexed()
        .http3(quic)
        .expect("the two stacks agree")
        .egress(filter)
}

/// [`native_through`], behind an `hclient::Client`.
///
/// # Panics
///
/// If the client refuses the transport, which it does not.
pub fn client_through(
    filter: hclient_masque::Masque,
    trusting_these: &[&dyn Trusted],
) -> hclient::Client {
    hclient::Client::builder(native_through(filter, trusting_these))
        .build()
        .expect("the client takes the transport")
}
