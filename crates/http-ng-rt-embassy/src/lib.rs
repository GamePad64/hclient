//! `http-ng-rt` capabilities implemented on top of embassy: the clock is
//! `embassy-time`, the sockets are `embassy-net` (smoltcp), and the whole
//! thing runs under `embassy_executor::Executor` with no thread, no
//! reactor and no `std::net` socket anywhere on the client path.
//!
//! ```ignore
//! let pool = SocketPool::<2, 1536, 1536>::leak(stack);
//! let rt = Embassy::new(stack, pool);
//! let client = Client::builder(Native::new(rt, NoTls, IpLiteralOnly)).build()?;
//! ```
//!
//! Nothing above [`Embassy`] changes: `http_ng_native::Native` takes it as
//! its `R` exactly as it takes `Tokio` or `Smol`, and the W7 research
//! established that `TcpConnect::Stream` needs no lifetime for this to work
//! — `embassy_net::tcp::TcpSocket<'a>` holds no buffers, so with the
//! buffers in a `'static` the socket is `TcpSocket<'static>`.
//!
//! # The third `Timer::Instant` in the workspace
//!
//! `Instant = embassy_time::Instant`, alongside tokio's
//! `tokio::time::Instant` and smol's `std::time::Instant`. That is not
//! decoration: `embassy_time::Instant` is a tick count with no relation to
//! the wall clock and no `std` behind it, which is exactly the case the
//! associated type exists for. A backend forced to hand back
//! `std::time::Instant` would have to invent one on a device where
//! `std::time::Instant::now()` does not exist.
//!
//! # Cancellation: why this crate owns a socket pool
//!
//! `Transport::execute`'s contract (v0.2 W1) is that dropping the future
//! stops the exchange, and `http_ng_native::Native` declares
//! `CancelSupport::Supported` **structurally** — the future owns the
//! socket, so dropping it closes the connection. On embassy-net that
//! reasoning breaks at the last step, and it was measured breaking:
//! `TcpSocket::drop` removes the socket from smoltcp's `SocketSet` before
//! the stack can turn the queued FIN into a packet, so the server sees
//! nothing at all — the W7 research watched a server hold such a
//! connection open for two seconds after the client had dropped everything.
//!
//! The research offered two ways out, and this crate takes the second:
//!
//! 1. **Declare `CancelSupport::None`.** Honest, and the mechanism exists —
//!    but there is nowhere to declare it from. `Capabilities` belongs to
//!    the `Transport`, and the `Transport` here is `Native`, which
//!    hardcodes `Supported` on the strength of its own reasoning about its
//!    own future. A runtime plugged in underneath cannot lower it: the
//!    `TcpConnect` seam has no channel for it, and adding one would mean
//!    `Native` asking every runtime "do your sockets close on drop?", a
//!    question with exactly one wrong answer to date. So this option is not
//!    "less good", it is **unavailable without a change in
//!    `http-ng-native`** — and it would leave the capability lying for
//!    every build made before that change landed.
//! 2. **Keep the socket alive past the drop**, which is what
//!    [`SocketPool`] does: `PooledSocket::drop` calls `close()` and moves
//!    the socket to a closing list instead of dropping it, so it is still
//!    registered with smoltcp when the stack next runs — and `close()`
//!    itself wakes the stack task, so "next" means immediately.
//!    `Native`'s claim stays true, and the acceptance
//!    (`tests/tuntap.rs`) checks it the same way `http-ng-native`'s own
//!    `cancel.rs` does: from the server's socket, not from the client's
//!    task, against a control that holds the future and sees the
//!    connection stay open.
//!
//! # What is deliberately not implemented
//!
//! - **`Spawn`.** `Native` never asks for it (`R: TcpConnect + Timer` is
//!   its whole bound), and W2 established it could not be used here anyway:
//!   `http_ng_rt::Spawn`'s useful implementations want `F: Send + 'static`
//!   while this vertical's IO is deliberately not `Send`. It is
//!   *implementable* on embassy — `raw::TaskStorage::<F>::spawn` takes a
//!   `&'static self`, so `Box::leak` per call would do it — at the cost of
//!   one leaked `TaskStorage<F>` per spawn, with no way to reclaim it.
//! - **`Blocking`.** There is no thread pool on a microcontroller. The one
//!   consequence is that `http_ng_dns_system::SystemDns` (which is
//!   `impl<B: Blocking> Resolve`) cannot be used; `IpLiteralOnly` can.
//! - **A `Resolve` over `embassy_net::Stack::dns_query`.** It is a small
//!   and obvious piece — `dns_query` is an `async fn` needing no thread
//!   pool — and it is **not written here**: it needs embassy-net's `dns`
//!   feature and a DNS server on the test link, which is a second piece of
//!   harness for a question this task does not turn on.
//! - **`TcpAdoptStd`.** There is no file descriptor to adopt. The seam
//!   already expresses that by the trait simply not being implemented.

#![forbid(unsafe_code)]

mod io;
mod sockets;

pub use io::EmbassyIo;
pub use sockets::{PooledSocket, SocketBuffers, SocketPool};

use core::net::SocketAddr;
use embassy_net::Stack;
use http_ng_rt::{TcpConnect, TcpOpts, TcpOptsSupport, Timer};
use std::time::Duration;

/// The runtime: an `embassy-net` stack plus a bounded pool of sockets on
/// it, both owned by the application and both `'static`.
///
/// `Copy`, like `embassy_net::Stack` itself — this is two pointers, and
/// `Native` wants to be able to clone the runtime it was given.
pub struct Embassy<const N: usize, const TX: usize = 1536, const RX: usize = 1536> {
    stack: Stack<'static>,
    sockets: &'static SocketPool<N, TX, RX>,
}

// Hand-written, and not `#[derive]`: `derive(Clone)` would demand `Clone`
// on the const-generic parameters' types, and `embassy_net::Stack` has no
// `Debug` to derive from either.
impl<const N: usize, const TX: usize, const RX: usize> Clone for Embassy<N, TX, RX> {
    fn clone(&self) -> Self {
        *self
    }
}
impl<const N: usize, const TX: usize, const RX: usize> Copy for Embassy<N, TX, RX> {}

impl<const N: usize, const TX: usize, const RX: usize> core::fmt::Debug for Embassy<N, TX, RX> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Embassy")
            .field("sockets", self.sockets)
            .finish_non_exhaustive()
    }
}

impl<const N: usize, const TX: usize, const RX: usize> Embassy<N, TX, RX> {
    /// The `stack` must be the one the `sockets` were built on: a
    /// `TcpSocket` holds a handle into that stack's `SocketSet` and means
    /// nothing anywhere else.
    pub fn new(stack: Stack<'static>, sockets: &'static SocketPool<N, TX, RX>) -> Self {
        Self { stack, sockets }
    }

    /// The stack this runtime connects on — the application already has it,
    /// but code generic over the runtime may not.
    pub fn stack(&self) -> Stack<'static> {
        self.stack
    }

    /// The socket pool, for the bookkeeping assertions the acceptance
    /// makes about slots.
    pub fn sockets(&self) -> &'static SocketPool<N, TX, RX> {
        self.sockets
    }
}

impl<const N: usize, const TX: usize, const RX: usize> Timer for Embassy<N, TX, RX> {
    type Instant = embassy_time::Instant;

    async fn sleep(&self, d: Duration) {
        embassy_time::Timer::after(to_embassy(d)).await;
    }

    fn now(&self) -> Self::Instant {
        embassy_time::Instant::now()
    }

    fn elapsed_since(&self, earlier: Self::Instant) -> Duration {
        Duration::from_micros(
            embassy_time::Instant::now()
                .saturating_duration_since(earlier)
                .as_micros(),
        )
    }
}

/// `std::time::Duration` → `embassy_time::Duration`, **rounding up** and
/// saturating.
///
/// Rounding is not a detail here. `embassy_time::Duration` counts ticks,
/// and on a device those can be as coarse as 32768 Hz, where a rounded-down
/// 20 µs timeout is zero ticks — a timeout that fires before the operation
/// it was meant to bound, which is worse than no timeout at all. embassy's
/// own `from_nanos` rounds up for the same reason; `from_nanos_floor` sits
/// right next to it in the same file and is the wrong one.
///
/// Saturation matters at the other end, and none of embassy's own
/// constructors can be used for it: `Duration::MAX` in std is about 5.8e11
/// years, `from_nanos` multiplies before dividing and wraps,
/// `try_from_nanos` guards its multiply but then overflows inside
/// `div_ceil` (`(num + den - 1)`, `embassy-time-0.5.1/src/duration.rs:277`)
/// — measured, as a debug-build panic, by
/// `an_unrepresentable_duration_saturates_instead_of_wrapping`. So the
/// arithmetic is done here, in `u128`, against embassy's own public
/// `TICK_HZ`, and clamped once at the end.
fn to_embassy(d: Duration) -> embassy_time::Duration {
    let ticks = d
        .as_nanos()
        .saturating_mul(u128::from(embassy_time::TICK_HZ))
        .div_ceil(1_000_000_000);
    embassy_time::Duration::from_ticks(u64::try_from(ticks).unwrap_or(u64::MAX))
}

impl<const N: usize, const TX: usize, const RX: usize> TcpConnect for Embassy<N, TX, RX> {
    type Stream = EmbassyIo<N, TX, RX>;

    /// Two of the six, and the other four are refused rather than ignored.
    ///
    /// `nodelay` and `keepalive` are real here — `TcpSocket::
    /// set_nagle_enabled` and `set_keep_alive`
    /// (`embassy-net-0.9.1/src/tcp.rs:353,363`) — because this crate owns
    /// the `TcpSocket` itself. (The W7 research said "none of them",
    /// looking at `TcpConnection` from embassy's own `TcpClient`, which
    /// exposes neither.)
    ///
    /// The other four are not missing work, they are structurally absent:
    ///
    /// - `local_address` — the address is the *stack's*, configured once
    ///   for the whole interface; `connect` chooses only a local port.
    /// - `send_buffer_size`, `recv_buffer_size` — `TX` and `RX`, fixed when
    ///   the pool was built. Accepting a larger value would be a lie and
    ///   accepting a smaller one silently narrows a window the caller
    ///   asked to widen.
    /// - `reuse_address` — `SO_REUSEADDR` has no counterpart in smoltcp;
    ///   there is no bind, and no address in a `TIME_WAIT` to steal.
    const APPLIES: TcpOptsSupport = TcpOptsSupport {
        nodelay: true,
        keepalive: true,
        local_address: false,
        send_buffer_size: false,
        recv_buffer_size: false,
        reuse_address: false,
    };

    async fn connect(
        &self,
        addr: SocketAddr,
        opts: &TcpOpts,
    ) -> std::io::Result<EmbassyIo<N, TX, RX>> {
        // First, before a slot is taken: an option this runtime cannot
        // apply fails the connect, naming itself. See `TcpConnect::connect`
        // — silently ignoring it is the one answer that is not available.
        opts.reject_unsupported(Self::APPLIES)?;
        let endpoint = endpoint(addr)?;

        let mut sock = self.sockets.acquire().await;
        // Every setting is written on every connect, because smoltcp's
        // `reset()` — which `connect` calls — deliberately does NOT clear
        // them (`smoltcp-0.13.1/src/socket/tcp.rs`, `reset`), so a reused
        // socket would otherwise inherit whatever the previous exchange,
        // or the closing handshake, left behind.
        //
        // `set_timeout(None)`: an established connection here has no
        // inactivity timeout. The one the pool sets while closing
        // (`CLOSING_TIMEOUT`) would otherwise survive into this connection
        // and abort a legitimately slow response.
        sock.get_mut().set_timeout(None);
        // Nagle is smoltcp's default-on; `nodelay` is its inverse.
        sock.get_mut().set_nagle_enabled(!opts.nodelay);
        sock.get_mut()
            .set_keep_alive(opts.keepalive.map(to_embassy));
        sock.get_mut()
            .connect(endpoint)
            .await
            .map_err(connect_err)?;
        Ok(EmbassyIo::new(sock))
    }
}

fn connect_err(e: embassy_net::tcp::ConnectError) -> std::io::Error {
    use embassy_net::tcp::ConnectError as E;
    use std::io::ErrorKind as K;
    let kind = match e {
        E::ConnectionReset => K::ConnectionRefused,
        E::TimedOut => K::TimedOut,
        E::NoRoute => K::NetworkUnreachable,
        // "The socket is already connected or listening" — from a pool
        // that only ever hands out `Closed`/`TimeWait` sockets this would
        // be a bug in this crate, not a network condition.
        E::InvalidState => K::Other,
    };
    std::io::Error::new(kind, e)
}

/// `SocketAddr` → smoltcp's endpoint, or a typed error for an address
/// family this build left out.
///
/// smoltcp's `IpAddress` has one variant per family, each gated on its
/// `proto-ipv4`/`proto-ipv6` feature, so "no IPv6 in this build" is not a
/// runtime configuration but an absent enum variant. A client asked for
/// `[::1]:80` on an IPv4-only build gets an error naming the feature —
/// which is a good deal more useful than the type error the naive version
/// of this function is.
fn endpoint(addr: SocketAddr) -> std::io::Result<embassy_net::IpEndpoint> {
    match addr {
        SocketAddr::V4(v4) => {
            #[cfg(feature = "proto-ipv4")]
            {
                Ok(v4.into())
            }
            #[cfg(not(feature = "proto-ipv4"))]
            {
                let _ = v4;
                Err(no_family("IPv4", "proto-ipv4"))
            }
        }
        SocketAddr::V6(v6) => {
            #[cfg(feature = "proto-ipv6")]
            {
                Ok(v6.into())
            }
            #[cfg(not(feature = "proto-ipv6"))]
            {
                let _ = v6;
                Err(no_family("IPv6", "proto-ipv6"))
            }
        }
    }
}

#[cfg(not(all(feature = "proto-ipv4", feature = "proto-ipv6")))]
fn no_family(family: &str, feature: &str) -> std::io::Error {
    std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        format!(
            "this build of http-ng-rt-embassy has no {family} support: \
             enable the `{feature}` feature of http-ng-rt-embassy"
        ),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    // These run on the host with no stack, no driver and no namespace:
    // everything here is arithmetic or a table, and the parts that need a
    // network live in `tests/tuntap.rs`.

    #[test]
    fn a_duration_too_small_for_a_tick_still_sleeps_for_one() {
        // The failure this pins is a timeout that fires immediately
        // because it rounded down to zero ticks. `from_nanos_floor` would
        // pass every other test in this file.
        assert!(to_embassy(Duration::from_nanos(1)).as_ticks() >= 1);
        assert!(to_embassy(Duration::from_nanos(1)) > embassy_time::Duration::MIN);
    }

    #[test]
    fn a_duration_is_never_rounded_down() {
        // A tick is 1 us under `embassy-time/std`, so 1500 ns is a tick and
        // a half: rounding down gives 1, and a timeout 33% shorter than
        // asked for.
        assert_eq!(to_embassy(Duration::from_nanos(1500)).as_ticks(), 2);
    }

    #[test]
    fn an_unrepresentable_duration_saturates_instead_of_wrapping() {
        // `Duration::MAX.as_nanos()` does not fit in a u64; `as u64` would
        // wrap it to something small and turn "effectively never" into a
        // prompt timeout.
        assert_eq!(
            to_embassy(Duration::MAX),
            embassy_time::Duration::MAX,
            "the largest std duration must map to the largest embassy one"
        );
        assert_eq!(to_embassy(Duration::ZERO), embassy_time::Duration::MIN);
    }

    #[test]
    fn round_trip_through_embassy_keeps_a_millisecond_scale_duration() {
        // The scale `Native` actually uses it at: pool idle timeouts and
        // connect deadlines.
        let d = Duration::from_millis(1500);
        assert_eq!(
            Duration::from_micros(to_embassy(d).as_micros()),
            Duration::from_millis(1500)
        );
    }

    #[test]
    fn the_two_options_this_runtime_can_apply_are_the_two_it_declares() {
        // Reading the const back is not the point; the point is that the
        // pair `(nodelay, keepalive)` and only that pair passes
        // `reject_unsupported`, which is what `connect` calls.
        type Rt = Embassy<1>;
        let ok = TcpOpts {
            nodelay: true,
            keepalive: Some(Duration::from_secs(30)),
            ..TcpOpts::default()
        };
        assert!(ok.reject_unsupported(<Rt as TcpConnect>::APPLIES).is_ok());
    }

    #[test]
    fn each_option_this_runtime_cannot_apply_is_refused_by_name() {
        // One case per unappliable option, not one case for the set: an
        // implementation that refused a fixed option, or that had
        // `APPLIES` wrong in only one field, would pass a single-case
        // test.
        type Rt = Embassy<1>;
        let cases: [(&str, TcpOpts); 4] = [
            (
                "local_address",
                TcpOpts {
                    local_address: Some(core::net::IpAddr::from([127, 0, 0, 1])),
                    ..TcpOpts::default()
                },
            ),
            (
                "send_buffer_size",
                TcpOpts {
                    send_buffer_size: Some(64 * 1024),
                    ..TcpOpts::default()
                },
            ),
            (
                "recv_buffer_size",
                TcpOpts {
                    recv_buffer_size: Some(64 * 1024),
                    ..TcpOpts::default()
                },
            ),
            (
                "reuse_address",
                TcpOpts {
                    reuse_address: true,
                    ..TcpOpts::default()
                },
            ),
        ];
        for (name, opts) in cases {
            let err = opts
                .reject_unsupported(<Rt as TcpConnect>::APPLIES)
                .expect_err("this runtime cannot apply {name}");
            assert_eq!(err.kind(), std::io::ErrorKind::Unsupported);
            assert!(
                err.to_string().contains(name),
                "the refusal must name {name}, got: {err}"
            );
            let payload = err
                .get_ref()
                .and_then(|e| e.downcast_ref::<http_ng_rt::UnsupportedTcpOpts>())
                .expect("typed payload");
            assert_eq!(
                payload.names().collect::<Vec<_>>(),
                [name],
                "exactly one option was set, so exactly one must be named"
            );
        }
    }

    #[test]
    fn a_default_tcp_opts_asks_for_nothing_this_runtime_cannot_do() {
        // `Native` passes `TcpOpts::default()` unless the caller called
        // `tcp_opts`, so this is the path every ordinary request takes.
        type Rt = Embassy<1>;
        assert!(
            TcpOpts::default()
                .reject_unsupported(<Rt as TcpConnect>::APPLIES)
                .is_ok()
        );
    }

    #[test]
    #[cfg(not(feature = "proto-ipv6"))]
    fn an_address_family_left_out_of_the_build_is_a_typed_error() {
        // Not a panic, and not a silent fall back to v4: the enum variant
        // for an IPv6 address does not exist in this build at all.
        let err =
            endpoint("[::1]:80".parse().expect("literal")).expect_err("this build has no IPv6");
        assert_eq!(err.kind(), std::io::ErrorKind::Unsupported);
        assert!(err.to_string().contains("proto-ipv6"), "{err}");
    }

    #[test]
    #[cfg(feature = "proto-ipv4")]
    fn an_ipv4_endpoint_survives_the_conversion_intact() {
        let ep =
            endpoint("192.168.69.1:8080".parse().expect("literal")).expect("v4 is compiled in");
        assert_eq!(ep.port, 8080);
        assert_eq!(
            ep.addr,
            embassy_net::IpAddress::Ipv4(core::net::Ipv4Addr::new(192, 168, 69, 1))
        );
    }
}
