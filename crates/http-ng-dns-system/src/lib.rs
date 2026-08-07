//! Системный резолвер поверх `std::net::ToSocketAddrs` (то есть `getaddrinfo`).
//!
//! `getaddrinfo` блокирующий на всех платформах, поэтому крейт требует
//! способности `Blocking` — и потому недоступен там, где её нет (wasm).
//!
//! **Ограничение, которое надо знать:** `getaddrinfo` никогда не вернёт
//! HTTPS/SVCB-записи. Значит на системном резолвере недостижимы ни ECH, ни
//! обнаружение HTTP/3 на первом запросе. `lookup_svcb` честно пуст — и
//! `supports_svcb()` честно `false`, дефолт `Resolve` не переопределён:
//! переопределить только `lookup_svcb` значило бы утверждать способность,
//! которой нет (см. doc-комментарий `Resolve::supports_svcb` в
//! `http-ng-dns`).
//!
//! **Известное ограничение: один вызов `getaddrinfo` на оба семейства.**
//! curl 8.20 делает **два**, в разных потоках, чтобы частичные результаты
//! запускали Happy Eyeballs раньше. Разделение на два слота — задача v0.2;
//! сейчас важнее, чтобы форма трейта `Resolve` это допускала, а она
//! допускает (раздельные `lookup_ipv4`/`lookup_ipv6`, не один метод,
//! возвращающий оба семейства разом).
#![forbid(unsafe_code)]

use futures_core::Stream;
use futures_util::StreamExt;
use http_ng_core::{Error, ErrorKind};
use http_ng_dns::{Resolve, ResolvedAddr};
use http_ng_rt::{Blocking, Cancelled};
use std::net::{IpAddr, ToSocketAddrs};

#[derive(Debug, Clone)]
pub struct SystemDns<B> {
    blocking: B,
}

impl<B> SystemDns<B> {
    pub fn new(blocking: B) -> Self {
        Self { blocking }
    }
}

#[derive(Debug)]
struct ResolveFailed(String, std::io::Error);
impl std::fmt::Display for ResolveFailed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "failed to resolve `{}`: {}", self.0, self.1)
    }
}
impl std::error::Error for ResolveFailed {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.1)
    }
}

impl<B: Blocking> SystemDns<B> {
    /// `Blocking::run` (Task 1, `amendment-C5`) возвращает
    /// `Result<T, Cancelled>`, а `T` здесь сам `Result<Vec<IpAddr>,
    /// ResolveFailed>` — то есть `res` ниже вложен в два слоя, и у него
    /// РОВНО три обитаемых формы, не две:
    ///
    /// - `Ok(Ok(addrs))` — `getaddrinfo` отработал и что-то вернул (может
    ///   быть пустым списком — это не ошибка).
    /// - `Ok(Err(ResolveFailed))` — `getaddrinfo` реально отказал: имя не
    ///   резолвится, это `ErrorKind::Resolve`.
    /// - `Err(Cancelled)` — пул фоновых потоков ушёл раньше, чем задача
    ///   успела выполниться (обычно рантайм завершает работу). Это НЕ
    ///   `ErrorKind::Resolve`: имя может быть в полном порядке, просто
    ///   ответа не будет никогда. Смешать её с DNS-отказом сказало бы
    ///   вызывающей стороне, что её DNS сломан, когда на деле завершается
    ///   процесс; молча превратить её в пустой стрим сделало бы её
    ///   неотличимой от «резолвер спросил и ничего не нашёл» — тот же
    ///   принцип, что развёл `supports_svcb()` и пустой `lookup_svcb` в
    ///   `http-ng-dns`, применённый здесь не к отсутствующей способности, а
    ///   к отказавшей попытке. Поэтому `Cancelled` заворачивается в
    ///   `ErrorKind::Other`, отдельно от `ErrorKind::Resolve` — категория,
    ///   которая для этой ошибки не подходит по значению `kind()`, но не
    ///   молчит и не выдаёт себя за DNS-отказ. `ErrorKind` не несёт
    ///   отдельного варианта для отмены рантайма (см.
    ///   `http-ng-core::error::ErrorKind`) — `Other` тот же выбор, что и
    ///   дефолт `Transport::to_error` в `http-ng-core` для ошибки без
    ///   собственной категории.
    fn lookup(&self, name: &str, want_v6: bool) -> impl Stream<Item = Result<ResolvedAddr, Error>> {
        let owned = name.to_owned();
        let fut = self.blocking.run(move || {
            (owned.as_str(), 0u16)
                .to_socket_addrs()
                .map(|it| it.map(|s| s.ip()).collect::<Vec<IpAddr>>())
                .map_err(|e| ResolveFailed(owned.clone(), e))
        });
        futures_util::stream::once(fut).flat_map(move |res| match res {
            Ok(Ok(addrs)) => futures_util::stream::iter(
                addrs
                    .into_iter()
                    .filter(|a| a.is_ipv6() == want_v6)
                    .map(|addr| Ok(ResolvedAddr { addr, ttl: None }))
                    .collect::<Vec<_>>(),
            ),
            Ok(Err(e)) => futures_util::stream::iter(vec![Err(Error::new(ErrorKind::Resolve, e))]),
            Err(Cancelled) => {
                futures_util::stream::iter(vec![Err(Error::new(ErrorKind::Other, Cancelled))])
            }
        })
    }
}

impl<B: Blocking> Resolve for SystemDns<B> {
    fn lookup_ipv4(&self, name: &str) -> impl Stream<Item = Result<ResolvedAddr, Error>> {
        self.lookup(name, false)
    }
    fn lookup_ipv6(&self, name: &str) -> impl Stream<Item = Result<ResolvedAddr, Error>> {
        self.lookup(name, true)
    }
    // `supports_svcb`/`lookup_svcb` намеренно не переопределены: дефолты
    // `Resolve` (`false` / пустой стрим) — точный, честный ответ для
    // `getaddrinfo`, который SVCB/HTTPS-записи вернуть не может в принципе.
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures_util::StreamExt;
    use http_ng_rt::{Blocking, Cancelled};

    struct Inline;
    impl Blocking for Inline {
        async fn run<T: Send + 'static, F: FnOnce() -> T + Send + 'static>(
            &self,
            f: F,
        ) -> Result<T, Cancelled> {
            Ok(f())
        }
    }

    #[test]
    fn resolves_localhost_into_the_right_family_streams() {
        let r = SystemDns::new(Inline);
        let v4: Vec<_> = futures_executor::block_on(r.lookup_ipv4("localhost").collect());
        let v4: Vec<_> = v4.into_iter().filter_map(Result::ok).collect();
        assert!(v4.iter().all(|a| a.addr.is_ipv4()), "в v4-стриме только v4");

        let v6: Vec<_> = futures_executor::block_on(r.lookup_ipv6("localhost").collect());
        let v6: Vec<_> = v6.into_iter().filter_map(Result::ok).collect();
        assert!(v6.iter().all(|a| a.addr.is_ipv6()), "в v6-стриме только v6");

        // `localhost` резолвится по-разному на разных машинах и в разных
        // CI-образах: только v4, только v6, или оба, и без гарантии порядка
        // внутри семейства (см. doc-комментарий `Resolve` в `http-ng-dns` —
        // «Гарантия порядка внутри стрима: её нет»). Проверяются только
        // инварианты (партиционирование по семействам выше и то, что
        // резолвится хоть что-то здесь), не точный состав — иначе тест
        // прошёл бы на одной машине и упал на другой по причине, не
        // связанной с кодом.
        assert!(
            !v4.is_empty() || !v6.is_empty(),
            "localhost должен резолвиться"
        );
    }

    #[test]
    fn unresolvable_name_yields_an_error_not_an_empty_stream() {
        let r = SystemDns::new(Inline);
        let got: Vec<_> = futures_executor::block_on(r.lookup_ipv4("invalid.invalid.").collect());
        assert!(
            got.iter().any(|x| x.is_err()),
            "пустой стрим неотличим от «политика всё отфильтровала»"
        );
        let err = got.into_iter().find_map(Result::err).unwrap();
        assert_eq!(
            err.kind(),
            &ErrorKind::Resolve,
            "настоящий отказ getaddrinfo обязан классифицироваться как Resolve"
        );
    }

    #[test]
    fn svcb_is_empty_because_getaddrinfo_cannot_return_it() {
        let r = SystemDns::new(Inline);
        let got: Vec<_> = futures_executor::block_on(r.lookup_svcb("example.com").collect());
        assert!(got.is_empty());
        // Пустой стрим сам по себе неоднозначен (см. doc-комментарий
        // `Resolve::supports_svcb` в `http-ng-dns`): без парной проверки
        // способности этот тест прошёл бы и для резолвера, который
        // заявляет, что умеет SVCB, но ничего не нашёл. `getaddrinfo`
        // честно не умеет SVCB вовсе — обе половины пары обязаны это
        // подтверждать вместе.
        assert!(
            !r.supports_svcb(),
            "пустой lookup_svcb без supports_svcb() == false — ложь по умолчанию"
        );
    }

    #[test]
    fn cancellation_is_not_mistaken_for_an_empty_or_dns_error_stream() {
        struct AlwaysCancelled;
        impl Blocking for AlwaysCancelled {
            async fn run<T: Send + 'static, F: FnOnce() -> T + Send + 'static>(
                &self,
                _f: F,
            ) -> Result<T, Cancelled> {
                Err(Cancelled)
            }
        }

        let r = SystemDns::new(AlwaysCancelled);
        let got: Vec<_> = futures_executor::block_on(r.lookup_ipv4("example.com").collect());
        assert_eq!(got.len(), 1, "уход пула — не пустой стрим и не тишина");
        let err = got
            .into_iter()
            .next()
            .unwrap()
            .expect_err("обязана быть ошибка");
        assert_ne!(
            err.kind(),
            &ErrorKind::Resolve,
            "отмену пула нельзя путать с отказом DNS — имя может быть в полном порядке"
        );
    }
}
