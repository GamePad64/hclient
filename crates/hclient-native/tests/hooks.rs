//! The observability hooks (v0.4 W2), checked against what the server saw.
//!
//! # Every claim about a connection is the server's, not the hook's
//!
//! A hook that reports "reused" is trivially right about itself and can be
//! entirely wrong about the wire, which is the only thing a caller cares
//! about — so nothing below asserts an event on its own. Each event is
//! asserted **against a number the server produced**: how many TCP
//! connections it accepted, and how many it closed. One accept and one
//! `Reused` is reuse; two accepts and one `Reused` is a lie, and it is a
//! lie this file fails on. `tests/pool.rs` made the same argument for the
//! pool itself and this is that argument applied one layer up.
//!
//! # The pairs
//!
//! Nearly every test here has a control that differs in one field, for
//! `tests/pool.rs`'s reason: `two_requests_report_one_connect_and_one_reuse`
//! would also pass against a transport that emitted `Reused` unconditionally,
//! so `without_a_pool_the_same_two_requests_report_two_connects_and_no_reuse`
//! runs the same two requests against the same server through a transport
//! whose only difference is `Native::without_pool`.
#![cfg(not(target_family = "wasm"))]

use hclient::Client;
use hclient_core::unversioned::{Event, Hooks};
use hclient_dns_system::SystemDns;
use hclient_native::Native;
use hclient_rt_tokio::Tokio;
use hclient_tls_rustls::Rustls;
use std::io::{Read, Write};
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// Ceiling for anything that must not hang.
const BOUND: Duration = Duration::from_secs(30);

// ── the recorder ────────────────────────────────────────────────────────

/// What a test asserts on: one event, flattened to the facts, with the
/// borrowed pieces copied out because `Event<'_>` cannot outlive the call.
#[derive(Debug, Clone, PartialEq)]
enum Seen {
    /// This suite's transports never opt in to `1xx` — `Native::watching_1xx`
    /// is what turns it on — so this variant should stay unused here, and
    /// it is recorded rather than `unreachable!`d so that a regression
    /// shows up as an unexpected entry in an assertion rather than as a
    /// panic inside a hook on the request path.
    Informational {
        id: u64,
        status: u16,
    },
    Connected {
        id: u64,
        uri: String,
        remote: Option<SocketAddr>,
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
        /// this one always can — so every assertion below reads `Some`,
        /// which is the shape of the claim.
        version: Option<http::Version>,
        status: u16,
        elapsed: Duration,
    },
    Closed {
        id: u64,
        reason: Why,
    },
}

/// [`hclient_core::unversioned::CloseReason`] with the error's category
/// kept and the error itself dropped, so a test can compare with `==`.
#[derive(Debug, Clone, PartialEq)]
enum Why {
    Ended,
    Stale,
    Failed(String),
}

/// A hook that writes down what it was told.
///
/// **It takes a `Mutex` from inside the request path**, which is not
/// incidental: `Hooks`'s contract says no backend calls a hook while
/// holding a lock of its own, and a recorder that locks is the cheapest
/// way to have a test notice if that ever stops being true.
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

    fn connects(&self) -> usize {
        self.take()
            .iter()
            .filter(|e| matches!(e, Seen::Connected { .. }))
            .count()
    }

    fn reuses(&self) -> usize {
        self.take()
            .iter()
            .filter(|e| matches!(e, Seen::Reused { .. }))
            .count()
    }

    fn heads(&self) -> usize {
        self.take()
            .iter()
            .filter(|e| matches!(e, Seen::Head { .. }))
            .count()
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
}

impl Hooks for Recorder {
    fn on(&self, event: Event<'_>) {
        assert!(
            !self.explode.load(Ordering::SeqCst),
            "the hook was told to panic"
        );
        let seen = match event {
            Event::Informational(e) => Seen::Informational {
                id: e.id.get(),
                status: e.status.as_u16(),
            },
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
                    hclient_core::unversioned::CloseReason::Ended => Why::Ended,
                    hclient_core::unversioned::CloseReason::Stale => Why::Stale,
                    hclient_core::unversioned::CloseReason::Failed(err) => {
                        Why::Failed(format!("{:?}", err.kind()))
                    }
                },
            },
        };
        self.seen.lock().expect("recorder").push(seen);
    }
}

// ── the server ──────────────────────────────────────────────────────────

/// What the fixture server does beyond answering — the same shape as
/// `tests/pool.rs`'s, cut down to what this file needs and with one
/// behaviour that file has no use for ([`Behaviour::truncate`]).
#[derive(Clone, Default)]
struct Behaviour {
    /// Answer with `Connection: close`, so hyper's `Connection` future
    /// completes and the client learns the connection is over from the
    /// response itself.
    connection_close_header: bool,
    /// Promise a hundred bytes, send five, and close: the response head
    /// is fine and the body is not, which is the only way to make the
    /// *connection* fail rather than merely end.
    truncate: bool,
    /// Close the socket after this many responses, with no header saying
    /// so — the server whose keep-alive budget ran out, which the client
    /// can only discover by looking.
    responses_before_close: Option<usize>,
    /// Wait this long between the last response and dropping the socket.
    ///
    /// The same field, for the same reason, as `tests/pool.rs`'s: a
    /// server that writes a response and closes in the next few
    /// instructions races the client, and which side wins decides whether
    /// the connection was ever *pooled* — which is the whole premise of
    /// [`a_pooled_connection_the_server_closed_while_idle_is_reported_stale`].
    /// Without the delay that test saw `Ended` from the body's own poll
    /// about half the time, which is a different (and also true) fact
    /// about a different situation.
    close_delay: Duration,
    /// Bumped once per socket after it has been dropped, so a test can
    /// wait for a close that has actually happened rather than sleeping.
    closes: Option<Arc<AtomicUsize>>,
}

/// A server that counts the connections it accepts.
fn server(behaviour: Behaviour) -> (SocketAddr, Arc<AtomicUsize>) {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = listener.local_addr().expect("local_addr");
    let accepted = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&accepted);
    std::thread::spawn(move || {
        for sock in listener.incoming() {
            let Ok(sock) = sock else { continue };
            counter.fetch_add(1, Ordering::SeqCst);
            let behaviour = behaviour.clone();
            std::thread::spawn(move || serve(sock, behaviour));
        }
    });
    (addr, accepted)
}

fn serve(mut sock: std::net::TcpStream, behaviour: Behaviour) {
    sock.set_read_timeout(Some(BOUND)).expect("read timeout");
    let mut buf: Vec<u8> = Vec::new();
    let mut chunk = [0u8; 1024];
    let mut served = 0usize;
    loop {
        let head_end = loop {
            if let Some(i) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                break i + 4;
            }
            match sock.read(&mut chunk) {
                Ok(0) | Err(_) => return,
                Ok(n) => buf.extend_from_slice(&chunk[..n]),
            }
        };
        buf.drain(..head_end);

        let wrote = if behaviour.truncate {
            sock.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 100\r\n\r\nshort")
        } else if behaviour.connection_close_header {
            sock.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok")
        } else {
            sock.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok")
        };
        if wrote.is_err() {
            return;
        }
        served += 1;
        if behaviour.truncate || Some(served) == behaviour.responses_before_close {
            std::thread::sleep(behaviour.close_delay);
            drop(sock);
            if let Some(closes) = &behaviour.closes {
                closes.fetch_add(1, Ordering::SeqCst);
            }
            return;
        }
    }
}

/// Waits for the server to have accepted `n` connections.
///
/// For the one test where nothing is exchanged on the connection: the
/// accept loop bumps its counter on a thread of its own, and a client
/// that connects and immediately refuses can reach its assertion first.
async fn server_has_accepted(accepted: &AtomicUsize, n: usize) {
    tokio::time::timeout(BOUND, async {
        while accepted.load(Ordering::SeqCst) < n {
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("the server never accepted {n} connection(s)"));
    assert_eq!(accepted.load(Ordering::SeqCst), n, "and not more than {n}");
}

/// Waits for the server to report `n` closed sockets — the same helper,
/// for the same reason, as `tests/pool.rs`'s: a test that slept instead
/// would be asserting on a race.
async fn server_has_closed(closes: &AtomicUsize, n: usize) {
    tokio::time::timeout(BOUND, async {
        while closes.load(Ordering::SeqCst) < n {
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("the server never closed {n} connection(s)"));
}

// ── the client ──────────────────────────────────────────────────────────

type Watched = Native<Tokio, Rustls, SystemDns<Tokio>, Recorder>;

fn watched(rec: &Recorder) -> Watched {
    Native::new(Tokio, Rustls::with_webpki_roots(), SystemDns::new(Tokio)).hooks(rec.clone())
}

async fn get_ok(client: &Client<Watched>, addr: SocketAddr) {
    let resp = tokio::time::timeout(BOUND, client.get(&format!("http://{addr}/")).send())
        .await
        .expect("must not hang")
        .expect("request must succeed");
    assert_eq!(resp.status(), 200);
    assert_eq!(
        resp.collect().await.expect("body").text().expect("text"),
        "ok"
    );
}

// ── reuse, against the server's own count ───────────────────────────────

#[tokio::test]
async fn two_requests_report_one_connect_and_one_reuse() {
    let (addr, accepted) = server(Behaviour::default());
    let rec = Recorder::default();
    let client = Client::builder(watched(&rec)).build().unwrap();

    get_ok(&client, addr).await;
    get_ok(&client, addr).await;

    assert_eq!(
        accepted.load(Ordering::SeqCst),
        1,
        "premise: the server accepted one connection for two requests"
    );
    assert_eq!(rec.connects(), 1, "one connection was made");
    assert_eq!(rec.reuses(), 1, "and the second request reused it");
    assert_eq!(rec.heads(), 2, "both requests got a head");
}

/// The control, and the test that makes the one above mean anything: the
/// same two requests, the same server, one field different.
#[tokio::test]
async fn without_a_pool_the_same_two_requests_report_two_connects_and_no_reuse() {
    let (addr, accepted) = server(Behaviour::default());
    let rec = Recorder::default();
    let client = Client::builder(watched(&rec).without_pool())
        .build()
        .unwrap();

    get_ok(&client, addr).await;
    get_ok(&client, addr).await;

    assert_eq!(
        accepted.load(Ordering::SeqCst),
        2,
        "premise: without a pool the server accepted two connections"
    );
    assert_eq!(rec.connects(), 2, "two connections were made");
    assert_eq!(
        rec.reuses(),
        0,
        "and nothing was reused — a `Reused` here would be a report of \
         something the server can prove did not happen"
    );
}

/// The id is the only thing tying a later event to the connection it is
/// about, so a `Reused` naming a connection nobody made is worse than no
/// id at all.
#[tokio::test]
async fn the_reuse_names_the_connection_that_was_made() {
    let (addr, _accepted) = server(Behaviour::default());
    let rec = Recorder::default();
    let client = Client::builder(watched(&rec)).build().unwrap();

    get_ok(&client, addr).await;
    get_ok(&client, addr).await;

    let seen = rec.take();
    let made = seen
        .iter()
        .find_map(|e| match e {
            Seen::Connected { id, .. } => Some(*id),
            _ => None,
        })
        .expect("a connection was made");
    let reused = seen
        .iter()
        .find_map(|e| match e {
            Seen::Reused { id, .. } => Some(*id),
            _ => None,
        })
        .expect("a connection was reused");
    assert_eq!(reused, made, "the reuse must name the connection it reuses");
    assert_ne!(made, 0, "a watched connection never gets the unwatched id");
    for e in &seen {
        if let Seen::Head { id, .. } = e {
            assert_eq!(*id, made, "both heads arrived on that same connection");
        }
    }
}

// ── what a connect says ─────────────────────────────────────────────────

#[tokio::test]
async fn the_connect_names_the_address_that_answered_and_the_protocol_spoken() {
    let (addr, _accepted) = server(Behaviour::default());
    let rec = Recorder::default();
    let client = Client::builder(watched(&rec)).build().unwrap();
    get_ok(&client, addr).await;

    let Some(Seen::Connected {
        remote,
        version,
        uri,
        ..
    }) = rec.take().into_iter().next()
    else {
        panic!("the first event must be the connect");
    };
    assert_eq!(
        remote,
        Some(addr),
        "the address reported must be the one the server is listening on — \
         `Some`, because a TCP connection has one; a Unix-domain socket is \
         where the `None` lives"
    );
    assert_eq!(version, http::Version::HTTP_11);
    assert_eq!(uri, format!("http://{addr}/"));
}

/// `tls: None` is a fact and not a missing measurement — a `Some(ZERO)`
/// here would read as an instant handshake, which is a different claim
/// and a false one.
#[tokio::test]
async fn a_plaintext_connection_reports_no_tls_phase_at_all() {
    let (addr, _accepted) = server(Behaviour::default());
    let rec = Recorder::default();
    let client = Client::builder(watched(&rec)).build().unwrap();
    get_ok(&client, addr).await;

    let Some(Seen::Connected { tls, .. }) = rec.take().into_iter().next() else {
        panic!("the first event must be the connect");
    };
    assert_eq!(tls, None, "`http://` has no handshake to time");
}

/// The one invariant the three phases really do have, asserted as an
/// ordering rather than as a magnitude: they are disjoint intervals
/// inside the whole connect, so their sum cannot exceed it. A phase
/// measured from an earlier instant than its own start breaks this
/// without any clock-watching.
#[tokio::test]
async fn the_phases_fit_inside_the_connect_they_are_phases_of() {
    let (addr, _accepted) = server(Behaviour::default());
    let rec = Recorder::default();
    let client = Client::builder(watched(&rec)).build().unwrap();
    get_ok(&client, addr).await;

    let Some(Seen::Connected {
        dns,
        tcp,
        tls,
        total,
        ..
    }) = rec.take().into_iter().next()
    else {
        panic!("the first event must be the connect");
    };
    let sum = dns + tcp + tls.unwrap_or(Duration::ZERO);
    assert!(
        sum <= total,
        "dns {dns:?} + tcp {tcp:?} + tls {tls:?} = {sum:?} must fit inside the connect's {total:?}"
    );
}

/// `Head::elapsed` contains the connect when there was one — the pair
/// (`elapsed`, `total`) is what tells a caller whether it was the
/// connection or the server, and that only works if the head's clock
/// started first.
#[tokio::test]
async fn the_head_is_timed_from_before_the_connect_not_after_it() {
    let (addr, _accepted) = server(Behaviour::default());
    let rec = Recorder::default();
    let client = Client::builder(watched(&rec)).build().unwrap();
    get_ok(&client, addr).await;

    let seen = rec.take();
    let Some(Seen::Connected { total, .. }) = seen.first().cloned() else {
        panic!("the first event must be the connect");
    };
    let Some(Seen::Head { elapsed, .. }) = seen
        .iter()
        .find(|e| matches!(e, Seen::Head { .. }))
        .cloned()
    else {
        panic!("a head must have been reported");
    };
    assert!(
        elapsed >= total,
        "the head at {elapsed:?} must include the connect's {total:?}"
    );
}

#[tokio::test]
async fn the_head_reports_the_status_the_server_sent() {
    let (addr, _accepted) = server(Behaviour::default());
    let rec = Recorder::default();
    let client = Client::builder(watched(&rec)).build().unwrap();
    get_ok(&client, addr).await;

    let heads: Vec<_> = rec
        .take()
        .into_iter()
        .filter_map(|e| match e {
            Seen::Head {
                status, version, ..
            } => Some((status, version)),
            _ => None,
        })
        .collect();
    assert_eq!(heads, vec![(200, Some(http::Version::HTTP_11))]);
}

// ── why a connection ended ──────────────────────────────────────────────

/// `Connection: close`: the exchange finishes the connection and nothing
/// went wrong. The control that makes this more than "some close event
/// fired" is `two_requests_report_one_connect_and_one_reuse` above, where
/// the same request against a keep-alive server reports **no** close and
/// a reuse instead.
#[tokio::test]
async fn a_connection_the_server_asked_to_close_ends_and_says_it_ended() {
    let (addr, _accepted) = server(Behaviour {
        connection_close_header: true,
        ..Behaviour::default()
    });
    let rec = Recorder::default();
    let client = Client::builder(watched(&rec)).build().unwrap();
    get_ok(&client, addr).await;

    let closes = rec.closes();
    assert_eq!(closes.len(), 1, "exactly one close for one connection");
    assert_eq!(closes[0].1, Why::Ended);
    assert_eq!(
        closes[0].0,
        rec.take()
            .iter()
            .find_map(|e| match e {
                Seen::Connected { id, .. } => Some(*id),
                _ => None,
            })
            .expect("a connection was made"),
        "the close must name the connection that was made"
    );
}

/// A body that stops short of its `Content-Length` is a **connection**
/// failure, and the close carries the error rather than a category of
/// our own invention.
#[tokio::test]
async fn a_truncated_body_closes_the_connection_with_the_failure() {
    let (addr, _accepted) = server(Behaviour {
        truncate: true,
        ..Behaviour::default()
    });
    let rec = Recorder::default();
    let client = Client::builder(watched(&rec)).build().unwrap();

    let resp = tokio::time::timeout(BOUND, client.get(&format!("http://{addr}/")).send())
        .await
        .expect("must not hang")
        .expect("the head is fine; it is the body that is not");
    let failed = resp.collect().await;
    assert!(failed.is_err(), "premise: the body must fail");

    let closes = rec.closes();
    assert_eq!(closes.len(), 1, "exactly one close");
    assert!(
        matches!(closes[0].1, Why::Failed(_)),
        "a connection that failed must not be reported as one that ended: {:?}",
        closes[0].1
    );
}

/// The one that explains a connect a caller did nothing to deserve: a
/// pooled connection the peer closed while it sat idle. The second
/// request finds it dead, reports it `Stale`, and pays for a fresh
/// connection — and the server's accept count is what says the fresh
/// connection was real.
#[tokio::test]
async fn a_pooled_connection_the_server_closed_while_idle_is_reported_stale() {
    let closed = Arc::new(AtomicUsize::new(0));
    let (addr, accepted) = server(Behaviour {
        responses_before_close: Some(1),
        // Long enough for the first request's body to end and its
        // connection to be checked in before the server closes it — see
        // `Behaviour::close_delay`.
        close_delay: Duration::from_millis(150),
        closes: Some(Arc::clone(&closed)),
        ..Behaviour::default()
    });
    let rec = Recorder::default();
    let client = Client::builder(watched(&rec)).build().unwrap();

    get_ok(&client, addr).await;
    // The premise: the socket is shut, not about to be. Without this the
    // second request wins the race as often as not and the test asserts
    // on whichever side of it the machine happened to be.
    server_has_closed(&closed, 1).await;
    // **And a turn for this client's reactor**, which is a different fact
    // from the one above and the one the premise actually needs.
    // `server_has_closed` says the peer dropped its socket; whether tokio
    // has processed the `FIN`'s readiness by the time `is_reusable` takes
    // its single non-suspending look is a scheduling question, and under
    // an oversubscribed run the answer was no — six captures in sixty at
    // `-j96`, every one of them `IncompleteMessage` with `accepted=1` and
    // the connection's `Closed(Ended)` arriving from inside the second
    // exchange instead of from the checkout.
    //
    // That is the far point of the pooled-reuse window
    // (`docs/pooled-reuse-race.md`), which this workspace documents as
    // residual and deliberately unfixed — so what the test was catching
    // was a race it exists to sit on one side of, not a defect. A sleep
    // here is a guard on the premise, not an assertion.
    tokio::time::sleep(Duration::from_millis(50)).await;
    get_ok(&client, addr).await;

    assert_eq!(
        accepted.load(Ordering::SeqCst),
        2,
        "premise: the second request had to open a connection of its own"
    );
    let closes = rec.closes();
    assert_eq!(
        closes.iter().filter(|(_, w)| *w == Why::Stale).count(),
        1,
        "the dead pooled connection must be reported stale: {closes:?}"
    );
    assert_eq!(
        rec.connects(),
        2,
        "and the fresh connection the caller paid for must be reported too"
    );
    assert_eq!(rec.reuses(), 0, "nothing was reused");
}

/// The close reasons are three because the code can tell three apart, and
/// this is the test that would fail if they collapsed into one: the same
/// client, two servers, two different answers.
#[tokio::test]
async fn the_three_reasons_are_not_one_reason_wearing_three_names() {
    let (ended_addr, _a) = server(Behaviour {
        connection_close_header: true,
        ..Behaviour::default()
    });
    let (failed_addr, _b) = server(Behaviour {
        truncate: true,
        ..Behaviour::default()
    });

    let rec = Recorder::default();
    let client = Client::builder(watched(&rec)).build().unwrap();
    get_ok(&client, ended_addr).await;
    let resp = client
        .get(&format!("http://{failed_addr}/"))
        .send()
        .await
        .expect("head");
    assert!(resp.collect().await.is_err(), "premise: the body fails");

    let reasons: Vec<Why> = rec.closes().into_iter().map(|(_, w)| w).collect();
    assert_eq!(reasons.len(), 2);
    assert_eq!(reasons[0], Why::Ended);
    assert!(matches!(reasons[1], Why::Failed(_)), "{reasons:?}");
}

/// **One socket, one close event, even from a caller that keeps asking.**
///
/// `http_body` leaves polling past the end unspecified, so a wrapper (or
/// a caller) that polls once more is not doing anything wrong — and a
/// second `Closed` for the same connection would make anybody counting
/// open connections from these events drift downwards, which looks
/// exactly like a leak in the other direction. `H1Body::report_closed`
/// takes the id rather than reading it, and this is the test that makes
/// that `take` mean something: mutation testing found the version that
/// only reads it passing every other test in this file.
///
/// It goes through `Transport::execute` rather than `Client`, because
/// `Client` hands back a body it has already decided how to consume and
/// this test's whole subject is the extra poll.
#[tokio::test]
async fn a_body_polled_past_its_end_reports_one_close_and_not_two() {
    use hclient_core::RequestBody;
    use hclient_core::unversioned::Transport;
    use http_body::Body;

    // `without_pool`, and that is what makes the test deterministic
    // rather than a race. The connection outlives the exchange (the
    // server keeps it), so the body — not `h1::exchange` — is what
    // reports the end; with reuse off there is no check-in to take the
    // id first, so `report_closed` is reached with something to take.
    // Against a `Connection: close` server the end is reported from
    // inside `exchange` instead, before the head, and the body never has
    // an id at all: that version of this test passed with the `take`
    // removed, which is how the shape above was arrived at.
    let (addr, _accepted) = server(Behaviour::default());
    let rec = Recorder::default();
    let t = watched(&rec).without_pool();

    let req = http::Request::builder()
        .uri(format!("http://{addr}/"))
        .body(RequestBody::Empty)
        .unwrap();
    let resp = t.execute(req).await.expect("request");
    let mut body = std::pin::pin!(resp.into_body());

    let mut ended = false;
    for _ in 0..8 {
        let frame = tokio::time::timeout(
            BOUND,
            std::future::poll_fn(|cx| body.as_mut().poll_frame(cx)),
        )
        .await
        .expect("the body must not hang");
        match frame {
            Some(Ok(_)) => assert!(!ended, "no frame may follow the end"),
            Some(Err(e)) => panic!("the body must not fail: {e}"),
            None => ended = true,
        }
    }
    assert!(ended, "the body must have ended within eight polls");

    assert_eq!(
        rec.closes().len(),
        1,
        "one connection ended once, however many times its body was asked: {:?}",
        rec.closes()
    );
}

/// **A connection that was made and then refused is still reported as
/// made.**
///
/// `Native::execute` emits `Connected` before it checks a
/// [`RequireVersion`] demand, and the order is the decision: the
/// connection exists at that instant and the server has paid for a TCP
/// handshake, so a caller asking "why was that slow" must be able to see
/// it. Emitting after the refusal would leave the one case where a
/// connect was made and thrown away invisible — which is the case a
/// caller most wants explained.
///
/// The refusal itself is `tests/require_version.rs`'s subject; what is
/// asserted here is only that the event survived it.
#[tokio::test]
async fn a_connection_refused_by_a_version_demand_was_still_reported_made() {
    use hclient::RequireVersion;

    let (addr, accepted) = server(Behaviour::default());
    let rec = Recorder::default();
    let client = Client::builder(watched(&rec)).build().unwrap();

    // Built as an `http::Request` rather than through `RequestBuilder`,
    // which has no extension setter — the same route
    // `tests/require_version.rs` takes, and the same one a caller has.
    let mut req = http::Request::builder()
        .method("GET")
        .uri(format!("http://{addr}/"))
        .body(hclient_core::RequestBody::Empty)
        .unwrap();
    req.extensions_mut()
        .insert(RequireVersion(http::Version::HTTP_2));
    let refused = client.execute(req).await;
    assert!(
        refused.is_err(),
        "`http://` cannot negotiate h2, so the demand must be refused"
    );

    // Waited for, not asserted outright: nothing was exchanged on this
    // connection, so the accept loop's `fetch_add` races the client's
    // refusal. The bound turns "has it happened yet" into a failure
    // rather than into a flake.
    server_has_accepted(&accepted, 1).await;
    assert_eq!(
        rec.connects(),
        1,
        "the connection the server accepted must be reported, refusal or not"
    );
    assert_eq!(rec.heads(), 0, "and not a byte of HTTP went out on it");
}

// ── a hook that misbehaves ──────────────────────────────────────────────

/// **A panicking hook unwinds out and leaves nothing broken behind it.**
///
/// The claim being tested is structural rather than about panics: no hook
/// is called while this transport holds the connection pool's mutex. If
/// one were — if `Closed`/`Stale` moved inside `Pool::take`, say — a
/// panicking hook would poison that mutex, and every later request on the
/// same transport would panic with "connection pool poisoned" instead of
/// working. So the test panics from a hook at the point nearest the pool
/// (`Reused`, emitted immediately after a checkout) and then requires the
/// **next** request to succeed.
///
/// It is a `#[test]` with a runtime built by hand rather than a
/// `#[tokio::test]`, because `catch_unwind` has to wrap the `block_on`:
/// a panic inside a spawned task would be caught by the runtime and the
/// test would be about `JoinError` instead of about the pool.
#[test]
fn a_panicking_hook_leaves_the_transport_usable() {
    let (addr, _accepted) = server(Behaviour::default());
    let rec = Recorder::default();
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime");
    let client = Client::builder(watched(&rec)).build().unwrap();

    // One quiet request, so there is a connection in the pool for the
    // next one to check out and report a `Reused` about.
    rt.block_on(get_ok(&client, addr));

    rec.explode.store(true, Ordering::SeqCst);
    let panicked = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        rt.block_on(get_ok(&client, addr));
    }));
    assert!(
        panicked.is_err(),
        "the panic must reach the caller rather than being swallowed"
    );

    rec.explode.store(false, Ordering::SeqCst);
    // The whole point: the transport, its pool and its mutex survived.
    rt.block_on(get_ok(&client, addr));
}

/// A hook that blocks holds up its own request and nothing else — which
/// is the honest consequence of "called synchronously, on the task that
/// is driving the request", and worth pinning because the alternative a
/// reader might assume (an internal queue, a spawned task) is exactly
/// what this seam does not have.
#[tokio::test]
async fn a_slow_hook_delays_its_own_request_and_no_other() {
    #[derive(Clone)]
    struct Slow(Arc<AtomicUsize>);
    impl Hooks for Slow {
        fn on(&self, _event: Event<'_>) {
            self.0.fetch_add(1, Ordering::SeqCst);
            std::thread::sleep(Duration::from_millis(20));
        }
    }
    let (addr, _accepted) = server(Behaviour::default());
    let calls = Arc::new(AtomicUsize::new(0));
    let client = Client::builder(
        Native::new(Tokio, Rustls::with_webpki_roots(), SystemDns::new(Tokio))
            .hooks(Slow(Arc::clone(&calls))),
    )
    .build()
    .unwrap();

    let resp = tokio::time::timeout(BOUND, client.get(&format!("http://{addr}/")).send())
        .await
        .expect("a blocking hook must not hang the request")
        .expect("request");
    assert_eq!(resp.status(), 200);
    assert!(
        calls.load(Ordering::SeqCst) >= 2,
        "the connect and the head at least"
    );
}
