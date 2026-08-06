// `Timeouts` определён в `http-ng-core` (Task 8): его читают транспорты из
// `http::Extensions`, а от `http-ng` они не зависят.
pub use http_ng_core::Timeouts;
use http_ng_core::{Capabilities, Error, ErrorKind, UnsupportedCapability};
use http_ng_proto::redirect::RedirectPolicy;

#[derive(Debug, Clone, Default)]
pub struct Config {
    pub timeouts: Timeouts,
    pub redirect: RedirectPolicy,
    pub base_url: Option<http::Uri>,
}

/// Базовый URL непригоден для разрешения этого запроса.
///
/// `pub` и реэкспортируется фасадом не для красоты: вызывающая сторона
/// обязана уметь отличить именно это от любой другой `ErrorKind::Other`
/// через `Error::source().downcast_ref::<InvalidBaseUrl>()` — тот же приём,
/// что у `mock::QueueEmpty`. Оба поля публичны, чтобы диагностика называла
/// конкретную пару, а не только факт.
///
/// `requested` — `String`, а не `http::Uri`: разрешение работает на СТРОКЕ
/// до разбора (см. `effective_uri`), и ровно те ссылки, ради которых база
/// существует, как `http::Uri` не выражаются вовсе.
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

/// URI, по которому запрос действительно уйдёт: `url`, разрешённый
/// относительно `base`, если база задана.
///
/// Правило — RFC 3986 §5, ровно то же, каким `redirect::decide` разрешает
/// `Location:`; общая реализация — `http_ng_proto::uri::resolve_reference`.
/// Один клиент не должен понимать `/x` двумя способами в зависимости от
/// того, прислал его сервер или вызывающая сторона.
///
/// **Работает на строке, а не на `http::Uri`, и это вынужденно.**
/// `http::Uri` не умеет представлять path-relative ссылку вообще: `"v1/things"`,
/// `"search?q=1"` и `""` — все три `InvalidUri` (измерено). А это ровно те
/// формы, ради которых база и существует: ссылка с ведущим `/` по RFC
/// ЗАМЕНЯЕТ путь базы целиком, так что если разрешать уже разобранные
/// `Uri`, путь базы не смог бы повлиять ни на что и настройка была бы
/// «задать origin», а не «задать базовый URL».
///
/// Без базы — обычный разбор: выдумывать за вызывающую сторону схему и
/// authority неоткуда, а транспорт, которому нужен absolute-form, отвергнет
/// относительный URI сам и типизированно (`WasiHttp` → `scheme_of`).
///
/// До этого раунда функции не существовало вовсе, а `Config::base_url`
/// писался сеттером и не читался ниоткуда — третий в проекте случай
/// «сохранили и проигнорировали» и второй в этой же структуре после
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
///
/// Деструктурирует `cfg` без `..`-остатка — по тому же рецепту, что
/// `Capabilities::none_is_the_conservative_base` в `http-ng-core`: новое
/// поле в `Config` становится ошибкой компиляции, называющей его, а не тихо
/// пропускается (то же для полей `Timeouts` — в
/// `check_timeouts_supported`). `redirect` и `base_url` сегодня намеренно не
/// проверяются на поддержку — `_` явно фиксирует это решение, а не забывает
/// про поле.
///
/// «Не проверяются **на поддержку**» — это про `Capabilities`, и только про
/// них. Оба поля при этом ПРИМЕНЯЮТСЯ: `base_url` — в `effective_uri` на
/// каждом запросе, `redirect` — стадией редиректа в `Client::execute`.
/// Уточнение здесь потому, что прежняя формулировка легко читалась как
/// «этими полями никто не занимается», а `base_url` ровно таким и был —
/// сохранялся и не применялся.
///
/// `redirect: _` перестанет быть безобидным, как только появится бэкенд с
/// `RedirectSupport::Internal`: он следует редиректам сам, стадия
/// `Client`'а не увидит ни одного 3xx, и заданный `RedirectPolicy` станет
/// ровно тем тихим no-op, против которого построен весь этот модуль. Ни
/// один существующий бэкенд не `Internal` (`WasiHttp` — `Transparent`, см.
/// `RedirectSupport`), так что сегодня проверять нечего; триггер —
/// браузерный `fetch` вертикали 3.
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

/// Та же проверка, но по одному `Timeouts`, а не по всему `Config` — потому
/// что `Client::execute` проверяет **слитый** результат
/// `effective_timeouts`, а не конфигурацию клиента (B1/M3 финального ревью
/// ветки: до него per-request таймауты не проверялись вовсе, а клиентские
/// проверялись здесь и не доезжали до транспорта). Общее тело, а не вторая
/// копия массива `checks`: разойтись эти две проверки не должны, а два
/// списка фаз разойдутся при первой же новой фазе.
///
/// `pub(crate)`, в отличие от `check_supported`: фасад `http-ng` и так
/// экспортирует больше плюмбинга, чем стоило бы (находка §6.7 того же
/// ревью), и увеличивать этот долг новым именем не за чем.
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
