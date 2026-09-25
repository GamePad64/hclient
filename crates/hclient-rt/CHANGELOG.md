# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.1.1](https://github.com/GamePad64/hclient/compare/hclient-rt-v0.1.0...hclient-rt-v0.1.1) - 2026-09-25

### Other

- every crate's front page starts with what it is and a working example
- doc comments speak to the docs.rs reader, and the argument moves beside them
- every public item is documented, and every library asks for missing_docs
- *(rt)* TokioHandle is total only while its runtime lives

## [0.1.0](https://github.com/GamePad64/hclient/compare/hclient-rt-v0.1.0-alpha.11...hclient-rt-v0.1.0) - 2026-09-23

### Added

- *(rt)* [**breaking**] TCP, UDP and IPC refuse the same way, on entry
- *(rt)* [**breaking**] Shutdown is half-close and nothing else
- *(rt)* [**breaking**] IPC is a trait of its own, and hclient-rt's modules follow its seams
- *(rt)* [**breaking**] the three support reports share one name, one shape and one refusal
- *(rt)* [**breaking**] one connect for every same-machine endpoint, so named pipes are not a major version
- *(rt)* [**breaking**] UdpCaps is built from NONE, and Shutdown has one path

### Other

- *(rt)* hclient-rt 0.1.0, and the eleven requirements naming it
- hclient-rt's README, the seam-growth policy, and where it stands
- *(rt)* say what a seam stream's poll_close should be
- *(rt)* Spawn::spawn does not fail, and says who that excludes
- *(rt)* [**breaking**] take `hyper` out of the byte-stream seam

## [0.1.0-alpha.11](https://github.com/GamePad64/hclient/compare/hclient-rt-v0.1.0-alpha.10...hclient-rt-v0.1.0-alpha.11) - 2026-09-18

### Other

- close `hclient-rt`'s 23 mutation survivors, and one of them is a performance claim

## [0.1.0-alpha.10](https://github.com/GamePad64/hclient/compare/hclient-rt-v0.1.0-alpha.9...hclient-rt-v0.1.0-alpha.10) - 2026-09-15

### Other

- every `allow` carries `reason`, and the lint checks it per site

## [0.1.0-alpha.9](https://github.com/GamePad64/hclient/compare/hclient-rt-v0.1.0-alpha.8...hclient-rt-v0.1.0-alpha.9) - 2026-09-10

### Other

- updated the following local packages: hclient-core

## [0.1.0-alpha.8](https://github.com/GamePad64/hclient/compare/hclient-rt-v0.1.0-alpha.7...hclient-rt-v0.1.0-alpha.8) - 2026-09-10

### Other

- take `clippy::pedantic`, and the two lints refused have reasons of this workspace's own
- [**breaking**] three capabilities were two-variant enums answering a yes/no question

## [0.1.0-alpha.7](https://github.com/GamePad64/hclient/compare/hclient-rt-v0.1.0-alpha.6...hclient-rt-v0.1.0-alpha.7) - 2026-09-09

### Other

- updated the following local packages: hclient-core

## [0.1.0-alpha.6](https://github.com/GamePad64/hclient/compare/hclient-rt-v0.1.0-alpha.5...hclient-rt-v0.1.0-alpha.6) - 2026-09-09

### Added

- [**breaking**] hclient-core's public plane is modules, not sixty-one flat names

## [0.1.0-alpha.5](https://github.com/GamePad64/hclient/compare/hclient-rt-v0.1.0-alpha.4...hclient-rt-v0.1.0-alpha.5) - 2026-09-07

### Added

- [**breaking**] dissolve the `unversioned` quarantine

## [0.1.0-alpha.4](https://github.com/GamePad64/hclient/compare/hclient-rt-v0.1.0-alpha.3...hclient-rt-v0.1.0-alpha.4) - 2026-09-06

### Added

- release with release-plz, and every crate owns its version
