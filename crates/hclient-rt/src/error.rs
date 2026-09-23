//! Every way a runtime can refuse, and all four are the same refusal.
//!
//! This crate is seams and no implementation, so nothing here fails the
//! way work fails — there is no socket to time out and no query to lose.
//! What is left is a runtime saying **no** to something a caller asked
//! for, and that is what unites these four: three are a capability the
//! platform does not have ([`UnsupportedTcp`],
//! [`UnsupportedUdp`], [`UnsupportedIpc`]) and the fourth
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

use crate::caps::TcpSupport;

/// The lead of a refusal and every offending name, `", "`-separated after
/// one space — written once for all three, so the three messages cannot
/// drift into three punctuations. The separator is pinned by the TCP
/// refusal's two-name test, the only one of the three that can name two
/// today.
fn refusal(
    f: &mut std::fmt::Formatter<'_>,
    lead: &str,
    names: impl Iterator<Item = &'static str>,
) -> std::fmt::Result {
    f.write_str(lead)?;
    for (i, name) in names.enumerate() {
        f.write_str(if i > 0 { ", " } else { " " })?;
        f.write_str(name)?;
    }
    Ok(())
}

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
pub struct UnsupportedTcp {
    /// `true` where the caller asked for an option the runtime does not
    /// apply — i.e. set in [`TcpOpts`](crate::TcpOpts) and absent from
    /// [`TcpConnect::TCP_SUPPORT`](crate::TcpConnect::TCP_SUPPORT).
    pub(crate) missing: TcpSupport,
}

impl UnsupportedTcp {
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
impl Display for UnsupportedTcp {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        refusal(
            f,
            "this runtime cannot apply these TCP socket options, and does not ignore them:",
            self.names(),
        )?;
        // Where the claim came from, because half the readers of this
        // message are on the wrong side of it. `TcpConnect::TCP_SUPPORT`
        // defaults to `NONE`, so a runtime that *does* apply an option and
        // forgot the line refuses it here — which has happened once in
        // this workspace already (`TokioHandle`, found by measurement).
        // Naming the option alone sends that author looking at their
        // `connect` body, where the code is correct and the bug is not.
        f.write_str(" (a runtime that does apply one declares it in TcpConnect::TCP_SUPPORT)")
    }
}

impl StdError for UnsupportedTcp {}

/// A runtime was asked to dial a kind of same-machine endpoint it does
/// not dial.
///
/// Carried inside an [`std::io::Error`] with
/// [`ErrorKind::Unsupported`](std::io::ErrorKind::Unsupported) by
/// [`RefuseIpc`](crate::RefuseIpc), the shape [`UnsupportedTcp`] and
/// [`UnsupportedUdp`] already use. Reachable only past
/// [`TcpConnect::IPC_SUPPORT`](crate::TcpConnect::IPC_SUPPORT), which
/// `hclient_native::Native::unix_socket` checks at the call that
/// configures it — so a caller normally meets the refusal where they
/// wrote the path, not on the wire.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UnsupportedIpc {
    pub(crate) kind: &'static str,
}

impl UnsupportedIpc {
    /// The refused kinds — one, since an endpoint has one — as
    /// [`IpcAddr::kind`](crate::IpcAddr::kind) names it. An iterator for the
    /// shape [`UnsupportedTcp::names`] and [`UnsupportedUdp::names`] have,
    /// so the three refusals read the same way.
    pub fn names(&self) -> impl Iterator<Item = &'static str> {
        std::iter::once(self.kind)
    }
}

impl Display for UnsupportedIpc {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        refusal(
            f,
            "this runtime cannot dial these same-machine endpoints, and does not fall back:",
            self.names(),
        )?;
        f.write_str(" (a runtime that does dial one declares it in TcpConnect::IPC_SUPPORT)")
    }
}

impl StdError for UnsupportedIpc {}

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
/// `io::Error::get_ref().downcast_ref()` — the shape [`UnsupportedTcp`]
/// already uses, so a caller who wants to react per-offload does not have
/// to scrape `Display`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UnsupportedUdp {
    pub(crate) gso: bool,
}

impl UnsupportedUdp {
    /// The offending offload names. Every one of them, not just the first.
    pub fn names(&self) -> impl Iterator<Item = &'static str> {
        [("gso", self.gso)]
            .into_iter()
            .filter_map(|(name, bad)| bad.then_some(name))
    }
}

impl Display for UnsupportedUdp {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        refusal(
            f,
            "this socket does not have these UDP offloads, and does not silently drop them:",
            self.names(),
        )?;
        f.write_str(" (a socket that does have one declares it in UdpDatagrams::support)")
    }
}

impl StdError for UnsupportedUdp {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::caps::TcpSupport;

    /// Both list-shaped errors write their own separators — `" "` before
    /// the first name and `", "` before every one after it — and until
    /// this module nothing read a rendered message at all. `caps.rs`'s
    /// tests assert `contains(name)`, which is true of every separator a
    /// mutation can produce, so all four `i > 0` mutants survived the
    /// suite and the whole `UnsupportedUdp::fmt` body did too.
    ///
    /// Pinned as a whole string rather than by `contains`, because the
    /// defect these errors exist to prevent is a caller reading the
    /// message: `"…: nodelay, keepalive"` and `"…:, nodelay, keepalive"`
    /// name the same two options and only one of them is a sentence.
    #[test]
    fn the_tcp_message_separates_names_with_a_comma_and_the_first_with_a_space() {
        let one = UnsupportedTcp {
            missing: TcpSupport::NONE.nodelay(true),
        };
        assert_eq!(
            one.to_string(),
            "this runtime cannot apply these TCP socket options, and does not ignore them: \
             nodelay (a runtime that does apply one declares it in TcpConnect::TCP_SUPPORT)"
        );

        // Two names, which is the case that discriminates: with one name
        // the separator is whatever the `else` arm writes whatever the
        // condition does.
        let two = UnsupportedTcp {
            missing: TcpSupport::NONE.nodelay(true).reuse_address(true),
        };
        assert_eq!(
            two.to_string(),
            "this runtime cannot apply these TCP socket options, and does not ignore them: \
             nodelay, reuse_address \
             (a runtime that does apply one declares it in TcpConnect::TCP_SUPPORT)"
        );

        // And the empty case, which no caller can reach — `reject_unsupported`
        // returns `Ok` when nothing is missing — but which the `Display` is
        // free to render, so it is pinned rather than left to a reader to
        // work out. It is the only rendering with no separator at all.
        let none = UnsupportedTcp {
            missing: TcpSupport::NONE,
        };
        assert_eq!(
            none.to_string(),
            "this runtime cannot apply these TCP socket options, and does not ignore them: \
             (a runtime that does apply one declares it in TcpConnect::TCP_SUPPORT)"
        );
    }

    #[test]
    fn the_udp_message_names_every_offload_in_the_same_shape() {
        assert_eq!(
            UnsupportedUdp { gso: true }.to_string(),
            "this socket does not have these UDP offloads, and does not silently drop them: gso \
             (a socket that does have one declares it in UdpDatagrams::support)"
        );
    }

    /// `names()` is the half a caller reads as data rather than as prose.
    #[test]
    fn names_are_yielded_in_field_order_and_only_for_offending_entries() {
        let offending = UnsupportedUdp { gso: true };
        assert_eq!(offending.names().collect::<Vec<_>>(), ["gso"]);
        let neither = UnsupportedUdp { gso: false };
        assert_eq!(neither.names().count(), 0);
    }
}
