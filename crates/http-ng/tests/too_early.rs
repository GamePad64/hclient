//! `425 Too Early` — RFC 8470 §5.2 — from outside the client.
//!
//! The third of 0-RTT's three failure paths, and the only one that is a
//! response status rather than a transport event: the server accepted the
//! early data, declined to risk processing it, and asked for the request
//! again outside early data. `docs/h3-research.md` §3.5 has the table of
//! all three; the other two are the transport's, this one is
//! `Client::run`'s, because the decision to repeat belongs to whoever owns
//! the operation.
//!
//! **Everything that can be observed from a socket is observed from one.**
//! The headline tests run against a real `std::net::TcpListener` speaking
//! HTTP/1.1 by hand, and assert on what the server *received* — two
//! requests, byte for byte the same — rather than on a branch having been
//! taken. A test that only proved the branch exists would stay green for a
//! client that replayed an empty body, replayed for ever, or replayed on a
//! budget of its own.
//!
//! The mock half at the bottom covers the axis a loopback server cannot
//! reach honestly: `RequestBody::retry_kind()`, whose `Impossible` case is
//! a property of the caller's body and never of the wire.
//!
//! The whole-file gate is the one `deadline.rs` and `two_runtimes.rs`
//! carry, for the same reason: on `wasm32-*` there is no `TcpListener` and
//! the native dev-dependencies below do not build.
#![cfg(not(target_family = "wasm"))]

use http_ng::{Client, ErrorKind, Phase, RequestBody};
use http_ng_dns_system::SystemDns;
use http_ng_native::Native;
use http_ng_rt_tokio::Tokio;
use http_ng_tls_rustls::Rustls;
use std::io::{Read, Write};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

type NativeTransport = Native<Tokio, Rustls, SystemDns<Tokio>>;

fn transport() -> NativeTransport {
    // `with_webpki_roots`, like `deadline.rs`: no handshake happens on
    // these plain-HTTP servers, but `Native::new` still needs a concrete
    // `TlsConnect`, and this one does not touch the system trust store.
    Native::new(Tokio, Rustls::with_webpki_roots(), SystemDns::new(Tokio))
}

fn rt() -> tokio::runtime::Runtime {
    tokio::runtime::Runtime::new().expect("tokio runtime")
}

/// How long a test waits before calling a replay loop unbounded.
///
/// `a_server_that_answers_425_for_ever_gets_exactly_two_requests` cannot
/// fail without this — an unbounded replay does not return a wrong answer,
/// it returns no answer, and a hung suite is a worse signal than a red one
/// (the same reasoning, and the same shape, as `deadline.rs`'s `GUARD`).
const GUARD: Duration = Duration::from_secs(6);

/// The guard, applied — named so the panic says which wait never ended.
async fn guarded<F: std::future::Future>(what: &str, f: F) -> F::Output {
    match tokio::time::timeout(GUARD, f).await {
        Ok(v) => v,
        Err(_) => panic!("{what}: nothing ended this within {GUARD:?}"),
    }
}

const R425: &str = "HTTP/1.1 425 Too Early\r\nContent-Length: 0\r\n\r\n";
const R200: &str = "HTTP/1.1 200 OK\r\nContent-Length: 4\r\n\r\ndone";

/// A server that answers the n-th request it receives with `script[n]`,
/// the last entry repeating for ever, after `delay`.
///
/// One thread per connection, so a client that opens a second connection
/// for the replay (rather than reusing the pooled one) is served rather
/// than deadlocked against a single-threaded accept loop — the test must
/// not depend on which of the two the pool chooses.
///
/// The `Vec<String>` it hands back is the whole of every request as it
/// arrived on the wire, head and body: that is what makes "the client sent
/// the same request again" an assertion about bytes rather than about
/// `Client`'s own view of itself.
fn scripted_server(
    script: Vec<&'static str>,
    delay: Duration,
) -> (std::net::SocketAddr, Arc<Mutex<Vec<String>>>) {
    let l = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = l.local_addr().expect("addr");
    let seen: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let recorder = Arc::clone(&seen);
    std::thread::spawn(move || {
        for s in l.incoming() {
            let Ok(mut s) = s else { continue };
            let recorder = Arc::clone(&recorder);
            let script = script.clone();
            std::thread::spawn(move || {
                let mut buf: Vec<u8> = Vec::new();
                while let Some(req) = read_one_request(&mut s, &mut buf) {
                    let n = {
                        let mut g = recorder.lock().expect("recorder lock");
                        g.push(req);
                        g.len()
                    };
                    std::thread::sleep(delay);
                    let answer = script[(n - 1).min(script.len() - 1)];
                    if s.write_all(answer.as_bytes()).is_err() || s.flush().is_err() {
                        break;
                    }
                }
            });
        }
    });
    (addr, seen)
}

/// One whole request off the socket, leftovers kept in `buf` so a second
/// request on the same (pooled) connection is read as a request and not as
/// the tail of the first.
fn read_one_request(s: &mut std::net::TcpStream, buf: &mut Vec<u8>) -> Option<String> {
    let head_end = loop {
        if let Some(i) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            break i + 4;
        }
        let mut chunk = [0u8; 1024];
        match s.read(&mut chunk) {
            Ok(0) | Err(_) => return None,
            Ok(n) => buf.extend_from_slice(&chunk[..n]),
        }
    };
    let head = String::from_utf8_lossy(&buf[..head_end]).into_owned();
    let len = content_length(&head);
    while buf.len() < head_end + len {
        let mut chunk = [0u8; 1024];
        match s.read(&mut chunk) {
            Ok(0) | Err(_) => return None,
            Ok(n) => buf.extend_from_slice(&chunk[..n]),
        }
    }
    let whole = String::from_utf8_lossy(&buf[..head_end + len]).into_owned();
    buf.drain(..head_end + len);
    Some(whole)
}

fn content_length(head: &str) -> usize {
    head.lines()
        .find_map(|l| {
            let (name, value) = l.split_once(':')?;
            name.trim()
                .eq_ignore_ascii_case("content-length")
                .then(|| value.trim().parse().ok())?
        })
        .unwrap_or(0)
}

/// A `POST` with a body, so a replay that lost the payload is visible in
/// the recording rather than only in the status.
///
/// Generic over the clock, and it has to be: the budget test's client
/// carries `Tokio`, the others carry `DefaultClock` — which is `Tokio` in
/// an `--all-features` build and `NoClock` in a `--no-default-features`
/// one, so a signature naming either concretely compiles in exactly one of
/// the two builds `just test`/`just test-no-default` run.
fn post_payload<'c, Tm: http_ng_core::unversioned::Timer + Clone>(
    c: &'c Client<NativeTransport, Tm>,
    url: &str,
) -> http_ng::RequestBuilder<'c, NativeTransport, Tm> {
    c.post(url)
        .body(RequestBody::Full(bytes::Bytes::from_static(b"payload")))
}

// =====================================================================
// The two halves RFC 8470 §5.2 actually asks for: retry, and stop.
// =====================================================================

/// The server answers `425` once and `200` next, and the caller sees the
/// `200` — having sent the same request twice.
///
/// Three assertions, and the first two are the ones that matter: the
/// server *received* a second request, and that request is byte for byte
/// the first one. A replay that dropped the body, rewrote the method or
/// lost a header would satisfy "the caller got a 200" just as well, since
/// this server answers whatever arrives second.
#[test]
fn a_425_is_replayed_once_and_the_second_answer_is_the_one_returned() {
    let (addr, seen) = scripted_server(vec![R425, R200], Duration::ZERO);
    let c = Client::builder(transport()).build().expect("client");

    let (status, text) = rt().block_on(guarded("a 425 then a 200", async {
        let r = post_payload(&c, &format!("http://{addr}/t"))
            .send()
            .await
            .expect("send");
        let status = r.status();
        let text = r.collect().await.expect("collect").text().expect("text");
        (status, text)
    }));

    assert_eq!(status, 200, "the replay's answer is the caller's answer");
    assert_eq!(text, "done");

    let seen = seen.lock().expect("recorder lock");
    assert_eq!(seen.len(), 2, "the server received the replay");
    assert!(
        seen[0].starts_with("POST /t HTTP/1.1\r\n") && seen[0].ends_with("payload"),
        "the first request is the caller's: {:?}",
        seen[0]
    );
    assert_eq!(
        seen[0], seen[1],
        "the replay is the same request, byte for byte"
    );
}

/// The control, and the reason the replay is written without a loop: a
/// server wedged on `425` must end the operation, not the process.
///
/// **Without a bound on the replay this test does not go red — it never
/// returns**, which is why it carries `GUARD` and why the assertion on the
/// request count is `2` rather than "more than one".
#[test]
fn a_server_that_answers_425_for_ever_gets_exactly_two_requests() {
    let (addr, seen) = scripted_server(vec![R425], Duration::ZERO);
    let c = Client::builder(transport()).build().expect("client");

    let status = rt().block_on(guarded("a server wedged on 425", async {
        post_payload(&c, &format!("http://{addr}/t"))
            .send()
            .await
            .expect("send")
            .status()
    }));

    assert_eq!(
        status, 425,
        "the second 425 is the server's answer, and it is handed over as one"
    );
    assert_eq!(
        seen.lock().expect("recorder lock").len(),
        2,
        "one replay, not none and not a stream of them"
    );
}

// =====================================================================
// The budget. The replay is inside `Client::run`, which `Client::execute`
// wraps in `within(..)` after reading the clock once.
// =====================================================================

/// Each answer costs `ANSWER`, and the bound is set between one of them
/// and two: enough for the `425` and the replay's flight, not enough for
/// the replay's answer.
const ANSWER: Duration = Duration::from_millis(400);
const TOTAL: Duration = Duration::from_millis(600);

/// A replay that restarted the clock would return a `200` here. It has to
/// time out instead.
///
/// This one test refuses three different mutations, which is why the
/// assertions look redundant and are not:
///
/// - **the replay moved outside `within(..)`** (retried around
///   `Client::execute`, or by a caller): the second attempt would get a
///   fresh 600 ms, its answer would arrive at 800 ms, and `expect_err`
///   would panic on an `Ok`;
/// - **no replay at all**: the `425` would come back as a perfectly good
///   `Ok` at 400 ms, and `expect_err` would panic on that instead;
/// - **a replay that happens but is not waited for**: `seen.len() == 2`
///   says the second request reached the server, so the timeout is the
///   budget running out mid-flight and not the replay never flying.
#[test]
fn the_replay_spends_what_is_left_of_the_total_rather_than_a_fresh_one() {
    let (addr, seen) = scripted_server(vec![R425, R200], ANSWER);
    let c = Client::builder(transport())
        .total_timeout(Tokio, TOTAL)
        .build()
        .expect("client");

    let (err, elapsed) = rt().block_on(guarded("a 425 under a total timeout", async {
        let started = Instant::now();
        let err = post_payload(&c, &format!("http://{addr}/t"))
            .send()
            .await
            .expect_err("the budget ran out during the replay");
        (err, started.elapsed())
    }));

    assert!(
        matches!(err.kind(), ErrorKind::Timeout(Phase::Total)),
        "the operation's own bound fired, not something else: {err:?}"
    );
    assert!(
        elapsed < ANSWER * 2,
        "the replay restarted the clock: {elapsed:?} is past both answers"
    );
    assert!(
        elapsed >= TOTAL - Duration::from_millis(50),
        "it gave up before the bound: {elapsed:?}"
    );
    assert_eq!(
        seen.lock().expect("recorder lock").len(),
        2,
        "the replay did reach the server, and then the budget ended it"
    );
}

// =====================================================================
// The jar, which learns from a `425` without rewriting the replay.
// =====================================================================

/// A `Set-Cookie` on the `425` is stored — and the replay still goes out
/// as the request the server asked to have repeated, without it.
///
/// Both halves are the decision, not an accident of ordering.
/// `attach_cookies` runs once per hop, before the first attempt, so the two
/// attempts of one hop carry the same headers; the cookie a `425` hands
/// back reaches the next HOP, where the jar's per-hop rule derives it
/// fresh. The alternative — re-deriving between two attempts of one hop —
/// would make the replay a request the server never asked for, which is
/// the opposite of what a retry is.
#[cfg(feature = "cookies")]
#[test]
fn a_425_teaches_the_jar_without_rewriting_the_replay() {
    const R425_COOKIE: &str =
        "HTTP/1.1 425 Too Early\r\nSet-Cookie: k=v; Path=/\r\nContent-Length: 0\r\n\r\n";

    let (addr, seen) = scripted_server(vec![R425_COOKIE, R200], Duration::ZERO);
    let c = Client::builder(transport())
        .cookie_jar(http_ng::cookie::CookieJar::new())
        .build()
        .expect("client");
    let url = format!("http://{addr}/t");

    let status = rt().block_on(guarded("a 425 carrying a cookie", async {
        post_payload(&c, &url).send().await.expect("send").status()
    }));
    assert_eq!(status, 200);

    let seen = seen.lock().expect("recorder lock");
    assert_eq!(seen.len(), 2);
    assert!(
        !seen[1].to_ascii_lowercase().contains("\r\ncookie:"),
        "the replay is the same request, not a new one built from the 425's answer: {:?}",
        seen[1]
    );
    assert_eq!(
        c.cookies()
            .expect("this client has a jar")
            .cookie_header(&url.parse().expect("uri"), std::time::SystemTime::now())
            .map(|v| v.to_str().expect("ascii").to_owned()),
        Some("k=v".to_owned()),
        "the jar learned from the 425 all the same"
    );
}

// =====================================================================
// `RequestBody::retry_kind()` — the axis a loopback server cannot reach,
// because a single-pass body is a property of the caller's body and not
// of the wire.
// =====================================================================

#[cfg(feature = "test-util")]
mod replayability {
    use http_ng::mock::MockTransport;
    use http_ng::{Client, RequestBody, RetryKind};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn too_early() -> http::Response<&'static str> {
        http::Response::builder().status(425).body("").unwrap()
    }
    fn ok() -> http::Response<&'static str> {
        http::Response::builder().status(200).body("done").unwrap()
    }

    /// A single-pass body cannot be sent twice, so the `425` is what the
    /// caller gets — and no second request is invented to replace it.
    ///
    /// The mutation this exists for is not "the replay stops happening"
    /// (deleting the `retry_kind` gate leaves `rewind()` answering `None`
    /// for the same body — the same three-way split spelled differently).
    /// It is **the replay happening anyway, with whatever body is to
    /// hand** — `replay.unwrap_or_default()`, an empty POST to a server
    /// that asked for the real request again. That mutant sends a second
    /// request here and gets the queued `200`, and both assertions below
    /// fail.
    #[test]
    fn a_single_pass_body_is_not_replayed_and_the_425_reaches_the_caller() {
        struct OneShot(Option<bytes::Bytes>);
        impl http_body::Body for OneShot {
            type Data = bytes::Bytes;
            type Error = http_ng_core::Error;
            fn poll_frame(
                mut self: std::pin::Pin<&mut Self>,
                _: &mut std::task::Context<'_>,
            ) -> std::task::Poll<Option<Result<http_body::Frame<bytes::Bytes>, Self::Error>>>
            {
                std::task::Poll::Ready(self.0.take().map(|b| Ok(http_body::Frame::data(b))))
            }
        }

        let m = MockTransport::new();
        m.push_response(too_early());
        m.push_response(ok());

        let c = Client::builder(m).build().expect("client");
        let req = http::Request::builder()
            .method("POST")
            .uri("https://a/x")
            .body(RequestBody::Streaming(Box::new(OneShot(Some(
                bytes::Bytes::from_static(b"payload"),
            )))))
            .unwrap();
        let resp = futures_executor::block_on(c.execute(req)).expect("execute");

        assert_eq!(resp.status(), 425, "the server's answer, handed over as-is");
        assert_eq!(
            c.transport().requests().len(),
            1,
            "no second request was invented for a body that cannot be replayed"
        );
    }

    /// `ViaFactory` replays, and the factory is called **once for the
    /// hop**, not once per attempt: the snapshot taken before the first
    /// attempt is what the replay is built from.
    ///
    /// Worth pinning rather than leaving to the eye, because it is what
    /// makes the second attempt provably the same body as the first — a
    /// factory called twice could, in violation of its own contract,
    /// produce something else.
    #[test]
    fn a_rewindable_body_is_replayed_from_the_snapshot_taken_before_the_first_attempt() {
        let calls = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&calls);

        let m = MockTransport::new();
        m.push_response(too_early());
        m.push_response(ok());

        let c = Client::builder(m).build().expect("client");
        let req = http::Request::builder()
            .method("POST")
            .uri("https://a/x")
            .body(RequestBody::rewindable(move || {
                counter.fetch_add(1, Ordering::SeqCst);
                RequestBody::Full(bytes::Bytes::from_static(b"payload"))
            }))
            .unwrap();
        let resp = futures_executor::block_on(c.execute(req)).expect("execute");

        assert_eq!(resp.status(), 200);
        let seen = c.transport().requests();
        assert_eq!(seen.len(), 2, "the replay went out");
        assert_eq!(
            seen[0].retry_kind,
            RetryKind::ViaFactory,
            "the caller's own body"
        );
        assert_eq!(
            (seen[1].retry_kind, seen[1].body_size_hint),
            (RetryKind::Free, Some(7)),
            "the replay carries what the factory produced, all seven bytes of it"
        );
        assert_eq!(
            calls.load(Ordering::SeqCst),
            1,
            "one snapshot per hop, not one per attempt"
        );
    }

    /// The replay is per hop, not per operation: a `425` on the second hop
    /// of a redirect chain is replayed even though the first hop's replay
    /// is already spent.
    ///
    /// The count is what pins the policy — four requests, not three (a
    /// per-operation budget spent by hop 1) and not two.
    #[test]
    fn each_redirect_hop_carries_its_own_replay() {
        fn to(loc: &'static str) -> http::Response<&'static str> {
            http::Response::builder()
                .status(302)
                .header("location", loc)
                .body("")
                .unwrap()
        }

        let m = MockTransport::new();
        m.push_response(too_early()); // hop 1
        m.push_response(to("https://a/second")); // hop 1, replayed
        m.push_response(too_early()); // hop 2
        m.push_response(ok()); // hop 2, replayed

        let c = Client::builder(m).build().expect("client");
        let req = http::Request::builder()
            .uri("https://a/first")
            .body(RequestBody::Empty)
            .unwrap();
        let resp = futures_executor::block_on(c.execute(req)).expect("execute");

        assert_eq!(resp.status(), 200);
        let seen = c.transport().requests();
        assert_eq!(seen.len(), 4, "two hops, each replayed once");
        let uris: Vec<String> = seen.iter().map(|r| r.uri.to_string()).collect();
        assert_eq!(
            uris,
            vec![
                "https://a/first",
                "https://a/first",
                "https://a/second",
                "https://a/second"
            ]
        );
    }

    /// RFC 8470 §5.2: the retry MUST NOT itself be sent in early data.
    ///
    /// The duty was owed vacuously when the replay was written — no
    /// transport here could offer early data — and stopped being vacuous
    /// the moment `http-ng-h3` landed. The tempting argument that it is
    /// still vacuous ("the handshake finished long ago, so later streams
    /// are 1-RTT anyway") holds only of the connection that happens to
    /// still be pooled: `AllowEarlyData` is part of h3's pool key, so a
    /// marked replay asks for the early-data connection *specifically*,
    /// and a fresh one goes into early data again.
    ///
    /// The observer is what the transport was handed, not what the client
    /// believes it sent.
    #[test]
    fn the_replay_is_not_marked_for_early_data_even_though_the_first_attempt_was() {
        let m = MockTransport::new();
        m.push_response(too_early());
        m.push_response(ok());

        let c = Client::builder(m).build().expect("client");
        let mut req = http::Request::builder()
            .method("POST")
            .uri("https://a/x")
            .body(RequestBody::Full(bytes::Bytes::from_static(b"payload")))
            .unwrap();
        req.extensions_mut().insert(http_ng_core::AllowEarlyData);

        let resp = futures_executor::block_on(c.execute(req)).expect("execute");
        assert_eq!(resp.status(), 200);

        let seen = c.transport().requests();
        assert_eq!(seen.len(), 2, "the replay went out");
        assert!(
            seen[0]
                .extensions
                .get::<http_ng_core::AllowEarlyData>()
                .is_some(),
            "the caller's mark must survive to the first attempt, or this test \
             would pass against a client that strips it everywhere"
        );
        assert!(
            seen[1]
                .extensions
                .get::<http_ng_core::AllowEarlyData>()
                .is_none(),
            "the 425 replay went out still marked for early data — against the \
             very server that just refused to risk one"
        );
    }

    /// The strip is on a clone of the hop, so the mark reaches the next
    /// redirect hop — and that is a decision, not a detail of where the
    /// line sits.
    ///
    /// A redirect after a `425` is a **different request**, which the
    /// caller marked too. Withdrawing the opt-in for the rest of the chain
    /// would be a silent downgrade to 1-RTT that nothing announces, and it
    /// would buy nothing that is not already bought: if the next hop's
    /// origin is also unwilling, it answers `425` and that hop gets its own
    /// replay. The cost of keeping the mark is bounded and self-correcting;
    /// the cost of dropping it is invisible.
    ///
    /// Stripping `hp` itself instead of a clone passes the test above and
    /// fails this one, which is the only thing that tells the two apart.
    #[test]
    fn the_hop_after_a_replayed_425_still_carries_the_mark() {
        let m = MockTransport::new();
        m.push_response(too_early()); // hop 1
        m.push_response(
            http::Response::builder() // hop 1, replayed
                .status(302)
                .header("location", "https://a/second")
                .body("")
                .unwrap(),
        );
        m.push_response(ok()); // hop 2

        let c = Client::builder(m).build().expect("client");
        let mut req = http::Request::builder()
            .method("POST")
            .uri("https://a/first")
            .body(RequestBody::Full(bytes::Bytes::from_static(b"payload")))
            .unwrap();
        req.extensions_mut().insert(http_ng_core::AllowEarlyData);

        let resp = futures_executor::block_on(c.execute(req)).expect("execute");
        assert_eq!(resp.status(), 200);

        assert_eq!(marks(&c), vec![true, false, true], "{}", MARKS);
    }

    /// And with no `425` anywhere, the mark survives every hop untouched.
    ///
    /// This is the control that keeps the other two honest, and it needs to
    /// span a redirect rather than a single request: a strip moved one line
    /// out of the `425` branch — into the loop body, after the send —
    /// passes every other assertion in this file, because nowhere else does
    /// the mark have to survive a hop boundary. What it would take away is
    /// not safety but the caller's opt-in, silently, from the second hop
    /// on. (Measured, not imagined: that mutant survived the first pass of
    /// this module.)
    #[test]
    fn a_redirect_chain_without_a_425_keeps_the_mark_on_every_hop() {
        let m = MockTransport::new();
        m.push_response(
            http::Response::builder()
                .status(302)
                .header("location", "https://a/second")
                .body("")
                .unwrap(),
        );
        m.push_response(ok());

        let c = Client::builder(m).build().expect("client");
        let mut req = http::Request::builder()
            .uri("https://a/first")
            .body(RequestBody::Empty)
            .unwrap();
        req.extensions_mut().insert(http_ng_core::AllowEarlyData);

        let resp = futures_executor::block_on(c.execute(req)).expect("execute");
        assert_eq!(resp.status(), 200);

        assert_eq!(marks(&c), vec![true, true], "{}", MARKS);
    }

    const MARKS: &str = "one bool per request the transport was handed, in order: \
                         whether it carried AllowEarlyData";

    /// The mark as the transport saw it, per request, in order — the
    /// observer being what was handed over rather than what the client
    /// believes it sent.
    fn marks(c: &Client<MockTransport>) -> Vec<bool> {
        c.transport()
            .requests()
            .iter()
            .map(|r| r.extensions.get::<http_ng_core::AllowEarlyData>().is_some())
            .collect()
    }
}
