# Proxies

> Part of the [AGENTS.md](../../AGENTS.md) record, split 2026-10-05. The root
> file keeps the operational rules and the index of parts. Sections below
> keep their original headings, and a "this file" in the text named the
> whole record when it was written.

### The proxy protocols became sans-io, and `CONNECT` stopped needing an HTTP client

`hclient-proxy`: the seam, `Proxy<P>`, the bypass matcher and all three
protocols, as **state machines with no IO trait in them at all**. A
handshake is handed the bytes that arrived and answers with the bytes to
send, or *not yet*, or *the tunnel is open*; `hclient-native`'s driver is
thirty lines and is the only thing left in the family that knows what a
`poll_read` is.

**The premise of the move was `CONNECT`, and it was the one protocol that
looked immovable.** It drove `hyper`'s h1 dispatcher through
`crate::upgrade` — `http1::Connection`, `poll_without_shutdown`,
`into_parts` for the leftover read buffer — because writing one request
and reading one response needed an HTTP client. What made it movable is a
fact about the *message* rather than about hyper: **a `CONNECT` response
has no body under any framing rule** (RFC 9110 §9.3.6), so chunked
decoding, `Content-Length` and the interaction between the two — the hard
half of HTTP/1 — have no subject. What replaced forty lines of dispatcher
is `hclient_proto::head`, forty lines of parser.

**That parser is `winnow` and the head is a grammar**, which is this
workspace's own rule about where combinators pay, applied for the third
time and coming out the other way from the last two: `Cache-Control` was a
grammar and shrank, `charset` was a *cut* and grew. The input is
`winnow::Partial`, so **incomplete is the parser's answer rather than a
scan of ours** — the case a hand-written scan gets wrong is a terminator
split across two reads, and a scan restarted from the beginning on every
read is quadratic in the head's length. `a_head_arriving_one_byte_at_a_time_is_never_wrong_before_it_is_complete`
pins it at every prefix.

**Three refusals are the parser's own**, each a `MUST` somewhere: a bare
`LF` (RFC 9112 §2.2 *permits* accepting one, and two line grammars is a
thing two implementations disagree about), an obs-fold continuation (§5.2),
and whitespace before the colon (§5, the request-smuggling shape).

**What sans-io bought is not tidiness, it is reach.** Every rule in every
protocol is now killed by a test that opens no file descriptor, against
the byte sequences the RFCs print — and the SOCKS5 reply, whose length is
not known until `ATYP` and its length byte have arrived, is fed one byte
at a time with the buffer asserted untouched at each step. The contract
that makes that possible is one sentence and it is the only thing the two
SOCKS protocols share: **a handshake that answers `NeedMore` has consumed
nothing.** They share no bytes at all otherwise — `VN=4` against `0x05`,
`CD=90` against `REP=0`, NUL-terminated fields against length prefixes —
which is the same evidence for the seam that two protocols sharing nothing
was before the move. What they *used* to share, `frames.rs`'s byte-exact
IO, is gone: the driver does it once for all three.

**The seam is narrower than the trait it replaced, and the loss is
stated.** A protocol that has to **wrap** the IO cannot be written against
`Handshake` — TLS to the proxy itself is the real example — where the old
`ProxyProtocol::tunnel` took the stream and could have. That is not a
regression: it was unsupported before. **It is lifted now, and not in
the driver**: TLS to the proxy is `Dial::connect_tls`, run by the
transport before the handshake, so a `Handshake` still never sees
anything but a stream and `ParseError::TlsToProxyUnsupported` is gone.

**One knob turned out to have one setting and was deleted.**
`upgrade::exchange` took the accepted status as a parameter *because there
were two callers*, and its doc said so; with `CONNECT` gone there is one,
and a parameter with one value is the distinction-with-one-reachable-side
that `UpgradeSupport`'s spare variants were deleted for. The `101` check is
inlined and the paragraph explaining the parameter is now the paragraph
explaining its absence — which is the maintenance this file records
failing three times over.

**The regression net was already there, and it is the reason this was
attemptable at all.** `tests/proxy.rs` watches bytes from the proxy's own
side of the wire — the request line's shape, the origin travelling by name
— and its 20 tests passed against the rewritten implementation unchanged.
A refactor of a connect path with no such net would have been a different
proposition.

### The machine's own proxy settings, read by us, and the PAC script reported rather than run

`hclient-proxy`'s `system` feature, `Native::system_proxy()`, and
`hclient`'s `proxy`/`system-proxy` features so that a caller reaches all
of it without naming `hclient-native` — with `hclient::default_transport()`
as the piece that was missing, since a proxy is configured **on the
transport** and there was no way to get one from the facade.

**`Client::new()` reads them, and that is the good-citizen half.** It is
the `system-proxy` feature, **in `default`**, and no call: a
convenience constructor that ignored `HTTPS_PROXY` would be the one
program on the machine that does, and a port from curl or reqwest — both
of which honour it by default — would silently start going direct.
`default_transport()` deliberately does **not** read, because it is the
seam for configuring a transport and `unix_socket` refuses when a proxy
is configured: a chain that failed on machines with an `HTTP_PROXY` and
not on others is worse than an explicit line.

**The feature is positive and sits in `default`, which took the weak
form to be affordable.** `system-proxy = ["hclient-native?/system-proxy"]`
— with the question mark — reaches the transport only where the transport
is already in the graph, so a build without `default-transport` pays
nothing and a consumer turns the behaviour off with
`default-features = false`. The plain form would have pulled
`hclient-native` in, and tokio, rustls and the system resolver with it,
into every graph that took this crate's defaults: the floor
`default-transport` was reversed out of `default` for, arriving by the
back door. `just graph-default-has-no-transport` asserts both directions
and was checked by deleting the `?` and watching it fail.

**A feature that turned proxying off was the shape considered first and
it is the wrong one.** Cargo unifies features, so a negative switch is
unified too: one crate in a dependency tree could take proxying away from
every other, silently. A positive default that a consumer drops with
`default-features = false` puts the decision with the build that wants
it, which is the contract everybody already knows. The runtime lever —
`Client::builder(default_transport()?)`, which reads nothing — answers
the other question, and is pinned by `tests/proxy_default.rs` rather than
described.

That split needed a second translation rather than a second policy.
`system::rules` **refuses** a configuration this client cannot express
in full, and `rules_lossy` **installs what it can and reports the
rest** — because a refusal is only useful to somebody who can act on it,
and `Client::new()` did not ask. A constructor that refused would be a
client that will not start on a network with WPAD. Nothing is silent at
the API level: the lenient half returns the report, and only the
constructor discards it. A machine with a PAC script *and* a static
proxy gets the static one, which is WinINET's own fallback rather than an
invention of ours.

**`DefaultTransport` names `HttpConnect` even where no proxy is
configured**, and that is what keeps it one type: `Client::new` builds a
proxied transport on a proxied machine and an empty-listed one
everywhere else, so an alias naming `NoProxy` would have made
`transport_as::<DefaultTransport>()` — the documented way past the facade
— work on one machine and not on the next. Caught by the test that pins
two handles sharing one transport, which is the only place that downcast
is exercised.

**The readers are ours, and taking a crate for them was tried first and
measured.** `proxy_cfg` does all of it in one dependency; it also depends
on `url`, and through it on `idna` and the ICU tables — **28 crates**, on
exactly the two targets `hclient-idn` exists to keep those tables off —
and it pulls a second `windows-sys` major through `winreg`. Written here
over `windows-registry` and `system-configuration`, both of which expose
**safe** APIs, the cost is: **nothing on Linux**, where the environment
needs no crate at all; **+4** on Windows; **+6** on macOS; **+9** on
Android. And two things `proxy_cfg` does not read at all are now read —
the auto-config URL, and the platform's own SOCKS entry.

**Android is the dear one, and the reason is where its settings live.**
Every other platform keeps them in a file, a registry or a dictionary a
process can read; Android keeps them in **JVM system properties**, so
reaching them means calling into managed code — `jni` and `ndk-context`,
measured at 19 crates to 28, target-gated so no other build sees them.
Those properties rather than anything else because `java.net`'s own
`DefaultProxySelector` reads exactly them, which is what makes a proxy
this client takes the proxy every other client in the process takes.
An app that already carries `rustls-platform-verifier` has paid for the
same handle, since reaching the Android trust store needs it too.

That is also why the Android **backend** was weighed and not built: the
`rustls-platform-verifier` measurement leaves system proxy and the
cleartext policy as the whole of what a platform stack would add, and a
reader is what a setting needs — the same conclusion `hclient-urlsession`
reached from the other side when MDM roots turned out to be reachable
without it.

**Every rule is a pure function, and the OS-touching half holds no
decisions.** `WinHttpGetIEProxyConfigForCurrentUser`'s registry keys,
`SCDynamicStoreCopyProxies`'s dictionary and Android's four JNI calls are
a handful of lines each and cannot run on the platform this workspace is
developed on; what they hand to
`from_wininet` and to `from_parts` is data, and every rule — the
`scheme=host:port` list, `<local>`, which key means which scheme, the
`host:port` reading — is tested on any host. That is
`system-resolver`'s split between `sys` and its parsers, applied again —
named for `hclient-dns-system` until those platform modules moved there.

**Everything ambiguous is a named refusal rather than a quiet narrowing.**
A transport holds one proxy protocol, so a machine naming a SOCKS proxy
*and* an HTTP one is refused naming the SOCKS one; a bypass pattern the
matcher cannot state exactly is refused naming the pattern; a credential
that cannot become a header is refused naming its proxy. Each
alternative is the same defect in a different direction — dropping a
proxy sends traffic direct that the machine's owner routed, dropping a
bypass sends traffic through a proxy they excluded — and neither is
visible from the call site. This is the *silently ignored setting* defect
one layer below `Capabilities`, where the setting comes from the machine
rather than from the caller.

**A PAC script is the fourth refusal and the sharpest**, because ignoring
it means going **direct** on a machine whose owner routed its traffic
through a proxy — a policy violation, and on a network with no direct
egress a failure nobody can explain from the client's side. It is asked
first, before the static entries, because WinINET keeps those as the
script's *fallback*: honouring them would be taking the machine's second
answer while ignoring its first.

**Two rules came from looking at what a Mac actually ships, and both
would have been wrong by reasoning alone.** `Proxy::bypass_local()` is
the `<local>` / *Exclude simple hostnames* rule — a rule about the shape
of a name, so a flag rather than a pattern — and macOS ships it **on**.
And the bypass dialect grew a **subnet** form, `10.0.0.0/8` and the
abbreviated `169.254/16`, because `169.254/16` is in the default
exceptions list of every Mac: the design refused a subnet on the grounds
that the matcher deliberately has no address arithmetic, which would have
meant refusing the platform's own default configuration. A subnet never
matches a **name**, not even one that resolves into it — matching would
mean resolving a host to decide whether to proxy it, which is an extra
lookup and, on a proxied request, the DNS leak a proxy user is often
there to avoid.

**One `unsafe` came with the macOS reader, and it is amendment C13.**
`core-foundation` implements `ConcreteCFType` for `CFArray<*const
c_void>` alone, so the exceptions list can be downcast to an untyped
array and to no other, and its elements arrive as pointers with no safe
way to read one — checked in 0.9 and 0.10, and `objc2-core-foundation`
has the same wall one level up, at the dictionary. Skipping the list was
weighed and is worse, for the reason above: every Mac has one. What is
assumed is only that the pointer is a valid CF object; **which class it
is, is checked** — `downcast::<CFString>()` compares the type id. The
crate carries `#![deny(unsafe_code)]` rather than losing the attribute.

**Running the script was built, measured and withdrawn, and the
measurement is what is kept.** A PAC file is a JavaScript program, so
honouring one means carrying a JavaScript engine. It was written —
evaluator, the twelve host functions, 24 tests — and then removed, for
three reasons that are all about demand rather than about code:

- **reqwest does not run one**, checked by `grep` over 0.13.4's source:
  zero mentions. Neither does curl. The two most-used HTTP clients in the
  world ship without it.
- **The whole Rust ecosystem has one PAC crate**, `rama-pac`, at **57
  downloads** against its parent framework's 65,062.
- **It had no consumer here and no user anywhere.** `hclient-native`
  never asked it anything, and `hclient` is not published, so there was
  no request to answer. A feature with no reader is the shape this file
  records deleting `UpgradeSupport`'s spare variants for, one size up.

What it would have cost, measured on one program — the same PAC file and
the same four host functions on each engine, `opt-level = "z"`, fat LTO,
`panic = "abort"`, stripped:

| engine | crates | binary | ran the file |
|---|---|---|---|
| `boa_engine` 0.20 | 114 | 3,798 KiB | yes |
| `viperjs` 0.3 | **2** | **1,563 KiB** | yes |
| `nova_vm` 1.0 | 169 | — | not tried, heavier than Boa |

Two findings from that worth keeping. **Boa carries ICU without the
`intl` feature** — `icu_normalizer(_data)`, `icu_properties(_data)`,
`icu_collections`, `icu_locid(_transform)`, `icu_provider` — and it has
**no `default` feature at all**, so `default-features = false` buys
nothing; `icu_properties_data` alone is the 1.9 MB this file measures
elsewhere. And a 2-crate engine really does run a real PAC file
correctly, at 2.4× less binary — the argument against it is not size but
**WPAD**: a script can arrive from DHCP or DNS rather than from a
setting, which makes the engine an attack surface fed by the network, and
86% of test262 from one author is not the thing to point at it.

The wiring was never designed either, and its first question is the
sharpest: **fetching the script needs an HTTP client**, which is the
bootstrap problem `Doh::pinned`/`Doh::bootstrapped` solves for DNS and
nobody has solved here.

**What stayed is the half that was actually missing**:
`SystemProxies::pac()` reports the URL, behind no feature and at no
dependency cost, and `SystemProxyRefused::PacScript` turns a PAC machine
from *silently direct* into a named refusal pointing at
`hclient-urlsession`, which runs the script in the OS. The engine was
never what closed that defect.

**That last thing was recorded as knowable and not done, and the
obstacle recorded for it was the whole of the fix.**
`hclient-urlsession` reported `Capabilities::proxy == false` while the OS
applied a proxy underneath it — a capability that lies, the class this
file treats as worse than a silent downgrade because a caller can act on
a capability. What stood in the way was that `SystemProxies::detect`
reads the environment first, as curl and reqwest do, and `URLSession`
takes its proxies from the system configuration instead. So the addition
is `SystemProxies::detect_platform()`, which reads the platform store and
skips the environment, and the value is
`detect_platform().names_a_proxy()`.

**The order is not a preference, it is the only one that cannot
over-claim.** An environment-first read reports `true` on a machine whose
only proxy is a variable this transport ignores; the platform-only read is
exact if `URLSession` honours the system configuration alone and short of
the truth rather than ahead of it if it honours more. Which of those two
is the case is not settled by anything Apple publishes — DTS's own answer
is *"NSURLSession takes care of proxies for you"* and no more — so the
design is built to be right under either.

**`names_a_proxy` is not `is_empty` negated, and a PAC script is what
separates them.** `is_empty` answers *is there anything here this client
can install*, and a script is not, because nothing here runs one;
`names_a_proxy` answers *does the machine route through a proxy*, and a
script does. The collapse of an unknowable answer onto a `bool` is
towards `true` for the reason `SystemProxyRefused::PacScript` exists:
reporting a PAC machine as unproxied is the *silently direct* answer. A
`*` bypass is checked first and answers `false` even beside a script,
which is the under-claiming reading of a corner nothing documents. A
SOCKS entry answers `true` although `rules` refuses to install one
— the report is read off the machine, not off what this client could do
with it.

**What it still cannot see is WPAD auto-discovery**, which macOS spells
`ProxyAutoDiscoveryEnable` and Windows keeps in a binary blob: this module
reads a script the machine *names*, and a discovered one has no URL to
report, so honouring it needs an answer `SystemProxies` has no shape for.
Stated rather than fixed, and it is the under-claiming direction.

The split is `system-resolver`'s: every rule is
`hclient-proxy`'s and is tested on this workspace's own Linux hosts, the
`URLSession` side is one expression, and the environment-exclusion is
pinned by a test that re-runs the test binary as a **child process** with
an `HTTPS_PROXY` in its environment — `std::env::set_var` being `unsafe`
in edition 2024 — and compares the child's `detect_platform` against the
parent's, so it asserts nothing about the host it runs on. The cost is
measured: `hclient-urlsession` goes from **19 crates to 30** on
`aarch64-apple-darwin`, four of them the platform bindings that do the
reading and none of them `url` or ICU, which `just graph-proxy-cost`
asserts in both directions. It is **31** since the delegate's queue became
a channel — `futures-channel` alone, because `futures-core` and
`futures-sink` were already there through `hclient-core`, which names both
on the WebSocket seam.

### Proxies: an HTTP one and SOCKS5, behind one seam

`Native::proxy(Proxy::new(protocol, host, port))`, behind `hclient-native`'s
`proxy` feature, off by default. It changes `P` the way `.hooks(..)` changes
`H`. Two protocols ship and they **share no bytes** — one is HTTP, one is
RFC 1928 — which is what makes the seam evidence rather than a claim, the
same standard `Transport` and `WebSocketConnect` were held to.

**It is not a decorator over `TcpConnect`, and the reason is that seam's
signature.** `connect` takes a `SocketAddr` and nothing else, so a wrapper
could never hand the proxy the origin's *name*: the client would resolve it
locally and leak exactly the DNS a proxy user is often there to hide, and
`http://` could never take absolute-form, which is decided where the request
head is written. SOCKS5's `ATYP=0x03 DOMAINNAME` is what proves the leak is
a property of that seam rather than of proxying. So a proxy **replaces** the
resolve → Happy-Eyeballs → connect block: the resolver is not consulted for
the origin, HTTPS/SVCB discovery does not run, and Happy Eyeballs races the
*proxy's* addresses instead.

**Not `Box<dyn ProxyProtocol>` either**, and this crate's own history is the
argument: erasing the protocol erases the IO with it, and a boxed IO needs a
`Send` — the objection that disqualified `hyper::upgrade::Upgraded` for the
WebSocket work and `hyper/http2` before it. Hence a type parameter,
defaulted to `NoProxy`, which is an **empty enum**, so a transport nobody
configured holds an `Option` that cannot be `Some` rather than a stub that
exists to be absent.

**The `CONNECT` tunnel turned out to be the WebSocket upgrade seam with a
different accepted status**, and that was read in hyper 1.11 rather than
hoped: its h1 client sets `wants_upgrade` for `Method::CONNECT`
(`role.rs:240`) and skips the body for `CONNECT` + `is_success` (`:518`),
so `into_parts` yields the tunnel and the bytes read past it exactly as it
does for a `101`. `upgrade::exchange` takes the status test as a parameter,
so there is one copy of those forty delicate lines instead of two. The
request line needed nothing from hyper at all — it writes `http::Uri`'s
`Display` verbatim, so authority-form and absolute-form are both a matter
of handing it the right `Uri`.

Three things worth knowing before touching it. **A `407` refusing a tunnel
is `ErrorKind::Connect`, never a response** — it is the proxy's answer to
us, not the origin's to the caller — while a `407` answering an
absolute-form request *is* a response, from a server acting as origin for
it; both are pinned. **A non-empty `read_buf` after a tunnel is a refusal
rather than a rewind**, because nothing the origin might say can have
arrived before we wrote to it. And **the proxy in `PoolKey` is unreachable
today**, for the reason `.notes/v02-acceptance.md` already gives about the
TLS identity in the same key — a constant within any one pool — and it is
in the key for the moment a pool is shared between transports. That
unreachability is this work's mutation **control**, and it survives as the
comment predicts.

**`Proxy::bypass([..])` is the half of `NO_PROXY` that is not policy**, and
the split is the point: *reading the environment* is policy — which
variables, whose matching dialect, whether a library may read the
environment at all — and belongs to whoever builds the transport, where a
list the caller wrote down is not policy at all. The rules are small
because `NO_PROXY` has no specification and every implementation disagrees
about the corners: exact host at any port, `.example.com` for a domain and
everything under it, `host:port` for one port, and an address literal —
a v6 one taking RFC 3986 brackets to carry a port. No CIDR, no wildcard,
and a pattern in no accepted shape matches **nothing** rather than
approximately something. *(Both have since moved: subnets are accepted,
`*.x` is read as `.x`, and a pattern in no accepted shape is now
**refused** at `Proxy::bypass` rather than kept as one that never matches
— see the third pass over this crate's surface.)* **Nothing is bypassed by default, loopback
included**: excluding it would change what goes on the wire for a caller
who asked to proxy everything, which is what `TcpOpts`' every-field-off
default exists to avoid. The list is asked in two places — `connect`, so a
bypassed origin takes the ordinary path *in full*, and `Native::via`, so
its request is written origin-form rather than reaching an origin server
that never agreed to act as a proxy.

The feature has **no `dep:` entries**: neither protocol needs a third-party
crate, base64 included, so it buys code size back for a constrained target
and costs nobody a dependency — the WebSocket framing's argument has no
subject here. The seam itself is unconditional, because a third protocol
should not have to switch on a feature named after the two that ship.
`.notes/proxy-design.md`.

### The pooled-reuse race has three points, and the middle one was ours

A server can close a pooled HTTP/1 connection between the client's last
look and its write. Every HTTP/1 pool has this; `h1.rs` has called it
"residual" since v0.2 W2 and `.notes/nagle-and-nodelay.md` §6 names the two
expensive fixes it would take. Reproducing it deterministically — which
had never been done, because *"that instant cannot be hit from outside"* —
showed the window has **three** points rather than two, and that the
middle one was not a race with the network at all.

`try_send_request` puts the request into hyper's queue **eagerly**, when
the future is built. hyper's `poll_loop` then reads before it writes, and
a graceful EOF on an idle connection sets `close_read`, which makes
`can_write_head()` false — so the dispatcher **refuses to write**,
finishes with `Ok`, and says nothing about the request, which is still
sitting in its queue as a whole `http::Request`. This crate called that
`Failed::Sent`, with a comment reading *"we no longer own the request, so
there is nothing to hand back"*. The first half was true; the second was
one `drop` away from being false. hyper's `Envelope::drop` answers the
promise of every still-queued request **with the request attached**, and
that receiver lives inside the `Connection` the function is holding.

`h1::claim_back` drops the connection and asks once. **The verdict stays
hyper's** — nothing here judges what looks safe to resend, which is the
contract `Failed`'s doc states; what changed is that the question is now
asked at a moment when hyper can answer it. The far point is unchanged:
a request already taken apart and written out is `Failed::Sent`, the
caller is told, and at-most-once is intact.

**That is also why the attempt §6 records failed.** It polled the send
future on the connection's error arm *without* dropping the dispatcher —
asking a promise nothing would ever fulfil — and moved 9 failures in 20
to 6, a number that could not carry a decision. The mechanism was one
line away.

Measured twice, deterministically, and the two forms share no code.
Scripted in `h1.rs`, driven poll by poll with a noop waker: the EOF
placed at each point gives `NotSent` / **`Sent`** / `Sent` before and
`NotSent` / **`NotSent`** / `Sent` after. On a real socket through the
whole transport, with `LateEof(Tokio, n)` hiding the peer's `FIN` from
exactly `n` looks — eight sweeps of six arms each side, no disagreement
within a column:

| EOFs hidden | who finds the close | before | after |
|---|---|---|---|
| 0 | the pool's checkout poll | `200`, 2 accepts | `200`, 2 accepts |
| 1 | `exchange`'s look, request still ours | `200`, 2 accepts | `200`, 2 accepts |
| 2 | hyper's first read, request queued | **error, 1 accept** | **`200`, 2 accepts** |
| 3+ | hyper's read after writing | error, 1 accept | error, 1 accept |

**The two expensive fixes are still refused, and now for sharper reasons
than cost.** Suspending before the request is handed over buys nothing
certain — *a yield is not a fence*: it gives the reactor one more chance
to have delivered the `FIN`, moving the window rather than closing it,
and it costs a scheduler round trip on every pooled request plus the
*"exactly one poll, and it never suspends"* contract written where that
poll is. Replaying a request hyper will not hand back needs a notion of
method safety this codebase deliberately does not have, the same one
`.notes/h3-research.md` §3.5 declines for 0-RTT; `RetryKind` answers only
half of it, and the `425` precedent argues the other way, because there
the **server** asked for the repeat.

Two things worth knowing before touching it. The error a caller reads is
the **connection's cause and not hyper's answer to our own drop** —
`dispatch_gone` describes the drop — and that is not decoration:
`Native::run` discards a `NotSent` error because it retries, but
`Staged::exchange` carries no retry and surfaces it. That was a mutation
that survived all 278 tests before it was an assertion. And the
`LateEof → point` mapping is exact only while the server's close is
**late**: with a prompt close the *first* exchange's own teardown read can
meet the `FIN` and spend one of the hidden EOFs, shifting every row by
one — seen once, in one arm of one sweep, and gone in the 48 since.

**The three points report `Stale`, `Ended`, `Ended` — and the first two
are one socket in one state**, now pinned rather than corrected. Both
names are true at the middle row above (the peer closed it after a
response, *and* it was handed out already closed), and which one a caller
is told is decided by which of two adjacent polls noticed. This work neither introduced it
nor changes it; what it changes is that `Stale`'s own promise — *"the
event that explains the `Connected` following it"* — now has a
`Connected` following the middle row too. Deciding the reason from what
the request did would move the emission below the request's outcome and
put the one-`Closed`-per-socket rule behind three exits where `h1.rs`
leans on two, which is a hooks change wanting its own measurement.

`.notes/pooled-reuse-race.md`, including the mutation table and its
control — the connection's *error* arm, verified unreachable by replacing
it with a `panic!` and running the suite rather than by reading hyper.
