//! Which proxy serves which request — the part that is a rule rather
//! than a protocol.
//!
//! Everything here is a pure function of a host, a port and a scheme.
//! Nothing in this file knows what a socket is, which is why the whole of
//! it is testable with `assert!`.

use crate::{Approach, Handshake, Step};
use bytes::{Bytes, BytesMut};
use hclient_core::Error;

/// No proxy — and it is an **empty enum**, so `Proxy<NoProxy>` cannot be
/// constructed and the `Option` holding one is `None` by construction
/// rather than by discipline. A unit struct with `unreachable!()` bodies
/// would be a value that exists only to be absent, which is the shape
/// this workspace deleted `UpgradeSupport`'s spare variants for.
#[derive(Debug, Clone, Copy)]
pub enum NoProxy {}

impl Handshake for NoProxy {
    fn approach(&self, _: bool) -> Approach {
        match *self {}
    }

    fn begin(&mut self, _: &str, _: u16) -> Result<Bytes, Error> {
        match *self {}
    }

    fn advance(&mut self, _: &mut BytesMut) -> Result<Step, Error> {
        match *self {}
    }
}

/// Which request scheme a proxy serves, for a caller who has more than
/// one.
///
/// The distinction that motivates it is the ordinary corporate one — an
/// `HTTP_PROXY` and an `HTTPS_PROXY` pointing at different hosts — not two
/// different proxy *protocols*: a transport has one `P`, so every proxy on
/// one transport speaks the same one. That is a real limit and it is
/// stated rather than worked around, because erasing `P` to lift it would
/// erase the IO with it, which is the objection this crate's own root doc
/// records against `Box<dyn Handshake>`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProxyScheme {
    Http,
    Https,
}

/// Where a proxy lives, and which protocol it speaks.
#[derive(Debug, Clone)]
pub struct Proxy<P> {
    protocol: P,
    host: Box<str>,
    port: u16,
    bypass: Vec<Box<str>>,
    /// The `<local>` rule — see [`Proxy::bypass_local`]. A flag rather
    /// than a pattern because it is a rule about the shape of a name.
    bypass_local: bool,
    /// `None` — the default — means both schemes, which is what a caller
    /// with one proxy wants and what `Proxy::new` gives them.
    only: Option<ProxyScheme>,
}

impl<P> Proxy<P> {
    pub fn new(protocol: P, host: impl Into<Box<str>>, port: u16) -> Self {
        Self {
            protocol,
            host: host.into(),
            port,
            bypass: Vec::new(),
            bypass_local: false,
            only: None,
        }
    }

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
    /// That is still the rule, and [`crate::system`] is not an exception
    /// to it: it reads the environment and the platform's own settings
    /// *because a caller called it*, which is the transport's builder
    /// exercising exactly the policy this paragraph reserves for them.
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
    /// No wildcard. A pattern in no accepted shape matches nothing rather
    /// than approximately something.
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
    #[must_use]
    pub fn bypass<S: Into<Box<str>>>(mut self, patterns: impl IntoIterator<Item = S>) -> Self {
        self.bypass.extend(
            patterns
                .into_iter()
                .map(|p| p.into().to_ascii_lowercase().into_boxed_str()),
        );
        self
    }

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
    /// in it can say "any host with no dot". Widening the dialect to fit
    /// would have made every other pattern harder to read, for one rule
    /// that is a boolean everywhere it comes from.
    ///
    /// It is here because [`crate::system`] meets it constantly rather
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
    pub fn serves(&self, use_tls: bool, host: &str, port: u16) -> bool {
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

    /// The first proxy in `list` that serves this request, or `None` for
    /// direct.
    ///
    /// First-match-wins rather than most-specific-wins: a precedence rule
    /// would have to be learned, where an ordered list is read off the
    /// builder chain that wrote it. `NO_PROXY` implementations that invent
    /// a precedence are exactly what [`bypass`](Self::bypass)'s doc
    /// refuses to imitate.
    pub fn choose<'a>(
        list: &'a [Proxy<P>],
        use_tls: bool,
        host: &str,
        port: u16,
    ) -> Option<&'a Proxy<P>> {
        list.iter().find(|p| p.serves(use_tls, host, port))
    }

    pub fn protocol(&self) -> &P {
        &self.protocol
    }

    /// The protocol, to be driven. A handshake is a state machine and
    /// running one mutates it, so a transport takes it by value — two
    /// connections through one proxy are two handshakes.
    pub fn protocol_mut(&mut self) -> &mut P {
        &mut self.protocol
    }

    pub fn host(&self) -> &str {
        &self.host
    }

    pub fn port(&self) -> u16 {
        self.port
    }

    /// The pool-key component. Two proxies to one origin are two
    /// connections, and a tunnel reused through a *different* proxy would
    /// be a security defect rather than a redundancy — the same argument
    /// a pool key's TLS-identity field is already kept for.
    pub fn key(&self) -> String {
        format!("{}:{}", self.host, self.port)
    }
}

impl<P: Clone> Proxy<P> {
    /// A fresh handshake for one connection.
    ///
    /// `Clone` rather than `&mut`: the configured protocol is a template —
    /// credentials and options — and every connection needs its own state
    /// machine. Cloning one is cloning two `Box<str>`s at most.
    pub fn handshake(&self) -> P {
        self.protocol.clone()
    }
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
    if parts.len() > 4 {
        return None;
    }
    for (slot, part) in octets.iter_mut().zip(parts) {
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
    use crate::{HttpConnect, Socks5};

    #[test]
    fn only_an_http_proxy_treats_the_two_schemes_differently() {
        let http = HttpConnect::new();
        assert_eq!(http.approach(true), Approach::Tunnel);
        assert_eq!(http.approach(false), Approach::Absolute);

        let socks = Socks5::new();
        assert_eq!(socks.approach(true), Approach::Tunnel);
        assert_eq!(socks.approach(false), Approach::Tunnel);
    }

    /// The accepted forms match what they say and nothing beside it — a
    /// bypass list that matched approximately would send a request direct
    /// that the caller asked to be proxied.
    #[test]
    fn the_bypass_forms_match_what_they_say_and_nothing_beside_it() {
        let p = |pat: &str| Proxy::new(Socks5::new(), "px", 1080).bypass([pat]);

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

        // No wildcard: a pattern in no accepted shape matches nothing
        // rather than approximately something.
        assert!(p("*.example.com").serves(true, "api.example.com", 80));
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
        let p = |pat: &str| Proxy::new(Socks5::new(), "px", 1080).bypass([pat]);

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
    fn a_subnet_never_matches_a_name() {
        // Matching would mean resolving the host to decide whether to
        // proxy it — an extra lookup, and on a proxied request the DNS
        // leak a proxy user is often there to avoid.
        let p = Proxy::new(Socks5::new(), "px", 1080).bypass(["10.0.0.0/8"]);
        assert!(p.serves(true, "internal.example.com", 80));
    }

    #[test]
    fn a_pattern_that_is_not_a_subnet_but_has_a_slash_matches_nothing() {
        // The dialect's rule everywhere else, kept here: approximately
        // something is worse than nothing.
        let p = |pat: &str| Proxy::new(Socks5::new(), "px", 1080).bypass([pat]);
        assert!(p("example.com/8").serves(true, "example.com", 80));
        assert!(p("10.0.0.0/many").serves(true, "10.0.0.1", 80));
        assert!(p("10.0.0.0/33").serves(true, "10.0.0.1", 80));
        assert!(p("/8").serves(true, "10.0.0.1", 80));
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
    fn choose_takes_the_first_entry_that_serves() {
        let list = vec![
            Proxy::new(Socks5::new(), "specific", 1080).only_for(ProxyScheme::Https),
            Proxy::new(Socks5::new(), "catch-all", 1080),
        ];
        assert_eq!(
            Proxy::choose(&list, true, "example.com", 443)
                .unwrap()
                .host(),
            "specific"
        );
        assert_eq!(
            Proxy::choose(&list, false, "example.com", 80)
                .unwrap()
                .host(),
            "catch-all"
        );

        // A bypass on the first entry falls through to the next, which is
        // the rule that makes a per-proxy list right and a global one
        // wrong.
        let list = vec![
            Proxy::new(Socks5::new(), "first", 1080).bypass(["example.com"]),
            Proxy::new(Socks5::new(), "second", 1080),
        ];
        assert_eq!(
            Proxy::choose(&list, true, "example.com", 443)
                .unwrap()
                .host(),
            "second"
        );
    }

    #[test]
    fn the_pool_key_names_the_proxy_and_not_the_origin() {
        let p = Proxy::new(Socks5::new(), "px", 1080);
        assert_eq!(p.key(), "px:1080");
    }
}
