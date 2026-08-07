//! Чистые автоматы протокольных слоёв http-ng.
//!
//! Инвариант крейта: ни одного `async fn`, ни одной зависимости от рантайма.
//! Всё, что зависит от времени, принимает `now` параметром. Проверяется в CI.
#![forbid(unsafe_code)]

pub mod happy_eyeballs;
pub mod redirect;
pub mod sse;
pub mod uri;
