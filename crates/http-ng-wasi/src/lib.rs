//! Транспорт http-ng поверх `wasi:http` 0.3 (пакет `wasip3`).
//!
//! Собирается под `wasm32-wasip2`. Ни один тип `wasip3` не появляется в
//! публичном API этого крейта: `Body::Error` — это `http_ng_core::Error`,
//! которая стирает источник в `Arc<dyn std::error::Error + Send + Sync>`.
#![deny(unsafe_code)]

mod body;
mod convert;

pub use body::Body;

use convert::Payload;
use futures::join;
use http_ng_core::unversioned::Transport;
use http_ng_core::{
    Capabilities, Error, RedirectSupport, RequestBody, TimeoutSupport, Timeouts, TlsSupport,
    UpgradeSupport,
};
use wasip3::http::types::{ErrorCode, Fields, Request, RequestOptions};
use wasip3::http_compat::{BodyWriter, http_from_wasi_response};

/// Транспорт над амбиентным `wasi:http/client.send` — гость не держит
/// собственного сокета, всё сетевое взаимодействие делегировано хосту.
#[derive(Debug)]
pub struct WasiHttp {
    caps: Capabilities,
}

impl WasiHttp {
    pub fn new() -> Self {
        let mut caps = Capabilities::none();
        // wasi:http 0.3 симметричен по телам и умеет трейлеры в обе
        // стороны — богаче нативного. `streaming_request_body` и
        // `full_duplex` здесь не просто заявлены: `Transport::execute`
        // пишет тело запроса через `join!` параллельно с `client::send`
        // (честный дуплекс), а `RequestBody::Streaming` уходит в
        // `BodyWriter::send_http_body` как есть, кадр за кадром, без
        // буферизации в память (честный стриминг) — см. `convert::resolve_payload`.
        caps.streaming_request_body = true;
        caps.full_duplex = true;
        caps.request_trailers = true;
        caps.response_trailers = true;
        caps.timeouts = TimeoutSupport {
            connect: true,
            first_byte: true,
            between_bytes: true,
        };
        // И беднее по всему остальному: в спеке нет ни редиректов, ни TLS,
        // ни прокси, ни выбора версии, ни upgrade.
        caps.redirects = RedirectSupport::None;
        caps.tls_config = TlsSupport::None;
        caps.upgrade = UpgradeSupport::None;
        Self { caps }
    }
}

impl Default for WasiHttp {
    fn default() -> Self {
        Self::new()
    }
}

impl Transport for WasiHttp {
    type Body = Body;
    type Error = Error;

    async fn execute(
        &self,
        req: http::Request<RequestBody>,
    ) -> Result<http::Response<Body>, Error> {
        let (parts, body) = req.into_parts();
        let scheme = convert::scheme_of(&parts.uri)?;

        let header_list: Vec<(String, Vec<u8>)> = parts
            .headers
            .iter()
            .map(|(k, v)| (k.to_string(), v.as_bytes().to_vec()))
            .collect();
        let fields = Fields::from_list(&header_list).map_err(convert::fields_error)?;

        let timeouts = parts
            .extensions
            .get::<Timeouts>()
            .copied()
            .unwrap_or_default();
        let opts = RequestOptions::new();
        convert::apply_timeouts(
            &opts,
            timeouts.connect.map(|d| d.as_nanos() as u64),
            timeouts.first_byte.map(|d| d.as_nanos() as u64),
            timeouts.between_bytes.map(|d| d.as_nanos() as u64),
        )?;

        // `writer` и `payload` заводятся вместе, в одном `Option`: держать
        // их как два независимых `Option`, которые "должны" совпадать по
        // построению, было бы ровно тем классом инварианта, который потом
        // приходится закрывать недостижимой веткой `match`. Здесь эта пара
        // не может разойтись — писать нечего ровно тогда, когда писать
        // некому.
        let payload = convert::resolve_payload(body);
        let (writer_and_payload, contents, trailers) = match payload {
            None => {
                let (_, trailers) =
                    wasip3::wit_future::new::<Result<Option<Fields>, ErrorCode>>(|| Ok(None));
                (None, None, trailers)
            }
            Some(p) => {
                let (w, reader, trailers) = BodyWriter::new();
                (Some((w, p)), Some(reader), trailers)
            }
        };

        let (wasi_request, _) = Request::new(fields, contents, trailers, Some(opts));
        wasi_request
            .set_method(&convert::to_wasi_method(&parts.method))
            .map_err(|_| convert::rejected("method"))?;
        wasi_request
            .set_scheme(Some(&scheme))
            .map_err(|_| convert::rejected("scheme"))?;
        if let Some(a) = parts.uri.authority() {
            wasi_request
                .set_authority(Some(a.as_str()))
                .map_err(|_| convert::rejected("authority"))?;
        }
        wasi_request
            .set_path_with_query(parts.uri.path_and_query().map(|p| p.as_str()))
            .map_err(|_| convert::rejected("path_with_query"))?;

        // Структурная конкуррентность: тело пишется рядом с `send`, без
        // `spawn` — именно поэтому WASI-транспорту не нужна способность
        // `Spawn`, а `Capabilities::full_duplex` не ложь. Ни один из двух
        // `Result`, которые возвращает эта пара futures, не отбрасывается —
        // `convert::resolve_send` учитывает оба (см. её doc-комментарий про
        // политику при расхождении исходов).
        let wasi_response = match writer_and_payload {
            None => wasip3::http::client::send(wasi_request)
                .await
                .map_err(convert::wasi_err)?,
            Some((w, Payload::Bytes(bytes))) => {
                let mut b = Body::from_bytes(bytes);
                let (resp, written) = join!(
                    wasip3::http::client::send(wasi_request),
                    w.send_http_body(&mut b),
                );
                convert::resolve_send(resp, written)?
            }
            Some((w, Payload::Streaming(mut s))) => {
                let (resp, written) = join!(
                    wasip3::http::client::send(wasi_request),
                    w.send_http_body(&mut s),
                );
                convert::resolve_send(resp, written)?
            }
        };

        let (resp_parts, incoming) = http_from_wasi_response(wasi_response)
            .map_err(convert::wasi_err)?
            .into_parts();
        Ok(http::Response::from_parts(
            resp_parts,
            Body::from_incoming(incoming),
        ))
    }

    fn capabilities(&self) -> &Capabilities {
        &self.caps
    }
}
