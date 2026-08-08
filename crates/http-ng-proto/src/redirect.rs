//! The decision to follow a redirect. A pure function: no I/O, no time.

use http::{HeaderName, HeaderValue, Method, StatusCode, Uri};

/// Headers stripped when moving to a different origin.
pub const SENSITIVE_HEADERS: [HeaderName; 3] = [
    http::header::AUTHORIZATION,
    http::header::COOKIE,
    http::header::PROXY_AUTHORIZATION,
];

/// Whether, and how far, to follow a redirect chain.
///
/// Two different intents, kept apart deliberately: [`Self::None`] returns
/// the 3xx to the caller, [`Self::Limited`] follows and errors on
/// exceeding. `reqwest` draws the same line as `Policy::none()` versus
/// `Policy::limited(0)`.
///
/// This was a `struct { limit: u8 }` until the Task 10 acceptance ported a
/// live consumer onto it and found the first intent inexpressible — the
/// consumer's `follow_redirects: false` forwards the 302 upward, and
/// `limit: 0` turned that answer into an error.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RedirectPolicy {
    /// Do not follow. A 3xx reaches the caller as an ordinary response, its
    /// `Location` header intact, for the caller to inspect or forward.
    ///
    /// This is what `wasi-fetch`'s `redirect_limit(0)` did
    /// (`request.rs:135`: `if redirect_limit > 0 && status.is_redirection()`
    /// skips the whole redirect branch), and what the live consumer
    /// `act/components/http-client` means by `follow_redirects: false`. It
    /// was inexpressible here until this type became an enum.
    None,
    /// Follow up to this many hops; exceeding it is `TooManyRedirects`.
    ///
    /// `Limited(0)` therefore means "follow zero hops" — the first 3xx with
    /// a `Location` is an error. That is deliberately NOT the same as
    /// [`RedirectPolicy::None`], and the distinction is the entire reason
    /// this is an enum: folding the two into a single `limit: 0` put a
    /// discontinuity inside one field, where `0` returned the response and
    /// `1` errored on exceeding. `reqwest` keeps them apart the same way,
    /// as `Policy::none()` and `Policy::limited(0)`.
    Limited(u8),
}

impl Default for RedirectPolicy {
    /// Ten hops, matching what the struct form defaulted to.
    fn default() -> Self {
        Self::Limited(10)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Follow {
    /// Where to go. May carry userinfo sent by the server
    /// (`https://user:pass@host/`) — it's not part of the origin per RFC
    /// 6454 and so doesn't factor into `strip_sensitive`, but it's foreign
    /// input: don't silently promote it into trusted credentials further
    /// down the stack.
    pub uri: Uri,
    pub method: Method,
    /// Strip `SENSITIVE_HEADERS`: the host or scheme changed.
    pub strip_sensitive: bool,
    /// The method was downgraded to GET — the body must not be sent.
    pub drop_body: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RedirectAction {
    /// Not a redirect, or a redirect with no `Location` — return the response as-is.
    Stop,
    Follow(Follow),
    TooManyRedirects,
    InvalidLocation,
}

/// Port with the scheme's default substituted in.
///
/// Neither side of the comparison is normalized any more. `current`
/// arrives from the caller as written, and since `url` was removed the
/// resolved target keeps whatever the `Location` header said — `url` used
/// to strip a default port on serialization, so this function used to
/// correct an asymmetry and now covers both sides. Without it,
/// `https://a:443/` → `https://a/` reads as an origin change and strips
/// Authorization on every hop.
fn port_of(uri: &Uri) -> Option<u16> {
    uri.port_u16().or_else(|| match uri.scheme_str() {
        Some("https") => Some(443),
        Some("http") => Some(80),
        _ => None,
    })
}

pub fn decide(
    policy: &RedirectPolicy,
    hops: u8,
    current: &Uri,
    method: &Method,
    status: StatusCode,
    location: Option<&[u8]>,
) -> RedirectAction {
    // IMPORTANT: not `status.is_redirection()`. 300 Multiple Choices
    // requires user choice, 304 Not Modified is the response to a
    // conditional request, 305 Use Proxy hasn't been followed since 2014,
    // 306 is reserved.
    if !matches!(status.as_u16(), 301 | 302 | 303 | 307 | 308) {
        return RedirectAction::Stop;
    }
    let Some(location) = location else {
        return RedirectAction::Stop;
    };
    let limit = match policy {
        // "Do not follow" is a `Stop`, not an error: the 3xx is the caller's
        // answer, not a failure to reach one.
        RedirectPolicy::None => return RedirectAction::Stop,
        RedirectPolicy::Limited(n) => *n,
    };
    if hops >= limit {
        return RedirectAction::TooManyRedirects;
    }

    // Validate as a header value: rejects C0 control bytes and DEL, i.e.
    // closes CR/LF injection through Location. But NOT via `to_str()` —
    // that also rejects any byte >= 0x80, and raw non-ASCII (a
    // non-percent-encoded path, an IDN host) is formally invalid yet shows
    // up in practice; reqwest, through tower_http, follows it
    // (`str::from_utf8` on the raw bytes, with no ASCII restriction).
    let Ok(header) = HeaderValue::from_bytes(location) else {
        return RedirectAction::InvalidLocation;
    };
    let Ok(location) = core::str::from_utf8(header.as_bytes()) else {
        return RedirectAction::InvalidLocation;
    };
    // Shared with `ClientBuilder::base_url`'s RFC 3986 §5 implementation —
    // see `crate::uri`'s doc comment: this client has exactly one rule for
    // resolving a relative reference, regardless of whether the server sent
    // it in `Location:` or the caller sent it in `client.get(..)`.
    let Ok(uri) = crate::uri::resolve_reference(current, location) else {
        return RedirectAction::InvalidLocation;
    };

    // The host is case-insensitive (RFC 3986 §6.2.2.1) and nothing
    // lower-cases it any more: `url` used to, on the target only, so
    // `https://A.test/` + `Location: y` compared `a.test` against `A.test`
    // and stripped Authorization from a request that never left the host
    // it started on. `scheme_str` needs no such care — `http::Uri`
    // lower-cases the scheme itself.
    let same_host = match (uri.host(), current.host()) {
        (Some(target), Some(from)) => target.eq_ignore_ascii_case(from),
        (target, from) => target == from,
    };
    let cross_origin =
        !same_host || uri.scheme_str() != current.scheme_str() || port_of(&uri) != port_of(current);

    // 303 is always GET (except HEAD). Browsers and reqwest downgrade
    // 301/302 with POST to GET; diverging from 303 here would be
    // inconsistent.
    let downgrade = match status.as_u16() {
        303 => *method != Method::HEAD,
        301 | 302 => *method == Method::POST,
        _ => false,
    };
    let new_method = if downgrade {
        Method::GET
    } else {
        method.clone()
    };

    RedirectAction::Follow(Follow {
        uri,
        method: new_method,
        strip_sensitive: cross_origin,
        drop_body: downgrade,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use http::{Method, StatusCode, Uri};

    fn p() -> RedirectPolicy {
        RedirectPolicy::Limited(10)
    }
    fn u(s: &str) -> Uri {
        s.parse().unwrap()
    }

    fn go(status: u16, from: &str, to: &str, m: Method) -> RedirectAction {
        decide(
            &p(),
            0,
            &u(from),
            &m,
            StatusCode::from_u16(status).unwrap(),
            Some(to.as_bytes()),
        )
    }

    #[test]
    fn does_not_follow_300_304_305() {
        for s in [300u16, 304, 305, 306] {
            assert!(
                matches!(
                    go(s, "https://a/", "https://b/", Method::GET),
                    RedirectAction::Stop
                ),
                "status {s} must not be followed"
            );
        }
    }

    #[test]
    fn follows_the_five_real_redirects() {
        for s in [301u16, 302, 303, 307, 308] {
            assert!(
                matches!(
                    go(s, "https://a/", "https://a/x", Method::GET),
                    RedirectAction::Follow(_)
                ),
                "status {s}"
            );
        }
    }

    #[test]
    fn strips_sensitive_on_host_change() {
        let RedirectAction::Follow(f) = go(302, "https://a/", "https://b/", Method::GET) else {
            panic!()
        };
        assert!(f.strip_sensitive);
    }

    #[test]
    fn strips_sensitive_on_scheme_change_same_host() {
        let RedirectAction::Follow(f) = go(302, "https://a/", "http://a/", Method::GET) else {
            panic!()
        };
        assert!(f.strip_sensitive, "downgrade https->http must strip");
    }

    #[test]
    fn keeps_sensitive_on_same_origin() {
        let RedirectAction::Follow(f) = go(302, "https://a/one", "https://a/two", Method::GET)
        else {
            panic!()
        };
        assert!(!f.strip_sensitive);
    }

    #[test]
    fn post_downgrades_to_get_on_301_302_303() {
        for s in [301u16, 302, 303] {
            let RedirectAction::Follow(f) = go(s, "https://a/", "https://a/x", Method::POST) else {
                panic!("status {s}")
            };
            assert_eq!(f.method, Method::GET, "status {s}");
            assert!(f.drop_body, "status {s}");
        }
    }

    #[test]
    fn post_is_preserved_on_307_308() {
        for s in [307u16, 308] {
            let RedirectAction::Follow(f) = go(s, "https://a/", "https://a/x", Method::POST) else {
                panic!()
            };
            assert_eq!(f.method, Method::POST);
            assert!(!f.drop_body);
        }
    }

    #[test]
    fn head_stays_head_on_303() {
        let RedirectAction::Follow(f) = go(303, "https://a/", "https://a/x", Method::HEAD) else {
            panic!()
        };
        assert_eq!(f.method, Method::HEAD);
    }

    #[test]
    fn resolves_relative_location() {
        let RedirectAction::Follow(f) = go(302, "https://a/one/two", "../three", Method::GET)
        else {
            panic!()
        };
        assert_eq!(f.uri, u("https://a/three"));
    }

    #[test]
    fn missing_location_stops() {
        let r = decide(
            &p(),
            0,
            &u("https://a/"),
            &Method::GET,
            StatusCode::FOUND,
            None,
        );
        assert!(matches!(r, RedirectAction::Stop));
    }

    #[test]
    fn limit_is_enforced() {
        let r = decide(
            &RedirectPolicy::Limited(2),
            2,
            &u("https://a/"),
            &Method::GET,
            StatusCode::FOUND,
            Some(b"https://a/x"),
        );
        assert!(matches!(r, RedirectAction::TooManyRedirects));
    }

    /// `None` is the whole reason this type is an enum, and until this test
    /// the crate that OWNS it never constructed the variant: collapsing
    /// `None` back into `Limited(0)` left all 92 of this crate's tests
    /// green, and was caught only by a test in `http-ng` that reaches the
    /// semantics through `examples/portable.rs` — an example, the artifact
    /// most likely to be rewritten or trimmed.
    ///
    /// A 302 WITH a `Location` is the case that discriminates: without one,
    /// `decide` returns `Stop` for every policy, so the assertion would hold
    /// for the wrong reason.
    #[test]
    fn none_stops_without_following_and_does_not_report_too_many() {
        let r = decide(
            &RedirectPolicy::None,
            0,
            &u("https://a/"),
            &Method::GET,
            StatusCode::FOUND,
            Some(b"https://a/x"),
        );
        assert!(
            matches!(r, RedirectAction::Stop),
            "`None` must hand the 3xx back, not follow it and not error: {r:?}"
        );
    }

    /// The other side of the same distinction, in the owning crate:
    /// `Limited(0)` follows zero hops, so the first redirect IS an error.
    /// Together with the test above, neither variant's behaviour can be
    /// satisfied by the other's code path.
    #[test]
    fn limited_zero_errors_where_none_would_stop() {
        let r = decide(
            &RedirectPolicy::Limited(0),
            0,
            &u("https://a/"),
            &Method::GET,
            StatusCode::FOUND,
            Some(b"https://a/x"),
        );
        assert!(
            matches!(r, RedirectAction::TooManyRedirects),
            "`Limited(0)` must error where `None` stops: {r:?}"
        );
    }

    /// QUERY is safe and idempotent, so 301 and 302 preserve it — method
    /// AND body — exactly as they do for PUT or PATCH. The rewrite-to-GET
    /// in `decide` applies to POST alone, which is what RFC 9110 §15.4.2
    /// and §15.4.3 describe as the historical behaviour being codified.
    ///
    /// This holds today only because QUERY is not POST. It would be easy to
    /// "fix" into corruption by anyone who groups QUERY with POST for
    /// having a body — which is why it is pinned rather than left implied.
    /// Dropping the body of a QUERY does not degrade the request, it
    /// changes what was asked.
    #[test]
    fn query_survives_301_and_302_with_its_body() {
        for status in [301u16, 302] {
            let r = decide(
                &p(),
                0,
                &u("https://a/search"),
                &Method::QUERY,
                StatusCode::from_u16(status).unwrap(),
                Some(b"https://a/search2"),
            );
            let RedirectAction::Follow(f) = r else {
                panic!("expected a Follow for status {status}, got {r:?}");
            };
            assert_eq!(
                f.method,
                Method::QUERY,
                "QUERY is safe; only POST is rewritten to GET on {status}"
            );
            assert!(
                !f.drop_body,
                "the body IS the query on {status} — dropping it changes the question, \
                 it does not merely weaken the request"
            );
        }
    }

    /// 303 is the exception, and QUERY claims none: "retrieve the result
    /// with GET" is what the status means, so the method becomes GET and
    /// the body goes. The pair with the test above is the point — one
    /// without the other would let a blanket rule pass for half the wrong
    /// reason.
    #[test]
    fn query_is_still_downgraded_by_303_like_every_other_method() {
        let r = decide(
            &p(),
            0,
            &u("https://a/search"),
            &Method::QUERY,
            StatusCode::SEE_OTHER,
            Some(b"https://a/result"),
        );
        let RedirectAction::Follow(f) = r else {
            panic!("expected a Follow, got {r:?}");
        };
        assert_eq!(f.method, Method::GET);
        assert!(f.drop_body);
    }

    #[test]
    fn garbage_location_is_reported() {
        let r = decide(
            &p(),
            0,
            &u("https://a/"),
            &Method::GET,
            StatusCode::FOUND,
            Some(b"ht!tp://\x00"),
        );
        assert!(matches!(r, RedirectAction::InvalidLocation));
    }

    // ── review: finding 1 — asymmetry in default ports ──────────────
    //
    // `current` arrives as-is from the caller (it may carry an explicit
    // `:443`), and so does the redirect target now that `url` — which
    // stripped the default port on serialization — is gone. Without
    // `port_of` this would read as an origin change and strip
    // Authorization on every hop.

    #[test]
    fn keeps_sensitive_when_current_has_explicit_default_port() {
        let RedirectAction::Follow(f) = go(302, "https://a:443/", "https://a/", Method::GET) else {
            panic!()
        };
        assert!(
            !f.strip_sensitive,
            "explicit :443 on current must not read as cross-origin"
        );
    }

    #[test]
    fn keeps_sensitive_when_location_has_explicit_default_port() {
        let RedirectAction::Follow(f) = go(302, "https://a/", "https://a:443/", Method::GET) else {
            panic!()
        };
        assert!(
            !f.strip_sensitive,
            "explicit :443 on the target must not read as cross-origin"
        );
    }

    #[test]
    fn keeps_sensitive_when_current_has_explicit_default_port_http() {
        let RedirectAction::Follow(f) = go(302, "http://a:80/", "http://a/", Method::GET) else {
            panic!()
        };
        assert!(
            !f.strip_sensitive,
            "explicit :80 on current must not read as cross-origin"
        );
    }

    #[test]
    fn a_genuinely_different_port_is_still_cross_origin() {
        let RedirectAction::Follow(f) = go(302, "https://a:8443/", "https://a/", Method::GET)
        else {
            panic!()
        };
        assert!(
            f.strip_sensitive,
            "8443 vs default 443 is a real origin change"
        );
    }

    // ── the same asymmetry, in the host ─────────────────────────────
    //
    // The host is case-insensitive (RFC 3986 §6.2.2.1) and nothing
    // lower-cases it any more: `url` did, on the RESOLVED TARGET ONLY,
    // while `current` came from the caller as written. So before `url` was
    // removed, `current = https://A.test/` plus ANY Location — even a
    // relative one — compared `a.test` against `A.test` and stripped
    // Authorization from a request that never left the host it started on.
    // Both directions are checked because the old asymmetry only broke one
    // of them, and a fix that only handles that one would look right.

    #[test]
    fn a_host_differing_only_in_case_is_not_a_change_of_origin() {
        for (from, to) in [
            ("https://A.test/", "https://a.test/x"),
            ("https://a.test/", "https://A.TEST/x"),
            ("https://A.test/", "y"),
        ] {
            let RedirectAction::Follow(f) = go(302, from, to, Method::GET) else {
                panic!("{from} -> {to} must be followed at all")
            };
            assert!(
                !f.strip_sensitive,
                "{from} -> {to}: a host that differs only in case is the same origin"
            );
        }
    }

    #[test]
    fn a_genuinely_different_host_is_still_cross_origin() {
        let RedirectAction::Follow(f) = go(302, "https://a.test/", "https://b.test/x", Method::GET)
        else {
            panic!()
        };
        assert!(
            f.strip_sensitive,
            "case-insensitivity must not turn into host-insensitivity"
        );
    }

    // ── review: finding 2 — the contents of SENSITIVE_HEADERS were unchecked ──
    //
    // Review's mutation test: replacing the constant with three copies of
    // content-type left all twelve tests green, because nothing read the
    // constant itself.

    #[test]
    fn sensitive_headers_are_exactly_the_three_credential_carriers() {
        assert_eq!(
            SENSITIVE_HEADERS,
            [
                http::header::AUTHORIZATION,
                http::header::COOKIE,
                http::header::PROXY_AUTHORIZATION,
            ]
        );
    }

    #[test]
    fn strip_sensitive_removes_only_the_credential_headers() {
        let RedirectAction::Follow(f) = go(302, "https://a/", "https://b/", Method::GET) else {
            panic!()
        };
        assert!(f.strip_sensitive);

        // Simulate what the caller code must do: strip only
        // SENSITIVE_HEADERS, leave the rest of the headers untouched.
        let mut headers = http::HeaderMap::new();
        headers.insert(http::header::AUTHORIZATION, "secret".parse().unwrap());
        headers.insert(
            http::header::CONTENT_TYPE,
            "application/json".parse().unwrap(),
        );
        if f.strip_sensitive {
            for name in &SENSITIVE_HEADERS {
                headers.remove(name);
            }
        }
        assert!(
            !headers.contains_key(http::header::AUTHORIZATION),
            "Authorization must be stripped"
        );
        assert!(
            headers.contains_key(http::header::CONTENT_TYPE),
            "unrelated headers must survive"
        );
    }

    // ── review: finding 3 — Location validation was stricter than the ecosystem ──────
    //
    // `HeaderValue::from_bytes` closes CR/LF injection (C0 control bytes
    // and DEL). But `to_str()` additionally rejects any byte >= 0x80, and
    // raw non-ASCII in Location — a non-percent-encoded path, a "raw" IDN
    // host — shows up in practice; reqwest (through tower_http) follows
    // such a Location. We check both sides: non-ASCII passes, control
    // bytes don't.

    #[test]
    fn raw_utf8_path_is_followed() {
        let RedirectAction::Follow(f) = go(302, "https://a/", "/caf\u{e9}", Method::GET) else {
            panic!("raw UTF-8 path must not be rejected as InvalidLocation")
        };
        assert_eq!(f.uri, u("https://a/caf%C3%A9"));
    }

    /// The `idn` feature is what decides this one, and the two answers are
    /// written out rather than one being `cfg`-ed away, so neither build
    /// can quietly stop testing it. With the feature: the same A-label
    /// `url` used to produce. Without it: `InvalidLocation`, because the
    /// crate has no Unicode tables and will not guess — the error that
    /// says so and names the A-label is `uri::UriError::NonAsciiHost`,
    /// which `decide` flattens into `InvalidLocation` the same way it
    /// flattens every other resolution failure.
    #[cfg(feature = "idn")]
    #[test]
    fn raw_utf8_idn_host_is_followed() {
        let RedirectAction::Follow(f) = go(
            302,
            "https://a/",
            "https://m\u{fc}nchen.example/",
            Method::GET,
        ) else {
            panic!("raw UTF-8 IDN host must not be rejected as InvalidLocation")
        };
        assert_eq!(f.uri, u("https://xn--mnchen-3ya.example/"));
        assert!(f.strip_sensitive, "host actually changed");
    }

    #[cfg(not(feature = "idn"))]
    #[test]
    fn raw_utf8_idn_host_is_rejected_without_the_idn_feature() {
        let r = go(
            302,
            "https://a/",
            "https://m\u{fc}nchen.example/",
            Method::GET,
        );
        assert!(
            matches!(r, RedirectAction::InvalidLocation),
            "without `idn` there is nothing to convert a U-label with, and inventing \
             a host would be worse than refusing: {r:?}"
        );
        // The A-label form of the same host is ASCII and still followed —
        // the feature removes IDNA, not internationalised hosts.
        let RedirectAction::Follow(f) = go(
            302,
            "https://a/",
            "https://xn--mnchen-3ya.example/",
            Method::GET,
        ) else {
            panic!("an A-label needs no Unicode tables and must still be followed")
        };
        assert_eq!(f.uri, u("https://xn--mnchen-3ya.example/"));
    }

    #[test]
    fn bare_cr_in_location_is_rejected() {
        let r = decide(
            &p(),
            0,
            &u("https://a/"),
            &Method::GET,
            StatusCode::FOUND,
            Some(b"https://b/\r"),
        );
        assert!(matches!(r, RedirectAction::InvalidLocation));
    }

    #[test]
    fn bare_lf_in_location_is_rejected() {
        let r = decide(
            &p(),
            0,
            &u("https://a/"),
            &Method::GET,
            StatusCode::FOUND,
            Some(b"https://b/\n"),
        );
        assert!(matches!(r, RedirectAction::InvalidLocation));
    }

    #[test]
    fn crlf_header_injection_is_rejected() {
        let r = decide(
            &p(),
            0,
            &u("https://a/"),
            &Method::GET,
            StatusCode::FOUND,
            Some(b"https://b/\r\nX-Injected: 1"),
        );
        assert!(matches!(r, RedirectAction::InvalidLocation));
    }
}
