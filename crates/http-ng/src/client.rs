use crate::config::{
    Config, check_supported, check_timeouts_supported, effective_timeouts, effective_uri,
};
use crate::request::RequestBuilder;
use crate::stages::redirect::{HopParts, next_hop};
use http_ng_core::Timeouts;
use http_ng_core::unversioned::Transport;
use http_ng_core::{Capabilities, Error, ErrorKind, RequestBody, UnsupportedCapability};
use http_ng_proto::redirect::{RedirectAction, RedirectPolicy, decide};

#[derive(Debug)]
pub struct ClientBuilder<T> {
    transport: T,
    config: Config,
}

impl<T: Transport> ClientBuilder<T> {
    pub fn new(transport: T) -> Self {
        Self {
            transport,
            config: Config::default(),
        }
    }
    pub fn redirect(mut self, policy: RedirectPolicy) -> Self {
        self.config.redirect = policy;
        self
    }
    /// Таймауты по умолчанию для каждого запроса этого клиента.
    ///
    /// Поле за полем перекрываются `RequestBuilder::timeouts`; слияние
    /// делает `Client::execute` (`effective_timeouts`), и его результат —
    /// то, что реально едет транспорту в `http::Extensions`. Неподдерживаемая
    /// транспортом фаза — ошибка на `build()`, а с B1/M3 и на `execute()`
    /// для того, что задал сам запрос.
    pub fn timeouts(mut self, t: Timeouts) -> Self {
        self.config.timeouts = t;
        self
    }
    /// База, относительно которой разрешается URI каждого запроса.
    ///
    /// Ответ этой библиотеки на reqwest #988 и #213 (открыты с 2017 и 2020,
    /// 104 голоса). До фикс-раунда 3 значение здесь сохранялось и не
    /// читалось ниоткуда — то есть сеттер был тихим no-op.
    ///
    /// **Правило — RFC 3986 §5**, то же самое, каким разрешается `Location:`
    /// из ответа: один клиент не должен понимать `/x` двумя способами в
    /// зависимости от того, прислал его сервер или вызывающая сторона.
    /// Отсюда два следствия, которые стоит прочитать до того, как они
    /// удивят:
    ///
    /// ```text
    /// base "https://api.test/v1/"   + "things"    -> https://api.test/v1/things
    /// base "https://api.test/v1/"   + "/things"   -> https://api.test/things      // ведущий / ЗАМЕНЯЕТ путь базы
    /// base "https://api.test/v1"    + "things"    -> https://api.test/things      // база без / — не каталог
    /// base "https://api.test/v1/"   + "https://other.test/x" -> https://other.test/x
    /// ```
    ///
    /// То есть база с путём почти всегда должна заканчиваться слэшем, а
    /// ссылка — НЕ начинаться с него. Обе строки выше — не наша
    /// самодеятельность, а merge и §5.2.2 RFC; они же работают в `url::Url::
    /// join`, в браузерном `new URL(ref, base)` и в `urllib.parse.urljoin`.
    ///
    /// Сама база обязана быть абсолютной. Относительная (`/api/`) —
    /// типизированная ошибка `InvalidBaseUrl` из `send()`/`execute()`, а не
    /// тихо проигнорированная настройка. Проверки на `build()` нет
    /// сознательно: она потребовала бы сменить тип ошибки `build()`, что
    /// шире этого раунда — записано в отчёте.
    ///
    /// Ограничение, о котором стоит знать: `Client::execute`, принимающий
    /// готовый `http::Request`, видит уже разобранный `http::Uri`, а тот
    /// path-relative ссылку не представляет вовсе (`"things"` —
    /// `InvalidUri`). Через этот вход база может дать запросу схему и
    /// authority, но не путь. `RequestBuilder` (`client.get("things")`)
    /// разрешает исходную строку до разбора и такого ограничения не имеет.
    pub fn base_url(mut self, uri: http::Uri) -> Self {
        self.config.base_url = Some(uri);
        self
    }
    /// Проверяет конфигурацию против возможностей транспорта. Ни одного
    /// тихого no-op: неподдерживаемая настройка — ошибка здесь и сейчас.
    pub fn build(self) -> Result<Client<T>, UnsupportedCapability> {
        check_supported(
            &self.config,
            self.transport.capabilities(),
            backend_name::<T>(),
        )?;
        Ok(Client {
            transport: self.transport,
            config: self.config,
        })
    }
}

fn backend_name<T>() -> &'static str {
    // Имя типа достаточно информативно для сообщения об ошибке и ничего не стоит.
    std::any::type_name::<T>()
}

// Раздвоенное объявление, а не одно `pub struct Client<T = crate::
// DefaultTransport>` под условным дефолтом: у Rust нет способа сделать
// генерик-дефолт сам условным на фиче — `#[cfg]` на отдельном параметре
// дефолта внутри одного объявления структуры не читается компилятором.
// Без фичи `default-transport` `Client` обязан требовать `T` явно (обычная
// ошибка компиляции «missing generics» на `Client` без параметра — та же
// честная ошибка, что и у отсутствующего `DefaultTransport`, см. его
// doc-комментарий в `lib.rs`), а не резолвиться в дефолт из ветки, которой
// с выключенной фичей вообще не существует. Оба варианта ниже — тот же
// набор полей, `impl<T: Transport> Client<T>` дальше применяется к обоим
// одинаково: дефолт параметра генерика влияет только на места вызова, где
// `Client` написан без явных `<...>` (например, возврат `Client::new()`
// ниже), не на сигнатуры existing impl-блоков.
#[cfg(feature = "default-transport")]
#[derive(Debug)]
pub struct Client<T = crate::DefaultTransport> {
    transport: T,
    config: Config,
}
#[cfg(not(feature = "default-transport"))]
#[derive(Debug)]
pub struct Client<T> {
    transport: T,
    config: Config,
}

impl<T: Transport> Client<T> {
    pub fn builder(transport: T) -> ClientBuilder<T> {
        ClientBuilder::new(transport)
    }
    pub fn transport(&self) -> &T {
        &self.transport
    }
    pub fn config(&self) -> &Config {
        &self.config
    }
    /// Что умеет транспорт этого клиента.
    ///
    /// Форвардер существует, чтобы ответ на самый естественный вопрос к
    /// `Capabilities` не требовал тащить в область видимости
    /// `unversioned::Transport` (Task 17 fix round 2) — трейт намеренно в
    /// semver-карантине (см. doc-комментарий
    /// `http-ng-core/src/unversioned/mod.rs`) и в фасад `http-ng` не входит.
    /// Без этого форвардера `client.transport().capabilities()` — трейтовый
    /// метод — был единственным путём, а `client.transport()` возвращает
    /// `&T`, так что вызов `.capabilities()` на нём требовал бы `Transport`
    /// в `use`.
    pub fn capabilities(&self) -> &Capabilities {
        self.transport.capabilities()
    }

    pub fn request(&self, method: http::Method, url: &str) -> RequestBuilder<'_, T> {
        RequestBuilder::new(self, method, url)
    }
    pub fn get(&self, url: &str) -> RequestBuilder<'_, T> {
        self.request(http::Method::GET, url)
    }
    pub fn post(&self, url: &str) -> RequestBuilder<'_, T> {
        self.request(http::Method::POST, url)
    }
    pub fn put(&self, url: &str) -> RequestBuilder<'_, T> {
        self.request(http::Method::PUT, url)
    }
    pub fn delete(&self, url: &str) -> RequestBuilder<'_, T> {
        self.request(http::Method::DELETE, url)
    }
    pub fn patch(&self, url: &str) -> RequestBuilder<'_, T> {
        self.request(http::Method::PATCH, url)
    }
    pub fn head(&self, url: &str) -> RequestBuilder<'_, T> {
        self.request(http::Method::HEAD, url)
    }

    /// Порядок стадий фиксирован и корректен по построению.
    /// В v0.1 стадия одна — redirect.
    ///
    /// `where T::Error: Send + Sync + 'static` — второе документированное
    /// исключение из инварианта «ядро не объявляет Send/Sync» (spec
    /// amendment C1, сестра исключения у `Error::source`). Без него
    /// `Transport::to_error` ниже не вызвался бы для абстрактного `T`: его
    /// собственная where-клауза требует того же бонда, потому что его
    /// дефолтное тело зовёт `Error::new`, а `Error` хранит источник как
    /// `Arc<dyn Error + Send + Sync>`, и стирание типа не пропускает
    /// auto-traits неограниченного объекта-трейта (проверено компиляцией —
    /// без этой границы E0277). Бонд живёт здесь и на самом методе, а не на
    /// трейте `Transport` целиком, как и задокументировано в
    /// `http-ng-core`'s lib.rs: транспорт с честно `!Send` ошибкой остаётся
    /// представимым, он просто не может пользоваться `Client`.
    pub async fn execute(
        &self,
        req: http::Request<RequestBody>,
    ) -> Result<http::Response<T::Body>, Error>
    where
        T::Error: Send + Sync + 'static, // send-bound-exception: amendment-C1
    {
        let (parts, mut body) = req.into_parts();

        // Базовый URL применяется здесь, а не только в `RequestBuilder`:
        // `execute` — публичный вход, принимающий готовый `http::Request`, и
        // настройка, работающая лишь на одном из двух путей, была бы
        // починена наполовину. Идемпотентно — `RequestBuilder::send`
        // разрешает тот же URI заранее (ему результат нужен для
        // `Response::url()`), а разрешение уже абсолютного URI возвращает
        // его самого (RFC 3986 §5.2.2).
        let uri = effective_uri(self.config.base_url.as_ref(), &parts.uri.to_string())?;

        let mut hp = HopParts {
            method: parts.method,
            uri,
            headers: parts.headers,
            version: parts.version,
            extensions: parts.extensions,
        };

        // B1/M3 финального ревью ветки, две половины одной дыры. До него
        // `effective_timeouts` не вызывалась ниоткуда в продакшн-коде —
        // `ClientBuilder::timeouts()` был тихим no-op, потому что
        // единственный канал к транспорту это `http::Extensions`, а
        // клиентская конфигурация в них не попадала; и симметрично
        // `RequestBuilder::timeouts()` писал в `Extensions` вообще без
        // проверки против `Capabilities`, тогда как та же настройка на
        // уровне клиента давала `UnsupportedCapability` на `build()`.
        //
        // Слияние и проверка живут здесь, а не в `build()` и не в
        // `RequestBuilder`, потому что только здесь известен ОБА слагаемых.
        // Результат кладётся в `extensions` до цикла: следующие хопы
        // клонируют их из предыдущего (`stages::redirect::next_hop`), так
        // что слить достаточно один раз.
        let effective = effective_timeouts(&hp.extensions, &self.config.timeouts);
        check_timeouts_supported(
            &effective,
            self.transport.capabilities(),
            backend_name::<T>(),
        )
        .map_err(|e| Error::new(ErrorKind::Unsupported, e))?;
        hp.extensions.insert(effective);

        let mut hops: u8 = 0;

        loop {
            // Снимок для переигрывания снимается ДО отправки: после неё тело
            // уже потреблено. Для `Streaming` вернётся `None` — и это честно
            // известно заранее, а не после провала ретрая.
            let replay = body.rewind();
            let sending = std::mem::replace(&mut body, RequestBody::Empty);

            let resp = self
                .transport
                .execute(hp.to_request(sending))
                .await
                // Не `Error::new(ErrorKind::Other, e)`: B2 финального ревью
                // ветки — безусловное обёртывание расплющивало категорию
                // ЛЮБОЙ ошибки транспорта в `Other`, обесценивая всю
                // таксономию `ErrorKind`. Решает бэкенд, а не эта строка:
                // дефолт `Transport::to_error` обёртывает ровно так же,
                // а бэкенд, чья ошибка уже `Error`, отдаёт её как есть.
                .map_err(|e| self.transport.to_error(e))?;

            let location = resp
                .headers()
                .get(http::header::LOCATION)
                .map(|v| v.as_bytes());
            let action = decide(
                &self.config.redirect,
                hops,
                &hp.uri,
                &hp.method,
                resp.status(),
                location,
            );

            match action {
                RedirectAction::Stop => return Ok(resp),
                RedirectAction::TooManyRedirects => {
                    return Err(Error::new(
                        ErrorKind::Redirect,
                        TooMany(self.config.redirect.limit),
                    ));
                }
                RedirectAction::InvalidLocation => {
                    return Err(Error::new(ErrorKind::Redirect, BadLocation));
                }
                RedirectAction::Follow(f) => {
                    hops += 1;
                    let Some((next_hp, next_body)) = next_hop(&hp, replay, &f) else {
                        return Ok(resp);
                    };
                    hp = next_hp;
                    body = next_body;
                }
            }
        }
    }
}

// `not(target_family = "wasm")`, а не только `feature = "default-transport"`
// — тот же двойной гейт, что у `DefaultTransport` самого (`lib.rs`): на
// wasm-таргетах, где ветка `DefaultTransport` не существует (см. её
// doc-комментарий), этот `impl` для `Client<crate::DefaultTransport>`
// ссылался бы на несуществующий тип. Раздельные гейты дали бы то же самое
// поведение (`impl` для несуществующего типа тоже не компилируется), но
// повторение условия делает причину видимой на месте, а не только в
// `lib.rs`.
#[cfg(all(feature = "default-transport", not(target_family = "wasm")))]
impl Client<crate::DefaultTransport> {
    /// Клиент с транспортом по умолчанию.
    ///
    /// На native требует окружающего tokio-рантайма: `tokio::spawn` и
    /// `tokio::time::sleep` вне рантайма паникуют. Ровно так же ведёт себя
    /// reqwest. Явный путь без этого требования — `Client::builder(Native::
    /// new(rt, tls, dns))` с рантаймом на выбор (см. `crates/http-ng/tests/
    /// two_runtimes.rs`, тот же конструктор для tokio и для smol).
    ///
    /// `.expect("platform verifier")` на ошибке `Rustls::
    /// with_platform_verifier()`, а не проброс через `Result` этой функции:
    /// возвращаемая ошибка здесь — `UnsupportedCapability` (`what`,
    /// `backend`, оба `&'static str`) — типизированный ответ на «транспорт
    /// не поддерживает вот эту настройку клиента», а не на «системное
    /// хранилище доверия не удалось прочитать». Смешать их значило бы
    /// врать о причине отказа тем же способом, каким «нет молчаливых
    /// no-op» запрещает вертикали лгать об успехе — здесь наоборот: не
    /// лгать о категории отказа. Отказ `with_platform_verifier()` на
    /// практике означает окружение без работающего системного хранилища
    /// сертификатов — по наблюдению `rustls-platform-verifier`, состояние
    /// среды, не конфигурация клиента, и `expect` делает это громким
    /// падением с ясным сообщением вместо тихого молчания, а не подменяет
    /// его.
    pub fn new() -> Result<Self, http_ng_core::UnsupportedCapability> {
        let rt = http_ng_rt_tokio::Tokio;
        Self::builder(http_ng_native::Native::new(
            rt,
            http_ng_tls_rustls::Rustls::with_platform_verifier().expect("platform verifier"),
            http_ng_dns_system::SystemDns::new(rt),
        ))
        .build()
    }
}

#[derive(Debug)]
struct TooMany(u8);
impl std::fmt::Display for TooMany {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "exceeded redirect limit of {}", self.0)
    }
}
impl std::error::Error for TooMany {}

#[derive(Debug)]
struct BadLocation;
impl std::fmt::Display for BadLocation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Location header is not a resolvable URI")
    }
}
impl std::error::Error for BadLocation {}
