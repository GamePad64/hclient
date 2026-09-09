# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.1.0-alpha.7](https://github.com/GamePad64/hclient/compare/hclient-core-v0.1.0-alpha.6...hclient-core-v0.1.0-alpha.7) - 2026-09-09

### Other

- [**breaking**] `host` is `url` and `identity` is `tls`

## [0.1.0-alpha.6](https://github.com/GamePad64/hclient/compare/hclient-core-v0.1.0-alpha.5...hclient-core-v0.1.0-alpha.6) - 2026-09-09

### Added

- [**breaking**] `Timeouts` and `TimeoutSupport` are `#[non_exhaustive]`, via `bon`
- [**breaking**] hclient-core's public plane is modules, not sixty-one flat names

### Fixed

- thirty methods were `#[must_use]` and thirteen equally pure ones were not

### Other

- a biconditional was stated unconditionally, and one backend is the case
- four modules opened straight into `use` lines, and `hooks` hid an audience
- the crate root had no map, and two of its facts had drifted
- what growing `hclient-core` costs, simulated rather than promised
- three `#[non_exhaustive]` decisions were being made by silence
- a const builder answers half the `#[non_exhaustive]` objection
- four types in hclient-core were silent on `#[non_exhaustive]`
- [**breaking**] `caps` held two vocabularies, and no call site used both

## [0.1.0-alpha.5](https://github.com/GamePad64/hclient/compare/hclient-core-v0.1.0-alpha.4...hclient-core-v0.1.0-alpha.5) - 2026-09-07

### Added

- [**breaking**] dissolve the `unversioned` quarantine
- [**breaking**] `box_body` is `pub(crate)`, because nobody outside could call it

## [0.1.0-alpha.4](https://github.com/GamePad64/hclient/compare/hclient-core-v0.1.0-alpha.3...hclient-core-v0.1.0-alpha.4) - 2026-09-06

### Added

- release with release-plz, and every crate owns its version

### Other

- The auth seam joins every other seam a third party implements
