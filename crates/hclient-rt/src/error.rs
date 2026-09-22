//! Every way a runtime can refuse, and all four are the same refusal.
//!
//! This crate is seams and no implementation, so nothing here fails the
//! way work fails — there is no socket to time out and no query to lose.
//! What is left is a runtime saying **no** to something a caller asked
//! for, and that is what unites these four: three are a capability the
//! platform does not have ([`UnsupportedTcpOpts`],
//! [`UnsupportedUdpOffload`], [`UnsupportedIpc`]) and the fourth
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

/// A runtime was asked to dial a kind of same-machine endpoint it does
/// not dial.
///
/// Carried inside an [`std::io::Error`] with
/// [`ErrorKind::Unsupported`](std::io::ErrorKind::Unsupported) by
/// [`RefuseIpc`](crate::RefuseIpc), the shape [`UnsupportedTcpOpts`] and
/// [`UnsupportedUdpOffload`] already use. Reachable only past
/// [`TcpConnect::IPC`](crate::TcpConnect::IPC), which
/// `hclient_native::Native::unix_socket` checks at the call that
/// configures it — so a caller normally meets the refusal where they
/// wrote the path, not on the wire.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("this runtime does not connect to {kind} endpoints")]
pub struct UnsupportedIpc {
    pub(crate) kind: &'static str,
}

impl UnsupportedIpc {
    /// The refused kind — [`IpcAddr::kind`](crate::IpcAddr::kind)'s name.
    #[must_use]
    pub const fn kind(&self) -> &'static str {
        self.kind
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::caps::TcpOptsSupport;

    /// Both list-shaped errors write their own separators — `" "` before
    /// the first name and `", "` before every one after it — and until
    /// this module nothing read a rendered message at all. `caps.rs`'s
    /// tests assert `contains(name)`, which is true of every separator a
    /// mutation can produce, so all four `i > 0` mutants survived the
    /// suite and the whole `UnsupportedUdpOffload::fmt` body did too.
    ///
    /// Pinned as a whole string rather than by `contains`, because the
    /// defect these errors exist to prevent is a caller reading the
    /// message: `"…: gso, ecn"` and `"…:, gso, ecn"` name the same two
    /// offloads and only one of them is a sentence.
    #[test]
    fn the_tcp_message_separates_names_with_a_comma_and_the_first_with_a_space() {
        let one = UnsupportedTcpOpts {
            missing: TcpOptsSupport::NONE.nodelay(true),
        };
        assert_eq!(
            one.to_string(),
            "this runtime cannot apply these TCP socket options, and does not ignore them: \
             nodelay (a runtime that does apply one declares it in TcpConnect::APPLIES)"
        );

        // Two names, which is the case that discriminates: with one name
        // the separator is whatever the `else` arm writes whatever the
        // condition does.
        let two = UnsupportedTcpOpts {
            missing: TcpOptsSupport::NONE.nodelay(true).reuse_address(true),
        };
        assert_eq!(
            two.to_string(),
            "this runtime cannot apply these TCP socket options, and does not ignore them: \
             nodelay, reuse_address \
             (a runtime that does apply one declares it in TcpConnect::APPLIES)"
        );

        // And the empty case, which no caller can reach — `reject_unsupported`
        // returns `Ok` when nothing is missing — but which the `Display` is
        // free to render, so it is pinned rather than left to a reader to
        // work out. It is the only rendering with no separator at all.
        let none = UnsupportedTcpOpts {
            missing: TcpOptsSupport::NONE,
        };
        assert_eq!(
            none.to_string(),
            "this runtime cannot apply these TCP socket options, and does not ignore them: \
             (a runtime that does apply one declares it in TcpConnect::APPLIES)"
        );
    }

    #[test]
    fn the_udp_message_names_every_offload_in_the_same_shape() {
        assert_eq!(
            UnsupportedUdpOffload {
                gso: true,
                ecn: false
            }
            .to_string(),
            "this socket does not have these UDP offloads, and does not silently drop them: gso"
        );
        // `ecn: true` is unreachable from `Datagrams::reject_unsupported`
        // today — see that method's own comment, where the `ecn` local is a
        // deliberate constant `false`. The field is `pub(crate)`, so this
        // module can still build the value, and it is worth building: the
        // two-name rendering is the only one where the separator between
        // names is observable, and `names()`' `ecn` arm has no other reader.
        assert_eq!(
            UnsupportedUdpOffload {
                gso: true,
                ecn: true
            }
            .to_string(),
            "this socket does not have these UDP offloads, and does not silently drop them: \
             gso, ecn"
        );
        assert_eq!(
            UnsupportedUdpOffload {
                gso: false,
                ecn: true
            }
            .to_string(),
            "this socket does not have these UDP offloads, and does not silently drop them: ecn"
        );
    }

    /// `names()` is the half a caller reads as data rather than as prose,
    /// and the `ecn` arm of the UDP one is reachable from nowhere else in
    /// the workspace.
    #[test]
    fn names_are_yielded_in_field_order_and_only_for_offending_entries() {
        let both = UnsupportedUdpOffload {
            gso: true,
            ecn: true,
        };
        assert_eq!(both.names().collect::<Vec<_>>(), ["gso", "ecn"]);
        let neither = UnsupportedUdpOffload {
            gso: false,
            ecn: false,
        };
        assert_eq!(neither.names().count(), 0);
    }
}
