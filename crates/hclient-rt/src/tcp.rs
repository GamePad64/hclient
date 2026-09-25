//! TCP: the options a caller can ask for, which of them a runtime applies,
//! and the connect itself.
//!
//! Same-machine endpoints are a seam of their own, [`IpcConnect`](crate::IpcConnect),
//! which extends this one rather than living on it.

use crate::error::UnsupportedTcp;
use std::future::Future;
use std::net::{IpAddr, SocketAddr};
use std::time::Duration;

/// Socket options are applied in hclient **once**, on the `socket2::Socket`,
/// and the runtime only adopts the descriptor (`TcpAdoptStd`). Otherwise
/// every runtime crate would rewrite this whole rigmarole again.
///
/// # `default()` is all-off, and that was re-decided rather than inherited
///
/// Nagle's algorithm costs the head of a `Native` TLS exchange **41 ms** on
/// loopback — measured from the server's side of the wire in
/// `hclient-native`'s `tests/nagle_cost.rs`, and 0.9 ms with `nodelay` set.
/// Every field here stays `false`/`None` anyway, for two reasons that are
/// not caution:
///
/// - **This is a socket seam, and it does not know who is writing.** The
///   41 ms is the write-write-read pattern of a request over TLS meeting a
///   peer's delayed ACK. A protocol that streams one way is exactly the one
///   Nagle helps, and a default here would impose one caller's protocol on
///   every other caller of the trait.
/// - **A set option is a refusal, not a preference.**
///   [`TcpOpts::reject_unsupported`] fails the connect on a runtime whose
///   [`TcpConnect::TCP_SUPPORT`] does not cover it, and that default is `NONE`.
///   Turning a field on here would turn every connect on a backend that
///   forgot to declare `TCP_SUPPORT` into an `Unsupported` error for an option
///   its caller never mentioned — a performance fix aimed straight at the
///   implementors the `NONE` default was written to protect.
///
/// So the opinion lives where the protocol is: `hclient_native::Native::new`
/// asks for `nodelay`, and asks only where the runtime declares it applies
/// it.
///
/// # Building one
///
/// The struct is `#[non_exhaustive]`, so from another crate it is built
/// with the chained setters rather than a literal —
/// `TcpOpts::default().nodelay(true)` — and a new option can be added
/// without breaking anyone. `TcpOpts::default().nodelay(true)` is one
/// line at the call site and costs nothing when an eleventh option arrives.
///
/// The fields stay `pub` because a runtime **reads** them —
/// `#[non_exhaustive]` blocks construction and matching from outside,
/// not field access.
#[derive(Debug, Clone, Default)]
#[non_exhaustive]
pub struct TcpOpts {
    /// `TCP_NODELAY` — Nagle's algorithm off. See the type's own doc for
    /// why `default()` leaves it `false` and who turns it on.
    pub nodelay: bool,
    /// `TCP_KEEPIDLE` — how long a connection may be idle before the
    /// first probe.
    ///
    /// **One setting in three parts, with
    /// [`keepalive_interval`](Self::keepalive_interval) and
    /// [`keepalive_retries`](Self::keepalive_retries).** Setting *any* of
    /// the three turns `SO_KEEPALIVE` on; each part left `None` keeps the
    /// operating system's value for it. That is `socket2::TcpKeepalive`'s own shape
    /// and it is stated here because the field names do not say it: a
    /// caller who sets only the interval has switched keepalive on, with
    /// the OS's idle time.
    pub keepalive: Option<Duration>,
    /// `TCP_KEEPINTVL` — the gap between probes once they have started.
    ///
    /// Worth setting with [`keepalive`](Self::keepalive) rather than
    /// instead of it: the idle time decides *when a dead peer starts being
    /// noticed* and this decides *how fast the noticing then goes*, and
    /// Linux's defaults are 7200 s and 75 s, so an untouched idle time
    /// makes the interval nearly irrelevant.
    pub keepalive_interval: Option<Duration>,
    /// `TCP_KEEPCNT` — how many unanswered probes end the connection.
    pub keepalive_retries: Option<u32>,
    /// `SO_BINDTODEVICE` — the interface this socket must use, by name.
    ///
    /// Not [`local_address`](Self::local_address) under another name: an
    /// address binds the *source address*, and the kernel still routes by
    /// its table, so a request can leave through a different interface
    /// that happens to hold the same address. This binds the **interface**,
    /// which is what a caller on a multi-homed host or inside a VRF
    /// actually means. Linux, Android and Fuchsia only — see
    /// [`TcpSupport`], which is where a runtime says so per target.
    ///
    /// A `String` rather than a `&'static str` because an interface name
    /// is configuration a caller reads at run time, and rather than bytes
    /// because every interface name on every platform that has this option
    /// is ASCII.
    pub bind_device: Option<String>,
    /// `TCP_USER_TIMEOUT` — how long transmitted data may stay
    /// unacknowledged before the connection is dropped.
    ///
    /// **The one option here that catches a peer which vanished
    /// mid-transfer**, where keepalive only catches an *idle* one: probes
    /// are sent when nothing is in flight, so a connection with unsent
    /// acknowledgements sits in retransmission for minutes with keepalive
    /// never firing. Linux, Android, Fuchsia and Cygwin only.
    ///
    /// It overlaps `Timeouts::between_bytes` and does not replace it: this
    /// is the kernel's, applies to a socket rather than to an exchange, and
    /// is the only one of the two that a build with no `Client` above it
    /// can reach.
    pub user_timeout: Option<Duration>,
    /// The source address to bind before connecting; `None` lets the
    /// kernel choose.
    ///
    /// Only the address is bound — the port is always ephemeral. It must
    /// be of the same family as the address being connected to.
    pub local_address: Option<IpAddr>,
    /// `SO_SNDBUF` — the kernel's send buffer, in bytes; `None` keeps the
    /// operating system's default.
    ///
    /// The kernel may round or double the value (Linux doubles it for
    /// bookkeeping), so what reads back need not equal what was set.
    pub send_buffer_size: Option<usize>,
    /// `SO_RCVBUF` — the kernel's receive buffer, in bytes; `None` keeps
    /// the operating system's default.
    ///
    /// The same rounding as [`send_buffer_size`](Self::send_buffer_size)
    /// applies.
    pub recv_buffer_size: Option<usize>,
    /// `SO_REUSEADDR` — allow binding a local address still held by a
    /// connection in `TIME_WAIT`.
    ///
    /// Only matters together with [`local_address`](Self::local_address);
    /// `false` leaves the option unset.
    pub reuse_address: bool,
}

impl TcpOpts {
    // Maintainer notes (not rendered):
    // Chained setters, and they exist so this struct can grow.
    //
    // It is `#[non_exhaustive]`, so `TcpOpts { nodelay: true,
    // ..Default::default() }` does not compile from another crate — and
    // that expression was the whole argument this type's own doc used to
    // make **against** the attribute. Setters answer the argument rather
    // than losing to it: `TcpOpts::default().nodelay(true)` is one line
    // at the call site and costs nothing when an eleventh option arrives.
    //
    // The measurement behind that: this struct went from six fields to
    // ten in thirty-odd commits, and `TCP_FASTOPEN`, DSCP and `SO_MARK`
    // are all still unwritten. Each of those was a major version before
    // the attribute and is additive after it.
    /// Set `TCP_NODELAY`.
    #[must_use]
    pub fn nodelay(mut self, value: bool) -> Self {
        self.nodelay = value;
        self
    }

    /// Set the keepalive idle time.
    #[must_use]
    pub fn keepalive(mut self, value: Option<Duration>) -> Self {
        self.keepalive = value;
        self
    }

    /// Set the interval between keepalive probes.
    #[must_use]
    pub fn keepalive_interval(mut self, value: Option<Duration>) -> Self {
        self.keepalive_interval = value;
        self
    }

    /// Set how many keepalive probes go unanswered before the kernel gives up.
    #[must_use]
    pub fn keepalive_retries(mut self, value: Option<u32>) -> Self {
        self.keepalive_retries = value;
        self
    }

    /// Set the interface to bind to.
    #[must_use]
    pub fn bind_device(mut self, value: Option<String>) -> Self {
        self.bind_device = value;
        self
    }

    /// Set `TCP_USER_TIMEOUT`.
    #[must_use]
    pub fn user_timeout(mut self, value: Option<Duration>) -> Self {
        self.user_timeout = value;
        self
    }

    /// Set the source address.
    #[must_use]
    pub fn local_address(mut self, value: Option<IpAddr>) -> Self {
        self.local_address = value;
        self
    }

    /// Set `SO_SNDBUF`.
    #[must_use]
    pub fn send_buffer_size(mut self, value: Option<usize>) -> Self {
        self.send_buffer_size = value;
        self
    }

    /// Set `SO_RCVBUF`.
    #[must_use]
    pub fn recv_buffer_size(mut self, value: Option<usize>) -> Self {
        self.recv_buffer_size = value;
        self
    }

    /// Set `SO_REUSEADDR`.
    #[must_use]
    pub fn reuse_address(mut self, value: bool) -> Self {
        self.reuse_address = value;
        self
    }
}

/// Which of [`TcpOpts`]' fields a runtime can actually apply.
///
/// One `bool` per field of `TcpOpts`, not a count and not a bitflags crate:
/// the error a caller gets has to name the option it asked for, and a
/// field-per-field mirror is the only shape that can.
///
/// # Building one
///
/// Chained `const` setters, so a runtime states its support from
/// [`Self::NONE`] and this type can grow.
///
/// `const` rather than plain, because the value a runtime writes is an
/// associated **constant** — `TcpConnect::TCP_SUPPORT` — computed with
/// `cfg!`. So the ordinary shape is
/// `TcpSupport::NONE.nodelay(true).bind_device(cfg!(target_os = "linux"))`,
/// which reads as the claim it is.
///
/// ```
/// use hclient_rt::TcpSupport;
///
/// const SUPPORT: TcpSupport = TcpSupport::NONE
///     .nodelay(true)
///     .keepalive(true)
///     .bind_device(cfg!(target_os = "linux"));
/// assert!(SUPPORT.nodelay);
/// assert!(!SUPPORT.reuse_address);
/// ```
///
/// Starting from `NONE` rather than from a literal is also the
/// understating direction, which this seam's own rule requires: an
/// understated claim costs a caller a named `Unsupported`, an
/// overstated one costs them an option silently not applied.
// Deliberately many bools, one per `TcpOpts` field — see this type's own
// doc above and AGENTS.md "A capability that answers yes or no is a
// `bool`".
#[allow(
    clippy::struct_excessive_bools,
    reason = "Which of [`TcpOpts`]' six fields a runtime can actually apply. One `bool` per field of `TcpOpts`, not a count and not a bitflags crate: the error a caller gets has to name the option it asked for, and a field-per-field mirror is the only shape that can. Deliberately many bools, one per `TcpOpts`..."
)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct TcpSupport {
    /// The runtime applies [`TcpOpts::nodelay`](TcpOpts#structfield.nodelay).
    pub nodelay: bool,
    /// The runtime applies [`TcpOpts::keepalive`](TcpOpts#structfield.keepalive),
    /// the idle time before the first probe.
    pub keepalive: bool,
    /// The runtime applies
    /// [`TcpOpts::keepalive_interval`](TcpOpts#structfield.keepalive_interval).
    pub keepalive_interval: bool,
    /// The runtime applies
    /// [`TcpOpts::keepalive_retries`](TcpOpts#structfield.keepalive_retries),
    /// which some targets (OpenBSD, Redox, Solaris) cannot set.
    pub keepalive_retries: bool,
    /// `SO_BINDTODEVICE`, which exists on Linux, Android and Fuchsia and
    /// nowhere else — so a runtime that sets this **must** decide it per
    /// target rather than in one constant.
    pub bind_device: bool,
    /// `TCP_USER_TIMEOUT`, Linux/Android/Fuchsia/Cygwin — the same
    /// per-target rule as [`bind_device`](Self::bind_device).
    pub user_timeout: bool,
    /// The runtime applies
    /// [`TcpOpts::local_address`](TcpOpts#structfield.local_address).
    pub local_address: bool,
    /// The runtime applies
    /// [`TcpOpts::send_buffer_size`](TcpOpts#structfield.send_buffer_size).
    pub send_buffer_size: bool,
    /// The runtime applies
    /// [`TcpOpts::recv_buffer_size`](TcpOpts#structfield.recv_buffer_size).
    pub recv_buffer_size: bool,
    /// The runtime applies
    /// [`TcpOpts::reuse_address`](TcpOpts#structfield.reuse_address).
    pub reuse_address: bool,
}

impl TcpSupport {
    /// Nothing applied — the default for [`TcpConnect::TCP_SUPPORT`], and the
    /// conservative base a runtime turns individual fields on from.
    pub const NONE: Self = Self {
        nodelay: false,
        keepalive: false,
        keepalive_interval: false,
        keepalive_retries: false,
        bind_device: false,
        user_timeout: false,
        local_address: false,
        send_buffer_size: false,
        recv_buffer_size: false,
        reuse_address: false,
    };

    /// Claim, or disclaim, `nodelay`.
    #[must_use]
    pub const fn nodelay(mut self, applies: bool) -> Self {
        self.nodelay = applies;
        self
    }

    /// Claim, or disclaim, `keepalive`.
    #[must_use]
    pub const fn keepalive(mut self, applies: bool) -> Self {
        self.keepalive = applies;
        self
    }

    /// Claim, or disclaim, `keepalive_interval`.
    #[must_use]
    pub const fn keepalive_interval(mut self, applies: bool) -> Self {
        self.keepalive_interval = applies;
        self
    }

    /// Claim, or disclaim, `keepalive_retries`.
    #[must_use]
    pub const fn keepalive_retries(mut self, applies: bool) -> Self {
        self.keepalive_retries = applies;
        self
    }

    /// Claim, or disclaim, `bind_device`.
    #[must_use]
    pub const fn bind_device(mut self, applies: bool) -> Self {
        self.bind_device = applies;
        self
    }

    /// Claim, or disclaim, `user_timeout`.
    #[must_use]
    pub const fn user_timeout(mut self, applies: bool) -> Self {
        self.user_timeout = applies;
        self
    }

    /// Claim, or disclaim, `local_address`.
    #[must_use]
    pub const fn local_address(mut self, applies: bool) -> Self {
        self.local_address = applies;
        self
    }

    /// Claim, or disclaim, `send_buffer_size`.
    #[must_use]
    pub const fn send_buffer_size(mut self, applies: bool) -> Self {
        self.send_buffer_size = applies;
        self
    }

    /// Claim, or disclaim, `recv_buffer_size`.
    #[must_use]
    pub const fn recv_buffer_size(mut self, applies: bool) -> Self {
        self.recv_buffer_size = applies;
        self
    }

    /// Claim, or disclaim, `reuse_address`.
    #[must_use]
    pub const fn reuse_address(mut self, applies: bool) -> Self {
        self.reuse_address = applies;
        self
    }
}

impl TcpOpts {
    /// Fail when the caller set an option `support` says this runtime does not
    /// apply — the one sanctioned answer to an option a runtime cannot
    /// honour, since silently ignoring it is not one.
    ///
    /// Only fields that are actually *set* can offend: [`TcpOpts::default`]
    /// is all-off, so even a runtime with [`TcpSupport::NONE`] still
    /// serves every caller that never asked for anything.
    ///
    /// # Errors
    ///
    /// An [`std::io::ErrorKind::Unsupported`] carrying [`UnsupportedTcp`]
    /// when a field this caller set is one `support` says the runtime cannot
    /// apply, naming every such field rather than just the first.
    pub fn reject_unsupported(&self, support: TcpSupport) -> std::io::Result<()> {
        let missing = TcpSupport {
            nodelay: self.nodelay && !support.nodelay,
            keepalive: self.keepalive.is_some() && !support.keepalive,
            keepalive_interval: self.keepalive_interval.is_some() && !support.keepalive_interval,
            keepalive_retries: self.keepalive_retries.is_some() && !support.keepalive_retries,
            bind_device: self.bind_device.is_some() && !support.bind_device,
            user_timeout: self.user_timeout.is_some() && !support.user_timeout,
            local_address: self.local_address.is_some() && !support.local_address,
            send_buffer_size: self.send_buffer_size.is_some() && !support.send_buffer_size,
            recv_buffer_size: self.recv_buffer_size.is_some() && !support.recv_buffer_size,
            reuse_address: self.reuse_address && !support.reuse_address,
        };
        if missing == TcpSupport::NONE {
            return Ok(());
        }
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            UnsupportedTcp { missing },
        ))
    }
}

/// A runtime that can open an outgoing TCP connection.
///
/// This is the seam a native transport dials through: a runtime crate
/// implements it once for its handle type (a unit struct, or a wrapper
/// around a runtime handle), and the transport above it puts TLS and HTTP
/// on whatever [`Stream`](Self::Stream) comes back. A runtime also
/// implements [`Timer`](crate::Timer), and optionally [`Blocking`](crate::Blocking),
/// [`Spawn`](crate::Spawn), [`UdpBind`](crate::UdpBind) and
/// [`IpcConnect`](crate::IpcConnect) — each a separate trait, so a runtime
/// without, say, file descriptors implements only what it has.
///
/// # Implementing it
///
/// - Declare [`TCP_SUPPORT`](Self::TCP_SUPPORT): which [`TcpOpts`] fields
///   this runtime applies **on the target being compiled for**, built from
///   [`TcpSupport::NONE`] with `cfg!` where support differs by platform.
///   Leaving it out claims nothing, which is safe but refuses every option.
/// - In [`connect`](Self::connect), call
///   [`TcpOpts::reject_unsupported`] with that same constant before
///   anything else, then apply every option that is set, then connect.
/// - Hand back a stream implementing `futures-io`'s
///   [`AsyncRead`](futures_io::AsyncRead) and
///   [`AsyncWrite`](futures_io::AsyncWrite) plus [`Shutdown`](crate::Shutdown),
///   whose `poll_shutdown` sends FIN and keeps the read half open. Its
///   `poll_close` should do the same half-close.
///
/// On platforms with file descriptors a runtime can build and configure a
/// `std` socket itself and adopt it; [`TcpAdoptStd`] is that second half.
///
/// # Same-machine endpoints
///
/// Unix-domain sockets are [`IpcConnect`](crate::IpcConnect), which
/// requires this trait: a same-machine connect must hand back the same
/// [`Stream`](Self::Stream) type, so one transport carries both. A runtime
/// with no such endpoints implements this trait alone.
///
/// # Example
///
/// A deliberately tiny runtime over blocking `std` sockets. It is correct
/// as a contract illustration — it refuses what it cannot apply and
/// half-closes properly — but blocking calls inside `poll_*` stall an
/// async executor, so a real runtime registers the socket with a reactor.
///
/// ```no_run
/// use std::io::{self, Read, Write};
/// use std::net::{Shutdown as HalfClose, SocketAddr, TcpStream};
/// use std::pin::Pin;
/// use std::task::{Context, Poll};
///
/// use hclient_rt::{Shutdown, TcpConnect, TcpOpts, TcpSupport};
///
/// struct BlockingRuntime;
///
/// struct BlockingStream(TcpStream);
///
/// impl futures_io::AsyncRead for BlockingStream {
///     fn poll_read(
///         self: Pin<&mut Self>,
///         _: &mut Context<'_>,
///         buf: &mut [u8],
///     ) -> Poll<io::Result<usize>> {
///         Poll::Ready((&self.0).read(buf))
///     }
/// }
///
/// impl futures_io::AsyncWrite for BlockingStream {
///     fn poll_write(
///         self: Pin<&mut Self>,
///         _: &mut Context<'_>,
///         buf: &[u8],
///     ) -> Poll<io::Result<usize>> {
///         Poll::Ready((&self.0).write(buf))
///     }
///     fn poll_flush(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<io::Result<()>> {
///         Poll::Ready((&self.0).flush())
///     }
///     // The same half-close as `poll_shutdown`, as `Shutdown` recommends.
///     fn poll_close(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
///         self.poll_shutdown(cx)
///     }
/// }
///
/// impl Shutdown for BlockingStream {
///     fn poll_shutdown(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<io::Result<()>> {
///         // Close the writing half only: the response is still to be read.
///         Poll::Ready(self.0.shutdown(HalfClose::Write))
///     }
/// }
///
/// impl TcpConnect for BlockingRuntime {
///     type Stream = BlockingStream;
///
///     // `std::net::TcpStream` can set Nagle, and nothing else here.
///     const TCP_SUPPORT: TcpSupport = TcpSupport::NONE.nodelay(true);
///
///     type Connecting<'a> = std::future::Ready<io::Result<BlockingStream>>;
///
///     fn connect<'a>(&'a self, addr: SocketAddr, opts: &TcpOpts) -> Self::Connecting<'a> {
///         std::future::ready((|| {
///             // Refuse, by name, any option this runtime would otherwise drop.
///             opts.reject_unsupported(Self::TCP_SUPPORT)?;
///             let stream = TcpStream::connect(addr)?;
///             stream.set_nodelay(opts.nodelay)?;
///             Ok(BlockingStream(stream))
///         })())
///     }
/// }
/// ```
pub trait TcpConnect {
    /// The connected byte stream [`connect`](Self::connect) hands back.
    ///
    /// Reads and writes are `futures-io`'s traits; [`Shutdown`](crate::Shutdown)
    /// adds the half-close an HTTP/1 client needs to send its request and
    /// still read the response. `Unpin` lets TLS and HTTP layers hold it by
    /// value without pinning it themselves. The same type is returned for
    /// same-machine connections through [`IpcConnect`](crate::IpcConnect),
    /// so a runtime dialling both usually makes this an enum.
    type Stream: ::futures_io::AsyncRead + ::futures_io::AsyncWrite + crate::io::Shutdown + Unpin;

    /// Which [`TcpOpts`] fields this runtime actually applies.
    ///
    /// Declare it per target — see [`TcpSupport`] for the `const` builder —
    /// and check requests against it in [`connect`](Self::connect). A
    /// transport also reads it at configuration time, so a caller who asks
    /// for an option this runtime lacks is refused before any connection is
    /// attempted.
    ///
    /// # Why the default is `NONE` and not `ALL`
    ///
    /// A default is a claim made by silence, and it must never be stronger
    /// than the truth. `ALL` would make a
    /// backend that forgot the line claim it applies every option; `NONE`
    /// makes it understate itself, so the worst case is one refused connect
    /// too many rather than an option dropped on the floor without a trace.
    // Maintainer notes (not rendered):
    // A default is a claim made by silence, and it must never be stronger
    // than the truth — the rule written down on
    // [`false`](false) and
    // learned from `RedirectSupport::Transparent`.
    const TCP_SUPPORT: TcpSupport = TcpSupport::NONE;

    // Maintainer notes (not rendered):
    // **An associated type, not an RPITIT, and it demands nothing.**
    // A consumer that must prove its own future `Send` — `Native`, so
    // that `hclient::Client`'s can be — has to *name* this one, and
    // `impl Future` has no name. An associated type is nameable while
    // leaving the answer to the implementor: `Tokio` and `Smol` box it
    // `Send`, `hclient-rt-embassy` boxes it plain, because
    // `embassy_net::Stack` is `&RefCell<..>` and its executor is
    // single-threaded. Both satisfy this trait. Writing `+ Send` into the
    // seam instead would have excluded the second, which is the whole
    // difference between naming a property and requiring it.
    /// The future [`connect`](Self::connect) hands back.
    ///
    /// **An associated type, not an `impl Future`, and it demands
    /// nothing.** A consumer that must prove its own future `Send` has to
    /// *name* this one, and `impl Future` has no name. An associated type
    /// is nameable while leaving the answer to the implementor: a runtime
    /// on a multi-threaded executor boxes it `Send`, one on a
    /// single-threaded executor boxes it plain, and both satisfy this
    /// trait.
    ///
    /// It costs the implementor a `Box::pin`, because an `async fn` body
    /// has no name either — one allocation per connect, against a round
    /// trip.
    ///
    /// `opts` is **not** borrowed by the future: the lifetime here is
    /// `&self`'s alone, so an implementor clones what it needs. That is
    /// what keeps the type one parameter wide rather than two.
    type Connecting<'a>: Future<Output = std::io::Result<Self::Stream>>
    where
        Self: 'a;

    /// Open a TCP connection to `addr`, applying `opts`.
    ///
    /// `addr` is already resolved — name resolution happens above this
    /// seam — and one call makes one attempt; racing address families is
    /// the caller's business.
    ///
    /// # The options are not optional
    ///
    /// A runtime that cannot apply an option the caller set **must fail
    /// this call** — [`TcpOpts::reject_unsupported`] is the shared way to
    /// do it, and the error it builds names the option. Ignoring it is not
    /// an available answer: `connect` returns `io::Result<Self::Stream>`
    /// and nothing else, so an option quietly dropped here is dropped
    /// without a trace anywhere in the stack. Pass it
    /// [`Self::TCP_SUPPORT`]. Options left unset (`None` or `false`) must
    /// leave the operating system's defaults alone.
    ///
    /// On platforms with file descriptors the whole set is applied outside
    /// the runtime, on a `socket2::Socket`, and the runtime only adopts the
    /// finished socket ([`TcpAdoptStd`]) — which is why both shipped
    /// runtimes declare every option their target has and refuse only the
    /// ones it has not.
    ///
    /// # Errors
    ///
    /// The returned future resolves to an error of kind
    /// [`std::io::ErrorKind::Unsupported`] carrying
    /// [`UnsupportedTcp`] when `opts` asks for something this runtime does
    /// not apply, and otherwise to whatever the operating system answers
    /// while creating, configuring or connecting the socket.
    fn connect<'a>(&'a self, addr: SocketAddr, opts: &TcpOpts) -> Self::Connecting<'a>;
}

/// On platforms with file descriptors, the whole set of socket options is
/// applied outside the runtime, and the runtime only adopts the finished
/// socket.
pub trait TcpAdoptStd: TcpConnect {
    /// # Errors
    ///
    /// Whatever the OS or the runtime's own reactor registration returns
    /// while taking ownership of `std` — switching it to non-blocking mode
    /// and handing the descriptor to the runtime's async socket type, both
    /// of which are OS calls that can fail on an already-broken or
    /// already-closed descriptor.
    fn adopt(&self, std: std::net::TcpStream) -> std::io::Result<Self::Stream>;
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::pin::Pin;
    use std::task::Context;
    use std::task::Poll;

    #[test]
    fn tcp_opts_default_is_conservative() {
        // All SIX fields, not four: a hand-written `Default` that set
        // `send_buffer_size`/`recv_buffer_size` to `Some(1 << 20)` would
        // pass this test unnoticed if only the other four were checked
        // if only the other four were checked. `#[derive(Default)]` gives
        // `None` by construction, but the test's name promises the
        // whole struct — so the test must check the whole struct.
        let o = TcpOpts::default();
        // "The user turns nodelay on, not us" is how this line read until
        // the 41 ms was measured, and it was half right: the user, or the
        // transport that knows what protocol is about to be spoken —
        // `hclient_native::Native::new`, which asks for it and only where
        // `TcpConnect::TCP_SUPPORT` says the runtime applies it. Not this
        // seam, which cannot know either thing, and where a `true` would
        // become a refused connect on every backend that left `TCP_SUPPORT`
        // at its default. See the type's doc.
        assert!(!o.nodelay, "the seam has no opinion about who is writing");
        assert!(o.keepalive.is_none());
        assert!(o.local_address.is_none());
        assert!(o.send_buffer_size.is_none());
        assert!(o.recv_buffer_size.is_none());
        assert!(!o.reuse_address);
    }

    #[test]
    fn each_setter_sets_its_own_field_and_keeps_the_ones_before_it() {
        // The setters exist so `TcpOpts` can be `#[non_exhaustive]` — they
        // are the *only* way a caller outside this crate builds one — and
        // until this test nothing here read them at all. What pinned them
        // was `hclient-native/tests/tcp_opts.rs`, one crate away and over a
        // real socket; a property this crate's own suite cannot lose is one
        // it should assert.
        //
        // Chained rather than one call per assertion, and that is the half
        // that discriminates: a setter that threw its receiver away and
        // answered `Default::default()` would drop every field set before
        // it, while still looking right to a one-call-per-field test. Ten
        // mutants of exactly that shape survived the suite. So the chain
        // builds up, and the assertions read the whole struct at the end —
        // where a lost field is visible.
        //
        // Compared field by field rather than against `every_field_set()`,
        // because `TcpOpts` derives no `PartialEq` and giving it one to
        // please a test would widen the public surface of a frozen type.
        // The fixture is a struct literal besides, which can only be written
        // inside this crate and so exercises none of this.
        let o = TcpOpts::default()
            .nodelay(true)
            .keepalive(Some(Duration::from_secs(30)))
            .keepalive_interval(Some(Duration::from_secs(5)))
            .keepalive_retries(Some(3))
            .bind_device(Some("lo".to_owned()))
            .user_timeout(Some(Duration::from_secs(20)))
            .local_address(Some(IpAddr::from([127, 0, 0, 1])))
            .send_buffer_size(Some(4096))
            .recv_buffer_size(Some(4096))
            .reuse_address(true);
        assert!(o.nodelay);
        assert_eq!(o.keepalive, Some(Duration::from_secs(30)));
        assert_eq!(o.keepalive_interval, Some(Duration::from_secs(5)));
        assert_eq!(o.keepalive_retries, Some(3));
        assert_eq!(o.bind_device.as_deref(), Some("lo"));
        assert_eq!(o.user_timeout, Some(Duration::from_secs(20)));
        assert_eq!(o.local_address, Some(IpAddr::from([127, 0, 0, 1])));
        assert_eq!(o.send_buffer_size, Some(4096));
        assert_eq!(o.recv_buffer_size, Some(4096));
        assert!(o.reuse_address);

        // The clearing direction too, since every setter takes the value
        // rather than only turning something on: a setter that ignored a
        // `false`/`None` would leave a caller unable to undo a field, and
        // `Default::default()` passes a clearing assertion by construction —
        // which is why this half cannot stand alone.
        let cleared = o
            .nodelay(false)
            .keepalive(None)
            .keepalive_interval(None)
            .keepalive_retries(None)
            .bind_device(None)
            .user_timeout(None)
            .local_address(None)
            .send_buffer_size(None)
            .recv_buffer_size(None)
            .reuse_address(false);
        assert!(!cleared.nodelay);
        assert_eq!(cleared.keepalive, None);
        assert_eq!(cleared.keepalive_interval, None);
        assert_eq!(cleared.keepalive_retries, None);
        assert_eq!(cleared.bind_device, None);
        assert_eq!(cleared.user_timeout, None);
        assert_eq!(cleared.local_address, None);
        assert_eq!(cleared.send_buffer_size, None);
        assert_eq!(cleared.recv_buffer_size, None);
        assert!(!cleared.reuse_address);
    }

    /// Every field of `TcpOpts` set to something a runtime would have to
    /// act on, paired with the `TcpSupport` field that covers it.
    ///
    /// Named for a count until the count changed, which is why it is not
    /// named for one any more: the pairing is what the tests below read,
    /// and a name carrying a number goes stale the first time the struct
    /// grows.
    fn every_field_set() -> TcpOpts {
        TcpOpts {
            nodelay: true,
            keepalive: Some(Duration::from_secs(30)),
            keepalive_interval: Some(Duration::from_secs(5)),
            keepalive_retries: Some(3),
            bind_device: Some("lo".to_owned()),
            user_timeout: Some(Duration::from_secs(20)),
            local_address: Some(IpAddr::from([127, 0, 0, 1])),
            send_buffer_size: Some(4096),
            recv_buffer_size: Some(4096),
            reuse_address: true,
        }
    }

    /// Every option name, in `TcpOpts`' own field order — which is the
    /// order `UnsupportedTcp::names` walks, so this list going stale
    /// is the same failure as that one going stale.
    ///
    /// The length is inferred rather than written: it was `[&str; 6]`, and
    /// a number in a type is one more thing to remember when the struct
    /// grows. It grew.
    const NAMES: &[&str] = &[
        "nodelay",
        "keepalive",
        "keepalive_interval",
        "keepalive_retries",
        "bind_device",
        "user_timeout",
        "local_address",
        "send_buffer_size",
        "recv_buffer_size",
        "reuse_address",
    ];

    /// Every option applied. Local to the tests since `TcpSupport::ALL`
    /// went: see [`TcpSupport`]'s own doc on why no public constant may mean
    /// *every field*.
    fn all() -> TcpSupport {
        TcpSupport {
            nodelay: true,
            keepalive: true,
            keepalive_interval: true,
            keepalive_retries: true,
            bind_device: true,
            user_timeout: true,
            local_address: true,
            send_buffer_size: true,
            recv_buffer_size: true,
            reuse_address: true,
        }
    }

    /// [`all`] with exactly one field turned off, in the same order as
    /// `NAMES` — so a test can walk both together and check that the error
    /// names the one option that was withheld.
    fn all_but(i: usize) -> TcpSupport {
        let mut can = all();
        match i {
            0 => can.nodelay = false,
            1 => can.keepalive = false,
            2 => can.keepalive_interval = false,
            3 => can.keepalive_retries = false,
            4 => can.bind_device = false,
            5 => can.user_timeout = false,
            6 => can.local_address = false,
            7 => can.send_buffer_size = false,
            8 => can.recv_buffer_size = false,
            9 => can.reuse_address = false,
            _ => unreachable!("one arm per NAMES entry"),
        }
        can
    }

    #[test]
    fn reject_unsupported_is_a_no_op_against_all() {
        // The claim `TcpConnect::TCP_SUPPORT`' doc makes about the two shipped
        // runtimes: they apply the whole set, so the check they don't call
        // could not have refused anything anyway.
        assert!(every_field_set().reject_unsupported(all()).is_ok());
    }

    #[test]
    fn a_runtime_that_applies_nothing_still_serves_a_caller_that_asked_for_nothing() {
        // Why `TcpSupport::NONE` is a usable default and not a brick
        // wall: `TcpOpts::default()` sets nothing, and that is what
        // `Native` passes unless the caller called `tcp_opts`.
        assert!(
            TcpOpts::default()
                .reject_unsupported(TcpSupport::NONE)
                .is_ok()
        );
    }

    #[test]
    fn each_unappliable_option_is_named_on_its_own() {
        // One case per option, not one case: an implementation that named
        // a fixed option, or the first one it found, would pass a test
        // that only ever withheld `nodelay`.
        //
        // **Compared as data and not as substrings of the message**, which
        // is what the neighbour above already does and this one did not.
        // It worked while no two names shared a prefix; `keepalive` and
        // `keepalive_interval` ended that, and the failure was the test
        // reporting that a withheld `keepalive_interval` had *also* named
        // `keepalive` — which the message never did.
        for (i, name) in NAMES.iter().enumerate() {
            let err = every_field_set()
                .reject_unsupported(all_but(i))
                .expect_err("the one option this runtime cannot apply was set");
            let named: Vec<&str> = err
                .get_ref()
                .and_then(|e| e.downcast_ref::<UnsupportedTcp>())
                .expect("typed payload")
                .names()
                .collect();
            assert_eq!(
                named,
                [*name],
                "a withheld {name} must be the only option named"
            );
            // And the message really does carry it, since that is what a
            // caller who does not downcast will read.
            assert!(err.to_string().contains(name), "{err}");
        }
    }

    #[test]
    fn the_message_names_the_constant_an_implementor_would_have_to_change() {
        // The other audience for this error is the backend author whose
        // `connect` applies the option perfectly well and whose `TCP_SUPPORT`
        // line is missing — `TokioHandle`, in this workspace, found by
        // measurement rather than by reading. The option's name sends
        // them to their `connect` body; the constant's name sends them to
        // the defect.
        let err = every_field_set()
            .reject_unsupported(all_but(0))
            .expect_err("nodelay was withheld");
        let msg = err.to_string();
        assert!(msg.contains("TcpConnect::TCP_SUPPORT"), "{msg}");
    }

    #[test]
    fn all_offending_options_are_named_not_only_the_first() {
        let err = every_field_set()
            .reject_unsupported(TcpSupport::NONE)
            .expect_err("nothing can be applied and everything was asked for");
        let msg = err.to_string();
        for name in NAMES {
            assert!(msg.contains(name), "{name} missing from: {msg}");
        }
    }

    #[test]
    fn the_error_is_unsupported_and_carries_a_typed_payload() {
        // `ErrorKind::Unsupported` rather than `Other`, and the names
        // reachable as data rather than only by parsing the message —
        // otherwise a caller wanting to react per-option has to scrape
        // `Display`.
        // Indexed through `NAMES` rather than by a literal, so that a
        // field inserted above this one moves the index and the expected
        // name together. It was `all_but(2)` against `["local_address"]`
        // and four fields arrived above it.
        const I: usize = 6;
        let err = every_field_set()
            .reject_unsupported(all_but(I))
            .expect_err("one option was withheld");
        assert_eq!(err.kind(), std::io::ErrorKind::Unsupported);
        let payload = err
            .get_ref()
            .and_then(|e| e.downcast_ref::<UnsupportedTcp>())
            .expect("the typed payload survives the trip through io::Error");
        assert_eq!(payload.names().collect::<Vec<_>>(), [NAMES[I]]);
        assert_eq!(NAMES[I], "local_address", "the index still names it");
    }

    #[test]
    fn an_option_left_unset_is_not_an_offence_even_when_unsupported() {
        // The check is about what the caller ASKED for, not about what the
        // runtime lacks: a runtime that applies nothing owes nothing to a
        // caller who set nothing. Without this distinction
        // `TcpSupport::NONE` would refuse every connect.
        let opts = TcpOpts {
            nodelay: true,
            ..TcpOpts::default()
        };
        let err = opts
            .reject_unsupported(TcpSupport::NONE)
            .expect_err("nodelay was set and cannot be applied");
        let payload = err
            .get_ref()
            .and_then(|e| e.downcast_ref::<UnsupportedTcp>())
            .expect("typed payload");
        assert_eq!(payload.names().collect::<Vec<_>>(), ["nodelay"], "{err}");
    }

    #[test]
    fn a_runtime_that_declares_nothing_applies_nothing() {
        // The default is a claim made by silence, and this is the only
        // test that reads it. All three shipped runtimes declare
        // `TCP_SUPPORT` explicitly — tokio and smol `ALL`, embassy its own
        // two-of-six — so flipping the default to `ALL` passes the whole
        // workspace suite otherwise: 878/878, measured.
        // The rule it protects is that a backend which forgets the line
        // must understate itself, so the worst case is one refused
        // connect too many rather than an option dropped on the floor
        // without a trace.
        struct Forgetful;
        // Never constructed: it exists only so `Forgetful` can satisfy
        // the associated type without a runtime behind it.
        struct NeverIo;
        impl futures_io::AsyncRead for NeverIo {
            fn poll_read(
                self: Pin<&mut Self>,
                _: &mut Context<'_>,
                _: &mut [u8],
            ) -> Poll<std::io::Result<usize>> {
                unreachable!("this runtime never connects")
            }
        }
        impl crate::io::Shutdown for NeverIo {
            fn poll_shutdown(
                self: Pin<&mut Self>,
                _: &mut Context<'_>,
            ) -> Poll<std::io::Result<()>> {
                unreachable!("this runtime never connects")
            }
        }
        impl futures_io::AsyncWrite for NeverIo {
            fn poll_write(
                self: Pin<&mut Self>,
                _: &mut Context<'_>,
                _: &[u8],
            ) -> Poll<std::io::Result<usize>> {
                unreachable!("this runtime never connects")
            }
            fn poll_flush(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<std::io::Result<()>> {
                unreachable!("this runtime never connects")
            }
            fn poll_close(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<std::io::Result<()>> {
                unreachable!("this runtime never connects")
            }
        }
        /// Short and named, so the marker sits on a line `cargo fmt` has
        /// no reason to reflow — the rule C12 records about where a bound
        /// is written.
        type NeverConnect<'a> = Pin<Box<dyn Future<Output = std::io::Result<NeverIo>> + Send + 'a>>; // send-bound-exception: amendment-C15

        impl TcpConnect for Forgetful {
            type Stream = NeverIo;
            // No `TCP_SUPPORT` line, deliberately — that absence is the
            // subject of this test.
            type Connecting<'a>
                = NeverConnect<'a>
            where
                Self: 'a;

            fn connect<'a>(&'a self, _: SocketAddr, _: &TcpOpts) -> Self::Connecting<'a> {
                Box::pin(async move { unreachable!("this runtime never connects") })
            }
        }

        assert_eq!(
            <Forgetful as TcpConnect>::TCP_SUPPORT,
            TcpSupport::NONE,
            "a runtime that declares nothing must not claim to apply anything"
        );
        // And the consequence, not only the constant: a caller who asks
        // such a runtime for all six gets all six refused by name, rather
        // than silently honoured on paper.
        let err = every_field_set()
            .reject_unsupported(<Forgetful as TcpConnect>::TCP_SUPPORT)
            .expect_err("a runtime that applies nothing must refuse everything asked of it");
        let payload = err
            .get_ref()
            .and_then(|e| e.downcast_ref::<UnsupportedTcp>())
            .expect("typed payload");
        assert_eq!(payload.names().collect::<Vec<_>>(), NAMES);
    }
}
