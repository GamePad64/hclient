# TLS fingerprint control: a feasibility study, and a refusal that changed shape

`docs/competitive-gaps.md` §2.5 and §4 record JA3/JA4 fingerprint control
as **refused**, on two grounds: *rustls closed it not planned*, and
*`http::HeaderMap` lowercases names, so browser header casing is
unreproducible anyway*. The owner reopened the question. This document is
the answer, and it re-decides rather than re-states: **one of the two legs
survives inspection, the other is half wrong, and the refusal stands on a
third reason neither of them named.**

Nothing here is implemented. Every claim is graded the way
`docs/competitive-gaps.md` §0 grades them:

- **executed** — a program was built and run, and its output is quoted;
- **read** — a file at a version is named. Where the source is a GitHub
  issue or a specification rather than a file on disk, the issue number or
  the document is named instead, and the quotation is verbatim;
- **not checked** — said in the sentence that makes the claim.

The programs written for this live in no repository. What they did is
described precisely enough to be written again, because a measurement
whose method is not recoverable is an anecdote.

---

## 1. What this client looks like today, measured from outside

The first thing to establish is not what a fingerprint *is* but what this
workspace's fingerprint *already is*, because a design that starts from
the specification rather than from the artefact tends to solve the wrong
distance.

**Executed.** A scratch crate depending on `crates/hclient` by path with
`default-transport` and `hclient-native/http2`, one `Client::new()`, one
`GET https://tls.peet.ws/api/all` — a third-party service that reports
back the fingerprints it computed for the connection it just served:

```
ja4          : t13d1011h2_61a7ad8aa9b6_3fcd1a44f3e3
ja3_hash     : d1286137b6bd6eab47903c590c86f63c   (this run only — see §3)
akamai h2 fp : |00|0|m,s,a,p
```

Beside it, on the same host and the same afternoon, real Chrome 152
headless and the system curl, fetching the same URL:

| client | JA4 | Akamai HTTP/2 |
|---|---|---|
| **`hclient`** (rustls 0.23.43 + ring, h2 0.4.19) | `t13d1011h2_61a7ad8aa9b6_3fcd1a44f3e3` | `\|00\|0\|m,s,a,p` |
| Chrome 152 | `t13d1517h2_8daaf6152771_cb7bf5808d99` | `1:65536;2:0;4:6291456;6:262144\|15663105\|0\|m,a,s,p` |
| curl 8.18.0 / OpenSSL 3.5.5 | `t13d3013h2_1d37bd780c83_8537cf56674e` | `3:100;4:65536;2:0\|1048510465\|0\|m,s,a,p` |

Two things in that table are worth more than the rest of this document.

**The SETTINGS field of this client's HTTP/2 fingerprint is empty.**
`h2::frame::Settings::for_each` (h2 0.4.19, `src/frame/settings.rs:229`,
**read**) emits a parameter only where the corresponding `Option` is
`Some`, and `H2Opts`' four fields all default to `None` — which
`hclient-native`'s own doc argues for on the grounds that a value set here
goes on the wire and a default of ours would change what a caller who
asked for nothing announces. That argument is right and its consequence
was not measured: **what a caller who asked for nothing announces is
nothing**, and no client anybody fingerprints does that. curl announces
three parameters and Chrome four, both measured here. An empty
SETTINGS frame is not a weak signal, it is a strong one, and this client
acquired it by being careful.

**Its cipher hash cannot be made to equal Chrome's, and that is not a
matter of configuration.** §4.1.

### 1.1 How these numbers were checked, since half of them are this document's own

Some measurements here come from `tls.peet.ws` and some from a local
listener written for this, which reads a ClientHello off a socket and
computes JA3 and JA4 from it. The second needs validating or it is a
program agreeing with itself, so it was validated three ways, and one of
them found a real error:

- the same `curl` was pointed at the local listener and then at
  `tls.peet.ws`. Locally `b` and `c` came out `1d37bd780c83` and
  `8537cf56674e`; the service returned `1d37bd780c83` and `8537cf56674e`;
- the local Chrome 152 capture gives `b = 8daaf6152771`, which is the
  value printed in FoxIO's own worked example;
- the local Chrome `c = cb7bf5808d99` is the service's Chrome `c`.

**The error the first check found is the one §2.2 warns about.** The first
implementation excluded SNI and ALPN from JA4's extension *count*, because
part `c` excludes them from its *list*. The service returned `…1011h2`
against a locally computed `…1009h2`, over a hello that was in hand and
could be counted by eye. The spec says *"Include SNI and ALPN"* in as many
words, and the sentence had been read past. **A local implementation with
no oracle would have published that number**, and every figure in §1 would
have been two short.

---

## 2. What a fingerprint is, field by field

### 2.1 JA3 — order-sensitive, and effectively dead

**Read**, `salesforce/ja3` README and `python/ja3.py`. The string is

```
SSLVersion,Cipher,SSLExtension,EllipticCurve,EllipticCurvePointFormat
```

— the ClientHello's *legacy* version, the cipher list, the extension
**type IDs**, `supported_groups`, `ec_point_formats`, decimal, `-` within
a field and `,` between, MD5 of the whole. GREASE values (RFC 8701) are
filtered out of ciphers, extensions and groups; nothing is sorted, in the
README's words *"in order"*, and there is no sort anywhere in the
reference implementation.

It omits `signature_algorithms`, ALPN, `supported_versions` contents and
`key_share` contents, which is why it discriminates poorly among TLS 1.3
clients. Salesforce archived it; its author now maintains JA4 elsewhere.

### 2.2 JA4 — three parts, and only one list still has an order

**Read**, `FoxIO-LLC/ja4/technical_details/JA4.md`.

```
a: t|q|d  ‖ TLS version (2) ‖ d|i (SNI) ‖ cipher count (2) ‖ ext count (2) ‖ first+last char of first ALPN
b: sha256(cipher hex list, SORTED)[..12]
c: sha256(ext hex list SORTED, minus 0x0000 and 0x0010, "_" sig-alg hex list IN ORDER)[..12]
```

Four details decide what an implementation would have to control, and the
second of them was got wrong here before §1.1's oracle caught it:

- the **version** comes from the highest non-GREASE value in
  `supported_versions`, not from the legacy field JA3 reads;
- the **extension count** includes SNI and ALPN, while the `c` **list**
  excludes them. **Executed**: `hclient` sends eleven extensions,
  `tls.peet.ws` returns `t13d10`**`11`**`h2`, and the `ja4_r` it returns
  beside it lists nine. Counting nine is the natural reading and is
  wrong;
- **ciphers and extensions are sorted**, so their emission order does not
  reach the hash;
- **`signature_algorithms` is not sorted.** It is the one list whose
  *sequence* still matters. An implementation with the right sig-alg set
  in the wrong order gets a different `c`.

`JA4_r` is the same data unhashed and still sorted. The order-sensitive
variants are `JA4_o` / `JA4_ro`, which almost nobody deploys, for the
reason in §3.

**The licence is not uniform and the difference lands exactly where this
project would need it.** The JA4 TLS client fingerprint is BSD-3-Clause
and FoxIO disclaims patent coverage over it. **JA4+ — including JA4H, the
HTTP client fingerprint — is the FoxIO License 1.1, which forbids
monetisation and forbids providing it on a hosted service.** So the TLS
half is free to implement and the HTTP half is not, which matters to a
crate that publishes under `MIT OR Apache-2.0`.

### 2.3 The Akamai HTTP/2 fingerprint

**Read**, Shuster, *Passive Fingerprinting of HTTP/2 Clients*, Akamai,
Black Hat EU 2017. Four components, pipe-separated:

```
SETTINGS(id:value, ";"-joined, IN WIRE ORDER) | WINDOW_UPDATE increment | PRIORITY tuples | pseudo-header order
```

`00` for an absent WINDOW_UPDATE, `0` for absent PRIORITY frames, and the
pseudo-header order written with the letters `m` `a` `s` `p`. Chrome is
`m,a,s,p`; Firefox `m,p,a,s`; Safari `m,s,p,a`.

---

## 3. The first thing that must be got right: Chrome has no extension order

**Executed.** Four consecutive headless Chrome 152 connections to a
listener that reads the ClientHello and nothing else, and five consecutive
`rustls` 0.23.43 connections through the same listener with an unchanged
config:

| client | distinct extension orders | distinct JA3 | distinct JA4 |
|---|---|---|---|
| Chrome 152, 4 connections | **4** | **4** | 1 |
| rustls 0.23.43, 5 connections | **5** | **5** | 1 |

Both randomise. Chrome has done so since **Chrome 110** — `chromestatus`
feature 5124606246518784, *"Randomize the order of TLS ClientHello
extensions, to reduce potential ecosystem brittleness"* — implemented as
`SSL_set_permute_extensions(ssl_.get(), 1)`, called unconditionally in
`net/socket/ssl_client_socket_impl.cc`, and executed in BoringSSL as a
Fisher–Yates shuffle seeded from `RAND_bytes` **per handshake**. rustls
has done so since **0.23.0**, whose release note says *"Extension ordering
in ClientHello messages are now randomised as an anti-fingerprinting
measure. We do not foresee any interoperability issues as Chrome has
already rolled out the same change."*

**So "reproduce Chrome's extension order" is not a requirement that can be
written down.** There is no such order; there is a uniform random
permutation. A Chrome JA3 hash is not a constant, and neither is a
`JA4_o`. This kills a whole class of design: anything whose acceptance
test is "our JA3 equals Chrome's JA3" is testing against a random
variable.

It also inverts a natural assumption about which stack is more
conspicuous. **A client with a *fixed* extension order is more
distinguishable from Chrome than one that shuffles**, because a stable
JA3 where Chrome has none is itself the tell. rustls, by refusing this
feature, arrived at Chrome's behaviour.

Firefox is the opposite case and worth knowing before someone picks a
different target: **read**, NSS gained the capability (Bugzilla 1789436)
but current `mozilla-central` turns it on nowhere — no permutation pref in
`StaticPrefList.yaml`, no `SSL_set_permute_extensions`-equivalent call in
`security/manager/ssl/nsNSSIOLayer.cpp` — so Firefox's order is
deterministic and *is* matchable. Firefox was not captured here; two
attempts to drive it headless against the listener produced no connection,
and the claim is a source reading rather than a measurement.

---

## 4. The options in Rust, measured

### 4.1 rustls — how far the public API gets, exactly

**Read**, `rustls` 0.23.43 (`Cargo.lock`). Every public field of
`ClientConfig` that reaches the ClientHello: `alpn_protocols`,
`enable_sni`, `resumption`, `enable_early_data`, `cert_decompressors`,
`send_ticket_request`, `require_ems`, plus — through `CryptoProvider` —
`cipher_suites`, `kx_groups` and the signature schemes. Extension order is
`pub(crate) order_seed: u16` in `msgs/handshake.rs`, with no setter, no
getter and no way to disable it. The whole `ClientExtensions` struct is
crate-private.

**Executed**, the best a caller can do through that surface — the cipher
list reordered to Chrome's, the groups reordered, Chrome's ALPN — against
what Chrome actually sends:

```
rustls+ring has  9 cipher suites in total.
Chrome offers   15 non-GREASE cipher suites.
Chrome suites rustls+ring CANNOT offer (6): c013 c014 009c 009d 002f 0035
rustls suites Chrome does not offer      (0): —
```

Those six are the CBC and static-RSA suites. rustls does not implement
them, on purpose, and never will. **JA4's `b` is a hash over the sorted
cipher list, so `b` cannot be made to equal Chrome's `8daaf6152771` by any
configuration whatsoever.** Measured, it is `61a7ad8aa9b6` and it stays
`61a7ad8aa9b6`. That single fact settles more of this question than
anything else in the document: the target is not merely hard to reach, it
is out of range, and the distance does not close with effort.

Two smaller measured points in the same direction. rustls appends
`TLS_EMPTY_RENEGOTIATION_INFO_SCSV` (`0x00ff`) to the cipher list, which
Chrome does not send and which rustls#2485 confirms cannot be removed —
so even the *count* is off by one in a direction nothing can fix. And
rustls sends **no GREASE at all**: `SSL_CTX_set_grease_enabled` has no
counterpart, and rustls#357 refused GREASE in 2022 (*"the complexity/value
trade-off is not a good one for rustls"*).

Extensions Chrome sends that this hello does not: `65281`
(renegotiation_info), `27` (compress_certificate), `17613` (ALPS),
`51764`, `65037` (ECH GREASE), `18` (SCT). **Executed**, exactly one of
the six is reachable: setting `cfg.cert_decompressors` puts `27` on the
wire and moves the JA4 to `t13d1012h2_61a7ad8aa9b6_69ed562cf35e` — which
is the point in miniature, because it changes `c` to a *third* value that
is nobody's. And `65037` is not reachable at all on the provider this
workspace uses: **executed**,
`rustls::crypto::ring::hpke::ALL_SUPPORTED_SUITES` is empty, so ECH GREASE
would require moving the whole workspace to `aws-lc-rs`.

### 4.2 rustls' own position: refused on principle, and the codebase moved away from it

The existing refusal says *"rustls closed it not planned"*. **That is
true and it understates the case.** It is not one issue; it is a
seven-year chain — #1125 (2022), #1421 (2023), #1501, #1815, #1857, #1932,
#2414, #2485, #2498 (2025), #3207 (2026) — closed `not planned` or
`duplicate`, with a consistent maintainer position.

The distinction the owner asked for — principle or effort — is
answerable. **cpu**, closing #1421, left an effort-shaped door open:
*"if a contributor was able to propose a change that was easy to review
and not especially invasive I think it would be something we'd be open
to."* A contributor walked through it the next day with PR #1564, a
`ClientHelloCamouflager`, +360/−68 across 12 files, with a working
Chrome-102 example. **ctz** declined it:

> I'm not super keen in general on any feature in the core rustls library
> to make it look more or less like any other TLS library for the purposes
> of evading fingerprinting.

and then went further, in #1932:

> I think the goals of parroting other TLS implementations for the benefit
> of fingerprinting is at odds with other goals we have, such as not
> contributing to the ossification of TLS on the internet. **That is why we
> randomise extension orders.**

**So it is principle, and the principle is anti-ossification rather than
anti-scraping** — nobody in seven years of those threads makes a moral
argument. PR #1475 then deleted the per-extension types and the stored
extension order from rustls' internal representation, which makes the
codebase structurally hostile to the feature rather than merely
uninterested. The maintainers' recommended escape hatch is a fork —
`3andne/craftls` — which **read**: tracks rustls **0.22**, was last pushed
**2024-01-19**, and was never published to crates.io. The recommended
alternative does not currently exist.

The current standing position, ctz on #1932 in January 2026, is worth
having verbatim because it is the only thing that could change:

> From my side, a well-maintained and used fork would be evidence that
> this is a feature valued by a number of people.

### 4.3 BoringSSL — what it costs, executed

`boring` 5.2.0 (Cloudflare) is how the Rust ecosystem does this.
**Executed** on this host:

| measurement | value |
|---|---|
| `cargo tree -e normal`, unique crates | **13** |
| `cargo tree -e normal,build` | **35** — bindgen 0.72, cmake, cc, clang-sys, libloading, regex, nom … |
| vendored BoringSSL source in `boring-sys-5.2.0/deps` | **32 MB** |
| clean release build, wall clock | **58.5 s** |
| peak RSS during that build | **575 MB** |
| resulting `target/` | **422 MB** |
| host tools required | **cmake**, a C/C++ compiler, **libclang** (bindgen) |
| `--target wasm32-unknown-unknown` | **fails** — `wasm-ld: cannot open crt1.o` … in `boring-sys`' build script |
| `--target wasm32-wasip2` | **fails** — `clang: unable to execute command: posix_spawn failed` |

What it buys, **read** in `boring-5.2.0/src/ssl/mod.rs`:
`set_cipher_list`, `set_curves_list`, `set_sigalgs_list`,
`set_alpn_protos`, **`set_grease_enabled`**, **`set_permute_extensions`**
(a bool), `set_verify_algorithm_prefs`.

What it does **not** buy: an exact extension order, `record_size_limit`,
delegated credentials, ALPS. **Read**, all four are zero hits across the
whole of `boring` 5.2.0's `src/`.

### 4.4 The fork that does buy them, and what it weighs

`wreq` 0.16.1 (published three days before this was written) is the live
Rust impersonation client, and its TLS stack is `btls`/`btls-sys`/
`tokio-btls` — one maintainer's fork of `boring`. **Read**, `btls`
0.5.6 has `set_extension_permutation(&[ExtensionType])`,
`set_record_size_limit`, `set_delegated_credentials`,
`add_application_settings`, `set_alps_use_new_codepoint`; and
`btls-sys` 0.5.6 carries **five patches to BoringSSL totalling 2,304
lines**, of which `boringssl.patch` alone is **1,477**, over a 29 MB
vendored C tree. Those patches *add the C functions* —
`SSL_CTX_set_extension_order`, `SSL_CTX_set_record_size_limit`,
`SSL_CTX_set_delegated_credentials` are not upstream BoringSSL.

**That is the real number.** Exact ClientHello control in Rust today costs
a patched fork of a 32 MB C library, and the patch is against a codebase
Google rebases continuously.

### 4.5 A pure-Rust uTLS port: there is not one

`refraction-networking/utls` and `bogdanfinn/tls-client` are Go. **Read**,
crates.io: `rustls-utls` and `tls-client` return *does not exist*; `utls`
is a yanked, unrelated utility crate by a different author. `craftls` is
§4.2's abandoned fork and is not published. **There is no pure-Rust uTLS.**

### 4.6 Download counts, and this workspace's own instrument

AGENTS.md kills candidates on ecosystem demand — `rvcr` at 437 was refused
on exactly this, and it reads **438** today, which is how much this
instrument drifts and therefore how much weight a single figure carries.
The same instrument, run for this document (crates.io's
`recent_downloads`, which is a **90-day** figure; the numbers in AGENTS.md's
ecosystem table are the same field, where they are labelled *per month*):

| crate | recent downloads | |
|---|---|---|
| `boring` | **1,078,033** | Cloudflare's BoringSSL bindings |
| `http2` | **734,836** | the `h2` fork, one maintainer |
| `wreq` | **657,611** | the impersonating client |
| `wreq-util` | 628,738 | its 100+ browser profiles |
| `tokio-boring` | 619,025 | |
| `btls` | 156,849 | the patched BoringSSL fork |
| `rquest` | *yanked* | renamed to `wreq` |
| `utls` (unrelated crate) | 31 | |
| **`rvcr`, for scale** | **438** | the thing this workspace refused |

**This does not die the way `rvcr` did, and saying otherwise would be
dishonest.** `wreq` at 657,611 sits between `reqwest-websocket` (564,728)
and `reqwest-retry` (7,657,595) in AGENTS.md's own table — that is, above
the cookie store and the WebSocket bolt-on, both of which are in the box
here. The demand is real. §7 is why that does not decide it.

---

## 5. The header half, which is not TLS — and the second leg is half wrong

The refusal's second ground is: *`http::HeaderMap` lowercases names, so
browser header casing is unreproducible anyway*. **Executed**, `http`
1.5.0:

```
HeaderName::from_bytes(b"User-Agent").as_str() == "user-agent"
HeaderName::from_bytes(b"DNT").as_str()        == "dnt"
```

and no method — `as_str`, `AsRef<[u8]>`, `Debug` — returns the original
bytes. **The casing claim is true.** But three things about it were never
established.

**One: on HTTP/2 and HTTP/3 it is not a limitation, it is the
specification.** RFC 9113 §8.2.1 requires field names to be lowercase.
Chrome's own h2 request, **executed** and read back from `tls.peet.ws`,
carries `sec-ch-ua`, `user-agent`, `accept-encoding` — all lowercase,
because they must be. A fingerprint that reads header case reads it on
HTTP/1.1 only, and the connection this document measured against a
fingerprinting service negotiated h2 in every arm — `hclient`'s, Chrome's
and curl's alike.

**Two: hyper already offers the h1 half, in the weak form.** **Read**,
`hyper` 1.11.1's public `client::conn::http1::Builder` has
`title_case_headers(bool)` and `preserve_header_case(bool)` — the second
of which documents its own uselessness for this purpose: *"Since the
relevant extension is still private, there is no way to interact with the
original cases. The only effect this can have now is to forward the cases
in a proxy-like fashion."* So a caller can get *all* headers Title-Cased,
which is not the same as getting Chrome's mixed set, and cannot get
anything else. `preserve_header_order` exists and is behind the `ffi`
feature.

**Three, and this is the part the refusal got wrong: header *order* is
not lost, and order is what the h2 fingerprint reads.** **Executed**,
`http` 1.5.0's `HeaderMap` iterates in insertion order — fifteen headers
in a browser-shaped sequence came back in that sequence, and forty headers
survived the capacity growth that would have rehashed them. `remove` then
`insert` moves the entry to the end, which is a fact a redirect hop or an
auth retry can reach, and is worth knowing rather than a defect. `h2`
0.4.19 encodes fields by iterating exactly that map
(`frame/headers.rs`'s `Iter`), so **the field order a caller builds is the
field order on the wire.**

### 5.1 The HTTP/2 fingerprint is three-quarters reachable with no fork at all

This is the sharpest measurement in the document.

**Executed.** A scratch program using upstream `h2` 0.4.19 and
`tokio-rustls`, with five `client::Builder` calls chosen to match the
Chrome 152 SETTINGS measured in §1, fetching `tls.peet.ws`:

```rust
b.header_table_size(65536)            // 1:65536
 .enable_push(false)                  // 2:0
 .initial_window_size(6291456)        // 4:6291456
 .max_header_list_size(262144)        // 6:262144
 .initial_connection_window_size(15 * 1024 * 1024);   // WINDOW_UPDATE 15663105
```

What came back:

```
ours   : 1:65536;2:0;4:6291456;6:262144|15663105|0|m,s,a,p
Chrome : 1:65536;2:0;4:6291456;6:262144|15663105|0|m,a,s,p
```

**Identical in three of four components.** The SETTINGS parameters, their
values and their wire order match exactly. That the *order* agrees is luck
rather than design and is worth saying so: `h2` emits in ascending
identifier order (`Settings::for_each`, **read**), and Chrome's frame —
**executed**, read back from the same service's `sent_frames` — is
`HEADER_TABLE_SIZE, ENABLE_PUSH, INITIAL_WINDOW_SIZE,
MAX_HEADER_LIST_SIZE`, which is 1, 2, 4, 6, also ascending. Two
implementations that both sort agree; a target that did not sort would be
out of reach here for the same reason the pseudo-headers are. The
connection WINDOW_UPDATE matches. Neither sends PRIORITY.

The one component that differs is the pseudo-header order, and it is
**one hardcoded `if let` chain**: `h2`'s `frame::headers::Iter::next`
yields method, scheme, authority, path in that fixed sequence, so `h2`
emits `m,s,a,p` — nghttp2's order, which is why curl matches it, and
which is *not* Chrome's.

So on the HTTP side the gap between this workspace and a browser is: two
`Option` fields `H2Opts` does not have (`header_table_size` and
`enable_push` — it has the other four), and one upstream `h2` change
nobody has asked for.

---

## 6. Whether the browser story survives

`hclient`'s central claim is that the same application code builds for
native, WASI and the browser. A fingerprint-controlling backend is
native-only by construction. Does that damage the claim?

**No, and the reason is sharper than "so is `hclient-tls-native-tls`".**

`hclient-tls-native-tls` is native-only because a platform TLS stack is a
fact about a platform. A fingerprint backend is native-only for a
different reason, and the difference is the interesting one: **on the two
ambient backends this workspace does not perform the handshake at all.**
`hclient-fetch` hands the request to the browser; `hclient-wasi` hands it
to the host's `wasi:http` implementation. The fingerprint those
connections carry belongs to Chrome and to wasmtime respectively, and it
is *already* a browser's — a browser build has the feature by definition
and has no way to want it. So the seam does not have to express anything
new, and nothing is `Unsupported` anywhere: the question does not arise on
the two targets that cannot answer it.

That is the `WebSocketConnect` argument arriving from the other side. The
WebSocket framing became its own trait because "hand me the socket after
the 101" is answerable by one backend of four. Here, "shape my
ClientHello" is answerable by exactly the backends that *own* a
ClientHello, and it happens to already be a per-connector property of
`TlsConnect` implementations rather than a method on any trait.

**Concretely: `TlsConnect` needs no change.** A fingerprint profile is a
property of the connector, exactly as `Rustls::from_config`'s
`ClientConfig` is, and `TlsConfigId::new_unique()` already keeps two
differently-shaped connectors from sharing a pooled socket — the same
mechanism the mTLS work leans on for identity isolation, working here for
free. Adding a field to `TlsRequest` would be the wrong shape anyway: a
fingerprint is a claim about who the client *is*, which is not a
per-request fact.

**One consequence to state before someone leans on it.** The QUIC seam
does not follow. `QuicTlsConnect::quic_client_config` returns
`Arc<dyn quinn_proto::crypto::ClientConfig>`, and **read**, `quinn-proto`
0.11.17 ships exactly one implementation of that trait, in
`src/crypto/rustls.rs`. There is no BoringSSL path. So a client that was
Chrome-shaped over TCP would be rustls-shaped over QUIC or absent from
QUIC entirely — and Chrome reaches a large fraction of the web over
HTTP/3. That is a second mismatch, in the same family as §7's.

---

## 7. What it is for, and why a partial answer is worth less than nothing

Three uses, and they are not the same feature.

**Scraping past bot detection.** The dominant use, and the one the
download numbers in §4.6 measure. What it needs is *the whole thing*: a
fingerprint that is Chrome's, on a client whose `User-Agent` says Chrome.

**A compatibility shim.** Some WAFs reject unfamiliar clients outright.
`wreq`'s own README names a concrete instance — *"some WAFs … reject HTTP/1
requests with lowercase headers"* — and points at a `reqwest` discussion
thread for it. What this use needs is much less than the first: *any*
ordinary fingerprint, not a specific browser's. §1's empty SETTINGS frame
is precisely the kind of thing that trips such a filter, and a caller
meeting it has no lever here today.

**Security research.** Needs the knobs and no profiles at all: the point
is to send a hello the researcher specifies.

**These three want different things, and only the first wants
impersonation.** That matters because the first is the one that fails
badly when done partially.

**The uncanny valley is not folklore, and there is a citation.** Akamai
patent **US 11,184,390 B2**, *Bot detection in an edge network using
transport layer security (TLS) fingerprint*, describes the mechanism in
its own words:

> …based on the principle that good browsers (such as Chrome, Firefox,
> Safari, and the like) have a few valid combinations of TLS fingerprints
> for each browser version. The "known" or "correct" combinations are
> learned a-priori… **A bot script masquerading its user-agent as one of
> the well-known browsers is then caught by checking for the existence of
> the user-agent and the TLS fingerprint in the "known/correct" table.**

Akamai's own 2017 white paper says the same about the HTTP/2 layer:
*"HTTP/2 fingerprint… may be leveraged to detect clients that spoof their
User-Agent string."*

**Now put §4.1 and §5.1 next to that.** Measured, with no forks, this
workspace could reach three of four components of Chrome's Akamai
fingerprint, and could never reach Chrome's JA4 `b`. That is not
"progress towards impersonation"; it is **precisely the contradictory
pair the patent describes** — a browser-shaped HTTP/2 layer under a TLS
layer that is not any browser's, on a request whose `User-Agent` would
have to claim one. A client that sent an honest fingerprint would be
classified as an honest client of an unknown kind; a client that sent
three-quarters of Chrome's would be classified as a liar. **The partial
answer is worse than the absence.**

Two arXiv results point the same way and are worth naming even though
neither measures this exact join: *FP-Inconsistent* (2406.07647) shows
cross-attribute inconsistency rules cutting evasive-bot success against
DataDome and BotD by 48% and 45%; *Unmasking Web Agents with Multi-Layer
Fingerprinting* (2606.30119) reports that *"stealth and anti-detection
mechanisms often increase detectability rather than decrease it."*

---

## 8. The maintenance cost, under this workspace's own rule

AGENTS.md refuses an MSRV job, and the reason is general:

> A job checking a fixed version would be a second statement of the same
> promise, staler than the first, and it is the one people would trust:
> the moment stable moves past the pin, that job goes on passing while
> checking a toolchain nobody supports.

**Apply it here and it comes out harder than it does for MSRV, on three
counts.**

*The cadence is somebody else's and it is fast.* The drift is visible
between two observations of the same browser made inside this document.
The Akamai paper's Chrome row (2017, **read**) is
`1:65536;3:1000;4:6291456|15663105|0`; Chrome 152 (**executed**) is
`1:65536;2:0;4:6291456;6:262144|15663105|0`. `3:1000` left, `2:0` and
`6:262144` arrived. On the TLS side the same run shows Chrome 152 leading
`supported_groups` with `0x11ec` — a hybrid post-quantum group that
`rustls`+`ring` does not have at all, and that no profile written before
it existed would carry. `wreq-util` is a separate crate holding 100+
profiles precisely because the profiles move faster than the client.

*The failure is silent.* A stale MSRV job passes while checking the wrong
toolchain — bad, and visible to anyone who looks. A stale
`ChromeProfile::V152` **keeps working**: the handshake completes, the
response arrives, and the only symptom is that a fingerprinting service
somewhere classifies the caller as a liar. There is no local check that
can fail, because the oracle is a third party's private model. This is
the *check that cannot fail* defect this workspace has found repeatedly —
`test-doc`, `test-no-default`, the rendered docs, `hc`'s backend refusal —
in its worst form: a check that cannot even be written.

*The claim would be about somebody else's product.* AGENTS.md's rule —
*a claim about a third party is exactly as perishable as the check behind
it* — has been met from four directions in this repository already. A
profile named `chrome` is a claim about Chrome, in a published crate,
under a version number that promises not to break.

**And the honest way to escape all three is the one this document ends
with: ship the knobs and no profiles.** Then the staleness is the
caller's, who can see their own traffic being blocked, rather than ours,
who cannot.

---

## 9. The verdict

**Do not build it. Build a narrower thing, and it is not the narrower
thing the question anticipated.**

The refusal in `docs/competitive-gaps.md` stands, and both of its stated
grounds need amending:

- *"rustls closed it not planned"* — **stands, and understates.** It is
  ten issues over seven years, one working PR declined, a refusal on
  anti-ossification principle, and a codebase change (#1475) that removed
  the internal representation the feature would need. The escape hatch
  the maintainers recommend, `craftls`, is dead.
- *"`http::HeaderMap` lowercases names, so browser header casing is
  unreproducible anyway"* — **half wrong, and the half that is right is
  the less important half.** Casing is indeed unreachable, and it is
  meaningless on h2/h3, where lowercase is mandatory and where the servers
  that fingerprint live. Header **order** — which is what the Akamai
  fingerprint actually reads — is preserved by `HeaderMap` and is fully
  controllable today. The leg should be struck and replaced.

**What replaces them is a third reason neither named, and it is the one
that decides it.** rustls+ring physically lacks six of Chrome's fifteen
cipher suites, so **JA4's `b` component cannot be made equal to Chrome's
by any configuration** — measured, `61a7ad8aa9b6` against `8daaf6152771`.
Reaching it means BoringSSL; reaching an exact extension order means a
patched fork of BoringSSL (2,304 lines of patch over 32 MB of vendored C,
plus cmake and libclang, plus both wasm targets failing); reaching the
pseudo-header order means forking `h2`. That is what all three parties who
have done this in Rust actually did — `rquest` forked four crates, `wreq`
maintains eight, `impit` maintains four and consequently **cannot publish
to crates.io at all**, because a `[patch]` section forbids it. This
workspace publishes 25 crates and has an explicit policy of publishing all
of them on every release; `[patch]` is not an available answer here.

Against that, §7: the partial answer that *would* be affordable is the
one the Akamai patent is built to catch.

**The demand is real and this is not `rvcr`.** `wreq` at 657,611 and
`boring` at 1,078,033 are not rounding errors, and a document that killed
this on download counts would be using the instrument dishonestly. What
kills it is that the demand is already met by a maintained crate, at a
cost this workspace would be duplicating rather than reducing.

**And the refusal costs a third party nothing, which is the payoff of the
seam.** `TlsConnect` is public, `TlsInfo` is `#[non_exhaustive]` with
chained setters *specifically so that backends outside this workspace can
build one*, `TlsConfigId::new_unique()` is public, and `Native<R, T, D>`
takes any `T: TlsConnect`. So `hclient-tls-boring` can be written by
somebody else, in their own crate, against the published surface, with no
change to any crate here. That is not a consolation; it is the design
working. It is also the honest reading of ctz's standing position in
§4.2 — *a well-maintained and used fork would be evidence* — applied one
layer up: if this feature has an owner, the seam already lets them own it.

### 9.1 The narrower thing to actually do

Not "expose the ClientHello and ship no profiles" — that option does not
exist without BoringSSL, because rustls exposes nothing to expose. What
exists is smaller, cheaper and better argued on its own merits:

**Give `H2Opts` its two missing fields: `header_table_size` and
`enable_push`.** Both are plain `Option`s forwarded to
`h2::client::Builder`, which already has them; both follow `H2Opts`' own
rule that `None` means *whatever `h2` chooses*; neither costs a
dependency, a feature or a capability. What they buy is that the SETTINGS
half of this client's HTTP/2 fingerprint becomes **expressible at all**,
where today it is empty and unreachable.

The argument for them is not impersonation. It is that **an empty SETTINGS
frame is the most distinctive thing this client does, and it acquired it
by accident of a good default.** `hclient-native`'s own doc is right that
a value set here goes on the wire and that a default of ours would change
what a silent caller announces; what was not noticed is that announcing
nothing is itself a choice, and it is the one choice no other HTTP/2
client makes. A caller who is being rejected by a WAF — §7's second use,
the legitimate one — has no way to fix it today and would have one for
two `Option`s.

`enable_push` needs its own defence, because AGENTS.md already refused
`max_concurrent_streams` as *a knob with no subject*. The subject here is
different: `max_concurrent_streams` governs behaviour nothing exercises,
where `SETTINGS_ENABLE_PUSH` governs **a byte pair on the wire**, and the
whole point of setting it is that it is observed. Chrome sends `2:0`;
`h2` sends nothing. That is the subject.

Two things deliberately **not** in the narrow version:

- **No test pinning the fingerprint.** A test asserting
  `ja4 == "t13d1011h2_…"` would be a check pinned to a value that upstream
  rustls can move without breaking anything — the mirror of AGENTS.md's
  rule against a CI check on dependency-graph crate counts, which *"would
  fail for an upstream release that broke nothing here"*. The numbers in
  §1 are colour, and belong in a document rather than in CI.
- **No `Capabilities` field.** There is nothing for a client-level setting
  to gate: a fingerprint is configured on the transport, which is the
  object that answers the question, so it is a *report* in the sense
  AGENTS.md's gate/report split defines — and `proxy` is already the
  worked example of a report that will never be a gate.
- **No JA4 calculator, and it is blocked rather than declined.** The
  obvious adjacent nicety is a helper that tells a caller what fingerprint
  their own client just sent, and the JA4 TLS licence permits writing one
  (§2.2). It cannot be written here: a JA4 needs the ClientHello's bytes,
  and rustls hands its client none — nothing on `ClientConfig` or
  `ClientConnection` returns them, and the nearest request, rustls#3219
  (*"Expose original ClientHello bytes"*), is about the **server** side
  and was closed unmerged on 2026-08-28. Everything in §1 was measured by
  putting a listener in front of the client or by asking a third party,
  and neither is something a library can do for its caller. A helper that
  computed a JA4 from *what we believe we configured* rather than from
  what went out would be this file's *capability that lies*, in the one
  place where the whole point is to know what actually left.

### 9.2 What would have to change for the answer to become yes

Written down so the question can be re-answered cheaply rather than
re-researched, and so that a "yes" has to name which of these moved:

1. **rustls acquires the surface.** Unlikely on the record in §4.2, and
   ctz has named the one thing that would count as evidence: a
   well-maintained, used fork. If `craftls` or a successor becomes that,
   the ground shifts.
2. **A maintained pure-Rust ClientHello-shaping crate appears** that does
   not require patching C. Today there is none; §4.5.
3. **The cipher-suite floor moves.** rustls implementing the CBC and
   static-RSA suites would be required for `b` to be reachable, and rustls
   will not do that for good reasons.
4. **`h2` makes the pseudo-header order configurable.** One `if let`
   chain. This is the cheapest of the five and the least useful alone.
5. **Somebody writes `hclient-tls-boring` outside this workspace and it
   gains users.** Nothing here has to change for that to happen, which is
   §9's point; and if it does happen, the question of adopting it becomes
   an ordinary dependency question rather than this one.

None of those is in this workspace's control, and that is the last
argument: **the thing being asked for is not a design this project could
get right — it is a maintenance obligation against a target Google moves
every four weeks, whose failure mode is silent, and whose correctness this
repository has no way to check.** That is a sentence AGENTS.md has already
written three times, about three other things, and it has been right every
time.
