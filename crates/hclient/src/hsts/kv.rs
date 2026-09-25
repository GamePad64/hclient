//! [`HstsStore`] over [`KeyValueStore`] — the first of the four
//! wrappers.
//!
//! The argument lives on [`KvStore`] itself, because this module is
//! private and rustdoc publishes nothing from it: a reader of the
//! rendered page meets the type, never the module.

use super::{Entry, HstsStore};
use hclient_core::kv::KeyValueStore;
use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::Duration;
use web_time::SystemTime;

/// The namespace this wrapper writes under.
///
/// One store can back all four seams, and this is what keeps a cookie
/// for `com.example` and an HSTS policy for `com.example` from being one
/// entry.
const NS: &str = "hsts";

// Maintainer notes (not rendered):
//
// # This is the only `HstsStore` this crate ships
//
// There was a second — a `MemoryStore` of 43 lines holding a
// `Mutex<HashMap<String, Entry>>` — and it went when this arrived,
// because it had become a name whose only purpose was a distinction the
// code no longer draws. Unlike [`cookie`](crate::cookie) and
// [`cache`](crate::cache), whose own memory stores carry a capacity and
// an eviction policy the byte seam deliberately has not got, this seam
// has neither: RFC 6797 entries are bounded by the origins a caller
// chose to visit, and evicting one ends with a request in clear text.
// So the old store held a map and nothing else, which is exactly what
// [`InMemory`](super::InMemory) is.
//
// What it costs is real and is why this was a decision rather than a
// tidy-up: an encode and a decode per operation, where a map held the
// [`Entry`] itself. Against that, two in-memory `HstsStore`s are two
// readings of §8.2 that can drift — a defect this workspace has met
// before — and the encoding sits on a path that is about to touch the
// network.
//
// Handing the expiry to the byte
// store as well would make that eviction **unobservable**: the entry
// would stop being answered whether or not the rule above ran, so the
// test that pins §8.1.1 — *the store no longer holds it* — would pass
// over an `Hsts` that had stopped evicting anything. A check that
// cannot fail is not a check.

/// An [`HstsStore`] over any [`KeyValueStore`].
/// [`KvStore`] is an [`HstsStore`] built on any byte store, so putting
/// the policy set on disk or in Redis is a matter of implementing
/// [`KeyValueStore`] once rather than each of the four seams above it.
///
/// # What the wrapper owns
///
/// The byte seam knows a namespace, a string key and opaque bytes; every
/// piece of HSTS vocabulary is here.
///
/// **The key is the domain with its labels reversed** — `com.example.a`
/// for `a.example.com`. That is the design's rule for every wrapper: it
/// turns RFC 6797 §8.2's suffix relation into a prefix one, so one host's
/// policies sit in one range of an ordered store and *forget everything
/// under this domain* is a prefix operation. It does **not** remove the
/// need for [`get_many`](KeyValueStore::get_many) — §8.2 matches *up* the
/// tree, so the candidates are a chain of prefixes of each other rather
/// than a common prefix of a set.
///
/// **An IP literal is never reversed**, because a reversed `1.0.0.127` is
/// a different address. [`hsts`](super) refuses to note one at all
/// (§8.1.1), so nothing should reach this — and the reversal is
/// written to be correct anyway rather than to rely on that, since a
/// store is also read by whatever was written into it before.
///
/// # Expiry stays above this seam, deliberately
///
/// [`KeyValueStore::put`] takes an expiry and the store will hide what
/// has passed it — and this wrapper passes **`None`**, keeping every
/// entry until something removes it.
///
/// That reads backwards and is the one decision here worth the
/// paragraph. §8.1.1's *"MUST evict all expired Known HSTS Hosts"* is
/// applied by [`Hsts::is_known`](super::Hsts), which reads
/// [`Entry::expires_at`], ignores what has passed and calls
/// [`remove`](HstsStore::remove) on it. Handing the expiry to the byte
/// store as well would make that eviction **unobservable**: the entry
/// would stop being answered whether or not the rule above ran.
///
/// So the expiry is stated in exactly one place, which is inside the
/// encoded [`Entry`], and one layer enforces it. A store that outlives
/// the process therefore keeps expired entries until the next read
/// sweeps them, which is what `MemoryStore` next door does today.
///
/// # No capacity bound
///
/// Unlike [`cookie`](crate::cookie) and [`cache`](crate::cache), whose
/// own memory stores carry a capacity and an eviction policy the byte
/// seam deliberately has not got, this seam has neither: RFC 6797
/// entries are bounded by the origins a caller chose to visit, and
/// evicting one ends with a request in clear text.
///
/// Encoding costs one encode and one decode per operation, where a plain
/// map would hold the [`Entry`] itself.
#[derive(Debug, Clone, Default)]
pub struct KvStore<K> {
    kv: K,
}

impl<K> KvStore<K> {
    /// An HSTS store backed by `kv`.
    pub const fn new(kv: K) -> Self {
        Self { kv }
    }

    /// The byte store underneath, for a caller that shares one between
    /// seams and wants to reach it.
    pub const fn inner(&self) -> &K {
        &self.kv
    }
}

/// A domain with its labels reversed — `a.example.com` to
/// `com.example.a` — which is what puts one host's entries in one range
/// of an ordered store.
///
/// **An IP literal is returned unchanged**, because reversing one
/// produces a different address rather than a different spelling of the
/// same one. A v6 literal is left alone for the stronger reason that its
/// separator is not `.` at all; a v4 literal is recognised by its labels
/// all being numeric, which no registrable name can be.
fn reverse_labels(domain: &str) -> String {
    if is_ip_literal(domain) {
        return domain.to_owned();
    }
    let mut out = String::with_capacity(domain.len());
    for label in domain.rsplit('.') {
        if !out.is_empty() {
            out.push('.');
        }
        out.push_str(label);
    }
    out
}

/// Whether `host` is an address rather than a name.
///
/// Deliberately not a parse: what this decides is whether reversing the
/// labels would change the meaning, and every shape that would is caught
/// by *contains a colon* or *every label is numeric*. A hostname cannot
/// be either — RFC 1123 allows a leading digit but not an all-numeric
/// TLD.
fn is_ip_literal(host: &str) -> bool {
    host.contains(':')
        || (!host.is_empty()
            && host
                .split('.')
                .all(|l| !l.is_empty() && l.bytes().all(|b| b.is_ascii_digit())))
}

// ---- the encoding ------------------------------------------------------
//
// Written by hand rather than with a serialisation crate, for the reason
// this workspace removed `url` and hand-wrote base64: it is a handful of
// lines whose defects are loud, `serde` is behind the `json` feature and
// is not in this crate's default graph, and `SystemTime` has no `Serialize`
// of its own anyway.
//
// The format is the one a reader can check by eye:
//
//     <expires_at, seconds since the epoch, 8 bytes little-endian>
//     <include_subdomains, 1 byte, 0 or 1>
//     <domain, UTF-8, the rest>
//
// Fixed-width fields first so that decoding needs no delimiter, and the
// domain last so that it needs no length. A pre-epoch expiry is not
// representable and is stored as the epoch itself — a value already in
// the past, which is the under-claiming direction: an entry that reads
// back as expired stops upgrading, where one that read back as far in
// the future would keep upgrading a host whose policy had lapsed.

/// How many bytes the fixed-width head takes.
const HEAD: usize = 9;

fn encode(entry: &Entry) -> Vec<u8> {
    let secs = entry
        .expires_at()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or(Duration::ZERO)
        .as_secs();

    let domain = entry.domain().as_bytes();
    let mut out = Vec::with_capacity(HEAD + domain.len());
    out.extend_from_slice(&secs.to_le_bytes());
    out.push(u8::from(entry.include_subdomains()));
    out.extend_from_slice(domain);
    out
}

/// Seconds since the epoch as an instant, or `None` where the platform
/// cannot represent it.
///
/// **`checked_add`, and this is a defect this decoder shipped with.**
/// Every one of these values came out of a store and none is trusted, so
/// a length or a count that is wrong is refused — and a *timestamp* that
/// is wrong was being added to `UNIX_EPOCH` unchecked, which panics
/// rather than refusing. `SystemTime`'s range is the platform's: on
/// Windows it is narrower than on Linux, so `u64::MAX` seconds overflows
/// there and does not here, and the test that feeds this decoder rubbish
/// passed on every machine this workspace runs and failed on
/// `test (windows-latest)`.
///
/// Refusing loses one entry; panicking takes the caller's thread down
/// for a value a store handed back, which is the one outcome a decoder
/// written to refuse must not have.
fn at_epoch_plus(secs: u64) -> Option<SystemTime> {
    SystemTime::UNIX_EPOCH.checked_add(Duration::from_secs(secs))
}

/// The inverse of [`encode`], or `None` for bytes it did not write.
///
/// A store can hand back anything — a truncated write, a value from an
/// older format, another program's key — so this refuses rather than
/// assuming. Refusing loses one policy and re-upgrades nothing, which is
/// the safe direction; a decoder that guessed could hand
/// [`Hsts`](super::Hsts) a policy for a domain nobody asserted.
fn decode(bytes: &[u8]) -> Option<Entry> {
    let (head, domain) = bytes.split_at_checked(HEAD)?;
    let secs = u64::from_le_bytes(head[..8].try_into().ok()?);
    let include_subdomains = match head[8] {
        0 => false,
        1 => true,
        // Not `!= 0`: this byte is written as 0 or 1 by `encode`, so
        // anything else is a value this decoder did not write, and
        // reading it as `true` would assert `includeSubDomains` over a
        // whole subtree on the strength of a corrupt byte.
        _ => return None,
    };
    let domain = std::str::from_utf8(domain).ok()?;
    if domain.is_empty() {
        return None;
    }
    Some(Entry::new(domain, at_epoch_plus(secs)?, include_subdomains))
}

// Maintainer notes (not rendered):
//
// **A named type rather than a boxed one, and that is the finding
// rather than a style.** `Pin<Box<dyn Future<Output = _>>>` was the
// first shape here, with a doc comment claiming `Send` would be
// *inferred from `K`*. It is not: a `dyn` that declares no auto
// traits does not hide `Send`, it **removes** it — so
// [`ClientBuilder::hsts`](crate::ClientBuilder::hsts), which asks
// `for<'a> S::Get<'a>: Send`, refused a store whose futures were
// `Send` the whole time, and `Hsts::new()` stopped being usable with
// a `Client` at all. This workspace has now met that shape five
// times.
//
// Naming it makes the property real: a byte store answering
// [`Ready`](std::future::Ready) yields a `Send` future here with
// nothing declared, and one holding an `Rc` yields a `!Send` one and
// stays usable outside a `Client`. Amendment C15 from one more
// direction — and it allocates nothing, where the box allocated per
// call.

pin_project_lite::pin_project! {
    /// [`KvStore`]'s answer to [`HstsStore::get`]: the byte store's own
    /// future with this module's decoding applied to what it yields.
    ///
    /// A named type rather than a boxed one: a byte store answering
    /// [`Ready`](std::future::Ready) yields a `Send` future here with
    /// nothing declared, and one holding an `Rc` yields a `!Send` one and
    /// stays usable outside a `Client`. It allocates nothing, where a
    /// boxed future would allocate per call.
    #[derive(Debug)]
    pub struct Get<F> {
        #[pin]
        inner: F,
    }
}

impl<F: Future<Output = Vec<Vec<Vec<u8>>>>> Future for Get<F> {
    type Output = Vec<Entry>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        self.project().inner.poll(cx).map(|found| {
            found
                .into_iter()
                .flatten()
                // A value this wrapper did not write is **skipped**, never
                // repaired: handing `Hsts` a policy nobody asserted is the
                // one outcome worse than losing one.
                .filter_map(|bytes| decode(&bytes))
                .collect()
        })
    }
}

pin_project_lite::pin_project! {
    /// [`KvStore`]'s answer to [`HstsStore::put`]: a remove, and then the
    /// write.
    ///
    /// Two steps because the byte seam **appends** — see
    /// [`HstsStore::put`]'s own note on why replacement is the wrapper's
    /// question. Named for [`Get`]'s reason, so that `Send` follows from
    /// the byte store rather than being declared here.
    #[project = PutProj]
    #[derive(Debug)]
    pub enum Put<'a, K: KeyValueStore> {
        Removing {
            #[pin]
            removing: K::Done<'a>,
            store: &'a K,
            key: String,
            value: Vec<u8>,
        },
        Writing {
            #[pin]
            writing: K::Done<'a>,
        },
    }
}

impl<K: KeyValueStore<Instant = SystemTime>> Future for Put<'_, K> {
    type Output = ();

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        loop {
            match self.as_mut().project() {
                PutProj::Removing {
                    removing,
                    store,
                    key,
                    value,
                } => {
                    std::task::ready!(removing.poll(cx));
                    let writing = store.put(
                        NS,
                        std::mem::take(key),
                        std::mem::take(value),
                        // No expiry, deliberately — see [`KvStore`].
                        None,
                        SystemTime::UNIX_EPOCH,
                    );
                    self.set(Put::Writing { writing });
                }
                PutProj::Writing { writing } => return writing.poll(cx),
            }
        }
    }
}

impl<K: KeyValueStore<Instant = SystemTime>> HstsStore for KvStore<K> {
    type Get<'a>
        = Get<K::GetMany<'a>>
    where
        Self: 'a;
    type Done<'a>
        = Put<'a, K>
    where
        Self: 'a;

    fn get<'a>(&'a self, domains: &'a [String]) -> Self::Get<'a> {
        // One call rather than one per candidate: §8.2 asks about every
        // ancestor of a host at once, and a remote store should pay one
        // round trip for a request.
        //
        // `now` is the epoch because this wrapper stores no expiry — see
        // [`KvStore`]. Passing a real clock would hide nothing extra and
        // would need one, which this module has not got.
        let keys = domains.iter().map(|d| reverse_labels(d)).collect();
        Get {
            inner: self.kv.get_many(NS, keys, SystemTime::UNIX_EPOCH),
        }
    }

    fn put(&self, entry: Entry) -> Self::Done<'_> {
        let key = reverse_labels(entry.domain());
        Put::Removing {
            // **Removed before it is written, because the byte seam
            // appends.** §8.1's second bullet is an *update* of one
            // host's cached information, so two entries under one name is
            // a state RFC 6797 has no reading for.
            //
            // The pair is not atomic, and a store shared between
            // processes can be read between the two — answering nothing
            // for a domain that has a policy. That loses an upgrade for
            // the width of one write rather than inventing one, and
            // closing it needs a compare-and-set the byte seam has not
            // got.
            removing: self.kv.remove(NS, key.clone()),
            store: &self.kv,
            value: encode(&entry),
            key,
        }
    }

    fn remove<'a>(&'a self, domain: &'a str) -> Self::Done<'a> {
        let key = reverse_labels(domain);
        // The same two-step type, with an empty write: `remove` is
        // `put`'s first half, and one future type per `Done` is what the
        // seam asks for.
        Put::Writing {
            writing: self.kv.remove(NS, key),
        }
    }

    fn clear(&self) -> Self::Done<'_> {
        Put::Writing {
            writing: self.kv.clear(NS),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hsts::Hsts;
    use futures_executor::block_on;
    use hclient_core::kv::MemoryStore as Kv;

    fn at(secs: u64) -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_secs(secs)
    }

    fn store() -> KvStore<Kv<SystemTime>> {
        KvStore::new(Kv::new())
    }

    fn entry(domain: &str, expires: u64, subs: bool) -> Entry {
        Entry::new(domain, at(expires), subs)
    }

    // ---- the encoding ------------------------------------------------

    /// Every field survives the round trip. Written over several shapes
    /// rather than one, because a decoder that dropped a field would
    /// still round-trip whichever value happened to be its default.
    #[test]
    fn every_field_survives_the_round_trip() {
        for e in [
            entry("example.com", 0, false),
            entry("a.b.example.com", 1, true),
            entry("xn--mnchen-3ya.de", 4_102_444_800, false),
            entry("127.0.0.1", u64::from(u32::MAX), true),
        ] {
            assert_eq!(decode(&encode(&e)).as_ref(), Some(&e), "{e:?}");
        }
    }

    /// A timestamp too large for this platform is refused rather than
    /// panicking — see `cookie::kv`'s test of the same name.
    #[test]
    fn a_timestamp_that_overflows_the_clock_is_refused_rather_than_panicking() {
        let mut bytes = encode(&entry("example.com", 100, false));
        bytes[..8].copy_from_slice(&u64::MAX.to_le_bytes());
        assert!(decode(&bytes).is_none());
    }

    /// **A store hands back whatever it holds**, including bytes this
    /// encoder never wrote — a truncated write, another program's key,
    /// an older format. Refusing loses one policy; guessing would assert
    /// a policy for a domain nobody asserted.
    #[test]
    fn bytes_this_encoder_did_not_write_are_refused() {
        let good = encode(&entry("example.com", 10, true));

        assert_eq!(decode(&[]), None, "empty");
        assert_eq!(decode(&good[..HEAD - 1]), None, "shorter than the head");
        assert_eq!(decode(&good[..HEAD]), None, "a head with no domain");
        assert_eq!(decode(&[0xff; 32]), None, "not a flag byte, not UTF-8");

        let mut bad_utf8 = good.clone();
        bad_utf8.truncate(HEAD);
        bad_utf8.push(0xff);
        assert_eq!(decode(&bad_utf8), None, "a domain that is not UTF-8");
    }

    /// The flag byte is read as `0` or `1` and never as `!= 0`, because
    /// a corrupt byte read as `true` asserts `includeSubDomains` over a
    /// whole subtree.
    #[test]
    fn a_flag_byte_that_is_neither_zero_nor_one_is_refused() {
        let mut bytes = encode(&entry("example.com", 10, false));
        bytes[8] = 2;
        assert_eq!(decode(&bytes), None);
    }

    // ---- the key -----------------------------------------------------

    #[test]
    fn a_name_is_reversed_label_by_label() {
        assert_eq!(reverse_labels("a.b.example.com"), "com.example.b.a");
        assert_eq!(reverse_labels("example.com"), "com.example");
        assert_eq!(reverse_labels("localhost"), "localhost");
    }

    /// **An IP literal is not a name and reversing one changes the
    /// address.** The hazard the design named: a reversal applied in one
    /// direction and not the other would let one host read another's
    /// policies.
    #[test]
    fn an_ip_literal_is_left_alone() {
        for host in ["127.0.0.1", "8.8.8.8", "::1", "2001:db8::1", "1.0.0.127"] {
            assert_eq!(reverse_labels(host), host, "{host}");
        }
    }

    /// The pair that says the literal rule discriminates rather than
    /// merely existing: `1.0.0.127` and `127.0.0.1` are different
    /// addresses and must stay different keys, where reversing either
    /// would collapse them onto each other.
    #[test]
    fn two_reversed_ip_literals_do_not_collide() {
        assert_ne!(reverse_labels("127.0.0.1"), reverse_labels("1.0.0.127"));
    }

    /// A name whose labels are digits does not exist — RFC 1123 forbids
    /// an all-numeric TLD — so treating it as a literal costs nothing,
    /// and the near neighbour that *is* a name must still be reversed.
    #[test]
    fn a_name_beginning_with_a_digit_is_still_a_name() {
        assert_eq!(reverse_labels("1.example.com"), "com.example.1");
        assert_eq!(reverse_labels("3com.net"), "net.3com");
    }

    // ---- the seam's own obligations ----------------------------------

    #[test]
    fn a_stored_entry_comes_back() {
        let s = store();
        block_on(s.put(entry("example.com", 100, true)));

        let got = block_on(s.get(&["com.example".to_owned()]));
        assert!(got.is_empty(), "the key is reversed, the query is not");

        let got = block_on(s.get(&["example.com".to_owned()]));
        assert_eq!(got, vec![entry("example.com", 100, true)]);
    }

    /// **Obligation 2: `put` replaces by domain.** §8.1's second bullet
    /// is an *update*, and two entries under one name is a state RFC
    /// 6797 has no reading for — so the wrapper removes before it
    /// writes, because the byte seam appends.
    #[test]
    fn a_second_put_for_one_domain_replaces_rather_than_accumulating() {
        let s = store();
        block_on(s.put(entry("example.com", 100, true)));
        block_on(s.put(entry("example.com", 200, false)));

        assert_eq!(
            block_on(s.get(&["example.com".to_owned()])),
            vec![entry("example.com", 200, false)],
            "one entry, and it is the second"
        );
    }

    /// **Obligation 1: `get` answers exact matches and nothing else.** A
    /// store applying a suffix rule of its own would be answering a
    /// question §8.2 has already answered — and the reversed key makes
    /// that mistake *easy*, since an ancestor's key really is a prefix
    /// of its descendant's.
    #[test]
    fn get_answers_exact_names_and_never_a_prefix_match() {
        let s = store();
        block_on(s.put(entry("example.com", 100, true)));

        assert!(
            block_on(s.get(&["a.example.com".to_owned()])).is_empty(),
            "a descendant is not an exact match, however the key is spelled"
        );
        assert!(
            block_on(s.get(&["com".to_owned()])).is_empty(),
            "nor is an ancestor"
        );
    }

    /// The batch answers every candidate at once, which is what
    /// `get_many` exists for — and only the ones that are held.
    #[test]
    fn a_batch_of_candidates_answers_only_the_domains_held() {
        let s = store();
        block_on(s.put(entry("example.com", 100, true)));
        block_on(s.put(entry("a.b.example.com", 200, false)));

        let mut got = block_on(s.get(&[
            "a.b.example.com".to_owned(),
            "b.example.com".to_owned(),
            "example.com".to_owned(),
            "com".to_owned(),
        ]));
        got.sort_by(|x, y| x.domain().cmp(y.domain()));

        assert_eq!(
            got,
            vec![
                entry("a.b.example.com", 200, false),
                entry("example.com", 100, true),
            ]
        );
    }

    /// **A value this wrapper did not write is skipped, not repaired.**
    /// [`decode`]'s refusal has to *reach* the answer: a `get` that
    /// substituted anything for a value it could not read would hand
    /// [`Hsts`](super::Hsts) a policy nobody asserted, which is the one
    /// thing worse than losing one.
    ///
    /// Written through the byte store directly, because nothing above
    /// this wrapper can produce such a value — which is the point: a
    /// store outlives a format, and is also read by whatever wrote into
    /// it before.
    #[test]
    fn a_value_the_decoder_refuses_is_skipped_rather_than_substituted() {
        let kv = Kv::<SystemTime>::new();
        // Under the key `com.example` would be for `example.com`.
        block_on(kv.put(
            "hsts",
            "com.example".to_owned(),
            b"not an entry".to_vec(),
            None,
            at(0),
        ));
        let s = KvStore::new(kv);

        assert!(
            block_on(s.get(&["example.com".to_owned()])).is_empty(),
            "rubbish under a key answers nothing at all"
        );
    }

    /// And the neighbour survives it: one unreadable value must not cost
    /// the readable ones sharing the request.
    #[test]
    fn one_unreadable_value_does_not_hide_the_readable_ones_beside_it() {
        let kv = Kv::<SystemTime>::new();
        block_on(kv.put(
            "hsts",
            "com.example".to_owned(),
            b"rubbish".to_vec(),
            None,
            at(0),
        ));
        let s = KvStore::new(kv);
        block_on(s.put(entry("other.test", 100, true)));

        assert_eq!(
            block_on(s.get(&["example.com".to_owned(), "other.test".to_owned()])),
            vec![entry("other.test", 100, true)]
        );
    }

    #[test]
    fn remove_forgets_one_domain_and_leaves_its_neighbours() {
        let s = store();
        block_on(s.put(entry("example.com", 100, true)));
        block_on(s.put(entry("other.test", 100, false)));

        block_on(s.remove("example.com"));

        assert!(block_on(s.get(&["example.com".to_owned()])).is_empty());
        assert_eq!(
            block_on(s.get(&["other.test".to_owned()])),
            vec![entry("other.test", 100, false)]
        );
    }

    #[test]
    fn clear_forgets_every_policy() {
        let s = store();
        block_on(s.put(entry("example.com", 100, true)));
        block_on(s.put(entry("other.test", 100, false)));

        block_on(s.clear());

        assert!(block_on(s.get(&["example.com".to_owned(), "other.test".to_owned()])).is_empty());
    }

    /// **`clear` is this namespace's alone**, which is what lets one byte
    /// store back all four seams: a caller dropping its HSTS set must not
    /// drop its cookies with it.
    #[test]
    fn clear_leaves_another_namespace_alone() {
        let kv = Kv::<SystemTime>::new();
        block_on(kv.put(
            "cookie",
            "com.example".to_owned(),
            b"c".to_vec(),
            None,
            at(0),
        ));
        let s = KvStore::new(kv);
        block_on(s.put(entry("example.com", 100, true)));

        block_on(s.clear());

        assert_eq!(
            block_on(s.inner().get("cookie", "com.example", at(0))),
            vec![b"c".to_vec()],
            "the cookie namespace is untouched"
        );
    }

    // ---- what the wrapper must NOT do --------------------------------

    /// **An expired entry is still held and still answered**, because
    /// §8.1.1's eviction belongs to [`Hsts::is_known`] and handing the
    /// expiry to the byte store would make that eviction unobservable —
    /// the entry would stop being answered whether or not the rule ran.
    ///
    /// So this asserts the *absence* of a behaviour, which is unusual
    /// and is the point: it is what keeps
    /// `an_expired_policy_does_not_upgrade_and_is_evicted` next door a
    /// check that can fail.
    #[test]
    fn an_expired_entry_is_answered_by_the_store_and_evicted_above_it() {
        let s = store();
        block_on(s.put(entry("example.com", 100, false)));

        assert_eq!(
            block_on(s.get(&["example.com".to_owned()])),
            vec![entry("example.com", 100, false)],
            "the store holds it long past its expiry; the rules above read it"
        );
    }

    /// And the whole of §8.1.1 over this store, end to end: the policy
    /// stops upgrading **and** the entry is gone — the second half being
    /// what the test above keeps meaningful.
    #[test]
    fn the_rules_above_still_evict_an_expired_policy_through_this_store() {
        let h = Hsts::with_store(store());
        let mut headers = http::HeaderMap::new();
        headers.insert(
            "strict-transport-security",
            http::HeaderValue::from_static("max-age=100"),
        );
        block_on(h.note(
            &"https://example.com/".parse().unwrap(),
            &headers,
            true,
            at(0),
        ));

        assert_eq!(
            block_on(h.upgrade(&"http://example.com/".parse().unwrap(), at(101))),
            None,
            "expired, so no upgrade"
        );
        assert!(
            block_on(h.store().get(&["example.com".to_owned()])).is_empty(),
            "and evicted, which is §8.1.1 rather than the store hiding it"
        );
    }
}
