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
///
/// `Discard` comes with it: `Timer::Sleep` is a named associated type, and
/// a runtime whose native timer resolves to something other than `()` — as
/// `async_io::Timer` does — needs the adapter to satisfy it. Re-exported
/// here so a runtime crate does not have to depend on `http-ng-core`
/// directly just to name one wrapper.
pub use http_ng_core::unversioned::{Discard, Timer};
