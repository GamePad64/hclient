use std::sync::Arc;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    Connect,
    FirstByte,
    BetweenBytes,
    Total,
}

/// Категория ошибки. Существует, чтобы потребителю не приходилось
/// классифицировать ошибки подстрочным матчингом по `Display`.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ErrorKind {
    Resolve,
    Connect,
    Tls,
    Redirect,
    Timeout(Phase),
    Body,
    Decode,
    Status,
    Unsupported,
    /// Способность, стоящая за отказавшей операцией, ушла из-под неё раньше,
    /// чем та успела выполниться — как правило, рантайм завершает работу,
    /// пока задача ещё стояла в очереди (см. `http_ng_rt::Cancelled`,
    /// возвращаемая `Blocking::run`, вертикаль 2, Task 1, `amendment-C5`).
    ///
    /// Отдельный вариант, а не `Other`: `Other` — честный ответ для
    /// СОБСТВЕННО непрозрачной ошибки бэкенда (дефолт
    /// `Transport::to_error`, когда бэкенду нечего сказать о категории).
    /// Отмена — противоположность непрозрачности: это заранее известное,
    /// уже типизированное условие (`Cancelled` — не строка и не код ошибки
    /// ОС, а конкретный тип), которое встретится у КАЖДОГО будущего
    /// потребителя способности `Blocking`, а не один раз у одного бэкенда.
    /// Смешивать её ни с `Other`, ни тем более с категорией самой отказавшей
    /// операции (например, `Resolve` для DNS-резолвера поверх `Blocking`,
    /// см. `http-ng-dns-system`) нельзя по той же причине, по которой
    /// `Resolve` и `Other` не смешивают друг с другом: вызывающая сторона
    /// обязана уметь отличить «эта попытка отказала по существу» от «эта
    /// попытка не завершилась, потому что рантайм завершает работу» без
    /// downcast — просто сравнив `kind()`.
    Cancelled,
    Other,
}

/// `Clone` намеренно: непрозрачная и неклонируемая ошибка reqwest — источник
/// постоянных жалоб (reqwest#1053).
///
/// `source` обязан быть `Send + Sync` — это единственное документированное
/// исключение из инварианта крейта "ни одного объявленного бонда
/// `Send`/`Sync`". Без этого бонда `Arc<dyn Error>` стирает auto-traits
/// источника, и `Error` (а вместе с ней future, который возвращает
/// `Client::execute`) была бы `!Send` для любого транспорта —
/// `tokio::spawn(client.get(u).send())` не компилировался бы никогда. Все
/// три бэкенда v0.1 (hyper, wasi:http, browser fetch без
/// `target_feature = "atomics"`) уже производят `Send + Sync`-ошибки, так
/// что это фиксация факта, а не новое ограничение; транспорт с
/// принципиально `!Send`-ошибкой не сможет использовать эту обёртку.
#[derive(Debug, Clone)]
pub struct Error {
    kind: ErrorKind,
    source: Arc<dyn std::error::Error + Send + Sync + 'static>, // send-bound-exception: amendment-C1
}

impl Error {
    pub fn new<E>(kind: ErrorKind, source: E) -> Self
    where
        E: std::error::Error + Send + Sync + 'static, // send-bound-exception: amendment-C1
    {
        Self {
            kind,
            source: Arc::new(source),
        }
    }
    pub fn kind(&self) -> &ErrorKind {
        &self.kind
    }
    pub fn is_timeout(&self) -> bool {
        matches!(self.kind, ErrorKind::Timeout(_))
    }
    pub fn is_redirect(&self) -> bool {
        matches!(self.kind, ErrorKind::Redirect)
    }
    pub fn is_connect(&self) -> bool {
        matches!(self.kind, ErrorKind::Connect)
    }
    pub fn is_unsupported(&self) -> bool {
        matches!(self.kind, ErrorKind::Unsupported)
    }
    pub fn is_cancelled(&self) -> bool {
        matches!(self.kind, ErrorKind::Cancelled)
    }
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:?}: {}", self.kind, self.source)
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&*self.source)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug)]
    struct Src;
    impl std::fmt::Display for Src {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            write!(f, "boom")
        }
    }
    impl std::error::Error for Src {}

    #[test]
    fn preserves_kind_and_source_without_stringifying() {
        let e = Error::new(ErrorKind::Resolve, Src);
        assert_eq!(e.kind(), &ErrorKind::Resolve);
        // Источник доступен целиком — не подстрокой сообщения.
        let src = std::error::Error::source(&e).unwrap();
        assert!(src.downcast_ref::<Src>().is_some());
    }

    #[test]
    fn is_clone_which_reqwest_error_is_not() {
        let e = Error::new(ErrorKind::Connect, Src);
        let c = e.clone();
        assert_eq!(c.kind(), &ErrorKind::Connect);
        // Клон должен шарить один и тот же source, а не копировать и не
        // терять его: указатели на источник у оригинала и клона совпадают.
        let a = std::error::Error::source(&e).unwrap() as *const dyn std::error::Error;
        let b = std::error::Error::source(&c).unwrap() as *const dyn std::error::Error;
        assert!(std::ptr::eq(a, b));
    }

    #[test]
    fn predicates_agree_with_kind() {
        assert!(Error::new(ErrorKind::Timeout(Phase::Connect), Src).is_timeout());
        assert!(Error::new(ErrorKind::Redirect, Src).is_redirect());
        assert!(Error::new(ErrorKind::Connect, Src).is_connect());
        assert!(!Error::new(ErrorKind::Body, Src).is_connect());
        assert!(Error::new(ErrorKind::Unsupported, Src).is_unsupported());
        assert!(!Error::new(ErrorKind::Body, Src).is_unsupported());
        assert!(Error::new(ErrorKind::Cancelled, Src).is_cancelled());
        // Отмена — не отказ DNS и не непрозрачная "прочая" ошибка: обе
        // проверки нужны, одной было бы недостаточно, чтобы поймать
        // регресс, спутавший `Cancelled` с любым из этих двух соседей.
        assert!(!Error::new(ErrorKind::Resolve, Src).is_cancelled());
        assert!(!Error::new(ErrorKind::Other, Src).is_cancelled());
    }

    // `Error: Send + Sync` (spec amendment C1) — moved to
    // `crates/http-ng-core/tests/shape.rs` per amendment C3: a bare
    // `fn _assert<T: Send + Sync>() {}` inside `src` matches the
    // `no-declared-send` guard's own pattern. Fix round 1 for Task 12
    // dropped this file's blanket exclusion from that guard in favour of
    // per-line `send-bound-exception` markers, which turned this
    // previously-shielded compile-time assertion into a false positive.
    // Relocating it (rather than marking it) shrinks the guard's blind
    // spot instead of growing it — the assertion needs zero exception
    // once it's not sharing a file with the two lines that actually are
    // the exception.
}
