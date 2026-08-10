use http::HeaderName;

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
    Internal,
    /// We set the policy.
    Configurable,
    /// We set the policy and see every hop.
    Inspectable,
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

/// What the transport can do **in this process, right now**.
///
/// A runtime fact, not a `cfg!`: one wasm binary runs in both Chrome
/// (streaming request body available since 131) and Safari (not available).
#[non_exhaustive]
#[derive(Debug, Clone)]
pub struct Capabilities {
    pub streaming_request_body: bool,
    pub full_duplex: bool,
    pub request_trailers: bool,
    pub response_trailers: bool,
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
    pub tls_config: TlsSupport,
    pub client_certs: bool,
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
    pub owns_cache: bool,
    pub version_select: bool,
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
            connect: true,
            first_byte: true,
            between_bytes: false,
        };
        assert!(t.connect && t.first_byte && !t.between_bytes);
    }
}
