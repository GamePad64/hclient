//! Mirrors `crates/http-ng-rt-tokio/tests/tokio_socket_opts_tests.rs` for
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
use http_ng_rt::{TcpConnect, TcpOpts};
use http_ng_rt_smol::Smol;
use std::net::{IpAddr, Ipv4Addr};

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
    // 127.0.0.0/8 is entirely loopback on Linux, so 127.0.0.2 is a valid
    // local address distinct from the default: it discriminates "the
    // option took effect" from "the OS default happened to match" (a naive
    // test against 127.0.0.1 couldn't do this, since that's already the
    // default route to a 127.0.0.1 destination).
    let addr = spawn_accepting_listener();
    let opts = TcpOpts {
        local_address: Some(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 2))),
        ..Default::default()
    };
    futures_executor::block_on(async {
        let s = Smol.connect(addr, &opts).await.expect("connect");
        let local = s.get_ref().local_addr().expect("local_addr query");
        assert_eq!(
            local.ip(),
            IpAddr::V4(Ipv4Addr::new(127, 0, 0, 2)),
            "TcpOpts::local_address did not select the connecting source IP"
        );
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
        let local = s.get_ref().local_addr().expect("local_addr query");
        assert_ne!(local.ip(), IpAddr::V4(Ipv4Addr::new(127, 0, 0, 2)));
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
        let small_size = socket2::SockRef::from(small.get_ref())
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
        let large_size = socket2::SockRef::from(large.get_ref())
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
        let small_size = socket2::SockRef::from(small.get_ref())
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
        let large_size = socket2::SockRef::from(large.get_ref())
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
        let enabled = socket2::SockRef::from(s.get_ref())
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
        let enabled = socket2::SockRef::from(s.get_ref())
            .reuse_address()
            .expect("reuse_address query");
        assert!(
            !enabled,
            "SO_REUSEADDR must default to off; TcpOpts::default() must not enable it"
        );
    });
}
