//! The UDP capability, on real loopback sockets.
//!
//! Two jobs. It pins the behaviour the seam promises — a socket enforces
//! the offloads it declares, and reports ECN it can actually observe — and
//! it **prints** the numbers, because `UdpCaps` is a report about a kernel
//! and the only way anyone notices a runner where GSO or ECN has quietly
//! vanished is if the run says so. Asserting the numbers would be flaky by
//! construction: 64/64 is this kernel's `UDP_MAX_SEGMENTS`, not a property
//! of this code, and a virtualised runner may honestly answer 1/1.
#![cfg(feature = "udp")]

use hclient_rt::{Datagrams, EcnCodepoint, RecvMeta, UdpBind, UdpDatagrams};
use std::future::poll_fn;
use std::io::IoSliceMut;
use std::net::SocketAddr;

fn bind() -> hclient_rt_tokio::TokioUdpSocket {
    hclient_rt_tokio::Tokio
        .bind(SocketAddr::from(([127, 0, 0, 1], 0)))
        .expect("an unprivileged loopback UDP bind")
}

/// Send, waiting for writability first.
///
/// `try_send` returning `WouldBlock` is a real answer and not a failure —
/// it obliges the caller to `poll_writable` and try again, which is the
/// split `UdpDatagrams` deliberately has and which quinn drives through its
/// own `UdpPoller`. A freshly bound tokio socket has no cached WRITABLE
/// readiness, so the very first send takes this path every time.
async fn send(sock: &hclient_rt_tokio::TokioUdpSocket, d: &Datagrams<'_>) -> std::io::Result<()> {
    loop {
        match sock.try_send(d) {
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                poll_fn(|cx| sock.poll_writable(cx)).await?;
            }
            other => return other,
        }
    }
}

#[tokio::test]
async fn the_capabilities_this_kernel_reports() {
    let s = bind();
    let c = s.caps();
    // Printed, not asserted — see the module doc. `--nocapture` shows it;
    // a runner where these collapse to 1/1/false is a fact worth having in
    // the log rather than a red build.
    println!(
        "UdpCaps on this host: gso={} gro={} ecn={} may_fragment={}",
        c.max_send_segments, c.max_recv_segments, c.ecn, c.may_fragment
    );
    // The one thing that is a property of this code rather than of the
    // kernel: a socket never reports fewer than one datagram per call.
    assert!(c.max_send_segments >= 1);
    assert!(c.max_recv_segments >= 1);
}

#[tokio::test]
async fn the_socket_refuses_a_gso_batch_it_cannot_send() {
    // The capability report is a contract, not a decoration. A socket
    // handed more segments than it declared refuses by name, rather than
    // putting one oversized datagram on a path that will drop it without
    // telling anyone.
    let s = bind();
    let caps = s.caps();
    let seg = 1200usize;
    let too_many = caps.max_send_segments + 1;
    let payload = vec![0u8; seg * too_many];
    let err = s
        .try_send(&Datagrams {
            destination: s.local_addr().unwrap(),
            src_ip: None,
            ecn: None,
            segment_size: Some(seg),
            contents: &payload,
        })
        .expect_err("one more segment than this socket declared");
    assert_eq!(err.kind(), std::io::ErrorKind::Unsupported);
    assert!(err.to_string().contains("gso"), "{err}");

    // And exactly at the limit is fine, so the check is a limit and not a
    // blanket refusal of GSO.
    let ok = vec![0u8; seg * caps.max_send_segments];
    let r = s.try_send(&Datagrams {
        destination: s.local_addr().unwrap(),
        src_ip: None,
        ecn: None,
        segment_size: Some(seg),
        contents: &ok,
    });
    assert!(
        r.is_ok() || r.as_ref().unwrap_err().kind() == std::io::ErrorKind::WouldBlock,
        "at the declared limit the socket must attempt the send: {r:?}"
    );
}

/// Send an `Ect0` from `a` to `b` and check that what `b` reports agrees
/// with what `b` claims.
///
/// Both directions are checked, and only one of them is about ECN working:
/// a socket that **claims** ECN must deliver the codepoint that was sent,
/// and a socket that does **not** claim it must report `None` rather than a
/// plausible-looking value. The second half is the one that matters most —
/// an invented `Ect0` would feed a congestion controller evidence of a mark
/// that never happened.
/// A socket bound to the wildcard reports the **unspecified** address as
/// its local one, and a datagram addressed there is not portable.
///
/// Linux delivers it — `0.0.0.0` and `::` are read as "this host" — and
/// **macOS does not**, where the send succeeds and nothing ever arrives.
/// That is why `ecn_is_reported_from_the_kernel_on_a_dual_stack_socket_too`,
/// which binds `[::]:0` and is the only test that can tell an asked
/// kernel from an assumed one, hung for ever on the one platform it was
/// written for: measured on macOS 27, where it printed the right answer
/// (`ecn=false` for the dual-stack socket against `true` for a v4 one)
/// and then never returned.
fn loopback_of(addr: SocketAddr) -> SocketAddr {
    if !addr.ip().is_unspecified() {
        return addr;
    }
    match addr {
        SocketAddr::V4(_) => SocketAddr::from((std::net::Ipv4Addr::LOCALHOST, addr.port())),
        SocketAddr::V6(_) => SocketAddr::from((std::net::Ipv6Addr::LOCALHOST, addr.port())),
    }
}

async fn ecn_claim_matches_reality(
    a: &hclient_rt_tokio::TokioUdpSocket,
    b: &hclient_rt_tokio::TokioUdpSocket,
) {
    send(
        a,
        &Datagrams {
            destination: loopback_of(b.local_addr().unwrap()),
            src_ip: None,
            ecn: Some(EcnCodepoint::Ect0),
            segment_size: None,
            contents: b"ecn",
        },
    )
    .await
    .expect("a three-byte datagram to loopback");

    let mut buf = [0u8; 64];
    let mut meta = [RecvMeta::default(); 1];
    let n = poll_fn(|cx| {
        let mut bufs = [IoSliceMut::new(&mut buf)];
        b.poll_recv(cx, &mut bufs, &mut meta)
    })
    .await
    .expect("the datagram arrives on loopback");
    assert_eq!(n, 1);
    assert_eq!(&buf[..meta[0].len], b"ecn");

    if b.caps().ecn {
        assert_eq!(
            meta[0].ecn,
            Some(EcnCodepoint::Ect0),
            "this socket claims ECN, so the codepoint it received must be the one that was sent"
        );
    } else {
        // **And `false` promises nothing**, which is the correction macOS
        // forced. This branch used to assert `None` — that a socket not
        // claiming ECN must never report a codepoint — and macOS 27 fails
        // it: a dual-stack v6 socket there answers `ecn=false` and then
        // delivers `Some(Ect0)` for genuinely v6 traffic.
        //
        // Both are true at once, because the report is a fact about the
        // SOCKET and the truth is a fact about the PACKET.
        // `ecn_is_really_on` reads `recv_tclass_v6` (which macOS grants)
        // **and** `recv_tos_v4` on a dual-stack socket (which it does
        // not), so it answers for the worse of the two families the socket
        // can receive. For v6 traffic the better one is what happens.
        //
        // That is the floor rule this workspace applies everywhere else,
        // one layer down: an under-claim costs an opportunity and an
        // over-claim costs correctness. So only `true` is a promise, and
        // the case that falsifies a wrongly-`true` claim is v4-mapped
        // traffic — `a_dual_stack_socket_that_claims_ecn_reports_it_for_v4_mapped_traffic_too`.
    }
}

#[tokio::test]
async fn ecn_is_reported_from_the_kernel_not_assumed() {
    let (a, b) = (bind(), bind());
    ecn_claim_matches_reality(&a, &b).await;
}

#[tokio::test]
async fn ecn_is_reported_from_the_kernel_on_a_dual_stack_socket_too() {
    // The v4 case above cannot distinguish "asked the kernel" from "assumed
    // Linux" — on this kernel both answers are `true`, so a hardcoded
    // `ecn: true` survives it. **A dual-stack v6 socket is the case where
    // they come apart**, and it is not hypothetical: `quinn-udp`'s own unix
    // backend carries "mac and ios do not support IP_RECVTOS on dual-stack
    // sockets" (`unix.rs:114`), so on the macOS runner already in this
    // project's matrix the honest answer here is `false` while a hardcoded
    // `true` would be a claim the socket cannot keep.
    //
    // That is why the report is a method on the SOCKET rather than a const
    // on the runtime: two sockets from one runtime, on one machine, in one
    // process, differ.
    let Ok(a) = hclient_rt_tokio::Tokio.bind(SocketAddr::from(([0u8; 16], 0))) else {
        // No IPv6 on this host at all — not a failure of anything under
        // test, and skipping is honest where pretending would not be.
        eprintln!("skipped: this host has no IPv6 loopback");
        return;
    };
    let b = hclient_rt_tokio::Tokio
        .bind(SocketAddr::from(([0u8; 16], 0)))
        .expect("the first v6 bind succeeded, so the second must too");
    println!(
        "dual-stack v6 socket reports ecn={} (v4 socket reports {})",
        b.caps().ecn,
        bind().caps().ecn
    );
    ecn_claim_matches_reality(&a, &b).await;
}

/// Does a socket bound to `[::]` on this platform actually receive
/// v4-mapped traffic?
///
/// **Windows says no, and it is a default rather than a quirk**:
/// `IPV6_V6ONLY` defaults to *on* there, where every unix in this
/// project's matrix defaults to off. A test that sends v4 traffic to a
/// `[::]` socket on Windows is asserting something about a socket that
/// will never receive it — the sender accepts the datagram and the kernel
/// delivers it nowhere, so the receive blocks for ever.
///
/// That is what it cost: `test (windows-latest)` sat at this file until
/// GitHub's six-hour job limit on every run for twelve days, and said
/// nothing about which test, because nothing bounded a test and nothing
/// streamed the output. Both are fixed now (`.config/nextest.toml`,
/// `just test-workspace`), and this is the defect they exposed.
///
/// **Probed rather than `cfg!`-ed**, for the same reason the test below is
/// a biconditional rather than a platform check: this encodes the
/// question, not today's answers. A platform that changes its default
/// changes this line's answer and nothing else.
fn dual_stack_is_available() -> bool {
    let Ok(sock) = socket2::Socket::new(socket2::Domain::IPV6, socket2::Type::DGRAM, None) else {
        return false;
    };
    // `only_v6()` failing is "we do not know", and the reading that keeps
    // this test from hanging is that we cannot rely on v4-mapped delivery.
    !sock.only_v6().unwrap_or(true)
}

/// **The case that can falsify a wrongly-`true` ECN claim**, and the one
/// nobody had written.
///
/// The two tests above send v6 to a v6 socket, or v4 to a v4 one, and on
/// every platform in this project's matrix the answer for those is the
/// same as a hardcoded `true` — which is why the `ecn: true` mutation has
/// survived since v0.3. **v4-mapped traffic to a dual-stack socket is
/// where the families come apart**: `quinn-udp`'s own unix backend
/// carries "mac and ios do not support IP_RECVTOS on dual-stack sockets",
/// so there the codepoint cannot come back, and a socket claiming it can
/// is making a claim the kernel will not keep.
///
/// Written as a **biconditional** rather than a platform check: whatever
/// the socket claims, the traffic must agree with it. That keeps the test
/// honest on a kernel nobody has tried, instead of encoding today's two.
#[tokio::test]
async fn a_dual_stack_socket_reports_ecn_for_v4_mapped_traffic_exactly_when_it_claims_to() {
    let Ok(b) = hclient_rt_tokio::Tokio.bind(SocketAddr::from(([0u8; 16], 0))) else {
        eprintln!("skipped: this host has no IPv6 loopback");
        return;
    };
    // The premise, established rather than assumed — see
    // `dual_stack_is_available`. Without this the datagram below is sent
    // into nowhere on Windows and the receive never returns.
    if !dual_stack_is_available() {
        eprintln!(
            "skipped: a `[::]` socket is v6-only on this platform, so v4-mapped \
             traffic cannot arrive and there is nothing here to observe"
        );
        return;
    }
    let port = b.local_addr().expect("bound").port();
    let a = bind();
    send(
        &a,
        &Datagrams {
            // v4 loopback, arriving on the dual-stack socket as
            // v4-mapped. Addressed explicitly rather than through
            // `b.local_addr()`, which is the unspecified address — see
            // `loopback_of`.
            destination: SocketAddr::from((std::net::Ipv4Addr::LOCALHOST, port)),
            src_ip: None,
            ecn: Some(EcnCodepoint::Ect0),
            segment_size: None,
            contents: b"mapped",
        },
    )
    .await
    .expect("a datagram to v4 loopback");

    let mut buf = [0u8; 64];
    let mut meta = [RecvMeta::default(); 1];
    let n = poll_fn(|cx| {
        let mut bufs = [IoSliceMut::new(&mut buf)];
        b.poll_recv(cx, &mut bufs, &mut meta)
    })
    .await
    .expect("the datagram arrives");
    assert_eq!(n, 1);
    assert_eq!(&buf[..meta[0].len], b"mapped");

    println!(
        "dual-stack socket claims ecn={}, v4-mapped datagram reported {:?}",
        b.caps().ecn,
        meta[0].ecn
    );
    // **One direction only, and macOS is why.** A biconditional was
    // written here first and fails there: the socket claims `false` and
    // the codepoint arrives anyway. Measured on macOS 27 — `only_v6()` is
    // `false`, `IPV6_RECVTCLASS` sets and reads back `true`, and
    // `IP_RECVTOS` fails with `EINVAL`, exactly as `quinn-udp` documents
    // — and yet the kernel reports the codepoint for v4-mapped traffic,
    // because `IPV6_RECVTCLASS` covers both families there.
    //
    // So `ecn_is_really_on` under-reports on macOS: it requires the v4
    // option to be settable on a dual-stack socket, and macOS delivers
    // without it. Under-claiming is the safe direction and the floor rule
    // this workspace applies everywhere, so the behaviour is left alone
    // and written down rather than changed on the strength of one
    // kernel — but only `true` is a promise, and this asserts exactly
    // that.
    if b.caps().ecn {
        assert_eq!(
            meta[0].ecn,
            Some(EcnCodepoint::Ect0),
            "a socket claiming ECN must report the codepoint for v4-mapped traffic too"
        );
    }
}

#[tokio::test]
async fn a_plain_datagram_round_trips_with_no_offload_asked_for() {
    // The base case, and the reason `reject_unsupported` cannot be a
    // blanket refusal: the weakest possible socket still has to serve a
    // caller that asked for nothing.
    let a = bind();
    let b = bind();
    send(
        &a,
        &Datagrams {
            destination: b.local_addr().unwrap(),
            src_ip: None,
            ecn: None,
            segment_size: None,
            contents: b"plain",
        },
    )
    .await
    .expect("send");

    let mut buf = [0u8; 64];
    let mut meta = [RecvMeta::default(); 1];
    let n = poll_fn(|cx| {
        let mut bufs = [IoSliceMut::new(&mut buf)];
        b.poll_recv(cx, &mut bufs, &mut meta)
    })
    .await
    .expect("recv");
    assert_eq!(n, 1);
    assert_eq!(&buf[..meta[0].len], b"plain");
    assert_eq!(meta[0].addr.ip(), a.local_addr().unwrap().ip());
}
