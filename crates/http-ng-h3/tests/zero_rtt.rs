//! 0-RTT **acceptance**, end to end, watched from outside both endpoints.
//!
//! `live.rs` covers the failure paths: `into_0rtt` refusing for want of key
//! material, and a server rejecting the keys it was offered. The happy path
//! was the gap `docs/v03-acceptance.md` recorded — *"0-RTT ACCEPTANCE has
//! not been observed end to end here; rejection has"* — and it is the one
//! that cannot be closed from either endpoint, because both endpoints'
//! answers are the thing under test. A client that awaited
//! `quinn::ZeroRttAccepted` and printed it would be reporting its own
//! opinion of its own behaviour.
//!
//! So the observers are two, and neither is the client:
//!
//! - **The wire** (`mod wire`): a UDP relay counting QUIC packet types in
//!   cleartext long headers. A 0-RTT packet exists only to carry
//!   application data sent before the handshake completed, so its presence
//!   *is* the claim, and its position before the client's first Handshake
//!   packet — where the client's `Finished` lives — is the ordering.
//! - **The server** (`server::start_watching_early_data`): when it resolved
//!   the request, against when its own handshake completed. A server that
//!   had discarded the early data would see the request only after the
//!   handshake, because that is when the client would have had to resend
//!   it.
//!
//! The relay is what makes the second of those causal rather than merely
//! likely, and it does it **without a window**. It holds the server's
//! flight back until the server has resolved a request, and a client's
//! handshake completes on processing that flight — so a request the server
//! resolved before the release is one it resolved before its handshake, at
//! any speed and under any load. There is no duration to tune and none to
//! lose: `wire::Wire::release` carries the measurement of what a tuned one
//! cost. The failure the test exists to catch survives the change and gets
//! sharper — a server that discarded the early data can only resolve the
//! request after the handshake, which cannot happen while the flight is
//! held, so nothing releases it and the backstop expires.
//!
//! # The server-side signal that looked right and is not
//!
//! `Connecting::into_0rtt` hands back a `ZeroRttAccepted` future, and the
//! obvious reading is that a server can await it to learn whether it
//! accepted the client's early data. It cannot:
//! `quinn_proto::Connection::accepted_0rtt` is assigned **client-side
//! only** (`quinn-proto-0.11.16/src/connection/mod.rs:2540`, a block guarded
//! by `if self.side.is_client()` with a `debug_assert!` of the same inside
//! it), so a server always reports `false`. This test was written against
//! that assumption first and read `0` while the relay was counting the
//! 0-RTT packets going past — which is how the timing observation below
//! came to exist.
//!
//! Measured on this host: 9 zero-RTT packets carrying 6375 bytes before any
//! Handshake packet from the client; the server resolving the request
//! 2.6 ms into the connection and completing its handshake at 7.2 ms, with
//! the relay holding its flight for the 4.1 ms in between.
#![cfg(not(target_family = "wasm"))]

mod server;
mod wire;

use http_body_util::BodyExt;
use http_ng_core::unversioned::Transport;
use http_ng_core::{AllowEarlyData, RequestBody};
use http_ng_dns::IpLiteralOnly;
use http_ng_h3::H3;
use http_ng_rt_tokio::TokioHandle;
use server::Behaviour;
use std::time::Duration;
use wire::{Kind, Wire};

/// The relay's backstop: how long it will hold the server's flight if
/// nothing releases it.
///
/// **Not the plan.** The hold normally ends the moment the server has
/// resolved a request — see `wire::Wire::release`, which carries the
/// measurement that made an event-driven hold necessary rather than tidy.
/// Reaching this deadline means the server never resolved one while its
/// handshake was outstanding, which is the failure this test exists to
/// catch; the value only decides whether that failure arrives as an
/// assertion with numbers in it or as a wedged suite.
///
/// It stays under quinn's first PTO (about a second: no RTT sample, a
/// 333 ms assumed initial RTT, so `srtt + 4*rttvar + max_ack_delay` lands
/// near 1022 ms) for the happy path, where the release comes in
/// milliseconds. On the failure path a retransmission or two is the least
/// of what has gone wrong.
const BACKSTOP: Duration = Duration::from_secs(5);

/// A header big enough that carrying it is not something the h3 control
/// stream could have done.
///
/// Without this the test would prove only that *some* early data went out —
/// and h3 opens a control stream and sends SETTINGS on it the moment the
/// connection exists, which is a few dozen bytes of early data that says
/// nothing about the request. Eight kilobytes of it cannot be anything but
/// the request's own header section.
const PADDING: usize = 8192;

fn padded_get(addr: std::net::SocketAddr, path: &str, pad: bool) -> http::Request<RequestBody> {
    let mut b = http::Request::builder().uri(format!("https://{addr}{path}"));
    if pad {
        // Not one repeated byte: QPACK may Huffman-code a literal, and a
        // run of a single character would compress far enough to make the
        // byte count below meaningless. A rotation through the alphanumeric
        // range codes at five to six bits a character, so the worst case is
        // about 70% of the length rather than a few per cent of it — which
        // is why the assertion asks for half.
        const ALPHABET: &[u8] = b"abcdefghijklmnopqrstuvwxyz0123456789";
        let v: String = (0..PADDING)
            .map(|i| ALPHABET[(i * 7 + i / 36) % ALPHABET.len()] as char)
            .collect();
        b = b.header("x-padding", v);
    }
    b.body(RequestBody::Empty).unwrap()
}

async fn body_of<B>(r: http::Response<B>) -> String
where
    B: http_body::Body<Data = bytes::Bytes>,
    B::Error: std::fmt::Debug,
{
    let bytes = r.into_body().collect().await.unwrap().to_bytes();
    String::from_utf8(bytes.to_vec()).unwrap()
}

/// Wait until the server's `n`th connection has finished its handshake.
///
/// It need not have happened by the time the caller holds the response —
/// that is the whole point of 0-RTT, and the ordering `docs/h3-research.md`
/// §3.2 measured, where the verdict landed 50 µs *after* the response body.
/// Here the relay widens that to the length of the hold.
///
/// A bounded wait rather than a sleep: on a fast host it returns in
/// microseconds, and on a slow one it does not become a flake.
async fn handshake_of(s: &server::Server, n: usize) -> Duration {
    for _ in 0..300 {
        if let Some(t) = s.timings().get(n).and_then(|t| t.handshake_done()) {
            return t;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("the server's connection {n} never completed its handshake");
}

#[tokio::test(flavor = "multi_thread")]
async fn early_data_is_accepted_and_the_wire_shows_it_leaving_before_the_handshake() {
    let s = server::start_watching_early_data(Behaviour::Echo);
    let wire = Wire::in_front_of(s.addr);
    let t = H3::new(
        TokioHandle::current().expect("inside #[tokio::test]"),
        server::client_tls(&s.cert_der),
        IpLiteralOnly,
    )
    .expect("H3::new does no I/O");

    // ---- Phase one: an ordinary visit, to be issued a ticket. ----
    //
    // It is also the negative control, and that is why it is asserted on
    // rather than just awaited: an unmarked request must put **nothing** in
    // early data, so the only difference between the two phases below is
    // the extension the caller wrote.
    let r = t
        .execute(padded_get(wire.addr, "/ticket", false))
        .await
        .expect("an ordinary h3 request");
    assert_eq!(r.status(), 200);
    let _ = body_of(r).await;
    // NewSessionTicket arrives after the handshake, on its own schedule.
    // Without this the second connection has nothing to resume from and the
    // test would pass vacuously through the `into_0rtt` refusal path — as
    // `live.rs`'s rejection test records for the same reason.
    tokio::time::sleep(Duration::from_millis(200)).await;

    let control = wire.client_sent();
    assert!(
        !control.iter().any(|p| p.kind == Kind::ZeroRtt),
        "an unmarked request must send no early data at all, or the phase \
         below would be measuring something that happens anyway: {control:?}"
    );

    // ---- Phase two: the same client, one marked request. ----
    wire.forget();
    wire.hold_server_flight(BACKSTOP);

    // The hold ends on the EVENT it exists to order against, not on a
    // clock: as soon as the server has resolved a request on this second
    // connection, the flight is let through. Until then the client's
    // handshake cannot complete, so a request resolved before the release
    // is a request resolved before the handshake — under any load, at any
    // speed. See `wire::Wire::release` for the measurement that made this
    // necessary rather than tidy.
    //
    // The test thread is about to block in `execute`, so the watcher has to
    // be somewhere else; it holds only the two `Arc`s it needs.
    let watcher = {
        let (releaser, timings) = (wire.releaser(), s.timings.clone());
        tokio::spawn(async move {
            for _ in 0..1000 {
                let resolved = timings
                    .lock()
                    .unwrap()
                    .get(1)
                    .and_then(|t| t.first_request())
                    .is_some();
                // Both, and the second half is not belt-and-braces. With
                // early data the request rides in the client's FIRST
                // flight, so the server can resolve it before its own reply
                // has reached the relay to be held at all — and a watcher
                // looking only at `resolved` then releases a hold that
                // caught nothing. The handshake was never delayed, the
                // ordering asserted below is a coincidence, and the
                // fixture's own guard says so: 3 runs in 20 failed under
                // `-j96` across the whole workspace.
                //
                // It must be `entered` and not `datagrams`. The first
                // attempt used `datagrams`, which the relay increments when
                // a wait *finishes* — so while it holds the flight the
                // count is zero, the condition can never become true, and
                // the hold runs to its backstop. That failed 3 times in 3,
                // which is the useful kind of wrong: a deadlock is visible
                // where a race is not.
                if resolved && releaser.held().entered > 0 {
                    releaser.release();
                    return true;
                }
                tokio::time::sleep(Duration::from_millis(2)).await;
            }
            // Let the exchange finish anyway, so the failure below is an
            // assertion with the numbers in it rather than a wedged suite.
            releaser.release();
            false
        })
    };

    let mut marked = padded_get(wire.addr, "/early", true);
    marked.extensions_mut().insert(AllowEarlyData);
    let started = std::time::Instant::now();
    let r = t.execute(marked).await.expect("a marked request");
    let answered = started.elapsed();
    let released_on_the_request = watcher.await.expect("the watcher does not panic");
    assert_eq!(r.status(), 200);
    assert_eq!(body_of(r).await, "hello over h3");

    // ---- What the wire saw. ----
    let sent = wire.client_sent();
    let first_early = sent.iter().position(|p| p.kind == Kind::ZeroRtt);
    let first_handshake = sent.iter().position(|p| p.kind == Kind::Handshake);
    let early_bytes: usize = sent
        .iter()
        .filter(|p| p.kind == Kind::ZeroRtt)
        .map(|p| p.len)
        .sum();
    println!(
        "0-RTT packets: {} carrying {early_bytes} bytes; first at index {first_early:?}, \
         first client Handshake packet at {first_handshake:?}; response after {answered:?}",
        sent.iter().filter(|p| p.kind == Kind::ZeroRtt).count(),
    );

    let first_early = first_early.expect(
        "no 0-RTT packet on the wire: the request did not go out in early data, \
         whatever either endpoint believes",
    );
    if let Some(h) = first_handshake {
        assert!(
            first_early < h,
            "the early data must precede the client's Finished, which is what \
             makes it early: 0-RTT at {first_early}, Handshake at {h}"
        );
    }
    assert!(
        early_bytes >= PADDING / 2,
        "only {early_bytes} bytes of early data: enough for h3's SETTINGS and \
         not for a {PADDING}-byte header section, so this proves the control \
         stream went early and says nothing about the request"
    );

    // ---- What the server saw. ----
    //
    // The marked request is on the server's second connection: the mark is
    // part of the pool key, so it cannot be served by the unmarked one.
    let handshake = handshake_of(&s, 1).await;
    let request = s.timings()[1]
        .first_request()
        .expect("the server answered, so it resolved a request");
    println!("server: request at {request:?}, handshake completed at {handshake:?}");

    // The fixture check — "the hold was not a no-op" — asked from the
    // relay, which is the only party that knows, and answered on the
    // relay's own clock.
    //
    // It used to read `handshake >= HOLD`, and that was **wrong by
    // construction rather than by margin**: `handshake` is measured from
    // the moment the SERVER accepted the connection, while `HOLD` runs from
    // the moment this test armed the relay — and between those two lie the
    // request being built, `execute` resolving and checking out, quinn
    // emitting an Initial, the datagram crossing the relay, and the
    // server's task starting. So the two numbers have different origins and
    // the recorded one is always the smaller, by an amount nobody bounded.
    // It failed about one run in three under load, at 146-150 ms against
    // 150. Widening the window would have been the wrong repair twice over:
    // it would have kept a comparison that cannot be right, and every
    // millisecond of tolerance is a millisecond in which a post-handshake
    // packet could start to count.
    let held = wire.held();
    println!(
        "relay: held {} server datagram(s), longest wait {:?}",
        held.datagrams, held.longest
    );
    assert!(
        held.datagrams > 0,
        "the relay forwarded every server datagram without waiting, so the \
         client's handshake was never held back and the separation below \
         would be a race rather than a guarantee"
    );
    assert!(
        released_on_the_request,
        "the hold ran to its {BACKSTOP:?} backstop: the server never resolved \
         a request while its handshake was outstanding, which is what \
         discarding the early data looks like from here"
    );

    // The claim, restated from the server's own record. It cannot fail once
    // the release above happened on the request — the handshake could not
    // complete while the flight was held — and it is asserted anyway,
    // because a fixture that stopped releasing on the event would otherwise
    // go on passing.
    assert!(
        request < handshake,
        "the server resolved the request at {request:?} and completed its \
         handshake at {handshake:?}: a request the server saw only after the \
         handshake is one whose early data it discarded"
    );
    assert_eq!(
        s.requests(),
        2,
        "one request per phase, and the marked one was not replayed"
    );
    assert_eq!(
        s.accepted(),
        2,
        "a marked request does not share the unmarked one's connection: \
         `enable_early_data` is a property of the rustls config, so it is \
         part of the pool key"
    );
}
