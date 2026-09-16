# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.1.0-alpha.8](https://github.com/GamePad64/hclient/compare/hclient-proto-v0.1.0-alpha.7...hclient-proto-v0.1.0-alpha.8) - 2026-09-16

### Added

- [**breaking**] give `Link` a constructor and close `All`/`RetryAll`'s frozen field

### Fixed

- [**breaking**] take `winnow` off `hclient-proto`'s public surface before the freeze

## [0.1.0-alpha.7](https://github.com/GamePad64/hclient/compare/hclient-proto-v0.1.0-alpha.6...hclient-proto-v0.1.0-alpha.7) - 2026-09-15

### Fixed

- an overflowing `retry:` was dropped, and two rules had no test

### Other

- every `allow` carries `reason`, and the lint checks it per site
- two boundaries, and one branch nothing can reach
- three composition rules whose direction was unasserted

## [0.1.0-alpha.6](https://github.com/GamePad64/hclient/compare/hclient-proto-v0.1.0-alpha.5...hclient-proto-v0.1.0-alpha.6) - 2026-09-10

### Other

- every `#[allow(clippy::..)]` says why, and four gates that never ran on a push
- take `clippy::pedantic`, and the two lints refused have reasons of this workspace's own

## [0.1.0-alpha.5](https://github.com/GamePad64/hclient/compare/hclient-proto-v0.1.0-alpha.4...hclient-proto-v0.1.0-alpha.5) - 2026-09-07

### Other

- updated the following local packages: hclient-idn

## [0.1.0-alpha.4](https://github.com/GamePad64/hclient/compare/hclient-proto-v0.1.0-alpha.3...hclient-proto-v0.1.0-alpha.4) - 2026-09-06

### Added

- release with release-plz, and every crate owns its version
