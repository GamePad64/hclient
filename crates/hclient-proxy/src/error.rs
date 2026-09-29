//! Every refusal in this crate, and each is a refusal to send bytes
//! somewhere the caller did not choose.
//!
//! That is the whole subject, and it is why three protocols that share no
//! bytes on the wire — HTTP `CONNECT`, SOCKS4 and SOCKS5 — and a reader of
//! the machine's own settings all belong in one file. A proxy's job is to
//! stand between a request and its origin, so **every way this crate can
//! fail is a way that arrangement did not hold**: the proxy said no
//! ([`ProxyRefused`], [`Socks4Refused`], [`Socks5Refused`]), the proxy
//! answered something that is not its protocol ([`ConnectError`],
//! [`Socks4HandshakeError`], [`Socks5HandshakeError`]), or the machine
//! named a configuration this client cannot state exactly
//! ([`SystemProxyRefused`], [`ParseError`]).
//!
//! **The last two are the same rule as the first six, one layer down.**
//! `translate.rs` says it about the machine and it is true of the wire as
//! well: a quiet narrowing here sends traffic direct that somebody routed
//! through a proxy, or through one they excluded — both changes to where
//! the bytes go, made on somebody's behalf and invisible from the call
//! site. So nothing in this crate degrades, and the file is short because
//! there is nothing here but named refusals.
//!
//! **A refusal is not the only thing a caller gets told, and the other
//! kind stayed behind.** `system::UnsupportedBypass` and
//! `system::BypassReason` describe a pattern the machine named that this
//! matcher cannot express — but they implement `Display` and not `Error`,
//! and they are never an `Err`: they are carried on `SystemProxies` as a
//! record of what was read, beside `ignored` and `pac`. They are values,
//! so they live with the settings they describe.
//!
//! Each type is re-exported at the path it already had — the crate root
//! for the three protocols, [`crate::system`] for the two behind the
//! `system` feature — so no consumer's `use` line moves. The two gated
//! ones carry that `#[cfg]` here as well, on the item rather than on a
//! re-export, so a build without the feature has no way to reach a type
//! whose module does not exist.

use hclient_core::error::Error;
use hclient_proto::head;

#[cfg(feature = "system")]
use crate::system::ProxyKind;

/// The proxy refused the tunnel. Deliberately **not** a response: a `407`
/// is the proxy's answer to us, not the origin's answer to the caller,
/// and handing it back as one would report a refusal to connect as an
/// HTTP result the caller could act on.
#[derive(Debug, thiserror::Error)]
#[error("the proxy refused CONNECT with {status}")]
#[non_exhaustive]
pub struct ProxyRefused {
    /// The status the proxy answered — `407` when it wants credentials.
    pub status: http::StatusCode,
}

/// The proxy answered something that is not an HTTP response, or too much
/// of one.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ConnectError {
    /// The bytes the proxy sent back do not parse as an HTTP response head
    /// at all.
    #[error("the proxy's answer to CONNECT is not an HTTP response head: {0}")]
    Malformed(#[source] MalformedHead),
    /// A head that never ends is a proxy holding the connection open at
    /// our expense, and the bound is ours because HTTP states none.
    #[error("the proxy's response head passed {0} bytes without ending")]
    HeadTooLong(usize),
    /// The origin's host and port cannot be written as a `CONNECT`
    /// authority — the host, and the port.
    #[error("`{0}:{1}` cannot be written as an authority")]
    BadAuthority(Box<str>, u16),
    /// A Basic-auth username with a `:` in it. RFC 7617 §2 makes the colon
    /// the separator, so `a:b` with password `c` and `a` with password
    /// `b:c` would put the same bytes on the wire.
    #[error("a Basic-auth username may not contain a colon")]
    ColonInUsername,
}

// Maintainer notes (not rendered):
// A newtype rather than `hclient_proto::head::HeadError` itself, because
// that crate is internal — it promises no stable interface — and a public
// field naming its type would make this crate's stable promise depend on
// one it does not own: the rule `just internal-crates-stay-internal`
// checks. The parser's own error is still the
// `source()`, so nothing a log prints is lost.
/// Why a `CONNECT` response head did not parse. Its [`Display`](std::fmt::Display)
/// names the defect; there is nothing to match on.
#[derive(Debug)]
pub struct MalformedHead(pub(crate) head::HeadError);

impl std::fmt::Display for MalformedHead {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Display::fmt(&self.0, f)
    }
}

// No `source`: the display is the parser error's own, so naming that
// error again as the source would print it twice in a chain, and the
// parser's error has no source of its own to pass on.
impl std::error::Error for MalformedHead {}

/// How a SOCKS5 UDP association failed.
///
/// Two answers because they call for two different things: a proxy that
/// does not relay UDP still carries streams, so a request may switch to
/// one, where any other failure means the proxy could not be used at all.
#[derive(Debug, thiserror::Error)]
pub(crate) enum AssociateError {
    /// The proxy does not relay UDP (`REP=0x07`, command not supported).
    #[error("the SOCKS5 proxy does not relay UDP")]
    Unsupported(#[source] Error),
    /// Any other failure: a malformed reply, a refusal, bad credentials.
    #[error("the SOCKS5 UDP association failed")]
    Failed(#[source] Error),
}

/// What a `USERID` or a host name cannot be.
///
/// `PartialEq`, so a caller that has downcast the source can compare it.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
#[non_exhaustive]
pub enum Socks4HandshakeError {
    /// The field is `NUL`-terminated and has no length prefix, so a `NUL`
    /// inside it would end it early and the rest would be read as the
    /// next field.
    #[error("the SOCKS4 USERID contains a NUL, which terminates the field")]
    NulInUserid,
    /// The reply's first byte is not `0`. SOCKS4's reply carries a version
    /// of **zero**, not four — a detail every implementation gets wrong
    /// once.
    #[error("the SOCKS4 proxy replied with VN={0:#04x}, where the protocol specifies 0x00")]
    BadReplyVersion(u8),
    /// A hostname too long for the field, which has no length prefix and
    /// is `NUL`-terminated — so this is a bound on the whole request
    /// rather than on a byte.
    #[error("the host name is {0} bytes, past what a SOCKS4a request can carry")]
    HostTooLong(usize),
    /// A `NUL` in the hostname, for `NulInUserid`'s reason.
    #[error("the host name contains a NUL, which terminates the field")]
    NulInHost,
}

/// SOCKS4's `CD`, which is one byte and has no HTTP meaning at all.
#[derive(Debug, thiserror::Error)]
#[error("the SOCKS4 proxy refused with CD={cd} ({})", socks4_reply(*cd))]
#[non_exhaustive]
pub struct Socks4Refused {
    /// The `CD` byte the proxy sent back. `90` is a grant; `91`, `92` and
    /// `93` are the protocol's three refusal reasons, and any other value
    /// is unassigned.
    pub cd: u8,
}

/// The four `CD` values the protocol defines, by name.
fn socks4_reply(cd: u8) -> &'static str {
    match cd {
        90 => "request granted",
        91 => "request rejected or failed",
        92 => "rejected: identd unreachable from the proxy",
        93 => "rejected: identd reported a different user",
        _ => "unassigned",
    }
}

/// RFC 1928 §6's `REP`, which is one byte and has no HTTP meaning at all.
#[derive(Debug, thiserror::Error)]
#[error("the SOCKS5 proxy refused with REP={rep:#04x} ({})", socks5_reply(*rep))]
#[non_exhaustive]
pub struct Socks5Refused {
    /// The `REP` byte the proxy sent back, naming why the request was
    /// refused. RFC 1928 §6 defines eight failure reasons (`0x01`–`0x08`);
    /// any other value is unassigned.
    pub rep: u8,
}

fn socks5_reply(rep: u8) -> &'static str {
    match rep {
        0x01 => "general failure",
        0x02 => "connection not allowed by ruleset",
        0x03 => "network unreachable",
        0x04 => "host unreachable",
        0x05 => "connection refused",
        0x06 => "TTL expired",
        0x07 => "command not supported",
        0x08 => "address type not supported",
        _ => "unassigned",
    }
}

/// The proxy would not agree to any method we offered, or refused the
/// credentials. `0xFF` is RFC 1928 §3's "no acceptable methods".
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Socks5HandshakeError {
    /// RFC 1928 §3: the method-selection reply named `0xFF`, meaning the
    /// proxy accepted none of the methods this client offered.
    #[error("the SOCKS5 proxy accepted none of the authentication methods offered")]
    NoAcceptableMethods,
    /// The proxy's method-selection reply names a method this client never
    /// offered.
    #[error("the SOCKS5 proxy chose method {0:#04x}, which was not offered")]
    UnofferedMethod(u8),
    /// The username/password sub-negotiation (RFC 1929) failed: the proxy
    /// did not accept the credentials.
    #[error("the SOCKS5 proxy rejected the username and password")]
    BadCredentials,
    /// The proxy's reply carries a protocol version other than `5`.
    #[error("the SOCKS5 proxy answered version {0} rather than 5")]
    BadVersion(u8),
    /// The proxy's reply names an address type (`ATYP`) RFC 1928 §5 does
    /// not define, so the reply cannot be framed.
    #[error("the SOCKS5 proxy's reply names address type {0:#04x}, which RFC 1928 does not define")]
    BadAddressType(u8),
    /// A destination host name is longer than the 255 bytes SOCKS5's
    /// length-prefixed `DOMAINNAME` field can carry.
    #[error("a SOCKS5 host name must be at most 255 bytes, this one is {0}")]
    HostTooLong(usize),
    /// A username or password is longer than the 255 bytes RFC 1929's
    /// length-prefixed fields can carry.
    #[error("a SOCKS5 username and password must each be at most 255 bytes")]
    CredentialTooLong,
    /// A `DOMAINNAME` (`ATYP=0x03`) that is complete — the length byte and
    /// every byte it named have arrived — and is not valid UTF-8, so it
    /// cannot be handed back as a host name.
    ///
    /// This is a different failure from [`BadAddressType`](Self::BadAddressType):
    /// the address type is one RFC 1928 defines, and the reply is simply
    /// not framed the way a client can read a name from. Reporting it as
    /// an unknown address type would name `0x03`, which is defined —
    /// answering it as `NeedMore` instead would stall forever, since no
    /// further bytes make an already-complete name valid.
    #[error("the SOCKS5 proxy's reply names a DOMAINNAME that is not valid UTF-8")]
    NonUtf8Name,
}

#[cfg(feature = "system")]
/// Why the system's configuration could not be installed on a transport
/// as it stands.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum SystemProxyRefused {
    /// The machine names a proxy whose protocol is not the one this call
    /// installs.
    ///
    /// This call installs HTTP proxies only, so a configuration naming a
    /// SOCKS proxy as well has no faithful reading here: installing half
    /// of it would send some traffic direct. Build the one you want by
    /// hand — `Proxy::new(Socks5::new(), host, port)` — which
    /// also makes the choice visible at the call site, where a rule of
    /// ours would not be.
    #[error(
        "the system names a {kind:?} proxy at {host}:{port}, and this call installs HTTP proxies; \
         build that one with `Proxy::new(..)`"
    )]
    MixedProtocols {
        /// The protocol the system names for this proxy.
        kind: ProxyKind,
        /// The proxy's host, as the system reported it.
        host: Box<str>,
        /// The proxy's port, as the system reported it.
        port: u16,
    },
    /// A bypass pattern this crate's matcher cannot express — a subnet,
    /// or a wildcard that is not a leading `*.`.
    ///
    /// Honouring it approximately is what the matcher's own dialect
    /// refuses to do (*a pattern in no accepted shape matches nothing
    /// rather than approximately something*), and dropping it would put a
    /// host the machine excluded back on the proxy.
    #[error(
        "the system's bypass list contains {0}, which this client's matcher cannot state exactly; \
         read `SystemProxies` yourself and decide, rather than have this call guess"
    )]
    UnrepresentableBypass(Box<str>),
    /// The machine's proxy is a **PAC script**, which decides per request
    /// by running JavaScript, and nothing here runs one.
    ///
    /// Refused rather than ignored, and this is the sharpest of the four:
    /// ignoring it means going **direct** on a machine whose owner routed
    /// its traffic through a proxy — a policy violation, and on a network
    /// where direct egress is blocked, a failure nobody can explain from
    /// the client's side. `hclient-urlsession` honours it on Apple
    /// platforms, because `URLSession` runs the script in the OS.
    #[error(
        "the machine's proxy is the auto-config script at {0}, which decides per request and \
         which nothing here runs; on Apple platforms `hclient-urlsession` honours it in the OS, \
         and otherwise name a proxy explicitly with `Proxy::new(..)`"
    )]
    PacScript(Box<str>),
    /// A credential the machine named cannot become a header — a colon in
    /// the username, or a byte no header value may carry.
    ///
    /// Installing the proxy without it would authenticate against
    /// nothing and collect a `407` the caller could not explain.
    #[error("the credential for the proxy at {host}:{port} cannot be sent as a header")]
    UnusableCredential {
        /// The proxy's host, as the system reported it.
        host: Box<str>,
        /// The proxy's port, as the system reported it.
        port: u16,
    },
}

#[cfg(feature = "system")]
/// A proxy value the platform gave that names no proxy this client can
/// reach.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum ParseError {
    /// A scheme this crate does not know — `quic://`, a typo, a `.pac`
    /// URL that landed in a proxy variable.
    #[error("unknown proxy scheme `{0}`")]
    UnknownScheme(Box<str>),
    /// `host:70000`, `host:` — there is a colon and what follows it is
    /// not a port.
    #[error("`{0}` is not a port")]
    BadPort(Box<str>),
    /// Nothing left after the scheme and the userinfo.
    #[error("no host")]
    NoHost,
}

// Maintainer notes (not rendered):
// **The driver's rule rather than any protocol's**: it lived in
// `hclient-native` until the driver moved here beside the handshakes, and
// what it says did not change. A handshake reports faithfully
// how much of the buffer was its own, and what to make of the rest is a
// question about what happens next. Nothing the origin might say can have
// arrived yet — the client has not written to it — so these bytes are the
// proxy's, and carrying them on would feed them to the TLS handshake, or
// to hyper, as if the origin had sent them. A refusal to connect rather
// than a rewind, because the rewind is the quieter failure and the worse
// one.
/// The proxy sent bytes past the end of its own handshake.
///
/// For HTTP, which speaks first, nothing the origin might say can have
/// arrived yet — the client has not written to it — so these bytes are the
/// proxy's, and carrying them on would feed them to the TLS handshake, or
/// to hyper, as if the origin had sent them. The connection is refused.
/// See [`drive_exact`](crate::drive_exact) for the protocols this does not
/// hold of.
#[derive(Debug, thiserror::Error)]
#[error(
    "the proxy sent {bytes} bytes past its own handshake, before anything was sent to the origin"
)]
#[non_exhaustive]
pub struct ProxySpokeFirst {
    /// How many bytes arrived past the handshake.
    pub bytes: usize,
}

// Maintainer notes (not rendered):
//
// A bypass pattern the system named that this workspace's matcher cannot
// express. It lived in `system` until `Proxy::bypass` became fallible:
// a caller's own list is refused with the same value the machine's is
// reported with, so the two dialects cannot drift apart.
/// A bypass pattern this crate's matcher cannot express — refused by
/// [`Proxy::bypass`](crate::Proxy::bypass), or reported by the system
/// reader.
///
/// It exists so that such a pattern is **visible rather than dropped**.
/// Dropping one silently sends traffic through a proxy that the machine's
/// owner said should go direct, which is a privacy change made on their
/// behalf and without their knowledge — the mirror of the rule that keeps
/// a bypass list from being invented in the first place.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnsupportedBypass {
    pattern: Box<str>,
    reason: BypassReason,
}

impl UnsupportedBypass {
    pub(crate) fn new(pattern: &str, reason: BypassReason) -> Self {
        Self {
            pattern: pattern.into(),
            reason,
        }
    }

    /// The pattern as it was written.
    pub fn pattern(&self) -> &str {
        &self.pattern
    }

    /// Why this pattern could not be translated.
    pub fn reason(&self) -> BypassReason {
        self.reason
    }
}

impl std::error::Error for UnsupportedBypass {}

impl std::fmt::Display for UnsupportedBypass {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "`{}` ({})", self.pattern, self.reason)
    }
}

// Maintainer notes (not rendered):
//
// One variant, and it stays an enum rather than becoming a unit struct:
// `Cidr` was the second until subnets became statable, and the shape
// that admitted a second reason is the shape that will admit the next.
/// Why a bypass pattern could not be translated.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum BypassReason {
    /// `192.168.1.*`, `10.*.*.*` — a wildcard anywhere but as a leading
    /// `*.` label.
    Wildcard,
    /// In none of the accepted forms: an empty pattern, a port that is not
    /// a number, an unclosed bracket, a subnet whose prefix is not an
    /// address or is longer than its family.
    Malformed,
}

impl std::fmt::Display for BypassReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Wildcard => f.write_str("a wildcard that is not a leading `*.`"),
            Self::Malformed => f.write_str("in none of the forms a bypass accepts"),
        }
    }
}
