//! Turning what the platform said into what this workspace can act on.
//!
//! Two grammars, neither of them ours, and both written down here because
//! there is no specification for either: a proxy *value* (`host:port`,
//! `http://user:pass@host:port`, `socks5://host`) and a bypass *pattern*
//! (`.example.com`, `*.example.com`, `<local>`, `10.0.0.0/8`).
//!
//! Everything in this file is a pure function over strings, which is the
//! point: the platform half of this crate is four lines that cannot be
//! run on the machine this workspace is developed on, and this half is
//! the rest of it.

use super::{BypassReason, Credentials, ProxyEntry, ProxyKind, Scheme};

use std::fmt;

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

impl std::error::Error for ParseError {}

/// One `(key, value)` from the platform into an entry.
pub(crate) fn entry(
    key: &str,
    value: &str,
    applies_to: Option<Scheme>,
) -> Result<ProxyEntry, ParseError> {
    let value = value.trim();

    // The scheme, where there is one. Its absence is ordinary rather than
    // an error: Windows and macOS both hand back a bare `host:port`, and
    // it is only the environment that tends to carry a URL.
    let (scheme, rest) = match value.split_once("://") {
        Some((s, rest)) => (Some(s.to_ascii_lowercase()), rest),
        None => (None, value),
    };

    let kind = match scheme.as_deref() {
        Some("http") => ProxyKind::Http,
        Some("https") => return Err(ParseError::TlsToProxyUnsupported),
        Some("socks5" | "socks5h") => ProxyKind::Socks5,
        // `socks4a` is `socks4` with a hostname, which is the only form
        // `hclient-native`'s `Socks4` sends anyway, so the two spellings
        // are one kind here rather than a distinction with no consequence.
        Some("socks" | "socks4" | "socks4a") => ProxyKind::Socks4,
        Some(other) => return Err(ParseError::UnknownScheme(other.into())),
        // No scheme. `socks=host:port` is Windows's own spelling for a
        // SOCKS proxy inside `ProxyServer`, and the version it means is
        // argued in `ProxyKind::Socks4`'s doc.
        None if key == "socks" => ProxyKind::Socks4,
        // macOS's SOCKS is SOCKS5 — its own dialog's word, and what
        // Chromium and Firefox both read it as — which is why `read.rs`
        // gives it a different key from Windows's unversioned `socks=`.
        None if key == "socks5" => ProxyKind::Socks5,
        None => ProxyKind::Http,
    };

    // A path on a proxy URL means nothing to any client; a trailing `/`
    // is how everybody writes one anyway.
    let rest = rest.split(['/', '?', '#']).next().unwrap_or("");

    let (credentials, authority) = match rest.rsplit_once('@') {
        Some((userinfo, authority)) => (Some(userinfo), authority),
        None => (None, rest),
    };

    let (host, port) = split_authority(authority)?;
    if host.is_empty() {
        return Err(ParseError::NoHost);
    }

    Ok(ProxyEntry {
        kind,
        host: host.to_ascii_lowercase().into_boxed_str(),
        port: port.unwrap_or(match kind {
            // Plain HTTP to the proxy, so 80 — never 443, which would be
            // the port of the thing this client refuses to speak to.
            ProxyKind::Http => 80,
            ProxyKind::Socks5 | ProxyKind::Socks4 => 1080,
        }),
        applies_to,
        credentials: credentials.map(|u| {
            let (user, password) = u.split_once(':').unwrap_or((u, ""));
            Credentials {
                user: percent_decode(user).into_boxed_str(),
                password: percent_decode(password).into_boxed_str(),
            }
        }),
    })
}

/// `host`, `host:port`, `[::1]`, `[::1]:8080`.
///
/// **An IPv6 literal is why this is not one `rsplit_once(':')`**, which
/// is the same trap `hclient-native`'s own `split_pattern` documents one
/// crate over: `::1` splits into `("::", "1")` and `1` parses as a port,
/// so a bare v6 address would silently become the host `::` at port 1.
/// RFC 3986 §3.2.2's brackets are the disambiguator.
fn split_authority(authority: &str) -> Result<(&str, Option<u16>), ParseError> {
    if let Some(rest) = authority.strip_prefix('[') {
        let (host, after) = rest.split_once(']').ok_or(ParseError::NoHost)?;
        return match after {
            "" => Ok((host, None)),
            _ => {
                let p = after
                    .strip_prefix(':')
                    .ok_or(ParseError::BadPort(after.into()))?;
                Ok((host, Some(port(p)?)))
            }
        };
    }
    match authority.rsplit_once(':') {
        // More than one colon and no brackets: a bare IPv6 literal, which
        // is what macOS stores for `HTTPProxy` when the proxy is one.
        Some(_) if authority.matches(':').count() > 1 => Ok((authority, None)),
        Some((host, p)) => Ok((host, Some(port(p)?))),
        None => Ok((authority, None)),
    }
}

fn port(s: &str) -> Result<u16, ParseError> {
    s.parse().map_err(|_| ParseError::BadPort(s.into()))
}

/// `%40` -> `@`. Only in userinfo, and only because that is where a
/// password with a `@` or a `:` in it has to be written — curl reads them
/// back the same way, and a password taken verbatim would be the wrong
/// password with no way to tell.
fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%'
            && i + 2 < b.len()
            && let Some(v) = hex(b[i + 1]).zip(hex(b[i + 2])).map(|(h, l)| h * 16 + l)
        {
            out.push(v as char);
            i += 3;
            continue;
        }
        out.push(b[i] as char);
        i += 1;
    }
    out
}

fn hex(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

/// What one bypass pattern turns into.
pub(crate) enum Bypass {
    /// A pattern in `hclient-native`'s dialect, ready to hand over.
    Pattern(Box<str>),
    /// The `<local>` / *exclude simple hostnames* rule.
    Local,
    /// A bare `*` — bypass everything.
    Everything,
    /// A pattern that asks for something already true, so honouring it
    /// costs nothing and refusing it would refuse a configuration that is
    /// in fact met exactly.
    AlreadyTrue,
    Unsupported(BypassReason),
}

/// One pattern from `NO_PROXY`, `ProxyOverride` or `ExceptionsList`.
///
/// The three dialects overlap and none of them is specified. What is
/// translated here is the intersection that `hclient-native`'s matcher
/// can state exactly; everything else is surfaced rather than
/// approximated, which is that matcher's own rule — *a pattern in no
/// accepted shape matches nothing rather than approximately something* —
/// applied one layer up, where the caller can still see it.
pub(crate) fn bypass(pattern: &str) -> Bypass {
    let p = pattern.trim().to_ascii_lowercase();
    match p.as_str() {
        "" => return Bypass::AlreadyTrue,
        "*" => return Bypass::Everything,
        "<local>" => return Bypass::Local,
        // Windows's `<-loopback>` turns OFF its implicit loopback bypass.
        // Nothing here bypasses loopback to begin with — `Proxy::bypass`
        // documents that it proxies everything until told otherwise — so
        // the setting is already honoured.
        "<-loopback>" => return Bypass::AlreadyTrue,
        _ => {}
    }
    // A subnet is a pattern this matcher states exactly — see
    // `Proxy::bypass`. It was a refusal until macOS was looked at: every
    // Mac ships `169.254/16` in its default exceptions list, so refusing
    // on a subnet would have refused the platform's own default.
    // `*.example.com` is Windows's and macOS's spelling of what
    // `hclient-native` writes `.example.com`: that host and everything
    // under it. Anywhere else, a `*` is a wildcard this workspace does
    // not have.
    if let Some(rest) = p.strip_prefix("*.") {
        return match rest.contains('*') {
            true => Bypass::Unsupported(BypassReason::Wildcard),
            false => Bypass::Pattern(format!(".{rest}").into_boxed_str()),
        };
    }
    if p.contains('*') {
        return Bypass::Unsupported(BypassReason::Wildcard);
    }
    Bypass::Pattern(p.into_boxed_str())
}

#[cfg(test)]
mod tests {
    use super::*;
    use rstest::rstest;

    fn parse(value: &str) -> Result<ProxyEntry, ParseError> {
        entry("http", value, Some(Scheme::Http))
    }

    #[rstest]
    #[case("proxy.corp:8080", ProxyKind::Http, "proxy.corp", 8080)]
    #[case("http://proxy.corp:8080", ProxyKind::Http, "proxy.corp", 8080)]
    #[case("http://proxy.corp:8080/", ProxyKind::Http, "proxy.corp", 8080)]
    #[case("PROXY.CORP:8080", ProxyKind::Http, "proxy.corp", 8080)]
    #[case("proxy.corp", ProxyKind::Http, "proxy.corp", 80)]
    #[case("socks5://proxy.corp", ProxyKind::Socks5, "proxy.corp", 1080)]
    #[case("socks5h://proxy.corp:1081", ProxyKind::Socks5, "proxy.corp", 1081)]
    #[case("socks4://proxy.corp", ProxyKind::Socks4, "proxy.corp", 1080)]
    #[case("socks://proxy.corp", ProxyKind::Socks4, "proxy.corp", 1080)]
    #[case("[::1]:3128", ProxyKind::Http, "::1", 3128)]
    #[case("[2001:db8::1]", ProxyKind::Http, "2001:db8::1", 80)]
    #[case("  proxy.corp:8080  ", ProxyKind::Http, "proxy.corp", 8080)]
    fn values_this_client_can_act_on(
        #[case] value: &str,
        #[case] kind: ProxyKind,
        #[case] host: &str,
        #[case] port: u16,
    ) {
        let e = parse(value).expect(value);
        assert_eq!((e.kind(), e.host(), e.port()), (kind, host, port));
    }

    #[test]
    fn a_bare_ipv6_literal_is_not_read_as_a_port() {
        // The trap this file's `split_authority` exists for: macOS stores
        // `HTTPProxy` as a bare address, and `rsplit_once(':')` would
        // make `::1` into the host `::` at port 1 — a proxy at an address
        // nobody configured, reached without an error.
        let e = parse("::1").expect("a bare v6 literal");
        assert_eq!((e.host(), e.port()), ("::1", 80));
    }

    #[test]
    fn tls_to_the_proxy_is_refused_rather_than_downgraded() {
        assert_eq!(
            parse("https://proxy.corp:8443"),
            Err(ParseError::TlsToProxyUnsupported)
        );
    }

    #[rstest]
    #[case("gopher://proxy.corp")]
    #[case("proxy.corp:99999")]
    #[case("proxy.corp:")]
    #[case("://proxy.corp")]
    fn values_it_cannot(#[case] value: &str) {
        assert!(parse(value).is_err(), "{value} parsed");
    }

    #[test]
    fn a_windows_socks_entry_is_socks4_without_saying_so() {
        // Windows writes `socks=host:port` with no version anywhere, and
        // curl and Chrome both read that as SOCKS4. The key is what
        // carries the meaning here, so it is passed in rather than
        // guessed from the value.
        let e = entry("socks", "proxy.corp:1080", None).expect("socks entry");
        assert_eq!(e.kind(), ProxyKind::Socks4);
        // The control: the same value under any other key is HTTP, so the
        // kind is genuinely read off the key rather than defaulted.
        assert_eq!(
            entry("http", "proxy.corp:1080", None).unwrap().kind(),
            ProxyKind::Http
        );
    }

    #[rstest]
    #[case("http://alice:hunter2@proxy:8080", "alice", "hunter2")]
    #[case("http://alice@proxy:8080", "alice", "")]
    #[case("http://alice:p%40ss@proxy:8080", "alice", "p@ss")]
    #[case("http://alice:a%3Ab@proxy:8080", "alice", "a:b")]
    // A `%` that begins no escape is a `%`, which is what a password
    // containing one looks like when nobody encoded it.
    #[case("http://alice:100%pure@proxy:8080", "alice", "100%pure")]
    fn credentials_come_out_decoded(
        #[case] value: &str,
        #[case] user: &str,
        #[case] password: &str,
    ) {
        let e = parse(value).expect(value);
        let c = e.credentials().expect("credentials");
        assert_eq!((c.user(), c.password()), (user, password));
        // The host survives the userinfo split — the `@` in an encoded
        // password must not end the userinfo early.
        assert_eq!((e.host(), e.port()), ("proxy", 8080));
    }

    #[rstest]
    // A subnet, including the abbreviated form macOS ships by default.
    #[case("10.0.0.0/8", "10.0.0.0/8")]
    #[case("169.254/16", "169.254/16")]
    #[case("fd00::/8", "fd00::/8")]
    #[case("example.com", "example.com")]
    #[case(".example.com", ".example.com")]
    #[case("*.example.com", ".example.com")]
    #[case("EXAMPLE.COM", "example.com")]
    #[case("example.com:8080", "example.com:8080")]
    #[case("127.0.0.1", "127.0.0.1")]
    #[case("[::1]:8080", "[::1]:8080")]
    fn patterns_that_translate(#[case] pattern: &str, #[case] want: &str) {
        match bypass(pattern) {
            Bypass::Pattern(p) => assert_eq!(&*p, want),
            _ => panic!("{pattern} did not translate"),
        }
    }

    #[rstest]
    #[case("192.168.1.*", BypassReason::Wildcard)]
    #[case("*.*.example.com", BypassReason::Wildcard)]
    #[case("ex*mple.com", BypassReason::Wildcard)]
    fn patterns_that_do_not(#[case] pattern: &str, #[case] reason: BypassReason) {
        match bypass(pattern) {
            Bypass::Unsupported(r) => assert_eq!(r, reason),
            _ => panic!("{pattern} translated, and it should not have"),
        }
    }
}
