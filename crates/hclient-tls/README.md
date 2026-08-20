# hclient-tls

**The TLS seam: `TlsConnect`, `TlsIdentity`, and `NoTls`.**

One method — handshake and wrap in a single step, because no caller
anywhere wants one without the other. `reports_alpn` is defaulted `false`
so a backend that cannot read the negotiated protocol back cannot leave the
client speaking HTTP/1 into an h2 connection. `NoTls` is the third choice:
no TLS stack at all, where `https://` fails at connect with a typed error
rather than a claim.

Part of [hclient](https://github.com/GamePad64/hclient) — an HTTP client
complete enough to build a new curl on, or a browser. See the repository
for the whole shape, and `AGENTS.md` in it for why this piece is its own
crate.

## Licence

MIT or Apache-2.0, at your option.
