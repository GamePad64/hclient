> **`Transport::to_error` и `Native`.** Вертикаль 1 (находка B2 её финального
> ревью) добавила в шов дефолтный хук
> `fn to_error(&self, e: Self::Error) -> Error`, превращающий ошибку бэкенда в
> ошибку библиотеки. У `Native` `type Error = Error`, то есть категория УЖЕ
> проставлена там, где отказ произошёл (`ErrorKind::Resolve` в `execute`,
> `Connect` в `race_connect`, `Tls` в `TlsConnect`, `Body` в `h1`).
>
> **Потерять её нельзя.** Дефолт хука сперва проверяет, не является ли
> `Self::Error` в точности `http_ng_core::Error`, и если да — пропускает её
> насквозь. Так что для `Native` правильное поведение есть поведение по
> умолчанию, и «забыл переопределить» перестало быть дефектом. Первая версия
> хука заворачивала безусловно, и забывчивый бэкенд молча терял всю свою
> таксономию: `kind()` становился `Other`, предикаты `is_*` — `false` для
> всего сразу, `Display` печатал категорию дважды. Защитой служила проза;
> фикс-раунд 3 заменил её механизмом.
>
> **Переопределять всё равно надо — явно, тождеством** (Step 3 ниже). Не ради
> корректности, а ради читаемости: явное `fn to_error(&self, e: Self::Error)
> -> Error { e }` называет намерение в точке, где его читают, и переживёт
> возможное изменение дефолта. И тест из Step 1 обязателен: он проверяет не
> дефолт (тот проверен в `http-ng-core`), а что категория `Native` реально
> доезжает до вызывающей стороны через весь путь `Client::execute`.
>
> **Дефолт не покрывает** бэкенд, чья ошибка — СВОЙ тип, несущий категорию
> внутри: угадать чужое перечисление он не может, и без переопределения такая
> ошибка станет `ErrorKind::Other`. К `Native` это не относится, но относится
> к любому будущему бэкенду, который решит завести собственный тип ошибки.

# http-ng v0.1, вертикаль 2: native — план реализации

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Тот же прикладной код, что в вертикали 1, ходит по настоящей сети —
TCP + TLS + HTTP/1.1 — и работает **на tokio и на smol** без единого `#[cfg]` в
общем коде.

**Architecture:** Рантайм разложен не на один `Runtime`, а на раздельные
способности (`Spawn`, `TcpConnect`, `TcpAdoptStd`, `Blocking`), чтобы транспорт
требовал только то, чем пользуется. TLS-адаптер пишется напрямую под
`hyper::rt::Read/Write`, а не под futures-io или tokio-io — поэтому
per-runtime TLS-склейки не существует вообще. HTTP/1-соединение драйвится
**инлайн**, без spawn: это доказывает, что рантайм-шов настоящий, потому что
клиент едет на голом `futures`-executor'е.

**Tech Stack:** `hyper` 1.11 (только `client` + `http1`), `rustls` 0.23,
`rustls-pki-types` 1.15, `rustls-platform-verifier` 0.7, `webpki-roots` 1.0,
`socket2` 0.6, `tokio` 1 (`net`,`rt`,`time`,`sync`), `smol` / `async-io` 2 +
`async-net` 2, `futures` 0.3.

## Global Constraints

Наследуются из плана вертикали 1 и дополняются:

- **`http-ng-core` и `http-ng` по-прежнему не содержат ни одного объявленного
  бонда `Send`/`Sync`.** Все требования `Send` заперты в `http-ng-native`,
  `http-ng-tls-*`, `http-ng-dns-hickory` и приходят от чужого кода.
- **`http-ng-rt` не тянет ни один рантайм.** Только `hyper` (ради `rt`-трейтов)
  и `futures-io`. Реализации — в `-rt-tokio` / `-rt-smol`.
- **Ни один тип hyper, rustls или socket2 не появляется в публичном API**
  `http-ng-native`. `hyper::upgrade::Upgraded` — под особым запретом.
- **`unsafe` запрещён везде** (`#![deny(unsafe_code)]`). Шим futures-io →
  hyper::rt пишется через безопасный `ReadBufCursor::put_slice`, как советует
  документация самого hyper.
- **Бэкенд, чей `Transport::Error` — СВОЙ тип, несущий категорию внутри,
  обязан переопределить `Transport::to_error`.** Дефолт узнаёт только
  `http_ng_core::Error` (её он пропускает насквозь — то есть `Native` с
  `type Error = Error` защищён структурно); чужое перечисление он угадать не
  может, и без переопределения такая ошибка станет `ErrorKind::Other`.
  Переопределять тождеством стоит и там, где дефолт уже прав — явная строка
  называет намерение. Подробности — в блоке над Step 1 задачи 13 и в
  doc-комментарии самого метода в `http-ng-core`.
- Пул соединений **не входит** в эту вертикаль: одно соединение на запрос.
- MSRV: 1.85. `rust-version` у всех крейтов вертикали — `"1.85"`. Job `msrv`
  в CI гоняет `cargo check --all-features --all-targets` (с `--all-targets` —
  с фикс-раунда финального ревью вертикали 1; до него тестовые таргеты не
  проверялись при 1.85 ни разу), но его список пакетов — три ядровых крейта.
  Крейты этой вертикали нужно в него добавить.

## Файловая структура

```
crates/http-ng-rt/
  src/lib.rs                 реэкспорт Timer из core, TcpOpts
  src/caps.rs                Spawn, TcpConnect, TcpAdoptStd, Blocking
  src/futures_io.rs          FuturesIo<S>: futures-io -> hyper::rt
crates/http-ng-rt-tokio/src/lib.rs
crates/http-ng-rt-smol/src/lib.rs
crates/http-ng-dns/
  src/lib.rs                 Resolve, ResolvedAddr, SvcbEndpoint
crates/http-ng-dns-system/src/lib.rs
crates/http-ng-tls/
  src/lib.rs                 TlsConnect, TlsRequest, TlsInfo
crates/http-ng-tls-rustls/
  src/lib.rs                 Rustls: TlsConnect
  src/stream.rs              TlsStream<S>: hyper::rt::Read + Write
crates/http-ng-native/
  src/lib.rs                 Native<R, T, D>: Transport
  src/connect.rs             Happy Eyeballs + TCP + TLS + ALPN
  src/h1.rs                  handshake + инлайн-драйв соединения
  src/body.rs                мост RequestBody -> http_body::Body с Send-ошибкой
crates/http-ng-proto/
  src/happy_eyeballs.rs      чистый планировщик RFC 8305 (задача 5)
crates/http-ng/
  src/lib.rs                 DefaultTransport + Client<T = DefaultTransport>
```

---

### Task 1: `http-ng-rt` — раздельные способности рантайма

**Files:**
- Create: `crates/http-ng-rt/Cargo.toml`, `src/lib.rs`, `src/caps.rs`
- Test: внутри `caps.rs`

**Interfaces:**
- Consumes: `http_ng_core::unversioned::Timer`.
- Produces:
  - `pub trait Spawn<F: Future<Output = ()>> { fn spawn(&self, f: F); }`
  - `pub trait TcpConnect { type Stream: hyper::rt::Read + hyper::rt::Write + Unpin; fn connect(&self, addr: SocketAddr, opts: &TcpOpts) -> impl Future<Output = std::io::Result<Self::Stream>>; }`
  - `pub trait TcpAdoptStd: TcpConnect { fn adopt(&self, std: std::net::TcpStream) -> std::io::Result<Self::Stream>; }`
  - `pub trait Blocking { fn run<T, F: FnOnce() -> T>(&self, f: F) -> impl Future<Output = T>; }`
  - `pub struct TcpOpts { pub nodelay: bool, pub keepalive: Option<Duration>, pub local_address: Option<IpAddr>, pub send_buffer_size: Option<usize>, pub recv_buffer_size: Option<usize>, pub reuse_address: bool }` (`Default`)
  - `pub use http_ng_core::unversioned::Timer;`

- [ ] **Step 1: Написать падающий тест**

```rust
// crates/http-ng-rt/src/caps.rs
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tcp_opts_default_is_conservative() {
        let o = TcpOpts::default();
        assert!(!o.nodelay, "nodelay включает пользователь, не мы");
        assert!(o.keepalive.is_none());
        assert!(o.local_address.is_none());
        assert!(!o.reuse_address);
    }

    #[test]
    fn spawn_is_generic_over_the_future_not_boxed() {
        // Форма скопирована у hyper::rt::Executor: генерик по F, ноль бондов
        // в объявлении. Send добавляет impl, а не трейт.
        struct Immediate;
        impl<F: std::future::Future<Output = ()>> Spawn<F> for Immediate {
            fn spawn(&self, f: F) { futures_executor::block_on(f) }
        }
        let done = std::rc::Rc::new(std::cell::Cell::new(false));
        let d = done.clone();
        // !Send future — трейт это допускает.
        Immediate.spawn(async move { d.set(true) });
        assert!(done.get());
    }
}
```

- [ ] **Step 2: Запустить и убедиться, что падает**

Run: `cargo test -p http-ng-rt`
Expected: FAIL — крейта нет.

- [ ] **Step 3: Создать крейт**

```toml
# crates/http-ng-rt/Cargo.toml
[package]
name = "http-ng-rt"
version = "0.1.0"
description = "Способности рантайма, нужные native-транспорту http-ng"
edition.workspace = true
rust-version.workspace = true
license.workspace = true
repository.workspace = true

[dependencies]
futures-io   = { version = "0.3", default-features = false, features = ["std"] }
http-ng-core = { workspace = true }
hyper        = { version = "1.11", default-features = false }

[dev-dependencies]
futures-executor = { version = "0.3", default-features = false, features = ["std"] }

[lints]
workspace = true
```

```rust
// crates/http-ng-rt/src/lib.rs
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
```

```rust
// crates/http-ng-rt/src/caps.rs
use std::future::Future;
use std::net::{IpAddr, SocketAddr};
use std::time::Duration;

/// Форма скопирована у `hyper::rt::Executor` намеренно: генерик по future,
/// ноль бондов в объявлении. `Send` добавляет `impl`, а не трейт, поэтому
/// однопоточные рантаймы реализуют его честно.
pub trait Spawn<F: Future<Output = ()>> {
    fn spawn(&self, f: F);
}

/// Опции сокета применяются в http-ng **один раз**, на `socket2::Socket`, и
/// рантайм только усыновляет дескриптор (`TcpAdoptStd`). Иначе каждый
/// рантайм-крейт переписывал бы эту простыню заново.
#[derive(Debug, Clone, Default)]
pub struct TcpOpts {
    pub nodelay: bool,
    pub keepalive: Option<Duration>,
    pub local_address: Option<IpAddr>,
    pub send_buffer_size: Option<usize>,
    pub recv_buffer_size: Option<usize>,
    pub reuse_address: bool,
}

pub trait TcpConnect {
    type Stream: hyper::rt::Read + hyper::rt::Write + Unpin;

    fn connect(
        &self,
        addr: SocketAddr,
        opts: &TcpOpts,
    ) -> impl Future<Output = std::io::Result<Self::Stream>>;
}

/// На платформах с файловыми дескрипторами весь набор socket-опций
/// применяется вне рантайма, а рантайм только усыновляет готовый сокет.
pub trait TcpAdoptStd: TcpConnect {
    fn adopt(&self, std: std::net::TcpStream) -> std::io::Result<Self::Stream>;
}

/// Отдельный трейт, а не метод: `getaddrinfo` блокирующий, а на wasm и
/// embedded блокирующего пула нет вовсе. Отсутствие способности должно быть
/// ошибкой компиляции, а не `unimplemented!()` в рантайме.
///
/// **Единственное место во всём проекте, где `Send` объявляем мы сами**, и он
/// здесь честен: и `tokio::task::spawn_blocking`, и `blocking::unblock`
/// требуют `Send + 'static`, а способности `Blocking` на wasm нет вовсе —
/// заражать ей нечего.
pub trait Blocking {
    fn run<T: Send + 'static, F: FnOnce() -> T + Send + 'static>(
        &self,
        f: F,
    ) -> impl Future<Output = T>;
}
```

- [ ] **Step 4: Создать заглушку `futures_io.rs` и запустить тесты**

```rust
// crates/http-ng-rt/src/futures_io.rs
// Реализация — Task 2.
pub struct FuturesIo<S> { pub(crate) inner: S }
```

Run: `cargo test -p http-ng-rt`
Expected: PASS, два теста.

- [ ] **Step 5: Commit**

```bash
git add crates/http-ng-rt
git commit -m "feat(rt): separate runtime capability traits instead of one Runtime"
```

---

### Task 2: `http-ng-rt` — шим `futures-io` → `hyper::rt`

Этого моста нет нигде: в hyper-util только `TokioIo`, а `smol-hyper` 0.1.1
мёртв с 2023-12-29 **и** реализует направление не в ту сторону. Без него
smol-бэкенда не существует.

**Files:**
- Modify: `crates/http-ng-rt/src/futures_io.rs`
- Test: внутри `futures_io.rs`

**Interfaces:**
- Consumes: `futures_io::{AsyncRead, AsyncWrite}`.
- Produces: `pub struct FuturesIo<S>`; `FuturesIo::new(inner: S) -> Self`;
  `FuturesIo::into_inner(self) -> S`;
  `impl<S: AsyncRead + Unpin> hyper::rt::Read for FuturesIo<S>`;
  `impl<S: AsyncWrite + Unpin> hyper::rt::Write for FuturesIo<S>`.

- [ ] **Step 1: Написать падающие тесты**

```rust
// crates/http-ng-rt/src/futures_io.rs
#[cfg(test)]
mod tests {
    use super::*;
    use futures_executor::block_on;
    use std::pin::Pin;

    /// Источник, отдающий данные порциями, чтобы поймать частичные чтения.
    struct Chunked { data: Vec<u8>, at: usize, step: usize }
    impl futures_io::AsyncRead for Chunked {
        fn poll_read(mut self: Pin<&mut Self>, _: &mut std::task::Context<'_>,
                     buf: &mut [u8]) -> std::task::Poll<std::io::Result<usize>> {
            let n = self.step.min(buf.len()).min(self.data.len() - self.at);
            buf[..n].copy_from_slice(&self.data[self.at..self.at + n]);
            self.at += n;
            std::task::Poll::Ready(Ok(n))
        }
    }

    fn read_all(mut io: FuturesIo<Chunked>) -> Vec<u8> {
        let mut out = Vec::new();
        let mut store = [0u8; 8];
        loop {
            let mut rb = hyper::rt::ReadBuf::new(&mut store);
            let poll = block_on(std::future::poll_fn(|cx| {
                hyper::rt::Read::poll_read(Pin::new(&mut io), cx, rb.unfilled())
            }));
            poll.unwrap();
            let filled = rb.filled().to_vec();
            if filled.is_empty() { return out }
            out.extend_from_slice(&filled);
        }
    }

    #[test]
    fn forwards_bytes_through_partial_reads() {
        let io = FuturesIo::new(Chunked { data: b"hello world".to_vec(), at: 0, step: 3 });
        assert_eq!(read_all(io), b"hello world");
    }

    #[test]
    fn never_writes_more_than_remaining() {
        // step больше, чем ёмкость буфера: put_slice не должен паниковать.
        let io = FuturesIo::new(Chunked { data: vec![7u8; 64], at: 0, step: 64 });
        assert_eq!(read_all(io).len(), 64);
    }

    #[test]
    fn into_inner_round_trips() {
        let io = FuturesIo::new(Chunked { data: vec![], at: 0, step: 1 });
        let c = io.into_inner();
        assert_eq!(c.step, 1);
    }
}
```

- [ ] **Step 2: Запустить и убедиться, что падает**

Run: `cargo test -p http-ng-rt futures_io`
Expected: FAIL — `no function new`.

- [ ] **Step 3: Реализовать**

```rust
// crates/http-ng-rt/src/futures_io.rs
use hyper::rt::ReadBufCursor;
use std::pin::Pin;
use std::task::{Context, Poll};

/// Мост `futures_io::{AsyncRead, AsyncWrite}` → `hyper::rt::{Read, Write}`.
///
/// В hyper-util есть только `TokioIo`; `smol-hyper` 0.1.1 мёртв с 2023-12-29 и
/// мостит в противоположную сторону. Поэтому мост наш.
///
/// Реализация **без `unsafe`**: читаем во временный буфер на стеке и копируем
/// через безопасный `ReadBufCursor::put_slice` — именно этот приём рекомендует
/// документация `hyper::rt::Read`. Цена — одно копирование на чтение;
/// zero-copy требует `unsafe as_mut`/`advance` и отложен.
#[derive(Debug)]
pub struct FuturesIo<S> {
    inner: S,
}

/// Размер стекового буфера. 8 KiB — типичный размер чтения у hyper,
/// поэтому лишних итераций не возникает.
const SCRATCH: usize = 8 * 1024;

impl<S> FuturesIo<S> {
    pub fn new(inner: S) -> Self {
        Self { inner }
    }
    pub fn into_inner(self) -> S {
        self.inner
    }
    pub fn get_ref(&self) -> &S {
        &self.inner
    }
}

impl<S: futures_io::AsyncRead + Unpin> hyper::rt::Read for FuturesIo<S> {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        mut buf: ReadBufCursor<'_>,
    ) -> Poll<std::io::Result<()>> {
        let want = buf.remaining().min(SCRATCH);
        if want == 0 {
            return Poll::Ready(Ok(()));
        }
        let mut scratch = [0u8; SCRATCH];
        let n = std::task::ready!(
            Pin::new(&mut self.inner).poll_read(cx, &mut scratch[..want])
        )?;
        buf.put_slice(&scratch[..n]);
        Poll::Ready(Ok(()))
    }
}

impl<S: futures_io::AsyncWrite + Unpin> hyper::rt::Write for FuturesIo<S> {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        Pin::new(&mut self.inner).poll_write(cx, buf)
    }

    fn poll_flush(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.inner).poll_flush(cx)
    }

    fn poll_shutdown(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.inner).poll_close(cx)
    }

    fn poll_write_vectored(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bufs: &[std::io::IoSlice<'_>],
    ) -> Poll<std::io::Result<usize>> {
        Pin::new(&mut self.inner).poll_write_vectored(cx, bufs)
    }

    fn is_write_vectored(&self) -> bool {
        true
    }
}
```

- [ ] **Step 4: Запустить тесты**

Run: `cargo test -p http-ng-rt`
Expected: PASS, пять тестов.

- [ ] **Step 5: Проверить, что unsafe действительно нет**

Run: `! grep -rn "unsafe" crates/http-ng-rt/src && echo OK`
Expected: `OK`.

- [ ] **Step 6: Commit**

```bash
git add crates/http-ng-rt
git commit -m "feat(rt): safe futures-io to hyper::rt bridge, missing from the ecosystem"
```

---

### Task 3: `http-ng-rt-tokio`

**Files:**
- Create: `crates/http-ng-rt-tokio/Cargo.toml`, `src/lib.rs`
- Test: внутри `lib.rs`

**Interfaces:**
- Consumes: трейты Task 1, `FuturesIo` Task 2.
- Produces: `pub struct Tokio`; `impl Timer for Tokio { type Instant = tokio::time::Instant }`;
  `impl<F: Future<Output=()> + Send + 'static> Spawn<F> for Tokio`;
  `impl TcpConnect for Tokio { type Stream = TokioIo }`; `impl TcpAdoptStd for Tokio`;
  `impl Blocking for Tokio`; `pub struct TokioIo(tokio::net::TcpStream)` —
  реализует `hyper::rt::Read/Write` напрямую (без `FuturesIo`, потому что у
  tokio свои IO-трейты).

- [ ] **Step 1: Написать падающие тесты**

```rust
// crates/http-ng-rt-tokio/src/lib.rs
#[cfg(test)]
mod tests {
    use super::*;
    use http_ng_rt::{Blocking, TcpConnect, TcpOpts, Timer};
    use std::time::Duration;

    #[tokio::test]
    async fn timer_sleeps_and_measures() {
        let t = Tokio;
        let start = t.now();
        t.sleep(Duration::from_millis(20)).await;
        assert!(t.elapsed_since(start) >= Duration::from_millis(20));
    }

    #[tokio::test]
    async fn blocking_runs_off_the_reactor() {
        let out = Tokio.run(|| 6 * 7).await;
        assert_eq!(out, 42);
    }

    #[tokio::test]
    async fn connects_to_a_local_listener_with_options() {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = l.local_addr().unwrap();
        std::thread::spawn(move || { let _ = l.accept(); });

        let opts = TcpOpts { nodelay: true, ..Default::default() };
        let s = Tokio.connect(addr, &opts).await.expect("connect");
        drop(s);
    }
}
```

- [ ] **Step 2: Запустить и убедиться, что падает**

Run: `cargo test -p http-ng-rt-tokio`
Expected: FAIL — крейта нет.

- [ ] **Step 3: Создать крейт и реализовать**

```toml
# crates/http-ng-rt-tokio/Cargo.toml
[package]
name = "http-ng-rt-tokio"
version = "0.1.0"
description = "Реализация способностей рантайма http-ng поверх tokio"
edition.workspace = true
rust-version.workspace = true
license.workspace = true
repository.workspace = true

[dependencies]
http-ng-rt = { path = "../http-ng-rt", version = "0.1.0" }
hyper      = { version = "1.11", default-features = false }
socket2    = { version = "0.6", features = ["all"] }
tokio      = { version = "1", features = ["net", "rt", "time", "sync"] }

[dev-dependencies]
tokio = { version = "1", features = ["macros", "rt-multi-thread"] }

[lints]
workspace = true
```

```rust
// crates/http-ng-rt-tokio/src/lib.rs
//! Реализация способностей `http-ng-rt` поверх tokio.
#![deny(unsafe_code)]

mod io;

pub use io::TokioIo;

use http_ng_rt::{Blocking, Spawn, TcpAdoptStd, TcpConnect, TcpOpts, Timer};
use std::future::Future;
use std::net::SocketAddr;
use std::time::Duration;

/// ZST: tokio-хендл берётся из окружающего рантайма, как это делает reqwest.
/// Вне рантайма `spawn`/`sleep` паникуют — задокументировано.
#[derive(Debug, Clone, Copy, Default)]
pub struct Tokio;

impl Timer for Tokio {
    type Instant = tokio::time::Instant;
    fn sleep(&self, d: Duration) -> impl Future<Output = ()> {
        tokio::time::sleep(d)
    }
    fn now(&self) -> Self::Instant {
        tokio::time::Instant::now()
    }
    fn elapsed_since(&self, earlier: Self::Instant) -> Duration {
        tokio::time::Instant::now().saturating_duration_since(earlier)
    }
}

impl<F: Future<Output = ()> + Send + 'static> Spawn<F> for Tokio {
    fn spawn(&self, f: F) {
        tokio::spawn(f);
    }
}

impl Blocking for Tokio {
    fn run<T: Send + 'static, F: FnOnce() -> T + Send + 'static>(&self, f: F)
        -> impl Future<Output = T>
    {
        async move {
            tokio::task::spawn_blocking(f).await.expect("blocking task panicked")
        }
    }
}

impl TcpConnect for Tokio {
    type Stream = TokioIo;

    async fn connect(&self, addr: SocketAddr, opts: &TcpOpts) -> std::io::Result<TokioIo> {
        // Опции применяются на `socket2::Socket` **один раз**, а рантайм
        // усыновляет готовый дескриптор. Это и есть шов `TcpAdoptStd`:
        // без него каждый рантайм-крейт переписывал бы эту простыню заново.
        let sock = build_socket(addr, opts)?;
        sock.set_nonblocking(true)?;
        let std_stream: std::net::TcpStream = sock.into();
        let tcp = tokio::net::TcpSocket::from_std_stream(std_stream)
            .connect(addr)
            .await?;
        apply_post_connect(&tcp, opts)?;
        Ok(TokioIo::new(tcp))
    }
}

impl TcpAdoptStd for Tokio {
    fn adopt(&self, std: std::net::TcpStream) -> std::io::Result<TokioIo> {
        std.set_nonblocking(true)?;
        Ok(TokioIo::new(tokio::net::TcpStream::from_std(std)?))
    }
}

fn build_socket(addr: SocketAddr, opts: &TcpOpts) -> std::io::Result<socket2::Socket> {
    let domain = socket2::Domain::for_address(addr);
    let sock = socket2::Socket::new(domain, socket2::Type::STREAM, Some(socket2::Protocol::TCP))?;
    if opts.reuse_address {
        sock.set_reuse_address(true)?;
    }
    if let Some(size) = opts.send_buffer_size {
        sock.set_send_buffer_size(size)?;
    }
    if let Some(size) = opts.recv_buffer_size {
        sock.set_recv_buffer_size(size)?;
    }
    if let Some(ip) = opts.local_address {
        sock.bind(&SocketAddr::new(ip, 0).into())?;
    }
    Ok(sock)
}

fn apply_post_connect(tcp: &tokio::net::TcpStream, opts: &TcpOpts) -> std::io::Result<()> {
    if opts.nodelay {
        tcp.set_nodelay(true)?;
    }
    if let Some(d) = opts.keepalive {
        let sock = socket2::SockRef::from(tcp);
        sock.set_tcp_keepalive(&socket2::TcpKeepalive::new().with_time(d))?;
    }
    Ok(())
}
```

```rust
// crates/http-ng-rt-tokio/src/io.rs
use hyper::rt::ReadBufCursor;
use std::pin::Pin;
use std::task::{Context, Poll};
use tokio::io::{AsyncRead, AsyncWrite};

/// Мост `tokio::net::TcpStream` → `hyper::rt`. Без `unsafe`: читаем во
/// временный буфер и копируем безопасным `put_slice`.
#[derive(Debug)]
pub struct TokioIo(tokio::net::TcpStream);

const SCRATCH: usize = 8 * 1024;

impl TokioIo {
    pub(crate) fn new(s: tokio::net::TcpStream) -> Self { Self(s) }
}

impl hyper::rt::Read for TokioIo {
    fn poll_read(mut self: Pin<&mut Self>, cx: &mut Context<'_>, mut buf: ReadBufCursor<'_>)
        -> Poll<std::io::Result<()>>
    {
        let want = buf.remaining().min(SCRATCH);
        if want == 0 { return Poll::Ready(Ok(())) }
        let mut scratch = [0u8; SCRATCH];
        let mut rb = tokio::io::ReadBuf::new(&mut scratch[..want]);
        std::task::ready!(Pin::new(&mut self.0).poll_read(cx, &mut rb))?;
        let filled = rb.filled().len();
        buf.put_slice(&scratch[..filled]);
        Poll::Ready(Ok(()))
    }
}

impl hyper::rt::Write for TokioIo {
    fn poll_write(mut self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &[u8])
        -> Poll<std::io::Result<usize>>
    { Pin::new(&mut self.0).poll_write(cx, buf) }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>)
        -> Poll<std::io::Result<()>>
    { Pin::new(&mut self.0).poll_flush(cx) }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>)
        -> Poll<std::io::Result<()>>
    { Pin::new(&mut self.0).poll_shutdown(cx) }

    fn poll_write_vectored(mut self: Pin<&mut Self>, cx: &mut Context<'_>,
                           bufs: &[std::io::IoSlice<'_>]) -> Poll<std::io::Result<usize>>
    { Pin::new(&mut self.0).poll_write_vectored(cx, bufs) }

    fn is_write_vectored(&self) -> bool { self.0.is_write_vectored() }
}
```

- [ ] **Step 4: Запустить тесты**

Run: `cargo test -p http-ng-rt-tokio`
Expected: PASS, три теста.

- [ ] **Step 5: Commit**

```bash
git add crates/http-ng-rt-tokio
git commit -m "feat(rt-tokio): tokio implementation of the runtime capabilities"
```

---

### Task 4: `http-ng-rt-smol`

Тот же набор способностей на smol. Именно эта задача доказывает, что шов
настоящий: если здесь понадобится `#[cfg]` в общем коде — шов декоративен.

**Files:**
- Create: `crates/http-ng-rt-smol/Cargo.toml`, `src/lib.rs`
- Test: внутри `lib.rs`

**Interfaces:**
- Consumes: то же, что Task 3, плюс `FuturesIo` (у smol IO уже `futures-io`,
  поэтому отдельный `SmolIo` не нужен).
- Produces: `pub struct Smol`; `impl Timer for Smol { type Instant = std::time::Instant }`;
  `impl<F: Future<Output=()> + Send + 'static> Spawn<F> for Smol`;
  `impl TcpConnect for Smol { type Stream = FuturesIo<async_net::TcpStream> }`;
  `impl TcpAdoptStd for Smol`; `impl Blocking for Smol`.

- [ ] **Step 1: Написать падающие тесты**

```rust
// crates/http-ng-rt-smol/src/lib.rs
#[cfg(test)]
mod tests {
    use super::*;
    use http_ng_rt::{Blocking, TcpConnect, TcpOpts, Timer};
    use std::time::Duration;

    #[test]
    fn timer_sleeps_and_measures() {
        futures_executor::block_on(async {
            let t = Smol;
            let start = t.now();
            t.sleep(Duration::from_millis(20)).await;
            assert!(t.elapsed_since(start) >= Duration::from_millis(20));
        });
    }

    #[test]
    fn blocking_runs_off_the_reactor() {
        let out = futures_executor::block_on(Smol.run(|| 6 * 7));
        assert_eq!(out, 42);
    }

    #[test]
    fn connects_to_a_local_listener() {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = l.local_addr().unwrap();
        std::thread::spawn(move || { let _ = l.accept(); });
        futures_executor::block_on(async {
            let s = Smol.connect(addr, &TcpOpts { nodelay: true, ..Default::default() })
                .await.expect("connect");
            drop(s);
        });
    }
}
```

- [ ] **Step 2: Запустить и убедиться, что падает**

Run: `cargo test -p http-ng-rt-smol`
Expected: FAIL — крейта нет.

- [ ] **Step 3: Создать крейт и реализовать**

```toml
# crates/http-ng-rt-smol/Cargo.toml
[package]
name = "http-ng-rt-smol"
version = "0.1.0"
description = "Реализация способностей рантайма http-ng поверх smol"
edition.workspace = true
rust-version.workspace = true
license.workspace = true
repository.workspace = true

[dependencies]
async-net     = "2"
async-io      = "2"
blocking      = "1"
futures-lite  = "2"
http-ng-rt    = { path = "../http-ng-rt", version = "0.1.0" }
socket2       = { version = "0.6", features = ["all"] }

[dev-dependencies]
futures-executor = { version = "0.3", default-features = false, features = ["std"] }

[lints]
workspace = true
```

```rust
// crates/http-ng-rt-smol/src/lib.rs
//! Реализация способностей `http-ng-rt` поверх smol.
//!
//! **Никакого `async-compat`.** Он поднимает второй рантайм в процессе, если
//! tokio-контекст не найден, — то есть скрывает ровно ту проблему, которую эта
//! вертикаль должна выявить.
#![deny(unsafe_code)]

use http_ng_rt::{Blocking, FuturesIo, Spawn, TcpAdoptStd, TcpConnect, TcpOpts, Timer};
use std::future::Future;
use std::net::SocketAddr;
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Copy, Default)]
pub struct Smol;

impl Timer for Smol {
    type Instant = Instant;
    fn sleep(&self, d: Duration) -> impl Future<Output = ()> {
        async move { async_io::Timer::after(d).await; }
    }
    fn now(&self) -> Instant { Instant::now() }
    fn elapsed_since(&self, earlier: Instant) -> Duration {
        Instant::now().saturating_duration_since(earlier)
    }
}

impl<F: Future<Output = ()> + Send + 'static> Spawn<F> for Smol {
    fn spawn(&self, f: F) {
        // `detach` намеренно: время жизни задачи привязано к соединению,
        // а не к вызывающему.
        smol_spawn(f);
    }
}

fn smol_spawn<F: Future<Output = ()> + Send + 'static>(f: F) {
    static EXEC: std::sync::OnceLock<async_executor::Executor<'static>> =
        std::sync::OnceLock::new();
    let ex = EXEC.get_or_init(|| {
        let ex = async_executor::Executor::new();
        std::thread::Builder::new()
            .name("http-ng-smol".into())
            .spawn(|| futures_lite::future::block_on(
                EXEC.get().expect("initialised").run(std::future::pending::<()>())
            ))
            .expect("spawn executor thread");
        ex
    });
    ex.spawn(f).detach();
}

impl Blocking for Smol {
    fn run<T, F: FnOnce() -> T>(&self, f: F) -> impl Future<Output = T> {
        // `blocking` — тот же пул, что использует сам smol.
        async move { blocking::unblock(f).await }
    }
}

impl TcpConnect for Smol {
    type Stream = FuturesIo<async_net::TcpStream>;

    async fn connect(&self, addr: SocketAddr, opts: &TcpOpts)
        -> std::io::Result<Self::Stream>
    {
        let tcp = async_net::TcpStream::connect(addr).await?;
        if opts.nodelay { tcp.set_nodelay(true)?; }
        if let Some(d) = opts.keepalive {
            let sock = socket2::SockRef::from(&tcp);
            sock.set_tcp_keepalive(&socket2::TcpKeepalive::new().with_time(d))?;
        }
        Ok(FuturesIo::new(tcp))
    }
}

impl TcpAdoptStd for Smol {
    fn adopt(&self, std: std::net::TcpStream) -> std::io::Result<Self::Stream> {
        std.set_nonblocking(true)?;
        Ok(FuturesIo::new(async_net::TcpStream::try_from(std)?))
    }
}
```

Добавить `async-executor = "1"` в зависимости. Бонды `Send + 'static` на
`Blocking::run` уже стоят в Task 1 — их требует `blocking::unblock`.

- [ ] **Step 4: Запустить тесты**

Run: `cargo test -p http-ng-rt-smol`
Expected: PASS, три теста.

- [ ] **Step 5: Убедиться, что `async-compat` не появился в графе**

Run: `! cargo tree -p http-ng-rt-smol -e normal --prefix none | grep -q async-compat && echo OK`
Expected: `OK`.

- [ ] **Step 6: Commit**

```bash
git add crates/http-ng-rt-smol crates/http-ng-rt
git commit -m "feat(rt-smol): smol implementation without async-compat"
```

---

### Task 5: `http-ng-proto` — планировщик Happy Eyeballs (RFC 8305)

Готового нет: `happy-eyeballs` 0.2.1 мёртв с 2023-05, `happyeyeballs` сам
объявляет себя не-RFC-совместимым, а hyper-util реализует **RFC 6555** за
запечатанным трейтом. Планировщик чистый и принимает `now` параметром — значит
константы 50 мс и 250 мс тестируются **без единого `sleep`**.

**Files:**
- Create: `crates/http-ng-proto/src/happy_eyeballs.rs`
- Modify: `crates/http-ng-proto/src/lib.rs`
- Test: внутри `happy_eyeballs.rs`

**Interfaces:**
- Consumes: ничего.
- Produces:
  - `pub struct HeConfig { pub resolution_delay: Duration, pub attempt_delay: Duration, pub first_family_count: usize }` (`Default` = 50 мс / 250 мс / 1)
  - `pub struct Scheduler`; `Scheduler::new(cfg: HeConfig) -> Self`
  - `Scheduler::offer_v6(&mut self, addrs: &[IpAddr])`, `offer_v4(&mut self, addrs: &[IpAddr])`
  - `Scheduler::mark_v6_done(&mut self)`, `mark_v4_done(&mut self)`
  - `Scheduler::poll(&mut self, elapsed: Duration) -> HeAction`
  - `pub enum HeAction { Start(IpAddr), Wait(Duration), Exhausted }`

- [ ] **Step 1: Написать падающие тесты**

```rust
// crates/http-ng-proto/src/happy_eyeballs.rs
#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

    fn v6(n: u16) -> IpAddr { IpAddr::V6(Ipv6Addr::new(0x20, 0, 0, 0, 0, 0, 0, n)) }
    fn v4(n: u8) -> IpAddr { IpAddr::V4(Ipv4Addr::new(10, 0, 0, n)) }
    fn ms(n: u64) -> Duration { Duration::from_millis(n) }

    #[test]
    fn prefers_ipv6_first() {
        let mut s = Scheduler::new(HeConfig::default());
        s.offer_v6(&[v6(1)]);
        s.offer_v4(&[v4(1)]);
        s.mark_v6_done(); s.mark_v4_done();
        assert_eq!(s.poll(ms(0)), HeAction::Start(v6(1)));
    }

    #[test]
    fn waits_resolution_delay_for_ipv6_before_falling_back_to_ipv4() {
        let mut s = Scheduler::new(HeConfig::default());
        s.offer_v4(&[v4(1)]);
        s.mark_v4_done();
        // AAAA ещё не пришли: RFC 8305 §3 велит подождать Resolution Delay.
        assert_eq!(s.poll(ms(0)), HeAction::Wait(ms(50)));
        assert_eq!(s.poll(ms(50)), HeAction::Start(v4(1)));
    }

    #[test]
    fn interleaves_families_with_first_family_count_of_one() {
        let mut s = Scheduler::new(HeConfig::default());
        s.offer_v6(&[v6(1), v6(2)]);
        s.offer_v4(&[v4(1), v4(2)]);
        s.mark_v6_done(); s.mark_v4_done();
        assert_eq!(s.poll(ms(0)),   HeAction::Start(v6(1)));
        assert_eq!(s.poll(ms(250)), HeAction::Start(v4(1)));
        assert_eq!(s.poll(ms(500)), HeAction::Start(v6(2)));
        assert_eq!(s.poll(ms(750)), HeAction::Start(v4(2)));
    }

    #[test]
    fn enforces_the_attempt_delay_between_starts() {
        let mut s = Scheduler::new(HeConfig::default());
        s.offer_v6(&[v6(1), v6(2)]);
        s.mark_v6_done(); s.mark_v4_done();
        assert_eq!(s.poll(ms(0)),   HeAction::Start(v6(1)));
        assert_eq!(s.poll(ms(100)), HeAction::Wait(ms(150)));
        assert_eq!(s.poll(ms(250)), HeAction::Start(v6(2)));
    }

    #[test]
    fn reports_exhausted_when_everything_is_started_and_resolvers_are_done() {
        let mut s = Scheduler::new(HeConfig::default());
        s.offer_v6(&[v6(1)]);
        s.mark_v6_done(); s.mark_v4_done();
        assert_eq!(s.poll(ms(0)), HeAction::Start(v6(1)));
        assert_eq!(s.poll(ms(999)), HeAction::Exhausted);
    }

    #[test]
    fn attempt_delay_is_clamped_to_the_rfc_range() {
        let c = HeConfig { attempt_delay: ms(1), ..Default::default() };
        assert_eq!(Scheduler::new(c).config().attempt_delay, ms(10), "нижняя граница RFC 8305");
        let c = HeConfig { attempt_delay: Duration::from_secs(30), ..Default::default() };
        assert_eq!(Scheduler::new(c).config().attempt_delay, Duration::from_secs(2),
                   "верхняя граница RFC 8305");
    }
}
```

- [ ] **Step 2: Запустить и убедиться, что падает**

Run: `cargo test -p http-ng-proto happy`
Expected: FAIL — `cannot find type Scheduler`.

- [ ] **Step 3: Реализовать**

```rust
// crates/http-ng-proto/src/happy_eyeballs.rs
//! Планировщик Happy Eyeballs v2 (RFC 8305). Чистый: время приходит
//! параметром `elapsed`, поэтому константы проверяются без `sleep`.

use core::time::Duration;
use std::collections::VecDeque;
use std::net::IpAddr;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HeConfig {
    /// RFC 8305 §3: ждать AAAA столько, прежде чем идти по A.
    pub resolution_delay: Duration,
    /// RFC 8305 §5: пауза между запусками попыток. Clamp 10 мс…2 с.
    pub attempt_delay: Duration,
    /// RFC 8305 §4: сколько адресов первого семейства идёт подряд.
    pub first_family_count: usize,
}

impl Default for HeConfig {
    fn default() -> Self {
        Self {
            resolution_delay: Duration::from_millis(50),
            attempt_delay: Duration::from_millis(250),
            first_family_count: 1,
        }
    }
}

const ATTEMPT_MIN: Duration = Duration::from_millis(10);
const ATTEMPT_MAX: Duration = Duration::from_secs(2);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HeAction {
    Start(IpAddr),
    Wait(Duration),
    Exhausted,
}

#[derive(Debug)]
pub struct Scheduler {
    cfg: HeConfig,
    v6: VecDeque<IpAddr>,
    v4: VecDeque<IpAddr>,
    v6_done: bool,
    v4_done: bool,
    started: usize,
    last_start: Option<Duration>,
    /// Сколько адресов первого семейства уже выдано подряд.
    run_in_first_family: usize,
}

impl Scheduler {
    pub fn new(mut cfg: HeConfig) -> Self {
        cfg.attempt_delay = cfg.attempt_delay.clamp(ATTEMPT_MIN, ATTEMPT_MAX);
        Self {
            cfg,
            v6: VecDeque::new(),
            v4: VecDeque::new(),
            v6_done: false,
            v4_done: false,
            started: 0,
            last_start: None,
            run_in_first_family: 0,
        }
    }

    pub fn config(&self) -> &HeConfig { &self.cfg }

    pub fn offer_v6(&mut self, addrs: &[IpAddr]) { self.v6.extend(addrs.iter().copied()) }
    pub fn offer_v4(&mut self, addrs: &[IpAddr]) { self.v4.extend(addrs.iter().copied()) }
    pub fn mark_v6_done(&mut self) { self.v6_done = true }
    pub fn mark_v4_done(&mut self) { self.v4_done = true }

    pub fn poll(&mut self, elapsed: Duration) -> HeAction {
        // Пауза между попытками.
        if let Some(last) = self.last_start {
            let next_at = last + self.cfg.attempt_delay;
            if elapsed < next_at {
                return HeAction::Wait(next_at - elapsed);
            }
        }

        // RFC 8305 §3: пока AAAA не пришли и резолвер не закончил, придержать
        // IPv4 на Resolution Delay.
        if self.v6.is_empty() && !self.v6_done && elapsed < self.cfg.resolution_delay {
            return HeAction::Wait(self.cfg.resolution_delay - elapsed);
        }

        let take_v6 = if self.v6.is_empty() {
            false
        } else if self.v4.is_empty() {
            true
        } else if self.started == 0 {
            true // первым всегда IPv6
        } else {
            // Интерливинг: после First Address Family Count адресов первого
            // семейства чередуем.
            self.run_in_first_family < self.cfg.first_family_count
        };

        let picked = if take_v6 { self.v6.pop_front() } else { self.v4.pop_front() };

        match picked {
            Some(addr) => {
                self.started += 1;
                self.last_start = Some(elapsed);
                self.run_in_first_family = if take_v6 { self.run_in_first_family + 1 } else { 0 };
                HeAction::Start(addr)
            }
            None if self.v6_done && self.v4_done => HeAction::Exhausted,
            None => HeAction::Wait(self.cfg.resolution_delay),
        }
    }
}
```

- [ ] **Step 4: Подключить и запустить**

Добавить `pub mod happy_eyeballs;` в `crates/http-ng-proto/src/lib.rs`.

Run: `cargo test -p http-ng-proto`
Expected: PASS, шесть тестов Happy Eyeballs плюс всё из вертикали 1.

- [ ] **Step 5: Добавить фазз-таргет**

```rust
// crates/http-ng-proto/fuzz/fuzz_targets/happy_eyeballs.rs
#![no_main]
use libfuzzer_sys::fuzz_target;
use http_ng_proto::happy_eyeballs::{HeAction, HeConfig, Scheduler};
use std::net::{IpAddr, Ipv4Addr};
use core::time::Duration;

// Инвариант: планировщик всегда сходится к Exhausted и не паникует.
fuzz_target!(|data: &[u8]| {
    let mut s = Scheduler::new(HeConfig::default());
    let addrs: Vec<IpAddr> = data.iter().take(16)
        .map(|b| IpAddr::V4(Ipv4Addr::new(10, 0, 0, *b))).collect();
    s.offer_v4(&addrs);
    s.mark_v4_done();
    s.mark_v6_done();
    let mut t = Duration::ZERO;
    for _ in 0..64 {
        match s.poll(t) {
            HeAction::Start(_) => {}
            HeAction::Wait(d) => t += d.max(Duration::from_millis(1)),
            HeAction::Exhausted => return,
        }
        t += Duration::from_millis(1);
    }
});
```

Добавить второй `[[bin]]` в `crates/http-ng-proto/fuzz/Cargo.toml`.

Run: `cd crates/http-ng-proto/fuzz && cargo +nightly fuzz run happy_eyeballs -- -max_total_time=60`
Expected: 60 секунд без паник.

- [ ] **Step 6: Commit**

```bash
git add crates/http-ng-proto
git commit -m "feat(proto): RFC 8305 Happy Eyeballs scheduler testable without sleeping"
```

---

### Task 6: `http-ng-dns` — трейт резолвера

**Files:**
- Create: `crates/http-ng-dns/Cargo.toml`, `src/lib.rs`
- Test: внутри `lib.rs`

**Interfaces:**
- Produces:
  - `pub struct ResolvedAddr { pub addr: IpAddr, pub ttl: Option<Duration> }`
  - `pub struct SvcbEndpoint { pub priority: u16, pub target: String, pub alpn: Vec<Vec<u8>>, pub port: Option<u16>, pub ipv4hint: Vec<Ipv4Addr>, pub ipv6hint: Vec<Ipv6Addr>, pub ech_config_list: Option<bytes::Bytes> }`
  - `pub trait Resolve { fn lookup_ipv4(&self, name: &str) -> impl Stream<Item = Result<ResolvedAddr, Error>>; fn lookup_ipv6(&self, name: &str) -> impl Stream<Item = Result<ResolvedAddr, Error>>; fn lookup_svcb(&self, _name: &str) -> impl Stream<Item = Result<SvcbEndpoint, Error>> { futures_util::stream::empty() } }`

- [ ] **Step 1: Написать падающий тест**

```rust
// crates/http-ng-dns/src/lib.rs
#[cfg(test)]
mod tests {
    use super::*;
    use futures_util::StreamExt;

    struct Static;
    impl Resolve for Static {
        fn lookup_ipv4(&self, _: &str) -> impl futures_core::Stream<Item = Result<ResolvedAddr, Error>> {
            futures_util::stream::iter(vec![Ok(ResolvedAddr {
                addr: "127.0.0.1".parse().unwrap(), ttl: None,
            })])
        }
        fn lookup_ipv6(&self, _: &str) -> impl futures_core::Stream<Item = Result<ResolvedAddr, Error>> {
            futures_util::stream::empty()
        }
        // lookup_svcb намеренно не реализован — дефолт обязан работать.
    }

    #[test]
    fn svcb_has_a_default_returning_empty() {
        let got: Vec<_> = futures_executor::block_on(Static.lookup_svcb("x").collect());
        assert!(got.is_empty(),
            "иначе getaddrinfo, wasi и embedded не смогли бы реализовать трейт");
    }

    #[test]
    fn families_are_separate_streams() {
        let v4: Vec<_> = futures_executor::block_on(Static.lookup_ipv4("x").collect());
        let v6: Vec<_> = futures_executor::block_on(Static.lookup_ipv6("x").collect());
        assert_eq!(v4.len(), 1);
        assert_eq!(v6.len(), 0, "по AAAA надо коннектиться, не дожидаясь A");
    }
}
```

- [ ] **Step 2: Запустить и убедиться, что падает**

Run: `cargo test -p http-ng-dns`
Expected: FAIL — крейта нет.

- [ ] **Step 3: Создать и реализовать**

```toml
# crates/http-ng-dns/Cargo.toml
[package]
name = "http-ng-dns"
version = "0.1.0"
description = "Трейт подключаемого резолвера для http-ng"
edition.workspace = true
rust-version.workspace = true
license.workspace = true
repository.workspace = true

[dependencies]
bytes         = { workspace = true }
futures-core  = { workspace = true }
futures-util  = { version = "0.3", default-features = false, features = ["std"] }
http-ng-core  = { workspace = true }

[dev-dependencies]
futures-executor = { version = "0.3", default-features = false, features = ["std"] }

[lints]
workspace = true
```

```rust
// crates/http-ng-dns/src/lib.rs
//! Подключаемое разрешение имён.
//!
//! Раздельные стримы по семействам, а не `Vec<SocketAddr>`: RFC 8305 требует
//! начинать соединяться по AAAA, не дожидаясь A. `lookup_svcb` имеет
//! дефолтную реализацию, возвращающую пусто, — иначе `getaddrinfo`, `wasi:http`
//! и embedded не смогли бы реализовать трейт.
#![deny(unsafe_code)]

use bytes::Bytes;
use futures_core::Stream;
use http_ng_core::Error;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::time::Duration;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedAddr {
    pub addr: IpAddr,
    pub ttl: Option<Duration>,
}

/// RFC 9460 HTTPS/SVCB. `alpn` даёт обнаружение h3 без Alt-Svc,
/// `ech_config_list` кормит `rustls::EchConfig` напрямую.
///
/// Заложено с первого дня: если зафиксировать резолвер на `SocketAddr`, ECH и
/// h3-discovery закрыты навсегда без ломающего изменения.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SvcbEndpoint {
    pub priority: u16,
    pub target: String,
    pub alpn: Vec<Vec<u8>>,
    pub port: Option<u16>,
    pub ipv4hint: Vec<Ipv4Addr>,
    pub ipv6hint: Vec<Ipv6Addr>,
    pub ech_config_list: Option<Bytes>,
}

pub trait Resolve {
    fn lookup_ipv4(&self, name: &str) -> impl Stream<Item = Result<ResolvedAddr, Error>>;
    fn lookup_ipv6(&self, name: &str) -> impl Stream<Item = Result<ResolvedAddr, Error>>;

    fn lookup_svcb(&self, _name: &str) -> impl Stream<Item = Result<SvcbEndpoint, Error>> {
        futures_util::stream::empty()
    }
}
```

- [ ] **Step 4: Запустить тесты**

Run: `cargo test -p http-ng-dns`
Expected: PASS, два теста.

- [ ] **Step 5: Commit**

```bash
git add crates/http-ng-dns
git commit -m "feat(dns): Resolve trait with per-family streams and a defaulted SVCB lookup"
```

---

### Task 7: `http-ng-dns-system` — getaddrinfo через `Blocking`

**Files:**
- Create: `crates/http-ng-dns-system/Cargo.toml`, `src/lib.rs`
- Test: внутри `lib.rs`

**Interfaces:**
- Consumes: `Resolve` (Task 6), `Blocking` (Task 1).
- Produces: `pub struct SystemDns<B> { blocking: B }`; `SystemDns::new(b: B) -> Self`;
  `impl<B: Blocking> Resolve for SystemDns<B>`.

- [ ] **Step 1: Написать падающие тесты**

```rust
// crates/http-ng-dns-system/src/lib.rs
#[cfg(test)]
mod tests {
    use super::*;
    use futures_util::StreamExt;
    use http_ng_rt::Blocking;

    struct Inline;
    impl Blocking for Inline {
        fn run<T: Send + 'static, F: FnOnce() -> T + Send + 'static>(&self, f: F)
            -> impl std::future::Future<Output = T> { async move { f() } }
    }

    #[test]
    fn resolves_localhost_into_the_right_family_streams() {
        let r = SystemDns::new(Inline);
        let v4: Vec<_> = futures_executor::block_on(r.lookup_ipv4("localhost").collect());
        let v4: Vec<_> = v4.into_iter().filter_map(Result::ok).collect();
        assert!(v4.iter().all(|a| a.addr.is_ipv4()), "в v4-стриме только v4");

        let v6: Vec<_> = futures_executor::block_on(r.lookup_ipv6("localhost").collect());
        let v6: Vec<_> = v6.into_iter().filter_map(Result::ok).collect();
        assert!(v6.iter().all(|a| a.addr.is_ipv6()), "в v6-стриме только v6");

        assert!(!v4.is_empty() || !v6.is_empty(), "localhost должен резолвиться");
    }

    #[test]
    fn unresolvable_name_yields_an_error_not_an_empty_stream() {
        let r = SystemDns::new(Inline);
        let got: Vec<_> = futures_executor::block_on(
            r.lookup_ipv4("invalid.invalid.").collect());
        assert!(got.iter().any(|x| x.is_err()),
                "пустой стрим неотличим от «политика всё отфильтровала»");
    }

    #[test]
    fn svcb_is_empty_because_getaddrinfo_cannot_return_it() {
        let r = SystemDns::new(Inline);
        let got: Vec<_> = futures_executor::block_on(r.lookup_svcb("example.com").collect());
        assert!(got.is_empty());
    }
}
```

- [ ] **Step 2: Запустить и убедиться, что падает**

Run: `cargo test -p http-ng-dns-system`
Expected: FAIL — крейта нет.

- [ ] **Step 3: Создать и реализовать**

```toml
# crates/http-ng-dns-system/Cargo.toml
[package]
name = "http-ng-dns-system"
version = "0.1.0"
description = "Системный резолвер (getaddrinfo) для http-ng через способность Blocking"
edition.workspace = true
rust-version.workspace = true
license.workspace = true
repository.workspace = true

[dependencies]
futures-core = { workspace = true }
futures-util = { version = "0.3", default-features = false, features = ["std"] }
http-ng-core = { workspace = true }
http-ng-dns  = { path = "../http-ng-dns", version = "0.1.0" }
http-ng-rt   = { path = "../http-ng-rt",  version = "0.1.0" }

[dev-dependencies]
futures-executor = { version = "0.3", default-features = false, features = ["std"] }

[lints]
workspace = true
```

```rust
// crates/http-ng-dns-system/src/lib.rs
//! Системный резолвер поверх `std::net::ToSocketAddrs` (то есть `getaddrinfo`).
//!
//! `getaddrinfo` блокирующий на всех платформах, поэтому крейт требует
//! способности `Blocking` — и потому недоступен там, где её нет (wasm).
//!
//! **Ограничение, которое надо знать:** `getaddrinfo` никогда не вернёт
//! HTTPS/SVCB-записи. Значит на системном резолвере недостижимы ни ECH, ни
//! обнаружение HTTP/3 на первом запросе. `lookup_svcb` честно пуст.
#![deny(unsafe_code)]

use futures_core::Stream;
use http_ng_core::{Error, ErrorKind};
use http_ng_dns::{ResolvedAddr, Resolve};
use http_ng_rt::Blocking;
use std::net::{IpAddr, ToSocketAddrs};

#[derive(Debug, Clone)]
pub struct SystemDns<B> {
    blocking: B,
}

impl<B> SystemDns<B> {
    pub fn new(blocking: B) -> Self { Self { blocking } }
}

#[derive(Debug)]
struct ResolveFailed(String, std::io::Error);
impl std::fmt::Display for ResolveFailed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "failed to resolve `{}`: {}", self.0, self.1)
    }
}
impl std::error::Error for ResolveFailed {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> { Some(&self.1) }
}

impl<B: Blocking> SystemDns<B> {
    fn lookup(&self, name: &str, want_v6: bool)
        -> impl Stream<Item = Result<ResolvedAddr, Error>>
    {
        let owned = name.to_owned();
        let fut = self.blocking.run(move || {
            (owned.as_str(), 0u16).to_socket_addrs()
                .map(|it| it.map(|s| s.ip()).collect::<Vec<IpAddr>>())
                .map_err(|e| ResolveFailed(owned.clone(), e))
        });
        futures_util::stream::once(fut).flat_map(move |res| match res {
            Err(e) => futures_util::stream::iter(vec![
                Err(Error::new(ErrorKind::Resolve, e))
            ]),
            Ok(addrs) => futures_util::stream::iter(
                addrs.into_iter()
                    .filter(|a| a.is_ipv6() == want_v6)
                    .map(|addr| Ok(ResolvedAddr { addr, ttl: None }))
                    .collect::<Vec<_>>()
            ),
        })
    }
}

impl<B: Blocking> Resolve for SystemDns<B> {
    fn lookup_ipv4(&self, name: &str) -> impl Stream<Item = Result<ResolvedAddr, Error>> {
        self.lookup(name, false)
    }
    fn lookup_ipv6(&self, name: &str) -> impl Stream<Item = Result<ResolvedAddr, Error>> {
        self.lookup(name, true)
    }
}
```

> **Известное ограничение, задокументировать в rustdoc:** здесь один вызов
> `getaddrinfo` на оба семейства, а curl 8.20 делает **два**, в разных потоках,
> чтобы частичные результаты запускали Happy Eyeballs раньше. Разделение на два
> слота — задача v0.2; сейчас важнее, чтобы форма трейта это допускала, а она
> допускает.

- [ ] **Step 4: Обновить бонды `Blocking` в `http-ng-rt`**

`blocking::unblock` и `tokio::spawn_blocking` требуют `Send + 'static`.
Привести трейт к:

```rust
pub trait Blocking {
    fn run<T: Send + 'static, F: FnOnce() -> T + Send + 'static>(&self, f: F)
        -> impl Future<Output = T>;
}
```

Это единственное место во всей вертикали, где `Send` объявлен нами, и он честен:
способности `Blocking` на wasm нет вовсе, поэтому заражать ей нечего.

- [ ] **Step 5: Запустить тесты**

Run: `cargo test -p http-ng-dns-system && cargo test -p http-ng-rt-tokio && cargo test -p http-ng-rt-smol`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/http-ng-dns-system crates/http-ng-rt crates/http-ng-rt-tokio crates/http-ng-rt-smol
git commit -m "feat(dns-system): getaddrinfo resolver over the Blocking capability"
```

---

### Task 8: `http-ng-tls` — трейт TLS

**Files:**
- Create: `crates/http-ng-tls/Cargo.toml`, `src/lib.rs`
- Test: внутри `lib.rs`

**Interfaces:**
- Produces:
  - `pub struct TlsRequest<'a> { pub server_name: &'a str, pub alpn: &'a [&'a [u8]], pub ech: Option<&'a [u8]> }`
  - `pub struct TlsInfo { pub alpn: Option<Vec<u8>>, pub peer_certificates: Option<Vec<Vec<u8>>>, pub protocol_version: Option<String>, pub cipher_suite: Option<String> }` — **все поля `Option`**
  - `pub trait TlsConnect { type Stream<S>: hyper::rt::Read + hyper::rt::Write + Unpin where S: hyper::rt::Read + hyper::rt::Write + Unpin; fn connect<S>(&self, io: S, req: TlsRequest<'_>) -> impl Future<Output = Result<(Self::Stream<S>, TlsInfo), Error>> where S: hyper::rt::Read + hyper::rt::Write + Unpin; }`

- [ ] **Step 1: Написать падающий тест**

```rust
// crates/http-ng-tls/src/lib.rs
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tls_info_is_all_optional() {
        // native-tls отдаёт только leaf-сертификат, ALPN и
        // tls-server-end-point; трейт обязан это допускать.
        let i = TlsInfo::default();
        assert!(i.alpn.is_none());
        assert!(i.peer_certificates.is_none());
        assert!(i.protocol_version.is_none());
        assert!(i.cipher_suite.is_none());
    }

    #[test]
    fn alpn_lives_on_the_request_not_the_config() {
        // Пин версии и h2-prior-knowledge требуют разного набора ALPN для
        // разных соединений к одному origin.
        let req = TlsRequest { server_name: "example.com", alpn: &[b"http/1.1"], ech: None };
        assert_eq!(req.alpn, &[b"http/1.1".as_slice()]);
    }

    #[test]
    fn ech_slot_exists_before_it_is_implemented() {
        // ECH — RFC 9849; EchConfigList приходит из HTTPS/SVCB. Не заложи мы
        // поле сразу, добавление стало бы ломающим изменением.
        let req = TlsRequest { server_name: "e.com", alpn: &[], ech: Some(&[1, 2, 3]) };
        assert_eq!(req.ech, Some(&[1u8, 2, 3][..]));
    }
}
```

- [ ] **Step 2: Запустить и убедиться, что падает**

Run: `cargo test -p http-ng-tls`
Expected: FAIL — крейта нет.

- [ ] **Step 3: Создать и реализовать**

```toml
# crates/http-ng-tls/Cargo.toml
[package]
name = "http-ng-tls"
version = "0.1.0"
description = "Трейт подключаемого TLS для http-ng"
edition.workspace = true
rust-version.workspace = true
license.workspace = true
repository.workspace = true

[dependencies]
http-ng-core = { workspace = true }
hyper        = { version = "1.11", default-features = false }

[lints]
workspace = true
```

```rust
// crates/http-ng-tls/src/lib.rs
//! Подключаемый TLS.
//!
//! Трейт типизирован на `hyper::rt::Read/Write`, а **не** на futures-io или
//! tokio-io. Следствие: per-runtime TLS-склейки не существует вообще — один
//! адаптер обслуживает все рантаймы.
#![deny(unsafe_code)]

use http_ng_core::Error;
use std::future::Future;

/// ALPN живёт на **коннекте**, а не на конфиге: пин версии и
/// h2-prior-knowledge требуют разных наборов для разных соединений к одному
/// origin. Реализация кэширует конфиг по набору ALPN у себя.
#[derive(Debug, Clone, Copy)]
pub struct TlsRequest<'a> {
    pub server_name: &'a str,
    pub alpn: &'a [&'a [u8]],
    /// RFC 9849 Encrypted Client Hello. Берётся из HTTPS/SVCB-записи.
    /// Слот заложен сразу: добавить его позже — ломающее изменение.
    pub ech: Option<&'a [u8]>,
}

/// Все поля `Option`, потому что native-tls отдаёт только leaf-сертификат,
/// ALPN и tls-server-end-point.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TlsInfo {
    pub alpn: Option<Vec<u8>>,
    pub peer_certificates: Option<Vec<Vec<u8>>>,
    pub protocol_version: Option<String>,
    pub cipher_suite: Option<String>,
}

pub trait TlsConnect {
    type Stream<S>: hyper::rt::Read + hyper::rt::Write + Unpin
    where
        S: hyper::rt::Read + hyper::rt::Write + Unpin;

    fn connect<S>(
        &self,
        io: S,
        req: TlsRequest<'_>,
    ) -> impl Future<Output = Result<(Self::Stream<S>, TlsInfo), Error>>
    where
        S: hyper::rt::Read + hyper::rt::Write + Unpin;
}
```

- [ ] **Step 4: Запустить тесты**

Run: `cargo test -p http-ng-tls`
Expected: PASS, три теста.

- [ ] **Step 5: Commit**

```bash
git add crates/http-ng-tls
git commit -m "feat(tls): TlsConnect trait typed on hyper::rt, ALPN per connect, ECH slot"
```

---

### Task 9: `http-ng-tls-rustls` — поток

Самая большая единица вертикали. Адаптер строится на поверхности rustls,
стабильной с 0.20 (`process_new_packets`, `wants_read`/`wants_write`,
`read_tls`/`write_tls`), **не** на `unbuffered` — тот удалён в main rustls
(PR #2905, 2026-02-06).

**Files:**
- Create: `crates/http-ng-tls-rustls/Cargo.toml`, `src/lib.rs`, `src/stream.rs`
- Test: `crates/http-ng-tls-rustls/tests/handshake.rs`

**Interfaces:**
- Consumes: `TlsConnect`, `TlsRequest`, `TlsInfo` (Task 8).
- Produces:
  - `pub struct Rustls { .. }`; `Rustls::with_platform_verifier() -> Result<Self, Error>`;
    `Rustls::with_webpki_roots() -> Self`; `Rustls::from_config(Arc<rustls::ClientConfig>) -> Self`
  - `pub struct TlsStream<S>`; `impl<S> hyper::rt::Read + hyper::rt::Write for TlsStream<S>`
  - `impl TlsConnect for Rustls { type Stream<S> = TlsStream<S>; }`

- [ ] **Step 1: Написать падающий интеграционный тест**

```rust
// crates/http-ng-tls-rustls/tests/handshake.rs
//! Тест поднимает настоящий TLS-сервер на rustls и проверяет, что наш адаптер
//! доводит хендшейк до конца и прокачивает байты в обе стороны.

use http_ng_rt::{TcpConnect, TcpOpts};
use http_ng_rt_tokio::Tokio;
use http_ng_tls::{TlsConnect, TlsRequest};
use http_ng_tls_rustls::Rustls;

mod server;  // см. Step 3: минимальный TLS-эхо-сервер на самоподписанном серте

#[tokio::test]
async fn completes_handshake_and_echoes() {
    let (addr, ca_der) = server::spawn_tls_echo();

    let mut roots = rustls::RootCertStore::empty();
    roots.add(ca_der.into()).unwrap();
    let cfg = rustls::ClientConfig::builder()
        .with_root_certificates(roots)
        .with_no_client_auth();

    let tls = Rustls::from_config(std::sync::Arc::new(cfg));
    let tcp = Tokio.connect(addr, &TcpOpts::default()).await.unwrap();
    let (mut stream, info) = tls.connect(tcp, TlsRequest {
        server_name: "localhost", alpn: &[b"http/1.1"], ech: None,
    }).await.expect("handshake");

    assert_eq!(info.alpn.as_deref(), Some(b"http/1.1".as_slice()),
               "согласованный ALPN должен быть виден");

    // Прокачка байтов через hyper::rt-интерфейс.
    let sent = b"ping";
    let n = std::future::poll_fn(|cx|
        hyper::rt::Write::poll_write(std::pin::Pin::new(&mut stream), cx, sent)
    ).await.unwrap();
    assert_eq!(n, 4);

    let mut store = [0u8; 16];
    let mut rb = hyper::rt::ReadBuf::new(&mut store);
    std::future::poll_fn(|cx|
        hyper::rt::Read::poll_read(std::pin::Pin::new(&mut stream), cx, rb.unfilled())
    ).await.unwrap();
    assert_eq!(rb.filled(), b"ping");
}

#[tokio::test]
async fn rejects_an_untrusted_certificate() {
    let (addr, _ca) = server::spawn_tls_echo();
    let tls = Rustls::with_webpki_roots(); // публичные корни — наш серт им неизвестен
    let tcp = Tokio.connect(addr, &TcpOpts::default()).await.unwrap();
    let err = tls.connect(tcp, TlsRequest {
        server_name: "localhost", alpn: &[], ech: None,
    }).await.err().expect("must fail");
    assert!(matches!(err.kind(), http_ng_core::ErrorKind::Tls), "{err}");
}
```

- [ ] **Step 2: Запустить и убедиться, что падает**

Run: `cargo test -p http-ng-tls-rustls`
Expected: FAIL — крейта нет.

- [ ] **Step 3: Создать крейт и тестовый сервер**

```toml
# crates/http-ng-tls-rustls/Cargo.toml
[package]
name = "http-ng-tls-rustls"
version = "0.1.0"
description = "TLS-бэкенд http-ng на rustls, адаптер написан против hyper::rt"
edition.workspace = true
rust-version.workspace = true
license.workspace = true
repository.workspace = true

[features]
default = []
platform-verifier = ["dep:rustls-platform-verifier"]
webpki-roots      = ["dep:webpki-roots"]

[dependencies]
bytes            = { workspace = true }
http-ng-core     = { workspace = true }
http-ng-tls      = { path = "../http-ng-tls", version = "0.1.0" }
hyper            = { version = "1.11", default-features = false }
rustls           = { version = "0.23", default-features = false, features = ["std", "ring", "tls12"] }
rustls-pki-types = "1.15"
rustls-platform-verifier = { version = "0.7", optional = true }
webpki-roots     = { version = "1.0", optional = true }

[dev-dependencies]
http-ng-rt       = { path = "../http-ng-rt" }
http-ng-rt-tokio = { path = "../http-ng-rt-tokio" }
rcgen            = "0.14"
tokio            = { version = "1", features = ["macros", "rt-multi-thread", "net", "io-util"] }
tokio-rustls     = { version = "0.26", default-features = false, features = ["ring"] }

[lints]
workspace = true
```

```rust
// crates/http-ng-tls-rustls/tests/server.rs
//! Минимальный TLS-эхо-сервер на самоподписанном сертификате.
//! Живёт в dev-dependencies и в публичный граф не попадает.

use std::net::SocketAddr;
use std::sync::Arc;

pub fn spawn_tls_echo() -> (SocketAddr, Vec<u8>) {
    let cert = rcgen::generate_simple_self_signed(vec!["localhost".into()]).unwrap();
    let cert_der = cert.cert.der().to_vec();
    let key_der = cert.signing_key.serialize_der();

    let cfg = rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(
            vec![cert_der.clone().into()],
            rustls_pki_types::PrivateKeyDer::Pkcs8(key_der.into()),
        )
        .unwrap();
    let mut cfg = cfg;
    cfg.alpn_protocols = vec![b"http/1.1".to_vec()];
    let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(cfg));

    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    listener.set_nonblocking(true).unwrap();

    std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all().build().unwrap();
        rt.block_on(async move {
            let listener = tokio::net::TcpListener::from_std(listener).unwrap();
            loop {
                let Ok((tcp, _)) = listener.accept().await else { continue };
                let acceptor = acceptor.clone();
                tokio::spawn(async move {
                    let Ok(mut tls) = acceptor.accept(tcp).await else { return };
                    let mut buf = [0u8; 1024];
                    use tokio::io::{AsyncReadExt, AsyncWriteExt};
                    while let Ok(n) = tls.read(&mut buf).await {
                        if n == 0 { break }
                        if tls.write_all(&buf[..n]).await.is_err() { break }
                    }
                });
            }
        });
    });

    (addr, cert_der)
}
```

- [ ] **Step 4: Реализовать поток**

```rust
// crates/http-ng-tls-rustls/src/stream.rs
use http_ng_core::{Error, ErrorKind};
use hyper::rt::{Read, ReadBufCursor, Write};
use std::io::{Read as _, Write as _};
use std::pin::Pin;
use std::task::{Context, Poll, ready};

const SCRATCH: usize = 16 * 1024;

/// TLS поверх любого `hyper::rt`-транспорта.
///
/// Построен на поверхности rustls, стабильной с 0.20: `read_tls` /
/// `process_new_packets` / `wants_write` / `write_tls`. **Не** на `unbuffered`
/// — тот удалён в main rustls (PR #2905, 2026-02-06), и адаптер на нём пришлось
/// бы переписывать целиком под 0.24.
#[derive(Debug)]
pub struct TlsStream<S> {
    io: S,
    conn: rustls::ClientConnection,
    /// Байты, вычитанные из сокета, но ещё не скормленные rustls.
    read_pending: bool,
}

impl<S> TlsStream<S> {
    pub(crate) fn new(io: S, conn: rustls::ClientConnection) -> Self {
        Self { io, conn, read_pending: false }
    }
    pub(crate) fn conn(&self) -> &rustls::ClientConnection { &self.conn }
    pub(crate) fn parts_mut(&mut self) -> (&mut S, &mut rustls::ClientConnection) {
        (&mut self.io, &mut self.conn)
    }
}

fn tls_err<E: std::error::Error + 'static>(e: E) -> std::io::Error {
    std::io::Error::other(format!("tls: {e}"))
}

/// Прокачать всё, что rustls хочет записать, в нижележащий транспорт.
pub(crate) fn flush_outgoing<S: Write + Unpin>(
    io: &mut S,
    conn: &mut rustls::ClientConnection,
    cx: &mut Context<'_>,
) -> Poll<std::io::Result<()>> {
    while conn.wants_write() {
        let mut buf = Vec::new();
        conn.write_tls(&mut buf).map_err(tls_err)?;
        let mut written = 0;
        while written < buf.len() {
            let n = ready!(Pin::new(&mut *io).poll_write(cx, &buf[written..]))?;
            if n == 0 {
                return Poll::Ready(Err(std::io::ErrorKind::WriteZero.into()));
            }
            written += n;
        }
    }
    Pin::new(io).poll_flush(cx)
}

/// Вычитать из транспорта и скормить rustls. `Ok(false)` — EOF.
pub(crate) fn pump_incoming<S: Read + Unpin>(
    io: &mut S,
    conn: &mut rustls::ClientConnection,
    cx: &mut Context<'_>,
) -> Poll<std::io::Result<bool>> {
    let mut scratch = [0u8; SCRATCH];
    let mut rb = hyper::rt::ReadBuf::new(&mut scratch);
    ready!(Pin::new(io).poll_read(cx, rb.unfilled()))?;
    let filled = rb.filled();
    if filled.is_empty() {
        return Poll::Ready(Ok(false));
    }
    let mut cursor = std::io::Cursor::new(filled);
    while (cursor.position() as usize) < filled.len() {
        conn.read_tls(&mut cursor).map_err(tls_err)?;
        conn.process_new_packets().map_err(tls_err)?;
    }
    Poll::Ready(Ok(true))
}

impl<S: Read + Write + Unpin> Read for TlsStream<S> {
    fn poll_read(mut self: Pin<&mut Self>, cx: &mut Context<'_>, mut buf: ReadBufCursor<'_>)
        -> Poll<std::io::Result<()>>
    {
        let this = &mut *self;
        loop {
            // 1. Отдать уже расшифрованное.
            let mut scratch = [0u8; SCRATCH];
            let want = buf.remaining().min(SCRATCH);
            if want == 0 { return Poll::Ready(Ok(())) }
            match this.conn.reader().read(&mut scratch[..want]) {
                Ok(0) => {}
                Ok(n) => { buf.put_slice(&scratch[..n]); return Poll::Ready(Ok(())) }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
                Err(e) => return Poll::Ready(Err(e)),
            }
            // 2. Отдать всё исходящее (renegotiation, close_notify и т.п.).
            ready!(flush_outgoing(&mut this.io, &mut this.conn, cx))?;
            // 3. Дочитать из транспорта.
            let more = ready!(pump_incoming(&mut this.io, &mut this.conn, cx))?;
            if !more { return Poll::Ready(Ok(())) } // EOF
            this.read_pending = true;
        }
    }
}

impl<S: Read + Write + Unpin> Write for TlsStream<S> {
    fn poll_write(mut self: Pin<&mut Self>, cx: &mut Context<'_>, data: &[u8])
        -> Poll<std::io::Result<usize>>
    {
        let this = &mut *self;
        let n = this.conn.writer().write(data)?;
        ready!(flush_outgoing(&mut this.io, &mut this.conn, cx))?;
        Poll::Ready(Ok(n))
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>)
        -> Poll<std::io::Result<()>>
    {
        let this = &mut *self;
        this.conn.writer().flush()?;
        flush_outgoing(&mut this.io, &mut this.conn, cx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>)
        -> Poll<std::io::Result<()>>
    {
        let this = &mut *self;
        this.conn.send_close_notify();
        ready!(flush_outgoing(&mut this.io, &mut this.conn, cx))?;
        Pin::new(&mut this.io).poll_shutdown(cx)
    }
}
```

- [ ] **Step 5: Реализовать `Rustls` и хендшейк**

```rust
// crates/http-ng-tls-rustls/src/lib.rs
//! TLS-бэкенд на rustls.
//!
//! **rustls не появляется в публичном API `http-ng`** — иначе выход 0.24 стал
//! бы нашим ломающим релизом. В 0.24 ожидаются: удалённая фича `std`,
//! провайдеры вынесены в `rustls-ring`/`rustls-aws-lc-rs`, MSRV 1.85,
//! edition 2024. Один переписанный крейт заложен в бюджет.
#![deny(unsafe_code)]

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
        Self { base: cfg, by_alpn: Mutex::new(HashMap::new()) }
    }

    #[cfg(feature = "webpki-roots")]
    pub fn with_webpki_roots() -> Self {
        let roots: rustls::RootCertStore = webpki_roots::TLS_SERVER_ROOTS.iter()
            .cloned().collect();
        Self::from_config(Arc::new(
            rustls::ClientConfig::builder()
                .with_root_certificates(roots)
                .with_no_client_auth()
        ))
    }

    #[cfg(feature = "platform-verifier")]
    pub fn with_platform_verifier() -> Result<Self, Error> {
        let cfg = rustls_platform_verifier::tls_config();
        Ok(Self::from_config(Arc::new(cfg)))
    }

    fn config_for(&self, alpn: &[&[u8]]) -> Arc<rustls::ClientConfig> {
        if alpn.is_empty() {
            return self.base.clone();
        }
        let key: Vec<Vec<u8>> = alpn.iter().map(|a| a.to_vec()).collect();
        let mut cache = self.by_alpn.lock().expect("alpn cache poisoned");
        cache.entry(key.clone()).or_insert_with(|| {
            let mut cfg = (*self.base).clone();
            cfg.alpn_protocols = key;
            Arc::new(cfg)
        }).clone()
    }
}

impl TlsConnect for Rustls {
    type Stream<S> = TlsStream<S>
    where S: hyper::rt::Read + hyper::rt::Write + Unpin;

    async fn connect<S>(&self, io: S, req: TlsRequest<'_>)
        -> Result<(TlsStream<S>, TlsInfo), Error>
    where S: hyper::rt::Read + hyper::rt::Write + Unpin
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
        }).await?;

        let c = stream.conn();
        let info = TlsInfo {
            alpn: c.alpn_protocol().map(|a| a.to_vec()),
            peer_certificates: c.peer_certificates()
                .map(|cs| cs.iter().map(|d| d.as_ref().to_vec()).collect()),
            protocol_version: c.protocol_version().map(|v| format!("{v:?}")),
            cipher_suite: c.negotiated_cipher_suite().map(|s| format!("{:?}", s.suite())),
        };
        Ok((stream, info))
    }
}
```

Сделать `stream::{flush_outgoing, pump_incoming}` `pub(crate)` и добавить
`ErrorKind::Tls` в маппинг ошибок хендшейка.

- [ ] **Step 6: Запустить тесты**

Run: `cargo test -p http-ng-tls-rustls --features webpki-roots`
Expected: PASS, два теста.

- [ ] **Step 7: Проверить, что rustls не течёт в публичный API `http-ng`**

Run: `! grep -rn "rustls" crates/http-ng/src crates/http-ng-core/src && echo OK`
Expected: `OK`.

- [ ] **Step 8: Commit**

```bash
git add crates/http-ng-tls-rustls
git commit -m "feat(tls-rustls): TLS stream over hyper::rt on the 0.20-stable rustls surface"
```

---

### Task 10: `http-ng-native` — тело запроса с Send-ошибкой

`hyper::client::conn::http1::handshake<T, B>` требует
`B::Error: Into<Box<dyn StdError + Send + Sync>>` и `B::Data: Send`. Наш
`http_ng_core::Error` держит `Arc<dyn Error + 'static>` и **не** `Send + Sync`.
Требование запирается здесь и в ядро не течёт.

**Files:**
- Create: `crates/http-ng-native/Cargo.toml`, `src/lib.rs`, `src/body.rs`
- Test: внутри `body.rs`

**Interfaces:**
- Consumes: `http_ng_core::RequestBody`.
- Produces: `pub(crate) struct OutgoingBody`; `OutgoingBody::from_request_body(RequestBody) -> Self`;
  `impl http_body::Body for OutgoingBody { type Data = Bytes; type Error = BoxError; }`
  где `pub(crate) type BoxError = Box<dyn std::error::Error + Send + Sync>`.

- [ ] **Step 1: Написать падающие тесты**

```rust
// crates/http-ng-native/src/body.rs
#[cfg(test)]
mod tests {
    use super::*;
    use http_body_util::BodyExt;
    use http_ng_core::RequestBody;

    #[test]
    fn error_type_satisfies_hypers_send_sync_bound() {
        fn assert_bound<B: http_body::Body>()
        where B::Error: Into<Box<dyn std::error::Error + Send + Sync>>, B::Data: bytes::Buf + Send {}
        assert_bound::<OutgoingBody>();
    }

    #[test]
    fn full_body_yields_its_bytes_once() {
        let b = OutgoingBody::from_request_body(
            RequestBody::Full(bytes::Bytes::from_static(b"payload")));
        let collected = futures_executor::block_on(b.collect()).unwrap().to_bytes();
        assert_eq!(&collected[..], b"payload");
    }

    #[test]
    fn empty_body_is_end_stream_immediately() {
        let b = OutgoingBody::from_request_body(RequestBody::Empty);
        assert!(http_body::Body::is_end_stream(&b));
    }

    #[test]
    fn size_hint_is_exact_for_buffered_bodies() {
        let b = OutgoingBody::from_request_body(
            RequestBody::Full(bytes::Bytes::from_static(b"1234")));
        assert_eq!(http_body::Body::size_hint(&b).exact(), Some(4));
    }
}
```

- [ ] **Step 2: Запустить и убедиться, что падает**

Run: `cargo test -p http-ng-native`
Expected: FAIL — крейта нет.

- [ ] **Step 3: Создать крейт и реализовать тело**

```toml
# crates/http-ng-native/Cargo.toml
[package]
name = "http-ng-native"
version = "0.1.0"
description = "Native-транспорт http-ng: TCP + TLS + HTTP/1.1 на hyper"
edition.workspace = true
rust-version.workspace = true
license.workspace = true
repository.workspace = true

[dependencies]
bytes          = { workspace = true }
futures-util   = { version = "0.3", default-features = false, features = ["std"] }
http           = { workspace = true }
http-body      = { workspace = true }
http-body-util = { workspace = true }
http-ng-core   = { workspace = true }
http-ng-dns    = { path = "../http-ng-dns",   version = "0.1.0" }
http-ng-proto  = { workspace = true }
http-ng-rt     = { path = "../http-ng-rt",    version = "0.1.0" }
http-ng-tls    = { path = "../http-ng-tls",   version = "0.1.0" }
hyper          = { version = "1.11", default-features = false, features = ["client", "http1"] }

[dev-dependencies]
futures-executor = { version = "0.3", default-features = false, features = ["std"] }

[lints]
workspace = true
```

```rust
// crates/http-ng-native/src/body.rs
use bytes::Bytes;
use http_body::{Body, Frame, SizeHint};
use http_ng_core::RequestBody;
use std::pin::Pin;
use std::task::{Context, Poll};

/// hyper требует `B::Error: Into<Box<dyn StdError + Send + Sync>>`, а наш
/// `http_ng_core::Error` держит `Arc<dyn Error + 'static>` без `Send`.
/// Требование `Send` запирается здесь и в ядро не течёт.
pub(crate) type BoxError = Box<dyn std::error::Error + Send + Sync>;

#[derive(Debug)]
pub(crate) struct OutgoingBody {
    inner: Option<Bytes>,
}

impl OutgoingBody {
    pub(crate) fn from_request_body(body: RequestBody) -> Self {
        // В v0.1 native-транспорт отправляет только буферизованные тела.
        // Стриминговые приедут вместе с пулом и retry — форма `RequestBody`
        // это уже допускает.
        let inner = match body {
            RequestBody::Empty => None,
            RequestBody::Full(b) if b.is_empty() => None,
            RequestBody::Full(b) => Some(b),
            RequestBody::Rewindable(f) => match f() {
                RequestBody::Full(b) if !b.is_empty() => Some(b),
                _ => None,
            },
            RequestBody::Streaming(_) => None,
        };
        Self { inner }
    }
}

impl Body for OutgoingBody {
    type Data = Bytes;
    type Error = BoxError;

    fn poll_frame(mut self: Pin<&mut Self>, _cx: &mut Context<'_>)
        -> Poll<Option<Result<Frame<Bytes>, BoxError>>>
    {
        Poll::Ready(self.inner.take().map(|b| Ok(Frame::data(b))))
    }

    fn is_end_stream(&self) -> bool { self.inner.is_none() }

    fn size_hint(&self) -> SizeHint {
        match &self.inner {
            Some(b) => SizeHint::with_exact(b.len() as u64),
            None => SizeHint::with_exact(0),
        }
    }
}
```

- [ ] **Step 4: Запустить тесты**

Run: `cargo test -p http-ng-native`
Expected: PASS, четыре теста.

- [ ] **Step 5: Commit**

```bash
git add crates/http-ng-native
git commit -m "feat(native): outgoing body confining hyper's Send bound to this crate"
```

---

### Task 11: `http-ng-native` — коннектор

**Files:**
- Create: `crates/http-ng-native/src/connect.rs`
- Modify: `crates/http-ng-native/src/lib.rs`
- Test: `crates/http-ng-native/tests/connect.rs`

**Interfaces:**
- Consumes: `Scheduler`/`HeAction` (Task 5), `Resolve` (Task 6), `TcpConnect`,
  `Timer` (Task 1), `TlsConnect` (Task 8).
- Produces:
  - `pub(crate) enum Conn<P, T> { Plain(P), Tls(T) }` — реализует `hyper::rt::Read + Write`
  - `pub(crate) async fn connect<R, D, L>(rt: &R, dns: &D, tls: &L, uri: &http::Uri, opts: &TcpOpts, alpn: &[&[u8]]) -> Result<(Conn<R::Stream, L::Stream<R::Stream>>, Option<TlsInfo>), Error>`

- [ ] **Step 1: Написать падающий тест**

```rust
// crates/http-ng-native/tests/connect.rs
//! Проверяем, что коннектор действительно гоняет Happy Eyeballs: сначала
//! пробуется мёртвый адрес, затем живой, и соединение получается.

use http_ng_native::testing::connect_for_test;
use http_ng_rt_tokio::Tokio;

#[tokio::test]
async fn falls_over_from_a_dead_address_to_a_live_one() {
    let live = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = live.local_addr().unwrap();
    std::thread::spawn(move || { let _ = live.accept(); });

    // 198.51.100.1 — TEST-NET-2, гарантированно не отвечает.
    let dead: std::net::IpAddr = "198.51.100.1".parse().unwrap();
    let conn = connect_for_test(&Tokio, &[dead, addr.ip()], addr.port()).await;
    assert!(conn.is_ok(), "должны дойти до живого адреса");
}

#[tokio::test]
async fn reports_connect_kind_when_everything_is_dead() {
    let dead: std::net::IpAddr = "198.51.100.1".parse().unwrap();
    let err = connect_for_test(&Tokio, &[dead], 81).await.err().expect("must fail");
    assert!(matches!(err.kind(), http_ng_core::ErrorKind::Connect), "{err}");
}
```

- [ ] **Step 2: Запустить и убедиться, что падает**

Run: `cargo test -p http-ng-native --test connect`
Expected: FAIL — `connect_for_test` не найден.

- [ ] **Step 3: Реализовать коннектор**

```rust
// crates/http-ng-native/src/connect.rs
use futures_util::stream::{FuturesUnordered, StreamExt};
use http_ng_core::{Error, ErrorKind};
use http_ng_proto::happy_eyeballs::{HeAction, HeConfig, Scheduler};
use http_ng_rt::{TcpConnect, TcpOpts, Timer};
use hyper::rt::{Read, ReadBufCursor, Write};
use std::net::{IpAddr, SocketAddr};
use std::pin::Pin;
use std::task::{Context, Poll};

/// Соединение: с TLS или без. Оба варианта — `hyper::rt` IO.
#[derive(Debug)]
pub enum Conn<P, T> { Plain(P), Tls(T) }

impl<P: Read + Unpin, T: Read + Unpin> Read for Conn<P, T> {
    fn poll_read(self: Pin<&mut Self>, cx: &mut Context<'_>, buf: ReadBufCursor<'_>)
        -> Poll<std::io::Result<()>>
    {
        match self.get_mut() {
            Conn::Plain(p) => Pin::new(p).poll_read(cx, buf),
            Conn::Tls(t) => Pin::new(t).poll_read(cx, buf),
        }
    }
}

impl<P: Write + Unpin, T: Write + Unpin> Write for Conn<P, T> {
    fn poll_write(self: Pin<&mut Self>, cx: &mut Context<'_>, b: &[u8])
        -> Poll<std::io::Result<usize>>
    {
        match self.get_mut() {
            Conn::Plain(p) => Pin::new(p).poll_write(cx, b),
            Conn::Tls(t) => Pin::new(t).poll_write(cx, b),
        }
    }
    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        match self.get_mut() {
            Conn::Plain(p) => Pin::new(p).poll_flush(cx),
            Conn::Tls(t) => Pin::new(t).poll_flush(cx),
        }
    }
    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        match self.get_mut() {
            Conn::Plain(p) => Pin::new(p).poll_shutdown(cx),
            Conn::Tls(t) => Pin::new(t).poll_shutdown(cx),
        }
    }
}

#[derive(Debug)] pub(crate) struct AllAttemptsFailed(pub(crate) usize);
impl std::fmt::Display for AllAttemptsFailed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "all {} connection attempts failed", self.0)
    }
}
impl std::error::Error for AllAttemptsFailed {}

/// Happy Eyeballs по RFC 8305 **без `spawn`**: попытки живут в
/// `FuturesUnordered`, а не в задачах, потому что `spawn` потребовал бы
/// `Send + 'static` и закрыл бы однопоточные рантаймы.
pub(crate) async fn race_connect<R>(
    rt: &R,
    addrs_v6: Vec<IpAddr>,
    addrs_v4: Vec<IpAddr>,
    port: u16,
    opts: &TcpOpts,
) -> Result<R::Stream, Error>
where R: TcpConnect + Timer
{
    let mut sched = Scheduler::new(HeConfig::default());
    sched.offer_v6(&addrs_v6);
    sched.offer_v4(&addrs_v4);
    sched.mark_v6_done();
    sched.mark_v4_done();

    let start = rt.now();
    let mut attempts = FuturesUnordered::new();
    let mut launched = 0usize;

    loop {
        let elapsed = rt.elapsed_since(start);
        match sched.poll(elapsed) {
            HeAction::Start(ip) => {
                launched += 1;
                attempts.push(rt.connect(SocketAddr::new(ip, port), opts));
            }
            HeAction::Wait(d) => {
                if attempts.is_empty() {
                    rt.sleep(d).await;
                    continue;
                }
                futures_util::select_biased! {
                    res = attempts.next() => {
                        if let Some(Ok(s)) = res { return Ok(s) }
                    }
                    _ = futures_util::FutureExt::fuse(rt.sleep(d)) => {}
                }
            }
            HeAction::Exhausted => {
                while let Some(res) = attempts.next().await {
                    if let Ok(s) = res { return Ok(s) }
                }
                return Err(Error::new(ErrorKind::Connect, AllAttemptsFailed(launched)));
            }
        }
    }
}
```

Для `select_biased!` добавить `futures-util` с фичей `async-await-macro`.

- [ ] **Step 4: Экспортировать тестовый хелпер**

```rust
// в crates/http-ng-native/src/lib.rs
#[doc(hidden)]
pub mod testing {
    use super::*;
    /// Только для интеграционных тестов: гонит Happy Eyeballs по готовому
    /// списку адресов, минуя DNS.
    pub async fn connect_for_test<R>(rt: &R, addrs: &[std::net::IpAddr], port: u16)
        -> Result<R::Stream, http_ng_core::Error>
    where R: http_ng_rt::TcpConnect + http_ng_rt::Timer
    {
        let (v6, v4): (Vec<_>, Vec<_>) = addrs.iter().copied().partition(|a| a.is_ipv6());
        crate::connect::race_connect(rt, v6, v4, port, &http_ng_rt::TcpOpts::default()).await
    }
}
```

- [ ] **Step 5: Запустить тесты**

Run: `cargo test -p http-ng-native --test connect`
Expected: PASS, два теста. Второй занимает ~несколько секунд (ожидание
таймаута на TEST-NET-2) — это нормально.

- [ ] **Step 6: Commit**

```bash
git add crates/http-ng-native
git commit -m "feat(native): RFC 8305 connector racing attempts without spawn"
```

---

### Task 12: `http-ng-native` — HTTP/1 с инлайн-драйвом соединения

Технический центр вертикали. h1-handshake не требует ни executor'а, ни таймера,
а `Connection` поллится **рядом** с ответом — значит клиент едет на голом
`futures`-executor'е с нулевой способностью спавнить. Это и доказывает, что
рантайм-шов настоящий.

**Files:**
- Create: `crates/http-ng-native/src/h1.rs`
- Modify: `crates/http-ng-native/src/lib.rs`
- Test: `crates/http-ng-native/tests/h1.rs`

**Interfaces:**
- Consumes: `OutgoingBody` (Task 10), `Conn` (Task 11).
- Produces:
  - `pub struct NativeBody` — тело ответа, которое **само драйвит соединение**;
    `impl http_body::Body for NativeBody { type Data = Bytes; type Error = Error }`
  - `pub(crate) async fn exchange<I>(io: I, req: http::Request<OutgoingBody>) -> Result<http::Response<NativeBody>, Error>`

- [ ] **Step 1: Написать падающий тест**

```rust
// crates/http-ng-native/tests/h1.rs
//! Сервер — голый `std::net::TcpListener`, говорящий HTTP/1.1 руками.
//! Никаких серверных фреймворков: тест проверяет наш клиент, а не чужой сервер.

use std::io::{Read, Write};

fn spawn_h1_server(response: &'static str) -> std::net::SocketAddr {
    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = l.local_addr().unwrap();
    std::thread::spawn(move || {
        for stream in l.incoming() {
            let Ok(mut s) = stream else { continue };
            let mut buf = [0u8; 2048];
            let _ = s.read(&mut buf);
            let _ = s.write_all(response.as_bytes());
            let _ = s.flush();
        }
    });
    addr
}

#[test]
fn works_on_a_bare_futures_executor_with_no_spawn() {
    // Ключевой тест вертикали: ни tokio, ни smol — только futures::block_on.
    let addr = spawn_h1_server(
        "HTTP/1.1 200 OK\r\nContent-Length: 5\r\n\r\nhello");

    futures_executor::block_on(async move {
        let std_tcp = std::net::TcpStream::connect(addr).unwrap();
        std_tcp.set_nonblocking(false).unwrap();
        // Блокирующий сокет в блокирующем executor'е — допустимо для теста:
        // проверяется, что hyper не требует ни spawn, ни таймера.
        let io = http_ng_native::testing::blocking_io(std_tcp);
        let req = http::Request::builder().uri("/").body(
            http_ng_native::testing::empty_body()).unwrap();
        let resp = http_ng_native::testing::exchange_for_test(io, req).await.unwrap();
        assert_eq!(resp.status(), 200);
        let body = http_ng_native::testing::collect(resp.into_body()).await.unwrap();
        assert_eq!(&body[..], b"hello");
    });
}

#[test]
fn body_keeps_driving_the_connection_after_headers() {
    // Тело приходит отдельным чанком после заголовков: если бы соединение
    // перестали поллить, чтение зависло бы.
    let addr = spawn_h1_server(
        "HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n5\r\nhello\r\n0\r\n\r\n");
    futures_executor::block_on(async move {
        let std_tcp = std::net::TcpStream::connect(addr).unwrap();
        let io = http_ng_native::testing::blocking_io(std_tcp);
        let req = http::Request::builder().uri("/").body(
            http_ng_native::testing::empty_body()).unwrap();
        let resp = http_ng_native::testing::exchange_for_test(io, req).await.unwrap();
        let body = http_ng_native::testing::collect(resp.into_body()).await.unwrap();
        assert_eq!(&body[..], b"hello");
    });
}
```

- [ ] **Step 2: Запустить и убедиться, что падает**

Run: `cargo test -p http-ng-native --test h1`
Expected: FAIL — `exchange_for_test` не найден.

- [ ] **Step 3: Реализовать обмен с инлайн-драйвом**

```rust
// crates/http-ng-native/src/h1.rs
use crate::body::{BoxError, OutgoingBody};
use bytes::Bytes;
use http_body::{Body, Frame};
use http_ng_core::{Error, ErrorKind};
use hyper::client::conn::http1;
use std::pin::Pin;
use std::task::{Context, Poll};

/// Тело ответа, которое **само поллит соединение**.
///
/// Без этого после прихода заголовков соединение перестало бы двигаться:
/// hyper требует, чтобы кто-то драйвил `Connection`, а мы принципиально не
/// спавним — иначе понадобился бы `Send + 'static` и однопоточные рантаймы
/// оказались бы закрыты.
pub struct NativeBody {
    incoming: hyper::body::Incoming,
    conn: Option<Pin<Box<dyn std::future::Future<Output = hyper::Result<()>>>>>,
}

impl std::fmt::Debug for NativeBody {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("NativeBody")
    }
}

impl Body for NativeBody {
    type Data = Bytes;
    type Error = Error;

    fn poll_frame(mut self: Pin<&mut Self>, cx: &mut Context<'_>)
        -> Poll<Option<Result<Frame<Bytes>, Error>>>
    {
        let this = &mut *self;
        // Сначала подвинуть соединение — иначе данные не приедут.
        if let Some(conn) = this.conn.as_mut() {
            match conn.as_mut().poll(cx) {
                Poll::Ready(Ok(())) => { this.conn = None }
                Poll::Ready(Err(e)) => {
                    this.conn = None;
                    return Poll::Ready(Some(Err(Error::new(ErrorKind::Body, e))));
                }
                Poll::Pending => {}
            }
        }
        match Pin::new(&mut this.incoming).poll_frame(cx) {
            Poll::Ready(Some(Ok(f))) => Poll::Ready(Some(Ok(f))),
            Poll::Ready(Some(Err(e))) =>
                Poll::Ready(Some(Err(Error::new(ErrorKind::Body, e)))),
            Poll::Ready(None) => Poll::Ready(None),
            Poll::Pending => Poll::Pending,
        }
    }

    fn is_end_stream(&self) -> bool { self.incoming.is_end_stream() }
}

/// Один запрос по одному соединению. Пула в v0.1 нет.
pub(crate) async fn exchange<I>(io: I, req: http::Request<OutgoingBody>)
    -> Result<http::Response<NativeBody>, Error>
where I: hyper::rt::Read + hyper::rt::Write + Unpin + 'static
{
    let (mut sender, conn) = http1::handshake::<I, OutgoingBody>(io).await
        .map_err(|e| Error::new(ErrorKind::Connect, e))?;

    // Драйвим соединение и запрос **вместе**, без spawn.
    let mut conn = Box::pin(conn);
    let mut send = Box::pin(sender.send_request(req));

    let resp = std::future::poll_fn(|cx| {
        if let Poll::Ready(r) = conn.as_mut().poll(cx) {
            if let Err(e) = r {
                return Poll::Ready(Err(Error::new(ErrorKind::Connect, e)));
            }
        }
        match send.as_mut().poll(cx) {
            Poll::Ready(Ok(r)) => Poll::Ready(Ok(r)),
            Poll::Ready(Err(e)) => Poll::Ready(Err(Error::new(ErrorKind::Connect, e))),
            Poll::Pending => Poll::Pending,
        }
    }).await?;

    let (parts, incoming) = resp.into_parts();
    Ok(http::Response::from_parts(parts, NativeBody {
        incoming,
        conn: Some(conn as Pin<Box<dyn std::future::Future<Output = hyper::Result<()>>>>),
    }))
}
```

Пояснение к `Box<dyn Future>`: это **единственное** место в вертикали, где мы
боксим, и боксим не ради стирания `Send`, а чтобы сложить соединение в поле
тела. Тип остаётся `!Send`-совместимым — бонд `Send` не объявляется.

- [ ] **Step 4: Добавить тестовые хелперы**

```rust
// в crates/http-ng-native/src/lib.rs, mod testing
    pub use crate::h1::NativeBody;

    pub fn empty_body() -> crate::body::OutgoingBody {
        crate::body::OutgoingBody::from_request_body(http_ng_core::RequestBody::Empty)
    }

    /// Блокирующий `std::net::TcpStream` как `hyper::rt` IO — только для тестов
    /// на голом executor'е, где реактора нет вовсе.
    pub fn blocking_io(s: std::net::TcpStream) -> BlockingIo { BlockingIo(s) }

    pub struct BlockingIo(std::net::TcpStream);
    // impl hyper::rt::Read / Write через std::io::{Read, Write},
    // всегда возвращая Poll::Ready — сокет блокирующий.

    pub async fn exchange_for_test<I>(io: I, req: http::Request<crate::body::OutgoingBody>)
        -> Result<http::Response<crate::h1::NativeBody>, http_ng_core::Error>
    where I: hyper::rt::Read + hyper::rt::Write + Unpin + 'static
    { crate::h1::exchange(io, req).await }

    pub async fn collect(b: crate::h1::NativeBody)
        -> Result<bytes::Bytes, http_ng_core::Error>
    {
        use http_body_util::BodyExt;
        Ok(b.collect().await?.to_bytes())
    }
```

`BlockingIo` реализовать полностью: `poll_read` читает в стековый буфер и
кладёт через `put_slice`, `poll_write`/`poll_flush` — прямые вызовы
`std::io::Write`, `poll_shutdown` — `shutdown(Both)`. Всегда `Poll::Ready`.

- [ ] **Step 5: Запустить тесты**

Run: `cargo test -p http-ng-native --test h1`
Expected: PASS, два теста. **Это и есть доказательство рантайм-нейтральности:**
ни tokio, ни smol в тесте нет.

- [ ] **Step 6: Commit**

```bash
git add crates/http-ng-native
git commit -m "feat(native): HTTP/1 exchange driving the connection inline, no spawn"
```

---

### Task 13: `http-ng-native` — `Native<R, T, D>: Transport`

**Files:**
- Modify: `crates/http-ng-native/src/lib.rs`
- Test: `crates/http-ng-native/tests/transport.rs`

**Interfaces:**
- Consumes: всё предыдущее.
- Produces:
  - `pub struct Native<R, T, D> { rt: R, tls: T, dns: D, opts: TcpOpts }`
  - `Native::new(rt: R, tls: T, dns: D) -> Self`; `Native::tcp_opts(self, o: TcpOpts) -> Self`
  - `impl<R, T, D> Transport for Native<R, T, D>` с `type Body = NativeBody; type Error = Error;`
  - **`Transport::to_error` ОБЯЗАН быть переопределён тождеством** — см. блок
    ниже, это не необязательная деталь
  - `Capabilities`: `streaming_request_body: false` (v0.1), `redirects: Configurable`,
    `tls_config: Full`, `version_reported: true`, `timeouts.connect: true`,
    `timeouts.first_byte: false`, `timeouts.between_bytes: false`,
    `upgrade: UpgradeSupport::None` (h1-upgrade — v0.3)

> **Обязательное переопределение `Transport::to_error`.** Вертикаль 1
> (находка B2 её финального ревью) добавила в шов дефолтный хук
> `fn to_error(&self, e: Self::Error) -> Error`, превращающий ошибку бэкенда
> в ошибку библиотеки. Дефолт заворачивает её с `ErrorKind::Other` — это верно
> ровно для бэкенда, которому нечего сказать о категории. `Native` — не он:
> его `type Error = Error`, то есть категория УЖЕ проставлена
> (`ErrorKind::Resolve` в `execute`, `Connect` в `race_connect`, `Tls` в
> `TlsConnect`, `Body` в `h1`). Без переопределения `Client::execute` завернёт
> её во вторую ошибку, и у потребителя `kind()` станет `Other`, а
> `is_timeout()`/`is_connect()`/`is_unsupported()` — `false` для всего сразу;
> `Display` вдобавок напечатает категорию дважды (`Other: Connect: …`).
>
> Именно так вертикаль 1 и отгрузилась бы: `http-ng-wasi` раскладывал 39
> вариантов `ErrorCode` на восемь `ErrorKind` сорока строками, и всё это
> выбрасывалось слоем выше. 165 тестов этого не видели, потому что ни один не
> проверял, что категория транспорта доживает до вызывающей стороны.
>
> Компилятор эту обязанность не проверяет и не может: дефолт остаётся
> дефолтом сознательно (альтернативы потребовали бы `Send + Sync` от ошибки
> КАЖДОГО бэкенда на уровне трейта, а поправка C1 сохраняет представимость
> транспорта с честно `!Send` ошибкой). Проверяет её только тест — он в Step 1
> ниже, и он обязателен, а не «желателен». Образцы:
> `to_error_is_the_identity_so_the_classification_survives_the_client`
> (`crates/http-ng-wasi/src/convert.rs`) и
> `crates/http-ng/tests/transport_error.rs`.

- [ ] **Step 1: Написать падающий тест**

```rust
// crates/http-ng-native/tests/transport.rs
use http_ng::Client;
use http_ng_dns_system::SystemDns;
use http_ng_native::Native;
use http_ng_rt_tokio::Tokio;
use http_ng_tls_rustls::Rustls;

fn spawn_h1_server() -> std::net::SocketAddr {
    use std::io::{Read, Write};
    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = l.local_addr().unwrap();
    std::thread::spawn(move || {
        for s in l.incoming() {
            let Ok(mut s) = s else { continue };
            let mut b = [0u8; 2048];
            let _ = s.read(&mut b);
            let _ = s.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok");
        }
    });
    addr
}

#[tokio::test]
async fn end_to_end_over_plain_tcp() {
    let addr = spawn_h1_server();
    let t = Native::new(Tokio, Rustls::with_webpki_roots(), SystemDns::new(Tokio));
    let c = Client::builder(t).build().unwrap();
    let resp = c.get(&format!("http://{addr}/")).send().await.unwrap();
    assert_eq!(resp.status(), 200);
    assert_eq!(resp.collect().await.unwrap().text().unwrap(), "ok");
}

#[tokio::test]
async fn capabilities_are_honest_about_v01_limits() {
    use http_ng_core::unversioned::Transport;
    let t = Native::new(Tokio, Rustls::with_webpki_roots(), SystemDns::new(Tokio));
    let caps = t.capabilities();
    assert!(!caps.streaming_request_body, "в v0.1 тело буферизовано");
    assert!(caps.timeouts.connect);
    assert!(!caps.timeouts.first_byte, "нет пула и таймера ответа — заявлять нельзя");
    assert_eq!(caps.upgrade, http_ng_core::UpgradeSupport::None);
    assert_eq!(caps.tls_config, http_ng_core::TlsSupport::Full);
}

/// Категория, которую проставил `Native`, обязана дожить до вызывающей
/// стороны через весь путь `Client::execute` (см. блок над Step 1). Тест
/// проверяет именно этот путь целиком, а не дефолт `to_error` — тот
/// проверен в `http-ng-core/tests/shape.rs` и сам по себе гарантирует
/// пропуск `Error` насквозь.
///
/// Хост выбран несуществующим намеренно: это единственный отказ, который
/// `execute` производит без сети и без сервера, и `wasi`-аналог этого
/// теста устроен так же — гоняет реальный классификатор бэкенда, а не
/// сконструированную вручную `Error`.
#[tokio::test]
async fn transport_error_kind_survives_the_client_instead_of_flattening_to_other() {
    let t = Native::new(Tokio, Rustls::with_webpki_roots(), SystemDns::new(Tokio));
    let c = Client::builder(t).build().unwrap();
    let err = c.get("http://nonexistent.invalid/").send().await.unwrap_err();

    assert_eq!(
        *err.kind(),
        http_ng::ErrorKind::Resolve,
        "категория обязана дожить до вызывающей стороны, а не расплющиться в Other: {err}"
    );
    assert!(
        !err.to_string().starts_with("Other:"),
        "категория печатается один раз, и это настоящая категория: {err}"
    );
}

#[tokio::test]
async fn unsupported_timeout_is_rejected_at_build_time() {
    use std::time::Duration;
    let t = Native::new(Tokio, Rustls::with_webpki_roots(), SystemDns::new(Tokio));
    let err = Client::builder(t)
        .timeouts(http_ng::Timeouts {
            between_bytes: Some(Duration::from_secs(1)), ..Default::default() })
        .build().unwrap_err();
    assert_eq!(err.what, "between_bytes_timeout");
}
```

- [ ] **Step 2: Запустить и убедиться, что падает**

Run: `cargo test -p http-ng-native --test transport`
Expected: FAIL — `Native` не найден.

- [ ] **Step 3: Реализовать транспорт**

```rust
// в crates/http-ng-native/src/lib.rs
use futures_util::StreamExt;
use http_ng_core::unversioned::Transport;
use http_ng_core::{Capabilities, Error, ErrorKind, RedirectSupport, RequestBody,
                   TimeoutSupport, Timeouts, TlsSupport, UpgradeSupport};
use http_ng_dns::Resolve;
use http_ng_rt::{TcpConnect, TcpOpts, Timer};
use http_ng_tls::{TlsConnect, TlsRequest};

#[derive(Debug)]
pub struct Native<R, T, D> {
    rt: R,
    tls: T,
    dns: D,
    opts: TcpOpts,
    caps: Capabilities,
}

impl<R, T, D> Native<R, T, D> {
    pub fn new(rt: R, tls: T, dns: D) -> Self {
        let mut caps = Capabilities::none();
        // Честно про v0.1: пула нет, стриминга тела запроса нет, upgrade нет.
        caps.streaming_request_body = false;
        caps.redirects = RedirectSupport::Configurable;
        caps.tls_config = TlsSupport::Full;
        caps.version_reported = true;
        caps.timeouts = TimeoutSupport {
            connect: true, first_byte: false, between_bytes: false,
        };
        caps.upgrade = UpgradeSupport::None;
        Self { rt, tls, dns, opts: TcpOpts::default(), caps }
    }

    pub fn tcp_opts(mut self, o: TcpOpts) -> Self { self.opts = o; self }
}

#[derive(Debug)] struct MissingHost;
impl std::fmt::Display for MissingHost {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "request URI has no host")
    }
}
impl std::error::Error for MissingHost {}

impl<R, T, D> Transport for Native<R, T, D>
where
    R: TcpConnect + Timer,
    R::Stream: 'static,
    T: TlsConnect,
    T::Stream<R::Stream>: 'static,
    D: Resolve,
{
    type Body = h1::NativeBody;
    type Error = Error;

    async fn execute(&self, req: http::Request<RequestBody>)
        -> Result<http::Response<Self::Body>, Error>
    {
        let (parts, body) = req.into_parts();
        let host = parts.uri.host().ok_or_else(||
            Error::new(ErrorKind::Other, MissingHost))?.to_owned();
        let https = parts.uri.scheme_str() == Some("https");
        let port = parts.uri.port_u16().unwrap_or(if https { 443 } else { 80 });

        // Раздельные стримы: коннектимся по AAAA, не дожидаясь A.
        let v6: Vec<_> = self.dns.lookup_ipv6(&host)
            .filter_map(|r| async { r.ok().map(|a| a.addr) }).collect().await;
        let v4: Vec<_> = self.dns.lookup_ipv4(&host)
            .filter_map(|r| async { r.ok().map(|a| a.addr) }).collect().await;
        if v6.is_empty() && v4.is_empty() {
            return Err(Error::new(ErrorKind::Resolve, MissingHost));
        }

        let tcp = connect::race_connect(&self.rt, v6, v4, port, &self.opts).await?;

        let outgoing = body::OutgoingBody::from_request_body(body);
        let mut req = http::Request::from_parts(parts, outgoing);
        // hyper h1 требует origin-form и заголовок Host.
        strip_to_origin_form(&mut req, &host, port, https);

        if https {
            let (stream, _info) = self.tls.connect(tcp, TlsRequest {
                server_name: &host, alpn: &[b"http/1.1"], ech: None,
            }).await?;
            h1::exchange(stream, req).await
        } else {
            h1::exchange(tcp, req).await
        }
    }

    /// Тождество: `Self::Error` — уже `http_ng_core::Error`, и категория в
    /// ней проставлена там, где отказ произошёл (`Resolve` выше, `Connect` в
    /// `race_connect`, `Tls` в `TlsConnect`, `Body` в `h1`). Дефолт хука
    /// сделал бы ровно это же (он узнаёт нашу `Error` и пропускает её
    /// насквозь), так что строка избыточна по поведению и нужна по смыслу:
    /// называет намерение там, где его читают. См. блок над Step 1.
    fn to_error(&self, e: Self::Error) -> Error { e }

    fn capabilities(&self) -> &Capabilities { &self.caps }
}

fn strip_to_origin_form(
    req: &mut http::Request<body::OutgoingBody>,
    host: &str,
    port: u16,
    https: bool,
) {
    let default_port = if https { 443 } else { 80 };
    let authority = if port == default_port {
        host.to_owned()
    } else {
        format!("{host}:{port}")
    };
    if !req.headers().contains_key(http::header::HOST) {
        if let Ok(v) = http::HeaderValue::from_str(&authority) {
            req.headers_mut().insert(http::header::HOST, v);
        }
    }
    let pq = req.uri().path_and_query().map(|p| p.as_str()).unwrap_or("/").to_owned();
    if let Ok(u) = pq.parse::<http::Uri>() {
        *req.uri_mut() = u;
    }
}
```

- [ ] **Step 4: Запустить тесты**

Run: `cargo test -p http-ng-native`
Expected: PASS, все тесты крейта.

- [ ] **Step 5: Commit**

```bash
git add crates/http-ng-native
git commit -m "feat(native): Native transport wiring runtime, TLS and DNS together"
```

---

### Task 14: `http-ng` — `DefaultTransport` и сквозной прогон на двух рантаймах

**Files:**
- Modify: `crates/http-ng/Cargo.toml`, `crates/http-ng/src/client.rs`, `src/lib.rs`
- Create: `crates/http-ng/tests/two_runtimes.rs`
- Modify: `.github/workflows/ci.yml`
- Create: `README.md` (обновить раздел «Статус»)

**Interfaces:**
- Consumes: `Native` (Task 13), `Tokio` (Task 3), `Smol` (Task 4).
- Produces:
  - `pub type DefaultTransport` — cfg-выбранный по таргету
  - `pub struct Client<T = DefaultTransport>` — добавлен дефолтный параметр
  - `Client::new() -> Result<Client<DefaultTransport>, UnsupportedCapability>` под
    фичей `default-transport`

- [ ] **Step 1: Написать падающий тест**

```rust
// crates/http-ng/tests/two_runtimes.rs
//! Один и тот же код, два рантайма, ноль cfg. Если этот файл потребует
//! `#[cfg]`, рантайм-шов декоративен и вертикаль провалена.

use http_ng::{Client, Timeouts};
use http_ng_dns_system::SystemDns;
use http_ng_native::Native;
use http_ng_tls_rustls::Rustls;

fn spawn_server() -> std::net::SocketAddr {
    use std::io::{Read, Write};
    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = l.local_addr().unwrap();
    std::thread::spawn(move || {
        for s in l.incoming() {
            let Ok(mut s) = s else { continue };
            let mut b = [0u8; 2048];
            let _ = s.read(&mut b);
            let _ = s.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 4\r\n\r\nsame");
        }
    });
    addr
}

/// Обобщённая функция: её тело — тот самый «один код на все рантаймы».
async fn fetch_once<R>(rt: R, addr: std::net::SocketAddr) -> String
where
    R: http_ng_rt::TcpConnect + http_ng_rt::Timer + http_ng_rt::Blocking + Clone,
    R::Stream: 'static,
{
    let t = Native::new(rt.clone(), Rustls::with_webpki_roots(), SystemDns::new(rt));
    let c = Client::builder(t)
        .timeouts(Timeouts { connect: Some(std::time::Duration::from_secs(5)),
                             ..Default::default() })
        .build().unwrap();
    c.get(&format!("http://{addr}/")).send().await.unwrap()
        .collect().await.unwrap().text().unwrap()
}

#[test]
fn identical_code_on_tokio() {
    let addr = spawn_server();
    let rt = tokio::runtime::Runtime::new().unwrap();
    assert_eq!(rt.block_on(fetch_once(http_ng_rt_tokio::Tokio, addr)), "same");
}

#[test]
fn identical_code_on_smol() {
    let addr = spawn_server();
    assert_eq!(
        futures_executor::block_on(fetch_once(http_ng_rt_smol::Smol, addr)),
        "same"
    );
}
```

- [ ] **Step 2: Запустить и убедиться, что падает**

Run: `cargo test -p http-ng --test two_runtimes`
Expected: FAIL — dev-зависимостей нет.

- [ ] **Step 3: Добавить `DefaultTransport` и дефолтный параметр**

```rust
// в crates/http-ng/src/lib.rs
/// Транспорт по умолчанию, выбираемый **таргетом, а не пользователем**.
///
/// Дефолт — мнение, а не ограничение: `Client` без параметра означает
/// `Client<DefaultTransport>`, а `Client<ЧтоУгодно>` работает так же.
/// Взаимоисключающих cargo-фич не возникает, потому что выбирает таргет.
#[cfg(all(feature = "default-transport", not(target_family = "wasm")))]
pub type DefaultTransport = http_ng_native::Native<
    http_ng_rt_tokio::Tokio,
    http_ng_tls_rustls::Rustls,
    http_ng_dns_system::SystemDns<http_ng_rt_tokio::Tokio>,
>;

#[cfg(all(feature = "default-transport", target_family = "wasm", target_os = "wasi"))]
pub type DefaultTransport = http_ng_wasi::WasiHttp;
```

В `client.rs` заменить объявление на:

```rust
#[cfg(feature = "default-transport")]
pub struct Client<T = crate::DefaultTransport> { transport: T, config: Config }
#[cfg(not(feature = "default-transport"))]
pub struct Client<T> { transport: T, config: Config }
```

и добавить:

```rust
#[cfg(all(feature = "default-transport", not(target_family = "wasm")))]
impl Client<crate::DefaultTransport> {
    /// Клиент с транспортом по умолчанию.
    ///
    /// На native требует окружающего tokio-рантайма: `tokio::spawn` и
    /// `tokio::time::sleep` вне рантайма паникуют. Ровно так же ведёт себя
    /// reqwest. Явный путь — `Client::builder(Native::new(rt, tls, dns))`.
    pub fn new() -> Result<Self, http_ng_core::UnsupportedCapability> {
        let rt = http_ng_rt_tokio::Tokio;
        Self::builder(http_ng_native::Native::new(
            rt,
            http_ng_tls_rustls::Rustls::with_platform_verifier()
                .expect("platform verifier"),
            http_ng_dns_system::SystemDns::new(rt),
        )).build()
    }
}
```

В `Cargo.toml` добавить фичу и target-зависимости:

```toml
[features]
default = []
test-util = []
default-transport = []

[target.'cfg(not(target_family = "wasm"))'.dependencies]
http-ng-native     = { path = "../http-ng-native",     version = "0.1.0", optional = true }
http-ng-rt-tokio   = { path = "../http-ng-rt-tokio",   version = "0.1.0", optional = true }
http-ng-tls-rustls = { path = "../http-ng-tls-rustls", version = "0.1.0", optional = true }
http-ng-dns-system = { path = "../http-ng-dns-system", version = "0.1.0", optional = true }

[dev-dependencies]
futures-executor   = { version = "0.3", default-features = false, features = ["std"] }
http-ng-native     = { path = "../http-ng-native" }
http-ng-rt         = { path = "../http-ng-rt" }
http-ng-rt-tokio   = { path = "../http-ng-rt-tokio" }
http-ng-rt-smol    = { path = "../http-ng-rt-smol" }
http-ng-tls-rustls = { path = "../http-ng-tls-rustls", features = ["webpki-roots"] }
http-ng-dns-system = { path = "../http-ng-dns-system" }
tokio              = { version = "1", features = ["rt-multi-thread"] }
```

Фичу `default-transport` дополнить списком `dep:` для не-wasm таргетов.

- [ ] **Step 4: Запустить сквозной тест**

Run: `cargo test -p http-ng --test two_runtimes`
Expected: PASS, два теста. Убедиться, что в `two_runtimes.rs` **нет ни одного
`#[cfg]`** — это и есть критерий приёмки вертикали.

- [ ] **Step 5: Проверить, что смол-путь не подтащил async-compat и tokio-рантайм**

Run: `cargo tree -p http-ng-rt-smol -e normal --prefix none | grep -E '^(tokio|async-compat)' && exit 1 || echo OK`
Expected: `OK`.

- [ ] **Step 6: Обновить CI**

```yaml
  # добавить job
  two-runtimes:
    strategy:
      matrix:
        os: [ubuntu-latest, macos-latest, windows-latest]
    runs-on: ${{ matrix.os }}
    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@stable
      - run: cargo test -p http-ng --test two_runtimes
      - name: smol path must not pull tokio or async-compat
        run: |
          cargo tree -p http-ng-rt-smol -e normal --prefix none \
            | grep -E '^(tokio|async-compat)' && exit 1 || true
```

- [ ] **Step 7: Commit**

```bash
git add crates/http-ng .github/workflows/ci.yml README.md
git commit -m "feat(http-ng): DefaultTransport and identical code proven on tokio and smol"
```

---

## Что эта вертикаль доказала и что осталось

**Доказано:** рантайм-шов настоящий — один и тот же обобщённый код работает на
tokio и smol без `#[cfg]`, а h1-обмен едет даже на голом `futures`-executor'е
без spawn и без таймера. TLS-адаптер один на все рантаймы, потому что написан
против `hyper::rt`. `Send` объявлен ровно в одном месте (`Blocking`) и не
заражает ядро.

**Не доказано и переходит в вертикаль 3:** рантайм-модель `Capabilities`
(нужен fetch с его различием Chrome/Safari); реконнект `SseStream`; приёмка
`act`.

**Осознанно не сделано в v0.1, записать в rustdoc:** пул соединений (одно
соединение на запрос); стриминговые тела запроса; `first_byte`/`between_bytes`
таймауты (заявлены как неподдерживаемые, а не сделаны молча); один
`getaddrinfo` на оба семейства вместо двух слотов; h1-upgrade.
