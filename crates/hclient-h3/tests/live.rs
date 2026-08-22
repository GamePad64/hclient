//! HTTP/3 over real QUIC on loopback.
//!
//! Every test here **runs**. The research measured a full client-plus-server
//! h3 exchange at `wall=2.9ms` on this shape of setup with an `rcgen`
//! certificate and two unprivileged `UdpSocket::bind("127.0.0.1:0")` calls,
//! so a compile-only claim would have been a choice rather than a
//! constraint.
#![cfg(not(target_family = "wasm"))]

mod server;
mod wire;

use hclient_core::unversioned::Transport;
use hclient_core::{AllowEarlyData, EarlyDataSupport, ErrorKind, RequestBody};
use hclient_dns::IpLiteralOnly;
use hclient_h3::H3;
use hclient_rt_tokio::TokioHandle;
use http_body_util::BodyExt;
use server::Behaviour;

/// The transport under test, over the real runtime seam.
fn h3(
    cert: &rustls::pki_types::CertificateDer<'static>,
) -> H3<TokioHandle, hclient_tls_rustls::Rustls, IpLiteralOnly> {
    H3::new(
        TokioHandle::current().expect("inside #[tokio::test]"),
        server::client_tls(cert),
        // The server is on 127.0.0.1 and the certificate names `localhost`,
        // so the request URI carries the literal and the TLS server name
        // comes from it. A resolver would be a second thing under test.
        IpLiteralOnly,
    )
    .expect("H3::new does no I/O")
}

fn get(addr: std::net::SocketAddr, path: &str) -> http::Request<RequestBody> {
    http::Request::builder()
        .uri(format!("https://{addr}{path}"))
        .body(RequestBody::Empty)
        .unwrap()
}

async fn body_of<B>(r: http::Response<B>) -> String
where
    B: http_body::Body<Data = bytes::Bytes>,
    B::Error: std::fmt::Debug,
{
    let bytes = r.into_body().collect().await.unwrap().to_bytes();
    String::from_utf8(bytes.to_vec()).unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn a_real_request_over_real_quic() {
    // The whole stack: our `quinn::Runtime` over `hclient_rt`'s `Timer`,
    // `Spawn` and the new `UdpBind`; our QUIC TLS seam over rustls; h3 on
    // top. An HTTP/1.1 or HTTP/2 client would get nothing at all from this
    // server — it speaks only QUIC — so a green run here is not something a
    // fallback could have produced.
    let s = server::start(Behaviour::Echo);
    let t = h3(&s.cert_der);

    let resp = t.execute(get(s.addr, "/hello")).await.expect("h3 request");
    assert_eq!(resp.status(), 200);
    assert_eq!(resp.version(), http::Version::HTTP_3);
    assert_eq!(body_of(resp).await, "hello over h3");
    assert_eq!(s.requests(), 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn two_requests_share_one_connection() {
    // The observer is the SERVER's accept count, not any counter this crate
    // also wrote — the rule `hclient-native`'s pool test already follows.
    let s = server::start(Behaviour::Echo);
    let t = h3(&s.cert_der);

    for _ in 0..2 {
        let r = t.execute(get(s.addr, "/x")).await.expect("h3 request");
        assert_eq!(r.status(), 200);
        let _ = body_of(r).await;
    }
    assert_eq!(s.requests(), 2, "both requests reached the server");
    assert_eq!(
        s.accepted(),
        1,
        "the second request must reuse the first connection, not open a second"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn requests_are_multiplexed_not_serialised() {
    // The property that distinguishes this transport's pool policy from
    // `hclient-native`'s h2 one, where a connection is checked out
    // EXCLUSIVELY for one exchange. Here two requests are in flight at once
    // on one connection, and the way to see it from outside is timing: the
    // server holds each request for 300 ms, so serialised would be ~600 ms
    // and multiplexed ~300 ms.
    //
    // The threshold is deliberately loose (450 ms) — this is a
    // one-bit question and the test must not become a benchmark that fails
    // on a busy runner.
    let s = server::start(Behaviour::Slow(std::time::Duration::from_millis(300)));
    let t = h3(&s.cert_der);

    // One request first, so the connection and its handshake are not part
    // of what is being timed.
    let _ = body_of(t.execute(get(s.addr, "/warm")).await.unwrap()).await;

    let start = std::time::Instant::now();
    let (a, b) = tokio::join!(t.execute(get(s.addr, "/a")), t.execute(get(s.addr, "/b")));
    let elapsed = start.elapsed();
    assert_eq!(a.unwrap().status(), 200);
    assert_eq!(b.unwrap().status(), 200);
    assert_eq!(s.accepted(), 1, "still one connection");
    assert!(
        elapsed < std::time::Duration::from_millis(450),
        "two 300ms requests on one connection took {elapsed:?}: that is \
         serialised, not multiplexed"
    );
}

/// One request, a gap longer than the server's idle timeout, then a second
/// request — reporting how many connections the server had to accept.
///
/// `1` means the connection survived the gap; `2` means it died and was
/// silently replaced. The observer is the server's own accept count, which
/// is the only thing that tells those two apart: from the client's side
/// both look like a successful second request.
async fn survives_gap(keep_alive: Option<std::time::Duration>) -> usize {
    let s = server::start_with_idle_timeout(
        Behaviour::Echo,
        Some(std::time::Duration::from_millis(1000)),
    );
    let t = match keep_alive {
        Some(d) => h3(&s.cert_der).keep_alive_interval(d),
        None => h3(&s.cert_der).without_keep_alive(),
    };

    let _ = body_of(t.execute(get(s.addr, "/first")).await.unwrap()).await;
    assert_eq!(s.accepted(), 1, "the first request opens exactly one");

    tokio::time::sleep(std::time::Duration::from_millis(1500)).await;

    let second = t.execute(get(s.addr, "/second")).await.expect("a response");
    assert_eq!(second.status(), 200);
    let _ = body_of(second).await;
    s.accepted()
}

#[tokio::test(flavor = "multi_thread")]
async fn an_idle_connection_survives_only_because_of_the_keep_alive() {
    // The A/B the research ran, with one correction it did not have:
    // there, the variable was whether anything
    // *drove* the connection. Here the driver is spawned in **both** arms —
    // it has to be, it is spawned in `connect` — so the only difference
    // left is the keep-alive, and the result says the driver alone is not
    // sufficient.
    //
    // That is worth a test rather than a comment because it is exactly the
    // shape of thing that gets "simplified" away: a driver is running, the
    // connection is being polled, and it dies anyway, because polling is
    // what lets a connection send a PING and not what makes it decide to.
    let with = survives_gap(Some(std::time::Duration::from_millis(300))).await;
    assert_eq!(
        with, 1,
        "with a 300ms keep-alive under a 1000ms idle timeout, the connection \
         must live across a 1500ms gap"
    );

    let without = survives_gap(None).await;
    assert_eq!(
        without, 2,
        "and without one it must not — otherwise the arm above proves \
         nothing, because something else is keeping the connection alive"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn dropping_one_request_does_not_disturb_the_others() {
    // W1's contract, and on this transport it has a subject: there really
    // are neighbours on the connection. In `hclient-native`'s h2 the same
    // property holds vacuously, because the pool hands out a connection
    // exclusively.
    let s = server::start(Behaviour::Slow(std::time::Duration::from_millis(400)));
    let t = h3(&s.cert_der);
    let _ = body_of(t.execute(get(s.addr, "/warm")).await.unwrap()).await;

    // `Box::pin`, not `tokio::pin!`: the latter gives a `Pin<&mut F>`, and
    // dropping THAT drops a borrow rather than the future — the future
    // itself would live on to the end of the scope and this test would
    // check nothing. (clippy says so too, which is how it was caught.)
    let mut doomed = Box::pin(t.execute(get(s.addr, "/doomed")));
    let survivor = t.execute(get(s.addr, "/survivor"));

    // Poll the doomed request far enough to open its stream, then drop it.
    tokio::select! {
        _ = &mut doomed => panic!("the server holds for 400ms; this cannot finish in 50"),
        _ = tokio::time::sleep(std::time::Duration::from_millis(50)) => {}
    }
    drop(doomed);

    let r = survivor
        .await
        .expect("cancelling one stream must not tear down the connection");
    assert_eq!(r.status(), 200);
    assert_eq!(body_of(r).await, "hello over h3");
    assert_eq!(s.accepted(), 1, "and it was the same connection throughout");
}

#[tokio::test(flavor = "multi_thread")]
async fn early_data_is_offered_only_to_a_request_the_caller_marked() {
    // The gate is the caller's mark, and this is the half of it that no
    // amount of body inspection could get right: both requests below have
    // the same (trivially replayable) body, so the ONLY difference is the
    // extension.
    let s = server::start(Behaviour::Echo);
    let t = h3(&s.cert_der);
    assert_eq!(
        t.capabilities().early_data,
        EarlyDataSupport::Supported,
        "rustls offers early data, so this transport reports that it can"
    );

    // A first visit, to get a ticket into the store.
    let _ = body_of(t.execute(get(s.addr, "/first")).await.unwrap()).await;

    // Marked and unmarked requests both work; the observable difference is
    // that they do not share a connection, because `enable_early_data` is a
    // property of the rustls config and therefore of the connection.
    let mut marked = get(s.addr, "/marked");
    marked.extensions_mut().insert(AllowEarlyData);
    let r = t.execute(marked).await.expect("marked request");
    assert_eq!(r.status(), 200);
    let _ = body_of(r).await;

    assert_eq!(
        s.accepted(),
        2,
        "a request marked for early data must not be served by a connection \
         built without it — the flag is part of the pool key"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_rejected_0_rtt_request_is_replayed_and_the_caller_never_sees_it() {
    // The second of the three failure paths, and the only one this
    // transport can do anything about. Two servers present the SAME
    // certificate with DIFFERENT ticketers, so a ticket issued by the first
    // is offered to the second and refused — the research's scenario 3,
    // where the response came back `Err(Undefined(ZeroRttRejected))`.
    //
    // This test asserts the caller gets a `200` instead. Which means it
    // asserts three things at once, and could not pass without any of them:
    // that `into_0rtt` actually took the shortcut (otherwise there is
    // nothing to reject), that the rejection was detected (by awaiting the
    // verdict, not by matching an error string), and that the replay went
    // out on the same connection.
    let (a, b) = server::start_two_sharing_a_certificate(Behaviour::Echo);
    // ONE `Rustls`, therefore one QUIC ticket store — which is what makes
    // a ticket from `a` reachable when connecting to `b`. Both servers are
    // `127.0.0.1`, and rustls keys its store by server name.
    let tls = server::client_tls(&a.cert_der);
    let t = H3::new(TokioHandle::current().unwrap(), tls, IpLiteralOnly).unwrap();

    // A first, ordinary visit, to be issued a ticket.
    let _ = body_of(t.execute(get(a.addr, "/ticket")).await.unwrap()).await;
    // NewSessionTicket arrives after the handshake, on its own schedule.
    // Without this the second connection has nothing to resume from and the
    // test would pass vacuously through the `into_0rtt` refusal path.
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;

    let mut marked = get(b.addr, "/replayed");
    marked.extensions_mut().insert(AllowEarlyData);
    let r = t
        .execute(marked)
        .await
        .expect("a rejected 0-RTT request is replayed, not surfaced");
    assert_eq!(r.status(), 200);
    assert_eq!(body_of(r).await, "hello over h3");
    assert_eq!(
        b.requests(),
        1,
        "the replay is a second STREAM, not a second request the server sees \
         answered twice"
    );
}

/// The same rejection, landing one step earlier — **on h3's control stream,
/// while the client is still setting itself up** — and it is not the
/// caller's either.
///
/// # This is a flake with the luck taken out of it
///
/// The test above replays a rejection that arrives on the *request* stream,
/// which is the case `crate::early`'s table describes. A rejection that
/// arrives a few microseconds earlier lands on the **control** stream
/// instead, and RFC 9114 §6.2.1 obliges h3 to treat that as
/// `H3_CLOSED_CRITICAL_STREAM` and close the QUIC connection — so
/// `h3::client::builder().build(..)` fails and there is no request yet for
/// a replay to be made of. The caller was handed
/// `ErrorKind::Connect(H3_CLOSED_CRITICAL_STREAM .. 0-RTT rejected)`,
/// which is exactly the rejection this crate promises it will never see.
///
/// It was found as a flake — **2 failures in 277 concurrent runs of this
/// suite**, 0 in 846 after.
///
/// # Two fixtures, and each removes one half of the luck
///
/// The failure needs the rejection to arrive **after** h3 opened its
/// control stream and **before** its write finished. Each end of that is
/// arranged rather than waited for, and neither is a duration:
///
/// - **After the open** is [`wire::Wire`], holding the server's flight
///   until the relay has seen a 0-RTT packet from the client. While the
///   flight is held the client's connection *cannot* learn of the
///   rejection, so whenever the scheduler next gives it a core it opens
///   early-data streams; a 0-RTT packet on the wire is that having
///   happened. Without it the client can be descheduled between
///   `into_0rtt()` and h3's first `poll_open_send`, the rejection lands
///   first, and `quinn_proto`'s `zero_rtt_rejected` — which resets the
///   next-stream-id counters — leaves h3 opening ordinary 1-RTT streams
///   and everything working. Measured: a 50 ms delay in exactly that place
///   makes both 0-RTT tests pass on the **unfixed** library, and the
///   preemption is the same thing at microsecond scale — 3 failures in 246
///   concurrent runs of this suite while this test relied on losing that
///   race being unlikely.
/// - **Before the write finishes** is the server pair's 8-byte
///   flow-control window: h3's SETTINGS frame does not fit, so the write
///   parks for credit that a discarded early-data stream will never be
///   given. Without it the write completes locally, h3 never notices the
///   rejection at all, and the exchange takes the *request* stream's path
///   above. See `server::start_two_sharing_a_certificate_and_a_tiny_window`.
///
/// # What the assertions are, and why the server makes them
///
/// `b.dialled() == 2` is the whole fix seen from the far side of the wire:
/// the early-data connection was destroyed and a second, ordinary one was
/// dialled to the same server. `b.requests() == 1` is the other half — the
/// fallback is a second **connection**, never a second request, which is
/// what keeps this out of `RetryKind`'s territory. Nothing of the caller's
/// request had been written when the first connection died.
///
/// **`dialled` and not `accepted`, and that was this test's own bug.** It
/// first asserted `b.accepted() == 2`, which counts connections *after*
/// their handshakes; this client closes the first one the instant its
/// handshake completes, so under load the server's `Incoming::await` loses
/// that race and yields an error rather than a connection — 1 failure in
/// 280 concurrent runs of this suite, the fixture being wrong rather than
/// the transport. `dialled` is incremented where `Endpoint::accept` yields,
/// before the handshake, and is right for a structural reason rather than a
/// probabilistic one: the accept loop is sequential on one thread, so a
/// test answered on the second connection is a test whose first connection
/// was already counted.
#[tokio::test(flavor = "multi_thread")]
async fn a_0_rtt_rejection_on_the_control_stream_is_not_the_callers_either() {
    // Small enough that h3's SETTINGS frame cannot be written in one go,
    // large enough that an ordinary connection recovers through
    // `MAX_STREAM_DATA` — the ticket-issuing exchange below is the control
    // that says so, because it is an ordinary connection and it succeeds.
    let (a, b) = server::start_two_sharing_a_certificate_and_a_tiny_window(Behaviour::Echo, 8);
    let tls = server::client_tls(&a.cert_der);
    let t = H3::new(TokioHandle::current().unwrap(), tls, IpLiteralOnly).unwrap();

    let _ = body_of(t.execute(get(a.addr, "/ticket")).await.unwrap()).await;
    // As above: `NewSessionTicket` arrives on its own schedule, and without
    // this the second connection has nothing to resume from and the test
    // passes vacuously through `into_0rtt`'s refusal path.
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;

    // The relay goes in front of `b` only, and only for the marked request:
    // the ticket above is an ordinary exchange and has nothing to order.
    let wire = wire::Wire::in_front_of(b.addr);
    // A backstop rather than the plan — the hold ends on the client's first
    // 0-RTT packet. Reaching it means the client put nothing into early
    // data, which is a failure worth seeing rather than one to wait out, and
    // it stays well under quinn's first PTO (~1 s) so nothing retransmits.
    wire.hold_server_flight(std::time::Duration::from_millis(800));
    let watcher = wire.release_on_early_data(std::time::Duration::from_secs(5));

    let mut marked = get(wire.addr, "/replayed");
    marked.extensions_mut().insert(AllowEarlyData);
    let r = t
        .execute(marked)
        .await
        .expect("a 0-RTT rejection is not a connect failure, wherever it lands");
    assert_eq!(r.status(), 200);
    assert_eq!(body_of(r).await, "hello over h3");
    assert!(
        watcher.join().expect("the relay's watcher thread"),
        "the premise: the client really did put something into early data, \
         so there really was an early-data control stream for the rejection \
         to destroy. Without this the test could pass on a client that \
         quietly did an ordinary handshake"
    );
    assert_eq!(
        b.dialled(),
        2,
        "the early-data connection was destroyed by the rejection and a \
         second, ordinary one was dialled — the server counts both offered \
         to its endpoint"
    );
    assert_eq!(
        b.requests(),
        1,
        "and the request went out exactly once, on the second: this is a \
         second CONNECTION, not a second request"
    );
}

/// The other half of that fallback, and the reason it is conditional: a
/// connect that failed with **nothing** in early data is not dialled a
/// second time.
///
/// `AllowEarlyData` is on the request and there has been no prior visit, so
/// there is no ticket, `into_0rtt` refuses and the handshake is an ordinary
/// one — which is the state in which a fallback would be a plain retry of a
/// failing connect: the same arguments to the same `connect_with`, arriving
/// at the error it already had, having spent the caller's `Timeouts::
/// connect` on the way. The bound itself cannot be doubled — `H3::stage`
/// wraps both dials in one `within_connect` — which is why the assertion
/// below is a dial count and not a duration.
///
/// The server is the one fixture here that makes `build` fail without any
/// early data being involved — see [`server::Behaviour::CloseOnAccept`] for
/// why it needs the window too.
#[tokio::test(flavor = "multi_thread")]
async fn a_connect_that_put_nothing_in_early_data_is_not_dialled_twice() {
    let s = server::start_with_a_tiny_window(Behaviour::CloseOnAccept, 8);
    let t = h3(&s.cert_der);

    let mut marked = get(s.addr, "/marked");
    marked.extensions_mut().insert(AllowEarlyData);
    let e = t
        .execute(marked)
        .await
        .map(|_| ())
        .expect_err("the server closed the connection under h3's setup");

    assert_eq!(
        *e.kind(),
        ErrorKind::Connect,
        "and it is the connect's failure, reported as such: {e:?}"
    );
    assert_eq!(
        s.dialled(),
        1,
        "one dial, not two — the request was marked, so the fallback was \
         reachable; what makes it unreachable is that nothing went out in \
         early data for the rejection to have destroyed"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_425_reaches_the_caller_untouched() {
    // RFC 8470 §5.2's third failure path, and the one this layer does not
    // own: the retry belongs to `Client`, which is the only thing that owns
    // a retry loop. So this pins what the TRANSPORT does — pass a `425`
    // through untouched — and stays true however the client-side retry
    // evolves, rather than becoming stale the moment somebody writes one.
    //
    // A transport that retried here would be making a redirect-shaped
    // decision behind the caller's back, on a response the caller can see.
    let s = server::start(Behaviour::TooEarly);
    let t = h3(&s.cert_der);
    let r = t.execute(get(s.addr, "/early")).await.expect("a response");
    assert_eq!(
        r.status(),
        http::StatusCode::TOO_EARLY,
        "the transport does not retry a 425; it reaches the caller"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn plaintext_http_is_refused_rather_than_silently_upgraded() {
    let s = server::start(Behaviour::Echo);
    let t = h3(&s.cert_der);
    let req = http::Request::builder()
        .uri(format!("http://{}/x", s.addr))
        .body(RequestBody::Empty)
        .unwrap();
    let err = t
        .execute(req)
        .await
        .expect_err("QUIC has no plaintext form");
    assert!(
        err.to_string().contains("no plaintext form"),
        "the error must say why, not just fail: {err}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn capabilities_describe_this_implementation_not_the_protocol() {
    // HTTP/3 does full duplex and streaming request bodies: a QUIC stream's
    // two halves are independent. That is NOT why these two are `true` —
    // they were `false` for as long as `execute` wrote the whole request
    // body and then read the head, and they moved in the change that split
    // the stream and made the write a future polled beside
    // `recv_response`.
    //
    // This is the declaration half of a pair, and it is worth nothing on
    // its own: flipping either field here is caught by this test, and
    // undoing the implementation while leaving the fields alone is caught
    // by `tests/streaming.rs`, which cannot pass without the behaviour.
    // `full_duplex` is the one that has to be earned rather than argued —
    // over-claiming it costs a caller a deadlock rather than a
    // degradation, which is the argument W3's floor rule is built on — and
    // `a_response_head_arrives_while_the_request_body_is_still_going_out`
    // is the exchange that cannot complete without it.
    let s = server::start(Behaviour::Echo);
    let t = h3(&s.cert_der);
    let c = t.capabilities();
    assert!(
        c.full_duplex,
        "the response head is delivered while the request body is still \
         being written — tests/streaming.rs measures it from the server"
    );
    assert!(
        c.streaming_request_body,
        "a RequestBody::Streaming is written frame by frame, paced by the \
         peer's flow control — tests/streaming.rs measures that too"
    );
    // Two neighbours that stayed `false`/`true` in the same set, so this is
    // a description of the implementation rather than a habit of saying
    // yes. `request_trailers` is the interesting one now: a streaming body
    // can produce a trailers frame, and this transport answers with a
    // typed error rather than dropping it.
    assert!(!c.request_trailers, "nothing here sends request trailers");
    assert!(c.response_trailers, "H3Body yields a trailers frame");
    assert_eq!(c.connection_reuse, hclient_core::ReuseSupport::Supported);
}

// ── `Timeouts::connect`, declared and enforced in the same change ───────
//
// This crate had no timeouts by decision, `connect` being the cheapest one
// and deliberately not added. The rule is that a declaration and its
// enforcement land in one change; these are the two halves of it.

/// A UDP port that is bound and answers nothing. The socket is returned so
/// that the caller keeps it alive for the length of the test.
///
/// **Held rather than merely picked, and on this kernel that makes no
/// measurable difference** — which is worth writing down, because the
/// obvious justification for holding it is not one this suite can produce.
/// The argument would be that a datagram to a port nobody holds draws an
/// ICMP port-unreachable, which quinn could turn into a prompt connection
/// error, making an "unused port" fixture measure an error path rather than
/// a silence. Mutated and run: dropping the socket and keeping the address
/// leaves all three tests below green on this Linux runner, so nothing here
/// is delivering that ICMP to quinn.
///
/// The socket stays anyway, as a portability precaution named as such. The
/// arm that would break if some platform *did* deliver it is the control,
/// [`without_the_bound_the_same_handshake_is_still_going`] — a prompt error
/// there turns a control into a flake, on a runner nobody is watching. Two
/// lines is a cheap way not to find that out on macOS.
fn black_hole() -> (std::net::UdpSocket, std::net::SocketAddr) {
    let sock = std::net::UdpSocket::bind("127.0.0.1:0").expect("bind");
    let addr = sock.local_addr().expect("local_addr");
    (sock, addr)
}

/// How long the bounded arm is given, and the unit the control's window is
/// a multiple of.
const CONNECT_BOUND: std::time::Duration = std::time::Duration::from_millis(300);

#[tokio::test(flavor = "multi_thread")]
async fn a_connect_timeout_cuts_a_quic_handshake_that_never_completes() {
    let (_hole, addr) = black_hole();
    // A certificate with no server behind it: nothing here gets far enough
    // to check one, and starting a server would give the handshake
    // something to complete against.
    let id = server::identity();
    let t = h3(&id.cert_der);

    let mut req = get(addr, "/x");
    req.extensions_mut().insert(hclient_core::Timeouts {
        resolve: None,
        connect: Some(CONNECT_BOUND),
        ..Default::default()
    });

    let started = std::time::Instant::now();
    let err = t
        .execute(req)
        .await
        .expect_err("nothing answers on that port");
    let elapsed = started.elapsed();

    assert_eq!(
        *err.kind(),
        hclient_core::ErrorKind::Timeout(hclient_core::Phase::Connect),
        "the phase has to be readable without parsing a message: {err}"
    );
    assert_eq!(
        std::error::Error::source(&err)
            .and_then(|s| s.downcast_ref::<hclient_h3::ConnectTimedOut>()),
        Some(&hclient_h3::ConnectTimedOut(CONNECT_BOUND)),
        "and the bound that was in force has to be readable off the source rather than \
         reconstructed from the caller's own copy: {err}"
    );
    assert!(
        elapsed < CONNECT_BOUND * 4,
        "the bound must be what ended this, not quinn's own idle timeout: {elapsed:?}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn without_the_bound_the_same_handshake_is_still_going() {
    // The control, and the half that gives the test above its meaning: the
    // same black hole, the same transport, one difference — no `Timeouts`
    // in the extensions. Without it, "it failed after 300 ms" would equally
    // be what a handshake that fails on its own in 300 ms looks like, and
    // the bound would be measuring nothing.
    //
    // Four times the bound, so the two windows cannot overlap on a loaded
    // runner. The wait is real time in the suite, and it is what a control
    // costs here.
    let (_hole, addr) = black_hole();
    let id = server::identity();
    let t = h3(&id.cert_der);

    let outcome = tokio::time::timeout(CONNECT_BOUND * 4, t.execute(get(addr, "/x"))).await;
    assert!(
        outcome.is_err(),
        "an unbounded handshake against a black hole must still be waiting when the bounded \
         one has already given up; it finished instead"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_client_may_now_set_a_connect_timeout_over_h3() {
    // The caller-visible half, and the reason the capability had to move in
    // the same change: `check_timeouts_supported` refuses a `connect`
    // timeout at `build()` against a transport declaring
    // `timeouts.connect == false`, so before this commit these lines were
    // an `UnsupportedCapability` rather than a timeout.
    let (_hole, addr) = black_hole();
    let id = server::identity();
    let client = hclient::Client::builder(h3(&id.cert_der))
        .timeouts(hclient::Timeouts {
            resolve: None,
            connect: Some(CONNECT_BOUND),
            ..Default::default()
        })
        .build()
        .expect("the capability is declared, so the builder must accept the setting");

    let err = client
        .get(&format!("https://{addr}/x"))
        .send()
        .await
        .expect_err("nothing answers on that port");
    assert_eq!(
        *err.kind(),
        hclient_core::ErrorKind::Timeout(hclient_core::Phase::Connect),
        "{err}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn h3_declares_the_timeouts_it_enforces_and_no_others() {
    // The declaration pinned beside the measurement, the shape
    // `capabilities_describe_the_implementation_not_the_protocol` above
    // uses for `full_duplex`: turning `connect` back off would put the two
    // tests above out of a `Client`'s reach and otherwise leave a green
    // suite, and turning either of the other two on would claim a bound
    // nothing in `execute` applies.
    let id = server::identity();
    let t = h3(&id.cert_der);
    let c = t.capabilities().timeouts;
    assert!(c.connect, "enforced in `execute`, measured above");
    assert!(
        !c.first_byte,
        "nothing bounds `one_attempt`; declaring this would be a silent no-op"
    );
    assert!(
        !c.between_bytes,
        "`H3Body` holds no sleep; declaring this would be a silent no-op"
    );
}
