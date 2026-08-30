//! What the OS said, rather than what this crate thinks it meant.
//!
//! **One type, and its being one is the whole policy of this backend.**
//! Everything here happens inside `URLSession`, so every failure is
//! Apple's — a per-app VPN refusing a route, a PAC script sending the
//! request somewhere that does not answer, a trust evaluation the system
//! made. This crate invents no taxonomy over that, for the reason its
//! module doc gives about redirects and cookies: a translation at this
//! boundary is a second vocabulary, and the caller who reached for this
//! backend did so because they wanted the platform's own answer.
//!
//! [`UrlSessionError`] is re-exported at the crate root, where it has
//! always been, so no consumer's `use` line moves.

/// What `URLSession` said went wrong, in Apple's own words.
///
/// `NSError`'s `localizedDescription` and nothing more: its `domain` and
/// `code` are stable enough to match on, but mapping them onto this
/// workspace's `ErrorKind` would be a second vocabulary invented at the
/// boundary — the same reason `hclient-fetch` reports what the browser
/// said rather than a translation of it.
#[derive(Debug, thiserror::Error)]
#[error("URLSession: {0}")]
pub struct UrlSessionError(pub String);
