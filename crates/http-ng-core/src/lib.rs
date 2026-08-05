//! Контракт плагина http-ng.
//!
//! Инвариант крейта: ни одного объявленного бонда `Send`/`Sync`. Send-ность
//! выводится auto-traits через `impl Future`.
#![deny(unsafe_code)]

mod error;

pub use error::{Error, ErrorKind, Phase};
