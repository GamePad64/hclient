# Proxy support: where it can live, and what it is not

Written because `Capabilities::proxy` has been an inert `bool` since v0.1
— set in one place to `false`, read by nobody, with no way for a caller
to ask for a proxy — and `docs/v03-design.md` §5.7 dismissed the feature
in passing as *"a proxy feature nobody has asked for"* while deleting
`UpgradeSupport`. Someone has now asked.

## 1. One backend of four, which decides the shape

| backend | can it proxy | why |
|---|---|---|
| `hclient-native` | **yes** | it owns the socket |
| `hclient-fetch` | no | the browser applies the system proxy and exposes no API for one; a `fetch`-shaped transport has no connect step to intercept |
| `hclient-wasi` | no | `wasi:http@0.3.0`'s client interface is one function with **no connection resource in the WIT** — the same fact that gave hooks only `Head` on this backend. The host may proxy; the guest cannot ask |
| `hclient-h3` | not with an ordinary proxy | HTTP proxies carry TCP. Carrying QUIC needs `CONNECT-UDP` (RFC 9298, MASQUE) over an HTTP/3 proxy, which is a different protocol against a different kind of server — not this feature with a wider bound |

So this is **not a method on `Transport`**, for the reason `StagedConnect`
and `WebSocketConnect` are not: a seam three of four backends answer
`Unsupported` to is dishonest, and the seam expresses itself by being
implemented.

## 2. Why not a `TcpConnect` decorator, which would cost nothing

The tempting shape is a wrapper `R` implementing
[`TcpConnect`](../crates/hclient-rt/src/caps.rs) that dials the proxy and
tunnels — **zero changes to `hclient-native`**, its own crate, done.

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

`hclient_native::connect::connect` already takes `uri: &Uri`, so the
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
  `hclient-proto`'s redirect logic already strips it across origins.
- **The pool key must name the proxy.** Two proxies to one origin are two
  connections, and a pooled tunnel reused through a different proxy would
  be a security defect rather than a redundancy — the same argument
  `PoolKey`'s TLS-identity field is already kept for.
- **`Timeouts::connect` bounds the whole approach** — dialling the proxy
  *and* the `CONNECT` exchange — and is spent once, which is
  `hclient-select`'s arithmetic for the h3→TCP fallback one layer down.
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
for `hclient-fetch` is `false` while a browser may well be proxying: the
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

Both ship in `hclient-native` behind one `proxy` feature, off by default.
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
- **`CONNECT-UDP` / MASQUE for `hclient-h3`.** §1's last row.

## 8. What building it found, which the design above did not predict

- **The `CONNECT` tunnel is the WebSocket upgrade seam with a different
  accepted status**, and that was read rather than hoped: hyper 1.11's own
  h1 client sets `wants_upgrade` for `Method::CONNECT` (`role.rs:240`) and
  skips the body for `CONNECT` + `is_success` (`:518`, `:528`), so
  `poll_without_shutdown` + `into_parts` yields the tunnel and the bytes
  read past it exactly as it does for a `101`. `upgrade::exchange` now
  takes the accepted-status test as a parameter and has **one** copy of
  the forty delicate lines instead of two.
- **The request line needed nothing from hyper.** It writes `http::Uri`'s
  `Display` verbatim (`role.rs:1212`), so authority-form for `CONNECT` and
  absolute-form for `http://` are both just a matter of handing it the
  right `Uri` — which is what `hclient-native` already does for
  origin-form.
- **`NoProxy` is an empty enum**, so a transport nobody configured a proxy
  on holds an `Option` that cannot be `Some` — by construction rather than
  by discipline. A unit struct with `unreachable!()` bodies would be a
  value existing only to be absent.
- **The proxy in `PoolKey` is unreachable today**, and that is stated
  rather than tested. A proxy is configured on the transport and each
  transport owns its own pool, so it is a constant within any one pool —
  precisely what `docs/v02-acceptance.md` already says about the TLS
  identity in the same key, and it is in the key for the same reason: the
  moment a pool is shared between transports, its absence stops being a
  redundancy and becomes a tunnel handed to a request routed elsewhere.
  The mutation that changes its shape is this work's **control**, and it
  survives as predicted.
- **A non-empty `read_buf` after a tunnel is a refusal, not a rewind.**
  Nothing the origin might say can have arrived — the client has not
  written to it — so those bytes are the proxy's, and carrying them on
  would feed them to the TLS handshake, or to hyper, as if the origin had
  sent them.
- **One fixture bug that looked exactly like a client bug.** The scripted
  socket handed its reply back on the first poll, so hyper saw a response
  with no request in flight and failed the exchange with
  `hyper::Error(Canceled, UnexpectedMessage)`. A socket is silent until it
  has been written to, and the fixture is now.

### Mutations

Anchor **189** (`hclient-native`, `--features proxy`), `--no-fail-fast`.

| # | mutation | verdict | killed |
|---|---|---|---|
| M1 | `HttpConnect::approach` always answers `Tunnel` | killed | 3 |
| M2 | SOCKS5 sends `ATYP=0x01` where the name should go | killed | 1 |
| M3 | `Capabilities::proxy` is left at `false` | killed | 1 |
| M4 | `Proxy-Authorization` is dropped from an absolute-form request | killed | 1 |
| M5 | **control** — the pool key's proxy component changes shape | **survived, as predicted** | 0 |
| M6 | the tunnelled TLS handshake takes the **proxy's** name as its SNI | killed | 1 |
| M7 | the `read_buf` guard after a tunnel is removed | killed | 1 |

M5 is a control by argument rather than by luck: the paragraph above says
no observer exists, and the mutation is how that claim is checked instead
of asserted.

### The tunnel end to end, which closed this section's own gap

This section read *"what is not proven either way is the TLS handshake
riding a tunnel — the origin's SNI over a proxy's socket — and that is
the first thing a second pass should add."* It is added.

`tls_rides_the_tunnel_and_the_origin_is_greeted_with_its_own_name` runs a
real `tokio-rustls` origin behind a real tunnelling proxy and makes three
claims in one exchange: the `CONNECT` names the **origin's** authority;
the origin's certificate validates, so the stream is end to end rather
than terminated at the proxy; and the SNI the origin was greeted with is
`localhost` rather than `127.0.0.1`, which is the proxy's own host and
exactly what a connector taking its name from the socket would have sent.
The name is read off the accepted connection (`ServerConnection::
server_name`) rather than inferred from the request succeeding — a
handshake that failed for having no SNI at all and one that sent the
wrong name are different defects.

The resolver in that test is `IpLiteralOnly`, so `localhost` is a name
this client **cannot** resolve: reaching the origin at all is the proof
that the name was carried rather than looked up.

`a_proxy_that_speaks_first_is_refused` is its sibling and inverts one
thing — the fixture appends eight bytes after its `200`. The error names
both the defect and the count, `ProxySpokeFirst(8)`, and the test reads
the source type rather than the rendered message.

### The same three claims over SOCKS5, and a tunnel that dies

`tls_rides_a_socks5_tunnel_with_the_origin_name_too` repeats the row
above over a protocol that shares no bytes with `CONNECT`, and that is
the strongest evidence the seam is where it belongs: the handshake, the
certificate and the SNI are the transport's business, and none of them
changes with whatever carried the bytes.

`a_tunnel_that_dies_after_it_is_established_fails_rather_than_hangs` is
the case every refusal above misses — the proxy *agrees*, then drops the
socket, which is what a real one does when its own upstream dies. It uses
the **real** TLS backend deliberately: with `NoTls` an `https://` request
fails identically whether the tunnel is alive or dead, so that test would
have passed for a client that never noticed. `ErrorKind::Tls` rather than
`Connect` is itself the evidence the tunnel was established and the
handshake went into it.

### Still not covered

- ~~A proxy whose own upstream refuses~~ — **done**, and it took three
  codes rather than one: `0x05` refused, `0x04` host unreachable, `0x02`
  not allowed by ruleset, because a client reporting every refusal as the
  same value would pass a single-row test. The mutation that pins one
  `REP` kills exactly that test.

  Its sibling is the distinction that mattered: **refusing the greeting is
  not refusing the CONNECT.** RFC 1928 §3's `0xFF` never reaches the
  request stage, so reporting it as a `REP` would name a byte the proxy
  never sent — asserted in both directions, that the handshake error is
  there and that a reply code is not.
- **PAC, and reading `HTTP_PROXY`/`NO_PROXY` from the environment**,
  which §7 lists as deliberately out of scope — and see §9, which splits
  that question in two.

## 9. The bypass list, and the half of `NO_PROXY` that is not policy

§7 put `NO_PROXY` out of scope in one line, and that line conflated two
questions. **Reading the environment is policy** — which variables, whose
matching dialect, whether a library may read the environment at all — and
policy belongs to whoever builds the transport. **A list the caller wrote
down is not policy at all**: they said what they wanted. Only the first is
still out.

`Proxy::bypass([..])`, and the rules are small on purpose, because
`NO_PROXY` has no specification and every implementation disagrees about
the corners. Four forms, matched case-insensitively: an exact host at any
port; `.example.com` for a domain and everything under it; `host:port` for
one port alone; and an address literal, which is just a host — a v6 one
taking RFC 3986 brackets to carry a port. No CIDR, no wildcard. **A
pattern in no accepted shape matches nothing rather than approximately
something**, which is the direction that fails safe: an unmatched pattern
proxies a request the caller wanted proxied anyway, where a loosely
matched one sends direct a request they asked to be tunnelled.

**Nothing is bypassed by default, loopback included**, and that is the
decision. Excluding it would be this crate deciding on a caller's behalf
that a request they asked to proxy should not be — a default that changes
what goes on the wire without being asked, which is what `TcpOpts`'
every-field-off default exists to avoid.

**`serves` is asked in two places, and both are needed.** `connect` asks
it so a bypassed origin takes the ordinary path *in full* — its resolver,
its discovery, its Happy Eyeballs — rather than a proxied path with the
proxy removed. `Native::via` asks it so the request is written in
origin-form: a bypassed request in absolute-form would reach an origin
server that never agreed to act as a proxy, which is the quieter half of
getting this wrong and is the mutation M9 exists to catch.

**One defect the tests found rather than the design.** `split_pattern`
first used a single `rsplit_once(':')`, so `::1` became "the host `::` at
port 1" — the comment above it claimed the right-hand split avoided
exactly that, and it did not. Brackets are the disambiguator, required
here for the same reason they are in an authority.

| # | mutation | verdict | killed |
|---|---|---|---|
| M8 | `Proxy::serves` ignores the list | killed | 2 |
| M9 | `Native::via` does not ask the list | killed | 1 — the *shape* assertion, the request having landed direct but written absolute-form |
