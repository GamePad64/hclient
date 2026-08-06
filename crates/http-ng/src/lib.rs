//! Кроссплатформенный асинхронный HTTP-клиент.
//!
//! Инвариант крейта: ни одного объявленного бонда `Send`/`Sync`, ни одного
//! `#[cfg]`-переключаемого трейт-алиаса. Send-ность выводится auto-traits.
#![deny(unsafe_code)]

mod client;
mod config;
#[cfg(feature = "test-util")]
pub mod mock;
mod stages;

pub use client::{Client, ClientBuilder};
pub use config::{Config, Timeouts, check_supported, effective_timeouts};
pub use http_ng_core::{Capabilities, RequestBody, UnsupportedCapability};
pub use http_ng_proto::redirect::RedirectPolicy;
