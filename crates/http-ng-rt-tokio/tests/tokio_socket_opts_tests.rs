//! Reviewer-written probe (not part of the implementer's work): checks the
//! four `TcpOpts` fields not already covered by the implementer's own
//! `connects_to_a_local_listener_with_options` (nodelay) and
//! `connects_with_keepalive_enabled` (keepalive) tests: `local_address`,
//! `send_buffer_size`, `recv_buffer_size`, `reuse_address`. Point D of the
//! Task 3 review: verify each option is actually applied on a real
//! connected socket, not merely accepted without effect.
//!
//! Ran as an integration test (`crates/http-ng-rt-tokio/tests/*.rs`) in a
//! throwaway clone during the Task 3 review: all 6 tests passed against
//! `http-ng-rt-tokio` at `2948375`. To re-run: drop this file into
//! `crates/http-ng-rt-tokio/tests/` in a scratch clone and `cargo test -p
//! http-ng-rt-tokio --test tokio_socket_opts_tests --all-features`.
//!
//! IMPORTANT design note for whoever extends this file: the first version
//! of the buffer-size tests asserted "explicit request > default/untouched
//! baseline" and failed in this exact sandbox - not because `build_socket`
//! ignored the option, but because an *unset* SO_SNDBUF/SO_RCVBUF uses
//! Linux's dynamic auto-tuning, which had already grown to ~3.68 MB on this
//! box, comfortably above a pinned 1 MiB request (which the kernel doubles
//! on readback to ~2 MiB, per its usual SO_SNDBUF/SO_RCVBUF accounting).
//! So "pinned value > default" is not a reliable signal of "the setter took
//! effect" - it can go either way depending on how aggressively the host's
//! defaults auto-tune. Do not resurrect that comparison; instead this file
//! requests two *different* explicit sizes and checks the readback is
//! monotonic in what was asked for, which isolates "does build_socket's
//! setter work" from "how does this host's auto-tuned default compare
//! today".
use http_ng_rt::{TcpConnect, TcpOpts};
use http_ng_rt_tokio::Tokio;
use std::net::{IpAddr, Ipv4Addr};

fn spawn_accepting_listener() -> std::net::SocketAddr {
    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = l.local_addr().unwrap();
    std::thread::spawn(move || {
        // Accept in a loop so multiple test connects in this file (each
        // its own listener, actually - see below) don't race a
        // single-shot accept.
        let _ = l.accept();
    });
    addr
}

#[tokio::test]
async fn local_address_selects_the_connecting_source_ip() {
    // 127.0.0.0/8 is entirely loopback on Linux, so 127.0.0.2 is a valid,
    // distinct local address to bind from - this discriminates "the option
    // took effect" from "the OS default happened to match" (which a naive
    // test against 127.0.0.1 could not do, since that's already the
    // default route to a 127.0.0.1 destination).
    let addr = spawn_accepting_listener();
    let opts = TcpOpts {
        local_address: Some(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 2))),
        ..Default::default()
    };
    let s = Tokio.connect(addr, &opts).await.expect("connect");
    let local = s.get_ref().local_addr().expect("local_addr query");
    assert_eq!(
        local.ip(),
        IpAddr::V4(Ipv4Addr::new(127, 0, 0, 2)),
        "TcpOpts::local_address did not select the connecting source IP"
    );
}

#[tokio::test]
async fn default_local_address_is_not_127_0_0_2() {
    // Control for the test above: without the option, the source must NOT
    // be 127.0.0.2 (otherwise the previous test would pass even if
    // local_address were silently ignored, because the OS default could
    // coincidentally match).
    let addr = spawn_accepting_listener();
    let s = Tokio
        .connect(addr, &TcpOpts::default())
        .await
        .expect("connect");
    let local = s.get_ref().local_addr().expect("local_addr query");
    assert_ne!(local.ip(), IpAddr::V4(Ipv4Addr::new(127, 0, 0, 2)));
}

// Comparing an explicit request against the OS *default* baseline turned
// out to be unreliable in this sandbox: an unset SO_SNDBUF/SO_RCVBUF uses
// Linux's dynamic auto-tuning, which can already exceed a small pinned
// request (observed: default baseline 3,677,184 bytes vs. an explicit 1 MiB
// request read back as 2,097,152 - the kernel doubles what you ask for, but
// that's still less than the autotuned default). So instead: request two
// *different* explicit sizes and confirm the readback is monotonic in what
// was asked for - that isolates "does the setter take effect" from "how the
// OS's default auto-tuning happens to compare", and a `false &&`-style
// no-op mutation would collapse both to the same (autotuned-default) value.
#[tokio::test]
async fn send_buffer_size_is_applied_before_connect() {
    let small_addr = spawn_accepting_listener();
    let small = Tokio
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

    let large_addr = spawn_accepting_listener();
    let requested = 1usize << 20; // 1 MiB
    let large = Tokio
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
}

#[tokio::test]
async fn recv_buffer_size_is_applied_before_connect() {
    let small_addr = spawn_accepting_listener();
    let small = Tokio
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

    let large_addr = spawn_accepting_listener();
    let requested = 1usize << 20; // 1 MiB
    let large = Tokio
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
}

#[tokio::test]
async fn reuse_address_is_applied_before_connect() {
    let addr = spawn_accepting_listener();
    let opts = TcpOpts {
        reuse_address: true,
        ..Default::default()
    };
    let s = Tokio.connect(addr, &opts).await.expect("connect");
    let enabled = socket2::SockRef::from(s.get_ref())
        .reuse_address()
        .expect("reuse_address query");
    assert!(enabled, "TcpOpts::reuse_address did not set SO_REUSEADDR");
}

#[tokio::test]
async fn default_reuse_address_is_off() {
    // Control for the test above.
    let addr = spawn_accepting_listener();
    let s = Tokio
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
}
