# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.1.3](https://github.com/GamePad64/hclient/compare/hclient-wasi-v0.1.2...hclient-wasi-v0.1.3) - 2026-09-25

### Added

- *(core)* reports `TlsSupport::Platform`, the state `hclient-core` 0.2.1 added for TLS the host performs; this crate's own API is unchanged

### Other

- every crate's front page starts with what it is and a working example
- doc comments speak to the docs.rs reader, and the argument moves beside them
- every public item is documented, and every library asks for missing_docs

## [0.1.2](https://github.com/GamePad64/hclient/compare/hclient-wasi-v0.1.1...hclient-wasi-v0.1.2) - 2026-09-23

### Fixed

- *(wasi)* on wasmtime 49 dropping the transmission future lost the trailers

### Other

- run the wasi suite with a wasm32-wasip3 guest
- *(wasi)* make the guest say what it received, not only what it wanted

## [0.1.1](https://github.com/GamePad64/hclient/compare/hclient-wasi-v0.1.0...hclient-wasi-v0.1.1) - 2026-09-18

### Other

- updated the following local packages: hclient

## [0.1.0](https://github.com/GamePad64/hclient/compare/hclient-wasi-v0.1.0-alpha.12...hclient-wasi-v0.1.0) - 2026-09-18

### Other

- *(wasi)* [**breaking**] `hclient-wasi` leaves the pre-release series
- *(deps)* take `wasip3` 0.9, and the WIT version behind it did not move

## [0.1.0-alpha.12](https://github.com/GamePad64/hclient/compare/hclient-wasi-v0.1.0-alpha.11...hclient-wasi-v0.1.0-alpha.12) - 2026-09-18

### Other

- updated the following local packages: hclient

## [0.1.0-alpha.11](https://github.com/GamePad64/hclient/compare/hclient-wasi-v0.1.0-alpha.10...hclient-wasi-v0.1.0-alpha.11) - 2026-09-16

### Other

- updated the following local packages: hclient

## [0.1.0-alpha.10](https://github.com/GamePad64/hclient/compare/hclient-wasi-v0.1.0-alpha.9...hclient-wasi-v0.1.0-alpha.10) - 2026-09-15

### Added

- `send_transport!` writes the impl a backend cannot forget

### Other

- `send_transport!` is named where the trait it writes is
- every `allow` carries `reason`, and the lint checks it per site

## [0.1.0-alpha.9](https://github.com/GamePad64/hclient/compare/hclient-wasi-v0.1.0-alpha.8...hclient-wasi-v0.1.0-alpha.9) - 2026-09-10

### Other

- updated the following local packages: hclient-core, hclient

## [0.1.0-alpha.8](https://github.com/GamePad64/hclient/compare/hclient-wasi-v0.1.0-alpha.7...hclient-wasi-v0.1.0-alpha.8) - 2026-09-10

### Other

- take `clippy::pedantic`, and the two lints refused have reasons of this workspace's own
- [**breaking**] three capabilities were two-variant enums answering a yes/no question
- [**breaking**] `bon` leaves `hclient-core`, and two `const fn` replace it

## [0.1.0-alpha.7](https://github.com/GamePad64/hclient/compare/hclient-wasi-v0.1.0-alpha.6...hclient-wasi-v0.1.0-alpha.7) - 2026-09-09

### Other

- updated the following local packages: hclient-core, hclient

## [0.1.0-alpha.6](https://github.com/GamePad64/hclient/compare/hclient-wasi-v0.1.0-alpha.5...hclient-wasi-v0.1.0-alpha.6) - 2026-09-09

### Added

- [**breaking**] `Timeouts` and `TimeoutSupport` are `#[non_exhaustive]`, via `bon`
- [**breaking**] hclient-core's public plane is modules, not sixty-one flat names

### Other

- [**breaking**] `caps` held two vocabularies, and no call site used both

## [0.1.0-alpha.5](https://github.com/GamePad64/hclient/compare/hclient-wasi-v0.1.0-alpha.4...hclient-wasi-v0.1.0-alpha.5) - 2026-09-07

### Added

- [**breaking**] dissolve the `unversioned` quarantine

## [0.1.0-alpha.4](https://github.com/GamePad64/hclient/compare/hclient-wasi-v0.1.0-alpha.3...hclient-wasi-v0.1.0-alpha.4) - 2026-09-06

### Added

- release with release-plz, and every crate owns its version
