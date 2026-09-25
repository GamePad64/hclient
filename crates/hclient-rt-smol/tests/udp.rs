//! The UDP capability on smol, on real loopback sockets.
//!
//! **The sibling of `crates/hclient-rt-tokio/tests/udp.rs`, and it did not
//! exist.** `hclient-rt-smol`'s `udp.rs` opens by saying why the second
//! implementation of the seam is worth having — *a seam with one
//! implementation is a design* — and then had no test of its own: all 15
//! of its mutants survived a `cargo mutants -p hclient-rt-smol` sweep,
//! because the only thing exercising this file was
//! `hclient-rt-pair-check`'s UDP pair property, which a crate-scoped sweep
//! does not run. A property asserted only from another crate is a property
//! this crate's own suite cannot lose, which is the shape this workspace
//! records about `test-doc` and `test-no-default`: the check existed and
//! nothing local called it.
//!
//! Numbers a kernel chooses are **printed rather than asserted**, for the
//! tokio sibling's stated reason: 64/64 is this host's `UDP_MAX_SEGMENTS`
//! and a virtualised runner may honestly answer 1/1, so asserting them
//! would be flaky by construction.
#![cfg(feature = "udp")]

use hclient_rt::{Datagrams, EcnCodepoint, RecvMeta, UdpBind, UdpDatagrams};
use hclient_rt_smol::{Smol, SmolUdpSocket};
use std::future::poll_fn;
use std::io::IoSliceMut;
use std::net::SocketAddr;

fn bind() -> SmolUdpSocket {
    Smol.bind(SocketAddr::from(([127, 0, 0, 1], 0)))
        .expect("an unprivileged loopback UDP bind")
}

/// Send, waiting for writability first.
///
/// `try_send` answering `WouldBlock` is a real answer rather than a
/// failure — it obliges the caller to `poll_writable` and try again, which
/// is the split `UdpDatagrams` deliberately has. This backend's own module
/// doc records that, unlike tokio's, it **never** takes that path on a
/// first send, because `async-io` caches no readiness to refuse against;
/// the loop is here so the helper is correct on either runtime rather than
/// because this one needs it.
async fn send(sock: &SmolUdpSocket, d: &Datagrams<'_>) -> std::io::Result<()> {
    loop {
        match sock.try_send(d) {
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                poll_fn(|cx| sock.poll_writable(cx)).await?;
            }
            other => return other,
        }
    }
}

/// One datagram, with a bound so a regression that makes `poll_recv` never
/// resolve reports a failure rather than wedging the binary — the bound
/// `adversarial_smol_connect.rs` applies to every wait, for its reason.
async fn recv_one(sock: &SmolUdpSocket, buf: &mut [u8]) -> std::io::Result<(usize, RecvMeta)> {
    let mut meta = [RecvMeta::default(); 1];
    let n = futures_lite::future::or(
        poll_fn(|cx| {
            let mut bufs = [IoSliceMut::new(buf)];
            sock.poll_recv(cx, &mut bufs, &mut meta)
        }),
        async {
            async_io::Timer::after(std::time::Duration::from_secs(10)).await;
            Err(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "no datagram within 10s — treating a stall as a regression",
            ))
        },
    )
    .await?;
    Ok((n, meta[0]))
}

// **There is deliberately no test of `poll_writable` here, and the
// measurement is why.** A test was written — ask it directly on a bound
// socket and require it to resolve — and then deleted, because it does not
// discriminate: it passed with `poll_writable`'s body replaced by
// `Poll::Ready(Ok(()))` (7 of 7, with the mutation applied by hand), which
// is the mutation it existed to kill. A test that cannot fail is worse
// than none.
//
// Two measurements say the mutant is unkillable from this crate rather
// than merely unpinned. `poll_writable` is called **0 times** across every
// send in this file — counted with an `eprintln!` in its body — because
// this backend's own module doc records that `async-io` caches no
// readiness to refuse against, so a loopback `try_send` never answers
// `WouldBlock` and the retry arm above is never taken. And the only other
// observable, a `Pending` that registers a waker, needs a socket that is
// **not** writable: driving one there was tried and failed — 20,000
// 1400-byte sends into an unread peer with `SO_SNDBUF` at 4608 never
// blocked once, because a loopback UDP send drops rather than queues.
//
// So the honest statement is that `poll_writable` is exercised where it is
// actually used — `hclient-rt-pair-check`'s UDP pair property and quinn's
// own `UdpPoller` — and that a local test would be decoration. This is the
// same shape as the two `ecn_is_really_on` mutants below: not a gap, an
// observable this host cannot reach.

#[test]
fn the_capabilities_this_kernel_reports() {
    let s = bind();
    let c = s.support();
    println!(
        "UdpSupport on this host: gso={} gro={} ecn={} may_fragment={}",
        c.max_send_segments, c.max_recv_segments, c.ecn, c.may_fragment
    );
    // The one part that is a property of this code rather than of the
    // kernel: a socket never reports fewer than one datagram per call.
    assert!(c.max_send_segments >= 1);
    assert!(c.max_recv_segments >= 1);
}

/// A datagram sent through this backend arrives, with its bytes and its
/// source intact.
///
/// The floor `try_send`, `poll_writable` and `poll_recv` all sit on: with
/// any one of the three replaced by a success that moves no bytes, nothing
/// arrives and this fails. Measured — each of those three mutations turns
/// exactly this test red.
#[test]
fn a_datagram_round_trips_through_the_seam() {
    futures_executor::block_on(async {
        let (a, b) = (bind(), bind());
        let to = b.local_addr().expect("local_addr");
        send(
            &a,
            &Datagrams::new(to, b"smol-udp")
                .src_ip(None)
                .ecn(None)
                .segment_size(None),
        )
        .await
        .expect("an eight-byte datagram to loopback");

        let mut buf = [0u8; 64];
        let (n, meta) = recv_one(&b, &mut buf).await.expect("the datagram arrives");
        assert_eq!(n, 1, "one datagram was sent, so one is reported");
        assert_eq!(
            &buf[..meta.len],
            b"smol-udp",
            "the bytes that arrive are the bytes that were sent"
        );
        assert_eq!(
            meta.addr,
            a.local_addr().expect("sender local_addr"),
            "the datagram is attributed to the socket that sent it"
        );
    });
}

/// `poll_recv` asked for zero slots answers zero **without touching the
/// socket**, and asked for slots answers the datagram.
///
/// The pair is the assertion. `slots == 0` is an early return whose
/// mutation to `slots != 0` is invisible to any test that only ever asks
/// for one slot: with the comparison inverted, a one-slot call returns
/// `Ok(0)` and a zero-slot call runs the receive loop. So the zero case
/// has to be asked for explicitly, and the non-zero case has to be right
/// beside it — either alone passes under the mutation.
#[test]
fn zero_slots_answers_zero_and_does_not_consume_the_datagram() {
    futures_executor::block_on(async {
        let (a, b) = (bind(), bind());
        let to = b.local_addr().expect("local_addr");
        send(
            &a,
            &Datagrams::new(to, b"kept")
                .src_ip(None)
                .ecn(None)
                .segment_size(None),
        )
        .await
        .expect("send");

        // Zero slots: an answer, not a wait, and not a read.
        let mut meta: [RecvMeta; 0] = [];
        let mut bufs: [IoSliceMut<'_>; 0] = [];
        let n = poll_fn(|cx| b.poll_recv(cx, &mut bufs, &mut meta)).await;
        assert_eq!(
            n.expect("zero slots is an answer rather than an error"),
            0,
            "no slots means nothing can be reported"
        );

        // And the datagram is still there, which is what says the early
        // return did not consume it.
        let mut buf = [0u8; 64];
        let (got, m) = recv_one(&b, &mut buf).await.expect("still deliverable");
        assert_eq!(got, 1);
        assert_eq!(&buf[..m.len], b"kept");
    });
}

/// `Datagrams::reject_unsupported` is consulted, so a GSO batch beyond what
/// this socket reports is refused rather than truncated.
///
/// The capability report is a contract: a batch past it would otherwise
/// leave one oversized datagram on a path that drops it silently. Skipped
/// where the kernel offers no GSO at all, because there is then no "beyond"
/// to ask for — and saying so is better than an assertion that passes
/// vacuously on such a host.
#[test]
fn the_socket_refuses_a_gso_batch_it_cannot_send() {
    futures_executor::block_on(async {
        let (a, b) = (bind(), bind());
        let max = a.support().max_send_segments;
        if max <= 1 {
            println!("this kernel reports max_send_segments={max}; nothing to exceed");
            return;
        }
        let to = b.local_addr().expect("local_addr");
        // One segment more than the socket says it can send.
        let seg = 8usize;
        let contents = vec![b'x'; seg * (max + 1)];
        let err = a
            .try_send(
                &Datagrams::new(to, &contents)
                    .src_ip(None)
                    .ecn(None)
                    .segment_size(Some(seg)),
            )
            .expect_err("a batch beyond the reported maximum must be refused");
        println!("refused a {}-segment batch (max {max}): {err}", max + 1);
    });
}

/// A socket that **claims** ECN reports the codepoint that was sent.
///
/// One-directional on purpose, and that is the workspace's recorded
/// finding rather than a convenience: only a `true` claim is a promise.
/// `ecn_is_really_on` under-reports on macOS — it asks for `IP_RECVTOS` on
/// a dual-stack socket, which that kernel does not grant although
/// `IPV6_RECVTCLASS` already covers both families — so a `false` claim
/// there sits beside a socket that does report marks. An under-claim costs
/// an opportunity and an over-claim costs correctness, so this asserts the
/// direction that is a promise and prints the other.
#[test]
fn a_socket_that_claims_ecn_reports_the_codepoint_it_received() {
    futures_executor::block_on(async {
        let (a, b) = (bind(), bind());
        let to = b.local_addr().expect("local_addr");
        send(
            &a,
            &Datagrams::new(to, b"ecn")
                .src_ip(None)
                .ecn(Some(EcnCodepoint::Ect0))
                .segment_size(None),
        )
        .await
        .expect("a three-byte datagram to loopback");

        let mut buf = [0u8; 64];
        let (n, meta) = recv_one(&b, &mut buf).await.expect("the datagram arrives");
        assert_eq!(n, 1);
        assert_eq!(&buf[..meta.len], b"ecn");

        if b.support().ecn {
            assert_eq!(
                meta.ecn,
                Some(EcnCodepoint::Ect0),
                "this socket claims ECN, so the codepoint it received must be the one sent"
            );
        } else {
            println!("this socket reports ecn=false; a false claim promises nothing");
        }
    });
}

/// The ECN claim is read back from the kernel per socket, not decided once.
///
/// **The discriminator is a v6-only socket**, and finding that took
/// measuring rather than reading. After `quinn_udp::UdpSocketState::new`
/// has set what it can, this host answers:
///
/// | bind | `recv_tos_v4` | `recv_tclass_v6` | `only_v6` |
/// |---|---|---|---|
/// | `127.0.0.1:0` | `Ok(true)` | `Err(ENOTSUP)` | `Err(ENOTSUP)` |
/// | `[::]:0` | `Ok(true)` | `Ok(true)` | `Ok(false)` |
/// | `[::1]:0` | **`Ok(false)`** | `Ok(true)` | `Ok(true)` |
///
/// So the v4 and dual-stack sockets answer `true` under the original *and*
/// under three of the mutations of `ecn_is_really_on`'s v6 arm, and only
/// the **v6-only** row separates them: it is the one shape where
/// `recv_tos_v4` is `false` while the socket legitimately reports ECN,
/// because no v4-mapped traffic can reach it. Measured with each mutation
/// applied by hand — `dual = only_v6()` (the `!` deleted), `!dual && tos4`,
/// and `dual || tos4` each make this socket answer `false`, and each leaves
/// the other two shapes answering `true`.
///
/// Asserted as `true` rather than as "whatever the kernel says", because
/// on a kernel granting `IPV6_RECVTCLASS` — which every platform this arm
/// compiles for does — a socket that cannot receive v4-mapped traffic has
/// nothing left to disclaim. A kernel that refuses the option is the one
/// case this cannot hold on, and it is the same case the two remaining
/// `ecn_is_really_on` mutants are unkillable in.
///
/// **What this does not kill, stated because the assertion's direction
/// invites the opposite reading**: `ecn_is_really_on -> true` for the
/// whole function. This test asserts `ecn` *is* `true`, so a mutant
/// forcing that answer satisfies it rather than failing it — measured,
/// 39/39 green with an early `return true` in place. What is pinned is
/// the `!dual` short-circuit **inside** the v6 arm, verified by
/// `v6 && (dual && tos4)`, which fails exactly this test and nothing
/// else. The distinction matters because AGENTS.md records a *different*
/// ECN mutant — the hardcoded `ecn: true` in `UdpSupport` — as unkillable on
/// every platform, and a reader who merges the two concludes no ECN
/// mutation is worth a test.
/// The socket is built here and **adopted** rather than bound, because
/// `only_v6` has to be established before this crate sees the descriptor
/// and `SmolUdpSocket` exposes no accessor to read it back — deliberately,
/// and widening that surface for a test is the wrong direction. `adopt` is
/// the seam's own entry point for a caller-configured socket, so this
/// exercises it as well.
///
/// **Not on Windows**, where the probe cannot see the option `quinn-udp`
/// sets there and so answers `false` for every socket — see
/// `ecn_is_really_on`'s own doc. The claim this pins is about the v6 arm's
/// logic, and on Windows no arm runs.
#[test]
#[cfg_attr(
    windows,
    ignore = "the ECN probe reads IPV6_RECVTCLASS, and quinn-udp sets IPV6_RECVECN on Windows"
)]
fn a_v6_only_socket_claims_ecn_although_it_grants_no_v4_recvtos() {
    use hclient_rt::UdpAdoptStd;
    let Ok(raw) = socket2::Socket::new(socket2::Domain::IPV6, socket2::Type::DGRAM, None) else {
        println!("no IPv6 support on this host; nothing to measure");
        return;
    };
    if raw.set_only_v6(true).is_err() {
        println!("IPV6_V6ONLY is not settable here; the discriminating shape is unavailable");
        return;
    }
    let addr: SocketAddr = "[::1]:0".parse().expect("a v6 loopback literal");
    if raw.bind(&addr.into()).is_err() {
        println!("no IPv6 loopback on this host; nothing to measure");
        return;
    }
    // The precondition this test exists for, read back rather than assumed:
    // if the socket is not actually v6-only the assertion below would be
    // measuring the dual-stack row instead and would pass for the wrong
    // reason.
    if !raw.only_v6().unwrap_or(false) {
        println!("this socket is not v6-only; the discriminating shape is unavailable");
        return;
    }
    let s = Smol
        .adopt(std::net::UdpSocket::from(raw))
        .expect("adopting a bound v6-only socket");
    println!("v6-only socket reports ecn={}", s.support().ecn);
    assert!(
        s.support().ecn,
        "a v6-only socket receives no v4-mapped traffic, so `IP_RECVTOS` being \
         unset cannot disclaim ECN — the v6 arm must not require it"
    );
}

// **Three `poll_recv` mutants are left alive deliberately**, and all
// three are the same observable: the `WouldBlock` arm of its receive loop,
// which on this backend is never taken. Counted with an `eprintln!` in that
// arm — **0** hits across every test in this crate, and **0** again under a
// purpose-built race with four threads polling one socket against 200
// queued datagrams, which is the shape that arm exists for (readiness
// reported, then the datagram taken by someone else before the `recv`).
//
// The reason is the one this backend's own module doc gives from the send
// side: `async-io` caches no readiness. `Source::poll_ready` answers
// `Ready` only when the reactor's tick has moved past the one recorded at
// the caller's last `Pending`, and re-arms on every registration — so a
// poller that loses the race is told `Pending` by `poll_readable` rather
// than being waved through to a `recv` that finds nothing.
//
// **A consumer outside the reactor does not reach it either**, which this
// note used to name as the missing instrument. It was built — a socket
// adopted through `UdpAdoptStd` with a clone of its descriptor kept back,
// polled once to register, sent a datagram, the datagram taken through the
// clone, then polled again — and it answers `Pending` under the original
// *and* under `guard -> false`, so the arm is not entered. The same test
// kills both mutants on `hclient-rt-tokio`, whose reactor does cache
// readiness (`readiness_for_a_datagram_someone_else_took_is_waited_out`
// there). Here it would be a test that cannot fail, so it is not kept.
//
// So the arm is right to exist — `quinn` drives this socket from several
// tasks, and the tokio twin genuinely takes it — and it is not pinnable
// from here: an observable this backend cannot reach is not a gap.

// `poll_writable -> Ready(Ok(()))` stays alive, for the reason recorded
// above the capability test at the head of this file: a loopback UDP send
// drops rather than queues, so no socket here is ever unwritable.

/// The descriptor `AsFd`/`AsSocket` hands out is this socket's own: read
/// through `socket2`, it names the address the socket reports for itself.
/// A descriptor for some other socket would name another port.
#[test]
fn the_descriptor_is_the_sockets_own() {
    let s: SmolUdpSocket = Smol
        .bind(SocketAddr::from(([127, 0, 0, 1], 0)))
        .expect("bind");
    let via_fd = socket2::SockRef::from(&s)
        .local_addr()
        .expect("local_addr through the descriptor")
        .as_socket()
        .expect("an IP socket");
    assert_eq!(via_fd, s.local_addr().expect("local_addr"));
}
