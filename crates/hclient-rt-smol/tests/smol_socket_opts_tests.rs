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
    let opts = TcpOpts::default().local_address(Some(IpAddr::V4(SECOND_LOOPBACK)));
    futures_executor::block_on(async {
        let connected = Smol.connect(addr, &opts).await;
        if assignable {
            let s = connected.expect("connect");
            let local = socket2::SockRef::from(&s)
                .local_addr()
                .expect("local_addr query")
                .as_socket()
                .expect("an IP socket");
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
        let local = socket2::SockRef::from(&s)
            .local_addr()
            .expect("local_addr query")
            .as_socket()
            .expect("an IP socket");
        assert_ne!(local.ip(), IpAddr::V4(SECOND_LOOPBACK));
    });
}

#[test]
fn send_buffer_size_is_applied_before_connect() {
    let small_addr = spawn_accepting_listener();
    let large_addr = spawn_accepting_listener();
    futures_executor::block_on(async {
        let small = Smol
            .connect(small_addr, &TcpOpts::default().send_buffer_size(Some(4096)))
            .await
            .expect("small connect");
        let small_size = socket2::SockRef::from(&small)
            .send_buffer_size()
            .expect("small send_buffer_size query");

        let requested = 1usize << 20; // 1 MiB
        let large = Smol
            .connect(
                large_addr,
                &TcpOpts::default().send_buffer_size(Some(requested)),
            )
            .await
            .expect("large connect");
        let large_size = socket2::SockRef::from(&large)
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
            .connect(small_addr, &TcpOpts::default().recv_buffer_size(Some(4096)))
            .await
            .expect("small connect");
        let small_size = socket2::SockRef::from(&small)
            .recv_buffer_size()
            .expect("small recv_buffer_size query");

        let requested = 1usize << 20; // 1 MiB
        let large = Smol
            .connect(
                large_addr,
                &TcpOpts::default().recv_buffer_size(Some(requested)),
            )
            .await
            .expect("large connect");
        let large_size = socket2::SockRef::from(&large)
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
    let opts = TcpOpts::default().reuse_address(true);
    futures_executor::block_on(async {
        let s = Smol.connect(addr, &opts).await.expect("connect");
        let enabled = socket2::SockRef::from(&s)
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
        let enabled = socket2::SockRef::from(&s)
            .reuse_address()
            .expect("reuse_address query");
        assert!(
            !enabled,
            "SO_REUSEADDR must default to off; TcpOpts::default() must not enable it"
        );
    });
}

/// **Keepalive is one setting in three parts, and setting *any* of them
/// switches `SO_KEEPALIVE` on.** `build_socket` reaches
/// `set_tcp_keepalive` — the call that enables the option — through
/// `keepalive.is_some() || keepalive_interval.is_some() ||
/// keepalive_retries.is_some()`, and `TcpOpts::keepalive` states the rule
/// in prose because the field names do not say it: a caller who sets only
/// the interval has switched keepalive on with the OS's idle time.
///
/// Nothing asserted it. `cargo mutants` replaces the **second** `||` with
/// `&&`, and `a || (b && c)` — `&&` binds tighter — still fires for a
/// plain `keepalive`, so `connects_with_keepalive_enabled` cannot see it.
/// Exactly two of the eight settings distinguish the two expressions, and
/// both are the parts-without-the-whole case this rule is about:
/// `keepalive_interval` alone, and `keepalive_retries` alone. Both are
/// asserted here; measured with the mutation applied by hand, each one
/// alone turns red.
///
/// A parameterised loop rather than two functions, so a fourth part cannot
/// be added to `TcpOpts` and quietly keep the old coverage.
#[test]
fn any_one_keepalive_part_on_its_own_switches_keepalive_on() {
    for (part, opts) in [
        (
            "keepalive_interval",
            TcpOpts::default().keepalive_interval(Some(std::time::Duration::from_secs(9))),
        ),
        (
            "keepalive_retries",
            TcpOpts::default().keepalive_retries(Some(4)),
        ),
    ] {
        let addr = spawn_accepting_listener();
        futures_executor::block_on(async {
            let s = Smol.connect(addr, &opts).await.expect("connect");
            let enabled = socket2::SockRef::from(&s)
                .keepalive()
                .expect("keepalive query");
            assert!(
                enabled,
                "TcpOpts::{part} on its own must switch SO_KEEPALIVE on: \
                 set_tcp_keepalive is what enables it, and each part left None \
                 keeps the OS's value"
            );
        });
    }
}

/// The control for the test above, and the half that makes it a claim
/// about the parts rather than about connecting at all: with **no** part
/// set, `SO_KEEPALIVE` stays off.
///
/// Without this, a kernel (or a mutation) that enabled keepalive on every
/// socket would pass every assertion above.
#[test]
fn no_keepalive_part_leaves_keepalive_off() {
    let addr = spawn_accepting_listener();
    futures_executor::block_on(async {
        let s = Smol
            .connect(addr, &TcpOpts::default())
            .await
            .expect("connect");
        let enabled = socket2::SockRef::from(&s)
            .keepalive()
            .expect("keepalive query");
        assert!(
            !enabled,
            "TcpOpts::default() sets no keepalive part, so SO_KEEPALIVE must stay off"
        );
    });
}

/// `TCP_SUPPORT` claims `keepalive_retries` exactly where
/// `socket2::TcpKeepalive::with_retries` exists, and the direction is the
/// one the seam requires.
///
/// The constant is written `!cfg!(any(openbsd, redox, solaris))`, and
/// deleting that `!` inverts it — which no test could see, because
/// `TCP_SUPPORT` was only ever compared against a socket for `nodelay` and
/// `keepalive`. The inversion is the **overstating** direction on the
/// three platforms it names and the **understating** one everywhere else,
/// and this workspace's rule is that an understated `TCP_SUPPORT` costs a
/// caller a named `Unsupported` error while an overstated one costs them
/// an option silently not applied.
///
/// So the assertion is tied to the same `cfg` the applying code in
/// `build_socket` is gated on, rather than to a literal: a target where
/// `with_retries` is absent is a target where the claim must be `false`,
/// and one where it compiles is one where the claim must be `true`. Stated
/// as a biconditional for the reason `Head::version` and
/// `version_reported` are — two copies of one fact drift, and the way to
/// stop that is to assert they agree.
#[test]
fn applies_claims_keepalive_retries_exactly_where_socket2_can_set_them() {
    let with_retries_exists = !cfg!(any(
        target_os = "openbsd",
        target_os = "redox",
        target_os = "solaris"
    ));
    assert_eq!(
        <Smol as TcpConnect>::TCP_SUPPORT.keepalive_retries,
        with_retries_exists,
        "TCP_SUPPORT.keepalive_retries must be true exactly where \
         socket2::TcpKeepalive::with_retries exists — an overstated claim is \
         an option silently not applied"
    );
}

/// The two `cfg`-computed `TCP_SUPPORT` fields say Linux, which is what this
/// host is, and the whole point of them being `cfg!` rather than constants
/// is that they are **not** claimed on every target.
///
/// Asserted against the same predicates `build_socket` gates the applying
/// code on, for `keepalive_retries`' reason: the claim and the code that
/// honours it are two statements of one fact, and a test that repeats a
/// literal instead would pin the literal rather than the agreement.
#[test]
fn the_platform_gated_applies_fields_agree_with_the_code_that_applies_them() {
    let bind_device_compiles = cfg!(any(
        target_os = "android",
        target_os = "fuchsia",
        target_os = "linux"
    ));
    let user_timeout_compiles = cfg!(any(
        target_os = "android",
        target_os = "fuchsia",
        target_os = "linux",
        target_os = "cygwin"
    ));
    let a = <Smol as TcpConnect>::TCP_SUPPORT;
    assert_eq!(
        a.bind_device, bind_device_compiles,
        "TCP_SUPPORT.bind_device must match where SO_BINDTODEVICE is actually set"
    );
    assert_eq!(
        a.user_timeout, user_timeout_compiles,
        "TCP_SUPPORT.user_timeout must match where TCP_USER_TIMEOUT is actually set"
    );
    // And the fields that are unconditional stay claimed, so an edit that
    // narrows the constant wholesale is caught here rather than by a
    // caller meeting an `Unsupported` for an option this runtime applies.
    assert!(a.nodelay && a.keepalive && a.keepalive_interval);
    assert!(a.local_address && a.send_buffer_size && a.recv_buffer_size && a.reuse_address);
}

/// **A direct caller is refused an option this target cannot apply**, with
/// nothing dialled. The check used to live only in
/// `hclient_native::Native::tcp_opts`, so a caller reaching the runtime
/// directly had `bind_device` silently dropped where `build_socket`'s `cfg`
/// skips it. Gated to exactly those targets: on Linux the option is
/// applied, and the test that reads it back off the socket is the pin.
#[cfg(not(any(target_os = "android", target_os = "fuchsia", target_os = "linux")))]
#[test]
fn an_option_this_target_cannot_apply_is_refused_before_connecting() {
    const { assert!(!<Smol as TcpConnect>::TCP_SUPPORT.bind_device) };
    // Nothing listens here, so a connect that went ahead would fail with
    // a different kind — the refusal is what separates the two.
    let addr = std::net::SocketAddr::from((Ipv4Addr::LOCALHOST, 9));
    let opts = TcpOpts::default().bind_device(Some("lo0".to_owned()));
    let err = futures_lite::future::block_on(Smol.connect(addr, &opts)).expect_err("refused");
    assert_eq!(err.kind(), std::io::ErrorKind::Unsupported);
    let names = err
        .get_ref()
        .and_then(|p| p.downcast_ref::<hclient_rt::UnsupportedTcp>())
        .map(|p| p.names().collect::<Vec<_>>());
    assert_eq!(names, Some(vec!["bind_device"]));
}
