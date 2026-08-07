//! Native-транспорт http-ng: TCP + TLS + HTTP/1.1 поверх hyper.
//!
//! Этот крейт собирает воедино рантайм ([`http_ng_rt`]), DNS ([`http_ng_dns`])
//! и TLS ([`http_ng_tls`]) поверх `hyper`. Task 10 закладывает только адаптер
//! тела запроса ([`body`], `pub(crate)`) — коннектор, HTTP/1-драйвер и сам
//! `Transport` появятся в Tasks 11–13, поэтому крейт пока не экспортирует
//! ничего публично.
#![forbid(unsafe_code)]

mod body;
