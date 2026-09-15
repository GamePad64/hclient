# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.1.0-alpha.10](https://github.com/GamePad64/hclient/compare/hclient-v0.1.0-alpha.9...hclient-v0.1.0-alpha.10) - 2026-09-15

### Fixed

- split the key test, which needed a list the feature-off build has not

### Other

- RFC 8878's skippable frames, which no encoder produces
- a zstd body that ends at a frame header, which `&&` would accept
- `coding()` was only ever asked of a body that decoded nothing
- the public-suffix seam had never been given a caller's own list
- the store seam's own methods, driven the way another store would
- the deflate sniff and its bounds, none of which a socket can reach
- the expiry sweep had no test because the wire cannot see it
- the size bound on the restore path was never asked at its edge
- `SameSite=None` and a pre-epoch `Expires`, neither of them reached
- `CookieJar::clear` had no caller anywhere
- §5.7's replacement rule was dead on the batch path
- twelve public accessors on `Cookie` had no reader at all
- the two narrowing directives were never asked at their boundary
- four of the five delimiter ranges were never separated
- a cached response that is not 200 keeps its own status
- a third boundary, and a timed-out body that denied it
- a retry moves the resend counter, and the retry loop was the gap
- a cache boundary, and a caller's own validator
- every `allow` carries `reason`, and the lint checks it per site
- two rules that were right, documented and dead

## [0.1.0-alpha.9](https://github.com/GamePad64/hclient/compare/hclient-v0.1.0-alpha.8...hclient-v0.1.0-alpha.9) - 2026-09-10

### Other

- [**breaking**] a trait a `dyn` is taken of is `Dyn*`, not `Box*`

## [0.1.0-alpha.8](https://github.com/GamePad64/hclient/compare/hclient-v0.1.0-alpha.7...hclient-v0.1.0-alpha.8) - 2026-09-10

### Other

- take `clippy::pedantic`, and the two lints refused have reasons of this workspace's own
- [**breaking**] `DecompressionSupport` is a `bool` too — there are two parties, not three
- [**breaking**] three capabilities were two-variant enums answering a yes/no question
- [**breaking**] erasure is `Box*` and lives beside the trait it erases
- [**breaking**] `bon` leaves `hclient-core`, and two `const fn` replace it

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
