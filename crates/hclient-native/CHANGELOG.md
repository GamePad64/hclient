# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

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
