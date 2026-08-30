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

use hclient_proto::head;

#[cfg(feature = "system")]
use crate::system::ProxyKind;
#[cfg(feature = "system")]
use std::fmt;

/// The proxy refused the tunnel. Deliberately **not** a response: a `407`
/// is the proxy's answer to us, not the origin's answer to the caller,
/// and handing it back as one would report a refusal to connect as an
/// HTTP result the caller could act on.
#[derive(Debug, thiserror::Error)]
#[error("the proxy refused CONNECT with {0}")]
pub struct ProxyRefused(pub http::StatusCode);

/// The proxy answered something that is not an HTTP response, or too much
/// of one.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ConnectError {
    #[error("the proxy's answer to CONNECT is not an HTTP response head: {0}")]
    Malformed(#[from] head::HeadError),
    /// A head that never ends is a proxy holding the connection open at
    /// our expense, and the bound is ours because HTTP states none.
    #[error("the proxy's response head passed {0} bytes without ending")]
    HeadTooLong(usize),
    #[error("`{0}:{1}` cannot be written as an authority")]
    BadAuthority(Box<str>, u16),
}

/// What a `USERID` or a host name cannot be.
///
/// `PartialEq` because `userid` hands one straight back rather than
/// wrapping it, so a caller — and this crate's own tests — compares it.
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
    #[error("the SOCKS5 proxy accepted none of the authentication methods offered")]
    NoAcceptableMethods,
    #[error("the SOCKS5 proxy chose method {0:#04x}, which was not offered")]
    UnofferedMethod(u8),
    #[error("the SOCKS5 proxy rejected the username and password")]
    BadCredentials,
    #[error("the SOCKS5 proxy answered version {0} rather than 5")]
    BadVersion(u8),
    #[error("a SOCKS5 host name must be at most 255 bytes, this one is {0}")]
    HostTooLong(usize),
    #[error("a SOCKS5 username and password must each be at most 255 bytes")]
    CredentialTooLong,
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
    /// A transport holds one `P`, so a configuration naming both an HTTP
    /// and a SOCKS proxy has no faithful reading here. Build the one you
    /// want by hand — `Proxy::new(Socks5::new(), host, port)` — which
    /// also makes the choice visible at the call site, where a rule of
    /// ours would not be.
    #[error(
        "the system names a {kind:?} proxy at {host}:{port}, and this call installs HTTP proxies; \
         a transport holds one proxy protocol, so build that one with `Proxy::new(..)`"
    )]
    MixedProtocols {
        kind: ProxyKind,
        host: Box<str>,
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
    UnusableCredential { host: Box<str>, port: u16 },
}

#[cfg(feature = "system")]
/// A proxy value the platform gave that names no proxy this client can
/// reach.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum ParseError {
    /// `https://proxy:8443` — TLS *to the proxy*, which is not the same
    /// thing as an `HTTPS_PROXY` (that one names the proxy for `https://`
    /// requests, and the hop to it is ordinary HTTP).
    ///
    /// **Refused rather than downgraded**, and this is the one refusal
    /// here that is about safety rather than tidiness: reading it as
    /// plaintext would send the `CONNECT` line, and any
    /// `Proxy-Authorization` with it, in the clear to a proxy whose owner
    /// configured TLS precisely so that it would not be.
    TlsToProxyUnsupported,
    /// A scheme this crate does not know — `quic://`, a typo, a `.pac`
    /// URL that landed in a proxy variable.
    UnknownScheme(Box<str>),
    /// `host:70000`, `host:` — there is a colon and what follows it is
    /// not a port.
    BadPort(Box<str>),
    /// Nothing left after the scheme and the userinfo.
    NoHost,
}

#[cfg(feature = "system")]
impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TlsToProxyUnsupported => f.write_str(
                "an `https://` proxy URL means TLS to the proxy itself, which this client cannot \
                 speak; for a proxy that serves `https://` requests, name it as `http://`",
            ),
            Self::UnknownScheme(s) => write!(f, "unknown proxy scheme `{s}`"),
            Self::BadPort(p) => write!(f, "`{p}` is not a port"),
            Self::NoHost => f.write_str("no host"),
        }
    }
}

#[cfg(feature = "system")]
impl std::error::Error for ParseError {}
