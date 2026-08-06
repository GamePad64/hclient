use bytes::Bytes;
use std::sync::Arc;

/// Можно ли переиграть это тело — известно **до** отправки.
///
/// `reqwest::Request::try_clone() -> Option<Request>` отвечает на тот же вопрос
/// после того, как retry-слой уже решил ретраить, и поэтому молча выключает
/// ретраи на стриминговых телах.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RetryKind {
    /// Переигрывается бесплатно.
    Free,
    /// Переигрывается вызовом фабрики.
    ViaFactory,
    /// Переиграть нельзя.
    Impossible,
}

/// Границы `Send + Sync` — задокументированное исключение из инварианта
/// крейта "нигде не объявляем `Send`/`Sync`" (spec amendment C2, сестра C1
/// у [`crate::Error`]). Без них `RequestBody` был бы `!Send`, значит
/// `http::Request<RequestBody>` был бы `!Send`, значит футура, которую
/// возвращает `Transport::execute`, была бы `!Send` для любого бэкенда —
/// `tokio::spawn(client.get(u).send())` не собрался бы никогда. `Sync`
/// нужен только здесь, у `Arc`: `Arc<T>: Send` требует `T: Send + Sync`,
/// тогда как `Box<T>: Send` (см. [`RequestBody::Streaming`]) требует лишь
/// `T: Send`.
pub type RewindFactory = Arc<dyn Fn() -> RequestBody + Send + Sync>;

/// Тело запроса с явным контрактом переигрывания.
#[derive(Default)]
pub enum RequestBody {
    #[default]
    Empty,
    Full(Bytes),
    /// Переигрывается вызовом фабрики.
    ///
    /// **Контракт фабрики.** Она обязана быть чистой: каждый вызов должен
    /// производить тело, эквивалентное предыдущему (то же содержимое, тот же
    /// размер). Фабрика со скрытым состоянием, отдающая на каждый вызов
    /// разное тело — это лежащий на поверхности, но недокументированный
    /// источник багов, поэтому `size_hint()` для этого варианта намеренно
    /// возвращает `None`: гадать по первому вызову опасно, если контракт
    /// нарушен.
    ///
    /// Фабрика вправе легально вернуть `RequestBody::Streaming` — это не
    /// живая ложь, потому что `retry_kind()` и `rewind()` всегда
    /// вычисляются заново по тому объекту, который сейчас лежит внутри
    /// `RequestBody`, а не кэшируются на момент создания `Rewindable`.
    /// **Инвариант, важный для retry-слоя (Task 8): всегда переспрашивай
    /// `retry_kind()` у того тела, которое сейчас на руках, и никогда не
    /// кэшируй его через `rewind()`.**
    Rewindable(RewindFactory),
    /// Однопроходное тело. Конкретный поток задаёт транспорт; в v0.1 ядру
    /// достаточно знать, что переиграть его нельзя.
    ///
    /// `+ Send` — то же исключение C2, что у [`RewindFactory`]: `Box<T>: Send`
    /// требует только `T: Send`, `Sync` здесь не нужен.
    Streaming(Box<dyn http_body::Body<Data = Bytes, Error = crate::Error> + Unpin + Send>),
}

impl std::fmt::Debug for RequestBody {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RequestBody::Empty => f.write_str("Empty"),
            RequestBody::Full(b) => write!(f, "Full({} bytes)", b.len()),
            RequestBody::Rewindable(_) => f.write_str("Rewindable(..)"),
            RequestBody::Streaming(_) => f.write_str("Streaming(..)"),
        }
    }
}

impl RequestBody {
    pub fn rewindable<F>(f: F) -> Self
    where
        F: Fn() -> RequestBody + Send + Sync + 'static,
    {
        RequestBody::Rewindable(Arc::new(f))
    }

    pub fn retry_kind(&self) -> RetryKind {
        match self {
            RequestBody::Empty | RequestBody::Full(_) => RetryKind::Free,
            RequestBody::Rewindable(_) => RetryKind::ViaFactory,
            RequestBody::Streaming(_) => RetryKind::Impossible,
        }
    }

    pub fn rewind(&self) -> Option<RequestBody> {
        match self {
            RequestBody::Empty => Some(RequestBody::Empty),
            RequestBody::Full(b) => Some(RequestBody::Full(b.clone())),
            RequestBody::Rewindable(f) => Some(f()),
            RequestBody::Streaming(_) => None,
        }
    }

    pub fn size_hint(&self) -> Option<u64> {
        match self {
            RequestBody::Empty => Some(0),
            RequestBody::Full(b) => Some(b.len() as u64),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bytes::Bytes;

    #[test]
    fn replayability_is_knowable_before_sending() {
        assert_eq!(RequestBody::Empty.retry_kind(), RetryKind::Free);
        assert_eq!(
            RequestBody::Full(Bytes::from_static(b"x")).retry_kind(),
            RetryKind::Free
        );
    }

    #[test]
    fn rewindable_replays_through_factory() {
        let b = RequestBody::rewindable(|| RequestBody::Full(Bytes::from_static(b"same")));
        assert_eq!(b.retry_kind(), RetryKind::ViaFactory);
        let again = b.rewind().expect("rewindable must rewind");
        assert!(matches!(again, RequestBody::Full(ref x) if &x[..] == b"same"));
    }

    #[test]
    fn full_rewind_preserves_the_payload() {
        let b = RequestBody::Full(Bytes::from_static(b"abc"));
        match b.rewind().expect("Full реиграется") {
            RequestBody::Full(x) => assert_eq!(&x[..], b"abc"),
            other => panic!("ожидался Full, получен {other:?}"),
        }
    }

    #[test]
    fn a_factory_survives_repeated_replays() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let calls = Arc::new(AtomicUsize::new(0));
        let c = calls.clone();
        let b = RequestBody::rewindable(move || {
            c.fetch_add(1, Ordering::SeqCst);
            RequestBody::Full(Bytes::from_static(b"same"))
        });
        for _ in 0..3 {
            let again = b.rewind().expect("rewindable реиграется");
            assert!(matches!(again, RequestBody::Full(ref x) if &x[..] == b"same"));
            assert_eq!(
                b.retry_kind(),
                RetryKind::ViaFactory,
                "вид не меняется от повторов"
            );
        }
        assert_eq!(calls.load(Ordering::SeqCst), 3);
    }

    /// Пара `Empty`/`Full` — единственные варианты, для которых размер
    /// известен заранее. `Rewindable` и `Streaming` покрыты отдельно
    /// (`rewindable_replays_through_factory`,
    /// `streaming_is_honest_about_being_unreplayable`) и туда не входят —
    /// имя теста не должно обещать охват, которого здесь нет.
    #[test]
    fn size_hint_is_known_for_empty_and_full_bodies() {
        assert_eq!(RequestBody::Empty.size_hint(), Some(0));
        assert_eq!(
            RequestBody::Full(Bytes::from_static(b"abcd")).size_hint(),
            Some(4)
        );
    }

    /// Тело без единого байта в буфере: `poll_frame` сразу возвращает
    /// `Ready(None)`. Нужно только для того, чтобы сконструировать
    /// `RequestBody::Streaming` в тестах — конкретный транспорт задаёт
    /// свою реализацию.
    struct EmptyStream;
    impl http_body::Body for EmptyStream {
        type Data = Bytes;
        type Error = crate::Error;
        fn poll_frame(
            self: std::pin::Pin<&mut Self>,
            _: &mut std::task::Context<'_>,
        ) -> std::task::Poll<Option<Result<http_body::Frame<Bytes>, Self::Error>>> {
            std::task::Poll::Ready(None)
        }
    }

    #[test]
    fn streaming_is_honest_about_being_unreplayable() {
        let b = RequestBody::Streaming(Box::new(EmptyStream));
        assert_eq!(b.retry_kind(), RetryKind::Impossible);
        assert!(b.rewind().is_none(), "должен вернуть None, а не паниковать");
        assert_eq!(b.size_hint(), None);
    }

    /// Свойство, ради которого существует поправка C2: без `+ Send + Sync`
    /// на обоих объектах-трейтах `RequestBody` был бы `!Send`, и вместе с
    /// ней `http::Request<RequestBody>` — а значит футура, которую вернёт
    /// `Transport::execute`, не смогла бы попасть в `tokio::spawn` ни для
    /// одного бэкенда.
    #[test]
    fn request_body_is_send_so_transport_futures_can_be_spawned() {
        fn assert_send<T: Send>() {}
        assert_send::<RequestBody>();
        assert_send::<http::Request<RequestBody>>();
    }
}
