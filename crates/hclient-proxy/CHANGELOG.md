# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.1.0](https://github.com/GamePad64/hclient/compare/hclient-proxy-v0.1.0-alpha.16...hclient-proxy-v0.1.0) - 2026-10-06

### Fixed

- *(proxy)* the deferred audit minors, verified one by one

### Other

- *(proxy)* the freeze — 0.1.0
- *(proxy)* the root is doors — five pub modules, twelve names

## [0.1.0-alpha.16](https://github.com/GamePad64/hclient/compare/hclient-proxy-v0.1.0-alpha.15...hclient-proxy-v0.1.0-alpha.16) - 2026-10-05

### Added

- *(proxy)* [**breaking**] a plain CONNECT defaults to HTTP/2, and each enum says why it is or is not exhaustive
- *(proxy)* constructors for the four refusals a foreign filter reports
- *(proxy)* a SOCKS5 rule with UDP opens a datagram path through the relay
- *(proxy)* SOCKS5 UDP ASSOCIATE and its datagram header, as sans-io
- *(proxy)* a filter may open a datagram path, and refuses one by default
- *(proxy)* [**breaking**] Dial lends UDP, the proxy's name and HTTP tunnels, each refused by default
- *(proxy)* a datagram path, one peer and whole datagrams, in quinn's shape
- *(rt)* Io names the byte-stream seam's four bounds, and hclient-proxy re-exports it
- *(proxy)* [**breaking**] Proxy::tls, and an https:// system proxy is TLS to the proxy rather than a refusal
- *(proxy)* Dial::connect_tls, TLS a filter asks the transport to run over its stream
- *(proxy)* [**breaking**] a proxy reached over a socket is IpcProxy, which only proxy_over_ipc takes
- *(proxy)* drive_exact refuses bytes past a handshake, and Rules uses it
- *(native)* [**breaking**] Native::egress installs an external filter, and a filter may wrap the stream
- *(proxy)* Rules, the default egress filter — mixed protocols, IPC reach, the Unix policy
- *(proxy)* the egress seam — Dial, EgressFilter and the three outcomes of an attempt

### Fixed

- the rest of clippy 1.99's assert_is_empty, everywhere
- *(proxy)* [**breaking**] `TunnelRequest::version` is an `Option`, and nothing rewrites it
- *(proxy)* [**breaking**] `SendEgressFilter::open_datagrams_send` has no default
- *(proxy)* a UDP socket that will not bind is a failure, not a refusal
- *(proxy)* `TunnelRequest`'s `Debug` withholds a credential's value
- *(proxy)* a SOCKS5 path yields the task after 64 discarded datagrams, and a lent UDP socket says it may coalesce
- *(proxy)* [**breaking**] `Associate` is SOCKS5's alone, and sealed
- *(proxy)* [**breaking**] a bypass pattern in no accepted shape is refused by name, and the two dialects are one
- *(proxy)* Socks5's Debug no longer prints the password
- *(proxy)* a colon in a Basic username is refused, a bad ATYP is malformed rather than a refusal, and the erasures are pinned
- *(proxy)* a bracketed v6 TLS proxy is checked by its bare address; the body behind a failing connection is read in the tests
- *(proxy)* the review's minors — IpcProxy holds its address once, push_ipc and drive_exact say what they cannot see
- *(native)* the filter seam's contract, a live connect budget, and the stream capability read

### Other

- *(proxy)* the freeze stays a plan — 0.1.0-alpha.16
- *(proxy)* the surface is what a consumer names
- the suppressions are #[expect], where the lint fires in this build
- *(proxy)* the association's doc no longer links its private opener
- *(proxy)* the association opens its own datagram path
- *(proxy)* [**breaking**] hclient-proxy 0.1.0
- *(proxy)* [**breaking**] the test doubles are behind a `test-util` feature
- *(proxy)* [**breaking**] `DynHandshake` and `BoxHandshake` are crate-private
- *(proxy)* `# Errors` on every `Dial` method and its `DynDial` mirror
- *(proxy)* what `DatagramPath` promises about a short buffer and about its size
- *(proxy)* a flood of relay fragments yields the task
- *(proxy)* [**breaking**] `UnsupportedBypass` and `BypassReason` at the root only
- *(proxy)* [**breaking**] one opaque `Association` in place of the sealed `Associate` and its three types
- HTTP/3 through a filter, SOCKS5 UDP, tunnels and the MASQUE experiment, written down
- *(proxy,masque)* close the gaps the mutation sweeps found in the datagram paths
- *(proxy)* [**breaking**] Proxy::handshake goes, IpcProxy reports its scheme, and the third pass is written down
- *(proxy)* [**breaking**] a route borrows its pool key from the filter, and absolute-form can grow
- *(proxy)* [**breaking**] Dial::connect and connect_ipc may borrow their arguments
- *(proxy)* [**breaking**] every setter fails as an Error, drive_exact says which protocols it is right for, and the second audit is written down
- *(proxy)* [**breaking**] the machine's settings arrive as Rules, and first-match-wins has one home
- *(proxy)* [**breaking**] Attempt is Failed or Unsupported, and a filtered decision carries a Route
- *(proxy)* [**breaking**] the surface a stable version promises — Target gets a constructor, Reach goes private, ProxySpokeFirst gets a name
- *(proxy)* the review's minors — Proxy::tls and the default certificate, the crate doc points at connect_tls, hook order, a stale AGENTS.md claim
- *(native)* [**breaking**] one egress filter decides where a connection goes; P is gone
- *(proxy)* the handshake driver lives beside the handshakes

## [0.1.0-alpha.15](https://github.com/GamePad64/hclient/compare/hclient-proxy-v0.1.0-alpha.14...hclient-proxy-v0.1.0-alpha.15) - 2026-09-25

### Added

- *(proxy)* [**breaking**] the surface a stable number would promise, and a bypass that meant two things

## [0.1.0-alpha.14](https://github.com/GamePad64/hclient/compare/hclient-proxy-v0.1.0-alpha.13...hclient-proxy-v0.1.0-alpha.14) - 2026-09-25

### Other

- every crate's front page starts with what it is and a working example
- doc comments speak to the docs.rs reader, and the argument moves beside them
- every public item is documented, and every library asks for missing_docs

## [0.1.0-alpha.13](https://github.com/GamePad64/hclient/compare/hclient-proxy-v0.1.0-alpha.12...hclient-proxy-v0.1.0-alpha.13) - 2026-09-23

### Other

- updated the following local packages: hclient-core

## [0.1.0-alpha.12](https://github.com/GamePad64/hclient/compare/hclient-proxy-v0.1.0-alpha.11...hclient-proxy-v0.1.0-alpha.12) - 2026-09-18

### Other

- *(deps)* [**breaking**] update the ecosystem, and `quinn-udp` 0.6.2 named a real asymmetry

## [0.1.0-alpha.11](https://github.com/GamePad64/hclient/compare/hclient-proxy-v0.1.0-alpha.10...hclient-proxy-v0.1.0-alpha.11) - 2026-09-16

### Added

- `trace!` the decisions that a failure cannot be read backwards from

### Other

- close fourteen real gaps in `hclient-proxy`, and the tool that found them was wrong about most of them

## [0.1.0-alpha.10](https://github.com/GamePad64/hclient/compare/hclient-proxy-v0.1.0-alpha.9...hclient-proxy-v0.1.0-alpha.10) - 2026-09-15

### Other

- every `allow` carries `reason`, and the lint checks it per site

## [0.1.0-alpha.9](https://github.com/GamePad64/hclient/compare/hclient-proxy-v0.1.0-alpha.8...hclient-proxy-v0.1.0-alpha.9) - 2026-09-10

### Other

- updated the following local packages: hclient-core

## [0.1.0-alpha.8](https://github.com/GamePad64/hclient/compare/hclient-proxy-v0.1.0-alpha.7...hclient-proxy-v0.1.0-alpha.8) - 2026-09-10

### Other

- every `#[allow(clippy::..)]` says why, and four gates that never ran on a push
- take `clippy::pedantic`, and the two lints refused have reasons of this workspace's own

## [0.1.0-alpha.7](https://github.com/GamePad64/hclient/compare/hclient-proxy-v0.1.0-alpha.6...hclient-proxy-v0.1.0-alpha.7) - 2026-09-09

### Other

- updated the following local packages: hclient-core

## [0.1.0-alpha.6](https://github.com/GamePad64/hclient/compare/hclient-proxy-v0.1.0-alpha.5...hclient-proxy-v0.1.0-alpha.6) - 2026-09-09

### Added

- [**breaking**] hclient-core's public plane is modules, not sixty-one flat names

## [0.1.0-alpha.5](https://github.com/GamePad64/hclient/compare/hclient-proxy-v0.1.0-alpha.4...hclient-proxy-v0.1.0-alpha.5) - 2026-09-07

### Other

- updated the following local packages: hclient-core, hclient-proto

## [0.1.0-alpha.4](https://github.com/GamePad64/hclient/compare/hclient-proxy-v0.1.0-alpha.3...hclient-proxy-v0.1.0-alpha.4) - 2026-09-06

### Added

- release with release-plz, and every crate owns its version

### Other

- `jni` 0.22 is a redesign wearing a minor version
