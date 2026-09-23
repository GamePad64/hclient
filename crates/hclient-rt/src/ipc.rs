//! Same-machine connections: where one goes, and which kinds a runtime
//! can dial.
//!
//! # A seam of its own, extending the TCP one
//!
//! [`IpcConnect`] requires [`TcpConnect`], and for one reason: the stream a
//! same-machine connect hands back must be the **same type** a TCP connect
//! does, because a transport carries one IO type and puts TLS and HTTP on
//! it either way. What the split buys is that a runtime with no file
//! descriptors — `hclient-rt-embassy`, a NAL stack, every test double —
//! implements nothing here rather than a refusal it has to name.
//!
//! It lived on [`TcpConnect`] for a vertical, on the argument that a seam
//! of its own could not be reached: putting `R: IpcConnect` on
//! `hclient_native::Native` would tax every runtime, and a stored function
//! pointer returning a boxed future drops its auto traits. The second half
//! is answered the way `Native::http3` answers it — the box **declares**
//! `Send` and the bound sits on the opt-in `Native::unix_socket`, so it is
//! proven where the runtime is concrete — and the first half then has no
//! subject: nothing but that constructor names this trait.
//!
//! # One method and one enum, so the next kind is not a major version
//!
//! This was `connect_unix(&Path)` with its own associated future type and
//! a `SUPPORTS_UNIX` flag. Docker, containerd and every gRPC daemon that
//! listens on a Unix-domain socket on Linux listen on a **named pipe** on
//! Windows, so a second kind was always coming — and an associated type
//! cannot have a default on stable Rust, so a `connect_named_pipe` with
//! its own future would have broken every implementor at once.
//!
//! So the kind is a value: [`IpcAddr`] is `#[non_exhaustive]`,
//! [`IpcSupport`] says which kinds a runtime dials, and a runtime refuses
//! the rest with [`IpcAddr::reject_unsupported`] on entry — exactly as a
//! TCP connect refuses an option with
//! [`TcpOpts::reject_unsupported`](crate::TcpOpts::reject_unsupported) and
//! a UDP send an offload with
//! [`Datagrams::reject_unsupported`](crate::Datagrams::reject_unsupported).
//! A new kind is then a new variant and a new flag — additive for every
//! runtime, whose report says `false` for it until it learns it.
//!
//! **There was a refusal future here, `RefuseIpc`, and it made IPC the one
//! seam refusing in a different way.** A runtime handed it to the kinds its
//! `match` did not name, so the refusal came from *which arm ran* rather
//! than from the report — and a runtime whose `IPC_SUPPORT` said `true`
//! for a kind its `match` forgot would refuse through the wildcard with no
//! sign the two disagreed. Checking the report first makes the wildcard
//! arm unreachable by construction, which is the same claim TCP and UDP
//! make, and costs a runtime nothing it did not already have.

use crate::TcpConnect;
use crate::error::UnsupportedIpc;
use std::future::Future;
use std::path::PathBuf;

/// A runtime that can connect to a same-machine endpoint — a Unix-domain
/// socket today, and the kinds [`IpcAddr`] gains later. See the module
/// documentation for why this extends [`TcpConnect`] rather than living on
/// it.
pub trait IpcConnect: TcpConnect {
    /// Which endpoint kinds [`connect_ipc`](Self::connect_ipc) dials — see
    /// [`IpcSupport`].
    ///
    /// [`TcpConnect::TCP_SUPPORT`]'s shape, and defaulted the same way and
    /// for the same reason: a runtime that says nothing here refuses the
    /// setting, where one that over-claimed would fail every connect at
    /// the socket instead of at the call that asked — which is what lets
    /// `hclient_native::Native::unix_socket` refuse at configuration. A
    /// runtime implements this trait for the kinds some of its targets
    /// have, and says per target here which ones this one does.
    const IPC_SUPPORT: IpcSupport = IpcSupport::NONE;

    /// The future [`connect_ipc`](Self::connect_ipc) hands back.
    ///
    /// [`TcpConnect::Connecting`]'s shape: an associated type rather than
    /// an RPITIT, so a consumer that must prove its own future `Send` can
    /// name this one.
    type ConnectingIpc<'a>: Future<Output = std::io::Result<Self::Stream>>
    where
        Self: 'a;

    /// Connect to `addr`.
    ///
    /// **One method for every kind**, and the refusal comes first: a
    /// runtime calls [`IpcAddr::reject_unsupported`] with its own
    /// [`IPC_SUPPORT`](Self::IPC_SUPPORT) on entry, then matches the kinds
    /// it dials. The `match` still needs a wildcard arm, since [`IpcAddr`]
    /// is `#[non_exhaustive]`, and after the check that arm is
    /// unreachable — a runtime reaching it has a report claiming a kind
    /// its `match` does not dial, which is the runtime's bug.
    ///
    /// **No [`TcpOpts`](crate::TcpOpts)**: every field there is a TCP or
    /// IP socket option, and no same-machine endpoint has them. A parameter
    /// that could only ever be ignored is worse than no parameter.
    fn connect_ipc<'a>(&'a self, addr: &IpcAddr) -> Self::ConnectingIpc<'a>;
}

/// Where a same-machine connection goes.
///
/// `#[non_exhaustive]`: a runtime's `match` over this needs a wildcard arm,
/// and that arm is where a kind added later is refused — which is the
/// whole of how adding one stays non-breaking.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum IpcAddr {
    /// A Unix-domain stream socket at this path.
    ///
    /// No [`TcpOpts`](crate::TcpOpts) apply: every field there is a TCP
    /// or IP socket option, and `AF_UNIX` has none of them — no Nagle, no
    /// keepalive, no source address, no interface.
    Unix(PathBuf),
}

impl IpcAddr {
    /// A Unix-domain socket at `path`.
    pub fn unix(path: impl Into<PathBuf>) -> Self {
        Self::Unix(path.into())
    }

    /// The kind's name, as a refusal names it.
    #[must_use]
    pub const fn kind(&self) -> &'static str {
        match self {
            Self::Unix(_) => "unix",
        }
    }

    /// `Ok` when `support` dials this kind, and the refusal otherwise.
    ///
    /// Asked twice, by two parties: a runtime asks it of its own
    /// [`IpcConnect::IPC_SUPPORT`] on entry to
    /// [`connect_ipc`](IpcConnect::connect_ipc), and a transport asks it at
    /// configuration, so a caller meets the refusal where they wrote the
    /// path rather than on the first request.
    /// [`TcpOpts::reject_unsupported`](crate::TcpOpts::reject_unsupported)'s
    /// shape, one seam over.
    ///
    /// # Errors
    ///
    /// An [`std::io::ErrorKind::Unsupported`] carrying [`UnsupportedIpc`]
    /// naming this kind, when `support` does not dial it.
    pub fn reject_unsupported(&self, support: IpcSupport) -> std::io::Result<()> {
        if support.allows(self) {
            return Ok(());
        }
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            UnsupportedIpc { kind: self.kind() },
        ))
    }
}

/// Which [`IpcAddr`] kinds a runtime dials.
///
/// [`TcpSupport`](crate::TcpSupport)'s shape: `#[non_exhaustive]`,
/// built from [`NONE`](Self::NONE) with `const` setters, and defaulted to
/// `NONE` on [`IpcConnect::IPC_SUPPORT`](crate::IpcConnect::IPC_SUPPORT) — a claim made by
/// silence must never be stronger than the truth. It is a constant rather
/// than something a connect discovers because the answer is a property of
/// the runtime and the target, and a caller should learn it at
/// configuration rather than on the wire.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct IpcSupport {
    /// Unix-domain stream sockets.
    pub unix: bool,
}

impl IpcSupport {
    /// Dials nothing: the default, and every runtime without file
    /// descriptors.
    pub const NONE: Self = Self { unix: false };

    /// Whether Unix-domain sockets are dialled.
    #[must_use]
    pub const fn unix(mut self, dials: bool) -> Self {
        self.unix = dials;
        self
    }

    /// Whether `addr`'s kind is one this runtime dials. Crate-private, as
    /// its TCP and UDP counterparts would be: the question is asked through
    /// [`IpcAddr::reject_unsupported`], which is the one answer a caller
    /// acts on.
    #[must_use]
    pub(crate) const fn allows(self, addr: &IpcAddr) -> bool {
        match addr {
            IpcAddr::Unix(_) => self.unix,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nothing_is_dialled_by_default_and_the_setter_reaches_its_kind() {
        let unix = IpcAddr::unix("/run/x.sock");
        assert!(!IpcSupport::NONE.allows(&unix));
        assert!(IpcSupport::NONE.unix(true).allows(&unix));
        assert!(!IpcSupport::NONE.unix(true).unix(false).allows(&unix));
    }

    #[test]
    fn configuration_refuses_exactly_what_the_runtime_does_not_dial() {
        let unix = IpcAddr::unix("/run/x.sock");
        assert!(unix.reject_unsupported(IpcSupport::NONE.unix(true)).is_ok());
        let err = unix
            .reject_unsupported(IpcSupport::NONE)
            .expect_err("a runtime that dials nothing refuses a socket");
        assert_eq!(err.kind(), std::io::ErrorKind::Unsupported);
        assert_eq!(
            err.get_ref()
                .and_then(|p| p.downcast_ref::<UnsupportedIpc>())
                .map(|p| p.names().collect::<Vec<_>>()),
            Some(vec!["unix"])
        );
    }

    #[test]
    fn the_refusal_reads_as_a_sentence() {
        let e = IpcAddr::unix("/run/x.sock")
            .reject_unsupported(IpcSupport::NONE)
            .expect_err("nothing is dialled");
        assert_eq!(
            e.to_string(),
            "this runtime cannot dial these same-machine endpoints, and does not fall back: unix \
             (a runtime that does dial one declares it in IpcConnect::IPC_SUPPORT)"
        );
    }
}
