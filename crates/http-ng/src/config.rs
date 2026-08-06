// `Timeouts` определён в `http-ng-core` (Task 8): его читают транспорты из
// `http::Extensions`, а от `http-ng` они не зависят.
pub use http_ng_core::Timeouts;
use http_ng_core::{Capabilities, UnsupportedCapability};
use http_ng_proto::redirect::RedirectPolicy;

#[derive(Debug, Clone, Default)]
pub struct Config {
    pub timeouts: Timeouts,
    pub redirect: RedirectPolicy,
    pub base_url: Option<http::Uri>,
}

/// «Request-first, client-fallback», поле за полем.
///
/// reqwest этого не умеет (issue #2641 не реализован), из-за чего `act-cli`
/// вынужден строить отдельный `reqwest::Client` на каждый вызов компонента.
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

/// Вызывается из `ClientBuilder::build()`. Ни одного тихого no-op.
pub fn check_supported(
    cfg: &Config,
    caps: &Capabilities,
    backend: &'static str,
) -> Result<(), UnsupportedCapability> {
    let checks = [
        (
            cfg.timeouts.connect.is_some(),
            caps.timeouts.connect,
            "connect_timeout",
        ),
        (
            cfg.timeouts.first_byte.is_some(),
            caps.timeouts.first_byte,
            "first_byte_timeout",
        ),
        (
            cfg.timeouts.between_bytes.is_some(),
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
        assert_eq!(eff.connect, secs(9), "запрос перекрывает");
        assert_eq!(
            eff.first_byte,
            secs(2),
            "остальное падает обратно на клиент"
        );
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

    // ── доп. проверки: field-by-field, а не all-or-nothing ─────────────
    //
    // `request_overrides_client_field_by_field` перекрывает `connect` и
    // проверяет, что `first_byte`/`between_bytes` падают на клиент — это
    // уже отличает "поле за полем" от "всё или ничего" (наивная реализация
    // "raз в extensions есть Timeouts — берём его целиком" вернула бы здесь
    // `None`, а не `client.first_byte`). Тест ниже бьёт по другому полю
    // (`first_byte`), чтобы то же свойство не было артефактом того, что
    // `connect` — первое поле структуры.
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
        assert_eq!(eff.connect, secs(1), "не перекрыт запросом — берём клиента");
        assert_eq!(eff.first_byte, secs(9), "запрос перекрывает");
        assert_eq!(
            eff.between_bytes,
            secs(3),
            "не перекрыт запросом — берём клиента"
        );
    }

    // ── доп. проверки: check_supported называет ИМЕННО ту фазу ──────────
    //
    // `unsupported_timeout_is_an_error_not_a_silent_noop` покрывает только
    // `between_bytes`. Раз `checks` в `check_supported` — массив из трёх
    // независимых троек, ошибка малого рефакторинга (например, копипаста
    // индекса) могла бы вернуть верную ошибку для одной фазы и неверную
    // (не то поле в `what`, либо не та фаза триггерит ошибку) для двух
    // других незамеченной. Проверяем все три фазы по отдельности: у каждой
    // запрошено только это поле, и не поддерживается в `Capabilities`
    // только оно же.
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
