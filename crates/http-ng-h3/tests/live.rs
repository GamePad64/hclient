//! HTTP/3 over real QUIC on loopback.
//!
//! Every test here **runs**. The research measured a full client-plus-server
//! h3 exchange at `wall=2.9ms` on this shape of setup with an `rcgen`
//! certificate and two unprivileged `UdpSocket::bind("127.0.0.1:0")` calls,
//! so a compile-only claim would have been a choice rather than a
//! constraint.
#![cfg(not(target_family = "wasm"))]

mod server;

use http_body_util::BodyExt;
use http_ng_core::unversioned::Transport;
use http_ng_core::{AllowEarlyData, EarlyDataSupport, RequestBody};
use http_ng_dns::IpLiteralOnly;
use http_ng_h3::H3;
use http_ng_rt_tokio::TokioHandle;
use server::Behaviour;

/// The transport under test, over the real runtime seam.
fn h3(
    cert: &rustls::pki_types::CertificateDer<'static>,
) -> H3<TokioHandle, http_ng_tls_rustls::Rustls, IpLiteralOnly> {
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
    // The whole stack: our `quinn::Runtime` over `http_ng_rt`'s `Timer`,
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
    // also wrote — the rule `http-ng-native`'s pool test already follows.
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
    // `http-ng-native`'s h2 one, where a connection is checked out
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
    // The A/B the research ran (`docs/h3-research.md` §1.5), with one
    // correction it did not have: there, the variable was whether anything
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
    // are neighbours on the connection. In `http-ng-native`'s h2 the same
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
async fn a_425_reaches_the_caller_untouched() {
    // RFC 8470 §5.2's third failure path, and the one nothing in this
    // workspace handles. The test exists to pin that it is NOT handled: a
    // `425` arrives as an ordinary response rather than being retried, and
    // the day someone implements the retry this test is what tells them
    // they changed an observable behaviour.
    let s = server::start(Behaviour::TooEarly);
    let t = h3(&s.cert_der);
    let r = t.execute(get(s.addr, "/early")).await.expect("a response");
    assert_eq!(
        r.status(),
        http::StatusCode::TOO_EARLY,
        "425 is not retried anywhere; it reaches the caller"
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
    // two halves are independent. `execute` does neither — it writes the
    // whole request body, then reads the head — and these two `false`s are
    // therefore about this code and not about the protocol.
    //
    // The one that matters is `full_duplex`: over-claiming it costs a
    // caller a deadlock rather than a degradation, which is the argument
    // W3's floor rule is built on. Pinned here so that implementing duplex
    // and forgetting to move the declaration is a red test, and so that
    // moving the declaration without implementing it is one too.
    let s = server::start(Behaviour::Echo);
    let t = h3(&s.cert_der);
    let c = t.capabilities();
    assert!(
        !c.full_duplex,
        "execute writes the request body before reading the response head"
    );
    assert!(
        !c.streaming_request_body,
        "and it refuses a streaming body outright — see the test below"
    );
    // The two `true`s in the same set, so this is a description of the
    // implementation rather than a habit of saying no.
    assert!(c.response_trailers, "H3Body yields a trailers frame");
    assert_eq!(c.connection_reuse, http_ng_core::ReuseSupport::Supported);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_streaming_request_body_is_refused_by_name() {
    // `Capabilities::streaming_request_body` is `false`, and this is what
    // makes that declaration cost something: the refusal is typed and names
    // the capability, rather than the body being quietly buffered.
    let s = server::start(Behaviour::Echo);
    let t = h3(&s.cert_der);
    let body = RequestBody::Streaming(Box::new(
        http_body_util::Empty::<bytes::Bytes>::new()
            .map_err(|e: std::convert::Infallible| match e {}),
    ));
    let req = http::Request::builder()
        .uri(format!("https://{}/x", s.addr))
        .body(body)
        .unwrap();
    let err = t
        .execute(req)
        .await
        .expect_err("declared false, so refused");
    assert!(err.is_unsupported(), "{err}");
    assert!(err.to_string().contains("streaming_request_body"), "{err}");
}
