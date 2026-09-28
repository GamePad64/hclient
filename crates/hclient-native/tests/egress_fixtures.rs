//! Shared fixtures for the egress-datagram tests: a filter's datagram path
//! as a real test would meet one, without a real relay in the way.
#![cfg(all(feature = "http3", not(target_family = "wasm")))]
#![allow(
    dead_code,
    reason = "a fixture module included by `#[path]`; each including test file uses its own subset"
)]

use hclient_proxy::{BoxPath, DatagramPath};
use std::io;
use std::net::SocketAddr;
use std::task::{Context, Poll};

/// What a UDP relay's path to one peer carries: a common Ethernet MTU less
/// an IPv4 and a UDP header and a relay's own 20 bytes of framing.
pub const BRIDGE_MAX: usize = 1452;

/// A [`DatagramPath`] over a plain UDP socket aimed at one peer.
///
/// The simplest honest relay: every datagram the QUIC stack hands the
/// path leaves this host on a socket of its own, addressed to `peer`,
/// and every datagram `peer` sends back is handed up whole. What it
/// proves is that a transport addressed nothing itself — the only
/// address anything was sent to is the one this fixture was built with.
#[derive(Debug)]
pub struct UdpBridge {
    sock: tokio::net::UdpSocket,
    max: usize,
}

impl DatagramPath for UdpBridge {
    fn try_send(&self, datagram: &[u8]) -> io::Result<()> {
        if datagram.len() > self.max {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("{} bytes over a {}-byte path", datagram.len(), self.max),
            ));
        }
        self.sock.try_send(datagram).map(|_| ())
    }

    fn poll_writable(&self, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        self.sock.poll_send_ready(cx)
    }

    fn poll_recv(&self, cx: &mut Context<'_>, buf: &mut [u8]) -> Poll<io::Result<usize>> {
        let mut rb = tokio::io::ReadBuf::new(buf);
        std::task::ready!(self.sock.poll_recv(cx, &mut rb))?;
        Poll::Ready(Ok(rb.filled().len()))
    }

    fn max_datagram_size(&self) -> usize {
        self.max
    }
}

/// A path to `peer` over a fresh loopback UDP socket.
///
/// # Panics
///
/// Outside a tokio runtime, or if loopback cannot bind a socket.
pub fn udp_bridge(peer: SocketAddr) -> BoxPath {
    udp_bridge_of(peer, BRIDGE_MAX)
}

/// [`udp_bridge`] claiming `max` bytes per datagram.
///
/// # Panics
///
/// As [`udp_bridge`].
pub fn udp_bridge_of(peer: SocketAddr, max: usize) -> BoxPath {
    let std = std::net::UdpSocket::bind("127.0.0.1:0").expect("bind a loopback UDP socket");
    std.connect(peer).expect("connect the bridge to its peer");
    std.set_nonblocking(true).expect("non-blocking");
    let sock = tokio::net::UdpSocket::from_std(std).expect("inside a tokio runtime");
    BoxPath::new(UdpBridge { sock, max })
}

/// How a [`ForwardFilter`] answers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Declares datagrams and no stream; opens a path to its peer.
    DatagramsOnly,
    /// Declares both, refuses every datagram path as unsupported, and
    /// carries a stream to its peer — the switch a transport may make.
    RefusingDatagramsWithStream,
    /// Declares a stream and no datagrams.
    StreamOnly,
    /// Declares both, and its proxy cannot be reached for datagrams —
    /// final, never a switch.
    Failing,
    /// Declares both, and opens a path too small for QUIC (1199 bytes).
    SmallPathWithStream,
    /// Declares datagrams only, takes this long to open a path, and the
    /// path it opens goes to its peer — which a test makes a bound, silent
    /// socket, so the QUIC handshake over it is never answered.
    Slow(std::time::Duration),
    /// Declares both, and carries each to its peer.
    Both,
    /// Declares both; takes `.0` to open a path too small for QUIC, and
    /// `.1` to open each stream to its peer — so a switch to the stream
    /// after QUIC over the path failed spends a measurable share of the
    /// caller's connect bound on each side.
    SlowSmallPathSlowStream(std::time::Duration, std::time::Duration),
}

/// Every request is filtered under the key `"fwd"`, and whatever the filter
/// opens goes to one fixed `peer` — so a request that arrives there came
/// through this filter, and the origin's own name was never needed to get
/// it there.
///
/// The in-tree twin of the outside witness's `Forward`, with modes for the
/// ways a filter can answer a request for datagrams, and a count of how
/// often it was asked.
#[derive(Debug, Clone)]
pub struct ForwardFilter {
    mode: Mode,
    peer: SocketAddr,
    datagram_attempts: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    stream_opens: std::sync::Arc<std::sync::atomic::AtomicUsize>,
}

impl ForwardFilter {
    /// A filter in `mode` forwarding to `peer`.
    pub fn new(mode: Mode, peer: SocketAddr) -> Self {
        Self {
            mode,
            peer,
            datagram_attempts: std::sync::Arc::default(),
            stream_opens: std::sync::Arc::default(),
        }
    }

    /// How often a datagram path was asked for.
    pub fn datagram_attempts(&self) -> usize {
        self.datagram_attempts
            .load(std::sync::atomic::Ordering::SeqCst)
    }

    /// How often a stream was asked for.
    pub fn stream_opens(&self) -> usize {
        self.stream_opens.load(std::sync::atomic::Ordering::SeqCst)
    }

    fn support(&self) -> hclient_proxy::FilterSupport {
        use hclient_proxy::FilterSupport;
        match self.mode {
            Mode::DatagramsOnly | Mode::Slow(_) => FilterSupport::NONE.with_datagrams(),
            Mode::StreamOnly => FilterSupport::STREAM,
            Mode::RefusingDatagramsWithStream
            | Mode::Failing
            | Mode::SmallPathWithStream
            | Mode::SlowSmallPathSlowStream(..)
            | Mode::Both => FilterSupport::STREAM.with_datagrams(),
        }
    }
}

fn connect_error(msg: &'static str) -> hclient_core::error::Error {
    hclient_core::error::Error::new(
        hclient_core::error::ErrorKind::Connect,
        io::Error::other(msg),
    )
}

impl hclient_proxy::EgressFilter for ForwardFilter {
    type Wrapped<S: hclient_proxy::Io> = S;

    fn route(&self, _: &hclient_proxy::Target<'_>) -> hclient_proxy::Decision<'_> {
        hclient_proxy::Decision::Filtered(hclient_proxy::Route::new(
            self.support(),
            "fwd",
            hclient_proxy::RequestForm::Origin,
        ))
    }

    async fn open_stream<'a, C: hclient_proxy::Dial + 'a>(
        &'a self,
        _: hclient_proxy::Target<'a>,
        ctx: &'a C,
    ) -> Result<hclient_proxy::Opened<C::Stream, C::Stream>, hclient_proxy::Attempt>
    where
        Self: Sized,
    {
        self.stream_opens
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        if let Mode::SlowSmallPathSlowStream(_, delay) = self.mode {
            tokio::time::sleep(delay).await;
        }
        // By address: the peer is a literal, so nothing here names the
        // origin to a resolver either.
        let stream = ctx
            .connect(&self.peer.ip().to_string(), self.peer.port())
            .await
            .map_err(hclient_proxy::Attempt::Failed)?;
        Ok(hclient_proxy::Opened::Raw(stream))
    }

    fn open_datagrams<'a, C: hclient_proxy::Dial + 'a>(
        &'a self,
        _: hclient_proxy::Target<'a>,
        _: &'a C,
    ) -> impl std::future::Future<Output = Result<BoxPath, hclient_proxy::Attempt>> + 'a
    where
        Self: Sized,
        C::Stream: Send + 'static, // send-bound-exception: amendment-C16
    {
        self.datagram_attempts
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let (mode, peer) = (self.mode, self.peer);
        async move {
            match mode {
                Mode::DatagramsOnly | Mode::Both => Ok(udp_bridge(peer)),
                Mode::Slow(delay) => {
                    tokio::time::sleep(delay).await;
                    Ok(udp_bridge(peer))
                }
                Mode::SmallPathWithStream => Ok(udp_bridge_of(peer, 1199)),
                Mode::SlowSmallPathSlowStream(delay, _) => {
                    tokio::time::sleep(delay).await;
                    Ok(udp_bridge_of(peer, 1199))
                }
                Mode::RefusingDatagramsWithStream | Mode::StreamOnly => Err(
                    hclient_proxy::Attempt::Unsupported(hclient_core::error::Error::new(
                        hclient_core::error::ErrorKind::Unsupported,
                        io::Error::other("this proxy relays no datagrams"),
                    )),
                ),
                Mode::Failing => Err(hclient_proxy::Attempt::Failed(connect_error(
                    "the proxy could not be reached",
                ))),
            }
        }
    }
}

impl hclient_proxy::SendEgressFilter for ForwardFilter {
    fn open_stream_send<'a>(
        &'a self,
        t: hclient_proxy::Target<'a>,
        ctx: &'a hclient_proxy::BoxDial<'a>,
    ) -> hclient_proxy::BoxOpening<'a> {
        Box::pin(async move {
            hclient_proxy::EgressFilter::open_stream(self, t, ctx)
                .await
                .map(hclient_proxy::erase)
        })
    }

    fn open_datagrams_send<'a>(
        &'a self,
        t: hclient_proxy::Target<'a>,
        ctx: &'a hclient_proxy::BoxDial<'a>,
    ) -> hclient_proxy::BoxPathOpening<'a> {
        Box::pin(hclient_proxy::EgressFilter::open_datagrams(self, t, ctx))
    }
}

/// A resolver that answers every address question with loopback and
/// writes down every name it was asked about, whatever the type — so
/// *"the origin was never resolved"* is read off a log rather than
/// inferred.
#[derive(Debug, Clone, Default)]
pub struct NameLog(std::sync::Arc<std::sync::Mutex<Vec<String>>>);

impl NameLog {
    /// Every name asked about, in order.
    ///
    /// # Panics
    ///
    /// If the log's mutex is poisoned.
    pub fn names(&self) -> Vec<String> {
        self.0.lock().expect("name log").clone()
    }
}

impl hclient_dns::Resolve for NameLog {
    type Records<'a>
        = std::pin::Pin<
        Box<
            dyn futures_core::Stream<Item = Result<hclient_dns::Record, hclient_core::error::Error>>
                + Send
                + 'a,
        >,
    >
    where
        Self: 'a;

    fn supports(&self, rtype: u16) -> bool {
        matches!(
            rtype,
            hclient_dns::rtype::A | hclient_dns::rtype::AAAA | hclient_dns::rtype::HTTPS
        )
    }

    fn lookup<'a>(&'a self, name: &str, rtype: u16) -> Self::Records<'a> {
        self.0.lock().expect("name log").push(name.to_owned());
        match rtype {
            hclient_dns::rtype::A => Box::pin(futures_util::stream::iter(vec![Ok(
                hclient_dns::Record::new(hclient_dns::RData::from(std::net::IpAddr::from([
                    127, 0, 0, 1,
                ]))),
            )])),
            _ => Box::pin(futures_util::stream::empty()),
        }
    }
}
