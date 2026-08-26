//! Proxy auto-config: running the machine's `.pac` script.
//!
//! Behind the `pac` feature, off by default, and **it is the most
//! expensive feature in this workspace by a wide margin**: a PAC file is
//! a JavaScript program, so honouring one means carrying a JavaScript
//! engine.
//!
//! Measured rather than estimated — one binary that drives a handshake
//! and evaluates a script, `opt-level = "z"`, fat LTO, `panic = "abort"`,
//! stripped:
//!
//! | build | binary | over the protocols alone |
//! |---|---|---|
//! | the three protocols | 313 KiB | — |
//! | `system` | 341 KiB | +28 KiB |
//! | `pac` | **3,840 KiB** | **+3.4 MiB, twelve times over** |
//!
//! `boa_engine` is 114 crates on its own, more than twice `hclient`'s
//! whole graph. So nothing switches this on for you, and
//! [`SystemProxies::pac`](crate::system::SystemProxies::pac) reports the
//! script's URL **without** the feature — a build that declines the
//! engine still knows the machine has one, and can say so rather than
//! going quietly direct.
//!
//! ```no_run
//! use hclient_proxy::pac::{Pac, PacEnv};
//!
//! let pac = Pac::compile("function FindProxyForURL(url, host) { return 'DIRECT'; }")?;
//! let verdict = pac.find("https://example.com/x", "example.com", &PacEnv::default())?;
//! # Ok::<(), hclient_proxy::pac::PacError>(())
//! ```
//!
//! # Sans-io, which for a PAC file takes saying twice
//!
//! **Fetching the script is not here.** A `.pac` URL is fetched over
//! HTTP, and a proxy module that fetched it would need a client — the
//! dependency direction this whole crate exists to avoid. The caller
//! fetches it, with their own client, their own timeout and their own
//! caching, and hands over the text.
//!
//! **The script's own IO is not here either, and that is the sharper
//! half.** The PAC environment specifies functions that resolve names —
//! `dnsResolve`, `isResolvable`, `myIpAddress`, and `isInNet` when it is
//! handed a name. They come from [`PacEnv`], which a caller fills from
//! whatever resolver it already has, and whose default answers
//! *unresolvable*: a script asking a question nobody answered gets a
//! definite negative rather than a hang or an invented address that sends
//! the request to the wrong proxy.
//!
//! The clock is the same shape, and this module is clockless like every
//! other sans-io part of this workspace: [`PacEnv::with_now`] carries the
//! time a caller chooses to expose, is unset by default, and the calendar
//! functions then answer `false`.
//!
//! # What a verdict is, and why it is a list
//!
//! `FindProxyForURL` returns a string like
//! `"PROXY a:8080; PROXY b:8080; DIRECT"` — a **fallback chain**, tried in
//! order. So [`Pac::find`] hands back every entry rather than the first:
//! which ones a caller may try, and how it decides one has failed, is a
//! policy belonging to whoever owns the connection.
//!
//! # What is not wired up
//!
//! Nothing in `hclient-native` consults a PAC file yet. A static proxy
//! list is installed on a transport once; a script decides **per
//! request**, which is a different shape in the connect path and comes
//! with questions this module deliberately does not answer — when the
//! script is refetched, what happens while it is being fetched, and
//! whether a failed proxy is remembered. This is the evaluator; the
//! policy above it is somebody's decision, not a default.

use std::net::IpAddr;
use std::rc::Rc;
use std::time::SystemTime;

use crate::system::{ProxyEntry, ProxyKind};

mod engine;

/// A PAC script, checked and ready to run.
///
/// Compiling is separate from evaluating because a PAC file is consulted
/// **once per request**, and parsing the source each time would put a
/// JavaScript parse on every connection.
#[derive(Debug, Clone)]
pub struct Pac {
    source: String,
}

/// What answers `dnsResolve` and its two neighbours.
///
/// A type alias for clippy's sake, and it says the thing the bare type
/// buries: **no `Send`, no `Sync`, no `'static` beyond the closure's
/// own** — the environment never leaves the thread that installs it.
type Resolver = Rc<dyn Fn(&str) -> Option<IpAddr>>;

/// What the script may ask the world, and what it is told.
///
/// Every default is the answer that admits ignorance rather than
/// inventing one — see the module doc.
#[derive(Clone)]
pub struct PacEnv {
    resolve: Resolver,
    my_ip: IpAddr,
    now: Option<SystemTime>,
}

impl Default for PacEnv {
    fn default() -> Self {
        Self {
            resolve: Rc::new(|_| None),
            my_ip: IpAddr::from([127, 0, 0, 1]),
            now: None,
        }
    }
}

impl std::fmt::Debug for PacEnv {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PacEnv")
            .field("my_ip", &self.my_ip)
            .field("now", &self.now)
            .finish_non_exhaustive()
    }
}

impl PacEnv {
    /// What `dnsResolve`, `isResolvable` and the name form of `isInNet`
    /// are answered with.
    ///
    /// **`Rc<dyn Fn>` with no `Send` and no `Sync`**, deliberately: the
    /// environment never crosses a thread — it is installed for the
    /// length of one evaluation and taken down after it — so a caller
    /// holding an `Rc` is not shut out, which is the property this
    /// workspace protects at every seam.
    ///
    /// The default answers `None` for every name. That is what an
    /// unresolvable name looks like to a PAC file, and it is the honest
    /// answer from a client that was given no resolver — where a
    /// fabricated address would send the script down the wrong branch.
    #[must_use]
    pub fn with_resolver(mut self, resolve: impl Fn(&str) -> Option<IpAddr> + 'static) -> Self {
        self.resolve = Rc::new(resolve);
        self
    }

    /// What `myIpAddress()` answers.
    ///
    /// The default is `127.0.0.1`, which is what every implementation
    /// answers when it cannot tell.
    #[must_use]
    pub fn with_my_ip(mut self, ip: IpAddr) -> Self {
        self.my_ip = ip;
        self
    }

    /// The wall clock, for `weekdayRange` and `timeRange`.
    ///
    /// `None` — the default — makes them answer `false`, and it is the
    /// default because this module is clockless for the reason
    /// `hclient::cookie` and `hclient::cache` are. Read as **UTC**; see
    /// `engine`'s module doc for why local time is not available and what
    /// that costs a script that does not say `"GMT"`.
    #[must_use]
    pub fn with_now(mut self, now: SystemTime) -> Self {
        self.now = Some(now);
        self
    }
}

/// One entry of a PAC verdict.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PacVerdict {
    /// `DIRECT` — no proxy for this request.
    Direct,
    /// `PROXY host:port`, `SOCKS host:port`, `SOCKS5 host:port`.
    ///
    /// Carries the same [`ProxyEntry`] the platform's static settings
    /// produce, so a caller has one vocabulary rather than two.
    Proxy(ProxyEntry),
}

/// What went wrong.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum PacError {
    #[error("the PAC script does not compile: {0}")]
    Compile(Box<str>),
    #[error("FindProxyForURL failed: {0}")]
    Eval(Box<str>),
    #[error("the PAC script defines no FindProxyForURL function")]
    NoEntryPoint,
    #[error("FindProxyForURL returned `{0}`, which names nothing this client can reach")]
    BadVerdict(Box<str>),
}

impl Pac {
    /// Check that the script parses and defines `FindProxyForURL`.
    ///
    /// Both are worth finding out before a request rather than during
    /// one: a machine whose PAC file is broken is misconfigured, and the
    /// failure belongs where somebody can act on it.
    pub fn compile(source: &str) -> Result<Self, PacError> {
        let pac = Self {
            source: source.to_owned(),
        };
        engine::with_context(&pac.source, &PacEnv::default(), |ctx| {
            engine::entry_point(ctx).map(|_| ())
        })?;
        Ok(pac)
    }

    /// Run `FindProxyForURL(url, host)`.
    ///
    /// The result is the whole fallback chain, in the script's order —
    /// see the module doc for why the first entry is not chosen here.
    pub fn find(&self, url: &str, host: &str, env: &PacEnv) -> Result<Vec<PacVerdict>, PacError> {
        let verdict = engine::with_context(&self.source, env, |ctx| engine::call(ctx, url, host))?;
        parse_verdict(&verdict)
    }
}

/// `"PROXY a:8080; DIRECT"` into entries.
///
/// Unknown keywords are **skipped rather than refused**, which is the one
/// place this module is lenient, and the reason is the format's own: the
/// list is a fallback chain, so an entry nobody understands is one
/// alternative fewer, where refusing the whole line would discard the
/// ones that were understood. A line with nothing usable in it *is* an
/// error, because that is a script whose answer this client cannot act on
/// at all — and silently going direct on it is the failure the whole
/// `system` module is built to avoid.
fn parse_verdict(s: &str) -> Result<Vec<PacVerdict>, PacError> {
    let mut out = Vec::new();
    for entry in s.split(';') {
        let mut words = entry.split_whitespace();
        let Some(keyword) = words.next() else {
            continue;
        };
        let kind = match keyword.to_ascii_uppercase().as_str() {
            "DIRECT" => {
                out.push(PacVerdict::Direct);
                continue;
            }
            "PROXY" => ProxyKind::Http,
            // `HTTPS` is Chromium's extension and means TLS **to the
            // proxy**, which this workspace refuses everywhere for one
            // reason — see `system::ParseError::TlsToProxyUnsupported`.
            // Skipped rather than read as `PROXY`, which would send the
            // `CONNECT` line and any credential with it in the clear.
            "HTTPS" => continue,
            "SOCKS" | "SOCKS4" => ProxyKind::Socks4,
            "SOCKS5" => ProxyKind::Socks5,
            _ => continue,
        };
        let Some(authority) = words.next() else {
            continue;
        };
        if let Ok(entry) = crate::system::entry_from_authority(kind, authority) {
            out.push(PacVerdict::Proxy(entry));
        }
    }
    if out.is_empty() {
        return Err(PacError::BadVerdict(s.into()));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn find(script: &str, url: &str, host: &str) -> Result<Vec<PacVerdict>, PacError> {
        Pac::compile(script)?.find(url, host, &PacEnv::default())
    }

    fn one(script: &str, url: &str, host: &str) -> PacVerdict {
        find(script, url, host).expect("a verdict").remove(0)
    }

    const DIRECT: &str = "function FindProxyForURL(url, host) { return 'DIRECT'; }";

    #[test]
    fn a_script_that_returns_direct_says_so() {
        assert_eq!(
            one(DIRECT, "https://example.com/", "example.com"),
            PacVerdict::Direct
        );
    }

    #[test]
    fn the_url_and_the_host_both_reach_the_script() {
        // Both arguments, because a script branching on the URL's scheme
        // and one branching on the host are equally ordinary.
        let s = "function FindProxyForURL(url, host) { return 'PROXY ' + host + ':' + \
                 (url.indexOf('https:') == 0 ? 443 : 80); }";
        let PacVerdict::Proxy(e) = one(s, "https://example.com/x", "example.com") else {
            panic!("a proxy")
        };
        assert_eq!((e.host(), e.port()), ("example.com", 443));

        let PacVerdict::Proxy(e) = one(s, "http://example.com/x", "example.com") else {
            panic!("a proxy")
        };
        assert_eq!(e.port(), 80);
    }

    #[test]
    fn a_fallback_chain_comes_back_whole_and_in_order() {
        // The chain is the point: a caller decides how far down it to go,
        // and one that only ever saw the first entry could not fall back.
        let s = "function FindProxyForURL(u, h) { return 'PROXY a:8080; SOCKS5 b:1080; DIRECT'; }";
        let v = find(s, "https://x/", "x").expect("a verdict");
        assert_eq!(v.len(), 3);
        assert!(
            matches!(&v[0], PacVerdict::Proxy(e) if e.host() == "a" && e.kind() == ProxyKind::Http)
        );
        assert!(
            matches!(&v[1], PacVerdict::Proxy(e) if e.host() == "b" && e.kind() == ProxyKind::Socks5)
        );
        assert_eq!(v[2], PacVerdict::Direct);
    }

    #[test]
    fn an_https_entry_is_skipped_rather_than_read_as_a_plain_proxy() {
        // Reading Chromium's `HTTPS` keyword as `PROXY` would send the
        // CONNECT line, and any credential with it, in the clear to a
        // proxy whose owner configured TLS precisely so that it would not
        // be.
        let s = "function FindProxyForURL(u, h) { return 'HTTPS secure:443; PROXY plain:8080'; }";
        let v = find(s, "https://x/", "x").expect("a verdict");
        assert_eq!(v.len(), 1);
        assert!(matches!(&v[0], PacVerdict::Proxy(e) if e.host() == "plain"));
    }

    #[test]
    fn an_unknown_keyword_costs_one_alternative_and_not_the_line() {
        let s = "function FindProxyForURL(u, h) { return 'QUIC q:443; PROXY a:8080'; }";
        assert_eq!(find(s, "https://x/", "x").expect("a verdict").len(), 1);
    }

    #[test]
    fn a_line_with_nothing_usable_is_an_error() {
        // The control for the two tests above: leniency about *entries*
        // must not become silence about an answer this client cannot act
        // on at all.
        let s = "function FindProxyForURL(u, h) { return 'QUIC q:443'; }";
        assert!(matches!(
            find(s, "https://x/", "x"),
            Err(PacError::BadVerdict(_))
        ));
    }

    #[test]
    fn a_script_that_does_not_parse_fails_at_compile_time() {
        assert!(matches!(
            Pac::compile("function FindProxyForURL(u, h) { return 'DIRECT' "),
            Err(PacError::Compile(_))
        ));
    }

    #[test]
    fn a_script_with_no_entry_point_is_named_as_such() {
        // A file that parses and defines nothing is a misconfiguration a
        // caller can fix, so it is worth its own error rather than a
        // failure at the first request.
        assert!(matches!(
            Pac::compile("var x = 1;"),
            Err(PacError::NoEntryPoint)
        ));
        assert!(matches!(
            Pac::compile("var FindProxyForURL = 3;"),
            Err(PacError::NoEntryPoint)
        ));
    }

    #[test]
    fn a_script_that_throws_reports_the_throw() {
        let s = "function FindProxyForURL(u, h) { throw new Error('nope'); }";
        assert!(matches!(find(s, "https://x/", "x"), Err(PacError::Eval(_))));
    }

    // --- the helper functions ------------------------------------------

    #[test]
    fn is_plain_host_name_is_about_dots() {
        let s = "function FindProxyForURL(u, h) { \
                 return isPlainHostName(h) ? 'DIRECT' : 'PROXY p:8080'; }";
        assert_eq!(one(s, "http://intranet/", "intranet"), PacVerdict::Direct);
        assert!(matches!(one(s, "http://a.b/", "a.b"), PacVerdict::Proxy(_)));
    }

    #[test]
    fn dns_domain_is_matches_a_suffix() {
        let s = "function FindProxyForURL(u, h) { \
                 return dnsDomainIs(h, '.example.com') ? 'DIRECT' : 'PROXY p:8080'; }";
        assert_eq!(
            one(s, "http://a.example.com/", "a.example.com"),
            PacVerdict::Direct
        );
        assert!(matches!(
            one(s, "http://notexample.com/", "notexample.com"),
            PacVerdict::Proxy(_)
        ));
    }

    #[test]
    fn local_host_or_domain_is_matches_the_short_name_and_the_long_one() {
        let s = "function FindProxyForURL(u, h) { \
                 return localHostOrDomainIs(h, 'www.example.com') ? 'DIRECT' : 'PROXY p:8080'; }";
        assert_eq!(one(s, "http://www/", "www"), PacVerdict::Direct);
        assert_eq!(
            one(s, "http://www.example.com/", "www.example.com"),
            PacVerdict::Direct
        );
        // A different host that happens to share a prefix is not it.
        assert!(matches!(
            one(s, "http://www.example.org/", "www.example.org"),
            PacVerdict::Proxy(_)
        ));
    }

    #[test]
    fn sh_exp_match_reads_shell_globs_and_not_regexes() {
        let s = "function FindProxyForURL(u, h) { \
                 return shExpMatch(h, '*.example.com') ? 'DIRECT' : 'PROXY p:8080'; }";
        assert_eq!(
            one(s, "http://a.example.com/", "a.example.com"),
            PacVerdict::Direct
        );
        // The regex translation this avoids would match this, because an
        // unescaped `.` means any character.
        assert!(matches!(
            one(s, "http://aXexample1com/", "aXexample1com"),
            PacVerdict::Proxy(_)
        ));
    }

    #[test]
    fn is_in_net_compares_addresses_under_a_mask() {
        let s = "function FindProxyForURL(u, h) { \
                 return isInNet(h, '10.0.0.0', '255.0.0.0') ? 'DIRECT' : 'PROXY p:8080'; }";
        assert_eq!(one(s, "http://10.1.2.3/", "10.1.2.3"), PacVerdict::Direct);
        assert!(matches!(
            one(s, "http://11.1.2.3/", "11.1.2.3"),
            PacVerdict::Proxy(_)
        ));
    }

    #[test]
    fn an_unresolvable_name_is_a_definite_negative_rather_than_a_guess() {
        // The default `PacEnv` resolves nothing, and a script asking
        // about a name must be told *no* rather than handed an invented
        // address that sends the request to the wrong proxy.
        let s = "function FindProxyForURL(u, h) { \
                 return isResolvable(h) ? 'PROXY p:8080' : 'DIRECT'; }";
        assert_eq!(one(s, "http://a.b/", "a.b"), PacVerdict::Direct);

        let r = "function FindProxyForURL(u, h) { \
                 return dnsResolve(h) === null ? 'DIRECT' : 'PROXY p:8080'; }";
        assert_eq!(one(r, "http://a.b/", "a.b"), PacVerdict::Direct);
    }

    #[test]
    fn an_address_literal_needs_no_resolver() {
        // `isInNet` is called both with a name and with the output of
        // `dnsResolve`, so a version that always resolved would ask the
        // resolver about `10.0.0.1` — and the default resolver would then
        // answer no.
        let s = "function FindProxyForURL(u, h) { \
                 return dnsResolve('10.0.0.1') === '10.0.0.1' ? 'DIRECT' : 'PROXY p:8080'; }";
        assert_eq!(one(s, "http://x/", "x"), PacVerdict::Direct);
    }

    #[test]
    fn a_resolver_the_caller_supplies_is_the_one_the_script_asks() {
        let env = PacEnv::default()
            .with_resolver(|h| (h == "known.example").then(|| IpAddr::from([10, 0, 0, 7])));
        let pac = Pac::compile(
            "function FindProxyForURL(u, h) { \
             return isInNet(dnsResolve(h), '10.0.0.0', '255.0.0.0') ? 'DIRECT' : 'PROXY p:8080'; }",
        )
        .expect("compiles");

        assert_eq!(
            pac.find("http://known.example/", "known.example", &env)
                .unwrap()[0],
            PacVerdict::Direct
        );
        assert!(matches!(
            pac.find("http://other.example/", "other.example", &env)
                .unwrap()[0],
            PacVerdict::Proxy(_)
        ));
    }

    #[test]
    fn the_calendar_functions_answer_false_without_a_clock() {
        // Clockless by default, like every other sans-io part of this
        // workspace — and `false` rather than an error, because a script
        // guarding a branch on `weekdayRange` should take the other
        // branch rather than fail.
        let s = "function FindProxyForURL(u, h) { \
                 return weekdayRange('MON', 'SUN') ? 'PROXY p:8080' : 'DIRECT'; }";
        assert_eq!(one(s, "http://a.b/", "a.b"), PacVerdict::Direct);
    }

    #[test]
    fn a_clock_the_caller_supplies_answers_the_weekday() {
        // 2026-08-27 is a Thursday. Both directions, so that a version
        // answering `true` for everything fails.
        // 2026-08-27T12:00:00Z. Checked against a calendar rather than
        // assumed: the first draft of this line was a Friday, and every
        // assertion below still passed but for the wrong day.
        let thursday = SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_787_832_000);
        let env = PacEnv::default().with_now(thursday);
        let ask = |from: &str, to: &str| {
            let s = format!(
                "function FindProxyForURL(u, h) {{ \
                 return weekdayRange('{from}', '{to}') ? 'PROXY p:8080' : 'DIRECT'; }}"
            );
            Pac::compile(&s)
                .unwrap()
                .find("http://x/", "x", &env)
                .unwrap()[0]
                .clone()
        };
        assert!(matches!(ask("MON", "FRI"), PacVerdict::Proxy(_)));
        assert_eq!(ask("SAT", "SUN"), PacVerdict::Direct);
        // A range that wraps the week, which is the case an ordinary
        // `from..=to` gets wrong.
        assert!(matches!(ask("WED", "MON"), PacVerdict::Proxy(_)));
        assert_eq!(ask("FRI", "TUE"), PacVerdict::Direct);
    }

    #[test]
    fn my_ip_address_answers_loopback_until_a_caller_says_otherwise() {
        let s = "function FindProxyForURL(u, h) { return 'PROXY ' + myIpAddress() + ':8080'; }";
        let PacVerdict::Proxy(e) = one(s, "http://a.b/", "a.b") else {
            panic!("a proxy")
        };
        assert_eq!(e.host(), "127.0.0.1");

        let env = PacEnv::default().with_my_ip(IpAddr::from([192, 168, 1, 5]));
        let PacVerdict::Proxy(e) = Pac::compile(s)
            .unwrap()
            .find("http://a.b/", "a.b", &env)
            .unwrap()
            .remove(0)
        else {
            panic!("a proxy")
        };
        assert_eq!(e.host(), "192.168.1.5");
    }

    #[test]
    fn dns_domain_levels_counts_dots() {
        let s = "function FindProxyForURL(u, h) { \
                 return dnsDomainLevels(h) > 1 ? 'PROXY p:8080' : 'DIRECT'; }";
        assert_eq!(one(s, "http://a.b/", "a.b"), PacVerdict::Direct);
        assert!(matches!(
            one(s, "http://a.b.c/", "a.b.c"),
            PacVerdict::Proxy(_)
        ));
    }

    #[test]
    fn a_realistic_corporate_script_routes_both_ways() {
        // The shape these files actually take, so the helpers are
        // exercised together rather than one per test.
        let s = "function FindProxyForURL(url, host) {
                    if (isPlainHostName(host) ||
                        shExpMatch(host, '*.local') ||
                        isInNet(host, '10.0.0.0', '255.0.0.0')) {
                        return 'DIRECT';
                    }
                    if (dnsDomainIs(host, '.corp.example')) {
                        return 'PROXY internal:3128; DIRECT';
                    }
                    return 'PROXY edge:8080; DIRECT';
                 }";
        assert_eq!(one(s, "http://intranet/", "intranet"), PacVerdict::Direct);
        assert_eq!(
            one(s, "http://printer.local/", "printer.local"),
            PacVerdict::Direct
        );
        assert_eq!(one(s, "http://10.1.2.3/", "10.1.2.3"), PacVerdict::Direct);

        let PacVerdict::Proxy(e) = one(s, "https://git.corp.example/", "git.corp.example") else {
            panic!("a proxy")
        };
        assert_eq!((e.host(), e.port()), ("internal", 3128));

        let PacVerdict::Proxy(e) = one(s, "https://example.com/", "example.com") else {
            panic!("a proxy")
        };
        assert_eq!((e.host(), e.port()), ("edge", 8080));
    }

    #[test]
    fn one_compiled_script_can_be_run_many_times() {
        // What `compile` exists for: a PAC file is consulted once per
        // request, so the parse must not be.
        let pac = Pac::compile(DIRECT).expect("compiles");
        for _ in 0..3 {
            assert_eq!(
                pac.find("http://x/", "x", &PacEnv::default()).unwrap()[0],
                PacVerdict::Direct
            );
        }
    }
}
