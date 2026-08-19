use http::HeaderName;

/// Who follows a redirect chain: nobody, `Client`, or the backend.
///
/// Three variants, and only the third is branched on anywhere
/// (`check_redirect_supported`, `http-ng/src/config.rs`). That is the whole
/// content of the enum rather than an accident of implementation: the first
/// two differ in what a caller *reading the field* may conclude, the third
/// differs in what `Client` will *do*.
///
/// # What a test can and cannot catch here
///
/// Measured (v0.4 W1), not asserted. With `http-ng-native` made to declare
/// each variant in turn and `cargo nextest run -p http-ng-native -p http-ng
/// --all-features` (362 tests) run against each: `Internal` fails **two** —
/// the capability read-back in `http-ng-native/tests/transport.rs`, and
/// `http-ng::deadline::the_deadline_spans_redirect_hops_rather_than_restarting_on_each`,
/// which dies at `build()` with `UnsupportedCapability { what:
/// "redirect_policy" }`. `None` and `Transparent` each fail exactly **one**,
/// the read-back, and nothing else.
///
/// So `Internal` versus not-`Internal` is the only distinction any behaviour
/// in this workspace can witness; between `None` and `Transparent` the field
/// is a claim a caller reads and no test can contradict. That asymmetry is
/// why a variant here has to earn its place from a *carrier* rather than
/// from a doc comment — nothing else will catch it lying.
///
/// # The two variants that used to be here
///
/// `Configurable` ("We set the policy.") and `Inspectable` ("We set the
/// policy and see every hop.") came from the original design sketch and
/// shipped with exactly those one-sentence docs, no branch, and — after
/// `http-ng-fetch` corrected itself to `Internal` — no carrier. They are
/// gone (v0.4 W1 deliverable 1, a v0.3-era defect), for the reason
/// `UpgradeSupport` went in v0.3 W4: *a capability variant exists only if a
/// caller decision turns on it.*
///
/// What made `Configurable` unimplementable rather than merely unused:
/// **the policy never crosses the seam.** `Client::run` merges the
/// client-level and per-request `RedirectPolicy` and deliberately does not
/// write the result back into the request's extensions — *"no transport
/// reads a `RedirectPolicy`"* (`http-ng/src/client.rs`). A backend claiming
/// to set the policy could see only what a `RequestBuilder` happened to
/// leave in the extension bag, never one set on the client, so
/// `Client::builder(..).redirect(Limited(2))` would be silently ignored by
/// the one variant whose name promises to honour it.
///
/// `http-ng-native` was its only carrier, and it declared `Configurable`
/// while containing no redirect handling at all — zero matches for
/// `Location` or a 3xx status in its `src/`. It reports `Transparent` now,
/// which is what it always did. `http-ng-fetch` had made the same mistake
/// and corrected it to `Internal` a vertical earlier, in an audit that
/// never reached the native crate; its
/// `redirects_are_internal_not_configurable` still names the variant, and
/// is the record of that.
///
/// Re-adding a variant is a breaking change for an external `match` — this
/// enum is deliberately not `#[non_exhaustive]`, see [`CancelSupport`] —
/// and that cost is the point: the variant should arrive *with* the backend
/// that carries it. The nearest candidates are a `libcurl` backend
/// (`CURLOPT_FOLLOWLOCATION` plus `CURLOPT_MAXREDIRS` is a genuinely
/// declarative policy) and WinHTTP; neither is planned, and both would also
/// need the seam to start carrying the merged policy.
///
/// **URLSession is not that backend, and that is the evidence this was
/// decided on**, because `http-ng-urlsession` (v0.4 W3) is the next
/// platform stack due and the obvious place a "configurable" backend would
/// come from. It offers no declarative redirect policy at all: the hook is
/// `urlSession(_:task:willPerformHTTPRedirection:newRequest:completionHandler:)`,
/// whose completion handler takes *"either the value of the `request`
/// parameter, a modified URL request object, or `NULL` to refuse the
/// redirect and return the body of the redirect response"*, and which
/// *"is called only for tasks in default and ephemeral sessions. Tasks in
/// background sessions automatically follow redirects."* (Apple, developer
/// documentation for that method.) The platform therefore hands out exactly
/// two of the variants below — `Internal` for a background session, where
/// there is no hook to install, and `Transparent` for a foreground one, by
/// answering `nil` so the 3xx becomes the task's response and `Client`'s
/// stage does the chain. A third reading, following inside the delegate
/// while counting hops there, is not a platform affordance but a second
/// implementation of `http_ng_proto::redirect` — and one that would silently
/// drop what `Client`'s stage carries per hop: `SENSITIVE_HEADERS` stripped
/// across an origin, cookies re-derived rather than carried, and the
/// `AllowEarlyData` mark taken off.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RedirectSupport {
    /// No redirects, and nothing to observe.
    ///
    /// The conservative base: `Capabilities::none()` returns this same
    /// value, so "the backend said nothing about redirects" and "the
    /// backend said `None`" are the same observation. This is exactly why
    /// "3xx arrives as-is" gets its own `Transparent`: conflating "the
    /// field wasn't filled in" with a substantive claim about backend
    /// behavior means having a capability that lies (branch final review
    /// M2).
    None,
    /// The backend doesn't follow redirects itself: the 3xx arrives at us
    /// as an ordinary response, and following the chain is the job of the
    /// redirect stage in `Client`.
    ///
    /// Not the same as `None`, even though `Capabilities::none()` also
    /// returns `None`: here redirects are fully observable and controllable,
    /// just not by the backend. `RedirectPolicy` works and does exactly
    /// what it promises.
    ///
    /// This is what `wasi:http` does (review Task 16 resolution, finding
    /// B-9 — measured on a live wasmtime host: the 3xx response reaches the
    /// guest as-is). Before branch final review M2, `WasiHttp` was forced to
    /// claim `None` for lack of this variant, and a caller who concluded
    /// from `redirects == None` that redirects were impossible here was
    /// wrong about the one backend that actually existed.
    ///
    /// **Three backends report it**: `http-ng-wasi`, `http-ng-h3`, and —
    /// since v0.4 W1 — `http-ng-native`, which had said `Configurable`
    /// since v0.1 while implementing nothing of the sort. It is also what
    /// an `http-ng-urlsession` on a default or ephemeral session would
    /// report, by refusing each hop in its delegate; see the type's own
    /// doc for why that is a choice the platform allows and not one it
    /// makes.
    Transparent,
    /// The backend follows redirects itself; we neither control nor see it.
    ///
    /// **The example is in this workspace**: `http-ng-fetch` reports this
    /// variant. A browser's `fetch()` with `redirect: "follow"` (the
    /// default, and the only thing that crate ever sends — its
    /// `convert.rs` never calls `RequestInit::set_redirect`) follows the
    /// redirect inside the browser, and the JS code sees only the final
    /// response, with no way to intercept the intermediate hops.
    ///
    /// For such a backend, `Client`'s redirect stage will never see a
    /// single 3xx, and whatever `RedirectPolicy` was set would be a silent
    /// no-op. So `check_supported` **does** check this field
    /// (`http-ng/src/config.rs`, `check_redirect_supported`): a
    /// `RedirectPolicy` the caller actually asked for — client-level at
    /// `build()`, or per-request at `execute()`, whichever is in effect —
    /// against an `Internal` backend is an `UnsupportedCapability { what:
    /// "redirect_policy" }`, not a setting that quietly does nothing. A
    /// caller who configured nothing is unaffected: that is why
    /// `Config::redirect` is an `Option`.
    ///
    /// An earlier version of this doc said the example was "not in this
    /// workspace" and that the field was deliberately unchecked. Both
    /// halves stopped being true when `http-ng-fetch` landed.
    ///
    /// The variant an `http-ng-urlsession` **background** session will have
    /// to report, and there it is forced rather than chosen: the redirect
    /// delegate is not called for background tasks at all.
    Internal,
}

/// Whether dropping the future returned by
/// [`Transport::execute`](crate::unversioned::Transport::execute) stops the
/// exchange — see that method's doc comment for the contract itself, of
/// which this enum is the one honest way out.
///
/// # Why two variants and not three
///
/// A first draft had a third variant splitting `Supported` by who performs
/// the cancellation: the transport tearing down a socket it owns, versus
/// the transport asking an ambient host to stop. The split was dropped
/// because no caller decision turns on it. A capability answers a question
/// the caller actually asks — here, "can I rely on a drop ending the
/// exchange?" — and *who* ends it is an implementation detail. Both shapes
/// give a guarantee of exactly the same strength, including its limit:
/// bytes already sent are already sent, and the server may have acted on
/// them either way.
///
/// The distinction is worth knowing even though it isn't worth a variant,
/// and connection pooling (v0.2 W2) is where it will matter: `http-ng-native`
/// owns the socket and closes it itself, while `http-ng-fetch` and
/// `http-ng-wasi` ask the browser and the `wasi:http` host respectively —
/// `AbortController::abort()` and the Component Model's `subtask.cancel`.
/// A pool can only exist for the first kind, which is the same reason W2
/// puts the pool in `http-ng-native` and nowhere else.
///
/// Not `#[non_exhaustive]`, deliberately: no other enum in this file is
/// (`RedirectSupport`, `TlsSupport`, `DecompressionSupport` are all plain), and
/// consistency across the capability set is worth more than reserving the
/// right to add a variant to this one alone.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CancelSupport {
    /// Dropping the future does not stop the exchange: it may run to
    /// completion, unobserved, on a connection this transport no longer
    /// reports on.
    ///
    /// The conservative base — [`Capabilities::none()`] returns this — and
    /// here, unlike [`RedirectSupport::None`], that costs nothing. The
    /// lesson recorded on `RedirectSupport::Transparent` was not "every
    /// capability needs a variant for the unfilled case", it was "a default
    /// must never be stronger than the truth". A backend that never touches
    /// this field is read as "do not rely on a drop stopping anything",
    /// which is the safe reading of silence and is also exactly what a
    /// backend that genuinely cannot cancel means. The two coincide, so
    /// there is nothing to tell apart — whereas for redirects they did not:
    /// `None` there is a substantive "redirects are impossible", which is a
    /// far stronger claim than "the field was not filled in", and a
    /// transparent backend forced to say it was misread.
    None,
    /// Dropping the future stops the exchange, as far as this transport
    /// controls it.
    ///
    /// What that does and does not promise is the contract on
    /// [`Transport::execute`](crate::unversioned::Transport::execute); the
    /// short version is that our side stops, and the server's side is not
    /// ours to promise anything about.
    Supported,
}

/// Whether a request may travel over a connection an earlier request
/// already used, or whether every request opens a socket of its own.
///
/// # Why two variants and not three
///
/// The v0.2 design document asked for three, on
/// [`RedirectSupport`]'s precedent: reuse that is ours and configurable,
/// reuse that belongs to an ambient host and is not ours to control
/// (`http-ng-fetch`, `http-ng-wasi`), and none. The middle one is not here,
/// and the reason is a sharper reading of [`RedirectSupport`] than "the
/// owner differs".
///
/// [`RedirectSupport::Internal`] earns its variant because
/// `check_supported` **refuses** on it: `ClientBuilder::redirect` exists,
/// it is a portable, client-level setting, and a backend that follows
/// redirects internally would silently ignore it — so the variant is what
/// turns a silent no-op into an `UnsupportedCapability`. That is a caller
/// decision, made by code that can be pointed at.
///
/// No such setting exists for reuse. The pool is configured on the
/// concrete transport that owns it (`http_ng_native::Native::pool`),
/// because a pool's idle timeout is a property of a connection between
/// requests and not of any one request — so there is nothing for
/// `check_supported` to refuse, and a caller holding a generic `T:
/// Transport` learns nothing actionable from *who* keeps the connection
/// alive. The question the design document itself named — "are my requests
/// going over reused connections, because it changes how I batch work" —
/// is answered by the two variants below, and adding "who owns it" would
/// re-add exactly the axis [`CancelSupport`] rejected one capability
/// earlier.
///
/// **The condition under which the third variant arrives**, written down
/// so the next reader does not have to re-derive it: as soon as there is a
/// portable, client-level pool setting that a host-managed backend would
/// have to reject, the variant arrives *together with that setting and
/// with its arm in `check_supported`* — the same order in which
/// [`RedirectSupport::Transparent`] arrived, once a backend existed that
/// was being misread without it. Not before: a variant no caller can
/// branch on is a distinction the capability set has to carry forever for
/// nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReuseSupport {
    /// Every request opens a new connection, and closes it when it is done.
    ///
    /// The conservative base — [`Capabilities::none()`] returns this — and,
    /// as with [`CancelSupport::None`], silence and the substantive claim
    /// coincide: a caller who reads this plans for a handshake per request,
    /// which is exactly what a backend that never filled the field in will
    /// give them.
    None,
    /// Requests to the same origin may travel over a connection an earlier
    /// request already used.
    ///
    /// Says nothing about *when* one is reused — that depends on what is
    /// idle at the moment, and no caller can predict it per request. What
    /// it does promise is the thing a caller batches on: a second request
    /// to an origin need not pay for a TCP and TLS handshake again.
    Supported,
}

/// Whether the transport hands back a response body it has already
/// decoded, or the bytes exactly as the server put them on the wire.
///
/// The question a caller asks of this is "must I reverse a
/// `Content-Encoding` myself, and may I ask for one?" — both halves at
/// once, because they are one fact about the transport. `http-ng`'s
/// `Client` is that caller: it reads this field and nothing else to decide
/// whether to advertise `Accept-Encoding` and whether to decode.
///
/// # Why this is NOT read off `forbidden_request_headers`
///
/// `http-ng-fetch` lists [`http::header::ACCEPT_ENCODING`] among its
/// forbidden request headers, and it also decompresses internally, so on
/// that one backend the two answers coincide — which is exactly what makes
/// deriving one from the other tempting and wrong. "This header cannot be
/// sent" and "the body reaching you is already decoded" are different
/// claims: a transport that forbids the header while decompressing nothing
/// is perfectly coherent (a proxy-shaped backend that pins its own
/// `Accept-Encoding`, say), and a client that inferred "already decoded"
/// from "header forbidden" would hand that caller compressed bytes
/// labelled as plaintext. That is the "capability that lies" defect this
/// workspace has caught four times, which is why this is its own field.
///
/// The reverse inference is just as wrong and is the one `Client`
/// implements: a `None` transport that forbids `Accept-Encoding` gets no
/// header from us and still gets its response decoded, because a
/// `Content-Encoding` the server applied unbidden is still ours to reverse.
///
/// # Why two variants and not three
///
/// [`CancelSupport`]'s rule, applied a third time: a variant exists only
/// if a caller decision turns on it. The third variant that suggests
/// itself is "the transport can decompress, if asked" — configurable
/// rather than automatic. No transport in this workspace or outside it
/// works that way today, and there is no client-level setting for it to
/// answer: `Client` does not offer "decompress, but at the transport
/// layer". A variant no caller can branch on is a distinction the
/// capability set carries forever for nothing.
///
/// **The condition under which it arrives**, on
/// [`RedirectSupport::Transparent`]'s precedent: together with the setting
/// that asks for it and its arm in `check_supported`, once a backend
/// exists that is being misread without it. Not before.
///
/// # Silence and the substantive claim coincide here
///
/// [`Self::None`] is what [`Capabilities::none()`] returns, so "the
/// backend never filled this in" and "the backend hands the bytes over
/// untouched" are the same value — and, as with [`CancelSupport::None`]
/// and [`ReuseSupport::None`], that costs nothing, because the two mean
/// the same thing to a caller: decode it yourself. The
/// [`RedirectSupport`] problem, where `None` was a strictly stronger claim
/// than silence and a `Transparent` backend was misread for lack of a
/// third value, does not arise.
///
/// Not `#[non_exhaustive]`, for consistency with every other enum in this
/// file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DecompressionSupport {
    /// The response body arrives exactly as it came off the wire: a
    /// `Content-Encoding` the server applied is still applied, and
    /// reversing it belongs to whoever reads the body.
    ///
    /// The conservative base — [`Capabilities::none()`] returns this — and
    /// the honest answer for every transport that moves bytes rather than
    /// interpreting them: `http-ng-native` (hyper hands the body through
    /// as it arrives) and `http-ng-wasi` (`wasi:http` 0.3 defines no
    /// content-coding behaviour of its own) are both this.
    None,
    /// The transport decodes `Content-Encoding` itself, before a single
    /// byte reaches us, and chooses what to ask for — so `Accept-Encoding`
    /// is not ours to set either, and decoding again would corrupt every
    /// compressed response.
    ///
    /// Named after [`RedirectSupport::Internal`], and for the same shape
    /// of reason: the backend does it, we neither control nor see it. The
    /// example is again the browser — `http-ng-fetch` reports this,
    /// derived from the same in-crate fact its `Body::size_hint` already
    /// rests on (a `Content-Length` under a `Content-Encoding` describes
    /// bytes this transport never yields, because the browser has already
    /// reversed the coding).
    ///
    /// Note what this does NOT promise: that the response headers were
    /// tidied up afterwards. `fetch` leaves `Content-Encoding` and
    /// `Content-Length` on the response describing the wire, not the body
    /// you get — which is precisely why the size hint has to distrust
    /// them.
    Internal,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TlsSupport {
    None,
    ServerTrustCallbackOnly,
    Full,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TimeoutSupport {
    /// Whether [`Timeouts::resolve`] is enforced. Honestly `false` on
    /// every ambient backend: `wasi:http` and `fetch` do the resolving
    /// inside the host, so there is no moment for a client to bound.
    pub resolve: bool,
    pub connect: bool,
    pub first_byte: bool,
    pub between_bytes: bool,
}

/// The timeout triple — `wasi:http`'s shape, the richest of the ambient
/// models.
///
/// Collapses to a single `AbortController` in fetch; on native it splits
/// into connector / response-wait / body-idle. A single `Duration` throws
/// away information the WASI backend knows how to use.
///
/// Lives in `http-ng-core` because transports read it from the request's
/// `http::Extensions`, and they don't depend on `http-ng`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Timeouts {
    /// A bound on **getting an address to try**, separate from the connect
    /// budget that follows it.
    ///
    /// # What it bounds, which is not a phase boundary
    ///
    /// Happy Eyeballs interleaves resolution with connecting on purpose —
    /// the resolver is a `Stream` and `http-ng-native` starts connecting to
    /// the first address while the rest are still arriving — so there is no
    /// instant at which *resolution finished*, and a bound on one would
    /// have nothing to attach to. What this bounds is the wait for the
    /// **first** address from either family, which is exactly the failure a
    /// caller cannot otherwise diagnose: a resolver that hangs looks like
    /// an origin that is unreachable, and only the first is worth a
    /// different retry.
    ///
    /// It therefore does **not** apply where the connection does not depend
    /// on the resolver — an IP literal, and an HTTPS record carrying
    /// address hints, both of which give a connector somewhere to go
    /// without an answer.
    ///
    /// # Why not simply a smaller `connect`
    ///
    /// Because the two answer different questions and a caller who cares
    /// wants both: `resolve` says *how long may I wait to learn where to
    /// go*, `connect` says *and how long may going there take*. Folding
    /// them loses which one failed, which is the whole gap. Overlapping
    /// budgets are the caller's to reconcile; nothing here subtracts one
    /// from the other, because a resolver that answered in 10 ms has not
    /// spent any of the connect budget in any sense a connector can see.
    pub resolve: Option<core::time::Duration>,
    pub connect: Option<core::time::Duration>,
    pub first_byte: Option<core::time::Duration>,
    pub between_bytes: Option<core::time::Duration>,
}

/// Whether a transport can put a request into TLS 1.3 early data (0-RTT).
///
/// # This is the floor, and it says less than it looks like
///
/// [`Self::Supported`] means only *"this transport is able to offer early
/// data"*. It never means a particular request went into early data, and it
/// never means one was accepted. In QUIC the acceptance verdict arrives
/// **after the response** — measured at 8.63 ms against a response at
/// 8.58 ms (`docs/h3-research.md` §3.2) — so it is a future, not a
/// property of a transport, and nothing about it can live in a value that
/// [`Transport::capabilities`](crate::unversioned::Transport::capabilities)
/// determines once at construction.
///
/// # Why the default is `None` with unusual force
///
/// Every other capability here follows the rule that a default must not be
/// stronger than the truth, and the cost of breaking it is a buffered copy,
/// a lost optimisation, or — for `full_duplex` — a deadlock. This one costs
/// **replay exposure**: early data is data an attacker who captured it can
/// send again, at a moment of their choosing, to a server that will act on
/// it. So [`Capabilities::none()`] reports `None`, every transport that
/// ships today reports `None`, and a transport that forgets this field
/// reports `None`.
///
/// # Reporting `Supported` is not sufficient to put anything in early data
///
/// It is necessary and nothing more. The gate is the caller's, per request
/// — see [`AllowEarlyData`] — and a transport that reports `Supported` must
/// still refuse to place a request the caller did not mark.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EarlyDataSupport {
    /// This transport never offers early data. The conservative base, what
    /// [`Capabilities::none()`] returns, and the honest answer for every
    /// transport in this workspace except `http-ng-h3`.
    None,
    /// This transport can offer early data for a request the caller has
    /// marked with [`AllowEarlyData`]. See this enum's doc for the three
    /// things it still does not mean.
    Supported,
}

/// The caller's per-request statement that this request may go into TLS 1.3
/// early data (0-RTT).
///
/// Put into `http::Extensions` on the request. Absent, the request waits
/// for the handshake to complete, and **there is no configuration in which
/// a request the caller did not mark ends up in early data**. Present
/// against a transport reporting [`EarlyDataSupport::None`], it is a typed
/// [`UnsupportedCapability`] rather than a silent no-op.
///
/// # What marking a request asserts, and what it does not
///
/// **It is an assertion that replaying this request is SAFE — not that
/// replaying it is POSSIBLE.** Those are different questions, and only the
/// caller can answer the first one.
///
/// [`RequestBody::retry_kind`](crate::RequestBody::retry_kind) answers the
/// second: `Free`, `ViaFactory`, `Impossible` — *can I send these bytes
/// again*. A transport needs that answer, because a rejected 0-RTT request
/// has to be replayed after the handshake and a
/// [`RetryKind::Impossible`](crate::RetryKind::Impossible) body cannot be.
/// So `RetryKind` is a **correctness** precondition here, and it is checked
/// as one.
///
/// It is emphatically **not** a safety condition, and reading it as one is
/// the mistake this paragraph exists to prevent. `POST /transfer` with a
/// fully buffered body is `RetryKind::Free` — trivially replayable, and
/// precisely the request that must never enter early data, because *an
/// attacker* can replay it too. quinn says the same in one line: *"this
/// enables transmission of 0-RTT data, which is vulnerable to replay
/// attacks, and should therefore never invoke non-idempotent operations"*.
///
/// The notion that would answer the safety question — method safety and
/// idempotency — deliberately does not exist in this codebase, and its
/// absence is written down where the one v0.2 retry lives. RFC 8470 §2 puts
/// the default on the conservative side (*"clients MAY send requests with
/// safe HTTP methods … and MUST NOT send unsafe methods (or methods whose
/// safety is not known) in early data"*) and, in the same sentence, says
/// why a method table cannot be the whole answer: *"absent other
/// information"*. `GET` is not safe on plenty of real APIs, and only the
/// caller knows which. Hence this extension: **a caller-visible decision,
/// with a method check beneath it, rather than a table hidden in a
/// transport.**
///
/// # The third failure path
///
/// A request placed in early data can fail in three places, not one: no
/// usable key material (nothing was risked, fall back silently), the server
/// rejecting the 0-RTT keys (replay on the same connection once the
/// handshake finishes — the transport's job, invisible to the caller), and
/// **HTTP `425 Too Early`** (RFC 8470 §5.2), which arrives a full round
/// trip later and must be retried *not* in early data. The third is a
/// status-code branch in the client, not in a transport.
///
/// **A retry built for a `425` must remove this extension from the request
/// it replays.** RFC 8470 requires it, and it is not a formality: on
/// `http-ng-h3` this mark is part of the connection pool's key, so a
/// replay that kept it would ask for the early-data connection and — if
/// that one has been evicted or closed since — would open a fresh one and
/// go out in early data again, to the server that just refused to risk it.
/// See `http_ng_h3::early`.
///
/// # The other boundary: an origin
///
/// The mark does not cross one, and `http-ng`'s redirect stage drops it on
/// the same condition that drops `Cookie` and `Authorization` — the host or
/// scheme changed.
///
/// The asymmetry is the point and the two halves are easy to conflate. This
/// is a claim about what a request does **at a server**, so a caller who
/// marked a request for origin A never judged origin B, and carrying it
/// across would act on a judgement nobody made. A *method* change is the
/// opposite case and the mark stays: a `303` rewriting `POST` to `GET`
/// leaves a request strictly less consequential than the one already
/// vouched for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AllowEarlyData;

/// The caller's per-request statement that this request needs a particular
/// HTTP version, and must fail rather than go out over another one.
///
/// Put into `http::Extensions` on the request, and read by the transport at
/// the moment the protocol becomes known — which is **before the head is
/// written**, on every transport here that honours one. Absent, the
/// transport picks as it always did.
///
/// # It is [`AllowEarlyData`]'s mechanism with the polarity reversed
///
/// Same shape: a mark in the request's extensions that a transport reads
/// and acts on before sending, `Copy`, defined in this crate because
/// transports read it and do not depend on `http-ng`. The difference is
/// that one is a permission and this is a requirement, and that difference
/// is why both have to be per request rather than per client — see below.
///
/// # Why a demand and not a question
///
/// [`Capabilities::full_duplex`] and its neighbours report the **floor**:
/// the value that holds on the worst protocol a transport might negotiate.
/// That is right for a static answer and cannot be otherwise — Cargo
/// unifies features across a graph, so a library built on `http-ng` can
/// never know whether some other crate turned `http2` on — but it leaves a
/// caller who genuinely needs HTTP/2 with no way to act.
///
/// The two answers that do not work were measured against the code first
/// (`docs/v03-acceptance.md`, and Appendix A of `docs/v04-design.md`):
///
/// - **Per response.** `Response::version()` already answers it, honestly,
///   and *after the fact*. A caller structured for bidirectional streaming
///   has to decide before it sends.
/// - **Per connection.** There is no connection handle in the public API,
///   so it means either a new seam or a query answered from a pool — and
///   the pooled answer is racy in the way that matters: the entry can be
///   evicted between the answer and the request that relied on it. It
///   would be a fact about the past presented as a promise about the next
///   request.
///
/// This is the third: the caller states the requirement, and the transport
/// converts "the floor says no" into "this connection says yes" for one
/// request, or fails it before committing to a shape that would deadlock.
///
/// # Why it cannot be a client-level setting
///
/// Turning an ALPN outcome into a request failure is **correct for gRPC**,
/// whose RPC cannot proceed over HTTP/1.1 at all, and **wrong for a
/// browser-shaped client**, which should degrade quietly. Only the caller
/// knows which of the two it is — the same argument that put
/// [`AllowEarlyData`] in the caller's hands rather than in a transport's
/// configuration.
///
/// # Exact match, deliberately, not a minimum
///
/// `RequireVersion(HTTP_2)` is satisfied by HTTP/2 and by nothing else. It
/// is tempting to read it as "at least", and there is no ordering that
/// makes that mean anything: a caller who needs h2 framing does not want
/// HTTP/3 instead, and a caller who needs HTTP/1.1 — to keep an upgrade
/// path open, say — wants strictly less than HTTP/2, not more. A "minimum"
/// reading would satisfy the first demand with the wrong protocol and be
/// unable to express the second at all.
///
/// # Refusal, and the two shapes it takes
///
/// - The **backend cannot honour demands at all**
///   ([`Capabilities::version_select`] is `false` — `http-ng-fetch` and
///   `http-ng-wasi`, neither of which chooses or even learns the version):
///   a typed [`UnsupportedCapability`] from `Client`, the same arm a
///   [`RedirectPolicy`](https://docs.rs/) against
///   [`RedirectSupport::Internal`] takes. It fires whatever version was
///   demanded, because the backend cannot answer for any of them.
/// - The **backend honours demands and this connection does not match**:
///   a typed [`VersionNotAvailable`] under
///   [`ErrorKind::Unsupported`](crate::ErrorKind::Unsupported), raised by
///   the transport before the head goes out.
///
/// A transport that always speaks one version still *honours* demands —
/// `http-ng-h3` reports `version_select: true` and answers
/// `RequireVersion(HTTP_3)` by proceeding and everything else with
/// [`VersionNotAvailable`]. Reporting `false` there would refuse the one
/// demand it trivially satisfies.
///
/// # The origin boundary, and why this one crosses it
///
/// [`AllowEarlyData`] comes off on a cross-origin redirect, because
/// "replaying this is safe" is a claim about what a request does *at a
/// server* and the caller judged only the first one. **This mark is not
/// that kind of claim.** It is a statement about the caller's own code —
/// "the thing I am about to do needs this protocol" — and it is equally
/// true at hop 1 and at hop 4. Dropping it across an origin would mean a
/// redirect could silently deliver over HTTP/1.1 exactly the request that
/// said it could not use HTTP/1.1, which is the failure the mark exists to
/// prevent, arriving through the one door left open.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RequireVersion(pub http::Version);

/// A [`RequireVersion`] demand the connection in hand does not satisfy.
///
/// Carries both halves, because "HTTP/2 was required" and "HTTP/1.1 is
/// what this connection negotiated" are separately actionable — the first
/// is the caller's own request coming back, the second is a fact about the
/// server or the TLS configuration.
///
/// One type in this crate rather than one per backend (the shape
/// `http_ng_h3::RequestTrailersNotSent` takes), because a caller
/// downcasting on it must not have to know which transport is underneath:
/// the demand is portable, so its refusal is too.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error(
    "the request required {required:?} and this connection negotiated {negotiated:?}; \
     it was refused before the head was written"
)]
pub struct VersionNotAvailable {
    pub required: http::Version,
    pub negotiated: http::Version,
}

/// The one comparison, shared by every transport that honours a demand.
///
/// `Ok(())` when there is no demand or `negotiated` satisfies it; a typed
/// [`VersionNotAvailable`] under
/// [`ErrorKind::Unsupported`](crate::ErrorKind::Unsupported) otherwise.
///
/// A function here rather than a `==` at each call site so that the rule —
/// exact match, absence means no demand — has one definition. Two
/// transports enforce it today and they must not drift.
///
/// **What it does not do is decide *when* to call it.** That is the whole
/// content of the guarantee: `check_version` at the wrong point is a check
/// that reports a violation after the bytes are already gone. Each caller
/// places it where the protocol is first known and no head has been
/// written, and pins that placement with a test that asserts the server
/// saw nothing.
pub fn check_version(
    extensions: &http::Extensions,
    negotiated: http::Version,
) -> Result<(), crate::Error> {
    match extensions.get::<RequireVersion>() {
        Some(&RequireVersion(required)) if required != negotiated => Err(crate::Error::new(
            crate::ErrorKind::Unsupported,
            VersionNotAvailable {
                required,
                negotiated,
            },
        )),
        _ => Ok(()),
    }
}

/// What the transport can do **in this process, right now**.
///
/// A runtime fact, not a `cfg!`: one wasm binary runs in both Chrome
/// (streaming request body available since 131) and Safari (not available).
///
/// # Two kinds of field, and the difference was never written down
///
/// Every field here is one of two things, and reading them as one kind is
/// how `docs/competitive-gaps.md` §7 came to ask whether
/// [`proxy`](Self::proxy) "should have a reader at all".
///
/// - **A gate.** The field guards a setting a caller made on the
///   *`Client`*, and `ClientBuilder::build` refuses when the transport
///   cannot honour it — the model this whole type exists for, taken from
///   `wasi:http`'s own setters returning
///   `result<_, request-options-error::not-supported>`. A gate with no
///   branch is the *silently ignored setting* defect, and this project has
///   closed four of them: `redirects`, `owns_cookie_jar`, `owns_cache` and
///   the `timeouts` triple each earned a branch the day the setting
///   arrived.
/// - **A report.** The field states a fact about the transport, and
///   nothing at the client level could refuse it, because the setting it
///   describes is configured *on the transport*. `proxy`, `client_certs`,
///   `tls_config`, `early_data`, `connection_reuse`, `cancel_on_drop`,
///   `full_duplex`, `streaming_request_body`, the two trailer flags and
///   `version_reported` are all this kind. Its reader is the caller.
///
/// **A report is not a dead field.** `upgrade` was deleted for having no
/// reader, and the difference is that its four variants encoded a
/// distinction with one reachable side — where a report has both values
/// reachable and answers a question only it can answer.
///
/// The classification is enforced rather than described:
/// `every_capability_is_a_gate_or_a_report` in this module destructures
/// the struct with no `..`, so a field added later is a compile error
/// until somebody decides which kind it is.
#[non_exhaustive]
#[derive(Debug, Clone)]
pub struct Capabilities {
    /// Whether a request body may be written as it is produced.
    ///
    /// Reported. `Client` does not gate on it — see this type's doc for
    /// why some fields do and some do not — which is why
    /// `http-ng-urlsession` refuses a `Streaming` body with a typed error
    /// of its own rather than relying on a check that does not happen.
    pub streaming_request_body: bool,
    /// Whether the response may begin arriving before the request body has
    /// finished. Reported.
    pub full_duplex: bool,
    /// Reported.
    pub request_trailers: bool,
    /// Reported.
    pub response_trailers: bool,
    /// Who follows a redirect — see [`RedirectSupport`].
    ///
    /// **A gate**: `RedirectPolicy` and a redirect predicate are `Client`
    /// settings, and [`RedirectSupport::Internal`] means the transport has
    /// already followed the chain by the time anything is handed back, so
    /// either setting would silently not apply.
    pub redirects: RedirectSupport,
    /// What dropping an in-flight `execute` future does — see
    /// [`CancelSupport`] and the contract on
    /// [`Transport::execute`](crate::unversioned::Transport::execute).
    pub cancel_on_drop: CancelSupport,
    /// Whether a connection is reused across requests — see
    /// [`ReuseSupport`].
    pub connection_reuse: ReuseSupport,
    /// Whether the transport already decoded the response body's
    /// `Content-Encoding` — see [`DecompressionSupport`].
    pub response_decompression: DecompressionSupport,
    /// Whether the transport can put a marked request into TLS 1.3 early
    /// data — see [`EarlyDataSupport`], which says less than its name
    /// suggests and says so at length.
    pub early_data: EarlyDataSupport,
    /// What TLS configuration this transport accepts — see [`TlsSupport`].
    ///
    /// **Reported, not a gate.** A `Client` has no TLS setting to refuse:
    /// the trust store, the client certificate and the ALPN list are all
    /// configured on the `TlsConnect` a transport was built with. See this
    /// type's own doc for the two kinds of field.
    pub tls_config: TlsSupport,
    /// Whether the TLS configuration this transport holds presents a
    /// client certificate.
    ///
    /// Reported, for [`tls_config`](Self::tls_config)'s reason. Read off
    /// `TlsIdentity::presents_client_certs` by the backends rather than
    /// from a constant, which is what stopped one connector giving two
    /// answers depending on which stack held it.
    pub client_certs: bool,
    /// Whether this transport sends through a proxy.
    ///
    /// **Reported, and it will never be a gate** — which is the answer to
    /// a question `docs/competitive-gaps.md` §7 raised and left open. The
    /// setting it would guard is `Native::proxy`, which is on the
    /// transport that would answer the question, so there is nothing at
    /// the client level to refuse. That makes it unlike
    /// [`owns_cookie_jar`](Self::owns_cookie_jar), where the client owns
    /// the setting and the transport owns the conflict.
    ///
    /// It is not [`upgrade`](https://docs.rs/http-ng-core)'s case either,
    /// the four-variant enum deleted for having no reader: both values
    /// here are reachable, and the reader is the caller — *will my
    /// requests go through a proxy* is a question a diagnostic asks and
    /// only this field answers.
    pub proxy: bool,
    /// Whether the transport keeps its own cookie jar: attaching `Cookie`
    /// to outgoing requests and processing `Set-Cookie` on incoming ones,
    /// without being asked.
    ///
    /// `true` for `http-ng-fetch` — the browser does both, and `Cookie` is
    /// on that backend's `forbidden_request_headers`, so a client-side jar
    /// there would not merely be redundant, it would send every cookie
    /// twice and store every `Set-Cookie` twice. `false` for
    /// `http-ng-native` and `http-ng-wasi`.
    ///
    /// # Why a `bool` and not an enum
    ///
    /// The same question [`CancelSupport`] and [`ReuseSupport`] were made
    /// to answer: a variant exists only if a caller decision turns on it.
    /// This field answers exactly one decision — "do I run a jar of my own
    /// for this transport?" — and it is binary. The two axes an enum would
    /// add do not carry decisions:
    ///
    /// - *Who* owns it (the browser, an ambient host) is the split
    ///   [`CancelSupport`] already rejected once, for the same reason.
    /// - Attaching versus storing could in principle come apart, and in
    ///   practice never has: a backend that attaches cookies it did not
    ///   store, or stores cookies it will not attach, is not a shape any
    ///   of the three backends here or any ambient HTTP API takes.
    ///
    /// What it does *not* answer — deliberately, and this is where a third
    /// state would arrive if it ever arrives — is whether a jar-owning
    /// backend can be asked to stop, or its jar inspected. There is no
    /// portable setting for either, so there is nothing to refuse. When a
    /// client-level cookie setting exists, it earns its refusal here the
    /// way [`RedirectSupport::Internal`] earned its variant: the setting,
    /// the variant and the `check_supported` arm arrive together.
    pub owns_cookie_jar: bool,
    /// Whether the transport keeps its own HTTP response cache: serving a
    /// stored response instead of sending, and storing what it fetches,
    /// without being asked.
    ///
    /// `true` for `http-ng-fetch` — the browser has an HTTP cache and
    /// applies it inside `fetch()`. `false` for `http-ng-native`,
    /// `http-ng-h3` and `http-ng-wasi`, none of which stores a response
    /// anywhere. `wasi:http`'s host may well have a cache; the guest
    /// cannot see it, and a capability is a claim about what this code
    /// does rather than about what is downstream of it — the same line
    /// `owns_cookie_jar` holds for the same backend.
    ///
    /// # This field had no reader for four verticals
    ///
    /// It shipped in v0.1 as `false` everywhere but one backend, branched
    /// on nowhere, and was on the same list `version_select` was rescued
    /// from — *a variant exists only if a caller decision turns on it*. The
    /// decision that arrived is `ClientBuilder::cache`, and a client-side
    /// cache against a transport reporting `true` is an
    /// [`UnsupportedCapability`] at `build()`, the same arm
    /// `owns_cookie_jar` takes for a jar and [`RedirectSupport::Internal`]
    /// takes for a redirect policy.
    ///
    /// # Why a `bool` and not an enum
    ///
    /// [`Self::owns_cookie_jar`]'s answer, one field up, applies verbatim:
    /// this field settles exactly one decision — *do I run a cache of my
    /// own for this transport?* — and it is binary. *Who* owns it is the
    /// split [`CancelSupport`] rejected; storing versus serving could in
    /// principle come apart and in practice never has.
    ///
    /// What it deliberately does **not** answer is whether a cache-owning
    /// backend can be asked to bypass, revalidate or clear. There is no
    /// portable setting for any of the three — `fetch()`'s `cache` option
    /// is a browser API a `Transport` seam has no counterpart for — so
    /// there is nothing to refuse. That is where a third state would
    /// arrive if it ever arrives.
    pub owns_cache: bool,
    /// Whether the transport honours a per-request [`RequireVersion`]
    /// demand: reads it, and either serves the request over that version
    /// or fails it with [`VersionNotAvailable`] **before the head is
    /// written**.
    ///
    /// # It says "honours", not "chooses"
    ///
    /// A transport that only ever speaks one version reports `true` if it
    /// answers demands — `http-ng-h3` does, by proceeding on
    /// `RequireVersion(HTTP_3)` and refusing everything else. Reporting
    /// `false` there would make `Client` refuse the one demand it
    /// trivially satisfies, which is the opposite of honest.
    ///
    /// `false` is for a transport that cannot answer at all:
    /// `http-ng-fetch` and `http-ng-wasi` neither select the version nor
    /// learn it (both also report `version_reported: false`), so a demand
    /// against either becomes an [`UnsupportedCapability`] from `Client` —
    /// the same arm a `RedirectPolicy` against
    /// [`RedirectSupport::Internal`] takes.
    ///
    /// # This field had no reader for three verticals
    ///
    /// It shipped in v0.1 as `false` everywhere, branched on nowhere, and
    /// was on its way to being deleted under v0.2's rule that *a variant
    /// exists only if a caller decision turns on it* (`docs/v04-design.md`
    /// P5 catches the same shape in `RedirectSupport`, and two of those
    /// variants were deleted for it). [`RequireVersion`] is the caller
    /// decision that arrived, so the field is kept rather than removed —
    /// but the rule stands, and it is the reason the demand and this
    /// field's first `true` land in one change.
    pub version_select: bool,
    /// Whether `Response::version()` is something the transport observed.
    ///
    /// `false` says the value on the response is `http`'s builder default
    /// standing in for a fact the backend never learned — the browser will
    /// not tell a page which protocol it spoke, and `wasi:http@0.3.0` has
    /// no version concept at all.
    ///
    /// The observability seam asks the same question one field over and
    /// answers it in the event rather than here, because a
    /// [`Hooks`](crate::unversioned::Hooks) impl is handed an
    /// [`Event`](crate::unversioned::Event) and no capabilities:
    /// [`Head::version`](crate::unversioned::Head::version) is `Some`
    /// exactly when this field is `true`. Two spellings of one fact, in
    /// the two places that can each be read on their own.
    pub version_reported: bool,
    pub timeouts: TimeoutSupport,
    pub informational_1xx: bool,
    pub forbidden_request_headers: &'static [HeaderName],
}

impl Capabilities {
    /// Everything off. The base from which a backend turns on what it actually supports.
    pub const fn none() -> Self {
        Self {
            streaming_request_body: false,
            full_duplex: false,
            request_trailers: false,
            response_trailers: false,
            redirects: RedirectSupport::None,
            cancel_on_drop: CancelSupport::None,
            connection_reuse: ReuseSupport::None,
            response_decompression: DecompressionSupport::None,
            early_data: EarlyDataSupport::None,
            tls_config: TlsSupport::None,
            client_certs: false,
            proxy: false,
            owns_cookie_jar: false,
            owns_cache: false,
            version_select: false,
            version_reported: false,
            timeouts: TimeoutSupport {
                resolve: false,
                connect: false,
                first_byte: false,
                between_bytes: false,
            },
            informational_1xx: false,
            forbidden_request_headers: &[],
        }
    }
}

/// A setting the chosen transport cannot honor.
///
/// Returned from `build()` rather than silently ignored. The model is
/// wasi:http itself, whose setters return `request-options-error::not-supported`.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("backend `{backend}` does not support `{what}`")]
pub struct UnsupportedCapability {
    pub what: &'static str,
    pub backend: &'static str,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// **Every field is a gate or a report, and adding one without saying
    /// which is a compile error.**
    ///
    /// The distinction is [`Capabilities`]' own doc; this is what keeps it
    /// from going stale. Destructured with no `..` rest pattern —
    /// `#[non_exhaustive]` blocks that only from outside the crate, which
    /// is why the classification has to live here rather than in
    /// `http-ng`, where the branches themselves are.
    ///
    /// The lists are asserted against each other rather than merely
    /// written: a field named in both, or in neither, fails a line.
    #[test]
    fn every_capability_is_a_gate_or_a_report() {
        let c = Capabilities::none();
        let Capabilities {
            // ── gates: a `Client` setting the transport can refuse ──
            //
            // Each of these has a branch in `http_ng::check_supported` and
            // a test naming the setting it refuses.
            redirects,
            response_decompression,
            owns_cookie_jar,
            owns_cache,
            version_select,
            timeouts,
            forbidden_request_headers,
            // `informational_1xx` is a gate in the other direction: no
            // `Client` setting turns it on, and what it guards is a
            // *claim* — `Native::hooks` clears it, because a transport
            // reporting `true` while reporting nothing is a capability
            // that lies.
            informational_1xx,

            // ── reports: a fact whose setting lives on the transport ──
            streaming_request_body,
            full_duplex,
            request_trailers,
            response_trailers,
            cancel_on_drop,
            connection_reuse,
            early_data,
            tls_config,
            client_certs,
            proxy,
            version_reported,
        } = &c;

        // The gates, at their conservative base: each is the value that
        // refuses a caller's setting rather than silently dropping it.
        assert_eq!(*redirects, RedirectSupport::None);
        assert_eq!(*response_decompression, DecompressionSupport::None);
        assert!(!owns_cookie_jar);
        assert!(!owns_cache);
        assert!(!version_select);
        assert!(!timeouts.resolve && !timeouts.connect);
        assert!(!timeouts.first_byte && !timeouts.between_bytes);
        assert!(forbidden_request_headers.is_empty());
        assert!(!informational_1xx);

        // The reports, likewise understated: a report that over-claims
        // costs a caller correctness, one that under-claims costs an
        // opportunity — the floor rule, which is why `none()` is the base
        // every transport starts from.
        assert!(!streaming_request_body);
        assert!(!full_duplex);
        assert!(!request_trailers);
        assert!(!response_trailers);
        assert_eq!(*cancel_on_drop, CancelSupport::None);
        assert_eq!(*connection_reuse, ReuseSupport::None);
        assert_eq!(*early_data, EarlyDataSupport::None);
        assert_eq!(*tls_config, TlsSupport::None);
        assert!(!client_certs);
        assert!(!proxy);
        assert!(!version_reported);
    }

    #[test]
    fn none_is_the_conservative_base() {
        // Every field, spelled out individually — not
        // `assert_eq!` on the whole struct via a derived `PartialEq`, which
        // `Capabilities` deliberately does not implement (it's
        // `#[non_exhaustive]` so its shape stays ours to change, and a
        // struct-wide `PartialEq` would be a public trait impl added purely
        // for a test's convenience).
        //
        // Destructured with no `..` rest pattern — `#[non_exhaustive]` only
        // blocks that from outside the crate, and this test lives inside it.
        // The count used to be written here as a number and had drifted by
        // two by the time `upgrade` was removed (it said 18 against a
        // struct with 20 fields), which is the whole argument against
        // stating it: the destructure below is the count, and it cannot go
        // stale.
        // A prior version of this comment claimed the individual assertions
        // below "fail informatively when a seventeenth field is added and
        // someone forgets to default it". That was false: a reviewer added a
        // seventeenth field, set it to `true` in `none()`, and the old
        // `let c = Capabilities::none(); assert!(!c.streaming_request_body);
        // ...` form compiled and passed without any indication the new field
        // existed. Only the exhaustive destructure below actually catches
        // that: omitting a field from the pattern is a compile error naming
        // it, because `..` is not present to absorb it silently.
        let Capabilities {
            streaming_request_body,
            full_duplex,
            request_trailers,
            response_trailers,
            redirects,
            cancel_on_drop,
            connection_reuse,
            response_decompression,
            early_data,
            tls_config,
            client_certs,
            proxy,
            owns_cookie_jar,
            owns_cache,
            version_select,
            version_reported,
            timeouts,
            informational_1xx,
            forbidden_request_headers,
        } = Capabilities::none();
        assert!(!streaming_request_body);
        assert!(!full_duplex);
        assert!(!request_trailers);
        assert!(!response_trailers);
        assert_eq!(redirects, RedirectSupport::None);
        assert_eq!(cancel_on_drop, CancelSupport::None);
        assert_eq!(connection_reuse, ReuseSupport::None);
        assert_eq!(response_decompression, DecompressionSupport::None);
        assert_eq!(
            early_data,
            EarlyDataSupport::None,
            "the one capability whose over-claim costs replay exposure rather \
             than a buffered copy"
        );
        assert_eq!(tls_config, TlsSupport::None);
        assert!(!client_certs);
        assert!(!proxy);
        assert!(!owns_cookie_jar);
        assert!(!owns_cache);
        assert!(!version_select);
        assert!(!version_reported);
        assert_eq!(
            timeouts,
            TimeoutSupport {
                resolve: false,
                connect: false,
                first_byte: false,
                between_bytes: false,
            }
        );
        assert!(!informational_1xx);
        assert!(forbidden_request_headers.is_empty());
    }

    #[test]
    fn unsupported_names_both_the_feature_and_the_backend() {
        let e = UnsupportedCapability {
            what: "connect_timeout",
            backend: "wasi:http",
        };
        let msg = e.to_string();
        assert!(msg.contains("connect_timeout"), "{msg}");
        assert!(msg.contains("wasi:http"), "{msg}");
    }

    #[test]
    fn timeout_support_is_per_phase_not_a_single_flag() {
        let t = TimeoutSupport {
            resolve: true,
            connect: true,
            first_byte: true,
            between_bytes: false,
        };
        assert!(t.connect && t.first_byte && !t.between_bytes);
    }

    /// No mark, no opinion. The absence of a demand is the overwhelmingly
    /// common case and it must not cost a request anything, on any
    /// version — including the ones nothing in this workspace speaks, so
    /// that the rule is "absent means silent" rather than "absent means
    /// the ones we happened to list".
    #[test]
    fn an_unmarked_request_is_satisfied_by_every_version() {
        let e = http::Extensions::new();
        for v in [
            http::Version::HTTP_09,
            http::Version::HTTP_10,
            http::Version::HTTP_11,
            http::Version::HTTP_2,
            http::Version::HTTP_3,
        ] {
            assert!(check_version(&e, v).is_ok(), "{v:?}");
        }
    }

    #[test]
    fn a_demand_the_connection_meets_passes() {
        let mut e = http::Extensions::new();
        e.insert(RequireVersion(http::Version::HTTP_2));
        assert!(check_version(&e, http::Version::HTTP_2).is_ok());
    }

    /// The refusal carries both halves and is `Unsupported`, not `Other`:
    /// a caller sorting failures by `kind()` must be able to tell "this
    /// connection cannot do what I asked" from a genuine transport
    /// failure without a downcast.
    #[test]
    fn a_demand_the_connection_misses_is_a_typed_unsupported() {
        let mut e = http::Extensions::new();
        e.insert(RequireVersion(http::Version::HTTP_2));
        let err = check_version(&e, http::Version::HTTP_11).unwrap_err();
        assert_eq!(*err.kind(), crate::ErrorKind::Unsupported);
        let named = std::error::Error::source(&err)
            .and_then(|s| s.downcast_ref::<VersionNotAvailable>())
            .expect("the source must be the typed refusal, not an opaque string");
        assert_eq!(
            *named,
            VersionNotAvailable {
                required: http::Version::HTTP_2,
                negotiated: http::Version::HTTP_11,
            }
        );
    }

    /// Exact match in **both** directions, and the second one is the
    /// interesting half: a caller demanding HTTP/1.1 — to keep an upgrade
    /// path open — must not be quietly served over HTTP/2 on the grounds
    /// that HTTP/2 is "newer". A `>=` comparison would pass this test's
    /// sibling above and fail here, which is why the pair is written out
    /// rather than parameterised into one loop over "mismatches".
    #[test]
    fn a_newer_version_does_not_satisfy_a_demand_for_an_older_one() {
        let mut e = http::Extensions::new();
        e.insert(RequireVersion(http::Version::HTTP_11));
        let err = check_version(&e, http::Version::HTTP_2).unwrap_err();
        assert_eq!(*err.kind(), crate::ErrorKind::Unsupported);
    }

    /// The message names both versions. Not a `Display` assertion for its
    /// own sake: `VersionNotAvailable` reaches a log or a `{e}` far more
    /// often than it reaches a downcast, and a message naming only one of
    /// the two versions leaves the reader unable to tell which end was
    /// wrong.
    #[test]
    fn the_refusal_message_names_both_versions() {
        let msg = VersionNotAvailable {
            required: http::Version::HTTP_2,
            negotiated: http::Version::HTTP_11,
        }
        .to_string();
        assert!(msg.contains("HTTP/2.0"), "{msg}");
        assert!(msg.contains("HTTP/1.1"), "{msg}");
    }
}
