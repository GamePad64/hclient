//! What stops an authentication exchange, from the two sides that can.
//!
//! [`TooManyLegs`] is the **client's** verdict and [`DigestError`] is a
//! **scheme's**, and the split is the seam's: `Client::run` enforces three
//! rules a flow cannot override — a body that cannot be replayed, an
//! origin that credentials may not cross, and `MAX_LEGS` — where a scheme
//! answers only for its own challenge. A third party writing NTLM or
//! Negotiate in their own crate, which is what this seam exists for, will
//! meet the first and never the second.
//!
//! **Why these are not in `hclient`'s own `error.rs`.** This module is a
//! seam rather than a part of the client: its whole argument is that the
//! Kerberos glue nobody has written belongs in somebody else's crate, and
//! an implementor of [`AuthFlow`](crate::auth::AuthFlow) reads `auth`
//! rather than the client's failure list. `cookie` and `cache` are here
//! for a different reason of the same shape — they were crates.
//!
//! [`TooManyLegs`] is re-exported from [`crate::auth`] and [`DigestError`]
//! from [`crate::auth::digest`], where they have always been, so no
//! consumer's `use` line moves. [`DigestError`] keeps the `digest-auth`
//! gate on the item, so a build without the feature has no way to reach
//! a type whose module does not exist.

use super::MAX_LEGS;

/// A flow ran out of legs.
#[derive(Debug, thiserror::Error)]
#[error("authentication did not finish in {MAX_LEGS} attempts")]
pub struct TooManyLegs;

#[cfg(feature = "digest-auth")]
/// The challenge could not be answered.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
#[non_exhaustive]
pub enum DigestError {
    /// No `WWW-Authenticate` value named `Digest` with an algorithm this
    /// build implements.
    #[error("the server offered no Digest challenge this client can answer")]
    NoUsableChallenge,
    /// A `Digest` challenge without `realm` or without `nonce`, which
    /// RFC 7616 §3.3 makes required.
    #[error("the Digest challenge is missing its `{parameter}`")]
    MissingParameter { parameter: &'static str },
    /// The server asked for `qop="auth-int"` and nothing else. See this
    /// module's doc for why that is refused rather than approximated.
    #[error("the server requires qop=auth-int, which needs the request body hashed")]
    AuthIntUnsupported,
}
