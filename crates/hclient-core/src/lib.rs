//! Plugin contract for hclient.
//!
//! Crate invariant: the seam traits (`Transport`, `Timer`, middleware) do not
//! declare `Send`/`Sync` bounds — Send-ness is inferred by auto-traits
//! through `impl Future`. The one documented exception is [`Error`]: its
//! `source` must be `Send + Sync`, or a client could not build one from a
//! backend's error at all.
//!
//! **This paragraph used to end by naming where that bound lived** — *"in
//! `Client::execute`'s own where-clause, not on the `Transport` trait
//! itself"* — and it lives nowhere now. `hclient::Client` stopped naming
//! its transport, so `Transport::to_error` is called from
//! [`unversioned::erased::BoxedTransport`]'s blanket impl, where `Self` is
//! concrete and the bound is discharged rather than declared; four
//! where-clauses left `client.rs` with it. The invariant is unchanged and
//! is one obligation lighter, which is the direction it is supposed to
//! move in.
//!
//! [`unversioned::erased`] names `Send + Sync` on two type aliases a facade
//! writes at its own use site, and it is **not a seam**: no backend
//! implements it and none is taxed by it — a blanket impl covers every
//! `Transport`, and a backend that cannot meet the bound is refused at a
//! constructor rather than at a trait.
//!
//! It is not the only place in this crate that names those traits, and
//! this line said it was until the third rendered-docs pass caught it:
//! `RequestBody`'s rewind factory and streaming arm carry one (amendment
//! C2), and `Error::source` carries the one named two paragraphs up. The
//! rule is what distinguishes them from a seam bound, not the count —
//! every site carries a `send-bound-exception` marker naming its
//! amendment, so `grep` answers *which* and *how many* and a sentence
//! cannot go stale in its place.
#![forbid(unsafe_code)]

mod body;
mod caps;
mod error;
mod host;
pub mod unversioned;

pub use body::{RequestBody, RetryKind, RewindFactory};
pub use caps::{
    AllowEarlyData, CancelSupport, Capabilities, DecompressionSupport, EarlyDataSupport,
    RedirectSupport, RequireVersion, ReuseSupport, TimeoutSupport, Timeouts, TlsSupport,
    UnsupportedCapability, VersionNotAvailable, check_version,
};
pub use error::{Error, ErrorKind, Phase};
pub use host::bare_host;
