//! Which proxy serves which request — the part that is a rule rather
//! than a protocol.
//!
//! Everything here is a pure function of a host, a port and a scheme.
//! Nothing in this file knows what a socket is, which is why the whole of
//! it is testable with `assert!`.

/// Which request scheme a proxy serves, for a caller who has more than
/// one.
///
/// The distinction that motivates it is the ordinary corporate one — an
/// `HTTP_PROXY` and an `HTTPS_PROXY` pointing at different hosts. The two
/// may speak different protocols: a [`Rules`](crate::Rules) list holds any
/// mix.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ProxyScheme {
    /// Plain `http://` requests.
    Http,
    /// `https://` requests.
    Https,
}

// Private: a public `Proxy` is always reached over TCP — a same-machine
// proxy is an `IpcProxy`, and only `Rules` turns one into a `Proxy` with
// `Reach::Ipc` inside. Public, it froze that representation (an `Arc` and
// all) and made `host()` answer `""` for a value no caller could hold.
/// How a proxy itself is reached.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Reach {
    /// By name or literal and port, dialled by the transport exactly as a
    /// direct connection would be.
    Tcp {
        /// The proxy's host.
        host: Box<str>,
        /// The proxy's port.
        port: u16,
    },
    /// Over a same-machine socket, for a rule made from an [`IpcProxy`].
    Ipc(std::sync::Arc<hclient_rt::IpcAddr>),
}

/// Where a proxy lives, and which protocol it speaks.
#[derive(Debug, Clone)]
pub struct Proxy<P> {
    protocol: P,
    reach: Reach,
    bypass: Vec<Box<str>>,
    /// The `<local>` rule — see [`Proxy::bypass_local`]. A flag rather
    /// than a pattern because it is a rule about the shape of a name.
    bypass_local: bool,
    /// `None` — the default — means both schemes, which is what a caller
    /// with one proxy wants and what `Proxy::new` gives them.
    only: Option<ProxyScheme>,
    /// Whether the hop to the proxy itself is TLS — see [`Proxy::tls`].
    tls: bool,
}

impl<P> Proxy<P> {
    /// A proxy at `host:port`, speaking `protocol`, with no bypass list and
    /// serving both schemes.
    pub fn new(protocol: P, host: impl Into<Box<str>>, port: u16) -> Self {
        Self {
            protocol,
            reach: Reach::Tcp {
                host: host.into(),
                port,
            },
            bypass: Vec::new(),
            bypass_local: false,
            only: None,
            tls: false,
        }
    }

    /// Reach the proxy over TLS: the connection to the proxy itself is
    /// encrypted, checked against the proxy's `host` with the transport's
    /// own TLS backend and trust — an `https://` proxy URL.
    ///
    /// Any protocol may be spoken over it. A proxy reached over a socket
    /// has no such method: there is no name to check a certificate
    /// against.
    ///
    /// The backend is the transport's own, configuration included: a
    /// client certificate it presents by default is presented to a proxy
    /// that asks for one, exactly as to an origin that does.
    #[must_use]
    pub fn tls(mut self) -> Self {
        self.tls = true;
        self
    }

    /// Whether this proxy is reached over TLS.
    pub fn is_tls(&self) -> bool {
        self.tls
    }

    // Maintainer notes (not rendered):
    // That is still the rule, and `system` is not an exception
    // to it: it reads the environment and the platform's own settings
    // *because a caller called it*, which is the transport's builder
    // exercising exactly the policy this paragraph reserves for them.
    /// Origins this proxy does **not** serve, which go direct instead.
    ///
    /// # Why there is no default, and why nothing is read from the
    /// environment
    ///
    /// Excluding loopback by default would be this crate deciding, on a
    /// caller's behalf, that a request they asked to proxy should not be —
    /// a default that changes what goes on the wire without being asked,
    /// which is the shape `TcpOpts`' every-field-off default exists to
    /// avoid. So a proxy proxies everything until told otherwise, and a
    /// caller who also talks to `127.0.0.1` says so here.
    ///
    /// `HTTP_PROXY`/`NO_PROXY` are a different question and stay out of
    /// *this* method: *which* variables, whose matching dialect, and
    /// whether a library may read the environment at all are policy, and
    /// policy belongs to whoever builds the transport. **This** list is
    /// not policy — the caller wrote it down.
    ///
    /// # The rules, which are small on purpose
    ///
    /// `NO_PROXY` has no specification and every implementation disagrees
    /// about the corners. Rather than pick one dialect and be subtly
    /// wrong, these are the forms this accepts, matched
    /// case-insensitively against the request's host:
    ///
    /// - `example.com` — that host exactly, at any port.
    /// - `.example.com` — that host **and** any subdomain of it.
    /// - `example.com:8080` — that host at that port alone.
    /// - `127.0.0.1`, `::1` — an address literal is just a host. A v6 one
    ///   takes RFC 3986 brackets to carry a port: `[::1]:8080`.
    /// - `10.0.0.0/8`, `169.254/16`, `fd00::/8` — a subnet, matched
    ///   against a host that **is** an address literal and against
    ///   nothing else. The abbreviated form is accepted because macOS
    ///   ships one: `169.254/16` is in the default exceptions list of
    ///   every Mac, so a dialect without it would refuse the platform's
    ///   own default configuration.
    ///
    /// No wildcard, with one exception every platform writes:
    /// `*.example.com` is read as `.example.com`. Anything else is refused
    /// by name rather than kept as a pattern that never matches — a list
    /// that silently ignored a typo would proxy a request the caller asked
    /// to send direct.
    ///
    /// **A subnet never matches a name**, not even one that resolves into
    /// it. Matching would mean resolving the host to decide whether to
    /// proxy it — a DNS lookup done before, and in addition to, the one
    /// the connection needs, and on a proxied request the leak a proxy
    /// user is often there to avoid. Every implementation that gets this
    /// right does the same.
    ///
    /// # A bypass belongs to the proxy that carries it
    ///
    /// With one proxy — the overwhelming majority — a bypassed host goes
    /// direct, and that is `NO_PROXY`'s meaning. With several, a bypassed
    /// host **falls through to the next proxy**, and only goes direct when
    /// the list runs out.
    ///
    /// The global reading is the worse one *because* the list exists: a
    /// host bypassed on an `https`-only proxy would take an `http://`
    /// request direct, past an `http` proxy that was never in the running
    /// and never mentioned it. A caller who wants the global rule writes
    /// the list on each proxy, which is honest because they wrote it.
    ///
    /// # Errors
    ///
    /// [`UnsupportedBypass`](crate::UnsupportedBypass), naming the first
    /// pattern in none of the forms above: a wildcard anywhere but a
    /// leading `*.`, or a pattern that is malformed.
    pub fn bypass<S: AsRef<str>>(
        mut self,
        patterns: impl IntoIterator<Item = S>,
    ) -> Result<Self, crate::UnsupportedBypass> {
        for p in patterns {
            let p = p.as_ref();
            let normal =
                normalize_bypass(p).map_err(|reason| crate::UnsupportedBypass::new(p, reason))?;
            self.bypass.push(normal);
        }
        Ok(self)
    }

    /// Patterns already through [`normalize_bypass`] — the system reader's,
    /// which it checked and reported on as it read them.
    #[cfg(feature = "system")]
    pub(crate) fn bypass_normalized(
        mut self,
        patterns: impl IntoIterator<Item = Box<str>>,
    ) -> Self {
        self.bypass.extend(patterns);
        self
    }

    // Maintainer notes (not rendered):
    // Widening the dialect to fit
    // would have made every other pattern harder to read, for one rule
    // that is a boolean everywhere it comes from.
    /// Also send a host with **no dot in it** direct — `intranet`,
    /// `localhost`, `build-server`.
    ///
    /// Off by default, like every other field here, and for
    /// [`bypass`](Self::bypass)'s reason: a default that takes traffic off
    /// a proxy the caller asked for is a decision made on their behalf.
    ///
    /// # Why this is a flag and not a pattern
    ///
    /// Because it is not a pattern. Windows spells it `<local>` in
    /// `ProxyOverride` and macOS spells it *Exclude simple hostnames*;
    /// both are a rule about the **shape** of a name rather than a name,
    /// and the dialect above is deliberately small enough that no pattern
    /// in it can say "any host with no dot".
    ///
    /// It is here because `system` meets it constantly rather
    /// than in a corner: macOS ships with it **on**, so a translation that
    /// could not express it would be wrong on most Macs.
    ///
    /// The rule is the platforms' own and it is about dots, not about
    /// resolution: `10.0.0.5` has dots and is not local by it, and
    /// `localhost` is local by it only because it happens to have none.
    #[must_use]
    pub fn bypass_local(mut self) -> Self {
        self.bypass_local = true;
        self
    }

    /// Use this proxy for one scheme only.
    ///
    /// The default is both, and stays the honest default: a caller who
    /// names one proxy means it for everything, and narrowing it silently
    /// would send half their traffic direct.
    ///
    /// Ordering is the caller's, not a precedence rule of ours: a
    /// transport builds a list and **the first entry that serves a
    /// request wins**. So an unrestricted proxy placed first shadows
    /// everything after it, which is visible at the call site rather than
    /// hidden in a rule.
    #[must_use]
    pub fn only_for(mut self, scheme: ProxyScheme) -> Self {
        self.only = Some(scheme);
        self
    }

    /// Whether this proxy is used for a request to `host:port` under
    /// `use_tls`.
    ///
    /// Two questions in one, and they are asked in this order because they
    /// fail differently: a scheme this proxy does not serve means *try the
    /// next proxy*, where a bypassed host means *go direct* — and the
    /// caller of this function collapses them only because a list that
    /// runs out is itself "go direct".
    pub(crate) fn serves(&self, use_tls: bool, host: &str, port: u16) -> bool {
        let wanted = if use_tls {
            ProxyScheme::Https
        } else {
            ProxyScheme::Http
        };
        if self.only.is_some_and(|only| only != wanted) {
            return false;
        }
        let host = host.trim_start_matches('[').trim_end_matches(']');
        // Asked before the patterns because it is cheaper and because the
        // two are independent: `<local>` is a rule about the shape of the
        // name, the patterns are about the name itself.
        if self.bypass_local && !host.contains('.') {
            return false;
        }
        !self.bypass.iter().any(|p| matches_bypass(p, host, port))
    }

    /// The scheme this proxy is restricted to, if any.
    pub fn scheme(&self) -> Option<ProxyScheme> {
        self.only
    }

    /// The configured protocol, as a template — not the per-connection
    /// state machine: clone it for one of those, since every connection
    /// needs its own.
    pub fn protocol(&self) -> &P {
        &self.protocol
    }

    /// How this proxy itself is reached.
    pub(crate) fn reach(&self) -> &Reach {
        &self.reach
    }

    /// The same proxy with its protocol converted — how a list of mixed
    /// protocols is built without re-stating every other setting.
    pub fn map_protocol<Q>(self, f: impl FnOnce(P) -> Q) -> Proxy<Q> {
        Proxy {
            protocol: f(self.protocol),
            reach: self.reach,
            bypass: self.bypass,
            bypass_local: self.bypass_local,
            only: self.only,
            tls: self.tls,
        }
    }

    /// The proxy's host, as configured.
    pub fn host(&self) -> &str {
        match &self.reach {
            Reach::Tcp { host, .. } => host,
            Reach::Ipc(_) => "",
        }
    }

    /// The proxy's port, as configured.
    pub fn port(&self) -> u16 {
        match &self.reach {
            Reach::Tcp { port, .. } => *port,
            Reach::Ipc(_) => 0,
        }
    }
}

/// A proxy reached over a same-machine socket rather than TCP — Tor's
/// `SocksPort unix:/path` shape.
///
/// Its own type rather than a [`Proxy`] with an address kind, because a
/// transport needs a same-machine dialler to reach it and proves it has
/// one in the method that accepts this type; a `Proxy` goes wherever a
/// dialler by name can.
#[derive(Debug, Clone)]
pub struct IpcProxy<P> {
    addr: std::sync::Arc<hclient_rt::IpcAddr>,
    // Everything but where the proxy is: `reach` stays the placeholder
    // `Proxy::new` gave it until `into_proxy` sets it from `addr`, so the
    // address is held once.
    inner: Proxy<P>,
}

impl<P> IpcProxy<P> {
    /// `protocol`, spoken to a proxy listening on `addr`.
    pub fn new(protocol: P, addr: hclient_rt::IpcAddr) -> Self {
        Self {
            addr: std::sync::Arc::new(addr),
            inner: Proxy::new(protocol, "", 0),
        }
    }

    /// See [`Proxy::bypass`].
    ///
    /// # Errors
    ///
    /// [`Proxy::bypass`]'s.
    pub fn bypass<S: AsRef<str>>(
        mut self,
        patterns: impl IntoIterator<Item = S>,
    ) -> Result<Self, crate::UnsupportedBypass> {
        self.inner = self.inner.bypass(patterns)?;
        Ok(self)
    }

    /// See [`Proxy::bypass_local`].
    #[must_use]
    pub fn bypass_local(mut self) -> Self {
        self.inner = self.inner.bypass_local();
        self
    }

    /// See [`Proxy::only_for`].
    #[must_use]
    pub fn only_for(mut self, scheme: ProxyScheme) -> Self {
        self.inner = self.inner.only_for(scheme);
        self
    }

    /// The scheme this proxy is restricted to, if any.
    pub fn scheme(&self) -> Option<ProxyScheme> {
        self.inner.scheme()
    }

    /// Where the proxy listens.
    pub fn addr(&self) -> &hclient_rt::IpcAddr {
        &self.addr
    }

    /// The protocol, as configured.
    pub fn protocol(&self) -> &P {
        self.inner.protocol()
    }

    /// The same proxy speaking `f(protocol)`.
    #[must_use]
    pub fn map_protocol<Q>(self, f: impl FnOnce(P) -> Q) -> IpcProxy<Q> {
        IpcProxy {
            addr: self.addr,
            inner: self.inner.map_protocol(f),
        }
    }

    pub(crate) fn into_proxy(self) -> Proxy<P> {
        let mut inner = self.inner;
        inner.reach = Reach::Ipc(self.addr);
        inner
    }
}

/// A bypass pattern in this crate's dialect, lower-cased, or why it is
/// not one — the one statement of the dialect, shared by
/// [`Proxy::bypass`] and the system reader.
pub(crate) fn normalize_bypass(pattern: &str) -> Result<Box<str>, crate::BypassReason> {
    let p = pattern.to_ascii_lowercase();
    // Windows's and macOS's spelling of what this dialect writes
    // `.example.com`: that host and everything under it.
    let p = match p.strip_prefix("*.") {
        Some(rest) => format!(".{rest}"),
        None => p,
    };
    if p.contains('*') {
        return Err(crate::BypassReason::Wildcard);
    }
    let well_formed = match p.split_once('/') {
        Some((addr, len)) => subnet_is_well_formed(addr, len),
        None => host_pattern_is_well_formed(&p),
    };
    if well_formed {
        Ok(p.into_boxed_str())
    } else {
        Err(crate::BypassReason::Malformed)
    }
}

fn digits(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit())
}

fn subnet_is_well_formed(addr: &str, len: &str) -> bool {
    let Some(len) = digits(len).then(|| len.parse::<u32>().ok()).flatten() else {
        return false;
    };
    match parse_prefix(addr) {
        Some(std::net::IpAddr::V4(_)) => len <= 32,
        Some(std::net::IpAddr::V6(_)) => len <= 128,
        None => false,
    }
}

fn host_pattern_is_well_formed(p: &str) -> bool {
    let port_ok = |port: &str| digits(port) && port.parse::<u16>().is_ok();
    if p.is_empty()
        || p.bytes()
            .any(|b| b.is_ascii_whitespace() || b.is_ascii_control() || b"<>".contains(&b))
    {
        return false;
    }
    if let Some(rest) = p.strip_prefix('[') {
        return match rest.split_once(']') {
            Some((host, tail)) => {
                host.parse::<std::net::Ipv6Addr>().is_ok()
                    && (tail.is_empty() || tail.strip_prefix(':').is_some_and(port_ok))
            }
            None => false,
        };
    }
    // Two colons or more is a bare v6 literal, which carries no port.
    if p.matches(':').count() > 1 {
        return p.parse::<std::net::Ipv6Addr>().is_ok();
    }
    let host = match p.rsplit_once(':') {
        Some((host, port)) if port_ok(port) => host,
        Some(_) => return false,
        None => p,
    };
    let host = host.strip_prefix('.').unwrap_or(host);
    !host.is_empty() && !host.contains(['[', ']'])
}

/// One pattern against one origin. Separate from [`Proxy::serves`] so the
/// forms can be tested one at a time rather than through a list.
fn matches_bypass(pattern: &str, host: &str, port: u16) -> bool {
    let host = host.to_ascii_lowercase();
    // A subnet carries no port and is not a name, so it is answered
    // before the host/port split rather than inside it — `10.0.0.0/8`
    // would otherwise be read as the host `10.0.0.0/8`.
    if pattern.contains('/') {
        return matches_subnet(pattern, &host);
    }
    let (p_host, p_port) = split_pattern(pattern);
    match p_port {
        Some(want) => want == port && host_matches(p_host, &host),
        None => host_matches(p_host, &host),
    }
}

/// `10.0.0.0/8`, `169.254/16`, `fd00::/8` against a host.
///
/// The host must **be** an address; a name is never matched, for the
/// reason [`Proxy::bypass`] gives. A pattern that is not an address and a
/// prefix length matches nothing, which is the dialect's rule everywhere
/// else.
fn matches_subnet(pattern: &str, host: &str) -> bool {
    let Some((addr, len)) = pattern.split_once('/') else {
        return false;
    };
    let Ok(len) = len.parse::<u32>() else {
        return false;
    };
    // `[::1]` arrives wearing the brackets an authority gives it; a
    // pattern never does.
    let host = host.trim_start_matches('[').trim_end_matches(']');
    let Ok(host) = host.parse::<std::net::IpAddr>() else {
        return false;
    };
    match (parse_prefix(addr), host) {
        (Some(std::net::IpAddr::V4(net)), std::net::IpAddr::V4(h)) if len <= 32 => {
            same_prefix(&net.octets(), &h.octets(), len)
        }
        (Some(std::net::IpAddr::V6(net)), std::net::IpAddr::V6(h)) if len <= 128 => {
            same_prefix(&net.octets(), &h.octets(), len)
        }
        // A v4 pattern and a v6 host are different families and match
        // nothing — deliberately, including `::ffff:10.0.0.1`, because
        // reading a v4-mapped address as v4 here would make one pattern
        // mean two things.
        _ => false,
    }
}

/// An address, accepting the abbreviated v4 form a subnet is written in.
///
/// `169.254/16` is not an address by `IpAddr`'s grammar, and it is what
/// macOS ships in every default exceptions list, so the missing octets
/// are filled with zeroes — which is what the notation means.
fn parse_prefix(addr: &str) -> Option<std::net::IpAddr> {
    if let Ok(ip) = addr.parse::<std::net::IpAddr>() {
        return Some(ip);
    }
    if addr.contains(':') {
        return None;
    }
    let mut octets = [0u8; 4];
    let parts: Vec<&str> = addr.split('.').collect();
    // Only the abbreviated forms are this fallback's to read: a four-part
    // string `IpAddr` refused was refused for a reason, and more than four
    // would be truncated by `zip`. (With the octet guard below, four parts
    // cannot reach here; the bound says so rather than relying on it.)
    if parts.len() >= 4 {
        return None;
    }
    for (slot, part) in octets.iter_mut().zip(parts) {
        // Digits only, and no leading zero: `u8::from_str` also takes
        // `+10`, and `010` is ten to it and eight to `inet_aton`. The
        // strict parser refuses both, so this one does too.
        if !part.bytes().all(|b| b.is_ascii_digit()) || (part.len() > 1 && part.starts_with('0')) {
            return None;
        }
        *slot = part.parse().ok()?;
    }
    Some(std::net::IpAddr::from(octets))
}

/// Whether two addresses agree on their first `len` bits.
fn same_prefix(net: &[u8], host: &[u8], len: u32) -> bool {
    let whole = (len / 8) as usize;
    let bits = len % 8;
    if net[..whole] != host[..whole] {
        return false;
    }
    if bits == 0 {
        return true;
    }
    let mask = 0xFFu8 << (8 - bits);
    net[whole] & mask == host[whole] & mask
}

/// A pattern into its host and its optional port.
///
/// **An IPv6 literal is why this is not one `rsplit_once(':')`.** `::1`
/// splits into `("::", "1")`, and `1` parses as a port — so a bare v6
/// address would silently become "the host `::` at port 1", matching
/// nothing a caller meant. RFC 3986 §3.2.2's brackets are the
/// disambiguator and are required here for the same reason they are in an
/// authority: `[::1]:8080` binds a port, `::1` does not.
fn split_pattern(pattern: &str) -> (&str, Option<u16>) {
    if let Some(rest) = pattern.strip_prefix('[') {
        return match rest.split_once(']') {
            Some((h, "")) => (h, None),
            Some((h, tail)) => (h, tail.strip_prefix(':').and_then(|p| p.parse().ok())),
            // An unclosed bracket is in no accepted shape, so it matches
            // nothing rather than approximately something.
            None => (pattern, None),
        };
    }
    if pattern.matches(':').count() > 1 {
        return (pattern, None);
    }
    match pattern.rsplit_once(':') {
        Some((h, p)) => match p.parse::<u16>() {
            Ok(port) => (h, Some(port)),
            Err(_) => (pattern, None),
        },
        None => (pattern, None),
    }
}

fn host_matches(pattern: &str, host: &str) -> bool {
    match pattern.strip_prefix('.') {
        // `.example.com` is the domain and everything under it. The
        // leading dot is not part of the name, so `example.com` itself
        // matches — which is what a reader expects and what most
        // `NO_PROXY` implementations do.
        Some(domain) => host == domain || host.ends_with(pattern),
        None => host == pattern,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::BypassReason;

    /// Why `pat` is refused — it must be.
    fn refused(pat: &str) -> BypassReason {
        let err = Proxy::new(Socks5::new(), "px", 1080)
            .bypass([pat])
            .expect_err("a pattern in no accepted shape");
        assert_eq!(
            err.pattern(),
            pat,
            "the refusal names the pattern as written"
        );
        err.reason()
    }
    use crate::{Approach, Handshake, HttpConnect, Socks5};

    #[test]
    fn only_an_http_proxy_treats_the_two_schemes_differently() {
        let http = HttpConnect::new();
        assert_eq!(http.approach(true), Approach::Tunnel);
        assert_eq!(http.approach(false), Approach::Absolute);

        let socks = Socks5::new();
        assert_eq!(socks.approach(true), Approach::Tunnel);
        assert_eq!(socks.approach(false), Approach::Tunnel);

        // **`Socks4` too, and it was the one nobody asked.** Both SOCKS
        // protocols are byte tunnels with no idea that HTTP exists, so
        // neither has an absolute-form question to answer — and only
        // `Socks5` was pinned: making `Socks4::approach` answer
        // `Absolute` left the whole suite at 152 passing. A transport
        // that believed it could write an absolute-form request line
        // into a SOCKS4 tunnel would send it to the origin, which never
        // agreed to act as a proxy for itself.
        let socks4 = crate::Socks4::new();
        assert_eq!(socks4.approach(true), Approach::Tunnel);
        assert_eq!(socks4.approach(false), Approach::Tunnel);
    }

    /// The accepted forms match what they say and nothing beside it — a
    /// bypass list that matched approximately would send a request direct
    /// that the caller asked to be proxied.
    #[test]
    fn the_bypass_forms_match_what_they_say_and_nothing_beside_it() {
        let p = |pat: &str| {
            Proxy::new(Socks5::new(), "px", 1080)
                .bypass([pat])
                .expect("an accepted pattern")
        };

        // Exact host, at any port, case-insensitively.
        assert!(!p("example.com").serves(true, "example.com", 443));
        assert!(!p("example.com").serves(true, "EXAMPLE.COM", 8080));
        assert!(p("example.com").serves(true, "api.example.com", 443));
        assert!(p("example.com").serves(true, "notexample.com", 443));

        // Domain and everything under it.
        assert!(!p(".example.com").serves(true, "example.com", 443));
        assert!(!p(".example.com").serves(true, "api.example.com", 443));
        assert!(!p(".example.com").serves(true, "a.b.example.com", 443));
        assert!(p(".example.com").serves(true, "notexample.com", 443));

        // Host at one port alone.
        assert!(!p("example.com:8080").serves(true, "example.com", 8080));
        assert!(p("example.com:8080").serves(true, "example.com", 443));

        // An address literal is just a host, and a v6 one arrives here
        // wearing the brackets RFC 3986 gives the authority.
        assert!(!p("127.0.0.1").serves(true, "127.0.0.1", 80));
        assert!(p("127.0.0.1").serves(true, "127.0.0.2", 80));
        assert!(!p("::1").serves(true, "[::1]", 80));
        assert!(!p("::1").serves(true, "::1", 1), "`::1` binds no port");
        assert!(!p("[::1]:8080").serves(true, "[::1]", 8080));
        assert!(p("[::1]:8080").serves(true, "[::1]", 80));

        // The one wildcard every platform writes is read as the domain
        // form; any other is refused rather than kept as a pattern that
        // never matches.
        assert!(!p("*.example.com").serves(true, "api.example.com", 80));
        assert!(!p("*.example.com").serves(true, "example.com", 80));
        assert_eq!(refused("192.168.1.*"), BypassReason::Wildcard);
        assert_eq!(refused("*.*.example.com"), BypassReason::Wildcard);
    }

    /// Empty by default, which is the decision rather than an oversight:
    /// excluding loopback for a caller who asked to proxy everything
    /// would change what goes on the wire without being asked.
    #[test]
    fn nothing_is_bypassed_until_a_caller_says_so() {
        let p = Proxy::new(Socks5::new(), "px", 1080);
        assert!(p.serves(true, "127.0.0.1", 80));
        assert!(p.serves(true, "localhost", 80));
    }

    #[test]
    fn a_subnet_matches_an_address_in_it_and_nothing_else() {
        let p = |pat: &str| {
            Proxy::new(Socks5::new(), "px", 1080)
                .bypass([pat])
                .expect("an accepted pattern")
        };

        assert!(!p("10.0.0.0/8").serves(true, "10.1.2.3", 80));
        assert!(p("10.0.0.0/8").serves(true, "11.1.2.3", 80));
        // The abbreviated form, which is what macOS ships in the default
        // exceptions list of every Mac.
        assert!(!p("169.254/16").serves(true, "169.254.1.1", 80));
        assert!(p("169.254/16").serves(true, "169.255.1.1", 80));
        // A prefix that does not fall on a byte boundary.
        assert!(!p("192.168.4.0/22").serves(true, "192.168.7.9", 80));
        assert!(p("192.168.4.0/22").serves(true, "192.168.8.1", 80));
        // v6, with and without the brackets an authority gives a host.
        assert!(!p("fd00::/8").serves(true, "fd12::1", 80));
        assert!(!p("fd00::/8").serves(true, "[fd12::1]", 80));
        assert!(p("fd00::/8").serves(true, "fe00::1", 80));
        // `/0` is everything of that family, and nothing of the other.
        assert!(!p("0.0.0.0/0").serves(true, "8.8.8.8", 80));
        assert!(p("0.0.0.0/0").serves(true, "::1", 80));
    }

    #[test]
    fn a_prefix_longer_than_its_family_matches_nothing_rather_than_panicking() {
        // **The `len <= 32` and `len <= 128` guards, which stand between
        // a pattern and a panic**, and which nothing asked: both forced
        // to `true` left the suite at 134 passing.
        //
        // The existing `10.0.0.0/33` row cannot see them, because its
        // host `10.0.0.1` differs from the network in the fourth octet,
        // so `same_prefix`'s whole-bytes comparison answers `false`
        // before the partial byte is reached. The host here **agrees on
        // every octet**, which is the only input that gets as far as the
        // out-of-range read.
        let p = |pat: &str| {
            Proxy::new(Socks5::new(), "px", 1080)
                .bypass([pat])
                .expect("an accepted pattern")
        };

        // **One bit past the family and a whole byte past it, because
        // they are two different panics** — measured on `same_prefix`
        // directly: `/33` reads `net[4]` and panics *index out of
        // bounds*, where `/40` takes `net[..5]` and panics *range end
        // index 5 out of range*. Either row alone kills the mutation,
        // so neither is kept for the kill; they are kept because a
        // guard that came back for only one of the two shapes would
        // leave the other reachable.
        assert!(!matches_bypass("10.0.0.0/33", "10.0.0.0", 80));
        assert_eq!(refused("10.0.0.0/33"), BypassReason::Malformed);
        assert!(!matches_bypass("10.0.0.0/40", "10.0.0.0", 80));
        assert_eq!(refused("10.0.0.0/40"), BypassReason::Malformed);
        // v6, the same pair, whose guard is a separate match arm.
        assert!(!matches_bypass("fd00::/129", "fd00::", 80));
        assert_eq!(refused("fd00::/129"), BypassReason::Malformed);
        assert!(!matches_bypass("fd00::/136", "fd00::", 80));
        assert_eq!(refused("fd00::/136"), BypassReason::Malformed);
        // The control, at the widest length each family really has: an
        // exact address still matches, so the guards refuse what is out
        // of range and nothing else.
        assert!(!p("10.0.0.0/32").serves(true, "10.0.0.0", 80));
        assert!(!p("fd00::/128").serves(true, "fd00::", 80));
    }

    #[test]
    fn a_prefix_with_more_octets_than_an_address_matches_nothing() {
        // `parse_prefix` fills the missing octets of an abbreviated form
        // like `169.254`, and it has to refuse the other direction — a
        // pattern with **more** than four labels, which `zip` would
        // otherwise truncate to the first four and honour as if the rest
        // had not been written.
        //
        // Measured: `parts.len() > 4` weakened to `== 4` left the suite
        // at 134 passing, and turns `1.2.3.4.5/8` into the subnet
        // `1.2.3.4/8` — a pattern that matches a network nobody wrote
        // down. (`>= 4` is **equivalent** rather than unkilled, and this
        // note said so once before it was true: `u8::from_str` accepted
        // `010` and `+10`, which `IpAddr` refuses, so a four-label string
        // could survive the octet parse without being an address. The
        // octet parse takes digits without a leading zero now, and with
        // that every four-label survivor already returned above — the
        // leading-zero test below is the one that pins the guard.)
        let p = |pat: &str| {
            Proxy::new(Socks5::new(), "px", 1080)
                .bypass([pat])
                .expect("an accepted pattern")
        };
        // The host the truncation would produce, and a host inside the
        // `/8` it would produce. Either alone kills the mutation; both
        // are here because the refusal is *total* — a pattern in no
        // accepted shape matches nothing, rather than matching the
        // prefix that happens to be left after the labels it dropped.
        assert!(!matches_bypass("1.2.3.4.5/8", "1.2.3.4", 80));
        assert_eq!(refused("1.2.3.4.5/8"), BypassReason::Malformed);
        assert!(!matches_bypass("1.2.3.4.5/8", "1.0.0.1", 80));
        assert_eq!(refused("1.2.3.4.5/8"), BypassReason::Malformed);
        // The control, one label shorter, which is a subnet and does
        // match — so the refusal above is about the count and not about
        // the pattern being odd.
        assert!(!p("1.2.3.4/8").serves(true, "1.0.0.1", 80));
    }

    #[test]
    fn a_prefix_with_a_leading_zero_matches_nothing() {
        // `010` is decimal ten to `u8::from_str` and octal eight to
        // `inet_aton`, which is what the platform that wrote the pattern
        // may have meant. `IpAddr` refuses it for that reason, and the
        // abbreviated-form fallback must not quietly accept what the
        // strict parser refused: a pattern in no accepted shape matches
        // nothing.
        let p = |pat: &str| {
            Proxy::new(Socks5::new(), "px", 1080)
                .bypass([pat])
                .expect("an accepted pattern")
        };
        for pat in [
            "010.0.0.0/8",
            "010/8",
            "169.0254/16",
            "+10.0.0.0/8",
            "+10/8",
        ] {
            for host in ["10.1.2.3", "8.1.2.3", "169.254.1.1", "169.172.1.1"] {
                assert!(!matches_bypass(pat, host, 80), "{pat} bypassed {host}");
            }
            assert_eq!(refused(pat), BypassReason::Malformed, "{pat}");
        }
        // The controls: the same networks written without the zero.
        assert!(!p("10.0.0.0/8").serves(true, "10.1.2.3", 80));
        assert!(!p("10/8").serves(true, "10.1.2.3", 80));
        assert!(!p("169.254/16").serves(true, "169.254.1.1", 80));
        // `0` alone is a zero, not a leading one.
        assert!(!p("0.0.0.0/0").serves(true, "10.1.2.3", 80));
        assert!(!p("10.0/16").serves(true, "10.0.1.1", 80));
    }

    #[test]
    fn a_host_pattern_in_no_accepted_shape_is_refused_naming_it() {
        for pat in [
            "",
            ".",
            "example.com:http",
            "example.com:70000",
            "[::1",
            "[::1]:",
            "[example.com]",
            "::not-v6",
            "<local>",
            "exa mple.com",
        ] {
            assert_eq!(refused(pat), BypassReason::Malformed, "{pat:?}");
        }
        // Every refusal is of the whole call: nothing before the bad
        // pattern is kept either.
        assert!(
            Proxy::new(Socks5::new(), "px", 1080)
                .bypass(["example.com", "bad:port"])
                .is_err()
        );
        // And a same-machine proxy refuses by the same rule.
        let ipc = IpcProxy::new(Socks5::new(), hclient_rt::IpcAddr::Unix("/s".into()));
        assert_eq!(
            ipc.bypass(["*.x.*"]).expect_err("a wildcard").reason(),
            BypassReason::Wildcard
        );
    }

    #[test]
    fn a_subnet_never_matches_a_name() {
        // Matching would mean resolving the host to decide whether to
        // proxy it — an extra lookup, and on a proxied request the DNS
        // leak a proxy user is often there to avoid.
        let p = Proxy::new(Socks5::new(), "px", 1080)
            .bypass(["10.0.0.0/8"])
            .unwrap();
        assert!(p.serves(true, "internal.example.com", 80));
    }

    #[test]
    fn a_pattern_that_is_not_a_subnet_but_has_a_slash_is_refused() {
        // Refused at the setter, and — the matcher's own guard, kept for a
        // pattern that reached it some other way — matching nothing, since
        // approximately something is worse than nothing.
        assert!(!matches_bypass("example.com/8", "example.com", 80));
        assert_eq!(refused("example.com/8"), BypassReason::Malformed);
        assert!(!matches_bypass("10.0.0.0/many", "10.0.0.1", 80));
        assert_eq!(refused("10.0.0.0/many"), BypassReason::Malformed);
        assert!(!matches_bypass("10.0.0.0/33", "10.0.0.1", 80));
        assert_eq!(refused("10.0.0.0/33"), BypassReason::Malformed);
        assert!(!matches_bypass("/8", "10.0.0.1", 80));
        assert_eq!(refused("/8"), BypassReason::Malformed);
    }

    #[test]
    fn the_local_rule_is_about_dots_and_nothing_else() {
        let p = Proxy::new(Socks5::new(), "px", 1080).bypass_local();
        assert!(!p.serves(true, "intranet", 80));
        assert!(!p.serves(true, "localhost", 80));
        // Has dots, so it is not local by this rule — which is the
        // platforms' own reading and the surprising half.
        assert!(p.serves(true, "10.0.0.5", 80));
        assert!(p.serves(true, "example.com", 80));
    }

    #[test]
    fn a_scheme_restriction_and_a_bypass_fail_differently() {
        // Both answer `false` here, and the caller collapses them only
        // because a list that runs out is itself "go direct".
        let only_https = Proxy::new(Socks5::new(), "px", 1080).only_for(ProxyScheme::Https);
        assert!(!only_https.serves(false, "example.com", 80));
        assert!(only_https.serves(true, "example.com", 443));
    }

    #[test]
    fn a_same_machine_proxy_reports_its_scheme_restriction_too() {
        let addr = || hclient_rt::IpcAddr::Unix("/s".into());
        assert_eq!(IpcProxy::new(Socks5::new(), addr()).scheme(), None);
        assert_eq!(
            IpcProxy::new(Socks5::new(), addr())
                .only_for(ProxyScheme::Https)
                .scheme(),
            Some(ProxyScheme::Https)
        );
    }

    #[test]
    fn the_scheme_accessor_reports_the_restriction_that_was_set() {
        // `scheme()` is the only way a caller — or a translator
        // comparing two installations — can read back what `only_for`
        // set, and nothing asserted it: replacing the whole body with
        // `None` left the suite at 134 passing. The one existing reader
        // is `an_ordinary_machine_installs_the_same_list_either_way`,
        // which compares the strict and lenient paths against **each
        // other**, so a `scheme()` that always answered `None` agreed
        // with itself on both sides.
        let unrestricted = Proxy::new(Socks5::new(), "px", 1080);
        assert_eq!(unrestricted.scheme(), None);

        for want in [ProxyScheme::Http, ProxyScheme::Https] {
            let p = Proxy::new(Socks5::new(), "px", 1080).only_for(want);
            assert_eq!(p.scheme(), Some(want));
        }
    }

    #[test]
    fn the_host_and_port_are_the_proxys_and_not_the_origins() {
        let p = Proxy::new(Socks5::new(), "px", 1080);
        assert_eq!((p.host(), p.port()), ("px", 1080));
    }
}
