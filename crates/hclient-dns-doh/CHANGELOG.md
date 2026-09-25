# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.1.0-alpha.16](https://github.com/GamePad64/hclient/compare/hclient-dns-doh-v0.1.0-alpha.15...hclient-dns-doh-v0.1.0-alpha.16) - 2026-09-25

### Other

- every crate's front page starts with what it is and a working example
- doc comments speak to the docs.rs reader, and the argument moves beside them
- every public item is documented, and every library asks for missing_docs
- *(tls)* hclient-tls 0.1.0, and the seven requirements naming it

## [0.1.0-alpha.15](https://github.com/GamePad64/hclient/compare/hclient-dns-doh-v0.1.0-alpha.14...hclient-dns-doh-v0.1.0-alpha.15) - 2026-09-23

### Other

- updated the following local packages: hclient-core, hclient-tls, hclient-native, hclient-rt-tokio, hclient-tls-rustls, hclient-dns

## [0.1.0-alpha.14](https://github.com/GamePad64/hclient/compare/hclient-dns-doh-v0.1.0-alpha.13...hclient-dns-doh-v0.1.0-alpha.14) - 2026-09-18

### Other

- *(dns)* [**breaking**] `hclient-dns` leaves the pre-release series

## [0.1.0-alpha.13](https://github.com/GamePad64/hclient/compare/hclient-dns-doh-v0.1.0-alpha.12...hclient-dns-doh-v0.1.0-alpha.13) - 2026-09-18

### Other

- *(deps)* bump `hclient-native` to `0.1.0-alpha.13`

## [0.1.0-alpha.12](https://github.com/GamePad64/hclient/compare/hclient-dns-doh-v0.1.0-alpha.11...hclient-dns-doh-v0.1.0-alpha.12) - 2026-09-18

### Fixed

- [**breaking**] take `domain` off `hclient-dns`'s public surface

### Other

- [**breaking**] one DNS decoder, because `domain` does both granularities

## [0.1.0-alpha.11](https://github.com/GamePad64/hclient/compare/hclient-dns-doh-v0.1.0-alpha.10...hclient-dns-doh-v0.1.0-alpha.11) - 2026-09-16

### Added

- `trace!` the decisions that a failure cannot be read backwards from

## [0.1.0-alpha.10](https://github.com/GamePad64/hclient/compare/hclient-dns-doh-v0.1.0-alpha.9...hclient-dns-doh-v0.1.0-alpha.10) - 2026-09-15

### Other

- the mutation scripts were shipping in three published crates
- every `allow` carries `reason`, and the lint checks it per site

## [0.1.0-alpha.9](https://github.com/GamePad64/hclient/compare/hclient-dns-doh-v0.1.0-alpha.8...hclient-dns-doh-v0.1.0-alpha.9) - 2026-09-10

### Other

- updated the following local packages: hclient-core, hclient-native, hclient-dns, hclient-tls, hclient-rt-tokio, hclient-tls-rustls

## [0.1.0-alpha.8](https://github.com/GamePad64/hclient/compare/hclient-dns-doh-v0.1.0-alpha.7...hclient-dns-doh-v0.1.0-alpha.8) - 2026-09-10

### Other

- take `clippy::pedantic`, and the two lints refused have reasons of this workspace's own
- a third copy of the `bon` claim, in the crate whose `const` decided it
- [**breaking**] `bon` leaves `hclient-core`, and two `const fn` replace it

## [0.1.0-alpha.7](https://github.com/GamePad64/hclient/compare/hclient-dns-doh-v0.1.0-alpha.6...hclient-dns-doh-v0.1.0-alpha.7) - 2026-09-09

### Other

- [**breaking**] `host` is `url` and `identity` is `tls`

## [0.1.0-alpha.6](https://github.com/GamePad64/hclient/compare/hclient-dns-doh-v0.1.0-alpha.5...hclient-dns-doh-v0.1.0-alpha.6) - 2026-09-09

### Added

- [**breaking**] `Timeouts` and `TimeoutSupport` are `#[non_exhaustive]`, via `bon`
- [**breaking**] hclient-core's public plane is modules, not sixty-one flat names

### Other

- [**breaking**] `caps` held two vocabularies, and no call site used both

## [0.1.0-alpha.5](https://github.com/GamePad64/hclient/compare/hclient-dns-doh-v0.1.0-alpha.4...hclient-dns-doh-v0.1.0-alpha.5) - 2026-09-07

### Added

- [**breaking**] dissolve the `unversioned` quarantine

## [0.1.0-alpha.4](https://github.com/GamePad64/hclient/compare/hclient-dns-doh-v0.1.0-alpha.3...hclient-dns-doh-v0.1.0-alpha.4) - 2026-09-06

### Added

- release with release-plz, and every crate owns its version
