# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.1.0-alpha.19](https://github.com/GamePad64/hclient/compare/hclient-native-v0.1.0-alpha.18...hclient-native-v0.1.0-alpha.19) - 2026-10-06

### Added

- *(error)* one MissingScheme, named the same under every backend

### Fixed

- *(proxy)* the deferred audit minors, verified one by one

### Other

- *(proxy)* the freeze — 0.1.0
- *(proxy)* the root is doors — five pub modules, twelve names

## [0.1.0-alpha.18](https://github.com/GamePad64/hclient/compare/hclient-native-v0.1.0-alpha.17...hclient-native-v0.1.0-alpha.18) - 2026-10-05

### Added

- *(error)* a URL this client cannot serve is Uri, not Unsupported
- *(proxy)* [**breaking**] a plain CONNECT defaults to HTTP/2, and each enum says why it is or is not exhaustive
- *(native)* extended CONNECT tunnels over HTTP/3 carry their datagrams, on connections of their own
- *(native)* lend a filter CONNECT and extended CONNECT tunnels over HTTP/2
- *(native)* HTTP/3 goes through a filter that opens a datagram path, and a refusal falls back to its stream
- *(native)* the QUIC arm stages a connection over a filter's path, pooled apart from direct ones
- *(native)* a QUIC endpoint over a filter's datagram path
- *(native)* lend a filter the runtime's UDP and the proxy's addresses
- *(rt)* Io names the byte-stream seam's four bounds, and hclient-proxy re-exports it
- *(native)* [**breaking**] DialStream, and the transport runs TLS a filter asks for over it
- *(proxy)* [**breaking**] a proxy reached over a socket is IpcProxy, which only proxy_over_ipc takes
- *(native)* [**breaking**] Native::egress installs an external filter, and a filter may wrap the stream
- *(native)* NativeDial lends the transport's connect path to a filter

### Fixed

- the rest of clippy 1.99's assert_is_empty, everywhere
- *(proxy)* [**breaking**] `TunnelRequest::version` is an `Option`, and nothing rewrites it
- *(proxy)* [**breaking**] `SendEgressFilter::open_datagrams_send` has no default
- *(native)* QUIC over a filter's path starts at 1200 and discovers upwards
- *(native)* a proxy whose HTTP/3 tunnel failed is not asked again while HTTP/2 can carry one
- *(native)* an HTTP/2 tunnel is refused unless the proxy's TLS selected `h2`
- *(native)* quinn is named inside `mod http3` again
- *(native)* a filtered stream fallback spends the connect bound once, and a non-h3 demand takes the stream
- *(native)* an HTTP/3 tunnel attempt leaves HTTP/2 a real share of the time
- *(native)* an HTTP/2 tunnel wakes its reader and its writer, whichever task polled last
- *(native)* opening a filter's path and the QUIC handshake over it spend one connect bound
- *(proxy)* [**breaking**] a bypass pattern in no accepted shape is refused by name, and the two dialects are one
- *(proxy)* a bracketed v6 TLS proxy is checked by its bare address; the body behind a failing connection is read in the tests
- *(native)* a response already read wins over the connection failing behind it
- *(proxy)* the review's minors — IpcProxy holds its address once, push_ipc and drive_exact say what they cannot see
- *(native)* the filter seam's contract, a live connect budget, and the stream capability read
- *(native)* the QUIC arm no longer leaves a proxy or a Unix socket

### Other

- *(proxy)* the freeze stays a plan — 0.1.0-alpha.16
- the rendered pages no longer name a work item a reader cannot follow
- the suppressions are #[expect], where the lint fires in this build
- *(proxy)* [**breaking**] hclient-proxy 0.1.0
- *(proxy)* [**breaking**] the test doubles are behind a `test-util` feature
- *(native)* the stream fallback's budget is read off the lent context, not raced
- HTTP/3 through a filter, SOCKS5 UDP, tunnels and the MASQUE experiment, written down
- *(native)* HTTP/3 through a real SOCKS5 UDP relay, and every way the relay can say no
- *(proxy)* [**breaking**] a route borrows its pool key from the filter, and absolute-form can grow
- *(proxy)* [**breaking**] Dial::connect and connect_ipc may borrow their arguments
- *(proxy)* [**breaking**] every setter fails as an Error, drive_exact says which protocols it is right for, and the second audit is written down
- *(proxy)* [**breaking**] the machine's settings arrive as Rules, and first-match-wins has one home
- *(proxy)* [**breaking**] Attempt is Failed or Unsupported, and a filtered decision carries a Route
- *(proxy)* [**breaking**] the surface a stable version promises — Target gets a constructor, Reach goes private, ProxySpokeFirst gets a name
- *(proxy)* the review's minors — Proxy::tls and the default certificate, the crate doc points at connect_tls, hook order, a stale AGENTS.md claim
- *(native)* HTTPS proxies end to end — TLS in TLS, trust, reuse, the connect bound, the erased path
- *(native)* connect's doc back on connect, the filter path described, stale egress references gone
- *(native)* https, hooks, absolute-form and the connect bound through an external filter
- *(native)* mixed protocols, SOCKS5 over a Unix socket, tunnel reuse and the connect bound through a filter
- *(native)* [**breaking**] one egress filter decides where a connection goes; P is gone
- *(proxy)* the handshake driver lives beside the handshakes
- *(proxy)* hclient-proxy stays in the pre-release series
- *(proxy)* hclient-proxy leaves the pre-release series at 0.1.0

## [0.1.0-alpha.17](https://github.com/GamePad64/hclient/compare/hclient-native-v0.1.0-alpha.16...hclient-native-v0.1.0-alpha.17) - 2026-09-25

### Added

- *(proxy)* [**breaking**] the surface a stable number would promise, and a bypass that meant two things

## [0.1.0-alpha.16](https://github.com/GamePad64/hclient/compare/hclient-native-v0.1.0-alpha.15...hclient-native-v0.1.0-alpha.16) - 2026-09-25

### Added

- *(native)* [**breaking**] endpoint and the two default constants leave the public API
- *(native)* [**breaking**] Native has its documentation back, and four more items leave the surface
- *(native)* [**breaking**] the root holds the transport, and five modules hold the rest
- *(native)* [**breaking**] caps is private, and Disagreement lives at the root
- *(rt)* [**breaking**] UDP is unconditional in both runtimes
- *(rt-smol)* [**breaking**] SmolIo is opaque, and reaches the socket through AsFd

### Other

- every crate's front page starts with what it is and a working example
- doc comments speak to the docs.rs reader, and the argument moves beside them
- require rustls 0.23.45
- *(tls)* hclient-tls 0.1.0, and the seven requirements naming it
- *(tls)* what a backend over another TLS library owes Shutdown

## [0.1.0-alpha.15](https://github.com/GamePad64/hclient/compare/hclient-native-v0.1.0-alpha.14...hclient-native-v0.1.0-alpha.15) - 2026-09-23

### Added

- *(rt)* [**breaking**] Shutdown is half-close and nothing else
- *(rt)* [**breaking**] IPC is a trait of its own, and hclient-rt's modules follow its seams
- *(rt)* [**breaking**] the three support reports share one name, one shape and one refusal
- *(rt)* [**breaking**] one connect for every same-machine endpoint, so named pipes are not a major version
- *(tls)* [**breaking**] TlsRequest is built with new(), and the reserved 0-RTT slots go before the freeze
- *(altsvc)* an `AltSvcStore` over the byte KV

### Fixed

- *(native)* unix_socket's example bounds its runtime on IpcConnect
- *(kv)* a decoder written to refuse was panicking on a bad timestamp

### Other

- *(rt)* hclient-rt 0.1.0, and the eleven requirements naming it
- *(tls)* stop describing the QUIC seam as it was two designs ago
- *(tls)* [**breaking**] the QUIC seam carries its own config, and links no stack
- *(altsvc)* [**breaking**] drop `MemoryStore`, and make the byte store share on clone
- *(tls)* [**breaking**] take `quinn-proto` out of the QUIC TLS seam
- *(rt)* [**breaking**] take `hyper` out of the byte-stream seam

## [0.1.0-alpha.14](https://github.com/GamePad64/hclient/compare/hclient-native-v0.1.0-alpha.13...hclient-native-v0.1.0-alpha.14) - 2026-09-18

### Other

- *(dns)* [**breaking**] `hclient-dns` leaves the pre-release series
- *(native)* pin the gRPC `content-length` rule where it has a subject

## [0.1.0-alpha.13](https://github.com/GamePad64/hclient/compare/hclient-native-v0.1.0-alpha.12...hclient-native-v0.1.0-alpha.13) - 2026-09-18

### Fixed

- HTTP/2 requests now declare `content-length` when the body's size is known.
  hyper writes that header on the HTTP/1 path and h2 does not — it frames the
  body and has no need of it — so a server that sizes the body from the header
  rather than reading to end-of-stream saw an empty request. An OCI registry
  rejected every blob upload this way, reporting the digest of empty content
  against the one the uploader declared. Only an exact size is declared, and a
  caller's own value is never overwritten.

## [0.1.0-alpha.12](https://github.com/GamePad64/hclient/compare/hclient-native-v0.1.0-alpha.11...hclient-native-v0.1.0-alpha.12) - 2026-09-18

### Fixed

- *(native)* strip `Host` from an HTTP/2 request, beside the rest

## [0.1.0-alpha.11](https://github.com/GamePad64/hclient/compare/hclient-native-v0.1.0-alpha.10...hclient-native-v0.1.0-alpha.11) - 2026-09-16

### Added

- `trace!` the decisions that a failure cannot be read backwards from

### Other

- ask `IdleTimeout` its own two answers, from the only path that can
- the QUIC timer seam is a waker, and CPU time is what sees it
- the h2 body must not claim a stream ended while frames remain
- a closed pooled h2 connection is rejected at checkout, not retried past
- `NativeBody` must not claim a stream ended while frames remain
- an escaped quote must not let a comma split an Alt-Svc member
- an entry reports the persist flag it was built with
- the lowest-priority SVCB record wins, and nothing said so
- a finished TLS exchange sends `close_notify`, and nothing said so
- a SOCKS proxy gets origin-form, and nothing said so

## [0.1.0-alpha.10](https://github.com/GamePad64/hclient/compare/hclient-native-v0.1.0-alpha.9...hclient-native-v0.1.0-alpha.10) - 2026-09-15

### Added

- `send_transport!` writes the impl a backend cannot forget

### Fixed

- `http2(true)`'s refusal could not fire where the suite runs
- rustls backpressure was a signal, and this crate read it as a failure

### Other

- `Timeouts::resolve` releases on either family, and nothing said so
- the h3 half of the backpressure question, which is a negative
- `send_transport!` is named where the trait it writes is
- every `allow` carries `reason`, and the lint checks it per site

## [0.1.0-alpha.9](https://github.com/GamePad64/hclient/compare/hclient-native-v0.1.0-alpha.8...hclient-native-v0.1.0-alpha.9) - 2026-09-10

### Other

- [**breaking**] a trait a `dyn` is taken of is `Dyn*`, not `Box*`

## [0.1.0-alpha.8](https://github.com/GamePad64/hclient/compare/hclient-native-v0.1.0-alpha.7...hclient-native-v0.1.0-alpha.8) - 2026-09-10

### Other

- every `#[allow(clippy::..)]` says why, and four gates that never ran on a push
- take `clippy::pedantic`, and the two lints refused have reasons of this workspace's own
- [**breaking**] `DecompressionSupport` is a `bool` too — there are two parties, not three
- [**breaking**] three capabilities were two-variant enums answering a yes/no question
- [**breaking**] erasure is `Box*` and lives beside the trait it erases
- [**breaking**] `bon` leaves `hclient-core`, and two `const fn` replace it

## [0.1.0-alpha.7](https://github.com/GamePad64/hclient/compare/hclient-native-v0.1.0-alpha.6...hclient-native-v0.1.0-alpha.7) - 2026-09-09

### Other

- [**breaking**] `host` is `url` and `identity` is `tls`

## [0.1.0-alpha.6](https://github.com/GamePad64/hclient/compare/hclient-native-v0.1.0-alpha.5...hclient-native-v0.1.0-alpha.6) - 2026-09-09

### Added

- [**breaking**] `Timeouts` and `TimeoutSupport` are `#[non_exhaustive]`, via `bon`
- [**breaking**] hclient-core's public plane is modules, not sixty-one flat names

### Other

- [**breaking**] `caps` held two vocabularies, and no call site used both

## [0.1.0-alpha.5](https://github.com/GamePad64/hclient/compare/hclient-native-v0.1.0-alpha.4...hclient-native-v0.1.0-alpha.5) - 2026-09-07

### Added

- [**breaking**] dissolve the `unversioned` quarantine

## [0.1.0-alpha.4](https://github.com/GamePad64/hclient/compare/hclient-native-v0.1.0-alpha.3...hclient-native-v0.1.0-alpha.4) - 2026-09-06

### Added

- release with release-plz, and every crate owns its version

### Other

- Four seams for state a client keeps, and one recorded argument overturned
