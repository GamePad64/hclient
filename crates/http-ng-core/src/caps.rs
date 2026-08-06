use http::HeaderName;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RedirectSupport {
    /// Редиректов нет и наблюдать нечего.
    None,
    /// Бэкенд следует сам, мы не управляем и не видим (wasi:http).
    Internal,
    /// Мы задаём политику.
    Configurable,
    /// Мы задаём политику и видим каждый хоп.
    Inspectable,
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

/// Тройка таймаутов — форма `wasi:http`, богатейшая из ambient-моделей.
///
/// В fetch схлопывается в один `AbortController`, в native раскладывается на
/// коннектор / ожидание ответа / idle тела. Один `Duration` выбрасывает
/// информацию, которой WASI-бэкенд умеет пользоваться.
///
/// Живёт в `http-ng-core`, потому что транспорты читают её из
/// `http::Extensions` запроса, а от `http-ng` они не зависят.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Timeouts {
    pub connect: Option<core::time::Duration>,
    pub first_byte: Option<core::time::Duration>,
    pub between_bytes: Option<core::time::Duration>,
}

/// Что транспорт умеет **в этом процессе, сейчас**.
///
/// Именно рантайм, а не `cfg!`: один wasm-бинарь работает и в Chrome
/// (streaming request body есть с 131), и в Safari (нет).
#[non_exhaustive]
#[derive(Debug, Clone)]
pub struct Capabilities {
    pub streaming_request_body: bool,
    pub full_duplex: bool,
    pub request_trailers: bool,
    pub response_trailers: bool,
    pub redirects: RedirectSupport,
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
    /// Всё выключено. База, от которой бэкенд включает то, что действительно умеет.
    pub const fn none() -> Self {
        Self {
            streaming_request_body: false,
            full_duplex: false,
            request_trailers: false,
            response_trailers: false,
            redirects: RedirectSupport::None,
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

/// Настройка, которую выбранный транспорт не может выполнить.
///
/// Возвращается из `build()`, а не игнорируется молча. Образец — сам wasi:http,
/// где сеттеры возвращают `request-options-error::not-supported`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnsupportedCapability {
    pub what: &'static str,
    pub backend: &'static str,
}

impl std::fmt::Display for UnsupportedCapability {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "backend `{}` does not support `{}`",
            self.backend, self.what
        )
    }
}
impl std::error::Error for UnsupportedCapability {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn none_is_the_conservative_base() {
        // Every one of the 16 fields, spelled out individually — not
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
