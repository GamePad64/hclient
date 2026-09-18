# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.1.0](https://github.com/GamePad64/hclient/compare/hclient-cli-v0.1.0-alpha.11...hclient-cli-v0.1.0) - 2026-09-18

### Other

- [**breaking**] `hclient-cli` leaves the pre-release series, because a binary has no API to promise

## [0.1.0-alpha.11](https://github.com/GamePad64/hclient/compare/hclient-cli-v0.1.0-alpha.10...hclient-cli-v0.1.0-alpha.11) - 2026-09-16

### Other

- close 54 of `hclient-cli`'s 60 mutation survivors, and name the six that stay

## [0.1.0-alpha.10](https://github.com/GamePad64/hclient/compare/hclient-cli-v0.1.0-alpha.9...hclient-cli-v0.1.0-alpha.10) - 2026-09-15

### Other

- every `allow` carries `reason`, and the lint checks it per site

## [0.1.0-alpha.9](https://github.com/GamePad64/hclient/compare/hclient-cli-v0.1.0-alpha.8...hclient-cli-v0.1.0-alpha.9) - 2026-09-10

### Other

- [**breaking**] a trait a `dyn` is taken of is `Dyn*`, not `Box*`

## [0.1.0-alpha.8](https://github.com/GamePad64/hclient/compare/hclient-cli-v0.1.0-alpha.7...hclient-cli-v0.1.0-alpha.8) - 2026-09-10

### Other

- take `clippy::pedantic`, and the two lints refused have reasons of this workspace's own
- [**breaking**] erasure is `Box*` and lives beside the trait it erases

## [0.1.0-alpha.7](https://github.com/GamePad64/hclient/compare/hclient-cli-v0.1.0-alpha.6...hclient-cli-v0.1.0-alpha.7) - 2026-09-09

### Other

- updated the following local packages: hclient-core, hclient-native, hclient-tls-rustls, hclient, hclient-dns, hclient-dns-system, hclient-rt-tokio, hclient-tls-native-tls, hclient-tungstenite

## [0.1.0-alpha.6](https://github.com/GamePad64/hclient/compare/hclient-cli-v0.1.0-alpha.5...hclient-cli-v0.1.0-alpha.6) - 2026-09-09

### Added

- [**breaking**] hclient-core's public plane is modules, not sixty-one flat names

### Other

- [**breaking**] `caps` held two vocabularies, and no call site used both

## [0.1.0-alpha.5](https://github.com/GamePad64/hclient/compare/hclient-cli-v0.1.0-alpha.4...hclient-cli-v0.1.0-alpha.5) - 2026-09-07

### Added

- [**breaking**] dissolve the `unversioned` quarantine

## [0.1.0-alpha.4](https://github.com/GamePad64/hclient/compare/hclient-cli-v0.1.0-alpha.3...hclient-cli-v0.1.0-alpha.4) - 2026-09-06

### Added

- release with release-plz, and every crate owns its version

### Other

- Two types held the answer in a private field and had no way to say it
