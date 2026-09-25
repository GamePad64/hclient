//! Internal: the sans-io pieces hclient's transports share. **Not a public
//! API** — depend on `hclient`, `hclient-native` or `hclient-proxy` instead.
//!
//! This crate is published only because the crates above depend on it and
//! crates.io needs every dependency to be there. Its API carries no
//! stability promise: it changes whenever the transports need it to, and
//! it moves its minor version each time. No other hclient crate
//! re-exports anything from it, and a check enforces that.
//!
//! Everything in here is **sans-io**: bytes and durations go in, decisions
//! come out, and no function opens a socket, reads a clock or draws
//! entropy.
//!
//! ```
//! use hclient_proto::head;
//!
//! let (head, len) = head::parse_response(b"HTTP/1.1 200 Connection established\r\n\r\n")
//!     .unwrap()
//!     .expect("a complete head");
//! assert_eq!(head.status, http::StatusCode::OK);
//! assert_eq!(len, 39);
//! ```
//!
//! # What is here
//!
//! - [`head`] — an RFC 9112 response head parsed from bytes, for a
//!   `CONNECT` tunnel or anything else that reads HTTP/1 by hand.
//! - [`happy_eyeballs`] — the RFC 8305 connection-racing
//!   [`Scheduler`](happy_eyeballs::Scheduler), with elapsed time as a
//!   parameter.
//! - [`encode`] — base64 and `application/x-www-form-urlencoded`,
//!   encode only.

// Maintainer notes (not rendered):
//
// Crate invariant: no `async fn`, no runtime dependency, anywhere. Anything
// that depends on time takes `now` as a parameter. Enforced in CI.
//
// This crate used to hold the redirect and retry policies, `Backoff`, the
// SSE and line decoders, `Link`, the header-field grammar and the URI
// parser as well, and `hclient` re-exported the policy types from here.
// The owner's rule is that this crate is internal and nothing is
// re-exported from it, so those modules moved into `hclient`, the only
// crate that used them; they live under `hclient::sansio`, still pure and
// still tested with no socket. What stayed is what more than one transport
// needs: `head` (`hclient-native`, `hclient-proxy`, `hclient-winhttp`),
// `happy_eyeballs` (`hclient-native`) and `encode` (`hclient`,
// `hclient-proxy`).
#![forbid(unsafe_code)]
#![warn(missing_docs)]

mod error;

pub mod encode;
pub mod happy_eyeballs;
pub mod head;
