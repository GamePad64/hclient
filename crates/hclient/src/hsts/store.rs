//! Where the known HSTS hosts live, and the enumeration that lets a
//! store answer an exact lookup.

use std::collections::HashMap;
use std::future::{Future, Ready, ready};
use std::sync::Mutex;
// `web_time`, not `std::time`, for the reason the jar and the cache read
// it: an entry's lifetime has to mean something outside the process that
// stored it, and off `wasm32-unknown-unknown` this IS
// `std::time::SystemTime`. `no-std-wall-clock-in-the-client` keeps the
// plain import from coming back.
use web_time::SystemTime;

/// One known HSTS host — the whole of what a [`HstsStore`] holds.
///
/// Public because a store outside this crate has to be able to hold one
/// and hand it back, which is [`StoredResponse::new`]'s argument one
/// module over and was found there by writing a store rather than by
/// reading the code.
///
/// [`StoredResponse::new`]: crate::cache::StoredResponse::new
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Entry {
    expires_at: SystemTime,
    include_subdomains: bool,
}

impl Entry {
    /// A policy that stops applying at `expires_at`.
    pub fn new(expires_at: SystemTime, include_subdomains: bool) -> Self {
        Self {
            expires_at,
            include_subdomains,
        }
    }

    /// When this policy stops applying — RFC 6797 §6.1.1's `max-age`
    /// resolved against the `now` that stored it.
    ///
    /// A calendar time rather than an offset, for the reason
    /// `AltSvcStore`'s [`Entry`] carries one: an offset from one
    /// process's epoch means nothing to a file, and a seam whose entries
    /// cannot outlive the process would name a use it could not serve.
    pub fn expires_at(&self) -> SystemTime {
        self.expires_at
    }

    /// §6.1.2's `includeSubDomains`, as this host asserted it.
    pub fn include_subdomains(&self) -> bool {
        self.include_subdomains
    }
}

/// Every domain name that could hold a policy covering `host`, most
/// specific first.
///
/// This is RFC 6797 §8.2's matching turned inside out, and it is the same
/// move [`candidate_domains`](crate::cookie::CookieStore) makes for the
/// jar and for the same reason: §8.2 defines matching as a relation —
/// *"a label-for-label match between an entire Known HSTS Host's domain
/// name and a right-hand portion of the given domain name"* — so a store
/// asked *"what covers `a.b.example.com`?"* would have to implement §8.2
/// itself, which is the thing this seam exists to keep on this side of
/// it. What **is** exact is the name a policy was stored under, and the
/// set of names that can cover a host is bounded and computable from the
/// host alone.
///
/// The first element is the host itself — §8.2's *congruent* match — and
/// each one after it is a *superdomain* match. `is_covered_by` is what
/// reads the distinction, because only the superdomains need
/// `includeSubDomains`.
///
/// **An IP literal has exactly one candidate and can hold no policy**,
/// which is §8.1.1's rule and is applied at the two ends rather than
/// here: this function is asked only about names that reached it.
pub(super) fn candidate_domains(host: &str) -> Vec<String> {
    let mut out = vec![host.to_ascii_lowercase()];
    let mut rest = host;
    while let Some((_, tail)) = rest.split_once('.') {
        if tail.is_empty() {
            break;
        }
        out.push(tail.to_ascii_lowercase());
        rest = tail;
    }
    out
}

/// Where an [`Hsts`](super::Hsts) keeps its known hosts.
///
/// Implement it to put the policy set on disk, in a database or in the
/// browser's own storage; [`MemoryStore`] is what a plain
/// [`Hsts::new`](super::Hsts::new) uses and is the reference for what the
/// methods mean.
///
/// # The obligations
///
/// Two, and neither is an RFC 6797 rule — the rules are
/// [`Hsts`](super::Hsts)'s, applied to whatever this answers:
///
/// 1. [`get`](Self::get) answers **exact** domain matches, and nothing
///    else. A store applying a suffix rule of its own would be answering
///    a question §8.2 has already answered, and answering it a second
///    way is how the two would drift.
/// 2. [`put`](Self::put) replaces by domain name. §8.1's second bullet is
///    an *update* of one host's cached information, so two entries under
///    one name is a state the RFC has no reading for.
///
/// **There is no capacity obligation here, unlike
/// [`CookieStore`](crate::cookie::CookieStore)'s third.** A jar is filled
/// by whatever a server chooses to set, so an unbounded one is a
/// memory-exhaustion bug with a server on the other end of it. This set
/// grows by one entry per *origin the caller chose to visit over HTTPS*,
/// which the caller already bounded by making the requests — and evicting
/// an HSTS policy to save memory is the one direction that ends with a
/// request going out in clear text. [`MemoryStore`] therefore has no
/// `with_capacity`, and that absence is the decision.
///
/// # Associated futures, not `async fn`
///
/// [`CookieStore`](crate::cookie::CookieStore)'s five-way measurement
/// applies here verbatim and is not repeated: `Client` boxes this
/// `Send + Sync`, an `async fn` in a trait is an RPITIT that a generic
/// impl cannot prove `Send`, and the three repairs each cost either
/// stable Rust, every single-threaded store, or an allocation per call.
/// Naming is not requiring — amendment C15.
pub trait HstsStore {
    /// The answer to [`get`](Self::get).
    type Get<'a>: Future<Output = Vec<(String, Entry)>> + 'a
    where
        Self: 'a;
    /// The answer to [`put`](Self::put) and [`remove`](Self::remove).
    type Done<'a>: Future<Output = ()> + 'a
    where
        Self: 'a;

    /// Every entry whose domain is **exactly** one of `domains`, paired
    /// with the domain it was stored under.
    ///
    /// The name comes back because §8.3's precedence turns on *which*
    /// candidate matched — a congruent match applies whatever it says,
    /// where a superdomain match applies only with `includeSubDomains` —
    /// and a bare list of entries could not tell the two apart.
    fn get<'a>(&'a self, domains: &'a [String]) -> Self::Get<'a>;

    /// Remember `entry` for `domain`, replacing whatever was there.
    fn put<'a>(&'a self, domain: &'a str, entry: Entry) -> Self::Done<'a>;

    /// Forget `domain` — §6.1.1's `max-age=0`.
    fn remove<'a>(&'a self, domain: &'a str) -> Self::Done<'a>;

    /// Forget every policy.
    ///
    /// **On the seam rather than built over the others, because it
    /// cannot be**: [`get`](Self::get) answers an exact set of names, so
    /// there is no way to enumerate what is held and remove it one by
    /// one. A store knows its own contents, and this is the only method
    /// that asks it to say so.
    ///
    /// What it is for is a caller changing whose requests these are — a
    /// profile switch, a logout, a test between cases. RFC 6797 has no
    /// opinion on that: §8.1's rules are about what a *host* may ask a UA
    /// to forget, and this is the UA's own decision about a set it owns.
    /// That is why it is here rather than part of
    /// [`note`](super::Hsts::note)'s reading of a header.
    fn clear(&self) -> Self::Done<'_>;
}

/// The store this crate ships: a `HashMap` in memory.
#[derive(Debug, Default)]
pub struct MemoryStore {
    entries: Mutex<HashMap<String, Entry>>,
}

impl MemoryStore {
    /// An empty set of known hosts.
    pub fn new() -> Self {
        Self::default()
    }
}

impl HstsStore for MemoryStore {
    type Get<'a> = Ready<Vec<(String, Entry)>>;
    type Done<'a> = Ready<()>;

    fn get<'a>(&'a self, domains: &'a [String]) -> Self::Get<'a> {
        let map = self.entries.lock().expect("hsts store poisoned");
        ready(
            domains
                .iter()
                .filter_map(|d| map.get(d).map(|e| (d.clone(), *e)))
                .collect(),
        )
    }

    fn put<'a>(&'a self, domain: &'a str, entry: Entry) -> Self::Done<'a> {
        self.entries
            .lock()
            .expect("hsts store poisoned")
            .insert(domain.to_owned(), entry);
        ready(())
    }

    fn remove<'a>(&'a self, domain: &'a str) -> Self::Done<'a> {
        self.entries
            .lock()
            .expect("hsts store poisoned")
            .remove(domain);
        ready(())
    }

    fn clear(&self) -> Self::Done<'_> {
        self.entries.lock().expect("hsts store poisoned").clear();
        ready(())
    }
}
