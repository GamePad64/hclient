//! Plugin contract for http-ng.
//!
//! Crate invariant: the seam traits (`Transport`, `Timer`, middleware) do not
//! declare `Send`/`Sync` bounds — Send-ness is inferred by auto-traits
//! through `impl Future`. The one documented exception is [`Error`]: its
//! `source` must be `Send + Sync`, or `Client::execute` could not return a
//! Send-compatible future for any backend. The bound
//! `T::Error: Send + Sync + 'static` lives in `Client::execute`'s own
//! where-clause, not on the `Transport` trait itself.
#![forbid(unsafe_code)]

mod body;
mod caps;
mod error;
mod host;
pub mod unversioned;

pub use body::{RequestBody, RetryKind, RewindFactory};
pub use caps::{
    AllowEarlyData, CancelSupport, Capabilities, DecompressionSupport, EarlyDataSupport,
    RedirectSupport, ReuseSupport, TimeoutSupport, Timeouts, TlsSupport, UnsupportedCapability,
};
pub use error::{Error, ErrorKind, Phase};
pub use host::bare_host;
