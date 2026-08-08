//! A cookie jar: RFC 6265bis parsing, storage, domain and path matching,
//! and expiry — with no I/O and no clock of its own.
//!
//! # Why a crate and not a module in `http-ng`
//!
//! The same reason `http-ng-proto` is a crate. A jar is *used* by
//! `http-ng`, but the substance of it — §5.2's parse, §5.4's retrieval,
//! §5.7's storage model — is a pure function of headers, a URI and a
//! `now`. Putting it here makes "cookies behave the same on every backend"
//! a structural fact rather than a consequence of everyone happening to go
//! through `Client`.
//!
//! This crate is a leaf: it depends on `http` and `thiserror`, and on
//! `public-suffix` behind a default feature. Not on `http-ng-core`, not on
//! `http-ng`, not on any transport.
//!
//! # Where the interesting behaviour is
//!
//! Every rule worth having is a **refusal**, and each has a home:
//!
//! - a `Domain` for a sibling host, and the `/foo` that must not match
//!   `/foobar` — [`matching`](self), from RFC 6265bis §5.1.3 and §5.1.4;
//! - a `Domain` that is a public suffix — [`PublicSuffixList`], whose
//!   module documentation carries the measurement that chose the list and
//!   names the gap it leaves;
//! - a `Secure` cookie offered over an insecure request, and the
//!   `__Secure-`/`__Host-` name prefixes — [`CookieJar::store`];
//! - an `Expires` that does not parse the way an HTTP-date parser would —
//!   see the `date.rs` module documentation, and `tests/dates.rs` for the
//!   corpus that decides it.
//!
//! # What this crate deliberately does not do
//!
//! - **It does not enforce `SameSite`.** The attribute is parsed and
//!   reported; acting on it needs the initiating browsing context, which a
//!   non-browser client does not have. See [`SameSite`].
//! - **It does not attach or read anything by itself.** Wiring it into
//!   `Client` — including the `Capabilities::owns_cookie_jar` check that
//!   must switch it off against a transport owning its own jar, such as
//!   `http-ng-fetch` — is a separate step and is not in this crate.
//! - **It does not persist.** [`CookieJar::iter`] hands out everything
//!   held; where that goes is the caller's decision.
//! - **It has no background sweep.** Expired cookies are filtered on
//!   retrieval and dropped on the next [`store`](CookieJar::store); a jar
//!   nobody touches keeps its expired entries until it is touched. Same
//!   reason the native pool has no reaper — there is nothing here to run
//!   one.

#![forbid(unsafe_code)]

mod date;
mod jar;
mod matching;
mod parse;
mod suffix;

pub use jar::{Cookie, CookieJar, Limits, Rejected};
pub use parse::{ParseError, SameSite, SetCookie};
pub use suffix::{BuiltinList, NoList, PublicSuffixList};
