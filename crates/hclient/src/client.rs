use crate::cached::{Cached, Plan};
use crate::config::{
    Config, check_redirect_supported, check_supported, check_timeouts_supported,
    check_version_demand_supported, effective_redirect, effective_timeouts, effective_uri,
};
use crate::deadline::{Deadline, within};
use crate::decompress::{self, Decompressed};
use crate::request::RequestBuilder;
use crate::stages::redirect::{HopParts, next_hop};
use core::time::Duration;
use hclient_core::Timeouts;
use hclient_core::unversioned::Timer;
use hclient_core::{Capabilities, Error, ErrorKind, RequestBody, RetryKind, UnsupportedCapability};
use hclient_proto::redirect::{RedirectAction, RedirectPolicy, decide};
use hclient_proto::retry::{Outcome, RetryPolicy, RetryVerdict, retry_after_seconds};
use std::fmt::Debug;
use std::sync::Arc;
#[cfg(any(feature = "cookies", feature = "cache"))]
use std::sync::Mutex;
// The wall clock this client reads for the jar and the cache.
//
// `web_time`, not `std::time`, and the difference is one target: off
// `wasm32-unknown-unknown` this IS `std::time::SystemTime` — the crate's
// whole body there is `pub use std::time::*;` — so no signature in this
// crate or in the jar's changes, and no native build behaves differently.
// On that one target `std` has no clock and `now()` aborts, where this
// reads the browser's. `no-std-wall-clock-in-the-client` is what keeps
// the plain import from coming back; its note says what it does and does
// not match.
use web_time::SystemTime;

pub struct ClientBuilder {
    transport: Box<hclient_core::unversioned::erased::SharedTransport>,
    /// The transport's type name, captured at construction: erasure loses
    /// the type, and four capability refusals name the backend.
    backend: &'static str,
    /// The clock a total timeout is measured with.
    ///
    /// Not an `Option`: the absence of a clock is a TYPE
    /// ([`crate::NoClock`]), not a `None`, so that no total timeout can be
    /// configured against a client that cannot measure one. See
    /// [`Self::total_timeout`] and [`crate::NoClock`]'s doc comment.
    timer: Arc<hclient_core::unversioned::erased::SharedTimer>,
    config: Config,
    /// The jar itself, on its way to `Inner`. `Config` carries only the
    /// bit that says one was asked for — see `Config::cookies` for why the
    /// two halves live apart.
    #[cfg(feature = "cookies")]
    jar: Option<crate::cookie::CookieJar<crate::erased::AnyList>>,
    /// The cache itself, on its way to `Inner`, already behind the `Arc`
    /// it will share with every clone of the client **and with every
    /// recording response body** — see `cached::Cache`. `Config` carries
    /// only the bit that says one was asked for.
    #[cfg(feature = "cache")]
    cache: Option<crate::cached::Cache>,
}

/// The clock starts as [`crate::DefaultClock`] rather than being chosen by
/// the caller: `new` takes only a transport, so nothing in the call could
/// infer a clock, and a builder generic over one would be uninferrable at
/// the call site. [`Self::total_timeout`] is where a caller supplies their
/// own, and it hands back a `ClientBuilder` like any other step — erasure
/// is what makes that possible, since the clock is a field rather than a
/// type parameter and swapping it changes no type.
impl ClientBuilder {
    pub fn new<T>(transport: T) -> Self
    where
        T: hclient_core::unversioned::erased::BoxedTransport + Send + Sync + 'static, // send-bound-exception: amendment-C12
    {
        Self {
            backend: std::any::type_name::<T>(),
            transport: Box::new(transport),
            timer: Arc::new(crate::DefaultClock::default()),
            config: Config::default(),
            #[cfg(feature = "cookies")]
            jar: None,
            #[cfg(feature = "cache")]
            cache: None,
        }
    }
}

impl ClientBuilder {
    /// The redirect policy for every request from this client.
    ///
    /// Stores `Some(policy)`, and that wrapping carries meaning: it is what
    /// makes "the caller asked for a redirect policy" distinguishable from
    /// "the caller said nothing", which is the difference between rejecting
    /// an unhonourable setting and rejecting every client built on a
    /// backend that follows redirects itself (see `check_redirect_supported`
    /// in `config.rs`). Never call this and the policy stays `None`, which
    /// every read site turns into `RedirectPolicy::default()` — ten hops,
    /// exactly as before this method learned to say `Some`.
    ///
    /// Overridden wholesale by [`RequestBuilder::redirect`]; the merge is
    /// `config::effective_redirect`, run by `Client::execute`, and it is
    /// the MERGED value that gets checked against the transport's
    /// `Capabilities` — same shape as `timeouts` below.
    ///
    /// [`RequestBuilder::redirect`]: crate::RequestBuilder::redirect
    pub fn redirect<P>(mut self, policy: P) -> Self
    where
        P: RedirectPolicy + Send + Sync + 'static, // send-bound-exception: amendment-C12
    {
        self.config.redirect = Some(std::sync::Arc::new(policy));
        self
    }
    /// Default timeouts for every request from this client.
    ///
    /// Overridden field by field by `RequestBuilder::timeouts`; the merge
    /// is done by `Client::execute` (`effective_timeouts`), and its result
    /// is what actually goes to the transport in `http::Extensions`. A
    /// phase the transport doesn't support is an error at `build()`, and
    /// with B1/M3 also at `execute()` for whatever the request itself set.
    pub fn timeouts(mut self, t: Timeouts) -> Self {
        self.config.timeouts = t;
        self
    }

    /// Stop a response body after `bytes`, with a typed
    /// [`ResponseTooLarge`](crate::error::ResponseTooLarge).
    ///
    /// **It counts what the caller receives**, after any
    /// `Content-Encoding` is reversed, which is the axis a decompression
    /// bomb lives on — a limit applied to the wire would pass one by
    /// definition. `crate::limit::Limited`'s module doc has the full argument,
    /// including the cost: a bound of N does not promise that fewer than
    /// N bytes crossed the wire.
    ///
    /// **Unset by default**, deliberately. A default ceiling would fail a
    /// caller's legitimate large download on a number this crate picked,
    /// which is what `TcpOpts`' every-field-off default exists to avoid.
    ///
    /// Independent of every `Timeouts` field: `total` bounds the
    /// operation's *time*, and a body dripping under the rate this bounds
    /// is stopped by neither unless both are set.
    pub fn response_limit(mut self, bytes: u64) -> Self {
        self.config.response_limit = Some(bytes);
        self
    }

    /// `User-Agent` on every request this client sends.
    ///
    /// There is **no default** — see [`Config::default_headers`] for why
    /// this crate does not invent one.
    ///
    /// A `HeaderValue` rather than a `&str`, which is [`Self::base_url`]'s
    /// convention one setter up: the parse is the caller's, so this
    /// builder needs no way to be half-configured and no error to latch
    /// until `build()`. `HeaderValue::from_static("app/1.0")` is the
    /// usual form and is checked at compile time.
    pub fn user_agent(self, value: http::HeaderValue) -> Self {
        self.default_header(http::header::USER_AGENT, value)
    }

    /// One header on every request this client sends, replacing any
    /// previous default of the same name.
    ///
    /// Applied once per redirect hop, and never over a header the caller
    /// set on the request itself. A header this client's transport
    /// forbids is an `UnsupportedCapability` at `build()` rather than a
    /// value quietly dropped — `hclient-fetch` forbids several, including
    /// `User-Agent`, because the browser writes them.
    pub fn default_header(mut self, name: http::HeaderName, value: http::HeaderValue) -> Self {
        self.config.default_headers.insert(name, value);
        self
    }

    /// Every default header at once, replacing the set.
    ///
    /// Wholesale rather than field by field, the same shape
    /// [`Self::timeouts`] and `Native::tcp_opts` take — and with the same
    /// cost, which is that a caller who sets this after
    /// [`Self::user_agent`] has replaced it.
    pub fn default_headers(mut self, headers: http::HeaderMap) -> Self {
        self.config.default_headers = headers;
        self
    }
    /// The base against which each request's URI is resolved.
    ///
    /// This library's answer to reqwest #988 and #213 (open since 2017 and
    /// 2020, 104 votes).
    ///
    /// **The rule is RFC 3986 §5**, the exact same one that resolves a
    /// response's `Location:`: one client shouldn't understand `/x` two
    /// different ways depending on whether the server sent it or the
    /// caller did. Two consequences follow, worth reading before they
    /// surprise you:
    ///
    /// ```text
    /// base "https://api.test/v1/"   + "things"    -> https://api.test/v1/things
    /// base "https://api.test/v1/"   + "/things"   -> https://api.test/things      // a leading / REPLACES the base's path
    /// base "https://api.test/v1"    + "things"    -> https://api.test/things      // a base without / is not a directory
    /// base "https://api.test/v1/"   + "https://other.test/x" -> https://other.test/x
    /// ```
    ///
    /// In other words, a base with a path should almost always end in a
    /// slash, and a reference should NOT start with one. Neither of these
    /// lines is our own invention — they're the merge algorithm and RFC
    /// §5.2.2; the same rules drive the `url` crate's `Url::join`, the
    /// browser's `new URL(ref, base)`, and `urllib.parse.urljoin`. Since
    /// this round the implementation is our own
    /// (`hclient_proto::uri::resolve_reference`, RFC 3986 §5.2 written
    /// out rather than delegated to `url`); the handful of places where
    /// the RFC and WHATWG genuinely disagree are listed on that function
    /// and pinned against `url` itself in
    /// `crates/hclient-proto/tests/uri_resolution.rs`.
    ///
    /// The base itself must be absolute. A relative one (`/api/`) is a
    /// typed `InvalidBaseUrl` error from `send()`/`execute()`, not a
    /// silently ignored setting. There's deliberately no check at
    /// `build()`: that would require changing `build()`'s error type,
    /// which is wider than this round — noted in the report.
    ///
    /// A limitation worth knowing: `Client::execute`, which takes an
    /// already-built `http::Request`, sees an already-parsed `http::Uri`,
    /// and that type can't represent a path-relative reference at all
    /// (`"things"` is `InvalidUri`). Through this entry point the base can
    /// give the request a scheme and authority, but not a path.
    /// `RequestBuilder` (`client.get("things")`) resolves the original
    /// string before parsing and has no such limitation.
    pub fn base_url(mut self, uri: http::Uri) -> Self {
        self.config.base_url = Some(uri);
        self
    }
    #[must_use]
    pub fn retry<Tm2, P>(mut self, timer: Tm2, policy: P) -> Self
    where
        Tm2: Timer + Clone + Send + Sync + 'static, // send-bound-exception: amendment-C12
        Tm2::Instant: Send,                         // send-bound-exception: amendment-C12
        Tm2::Sleep: Send + 'static,                 // send-bound-exception: amendment-C12
        P: RetryPolicy + Send + Sync + 'static,     // send-bound-exception: amendment-C12
    {
        self.timer = Arc::new(timer);
        self.config.retry = Some(Arc::new(policy));
        self
    }

    pub fn total_timeout<Tm2>(mut self, timer: Tm2, total: Duration) -> Self
    where
        Tm2: Timer + Clone + Send + Sync + 'static, // send-bound-exception: amendment-C12
        Tm2::Instant: Send,                         // send-bound-exception: amendment-C12
        Tm2::Sleep: Send + 'static,                 // send-bound-exception: amendment-C12
    {
        self.timer = Arc::new(timer);
        self.config.total = Some(total);
        self
    }

    /// Keep cookies: attach `Cookie` to every request this client sends,
    /// and store every `Set-Cookie` it gets back.
    ///
    /// `.cookie_jar(CookieJar::new())` is the "just turn it on" form; the
    /// argument is there because a jar is worth configuring
    /// ([`Limits`](crate::cookie::Limits), a fresher public suffix list)
    /// and worth restoring from disk, and a `bool` could express neither.
    /// The jar is shared by every clone of the built client — it is state,
    /// not configuration, and [`Client::cookies`] is how to read it back
    /// out.
    ///
    /// **Against a transport that keeps its own jar this is an error at
    /// [`build`](Self::build)**, not a setting that quietly does the work
    /// twice — see `config::check_cookies_supported` for what "twice"
    /// actually costs. `hclient-fetch` is the backend that reports it.
    ///
    /// The rules are RFC 6265bis and they live in `hclient-cookie`, which
    /// has no clock and no I/O; what this crate adds is the three things a
    /// jar cannot do for itself — deciding *when* (`Client::run`, once per
    /// redirect hop rather than once per operation), deciding *whether*
    /// (the capability gate above), and supplying a `now`.
    ///
    /// **The `now` is `web_time::SystemTime::now()`, read once per
    /// operation, and not the client's [`Timer`].** That is a deliberate
    /// difference from the total timeout one method up, and the reason is
    /// in the two clocks' shapes: `Timer::Instant` is `Copy +
    /// PartialOrd` with an `elapsed_since` — a stopwatch with no epoch —
    /// while `Expires` is a calendar date, and no amount of elapsed time
    /// names one. Anchoring a wall clock once and advancing it with
    /// `Timer::elapsed_since` would use the client's clock, and would
    /// freeze outright for [`NoClock`](crate::NoClock), whose
    /// `elapsed_since` is `Duration::ZERO` for ever: every `Expires` in
    /// the future, every deletion ignored, silently. A clockless client
    /// can configure a jar — nothing about cookies needs a timer — so
    /// that is not a hypothetical.
    ///
    /// **The clock is `web-time`'s, and until it was, a cookie jar in a
    /// browser was not a configuration that existed.**
    /// `std::time::SystemTime::now()` does not fail on
    /// `wasm32-unknown-unknown` — it **panics**, `std` having no clock on
    /// that target at all — so a jar there was unreachable rather than
    /// merely discouraged: the first request that stored or matched a
    /// cookie aborted the module. Measured rather than recalled, in one
    /// `.wasm` holding both calls: `std`'s traps on `unreachable` inside
    /// `<std::time::SystemTime>::now`, `web_time`'s answers with the
    /// host's own `Date.now()`.
    ///
    /// Off that target `web_time::SystemTime` **is**
    /// `std::time::SystemTime` — the crate's whole body there is `pub use
    /// std::time::*;` — so this is not a second clock with its own
    /// behaviour to learn: every signature the jar exposes is the same
    /// type it always was, and a native build resolves one extra crate
    /// that contains a re-export. `no-std-wall-clock-in-the-client` is
    /// what stops the plain import from coming back.
    ///
    /// **The list is the caller's**, and is erased on the way in — see
    /// [`AnyList`](crate::erased::AnyList) for why that rather than a type
    /// parameter on `Client`, and for the one `Send` bound it costs.
    /// `CookieJar::new()` is still the plain form; `CookieJar::
    /// with_public_suffix_list(NoList)` is what drops the compiled-in
    /// list's 77 KiB at run time.
    #[cfg(feature = "cookies")]
    pub fn cookie_jar<P>(mut self, jar: crate::cookie::CookieJar<P>) -> Self
    where
        P: crate::cookie::PublicSuffixList + Send + 'static, // send-bound-exception: amendment-C12
    {
        self.config.cookies = true;
        self.jar = Some(jar.map_suffixes(crate::erased::AnyList::new));
        self
    }

    /// Keep an RFC 9111 response cache: serve a fresh stored response
    /// without sending anything, revalidate a stale one conditionally, and
    /// store what comes back.
    ///
    /// `.cache(HttpCache::new())` is the "just turn it on" form. The
    /// argument is there because a cache is worth configuring — a body
    /// bound ([`Limits`](crate::cache::Limits)), a store size
    /// ([`MemoryStore::with_capacity`](crate::cache::MemoryStore::with_capacity)),
    /// or a store of the caller's own
    /// ([`CacheStore`](crate::cache::CacheStore)) — and a `bool` could
    /// express none of it. The cache is shared by every clone of the built
    /// client, and by every response body it hands out, because a body
    /// that may be stored is recorded as the caller reads it and commits
    /// when it ends. [`Client::cache`] is how to read it back out.
    ///
    /// **Against a transport that keeps its own cache this is an error at
    /// [`build`](Self::build)**, not a setting that quietly does the work
    /// twice — `config::check_cache_supported` says what "twice" costs,
    /// and `hclient-fetch` is the backend that reports it. That capability,
    /// `Capabilities::owns_cache`, has existed since v0.1 with nothing
    /// reading it; this method is what gave it a reader.
    ///
    /// The rules are `hclient-cache`'s, sans-io and clockless. What this
    /// crate adds is the three things a cache cannot do for itself:
    /// deciding *when* (`Client::run`, once per redirect hop and
    /// re-derived rather than carried, exactly as the jar is), deciding
    /// *whether* (the capability gate above), and supplying a `now`.
    ///
    /// **The `now` is `web_time::SystemTime::now()`, and not the client's
    /// [`Timer`].** The argument is [`Self::cookie_jar`]'s in full and it
    /// is if anything sharper here: `Date`, `Expires` and `Age` are
    /// calendar values and RFC 9111 §4.2.3's arithmetic subtracts one from
    /// another, while `Timer::Instant` is a stopwatch with no epoch.
    /// Anchoring a wall clock once and advancing it with
    /// `Timer::elapsed_since` would freeze outright under
    /// [`NoClock`](crate::NoClock), whose `elapsed_since` is
    /// `Duration::ZERO` for ever — and a frozen clock in a cache does not
    /// merely mis-age entries, it makes **every** stored response fresh
    /// for ever. A clockless client can configure a cache, so that is not
    /// hypothetical.
    ///
    /// The clock is read **only when a cache is configured**, and what
    /// that used to be protecting against is worth keeping:
    /// `std::time::SystemTime::now()` **panics on
    /// `wasm32-unknown-unknown`**, where `std` has no clock at all, so
    /// while this crate read `std`'s clock an HTTP cache in a browser was
    /// a configuration that could not run — not refused, which is what
    /// [`Capabilities::owns_cache`](hclient_core::Capabilities) does one
    /// line up, but aborting on the first hop that consulted it. The
    /// clock is `web-time`'s now and that configuration exists;
    /// [`Self::cookie_jar`] has the measurement. The narrowing stays
    /// anyway, because a client that never asked for a cache must not
    /// start reading a clock — the same rule `execute` follows for
    /// `Timer::sleep`.
    ///
    /// # What a cache changes that a jar does not
    ///
    /// A cache can answer a request **without sending it**, so a hop that
    /// hits does not reach the transport at all. Three consequences, each
    /// pinned by a test:
    ///
    /// - hooks see nothing for that hop, because there was no exchange;
    /// - a `Set-Cookie` on a stored response is **not** re-applied to the
    ///   jar. The jar learned from it when it arrived, and replaying an
    ///   old `Set-Cookie` on every hit would resurrect a cookie the server
    ///   had since deleted;
    /// - `Timeouts::connect`/`first_byte`/`between_bytes` bound nothing,
    ///   there being nothing to bound. `total` still covers the operation,
    ///   which now may consist of no I/O at all.
    ///
    /// **The store is the caller's**, and is erased on the way in — see
    /// [`AnyStore`](crate::erased::AnyStore). `HttpCache::new()` is still the plain
    /// form; `HttpCache::with_store(..)` is how a disk-backed or shared
    /// store gets here.
    #[cfg(feature = "cache")]
    pub fn cache<S>(mut self, cache: crate::cache::HttpCache<S>) -> Self
    where
        S: crate::cache::CacheStore + Send + 'static, // send-bound-exception: amendment-C12
    {
        self.config.cache = true;
        self.cache = Some(Arc::new(Mutex::new(
            cache.map_store(crate::erased::AnyStore::new),
        )));
        self
    }

    /// Checks the configuration against the transport's capabilities. Not
    /// a single silent no-op: an unsupported setting is an error, here and
    /// now.
    pub fn build(self) -> Result<Client, UnsupportedCapability> {
        check_supported(&self.config, self.transport.capabilities(), self.backend)?;
        Ok(Client {
            inner: Arc::new(Inner {
                backend: self.backend,
                transport: self.transport,
                timer: self.timer,
                #[cfg(feature = "cookies")]
                cookies: self.jar.map(Mutex::new),
                #[cfg(feature = "cache")]
                cache: self.cache,
            }),
            config: self.config,
        })
    }
}

// `Debug` is written out for these three rather than derived, because the
// two erased seams are trait objects and a trait object is not `Debug`.
// Requiring it of them would tax every backend and every clock for a
// derive; what a reader of a `{:?}` actually wants here is which backend
// this client holds, and erasure has kept that as a string.
impl Debug for ClientBuilder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ClientBuilder")
            .field("backend", &self.backend)
            .field("config", &self.config)
            .finish_non_exhaustive()
    }
}

impl Debug for Client {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Client")
            .field("backend", &self.inner.backend)
            .field("config", &self.config)
            .finish_non_exhaustive()
    }
}

/// The client, and it names no type parameters at all.
///
/// The transport and the clock live behind
/// `hclient_core::unversioned::erased`, so a library takes `&Client` with
/// no `where` clause. `Clone` is an `Arc` bump.
#[derive(Clone)]
pub struct Client {
    inner: Arc<Inner>,
    config: Config,
}

struct Inner {
    backend: &'static str,
    transport: Box<hclient_core::unversioned::erased::SharedTransport>,
    timer: Arc<hclient_core::unversioned::erased::SharedTimer>,
    /// The cookie jar, if one was asked for — **here and not in `Config`**
    /// for the reason the `Config::cookies` bit records: a jar is shared
    /// state, and `Config` is cloned per handle.
    ///
    /// `Mutex` rather than `RefCell` because a `Client` is meant to cross
    /// a `tokio::spawn`, and `!Sync` here would take that away from every
    /// client whether or not it keeps cookies. The lock is held across no
    /// `.await` at all — `cookie_header` and `store_response` are pure
    /// functions of the jar, the URI and a `now`, which is what the
    /// sans-io shape of `hclient-cookie` buys here.
    #[cfg(feature = "cookies")]
    cookies: Option<Mutex<crate::cookie::CookieJar<crate::erased::AnyList>>>,
    /// The response cache, if one was asked for.
    ///
    /// Already an `Arc<Mutex<..>>` rather than a `Mutex` like the jar
    /// beside it, and the difference is not cosmetic: a body that may be
    /// stored is handed a clone of this handle and commits into it when it
    /// ends, which can be long after the `Client` handle that made the
    /// request has been dropped. The jar is only ever touched while a hop
    /// is in flight.
    #[cfg(feature = "cache")]
    cache: Option<crate::cached::Cache>,
}

/// `Client::builder(t)` takes only a transport, and the clock it starts
/// with is [`crate::DefaultClock`] — see `ClientBuilder::new`.
///
/// **Cloning shares the transport and the clock rather than duplicating
/// them**, and `#[derive(Clone)]` is now enough to say so: erasure put
/// both behind the `Arc` in `Inner`, so there is no `T: Clone` for a
/// derive to demand and no way for a clone to copy a transport. This used
/// to be a hand-written impl carrying that argument in a comment.
impl Client {
    pub fn builder<T>(transport: T) -> ClientBuilder
    where
        T: hclient_core::unversioned::erased::BoxedTransport + Send + Sync + 'static, // send-bound-exception: amendment-C12
    {
        ClientBuilder::new(transport)
    }
}

/// Adjusting the bound on an already-built client, for a client whose
/// clock is the target's default one.
///
/// This is the call that keeps `Client`'s type from growing when a timeout
/// is switched on: `Client::new()?.total_timeout(d)` is still a `Client`,
/// so `struct App { http: Client }` keeps compiling. The design doc
/// rejects tower layers for compression (§W5) on exactly that ground, and
/// `tests/deadline_client_type.rs` pins it here.
///
/// **Why this is not written over `Tm: Timer`.** [`crate::NoClock`] — the
/// clock slot of a client that never got one — implements `Timer` too (it
/// has to; `execute` needs a clock type either way), and Rust cannot say
/// "any timer except that one". A no-argument setter over all `Tm: Timer`
/// would therefore exist on a clockless client and do nothing, which is
/// the silent no-op this crate refuses. For any other clock the bound is
/// set in the same call that supplies it,
/// [`ClientBuilder::total_timeout`], where no such gap exists.
///
/// `#[cfg(feature = "default-transport")]`: without the feature
/// `DefaultClock` *is* `NoClock`, so this method must not exist at all.
#[cfg(feature = "default-transport")]
impl Client {
    /// A client sharing this one's transport, bounded by `total`.
    ///
    /// Consumes `self` and returns a new handle rather than mutating in
    /// place: a `Client` behind an `Arc` may already have clones, and
    /// changing their budget from here would be action at a distance.
    /// Clone first if the unbounded handle is still wanted.
    ///
    /// **On native the clock is `Tokio`, so a bounded request needs an
    /// ambient tokio runtime** — the same condition [`Client::new`] already
    /// carries, now reaching one call further, since an unbounded request
    /// never constructs a sleep at all. A test that drives a mock on
    /// `futures_executor` should therefore give the bound its own clock
    /// through [`ClientBuilder::total_timeout`] (`hclient`'s `test-util`
    /// feature carries [`crate::mock::TestTimer`] for exactly this),
    /// rather than reach for this method and meet
    /// `tokio::time::sleep`'s panic.
    pub fn total_timeout(mut self, total: Duration) -> Self {
        self.config.total = Some(total);
        self
    }
}

impl Client {
    /// This client's transport as a concrete type, if that is the type it
    /// was built with.
    ///
    /// `Client` names no type parameter, which is the whole point of it,
    /// and the price is that the backend's type is gone. This is how a
    /// caller asks for it back — a mock's recorded requests, or a `Native`
    /// to lend to a WebSocket connector. `None` means the client holds a
    /// different backend: nothing checked the guess when it was built.
    ///
    /// **There is deliberately no untyped `transport()` beside this**, and
    /// it existed for one commit. Three things were wrong with it and the
    /// third is the one that decides. It returned
    /// `&hclient_core::unversioned::erased::SharedTransport`, a path this
    /// facade does not re-export — so naming the return type meant adding a
    /// dependency on `hclient-core` to a crate that wanted only `hclient`,
    /// which is the tax erasure exists to remove. The name would also have
    /// collided in a reader's head with [`crate::erased`], which is this
    /// crate's *other* erasure and a different subject. And what the value
    /// offered was `capabilities()` — already forwarded by
    /// [`Self::capabilities`] — plus `execute_boxed`, which sends a request
    /// with the redirect policy, the cookie jar, the cache and every
    /// timeout skipped, while reading like "send a request".
    ///
    /// Adding it back later is a minor version and removing it later is a
    /// major one, which is what settles the direction before the first
    /// publish.
    pub fn transport_as<T: 'static>(&self) -> Option<&T> {
        self.inner.transport.as_any().downcast_ref::<T>()
    }
    pub fn config(&self) -> &Config {
        &self.config
    }
    /// What this client's transport can do.
    ///
    /// This forwarder exists so that answering the most natural question
    /// about `Capabilities` doesn't require dragging `unversioned::
    /// Transport` into scope — the trait is
    /// deliberately in semver quarantine (see the doc comment on
    /// `hclient-core/src/unversioned/mod.rs`) and isn't part of the
    /// `hclient` facade. Reaching the transport and calling the trait
    /// method was once the only path; since erasure it is **the only
    /// path at all**, because [`Self::transport_as`] hands back a
    /// concrete backend and a caller who does not know which one they hold
    /// has nothing else to ask.
    pub fn capabilities(&self) -> &Capabilities {
        self.inner.transport.capabilities()
    }

    /// This client's cookie jar, if it was given one — locked for as long
    /// as the guard is held.
    ///
    /// `None` when no jar was configured. That is the same answer for
    /// "cookies were never switched on" and for "the transport keeps its
    /// own", because the second case never gets past
    /// [`ClientBuilder::build`] and so cannot reach this method at all.
    ///
    /// The guard is the API rather than a snapshot: persisting a jar
    /// ([`CookieJar::iter`](crate::cookie::CookieJar::iter)) and seeding
    /// one from disk are both wanted, and a `Vec<Cookie>` copy would
    /// answer only the first. Every clone of this client shares the jar
    /// behind it, so a guard held across an `.await` blocks that client's
    /// other requests — hold it to read, not to work.
    ///
    /// A poisoned lock is recovered rather than propagated
    /// (`PoisonError::into_inner`): the jar is a `Vec` of parsed cookies,
    /// a panic while holding it cannot leave it half-written in any sense
    /// this type can observe, and a client that stopped sending cookies
    /// because an unrelated task panicked would be a worse answer than a
    /// slightly stale jar.
    #[cfg(feature = "cookies")]
    pub fn cookies(
        &self,
    ) -> Option<std::sync::MutexGuard<'_, crate::cookie::CookieJar<crate::erased::AnyList>>> {
        Some(lock(self.inner.cookies.as_ref()?))
    }

    /// This client's response cache, if it was given one — locked for as
    /// long as the guard is held.
    ///
    /// `None` when no cache was configured. That is the same answer for
    /// "caching was never switched on" and for "the transport keeps its
    /// own", because the second case never gets past
    /// [`ClientBuilder::build`] and so cannot reach this method at all —
    /// the shape [`Client::cookies`] already has.
    ///
    /// **Holding this guard blocks more than this client's requests.** A
    /// response body still being read holds the same lock's `Arc` and takes
    /// it once, when the body ends; a guard held across an `.await` can
    /// therefore stall a body belonging to a client handle that no longer
    /// exists. Hold it to read or to clear, not to work.
    ///
    /// A poisoned lock is recovered rather than propagated
    /// (`PoisonError::into_inner`), for the reason [`Client::cookies`]
    /// gives: the store is a map of parsed responses, a panic while
    /// holding it cannot leave it half-written in any sense this type can
    /// observe, and a client that stopped caching because an unrelated
    /// task panicked would be a worse answer than a slightly stale store.
    #[cfg(feature = "cache")]
    pub fn cache(
        &self,
    ) -> Option<std::sync::MutexGuard<'_, crate::cache::HttpCache<crate::erased::AnyStore>>> {
        Some(crate::cached::lock(self.inner.cache.as_ref()?))
    }

    /// The verb methods below, and this one, take `impl AsRef<str>` rather
    /// than `&str`, which is what lets a caller holding a `url::Url` pass
    /// it directly — `url::Url` implements `AsRef<str>`, so this crate
    /// accepts one **without depending on `url`**, which
    /// `hclient-proto` removed at measured cost. `&str`, `String`,
    /// `&String` and `Cow<str>` come along for the same reason.
    ///
    /// It is deliberately not an `IntoUrl`-style trait of ours. Such a
    /// trait's value is the conversions it names, and the one worth naming
    /// here is `url::Url` — which `AsRef<str>` already reaches, at no
    /// dependency and with nothing for a caller to learn.
    pub fn request(&self, method: http::Method, url: impl AsRef<str>) -> RequestBuilder<'_> {
        RequestBuilder::new(self, method, url.as_ref())
    }
    pub fn get(&self, url: impl AsRef<str>) -> RequestBuilder<'_> {
        self.request(http::Method::GET, url)
    }
    pub fn post(&self, url: impl AsRef<str>) -> RequestBuilder<'_> {
        self.request(http::Method::POST, url)
    }
    pub fn put(&self, url: impl AsRef<str>) -> RequestBuilder<'_> {
        self.request(http::Method::PUT, url)
    }
    pub fn delete(&self, url: impl AsRef<str>) -> RequestBuilder<'_> {
        self.request(http::Method::DELETE, url)
    }
    pub fn patch(&self, url: impl AsRef<str>) -> RequestBuilder<'_> {
        self.request(http::Method::PATCH, url)
    }
    pub fn head(&self, url: impl AsRef<str>) -> RequestBuilder<'_> {
        self.request(http::Method::HEAD, url)
    }
    /// HTTP QUERY: a **safe, idempotent** request that carries a body.
    ///
    /// The method GET should have had for large or structured queries. A
    /// filter that does not fit in a URI has, until now, had to be sent as
    /// POST — losing safety, idempotency and cacheability to work around a
    /// length limit. QUERY keeps all three and puts the query in the body.
    ///
    /// Specified in `draft-ietf-httpbis-safe-method-w-body`; `http` 1.5
    /// carries [`http::Method::QUERY`], so nothing here parses a string.
    ///
    /// **A body is the point** — `client.query(url)` with no `.body(..)`
    /// sends an empty one, which is a well-formed but pointless request.
    ///
    /// Redirects treat it as the safe method it is: 301 and 302 preserve
    /// both the method and the body, exactly as they do for PUT or PATCH,
    /// because the historical rewrite-to-GET applies to POST alone. A 303
    /// still becomes a GET without a body — that is what 303 means, and
    /// QUERY claims no exemption from it. Both directions are pinned by
    /// tests in `hclient-proto`, since the correct behaviour here follows
    /// only from QUERY not being POST, and would be easy to "fix" into
    /// corruption by anyone who groups it with POST for having a body.
    pub fn query(&self, url: impl AsRef<str>) -> RequestBuilder<'_> {
        self.request(http::Method::QUERY, url)
    }

    /// Starts an SSE request. **On its own, `.connect()` is one-shot** —
    /// a single attempt, no reconnect, the same contract as
    /// [`crate::sse::SseStream::new`] (which is still the right call for a
    /// response you already have in hand, with no `Client` involved at
    /// all). Add [`crate::sse::SseBuilder::with_timer`] before `.connect()` to
    /// get a [`crate::sse::ReconnectingSseStream`] instead — that call is the
    /// actual gate between the two behaviors, not this method or any
    /// option on it.
    pub fn sse(&self, url: impl AsRef<str>) -> crate::sse::SseBuilder<'_> {
        crate::sse::SseBuilder::new(self, url.as_ref())
    }

    /// The stage order is fixed and correct by construction.
    /// In v0.1 there's one stage — redirect.
    ///
    /// **This method used to carry `where T::Error: Send + Sync + 'static`,
    /// and erasure removed it along with three siblings** — the second
    /// documented exception to the "core declares no `Send`/`Sync`"
    /// invariant (spec amendment-C1). It was needed because
    /// `Transport::to_error` is called for an abstract `T` and its own
    /// where-clause requires the bound, `Error` storing its source as
    /// `Arc<dyn Error + Send + Sync>`. There is no abstract `T` here any
    /// more: `BoxedTransport::execute_boxed` calls `to_error` where `Self`
    /// is concrete, so the bound is discharged at the blanket impl and
    /// four exception markers left this file with it. That is worth more
    /// than the ergonomics the erasure was for — the invariant's own point
    /// is that a bound declared where the type is abstract propagates to
    /// backends that cannot satisfy it.
    pub async fn execute(
        &self,
        req: http::Request<RequestBody>,
    ) -> Result<http::Response<crate::body::ClientBody>, Error> {
        self.execute_with(req, None)
            .await
            .map(|(resp, _final_uri)| resp)
    }

    /// [`Self::execute`] with the one thing that cannot travel in the
    /// request: digest credentials.
    ///
    /// Separate rather than a parameter on the public method, because a
    /// caller who has credentials wrote them on a
    /// [`RequestBuilder`](crate::RequestBuilder) and a caller who does not
    /// should not be made to pass `None`.
    pub(crate) async fn execute_with(
        &self,
        mut req: http::Request<RequestBody>,
        auth: Option<crate::auth::SharedAuth>,
    ) -> Result<(http::Response<crate::body::ClientBody>, http::Uri), Error> {
        // Content negotiation happens ONCE, here, before the first hop —
        // and the `Accept-Encoding` it may set travels to every subsequent
        // one, because `stages::redirect::next_hop` clones the previous
        // hop's headers. The same arrangement `effective_timeouts` gets one
        // level down, for the same reason: a request that stopped asking
        // for a coding halfway through a redirect chain would decode the
        // first response and not the last.
        //
        // The gate is `Capabilities::response_decompression` and nothing
        // else — see `decompress::negotiate`, which is where the
        // `forbidden_request_headers` trap is spelled out.
        let decoders = decompress::negotiate(
            req.headers_mut(),
            self.inner.transport.capabilities(),
            decompress::Decoders::compiled_in(),
        );
        // The deadline starts HERE, once, and not inside the loop below:
        // a bound that restarted on every redirect hop would not be a
        // bound on the operation, and that is the whole reason this is not
        // a `tower` layer (a layer is called per hop and cannot see the
        // chain). `now()` is taken before anything is sent, so DNS, the
        // connect and the TLS handshake are all inside it.
        let started = self.inner.timer.now_boxed();
        let total = self.config.total;
        let resp = match total {
            // No bound asked for: not a `sleep` that never fires, but no
            // sleep constructed at all. `Tokio::sleep` panics outside a
            // runtime, and a client that never asked for a timeout must
            // not start requiring one.
            // The digest replay lives INSIDE this future, like the `425`
            // one, so it spends what is left of `total` rather than a
            // fresh copy of it — a bound a server can double by answering
            // `401` is not a bound.
            None => self.run(req, auth).await?,
            Some(t) => within(self.run(req, auth), &*self.inner.timer, t).await?,
        };
        let (resp, final_uri) = resp;
        let (mut parts, body) = resp.into_parts();
        // Also strips `Content-Encoding` and `Content-Length` when it
        // returns a decoder — both describe the wire, and neither is true
        // of the body assembled below.
        let decoder = decompress::decoder_for(&mut parts, decoders);
        // **The order of these two wrappers is load-bearing, in this
        // direction only.** The deadline goes on FIRST, directly around
        // the transport's body, so it is polled once per COMPRESSED frame;
        // the decoder goes outside it and may loop over many of those
        // before it yields anything. The other way round, a single poll of
        // the decoder could pull an unbounded number of frames off the
        // wire without the clock ever being consulted, and a slow server
        // sending highly compressible padding would walk straight around
        // the bound. `decompress`'s module doc comment has the long form.
        //
        // The bound outlives `execute`, because the operation does: the
        // dribbling body it exists for arrives entirely after the head.
        // `hclient-wasi`'s `Body` carries an unfinished write future the
        // same way, and for the same reason — `Transport::execute`'s
        // signature is untouched by any of it.
        let body = Deadline::new(body, &*self.inner.timer, started, total);
        // **Outermost**, so it counts what the caller receives rather than
        // what crossed the wire — see `limit.rs` for why that is the side
        // the threat is on, and why `Deadline` sits the other way round.
        Ok((
            http::Response::from_parts(
                parts,
                crate::limit::Limited::new(
                    Decompressed::new(body, decoder),
                    self.config().response_limit,
                ),
            ),
            final_uri,
        ))
    }

    /// The stages themselves, unbounded — `execute` above puts the bound
    /// around this whole thing.
    ///
    /// Split out rather than inlined so that the deadline wraps ONE future
    /// covering every hop: the redirect loop is inside here, so dropping
    /// this future on expiry drops whichever hop is in flight, and under
    /// `Transport::execute`'s contract that stops the exchange
    /// instead of leaving it to finish unobserved.
    async fn run(
        &self,
        req: http::Request<RequestBody>,
        mut auth: Option<crate::auth::SharedAuth>,
    ) -> Result<
        (
            http::Response<Cached<hclient_core::unversioned::erased::BoxBody>>,
            http::Uri,
        ),
        Error,
    > {
        let (parts, mut body) = req.into_parts();

        // The base URL is applied here, not only in `RequestBuilder`:
        // `execute` is a public entry point that takes an already-built
        // `http::Request`, and a setting that only worked on one of the
        // two paths would be half-fixed. Idempotent — `RequestBuilder::
        // send` resolves the same URI ahead of time (it needs the result
        // for `Response::url()`), and resolving an already-absolute URI
        // returns it unchanged (RFC 3986 §5.2.2).
        let uri = effective_uri(self.config.base_url.as_ref(), &parts.uri.to_string())?;

        let mut hp = HopParts {
            method: parts.method,
            uri,
            headers: parts.headers,
            version: parts.version,
            extensions: parts.extensions,
        };

        // Two halves of one hole. Without this call,
        // `effective_timeouts` is never reached from production code and
        // `ClientBuilder::timeouts()` is a silent no-op, because the only
        // channel to the transport is
        // `http::Extensions`, and the client's configuration never made
        // it in there; and symmetrically, `RequestBuilder::timeouts()`
        // wrote to `Extensions` with no check against `Capabilities` at
        // all, whereas the same setting at the client level produced an
        // `UnsupportedCapability` at `build()`.
        //
        // The merge and the check live here, not in `build()` and not in
        // `RequestBuilder`, because only here are BOTH operands known. The
        // result is stored in `extensions` before the loop: subsequent
        // hops clone them from the previous one
        // (`stages::redirect::next_hop`), so merging once is enough.
        let effective = effective_timeouts(&hp.extensions, &self.config.timeouts);
        check_timeouts_supported(
            &effective,
            self.inner.transport.capabilities(),
            self.inner.backend,
        )
        .map_err(|e| Error::new(ErrorKind::Unsupported, e))?;
        hp.extensions.insert(effective);

        // The redirect policy gets the same treatment as the timeouts
        // above, for the same two reasons. It can be overridden per request
        // (`RequestBuilder::redirect`; `act`'s `http-client` component
        // computes its limit as `follow_redirects ? 10 : 0` on every call),
        // so only here are both operands known — and the CHECK has to see
        // the merged value, or a per-request policy against a backend that
        // follows redirects internally would be the one path left
        // unchecked. Any policy at all is what a browser can never
        // honour: fetch's `redirect: "follow"` default isn't overridable
        // through `hclient-fetch`, so it can be told neither "stop" nor a
        // limit. The branch that consumer actually takes is
        // `Forbid` — "do not follow, hand me the 3xx" — which
        // `decide` answers with `Stop` before any hop counting.
        //
        // Unlike the timeouts, the merged value is NOT written back into
        // `extensions`: no transport reads a `RedirectPolicy`, and the only
        // consumer is the loop right below. What a `RequestBuilder` put
        // there does travel on to the transport, harmlessly and
        // unavoidably — `Extensions` is one shared bag — and carries across
        // hops with everything else (`stages::redirect::next_hop`), which
        // costs nothing here since the value is read once, before the loop.
        let redirect = effective_redirect(&hp.extensions, &self.config.redirect);
        check_redirect_supported(
            &redirect,
            self.inner.transport.capabilities(),
            self.inner.backend,
        )
        .map_err(|e| Error::new(ErrorKind::Unsupported, e))?;
        // The client's own default when nobody set a policy, and it is a
        // **concrete** one rather than a trait default: `Limit::new(10)`
        // is what this crate has always followed, and a trait whose
        // `follow` defaulted to ten would be a limit no implementor could
        // opt out of.
        let redirect: crate::config::SharedRedirectPolicy = redirect
            .unwrap_or_else(|| std::sync::Arc::new(hclient_proto::redirect::Limit::default()));

        // The third of the three, and the only one with nothing to merge:
        // `RequireVersion` is per request by design (see
        // `check_version_demand_supported`), so what is checked is exactly
        // what the caller put in the extension bag.
        //
        // **Once, before the loop, and it covers every hop** — because the
        // mark travels unchanged: `stages::redirect::next_hop` clones the
        // extensions and strips only `AllowEarlyData`, and only across an
        // origin. The demand deliberately is not on that list. It is a
        // statement about the caller's own code — "what I am about to do
        // needs this protocol" — equally true at hop 1 and hop 4, where
        // `AllowEarlyData` is a judgement about a particular server that
        // nobody made about the next one. Dropping it on a cross-origin
        // redirect would let a 302 deliver over HTTP/1.1 precisely the
        // request that said it could not use HTTP/1.1.
        //
        // One transport, one capability: the backend is the same for every
        // hop of an operation, so there is no second answer a later hop
        // could give.
        check_version_demand_supported(
            &hp.extensions,
            self.inner.transport.capabilities(),
            self.inner.backend,
        )
        .map_err(|e| Error::new(ErrorKind::Unsupported, e))?;

        // Read once, before the loop, and about the CALLER's request
        // rather than about the hop in hand — a `Cookie` header attached
        // by the jar on hop 1 would otherwise read as the caller's on hop
        // 2, and the jar would then stop attaching for the rest of the
        // chain. The rule this decides is `attach_cookies`'.
        let caller_owns_the_cookie_header = hp.headers.contains_key(http::header::COOKIE);

        // The same rule one header over, and it has to be the same rule.
        // A caller who sent `If-None-Match` is asking the ORIGIN a
        // question; a cache that answered it from a stored copy would be
        // answering a different one, and a cache that *added* its own
        // condition on top would be sending two.
        //
        // Read once, from the caller's request, exactly as the cookie line
        // above is and for the same reason with the sign reversed: the
        // conditions this client adds on hop 1 are cloned into hop 2 by
        // `next_hop`, so a per-hop reading would see our own header and
        // stand aside for the rest of the chain — while sending hop 1's
        // validator for hop 2's resource. `cache_before` strips them
        // instead, per hop, which is the `Cookie` header's *removed before
        // it is set* one line further down.
        let caller_owns_the_conditionals = hp.headers.contains_key(http::header::IF_NONE_MATCH)
            || hp.headers.contains_key(http::header::IF_MODIFIED_SINCE);

        let mut hops: u8 = 0;
        // Only when something will read it: the chain costs a `Uri` clone
        // per hop, and a client with no predicate has nobody to hand it to.
        // **Always collected now, where it used to be gated on a predicate
        // being installed.** With one trait there is no way to ask whether
        // the policy reads the history — that question was a second method,
        // and a second method is what this change removed. The cost is a
        // `Vec<Uri>` bounded by the hop count, which is a `u8`: at most ten
        // cheap clones on an ordinary chain, against as many network round
        // trips. Measured against a method on the trait and judged not
        // worth it.
        let mut visited: Vec<http::Uri> = vec![hp.uri.clone()];

        loop {
            // Cookies are attached PER HOP, not once for the operation,
            // and re-derived rather than carried: `next_hop` clones the
            // previous hop's headers, and a 302 within one origin is
            // exactly where a `Cookie` scoped to `/one` would otherwise
            // travel to `/two`. The redirect stage's `SENSITIVE_HEADERS`
            // strip covers the cross-origin case and only that one, which
            // is a different question (credentials leaving the origin)
            // from this one (a header that stopped being right for the
            // path).
            self.attach_cookies(&mut hp, caller_owns_the_cookie_header);
            // **Per hop, and the caller's own header wins.** Per hop
            // because a redirect leads to a request this client is making
            // too, and a `User-Agent` that vanished after the first hop
            // would be a stranger thing than one that was never there.
            // The caller's wins for the reason `Host:` and `Content-Type`
            // already follow: a header written on the request is a
            // decision about that request, and a default is a decision
            // about the client.
            //
            // A default a backend forbids never reaches here — it is an
            // `UnsupportedCapability` at `build()`, see
            // `config::check_default_headers_supported`.
            for (name, value) in &self.config().default_headers {
                if !hp.headers.contains_key(name) {
                    hp.headers.append(name.clone(), value.clone());
                }
            }

            // The replay snapshot is taken BEFORE sending: after that, the
            // body is already consumed. For `Streaming` this returns
            // `None` — and that's known honestly ahead of time, not after
            // a failed retry. It has two readers below: the `425 Too Early`
            // replay, and the next redirect hop.
            let replay = body.rewind();
            let sending = std::mem::replace(&mut body, RequestBody::Empty);

            // The cache, per hop and re-derived, for the reason the jar is
            // — and with one thing the jar cannot do: it may answer the
            // hop outright, in which case nothing is sent at all.
            //
            // `Break` is that answer: a fresh stored response, or the
            // `504` RFC 9111 §5.2.1.7 obliges for `only-if-cached` with
            // nothing to serve. `sending` is dropped there, which is the
            // whole point of a hit; and the redirect decision below runs
            // on it exactly as on a live response, because a cached `301`
            // is still a `301`.
            //
            // `Continue` carries whether this hop is a **revalidation**,
            // which is what makes a `304` mean *serve the stored body*
            // rather than *hand the caller a bodyless 304*.
            let resp = match self.cache_before(&mut hp, caller_owns_the_conditionals) {
                std::ops::ControlFlow::Break(answered) => answered,
                std::ops::ControlFlow::Continue(plan) => {
                    // **This is written out here rather than factored into
                    // a method, and the reason is `Send`.** A helper taking
                    // `replay: Option<&RequestBody>` held that borrow for
                    // the whole of its future, which made the future
                    // require `RequestBody: Sync`, and `RequestBody` holds
                    // a `Box<dyn Body + Send + Unpin>` that is deliberately
                    // not `Sync`. The future used to start as a let-bound
                    // a `Send`-requiring chain so `tokio::spawn` could
                    // have run it, but `tests/shape.rs` pins the opposite
                    // — `Client::execute`'s future is `!Send` after erasure —
                    // so the borrow is inlined: the temporary ends before
                    // the `.await`.
                    //
                    // RFC 9111 §4.2.3's `request_time`, and it is read
                    // **before** the request rather than after the response
                    // for the reason the RFC gives in as many words: *"a
                    // cache MUST interpret this value relative to the time
                    // the request was initiated, not the time that the
                    // response was received"*. The gap between the two is
                    // `response_delay`, which is added to a received `Age`.
                    // **One flow per hop, made here**, before the first
                    // send — so a scheme that can authenticate
                    // pre-emptively (Basic, or Digest with a nonce it
                    // already has) never needs a challenge at all, and so
                    // the flow that sees the response is the one that
                    // authorized the request. Two flows was the first
                    // shape and it was wrong in a way the tests caught
                    // immediately: the second had never seen the request,
                    // so a scheme that hashes the method — which digest
                    // does — had nothing to hash.
                    //
                    // It sits **outside** the retry loop deliberately: a
                    // retry re-sends the same request, and a flow
                    // counting legs must not count an attempt that
                    // failed for the network's reasons.
                    let mut flow = auth.as_ref().map(|a| {
                        let mut f = a.start();
                        f.authorize(&hp.method, &hp.uri, &mut hp.headers);
                        f
                    });

                    let mut requested_at = self.cache_now();

                    // **The retry loop, and it wraps the send alone.** A
                    // retry is about getting *an answer for this hop*;
                    // the redirect decision, the cache and the `425`
                    // replay below all operate on the answer, so they see
                    // one outcome however many attempts produced it.
                    //
                    // **Per hop rather than per operation**, which is the
                    // `425` replay's rule three screens down and for its
                    // reason: hop 3 answers a different request than hop 1
                    // sent, and a budget spent on hop 1 has nothing to say
                    // about hop 3. Bounded either way — `Backoff`'s
                    // attempt count bounds each hop, the redirect policy
                    // bounds the hops, and `Timeouts::total` bounds the
                    // lot from outside this future.
                    //
                    // **`replayable` is read off the snapshot's *kind*,
                    // never by building a body.** Calling a
                    // `Rewindable`'s factory to find out whether it has
                    // one would make this a second caller of it, and
                    // `hclient`'s own
                    // `a_rewindable_body_is_replayed_from_the_snapshot_taken_before_the_first_attempt`
                    // counts those calls to pin *one snapshot per hop, not
                    // one per attempt* — the same trap `hclient-mock`'s
                    // body recording fell into and was fixed for.
                    let replayable = replay.as_ref().is_some_and(|b| b.retry_kind().may_replay());
                    let policy = self.config().retry.clone();
                    let mut sending = Some(sending);
                    let mut attempt: u32 = 0;

                    let mut resp = loop {
                        attempt += 1;
                        let this = match sending.take() {
                            Some(b) => b,
                            None => match replay_for_too_early(replay.as_ref()) {
                                Some(b) => b,
                                // Unreachable while `replayable` gates
                                // every path that gets here, and a `break`
                                // rather than a panic because the honest
                                // answer to "we cannot rebuild the body"
                                // is the outcome we already have.
                                None => {
                                    return Err(Error::new(
                                        ErrorKind::Body,
                                        BodyVanishedBeforeRetry,
                                    ));
                                }
                            },
                        };

                        let outcome = self.send_hop(&hp, this).await;
                        let Some(policy) = policy.as_ref() else {
                            // No policy: the first outcome is the answer,
                            // and an error propagates exactly as it did
                            // before this loop existed.
                            break outcome?;
                        };

                        let what = match &outcome {
                            Ok(r) => {
                                let (after, unreadable) = read_retry_after(r.headers());
                                Outcome::status(r.status(), after, unreadable)
                            }
                            // **`is_unsent`, not the `ErrorKind`.** The
                            // category cannot answer this: a response head
                            // over `H1Opts::max_headers` is
                            // `ErrorKind::Connect` too, and it happens
                            // after the request went out. Only the
                            // transport knows, and one that says nothing
                            // costs a retry rather than a duplicate.
                            Err(e) if e.is_unsent() => Outcome::Unsent,
                            Err(_) => break outcome?,
                        };

                        // One ask where there used to be a policy call and
                        // then a predicate call. A guard is a policy now,
                        // so `Standard::transient().and(SafeMethodsOnly)`
                        // is one value and answers once.
                        let verdict = policy.retry(&hclient_proto::retry::ProposedRetry::new(
                            &hp.method,
                            &hp.uri,
                            attempt,
                            what,
                            replayable,
                            crate::sse::jitter(),
                        ));

                        match verdict {
                            RetryVerdict::After(delay) => {
                                drop(outcome);
                                self.inner.timer.sleep_boxed(delay).await;
                                // RFC 9111 §4.2.3 again: the stored age
                                // arithmetic is relative to when *this*
                                // attempt was initiated, so the stamp
                                // moves with the retry rather than dating
                                // the response to the first try.
                                requested_at = self.cache_now();
                            }
                            RetryVerdict::Stop => break outcome?,
                        }
                    };

                    // **RFC 8470 §5.2 — `425 Too Early`: the third way a
                    // request that went into early data can fail, and the
                    // only one of the three a transport structurally cannot
                    // close.** The other two never reach this code: the
                    // handshake refusing early data (nothing was sent, so
                    // nothing is at risk) and the server rejecting the
                    // 0-RTT keys (`ZeroRttRejected`, which the transport
                    // replays on the same connection once the handshake
                    // completes). This one is a *status code* — the server
                    // got the data, declined to risk processing it, and
                    // asked for the request again outside early data.
                    // Deciding that belongs to whoever owns the operation,
                    // which is this loop and not a transport.
                    //
                    // **Once, and by construction rather than by a
                    // counter**: there is no loop around these two lines,
                    // so a server wedged on `425` gets two requests and the
                    // caller gets the second `425`. An unbounded retry
                    // against exactly that server is an infinite one.
                    //
                    // **Per hop, not per operation.** A `425` on hop 3
                    // answers a different request than hop 1 sent, and
                    // declining to replay it because hop 1 was already
                    // replayed would make the behaviour depend on history.
                    // The count is bounded either way: at most two attempts
                    // per hop, and the hops are bounded by the redirect
                    // limit — and by the paragraph below.
                    //
                    // **The replay is inside `run`, and that is what puts
                    // it inside the operation's budget.** `execute` reads
                    // the clock once and wraps the whole of `run` in
                    // `within(..)`; a replay bolted on outside that —
                    // around `execute`, or in a caller — would give the
                    // second attempt a fresh `total`, and a `total` a
                    // server can double by answering `425` is not a bound.
                    //
                    // **"Outside early data" is what the replay owes RFC
                    // 8470**, and the strip below is that. It was owed
                    // vacuously when this branch was written — no transport
                    // here could put anything in early data — and has been
                    // owed for real since `hclient-h3` landed. Admission is
                    // a per-request opt-in the CALLER sets, so a request
                    // carrying no mark cannot end up in early data.
                    //
                    // **The trap is not in this branch; it is in the one
                    // that sits next to it.** `retry_kind()` below answers
                    // "can I send this body again", and for `425` that is
                    // the entire question: the server asked for the repeat
                    // itself, so there is nobody to protect the request
                    // from. Admission to early data asks the OTHER question
                    // — "may an attacker send this again" — which is method
                    // safety, a notion this codebase deliberately does not
                    // have (`hclient-native`'s W2 retry documents why it
                    // never needed one: nothing had reached the wire). The
                    // two questions disagree on the same request: `POST
                    // /transfer` with `RequestBody::Full(..)` is
                    // `RetryKind::Free` and is precisely what must never
                    // enter early data. `RetryKind` is a precondition for
                    // 0-RTT, never a permission.
                    //
                    // **And no `else` carrying an error of ours.** A body
                    // that cannot be sent twice leaves the `425` standing
                    // as the answer: it is the server's answer, complete
                    // and typed already, and replacing it with an `Error`
                    // would hide a status the caller can act on behind a
                    // category it cannot.
                    if resp.status() == http::StatusCode::TOO_EARLY
                        && let Some(again) = replay_for_too_early(replay.as_ref())
                    {
                        // It is not enough that the handshake completed
                        // long ago. `AllowEarlyData` is part of h3's
                        // connection-pool key, so a marked retry asks for
                        // the early-data connection *specifically*; if that
                        // entry has been evicted or closed by the peer
                        // since, the retry opens a fresh connection and
                        // goes into early data again — against the very
                        // server that just refused to risk one. The
                        // connection that happens to still be pooled is an
                        // accident, not a guarantee.
                        //
                        // Stripped on a clone: the next hop is a different
                        // request and keeps whatever the caller asked for.
                        let mut retry = hp.clone();
                        retry.extensions.remove::<hclient_core::AllowEarlyData>();
                        // Re-read, so an entry stored from a replay is aged
                        // from the request that actually produced it.
                        // Keeping the first attempt's stamp would fold the
                        // whole of that attempt into `response_delay` and
                        // hand the entry an age it never had.
                        requested_at = self.cache_now();
                        resp = self.send_hop(&retry, again).await?;
                    }

                    // **The `401` answer, and it is the `425` branch's shape**: a
                    // status-code test, one resend, inside the same `total` budget,
                    // gated on `RequestBody::retry_kind()`. What differs is that
                    // the resend carries a header computed from what came back.
                    //
                    // Once per hop, deliberately. A second `401` after an answer
                    // means the credentials are wrong — or, with `stale=true`, that
                    // the nonce expired between the challenge and the answer, which
                    // a second round would only race again. Either way the `401` is
                    // the server's answer and it goes to the caller.
                    // **Authentication, as many legs as the scheme needs.**
                    // This was a hard-coded `401` branch computing a
                    // digest header; it is a loop over `AuthFlow` now, so
                    // NTLM and Negotiate are writable in somebody else's
                    // crate over `sspi` or `libgssapi` — which is where
                    // the demand measurably is, and where no HTTP client
                    // in this ecosystem currently lets them go.
                    //
                    // Bounded twice over: `MAX_LEGS` here, and the body's
                    // own `retry_kind()`, which is asked before every
                    // extra leg exactly as it is for `425` and a retry.
                    if let Some(flow) = flow.as_mut() {
                        let mut legs: u8 = 1;
                        loop {
                            match flow.on_response(resp.status(), resp.headers()) {
                                crate::auth::AuthStep::Done => break,
                                crate::auth::AuthStep::Again => {}
                            }
                            legs += 1;
                            if legs > crate::auth::MAX_LEGS {
                                return Err(Error::new(
                                    ErrorKind::Status,
                                    crate::auth::TooManyLegs,
                                ));
                            }
                            let Some(again) = replay_for_too_early(replay.as_ref()) else {
                                // A body that cannot be sent twice leaves
                                // the challenge standing as the answer,
                                // which is the server's own and is more
                                // use than an error of ours.
                                break;
                            };
                            let mut next = hp.clone();
                            flow.authorize(&next.method, &next.uri, &mut next.headers);
                            requested_at = self.cache_now();
                            resp = self.send_hop(&next, again).await?;
                        }
                    }

                    self.cache_after(&hp, plan, resp, requested_at)
                }
            };

            let location = resp
                .headers()
                .get(http::header::LOCATION)
                .map(|v| v.as_bytes());
            let action = decide(
                &*redirect,
                hops,
                &hp.uri,
                &hp.method,
                resp.status(),
                location,
                &visited,
            );

            match action {
                RedirectAction::Stop => return Ok((resp, hp.uri)),
                // **One arm where there were two.** A policy that refuses
                // names its own reason, so "the limit" and "a caller's own
                // rule" stopped needing separate handling here — and the
                // message is the policy's rather than this function's
                // reconstruction of which policy was in force, which is
                // what the old arm had to do by matching the enum back.
                RedirectAction::Refused { why, to } => {
                    return Err(Error::new(
                        ErrorKind::Redirect,
                        RedirectRefused {
                            to,
                            status: resp.status(),
                            why,
                            after_hops: hops,
                        },
                    ));
                }
                RedirectAction::InvalidLocation => {
                    return Err(Error::new(ErrorKind::Redirect, BadLocation));
                }
                RedirectAction::Follow(f) => {
                    // **Credentials do not cross an origin**, by the rule
                    // that is already stripping `Authorization` three lines
                    // down in `next_hop`. Answering a `401` from a host the
                    // caller never named would send a password derived
                    // secret to a server they never vouched for — the
                    // judgement `AllowEarlyData` had to be taken off for,
                    // and a stronger case, since this one is the password.
                    // **Credentials do not cross an origin**, which was
                    // digest's rule and is now every scheme's: a
                    // password-derived secret must not reach a server the
                    // caller never named.
                    if f.strip_sensitive {
                        auth = None;
                    }
                    // Asked here rather than inside `decide`, and only
                    // about a hop `decide` approved — see
                    // `crate::predicate`. `strip_sensitive` is handed over
                    // as `cross_origin` rather than recomputed, so a
                    // predicate refusing cross-origin hops and the client
                    // removing credentials cannot disagree about what an
                    // origin is.
                    hops += 1;
                    visited.push(f.uri.clone());
                    let Some((next_hp, next_body)) = next_hop(&hp, replay, &f) else {
                        return Ok((resp, hp.uri));
                    };
                    hp = next_hp;
                    body = next_body;
                }
            }
        }
    }

    /// One attempt at one hop: send it, classify a transport failure, and
    /// let the jar learn from whatever came back.
    ///
    /// Two callers, and they are the two attempts a single hop can make —
    /// the request as it stands, and the `425 Too Early` replay above.
    /// Factored out rather than written twice so the replay cannot drift
    /// from the original: both need `to_error`'s classification, and both
    /// need `store_cookies`, since a `Set-Cookie` riding a `425` is exactly
    /// what a hand-copied second call site would eventually stop storing.
    async fn send_hop(
        &self,
        hp: &HopParts,
        body: RequestBody,
    ) -> Result<http::Response<hclient_core::unversioned::erased::BoxBody>, Error> {
        let resp = self
            .inner
            .transport
            .execute_boxed(hp.to_request(body))
            .await
            // No `map_err` here, unlike every version of this line before
            // erasure: `execute_boxed` has already called the backend's own
            // `Transport::to_error`, which is what B2 of the branch's final
            // review asked for — the backend decides its error's category,
            // not this line, and one that is already an `Error` is handed
            // back as-is.
            ?;

        // Stored per hop too, and against THIS hop's URI: a login handing
        // back `Set-Cookie` on its 302 is the ordinary case, and a jar that
        // only looked at the final response would miss every one of them.
        // Scoped to the hop that sent the header, because `Domain`/`Path`
        // are relative to the request that got the response, not to
        // wherever the chain ends up.
        self.store_cookies(&hp.uri, resp.headers());
        Ok(resp)
    }

    /// Puts this hop's cookies on the request, if this client keeps a jar.
    ///
    /// Three decisions are in these few lines, and none of them is
    /// obvious.
    ///
    /// **A caller's own `Cookie` header wins, for the whole operation.**
    /// The precedent is `decompress::negotiate`'s treatment of a
    /// caller-set `Accept-Encoding` — "the caller did their own
    /// negotiating" — and reqwest makes the same call. Note the scope:
    /// `caller_owns_the_header` is decided once, from the original
    /// request, so a cross-origin redirect that strips the caller's header
    /// (`SENSITIVE_HEADERS`) does not hand the jar the wheel halfway
    /// through. Hands off means hands off.
    ///
    /// **Removed before it is set**, rather than `insert`-ed over.
    /// `HeaderMap::insert` would in fact overwrite, but only when there is
    /// something to write: a hop whose jar match is empty has to end up
    /// with NO header, and the previous hop's — cloned in by `next_hop` —
    /// is what would otherwise remain.
    ///
    /// **`web_time::SystemTime::now()`, read here.** Why the client's
    /// [`Timer`] cannot supply it, and why the clock is `web-time`'s
    /// rather than `std`'s, are both on [`ClientBuilder::cookie_jar`];
    /// the short forms are that `Timer` is a stopwatch and `Expires` is a
    /// date, and that `std`'s wall clock panics in a browser.
    #[cfg(feature = "cookies")]
    fn attach_cookies(&self, hp: &mut HopParts, caller_owns_the_header: bool) {
        let Some(jar) = self.inner.cookies.as_ref() else {
            return;
        };
        if caller_owns_the_header {
            return;
        }
        hp.headers.remove(http::header::COOKIE);
        if let Some(v) = lock(jar).cookie_header(&hp.uri, SystemTime::now()) {
            hp.headers.insert(http::header::COOKIE, v);
        }
    }

    /// The twin without the feature. A no-op function rather than a
    /// `#[cfg]` around the call site in `run`: the call site says what
    /// happens and when, and burying it in a conditional would put the
    /// per-hop reasoning above behind a feature flag too.
    #[cfg(not(feature = "cookies"))]
    fn attach_cookies(&self, _: &mut HopParts, _: bool) {}

    /// Stores this hop's `Set-Cookie` headers, if this client keeps a jar.
    ///
    /// Runs even when the caller supplied their own `Cookie` header and
    /// `attach_cookies` therefore did nothing: the two halves are separate
    /// decisions, a browser behaves the same way, and a jar that stopped
    /// learning because one request was hand-rolled would go stale in a
    /// way nothing announces.
    ///
    /// Refusals are dropped here, by `CookieJar::store_response`'s
    /// contract — one malformed `Set-Cookie` must not stop the others, and
    /// a server sending `Domain=co.uk` gets no say in whether the rest of
    /// its cookies arrive. A caller who needs the reasons calls
    /// `CookieJar::store` per header, through [`Client::cookies`].
    #[cfg(feature = "cookies")]
    fn store_cookies(&self, uri: &http::Uri, headers: &http::HeaderMap) {
        let Some(jar) = self.inner.cookies.as_ref() else {
            return;
        };
        lock(jar).store_response(uri, headers, SystemTime::now());
    }

    /// The twin without the feature — see `attach_cookies`'.
    #[cfg(not(feature = "cookies"))]
    fn store_cookies(&self, _: &http::Uri, _: &http::HeaderMap) {}

    /// `web_time::SystemTime::now()`, but only where something will read
    /// it.
    ///
    /// The narrowing was written when the clock was `std`'s, whose `now()`
    /// **panics on `wasm32-unknown-unknown`**, and it is kept now that the
    /// clock cannot panic anywhere: a client that never asked for a cache
    /// must not start reading a clock — the same rule `execute` follows
    /// for `Timer::sleep`, which is constructed only on the branch that
    /// has a bound to measure. What changed is that this is no longer the
    /// thing standing between a browser build and a cache; see
    /// [`ClientBuilder::cache`].
    ///
    /// The value handed back when there is no cache is never read by
    /// anything; it is `UNIX_EPOCH` rather than a panic because the
    /// alternative is an `Option` threaded through two signatures to say
    /// what the store's absence already says.
    #[cfg(feature = "cache")]
    fn cache_now(&self) -> SystemTime {
        if self.inner.cache.is_some() {
            SystemTime::now()
        } else {
            SystemTime::UNIX_EPOCH
        }
    }

    /// The twin without the feature — see `attach_cookies`'.
    #[cfg(not(feature = "cache"))]
    fn cache_now(&self) -> SystemTime {
        SystemTime::UNIX_EPOCH
    }

    /// Asks the cache about this hop, before anything is sent.
    ///
    /// `Break` means the cache answered it and the transport is not
    /// called; `Continue` carries whether the hop that is about to go out
    /// is a revalidation.
    ///
    /// Three decisions live in these few lines.
    ///
    /// **A caller's own conditional wins, for the whole operation**, and
    /// the precedent is `attach_cookies`' treatment of a caller-set
    /// `Cookie` and `decompress::negotiate`'s of a caller-set
    /// `Accept-Encoding`. Note the scope: `caller_owns_the_conditionals`
    /// is decided once, from the original request, so a cross-origin
    /// redirect does not hand the cache the wheel halfway through.
    /// `hclient-cache` refuses such a request on its own account too — the
    /// two checks are the same rule stated where each layer can act on it,
    /// and the one here is what stops the *conditions this client adds*
    /// from being mistaken for the caller's on the next hop.
    ///
    /// **Removed before they are set.** A hop whose plan asks for no
    /// condition must carry none, and the previous hop's — cloned in by
    /// `next_hop` — is what would otherwise remain, naming a different
    /// resource's validator. Exactly the `Cookie` header's rule, and for
    /// exactly the same reason.
    ///
    /// **`web_time::SystemTime::now()`, read here.** Why the client's
    /// [`Timer`] cannot supply it, and why it is `web-time`'s clock, are
    /// both on [`ClientBuilder::cache`].
    #[cfg(feature = "cache")]
    fn cache_before(
        &self,
        hp: &mut HopParts,
        caller_owns_the_conditionals: bool,
    ) -> std::ops::ControlFlow<
        http::Response<Cached<hclient_core::unversioned::erased::BoxBody>>,
        Plan,
    > {
        use std::ops::ControlFlow::{Break, Continue};
        let Some(cache) = self.inner.cache.as_ref() else {
            return Continue(Plan::default());
        };
        if caller_owns_the_conditionals {
            return Continue(Plan::default());
        }
        hp.headers.remove(http::header::IF_NONE_MATCH);
        hp.headers.remove(http::header::IF_MODIFIED_SINCE);

        let now = SystemTime::now();
        match crate::cached::lock(cache).lookup(&hp.method, &hp.uri, &hp.headers, now) {
            crate::cache::Lookup::Miss => Continue(Plan::default()),
            crate::cache::Lookup::Hit(stored) => Break(crate::cached::serve(stored)),
            crate::cache::Lookup::Unsatisfiable => Break(crate::cached::only_if_cached_miss()),
            crate::cache::Lookup::Revalidate {
                key,
                stale,
                conditions,
            } => {
                for (name, value) in conditions {
                    hp.headers.insert(name, value);
                }
                Continue(Plan(Some(Box::new(crate::cached::Revalidating {
                    key,
                    stale,
                }))))
            }
        }
    }

    /// The twin without the feature. A method rather than a `#[cfg]`
    /// around the call site in `run`, for the reason `attach_cookies`'
    /// twin exists: the call site says what happens and when, and burying
    /// it in a conditional would put the per-hop reasoning behind a feature
    /// flag too.
    #[cfg(not(feature = "cache"))]
    fn cache_before(
        &self,
        _: &mut HopParts,
        _: bool,
    ) -> std::ops::ControlFlow<
        http::Response<Cached<hclient_core::unversioned::erased::BoxBody>>,
        Plan,
    > {
        std::ops::ControlFlow::Continue(Plan)
    }

    /// What the cache makes of the answer: a `304` served from the store,
    /// an invalidation, or a body worth recording.
    ///
    /// **A `304` is only ever swallowed when this client asked the
    /// question.** `plan` carries the stale entry, and it is `None` for
    /// every hop the cache did not condition — including one whose
    /// conditional the caller wrote — so a `304` arriving there reaches
    /// the caller as the response it is. That is the same distinction
    /// `cache_before` makes one method up, on the other side of the wire.
    ///
    /// **The transport's body is dropped on that path, and that is not a
    /// leak.** A `304` carries no body by definition (RFC 9110 §15.4.5),
    /// so what is dropped is an empty one; dropping it also stops the
    /// exchange under `Transport::execute`'s contract, which is right —
    /// there is nothing left to read.
    ///
    /// **Invalidation runs for every hop, not only for cacheable ones.**
    /// RFC 9111 §4.4 is about what an unsafe method did to the resource,
    /// which has nothing to do with whether its own response can be
    /// stored; a `POST` returning `201` stores nothing and must still
    /// evict the `GET`.
    #[cfg(feature = "cache")]
    fn cache_after(
        &self,
        hp: &HopParts,
        plan: Plan,
        resp: http::Response<hclient_core::unversioned::erased::BoxBody>,
        requested_at: SystemTime,
    ) -> http::Response<Cached<hclient_core::unversioned::erased::BoxBody>> {
        let Some(cache) = self.inner.cache.as_ref() else {
            return resp.map(Cached::live);
        };
        let received_at = SystemTime::now();
        let (parts, body) = resp.into_parts();

        if let Some(r) = plan.0 {
            if parts.status == http::StatusCode::NOT_MODIFIED {
                let served = crate::cached::lock(cache).revalidated(
                    &r.key,
                    r.stale,
                    &parts,
                    requested_at,
                    received_at,
                );
                return crate::cached::serve(served);
            }
            // Not a `304`, so the entry we conditioned on is no longer the
            // origin's answer — see `HttpCache::superseded` for why it is
            // removed here rather than left for the storing path below to
            // replace.
            crate::cached::lock(cache).superseded(&r.key, &r.stale);
        }

        let mut guard = crate::cached::lock(cache);
        guard.invalidated_by(&hp.method, &hp.uri, parts.status);
        let storing = guard.storing(
            &hp.method,
            &hp.uri,
            &hp.headers,
            &parts,
            requested_at,
            received_at,
        );
        drop(guard);

        let body = match storing {
            Ok(s) => Cached::recording(body, crate::cached::Recorder::new(Arc::clone(cache), s)),
            // The reason is typed and is dropped here, exactly as a
            // `Set-Cookie` refusal is: one uncacheable response must not
            // fail an exchange. `crate::cache::NotStored` is what a caller
            // driving the cache themselves would read.
            Err(_) => Cached::live(body),
        };
        http::Response::from_parts(parts, body)
    }

    /// The twin without the feature — see `cache_before`'.
    #[cfg(not(feature = "cache"))]
    fn cache_after(
        &self,
        _: &HopParts,
        _: Plan,
        resp: http::Response<hclient_core::unversioned::erased::BoxBody>,
        _: SystemTime,
    ) -> http::Response<Cached<hclient_core::unversioned::erased::BoxBody>> {
        resp.map(Cached::live)
    }
}

/// The body a `425 Too Early` replay is sent with, or `None` when this
/// request cannot honestly be sent a second time.
///
/// `snapshot` is the rewind taken before the attempt that got the `425`.
/// The verdict is read off the body **that is about to be sent**, never off
/// one cached from before a `rewind()` — `RequestBody::Rewindable`'s own doc
/// comment makes that an invariant, because a factory is allowed to hand
/// back a `Streaming` body, in which case the snapshot of a `ViaFactory`
/// body is an `Impossible` one.
///
/// `RetryKind` and `rewind()` are two spellings of the same three-way
/// split, so the match below is not the only thing standing between a
/// `Streaming` body and a second send — `rewind()` would answer `None` for
/// it anyway. It is written as the gate regardless, because it is the
/// question being asked ("may this be replayed"), and because the honest
/// failure mode of deleting it is not "no retry" but "retry with whatever
/// body is to hand", which is how a truncated or empty request reaches a
/// server that asked for the real one again.
///
/// What this deliberately does NOT consult is the method. `425` needs no
/// idempotency judgement: the server asked for the repeat. Early data does,
/// and this function is not the place to grow one — see the long comment at
/// the call site.
/// The body could not be rebuilt for a retry the policy had approved.
///
/// Unreachable while `replayable` gates every path that reaches it —
/// it exists so that the impossible case is a typed error a caller can
/// read rather than a panic in a client that had already worked.
#[derive(Debug, thiserror::Error)]
#[error("the request body could not be rebuilt for a retry")]
struct BodyVanishedBeforeRetry;

/// Reads `Retry-After` into the pair [`Outcome::status`] wants.
///
/// The two `None`s are different and both are returned: *the header was
/// absent* lets the backoff decide, where *the header was present and
/// unreadable* stops the retry. Collapsing them would make a server
/// naming an `HTTP-date` get retried on our own schedule, which is the
/// one thing the header exists to prevent.
fn read_retry_after(headers: &http::HeaderMap) -> (Option<Duration>, bool) {
    let Some(raw) = headers.get(http::header::RETRY_AFTER) else {
        return (None, false);
    };
    let parsed = raw.to_str().ok().and_then(retry_after_seconds);
    (parsed, parsed.is_none())
}

fn replay_for_too_early(snapshot: Option<&RequestBody>) -> Option<RequestBody> {
    match snapshot.map(|b| (b.retry_kind(), b)) {
        Some((RetryKind::Free | RetryKind::ViaFactory, b)) => b.rewind(),
        // `Impossible`, or nothing was replayable to begin with.
        _ => None,
    }
}

/// Locks the jar, recovering from poisoning rather than propagating it.
///
/// See [`Client::cookies`] for why a poisoned jar is still a usable one.
#[cfg(feature = "cookies")]
fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

// `not(target_family = "wasm")`, not just `feature = "default-transport"`
// — the same double gate as `DefaultTransport` itself (`lib.rs`): on wasm
// targets, where the `DefaultTransport` branch doesn't exist (see its doc
// comment), this `impl Client` block's `new` would refer
// to a nonexistent type. Separate gates would give the same behavior (an
// `impl` for a nonexistent type also fails to compile), but repeating the
// condition makes the reason visible on the spot, not only in `lib.rs`.
#[cfg(all(feature = "default-transport", not(target_family = "wasm")))]
impl Client {
    /// A client with the default transport.
    ///
    /// On native this requires a surrounding tokio runtime: `tokio::spawn`
    /// and `tokio::time::sleep` panic outside a runtime. reqwest behaves
    /// exactly the same way. The explicit path without this requirement is
    /// `Client::builder(Native::new(rt, tls, dns))` with a runtime of your
    /// choice (see `crates/hclient/tests/two_runtimes.rs`, the same
    /// constructor for tokio and for smol).
    ///
    /// # Errors
    ///
    /// Two, and they are told apart by [`ErrorKind`](hclient_core::ErrorKind)
    /// rather than by which function you called:
    /// `Rustls::with_platform_verifier()` failing to read the OS trust
    /// store is `ErrorKind::Tls`, and a client setting the transport
    /// cannot honour is `ErrorKind::Unsupported`, carrying the
    /// `UnsupportedCapability` that names it as its source.
    ///
    /// **This function used to panic on the first of those, and there was
    /// a `try_new` beside it that did not.** The split was argued from the
    /// error type: `UnsupportedCapability` is a typed answer to *the
    /// transport does not support this setting* and not to *the trust
    /// store could not be read*, so `new` returned the narrow type and
    /// `.expect`ed the other cause. The argument was sound and the naming
    /// it produced was not — **both functions returned `Result`**, so
    /// `try_` marked the one that was fallible about *more things* rather
    /// than the one that was fallible at all, which is not what the prefix
    /// means in Rust.
    ///
    /// Keeping the wider type and dropping the panic resolves it without
    /// giving anything up, because `ErrorKind` already draws the line the
    /// two types were drawing. What it removes is a library panicking on a
    /// machine whose certificate store cannot be read — which `try_new`'s
    /// own doc listed real cases for, and which nothing in this workspace
    /// called: `try_new` had no caller outside this file.
    pub fn new() -> Result<Self, hclient_core::Error> {
        let transport = Self::default_native_transport()?;
        // **The machine's own proxy, honoured by default.** `HTTP_PROXY`
        // and `HTTPS_PROXY` where the environment names them, the
        // registry on Windows, the dynamic store on macOS — because a
        // convenience constructor that ignored them would be the one
        // client on the machine that does, and a port from curl or
        // reqwest would silently start going direct.
        //
        // Lenient rather than strict, and the report is discarded here
        // rather than swallowed elsewhere: a configuration this client
        // cannot express in full is not a reason to refuse to construct
        // — `system_proxies_from_lossy`'s doc has each case. A caller who
        // wants to *see* what was dropped reads `SystemProxies::detect()`
        // and installs it themselves, which is what
        // `Native::system_proxy` is for.
        //
        // **Two ways out, and they answer different questions.** At
        // build time, `default-features = false` drops the
        // `system-proxy` feature and this block with it — a positive
        // feature turned off, which is the shape a *negative* one could
        // not have had: Cargo unifies features, so a switch that turned
        // proxying off would have been unified too, and one crate in a
        // dependency tree could take it from every other. At run time,
        // `Client::builder(default_transport()?)` builds the same client
        // over the same stack and reads nothing, because that seam
        // deliberately does not. `tests/proxy_default.rs` pins the second
        // so that it cannot quietly stop working.
        #[cfg(feature = "system-proxy")]
        let transport = {
            let (t, _dropped) = transport
                .system_proxies_from_lossy(&hclient_native::proxy::system::SystemProxies::detect());
            t
        };
        Self::builder(transport)
            .build()
            .map_err(|e| hclient_core::Error::new(hclient_core::ErrorKind::Unsupported, e))
    }

    /// The construction of the default transport for [`Client::new`] —
    /// the one operation in it that can genuinely fail
    /// (`Rustls::with_platform_verifier()`). Its own function since there
    /// were two constructors sharing it; kept as one because the reason
    /// for the `Result` is worth a place to write down. `Result<_,
    /// hclient_core::Error>`, not `UnsupportedCapability`:
    /// `with_platform_verifier()` already returns an `Error`
    /// (`ErrorKind::Tls`) itself, and `Native::new`/`SystemDns::new` can't
    /// fail at all (ordinary constructors, no IO) — wrapping their
    /// nonexistent failure in a `Result` would have nothing to justify it.
    #[cfg(not(feature = "http3"))]
    pub(crate) fn default_native_transport() -> Result<crate::DefaultTransport, hclient_core::Error>
    {
        let rt = hclient_rt_tokio::Tokio;
        let tls = hclient_tls_rustls::Rustls::with_platform_verifier()?;
        // `.with_proxies(Vec::new())` names the `P` the alias promises
        // without configuring anything: this seam stays inert, and
        // `Client::new` is what fills the list. See `DefaultTransport`.
        Ok(
            hclient_native::Native::new(rt, tls, hclient_dns_system::SystemDns::new(rt))
                .with_proxies(Vec::new()),
        )
    }

    /// The `http3` sibling of the function above, forked on the same single
    /// question `NativeDefault` (`lib.rs`) is forked on, and with the same
    /// discipline: the two `#[cfg]`s are syntactic negations of each other.
    ///
    /// **One `Rustls`, cloned into both stacks.** It carries the platform
    /// verifier, which reads the OS trust store, and handing each member
    /// its own would read it twice and give the pair two configuration
    /// identities where they are one configuration. That `Rustls: Clone`
    /// shares its ALPN cache and its QUIC ticket store rather than forking
    /// them is what makes the clone the right thing rather than the cheap
    /// one — see that type's own doc.
    ///
    /// The resolver is cloned three times rather than shared behind a
    /// handle, because each stack owns its own and `Selecting`'s third
    /// handle is the one it asks for HTTPS records — deliberately not
    /// borrowed from a member, see that type's `dns` field.
    ///
    /// `Selecting::new` returns a `Disagreement` when the two members
    /// cannot be given one honest `Capabilities`. It cannot happen here —
    /// both are built with their own defaults, and the one reachable
    /// disagreement in this workspace is `Native::without_pool()` against
    /// `H3` — so this maps it into the `ErrorKind::Unsupported` the caller
    /// already has to handle rather than unwrapping.
    #[cfg(feature = "http3")]
    pub(crate) fn default_native_transport() -> Result<crate::DefaultTransport, hclient_core::Error>
    {
        let rt = hclient_rt_tokio::Tokio;
        let tls = hclient_tls_rustls::Rustls::with_platform_verifier()?;
        let tcp =
            hclient_native::Native::new(rt, tls.clone(), hclient_dns_system::SystemDns::new(rt));
        let quic = hclient_native::H3::new(rt, tls, hclient_dns_system::SystemDns::new(rt))?;
        // `.proxies(Vec::new())` for the reason the other fork gives:
        // the alias names the `P` a proxied machine will need, and this
        // seam stays inert either way.
        tcp.http3(quic)
            .map(|t| t.with_proxies(Vec::new()))
            .map_err(|e| hclient_core::Error::new(hclient_core::ErrorKind::Unsupported, e))
    }
}

// The browser sibling of the `Client::new` above.
// The gate is the mirror image of that one and of `DefaultTransport`'s own
// (`lib.rs`): `all(target_family = "wasm", target_os = "unknown")`, NOT a
// bare `target_family = "wasm"` — `wasm32-wasip2` is also `wasm`, and there
// is deliberately no `DefaultTransport` branch there (see that type's doc
// comment for why `hclient` doesn't depend on `hclient-wasi`), so a bare
// wasm gate would `impl` for a type that doesn't exist on that target.
#[cfg(all(
    feature = "default-transport",
    target_family = "wasm",
    target_os = "unknown"
))]
impl Client {
    /// A client with the browser transport.
    ///
    /// No `Result`, unlike the native [`Client::new`] — and no panic
    /// smuggled in to pay for that. The native version's `Result` exists
    /// for one failure that has no counterpart here: reading the OS trust
    /// store. `Fetch::new()` cannot fail at all — it probes the running
    /// browser and stores the answer (`hclient-fetch`'s `caps::probe`),
    /// with no I/O and no fallible step — and TLS is the browser's
    /// business, not ours.
    ///
    /// # Panics
    ///
    /// The `.expect` below is unreachable for the configuration this
    /// function itself builds, and that rests on one fact two files away
    /// rather than on anything visible at the call site — so, named here:
    /// **`Config::default()` leaves `redirect: None`** (`config.rs`), and
    /// `check_redirect_supported` rejects only a `Some` policy against a
    /// `RedirectSupport::Internal` backend, which `Fetch` is. Nothing else
    /// in the default config can trip `build()`: `Timeouts::default()` is
    /// three `None`s, and `base_url` is not checked against `Capabilities`
    /// at all.
    ///
    /// **The moment `Config`'s default becomes
    /// `Some(RedirectPolicy::default())`, or `ClientBuilder` starts
    /// storing a policy eagerly instead of leaving it `None` until
    /// `.redirect(...)` is called, this line panics in every browser
    /// program that calls `Client::new()`** — not in a test here, in the
    /// consumer's own program. That is the dependency to preserve, or to
    /// replace this constructor's signature over.
    ///
    /// A caller who does configure a redirect policy on a browser client
    /// gets an ordinary `Err` from `Client::builder(Fetch::new())
    /// .redirect(..).build()`, never a panic: that path doesn't go
    /// through this function.
    pub fn new() -> Self {
        Self::builder(hclient_fetch::Fetch::new())
            .build()
            .expect("fetch transport with default config is always supported")
    }
}

// Not for looks and not to silence `clippy::new_without_default`, though it
// does both: on this target `Client::new()` is infallible and takes no
// arguments, which is exactly the shape `Default` describes. The native
// `Client::new()` returns a `Result` and so has no `Default` to offer —
// the asymmetry is in the constructors, not in this impl. `hclient-fetch`
// makes the same pairing for `Fetch` itself.
#[cfg(all(
    feature = "default-transport",
    target_family = "wasm",
    target_os = "unknown"
))]
impl Default for Client {
    fn default() -> Self {
        Self::new()
    }
}

// **The third branch, where `Client::new` does not exist — and what a
// reader got for that named nothing.** Measured from a fresh crate outside
// this workspace, on the two lines this crate's front page opens with:
// `cargo add hclient` resolves, and `Client::new()` is `error[E0599]: no
// associated function or constant named `new` found for struct `Client``,
// with no mention of a feature anywhere in it.
//
// **The free function beside it needs nothing like this, and that is the
// measurement that decided the shape of the fix.** `hclient::
// default_transport()` under the same build is `error[E0425]` carrying
// rustc's own *"found an item that was configured out"* note, which points
// at the `#[cfg]` line and underlines `feature = "default-transport"`. That
// note is emitted for **path** resolution and not for associated-item
// lookup, so a free function announces its own gate and an inherent `fn`
// in a `#[cfg]`-ed-out `impl` block does not. Hence one stub, here, rather
// than a pair.
//
// So on this branch the item **exists**, with a where-clause nothing can
// satisfy and the message on the trait it names. The lifetime is
// load-bearing rather than decoration: a where-clause predicate carrying
// no generic parameter is checked at the *definition* site, so the obvious
// `where Self: DefaultTransportFeature` refuses to compile this crate at
// all — measured, `error[E0277]` on the `where` line itself. `'g` gives the
// predicate a parameter, which defers it to the call site, and the caller
// never writes it.
#[cfg(not(any(
    // The native `Client::new` above.
    all(feature = "default-transport", not(target_family = "wasm")),
    // The browser one. Both conditions are repeated rather than named once,
    // for the reason the gate on the native block gives: the branch a
    // reader is standing in should say what it excludes.
    all(
        feature = "default-transport",
        target_family = "wasm",
        target_os = "unknown"
    )
)))]
pub(crate) mod without_a_default_transport {
    /// The unsatisfiable bound behind [`Client::new`]'s refusal, and the
    /// carrier of the message a reader sees. Never implemented, for any
    /// type, in any build.
    ///
    /// [`Client::new`]: super::Client::new
    // **Two headlines, because there are two reasons to be standing here
    // and only one of them is the feature.** Within this module the pair is
    // exhaustive and mutually exclusive: the module compiles only where
    // neither positive gate holds, so either the feature is off (any
    // target), or it is on and the target is `wasm` and not the browser —
    // which is `wasm32-wasip2`, where turning the feature on changes
    // nothing. Telling a WASI caller to add a feature they already have is
    // the shape of advice that costs an afternoon.
    #[cfg_attr(
        not(feature = "default-transport"),
        diagnostic::on_unimplemented(
            message = "`Client::new()` needs the `default-transport` feature of the `hclient` crate",
            label = "this build of `hclient` has no default transport",
            note = "add it: `cargo add hclient --features default-transport`",
            note = "it is deliberately not a default: Cargo unifies features across a dependency graph, so a default here would be a floor, putting tokio, rustls and the system resolver into every build that contains this crate",
            note = "or name a transport, as every target may: `hclient::Client::builder(transport).build()?`"
        )
    )]
    #[cfg_attr(
        all(
            feature = "default-transport",
            target_family = "wasm",
            not(target_os = "unknown")
        ),
        diagnostic::on_unimplemented(
            message = "`Client::new()` has no default transport to build on `wasm32-wasip2`",
            label = "`default-transport` is enabled and does not reach this target",
            note = "`hclient` deliberately does not depend on `hclient-wasi`, so the WASI transport is named rather than defaulted: `hclient::Client::builder(hclient_wasi::WasiHttp::new()).build()?`",
            note = "adding the `default-transport` feature does not help here — it is already on"
        )
    )]
    pub trait DefaultTransportFeature<'g> {}

    impl super::Client {
        /// Not available in this build — see [`DefaultTransportFeature`].
        ///
        /// The two real constructors are gated on `default-transport`, and
        /// this stands in their place so that calling one says which
        /// feature is missing instead of which name is.
        ///
        /// # Errors
        ///
        /// None reachable: the bound is unsatisfiable, so no call to this
        /// compiles. The `Result` is the native signature's, because that
        /// is the one the front page's `?` is written against — a caller
        /// who reaches this has one error rather than two.
        pub fn new<'g>() -> Result<Self, hclient_core::Error>
        where
            Self: DefaultTransportFeature<'g>,
        {
            unreachable!("`Client::new` has an unsatisfiable bound; no call to it compiles")
        }
    }
}

/// A redirect policy refused a hop.
///
/// Raised beside the unresolvable-`Location` error, because both are
/// `decide`'s answers
/// turned into errors at the same place. It replaced `TooMany(u8)`, which
/// could say only one of the things a policy can now refuse for.
#[derive(Debug, thiserror::Error)]
#[error("the redirect policy refused a {status} to {to} after {after_hops} hops: {why}")]
#[non_exhaustive]
pub struct RedirectRefused {
    pub to: http::Uri,
    pub status: http::StatusCode,
    /// The policy's own words — `"redirect limit reached"`,
    /// `"redirect leaves the origin"`, or whatever a caller's own policy
    /// returned.
    pub why: &'static str,
    /// How many hops had already been taken.
    ///
    /// The client's fact rather than the policy's, which is why it is
    /// here and not in the verdict: a `&'static str` cannot carry a
    /// number without allocating, and *how far did this get* is true of
    /// every refusal rather than only a limit.
    pub after_hops: u8,
}

#[derive(Debug, thiserror::Error)]
#[error("Location header is not a resolvable URI")]
struct BadLocation;
