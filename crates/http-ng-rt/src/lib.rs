//! Runtime capabilities for http-ng's native transport.
//!
//! Separate traits rather than one `Runtime`: the transport demands only
//! what it uses, and a backend without sockets isn't forced to implement
//! `connect` with a stub that panics.
#![forbid(unsafe_code)]

mod caps;
mod futures_io;

pub use caps::{Blocking, Cancelled, Spawn, TcpAdoptStd, TcpConnect, TcpOpts};
pub use futures_io::FuturesIo;

/// `Timer` is defined once, in `http-ng-core`: the portable core needs it
/// for timeouts and backoff. This is just a re-export.
pub use http_ng_core::unversioned::Timer;
