//! Runtime capabilities for hclient's native transport — [`TcpConnect`],
//! [`Timer`] and the optional siblings a runtime implements to back
//! `hclient-native`.
//!
//! Separate traits rather than one `Runtime`: the transport demands only
//! what it uses, and a backend without sockets isn't forced to implement
//! `connect` with a stub that panics.
//!
//! # Quick start
//!
//! `hclient-native` is generic over any `R: TcpConnect`, and calls
//! straight through the trait — this is the shape it, or any other
//! transport built on this seam, writes:
//!
//! ```
//! use hclient_rt::{TcpConnect, TcpOpts};
//! use std::net::SocketAddr;
//!
//! async fn dial<R: TcpConnect>(runtime: &R, addr: SocketAddr) -> std::io::Result<R::Stream> {
//!     runtime.connect(addr, &TcpOpts::default()).await
//! }
//! ```
//!
//! `hclient-rt-tokio` and `hclient-rt-smol` are the two shipped
//! implementations. [`TcpConnect`]'s own doc has a worked skeleton for
//! writing a third, including the `TCP_SUPPORT` constant every runtime
//! declares.
//!
//! # Key concepts
//!
//! - [`TcpConnect`] — open an outgoing TCP connection; [`TcpOpts`] is what
//!   a caller asks for, [`TcpSupport`] is what a runtime can actually apply.
//! - [`IpcConnect`] — same-machine sockets (Unix domain today), an
//!   extension of `TcpConnect` for a runtime that has file descriptors.
//! - [`Timer`] (re-exported from `hclient-core`) — a runtime's clock, for
//!   timeouts and backoff.
//! - [`Blocking`] and [`Spawn`] — optional: run blocking work off the
//!   reactor, and spawn a background task.
//! - [`UdpBind`] and [`Datagrams`] — outgoing UDP, for QUIC.
//!
//! # Three support reports, one shape
//!
//! Each seam above that can be asked for something a runtime may not have
//! answers the same way, so learning one is learning all three: a
//! `#[non_exhaustive]` report ([`TcpSupport`], [`UdpSupport`],
//! [`IpcSupport`]) built from a `NONE` baseline with `const` setters, a
//! request that carries a [`reject_unsupported`](TcpOpts::reject_unsupported)
//! check, and a named refusal ([`UnsupportedTcp`], [`UnsupportedUdp`],
//! [`UnsupportedIpc`]) a transport can call at configuration, before
//! anything is on the wire. See [`TcpOpts::reject_unsupported`] for the
//! full shape and why `NONE` rather than `ALL` is the only starting point.
//!
//! # Where to go next
//!
//! `hclient-rt-tokio` and `hclient-rt-smol` implement every trait here;
//! `hclient-native` is the transport that dials through this seam.

// Maintainer notes (not rendered):
//
// The original front-page prose, kept verbatim because it explains the
// three-report shape in full:
//
// Runtime capabilities for hclient's native transport.
//
// Separate traits rather than one `Runtime`: the transport demands only
// what it uses, and a backend without sockets isn't forced to implement
// `connect` with a stub that panics.
//
// # Three support reports, one shape
//
// Each seam that can be asked for something a runtime may not have
// answers the same way, so learning one is learning all three:
//
// | seam | the report | declared as | the request | the check | the refusal |
// |---|---|---|---|---|---|
// | TCP options | [`TcpSupport`] | [`TcpConnect::TCP_SUPPORT`] | [`TcpOpts`] | [`TcpOpts::reject_unsupported`] | [`UnsupportedTcp`] |
// | UDP offloads | [`UdpSupport`] | [`UdpDatagrams::support`] | [`Datagrams`] | [`Datagrams::reject_unsupported`] | [`UnsupportedUdp`] |
// | same-machine endpoints | [`IpcSupport`] | [`IpcConnect::IPC_SUPPORT`] | [`IpcAddr`] | [`IpcAddr::reject_unsupported`] | [`UnsupportedIpc`] |
//
// What they share, and why:
//
// - **`#[non_exhaustive]`, built from `NONE` with `const` setters.** A
//   runtime writes the report and the layer above only reads it, so the
//   next option, offload or endpoint kind must not break every runtime.
// - **`NONE` is the only starting point, and there is no `ALL`.** `NONE`
//   is the understating answer: a runtime that forgets a line refuses
//   something it could have done, where one that over-claims silently
//   drops what a caller asked for. A constant meaning *every field* would
//   claim the next field too, the day this crate adds one.
// - **The check lives on the request**, is called `reject_unsupported`,
//   and answers an [`std::io::Error`] of kind `Unsupported` carrying the
//   refusal — so a runtime calls it on the way in and a transport can call
//   it at configuration, before anything is on the wire.
// - **Each refusal has `names()`** and a message that lists every offender
//   and says where a runtime that does have one declares it.
//
// **The one difference is real, not an inconsistency.** TCP and IPC
// support are properties of the runtime and the target, so they are
// associated constants and a caller learns them at configuration. UDP
// offloads are properties of **one socket on one kernel** — GSO segment
// counts and whether ECN marks are delivered are measured at bind — so
// [`UdpDatagrams::support`] is a method on the socket.
//
// A constant meaning *every field* would claim the next field too, the
// day this crate adds one — and it was caught doing the damage it
// predicts before it went: `TokioHandle` declared it while delegating to
// a runtime that declares less.
#![forbid(unsafe_code)]
#![warn(missing_docs)]

mod error;
mod io;
mod ipc;
mod spawn;
mod tcp;
mod udp;

pub use error::{Cancelled, UnsupportedIpc, UnsupportedTcp, UnsupportedUdp};
pub use io::Shutdown;
pub use ipc::{IpcAddr, IpcConnect, IpcSupport};
pub use spawn::{Blocking, Spawn};
pub use tcp::{TcpAdoptStd, TcpConnect, TcpOpts, TcpSupport};
pub use udp::{Datagrams, EcnCodepoint, RecvMeta, UdpAdoptStd, UdpBind, UdpDatagrams, UdpSupport};

/// `Timer` is defined once, in `hclient-core`: the portable core needs it
/// for timeouts and backoff. This is just a re-export.
///
/// `Discard` comes with it: `Timer::Sleep` is a named associated type, and
/// a runtime whose native timer resolves to something other than `()` — as
/// `async_io::Timer` does — needs the adapter to satisfy it. Re-exported
/// here so a runtime crate does not have to depend on `hclient-core`
/// directly just to name one wrapper.
pub use hclient_core::timer::{Discard, Timer};
