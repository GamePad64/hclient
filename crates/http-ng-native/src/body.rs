//! Адаптер `http_ng_core::RequestBody` → `http_body::Body`, ожидаемый
//! `hyper::client::conn::http1::handshake<T, B>`.
//!
//! # Про `Send` и почему `type Error` — это `http_ng_core::Error`, не `BoxError`
//!
//! `handshake` требует `B::Error: Into<Box<dyn StdError + Send + Sync>>` и
//! `B::Data: Send`. Первая версия этого файла читала это как «наш `Error`
//! не подходит — нужен `Box<dyn Error + Send + Sync>` в самом крейте». Это
//! было неверно уже на момент написания: `http_ng_core::Error` держит
//! `Arc<dyn std::error::Error + Send + Sync + 'static>` (поправка C1 ядра, не
//! голый `Arc<dyn Error>`) и сама реализует `Error + Send + Sync + 'static`.
//! Блэнкет-импл стандартной библиотеки (`impl<E: Error + Send + Sync + 'a>
//! From<E> for Box<dyn Error + Send + Sync + 'a>`) закрывает требуемый бонд
//! без единой строчки нашего кода — `assert_bound` в тестах ниже проверяет
//! это напрямую на `OutgoingBody`, а не на голом `http_ng_core::Error`,
//! потому что именно `<OutgoingBody as Body>::Error` — то место, которое
//! реально подставляется в `handshake`.
//!
//! Значит `BoxError` в этом файле не нужен вовсе: заворачивать `Error` в
//! `Box<dyn StdError + Send + Sync>` здесь означало бы терять `ErrorKind` на
//! входе в транспорт — ровно тот дефект (B2 финального ревью вертикали 1),
//! из-за которого `Transport::to_error` в ядре вообще существует. `Bound
//! Send` у hyper — настоящий и подтверждён (`hyper::proto::h1::dispatch::
//! Dispatcher` требует `Bs::Error: Into<Box<dyn StdError + Send + Sync>>`
//! в своей where-клаузе), но он уже удовлетворён `Error` как есть; неоткуда
//! брать `BoxError`, некуда его девать.
//!
//! # Держит ли `ErrorKind` дорогу через `hyper::Error`?
//!
//! Да — без стрингификации, доказано тестом
//! `streaming_bodys_error_kind_survives_hyper_error_source` ниже, а не
//! только чтением исходников. Маршрут (hyper 1.11.0,
//! `src/proto/h1/dispatch.rs`):
//!
//! 1. `Dispatcher::poll_write` вызывает `body.poll_frame(cx)`. Наш
//!    `poll_frame` для `RequestBody::Streaming` отдаёт `Err(e)` с `e:
//!    http_ng_core::Error` как есть — `Self::Error` этого impl'а РОВНО
//!    `http_ng_core::Error`, никакого промежуточного бокса нет.
//! 2. Хук `crate::Error::new_user_body(e)` вызывает `.with(e)`, где `.with<C:
//!    Into<Cause>>` — `Cause = Box<dyn StdError + Send + Sync>`. `e.into()`
//!    использует тот же блэнкет-импл стандартной библиотеки: `Box::new(e)`
//!    как `Box<dyn Error + Send + Sync>`, конкретный тип за vtable не
//!    теряется — это упаковка в `dyn`, не сериализация в строку.
//! 3. `hyper::Error::source()` возвращает `Some(&**cause as &(dyn StdError +
//!    'static))`. `downcast_ref::<http_ng_core::Error>()` на этом
//!    trait-объекте — стандартный инвариантный метод `dyn Error + 'static`
//!    (не требует `Any`, есть у любого `Error`-типа с 1.0) — успешно
//!    восстанавливает исходное значение, `ErrorKind` включительно.
//!
//! Значит правильный маршрут для Task 12/13, когда `SendRequest::
//! send_request` вернёт `Err(hyper::Error)` из-за отказавшего тела:
//! `err.source().and_then(|s| s.downcast_ref::<http_ng_core::Error>())`
//! ДО того, как заворачивать ошибку через `Transport::to_error` —
//! `to_error`'s дефолт сам умеет узнавать «это уже наша `Error`» только
//! когда `Self::Error` сама `http_ng_core::Error`, а `hyper::Error` — чужой
//! тип с нашей `Error` внутри `source()`, а не поверх. Извлечь её оттуда —
//! забота коннектора/драйвера, не этого файла: здесь только доказательство,
//! что извлекать есть что.
//!
//! # Ни один вариант `RequestBody` не превращается в пустое тело молча
//!
//! `Streaming` — не буфер, поэтому пробрасывается как поток, а не собирается
//! в память и не отбрасывается (что уже было дефектом `wasi`-транспорта
//! вертикали 1: стриминговое тело молча становилось пустым запросом при
//! заявленной поддержке стриминга). `Rewindable` — вызывается фабрика и её
//! результат обрабатывается ТЕМ ЖЕ путём, что и любой другой `RequestBody`
//! (рекурсивно через [`Inner::from_request_body`]), а не частичным матчем,
//! который принимает только `Full` и всё остальное схлопывает в `None`:
//! фабрика вправе легально вернуть `RequestBody::Streaming` (см.
//! doc-комментарий `RequestBody::Rewindable` в `http-ng-core`), и такой
//! результат обязан остаться потоком, а не превратиться в пустое тело
//! только потому, что это не `Full`. Тесты `rewindable_*` ниже
//! mutation-проверены: возврат к частичному матчу гасит именно их, не
//! только новый тест на `Streaming`.
//!
//! # `Inner`/`OutgoingBody` вне тестов пока не используются
//!
//! То, что реально их сконструирует (коннектор, HTTP/1-драйвер), приезжает
//! в Task 11–13. `cfg(not(test))`, а не голый `expect`: в тестовой сборке
//! `#[cfg(test)] mod tests` ниже использует их по-настоящему, `dead_code`
//! там не сработает, и непарный `expect` сам обернулся бы предупреждением
//! (`unfulfilled_lint_expectations`) — атрибут ниже ограничен ровно той
//! сборкой, где утверждение "этот код ещё не вызывается" верно.
#![cfg_attr(
    not(test),
    expect(
        dead_code,
        reason = "используются коннектором/драйвером Task 11–13; до тех пор — только тестами этого файла"
    )
)]

use bytes::Bytes;
use http_body::{Body, Frame, SizeHint};
use http_ng_core::{Error, RequestBody};
use std::pin::Pin;
use std::task::{Context, Poll};

/// Буферизованное тело или проброшенный поток — решается один раз, в
/// [`OutgoingBody::from_request_body`], а не на каждый `poll_frame`.
enum Inner {
    /// `None` — тело уже честно пусто (`RequestBody::Empty`, пустой `Full`,
    /// или результат `Rewindable`/фабрики, схлопнувшийся до того же).
    Buffered(Option<Bytes>),
    /// `Unpin + Send` — те же бонды, что у `RequestBody::Streaming` в ядре
    /// (amendment-C2 там же), просто пронесённые через границу крейта:
    /// адаптер оборачивает чужой поток, а не производит собственный
    /// `!Unpin`/`!Send` тип, так что сохранение бондов ничего не стоит.
    Streaming(Box<dyn Body<Data = Bytes, Error = Error> + Unpin + Send>),
}

impl Inner {
    fn from_request_body(body: RequestBody) -> Self {
        match body {
            RequestBody::Empty => Inner::Buffered(None),
            RequestBody::Full(b) if b.is_empty() => Inner::Buffered(None),
            RequestBody::Full(b) => Inner::Buffered(Some(b)),
            // Фабрика легально может вернуть что угодно, включая другой
            // `Rewindable` — тот же путь конвертации разбирает и его, а не
            // частичный матч, который знает только про `Full`.
            RequestBody::Rewindable(f) => Inner::from_request_body(f()),
            RequestBody::Streaming(s) => Inner::Streaming(s),
        }
    }
}

/// Тело запроса для `hyper::client::conn::http1::handshake`.
///
/// `type Error = http_ng_core::Error` — см. doc-комментарий модуля про то,
/// почему это не `Box<dyn StdError + Send + Sync>`.
#[derive(Debug)]
pub(crate) struct OutgoingBody {
    inner: Inner,
}

impl std::fmt::Debug for Inner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Inner::Buffered(b) => f.debug_tuple("Buffered").field(b).finish(),
            Inner::Streaming(_) => f.write_str("Streaming(..)"),
        }
    }
}

impl OutgoingBody {
    pub(crate) fn from_request_body(body: RequestBody) -> Self {
        Self {
            inner: Inner::from_request_body(body),
        }
    }
}

impl Body for OutgoingBody {
    type Data = Bytes;
    type Error = Error;

    fn poll_frame(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, Error>>> {
        // `OutgoingBody` не хранит `!Unpin`-полей напрямую (`Inner::
        // Streaming` — уже `Box<dyn .. + Unpin>`), так что проекция —
        // обычный `get_mut`, без `pin_project`.
        match &mut self.get_mut().inner {
            Inner::Buffered(opt) => Poll::Ready(opt.take().map(|b| Ok(Frame::data(b)))),
            Inner::Streaming(s) => Pin::new(&mut **s).poll_frame(cx),
        }
    }

    fn is_end_stream(&self) -> bool {
        match &self.inner {
            Inner::Buffered(opt) => opt.is_none(),
            Inner::Streaming(s) => s.is_end_stream(),
        }
    }

    fn size_hint(&self) -> SizeHint {
        match &self.inner {
            Inner::Buffered(Some(b)) => SizeHint::with_exact(b.len() as u64),
            Inner::Buffered(None) => SizeHint::with_exact(0),
            // `Rewindable`-фабрика намеренно не имеет собственного
            // size_hint в `RequestBody` (см. её doc-комментарий в
            // `http-ng-core`); проброшенный `Streaming` отдаёт то, что
            // знает конкретный поток, — не обязательно точное.
            Inner::Streaming(s) => s.size_hint(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use http_body_util::BodyExt;
    use http_ng_core::{ErrorKind, RequestBody};

    #[test]
    fn error_type_satisfies_hypers_send_sync_bound() {
        fn assert_bound<B: http_body::Body>()
        where
            B::Error: Into<Box<dyn std::error::Error + Send + Sync>>,
            B::Data: bytes::Buf + Send,
        {
        }
        assert_bound::<OutgoingBody>();
    }

    #[test]
    fn full_body_yields_its_bytes_once() {
        let b = OutgoingBody::from_request_body(RequestBody::Full(bytes::Bytes::from_static(
            b"payload",
        )));
        let collected = futures_executor::block_on(b.collect()).unwrap().to_bytes();
        assert_eq!(&collected[..], b"payload");
    }

    #[test]
    fn empty_body_is_end_stream_immediately() {
        let b = OutgoingBody::from_request_body(RequestBody::Empty);
        assert!(http_body::Body::is_end_stream(&b));
    }

    #[test]
    fn size_hint_is_exact_for_buffered_bodies() {
        let b =
            OutgoingBody::from_request_body(RequestBody::Full(bytes::Bytes::from_static(b"1234")));
        assert_eq!(http_body::Body::size_hint(&b).exact(), Some(4));
    }

    /// Пустой `Full` — тот же честно-пустой случай, что `Empty`, не
    /// отдельный молчаливый спецкейс: `Inner::Buffered(None)` для обоих.
    #[test]
    fn empty_full_body_is_end_stream_immediately() {
        let b = OutgoingBody::from_request_body(RequestBody::Full(bytes::Bytes::new()));
        assert!(http_body::Body::is_end_stream(&b));
        assert_eq!(http_body::Body::size_hint(&b).exact(), Some(0));
    }

    /// `Rewindable`, чья фабрика отдаёт `Full` — обязан пройти как обычный
    /// буфер, не как пустое тело. Mutation-проверка: если
    /// `Inner::from_request_body` вернуть к брифовскому частичному матчу
    /// (`RequestBody::Full(b) if !b.is_empty() => Some(b), _ => None`), этот
    /// тест по-прежнему зелёный (Full — ровно тот вариант, что матч ловит).
    /// Красным его гасит следующий тест, `streaming`-факторный.
    #[test]
    fn rewindable_body_yields_the_factorys_bytes() {
        let b = OutgoingBody::from_request_body(RequestBody::rewindable(|| {
            RequestBody::Full(bytes::Bytes::from_static(b"rewound"))
        }));
        let collected = futures_executor::block_on(b.collect()).unwrap().to_bytes();
        assert_eq!(&collected[..], b"rewound");
    }

    /// Фабрика `Rewindable`, легально возвращающая `Streaming` (см.
    /// doc-комментарий `RequestBody::Rewindable` в `http-ng-core`), обязана
    /// остаться потоком — не схлопнуться в пустое тело, потому что «это не
    /// `Full`». Mutation-проверка: брифовский частичный матч (`RequestBody::
    /// Full(b) if !b.is_empty() => Some(b), _ => None`) на этом входе даёт
    /// `Buffered(None)`, значит `collect()` вернул бы пустые байты, а этот
    /// тест ожидает конкретное содержимое — переключение на частичный матч
    /// красит именно этот тест, не предыдущий.
    #[test]
    fn rewindable_body_may_legally_produce_a_streaming_body() {
        let b = OutgoingBody::from_request_body(RequestBody::rewindable(|| {
            RequestBody::Streaming(Box::new(OneShotStream::data(b"streamed-via-factory")))
        }));
        let collected = futures_executor::block_on(b.collect()).unwrap().to_bytes();
        assert_eq!(&collected[..], b"streamed-via-factory");
    }

    /// Однопроходный поток одного data-фрейма, для тестов. `Option`, а не
    /// готовая очередь: нужен ровно один переход `Some -> None` на
    /// `poll_frame`, второй вызов обязан вернуть `Ready(None)`.
    struct OneShotStream(Option<Result<Frame<Bytes>, Error>>);

    impl OneShotStream {
        fn data(bytes: &'static [u8]) -> Self {
            Self(Some(Ok(Frame::data(Bytes::from_static(bytes)))))
        }
        fn error(e: Error) -> Self {
            Self(Some(Err(e)))
        }
    }

    impl Body for OneShotStream {
        type Data = Bytes;
        type Error = Error;
        fn poll_frame(
            mut self: Pin<&mut Self>,
            _cx: &mut Context<'_>,
        ) -> Poll<Option<Result<Frame<Bytes>, Error>>> {
            Poll::Ready(self.0.take())
        }
        fn is_end_stream(&self) -> bool {
            self.0.is_none()
        }
    }

    /// `Streaming` пробрасывается как поток: `poll_frame` отдаёт кадр
    /// напрямую, без буферизации в память целиком первым делом. Mutation-
    /// проверка: брифовский `RequestBody::Streaming(_) => None` красит этот
    /// тест сразу — `collect()` увидел бы пустое тело вместо `streamed`.
    #[test]
    fn streaming_body_forwards_frames_without_buffering() {
        let b = OutgoingBody::from_request_body(RequestBody::Streaming(Box::new(
            OneShotStream::data(b"streamed"),
        )));
        assert!(!http_body::Body::is_end_stream(&b));
        let collected = futures_executor::block_on(b.collect()).unwrap().to_bytes();
        assert_eq!(&collected[..], b"streamed");
    }

    /// `Streaming`, чей `poll_frame` отдаёт ошибку, обязан пробросить её
    /// как есть, `ErrorKind` включительно — не потерять категорию по дороге
    /// через адаптер. Отдельно от `streaming_bodys_error_kind_survives_
    /// hyper_error_source`: этот тест проверяет сам адаптер, тот — что
    /// категория переживает ЕЩЁ и `hyper::Error`.
    #[test]
    fn streaming_bodys_error_kind_survives_the_adapter() {
        let source = std::io::Error::other("boom");
        let b = OutgoingBody::from_request_body(RequestBody::Streaming(Box::new(
            OneShotStream::error(Error::new(ErrorKind::Body, source)),
        )));
        let e = futures_executor::block_on(b.collect()).unwrap_err();
        assert_eq!(e.kind(), &ErrorKind::Body);
    }

    // --- Доказательство раунд-трипа через настоящий hyper-хендшейк ---
    //
    // Всё ниже — минимальный `hyper::rt::Read + Write` (пишет в никуда,
    // читает `Pending` — см. doc-комментарий `SinkIo::poll_read`), плюс
    // один настоящий `hyper::client::conn::http1::handshake` с настоящим
    // `SendRequest::send_request`. Опрашивается вручную, `Waker::noop()`
    // (стабилен с 1.85 — та же граница MSRV, что у вертикали, приём уже
    // использован в `http-ng-tls::tests::poll_once`): весь путь —
    // рукопожатие, запись заголовков, отказ тела, доставка ошибки в
    // `send_request` — укладывается в один опрос `conn`, затем один опрос
    // `send_request`, без реального экзекьютора и без сна. Ограниченность
    // здесь не числом итераций, а структурная: `poll_once` паникует на
    // `Pending` вместо того, чтобы зависнуть — и раз `SinkIo::poll_write`/
    // `poll_flush`/`poll_shutdown` сами никогда не возвращают `Pending`, а
    // единственный `Pending` (`poll_read`) по анализу ниже не блокирует
    // прогресс, `poll_once` на `conn`/`send_request` не паникует.
    use std::io;

    fn poll_once<F: std::future::Future>(fut: std::pin::Pin<&mut F>) -> F::Output {
        let mut cx = Context::from_waker(std::task::Waker::noop());
        match fut.poll(&mut cx) {
            Poll::Ready(v) => v,
            Poll::Pending => panic!("SinkIo не должен вызывать Pending нигде на этом пути"),
        }
    }

    /// Раковина: принимает и отбрасывает любую запись; на чтение всегда
    /// отвечает `Pending` (см. doc-комментарий `poll_read` — почему не
    /// мгновенный EOF). Сырых байт ответа этому тесту не нужно —
    /// интересует только то, что происходит с ошибкой ТЕЛА запроса.
    #[derive(Default)]
    struct SinkIo;

    impl hyper::rt::Read for SinkIo {
        fn poll_read(
            self: Pin<&mut Self>,
            _cx: &mut Context<'_>,
            _buf: hyper::rt::ReadBufCursor<'_>,
        ) -> Poll<io::Result<()>> {
            // `Pending`, не мгновенный EOF — и вот почему это не расходится
            // с "`SinkIo` никогда не блокирует" из комментария выше модуля.
            //
            // Свежее клиентское соединение стартует в `KA::Busy` (hyper
            // 1.11.0, `proto/h1/conn.rs`, `Conn::new`), то есть ДО первой
            // записи `poll_read`(диспетчерский, `proto/h1/dispatch.rs`)
            // падает в ветку `poll_read_keep_alive` → `require_empty_read`:
            // там ЛЮБОЙ немедленный EOF на этой стадии интерпретируется как
            // "нашли неожиданный EOF на занятом соединении" —
            // `crate::Error::new_incomplete()` — и именно это первая версия
            // этого теста и поймала (`hyper::Error(Canceled,
            // hyper::Error(IncompleteMessage))` вместо ошибки тела).
            // Реальный сокет тут вернул бы `Pending` (данных ещё нет, но
            // соединение не закрыто), не `Ready(EOF)` — `SinkIo` обязан
            // повторить это же различие, а не только "никогда не блокирует
            // по-настоящему".
            //
            // `Pending` здесь не блокирует раунд-трип: `Dispatcher::
            // poll_loop` (hyper 1.11.0) вызывает `let _ =
            // self.poll_read(cx)?;` — `?` на `Poll<Result<T, E>>>`
            // пробрасывает наружу только `Err`, а `Pending` возвращает как
            // ЗНАЧЕНИЕ (`Poll<T>`), которое здесь тут же отбрасывается
            // `let _ =`. Проверено отдельно, вне этого дерева: минимальный
            // повтор `fn f() -> Poll<Result<u32,String>> { Poll::Pending }`,
            // вызванный как `let _ = f()?;` внутри функции, возвращающей
            // `Poll<Result<(), String>>>`, продолжает выполнение ПОСЛЕ `?`
            // — не короткое замыкание. Значит `poll_write` (и с ним отказ
            // тела) вызывается в ТОМ ЖЕ проходе `poll_loop`, что и этот
            // `Pending`-чтение, что тест ниже и подтверждает получением
            // ожидаемой ошибки без единого дополнительного опроса.
            Poll::Pending
        }
    }

    impl hyper::rt::Write for SinkIo {
        fn poll_write(
            self: Pin<&mut Self>,
            _cx: &mut Context<'_>,
            buf: &[u8],
        ) -> Poll<io::Result<usize>> {
            Poll::Ready(Ok(buf.len()))
        }
        fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
            Poll::Ready(Ok(()))
        }
        fn poll_shutdown(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
            Poll::Ready(Ok(()))
        }
    }

    #[test]
    fn streaming_bodys_error_kind_survives_hyper_error_source() {
        let source = std::io::Error::other("stream broke");
        let original = Error::new(ErrorKind::Body, source);

        let body = OutgoingBody::from_request_body(RequestBody::Streaming(Box::new(
            OneShotStream::error(original.clone()),
        )));

        let req = http::Request::builder()
            .method("POST")
            .uri("/")
            .header("host", "example.invalid")
            .body(body)
            .unwrap();

        let handshake = hyper::client::conn::http1::handshake::<_, OutgoingBody>(SinkIo);
        let mut handshake = std::pin::pin!(handshake);
        let (mut sender, conn) =
            poll_once(handshake.as_mut()).expect("handshake never blocks on SinkIo");

        // Ставится в очередь синхронно внутри `send_request` (до первого
        // `.await`), поэтому порядок «сначала опросить conn, потом этот
        // future» ниже гарантированно видит уже поставленный запрос.
        let send_fut = sender.send_request(req);
        let mut send_fut = std::pin::pin!(send_fut);

        let mut conn = std::pin::pin!(conn);
        // `conn` в этом сценарии тоже резолвится за один опрос: запись
        // заголовков, отказ тела и доставка ошибки в канал `send_request`
        // происходят синхронно внутри одного `poll_write` — см.
        // doc-комментарий модуля.
        let _ = poll_once(conn.as_mut());

        let err = match poll_once(send_fut.as_mut()) {
            Ok(_) => panic!("отказавшее тело обязано провалить send_request"),
            Err(e) => e,
        };

        let recovered = std::error::Error::source(&err)
            .and_then(|s| s.downcast_ref::<Error>())
            .unwrap_or_else(|| {
                panic!("hyper::Error::source() обязан отдать нашу http_ng_core::Error, получено: {err:?}")
            });
        assert_eq!(
            recovered.kind(),
            &ErrorKind::Body,
            "ErrorKind обязан пережить путь через hyper::Error::new_user_body/.with()"
        );
    }
}
