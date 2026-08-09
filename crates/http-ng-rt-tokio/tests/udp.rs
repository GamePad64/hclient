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

#[tokio::test]
async fn ecn_is_reported_from_the_kernel_not_assumed() {
    // A socket that CLAIMS ECN must actually carry it: send an `Ect0` to
    // ourselves and read the codepoint back off the receive metadata.
    //
    // The mutation this kills is `caps.ecn = true` hardcoded — on a kernel
    // without `IP_RECVTOS` the claim would then be false and this test
    // fails. **On a kernel that has it, the mutant survives**, which is a
    // property of the environment rather than of the test: see
    // `docs/v03-acceptance.md`. What it does pin everywhere is the other
    // direction — a socket that reports `ecn: false` must not be silently
    // fabricating codepoints on the way in.
    let a = bind();
    let b = bind();
    let to = b.local_addr().unwrap();

    send(
        &a,
        &Datagrams {
            destination: to,
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
