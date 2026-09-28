//! A real SOCKS5 proxy on loopback that relays UDP (RFC 1928 §4
//! `CMD=0x03` and the §7 datagram header) and splices TCP (`CMD=0x01`) —
//! so an HTTP/3 request through it is a real QUIC handshake that crossed a
//! relay, and a request that fell back to the proxy's stream has somewhere
//! to go.
//!
//! It resolves nothing: an address in a request is used as given, and a
//! name — whatever it is — is taken to mean loopback, where every test's
//! origin listens. That is the proxy side's resolver, so a request that
//! reached the origin by name was named to the proxy and to nothing else.
#![cfg(not(target_family = "wasm"))]
#![allow(
    dead_code,
    reason = "a fixture module included by `#[path]`; each including test file uses its own subset"
)]

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// How the relay answers a request for a UDP association.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Behaviour {
    /// Associates, reporting the loopback address of its relay socket.
    Relay,
    /// Answers `REP=0x07`, *command not supported*.
    RefuseCommand,
    /// Associates, reporting `0.0.0.0` and the relay socket's port — RFC
    /// 1928's way of saying *the address you reached me at*.
    BindUnspecified,
    /// Answers the greeting and never answers the association request.
    Stall,
}

#[derive(Debug, Default)]
struct Counters {
    associations: AtomicUsize,
    connects: AtomicUsize,
    datagrams_relayed: AtomicUsize,
    control_closed: AtomicUsize,
    /// Association requests read and deliberately left unanswered.
    stalled: AtomicUsize,
    /// While set, datagrams from the client are dropped rather than
    /// relayed.
    dropping: std::sync::atomic::AtomicBool,
    /// The loopback address of the newest association's client-facing
    /// socket, and of the socket it forwards from.
    udp: Mutex<Option<(SocketAddr, SocketAddr)>>,
    /// Every host a datagram header named, as written.
    named: Mutex<Vec<String>>,
}

/// The relay, running on the current tokio runtime until it is dropped
/// with the runtime.
#[derive(Debug)]
pub struct Socks5Udp {
    addr: SocketAddr,
    counters: Arc<Counters>,
    /// Bumped by [`Socks5Udp::close_control`]; every control connection
    /// open at the time ends when it moves.
    close: tokio::sync::watch::Sender<u64>,
}

impl Socks5Udp {
    /// Bind on loopback and start answering.
    ///
    /// # Panics
    ///
    /// Outside a tokio runtime, or if loopback cannot bind.
    pub async fn start(behaviour: Behaviour) -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind the relay's TCP port");
        let addr = listener.local_addr().expect("local_addr");
        let counters = Arc::new(Counters::default());
        let (close, _) = tokio::sync::watch::channel(0u64);
        let (c, closing) = (counters.clone(), close.clone());
        tokio::spawn(async move {
            while let Ok((sock, _)) = listener.accept().await {
                tokio::spawn(serve(sock, behaviour, c.clone(), closing.subscribe()));
            }
        });
        Self {
            addr,
            counters,
            close,
        }
    }

    /// The proxy's TCP address.
    pub fn addr(&self) -> SocketAddr {
        self.addr
    }

    /// The newest association's client-facing UDP socket.
    ///
    /// # Panics
    ///
    /// Before any association.
    pub fn udp_addr(&self) -> SocketAddr {
        self.sockets().0
    }

    /// The socket the newest association forwards from — the only address
    /// an origin behind this relay can see.
    ///
    /// # Panics
    ///
    /// Before any association.
    pub fn outbound_addr(&self) -> SocketAddr {
        self.sockets().1
    }

    fn sockets(&self) -> (SocketAddr, SocketAddr) {
        self.counters
            .udp
            .lock()
            .expect("relay sockets")
            .expect("an association was made")
    }

    /// UDP associations granted or refused — every `CMD=0x03` answered.
    pub fn associations(&self) -> usize {
        self.counters.associations.load(Ordering::SeqCst)
    }

    /// TCP `CONNECT`s spliced.
    pub fn connects(&self) -> usize {
        self.counters.connects.load(Ordering::SeqCst)
    }

    /// Datagrams relayed in either direction.
    pub fn datagrams_relayed(&self) -> usize {
        self.counters.datagrams_relayed.load(Ordering::SeqCst)
    }

    /// Control connections the client closed.
    pub fn control_closed(&self) -> usize {
        self.counters.control_closed.load(Ordering::SeqCst)
    }

    /// Every host a datagram header named, as the client wrote it.
    ///
    /// # Panics
    ///
    /// If the mutex is poisoned.
    pub fn named_hosts(&self) -> Vec<String> {
        self.counters.named.lock().expect("named hosts").clone()
    }

    /// Drop every control connection open now — the proxy ending its
    /// associations.
    pub fn close_control(&self) {
        self.close.send_modify(|g| *g += 1);
    }

    /// Drop every datagram a client sends from now on, without ending
    /// anything — so a request in flight waits rather than fails.
    pub fn stop_relaying(&self) {
        self.counters.dropping.store(true, Ordering::SeqCst);
    }

    /// Wait until an association request has been read and left
    /// unanswered.
    ///
    /// # Panics
    ///
    /// If none arrived within `within`.
    pub async fn wait_stalled(&self, within: std::time::Duration) {
        let deadline = tokio::time::Instant::now() + within;
        while self.counters.stalled.load(Ordering::SeqCst) == 0 {
            assert!(
                tokio::time::Instant::now() < deadline,
                "no association request arrived within {within:?}"
            );
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    }

    /// Wait until the client has closed a control connection.
    ///
    /// # Panics
    ///
    /// If none was closed within `within`.
    pub async fn wait_control_closed(&self, within: std::time::Duration) {
        let deadline = tokio::time::Instant::now() + within;
        while self.control_closed() == 0 {
            assert!(
                tokio::time::Instant::now() < deadline,
                "the client's control connection was still open after {within:?}"
            );
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    }
}

/// `ATYP ADDR PORT` at the start of `b`: the host as written and the
/// port, and how many bytes they took.
fn parse_addr(b: &[u8]) -> Option<(String, u16, usize)> {
    let (host, at) = match *b.first()? {
        0x01 => (
            IpAddr::from(<[u8; 4]>::try_from(b.get(1..5)?).ok()?).to_string(),
            5,
        ),
        0x04 => (
            IpAddr::from(<[u8; 16]>::try_from(b.get(1..17)?).ok()?).to_string(),
            17,
        ),
        0x03 => {
            let n = usize::from(*b.get(1)?);
            (String::from_utf8(b.get(2..2 + n)?.to_vec()).ok()?, 2 + n)
        }
        _ => return None,
    };
    let port = u16::from_be_bytes(b.get(at..at + 2)?.try_into().ok()?);
    Some((host, port, at + 2))
}

/// Where `host` is, to this relay: a literal as itself, and any name as
/// loopback.
fn resolve(host: &str, port: u16) -> SocketAddr {
    let ip = host
        .trim_start_matches('[')
        .trim_end_matches(']')
        .parse()
        .unwrap_or(IpAddr::V4(Ipv4Addr::LOCALHOST));
    SocketAddr::new(ip, port)
}

/// `05 REP 00 ATYP ADDR PORT` for `a`.
fn reply(rep: u8, a: SocketAddr) -> Vec<u8> {
    let mut r = vec![0x05, rep, 0x00];
    match a.ip() {
        IpAddr::V4(v) => {
            r.push(0x01);
            r.extend_from_slice(&v.octets());
        }
        IpAddr::V6(v) => {
            r.push(0x04);
            r.extend_from_slice(&v.octets());
        }
    }
    r.extend_from_slice(&a.port().to_be_bytes());
    r
}

/// Read one SOCKS5 request after the greeting: `CMD` and the address.
async fn read_request(sock: &mut tokio::net::TcpStream) -> Option<(u8, String, u16)> {
    let mut head = [0u8; 4];
    sock.read_exact(&mut head).await.ok()?;
    let rest_len = match head[3] {
        0x01 => 4 + 2,
        0x04 => 16 + 2,
        0x03 => {
            let mut n = [0u8; 1];
            sock.read_exact(&mut n).await.ok()?;
            let mut rest = vec![0u8; usize::from(n[0]) + 2];
            sock.read_exact(&mut rest).await.ok()?;
            let mut all = vec![0x03, n[0]];
            all.extend_from_slice(&rest);
            let (h, p, _) = parse_addr(&all)?;
            return Some((head[1], h, p));
        }
        _ => return None,
    };
    let mut rest = vec![0u8; rest_len];
    sock.read_exact(&mut rest).await.ok()?;
    let mut all = vec![head[3]];
    all.extend_from_slice(&rest);
    let (h, p, _) = parse_addr(&all)?;
    Some((head[1], h, p))
}

async fn serve(
    mut sock: tokio::net::TcpStream,
    behaviour: Behaviour,
    counters: Arc<Counters>,
    mut close: tokio::sync::watch::Receiver<u64>,
) {
    // The greeting: `05 NMETHODS METHODS`, answered with no authentication.
    let mut g = [0u8; 2];
    if sock.read_exact(&mut g).await.is_err() || g[0] != 0x05 {
        return;
    }
    let mut methods = vec![0u8; usize::from(g[1])];
    if sock.read_exact(&mut methods).await.is_err() || sock.write_all(&[0x05, 0x00]).await.is_err()
    {
        return;
    }
    let Some((cmd, host, port)) = read_request(&mut sock).await else {
        return;
    };
    match cmd {
        0x01 => connect(sock, &host, port, &counters).await,
        0x03 => associate(sock, behaviour, counters, &mut close).await,
        _ => {
            let _ = sock
                .write_all(&reply(0x07, SocketAddr::from(([0, 0, 0, 0], 0))))
                .await;
        }
    }
}

/// `CMD=0x01`: open the target and splice.
async fn connect(mut sock: tokio::net::TcpStream, host: &str, port: u16, counters: &Counters) {
    let Ok(mut out) = tokio::net::TcpStream::connect(resolve(host, port)).await else {
        let _ = sock
            .write_all(&reply(0x05, SocketAddr::from(([0, 0, 0, 0], 0))))
            .await;
        return;
    };
    counters.connects.fetch_add(1, Ordering::SeqCst);
    let local = out.local_addr().expect("local_addr");
    if sock.write_all(&reply(0x00, local)).await.is_err() {
        return;
    }
    let _ = tokio::io::copy_bidirectional(&mut sock, &mut out).await;
}

/// `CMD=0x03`: answer as `behaviour` says, then relay until the control
/// connection ends from either side.
async fn associate(
    mut sock: tokio::net::TcpStream,
    behaviour: Behaviour,
    counters: Arc<Counters>,
    close: &mut tokio::sync::watch::Receiver<u64>,
) {
    let generation = *close.borrow_and_update();
    let closed_by_us = async {
        while close.changed().await.is_ok() {
            if *close.borrow() != generation {
                return;
            }
        }
        std::future::pending::<()>().await;
    };
    let relay = match behaviour {
        Behaviour::Stall => {
            counters.stalled.fetch_add(1, Ordering::SeqCst);
            None
        }
        Behaviour::RefuseCommand => {
            counters.associations.fetch_add(1, Ordering::SeqCst);
            let _ = sock
                .write_all(&reply(0x07, SocketAddr::from(([0, 0, 0, 0], 0))))
                .await;
            return;
        }
        Behaviour::Relay | Behaviour::BindUnspecified => {
            let inbound = tokio::net::UdpSocket::bind("127.0.0.1:0")
                .await
                .expect("bind the relay's client-facing UDP socket");
            let outbound = tokio::net::UdpSocket::bind("127.0.0.1:0")
                .await
                .expect("bind the relay's outbound UDP socket");
            let (a, b) = (
                inbound.local_addr().expect("local_addr"),
                outbound.local_addr().expect("local_addr"),
            );
            *counters.udp.lock().expect("relay sockets") = Some((a, b));
            counters.associations.fetch_add(1, Ordering::SeqCst);
            let told = if behaviour == Behaviour::BindUnspecified {
                SocketAddr::from(([0, 0, 0, 0], a.port()))
            } else {
                a
            };
            if sock.write_all(&reply(0x00, told)).await.is_err() {
                return;
            }
            Some(relay(inbound, outbound, counters.clone()))
        }
    };
    // The association lives exactly as long as its control connection.
    let control = async {
        let mut byte = [0u8; 1];
        loop {
            match sock.read(&mut byte).await {
                Ok(0) | Err(_) => {
                    counters.control_closed.fetch_add(1, Ordering::SeqCst);
                    return;
                }
                Ok(_) => {}
            }
        }
    };
    let relaying = async {
        match relay {
            Some(r) => r.await,
            None => std::future::pending().await,
        }
    };
    tokio::select! {
        () = control => {}
        () = closed_by_us => {}
        () = relaying => {}
    }
}

/// Relay datagrams: a client's §7 header names the target, and the payload
/// goes out bare from `outbound`; what comes back is wrapped in an
/// `ATYP=1`/`ATYP=4` header naming its source and sent to the client's
/// latest address from `inbound`.
async fn relay(
    inbound: tokio::net::UdpSocket,
    outbound: tokio::net::UdpSocket,
    counters: Arc<Counters>,
) {
    let mut client: Option<SocketAddr> = None;
    let (mut up, mut down) = (vec![0u8; 65536], vec![0u8; 65536]);
    loop {
        tokio::select! {
            got = inbound.recv_from(&mut up) => {
                let Ok((n, from)) = got else { return };
                let d = &up[..n];
                // RSV RSV FRAG, then the address; a fragment is dropped.
                if n < 4 || d[2] != 0 || counters.dropping.load(Ordering::SeqCst) {
                    continue;
                }
                let Some((host, port, used)) = parse_addr(&d[3..]) else { continue };
                client = Some(from);
                counters.named.lock().expect("named hosts").push(host.clone());
                if outbound.send_to(&d[3 + used..], resolve(&host, port)).await.is_ok() {
                    counters.datagrams_relayed.fetch_add(1, Ordering::SeqCst);
                }
            }
            got = outbound.recv_from(&mut down) => {
                let Ok((n, from)) = got else { return };
                let Some(to) = client else { continue };
                let mut d = reply(0x00, from);
                // `05 00 00` is a reply's `VER REP RSV`; a datagram's is
                // `RSV RSV FRAG`, all zero.
                d[0] = 0;
                d.extend_from_slice(&down[..n]);
                if inbound.send_to(&d, to).await.is_ok() {
                    counters.datagrams_relayed.fetch_add(1, Ordering::SeqCst);
                }
            }
        }
    }
}
