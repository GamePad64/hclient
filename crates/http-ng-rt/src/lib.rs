//! Способности рантайма для native-транспорта http-ng.
//!
//! Раздельные трейты, а не один `Runtime`: транспорт требует только то, чем
//! пользуется, а бэкенд без сокетов не обязан реализовывать `connect` заглушкой,
//! которая паникует.
#![deny(unsafe_code)]

mod caps;
mod futures_io;

pub use caps::{Blocking, Spawn, TcpAdoptStd, TcpConnect, TcpOpts};
pub use futures_io::FuturesIo;

/// `Timer` определён один раз, в `http-ng-core`: он нужен портативному ядру
/// для таймаутов и backoff. Здесь только реэкспорт.
pub use http_ng_core::unversioned::Timer;
