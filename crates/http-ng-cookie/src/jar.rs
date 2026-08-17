//! The storage model (RFC 6265bis §5.7) and retrieval (§5.4).
//!
//! This is where the request URI, the clock and the public suffix list meet
//! the parsed header. Everything that can be decided from the header alone
//! already was, in `parse.rs`.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use http::{HeaderMap, HeaderValue, Uri};

use crate::matching::{
    canonical_host, default_path, domain_matches, is_ip_literal, is_secure_request, path_matches,
    request_path,
};
use crate::parse::{ParseError, SameSite, SetCookie};
use crate::suffix::{BuiltinList, PublicSuffixList};

/// RFC 6265bis §5.5: an expiry further out than this is capped to it.
///
/// 400 days, the figure the draft settled on and the one Chrome ships. It
/// also removes the arithmetic hazard from the other end — a server sending
/// `Max-Age=9223372036854775807` cannot overflow anything, because the sum
/// is never computed.
const MAX_EXPIRY: Duration = Duration::from_secs(400 * 24 * 60 * 60);

/// How large the jar is allowed to get.
///
/// A jar with no bound is a memory-exhaustion bug with a server on the
/// other end of it, so the bound is part of the type rather than a later
/// hardening. The defaults are RFC 6265 §6.1's minimums, which is the
/// smallest set of numbers that cannot be called arbitrary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    /// Total cookies across all domains. RFC 6265 §6.1: at least 3000.
    pub max_cookies: usize,
    /// Cookies for any one domain. RFC 6265 §6.1: at least 50.
    pub max_per_domain: usize,
    /// `name.len() + value.len()`. RFC 6265 §6.1: at least 4096 bytes.
    /// A larger cookie is refused outright rather than truncated, because a
    /// truncated cookie is a wrong cookie.
    pub max_name_value_bytes: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_cookies: 3000,
            max_per_domain: 50,
            max_name_value_bytes: 4096,
        }
    }
}

/// One stored cookie.
///
/// Fields are private and read through accessors: the invariants that make
/// `domain` and `path` safe to match against — lowercased, no leading dot,
/// path always absolute — are established once in
/// [`CookieJar::store`] and would be a caller's problem if the fields were
/// public.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cookie {
    name: String,
    value: String,
    domain: String,
    path: String,
    expires: Option<SystemTime>,
    creation: SystemTime,
    last_access: SystemTime,
    /// Insertion order, and the tiebreak for §5.4's "earlier creation-time
    /// first" when two cookies share a `SystemTime` — which they do
    /// routinely, because one response's `Set-Cookie` headers are all
    /// stored with the same `now`.
    seq: u64,
    host_only: bool,
    persistent: bool,
    secure: bool,
    http_only: bool,
    same_site: Option<SameSite>,
}

impl Cookie {
    pub fn name(&self) -> &str {
        &self.name
    }
    pub fn value(&self) -> &str {
        &self.value
    }
    pub fn domain(&self) -> &str {
        &self.domain
    }
    pub fn path(&self) -> &str {
        &self.path
    }
    /// `None` for a session cookie — one with neither `Expires` nor
    /// `Max-Age`.
    pub fn expires(&self) -> Option<SystemTime> {
        self.expires
    }
    /// Whether this cookie goes only to the exact host that set it (no
    /// usable `Domain` attribute), rather than to that host's subdomains.
    pub fn host_only(&self) -> bool {
        self.host_only
    }
    pub fn persistent(&self) -> bool {
        self.persistent
    }
    pub fn secure(&self) -> bool {
        self.secure
    }
    pub fn http_only(&self) -> bool {
        self.http_only
    }
    /// The `SameSite` attribute as sent. Nothing in this crate enforces it
    /// — see [`SameSite`].
    pub fn same_site(&self) -> Option<SameSite> {
        self.same_site
    }
    pub fn creation(&self) -> SystemTime {
        self.creation
    }

    fn is_expired(&self, now: SystemTime) -> bool {
        self.expires.is_some_and(|e| e <= now)
    }
}

/// Why a `Set-Cookie` was not stored.
///
/// Everything here is a refusal the RFC requires, and each one is a place a
/// cookie would otherwise end up somewhere it does not belong. They are
/// reported rather than swallowed because "the cookie silently did not
/// arrive" is among the harder things to debug in an HTTP client.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum Rejected {
    /// The header is not a cookie at all — see [`ParseError`].
    #[error("malformed Set-Cookie: {0}")]
    Malformed(#[from] ParseError),
    /// The request URI has no host, so there is nothing to scope a cookie
    /// to.
    #[error("the request URI has no host")]
    NoHost,
    /// §5.7: the `Domain` attribute does not domain-match the request host.
    /// The sibling-domain case (`Domain=example.com` from
    /// `notexample.com`) lands here.
    #[error("Domain={domain} does not match request host {host}")]
    DomainMismatch { domain: String, host: String },
    /// §5.7: the `Domain` attribute is a public suffix and is not the
    /// request host itself — `Domain=co.uk`, which would put one cookie on
    /// every registrant under `.uk`.
    #[error("Domain={domain} is a public suffix")]
    DomainIsPublicSuffix { domain: String },
    /// The same refusal, from a build with no list to consult: this crate
    /// without the `public-suffix` feature, or a jar built on
    /// [`NoList`](crate::NoList). Every `Domain` attribute is refused
    /// there, so the cause is the build rather than the cookie.
    #[error("Domain={domain} cannot be checked: this build has no public suffix list")]
    NoPublicSuffixList { domain: String },
    /// §5.7: a `Secure` cookie offered over a scheme that is not secure.
    #[error("a Secure cookie was offered over a non-secure request")]
    SecureOverInsecure,
    /// §4.1.3: a `__Secure-` name without `Secure`, or over a non-secure
    /// request.
    #[error("the __Secure- prefix requires a Secure cookie over a secure request")]
    SecurePrefix,
    /// §4.1.3: a `__Host-` name that is not `Secure`, not host-only, or
    /// not scoped to `/`.
    #[error("the __Host- prefix requires Secure, no Domain, and Path=/")]
    HostPrefix,
    /// [`Limits::max_name_value_bytes`].
    #[error("name and value are {bytes} bytes, over the {limit}-byte limit")]
    TooLarge { bytes: usize, limit: usize },
}

/// A cookie jar: parse, store, expire and hand back.
///
/// Sans-io and clockless — every method that needs the time takes it as a
/// `now` parameter, the same rule `http-ng-proto` runs under. Nothing here
/// reads a clock, opens a socket or spawns anything, which is what makes
/// "the same cookie behaviour on every backend" a structural fact rather
/// than a consequence of everyone happening to call the same client.
///
/// ```
/// use std::time::SystemTime;
/// use http::{HeaderValue, Uri};
/// use http_ng_cookie::CookieJar;
///
/// let mut jar = CookieJar::new();
/// let uri: Uri = "https://www.example.com/app".parse().unwrap();
/// let now = SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_700_000_000);
///
/// jar.store(&uri, &HeaderValue::from_static("sid=abc; Domain=example.com"), now).unwrap();
///
/// let other: Uri = "https://api.example.com/v1".parse().unwrap();
/// assert_eq!(jar.cookie_header(&other, now).unwrap(), "sid=abc");
/// ```
///
/// The type parameter is the public suffix list; see
/// [`PublicSuffixList`] for why it is a seam and not a fixed table.
#[derive(Debug, Clone)]
pub struct CookieJar<P = BuiltinList> {
    cookies: Vec<Cookie>,
    limits: Limits,
    suffixes: P,
    next_seq: u64,
}

impl Default for CookieJar<BuiltinList> {
    fn default() -> Self {
        Self::new()
    }
}

impl CookieJar<BuiltinList> {
    /// A jar with default [`Limits`] and the compiled-in public suffix
    /// list.
    pub fn new() -> Self {
        Self::with_public_suffix_list(BuiltinList)
    }
}

impl<P: PublicSuffixList> CookieJar<P> {
    /// A jar over a caller-supplied list — a fresher snapshot than the one
    /// this crate was built with, or [`NoList`](crate::NoList).
    pub fn with_public_suffix_list(suffixes: P) -> Self {
        Self {
            cookies: Vec::new(),
            limits: Limits::default(),
            suffixes,
            next_seq: 0,
        }
    }

    /// The same jar over a different public suffix list — every cookie,
    /// both bounds and the sequence counter carried across.
    ///
    /// The list is a seam and the jar is storage, so the two should be
    /// separable after construction as well as at it. What actually asked
    /// for this is `http-ng`, which holds one jar type for every caller
    /// and so must erase `P`; the operation is not specific to that —
    /// swapping a stale compiled-in snapshot for a freshly fetched list
    /// without losing the cookies is the same call.
    ///
    /// Rebuilding by iteration would not do: `next_seq` is what orders
    /// cookies of equal path length in the `Cookie` header, and a jar
    /// rebuilt from `iter` would restart it.
    pub fn map_suffixes<Q>(self, f: impl FnOnce(P) -> Q) -> CookieJar<Q> {
        CookieJar {
            cookies: self.cookies,
            limits: self.limits,
            suffixes: f(self.suffixes),
            next_seq: self.next_seq,
        }
    }

    /// Replace the bounds. Applied on the next [`store`](Self::store); it
    /// does not evict what is already held.
    #[must_use]
    pub fn with_limits(mut self, limits: Limits) -> Self {
        self.limits = limits;
        self
    }

    pub fn limits(&self) -> Limits {
        self.limits
    }

    /// How many cookies are held, expired ones included — they are removed
    /// on the next [`store`](Self::store) that names a `now` past them, not
    /// by a background sweep this crate has no way to run.
    pub fn len(&self) -> usize {
        self.cookies.len()
    }

    pub fn is_empty(&self) -> bool {
        self.cookies.is_empty()
    }

    pub fn clear(&mut self) {
        self.cookies.clear();
    }

    /// Every cookie held, in insertion order. For inspection and
    /// persistence; retrieval for a request is
    /// [`matching`](Self::matching).
    pub fn iter(&self) -> impl Iterator<Item = &Cookie> {
        self.cookies.iter()
    }

    /// Store one `Set-Cookie`, per RFC 6265bis §5.7.
    pub fn store(
        &mut self,
        uri: &Uri,
        set_cookie: &HeaderValue,
        now: SystemTime,
    ) -> Result<(), Rejected> {
        let host = canonical_host(uri).ok_or(Rejected::NoHost)?;
        let parsed = SetCookie::parse(set_cookie.as_bytes())?;

        let bytes = parsed.name.len() + parsed.value.len();
        if bytes > self.limits.max_name_value_bytes {
            return Err(Rejected::TooLarge {
                bytes,
                limit: self.limits.max_name_value_bytes,
            });
        }

        let (domain, host_only) = self.scope_domain(&host, parsed.domain.as_deref())?;

        let path = match parsed.path {
            Some(ref p) => p.clone(),
            None => default_path(request_path(uri)),
        };

        let secure_request = is_secure_request(uri);
        // §5.7: a `Secure` cookie may only be *set* over a secure request.
        // Without this, plain http on a shared network can plant the cookie
        // the https site will then send back.
        if parsed.secure && !secure_request {
            return Err(Rejected::SecureOverInsecure);
        }

        // §4.1.3, the two name prefixes. Compared case-insensitively, as
        // 6265bis specifies and as browsers implement.
        let lower = parsed.name.to_ascii_lowercase();
        if lower.starts_with("__secure-") && !(parsed.secure && secure_request) {
            return Err(Rejected::SecurePrefix);
        }
        if lower.starts_with("__host-")
            && !(parsed.secure && secure_request && host_only && path == "/")
        {
            return Err(Rejected::HostPrefix);
        }

        let (expires, persistent) = expiry(&parsed, now);

        let mut cookie = Cookie {
            name: parsed.name,
            value: parsed.value,
            domain,
            path,
            expires,
            creation: now,
            last_access: now,
            seq: self.next_seq,
            host_only,
            persistent,
            secure: parsed.secure,
            http_only: parsed.http_only,
            same_site: parsed.same_site,
        };

        match self.position_of(&cookie) {
            Some(i) => {
                // §5.7: a replacement keeps the *old* cookie's creation
                // time. Without it, refreshing a session cookie would move
                // it to the back of §5.4's ordering and change which of two
                // equally specific cookies a server sees first.
                cookie.creation = self.cookies[i].creation;
                cookie.seq = self.cookies[i].seq;
                self.cookies[i] = cookie;
            }
            None => {
                self.next_seq += 1;
                self.make_room_for(&cookie, now);
                self.cookies.push(cookie);
            }
        }

        // A `Max-Age=0` or a past `Expires` is a deletion: it is stored as
        // an already-expired cookie by the steps above and removed here, so
        // deletion is the same code path as expiry rather than a second
        // one that could disagree with it.
        self.cookies.retain(|c| !c.is_expired(now));
        Ok(())
    }

    /// Store every `Set-Cookie` in a response's headers, returning how many
    /// were accepted.
    ///
    /// Refusals are dropped rather than returned, because one bad
    /// `Set-Cookie` must not stop the others from being stored — that is
    /// what a browser does and what a server assumes. Use
    /// [`store`](Self::store) per header when the reasons matter.
    pub fn store_response(&mut self, uri: &Uri, headers: &HeaderMap, now: SystemTime) -> usize {
        headers
            .get_all(http::header::SET_COOKIE)
            .iter()
            .filter(|value| self.store(uri, value, now).is_ok())
            .count()
    }

    /// The cookies that apply to `uri`, in the order RFC 6265bis §5.4
    /// requires: longer paths first, then earlier creation time.
    ///
    /// Read-only — it does not update last-access times, so it cannot
    /// change which cookie the bound would evict next.
    /// [`cookie_header`](Self::cookie_header) is the one that does.
    pub fn matching(&self, uri: &Uri, now: SystemTime) -> Vec<&Cookie> {
        let mut out: Vec<&Cookie> = self
            .indices_matching(uri, now)
            .into_iter()
            .map(|i| &self.cookies[i])
            .collect();
        out.sort_by(|a, b| {
            b.path
                .len()
                .cmp(&a.path.len())
                .then(a.creation.cmp(&b.creation))
                .then(a.seq.cmp(&b.seq))
        });
        out
    }

    /// The `Cookie` request header for `uri`, or `None` when nothing
    /// matches.
    ///
    /// Takes `&mut self` because §5.4 updates each returned cookie's
    /// last-access time, which is what [`Limits`] evicts on.
    pub fn cookie_header(&mut self, uri: &Uri, now: SystemTime) -> Option<HeaderValue> {
        let mut indices = self.indices_matching(uri, now);
        if indices.is_empty() {
            return None;
        }
        indices.sort_by(|a, b| {
            let (a, b) = (&self.cookies[*a], &self.cookies[*b]);
            b.path
                .len()
                .cmp(&a.path.len())
                .then(a.creation.cmp(&b.creation))
                .then(a.seq.cmp(&b.seq))
        });

        let mut out = Vec::new();
        for i in &indices {
            let cookie = &self.cookies[*i];
            if !out.is_empty() {
                out.extend_from_slice(b"; ");
            }
            out.extend_from_slice(cookie.name.as_bytes());
            out.push(b'=');
            out.extend_from_slice(cookie.value.as_bytes());
        }
        for i in indices {
            self.cookies[i].last_access = now;
        }

        // Every byte here already survived `parse.rs`'s CTL check, so the
        // only way this fails is a bug in that check — which is exactly
        // when silence would be worst.
        HeaderValue::from_bytes(&out).ok()
    }

    fn indices_matching(&self, uri: &Uri, now: SystemTime) -> Vec<usize> {
        let Some(host) = canonical_host(uri) else {
            return Vec::new();
        };
        let path = request_path(uri);
        let secure = is_secure_request(uri);
        self.cookies
            .iter()
            .enumerate()
            .filter(|(_, c)| {
                !c.is_expired(now)
                    && if c.host_only {
                        host == c.domain
                    } else {
                        domain_matches(&host, &c.domain)
                    }
                    && path_matches(path, &c.path)
                    && (!c.secure || secure)
            })
            .map(|(i, _)| i)
            .collect()
    }

    /// RFC 6265bis §5.7's domain steps, in the order the RFC puts them —
    /// the public-suffix check comes **before** the domain-match one,
    /// because `Domain=co.uk` from `www.bbc.co.uk` passes the domain-match
    /// and must still be refused.
    fn scope_domain(
        &self,
        host: &str,
        attribute: Option<&str>,
    ) -> Result<(String, bool), Rejected> {
        let Some(domain) = attribute else {
            return Ok((host.to_owned(), true));
        };

        if is_ip_literal(host) {
            // §5.7 leaves this to domain-match, which refuses everything
            // but equality for an IP literal. Spelled out because the
            // rescue below would otherwise never fire for `Domain=127.0.0.1`
            // on `127.0.0.1`, and the public suffix list has opinions about
            // `1` that are none of its business.
            return if domain == host {
                Ok((host.to_owned(), true))
            } else {
                Err(Rejected::DomainMismatch {
                    domain: domain.to_owned(),
                    host: host.to_owned(),
                })
            };
        }

        if self.suffixes.is_public_suffix(domain) {
            // The one rescue: a `Domain` identical to the request host is
            // downgraded to a host-only cookie rather than refused. This is
            // what keeps `Domain=localhost` on `http://localhost` working —
            // `localhost` is a public suffix by the list's prevailing `*`
            // rule — and it is the only reason a no-list build is usable at
            // all.
            return if domain == host {
                Ok((host.to_owned(), true))
            } else if self.suffixes.has_list() {
                Err(Rejected::DomainIsPublicSuffix {
                    domain: domain.to_owned(),
                })
            } else {
                Err(Rejected::NoPublicSuffixList {
                    domain: domain.to_owned(),
                })
            };
        }

        if !domain_matches(host, domain) {
            return Err(Rejected::DomainMismatch {
                domain: domain.to_owned(),
                host: host.to_owned(),
            });
        }
        Ok((domain.to_owned(), false))
    }

    /// §5.7's replacement key: name, domain, **host-only flag** and path.
    ///
    /// The host-only flag is in the key and easy to leave out — RFC 6265
    /// itself did, and 6265bis added it. Without it, `a=1` set by
    /// `example.com` and `a=2; Domain=example.com` set by the same host
    /// collapse into one cookie, and the survivor is whichever arrived
    /// last: a host-only cookie silently acquires a subdomain scope it was
    /// never given, or loses the one it had.
    fn position_of(&self, cookie: &Cookie) -> Option<usize> {
        self.cookies.iter().position(|c| {
            c.name == cookie.name
                && c.domain == cookie.domain
                && c.host_only == cookie.host_only
                && c.path == cookie.path
        })
    }

    /// RFC 6265 §5.3's eviction: expired cookies first, then the least
    /// recently used, per domain and then overall.
    fn make_room_for(&mut self, incoming: &Cookie, now: SystemTime) {
        self.cookies.retain(|c| !c.is_expired(now));

        while self
            .cookies
            .iter()
            .filter(|c| c.domain == incoming.domain)
            .count()
            >= self.limits.max_per_domain
        {
            let Some(victim) = self.least_recently_used(Some(&incoming.domain)) else {
                break;
            };
            self.cookies.remove(victim);
        }

        while self.cookies.len() >= self.limits.max_cookies {
            let Some(victim) = self.least_recently_used(None) else {
                break;
            };
            self.cookies.remove(victim);
        }
    }

    fn least_recently_used(&self, domain: Option<&str>) -> Option<usize> {
        self.cookies
            .iter()
            .enumerate()
            .filter(|(_, c)| domain.is_none_or(|d| c.domain == d))
            .min_by(|(_, a), (_, b)| a.last_access.cmp(&b.last_access).then(a.seq.cmp(&b.seq)))
            .map(|(i, _)| i)
    }
}

/// §5.7's expiry steps: `Max-Age` beats `Expires`, both are capped at
/// [`MAX_EXPIRY`], and neither means a session cookie.
fn expiry(parsed: &SetCookie, now: SystemTime) -> (Option<SystemTime>, bool) {
    let cap = now.checked_add(MAX_EXPIRY);
    match (parsed.max_age, parsed.expires) {
        (Some(seconds), _) => {
            let at = if seconds <= 0 {
                // "The earliest representable date and time" — anything at
                // or before `now` deletes, and the epoch is the value the
                // RFC's own wording points at.
                UNIX_EPOCH
            } else {
                let requested = u64::try_from(seconds)
                    .ok()
                    .and_then(|s| now.checked_add(Duration::from_secs(s)));
                match (requested, cap) {
                    (Some(r), Some(c)) => r.min(c),
                    (None, Some(c)) => c,
                    (r, None) => r.unwrap_or(now),
                }
            };
            (Some(at), true)
        }
        (None, Some(seconds)) => {
            let at = from_unix(seconds);
            (Some(cap.map_or(at, |c| at.min(c))), true)
        }
        (None, None) => (None, false),
    }
}

fn from_unix(seconds: i64) -> SystemTime {
    if seconds >= 0 {
        UNIX_EPOCH + Duration::from_secs(seconds.unsigned_abs())
    } else {
        UNIX_EPOCH - Duration::from_secs(seconds.unsigned_abs())
    }
}
