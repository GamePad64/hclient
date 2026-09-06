//! RFC 6797 HTTP Strict Transport Security: note what a host asserts,
//! and upgrade `http://` to `https://` before the request is made — with
//! no I/O and no clock of its own.
//!
//! # What this changes that the jar and the cache do not
//!
//! The other two memories in this crate change how fast a request is
//! answered, or whether a header rides along. This one changes **where
//! the request goes**, and the direction it changes it in is the whole
//! point: a caller who typed `http://` gets `https://`, and a network
//! that would have seen the request in clear text does not.
//!
//! That also decides how its failures are read. A wrong cookie is a
//! request the server refuses; a wrong cache entry is a stale body. A
//! wrong entry *here* is either a request going out in clear text that
//! should not have — which is the defect this exists against — or an
//! `https://` request to an origin that speaks only `http://`, which
//! fails at connect. Both directions are visible; neither is silent. What
//! would be silent is not having it at all, which is the state before
//! this module.
//!
//! # Why this is in `Client` and not in a transport
//!
//! `Alt-Svc` is the near neighbour and it lives in `hclient-native`,
//! because the protocol it chooses between is that transport's own
//! business and no other backend has an opinion. HSTS is the opposite:
//! the scheme is settled **before** any transport is asked, it decides
//! which port and which TLS handshake happen at all, and a `Location:
//! http://…` on a redirect is `Client`'s to resolve. §8.3 says so in as
//! many words — *"whenever the UA prepares to 'load' … any 'http' URI
//! (including when following HTTP redirects)"* — and the redirect loop
//! is here.
//!
//! It is also not in [`hclient_proto`], where the redirect *mechanism*
//! lives, for the reason the jar and the cache are not: this needs a
//! calendar clock and a store, and that crate is the sans-io leaf whose
//! dependency count is guarded.
//!
//! # There is no capability gate, and that is a decision
//!
//! [`ClientBuilder::cookie_jar`](crate::ClientBuilder::cookie_jar) and
//! [`ClientBuilder::cache`](crate::ClientBuilder::cache) are refused at
//! `build()` against a transport that keeps its own — `hclient-fetch`,
//! where a browser does both — because doing the work twice is
//! *harmful*: two `Cookie` headers, two stored copies. A browser applies
//! HSTS too, and doing that twice is not harmful, because both answers
//! are the same answer: `https://`. Upgrading a URI that the browser
//! would also have upgraded changes nothing about the request that goes
//! out.
//!
//! So a capability here would be a gate with nothing to refuse, which is
//! the *distinction with one reachable side* this workspace deletes
//! rather than adds — `UpgradeSupport`'s four variants went for it. If a
//! transport ever appears that would be made **wrong** by an upgrade
//! above it, the capability, the check and the refusal arrive together,
//! which is the rule `owns_cookie_jar`'s own doc states for its third
//! state.
//!
//! What this module does **not** do is claim the browser's HSTS is
//! reachable from here: [`Hsts`] over `hclient-fetch` keeps a second,
//! private set of known hosts that the browser's own set neither reads
//! nor writes. That costs a redundant upgrade and buys nothing there,
//! which is worth knowing before switching it on for a browser build.
//!
//! # What it deliberately does not do
//!
//! - **No preload list.** Chromium ships one of about 150,000 entries,
//!   and it is a *policy* — who is on it, how it is updated, what a stale
//!   copy means — rather than a mechanism. [`HstsStore`] is where one
//!   would go: a store answering `example.com` from a compiled-in list is
//!   exactly the shape this seam takes, and it needs nothing here to
//!   change. The absence is why the first request to a never-visited host
//!   is not protected, which is HSTS's own bootstrap gap (§14.6) rather
//!   than one this adds.
//! - **It does not enforce §8.4's *"terminate the connection if there are
//!   any errors … with the underlying secure transport"*.** That is a
//!   rule about a TLS handshake, and this crate does not conduct one —
//!   the seam that could is `TlsConnect`, one layer below `Transport`. A
//!   client that upgrades the scheme and then accepts a bad certificate
//!   has honoured half of HSTS, and the half it honours is the half that
//!   moves the bytes onto TLS at all. Said here rather than left to be
//!   discovered, because it is the one place this module is narrower than
//!   the RFC it names.
//! - **It has no background sweep.** An expired entry is dropped when it
//!   is next looked up — §8.1.1's *"MUST evict all expired Known HSTS
//!   Hosts"* read as *before it can affect an answer*, which is the jar's
//!   rule and the alt-svc cache's for the same reason: there is nothing
//!   here to run a sweep.

mod parse;
mod store;

use std::time::Duration;
use web_time::SystemTime;

pub use parse::Directives;
pub use store::{Entry, HstsStore, MemoryStore};

use store::candidate_domains;

/// An asserted `max-age` further out than this is capped to it.
///
/// **RFC 6797 sets no ceiling**, and the cap is this workspace's, for the
/// reason `cookie::MAX_EXPIRY` and `altsvc::MAX_LEASE` carry the same
/// figure: `SystemTime` has no `MAX` to saturate towards, so an absurd
/// `max-age` has to become either a panic or an immediate expiry, and the
/// second silently drops a policy whose whole job is not to be dropped.
/// 400 days is RFC 6265bis's number rather than one invented here, and
/// the sum is never computed, so the hazard has no end to be at.
///
/// **It is `pub` where `cookie::MAX_EXPIRY` is `pub(super)`**, and the
/// difference is a reader rather than a preference: that constant is
/// named by nothing a caller can see, where this one is what
/// [`Directives::max_age`]'s documentation points at to explain why an
/// absurd value cannot overflow. A number a public doc comment reasons
/// about should be a number a reader can look up.
///
/// The direction is safe: a host asserting a century gets 400 days and
/// re-asserts it on the next visit, which §11.2 assumes happens
/// (*"the UA … refreshes the expiry time"*). A host asserting less than
/// 400 days is unaffected, which is every real deployment —
/// `max-age=31536000`, one year, is what the common guidance says.
pub const MAX_AGE_CAP: Duration = Duration::from_secs(400 * 24 * 60 * 60);

/// The RFC 6797 rules over a [`HstsStore`].
///
/// `Hsts::new()` is the plain form, over a [`MemoryStore`];
/// [`with_store`](Self::with_store) takes one of the caller's own.
#[derive(Debug, Default)]
pub struct Hsts<S = MemoryStore> {
    store: S,
}

impl Hsts<MemoryStore> {
    /// An empty policy set in memory.
    pub fn new() -> Self {
        Self {
            store: MemoryStore::new(),
        }
    }
}

impl<S: HstsStore> Hsts<S> {
    /// The rules over a store of the caller's own.
    pub fn with_store(store: S) -> Self {
        Self { store }
    }

    /// The store this was built over.
    ///
    /// **`store` where [`HttpCache`](crate::cache::HttpCache) calls the
    /// same thing `store_ref`**, and the difference is a name collision
    /// rather than a disagreement: that type has a `store(..)` *verb* —
    /// it puts a response away — so its accessor had to be called
    /// something else. Nothing here is called `store` but the noun, so
    /// this reads as [`CookieJar`](crate::cookie::CookieJar)'s does.
    pub fn store(&self) -> &S {
        &self.store
    }

    /// Forget every policy this set holds.
    ///
    /// For a caller changing whose requests these are — a profile switch,
    /// a logout, a test between cases. It is **not** RFC 6797's
    /// `max-age=0`, which is one host withdrawing its own assertion and
    /// is [`note`](Self::note)'s business; this is the user agent's own
    /// decision about a set it owns.
    ///
    /// The bluntness is the point, and it is worth knowing which way it
    /// cuts: every host that has to be visited again over `https://`
    /// before it is protected again, so clearing is the *unsafe*
    /// direction and a caller should mean it.
    pub async fn clear(&self) {
        self.store.clear().await;
    }

    /// The same rules over a different store — `CookieJar::map_store`'s
    /// twin and `pub(crate)` for the same reason: the one thing that asks
    /// is [`ClientBuilder::hsts`](crate::ClientBuilder::hsts), which holds
    /// one type for every caller. A caller who wants a different store
    /// builds one with [`with_store`](Self::with_store).
    pub(crate) fn map_store<R>(self, f: impl FnOnce(S) -> R) -> Hsts<R> {
        Hsts {
            store: f(self.store),
        }
    }

    /// §8.3: whether `uri` names a known HSTS host, and what to send
    /// instead if it does.
    ///
    /// `None` means *leave it alone* — either the scheme was not `http`,
    /// or no policy covers the host. `Some(uri)` is the upgraded form,
    /// and it is what the request must be sent to.
    ///
    /// # The three things §8.3 says about the port, and the one everyone
    /// gets wrong
    ///
    /// - no explicit port — *"the UA MUST NOT add one"*, so the upgraded
    ///   URI is portless and 443 arrives as `https`'s default;
    /// - an explicit `:80` — *"the UA MUST convert the port component to
    ///   be '443'"*;
    /// - **any other explicit port — *"the port component value MUST be
    ///   preserved"***. So `http://a.test:8080/` upgrades to
    ///   `https://a.test:8080/` and **not** to `:443`. The RFC's own NOTE
    ///   says why this looks wrong and is not: *"These steps ensure that
    ///   the HSTS Policy applies to HTTP over any TCP port of an HSTS
    ///   Host"* — and warns, in the next NOTE, that such a request is
    ///   *"reasonably likely"* to fail because a plain HTTP server is
    ///   listening there. Failing to reach it is the intended outcome;
    ///   reaching it in clear text is not.
    pub async fn upgrade(&self, uri: &http::Uri, now: SystemTime) -> Option<http::Uri> {
        // §8.3's trigger is "any 'http' URI". `https` is already where we
        // want to be, and any other scheme is not this RFC's subject —
        // see this module's doc on what it does not do.
        if uri.scheme_str() != Some("http") {
            return None;
        }
        let host = uri.host()?;
        // §8.3 step 3: an IP literal matches no known host. Checked here
        // as well as when noting, because a store handed to us may hold
        // an entry §8.1.1 would never have written.
        if is_ip_literal(host) {
            return None;
        }
        if !self.is_known(host, now).await {
            return None;
        }

        let mut parts = uri.clone().into_parts();
        parts.scheme = Some(http::uri::Scheme::HTTPS);
        // An authority with no host cannot have got past `uri.host()`
        // above; the port is what this rebuilds.
        let authority = parts.authority.as_ref()?;
        let rebuilt = match authority.port_u16() {
            // "MUST convert the port component to be '443'" — and it is
            // written out rather than dropped, because dropping it would
            // also be correct here and would make the two branches below
            // one, hiding that the RFC asks for a *conversion* and not a
            // normalisation.
            Some(80) => format!("{}:443", authority.host()),
            // "the port component value MUST be preserved"
            Some(other) => format!("{}:{other}", authority.host()),
            // "MUST NOT add one"
            None => authority.host().to_owned(),
        };
        parts.authority = Some(rebuilt.parse().ok()?);
        // A URI with an authority and no path is not a valid `http::Uri`
        // to rebuild; `into_parts` on an absolute URI always yields one,
        // and this is the belt for the case it does not.
        if parts.path_and_query.is_none() {
            parts.path_and_query = Some(http::uri::PathAndQuery::from_static("/"));
        }
        http::Uri::from_parts(parts).ok()
    }

    /// §8.2 and §8.3 step 5: is there a policy covering `host`?
    ///
    /// **The answer is an existence question rather than a search for the
    /// best match, and that is the RFC's own algorithm rather than a
    /// simplification.** §8.3 step 5 reads: *"If, when performing domain
    /// name matching any superdomain match with an asserted
    /// includeSubDomains directive is found, or, if no superdomain
    /// matches with asserted includeSubDomains directives are found and a
    /// congruent match is found (with or without an asserted
    /// includeSubDomains directive)"* — so **any** covering entry
    /// upgrades, and a more specific entry cannot veto a less specific
    /// one.
    ///
    /// That is worth stating because it is the opposite of what the same
    /// shape means one module over: in RFC 6265 a more specific cookie
    /// domain wins, and a reader carrying that intuition here would
    /// expect `b.example.com` *without* `includeSubDomains` to stop
    /// `example.com` *with* it from covering `a.b.example.com`. It does
    /// not. Checked against the errata before it was relied on — five
    /// reported for RFC 6797, all rejected, none touching §8.2 or §8.3.
    ///
    /// Expiry is applied here rather than by a sweep, which is §8.1.1's
    /// *"MUST evict all expired Known HSTS Hosts"* read as *before one
    /// can affect an answer*.
    async fn is_known(&self, host: &str, now: SystemTime) -> bool {
        let candidates = candidate_domains(host);
        let found = self.store.get(&candidates).await;

        let mut expired: Vec<String> = Vec::new();
        let mut covered = false;
        for entry in found {
            if entry.expires_at() <= now {
                expired.push(entry.domain().to_owned());
                continue;
            }
            // The congruent match is `candidates[0]` — the host itself —
            // and applies whatever it says; every other candidate is a
            // superdomain match and applies only with
            // `includeSubDomains`.
            let congruent = entry.domain() == candidates[0];
            if congruent || entry.include_subdomains() {
                covered = true;
            }
        }
        for domain in expired {
            self.store.remove(&domain).await;
        }
        covered
    }

    /// §8.1: note what this response's `Strict-Transport-Security` field
    /// asserts, if anything.
    ///
    /// `secure` is whether the response arrived over a secure transport —
    /// §8.1's *"If an HTTP response is received over insecure transport,
    /// the UA MUST ignore any present STS header field(s)"*. It is a
    /// parameter rather than something read off `uri` because that is the
    /// caller's fact: this module is clockless and transport-less, and
    /// what counts as secure is [`Client`](crate::Client)'s to decide.
    ///
    /// # Three refusals, and each is a `MUST`
    ///
    /// - **Not secure** — §8.1, above. This is the one that matters: an
    ///   attacker who can rewrite a plaintext response could otherwise
    ///   assert a policy for a host they do not own, or a `max-age=0`
    ///   that deletes one the host really did assert.
    /// - **An IP literal** — §8.1.1's *"MUST NOT note this host as a
    ///   Known HSTS Host"*.
    /// - **A malformed field value** — §6.1 requirement 4, applied by
    ///   [`Directives::parse`].
    ///
    /// # Only the first header field
    ///
    /// §8.1: *"If a UA receives more than one STS header field in an HTTP
    /// response message over secure transport, then the UA MUST process
    /// only the first such header field."* That is a bespoke HSTS rule
    /// rather than HTTP's ordinary list-combining, and it is the reason
    /// this takes the whole [`HeaderMap`](http::HeaderMap) rather than
    /// one value: a caller passing `get_all(..)` and combining would
    /// implement the rule the RFC replaced.
    pub async fn note(
        &self,
        uri: &http::Uri,
        headers: &http::HeaderMap,
        secure: bool,
        now: SystemTime,
    ) {
        // §8.1: ignore everything over insecure transport.
        if !secure {
            return;
        }
        let Some(host) = uri.host() else {
            return;
        };
        // §8.1.1: an IP literal is never noted.
        if is_ip_literal(host) {
            return;
        }
        // §8.1: "process only the first such header field".
        let Some(value) = headers.get("strict-transport-security") else {
            return;
        };
        let Ok(value) = value.to_str() else {
            return;
        };
        let Some(directives) = Directives::parse(value) else {
            return;
        };

        let domain = host.to_ascii_lowercase();
        if directives.max_age == 0 {
            // §6.1.1: "signals the UA to cease regarding the host as a
            // Known HSTS Host, including the includeSubDomains directive
            // (if asserted for that HSTS Host)".
            //
            // Only this host's own entry: §8.1.1 is explicit that a
            // superdomain's policy is not ours to touch — "The UA MUST
            // NOT modify the expiry time or the includeSubDomains
            // directive of any superdomain matched Known HSTS Host" — so
            // `a.example.com` sending `max-age=0` does not un-protect
            // `example.com`, and the next request to `a.example.com` is
            // still upgraded by its parent's policy. That reads as a bug
            // and is the RFC working: a host may withdraw its own
            // assertion and not its parent's.
            self.store.remove(&domain).await;
            return;
        }

        let expires_at = now
            .checked_add(MAX_AGE_CAP.min(Duration::from_secs(directives.max_age)))
            // Unreachable with the cap above, and checked rather than
            // trusted: the alternative is a panic on a number a peer
            // chose.
            .unwrap_or(now);
        self.store
            .put(Entry::new(
                domain,
                expires_at,
                directives.include_subdomains,
            ))
            .await;
    }
}

/// §8.1.1 and §8.3 step 3's test: does the host match RFC 3986 §3.2.2's
/// `IP-literal` or `IPv4address`?
///
/// A syntactic question rather than a DNS one, which is what the RFC
/// asks. `http::Uri::host` leaves the brackets on an IPv6 literal, so
/// they are stripped before parsing — without that, `[::1]` reads as a
/// name and could be noted as a known host.
fn is_ip_literal(host: &str) -> bool {
    let bare = host
        .strip_prefix('[')
        .and_then(|h| h.strip_suffix(']'))
        .unwrap_or(host);
    bare.parse::<std::net::IpAddr>().is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures_executor::block_on;

    fn uri(s: &str) -> http::Uri {
        s.parse().unwrap()
    }

    fn at(secs: u64) -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_secs(secs)
    }

    fn headers(v: &str) -> http::HeaderMap {
        let mut h = http::HeaderMap::new();
        h.insert("strict-transport-security", v.parse().unwrap());
        h
    }

    /// Note a policy from `https://<host>/` and hand back the rules.
    fn known(host: &str, value: &str) -> Hsts<MemoryStore> {
        let h = Hsts::new();
        block_on(h.note(
            &uri(&format!("https://{host}/")),
            &headers(value),
            true,
            at(0),
        ));
        h
    }

    // ---- §8.2, the enumeration --------------------------------------

    #[test]
    fn candidates_are_the_host_and_every_superdomain() {
        assert_eq!(
            candidate_domains("qaz.bar.foo.example.com"),
            // RFC 6797 §8.2's own example, which lists
            // `bar.foo.example.com` and `foo.example.com` as superdomain
            // matches for this name.
            vec![
                "qaz.bar.foo.example.com",
                "bar.foo.example.com",
                "foo.example.com",
                "example.com",
                "com",
            ]
        );
    }

    #[test]
    fn candidates_are_lowercased_because_8_2_matches_case_insensitively() {
        assert_eq!(
            candidate_domains("A.Example.COM"),
            vec!["a.example.com", "example.com", "com"]
        );
    }

    // ---- §6.1, the grammar ------------------------------------------

    #[test]
    fn a_plain_max_age_is_the_common_deployment() {
        assert_eq!(
            Directives::parse("max-age=31536000"),
            Some(Directives {
                max_age: 31_536_000,
                include_subdomains: false
            })
        );
    }

    #[test]
    fn directive_names_are_case_insensitive() {
        // §6.1 requirement 3.
        assert_eq!(
            Directives::parse("MAX-AGE=1; INCLUDESUBDOMAINS"),
            Some(Directives {
                max_age: 1,
                include_subdomains: true
            })
        );
    }

    #[test]
    fn order_is_not_significant() {
        // §6.1 requirement 1.
        assert_eq!(
            Directives::parse("includeSubDomains; max-age=1"),
            Directives::parse("max-age=1; includeSubDomains")
        );
    }

    #[test]
    fn a_quoted_max_age_is_unescaped_first() {
        // §6.1.1: "after quoted-string unescaping, if necessary".
        assert_eq!(Directives::parse(r#"max-age="600""#).unwrap().max_age, 600);
    }

    #[test]
    fn an_unrecognised_directive_is_ignored_and_the_rest_processed() {
        // §6.1 requirement 5 — and `preload` is the one that matters,
        // because it is not in RFC 6797 and is on most real deployments.
        assert_eq!(
            Directives::parse("max-age=1; includeSubDomains; preload"),
            Some(Directives {
                max_age: 1,
                include_subdomains: true
            })
        );
    }

    #[rstest::rstest]
    // §6.1.1: `max-age` is REQUIRED.
    #[case("includeSubDomains")]
    #[case("")]
    // §6.1.1: the value is `delta-seconds`, i.e. `1*DIGIT`.
    #[case("max-age=abc")]
    #[case("max-age")]
    #[case("max-age=")]
    #[case("max-age=-1")]
    #[case("max-age=1.5")]
    // §6.1 requirement 2: "All directives MUST appear only once".
    #[case("max-age=1; max-age=2")]
    #[case("max-age=1; includeSubDomains; includeSubDomains")]
    // §6.1.2: "a valueless directive".
    #[case("max-age=1; includeSubDomains=1")]
    // §6.1 requirement 4's "or other header field value data".
    #[case("max-age=1 rubbish")]
    #[case(r#"max-age="600"#)]
    fn a_malformed_value_is_ignored_whole(#[case] value: &str) {
        assert_eq!(Directives::parse(value), None, "{value:?} should not parse");
    }

    #[test]
    fn an_absurd_max_age_parses_rather_than_being_refused() {
        // The production is `1*DIGIT` with no ceiling, so this conforms;
        // what it *means* is the rules layer's question. Saturating here
        // is what lets `MAX_AGE_CAP` be the only place a bound is
        // applied.
        assert_eq!(
            Directives::parse("max-age=99999999999999999999")
                .unwrap()
                .max_age,
            u64::MAX
        );
    }

    #[test]
    fn an_empty_directive_conforms_because_the_grammar_brackets_it() {
        // §6.1: `[ directive ] *( ";" [ directive ] )`.
        assert_eq!(Directives::parse("max-age=1;").unwrap().max_age, 1);
        assert_eq!(Directives::parse(";max-age=1").unwrap().max_age, 1);
        assert!(
            Directives::parse("max-age=1;;includeSubDomains")
                .unwrap()
                .include_subdomains
        );
    }

    // ---- §8.3, the upgrade ------------------------------------------

    #[test]
    fn a_known_host_upgrades_and_gains_no_port() {
        // §8.3: "the UA MUST NOT add one".
        let h = known("a.test", "max-age=100");
        assert_eq!(
            block_on(h.upgrade(&uri("http://a.test/x?y=1"), at(1))),
            Some(uri("https://a.test/x?y=1"))
        );
    }

    #[test]
    fn an_explicit_port_80_becomes_443() {
        // §8.3: "the UA MUST convert the port component to be '443'".
        let h = known("a.test", "max-age=100");
        assert_eq!(
            block_on(h.upgrade(&uri("http://a.test:80/"), at(1))),
            Some(uri("https://a.test:443/"))
        );
    }

    #[test]
    fn any_other_explicit_port_is_preserved_rather_than_forced_to_443() {
        // §8.3: "the port component value MUST be preserved" — the rule
        // implementations most often get wrong, and the RFC's own NOTE
        // says the resulting request is "reasonably likely" to fail.
        // Failing to reach it is the intended outcome; reaching it in
        // clear text is not.
        let h = known("a.test", "max-age=100");
        assert_eq!(
            block_on(h.upgrade(&uri("http://a.test:8080/"), at(1))),
            Some(uri("https://a.test:8080/"))
        );
    }

    #[test]
    fn an_unknown_host_is_left_alone() {
        let h = Hsts::new();
        assert_eq!(block_on(h.upgrade(&uri("http://a.test/"), at(1))), None);
    }

    #[test]
    fn an_https_uri_is_left_alone() {
        // §8.3's trigger is "any 'http' URI".
        let h = known("a.test", "max-age=100");
        assert_eq!(block_on(h.upgrade(&uri("https://a.test/"), at(1))), None);
    }

    #[test]
    fn an_expired_policy_does_not_upgrade_and_is_evicted() {
        // §8.1.1: "MUST evict all expired Known HSTS Hosts".
        let h = known("a.test", "max-age=100");
        assert_eq!(block_on(h.upgrade(&uri("http://a.test/"), at(101))), None);
        // Evicted rather than merely ignored: the store no longer holds
        // it, which is what makes the eviction observable at all.
        assert!(block_on(h.store().get(&["a.test".to_owned()])).is_empty());
    }

    // ---- §8.2 + §8.3 step 5, the precedence -------------------------

    #[test]
    fn a_subdomain_is_not_covered_without_include_subdomains() {
        let h = known("example.com", "max-age=100");
        assert_eq!(
            block_on(h.upgrade(&uri("http://a.example.com/"), at(1))),
            None
        );
    }

    #[test]
    fn a_subdomain_is_covered_with_include_subdomains() {
        let h = known("example.com", "max-age=100; includeSubDomains");
        assert_eq!(
            block_on(h.upgrade(&uri("http://a.b.example.com/"), at(1))),
            Some(uri("https://a.b.example.com/"))
        );
    }

    /// §8.3 step 5, and the rule a reader arriving from RFC 6265 will
    /// expect to go the other way.
    ///
    /// *"If … any superdomain match with an asserted includeSubDomains
    /// directive is found, or, if no superdomain matches with asserted
    /// includeSubDomains directives are found and a congruent match is
    /// found"* — so the ancestor's assertion is checked first and wins.
    /// A more specific entry **cannot veto** a less specific one, which
    /// is the opposite of a cookie's `Domain`.
    #[test]
    fn an_ancestors_include_subdomains_beats_a_nearer_entry_without_it() {
        let h = known("example.com", "max-age=100; includeSubDomains");
        block_on(h.note(
            &uri("https://b.example.com/"),
            &headers("max-age=100"),
            true,
            at(0),
        ));
        assert_eq!(
            block_on(h.upgrade(&uri("http://a.b.example.com/"), at(1))),
            Some(uri("https://a.b.example.com/"))
        );
    }

    // ---- §8.1, noting -----------------------------------------------

    #[test]
    fn a_header_over_insecure_transport_is_ignored() {
        // §8.1, and the one that matters: an attacker who can rewrite a
        // plaintext response could otherwise assert a policy for a host
        // they do not own.
        let h = Hsts::new();
        block_on(h.note(
            &uri("http://a.test/"),
            &headers("max-age=100"),
            false,
            at(0),
        ));
        assert_eq!(block_on(h.upgrade(&uri("http://a.test/"), at(1))), None);
    }

    #[test]
    fn max_age_zero_removes_the_policy() {
        // §6.1.1: "signals the UA to cease regarding the host as a Known
        // HSTS Host".
        let h = known("a.test", "max-age=100");
        block_on(h.note(&uri("https://a.test/"), &headers("max-age=0"), true, at(1)));
        // **The row is gone, and this is read before `upgrade` rather
        // than after it** — which two mutations were needed to get
        // right. With the deletion branch disabled, `max-age=0` stores
        // an entry expiring at `now + 0`; asserting only that the next
        // request is not upgraded is green, because *expiry* declines it
        // too, and asserting an empty store *after* `upgrade` is green
        // as well, because `upgrade`'s own §8.1.1 eviction has by then
        // swept the entry the deletion should have removed. In memory
        // all three states look alike; in a store that outlives the
        // process, "never written" and "written, then swept on the next
        // lookup that happens to occur" are different rows.
        assert!(
            block_on(h.store().get(&["a.test".to_owned()])).is_empty(),
            "max-age=0 left an entry behind"
        );
        assert_eq!(block_on(h.upgrade(&uri("http://a.test/"), at(2))), None);
    }

    #[test]
    fn max_age_zero_does_not_touch_a_superdomains_policy() {
        // §8.1.1: "The UA MUST NOT modify the expiry time or the
        // includeSubDomains directive of any superdomain matched Known
        // HSTS Host." So a child may withdraw its own assertion and not
        // its parent's — and the child stays covered.
        let h = known("example.com", "max-age=100; includeSubDomains");
        block_on(h.note(
            &uri("https://a.example.com/"),
            &headers("max-age=0"),
            true,
            at(1),
        ));
        assert_eq!(
            block_on(h.upgrade(&uri("http://a.example.com/"), at(2))),
            Some(uri("https://a.example.com/"))
        );
    }

    #[test]
    fn a_later_header_replaces_the_earlier_one() {
        // §8.1's second bullet: "Update the UA's cached information".
        let h = known("a.test", "max-age=100; includeSubDomains");
        block_on(h.note(
            &uri("https://a.test/"),
            &headers("max-age=100"),
            true,
            at(0),
        ));
        // `includeSubDomains` is gone, so the subdomain is no longer
        // covered — a replacement rather than a merge.
        assert_eq!(block_on(h.upgrade(&uri("http://x.a.test/"), at(1))), None);
        assert!(block_on(h.upgrade(&uri("http://a.test/"), at(1))).is_some());
    }

    #[test]
    fn an_absurd_max_age_is_capped_rather_than_overflowing() {
        // `SystemTime` has no `MAX` to saturate towards, so the sum is
        // never computed — `MAX_AGE_CAP` is applied first.
        let h = known("a.test", "max-age=99999999999999999999");
        let held = block_on(h.store().get(&["a.test".to_owned()]));
        assert_eq!(held.len(), 1);
        assert_eq!(held[0].expires_at(), at(0) + MAX_AGE_CAP);
        // And it is still a policy, rather than one that expired the
        // instant it arrived — which is the direction that silently
        // loses it.
        assert!(block_on(h.upgrade(&uri("http://a.test/"), at(1))).is_some());
    }

    #[test]
    fn a_malformed_header_leaves_an_existing_policy_alone() {
        // §6.1 requirement 4 is "ignore the header field", not "forget
        // what you knew".
        let h = known("a.test", "max-age=100");
        block_on(h.note(
            &uri("https://a.test/"),
            &headers("max-age=abc"),
            true,
            at(1),
        ));
        assert!(block_on(h.upgrade(&uri("http://a.test/"), at(2))).is_some());
    }

    #[test]
    fn only_the_first_header_field_is_processed() {
        // §8.1: "the UA MUST process only the first such header field."
        // A bespoke HSTS rule rather than HTTP's list-combining, which is
        // why `note` takes the whole `HeaderMap`.
        let h = Hsts::new();
        let mut hm = http::HeaderMap::new();
        hm.append("strict-transport-security", "max-age=100".parse().unwrap());
        hm.append("strict-transport-security", "max-age=0".parse().unwrap());
        block_on(h.note(&uri("https://a.test/"), &hm, true, at(0)));
        // The second field would have deleted it; only the first counts.
        assert!(block_on(h.upgrade(&uri("http://a.test/"), at(1))).is_some());
    }

    #[test]
    fn clear_forgets_every_policy() {
        let h = known("a.test", "max-age=100");
        block_on(h.note(
            &uri("https://b.test/"),
            &headers("max-age=100"),
            true,
            at(0),
        ));
        // The control comes first: without it this passes for a `note`
        // that never stored anything.
        assert!(block_on(h.upgrade(&uri("http://a.test/"), at(1))).is_some());
        assert!(block_on(h.upgrade(&uri("http://b.test/"), at(1))).is_some());

        block_on(h.clear());

        assert_eq!(block_on(h.upgrade(&uri("http://a.test/"), at(1))), None);
        assert_eq!(block_on(h.upgrade(&uri("http://b.test/"), at(1))), None);
        // The rows are gone rather than merely unmatched — the
        // distinction `max_age_zero_removes_the_policy` took three
        // attempts to pin, applied here for free because there is no
        // expiry sweep in the way.
        assert!(block_on(h.store().get(&["a.test".to_owned(), "b.test".to_owned()])).is_empty());
    }

    // ---- §8.1.1 and §8.3 step 3, IP literals -------------------------

    #[rstest::rstest]
    #[case("127.0.0.1")]
    #[case("[::1]")]
    fn an_ip_literal_is_never_noted(#[case] host: &str) {
        // §8.1.1: "the UA MUST NOT note this host as a Known HSTS Host".
        //
        // **Asserted on the store rather than on `upgrade`**, and the
        // difference is a mutation that survived the first version of
        // this test: `upgrade` refuses an IP literal on its own account
        // (§8.3 step 3), so reading the refusal there is green even for
        // a client that notes every literal it meets. What §8.1.1
        // forbids is the *noting*, and the store is where that shows.
        let h = Hsts::new();
        block_on(h.note(
            &uri(&format!("https://{host}/")),
            &headers("max-age=100"),
            true,
            at(0),
        ));
        let bare = host.trim_start_matches('[').trim_end_matches(']');
        assert!(
            block_on(h.store().get(&[host.to_owned(), bare.to_owned()])).is_empty(),
            "{host} was noted as a known HSTS host"
        );
        assert_eq!(
            block_on(h.upgrade(&uri(&format!("http://{host}/")), at(1))),
            None
        );
    }

    #[test]
    fn an_ip_literal_is_not_upgraded_even_by_a_store_that_holds_one() {
        // §8.3 step 3, checked at the reading end too: a store handed to
        // us may hold an entry §8.1.1 would never have written.
        let store = MemoryStore::new();
        block_on(store.put(Entry::new("127.0.0.1", at(100), false)));
        let h = Hsts::with_store(store);
        assert_eq!(block_on(h.upgrade(&uri("http://127.0.0.1/"), at(1))), None);
    }

    #[test]
    fn an_ipv6_literal_is_recognised_through_its_brackets() {
        // `http::Uri::host` leaves the brackets on, so without stripping
        // them `[::1]` reads as a name and would be noted.
        assert!(is_ip_literal("[::1]"));
        assert!(is_ip_literal("::1"));
        assert!(is_ip_literal("127.0.0.1"));
        assert!(!is_ip_literal("a.test"));
        assert!(!is_ip_literal("127.0.0.1.test"));
    }
}
