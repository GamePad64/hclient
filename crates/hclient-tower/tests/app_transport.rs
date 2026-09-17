//! A server tested in process: the real `hclient::Client` against a real
//! `tower::Service`, with no socket.
//!
//! The service here is a `service_fn` rather than an `axum::Router`, so
//! this crate's dev-dependencies stay small — but the shape is the same,
//! and `axum_router.rs` beside this file proves it against axum itself.

use std::sync::{Arc, Mutex};

use hclient_core::body::RequestBody;
use hclient_tower::app::{AppTransport, OutgoingBody};
use http_body_util::BodyExt as _;

type Body = http_body_util::Full<bytes::Bytes>;

fn body(s: &str) -> Body {
    http_body_util::Full::new(bytes::Bytes::from(s.to_owned()))
}

/// Records what the service was asked, so a test can assert on the hops
/// the client made rather than only on what came back.
#[derive(Clone, Default)]
struct Seen(Arc<Mutex<Vec<(http::Method, String)>>>);

fn app(
    seen: Seen,
) -> impl tower_service::Service<
    http::Request<OutgoingBody>,
    Response = http::Response<Body>,
    Error = std::convert::Infallible,
    Future: Send,
> + Clone
+ Send
+ Sync
+ 'static {
    tower::service_fn(move |req: http::Request<OutgoingBody>| {
        let seen = seen.clone();
        async move {
            let path = req.uri().path().to_owned();
            seen.0
                .lock()
                .unwrap()
                .push((req.method().clone(), path.clone()));
            let resp = match path.as_str() {
                "/one" => http::Response::builder()
                    .status(302)
                    .header("location", "/two")
                    .header("set-cookie", "sid=abc; Path=/")
                    .body(body(""))
                    .unwrap(),
                "/two" => {
                    let cookie = req
                        .headers()
                        .get(http::header::COOKIE)
                        .and_then(|v| v.to_str().ok())
                        .unwrap_or("<none>")
                        .to_owned();
                    http::Response::new(body(&format!("arrived with {cookie}")))
                }
                "/echo" => {
                    let bytes = req.into_body().collect().await.unwrap().to_bytes();
                    http::Response::new(body(&String::from_utf8_lossy(&bytes)))
                }
                _ => http::Response::builder()
                    .status(404)
                    .body(body(""))
                    .unwrap(),
            };
            Ok::<_, std::convert::Infallible>(resp)
        }
    })
}

fn rt() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
}

/// **The whole point.** One `send()` walks a redirect, stores a cookie and
/// presents it on the next hop — client behaviour, over a service, with
/// nothing listening on any port.
#[test]
fn the_real_client_walks_a_redirect_and_carries_a_cookie_with_no_socket() {
    let seen = Seen::default();
    let client = hclient::Client::builder(AppTransport::new("testserver", app(seen.clone())))
        .cookie_jar(hclient::cookie::CookieJar::new())
        .build()
        .expect("supported");

    let text = rt()
        .block_on(async {
            client
                .get("http://testserver/one")
                .send()
                .await?
                .collect()
                .await?
                .text()
        })
        .expect("the second hop's body");

    assert_eq!(text, "arrived with sid=abc", "the jar ran between the hops");
    assert_eq!(
        *seen.0.lock().unwrap(),
        vec![
            (http::Method::GET, "/one".to_owned()),
            (http::Method::GET, "/two".to_owned())
        ],
        "one call, two hops, both seen by the service"
    );
}

/// A request body reaches the service — which is what `OutgoingBody` is
/// for, and the half a response-only test would not touch.
#[test]
fn a_request_body_reaches_the_service() {
    let client = hclient::Client::builder(AppTransport::new("testserver", app(Seen::default())))
        .build()
        .expect("supported");

    let echoed = rt()
        .block_on(async {
            client
                .post("http://testserver/echo")
                .body(RequestBody::Full(bytes::Bytes::from_static(b"payload")))
                .send()
                .await?
                .collect()
                .await?
                .text()
        })
        .expect("the echo");

    assert_eq!(echoed, "payload");
}

/// **Every capability this transport claims is the conservative one**, and
/// the doc comment on `AppTransport::new` says so field by field: no TLS
/// handshake happens, no proxy is consulted, no connection is reused
/// because none exists, and a `tower::Service` takes the whole request
/// before it answers, so there is no full duplex.
///
/// **This test cannot be killed by mutating `capabilities()`, and that is
/// recorded rather than hidden.** `AppTransport::new` stores
/// `Capabilities::default()` and there is no setter, so the sweep's
/// `Box::leak(Box::new(Default::default()))` computes the same value for
/// every reachable instance — measured, by formatting both and comparing.
/// It is an equivalent mutant, and no assertion can discriminate it. What
/// this pins instead is the *claim*: the day somebody gives this transport
/// a capability it cannot keep — `full_duplex` above all, which a caller
/// streaming a body would deadlock on — the line fails here rather than in
/// whoever believed it.
#[test]
fn the_app_transport_claims_nothing_it_cannot_keep() {
    let t = AppTransport::new("testserver", app(Seen::default()));
    let caps = hclient_core::transport::Transport::capabilities(&t);

    assert!(
        !caps.full_duplex,
        "a `tower::Service` takes the whole request and then answers"
    );
    assert!(!caps.streaming_request_body);
    assert!(!caps.connection_reuse, "there is no connection to reuse");
    assert!(!caps.proxy, "no proxy is consulted in process");
    assert!(!caps.owns_cookie_jar, "the client's jar is the only one");
    assert!(!caps.owns_cache);
    assert_eq!(
        caps.redirects,
        hclient_core::caps::RedirectSupport::None,
        "the service answers one request; the client follows the chain"
    );
    assert_eq!(
        caps.tls_config,
        hclient_core::caps::TlsSupport::None,
        "no handshake happens at all"
    );

    // The control: this is the conservative default rather than a set of
    // claims that happens to read as one, which is what makes the absence
    // of a setter on `AppTransport` the property being relied on.
    assert_eq!(
        format!("{caps:?}"),
        format!("{:?}", hclient_core::caps::Capabilities::default())
    );
}

/// **A test that names a real host is refused, not served.** Without
/// this, a URL typo would be answered by the local router and the test
/// would pass while reaching nothing.
#[test]
fn a_request_for_another_authority_is_refused_by_name() {
    let seen = Seen::default();
    let client = hclient::Client::builder(AppTransport::new("testserver", app(seen.clone())))
        .build()
        .expect("supported");

    let err = rt()
        .block_on(async { client.get("http://example.com/one").send().await })
        .expect_err("must not be served");

    let msg = format!("{err:#}");
    assert!(
        msg.contains("example.com"),
        "it names what was asked: {msg}"
    );
    assert!(msg.contains("testserver"), "and what it serves: {msg}");
    assert!(
        seen.0.lock().unwrap().is_empty(),
        "and the service never saw it"
    );
}
