# hclient-dns-hickory

**`Resolve` over hickory-dns, in process — and the way to get DNS-over-TLS or DNS-over-QUIC.**

For builds that want DNS behaviour independent of the host's configuration
— a container with no resolver, a test that must not touch the network, a
program that needs the same answers on every platform.

## DNS-over-TLS and DNS-over-QUIC

**This crate already does both**, and nothing outside a parenthetical said
so until `docs/dot-and-doq.md` was written. [`Hickory::new`] takes a
`Resolver` you built, so the transport is chosen in your own
`ResolverConfig` and this crate needs no feature, no constructor and no
line of code for it.

Three things to know before you try:

1. Ask `hickory-resolver` for `tls-ring` (DoT) or `quic-ring` (DoQ) — the
   `-ring` half specifically. `tls-aws-lc-rs` resolves to the same crate
   count and swaps 8.2 MB of `ring` for 68 MB of `aws-lc-sys` with a build
   script.
2. **Also ask for `rustls-platform-verifier`, or nothing will resolve and
   the error will not say why.** With a TLS provider and no roots feature,
   hickory builds an empty `RootCertStore`: it compiles, the resolver
   builds, and every lookup fails as `no connections available`, naming
   neither TLS nor certificates. It fails closed, which is the right
   direction, and it is the first thing a reader hits.
3. The machine may already be doing DoT — systemd-resolved, Android
   Private DNS, Apple's profiles, Windows 11 — in which case
   `hclient-dns-system`, the default, inherits it for free, and a
   client-side resolver **overrides** the administrator rather than adding
   to them.

Part of [hclient](https://github.com/GamePad64/hclient) — an HTTP client
complete enough to build a new curl on, or a browser. See the repository
for the whole shape, and `AGENTS.md` in it for why this piece is its own
crate.

## Licence

MIT or Apache-2.0, at your option.
