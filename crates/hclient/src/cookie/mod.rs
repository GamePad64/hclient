//! A cookie jar: RFC 6265bis parsing, storage, domain and path matching,
//! and expiry — with no I/O and no clock of its own.
//!
//! # Why a module and not a crate
//!
//! It was `hclient-cookie` for four verticals, under the argument that a
//! jar is a pure function of headers, a URI and a `now`, so putting it in
//! a leaf crate made *"cookies behave the same on every backend"* a
//! structural fact rather than a consequence of everyone going through
//! `Client`.
//!
//! That argument was about **where the code cannot reach**, and it turned
//! out to have exactly one consumer: `cargo tree -i hclient-cookie` named
//! `hclient` and nothing else. A crate boundary earns its keep in this
//! workspace when it holds a dependency a feature would otherwise spread
//! (`hclient-tls-quic` carries `quinn-proto`; `hclient-tungstenite`
//! carries `tungstenite`), and this one carried `public-suffix` — which
//! the `cookies` feature already gates just as well from here.
//!
//! **What is unchanged is the discipline, and it is worth naming because
//! a module makes it easy to lose.** Nothing here may reach for
//! [`crate::Client`], for `hclient_core`, or for any transport: the jar
//! stays a pure function of its inputs, and `Client` is what supplies the
//! `now`. The public path is unchanged too — `hclient::cookie::CookieJar`
//! is what it always was, so no consumer's `use` line moves.
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
//!   see the `date.rs` module documentation, and `tests/cookie_dates.rs`
//!   for the corpus that decides it.
//!
//! # What this deliberately does not do
//!
//! - **It does not enforce `SameSite`.** The attribute is parsed and
//!   reported; acting on it needs the initiating browsing context, which a
//!   non-browser client does not have. See [`SameSite`].
//! - **It does not attach or read anything by itself.** Wiring it into
//!   `Client` — including the `Capabilities::owns_cookie_jar` check that
//!   must switch it off against a transport owning its own jar, such as
//!   `hclient-fetch` — is [`Client`](crate::Client)'s, not this module's.
//! - **It does not choose a serialisation.** [`CookieJar::records`] hands
//!   out a [`CookieRecord`] per persistent cookie and
//!   [`CookieJar::restore`] takes one back with its creation and
//!   last-access times intact; the *format* — and the file, the database
//!   row, the `localStorage` key — is the caller's, and `persist.rs`
//!   carries the measurement behind that.
//! - **It has no background sweep.** Expired cookies are filtered on
//!   retrieval and dropped on the next [`store`](CookieJar::store); a jar
//!   nobody touches keeps its expired entries until it is touched. Same
//!   reason the native pool has no reaper — there is nothing here to run
//!   one.

mod date;
mod jar;
mod matching;
mod parse;
mod persist;
mod suffix;

pub use jar::{Cookie, CookieJar, Limits, Rejected};
pub use parse::{ParseError, SameSite, SetCookie};
pub use persist::CookieRecord;
pub use suffix::{BuiltinList, NoList, PublicSuffixList};
