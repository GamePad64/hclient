//! Saving a jar and loading it back, with the facts that decide §5.4's
//! order and §5.3's eviction intact.
//!
//! # Why this is not a `CookieStore` trait
//!
//! The sibling module's [`CacheStore`](crate::cache::CacheStore) is a
//! delegation seam — the policy asks it for the entries under one key and
//! the caller may back it with anything — and the argument written at the
//! top of `cache/store.rs` for why that is safe to hand out is exactly the
//! argument against doing the same here: *"a wrong `CacheStore` loses
//! entries or keeps too many, and cannot serve a stale response to a
//! request that forbade one."*
//!
//! A cookie store can. RFC 9111 lookup is **by key**, so a cache store
//! needs one method that takes a key and one that takes a key and an
//! entry. RFC 6265bis §5.4 retrieval is a **scan**: every held cookie is
//! tested with `domain_matches` and `path_matches`, and a store keyed by
//! domain would have to be asked once per label of the request host.
//! Storage (§5.7) needs the four-part replacement key, eviction (§5.3)
//! needs a least-recently-used pick both per domain and overall, and
//! [`CookieJar::cookie_header`] has to write a last-access time back to
//! the entries it just returned. A trait wide enough for all of that
//! carries the matching rules across the seam — and a wrong implementation
//! of it hands one origin's cookie to another, which is the one failure
//! this module exists to prevent. `matching.rs`'s own doc comment says the
//! same thing from the other side: *"a version of this file with the
//! boundary checks deleted still returns cookies to the host that set
//! it."*
//!
//! So the jar keeps its `Vec<Cookie>` and gains an honest save and load
//! instead. What a caller who wants a database gets is the records; what
//! they do not get is the ability to break domain matching.
//!
//! **The ecosystem agrees, and it was read rather than recalled.** The
//! crate this workspace's own competitive table names —
//! `reqwest_cookie_store`, at 347k downloads a month — is a `Mutex`
//! wrapper and nothing else; the jar underneath it, `cookie_store` 0.22.1,
//! is a **struct** and not a trait, and its persistence is exactly this:
//! records out, records back, with `from_cookies` inserting straight into
//! its map. `reqwest`'s own `cookie::CookieStore` *is* a trait, and it is
//! not a storage seam either — `set_cookies(&self, &mut dyn
//! Iterator<Item = &HeaderValue>, &Url)` and `cookies(&self, &Url) ->
//! Option<HeaderValue>` replace the **whole jar**, parsing and matching
//! and all. Nobody in this ecosystem has published a storage seam under a
//! cookie jar, and the reason is the paragraph above.
//!
//! Two places this goes further than `cookie_store` does, both measured
//! by reading its source. Its record carries `expires` and **no creation
//! time and no last-access time** at all — four fields, of which one is
//! temporal — so it cannot implement §5.4's *"earlier creation-times
//! first"* tiebreak from a reloaded jar, or §5.3's least-recently-used
//! eviction. And its load path calls neither `insert` nor its
//! public-suffix check, so a cookie saved for a domain that has since
//! been **added** to the list comes back: [`CookieJar::restore`] re-checks
//! it, which is the one direction `suffix.rs`'s own doc says a stale
//! snapshot can hurt in.
//!
//! # What was already expressible, measured rather than assumed
//!
//! [`CookieJar::iter`] and [`CookieJar::store`] almost make a round trip:
//! read the jar out, synthesise one `Set-Cookie` per cookie, put them
//! back. The reason to build this anyway is narrower than it first looks,
//! and it is worth writing down because the wider claim is false.
//!
//! **Order survives that replay, and it was expected not to.** §5.4 sorts
//! by path length and then by creation time, a replay gives every cookie
//! the same creation time, so the ordering looks lost — except that
//! [`Cookie`]'s `seq` breaks that tie in insertion order, and `iter`
//! yields in insertion order. Measured on this jar: two cookies sharing a
//! path, saved and replayed through `store`, come back in the same order.
//! **What breaks it is a clock that steps backwards** — an NTP correction
//! between two `Set-Cookie`s — after which insertion order and creation
//! order disagree, and the replay flattens the disagreement onto insertion
//! order.
//!
//! What a replay genuinely cannot carry is:
//!
//! - **`last_access`**, which is what [`Limits`](super::Limits) evicts on
//!   (§5.3). Every replayed cookie is equally recent, so the first jar to
//!   hit its bound after a restart evicts in insertion order rather than
//!   by use;
//! - **`creation`** as a value a caller can read back, which is a fact
//!   about the cookie rather than about this process;
//! - and the work: an absolute `Expires` has to be formatted as an
//!   HTTP-date — this module parses those and does not write them — or
//!   turned into a `Max-Age` by arithmetic against the restore time, which
//!   is where a caller reaches for the *original* `Max-Age` and grants a
//!   cookie a second full lifetime.
//!
//! # Serialisation is the caller's
//!
//! [`CookieRecord`]'s fields are public and there is no `serde` impl.
//! `serde` with `derive` is **7 crates** in this graph (`serde`,
//! `serde_core`, `serde_derive`, `syn`, `quote`, `proc-macro2`,
//! `unicode-ident`) and without it 2 — measured — and Cargo unifies
//! features, so a `cookies` feature that pulled it would be a floor rather
//! than a default for every build in the graph. This crate already keeps
//! `serde` behind `json`, off by default, for that reason.
//!
//! The cost of not shipping it is real and is named: a caller cannot
//! `#[derive(Serialize)]` on a foreign type, so they write a mirror struct
//! with `From` in both directions, or `#[serde(remote = "..")]`. Public
//! fields are what make either of those a few lines. And the format is
//! genuinely theirs — JSON, a database row, a browser's `localStorage` —
//! where a `serde` impl here would freeze one on-disk shape into this
//! crate's semver promise.

// `web_time`, not `std::time`: on `wasm32-unknown-unknown` these
// timestamps come from a clock `std` does not have there, and on every
// other target `web_time::SystemTime` IS `std::time::SystemTime`, so
// nothing in the signatures below changes.
use web_time::SystemTime;

use super::jar::{Cookie, CookieJar, MAX_EXPIRY, Rejected};
use super::matching::is_ip_literal;
use super::parse::{ParseError, SameSite, is_ctl};
use super::suffix::PublicSuffixList;

/// One persistent cookie, as plain data: every fact [`CookieJar::restore`]
/// needs to put it back the way it was.
///
/// # A session cookie cannot be written down
///
/// `expires` is a [`SystemTime`] and not an `Option<SystemTime>`, so
/// *"neither `Expires` nor `Max-Age`"* — a session cookie — has no
/// representation here at all, and [`Cookie::to_record`] answers `None`
/// for one. That is the rule everybody knows about cookies made
/// structural rather than made a filter the save side has to remember:
/// `jar.iter().filter_map(Cookie::to_record)` cannot save a session
/// cookie, and neither can a caller who forgets why they should not.
///
/// # A record and not a `CookieStore` trait
///
/// The sibling [`HttpCache`](crate::cache::HttpCache) delegates to a
/// [`CacheStore`](crate::cache::CacheStore) and a caller may back that
/// with anything, because RFC 9111 lookup is **by key**: a wrong store
/// loses entries or keeps too many and cannot answer the wrong request.
/// RFC 6265bis §5.4 retrieval is a **scan** — every held cookie tested
/// with §5.1.3's domain-match and §5.1.4's path-match — so a trait wide
/// enough to serve it carries those two rules across the seam, and a
/// wrong implementation hands one origin's cookie to another. The jar
/// therefore keeps its own storage and hands out records instead: what a
/// caller gets is everything they need to persist a jar, and what they
/// do not get is the ability to break domain matching.
///
/// The ecosystem reached the same place. `cookie_store` 0.22.1 — the jar
/// under `reqwest_cookie_store` — is a **struct** and not a trait, and
/// persists by round-tripping records exactly like this;
/// `reqwest::cookie::CookieStore` *is* a trait and is not a storage seam
/// either, taking `Set-Cookie` values in and a whole `Cookie` header out.
/// Two things here go further, both read off `cookie_store`'s source: its
/// record carries no creation time and no last-access time at all, so a
/// reloaded jar cannot implement §5.4's creation tiebreak or §5.3's
/// least-recently-used eviction; and its load path runs none of the
/// storage-model checks, where [`CookieJar::restore`] runs them.
///
/// # The serialisation is yours
///
/// The fields are public and there is no `serde` impl. `serde` with
/// `derive` is **7 crates** in this graph and 2 without — measured — and
/// Cargo unifies features, so a `cookies` feature that pulled it would be
/// a floor for every build in the graph rather than a default. The cost
/// of that is named rather than waved away: a caller cannot derive on a
/// foreign type, so they write a mirror struct with `From` both ways, or
/// `#[serde(remote = "..")]`. Public fields are what make either a few
/// lines, and the format — a file, a database row, a `localStorage` key
/// — really is theirs.
///
/// # Not `#[non_exhaustive]`
///
/// The caller **builds** this — that is the whole of the load side — and
/// the attribute forbids a struct literal from outside the defining
/// crate. Same answer as [`Limits`](super::Limits) and `TcpOpts`, for the
/// same reason and not by analogy.
///
/// There is deliberately no `Default` either: `creation` and
/// `last_access` have no honest default, and `UNIX_EPOCH` is not a
/// missing value but a wrong one — it sorts first in §5.4 and is evicted
/// first by §5.3.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CookieRecord {
    pub name: String,
    pub value: String,
    /// Lowercased and without the leading `.` some servers send;
    /// [`CookieJar::restore`] applies both rather than refusing, because
    /// §5.2.3 makes them a normalisation rather than a rule.
    pub domain: String,
    /// Absolute, beginning with `/`.
    pub path: String,
    /// When this cookie stops being sent — already capped to §5.5's 400
    /// days by whatever stored it, and capped again on the way back in.
    pub expires: SystemTime,
    /// §5.4's tiebreak among cookies of equal path length. Carrying it is
    /// most of the reason this type exists.
    pub creation: SystemTime,
    /// §5.3's eviction key, and the other reason.
    pub last_access: SystemTime,
    /// Whether this cookie goes only to the exact host that set it.
    /// Part of §5.7's replacement key, so it is not decoration: without
    /// it a reload can collapse two cookies into one.
    pub host_only: bool,
    pub secure: bool,
    pub http_only: bool,
    pub same_site: Option<SameSite>,
}

impl Cookie {
    /// When this cookie was last returned by
    /// [`CookieJar::cookie_header`] — or, until it has been, when it was
    /// stored.
    ///
    /// Read-only and public because [`CookieRecord`] carries it: it is
    /// [`Limits`](super::Limits)' eviction key, so a jar that saves
    /// everything else and loses this comes back with its
    /// least-recently-used order replaced by its insertion order.
    pub fn last_access(&self) -> SystemTime {
        self.last_access
    }

    /// This cookie as plain data, or `None` when it is a session cookie.
    ///
    /// The `None` is the whole point — see [`CookieRecord`]. It is not an
    /// error: a jar holds session cookies on purpose, and the answer to
    /// *"save this one"* for one of them is *"no"* rather than *"that
    /// failed"*.
    pub fn to_record(&self) -> Option<CookieRecord> {
        Some(CookieRecord {
            name: self.name.clone(),
            value: self.value.clone(),
            domain: self.domain.clone(),
            path: self.path.clone(),
            expires: self.expires?,
            creation: self.creation,
            last_access: self.last_access,
            host_only: self.host_only,
            secure: self.secure,
            http_only: self.http_only,
            same_site: self.same_site,
        })
    }
}

impl<P: PublicSuffixList> CookieJar<P> {
    /// Everything worth saving, in the order [`restore`](Self::restore)
    /// wants it back.
    ///
    /// `jar.iter().filter_map(Cookie::to_record)` exactly — spelled as a
    /// method because this is the name a reader looking for persistence
    /// will type, and a feature nobody can find is one this crate has
    /// already shipped twice.
    ///
    /// Expired cookies are **not** filtered, because that would need a
    /// clock this type does not have; [`restore`](Self::restore) drops
    /// them on the way back in, which is the arrival this rule has to
    /// hold at anyway.
    pub fn records(&self) -> impl Iterator<Item = CookieRecord> + '_ {
        self.cookies.iter().filter_map(Cookie::to_record)
    }

    /// Put one saved cookie back, with its creation and last-access times
    /// intact.
    ///
    /// # It takes no URI, and that is the design
    ///
    /// [`store`](Self::store) needs a request URI because a `Set-Cookie`
    /// is a *claim* that has to be checked against where it came from. A
    /// record is already scoped — it carries the domain, the path and the
    /// host-only flag §5.7 derived — so a URI here would be a second
    /// input that could disagree with the first, and the way it would
    /// disagree is by widening a cookie's scope. Everything §5.7 checks
    /// against a request is re-checked here against the record's own
    /// facts instead:
    ///
    /// - `__Secure-` needs `secure`, and `__Host-` needs `secure`,
    ///   `host_only` and `path == "/"` — the durable half of §4.1.3,
    ///   which is the whole of it once there is no request to be secure;
    /// - a `domain` that is a public suffix is refused **unless the
    ///   cookie is host-only**, which is [`store`](Self::store)'s own
    ///   rescue and the reason `Domain=localhost` works. This is where a
    ///   list that grew since the jar was saved earns its keep: a cookie
    ///   set for a shared-hosting domain before that domain was added to
    ///   the list does not come back;
    /// - the bounds in [`Limits`](super::Limits) apply, so a hand-edited
    ///   file cannot make the jar larger than a server could.
    ///
    /// # An expired cookie is dropped rather than refused
    ///
    /// `Ok(())` and nothing stored, the same answer
    /// [`store`](Self::store) gives a `Max-Age=0` deletion — expiry and
    /// deletion are one code path there and one here, rather than two
    /// that could come to disagree. So a load never resurrects a cookie
    /// that died while the process was not running, and never reports
    /// that as a failure either.
    ///
    /// # Order
    ///
    /// Restoring in [`records`](Self::records)' order reproduces the
    /// jar's own insertion order, which is what breaks §5.4's remaining
    /// tie between two cookies of equal path length and equal creation
    /// time. Any other order is still correct wherever the creation times
    /// differ, which is the case that mattered.
    ///
    /// ```
    /// use std::time::{Duration, SystemTime};
    /// use http::{HeaderValue, Uri};
    /// use hclient::cookie::{CookieJar, CookieRecord};
    ///
    /// let uri: Uri = "https://www.example.com/app".parse().unwrap();
    /// let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
    ///
    /// let mut jar = CookieJar::new();
    /// jar.store(&uri, &HeaderValue::from_static("sid=abc; Max-Age=86400"), now)?;
    /// jar.store(&uri, &HeaderValue::from_static("tmp=xyz"), now)?;
    ///
    /// // Save. The session cookie has no record, so it cannot be written
    /// // down even by a caller who forgot that it should not be.
    /// let saved: Vec<CookieRecord> = jar.records().collect();
    /// assert_eq!(saved.len(), 1);
    ///
    /// // Load, into a jar that has never seen the server.
    /// let restart = now + Duration::from_secs(600);
    /// let mut jar = CookieJar::new();
    /// for record in saved {
    ///     jar.restore(record, restart)?;
    /// }
    /// assert_eq!(jar.cookie_header(&uri, restart).unwrap(), "sid=abc");
    /// # Ok::<(), hclient::cookie::Rejected>(())
    /// ```
    pub fn restore(&mut self, record: CookieRecord, now: SystemTime) -> Result<(), Rejected> {
        let CookieRecord {
            name,
            value,
            domain,
            path,
            expires,
            creation,
            last_access,
            host_only,
            secure,
            http_only,
            same_site,
        } = record;

        if name.is_empty() {
            return Err(Rejected::Malformed(ParseError::EmptyName));
        }
        if name.bytes().chain(value.bytes()).any(is_ctl) {
            return Err(Rejected::Malformed(ParseError::ControlCharacter));
        }
        let bytes = name.len() + value.len();
        if bytes > self.limits.max_name_value_bytes {
            return Err(Rejected::TooLarge {
                bytes,
                limit: self.limits.max_name_value_bytes,
            });
        }

        // §5.2.3's normalisation, applied for the same reason the parser
        // applies it: a leading dot and a capital letter are how the
        // attribute is *written*, not a different scope.
        let domain = domain
            .strip_prefix('.')
            .unwrap_or(&domain)
            .to_ascii_lowercase();
        if domain.is_empty() {
            return Err(Rejected::EmptyDomain);
        }
        if !path.starts_with('/') {
            return Err(Rejected::RelativePath { path });
        }

        // `store` can never produce this pair — §5.7 makes an IP-literal
        // host host-only unconditionally — and `domain_matches` is not
        // written to survive it. Measured: `domain_matches("evil.1.2.3.4",
        // "1.2.3.4")` is **true**, because §5.1.3's IP test asks whether
        // the *request host* is a literal and here it is not. So a record
        // carrying this pair is a cookie for every name ending in
        // `.1.2.3.4`, and it is refused here rather than repaired,
        // because there is nothing to repair it into that the saver meant.
        if !host_only && is_ip_literal(&domain) {
            return Err(Rejected::IpDomainNotHostOnly { domain });
        }

        if !host_only && self.suffixes.is_public_suffix(&domain) {
            return Err(if self.suffixes.has_list() {
                Rejected::DomainIsPublicSuffix { domain }
            } else {
                Rejected::NoPublicSuffixList { domain }
            });
        }

        let lower = name.to_ascii_lowercase();
        if lower.starts_with("__secure-") && !secure {
            return Err(Rejected::SecurePrefix);
        }
        if lower.starts_with("__host-") && !(secure && host_only && path == "/") {
            return Err(Rejected::HostPrefix);
        }

        // §5.5's cap again, against *this* `now`: a saved expiry was
        // capped when it was stored, and a hand-written one was not.
        let expires = match now.checked_add(MAX_EXPIRY) {
            Some(cap) => expires.min(cap),
            None => expires,
        };
        if expires <= now {
            return Ok(());
        }

        let mut cookie = Cookie {
            name,
            value,
            domain,
            path,
            expires: Some(expires),
            creation,
            last_access,
            seq: self.next_seq,
            host_only,
            // A record is a persistent cookie by construction.
            persistent: true,
            secure,
            http_only,
            same_site,
        };

        match self.position_of(&cookie) {
            Some(i) => {
                // Unlike `store`, the record's own `creation` stands: a
                // `Set-Cookie` is a fresh statement about a cookie the jar
                // already has, where a record *is* that cookie, times and
                // all. What is kept is `seq`, which is the jar's insertion
                // identity rather than the cookie's — a restored cookie
                // must not jump the queue against one already held.
                cookie.seq = self.cookies[i].seq;
                self.cookies[i] = cookie;
            }
            None => {
                self.next_seq += 1;
                self.make_room_for(&cookie, now);
                self.cookies.push(cookie);
            }
        }
        Ok(())
    }
}
