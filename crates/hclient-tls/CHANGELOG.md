# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.1.1](https://github.com/GamePad64/hclient/compare/hclient-tls-v0.1.0...hclient-tls-v0.1.1) - 2026-09-25

### Other

- every crate's front page starts with what it is and a working example
- doc comments speak to the docs.rs reader, and the argument moves beside them
- every public item is documented, and every library asks for missing_docs
- *(tls)* TlsInfo says what hclient-tls-native-tls reports

## [0.1.0-alpha.11](https://github.com/GamePad64/hclient/compare/hclient-tls-v0.1.0-alpha.10...hclient-tls-v0.1.0-alpha.11) - 2026-09-23

### Added

- *(tls)* [**breaking**] TlsRequest is built with new(), and the reserved 0-RTT slots go before the freeze

### Other

- *(rt)* hclient-rt 0.1.0, and the eleven requirements naming it
- *(tls)* stop describing the QUIC seam as it was two designs ago
- *(tls)* split the crate into its two seams and what they share
- *(tls)* the two seams are peers, and the crate says so
- *(tls)* [**breaking**] the QUIC seam carries its own config, and links no stack
- *(tls)* [**breaking**] take `quinn-proto` out of the QUIC TLS seam
- *(rt)* [**breaking**] take `hyper` out of the byte-stream seam

## [0.1.0-alpha.10](https://github.com/GamePad64/hclient/compare/hclient-tls-v0.1.0-alpha.9...hclient-tls-v0.1.0-alpha.10) - 2026-09-18

### Other

- pin `hclient-tls`'s defaulted seam members and its `TlsInfo` setters

## [0.1.0-alpha.9](https://github.com/GamePad64/hclient/compare/hclient-tls-v0.1.0-alpha.8...hclient-tls-v0.1.0-alpha.9) - 2026-09-10

### Other

- updated the following local packages: hclient-core

## [0.1.0-alpha.8](https://github.com/GamePad64/hclient/compare/hclient-tls-v0.1.0-alpha.7...hclient-tls-v0.1.0-alpha.8) - 2026-09-10

### Other

- take `clippy::pedantic`, and the two lints refused have reasons of this workspace's own

## [0.1.0-alpha.7](https://github.com/GamePad64/hclient/compare/hclient-tls-v0.1.0-alpha.6...hclient-tls-v0.1.0-alpha.7) - 2026-09-09

### Other

- [**breaking**] `host` is `url` and `identity` is `tls`

## [0.1.0-alpha.6](https://github.com/GamePad64/hclient/compare/hclient-tls-v0.1.0-alpha.5...hclient-tls-v0.1.0-alpha.6) - 2026-09-09

### Added

- [**breaking**] hclient-core's public plane is modules, not sixty-one flat names

### Other

- [**breaking**] `caps` held two vocabularies, and no call site used both

## [0.1.0-alpha.5](https://github.com/GamePad64/hclient/compare/hclient-tls-v0.1.0-alpha.4...hclient-tls-v0.1.0-alpha.5) - 2026-09-07

### Added

- [**breaking**] dissolve the `unversioned` quarantine

## [0.1.0-alpha.4](https://github.com/GamePad64/hclient/compare/hclient-tls-v0.1.0-alpha.3...hclient-tls-v0.1.0-alpha.4) - 2026-09-06

### Added

- release with release-plz, and every crate owns its version
