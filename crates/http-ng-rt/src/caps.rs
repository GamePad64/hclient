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

/// Socket options are applied in http-ng **once**, on the `socket2::Socket`,
/// and the runtime only adopts the descriptor (`TcpAdoptStd`). Otherwise
/// every runtime crate would rewrite this whole rigmarole again.
///
/// # `default()` is all-off, and that was re-decided rather than inherited
///
/// Nagle's algorithm costs the head of a `Native` TLS exchange **41 ms** on
/// loopback — measured from the server's side of the wire in
/// `http-ng-native`'s `tests/nagle_cost.rs`, and 0.9 ms with `nodelay` set.
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
/// So the opinion lives where the protocol is: `http_ng_native::Native::new`
/// asks for `nodelay`, and asks only where the runtime declares it applies
/// it.
#[derive(Debug, Clone, Default)]
pub struct TcpOpts {
    /// `TCP_NODELAY` — Nagle's algorithm off. See the type's own doc for
    /// why `default()` leaves it `false` and who turns it on.
    pub nodelay: bool,
    pub keepalive: Option<Duration>,
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
impl std::fmt::Display for UnsupportedTcpOpts {
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

impl std::error::Error for UnsupportedTcpOpts {}

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
    /// [`CancelSupport::None`](http_ng_core::CancelSupport::None) and
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
}

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
/// infect. The justification is `amendment-C5`
/// (`docs/superpowers/specs/2026-08-05-http-ng-design.md`), an amendment
/// separate from C1/C2: those two are about erasing auto-traits in `dyn
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
    fn run<T, F>(&self, f: F) -> impl Future<Output = Result<T, Cancelled>>
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

    #[test]
    fn tcp_opts_default_is_conservative() {
        // All SIX fields, not four: a hand-written `Default` that set
        // `send_buffer_size`/`recv_buffer_size` to `Some(1 << 20)` would
        // pass this test unnoticed if only the other four were checked
        // (carried finding, review Task 1). Today `#[derive(Default)]`
        // gives `None` by construction, but the test's name promises the
        // whole struct — so the test must check the whole struct.
        let o = TcpOpts::default();
        // "The user turns nodelay on, not us" is how this line read until
        // the 41 ms was measured, and it was half right: the user, or the
        // transport that knows what protocol is about to be spoken —
        // `http_ng_native::Native::new`, which asks for it and only where
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
    fn all_six_set() -> TcpOpts {
        TcpOpts {
            nodelay: true,
            keepalive: Some(Duration::from_secs(30)),
            local_address: Some(IpAddr::from([127, 0, 0, 1])),
            send_buffer_size: Some(4096),
            recv_buffer_size: Some(4096),
            reuse_address: true,
        }
    }

    const NAMES: [&str; 6] = [
        "nodelay",
        "keepalive",
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
            2 => can.local_address = false,
            3 => can.send_buffer_size = false,
            4 => can.recv_buffer_size = false,
            5 => can.reuse_address = false,
            _ => unreachable!("NAMES has six entries"),
        }
        can
    }

    #[test]
    fn reject_unsupported_is_a_no_op_against_all() {
        // The claim `TcpConnect::APPLIES`' doc makes about the two shipped
        // runtimes: they apply the whole set, so the check they don't call
        // could not have refused anything anyway.
        assert!(
            all_six_set()
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
        // Six separate cases, not one: an implementation that named a
        // fixed option, or the first one it found, would pass a test that
        // only ever withheld `nodelay`.
        for (i, name) in NAMES.iter().enumerate() {
            let err = all_six_set()
                .reject_unsupported(all_but(i))
                .expect_err("the one option this runtime cannot apply was set");
            let msg = err.to_string();
            assert!(
                msg.contains(name),
                "the error for a withheld {name} must name it, got: {msg}"
            );
            for other in NAMES.iter().filter(|o| *o != name) {
                assert!(
                    !msg.contains(other),
                    "only {name} was withheld, but the error also names {other}: {msg}"
                );
            }
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
        let err = all_six_set()
            .reject_unsupported(all_but(0))
            .expect_err("nodelay was withheld");
        let msg = err.to_string();
        assert!(msg.contains("TcpConnect::APPLIES"), "{msg}");
    }

    #[test]
    fn all_offending_options_are_named_not_only_the_first() {
        let err = all_six_set()
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
        let err = all_six_set()
            .reject_unsupported(all_but(2))
            .expect_err("local_address was withheld");
        assert_eq!(err.kind(), std::io::ErrorKind::Unsupported);
        let payload = err
            .get_ref()
            .and_then(|e| e.downcast_ref::<UnsupportedTcpOpts>())
            .expect("the typed payload survives the trip through io::Error");
        assert_eq!(payload.names().collect::<Vec<_>>(), ["local_address"]);
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
        // workspace suite otherwise: 878/878, measured (W7 mutation M4).
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
                self: std::pin::Pin<&mut Self>,
                _: &mut std::task::Context<'_>,
                _: hyper::rt::ReadBufCursor<'_>,
            ) -> std::task::Poll<std::io::Result<()>> {
                unreachable!("this runtime never connects")
            }
        }
        impl hyper::rt::Write for NeverIo {
            fn poll_write(
                self: std::pin::Pin<&mut Self>,
                _: &mut std::task::Context<'_>,
                _: &[u8],
            ) -> std::task::Poll<std::io::Result<usize>> {
                unreachable!("this runtime never connects")
            }
            fn poll_flush(
                self: std::pin::Pin<&mut Self>,
                _: &mut std::task::Context<'_>,
            ) -> std::task::Poll<std::io::Result<()>> {
                unreachable!("this runtime never connects")
            }
            fn poll_shutdown(
                self: std::pin::Pin<&mut Self>,
                _: &mut std::task::Context<'_>,
            ) -> std::task::Poll<std::io::Result<()>> {
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
        let err = all_six_set()
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
