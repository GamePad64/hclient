//! The cookie jar wired into `Client`: what a real server actually sees on
//! the wire, and the one configuration that is refused instead.
//!
//! `hclient-cookie` is already tested to death on its own — 95 tests, RFC
//! 6265bis §5.2/§5.4/§5.7 and a date corpus with two oracles — and none of
//! that says whether `Client` ever calls it, calls it at the right moment,
//! or calls it with the right URI. So nothing here re-tests the jar. Every
//! test below is about the wiring, and **the observer is outside the
//! client**: a loopback server recording the `Cookie` header it was sent,
//! never the jar's own view of itself. A jar that stores perfectly and
//! attaches nothing passes every assertion made in `hclient-cookie` and
//! fails the first one here.
//!
//! The gate is the exception, and it has to be: "a client-side jar against
//! a jar-owning backend is refused at `build()`" is a fact about a type
//! that never sends anything, so the mock is the only place to state it.
//!
//! The whole-file gate has three parts, and `cookies` is the interesting
//! one: `--no-default-features` builds of this crate have no jar to wire,
//! and `just test-no-default` runs exactly that.
#![cfg(all(
    feature = "cookies",
    feature = "test-util",
    not(target_family = "wasm")
))]

use hclient::Client;
use hclient::cookie::CookieJar;
use hclient::error::UnsupportedCapability;
use hclient_dns_system::SystemDns;
use hclient_native::Native;
use hclient_rt_tokio::Tokio;
use hclient_tls_rustls::Rustls;
use std::io::{Read, Write};
use std::sync::{Arc, Mutex};

type NativeTransport = Native<Tokio, Rustls, SystemDns<Tokio>>;

fn transport() -> NativeTransport {
    // `with_webpki_roots`, as everywhere else in this crate's live tests:
    // these servers speak plain HTTP, so no handshake happens, but
    // `Native::new` still needs a concrete `TlsConnect`.
    Native::new(Tokio, Rustls::with_webpki_roots(), SystemDns::new(Tokio))
}

fn rt() -> tokio::runtime::Runtime {
    tokio::runtime::Runtime::new().expect("tokio runtime")
}

/// One request as the server saw it — the only evidence any test here
/// accepts.
#[derive(Debug, Clone)]
struct Seen {
    path: String,
    cookie: Option<String>,
}

/// A server that records what it was sent and answers by a script.
///
/// **Keep-alive is handled properly** rather than by closing after each
/// response, and that is not incidental: `Native::new` pools by default
/// (v0.2 W2), so a server that hung up after every reply would have the
/// client racing a dead pooled socket and retrying — a second variable in
/// tests that are about cookies. Here the same connection carries every
/// request of a test, which is also the shape a real server has.
fn recording_server(
    respond: impl Fn(&str) -> String + Send + 'static,
) -> (std::net::SocketAddr, Arc<Mutex<Vec<Seen>>>) {
    let l = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = l.local_addr().expect("addr");
    let seen = Arc::new(Mutex::new(Vec::new()));
    let log = Arc::clone(&seen);
    std::thread::spawn(move || {
        for s in l.incoming() {
            let Ok(mut s) = s else { continue };
            loop {
                let mut buf = Vec::new();
                let mut b = [0u8; 1024];
                // Read until the head is complete. Every request these
                // tests send is a GET with no body, so the head is the
                // whole of it.
                let complete = loop {
                    match s.read(&mut b) {
                        Ok(0) | Err(_) => break false,
                        Ok(n) => {
                            buf.extend_from_slice(&b[..n]);
                            if buf.windows(4).any(|w| w == b"\r\n\r\n") {
                                break true;
                            }
                        }
                    }
                };
                if !complete {
                    break;
                }
                let text = String::from_utf8_lossy(&buf).into_owned();
                let path = text
                    .split_whitespace()
                    .nth(1)
                    .unwrap_or("/")
                    .trim_start_matches("http://")
                    .to_owned();
                let cookie = text
                    .lines()
                    .find(|l| l.to_ascii_lowercase().starts_with("cookie:"))
                    .map(|l| l["cookie:".len()..].trim().to_owned());
                log.lock().expect("log").push(Seen {
                    path: path.clone(),
                    cookie,
                });
                if s.write_all(respond(&path).as_bytes()).is_err() || s.flush().is_err() {
                    break;
                }
            }
        }
    });
    (addr, seen)
}

fn ok_with(set_cookie: Option<&str>) -> String {
    match set_cookie {
        Some(sc) => format!("HTTP/1.1 200 OK\r\nSet-Cookie: {sc}\r\nContent-Length: 0\r\n\r\n"),
        None => "HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n".to_owned(),
    }
}

fn redirect_to(location: &str, set_cookie: Option<&str>) -> String {
    let sc = match set_cookie {
        Some(sc) => format!("Set-Cookie: {sc}\r\n"),
        None => String::new(),
    };
    format!("HTTP/1.1 302 Found\r\nLocation: {location}\r\n{sc}Content-Length: 0\r\n\r\n")
}

fn jarred() -> Client<NativeTransport> {
    Client::builder(transport())
        .cookie_jar(CookieJar::new())
        .build()
        .expect("the native transport keeps no jar of its own")
}

async fn get(c: &Client<NativeTransport>, addr: std::net::SocketAddr, path: &str) {
    c.get(&format!("http://{addr}{path}"))
        .send()
        .await
        .expect("the server answers")
        .collect()
        .await
        .expect("an empty body collects");
}

/// The headline: a cookie the server set on one request comes back on the
/// next.
///
/// Both halves are being checked at once and neither can stand in for the
/// other — a client that stored nothing and a client that stored
/// everything but attached nothing fail this identically, which is why the
/// two mutations that break them are both listed in the commit.
#[test]
fn a_cookie_the_server_set_comes_back_on_the_next_request() {
    let (addr, seen) = recording_server(|path| match path {
        "/set" => ok_with(Some("sid=abc; Path=/")),
        _ => ok_with(None),
    });
    let c = jarred();

    rt().block_on(async {
        get(&c, addr, "/set").await;
        get(&c, addr, "/again").await;
    });

    let seen = seen.lock().expect("log").clone();
    assert_eq!(seen.len(), 2, "{seen:?}");
    assert_eq!(
        seen[0].cookie, None,
        "the first request cannot carry a cookie nobody had set yet"
    );
    assert_eq!(
        seen[1].cookie.as_deref(),
        Some("sid=abc"),
        "the jar neither stored the Set-Cookie nor attached it: {seen:?}"
    );
}

/// The control, and the reason the test above is about wiring rather than
/// about the network. The same server and the same two requests, with no
/// jar configured: nothing comes back.
///
/// Without this, a client that attached `sid=abc` to everything for ever —
/// or a server that echoed it — would pass the headline test.
#[test]
fn without_a_jar_nothing_comes_back() {
    let (addr, seen) = recording_server(|path| match path {
        "/set" => ok_with(Some("sid=abc; Path=/")),
        _ => ok_with(None),
    });
    let c = Client::builder(transport()).build().expect("supported");

    rt().block_on(async {
        get(&c, addr, "/set").await;
        get(&c, addr, "/again").await;
    });

    let seen = seen.lock().expect("log").clone();
    assert_eq!(seen.len(), 2, "{seen:?}");
    assert_eq!(
        seen[1].cookie, None,
        "a client with no jar must send no Cookie header at all: {seen:?}"
    );
}

/// A cookie set on a **redirect response** reaches the very next hop.
///
/// This is the ordinary shape of a login: the 302 carries the session
/// cookie and the browser is expected to have it by the time it asks for
/// the page. A client that stored cookies only from the response it
/// finally hands back would drop it, and every test above would still
/// pass.
#[test]
fn a_cookie_set_on_a_redirect_reaches_the_next_hop() {
    let (addr, seen) = recording_server(|path| match path {
        "/login" => redirect_to("/home", Some("sid=abc; Path=/")),
        _ => ok_with(None),
    });
    let c = jarred();

    rt().block_on(async { get(&c, addr, "/login").await });

    let seen = seen.lock().expect("log").clone();
    assert_eq!(seen.len(), 2, "the redirect was not followed: {seen:?}");
    assert_eq!(seen[1].path, "/home");
    assert_eq!(
        seen[1].cookie.as_deref(),
        Some("sid=abc"),
        "the Set-Cookie on the 302 never reached the jar, or the jar was \
         consulted once for the whole operation instead of once per hop: \
         {seen:?}"
    );
}

/// The other half of "once per hop": a `Cookie` header that was right for
/// one hop must not travel to the next.
///
/// The cookie is scoped `Path=/one`, and the redirect stays inside the
/// origin — so `SENSITIVE_HEADERS`, which strips `Cookie` only when the
/// origin changes, does nothing here, and `next_hop` clones the previous
/// hop's headers verbatim. Attaching before the loop instead of inside it,
/// or setting the header without clearing it first, sends `scoped=1` to
/// `/two/y`: a cookie delivered outside the path the server scoped it to.
#[test]
fn a_cookie_scoped_to_one_path_does_not_ride_a_redirect_to_another() {
    let (addr, seen) = recording_server(|path| match path {
        "/set" => ok_with(Some("scoped=1; Path=/one")),
        "/one/x" => redirect_to("/two/y", None),
        _ => ok_with(None),
    });
    let c = jarred();

    rt().block_on(async {
        get(&c, addr, "/set").await;
        get(&c, addr, "/one/x").await;
    });

    let seen = seen.lock().expect("log").clone();
    assert_eq!(seen.len(), 3, "{seen:?}");
    assert_eq!(
        seen[1].cookie.as_deref(),
        Some("scoped=1"),
        "the cookie should be attached inside its own path: {seen:?}"
    );
    assert_eq!(seen[2].path, "/two/y");
    assert_eq!(
        seen[2].cookie, None,
        "a Cookie header attached for /one/x was carried into /two/y, which \
         is outside the path the server scoped it to: {seen:?}"
    );
}

/// A caller who sets `Cookie` themselves keeps it, and keeps it whole.
///
/// The precedent is `decompress::negotiate`'s treatment of a caller-set
/// `Accept-Encoding`: someone who wrote the header meant it, and merging
/// the jar into it — or overwriting it — is a surprise either way. The jar
/// still *learns*, which the next assertion checks: the two halves are
/// separate decisions.
#[test]
fn a_caller_set_cookie_header_is_left_alone_and_the_jar_still_learns() {
    let (addr, seen) = recording_server(|path| match path {
        "/set" => ok_with(Some("sid=abc; Path=/")),
        "/manual" => ok_with(Some("second=2; Path=/")),
        _ => ok_with(None),
    });
    let c = jarred();

    rt().block_on(async {
        get(&c, addr, "/set").await;
        c.get(&format!("http://{addr}/manual"))
            .header("Cookie", "manual=1")
            .send()
            .await
            .expect("answers")
            .collect()
            .await
            .expect("empty body");
        get(&c, addr, "/after").await;
    });

    let seen = seen.lock().expect("log").clone();
    assert_eq!(
        seen[1].cookie.as_deref(),
        Some("manual=1"),
        "the caller's own Cookie header was replaced or added to: {seen:?}"
    );
    let after = seen[2].cookie.clone().unwrap_or_default();
    assert!(
        after.contains("sid=abc") && after.contains("second=2"),
        "the jar must go on storing while a caller drives the header by \
         hand — got {after:?}"
    );
}

/// The jar is shared by every clone of the client, and readable from
/// outside.
///
/// `Client::clone` is an `Arc` bump, and `Client::total_timeout` hands
/// back a second handle over the same transport by cloning the
/// configuration. A jar that lived in `Config` would be *copied* by that,
/// and the two handles would quietly disagree about the session.
#[test]
fn the_jar_is_shared_by_clones_and_readable() {
    let (addr, _) = recording_server(|path| match path {
        "/set" => ok_with(Some("sid=abc; Path=/")),
        _ => ok_with(None),
    });
    let c = jarred();
    let clone = c.clone();

    rt().block_on(async { get(&clone, addr, "/set").await });

    let names: Vec<String> = c
        .cookies()
        .expect("this client was given a jar")
        .iter()
        .map(|k| format!("{}={}", k.name(), k.value()))
        .collect();
    assert_eq!(
        names,
        vec!["sid=abc".to_owned()],
        "a request made through a clone did not reach the original's jar"
    );
}

/// A client with no jar reports none, rather than an empty one.
#[test]
fn a_client_without_a_jar_reports_none() {
    let c = Client::builder(transport()).build().expect("supported");
    assert!(c.cookies().is_none());
}

/// The gate: a jar of our own against a backend that keeps its own is a
/// typed refusal at `build()`, not a silent no-op.
///
/// This is the arm `Capabilities::owns_cookie_jar`'s doc comment promised
/// would arrive together with the setting — the same way
/// `RedirectSupport::Internal` earned its variant. `hclient-fetch` is the
/// backend that reports `true`, and it cannot be built for this target, so
/// the capability is fabricated on the mock: the check reads a `bool`, and
/// where the `bool` came from is not something it can tell.
#[test]
fn a_jar_against_a_transport_that_keeps_its_own_is_refused_at_build() {
    use hclient::mock::MockTransport;

    let mut caps = hclient::caps::Capabilities::none();
    caps.owns_cookie_jar = true;
    let m = MockTransport::new().with_capabilities(caps);

    let err = Client::builder(m)
        .cookie_jar(CookieJar::new())
        .build()
        .expect_err("a client-side jar cannot be honoured here");
    assert_eq!(
        err.what, "cookie_jar",
        "the refusal must name the setting: {err}"
    );
}

/// The control for the gate, and it is not ceremony: a check written as
/// `if caps.owns_cookie_jar` with the `cookies` half forgotten refuses
/// nothing, and a check written as `if cookies` with the capability half
/// forgotten refuses **every** jar ever configured. Only the pair of tests
/// tells those two apart from the correct one.
#[test]
fn the_same_jar_against_a_transport_that_keeps_none_builds() {
    use hclient::mock::MockTransport;

    let c = Client::builder(MockTransport::new())
        .cookie_jar(CookieJar::new())
        .build()
        .expect("a transport that keeps no jar is exactly where ours belongs");
    assert!(
        !c.capabilities().owns_cookie_jar,
        "the mock is the 'keeps no jar' side of this pair"
    );
    assert!(c.cookies().is_some());
}

/// And a client that never asked for a jar builds against a jar-owning
/// backend, which is the whole reason the refusal reads a `bool` the
/// caller set rather than the presence of the capability.
///
/// `Client::new()` in a browser goes through this line. If it ever stopped
/// holding, every browser program calling it would meet an
/// `UnsupportedCapability` for a setting it never made.
#[test]
fn no_jar_against_a_jar_owning_transport_is_fine() {
    use hclient::mock::MockTransport;

    let mut caps = hclient::caps::Capabilities::none();
    caps.owns_cookie_jar = true;
    let m = MockTransport::new().with_capabilities(caps);

    let built: Result<_, UnsupportedCapability> = Client::builder(m).build();
    assert!(
        built.is_ok(),
        "a client that never mentioned cookies must not be refused by a \
         backend that keeps its own jar"
    );
}
