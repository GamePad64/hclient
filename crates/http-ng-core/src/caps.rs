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
/// (`RedirectSupport`, `TlsSupport`, `UpgradeSupport` are all plain), and
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TlsSupport {
    None,
    ServerTrustCallbackOnly,
    Full,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UpgradeSupport {
    None,
    H1,
    ExtendedConnect,
    Both,
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
    pub tls_config: TlsSupport,
    pub client_certs: bool,
    pub proxy: bool,
    pub owns_cookie_jar: bool,
    pub owns_cache: bool,
    pub version_select: bool,
    pub version_reported: bool,
    pub timeouts: TimeoutSupport,
    pub informational_1xx: bool,
    pub upgrade: UpgradeSupport,
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
            upgrade: UpgradeSupport::None,
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
        // Every one of the 17 fields, spelled out individually — not
        // `assert_eq!` on the whole struct via a derived `PartialEq`, which
        // `Capabilities` deliberately does not implement (it's
        // `#[non_exhaustive]` so its shape stays ours to change, and a
        // struct-wide `PartialEq` would be a public trait impl added purely
        // for a test's convenience).
        //
        // Destructured with no `..` rest pattern — `#[non_exhaustive]` only
        // blocks that from outside the crate, and this test lives inside it.
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
            tls_config,
            client_certs,
            proxy,
            owns_cookie_jar,
            owns_cache,
            version_select,
            version_reported,
            timeouts,
            informational_1xx,
            upgrade,
            forbidden_request_headers,
        } = Capabilities::none();
        assert!(!streaming_request_body);
        assert!(!full_duplex);
        assert!(!request_trailers);
        assert!(!response_trailers);
        assert_eq!(redirects, RedirectSupport::None);
        assert_eq!(cancel_on_drop, CancelSupport::None);
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
        assert_eq!(upgrade, UpgradeSupport::None);
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
