//! Same-machine connections: where one goes, which kinds a runtime can
//! dial, and the refusal for the kinds it cannot.
//!
//! # One method and one enum, so the next kind is not a major version
//!
//! This was `connect_unix(&Path)` with its own associated future type and
//! a `SUPPORTS_UNIX` flag. Docker, containerd and every gRPC daemon that
//! listens on a Unix-domain socket on Linux listen on a **named pipe** on
//! Windows, so a second kind was always coming — and an associated type
//! cannot have a default on stable Rust, so a `connect_named_pipe` with
//! its own future would have broken every [`TcpConnect`](crate::TcpConnect)
//! implementor at once.
//!
//! So the kind is a value: [`IpcAddr`] is `#[non_exhaustive]`, a runtime
//! matches the kinds it dials and refuses the rest with a wildcard arm,
//! and [`IpcSupport`] says which kinds it dials before anybody tries. A
//! new kind is then a new variant and a new flag — additive for every
//! runtime, each of which refuses it until it learns it.

use crate::error::UnsupportedIpc;
use std::future::Future;
use std::marker::PhantomData;
use std::path::PathBuf;
use std::pin::Pin;
use std::task::{Context, Poll};

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

    /// `Ok` when `can` dials this kind, and the refusal a runtime would
    /// hand back when it does not — so a caller configuring a transport
    /// meets it at configuration rather than on the wire.
    /// [`TcpOpts::reject_unsupported`](crate::TcpOpts::reject_unsupported)'s
    /// shape, one seam over.
    ///
    /// # Errors
    ///
    /// An [`std::io::ErrorKind::Unsupported`] carrying [`UnsupportedIpc`]
    /// naming this kind, when `can` does not dial it.
    pub fn reject_unsupported(&self, can: IpcSupport) -> std::io::Result<()> {
        if can.allows(self) {
            return Ok(());
        }
        Err(refusal(self.kind()))
    }
}

fn refusal(kind: &'static str) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::Unsupported, UnsupportedIpc { kind })
}

/// Which [`IpcAddr`] kinds a runtime dials.
///
/// [`TcpOptsSupport`](crate::TcpOptsSupport)'s shape: `#[non_exhaustive]`,
/// built from [`NONE`](Self::NONE) with `const` setters, and defaulted to
/// `NONE` on [`TcpConnect::IPC`](crate::TcpConnect::IPC) — a claim made by
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

    /// Whether `addr`'s kind is one this runtime dials.
    #[must_use]
    pub const fn allows(&self, addr: &IpcAddr) -> bool {
        match addr {
            IpcAddr::Unix(_) => self.unix,
        }
    }
}

/// The refusal a runtime hands back from
/// [`TcpConnect::connect_ipc`](crate::TcpConnect::connect_ipc) for a kind it
/// does not dial, as a type it can name.
///
/// Ready on the first poll with [`ErrorKind::Unsupported`] carrying
/// [`UnsupportedIpc`], and `Send` whatever `S` is, because it never holds
/// one.
///
/// [`ErrorKind::Unsupported`]: std::io::ErrorKind::Unsupported
#[derive(Debug)]
pub struct RefuseIpc<S> {
    kind: &'static str,
    stream: PhantomData<fn() -> S>,
}

impl<S> RefuseIpc<S> {
    /// A refusal naming `addr`'s kind.
    #[must_use]
    pub const fn new(addr: &IpcAddr) -> Self {
        Self {
            kind: addr.kind(),
            stream: PhantomData,
        }
    }
}

impl<S> Future for RefuseIpc<S> {
    type Output = std::io::Result<S>;

    fn poll(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<Self::Output> {
        Poll::Ready(Err(refusal(self.kind)))
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
                .map(UnsupportedIpc::kind),
            Some("unix")
        );
    }

    #[test]
    fn the_refusal_is_unsupported_and_names_the_kind() {
        let addr = IpcAddr::unix("/run/x.sock");
        let mut f = std::pin::pin!(RefuseIpc::<()>::new(&addr));
        let Poll::Ready(Err(e)) = f
            .as_mut()
            .poll(&mut Context::from_waker(std::task::Waker::noop()))
        else {
            panic!("a refusal is ready at once and is an error");
        };
        assert_eq!(e.kind(), std::io::ErrorKind::Unsupported);
        let payload = e
            .get_ref()
            .and_then(|p| p.downcast_ref::<UnsupportedIpc>())
            .expect("the payload is typed, not only a message");
        assert_eq!(payload.kind(), "unix");
        assert!(e.to_string().contains("unix"), "{e}");
    }
}
