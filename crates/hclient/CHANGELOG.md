# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.1.0-alpha.7](https://github.com/GamePad64/hclient/compare/hclient-v0.1.0-alpha.6...hclient-v0.1.0-alpha.7) - 2026-09-09

### Other

- [**breaking**] `host` is `url` and `identity` is `tls`

## [0.1.0-alpha.6](https://github.com/GamePad64/hclient/compare/hclient-v0.1.0-alpha.5...hclient-v0.1.0-alpha.6) - 2026-09-09

### Added

- [**breaking**] `Timeouts` and `TimeoutSupport` are `#[non_exhaustive]`, via `bon`
- [**breaking**] hclient-core's public plane is modules, not sixty-one flat names

### Fixed

- two capability enums were readable and not nameable

### Other

- [**breaking**] `caps` held two vocabularies, and no call site used both

## [0.1.0-alpha.5](https://github.com/GamePad64/hclient/compare/hclient-v0.1.0-alpha.4...hclient-v0.1.0-alpha.5) - 2026-09-07

### Added

- [**breaking**] dissolve the `unversioned` quarantine

## [0.1.0-alpha.4](https://github.com/GamePad64/hclient/compare/hclient-v0.1.0-alpha.3...hclient-v0.1.0-alpha.4) - 2026-09-06

### Added

- release with release-plz, and every crate owns its version

### Other

- An extension follows a redirect to an origin the caller never named
- Four auth setters, two mechanisms, and nothing said which wins
- Three constants claim to agree on a number and nothing made them
- Two types held the answer in a private field and had no way to say it
- Three public doors nothing named, and a parsed type that could not grow
- The HSTS seam hands back entries, not pairs
- Six things the HSTS surface got wrong, found by reading it beside its neighbours
- The decoders write into an arena, not a fresh Vec per frame
- gzip and brotli get files, so every coding has one
- The codings are a registry, so adding one is one declaration
- The decompressor is a trait, and the objection to it was half wrong
- brotli 9 / brotli-decompressor 6, and the check that had nothing behind it
- HSTS, the one memory that changes where a request goes
- Four seams for state a client keeps, and one recorded argument overturned
- The auth seam joins every other seam a third party implements
- `#![cfg]` on an example file compiles its `main` away
- Six examples, and every one of them runs
