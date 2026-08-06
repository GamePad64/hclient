//! Тело ответа `wasi:http`.

use bytes::Bytes;
use http_body::{Body as HttpBody, Frame, SizeHint};
use http_ng_core::{Error, ErrorKind};
use std::pin::Pin;
use std::task::{Context, Poll};
use wasip3::http_compat::IncomingResponseBody;

/// Тело ответа `wasi:http`. Читает поток инлайн, без фоновой задачи — значит
/// транспорту не нужна способность `spawn`.
pub struct Body {
    inner: Inner,
}

enum Inner {
    Incoming(IncomingResponseBody),
    /// Буферизованные исходящие байты одним кадром: адаптер `Bytes` ->
    /// `http_body::Body`, нужный `Transport::execute` для передачи
    /// `convert::Payload::Bytes` в `BodyWriter::send_http_body` — тому нужен
    /// именно `http_body::Body`, а не голые байты. Не часть публичного API
    /// тела ОТВЕТА (см. `from_bytes` — `pub(crate)`, не `pub`); занимает
    /// вариант в этом же `enum`, а не отдельный тип, чтобы не заводить
    /// второй `http_body::Body` только ради одного кадра. `None` внутри —
    /// кадр уже отдан.
    Buffered(Option<Bytes>),
    Done,
}

impl std::fmt::Debug for Body {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.inner {
            Inner::Incoming(_) => f.write_str("Body(incoming)"),
            Inner::Buffered(_) => f.write_str("Body(buffered)"),
            Inner::Done => f.write_str("Body(done)"),
        }
    }
}

impl Body {
    pub(crate) fn from_incoming(i: IncomingResponseBody) -> Self {
        Self {
            inner: Inner::Incoming(i),
        }
    }

    /// Адаптер `Bytes` -> `http_body::Body`, ровно один кадр данных. Только
    /// для `Transport::execute` (см. `Inner::Buffered`) — не предназначен
    /// вызывающей стороне, поэтому `pub(crate)`.
    pub(crate) fn from_bytes(b: Bytes) -> Self {
        Self {
            inner: Inner::Buffered(Some(b)),
        }
    }

    /// Пустое тело: ни одного кадра, `is_end_stream()` истинно с самого
    /// начала, `size_hint()` — точный ноль.
    pub fn empty() -> Self {
        Self { inner: Inner::Done }
    }
}

impl HttpBody for Body {
    type Data = Bytes;
    type Error = Error;

    fn poll_frame(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, Error>>> {
        match &mut self.inner {
            Inner::Incoming(i) => match Pin::new(i).poll_frame(cx) {
                Poll::Ready(Some(Ok(f))) => Poll::Ready(Some(Ok(f))),
                Poll::Ready(Some(Err(e))) => {
                    // `ErrorCode` идёт в `Error::new` как есть, без обёртки.
                    // Обёртка была бы лишней машинерией без выигрыша:
                    // `ErrorCode` уже реализует `Debug`/`Display`/
                    // `core::error::Error` вручную (wasip3 `service.rs`) —
                    // причём его собственный `Display` тоже устроен как
                    // `write!(f, "{:?}", self)`, так что обёртка не сделала
                    // бы вывод содержательнее. А поскольку `Error::new`
                    // стирает источник в `Arc<dyn Error + Send + Sync>`,
                    // конкретный тип `ErrorCode` и так не появляется в
                    // публичном API этого крейта что через обёртку, что без
                    // неё; разница только в том, что без обёртки вызывающая
                    // сторона может честно даункаститься до настоящего типа
                    // через `Error::source()`, а обёртка эту возможность
                    // закрывала бы.
                    self.inner = Inner::Done;
                    Poll::Ready(Some(Err(Error::new(ErrorKind::Body, e))))
                }
                Poll::Ready(None) => {
                    self.inner = Inner::Done;
                    Poll::Ready(None)
                }
                Poll::Pending => Poll::Pending,
            },
            // Ровно один кадр, затем `None` навсегда — `Option::take` уже
            // даёт это поведение без отдельного перехода в `Inner::Done`.
            Inner::Buffered(slot) => Poll::Ready(slot.take().map(|b| Ok(Frame::data(b)))),
            Inner::Done => Poll::Ready(None),
        }
    }

    /// Делегирует внутреннему телу вместо того, чтобы переспрашивать
    /// собственное состояние. `IncomingResponseBody` знает о конце потока
    /// из своего состояния раньше, чем мы — только после того, как САМИ
    /// один раз опросим `poll_frame` и увидим `Ready(None)`. Слабая версия
    /// (`matches!(self.inner, Inner::Done)`) — ровно тот дефект хостовой
    /// стороны `act` (`http_body_util::StreamBody` всегда возвращает
    /// `false`), из-за которого гости трапались посреди чтения
    /// HTTP/2-ответов; воспроизводить его здесь было бы бессмысленно —
    /// это и есть весь мотив задачи.
    ///
    /// **Непокрытая юнит-тестами ветка, защищённая интеграционным тестом.**
    /// `Inner::Incoming(i) => i.is_end_stream()` — центральная строка всей
    /// задачи — не покрыта юнит-тестом ниже: у `IncomingResponseBody` нет
    /// конструктора без настоящего хоста `wasi:http`
    /// (`wasip3::http::types::Response` — непрозрачный WIT-ресурс).
    /// Мутационный прогон ревью подтвердил дыру: замена этой ветки на
    /// жёсткий `false` (тот самый баг `act`) не роняет ни один тест из
    /// `#[cfg(test)] mod tests` ниже.
    ///
    /// Task 16 закрывает это `crates/http-ng-wasi/tests/live_roundtrip.rs` +
    /// `examples/live_roundtrip_guest.rs`: реальный запрос через
    /// `WasiHttp::execute` под `wasmtime` (`.cargo/config.toml`,
    /// `runner = "wasmtime run -S http --"`) против мок-сервера, отвечающего
    /// `chunked` с трейлером — именно трейлер даёт то самое окно, где
    /// `i.is_end_stream()` уже `true`, а `self.inner` этого объекта ещё
    /// `Inner::Incoming` (см. doc-комментарий модуля
    /// `live_roundtrip_guest.rs` про то, почему без трейлера такого окна
    /// вообще не существует — с одним лишь `Content-Length` эта ветка и
    /// хардкод-`false` неотличимы даже вживую). Тот же мутационный прогон,
    /// применённый к этому тесту, красный.
    fn is_end_stream(&self) -> bool {
        match &self.inner {
            Inner::Done => true,
            Inner::Buffered(slot) => slot.is_none(),
            Inner::Incoming(i) => i.is_end_stream(),
        }
    }

    /// Форвардит хостовую оценку (`content-length`), а не отбрасывает её:
    /// `IncomingResponseBody` уже вычислила `size_hint()` из заголовков
    /// ответа, пересчитывать нечего — кроме одного случая: `i.size_hint()`
    /// сама никогда не уменьшается (она разово посчитана из
    /// `content-length` и с тех пор не меняется), а `poll_frame` держит
    /// `self.inner` в `Incoming` ещё один вызов ПОСЛЕ того, как
    /// `i.is_end_stream()` уже стало истинным — кадр трейлеров (или
    /// последний `Ready(None)`) увиден внутренним телом, но переход в
    /// `Inner::Done` у нас происходит только на следующем `poll_frame`.
    /// В этом окне некорректированная `i.size_hint()` обещала бы верхнюю
    /// границу байтов, которых больше не будет — переоценка, а не
    /// недооценка, и именно переоценка вредна вызывающей стороне (аллокация
    /// под обещанный остаток, ожидание байтов, которые не придут). См.
    /// `size_hint_honoring_end` — логика вынесена в чистую функцию, чтобы
    /// её можно было проверить без живого `IncomingResponseBody`.
    fn size_hint(&self) -> SizeHint {
        match &self.inner {
            Inner::Done => SizeHint::with_exact(0),
            Inner::Buffered(Some(b)) => SizeHint::with_exact(b.len() as u64),
            Inner::Buffered(None) => SizeHint::with_exact(0),
            Inner::Incoming(i) => size_hint_honoring_end(i.is_end_stream(), i.size_hint()),
        }
    }
}

/// Не даёт устаревшей оценке хоста пережить собственный конец потока — см.
/// комментарий у `Body::size_hint`. Чистая функция специально: сам сценарий
/// (`Inner::Incoming` с `is_end_stream() == true`) недостижим без живого
/// `IncomingResponseBody`, а эта логика — достижима и обязана быть
/// проверена саму по себе.
fn size_hint_honoring_end(is_end_stream: bool, upstream: SizeHint) -> SizeHint {
    if is_end_stream {
        SizeHint::with_exact(0)
    } else {
        upstream
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Всё, что ниже, конструируется без хоста `wasi:http` — `Body::empty()`
    // и `Body::from_bytes()` не трогают `IncomingResponseBody`. Ветки
    // `Inner::Incoming` (реальный `poll_frame`, делегирование
    // `is_end_stream`/`size_hint` живому телу) здесь не проверить:
    // `IncomingResponseBody` создаётся только из
    // `wasip3::http::types::Response` — непрозрачного WIT-ресурса, которому
    // неоткуда взяться без настоящего хоста `wasi:http`. `is_end_stream`, где
    // это особенно чувствительно, отмечено отдельным doc-комментарием на
    // самом методе — там же итог того, закрыл ли Task 16 эту дыру
    // интеграционным тестом под wasmtime или нет. Исключение —
    // `size_hint_honoring_end`: её решение, устаревшую оценку или нет
    // возвращать, вынесено в чистую функцию именно затем, чтобы не делить
    // её судьбу с остальными `Inner::Incoming`-ветками — она проверена ниже.

    #[test]
    fn empty_body_yields_no_frames() {
        let waker = std::task::Waker::noop();
        let mut cx = Context::from_waker(waker);
        let mut b = std::pin::pin!(Body::empty());
        match b.as_mut().poll_frame(&mut cx) {
            Poll::Ready(None) => {}
            other => panic!("expected end of stream, got {other:?}"),
        }
    }

    #[test]
    fn empty_body_reports_end_of_stream_up_front() {
        assert!(
            Body::empty().is_end_stream(),
            "an empty body has nothing left to read from the very start"
        );
    }

    #[test]
    fn empty_body_has_an_exact_zero_size_hint() {
        let hint = Body::empty().size_hint();
        assert_eq!(hint.lower(), 0);
        assert_eq!(hint.upper(), Some(0));
    }

    /// Ревью round 1, Finding 1: `IncomingBody::size_hint()` считается один
    /// раз из `content-length` и сама никогда не уменьшается, а
    /// `Body::poll_frame` держит `self.inner` в `Incoming` ещё один вызов
    /// после того, как внутреннее тело уже сообщило `is_end_stream() ==
    /// true`. Без коррекции в этом окне наружу ушла бы устаревшая верхняя
    /// граница — обещание байтов, которых больше не будет. Ставим заведомо
    /// "богатую" оценку (`4096`), чтобы тест не мог случайно совпасть с
    /// правильным ответом (`0`) при сломанной логике.
    #[test]
    fn size_hint_does_not_over_promise_once_the_stream_has_ended() {
        let mut stale = SizeHint::new();
        stale.set_upper(4096);

        let hint = size_hint_honoring_end(true, stale);

        assert_eq!(hint.lower(), 0);
        assert_eq!(hint.upper(), Some(0));
    }

    /// Симметрия к тесту выше: пока поток не закончился, оценку хоста
    /// нужно передать как есть, а не тоже занулить.
    #[test]
    fn size_hint_passes_through_the_upstream_estimate_mid_stream() {
        let mut mid = SizeHint::new();
        mid.set_upper(4096);

        let hint = size_hint_honoring_end(false, mid);

        assert_eq!(hint.lower(), mid.lower());
        assert_eq!(hint.upper(), mid.upper());
    }

    // `Inner::Buffered` — добавлен Task 16 как адаптер `Bytes` ->
    // `http_body::Body` для `Transport::execute` (`convert::Payload::Bytes`
    // идёт в `BodyWriter::send_http_body`, которому нужен `http_body::Body`,
    // а не голые байты). Не требует хоста — тестируется наравне с `empty()`.

    #[test]
    fn buffered_body_yields_exactly_one_data_frame_then_ends() {
        let waker = std::task::Waker::noop();
        let mut cx = Context::from_waker(waker);
        let mut b = std::pin::pin!(Body::from_bytes(Bytes::from_static(b"abc")));

        match b.as_mut().poll_frame(&mut cx) {
            Poll::Ready(Some(Ok(f))) => {
                assert_eq!(f.into_data().ok().as_deref(), Some(&b"abc"[..]));
            }
            other => panic!("expected one data frame, got {other:?}"),
        }
        match b.as_mut().poll_frame(&mut cx) {
            Poll::Ready(None) => {}
            other => panic!("expected end of stream after the one frame, got {other:?}"),
        }
    }

    #[test]
    fn buffered_body_reports_end_of_stream_only_after_the_frame_is_taken() {
        let mut b = Body::from_bytes(Bytes::from_static(b"abc"));
        assert!(!b.is_end_stream(), "кадр ещё не отдан");

        let waker = std::task::Waker::noop();
        let mut cx = Context::from_waker(waker);
        let _ = std::pin::Pin::new(&mut b).poll_frame(&mut cx);

        assert!(b.is_end_stream(), "единственный кадр уже отдан");
    }

    #[test]
    fn buffered_body_size_hint_is_exact_and_shrinks_to_zero_after_the_frame() {
        let mut b = Body::from_bytes(Bytes::from_static(b"abcd"));
        let before = b.size_hint();
        assert_eq!(before.lower(), 4);
        assert_eq!(before.upper(), Some(4));

        let waker = std::task::Waker::noop();
        let mut cx = Context::from_waker(waker);
        let _ = std::pin::Pin::new(&mut b).poll_frame(&mut cx);

        let after = b.size_hint();
        assert_eq!(after.lower(), 0);
        assert_eq!(after.upper(), Some(0));
    }
}
