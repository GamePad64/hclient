//! Every way a runtime can refuse, and all four are the same refusal.
//!
//! This crate is seams and no implementation, so nothing here fails the
//! way work fails — there is no socket to time out and no query to lose.
//! What is left is a runtime saying **no** to something a caller asked
//! for, and that is what unites these four: three are a capability the
//! platform does not have ([`UnsupportedTcpOpts`],
//! [`UnsupportedUdpOffload`], [`UnixSocketsUnsupported`]) and the fourth
//! is a capability withdrawn mid-flight ([`Cancelled`], the thread pool
//! going away before the work started).
//!
//! **The refusal is the point rather than a shortcoming.** Every one of
//! them exists because the alternative is a setting silently ignored, and
//! the two list-shaped ones say so in the same way: `Display` names
//! **every** offending option, because a caller who fixed the one the
//! message mentioned would otherwise meet a second identical failure. The
//! two are hand-written rather than `thiserror`, for the reason written
//! where they are: the message is a computed list, so the derive would buy
//! nothing and would cost an intermediate `String`.
//!
//! Each is re-exported at the crate root, where it has always been, so no
//! consumer's `use` line moves.

use std::error::Error as StdError;
use std::fmt::Display;

use crate::caps::TcpOptsSupport;

/// The caller set socket options this runtime cannot apply.
///
/// Carried inside an [`std::io::Error`] with
/// [`ErrorKind::Unsupported`](std::io::ErrorKind::Unsupported) by
/// [`TcpOpts::reject_unsupported`](crate::TcpOpts::reject_unsupported), and reachable again through
/// `io::Error::get_ref().downcast_ref()`.
///
/// `Display` names **every** offending option, not just the first: a caller
/// who set two unappliable options and fixed the one the message mentioned
/// would otherwise get a second, identical-looking failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UnsupportedTcpOpts {
    /// `true` where the caller asked for an option the runtime does not
    /// apply — i.e. set in [`TcpOpts`](crate::TcpOpts) and absent from
    /// [`TcpConnect::APPLIES`](crate::TcpConnect::APPLIES).
    pub(crate) missing: TcpOptsSupport,
}

impl UnsupportedTcpOpts {
    /// The offending option names, in [`TcpOpts`](crate::TcpOpts)' own field order.
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

/// A runtime that declares no Unix-domain support was asked for a
/// connection to one.
///
/// Reachable only past [`TcpConnect::SUPPORTS_UNIX`](crate::TcpConnect::SUPPORTS_UNIX), which
/// `hclient_native::Native::unix_socket` checks at the call that
/// configures it — so a caller normally meets the refusal where they
/// wrote the path, not on the wire.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("this runtime does not connect to Unix-domain sockets")]
pub struct UnixSocketsUnsupported;

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

/// The caller asked for an offload this socket does not have.
///
/// Carried inside an [`std::io::Error`] with
/// [`ErrorKind::Unsupported`](std::io::ErrorKind::Unsupported) by
/// [`Datagrams::reject_unsupported`](crate::Datagrams::reject_unsupported), and reachable again through
/// `io::Error::get_ref().downcast_ref()` — the shape [`UnsupportedTcpOpts`]
/// already uses, so a caller who wants to react per-offload does not have
/// to scrape `Display`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UnsupportedUdpOffload {
    pub(crate) gso: bool,
    pub(crate) ecn: bool,
}

impl UnsupportedUdpOffload {
    /// The offending offload names. Every one of them, not just the first.
    pub fn names(&self) -> impl Iterator<Item = &'static str> {
        [("gso", self.gso), ("ecn", self.ecn)]
            .into_iter()
            .filter_map(|(name, bad)| bad.then_some(name))
    }
}

impl Display for UnsupportedUdpOffload {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(
            "this socket does not have these UDP offloads, and does not silently drop them:",
        )?;
        for (i, name) in self.names().enumerate() {
            f.write_str(if i > 0 { ", " } else { " " })?;
            f.write_str(name)?;
        }
        Ok(())
    }
}

impl StdError for UnsupportedUdpOffload {}
