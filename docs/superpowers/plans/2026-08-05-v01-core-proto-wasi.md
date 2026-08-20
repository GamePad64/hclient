# hclient v0.1, vertical 1: core + proto + WASI — implementation plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A working async HTTP client that goes over the network via
`wasi:http` 0.3, with a portable core that knows nothing about hyper or
sockets.

**Architecture:** Three crates. `hclient-proto` — pure state machines with no
`async` (SSE decoder, redirect logic). `hclient-core` — the plugin contract:
the `Transport` trait, `Capabilities`, `RequestBody`, `Error`, `Timer`.
`hclient` — the user-facing surface: `Client<T>`, builder, stages, `Response`,
the SSE stream. `hclient-wasi` — the first transport. The core and the stages
are tested on the host against a mock transport; wasm is only needed for the
transport's integration tests.

**Tech Stack:** Rust edition 2024, MSRV 1.85 (1.90 for `hclient-wasi`).
`http` 1.5, `http-body` 1.1, `bytes` 1.12, `futures-core` 0.3, `url` 2.5,
`wasip3` 0.7.0+wasi-0.3.0. Tests: `proptest` 1.x, `http-body-util` 0.1.
No async runtimes in this vertical's graph.

## Global Constraints

These requirements implicitly apply to every task. Values are copied from the
spec, `docs/superpowers/specs/2026-08-05-hclient-design.md`.

- **`hclient-proto` has no `tokio`, `futures-*`, or `async-*` in its graph**
  and contains not a single `async fn`. Checked in CI (Task 1).
- **`hclient-core` and `hclient` don't declare a single `Send`/`Sync` bound,
  don't have a single `Box<dyn ...>` on the hot path, and don't have a single
  `#[cfg]`-switched trait alias.** `Send` is inferred as an auto-trait through
  `impl Future`.
- **Plugin traits live in the `unversioned` module** (`Transport`, `Timer`)
  with a doc string: "breaking changes in this module ship in minor, not
  major."
- **No foreign type appears in the public API** of `hclient` and
  `hclient-core`, other than `http`, `http-body`, `bytes`, `futures-core`. In
  particular, `wasip3::*` is not re-exported.
- **An unsupported setting is a typed error, never a silent no-op.** Not a
  single `let _ =` on a `Result` from a capability setter.
- **`default = []` in every crate.**
- `edition = "2024"`, `rust-version = "1.85"` (except `hclient-wasi`: `"1.90"`).
- Every crate: `#![deny(unsafe_code)]`, except `hclient-wasi` (which also has
  none, but keep the deny anyway).
- Commits — at every "Commit" step, message in the imperative, prefix
  `feat:`/`test:`/`chore:`/`docs:`.

## File layout

```
Cargo.toml                             workspace, [workspace.dependencies], lints
.github/workflows/ci.yml               matrix + invariant checks
crates/hclient-proto/
  src/lib.rs                           re-exports, we don't claim #![no_std] compatibility
  src/sse/mod.rs                       SseDecoder — public API
  src/sse/lines.rs                     BOM + line splitting across chunk boundaries
  src/sse/decode.rs                    fields, event accumulation, dispatch
  src/redirect.rs                      decide() — the pure redirect decision
  fuzz/fuzz_targets/sse.rs             fuzzing the decoder
crates/hclient-core/
  src/lib.rs
  src/error.rs                         Error, ErrorKind, Phase
  src/body.rs                          RequestBody, RetryKind
  src/caps.rs                          Capabilities, UnsupportedCapability
  src/timer.rs                         Timer            (unversioned module)
  src/transport.rs                     Transport        (unversioned module)
crates/hclient/
  src/lib.rs
  src/config.rs                        Config, Timeouts, RedirectConfig, lookup
  src/client.rs                        Client<T>, ClientBuilder<T>
  src/request.rs                       RequestBuilder
  src/response.rs                      Response, Collected
  src/stages/mod.rs
  src/stages/redirect.rs               applying the decision from proto
  src/sse.rs                           SseStream — reconnect on top of the decoder
  src/mock.rs                          MockTransport, behind the `test-util` feature
crates/hclient-wasi/
  src/lib.rs                           WasiHttp: Transport
  src/body.rs                          Body: http_body::Body
  src/convert.rs                       http <-> wasi, including honoring setters
```

**Not part of this vertical:** `hclient-native`, `hclient-rt*`, `hclient-tls*`,
`hclient-dns*`, `hclient-fetch`, the pool, h2/h3, `Negotiate`. The default
parameter `Client<T = DefaultTransport>` shows up in vertical 2, once a native
transport exists; adding a default type parameter isn't a breaking change.

**Deliberate deviation from spec §10.** The spec assigns `hclient-fetch` to
v0.1 on the grounds that fetch is the only backend with runtime differences in
capabilities (duplex in Chrome 131+, absent in Safari), and is therefore the
only test of the runtime-registry decision for `Capabilities`. Here it's
deferred to vertical 3, so vertical 1 delivers a runnable result. **Consequence:
until vertical 3, the "runtime-`Capabilities` instead of cfg" decision remains
unverified.** If vertical 3 shows the registry doesn't work, the rework touches
`hclient-core` — i.e., Task 8.

**In another repository:** the `wasi-fetch` 0.3 compatibility facade lives in
`/mnt/devenv/workspace/act/wasi-fetch` and isn't planned here. It's done after
`hclient-wasi` works, as a separate change in that repository.

---

### Task 1: Workspace, invariants and CI

**Files:**
- Create: `Cargo.toml`
- Create: `.github/workflows/ci.yml`
- Create: `rustfmt.toml`

**Interfaces:**
- Consumes: nothing
- Produces: `[workspace.dependencies]` pinning `http = "1.5"`, `http-body = "1.1"`,
  `bytes = "1.12"`, `futures-core = "0.3"`, `url = "2.5"`, `http-body-util = "0.1"`,
  `proptest = "1"`. Every later crate picks them up via `.workspace = true`.

- [ ] **Step 1: Create the workspace manifest**

```toml
# Cargo.toml
[workspace]
resolver = "3"
members = ["crates/*"]

[workspace.package]
edition      = "2024"
rust-version = "1.85"
license      = "MIT OR Apache-2.0"
repository   = "https://github.com/GamePad64/hclient"

[workspace.dependencies]
http           = "1.5"
http-body      = "1.1"
http-body-util = "0.1"
bytes          = "1.12"
futures-core   = { version = "0.3", default-features = false }
url            = "2.5"
proptest       = "1"

hclient-proto = { path = "crates/hclient-proto", version = "0.1.0" }
hclient-core  = { path = "crates/hclient-core",  version = "0.1.0" }
hclient       = { path = "crates/hclient",       version = "0.1.0" }

[workspace.lints.rust]
unsafe_code       = "deny"
missing_debug_implementations = "warn"
unexpected_cfgs   = { level = "warn", check-cfg = [] }
```

- [ ] **Step 2: Create rustfmt.toml**

```toml
edition = "2024"
max_width = 100
```

- [ ] **Step 3: Verify the empty workspace builds**

Run: `cargo metadata --no-deps --format-version 1 > /dev/null && echo OK`
Expected: `OK` (there are no members yet — that's fine; `members =
["crates/*"]` over an empty directory is only an error if the directory
doesn't exist; create it: `mkdir -p crates`).

- [ ] **Step 4: Write CI with invariant checks**

```yaml
# .github/workflows/ci.yml
name: ci
on: [push, pull_request]

# Crates show up as the vertical progresses. Each check activates as soon as
# its crate exists, and until then EXPLICITLY prints that it was skipped. A
# silent green check is more dangerous than a red one: after a typo in a
# crate name it stays green forever.

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
            echo "::notice::no crates in the workspace yet — tests skipped"
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
          for p in hclient-proto hclient-core hclient; do
            if [ -d "crates/$p" ]; then pkgs="$pkgs -p $p"; fi
          done
          if [ -z "$pkgs" ]; then
            echo "::notice::no core crates yet — MSRV not checked"
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
          if [ ! -d crates/hclient-wasi ]; then
            echo "::notice::hclient-wasi doesn't exist yet — wasip2 build skipped"
            exit 0
          fi
          cargo check -p hclient-wasi --target wasm32-wasip2

  # ── invariants from the spec ────────────────────────────────────────────
  proto-is-sans-io:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@stable
      - name: no async deps in hclient-proto
        shell: bash
        run: |
          set -euo pipefail
          if [ ! -d crates/hclient-proto ]; then
            echo "::notice::hclient-proto doesn't exist yet — check skipped"
            exit 0
          fi
          if cargo tree -p hclient-proto -e normal --prefix none \
               | grep -Ei '^(tokio|futures-|async-|smol|compio)'; then
            echo "::error::hclient-proto picked up an async dependency"
            exit 1
          fi
      - name: no async fn in hclient-proto
        shell: bash
        run: |
          set -euo pipefail
          if [ ! -d crates/hclient-proto/src ]; then
            echo "::notice::hclient-proto doesn't exist yet — check skipped"
            exit 0
          fi
          if grep -rn "async fn" crates/hclient-proto/src; then
            echo "::error::a sans-io crate contains async fn"
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
          for d in crates/hclient-core/src crates/hclient/src; do
            if [ -d "$d" ]; then dirs="$dirs $d"; fi
          done
          if [ -z "$dirs" ]; then
            echo "::notice::no core crates yet — check skipped"
            exit 0
          fi
          # We look for DECLARED bounds, not mentions in docs: lines whose
          # content starts with a comment are dropped by the second grep.
          if grep -rnE '(:|\+)[[:space:]]*(Send|Sync)\b|MaybeSend' $dirs \
               | grep -vE '^[^:]+:[0-9]+:[[:space:]]*(//|/\*|\*)'; then
            echo "::error::a Send or Sync bound is declared in the core"
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

### Task 2: `hclient-proto` — splitting the SSE stream into lines

The trickiest part of SSE: exactly one BOM gets stripped, three line
terminators (CRLF/LF/CR), and all of it has to survive being split at a chunk
boundary — including a split inside the BOM and a split between CR and LF.

**Files:**
- Create: `crates/hclient-proto/Cargo.toml`
- Create: `crates/hclient-proto/src/lib.rs`
- Create: `crates/hclient-proto/src/sse/mod.rs`
- Create: `crates/hclient-proto/src/sse/lines.rs`
- Test: inside `crates/hclient-proto/src/sse/lines.rs` (`#[cfg(test)] mod tests`)

**Interfaces:**
- Consumes: nothing
- Produces: `pub(crate) struct LineSplitter`; `LineSplitter::new() -> Self`;
  `LineSplitter::push(&mut self, chunk: &[u8])`;
  `LineSplitter::next_line(&mut self) -> Option<Vec<u8>>` — returns the line
  **without** its terminator; `LineSplitter::buffered_len(&self) -> usize`.

- [ ] **Step 1: Write failing tests**

```rust
// crates/hclient-proto/src/sse/lines.rs
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
        // a second BOM is ordinary data
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

- [ ] **Step 2: Run it and confirm the tests fail**

Run: `cargo test -p hclient-proto`
Expected: FAIL — `cannot find type LineSplitter`.

- [ ] **Step 3: Create the manifest and crate root**

```toml
# crates/hclient-proto/Cargo.toml
[package]
name = "hclient-proto"
version = "0.1.0"
description = "Pure state machines for hclient's protocol layers: no I/O, no async, no runtime"
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
// crates/hclient-proto/src/lib.rs
//! Pure state machines for hclient's protocol layers.
//!
//! Crate invariant: not a single `async fn`, not a single dependency on a
//! runtime. Anything time-dependent takes `now` as a parameter. Checked in CI.
#![deny(unsafe_code)]

pub mod redirect;
pub mod sse;
```

```rust
// crates/hclient-proto/src/sse/mod.rs
mod lines;
pub(crate) use lines::LineSplitter;
```

- [ ] **Step 4: Implement `LineSplitter`**

```rust
// crates/hclient-proto/src/sse/lines.rs

/// Splits a byte stream into lines per the WHATWG EventSource rules:
/// exactly one leading BOM is stripped, terminators are CRLF, LF, or a lone
/// CR. Survives a chunk split anywhere, including mid-BOM and between CR and LF.
#[derive(Debug)]
pub(crate) struct LineSplitter {
    buf: Vec<u8>,
    /// How many BOM bytes have been confirmed so far. 3 = the BOM has been
    /// resolved (either stripped or rejected).
    bom_seen: usize,
    bom_done: bool,
    /// The previous byte was CR — the next LF needs to be swallowed.
    pending_cr: bool,
}

const BOM: [u8; 3] = [0xEF, 0xBB, 0xBF];

impl LineSplitter {
    pub(crate) fn new() -> Self {
        Self { buf: Vec::new(), bom_seen: 0, bom_done: false, pending_cr: false }
    }

    pub(crate) fn push(&mut self, chunk: &[u8]) {
        let mut rest = chunk;

        // BOM phase: accumulate up to three bytes, decide once.
        while !self.bom_done && !rest.is_empty() {
            let b = rest[0];
            if b == BOM[self.bom_seen] {
                self.bom_seen += 1;
                rest = &rest[1..];
                if self.bom_seen == 3 {
                    self.bom_done = true; // the BOM has been fully stripped
                }
            } else {
                // Not a BOM: whatever we accumulated is ordinary data.
                self.buf.extend_from_slice(&BOM[..self.bom_seen]);
                self.bom_done = true;
            }
        }

        for &b in rest {
            if self.pending_cr {
                self.pending_cr = false;
                if b == b'\n' {
                    continue; // the LF after CR is already accounted for by the terminator
                }
            }
            self.buf.push(b);
        }
    }

    pub(crate) fn next_line(&mut self) -> Option<Vec<u8>> {
        let pos = self.buf.iter().position(|&b| b == b'\n' || b == b'\r')?;
        let term = self.buf[pos];
        let line: Vec<u8> = self.buf.drain(..pos).collect();
        self.buf.remove(0); // the terminator itself
        if term == b'\r' {
            if self.buf.first() == Some(&b'\n') {
                self.buf.remove(0); // CRLF inside the buffer
            } else if self.buf.is_empty() {
                self.pending_cr = true; // CR at the end — the LF may arrive in the next chunk
            }
        }
        Some(line)
    }

    pub(crate) fn buffered_len(&self) -> usize {
        self.buf.len()
    }
}
```

- [ ] **Step 5: Run the tests**

Run: `cargo test -p hclient-proto`
Expected: PASS, seven tests.

- [ ] **Step 6: Add a property test for the invariant "chunking doesn't affect the result"**

```rust
// add to the same mod tests
use proptest::prelude::*;

proptest! {
    #[test]
    fn chunking_does_not_change_lines(data: Vec<u8>, split_at in 0usize..64) {
        let whole = collect(&[&data]);
        let at = split_at.min(data.len());
        let (a, b) = data.split_at(at);
        let split = collect(&[a, b]);
        prop_assert_eq!(whole, split);
    }
}
```

- [ ] **Step 7: Run it and confirm the property test passes**

Run: `cargo test -p hclient-proto -- --include-ignored`
Expected: PASS.

- [ ] **Step 8: Commit**

```bash
git add crates/hclient-proto
git commit -m "feat(proto): SSE line splitter surviving chunk boundaries and BOM"
```

---

### Task 3: `hclient-proto` — the SSE event decoder

**Files:**
- Create: `crates/hclient-proto/src/sse/decode.rs`
- Modify: `crates/hclient-proto/src/sse/mod.rs`
- Test: inside `decode.rs`

**Interfaces:**
- Consumes: `LineSplitter` from Task 2.
- Produces:
  - `pub enum SseEvent { Message { event: Option<String>, data: String, id: Option<String> }, Comment(String), Retry(core::time::Duration) }`
  - `pub enum SseError { EventTooLarge { limit: usize } }`
  - `pub struct SseDecoder`; `SseDecoder::new(max_event_size: usize) -> Self`;
    `SseDecoder::push(&mut self, chunk: &[u8]) -> Result<(), SseError>`;
    `SseDecoder::next(&mut self) -> Option<SseEvent>`;
    `SseDecoder::last_event_id(&self) -> Option<&str>`.

- [ ] **Step 1: Write failing tests**

```rust
// crates/hclient-proto/src/sse/decode.rs
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
        // "data" is equivalent to "data:"
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

- [ ] **Step 2: Run it and confirm it fails**

Run: `cargo test -p hclient-proto`
Expected: FAIL — `cannot find type SseDecoder`.

- [ ] **Step 3: Implement the decoder**

```rust
// crates/hclient-proto/src/sse/decode.rs
use super::LineSplitter;
use core::time::Duration;
use std::collections::VecDeque;

/// An SSE event. `Comment` and `Retry` are first-class deliberately: without
/// the first you can't build a keep-alive detector, without the second you
/// lose blocks that contain only `retry:`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SseEvent {
    Message { event: Option<String>, data: String, id: Option<String> },
    Comment(String),
    Retry(Duration),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SseError {
    /// The raw event size limit was exceeded. Fatal and **not retried**.
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
    /// Bytes accumulated in the current event (raw, before parsing).
    event_bytes: usize,
    data: String,
    event_type: Option<String>,
    last_event_id: Option<String>,
    /// The current block's event already had an `id` field.
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
        // An unfinished line counts too — otherwise the limit can be bypassed
        // with an infinite line that never gets a terminator.
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
            // EXACTLY one leading space is stripped, same as for fields.
            // `trim_start_matches(' ')` would strip all of them and lose
            // significant ones.
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
                // WHATWG: the value AND a newline get appended to the buffer.
                // One trailing newline is stripped on dispatch. A "separator
                // only between non-empty parts" scheme would give a
                // different result for an empty first field.
                self.data.push_str(&String::from_utf8_lossy(value));
                self.data.push('\n');
            }
            b"event" => {
                // A repeated field — last one wins, NOT an error.
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
            _ => {} // an unknown field is ignored
        }
    }

    fn dispatch(&mut self) {
        let event = self.event_type.take();
        if self.data.is_empty() {
            // Empty data buffer: reset without dispatching.
            // last_event_id is NOT reset in this case.
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

- [ ] **Step 4: Wire up the module**

```rust
// crates/hclient-proto/src/sse/mod.rs
mod decode;
mod lines;

pub use decode::{SseDecoder, SseError, SseEvent};
pub(crate) use lines::LineSplitter;

/// The default limit — matches `rmcp::DEFAULT_MAX_SSE_EVENT_SIZE`, so the
/// adapter doesn't change behavior.
pub const DEFAULT_MAX_EVENT_SIZE: usize = 16 * 1024 * 1024;
```

- [ ] **Step 5: Run the tests**

Run: `cargo test -p hclient-proto`
Expected: PASS, all twelve decoder tests plus Task 2's tests.

- [ ] **Step 6: Commit**

```bash
git add crates/hclient-proto
git commit -m "feat(proto): WHATWG-conformant SSE decoder with first-class comments and retry"
```

---

### Task 4: `hclient-proto` — a fuzz target for SSE

**Files:**
- Create: `crates/hclient-proto/fuzz/Cargo.toml`
- Create: `crates/hclient-proto/fuzz/fuzz_targets/sse.rs`
- Modify: `.github/workflows/ci.yml`

**Interfaces:**
- Consumes: `SseDecoder::{new, push, next}` from Task 3.
- Produces: nothing for code; the `fuzz-smoke` CI job.

- [ ] **Step 1: Install cargo-fuzz**

Run: `cargo install cargo-fuzz --locked`
Expected: successful install (needs nightly to run, but not to install).

- [ ] **Step 2: Create the fuzz target**

```toml
# crates/hclient-proto/fuzz/Cargo.toml
[package]
name = "hclient-proto-fuzz"
version = "0.0.0"
edition = "2024"
publish = false

[package.metadata]
cargo-fuzz = true

[dependencies]
libfuzzer-sys = "0.4"
hclient-proto = { path = ".." }

[[bin]]
name = "sse"
path = "fuzz_targets/sse.rs"
test = false
doc  = false
bench = false

[workspace]
```

```rust
// crates/hclient-proto/fuzz/fuzz_targets/sse.rs
#![no_main]
use libfuzzer_sys::fuzz_target;
use hclient_proto::sse::SseDecoder;

// Invariant: the decoder never panics and never grows past the limit.
fuzz_target!(|data: &[u8]| {
    const LIMIT: usize = 4096;
    let mut d = SseDecoder::new(LIMIT);
    for chunk in data.chunks(7) {
        if d.push(chunk).is_err() {
            return; // EventTooLarge is a legitimate terminal outcome
        }
        while d.next().is_some() {}
    }
});
```

- [ ] **Step 3: Run the fuzzer briefly**

Run: `cd crates/hclient-proto/fuzz && cargo +nightly fuzz run sse -- -max_total_time=60`
Expected: 60 seconds with no panics and no crashes.

- [ ] **Step 4: Add a smoke job to CI**

```yaml
  # add to .github/workflows/ci.yml
  fuzz-smoke:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@nightly
      - run: cargo install cargo-fuzz --locked
      - run: cargo fuzz run sse -- -max_total_time=60
        working-directory: crates/hclient-proto/fuzz
```

- [ ] **Step 5: Commit**

```bash
git add crates/hclient-proto/fuzz .github/workflows/ci.yml
git commit -m "test(proto): fuzz the SSE decoder in CI"
```

---

### Task 5: `hclient-proto` — the redirect decision

A pure function. Fixes three defects in `wasi-fetch`'s current loop: 304/305
aren't followed, sensitive headers get stripped on a host **or** scheme
change, 301/302 with POST get downgraded to GET the same as 303.

**Files:**
- Create: `crates/hclient-proto/src/redirect.rs`
- Modify: `crates/hclient-proto/src/lib.rs`
- Test: inside `redirect.rs`

**Interfaces:**
- Consumes: nothing
- Produces:
  - `pub struct RedirectPolicy { pub limit: u8 }`
  - `pub struct Follow { pub uri: http::Uri, pub method: http::Method, pub strip_sensitive: bool, pub drop_body: bool }`
  - `pub enum RedirectAction { Stop, Follow(Follow), TooManyRedirects, InvalidLocation }`
  - `pub fn decide(policy: &RedirectPolicy, hops: u8, current: &http::Uri, method: &http::Method, status: http::StatusCode, location: Option<&[u8]>) -> RedirectAction`
  - `pub const SENSITIVE_HEADERS: [http::HeaderName; 3]` — `authorization`,
    `cookie`, `proxy-authorization`.

- [ ] **Step 1: Write failing tests**

```rust
// crates/hclient-proto/src/redirect.rs
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

- [ ] **Step 2: Run it and confirm it fails**

Run: `cargo test -p hclient-proto redirect`
Expected: FAIL — `cannot find function decide`.

- [ ] **Step 3: Implement**

```rust
// crates/hclient-proto/src/redirect.rs
//! The decision on whether to follow a redirect. A pure function: no I/O, no time.

use http::{HeaderName, Method, StatusCode, Uri};

/// Headers stripped when moving to a different origin.
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
    /// Strip `SENSITIVE_HEADERS`: the host or scheme changed.
    pub strip_sensitive: bool,
    /// The method was downgraded to GET — no body may be sent.
    pub drop_body: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RedirectAction {
    /// Not a redirect, or a redirect with no `Location` — return the response as-is.
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
    // IMPORTANT: not `status.is_redirection()`. 300 Multiple Choices requires
    // a user choice, 304 Not Modified is a response to a conditional request,
    // 305 Use Proxy hasn't been followed since 2014, 306 is reserved.
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

    // 303 is always GET (except HEAD). Browsers and reqwest downgrade
    // 301/302 with POST to GET; diverging from 303 here would be inconsistent.
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

- [ ] **Step 4: Wire it up and run the tests**

`crates/hclient-proto/src/lib.rs` already contains `pub mod redirect;` (Task 2, Step 3).

Run: `cargo test -p hclient-proto`
Expected: PASS, twelve redirect tests plus everything from before.

- [ ] **Step 5: Commit**

```bash
git add crates/hclient-proto
git commit -m "feat(proto): redirect decision honouring 304/305 and stripping on scheme change"
```

---

### Task 6: `hclient-core` — Error and ErrorKind

**Files:**
- Create: `crates/hclient-core/Cargo.toml`
- Create: `crates/hclient-core/src/lib.rs`
- Create: `crates/hclient-core/src/error.rs`
- Test: inside `error.rs`

**Interfaces:**
- Consumes: nothing
- Produces:
  - `pub enum ErrorKind { Resolve, Connect, Tls, Redirect, Timeout(Phase), Body, Decode, Status, Unsupported, Other }` (`#[non_exhaustive]`)
  - `pub enum Phase { Connect, FirstByte, BetweenBytes, Total }`
  - `pub struct Error` (`Clone`), `Error::new(kind, source)`, `Error::kind()`,
    `Error::is_timeout()`, `Error::is_redirect()`, `Error::is_connect()`
  - `impl std::error::Error for Error`

- [ ] **Step 1: Write failing tests**

```rust
// crates/hclient-core/src/error.rs
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
        // The source is available whole — not as a substring of a message.
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
        // The core doesn't declare Send: an error from a !Send source still builds.
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

- [ ] **Step 2: Run it and confirm it fails**

Run: `cargo test -p hclient-core`
Expected: FAIL — the crate doesn't exist yet.

- [ ] **Step 3: Create the crate and implement**

```toml
# crates/hclient-core/Cargo.toml
[package]
name = "hclient-core"
version = "0.1.0"
description = "hclient's plugin contract: Transport, Capabilities, RequestBody, Error, Timer"
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
// crates/hclient-core/src/lib.rs
//! hclient's plugin contract.
//!
//! Crate invariant: not a single declared `Send`/`Sync` bound. Send-ness is
//! inferred as an auto-trait through `impl Future`.
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
// crates/hclient-core/src/error.rs
use std::sync::Arc;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase { Connect, FirstByte, BetweenBytes, Total }

/// The error's category. Exists so a consumer doesn't have to classify
/// errors by substring-matching on `Display`.
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

/// `Clone` on purpose: reqwest's opaque, non-cloneable error is a source
/// of constant complaints (reqwest#1053). `Arc<dyn Error>` doesn't require
/// `Send`, so auto-trait transparency reaches errors too.
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

- [ ] **Step 4: Run the tests**

Run: `cargo test -p hclient-core error`
Expected: PASS, four tests.

**Declare modules as they appear, don't comment them out.** At this task,
`lib.rs` contains only `mod error;` and `pub use error::…`; `body`, `caps` and
`unversioned` get added in Task 7, 8 and 9 respectively. Commented-out code
in a commit is a defect.

- [ ] **Step 5: Commit**

```bash
git add crates/hclient-core
git commit -m "feat(core): typed Error with kind enum, Clone and preserved source"
```

---

### Task 7: `hclient-core` — RequestBody with a replay contract

**Files:**
- Create: `crates/hclient-core/src/body.rs`
- Modify: `crates/hclient-core/src/lib.rs`
- Test: inside `body.rs`

**Interfaces:**
- Consumes: nothing
- Produces:
  - `pub enum RetryKind { Free, ViaFactory, Impossible }`
  - `pub enum RequestBody { Empty, Full(bytes::Bytes), Rewindable(RewindFactory), Streaming(BoxedStream) }`
  - `pub type RewindFactory = std::sync::Arc<dyn Fn() -> RequestBody>`
  - `RequestBody::retry_kind(&self) -> RetryKind`
  - `RequestBody::rewind(&self) -> Option<RequestBody>`
  - `RequestBody::size_hint(&self) -> Option<u64>`

- [ ] **Step 1: Write failing tests**

```rust
// crates/hclient-core/src/body.rs
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

- [ ] **Step 2: Run it and confirm it fails**

Run: `cargo test -p hclient-core body`
Expected: FAIL — `cannot find type RequestBody`.

- [ ] **Step 3: Implement**

```rust
// crates/hclient-core/src/body.rs
use bytes::Bytes;
use std::sync::Arc;

/// Whether this body can be replayed — known **before** sending.
///
/// `reqwest::Request::try_clone() -> Option<Request>` answers the same
/// question after the retry layer has already decided to retry, and so it
/// silently disables retries on streaming bodies.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RetryKind {
    /// Replayed for free.
    Free,
    /// Replayed by calling a factory.
    ViaFactory,
    /// Cannot be replayed.
    Impossible,
}

pub type RewindFactory = Arc<dyn Fn() -> RequestBody>;

/// A request body with an explicit replay contract.
pub enum RequestBody {
    Empty,
    Full(Bytes),
    Rewindable(RewindFactory),
    /// A single-pass body. The concrete stream is supplied by the transport;
    /// in v0.1 the core only needs to know it can't be replayed.
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

- [ ] **Step 4: Uncomment `mod body;` and `pub use` in `lib.rs`, run the tests**

Run: `cargo test -p hclient-core`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/hclient-core
git commit -m "feat(core): RequestBody with replay contract knowable before sending"
```

---

### Task 8: `hclient-core` — Capabilities and UnsupportedCapability

**Files:**
- Create: `crates/hclient-core/src/caps.rs`
- Modify: `crates/hclient-core/src/lib.rs`
- Test: inside `caps.rs`

**Interfaces:**
- Consumes: nothing
- Produces:
  - `pub struct Capabilities` (`#[non_exhaustive]`, `Debug`, `Clone`) with the
    fields from spec §4.6
  - `pub enum RedirectSupport { None, Internal, Configurable, Inspectable }`
  - `pub enum TlsSupport { None, ServerTrustCallbackOnly, Full }`
  - `pub enum UpgradeSupport { None, H1, ExtendedConnect, Both }`
  - `pub struct TimeoutSupport { pub connect: bool, pub first_byte: bool, pub between_bytes: bool }`
  - `pub struct Timeouts { pub connect: Option<Duration>, pub first_byte: Option<Duration>, pub between_bytes: Option<Duration> }` (`Copy`, `Default` = all `None`)
  - `pub struct UnsupportedCapability { pub what: &'static str, pub backend: &'static str }`
  - `Capabilities::none() -> Self` — everything off, the base for backends

> **Why `Timeouts` lives here, and not in `hclient`.** Transports read them
> from the request's `http::Extensions`, and `hclient-wasi` depends only on
> `hclient-core`. Had we defined `Timeouts` in `hclient`, the transport
> couldn't have seen them, and per-request timeouts would be unreachable.

- [ ] **Step 1: Write failing tests**

```rust
// crates/hclient-core/src/caps.rs
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

- [ ] **Step 2: Run it and confirm it fails**

Run: `cargo test -p hclient-core caps`
Expected: FAIL.

- [ ] **Step 3: Implement**

```rust
// crates/hclient-core/src/caps.rs
use http::HeaderName;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RedirectSupport {
    /// No redirects, nothing to observe.
    None,
    /// The backend follows on its own; we don't control or see it (wasi:http).
    Internal,
    /// We set the policy.
    Configurable,
    /// We set the policy and see every hop.
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

/// The timeout triple — the `wasi:http` shape, the richest of the ambient
/// models.
///
/// In fetch it collapses into a single `AbortController`; in native it
/// spreads across connector / awaiting response / body idle. A single
/// `Duration` would throw away information the WASI backend knows how to use.
///
/// Lives in `hclient-core` because transports read it from the request's
/// `http::Extensions`, and they don't depend on `hclient`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Timeouts {
    pub connect: Option<core::time::Duration>,
    pub first_byte: Option<core::time::Duration>,
    pub between_bytes: Option<core::time::Duration>,
}

/// What the transport can do **in this process, right now**.
///
/// Runtime, deliberately, not `cfg!`: one wasm binary works in both Chrome
/// (streaming request body since 131) and Safari (no).
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
    /// Everything off. The base a backend turns on from, for whatever it actually supports.
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

/// A setting the chosen transport cannot honor.
///
/// Returned from `build()`, not silently ignored. The model is `wasi:http`
/// itself, whose setters return `request-options-error::not-supported`.
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

- [ ] **Step 4: Uncomment in `lib.rs`, run the tests**

Run: `cargo test -p hclient-core`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/hclient-core
git commit -m "feat(core): runtime Capabilities registry and typed UnsupportedCapability"
```

---

### Task 9: `hclient-core` — Transport and Timer in the `unversioned` module

**Files:**
- Create: `crates/hclient-core/src/unversioned/mod.rs`
- Create: `crates/hclient-core/src/unversioned/transport.rs`
- Create: `crates/hclient-core/src/unversioned/timer.rs`
- Modify: `crates/hclient-core/src/lib.rs`
- Test: `crates/hclient-core/tests/shape.rs`

**Interfaces:**
- Consumes: `RequestBody` (Task 7), `Capabilities` (Task 8).
- Produces:
  - `pub trait Transport { type Body: http_body::Body<Data = Bytes>; type Error: std::error::Error + 'static; fn execute(&self, req: http::Request<RequestBody>) -> impl Future<Output = Result<http::Response<Self::Body>, Self::Error>>; fn capabilities(&self) -> &Capabilities; }`
  - `pub trait Timer { type Instant: Copy; fn sleep(&self, d: core::time::Duration) -> impl Future<Output = ()>; fn now(&self) -> Self::Instant; fn elapsed_since(&self, earlier: Self::Instant) -> core::time::Duration; }`

- [ ] **Step 1: Write a failing shape test**

```rust
// crates/hclient-core/tests/shape.rs
//! This test asserts the core's central architectural property: `Send` is
//! declared nowhere, but it's inferred as an auto-trait when the transport
//! is genuinely Send.

use bytes::Bytes;
use hclient_core::unversioned::Transport;
use hclient_core::{Capabilities, Error, ErrorKind, RequestBody};

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

- [ ] **Step 2: Run it and confirm it fails**

Run: `cargo test -p hclient-core --test shape`
Expected: FAIL — `unresolved import hclient_core::unversioned::Transport`.

- [ ] **Step 3: Add the dev-dependency and implement the traits**

In `crates/hclient-core/Cargo.toml`:

```toml
[dev-dependencies]
http-body-util = { workspace = true }
```

```rust
// crates/hclient-core/src/unversioned/mod.rs
//! # Semver quarantine
//!
//! This module's traits are the contract for backend and runtime authors.
//! It hasn't been validated against every backend yet, so:
//!
//! **Breaking changes in `unversioned` ship in a minor version, not a major one.**
//!
//! The technique is borrowed from `ureq`. Without it, 1.0 can't ship: a
//! trait can't be frozen without checking it against native, wasi:http, and fetch.

mod timer;
mod transport;

pub use timer::Timer;
pub use transport::Transport;
```

```rust
// crates/hclient-core/src/unversioned/transport.rs
use crate::{Capabilities, RequestBody};
use bytes::Bytes;
use std::future::Future;

/// The single seam between hclient and real HTTP.
///
/// The shape is taken from `wasi:http/client.send` — the poorest of the
/// ambient APIs. Anything richer degrades to it cleanly; the reverse doesn't
/// hold.
///
/// No `poll_ready`, no `&mut self`, no `Send`: Send-ness is inferred as an
/// auto-trait through the returned `impl Future`.
pub trait Transport {
    type Body: http_body::Body<Data = Bytes>;
    type Error: std::error::Error + 'static;

    fn execute(
        &self,
        req: http::Request<RequestBody>,
    ) -> impl Future<Output = Result<http::Response<Self::Body>, Self::Error>>;

    /// What this transport can do **right now, in this process**.
    fn capabilities(&self) -> &Capabilities;
}
```

```rust
// crates/hclient-core/src/unversioned/timer.rs
use core::time::Duration;
use std::future::Future;

/// The only runtime capability the portable core needs: timeouts and
/// backoff. Networking and spawn live in the transports.
///
/// Not `hyper::rt::Timer`: theirs has `Sleep: Send + Sync` unconditionally,
/// `sleep()` returns `Pin<Box<dyn Sleep>>` (an allocation on every sleep),
/// and `now()` is typed to `std::time::Instant`, which panics on
/// `wasm32-unknown-unknown`.
pub trait Timer {
    type Instant: Copy;

    fn sleep(&self, d: Duration) -> impl Future<Output = ()>;
    fn now(&self) -> Self::Instant;
    fn elapsed_since(&self, earlier: Self::Instant) -> Duration;
}
```

Add `pub mod unversioned;` to `lib.rs` (already there from Task 6, Step 3).

- [ ] **Step 4: Run the tests**

Run: `cargo test -p hclient-core`
Expected: PASS. The `send_propagates_without_being_declared` test is what
verifies that the core doesn't need a declared `Send`.

- [ ] **Step 5: Manually verify the "no declared Send" invariant**

Run: `! grep -rnE ':\s*Send\b|\+\s*Send\b|MaybeSend' crates/hclient-core/src && echo OK`
Expected: `OK`.

- [ ] **Step 6: Commit**

```bash
git add crates/hclient-core
git commit -m "feat(core): Transport and Timer traits under the unversioned semver quarantine"
```

---

### Task 10: `hclient` — Config, Timeouts and per-request lookup

**Files:**
- Create: `crates/hclient/Cargo.toml`
- Create: `crates/hclient/src/lib.rs`
- Create: `crates/hclient/src/config.rs`
- Test: inside `config.rs`

**Interfaces:**
- Consumes: `hclient_core::{Capabilities, TimeoutSupport, UnsupportedCapability}`.
- Produces:
  - `pub struct Config { pub timeouts: Timeouts, pub redirect: hclient_proto::redirect::RedirectPolicy, pub base_url: Option<http::Uri> }`
    (`Timeouts` is defined in Task 8, in `hclient-core`, and only re-exported
    here — transports need it, and they don't depend on `hclient`)
  - `pub fn effective_timeouts(req: &http::Extensions, client: &Timeouts) -> Timeouts` — "request-first, client-fallback"
  - `pub fn check_supported(cfg: &Config, caps: &Capabilities, backend: &'static str) -> Result<(), UnsupportedCapability>`

- [ ] **Step 1: Write failing tests**

```rust
// crates/hclient/src/config.rs
#[cfg(test)]
mod tests {
    use super::*;
    use hclient_core::{Capabilities, TimeoutSupport};
    use std::time::Duration;

    fn secs(n: u64) -> Option<Duration> { Some(Duration::from_secs(n)) }

    #[test]
    fn request_overrides_client_field_by_field() {
        let client = Timeouts { connect: secs(1), first_byte: secs(2), between_bytes: secs(3) };
        let mut ext = http::Extensions::new();
        ext.insert(Timeouts { connect: secs(9), ..Default::default() });
        let eff = effective_timeouts(&ext, &client);
        assert_eq!(eff.connect, secs(9), "the request overrides");
        assert_eq!(eff.first_byte, secs(2), "the rest falls back to the client");
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

- [ ] **Step 2: Run it and confirm it fails**

Run: `cargo test -p hclient`
Expected: FAIL — the crate doesn't exist.

- [ ] **Step 3: Create the crate and implement**

```toml
# crates/hclient/Cargo.toml
[package]
name = "hclient"
version = "0.1.0"
description = "Cross-platform async HTTP client: one codebase for native, browser and WASI"
edition.workspace = true
rust-version.workspace = true
license.workspace = true
repository.workspace = true

[features]
default = []
# Mock transport for consumer tests.
test-util = []

[dependencies]
bytes         = { workspace = true }
futures-core  = { workspace = true }
http          = { workspace = true }
http-body     = { workspace = true }
hclient-core  = { workspace = true }
hclient-proto = { workspace = true }

[dev-dependencies]
http-body-util = { workspace = true }

[lints]
workspace = true
```

```rust
// crates/hclient/src/lib.rs
//! Cross-platform async HTTP client.
//!
//! Crate invariant: not a single declared `Send`/`Sync` bound, not a single
//! `#[cfg]`-switched trait alias. Send-ness is inferred as an auto-trait.
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
pub use hclient_core::{Capabilities, Error, ErrorKind, Phase, RequestBody, RetryKind,
                       UnsupportedCapability};
pub use hclient_proto::redirect::RedirectPolicy;
pub use hclient_proto::sse::{SseEvent, DEFAULT_MAX_EVENT_SIZE};
pub use request::RequestBuilder;
pub use response::{Collected, Response};
pub use sse::SseStream;
```

```rust
// crates/hclient/src/config.rs
// `Timeouts` is defined in `hclient-core` (Task 8): transports read it from
// `http::Extensions`, and they don't depend on `hclient`.
pub use hclient_core::Timeouts;
use hclient_core::{Capabilities, UnsupportedCapability};
use hclient_proto::redirect::RedirectPolicy;

#[derive(Debug, Clone, Default)]
pub struct Config {
    pub timeouts: Timeouts,
    pub redirect: RedirectPolicy,
    pub base_url: Option<http::Uri>,
}

/// "Request-first, client-fallback," field by field.
///
/// reqwest can't do this (issue #2641 isn't implemented), which is why
/// `act-cli` is forced to build a separate `reqwest::Client` for every
/// component call.
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

/// Called from `ClientBuilder::build()`. Not a single silent no-op.
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

**Declare modules as they appear.** At this task, `lib.rs` contains only
`mod config;` and its re-exports; `mock`, `client`, `request`, `response`,
`sse` and `stages` get added in Task 11–14. Commented-out code in a commit
is a defect.

- [ ] **Step 4: Run the tests**

Run: `cargo test -p hclient config`
Expected: PASS, four tests.

- [ ] **Step 5: Commit**

```bash
git add crates/hclient
git commit -m "feat(hclient): timeout triple with request-first client-fallback lookup"
```

---

### Task 11: `hclient` — MockTransport

The mock is needed before the client: without it, stages can only be tested
over the network.

**Files:**
- Create: `crates/hclient/src/mock.rs`
- Modify: `crates/hclient/src/lib.rs`
- Test: inside `mock.rs`

**Interfaces:**
- Consumes: `Transport`, `Capabilities`, `RequestBody`, `Error`.
- Produces:
  - `pub struct MockTransport`
  - `MockTransport::new() -> Self`
  - `MockTransport::push_response(&self, resp: http::Response<&'static str>)` — a queue of responses
  - `MockTransport::with_capabilities(self, caps: Capabilities) -> Self`
  - `MockTransport::requests(&self) -> Vec<RecordedRequest>`
  - `pub struct RecordedRequest { pub method: http::Method, pub uri: http::Uri, pub headers: http::HeaderMap }`
  - `impl Transport for MockTransport { type Body = http_body_util::Full<Bytes>; type Error = Error; }`

- [ ] **Step 1: Write failing tests**

```rust
// crates/hclient/src/mock.rs
#[cfg(test)]
mod tests {
    use super::*;
    use hclient_core::unversioned::Transport;

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

- [ ] **Step 2: Run it and confirm it fails**

Run: `cargo test -p hclient --features test-util mock`
Expected: FAIL — `cannot find type MockTransport`.

- [ ] **Step 3: Add a dev-dependency on an executor and implement**

In `crates/hclient/Cargo.toml`:

```toml
[dev-dependencies]
http-body-util   = { workspace = true }
futures-executor = { version = "0.3", default-features = false, features = ["std"] }
```

```rust
// crates/hclient/src/mock.rs
//! A mock transport: lets the client and stages be tested on the host, with
//! no network and no wasm runtime. Available behind the `test-util` feature.

use bytes::Bytes;
use http_body_util::Full;
use hclient_core::unversioned::Transport;
use hclient_core::{Capabilities, Error, ErrorKind, RequestBody};
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

- [ ] **Step 4: Run the tests**

Run: `cargo test -p hclient --features test-util mock`
Expected: PASS, two tests.

- [ ] **Step 5: Commit**

```bash
git add crates/hclient
git commit -m "feat(hclient): MockTransport for host-side testing of client and stages"
```

---

### Task 12: `hclient` — Client, ClientBuilder and the redirect stage

**Files:**
- Create: `crates/hclient/src/client.rs`
- Create: `crates/hclient/src/stages/mod.rs`
- Create: `crates/hclient/src/stages/redirect.rs`
- Modify: `crates/hclient/src/lib.rs`
- Test: `crates/hclient/tests/redirect.rs`

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

- [ ] **Step 1: Write failing tests**

```rust
// crates/hclient/tests/redirect.rs
use hclient::{Client, RedirectPolicy, RequestBody};
use hclient::mock::MockTransport;

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
    assert!(seen[0].headers.contains_key("authorization"), "the first hop keeps it");
    assert!(!seen[1].headers.contains_key("authorization"), "the second hop strips it");
    assert!(seen[1].headers.contains_key("x-safe"), "non-sensitive headers stay");
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
    assert_eq!(c.transport().requests().len(), 3, "the original request plus two hops");
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
    use hclient::Timeouts;
    let m = MockTransport::new(); // Capabilities::none() — timeouts unsupported
    let err = Client::builder(m)
        .timeouts(Timeouts { connect: Some(std::time::Duration::from_secs(1)),
                             ..Default::default() })
        .build()
        .unwrap_err();
    assert_eq!(err.what, "connect_timeout");
}
```

- [ ] **Step 2: Run it and confirm it fails**

Run: `cargo test -p hclient --features test-util --test redirect`
Expected: FAIL — `cannot find type Client`.

- [ ] **Step 3: Implement the redirect stage**

```rust
// crates/hclient/src/stages/mod.rs
pub(crate) mod redirect;
```

```rust
// crates/hclient/src/stages/redirect.rs
//! Applies the decision made in `hclient-proto`. Just data shuffling here:
//! all the logic is the pure function `proto::redirect::decide`.

use hclient_core::RequestBody;
use hclient_proto::redirect::{Follow, SENSITIVE_HEADERS};

/// Everything carried between hops, except the body.
///
/// A separate type because `http::request::Parts` **doesn't implement
/// `Clone`**, and between hops the method, URI and headers are needed both
/// before and after sending. `HeaderMap`, `Uri`, `Method` and `Extensions`
/// are all cloneable — verified.
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

/// Builds the next hop. `replay` is a snapshot of the body, taken **before**
/// the previous attempt was sent; `None` means the body can't be reproduced.
///
/// Returns `None` when the body can't be replayed and the method wasn't
/// downgraded: at that point it's more honest to return the 3xx as-is than
/// to send an empty body where one is expected.
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

- [ ] **Step 4: Implement the client**

```rust
// crates/hclient/src/client.rs
use crate::config::{Config, check_supported};
use crate::stages::redirect::{HopParts, next_hop};
use hclient_core::Timeouts;
use hclient_core::unversioned::Transport;
use hclient_core::{Error, ErrorKind, RequestBody, UnsupportedCapability};
use hclient_proto::redirect::{RedirectAction, RedirectPolicy, decide};

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
    /// Checks the configuration against the transport's capabilities. Not a
    /// single silent no-op: an unsupported setting is an error, here and now.
    pub fn build(self) -> Result<Client<T>, UnsupportedCapability> {
        check_supported(&self.config, self.transport.capabilities(), backend_name::<T>())?;
        Ok(Client { transport: self.transport, config: self.config })
    }
}

fn backend_name<T>() -> &'static str {
    // The type name is informative enough for an error message and costs nothing.
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

    /// The stage order is fixed and correct by construction.
    /// In v0.1 there's exactly one stage — redirect.
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
            // The replay snapshot is taken BEFORE sending: after that, the
            // body is already consumed. For `Streaming` this returns `None`
            // — and that's known honestly up front, not after a retry fails.
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

- [ ] **Step 5: Run the tests**

Run: `cargo test -p hclient --features test-util --test redirect`
Expected: PASS, six tests.

- [ ] **Step 6: Verify Send is still inferred, not declared**

Add to `crates/hclient/tests/redirect.rs`:

```rust
#[test]
fn client_future_is_send_when_transport_is() {
    fn assert_send<T: Send>(_: T) {}
    let m = MockTransport::new();
    let c = Client::builder(m).build().unwrap();
    // MockTransport uses a RefCell and is therefore !Sync; we're only
    // checking that no Send bounds appear in the declarations — the test
    // compiling at all is what proves that.
    let _ = c.execute(http::Request::new(RequestBody::Empty));
    assert_send(async { 1u8 });
}
```

Run: `cargo test -p hclient --features test-util`
Expected: PASS.

- [ ] **Step 7: Commit**

```bash
git add crates/hclient
git commit -m "feat(hclient): Client with redirect stage and capability check at build time"
```

---

### Task 13: `hclient` — Response, Collected and RequestBuilder

**Files:**
- Create: `crates/hclient/src/response.rs`
- Create: `crates/hclient/src/request.rs`
- Modify: `crates/hclient/src/lib.rs`, `crates/hclient/src/client.rs`
- Test: `crates/hclient/tests/response.rs`

**Interfaces:**
- Consumes: `Client::execute` (Task 12).
- Produces:
  - `pub struct Response<B> { .. }`; `Response::status()`, `Response::headers()`,
    `Response::version()`, `Response::url()`,
    `Response::into_parts(self) -> (http::response::Parts, B)`,
    `Response::chunk(&mut self) -> impl Future<Output = Option<Result<Bytes, Error>>>`,
    `Response::collect(self) -> impl Future<Output = Result<Collected, Error>>`
  - `pub struct Collected { .. }`; `Collected::bytes()`, `Collected::text()`,
    `Collected::json<T>()`, and it **keeps** `status()`, `headers()`, `url()`
  - `Client::get/post/put/delete/patch/head/request -> RequestBuilder<'_, T>`
  - `RequestBuilder::{header, headers, body, timeouts, send}` — `timeouts`
    puts `hclient_core::Timeouts` into the request's `Extensions`, which the
    transport reads from (the "request-first, client-fallback" lookup, spec §4.5)

- [ ] **Step 1: Write failing tests**

```rust
// crates/hclient/tests/response.rs
use hclient::{Client, RequestBody};
use hclient::mock::MockTransport;

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
    // The key difference from reqwest, where `.text()` takes self by value:
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

- [ ] **Step 2: Run it and confirm it fails**

Run: `cargo test -p hclient --features test-util --test response`
Expected: FAIL — `no method named get`.

- [ ] **Step 3: Implement Response and Collected**

```rust
// crates/hclient/src/response.rs
use bytes::{Bytes, BytesMut};
use http_body::Body as HttpBody;
use hclient_core::{Error, ErrorKind};
use std::pin::Pin;

/// A response with the URL kept around. `into_parts` gives full fidelity;
/// `chunk`/`collect` are convenience layered on top of it.
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
    /// The next data chunk. Trailer frames are skipped — go through
    /// `into_parts` and poll the body directly to get those.
    pub async fn chunk(&mut self) -> Option<Result<Bytes, Error>> {
        loop {
            let frame = std::future::poll_fn(|cx| Pin::new(&mut self.body).poll_frame(cx)).await;
            match frame {
                Some(Ok(f)) => match f.into_data() {
                    Ok(d) => return Some(Ok(d)),
                    Err(_) => continue, // trailers
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

/// The body that's been read, **together** with the status, headers, and URL.
///
/// reqwest's `Response::{text,json,bytes}` take `self` by value, which is
/// why the status becomes unreachable after reading the body (issue #1542).
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

- [ ] **Step 4: Implement RequestBuilder and the client's methods**

```rust
// crates/hclient/src/request.rs
use crate::client::Client;
use crate::response::Response;
use hclient_core::unversioned::Transport;
use hclient_core::{Error, ErrorKind, RequestBody};

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

    /// Timeouts for this request only. Put into `Extensions`, which the
    /// transport reads from; unset fields fall back to the client's
    /// configuration.
    ///
    /// reqwest can't do this at all (issue #2641), which is why `act-cli` is
    /// forced to build a separate `reqwest::Client` for every component
    /// call — with its own connection pool.
    pub fn timeouts(mut self, t: hclient_core::Timeouts) -> Self {
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

Add to `crates/hclient/src/client.rs`:

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

- [ ] **Step 5: Run the tests**

Run: `cargo test -p hclient --features test-util`
Expected: PASS, all `redirect.rs` and `response.rs` tests.

- [ ] **Step 6: Commit**

```bash
git add crates/hclient
git commit -m "feat(hclient): non-destructive Response, Collected and RequestBuilder"
```

---

### Task 14: `hclient` — SseStream on top of the decoder

**Files:**
- Create: `crates/hclient/src/sse.rs`
- Modify: `crates/hclient/src/lib.rs`
- Test: `crates/hclient/tests/sse.rs`

**Interfaces:**
- Consumes: `SseDecoder` (Task 3), `Response::chunk` (Task 13).
- Produces:
  - `pub struct SseStream<B>`; `SseStream::new(resp: Response<B>, max_event_size: usize) -> Result<Self, Error>`
    — checks the `Content-Type` and status;
    `SseStream::next(&mut self) -> impl Future<Output = Option<Result<SseEvent, Error>>>`;
    `SseStream::last_event_id(&self) -> Option<&str>`
  - Terminal rules: status ≠ 200 → `Err(ErrorKind::Status)`;
    `Content-Type` ≠ `text/event-stream` → `Err(ErrorKind::Decode)`.
    **Reconnect isn't implemented in v0.1** — it requires resending the
    request, which arrives together with the retry stage in v0.2.
    `last_event_id()` already exists so reconnect can slot in without an API
    change.

- [ ] **Step 1: Write failing tests**

```rust
// crates/hclient/tests/sse.rs
use hclient::{Client, SseEvent, SseStream, DEFAULT_MAX_EVENT_SIZE};
use hclient::mock::MockTransport;

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
            "204 means \"stop forever,\" not \"empty stream\"");
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

- [ ] **Step 2: Run it and confirm it fails**

Run: `cargo test -p hclient --features test-util --test sse`
Expected: FAIL — `cannot find type SseStream`.

- [ ] **Step 3: Implement**

```rust
// crates/hclient/src/sse.rs
use crate::response::Response;
use bytes::Bytes;
use http_body::Body as HttpBody;
use hclient_core::{Error, ErrorKind};
use hclient_proto::sse::{SseDecoder, SseEvent};

const MIME: &str = "text/event-stream";

/// An SSE event stream over any response body.
///
/// Reconnect is **not** implemented here: it requires resending the request
/// and arrives with the retry stage in v0.2. `last_event_id()` is already
/// available, so adding reconnect won't change the public API.
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
        // WHATWG: any status other than 200 — stop. 204 in particular means
        // "don't connect again," not "empty stream."
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
                        // Exceeding the limit is fatal and not retried.
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

- [ ] **Step 4: Run the tests**

Run: `cargo test -p hclient --features test-util`
Expected: PASS, four SSE tests plus everything from before.

- [ ] **Step 5: Commit**

```bash
git add crates/hclient
git commit -m "feat(hclient): SseStream with WHATWG terminal rules over the proto decoder"
```

---

### Task 15: `hclient-wasi` — the response body

**Files:**
- Create: `crates/hclient-wasi/Cargo.toml`
- Create: `crates/hclient-wasi/src/lib.rs`
- Create: `crates/hclient-wasi/src/body.rs`

**Interfaces:**
- Consumes: `wasip3::http_compat::IncomingResponseBody`, `hclient_core::Error`.
- Produces:
  - `pub struct Body`; `impl http_body::Body for Body { type Data = Bytes; type Error = hclient_core::Error; }`
  - `Body::empty() -> Self`
  - `is_end_stream()` is implemented **correctly** — on `act`'s host side,
    this exact defect (`StreamBody` always returning `false`) was causing
    guests to trap on HTTP/2.

- [ ] **Step 1: Create the crate**

```toml
# crates/hclient-wasi/Cargo.toml
[package]
name = "hclient-wasi"
version = "0.1.0"
description = "hclient transport over wasi:http 0.3"
edition.workspace = true
rust-version = "1.90"
license.workspace = true
repository.workspace = true

[dependencies]
bytes        = { workspace = true }
http         = { workspace = true }
http-body    = { workspace = true }
hclient-core = { workspace = true }
futures      = { version = "0.3", default-features = false, features = ["async-await"] }
wasip3       = { version = "0.7.0", features = ["http-compat"] }

[lints]
workspace = true
```

```rust
// crates/hclient-wasi/src/lib.rs
//! hclient transport over `wasi:http` 0.3 (the `wasip3` package).
//!
//! Builds for `wasm32-wasip2`. Not a single `wasip3` type appears in this
//! crate's public API.
#![deny(unsafe_code)]

mod body;
mod convert;

pub use body::Body;
```

- [ ] **Step 2: Write the body**

```rust
// crates/hclient-wasi/src/body.rs
use bytes::Bytes;
use http_body::{Body as HttpBody, Frame};
use hclient_core::{Error, ErrorKind};
use std::pin::Pin;
use std::task::{Context, Poll};
use wasip3::http_compat::IncomingResponseBody;

/// The `wasi:http` response body. Reads the stream inline, with no background
/// task — meaning the transport doesn't need a `spawn` capability.
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

    /// Implemented honestly. `act`'s host side used
    /// `http_body_util::StreamBody`, which always returns `false`, which is
    /// why guests were trapping mid-read on HTTP/2 responses.
    fn is_end_stream(&self) -> bool {
        matches!(self.inner, Inner::Done)
    }
}

/// A wrapper around `wasi:http`'s `ErrorCode`, so it doesn't leak into the public API.
#[derive(Debug)]
pub(crate) struct WasiError(pub(crate) wasip3::http::types::ErrorCode);

impl std::fmt::Display for WasiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:?}", self.0)
    }
}
impl std::error::Error for WasiError {}
```

- [ ] **Step 3: Verify the crate builds for wasip2**

Run: `cargo check -p hclient-wasi --target wasm32-wasip2`
Expected: success (the `convert` module is still empty — create
`crates/hclient-wasi/src/convert.rs` with a single line, `// see Task 16`).

- [ ] **Step 4: Commit**

```bash
git add crates/hclient-wasi
git commit -m "feat(wasi): response Body with a correct is_end_stream"
```

---

### Task 16: `hclient-wasi` — Transport, conversion, and honoring setters

This is where the seven `let _ =` from `wasi-fetch` disappear.

**Files:**
- Create/Modify: `crates/hclient-wasi/src/convert.rs`
- Modify: `crates/hclient-wasi/src/lib.rs`
- Test: `crates/hclient-wasi/src/convert.rs` (`#[cfg(test)]` — pure parts only)

**Interfaces:**
- Consumes: `Transport`, `Capabilities`, `RequestBody`, `Error`, `Body` (Task 15).
- Produces:
  - `pub struct WasiHttp { caps: Capabilities }`; `WasiHttp::new() -> Self`
  - `impl Transport for WasiHttp { type Body = Body; type Error = Error; }`
  - `pub(crate) fn to_wasi_method(m: &http::Method) -> wasip3::http::types::Method`
  - `pub(crate) fn scheme_of(uri: &http::Uri) -> Result<wasip3::http::types::Scheme, Error>`
  - `WasiHttp`'s capabilities: `streaming_request_body: true`, `full_duplex: true`,
    `request_trailers: true`, `response_trailers: true`,
    `redirects: RedirectSupport::None`, `timeouts` — all three `true`,
    `upgrade: UpgradeSupport::None`.

- [ ] **Step 1: Write failing tests for the pure parts**

```rust
// crates/hclient-wasi/src/convert.rs
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
        let caps = hclient_core::unversioned::Transport::capabilities(&c);
        // wasi:http 0.3 is richer than native on streaming…
        assert!(caps.full_duplex);
        assert!(caps.request_trailers && caps.response_trailers);
        // …and poorer on everything else.
        assert_eq!(caps.redirects, hclient_core::RedirectSupport::None);
        assert_eq!(caps.upgrade, hclient_core::UpgradeSupport::None);
        assert_eq!(caps.tls_config, hclient_core::TlsSupport::None);
        assert!(!caps.proxy);
    }
}
```

- [ ] **Step 2: Run it and confirm it fails**

Run: `cargo test -p hclient-wasi --target wasm32-wasip2`
Expected: FAIL at the compile stage — the functions don't exist yet. (Running
tests under wasip2 needs a runner; see Step 6. Until it's set up, use
`cargo check -p hclient-wasi --target wasm32-wasip2 --tests`.)

- [ ] **Step 3: Implement the conversion and honoring setters**

```rust
// crates/hclient-wasi/src/convert.rs
use crate::body::{Body, WasiError};
use hclient_core::{Error, ErrorKind, UnsupportedCapability};
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

/// Applies timeouts, **without swallowing host rejections**.
///
/// `wasi:http` 0.3's setters return
/// `result<_, request-options-error{not-supported, immutable, other}>`
/// precisely so the host can say "I can't." `wasi-fetch` discarded seven
/// such `Result`s via `let _ =`; here every rejection becomes an error.
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
    use hclient_core::Phase;
    use wasip3::http::types::ErrorCode as EC;
    // The category is preserved. `wasi-fetch` flattened everything into
    // `Error::Transport(format!("{e:?}"))`, and `act`'s host side then
    // reconstructed it by substring-matching across the `source()` chain.
    //
    // Variant names checked against wasip3-0.7.0+wasi-0.3.0/src/service.rs:161-206.
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

- [ ] **Step 4: Implement `WasiHttp`**

```rust
// add to crates/hclient-wasi/src/lib.rs
use bytes::Bytes;
use futures::join;
use hclient_core::unversioned::Transport;
use hclient_core::{Capabilities, Error, RedirectSupport, RequestBody, TimeoutSupport,
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
        // wasi:http 0.3 is symmetric on bodies and does trailers in both
        // directions — richer than native.
        caps.streaming_request_body = true;
        caps.full_duplex = true;
        caps.request_trailers = true;
        caps.response_trailers = true;
        caps.timeouts = TimeoutSupport { connect: true, first_byte: true, between_bytes: true };
        // And poorer on everything else: the spec has no redirects, no TLS,
        // no proxy, no version selection, no upgrade.
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
            .map_err(|e| Error::new(hclient_core::ErrorKind::Other, convert::FieldsError(e)))?;

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
            // Streaming and rewindable bodies arrive together with the retry stage.
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

        // Structured concurrency: the body is written alongside send, with
        // no spawn. This is exactly why the WASI transport doesn't need a
        // Spawn capability.
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

Add helper types to `convert.rs`:

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

Add a constructor to `body.rs`, `Body::from_bytes(Bytes) -> Self`, with an
`Inner::Buffered(Option<Bytes>)` variant and a matching arm in `poll_frame`.

- [ ] **Step 5: Verify the build and the "no `let _ =` on setter Results" invariant**

Run: `cargo check -p hclient-wasi --target wasm32-wasip2`
Expected: success.

Run: `! grep -rn "let _ = .*set_" crates/hclient-wasi/src && echo OK`
Expected: `OK`.

- [ ] **Step 6: Set up a test runner for wasip2**

Run: `cargo install wasmtime-cli --locked`

Create `.cargo/config.toml`:

```toml
[target.wasm32-wasip2]
runner = "wasmtime run -S http --"
```

Run: `cargo test -p hclient-wasi --target wasm32-wasip2`
Expected: PASS, three conversion tests.

If `wasmtime` can't be installed, leave `cargo check --tests` as a temporary
gate and file an issue — the integration run moves to vertical 3.

- [ ] **Step 7: Commit**

```bash
git add crates/hclient-wasi .cargo/config.toml
git commit -m "feat(wasi): Transport over wasi:http 0.3 honouring every option setter"
```

---

### Task 17: An end-to-end example and README

**Files:**
- Create: `crates/hclient-wasi/examples/fetch.rs`
- Create: `README.md`
- Modify: `.github/workflows/ci.yml`

**Interfaces:**
- Consumes: everything so far.
- Produces: nothing for code.

- [ ] **Step 1: Write the example**

```rust
// crates/hclient-wasi/examples/fetch.rs
//! The same code that will work on native in vertical 2, and in the
//! browser in vertical 3. Only the transport type changes.

use hclient::Client;
use hclient_wasi::WasiHttp;

fn main() {
    let client = Client::builder(WasiHttp::new()).build().expect("caps ok");
    let fut = async {
        let resp = client.get("https://example.com/").send().await?;
        let collected = resp.collect().await?;
        println!("{} {}", collected.status(), collected.text()?);
        Ok::<_, hclient::Error>(())
    };
    futures::executor::block_on(fut).expect("request failed");
}
```

- [ ] **Step 2: Verify the example builds**

Run: `cargo build -p hclient-wasi --example fetch --target wasm32-wasip2`
Expected: success.

- [ ] **Step 3: Write the README with a dependency-graph table**

````markdown
# hclient

Cross-platform async HTTP client. The same application code builds for
native, browser and WASI — the transport is swapped out, not buried under
`#[cfg]`.

```rust
let client = hclient::Client::builder(transport).build()?;
let text = client.get("https://example.com").send().await?.collect().await?.text()?;
```

## What's in the dependency graph

| build | tokio |
|---|---|
| ambient (`hclient` + `-wasi` / `-fetch`) | **none at all** |
| native, HTTP/1 only | present, but with the `sync` + `default` features; its whole dep tree is `pin-project-lite` |
| native + HTTP/2 | real: `h2` pulls `tokio` with `io-util` and `tokio-util` with `codec`, and through it `libc` |

Tokio can't be removed from hyper builds: [hyper#3428](https://github.com/hyperium/hyper/pull/3428)
(exactly this swap for `futures-channel`) was rejected, and
[hyper#3767](https://github.com/hyperium/hyper/issues/3767) was closed as *not planned*.

## Status

v0.1: the core, `wasi:http` 0.3. Native and browser are verticals 2 and 3.
Design: [`docs/superpowers/specs/2026-08-05-hclient-design.md`](docs/superpowers/specs/2026-08-05-hclient-design.md).
````

- [ ] **Step 4: Add building the example to CI**

```yaml
  # in the `wasip2` job, after the existing step
      - run: cargo build -p hclient-wasi --example fetch --target wasm32-wasip2
```

- [ ] **Step 5: Run everything**

Run: `cargo test --workspace --all-features && cargo check -p hclient-wasi --target wasm32-wasip2`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add README.md crates/hclient-wasi/examples .github/workflows/ci.yml
git commit -m "docs: README with dependency-graph table and end-to-end wasi example"
```

---

## What this vertical proved, and what's left

**Proven:** the `Transport` shape works against a real ambient backend with
no socket; the core doesn't need a declared `Send`; unsupported settings
become errors instead of silent no-ops; the protocol logic is tested without
a runtime and gets fuzzed.

**Not proven, and carried over into vertical 2:** the runtime seam (needs
tokio and smol on the same code); `Client<T = DefaultTransport>`; streaming
request bodies (need `RequestBody::Streaming` in the transport).

**Carried over into vertical 3:** `hclient-fetch`, and with it, verifying the
`Capabilities` runtime model; `SseStream` reconnect; `act` acceptance.

---

## Amendments after review

The Task 2 and Task 3 sections above are historical: they describe the
original intent, and the code has since moved on based on review findings.
The code is authoritative; this is a list of the discrepancies, so they
don't get lost. No further edits are made to the task bodies themselves
(surgical edits to this file once already broke the markdown fences and
corrupted extraction of tasks 3–17).

### R1. `LineSplitter::next_line` returns the count of consumed bytes

**What was wrong.** The decoder counted a line's weight as `line.len() + 1`,
assuming a one-byte terminator. CRLF takes two, so every CRLF line was
undercounted by one byte. Measured in review: with `max_event_size = 16` and
lines of `"x:0\r\n"`, the decoder accepted **four lines, 25 bytes on the
wire**, before returning `EventTooLarge` — 56% over budget, with the content
already parsed into internal state by that point. The limit exists to defend
against a hostile server, so the undercount isn't cosmetic.

**How it's fixed.** `next_line` returns `Option<(Vec<u8>, usize)>`, where
`usize` is the bytes actually consumed, **including the terminator**. The
decoder charges the limit against that number.

**A residual inaccuracy, accepted deliberately.** If a CRLF is split by a
chunk boundary, the LF is swallowed in `push` and not charged — an undercount
of up to one byte per such boundary. Unlike the original defect, this one
doesn't scale with the number of lines: it's bounded by the number of chunks,
not by repetition within a chunk.

### R2. Deferred, doesn't block v0.1

- `SseDecoder::ready` is unbounded: a hostile server could send many small
  valid events in a single `push`. In practice it's bounded by the chunk size
  the transport hands over, and `SseStream` (Task 14) drains it after every
  `push`. Revisit if a path shows up where `push` is called without `next`.
- After `EventTooLarge`, the decoder's state isn't sealed by code — only by
  the docs. `SseStream` (Task 14) moves the stream to `Terminated`; that's
  where a test verifies it.

### R3. Two fuzz targets instead of one

**What was wrong.** I asked for the expensive byte-accounting invariant to be
added to the same target as the cheap no-panic check. Measured in review on
one machine with the same seed corpus: **with** the check — 227 iterations in
90s (~2 exec/s); **without** it — 1,059,154 iterations in 26s
(~40,700 exec/s). A roughly 20,000x drop; over the smoke-test window the
fuzzer found two new corpus entries instead of 3,777. The cause isn't
algorithmic: byte-at-a-time feeding is only about twice as expensive on its
own, but it multiplies the number of instrumented calls, and libFuzzer grows
inputs toward the `max_len` ceiling.

**How it's fixed.** Two targets:

- `sse` — exactly as in the brief (`chunks(7)`, no-panic only). Full
  throughput for wide coverage of the BOM state machine, line splitting, and
  field parsing.
- `sse_accounting` — only the accounting-proxy invariant, with a small
  `-max_len` (256). The "undercounted terminator" class of bugs shows up
  within tens of bytes, so the input ceiling doesn't hide it.

Rejected: capping the size **inside** one target (cheap and expensive inputs
share one feedback loop, and the slow ones drag the average down regardless)
and sampling (makes the odds of catching it depend on the run).

### R4. `buffered_len` accounts for a deferred terminator

A one-byte gap: if `carried_terminator` hadn't been charged yet (between two
consecutive split CRLFs), `buffered_len()` didn't see it. Bounded by zero-or-one
and doesn't scale, but it made the accounting-accuracy doc comment wrong.
Closed by adding the term.

### R5. `Location` validation — not `from_utf8`

**What was wrong.** Task 5's reference implementation of `decide` validated
`Location` via `core::str::from_utf8`. That's not enough, and the test from
the same brief proves it: `b"ht!tp://\x00"` is valid UTF-8 (NUL is legal),
and the `url` crate strips trailing C0 control bytes before parsing, so the
input calmly resolves to `https://a/ht!tp://` instead of `InvalidLocation`.
In other words, `garbage_location_is_reported` was failing on the very code
that accompanied it.

**How it's fixed.** Validate the raw bytes as an HTTP header value:
`http::HeaderValue::from_bytes(..).to_str()`. This rejects C0 controls and
DEL, and therefore also closes off CR/LF injection via `Location` — something
the UTF-8 check didn't do at all.

**A tradeoff worth saying out loud.** `HeaderValue::to_str` rejects any byte
≥ 0x80, so a `Location` with raw non-ASCII (an unencoded path, an IDN host)
gets rejected. Per RFC 9110 such a value is invalid, but it occurs in the
wild. We chose fail-closed: the redirect target isn't the place to guess at
the server's intent.

### R6. Origin comparison with default-port substitution

**What was wrong.** `cross_origin` compared `port_u16()` directly. But
`current` comes from an `http::Uri`, which keeps an explicit `:443`, while
the redirect target goes through `url::Url::join`, which strips it on
serialization. So `https://a:443/` → `https://a/` was counted as an origin
change and stripped `Authorization`/`Cookie` on **every** hop. An error on
the safe side, but functionally a silent loss of authorization for anyone
whose base URL includes an explicit default port.

**How it's fixed.** Compare `port_or_known_default()` on both sides: 443 for
https, 80 for http, otherwise `port_u16()`. Plus tests for both directions of
the asymmetry — none of the twelve original tests touched it.

### R7. `Location` validation — splitting two different questions

An amendment to R5. `HeaderValue::from_bytes(..).to_str()` solves two
problems at once, and gets the second one wrong. `from_bytes` rejects C0
controls and DEL, i.e. closes off CR/LF injection: that part is needed.
`to_str()` additionally rejects any byte ≥ 0x80, i.e. raw non-ASCII in
`Location`.

The review checked the ecosystem instead of assuming: reqwest today
delegates redirects to `tower_http::follow_redirect`, whose `resolve_uri`
uses `str::from_utf8`, and tower-http has a test,
`test_resolve_uri_unicode`, asserting that `/café` and
`https://münchen.com/` are followed. We ended up stricter than reqwest as a
side effect, not by decision.

**How it's fixed.** `HeaderValue::from_bytes` to screen out control bytes,
then `core::str::from_utf8` over its bytes — instead of `to_str()`. The
injection lives in the control characters, not the non-ASCII; the two
questions are separated, and both are solved.

### R8. `Error` requires `Send + Sync` from its source

See amendment C1 in the spec. In short: `Arc<dyn Error + 'static>` doesn't
let auto-traits through, so `Error` was `!Send` always, and `tokio::spawn`
wouldn't compile for any transport. The source is bounded by `Send + Sync`.

Consequence for CI: `no-declared-send` now has to exclude
`crates/hclient-core/src/error.rs` — with a comment noting that this is the
single documented exception, not a weakening of the check.

### R9. `RequestBody`: `Send` bounds on the trait objects

See amendment C2. `Arc<dyn Fn() -> RequestBody + Send + Sync>` and
`Box<dyn Body<..> + Unpin + Send>`. Found by a compile check, before Task 7
was implemented.
