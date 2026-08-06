use crate::config::{Config, check_supported};
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
    pub fn timeouts(mut self, t: Timeouts) -> Self {
        self.config.timeouts = t;
        self
    }
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
    /// `Error::new(ErrorKind::Other, e)` ниже не собрался бы для абстрактного
    /// `T`: `Error` хранит источник как `Arc<dyn Error + Send + Sync>`, и
    /// стирание типа не пропускает auto-traits неограниченного объекта-трейта
    /// (проверено компиляцией — без этой границы E0277 на обоих бондах).
    /// Бонд живёт здесь, а не в самом трейте `Transport`, как и
    /// задокументировано в `http-ng-core`'s lib.rs.
    pub async fn execute(
        &self,
        req: http::Request<RequestBody>,
    ) -> Result<http::Response<T::Body>, Error>
    where
        T::Error: Send + Sync + 'static, // send-bound-exception: amendment-C1
    {
        let (parts, mut body) = req.into_parts();
        let mut hp = HopParts {
            method: parts.method,
            uri: parts.uri,
            headers: parts.headers,
            version: parts.version,
            extensions: parts.extensions,
        };
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
                .map_err(|e| Error::new(ErrorKind::Other, e))?;

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
