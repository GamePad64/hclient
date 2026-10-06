//! Shared fixtures for the egress-datagram tests: a filter's datagram path
//! as a real test would meet one, without a real relay in the way.
#![cfg(all(feature = "http3", not(target_family = "wasm")))]
#![allow(
    dead_code,
    reason = "a fixture module included by `#[path]`; each including test file uses its own subset"
)]

use hclient_proxy::{datagram::BoxPath, datagram::DatagramPath};
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
    /// Where set, a datagram over this many bytes is accepted and never
    /// sent — a link narrower than the path claims, which is what a
    /// SOCKS relay behind a VPN or `PPPoE` looks like to the stack above.
    drops_over: Option<usize>,
    /// How many datagrams were dropped for being over `drops_over`.
    dropped: std::sync::Arc<std::sync::atomic::AtomicUsize>,
}

impl DatagramPath for UdpBridge {
    fn try_send(&self, datagram: &[u8]) -> io::Result<()> {
        if datagram.len() > self.max {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("{} bytes over a {}-byte path", datagram.len(), self.max),
            ));
        }
        if self.drops_over.is_some_and(|n| datagram.len() > n) {
            self.dropped
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            return Ok(());
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
    bridge(peer, max, None, std::sync::Arc::default())
}

/// [`udp_bridge`] that claims [`BRIDGE_MAX`] and silently drops anything
/// over `drops_over` bytes, with a count of what it dropped.
///
/// # Panics
///
/// As [`udp_bridge`].
pub fn udp_bridge_narrower_than_it_claims(
    peer: SocketAddr,
    drops_over: usize,
) -> (BoxPath, std::sync::Arc<std::sync::atomic::AtomicUsize>) {
    let dropped = std::sync::Arc::default();
    (
        bridge(
            peer,
            BRIDGE_MAX,
            Some(drops_over),
            std::sync::Arc::clone(&dropped),
        ),
        dropped,
    )
}

fn bridge(
    peer: SocketAddr,
    max: usize,
    drops_over: Option<usize>,
    dropped: std::sync::Arc<std::sync::atomic::AtomicUsize>,
) -> BoxPath {
    let std = std::net::UdpSocket::bind("127.0.0.1:0").expect("bind a loopback UDP socket");
    std.connect(peer).expect("connect the bridge to its peer");
    std.set_nonblocking(true).expect("non-blocking");
    let sock = tokio::net::UdpSocket::from_std(std).expect("inside a tokio runtime");
    BoxPath::new(UdpBridge {
        sock,
        max,
        drops_over,
        dropped,
    })
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
    lent: std::sync::Arc<std::sync::Mutex<Vec<Lent>>>,
}

/// What a lent context said was left of the connect bound, and when the
/// filter asked it.
#[derive(Debug, Clone, Copy)]
pub struct Lent {
    /// `true` for a datagram path, `false` for a stream.
    pub datagrams: bool,
    /// When the filter was asked, on the test's clock.
    pub at: std::time::Instant,
    /// [`hclient_proxy::egress::Dial::remaining`] at that moment.
    pub remaining: Option<std::time::Duration>,
}

impl ForwardFilter {
    /// A filter in `mode` forwarding to `peer`.
    pub fn new(mode: Mode, peer: SocketAddr) -> Self {
        Self {
            mode,
            peer,
            datagram_attempts: std::sync::Arc::default(),
            stream_opens: std::sync::Arc::default(),
            lent: std::sync::Arc::default(),
        }
    }

    /// Every context this filter was lent, in the order it was asked.
    pub fn lent(&self) -> Vec<Lent> {
        self.lent
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    fn note_lent<C: hclient_proxy::egress::Dial>(&self, datagrams: bool, ctx: &C) {
        let remaining = ctx.remaining();
        self.lent
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(Lent {
                datagrams,
                at: std::time::Instant::now(),
                remaining,
            });
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

    fn support(&self) -> hclient_proxy::egress::FilterSupport {
        use hclient_proxy::egress::FilterSupport;
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

impl hclient_proxy::egress::EgressFilter for ForwardFilter {
    type Wrapped<S: hclient_proxy::egress::Io> = S;

    fn route(&self, _: &hclient_proxy::egress::Target<'_>) -> hclient_proxy::egress::Decision<'_> {
        hclient_proxy::egress::Decision::Filtered(hclient_proxy::egress::Route::new(
            self.support(),
            "fwd",
            hclient_proxy::egress::RequestForm::Origin,
        ))
    }

    async fn open_stream<'a, C: hclient_proxy::egress::Dial + 'a>(
        &'a self,
        _: hclient_proxy::egress::Target<'a>,
        ctx: &'a C,
    ) -> Result<hclient_proxy::egress::Opened<C::Stream, C::Stream>, hclient_proxy::egress::Attempt>
    where
        Self: Sized,
    {
        self.stream_opens
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        self.note_lent(false, ctx);
        if let Mode::SlowSmallPathSlowStream(_, delay) = self.mode {
            tokio::time::sleep(delay).await;
        }
        // By address: the peer is a literal, so nothing here names the
        // origin to a resolver either.
        let stream = ctx
            .connect(&self.peer.ip().to_string(), self.peer.port())
            .await
            .map_err(hclient_proxy::egress::Attempt::Failed)?;
        Ok(hclient_proxy::egress::Opened::Raw(stream))
    }

    fn open_datagrams<'a, C: hclient_proxy::egress::Dial + 'a>(
        &'a self,
        _: hclient_proxy::egress::Target<'a>,
        ctx: &'a C,
    ) -> impl std::future::Future<Output = Result<BoxPath, hclient_proxy::egress::Attempt>> + 'a
    where
        Self: Sized,
        C::Stream: Send + 'static, // send-bound-exception: amendment-C16
    {
        self.datagram_attempts
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        self.note_lent(true, ctx);
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
                    hclient_proxy::egress::Attempt::Unsupported(hclient_core::error::Error::new(
                        hclient_core::error::ErrorKind::Unsupported,
                        io::Error::other("this proxy relays no datagrams"),
                    )),
                ),
                Mode::Failing => Err(hclient_proxy::egress::Attempt::Failed(connect_error(
                    "the proxy could not be reached",
                ))),
            }
        }
    }
}

impl hclient_proxy::egress::SendEgressFilter for ForwardFilter {
    fn open_stream_send<'a>(
        &'a self,
        t: hclient_proxy::egress::Target<'a>,
        ctx: &'a hclient_proxy::egress::BoxDial<'a>,
    ) -> hclient_proxy::egress::BoxOpening<'a> {
        Box::pin(async move {
            hclient_proxy::egress::EgressFilter::open_stream(self, t, ctx)
                .await
                .map(hclient_proxy::egress::erase)
        })
    }

    fn open_datagrams_send<'a>(
        &'a self,
        t: hclient_proxy::egress::Target<'a>,
        ctx: &'a hclient_proxy::egress::BoxDial<'a>,
    ) -> hclient_proxy::egress::BoxPathOpening<'a> {
        Box::pin(hclient_proxy::egress::EgressFilter::open_datagrams(
            self, t, ctx,
        ))
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
