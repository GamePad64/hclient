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
    /// **Непроверенная ветка.** `Inner::Incoming(i) => i.is_end_stream()` —
    /// центральная строка всей задачи — не покрыта юнит-тестом: у
    /// `IncomingResponseBody` нет конструктора без настоящего хоста
    /// `wasi:http` (`wasip3::http::types::Response` — непрозрачный
    /// WIT-ресурс). Мутационный прогон ревью подтвердил дыру: замена этой
    /// ветки на жёсткий `false` (тот самый баг `act`) не роняет ни один
    /// тест из `#[cfg(test)] mod tests` ниже. Покрытие появится только в
    /// Task 16, который поднимает `.cargo/config.toml` с
    /// `runner = "wasmtime run -S http --"` и гоняет
    /// `cargo test -p http-ng-wasi --target wasm32-wasip2` под настоящим
    /// хостом — до этого момента риск регрессии здесь ничем не защищён.
    fn is_end_stream(&self) -> bool {
        match &self.inner {
            Inner::Done => true,
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
    // не трогает `IncomingResponseBody`. Ветки `Inner::Incoming` (реальный
    // `poll_frame`, делегирование `is_end_stream`/`size_hint` живому телу)
    // здесь не проверить: `IncomingResponseBody` создаётся только из
    // `wasip3::http::types::Response` — непрозрачного WIT-ресурса, которому
    // неоткуда взяться без настоящего хоста `wasi:http`. Эти пути остаются
    // непроверенными до Task 16 (или интеграционного теста поверх реального
    // хоста); `is_end_stream`, где это особенно чувствительно, отмечено
    // отдельным doc-комментарием на самом методе. Исключение —
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
}
