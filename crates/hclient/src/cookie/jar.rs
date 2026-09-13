//! The storage model (RFC 6265bis §5.7) and retrieval (§5.4).
//!
//! This is where the request URI, the clock and the public suffix list meet
//! the parsed header. Everything that can be decided from the header alone
//! already was, in `parse.rs`.

use crate::cookie::error::Rejected;
use std::time::Duration;
// `web_time`, not `std::time`, for the clock types. On every target but
// `wasm32-unknown-unknown` it is a re-export of `std::time`, so this is
// the same type with the same behaviour and no signature here changes; on
// the browser it is the one that does not panic, which is what made a
// cookie jar there a configuration that did not exist. `Duration` stays
// `std`'s: it carries no clock.
// `scripts/ast-grep/rules/no-std-wall-clock-in-the-client.yml` keeps it so.
use web_time::{SystemTime, UNIX_EPOCH};

use std::sync::atomic::{AtomicU64, Ordering};

use http::{HeaderMap, HeaderValue, Uri};

use super::matching::{
    canonical_host, default_path, domain_matches, is_ip_literal, is_secure_request, path_matches,
    request_path,
};
use super::parse::{SameSite, SetCookie};
use super::store::{CookieKey, CookieStore, MemoryStore, candidate_domains};
use super::suffix::{BuiltinList, PublicSuffixList};

/// RFC 6265bis §5.5: an expiry further out than this is capped to it.
///
/// 400 days, the figure the draft settled on and the one Chrome ships. It
/// also removes the arithmetic hazard from the other end — a server sending
/// `Max-Age=9223372036854775807` cannot overflow anything, because the sum
/// is never computed.
pub(super) const MAX_EXPIRY: Duration = Duration::from_hours(9600);
/// How large one cookie is allowed to be.
///
/// **A refusal, and that is why it is here and not in the store.** The
/// bound on a cookie's `name` + `value` is decided before a [`Cookie`]
/// exists at all — a larger one never reaches storage — where the bound
/// on *how many* cookies are kept is what a store can hold, and lives
/// with the store as [`Capacity`](super::Capacity). The line is
/// [`crate::cache`]'s: a wrong `Capacity` loses cookies, where a wrong
/// refusal would store a cookie the jar was told not to.
///
/// The default is RFC 6265 §6.1's minimum, which is the smallest number
/// here that cannot be called arbitrary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    /// `name.len() + value.len()`. RFC 6265 §6.1: at least 4096 bytes.
    /// A larger cookie is refused outright rather than truncated, because
    /// a truncated cookie is a wrong cookie.
    pub max_name_value_bytes: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_name_value_bytes: 4096,
        }
    }
}

/// One stored cookie.
///
/// Fields are `pub(super)` — read through accessors from anywhere else:
/// the invariants that make `domain` and `path` safe to match against —
/// lowercased, no leading dot, path always absolute — are established in
/// exactly two places, [`CookieJar::store`] and
/// [`CookieJar::restore`](super::CookieJar::restore), and would be a
/// caller's problem if the fields were public.
// RFC 6265's own attribute set: `Secure`, `HttpOnly` and the rest are
// independent flags a `Set-Cookie` either carries or does not, so the
// count is the header's rather than this type's.
#[allow(
    clippy::struct_excessive_bools,
    reason = "One stored cookie. Fields are `pub(super)` — read through accessors from anywhere else: the invariants that make `domain` and `path` safe to match against — lowercased, no leading dot, path always absolute — are established in exactly two places, [`CookieJar::store`] and [`CookieJar::restore`](su..."
)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cookie {
    pub(super) name: String,
    pub(super) value: String,
    pub(super) domain: String,
    pub(super) path: String,
    pub(super) expires: Option<SystemTime>,
    pub(super) creation: SystemTime,
    pub(super) last_access: SystemTime,
    /// Insertion order, and the tiebreak for §5.4's "earlier creation-time
    /// first" when two cookies share a `SystemTime` — which they do
    /// routinely, because one response's `Set-Cookie` headers are all
    /// stored with the same `now`.
    pub(super) seq: u64,
    pub(super) host_only: bool,
    pub(super) persistent: bool,
    pub(super) secure: bool,
    pub(super) http_only: bool,
    pub(super) same_site: Option<SameSite>,
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

    /// Whether this cookie's expiry is at or before `now`.
    ///
    /// Public because a [`CookieStore`](super::CookieStore) is told a
    /// `now` on [`put`](super::CookieStore::put) precisely so that it can
    /// drop what has expired, and it has no other way to ask.
    pub fn is_expired(&self, now: SystemTime) -> bool {
        self.expires.is_some_and(|e| e <= now)
    }
}
/// Which side supplies a cookie's creation time when it replaces one the
/// jar already holds.
///
/// §5.7 says a `Set-Cookie` keeps the **old** cookie's creation time:
/// without that, refreshing a session cookie would move it to the back of
/// §5.4's ordering and change which of two equally specific cookies a
/// server sees first. A [`CookieRecord`](super::CookieRecord) is the
/// other way round — a record *is* the cookie, times and all — so the
/// record's own creation time stands.
///
/// A two-variant enum rather than a `bool` because the two call sites
/// differ in exactly this and the names are what say so at them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Arrival {
    /// A fresh statement about a cookie the jar may already have.
    SetCookie,
    /// A cookie coming back from storage, carrying its own times.
    Record,
}

/// A cookie jar: parse, store, expire and hand back.
///
/// Clockless — every method that needs the time takes it as a `now`
/// parameter, the same rule `hclient-proto` runs under. Nothing here
/// reads a clock or spawns anything, which is what makes "the same cookie
/// behaviour on every backend" a structural fact rather than a
/// consequence of everyone happening to call the same client.
///
/// **It is no longer sans-io, and the seam is why.** Where the cookies
/// live is [`CookieStore`]'s: a jar over a store on disk awaits a disk, a
/// jar over the default [`MemoryStore`] awaits [`std::future::Ready`] and
/// suspends never. The *rules* do no I/O either way — every refusal below
/// is decided before the store is asked.
///
/// ```
/// use std::time::SystemTime;
/// use http::{HeaderValue, Uri};
/// use hclient::cookie::CookieJar;
///
/// # futures_executor::block_on(async {
/// let jar = CookieJar::new();
/// let uri: Uri = "https://www.example.com/app".parse().unwrap();
/// let now = SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_700_000_000);
///
/// jar.store(&uri, &HeaderValue::from_static("sid=abc; Domain=example.com"), now)
///     .await
///     .unwrap();
///
/// let other: Uri = "https://api.example.com/v1".parse().unwrap();
/// assert_eq!(jar.cookie_header(&other, now).await.unwrap(), "sid=abc");
/// # });
/// ```
///
/// The first type parameter is the public suffix list; see
/// [`PublicSuffixList`] for why it is a seam and not a fixed table. The
/// second is where the cookies are kept; see [`CookieStore`].
#[derive(Debug)]
pub struct CookieJar<P = BuiltinList, S = MemoryStore> {
    store: S,
    pub(super) limits: Limits,
    pub(super) suffixes: P,
    /// §5.4's second tiebreak, drawn once for a cookie that is new to the
    /// jar and kept by every replacement of it.
    ///
    /// An `AtomicU64` because every method here takes `&self` now — which
    /// is what the store's own `&self` buys, and what removed the `Mutex`
    /// from [`Client`](crate::Client). `Relaxed` is enough: nothing
    /// orders anything else by this, and two cookies drawing different
    /// values is the whole requirement.
    next_seq: AtomicU64,
}

impl Default for CookieJar<BuiltinList, MemoryStore> {
    fn default() -> Self {
        Self::new()
    }
}

impl CookieJar<BuiltinList, MemoryStore> {
    /// A jar with default [`Limits`], the compiled-in public suffix list
    /// and the in-memory store.
    pub fn new() -> Self {
        Self::with_public_suffix_list(BuiltinList)
    }
}

impl<P: PublicSuffixList> CookieJar<P, MemoryStore> {
    /// A jar over a caller-supplied list — a fresher snapshot than the one
    /// this crate was built with, or [`NoList`](super::NoList).
    pub fn with_public_suffix_list(suffixes: P) -> Self {
        Self::with_store(suffixes, MemoryStore::new())
    }
}

impl<P: PublicSuffixList, S: CookieStore> CookieJar<P, S> {
    /// A jar over a store of the caller's own — on disk, in a database,
    /// in the browser's own storage.
    pub fn with_store(suffixes: P, store: S) -> Self {
        Self {
            store,
            limits: Limits::default(),
            suffixes,
            next_seq: AtomicU64::new(0),
        }
    }

    /// The same jar over a different public suffix list — the store, the
    /// bound and the sequence counter carried across.
    ///
    /// The list is a seam and the jar is rules, so the two should be
    /// separable after construction as well as at it. What actually asked
    /// for this is `hclient`, which holds one jar type for every caller
    /// and so must erase `P`; the operation is not specific to that —
    /// swapping a stale compiled-in snapshot for a freshly fetched list
    /// without losing the cookies is the same call.
    pub(crate) fn map_suffixes<Q>(self, f: impl FnOnce(P) -> Q) -> CookieJar<Q, S> {
        CookieJar {
            store: self.store,
            limits: self.limits,
            suffixes: f(self.suffixes),
            next_seq: self.next_seq,
        }
    }

    /// The same jar over a different store, everything else carried
    /// across — [`map_suffixes`](Self::map_suffixes)' counterpart and
    /// `HttpCache::map_store`'s twin one module over, `pub(crate)` for
    /// the same reason: the one thing that asks is
    /// [`ClientBuilder::cookie_jar`](crate::ClientBuilder::cookie_jar),
    /// which holds one jar type for every caller. A caller who wants a
    /// different store builds one with [`with_store`](Self::with_store).
    pub(crate) fn map_store<R>(self, f: impl FnOnce(S) -> R) -> CookieJar<P, R> {
        CookieJar {
            store: f(self.store),
            limits: self.limits,
            suffixes: self.suffixes,
            next_seq: self.next_seq,
        }
    }

    /// Replace the bound. Applied on the next [`store`](Self::store); it
    /// does not evict what is already held.
    #[must_use]
    pub fn with_limits(mut self, limits: Limits) -> Self {
        self.limits = limits;
        self
    }

    pub fn limits(&self) -> Limits {
        self.limits
    }

    /// How many cookies are held, expired ones included — they are
    /// removed on the next [`store`](Self::store) that names a `now` past
    /// them, not by a background sweep this crate has no way to run.
    pub async fn len(&self) -> usize {
        self.store.len().await
    }

    pub async fn is_empty(&self) -> bool {
        self.len().await == 0
    }

    pub async fn clear(&self) {
        self.store.clear().await;
    }

    /// Every cookie held, in insertion order. For inspection; retrieval
    /// for a request is [`matching`](Self::matching), and saving a jar is
    /// [`records`](Self::records) — which filters the session cookies
    /// this does not.
    ///
    /// **Owned rather than an iterator of borrows**, which is what the
    /// seam costs: a store on the far side of a socket has nothing to
    /// lend. It is bounded by [`Capacity`](super::Capacity), so the copy
    /// is bounded with it.
    pub async fn cookies(&self) -> Vec<Cookie> {
        self.store.all().await
    }

    /// Store one `Set-Cookie`, per RFC 6265bis §5.7.
    ///
    /// # Errors
    ///
    /// One [`Rejected`] variant per rule §5.7 states — see its own
    /// per-variant docs for which.
    pub async fn store(
        &self,
        uri: &Uri,
        set_cookie: &HeaderValue,
        now: SystemTime,
    ) -> Result<(), Rejected> {
        let cookie = self.prepare(uri, set_cookie, now)?;
        // A `Max-Age=0` or a past `Expires` is a deletion: the steps above
        // build it as an already-expired cookie and the store's own expiry
        // purge drops it, so deletion is the same code path as expiry
        // rather than a second one that could disagree with it.
        self.insert(cookie, now, Arrival::SetCookie).await;
        Ok(())
    }

    /// Everything §5.7 decides about one `Set-Cookie`, with the store
    /// untouched.
    ///
    /// **Split out because every rule here is a pure function of the
    /// header, the URI and a `now`** — the jar's sans-io half — and only
    /// the insertion needs to ask where the cookies live.
    /// [`store_response`](Self::store_response) is what wanted the line
    /// drawn: it can settle every refusal before it touches the store at
    /// all, and then read once for the whole response instead of once per
    /// header.
    fn prepare(
        &self,
        uri: &Uri,
        set_cookie: &HeaderValue,
        now: SystemTime,
    ) -> Result<Cookie, Rejected> {
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

        let cookie = Cookie {
            name: parsed.name,
            value: parsed.value,
            domain,
            path,
            expires,
            creation: now,
            last_access: now,
            // Filled in by `insert`, which is the only thing that knows
            // whether this cookie is new to the jar.
            seq: 0,
            host_only,
            persistent,
            secure: parsed.secure,
            http_only: parsed.http_only,
            same_site: parsed.same_site,
        };

        Ok(cookie)
    }

    /// Store every `Set-Cookie` in a response's headers, returning how many
    /// were accepted.
    ///
    /// Refusals are dropped rather than returned, because one bad
    /// `Set-Cookie` must not stop the others from being stored — that is
    /// what a browser does and what a server assumes. Use
    /// [`store`](Self::store) per header when the reasons matter.
    /// **One read for the whole response, not one per header**, and the
    /// difference is a fact about the store rather than about the rules.
    /// Each header used to go through [`store`](Self::store), and each of
    /// those asked the store what it already held so that §5.7's
    /// replacement could keep the old cookie's `seq` and `creation`.
    /// Measured from outside the workspace against a store that logs its
    /// statements: a response carrying five `Set-Cookie` headers cost
    /// **six reads and five writes**, where one read serves them all.
    /// `MemoryStore` answers [`std::future::Ready`] so it never showed;
    /// a store on the far side of a file or a socket pays every one.
    ///
    /// The batch still sees itself, which is the part that had to be
    /// built rather than saved: two `Set-Cookie` headers naming one
    /// cookie mean the second replaces the first, so a cookie already
    /// placed by *this* response is looked up in the batch before the
    /// snapshot — otherwise the second would draw a fresh `seq` and jump
    /// §5.4's queue against the first.
    pub async fn store_response(&self, uri: &Uri, headers: &HeaderMap, now: SystemTime) -> usize {
        let arriving: Vec<Cookie> = headers
            .get_all(http::header::SET_COOKIE)
            .iter()
            .filter_map(|v| self.prepare(uri, v, now).ok())
            .collect();
        if arriving.is_empty() {
            return 0;
        }

        // One question, covering every domain this response scopes a
        // cookie to. `get` takes a slice precisely so that it can be
        // asked once.
        let mut domains: Vec<String> = arriving.iter().map(|c| c.domain.clone()).collect();
        domains.sort();
        domains.dedup();
        let held = self.store.get(&domains).await;

        let stored = arriving.len();
        let mut placed: Vec<Cookie> = Vec::new();
        for mut cookie in arriving {
            let key = CookieKey::of(&cookie);
            let previous = placed
                .iter()
                .find(|c| CookieKey::of(c) == key)
                .or_else(|| held.iter().find(|c| CookieKey::of(c) == key));
            match previous {
                Some(old) => {
                    cookie.creation = old.creation;
                    cookie.seq = old.seq;
                }
                None => cookie.seq = self.next_seq.fetch_add(1, Ordering::Relaxed),
            }
            placed.push(cookie.clone());
            self.store.put(cookie, now).await;
        }
        stored
    }

    /// The cookies that apply to `uri`, in the order RFC 6265bis §5.4
    /// requires: longer paths first, then earlier creation time.
    ///
    /// Read-only — it does not update last-access times, so it cannot
    /// change which cookie the bound would evict next.
    /// [`cookie_header`](Self::cookie_header) is the one that does.
    pub async fn matching(&self, uri: &Uri, now: SystemTime) -> Vec<Cookie> {
        let Some(host) = canonical_host(uri) else {
            return Vec::new();
        };
        let path = request_path(uri);
        let secure = is_secure_request(uri);

        // The store answers exact domains, and this is the list of them:
        // §5.1.3's suffix rule enumerated rather than tested, so that
        // nothing on the far side of the seam has to know it. The filter
        // below still asks `domain_matches`, because a host-only cookie
        // is a different question and because an exact answer to the
        // wrong question is still wrong.
        let mut out = self.store.get(&candidate_domains(&host)).await;
        out.retain(|c| {
            !c.is_expired(now)
                && if c.host_only {
                    host == c.domain
                } else {
                    domain_matches(&host, &c.domain)
                }
                && path_matches(path, &c.path)
                && (!c.secure || secure)
        });
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
    /// Takes `&self` where it used to take `&mut self`, and still updates
    /// each returned cookie's last-access time — §5.4 requires it and
    /// [`Capacity`](super::Capacity) evicts on it. What changed is who
    /// holds the mutation: the store does, behind its own
    /// synchronisation, which is what let [`Client`](crate::Client) stop
    /// serialising every request behind one lock.
    pub async fn cookie_header(&self, uri: &Uri, now: SystemTime) -> Option<HeaderValue> {
        let matched = self.matching(uri, now).await;
        if matched.is_empty() {
            return None;
        }

        let mut out = Vec::new();
        for cookie in &matched {
            if !out.is_empty() {
                out.extend_from_slice(b"; ");
            }
            out.extend_from_slice(cookie.name.as_bytes());
            out.push(b'=');
            out.extend_from_slice(cookie.value.as_bytes());
        }

        let keys: Vec<CookieKey> = matched.iter().map(CookieKey::of).collect();
        self.store.touch(&keys, now).await;

        // Every byte here already survived `parse.rs`'s CTL check, so the
        // only way this fails is a bug in that check — which is exactly
        // when silence would be worst.
        HeaderValue::from_bytes(&out).ok()
    }

    /// Put a cookie into the store, resolving the two fields that belong
    /// to the **jar** rather than to the cookie.
    ///
    /// `seq` is the jar's insertion identity: a cookie already held keeps
    /// the one it has, so a refresh cannot jump §5.4's queue and a
    /// restored cookie cannot jump ahead of one already there. `creation`
    /// is §5.7's, and which side supplies it is what [`Arrival`] names.
    pub(super) async fn insert(&self, mut cookie: Cookie, now: SystemTime, arrival: Arrival) {
        let key = CookieKey::of(&cookie);
        let domains = [cookie.domain.clone()];
        let held = self.store.get(&domains).await;
        match held.iter().find(|c| CookieKey::of(c) == key) {
            Some(old) => {
                if arrival == Arrival::SetCookie {
                    cookie.creation = old.creation;
                }
                cookie.seq = old.seq;
            }
            None => cookie.seq = self.next_seq.fetch_add(1, Ordering::Relaxed),
        }
        self.store.put(cookie, now).await;
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

/// The one thing about [`MAX_EXPIRY`] that three modules assert and
/// nothing checked.
#[cfg(all(test, feature = "hsts"))]
mod agreement {
    /// `hsts::MAX_AGE_CAP`'s public doc says it carries "the same figure"
    /// as this constant, and `hclient-native`'s `altsvc::MAX_LEASE` says
    /// it takes "`MAX_EXPIRY` and its argument verbatim".
    ///
    /// **A claim about a value is exactly as perishable as the check
    /// behind it**, and there was none: the three spell
    /// `400 * 24 * 60 * 60` independently, so changing one leaves a
    /// public doc comment asserting an agreement that has ended. This
    /// pins the pair reachable from one crate.
    ///
    /// It lives here rather than beside `MAX_AGE_CAP` because this
    /// constant is `pub(super)` — deliberately, its own doc argues the
    /// narrowness — so `hsts` cannot see it and widening the visibility
    /// to be checked would undo the thing being checked.
    ///
    /// `altsvc::MAX_LEASE` is another crate's private constant and stays
    /// prose. Said rather than left to be found, because a check covering
    /// two of three reads as covering all three.
    #[test]
    fn the_hsts_cap_still_carries_this_figure() {
        assert_eq!(
            crate::hsts::MAX_AGE_CAP,
            super::MAX_EXPIRY,
            "`hsts::MAX_AGE_CAP`'s doc claims this crate's cookie figure"
        );
    }
}
