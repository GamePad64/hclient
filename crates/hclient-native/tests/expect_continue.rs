//! `Expect: 100-continue`, watched from the server's side of the wire.
//!
//! The claim is about what is **not** on the socket yet, which no
//! caller-side assertion can make. So each test is an A/B differing in one
//! call — `Native::expect_continue` — against a server that reads the
//! head, looks at its socket, and only then decides what to send.

use hclient::Client;
use hclient_core::RequestBody;
use hclient_dns::IpLiteralOnly;
use hclient_native::Native;
use hclient_rt_tokio::Tokio;
use hclient_tls::NoTls;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener};
use std::sync::mpsc;
use std::time::Duration;

const BOUND: Duration = Duration::from_secs(5);
/// Long enough that a client which did not withhold has certainly written,
/// on loopback, where a write is a memcpy. It bounds a **negative**
/// observation, which is the one shape that cannot be made causal — the
/// A/B is what carries the claim.
const SETTLE: Duration = Duration::from_millis(250);

/// Reads the head, waits, reports whether the body had arrived by then,
/// and only then sends `interim` (if any) followed by the response.
/// What the server saw: whether the body was on the socket **before** it
/// said anything, and whether it ever arrived at all.
///
/// The second half was missing at first, and the omission was load-bearing
/// — the fixture answered `200` whether or not a body came, so a client
/// that withheld the body **for ever** passed. The mutation that stops the
/// `100` from opening the gate survived until this field existed.
#[derive(Debug, PartialEq, Eq)]
struct Seen {
    early: bool,
    body: bool,
}

fn server(interim: Option<&'static [u8]>) -> (SocketAddr, mpsc::Receiver<Seen>) {
    let l = TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = l.local_addr().expect("addr");
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        for conn in l.incoming() {
            let Ok(mut s) = conn else { break };
            let mut head = Vec::new();
            let mut b = [0u8; 1];
            while !head.ends_with(b"\r\n\r\n") {
                match s.read(&mut b) {
                    Ok(0) | Err(_) => break,
                    Ok(_) => head.push(b[0]),
                }
            }
            std::thread::sleep(SETTLE);
            // The observation: is anything readable *before* we have said
            // anything at all?
            s.set_nonblocking(true).expect("nonblocking");
            let mut peek = [0u8; 64];
            let mut seen_bytes = Vec::new();
            if let Ok(n) = s.read(&mut peek) {
                seen_bytes.extend_from_slice(&peek[..n]);
            }
            let early = !seen_bytes.is_empty();
            s.set_nonblocking(false).expect("blocking");

            if let Some(interim) = interim {
                let _ = s.write_all(interim);
                let _ = s.flush();
            }
            // **Wait for the body rather than answering regardless.** A
            // fixture that replied either way would pass a client that
            // withheld for ever, which is exactly what it did until a
            // mutation went looking.
            s.set_read_timeout(Some(Duration::from_secs(3))).ok();
            while !seen_bytes.windows(7).any(|w| w == b"PAYLOAD") {
                match s.read(&mut peek) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => seen_bytes.extend_from_slice(&peek[..n]),
                }
            }
            let body = seen_bytes.windows(7).any(|w| w == b"PAYLOAD");
            let _ = tx.send(Seen { early, body });
            let _ =
                s.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nhi");
            let _ = s.flush();
        }
    });
    (addr, rx)
}

fn post(
    addr: SocketAddr,
    waiting: bool,
) -> impl std::future::Future<Output = Result<u16, hclient_core::Error>> {
    let base = Native::new(Tokio, NoTls, IpLiteralOnly);
    let transport = if waiting {
        // **Far longer than this test's own guard**, deliberately: with a
        // short bound the body would go out on the timer and the test
        // would pass without the `100` ever opening anything. Only the
        // `100` can release it here, and a client that ignored one hangs
        // rather than passing.
        base.expect_continue(Duration::from_secs(30))
    } else {
        base
    };
    let client = Client::builder(transport).build().expect("build");
    let url = format!("http://127.0.0.1:{}/upload", addr.port());
    async move {
        let resp = client
            .post(&url)
            .header("expect", "100-continue")
            .body(RequestBody::Full(bytes::Bytes::from_static(b"PAYLOAD")))
            .send()
            .await?;
        Ok(resp.status().as_u16())
    }
}

/// **With the opt-in, the body is not on the socket when the server
/// looks; without it, it is.** One call apart, and the pair is the claim:
/// either arm alone would pass for a client that always did one thing.
#[tokio::test(flavor = "multi_thread")]
async fn the_body_waits_for_the_100_and_without_the_opt_in_it_does_not() {
    for (waiting, expect_early) in [(true, false), (false, true)] {
        let (addr, early) = server(Some(b"HTTP/1.1 100 Continue\r\n\r\n"));
        let status = tokio::time::timeout(BOUND, post(addr, waiting))
            .await
            .expect("must not hang")
            .expect("the exchange completes either way");
        assert_eq!(status, 200);
        assert_eq!(
            early.recv_timeout(BOUND).expect("the server looked"),
            Seen {
                early: expect_early,
                body: true
            },
            "waiting={waiting}: the body must arrive either way, and only its \
             timing may differ"
        );
    }
}

/// **A server that never says `100` still gets the body**, once the bound
/// passes — RFC 9110 §10.1.1 makes that outcome *proceed*, not fail.
///
/// This is the arm that decides whether the feature is safe to switch on
/// at all: HTTP/1.0 servers and some proxies ignore `Expect` entirely, and
/// a client that waited for them for ever would hang every such upload.
#[tokio::test(flavor = "multi_thread")]
async fn a_server_that_never_continues_still_gets_the_body_after_the_bound() {
    let (addr, early) = server(None);
    let base = Native::new(Tokio, NoTls, IpLiteralOnly);
    let client = Client::builder(base.expect_continue(Duration::from_millis(400)))
        .build()
        .expect("build");
    let url = format!("http://127.0.0.1:{}/upload", addr.port());
    let status = tokio::time::timeout(BOUND, async {
        client
            .post(&url)
            .header("expect", "100-continue")
            .body(RequestBody::Full(bytes::Bytes::from_static(b"PAYLOAD")))
            .send()
            .await
            .map(|r| r.status().as_u16())
    })
    .await
    .expect("must not hang — a bound that never fires is a hang")
    .expect("the request completes without a 100");
    assert_eq!(status, 200);
    assert_eq!(
        early.recv_timeout(BOUND).expect("the server looked"),
        Seen {
            early: false,
            body: true
        },
        "withheld at the check, and sent afterwards anyway"
    );
}

/// **Without the header there is nothing to wait for**, even configured.
/// The gate needs both halves; this is the one the A/B above cannot show.
#[tokio::test(flavor = "multi_thread")]
async fn a_request_without_the_header_is_never_withheld() {
    let (addr, early) = server(None);
    let base = Native::new(Tokio, NoTls, IpLiteralOnly);
    let client = Client::builder(base.expect_continue(Duration::from_secs(30)))
        .build()
        .expect("build");
    let url = format!("http://127.0.0.1:{}/upload", addr.port());
    let status = tokio::time::timeout(BOUND, async {
        client
            .post(&url)
            .body(RequestBody::Full(bytes::Bytes::from_static(b"PAYLOAD")))
            .send()
            .await
            .map(|r| r.status().as_u16())
    })
    .await
    .expect("a 30s bound must never be reached by a request that never asked")
    .expect("send");
    assert_eq!(status, 200);
    assert_eq!(
        early.recv_timeout(BOUND).expect("the server looked"),
        Seen {
            early: true,
            body: true
        },
        "no header, no waiting"
    );
}
