# hclient-proto

The sans-io pieces hclient's transports share: an RFC 9112 response-head
parser, the RFC 8305 Happy Eyeballs scheduler, and two encoders.

**Usable from other crates, with no stable interface.** It follows
semver, but any `0.x` minor release may break its API — it changes
whenever hclient's transports need it to. Depend on a specific minor
(`hclient-proto = "0.1"`) and take a breaking release when you choose;
this is the `windows-sys` model. For a stable surface, depend on
`hclient`, `hclient-native` or `hclient-proxy` instead. No other hclient
crate exposes its types, so upgrading it never forces an upgrade of
anything else.

Part of [hclient](https://github.com/GamePad64/hclient), a cross-platform
HTTP client for native, browser and WASI targets.

## Licence

MIT or Apache-2.0, at your option.
