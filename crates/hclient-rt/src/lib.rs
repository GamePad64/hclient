//! Runtime capabilities for hclient's native transport.
//!
//! Separate traits rather than one `Runtime`: the transport demands only
//! what it uses, and a backend without sockets isn't forced to implement
//! `connect` with a stub that panics.
#![forbid(unsafe_code)]

mod caps;
mod futures_io;
mod udp;

pub use caps::{
    Blocking, Cancelled, Spawn, TcpAdoptStd, TcpConnect, TcpOpts, TcpOptsSupport,
    UnixSocketsUnsupported, UnsupportedTcpOpts,
};
pub use futures_io::FuturesIo;
pub use udp::{
    Datagrams, EcnCodepoint, RecvMeta, UdpAdoptStd, UdpBind, UdpCaps, UdpDatagrams,
    UnsupportedUdpOffload,
};

/// `Timer` is defined once, in `hclient-core`: the portable core needs it
/// for timeouts and backoff. This is just a re-export.
///
/// `Discard` comes with it: `Timer::Sleep` is a named associated type, and
/// a runtime whose native timer resolves to something other than `()` — as
/// `async_io::Timer` does — needs the adapter to satisfy it. Re-exported
/// here so a runtime crate does not have to depend on `hclient-core`
/// directly just to name one wrapper.
pub use hclient_core::unversioned::{Discard, Timer};
