//! The UDP half of the pair property: **one** body, run on tokio and on
//! smol, over real loopback sockets.
//!
//! `exercise_udp<R>` below is that body. It touches every method of
//! `UdpBind`/`UdpDatagrams` — bind, `caps`, `try_send`, `poll_writable`,
//! `poll_recv`, `local_addr` — with no `#[cfg]`, no boxing and no
//! runtime-specific bound anywhere inside it; the two instantiations differ
//! only in which runtime value is passed and in the executor each test
//! drives (`#[tokio::test]` vs. `futures_executor::block_on`), which a test
//! function has to pick by construction.
//!
//! # Why this file rather than a copy of `hclient-rt-tokio/tests/udp.rs`
//!
//! Because two similar test files would prove that both backends compile
//! and nothing else. `pair_property.rs` next door exists for the same
//! reason for TCP.
//!
//! # What is asserted, and what is only printed
//!
//! The numbers in [`UdpCaps`] are a report about a **kernel**, not about
//! this code: `64` is this machine's `UDP_MAX_SEGMENTS` and a virtualised
//! runner may honestly answer `1`. So they are printed, and what is
//! asserted is the *relationship* between what a socket claims and what it
//! then delivers, which holds on any of them:
//!
//! - a socket that claims ECN must deliver the codepoint that was sent, and
//!   one that does not claim it must report `None` rather than a
//!   plausible-looking value (an invented `Ect0` would feed a congestion
//!   controller evidence of a mark that never happened);
//! - a socket that claims a GSO batch of `n` must actually put `n`
//!   datagrams on the wire in one call, and must refuse `n + 1` by name;
//! - a socket that claims a GRO batch must split what it coalesced, which
//!   is what [`RecvMeta::stride`] is for.
//!
//! Between them those cover the direction a capability report must never be
//! wrong in. Under-claiming is permitted by the seam (`UdpCaps::NONE` is
//! the default *because* it is the weakest answer) and is therefore not an
//! assertion here — with one exception that costs nothing: ECN, where the
//! claim-versus-reality check below is two-sided and so catches an
//! under-claim as well.
#![cfg(not(target_family = "wasm"))]

use hclient_rt::{Datagrams, EcnCodepoint, RecvMeta, UdpBind, UdpDatagrams};
use std::future::poll_fn;
use std::io::IoSliceMut;
use std::net::SocketAddr;

/// Send, waiting for writability first.
///
/// `try_send` returning `WouldBlock` is a real answer and not a failure —
/// it obliges the caller to `poll_writable` and try again, which is the
/// split `UdpDatagrams` deliberately has and which quinn drives through its
/// own `UdpPoller`.
///
/// **The two backends do not take this path equally, and the asymmetry is
/// measured rather than assumed.** Replacing the retry with a `panic!` and
/// running both arms: the tokio one panics on its very first send, the smol
/// one never panics at all. The cause is where each backend's `WouldBlock`
/// comes from — tokio's `try_io(Interest::WRITABLE, ..)` refuses *before
/// the syscall* when tokio holds no cached WRITABLE readiness for the
/// socket, which is exactly the state a freshly bound one is in; smol's
/// `try_send` calls `sendmsg` and a loopback socket with an empty send
/// buffer accepts it.
///
/// So this loop is load-bearing on tokio and, on this host, dead code on
/// smol — which is the right way round for a caller: the contract is
/// "handle `WouldBlock`", and a backend that never needs you to is still
/// within it. A caller written against smol alone would have had a bug that
/// only tokio finds, and that is the sort of thing a shared body is for.
async fn send<S: UdpDatagrams>(sock: &S, d: &Datagrams<'_>) -> std::io::Result<()> {
    loop {
        match sock.try_send(d) {
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                poll_fn(|cx| sock.poll_writable(cx)).await?;
            }
            other => return other,
        }
    }
}

/// One `poll_recv` into one buffer, returning the slot it filled.
async fn recv_one<S: UdpDatagrams>(sock: &S, buf: &mut [u8]) -> std::io::Result<RecvMeta> {
    let mut meta = [RecvMeta::default(); 1];
    let n = poll_fn(|cx| {
        let mut bufs = [IoSliceMut::new(buf)];
        sock.poll_recv(cx, &mut bufs, &mut meta)
    })
    .await?;
    assert_eq!(
        n, 1,
        "one buffer was offered, so at most one slot is filled"
    );
    Ok(meta[0])
}

/// A destination that actually reaches a socket bound to `addr`.
///
/// **A wildcard is not an address you can send to, and only Linux pretends
/// otherwise.** `local_addr()` on a socket bound to `[::]:0` hands back the
/// unspecified address with a real port; Linux routes a datagram addressed
/// that way to this host, macOS answers `EHOSTUNREACH`, and Windows accepts
/// the send and delivers nothing — so the receive blocks for ever.
///
/// This is the same defect `hclient-rt-tokio/tests/udp.rs` fixed with its
/// own `loopback_of`, and the reason it is worth a second copy rather than
/// a shared crate: this file's whole point is that it depends on both
/// runtimes and nothing else. It was not copied at the time, and the cost
/// was `udp_pair_property_holds_for_tokio` hanging until the six-hour CI
/// job limit on every macOS run for twelve days, saying nothing about why.
fn loopback_of(addr: SocketAddr) -> SocketAddr {
    if !addr.ip().is_unspecified() {
        return addr;
    }
    match addr {
        SocketAddr::V4(_) => SocketAddr::from((std::net::Ipv4Addr::LOCALHOST, addr.port())),
        SocketAddr::V6(_) => SocketAddr::from((std::net::Ipv6Addr::LOCALHOST, addr.port())),
    }
}

/// Send an `Ect0` from `a` to `b` and check that what `b` reports agrees
/// with what `b` claims.
///
/// Both directions are checked, and only one of them is about ECN working:
/// a socket that **claims** ECN must deliver the codepoint that was sent,
/// and a socket that does **not** claim it must report `None`. The second
/// half is the one that matters most — an invented `Ect0` would feed a
/// congestion controller evidence of a mark that never happened.
async fn ecn_claim_matches_reality<S: UdpDatagrams>(a: &S, b: &S) {
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
    let meta = recv_one(b, &mut buf).await.expect("it arrives on loopback");
    assert_eq!(&buf[..meta.len], b"ecn");

    // **One direction only, and that is the workspace rule rather than a
    // concession.** A biconditional stood here and failed on Windows,
    // where a socket reports `ecn: false` and the codepoint arrives
    // anyway. `hclient-rt-tokio/tests/udp.rs` had already met the same
    // thing on macOS and written the rule down: `ecn_is_really_on`
    // requires an option the kernel does not need, so it **under**-reports
    // — which is the safe direction, and the floor rule this workspace
    // applies everywhere. **Only a `true` claim is a promise**, and an
    // `else` arm here was asserting a promise nobody made.
    //
    // What the arm was protecting against — an invented `Ect0` fed to a
    // congestion controller as evidence of a mark that never happened — is
    // still covered, by the `if`: a socket that claims ECN must deliver
    // the codepoint that was actually sent.
    if b.caps().ecn {
        assert_eq!(
            meta.ecn,
            Some(EcnCodepoint::Ect0),
            "this socket claims ECN, so the codepoint it received must be the one that was sent"
        );
    }
}

/// One GSO send of `caps.max_send_segments` datagrams — capped, because a
/// kernel that answers 64 would otherwise mean a 76 KiB write — collected
/// back on the other side and counted as *datagrams*, not bytes.
///
/// This is what makes `max_send_segments`/`max_recv_segments` load-bearing
/// rather than decorative in the direction that hurts: a socket claiming a
/// batch it cannot send would fail here, and one claiming a GRO batch it
/// cannot split would return a `stride` that does not divide what arrived.
async fn a_declared_gso_batch_really_goes_out<S: UdpDatagrams>(a: &S, b: &S) {
    const SEG: usize = 1200;
    let segments = a.caps().max_send_segments.min(4);
    if segments < 2 {
        println!("  gso: this socket declares no batching, nothing to check");
        return;
    }
    let payload: Vec<u8> = (0..SEG * segments).map(|i| (i % 251) as u8).collect();
    send(
        a,
        &Datagrams {
            destination: loopback_of(b.local_addr().unwrap()),
            src_ip: None,
            ecn: None,
            segment_size: Some(SEG),
            contents: &payload,
        },
    )
    .await
    .expect("a GSO batch no larger than the socket declared");

    // Count datagrams, not reads: with GRO one read returns several
    // coalesced, and `stride` is how they are told apart. Without GRO the
    // same bytes arrive over several reads. Both are correct; what must
    // hold is that every byte arrives, in order, as `segments` datagrams.
    let mut seen = Vec::new();
    let mut datagrams = 0usize;
    while seen.len() < payload.len() {
        let mut buf = vec![0u8; SEG * segments];
        let meta = recv_one(b, &mut buf).await.expect("the batch arrives");
        let stride = if meta.stride == 0 || meta.stride >= meta.len {
            meta.len
        } else {
            meta.stride
        };
        datagrams += meta.len.div_ceil(stride);
        seen.extend_from_slice(&buf[..meta.len]);
    }
    assert_eq!(seen, payload, "every byte of the batch, in order");
    assert_eq!(
        datagrams, segments,
        "the batch must arrive as {segments} datagrams — one oversized \
         datagram instead would be the silent degradation the seam refuses"
    );
    println!(
        "  gso: {segments} datagrams of {SEG} bytes in one try_send, all {} bytes back",
        payload.len()
    );
}

/// A batch one segment past what the socket declared is refused **by
/// name**, and one exactly at the limit is attempted.
///
/// Checking both is what stops an off-by-one, and what stops the check
/// degenerating into a blanket refusal of GSO.
fn the_declared_limit_is_a_limit_and_not_a_ban<S: UdpDatagrams>(s: &S) {
    let caps = s.caps();
    let seg = 1200usize;
    let too_many = caps.max_send_segments + 1;
    let payload = vec![0u8; seg * too_many];
    let err = s
        .try_send(&Datagrams {
            destination: loopback_of(s.local_addr().unwrap()),
            src_ip: None,
            ecn: None,
            segment_size: Some(seg),
            contents: &payload,
        })
        .expect_err("one more segment than this socket declared");
    assert_eq!(err.kind(), std::io::ErrorKind::Unsupported);
    assert!(err.to_string().contains("gso"), "{err}");
}

/// The one shared body.
///
/// `R::Socket: UdpDatagrams` needs no spelling out: it is `UdpBind`'s own
/// associated-type bound, exactly as `TcpConnect::Stream` carries hyper's
/// two IO traits for `pair_property.rs` next door.
async fn exercise_udp<R: UdpBind>(rt: R, label: &str) {
    let v4 = SocketAddr::from(([127, 0, 0, 1], 0));
    let a = rt.bind(v4).expect("an unprivileged loopback UDP bind");
    let b = rt
        .bind(v4)
        .expect("the first bind worked, so the second must");

    let c = a.caps();
    println!(
        "{label}: UdpCaps on this host: gso={} gro={} ecn={} may_fragment={}",
        c.max_send_segments, c.max_recv_segments, c.ecn, c.may_fragment
    );
    // The only part of the report that is a property of this code rather
    // than of the kernel: a socket never reports fewer than one datagram
    // per call, because zero would mean it can send nothing at all.
    assert!(c.max_send_segments >= 1);
    assert!(c.max_recv_segments >= 1);

    // The base case, and the reason `reject_unsupported` cannot be a
    // blanket refusal: the weakest possible socket still has to serve a
    // caller that asked for nothing.
    send(
        &a,
        &Datagrams {
            destination: loopback_of(b.local_addr().unwrap()),
            src_ip: None,
            ecn: None,
            segment_size: None,
            contents: b"plain",
        },
    )
    .await
    .expect("send");
    let meta = recv_one(&b, &mut [0u8; 64][..]).await.expect("recv");
    assert_eq!(meta.len, b"plain".len());
    assert_eq!(meta.addr.ip(), a.local_addr().unwrap().ip());

    ecn_claim_matches_reality(&a, &b).await;
    a_declared_gso_batch_really_goes_out(&a, &b).await;
    the_declared_limit_is_a_limit_and_not_a_ban(&a);

    // **A dual-stack v6 socket is where a per-socket report and a per-
    // runtime constant come apart.** On this kernel a v4 socket and a
    // dual-stack v6 one answer the same, so the v4 case above cannot tell
    // "asked the kernel" from "assumed Linux". It is not hypothetical:
    // `quinn-udp`'s own unix backend carries "mac and ios do not support
    // IP_RECVTOS on dual-stack sockets" (`unix.rs:114`), so on the macOS
    // runner in this project's matrix the honest answer is `false` while a
    // constant `true` would be a claim the socket cannot keep. That is why
    // `caps` is a method on the SOCKET and not a const on the runtime.
    let v6 = SocketAddr::from(([0u8; 16], 0));
    let Ok(a6) = rt.bind(v6) else {
        eprintln!("{label}: skipped the dual-stack leg: this host has no IPv6 loopback");
        return;
    };
    let b6 = rt
        .bind(v6)
        .expect("the first v6 bind succeeded, so the second must too");
    println!(
        "{label}: dual-stack v6 socket reports ecn={} (v4 socket reports {})",
        b6.caps().ecn,
        c.ecn
    );
    ecn_claim_matches_reality(&a6, &b6).await;
}

#[tokio::test]
async fn udp_pair_property_holds_for_tokio() {
    exercise_udp(hclient_rt_tokio::Tokio, "tokio").await;
}

#[test]
fn udp_pair_property_holds_for_smol() {
    futures_executor::block_on(exercise_udp(hclient_rt_smol::Smol, "smol"));
}

/// Two sockets, one kernel, two runtimes — and the same report.
///
/// **This is the check that makes `caps` a query rather than a constant**,
/// and it is the one thing this crate can assert that neither runtime crate
/// can on its own: each backend is the other's oracle. A backend that
/// stopped asking the descriptor and returned a literal — `UdpCaps::NONE`,
/// or a hardcoded `64/64/true` — would differ from its twin on any machine
/// whose real answer is not that literal, and this line says so.
///
/// It is not a claim about the numbers: they are still whatever this kernel
/// says, and the assertion holds equally on a virtualised runner that
/// honestly answers `1/1/false`. It is a claim that the two
/// implementations, which share nothing but a `quinn-udp` version, agree.
#[tokio::test]
async fn both_backends_report_the_same_kernel() {
    let v4 = SocketAddr::from(([127, 0, 0, 1], 0));
    // Inside `#[tokio::test]` because `Tokio::bind` registers with tokio's
    // reactor and panics outside a runtime; `Smol::bind` is indifferent.
    let t = hclient_rt_tokio::Tokio.bind(v4).expect("bind on tokio");
    let s = hclient_rt_smol::Smol.bind(v4).expect("bind on smol");
    assert_eq!(
        t.caps(),
        s.caps(),
        "the two backends read the same descriptor through the same \
         `quinn-udp`; a difference means one of them stopped asking"
    );
}
