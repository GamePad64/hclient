# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.1.0-alpha.11](https://github.com/GamePad64/hclient/compare/hclient-dns-system-v0.1.0-alpha.10...hclient-dns-system-v0.1.0-alpha.11) - 2026-09-16

### Added

- `trace!` the decisions that a failure cannot be read backwards from

### Other

- `SystemDns` must not disown the address types it always supports

## [0.1.0-alpha.10](https://github.com/GamePad64/hclient/compare/hclient-dns-system-v0.1.0-alpha.9...hclient-dns-system-v0.1.0-alpha.10) - 2026-09-15

### Other

- every `allow` carries `reason`, and the lint checks it per site

## [0.1.0-alpha.9](https://github.com/GamePad64/hclient/compare/hclient-dns-system-v0.1.0-alpha.8...hclient-dns-system-v0.1.0-alpha.9) - 2026-09-10

### Other

- updated the following local packages: hclient-core, hclient-dns, hclient-rt

## [0.1.0-alpha.8](https://github.com/GamePad64/hclient/compare/hclient-dns-system-v0.1.0-alpha.7...hclient-dns-system-v0.1.0-alpha.8) - 2026-09-10

### Other

- take `clippy::pedantic`, and the two lints refused have reasons of this workspace's own

## [0.1.0-alpha.7](https://github.com/GamePad64/hclient/compare/hclient-dns-system-v0.1.0-alpha.6...hclient-dns-system-v0.1.0-alpha.7) - 2026-09-09

### Other

- updated the following local packages: hclient-core, hclient-dns, hclient-rt

## [0.1.0-alpha.6](https://github.com/GamePad64/hclient/compare/hclient-dns-system-v0.1.0-alpha.5...hclient-dns-system-v0.1.0-alpha.6) - 2026-09-09

### Added

- [**breaking**] hclient-core's public plane is modules, not sixty-one flat names

## [0.1.0-alpha.5](https://github.com/GamePad64/hclient/compare/hclient-dns-system-v0.1.0-alpha.4...hclient-dns-system-v0.1.0-alpha.5) - 2026-09-07

### Other

- updated the following local packages: hclient-core, hclient-rt, system-resolver, hclient-dns

## [0.1.0-alpha.4](https://github.com/GamePad64/hclient/compare/hclient-dns-system-v0.1.0-alpha.3...hclient-dns-system-v0.1.0-alpha.4) - 2026-09-06

### Added

- release with release-plz, and every crate owns its version
