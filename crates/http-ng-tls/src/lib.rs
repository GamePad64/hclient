//! Подключаемый TLS.
//!
//! Трейт типизирован на `hyper::rt::Read`/`Write`, а **не** на futures-io или
//! tokio-io. Следствие: per-runtime TLS-склейки не существует вообще — один
//! адаптер (Task 9, rustls) обслуживает все рантаймы (`http-ng-rt-tokio`,
//! `http-ng-rt-smol`, и любой будущий), потому что `hyper::rt::{Read, Write}`
//! — единственная точка, к которой уже приведён любой `S` этой вертикали
//! (`http_ng_rt::FuturesIo`, `TokioIo`), а не ещё одна прослойка сверху.
#![forbid(unsafe_code)]

use http_ng_core::Error;
use std::future::Future;

/// Параметры одного TLS-подключения.
///
/// ALPN живёт на **коннекте**, а не на конфиге: пин версии и
/// h2-prior-knowledge требуют разных наборов ALPN для разных соединений к
/// одному origin (например, одна попытка форсирует `h2`-only, следующая —
/// `http/1.1`-фоллбэк). Реализация, которой это дорого пересчитывать на
/// каждый коннект, вправе кэшировать TLS-конфиг по конкретному набору ALPN у
/// себя — это её дело, не дело этого трейта.
#[derive(Debug, Clone, Copy)]
pub struct TlsRequest<'a> {
    pub server_name: &'a str,
    pub alpn: &'a [&'a [u8]],
    /// RFC 9849 Encrypted Client Hello. `EchConfigList` приходит из
    /// HTTPS/SVCB-записи (`http_ng_dns::SvcbEndpoint::ech_config_list`, Task
    /// 6). Слот заложен сразу, а не добавлен по факту первой реализации:
    /// добавление нового поля в структуру запроса позже было бы ломающим
    /// изменением для всех уже написанных реализаций `TlsConnect`.
    pub ech: Option<&'a [u8]>,
}

/// Результат TLS-рукопожатия, доступный вызывающей стороне.
///
/// **Все поля `Option`**: native-tls (единственный бэкенд, доступный без
/// выбора конкретной crypto-библиотеки) отдаёт только leaf-сертификат, ALPN
/// и tls-server-end-point — не полную цепочку, не версию протокола, не
/// шифр-сьют. Трейт обязан допускать бэкенд с таким урезанным набором.
/// Симметрично: бэкенд, который не может сообщить поле, обязан оставить его
/// `None`, а не подставлять правдоподобное значение — способность, которая
/// лжёт о своём состоянии, хуже способности, которой просто нет (тот же
/// принцип, что развёл `RedirectSupport::None`/`Transparent` в
/// `http-ng-core` и `supports_svcb()`/пустой стрим в `http-ng-dns`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TlsInfo {
    /// Согласованный ALPN-протокол этого соединения — один элемент, итог
    /// согласования, а не весь список-предложение из `TlsRequest::alpn`.
    pub alpn: Option<Vec<u8>>,
    /// Цепочка сертификатов пира, DER, в порядке leaf → root. Бэкенды вроде
    /// native-tls отдают только leaf — тогда `Vec` из одного элемента, не
    /// `None`: сертификат есть, просто цепочка неполна.
    pub peer_certificates: Option<Vec<Vec<u8>>>,
    /// Версия протокола TLS, согласованная на этом соединении.
    ///
    /// `String`, а не перечисление этого крейта: перечисления версий у
    /// разных бэкендов (rustls, native-tls поверх OpenSSL/SChannel/
    /// SecureTransport) не совпадают одно с другим по набору вариантов, и
    /// заводить здесь объединяющее перечисление значило бы либо отставать от
    /// нового бэкенда, либо тащить варианты, которых конкретный бэкенд
    /// никогда не вернёт. Чтобы два бэкенда не называли одну и ту же версию
    /// по-разному, значение обязано быть строкой реестрового вида, которую
    /// используют и `openssl`'s `SSL_get_version()`, и rustls:
    /// `"TLSv1.3"`, `"TLSv1.2"`, `"TLSv1.1"`, `"TLSv1.0"` — не
    /// `Debug`-форматирование внутреннего перечисления бэкенда (у rustls,
    /// например, `Debug` для `ProtocolVersion::TLSv1_3` печатает
    /// `TLSv1_3`, с подчёркиванием вместо точки — реализация обязана
    /// привести это к канонической форме, а не прокинуть `Debug`
    /// как есть).
    pub protocol_version: Option<String>,
    /// Шифр-сьют, согласованный на этом соединении.
    ///
    /// Тот же аргумент, что у `protocol_version`, и по той же причине —
    /// `String`, не перечисление. Значение обязано быть именем из реестра
    /// IANA TLS Cipher Suites, например `"TLS_AES_128_GCM_SHA256"` — это имя
    /// использует и rustls (`CipherSuite::TLS13_AES_128_GCM_SHA256` обязан
    /// быть приведён к реестровому имени без версии-префикса, а не отдан как
    /// есть через `Debug`), тогда как OpenSSL по умолчанию называет тот же
    /// шифр-сьют алиасом вроде `"ECDHE-RSA-AES128-GCM-SHA256"` — реализация
    /// поверх OpenSSL обязана перевести алиас в реестровое имя, иначе два
    /// бэкенда сообщат об одном и том же шифре двумя разными строками, и
    /// вызывающая сторона, сравнивающая их, ошибётся.
    pub cipher_suite: Option<String>,
}

/// Подключаемый TLS-хендшейк поверх произвольного транспорта.
///
/// Один метод, `connect`, а не отдельно "handshake" и "wrap": разделять их
/// нечем пользоваться — ни один вызывающий код этой вертикали не хочет
/// голый хендшейк без обёрнутого потока или наоборот.
pub trait TlsConnect {
    /// Обёрнутый поток после хендшейка. `S: hyper::rt::Read + Write + Unpin`
    /// в обоих местах (на самом типе и в его where-клаузе) — реализация не
    /// может обещать обёртку только для части возможных `S`; каждый `S`,
    /// который умеет `connect`, обязан получить обратно и рабочий
    /// `Stream<S>`.
    type Stream<S>: hyper::rt::Read + hyper::rt::Write + Unpin
    where
        S: hyper::rt::Read + hyper::rt::Write + Unpin;

    /// Выполняет TLS-хендшейк поверх уже установленного `io` (TCP-сокет от
    /// `http_ng_rt::TcpConnect`, обёрнутый в `FuturesIo`/`TokioIo` — сам
    /// `connect` про транспорт ничего не знает) и возвращает зашифрованный
    /// поток вместе с тем, что реализация может честно сообщить о
    /// согласованных параметрах.
    fn connect<S>(
        &self,
        io: S,
        req: TlsRequest<'_>,
    ) -> impl Future<Output = Result<(Self::Stream<S>, TlsInfo), Error>>
    where
        S: hyper::rt::Read + hyper::rt::Write + Unpin;
}

#[cfg(test)]
mod tests {
    use super::*;
    use hyper::rt::{Read, ReadBufCursor, Write};
    use std::collections::VecDeque;
    use std::io;
    use std::pin::{Pin, pin};
    use std::task::{Context, Poll, Waker};

    /// Опрашивает один `Future`/`poll_fn` синхронно и требует немедленной
    /// готовности. Каждый future в тестах этого модуля опирается на
    /// `Loopback`, который никогда не возвращает `Pending`, — настоящий
    /// экзекьютор здесь ничего не даёт; `Waker::noop()` (стабилен с 1.85,
    /// той же версии, что MSRV этой вертикали) закрывает вопрос без лишней
    /// зависимости вроде `futures-executor` ради единственного синхронного
    /// опроса.
    fn poll_once<F: Future>(mut fut: Pin<&mut F>) -> F::Output {
        let mut cx = Context::from_waker(Waker::noop());
        match fut.as_mut().poll(&mut cx) {
            Poll::Ready(v) => v,
            Poll::Pending => panic!("тестовый ввод-вывод не должен возвращать Pending"),
        }
    }

    /// `hyper::rt::Read + Write` без единой сторонней зависимости: пишет в
    /// общий буфер, читает из него же. Не мок для подсчёта вызовов — рабочий
    /// ввод-вывод, достаточный, чтобы реально прогнать байты через
    /// `TlsConnect::Stream<S>` туда-обратно.
    #[derive(Default)]
    struct Loopback {
        buf: VecDeque<u8>,
    }

    impl Read for Loopback {
        fn poll_read(
            mut self: Pin<&mut Self>,
            _cx: &mut Context<'_>,
            mut buf: ReadBufCursor<'_>,
        ) -> Poll<io::Result<()>> {
            let n = buf.remaining().min(self.buf.len());
            let chunk: Vec<u8> = self.buf.drain(..n).collect();
            buf.put_slice(&chunk);
            Poll::Ready(Ok(()))
        }
    }

    impl Write for Loopback {
        fn poll_write(
            mut self: Pin<&mut Self>,
            _cx: &mut Context<'_>,
            data: &[u8],
        ) -> Poll<io::Result<usize>> {
            self.buf.extend(data.iter().copied());
            Poll::Ready(Ok(data.len()))
        }
        fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
            Poll::Ready(Ok(()))
        }
        fn poll_shutdown(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
            Poll::Ready(Ok(()))
        }
    }

    /// Пропускающая реализация `TlsConnect::Stream<S>` — обёртка вокруг `S`,
    /// а не `type Stream<S> = S`: настоящий адаптер (Task 9, rustls) обязан
    /// оборачивать `S` в состояние TLS-сессии, и тождественный GAT такую
    /// форму вообще не проверил бы. Ничего не шифрует, только форвардит.
    struct PassThrough<S>(S);

    impl<S: Read + Unpin> Read for PassThrough<S> {
        fn poll_read(
            mut self: Pin<&mut Self>,
            cx: &mut Context<'_>,
            buf: ReadBufCursor<'_>,
        ) -> Poll<io::Result<()>> {
            Pin::new(&mut self.0).poll_read(cx, buf)
        }
    }

    impl<S: Write + Unpin> Write for PassThrough<S> {
        fn poll_write(
            mut self: Pin<&mut Self>,
            cx: &mut Context<'_>,
            data: &[u8],
        ) -> Poll<io::Result<usize>> {
            Pin::new(&mut self.0).poll_write(cx, data)
        }
        fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
            Pin::new(&mut self.0).poll_flush(cx)
        }
        fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
            Pin::new(&mut self.0).poll_shutdown(cx)
        }
    }

    /// Не шифрует ничего: репортит первый предложенный ALPN как
    /// "согласованный" и фиксированную версию протокола — ровно то, что
    /// нужно тесту, чтобы было что проверить в `TlsInfo`, ничего больше
    /// (`peer_certificates`/`cipher_suite` остаются `None` — честно, у
    /// заглушки их взять неоткуда).
    struct NoOpTls;

    impl TlsConnect for NoOpTls {
        type Stream<S>
            = PassThrough<S>
        where
            S: Read + Write + Unpin;

        fn connect<S>(
            &self,
            io: S,
            req: TlsRequest<'_>,
        ) -> impl Future<Output = Result<(Self::Stream<S>, TlsInfo), Error>>
        where
            S: Read + Write + Unpin,
        {
            let alpn = req.alpn.first().map(|proto| proto.to_vec());
            async move {
                Ok((
                    PassThrough(io),
                    TlsInfo {
                        alpn,
                        peer_certificates: None,
                        protocol_version: Some("TLSv1.3".to_string()),
                        cipher_suite: None,
                    },
                ))
            }
        }
    }

    #[test]
    fn connect_wraps_the_stream_and_negotiates_alpn() {
        // Байты ALPN построены из ЛОКАЛЬНЫХ `Vec<u8>`, не из `&'static
        // [u8]`-литералов — доказывает, что `TlsRequest<'a>` с ОДНОЙ и той же
        // `'a` на внешнем срезе и на байтах каждого элемента реально
        // конструируема без `'static` и без хранения `req` где-либо дольше
        // самого вызова `connect`, для которого она и задумана ("ALPN живёт
        // на коннекте" — см. doc-комментарий поля).
        let h2 = b"h2".to_vec();
        let http11 = b"http/1.1".to_vec();
        let alpn = [h2.as_slice(), http11.as_slice()];
        let req = TlsRequest {
            server_name: "example.com",
            alpn: &alpn,
            ech: None,
        };

        // `io` уже содержит данные ДО хендшейка — доказывает ниже, что
        // возвращённый `Stream<S>` реально оборачивает ЭТОТ `io`, а не
        // подставляет независимый источник, который случайно тоже
        // реализует `Read`/`Write`.
        let mut io = Loopback::default();
        io.buf.extend(*b"preexisting");

        let fut = NoOpTls.connect(io, req);
        let mut fut = pin!(fut);
        let (mut stream, info) = poll_once(fut.as_mut()).unwrap();

        assert_eq!(info.alpn.as_deref(), Some(b"h2".as_slice()));
        assert_eq!(info.protocol_version.as_deref(), Some("TLSv1.3"));
        assert!(info.peer_certificates.is_none());
        assert!(info.cipher_suite.is_none());

        // Данные, лежавшие в `io` ДО `connect`, видны через возвращённый
        // `Stream<S>` — значит это обёртка над переданным `io`, не новый
        // отсоединённый поток.
        let mut preexisting = [0u8; 11];
        let mut rb = hyper::rt::ReadBuf::new(&mut preexisting);
        let read = std::future::poll_fn(|cx| Pin::new(&mut stream).poll_read(cx, rb.unfilled()));
        poll_once(pin!(read).as_mut()).unwrap();
        assert_eq!(&preexisting, b"preexisting");

        // `Stream<S>` реально реализует `hyper::rt::Write`, не только
        // типизируется как таковой: пишем и читаем обратно через тот же
        // общий буфер `Loopback`.
        let write = std::future::poll_fn(|cx| Pin::new(&mut stream).poll_write(cx, b"ping"));
        let n = poll_once(pin!(write).as_mut()).unwrap();
        assert_eq!(n, 4);

        let mut echoed = [0u8; 4];
        let mut rb = hyper::rt::ReadBuf::new(&mut echoed);
        let read = std::future::poll_fn(|cx| Pin::new(&mut stream).poll_read(cx, rb.unfilled()));
        poll_once(pin!(read).as_mut()).unwrap();
        assert_eq!(&echoed, b"ping");
    }
}
