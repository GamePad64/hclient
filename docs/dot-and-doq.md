# DNS-over-TLS and DNS-over-QUIC: reachable already, and the gap is this page

Written to answer one question with evidence rather than impression: **should
this workspace grow a DoT or a DoQ backend behind the `Resolve` seam?**

The answer is **no, and the reason is not the one the question expects.** It
is not that DoT and DoQ are unwanted, nor that they are expensive. It is that
**a caller already has both today, through `hclient-dns-hickory`, without a
line changing in this repository** — executed below, against four public
resolvers, including an HTTPS/SVCB record arriving over DoT through the
unmodified seam. What was missing is that nothing outside one parenthetical
in a doc comment said so.

This is the shape AGENTS.md records under *the gap is a pointer, not a
feature*: the first consumer hand-rolled a form serialiser twice, six lines
each, for want of a `.form()` that existed, behind no feature, in the very
version it was porting against. Same defect, one crate over. So this document
is the pointer, and the recommendation is that it — plus a README paragraph
and a doc example that someone can copy — is the whole of the work.

The second recommendation is narrower and is a genuine finding rather than
documentation: **the configuration that a reader will write first does not
work, and fails in a way that names nothing.** §3.4.

---

## 0. What "checked" means here

The three grades `docs/competitive-gaps.md` uses, for the same reason:

- **executed** — a program was built and run, and its output is quoted.
- **read** — a specific file at a specific line was opened; cited as
  `path:line`.
- **not checked** — said in the sentence making the claim, never in a
  footnote.

Everything measured on 2026-08-30, on `x86_64-unknown-linux-gnu`, rustc
1.98.0 (88d9e12ae). Versions read from
`~/.cargo/registry/src/index.crates.io-*/`:

| crate | version | why |
|---|---|---|
| `hickory-resolver` | **0.26.1** | what `Cargo.lock` pins |
| `hickory-net` | 0.26.1 | where hickory's TLS/QUIC/H3 transports actually live |
| `hickory-proto` | 0.26.1 | |
| `curl` (binary on this host) | **8.18.0** | the DoH/DoT question, executed rather than read |

Download figures are crates.io's API, fetched 2026-08-30 with a
`User-Agent` — the header whose absence this workspace has already been
caught by once, when a name-availability check answered `403` for every name
including its control.

**One thing this host could not measure, said here rather than in a
footnote: it has no QUIC egress at all.** §5.3. Every claim about DoQ *on the
wire* below is therefore **not checked**, and is marked so where it appears.

---

## 1. The recommendation, first

| | verdict |
|---|---|
| Write an `hclient-dns-dot` crate | **no** — reachable today, §3 |
| Write an `hclient-dns-doq` crate | **no** — reachable today, §3; and see §5 for who you could talk to |
| Add DoT/DoQ constructors to `hclient-dns-hickory` | **no** — §4.2 |
| Document that hickory already does it, with a copyable example | **yes** — §3, §7 |
| Name the empty-root-store trap | **yes** — §3.4, the one real defect found |

The three numbers that decide it:

1. **Zero.** Lines of code in this repository that would have to change for a
   caller to resolve over DoT or DoQ today. Executed: §3.2.
2. **220 and 41.** All-time and 90-day downloads of `idoq`, the one
   Rust crate dedicated to DoQ. `idot`, its DoT sibling, is 1,544 and 632.
   This workspace has killed two features on this exact instrument — `rvcr`
   at 437 a month took record/replay with it, `rama-pac` at 57 took the PAC
   engine — and these are the same order or smaller. §6.
3. **Neither of the two most-used HTTP clients in the world implements
   either.** libcurl has `CURLOPT_DOH_URL` and has never had a DoT or DoQ
   option; `reqwest` enables `hickory-resolver` with `features = ["tokio"]`
   and nothing else, so even its optional resolver does plain UDP/TCP. §6.2.

And the thing that most changes the shape of the question, which is not a
number: **on every platform this client targets, DoT is an operating-system
setting.** systemd-resolved has `DNSOverTLS=` and no DoH and no DoQ; Android
"Private DNS" is DoT; Apple has `NEDNSOverTLSSettings`; Windows 11 has both.
A program resolving through `getaddrinfo` — which is `hclient-dns-system`,
this workspace's default — **inherits DoT on such a machine without
implementing anything**, and a client-side DoT resolver on that machine would
be a *second* encrypted resolver overriding the administrator's. §5.1.

---

## 2. What DoT and DoQ would add over the DoH we have

"It is a different port" is a fact, not a benefit. Four differences are real,
and only one of them is a benefit to a client of this shape.

### 2.1 DoH carries an HTTP tracking surface that DoT does not — real, and ours is already closed

This is the one privacy property DoT genuinely has and DoH lacks, and
**RFC 8484 says it about itself**, §8.2 (*In the Server*): HTTP's feature set
"can also be used for identification and tracking"; `Authorization` "explicitly
identif[ies] profiles in use", cookies are "designed as an explicit
state-tracking mechanism", and `User-Agent`/`Accept-Language` "often convey
specific information about the client version or locale". The RFC's own
mitigation is a `SHOULD NOT`: DoH clients should not accept cookies.

DoT has no HTTP layer at all, so none of this exists there.

**But this workspace already closes it structurally rather than by
discipline, and the argument is in AGENTS.md.** `hclient-dns-doh`'s request
is made by a `Transport`, **never by an `hclient::Client`** — the cookie jar,
the redirect policy and `Authorization` belong to `Client`, which `Transport`
has never heard of. So *a resolver's client is not the user's client* is a
thing that does not typecheck here, rather than a thing that is discouraged.
Every item RFC 8484 §8.2 warns about lives on the layer the DoH resolver
cannot reach.

So the privacy advantage DoT has over DoH in general is one this
implementation of DoH has already given up its ability to lose. What remains
is `User-Agent`, which is a constant string a caller can set, not a tracking
vector — **not checked**: whether `hclient-dns-doh` sends one at all.

### 2.2 Blocking resistance points the other way

RFC 8484 §8.1: "the use of the HTTPS default port 443 and the ability to mix
DoH traffic with other HTTPS traffic on the same connection can deter
unprivileged on-path devices from interfering with DNS operations and make
DNS traffic analysis more difficult."

RFC 7858 §3.1: "a DNS server that supports DNS over TLS **MUST** listen for
and accept TCP connections on port 853."

A dedicated port is trivially blocked; 443 among all other 443 is not. This
is a reason to prefer DoH, and it is the reason browsers chose DoH. It is not
a reason to add DoT.

**Executed, and it is a live demonstration rather than a citation.** This
host is a network of exactly the kind the argument is about: TCP to 53, 443
and 853 all open, UDP to 53 open, **and no QUIC connection to any port
succeeds** — see §5.3. DoH worked here. DoQ could not run at all.

### 2.3 DoQ's latency claim is real and is about a case this client does not have

RFC 9250's abstract: QUIC "eliminates the head-of-line blocking issues
inherent with TCP and provides more efficient packet-loss recovery than UDP",
with "latency characteristics similar to classic DNS over UDP".

Head-of-line blocking matters to a resolver multiplexing many concurrent
queries down one connection — a recursive server, a busy forwarder. An HTTP
client asks for A, AAAA and HTTPS for one origin, and this workspace already
issues those **at once** rather than in sequence: AGENTS.md records the second
commit that made discovery concurrent with the address lookups, taking a
DNS-dominated request's median from 456 ms to 340 ms and a record's cost from
404.6 ms to 0.8 ms. Three parallel queries do not queue behind each other.

RFC 9250 §4.5 also restricts the 0-RTT that would be DoQ's other latency
argument: "The 0-RTT mechanism MUST NOT be used to send DNS requests that are
not 'replayable' transactions… only transactions that have an OPCODE of QUERY
or NOTIFY are considered replayable". That is satisfiable for a stub
resolver, so it is a constraint rather than a blocker — but it is worth
noticing that it is the same distinction this workspace already draws twice,
in `RequestBody::retry_kind()` and in the `425 Too Early` replay: *can this be
sent again* is not *may an attacker send this again*.

### 2.4 Deployment reality is the deciding difference, and it inverts

| | DoT | DoQ | DoH |
|---|---|---|---|
| in browsers | no | no | **yes** (Firefox TRR, Chrome Secure DNS) |
| in libcurl | no | no | **yes** (`--doh-url`, 7.62.0) |
| in `reqwest` | no | no | no |
| as an OS setting the app inherits | **yes, everywhere** (§5.1) | no | partly (Windows 11, Android SDK 30 as DoH3) |
| public resolvers | all four probed (§3.2) | Quad9 confirmed; Cloudflare and Google **not confirmed from their own docs** (§5.2) | all four |

The row that matters for this repository is the fourth. **DoT's deployment
success is precisely as a thing applications do not implement.**

---

## 3. It is already reachable, and this is the finding

### 3.1 Why, structurally

`crates/hclient-dns-hickory/src/lib.rs:88` — the constructor takes a
**built** resolver:

```rust
/// Taking a built `Resolver` rather than re-exporting hickory's builder
/// keeps that crate's configuration surface — upstreams, DNSSEC,
/// DoT/DoH, cache policy — out of this crate's API, where it would have
/// to be version-tracked and would go stale.
pub fn new(resolver: Resolver<P>) -> Self
```

That decision, made for a different reason, is what makes this whole question
already answered. The transport a hickory `Resolver` speaks is decided in
`ResolverConfig`, which the **caller** builds. Our wrapper touches it not at
all, and `get_ref()` hands it back.

`crates/hclient-dns-hickory/Cargo.toml` asks for
`default-features = false, features = ["system-config", "tokio"]` — so *this
crate* enables no encrypted transport. It does not need to. **Cargo unifies
features across a graph**, the rule this workspace states repeatedly in the
other direction (a default is a floor; a feature switched on by one crate is
switched on for every crate). Here it works for the caller: they already
depend on `hickory-resolver` themselves — they must, to construct the
`Resolver` they pass in — so their `tls-ring` unifies onto the single
`hickory-resolver` in the graph, and the `#[cfg(feature = "__tls")]`
constructors light up.

The exact features, **read** from
`hickory-resolver-0.26.1/Cargo.toml:62-149`:

| what | feature | expands to |
|---|---|---|
| DoT, RFC 7858 | `tls-ring` / `tls-aws-lc-rs` | `hickory-net/tls-*`, `__tls` |
| DoQ, RFC 9250 | `quic-ring` / `quic-aws-lc-rs` | `hickory-net/quic-*`, `__quic`, `quinn/rustls-*` |
| DoH, RFC 8484 | `https-ring` / `https-aws-lc-rs` | `hickory-net/https-*`, `__https` |
| DoH3 | `h3-ring` / `h3-aws-lc-rs` | `hickory-net/h3-*`, `__h3` |

with `__tls = ["dep:rustls", "dep:tokio-rustls", "tokio"]`,
`__quic = ["dep:quinn", "__tls"]`, `__https = ["__tls"]`, `__h3 = ["__quic"]`.
None is in `default = ["system-config", "tokio"]`.

### 3.2 Executed

A scratch crate **outside this workspace**, depending on
`hclient-dns-hickory` by path and on `hickory-resolver` with
`["tokio", "tls-ring", "quic-ring", "webpki-roots"]`, resolving through
`hclient_dns::Resolve` — our trait, not hickory's — with no change to any
file in this repository:

```
--- DoT (tcp/853) ---
DoT cloudflare                         OK   2 addr  192 ms
DoT google                             OK   2 addr  194 ms
DoT quad9                              OK   2 addr  225 ms
DoT adguard                            OK   2 addr  233 ms
--- DoQ (udp/853) ---
DoQ cloudflare                         FAIL Resolve: request timed out  (15011 ms)
DoQ google                             FAIL Resolve: request timed out  (15011 ms)
DoQ quad9                              FAIL Resolve: request timed out  (15012 ms)
DoQ adguard                            FAIL Resolve: request timed out  (15069 ms)
--- QUIC on 443 (DoH3) vs QUIC on 853 (DoQ) ---
DoH3 cloudflare (quic udp/443)         FAIL Resolve: request timed out  (15075 ms)
DoH  cloudflare (tls tcp/443)          OK   2 addr  272 ms
--- controls ---
plain UDP/TCP cloudflare               OK   2 addr  36 ms
```

**Four DoT resolvers, four answers.** The DoQ rows say nothing about DoQ —
§5.3 — because the DoH3 row beneath them, which is QUIC on a *different*
port, fails identically. The two controls are what make that readable.

### 3.3 And the seam's own feature survives the change of transport

An `Resolve` backend is not much use here if it loses HTTPS/SVCB — that is
what `hclient-dns-hickory` exists for. **Executed**, an HTTPS lookup over
DoT — the capture below is verbatim from the run, and the method it names
was `lookup_svcb` before the seam collapsed to one `lookup(name, rtype)`:

```
supports_svcb = true
HTTPS rr over DoT: Ok(SvcbEndpoint { priority: 1, target: ".",
  alpn: [[104, 51], [104, 50]], port: None,
  ipv4hint: [104.16.132.229, 104.16.133.229],
  ipv6hint: [2606:4700::6810:84e5, 2606:4700::6810:85e5],
  ech_config_list: None })
```

`[104, 51], [104, 50]` is `h3`, `h2` — the ALPN preference list, in the
server's own order, which is the property `hclient-dns-hickory`'s own test
`alpn_keeps_the_order_the_server_sent` pins. So `Selecting`'s fast tier, the
address hints and the ECH slot all work over DoT with nothing added. There is
no half-reachable here.

### 3.4 The trap, which is the one real defect this investigation found

**The first run of the probe above failed**, and the way it failed is worth
the whole section. With `tls-ring` and **without** `webpki-roots` or
`rustls-platform-verifier`, the program compiled, the resolver built, and
every DoT query answered:

```
DoT  (RFC 7858, tcp/853): built, supports_svcb=true
    ERR Resolve: no connections available
```

**Read**, `hickory-net-0.26.1/src/tls.rs:217`, the root store at `:225-231` — `client_config()` builds

```rust
let builder = builder.with_root_certificates({
    #[cfg_attr(not(feature = "webpki-roots"), allow(unused_mut))]
    let mut root_store = RootCertStore::empty();
    #[cfg(feature = "webpki-roots")]
    root_store.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    root_store
});
```

an **empty root store**, silently, when neither roots feature is on. There is
no error at configuration time; there is a certificate failure at every
connect, reported three layers up as *no connections available*, which names
neither TLS nor roots nor a feature.

It fails **closed**, which is the right direction and is why this is a
documentation defect rather than a security one. But it is exactly the shape
this workspace has closed four times under *a missing method cannot say why
it is missing*: a caller who follows hickory's own `ResolverConfig::tls`
example gets a resolver that cannot resolve, and the message points nowhere.

**The fix is one word in whatever example we publish**, and it should be
`rustls-platform-verifier` rather than `webpki-roots`, because that is what
`DefaultTransport` already uses — a client that "just works" against the OS
trust store rather than one with explicitly chosen roots. **Executed** with
`["system-config", "tokio", "tls-ring", "rustls-platform-verifier"]`:

```
Ok(ResolvedAddr { addr: 172.66.147.243, ttl: Some(219s) })
Ok(ResolvedAddr { addr: 104.20.23.154, ttl: Some(219s) })
```

### 3.5 And the crypto provider must be the `-ring` half

`hickory-resolver` forks every one of the four features by provider.
`tls-ring` matches what this workspace picks everywhere —
`crates/hclient-tls-rustls/Cargo.toml:43` takes `rustls-ring` "and nothing
else". `tls-aws-lc-rs` would put a **second** provider in the graph, which is
the defect AGENTS.md already records from the outside: ACT's `oci-client`
brings `hyper-rustls` with `aws-lc-rs` while `hclient-tls-rustls` uses
`ring`, "so rustls correctly refuses to pick a provider and ACT installs one
explicitly."

**And the crate count hides it entirely**, measured:

| variant | crates | what changes |
|---|---|---|
| `tls-ring` + `quic-ring` | **110** | `ring` 0.17.14, `getrandom` |
| `tls-aws-lc-rs` + `quic-aws-lc-rs` | **110** | `aws-lc-rs` 1.18.0, `aws-lc-sys` 0.44.0 |

Identical counts; not remotely identical builds. `ring` is 8.2 MB of vendored
source with a build script. `aws-lc-sys` is **68 MB**, a build script at
`builder/main.rs`, and a `links = "aws_lc_0_44_0"` key — a C toolchain
requirement and a link-name that can collide. A count is a fact about a
resolution, as AGENTS.md says of every count in it; here it is a fact that
conceals the difference that matters.

---

## 4. What a native DoT/DoQ backend would cost, if written anyway

### 4.1 Crates: nothing new, which is the surprising half

**Measured**, four scratch crates outside this workspace, unique crates in
`cargo tree -e normal`:

| configuration | crates | added over the row above |
|---|---|---|
| hickory as `hclient-dns-hickory` configures it | **93** | — |
| \+ DoT (`tls-ring`, `webpki-roots`) | **104** | `ring`, `rustls`, `rustls-pki-types`, `rustls-webpki`, `tokio-rustls`, `webpki-roots`, `getrandom`, `log`, `subtle`, `untrusted`, `zeroize` |
| \+ DoT (`tls-ring`, `rustls-platform-verifier`) | 106 | the platform verifier instead |
| \+ DoQ (`quic-ring`) | **110** | `quinn`, `quinn-proto`, `quinn-udp`, `lru-slab`, `rand_pcg`, `rustc-hash` |
| \+ DoH3 (`h3-ring`) | **117** | `h3`, `h3-quinn`, `tokio-util`, `futures`, `futures-executor`, `fastrand`, `memchr` |

The premise the question raised — *quinn is already in our graph behind
HTTP/3, which may make DoQ cheaper than it looks* — is **correct, and further
than expected.** Every one of the eighteen crates in that table was checked
against this workspace's own `Cargo.lock`, and **all eighteen are already
there at exactly the same version**: `quinn` 0.11.11, `quinn-proto` 0.11.17,
`quinn-udp` 0.5.15, `h3` 0.0.8, `h3-quinn` 0.0.10, `ring` 0.17.14, `rustls`
0.23.43, `webpki-roots` 1.0.9, and the rest. Not one new crate *name* enters
an `--all-features` build of this workspace.

No `-sys` crate appears anywhere on the `ring` path. Build scripts: `ring`,
`quinn`, `quinn-udp` — all three already built by any `http3` build here.

**So cost is not the argument against DoT/DoQ, and this document should not
pretend it is.** The argument is §3 and §6.

### 4.2 But the cost is on the wrong side of the seam, and it does not go away

The 93-crate baseline is the point. `hclient-dns-doh` is **23 crates**;
`hclient-dns` is **14**. A caller reaching DoT through hickory pays 104 to get
there, and 93 of those are hickory itself — which they were already paying,
having chosen this backend. A caller who wanted encrypted DNS at 23 crates
already has it: that is DoH, and it is what this workspace built.

Which is also the answer to *should `hclient-dns-hickory` grow
`Hickory::dot(..)` constructors*: it would put hickory's `tls-*` features
into this crate's manifest, and features are additive, so the argument that
keeps `hclient-tls-quic` out of `hclient-tls` and `tungstenite` out of
`hclient-native` applies unchanged. And it would re-import into our API
precisely the surface `Hickory::new`'s doc comment says it keeps out, "where
it would have to be version-tracked and would go stale" — a claim about a
third party being exactly as perishable as the check behind it, which is this
file's neighbour's rule stated four times.

### 4.3 Neither is reachable in wasm, and hickory was already not

The claim this client makes is one API everywhere. **Executed**, `cargo check
--target`:

| | `wasm32-unknown-unknown` | `wasm32-wasip2` |
|---|---|---|
| `hclient-dns-doh` | **builds** | **builds** |
| hickory as we configure it (no DoT) | fails: `mio`, `tokio` net | fails: "Only features sync,macros,io-util,rt,time are supported on wasm" |
| hickory + DoT | fails, same, plus `getrandom` needs `js` | fails, same |

**The wasm exclusion belongs to hickory rather than to DoT** — the baseline
fails identically — so this is not a cost DoT adds. But the consequence
stands either way: a DoT or DoQ backend here would be native-only, where the
DNS privacy story this client can tell everywhere is DoH. In a browser
neither DoT nor DoQ is reachable at any price: there is no socket.

---

## 5. Who you could actually talk to

### 5.1 DoT's real home is the operating system

| platform | encrypted DNS as a system setting | source |
|---|---|---|
| Linux, systemd-resolved | **`DNSOverTLS=` (`true`/`opportunistic`).** No DoH key, no DoQ key. | `resolved.conf(5)`; systemd#8639 (DoH) still open |
| Android | **Private DNS = DoT since SDK 28**; DoH3 added at SDK 30 | developer.android.com, "bad-dns"; Android Developers Blog, Apr 2018 |
| Apple | `NEDNSOverTLSSettings` **and** `NEDNSOverHTTPSSettings`, system-wide via `NEDNSSettingsManager` or a profile | developer.apple.com, NetworkExtension |
| Windows 11 | **both** DoH and DoT in the Windows DNS client | Microsoft "Windows 11 security book", updated 2025-11-13 |

The last row **corrects the premise the question was asked under**, which
supposed Windows 11 was DoH-only. It is not; and correcting it strengthens
the conclusion rather than weakening it, because it makes the fourth row of
the table unanimous: on all four platforms an encrypted stub resolver is
something the machine's owner configures.

`hclient-dns-system` goes through `getaddrinfo`, and its own module doc
already gives the reason it is the default — "it honours whatever the machine
is configured to do — `/etc/hosts`, split-horizon VPN DNS, mDNS". Encrypted
transport is one more item on that list, and inheriting it is free.

**So a client-side DoT resolver is not additive on such a machine; it is an
override.** It replaces the administrator's upstream with the program's, which
is a decision, and it is the caller's to make — which is exactly where it
sits today.

### 5.2 DoQ's servers are thinner than DoT's

- **Quad9: confirmed**, from Quad9's own blog dated 2026-03-31 — "Quad9 has
  enabled DNS over HTTP/3 (DoH3) and DNS over QUIC (DoQ) across its global
  resolver network."
- **Cloudflare: not confirmed.** Cloudflare's own 1.1.1.1 documentation index
  (`developers.cloudflare.com/1.1.1.1/llms.txt`) lists DoH and DoT pages and
  **no DoQ page**; the DoQ URL 404s. A 2022 community claim of QUIC support is
  disputed in the same thread with no staff answer. Third-party lists asserting
  a `quic://` Cloudflare endpoint are **not** corroborated by Cloudflare.
- **Google: not confirmed.** Google's public-DNS docs describe DoT
  (`dns.google:853`) and DoH; the phrase "over HTTPS and QUIC" on that page
  most plausibly means DoH3.

And **DoH3 is not DoQ.** RFC 9250 frames DNS messages directly on QUIC
streams with no HTTP layer; DoH3 is RFC 8484 over an HTTP/3 connection. Quad9
announced them as two things enabled together, which is the tell. A client
implementing RFC 9250 today can reliably reach Quad9 and AdGuard; whether it
can reach the two largest public resolvers is **not established by their own
documentation**.

### 5.3 This host cannot test DoQ, and that is itself the §2.2 argument

Every QUIC transport failed here, including DoH3 on **UDP/443** — a different
port, the same failure, 15 s each. So the DoQ rows in §3.2 are evidence about
this network and about nothing else. **Executed** controls:

| probe | result |
|---|---|
| TCP 1.1.1.1:53, :443, :853 | all **open** |
| TCP 8.8.8.8:53, :443, :853 | all **open** |
| raw UDP query to 1.1.1.1:53 | **94-byte reply** |
| `curl https://cloudflare-quic.com/` over TCP | **200**, 0.32 s |
| DoH3 / DoQ, any resolver, any port | **timeout** |

UDP to 53 passes and UDP to 443 and 853 does not. That is a filtering policy,
and it is the ordinary one: **a network that permits UDP only to the DNS
port is a network on which DoQ cannot work and DoH can.** It is the same
condition AGENTS.md's Alt-Svc race is a hedge against — "a network that blocks
UDP/443" — met from the DNS side.

Incidentally, `curl 8.18.0` on this host, the Ubuntu build, has **no HTTP/3
at all**: `--http3-only` reports "the installed libcurl version does not
support this". The most-deployed HTTP client on Linux cannot speak QUIC as
shipped.

**Not checked**, and it would need a host with UDP egress: whether DoQ works
against Quad9 and AdGuard through this seam. The code path is hickory's and
is the same one DoT proved out, so the expectation is that it does; expectation
is not measurement, and this sentence is the place that says so.

---

## 6. Demand, measured

### 6.1 The dedicated crates are in the noise

crates.io API, fetched 2026-08-30. **90-day** figures as crates.io reports
them, not divided.

| crate | all-time | 90-day | what it is |
|---|---:|---:|---|
| `hickory-resolver` | 71,548,468 | 20,451,154 | the stub resolver everyone actually depends on |
| `hickory-server` | 3,469,546 | 1,378,725 | the crate whose DoT/DoQ support is *used* |
| `domain` (NLnet Labs) | 11,499,206 | 1,024,238 | second DNS stack; has DoT/DoQ |
| `dns-over-tls` | 1,762 | **9** | "simple DoT proxy" |
| `idot` | 1,544 | **632** | dedicated DoT client |
| `idoq` | **220** | **41** | dedicated DoQ client |
| `hopf-dns` | 175 | 175 | stub/forwarder, UDP/TCP/DoT/DoQ/DoH |
| `doh-core` | 64 | 64 | DoH/DoT/DoQ client library |
| `doh-cli` | 48 | 48 | CLI |

**This workspace's own precedents.** `rvcr` at 437 downloads a month killed
record/replay: "Nobody in Rust wants a VCR, and knowing that before proposing
it is worth more than the feature would have been." `rama-pac` at 57 killed
the PAC engine, alongside the finding that neither reqwest nor curl runs a PAC
script. `idoq` at 41 in ninety days is *below* the PAC crate on the same
instrument.

### 6.2 The two clients that matter do not do it

- **libcurl: DoH only, and never DoT or DoQ.** curl's complete
  symbols-in-versions list contains `CURLOPT_DOH_URL`,
  `CURLOPT_DOH_SSL_VERIFYHOST`, `CURLOPT_DOH_SSL_VERIFYPEER`,
  `CURLOPT_DOH_SSL_VERIFYSTATUS` and **zero** symbols matching `DOT` or `DOQ`.
  DoH arrived in 7.62.0 (2018); nothing since. Confirmed locally against the
  8.18.0 binary: `--doh-url` exists, no `--dot*`/`--doq*` option.
- **reqwest: neither, and not even DoH.** Its `hickory-dns` feature is
  `hickory-resolver = { version = "0.26", optional = true, features =
  ["tokio"] }` — the `tokio` feature alone, none of `tls-ring`/`https-ring`/
  `quic-ring`/`h3-ring` — and `src/dns/hickory.rs` builds via
  `builder_tokio()`, which reads `resolv.conf`. The feature swaps the *stub
  resolver implementation* for plain DNS; it exposes no encrypted transport at
  all.
- **Browsers: DoH only.** Firefox TRR and Chrome Secure DNS. No DoT or DoQ
  implementation was found in either, and no source claims one.

### 6.3 Where the demand is, it is server-shaped

Reverse dependencies, crates.io:

- `hickory-server` — the crate that implements DoT/DoQ/DoH **server-side** —
  has **26** reverse dependencies, and the visible ones are all daemons and
  forwarders: `crab-hole`, `koi-dns`, `aardvark-dns`, `iroh-dns-server`,
  `watfaq-dns`, `constellation-server`. No HTTP client library among them.
- `hickory-resolver` has **445**, topped by `reqwest`, `awc`, `actix-tls`,
  `mongodb`, `fred`, `libp2p-dns`, `iroh` — every one of them using it for
  plain stub resolution.

**Encrypted DNS transports in Rust are consumed by things that are DNS, not
by things that speak HTTP.** That is the clearest single statement of why this
belongs behind our seam rather than inside it.

**Not measured**, and it is the number that would settle it hardest: what
fraction of `hickory-resolver`'s 20.4 M 90-day downloads come from builds with
any `tls-*`/`quic-*`/`h3-*` feature on. crates.io does not report per-feature
breakdowns.

---

## 7. The bootstrap question, which turns out not to exist

`hclient-dns-doh` answers it in the type system, and AGENTS.md states the
shape: "`Doh::pinned` takes an IP literal and refuses a name,
`Doh::bootstrapped` takes a name and refuses a literal… Failing closed is the
default and failing open is visible in the type — `Doh<C>` is
`Doh<C, NoFallback>`."

**A DoT or DoQ backend would reuse neither, because the question cannot be
asked.** **Read**, `hickory-resolver-0.26.1/src/config.rs:308` and `:352`:

```rust
pub fn new(ip: IpAddr, trust_negative_responses: bool,
           connections: Vec<ConnectionConfig>) -> Self
...
pub fn tls(server_name: Arc<str>) -> Self
pub fn quic(server_name: Arc<str>) -> Self
```

The upstream's address is an **`IpAddr`** — a type, not a string — so there is
nothing to resolve and no bootstrap. `server_name` is not an alternative to
it: it is the TLS certificate identity, an input to verification and never to
resolution. DoT and DoQ are structurally `Doh::pinned`-only, and hickory's
signature already enforces it at the strength `Doh::pinned` achieves by a
runtime refusal.

**This is why DoH needed the distinction and DoT does not**, and it is worth
stating because it reads backwards at first: DoH's upstream is a *URL*, and a
URL's host may be a name, so the constructor must decide what resolves it. A
DoT upstream is an address plus a certificate name. There is no third thing.

One consequence, which a published example should say: `ProtocolConfig::Tls`
sets `enable_sni = false` — **read**, `hickory-resolver-0.26.1/src/tls.rs:36-37`,
with the comment "The port (853) of DOT is for dns dedicated, SNI is
unnecessary. (ISP block by the SNI name)". So a DoT connection does not put
the resolver's name on the wire in the clear, which a DoH connection to a
named endpoint does — a small real privacy difference in DoT's favour,
narrower than §2.1's and, unlike it, not already closed here. ECH would close
it, and AGENTS.md records that no TLS backend in this workspace applies one.

---

## 8. What would change this answer

Written down so that re-asking has a trigger rather than a mood, which is
what the acceptance documents' *deliberately not done* lists are for.

1. **A consumer asks.** The instrument this workspace trusts is a consumer
   written from outside, and it has been right three times. Nobody has asked
   for DoT or DoQ. If someone does, the first answer is §3 and it costs a
   link.
2. **`idoq`, or any dedicated DoQ crate, clears four figures a month.** It is
   at 41 in ninety days. Two features have already been killed at 437 and 57.
3. **Cloudflare or Google publishes a DoQ endpoint in their own
   documentation.** Today only Quad9's is confirmed by its operator (§5.2).
4. **`hickory-resolver` stops taking a caller-built `Resolver`**, or moves the
   transport configuration somewhere `Hickory::new` cannot pass through. That
   would break §3.1's whole argument, and it is the one thing here that is
   somebody else's release schedule.
5. **A `no_std`/wasm DoT becomes meaningful** — it cannot today, and the
   obstacle is not DoT: hickory does not build for either wasm target with or
   without it (§4.3), and in a browser there is no socket at any price.

None of these is close.

---

## 9. Summary

**DoT and DoQ should not be built here.** They are not refused for cost —
every crate they would add is already in this workspace's lockfile at the same
version — and not because they are worthless. They are refused because a
caller who wants either already has it, through the backend this workspace
already ships, by the constructor that already exists, and it was executed
above against four public resolvers with HTTPS/SVCB records arriving intact.

What was actually missing is a paragraph. Three things belong in it:

1. `hickory-resolver`'s feature — `tls-ring` for DoT, `quic-ring` for DoQ —
   **and the `-ring` half specifically**, §3.5.
2. **A roots feature, or nothing resolves and the error says nothing**:
   `rustls-platform-verifier`, matching `DefaultTransport`. §3.4 is the one
   defect this investigation found and it is the reason the paragraph is worth
   writing rather than merely tidy.
3. That on Linux, Android, Apple and Windows the machine may already be doing
   DoT, in which case `hclient-dns-system` — the default — inherits it, and a
   client-side resolver overrides rather than adds. §5.1.

The honest one-line version, for whoever re-asks in three months: **we do not
implement DNS-over-TLS; we hand you a resolver that does, and the reason you
could not find that is that we never wrote it down. Now we have.**
