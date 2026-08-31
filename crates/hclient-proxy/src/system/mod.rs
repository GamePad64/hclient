//! The operating system's proxy settings, read once and handed over as
//! **data**.
//!
//! ```no_run
//! let sys = hclient_proxy::system::SystemProxies::detect();
//! for entry in sys.entries() {
//!     println!("{:?} {}:{}", entry.kind(), entry.host(), entry.port());
//! }
//! ```
//!
//! # Reading and applying are different jobs
//!
//! What comes out of here is a list of hosts and a bypass list — no
//! socket, no connector, nothing from any transport — so a transport this
//! workspace never wrote can read the same settings the one it did wrote
//! reads. [`http_proxies`] is the translation into this crate's own
//! [`Proxy`](crate::Proxy) values, and it is the only part that knows
//! what a proxy protocol is.
//!
//! # What this module does not do
//!
//! **It does not execute a PAC script, and it cannot see that one is
//! configured.** A machine whose proxy is a `.pac` URL or WPAD
//! auto-discovery reports *no proxies* here, and requests then go direct
//! — which is what curl and reqwest also do, and is stated because a
//! silent direct connection is the one failure a caller cannot diagnose
//! from the outside. On an Apple platform `hclient-urlsession` is the
//! answer: `URLSession` runs the script itself, in the OS, which is one
//! of the three reasons that backend exists — and it reports that it is
//! doing so, through [`SystemProxies::detect_platform`] and
//! [`SystemProxies::names_a_proxy`], which are here for that caller.
//!
//! **It does not decide anything.** No pattern is matched here, no
//! request is routed here, and nothing is applied. A caller who wants the
//! settings ignored simply does not call [`SystemProxies::detect`] — the
//! environment is read because somebody asked for it to be read, which is
//! the line [`Proxy::bypass`](crate::Proxy::bypass) draws when it refuses
//! to read `NO_PROXY` on its own.

use std::fmt;

#[cfg(target_os = "android")]
mod jvm;
mod parse;
mod read;
mod translate;

pub use crate::error::{ParseError, SystemProxyRefused};
pub use translate::{http_proxies, http_proxies_lossy};

/// Which protocol a proxy speaks.
///
/// Not the scheme of the *request* — see [`Scheme`] for that. A SOCKS5
/// proxy carries `https://` requests perfectly well.
///
/// **Deliberately not `#[non_exhaustive]`**, unlike every error type in
/// this crate, and the discriminator is who stands on the other side:
/// this enum crosses a seam into a **translator** — `hclient-native`
/// matches it to pick a `ProxyProtocol` — where a `_` arm would be a
/// *mapping* rather than an *unknown*, and a variant added later would
/// quietly acquire whichever protocol the wildcard happened to name.
/// A compile error in every translator is the point. Same rule, same
/// reason as `SvcbRecordError` in `hclient-dns`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ProxyKind {
    /// An HTTP proxy: `CONNECT` for a tunnel, absolute-form for plain
    /// `http://`.
    Http,
    /// RFC 1928.
    Socks5,
    /// SOCKS4, and 4a for a hostname.
    ///
    /// **A bare `socks=host:port` on Windows, and a bare `socks://` URL,
    /// both land here rather than on [`Socks5`](Self::Socks5)**, which is
    /// a decision about somebody else's ambiguity: neither spelling
    /// carries a version, and curl and Chrome both read the unversioned
    /// form as SOCKS4. Reading it as SOCKS5 instead would make this crate
    /// the only implementation on the machine that does.
    Socks4,
}

/// Which request scheme an entry serves.
///
/// `None` — the field's own `Option` rather than a variant here — is an
/// entry that serves both, which is what an unqualified `ProxyServer` on
/// Windows and an `ALL_PROXY` in the environment both mean.
///
/// Not `#[non_exhaustive]`, for [`ProxyKind`]'s reason: it is translated
/// rather than merely read.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Scheme {
    Http,
    Https,
}

/// A username and password carried in a proxy URL's userinfo.
///
/// Percent-decoded, because that is how they are written when the
/// password contains a `@` or a `:` and how curl reads them back.
#[derive(Clone, PartialEq, Eq)]
pub struct Credentials {
    user: Box<str>,
    password: Box<str>,
}

impl Credentials {
    pub fn user(&self) -> &str {
        &self.user
    }

    pub fn password(&self) -> &str {
        &self.password
    }
}

/// Never prints the password. A proxy password reaching a log through a
/// `{:?}` is the same defect `hclient`'s `basic_auth` marks its header
/// sensitive against, and a derived `Debug` here would be the hole that
/// marking closes one layer up.
impl fmt::Debug for Credentials {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Credentials")
            .field("user", &self.user)
            .field("password", &"<redacted>")
            .finish()
    }
}

/// One proxy the system named.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProxyEntry {
    kind: ProxyKind,
    host: Box<str>,
    port: u16,
    applies_to: Option<Scheme>,
    credentials: Option<Credentials>,
}

impl ProxyEntry {
    pub fn kind(&self) -> ProxyKind {
        self.kind
    }

    pub fn host(&self) -> &str {
        &self.host
    }

    pub fn port(&self) -> u16 {
        self.port
    }

    /// The request scheme this entry serves, or `None` for both.
    pub fn applies_to(&self) -> Option<Scheme> {
        self.applies_to
    }

    pub fn credentials(&self) -> Option<&Credentials> {
        self.credentials.as_ref()
    }
}

/// A bypass pattern the system named that this workspace's matcher cannot
/// express.
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
    pub fn pattern(&self) -> &str {
        &self.pattern
    }

    pub fn reason(&self) -> BypassReason {
        self.reason
    }
}

impl fmt::Display for UnsupportedBypass {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "`{}` ({})", self.pattern, self.reason)
    }
}

/// Why a bypass pattern could not be translated.
///
/// One variant, and it stays an enum rather than becoming a unit struct:
/// `Cidr` was the second until subnets became statable, and the shape
/// that admitted a second reason is the shape that will admit the next.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum BypassReason {
    /// `192.168.1.*`, `10.*.*.*` — a wildcard anywhere but as a leading
    /// `*.` label.
    Wildcard,
}

impl fmt::Display for BypassReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Wildcard => f.write_str("a wildcard that is not a leading `*.`"),
        }
    }
}

/// What the system says about proxies.
///
/// Built by [`detect`](Self::detect), and by nothing else — there is no
/// constructor taking made-up values, because every field here is an
/// answer from the machine and a hand-built one would be a claim about a
/// machine nobody asked.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SystemProxies {
    entries: Vec<ProxyEntry>,
    bypass: Vec<Box<str>>,
    bypass_local: bool,
    bypass_everything: bool,
    unsupported_bypass: Vec<UnsupportedBypass>,
    ignored: Vec<Box<str>>,
    pac: Option<Box<str>>,
}

impl SystemProxies {
    /// Read the machine's proxy configuration.
    ///
    /// # Where it looks, and in what order
    ///
    /// The environment first — `HTTP_PROXY`, `HTTPS_PROXY`, `ALL_PROXY`,
    /// `NO_PROXY`, in either case — and the platform's own settings only
    /// where the environment named none. That order is not ours to
    /// choose: it is what curl, reqwest and every other client on the
    /// machine does, and an `HTTPS_PROXY` that this client alone ignored
    /// would be worse than one it alone honoured.
    ///
    /// The platform half is `WinHttpGetIEProxyConfigForCurrentUser` on
    /// Windows and `SCDynamicStoreCopyProxies` on macOS. On every other
    /// target the environment is the whole of it, which is the same
    /// answer curl gives on those targets.
    ///
    /// # It reads, it does not decide, and it cannot fail
    ///
    /// An empty result is an ordinary answer: most machines have no
    /// proxy. There is deliberately no error — a registry that will not
    /// open and a machine with nothing configured are indistinguishable
    /// from here, and a `Result` would have offered a distinction its
    /// caller could not act on.
    pub fn detect() -> Self {
        Self::from_raw(read::read())
    }

    /// The **platform's own** settings, with the environment left out.
    ///
    /// [`detect`](Self::detect) reads the environment first and falls
    /// through to the platform only where it named nothing, because that
    /// is what every other client on the machine does. This does the
    /// opposite of that courtesy on purpose, and it exists for exactly
    /// one kind of caller: a transport that **is** the operating system's
    /// own stack, and therefore honours the operating system's own
    /// settings and not a variable in this process's environment.
    ///
    /// `hclient-urlsession` is that caller. `URLSession` takes its
    /// proxies from the system configuration; nothing in Apple's
    /// documentation or in this workspace's reading says it consults
    /// `HTTP_PROXY`, and a `Capabilities::proxy` computed from
    /// [`detect`](Self::detect) would therefore report `true` on a
    /// machine whose only proxy is an environment variable that transport
    /// ignores. That is a capability that lies, which is the one failure
    /// this workspace treats as worse than a missing feature.
    ///
    /// **It is the understating answer of the two, in both readings.** If
    /// `URLSession` honours only the system configuration this is exact;
    /// if it also honoured the environment, this would miss a proxy
    /// rather than invent one. The environment-first order can only ever
    /// over-claim here, and this can only ever under-claim.
    ///
    /// On a target with no platform store — anything but Windows and
    /// Apple — this is always empty, because the environment *was* the
    /// whole of the answer there and it is the half deliberately not
    /// read.
    pub fn detect_platform() -> Self {
        Self::from_raw(read::platform())
    }

    fn from_raw(raw: read::Raw) -> Self {
        let mut out = Self::from_parts(raw.proxies, raw.bypass, raw.exclude_simple);
        out.pac = raw.pac.map(String::into_boxed_str);
        out
    }

    /// The proxies, **most specific first**.
    ///
    /// The order is this crate's and it is load-bearing rather than
    /// cosmetic: `hclient-native` takes the first entry that serves a
    /// request, so an unqualified proxy placed ahead of a scheme-specific
    /// one would shadow it. The system hands these over as an unordered
    /// map, so somebody has to impose an order, and *scheme-specific
    /// before catch-all* is the only one that preserves what the
    /// settings meant.
    pub fn entries(&self) -> &[ProxyEntry] {
        &self.entries
    }

    /// Hosts that go direct, in `hclient-native`'s own bypass dialect.
    pub fn bypass(&self) -> &[Box<str>] {
        &self.bypass
    }

    /// Whether a host with no dot in it goes direct.
    ///
    /// Windows spells this `<local>` in `ProxyOverride` and macOS spells
    /// it *Exclude simple hostnames*; they are the same rule and it is
    /// **on by default on macOS**, so a translation that could not
    /// express it would be wrong on most Macs rather than in a corner.
    pub fn bypass_local(&self) -> bool {
        self.bypass_local
    }

    /// Whether the configuration says *bypass everything* — a bare `*`.
    ///
    /// It is an answer rather than a pattern, and the honest reading of
    /// it is that this machine wants no proxy at all, so a caller should
    /// install none.
    pub fn bypass_everything(&self) -> bool {
        self.bypass_everything
    }

    /// Bypass patterns that could not be translated — see
    /// [`UnsupportedBypass`].
    pub fn unsupported_bypass(&self) -> &[UnsupportedBypass] {
        &self.unsupported_bypass
    }

    /// Settings that were read and are not for an HTTP client — an `ftp`
    /// proxy, a `gopher` one — named rather than dropped in silence.
    ///
    /// Nothing needs to act on these. They are here so that "this crate
    /// discarded nothing you cannot see" is a checkable statement rather
    /// than a promise.
    pub fn ignored(&self) -> &[Box<str>] {
        &self.ignored
    }

    /// The URL of a proxy auto-config script, where the machine names
    /// one.
    ///
    /// **Nothing here runs it**, and that is why it is reported rather
    /// than acted on: a `.pac` file is a JavaScript program whose
    /// `FindProxyForURL` decides per request, which needs an interpreter
    /// this client has no business carrying. What the value is for is
    /// [`http_proxies`], which refuses rather than going direct — because
    /// a machine that answers *ask the script* has not answered *no
    /// proxy*, and silently reading it as one is the failure a caller
    /// cannot diagnose from the outside.
    ///
    /// On an Apple platform `hclient-urlsession` is the way to honour it:
    /// `URLSession` runs the script itself, in the OS, which is one of
    /// the three reasons that backend exists.
    pub fn pac(&self) -> Option<&str> {
        self.pac.as_deref()
    }

    /// No proxy at all, however that came about — nothing configured, or
    /// a `*` bypass that takes everything direct.
    ///
    /// **A PAC script alone reads as empty here**, and that is right for
    /// what this answers: *is there anything in these settings that this
    /// client can install?* — and a script is not, because nothing here
    /// runs one. It is the wrong question for *is this machine proxied*,
    /// which is [`names_a_proxy`](Self::names_a_proxy), and the two
    /// separate on exactly that case.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty() || self.bypass_everything
    }

    /// Whether these settings put a proxy in front of anything at all.
    ///
    /// This is the question a **capability report** asks, and it is not
    /// [`is_empty`](Self::is_empty) negated. `is_empty` answers *is there
    /// something here this client can install*; this answers *does the
    /// machine route through a proxy*, which is what a caller wants to
    /// know from a transport that hands the routing to the OS — see
    /// `hclient-urlsession`, whose `Capabilities::proxy` is this value
    /// over [`detect_platform`](Self::detect_platform).
    ///
    /// The rule, and each clause is a decision:
    ///
    /// - **A `*` bypass answers `false` first.** It is the machine saying
    ///   *no proxy for anything*, and it says so about the script as well
    ///   as about the static entries — which is the under-claiming
    ///   reading of a corner (`*` beside a PAC script) that no
    ///   documentation settles.
    /// - **A PAC script answers `true`.** It decides per request, so the
    ///   honest answer is *unknown*, and a `bool` has no room for one. The
    ///   collapse is towards `true` because the reader is a diagnostic
    ///   asking *am I behind a proxy* and the machine's owner routed its
    ///   traffic through a script: reporting `false` there is the
    ///   *silently direct* answer that
    ///   [`SystemProxyRefused::PacScript`] exists to refuse to give one
    ///   layer down. A script that returns `DIRECT` for every URL makes
    ///   this an over-claim, and that is the one over-claim in here.
    /// - **Any entry answers `true`**, whatever its protocol. A SOCKS
    ///   proxy is one this crate's own [`http_proxies`] refuses to
    ///   install, and the OS installs it perfectly well — so a value read
    ///   off *what this client could do with them* would be wrong for the
    ///   transport that does not need this client to do anything.
    ///
    /// # What it cannot see
    ///
    /// **WPAD auto-discovery.** macOS spells it `ProxyAutoDiscoveryEnable`
    /// and Windows keeps it in a binary blob under
    /// `DefaultConnectionSettings`; neither is read by this module, which
    /// reads `ProxyAutoConfigURLString` and `AutoConfigURL` — a script the
    /// machine *names*. A machine that discovers its script instead is
    /// proxied and answers `false` here. It is stated rather than fixed
    /// because a discovered script has no URL to report, so honouring it
    /// needs an answer this type does not have a shape for; it is the
    /// under-claiming direction, which is the one to be wrong in.
    pub fn names_a_proxy(&self) -> bool {
        if self.bypass_everything {
            return false;
        }
        self.pac.is_some() || !self.entries.is_empty()
    }

    /// The translation, split from the reading so that every rule in it
    /// is testable on a machine with no such settings at all — which is
    /// every machine this workspace is developed on. The platform half
    /// above is four lines and cannot be tested here; this half is the
    /// rest and is tested exhaustively.
    fn from_parts(
        proxies: Vec<(String, String)>,
        bypass: Vec<String>,
        exclude_simple: bool,
    ) -> Self {
        let mut out = Self {
            bypass_local: exclude_simple,
            ..Self::default()
        };

        let mut proxies = proxies;
        proxies.sort();
        for (key, value) in proxies {
            let applies_to = match key.as_str() {
                "http" => Some(Scheme::Http),
                "https" => Some(Scheme::Https),
                // `*` is what Windows produces for an unqualified
                // `ProxyServer`, `all` what an `ALL_PROXY` produces.
                //
                // `socks` and `socks5` join them because a SOCKS proxy
                // carries every scheme — it is a byte tunnel with no idea
                // that HTTP exists. They are *entries* rather than
                // [`ignored`](Self::ignored) precisely so that a
                // transport which cannot speak them refuses by name: an
                // `ftp` proxy is not for us, where a SOCKS one is for us
                // and is one we cannot install.
                //
                // Which of the two SOCKS versions is the key's own
                // decision and is argued in `ProxyKind::Socks4`: Windows
                // writes `socks=` with no version, macOS means SOCKS5.
                "*" | "all" | "socks" | "socks5" => None,
                _ => {
                    out.ignored.push(format!("{key}={value}").into_boxed_str());
                    continue;
                }
            };
            match parse::entry(&key, &value, applies_to) {
                Ok(entry) => out.entries.push(entry),
                // A value the parser refuses is named in `ignored` for
                // the same reason an `ftp` proxy is: the alternative is a
                // setting that vanished.
                Err(e) => out
                    .ignored
                    .push(format!("{key}={value} ({e})").into_boxed_str()),
            }
        }
        // Scheme-specific before catch-all; `sort` above already made the
        // rest deterministic, and `sort_by_key` is stable, so it holds.
        out.entries.sort_by_key(|e| e.applies_to.is_none());

        let mut bypass = bypass;
        bypass.sort();
        for pattern in bypass {
            match parse::bypass(&pattern) {
                parse::Bypass::Pattern(p) => out.bypass.push(p),
                parse::Bypass::Local => out.bypass_local = true,
                parse::Bypass::Everything => out.bypass_everything = true,
                parse::Bypass::AlreadyTrue => {}
                parse::Bypass::Unsupported(reason) => {
                    out.unsupported_bypass.push(UnsupportedBypass {
                        pattern: pattern.into_boxed_str(),
                        reason,
                    })
                }
            }
        }

        out
    }
}

/// A [`SystemProxies`] built from values a test wrote down, for the
/// crates that have to translate one.
///
/// `#[doc(hidden)]`, and it is not part of this crate's public API. The
/// gap exists because `SystemProxies::detect` is the only real
/// constructor — deliberately, since every field in one is an answer from
/// a machine — and the translation below would otherwise
/// be testable only on a machine configured with a proxy, which is no
/// machine any of this is developed or CI'd on.
///
/// The environment would have been the other way in: set `HTTPS_PROXY`,
/// call `detect`. It is unavailable rather than unattractive —
/// `std::env::set_var` is `unsafe` in edition 2024 and this workspace
/// forbids `unsafe` outright.
#[doc(hidden)]
pub mod testing {
    use super::SystemProxies;

    /// The same translation [`SystemProxies::detect`] performs, over
    /// values a caller supplies instead of over the machine's.
    ///
    /// Keys are the platform's own: `http`, `https`, `socks`, `*` for an
    /// unqualified Windows `ProxyServer`, `all` for an `ALL_PROXY`.
    /// The same, for a machine whose proxy is an auto-config script.
    ///
    /// Separate rather than a fourth parameter, because a PAC URL is not
    /// a variation on the others: it is the answer that makes every
    /// other entry beside the point, and a call site that passes `None`
    /// for it on every line would say the opposite.
    pub fn system_proxies_with_pac(
        proxies: &[(&str, &str)],
        bypass: &[&str],
        exclude_simple: bool,
        pac: &str,
    ) -> SystemProxies {
        let mut out = system_proxies(proxies, bypass, exclude_simple);
        out.pac = Some(pac.into());
        out
    }

    pub fn system_proxies(
        proxies: &[(&str, &str)],
        bypass: &[&str],
        exclude_simple: bool,
    ) -> SystemProxies {
        SystemProxies::from_parts(
            proxies
                .iter()
                .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
                .collect(),
            bypass.iter().map(|s| (*s).to_owned()).collect(),
            exclude_simple,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(proxies: &[(&str, &str)], bypass: &[&str], exclude_simple: bool) -> SystemProxies {
        SystemProxies::from_parts(
            proxies
                .iter()
                .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
                .collect(),
            bypass.iter().map(|s| (*s).to_owned()).collect(),
            exclude_simple,
        )
    }

    #[test]
    fn a_scheme_specific_proxy_comes_before_a_catch_all_one() {
        // The whole reason this crate imposes an order: `hclient-native`
        // takes the first entry that serves a request, so the catch-all
        // arriving first would shadow the `https` one and every request
        // would go to the wrong host.
        let sys = cfg(
            &[("*", "everything:3128"), ("https", "secure:8443")],
            &[],
            false,
        );
        assert_eq!(sys.entries()[0].host(), "secure");
        assert_eq!(sys.entries()[0].applies_to(), Some(Scheme::Https));
        assert_eq!(sys.entries()[1].host(), "everything");
        assert_eq!(sys.entries()[1].applies_to(), None);
    }

    #[test]
    fn an_ftp_proxy_is_named_rather_than_dropped() {
        let sys = cfg(&[("ftp", "ftp-proxy:2121")], &[], false);
        assert!(sys.entries().is_empty());
        assert_eq!(&*sys.ignored()[0], "ftp=ftp-proxy:2121");
    }

    #[test]
    fn a_value_the_parser_refuses_is_named_too() {
        // The control for the test above: a *refused* value must not be
        // indistinguishable from no value, which is what an early
        // `continue` would have made it.
        let sys = cfg(&[("http", "https://tls-to-the-proxy:8080")], &[], false);
        assert!(sys.entries().is_empty());
        assert!(sys.ignored()[0].starts_with("http=https://tls-to-the-proxy:8080 ("));
    }

    #[test]
    fn exclude_simple_hostnames_survives_the_translation() {
        assert!(cfg(&[("http", "p:8080")], &[], true).bypass_local());
        assert!(cfg(&[("http", "p:8080")], &["<local>"], false).bypass_local());
        assert!(!cfg(&[("http", "p:8080")], &["example.com"], false).bypass_local());
    }

    #[test]
    fn a_star_bypass_means_no_proxy_at_all() {
        let sys = cfg(&[("http", "p:8080")], &["*"], false);
        assert!(sys.bypass_everything());
        // `is_empty` is what a caller branches on, and it must agree:
        // there is an entry, and it serves nothing.
        assert!(sys.is_empty());
    }

    #[test]
    fn an_untranslatable_pattern_is_surfaced_with_its_reason() {
        let sys = cfg(&[("http", "p:8080")], &["192.168.1.*"], false);
        assert!(sys.bypass().is_empty());
        let reasons: Vec<_> = sys
            .unsupported_bypass()
            .iter()
            .map(|u| u.reason())
            .collect();
        assert_eq!(reasons, [BypassReason::Wildcard]);
    }

    #[test]
    fn a_subnet_is_translated_rather_than_refused() {
        // Every Mac ships `169.254/16` in its default exceptions list, so
        // a refusal here would have been a refusal on the platform's own
        // default configuration — which is how this stopped being one.
        let sys = cfg(&[("http", "p:8080")], &["169.254/16", "10.0.0.0/8"], false);
        assert!(sys.unsupported_bypass().is_empty());
        assert_eq!(&*sys.bypass()[0], "10.0.0.0/8");
        assert_eq!(&*sys.bypass()[1], "169.254/16");
    }

    #[test]
    fn a_leading_star_label_is_translated_rather_than_refused() {
        let sys = cfg(&[("http", "p:8080")], &["*.example.com"], false);
        assert_eq!(&*sys.bypass()[0], ".example.com");
        assert!(sys.unsupported_bypass().is_empty());
    }

    #[test]
    fn minus_loopback_is_satisfied_by_doing_nothing() {
        // Windows's `<-loopback>` negates its own implicit loopback
        // bypass. Nothing here bypasses loopback in the first place, so
        // the request is already met — and it must not be reported as a
        // pattern nobody can express, which would refuse a configuration
        // that is in fact honoured exactly.
        let sys = cfg(&[("http", "p:8080")], &["<-loopback>"], false);
        assert!(sys.bypass().is_empty());
        assert!(sys.unsupported_bypass().is_empty());
    }

    #[test]
    fn nothing_configured_is_an_empty_answer_and_not_an_error() {
        let sys = cfg(&[], &[], false);
        assert!(sys.is_empty());
        assert!(sys.entries().is_empty());
    }

    fn cfg_pac(proxies: &[(&str, &str)], bypass: &[&str], pac: &str) -> SystemProxies {
        let mut sys = cfg(proxies, bypass, false);
        sys.pac = Some(pac.into());
        sys
    }

    #[test]
    fn a_static_proxy_is_a_proxy_the_machine_names() {
        assert!(cfg(&[("http", "p:8080")], &[], false).names_a_proxy());
    }

    #[test]
    fn a_machine_with_nothing_configured_names_none() {
        assert!(!cfg(&[], &[], false).names_a_proxy());
    }

    #[test]
    fn a_proxy_for_a_protocol_this_is_not_about_names_none() {
        // An `ftp` proxy lands in `ignored`, not in `entries` — so the
        // report must not read it as *this machine is proxied*, which is
        // a claim about HTTP.
        let sys = cfg(&[("ftp", "ftp-proxy:2121")], &[], false);
        assert!(!sys.ignored().is_empty());
        assert!(!sys.names_a_proxy());
    }

    #[test]
    fn a_pac_script_alone_names_a_proxy_and_offers_nothing_to_install() {
        // The case that separates the two questions, and the reason
        // `names_a_proxy` is not `is_empty` negated: there is nothing
        // here for `http_proxies` to install, and the machine is
        // nevertheless routed through a proxy by whoever runs the script.
        let sys = cfg_pac(&[], &[], "http://wpad/proxy.pac");
        assert!(sys.is_empty());
        assert!(sys.names_a_proxy());
    }

    #[test]
    fn a_star_bypass_answers_no_even_beside_a_pac_script() {
        // `*` is the machine saying *no proxy for anything*, and it is
        // read as saying it about the script too — the under-claiming
        // reading of a corner nothing documents.
        assert!(!cfg_pac(&[("http", "p:8080")], &["*"], "http://wpad/p.pac").names_a_proxy());
        assert!(!cfg(&[("http", "p:8080")], &["*"], false).names_a_proxy());
    }

    #[test]
    fn a_socks_proxy_this_client_cannot_install_still_names_a_proxy() {
        // The direction that says this is read off the *machine* rather
        // than off what this crate could do with it: `http_proxies`
        // refuses a SOCKS entry, and the OS installs it perfectly well.
        let sys = cfg(&[("socks5", "s:1080")], &[], false);
        assert!(http_proxies(&sys).is_err());
        assert!(sys.names_a_proxy());
    }

    /// The environment is read by [`SystemProxies::detect`] and must not
    /// reach [`SystemProxies::detect_platform`], which is the whole of
    /// why the second exists.
    ///
    /// A **child process**, because `std::env::set_var` is `unsafe` in
    /// edition 2024 and this workspace forbids `unsafe` — the same wall
    /// `system::testing` records. What it compares is this process's own
    /// platform answer against the child's, so the assertion holds on a
    /// machine that really is proxied as well as on one that is not:
    /// nothing here assumes anything about the host.
    #[test]
    fn the_environment_does_not_reach_the_platform_read() {
        const MARKER: &str = "HCLIENT_PROXY_ENV_ISOLATION_CHILD";
        const EXPECT: &str = "HCLIENT_PROXY_ENV_ISOLATION_EXPECT";

        if let Ok(expected) = std::env::var(EXPECT) {
            // The child. It was given an `HTTPS_PROXY` its parent did not
            // have.
            assert!(
                SystemProxies::detect().names_a_proxy(),
                "`detect` did not read the `HTTPS_PROXY` this process was started with"
            );
            assert_eq!(
                format!("{:?}", SystemProxies::detect_platform()),
                expected,
                "`detect_platform` changed when an `HTTPS_PROXY` was put in the environment"
            );
            return;
        }

        let expected = format!("{:?}", SystemProxies::detect_platform());
        let exe = std::env::current_exe().expect("the test binary's own path");
        let out = std::process::Command::new(exe)
            // The **libtest** name, which is the module path without
            // the crate segment. Hardcoded, and a rename that forgets it
            // does not go quiet: the child then runs nothing, and the
            // control below is what says so.
            .args([
                "system::tests::the_environment_does_not_reach_the_platform_read",
                "--exact",
                "--nocapture",
            ])
            .env(MARKER, "1")
            .env(EXPECT, &expected)
            .env("HTTPS_PROXY", "http://env-only.invalid:8080")
            .env_remove("NO_PROXY")
            .env_remove("no_proxy")
            .output()
            .expect("re-running this test binary");
        assert!(
            out.status.success(),
            "the child arm failed\n--- stdout ---\n{}\n--- stderr ---\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr),
        );
        // The control: a child that ran no test at all would also exit
        // zero, which is this workspace's recurring defect.
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            stdout.contains("1 passed"),
            "the child exited zero without running the test\n{stdout}"
        );
    }

    #[test]
    fn a_password_does_not_reach_a_debug_line() {
        let sys = cfg(&[("http", "http://alice:hunter2@p:8080")], &[], false);
        let rendered = format!("{:?}", sys.entries()[0]);
        assert!(rendered.contains("alice"), "{rendered}");
        assert!(!rendered.contains("hunter2"), "{rendered}");
    }
}
