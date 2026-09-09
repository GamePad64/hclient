# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.1.0-alpha.7](https://github.com/GamePad64/hclient/compare/hclient-mock-v0.1.0-alpha.6...hclient-mock-v0.1.0-alpha.7) - 2026-09-09

### Other

- updated the following local packages: hclient-core

## [0.1.0-alpha.6](https://github.com/GamePad64/hclient/compare/hclient-mock-v0.1.0-alpha.5...hclient-mock-v0.1.0-alpha.6) - 2026-09-09

### Added

- [**breaking**] `Timeouts` and `TimeoutSupport` are `#[non_exhaustive]`, via `bon`
- [**breaking**] hclient-core's public plane is modules, not sixty-one flat names

### Other

- [**breaking**] `caps` held two vocabularies, and no call site used both

## [0.1.0-alpha.5](https://github.com/GamePad64/hclient/compare/hclient-mock-v0.1.0-alpha.4...hclient-mock-v0.1.0-alpha.5) - 2026-09-07

### Added

- [**breaking**] dissolve the `unversioned` quarantine

## [0.1.0-alpha.4](https://github.com/GamePad64/hclient/compare/hclient-mock-v0.1.0-alpha.3...hclient-mock-v0.1.0-alpha.4) - 2026-09-06

### Added

- release with release-plz, and every crate owns its version
