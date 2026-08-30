//! The only way a double can fail.
//!
//! **One type, and its being one is the crate's contract rather than a
//! gap.** A mock that could fail in several ways would be modelling a
//! backend's failures, and this one deliberately does not: a scripted
//! response is handed back verbatim, so every other outcome is the
//! caller's own. What is left is the single thing the double itself can
//! be wrong about — being asked for a response nobody scripted — and it
//! is named so a test can assert on it rather than on it being the only
//! error the mock has today.
//!
//! [`QueueEmpty`] is re-exported at the crate root, where it has always
//! been, so no consumer's `use` line moves.

/// Returned instead of a response when the mock's queue is empty.
///
/// `pub`, not a private type: a test must be able to tell this apart from
/// any other `ErrorKind::Other` error via
/// `Error::source().downcast_ref::<QueueEmpty>()`, rather than relying on
/// this being the mock's only path to an error today.
#[derive(Debug, thiserror::Error)]
#[error("MockTransport: response queue is empty")]
pub struct QueueEmpty;
