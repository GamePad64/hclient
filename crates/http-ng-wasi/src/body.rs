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
    // Не конструируется до Task 16: единственный конструктор,
    // `Body::from_incoming`, существует уже сейчас (Transport должен найти
    // готовую точку входа, не переписывать `Body` под себя), но вызовет его
    // только `Transport::execute`. `expect`, а не `allow`: как только
    // Task 16 подключит конструктор, неисполнившееся ожидание лита
    // укажет ровно сюда — атрибут не даст забыть себя убрать.
    #[expect(dead_code, reason = "конструируется в Task 16 (Transport::execute)")]
    Incoming(IncomingResponseBody),
    Done,
}

impl std::fmt::Debug for Body {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.inner {
            Inner::Incoming(_) => f.write_str("Body(incoming)"),
            Inner::Done => f.write_str("Body(done)"),
        }
    }
}

impl Body {
    // См. `Inner::Incoming`: единственный вызов появится в Task 16.
    #[expect(dead_code, reason = "вызывается из Task 16 (Transport::execute)")]
    pub(crate) fn from_incoming(i: IncomingResponseBody) -> Self {
        Self {
            inner: Inner::Incoming(i),
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
                    // `ErrorCode` идёт в `Error::new` как есть, без обёртки:
                    // у него уже есть написанные вручную `Debug`/`Display`/
                    // `core::error::Error` (wasip3 `service.rs`), а
                    // `Error::new` всё равно стирает источник в
                    // `Arc<dyn Error + Send + Sync>` — обёртка не убрала бы
                    // тип из публичного API (он и так туда не попадает), а
                    // только заменила бы честный `Display` на `{:?}` и
                    // закрыла бы даункаст к настоящему типу.
                    self.inner = Inner::Done;
                    Poll::Ready(Some(Err(Error::new(ErrorKind::Body, e))))
                }
                Poll::Ready(None) => {
                    self.inner = Inner::Done;
                    Poll::Ready(None)
                }
                Poll::Pending => Poll::Pending,
            },
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
    fn is_end_stream(&self) -> bool {
        match &self.inner {
            Inner::Done => true,
            Inner::Incoming(i) => i.is_end_stream(),
        }
    }

    /// Форвардит хостовую оценку (`content-length`), а не отбрасывает её:
    /// `IncomingResponseBody` уже вычислила `size_hint()` из заголовков
    /// ответа, пересчитывать нечего.
    fn size_hint(&self) -> SizeHint {
        match &self.inner {
            Inner::Done => SizeHint::with_exact(0),
            Inner::Incoming(i) => i.size_hint(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Всё, что ниже, конструируется без хоста `wasi:http` — `Body::empty()`
    // не трогает `IncomingResponseBody`. Ветки `Inner::Incoming` (реальный
    // `poll_frame`, делегирование `is_end_stream`/`size_hint` живому телу)
    // здесь не проверить: `IncomingResponseBody` создаётся только из
    // `wasip3::http::types::Response` — непрозрачного WIT-ресурса, которому
    // неоткуда взяться без настоящего хоста `wasi:http`. Эти пути остаются
    // непроверенными до Task 16 (или интеграционного теста поверх реального
    // хоста).

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
}
