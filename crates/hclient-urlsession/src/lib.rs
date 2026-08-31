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
//!
//! # What it does take, and says so
//!
//! The proxy configuration, which is one of the three reasons above — and
//! [`Capabilities::proxy`](hclient_core::Capabilities::proxy) reports it,
//! read from the machine at construction rather than left at `false`.
//! Left at `false` it was a **capability that lies**: a caller asking
//! *will my requests go through a proxy* got `no` from a transport that
//! hands every request to a stack which proxies them.
//!
//! What is read is the **platform's own settings** —
//! `hclient_proxy::system::SystemProxies::detect_platform`, which skips
//! the `HTTP_PROXY`/`HTTPS_PROXY` variables that `detect` reads first.
//! `URLSession` takes its proxies from the system configuration, so a
//! value off `detect` would report `true` on a machine whose only proxy
//! is a variable this transport ignores. The platform read can only
//! under-claim, which is the direction to be wrong in.
//!
//! `true` says the machine names a proxy this transport hands to the OS.
//! It does not say that any one request goes through it — the exceptions
//! list decides that, and on a machine configured with a PAC script a
//! JavaScript program does, which `URLSession` runs and nothing in this
//! workspace can read. That is the same reading `hclient-native` gives
//! the field, where a proxy carrying a bypass list still reports `true`.
#![cfg(target_vendor = "apple")]

//! # WebSocket
//!
//! [`UrlSessionWebSocket`] implements
//! [`WebSocketConnect`](hclient_core::unversioned::WebSocketConnect) over
//! `NSURLSessionWebSocketTask`, **in this crate** rather than one of its
//! own: the rule that puts framing in a separate crate is about a
//! dependency to keep out of other graphs, and this costs zero crates —
//! the task is in the `objc2-foundation` feature already named here.
//!
//! **Three platforms now fit a seam shaped around the first.**
//! `WebSocketConnect` hands over *messages* because that is all a browser
//! can give; WinHTTP turned out to be the same shape and so is
//! Foundation, which delivers an `NSURLSessionWebSocketMessage` and takes
//! one back with the handshake, the masking and the ping/pong inside the
//! system. Three implementations that share no code agreeing on the shape
//! is what says the seam is not the browser's accident.
//!
//! Two details are Foundation's own and are handled rather than papered
//! over: a task opens **lazily**, so the handshake's failure arrives
//! through the first send or receive rather than from `websocket()`; and
//! the peer's close arrives as a **failed receive** with the code on the
//! task, which is read back and reported as
//! [`Message::Close`](hclient_core::unversioned::Message::Close).

mod body;
mod delegate;
mod error;
mod session;
mod websocket;

pub use body::UrlSessionBody;
pub use error::UrlSessionError;
pub use session::UrlSession;
pub use websocket::UrlSessionWebSocket;
