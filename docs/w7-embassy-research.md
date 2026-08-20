# W7 — embassy as a third runtime: research

Research only. Nothing in `crates/` changed. Every "it builds" below is a
command and its output; every "it cannot" is the compiler's own words;
everything that could not be checked here is marked **unverified** together
with what would settle it.

**Amended after the implementation (`hclient-rt-embassy`, W7).** Four
claims below were measured again by code that had to work rather than by a
spike, and three of them moved. Each is marked **[corrected by the
implementation]** where it stands; they are, in order: §1.4 `TcpOpts` (two
of six are appliable, not none), §1.5 `poll_shutdown` (a real half-close is
available), §6 consequence 1 (`CancelSupport::None` is not a choice this
task could make), and two behaviours nobody looked for — `embassy_time` is
unusable below `TcpConnect::connect`, and one abandoned connect starves ARP
for the whole interface. Both new ones are at the end, under
"Found while implementing" — and a later section, "Found by mutating the
tests", records the two places where the suite was measured to be checking
less than it looked like it was.

Spikes live in `spikes/` (untracked, outside `crates/`): `lifetime`,
`under-embassy`, `tuntap`, `espidf-check`, `espidf-asyncio`, `espidf-exec`,
`espidf-tls`.

Environment: `rustc 1.97.1 (8bab26f4f 2026-07-14)` for everything on the
host; `nightly-x86_64-unknown-linux-gnu` (`rustc 1.99.0-nightly`) only where
`-Zbuild-std` is needed for an esp-idf target. Base commit: `8870d5b`
(`origin/main`), `crates/hclient-core/src/caps.rs` contains `CancelSupport`,
so W1 is in.

---

## Summary

| question | answer | how |
|---|---|---|
| Q1 `'static` | **Not fatal, and the seam does not change.** `embassy-net`'s own `TcpClient`/`TcpClientState` is the bounded buffer pool; `TcpConnection<'static, N, TX, RX>` satisfies `TcpConnect::Stream` as it stands. | compiles + runs, §1 |
| Q1, third sub-point | **The strongest outcome is real.** The shipped `hclient-rt-smol`, unmodified, type-checks for `riscv32imc-esp-espidf`, and the whole `hclient::Client` runs under `embassy_executor` on a std host. | §2 |
| Q2 hyper | **hyper + tokio(`sync`) type-check for `riscv32imc-esp-espidf`.** No link step, so this is a `cargo check` claim, not a flashed binary. | §3 |
| Q3 `Spawn` | Implementable via `raw::TaskStorage` at the cost of one leaked `TaskStorage<F>` per call — **and not needed**: `Native` never asks for it. | §4 |
| Q3 `Blocking` | Not needed either, *unless* `SystemDns` is used. `IpLiteralOnly` and an embassy-net DNS resolver need nothing. | §4 |
| Q4 CI | riscv32 esp-idf: **`cargo check` only**, measured. `embassy-net` over a TAP device: **a real request, measured here**, in an unprivileged namespace. xtensa: **not on upstream rustc**, error quoted. | §5 |
| **not asked, but decisive** | **A naive embassy-net backend violates W1.** Dropping the `execute` future leaves the connection open as far as the server is concerned. Measured, with a control and with a fix that does not fit in `Drop`. | §6 |

---

## 1. Q1 — the `'static` question

### 1.1 What `TcpSocket<'a>` actually is

The premise "`TcpSocket<'a>` borrows both the buffers and the `Stack`" is
true of the *type*, but the struct is two words and holds neither buffer:

```rust
// embassy-net-0.9.1/src/tcp.rs:61
pub struct TcpSocket<'a> {
    io: TcpIo<'a>,
}
// :485
struct TcpIo<'a> {
    stack: Stack<'a>,
    handle: SocketHandle,
}
// :178
pub fn new(stack: Stack<'a>, rx_buffer: &'a mut [u8], tx_buffer: &'a mut [u8]) -> Self {
    let handle = stack.with_mut(|i| {
        let rx_buffer: &'static mut [u8] = unsafe { mem::transmute(rx_buffer) };
        let tx_buffer: &'static mut [u8] = unsafe { mem::transmute(tx_buffer) };
        i.sockets.add(tcp::Socket::new(...))
    });
```

The buffers are moved into the stack's `SocketSet`; `'a` exists only to stop
them being freed while the handle is alive. `Stack<'d>` is `Copy` and
covariant (`embassy-net-0.9.1/src/lib.rs:308`, `fn _assert_covariant`), and
in every real embassy program it is already `Stack<'static>` — the
`StackResources` live in a `static`. So the whole question reduces to: where
do the two `&'static mut [u8]` come from.

### 1.2 Four variants, four measured answers

`spikes/lifetime`, one cargo feature per variant so each error is captured
alone. All four use the same `embedded-io-async -> hyper::rt` adapter
(`spikes/lifetime/src/io.rs`, §1.5).

**v1 — buffers as locals in `connect`.** The obvious version.

```
$ cargo check --features v1_locals
error[E0515]: cannot return value referencing local variable `tx`
  --> src/v1_locals.rs:24:9
   |
20 |         let mut s = TcpSocket::new(self.stack, &mut rx, &mut tx);
   |                                                         ------- `tx` is borrowed here
...
24 |         Ok(HyperIo::new(s))
   |         ^^^^^^^^^^^^^^^^^^^ returns a value referencing data owned by the current function

error[E0515]: cannot return value referencing local variable `rx`
```

**v2 — `Box::leak` per connection.** Compiles, and the whole transport
accepts it:

```
$ cargo check --features v2_leak
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 0.07s
```

The assertion that made this worth running is in the spike:

```rust
fn assert_transport<T: hclient_core::unversioned::Transport>() {}
assert_transport::<hclient_native::Native<EmbassyNetLeak, NoTls, IpLiteralOnly>>();
```

So `TcpConnect`'s associated type is *not* what stands in the way — the
seam already admits an embassy socket. What v2 costs is 2 KiB per request,
forever. On a part with 256–512 KiB of RAM that is a bug, not a trade.

**v3 — the lifetime on the implementing type instead.**
`impl<'d> TcpConnect for EmbassyNetBorrowed<'d> { type Stream =
HyperIo<TcpSocket<'d>>; }` is legal and compiles *as a `TcpConnect` impl*.
It stops one level up:

```
$ cargo check --features v3_non_static
error: lifetime may not live long enough
  --> src/v3_non_static.rs:50:5
   |
48 |   pub fn native_accepts_a_borrowed_runtime<'d>() {
   |                                            -- lifetime `'d` defined here
...
   | |_____^ requires that `'d` must outlive `'static`
```

That is worth stating precisely, because it moves the blame: **the `'static`
is not in `hclient_rt::TcpConnect`. It is in `hclient-native`**, at
`crates/hclient-native/src/lib.rs:166-171`:

```rust
impl<R, T, D> Transport for Native<R, T, D>
where
    R: TcpConnect + Timer,
    R::Stream: 'static,
    T: TlsConnect,
    T::Stream<R::Stream>: 'static,
```

and it is there because `NativeBody` type-erases hyper's `Connection`
(`crates/hclient-native/src/h1.rs:130`):

```rust
type ConnFuture = Pin<Box<dyn Future<Output = hyper::Result<()>>>>;
```

— a `dyn Future` with no lifetime, i.e. `+ 'static`. A GAT or a lifetime on
`TcpConnect` would therefore buy nothing on its own; it would have to be
paired with `NativeBody<'a>` and `Native<'a, R, T, D>`. **That change is not
needed** (v4), so it is not proposed here.

**v4 — embassy's own bounded pool.** `embassy_net::tcp::client::TcpClient`
is exactly the "bounded buffer pool" the brief asks about, and it already
exists:

```rust
// embassy-net-0.9.1/src/tcp.rs:789
pub struct TcpClient<'d, const N: usize, const TX_SZ: usize = 1024, const RX_SZ: usize = 1024> {
    stack: Stack<'d>,
    state: &'d TcpClientState<N, TX_SZ, RX_SZ>,
    ...
}
// :864 — the connection owns a pool slot and gives it back
impl<'d, ...> Drop for TcpConnection<'d, N, TX_SZ, RX_SZ> {
    fn drop(&mut self) {
        unsafe {
            self.socket.close();
            self.state.pool.free(self.bufs);
        }
    }
}
```

With the pool held in a `'static` — a `static … StaticCell<TcpClientState<..>>`
on a real target, `Box::leak` once at construction in the spike — the
connection type is `TcpConnection<'static, N, TX, RX>`, and:

```
$ cargo check --features v4_static_pool
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 0.07s
```

with the same `assert_transport::<Native<EmbassyNetPooled, NoTls,
IpLiteralOnly>>()` inside.

**Answer to the brief's first bullet, in its own words.** Yes, a
`TcpSocket<'static>` can be obtained per connection without an unbounded
leak, and `Box::leak` of the *buffers* is indeed the obvious bug. A bounded
pool makes it sound; the pool is `TcpClientState<N, TX_SZ, RX_SZ>`, it is
**owned by the application**, not by our backend, exactly as the `Stack`
already is — the backend takes a `&'static TcpClient<'static, N, TX, RX>`
the same way it would take a `Stack<'static>`. `N`, `TX_SZ` and `RX_SZ`
become const parameters of the backend type. Nothing in `hclient-rt` or
`hclient-native` changes.

**Answer to the second bullet.** No change to `TcpConnect` is required, so
none is proposed. If one ever were, the honest smallest version is *not* a
GAT on `TcpConnect` — it is a lifetime on `Native` and `NativeBody`
together, because the `'static` is theirs.

### 1.3 It runs, not just compiles

`spikes/tuntap` puts the v4 shape on a real wire: `embassy-net` over a TAP
device inside an unprivileged user+network namespace, `hyper` on top through
the adapter, `hclient::Client` on top of that, all inside
`embassy_executor::Executor`. No `std::net` socket anywhere on the client
path — the TCP state machine is smoltcp's. The far end is an ordinary
blocking `std::net::TcpListener` on the kernel side of the tap.

```
$ unshare -Ur --net -- ./run.sh target-spike/debug/tuntap-spike
stack up: Some(Cidr { address: 192.168.69.2, prefix_len: 24 })
embassy-net + hyper + hclient: status=200 OK body="hello from the tap" in 904us
request 2 ok (pool N=2)
request 3 ok (pool N=2)
request 4 ok (pool N=2)
request 5 ok (pool N=2)
request 6 ok (pool N=2)
OK
```

Six requests through a two-slot pool is the leak check: a backend that
leaked a slot would fail on request 3.

### 1.4 `TcpAdoptStd` and `TcpOpts`

`TcpAdoptStd` is not implementable for the embassy-net shape — there is no
descriptor — and the seam already expresses that by simply not implementing
it. Nothing to design.

`TcpOpts` is a real gap and **not** a `Capabilities` question.

> **[corrected by the implementation]** "an embassy-net backend can honour
> none of them" is true of `TcpConnection`, which is what this section
> looked at, and false of a backend that owns the `TcpSocket` itself — which
> `hclient-rt-embassy` does, because the closing list of §6 requires it.
> `TcpSocket::set_nagle_enabled` and `set_keep_alive`
> (`embassy-net-0.9.1/src/tcp.rs:353,363`) make **two of the six** real:
> `nodelay` and `keepalive`. The remaining four are structurally absent, not
> merely unexposed: the local address belongs to the stack, the send and
> receive buffer sizes are the const parameters the pool was built with, and
> smoltcp has no `SO_REUSEADDR` to set. The shipped answer is
> `TcpConnect::APPLIES` plus `TcpOpts::reject_unsupported`, both added to
> `hclient-rt`: what a runtime cannot apply, it refuses by name.
 `Native`
passes `&TcpOpts` straight into `TcpConnect::connect`; an embassy-net
backend can honour none of `nodelay`, `keepalive`, `local_address`,
`send_buffer_size`, `recv_buffer_size`, `reuse_address` — smoltcp has
`set_keep_alive` and `set_nagle_enabled` on the raw `tcp::Socket`, neither
reachable through `TcpConnection`. There is **no mechanism anywhere in
`hclient-rt` for a runtime to report which options it applied**: `connect`
returns `io::Result<Stream>` and nothing else. `Capabilities` is the wrong
place for it (that is trap #1 from the design doc — it describes the
transport's contract with the caller, not the socket's), so this needs its
own answer at the `hclient-rt` seam, or a documented "silently ignored",
which this project does not accept. **This is the one place W7 genuinely
does touch a seam, and it is `TcpOpts`, not `TcpConnect::Stream`.**

### 1.5 The `hyper::rt` <-> `embedded-io-async` adapter

88 lines, safe, no allocation per poll (`spikes/lifetime/src/io.rs`). The
trick: embassy's socket futures are stateless — each poll either completes
or registers a waker *in the socket*, never in the future
(`embassy-net-0.9.1/src/tcp.rs:518` and `:556`, `TcpIo::read`/`TcpIo::write` are
plain `poll_fn` closures over `register_recv_waker`/`register_send_waker`) —
so a fresh one-shot future can be built, stack-pinned with
`core::pin::pin!` and polled exactly once per `poll_*`, then dropped:

```rust
let n = {
    let mut fut = std::pin::pin!(this.inner.read(&mut this.scratch));
    match fut.as_mut().poll(cx) { ... }
};
buf.put_slice(&this.scratch[..n]);
```

Two costs, both real and both recorded rather than smoothed over:

- one copy through a 2 KiB scratch buffer per read, same as
  `hclient_rt::FuturesIo` already pays and for the same reason (zero-copy
  needs `unsafe`, the workspace forbids it);
- **`poll_shutdown` cannot do what hyper asks.** `embedded_io_async::Write`
  has no shutdown, and `TcpConnection` exposes nothing else. The spike
  forwards it to `poll_flush`. For HTTP/1 without upgrades this has not
  bitten in any of the runs above, but it is a half-close hyper believes it
  performed and did not. Related to §6.

  > **[corrected by the implementation]** Also an artefact of using
  > `TcpConnection`. `hclient-rt-embassy` owns the `TcpSocket`, so
  > `poll_shutdown` is `close()` followed by `flush()` — the FIN hyper asked
  > for, actually sent. It turns out to carry more weight than "tidier":
  > with connection reuse off it is hyper's shutdown, not the pool's `Drop`,
  > that closes a *completed* exchange, which is why mutating the pool's
  > `close()` away is caught by the cancellation test and not by the
  > six-request one.

---

## 2. Q1, third sub-point — the outcome worth looking hardest for

**It is real, and it is the recommendation.**

### 2.1 The whole client under an embassy executor, on a std host

`spikes/under-embassy` defines `EmbassyStd`: `Timer` on `embassy_time`
(`type Instant = embassy_time::Instant`, not `std::time::Instant` — the
seam's associated type is what makes that possible), `TcpConnect` on
`async_net::TcpStream` + `FuturesIo`, `Blocking` on `blocking::unblock`. The
generic body is the same shape as `two_runtimes.rs`'s `fetch_once<R>`, with
no `#[cfg]` and no mention of embassy.

```
$ cargo run
A embassy-executor + EmbassyStd: body="under-embassy!!" in 607us
B embassy-executor + hclient_rt_smol::Smol (unmodified): body="under-embassy!!"
C embassy_time::Timer::after(250ms) under embassy executor: 250.149989ms
OK
```

Line B is the load-bearing one: **the crate that ships today,
`hclient-rt-smol`, unmodified, drives a real HTTP/1.1 request while running
inside `embassy_executor::Executor`.** This works because `async-io` runs
its reactor on its own thread, so `Async<T>` futures are polled by whatever
executor holds them.

**Sensitivity control** — otherwise the above proves nothing about the
executor. `hclient_rt_tokio::Tokio` must *not* work there:

```
$ cargo run --bin tokio_control
thread 'main' panicked at tokio-1.53.1/src/net/tcp/stream.rs:164:18:
there is no reactor running, must be called from the context of a Tokio 1.x runtime
```

**Not a busy-spin**, measured the way AGENTS.md records for Task 12
(`/proc/self/stat` around a server that answers after 600 ms):

```
$ cargo run --bin cpu_measure
server answers after 600ms: body="slow" wall=601.07205ms cpu=10ms
```

### 2.2 Does the same thing compile for esp-idf?

`spikes/espidf-asyncio` — the shipped crates, untouched, for the real
target. No ESP-IDF sysroot is present here, so this is `cargo check` with
`-Zbuild-std`: every crate in the graph is compiled, there is no link step.

```
$ cargo +nightly check --target riscv32imc-esp-espidf -Zbuild-std=std,panic_abort
    Checking polling v3.11.0
    Checking blocking v1.6.2
    Checking socket2 v0.6.5
    Checking async-net v2.0.0
    Checking hclient-native v0.1.0
    Checking hclient-dns-system v0.1.0
    Checking hclient-rt-smol v0.1.0
    Checking espidf-asyncio v0.0.0
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 3.19s
```

The crate under check instantiates both
`Native<Smol, NoTls, IpLiteralOnly>` and
`Native<Smol, NoTls, SystemDns<Smol>>` — the latter exercising `Blocking`.

This is not luck. `polling` 3.11 and `async-io` 2.6 carry explicit ESP-IDF
support in their own source:

```
polling-3.11.0/src/lib.rs:847      #[cfg(target_os = "espidf")] const DEFAULT_CAPACITY: usize = 32;
polling-3.11.0/src/poll.rs:766     #[cfg(any(target_os = "espidf", target_os = "hermit"))] mod notify { ... eventfd ... }
async-io-2.6.0/src/reactor.rs:46   /// ESP-IDF - being an embedded OS - does not need so many timers
                                   #[cfg(target_os = "espidf")] const TIMER_QUEUE_SIZE: usize = 100;
```

And the embassy executor and timer check for the same target
(`spikes/espidf-exec`, `embassy-executor` `arch-std` + `embassy-time` `std`):

```
$ cargo +nightly check --target riscv32imc-esp-espidf -Zbuild-std=std,panic_abort
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 2.86s
```

### 2.3 Two caveats that are not "yes but"

- **`async-io` is heavy for an MCU, and the ecosystem already forked it.**
  `esp-idf-svc` does not depend on `async-io`; it depends on
  [`async-io-mini`](https://github.com/sysgrok/async-io-mini) under the
  `async-io` name: `async-io = { version = "0.4", package = "async-io-mini",
  default-features = false, features = ["futures-io"] }`. Its README states
  the reason: real `async-io` "needs at least 8K stack" for any thread
  polling sockets and has non-trivial reactor memory, whereas async-io-mini
  "needs < 3K of stack with ESP-IDF" and "~500 bytes" of footprint, at the
  price of being "hard-coded to the `select` syscall" with no timers. It is
  API-compatible with `async_io::Async`. So the real esp-idf build would
  most likely swap the crate, not the code — but **unverified**: I did not
  compile `async-io-mini`, and `hclient-rt-smol` uses `async-net` and
  `blocking` as well as `async-io`. What would settle it: a
  `cargo check --target riscv32imc-esp-espidf` of a runtime crate written
  against `async-io-mini` directly.
- **`esp-idf-svc` itself is unverified here.** Its build script requires the
  ESP-IDF toolchain. Its `Cargo.toml` does carry an `embassy-time-driver`
  feature (`embassy-time-driver = ["dep:embassy-time-driver",
  "embassy-time-queue-utils"]`), which is the officially supported way to
  give `embassy_time` a driver on esp-idf — better than `embassy-time/std`,
  which is what the host spikes used.

---

## 3. Q2 — is hyper usable there at all

**Yes, as far as a compile can tell.** `spikes/espidf-check` puts `hyper`
1.11, `hclient-native`, `hclient`, `embassy-net`, `embassy-time` and
`embassy-executor` in one crate and checks them for the real target:

```
$ cargo +nightly check --target riscv32imc-esp-espidf -Zbuild-std=std,panic_abort
    Checking tokio v1.53.1
    Checking hyper v1.11.0
    Checking hclient v0.1.0
    Checking hclient-native v0.1.0
    Checking embassy-net v0.9.1
    Checking espidf-check v0.0.0
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 16.02s
```

```
$ cargo tree -e normal --target riscv32imc-esp-espidf -i tokio -f "{p} {f}"
tokio v1.53.1 default,sync
└── hyper v1.11.0 client,http1
```

The same `[default, sync]` leaf AGENTS.md already records, now measured on
an esp-idf target instead of a desktop one. Supporting reads: `hyper` 1.11
has **zero** `target_os = "espidf"` conditionals in its source; `tokio`
1.53.1 has **two**, both in `src/net/unix/ucred.rs`, which the `sync`
feature does not compile.

**What this claim is not.** There is no link step, no ESP-IDF sysroot, no
`ldproxy`, no flashed binary. `cargo check` proves the sources type-check
for the target's cfg set and pointer width; it does not prove the result
links, fits in flash, or runs. Settling that needs
`espup`/`esp-idf-template` and either hardware or QEMU — **unverified**.

**One thing that measurably does *not* check**, and it matters for a real
device: `hclient-tls-rustls`.

```
$ cargo +nightly check --target riscv32imc-esp-espidf -Zbuild-std=std,panic_abort
error: failed to run custom build command for `ring v0.17.14`
  cargo:warning=Compiler family detection failed due to error: ToolNotFound:
    failed to find tool "riscv32-esp-elf-gcc": No such file or directory (os error 2)
  error occurred in cc-rs: failed to find tool "riscv32-esp-elf-gcc"
```

That is an absent C toolchain here, not a verdict — the esp-idf setup ships
`riscv32-esp-elf-gcc` — so it is **unverified** rather than impossible. It
does mean the TLS half of a real esp-idf build is untested by anything in
this report, and `NoTls` is what every spike used.

### Why reqwless does not use hyper, and whether it applies to us

Its README is explicit about the requirement, and not about the reasoning:
*"implements an HTTP client that can be used in `no_std` environment, with
any transport that implements the traits from the `embedded-io` crate. No
alloc or std lib required!"* No statement about hyper appears anywhere in
it, so any "reqwless rejected hyper because X" would be invention. What is
checkable is that hyper cannot satisfy that requirement: it needs `std`, it
needs `alloc`, and it pulls `tokio` unconditionally (re-measured above).

**The trait-shape mismatch is not the reason, and that is the useful part.**
The plausible technical story — "hyper's `rt::Read`/`Write` are poll-based,
embedded is async-fn-based, they don't fit" — is false: the adapter is 62
safe lines (§1.5) and drove six real requests over embassy-net (§1.3).

So reqwless's constraint (`no_std`, no alloc) is **not ours**: the target is
esp-idf *with* `std`, stated by the owner and consistent with everything
above.

**Recommendation on the two shapes.** Build the hyper shape, not the
"embassy backend drives `hclient-proto` directly" shape.

- The hyper shape is measured working end to end, today, twice (§1.3, §2.1).
- The sans-io shape would be a second HTTP/1 implementation in this
  workspace with its own bugs, its own body model and its own cancellation
  semantics, and `Transport`'s `Body` associated type means the two would
  not share `NativeBody`. It is the strongest evidence the sans-io core was
  worth building, and it is also a vertical, not a task.
- If flash or RAM later says hyper does not fit, that is exactly the
  measurement that should trigger the sans-io shape — and it cannot be made
  here (§3, "what this claim is not").

---

## 4. Q3 — the two seams the design doc suspects

### `Spawn`

**Implementable, and not needed.**

`#[embassy_executor::task]` allocates a compile-time `TaskPool<F, N>` for
one concrete `F`, but the layer under it is public and safe:
`embassy_executor::raw::TaskStorage::<F>::spawn(&'static self, impl FnOnce()
-> F) -> SpawnToken<_>` (`embassy-executor-0.9.1/src/raw/mod.rs:222`), and
`AvailableTask::claim` takes `&'static TaskStorage<F>`. With `alloc` present
that reference can be produced:

```rust
impl<F: Future<Output = ()> + 'static> Spawn<F> for Embassy {
    fn spawn(&self, f: F) {
        let storage: &'static TaskStorage<F> = Box::leak(Box::new(TaskStorage::new()));
        self.spawner.spawn(storage.spawn(move || f)).expect("spawn");
    }
}
```

```
$ cargo run --bin spawn_probe
spawned 3 !Send futures on embassy's executor, ran = 3
cost: one leaked TaskStorage<F> per spawn call
OK
```

So the generic form *is* implementable, including for `!Send` futures (the
probe spawns three closures over an `Rc<Cell<u32>>`), at one leaked
`TaskStorage<F>` per call. There is no way to reclaim it: nothing lets a
caller ask whether a slot has been released, and `AvailableTask::claim`
needs the `&'static` before it can tell you. A bounded `TaskPool<F, N>`
cannot be sized by a generic `impl<F>`.

**Not implementing it is sufficient, and that is verified in the code, not
trusted from a sentence.** `grep -rn "Spawn" crates/` finds it only in
`hclient-rt` itself, the two runtime crates, and `hclient-rt-pair-check`'s
property test — nothing in `hclient-native` or `hclient` uses it. Two
independent confirmations:

```
$ cargo nextest run -p hclient-native --test h1
        PASS [   0.004s] (1/2) hclient-native::h1 works_on_a_bare_futures_executor_with_no_spawn
        PASS [   0.004s] (2/2) hclient-native::h1 body_keeps_driving_the_connection_after_headers
     Summary [   0.004s] 2 tests run: 2 passed, 0 skipped
```

and, more directly, the runtime types in `spikes/lifetime` (v4) and
`spikes/tuntap` implement **only** `TcpConnect + Timer`. `Native<…>:
Transport` resolved and six requests went over the wire (§1.3).

### `Blocking`

**Also not needed, with one named consequence.** `Native`'s `Transport`
impl asks for `R: TcpConnect + Timer` and nothing else
(`crates/hclient-native/src/lib.rs:166`). What breaks without `Blocking` is
exactly one thing: `SystemDns<B>` is `impl<B: Blocking> Resolve for
SystemDns<B>` (`crates/hclient-dns-system/src/lib.rs:177`), so the
`getaddrinfo` resolver becomes unusable. That is not a loss on this target:
`IpLiteralOnly` works, and `embassy_net::Stack` has its own `dns_query`,
which is an `async fn` needing no thread pool at all — a `Resolve` impl over
it is a small, obvious piece of the eventual backend (**unverified**: not
written or run here; what would settle it is an `impl Resolve` over
`Stack::dns_query` plus a live lookup on the tuntap harness).

If the "third way" (§2) is taken instead, `Blocking` *is* available —
`blocking::unblock` checks for esp-idf (§2.2) — so `SystemDns` keeps
working. That is one more reason to prefer that shape.

---

## 5. Q4 — what CI could actually run

**Reachable target: `riscv32imc-esp-espidf`, on upstream nightly, check
only.** Measured in §2.2 and §3. It is a `std` target (`*-espidf`). It needs
`-Zbuild-std=std,panic_abort` and therefore nightly + `rust-src`; no
Espressif fork, no `espup`, no ESP-IDF checkout. That is a genuinely cheap
CI job and it is honest about proving only that the trait bounds and cfgs
line up — the same shape as `portable-example-three-targets`.

**xtensa is not reachable on upstream rustc.** The target is listed
(`rustc +nightly --print target-list` includes `xtensa-esp32-espidf`), but
the LLVM in upstream nightly cannot codegen it:

```
$ cargo +nightly check --target xtensa-esp32-espidf -Zbuild-std=std,panic_abort
rustc-LLVM ERROR: Cannot select: 0x723e68d8b150: f32 = fp16_to_fp ...
In function: _RNvMNtCscN8rvJGfUVn_4core3f16C3f166is_nanCsbAGSlCo7CkM_17compiler_builtins
error: could not compile `compiler_builtins` (lib)
...
rustc-LLVM ERROR: Cannot select: ... src/num/imp/dec2flt/mod.rs:155:9
error: could not compile `core` (lib)
```

So the brief's assumption holds, now with the error attached: xtensa needs
Espressif's fork via `espup`. Not worth a CI job.

**The acceptance can run on Linux, over a real network stack.** This is the
best result in §5 and it was measured, not predicted:
`embassy-net` + `embassy-net-tuntap` + a TAP device, inside an
**unprivileged** user+network namespace — no root, no persistent change to
the host:

```sh
# spikes/tuntap/run.sh, invoked as: unshare -Ur --net -- ./run.sh <binary>
ip tuntap add dev tap0 mode tap
ip addr add 192.168.69.1/24 dev tap0
ip link set tap0 up
```

Output in §1.3. This is a `two_runtimes`-grade acceptance: a real request,
a real TCP state machine, a real server, on a GitHub-runner-shaped OS —
what it does *not* prove is that the same code links and runs on an
xtensa/riscv MCU.

Two things about the CI environment itself are **unverified**: whether
GitHub's `ubuntu-latest` still permits unprivileged user namespaces (recent
Ubuntu restricts them via `kernel.apparmor_restrict_unprivileged_userns`),
and whether the simpler `sudo ip tuntap add` route is acceptable. Runners do
give passwordless `sudo`, so the fallback exists; settling it takes one
throwaway workflow run. **QEMU's esp32 machine was not tried at all** — the
tuntap route is strictly cheaper and gives more, so it was not worth the
budget.

**Honest summary for the deliverable's own wording:** riscv32 esp-idf is
compile-only; the behavioural acceptance is embassy-executor +
embassy-net-over-tuntap on x86-64 Linux; nothing in this report has run on
esp32 hardware.

---

## 6. Not asked, and it changes the plan: W1

The design doc says W7 is best done after W1 "so the cancellation contract
is something the new backend is written against rather than retrofitted".
That was the right instinct, and the answer is worse than expected.

`Transport::execute`'s contract now requires that dropping the future stops
the transfer, and `Native` satisfies it structurally: the future owns
everything, so dropping it drops the socket. With embassy-net, dropping the
socket does not close anything the peer can see.

`spikes/tuntap/src/bin/cancel.rs` — same observer as
`crates/hclient-native/tests/cancel.rs`, i.e. the server's own socket, never
the client's task. The client drops the `send` future via
`embassy_time::with_timeout` while the server is deliberately not
responding:

```
$ unshare -Ur --net -- ./run.sh target-spike/debug/cancel
send future dropped on timeout: true
VERDICT: server saw NOTHING for 3s: the connection is still open
OK
```

`spikes/tuntap/src/bin/cancel2.rs` isolates the cause and finds the fix,
with a control so "nothing" cannot mean "the observer is broken":

```
$ unshare -Ur --net -- ./run.sh target-spike/debug/cancel2
1. TcpSocket dropped                       -> NOTHING for 2s (still open)
2. abort() + one stack poll, then dropped  -> RST
3. control: std::net::TcpStream dropped    -> EOF (peer closed cleanly)
```

The mechanism is in the source. `TcpConnection::drop` calls
`self.socket.close()` (queue a FIN) and then the `socket` field is dropped,
and `impl Drop for TcpSocket` is
(`embassy-net-0.9.1/src/tcp.rs:466`):

```rust
fn drop(&mut self) {
    self.io.stack.with_mut(|i| i.sockets.remove(self.io.handle));
}
```

The socket is removed from smoltcp's `SocketSet` before the stack is ever
polled again, so the queued FIN — or a queued RST from `abort()` — never
becomes a packet. Case 2 shows the packet does go out if the stack runs once
between the abort and the drop. **A synchronous `Drop` cannot arrange
that.**

Consequences for the W7 task, all of which have to be decided before code
is written:

1. A backend built the straightforward way must report
   `CancelSupport::None`. That is what `CancelSupport` is for, and it is
   the honest answer — but it makes embassy the first backend that cannot
   cancel, right after W1 concluded all three could.

   > **[corrected by the implementation]** There is nowhere to report it
   > from. `Capabilities` belongs to the `Transport`, and the transport here
   > is `hclient_native::Native`, which sets `cancel_on_drop =
   > CancelSupport::Supported` unconditionally
   > (`crates/hclient-native/src/lib.rs`) on the strength of reasoning about
   > its own future. A runtime plugged in underneath has no channel to lower
   > it, and giving it one is a change to `hclient-native`. So option 1 is
   > not the cheaper of two choices — it is unavailable without that change,
   > and would leave the capability lying meanwhile. The shipped backend
   > takes option 2.
2. Making it `Supported` needs the socket to outlive the drop by at least
   one stack poll: a "closing" list owned by whoever owns the `Stack`, fed
   from `Drop`, drained by the stack task. That is a design, not a detail,
   and it is per-backend (nothing above the `Transport` seam changes).
3. **The "third way" of §2 does not have this problem at all.** A
   `std::net::TcpStream`/`async_net::TcpStream` closes on drop like every
   other backend in this workspace, and the kernel sends the FIN. Case 3
   above is that control, measured.

---

## Found while implementing (not in the original research)

Two behaviours that no spike hit, both measured by
`hclient-rt-embassy`'s acceptance and both now shaping its code.

**`embassy_time` cannot be used below `TcpConnect::connect`.**
`hclient_native::connect::drive` polls its connect attempts through
`futures_util::stream::FuturesUnordered`, which polls each future with a
waker of its own; embassy's *integrated* timer queue refuses any waker it
did not make:

```text
panicked at embassy-executor-0.9.1/src/raw/waker.rs:38:
  Found waker not created by the Embassy executor.
  `embassy_time::Timer` only works with the Embassy executor.
  ...
  <FuturesUnordered<..connect..> as Stream>::poll_next
  hclient_native::connect::drive::{closure#0}
```

The backend's own `Timer::sleep` is unaffected — `with_connect_timeout` and
the Happy Eyeballs stagger are hand-rolled `poll_fn`s that pass `cx`
straight through, and a live connect timeout test pins that. Anything
inside `connect` has to find another clock; the socket pool uses smoltcp's
`Socket::set_timeout`, which the stack enforces while dispatching and which
needs no waker at all. An application that wants `embassy_time` to work
regardless would have to build it with a `generic-queue-*` feature —
**unverified**, not needed here, and settled by trying it.

**One abandoned connect starves ARP for the whole interface.** A socket
given up on before the peer ever answered ends up `Closed` with its
four-tuple still set, and smoltcp then wants to emit exactly one RST for it
— retrying once a second for ever when the peer's hardware address cannot
be resolved, because the tuple is only cleared once that RST is actually
sent. Each attempt spends the neighbour cache's **global** one-per-second
ARP budget, so a second socket dialling a perfectly reachable host never
gets an ARP request out:

```text
#1: neighbor 192.168.69.9 silence timer expired, rediscovering
outgoing segment will abort connection / sending RST|ACK
address 192.168.69.9 not in neighbor cache, sending ARP request
#3: neighbor 192.168.69.1 silence timer expired, rediscovering
outgoing segment will send data or flags / sending SYN
#3: neighbor 192.168.69.1 missing, silencing until t+1.000s
```

— repeating until the watchdog fired 30s later, with no ARP for `.1` ever
sent. The way out is `connect`'s own `reset()`, which clears the tuple, so
the pool hands out finished sockets **first**: the ghost is cleared at the
next request instead of never. It is the same shape as the `TcpOpts` gap
and as h3's `AsFd` finding (`docs/h3-research.md`): what the OS socket
offers, embassy-net structurally does not.

---

## Found by mutating the tests (not in the original research)

The backend's own suite was mutation-tested afterwards, and two of the
mutations survived. Both are recorded here because both were **holes in the
tests, not in the code** — the code was right and nothing was checking it.

**The closing list's *wait* was unreachable.** §6's fix has two halves: keep
the socket alive past the drop so the FIN can be dispatched, and, when the
slot is reclaimed, wait for that FIN before aborting what is left. The first
half was well covered — removing the list, or the `close()` that feeds it,
fails the cancellation scenario at once. The second was not covered at all:
deleting `sockets::finish_closing`'s `sock.flush().await` passed the whole
crate suite, 15/15. An `eprintln!` at the top of that function explains why —
**zero calls across all seven scenarios**. Every one of them awaits something
between releasing a socket and asking for the next, so the stack has always
run by then, `Inner::reclaim_finished` finds the socket already `Closed`, and
the closing list is never popped.

Reaching it takes a pool of exactly one slot and a release whose next
`acquire` lands in the *same executor turn* — the drop queues the FIN and
wakes the stack task, but waking is not running.
`a_slot_reclaimed_while_still_closing_waits_for_its_fin` arranges that, and
the probe confirms the state it produces: `finish_closing entered,
state=FinWait1`. Under the mutation that scenario reports `StillThere` where
it owes `Eof`: the abort replaces the undispatched FIN, and `connect`'s own
`reset()` then clears the pending RST too, so the far end learns nothing at
all — §6's silent teardown, reached from the other direction.

**The observer could not have seen an RST.** `ClientEnd` separates `Eof` from
`Reset` precisely so a FIN quietly swapped for an abort fails a test. Nothing
exercised the distinction: every scenario ended in a FIN, so `observe`'s
`ConnectionReset` arm was never taken, and folding it into `Eof` passed 16/16.
`an_abort_with_a_stack_poll_is_seen_as_a_reset` puts a real RST on the wire —
`abort()` with the stack given the one poll `TcpSocket::drop` denies it, which
is case 2 of the `cancel2` spike above, now in the tracked suite. It is also
the control for `naive`: the two differ by exactly one `await`, so "the server
saw nothing" is a fact about the missing poll and not about the tap link
swallowing packets.

Everything else held. The mutations that were killed, each by a named test:
the closing list itself and the `close()` in `PooledSocket::drop` (both by the
cancellation scenario, and the second by that one *only* — the six-request one
passes, since with reuse off it is hyper's shutdown that closes a completed
exchange); `TcpConnect::APPLIES`' default flipped from `NONE` to `ALL` (by
`a_runtime_that_declares_nothing_applies_nothing`, the only test that reads
it); each of the four fields embassy cannot apply flipped to `true` in turn,
one mutation per field, each caught both by the unit table and by the live
`connect`; `UnsupportedTcpOpts::names` made to name every option regardless of
what was asked for; the `reject_unsupported` call deleted from
`Embassy::connect` (by the live scenario alone — the unit tests call it
directly and cannot notice); and the two ends-with-a-FIN assertions inverted
to expect an RST.

---

## Recommendation

**The shape I would build:** `hclient-rt-espidf` — `Timer` on
`embassy_time` (driver from `esp-idf-svc`'s `embassy-time-driver` feature,
not `embassy-time/std`), `TcpConnect`/`TcpAdoptStd` on esp-idf's real `std`
sockets through `async-io-mini`, `Blocking` on `blocking::unblock`, run
under `embassy_executor` — because it is the only variant where the shipped
`hclient-rt-smol` already type-checks for `riscv32imc-esp-espidf` unchanged,
the whole client already runs under an embassy executor with zero CPU spin,
and cancellation keeps working; keep the `embassy-net` backend of §1 as the
`no-std`-stack fallback for boards where `std` sockets are not the network
path, since it too compiles and runs.

**The seam change it needs:** none in `TcpConnect`, `Timer`, `Spawn`,
`Blocking`, `Transport` or `Capabilities` — one only, and it is `TcpOpts`:
`hclient_rt::TcpConnect::connect` gives a runtime no way to say which socket
options it could not apply, and both embassy shapes would silently ignore
several, which this project does not allow.

> **[done, W7 implementation]** `TcpOptsSupport`, `TcpConnect::APPLIES`
> (defaulting to `NONE`, so silence understates rather than overstates) and
> `TcpOpts::reject_unsupported` now live in `hclient-rt`; the two shipped
> runtimes declare `ALL` and are otherwise untouched. What is still open is
> enforcement above the runtime: `Native::tcp_opts` could refuse once at
> build time instead of once per connect, and that line belongs in
> `hclient-native`.

**The one thing that would kill it:** the link step. Everything about
esp-idf in this report is `cargo check` — if `hyper` + `rustls`/`ring` +
ESP-IDF's newlib do not link, or do not fit in the flash and RAM budget of
the part, the hyper shape dies and the replacement is
`hclient-proto`-over-`embedded-io-async`, a vertical rather than a task.
