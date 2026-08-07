//! TLS-бэкенд на rustls.
//!
//! **rustls не появляется в публичном API `http-ng`** — иначе выход 0.24 стал
//! бы нашим ломающим релизом. В 0.24 ожидаются: удалённая фича `std`,
//! провайдеры вынесены в `rustls-ring`/`rustls-aws-lc-rs`, MSRV 1.85,
//! edition 2024. Один переписанный крейт заложен в бюджет.
//!
//! `forbid`, не `deny` (см. `http-ng-rt`, Task 2 вертикали 2, fix round 1):
//! `deny(unsafe_code)` переопределим локальным `#[allow(unsafe_code)]` рядом
//! с самим `unsafe`-блоком — компилятор промолчит; `forbid` не переопределим
//! изнутри крейта никак (`E0453`).
#![forbid(unsafe_code)]

mod stream;

pub use stream::TlsStream;

use http_ng_core::{Error, ErrorKind};
use http_ng_tls::{TlsConnect, TlsInfo, TlsRequest};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

#[derive(Debug)]
pub struct Rustls {
    base: Arc<rustls::ClientConfig>,
    /// ALPN задаётся на коннект, а `ClientConfig` его хранит внутри — поэтому
    /// кэшируем конфиг по набору ALPN. Без кэша каждый запрос строил бы
    /// конфиг заново, а это самая дорогая операция в rustls.
    by_alpn: Mutex<HashMap<Vec<Vec<u8>>, Arc<rustls::ClientConfig>>>,
}

impl Rustls {
    pub fn from_config(cfg: Arc<rustls::ClientConfig>) -> Self {
        Self {
            base: cfg,
            by_alpn: Mutex::new(HashMap::new()),
        }
    }

    #[cfg(feature = "webpki-roots")]
    pub fn with_webpki_roots() -> Self {
        let roots: rustls::RootCertStore = webpki_roots::TLS_SERVER_ROOTS.iter().cloned().collect();
        Self::from_config(Arc::new(
            rustls::ClientConfig::builder()
                .with_root_certificates(roots)
                .with_no_client_auth(),
        ))
    }

    /// Брифом этой задачи предлагался `rustls_platform_verifier::tls_config()`
    /// — такой свободной функции в `rustls-platform-verifier` 0.7 не
    /// существует (проверено по исходникам крейта: `src/lib.rs` экспортирует
    /// только два extension-трейта, `BuilderVerifierExt` на
    /// `ConfigBuilder<ClientConfig, WantsVerifier>` и `ConfigVerifierExt` на
    /// самом `ClientConfig`). Верный вызов — метод расширения
    /// `ClientConfig::with_platform_verifier()` из `ConfigVerifierExt`.
    #[cfg(feature = "platform-verifier")]
    pub fn with_platform_verifier() -> Result<Self, Error> {
        use rustls_platform_verifier::ConfigVerifierExt;
        let cfg = rustls::ClientConfig::with_platform_verifier()
            .map_err(|e| Error::new(ErrorKind::Tls, e))?;
        Ok(Self::from_config(Arc::new(cfg)))
    }

    fn config_for(&self, alpn: &[&[u8]]) -> Arc<rustls::ClientConfig> {
        if alpn.is_empty() {
            return self.base.clone();
        }
        let key: Vec<Vec<u8>> = alpn.iter().map(|a| a.to_vec()).collect();
        let mut cache = self.by_alpn.lock().expect("alpn cache poisoned");
        cache
            .entry(key.clone())
            .or_insert_with(|| {
                let mut cfg = (*self.base).clone();
                cfg.alpn_protocols = key;
                Arc::new(cfg)
            })
            .clone()
    }
}

/// Приводит версию протокола, которую rustls называет по имени варианта
/// перечисления (`TLSv1_3`, с подчёркиванием), к реестровому виду, который
/// документирует `TlsInfo::protocol_version` (`"TLSv1.3"`, с точкой) —
/// том же самом, что использует `SSL_get_version()` у OpenSSL.
///
/// Явный `match` по четырём вариантам семейства TLS, а не
/// `format!("{v:?}").replace('_', ".")`: `rustls::ProtocolVersion`
/// `#[non_exhaustive]` и несёт варианты вне семейства TLS (`SSLv2`, `SSLv3`,
/// `DTLSv1_0/2/3`) и вариант `Unknown(u16)` для нераспознанных значений.
/// `rustls::ClientConnection` в этой сборке (фичи `std`, `ring`, `tls12`, без
/// `unstable_apis`) никогда не согласует ничего вне TLS 1.0–1.3 — ни SSL, ни
/// DTLS вообще не реализованы в rustls, — так что ветка `_ => None` здесь не
/// наблюдаема на практике, а не угадана: это защита от будущего расширения
/// перечисления, а не текущий кейс. `None` — тот же принцип, что уже
/// установлен доком `TlsInfo::protocol_version`: значение, которое нечем
/// подтвердить как одну из четырёх канонических строк, честно остаётся
/// `None`, а не становится приблизительной или заведомо неверной строкой.
fn normalize_protocol_version(v: rustls::ProtocolVersion) -> Option<String> {
    use rustls::ProtocolVersion::*;
    match v {
        TLSv1_0 => Some("TLSv1.0".to_string()),
        TLSv1_1 => Some("TLSv1.1".to_string()),
        TLSv1_2 => Some("TLSv1.2".to_string()),
        TLSv1_3 => Some("TLSv1.3".to_string()),
        _ => None,
    }
}

/// Приводит имя cipher suite, которое rustls называет по имени варианта
/// перечисления, к реестровому имени IANA, которое документирует
/// `TlsInfo::cipher_suite`.
///
/// Только у сьютов TLS 1.3 имя варианта несёт версионный инфикс `13`
/// (`TLS13_AES_128_GCM_SHA256` и ещё четыре константы в
/// `rustls::CipherSuite`) — этот инфикс IANA в имени сьюта не использует
/// (`TLS_AES_128_GCM_SHA256`), и его обязана снять реализация. У сьютов
/// TLS 1.2 и старше rustls уже называет вариант ровно так, как называет его
/// IANA (`TLS_ECDHE_RSA_WITH_AES_128_GCM_SHA256`) — снимать нечего, имя
/// проходит без изменений.
///
/// `CipherSuite::as_str()` — не `format!("{suite:?}")`: у этого перечисления
/// (сгенерировано макросом `enum_builder!`, `rustls/src/msgs/macros.rs`) есть
/// публичный `as_str(&self) -> Option<&'static str>`, вопреки брифу этой
/// задачи, утверждавшему обратное. Он не отменяет саму нормализацию —
/// для распознанных вариантов `as_str()` возвращает буквально то же имя, что
/// и `Debug` (`"TLS13_AES_128_GCM_SHA256"`), с тем же инфиксом, который
/// всё равно предстоит снять, — но, в отличие от `Debug`, честно отдаёт
/// `None` для `CipherSuite::Unknown(_)`, а не форматированную строку вида
/// `"CipherSuite(0x9999)"`, которую потом пришлось бы отдельно распознавать
/// и отбрасывать. Сьют, для которого криптопровайдер не смог подобрать
/// имя, — тот же случай "нечем подтвердить каноническую форму", что и у
/// `normalize_protocol_version`: честный `None`, а не изобретённая строка.
fn normalize_cipher_suite(suite: rustls::CipherSuite) -> Option<String> {
    let raw = suite.as_str()?;
    Some(match raw.strip_prefix("TLS13_") {
        Some(rest) => format!("TLS_{rest}"),
        None => raw.to_string(),
    })
}

impl TlsConnect for Rustls {
    type Stream<S>
        = TlsStream<S>
    where
        S: hyper::rt::Read + hyper::rt::Write + Unpin;

    async fn connect<S>(&self, io: S, req: TlsRequest<'_>) -> Result<(TlsStream<S>, TlsInfo), Error>
    where
        S: hyper::rt::Read + hyper::rt::Write + Unpin,
    {
        let name = rustls_pki_types::ServerName::try_from(req.server_name)
            .map_err(|e| Error::new(ErrorKind::Tls, e))?
            .to_owned();
        let conn = rustls::ClientConnection::new(self.config_for(req.alpn), name)
            .map_err(|e| Error::new(ErrorKind::Tls, e))?;
        let mut stream = TlsStream::new(io, conn);

        // Довести хендшейк до конца, прежде чем отдавать поток наверх.
        std::future::poll_fn(|cx| {
            let (io, conn) = stream.parts_mut();
            loop {
                std::task::ready!(stream::flush_outgoing(io, conn, cx))
                    .map_err(|e| Error::new(ErrorKind::Tls, e))?;
                if !conn.is_handshaking() {
                    return std::task::Poll::Ready(Ok::<(), Error>(()));
                }
                let more = std::task::ready!(stream::pump_incoming(io, conn, cx))
                    .map_err(|e| Error::new(ErrorKind::Tls, e))?;
                if !more {
                    return std::task::Poll::Ready(Err(Error::new(
                        ErrorKind::Tls,
                        std::io::Error::from(std::io::ErrorKind::UnexpectedEof),
                    )));
                }
            }
        })
        .await?;

        let c = stream.conn();
        let info = TlsInfo {
            alpn: c.alpn_protocol().map(|a| a.to_vec()),
            peer_certificates: c
                .peer_certificates()
                .map(|cs| cs.iter().map(|d| d.as_ref().to_vec()).collect()),
            protocol_version: c.protocol_version().and_then(normalize_protocol_version),
            cipher_suite: c
                .negotiated_cipher_suite()
                .and_then(|s| normalize_cipher_suite(s.suite())),
        };
        Ok((stream, info))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Мутационно проверено вручную (см. отчёт задачи): временный откат
    // `normalize_protocol_version`/`normalize_cipher_suite` на
    // `format!("{v:?}")`/`format!("{:?}", s.suite())` красит именно эти два
    // теста в красный — `TLSv1_3`/`TLS13_AES_128_GCM_SHA256` не совпадают с
    // ожидаемыми `TLSv1.3`/`TLS_AES_128_GCM_SHA256`.

    #[test]
    fn protocol_version_is_dotted_not_underscored() {
        assert_eq!(
            normalize_protocol_version(rustls::ProtocolVersion::TLSv1_3).as_deref(),
            Some("TLSv1.3"),
            "Debug rustls печатает TLSv1_3 (подчёркивание) — реестровая форма с точкой"
        );
        assert_eq!(
            normalize_protocol_version(rustls::ProtocolVersion::TLSv1_2).as_deref(),
            Some("TLSv1.2")
        );
        assert_eq!(
            normalize_protocol_version(rustls::ProtocolVersion::TLSv1_1).as_deref(),
            Some("TLSv1.1")
        );
        assert_eq!(
            normalize_protocol_version(rustls::ProtocolVersion::TLSv1_0).as_deref(),
            Some("TLSv1.0")
        );
    }

    #[test]
    fn protocol_version_outside_tls_family_is_none_not_a_guess() {
        // Ни SSL, ни DTLS, ни нераспознанный ordinal — rustls-клиент никогда
        // не согласует ни то, ни другое; ветка защищает `#[non_exhaustive]`
        // перечисление от будущего расширения, а не текущий наблюдаемый
        // случай.
        assert_eq!(
            normalize_protocol_version(rustls::ProtocolVersion::SSLv3),
            None
        );
        assert_eq!(
            normalize_protocol_version(rustls::ProtocolVersion::Unknown(0xABCD)),
            None
        );
    }

    #[test]
    fn cipher_suite_strips_the_tls13_version_infix() {
        assert_eq!(
            normalize_cipher_suite(rustls::CipherSuite::TLS13_AES_128_GCM_SHA256).as_deref(),
            Some("TLS_AES_128_GCM_SHA256"),
            "Debug rustls печатает TLS13_AES_128_GCM_SHA256 — реестровое имя IANA без инфикса версии"
        );
        assert_eq!(
            normalize_cipher_suite(rustls::CipherSuite::TLS13_CHACHA20_POLY1305_SHA256).as_deref(),
            Some("TLS_CHACHA20_POLY1305_SHA256")
        );
    }

    #[test]
    fn cipher_suite_tls12_name_already_matches_iana_unchanged() {
        assert_eq!(
            normalize_cipher_suite(rustls::CipherSuite::TLS_ECDHE_RSA_WITH_AES_128_GCM_SHA256)
                .as_deref(),
            Some("TLS_ECDHE_RSA_WITH_AES_128_GCM_SHA256")
        );
    }

    #[test]
    fn cipher_suite_unrecognised_by_the_provider_is_none_not_debug_passthrough() {
        // `CipherSuite::Unknown(_)` — вариант, для которого нет реестрового
        // имени вообще; `Debug` напечатал бы `CipherSuite(0x9999)`, что не
        // является ни валидным именем IANA, ни честным `None`.
        assert_eq!(
            normalize_cipher_suite(rustls::CipherSuite::Unknown(0x9999)),
            None
        );
    }
}
