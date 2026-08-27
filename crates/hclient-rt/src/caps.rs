use futures_core::future::BoxFuture;
use std::error::Error as StdError;
use std::fmt::Display;
use std::future::Future;
use std::net::{IpAddr, SocketAddr};
use std::time::Duration;

/// The shape is deliberately copied from `hyper::rt::Executor`: generic
/// over the future, zero bounds in the declaration. `Send` is added by the
/// `impl`, not the trait, so single-threaded runtimes can implement it
/// honestly.
pub trait Spawn<F: Future<Output = ()>> {
    fn spawn(&self, f: F);
}

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
///   [`TcpConnect::APPLIES`] does not cover it, and that default is `NONE`.
///   Turning a field on here would turn every connect on a backend that
///   forgot to declare `APPLIES` into an `Unsupported` error for an option
///   its caller never mentioned — a performance fix aimed straight at the
///   implementors the `NONE` default was written to protect.
///
/// So the opinion lives where the protocol is: `hclient_native::Native::new`
/// asks for `nodelay`, and asks only where the runtime declares it applies
/// it.
#[derive(Debug, Clone, Default)]
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
    /// [`TcpOptsSupport`], which is where a runtime says so per target.
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
    pub local_address: Option<IpAddr>,
    pub send_buffer_size: Option<usize>,
    pub recv_buffer_size: Option<usize>,
    pub reuse_address: bool,
}

/// Which of [`TcpOpts`]' six fields a runtime can actually apply.
///
/// One `bool` per field of `TcpOpts`, not a count and not a bitflags crate:
/// the error a caller gets has to name the option it asked for, and a
/// field-per-field mirror is the only shape that can.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TcpOptsSupport {
    pub nodelay: bool,
    pub keepalive: bool,
    pub keepalive_interval: bool,
    pub keepalive_retries: bool,
    /// `SO_BINDTODEVICE`, which exists on Linux, Android and Fuchsia and
    /// nowhere else — so a runtime that sets this **must** decide it per
    /// target rather than in one constant. `TcpOptsSupport::ALL` is still
    /// literally every field, and is therefore no longer a value any real
    /// runtime can claim on every platform it builds for.
    pub bind_device: bool,
    /// `TCP_USER_TIMEOUT`, Linux/Android/Fuchsia/Cygwin — the same
    /// per-target rule as [`bind_device`](Self::bind_device).
    pub user_timeout: bool,
    pub local_address: bool,
    pub send_buffer_size: bool,
    pub recv_buffer_size: bool,
    pub reuse_address: bool,
}

impl TcpOptsSupport {
    /// Everything applied — what a runtime that hands the whole set to a
    /// `socket2::Socket` says. Both shipped runtimes do exactly that.
    pub const ALL: Self = Self {
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
    };
    /// Nothing applied — the default for [`TcpConnect::APPLIES`], and the
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
}

/// The caller set socket options this runtime cannot apply.
///
/// Carried inside an [`std::io::Error`] with
/// [`ErrorKind::Unsupported`](std::io::ErrorKind::Unsupported) by
/// [`TcpOpts::reject_unsupported`], and reachable again through
/// `io::Error::get_ref().downcast_ref()`.
///
/// `Display` names **every** offending option, not just the first: a caller
/// who set two unappliable options and fixed the one the message mentioned
/// would otherwise get a second, identical-looking failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UnsupportedTcpOpts {
    /// `true` where the caller asked for an option the runtime does not
    /// apply — i.e. set in [`TcpOpts`] and absent from
    /// [`TcpConnect::APPLIES`].
    missing: TcpOptsSupport,
}

impl UnsupportedTcpOpts {
    /// The offending option names, in [`TcpOpts`]' own field order.
    pub fn names(&self) -> impl Iterator<Item = &'static str> {
        let m = self.missing;
        [
            ("nodelay", m.nodelay),
            ("keepalive", m.keepalive),
            ("keepalive_interval", m.keepalive_interval),
            ("keepalive_retries", m.keepalive_retries),
            ("bind_device", m.bind_device),
            ("user_timeout", m.user_timeout),
            ("local_address", m.local_address),
            ("send_buffer_size", m.send_buffer_size),
            ("recv_buffer_size", m.recv_buffer_size),
            ("reuse_address", m.reuse_address),
        ]
        .into_iter()
        .filter_map(|(name, missing)| missing.then_some(name))
    }
}

// Hand-written rather than `thiserror`: the message is a computed list, so
// the derive would buy nothing, and this way the names are written straight
// into the formatter instead of through an intermediate `String`.
impl Display for UnsupportedTcpOpts {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(
            "this runtime cannot apply these TCP socket options, and does not ignore them:",
        )?;
        for (i, name) in self.names().enumerate() {
            f.write_str(if i > 0 { ", " } else { " " })?;
            f.write_str(name)?;
        }
        // Where the claim came from, because half the readers of this
        // message are on the wrong side of it. `TcpConnect::APPLIES`
        // defaults to `NONE`, so a runtime that *does* apply an option and
        // forgot the line refuses it here — which has happened once in
        // this workspace already (`TokioHandle`, found by measurement).
        // Naming the option alone sends that author looking at their
        // `connect` body, where the code is correct and the bug is not.
        f.write_str(" (a runtime that does apply one declares it in TcpConnect::APPLIES)")
    }
}

impl StdError for UnsupportedTcpOpts {}

impl TcpOpts {
    /// Fail when the caller set an option `can` says this runtime does not
    /// apply — the one sanctioned answer to an option a runtime cannot
    /// honour, since silently ignoring it is not one.
    ///
    /// Only fields that are actually *set* can offend: [`TcpOpts::default`]
    /// is all-off, so even a runtime with [`TcpOptsSupport::NONE`] still
    /// serves every caller that never asked for anything.
    ///
    /// A runtime whose [`TcpConnect::APPLIES`] is [`TcpOptsSupport::ALL`]
    /// need not call this at all — the call is a no-op by construction,
    /// which `reject_unsupported_is_a_no_op_against_all` pins.
    pub fn reject_unsupported(&self, can: TcpOptsSupport) -> std::io::Result<()> {
        let missing = TcpOptsSupport {
            nodelay: self.nodelay && !can.nodelay,
            keepalive: self.keepalive.is_some() && !can.keepalive,
            keepalive_interval: self.keepalive_interval.is_some() && !can.keepalive_interval,
            keepalive_retries: self.keepalive_retries.is_some() && !can.keepalive_retries,
            bind_device: self.bind_device.is_some() && !can.bind_device,
            user_timeout: self.user_timeout.is_some() && !can.user_timeout,
            local_address: self.local_address.is_some() && !can.local_address,
            send_buffer_size: self.send_buffer_size.is_some() && !can.send_buffer_size,
            recv_buffer_size: self.recv_buffer_size.is_some() && !can.recv_buffer_size,
            reuse_address: self.reuse_address && !can.reuse_address,
        };
        if missing == TcpOptsSupport::NONE {
            return Ok(());
        }
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            UnsupportedTcpOpts { missing },
        ))
    }
}

pub trait TcpConnect {
    type Stream: hyper::rt::Read + hyper::rt::Write + Unpin;

    /// Which [`TcpOpts`] fields this runtime actually applies.
    ///
    /// # Why the default is `NONE` and not `ALL`
    ///
    /// A default is a claim made by silence, and it must never be stronger
    /// than the truth — the rule written down on
    /// [`CancelSupport::None`](hclient_core::CancelSupport::None) and
    /// learned from `RedirectSupport::Transparent`. `ALL` would make a
    /// backend that forgot the line claim it applies every option; `NONE`
    /// makes it understate itself, so the worst case is one refused connect
    /// too many rather than an option dropped on the floor without a trace.
    const APPLIES: TcpOptsSupport = TcpOptsSupport::NONE;

    /// # The options are not optional
    ///
    /// A runtime that cannot apply an option the caller set **must fail
    /// this call** — [`TcpOpts::reject_unsupported`] is the shared way to
    /// do it, and the error it builds names the option. Ignoring it is not
    /// an available answer: `connect` returns `io::Result<Self::Stream>`
    /// and nothing else, so an option quietly dropped here is dropped
    /// without a trace anywhere in the stack.
    ///
    /// On platforms with file descriptors the whole set is applied outside
    /// the runtime, on a `socket2::Socket`, and the runtime only adopts the
    /// finished socket ([`TcpAdoptStd`]) — which is why both shipped
    /// runtimes declare [`TcpOptsSupport::ALL`] and never have to refuse
    /// anything.
    fn connect(
        &self,
        addr: SocketAddr,
        opts: &TcpOpts,
    ) -> impl Future<Output = std::io::Result<Self::Stream>>;

    /// Whether [`connect_unix`](Self::connect_unix) does anything.
    ///
    /// [`APPLIES`](Self::APPLIES)' shape, and defaulted the same way and
    /// for the same reason: a claim made by silence must never be stronger
    /// than the truth. A runtime that says nothing here refuses the
    /// setting, where one that over-claimed would fail every connect at
    /// the socket instead of at the call that asked.
    ///
    /// It is a `const` rather than something the connect discovers,
    /// because the answer is a property of the runtime and the target and
    /// a caller should learn it at configuration rather than on the wire —
    /// which is what lets `hclient_native::Native::unix_socket` refuse.
    const SUPPORTS_UNIX: bool = false;

    /// Connect to a Unix-domain socket at `path`.
    ///
    /// # Why it is here rather than on a seam of its own
    ///
    /// Because a seam of its own could not be reached. `Native`'s IO type
    /// **is** [`Self::Stream`], so a second trait would have to produce
    /// the same associated type — at which point it is this trait with an
    /// extra method — and putting `R: UnixConnect` on `Native` would tax
    /// every runtime that has no file descriptors. The `fn`-pointer trick
    /// that keeps `Spawn` off `Native`'s signature does not work here:
    /// `spawn` returns `()` where this returns a future, and boxing it
    /// would drop auto traits (spec amendment C1).
    ///
    /// So it is a defaulted method on the seam that already exists —
    /// `TlsConnect::reports_alpn`'s shape, `applies_ech`'s and
    /// `TlsIdentity::presents_client_certs`': a constant defaulted to the
    /// understating value, read by the layer above to decide whether to
    /// **ask**.
    ///
    /// # No `TcpOpts`
    ///
    /// Not an omission: every field of [`TcpOpts`] is a TCP or IP socket
    /// option, and `AF_UNIX` has none of them — no Nagle, no keepalive, no
    /// source address, no interface. A parameter that could only ever be
    /// ignored is worse than no parameter.
    ///
    /// The default is a refusal rather than a panic, and the error carries
    /// [`std::io::ErrorKind::Unsupported`] so a caller who reached it
    /// through some path that skipped
    /// [`SUPPORTS_UNIX`](Self::SUPPORTS_UNIX) still gets an answer rather
    /// than an abort.
    fn connect_unix(
        &self,
        path: &std::path::Path,
    ) -> impl Future<Output = std::io::Result<Self::Stream>> {
        let _ = path;
        async {
            Err(std::io::Error::new(
                std::io::ErrorKind::Unsupported,
                UnixSocketsUnsupported,
            ))
        }
    }
}

/// A runtime that declares no Unix-domain support was asked for a
/// connection to one.
///
/// Reachable only past [`TcpConnect::SUPPORTS_UNIX`], which
/// `hclient_native::Native::unix_socket` checks at the call that
/// configures it — so a caller normally meets the refusal where they
/// wrote the path, not on the wire.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("this runtime does not connect to Unix-domain sockets")]
pub struct UnixSocketsUnsupported;

/// On platforms with file descriptors, the whole set of socket options is
/// applied outside the runtime, and the runtime only adopts the finished
/// socket.
pub trait TcpAdoptStd: TcpConnect {
    fn adopt(&self, std: std::net::TcpStream) -> std::io::Result<Self::Stream>;
}

/// A separate trait, not a method: `getaddrinfo` blocks, and wasm and
/// embedded have no blocking pool at all. The absence of the capability
/// must be a compile error, not an `unimplemented!()` in the runtime.
///
/// **The one place in the whole project where we declare `Send` ourselves**,
/// and here it's honest: both `tokio::task::spawn_blocking` and
/// `blocking::unblock` require `Send + 'static`, and the `Blocking`
/// capability doesn't exist on wasm at all — there's nothing for it to
/// infect. The justification is `amendment-C5` (`docs/exceptions.md`), an
/// amendment separate from C1/C2: those two are about erasing auto-traits in `dyn
/// Trait` on the `Client -> Transport` path, whereas here the bound is
/// declared directly in the signature of a capability trait that simply
/// doesn't exist on wasm.
///
/// The bounds live in `where`, not in the generic parameter list `fn
/// run<T: Send + …>`, so each one can carry its own `send-bound-exception`
/// marker on its own line: the CI `no-declared-send` job matches bound
/// declarations line by line, and a single shared comment after the
/// generic list wouldn't cover it.
///
/// Two distinct failure modes of `f` are not conflated into one channel:
///
/// - A panic in `f` is a bug in the calling code. It must be re-raised as a
///   panic (`std::panic::resume_unwind`, with the original payload), not
///   quietly turned into a value that can be `?`-propagated — otherwise the
///   implementation hides a defect in the caller's code behind a `Result`.
/// - The background thread pool going away (for example, the runtime
///   shutting down while a task is still queued and hasn't started
///   running) is not a bug in the calling code, but an ordinary runtime
///   lifecycle event. The implementation must return [`Cancelled`], not
///   panic: a library panicking on a normal (if rare) runtime-shutdown
///   scenario would contradict the rest of the project ("no silent
///   no-ops... typed error, never a discarded value" — the same principle
///   applied here, just to failure instead of success).
pub trait Blocking {
    /// **A named future, not an RPITIT, and the `Send` costs nobody
    /// anything.** A consumer that has to *prove* its own future `Send` —
    /// `hclient-native`, so that `hclient::Client`'s can be — must be able
    /// to **name** this one, and `impl Future` has no name. A boxed
    /// `Send` future is the honest form here rather than an associated
    /// type per implementor, because this trait already requires
    /// `T: Send` and `F: Send`: a pool that cannot be handed work from
    /// another thread is not one, which is amendment C5's whole argument.
    /// So there is no implementor for whom the weaker form would be true,
    /// and nothing is excluded that was not already.
    ///
    /// The cost is one allocation per blocking call — set against handing
    /// work to a thread pool, which is what the call is for.
    fn run<T, F>(&self, f: F) -> BoxFuture<'_, Result<T, Cancelled>>
    where
        T: Send + 'static, // send-bound-exception: amendment-C5
        F: FnOnce() -> T + Send + 'static; // send-bound-exception: amendment-C5
}

/// The background thread pool that `Blocking::run` was supposed to run on
/// went away before the task got to start — for example, the runtime is
/// shutting down while the task is still queued. No payload: this is not a
/// failure of `f` (`f` never ran at all), but a signal from the runtime
/// that there will be no result.
///
/// A panic in `f`, by contrast, does NOT become `Cancelled` — it is
/// re-raised as a panic by the `Blocking` implementation, see the trait's
/// doc comment.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("blocking task pool went away before the work started")]
pub struct Cancelled;

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
        // `TcpConnect::APPLIES` says the runtime applies it. Not this
        // seam, which cannot know either thing, and where a `true` would
        // become a refused connect on every backend that left `APPLIES`
        // at its default. See the type's doc.
        assert!(!o.nodelay, "the seam has no opinion about who is writing");
        assert!(o.keepalive.is_none());
        assert!(o.local_address.is_none());
        assert!(o.send_buffer_size.is_none());
        assert!(o.recv_buffer_size.is_none());
        assert!(!o.reuse_address);
    }

    /// Every field of `TcpOpts` set to something a runtime would have to
    /// act on, paired with the `TcpOptsSupport` field that covers it.
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
    /// order `UnsupportedTcpOpts::names` walks, so this list going stale
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

    /// `TcpOptsSupport::ALL` with exactly one field turned off, in the same
    /// order as `NAMES` — so a test can walk both together and check that
    /// the error names the one option that was withheld.
    fn all_but(i: usize) -> TcpOptsSupport {
        let mut can = TcpOptsSupport::ALL;
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
        // The claim `TcpConnect::APPLIES`' doc makes about the two shipped
        // runtimes: they apply the whole set, so the check they don't call
        // could not have refused anything anyway.
        assert!(
            every_field_set()
                .reject_unsupported(TcpOptsSupport::ALL)
                .is_ok()
        );
    }

    #[test]
    fn a_runtime_that_applies_nothing_still_serves_a_caller_that_asked_for_nothing() {
        // Why `TcpOptsSupport::NONE` is a usable default and not a brick
        // wall: `TcpOpts::default()` sets nothing, and that is what
        // `Native` passes unless the caller called `tcp_opts`.
        assert!(
            TcpOpts::default()
                .reject_unsupported(TcpOptsSupport::NONE)
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
                .and_then(|e| e.downcast_ref::<UnsupportedTcpOpts>())
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
        // `connect` applies the option perfectly well and whose `APPLIES`
        // line is missing — `TokioHandle`, in this workspace, found by
        // measurement rather than by reading. The option's name sends
        // them to their `connect` body; the constant's name sends them to
        // the defect.
        let err = every_field_set()
            .reject_unsupported(all_but(0))
            .expect_err("nodelay was withheld");
        let msg = err.to_string();
        assert!(msg.contains("TcpConnect::APPLIES"), "{msg}");
    }

    #[test]
    fn all_offending_options_are_named_not_only_the_first() {
        let err = every_field_set()
            .reject_unsupported(TcpOptsSupport::NONE)
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
            .and_then(|e| e.downcast_ref::<UnsupportedTcpOpts>())
            .expect("the typed payload survives the trip through io::Error");
        assert_eq!(payload.names().collect::<Vec<_>>(), [NAMES[I]]);
        assert_eq!(NAMES[I], "local_address", "the index still names it");
    }

    #[test]
    fn an_option_left_unset_is_not_an_offence_even_when_unsupported() {
        // The check is about what the caller ASKED for, not about what the
        // runtime lacks: a runtime that applies nothing owes nothing to a
        // caller who set nothing. Without this distinction
        // `TcpOptsSupport::NONE` would refuse every connect.
        let opts = TcpOpts {
            nodelay: true,
            ..TcpOpts::default()
        };
        let err = opts
            .reject_unsupported(TcpOptsSupport::NONE)
            .expect_err("nodelay was set and cannot be applied");
        let payload = err
            .get_ref()
            .and_then(|e| e.downcast_ref::<UnsupportedTcpOpts>())
            .expect("typed payload");
        assert_eq!(payload.names().collect::<Vec<_>>(), ["nodelay"], "{err}");
    }

    #[test]
    fn a_runtime_that_declares_nothing_applies_nothing() {
        // The default is a claim made by silence, and this is the only
        // test that reads it. All three shipped runtimes declare
        // `APPLIES` explicitly — tokio and smol `ALL`, embassy its own
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
        impl hyper::rt::Read for NeverIo {
            fn poll_read(
                self: Pin<&mut Self>,
                _: &mut Context<'_>,
                _: hyper::rt::ReadBufCursor<'_>,
            ) -> Poll<std::io::Result<()>> {
                unreachable!("this runtime never connects")
            }
        }
        impl hyper::rt::Write for NeverIo {
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
            fn poll_shutdown(
                self: Pin<&mut Self>,
                _: &mut Context<'_>,
            ) -> Poll<std::io::Result<()>> {
                unreachable!("this runtime never connects")
            }
        }
        impl TcpConnect for Forgetful {
            type Stream = NeverIo;
            // No `APPLIES` line, deliberately — that absence is the
            // subject of this test.
            async fn connect(&self, _: SocketAddr, _: &TcpOpts) -> std::io::Result<NeverIo> {
                unreachable!("this runtime never connects")
            }
        }

        assert_eq!(
            <Forgetful as TcpConnect>::APPLIES,
            TcpOptsSupport::NONE,
            "a runtime that declares nothing must not claim to apply anything"
        );
        // And the consequence, not only the constant: a caller who asks
        // such a runtime for all six gets all six refused by name, rather
        // than silently honoured on paper.
        let err = every_field_set()
            .reject_unsupported(<Forgetful as TcpConnect>::APPLIES)
            .expect_err("a runtime that applies nothing must refuse everything asked of it");
        let payload = err
            .get_ref()
            .and_then(|e| e.downcast_ref::<UnsupportedTcpOpts>())
            .expect("typed payload");
        assert_eq!(payload.names().collect::<Vec<_>>(), NAMES);
    }

    #[test]
    fn spawn_is_generic_over_the_future_not_boxed() {
        // The shape is copied from hyper::rt::Executor: generic over F,
        // zero bounds in the declaration. Send is added by the impl, not
        // the trait.
        struct Immediate;
        impl<F: std::future::Future<Output = ()>> Spawn<F> for Immediate {
            fn spawn(&self, f: F) {
                futures_executor::block_on(f)
            }
        }
        let done = std::rc::Rc::new(std::cell::Cell::new(false));
        let d = done.clone();
        // !Send future — the trait allows this.
        Immediate.spawn(async move { d.set(true) });
        assert!(done.get());
    }
}
