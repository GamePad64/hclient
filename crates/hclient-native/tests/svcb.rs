//! Tier 2 discovery (v0.3 W2), watched from the far end of the socket.
//!
//! Every claim in this file is made by a **peer**, not by the client: a
//! plain `TcpListener` that records the first flight it is sent and then
//! closes. A record's port is "used" when the connection arrives at that
//! port; a hint "reaches Happy Eyeballs" when a connection arrives at the
//! hinted address with nothing else to derive it from; an ALPN offer is
//! what the ClientHello on the wire actually lists. Nothing here reads a
//! field of `Native`, and nothing asserts on an error message where a
//! socket can answer instead.
//!
//! # Why the peer never completes a handshake
//!
//! Everything asserted here happens in the client's **first flight**, so a
//! peer that answered would only add ways for the test to fail for
//! unrelated reasons (a certificate this test never provisioned, for
//! one). It accepts, reads the ClientHello, and drops the socket; the
//! request therefore always ends in an error, and the error is never the
//! observation. The same construction as
//! `hclient-tls-rustls/tests/ech.rs`, which is where the technique — and
//! the ECH claim these tests extend — comes from.
//!
//! # Why the URI never names a port, and what that costs the file
//!
//! Discovery applies at the scheme's default port only (RFC 9460 §9.5 —
//! see `hclient_native`'s `discovery` module doc), so every URI below is
//! `https://svcb.example/` and the *origin's own* endpoint is
//! `127.0.0.1:443`. An unprivileged test process cannot put a listener
//! there, so "the connection fell back to the origin" is visible only as
//! a refusal, never as an arrival. One property is out of reach because of
//! it — the in-request retry after a discovered endpoint fails — and it is
//! covered where it can be observed instead (`src/connect.rs`'s
//! `a_failed_discovered_endpoint_is_retried_without_the_record`, on the
//! attempt log of a fake runtime).

use bytes::Bytes;
use hclient_core::RequestBody;
use hclient_core::unversioned::Transport;
use hclient_dns::{Resolve, ResolvedAddr, SvcbEndpoint};
use hclient_native::{Native, SVCB_FAILURE_TTL};
use hclient_rt::{TcpConnect, TcpOpts, TcpOptsSupport, Timer};
use hclient_rt_tokio::Tokio;
use hclient_tls_rustls::Rustls;
use std::io::Read;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// The origin every test asks for. Long and unmistakable, so that finding
/// it inside a ClientHello is not a coincidence — see
/// [`the_name_a_record_asked_to_protect_goes_out_in_the_clear`].
const ORIGIN: &str = "svcb-origin-that-must-be-visible.example";

/// How long the peer waits for a first flight that should already be on
/// its way. Only bounds the failure case: a ClientHello on loopback is
/// written in microseconds, and the read loop below stops as soon as the
/// record is complete rather than waiting this out.
const READ_WINDOW: Duration = Duration::from_millis(500);

/// Bounds every request, so that a mutation which turns a failure into a
/// hang is red rather than eternal — the shape `tests/connect.rs` already
/// insists on for the same reason.
const BOUND: Duration = Duration::from_secs(10);

// --- the observer -------------------------------------------------------

/// A peer that answers nothing and reports every first flight it was sent.
struct Peer {
    addr: SocketAddr,
    seen: Arc<Mutex<Vec<Vec<u8>>>>,
}

impl Peer {
    fn start() -> Self {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = listener.local_addr().expect("local_addr");
        let seen = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&seen);
        std::thread::spawn(move || {
            for sock in listener.incoming() {
                let Ok(mut sock) = sock else { break };
                sock.set_read_timeout(Some(READ_WINDOW)).expect("timeout");
                let mut flight = Vec::new();
                let mut chunk = [0u8; 4096];
                while let Ok(n) = sock.read(&mut chunk) {
                    if n == 0 {
                        break;
                    }
                    flight.extend_from_slice(&chunk[..n]);
                    if tls_record_complete(&flight) {
                        break;
                    }
                }
                if !flight.is_empty() {
                    // Recorded BEFORE the socket is dropped, so a client
                    // that has already seen EOF cannot be ahead of this
                    // thread: the request the test awaits cannot return
                    // until the drop below, which happens after the push.
                    sink.lock().expect("peer sink").push(flight);
                }
            }
        });
        Self { addr, seen }
    }

    fn port(&self) -> u16 {
        self.addr.port()
    }

    /// The first flights this peer has received, in arrival order.
    fn flights(&self) -> Vec<Vec<u8>> {
        self.seen.lock().expect("peer sink").clone()
    }

    fn count(&self) -> usize {
        self.seen.lock().expect("peer sink").len()
    }
}

/// Whether `buf` holds a complete TLS record (5-byte header plus its
/// declared length). A ClientHello is one record, so this is what lets the
/// peer answer immediately instead of waiting out [`READ_WINDOW`].
fn tls_record_complete(buf: &[u8]) -> bool {
    if buf.len() < 5 {
        return false;
    }
    let declared = u16::from_be_bytes([buf[3], buf[4]]) as usize;
    buf.len() >= declared + 5
}

// --- reading the ClientHello -------------------------------------------

/// The ALPN protocol list a ClientHello offers, or `None` if there is no
/// ALPN extension in it.
///
/// A parser rather than a substring search, and the difference is not
/// pedantry: `\x02h2` occurs in a 32-byte random field roughly once in
/// half a million ClientHellos, which is exactly the kind of test that
/// fails once a year and is dismissed as flaky. This walks the structure
/// (RFC 8446 §4.1.2 for the message, RFC 7301 §3.1 for the extension) and
/// returns `None` for anything it cannot read, so a malformed flight is a
/// failed assertion rather than a false one.
#[cfg(feature = "http2")]
fn alpn_offer(flight: &[u8]) -> Option<Vec<Vec<u8>>> {
    let mut p = Cursor::new(flight);
    p.skip(5)?; // TLS record header
    if p.u8()? != 0x01 {
        return None; // not a ClientHello
    }
    p.skip(3)?; // handshake length
    p.skip(2)?; // legacy_version
    p.skip(32)?; // random
    let session = p.u8()? as usize;
    p.skip(session)?;
    let suites = p.u16()? as usize;
    p.skip(suites)?;
    let compression = p.u8()? as usize;
    p.skip(compression)?;
    let ext_total = p.u16()? as usize;
    let end = p.at + ext_total;
    while p.at < end {
        let kind = p.u16()?;
        let len = p.u16()? as usize;
        if kind != 0x0010 {
            p.skip(len)?;
            continue;
        }
        let list_len = p.u16()? as usize;
        let list_end = p.at + list_len;
        let mut out = Vec::new();
        while p.at < list_end {
            let n = p.u8()? as usize;
            out.push(p.take(n)?.to_vec());
        }
        return Some(out);
    }
    None
}

#[cfg(feature = "http2")]
struct Cursor<'a> {
    buf: &'a [u8],
    at: usize,
}

#[cfg(feature = "http2")]
impl<'a> Cursor<'a> {
    fn new(buf: &'a [u8]) -> Self {
        Self { buf, at: 0 }
    }
    fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        let out = self.buf.get(self.at..self.at + n)?;
        self.at += n;
        Some(out)
    }
    fn skip(&mut self, n: usize) -> Option<()> {
        self.take(n).map(|_| ())
    }
    fn u8(&mut self) -> Option<u8> {
        self.take(1).map(|b| b[0])
    }
    fn u16(&mut self) -> Option<u16> {
        self.take(2).map(|b| u16::from_be_bytes([b[0], b[1]]))
    }
}

// --- the resolver fixture ----------------------------------------------

/// A resolver that answers exactly what a test tells it to.
///
/// `supports_svcb` is a field rather than a constant, because one of the
/// claims below is about the capability itself: a resolver that says it
/// cannot ask must not be asked, however many records it would have
/// returned (`Resolve::supports_svcb`'s own doc draws that line, and this
/// is the consumer side of it).
#[derive(Clone, Default)]
struct FakeDns {
    v4: Vec<Ipv4Addr>,
    supports_svcb: bool,
    records: Vec<SvcbEndpoint>,
}

impl FakeDns {
    /// The origin's own addresses: loopback, which means the fallback
    /// endpoint is `127.0.0.1:443` — refused, and deliberately so (see the
    /// module doc).
    fn with_loopback() -> Self {
        Self {
            v4: vec![Ipv4Addr::LOCALHOST],
            ..Self::default()
        }
    }

    fn serving(mut self, record: SvcbEndpoint) -> Self {
        self.supports_svcb = true;
        self.records.push(record);
        self
    }

    /// The same records, from a resolver that reports it cannot do SVCB at
    /// all — the control for every test that acts on one.
    fn but_cannot_ask(mut self) -> Self {
        self.supports_svcb = false;
        self
    }
}

impl Resolve for FakeDns {
    type Ipv4<'a>
        = std::pin::Pin<
        Box<dyn futures_core::Stream<Item = Result<ResolvedAddr, hclient_core::Error>> + Send + 'a>,
    >
    where
        Self: 'a;

    fn lookup_ipv4<'a>(&'a self, _name: &str) -> Self::Ipv4<'a> {
        Box::pin({
            futures_util::stream::iter(
                self.v4
                    .clone()
                    .into_iter()
                    .map(|a| {
                        Ok(ResolvedAddr {
                            addr: IpAddr::V4(a),
                            ttl: None,
                        })
                    })
                    .collect::<Vec<_>>(),
            )
        })
    }

    type Ipv6<'a>
        = std::pin::Pin<
        Box<dyn futures_core::Stream<Item = Result<ResolvedAddr, hclient_core::Error>> + Send + 'a>,
    >
    where
        Self: 'a;

    fn lookup_ipv6<'a>(&'a self, _name: &str) -> Self::Ipv6<'a> {
        Box::pin(futures_util::stream::iter(Vec::new()))
    }

    fn supports_svcb(&self) -> bool {
        self.supports_svcb
    }

    type Svcb<'a>
        = std::pin::Pin<
        Box<dyn futures_core::Stream<Item = Result<SvcbEndpoint, hclient_core::Error>> + Send + 'a>,
    >
    where
        Self: 'a;

    fn lookup_svcb<'a>(&'a self, _name: &str) -> Self::Svcb<'a> {
        Box::pin({
            futures_util::stream::iter(self.records.clone().into_iter().map(Ok).collect::<Vec<_>>())
        })
    }
}

/// A ServiceMode record (priority 1) with nothing set — each test adds the
/// one parameter it is about.
fn service_record() -> SvcbEndpoint {
    SvcbEndpoint::new(1, ORIGIN.to_string())
}

// --- the client ---------------------------------------------------------

/// `Tokio`, plus an offset a test can add to every elapsed measurement.
///
/// The runtime seam is how time reaches `hclient-native` — `Native`'s
/// epoch, the pool's deadlines and the negative cache's window are all
/// `Timer::elapsed_since` against one instant — so a wrapper that adds to
/// that one method is enough to move the cache's window forward without
/// touching a single real timer: the Happy Eyeballs pacing below it goes
/// on running in real milliseconds, and a socket takes as long as it takes.
///
/// Chosen over `tokio::time::pause()` for two reasons. It needs no
/// `tokio/test-util`, which under Cargo's feature unification would be
/// switched on for every crate in a workspace test run, not just this one.
/// And a paused clock auto-advances to the next pending timer whenever the
/// runtime idles, which in a file whose connections are meant to *fail*
/// makes the amount of virtual time that passes a property of how the
/// scheduler happened to park.
///
/// At `skew = 0` it is `Tokio` with an extra `Arc` load per measurement,
/// which is what every test below one uses.
#[derive(Clone)]
struct Skewed {
    inner: Tokio,
    /// Milliseconds added to every `elapsed_since`.
    skew: Arc<AtomicU64>,
}

impl Skewed {
    fn new() -> Self {
        Self {
            inner: Tokio,
            skew: Arc::new(AtomicU64::new(0)),
        }
    }

    /// Moves this runtime's idea of elapsed time forward by `d`, for
    /// everything that reads it through `Timer`.
    fn advance(&self, d: Duration) {
        self.skew.fetch_add(d.as_millis() as u64, Ordering::SeqCst);
    }
}

impl TcpConnect for Skewed {
    type ConnectingUnix<'a>
        = hclient_rt::UnixUnsupported<Self::Stream>
    where
        Self: 'a;

    fn connect_unix<'a>(&'a self, _path: &std::path::Path) -> Self::ConnectingUnix<'a> {
        hclient_rt::UnixUnsupported::new()
    }

    type Stream = <Tokio as TcpConnect>::Stream;
    const APPLIES: TcpOptsSupport = <Tokio as TcpConnect>::APPLIES;
    type Connecting<'a>
        = std::pin::Pin<
        Box<dyn std::future::Future<Output = std::io::Result<Self::Stream>> + Send + 'a>,
    >
    where
        Self: 'a;

    fn connect<'a>(&'a self, addr: SocketAddr, opts: &TcpOpts) -> Self::Connecting<'a> {
        let opts = opts.clone();
        Box::pin(async move { self.inner.connect(addr, &opts).await })
    }
}

impl Timer for Skewed {
    type Instant = <Tokio as Timer>::Instant;
    type Sleep = <Tokio as Timer>::Sleep;
    fn sleep(&self, d: Duration) -> Self::Sleep {
        self.inner.sleep(d)
    }
    fn now(&self) -> Self::Instant {
        self.inner.now()
    }
    fn elapsed_since(&self, earlier: Self::Instant) -> Duration {
        self.inner.elapsed_since(earlier) + Duration::from_millis(self.skew.load(Ordering::SeqCst))
    }
}

fn transport(dns: FakeDns) -> Native<Skewed, Rustls, FakeDns> {
    Native::new(Skewed::new(), Rustls::with_webpki_roots(), dns)
}

/// One request, whose result is deliberately discarded: every peer in this
/// file drops the connection during the handshake, so every request here
/// fails, and the failure is never what is being measured.
async fn request(t: &Native<Skewed, Rustls, FakeDns>, uri: &str) {
    let req = http::Request::builder()
        .uri(uri)
        .body(RequestBody::Empty)
        .expect("request");
    let _ = tokio::time::timeout(BOUND, t.execute(req))
        .await
        .expect("a request must not hang");
}

fn origin_uri() -> String {
    format!("https://{ORIGIN}/")
}

// --- the port -----------------------------------------------------------

#[tokio::test]
async fn the_port_from_the_record_is_where_the_connection_goes() {
    let peer = Peer::start();
    let dns = FakeDns::with_loopback().serving(service_record().port(Some(peer.port())));

    request(&transport(dns), &origin_uri()).await;

    assert_eq!(
        peer.count(),
        1,
        "the record named this port; nothing else in the request did, and the \
         origin's own endpoint (127.0.0.1:443) has no listener at all"
    );
}

/// The control for the test above, and the one claim that is about the
/// capability rather than the record: the same records, from a resolver
/// that reports it cannot answer SVCB, must be left alone.
///
/// Without it, "the port was used" would also pass for a connector that
/// called `lookup_svcb` unconditionally — which is precisely what
/// `Resolve::supports_svcb` exists to prevent, since the default
/// `lookup_svcb` returns an empty stream and cannot be told apart from a
/// resolver that asked and found nothing.
#[tokio::test]
async fn a_resolver_that_says_it_cannot_ask_is_not_asked() {
    let peer = Peer::start();
    let dns = FakeDns::with_loopback()
        .serving(service_record().port(Some(peer.port())))
        .but_cannot_ask();

    request(&transport(dns), &origin_uri()).await;

    assert_eq!(
        peer.count(),
        0,
        "supports_svcb() is false, so the records must not be consulted at all"
    );
}

/// RFC 9460 §9.5: the record for a non-default port lives under a prefixed
/// name (`_8443._https.…`), and the one this client fetched is the origin
/// name's. Applying it to a URI that named its own port would be applying
/// one service's parameters to another's.
#[tokio::test]
async fn a_record_is_not_applied_to_a_uri_that_named_its_own_port() {
    let peer = Peer::start();
    let elsewhere = Peer::start();
    let dns = FakeDns::with_loopback().serving(service_record().port(Some(elsewhere.port())));

    // The URI names the peer's port itself. If the record were consulted,
    // the connection would go to `elsewhere` instead.
    request(
        &transport(dns),
        &format!("https://{ORIGIN}:{}/", peer.port()),
    )
    .await;

    assert_eq!(
        peer.count(),
        1,
        "the URI's own port is where the request goes when a record does not apply"
    );
    assert_eq!(
        elsewhere.count(),
        0,
        "the default-port record must not move a request that named a port"
    );
}

// --- the address hints --------------------------------------------------

/// The hints are the only source of an address here: the resolver answers
/// neither family. A connection can therefore only arrive if the hint
/// reached Happy Eyeballs.
#[tokio::test]
async fn the_address_hints_reach_happy_eyeballs() {
    let peer = Peer::start();
    let dns = FakeDns::default().serving(
        service_record()
            .port(Some(peer.port()))
            .ipv4hint(vec![Ipv4Addr::LOCALHOST]),
    );

    request(&transport(dns), &origin_uri()).await;

    assert_eq!(
        peer.count(),
        1,
        "with an empty resolver, the ipv4hint is the only address there is"
    );
}

// --- ECH ----------------------------------------------------------------

/// **The trap this whole item had to settle first.** `hclient-tls-rustls`
/// refuses a non-`None` `TlsRequest::ech` before a byte reaches the wire
/// (v0.3 W2 part 1, and `hclient-tls-rustls/tests/ech.rs` measures exactly
/// that). A connector that filled the field from every HTTPS record would
/// therefore make every origin publishing an ECH config **unreachable**:
/// discovery would turn a working request into a failing one.
///
/// So the config is offered only to a backend whose `applies_ech()` says
/// it will use one, and this test is what makes that conditional real: it
/// is red — with zero bytes at the peer — for a connector that fills the
/// field unconditionally, which is the state this file was first run in.
#[tokio::test]
async fn an_ech_publishing_origin_is_still_reachable() {
    let peer = Peer::start();
    // The ECH config is opaque on purpose: the decision must not depend on
    // parsing it, because an origin that published a malformed config is as
    // reachable as one that published a good one.
    let dns = FakeDns::with_loopback().serving(
        service_record()
            .port(Some(peer.port()))
            .ech_config_list(Some(Bytes::from_static(&[0x00, 0x41, 0xfe, 0x0d]))),
    );

    request(&transport(dns), &origin_uri()).await;

    assert_eq!(
        peer.count(),
        1,
        "an ECH config in the record must not cost the connection: no TLS backend \
         in this workspace applies one, and the config is offered only to a backend \
         that says it does"
    );
}

/// The other half of the decision, said out loud where it can be seen
/// rather than only in a doc comment: because no backend applies ECH, the
/// name the origin published a config to protect goes out **in the
/// clear**.
///
/// This is the cost of the choice above, and it is a fact about privacy
/// rather than an implementation detail. It is exhibited by the same
/// observer that asserts the connection survived, so neither half can be
/// changed without the other going red.
#[tokio::test]
async fn the_name_a_record_asked_to_protect_goes_out_in_the_clear() {
    let peer = Peer::start();
    let dns = FakeDns::with_loopback().serving(
        service_record()
            .port(Some(peer.port()))
            .ech_config_list(Some(Bytes::from_static(&[0x00, 0x41, 0xfe, 0x0d]))),
    );

    request(&transport(dns), &origin_uri()).await;

    let flight = peer.flights().pop().expect("a first flight must arrive");
    let needle = ORIGIN.as_bytes();
    assert!(
        flight.windows(needle.len()).any(|w| w == needle),
        "the ClientHello carries the server name in plaintext — this is what an \
         ECH config exists to prevent, and what a client with no ECH backend \
         still does after reading one"
    );
}

// --- the ALPN offer -----------------------------------------------------

/// RFC 9460 §7.1: a client offers the protocols it supports *from the
/// SVCB-ALPN set*, which is the record's `alpn` plus the scheme's default
/// (`http/1.1`). A record advertising only `http/1.1` therefore withdraws
/// `h2` from the offer — visible in the ClientHello and nowhere else.
///
/// Needs the `http2` feature: without it this transport offers `http/1.1`
/// alone whatever any record says, so there would be nothing for a record
/// to change.
#[cfg(feature = "http2")]
#[tokio::test]
async fn the_record_narrows_the_alpn_offer() {
    let peer = Peer::start();
    let dns = FakeDns::with_loopback().serving(
        service_record()
            .port(Some(peer.port()))
            .alpn(vec![b"http/1.1".to_vec()]),
    );

    request(&transport(dns), &origin_uri()).await;

    let flight = peer.flights().pop().expect("a first flight must arrive");
    let offer = alpn_offer(&flight).expect("the ClientHello must carry an ALPN extension");
    assert_eq!(
        offer,
        vec![b"http/1.1".to_vec()],
        "the record advertises http/1.1 only, so h2 is not in the SVCB-ALPN set \
         and must not be offered"
    );
}

/// **`Native::http1(false)` withdraws `http/1.1` from the ClientHello**,
/// which is the guarantee `Capabilities::full_duplex` rests on when that
/// setter raises the floor.
///
/// It lives in this file rather than beside its siblings in
/// `tests/version_policy.rs` because the `Peer`/`alpn_offer` pair that
/// reads a first flight is here, and one test travelling to the fixture is
/// cheaper than the fixture travelling to a shared module.
///
/// Without this, `version_policy.rs` would pass for a transport that
/// raised the floor and went on offering `http/1.1` anyway — a capability
/// that lies, and exactly the shape a test reading only `capabilities()`
/// cannot see.
#[cfg(feature = "http2")]
#[tokio::test]
async fn forbidding_http1_withdraws_it_from_the_alpn_offer() {
    let peer = Peer::start();
    let dns = FakeDns::with_loopback().serving(
        service_record()
            .port(Some(peer.port()))
            .alpn(vec![b"h2".to_vec(), b"http/1.1".to_vec()]),
    );
    let t = transport(dns)
        .http1(false)
        .expect("h2 is compiled in and on by default");

    request(&t, &origin_uri()).await;

    let flight = peer.flights().pop().expect("a first flight must arrive");
    let offer = alpn_offer(&flight).expect("the ClientHello must carry an ALPN extension");
    assert_eq!(
        offer,
        vec![b"h2".to_vec()],
        "the record advertises both, so the narrowing here is the setter's and \
         not the record's — a server that cannot speak h2 must find no overlap"
    );
}

/// The control, and the reason the test above is about the record rather
/// than about this transport having quietly stopped offering `h2`: the
/// same fixture, with `h2` advertised, must put `h2` on the wire — first,
/// because the list is ranked and h2 is what we want when it is on offer.
#[cfg(feature = "http2")]
#[tokio::test]
async fn a_record_that_advertises_h2_leaves_it_in_the_offer() {
    let peer = Peer::start();
    let dns = FakeDns::with_loopback().serving(
        service_record()
            .port(Some(peer.port()))
            .alpn(vec![b"h2".to_vec(), b"http/1.1".to_vec()]),
    );

    request(&transport(dns), &origin_uri()).await;

    let flight = peer.flights().pop().expect("a first flight must arrive");
    let offer = alpn_offer(&flight).expect("the ClientHello must carry an ALPN extension");
    assert_eq!(offer, vec![b"h2".to_vec(), b"http/1.1".to_vec()]);
}

/// `h3` in a record is read and deliberately not acted on: this transport
/// speaks TCP, `hclient-h3` is a different crate with different bounds,
/// and a `Client` names one transport. What must not happen is for `h3`
/// to reach the ALPN offer, where a server
/// selecting it would leave the connection unusable.
#[cfg(feature = "http2")]
#[tokio::test]
async fn h3_in_a_record_is_never_offered_on_the_tcp_path() {
    let peer = Peer::start();
    let dns = FakeDns::with_loopback().serving(
        service_record()
            .port(Some(peer.port()))
            .alpn(vec![b"h3".to_vec(), b"h2".to_vec()]),
    );

    request(&transport(dns), &origin_uri()).await;

    let flight = peer.flights().pop().expect("a first flight must arrive");
    let offer = alpn_offer(&flight).expect("the ClientHello must carry an ALPN extension");
    assert!(
        !offer.iter().any(|p| p == b"h3"),
        "h3 must never be offered over TCP: {offer:?}"
    );
    assert!(
        offer.iter().any(|p| p == b"h2"),
        "the rest of the record's set is still usable: {offer:?}"
    );
}

// --- AliasMode ----------------------------------------------------------

/// An AliasMode record (priority 0) carries no parameters at all — RFC
/// 9460 §2.4.1 — and sorts *below* every ServiceMode record. A selection
/// that took the lowest priority without skipping it would pick the alias
/// every time and act on an endpoint with nothing in it: discovery would
/// look wired up and do nothing.
#[tokio::test]
async fn an_aliasmode_record_does_not_outrank_the_service_it_precedes() {
    let peer = Peer::start();
    let dns = FakeDns::with_loopback()
        .serving(
            service_record()
                .priority(0)
                .target("alias.example".to_string()),
        )
        .serving(service_record().port(Some(peer.port())));

    request(&transport(dns), &origin_uri()).await;

    assert_eq!(
        peer.count(),
        1,
        "priority 0 is AliasMode, not the best endpoint"
    );
}

// --- the negative cache -------------------------------------------------

/// The connection through the record's endpoint fails (this peer drops
/// every handshake), and the next request must not pay for it again.
///
/// Two requests, one arrival: the second went to the origin's own endpoint
/// — refused, since nothing can listen on `127.0.0.1:443` here — without
/// touching the record.
#[tokio::test]
async fn a_failed_discovery_is_not_repeated_by_the_next_request() {
    let peer = Peer::start();
    let dns = FakeDns::with_loopback().serving(service_record().port(Some(peer.port())));
    let t = transport(dns);

    request(&t, &origin_uri()).await;
    request(&t, &origin_uri()).await;

    assert_eq!(
        peer.count(),
        1,
        "the first failure is remembered for SVCB_FAILURE_TTL; a second arrival \
         means every request pays a broken record again"
    );
}

/// The other half of the same mechanism: the memory is a *window*, not a
/// verdict. Past [`SVCB_FAILURE_TTL`] the record is consulted again — an
/// operator who fixes a record must not need a new process.
///
/// Time moves through [`Skewed`], the one seam by which it reaches this
/// crate at all: `Native`'s epoch and every deadline derived from it are
/// `Timer::elapsed_since` readings, so adding to that reading is exactly
/// what a wall clock passing would do to the cache — and to nothing else,
/// since no real timer is touched. A cache that never expired would leave
/// the second arrival at one.
#[tokio::test]
async fn the_negative_cache_expires() {
    let peer = Peer::start();
    let dns = FakeDns::with_loopback().serving(service_record().port(Some(peer.port())));
    let rt = Skewed::new();
    let t = Native::new(rt.clone(), Rustls::with_webpki_roots(), dns);

    request(&t, &origin_uri()).await;
    request(&t, &origin_uri()).await;
    assert_eq!(peer.count(), 1, "the window is open");

    rt.advance(SVCB_FAILURE_TTL + Duration::from_secs(1));
    request(&t, &origin_uri()).await;

    assert_eq!(
        peer.count(),
        2,
        "past the window the record is consulted again"
    );
}
