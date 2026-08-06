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

    /// Что этот транспорт умеет **сейчас, в этом процессе**.
    fn capabilities(&self) -> &Capabilities;
}
