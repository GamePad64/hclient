//! Подключаемое разрешение имён.
//!
//! Раздельные стримы по семействам, а не `Vec<SocketAddr>`: RFC 8305
//! требует начинать соединяться по AAAA, не дожидаясь A —
//! `http-ng-proto::happy_eyeballs::Scheduler` (Task 5) кормится результатами
//! по мере поступления, а не одним блоком после того, как резолвер закончил
//! оба семейства. Это единственная причина, по которой `Resolve` возвращает
//! `Stream`, а не `Future<Output = Vec<_>>`: ничто в трейте не заставляет
//! вызывающую сторону дожидаться конца стрима или собирать его в `Vec`
//! перед тем, как начать коннектиться к первому адресу.
//!
//! **Гарантия порядка внутри стрима: её нет.** `Resolve` не обещает, что
//! адреса одного семейства идут в порядке RFC 6724 §6 (Destination Address
//! Selection) — резолвер вправе отдавать их в порядке DNS-ответа, порядке
//! кэша или любом другом. Сортировка — работа вызывающей стороны (коннектор,
//! Task 11), ДО того, как результаты попадут в
//! `Scheduler::offer_v4`/`offer_v6`. `Scheduler` документирует то же самое
//! требование с другого конца шва (doc-комментарий структуры `Scheduler`:
//! «Сортировка... — забота вызывающей стороны, до `offer_*`; здесь её нет
//! намеренно»): ни резолвер, ни планировщик сортировкой не занимаются,
//! чтобы это не случилось дважды и не потерялось между ними в зазоре,
//! который иначе некому было бы закрыть.
//!
//! **SVCB — способность, а не факт.** `lookup_svcb` несёт тело по умолчанию,
//! возвращающее пустой стрим, — иначе `getaddrinfo`, `wasi:http` и
//! embedded-резолверы не смогли бы реализовать трейт вовсе, притом что
//! реального SVCB/HTTPS-запроса ни один из них не выполняет. Но пустой
//! стрим сам по себе неоднозначен: он мог бы значить и «этот резолвер не
//! умеет SVCB», и «резолвер спросил и получил ноль записей» — две разные
//! вещи, которые вызывающая сторона не обязана путать (тот же принцип, что
//! развёл `RedirectSupport::None` и `Transparent` в `http-ng-core`:
//! способность, которая врёт о своём отсутствии или о своём наличии, хуже
//! способности, которой просто нет). `supports_svcb()` — отдельная точка
//! входа для этого различия: резолвер, не умеющий SVCB, оставляет её
//! дефолтной `false` и наследует дефолтный `lookup_svcb`; резолвер, который
//! умеет, обязан переопределить ОБА метода вместе — переопределить только
//! `lookup_svcb` значит снова смешать «не умею» с «умею и не нашёл» для
//! всех, кто читает только `supports_svcb()`.
#![forbid(unsafe_code)]

use bytes::Bytes;
use futures_core::Stream;
use http_ng_core::Error;
use std::marker::PhantomData;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::Duration;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedAddr {
    pub addr: IpAddr,
    pub ttl: Option<Duration>,
}

/// RFC 9460 HTTPS/SVCB. `alpn` даёт обнаружение h3 без Alt-Svc,
/// `ech_config_list` кормит `rustls::EchConfig` напрямую.
///
/// Заложено с первого дня: если зафиксировать резолвер на `SocketAddr`, ECH и
/// h3-discovery закрыты навсегда без ломающего изменения.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SvcbEndpoint {
    pub priority: u16,
    pub target: String,
    pub alpn: Vec<Vec<u8>>,
    pub port: Option<u16>,
    pub ipv4hint: Vec<Ipv4Addr>,
    pub ipv6hint: Vec<Ipv6Addr>,
    pub ech_config_list: Option<Bytes>,
}

/// Поток, который сразу говорит «ничего нет».
///
/// Существует, чтобы `Resolve::lookup_svcb` имело тело по умолчанию, не
/// притаскивая `futures-util` в зависимости библиотеки: единственное, что
/// оттуда требовалось, — `stream::empty()`. `futures-core` даёт один крейт
/// (сам трейт `Stream`); `futures-util` тянет за собой ещё четыре
/// (`futures-task`, `pin-project-lite`, `slab` и себя саму) ради одной
/// функции, чьё тело — восемь строк ниже. `futures-util` по-прежнему нужен
/// тестам этого крейта (`stream::iter`, `StreamExt`) и живёт в
/// `[dev-dependencies]`, где не попадает в граф зависимостей потребителя.
struct EmptyStream<T>(PhantomData<T>);

impl<T> EmptyStream<T> {
    const fn new() -> Self {
        Self(PhantomData)
    }
}

impl<T> Stream for EmptyStream<T> {
    type Item = T;

    fn poll_next(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        Poll::Ready(None)
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        (0, Some(0))
    }
}

pub trait Resolve {
    /// A-записи. Каждый элемент стрима независим: ошибка на одном не
    /// обязана останавливать остальные (например, резолвер с несколькими
    /// апстримами может сообщить о частичном отказе и продолжить).
    fn lookup_ipv4(&self, name: &str) -> impl Stream<Item = Result<ResolvedAddr, Error>>;
    /// AAAA-записи. Отдельный стрим от `lookup_ipv4`, не вариант одного
    /// перечисления и не общий `Vec` — RFC 8305 §3/§4 требует начинать
    /// попытки по IPv6, не дожидаясь ответа по IPv4, а раздельные стримы —
    /// единственная форма, которая это позволяет без дополнительного
    /// разбора на стороне вызывающего.
    fn lookup_ipv6(&self, name: &str) -> impl Stream<Item = Result<ResolvedAddr, Error>>;

    /// Умеет ли резолвер SVCB/HTTPS-запросы вообще.
    ///
    /// Дефолт `false` образует пару с дефолтным `lookup_svcb` ниже: вместе
    /// они говорят «эта способность отсутствует», а не «есть, но нашлось
    /// ноль записей». Резолвер, дающий настоящий ответ на SVCB, обязан
    /// переопределить оба метода разом.
    fn supports_svcb(&self) -> bool {
        false
    }

    /// SVCB/HTTPS-записи (RFC 9460). Дефолт — пустой стрим: без него
    /// `getaddrinfo`-обёртка, `wasi:http` и embedded-резолверы, у которых
    /// нет доступа к сырым DNS-записям, не смогли бы реализовать трейт
    /// вовсе. Пустой стрим из дефолта и пустой стрим от резолвера,
    /// который реально спросил SVCB и ничего не нашёл, неразличимы на этом
    /// уровне намеренно — различие вынесено в `supports_svcb()` выше;
    /// вызывающая сторона, которой важна эта разница, обязана спросить его,
    /// а не выводить ответ из пустоты стрима.
    fn lookup_svcb(&self, _name: &str) -> impl Stream<Item = Result<SvcbEndpoint, Error>> {
        EmptyStream::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures_util::StreamExt;

    struct Static;
    impl Resolve for Static {
        fn lookup_ipv4(&self, _: &str) -> impl Stream<Item = Result<ResolvedAddr, Error>> {
            futures_util::stream::iter(vec![Ok(ResolvedAddr {
                addr: "127.0.0.1".parse().unwrap(),
                ttl: None,
            })])
        }
        fn lookup_ipv6(&self, _: &str) -> impl Stream<Item = Result<ResolvedAddr, Error>> {
            futures_util::stream::empty()
        }
        // lookup_svcb / supports_svcb намеренно не реализованы — дефолты
        // обязаны работать без них.
    }

    #[test]
    fn svcb_has_a_default_returning_empty() {
        let got: Vec<_> = futures_executor::block_on(Static.lookup_svcb("x").collect());
        assert!(
            got.is_empty(),
            "иначе getaddrinfo, wasi и embedded не смогли бы реализовать трейт"
        );
    }

    #[test]
    fn svcb_default_capability_is_false() {
        // Дефолт `lookup_svcb` (пусто) и дефолт `supports_svcb` (false)
        // обязаны совпадать по смыслу: пустой стрим без этого метода —
        // ложь по умолчанию, а не отсутствие ответа.
        assert!(
            !Static.supports_svcb(),
            "дефолт обязан явно сказать «не умею», а не молчать об этом"
        );
    }

    #[test]
    fn families_are_separate_streams() {
        let v4: Vec<_> = futures_executor::block_on(Static.lookup_ipv4("x").collect());
        let v6: Vec<_> = futures_executor::block_on(Static.lookup_ipv6("x").collect());
        assert_eq!(v4.len(), 1);
        assert_eq!(v6.len(), 0, "по AAAA надо коннектиться, не дожидаясь A");
    }

    struct Two;
    impl Resolve for Two {
        fn lookup_ipv4(&self, _: &str) -> impl Stream<Item = Result<ResolvedAddr, Error>> {
            futures_util::stream::iter(vec![
                Ok(ResolvedAddr {
                    addr: "10.0.0.1".parse().unwrap(),
                    ttl: None,
                }),
                Ok(ResolvedAddr {
                    addr: "10.0.0.2".parse().unwrap(),
                    ttl: None,
                }),
            ])
        }
        fn lookup_ipv6(&self, _: &str) -> impl Stream<Item = Result<ResolvedAddr, Error>> {
            futures_util::stream::empty()
        }
    }

    #[test]
    fn items_are_consumable_one_at_a_time_without_collecting() {
        // Стрим — не Vec: вызывающая сторона может забрать первый адрес и
        // (в реальном коннекторе) начать коннектиться, не дожидаясь второго
        // и не вызывая `.collect()` на всём стриме.
        let mut s = std::pin::pin!(Two.lookup_ipv4("x"));
        let first = futures_executor::block_on(s.next()).unwrap().unwrap();
        assert_eq!(first.addr, "10.0.0.1".parse::<IpAddr>().unwrap());
        // Второй элемент всё ещё лежит в стриме и не был затронут первым
        // `.next()` — доказывает, что забор первого не потребовал остального.
        let second = futures_executor::block_on(s.next()).unwrap().unwrap();
        assert_eq!(second.addr, "10.0.0.2".parse::<IpAddr>().unwrap());
    }

    struct WithSvcb;
    impl Resolve for WithSvcb {
        fn lookup_ipv4(&self, _: &str) -> impl Stream<Item = Result<ResolvedAddr, Error>> {
            futures_util::stream::empty()
        }
        fn lookup_ipv6(&self, _: &str) -> impl Stream<Item = Result<ResolvedAddr, Error>> {
            futures_util::stream::empty()
        }
        fn supports_svcb(&self) -> bool {
            true
        }
        fn lookup_svcb(&self, _: &str) -> impl Stream<Item = Result<SvcbEndpoint, Error>> {
            futures_util::stream::iter(vec![Ok(SvcbEndpoint {
                priority: 1,
                target: "svc.example".into(),
                alpn: vec![b"h3".to_vec()],
                port: Some(443),
                ipv4hint: vec![],
                ipv6hint: vec![],
                ech_config_list: None,
            })])
        }
    }

    #[test]
    fn a_resolver_implementing_svcb_reports_the_capability_and_the_data_together() {
        // Различие из doc-комментария `supports_svcb` работает в обе
        // стороны: резолвер, умеющий SVCB, обязан заявить об этом через
        // `supports_svcb()` И вернуть настоящие записи через `lookup_svcb`.
        assert!(WithSvcb.supports_svcb());
        let got: Vec<_> = futures_executor::block_on(WithSvcb.lookup_svcb("x").collect());
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].as_ref().unwrap().target, "svc.example");
    }
}
