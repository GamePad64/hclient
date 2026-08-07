// `Timeouts` is defined in `http-ng-core` (Task 8): transports read it from
// `http::Extensions`, and they don't depend on `http-ng`.
pub use http_ng_core::Timeouts;
use http_ng_core::{Capabilities, Error, ErrorKind, UnsupportedCapability};
use http_ng_proto::redirect::RedirectPolicy;

#[derive(Debug, Clone, Default)]
pub struct Config {
    pub timeouts: Timeouts,
    pub redirect: RedirectPolicy,
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
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InvalidBaseUrl {
    pub base: http::Uri,
    pub requested: String,
}

impl std::fmt::Display for InvalidBaseUrl {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "cannot resolve `{}` against base URL `{}` (a base URL must be absolute)",
            self.requested, self.base
        )
    }
}
impl std::error::Error for InvalidBaseUrl {}

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
/// `redirect: _` will stop being harmless the moment a backend with
/// `RedirectSupport::Internal` shows up: it follows redirects itself, the
/// `Client`'s stage never sees a single 3xx, and the configured
/// `RedirectPolicy` becomes exactly the silent no-op this whole module was
/// built against. No existing backend is `Internal` (`WasiHttp` is
/// `Transparent`, see `RedirectSupport`), so there's nothing to check
/// today; the trigger is vertical 3's browser `fetch`.
pub fn check_supported(
    cfg: &Config,
    caps: &Capabilities,
    backend: &'static str,
) -> Result<(), UnsupportedCapability> {
    let Config {
        timeouts,
        redirect: _,
        base_url: _,
    } = cfg;
    check_timeouts_supported(timeouts, caps, backend)
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
}
