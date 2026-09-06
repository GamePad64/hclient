//! RFC 6797 through the whole client: what a request's URI actually is by
//! the time a transport sees it.
//!
//! **The evidence in every test here is `RecordedRequest::uri`**, which is
//! what the transport was asked to fetch — not what the caller typed and
//! not what the response reports. That is the only place the property
//! this module exists for is visible: a unit test can show that
//! `Hsts::upgrade` answers `https://`, and only a test through `Client`
//! can show that the answer is what goes on the wire.
//!
//! The mock is the right instrument rather than a socket server, and for
//! once that is not about speed: a real server would have to *be* at
//! `https://`, so the test would need a certificate and a handshake to
//! observe a decision that is settled before either. The mock records the
//! URI and answers, which is exactly the observation.
#![cfg(all(feature = "hsts", feature = "test-util", not(target_family = "wasm")))]

use hclient::Client;
use hclient::hsts::{Entry, Hsts, HstsStore, MemoryStore};
use hclient::mock::MockTransport;

fn rt() -> tokio::runtime::Runtime {
    tokio::runtime::Runtime::new().expect("tokio runtime")
}

fn ok() -> http::Response<&'static str> {
    http::Response::builder().status(200).body("").unwrap()
}

/// A `200` that asserts a policy for whatever host answered it.
fn ok_with_sts(value: &str) -> http::Response<&'static str> {
    let mut r = http::Response::builder().status(200);
    r = r.header("strict-transport-security", value);
    r.body("").unwrap()
}

// ---- the upgrade reaches the transport ------------------------------

#[test]
fn a_known_host_is_requested_over_https_although_the_caller_wrote_http() {
    // The whole point of the module, asserted where it counts: the URI
    // the transport is handed.
    let rtx = rt();
    let m = MockTransport::new();
    m.push_response(ok());
    let store = MemoryStore::new();
    rtx.block_on(store.put(Entry::new(
        "a.test",
        std::time::SystemTime::now() + std::time::Duration::from_secs(3600),
        false,
    )));
    let c = Client::builder(m.clone())
        .hsts(Hsts::with_store(store))
        .build()
        .expect("build");

    rtx.block_on(async {
        let _ = c.get("http://a.test/path?q=1").send().await;
    });

    let reqs = m.requests();
    assert_eq!(reqs.len(), 1);
    assert_eq!(reqs[0].uri.to_string(), "https://a.test/path?q=1");
}

#[test]
fn an_unknown_host_reaches_the_transport_as_the_caller_wrote_it() {
    // The control. Without it the test above would pass for a client
    // that upgraded everything, which is a different — and much worse —
    // program: every `http://` request to a host that speaks no TLS
    // would fail.
    let rtx = rt();
    let m = MockTransport::new();
    m.push_response(ok());
    let c = Client::builder(m.clone())
        .hsts(Hsts::new())
        .build()
        .expect("build");

    rtx.block_on(async {
        let _ = c.get("http://a.test/path").send().await;
    });

    assert_eq!(m.requests()[0].uri.to_string(), "http://a.test/path");
}

#[test]
fn a_client_with_no_hsts_configured_upgrades_nothing() {
    // The second control, and it is about the *default* rather than
    // about the rules: the feature being compiled in must not change
    // what a caller who never called `.hsts(..)` sends. Cargo unifies
    // features, so this is the arm a neighbouring crate could otherwise
    // switch on for everybody.
    let rtx = rt();
    let m = MockTransport::new();
    m.push_response(ok_with_sts("max-age=3600"));
    m.push_response(ok());
    let c = Client::builder(m.clone()).build().expect("build");

    rtx.block_on(async {
        let _ = c.get("https://a.test/one").send().await;
        let _ = c.get("http://a.test/two").send().await;
    });

    assert_eq!(m.requests()[1].uri.to_string(), "http://a.test/two");
}

// ---- learning from a response ---------------------------------------

#[test]
fn a_policy_learned_over_https_upgrades_the_next_request() {
    // §8.1 and §8.3 end to end: the header arrives on one request and
    // changes where the next one goes.
    let rtx = rt();
    let m = MockTransport::new();
    m.push_response(ok_with_sts("max-age=3600"));
    m.push_response(ok());
    let c = Client::builder(m.clone())
        .hsts(Hsts::new())
        .build()
        .expect("build");

    rtx.block_on(async {
        let _ = c.get("https://a.test/one").send().await;
        let _ = c.get("http://a.test/two").send().await;
    });

    let reqs = m.requests();
    assert_eq!(reqs[0].uri.to_string(), "https://a.test/one");
    assert_eq!(reqs[1].uri.to_string(), "https://a.test/two");
}

#[test]
fn a_policy_asserted_over_http_is_ignored() {
    // §8.1: "If an HTTP response is received over insecure transport,
    // the UA MUST ignore any present STS header field(s)." The attack
    // this closes is the one that matters — an attacker who can rewrite
    // a plaintext response could otherwise assert a policy for a host
    // they do not own, or delete one the host really did assert.
    let rtx = rt();
    let m = MockTransport::new();
    m.push_response(ok_with_sts("max-age=3600"));
    m.push_response(ok());
    let c = Client::builder(m.clone())
        .hsts(Hsts::new())
        .build()
        .expect("build");

    rtx.block_on(async {
        let _ = c.get("http://a.test/one").send().await;
        let _ = c.get("http://a.test/two").send().await;
    });

    assert_eq!(m.requests()[1].uri.to_string(), "http://a.test/two");
}

// ---- redirects ------------------------------------------------------

#[test]
fn a_location_pointing_at_http_is_upgraded_before_it_is_followed() {
    // **§8.3's "including when following HTTP redirects", and the
    // sharpest test here.** A client that upgraded only the caller's own
    // URL would send hop 1 over TLS and hop 2 in clear text — which is
    // the downgrade HSTS exists against, arriving through the one door
    // the caller did not type.
    let rtx = rt();
    let m = MockTransport::new();
    m.push_response(
        http::Response::builder()
            .status(302)
            .header("strict-transport-security", "max-age=3600")
            .header("location", "http://a.test/two")
            .body("")
            .unwrap(),
    );
    m.push_response(ok());
    let c = Client::builder(m.clone())
        .hsts(Hsts::new())
        .build()
        .expect("build");

    rtx.block_on(async {
        let _ = c.get("https://a.test/one").send().await;
    });

    let reqs = m.requests();
    assert_eq!(reqs.len(), 2);
    assert_eq!(reqs[1].uri.to_string(), "https://a.test/two");
}

#[test]
fn a_redirect_to_a_second_known_host_is_upgraded_on_that_hosts_own_policy() {
    // The policy belongs to a **host**, not to an operation or an
    // origin: hop 1 goes to `a.test`, and hop 2's upgrade comes from
    // what `b.test` asserted on some earlier visit. A client that
    // scoped the policy set to the operation, or consulted it only for
    // the URL the caller typed, sends hop 2 in clear text.
    let rtx = rt();
    let m = MockTransport::new();
    m.push_response(
        http::Response::builder()
            .status(302)
            .header("location", "http://b.test/two")
            .body("")
            .unwrap(),
    );
    m.push_response(ok());
    let store = MemoryStore::new();
    let far = std::time::SystemTime::now() + std::time::Duration::from_secs(3600);
    rtx.block_on(store.put(Entry::new("b.test", far, false)));
    let c = Client::builder(m.clone())
        .hsts(Hsts::with_store(store))
        .build()
        .expect("build");

    rtx.block_on(async {
        let _ = c.get("http://a.test/one").send().await;
    });

    let reqs = m.requests();
    assert_eq!(reqs.len(), 2);
    // Hop 1 is untouched — `a.test` asserted nothing — which is what
    // makes hop 2's upgrade a fact about `b.test` rather than about the
    // client upgrading everything it sees.
    assert_eq!(reqs[0].uri.to_string(), "http://a.test/one");
    assert_eq!(reqs[1].uri.to_string(), "https://b.test/two");
}

// ---- the store is the caller's --------------------------------------

/// A store that is not this crate's, holding only plain data.
///
/// `ReloadingStore`'s counterpart one module over, and it asks the same
/// question: can an entry leave the process and come back? Everything it
/// keeps is a `String` and two primitives, rebuilt into an [`Entry`] on
/// every read — so a hit served out of it says the domain, the expiry and
/// the `includeSubDomains` flag all survived the journey.
#[derive(Default)]
struct OnDiskish {
    rows: std::sync::Mutex<Vec<(String, u64, bool)>>,
    reads: std::sync::atomic::AtomicUsize,
}

impl HstsStore for OnDiskish {
    type Get<'a> = std::future::Ready<Vec<Entry>>;
    type Done<'a> = std::future::Ready<()>;

    fn get<'a>(&'a self, domains: &'a [String]) -> Self::Get<'a> {
        self.reads
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let rows = self.rows.lock().unwrap();
        std::future::ready(
            rows.iter()
                .filter(|(d, _, _)| domains.iter().any(|q| q == d))
                .map(|(d, secs, sub)| {
                    Entry::new(
                        d,
                        std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(*secs),
                        *sub,
                    )
                })
                .collect(),
        )
    }

    fn put(&self, entry: Entry) -> Self::Done<'_> {
        let secs = entry
            .expires_at()
            .duration_since(std::time::SystemTime::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let mut rows = self.rows.lock().unwrap();
        rows.retain(|(d, _, _)| d != entry.domain());
        rows.push((entry.domain().to_owned(), secs, entry.include_subdomains()));
        std::future::ready(())
    }

    fn remove<'a>(&'a self, domain: &'a str) -> Self::Done<'a> {
        self.rows.lock().unwrap().retain(|(d, _, _)| d != domain);
        std::future::ready(())
    }

    fn clear(&self) -> Self::Done<'_> {
        self.rows.lock().unwrap().clear();
        std::future::ready(())
    }
}

#[test]
fn a_store_holding_only_plain_data_serves_the_upgrade() {
    // The seam's own claim, checked from outside the module: an entry
    // that was written down as three primitives and rebuilt still
    // upgrades a request.
    let rtx = rt();
    let m = MockTransport::new();
    m.push_response(ok_with_sts("max-age=3600; includeSubDomains"));
    m.push_response(ok());
    let c = Client::builder(m.clone())
        .hsts(Hsts::with_store(OnDiskish::default()))
        .build()
        .expect("build");

    rtx.block_on(async {
        let _ = c.get("https://example.test/one").send().await;
        // A *subdomain*, so the flag had to survive the round trip too.
        let _ = c.get("http://a.example.test/two").send().await;
    });

    assert_eq!(
        m.requests()[1].uri.to_string(),
        "https://a.example.test/two"
    );
}

#[test]
fn the_installed_store_is_the_one_consulted() {
    // The seam rather than the rules: an installed store must be what
    // answers, and a client that quietly kept its own would pass every
    // test above.
    let rtx = rt();
    let m = MockTransport::new();
    m.push_response(ok());
    let store = Counting::default();
    let c = Client::builder(m.clone())
        .hsts(Hsts::with_store(store.clone()))
        .build()
        .expect("build");

    rtx.block_on(async {
        let _ = c.get("http://a.test/x").send().await;
    });

    assert!(
        store.reads.load(std::sync::atomic::Ordering::Relaxed) > 0,
        "the installed store was never asked"
    );
}

/// A store that counts what it was asked.
///
/// The counter is behind its own `Arc` rather than the store being one:
/// `Client` takes the store by value, so the test needs a handle that
/// outlives the move, and the orphan rule forbids implementing this
/// crate's trait for `Arc<Counting>` from out here.
#[derive(Default, Clone)]
struct Counting {
    inner: std::sync::Arc<MemoryStore>,
    reads: std::sync::Arc<std::sync::atomic::AtomicUsize>,
}

impl HstsStore for Counting {
    type Get<'a> = std::future::Ready<Vec<Entry>>;
    type Done<'a> = std::future::Ready<()>;

    fn get<'a>(&'a self, domains: &'a [String]) -> Self::Get<'a> {
        self.reads
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        self.inner.get(domains)
    }
    fn put(&self, entry: Entry) -> Self::Done<'_> {
        self.inner.put(entry)
    }
    fn remove<'a>(&'a self, domain: &'a str) -> Self::Done<'a> {
        self.inner.remove(domain)
    }
    fn clear(&self) -> Self::Done<'_> {
        self.inner.clear()
    }
}

// ---- the policy set is readable back out ---------------------------

#[test]
fn the_policy_set_is_readable_through_the_client() {
    // **`Client::cookies` and `Client::cache`'s counterpart, and it was
    // missing until the public surface was reviewed against them.**
    // Nothing failed without it — which is why it needed looking for
    // rather than waiting for: a caller could install an `Hsts` and
    // never read it again, so what a host asserted, and every entry a
    // persisting store would write down, were one-way.
    let rtx = rt();
    let m = MockTransport::new();
    m.push_response(ok_with_sts("max-age=3600; includeSubDomains"));
    let c = Client::builder(m).hsts(Hsts::new()).build().expect("build");

    rtx.block_on(async {
        let _ = c.get("https://example.test/one").send().await;
    });

    let hsts = c.hsts().expect("the client keeps one");
    let held = rtx.block_on(hsts.store().get(&["example.test".to_owned()]));
    assert_eq!(held.len(), 1, "the visit is readable back out");
    assert!(
        held[0].include_subdomains(),
        "and so is what the host actually asserted"
    );
}

#[test]
fn a_client_with_no_hsts_reads_none() {
    // The control: without it the test above passes for a `hsts()` that
    // hands back a set nobody installed.
    let c = Client::builder(MockTransport::new())
        .build()
        .expect("build");
    assert!(c.hsts().is_none());
}
