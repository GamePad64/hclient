//! The netrc fill at the `Client` level: per hop, looked up by that
//! hop's own host, never overriding a caller's header, silenced by a
//! configured auth scheme. The parser's semantics are curl's — pinned
//! against the captured answers in `auth::netrc::tests` and re-captured
//! live in `tests/netrc_curl.rs`.

#![cfg(feature = "test-util")]

use hclient::auth::netrc::Netrc;
use hclient::mock::MockTransport;
use hclient::{Client, RequestBody};

// base64("alice:pw") — asserted as the exact wire value so a change in
// the encoding path fails loudly rather than plausibly.
const ALICE_BASIC: &str = "Basic YWxpY2U6cHc=";

fn localhost_netrc() -> Netrc {
    Netrc::parse("machine localhost\nlogin alice\npassword pw\n")
}

fn two_hops_client(netrc: Option<Netrc>, redirect_target: &str) -> (Client, MockTransport) {
    let m = MockTransport::new();
    m.push_response(
        http::Response::builder()
            .status(302)
            .header("location", redirect_target)
            .body("")
            .unwrap(),
    );
    m.push_response(http::Response::builder().status(200).body("done").unwrap());
    let mut b = Client::builder(m.clone());
    if let Some(netrc) = netrc {
        b = b.netrc(netrc);
    }
    (b.build().unwrap(), m)
}

fn run(c: &Client, url: &str) {
    let req = http::Request::builder()
        .uri(url)
        .body(RequestBody::Empty)
        .unwrap();
    futures_executor::block_on(c.execute(req)).unwrap();
}

fn auth_of(m: &MockTransport, hop: usize) -> Option<String> {
    m.requests()
        .get(hop)
        .and_then(|r| r.headers.get("authorization"))
        .map(|v| v.to_str().unwrap().to_owned())
}

#[test]
fn netrc_authenticates_a_listed_host() {
    let (c, m) = two_hops_client(Some(localhost_netrc()), "https://localhost/b");
    run(&c, "https://localhost/a");
    assert_eq!(auth_of(&m, 0).as_deref(), Some(ALICE_BASIC));
}

#[test]
fn without_netrc_nothing_is_added() {
    let (c, m) = two_hops_client(None, "https://localhost/b");
    run(&c, "https://localhost/a");
    assert_eq!(auth_of(&m, 0), None);
}

#[test]
fn the_callers_authorization_wins_over_the_file() {
    let (c, m) = two_hops_client(Some(localhost_netrc()), "https://localhost/b");
    let req = http::Request::builder()
        .uri("https://localhost/a")
        .header("authorization", "Bearer token")
        .body(RequestBody::Empty)
        .unwrap();
    futures_executor::block_on(c.execute(req)).unwrap();
    assert_eq!(auth_of(&m, 0).as_deref(), Some("Bearer token"));
}

#[test]
fn a_redirect_to_an_unlisted_host_goes_out_anonymous() {
    // Entry is `localhost` only; the first hop is listed, the redirect
    // target (`other.example`) is not. Nothing travels from hop 1 — the
    // redirect stage strips it — and the unlisted host gets nothing.
    let (c, m) = two_hops_client(Some(localhost_netrc()), "https://other.example/b");
    let req = http::Request::builder()
        .uri("https://localhost/a")
        .header("location-witness", "1")
        .body(RequestBody::Empty)
        .unwrap();
    let _ = req;
    run(&c, "https://localhost/a");
    assert_eq!(auth_of(&m, 0).as_deref(), Some(ALICE_BASIC));
    assert_eq!(auth_of(&m, 1), None);
}

#[test]
fn a_redirect_to_a_listed_host_carries_that_hosts_entry() {
    // The discriminator for per-hop lookup: the first hop is UNLISTED
    // (anonymous — nothing to carry), and the redirect lands on the
    // listed host. A one-shot fill would leave hop 2 anonymous; the
    // per-hop lookup puts the entry's credentials on it.
    let m2 = MockTransport::new();
    m2.push_response(
        http::Response::builder()
            .status(302)
            .header("location", "https://localhost/b")
            .body("")
            .unwrap(),
    );
    m2.push_response(http::Response::builder().status(200).body("done").unwrap());
    let c = Client::builder(m2.clone())
        .netrc(localhost_netrc())
        .build()
        .unwrap();
    run(&c, "https://unlisted.example/a");
    assert_eq!(auth_of(&m2, 0), None);
    assert_eq!(auth_of(&m2, 1).as_deref(), Some(ALICE_BASIC));
}

#[test]
fn a_configured_auth_scheme_silences_the_file() {
    // The caller attached a scheme: its conversation is the one they
    // asked for, and a preemptive Basic from the file would answer a
    // challenge nobody issued. The scheme here authenticates nothing —
    // its silence is the assertion.
    use hclient_core::auth::{Auth, AuthFlow, AuthRequest, AuthStep};

    #[derive(Debug)]
    struct Silent;
    impl Auth for Silent {
        fn start(&self) -> hclient_core::auth::BoxFlow {
            Box::new(SilentFlow)
        }
    }
    struct SilentFlow;
    impl AuthFlow for SilentFlow {
        fn authorize(&mut self, _req: &AuthRequest<'_>, _headers: &mut http::HeaderMap) {}
        fn on_response(&mut self, _s: http::StatusCode, _h: &http::HeaderMap) -> AuthStep {
            AuthStep::Done
        }
    }

    let m = MockTransport::new();
    m.push_response(http::Response::builder().status(200).body("done").unwrap());
    let c = Client::builder(m.clone())
        .netrc(localhost_netrc())
        .build()
        .unwrap();
    futures_executor::block_on(c.get("https://localhost/a").auth(Silent).send()).unwrap();
    assert_eq!(auth_of(&m, 0), None);
}

/// The machine-file half: `Client::new()` honours `$NETRC` — the proxy
/// ruling applied to credentials. The test runs **itself** as a child
/// process with the environment set, because `std::env::set_var` is
/// `unsafe` in edition 2024 and the file location is the machine's, not
/// this process's: the parent writes a fixture, hands the child `NETRC`
/// and a listener's URL, and asserts the child's `Client::new()`
/// authenticates. The child needs the `netrc-child` mode switch below
/// and a default transport, hence the gate.
#[cfg(all(feature = "default-transport", not(target_family = "wasm")))]
#[test]
fn netrc_default_constructor_honours_the_machine_file() {
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::process::Command;

    let fixture = std::env::temp_dir().join("hclient-netrc-child-test.netrc");
    std::fs::write(&fixture, "machine 127.0.0.1\nlogin alice\npassword pw\n").unwrap();

    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let t = std::thread::spawn(move || {
        let (mut sock, _) = listener.accept().unwrap();
        let mut buf = Vec::new();
        let mut chunk = [0u8; 4096];
        loop {
            let n = sock.read(&mut chunk).unwrap();
            if n > 0 {
                buf.extend_from_slice(&chunk[..n]);
            }
            if n == 0 || buf.windows(4).any(|w| w == b"\r\n\r\n") {
                break;
            }
        }
        let text = String::from_utf8_lossy(&buf);
        let mut auth = None;
        for line in text.lines() {
            let lower = line.to_ascii_lowercase();
            if let Some(v) = lower.strip_prefix("authorization: ") {
                auth = Some(line[line.len() - v.len()..].to_owned());
            }
        }
        let _ = sock.write_all(b"HTTP/1.1 200 OK\r\ncontent-length: 0\r\n\r\n");
        auth
    });

    let exe = std::env::current_exe().unwrap();
    let status = Command::new(exe)
        .args(["netrc_child_executor", "--exact"])
        .env("NETRC", &fixture)
        .env("NO_PROXY", "127.0.0.1")
        .env(
            "HCLIENT_NETRC_CHILD_URL",
            format!("http://127.0.0.1:{port}/p"),
        )
        .status()
        .expect("spawn self");
    assert!(status.success(), "the netrc child failed: {status}");
    let wire = t.join().unwrap().unwrap_or_else(|| "(none)".to_owned());
    assert_eq!(wire, "Basic YWxpY2U6cHc=", "Client::new() ignored $NETRC");
}

/// The child half of the test above: it **is** the process that
/// `Client::new()` runs in, spawned with `--exact` so the harness runs
/// nothing but this. The URL comes from the environment rather than an
/// argument, because a libtest harness would read an argument as a test
/// filter — which is how the first draft of this spawned itself
/// recursively and hung for 300 seconds.
#[cfg(all(feature = "default-transport", not(target_family = "wasm")))]
#[test]
fn netrc_child_executor() {
    let Ok(url) = std::env::var("HCLIENT_NETRC_CHILD_URL") else {
        return;
    };
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async move {
        let client = hclient::Client::new().expect("a client");
        client
            .post(url)
            .body("x")
            .send()
            .await
            .expect("the child's request");
    });
}
