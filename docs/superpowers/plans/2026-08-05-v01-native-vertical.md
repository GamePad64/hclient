> **`Transport::to_error` and `Native`.** Vertical 1 (finding B2 of its final
> review) added a default hook to the seam,
> `fn to_error(&self, e: Self::Error) -> Error`, that turns a backend error
> into a library error. `Native` has `type Error = Error`, meaning the
> category is ALREADY assigned at the point where the failure occurred
> (`ErrorKind::Resolve` in `execute`, `Connect` in `race_connect`, `Tls` in
> `TlsConnect`, `Body` in `h1`).
>
> **It must not be lost.** The hook's default first checks whether
> `Self::Error` is exactly `hclient_core::Error`, and if so, passes it
> straight through. So for `Native` the correct behavior is the default
> behavior, and "forgot to override it" has stopped being a defect. The
> first version of the hook wrapped unconditionally, and a forgetful backend
> would silently lose its entire taxonomy: `kind()` became `Other`, the
> `is_*` predicates all became `false` at once, `Display` printed the
> category twice. Prose was the safeguard; fix round 3 replaced it with a
> mechanism.
>
> **Overriding it is still required — explicitly, as an identity function**
> (Step 3 below). Not for correctness, but for readability: an explicit `fn
> to_error(&self, e: Self::Error) -> Error { e }` states the intent right
> where it's read, and survives a possible future change to the default.
> And the test from Step 1 is mandatory: it doesn't verify the default (that's
> covered in `hclient-core`) — it verifies that `Native`'s category actually
> makes it to the caller through the whole `Client::execute` path.
>
> **The default doesn't cover** a backend whose error is ITS OWN type,
> carrying the category inside: it can't guess a foreign enum, and without an
> override such an error becomes `ErrorKind::Other`. That doesn't apply to
> `Native`, but it applies to any future backend that decides to grow its own
> error type.

# hclient v0.1, vertical 2: native — implementation plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** The same application code as in vertical 1 goes over a real
network — TCP + TLS + HTTP/1.1 — and works **on tokio and on smol** with not
a single `#[cfg]` in the shared code.

**Architecture:** The runtime isn't broken into one `Runtime`, but into
separate capabilities (`Spawn`, `TcpConnect`, `TcpAdoptStd`, `Blocking`), so
the transport only requires what it actually uses. The TLS adapter is
written directly against `hyper::rt::Read/Write`, not futures-io or
tokio-io — so per-runtime TLS glue simply doesn't exist. The HTTP/1
connection is driven **inline**, with no spawn: this proves the runtime seam
is real, because the client runs on a bare `futures` executor.

**Tech Stack:** `hyper` 1.11 (`client` + `http1` only), `rustls` 0.23,
`rustls-pki-types` 1.15, `rustls-platform-verifier` 0.7, `webpki-roots` 1.0,
`socket2` 0.6, `tokio` 1 (`net`,`rt`,`time`,`sync`), `smol` / `async-io` 2 +
`async-net` 2, `futures` 0.3.

## Global Constraints

Inherited from vertical 1's plan, and extended:

- **`hclient-core` and `hclient` still don't contain a single declared
  `Send`/`Sync` bound.** Every `Send` requirement is locked inside
  `hclient-native`, `hclient-tls-*`, `hclient-dns-hickory`, and comes from
  someone else's code.
- **`hclient-rt` itself doesn't choose a runtime and contains no
  runtime-specific code.** Its direct dependencies are only `hyper` (for the
  `rt` traits) and `futures-io`; implementations live in `-rt-tokio` /
  `-rt-smol`. This is about direct dependencies, NOT the whole graph: hyper
  1.11.0 itself pulls in `tokio` (`features = ["sync"]`) unconditionally,
  with no `optional = true` — reproduced by building: `cargo clean -p tokio
  && cargo build -p hclient-rt` prints `Compiling tokio v1.53.1`. Vertical
  1's README already documents the `sync` feature honestly; there's no
  contradiction, only that the earlier phrasing read like a claim about the
  whole graph (found by Task 2's review, round 1).
- **Not a single hyper, rustls or socket2 type appears in `hclient-native`'s
  public API.** `hyper::upgrade::Upgraded` is under a special ban.
- **`unsafe` is forbidden everywhere** (`#![forbid(unsafe_code)]`, not
  `deny` — `deny` we could override with a local `#[allow(unsafe_code)]`
  inside the crate, `forbid` we can't; see Task 2 fix round 1). The
  futures-io → hyper::rt shim is written using the safe
  `ReadBufCursor::put_slice`, as hyper's own documentation recommends.
- **A backend whose `Transport::Error` is ITS OWN type, carrying the
  category inside, must override `Transport::to_error`.** The default only
  recognizes `hclient_core::Error` (which it passes straight through — i.e.
  `Native`, with `type Error = Error`, is structurally protected); it can't
  guess a foreign enum, and without an override such an error becomes
  `ErrorKind::Other`. Overriding with an identity function is worthwhile even
  where the default is already correct — the explicit line states the
  intent. Details are in the block above Step 1 of Task 13, and in the doc
  comment on the method itself in `hclient-core`.
- Connection pooling is **not part of** this vertical: one connection per
  request.
- MSRV: 1.85. Every crate in this vertical has `rust-version = "1.85"`. The
  `msrv` CI job runs `cargo check --all-features --all-targets` (with
  `--all-targets` — as of vertical 1's final-review fix round; before that,
  test targets had never once been checked against 1.85), but its package
  list is the three core crates. This vertical's crates need to be added to it.

## File layout

```
crates/hclient-rt/
  src/lib.rs                 re-export Timer from core, TcpOpts
  src/caps.rs                Spawn, TcpConnect, TcpAdoptStd, Blocking
  src/futures_io.rs          FuturesIo<S>: futures-io -> hyper::rt
crates/hclient-rt-tokio/src/lib.rs
crates/hclient-rt-smol/src/lib.rs
crates/hclient-dns/
  src/lib.rs                 Resolve, ResolvedAddr, SvcbEndpoint
crates/hclient-dns-system/src/lib.rs
crates/hclient-tls/
  src/lib.rs                 TlsConnect, TlsRequest, TlsInfo
crates/hclient-tls-rustls/
  src/lib.rs                 Rustls: TlsConnect
  src/stream.rs              TlsStream<S>: hyper::rt::Read + Write
crates/hclient-native/
  src/lib.rs                 Native<R, T, D>: Transport
  src/connect.rs             Happy Eyeballs + TCP + TLS + ALPN
  src/h1.rs                  handshake + driving the connection inline
  src/body.rs                RequestBody -> http_body::Body bridge with a Send error
crates/hclient-proto/
  src/happy_eyeballs.rs      pure RFC 8305 scheduler (task 5)
crates/hclient/
  src/lib.rs                 DefaultTransport + Client<T = DefaultTransport>
```

---

### Task 1: `hclient-rt` — separate runtime capabilities

**Files:**
- Create: `crates/hclient-rt/Cargo.toml`, `src/lib.rs`, `src/caps.rs`
- Test: inside `caps.rs`

**Interfaces:**
- Consumes: `hclient_core::unversioned::Timer`.
- Produces:
  - `pub trait Spawn<F: Future<Output = ()>> { fn spawn(&self, f: F); }`
  - `pub trait TcpConnect { type Stream: hyper::rt::Read + hyper::rt::Write + Unpin; fn connect(&self, addr: SocketAddr, opts: &TcpOpts) -> impl Future<Output = std::io::Result<Self::Stream>>; }`
  - `pub trait TcpAdoptStd: TcpConnect { fn adopt(&self, std: std::net::TcpStream) -> std::io::Result<Self::Stream>; }`
  - `pub trait Blocking { fn run<T, F: FnOnce() -> T>(&self, f: F) -> impl Future<Output = T>; }`
  - `pub struct TcpOpts { pub nodelay: bool, pub keepalive: Option<Duration>, pub local_address: Option<IpAddr>, pub send_buffer_size: Option<usize>, pub recv_buffer_size: Option<usize>, pub reuse_address: bool }` (`Default`)
  - `pub use hclient_core::unversioned::Timer;`

- [ ] **Step 1: Write a failing test**

```rust
// crates/hclient-rt/src/caps.rs
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tcp_opts_default_is_conservative() {
        let o = TcpOpts::default();
        assert!(!o.nodelay, "the user turns nodelay on, not us");
        assert!(o.keepalive.is_none());
        assert!(o.local_address.is_none());
        assert!(!o.reuse_address);
    }

    #[test]
    fn spawn_is_generic_over_the_future_not_boxed() {
        // The shape is copied from hyper::rt::Executor: generic over F, zero
        // bounds in the declaration. Send comes from the impl, not the trait.
        struct Immediate;
        impl<F: std::future::Future<Output = ()>> Spawn<F> for Immediate {
            fn spawn(&self, f: F) { futures_executor::block_on(f) }
        }
        let done = std::rc::Rc::new(std::cell::Cell::new(false));
        let d = done.clone();
        // A !Send future — the trait allows it.
        Immediate.spawn(async move { d.set(true) });
        assert!(done.get());
    }
}
```

- [ ] **Step 2: Run it and confirm it fails**

Run: `cargo test -p hclient-rt`
Expected: FAIL — the crate doesn't exist.

- [ ] **Step 3: Create the crate**

```toml
# crates/hclient-rt/Cargo.toml
[package]
name = "hclient-rt"
version = "0.1.0"
description = "Runtime capabilities needed by hclient's native transport"
edition.workspace = true
rust-version.workspace = true
license.workspace = true
repository.workspace = true

[dependencies]
futures-io   = { version = "0.3", default-features = false, features = ["std"] }
hclient-core = { workspace = true }
hyper        = { version = "1.11", default-features = false }

[dev-dependencies]
futures-executor = { version = "0.3", default-features = false, features = ["std"] }

[lints]
workspace = true
```

```rust
// crates/hclient-rt/src/lib.rs
//! Runtime capabilities for hclient's native transport.
//!
//! Separate traits, not one `Runtime`: the transport only requires what it
//! actually uses, and a backend with no sockets isn't forced to implement
//! `connect` as a stub that panics.
#![deny(unsafe_code)]

mod caps;
mod futures_io;

pub use caps::{Blocking, Spawn, TcpAdoptStd, TcpConnect, TcpOpts};
pub use futures_io::FuturesIo;

/// `Timer` is defined once, in `hclient-core`: the portable core needs it
/// for timeouts and backoff. This is just a re-export.
pub use hclient_core::unversioned::Timer;
```

```rust
// crates/hclient-rt/src/caps.rs
use std::future::Future;
use std::net::{IpAddr, SocketAddr};
use std::time::Duration;

/// The shape is deliberately copied from `hyper::rt::Executor`: generic
/// over the future, zero bounds in the declaration. `Send` comes from the
/// `impl`, not the trait, so single-threaded runtimes can implement it
/// honestly.
pub trait Spawn<F: Future<Output = ()>> {
    fn spawn(&self, f: F);
}

/// Socket options get applied in hclient **exactly once**, on a
/// `socket2::Socket`, and the runtime only adopts the descriptor
/// (`TcpAdoptStd`). Otherwise every runtime crate would rewrite this whole
/// sheet of options over again.
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

/// On platforms with file descriptors, the whole set of socket options gets
/// applied outside the runtime, and the runtime only adopts the finished
/// socket.
pub trait TcpAdoptStd: TcpConnect {
    fn adopt(&self, std: std::net::TcpStream) -> std::io::Result<Self::Stream>;
}

/// A separate trait, not a method: `getaddrinfo` is blocking, and on wasm
/// and embedded there's no blocking pool at all. The absence of the
/// capability should be a compile error, not `unimplemented!()` at runtime.
///
/// **The only place in the entire project where we declare `Send`
/// ourselves**, and it's honest here: both `tokio::task::spawn_blocking` and
/// `blocking::unblock` require `Send + 'static`, and the `Blocking`
/// capability doesn't exist on wasm at all — there's nothing for it to infect.
pub trait Blocking {
    fn run<T: Send + 'static, F: FnOnce() -> T + Send + 'static>(
        &self,
        f: F,
    ) -> impl Future<Output = T>;
}
```

- [ ] **Step 4: Create a `futures_io.rs` stub and run the tests**

```rust
// crates/hclient-rt/src/futures_io.rs
// Implementation — Task 2.
pub struct FuturesIo<S> { pub(crate) inner: S }
```

Run: `cargo test -p hclient-rt`
Expected: PASS, two tests.

- [ ] **Step 5: Commit**

```bash
git add crates/hclient-rt
git commit -m "feat(rt): separate runtime capability traits instead of one Runtime"
```

---

### Task 2: `hclient-rt` — a `futures-io` → `hyper::rt` shim

This bridge doesn't exist anywhere: hyper-util only has `TokioIo`, and
`smol-hyper` 0.1.1 has been dead since 2023-12-29 **and** bridges in the
wrong direction. Without it, no smol backend exists.

**Files:**
- Modify: `crates/hclient-rt/src/futures_io.rs`
- Test: inside `futures_io.rs`

**Interfaces:**
- Consumes: `futures_io::{AsyncRead, AsyncWrite}`.
- Produces: `pub struct FuturesIo<S>`; `FuturesIo::new(inner: S) -> Self`;
  `FuturesIo::into_inner(self) -> S`;
  `impl<S: AsyncRead + Unpin> hyper::rt::Read for FuturesIo<S>`;
  `impl<S: AsyncWrite + Unpin> hyper::rt::Write for FuturesIo<S>`.

- [ ] **Step 1: Write failing tests**

```rust
// crates/hclient-rt/src/futures_io.rs
#[cfg(test)]
mod tests {
    use super::*;
    use futures_executor::block_on;
    use std::pin::Pin;

    /// A source that hands out data in portions, to catch partial reads.
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
        // step is bigger than the buffer's capacity: put_slice must not panic.
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

- [ ] **Step 2: Run it and confirm it fails**

Run: `cargo test -p hclient-rt futures_io`
Expected: FAIL — `no function new`.

- [ ] **Step 3: Implement**

```rust
// crates/hclient-rt/src/futures_io.rs
use hyper::rt::ReadBufCursor;
use std::pin::Pin;
use std::task::{Context, Poll};

/// A bridge from `futures_io::{AsyncRead, AsyncWrite}` to `hyper::rt::{Read, Write}`.
///
/// hyper-util only has `TokioIo`; `smol-hyper` 0.1.1 has been dead since
/// 2023-12-29 and bridges the opposite direction. So this bridge is ours.
///
/// Implemented **with no `unsafe`**: we read into a temporary stack buffer
/// and copy through the safe `ReadBufCursor::put_slice` — exactly the
/// technique `hyper::rt::Read`'s own documentation recommends. The price is
/// one copy per read; zero-copy would need `unsafe as_mut`/`advance` and is
/// deferred.
#[derive(Debug)]
pub struct FuturesIo<S> {
    inner: S,
}

/// The size of the stack buffer. 8 KiB is a typical read size for hyper,
/// so no extra iterations occur.
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

- [ ] **Step 4: Run the tests**

Run: `cargo test -p hclient-rt`
Expected: PASS, five tests.

- [ ] **Step 5: Verify there's really no unsafe**

Run: `! grep -rn "unsafe" crates/hclient-rt/src && echo OK`
Expected: `OK`.

- [ ] **Step 6: Commit**

```bash
git add crates/hclient-rt
git commit -m "feat(rt): safe futures-io to hyper::rt bridge, missing from the ecosystem"
```

---

### Task 3: `hclient-rt-tokio`

**Files:**
- Create: `crates/hclient-rt-tokio/Cargo.toml`, `src/lib.rs`
- Test: inside `lib.rs`

**Interfaces:**
- Consumes: Task 1's traits, `FuturesIo` from Task 2.
- Produces: `pub struct Tokio`; `impl Timer for Tokio { type Instant = tokio::time::Instant }`;
  `impl<F: Future<Output=()> + Send + 'static> Spawn<F> for Tokio`;
  `impl TcpConnect for Tokio { type Stream = TokioIo }`; `impl TcpAdoptStd for Tokio`;
  `impl Blocking for Tokio`; `pub struct TokioIo(tokio::net::TcpStream)` —
  implements `hyper::rt::Read/Write` directly (without `FuturesIo`, because
  tokio has its own IO traits).

- [ ] **Step 1: Write failing tests**

```rust
// crates/hclient-rt-tokio/src/lib.rs
#[cfg(test)]
mod tests {
    use super::*;
    use hclient_rt::{Blocking, TcpConnect, TcpOpts, Timer};
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

- [ ] **Step 2: Run it and confirm it fails**

Run: `cargo test -p hclient-rt-tokio`
Expected: FAIL — the crate doesn't exist.

- [ ] **Step 3: Create the crate and implement**

```toml
# crates/hclient-rt-tokio/Cargo.toml
[package]
name = "hclient-rt-tokio"
version = "0.1.0"
description = "tokio implementation of hclient's runtime capabilities"
edition.workspace = true
rust-version.workspace = true
license.workspace = true
repository.workspace = true

[dependencies]
hclient-rt = { path = "../hclient-rt", version = "0.1.0" }
hyper      = { version = "1.11", default-features = false }
socket2    = { version = "0.6", features = ["all"] }
tokio      = { version = "1", features = ["net", "rt", "time", "sync"] }

[dev-dependencies]
tokio = { version = "1", features = ["macros", "rt-multi-thread"] }

[lints]
workspace = true
```

```rust
// crates/hclient-rt-tokio/src/lib.rs
//! tokio implementation of `hclient-rt`'s capabilities.
#![deny(unsafe_code)]

mod io;

pub use io::TokioIo;

use hclient_rt::{Blocking, Spawn, TcpAdoptStd, TcpConnect, TcpOpts, Timer};
use std::future::Future;
use std::net::SocketAddr;
use std::time::Duration;

/// A ZST: the tokio handle is taken from the ambient runtime, the same way
/// reqwest does it. Outside a runtime, `spawn`/`sleep` panic — documented.
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
        // The options get applied on a `socket2::Socket` **exactly once**,
        // and the runtime adopts the finished descriptor. This is the
        // `TcpAdoptStd` seam: without it, every runtime crate would rewrite
        // this whole sheet of options over again.
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
// crates/hclient-rt-tokio/src/io.rs
use hyper::rt::ReadBufCursor;
use std::pin::Pin;
use std::task::{Context, Poll};
use tokio::io::{AsyncRead, AsyncWrite};

/// A bridge from `tokio::net::TcpStream` to `hyper::rt`. No `unsafe`: we
/// read into a temporary buffer and copy through the safe `put_slice`.
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

- [ ] **Step 4: Run the tests**

Run: `cargo test -p hclient-rt-tokio`
Expected: PASS, three tests.

- [ ] **Step 5: Commit**

```bash
git add crates/hclient-rt-tokio
git commit -m "feat(rt-tokio): tokio implementation of the runtime capabilities"
```

---

### Task 4: `hclient-rt-smol`

The same set of capabilities on smol. This is exactly the task that proves
the seam is real: if a `#[cfg]` is needed in the shared code here, the seam
is decorative.

**Files:**
- Create: `crates/hclient-rt-smol/Cargo.toml`, `src/lib.rs`
- Test: inside `lib.rs`

**Interfaces:**
- Consumes: the same as Task 3, plus `FuturesIo` (smol's IO is already
  `futures-io`, so a separate `SmolIo` isn't needed).
- Produces: `pub struct Smol`; `impl Timer for Smol { type Instant = std::time::Instant }`;
  `impl<F: Future<Output=()> + Send + 'static> Spawn<F> for Smol`;
  `impl TcpConnect for Smol { type Stream = FuturesIo<async_net::TcpStream> }`;
  `impl TcpAdoptStd for Smol`; `impl Blocking for Smol`.

- [ ] **Step 1: Write failing tests**

```rust
// crates/hclient-rt-smol/src/lib.rs
#[cfg(test)]
mod tests {
    use super::*;
    use hclient_rt::{Blocking, TcpConnect, TcpOpts, Timer};
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

- [ ] **Step 2: Run it and confirm it fails**

Run: `cargo test -p hclient-rt-smol`
Expected: FAIL — the crate doesn't exist.

- [ ] **Step 3: Create the crate and implement**

```toml
# crates/hclient-rt-smol/Cargo.toml
[package]
name = "hclient-rt-smol"
version = "0.1.0"
description = "smol implementation of hclient's runtime capabilities"
edition.workspace = true
rust-version.workspace = true
license.workspace = true
repository.workspace = true

[dependencies]
async-net     = "2"
async-io      = "2"
blocking      = "1"
futures-lite  = "2"
hclient-rt    = { path = "../hclient-rt", version = "0.1.0" }
socket2       = { version = "0.6", features = ["all"] }

[dev-dependencies]
futures-executor = { version = "0.3", default-features = false, features = ["std"] }

[lints]
workspace = true
```

```rust
// crates/hclient-rt-smol/src/lib.rs
//! smol implementation of `hclient-rt`'s capabilities.
//!
//! **No `async-compat`.** It spins up a second runtime in the process if a
//! tokio context isn't found — which hides exactly the problem this vertical
//! is supposed to expose.
#![deny(unsafe_code)]

use hclient_rt::{Blocking, FuturesIo, Spawn, TcpAdoptStd, TcpConnect, TcpOpts, Timer};
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
        // `detach` is deliberate: the task's lifetime is tied to the
        // connection, not to the caller.
        smol_spawn(f);
    }
}

fn smol_spawn<F: Future<Output = ()> + Send + 'static>(f: F) {
    static EXEC: std::sync::OnceLock<async_executor::Executor<'static>> =
        std::sync::OnceLock::new();
    let ex = EXEC.get_or_init(|| {
        let ex = async_executor::Executor::new();
        std::thread::Builder::new()
            .name("hclient-smol".into())
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
        // `blocking` is the same pool smol itself uses.
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

Add `async-executor = "1"` to the dependencies. The `Send + 'static` bounds
on `Blocking::run` already exist from Task 1 — `blocking::unblock` requires them.

- [ ] **Step 4: Run the tests**

Run: `cargo test -p hclient-rt-smol`
Expected: PASS, three tests.

- [ ] **Step 5: Verify `async-compat` hasn't shown up in the graph**

Run: `! cargo tree -p hclient-rt-smol -e normal --prefix none | grep -q async-compat && echo OK`
Expected: `OK`.

- [ ] **Step 6: Commit**

```bash
git add crates/hclient-rt-smol crates/hclient-rt
git commit -m "feat(rt-smol): smol implementation without async-compat"
```

---

### Task 5: `hclient-proto` — the Happy Eyeballs scheduler (RFC 8305)

Nothing off the shelf: `happy-eyeballs` 0.2.1 has been dead since 2023-05,
`happyeyeballs` declares itself non-RFC-compliant, and hyper-util implements
**RFC 6555** behind a sealed trait. The scheduler is pure and takes `now` as
a parameter — meaning the 50ms and 250ms constants are tested **without a
single `sleep`**.

**Files:**
- Create: `crates/hclient-proto/src/happy_eyeballs.rs`
- Modify: `crates/hclient-proto/src/lib.rs`
- Test: inside `happy_eyeballs.rs`

**Interfaces:**
- Consumes: nothing.
- Produces:
  - `pub struct HeConfig { pub resolution_delay: Duration, pub attempt_delay: Duration, pub first_family_count: usize }` (`Default` = 50ms / 250ms / 1)
  - `pub struct Scheduler`; `Scheduler::new(cfg: HeConfig) -> Self`
  - `Scheduler::offer_v6(&mut self, addrs: &[IpAddr])`, `offer_v4(&mut self, addrs: &[IpAddr])`
  - `Scheduler::mark_v6_done(&mut self)`, `mark_v4_done(&mut self)`
  - `Scheduler::poll(&mut self, elapsed: Duration) -> HeAction`
  - `pub enum HeAction { Start(IpAddr), Wait(Duration), Exhausted }`

- [ ] **Step 1: Write failing tests**

```rust
// crates/hclient-proto/src/happy_eyeballs.rs
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
        // AAAA hasn't arrived yet: RFC 8305 §3 says wait the Resolution Delay.
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
        assert_eq!(Scheduler::new(c).config().attempt_delay, ms(10), "RFC 8305's lower bound");
        let c = HeConfig { attempt_delay: Duration::from_secs(30), ..Default::default() };
        assert_eq!(Scheduler::new(c).config().attempt_delay, Duration::from_secs(2),
                   "RFC 8305's upper bound");
    }
}
```

- [ ] **Step 2: Run it and confirm it fails**

Run: `cargo test -p hclient-proto happy`
Expected: FAIL — `cannot find type Scheduler`.

- [ ] **Step 3: Implement**

```rust
// crates/hclient-proto/src/happy_eyeballs.rs
//! The Happy Eyeballs v2 scheduler (RFC 8305). Pure: time comes in as the
//! `elapsed` parameter, so the constants are tested without `sleep`.

use core::time::Duration;
use std::collections::VecDeque;
use std::net::IpAddr;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HeConfig {
    /// RFC 8305 §3: how long to wait for AAAA before going with A.
    pub resolution_delay: Duration,
    /// RFC 8305 §5: pause between attempt starts. Clamped to 10ms…2s.
    pub attempt_delay: Duration,
    /// RFC 8305 §4: how many addresses of the first family go in a row.
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
    /// How many addresses of the first family have been handed out in a row so far.
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
        // Pause between attempts.
        if let Some(last) = self.last_start {
            let next_at = last + self.cfg.attempt_delay;
            if elapsed < next_at {
                return HeAction::Wait(next_at - elapsed);
            }
        }

        // RFC 8305 §3: while AAAA hasn't arrived and the resolver isn't
        // done, hold IPv4 back for the Resolution Delay.
        if self.v6.is_empty() && !self.v6_done && elapsed < self.cfg.resolution_delay {
            return HeAction::Wait(self.cfg.resolution_delay - elapsed);
        }

        let take_v6 = if self.v6.is_empty() {
            false
        } else if self.v4.is_empty() {
            true
        } else if self.started == 0 {
            true // IPv6 always goes first
        } else {
            // Interleaving: after First Address Family Count addresses of
            // the first family, alternate.
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

- [ ] **Step 4: Wire it up and run**

Add `pub mod happy_eyeballs;` to `crates/hclient-proto/src/lib.rs`.

Run: `cargo test -p hclient-proto`
Expected: PASS, six Happy Eyeballs tests plus everything from vertical 1.

- [ ] **Step 5: Add a fuzz target**

```rust
// crates/hclient-proto/fuzz/fuzz_targets/happy_eyeballs.rs
#![no_main]
use libfuzzer_sys::fuzz_target;
use hclient_proto::happy_eyeballs::{HeAction, HeConfig, Scheduler};
use std::net::{IpAddr, Ipv4Addr};
use core::time::Duration;

// Invariant: the scheduler always converges to Exhausted and never panics.
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

Add a second `[[bin]]` to `crates/hclient-proto/fuzz/Cargo.toml`.

Run: `cd crates/hclient-proto/fuzz && cargo +nightly fuzz run happy_eyeballs -- -max_total_time=60`
Expected: 60 seconds with no panics.

- [ ] **Step 6: Commit**

```bash
git add crates/hclient-proto
git commit -m "feat(proto): RFC 8305 Happy Eyeballs scheduler testable without sleeping"
```

---

### Task 6: `hclient-dns` — the resolver trait

**Files:**
- Create: `crates/hclient-dns/Cargo.toml`, `src/lib.rs`
- Test: inside `lib.rs`

**Interfaces:**
- Produces:
  - `pub struct ResolvedAddr { pub addr: IpAddr, pub ttl: Option<Duration> }`
  - `pub struct SvcbEndpoint { pub priority: u16, pub target: String, pub alpn: Vec<Vec<u8>>, pub port: Option<u16>, pub ipv4hint: Vec<Ipv4Addr>, pub ipv6hint: Vec<Ipv6Addr>, pub ech_config_list: Option<bytes::Bytes> }`
  - `pub trait Resolve { fn lookup_ipv4(&self, name: &str) -> impl Stream<Item = Result<ResolvedAddr, Error>>; fn lookup_ipv6(&self, name: &str) -> impl Stream<Item = Result<ResolvedAddr, Error>>; fn lookup_svcb(&self, _name: &str) -> impl Stream<Item = Result<SvcbEndpoint, Error>> { futures_util::stream::empty() } }`

- [ ] **Step 1: Write a failing test**

```rust
// crates/hclient-dns/src/lib.rs
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
        // lookup_svcb is deliberately not implemented — the default has to work.
    }

    #[test]
    fn svcb_has_a_default_returning_empty() {
        let got: Vec<_> = futures_executor::block_on(Static.lookup_svcb("x").collect());
        assert!(got.is_empty(),
            "otherwise getaddrinfo, wasi and embedded couldn't implement the trait");
    }

    #[test]
    fn families_are_separate_streams() {
        let v4: Vec<_> = futures_executor::block_on(Static.lookup_ipv4("x").collect());
        let v6: Vec<_> = futures_executor::block_on(Static.lookup_ipv6("x").collect());
        assert_eq!(v4.len(), 1);
        assert_eq!(v6.len(), 0, "you should connect on AAAA without waiting for A");
    }
}
```

- [ ] **Step 2: Run it and confirm it fails**

Run: `cargo test -p hclient-dns`
Expected: FAIL — the crate doesn't exist.

- [ ] **Step 3: Create and implement**

```toml
# crates/hclient-dns/Cargo.toml
[package]
name = "hclient-dns"
version = "0.1.0"
description = "hclient's pluggable resolver trait"
edition.workspace = true
rust-version.workspace = true
license.workspace = true
repository.workspace = true

[dependencies]
bytes         = { workspace = true }
futures-core  = { workspace = true }
futures-util  = { version = "0.3", default-features = false, features = ["std"] }
hclient-core  = { workspace = true }

[dev-dependencies]
futures-executor = { version = "0.3", default-features = false, features = ["std"] }

[lints]
workspace = true
```

```rust
// crates/hclient-dns/src/lib.rs
//! Pluggable name resolution.
//!
//! Separate streams per family, not a `Vec<SocketAddr>`: RFC 8305 requires
//! starting to connect on AAAA without waiting for A. `lookup_svcb` has a
//! default implementation returning empty — otherwise `getaddrinfo`,
//! `wasi:http` and embedded couldn't implement the trait.
#![deny(unsafe_code)]

use bytes::Bytes;
use futures_core::Stream;
use hclient_core::Error;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::time::Duration;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedAddr {
    pub addr: IpAddr,
    pub ttl: Option<Duration>,
}

/// RFC 9460 HTTPS/SVCB. `alpn` gives h3 discovery without Alt-Svc,
/// `ech_config_list` feeds `rustls::EchConfig` directly.
///
/// Built in from day one: pin the resolver to `SocketAddr`, and ECH and
/// h3 discovery are closed off forever without a breaking change.
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

- [ ] **Step 4: Run the tests**

Run: `cargo test -p hclient-dns`
Expected: PASS, two tests.

- [ ] **Step 5: Commit**

```bash
git add crates/hclient-dns
git commit -m "feat(dns): Resolve trait with per-family streams and a defaulted SVCB lookup"
```

---

### Task 7: `hclient-dns-system` — getaddrinfo through `Blocking`

**Files:**
- Create: `crates/hclient-dns-system/Cargo.toml`, `src/lib.rs`
- Test: inside `lib.rs`

**Interfaces:**
- Consumes: `Resolve` (Task 6), `Blocking` (Task 1).
- Produces: `pub struct SystemDns<B> { blocking: B }`; `SystemDns::new(b: B) -> Self`;
  `impl<B: Blocking> Resolve for SystemDns<B>`.

- [ ] **Step 1: Write failing tests**

```rust
// crates/hclient-dns-system/src/lib.rs
#[cfg(test)]
mod tests {
    use super::*;
    use futures_util::StreamExt;
    use hclient_rt::Blocking;

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
        assert!(v4.iter().all(|a| a.addr.is_ipv4()), "only v4 in the v4 stream");

        let v6: Vec<_> = futures_executor::block_on(r.lookup_ipv6("localhost").collect());
        let v6: Vec<_> = v6.into_iter().filter_map(Result::ok).collect();
        assert!(v6.iter().all(|a| a.addr.is_ipv6()), "only v6 in the v6 stream");

        assert!(!v4.is_empty() || !v6.is_empty(), "localhost must resolve");
    }

    #[test]
    fn unresolvable_name_yields_an_error_not_an_empty_stream() {
        let r = SystemDns::new(Inline);
        let got: Vec<_> = futures_executor::block_on(
            r.lookup_ipv4("invalid.invalid.").collect());
        assert!(got.iter().any(|x| x.is_err()),
                "an empty stream is indistinguishable from \"policy filtered everything out\"");
    }

    #[test]
    fn svcb_is_empty_because_getaddrinfo_cannot_return_it() {
        let r = SystemDns::new(Inline);
        let got: Vec<_> = futures_executor::block_on(r.lookup_svcb("example.com").collect());
        assert!(got.is_empty());
    }
}
```

- [ ] **Step 2: Run it and confirm it fails**

Run: `cargo test -p hclient-dns-system`
Expected: FAIL — the crate doesn't exist.

- [ ] **Step 3: Create and implement**

```toml
# crates/hclient-dns-system/Cargo.toml
[package]
name = "hclient-dns-system"
version = "0.1.0"
description = "hclient's system resolver (getaddrinfo) via the Blocking capability"
edition.workspace = true
rust-version.workspace = true
license.workspace = true
repository.workspace = true

[dependencies]
futures-core = { workspace = true }
futures-util = { version = "0.3", default-features = false, features = ["std"] }
hclient-core = { workspace = true }
hclient-dns  = { path = "../hclient-dns", version = "0.1.0" }
hclient-rt   = { path = "../hclient-rt",  version = "0.1.0" }

[dev-dependencies]
futures-executor = { version = "0.3", default-features = false, features = ["std"] }

[lints]
workspace = true
```

```rust
// crates/hclient-dns-system/src/lib.rs
//! A system resolver over `std::net::ToSocketAddrs` (i.e. `getaddrinfo`).
//!
//! `getaddrinfo` is blocking on every platform, so this crate requires the
//! `Blocking` capability — and is therefore unavailable wherever that
//! capability doesn't exist (wasm).
//!
//! **A limitation worth knowing:** `getaddrinfo` will never return
//! HTTPS/SVCB records. So neither ECH nor HTTP/3 discovery on the first
//! request are reachable on the system resolver. `lookup_svcb` is honestly empty.
#![deny(unsafe_code)]

use futures_core::Stream;
use hclient_core::{Error, ErrorKind};
use hclient_dns::{ResolvedAddr, Resolve};
use hclient_rt::Blocking;
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

> **A known limitation, to document in rustdoc:** this makes one
> `getaddrinfo` call for both families, while curl 8.20 makes **two**, on
> separate threads, so partial results can kick off Happy Eyeballs sooner.
> Splitting into two slots is a v0.2 task; what matters now is that the
> trait's shape allows it, and it does.

- [ ] **Step 4: Update `Blocking`'s bounds in `hclient-rt`**

`blocking::unblock` and `tokio::spawn_blocking` require `Send + 'static`.
Bring the trait to:

```rust
pub trait Blocking {
    fn run<T: Send + 'static, F: FnOnce() -> T + Send + 'static>(&self, f: F)
        -> impl Future<Output = T>;
}
```

This is the only place in the whole vertical where `Send` is declared by us,
and it's honest: the `Blocking` capability doesn't exist on wasm at all, so
there's nothing for it to infect.

- [ ] **Step 5: Run the tests**

Run: `cargo test -p hclient-dns-system && cargo test -p hclient-rt-tokio && cargo test -p hclient-rt-smol`
Expected: PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/hclient-dns-system crates/hclient-rt crates/hclient-rt-tokio crates/hclient-rt-smol
git commit -m "feat(dns-system): getaddrinfo resolver over the Blocking capability"
```

---

### Task 8: `hclient-tls` — the TLS trait

**Files:**
- Create: `crates/hclient-tls/Cargo.toml`, `src/lib.rs`
- Test: inside `lib.rs`

**Interfaces:**
- Produces:
  - `pub struct TlsRequest<'a> { pub server_name: &'a str, pub alpn: &'a [&'a [u8]], pub ech: Option<&'a [u8]> }`
  - `pub struct TlsInfo { pub alpn: Option<Vec<u8>>, pub peer_certificates: Option<Vec<Vec<u8>>>, pub protocol_version: Option<String>, pub cipher_suite: Option<String> }` — **every field is `Option`**
  - `pub trait TlsConnect { type Stream<S>: hyper::rt::Read + hyper::rt::Write + Unpin where S: hyper::rt::Read + hyper::rt::Write + Unpin; fn connect<S>(&self, io: S, req: TlsRequest<'_>) -> impl Future<Output = Result<(Self::Stream<S>, TlsInfo), Error>> where S: hyper::rt::Read + hyper::rt::Write + Unpin; }`

- [ ] **Step 1: Write a failing test**

```rust
// crates/hclient-tls/src/lib.rs
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tls_info_is_all_optional() {
        // native-tls only gives back the leaf certificate, ALPN, and
        // tls-server-end-point; the trait has to allow for that.
        let i = TlsInfo::default();
        assert!(i.alpn.is_none());
        assert!(i.peer_certificates.is_none());
        assert!(i.protocol_version.is_none());
        assert!(i.cipher_suite.is_none());
    }

    #[test]
    fn alpn_lives_on_the_request_not_the_config() {
        // Version pinning and h2 prior-knowledge require different ALPN
        // sets for different connections to the same origin.
        let req = TlsRequest { server_name: "example.com", alpn: &[b"http/1.1"], ech: None };
        assert_eq!(req.alpn, &[b"http/1.1".as_slice()]);
    }

    #[test]
    fn ech_slot_exists_before_it_is_implemented() {
        // ECH is RFC 9849; EchConfigList comes from HTTPS/SVCB. If we didn't
        // build the field in now, adding it later would be a breaking change.
        let req = TlsRequest { server_name: "e.com", alpn: &[], ech: Some(&[1, 2, 3]) };
        assert_eq!(req.ech, Some(&[1u8, 2, 3][..]));
    }
}
```

- [ ] **Step 2: Run it and confirm it fails**

Run: `cargo test -p hclient-tls`
Expected: FAIL — the crate doesn't exist.

- [ ] **Step 3: Create and implement**

```toml
# crates/hclient-tls/Cargo.toml
[package]
name = "hclient-tls"
version = "0.1.0"
description = "hclient's pluggable TLS trait"
edition.workspace = true
rust-version.workspace = true
license.workspace = true
repository.workspace = true

[dependencies]
hclient-core = { workspace = true }
hyper        = { version = "1.11", default-features = false }

[lints]
workspace = true
```

```rust
// crates/hclient-tls/src/lib.rs
//! Pluggable TLS.
//!
//! The trait is typed against `hyper::rt::Read/Write`, and **not** against
//! futures-io or tokio-io. Consequence: per-runtime TLS glue simply doesn't
//! exist — one adapter serves every runtime.
#![deny(unsafe_code)]

use hclient_core::Error;
use std::future::Future;

/// ALPN lives on the **connect**, not the config: version pinning and h2
/// prior-knowledge require different sets for different connections to the
/// same origin. An implementation caches its config keyed by ALPN set.
#[derive(Debug, Clone, Copy)]
pub struct TlsRequest<'a> {
    pub server_name: &'a str,
    pub alpn: &'a [&'a [u8]],
    /// RFC 9849 Encrypted Client Hello. Comes from an HTTPS/SVCB record.
    /// The slot is built in from day one: adding it later would be a
    /// breaking change.
    pub ech: Option<&'a [u8]>,
}

/// Every field is `Option`, because native-tls only gives back the leaf
/// certificate, ALPN, and tls-server-end-point.
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

- [ ] **Step 4: Run the tests**

Run: `cargo test -p hclient-tls`
Expected: PASS, three tests.

- [ ] **Step 5: Commit**

```bash
git add crates/hclient-tls
git commit -m "feat(tls): TlsConnect trait typed on hyper::rt, ALPN per connect, ECH slot"
```

---

### Task 9: `hclient-tls-rustls` — the stream

The biggest single unit of this vertical. The adapter is built on the
surface of rustls that's been stable since 0.20 (`process_new_packets`,
`wants_read`/`wants_write`, `read_tls`/`write_tls`), **not** on `unbuffered`
— that's been removed on rustls main (PR #2905, 2026-02-06).

**Files:**
- Create: `crates/hclient-tls-rustls/Cargo.toml`, `src/lib.rs`, `src/stream.rs`
- Test: `crates/hclient-tls-rustls/tests/handshake.rs`

**Interfaces:**
- Consumes: `TlsConnect`, `TlsRequest`, `TlsInfo` (Task 8).
- Produces:
  - `pub struct Rustls { .. }`; `Rustls::with_platform_verifier() -> Result<Self, Error>`;
    `Rustls::with_webpki_roots() -> Self`; `Rustls::from_config(Arc<rustls::ClientConfig>) -> Self`
  - `pub struct TlsStream<S>`; `impl<S> hyper::rt::Read + hyper::rt::Write for TlsStream<S>`
  - `impl TlsConnect for Rustls { type Stream<S> = TlsStream<S>; }`

- [ ] **Step 1: Write a failing integration test**

```rust
// crates/hclient-tls-rustls/tests/handshake.rs
//! This test brings up a real TLS server on rustls and checks that our
//! adapter carries the handshake through to completion and pumps bytes in
//! both directions.

use hclient_rt::{TcpConnect, TcpOpts};
use hclient_rt_tokio::Tokio;
use hclient_tls::{TlsConnect, TlsRequest};
use hclient_tls_rustls::Rustls;

mod server;  // see Step 3: a minimal TLS echo server on a self-signed cert

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
               "the negotiated ALPN must be visible");

    // Pump bytes through the hyper::rt interface.
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
    let tls = Rustls::with_webpki_roots(); // public roots — our cert is unknown to them
    let tcp = Tokio.connect(addr, &TcpOpts::default()).await.unwrap();
    let err = tls.connect(tcp, TlsRequest {
        server_name: "localhost", alpn: &[], ech: None,
    }).await.err().expect("must fail");
    assert!(matches!(err.kind(), hclient_core::ErrorKind::Tls), "{err}");
}
```

- [ ] **Step 2: Run it and confirm it fails**

Run: `cargo test -p hclient-tls-rustls`
Expected: FAIL — the crate doesn't exist.

- [ ] **Step 3: Create the crate and the test server**

```toml
# crates/hclient-tls-rustls/Cargo.toml
[package]
name = "hclient-tls-rustls"
version = "0.1.0"
description = "hclient's TLS backend on rustls, the adapter is written against hyper::rt"
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
hclient-core     = { workspace = true }
hclient-tls      = { path = "../hclient-tls", version = "0.1.0" }
hyper            = { version = "1.11", default-features = false }
rustls           = { version = "0.23", default-features = false, features = ["std", "ring", "tls12"] }
rustls-pki-types = "1.15"
rustls-platform-verifier = { version = "0.7", optional = true }
webpki-roots     = { version = "1.0", optional = true }

[dev-dependencies]
hclient-rt       = { path = "../hclient-rt" }
hclient-rt-tokio = { path = "../hclient-rt-tokio" }
rcgen            = "0.14"
tokio            = { version = "1", features = ["macros", "rt-multi-thread", "net", "io-util"] }
tokio-rustls     = { version = "0.26", default-features = false, features = ["ring"] }

[lints]
workspace = true
```

```rust
// crates/hclient-tls-rustls/tests/server.rs
//! A minimal TLS echo server on a self-signed certificate.
//! Lives in dev-dependencies and never lands in the public graph.

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

- [ ] **Step 4: Implement the stream**

```rust
// crates/hclient-tls-rustls/src/stream.rs
use hclient_core::{Error, ErrorKind};
use hyper::rt::{Read, ReadBufCursor, Write};
use std::io::{Read as _, Write as _};
use std::pin::Pin;
use std::task::{Context, Poll, ready};

const SCRATCH: usize = 16 * 1024;

/// TLS over any `hyper::rt` transport.
///
/// Built on the surface of rustls that's been stable since 0.20: `read_tls`
/// / `process_new_packets` / `wants_write` / `write_tls`. **Not** on
/// `unbuffered` — that's been removed on rustls main (PR #2905,
/// 2026-02-06), and an adapter built on it would have to be rewritten
/// entirely for 0.24.
#[derive(Debug)]
pub struct TlsStream<S> {
    io: S,
    conn: rustls::ClientConnection,
    /// Bytes read from the socket but not yet fed to rustls.
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

/// Pump everything rustls wants to write into the underlying transport.
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

/// Read from the transport and feed it to rustls. `Ok(false)` means EOF.
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
            // 1. Hand back whatever's already decrypted.
            let mut scratch = [0u8; SCRATCH];
            let want = buf.remaining().min(SCRATCH);
            if want == 0 { return Poll::Ready(Ok(())) }
            match this.conn.reader().read(&mut scratch[..want]) {
                Ok(0) => {}
                Ok(n) => { buf.put_slice(&scratch[..n]); return Poll::Ready(Ok(())) }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
                Err(e) => return Poll::Ready(Err(e)),
            }
            // 2. Send off everything outgoing (renegotiation, close_notify, etc.).
            ready!(flush_outgoing(&mut this.io, &mut this.conn, cx))?;
            // 3. Read more from the transport.
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

- [ ] **Step 5: Implement `Rustls` and the handshake**

```rust
// crates/hclient-tls-rustls/src/lib.rs
//! A TLS backend on rustls.
//!
//! **rustls never appears in `hclient`'s public API** — otherwise the 0.24
//! release would become our breaking release. Expected in 0.24: the `std`
//! feature removed, providers split out into `rustls-ring`/`rustls-aws-lc-rs`,
//! MSRV 1.85, edition 2024. One rewritten crate is budgeted for.
#![deny(unsafe_code)]

mod stream;

pub use stream::TlsStream;

use hclient_core::{Error, ErrorKind};
use hclient_tls::{TlsConnect, TlsInfo, TlsRequest};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

#[derive(Debug)]
pub struct Rustls {
    base: Arc<rustls::ClientConfig>,
    /// ALPN is set per connect, and `ClientConfig` stores it inside itself —
    /// so we cache the config keyed by ALPN set. Without the cache, every
    /// request would build the config over again, and that's the most
    /// expensive operation in rustls.
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

        // Carry the handshake through to completion before handing the stream up.
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

Make `stream::{flush_outgoing, pump_incoming}` `pub(crate)` and add
`ErrorKind::Tls` to the handshake error mapping.

- [ ] **Step 6: Run the tests**

Run: `cargo test -p hclient-tls-rustls --features webpki-roots`
Expected: PASS, two tests.

- [ ] **Step 7: Verify rustls doesn't leak into `hclient`'s public API**

Run: `! grep -rn "rustls" crates/hclient/src crates/hclient-core/src && echo OK`
Expected: `OK`.

- [ ] **Step 8: Commit**

```bash
git add crates/hclient-tls-rustls
git commit -m "feat(tls-rustls): TLS stream over hyper::rt on the 0.20-stable rustls surface"
```

---

### Task 10: `hclient-native` — a request body with a Send error

`hyper::client::conn::http1::handshake<T, B>` requires
`B::Error: Into<Box<dyn StdError + Send + Sync>>` and `B::Data: Send`. Our
`hclient_core::Error` holds an `Arc<dyn Error + 'static>` and is **not**
`Send + Sync`. The requirement gets locked up here and doesn't leak into the core.

**Files:**
- Create: `crates/hclient-native/Cargo.toml`, `src/lib.rs`, `src/body.rs`
- Test: inside `body.rs`

**Interfaces:**
- Consumes: `hclient_core::RequestBody`.
- Produces: `pub(crate) struct OutgoingBody`; `OutgoingBody::from_request_body(RequestBody) -> Self`;
  `impl http_body::Body for OutgoingBody { type Data = Bytes; type Error = BoxError; }`
  where `pub(crate) type BoxError = Box<dyn std::error::Error + Send + Sync>`.

- [ ] **Step 1: Write failing tests**

```rust
// crates/hclient-native/src/body.rs
#[cfg(test)]
mod tests {
    use super::*;
    use http_body_util::BodyExt;
    use hclient_core::RequestBody;

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

- [ ] **Step 2: Run it and confirm it fails**

Run: `cargo test -p hclient-native`
Expected: FAIL — the crate doesn't exist.

- [ ] **Step 3: Create the crate and implement the body**

```toml
# crates/hclient-native/Cargo.toml
[package]
name = "hclient-native"
version = "0.1.0"
description = "hclient's native transport: TCP + TLS + HTTP/1.1 on hyper"
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
hclient-core   = { workspace = true }
hclient-dns    = { path = "../hclient-dns",   version = "0.1.0" }
hclient-proto  = { workspace = true }
hclient-rt     = { path = "../hclient-rt",    version = "0.1.0" }
hclient-tls    = { path = "../hclient-tls",   version = "0.1.0" }
hyper          = { version = "1.11", default-features = false, features = ["client", "http1"] }

[dev-dependencies]
futures-executor = { version = "0.3", default-features = false, features = ["std"] }

[lints]
workspace = true
```

```rust
// crates/hclient-native/src/body.rs
use bytes::Bytes;
use http_body::{Body, Frame, SizeHint};
use hclient_core::RequestBody;
use std::pin::Pin;
use std::task::{Context, Poll};

/// hyper requires `B::Error: Into<Box<dyn StdError + Send + Sync>>`, and our
/// `hclient_core::Error` holds an `Arc<dyn Error + 'static>` with no `Send`.
/// The `Send` requirement gets locked up here and doesn't leak into the core.
pub(crate) type BoxError = Box<dyn std::error::Error + Send + Sync>;

#[derive(Debug)]
pub(crate) struct OutgoingBody {
    inner: Option<Bytes>,
}

impl OutgoingBody {
    pub(crate) fn from_request_body(body: RequestBody) -> Self {
        // In v0.1 the native transport only sends buffered bodies. Streaming
        // ones arrive together with the pool and retry — `RequestBody`'s
        // shape already allows for that.
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

- [ ] **Step 4: Run the tests**

Run: `cargo test -p hclient-native`
Expected: PASS, four tests.

- [ ] **Step 5: Commit**

```bash
git add crates/hclient-native
git commit -m "feat(native): outgoing body confining hyper's Send bound to this crate"
```

---

### Task 11: `hclient-native` — the connector

**Files:**
- Create: `crates/hclient-native/src/connect.rs`
- Modify: `crates/hclient-native/src/lib.rs`
- Test: `crates/hclient-native/tests/connect.rs`

**Interfaces:**
- Consumes: `Scheduler`/`HeAction` (Task 5), `Resolve` (Task 6), `TcpConnect`,
  `Timer` (Task 1), `TlsConnect` (Task 8).
- Produces:
  - `pub(crate) enum Conn<P, T> { Plain(P), Tls(T) }` — implements `hyper::rt::Read + Write`
  - `pub(crate) async fn connect<R, D, L>(rt: &R, dns: &D, tls: &L, uri: &http::Uri, opts: &TcpOpts, alpn: &[&[u8]]) -> Result<(Conn<R::Stream, L::Stream<R::Stream>>, Option<TlsInfo>), Error>`

- [ ] **Step 1: Write a failing test**

```rust
// crates/hclient-native/tests/connect.rs
//! Verifies that the connector really does run Happy Eyeballs: a dead
//! address is tried first, then a live one, and the connection succeeds.

use hclient_native::testing::connect_for_test;
use hclient_rt_tokio::Tokio;

#[tokio::test]
async fn falls_over_from_a_dead_address_to_a_live_one() {
    let live = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = live.local_addr().unwrap();
    std::thread::spawn(move || { let _ = live.accept(); });

    // 198.51.100.1 is TEST-NET-2, guaranteed not to answer.
    let dead: std::net::IpAddr = "198.51.100.1".parse().unwrap();
    let conn = connect_for_test(&Tokio, &[dead, addr.ip()], addr.port()).await;
    assert!(conn.is_ok(), "must reach the live address");
}

#[tokio::test]
async fn reports_connect_kind_when_everything_is_dead() {
    let dead: std::net::IpAddr = "198.51.100.1".parse().unwrap();
    let err = connect_for_test(&Tokio, &[dead], 81).await.err().expect("must fail");
    assert!(matches!(err.kind(), hclient_core::ErrorKind::Connect), "{err}");
}
```

- [ ] **Step 2: Run it and confirm it fails**

Run: `cargo test -p hclient-native --test connect`
Expected: FAIL — `connect_for_test` not found.

- [ ] **Step 3: Implement the connector**

```rust
// crates/hclient-native/src/connect.rs
use futures_util::stream::{FuturesUnordered, StreamExt};
use hclient_core::{Error, ErrorKind};
use hclient_proto::happy_eyeballs::{HeAction, HeConfig, Scheduler};
use hclient_rt::{TcpConnect, TcpOpts, Timer};
use hyper::rt::{Read, ReadBufCursor, Write};
use std::net::{IpAddr, SocketAddr};
use std::pin::Pin;
use std::task::{Context, Poll};

/// A connection: with TLS or without. Both variants are `hyper::rt` IO.
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

/// Happy Eyeballs per RFC 8305 **with no `spawn`**: attempts live in a
/// `FuturesUnordered`, not in tasks, because `spawn` would require
/// `Send + 'static` and would shut out single-threaded runtimes.
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

For `select_biased!`, add `futures-util` with the `async-await-macro` feature.

- [ ] **Step 4: Export a test helper**

```rust
// in crates/hclient-native/src/lib.rs
#[doc(hidden)]
pub mod testing {
    use super::*;
    /// For integration tests only: runs Happy Eyeballs over a ready-made
    /// list of addresses, bypassing DNS.
    pub async fn connect_for_test<R>(rt: &R, addrs: &[std::net::IpAddr], port: u16)
        -> Result<R::Stream, hclient_core::Error>
    where R: hclient_rt::TcpConnect + hclient_rt::Timer
    {
        let (v6, v4): (Vec<_>, Vec<_>) = addrs.iter().copied().partition(|a| a.is_ipv6());
        crate::connect::race_connect(rt, v6, v4, port, &hclient_rt::TcpOpts::default()).await
    }
}
```

- [ ] **Step 5: Run the tests**

Run: `cargo test -p hclient-native --test connect`
Expected: PASS, two tests. The second one takes a few seconds (waiting out
the TEST-NET-2 timeout) — that's expected.

- [ ] **Step 6: Commit**

```bash
git add crates/hclient-native
git commit -m "feat(native): RFC 8305 connector racing attempts without spawn"
```

---

### Task 12: `hclient-native` — HTTP/1 with the connection driven inline

The technical center of this vertical. The h1 handshake needs neither an
executor nor a timer, and `Connection` is polled **alongside** the response
— meaning the client runs on a bare `futures` executor with zero ability to
spawn. This is exactly what proves the runtime seam is real.

**Files:**
- Create: `crates/hclient-native/src/h1.rs`
- Modify: `crates/hclient-native/src/lib.rs`
- Test: `crates/hclient-native/tests/h1.rs`

**Interfaces:**
- Consumes: `OutgoingBody` (Task 10), `Conn` (Task 11).
- Produces:
  - `pub struct NativeBody` — a response body that **drives the connection itself**;
    `impl http_body::Body for NativeBody { type Data = Bytes; type Error = Error }`
  - `pub(crate) async fn exchange<I>(io: I, req: http::Request<OutgoingBody>) -> Result<http::Response<NativeBody>, Error>`

- [ ] **Step 1: Write a failing test**

```rust
// crates/hclient-native/tests/h1.rs
//! The server is a bare `std::net::TcpListener` speaking HTTP/1.1 by hand.
//! No server frameworks: the test verifies our client, not someone else's server.

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
    // The key test of the vertical: no tokio, no smol — just futures::block_on.
    let addr = spawn_h1_server(
        "HTTP/1.1 200 OK\r\nContent-Length: 5\r\n\r\nhello");

    futures_executor::block_on(async move {
        let std_tcp = std::net::TcpStream::connect(addr).unwrap();
        std_tcp.set_nonblocking(false).unwrap();
        // A blocking socket on a blocking executor is fine for a test:
        // it verifies that hyper needs neither spawn nor a timer.
        let io = hclient_native::testing::blocking_io(std_tcp);
        let req = http::Request::builder().uri("/").body(
            hclient_native::testing::empty_body()).unwrap();
        let resp = hclient_native::testing::exchange_for_test(io, req).await.unwrap();
        assert_eq!(resp.status(), 200);
        let body = hclient_native::testing::collect(resp.into_body()).await.unwrap();
        assert_eq!(&body[..], b"hello");
    });
}

#[test]
fn body_keeps_driving_the_connection_after_headers() {
    // The body arrives as a separate chunk after the headers: if the
    // connection stopped being polled, the read would hang.
    let addr = spawn_h1_server(
        "HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n5\r\nhello\r\n0\r\n\r\n");
    futures_executor::block_on(async move {
        let std_tcp = std::net::TcpStream::connect(addr).unwrap();
        let io = hclient_native::testing::blocking_io(std_tcp);
        let req = http::Request::builder().uri("/").body(
            hclient_native::testing::empty_body()).unwrap();
        let resp = hclient_native::testing::exchange_for_test(io, req).await.unwrap();
        let body = hclient_native::testing::collect(resp.into_body()).await.unwrap();
        assert_eq!(&body[..], b"hello");
    });
}
```

- [ ] **Step 2: Run it and confirm it fails**

Run: `cargo test -p hclient-native --test h1`
Expected: FAIL — `exchange_for_test` not found.

- [ ] **Step 3: Implement the exchange with an inline drive**

```rust
// crates/hclient-native/src/h1.rs
use crate::body::{BoxError, OutgoingBody};
use bytes::Bytes;
use http_body::{Body, Frame};
use hclient_core::{Error, ErrorKind};
use hyper::client::conn::http1;
use std::pin::Pin;
use std::task::{Context, Poll};

/// A response body that **polls the connection itself**.
///
/// Without this, the connection would stop moving once the headers
/// arrived: hyper requires someone to drive `Connection`, and we
/// deliberately don't spawn — otherwise `Send + 'static` would be needed
/// and single-threaded runtimes would be shut out.
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
        // Move the connection forward first — otherwise data won't arrive.
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

/// One request per connection. There's no pool in v0.1.
pub(crate) async fn exchange<I>(io: I, req: http::Request<OutgoingBody>)
    -> Result<http::Response<NativeBody>, Error>
where I: hyper::rt::Read + hyper::rt::Write + Unpin + 'static
{
    let (mut sender, conn) = http1::handshake::<I, OutgoingBody>(io).await
        .map_err(|e| Error::new(ErrorKind::Connect, e))?;

    // Drive the connection and the request **together**, with no spawn.
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

A note on `Box<dyn Future>`: this is the **only** place in the vertical
where we box, and we're not boxing to erase `Send` — we're boxing to store
the connection in the body's field. The type stays `!Send`-compatible — no
`Send` bound is declared.

- [ ] **Step 4: Add test helpers**

```rust
// in crates/hclient-native/src/lib.rs, mod testing
    pub use crate::h1::NativeBody;

    pub fn empty_body() -> crate::body::OutgoingBody {
        crate::body::OutgoingBody::from_request_body(hclient_core::RequestBody::Empty)
    }

    /// A blocking `std::net::TcpStream` as `hyper::rt` IO — for tests on a
    /// bare executor only, where there's no reactor at all.
    pub fn blocking_io(s: std::net::TcpStream) -> BlockingIo { BlockingIo(s) }

    pub struct BlockingIo(std::net::TcpStream);
    // impl hyper::rt::Read / Write via std::io::{Read, Write},
    // always returning Poll::Ready — the socket is blocking.

    pub async fn exchange_for_test<I>(io: I, req: http::Request<crate::body::OutgoingBody>)
        -> Result<http::Response<crate::h1::NativeBody>, hclient_core::Error>
    where I: hyper::rt::Read + hyper::rt::Write + Unpin + 'static
    { crate::h1::exchange(io, req).await }

    pub async fn collect(b: crate::h1::NativeBody)
        -> Result<bytes::Bytes, hclient_core::Error>
    {
        use http_body_util::BodyExt;
        Ok(b.collect().await?.to_bytes())
    }
```

Implement `BlockingIo` fully: `poll_read` reads into a stack buffer and
delivers via `put_slice`, `poll_write`/`poll_flush` are direct
`std::io::Write` calls, `poll_shutdown` is `shutdown(Both)`. Always
`Poll::Ready`.

- [ ] **Step 5: Run the tests**

Run: `cargo test -p hclient-native --test h1`
Expected: PASS, two tests. **This is the proof of runtime neutrality:**
neither tokio nor smol appears in the test.

- [ ] **Step 6: Commit**

```bash
git add crates/hclient-native
git commit -m "feat(native): HTTP/1 exchange driving the connection inline, no spawn"
```

---

### Task 13: `hclient-native` — `Native<R, T, D>: Transport`

**Files:**
- Modify: `crates/hclient-native/src/lib.rs`
- Test: `crates/hclient-native/tests/transport.rs`

**Interfaces:**
- Consumes: everything so far.
- Produces:
  - `pub struct Native<R, T, D> { rt: R, tls: T, dns: D, opts: TcpOpts }`
  - `Native::new(rt: R, tls: T, dns: D) -> Self`; `Native::tcp_opts(self, o: TcpOpts) -> Self`
  - `impl<R, T, D> Transport for Native<R, T, D>` with `type Body = NativeBody; type Error = Error;`
  - **`Transport::to_error` MUST be overridden with an identity function** —
    see the block below, this isn't an optional detail
  - `Capabilities`: `streaming_request_body: false` (v0.1), `redirects: Configurable`,
    `tls_config: Full`, `version_reported: true`, `timeouts.connect: true`,
    `timeouts.first_byte: false`, `timeouts.between_bytes: false`,
    `upgrade: UpgradeSupport::None` (h1 upgrade is v0.3)

> **Mandatory override of `Transport::to_error`.** Vertical 1
> (finding B2 of its final review) added a default hook to the seam,
> `fn to_error(&self, e: Self::Error) -> Error`, that turns a backend error
> into a library error. The default wraps it with `ErrorKind::Other` — which
> is correct exactly for a backend that has nothing to say about the
> category. `Native` isn't one of those: its `type Error = Error`, meaning
> the category is ALREADY assigned (`ErrorKind::Resolve` in `execute`,
> `Connect` in `race_connect`, `Tls` in `TlsConnect`, `Body` in `h1`).
> Without an override, `Client::execute` would wrap it in a second error, and
> a consumer's `kind()` would become `Other`, with
> `is_timeout()`/`is_connect()`/`is_unsupported()` all `false` at once;
> `Display` would additionally print the category twice
> (`Other: Connect: …`).
>
> This is exactly how vertical 1 would have shipped: `hclient-wasi` was
> sorting 39 `ErrorCode` variants into eight `ErrorKind`s across forty lines,
> and all of it was getting thrown away one layer up. 165 tests didn't catch
> it, because none of them checked that the transport's category survives to
> the caller.
>
> The compiler doesn't check this obligation and can't: the default stays a
> default deliberately (an alternative would require `Send + Sync` from
> EVERY backend's error at the trait level, and amendment C1 keeps a
> transport with an honestly `!Send` error representable). Only a test
> verifies it — it's Step 1 below, and it's mandatory, not "nice to have."
> Examples:
> `to_error_is_the_identity_so_the_classification_survives_the_client`
> (`crates/hclient-wasi/src/convert.rs`) and
> `crates/hclient/tests/transport_error.rs`.

- [ ] **Step 1: Write a failing test**

```rust
// crates/hclient-native/tests/transport.rs
use hclient::Client;
use hclient_dns_system::SystemDns;
use hclient_native::Native;
use hclient_rt_tokio::Tokio;
use hclient_tls_rustls::Rustls;

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
    use hclient_core::unversioned::Transport;
    let t = Native::new(Tokio, Rustls::with_webpki_roots(), SystemDns::new(Tokio));
    let caps = t.capabilities();
    assert!(!caps.streaming_request_body, "the body is buffered in v0.1");
    assert!(caps.timeouts.connect);
    assert!(!caps.timeouts.first_byte, "no pool and no response timer — can't declare it");
    assert_eq!(caps.upgrade, hclient_core::UpgradeSupport::None);
    assert_eq!(caps.tls_config, hclient_core::TlsSupport::Full);
}

/// The category `Native` assigned must survive to the caller through the
/// whole `Client::execute` path (see the block above Step 1). This test
/// checks exactly that whole path, not `to_error`'s default — that's
/// verified in `hclient-core/tests/shape.rs` and guarantees on its own that
/// `Error` passes straight through.
///
/// The host is deliberately nonexistent: it's the only failure `execute`
/// produces with no network and no server, and the `wasi` counterpart of
/// this test is built the same way — it runs the backend's real classifier,
/// not a hand-constructed `Error`.
#[tokio::test]
async fn transport_error_kind_survives_the_client_instead_of_flattening_to_other() {
    let t = Native::new(Tokio, Rustls::with_webpki_roots(), SystemDns::new(Tokio));
    let c = Client::builder(t).build().unwrap();
    let err = c.get("http://nonexistent.invalid/").send().await.unwrap_err();

    assert_eq!(
        *err.kind(),
        hclient::ErrorKind::Resolve,
        "the category must survive to the caller, not flatten into Other: {err}"
    );
    assert!(
        !err.to_string().starts_with("Other:"),
        "the category is printed once, and it's the real category: {err}"
    );
}

#[tokio::test]
async fn unsupported_timeout_is_rejected_at_build_time() {
    use std::time::Duration;
    let t = Native::new(Tokio, Rustls::with_webpki_roots(), SystemDns::new(Tokio));
    let err = Client::builder(t)
        .timeouts(hclient::Timeouts {
            between_bytes: Some(Duration::from_secs(1)), ..Default::default() })
        .build().unwrap_err();
    assert_eq!(err.what, "between_bytes_timeout");
}
```

- [ ] **Step 2: Run it and confirm it fails**

Run: `cargo test -p hclient-native --test transport`
Expected: FAIL — `Native` not found.

- [ ] **Step 3: Implement the transport**

```rust
// in crates/hclient-native/src/lib.rs
use futures_util::StreamExt;
use hclient_core::unversioned::Transport;
use hclient_core::{Capabilities, Error, ErrorKind, RedirectSupport, RequestBody,
                   TimeoutSupport, Timeouts, TlsSupport, UpgradeSupport};
use hclient_dns::Resolve;
use hclient_rt::{TcpConnect, TcpOpts, Timer};
use hclient_tls::{TlsConnect, TlsRequest};

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
        // Honest about v0.1: no pool, no streaming request body, no upgrade.
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

        // Separate streams: connect on AAAA without waiting for A.
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
        // hyper's h1 requires origin-form and a Host header.
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

    /// The identity function: `Self::Error` is already `hclient_core::Error`,
    /// and its category is assigned right where the failure occurred
    /// (`Resolve` above, `Connect` in `race_connect`, `Tls` in `TlsConnect`,
    /// `Body` in `h1`). The hook's default would do exactly the same thing
    /// (it recognizes our `Error` and passes it straight through), so this
    /// line is redundant behaviorally and needed semantically: it states the
    /// intent right where it's read. See the block above Step 1.
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

- [ ] **Step 4: Run the tests**

Run: `cargo test -p hclient-native`
Expected: PASS, every test in the crate.

- [ ] **Step 5: Commit**

```bash
git add crates/hclient-native
git commit -m "feat(native): Native transport wiring runtime, TLS and DNS together"
```

---

### Task 14: `hclient` — `DefaultTransport` and an end-to-end run on two runtimes

**Files:**
- Modify: `crates/hclient/Cargo.toml`, `crates/hclient/src/client.rs`, `src/lib.rs`
- Create: `crates/hclient/tests/two_runtimes.rs`
- Modify: `.github/workflows/ci.yml`
- Create: `README.md` (update the "Status" section)

**Interfaces:**
- Consumes: `Native` (Task 13), `Tokio` (Task 3), `Smol` (Task 4).
- Produces:
  - `pub type DefaultTransport` — cfg-selected by target
  - `pub struct Client<T = DefaultTransport>` — a default parameter added
  - `Client::new() -> Result<Client<DefaultTransport>, UnsupportedCapability>` behind
    the `default-transport` feature

- [ ] **Step 1: Write a failing test**

```rust
// crates/hclient/tests/two_runtimes.rs
//! The same code, two runtimes, zero cfg. If this file needs a `#[cfg]`,
//! the runtime seam is decorative and this vertical has failed.

use hclient::{Client, Timeouts};
use hclient_dns_system::SystemDns;
use hclient_native::Native;
use hclient_tls_rustls::Rustls;

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

/// A generic function: its body is exactly that "one piece of code for every runtime."
async fn fetch_once<R>(rt: R, addr: std::net::SocketAddr) -> String
where
    R: hclient_rt::TcpConnect + hclient_rt::Timer + hclient_rt::Blocking + Clone,
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
    assert_eq!(rt.block_on(fetch_once(hclient_rt_tokio::Tokio, addr)), "same");
}

#[test]
fn identical_code_on_smol() {
    let addr = spawn_server();
    assert_eq!(
        futures_executor::block_on(fetch_once(hclient_rt_smol::Smol, addr)),
        "same"
    );
}
```

- [ ] **Step 2: Run it and confirm it fails**

Run: `cargo test -p hclient --test two_runtimes`
Expected: FAIL — the dev-dependencies don't exist.

- [ ] **Step 3: Add `DefaultTransport` and the default parameter**

```rust
// in crates/hclient/src/lib.rs
/// The default transport, chosen by the **target, not the user**.
///
/// The default is an opinion, not a restriction: `Client` with no parameter
/// means `Client<DefaultTransport>`, and `Client<Whatever>` works the same
/// way. No mutually exclusive cargo features arise, because the target does
/// the choosing.
#[cfg(all(feature = "default-transport", not(target_family = "wasm")))]
pub type DefaultTransport = hclient_native::Native<
    hclient_rt_tokio::Tokio,
    hclient_tls_rustls::Rustls,
    hclient_dns_system::SystemDns<hclient_rt_tokio::Tokio>,
>;

#[cfg(all(feature = "default-transport", target_family = "wasm", target_os = "wasi"))]
pub type DefaultTransport = hclient_wasi::WasiHttp;
```

In `client.rs`, replace the declaration with:

```rust
#[cfg(feature = "default-transport")]
pub struct Client<T = crate::DefaultTransport> { transport: T, config: Config }
#[cfg(not(feature = "default-transport"))]
pub struct Client<T> { transport: T, config: Config }
```

and add:

```rust
#[cfg(all(feature = "default-transport", not(target_family = "wasm")))]
impl Client<crate::DefaultTransport> {
    /// A client with the default transport.
    ///
    /// On native, this requires an ambient tokio runtime: `tokio::spawn` and
    /// `tokio::time::sleep` panic outside a runtime. reqwest behaves exactly
    /// the same way. The explicit path is
    /// `Client::builder(Native::new(rt, tls, dns))`.
    pub fn new() -> Result<Self, hclient_core::UnsupportedCapability> {
        let rt = hclient_rt_tokio::Tokio;
        Self::builder(hclient_native::Native::new(
            rt,
            hclient_tls_rustls::Rustls::with_platform_verifier()
                .expect("platform verifier"),
            hclient_dns_system::SystemDns::new(rt),
        )).build()
    }
}
```

In `Cargo.toml`, add the feature and the target dependencies:

```toml
[features]
default = []
test-util = []
default-transport = []

[target.'cfg(not(target_family = "wasm"))'.dependencies]
hclient-native     = { path = "../hclient-native",     version = "0.1.0", optional = true }
hclient-rt-tokio   = { path = "../hclient-rt-tokio",   version = "0.1.0", optional = true }
hclient-tls-rustls = { path = "../hclient-tls-rustls", version = "0.1.0", optional = true }
hclient-dns-system = { path = "../hclient-dns-system", version = "0.1.0", optional = true }

[dev-dependencies]
futures-executor   = { version = "0.3", default-features = false, features = ["std"] }
hclient-native     = { path = "../hclient-native" }
hclient-rt         = { path = "../hclient-rt" }
hclient-rt-tokio   = { path = "../hclient-rt-tokio" }
hclient-rt-smol    = { path = "../hclient-rt-smol" }
hclient-tls-rustls = { path = "../hclient-tls-rustls", features = ["webpki-roots"] }
hclient-dns-system = { path = "../hclient-dns-system" }
tokio              = { version = "1", features = ["rt-multi-thread"] }
```

Extend the `default-transport` feature with a `dep:` list for non-wasm targets.

- [ ] **Step 4: Run the end-to-end test**

Run: `cargo test -p hclient --test two_runtimes`
Expected: PASS, two tests. Verify that `two_runtimes.rs` has **not a single
`#[cfg]`** — that's exactly this vertical's acceptance criterion.

- [ ] **Step 5: Verify the smol path hasn't pulled in async-compat or a tokio runtime**

Run: `cargo tree -p hclient-rt-smol -e normal --prefix none | grep -E '^(tokio|async-compat)' && exit 1 || echo OK`
Expected: `OK`.

- [ ] **Step 6: Update CI**

```yaml
  # add a job
  two-runtimes:
    strategy:
      matrix:
        os: [ubuntu-latest, macos-latest, windows-latest]
    runs-on: ${{ matrix.os }}
    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@stable
      - run: cargo test -p hclient --test two_runtimes
      - name: smol path must not pull tokio or async-compat
        run: |
          cargo tree -p hclient-rt-smol -e normal --prefix none \
            | grep -E '^(tokio|async-compat)' && exit 1 || true
```

- [ ] **Step 7: Commit**

```bash
git add crates/hclient .github/workflows/ci.yml README.md
git commit -m "feat(hclient): DefaultTransport and identical code proven on tokio and smol"
```

---

## What this vertical proved, and what's left

**Proven:** the runtime seam is real — the same generic code works on tokio
and smol with no `#[cfg]`, and the h1 exchange runs even on a bare `futures`
executor with no spawn and no timer. The TLS adapter is one for every
runtime, because it's written against `hyper::rt`. `Send` is declared in
exactly one place (`Blocking`) and doesn't infect the core.

**Not proven, and carried over into vertical 3:** the `Capabilities` runtime
model (needs fetch with its Chrome/Safari difference); `SseStream`
reconnect; `act` acceptance.

**Deliberately not done in v0.1, to record in rustdoc:** connection pooling
(one connection per request); streaming request bodies; `first_byte`/
`between_bytes` timeouts (declared unsupported, not silently unimplemented);
a single `getaddrinfo` call for both families instead of two slots; h1 upgrade.
