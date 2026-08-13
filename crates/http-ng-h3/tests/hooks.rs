//! The observability hooks over HTTP/3, checked against what the server saw.
//!
//! # Every claim about a connection is the server's, not the hook's
//!
//! A hook that reports "reused" is trivially right about itself and can be
//! entirely wrong about the wire, which is the only thing a caller cares
//! about — so nothing below asserts an event on its own. Each event is
//! asserted **against a number the server produced**: how many QUIC
//! connections it accepted, and how many requests it answered. One accept
//! and one `Reused` is reuse; two accepts and one `Reused` is a lie, and it
//! is a lie this file fails on. `tests/live.rs` made the same argument for
//! the pool itself and this is that argument applied one layer up.
//!
//! # Where this differs from `http-ng-native`'s file of the same name
//!
//! Three places, and all three are facts about QUIC rather than about the
//! seam — `crates/http-ng-h3/src/hooks.rs` argues each one where the code
//! is:
//!
//! - **`Reused` can fire while another request is in flight.** An h2
//!   connection is checked out of the pool exclusively; a QUIC connection
//!   is shared, so a second request joins one that is already carrying the
//!   first. `a_second_request_joins_a_connection_that_is_still_carrying_the_
//!   first` is that shape, and it is causal: the first response's body is
//!   still unread when the second request goes out.
//! - **`ConnectTiming::tls` is always `None` and `tcp` holds the QUIC
//!   attempt.** QUIC's handshake *is* TLS, and `into_0rtt` hands back a
//!   usable connection before it completes, so there is no completed
//!   handshake to time.
//! - **`CloseReason::Ended` never appears.** Nothing in HTTP/3 ends a
//!   connection because an exchange ended; the two reasons this transport
//!   can tell apart are `Stale` and `Failed`, and
//!   `the_two_reasons_are_not_one_reason_wearing_two_names` is the pair
//!   that says so.
#![cfg(not(target_family = "wasm"))]

mod server;

use http_body_util::BodyExt;
use http_ng_core::unversioned::{Event, Hooks, Transport};
use http_ng_core::{Error, ErrorKind, RequestBody};
use http_ng_dns::{IpLiteralOnly, Resolve, ResolvedAddr};
use http_ng_h3::H3;
use http_ng_rt_tokio::TokioHandle;
use server::Behaviour;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

// ── the recorder ────────────────────────────────────────────────────────

/// What a test asserts on: one event, flattened to the facts, with the
/// borrowed pieces copied out because `Event<'_>` cannot outlive the call.
#[derive(Debug, Clone, PartialEq)]
enum Seen {
    Connected {
        id: u64,
        uri: String,
        remote: SocketAddr,
        version: http::Version,
        dns: Duration,
        tcp: Duration,
        tls: Option<Duration>,
        total: Duration,
    },
    Reused {
        id: u64,
        uri: String,
        version: http::Version,
    },
    Head {
        id: u64,
        uri: String,
        /// `Option`, because `Head::version` is one: `None` is what a
        /// transport that could not observe the protocol reports, and
        /// this one always can — it speaks HTTP/3 and refuses every
        /// other demand.
        version: Option<http::Version>,
        status: u16,
        elapsed: Duration,
    },
    Closed {
        id: u64,
        reason: Why,
    },
}

/// [`http_ng_core::unversioned::CloseReason`] with the error's category
/// kept and the error itself dropped, so a test can compare with `==`.
#[derive(Debug, Clone, PartialEq)]
enum Why {
    Ended,
    Stale,
    Failed(ErrorKind),
}

/// A hook that writes down what it was told.
///
/// **It takes a `Mutex` from inside the request path**, which is not
/// incidental: `Hooks`'s contract says no backend calls a hook while
/// holding a lock of its own, and a recorder that locks is the cheapest way
/// to have a test notice if that ever stops being true. Here it would
/// notice twice over — this crate's pool mutex is taken in `checkout` on
/// the same code path that reports `Stale`.
#[derive(Clone, Default)]
struct Recorder {
    seen: Arc<Mutex<Vec<Seen>>>,
    /// When set, every event panics — see
    /// [`a_panicking_hook_leaves_the_transport_usable`].
    explode: Arc<AtomicBool>,
}

impl Recorder {
    fn take(&self) -> Vec<Seen> {
        self.seen.lock().expect("recorder").clone()
    }

    fn connects(&self) -> Vec<Seen> {
        self.take()
            .into_iter()
            .filter(|e| matches!(e, Seen::Connected { .. }))
            .collect()
    }

    fn reuses(&self) -> Vec<Seen> {
        self.take()
            .into_iter()
            .filter(|e| matches!(e, Seen::Reused { .. }))
            .collect()
    }

    fn heads(&self) -> Vec<Seen> {
        self.take()
            .into_iter()
            .filter(|e| matches!(e, Seen::Head { .. }))
            .collect()
    }

    fn closes(&self) -> Vec<(u64, Why)> {
        self.take()
            .iter()
            .filter_map(|e| match e {
                Seen::Closed { id, reason } => Some((*id, reason.clone())),
                _ => None,
            })
            .collect()
    }

    /// The one `Connected` this test expects, unpacked.
    fn only_connect(
        &self,
    ) -> (
        u64,
        SocketAddr,
        Duration,
        Duration,
        Option<Duration>,
        Duration,
    ) {
        let c = self.connects();
        assert_eq!(c.len(), 1, "expected exactly one Connected, got {c:#?}");
        match &c[0] {
            Seen::Connected {
                id,
                remote,
                dns,
                tcp,
                tls,
                total,
                ..
            } => (*id, *remote, *dns, *tcp, *tls, *total),
            other => unreachable!("{other:?}"),
        }
    }

    fn only_head(&self) -> (u64, u16, Duration) {
        let h = self.heads();
        assert_eq!(h.len(), 1, "expected exactly one Head, got {h:#?}");
        match &h[0] {
            Seen::Head {
                id,
                status,
                version,
                elapsed,
                uri: _,
            } => {
                // Asserted here rather than in one test, because every
                // head this file looks at comes through this helper and
                // the claim is about all of them. `Head::version` is an
                // `Option` so that `http-ng-fetch` and `http-ng-wasi` can
                // say *not observed*; this transport speaks HTTP/3 and
                // nothing else and reports `version_reported: true`, so
                // `None` here would be that field's other kind of lie —
                // and nothing in this crate would otherwise notice.
                assert_eq!(
                    *version,
                    Some(http::Version::HTTP_3),
                    "a transport whose capabilities say `version_reported: \
                     true` owes the event a version"
                );
                (*id, *status, *elapsed)
            }
            other => unreachable!("{other:?}"),
        }
    }
}

impl Hooks for Recorder {
    fn on(&self, event: Event<'_>) {
        assert!(
            !self.explode.load(Ordering::SeqCst),
            "the hook was told to panic"
        );
        let seen = match event {
            Event::Connected(e) => Seen::Connected {
                id: e.id.get(),
                uri: e.uri.to_string(),
                remote: e.remote,
                version: e.version,
                dns: e.timing.dns,
                tcp: e.timing.tcp,
                tls: e.timing.tls,
                total: e.timing.total,
            },
            Event::Reused(e) => Seen::Reused {
                id: e.id.get(),
                uri: e.uri.to_string(),
                version: e.version,
            },
            Event::Head(e) => Seen::Head {
                id: e.id.get(),
                uri: e.uri.to_string(),
                status: e.status.as_u16(),
                version: e.version,
                elapsed: e.elapsed,
            },
            Event::Closed(e) => Seen::Closed {
                id: e.id.get(),
                reason: match e.reason {
                    http_ng_core::unversioned::CloseReason::Ended => Why::Ended,
                    http_ng_core::unversioned::CloseReason::Stale => Why::Stale,
                    http_ng_core::unversioned::CloseReason::Failed(err) => {
                        Why::Failed(err.kind().clone())
                    }
                },
            },
        };
        self.seen.lock().expect("recorder").push(seen);
    }
}

// ── the fixtures ────────────────────────────────────────────────────────

/// The transport under test, watched.
fn watched(
    cert: &rustls::pki_types::CertificateDer<'static>,
    rec: &Recorder,
) -> H3<TokioHandle, http_ng_tls_rustls::Rustls, IpLiteralOnly, Recorder> {
    H3::new(
        TokioHandle::current().expect("inside #[tokio::test]"),
        server::client_tls(cert),
        IpLiteralOnly,
    )
    .expect("H3::new does no I/O")
    .hooks(rec.clone())
}

fn get(addr: SocketAddr, path: &str) -> http::Request<RequestBody> {
    http::Request::builder()
        .uri(format!("https://{addr}{path}"))
        .body(RequestBody::Empty)
        .unwrap()
}

async fn ok<T: Transport>(t: &T, addr: SocketAddr, path: &str)
where
    T::Body: http_body::Body<Data = bytes::Bytes>,
    <T::Body as http_body::Body>::Error: std::fmt::Debug,
{
    let r = t
        .execute(get(addr, path))
        .await
        .map_err(|_| ())
        .expect("h3 request");
    assert_eq!(r.status(), 200);
    let _ = r.into_body().collect().await.expect("body");
}

// ── connected, reused, head ─────────────────────────────────────────────

#[tokio::test(flavor = "multi_thread")]
async fn a_fresh_request_reports_the_connection_it_paid_for_and_the_head_it_got() {
    let s = server::start(Behaviour::Echo);
    let rec = Recorder::default();
    let t = watched(&s.cert_der, &rec);

    ok(&t, s.addr, "/hello").await;

    assert_eq!(s.accepted(), 1, "the server accepted one connection");
    // The version and the URI on the connection, which are not counts and
    // were not pinned until a mutation said so: reporting HTTP/1.1 here
    // survived every other test in this file, because nothing else reads
    // the field. It is `HTTP_3` by construction — `ALPN_H3` is the only
    // token offered and a connection that negotiates anything else never
    // reaches this line — and a claim that cannot be wrong is exactly the
    // one a regression would leave standing.
    match rec.connects().as_slice() {
        [Seen::Connected { version, uri, .. }] => {
            assert_eq!(*version, http::Version::HTTP_3);
            assert_eq!(uri, &format!("https://{}/hello", s.addr));
        }
        other => panic!("expected exactly one Connected, got {other:#?}"),
    }
    let (id, remote, _dns, tcp, tls, total) = rec.only_connect();
    assert_eq!(
        remote, s.addr,
        "the address that answered is the server's own, read back from \
         quinn rather than from what was dialled"
    );
    assert!(
        tcp > Duration::ZERO,
        "the QUIC attempt is a real measured interval, not a placeholder"
    );
    assert!(total >= tcp, "the attempt happened inside the connect");
    assert_eq!(
        tls, None,
        "QUIC's handshake IS TLS: there is no separate handshake to time, \
         and `into_0rtt` can hand back a usable connection before it \
         finishes at all — src/hooks.rs argues this where the code is"
    );

    let (head_id, status, elapsed) = rec.only_head();
    assert_eq!(head_id, id, "the head names the connection it arrived on");
    assert_eq!(status, 200);
    assert!(
        elapsed >= total,
        "`Head::elapsed` is measured from the same mark as \
         `ConnectTiming::total`, so it contains the connect — that pair is \
         what separates 'the connection was slow' from 'the server was slow'"
    );
    assert_eq!(rec.reuses(), vec![], "nothing was reused");
    assert_eq!(rec.closes(), vec![], "and nothing closed");
    assert_eq!(
        rec.take().len(),
        2,
        "two events and no more: {:#?}",
        rec.take()
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn two_requests_report_one_connect_and_one_reuse() {
    let s = server::start(Behaviour::Echo);
    let rec = Recorder::default();
    let t = watched(&s.cert_der, &rec);

    ok(&t, s.addr, "/one").await;
    ok(&t, s.addr, "/two").await;

    assert_eq!(s.requests(), 2, "both requests reached the server");
    assert_eq!(s.accepted(), 1, "on one connection — the server counted");
    assert_eq!(rec.connects().len(), 1);
    assert_eq!(rec.heads().len(), 2);

    let (made, ..) = rec.only_connect();
    match rec.reuses().as_slice() {
        [Seen::Reused { id, uri, version }] => {
            assert_eq!(
                *id, made,
                "the reuse names the connection that was MADE, not a fresh \
                 number — without this a hook could mint an id per request \
                 and no count would notice"
            );
            assert_eq!(uri, &format!("https://{}/two", s.addr));
            assert_eq!(*version, http::Version::HTTP_3);
        }
        other => panic!("expected exactly one Reused, got {other:#?}"),
    }
}

/// The control for the test above, and the reason it is not vacuous: the
/// same two requests through two transports, which share no pool, report
/// two connects and no reuse — and the server counts two connections.
///
/// `http-ng-native`'s equivalent uses `Native::without_pool()`; this
/// transport has no such switch, and two `H3`s is the same experiment.
#[tokio::test(flavor = "multi_thread")]
async fn two_transports_sharing_no_pool_report_two_connects_and_no_reuse() {
    let s = server::start(Behaviour::Echo);
    let rec = Recorder::default();
    let one = watched(&s.cert_der, &rec);
    let two = watched(&s.cert_der, &rec);

    ok(&one, s.addr, "/one").await;
    ok(&two, s.addr, "/two").await;

    assert_eq!(s.requests(), 2);
    assert_eq!(s.accepted(), 2, "two connections, as the server counted");
    assert_eq!(rec.connects().len(), 2, "and two Connected events");
    assert_eq!(
        rec.reuses(),
        vec![],
        "nothing was reused, so nothing may say so"
    );
    let ids: Vec<u64> = rec
        .connects()
        .iter()
        .map(|e| match e {
            Seen::Connected { id, .. } => *id,
            _ => unreachable!(),
        })
        .collect();
    assert_ne!(ids[0], ids[1], "two connections have two identities");
}

/// **The event that means something different here than it does over
/// TCP.** A QUIC connection is shared, so a second request does not wait
/// for the first to finish — it joins one that is still carrying it.
///
/// Causal rather than timed: the first response's body is deliberately not
/// read until after the second request has been sent, so the first exchange
/// is certainly still open. A transport that checked connections out
/// exclusively could not answer the second request on the same connection
/// at all, and the server's accept count would say so.
#[tokio::test(flavor = "multi_thread")]
async fn a_second_request_joins_a_connection_that_is_still_carrying_the_first() {
    let s = server::start(Behaviour::Echo);
    let rec = Recorder::default();
    let t = watched(&s.cert_der, &rec);

    // Held, not read: this exchange is still in flight for the whole of the
    // second request below.
    let first = t.execute(get(s.addr, "/first")).await.expect("first head");
    assert_eq!(first.status(), 200);

    let second = t
        .execute(get(s.addr, "/second"))
        .await
        .expect("second head");
    assert_eq!(second.status(), 200);

    assert_eq!(s.accepted(), 1, "one connection carried both");
    assert_eq!(s.requests(), 2);
    assert_eq!(rec.connects().len(), 1);
    assert_eq!(
        rec.reuses().len(),
        1,
        "the second request reused a connection whose first request had not \
         finished — `Reused` says 'a connection somebody else already made \
         is being used again', which stays true; what a caller must not do \
         is read two of them as two CONSECUTIVE uses"
    );

    // And both bodies still arrive afterwards, which is what says the first
    // exchange really was open rather than merely un-awaited.
    let _ = first.into_body().collect().await.expect("first body");
    let _ = second.into_body().collect().await.expect("second body");
    assert_eq!(rec.closes(), vec![], "nothing closed for any of that");
}

#[tokio::test(flavor = "multi_thread")]
async fn the_head_reports_the_status_the_server_sent() {
    // `425 Too Early`, because it is a status this transport passes through
    // untouched (`tests/live.rs`) and because a `200` would also be what a
    // hardcoded value looked like.
    let s = server::start(Behaviour::TooEarly);
    let rec = Recorder::default();
    let t = watched(&s.cert_der, &rec);

    let r = t.execute(get(s.addr, "/early")).await.expect("a response");
    assert_eq!(r.status(), 425);
    let _ = r.into_body().collect().await.expect("body");

    let (_, status, _) = rec.only_head();
    assert_eq!(status, 425);
}

// ── the phases ──────────────────────────────────────────────────────────

/// A resolver that answers `127.0.0.1` after a wait.
///
/// The wait is `tokio::time::sleep` rather than a thread sleep so the
/// runtime stays free to make progress — a blocking resolver would be
/// measuring the executor rather than the phase.
#[derive(Clone, Copy)]
struct SlowDns(Duration);

impl Resolve for SlowDns {
    /// Empty, not slow. `H3::resolve` asks v6 first and takes the first
    /// answer it gets, so a v6 stream that ends at once is what a v4-only
    /// host looks like, and the wait below is then the whole of resolution.
    fn lookup_ipv6(
        &self,
        _name: &str,
    ) -> impl futures_util::Stream<Item = Result<ResolvedAddr, Error>> {
        futures_util::stream::empty()
    }
    fn lookup_ipv4(
        &self,
        _name: &str,
    ) -> impl futures_util::Stream<Item = Result<ResolvedAddr, Error>> {
        let d = self.0;
        futures_util::stream::once(async move {
            tokio::time::sleep(d).await;
            Ok(ResolvedAddr {
                addr: IpAddr::V4(Ipv4Addr::LOCALHOST),
                ttl: None,
            })
        })
    }
}

/// Which number the code put a wait into, asked causally: the resolver
/// waits and nothing else does, so `dns > tcp` is a fact about attribution
/// rather than a stopwatch reading.
///
/// It closes two mutations at once. `tcp` stamped at the connect's start
/// rather than at the attempt's launch would contain the resolver's wait
/// and would therefore exceed `dns`; `dns` stamped at the attempt's launch
/// would be a few microseconds and would not.
#[tokio::test(flavor = "multi_thread")]
async fn a_slow_resolver_shows_up_as_dns_and_not_as_the_attempt() {
    const WAIT: Duration = Duration::from_millis(300);
    let s = server::start(Behaviour::Echo);
    let rec = Recorder::default();
    let t = H3::new(
        TokioHandle::current().expect("inside #[tokio::test]"),
        server::client_tls(&s.cert_der),
        SlowDns(WAIT),
    )
    .expect("H3::new does no I/O")
    .hooks(rec.clone());

    // A name rather than a literal, or `H3::resolve`'s IP shortcut would
    // answer before the resolver was asked. The certificate names
    // `localhost`, so the TLS side is not a second thing under test.
    let req = http::Request::builder()
        .uri(format!("https://localhost:{}/slow", s.addr.port()))
        .body(RequestBody::Empty)
        .unwrap();
    let r = t.execute(req).await.expect("h3 request");
    let _ = r.into_body().collect().await.expect("body");

    let (_, _, dns, tcp, tls, total) = rec.only_connect();
    assert!(
        dns > tcp,
        "the resolver waited {WAIT:?} and nothing else did, so the wait must \
         be in `dns` ({dns:?}) and not in the attempt ({tcp:?})"
    );
    assert!(dns >= WAIT, "and it must contain the whole wait: {dns:?}");
    assert!(
        dns + tcp <= total,
        "the phases are disjoint intervals inside the connect: \
         dns {dns:?} + tcp {tcp:?} > total {total:?}"
    );
    assert_eq!(tls, None);
}

/// A server that takes 300 ms to answer moves `Head::elapsed` and leaves
/// `ConnectTiming::total` alone — which is the pair the two figures exist
/// to make readable, and the one `http-ng-native`'s file records as
/// believed rather than measured.
#[tokio::test(flavor = "multi_thread")]
async fn a_slow_server_shows_up_in_the_head_and_not_in_the_connect() {
    const WAIT: Duration = Duration::from_millis(300);
    let s = server::start(Behaviour::Slow(WAIT));
    let rec = Recorder::default();
    let t = watched(&s.cert_der, &rec);

    ok(&t, s.addr, "/slow").await;

    let (_, _, _, _, _, total) = rec.only_connect();
    let (_, _, elapsed) = rec.only_head();
    assert!(
        elapsed >= WAIT,
        "the server held the answer for {WAIT:?}, so the head cannot have \
         been read sooner: {elapsed:?}"
    );
    assert!(
        total < elapsed,
        "and the connect ended before the request went out, so it cannot \
         contain the server's wait: connect {total:?}, head {elapsed:?}"
    );
}

// ── the two reasons a connection ends ───────────────────────────────────

/// A pooled connection the peer let die while it was idle: reported
/// `Stale`, naming the connection that was made, **before** the `Connected`
/// that replaces it.
///
/// The server's accept count is what says it really died — `1` would mean
/// the connection survived and the event was invented.
#[tokio::test(flavor = "multi_thread")]
async fn a_pooled_connection_that_died_while_idle_is_reported_stale() {
    let s = server::start_with_idle_timeout(Behaviour::Echo, Some(Duration::from_millis(1000)));
    let rec = Recorder::default();
    // Without a keep-alive, so the connection is allowed to die across the
    // gap — `tests/live.rs`'s idle A/B is the measurement that this is the
    // arm which loses its connection.
    let t = watched(&s.cert_der, &rec).without_keep_alive();

    ok(&t, s.addr, "/first").await;
    assert_eq!(s.accepted(), 1);
    let (first_id, ..) = rec.only_connect();

    tokio::time::sleep(Duration::from_millis(1500)).await;
    ok(&t, s.addr, "/second").await;

    assert_eq!(
        s.accepted(),
        2,
        "the connection really did die: the server had to accept a second"
    );
    assert_eq!(
        rec.closes(),
        vec![(first_id, Why::Stale)],
        "one close, naming the connection that was made, and `Stale` — the \
         event that explains the connect the caller did nothing to deserve"
    );
    assert_eq!(rec.connects().len(), 2);
    assert_eq!(rec.reuses(), vec![], "a dead connection is not a reuse");

    // Ordering, which is the half a count cannot see: the stale close comes
    // before the connect that replaces it.
    let kinds: Vec<&'static str> = rec
        .take()
        .iter()
        .map(|e| match e {
            Seen::Connected { .. } => "connected",
            Seen::Reused { .. } => "reused",
            Seen::Head { .. } => "head",
            Seen::Closed { .. } => "closed",
        })
        .collect();
    assert_eq!(kinds, ["connected", "head", "closed", "connected", "head"]);
}

/// A server that tears the connection down instead of answering: reported
/// `Failed`, and there is no head to report.
#[tokio::test(flavor = "multi_thread")]
async fn a_connection_the_server_tore_down_is_reported_failed() {
    let s = server::start(Behaviour::DieAfterHead);
    let rec = Recorder::default();
    let t = watched(&s.cert_der, &rec);

    let err = t
        .execute(get(s.addr, "/doomed"))
        .await
        .expect_err("the peer closed the connection; there is no response");
    assert_eq!(err.kind(), &ErrorKind::Body);

    assert_eq!(s.requests(), 1, "the server did read the head before dying");
    let (id, ..) = rec.only_connect();
    assert_eq!(
        rec.closes(),
        vec![(id, Why::Failed(ErrorKind::Body))],
        "the connection that failed is named, and the reason carries the \
         error rather than a category invented here"
    );
    assert_eq!(rec.heads(), vec![], "no head arrived, so none is reported");
}

/// The same death, met by the **body** instead of by `execute` — because
/// the response head arrived first.
///
/// Two observers, one event: `H3Body::poll_frame` is the only thing left
/// holding the connection once `execute` has returned, so a transport that
/// reported closes only from `execute` would report nothing at all here.
#[tokio::test(flavor = "multi_thread")]
async fn a_connection_that_dies_under_a_body_is_reported_by_the_body() {
    let s = server::start(Behaviour::HeadThenDie);
    let rec = Recorder::default();
    let t = watched(&s.cert_der, &rec);

    let r = t
        .execute(get(s.addr, "/head-then-die"))
        .await
        .expect("head");
    assert_eq!(r.status(), 200);
    let (id, ..) = rec.only_connect();
    let (head_id, ..) = rec.only_head();
    assert_eq!(head_id, id);
    assert_eq!(
        rec.closes(),
        vec![],
        "nothing has died yet: `execute` returned a head on a live connection"
    );

    let err = r
        .into_body()
        .collect()
        .await
        .expect_err("the connection went away mid-body");
    assert_eq!(err.kind(), &ErrorKind::Body);
    assert_eq!(
        rec.closes(),
        vec![(id, Why::Failed(ErrorKind::Body))],
        "and the body is what reported it"
    );
}

/// One data frame and then trailers — the one request shape this transport
/// refuses by name, borrowed from `tests/streaming.rs`, and here because it
/// is the cheapest **live-connection failure** in the crate.
struct DataThenTrailers {
    data: Option<bytes::Bytes>,
    trailers: Option<http::HeaderMap>,
}

impl http_body::Body for DataThenTrailers {
    type Data = bytes::Bytes;
    type Error = Error;

    fn poll_frame(
        mut self: std::pin::Pin<&mut Self>,
        _: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Result<http_body::Frame<bytes::Bytes>, Self::Error>>> {
        if let Some(d) = self.data.take() {
            return std::task::Poll::Ready(Some(Ok(http_body::Frame::data(d))));
        }
        std::task::Poll::Ready(
            self.trailers
                .take()
                .map(|t| Ok(http_body::Frame::trailers(t))),
        )
    }
}

/// **One stream's failure is not the connection's end**, and on a transport
/// whose whole point is that neighbours survive, announcing it as one would
/// be the loudest possible lie.
///
/// `quinn::Connection::close_reason()` is the only discriminator this crate
/// has, and this is the test that it is consulted rather than assumed: the
/// request fails, no `Closed` is reported, and the *same connection* then
/// answers a second request — which the server's own accept count confirms.
///
/// The failure is a request-trailers refusal because it is the one this
/// crate raises itself, on a connection that is provably untouched. Two
/// shapes that look like candidates are not: a server that drops the
/// request stream without answering makes the client's h3 connection fail
/// too (measured — the server accepted a second connection), and a
/// **cancelled** request produces no error at all, so no hook is called.
#[tokio::test(flavor = "multi_thread")]
async fn one_streams_failure_is_not_the_connections_end() {
    let s = server::start(Behaviour::CountBody);
    let rec = Recorder::default();
    let t = watched(&s.cert_der, &rec);

    let mut trailers = http::HeaderMap::new();
    trailers.insert("x-checksum", http::HeaderValue::from_static("deadbeef"));
    let req = http::Request::builder()
        .method("POST")
        .uri(format!("https://{}/trailers", s.addr))
        .body(RequestBody::Streaming(Box::new(DataThenTrailers {
            data: Some(bytes::Bytes::from_static(b"payload")),
            trailers: Some(trailers),
        })))
        .unwrap();
    let err = t
        .execute(req)
        .await
        .expect_err("declared false, so refused");
    assert!(err.is_unsupported(), "{err}");
    assert_eq!(
        rec.closes(),
        vec![],
        "the stream died and the connection did not, so nothing may say it \
         did: {:#?}",
        rec.take()
    );

    // The proof that it really did live: a second request on it.
    ok(&t, s.addr, "/after").await;
    assert_eq!(
        s.accepted(),
        1,
        "one accept for both requests — the same connection carried them"
    );
    assert_eq!(rec.reuses().len(), 1, "and it is reported as a reuse");
    assert_eq!(rec.closes(), vec![], "still no close");
}

/// A connection whose death several requests meet reports **one** `Closed`.
///
/// `http-ng-native` gets this for free: an h2 connection is checked out of
/// the pool exclusively, so one body is the only observer. Here the
/// connection is shared and its end can be met from three places — a
/// failing `execute`, a failing body, and the next checkout that finds it
/// dead — so the "already told" flag lives with the connection.
///
/// The shape below is the deterministic one: the first request meets the
/// death in `execute`, and the second finds the same entry still in the
/// pool and would report it `Stale` a second time.
#[tokio::test(flavor = "multi_thread")]
async fn one_connection_reports_one_close_however_many_requests_meet_its_death() {
    let s = server::start(Behaviour::DieAfterHead);
    let rec = Recorder::default();
    let t = watched(&s.cert_der, &rec);

    let _ = t.execute(get(s.addr, "/one")).await.expect_err("dies");
    let _ = t.execute(get(s.addr, "/two")).await.expect_err("dies too");

    assert_eq!(s.accepted(), 2, "each request paid for a connection");
    let ids: Vec<u64> = rec
        .connects()
        .iter()
        .map(|e| match e {
            Seen::Connected { id, .. } => *id,
            _ => unreachable!(),
        })
        .collect();
    assert_eq!(ids.len(), 2);
    assert_eq!(
        rec.closes(),
        vec![
            (ids[0], Why::Failed(ErrorKind::Body)),
            (ids[1], Why::Failed(ErrorKind::Body)),
        ],
        "two connections, two closes — and in particular the first is NOT \
         reported again as `Stale` when the second request finds it in the \
         pool, which is what would make a caller counting connections wrong \
         in the direction that looks like a leak"
    );
}

/// The pair, in one process: `Stale` and `Failed` are two reasons and not
/// one reason wearing two names.
///
/// A transport that answered `Stale` for everything, or `Failed` for
/// everything, passes each of the two tests above on its own — because each
/// only ever sees one kind of death. This runs both against one recorder.
#[tokio::test(flavor = "multi_thread")]
async fn the_two_reasons_are_not_one_reason_wearing_two_names() {
    let rec = Recorder::default();

    let idle = server::start_with_idle_timeout(Behaviour::Echo, Some(Duration::from_millis(1000)));
    let t = watched(&idle.cert_der, &rec).without_keep_alive();
    ok(&t, idle.addr, "/first").await;
    tokio::time::sleep(Duration::from_millis(1500)).await;
    ok(&t, idle.addr, "/second").await;

    let dying = server::start(Behaviour::DieAfterHead);
    let t2 = watched(&dying.cert_der, &rec);
    let _ = t2
        .execute(get(dying.addr, "/doomed"))
        .await
        .expect_err("dies");

    let reasons: Vec<Why> = rec.closes().into_iter().map(|(_, r)| r).collect();
    assert_eq!(
        reasons,
        vec![Why::Stale, Why::Failed(ErrorKind::Body)],
        "two deaths, two reasons — and never `Ended`, which has no subject \
         in HTTP/3: nothing here ends a connection because an exchange ended"
    );
}

/// `CloseReason::Ended` is emitted by nothing in this crate, and that is
/// deliberate rather than an omission — see `src/hooks.rs`.
///
/// Asserted across every test in this file that produces a close, because
/// a variant nothing can emit is exactly the capability lie this workspace
/// keeps catching, and the honest way to hold it is to say so where a
/// reader will look.
#[tokio::test(flavor = "multi_thread")]
async fn a_clean_exchange_ends_no_connection() {
    let s = server::start(Behaviour::Echo);
    let rec = Recorder::default();
    let t = watched(&s.cert_der, &rec);

    ok(&t, s.addr, "/one").await;
    ok(&t, s.addr, "/two").await;
    drop(t);

    assert_eq!(
        rec.closes(),
        vec![],
        "a QUIC connection outlives its streams: two finished exchanges end \
         nothing, and dropping the transport reports nothing either — no \
         hook is ever called from a `Drop`"
    );
}

// ── 0-RTT ───────────────────────────────────────────────────────────────

/// **No event says a request went out in early data, and none says whether
/// it was accepted.** This pins the absence rather than describing it.
///
/// Adding one would need either a field on `Connected` or a variant of its
/// own, and `http-ng-core` is not this backend's to extend; learning the
/// *verdict* would need more than that, because it resolves after the
/// response body (8.63 ms against 8.58 ms, `docs/h3-research.md` §3.2) — so
/// it would take either a spawned task, which this feature may not have, or
/// making the caller wait for the round trip 0-RTT exists to skip.
///
/// What a marked request does report is exactly what an unmarked one does,
/// and the rejected first stream is invisible: **one `Head` and no second
/// `Connected`**, because one request got one response.
#[tokio::test(flavor = "multi_thread")]
async fn a_replayed_0_rtt_request_reports_one_head_and_one_connection() {
    let (a, b) = server::start_two_sharing_a_certificate(Behaviour::Echo);
    let rec = Recorder::default();
    // One `Rustls`, therefore one ticket store: a ticket from `a` is offered
    // to `b`, whose ticketer refuses it. `tests/live.rs` argues the setup.
    let t = H3::new(
        TokioHandle::current().expect("inside #[tokio::test]"),
        server::client_tls(&a.cert_der),
        IpLiteralOnly,
    )
    .expect("H3::new does no I/O")
    .hooks(rec.clone());

    ok(&t, a.addr, "/ticket").await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    rec.seen.lock().unwrap().clear();

    let mut marked = get(b.addr, "/replayed");
    marked.extensions_mut().insert(http_ng_core::AllowEarlyData);
    let r = t.execute(marked).await.expect("replayed, not surfaced");
    assert_eq!(r.status(), 200);
    let _ = r.into_body().collect().await.expect("body");

    assert_eq!(
        rec.connects().len(),
        1,
        "one connection was made to the second server"
    );
    assert_eq!(
        rec.heads().len(),
        1,
        "one request, one response, one head — the rejected stream is a \
         detail of how the response was obtained and not an outcome"
    );
    assert_eq!(
        rec.closes(),
        vec![],
        "a rejected 0-RTT stream is reset; the connection is fine, and \
         saying otherwise would be the loudest possible lie about a \
         transport whose point is that neighbours survive"
    );
    let (_, _, _, _, tls, _) = rec.only_connect();
    assert_eq!(
        tls, None,
        "and there is still no handshake duration to report: this \
         connection carried a request BEFORE its handshake completed"
    );
}

// ── the two rules the seam is built on ──────────────────────────────────

/// A panicking hook unwinds to the caller and leaves the transport usable —
/// which is what "no hook is called under a lock" buys.
///
/// The panic fires at the `Connected` event, which is the emission nearest
/// this crate's pool: the connection has just been inserted under the pool's
/// mutex and the guard dropped. If the emission had stayed inside
/// `checkout`, that mutex would be poisoned and the third request below
/// would die on `expect("pool mutex poisoned")` rather than succeed.
#[test]
fn a_panicking_hook_leaves_the_transport_usable() {
    let s = server::start(Behaviour::Echo);
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("runtime");
    let rec = Recorder::default();
    let t = rt.block_on(async {
        H3::new(
            TokioHandle::current().expect("inside block_on"),
            server::client_tls(&s.cert_der),
            IpLiteralOnly,
        )
        .expect("H3::new does no I/O")
        .hooks(rec.clone())
    });

    rec.explode.store(true, Ordering::SeqCst);
    let panicked = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        rt.block_on(ok(&t, s.addr, "/boom"));
    }));
    assert!(
        panicked.is_err(),
        "the panic must reach the caller rather than being swallowed: \
         `catch_unwind` needs `UnwindSafe` and does nothing under \
         `panic = abort`, so catching would be a promise that holds in some \
         builds and not others"
    );

    rec.explode.store(false, Ordering::SeqCst);
    // The whole point: the transport, its pool and its mutex survived.
    rt.block_on(ok(&t, s.addr, "/after"));
    assert!(
        !rec.heads().is_empty(),
        "the request after the panic really did complete"
    );
}

/// A request that never gets a connection reports nothing at all.
///
/// The other direction of `Connected`'s honesty: it is emitted after the
/// connection exists and not before, so a connect that times out leaves no
/// event claiming one was made.
#[tokio::test(flavor = "multi_thread")]
async fn a_connect_that_never_completes_reports_nothing() {
    // A UDP socket nobody reads: a black hole, the same fixture
    // `tests/live.rs` uses for the connect timeout.
    let hole = std::net::UdpSocket::bind("127.0.0.1:0").expect("a loopback bind");
    let addr = hole.local_addr().unwrap();
    let s = server::start(Behaviour::Echo);
    let rec = Recorder::default();
    let t = watched(&s.cert_der, &rec);

    let mut req = get(addr, "/nowhere");
    req.extensions_mut().insert(http_ng_core::Timeouts {
        connect: Some(Duration::from_millis(300)),
        ..Default::default()
    });
    let err = t
        .execute(req)
        .await
        .expect_err("a black hole never answers");
    assert!(matches!(
        err.kind(),
        ErrorKind::Timeout(http_ng_core::Phase::Connect)
    ));
    assert_eq!(
        rec.take(),
        vec![],
        "no connection was made, so nothing may say one was"
    );
}
