# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.1.0-alpha.14](https://github.com/GamePad64/hclient/compare/hclient-tls-rustls-v0.1.0-alpha.13...hclient-tls-rustls-v0.1.0-alpha.14) - 2026-09-25

### Added

- *(native)* [**breaking**] the root holds the transport, and five modules hold the rest
- *(tls-rustls)* pin every seam obligation, and re-export rustls

### Other

- every crate's front page starts with what it is and a working example
- doc comments speak to the docs.rs reader, and the argument moves beside them
- every public item is documented, and every library asks for missing_docs
- *(tls-rustls)* feature badges, and build a config from the re-export
- *(tls)* open both backends with how to build one
- *(tls-rustls)* hide the with_webpki_roots stub and its trait
- require rustls 0.23.45
- *(tls)* hclient-tls 0.1.0, and the seven requirements naming it

## [0.1.0-alpha.13](https://github.com/GamePad64/hclient/compare/hclient-tls-rustls-v0.1.0-alpha.12...hclient-tls-rustls-v0.1.0-alpha.13) - 2026-09-23

### Added

- *(rt)* [**breaking**] Shutdown is half-close and nothing else
- *(tls)* [**breaking**] TlsRequest is built with new(), and the reserved 0-RTT slots go before the freeze

### Other

- *(rt)* hclient-rt 0.1.0, and the eleven requirements naming it
- *(tls)* stop describing the QUIC seam as it was two designs ago
- *(tls-rustls)* what `from_config` is for, and why a session store cannot be ours
- *(tls)* [**breaking**] the QUIC seam carries its own config, and links no stack
- *(tls)* [**breaking**] take `quinn-proto` out of the QUIC TLS seam
- *(rt)* [**breaking**] take `hyper` out of the byte-stream seam

## [0.1.0-alpha.12](https://github.com/GamePad64/hclient/compare/hclient-tls-rustls-v0.1.0-alpha.11...hclient-tls-rustls-v0.1.0-alpha.12) - 2026-09-18

### Other

- updated the following local packages: hclient-tls

## [0.1.0-alpha.11](https://github.com/GamePad64/hclient/compare/hclient-tls-rustls-v0.1.0-alpha.10...hclient-tls-rustls-v0.1.0-alpha.11) - 2026-09-16

### Added

- `trace!` the TLS read rhythm, which is what the last defect needed

## [0.1.0-alpha.10](https://github.com/GamePad64/hclient/compare/hclient-tls-rustls-v0.1.0-alpha.9...hclient-tls-rustls-v0.1.0-alpha.10) - 2026-09-15

### Fixed

- rustls backpressure was a signal, and this crate read it as a failure

### Other

- every `allow` carries `reason`, and the lint checks it per site

## [0.1.0-alpha.9](https://github.com/GamePad64/hclient/compare/hclient-tls-rustls-v0.1.0-alpha.8...hclient-tls-rustls-v0.1.0-alpha.9) - 2026-09-10

### Other

- updated the following local packages: hclient-core, hclient-tls

## [0.1.0-alpha.8](https://github.com/GamePad64/hclient/compare/hclient-tls-rustls-v0.1.0-alpha.7...hclient-tls-rustls-v0.1.0-alpha.8) - 2026-09-10

### Other

- take `clippy::pedantic`, and the two lints refused have reasons of this workspace's own

## [0.1.0-alpha.7](https://github.com/GamePad64/hclient/compare/hclient-tls-rustls-v0.1.0-alpha.6...hclient-tls-rustls-v0.1.0-alpha.7) - 2026-09-09

### Other

- [**breaking**] `host` is `url` and `identity` is `tls`

## [0.1.0-alpha.6](https://github.com/GamePad64/hclient/compare/hclient-tls-rustls-v0.1.0-alpha.5...hclient-tls-rustls-v0.1.0-alpha.6) - 2026-09-09

### Added

- [**breaking**] hclient-core's public plane is modules, not sixty-one flat names

### Other

- [**breaking**] `caps` held two vocabularies, and no call site used both

## [0.1.0-alpha.5](https://github.com/GamePad64/hclient/compare/hclient-tls-rustls-v0.1.0-alpha.4...hclient-tls-rustls-v0.1.0-alpha.5) - 2026-09-07

### Added

- [**breaking**] dissolve the `unversioned` quarantine

## [0.1.0-alpha.4](https://github.com/GamePad64/hclient/compare/hclient-tls-rustls-v0.1.0-alpha.3...hclient-tls-rustls-v0.1.0-alpha.4) - 2026-09-06

### Added

- release with release-plz, and every crate owns its version

### Other

- Four seams for state a client keeps, and one recorded argument overturned
