use crate::{Capabilities, RequestBody};
use bytes::Bytes;
use std::future::Future;

/// Единственный шов между http-ng и реальным HTTP.
///
/// Форма взята от `wasi:http/client.send` — самого бедного из ambient-API.
/// Всё, что богаче, деградирует к ней чисто; обратное неверно.
///
/// Ни `poll_ready`, ни `&mut self`, ни `Send`: Send-ность выводится
/// auto-traits через возвращаемый `impl Future`.
pub trait Transport {
    type Body: http_body::Body<Data = Bytes>;
    type Error: std::error::Error + 'static;

    fn execute(
        &self,
        req: http::Request<RequestBody>,
    ) -> impl Future<Output = Result<http::Response<Self::Body>, Self::Error>>;

    /// Способности транспорта, определённые один раз — при конструировании —
    /// и с тех пор неизменные для этого объекта. Это не проверка "прямо
    /// сейчас": сигнатура возвращает `&Capabilities`, а не вычисляет его
    /// заново на каждый вызов (пересчёт на каждый вызов не компилируется —
    /// `E0515`, а любая компилирующаяся альтернатива течёт память на
    /// каждый вызов). Бэкенду, чьи способности могут измениться по ходу
    /// процесса, нужно пересобрать транспорт заново.
    fn capabilities(&self) -> &Capabilities;
}
