//! Proxy protocols, as **sans-io handshakes**.
//!
//! Three ship — HTTP `CONNECT`, SOCKS5 and SOCKS4a — and they share no
//! bytes, which is what makes [`Handshake`] evidence that the shape is
//! general rather than the shape of its first caller.
//!
//! ```no_run
//! use hclient_proxy::{HttpConnect, Proxy};
//!
//! let proxy = Proxy::new(HttpConnect::new(), "proxy.corp", 8080)
//!     .bypass([".internal", "localhost"])
//!     .bypass_local();
//! # let _ = proxy;
//! ```
//!
//! # Sans-io, and what that buys
//!
//! Nothing here opens a socket, and nothing here names an IO trait. A
//! handshake is a state machine: it is handed the bytes that arrived and
//! answers with the bytes to send, or *not yet*, or *the tunnel is open*.
//! The transport owns the socket and drives it —
//! `hclient-native`'s `proxy::drive` is thirty lines and is the only
//! place in the family that knows what a `poll_read` is.
//!
//! Two things follow, and the second is the one that was paid for:
//!
//! - Every rule in every protocol is testable **without a socket**, and
//!   the tests here feed the exact byte sequences the RFCs print. A
//!   mutation in the SOCKS5 reply parser is killed by a test that never
//!   opens a file descriptor.
//! - `CONNECT` no longer needs an HTTP client to speak HTTP. It used to
//!   drive `hyper`'s h1 dispatcher through `hclient-native`'s upgrade
//!   seam — which tied the whole proxy family to hyper, to hyper's IO
//!   traits, and to a transport. What replaced it is
//!   [`hclient_proto::head`], because a `CONNECT` response is the one
//!   HTTP message with no body under any framing rule (RFC 9110 §9.3.6).
//!
//! # What it costs, stated where somebody will look for it
//!
//! A protocol that has to **wrap** the IO cannot be written against this
//! seam — TLS to the proxy itself is the real example. That is not a
//! regression: it was unsupported before this crate existed, and
//! [`system::ParseError::TlsToProxyUnsupported`] is where the refusal is
//! already written down. Lifting it means a driver that can hand a
//! handshake an upgraded stream, which is a change to
//! `hclient-native`'s thirty lines rather than to this seam.
//!
//! # Why this is a crate and not a module of `hclient-native`
//!
//! For the `system` feature, and only for it. The protocols themselves
//! carry no dependency — the whole reason `hclient-native`'s `proxy`
//! feature had no `dep:` line — but the machine's own settings carry
//! `proxy_cfg`, and through it `url` and the ICU tables. A feature on the
//! transport would put those into every build in any graph that switched
//! it on, which is the argument that keeps `quinn-proto` in
//! `hclient-tls-quic` and `tungstenite` in `hclient-tungstenite`.
//!
//! The second reason is the one a dependency graph cannot show: a
//! transport that is not `hclient-native` — `hclient-urlsession`, or
//! somebody else's — can read the same settings and speak the same
//! protocols without taking hyper with them.

// **Not `#![forbid(unsafe_code)]`, and this is the only reason.** One
// expression in `system/read.rs` borrows an element of a `CFArray` that
// macOS hands back, because `core-foundation` types that array as raw
// pointers and offers no safe way to read one — see the site, and
// amendment C13 in `docs/exceptions.md`. Everything else here, including
// all three protocols and every parser, is safe code; `deny` rather than
// `forbid` is what lets that one site exist without the attribute being
// deleted wholesale, and `scripts/unsafe-code-policy.sh` is what keeps a
// second site from appearing beside it unannounced.
#![deny(unsafe_code)]

use bytes::{Bytes, BytesMut};

mod connect;
mod proxy;
mod socks4;
mod socks5;
#[cfg(feature = "system")]
pub mod system;

pub use connect::{ConnectError, HttpConnect, ProxyRefused};
pub use proxy::{NoProxy, Proxy, ProxyScheme};
pub use socks4::{Socks4, Socks4HandshakeError, Socks4Refused};
pub use socks5::{Socks5, Socks5HandshakeError, Socks5Refused};

/// What a proxy does for one origin, which is not the same question for
/// the three protocols here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Approach {
    /// The proxy carries bytes. The request is written exactly as it
    /// would be to the origin, because as far as the request is
    /// concerned it is.
    Tunnel,
    /// The proxy is an HTTP origin server for this request: the request
    /// line takes absolute-form (`GET http://example.com/x HTTP/1.1`).
    /// Only an HTTP proxy answers this, and only for `http://`.
    Absolute,
}

/// What a handshake wants to happen next.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Step {
    /// Send these bytes to the proxy, then ask again.
    Write(Bytes),
    /// Nothing to send. More bytes from the proxy are needed before this
    /// handshake can say anything else.
    NeedMore,
    /// The tunnel is open. Whatever is left in the buffer is the
    /// **origin's**, and a driver that drops it loses the peer's first
    /// bytes for good.
    Done,
}

/// Turning a connection **to the proxy** into a connection **to the
/// origin**, as a state machine.
///
/// # The contract, which a driver depends on
///
/// [`begin`](Handshake::begin) is called once and its bytes are sent.
/// [`advance`](Handshake::advance) is then called with everything that
/// has arrived from the proxy and not yet been consumed, and it must:
///
/// - consume from the **front** of `from_peer` only what it has a
///   complete frame for, leaving the rest untouched;
/// - answer [`Step::NeedMore`] when a frame is incomplete, **without**
///   consuming a partial one — a driver reads more and calls again, so a
///   handshake that consumed a fragment would lose it;
/// - answer [`Step::Done`] the moment the tunnel is open, leaving
///   everything it did not consume in `from_peer`.
///
/// The buffer is the driver's, and it is the same one across calls. That
/// is what makes *not yet* free: nothing is copied to hold a partial
/// frame, because the partial frame is never taken.
pub trait Handshake {
    /// Asked once per connection, before anything is dialled.
    fn approach(&self, use_tls: bool) -> Approach;

    /// The first bytes to send, and the host and port they carry.
    ///
    /// `&mut self` because a handshake remembers what it asked for: the
    /// SOCKS5 method it offered decides which reply is legal, and no
    /// state machine can check that without keeping it.
    fn begin(&mut self, host: &str, port: u16) -> Result<Bytes, hclient_core::Error>;

    /// Consume what has arrived; answer what happens next.
    fn advance(&mut self, from_peer: &mut BytesMut) -> Result<Step, hclient_core::Error>;

    /// `Proxy-Authorization` for a request written in absolute-form, if
    /// this proxy wants one.
    ///
    /// Defaulted to `None`, and both SOCKS protocols leave it there —
    /// neither answers [`Approach::Absolute`], and their credentials are
    /// a sub-negotiation on the socket rather than a header. A tunnelled
    /// request carries no such header either: it is addressed to the
    /// origin, and `Proxy-Authorization` belongs to the hop, which is why
    /// `hclient-proto`'s redirect logic already strips it across origins.
    fn proxy_authorization(&self) -> Option<&http::HeaderValue> {
        None
    }
}

/// `n` bytes off the front of `buf`, or `None` with the buffer
/// **untouched**.
///
/// Three lines, shared by both SOCKS protocols, and it is the contract
/// rather than the code that is worth having in one place: a handshake
/// that consumed a partial frame would lose it, because the driver's next
/// read appends to this same buffer. The two protocols share nothing on
/// the wire — see [`Handshake`] — so this is the whole of what they have
/// in common, and it is deliberately not a trait.
pub(crate) fn take(buf: &mut BytesMut, n: usize) -> Option<Bytes> {
    (buf.len() >= n).then(|| buf.split_to(n).freeze())
}

/// Run a handshake to completion over a buffer that already holds every
/// byte the proxy will send, for tests and for nothing else.
///
/// `#[doc(hidden)]`: a real driver reads from a socket between calls, and
/// this one cannot. It exists so that a protocol's own tests can assert
/// the **whole** exchange — what went out, in what order, and what was
/// left over — in one call, which is the property that a sans-io
/// handshake makes testable at all.
#[doc(hidden)]
pub fn drive_for_test<H: Handshake>(
    h: &mut H,
    host: &str,
    port: u16,
    mut answer: impl FnMut(&[u8]) -> Vec<u8>,
) -> Result<(Vec<Vec<u8>>, Bytes), hclient_core::Error> {
    let mut written = Vec::new();
    let mut buf = BytesMut::new();

    let first = h.begin(host, port)?;
    buf.extend_from_slice(&answer(&first));
    written.push(first.to_vec());

    loop {
        match h.advance(&mut buf)? {
            Step::Done => return Ok((written, buf.freeze())),
            Step::Write(b) => {
                buf.extend_from_slice(&answer(&b));
                written.push(b.to_vec());
            }
            Step::NeedMore => {
                // The fixture has nothing left to say, so a handshake
                // still asking for bytes would hang a real driver. In a
                // test that is a bug in the fixture or in the machine,
                // and either way it must fail rather than loop.
                return Err(hclient_core::Error::new(
                    hclient_core::ErrorKind::Connect,
                    std::io::Error::from(std::io::ErrorKind::UnexpectedEof),
                ));
            }
        }
    }
}
