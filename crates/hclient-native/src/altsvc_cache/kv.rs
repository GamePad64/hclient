//! [`AltSvcStore`] over the byte seam.
//!
//! The argument lives on [`KvStore`] itself, because this module is
//! private and rustdoc publishes nothing from it.

use super::{AltSvcStore, Entry, Origin};
use hclient_core::kv::KeyValueStore;
use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::Duration;
use web_time::SystemTime;

/// The two namespaces this wrapper writes under, and the **`persist`
/// flag is which one** rather than a field of the value.
///
/// See [`KvStore`] for why.
const VOLATILE: &str = "altsvc";
const PERSISTENT: &str = "altsvc-persist";

/// An [`AltSvcStore`] over any [`KeyValueStore`], so an `Alt-Svc` memory
/// can share one backend with the client's cookie jar, HSTS set and
/// response cache instead of being a fifth `Mutex<HashMap>`.
///
/// # `persist` is in the key, and that is the whole design
///
/// RFC 7838 §2.2 asks a client to forget, on a network change, every
/// advertisement that did not carry `persist=1`. Over a map that is a
/// `retain` with a predicate; over a byte store it cannot be, because the
/// seam does not know what a `persist` flag is and will not grow a
/// `retain` that takes one — a predicate crossing that boundary is the
/// domain vocabulary the seam exists to keep out.
///
/// So the flag stops being part of the *value* and becomes part of the
/// **address**: a volatile advertisement is written under one namespace
/// and a persistent one under another, and §2.2 is then
/// [`clear`](KeyValueStore::clear) of the first — one operation, exact,
/// and cheap on a remote store where a `retain` would have meant reading
/// every entry back.
///
/// It also reads as what the RFC says. *"Forget the ones that did not ask
/// to survive"* is a statement about a **set**, and this makes the set a
/// thing that exists rather than a filter applied to a bigger one.
///
/// What it costs is that an origin can be written under either name, so
/// every read asks both and every write removes from both. Two operations
/// where a map had one — bounded, constant, and paid on a path that is
/// about to open a connection.
///
/// # Expiry is handed to the byte store here, unlike [`hsts`]
///
/// `hclient::hsts` deliberately keeps its expiry above the seam, because
/// RFC 6797 §8.1.1 makes eviction an observable duty of the rules and a
/// store that hid expired entries would make that duty unobservable.
/// Nothing here has that shape: [`AltSvcCache`](super::AltSvcCache)
/// compares `expires_at` itself on the way out and never removes on the
/// strength of it, so letting the store drop what has passed is a saving
/// with no rule behind it to hide. The entry still carries its own
/// expiry, so a store that ignores the hint stays correct.
///
/// [`hsts`]: https://docs.rs/hclient/latest/hclient/hsts/
#[derive(Debug, Clone, Default)]
pub struct KvStore<K> {
    kv: K,
}

impl<K> KvStore<K> {
    /// An `Alt-Svc` store backed by `kv`.
    pub const fn new(kv: K) -> Self {
        Self { kv }
    }

    /// The byte store underneath, for a caller that shares one between
    /// seams and wants to reach it.
    pub const fn inner(&self) -> &K {
        &self.kv
    }
}

/// The key an origin is written under: its host with the labels
/// reversed, then the port.
///
/// Reversing is the design's rule for every wrapper — it puts one host's
/// entries in one range of an ordered store. The port follows because an
/// origin is a `(host, port)` pair and §2.2 scopes an advertisement to
/// both.
///
/// **An IP literal is never reversed**, because a reversed `1.0.0.127` is
/// a different address. Unlike HSTS, which refuses to note one at all, an
/// `Alt-Svc` origin really can be a literal — a server at an address can
/// advertise — so this is reachable rather than defensive.
fn key_for(origin: &Origin) -> String {
    let host = &origin.host;
    let host = if is_ip_literal(host) {
        host.to_string()
    } else {
        let mut out = String::with_capacity(host.len());
        for label in host.rsplit('.') {
            if !out.is_empty() {
                out.push('.');
            }
            out.push_str(label);
        }
        out
    };
    // `\u{1}` as the separator, for the reason the design records: it
    // cannot occur in a host or in a decimal port, so nothing needs
    // escaping and there is no escape to get wrong.
    format!("{host}\u{1}{}", origin.port)
}

/// Whether `host` is an address rather than a name — see
/// [`key_for`]. Written as *would reversing change the meaning* rather
/// than as a parse.
fn is_ip_literal(host: &str) -> bool {
    host.contains(':')
        || (!host.is_empty()
            && host
                .split('.')
                .all(|l| !l.is_empty() && l.bytes().all(|b| b.is_ascii_digit())))
}

// ---- the encoding ------------------------------------------------------
//
// Eight bytes and nothing else: `persist` is in the namespace, so the
// value is the expiry alone. Hand-written for `hclient::hsts::kv`'s
// reason — a handful of lines whose defects are loud, against a
// serialisation crate this graph does not carry.
//
// A pre-epoch expiry is not representable and is stored as the epoch,
// which is already past. That is the under-claiming direction: an entry
// that reads back as expired stops being offered, where one reading far
// into the future would keep routing to an alt-authority whose lease had
// lapsed.

fn encode(entry: Entry) -> Vec<u8> {
    entry
        .expires_at()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or(Duration::ZERO)
        .as_secs()
        .to_le_bytes()
        .to_vec()
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
/// A store can hand back anything — a truncated write, an older format,
/// another program's key — so this refuses rather than assuming.
/// Refusing costs one advertisement and falls back to TCP, which is the
/// safe direction; a decoder that guessed could route a request to an
/// alt-authority nobody advertised.
fn decode(bytes: &[u8], persist: bool) -> Option<Entry> {
    let secs = u64::from_le_bytes(<[u8; 8]>::try_from(bytes).ok()?);
    Some(Entry::new(at_epoch_plus(secs)?, persist))
}

pin_project_lite::pin_project! {
    /// [`KvStore`]'s answer to [`AltSvcStore::get`]: the persistent
    /// namespace asked first, and the volatile one only if it answered
    /// nothing.
    ///
    /// **Two calls rather than one**, because
    /// [`get_many`](KeyValueStore::get_many) takes a single namespace and
    /// this wrapper keeps `persist` in the namespace — see [`KvStore`].
    /// The second is skipped on a hit, so a persistent advertisement
    /// costs one round trip and a miss costs two.
    ///
    /// **Named rather than boxed**, so `Send` is *inferred* from `K`
    /// rather than declared — a `dyn` with no auto traits removes the
    /// property instead of hiding it, which is the defect
    /// `hclient::hsts::kv` shipped with for one commit.
    #[project = GetProj]
    #[derive(Debug)]
    pub enum Get<'a, K: KeyValueStore> {
        Persistent {
            #[pin]
            running: K::GetMany<'a>,
            store: &'a K,
            key: String,
        },
        Volatile {
            #[pin]
            running: K::GetMany<'a>,
        },
    }
}

impl<K: KeyValueStore<Instant = SystemTime>> Future for Get<'_, K> {
    type Output = Option<Entry>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        loop {
            match self.as_mut().project() {
                GetProj::Persistent {
                    running,
                    store,
                    key,
                } => {
                    let found = std::task::ready!(running.poll(cx));
                    if let Some(entry) = found.into_iter().flatten().find_map(|b| decode(&b, true))
                    {
                        return Poll::Ready(Some(entry));
                    }
                    let running =
                        store.get_many(VOLATILE, vec![std::mem::take(key)], SystemTime::UNIX_EPOCH);
                    self.set(Get::Volatile { running });
                }
                GetProj::Volatile { running } => {
                    return running.poll(cx).map(|found| {
                        found
                            .into_iter()
                            .flatten()
                            // A value this wrapper did not write is
                            // **skipped**, never repaired: routing a
                            // request to an alt-authority nobody
                            // advertised is worse than losing one.
                            .find_map(|b| decode(&b, false))
                    });
                }
            }
        }
    }
}

pin_project_lite::pin_project! {
    /// [`KvStore`]'s answer to every method that changes something: up to
    /// three byte-store calls, run in order.
    ///
    /// Three because a write is *remove from both namespaces, then write
    /// to one* — [`KvStore`] says why the flag is in the key — and
    /// because the seam appends rather than replacing. Named for
    /// [`Get`]'s reason.
    #[project = DoneProj]
    #[derive(Debug)]
    pub enum Done<'a, K: KeyValueStore> {
        Step {
            #[pin]
            running: K::Done<'a>,
            store: &'a K,
            // What to do once `running` finishes, in order.
            rest: Vec<Step>,
        },
    }
}

/// One byte-store call a [`Done`] still owes.
#[derive(Debug)]
pub enum Step {
    /// Remove `key` from `ns`.
    Remove { ns: &'static str, key: String },
    /// Write `value` under `key` in `ns`, expiring at `expires`.
    Put {
        ns: &'static str,
        key: String,
        value: Vec<u8>,
        expires: SystemTime,
    },
}

impl<K: KeyValueStore<Instant = SystemTime>> Future for Done<'_, K> {
    type Output = ();

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        loop {
            let DoneProj::Step {
                running,
                store,
                rest,
            } = self.as_mut().project();
            std::task::ready!(running.poll(cx));
            if rest.is_empty() {
                return Poll::Ready(());
            }
            let store = *store;
            let next = rest.remove(0);
            let rest = std::mem::take(rest);
            let running = match next {
                Step::Remove { ns, key } => store.remove(ns, key),
                Step::Put {
                    ns,
                    key,
                    value,
                    expires,
                } => store.put(ns, key, value, Some(expires), SystemTime::UNIX_EPOCH),
            };
            self.set(Done::Step {
                running,
                store,
                rest,
            });
        }
    }
}

impl<K: KeyValueStore<Instant = SystemTime>> AltSvcStore for KvStore<K> {
    type Get<'a>
        = Get<'a, K>
    where
        Self: 'a;
    type Done<'a>
        = Done<'a, K>
    where
        Self: 'a;

    fn get<'a>(&'a self, origin: &'a Origin) -> Self::Get<'a> {
        let key = key_for(origin);
        // Both namespaces in one call rather than two, so a remote store
        // pays one round trip — which is the whole reason `get_many` is
        // on the seam.
        //
        // `now` is the epoch: the byte store may drop what it knows has
        // expired, and `AltSvcCache` compares `expires_at` itself on the
        // way out, so asking the store to filter would be a second
        // reading of one rule. Handing it a real clock needs one, and
        // this module has not got one.
        Get::Persistent {
            running: self
                .kv
                .get_many(PERSISTENT, vec![key.clone()], SystemTime::UNIX_EPOCH),
            store: &self.kv,
            key,
        }
    }

    fn put<'a>(&'a self, origin: &'a Origin, entry: Entry) -> Self::Done<'a> {
        let key = key_for(origin);
        let (write_to, clear_from) = if entry.persist() {
            (PERSISTENT, VOLATILE)
        } else {
            (VOLATILE, PERSISTENT)
        };
        // Three steps, and the order is what makes a re-advertisement
        // that flips `persist` land in exactly one namespace: clear the
        // other, clear this one (the seam appends, and §3 makes a
        // present field *replace* what was there), then write.
        Done::Step {
            running: self.kv.remove(clear_from, key.clone()),
            store: &self.kv,
            rest: vec![
                Step::Remove {
                    ns: write_to,
                    key: key.clone(),
                },
                Step::Put {
                    ns: write_to,
                    key,
                    value: encode(entry),
                    expires: entry.expires_at(),
                },
            ],
        }
    }

    fn remove<'a>(&'a self, origin: &'a Origin) -> Self::Done<'a> {
        let key = key_for(origin);
        // Both, because an origin may have been written under either and
        // a caller asking to forget it means both.
        Done::Step {
            running: self.kv.remove(VOLATILE, key.clone()),
            store: &self.kv,
            rest: vec![Step::Remove {
                ns: PERSISTENT,
                key,
            }],
        }
    }

    fn retain_persistent(&self) -> Self::Done<'_> {
        // RFC 7838 §2.2, and the reason `persist` is in the key: this is
        // one `clear` rather than a read of every entry and a predicate.
        Done::Step {
            running: self.kv.clear(VOLATILE),
            store: &self.kv,
            rest: Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures_executor::block_on;
    use hclient_core::kv::MemoryStore as Kv;

    fn at(secs: u64) -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_secs(secs)
    }

    fn store() -> KvStore<Kv<SystemTime>> {
        KvStore::new(Kv::new())
    }

    fn origin(host: &str, port: u16) -> Origin {
        Origin::new(host, port)
    }

    // ---- the encoding ------------------------------------------------

    /// The expiry survives; `persist` deliberately does **not** travel in
    /// the value, because it is the namespace — so `decode` is told which
    /// one answered.
    #[test]
    fn the_expiry_survives_the_round_trip_and_persist_comes_from_the_namespace() {
        for secs in [0, 1, 4_102_444_800, u64::from(u32::MAX)] {
            let e = Entry::new(at(secs), false);
            let bytes = encode(e);
            assert_eq!(decode(&bytes, false), Some(Entry::new(at(secs), false)));
            assert_eq!(
                decode(&bytes, true),
                Some(Entry::new(at(secs), true)),
                "the same eight bytes, read out of the other namespace"
            );
        }
    }

    /// A timestamp too large for this platform is refused rather than
    /// panicking — the same defect `hclient::cookie::kv` records, found
    /// by `test (windows-latest)`.
    #[test]
    fn a_timestamp_that_overflows_the_clock_is_refused_rather_than_panicking() {
        let bytes = u64::MAX.to_le_bytes().to_vec();
        assert!(decode(&bytes, false).is_none());
    }

    /// A store hands back whatever it holds, including bytes this
    /// encoder never wrote. Refusing costs one advertisement and falls
    /// back to TCP; guessing would route a request to an alt-authority
    /// nobody advertised.
    #[test]
    fn bytes_this_encoder_did_not_write_are_refused() {
        assert_eq!(decode(&[], false), None, "empty");
        assert_eq!(decode(&[0; 7], false), None, "one byte short");
        assert_eq!(decode(&[0; 9], false), None, "one byte long");
    }

    // ---- the key -----------------------------------------------------

    #[test]
    fn a_name_is_reversed_and_carries_its_port() {
        assert_eq!(
            key_for(&origin("a.b.example.com", 443)),
            "com.example.b.a\u{1}443"
        );
        assert_eq!(
            key_for(&origin("example.com", 8443)),
            "com.example\u{1}8443"
        );
    }

    /// **An IP literal is not reversed**, and unlike HSTS this is
    /// reachable rather than defensive: a server at an address really can
    /// advertise an alternative.
    #[test]
    fn an_ip_literal_keeps_its_order() {
        assert_eq!(key_for(&origin("127.0.0.1", 443)), "127.0.0.1\u{1}443");
        assert_eq!(key_for(&origin("::1", 443)), "::1\u{1}443");
    }

    /// The pair that says the literal rule discriminates: reversing
    /// either of these would collapse two addresses onto one key.
    #[test]
    fn two_reversed_ip_literals_do_not_collide() {
        assert_ne!(
            key_for(&origin("127.0.0.1", 443)),
            key_for(&origin("1.0.0.127", 443))
        );
    }

    /// The port is part of the origin, so one host on two ports is two
    /// entries — RFC 7838 §2.2 scopes an advertisement to both.
    #[test]
    fn one_host_on_two_ports_is_two_entries() {
        let s = store();
        block_on(s.put(&origin("example.com", 443), Entry::new(at(100), false)));

        assert!(block_on(s.get(&origin("example.com", 8443))).is_none());
        assert_eq!(
            block_on(s.get(&origin("example.com", 443))),
            Some(Entry::new(at(100), false))
        );
    }

    // ---- the seam ----------------------------------------------------

    #[test]
    fn a_stored_entry_comes_back_from_either_namespace() {
        for persist in [false, true] {
            let s = store();
            let o = origin("example.com", 443);
            block_on(s.put(&o, Entry::new(at(100), persist)));

            assert_eq!(
                block_on(s.get(&o)),
                Some(Entry::new(at(100), persist)),
                "persist={persist}"
            );
        }
    }

    #[test]
    fn an_unknown_origin_answers_nothing() {
        let s = store();
        assert!(block_on(s.get(&origin("example.com", 443))).is_none());
    }

    /// §3: a present `Alt-Svc` field **replaces** what was there. The
    /// byte seam appends, so the wrapper removes first — and without that
    /// a re-advertisement would leave two entries under one key.
    #[test]
    fn a_second_put_replaces_rather_than_accumulating() {
        let s = store();
        let o = origin("example.com", 443);
        block_on(s.put(&o, Entry::new(at(100), false)));
        block_on(s.put(&o, Entry::new(at(200), false)));

        assert_eq!(block_on(s.get(&o)), Some(Entry::new(at(200), false)));
    }

    /// **Flipping `persist` moves the entry between namespaces**, and
    /// leaves nothing behind in the one it came from. Without the cross
    /// removal an origin would be in both, and `retain_persistent` would
    /// then fail to forget a volatile advertisement that had once been
    /// persistent.
    #[test]
    fn re_advertising_with_a_different_persist_moves_the_entry() {
        let s = store();
        let o = origin("example.com", 443);

        block_on(s.put(&o, Entry::new(at(100), true)));
        block_on(s.put(&o, Entry::new(at(200), false)));
        assert_eq!(block_on(s.get(&o)), Some(Entry::new(at(200), false)));

        // And the other way round.
        block_on(s.put(&o, Entry::new(at(300), true)));
        assert_eq!(block_on(s.get(&o)), Some(Entry::new(at(300), true)));

        // The proof that nothing was left behind: forgetting the
        // volatile set must not bring an older entry back.
        block_on(s.retain_persistent());
        assert_eq!(block_on(s.get(&o)), Some(Entry::new(at(300), true)));
    }

    #[test]
    fn remove_forgets_an_origin_whichever_namespace_held_it() {
        for persist in [false, true] {
            let s = store();
            let o = origin("example.com", 443);
            block_on(s.put(&o, Entry::new(at(100), persist)));

            block_on(s.remove(&o));

            assert!(block_on(s.get(&o)).is_none(), "persist={persist}");
        }
    }

    /// **RFC 7838 §2.2, and the reason `persist` is in the key**: one
    /// `clear` forgets exactly the advertisements that did not ask to
    /// survive a network change.
    #[test]
    fn retain_persistent_keeps_the_persistent_and_drops_the_rest() {
        let s = store();
        let volatile = origin("volatile.test", 443);
        let persistent = origin("persistent.test", 443);
        block_on(s.put(&volatile, Entry::new(at(100), false)));
        block_on(s.put(&persistent, Entry::new(at(100), true)));

        block_on(s.retain_persistent());

        assert!(block_on(s.get(&volatile)).is_none());
        assert_eq!(
            block_on(s.get(&persistent)),
            Some(Entry::new(at(100), true))
        );
    }

    /// The namespaces are this wrapper's own: forgetting the volatile
    /// set must not reach a cookie jar sharing the same byte store.
    #[test]
    fn retain_persistent_leaves_another_seams_namespace_alone() {
        let kv = Kv::<SystemTime>::new();
        block_on(kv.put(
            "cookie",
            "com.example".to_owned(),
            b"c".to_vec(),
            None,
            at(0),
        ));
        let s = KvStore::new(kv);
        block_on(s.put(&origin("example.com", 443), Entry::new(at(100), false)));

        block_on(s.retain_persistent());

        assert_eq!(
            block_on(s.inner().get("cookie", "com.example", at(0))),
            vec![b"c".to_vec()]
        );
    }

    /// The byte store is told the expiry, so it may drop what has passed
    /// — a saving with no rule behind it to hide, unlike HSTS's.
    #[test]
    fn an_expired_entry_is_not_answered() {
        let s = store();
        let o = origin("example.com", 443);
        block_on(s.put(&o, Entry::new(at(100), false)));

        // `get` asks with the epoch, so this store answers it; what the
        // expiry buys is a store that sweeps on its own. Asserted through
        // the byte seam, which is where the hint lands.
        let key = key_for(&o);
        assert!(
            block_on(s.inner().get(VOLATILE, &key, at(101))).is_empty(),
            "the byte store hides it once its expiry has passed"
        );
        assert!(!block_on(s.inner().get(VOLATILE, &key, at(99))).is_empty());
    }
}
