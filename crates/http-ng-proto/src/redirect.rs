//! Решение о следовании редиректу. Чистая функция: ни I/O, ни времени.

use http::{HeaderName, HeaderValue, Method, StatusCode, Uri};

/// Заголовки, снимаемые при уходе на другой origin.
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
    /// Куда переходить. Может нести userinfo, присланный сервером
    /// (`https://user:pass@host/`) — он не входит в origin по RFC 6454 и
    /// поэтому не участвует в `strip_sensitive`, но это чужой ввод: не
    /// продвигать его молча в доверенные credentials ниже по стеку.
    pub uri: Uri,
    pub method: Method,
    /// Снять `SENSITIVE_HEADERS`: сменился host или scheme.
    pub strip_sensitive: bool,
    /// Метод понижен до GET — тело отправлять нельзя.
    pub drop_body: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RedirectAction {
    /// Не редирект, либо редирект без `Location` — вернуть ответ как есть.
    Stop,
    Follow(Follow),
    TooManyRedirects,
    InvalidLocation,
}

/// Порт с подстановкой умолчания по схеме.
///
/// `http::Uri` сохраняет явный `:443`, а цель редиректа проходит через
/// `url::Url`, который его срезает. Без нормализации `https://a:443/` →
/// `https://a/` читался бы как смена origin и снимал бы Authorization
/// на каждом хопе.
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
    // ВАЖНО: не `status.is_redirection()`. 300 Multiple Choices требует выбора
    // пользователя, 304 Not Modified — ответ на условный запрос, 305 Use Proxy
    // не следуют с 2014 года, 306 зарезервирован.
    if !matches!(status.as_u16(), 301 | 302 | 303 | 307 | 308) {
        return RedirectAction::Stop;
    }
    let Some(location) = location else {
        return RedirectAction::Stop;
    };
    if hops >= policy.limit {
        return RedirectAction::TooManyRedirects;
    }

    // Валидируем как значение заголовка: отвергает C0-управляющие и DEL,
    // то есть закрывает CR/LF-инъекцию через Location. Но НЕ через `to_str()`
    // — тот отвергает и любой байт >= 0x80, а сырой не-ASCII (не
    // percent-encoded путь, IDN-хост) формально невалиден и при этом
    // встречается на практике; reqwest через tower_http его следует
    // (`str::from_utf8` на сырых байтах, без ограничения по ASCII).
    let Ok(header) = HeaderValue::from_bytes(location) else {
        return RedirectAction::InvalidLocation;
    };
    let Ok(location) = core::str::from_utf8(header.as_bytes()) else {
        return RedirectAction::InvalidLocation;
    };
    let Ok(base) = url::Url::parse(&current.to_string()) else {
        return RedirectAction::InvalidLocation;
    };
    let Ok(joined) = base.join(location) else {
        return RedirectAction::InvalidLocation;
    };
    let Ok(uri) = joined.as_str().parse::<Uri>() else {
        return RedirectAction::InvalidLocation;
    };

    let cross_origin = uri.host() != current.host()
        || uri.scheme_str() != current.scheme_str()
        || port_of(&uri) != port_of(current);

    // 303 — всегда GET (кроме HEAD). 301/302 с POST браузеры и reqwest
    // понижают до GET; расхождение с 303 было бы непоследовательным.
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

    // ── ревью: находка 1 — асимметрия портов по умолчанию ──────────────
    //
    // `current` приходит как есть от вызывающего (может нести явный `:443`),
    // а цель редиректа всегда проходит через `url::Url`, который срезает
    // порт по умолчанию при сериализации. Без нормализации это читалось бы
    // как смена origin и снимало бы Authorization на каждом хопе.

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

    // ── ревью: находка 2 — содержимое SENSITIVE_HEADERS не было проверено ──
    //
    // Мутационный тест ревью: подмена константы на три копии content-type
    // оставляла все двенадцать тестов зелёными, потому что ничто не читало
    // саму константу.

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

        // Симулируем то, что должен делать вызывающий код: снять только
        // SENSITIVE_HEADERS, остальные заголовки оставить как есть.
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

    // ── ревью: находка 3 — валидация Location была строже экосистемы ──────
    //
    // `HeaderValue::from_bytes` закрывает CR/LF-инъекцию (C0-управляющие и
    // DEL). Но `to_str()` дополнительно отвергает любой байт >= 0x80, а
    // сырой не-ASCII в Location — не percent-encoded путь, «сырой» IDN-хост
    // — встречается на практике; reqwest (через tower_http) такой Location
    // следует. Проверяем обе стороны: не-ASCII проходит, управляющие байты —
    // нет.

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
