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

use http_ng_rt::{Datagrams, EcnCodepoint, RecvMeta, UdpBind, UdpDatagrams};
use std::io::IoSliceMut;
use std::net::SocketAddr;

fn bind() -> http_ng_rt_tokio::TokioUdpSocket {
    http_ng_rt_tokio::Tokio
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
async fn send(sock: &http_ng_rt_tokio::TokioUdpSocket, d: &Datagrams<'_>) -> std::io::Result<()> {
    loop {
        match sock.try_send(d) {
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                std::future::poll_fn(|cx| sock.poll_writable(cx)).await?;
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
async fn ecn_claim_matches_reality(
    a: &http_ng_rt_tokio::TokioUdpSocket,
    b: &http_ng_rt_tokio::TokioUdpSocket,
) {
    send(
        a,
        &Datagrams {
            destination: b.local_addr().unwrap(),
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
    let n = std::future::poll_fn(|cx| {
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
        assert_eq!(
            meta[0].ecn, None,
            "a socket that does not claim ECN must report the absence, never a plausible value"
        );
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
    let Ok(a) = http_ng_rt_tokio::Tokio.bind(SocketAddr::from(([0u8; 16], 0))) else {
        // No IPv6 on this host at all — not a failure of anything under
        // test, and skipping is honest where pretending would not be.
        eprintln!("skipped: this host has no IPv6 loopback");
        return;
    };
    let b = http_ng_rt_tokio::Tokio
        .bind(SocketAddr::from(([0u8; 16], 0)))
        .expect("the first v6 bind succeeded, so the second must too");
    println!(
        "dual-stack v6 socket reports ecn={} (v4 socket reports {})",
        b.caps().ecn,
        bind().caps().ecn
    );
    ecn_claim_matches_reality(&a, &b).await;
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
    let n = std::future::poll_fn(|cx| {
        let mut bufs = [IoSliceMut::new(&mut buf)];
        b.poll_recv(cx, &mut bufs, &mut meta)
    })
    .await
    .expect("recv");
    assert_eq!(n, 1);
    assert_eq!(&buf[..meta[0].len], b"plain");
    assert_eq!(meta[0].addr.ip(), a.local_addr().unwrap().ip());
}
