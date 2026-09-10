//! What a transport says it can do, read by `ClientBuilder::build`.
//!
//! [`Capabilities`] is one value, stored at construction — `Transport::
//! capabilities` returns a `&Capabilities`, so a transport answers once
//! and not per request. Everything else here is the vocabulary of one of
//! its fields.
//!
//! **Every field is a gate or a report**, and the difference decides who
//! reads it. A *gate* guards a setting a caller made on the `Client`, and
//! `build()` refuses when the transport cannot honour it — the
//! *silently ignored setting* defect, which this workspace has closed four
//! times. A *report* states a fact about the transport that nothing at the
//! client level could refuse, because the setting it describes is
//! configured on the transport; its reader is the caller. That
//! classification is enforced rather than described — see
//! `every_capability_is_a_gate_or_a_report` in this module.
//!
//! **The stored value is a floor.** Where a transport might negotiate
//! either of two protocols, the honest answer is the one that holds
//! whichever it gets: an over-claimed `full_duplex` deadlocks a caller,
//! an under-claimed one costs a buffered copy. A caller who needs to know
//! what *this* connection can do asks [`crate::req::RequireVersion`]
//! before the head instead.
use http::HeaderName;

/// Who follows a redirect chain: nobody, `Client`, or the backend.
///
/// Only [`Internal`](Self::Internal) is branched on: `Client::build()`
/// refuses a `RedirectPolicy` against a backend that walks the chain
/// itself, because a policy it cannot honour must not be silently ignored.
/// Between [`None`](Self::None) and [`Transparent`](Self::Transparent) the
/// field is a claim a caller reads and nothing in this workspace can
/// contradict — so a variant here earns its place from a backend that
/// carries it, not from being describable.
///
/// # Implementing this
///
/// **The redirect policy never crosses the seam.** `Client` merges the
/// client-level and per-request `RedirectPolicy` and does not write the
/// result into the request's extensions, so a transport cannot read one. A
/// backend that wanted to apply the caller's policy itself would see only
/// what a `RequestBuilder` happened to leave in the extension bag — never
/// one set on the client — so there is deliberately no variant for it.
///
/// A backend that follows redirects internally reports `Internal` and
/// gives up what `Client`'s stage does per hop: `SENSITIVE_HEADERS`
/// stripped across an origin, cookies re-derived rather than carried, and
/// the `AllowEarlyData` mark taken off. Answering the `3xx` to the caller
/// instead — `Transparent` — keeps all of it.
///
/// Apple's `URLSession` is the worked example: a background session has no
/// redirect hook to install and is `Internal`; a foreground one answers
/// `nil` from
/// `urlSession(_:task:willPerformHTTPRedirection:newRequest:completionHandler:)`
/// so the `3xx` becomes the response, and is `Transparent`.
///
/// # Adding a variant
///
/// This enum is deliberately not `#[non_exhaustive]` — see
/// [`CancelSupport`] — so a new variant breaks an external `match`. That
/// cost is the point: it should arrive **with** the backend that carries
/// it. A `libcurl` backend (`CURLOPT_FOLLOWLOCATION` plus
/// `CURLOPT_MAXREDIRS` is a genuinely declarative policy) or WinHTTP would
/// be candidates, and both would also need the seam to start carrying the
/// merged policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RedirectSupport {
    /// No redirects, and nothing to observe.
    ///
    /// The conservative base: `Capabilities::default()` returns this same
    /// value, so "the backend said nothing about redirects" and "the
    /// backend said `None`" are the same observation. This is exactly why
    /// "3xx arrives as-is" gets its own `Transparent`: conflating "the
    /// field wasn't filled in" with a substantive claim about backend
    /// behavior means having a capability that lies.
    #[default]
    None,
    /// The backend doesn't follow redirects itself: the 3xx arrives at us
    /// as an ordinary response, and following the chain is the job of the
    /// redirect stage in `Client`.
    ///
    /// Not the same as `None`, even though `Capabilities::default()` also
    /// returns `None`: here redirects are fully observable and controllable,
    /// just not by the backend. `RedirectPolicy` works and does exactly
    /// what it promises.
    ///
    /// This is what `wasi:http` does: the `3xx` reaches the guest as-is.
    ///
    /// **Reported by** `hclient-wasi`, `hclient-native` on both of its
    /// stacks, and `hclient-urlsession` — the last by refusing each hop in its
    /// delegate, which is a choice the platform allows rather than one it
    /// makes; see that crate's own doc.
    Transparent,
    /// The backend follows redirects itself; we neither control nor see it.
    ///
    /// **The example is in this workspace**: `hclient-fetch` reports this
    /// variant. A browser's `fetch()` with `redirect: "follow"` (the
    /// default, and the only thing that crate ever sends — its
    /// `convert.rs` never calls `RequestInit::set_redirect`) follows the
    /// redirect inside the browser, and the JS code sees only the final
    /// response, with no way to intercept the intermediate hops.
    ///
    /// For such a backend, `Client`'s redirect stage will never see a
    /// single 3xx, and whatever `RedirectPolicy` was set would be a silent
    /// no-op. So `check_supported` **does** check this field
    /// (`hclient/src/config.rs`, `check_redirect_supported`): a
    /// `RedirectPolicy` the caller actually asked for — client-level at
    /// `build()`, or per-request at `execute()`, whichever is in effect —
    /// against an `Internal` backend is an `UnsupportedCapability { what:
    /// "redirect_policy" }`, not a setting that quietly does nothing. A
    /// caller who configured nothing is unaffected: that is why
    /// `Config::redirect` is an `Option`.
    ///
    /// It is also the variant an `hclient-urlsession` **background** session
    /// must report, and there it is forced rather than chosen: the redirect
    /// delegate is not called for background tasks at all.
    Internal,
}

/// Whether dropping the future returned by
/// [`Transport::execute`](crate::transport::Transport::execute) stops the
/// exchange — see that method's doc comment for the contract itself, of
/// which this enum is the one honest way out.
///
/// # Why two variants and not three
///
/// A third variant could split `Supported` by who performs the
/// cancellation: the transport tearing down a socket it owns, versus the
/// transport asking an ambient host to stop. It is not here because no
/// caller decision turns on the difference. A capability answers a question
/// the caller actually asks — here, "can I rely on a drop ending the
/// exchange?" — and *who* ends it is an implementation detail. Both shapes
/// give a guarantee of exactly the same strength, including its limit:
/// bytes already sent are already sent, and the server may have acted on
/// them either way.
///
/// The distinction is worth knowing even though it is not worth a variant:
/// `hclient-native` owns the socket and closes it itself, while
/// `hclient-fetch` and `hclient-wasi` ask the browser and the `wasi:http`
/// host — `AbortController::abort()` and the Component Model's
/// `subtask.cancel`. Only the first kind can pool connections, which is
/// why the pool lives in `hclient-native` and nowhere else.
///
/// Not `#[non_exhaustive]`, deliberately: no other enum in this file is,
/// and consistency across the capability set is worth more than reserving
/// the right to add a variant to this one alone.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CancelSupport {
    /// Dropping the future does not stop the exchange: it may run to
    /// completion, unobserved, on a connection this transport no longer
    /// reports on.
    ///
    /// The conservative base — [`Capabilities::default()`] returns this — and
    /// here, unlike [`RedirectSupport::None`], that costs nothing. **A
    /// default must never be stronger than the truth**: a backend that
    /// never touches this field is read as "do not rely on a drop stopping
    /// anything",
    /// which is the safe reading of silence and is also exactly what a
    /// backend that genuinely cannot cancel means. The two coincide, so
    /// there is nothing to tell apart — whereas for redirects they did not:
    /// `None` there is a substantive "redirects are impossible", which is a
    /// far stronger claim than "the field was not filled in", and a
    /// transparent backend forced to say it was misread.
    #[default]
    None,
    /// Dropping the future stops the exchange, as far as this transport
    /// controls it.
    ///
    /// What that does and does not promise is the contract on
    /// [`Transport::execute`](crate::transport::Transport::execute); the
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
/// (`hclient-fetch`, `hclient-wasi`), and none. The middle one is not here,
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
/// concrete transport that owns it (`hclient_native::Native::pool`),
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
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ReuseSupport {
    /// Every request opens a new connection, and closes it when it is done.
    ///
    /// The conservative base — [`Capabilities::default()`] returns this — and,
    /// as with [`CancelSupport::None`], silence and the substantive claim
    /// coincide: a caller who reads this plans for a handshake per request,
    /// which is exactly what a backend that never filled the field in will
    /// give them.
    #[default]
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
/// once, because they are one fact about the transport. `hclient`'s
/// `Client` is that caller: it reads this field and nothing else to decide
/// whether to advertise `Accept-Encoding` and whether to decode.
///
/// # Why this is NOT read off `forbidden_request_headers`
///
/// `hclient-fetch` lists [`http::header::ACCEPT_ENCODING`] among its
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
/// [`Self::None`] is what [`Capabilities::default()`] returns, so "the
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
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DecompressionSupport {
    /// The response body arrives exactly as it came off the wire: a
    /// `Content-Encoding` the server applied is still applied, and
    /// reversing it belongs to whoever reads the body.
    ///
    /// The conservative base — [`Capabilities::default()`] returns this — and
    /// the honest answer for every transport that moves bytes rather than
    /// interpreting them: `hclient-native` (hyper hands the body through
    /// as it arrives) and `hclient-wasi` (`wasi:http` 0.3 defines no
    /// content-coding behaviour of its own) are both this.
    #[default]
    None,
    /// The transport decodes `Content-Encoding` itself, before a single
    /// byte reaches us, and chooses what to ask for — so `Accept-Encoding`
    /// is not ours to set either, and decoding again would corrupt every
    /// compressed response.
    ///
    /// Named after [`RedirectSupport::Internal`], and for the same shape
    /// of reason: the backend does it, we neither control nor see it. The
    /// example is again the browser — `hclient-fetch` reports this,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TlsSupport {
    #[default]
    None,
    ServerTrustCallbackOnly,
    Full,
}

/// Which of [`crate::req::Timeouts`]' bounds this transport enforces —
/// the field-per-field mirror of that struct, so a refusal can name the
/// bound a caller set rather than saying *timeouts*.
///
/// # `#[non_exhaustive]`, and why an added claim is `false` rather than a
/// compile error
///
/// This struct has grown once — `resolve` joined three fields in v0.4 —
/// and it grows again whenever [`crate::req::Timeouts`] does. The
/// attribute keeps that additive for everyone who reads one, and
/// [`Self::none`] plus the `with_*` setters keep it constructible for
/// everyone who writes one.
///
/// **It was a generated builder whose members were required**, so that a
/// field added here was a compile error at every transport rather than a
/// silent `false`. The argument was that an unset claim is a transport
/// saying it does not enforce a bound, and a claim nobody wrote is the
/// thing to refuse. **The ordering is the other way round**: a transport
/// cannot honestly report `true` before it implements the bound, so on
/// the day a field arrives `false` is the only true answer anywhere
/// except the crate that added the enforcement. The compile error did not
/// surface a decision; it demanded a diff whose content was forced.
///
/// The one time it happened says so. `resolve` arrived together with the
/// connector code in `hclient-native` that enforces it, and `true` was
/// written **once**: every other transport got a `false` carrying no
/// information. And [`Capabilities`] has always derived `Default`, with
/// eleven `bool` capabilities that become `false` when a transport does
/// not set them — so the requirement was giving this struct a property
/// its own container never had.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct TimeoutSupport {
    /// Whether [`crate::req::Timeouts::resolve`] is enforced. Honestly `false` on
    /// every ambient backend: `wasi:http` and `fetch` do the resolving
    /// inside the host, so there is no moment for a client to bound.
    pub resolve: bool,
    pub connect: bool,
    pub first_byte: bool,
    pub between_bytes: bool,
}

impl TimeoutSupport {
    /// No bound enforced — what a transport reports before it enforces
    /// anything, and the value every field of a newly added bound takes.
    ///
    /// Named for what it means rather than for emptiness: this is a
    /// transport's honest opening position, not a placeholder.
    #[must_use]
    pub const fn none() -> Self {
        Self {
            resolve: false,
            connect: false,
            first_byte: false,
            between_bytes: false,
        }
    }

    /// This transport enforces [`crate::req::Timeouts::resolve`].
    #[must_use]
    pub const fn with_resolve(mut self, enforced: bool) -> Self {
        self.resolve = enforced;
        self
    }

    /// This transport enforces [`crate::req::Timeouts::connect`].
    #[must_use]
    pub const fn with_connect(mut self, enforced: bool) -> Self {
        self.connect = enforced;
        self
    }

    /// This transport enforces [`crate::req::Timeouts::first_byte`].
    #[must_use]
    pub const fn with_first_byte(mut self, enforced: bool) -> Self {
        self.first_byte = enforced;
        self
    }

    /// This transport enforces [`crate::req::Timeouts::between_bytes`].
    #[must_use]
    pub const fn with_between_bytes(mut self, enforced: bool) -> Self {
        self.between_bytes = enforced;
        self
    }
}

/// Whether a transport can put a request into TLS 1.3 early data (0-RTT).
///
/// # This is the floor, and it says less than it looks like
///
/// [`Self::Supported`] means only *"this transport is able to offer early
/// data"*. It never means a particular request went into early data, and it
/// never means one was accepted. In QUIC the acceptance verdict arrives
/// **after the response** — measured at 8.63 ms against a response at
/// 8.58 ms — so it is a future, not a
/// property of a transport, and nothing about it can live in a value that
/// [`Transport::capabilities`](crate::transport::Transport::capabilities)
/// determines once at construction.
///
/// # Why the default is `None` with unusual force
///
/// Every other capability here follows the rule that a default must not be
/// stronger than the truth, and the cost of breaking it is a buffered copy,
/// a lost optimisation, or — for `full_duplex` — a deadlock. This one costs
/// **replay exposure**: early data is data an attacker who captured it can
/// send again, at a moment of their choosing, to a server that will act on
/// it. So [`Capabilities::default()`] reports `None`, every transport that
/// ships today reports `None`, and a transport that forgets this field
/// reports `None`.
///
/// # Reporting `Supported` is not sufficient to put anything in early data
///
/// It is necessary and nothing more. The gate is the caller's, per request
/// — see [`crate::req::AllowEarlyData`] — and a transport that reports `Supported` must
/// still refuse to place a request the caller did not mark.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum EarlyDataSupport {
    /// This transport never offers early data. The conservative base, what
    /// [`Capabilities::default()`] returns, and the honest answer for every
    /// transport in this workspace except `hclient_native::H3`.
    #[default]
    None,
    /// This transport can offer early data for a request the caller has
    /// marked with [`crate::req::AllowEarlyData`]. See this enum's doc for the three
    /// things it still does not mean.
    Supported,
}

/// What the transport can do **in this process, right now**.
///
/// A runtime fact, not a `cfg!`: one wasm binary runs in both Chrome
/// (streaming request body available since 131) and Safari (not available).
///
/// # Two kinds of field
///
/// Every field here is one of two things, and reading them as one kind is
/// what makes a field like [`proxy`](Self::proxy) look dead when it is
/// not.
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
#[derive(Debug, Clone, Default)]
pub struct Capabilities {
    /// Whether a request body may be written as it is produced.
    ///
    /// Reported. `Client` does not gate on it — see this type's doc for
    /// why some fields do and some do not — which is why
    /// `hclient-urlsession` refuses a `Streaming` body with a typed error
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
    /// [`Transport::execute`](crate::transport::Transport::execute).
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
    /// **Reported, and it will never be a gate.** The
    /// setting it would guard is `Native::proxy`, which is on the
    /// transport that would answer the question, so there is nothing at
    /// the client level to refuse. That makes it unlike
    /// [`owns_cookie_jar`](Self::owns_cookie_jar), where the client owns
    /// the setting and the transport owns the conflict.
    ///
    /// It is not [`upgrade`](https://docs.rs/hclient-core)'s case either,
    /// the four-variant enum deleted for having no reader: both values
    /// here are reachable, and the reader is the caller — *will my
    /// requests go through a proxy* is a question a diagnostic asks and
    /// only this field answers.
    pub proxy: bool,
    /// Whether the transport keeps its own cookie jar: attaching `Cookie`
    /// to outgoing requests and processing `Set-Cookie` on incoming ones,
    /// without being asked.
    ///
    /// `true` for `hclient-fetch` — the browser does both, and `Cookie` is
    /// on that backend's `forbidden_request_headers`, so a client-side jar
    /// there would not merely be redundant, it would send every cookie
    /// twice and store every `Set-Cookie` twice. `false` for
    /// `hclient-native` and `hclient-wasi`.
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
    /// `true` for `hclient-fetch` — the browser has an HTTP cache and
    /// applies it inside `fetch()`. `false` for `hclient-native` — both
    /// stacks — and `hclient-wasi`, neither of which stores a response
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
    /// [`UnsupportedCapability`](crate::error::UnsupportedCapability) at `build()`, the same arm
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
    /// Whether the transport honours a per-request [`crate::req::RequireVersion`]
    /// demand: reads it, and either serves the request over that version
    /// or fails it with [`crate::error::VersionNotAvailable`] **before the head is
    /// written**.
    ///
    /// # It says "honours", not "chooses"
    ///
    /// A transport that only ever speaks one version reports `true` if it
    /// answers demands — `hclient_native::H3` does, by proceeding on
    /// `RequireVersion(HTTP_3)` and refusing everything else. Reporting
    /// `false` there would make `Client` refuse the one demand it
    /// trivially satisfies, which is the opposite of honest.
    ///
    /// `false` is for a transport that cannot answer at all:
    /// `hclient-fetch` and `hclient-wasi` neither select the version nor
    /// learn it (both also report `version_reported: false`), so a demand
    /// against either becomes an [`UnsupportedCapability`](crate::error::UnsupportedCapability) from `Client` —
    /// the same arm a `RedirectPolicy` against
    /// [`RedirectSupport::Internal`] takes.
    ///
    /// # Why this field exists
    ///
    /// The rule is that *a capability exists only if a caller decision
    /// turns on it* — `RedirectSupport` lost two variants to it.
    /// [`crate::req::RequireVersion`] is the decision this one answers, and it is the
    /// reason the demand and this
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
    /// [`Hooks`](crate::hooks::Hooks) impl is handed an
    /// [`Event`](crate::hooks::Event) and no capabilities:
    /// [`Head::version`](crate::hooks::Head::version) is `Some`
    /// exactly when this field is `true`. Two spellings of one fact, in
    /// the two places that can each be read on their own.
    ///
    /// **Read that as a rule about the events a transport emits, not
    /// about every transport.** A backend with no observability seam
    /// emits no [`Head`](crate::hooks::Head) at all, so the `Some` side
    /// has no producer there and the biconditional is vacuous rather than
    /// broken — `hclient-winhttp` reports `true` here, honestly (it reads
    /// the version out of WinHTTP's flags), and implements no
    /// [`Hooks`](crate::hooks::Hooks). What a portable hook may conclude
    /// is the contrapositive, which is the direction it actually needs:
    /// a [`Head`](crate::hooks::Head) carrying `None` came from a
    /// transport reporting `false`, so the absence is a fact about the
    /// backend and never a field somebody forgot to fill in.
    pub version_reported: bool,
    pub timeouts: TimeoutSupport,
    pub informational_1xx: bool,
    pub forbidden_request_headers: &'static [HeaderName],
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::UnsupportedCapability;

    /// **Every field is a gate or a report, and adding one without saying
    /// which is a compile error.**
    ///
    /// The distinction is [`Capabilities`]' own doc; this is what keeps it
    /// from going stale. Destructured with no `..` rest pattern —
    /// `#[non_exhaustive]` blocks that only from outside the crate, which
    /// is why the classification has to live here rather than in
    /// `hclient`, where the branches themselves are.
    ///
    /// The lists are asserted against each other rather than merely
    /// written: a field named in both, or in neither, fails a line.
    #[test]
    fn every_capability_is_a_gate_or_a_report() {
        let c = Capabilities::default();
        let Capabilities {
            // ── gates: a `Client` setting the transport can refuse ──
            //
            // Each of these has a branch in `hclient::caps::check_supported` and
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
    fn the_default_is_the_conservative_base() {
        // Every field, spelled out individually — not
        // `assert_eq!` on the whole struct via a derived `PartialEq`, which
        // `Capabilities` deliberately does not implement (it's
        // `#[non_exhaustive]` so its shape stays ours to change, and a
        // struct-wide `PartialEq` would be a public trait impl added purely
        // for a test's convenience).
        //
        // Destructured with no `..` rest pattern — `#[non_exhaustive]` only
        // blocks that from outside the crate, and this test lives inside it.
        // The field count is deliberately not written here: the destructure
        // below *is* the count, and unlike a number it cannot go stale.
        // Per-field assertions would NOT catch a new field: adding one and
        // setting it `true` in `none()` leaves them compiling and passing.
        // Only the exhaustive destructure does — omitting a field from the
        // pattern is a compile error naming it, because `..` is not there
        // to absorb it silently.
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
        } = Capabilities::default();
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
}
