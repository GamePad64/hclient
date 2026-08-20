//! Mirrors `crates/hclient-rt-tokio/tests/tokio_socket_opts_tests.rs` for
//! the smol backend. Exists because `connect()` is exactly where the
//! brief/skeleton had a defect: `async_net::TcpStream::connect(addr)`
//! accepts no options at all, so `reuse_address`, `send_buffer_size`,
//! `recv_buffer_size`, and `local_address` were silently lost, and
//! `nodelay`/`keepalive` were applied only AFTER `connect()`. Each test
//! here reads the option back from a genuinely connected socket, rather
//! than relying on `connect()` having returned `Ok`.
//!
//! The same two design decisions as `tokio_socket_opts_tests.rs` are kept
//! deliberately:
//!
//! 1. Buffer sizes are compared as "two DIFFERENT explicit requests, the
//!    larger one reads back larger", not "explicit request > default".
//!    Reason: in this sandbox, `SO_SNDBUF`/`SO_RCVBUF` left unset are
//!    auto-tuned by the kernel above a small pinned request, so "request >
//!    default" doesn't signal "the setter worked" — it can go either way
//!    depending on how aggressively the host has already auto-tuned its
//!    default.
//! 2. Negative controls exist so the positive tests don't pass against a
//!    default that already happens to match the value under test.
use hclient_rt::{TcpConnect, TcpOpts};
use hclient_rt_smol::Smol;
use std::net::{IpAddr, Ipv4Addr};

/// The whole of `127.0.0.0/8` is loopback on Linux and Windows, so
/// `127.0.0.2` is assignable there — and it is a source IP *distinct* from
/// the default route to a `127.0.0.1` destination, which is exactly what
/// makes the positive assertion below discriminating (a naive test against
/// `127.0.0.1` could not tell "the option took effect" from "the OS default
/// happened to match").
///
/// macOS/BSD configure only `127.0.0.1` on `lo0`, so binding `127.0.0.2`
/// there is `EADDRNOTAVAIL` unless an alias was added by hand — measured on
/// a `macos-latest` runner, where this test failed with `Os { code: 49 }`.
/// CI adds the alias (see the `test` job in `ci.yml`) so the strong
/// assertion is what actually runs there; the fallback below is for a
/// developer's laptop, not for CI.
const SECOND_LOOPBACK: Ipv4Addr = Ipv4Addr::new(127, 0, 0, 2);

fn second_loopback_is_assignable() -> bool {
    std::net::TcpListener::bind((SECOND_LOOPBACK, 0)).is_ok()
}

fn spawn_accepting_listener() -> std::net::SocketAddr {
    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = l.local_addr().unwrap();
    std::thread::spawn(move || {
        let _ = l.accept();
    });
    addr
}

#[test]
fn local_address_selects_the_connecting_source_ip() {
    let addr = spawn_accepting_listener();
    let assignable = second_loopback_is_assignable();
    let opts = TcpOpts {
        local_address: Some(IpAddr::V4(SECOND_LOOPBACK)),
        ..Default::default()
    };
    futures_executor::block_on(async {
        let connected = Smol.connect(addr, &opts).await;
        if assignable {
            let s = connected.expect("connect");
            let local = s.get_ref().tcp().local_addr().expect("local_addr query");
            assert_eq!(
                local.ip(),
                IpAddr::V4(SECOND_LOOPBACK),
                "TcpOpts::local_address did not select the connecting source IP"
            );
        } else {
            // Weaker than reading the source IP back, but not vacuous, and
            // available on every host: an address that cannot be bound must
            // make `connect` FAIL. A silently dropped `local_address` would
            // connect happily from 127.0.0.1 instead — precisely the defect
            // this file exists to catch.
            let err = connected.expect_err(
                "TcpOpts::local_address was silently ignored: connecting from an unassignable \
                 local address succeeded",
            );
            assert_eq!(
                err.kind(),
                std::io::ErrorKind::AddrNotAvailable,
                "binding an unassignable local address should fail with AddrNotAvailable, got: {err}"
            );
        }
    });
}

#[test]
fn default_local_address_is_not_127_0_0_2() {
    // Control for the test above: without the option, the source must NOT
    // be 127.0.0.2 (otherwise the previous test would pass even if
    // local_address were silently ignored, because the OS default could
    // coincidentally match).
    let addr = spawn_accepting_listener();
    futures_executor::block_on(async {
        let s = Smol
            .connect(addr, &TcpOpts::default())
            .await
            .expect("connect");
        let local = s.get_ref().tcp().local_addr().expect("local_addr query");
        assert_ne!(local.ip(), IpAddr::V4(SECOND_LOOPBACK));
    });
}

#[test]
fn send_buffer_size_is_applied_before_connect() {
    let small_addr = spawn_accepting_listener();
    let large_addr = spawn_accepting_listener();
    futures_executor::block_on(async {
        let small = Smol
            .connect(
                small_addr,
                &TcpOpts {
                    send_buffer_size: Some(4096),
                    ..Default::default()
                },
            )
            .await
            .expect("small connect");
        let small_size = socket2::SockRef::from(small.get_ref().tcp())
            .send_buffer_size()
            .expect("small send_buffer_size query");

        let requested = 1usize << 20; // 1 MiB
        let large = Smol
            .connect(
                large_addr,
                &TcpOpts {
                    send_buffer_size: Some(requested),
                    ..Default::default()
                },
            )
            .await
            .expect("large connect");
        let large_size = socket2::SockRef::from(large.get_ref().tcp())
            .send_buffer_size()
            .expect("large send_buffer_size query");

        assert!(
            large_size > small_size,
            "TcpOpts::send_buffer_size did not take effect: requesting {requested} read back as \
             {large_size}, which is not larger than requesting 4096 (read back as {small_size})"
        );
    });
}

#[test]
fn recv_buffer_size_is_applied_before_connect() {
    let small_addr = spawn_accepting_listener();
    let large_addr = spawn_accepting_listener();
    futures_executor::block_on(async {
        let small = Smol
            .connect(
                small_addr,
                &TcpOpts {
                    recv_buffer_size: Some(4096),
                    ..Default::default()
                },
            )
            .await
            .expect("small connect");
        let small_size = socket2::SockRef::from(small.get_ref().tcp())
            .recv_buffer_size()
            .expect("small recv_buffer_size query");

        let requested = 1usize << 20; // 1 MiB
        let large = Smol
            .connect(
                large_addr,
                &TcpOpts {
                    recv_buffer_size: Some(requested),
                    ..Default::default()
                },
            )
            .await
            .expect("large connect");
        let large_size = socket2::SockRef::from(large.get_ref().tcp())
            .recv_buffer_size()
            .expect("large recv_buffer_size query");

        assert!(
            large_size > small_size,
            "TcpOpts::recv_buffer_size did not take effect: requesting {requested} read back as \
             {large_size}, which is not larger than requesting 4096 (read back as {small_size})"
        );
    });
}

#[test]
fn reuse_address_is_applied_before_connect() {
    let addr = spawn_accepting_listener();
    let opts = TcpOpts {
        reuse_address: true,
        ..Default::default()
    };
    futures_executor::block_on(async {
        let s = Smol.connect(addr, &opts).await.expect("connect");
        let enabled = socket2::SockRef::from(s.get_ref().tcp())
            .reuse_address()
            .expect("reuse_address query");
        assert!(enabled, "TcpOpts::reuse_address did not set SO_REUSEADDR");
    });
}

#[test]
fn default_reuse_address_is_off() {
    // Control for the test above.
    let addr = spawn_accepting_listener();
    futures_executor::block_on(async {
        let s = Smol
            .connect(addr, &TcpOpts::default())
            .await
            .expect("connect");
        let enabled = socket2::SockRef::from(s.get_ref().tcp())
            .reuse_address()
            .expect("reuse_address query");
        assert!(
            !enabled,
            "SO_REUSEADDR must default to off; TcpOpts::default() must not enable it"
        );
    });
}
