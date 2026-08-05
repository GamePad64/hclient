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

    // `Location` пришёл из сети как сырые байты. Сначала проверяем их как
    // валидное значение HTTP-заголовка (это отсекает управляющие символы
    // вроде NUL, которые являются легальным UTF-8, но не легальны в
    // field-value), и только потом — как UTF-8 строку.
    let Ok(location) = HeaderValue::from_bytes(location) else {
        return RedirectAction::InvalidLocation;
    };
    let Ok(location) = location.to_str() else {
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
        || uri.port_u16() != current.port_u16();

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
}
