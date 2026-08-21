//! `Timeouts::first_byte` and `Timeouts::between_bytes`, against servers
//! that behave the way each one exists for.
//!
//! # The observer is outside the client, and so is the misbehaviour
//!
//! A test that set a timeout and then checked the transport had stored it
//! would pass against a transport that stored it and did nothing else,
//! which is exactly the state these two were in until this work: honestly
//! declared `false`, and settable nowhere. So nothing here reads a field.
//! Each claim is a real socket to a real server built to misbehave in one
//! specific way, and the assertion is which typed error the caller gets and
//! roughly when.
//!
//! Three servers, one per shape the two bounds divide between them:
//!
//! - `answers_never` — accepts, reads the request, sends **not one byte**.
//!   That is `first_byte`'s case and nothing else's: there is no head, so
//!   there is no body to be idle.
//! - `head_then_silence` — sends a complete head promising a body, then
//!   nothing at all, for ever. `first_byte` is satisfied by the head; this
//!   is the case `Timeouts::total` deliberately cannot cut (nothing wakes
//!   an elapsed-time wrapper on a body nobody feeds), and it is
//!   `between_bytes`' whole reason to exist.
//! - `stalls_mid_body` — head, some of the body, then silence. The same
//!   bound, but reached after a frame has already gone through, which is
//!   what checks that the sleep is restarted rather than started once.
//!
//! # Every bound gets a control, and the controls are what make it a
//! measurement
//!
//! A timeout test that only shows an error can be passed by a transport
//! that fails everything. Each claim below is therefore paired:
//!
//! - the same silent server with the bound **unset** must hang (the test
//!   bounds it from outside and requires the hang), so the error came from
//!   our bound and not from the fixture giving up;
//! - a server that dribbles a byte at a time, well inside the bound, must
//!   **succeed** — which no implementation that starts one sleep and never
//!   restarts it can do.
#![cfg(not(target_family = "wasm"))]

use hclient::{Client, Timeouts};
use hclient_core::{ErrorKind, Phase};
use hclient_dns_system::SystemDns;
use hclient_native::{BetweenBytesElapsed, FirstByteTimedOut, Native};
use hclient_rt_tokio::Tokio;
use hclient_tls_rustls::Rustls;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::sync::mpsc;
use std::time::Duration;

/// The bound the tests set. Long enough not to race the loopback, short
/// enough that four tests of it cost a second between them.
const BOUND: Duration = Duration::from_millis(250);

/// How long a test waits for something that must NOT happen. Six times the
/// bound: an implementation that fired late has had five extra chances.
const PATIENCE: Duration = Duration::from_millis(1500);

/// What a fixture server does after it has read the request head.
#[derive(Clone, Copy)]
enum Behaviour {
    /// Not one byte, ever. The connection stays open.
    AnswersNever,
    /// A complete head promising ten bytes, then silence for ever.
    HeadThenSilence,
    /// Head, two of the promised ten bytes, then silence for ever.
    StallsMidBody,
    /// Head, then the ten bytes one at a time with `gap` between them —
    /// the control: slow, but never silent for as long as the bound.
    Dribbles(Duration),
}

/// Reads one request head off `sock`, then does `behaviour` — and in every
/// case holds the socket open afterwards, so that nothing the client sees
/// can be attributed to the server hanging up.
///
/// `closed` is told when, and only when, the **client** closes its end:
/// `Ok(0)` from `read` is a `FIN` and cannot be anything else (a timeout
/// is `Err`, data is `Ok(n > 0)`). That is how
/// `a_between_bytes_timeout_closes_the_connection_the_server_sees` gets an
/// observer for the one half of this bound that a returned error does not
/// show.
fn serve(mut sock: TcpStream, behaviour: Behaviour, closed: mpsc::Sender<()>) {
    let mut buf: Vec<u8> = Vec::new();
    let mut chunk = [0u8; 1024];
    while !buf.windows(4).any(|w| w == b"\r\n\r\n") {
        match sock.read(&mut chunk) {
            Ok(0) | Err(_) => return,
            Ok(n) => buf.extend_from_slice(&chunk[..n]),
        }
    }
    let head: &[u8] = b"HTTP/1.1 200 OK\r\nContent-Length: 10\r\n\r\n";
    let written = match behaviour {
        Behaviour::AnswersNever => Ok(()),
        Behaviour::HeadThenSilence => sock.write_all(head).and_then(|()| sock.flush()),
        Behaviour::StallsMidBody => sock
            .write_all(head)
            .and_then(|()| sock.write_all(b"01"))
            .and_then(|()| sock.flush()),
        Behaviour::Dribbles(gap) => (|| {
            sock.write_all(head)?;
            sock.flush()?;
            for i in 0..10u8 {
                std::thread::sleep(gap);
                sock.write_all(&[b'0' + i])?;
                sock.flush()?;
            }
            Ok(())
        })(),
    };
    if written.is_err() {
        return;
    }
    // Hold the connection open well past anything the tests wait for. The
    // client closing its end is what ends this thread, and that is the
    // only thing that should.
    let _ = sock.set_read_timeout(Some(PATIENCE * 4));
    if let Ok(0) = sock.read(&mut chunk) {
        let _ = closed.send(());
    }
}

fn server_watching(behaviour: Behaviour) -> (SocketAddr, mpsc::Receiver<()>) {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = listener.local_addr().expect("local_addr");
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        for sock in listener.incoming() {
            let Ok(sock) = sock else { continue };
            let tx = tx.clone();
            std::thread::spawn(move || serve(sock, behaviour, tx));
        }
    });
    (addr, rx)
}

/// The same server for the tests that have no interest in the close.
fn server(behaviour: Behaviour) -> SocketAddr {
    server_watching(behaviour).0
}

fn client(timeouts: Timeouts) -> Client {
    Client::builder(Native::new(
        Tokio,
        Rustls::with_webpki_roots(),
        SystemDns::new(Tokio),
    ))
    .timeouts(timeouts)
    .build()
    .expect("build")
}

/// One request, driven all the way through its body, bounded from outside
/// so that a bound which never fires is a failure with a name rather than
/// a hung run.
///
/// Collecting the body matters: `between_bytes` is enforced *in* the body,
/// so a helper that stopped at the head would never reach it.
async fn get_all(timeouts: Timeouts, addr: SocketAddr) -> Result<String, hclient_core::Error> {
    get_all_within(PATIENCE, timeouts, addr).await
}

/// [`get_all`] with the outer guard named, for the one test whose body is
/// *supposed* to take a long time.
///
/// **`PATIENCE`'s reasoning does not cover a body that dribbles.** It is
/// six times `BOUND` because a bound that must not fire has then had five
/// extra chances — an argument about tests where nothing should happen.
/// `a_slow_but_never_silent_body_is_not_cut_by_between_bytes` is the one
/// where something legitimately does: ten frames `BOUND / 5` apart take
/// **twice** the bound on their own, so the real margin there was 3x and
/// not 6x, and ten sequential sleeps plus TCP on a loaded `macos-latest`
/// runner fit inside it. It went red once that runner started finishing
/// runs at all.
///
/// This is a **hang guard and not a claim** — the assertion is the body's
/// value — so the number is generous on purpose, and it stays a named
/// failure rather than a hung run.
async fn get_all_within(
    patience: Duration,
    timeouts: Timeouts,
    addr: SocketAddr,
) -> Result<String, hclient_core::Error> {
    let c = client(timeouts);
    let out = tokio::time::timeout(patience, async {
        c.get(&format!("http://{addr}/"))
            .send()
            .await?
            .collect()
            .await?
            .text()
    })
    .await;
    match out {
        Ok(r) => r,
        Err(_) => panic!(
            "nothing ended this request within {patience:?} — the bound under test never fired"
        ),
    }
}

/// The same call with no expectation that it ends: returns `true` when it
/// was still running after `PATIENCE`. This is what the controls assert,
/// and it is why the claims above mean anything.
async fn hangs(timeouts: Timeouts, addr: SocketAddr) -> bool {
    let c = client(timeouts);
    tokio::time::timeout(PATIENCE, async {
        let _ = c
            .get(&format!("http://{addr}/"))
            .send()
            .await?
            .collect()
            .await?
            .text();
        Ok::<(), hclient_core::Error>(())
    })
    .await
    .is_err()
}

// ── first_byte ──────────────────────────────────────────────────────────

#[tokio::test]
async fn a_server_that_never_answers_hits_the_first_byte_bound() {
    let addr = server(Behaviour::AnswersNever);
    let err = get_all(
        Timeouts {
            resolve: None,
            first_byte: Some(BOUND),
            ..Default::default()
        },
        addr,
    )
    .await
    .expect_err("a server that sends nothing must not produce a response");

    assert_eq!(
        *err.kind(),
        ErrorKind::Timeout(Phase::FirstByte),
        "the phase is the point: a caller retries a `FirstByte` differently from a \
         `Connect` or a `BetweenBytes`: {err}"
    );
    let source = std::error::Error::source(&err)
        .and_then(|s| s.downcast_ref::<FirstByteTimedOut>())
        .expect("the source names the bound, so nobody has to parse the message");
    assert_eq!(*source, FirstByteTimedOut(BOUND));
}

/// The control: the same server, the same request, with the bound unset.
/// Without this, the test above would also pass against a transport that
/// failed every request to a server that happens to be slow.
#[tokio::test]
async fn without_the_first_byte_bound_the_same_server_hangs() {
    let addr = server(Behaviour::AnswersNever);
    assert!(
        hangs(Timeouts::default(), addr).await,
        "nothing but the bound can end this request, so with no bound it must not end"
    );
}

/// And the bound is not "any wait fails": a server that answers well
/// inside it is served normally.
#[tokio::test]
async fn a_server_that_answers_inside_the_first_byte_bound_is_not_cut() {
    let addr = server(Behaviour::Dribbles(Duration::from_millis(10)));
    let body = get_all(
        Timeouts {
            resolve: None,
            first_byte: Some(BOUND),
            ..Default::default()
        },
        addr,
    )
    .await
    .expect("the head arrives at once, so the first_byte bound has nothing to do");
    assert_eq!(body, "0123456789");
}

// ── between_bytes ───────────────────────────────────────────────────────

/// The case `Timeouts::total` deliberately cannot cut, and the reason this
/// bound exists: a head arrives, and then the peer goes **completely
/// silent**. Nothing wakes a wrapper that only reads a clock, so only a
/// bound holding a sleep of its own can fire here.
#[tokio::test]
async fn a_body_that_goes_silent_after_the_head_hits_the_between_bytes_bound() {
    let addr = server(Behaviour::HeadThenSilence);
    let err = get_all(
        Timeouts {
            resolve: None,
            between_bytes: Some(BOUND),
            ..Default::default()
        },
        addr,
    )
    .await
    .expect_err("a body that never arrives must not collect into a value");

    assert_eq!(
        *err.kind(),
        ErrorKind::Timeout(Phase::BetweenBytes),
        "{err}"
    );
    let source = std::error::Error::source(&err)
        .and_then(|s| s.downcast_ref::<BetweenBytesElapsed>())
        .expect("the source names the bound");
    assert_eq!(*source, BetweenBytesElapsed(BOUND));
}

/// The same bound reached after a frame has already gone through — which
/// is what shows the sleep is armed per gap rather than once for the whole
/// body.
#[tokio::test]
async fn a_body_that_stalls_half_way_hits_the_between_bytes_bound() {
    let addr = server(Behaviour::StallsMidBody);
    let err = get_all(
        Timeouts {
            resolve: None,
            between_bytes: Some(BOUND),
            ..Default::default()
        },
        addr,
    )
    .await
    .expect_err("a body that stops half way must not collect into a value");

    assert_eq!(
        *err.kind(),
        ErrorKind::Timeout(Phase::BetweenBytes),
        "{err}"
    );
}

/// The control for both: with no bound, the same stalled body never ends.
#[tokio::test]
async fn without_the_between_bytes_bound_a_stalled_body_hangs() {
    let addr = server(Behaviour::StallsMidBody);
    assert!(
        hangs(Timeouts::default(), addr).await,
        "the server holds the connection open and sends nothing more, so with no bound \
         the collect must not finish"
    );
}

/// The control that no "one sleep for the whole body" implementation can
/// pass: ten frames, each `BOUND / 5` apart, so the body takes **twice**
/// the bound in total and no single gap comes near it. It must succeed,
/// and the value must be the whole body rather than a prefix.
#[tokio::test]
async fn a_slow_but_never_silent_body_is_not_cut_by_between_bytes() {
    let addr = server(Behaviour::Dribbles(BOUND / 5));
    // Ten frames `BOUND / 5` apart: the body takes 2x `BOUND` when
    // everything is healthy, so the guard is sized against *that* rather
    // than against `BOUND` — see `get_all_within`.
    let body = get_all_within(
        PATIENCE * 4,
        Timeouts {
            resolve: None,
            between_bytes: Some(BOUND),
            ..Default::default()
        },
        addr,
    )
    .await
    .expect("no gap ever reached the bound, so nothing should have been cut");
    assert_eq!(body, "0123456789");
}

/// The two bounds are not one bound with two names: a body that goes
/// silent after the head is `BetweenBytes`, even when a `first_byte` bound
/// is set as well and is the smaller of the two. A transport that enforced
/// one timer for both phases would report `FirstByte` here.
#[tokio::test]
async fn a_silent_body_is_between_bytes_even_with_a_tighter_first_byte_bound_set() {
    let addr = server(Behaviour::HeadThenSilence);
    let err = get_all(
        Timeouts {
            resolve: None,
            first_byte: Some(BOUND / 2),
            between_bytes: Some(BOUND),
            ..Default::default()
        },
        addr,
    )
    .await
    .expect_err("the body never arrives");

    assert_eq!(
        *err.kind(),
        ErrorKind::Timeout(Phase::BetweenBytes),
        "the head arrived inside the first_byte bound, so the phase that failed is the \
         body's: {err}"
    );
}

/// Firing the bound **drops the body**, and dropping a response body
/// before it ends is a cancellation under `Transport::execute`'s contract
/// (v0.2 W1) — so the socket goes away rather than being left to drain
/// into nobody. A bound that returned an error and left the transfer
/// running would be a bound on the caller's patience, not on the
/// operation.
///
/// This is the one half of the bound the tests above cannot see: they call
/// `collect`, which consumes the response, so the socket would close on
/// the drop whatever the wrapper did. Here the `Response` is deliberately
/// **kept alive** past the error — `Response::chunk` seals after the first
/// `Err` but does not drop the body — so the only thing that can close the
/// connection is the wrapper having dropped it already.
#[tokio::test]
async fn a_between_bytes_timeout_closes_the_connection_the_server_sees() {
    let (addr, closed) = server_watching(Behaviour::HeadThenSilence);
    let c = client(Timeouts {
        resolve: None,
        between_bytes: Some(BOUND),
        ..Default::default()
    });
    let mut resp = tokio::time::timeout(PATIENCE, c.get(&format!("http://{addr}/")).send())
        .await
        .expect("the head arrives at once")
        .expect("and is a response");

    let err = tokio::time::timeout(PATIENCE, async {
        loop {
            match resp.chunk().await {
                Some(Ok(_)) => continue,
                Some(Err(e)) => break e,
                None => panic!("a body that never arrives must not end cleanly"),
            }
        }
    })
    .await
    .expect("the bound must fire");
    assert_eq!(
        *err.kind(),
        ErrorKind::Timeout(Phase::BetweenBytes),
        "{err}"
    );

    let saw_close =
        tokio::task::spawn_blocking(move || closed.recv_timeout(Duration::from_millis(500)))
            .await
            .expect("join");
    assert!(
        saw_close.is_ok(),
        "the server must have seen its connection close while the response was still held: \
         an error alone leaves the transfer running"
    );
    // Held to exactly here: dropping it earlier would close the socket for
    // a reason that has nothing to do with the bound.
    drop(resp);
}
