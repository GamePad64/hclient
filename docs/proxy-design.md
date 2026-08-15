# Proxy support: where it can live, and what it is not

Written because `Capabilities::proxy` has been an inert `bool` since v0.1
— set in one place to `false`, read by nobody, with no way for a caller
to ask for a proxy — and `docs/v03-design.md` §5.7 dismissed the feature
in passing as *"a proxy feature nobody has asked for"* while deleting
`UpgradeSupport`. Someone has now asked.

## 1. One backend of four, which decides the shape

| backend | can it proxy | why |
|---|---|---|
| `http-ng-native` | **yes** | it owns the socket |
| `http-ng-fetch` | no | the browser applies the system proxy and exposes no API for one; a `fetch`-shaped transport has no connect step to intercept |
| `http-ng-wasi` | no | `wasi:http@0.3.0`'s client interface is one function with **no connection resource in the WIT** — the same fact that gave hooks only `Head` on this backend. The host may proxy; the guest cannot ask |
| `http-ng-h3` | not with an ordinary proxy | HTTP proxies carry TCP. Carrying QUIC needs `CONNECT-UDP` (RFC 9298, MASQUE) over an HTTP/3 proxy, which is a different protocol against a different kind of server — not this feature with a wider bound |

So this is **not a method on `Transport`**, for the reason `StagedConnect`
and `WebSocketConnect` are not: a seam three of four backends answer
`Unsupported` to is dishonest, and the seam expresses itself by being
implemented.

## 2. Why not a `TcpConnect` decorator, which would cost nothing

The tempting shape is a wrapper `R` implementing
[`TcpConnect`](../crates/http-ng-rt/src/caps.rs) that dials the proxy and
tunnels — **zero changes to `http-ng-native`**, its own crate, done.

It is rejected on the signature:

```rust
fn connect(&self, addr: SocketAddr, opts: &TcpOpts) -> impl Future<..>;
```

A `SocketAddr` and nothing else. Three consequences, and each is the
feature rather than a detail:

- **The origin's name never reaches the proxy**, so the client resolves it
  locally and leaks exactly the DNS a proxy user is often there to hide.
  `socks5h`'s whole reason for existing is this distinction.
- **`http://` cannot use absolute-form.** An HTTP proxy expects
  `GET http://example.com/x HTTP/1.1`, which is decided where the request
  head is written, above the socket. A decorator would have to tunnel
  everything with `CONNECT`, and proxies commonly allow `CONNECT` to
  443 alone.
- **Happy Eyeballs would race the wrong addresses.** `connect.rs` resolves
  the origin and races its A/AAAA answers; with a proxy those addresses
  are never dialled, so the race is wasted work that spends
  `Timeouts::connect` on it.

## 3. Where it goes instead

`http_ng_native::connect::connect` already takes `uri: &Uri`, so the
origin is available **by name** at exactly the point that decides how to
reach it. A proxy **replaces** the resolve → Happy-Eyeballs → connect
block rather than decorating any part of it.

## 4. What a proxy changes, beyond which socket is opened

Each of these is a place the rest of the workspace has to agree, and they
are listed because getting one wrong is silent:

- **The resolver is not consulted for the origin.** The proxy resolves.
  Happy Eyeballs still applies — to the *proxy's* own addresses.
- **HTTPS/SVCB discovery must not run for the origin.**
  `Prefetch::prepare` answers `Discovered::NotConsulted`, which is the
  variant v0.4 W1 added precisely so that "we did not ask" is not
  confused with "there is no record". Address hints for an address
  nobody will dial, and a record port we would not honour, are worse
  than no answer.
- **`http://` takes absolute-form; `https://` takes a `CONNECT` tunnel**
  and then an ordinary TLS handshake carrying the **origin's** SNI, not
  the proxy's.
- **`Proxy-Authorization` is the proxy's, and is not the origin's.**
  `http-ng-proto`'s redirect logic already strips it across origins.
- **The pool key must name the proxy.** Two proxies to one origin are two
  connections, and a pooled tunnel reused through a different proxy would
  be a security defect rather than a redundancy — the same argument
  `PoolKey`'s TLS-identity field is already kept for.
- **`Timeouts::connect` bounds the whole approach** — dialling the proxy
  *and* the `CONNECT` exchange — and is spent once, which is
  `http-ng-select`'s arithmetic for the h3→TCP fallback one layer down.
- **A `407` from the proxy is not a response to the caller's request.**
  Surfacing it as one would report the proxy's refusal as the origin's
  answer. It is a typed connect error.

## 5. `Capabilities::proxy` becomes real rather than deleted

The `UpgradeSupport` rule — *a variant exists only if a caller decision
turns on it* — argues for deleting the field, and would have been right
while nothing could set it. With a proxy configurable on `Native` the
field has a producer, and the fix is the one `client_certs` just took:
**read it from the component that knows**, `self.proxy.is_some()`, and
give it the doc comment it has never had.

Its meaning has to be pinned in that comment, because the honest answer
for `http-ng-fetch` is `false` while a browser may well be proxying: the
field says **this transport applies a proxy configuration of its own**,
which is `owns_cookie_jar`'s phrasing and for the same reason. What the
host does behind a transport that owns no connection is not a fact this
field can carry.

## 6. SOCKS5 is in the first vertical, and that is what makes the seam real

Required rather than deferred, and the sequencing is better for it: a
seam with **one** implementer is a shape asserted to be general, and this
workspace has been wrong that way before — `Transport` earned its shape
by a live consumer ported onto it, `WebSocketConnect` by the browser
fitting it unchanged. Two protocols that share no bytes is the same
evidence.

They differ in exactly the places that decide whether the abstraction is
the right one:

| | HTTP `CONNECT` | SOCKS5 |
|---|---|---|
| the exchange | an HTTP request and response — this crate's own protocol, and `Upgrading` is already the machinery: *an upgraded byte stream, plus the `read_buf` hyper had already read past* | a byte protocol, RFC 1928, no HTTP anywhere |
| auth | `Proxy-Authorization`, an HTTP header | RFC 1929, its own sub-negotiation, refused before the request is sent |
| the origin's name | the request target of `CONNECT` | `ATYP=0x03 DOMAINNAME`, which is what `socks5h` names |
| `http://` origins | absolute-form, **no tunnel** — the one asymmetry, and it lives above the socket | tunnelled like everything else; the request is written unchanged |
| failure | a `407` or a `502`, an HTTP status | a one-byte `REP` code |

The last row of §2 is settled by the third: a SOCKS5 proxy takes the
origin **by name**, so the DNS leak that killed the `TcpConnect`
decorator is not a property of proxying — it is a property of a seam that
only carries a `SocketAddr`.

Both ship in `http-ng-native` behind one `proxy` feature, off by default.
Neither needs a third-party crate, so the argument that moved the
WebSocket framing out — a feature is additive, and `tungstenite` would
land in every build in the graph that switched it on — has no subject
here; what a feature does buy is the constrained-target build staying
byte-for-byte as it was. The trait is public, so a third protocol does
not have to live here.

## 7. Deliberately out of the first vertical

- **PAC files and system-proxy discovery.** Reading `HTTP_PROXY` from the
  environment is a policy decision (which variables, `NO_PROXY` matching
  rules, whether a library may read the environment at all) and is
  separable from being able to proxy at all.
- **`CONNECT-UDP` / MASQUE for `http-ng-h3`.** §1's last row.
