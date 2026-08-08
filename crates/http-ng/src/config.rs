// `Timeouts` is defined in `http-ng-core` (Task 8): transports read it from
// `http::Extensions`, and they don't depend on `http-ng`.
pub use http_ng_core::Timeouts;
use http_ng_core::{Capabilities, Error, ErrorKind, RedirectSupport, UnsupportedCapability};
use http_ng_proto::redirect::RedirectPolicy;

#[derive(Debug, Clone, Default)]
pub struct Config {
    pub timeouts: Timeouts,
    /// `Option::None` here is "the caller never asked for a redirect
    /// policy" — distinct from `Some(RedirectPolicy::None)`, which is the
    /// caller explicitly asking not to follow and to be handed the 3xx.
    /// The distinction is load-bearing: the first is accepted by every
    /// backend, the second is refused by one that follows redirects
    /// internally, because it cannot be honoured there.
    ///
    /// A third thing again is `Some(RedirectPolicy::Limited(0))` — follow
    /// zero hops, so the first 3xx is `ErrorKind::Redirect`.
    ///
    /// An `Option` rather than a bare `RedirectPolicy` because
    /// `check_supported` has to tell those two apart:
    /// `RedirectPolicy::default()` is `Limited(10)`, so a check that
    /// fired on any policy at all would reject every client built against
    /// a backend that follows redirects internally, including one whose
    /// author never mentioned redirects. Same idiom as `Timeouts`, whose
    /// fields are `Option<Duration>` for exactly the same reason.
    ///
    /// Every read site takes `unwrap_or_default()`, so an unconfigured
    /// client still follows up to `RedirectPolicy::default()`'s ten hops —
    /// this field's type changed, no behavior did.
    pub redirect: Option<RedirectPolicy>,
    pub base_url: Option<http::Uri>,
}

/// The base URL is unfit to resolve this request against.
///
/// `pub` and re-exported by the facade, not for looks: the caller must be
/// able to tell this apart from any other `ErrorKind::Other` via
/// `Error::source().downcast_ref::<InvalidBaseUrl>()` — the same trick
/// `mock::QueueEmpty` uses. Both fields are public so the diagnostic names
/// the specific pair, not just the fact.
///
/// `requested` is a `String`, not an `http::Uri`: resolution works on the
/// STRING before parsing (see `effective_uri`), and exactly the references
/// the base exists for aren't expressible as `http::Uri` at all.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("cannot resolve `{requested}` against base URL `{base}` (a base URL must be absolute)")]
pub struct InvalidBaseUrl {
    pub base: http::Uri,
    pub requested: String,
}

/// The URI the request will actually go out on: `url`, resolved against
/// `base` if a base is set.
///
/// The rule is RFC 3986 §5, the exact same one `redirect::decide` uses to
/// resolve `Location:`; the shared implementation is
/// `http_ng_proto::uri::resolve_reference`. One client shouldn't understand
/// `/x` two different ways depending on whether the server sent it or the
/// caller did.
///
/// **Works on the string, not on `http::Uri`, and that's forced.**
/// `http::Uri` can't represent a path-relative reference at all:
/// `"v1/things"`, `"search?q=1"`, and `""` are all `InvalidUri` (measured).
/// And these are exactly the forms the base exists for: a reference with a
/// leading `/` REPLACES the base's path entirely per RFC, so if already-
/// parsed `Uri`s were resolved, the base's path could never affect
/// anything and the setting would amount to "set the origin", not "set the
/// base URL".
///
/// Without a base, it's an ordinary parse: there's nowhere to invent a
/// scheme and authority for the caller from, and a transport that needs
/// absolute-form will reject a relative URI itself, with its own type
/// (`WasiHttp` → `scheme_of`).
///
/// Before this round the function didn't exist at all, and `Config::base_url`
/// was written by a setter and read by nothing — the third "saved and
/// ignored" case in the project, and the second in this same struct after
/// `timeouts` (B1).
pub(crate) fn effective_uri(base: Option<&http::Uri>, url: &str) -> Result<http::Uri, Error> {
    let Some(base) = base else {
        return url
            .parse::<http::Uri>()
            .map_err(|e| Error::new(ErrorKind::Other, e));
    };
    http_ng_proto::uri::resolve_reference(base, url).ok_or_else(|| {
        Error::new(
            ErrorKind::Other,
            InvalidBaseUrl {
                base: base.clone(),
                requested: url.to_owned(),
            },
        )
    })
}

/// "Request-first, client-fallback", field by field.
///
/// reqwest can't do this (issue #2641 is unimplemented), which forces
/// `act-cli` to build a separate `reqwest::Client` for every component
/// call.
pub fn effective_timeouts(req: &http::Extensions, client: &Timeouts) -> Timeouts {
    match req.get::<Timeouts>() {
        None => *client,
        Some(o) => Timeouts {
            connect: o.connect.or(client.connect),
            first_byte: o.first_byte.or(client.first_byte),
            between_bytes: o.between_bytes.or(client.between_bytes),
        },
    }
}

/// The same "request-first, client-fallback" rule for the redirect policy.
///
/// Exists because the limit really does vary per request in the consumer
/// this library is being built for: `act`'s `http-client` component
/// computes `if args.follow_redirects { 10 } else { 0 }` on every call, and
/// `follow_redirects` is a per-request argument. Without a per-request
/// override that shape can only be expressed by building a new `Client` per
/// request — the exact cost `RequestBuilder::timeouts` already exists to
/// avoid (reqwest #2641).
///
/// **Whole-value, not field-by-field like `effective_timeouts`.**
/// `RedirectPolicy` is a plain value, not an `Option`, so
/// there is no "field the request left unset" to fall through to the
/// client; a request that set a policy replaces the client's entirely. If
/// `RedirectPolicy` ever grows a second field, this is the function that
/// has to decide whether that stays true.
///
/// The channel is `http::Extensions`, the same one `timeouts` travels in —
/// but the reader is `Client::execute` itself, not the transport: no
/// transport reads a `RedirectPolicy`, and the redirect stage is the only
/// consumer there has ever been.
///
/// `pub(crate)`, unlike its `effective_timeouts` sibling above: same
/// reasoning as `check_redirect_supported` — nothing outside this crate has
/// a use for it, and the facade's export list is already over-wide
/// (finding §6.7 of the branch's final review).
pub(crate) fn effective_redirect(
    req: &http::Extensions,
    client: &Option<RedirectPolicy>,
) -> Option<RedirectPolicy> {
    req.get::<RedirectPolicy>().copied().or(*client)
}

/// Called from `ClientBuilder::build()`. Not a single silent no-op.
///
/// Destructures `cfg` without a `..`-remainder — the same recipe as
/// `Capabilities::none_is_the_conservative_base` in `http-ng-core`: a new
/// field on `Config` becomes a compile error naming it, instead of being
/// silently skipped (same for `Timeouts` fields, in
/// `check_timeouts_supported`). `redirect` and `base_url` are deliberately
/// not checked for support today — the `_` explicitly records that
/// decision rather than forgetting the field.
///
/// "Not checked **for support**" is about `Capabilities`, and only about
/// that. Both fields ARE applied: `base_url` in `effective_uri` on every
/// request, `redirect` by the redirect stage in `Client::execute`. This
/// note exists because the earlier wording read easily as "nobody handles
/// these fields", and `base_url` used to be exactly that — saved and never
/// applied.
///
/// `redirect` used to be `_` here too, under a note saying it "will stop
/// being harmless the moment a backend with `RedirectSupport::Internal`
/// shows up". **That moment arrived.** `http-ng-fetch` reports
/// `redirects: RedirectSupport::Internal`: the browser follows the chain
/// itself, `Client`'s redirect stage never sees a single 3xx, and a
/// configured `RedirectPolicy` would be precisely the silent no-op this
/// module was built against — the module whose own first line says "Not a
/// single silent no-op" would have contained exactly one. So the field is
/// checked now, by `check_redirect_supported` below.
///
/// `base_url` remains deliberately unchecked **for support** — the `_`
/// records that decision rather than forgetting the field.
pub fn check_supported(
    cfg: &Config,
    caps: &Capabilities,
    backend: &'static str,
) -> Result<(), UnsupportedCapability> {
    let Config {
        timeouts,
        redirect,
        base_url: _,
    } = cfg;
    check_timeouts_supported(timeouts, caps, backend)?;
    check_redirect_supported(redirect, caps, backend)
}

/// A `RedirectPolicy` the caller actually asked for, against a backend
/// that follows redirects itself, is an error — not a setting that quietly
/// does nothing.
///
/// **Only `Internal` is rejected**, and that is the whole point of the
/// variant existing separately from `None`. Under `Transparent` (what
/// `wasi:http` does) the 3xx reaches us and `Client`'s own stage honours
/// the policy in full; under `None` the backend has simply said nothing
/// about redirects, which `Capabilities::none()` returns for every field it
/// hasn't been told about — neither is a reason to reject a policy the
/// stage can carry out.
///
/// **Only `Some` is rejected**, and that is why `Config::redirect` is an
/// `Option`. `RedirectPolicy::default()` is `Limited(10)`; a check
/// reading a bare `RedirectPolicy` could not distinguish "follow up to ten
/// hops, and I mean it" from "I never mentioned redirects", so it would
/// reject every browser client ever built — including `Client::new()`'s
/// own, whose `.expect(...)` in `client.rs` rests on this exact line not
/// firing. That is not a fix, it is an unusable backend. The idiom is
/// `check_timeouts_supported`'s `connect.is_some()`, one function down:
/// "the caller asked for this".
///
/// `pub(crate)`, like `check_timeouts_supported` and for the same reason:
/// `Client::execute` needs it over a merged value rather than over a whole
/// `Config`, and the `http-ng` facade already exports more plumbing than it
/// should (finding §6.7 of the branch's final review).
pub(crate) fn check_redirect_supported(
    redirect: &Option<RedirectPolicy>,
    caps: &Capabilities,
    backend: &'static str,
) -> Result<(), UnsupportedCapability> {
    if redirect.is_some() && caps.redirects == RedirectSupport::Internal {
        return Err(UnsupportedCapability {
            what: "redirect_policy",
            backend,
        });
    }
    Ok(())
}

/// The same check, but over a single `Timeouts` rather than the whole
/// `Config` — because `Client::execute` checks the **merged** result of
/// `effective_timeouts`, not the client's configuration (B1/M3 of the
/// branch's final review: before it, per-request timeouts weren't checked
/// at all, and client-level ones were checked here but never reached the
/// transport). One shared body, not a second copy of the `checks` array:
/// these two checks must not drift apart, and two phase lists will drift
/// the moment a new phase shows up.
///
/// `pub(crate)`, unlike `check_supported`: the `http-ng` facade already
/// exports more plumbing than it should (finding §6.7 of the same review),
/// and there's no reason to grow that debt with a new name.
pub(crate) fn check_timeouts_supported(
    t: &Timeouts,
    caps: &Capabilities,
    backend: &'static str,
) -> Result<(), UnsupportedCapability> {
    let Timeouts {
        connect,
        first_byte,
        between_bytes,
    } = t;
    let checks = [
        (connect.is_some(), caps.timeouts.connect, "connect_timeout"),
        (
            first_byte.is_some(),
            caps.timeouts.first_byte,
            "first_byte_timeout",
        ),
        (
            between_bytes.is_some(),
            caps.timeouts.between_bytes,
            "between_bytes_timeout",
        ),
    ];
    for (requested, supported, what) in checks {
        if requested && !supported {
            return Err(UnsupportedCapability { what, backend });
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use http_ng_core::{Capabilities, TimeoutSupport};
    use std::time::Duration;

    fn secs(n: u64) -> Option<Duration> {
        Some(Duration::from_secs(n))
    }

    /// `Capabilities::none()` with only `redirects` changed — everything
    /// else stays off, so nothing but the field under test can make
    /// `check_supported` return `Err` and pass one of these tests for the
    /// wrong reason.
    fn caps_with_redirects(r: RedirectSupport) -> Capabilities {
        let mut c = Capabilities::none();
        c.redirects = r;
        c
    }

    #[test]
    fn request_overrides_client_field_by_field() {
        let client = Timeouts {
            connect: secs(1),
            first_byte: secs(2),
            between_bytes: secs(3),
        };
        let mut ext = http::Extensions::new();
        ext.insert(Timeouts {
            connect: secs(9),
            ..Default::default()
        });
        let eff = effective_timeouts(&ext, &client);
        assert_eq!(eff.connect, secs(9), "request overrides");
        assert_eq!(eff.first_byte, secs(2), "the rest falls back to the client");
        assert_eq!(eff.between_bytes, secs(3));
    }

    #[test]
    fn client_config_used_when_request_says_nothing() {
        let client = Timeouts {
            connect: secs(1),
            ..Default::default()
        };
        let eff = effective_timeouts(&http::Extensions::new(), &client);
        assert_eq!(eff.connect, secs(1));
    }

    #[test]
    fn unsupported_timeout_is_an_error_not_a_silent_noop() {
        let cfg = Config {
            timeouts: Timeouts {
                between_bytes: secs(5),
                ..Default::default()
            },
            ..Default::default()
        };
        let mut caps = Capabilities::none();
        caps.timeouts = TimeoutSupport {
            connect: true,
            first_byte: true,
            between_bytes: false,
        };
        let err = check_supported(&cfg, &caps, "wasi:http").unwrap_err();
        assert_eq!(err.what, "between_bytes_timeout");
        assert_eq!(err.backend, "wasi:http");
    }

    #[test]
    fn supported_config_passes() {
        let cfg = Config {
            timeouts: Timeouts {
                connect: secs(1),
                ..Default::default()
            },
            ..Default::default()
        };
        let mut caps = Capabilities::none();
        caps.timeouts = TimeoutSupport {
            connect: true,
            first_byte: false,
            between_bytes: false,
        };
        assert!(check_supported(&cfg, &caps, "wasi:http").is_ok());
    }

    // ── extra checks: field-by-field, not all-or-nothing ─────────────
    //
    // `request_overrides_client_field_by_field` overrides `connect` and
    // checks that `first_byte`/`between_bytes` fall back to the client —
    // that already tells "field by field" apart from "all or nothing" (a
    // naive "if extensions has a Timeouts — take it whole" implementation
    // would return `None` here, not `client.first_byte`). The test below
    // hits a different field (`first_byte`), so the same property isn't
    // an artifact of `connect` being the struct's first field.
    #[test]
    fn request_overrides_first_byte_only_leaves_others_from_client() {
        let client = Timeouts {
            connect: secs(1),
            first_byte: secs(2),
            between_bytes: secs(3),
        };
        let mut ext = http::Extensions::new();
        ext.insert(Timeouts {
            first_byte: secs(9),
            ..Default::default()
        });
        let eff = effective_timeouts(&ext, &client);
        assert_eq!(
            eff.connect,
            secs(1),
            "not overridden by request — take the client's"
        );
        assert_eq!(eff.first_byte, secs(9), "request overrides");
        assert_eq!(
            eff.between_bytes,
            secs(3),
            "not overridden by request — take the client's"
        );
    }

    // ── extra checks: check_supported names the RIGHT phase ──────────
    //
    // `unsupported_timeout_is_an_error_not_a_silent_noop` covers only
    // `between_bytes`. Since `checks` in `check_supported` is an array of
    // three independent triples, a small refactor slip (say, a copy-pasted
    // index) could return the right error for one phase and a wrong one
    // (wrong field in `what`, or the wrong phase triggering the error) for
    // the other two, unnoticed. Check all three phases separately: each
    // one requests only that field, and only that same field is
    // unsupported in `Capabilities`.
    #[test]
    fn unsupported_connect_is_named_connect_not_another_phase() {
        let cfg = Config {
            timeouts: Timeouts {
                connect: secs(1),
                ..Default::default()
            },
            ..Default::default()
        };
        let mut caps = Capabilities::none();
        caps.timeouts = TimeoutSupport {
            connect: false,
            first_byte: true,
            between_bytes: true,
        };
        let err = check_supported(&cfg, &caps, "wasi:http").unwrap_err();
        assert_eq!(err.what, "connect_timeout");
        assert_eq!(err.backend, "wasi:http");
    }

    #[test]
    fn unsupported_first_byte_is_named_first_byte_not_another_phase() {
        let cfg = Config {
            timeouts: Timeouts {
                first_byte: secs(1),
                ..Default::default()
            },
            ..Default::default()
        };
        let mut caps = Capabilities::none();
        caps.timeouts = TimeoutSupport {
            connect: true,
            first_byte: false,
            between_bytes: true,
        };
        let err = check_supported(&cfg, &caps, "wasi:http").unwrap_err();
        assert_eq!(err.what, "first_byte_timeout");
        assert_eq!(err.backend, "wasi:http");
    }

    #[test]
    fn unsupported_between_bytes_is_named_between_bytes_not_another_phase() {
        let cfg = Config {
            timeouts: Timeouts {
                between_bytes: secs(1),
                ..Default::default()
            },
            ..Default::default()
        };
        let mut caps = Capabilities::none();
        caps.timeouts = TimeoutSupport {
            connect: true,
            first_byte: true,
            between_bytes: false,
        };
        let err = check_supported(&cfg, &caps, "wasi:http").unwrap_err();
        assert_eq!(err.what, "between_bytes_timeout");
        assert_eq!(err.backend, "wasi:http");
    }

    // ── the redirect check: `Internal` backends, and only them ───────
    //
    // The prediction the old `redirect: _` in `check_supported` was
    // written against came true when `http-ng-fetch` started reporting
    // `RedirectSupport::Internal`. These four tests are the check that
    // replaced it, and the middle two matter as much as the first: a check
    // that rejected any configured policy, or one that rejected under any
    // non-`Configurable` variant, would break the browser backend or the
    // WASI one respectively while still passing the first test.

    #[test]
    fn configured_redirect_policy_against_an_internal_backend_is_an_error() {
        let cfg = Config {
            redirect: Some(RedirectPolicy::Limited(5)),
            ..Default::default()
        };
        let err = check_supported(
            &cfg,
            &caps_with_redirects(RedirectSupport::Internal),
            "fetch",
        )
        .unwrap_err();
        assert_eq!(err.what, "redirect_policy");
        assert_eq!(err.backend, "fetch");
    }

    /// The test that stops the fix from bricking the browser backend.
    ///
    /// `Config::default()` is what `Client::new()` builds on
    /// `wasm32-unknown-unknown`, and `Fetch` is an `Internal` backend — if
    /// "a policy is present" were read off a bare `RedirectPolicy` rather
    /// than an `Option`, `RedirectPolicy::default()` would make this `Err`
    /// and every browser client would fail to build, including one whose
    /// author never mentioned redirects.
    #[test]
    fn an_unconfigured_redirect_against_an_internal_backend_is_fine() {
        let cfg = Config::default();
        assert!(cfg.redirect.is_none(), "the premise of the whole check");
        assert!(
            check_supported(
                &cfg,
                &caps_with_redirects(RedirectSupport::Internal),
                "fetch"
            )
            .is_ok()
        );
    }

    /// `Transparent` specifically, not `None`: the two are different claims
    /// (`Capabilities::none()` returns `None` for a backend that said
    /// nothing, while `Transparent` is `wasi:http` positively stating that
    /// the 3xx arrives as-is), and a check written as "reject unless
    /// `Configurable`" would pass a `None`-only test while breaking the one
    /// real non-browser backend that follows redirects through `Client`'s
    /// own stage.
    #[test]
    fn configured_redirect_policy_against_a_transparent_backend_is_fine() {
        let cfg = Config {
            redirect: Some(RedirectPolicy::Limited(5)),
            ..Default::default()
        };
        assert!(
            check_supported(
                &cfg,
                &caps_with_redirects(RedirectSupport::Transparent),
                "wasi:http"
            )
            .is_ok()
        );
    }

    /// The merge is whole-value, and the request wins.
    ///
    /// Not "the request's limit is used" alone — that would also pass if
    /// the function ignored its `client` argument entirely — so both
    /// directions are checked here, and the client-only case below.
    #[test]
    fn request_redirect_policy_replaces_the_clients() {
        let client = Some(RedirectPolicy::Limited(3));
        let mut ext = http::Extensions::new();
        ext.insert(RedirectPolicy::Limited(7));
        assert_eq!(
            effective_redirect(&ext, &client),
            Some(RedirectPolicy::Limited(7)),
            "the request's policy wins"
        );
        assert_eq!(
            effective_redirect(&http::Extensions::new(), &client),
            Some(RedirectPolicy::Limited(3)),
            "with nothing on the request, the client's stands"
        );
        assert_eq!(
            effective_redirect(&http::Extensions::new(), &None),
            None,
            "neither side configured anything, and that stays distinguishable"
        );
    }

    /// A per-request policy against an `Internal` backend must be rejected
    /// on the same footing as a client-level one — checking only the
    /// client's `Config` would leave this path silently unchecked, which is
    /// the exact defect the whole module exists against.
    #[test]
    fn a_request_only_redirect_policy_is_still_checked_against_internal() {
        let mut ext = http::Extensions::new();
        ext.insert(RedirectPolicy::Limited(0));
        let merged = effective_redirect(&ext, &None);
        let err = check_redirect_supported(
            &merged,
            &caps_with_redirects(RedirectSupport::Internal),
            "fetch",
        )
        .unwrap_err();
        assert_eq!(err.what, "redirect_policy");
    }
}
