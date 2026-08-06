//! Кроссплатформенный асинхронный HTTP-клиент.
//!
//! Инвариант крейта: ни одного объявленного бонда `Send`/`Sync`, ни одного
//! `#[cfg]`-переключаемого трейт-алиаса. Send-ность выводится auto-traits.
#![deny(unsafe_code)]

mod config;

pub use config::{Config, Timeouts, check_supported, effective_timeouts};
