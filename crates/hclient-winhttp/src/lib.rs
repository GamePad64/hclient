//! Windows' WinHTTP behind [`Transport`](hclient_core::unversioned::Transport).
//!
//! The fifth ambient backend — after `hclient-wasi`, `hclient-fetch` and
//! `hclient-urlsession`, it owns no connection of its own — and it exists
//! for the list of things a userspace stack cannot reach on Windows.
//!
//! # Why this rather than `hclient-native` with rustls
//!
//! **The system proxy, including a PAC script the OS evaluates.** This is
//! the sharpest of the four and the one with a name already in this
//! workspace: `hclient-proxy`'s `system` feature reads WinINET's settings
//! and **refuses** a machine whose proxy is an auto-config script, because
//! nothing there runs JavaScript — `SystemProxyRefused::PacScript`, whose
//! own doc calls ignoring it *a policy violation, and on a network where
//! direct egress is blocked, a failure nobody can explain from the
//! client's side*. That error names `hclient-urlsession` as the answer on
//! Apple platforms and had no Windows answer to name. This is it:
//! `WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY` hands the whole question —
//! WPAD discovery, the script, its per-URL verdict — to WinHTTP, which
//! runs it in the OS.
//!
//! **Enterprise roots and SChannel policy.** Trust decisions are the
//! machine's: roots pushed by Group Policy or MDM, revocation
//! configuration, FIPS mode, and whatever a future Windows adds. That is a
//! fact about the device rather than a preference, which is the argument
//! `hclient-tls-native-tls` is built on one seam over — and the same
//! caveat applies here as there: it is the *reason* to be here, and the
//! price is that nothing about TLS is configurable through this seam.
//!
//! **Windows credential integration.** `WinHttpSetCredentials` reaches
//! Negotiate and NTLM against the logged-on identity, which no portable
//! client can do. Not wired up yet — see *deliberately not done* below —
//! but it is reachable only from here.
//!
//! # What it deliberately does NOT take from the OS
//!
//! WinHTTP will follow redirects and keep cookies for you, and this
//! transport **turns both off**, for the reason `hclient-urlsession` turns
//! off three: they are portable behaviour this workspace already
//! implements once, and a caller must not lose `hclient`'s versions by
//! choosing this backend for the OS-owned things it *is* here for.
//!
//! - `WINHTTP_DISABLE_REDIRECTS`, so a `3xx` is an ordinary response and
//!   `Client`'s policy decides — its hop limit, its `Authorization`
//!   stripping across origins, its per-hop predicate. This backend
//!   therefore reports
//!   [`RedirectSupport::Transparent`](hclient_core::RedirectSupport::Transparent),
//!   as `hclient-urlsession` does and `hclient-fetch` cannot.
//! - `WINHTTP_DISABLE_COOKIES`, so `Client`'s jar is the only one.
//!
//! **Decompression is the one where this backend is better placed than
//! either of its ambient siblings**, and it takes a decision to keep it
//! that way. `hclient-fetch` and `hclient-urlsession` both report
//! [`DecompressionSupport::Internal`](hclient_core::DecompressionSupport::Internal):
//! the platform decodes the body and there is no way to ask it not to.
//! WinHTTP decodes only when asked — `WINHTTP_OPTION_DECOMPRESSION`,
//! opt-in since Windows 8.1 — so **not asking** leaves `Content-Encoding`
//! on the wire and `hclient`'s own `gzip`/`brotli`/`deflate`/`zstd` in
//! force, identically to `hclient-native`. That is what this reports, and
//! it is a choice rather than a limitation.
//!
//! There is no cache decision to make: WinHTTP has no response cache at
//! all, unlike WinINET, so [`Capabilities::owns_cache`](hclient_core::Capabilities::owns_cache)
//! is `false` by construction rather than by a call.
//!
//! # Deliberately not done, each with what it would need
//!
//! - **HTTP/2.** `WINHTTP_OPTION_ENABLE_HTTP_PROTOCOL` with
//!   `WINHTTP_PROTOCOL_FLAG_HTTP2` is one call away, and it is not made:
//!   turning it on changes what every request puts on the wire, and
//!   [`Capabilities::version_reported`](hclient_core::Capabilities::version_reported)
//!   would then have to become `true`, which obliges reading
//!   `WINHTTP_QUERY_VERSION` back on every response. Neither half is
//!   verifiable without a Windows machine, and a capability that lies is
//!   worse than one that under-reports — this workspace's floor rule.
//! - **Streaming request bodies.** `WinHttpWriteData` is the piece, plus a
//!   `WRITE_COMPLETE` arm in the callback. Until then a
//!   [`RequestBody::Streaming`](hclient_core::RequestBody::Streaming) is a
//!   typed `Unsupported` error rather than a silent empty body, which is
//!   the same refusal `hclient-urlsession` makes and for the same reason.
//! - **WebSocket.** `WinHttpWebSocketCompleteUpgrade` and its five
//!   neighbours exist, and the seam for them is
//!   `hclient_core::unversioned::WebSocketConnect`. A separate crate, by
//!   the rule that put the framing in `hclient-tungstenite`.
//! - **Windows authentication.** `WinHttpSetCredentials` plus
//!   `WinHttpQueryAuthSchemes`, driven from a `401` — the shape
//!   `hclient`'s digest support already has one layer up, and it needs a
//!   decision about which schemes to offer silently.
//!
//! # What has not been observed
//!
//! Every claim here about *what WinHTTP does* is read from its
//! documentation and from `windows-sys` 0.61's declarations. **No line of
//! this crate has been run**: it is cross-checked with `cargo check
//! --target x86_64-pc-windows-msvc --all-targets` and nothing more, for
//! want of a Windows machine. The async contract this leans on hardest —
//! that a buffer handed to `WinHttpReadData` is WinHTTP's until
//! `READ_COMPLETE`, and that `HANDLE_CLOSING` is the last callback a
//! handle ever gets — is stated in `sys.rs` where the code depends on it,
//! so the next person with a Windows box knows exactly what to check.
#![cfg(windows)]

mod body;
mod error;
mod session;
mod sys;

pub use body::WinHttpBody;
pub use error::WinHttpError;
pub use session::WinHttp;
