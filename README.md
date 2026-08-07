# http-ng

Кроссплатформенный асинхронный HTTP-клиент. Один и тот же прикладной код
собирается под native, браузер и WASI — транспорт подменяется, а не
обкладывается `#[cfg]`.

```rust
let client = http_ng::Client::builder(transport).build()?;
let text = client.get("https://example.com").send().await?.collect().await?.text()?;
```

На native, с фичей `default-transport` (Task 14, вертикаль 2) — тот же код
без выбора транспорта вручную: `Client::new()` резолвит `DefaultTransport`
(`Native` на `tokio` + `rustls` с системным хранилищем доверия + системный
`getaddrinfo`) сам, по таргету, а не по фиче, которую выбирает пользователь.

```rust
let client = http_ng::Client::new()?; // требует окружающий tokio-рантайм
let text = client.get("https://example.com").send().await?.collect().await?.text()?;
```

Сквозное доказательство того, что ЭТОТ ЖЕ обобщённый код (не два
раздельных примера) реально бегает по сети на двух разных рантаймах без
единого `#[cfg]` —
[`crates/http-ng/tests/two_runtimes.rs`](crates/http-ng/tests/two_runtimes.rs):
`cargo test -p http-ng --test two_runtimes` инстанцирует одну и ту же
`fetch_once<R>` под `http_ng_rt_tokio::Tokio` (реальный `tokio::runtime::
Runtime`) и под `http_ng_rt_smol::Smol` (голый `futures_executor::block_on`,
без spawn и без `tokio` в графе смол-пути — см. следующий раздел).

Рабочий сквозной пример, который реально собирается и исполняется под
`wasmtime` (не только компилируется) —
[`crates/http-ng-wasi/examples/fetch.rs`](crates/http-ng-wasi/examples/fetch.rs):

```
cargo build -p http-ng-wasi --example fetch --target wasm32-wasip2
wasmtime run -S http -- target/wasm32-wasip2/debug/examples/fetch.wasm
```

## Что в графе зависимостей

Первая строка таблицы, как и раньше, проверяема прямо в этом репозитории:
`cargo tree -p http-ng-wasi -e normal --prefix none` не содержит `tokio`
вовсе (28 уникальных крейтов всего). Вторая и третья строки, в отличие от
их же версии в отчёте вертикали 1, теперь тоже измерены, а не предсказаны:
вертикаль 2 (`http-ng-native`, `http-ng-rt-tokio`, `http-ng-rt-smol`,
`http-ng-tls-rustls`, `http-ng-dns-system`) собрана, и с Task 14 у `http-ng`
есть `DefaultTransport` (native, только HTTP/1.1) — фича `default-transport`,
включающая ровно эти четыре крейта. Строка HTTP/2 остаётся тем же
исследованием заранее, что и была: `http-ng-h2` в этом репозитории не
существует (не в плане v0.1 вообще, а не только «пока не собрана»),
приведена нетронутой ради того же обоснования выбора HTTP/1-first, каким
она была написана в вертикали 1.

| сборка | tokio |
|---|---|
| ambient (`http-ng` + `-wasi` / `-fetch`) — измерено | **нет вообще** |
| `http-ng` c фичей `default-transport` (native, только HTTP/1.1) — измерено, Task 14 | настоящий: `[default, libc, mio, net, rt, socket2, sync, time]` — реактор `http-ng-rt-tokio` нужен для настоящих `TcpConnect`/`Timer`, это не «просто протащенный тип», см. ниже |
| `http-ng-rt-smol` в изоляции (без `http-ng`, ту же способность даёт `async-io`) — измерено, Task 14 | `[default, sync]` — лист без реактора, только `tokio::sync::oneshot`, см. ниже |
| native + HTTP/2 — не в плане v0.1, гипотетическая оценка не пересчитывалась в вертикали 2 | `h2` тянет `tokio` с `io-util` и `tokio-util` с `codec`, а через него `libc` |

**Обе средние строки — один и тот же факт `hyper`, измеренный в двух разных
местах графа, а не два независимых наблюдения.** `hyper` зависит от `tokio`
**безусловно, не по фиче** — `http-ng-rt`'s собственный `hyper = { version =
"1.11", default-features = false }` (нулевой набор фич) и всё равно тянет
`tokio` с фичей `sync`, проверено `cargo tree -p http-ng-rt -e normal -i
tokio` в этом дереве. Это тот же вывод, что вертикаль 1 сделала про
HTTP/1-путь по исходникам hyper (`tokio::sync::oneshot::Receiver` в
`src/upgrade.rs`, единственное место использования) — теперь подтверждено
измерением, а не только чтением кода. Крейт `http-ng-rt-smol` зависит от
`http-ng-rt`, а значит от `hyper`, а значит транзитивно и от этого
`tokio`-листа — **независимо от того, что сам `http-ng-rt-smol` не тянет ни
`tokio`, ни `async-compat` напрямую** (`cargo tree -p http-ng-rt-smol -e
normal` не содержит ни того, ни другого крейта в своих ПРЯМЫХ
зависимостях — проверяет CI job `two-runtimes`). Разница между строками
таблицы — не «у смол-пути нет tokio, у native есть», а какой РЕАКТОР
реально стоит за этим листом: у `http-ng-rt-smol` в изоляции — никакого
(`sync`-лист инертен, `tokio::sync::oneshot` не исполняется), у `http-ng` с
`default-transport` — настоящий (`http-ng-rt-tokio`, Task 3, тянет `mio` +
`net` + `rt` + `time` для настоящих сокетов и таймеров), и оба факта верны
одновременно: смол-рантайм по-прежнему не запускает НИ ОДНОЙ строчки tokio,
просто крейт `tokio` физически лежит на диске тем же листом, что и у любой
другой сборки, использующей `hyper`.

Убрать tokio из hyper-сборок нельзя: [hyper#3428](https://github.com/hyperium/hyper/pull/3428)
(ровно эта замена на `futures-channel`, спрятанная за feature-флагом) отклонена
мейнтейнером не по техническим причинам, а из-за необратимости решения:
*«As of 1.0, we are going to be very careful about adding new dependencies to
the public API… it "exposes" a crate feature that we could never remove»*.
[hyper#3767](https://github.com/hyperium/hyper/issues/3767) — отдельный тикет
с тем же выводом про единственное место использования — закрыт как *not
planned*.

**Второй факт, тоже измеренный, а не предположенный: тест-only busy-spin не
достигает продакшн-кода.** `http_ng_native::testing::blocking_io` (Task 12) —
`hyper::rt::Read`/`Write`-обёртка над `std::net::TcpStream` для теста на
голом `futures`-executor'е без реактора вовсе; при `WouldBlock` она вызывает
`cx.waker().wake_by_ref()` немедленно вместо настоящего ожидания готовности
через ОС. Измерено CPU-временем (`/proc/self/stat`) вокруг запроса к
серверу, отвечающему через 600мс: под `blocking_io` — wall 600.4мс, **cpu
600мс** (честный busy-spin на 100% времени ожидания); тот же самый код
обмена (`h1::exchange`/`NativeBody`), но с IO от `http_ng_rt_tokio::Tokio::
connect` (настоящий `tokio::net::TcpStream`, зарегистрированный в реакторе)
— wall 601.1мс, **cpu 0мс** (Task 12 review, раздел B). `two_runtimes.rs`
(Task 14) подтверждает предсказание того же раздела «не произойдёт под
tokio или smol» на практике: оба теста гоняют `Native` через настоящие
`http_ng_rt_tokio::Tokio`/`http_ng_rt_smol::Smol`, ни разу не касаясь
`testing::blocking_io` — он существует только под `#[doc(hidden)] pub mod
testing` и используется только в `tests/h1.rs` этого же крейта.

## Статус

v0.1: ядро (`http-ng-core`, `http-ng-proto`, `http-ng`) и два бэкенда —
`http-ng-wasi` поверх `wasi:http` 0.3 (вертикаль 1) и `http-ng-native`
поверх `hyper` + `rustls` + системного DNS (вертикаль 2, эта секция).
Браузер (`fetch`) — вертикаль 3, ещё не начата.

### Вертикаль 2 (native): что доказано

**Рантайм-шов настоящий, не декоративный.** Один и тот же обобщённый код
(`fetch_once<R>` в `crates/http-ng/tests/two_runtimes.rs`, границы —
`http_ng_rt::{TcpConnect, Timer, Blocking} + Clone`, ни одного `#[cfg]` во
всём файле) реально гоняет HTTP/1.1-запрос по настоящему TCP до настоящего
сервера на loopback — один раз под `http_ng_rt_tokio::Tokio` внутри
`tokio::runtime::Runtime`, один раз под `http_ng_rt_smol::Smol` на голом
`futures_executor::block_on`. Свойство подтверждено не только зелёным
прогоном: добавление `R::Instant: PartialEq<std::time::Instant>` к границе
`fetch_once` (тот же приём мутации, что `http-ng-rt-pair-check`'s
`pair_property.rs` уже применяла к способностям рантайма отдельно) ломает
инстанциацию на `Tokio` (`Instant = tokio::time::Instant`, обёртка,
`E0277: can't compare tokio::time::Instant with std::time::Instant`) и не
ломает `Smol` (`Instant = std::time::Instant` напрямую) — тест
чувствителен к регрессии шва, а не только к тому, компилируется ли файл
вообще.

**HTTP/1-обмен едет без spawn и без реактора там, где его нет.**
`http-ng-native/tests/h1.rs`'s `works_on_a_bare_futures_executor_with_no_spawn`
проверяет это на IO без реактора вовсе (Task 12); `two_runtimes.rs` выше
проверяет ту же собственность транспорта (`Native`) уже под настоящими
рантайм-бэкендами, а не только под тестовым busy-spin.

**`DefaultTransport`/`Client<T = DefaultTransport>`/`Client::new()`** —
фича `default-transport` (`default = []` для `http-ng`, как и для каждого
крейта вертикали — включать явно). На любом не-wasm таргете резолвится в
`Native<Tokio, Rustls, SystemDns<Tokio>>` с системным хранилищем доверия
(`rustls-platform-verifier`, не `webpki-roots` — «просто заработавший»
клиент, а не клиент с явно выбранными корнями). Без фичи, или на
`wasm32-unknown-unknown`/`wasm32-wasip2` (`target_os = "wasi"`), тип не
существует вовсе — обычная ошибка компиляции, а не более слабый молчаливый
транспорт; на wasip2/wasip1 намеренно нет ветки, использующей уже готовый
`http_ng_wasi::WasiHttp` через этот механизм — `http-ng` не зависит от
`http-ng-wasi` (инвариант, записанный в `http-ng-wasi/Cargo.toml`), и
заводить эту зависимость здесь означало бы путь, который ни один CI job
в этом репозитории не собирает (`wasip2`-job гоняет `http-ng-wasi`
напрямую). Прямой путь на WASI остаётся `Client::builder(http_ng_wasi::
WasiHttp::new())`, как и до этой задачи. Подробности резолюции — doc-
комментарий `DefaultTransport` в `crates/http-ng/src/lib.rs`.

**Что осталось не проверено вживую и переходит в вертикаль 3** (граница из
брифа вертикали, не сокращена этой задачей): рантайм-модель `Capabilities`
для fetch с его различием Chrome/Safari; реконнект `SseStream`; приёмка
`act`.

**Осознанно не сделано в v0.1** (записано, не спрятано): пул соединений
(одно соединение на запрос); стриминговые тела запроса; `first_byte`/
`between_bytes`-таймауты (заявлены как неподдерживаемые через
`Capabilities`, а не сделаны молча); один `getaddrinfo`-вызов на оба
семейства адресов вместо раздельных слотов v4/v6; h1-upgrade.

### Вертикаль 1 (WASI): что доказано

**Доказано.** Форма `Transport` реально работает против ambient-бэкенда без
собственного сокета гостя — не в теории, а под настоящим хостом `wasmtime`
(`crates/http-ng-wasi/tests/live_roundtrip.rs`). Настройка, которую транспорт
не поддерживает, становится типизированной ошибкой `UnsupportedCapability` уже
на `ClientBuilder::build()`, а не тихо игнорируется; то же самое на уровень
ниже — хост `wasi:http` отвергает значение опции запроса (таймаут, метод,
scheme) — тоже становится ошибкой, а не отбрасывается, и это не только
проверено вручную при реализации, а держится статическим анализом в CI
(`no-discarded-wasi-setters`) на каждый пуш.

**`full_duplex` объявлен `false` — и это про реализацию `http-ng-wasi`, не
про форму seam.** Сам протокол `wasi:http` 0.3 поддерживает дуплекс тела
запроса: данные тела могут идти, пока хост ещё не вернул ответ. Отгружаемый
`WasiHttp::execute` этого не даёт — `convert::race_send_with_body` дожидается
и `send`, и записи тела целиком (кроме раннего отказа `send`). Измерено на
живом хосте `wasmtime` (host-специфичное поведение, `wasi:http` его не
фиксирует): ответ существовал на сервере уже к t≈0.10s, а вызывающая сторона
видела его к t≈2.00s, когда дописалось тело; для тела без конца — не увидела
бы никогда.

Ограничение снимается **внутри `http-ng-wasi`, не трогая `Transport`.**
`Transport::execute` возвращает `http::Response<Self::Body>`, а `Self::Body` —
`http_ng_wasi::Body`, тип этого же крейта: недописанная футура записи
проносится в него и доопрашивается из `poll_frame`, отказ передачи становится
терминальной ошибкой тела. Финальное ревью ветки реализовало это как
proof-of-concept — около сорока строк, один новый вариант `Inner`, сигнатура
`Transport::execute` не тронута — и померило на том же госте и сервере:
ветка как есть висит до убийства на 25s, вариант с футурой в `Body` отдаёт
`RESPONSE_HEAD_RECEIVED status=200 OK` за 0.094s. Приём не новый: тот же
doc-комментарий `convert::resolve_send` предлагает ровно его для *другой*
отброшенной футуры (`transmitted`).

Отложено не из-за seam, а из-за трёх реальных цен, которые придётся
заплатить: (1) гвард необъявленных трейлеров не может отработать до возврата
`execute` — имена трейлеров известны только когда тело кончилось, так что
гвард переезжает в `Body` и становится терминальной ошибкой тела; (2) политика
`resolve_send` «ответ, пришедший поверх провалившейся записи тела, — не успех»
из ошибки уровня `execute` становится ошибкой уровня тела, то есть слабее;
(3) вызывающая сторона, которая никогда не читает тело ответа, никогда и не
дописывает тело запроса — это присуще дуплексу без `spawn` и требует
документирования. Работа вертикали 2, целиком внутри `http-ng-wasi`.

Дизайн: [`docs/superpowers/specs/2026-08-05-http-ng-design.md`](docs/superpowers/specs/2026-08-05-http-ng-design.md).
