use crate::config::{
    Config, check_redirect_supported, check_supported, check_timeouts_supported,
    effective_redirect, effective_timeouts, effective_uri,
};
use crate::deadline::{Deadline, within};
use crate::request::RequestBuilder;
use crate::stages::redirect::{HopParts, next_hop};
use core::time::Duration;
use http_ng_core::Timeouts;
use http_ng_core::unversioned::{Timer, Transport};
use http_ng_core::{Capabilities, Error, ErrorKind, RequestBody, UnsupportedCapability};
use http_ng_proto::redirect::{RedirectAction, RedirectPolicy, decide};

#[derive(Debug)]
pub struct ClientBuilder<T, Tm = crate::DefaultClock> {
    transport: T,
    /// The clock a total timeout is measured with.
    ///
    /// Not an `Option`: the absence of a clock is a TYPE
    /// ([`crate::NoClock`]), not a `None`, so that no total timeout can be
    /// configured against a client that cannot measure one. See
    /// [`Self::total_timeout`] and [`crate::NoClock`]'s doc comment.
    timer: Tm,
    config: Config,
}

/// `Tm` fixed to [`crate::DefaultClock`], not generic: a `new` over any
/// `Tm` would have nowhere to get a clock value from, and `Client::builder`
/// (which forwards here) would leave `Tm` unconstrained by its arguments
/// and so uninferrable at the call site. Pinning it here is what keeps
/// `Client::builder(t).build()` resolving to `Client<T>` — the bare form
/// existing code writes — rather than to some other clock.
impl<T: Transport> ClientBuilder<T, crate::DefaultClock> {
    pub fn new(transport: T) -> Self {
        Self {
            transport,
            timer: crate::DefaultClock::default(),
            config: Config::default(),
        }
    }
}

impl<T: Transport, Tm> ClientBuilder<T, Tm> {
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
    pub fn redirect(mut self, policy: RedirectPolicy) -> Self {
        self.config.redirect = Some(policy);
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
    /// The base against which each request's URI is resolved.
    ///
    /// This library's answer to reqwest #988 and #213 (open since 2017 and
    /// 2020, 104 votes). Before fix round 3, the value stored here was
    /// never read from anywhere — the setter was a silent no-op.
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
    /// (`http_ng_proto::uri::resolve_reference`, RFC 3986 §5.2 written
    /// out rather than delegated to `url`); the handful of places where
    /// the RFC and WHATWG genuinely disagree are listed on that function
    /// and pinned against `url` itself in
    /// `crates/http-ng-proto/tests/uri_resolution.rs`.
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
    /// A bound on the **whole operation**, and the clock that measures it
    /// — in one call, because neither is any use without the other.
    ///
    /// The gap this closes is recorded in `docs/v01-acceptance.md`:
    /// `connect`/`first_byte`/`between_bytes` bound three phases, and a
    /// response that starts promptly and then dribbles just under the
    /// `between_bytes` threshold is bounded by none of them. This one
    /// covers everything from the moment `execute` is entered — every
    /// redirect hop included, which is precisely why it lives in `Client`
    /// and cannot be a `tower` layer: a layer sees one hop, not the
    /// operation, and would restart its clock on each one.
    ///
    /// What expiry produces is `ErrorKind::Timeout(Phase::Total)` with a
    /// [`crate::TotalTimeoutElapsed`] source, and it stops the exchange
    /// rather than only reporting on it — see [`crate::Deadline`], which
    /// also states exactly what this does NOT cover (a body that goes
    /// completely silent after the head; that is `between_bytes`).
    ///
    /// **The clock is an argument, not something this crate supplies.**
    /// The reasoning is `SseBuilder::with_timer`'s, in full: an ambient
    /// per-target clock inside `http-ng` means a `#[cfg]` in the facade
    /// crate, and a `std::thread`-backed sleep that compiles on
    /// `wasm32-wasip2` while having no thread to hand out at runtime — a
    /// capability that looks supported and silently is not. A caller on
    /// `http-ng-rt-tokio` or `http-ng-rt-smol` already has one in scope,
    /// and `http-ng`'s `test-util` feature carries
    /// [`crate::mock::TestTimer`].
    ///
    /// A client whose clock is already the target's default does not need
    /// this call and can adjust the bound alone, on a built client:
    /// [`Client::total_timeout`].
    ///
    /// `Tm2: Clone` because the clock travels into every response body
    /// this client produces ([`crate::Deadline`] owns one); every clock in
    /// this workspace is a handle or a ZST, and `tests/two_runtimes.rs`
    /// already bounds runtimes by `Clone` for the same reason.
    pub fn total_timeout<Tm2: Timer + Clone>(
        self,
        timer: Tm2,
        total: Duration,
    ) -> ClientBuilder<T, Tm2> {
        ClientBuilder {
            transport: self.transport,
            timer,
            config: Config {
                total: Some(total),
                ..self.config
            },
        }
    }

    /// Checks the configuration against the transport's capabilities. Not
    /// a single silent no-op: an unsupported setting is an error, here and
    /// now.
    pub fn build(self) -> Result<Client<T, Tm>, UnsupportedCapability> {
        check_supported(
            &self.config,
            self.transport.capabilities(),
            backend_name::<T>(),
        )?;
        Ok(Client {
            inner: std::sync::Arc::new(Inner {
                transport: self.transport,
                timer: self.timer,
            }),
            config: self.config,
        })
    }
}

fn backend_name<T>() -> &'static str {
    // The type name is informative enough for an error message and costs nothing.
    std::any::type_name::<T>()
}

// A forked declaration, rather than one `pub struct Client<T = crate::
// DefaultTransport>` with a conditional default: Rust has no way to make a
// generic default itself conditional on a feature — a `#[cfg]` on a lone
// default parameter inside a single struct declaration isn't read by the
// compiler. Without the `default-transport` feature, `Client` must require
// `T` explicitly (an ordinary "missing generics" compile error on `Client`
// with no parameter — the same honest error as the missing
// `DefaultTransport`, see its doc comment in `lib.rs`), rather than
// resolving to a default from a branch that doesn't exist at all with the
// feature off. Both variants below carry the same set of fields;
// `impl<T: Transport> Client<T>` further down applies to both identically:
// the generic parameter's default only affects call sites where `Client`
// is written with no explicit `<...>` (e.g. `Client::new()`'s return type
// below), not the signatures of existing impl blocks.
#[cfg(feature = "default-transport")]
#[derive(Debug)]
pub struct Client<T = crate::DefaultTransport, Tm = crate::DefaultClock> {
    inner: std::sync::Arc<Inner<T, Tm>>,
    config: Config,
}
#[cfg(not(feature = "default-transport"))]
#[derive(Debug)]
pub struct Client<T, Tm = crate::DefaultClock> {
    inner: std::sync::Arc<Inner<T, Tm>>,
    config: Config,
}

/// The transport and the clock a `Client` shares with its clones.
///
/// Behind an `Arc` so that cloning a `Client` is an atomic increment rather
/// than a copy of the transport — which for a real backend owns a
/// connection pool, a TLS configuration and a resolver, none of which
/// should be duplicated by handing the client to a second task.
///
/// **The configuration is deliberately NOT in here**, unlike before v0.2
/// W4. It sits in `Client` itself, per handle, and is cloned with it
/// (`Timeouts` and `RedirectPolicy` are `Copy`; cloning an `http::Uri` is
/// a `Bytes` refcount). That is what lets [`Client::total_timeout`] hand
/// back a differently-bounded client over the SAME transport for the cost
/// of one atomic increment — a per-call-site budget without a second
/// connection pool.
#[derive(Debug)]
struct Inner<T, Tm> {
    transport: T,
    timer: Tm,
}

/// Cloning shares the transport and the clock; it does not duplicate them.
///
/// Hand-written rather than derived: `#[derive(Clone)]` would require
/// `T: Clone` and `Tm: Clone`, the first of which is both unnecessary —
/// the `Arc` is what is cloned — and wrong in meaning, since a `T: Clone`
/// transport would then be copied per clone instead of shared.
impl<T, Tm> Clone for Client<T, Tm> {
    fn clone(&self) -> Self {
        Self {
            inner: std::sync::Arc::clone(&self.inner),
            config: self.config.clone(),
        }
    }
}

/// `Tm` fixed to [`crate::DefaultClock`] for the same reason
/// `ClientBuilder::new` is — see its doc comment. `Client::builder(t)`
/// takes only a transport, so nothing in the call could infer a clock;
/// this is what keeps the result `Client<T>`, the bare form that already
/// appears in this workspace's own signatures.
impl<T: Transport> Client<T, crate::DefaultClock> {
    pub fn builder(transport: T) -> ClientBuilder<T, crate::DefaultClock> {
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
impl<T: Transport> Client<T, crate::DefaultClock> {
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
    /// through [`ClientBuilder::total_timeout`] (`http-ng`'s `test-util`
    /// feature carries [`crate::mock::TestTimer`] for exactly this),
    /// rather than reach for this method and meet
    /// `tokio::time::sleep`'s panic.
    pub fn total_timeout(mut self, total: Duration) -> Self {
        self.config.total = Some(total);
        self
    }
}

/// `Tm: Timer + Clone` — `Timer` because [`Self::execute`] measures with
/// it, `Clone` because the clock travels into every response body
/// ([`crate::Deadline`] owns one). Both hold for every clock in this
/// workspace, and for [`crate::NoClock`].
impl<T: Transport, Tm: Timer + Clone> Client<T, Tm> {
    pub fn transport(&self) -> &T {
        &self.inner.transport
    }
    pub fn config(&self) -> &Config {
        &self.config
    }
    /// What this client's transport can do.
    ///
    /// This forwarder exists so that answering the most natural question
    /// about `Capabilities` doesn't require dragging `unversioned::
    /// Transport` into scope (Task 17 fix round 2) — the trait is
    /// deliberately in semver quarantine (see the doc comment on
    /// `http-ng-core/src/unversioned/mod.rs`) and isn't part of the
    /// `http-ng` facade. Without this forwarder, `client.transport().
    /// capabilities()` — a trait method — was the only path, and
    /// `client.transport()` returns `&T`, so calling `.capabilities()` on
    /// it would require `Transport` in a `use`.
    pub fn capabilities(&self) -> &Capabilities {
        self.inner.transport.capabilities()
    }

    pub fn request(&self, method: http::Method, url: &str) -> RequestBuilder<'_, T, Tm> {
        RequestBuilder::new(self, method, url)
    }
    pub fn get(&self, url: &str) -> RequestBuilder<'_, T, Tm> {
        self.request(http::Method::GET, url)
    }
    pub fn post(&self, url: &str) -> RequestBuilder<'_, T, Tm> {
        self.request(http::Method::POST, url)
    }
    pub fn put(&self, url: &str) -> RequestBuilder<'_, T, Tm> {
        self.request(http::Method::PUT, url)
    }
    pub fn delete(&self, url: &str) -> RequestBuilder<'_, T, Tm> {
        self.request(http::Method::DELETE, url)
    }
    pub fn patch(&self, url: &str) -> RequestBuilder<'_, T, Tm> {
        self.request(http::Method::PATCH, url)
    }
    pub fn head(&self, url: &str) -> RequestBuilder<'_, T, Tm> {
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
    /// tests in `http-ng-proto`, since the correct behaviour here follows
    /// only from QUERY not being POST, and would be easy to "fix" into
    /// corruption by anyone who groups it with POST for having a body.
    pub fn query(&self, url: &str) -> RequestBuilder<'_, T, Tm> {
        self.request(http::Method::QUERY, url)
    }

    /// Starts an SSE request. **On its own, `.connect()` is one-shot** —
    /// a single attempt, no reconnect, the same contract as
    /// [`crate::SseStream::new`] (which is still the right call for a
    /// response you already have in hand, with no `Client` involved at
    /// all). Add [`crate::SseBuilder::with_timer`] before `.connect()` to
    /// get a [`crate::ReconnectingSseStream`] instead — that call is the
    /// actual gate between the two behaviors, not this method or any
    /// option on it.
    pub fn sse(&self, url: &str) -> crate::sse::SseBuilder<'_, T, Tm> {
        crate::sse::SseBuilder::new(self, url)
    }

    /// The stage order is fixed and correct by construction.
    /// In v0.1 there's one stage — redirect.
    ///
    /// `where T::Error: Send + Sync + 'static` — the second documented
    /// exception to the "core declares no Send/Sync" invariant (spec
    /// amendment-C1, sibling of the exception on `Error::source`). Without
    /// it, `Transport::to_error` below wouldn't be callable for an
    /// abstract `T`: its own where-clause requires the same bound, because
    /// its default body calls `Error::new`, and `Error` stores its source
    /// as `Arc<dyn Error + Send + Sync>`, and type erasure doesn't let an
    /// unbounded trait object's auto-traits through (verified by
    /// compilation — E0277 without this bound). The bound lives here, on
    /// the method itself, rather than on the `Transport` trait as a whole,
    /// exactly as documented in `http-ng-core`'s lib.rs: a transport with
    /// an honestly `!Send` error remains representable, it just can't be
    /// used with `Client`.
    pub async fn execute(
        &self,
        req: http::Request<RequestBody>,
    ) -> Result<http::Response<Deadline<T::Body, Tm>>, Error>
    where
        T::Error: Send + Sync + 'static, // send-bound-exception: amendment-C1
    {
        // The deadline starts HERE, once, and not inside the loop below:
        // a bound that restarted on every redirect hop would not be a
        // bound on the operation, and that is the whole reason this is not
        // a `tower` layer (a layer is called per hop and cannot see the
        // chain). `now()` is taken before anything is sent, so DNS, the
        // connect and the TLS handshake are all inside it.
        let started = self.inner.timer.now();
        let total = self.config.total;
        let resp = match total {
            // No bound asked for: not a `sleep` that never fires, but no
            // sleep constructed at all. `Tokio::sleep` panics outside a
            // runtime, and a client that never asked for a timeout must
            // not start requiring one.
            None => self.run(req).await?,
            Some(t) => within(self.run(req), &self.inner.timer, t).await?,
        };
        // The bound outlives `execute`, because the operation does: the
        // dribbling body this exists for arrives entirely after the head.
        // `http-ng-wasi`'s `Body` carries an unfinished write future the
        // same way, and for the same reason — `Transport::execute`'s
        // signature is untouched by either.
        Ok(resp.map(|body| Deadline::new(body, self.inner.timer.clone(), started, total)))
    }

    /// The stages themselves, unbounded — `execute` above puts the bound
    /// around this whole thing.
    ///
    /// Split out rather than inlined so that the deadline wraps ONE future
    /// covering every hop: the redirect loop is inside here, so dropping
    /// this future on expiry drops whichever hop is in flight, and under
    /// `Transport::execute`'s contract (v0.2 W1) that stops the exchange
    /// instead of leaving it to finish unobserved.
    async fn run(&self, req: http::Request<RequestBody>) -> Result<http::Response<T::Body>, Error>
    where
        T::Error: Send + Sync + 'static, // send-bound-exception: amendment-C1
    {
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

        // B1/M3 of the branch's final review, two halves of one hole.
        // Before it, `effective_timeouts` was never called from anywhere
        // in production code — `ClientBuilder::timeouts()` was a silent
        // no-op, because the only channel to the transport is
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
            backend_name::<T>(),
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
        // through `http-ng-fetch`, so it can be told neither "stop" nor a
        // limit. The branch that consumer actually takes is
        // `RedirectPolicy::None` — "do not follow, hand me the 3xx" — which
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
            backend_name::<T>(),
        )
        .map_err(|e| Error::new(ErrorKind::Unsupported, e))?;
        let redirect = redirect.unwrap_or_default();

        let mut hops: u8 = 0;

        loop {
            // The replay snapshot is taken BEFORE sending: after that, the
            // body is already consumed. For `Streaming` this returns
            // `None` — and that's known honestly ahead of time, not after
            // a failed retry.
            let replay = body.rewind();
            let sending = std::mem::replace(&mut body, RequestBody::Empty);

            let resp = self
                .inner
                .transport
                .execute(hp.to_request(sending))
                .await
                // Not `Error::new(ErrorKind::Other, e)`: B2 of the
                // branch's final review — unconditional wrapping flattened
                // the category of ANY transport error into `Other`,
                // devaluing the whole `ErrorKind` taxonomy. The backend
                // decides, not this line: the default `Transport::to_error`
                // wraps exactly the same way, and a backend whose error is
                // already an `Error` hands it back as-is.
                .map_err(|e| self.inner.transport.to_error(e))?;

            let location = resp
                .headers()
                .get(http::header::LOCATION)
                .map(|v| v.as_bytes());
            let action = decide(
                &redirect,
                hops,
                &hp.uri,
                &hp.method,
                resp.status(),
                location,
            );

            match action {
                RedirectAction::Stop => return Ok(resp),
                RedirectAction::TooManyRedirects => {
                    // Only `Limited` can reach here: `decide` turns `None`
                    // into `Stop` before any counting, because "do not
                    // follow" means the 3xx IS the answer.
                    let limit = match redirect {
                        RedirectPolicy::Limited(n) => n,
                        RedirectPolicy::None => 0,
                    };
                    return Err(Error::new(ErrorKind::Redirect, TooMany(limit)));
                }
                RedirectAction::InvalidLocation => {
                    return Err(Error::new(ErrorKind::Redirect, BadLocation));
                }
                RedirectAction::Follow(f) => {
                    hops += 1;
                    let Some((next_hp, next_body)) = next_hop(&hp, replay, &f) else {
                        return Ok(resp);
                    };
                    hp = next_hp;
                    body = next_body;
                }
            }
        }
    }
}

// `not(target_family = "wasm")`, not just `feature = "default-transport"`
// — the same double gate as `DefaultTransport` itself (`lib.rs`): on wasm
// targets, where the `DefaultTransport` branch doesn't exist (see its doc
// comment), this `impl` for `Client<crate::DefaultTransport>` would refer
// to a nonexistent type. Separate gates would give the same behavior (an
// `impl` for a nonexistent type also fails to compile), but repeating the
// condition makes the reason visible on the spot, not only in `lib.rs`.
#[cfg(all(feature = "default-transport", not(target_family = "wasm")))]
impl Client<crate::DefaultTransport> {
    /// A client with the default transport.
    ///
    /// On native this requires a surrounding tokio runtime: `tokio::spawn`
    /// and `tokio::time::sleep` panic outside a runtime. reqwest behaves
    /// exactly the same way. The explicit path without this requirement is
    /// `Client::builder(Native::new(rt, tls, dns))` with a runtime of your
    /// choice (see `crates/http-ng/tests/two_runtimes.rs`, the same
    /// constructor for tokio and for smol).
    ///
    /// # Panics
    ///
    /// Panics in exactly one case: `Rustls::with_platform_verifier()`
    /// couldn't read the OS's system trust store (`rustls-platform-
    /// verifier`'s own view is that this is a runtime environment
    /// condition, not a client configuration error). A client setting the
    /// transport doesn't support (`UnsupportedCapability`, this function's
    /// error type) doesn't cause a panic — it comes back as an ordinary
    /// `Err`.
    ///
    /// The non-panicking alternative is [`Client::try_new`]: the same
    /// construction, but with both failure modes (trust store AND
    /// unsupported setting) as `Err`, for a caller that must handle any
    /// failure, not just the second one. `.expect(...)` here, rather than
    /// propagating through this function's `Result`, is the same decision
    /// as before: `UnsupportedCapability` (`what`, `backend`, both
    /// `&'static str`) is a typed answer to "the transport doesn't support
    /// this particular client setting", not to "the system trust store
    /// couldn't be read", and conflating them would mean lying about the
    /// category of failure. `try_new` below resolves this not by
    /// conflating them but with a wider error type — `http_ng_core::Error`,
    /// where both causes are already distinguishable via `ErrorKind`
    /// (`Tls` and `Unsupported` respectively), so the panic here stays an
    /// ergonomics choice for "the common case", not the only path.
    pub fn new() -> Result<Self, http_ng_core::UnsupportedCapability> {
        let transport = Self::default_native_transport().expect("platform verifier");
        Self::builder(transport).build()
    }

    /// The non-panicking version of [`Client::new`] — the exact same
    /// construction (`Native<Tokio, Rustls, SystemDns<Tokio>>` with the
    /// system trust store), but both failure points become `Err` instead
    /// of just one of the two: `Rustls::with_platform_verifier()` gives
    /// `ErrorKind::Tls` (`with_platform_verifier` itself already returns
    /// that; the `?` below simply doesn't silence it into a panic), an
    /// incompatible setting at `build()` gives `ErrorKind::Unsupported`,
    /// the same trick `Client::execute` already applies to
    /// `UnsupportedCapability` from `check_timeouts_supported`
    /// (`Error::new(ErrorKind::Unsupported, e)`, `config.rs`) — not a new
    /// trick invented for this function, but reuse of an existing one.
    ///
    /// For a process where a panic isn't an acceptable way to learn about
    /// an environment without a working system certificate store
    /// (embedded systems, long-lived processes where `catch_unwind` around
    /// the client constructor isn't an option) — this path, not
    /// [`Client::new`].
    pub fn try_new() -> Result<Self, http_ng_core::Error> {
        let transport = Self::default_native_transport()?;
        Self::builder(transport)
            .build()
            .map_err(|e| http_ng_core::Error::new(http_ng_core::ErrorKind::Unsupported, e))
    }

    /// The shared construction of the default transport for
    /// [`Client::new`] and [`Client::try_new`] — the one operation that
    /// can genuinely fail for both (`Rustls::with_platform_verifier()`),
    /// so it's factored out once rather than duplicated. `Result<_,
    /// http_ng_core::Error>`, not `UnsupportedCapability`:
    /// `with_platform_verifier()` already returns an `Error`
    /// (`ErrorKind::Tls`) itself, and `Native::new`/`SystemDns::new` can't
    /// fail at all (ordinary constructors, no IO) — wrapping their
    /// nonexistent failure in a `Result` would have nothing to justify it.
    fn default_native_transport() -> Result<crate::DefaultTransport, http_ng_core::Error> {
        let rt = http_ng_rt_tokio::Tokio;
        let tls = http_ng_tls_rustls::Rustls::with_platform_verifier()?;
        Ok(http_ng_native::Native::new(
            rt,
            tls,
            http_ng_dns_system::SystemDns::new(rt),
        ))
    }
}

// The browser sibling of the `impl Client<crate::DefaultTransport>` above.
// The gate is the mirror image of that one and of `DefaultTransport`'s own
// (`lib.rs`): `all(target_family = "wasm", target_os = "unknown")`, NOT a
// bare `target_family = "wasm"` — `wasm32-wasip2` is also `wasm`, and there
// is deliberately no `DefaultTransport` branch there (see that type's doc
// comment for why `http-ng` doesn't depend on `http-ng-wasi`), so a bare
// wasm gate would `impl` for a type that doesn't exist on that target.
#[cfg(all(
    feature = "default-transport",
    target_family = "wasm",
    target_os = "unknown"
))]
impl Client<crate::DefaultTransport> {
    /// A client with the browser transport.
    ///
    /// No `Result`, unlike the native [`Client::new`] — and no panic
    /// smuggled in to pay for that. The native version's `Result` exists
    /// for one failure that has no counterpart here: reading the OS trust
    /// store. `Fetch::new()` cannot fail at all — it probes the running
    /// browser and stores the answer (`http-ng-fetch`'s `caps::probe`),
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
        Self::builder(http_ng_fetch::Fetch::new())
            .build()
            .expect("fetch transport with default config is always supported")
    }
}

// Not for looks and not to silence `clippy::new_without_default`, though it
// does both: on this target `Client::new()` is infallible and takes no
// arguments, which is exactly the shape `Default` describes. The native
// `Client::new()` returns a `Result` and so has no `Default` to offer —
// the asymmetry is in the constructors, not in this impl. `http-ng-fetch`
// makes the same pairing for `Fetch` itself.
#[cfg(all(
    feature = "default-transport",
    target_family = "wasm",
    target_os = "unknown"
))]
impl Default for Client<crate::DefaultTransport> {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, thiserror::Error)]
#[error("exceeded redirect limit of {0}")]
struct TooMany(u8);

#[derive(Debug, thiserror::Error)]
#[error("Location header is not a resolvable URI")]
struct BadLocation;
