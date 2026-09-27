//! Proxies for [`Native`](crate::Native): the configuration, the seam
//! and the three protocols.
//!
//! The protocols are `hclient-proxy`'s and are **sans-io**: state machines
//! handed the bytes that arrived, answering with the bytes to send. This
//! transport drives them over its socket.
//!
//! # The origin's name goes to the proxy
//!
//! [`TcpConnect::connect`](hclient_rt::TcpConnect::connect) takes a
//! `SocketAddr` and nothing else, so a wrapper implementing it could never
//! hand the proxy the origin's **name** — the client would resolve it
//! locally and leak exactly the DNS a proxy user is often there to hide,
//! and `http://` could never take absolute-form, because that is decided
//! where the request head is written. A proxy replaces the resolve →
//! Happy-Eyeballs → connect block; it does not decorate the socket.

// Maintainer notes (not rendered):
// Driving a [`Handshake`] over a socket — the whole of what proxying
// costs a transport.
//
// The protocols themselves are `hclient-proxy`'s and are **sans-io**: state machines that are
// handed the bytes that arrived and answer with the bytes to send. This
// file is the thirty lines that know what a `poll_read` is, and it is
// the same thirty lines for all three of them.
//
// # Why the protocols are a crate and this is not
//
// Because the split falls where a dependency does. The protocols carry
// none — which is why this feature has no `dep:` line — but the
// machine's own settings carry `proxy_cfg`, and through it `url` and the
// ICU tables. A feature on this crate would put those into every build
// in any graph that switched it on, which is the argument that keeps
// `tungstenite` in `hclient-tungstenite` (and kept `quinn-proto` out of
// `hclient-tls` until that seam stopped carrying it at all).
//
// What the split bought beyond that is measurable in this file: driving
// `CONNECT` used to mean driving **hyper's h1 dispatcher** through
// `crate::upgrade`, because writing one request and reading one response
// needed an HTTP client. It does not any more — see
// `hclient_proxy::connect` — so this file has one loop rather than a
// special case.
//
// # Why this is not a `TcpConnect` wrapper, which would have cost nothing

#[cfg(feature = "system-proxy")]
#[doc(inline)]
pub use hclient_proxy::system;
pub use hclient_proxy::{Approach, Handshake, IpcProxy, Proxy, ProxyScheme, Reach, Step};
// Maintainer notes (not rendered):
// The three protocols, behind the `proxy` feature exactly as they were
// before they moved. The seam above is unconditional because `Native`'s
// proxy constructors name `Proxy` and `Handshake` whatever the features.
/// The three protocols, behind the `proxy` feature. The seam above is
/// unconditional because `Native`'s proxy constructors name it.
#[cfg(feature = "proxy")]
pub use hclient_proxy::{
    ConnectError, HttpConnect, MalformedHead, ProxyRefused, Socks4, Socks4HandshakeError,
    Socks4Refused, Socks5, Socks5HandshakeError, Socks5Refused,
};

/// How the *request* is written, which is the one thing a proxy changes
/// above the socket.
///
/// [`AbsoluteForm`](Via::AbsoluteForm) is only ever the answer for an
/// HTTP proxy carrying an `http://` request, where the proxy is an origin
/// server for this request — RFC 9112 §3.2.2's absolute-form.
#[derive(Debug, Clone)]
pub(crate) enum Via {
    Direct,
    AbsoluteForm(Option<http::HeaderValue>),
}
