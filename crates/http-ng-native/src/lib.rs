//! Native-транспорт http-ng: TCP + TLS + HTTP/1.1 поверх hyper.
//!
//! Этот крейт собирает воедино рантайм ([`http_ng_rt`]), DNS ([`http_ng_dns`])
//! и TLS ([`http_ng_tls`]) поверх `hyper`. Task 10 заложила только адаптер
//! тела запроса ([`body`], `pub(crate)`); Task 11 добавляет коннектор
//! ([`connect`], тоже `pub(crate)` — HTTP/1-драйвер и сам `Transport`
//! появятся в Tasks 12–13 и станут первыми настоящими потребителями). Крейт
//! по-прежнему не экспортирует ничего публично, кроме тестового хелпера
//! [`testing`].
#![forbid(unsafe_code)]

mod body;
mod connect;

/// Только для интеграционных тестов этого крейта: `pub`, а не `pub(crate)`,
/// потому что `tests/*.rs` компилируются как отдельный внешний крейт и не
/// видят `pub(crate)`-элементы вроде `connect::race_connect` напрямую.
/// `#[doc(hidden)]` — это не часть публичного API крейта, а щель, специально
/// проделанная для интеграционных тестов задачи (см. `tests/connect.rs`,
/// `tests/dual_runtime.rs`); Task 12/13 не обязаны и не должны на неё
/// полагаться.
#[doc(hidden)]
pub mod testing {
    /// Гоняет Happy Eyeballs по готовому списку адресов, минуя DNS —
    /// обёртка над `connect::race_connect` с дефолтным `HeConfig` и
    /// `TcpOpts`, ровно то, что нужно тесту, который контролирует только
    /// список адресов и порт.
    pub async fn connect_for_test<R>(
        rt: &R,
        addrs: &[std::net::IpAddr],
        port: u16,
    ) -> Result<R::Stream, http_ng_core::Error>
    where
        R: http_ng_rt::TcpConnect + http_ng_rt::Timer,
    {
        let (v6, v4): (Vec<_>, Vec<_>) = addrs.iter().copied().partition(|a| a.is_ipv6());
        crate::connect::race_connect(
            rt,
            v6,
            v4,
            port,
            &http_ng_rt::TcpOpts::default(),
            http_ng_proto::happy_eyeballs::HeConfig::default(),
        )
        .await
    }
}
