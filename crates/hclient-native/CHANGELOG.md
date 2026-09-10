# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

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
