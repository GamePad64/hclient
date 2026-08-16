//! `1xx` responses reaching a hook, on both protocols.
//!
//! Every assertion is about what the hook was handed, in order — a `103`
//! whose headers arrive *after* the head it precedes would be useless, and
//! only the order shows that.

use http_ng::Client;
use http_ng_core::unversioned::{Event, Hooks, Transport};
use http_ng_dns::IpLiteralOnly;
use http_ng_native::Native;
use http_ng_rt_tokio::Tokio;
use http_ng_tls::NoTls;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener};
use std::sync::{Arc, Mutex};
use std::time::Duration;

const BOUND: Duration = Duration::from_secs(5);

#[derive(Debug, Clone, Default)]
struct Recorder(Arc<Mutex<Vec<String>>>);

impl Recorder {
    fn seen(&self) -> Vec<String> {
        self.0.lock().expect("recorder").clone()
    }
}

impl Hooks for Recorder {
    fn on(&self, event: Event<'_>) {
        let line = match event {
            Event::Informational(e) => format!(
                "1xx {} id={} link={}",
                e.status.as_u16(),
                e.id.get(),
                e.headers
                    .get("link")
                    .and_then(|v| v.to_str().ok())
                    .unwrap_or("<none>")
            ),
            Event::Head(e) => format!("head {}", e.status.as_u16()),
            Event::Connected(e) => format!("connected id={}", e.id.get()),
            Event::Reused(_) => "reused".into(),
            Event::Closed(_) => "closed".into(),
        };
        self.0.lock().expect("recorder").push(line);
    }
}

/// A server that sends `103 Early Hints`, then the response.
///
/// Written as raw bytes rather than through a server library, because the
/// thing under test is that an interim head is *not* the response — and
/// most server libraries will not let you send one at all.
fn server(interim: &'static [u8]) -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = listener.local_addr().expect("addr");
    std::thread::spawn(move || {
        for conn in listener.incoming() {
            let Ok(mut s) = conn else { break };
            let mut head = Vec::new();
            let mut byte = [0u8; 1];
            while !head.ends_with(b"\r\n\r\n") {
                match s.read(&mut byte) {
                    Ok(0) | Err(_) => break,
                    Ok(_) => head.push(byte[0]),
                }
            }
            let _ = s.write_all(interim);
            let _ = s.flush();
            let _ =
                s.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nhi");
            let _ = s.flush();
        }
    });
    addr
}

const EARLY_HINTS: &[u8] = b"HTTP/1.1 103 Early Hints\r\nLink: </s.css>; rel=preload\r\n\r\n";

/// **A `103` reaches the hook, with its headers, before the head.**
///
/// The order is the assertion. `103 Early Hints` exists to let a client
/// start fetching subresources while the origin is still thinking, so a
/// hook told about it after the response has arrived has been told
/// nothing it can act on.
#[tokio::test(flavor = "multi_thread")]
async fn a_103_reaches_the_hook_with_its_headers_and_before_the_head() {
    let addr = server(EARLY_HINTS);
    let rec = Recorder::default();
    let client = Client::builder(
        Native::new(Tokio, NoTls, IpLiteralOnly)
            .hooks(rec.clone())
            .watching_1xx(),
    )
    .build()
    .expect("build");

    let url = format!("http://127.0.0.1:{}/x", addr.port());
    let resp = tokio::time::timeout(BOUND, client.get(&url).send())
        .await
        .expect("must not hang")
        .expect("the 200 is the response");
    assert_eq!(resp.status(), 200, "a 1xx is not the response");

    let seen = rec.seen();
    let hint = seen
        .iter()
        .position(|l| l.starts_with("1xx 103"))
        .unwrap_or_else(|| panic!("no 1xx event: {seen:?}"));
    let head = seen
        .iter()
        .position(|l| l.starts_with("head"))
        .unwrap_or_else(|| panic!("no head event: {seen:?}"));
    assert!(hint < head, "the hint must precede the head: {seen:?}");
    assert!(
        seen[hint].ends_with("link=</s.css>; rel=preload"),
        "the headers travel with it: {:?}",
        seen[hint]
    );

    // **The id names the connection this arrived on**, which is what makes
    // the event usable to a caller holding several exchanges at once — and
    // was populated and unasserted until this line. `ConnectionId` is
    // monotonic and never `UNWATCHED` here, so equality with the
    // `Connected` that opened this exchange is the whole claim.
    let connected = seen
        .iter()
        .find(|l| l.starts_with("connected id="))
        .expect("a connection was opened")
        .trim_start_matches("connected ")
        .to_owned();
    assert!(
        seen[hint].contains(&connected),
        "the 1xx must name the connection it came in on: {seen:?}"
    );
}

/// **The control, and the capability agrees with it.** The same server and
/// the same hook without `watching_1xx()`: no event, and
/// `informational_1xx` is `false` — so a caller cannot be told the
/// transport reports them while it does not.
#[tokio::test(flavor = "multi_thread")]
async fn without_the_opt_in_there_is_no_event_and_the_capability_says_so() {
    let addr = server(EARLY_HINTS);
    let rec = Recorder::default();
    let transport = Native::new(Tokio, NoTls, IpLiteralOnly).hooks(rec.clone());
    assert!(!transport.capabilities().informational_1xx);

    let client = Client::builder(transport).build().expect("build");
    let url = format!("http://127.0.0.1:{}/x", addr.port());
    let resp = tokio::time::timeout(BOUND, client.get(&url).send())
        .await
        .expect("must not hang")
        .expect("the 200 still arrives");
    assert_eq!(resp.status(), 200, "a swallowed 1xx changes nothing else");

    let seen = rec.seen();
    assert!(
        !seen.iter().any(|l| l.starts_with("1xx")),
        "nothing was asked for, so nothing is reported: {seen:?}"
    );
    assert!(seen.iter().any(|l| l.starts_with("head")));
}

/// **The capability follows the opt-in**, asserted in the direction the
/// control above cannot reach.
#[tokio::test(flavor = "multi_thread")]
async fn the_capability_is_true_exactly_when_the_opt_in_was_taken() {
    let watched = Native::new(Tokio, NoTls, IpLiteralOnly)
        .hooks(Recorder::default())
        .watching_1xx();
    assert!(watched.capabilities().informational_1xx);
}

/// **A `100 Continue` is reported too, and is still not the response.**
///
/// A different status through the same path, because a client that
/// special-cased `103` would pass the tests above.
#[tokio::test(flavor = "multi_thread")]
async fn a_100_continue_is_reported_and_the_200_still_arrives() {
    let addr = server(b"HTTP/1.1 100 Continue\r\n\r\n");
    let rec = Recorder::default();
    let client = Client::builder(
        Native::new(Tokio, NoTls, IpLiteralOnly)
            .hooks(rec.clone())
            .watching_1xx(),
    )
    .build()
    .expect("build");

    let url = format!("http://127.0.0.1:{}/x", addr.port());
    let resp = tokio::time::timeout(BOUND, client.get(&url).send())
        .await
        .expect("must not hang")
        .expect("the 200 is the response");
    assert_eq!(resp.status(), 200);
    let seen = rec.seen();
    assert!(
        seen.iter()
            .any(|l| l.starts_with("1xx 100 ") && l.ends_with("link=<none>")),
        "the 100 must be reported, with no Link of its own: {seen:?}"
    );
}

/// **`.hooks(..)` must come before `.watching_1xx()`**, and the other
/// order compiles while watching nothing.
///
/// The same trap `.hooks(..)` sets for [`Native::multiplexed`] and for the
/// same reason — the stored pointer's type names `H` — so it is pinned the
/// same way: a pair of orders, one of which reports and one of which does
/// not. Asserted rather than only documented, because the compiler does
/// not catch it.
#[tokio::test(flavor = "multi_thread")]
async fn hooks_before_watching_1xx_reports_and_hooks_after_it_does_not() {
    for (reports, label) in [
        (true, "hooks then watching"),
        (false, "watching then hooks"),
    ] {
        let addr = server(EARLY_HINTS);
        let rec = Recorder::default();
        let base = Native::new(Tokio, NoTls, IpLiteralOnly);
        let transport = if reports {
            base.hooks(rec.clone()).watching_1xx()
        } else {
            base.hooks(Recorder::default())
                .watching_1xx()
                .hooks(rec.clone())
        };
        // The capability follows the pointer, so it disagrees too — which
        // is what keeps the wrong order from being a silent downgrade
        // *and* a lying capability at once.
        assert_eq!(
            transport.capabilities().informational_1xx,
            reports,
            "{label}: the capability"
        );

        let client = Client::builder(transport).build().expect("build");
        let url = format!("http://127.0.0.1:{}/x", addr.port());
        let resp = tokio::time::timeout(BOUND, client.get(&url).send())
            .await
            .expect("must not hang")
            .expect("the 200 arrives either way");
        assert_eq!(resp.status(), 200);
        assert_eq!(
            rec.seen().iter().any(|l| l.starts_with("1xx")),
            reports,
            "{label}: the event"
        );
    }
}
