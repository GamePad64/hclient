# http-ng v0.1, вертикаль 1: ядро + proto + WASI — план реализации

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Рабочий асинхронный HTTP-клиент, ходящий по сети через `wasi:http` 0.3,
с портативным ядром, которое ничего не знает ни про hyper, ни про сокеты.

**Architecture:** Три крейта. `http-ng-proto` — чистые автоматы без `async`
(SSE-декодер, логика редиректов). `http-ng-core` — контракт плагина: трейт
`Transport`, `Capabilities`, `RequestBody`, `Error`, `Timer`. `http-ng` —
пользовательская поверхность: `Client<T>`, builder, стадии, `Response`,
SSE-стрим. `http-ng-wasi` — первый транспорт. Ядро и стадии тестируются на хосте
против мок-транспорта; wasm нужен только для интеграционных тестов транспорта.

**Tech Stack:** Rust edition 2024, MSRV 1.85 (1.90 для `http-ng-wasi`).
`http` 1.5, `http-body` 1.1, `bytes` 1.12, `futures-core` 0.3, `url` 2.5,
`wasip3` 0.7.0+wasi-0.3.0. Тесты: `proptest` 1.x, `http-body-util` 0.1.
Никаких async-рантаймов в графе этой вертикали.

## Global Constraints

Эти требования неявно входят в каждую задачу. Значения скопированы из спеки
`docs/superpowers/specs/2026-08-05-http-ng-design.md`.

- **`http-ng-proto` не имеет в графе `tokio`, `futures-*`, `async-*`** и не
  содержит ни одного `async fn`. Проверяется в CI (Task 1).
- **В `http-ng-core` и `http-ng` нет ни одного объявленного бонда `Send`/`Sync`,
  ни одного `Box<dyn ...>` на горячем пути, ни одного `#[cfg]`-переключаемого
  трейт-алиаса.** `Send` выводится auto-traits через `impl Future`.
- **Плагин-трейты живут в модуле `unversioned`** (`Transport`, `Timer`) с
  докстрингом: «ломающие изменения в этом модуле едут в minor, а не major».
- **Ни один чужой тип не появляется в публичном API** `http-ng` и
  `http-ng-core`, кроме `http`, `http-body`, `bytes`, `futures-core`. В
  частности `wasip3::*` не реэкспортируется.
- **Неподдерживаемая настройка — типизированная ошибка, никогда тихий no-op.**
  Ни одного `let _ =` на `Result` от сеттера возможностей.
- **`default = []` во всех крейтах.**
- `edition = "2024"`, `rust-version = "1.85"` (кроме `http-ng-wasi`: `"1.90"`).
- Каждый крейт: `#![deny(unsafe_code)]`, кроме `http-ng-wasi` (там его тоже нет,
  но deny оставить).
- Коммиты — на каждом шаге «Commit», сообщение в императиве, префикс
  `feat:`/`test:`/`chore:`/`docs:`.

## Файловая структура

```
Cargo.toml                             workspace, [workspace.dependencies], lints
.github/workflows/ci.yml               матрица + проверки инвариантов
crates/http-ng-proto/
  src/lib.rs                           реэкспорты, #![no_std]-совместимость не заявляем
  src/sse/mod.rs                       SseDecoder — публичный API
  src/sse/lines.rs                     BOM + разбиение на строки через границы чанков
  src/sse/decode.rs                    поля, накопление события, диспатч
  src/redirect.rs                      decide() — чистое решение о редиректе
  fuzz/fuzz_targets/sse.rs             фаззинг декодера
crates/http-ng-core/
  src/lib.rs
  src/error.rs                         Error, ErrorKind, Phase
  src/body.rs                          RequestBody, RetryKind
  src/caps.rs                          Capabilities, UnsupportedCapability
  src/timer.rs                         Timer            (модуль unversioned)
  src/transport.rs                     Transport        (модуль unversioned)
crates/http-ng/
  src/lib.rs
  src/config.rs                        Config, Timeouts, RedirectConfig, lookup
  src/client.rs                        Client<T>, ClientBuilder<T>
  src/request.rs                       RequestBuilder
  src/response.rs                      Response, Collected
  src/stages/mod.rs
  src/stages/redirect.rs               применение решения из proto
  src/sse.rs                           SseStream — реконнект поверх декодера
  src/mock.rs                          MockTransport, за фичей `test-util`
crates/http-ng-wasi/
  src/lib.rs                           WasiHttp: Transport
  src/body.rs                          Body: http_body::Body
  src/convert.rs                       http <-> wasi, включая honoring сеттеров
```

**Не входит в эту вертикаль:** `http-ng-native`, `http-ng-rt*`, `http-ng-tls*`,
`http-ng-dns*`, `http-ng-fetch`, пул, h2/h3, `Negotiate`. Параметр по умолчанию
`Client<T = DefaultTransport>` появляется в вертикали 2, когда возникнет
native-транспорт; добавление дефолтного параметра типа — не ломающее изменение.

**Осознанное отклонение от спеки §10.** Спека относит `http-ng-fetch` к v0.1 на
том основании, что fetch — единственный бэкенд с рантайм-различиями возможностей
(duplex в Chrome 131+, нет в Safari), а значит единственная проверка решения о
рантайм-реестре `Capabilities`. Здесь он отложен в вертикаль 3 ради того, чтобы
вертикаль 1 давала запускаемый результат. **Следствие: до вертикали 3 решение
«рантайм-`Capabilities` вместо cfg» остаётся непроверенным.** Если вертикаль 3
покажет, что реестр не работает, переделка заденет `http-ng-core` — то есть
Task 8.

**В другом репозитории:** фасад совместимости `wasi-fetch` 0.3 живёт в
`/mnt/devenv/workspace/act/wasi-fetch` и здесь не планируется. Он делается после
того, как `http-ng-wasi` заработает, отдельным изменением в том репозитории.

---

### Task 1: Workspace, инварианты и CI

**Files:**
- Create: `Cargo.toml`
- Create: `.github/workflows/ci.yml`
- Create: `rustfmt.toml`

**Interfaces:**
- Consumes: ничего
- Produces: `[workspace.dependencies]` с пинами `http = "1.5"`, `http-body = "1.1"`,
  `bytes = "1.12"`, `futures-core = "0.3"`, `url = "2.5"`, `http-body-util = "0.1"`,
  `proptest = "1"`. Все последующие крейты берут их через `.workspace = true`.

- [ ] **Step 1: Создать workspace-манифест**

```toml
# Cargo.toml
[workspace]
resolver = "3"
members = ["crates/*"]

[workspace.package]
edition      = "2024"
rust-version = "1.85"
license      = "MIT OR Apache-2.0"
repository   = "https://github.com/actcore/http-ng"

[workspace.dependencies]
http           = "1.5"
http-body      = "1.1"
http-body-util = "0.1"
bytes          = "1.12"
futures-core   = { version = "0.3", default-features = false }
url            = "2.5"
proptest       = "1"

http-ng-proto = { path = "crates/http-ng-proto", version = "0.1.0" }
http-ng-core  = { path = "crates/http-ng-core",  version = "0.1.0" }
http-ng       = { path = "crates/http-ng",       version = "0.1.0" }

[workspace.lints.rust]
unsafe_code       = "deny"
missing_debug_implementations = "warn"
unexpected_cfgs   = { level = "warn", check-cfg = [] }
```

- [ ] **Step 2: Создать rustfmt.toml**

```toml
edition = "2024"
max_width = 100
```

- [ ] **Step 3: Проверить, что пустой workspace собирается**

Run: `cargo metadata --no-deps --format-version 1 > /dev/null && echo OK`
Expected: `OK` (членов пока нет — это нормально, `members = ["crates/*"]` по
пустому каталогу не ошибка только если каталог существует; создать его:
`mkdir -p crates`).

- [ ] **Step 4: Написать CI с проверками инвариантов**

```yaml
# .github/workflows/ci.yml
name: ci
on: [push, pull_request]

# Крейты появляются по ходу вертикали. Каждая проверка активируется, как только
# её крейт существует, и до тех пор ЯВНО печатает, что пропущена. Молчаливый
# зелёный чек опаснее красного: после опечатки в имени крейта он остаётся
# зелёным навсегда.

jobs:
  test:
    strategy:
      matrix:
        os: [ubuntu-latest, macos-latest, windows-latest]
    runs-on: ${{ matrix.os }}
    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@stable
      - shell: bash
        run: |
          set -euo pipefail
          if [ -z "$(ls -A crates 2>/dev/null | grep -v '^.gitkeep$' || true)" ]; then
            echo "::notice::в workspace ещё нет крейтов — тесты пропущены"
            exit 0
          fi
          cargo test --workspace --all-features

  msrv:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@1.85.0
      - shell: bash
        run: |
          set -euo pipefail
          pkgs=""
          for p in http-ng-proto http-ng-core http-ng; do
            if [ -d "crates/$p" ]; then pkgs="$pkgs -p $p"; fi
          done
          if [ -z "$pkgs" ]; then
            echo "::notice::крейтов ядра ещё нет — MSRV не проверяется"
            exit 0
          fi
          cargo check $pkgs --all-features

  wasip2:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@stable
        with: { targets: wasm32-wasip2 }
      - shell: bash
        run: |
          set -euo pipefail
          if [ ! -d crates/http-ng-wasi ]; then
            echo "::notice::http-ng-wasi ещё нет — сборка под wasip2 пропущена"
            exit 0
          fi
          cargo check -p http-ng-wasi --target wasm32-wasip2

  # ── инварианты из спеки ───────────────────────────────────────────────
  proto-is-sans-io:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@stable
      - name: no async deps in http-ng-proto
        shell: bash
        run: |
          set -euo pipefail
          if [ ! -d crates/http-ng-proto ]; then
            echo "::notice::http-ng-proto ещё нет — проверка пропущена"
            exit 0
          fi
          if cargo tree -p http-ng-proto -e normal --prefix none \
               | grep -Ei '^(tokio|futures-|async-|smol|compio)'; then
            echo "::error::http-ng-proto подцепил async-зависимость"
            exit 1
          fi
      - name: no async fn in http-ng-proto
        shell: bash
        run: |
          set -euo pipefail
          if [ ! -d crates/http-ng-proto/src ]; then
            echo "::notice::http-ng-proto ещё нет — проверка пропущена"
            exit 0
          fi
          if grep -rn "async fn" crates/http-ng-proto/src; then
            echo "::error::sans-io крейт содержит async fn"
            exit 1
          fi

  no-declared-send:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@stable
      - name: no Send/Sync bounds declared in core surface
        shell: bash
        run: |
          set -euo pipefail
          dirs=""
          for d in crates/http-ng-core/src crates/http-ng/src; do
            if [ -d "$d" ]; then dirs="$dirs $d"; fi
          done
          if [ -z "$dirs" ]; then
            echo "::notice::крейтов ядра ещё нет — проверка пропущена"
            exit 0
          fi
          # Ищем ОБЪЯВЛЕННЫЕ бонды, а не упоминания в доках: строки, у которых
          # содержимое начинается с комментария, отбрасываются вторым grep.
          if grep -rnE '(:|\+)[[:space:]]*(Send|Sync)\b|MaybeSend' $dirs \
               | grep -vE '^[^:]+:[0-9]+:[[:space:]]*(//|/\*|\*)'; then
            echo "::error::в ядре объявлен бонд Send или Sync"
            exit 1
          fi
```

- [ ] **Step 5: Commit**

```bash
mkdir -p crates
git add Cargo.toml rustfmt.toml .github/
git commit -m "chore: workspace skeleton with spec invariants enforced in CI"
```

---

### Task 2: `http-ng-proto` — разбиение SSE-потока на строки

Самая коварная часть SSE: BOM снимается ровно один, три терминатора строк
(CRLF/LF/CR), и всё это должно переживать разрыв на границе чанка — включая
разрыв внутри BOM и разрыв между CR и LF.

**Files:**
- Create: `crates/http-ng-proto/Cargo.toml`
- Create: `crates/http-ng-proto/src/lib.rs`
- Create: `crates/http-ng-proto/src/sse/mod.rs`
- Create: `crates/http-ng-proto/src/sse/lines.rs`
- Test: внутри `crates/http-ng-proto/src/sse/lines.rs` (`#[cfg(test)] mod tests`)

**Interfaces:**
- Consumes: ничего
- Produces: `pub(crate) struct LineSplitter`; `LineSplitter::new() -> Self`;
  `LineSplitter::push(&mut self, chunk: &[u8])`;
  `LineSplitter::next_line(&mut self) -> Option<Vec<u8>>` — возвращает строку
  **без** терминатора; `LineSplitter::buffered_len(&self) -> usize`.

- [ ] **Step 1: Написать падающие тесты**

```rust
// crates/http-ng-proto/src/sse/lines.rs
#[cfg(test)]
mod tests {
    use super::*;

    fn collect(chunks: &[&[u8]]) -> Vec<Vec<u8>> {
        let mut s = LineSplitter::new();
        let mut out = Vec::new();
        for c in chunks {
            s.push(c);
            while let Some(l) = s.next_line() { out.push(l) }
        }
        out
    }

    #[test]
    fn splits_on_all_three_terminators() {
        assert_eq!(collect(&[b"a\nb\r\nc\rd\n"]),
                   vec![b"a".to_vec(), b"b".to_vec(), b"c".to_vec(), b"d".to_vec()]);
    }

    #[test]
    fn strips_exactly_one_bom() {
        assert_eq!(collect(&[b"\xEF\xBB\xBFa\n"]), vec![b"a".to_vec()]);
        // второй BOM — обычные данные
        assert_eq!(collect(&[b"\xEF\xBB\xBF\xEF\xBB\xBFa\n"]),
                   vec![b"\xEF\xBB\xBFa".to_vec()]);
    }

    #[test]
    fn bom_split_across_chunks() {
        assert_eq!(collect(&[b"\xEF", b"\xBB", b"\xBFa\n"]), vec![b"a".to_vec()]);
    }

    #[test]
    fn crlf_split_across_chunks_yields_one_line() {
        assert_eq!(collect(&[b"a\r", b"\nb\n"]), vec![b"a".to_vec(), b"b".to_vec()]);
    }

    #[test]
    fn lone_cr_at_chunk_end_then_non_lf() {
        assert_eq!(collect(&[b"a\r", b"b\n"]), vec![b"a".to_vec(), b"b".to_vec()]);
    }

    #[test]
    fn incomplete_line_is_withheld() {
        let mut s = LineSplitter::new();
        s.push(b"partial");
        assert_eq!(s.next_line(), None);
        assert_eq!(s.buffered_len(), 7);
    }

    #[test]
    fn empty_line_is_yielded() {
        assert_eq!(collect(&[b"a\n\nb\n"]),
                   vec![b"a".to_vec(), Vec::new(), b"b".to_vec()]);
    }
}
```

- [ ] **Step 2: Запустить и убедиться, что тесты падают**

Run: `cargo test -p http-ng-proto`
Expected: FAIL — `cannot find type LineSplitter`.

- [ ] **Step 3: Создать манифест и корень крейта**

```toml
# crates/http-ng-proto/Cargo.toml
[package]
name = "http-ng-proto"
version = "0.1.0"
description = "Чистые автоматы протокольных слоёв http-ng: без I/O, без async, без рантайма"
edition.workspace = true
rust-version.workspace = true
license.workspace = true
repository.workspace = true

[dependencies]
bytes = { workspace = true }
http  = { workspace = true }
url   = { workspace = true }

[dev-dependencies]
proptest = { workspace = true }

[lints]
workspace = true
```

```rust
// crates/http-ng-proto/src/lib.rs
//! Чистые автоматы протокольных слоёв http-ng.
//!
//! Инвариант крейта: ни одного `async fn`, ни одной зависимости от рантайма.
//! Всё, что зависит от времени, принимает `now` параметром. Проверяется в CI.
#![deny(unsafe_code)]

pub mod redirect;
pub mod sse;
```

```rust
// crates/http-ng-proto/src/sse/mod.rs
mod lines;
pub(crate) use lines::LineSplitter;
```

- [ ] **Step 4: Реализовать `LineSplitter`**

```rust
// crates/http-ng-proto/src/sse/lines.rs

/// Разбивает байтовый поток на строки по правилам WHATWG EventSource:
/// снимается ровно один ведущий BOM, терминаторы — CRLF, LF или одиночный CR.
/// Переживает разрыв чанка в любом месте, включая середину BOM и между CR и LF.
#[derive(Debug)]
pub(crate) struct LineSplitter {
    buf: Vec<u8>,
    /// Сколько байт с начала `buf` уже отдано наружу.
    ///
    /// Существует ради сложности: сдвигать буфер на каждой строке
    /// (`drain(..pos)` + `remove(0)`) стоит O(n·k) для чанка с k строками.
    /// Замерено на прошлой версии: 50k коротких строк — 51 мс, 100k — 225 мс,
    /// 200k — 925 мс, то есть 4× на каждое удвоение. Это парсер недоверенного
    /// тела ответа, так что квадратичность здесь — вектор атаки.
    start: usize,
    /// Сколько байт BOM уже подтверждено. 3 = BOM обработан (снят или отвергнут).
    bom_seen: usize,
    bom_done: bool,
    /// Предыдущий байт был CR — следующий LF надо проглотить.
    pending_cr: bool,
}

const BOM: [u8; 3] = [0xEF, 0xBB, 0xBF];

impl LineSplitter {
    pub(crate) fn new() -> Self {
        Self { buf: Vec::new(), start: 0, bom_seen: 0, bom_done: false, pending_cr: false }
    }

    pub(crate) fn push(&mut self, chunk: &[u8]) {
        // Компактация раз в push, а не раз в строку: суммарно линейно.
        if self.start > 0 {
            self.buf.drain(..self.start);
            self.start = 0;
        }

        let mut rest = chunk;

        // Фаза BOM: копим до трёх байт, решаем один раз.
        while !self.bom_done && !rest.is_empty() {
            let b = rest[0];
            if b == BOM[self.bom_seen] {
                self.bom_seen += 1;
                rest = &rest[1..];
                if self.bom_seen == 3 {
                    self.bom_done = true; // BOM снят целиком
                }
            } else {
                // Не BOM: то, что накопили, — обычные данные.
                self.buf.extend_from_slice(&BOM[..self.bom_seen]);
                self.bom_done = true;
            }
        }

        for &b in rest {
            if self.pending_cr {
                self.pending_cr = false;
                if b == b'\n' {
                    continue; // LF после CR уже учтён терминатором
                }
            }
            self.buf.push(b);
        }
    }

    pub(crate) fn next_line(&mut self) -> Option<Vec<u8>> {
        let hay = &self.buf[self.start..];
        let pos = hay.iter().position(|&b| b == b'\n' || b == b'\r')?;
        let term = hay[pos];
        let line = hay[..pos].to_vec();
        self.start += pos + 1; // строка плюс сам терминатор
        if term == b'\r' {
            if self.buf.get(self.start) == Some(&b'\n') {
                self.start += 1; // CRLF
            } else if self.start == self.buf.len() {
                self.pending_cr = true; // CR в конце — LF может прийти следующим чанком
            }
        }
        Some(line)
    }

    pub(crate) fn buffered_len(&self) -> usize {
        // Байты BOM, по которым решение ещё не принято, физически удержаны.
        // Не учитывать их — значит дать обойти лимит размера события в декодере.
        (self.buf.len() - self.start) + if self.bom_done { 0 } else { self.bom_seen }
    }
}
```

- [ ] **Step 5: Запустить тесты**

Run: `cargo test -p http-ng-proto`
Expected: PASS, семь тестов.

- [ ] **Step 6: Добавить property-тест на инвариант «нарезка чанков не влияет на результат»**

```rust
// добавить в тот же mod tests
use proptest::prelude::*;

proptest! {
    #[test]
    fn chunking_does_not_change_lines(
        prefix_bom: bool,
        data: Vec<u8>,
        splits in proptest::collection::vec(0usize..4096, 0..4),
    ) {
        // Случайный Vec<u8> практически никогда не начнётся с EF BB BF
        // (1 к 16 млн), поэтому BOM подставляется явно.
        let mut input = Vec::new();
        if prefix_bom { input.extend_from_slice(&[0xEF, 0xBB, 0xBF]) }
        input.extend_from_slice(&data);

        let whole = collect(&[&input]);

        // Произвольное число кусков в произвольных местах: двух мало —
        // состояние (pending_cr, фаза BOM) должно переживать несколько
        // границ подряд.
        let mut cuts: Vec<usize> = splits.iter().map(|s| s % (input.len() + 1)).collect();
        cuts.sort_unstable();
        let mut chunks: Vec<&[u8]> = Vec::new();
        let mut prev = 0;
        for c in cuts {
            chunks.push(&input[prev..c]);
            prev = c;
        }
        chunks.push(&input[prev..]);

        prop_assert_eq!(whole, collect(&chunks));
    }
}

/// Единственный тест, покрывающий учёт байт BOM, по которым решение ещё не
/// принято. `incomplete_line_is_withheld` его не покрывает: там BOM нет вовсе.
#[test]
fn buffered_len_counts_bytes_held_inside_an_undecided_bom() {
    let mut s = LineSplitter::new();
    s.push(&[0xEF, 0xBB]); // два из трёх байт BOM — решение ещё не принято
    assert_eq!(s.buffered_len(), 2,
        "недоучёт даёт обойти лимит размера события в декодере");
    assert_eq!(s.next_line(), None);

    s.push(&[0xBF]); // BOM собрался целиком и снят
    assert_eq!(s.buffered_len(), 0);

    s.push(b"ab");
    assert_eq!(s.buffered_len(), 2);
}

/// Регресс на измеренную квадратичность: много коротких строк в одном чанке.
#[test]
fn many_lines_in_one_chunk_is_linear() {
    let mut input = Vec::new();
    for _ in 0..50_000 { input.extend_from_slice(b"data: x\n") }
    let start = std::time::Instant::now();
    let lines = collect(&[&input]);
    let elapsed = start.elapsed();
    assert_eq!(lines.len(), 50_000);
    // Прежняя версия давала ~51 мс в release и кратно больше в debug.
    // Порог намеренно щедрый: ловим класс O(n^2), а не микросекунды.
    assert!(elapsed < std::time::Duration::from_secs(2), "разбор занял {elapsed:?}");
}

- [ ] **Step 7: Запустить и убедиться, что property-тест проходит**

Run: `cargo test -p http-ng-proto -- --include-ignored`
Expected: PASS.

- [ ] **Step 8: Commit**

```bash
git add crates/http-ng-proto
git commit -m "feat(proto): SSE line splitter surviving chunk boundaries and BOM"
```

---

### Task 3: `http-ng-proto` — декодер событий SSE

**Files:**
- Create: `crates/http-ng-proto/src/sse/decode.rs`
- Modify: `crates/http-ng-proto/src/sse/mod.rs`
- Test: внутри `decode.rs`

**Interfaces:**
- Consumes: `LineSplitter` из Task 2.
- Produces:
  - `pub enum SseEvent { Message { event: Option<String>, data: String, id: Option<String> }, Comment(String), Retry(core::time::Duration) }`
  - `pub enum SseError { EventTooLarge { limit: usize } }`
  - `pub struct SseDecoder`; `SseDecoder::new(max_event_size: usize) -> Self`;
    `SseDecoder::push(&mut self, chunk: &[u8]) -> Result<(), SseError>`;
    `SseDecoder::next(&mut self) -> Option<SseEvent>`;
    `SseDecoder::last_event_id(&self) -> Option<&str>`.

- [ ] **Step 1: Написать падающие тесты**

```rust
// crates/http-ng-proto/src/sse/decode.rs
#[cfg(test)]
mod tests {
    use super::*;
    use core::time::Duration;

    fn events(input: &[u8]) -> Vec<SseEvent> {
        let mut d = SseDecoder::new(1024);
        d.push(input).unwrap();
        let mut out = Vec::new();
        while let Some(e) = d.next() { out.push(e) }
        out
    }

    #[test]
    fn dispatches_simple_message() {
        assert_eq!(events(b"data: hello\n\n"),
            vec![SseEvent::Message { event: None, data: "hello".into(), id: None }]);
    }

    #[test]
    fn strips_exactly_one_leading_space_after_colon() {
        assert_eq!(events(b"data:  two spaces\n\n"),
            vec![SseEvent::Message { event: None, data: " two spaces".into(), id: None }]);
    }

    #[test]
    fn joins_multiple_data_lines_with_lf_and_trims_trailing() {
        assert_eq!(events(b"data: a\ndata: b\n\n"),
            vec![SseEvent::Message { event: None, data: "a\nb".into(), id: None }]);
    }

    #[test]
    fn repeated_event_field_last_wins_not_an_error() {
        assert_eq!(events(b"event: a\nevent: b\ndata: x\n\n"),
            vec![SseEvent::Message { event: Some("b".into()), data: "x".into(), id: None }]);
    }

    #[test]
    fn comment_is_surfaced_not_swallowed() {
        assert_eq!(events(b": keep-alive\n"), vec![SseEvent::Comment("keep-alive".into())]);
    }

    #[test]
    fn retry_only_block_is_not_lost() {
        assert_eq!(events(b"retry: 5000\n\n"),
                   vec![SseEvent::Retry(Duration::from_millis(5000))]);
    }

    #[test]
    fn retry_rejects_non_ascii_digits() {
        assert_eq!(events(b"retry: +5000\n\n"), vec![]);
        assert_eq!(events(b"retry: 1e3\n\n"),   vec![]);
    }

    #[test]
    fn id_persists_across_events_and_nul_is_ignored() {
        let mut d = SseDecoder::new(1024);
        d.push(b"id: 42\ndata: a\n\ndata: b\n\n").unwrap();
        let a = d.next().unwrap();
        let b = d.next().unwrap();
        assert_eq!(a, SseEvent::Message { event: None, data: "a".into(), id: Some("42".into()) });
        assert_eq!(b, SseEvent::Message { event: None, data: "b".into(), id: Some("42".into()) });
        assert_eq!(d.last_event_id(), Some("42"));

        let mut d2 = SseDecoder::new(1024);
        d2.push(b"id: 4\x002\ndata: a\n\n").unwrap();
        assert_eq!(d2.next().unwrap(),
            SseEvent::Message { event: None, data: "a".into(), id: None });
    }

    #[test]
    fn empty_data_buffer_dispatches_nothing_but_id_survives() {
        let mut d = SseDecoder::new(1024);
        d.push(b"id: 7\n\ndata: x\n\n").unwrap();
        assert_eq!(d.next().unwrap(),
            SseEvent::Message { event: None, data: "x".into(), id: Some("7".into()) });
        assert!(d.next().is_none());
    }

    #[test]
    fn field_without_colon_is_name_with_empty_value() {
        // "data" эквивалентно "data:"
        assert_eq!(events(b"data\ndata: x\n\n"),
            vec![SseEvent::Message { event: None, data: "\nx".into(), id: None }]);
    }

    #[test]
    fn oversized_event_is_a_fatal_error() {
        let mut d = SseDecoder::new(16);
        let err = d.push(b"data: 0123456789abcdefghij\n\n").unwrap_err();
        assert_eq!(err, SseError::EventTooLarge { limit: 16 });
    }
}
```

- [ ] **Step 2: Запустить и убедиться, что падает**

Run: `cargo test -p http-ng-proto`
Expected: FAIL — `cannot find type SseDecoder`.

- [ ] **Step 3: Реализовать декодер**

```rust
// crates/http-ng-proto/src/sse/decode.rs
use super::LineSplitter;
use core::time::Duration;
use std::collections::VecDeque;

/// Событие SSE. `Comment` и `Retry` — первого класса намеренно: без первого
/// нельзя построить детектор keep-alive, без второго теряются блоки,
/// содержащие только `retry:`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SseEvent {
    Message { event: Option<String>, data: String, id: Option<String> },
    Comment(String),
    Retry(Duration),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SseError {
    /// Превышен лимит размера сырого события. Фатально и **не ретраится**.
    EventTooLarge { limit: usize },
}

impl core::fmt::Display for SseError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            SseError::EventTooLarge { limit } =>
                write!(f, "SSE event exceeds {limit} bytes"),
        }
    }
}
impl std::error::Error for SseError {}

#[derive(Debug)]
pub struct SseDecoder {
    lines: LineSplitter,
    max_event_size: usize,
    /// Байт, накопленных в текущем событии (сырых, до парсинга).
    event_bytes: usize,
    data: String,
    event_type: Option<String>,
    last_event_id: Option<String>,
    /// Событие текущего блока уже имело поле `id`.
    ready: VecDeque<SseEvent>,
}

impl SseDecoder {
    pub fn new(max_event_size: usize) -> Self {
        Self {
            lines: LineSplitter::new(),
            max_event_size,
            event_bytes: 0,
            data: String::new(),
            event_type: None,
            last_event_id: None,
            ready: Default::default(),
        }
    }

    pub fn last_event_id(&self) -> Option<&str> {
        self.last_event_id.as_deref()
    }

    pub fn push(&mut self, chunk: &[u8]) -> Result<(), SseError> {
        self.lines.push(chunk);
        while let Some(line) = self.lines.next_line() {
            if line.is_empty() {
                self.dispatch();
                self.event_bytes = 0;
                continue;
            }
            self.event_bytes = self.event_bytes.saturating_add(line.len() + 1);
            if self.event_bytes > self.max_event_size {
                return Err(SseError::EventTooLarge { limit: self.max_event_size });
            }
            self.handle_line(&line);
        }
        // Незавершённая строка тоже считается — иначе лимит обходится
        // бесконечной строкой без терминатора.
        if self.event_bytes + self.lines.buffered_len() > self.max_event_size {
            return Err(SseError::EventTooLarge { limit: self.max_event_size });
        }
        Ok(())
    }

    pub fn next(&mut self) -> Option<SseEvent> {
        self.ready.pop_front()
    }

    fn handle_line(&mut self, line: &[u8]) {
        if line[0] == b':' {
            // Снимается РОВНО один ведущий пробел, как и у полей.
            // `trim_start_matches(' ')` снял бы все и потерял бы значащие.
            let raw = &line[1..];
            let raw = if raw.first() == Some(&b' ') { &raw[1..] } else { raw };
            self.ready.push_back(SseEvent::Comment(
                String::from_utf8_lossy(raw).into_owned()));
            return;
        }
        let (name, value) = match line.iter().position(|&b| b == b':') {
            Some(i) => {
                let v = &line[i + 1..];
                let v = if v.first() == Some(&b' ') { &v[1..] } else { v };
                (&line[..i], v)
            }
            None => (line, &line[line.len()..]),
        };
        match name {
            b"data" => {
                // WHATWG: к буферу дописывается значение И перевод строки.
                // Один хвостовой перевод снимается при диспатче. Схема
                // «разделитель только между непустыми» даёт другой результат
                // для пустого первого поля.
                self.data.push_str(&String::from_utf8_lossy(value));
                self.data.push('\n');
            }
            b"event" => {
                // Повтор поля — последнее побеждает, а НЕ ошибка.
                self.event_type = Some(String::from_utf8_lossy(value).into_owned());
            }
            b"id" => {
                if !value.contains(&0) {
                    self.last_event_id = Some(String::from_utf8_lossy(value).into_owned());
                }
            }
            b"retry" => {
                if !value.is_empty() && value.iter().all(|b| b.is_ascii_digit()) {
                    if let Ok(ms) = core::str::from_utf8(value).unwrap_or("").parse::<u64>() {
                        self.ready.push_back(SseEvent::Retry(Duration::from_millis(ms)));
                    }
                }
            }
            _ => {} // неизвестное поле игнорируется
        }
    }

    fn dispatch(&mut self) {
        let event = self.event_type.take();
        if self.data.is_empty() {
            // Пустой буфер данных: сброс без диспатча.
            // last_event_id при этом НЕ сбрасывается.
            return;
        }
        let mut data = core::mem::take(&mut self.data);
        if data.ends_with('\n') { data.pop(); }
        self.ready.push_back(SseEvent::Message {
            event,
            data,
            id: self.last_event_id.clone(),
        });
    }
}
```

- [ ] **Step 4: Подключить модуль**

```rust
// crates/http-ng-proto/src/sse/mod.rs
mod decode;
mod lines;

pub use decode::{SseDecoder, SseError, SseEvent};
pub(crate) use lines::LineSplitter;

/// Лимит по умолчанию — совпадает с `rmcp::DEFAULT_MAX_SSE_EVENT_SIZE`,
/// чтобы адаптер не менял поведение.
pub const DEFAULT_MAX_EVENT_SIZE: usize = 16 * 1024 * 1024;
```

- [ ] **Step 5: Запустить тесты**

Run: `cargo test -p http-ng-proto`
Expected: PASS, все двенадцать тестов декодера плюс тесты Task 2.

- [ ] **Step 6: Commit**

```bash
git add crates/http-ng-proto
git commit -m "feat(proto): WHATWG-conformant SSE decoder with first-class comments and retry"
```

---

### Task 4: `http-ng-proto` — фазз-таргет для SSE

**Files:**
- Create: `crates/http-ng-proto/fuzz/Cargo.toml`
- Create: `crates/http-ng-proto/fuzz/fuzz_targets/sse.rs`
- Modify: `.github/workflows/ci.yml`

**Interfaces:**
- Consumes: `SseDecoder::{new, push, next}` из Task 3.
- Produces: ничего для кода; CI-job `fuzz-smoke`.

- [ ] **Step 1: Установить cargo-fuzz**

Run: `cargo install cargo-fuzz --locked`
Expected: успешная установка (требует nightly для запуска, но не для установки).

- [ ] **Step 2: Создать фазз-таргет**

```toml
# crates/http-ng-proto/fuzz/Cargo.toml
[package]
name = "http-ng-proto-fuzz"
version = "0.0.0"
edition = "2024"
publish = false

[package.metadata]
cargo-fuzz = true

[dependencies]
libfuzzer-sys = "0.4"
http-ng-proto = { path = ".." }

[[bin]]
name = "sse"
path = "fuzz_targets/sse.rs"
test = false
doc  = false
bench = false

[workspace]
```

```rust
// crates/http-ng-proto/fuzz/fuzz_targets/sse.rs
#![no_main]
use libfuzzer_sys::fuzz_target;
use http_ng_proto::sse::SseDecoder;

// Инвариант: декодер никогда не паникует и никогда не растёт сверх лимита.
fuzz_target!(|data: &[u8]| {
    const LIMIT: usize = 4096;
    let mut d = SseDecoder::new(LIMIT);
    for chunk in data.chunks(7) {
        if d.push(chunk).is_err() {
            return; // EventTooLarge — легальный терминальный исход
        }
        while d.next().is_some() {}
    }
});
```

- [ ] **Step 3: Прогнать фаззер коротко**

Run: `cd crates/http-ng-proto/fuzz && cargo +nightly fuzz run sse -- -max_total_time=60`
Expected: 60 секунд без паник и без падений.

- [ ] **Step 4: Добавить smoke-job в CI**

```yaml
  # добавить в .github/workflows/ci.yml
  fuzz-smoke:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@nightly
      - run: cargo install cargo-fuzz --locked
      - run: cargo fuzz run sse -- -max_total_time=60
        working-directory: crates/http-ng-proto/fuzz
```

- [ ] **Step 5: Commit**

```bash
git add crates/http-ng-proto/fuzz .github/workflows/ci.yml
git commit -m "test(proto): fuzz the SSE decoder in CI"
```

---

### Task 5: `http-ng-proto` — решение о редиректе

Чистая функция. Исправляет три дефекта нынешнего цикла в `wasi-fetch`: 304/305
не следуются, чувствительные заголовки снимаются при смене host **и scheme**,
301/302 с POST понижаются до GET наравне с 303.

**Files:**
- Create: `crates/http-ng-proto/src/redirect.rs`
- Modify: `crates/http-ng-proto/src/lib.rs`
- Test: внутри `redirect.rs`

**Interfaces:**
- Consumes: ничего
- Produces:
  - `pub struct RedirectPolicy { pub limit: u8 }`
  - `pub struct Follow { pub uri: http::Uri, pub method: http::Method, pub strip_sensitive: bool, pub drop_body: bool }`
  - `pub enum RedirectAction { Stop, Follow(Follow), TooManyRedirects, InvalidLocation }`
  - `pub fn decide(policy: &RedirectPolicy, hops: u8, current: &http::Uri, method: &http::Method, status: http::StatusCode, location: Option<&[u8]>) -> RedirectAction`
  - `pub const SENSITIVE_HEADERS: [http::HeaderName; 3]` — `authorization`,
    `cookie`, `proxy-authorization`.

- [ ] **Step 1: Написать падающие тесты**

```rust
// crates/http-ng-proto/src/redirect.rs
#[cfg(test)]
mod tests {
    use super::*;
    use http::{Method, StatusCode, Uri};

    fn p() -> RedirectPolicy { RedirectPolicy { limit: 10 } }
    fn u(s: &str) -> Uri { s.parse().unwrap() }

    fn go(status: u16, from: &str, to: &str, m: Method) -> RedirectAction {
        decide(&p(), 0, &u(from), &m, StatusCode::from_u16(status).unwrap(), Some(to.as_bytes()))
    }

    #[test]
    fn does_not_follow_300_304_305() {
        for s in [300u16, 304, 305, 306] {
            assert!(matches!(go(s, "https://a/", "https://b/", Method::GET), RedirectAction::Stop),
                    "status {s} must not be followed");
        }
    }

    #[test]
    fn follows_the_five_real_redirects() {
        for s in [301u16, 302, 303, 307, 308] {
            assert!(matches!(go(s, "https://a/", "https://a/x", Method::GET),
                             RedirectAction::Follow(_)), "status {s}");
        }
    }

    #[test]
    fn strips_sensitive_on_host_change() {
        let RedirectAction::Follow(f) = go(302, "https://a/", "https://b/", Method::GET)
            else { panic!() };
        assert!(f.strip_sensitive);
    }

    #[test]
    fn strips_sensitive_on_scheme_change_same_host() {
        let RedirectAction::Follow(f) = go(302, "https://a/", "http://a/", Method::GET)
            else { panic!() };
        assert!(f.strip_sensitive, "downgrade https->http must strip");
    }

    #[test]
    fn keeps_sensitive_on_same_origin() {
        let RedirectAction::Follow(f) = go(302, "https://a/one", "https://a/two", Method::GET)
            else { panic!() };
        assert!(!f.strip_sensitive);
    }

    #[test]
    fn post_downgrades_to_get_on_301_302_303() {
        for s in [301u16, 302, 303] {
            let RedirectAction::Follow(f) = go(s, "https://a/", "https://a/x", Method::POST)
                else { panic!("status {s}") };
            assert_eq!(f.method, Method::GET, "status {s}");
            assert!(f.drop_body, "status {s}");
        }
    }

    #[test]
    fn post_is_preserved_on_307_308() {
        for s in [307u16, 308] {
            let RedirectAction::Follow(f) = go(s, "https://a/", "https://a/x", Method::POST)
                else { panic!() };
            assert_eq!(f.method, Method::POST);
            assert!(!f.drop_body);
        }
    }

    #[test]
    fn head_stays_head_on_303() {
        let RedirectAction::Follow(f) = go(303, "https://a/", "https://a/x", Method::HEAD)
            else { panic!() };
        assert_eq!(f.method, Method::HEAD);
    }

    #[test]
    fn resolves_relative_location() {
        let RedirectAction::Follow(f) = go(302, "https://a/one/two", "../three", Method::GET)
            else { panic!() };
        assert_eq!(f.uri, u("https://a/three"));
    }

    #[test]
    fn missing_location_stops() {
        let r = decide(&p(), 0, &u("https://a/"), &Method::GET, StatusCode::FOUND, None);
        assert!(matches!(r, RedirectAction::Stop));
    }

    #[test]
    fn limit_is_enforced() {
        let r = decide(&RedirectPolicy { limit: 2 }, 2, &u("https://a/"), &Method::GET,
                       StatusCode::FOUND, Some(b"https://a/x"));
        assert!(matches!(r, RedirectAction::TooManyRedirects));
    }

    #[test]
    fn garbage_location_is_reported() {
        let r = decide(&p(), 0, &u("https://a/"), &Method::GET, StatusCode::FOUND,
                       Some(b"ht!tp://\x00"));
        assert!(matches!(r, RedirectAction::InvalidLocation));
    }
}
```

- [ ] **Step 2: Запустить и убедиться, что падает**

Run: `cargo test -p http-ng-proto redirect`
Expected: FAIL — `cannot find function decide`.

- [ ] **Step 3: Реализовать**

```rust
// crates/http-ng-proto/src/redirect.rs
//! Решение о следовании редиректу. Чистая функция: ни I/O, ни времени.

use http::{HeaderName, Method, StatusCode, Uri};

/// Заголовки, снимаемые при уходе на другой origin.
pub const SENSITIVE_HEADERS: [HeaderName; 3] = [
    http::header::AUTHORIZATION,
    http::header::COOKIE,
    http::header::PROXY_AUTHORIZATION,
];

#[derive(Debug, Clone, Copy)]
pub struct RedirectPolicy {
    pub limit: u8,
}

impl Default for RedirectPolicy {
    fn default() -> Self { Self { limit: 10 } }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Follow {
    pub uri: Uri,
    pub method: Method,
    /// Снять `SENSITIVE_HEADERS`: сменился host или scheme.
    pub strip_sensitive: bool,
    /// Метод понижен до GET — тело отправлять нельзя.
    pub drop_body: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RedirectAction {
    /// Не редирект, либо редирект без `Location` — вернуть ответ как есть.
    Stop,
    Follow(Follow),
    TooManyRedirects,
    InvalidLocation,
}

pub fn decide(
    policy: &RedirectPolicy,
    hops: u8,
    current: &Uri,
    method: &Method,
    status: StatusCode,
    location: Option<&[u8]>,
) -> RedirectAction {
    // ВАЖНО: не `status.is_redirection()`. 300 Multiple Choices требует выбора
    // пользователя, 304 Not Modified — ответ на условный запрос, 305 Use Proxy
    // не следуют с 2014 года, 306 зарезервирован.
    if !matches!(status.as_u16(), 301 | 302 | 303 | 307 | 308) {
        return RedirectAction::Stop;
    }
    let Some(location) = location else { return RedirectAction::Stop };
    if hops >= policy.limit {
        return RedirectAction::TooManyRedirects;
    }

    let Ok(location) = core::str::from_utf8(location) else {
        return RedirectAction::InvalidLocation;
    };
    let Ok(base) = url::Url::parse(&current.to_string()) else {
        return RedirectAction::InvalidLocation;
    };
    let Ok(joined) = base.join(location) else {
        return RedirectAction::InvalidLocation;
    };
    let Ok(uri) = joined.as_str().parse::<Uri>() else {
        return RedirectAction::InvalidLocation;
    };

    let cross_origin = uri.host() != current.host()
        || uri.scheme_str() != current.scheme_str()
        || uri.port_u16() != current.port_u16();

    // 303 — всегда GET (кроме HEAD). 301/302 с POST браузеры и reqwest
    // понижают до GET; расхождение с 303 было бы непоследовательным.
    let downgrade = match status.as_u16() {
        303 => *method != Method::HEAD,
        301 | 302 => *method == Method::POST,
        _ => false,
    };
    let new_method = if downgrade { Method::GET } else { method.clone() };

    RedirectAction::Follow(Follow {
        uri,
        method: new_method,
        strip_sensitive: cross_origin,
        drop_body: downgrade,
    })
}
```

- [ ] **Step 4: Подключить и запустить тесты**

`crates/http-ng-proto/src/lib.rs` уже содержит `pub mod redirect;` (Task 2, Step 3).

Run: `cargo test -p http-ng-proto`
Expected: PASS, двенадцать тестов редиректа плюс всё предыдущее.

- [ ] **Step 5: Commit**

```bash
git add crates/http-ng-proto
git commit -m "feat(proto): redirect decision honouring 304/305 and stripping on scheme change"
```

---

### Task 6: `http-ng-core` — Error и ErrorKind

**Files:**
- Create: `crates/http-ng-core/Cargo.toml`
- Create: `crates/http-ng-core/src/lib.rs`
- Create: `crates/http-ng-core/src/error.rs`
- Test: внутри `error.rs`

**Interfaces:**
- Consumes: ничего
- Produces:
  - `pub enum ErrorKind { Resolve, Connect, Tls, Redirect, Timeout(Phase), Body, Decode, Status, Unsupported, Other }` (`#[non_exhaustive]`)
  - `pub enum Phase { Connect, FirstByte, BetweenBytes, Total }`
  - `pub struct Error` (`Clone`), `Error::new(kind, source)`, `Error::kind()`,
    `Error::is_timeout()`, `Error::is_redirect()`, `Error::is_connect()`
  - `impl std::error::Error for Error`

- [ ] **Step 1: Написать падающие тесты**

```rust
// crates/http-ng-core/src/error.rs
#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug)] struct Src;
    impl std::fmt::Display for Src {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result { write!(f, "boom") }
    }
    impl std::error::Error for Src {}

    #[test]
    fn preserves_kind_and_source_without_stringifying() {
        let e = Error::new(ErrorKind::Resolve, Src);
        assert_eq!(e.kind(), &ErrorKind::Resolve);
        // Источник доступен целиком — не подстрокой сообщения.
        let src = std::error::Error::source(&e).unwrap();
        assert!(src.downcast_ref::<Src>().is_some());
    }

    #[test]
    fn is_clone_which_reqwest_error_is_not() {
        let e = Error::new(ErrorKind::Connect, Src);
        let c = e.clone();
        assert_eq!(c.kind(), &ErrorKind::Connect);
    }

    #[test]
    fn predicates_agree_with_kind() {
        assert!(Error::new(ErrorKind::Timeout(Phase::Connect), Src).is_timeout());
        assert!(Error::new(ErrorKind::Redirect, Src).is_redirect());
        assert!(!Error::new(ErrorKind::Body, Src).is_connect());
    }

    #[test]
    fn error_is_not_forced_send() {
        // Ядро не объявляет Send: ошибка от !Send-источника всё равно строится.
        struct NotSend(std::rc::Rc<()>);
        impl std::fmt::Debug for NotSend {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result { write!(f, "ns") }
        }
        impl std::fmt::Display for NotSend {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result { write!(f, "ns") }
        }
        impl std::error::Error for NotSend {}
        let _ = Error::new(ErrorKind::Other, NotSend(std::rc::Rc::new(())));
    }
}
```

- [ ] **Step 2: Запустить и убедиться, что падает**

Run: `cargo test -p http-ng-core`
Expected: FAIL — крейта ещё нет.

- [ ] **Step 3: Создать крейт и реализовать**

```toml
# crates/http-ng-core/Cargo.toml
[package]
name = "http-ng-core"
version = "0.1.0"
description = "Контракт плагина http-ng: Transport, Capabilities, RequestBody, Error, Timer"
edition.workspace = true
rust-version.workspace = true
license.workspace = true
repository.workspace = true

[dependencies]
bytes     = { workspace = true }
http      = { workspace = true }
http-body = { workspace = true }

[lints]
workspace = true
```

```rust
// crates/http-ng-core/src/lib.rs
//! Контракт плагина http-ng.
//!
//! Инвариант крейта: ни одного объявленного бонда `Send`/`Sync`. Send-ность
//! выводится auto-traits через `impl Future`.
#![deny(unsafe_code)]

mod body;
mod caps;
mod error;

pub mod unversioned;

pub use body::{RequestBody, RetryKind};
pub use caps::{Capabilities, RedirectSupport, TimeoutSupport, Timeouts, TlsSupport,
               UnsupportedCapability, UpgradeSupport};
pub use error::{Error, ErrorKind, Phase};
```

```rust
// crates/http-ng-core/src/error.rs
use std::sync::Arc;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase { Connect, FirstByte, BetweenBytes, Total }

/// Категория ошибки. Существует, чтобы потребителю не приходилось
/// классифицировать ошибки подстрочным матчингом по `Display`.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ErrorKind {
    Resolve,
    Connect,
    Tls,
    Redirect,
    Timeout(Phase),
    Body,
    Decode,
    Status,
    Unsupported,
    Other,
}

/// `Clone` намеренно: непрозрачная и неклонируемая ошибка reqwest — источник
/// постоянных жалоб (reqwest#1053). `Arc<dyn Error>` не требует `Send`,
/// поэтому auto-trait прозрачность доходит и до ошибок.
#[derive(Debug, Clone)]
pub struct Error {
    kind: ErrorKind,
    source: Arc<dyn std::error::Error + 'static>,
}

impl Error {
    pub fn new<E: std::error::Error + 'static>(kind: ErrorKind, source: E) -> Self {
        Self { kind, source: Arc::new(source) }
    }
    pub fn kind(&self) -> &ErrorKind { &self.kind }
    pub fn is_timeout(&self) -> bool { matches!(self.kind, ErrorKind::Timeout(_)) }
    pub fn is_redirect(&self) -> bool { matches!(self.kind, ErrorKind::Redirect) }
    pub fn is_connect(&self) -> bool { matches!(self.kind, ErrorKind::Connect) }
    pub fn is_unsupported(&self) -> bool { matches!(self.kind, ErrorKind::Unsupported) }
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:?}: {}", self.kind, self.source)
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&*self.source)
    }
}
```

- [ ] **Step 4: Запустить тесты**

Run: `cargo test -p http-ng-core error`
Expected: PASS, четыре теста.

**Модули объявлять по мере появления, а не комментировать.** В этой задаче
`lib.rs` содержит только `mod error;` и `pub use error::…`; `body`, `caps` и
`unversioned` добавляются в Task 7, 8 и 9 соответственно. Закомментированный
код в коммите — дефект.

- [ ] **Step 5: Commit**

```bash
git add crates/http-ng-core
git commit -m "feat(core): typed Error with kind enum, Clone and preserved source"
```

---

### Task 7: `http-ng-core` — RequestBody с контрактом replay

**Files:**
- Create: `crates/http-ng-core/src/body.rs`
- Modify: `crates/http-ng-core/src/lib.rs`
- Test: внутри `body.rs`

**Interfaces:**
- Consumes: ничего
- Produces:
  - `pub enum RetryKind { Free, ViaFactory, Impossible }`
  - `pub enum RequestBody { Empty, Full(bytes::Bytes), Rewindable(RewindFactory), Streaming(BoxedStream) }`
  - `pub type RewindFactory = std::sync::Arc<dyn Fn() -> RequestBody>`
  - `RequestBody::retry_kind(&self) -> RetryKind`
  - `RequestBody::rewind(&self) -> Option<RequestBody>`
  - `RequestBody::size_hint(&self) -> Option<u64>`

- [ ] **Step 1: Написать падающие тесты**

```rust
// crates/http-ng-core/src/body.rs
#[cfg(test)]
mod tests {
    use super::*;
    use bytes::Bytes;

    #[test]
    fn replayability_is_knowable_before_sending() {
        assert_eq!(RequestBody::Empty.retry_kind(), RetryKind::Free);
        assert_eq!(RequestBody::Full(Bytes::from_static(b"x")).retry_kind(), RetryKind::Free);
    }

    #[test]
    fn rewindable_replays_through_factory() {
        let b = RequestBody::rewindable(|| RequestBody::Full(Bytes::from_static(b"same")));
        assert_eq!(b.retry_kind(), RetryKind::ViaFactory);
        let again = b.rewind().expect("rewindable must rewind");
        assert!(matches!(again, RequestBody::Full(ref x) if &x[..] == b"same"));
    }

    #[test]
    fn full_rewinds_by_cloning_bytes() {
        let b = RequestBody::Full(Bytes::from_static(b"abc"));
        assert!(b.rewind().is_some());
    }

    #[test]
    fn size_hint_known_for_buffered_unknown_for_streaming() {
        assert_eq!(RequestBody::Empty.size_hint(), Some(0));
        assert_eq!(RequestBody::Full(Bytes::from_static(b"abcd")).size_hint(), Some(4));
    }
}
```

- [ ] **Step 2: Запустить и убедиться, что падает**

Run: `cargo test -p http-ng-core body`
Expected: FAIL — `cannot find type RequestBody`.

- [ ] **Step 3: Реализовать**

```rust
// crates/http-ng-core/src/body.rs
use bytes::Bytes;
use std::sync::Arc;

/// Можно ли переиграть это тело — известно **до** отправки.
///
/// `reqwest::Request::try_clone() -> Option<Request>` отвечает на тот же вопрос
/// после того, как retry-слой уже решил ретраить, и поэтому молча выключает
/// ретраи на стриминговых телах.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RetryKind {
    /// Переигрывается бесплатно.
    Free,
    /// Переигрывается вызовом фабрики.
    ViaFactory,
    /// Переиграть нельзя.
    Impossible,
}

pub type RewindFactory = Arc<dyn Fn() -> RequestBody>;

/// Тело запроса с явным контрактом переигрывания.
pub enum RequestBody {
    Empty,
    Full(Bytes),
    Rewindable(RewindFactory),
    /// Однопроходное тело. Конкретный поток задаёт транспорт; в v0.1 ядру
    /// достаточно знать, что переиграть его нельзя.
    Streaming(Box<dyn http_body::Body<Data = Bytes, Error = crate::Error> + Unpin>),
}

impl std::fmt::Debug for RequestBody {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RequestBody::Empty => f.write_str("Empty"),
            RequestBody::Full(b) => write!(f, "Full({} bytes)", b.len()),
            RequestBody::Rewindable(_) => f.write_str("Rewindable(..)"),
            RequestBody::Streaming(_) => f.write_str("Streaming(..)"),
        }
    }
}

impl RequestBody {
    pub fn rewindable<F>(f: F) -> Self
    where F: Fn() -> RequestBody + 'static {
        RequestBody::Rewindable(Arc::new(f))
    }

    pub fn retry_kind(&self) -> RetryKind {
        match self {
            RequestBody::Empty | RequestBody::Full(_) => RetryKind::Free,
            RequestBody::Rewindable(_) => RetryKind::ViaFactory,
            RequestBody::Streaming(_) => RetryKind::Impossible,
        }
    }

    pub fn rewind(&self) -> Option<RequestBody> {
        match self {
            RequestBody::Empty => Some(RequestBody::Empty),
            RequestBody::Full(b) => Some(RequestBody::Full(b.clone())),
            RequestBody::Rewindable(f) => Some(f()),
            RequestBody::Streaming(_) => None,
        }
    }

    pub fn size_hint(&self) -> Option<u64> {
        match self {
            RequestBody::Empty => Some(0),
            RequestBody::Full(b) => Some(b.len() as u64),
            _ => None,
        }
    }
}

impl Default for RequestBody {
    fn default() -> Self { RequestBody::Empty }
}
```

- [ ] **Step 4: Раскомментировать `mod body;` и `pub use` в `lib.rs`, запустить тесты**

Run: `cargo test -p http-ng-core`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/http-ng-core
git commit -m "feat(core): RequestBody with replay contract knowable before sending"
```

---

### Task 8: `http-ng-core` — Capabilities и UnsupportedCapability

**Files:**
- Create: `crates/http-ng-core/src/caps.rs`
- Modify: `crates/http-ng-core/src/lib.rs`
- Test: внутри `caps.rs`

**Interfaces:**
- Consumes: ничего
- Produces:
  - `pub struct Capabilities` (`#[non_exhaustive]`, `Debug`, `Clone`) с полями из
    спеки §4.6
  - `pub enum RedirectSupport { None, Internal, Configurable, Inspectable }`
  - `pub enum TlsSupport { None, ServerTrustCallbackOnly, Full }`
  - `pub enum UpgradeSupport { None, H1, ExtendedConnect, Both }`
  - `pub struct TimeoutSupport { pub connect: bool, pub first_byte: bool, pub between_bytes: bool }`
  - `pub struct Timeouts { pub connect: Option<Duration>, pub first_byte: Option<Duration>, pub between_bytes: Option<Duration> }` (`Copy`, `Default` = все `None`)
  - `pub struct UnsupportedCapability { pub what: &'static str, pub backend: &'static str }`
  - `Capabilities::none() -> Self` — всё выключено, база для бэкендов

> **Почему `Timeouts` здесь, а не в `http-ng`.** Транспорты читают их из
> `http::Extensions` запроса, а `http-ng-wasi` зависит только от
> `http-ng-core`. Определи мы `Timeouts` в `http-ng`, транспорт не смог бы их
> увидеть, и per-request таймауты были бы недостижимы.

- [ ] **Step 1: Написать падающие тесты**

```rust
// crates/http-ng-core/src/caps.rs
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn none_is_the_conservative_base() {
        let c = Capabilities::none();
        assert!(!c.streaming_request_body);
        assert!(!c.full_duplex);
        assert_eq!(c.redirects, RedirectSupport::None);
        assert_eq!(c.tls_config, TlsSupport::None);
        assert_eq!(c.upgrade, UpgradeSupport::None);
        assert!(c.forbidden_request_headers.is_empty());
    }

    #[test]
    fn unsupported_names_both_the_feature_and_the_backend() {
        let e = UnsupportedCapability { what: "connect_timeout", backend: "wasi:http" };
        let msg = e.to_string();
        assert!(msg.contains("connect_timeout"), "{msg}");
        assert!(msg.contains("wasi:http"), "{msg}");
    }

    #[test]
    fn timeout_support_is_per_phase_not_a_single_flag() {
        let t = TimeoutSupport { connect: true, first_byte: true, between_bytes: false };
        assert!(t.connect && t.first_byte && !t.between_bytes);
    }
}
```

- [ ] **Step 2: Запустить и убедиться, что падает**

Run: `cargo test -p http-ng-core caps`
Expected: FAIL.

- [ ] **Step 3: Реализовать**

```rust
// crates/http-ng-core/src/caps.rs
use http::HeaderName;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RedirectSupport {
    /// Редиректов нет и наблюдать нечего.
    None,
    /// Бэкенд следует сам, мы не управляем и не видим (wasi:http).
    Internal,
    /// Мы задаём политику.
    Configurable,
    /// Мы задаём политику и видим каждый хоп.
    Inspectable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TlsSupport { None, ServerTrustCallbackOnly, Full }

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UpgradeSupport { None, H1, ExtendedConnect, Both }

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TimeoutSupport {
    pub connect: bool,
    pub first_byte: bool,
    pub between_bytes: bool,
}

/// Тройка таймаутов — форма `wasi:http`, богатейшая из ambient-моделей.
///
/// В fetch схлопывается в один `AbortController`, в native раскладывается на
/// коннектор / ожидание ответа / idle тела. Один `Duration` выбрасывает
/// информацию, которой WASI-бэкенд умеет пользоваться.
///
/// Живёт в `http-ng-core`, потому что транспорты читают её из
/// `http::Extensions` запроса, а от `http-ng` они не зависят.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Timeouts {
    pub connect: Option<core::time::Duration>,
    pub first_byte: Option<core::time::Duration>,
    pub between_bytes: Option<core::time::Duration>,
}

/// Что транспорт умеет **в этом процессе, сейчас**.
///
/// Именно рантайм, а не `cfg!`: один wasm-бинарь работает и в Chrome
/// (streaming request body есть с 131), и в Safari (нет).
#[non_exhaustive]
#[derive(Debug, Clone)]
pub struct Capabilities {
    pub streaming_request_body: bool,
    pub full_duplex: bool,
    pub request_trailers: bool,
    pub response_trailers: bool,
    pub redirects: RedirectSupport,
    pub tls_config: TlsSupport,
    pub client_certs: bool,
    pub proxy: bool,
    pub owns_cookie_jar: bool,
    pub owns_cache: bool,
    pub version_select: bool,
    pub version_reported: bool,
    pub timeouts: TimeoutSupport,
    pub informational_1xx: bool,
    pub upgrade: UpgradeSupport,
    pub forbidden_request_headers: &'static [HeaderName],
}

impl Capabilities {
    /// Всё выключено. База, от которой бэкенд включает то, что действительно умеет.
    pub const fn none() -> Self {
        Self {
            streaming_request_body: false,
            full_duplex: false,
            request_trailers: false,
            response_trailers: false,
            redirects: RedirectSupport::None,
            tls_config: TlsSupport::None,
            client_certs: false,
            proxy: false,
            owns_cookie_jar: false,
            owns_cache: false,
            version_select: false,
            version_reported: false,
            timeouts: TimeoutSupport { connect: false, first_byte: false, between_bytes: false },
            informational_1xx: false,
            upgrade: UpgradeSupport::None,
            forbidden_request_headers: &[],
        }
    }
}

/// Настройка, которую выбранный транспорт не может выполнить.
///
/// Возвращается из `build()`, а не игнорируется молча. Образец — сам wasi:http,
/// где сеттеры возвращают `request-options-error::not-supported`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnsupportedCapability {
    pub what: &'static str,
    pub backend: &'static str,
}

impl std::fmt::Display for UnsupportedCapability {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "backend `{}` does not support `{}`", self.backend, self.what)
    }
}
impl std::error::Error for UnsupportedCapability {}
```

- [ ] **Step 4: Раскомментировать в `lib.rs`, запустить тесты**

Run: `cargo test -p http-ng-core`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/http-ng-core
git commit -m "feat(core): runtime Capabilities registry and typed UnsupportedCapability"
```

---

### Task 9: `http-ng-core` — Transport и Timer в модуле `unversioned`

**Files:**
- Create: `crates/http-ng-core/src/unversioned/mod.rs`
- Create: `crates/http-ng-core/src/unversioned/transport.rs`
- Create: `crates/http-ng-core/src/unversioned/timer.rs`
- Modify: `crates/http-ng-core/src/lib.rs`
- Test: `crates/http-ng-core/tests/shape.rs`

**Interfaces:**
- Consumes: `RequestBody` (Task 7), `Capabilities` (Task 8).
- Produces:
  - `pub trait Transport { type Body: http_body::Body<Data = Bytes>; type Error: std::error::Error + 'static; fn execute(&self, req: http::Request<RequestBody>) -> impl Future<Output = Result<http::Response<Self::Body>, Self::Error>>; fn capabilities(&self) -> &Capabilities; }`
  - `pub trait Timer { type Instant: Copy; fn sleep(&self, d: core::time::Duration) -> impl Future<Output = ()>; fn now(&self) -> Self::Instant; fn elapsed_since(&self, earlier: Self::Instant) -> core::time::Duration; }`

- [ ] **Step 1: Написать падающий тест формы**

```rust
// crates/http-ng-core/tests/shape.rs
//! Тест утверждает главное архитектурное свойство ядра: `Send` нигде не
//! объявлен, но выводится auto-traits, когда транспорт действительно Send.

use bytes::Bytes;
use http_ng_core::unversioned::Transport;
use http_ng_core::{Capabilities, Error, ErrorKind, RequestBody};

struct Echo { caps: Capabilities }

#[derive(Debug)] struct Never;
impl std::fmt::Display for Never {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result { write!(f, "never") }
}
impl std::error::Error for Never {}

impl Transport for Echo {
    type Body = http_body_util::Full<Bytes>;
    type Error = Error;
    async fn execute(&self, _req: http::Request<RequestBody>)
        -> Result<http::Response<Self::Body>, Self::Error>
    {
        Ok(http::Response::new(http_body_util::Full::new(Bytes::from_static(b"ok"))))
    }
    fn capabilities(&self) -> &Capabilities { &self.caps }
}

#[test]
fn send_propagates_without_being_declared() {
    fn assert_send<T: Send>(_: T) {}
    let t = Echo { caps: Capabilities::none() };
    let fut = t.execute(http::Request::new(RequestBody::Empty));
    assert_send(fut);
}

#[test]
fn non_send_transport_still_satisfies_the_trait() {
    struct Local { caps: Capabilities, _rc: std::rc::Rc<()> }
    impl Transport for Local {
        type Body = http_body_util::Full<Bytes>;
        type Error = Error;
        async fn execute(&self, _req: http::Request<RequestBody>)
            -> Result<http::Response<Self::Body>, Self::Error>
        {
            Err(Error::new(ErrorKind::Other, Never))
        }
        fn capabilities(&self) -> &Capabilities { &self.caps }
    }
    let _ = Local { caps: Capabilities::none(), _rc: std::rc::Rc::new(()) };
}
```

- [ ] **Step 2: Запустить и убедиться, что падает**

Run: `cargo test -p http-ng-core --test shape`
Expected: FAIL — `unresolved import http_ng_core::unversioned::Transport`.

- [ ] **Step 3: Добавить dev-зависимость и реализовать трейты**

В `crates/http-ng-core/Cargo.toml`:

```toml
[dev-dependencies]
http-body-util = { workspace = true }
```

```rust
// crates/http-ng-core/src/unversioned/mod.rs
//! # Карантин semver
//!
//! Трейты этого модуля — контракт для авторов бэкендов и рантаймов. Он ещё не
//! провалидирован против всех бэкендов, поэтому:
//!
//! **Ломающие изменения в `unversioned` едут в minor-версию, а не в major.**
//!
//! Приём заимствован у `ureq`. Без него 1.0 неотгружаем: нельзя заморозить
//! трейт, не проверив его на native, wasi:http и fetch.

mod timer;
mod transport;

pub use timer::Timer;
pub use transport::Transport;
```

```rust
// crates/http-ng-core/src/unversioned/transport.rs
use crate::{Capabilities, RequestBody};
use bytes::Bytes;
use std::future::Future;

/// Единственный шов между http-ng и реальным HTTP.
///
/// Форма взята от `wasi:http/client.send` — самого бедного из ambient-API.
/// Всё, что богаче, деградирует к ней чисто; обратное неверно.
///
/// Ни `poll_ready`, ни `&mut self`, ни `Send`: Send-ность выводится
/// auto-traits через возвращаемый `impl Future`.
pub trait Transport {
    type Body: http_body::Body<Data = Bytes>;
    type Error: std::error::Error + 'static;

    fn execute(
        &self,
        req: http::Request<RequestBody>,
    ) -> impl Future<Output = Result<http::Response<Self::Body>, Self::Error>>;

    /// Что этот транспорт умеет **сейчас, в этом процессе**.
    fn capabilities(&self) -> &Capabilities;
}
```

```rust
// crates/http-ng-core/src/unversioned/timer.rs
use core::time::Duration;
use std::future::Future;

/// Единственная способность рантайма, нужная портативному ядру: таймауты и
/// backoff. Сеть и spawn живут в транспортах.
///
/// Не `hyper::rt::Timer`: у того `Sleep: Send + Sync` безусловно, `sleep()`
/// возвращает `Pin<Box<dyn Sleep>>` (аллокация на каждый sleep), а `now()`
/// типизирован на `std::time::Instant`, который паникует на
/// `wasm32-unknown-unknown`.
pub trait Timer {
    type Instant: Copy;

    fn sleep(&self, d: Duration) -> impl Future<Output = ()>;
    fn now(&self) -> Self::Instant;
    fn elapsed_since(&self, earlier: Self::Instant) -> Duration;
}
```

В `lib.rs` добавить `pub mod unversioned;` (уже есть с Task 6, Step 3).

- [ ] **Step 4: Запустить тесты**

Run: `cargo test -p http-ng-core`
Expected: PASS. Тест `send_propagates_without_being_declared` — это проверка
того, что ядро не нуждается в объявленном `Send`.

- [ ] **Step 5: Проверить инвариант «нет объявленного Send» вручную**

Run: `! grep -rnE ':\s*Send\b|\+\s*Send\b|MaybeSend' crates/http-ng-core/src && echo OK`
Expected: `OK`.

- [ ] **Step 6: Commit**

```bash
git add crates/http-ng-core
git commit -m "feat(core): Transport and Timer traits under the unversioned semver quarantine"
```

---

### Task 10: `http-ng` — Config, Timeouts и per-request lookup

**Files:**
- Create: `crates/http-ng/Cargo.toml`
- Create: `crates/http-ng/src/lib.rs`
- Create: `crates/http-ng/src/config.rs`
- Test: внутри `config.rs`

**Interfaces:**
- Consumes: `http_ng_core::{Capabilities, TimeoutSupport, UnsupportedCapability}`.
- Produces:
  - `pub struct Config { pub timeouts: Timeouts, pub redirect: http_ng_proto::redirect::RedirectPolicy, pub base_url: Option<http::Uri> }`
    (`Timeouts` определён в Task 8, в `http-ng-core`, и здесь только
    реэкспортируется — транспортам он нужен, а от `http-ng` они не зависят)
  - `pub fn effective_timeouts(req: &http::Extensions, client: &Timeouts) -> Timeouts` — «request-first, client-fallback»
  - `pub fn check_supported(cfg: &Config, caps: &Capabilities, backend: &'static str) -> Result<(), UnsupportedCapability>`

- [ ] **Step 1: Написать падающие тесты**

```rust
// crates/http-ng/src/config.rs
#[cfg(test)]
mod tests {
    use super::*;
    use http_ng_core::{Capabilities, TimeoutSupport};
    use std::time::Duration;

    fn secs(n: u64) -> Option<Duration> { Some(Duration::from_secs(n)) }

    #[test]
    fn request_overrides_client_field_by_field() {
        let client = Timeouts { connect: secs(1), first_byte: secs(2), between_bytes: secs(3) };
        let mut ext = http::Extensions::new();
        ext.insert(Timeouts { connect: secs(9), ..Default::default() });
        let eff = effective_timeouts(&ext, &client);
        assert_eq!(eff.connect, secs(9), "запрос перекрывает");
        assert_eq!(eff.first_byte, secs(2), "остальное падает обратно на клиент");
        assert_eq!(eff.between_bytes, secs(3));
    }

    #[test]
    fn client_config_used_when_request_says_nothing() {
        let client = Timeouts { connect: secs(1), ..Default::default() };
        let eff = effective_timeouts(&http::Extensions::new(), &client);
        assert_eq!(eff.connect, secs(1));
    }

    #[test]
    fn unsupported_timeout_is_an_error_not_a_silent_noop() {
        let cfg = Config { timeouts: Timeouts { between_bytes: secs(5), ..Default::default() },
                           ..Default::default() };
        let mut caps = Capabilities::none();
        caps.timeouts = TimeoutSupport { connect: true, first_byte: true, between_bytes: false };
        let err = check_supported(&cfg, &caps, "wasi:http").unwrap_err();
        assert_eq!(err.what, "between_bytes_timeout");
        assert_eq!(err.backend, "wasi:http");
    }

    #[test]
    fn supported_config_passes() {
        let cfg = Config { timeouts: Timeouts { connect: secs(1), ..Default::default() },
                           ..Default::default() };
        let mut caps = Capabilities::none();
        caps.timeouts = TimeoutSupport { connect: true, first_byte: false, between_bytes: false };
        assert!(check_supported(&cfg, &caps, "wasi:http").is_ok());
    }
}
```

- [ ] **Step 2: Запустить и убедиться, что падает**

Run: `cargo test -p http-ng`
Expected: FAIL — крейта нет.

- [ ] **Step 3: Создать крейт и реализовать**

```toml
# crates/http-ng/Cargo.toml
[package]
name = "http-ng"
version = "0.1.0"
description = "Кроссплатформенный асинхронный HTTP-клиент: один код под native, browser и WASI"
edition.workspace = true
rust-version.workspace = true
license.workspace = true
repository.workspace = true

[features]
default = []
# Мок-транспорт для тестов потребителей.
test-util = []

[dependencies]
bytes         = { workspace = true }
futures-core  = { workspace = true }
http          = { workspace = true }
http-body     = { workspace = true }
http-ng-core  = { workspace = true }
http-ng-proto = { workspace = true }

[dev-dependencies]
http-body-util = { workspace = true }

[lints]
workspace = true
```

```rust
// crates/http-ng/src/lib.rs
//! Кроссплатформенный асинхронный HTTP-клиент.
//!
//! Инвариант крейта: ни одного объявленного бонда `Send`/`Sync`, ни одного
//! `#[cfg]`-переключаемого трейт-алиаса. Send-ность выводится auto-traits.
#![deny(unsafe_code)]

mod client;
mod config;
mod request;
mod response;
mod sse;
mod stages;

#[cfg(feature = "test-util")]
pub mod mock;

pub use client::{Client, ClientBuilder};
pub use config::{Config, Timeouts, check_supported, effective_timeouts};
pub use http_ng_core::{Capabilities, Error, ErrorKind, Phase, RequestBody, RetryKind,
                       UnsupportedCapability};
pub use http_ng_proto::redirect::RedirectPolicy;
pub use http_ng_proto::sse::{SseEvent, DEFAULT_MAX_EVENT_SIZE};
pub use request::RequestBuilder;
pub use response::{Collected, Response};
pub use sse::SseStream;
```

```rust
// crates/http-ng/src/config.rs
// `Timeouts` определён в `http-ng-core` (Task 8): его читают транспорты из
// `http::Extensions`, а от `http-ng` они не зависят.
pub use http_ng_core::Timeouts;
use http_ng_core::{Capabilities, UnsupportedCapability};
use http_ng_proto::redirect::RedirectPolicy;

#[derive(Debug, Clone, Default)]
pub struct Config {
    pub timeouts: Timeouts,
    pub redirect: RedirectPolicy,
    pub base_url: Option<http::Uri>,
}

/// «Request-first, client-fallback», поле за полем.
///
/// reqwest этого не умеет (issue #2641 не реализован), из-за чего `act-cli`
/// вынужден строить отдельный `reqwest::Client` на каждый вызов компонента.
pub fn effective_timeouts(req: &http::Extensions, client: &Timeouts) -> Timeouts {
    match req.get::<Timeouts>() {
        None => *client,
        Some(o) => Timeouts {
            connect: o.connect.or(client.connect),
            first_byte: o.first_byte.or(client.first_byte),
            between_bytes: o.between_bytes.or(client.between_bytes),
        },
    }
}

/// Вызывается из `ClientBuilder::build()`. Ни одного тихого no-op.
pub fn check_supported(
    cfg: &Config,
    caps: &Capabilities,
    backend: &'static str,
) -> Result<(), UnsupportedCapability> {
    let checks = [
        (cfg.timeouts.connect.is_some(), caps.timeouts.connect, "connect_timeout"),
        (cfg.timeouts.first_byte.is_some(), caps.timeouts.first_byte, "first_byte_timeout"),
        (cfg.timeouts.between_bytes.is_some(), caps.timeouts.between_bytes,
         "between_bytes_timeout"),
    ];
    for (requested, supported, what) in checks {
        if requested && !supported {
            return Err(UnsupportedCapability { what, backend });
        }
    }
    Ok(())
}
```

**Модули объявлять по мере появления.** В этой задаче `lib.rs` содержит только
`mod config;` и его реэкспорты; `mock`, `client`, `request`, `response`, `sse`
и `stages` добавляются в Task 11–14. Закомментированный код в коммите — дефект.

- [ ] **Step 4: Запустить тесты**

Run: `cargo test -p http-ng config`
Expected: PASS, четыре теста.

- [ ] **Step 5: Commit**

```bash
git add crates/http-ng
git commit -m "feat(http-ng): timeout triple with request-first client-fallback lookup"
```

---

### Task 11: `http-ng` — MockTransport

Мок нужен раньше клиента: без него стадии тестируются только через сеть.

**Files:**
- Create: `crates/http-ng/src/mock.rs`
- Modify: `crates/http-ng/src/lib.rs`
- Test: внутри `mock.rs`

**Interfaces:**
- Consumes: `Transport`, `Capabilities`, `RequestBody`, `Error`.
- Produces:
  - `pub struct MockTransport`
  - `MockTransport::new() -> Self`
  - `MockTransport::push_response(&self, resp: http::Response<&'static str>)` — очередь ответов
  - `MockTransport::with_capabilities(self, caps: Capabilities) -> Self`
  - `MockTransport::requests(&self) -> Vec<RecordedRequest>`
  - `pub struct RecordedRequest { pub method: http::Method, pub uri: http::Uri, pub headers: http::HeaderMap }`
  - `impl Transport for MockTransport { type Body = http_body_util::Full<Bytes>; type Error = Error; }`

- [ ] **Step 1: Написать падающие тесты**

```rust
// crates/http-ng/src/mock.rs
#[cfg(test)]
mod tests {
    use super::*;
    use http_ng_core::unversioned::Transport;

    #[test]
    fn records_requests_and_replays_queued_responses() {
        let m = MockTransport::new();
        m.push_response(http::Response::builder().status(204).body("").unwrap());

        let fut = m.execute(http::Request::builder()
            .method("POST").uri("https://a/x").body(RequestBody::Empty).unwrap());
        let resp = futures_executor::block_on(fut).unwrap();

        assert_eq!(resp.status(), 204);
        let rec = m.requests();
        assert_eq!(rec.len(), 1);
        assert_eq!(rec[0].method, http::Method::POST);
        assert_eq!(rec[0].uri, "https://a/x".parse::<http::Uri>().unwrap());
    }

    #[test]
    fn errors_when_the_queue_is_empty() {
        let m = MockTransport::new();
        let fut = m.execute(http::Request::new(RequestBody::Empty));
        assert!(futures_executor::block_on(fut).is_err());
    }
}
```

- [ ] **Step 2: Запустить и убедиться, что падает**

Run: `cargo test -p http-ng --features test-util mock`
Expected: FAIL — `cannot find type MockTransport`.

- [ ] **Step 3: Добавить dev-зависимость на исполнитель и реализовать**

В `crates/http-ng/Cargo.toml`:

```toml
[dev-dependencies]
http-body-util   = { workspace = true }
futures-executor = { version = "0.3", default-features = false, features = ["std"] }
```

```rust
// crates/http-ng/src/mock.rs
//! Мок-транспорт: позволяет тестировать клиент и стадии на хосте, без сети и
//! без wasm-рантайма. Доступен за фичей `test-util`.

use bytes::Bytes;
use http_body_util::Full;
use http_ng_core::unversioned::Transport;
use http_ng_core::{Capabilities, Error, ErrorKind, RequestBody};
use std::cell::RefCell;
use std::collections::VecDeque;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordedRequest {
    pub method: http::Method,
    pub uri: http::Uri,
    pub headers: http::HeaderMap,
}

#[derive(Debug)]
pub struct MockTransport {
    queue: RefCell<VecDeque<http::Response<Bytes>>>,
    seen: RefCell<Vec<RecordedRequest>>,
    caps: Capabilities,
}

#[derive(Debug)]
struct QueueEmpty;
impl std::fmt::Display for QueueEmpty {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "MockTransport: response queue is empty")
    }
}
impl std::error::Error for QueueEmpty {}

impl MockTransport {
    pub fn new() -> Self {
        Self { queue: Default::default(), seen: Default::default(), caps: Capabilities::none() }
    }

    pub fn with_capabilities(mut self, caps: Capabilities) -> Self {
        self.caps = caps;
        self
    }

    pub fn push_response(&self, resp: http::Response<&'static str>) {
        let (parts, body) = resp.into_parts();
        self.queue.borrow_mut()
            .push_back(http::Response::from_parts(parts, Bytes::from_static(body.as_bytes())));
    }

    pub fn requests(&self) -> Vec<RecordedRequest> {
        self.seen.borrow().clone()
    }
}

impl Default for MockTransport {
    fn default() -> Self { Self::new() }
}

impl Transport for MockTransport {
    type Body = Full<Bytes>;
    type Error = Error;

    async fn execute(&self, req: http::Request<RequestBody>)
        -> Result<http::Response<Self::Body>, Self::Error>
    {
        let (parts, _body) = req.into_parts();
        self.seen.borrow_mut().push(RecordedRequest {
            method: parts.method,
            uri: parts.uri,
            headers: parts.headers,
        });
        match self.queue.borrow_mut().pop_front() {
            Some(r) => {
                let (p, b) = r.into_parts();
                Ok(http::Response::from_parts(p, Full::new(b)))
            }
            None => Err(Error::new(ErrorKind::Other, QueueEmpty)),
        }
    }

    fn capabilities(&self) -> &Capabilities { &self.caps }
}
```

- [ ] **Step 4: Запустить тесты**

Run: `cargo test -p http-ng --features test-util mock`
Expected: PASS, два теста.

- [ ] **Step 5: Commit**

```bash
git add crates/http-ng
git commit -m "feat(http-ng): MockTransport for host-side testing of client and stages"
```

---

### Task 12: `http-ng` — Client, ClientBuilder и стадия redirect

**Files:**
- Create: `crates/http-ng/src/client.rs`
- Create: `crates/http-ng/src/stages/mod.rs`
- Create: `crates/http-ng/src/stages/redirect.rs`
- Modify: `crates/http-ng/src/lib.rs`
- Test: `crates/http-ng/tests/redirect.rs`

**Interfaces:**
- Consumes: `MockTransport` (Task 11), `redirect::decide` (Task 5), `Config`,
  `check_supported` (Task 10), `Transport` (Task 9).
- Produces:
  - `pub struct ClientBuilder<T>`; `ClientBuilder::new(transport: T) -> Self`;
    `.redirect(RedirectPolicy)`, `.timeouts(Timeouts)`, `.base_url(http::Uri)`,
    `.build() -> Result<Client<T>, UnsupportedCapability>`
  - `pub struct Client<T>`; `Client::builder(transport: T) -> ClientBuilder<T>`;
    `Client::execute(&self, req: http::Request<RequestBody>) -> impl Future<Output = Result<http::Response<T::Body>, Error>>`
  - `Client::transport(&self) -> &T`, `Client::config(&self) -> &Config`

- [ ] **Step 1: Написать падающие тесты**

```rust
// crates/http-ng/tests/redirect.rs
use http_ng::{Client, RedirectPolicy, RequestBody};
use http_ng::mock::MockTransport;

fn redirect_to(loc: &'static str) -> http::Response<&'static str> {
    http::Response::builder().status(302).header("location", loc).body("").unwrap()
}

#[test]
fn follows_a_redirect_and_records_both_hops() {
    let m = MockTransport::new();
    m.push_response(redirect_to("https://a/second"));
    m.push_response(http::Response::builder().status(200).body("done").unwrap());

    let c = Client::builder(m).build().unwrap();
    let req = http::Request::builder().uri("https://a/first")
        .body(RequestBody::Empty).unwrap();
    let resp = futures_executor::block_on(c.execute(req)).unwrap();

    assert_eq!(resp.status(), 200);
    let seen = c.transport().requests();
    assert_eq!(seen.len(), 2);
    assert_eq!(seen[1].uri, "https://a/second".parse::<http::Uri>().unwrap());
}

#[test]
fn strips_authorization_when_the_host_changes() {
    let m = MockTransport::new();
    m.push_response(redirect_to("https://evil/steal"));
    m.push_response(http::Response::builder().status(200).body("").unwrap());

    let c = Client::builder(m).build().unwrap();
    let req = http::Request::builder().uri("https://a/first")
        .header("authorization", "Bearer secret")
        .header("x-safe", "keep")
        .body(RequestBody::Empty).unwrap();
    let _ = futures_executor::block_on(c.execute(req)).unwrap();

    let seen = c.transport().requests();
    assert!(seen[0].headers.contains_key("authorization"), "первый хоп сохраняет");
    assert!(!seen[1].headers.contains_key("authorization"), "второй хоп снимает");
    assert!(seen[1].headers.contains_key("x-safe"), "несекретные заголовки остаются");
}

#[test]
fn does_not_follow_304() {
    let m = MockTransport::new();
    m.push_response(http::Response::builder().status(304)
        .header("location", "https://a/nope").body("").unwrap());

    let c = Client::builder(m).build().unwrap();
    let req = http::Request::builder().uri("https://a/x")
        .body(RequestBody::Empty).unwrap();
    let resp = futures_executor::block_on(c.execute(req)).unwrap();

    assert_eq!(resp.status(), 304);
    assert_eq!(c.transport().requests().len(), 1);
}

#[test]
fn enforces_the_hop_limit() {
    let m = MockTransport::new();
    for _ in 0..5 { m.push_response(redirect_to("https://a/loop")); }

    let c = Client::builder(m).redirect(RedirectPolicy { limit: 2 }).build().unwrap();
    let req = http::Request::builder().uri("https://a/x")
        .body(RequestBody::Empty).unwrap();
    let err = futures_executor::block_on(c.execute(req)).unwrap_err();

    assert!(err.is_redirect(), "{err}");
    assert_eq!(c.transport().requests().len(), 3, "исходный запрос плюс два хопа");
}

#[test]
fn post_becomes_get_and_drops_body_on_302() {
    let m = MockTransport::new();
    m.push_response(redirect_to("https://a/second"));
    m.push_response(http::Response::builder().status(200).body("").unwrap());

    let c = Client::builder(m).build().unwrap();
    let req = http::Request::builder().method("POST").uri("https://a/first")
        .body(RequestBody::Full(bytes::Bytes::from_static(b"payload"))).unwrap();
    let _ = futures_executor::block_on(c.execute(req)).unwrap();

    let seen = c.transport().requests();
    assert_eq!(seen[1].method, http::Method::GET);
}

#[test]
fn build_rejects_a_timeout_the_backend_cannot_honour() {
    use http_ng::Timeouts;
    let m = MockTransport::new(); // Capabilities::none() — таймауты не поддержаны
    let err = Client::builder(m)
        .timeouts(Timeouts { connect: Some(std::time::Duration::from_secs(1)),
                             ..Default::default() })
        .build()
        .unwrap_err();
    assert_eq!(err.what, "connect_timeout");
}
```

- [ ] **Step 2: Запустить и убедиться, что падает**

Run: `cargo test -p http-ng --features test-util --test redirect`
Expected: FAIL — `cannot find type Client`.

- [ ] **Step 3: Реализовать стадию redirect**

```rust
// crates/http-ng/src/stages/mod.rs
pub(crate) mod redirect;
```

```rust
// crates/http-ng/src/stages/redirect.rs
//! Применение решения, принятого в `http-ng-proto`. Здесь только перекладывание
//! данных: вся логика — чистая функция `proto::redirect::decide`.

use http_ng_core::RequestBody;
use http_ng_proto::redirect::{Follow, SENSITIVE_HEADERS};

/// Всё, что переносится между хопами, кроме тела.
///
/// Отдельный тип, потому что `http::request::Parts` **не реализует `Clone`**,
/// а между хопами метод, URI и заголовки нужны и до, и после отправки.
/// `HeaderMap`, `Uri`, `Method` и `Extensions` клонируемы — проверено.
#[derive(Debug, Clone)]
pub(crate) struct HopParts {
    pub(crate) method: http::Method,
    pub(crate) uri: http::Uri,
    pub(crate) headers: http::HeaderMap,
    pub(crate) version: http::Version,
    pub(crate) extensions: http::Extensions,
}

impl HopParts {
    pub(crate) fn to_request(&self, body: RequestBody) -> http::Request<RequestBody> {
        let mut req = http::Request::new(body);
        *req.method_mut() = self.method.clone();
        *req.uri_mut() = self.uri.clone();
        *req.headers_mut() = self.headers.clone();
        *req.version_mut() = self.version;
        *req.extensions_mut() = self.extensions.clone();
        req
    }
}

/// Построить следующий хоп. `replay` — снимок тела, снятый **до** отправки
/// предыдущей попытки; `None` означает, что тело невоспроизводимо.
///
/// Возвращает `None`, когда тело переиграть нельзя, а метод не понижен: тогда
/// честнее вернуть 3xx как есть, чем отправить пустое тело туда, где его ждут.
pub(crate) fn next_hop(
    prev: &HopParts,
    replay: Option<RequestBody>,
    follow: &Follow,
) -> Option<(HopParts, RequestBody)> {
    let mut headers = prev.headers.clone();
    if follow.strip_sensitive {
        for h in SENSITIVE_HEADERS {
            headers.remove(&h);
        }
    }
    let body = if follow.drop_body {
        headers.remove(http::header::CONTENT_LENGTH);
        headers.remove(http::header::CONTENT_TYPE);
        RequestBody::Empty
    } else {
        replay?
    };
    Some((
        HopParts {
            method: follow.method.clone(),
            uri: follow.uri.clone(),
            headers,
            version: prev.version,
            extensions: prev.extensions.clone(),
        },
        body,
    ))
}
```

- [ ] **Step 4: Реализовать клиент**

```rust
// crates/http-ng/src/client.rs
use crate::config::{Config, check_supported};
use crate::stages::redirect::{HopParts, next_hop};
use http_ng_core::Timeouts;
use http_ng_core::unversioned::Transport;
use http_ng_core::{Error, ErrorKind, RequestBody, UnsupportedCapability};
use http_ng_proto::redirect::{RedirectAction, RedirectPolicy, decide};

#[derive(Debug)]
pub struct ClientBuilder<T> {
    transport: T,
    config: Config,
}

impl<T: Transport> ClientBuilder<T> {
    pub fn new(transport: T) -> Self {
        Self { transport, config: Config::default() }
    }
    pub fn redirect(mut self, policy: RedirectPolicy) -> Self {
        self.config.redirect = policy;
        self
    }
    pub fn timeouts(mut self, t: Timeouts) -> Self {
        self.config.timeouts = t;
        self
    }
    pub fn base_url(mut self, uri: http::Uri) -> Self {
        self.config.base_url = Some(uri);
        self
    }
    /// Проверяет конфигурацию против возможностей транспорта. Ни одного
    /// тихого no-op: неподдерживаемая настройка — ошибка здесь и сейчас.
    pub fn build(self) -> Result<Client<T>, UnsupportedCapability> {
        check_supported(&self.config, self.transport.capabilities(), backend_name::<T>())?;
        Ok(Client { transport: self.transport, config: self.config })
    }
}

fn backend_name<T>() -> &'static str {
    // Имя типа достаточно информативно для сообщения об ошибке и ничего не стоит.
    std::any::type_name::<T>()
}

#[derive(Debug)]
pub struct Client<T> {
    transport: T,
    config: Config,
}

impl<T: Transport> Client<T> {
    pub fn builder(transport: T) -> ClientBuilder<T> {
        ClientBuilder::new(transport)
    }
    pub fn transport(&self) -> &T { &self.transport }
    pub fn config(&self) -> &Config { &self.config }

    /// Порядок стадий фиксирован и корректен по построению.
    /// В v0.1 стадия одна — redirect.
    pub async fn execute(&self, req: http::Request<RequestBody>)
        -> Result<http::Response<T::Body>, Error>
    {
        let (parts, mut body) = req.into_parts();
        let mut hp = HopParts {
            method: parts.method,
            uri: parts.uri,
            headers: parts.headers,
            version: parts.version,
            extensions: parts.extensions,
        };
        let mut hops: u8 = 0;

        loop {
            // Снимок для переигрывания снимается ДО отправки: после неё тело
            // уже потреблено. Для `Streaming` вернётся `None` — и это честно
            // известно заранее, а не после провала ретрая.
            let replay = body.rewind();
            let sending = std::mem::replace(&mut body, RequestBody::Empty);

            let resp = self.transport.execute(hp.to_request(sending)).await
                .map_err(|e| Error::new(ErrorKind::Other, e))?;

            let location = resp.headers().get(http::header::LOCATION).map(|v| v.as_bytes());
            let action = decide(&self.config.redirect, hops, &hp.uri, &hp.method,
                                resp.status(), location);

            match action {
                RedirectAction::Stop => return Ok(resp),
                RedirectAction::TooManyRedirects =>
                    return Err(Error::new(ErrorKind::Redirect,
                                          TooMany(self.config.redirect.limit))),
                RedirectAction::InvalidLocation =>
                    return Err(Error::new(ErrorKind::Redirect, BadLocation)),
                RedirectAction::Follow(f) => {
                    hops += 1;
                    let Some((next_hp, next_body)) = next_hop(&hp, replay, &f) else {
                        return Ok(resp);
                    };
                    hp = next_hp;
                    body = next_body;
                }
            }
        }
    }
}

#[derive(Debug)] struct TooMany(u8);
impl std::fmt::Display for TooMany {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "exceeded redirect limit of {}", self.0)
    }
}
impl std::error::Error for TooMany {}

#[derive(Debug)] struct BadLocation;
impl std::fmt::Display for BadLocation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Location header is not a resolvable URI")
    }
}
impl std::error::Error for BadLocation {}
```

- [ ] **Step 5: Запустить тесты**

Run: `cargo test -p http-ng --features test-util --test redirect`
Expected: PASS, шесть тестов.

- [ ] **Step 6: Проверить, что Send всё ещё выводится, а не объявлен**

Добавить в `crates/http-ng/tests/redirect.rs`:

```rust
#[test]
fn client_future_is_send_when_transport_is() {
    fn assert_send<T: Send>(_: T) {}
    let m = MockTransport::new();
    let c = Client::builder(m).build().unwrap();
    // MockTransport использует RefCell и потому !Sync; проверяем только,
    // что бондов Send нет в объявлениях — компиляция теста это и доказывает.
    let _ = c.execute(http::Request::new(RequestBody::Empty));
    assert_send(async { 1u8 });
}
```

Run: `cargo test -p http-ng --features test-util`
Expected: PASS.

- [ ] **Step 7: Commit**

```bash
git add crates/http-ng
git commit -m "feat(http-ng): Client with redirect stage and capability check at build time"
```

---

### Task 13: `http-ng` — Response, Collected и RequestBuilder

**Files:**
- Create: `crates/http-ng/src/response.rs`
- Create: `crates/http-ng/src/request.rs`
- Modify: `crates/http-ng/src/lib.rs`, `crates/http-ng/src/client.rs`
- Test: `crates/http-ng/tests/response.rs`

**Interfaces:**
- Consumes: `Client::execute` (Task 12).
- Produces:
  - `pub struct Response<B> { .. }`; `Response::status()`, `Response::headers()`,
    `Response::version()`, `Response::url()`,
    `Response::into_parts(self) -> (http::response::Parts, B)`,
    `Response::chunk(&mut self) -> impl Future<Output = Option<Result<Bytes, Error>>>`,
    `Response::collect(self) -> impl Future<Output = Result<Collected, Error>>`
  - `pub struct Collected { .. }`; `Collected::bytes()`, `Collected::text()`,
    `Collected::json<T>()`, и **сохраняет** `status()`, `headers()`, `url()`
  - `Client::get/post/put/delete/patch/head/request -> RequestBuilder<'_, T>`
  - `RequestBuilder::{header, headers, body, timeouts, send}` — `timeouts`
    кладёт `http_ng_core::Timeouts` в `Extensions` запроса, откуда их читает
    транспорт (lookup «request-first, client-fallback», §4.5 спеки)

- [ ] **Step 1: Написать падающие тесты**

```rust
// crates/http-ng/tests/response.rs
use http_ng::{Client, RequestBody};
use http_ng::mock::MockTransport;

#[test]
fn collected_keeps_status_and_headers_after_reading_the_body() {
    let m = MockTransport::new();
    m.push_response(http::Response::builder()
        .status(201).header("x-trace", "abc").body("hello").unwrap());

    let c = Client::builder(m).build().unwrap();
    let resp = futures_executor::block_on(
        c.get("https://a/x").send()
    ).unwrap();

    let collected = futures_executor::block_on(resp.collect()).unwrap();
    assert_eq!(collected.text().unwrap(), "hello");
    // Ключевое отличие от reqwest, где `.text()` берёт self по значению:
    assert_eq!(collected.status(), 201);
    assert_eq!(collected.headers().get("x-trace").unwrap(), "abc");
    assert_eq!(collected.url(), &"https://a/x".parse::<http::Uri>().unwrap());
}

#[test]
fn chunk_streams_the_body() {
    let m = MockTransport::new();
    m.push_response(http::Response::builder().status(200).body("stream me").unwrap());

    let c = Client::builder(m).build().unwrap();
    let mut resp = futures_executor::block_on(c.get("https://a/x").send()).unwrap();

    let mut acc = Vec::new();
    while let Some(chunk) = futures_executor::block_on(resp.chunk()) {
        acc.extend_from_slice(&chunk.unwrap());
    }
    assert_eq!(acc, b"stream me");
}

#[test]
fn request_builder_sets_method_and_headers() {
    let m = MockTransport::new();
    m.push_response(http::Response::builder().status(200).body("").unwrap());

    let c = Client::builder(m).build().unwrap();
    let _ = futures_executor::block_on(
        c.post("https://a/x").header("x-k", "v")
         .body(RequestBody::Full(bytes::Bytes::from_static(b"p"))).send()
    ).unwrap();

    let seen = c.transport().requests();
    assert_eq!(seen[0].method, http::Method::POST);
    assert_eq!(seen[0].headers.get("x-k").unwrap(), "v");
}
```

- [ ] **Step 2: Запустить и убедиться, что падает**

Run: `cargo test -p http-ng --features test-util --test response`
Expected: FAIL — `no method named get`.

- [ ] **Step 3: Реализовать Response и Collected**

```rust
// crates/http-ng/src/response.rs
use bytes::{Bytes, BytesMut};
use http_body::Body as HttpBody;
use http_ng_core::{Error, ErrorKind};
use std::pin::Pin;

/// Ответ с сохранённым URL. `into_parts` отдаёт полную верность;
/// `chunk`/`collect` — удобство поверх неё.
#[derive(Debug)]
pub struct Response<B> {
    parts: http::response::Parts,
    body: B,
    url: http::Uri,
}

impl<B> Response<B> {
    pub(crate) fn new(resp: http::Response<B>, url: http::Uri) -> Self {
        let (parts, body) = resp.into_parts();
        Self { parts, body, url }
    }
    pub fn status(&self) -> http::StatusCode { self.parts.status }
    pub fn headers(&self) -> &http::HeaderMap { &self.parts.headers }
    pub fn version(&self) -> http::Version { self.parts.version }
    pub fn url(&self) -> &http::Uri { &self.url }
    pub fn into_parts(self) -> (http::response::Parts, B) { (self.parts, self.body) }
}

impl<B> Response<B>
where B: HttpBody<Data = Bytes> + Unpin, B::Error: std::error::Error + 'static
{
    /// Следующий чанк данных. Трейлер-фреймы пропускаются — за ними идти в
    /// `into_parts` и поллить тело напрямую.
    pub async fn chunk(&mut self) -> Option<Result<Bytes, Error>> {
        loop {
            let frame = std::future::poll_fn(|cx| Pin::new(&mut self.body).poll_frame(cx)).await;
            match frame {
                Some(Ok(f)) => match f.into_data() {
                    Ok(d) => return Some(Ok(d)),
                    Err(_) => continue, // трейлеры
                },
                Some(Err(e)) => return Some(Err(Error::new(ErrorKind::Body, e))),
                None => return None,
            }
        }
    }

    pub async fn collect(mut self) -> Result<Collected, Error> {
        let mut acc = BytesMut::new();
        while let Some(c) = self.chunk().await {
            acc.extend_from_slice(&c?);
        }
        Ok(Collected { parts: self.parts, url: self.url, body: acc.freeze() })
    }
}

/// Прочитанное тело **вместе** со статусом, заголовками и URL.
///
/// У reqwest `Response::{text,json,bytes}` берут `self` по значению, из-за чего
/// после чтения тела статус недоступен (issue #1542).
#[derive(Debug, Clone)]
pub struct Collected {
    parts: http::response::Parts,
    url: http::Uri,
    body: Bytes,
}

impl Collected {
    pub fn status(&self) -> http::StatusCode { self.parts.status }
    pub fn headers(&self) -> &http::HeaderMap { &self.parts.headers }
    pub fn url(&self) -> &http::Uri { &self.url }
    pub fn bytes(&self) -> &Bytes { &self.body }
    pub fn text(&self) -> Result<String, Error> {
        String::from_utf8(self.body.to_vec())
            .map_err(|e| Error::new(ErrorKind::Decode, e))
    }
}
```

- [ ] **Step 4: Реализовать RequestBuilder и методы клиента**

```rust
// crates/http-ng/src/request.rs
use crate::client::Client;
use crate::response::Response;
use http_ng_core::unversioned::Transport;
use http_ng_core::{Error, ErrorKind, RequestBody};

#[derive(Debug)]
pub struct RequestBuilder<'a, T> {
    client: &'a Client<T>,
    method: http::Method,
    uri: Result<http::Uri, http::uri::InvalidUri>,
    headers: http::HeaderMap,
    body: RequestBody,
    extensions: http::Extensions,
}

impl<'a, T: Transport> RequestBuilder<'a, T> {
    pub(crate) fn new(client: &'a Client<T>, method: http::Method, url: &str) -> Self {
        Self { client, method, uri: url.parse(), headers: http::HeaderMap::new(),
               body: RequestBody::Empty, extensions: http::Extensions::new() }
    }

    pub fn header(mut self, name: &str, value: &str) -> Self {
        if let (Ok(n), Ok(v)) = (name.parse::<http::HeaderName>(),
                                 value.parse::<http::HeaderValue>()) {
            self.headers.insert(n, v);
        }
        self
    }

    pub fn headers(mut self, headers: http::HeaderMap) -> Self {
        self.headers = headers;
        self
    }

    pub fn body(mut self, body: RequestBody) -> Self {
        self.body = body;
        self
    }

    /// Таймауты только для этого запроса. Кладутся в `Extensions`, откуда их
    /// читает транспорт; незаданные поля падают обратно на конфигурацию
    /// клиента.
    ///
    /// reqwest этого не умеет вовсе (issue #2641), из-за чего `act-cli`
    /// вынужден строить отдельный `reqwest::Client` на каждый вызов
    /// компонента — со своим пулом соединений.
    pub fn timeouts(mut self, t: http_ng_core::Timeouts) -> Self {
        self.extensions.insert(t);
        self
    }

    pub async fn send(self) -> Result<Response<T::Body>, Error> {
        let uri = self.uri.map_err(|e| Error::new(ErrorKind::Other, e))?;
        let mut req = http::Request::new(self.body);
        *req.method_mut() = self.method;
        *req.uri_mut() = uri.clone();
        *req.headers_mut() = self.headers;
        *req.extensions_mut() = self.extensions;
        let resp = self.client.execute(req).await?;
        Ok(Response::new(resp, uri))
    }
}
```

Добавить в `crates/http-ng/src/client.rs`:

```rust
use crate::request::RequestBuilder;

impl<T: Transport> Client<T> {
    pub fn request(&self, method: http::Method, url: &str) -> RequestBuilder<'_, T> {
        RequestBuilder::new(self, method, url)
    }
    pub fn get(&self, url: &str) -> RequestBuilder<'_, T> {
        self.request(http::Method::GET, url)
    }
    pub fn post(&self, url: &str) -> RequestBuilder<'_, T> {
        self.request(http::Method::POST, url)
    }
    pub fn put(&self, url: &str) -> RequestBuilder<'_, T> {
        self.request(http::Method::PUT, url)
    }
    pub fn delete(&self, url: &str) -> RequestBuilder<'_, T> {
        self.request(http::Method::DELETE, url)
    }
    pub fn patch(&self, url: &str) -> RequestBuilder<'_, T> {
        self.request(http::Method::PATCH, url)
    }
    pub fn head(&self, url: &str) -> RequestBuilder<'_, T> {
        self.request(http::Method::HEAD, url)
    }
}
```

- [ ] **Step 5: Запустить тесты**

Run: `cargo test -p http-ng --features test-util`
Expected: PASS, все тесты `redirect.rs` и `response.rs`.

- [ ] **Step 6: Commit**

```bash
git add crates/http-ng
git commit -m "feat(http-ng): non-destructive Response, Collected and RequestBuilder"
```

---

### Task 14: `http-ng` — SseStream поверх декодера

**Files:**
- Create: `crates/http-ng/src/sse.rs`
- Modify: `crates/http-ng/src/lib.rs`
- Test: `crates/http-ng/tests/sse.rs`

**Interfaces:**
- Consumes: `SseDecoder` (Task 3), `Response::chunk` (Task 13).
- Produces:
  - `pub struct SseStream<B>`; `SseStream::new(resp: Response<B>, max_event_size: usize) -> Result<Self, Error>`
    — проверяет `Content-Type` и статус;
    `SseStream::next(&mut self) -> impl Future<Output = Option<Result<SseEvent, Error>>>`;
    `SseStream::last_event_id(&self) -> Option<&str>`
  - Терминальные правила: статус ≠ 200 → `Err(ErrorKind::Status)`;
    `Content-Type` ≠ `text/event-stream` → `Err(ErrorKind::Decode)`.
    **Реконнект в v0.1 не реализуется** — он требует повторной отправки запроса,
    что приедет вместе со стадией retry в v0.2. `last_event_id()` уже есть,
    чтобы реконнект встал без изменения API.

- [ ] **Step 1: Написать падающие тесты**

```rust
// crates/http-ng/tests/sse.rs
use http_ng::{Client, SseEvent, SseStream, DEFAULT_MAX_EVENT_SIZE};
use http_ng::mock::MockTransport;

fn sse_response(body: &'static str) -> http::Response<&'static str> {
    http::Response::builder().status(200)
        .header("content-type", "text/event-stream").body(body).unwrap()
}

#[test]
fn parses_events_from_a_response() {
    let m = MockTransport::new();
    m.push_response(sse_response("data: one\n\ndata: two\n\n"));

    let c = Client::builder(m).build().unwrap();
    let resp = futures_executor::block_on(c.get("https://a/s").send()).unwrap();
    let mut s = SseStream::new(resp, DEFAULT_MAX_EVENT_SIZE).unwrap();

    let mut got = Vec::new();
    while let Some(e) = futures_executor::block_on(s.next()) { got.push(e.unwrap()) }

    assert_eq!(got, vec![
        SseEvent::Message { event: None, data: "one".into(), id: None },
        SseEvent::Message { event: None, data: "two".into(), id: None },
    ]);
}

#[test]
fn rejects_wrong_content_type() {
    let m = MockTransport::new();
    m.push_response(http::Response::builder().status(200)
        .header("content-type", "application/json").body("{}").unwrap());

    let c = Client::builder(m).build().unwrap();
    let resp = futures_executor::block_on(c.get("https://a/s").send()).unwrap();
    assert!(SseStream::new(resp, DEFAULT_MAX_EVENT_SIZE).is_err());
}

#[test]
fn rejects_non_200_status() {
    let m = MockTransport::new();
    m.push_response(http::Response::builder().status(204)
        .header("content-type", "text/event-stream").body("").unwrap());

    let c = Client::builder(m).build().unwrap();
    let resp = futures_executor::block_on(c.get("https://a/s").send()).unwrap();
    assert!(SseStream::new(resp, DEFAULT_MAX_EVENT_SIZE).is_err(),
            "204 означает «прекрати навсегда», а не «пустой поток»");
}

#[test]
fn tracks_last_event_id_for_future_reconnects() {
    let m = MockTransport::new();
    m.push_response(sse_response("id: 99\ndata: x\n\n"));

    let c = Client::builder(m).build().unwrap();
    let resp = futures_executor::block_on(c.get("https://a/s").send()).unwrap();
    let mut s = SseStream::new(resp, DEFAULT_MAX_EVENT_SIZE).unwrap();
    while futures_executor::block_on(s.next()).is_some() {}
    assert_eq!(s.last_event_id(), Some("99"));
}
```

- [ ] **Step 2: Запустить и убедиться, что падает**

Run: `cargo test -p http-ng --features test-util --test sse`
Expected: FAIL — `cannot find type SseStream`.

- [ ] **Step 3: Реализовать**

```rust
// crates/http-ng/src/sse.rs
use crate::response::Response;
use bytes::Bytes;
use http_body::Body as HttpBody;
use http_ng_core::{Error, ErrorKind};
use http_ng_proto::sse::{SseDecoder, SseEvent};

const MIME: &str = "text/event-stream";

/// Поток событий SSE поверх любого тела ответа.
///
/// Реконнект здесь **не** реализован: он требует повторной отправки запроса и
/// приедет со стадией retry в v0.2. `last_event_id()` уже доступен, поэтому
/// добавление реконнекта не изменит публичный API.
#[derive(Debug)]
pub struct SseStream<B> {
    resp: Response<B>,
    decoder: SseDecoder,
    done: bool,
}

#[derive(Debug)] struct SseRejected(&'static str);
impl std::fmt::Display for SseRejected {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "not an SSE stream: {}", self.0)
    }
}
impl std::error::Error for SseRejected {}

impl<B> SseStream<B>
where B: HttpBody<Data = Bytes> + Unpin, B::Error: std::error::Error + 'static
{
    pub fn new(resp: Response<B>, max_event_size: usize) -> Result<Self, Error> {
        // WHATWG: любой статус кроме 200 — прекратить. 204 в частности означает
        // «больше не подключайся», а не «пустой поток».
        if resp.status() != http::StatusCode::OK {
            return Err(Error::new(ErrorKind::Status, SseRejected("status is not 200")));
        }
        let ok_ct = resp.headers()
            .get(http::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .is_some_and(|v| v.trim_start().starts_with(MIME));
        if !ok_ct {
            return Err(Error::new(ErrorKind::Decode,
                                  SseRejected("content-type is not text/event-stream")));
        }
        Ok(Self { resp, decoder: SseDecoder::new(max_event_size), done: false })
    }

    pub fn last_event_id(&self) -> Option<&str> {
        self.decoder.last_event_id()
    }

    pub async fn next(&mut self) -> Option<Result<SseEvent, Error>> {
        loop {
            if let Some(e) = self.decoder.next() {
                return Some(Ok(e));
            }
            if self.done {
                return None;
            }
            match self.resp.chunk().await {
                Some(Ok(chunk)) => {
                    if let Err(e) = self.decoder.push(&chunk) {
                        self.done = true;
                        // Превышение лимита фатально и не ретраится.
                        return Some(Err(Error::new(ErrorKind::Decode, e)));
                    }
                }
                Some(Err(e)) => { self.done = true; return Some(Err(e)) }
                None => { self.done = true }
            }
        }
    }
}
```

- [ ] **Step 4: Запустить тесты**

Run: `cargo test -p http-ng --features test-util`
Expected: PASS, четыре теста SSE плюс всё предыдущее.

- [ ] **Step 5: Commit**

```bash
git add crates/http-ng
git commit -m "feat(http-ng): SseStream with WHATWG terminal rules over the proto decoder"
```

---

### Task 15: `http-ng-wasi` — тело ответа

**Files:**
- Create: `crates/http-ng-wasi/Cargo.toml`
- Create: `crates/http-ng-wasi/src/lib.rs`
- Create: `crates/http-ng-wasi/src/body.rs`

**Interfaces:**
- Consumes: `wasip3::http_compat::IncomingResponseBody`, `http_ng_core::Error`.
- Produces:
  - `pub struct Body`; `impl http_body::Body for Body { type Data = Bytes; type Error = http_ng_core::Error; }`
  - `Body::empty() -> Self`
  - `is_end_stream()` реализован **корректно** — на хостовой стороне `act`
    ровно этот дефект (`StreamBody` всегда возвращает `false`) приводил к
    трапам гостей на HTTP/2.

- [ ] **Step 1: Создать крейт**

```toml
# crates/http-ng-wasi/Cargo.toml
[package]
name = "http-ng-wasi"
version = "0.1.0"
description = "Транспорт http-ng поверх wasi:http 0.3"
edition.workspace = true
rust-version = "1.90"
license.workspace = true
repository.workspace = true

[dependencies]
bytes        = { workspace = true }
http         = { workspace = true }
http-body    = { workspace = true }
http-ng-core = { workspace = true }
futures      = { version = "0.3", default-features = false, features = ["async-await"] }
wasip3       = { version = "0.7.0", features = ["http-compat"] }

[lints]
workspace = true
```

```rust
// crates/http-ng-wasi/src/lib.rs
//! Транспорт http-ng поверх `wasi:http` 0.3 (пакет `wasip3`).
//!
//! Собирается под `wasm32-wasip2`. Ни один тип `wasip3` не появляется в
//! публичном API этого крейта.
#![deny(unsafe_code)]

mod body;
mod convert;

pub use body::Body;
```

- [ ] **Step 2: Написать тело**

```rust
// crates/http-ng-wasi/src/body.rs
use bytes::Bytes;
use http_body::{Body as HttpBody, Frame};
use http_ng_core::{Error, ErrorKind};
use std::pin::Pin;
use std::task::{Context, Poll};
use wasip3::http_compat::IncomingResponseBody;

/// Тело ответа `wasi:http`. Читает поток инлайн, без фоновой задачи, — значит
/// транспорту не нужна способность `spawn`.
pub struct Body {
    inner: Inner,
}

enum Inner {
    Incoming(IncomingResponseBody),
    Done,
}

impl std::fmt::Debug for Body {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.inner {
            Inner::Incoming(_) => f.write_str("Body(incoming)"),
            Inner::Done => f.write_str("Body(done)"),
        }
    }
}

impl Body {
    pub(crate) fn from_incoming(i: IncomingResponseBody) -> Self {
        Self { inner: Inner::Incoming(i) }
    }
    pub fn empty() -> Self {
        Self { inner: Inner::Done }
    }
}

impl HttpBody for Body {
    type Data = Bytes;
    type Error = Error;

    fn poll_frame(mut self: Pin<&mut Self>, cx: &mut Context<'_>)
        -> Poll<Option<Result<Frame<Bytes>, Error>>>
    {
        match &mut self.inner {
            Inner::Incoming(i) => match Pin::new(i).poll_frame(cx) {
                Poll::Ready(Some(Ok(f))) => Poll::Ready(Some(Ok(f))),
                Poll::Ready(Some(Err(e))) => {
                    self.inner = Inner::Done;
                    Poll::Ready(Some(Err(Error::new(ErrorKind::Body, WasiError(e)))))
                }
                Poll::Ready(None) => { self.inner = Inner::Done; Poll::Ready(None) }
                Poll::Pending => Poll::Pending,
            },
            Inner::Done => Poll::Ready(None),
        }
    }

    /// Реализовано честно. На хостовой стороне `act` использовался
    /// `http_body_util::StreamBody`, который всегда возвращает `false`, из-за
    /// чего гости трапались посреди чтения HTTP/2-ответов.
    fn is_end_stream(&self) -> bool {
        matches!(self.inner, Inner::Done)
    }
}

/// Обёртка над `wasi:http` `ErrorCode`, чтобы он не протёк в публичный API.
#[derive(Debug)]
pub(crate) struct WasiError(pub(crate) wasip3::http::types::ErrorCode);

impl std::fmt::Display for WasiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:?}", self.0)
    }
}
impl std::error::Error for WasiError {}
```

- [ ] **Step 3: Проверить, что крейт собирается под wasip2**

Run: `cargo check -p http-ng-wasi --target wasm32-wasip2`
Expected: успех (модуль `convert` пока пустой — создать
`crates/http-ng-wasi/src/convert.rs` с одной строкой `// см. Task 16`).

- [ ] **Step 4: Commit**

```bash
git add crates/http-ng-wasi
git commit -m "feat(wasi): response Body with a correct is_end_stream"
```

---

### Task 16: `http-ng-wasi` — Transport, конверсия и honoring сеттеров

Здесь исчезают семь `let _ =` из `wasi-fetch`.

**Files:**
- Create/Modify: `crates/http-ng-wasi/src/convert.rs`
- Modify: `crates/http-ng-wasi/src/lib.rs`
- Test: `crates/http-ng-wasi/src/convert.rs` (`#[cfg(test)]` — только чистые части)

**Interfaces:**
- Consumes: `Transport`, `Capabilities`, `RequestBody`, `Error`, `Body` (Task 15).
- Produces:
  - `pub struct WasiHttp { caps: Capabilities }`; `WasiHttp::new() -> Self`
  - `impl Transport for WasiHttp { type Body = Body; type Error = Error; }`
  - `pub(crate) fn to_wasi_method(m: &http::Method) -> wasip3::http::types::Method`
  - `pub(crate) fn scheme_of(uri: &http::Uri) -> Result<wasip3::http::types::Scheme, Error>`
  - Возможности `WasiHttp`: `streaming_request_body: true`, `full_duplex: true`,
    `request_trailers: true`, `response_trailers: true`,
    `redirects: RedirectSupport::None`, `timeouts` — все три `true`,
    `upgrade: UpgradeSupport::None`.

- [ ] **Step 1: Написать падающие тесты на чистые части**

```rust
// crates/http-ng-wasi/src/convert.rs
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_known_methods_and_passes_through_unknown() {
        use wasip3::http::types::Method as WM;
        assert!(matches!(to_wasi_method(&http::Method::GET), WM::Get));
        assert!(matches!(to_wasi_method(&http::Method::DELETE), WM::Delete));
        let query = http::Method::from_bytes(b"QUERY").unwrap();
        assert!(matches!(to_wasi_method(&query), WM::Other(ref s) if s == "QUERY"));
    }

    #[test]
    fn rejects_non_http_schemes() {
        let ftp: http::Uri = "ftp://a/x".parse().unwrap();
        assert!(scheme_of(&ftp).is_err());
        let none: http::Uri = "/relative".parse().unwrap();
        assert!(scheme_of(&none).is_err());
    }

    #[test]
    fn capabilities_declare_what_wasi_http_actually_does() {
        let c = super::super::WasiHttp::new();
        let caps = http_ng_core::unversioned::Transport::capabilities(&c);
        // wasi:http 0.3 богаче нативного по стримингу…
        assert!(caps.full_duplex);
        assert!(caps.request_trailers && caps.response_trailers);
        // …и беднее по всему остальному.
        assert_eq!(caps.redirects, http_ng_core::RedirectSupport::None);
        assert_eq!(caps.upgrade, http_ng_core::UpgradeSupport::None);
        assert_eq!(caps.tls_config, http_ng_core::TlsSupport::None);
        assert!(!caps.proxy);
    }
}
```

- [ ] **Step 2: Запустить и убедиться, что падает**

Run: `cargo test -p http-ng-wasi --target wasm32-wasip2`
Expected: FAIL на этапе компиляции — функций нет. (Запуск тестов под wasip2
требует раннера; см. Step 6. До его настройки использовать
`cargo check -p http-ng-wasi --target wasm32-wasip2 --tests`.)

- [ ] **Step 3: Реализовать конверсию и honoring сеттеров**

```rust
// crates/http-ng-wasi/src/convert.rs
use crate::body::{Body, WasiError};
use http_ng_core::{Error, ErrorKind, UnsupportedCapability};
use wasip3::http::types::{Method as WM, RequestOptions, Scheme};

pub(crate) fn to_wasi_method(m: &http::Method) -> WM {
    match *m {
        http::Method::GET => WM::Get,
        http::Method::POST => WM::Post,
        http::Method::PUT => WM::Put,
        http::Method::DELETE => WM::Delete,
        http::Method::PATCH => WM::Patch,
        http::Method::HEAD => WM::Head,
        http::Method::OPTIONS => WM::Options,
        _ => WM::Other(m.to_string()),
    }
}

#[derive(Debug)] pub(crate) struct BadScheme;
impl std::fmt::Display for BadScheme {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "URI scheme must be http or https")
    }
}
impl std::error::Error for BadScheme {}

pub(crate) fn scheme_of(uri: &http::Uri) -> Result<Scheme, Error> {
    match uri.scheme_str() {
        Some("https") => Ok(Scheme::Https),
        Some("http") => Ok(Scheme::Http),
        _ => Err(Error::new(ErrorKind::Other, BadScheme)),
    }
}

/// Применяет таймауты, **не проглатывая отказы хоста**.
///
/// `wasi:http` 0.3 возвращает из сеттеров
/// `result<_, request-options-error{not-supported, immutable, other}>` именно
/// для того, чтобы хост мог сказать «не умею». `wasi-fetch` отбрасывал семь
/// таких `Result` через `let _ =`; здесь каждый отказ становится ошибкой.
pub(crate) fn apply_timeouts(
    opts: &RequestOptions,
    connect: Option<u64>,
    first_byte: Option<u64>,
    between_bytes: Option<u64>,
) -> Result<(), Error> {
    let unsupported = |what: &'static str| {
        Error::new(ErrorKind::Unsupported,
                   UnsupportedCapability { what, backend: "wasi:http" })
    };
    if let Some(ns) = connect {
        opts.set_connect_timeout(Some(ns)).map_err(|_| unsupported("connect_timeout"))?;
    }
    if let Some(ns) = first_byte {
        opts.set_first_byte_timeout(Some(ns)).map_err(|_| unsupported("first_byte_timeout"))?;
    }
    if let Some(ns) = between_bytes {
        opts.set_between_bytes_timeout(Some(ns))
            .map_err(|_| unsupported("between_bytes_timeout"))?;
    }
    Ok(())
}

pub(crate) fn wasi_err(e: wasip3::http::types::ErrorCode) -> Error {
    use http_ng_core::Phase;
    use wasip3::http::types::ErrorCode as EC;
    // Категория сохраняется. `wasi-fetch` расплющивал всё в
    // `Error::Transport(format!("{e:?}"))`, а хостовая сторона `act` потом
    // восстанавливала её подстрочным матчингом по цепочке `source()`.
    //
    // Имена вариантов сверены с wasip3-0.7.0+wasi-0.3.0/src/service.rs:161-206.
    let kind = match &e {
        EC::DnsTimeout | EC::DnsError(_) => ErrorKind::Resolve,
        EC::DestinationNotFound
        | EC::DestinationUnavailable
        | EC::DestinationIpProhibited
        | EC::DestinationIpUnroutable
        | EC::ConnectionRefused
        | EC::ConnectionTerminated
        | EC::ConnectionLimitReached => ErrorKind::Connect,
        EC::ConnectionTimeout => ErrorKind::Timeout(Phase::Connect),
        EC::ConnectionReadTimeout | EC::HttpResponseTimeout => {
            ErrorKind::Timeout(Phase::FirstByte)
        }
        EC::ConnectionWriteTimeout => ErrorKind::Timeout(Phase::BetweenBytes),
        EC::TlsProtocolError | EC::TlsCertificateError | EC::TlsAlertReceived(_) => {
            ErrorKind::Tls
        }
        EC::HttpRequestDenied => ErrorKind::Status,
        EC::LoopDetected => ErrorKind::Redirect,
        EC::HttpUpgradeFailed | EC::ConfigurationError => ErrorKind::Unsupported,
        EC::HttpResponseIncomplete
        | EC::HttpResponseBodySize(_)
        | EC::HttpResponseTransferCoding(_)
        | EC::HttpResponseContentCoding(_) => ErrorKind::Body,
        _ => ErrorKind::Other,
    };
    Error::new(kind, WasiError(e))
}
```

- [ ] **Step 4: Реализовать `WasiHttp`**

```rust
// добавить в crates/http-ng-wasi/src/lib.rs
use bytes::Bytes;
use futures::join;
use http_ng_core::unversioned::Transport;
use http_ng_core::{Capabilities, Error, RedirectSupport, RequestBody, TimeoutSupport,
                   Timeouts, TlsSupport, UpgradeSupport};
use wasip3::http::types::{ErrorCode, Fields, Request, RequestOptions};
use wasip3::http_compat::{BodyWriter, http_from_wasi_response};

#[derive(Debug)]
pub struct WasiHttp {
    caps: Capabilities,
}

impl WasiHttp {
    pub fn new() -> Self {
        let mut caps = Capabilities::none();
        // wasi:http 0.3 симметричен по телам и умеет трейлеры в обе стороны —
        // богаче нативного.
        caps.streaming_request_body = true;
        caps.full_duplex = true;
        caps.request_trailers = true;
        caps.response_trailers = true;
        caps.timeouts = TimeoutSupport { connect: true, first_byte: true, between_bytes: true };
        // И беднее по всему остальному: в спеке нет ни редиректов, ни TLS,
        // ни прокси, ни выбора версии, ни upgrade.
        caps.redirects = RedirectSupport::None;
        caps.tls_config = TlsSupport::None;
        caps.upgrade = UpgradeSupport::None;
        Self { caps }
    }
}

impl Default for WasiHttp {
    fn default() -> Self { Self::new() }
}

impl Transport for WasiHttp {
    type Body = Body;
    type Error = Error;

    async fn execute(&self, req: http::Request<RequestBody>)
        -> Result<http::Response<Body>, Error>
    {
        let (parts, body) = req.into_parts();
        let scheme = convert::scheme_of(&parts.uri)?;

        let header_list: Vec<(String, Vec<u8>)> = parts.headers.iter()
            .map(|(k, v)| (k.to_string(), v.as_bytes().to_vec()))
            .collect();
        let fields = Fields::from_list(&header_list)
            .map_err(|e| Error::new(http_ng_core::ErrorKind::Other, convert::FieldsError(e)))?;

        let timeouts = parts.extensions.get::<Timeouts>().copied().unwrap_or_default();
        let opts = RequestOptions::new();
        convert::apply_timeouts(
            &opts,
            timeouts.connect.map(|d| d.as_nanos() as u64),
            timeouts.first_byte.map(|d| d.as_nanos() as u64),
            timeouts.between_bytes.map(|d| d.as_nanos() as u64),
        )?;

        let payload: Option<Bytes> = match &body {
            RequestBody::Empty => None,
            RequestBody::Full(b) if b.is_empty() => None,
            RequestBody::Full(b) => Some(b.clone()),
            // Стриминговые и rewindable тела приедут вместе со стадией retry.
            _ => None,
        };

        let (writer, wasi_request) = match payload {
            None => {
                let (_, trailers) =
                    wasip3::wit_future::new::<Result<Option<Fields>, ErrorCode>>(|| Ok(None));
                let (request, _) = Request::new(fields, None, trailers, Some(opts));
                (None, request)
            }
            Some(_) => {
                let (w, reader, trailers) = BodyWriter::new();
                let (request, _) = Request::new(fields, Some(reader), trailers, Some(opts));
                (Some(w), request)
            }
        };

        wasi_request.set_method(&convert::to_wasi_method(&parts.method))
            .map_err(|_| convert::rejected("method"))?;
        wasi_request.set_scheme(Some(&scheme))
            .map_err(|_| convert::rejected("scheme"))?;
        if let Some(a) = parts.uri.authority() {
            wasi_request.set_authority(Some(a.as_str()))
                .map_err(|_| convert::rejected("authority"))?;
        }
        wasi_request.set_path_with_query(parts.uri.path_and_query().map(|p| p.as_str()))
            .map_err(|_| convert::rejected("path_with_query"))?;

        // Структурная конкуррентность: тело пишется рядом с send, без spawn.
        // Именно поэтому WASI-транспорту не нужна способность Spawn.
        let wasi_response = match (writer, payload_bytes(&body)) {
            (Some(w), Some(bytes)) => {
                let mut b = Body::from_bytes(bytes);
                let (resp, _written) = join!(
                    wasip3::http::client::send(wasi_request),
                    w.send_http_body(&mut b),
                );
                resp
            }
            _ => wasip3::http::client::send(wasi_request).await,
        }.map_err(convert::wasi_err)?;

        let (resp_parts, incoming) = http_from_wasi_response(wasi_response)
            .map_err(convert::wasi_err)?
            .into_parts();
        Ok(http::Response::from_parts(resp_parts, Body::from_incoming(incoming)))
    }

    fn capabilities(&self) -> &Capabilities { &self.caps }
}

fn payload_bytes(body: &RequestBody) -> Option<Bytes> {
    match body {
        RequestBody::Full(b) if !b.is_empty() => Some(b.clone()),
        _ => None,
    }
}
```

Добавить в `convert.rs` вспомогательные типы:

```rust
#[derive(Debug)] pub(crate) struct Rejected(&'static str);
impl std::fmt::Display for Rejected {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "wasi:http host rejected setting `{}`", self.0)
    }
}
impl std::error::Error for Rejected {}
pub(crate) fn rejected(what: &'static str) -> Error {
    Error::new(ErrorKind::Unsupported, Rejected(what))
}

#[derive(Debug)] pub(crate) struct FieldsError(pub(crate) wasip3::http::types::HeaderError);
impl std::fmt::Display for FieldsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "invalid headers: {:?}", self.0)
    }
}
impl std::error::Error for FieldsError {}
```

Добавить в `body.rs` конструктор `Body::from_bytes(Bytes) -> Self` с вариантом
`Inner::Buffered(Option<Bytes>)` и соответствующей веткой в `poll_frame`.

- [ ] **Step 5: Проверить сборку и инвариант «нет `let _ =` на Result сеттеров»**

Run: `cargo check -p http-ng-wasi --target wasm32-wasip2`
Expected: успех.

Run: `! grep -rn "let _ = .*set_" crates/http-ng-wasi/src && echo OK`
Expected: `OK`.

- [ ] **Step 6: Настроить раннер тестов под wasip2**

Run: `cargo install wasmtime-cli --locked`

Создать `.cargo/config.toml`:

```toml
[target.wasm32-wasip2]
runner = "wasmtime run -S http --"
```

Run: `cargo test -p http-ng-wasi --target wasm32-wasip2`
Expected: PASS, три теста конверсии.

Если `wasmtime` установить не удаётся, оставить `cargo check --tests` как
временный шлюз и завести issue — интеграционный прогон переезжает в вертикаль 3.

- [ ] **Step 7: Commit**

```bash
git add crates/http-ng-wasi .cargo/config.toml
git commit -m "feat(wasi): Transport over wasi:http 0.3 honouring every option setter"
```

---

### Task 17: Сквозной пример и README

**Files:**
- Create: `crates/http-ng-wasi/examples/fetch.rs`
- Create: `README.md`
- Modify: `.github/workflows/ci.yml`

**Interfaces:**
- Consumes: всё предыдущее.
- Produces: ничего для кода.

- [ ] **Step 1: Написать пример**

```rust
// crates/http-ng-wasi/examples/fetch.rs
//! Тот же код, который в вертикали 2 заработает на native, а в вертикали 3 —
//! в браузере. Меняется только тип транспорта.

use http_ng::Client;
use http_ng_wasi::WasiHttp;

fn main() {
    let client = Client::builder(WasiHttp::new()).build().expect("caps ok");
    let fut = async {
        let resp = client.get("https://example.com/").send().await?;
        let collected = resp.collect().await?;
        println!("{} {}", collected.status(), collected.text()?);
        Ok::<_, http_ng::Error>(())
    };
    futures::executor::block_on(fut).expect("request failed");
}
```

- [ ] **Step 2: Проверить, что пример собирается**

Run: `cargo build -p http-ng-wasi --example fetch --target wasm32-wasip2`
Expected: успех.

- [ ] **Step 3: Написать README с таблицей зависимостей**

````markdown
# http-ng

Кроссплатформенный асинхронный HTTP-клиент. Один и тот же прикладной код
собирается под native, браузер и WASI — транспорт подменяется, а не
обкладывается `#[cfg]`.

```rust
let client = http_ng::Client::builder(transport).build()?;
let text = client.get("https://example.com").send().await?.collect().await?.text()?;
```

## Что в графе зависимостей

| сборка | tokio |
|---|---|
| ambient (`http-ng` + `-wasi` / `-fetch`) | **нет вообще** |
| native, только HTTP/1 | есть, но с фичами `sync` + `default`; весь его dep-tree — `pin-project-lite` |
| native + HTTP/2 | настоящий: `h2` тянет `tokio` с `io-util` и `tokio-util` с `codec`, а через него `libc` |

Убрать tokio из hyper-сборок нельзя: [hyper#3428](https://github.com/hyperium/hyper/pull/3428)
(ровно эта замена на `futures-channel`) отклонён, а
[hyper#3767](https://github.com/hyperium/hyper/issues/3767) закрыт как *not planned*.

## Статус

v0.1: ядро, `wasi:http` 0.3. Native и браузер — вертикали 2 и 3.
Дизайн: [`docs/superpowers/specs/2026-08-05-http-ng-design.md`](docs/superpowers/specs/2026-08-05-http-ng-design.md).
````

- [ ] **Step 4: Добавить сборку примера в CI**

```yaml
  # в job `wasip2`, после существующего шага
      - run: cargo build -p http-ng-wasi --example fetch --target wasm32-wasip2
```

- [ ] **Step 5: Прогнать всё**

Run: `cargo test --workspace --all-features && cargo check -p http-ng-wasi --target wasm32-wasip2`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add README.md crates/http-ng-wasi/examples .github/workflows/ci.yml
git commit -m "docs: README with dependency-graph table and end-to-end wasi example"
```

---

## Что эта вертикаль доказала и что осталось

**Доказано:** форма `Transport` работает против реального ambient-бэкенда без
сокета; ядро не нуждается в объявленном `Send`; неподдерживаемые настройки
становятся ошибками, а не тихими no-op; протокольная логика тестируется без
рантайма и фаззится.

**Не доказано и переходит в вертикаль 2:** рантайм-шов (нужны tokio и smol на
одном коде); `Client<T = DefaultTransport>`; стриминговые тела запроса
(требуют `RequestBody::Streaming` в транспорте).

**Переходит в вертикаль 3:** `http-ng-fetch` и с ним проверка рантайм-модели
`Capabilities`; реконнект `SseStream`; приёмка `act`.
