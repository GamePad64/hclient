//! Транспорт http-ng поверх `wasi:http` 0.3 (пакет `wasip3`).
//!
//! Собирается под `wasm32-wasip2`. Ни один тип `wasip3` не появляется в
//! публичном API этого крейта: `Body::Error` — это `http_ng_core::Error`,
//! которая стирает источник в `Arc<dyn std::error::Error + Send + Sync>`.
#![deny(unsafe_code)]

mod body;
mod convert;

pub use body::Body;
