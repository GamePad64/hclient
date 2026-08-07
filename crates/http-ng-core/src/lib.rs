//! Контракт плагина http-ng.
//!
//! Инвариант крейта: seam-трейты (`Transport`, `Timer`, middleware) не
//! объявляют бонды `Send`/`Sync` — Send-ность выводится auto-traits через
//! `impl Future`. Единственное документированное исключение — [`Error`]: её
//! `source` обязан быть `Send + Sync`, иначе `Client::execute` не мог бы
//! вернуть Send-совместимый future ни для одного бэкенда. Бонд
//! `T::Error: Send + Sync + 'static` живёт в собственном where-клаузе
//! `Client::execute`, а не в самом трейте `Transport`.
#![forbid(unsafe_code)]

mod body;
mod caps;
mod error;
pub mod unversioned;

pub use body::{RequestBody, RetryKind, RewindFactory};
pub use caps::{
    Capabilities, RedirectSupport, TimeoutSupport, Timeouts, TlsSupport, UnsupportedCapability,
    UpgradeSupport,
};
pub use error::{Error, ErrorKind, Phase};
