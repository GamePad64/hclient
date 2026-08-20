//! Apple's `URLSession` behind [`Transport`](hclient_core::unversioned::Transport).
//!
//! The fourth ambient backend — after `hclient-wasi` and `hclient-fetch`,
//! it owns no connection of its own — and the reason it exists is the list
//! of things a userspace stack cannot reach on an Apple platform: per-app
//! VPN, the system proxy and its PAC file, and background transfer. Every
//! one of those is a fact about the device rather than a preference,
//! which is the same argument `hclient-tls-native-tls` is built on one
//! seam over.
//!
//! **Enterprise roots pushed by MDM were on that list and should not have
//! been.** `rustls-platform-verifier` 0.7.0 already reaches them — its
//! Apple path builds the trust evaluation with
//! `SecTrust::create_with_certificates` and then, for any extra roots,
//! calls `set_trust_anchor_certificates_only(false)` specifically so the
//! system's own anchors survive. Read rather than assumed, and it is the
//! verifier `hclient`'s own `DefaultTransport` already uses.
//!
//! # What it deliberately does NOT take from the OS
//!
//! `URLSession` will keep cookies, a response cache and a redirect policy
//! for you, and this transport **turns all three off**. That is the
//! decision worth knowing about, and it is not caution:
//!
//! - None of the three is in the list above. They are portable behaviour,
//!   and this workspace already has portable implementations of all three
//!   — `hclient-cookie`, `hclient-cache` and `hclient_proto::redirect` —
//!   whose whole point is that a caller gets the same answers on every
//!   backend.
//! - Leaving them on would make this the *second* backend to report
//!   `owns_cookie_jar` and `owns_cache`, and a client-side jar or cache
//!   against it would be an `UnsupportedCapability` at `build()`. A caller
//!   porting from `hclient-native` would lose two features by changing
//!   one line.
//! - Redirects are the sharpest of the three, because `URLSession` lets a
//!   delegate refuse them and the browser does not. So this backend
//!   reports [`RedirectSupport::Transparent`](hclient_core::RedirectSupport::Transparent) where `hclient-fetch`
//!   reports `Internal`, and `Client`'s redirect policy — its hop limit,
//!   its `Authorization` stripping across origins — works here and cannot
//!   there.
//!
//! The configuration is `ephemeral`, which is Apple's own name for a
//! session that persists nothing, plus an explicit `nil` for the cookie
//! storage. Both are asserted rather than assumed: see `tests/live.rs`.
#![cfg(target_vendor = "apple")]

mod body;
mod delegate;
mod session;

pub use body::UrlSessionBody;
pub use session::{UrlSession, UrlSessionError};
