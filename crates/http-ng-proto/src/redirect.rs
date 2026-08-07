//! The decision to follow a redirect. A pure function: no I/O, no time.

use http::{HeaderName, HeaderValue, Method, StatusCode, Uri};

/// Headers stripped when moving to a different origin.
pub const SENSITIVE_HEADERS: [HeaderName; 3] = [
    http::header::AUTHORIZATION,
    http::header::COOKIE,
    http::header::PROXY_AUTHORIZATION,
];

#[derive(Debug, Clone, Copy)]
pub struct RedirectPolicy {
    pub limit: u8,
}

impl Default for RedirectPolicy {
    fn default() -> Self {
        Self { limit: 10 }
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
/// `http::Uri` preserves an explicit `:443`, while the redirect target goes
/// through `url::Url`, which strips it. Without normalization,
/// `https://a:443/` → `https://a/` would read as an origin change and
/// strip Authorization on every hop.
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
    if hops >= policy.limit {
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
    let Some(uri) = crate::uri::resolve_reference(current, location) else {
        return RedirectAction::InvalidLocation;
    };

    let cross_origin = uri.host() != current.host()
        || uri.scheme_str() != current.scheme_str()
        || port_of(&uri) != port_of(current);

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
        RedirectPolicy { limit: 10 }
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
            &RedirectPolicy { limit: 2 },
            2,
            &u("https://a/"),
            &Method::GET,
            StatusCode::FOUND,
            Some(b"https://a/x"),
        );
        assert!(matches!(r, RedirectAction::TooManyRedirects));
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
    // `:443`), while the redirect target always goes through `url::Url`,
    // which strips the default port on serialization. Without
    // normalization this would read as an origin change and strip
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
