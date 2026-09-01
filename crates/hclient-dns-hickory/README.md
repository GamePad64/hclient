# hclient-dns-hickory

`Resolve` over hickory-dns, running in your process.

Use it when you want DNS-over-TLS or DNS-over-QUIC, or a resolver whose
behaviour does not vary by platform. It reads `/etc/resolv.conf` and talks
to the servers listed there, so it does not see configuration the OS keeps
elsewhere.

Part of [hclient](https://github.com/GamePad64/hclient), a cross-platform
HTTP client for native, browser and WASI targets.

## Licence

MIT or Apache-2.0, at your option.
