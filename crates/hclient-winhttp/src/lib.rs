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
//! # HTTP/2 and HTTP/3, and the read-back that the obvious call gets wrong
//!
//! [`WinHttp::protocols`] switches them on —
//! `WINHTTP_OPTION_ENABLE_HTTP_PROTOCOL` with
//! `WINHTTP_PROTOCOL_FLAG_HTTP2` (`0x1`) and `WINHTTP_PROTOCOL_FLAG_HTTP3`
//! (`0x2`) — and they are **off by default**, because turning one on
//! changes what every request puts on the wire and, for HTTP/3, which
//! transport protocol carries it. That is `hclient-native`'s decision
//! about its own QUIC arm, made here for the same reason.
//!
//! **This section replaces one that said the feature was one call away
//! and named the wrong second call.** It read that enabling HTTP/2 would
//! oblige reading `WINHTTP_QUERY_VERSION` back on every response to keep
//! [`Capabilities::version_reported`](hclient_core::Capabilities::version_reported)
//! honest. It would not: `WINHTTP_QUERY_VERSION` reads the **status
//! line**, an HTTP/2 or HTTP/3 response has none, and WinHTTP synthesises
//! `HTTP/1.1` into the raw header block this crate already parses. A
//! client following that instruction reports every h2 and h3 response as
//! HTTP/1.1 — which is the capability that lies the paragraph was written
//! to prevent, arriving through the repair. The option that answers is
//! `WINHTTP_OPTION_HTTP_PROTOCOL_USED`, and .NET's `WinHttpResponseParser`
//! bypasses the status line entirely once it reads non-zero.
//!
//! It is queried on **every** response, including where nothing was
//! enabled, so the claim rests on what WinHTTP reports rather than on the
//! mask's documented `0x0` default still being the default on a Windows
//! nobody here has seen.
//!
//! **A demand is now honoured rather than refused**, which is the other
//! half. `WINHTTP_OPTION_HTTP_PROTOCOL_REQUIRED` prevents a fallback off
//! the mask, so a [`RequireVersion`](hclient_core::RequireVersion) demand
//! narrows the mask for that one request and WinHTTP refuses the
//! connection rather than quietly answering over HTTP/1.1 —
//! [`Capabilities::version_select`](hclient_core::Capabilities::version_select)
//! is `true`. Without that option a demand could only be *noticed* after
//! the head, which is `check_version`'s own definition of a check placed
//! too late.
//!
//! **`version_select` was `false` before any of this and should not have
//! been.** `Client` refused every demand, `RequireVersion(HTTP_11)`
//! included — the one demand this backend has satisfied trivially since
//! it existed, and exactly the failure `Capabilities::version_select`'s
//! own doc names.
//!
//! [`WinHttp::keep_alive`] is the third of the group:
//! `WINHTTP_OPTION_HTTP2_KEEPALIVE` and `WINHTTP_OPTION_HTTP3_KEEPALIVE`
//! put the ping clock in the OS, where `hclient-native` needs
//! `multiplexed()` and a spawned driver of its own to send one.
//!
//! # Deliberately not done, each with what it would need
//!
//! - **Streaming request bodies.** `WinHttpWriteData` is the piece, plus a
//!   `WRITE_COMPLETE` arm in the callback. Until then a
//!   [`RequestBody::Streaming`](hclient_core::RequestBody::Streaming) is a
//!   typed `Unsupported` error rather than a silent empty body, which is
//!   the same refusal `hclient-urlsession` makes and for the same reason.
//! - **The rest of the HTTP/2 and HTTP/3 knobs.** WinHTTP documents
//!   `WINHTTP_OPTION_HTTP2_RECEIVE_WINDOW` (the pair `H2Opts` calls
//!   `initial_stream_window`/`connection_window` one crate over, through a
//!   `WINHTTP_HTTP2_RECEIVE_WINDOW` struct rather than a `DWORD`),
//!   `WINHTTP_OPTION_DISABLE_STREAM_QUEUE` — open a second connection at
//!   the peer's stream limit rather than queue, which is the decision
//!   `hclient-native` made the other way and pins with a test — and three
//!   HTTP/3 dials: `HTTP3_HANDSHAKE_TIMEOUT`, `HTTP3_INITIAL_RTT` and the
//!   query-only `HTTP3_STREAM_ERROR_CODE`. Each is a setter and none has
//!   a reader: no caller in this workspace can ask for one, and a knob
//!   with no consumer is what `UpgradeSupport`'s spare variants were
//!   deleted for. They are listed because the next person will otherwise
//!   re-read the option table to find them.
//! - **Windows authentication.** `WinHttpSetCredentials` plus
//!   `WinHttpQueryAuthSchemes`, driven from a `401` — the shape
//!   `hclient`'s digest support already has one layer up, and it needs a
//!   decision about which schemes to offer silently.
//!
//! # WebSocket, and the rule this crate cited at itself
//!
//! [`WinHttpWebSocket`] implements
//! [`WebSocketConnect`](hclient_core::unversioned::WebSocketConnect), and
//! it is **in this crate** rather than one of its own. The list above used
//! to say the opposite, citing the rule that put the framing in
//! `hclient-tungstenite` — and that rule is about a *dependency*: a
//! `websocket` feature on `hclient-native` would have put `tungstenite`
//! into every build in any graph that switched it on. Measured here, the
//! whole feature costs **zero crates**: `WinHttpWebSocketSend` and its
//! five neighbours are in the `Win32_Networking_WinHttp` feature this
//! crate already names. `hclient-fetch` is the precedent that fits, and
//! it keeps its seam impl at home for the same reason.
//!
//! **Almost none of RFC 6455 is here.** WinHTTP writes the handshake,
//! checks the `Sec-WebSocket-Accept`, masks, frames, and answers pings.
//! What is left is assembling fragments into messages, checking that a
//! text message is UTF-8 — the seam requires an error rather than a lossy
//! conversion, and WinHTTP does not check — and routing one completion
//! queue to two readers, since `Stream` and `Sink` are on one value.
//!
//! That a message-oriented seam designed around the browser fitted a
//! second backend unchanged is the strongest evidence it has had that its
//! shape is not the browser's accident.
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
mod websocket;

pub use body::WinHttpBody;
pub use error::WinHttpError;
pub use session::{Protocols, WinHttp};
pub use websocket::WinHttpWebSocket;
