# Revocation and Certificate Transparency: a design

Two additions to what this client is willing to believe about a server's
certificate: **CRL-based revocation checking**, and **Certificate
Transparency**. They are treated in one document because they arrive at the
same seam — rustls' `ServerCertVerifier` — and because they end in opposite
places: one is a design to build, the other is a measured refusal.

Nothing here is implemented. What is measured is marked as measured, and
where a claim is load-bearing it was **executed** rather than read: this
workspace has been wrong often enough about third-party code it only read.
Versions are the ones in this tree's `Cargo.lock` — `rustls` 0.23.43,
`rustls-webpki` 0.103.15, `rustls-pki-types` 1.15.1,
`rustls-platform-verifier` 0.7.0.

## 1. OCSP is out, and the interesting part is what that promotes

The owner's ruling is that OCSP is deprecated. It is, and the evidence is
worth writing down once, because the consequence is not *fall back to CRLs*
— it is that **CRLs stopped being a fallback and became the mechanism**.

**The CA/Browser Forum inverted the polarity in 2023.** Ballot SC-063v4,
*Make OCSP Optional, Require CRLs, and Incentivize Automation*, passed on
2023-07-13 (28–1 among issuers, 3–0 among consumers) and took effect
**2024-03-15**. Read from the ballot's own redline rather than from the
summary: the subscriber-certificate profile row for `id-ad-ocsp` changes
`MUST` → `MAY`, §4.9.7 loses its `(if applicable)` and gains the
effective-date row *"CAs MUST generate and publish CRLs"*, and §4.9.9/§4.9.10
are re-prefixed so that the OCSP rules apply only to a CA that chose to have
OCSP. Before that date OCSP was mandatory and CRLs were optional. After it,
exactly the reverse.

**The largest CA executed on it.** Let's Encrypt announced the intent on
2024-07-23 and the timeline on 2024-12-05: OCSP URLs left the certificates
on **2025-05-07**, the responders were switched off on **2025-08-06**, and
the closing post says *"Going forward, we will publish revocation
information exclusively via Certificate Revocation Lists (CRLs)."* Google
Trust Services announced the same in April 2025 and has since moved its date
to 2026-05-01.

**No major browser does an online OCSP lookup.** Chrome stopped for DV and
OV years ago and for EV in **Chrome 106** (announced 2022-08-24); the current
Chromium security FAQ says *"Chrome clients do not, by default, perform
'online' certificate revocation status checks using CRLs directly or via OCSP
URLs included in certificates."* Firefox shipped CRLite by default in
**137** (2025-04-01), and Mozilla's own announcement says it *"will be
disabling OCSP for domain validated certificates in Firefox 142"* — stated as
intent in August 2025, and **not confirmed here** against the 142 release
notes, which do not mention it.

**The reason given by all three is privacy, and it is the same reason.**
Let's Encrypt: *"the Certificate Authority operating the OCSP responder
immediately becomes aware of which website is being visited from that
visitor's particular IP address."* Mozilla puts the sharper version, because
OCSP rides plaintext HTTP: *"this information is also leaked to all on-path
observers."*

**And there is a cost number worth carrying into §3.** Mozilla measured
*"OCSP requests block the TLS handshake for 100 ms at the median."* That is
the ceiling any in-handshake revocation fetch has to beat, and §3 measures
what a CRL fetch actually costs against it.

### 1.1 The one OCSP shape with no privacy cost is also the one going away

Stapling is not the same object as an online lookup: the *server* fetches
the response and hands it over in the handshake, so the responder learns
nothing about the client and nothing blocks. And it is **reachable from
here**, which is a fact about the seam rather than a hope —
`ServerCertVerifier::verify_server_cert` takes `ocsp_response: &[u8]` as its
fourth parameter, in this exact version. Chrome still honours stapled
responses today.

It is refused anyway, and not because it is hard. A stapled response only
exists if the CA still runs a responder and the server still asks; both of
this workspace's own example origins are already past that — Let's Encrypt
has no responder at all, and Google Trust Services is removing the AIA
pointer. Building a parser for a field that is being emptied is building
towards zero. The narrower signal is Chrome's: from **Chrome 148**
(2026-05-05) SCTs delivered inside a stapled OCSP response stop counting
towards its CT policy, with the stated reasoning *"we expect support for
OCSP to only diminish further with time."*

So: no OCSP, in any of its three shapes, and the design below is a CRL
design because there is nothing else left to be.

## 2. What was measured about rustls

Five things, each of which removes an option.

### 2.1 The verifier is synchronous, so a fetch inside a handshake is not a policy choice

```rust
fn verify_server_cert(
    &self,
    end_entity: &CertificateDer<'_>,
    intermediates: &[CertificateDer<'_>],
    server_name: &ServerName<'_>,
    ocsp_response: &[u8],
    now: UnixTime,
) -> Result<ServerCertVerified, Error>;
```

`rustls-0.23.43/src/verify.rs:83`. No `async`, no `Poll`, no way to return
*not yet*. It is called from inside `process_new_packets`, which
`hclient-tls-rustls`'s `TlsStream` drives from a `poll`. **There is no point
in the handshake at which this client could await an HTTP request**, and
blocking the thread inside it would block the reactor.

That is not a constraint to design around; it is the whole shape of the
answer. Whatever CRLs the verifier consults, it must already have them.

### 2.2 CRL support is rustls' own, and it is a property of a verifier built once

`ServerCertVerifierBuilder::with_crls(impl IntoIterator<Item =
CertificateRevocationListDer<'static>>)`, plus
`only_check_end_entity_revocation()`, `allow_unknown_revocation_status()` and
`enforce_revocation_expiration()`
(`rustls-0.23.43/src/webpki/server_verifier.rs:52,69,82,95`). They forward to
`webpki::RevocationOptionsBuilder`, whose defaults are
`RevocationCheckDepth::Chain`, `UnknownStatusPolicy::Deny`,
`ExpirationPolicy::Ignore` (`rustls-webpki-0.103.15/src/crl/mod.rs:62-68`).

So **parsing, signature-checking and serial lookup are free** — nothing is
ours. `OwnedCertRevocationList::from_der` is public, and it refuses what
webpki does not support, by name: CRL versions other than 2, CRLs missing
`nextUpdate`, delta CRLs, CRLs over 4 GiB.

What is *not* free is that `WebPkiServerVerifier` stores
`crls: Vec<CertRevocationList<'static>>` as a field, built at
`build()`. The set is fixed for the verifier's life, the verifier is fixed
for the `ClientConfig`'s life, and `Rustls` holds an `Arc<ClientConfig>`. A
CRL that arrives later has nowhere to go.

### 2.3 The revocation check is not reachable on its own

The obvious repair for §2.2 — keep the platform verifier for *trust* and run
a revocation pass beside it — does not exist. `RevocationOptions` is public;
`RevocationOptions::check` is `pub(crate)`
(`rustls-webpki-0.103.15/src/crl/mod.rs:115`) and takes a `PathNode`, an
internal type produced only by `verify_cert`. There is no public
*is-this-chain-revoked* function anywhere in `rustls-webpki` 0.103.15.

Revocation is therefore reachable only *through* a full `WebPkiServerVerifier`
chain build, which decides §4.1.

### 2.4 The platform verifier already checks revocation on two platforms and not on the third

`Client::new()` builds `Rustls::with_platform_verifier()`
(`crates/hclient/src/client.rs:1844`), so this is what the default client does
today. Read out of `rustls-platform-verifier` 0.7.0's own README:

| OS | verification | revocation |
|---|---|---|
| Windows | CryptoAPI | yes |
| macOS / iOS | Security.framework | yes |
| Android | Trust Manager | sometimes (API ≥ 24) |
| Linux | webpki | **no** |
| WASM | webpki | **no** |

And its footnote names the gap in the words this design would otherwise have
had to invent: *"The fall-back webpki verifier configured for Linux/WASM does
not support providing CRLs for revocation checking. If you require revocation
checking on these platforms, prefer constructing your own
`WebPkiServerVerifier`, providing necessary CRLs."*

Confirmed in the source rather than taken from the table:
`verification/others.rs` builds `WebPkiServerVerifier::builder_with_provider(..)
.build()` with no `with_crls` anywhere in the crate, and `verification/windows.rs`
passes `CERT_CHAIN_REVOCATION_CHECK_END_CERT`,
`CERT_CHAIN_REVOCATION_ACCUMULATIVE_TIMEOUT` and `CERT_CHAIN_CACHE_END_CERT`
with a 10-second URL retrieval timeout — **and**
`CERT_CHAIN_POLICY_IGNORE_ALL_REV_UNKNOWN_FLAGS`, under a comment reading
*"Ignore any errors when trying to obtain OCSP revocation information. This is
also done in OpenSSL, Secure Transport from Apple, etc."*

Two things follow. The hole this design fills is **Linux-shaped**, which is
this project's CI, its containers and its servers. And where the platform
does check, it **fails open** — so "every real client fudges the unknown
case" (§5) is not a characterisation of other people's code, it is a
description of what `Client::new()` does on Windows today.

### 2.5 The certificate does not tell you where its CRL is — twice over

`Cert::crl_distribution_points()` is `pub(crate)`
(`rustls-webpki-0.103.15/src/cert.rs:244`), and `CrlDistributionPoint` is
`pub(crate)` beside it. So a fetcher cannot ask webpki for the URL; reading
the extension is ours, either as forty lines of DER walking or as a
dependency (§9).

The second half is worse and is a fact about the ecosystem rather than about
webpki. Measured on 2026-08-30, from live handshakes:

| origin | CRL distribution point |
|---|---|
| example.com | `http://c.cf-i.ssl.com/ae80…c3a208.crl` |
| letsencrypt.org | `http://ye2.c.lencr.org/58.crl` |
| google.com | `http://c.pki.goog/wr2/oBFYYahzgVI.crl` |
| **github.com** | **none — the certificate has no `cRLDistributionPoints` extension at all** |

github.com's leaf (Sectigo Public Server Authentication CA DV E36) carries an
`OCSP - URI:` and nothing else. SC-063 obliges the *CA* to publish CRLs; it
does not oblige the *certificate* to point at one. A design that treats "no
CRL" as "cannot verify, therefore refuse" makes github.com unreachable from
this client, which is the single measurement that decides §5.

## 3. What a CRL costs, measured on real ones

Fetched and parsed on 2026-08-30. Parse timings are
`OwnedCertRevocationList::from_der` in a `--release` build; peak RSS is
`/usr/bin/time -v` on a process that does nothing else.

| CRL | bytes | entries | fetch | parse | peak RSS | thisUpdate → nextUpdate | IDP |
|---|---|---|---|---|---|---|---|
| ssl.com, example.com's issuer | **45,477,083** | **928,098** | 3.94 s | 1.1–1.3 s | **295 MB** | 7 days | none |
| Let's Encrypt `ye2` shard 58 | 58,562 | 1,501 | 0.51 s | 282 µs | not measured | 9 days | critical |
| Google Trust Services `wr2` | 48,046 | 1,339 | 0.41 s | 202 µs | not measured | 10 days | critical |

Three readings.

**The in-handshake fetch is dead on arrival, by a factor of forty.** Mozilla's
100 ms median for an OCSP lookup is the bar; the *best* of these three is
410 ms and the one belonging to the origin in this workspace's own README
examples is **3.94 seconds and 45 MB**, before 1.2 seconds of parsing and
295 MB of resident memory. This is not a matter of timeouts or of doing it
concurrently. §2.1 already made it impossible; §3 makes it undesirable
independently, which is the useful redundancy — if rustls grew an async
verifier tomorrow, the answer would not change.

**Sharding is what makes the small numbers small.** Let's Encrypt's CDP names
shard 58 of its `ye2` issuer and its CRL carries a critical Issuing
Distribution Point; Google's does the same. ssl.com's carries no IDP, which
`webpki::CertRevocationList::authoritative` reads as *scope: everything*
(`crl/types.rs:112-116`) — one 45 MB list for the whole issuer. A client
holding CRLs per issuer is holding whatever shape the CA chose, and one CA in
three chose 45 MB.

**`nextUpdate` is given, on all three, at 7–10 days.** §4.4.

## 4. The design

### 4.1 Where it attaches: a constructor on `Rustls`, and it replaces the trust store rather than adding to it

`Rustls::with_revocation(roots, source) -> Rustls`, beside
`with_webpki_roots` and `with_platform_verifier`, behind a `revocation`
feature.

It holds a `ServerCertVerifier` of ours whose whole body is a delegation:

```rust
struct Revoking {
    current: ArcSwap<WebPkiServerVerifier>,   // or RwLock<Arc<..>>
    roots:   Arc<RootCertStore>,
}
```

`verify_server_cert` loads the current verifier and forwards; so do
`verify_tls12_signature`, `verify_tls13_signature` and
`supported_verify_schemes`. Refreshing rebuilds a `WebPkiServerVerifier` from
the same roots and the new CRL set and swaps it in. The read on the handshake
path is one atomic load.

**This buys the whole of §2.2 and §2.3 for zero new crates and no
cryptography of ours.** Parsing, CRL signature verification, the `cRLSign`
key-usage check, IDP scoping and the serial lookup all stay rustls'. What is
ours is a pointer swap and a fetch loop.

**The cost is stated where a caller meets it, because it is the reason this
is not a flag on `with_platform_verifier`.** `WebPkiServerVerifier` needs a
`RootCertStore`, so on Windows and macOS this **replaces** CryptoAPI and
Security.framework with webpki: the caller gains CRLs of their own choosing
and loses the OS's CA constraints, its enterprise policy and its own
revocation checking. On Linux it is a strict improvement, because the
platform verifier is already a `WebPkiServerVerifier` over
`rustls_native_certs::load_native_certs()` with the CRL slot left empty
(§2.4) — so `Rustls::with_revocation(native_certs(), ..)` is what the
platform verifier does, plus revocation.

There is no third option. §2.3 measured that a revocation-only pass beside
the platform verifier is unreachable, and running *two* full chain builds per
handshake to keep both is a cost and a divergence — two verifiers can
disagree about which chain they built, and then the CRL was checked against a
path the connection did not use.

### 4.2 `CrlSource`, and why the one that ships first fetches nothing

```rust
pub trait CrlSource {
    /// Every CRL this source can offer right now, DER.
    fn crls(&self) -> Vec<CertificateRevocationListDer<'static>>;
}
```

Deliberately not `async`, and deliberately not a fetch: it is asked on the
refresh path, which owns the network, and it hands back bytes. Two
implementations:

**`Preloaded`** — CRLs the caller supplies, from a file, a
configuration-management drop, a sidecar that already talks to CCADB. No
HTTP, no bootstrap, no clock. It is the one that ships first, and the
argument is this workspace's own: a component with no consumer is a
component with no evidence, and `Preloaded` is what makes the verifier, the
swap, the three-state outcome of §5 and the error of §6 all testable with no
socket at all. Everything below it is a second commit.

**`Fetched<C>`** — a source that reads CRL distribution points out of the
chain it is verifying and fetches them.

### 4.3 The bootstrap, and why it is not DoH's

The obvious reading is that this is `Doh` again: checking revocation needs
an HTTP client, and the HTTP client is us. Half of that transfers and half
does not.

**What transfers is the rule about *which* client.** `hclient-dns-doh`'s
module doc states it and the type system enforces it: what makes the request
is a `Transport`, **never an `hclient::Client`**, because a cookie jar, a
redirect policy and an `Authorization` header belong to `Client` and have no
business on a housekeeping fetch. `Fetched<C: Transport>` is the same
declaration, with the same consequence — no `total` bound, because that is
`Client`'s — and the same sentence one noun over: *a revocation checker's
client is not the user's client.*

**What does not transfer is the recursion, and that is a measurement rather
than a hope.** `Doh::pinned` / `Doh::bootstrapped` exist because resolving
the DoH server's name needs a resolver. Here the analogue would be
*verifying the CRL server's certificate needs a verifier* — and it does not,
because **a CRL is signed by the issuing CA and fetched over plaintext
HTTP**. All three distribution points measured in §2.5 are `http://`. The
integrity of the answer comes from the CA's signature over the CRL, which
webpki checks against the issuer's SPKI, not from the transport.

So the constructor pair `Doh` needed is not needed here. What is needed is a
**refusal**, in the shape this workspace uses for every other ambiguity a
caller cannot see: an `https://` distribution point is refused, naming it,
rather than followed. Following one is how a CRL fetch ends up needing the
verifier that is waiting for the CRL, and the failure is a deadlock or an
infinite regress rather than an error. The same refusal covers a redirect to
`https://`, which means `Fetched` sets no redirect policy at all and treats a
`3xx` as the answer it is — which it gets for free, because a `Transport`
follows nothing.

### 4.4 The cache lifetime is given, and that is the entire difference

This workspace refuses to invent a lifetime for someone else's answer. It
has no HTTPS/SVCB cache for exactly that reason — `SvcbEndpoint` carries no
TTL — and it *does* have an Alt-Svc cache, because RFC 7838's `ma` is the
origin's own statement of how long its advertisement is good for.

**A CRL is the Alt-Svc case, not the SVCB case.** RFC 5280 makes
`nextUpdate` part of the signed content; webpki refuses a CRL that omits it
(§2.2); and it was present on all three measured, at 7, 9 and 10 days. So the
refresh schedule is read off the artefact and nothing is invented:

- refresh at `nextUpdate`, and treat a CRL past it as **stale** rather than
  as absent (§5);
- refresh **sooner** than that, because SC-063 obliges a CA to republish
  *"within twenty-four (24) hours after recording a Certificate as
  revoked"* — so a client that only refreshes at `nextUpdate` is up to ten
  days behind a revocation the CA published yesterday. The honest re-check
  cadence is a caller-chosen interval bounded above by `nextUpdate`, with
  24 hours as the documented default because that is the number the CA is
  held to.

The `Cache-Control` on the fetch is deliberately not consulted. A CDN's idea
of how long to cache the bytes is a different fact from the CA's idea of how
long the statement is good for, and the second is signed.

### 4.5 Refreshing is the caller's call, and there is one opt-in that spawns

`Revocation::refresh(&self).await` — public, and nothing runs it. This
workspace does not spawn on a caller's behalf: the h3 body pump, the
WebSocket keep-alive and `stale-while-revalidate` are all written under that
sentence, and `Selecting::network_changed()` is already the precedent for a
housekeeping entry point that is public precisely because only the caller
knows when.

The one exception has a precedent too. `Native::multiplexed()` spawns, and
the bound sits on the opt-in constructor rather than on the type, so no
signature a `Spawn`-less runtime meets acquires one. `Revocation::refreshing_every(interval)`
on a `R: Spawn` can have the same shape, with the same price said in the
same place: **a spawner nobody drives turns "the CRLs go stale" into "the
CRLs never load"**, and the failure is silent in the fail-open direction.

## 5. Fail-open or fail-closed: three states, because two would be a lie

Every real client fudges the unknown case, and §2.4 measured that
`Client::new()` on Windows is one of them. The rule this workspace applies —
*a degraded value is only acceptable when the degradation has a direction* —
does not resolve it on its own, because the two directions are both real:
fail open and a revoked certificate is accepted; fail closed and, measured in
§2.5, **github.com becomes unreachable**, because its certificate names no
CRL at all.

The resolution is that *unknown* is two different facts, and collapsing them
is what makes the choice look binary. This is `Discovered::NoRecord` /
`NotConsulted` and `ClientCertAsk::Unobserved` / `NotAsked` for the third
time in this workspace:

| the chain is… | the answer |
|---|---|
| listed in a CRL we hold and trust | **refuse**, `Revoked` |
| covered by a CRL we hold, not listed | accept, `NotRevoked` |
| covered by a CRL we hold that is **past its `nextUpdate`** | **refuse**, `RevocationListStale` |
| issued by a CA we hold **no** CRL for | accept, `NoCrlForIssuer` |
| verified by a backend that does not check at all | accept, `Unchecked` |

The third row is fail-closed and is `ExpirationPolicy::Enforce`, which is
**not** rustls' default (`ExpirationPolicy::Ignore`, `crl/mod.rs:68`). It is
the row that carries the direction: a stale CRL is a statement we chose to
rely on and then stopped refreshing, so accepting it is accepting an answer
we know is out of date, and the failure it hides is the only failure this
feature exists to catch. Refusing costs a caller a connection they can fix by
refreshing; accepting costs them the feature, silently.

The fourth row is fail-open and is `UnknownStatusPolicy::Allow`, which **is**
rustls' non-default. It is fail-open because there is nothing else it can be:
the CA published no pointer, so there is no fetch that would have answered,
and refusing here is refusing a certificate for something its issuer did.
What makes that defensible rather than a fudge is that it is **reportable**:
the state reaches the caller through `TlsInfo` (§6), so *"this connection was
made to an origin whose CA does not tell us where its CRL is"* is a fact a
caller can log, alert on, or refuse themselves — where a `bool` would have
told them the connection was checked.

**The two rustls knobs are therefore both set, in opposite directions**, and
that pairing is the design rather than a default being accepted:
`.enforce_revocation_expiration()` on, `.allow_unknown_revocation_status()`
on. A caller who wants the strict reading gets
`Revocation::require_a_crl_for_every_issuer()`, which turns the fourth row
into a refusal and whose doc comment names github.com, because a caller
turning that on should meet the cost before their users do.

`only_check_end_entity_revocation()` is **not** set: the default is
`RevocationCheckDepth::Chain`, and an intermediate is the certificate whose
revocation matters most. It costs a CRL per issuer in the chain, and where
those are missing the fourth row already covers it.

## 6. What a caller sees

Two additions, and the split between them is this workspace's existing one:
the *outcome* rides `TlsInfo`, the *failure* rides `Error::source`.

**The outcome.** `TlsInfo::revocation: RevocationStatus`, the five-state enum
of §5, defaulting to `Unchecked` — the understating value, `ClientCertAsk`'s
shape exactly, so a backend that forgets the field can never be read as
having checked. It reaches a caller by the path `tls_version`, `tls_cipher`,
`alpn` and `client_cert` already take, and through the **same one setter**
they share, for the reason that setter exists: a backend either read the
handshake's outcome or it did not. `hc -v` prints it beside the SSL line, and
prints something different for each of the five, because *not revoked* and
*nobody looked* are the distinction the type is for.

**The failure, and it exposes a defect that is already here.** Today every
rustls handshake error becomes
`std::io::Error::other(format!("tls: {e}"))` under `ErrorKind::Tls`
(`crates/hclient-tls-rustls/src/stream.rs:34`). So a caller who wants to tell
*revoked* from *expired* from *unknown issuer* has to substring-match a
`Display` — which is the exact thing `ErrorKind`'s own doc comment says the
enum exists to prevent. The information is there and is thrown away: rustls
maps `webpki::Error::CertRevoked` to `CertificateError::Revoked`,
`UnknownRevocationStatus` and `CrlExpired { time, next_update }` to their own
variants (`rustls-0.23.43/src/webpki/mod.rs:79-84`), and `tls_err` formats
all of it into a string.

So the addition is a typed source, `UnexpectedStatus`'s shape — `thiserror`,
`#[non_exhaustive]`, public fields, reached through `Error::source` and
downcast:

```rust
#[derive(Debug, Clone, thiserror::Error)]
#[non_exhaustive]
pub enum CertificateRejected {
    #[error("the server's certificate has been revoked")]
    Revoked,
    #[error("the CRL for this issuer expired at {next_update:?}")]
    RevocationListStale { next_update: SystemTime },
    #[error("no revocation information for this certificate")]
    RevocationStatusUnknown,
    // …expired, name mismatch, unknown issuer: the rest of
    // `CertificateError`, translated rather than formatted
}
```

**Not a new `ErrorKind` variant.** `ErrorKind` answers *which stage failed*,
and a revoked certificate fails the TLS stage exactly as an expired one does;
a second axis inside it would be `Timeout(Phase)`'s shape applied to a
question `Phase` does not ask. The precedent is `Error::is_unsent()`, which
is a **field** rather than a kind precisely because *what a caller should do
next* and *where it broke* are different questions. And `ErrorKind` is
`#[non_exhaustive]`, so the cheapness of adding a variant is not the
argument for adding one.

**This is worth doing whether or not revocation is built**, and it is the
smallest useful piece of this whole document: today a client that cannot
reach an origin because its certificate expired and one that cannot reach it
because the name is wrong are the same value with different text.

## 7. What the seam owes: nothing, and the reason is not that it is small

`reports_alpn`, `applies_ech` and `TlsIdentity::config_id_for` are the shape
to compare against: a constant or a lookup defaulted to the understating
value, **read by the layer above to decide whether to ask the backend
something**. `hclient-native`'s connector reads `applies_ech` to decide
whether to fill `TlsRequest::ech` from an HTTPS record; it reads
`config_id_for` to resolve a label into a pool key before it opens a socket.
In each case the layer above holds an input and needs to know whether the
backend can take it.

**Revocation has no such input.** The CRLs are configured on the connector,
by the caller who built it; the transport holds nothing to hand over and
makes no per-request decision that turns on the answer. So no method reaches
`TlsConnect`, and adding one would be a question with no asker.

That is `Capabilities::proxy`'s classification arriving from the other side.
`AGENTS.md` records the rule: a **gate** guards a setting made on the
`Client`, and `build()` refuses when the transport cannot honour it; a
**report** states a fact about something configured *on the transport*, where
nothing at the client level could refuse it. Revocation is configured on the
`Rustls` value, which is the object that answers the question, so it cannot
be a gate — and the reader who cares is the reader holding that object.

**A `Capabilities::revocation_checked: bool` is refused, and for the reason
`Head::version` became an `Option`.** The honest answer is not a boolean: the
platform verifier checks the leaf only and fails open, a `Preloaded` build
checks the chain against whatever it was given, and a `NoTls` build checks
nothing. A `true` covering all three would be a *wrong* answer where the
absence is a missing one — and it would be reachable from a hook that has no
way to know which backend it is inside. `TlsInfo::revocation` says the same
thing per connection, where it is true.

`hclient-tls-native-tls` implements nothing here, and that is not a gap to
apologise for: its module doc already states that the platform stacks expose
neither the protocol version nor the cipher suite, and the OS is doing its own
revocation underneath (§2.4) on the two platforms where the OS has an
opinion. It leaves `TlsInfo::revocation` at `Unchecked`, which is the
understating default doing its job.

## 8. Certificate Transparency

The measurement here does not end in a design. It ends in a refusal, and the
refusal is a complete answer rather than a missing feature — the shape
`hclient-webtransport`'s `GOAWAY` finding takes.

### 8.1 rustls removed it, on the record, for this exact reason

**rustls 0.22.0 (2023-12-02)**, under *Breaking changes*, verbatim: *"Remove
support for SCTs provided via TLS extension. Ecosystem support for this is
rare compared to inclusion of SCTs in certificates."* The same release
removes `CertificateTransparencyPolicy` and
`WantsTransparencyPolicyOrClientCert`. Confirmed against the dependency
graph rather than the notes: rustls 0.21.12's normal dependencies include
`sct ^0.7.0`; 0.22.0's do not include `sct` at all.

The maintainers' stated reason is in PR #1329 rather than the changelog, and
it is the argument this section ends up making independently — *"one of the
challenges we'd have to overcome is keeping the metadata for CT logs in sync
and up to date, as well as matching the browser policies for the SCT
requirements. E.g., when Google turned down a deprecated log list endpoint it
wreaked havoc in the Android ecosystem because of a library that wasn't
keeping pace with the ecosystem failing closed."*

Measured in this tree: `signed_certificate_timestamp` appears nowhere in
rustls 0.23.43's source; `SCT => 0x0012` in `msgs/enums.rs` is an entry in
the extension-type enum and nothing reads it. rustls 0.24.0-dev.1 goes
further and has a `PeerMisbehaved::UnsolicitedSctList`.

**So the only SCTs reachable from a `ServerCertVerifier` are the ones
embedded in the certificate** — which is fine, because that is where
essentially all of them are, and §1.1 already retired the OCSP-stapled
delivery path.

### 8.2 The crate named for the job cannot verify them, and this was executed

`sct` 0.7.1 — *"Certificate transparency SCT verification library"*, by
rustls' author, ~12.2M downloads a month — hardcodes

```rust
const SCT_X509_ENTRY: [u8; 2] = [0, 0];
```

and writes it unconditionally into the signed structure
(`sct-0.7.1/src/lib.rs:159`, written at `:173`). That is RFC 6962's `x509_entry`, whose
`signed_entry` is the full leaf certificate. An **embedded** SCT is signed
over `precert_entry` (type 1), whose `signed_entry` is
`PreCert { issuer_key_hash[32], tbs_certificate }` — the TBSCertificate with
the SCT extension removed. The two hash different bytes.

Not read — run. Against `example.com`'s live chain on 2026-08-30, with the
log keys taken from Google's current `log_list.json`:

```
current Google log list: 48 logs
leaf DER 1002 bytes, reconstructed precert TBS 649 bytes
embedded SCTs: 2

SCT #0: operator=DigiCert state=usable log=944e4387faec… ts=1785363612470
   (A) sct 0.7.1 verify_sct, x509_entry over the leaf : Err(InvalidSignature)
   (B) precert_entry over the reconstructed TBS      : Ok(())
SCT #1: operator=Sectigo state=usable log=c8a3c47fc7b3… ts=1785363612492
   (A) sct 0.7.1 verify_sct, x509_entry over the leaf : Err(InvalidSignature)
   (B) precert_entry over the reconstructed TBS      : Ok(())
```

The probe is four steps and is worth keeping, because the negative half is
the one that would otherwise be believed rather than run:
`openssl s_client -showcerts` for the leaf and its issuer;
`x509-cert --features sct` to parse the extension;
`sct::verify_sct(leaf_der, wire, now, &[&log])` for `(A)`; and for `(B)` the
DER splice of §8.3 plus `ring::signature::ECDSA_P256_SHA256_ASN1` over
`0x00 || 0x00 || u64 timestamp || u16 1 || SHA-256(issuer SPKI) ||
u24 len(tbs) || tbs || u16 len(exts) || exts`. The log keys come from
`https://www.gstatic.com/ct/log_list/v3/log_list.json`, used here to *read* a
public key rather than to enforce a policy — which is the distinction §8.4
turns on.

So `sct` 0.7.1 verifies exactly the delivery mechanism rustls removed as
*"rare"*, and cannot verify the one that remains. Its own repository has had
no code change since 2024-11-19. It also does no policy at all: its `Log`
struct documents `description`, `url` and `operated_by` as *"not used by the
library"*, and `Error::should_be_fatal` returns **false** for `UnknownLog` —
which §8.4 turns into the sharpest fact in this document.

### 8.3 Reconstructing the precertificate is ours, and it is not the blocker

The `(B)` lines above are ~60 lines written for the probe: walk the leaf's
DER to the `[3] EXPLICIT` extensions element, drop the TLV whose `extnID` is
`1.3.6.1.4.1.11129.2.4.2`, re-encode the three enclosing lengths, prepend
`SHA-256` of the issuer's `SubjectPublicKeyInfo`, and hand the result to
`ring`. It works on the first real certificate it was pointed at.

The library route is measured and is oddly split:

- **`x509-cert` 0.3.0** has an `sct` feature that parses the extension in
  full — `SignedCertificateTimestampList`, `SerializedSct`,
  `SignedCertificateTimestamp`, `LogId`, `DigitallySigned` — but its
  `TbsCertificateInner` fields are `pub(crate)` behind read-only accessors,
  so a modified TBS **cannot be built and re-encoded** through its public
  API.
- **`x509-cert` 0.2** has public fields and **no `sct` feature**
  (`cargo add x509-cert@0.2 --features sct` → *"unrecognized feature"*).
- **`rustls-webpki`** grew SCT parsing on 2026-01-21, but only on the
  **0.104 alpha** line (0.104.0-alpha.3 onward; absent from 0.103.15, the
  stable release in this tree). Read from
  `rustls-webpki-0.104.0-alpha.7/src/sct.rs`, it does
  `let _signature_algorithm = …; let _signature = …` and carries a
  `_future_lifetime_for_signature: PhantomData` — it **discards exactly what
  verification needs**, and `EndEntityCert::sct_log_timestamps`'s own doc
  says *"Note this method does not verify the SCTs themselves."*
- **`x509-parser` 0.18.1** parses the extension and verifies nothing.
- **rust-openssl** binds no CT API at all, although the C library has one.

So the cryptography is available and the parsing is available and nobody
joins them. That is a weekend, not a blocker.

### 8.4 The blocker is the log list, and it is not technical

**Google's list is the only maintained one, and its own documentation forbids
this use.** From `log_lists.md` in the Chrome CT repository, bolded in the
original:

> **Chrome's CT log lists may not be used to facilitate CT enforcement in TLS
> clients other than Chrome without explicit written permission from Chrome's
> CT team.**

and, three lines down, *"Google must be able to make changes to the CT log
lists in response to incidents … including unannounced changes to the log
list to disrupt unauthorized use."* Building on it is building on a
dependency whose publisher has written down that it may break us on purpose.

**The alternative is archived and would produce a check that cannot fail.**
`ct-logs` 0.9.0, last published 2021-04-10, repository archived, README first
line *"Archived — This was part of an experimental effort to bring
certificate transparency to the rustls ecosystem. At the moment, that effort
is paused."* Measured, side by side:

| | logs | operators | newest shard | as of |
|---|---|---|---|---|
| `ct-logs` 0.9.0, compiled in | **34** | 6 | `Xenon2023` / `Oak2023` | 2021-04-10 |
| Google `log_list.json` | **48** (37 usable) | **8** | current | v89.34, 2026-08-29T13:39:10Z |

The two share not one currently-usable shard. A client shipping `ct-logs`
would resolve **zero** of today's SCTs to a known log, get `Error::UnknownLog`
for each, and — because `should_be_fatal()` returns `false` for exactly that
variant — treat every certificate as CT-compliant. **A security feature that
cannot fail**, which is this file's most-repeated defect, shipped as the
feature itself. It is also, precisely, the Android incident rustls' maintainer
cited in §8.1, with the sign flipped.

### 8.5 A policy needs a freshness timeout, and a compiled-in list cannot have one

Chrome's current policy (no version, last content change 2026-04-15) is short
enough to state exactly, and two of the rules a design would have to satisfy
are not about SCTs at all:

> At least one Embedded SCT from a CT log that was `Qualified`, `Usable`, or
> `ReadOnly` at the time of check; and there are Embedded SCTs from at least N
> distinct CT logs that were `Qualified`, `Usable`, `ReadOnly`, or `Retired`
> at the time of check … Among the SCTs satisfying requirement 2, at least two
> SCTs must be issued from distinct CT log operators as recognized by Chrome

with N = 2 for certificates of 180 days or less and 3 above that. The
one-Google-log rule is **gone** — removed for certificates issued on or after
2022-04-15 — and so, since 2026-04-15, is the requirement that one SCT come
from a non-tiled RFC 6962 log. Apple's policy differs in shape (a per-operator
cap rather than an operator floor, a 398-day ceiling, and it still requires an
RFC 6962 log). Mozilla enforces CT since Firefox 135 and has no log policy of
its own — it *"recognize[s] CT logs that appear in the Chromium
`log_list.json` list"*, which is the same dependency §8.4 refuses.

And the rule that decides it:

> Chrome will enforce CT so long as the `log_list_timestamp` of the freshest
> version of the log list Chrome stores is within the past 70 days … **All
> CT-enforcing user agents are strongly encouraged to implement a similar
> enforcement timeout**

A **log state that is per-log and time-varying**, an **operator identity that
is historical** (`previous_operators`, so the operator is the one at the time
of SCT issuance), and a **70-day freshness clock**. None of the three is
expressible in a list compiled into a released binary. The workspace's own
`nextUpdate`-vs-invented-TTL rule (§4.4) points the same way from the other
direction: the log list carries a `log_list_timestamp` and is *signed and
published daily* — which is exactly what makes it a live feed rather than a
constant, and a live feed we are told not to consume.

### 8.6 So: not built, and what is built instead is a report

The recommendation is to build **no CT verification**, and to say so in the
crate rather than leaving it as an absence someone rediscovers.

What is cheap, honest and useful is the observation half — the same split
mTLS took, where `ClientCertRequest` reports what the server asked and the
picker is the caller's (`docs/mtls-design.md` §3.5). A `TlsInfo::sct_log_ids:
Vec<[u8; 32]>` and a count is **a report, not a gate**: it needs no log list,
no policy, no freshness clock and no signature verification, and it lets
`hc -v` print *"2 embedded SCTs"* the way it already prints the cipher suite.
A caller who does have a list and a policy can verify them outside, and §8.3
says what that costs them.

**The precedent says the report needs a reader before it is written.**
`ClientCertRequest` was removed before its own commit for having neither a
producer nor a reader, and put back with both. `hc -v` is the reader here,
and it is a thin one; if that is judged too thin, the honest thing is to ship
nothing and keep this document.

**What would change the answer**, in order of likelihood: a log list
published for third-party consumption, with a stated freshness contract, by
anyone; written permission from Chrome's CT team, which their own page offers
to discuss; or `rustls-webpki` finishing what
[rustls/webpki#105](https://github.com/rustls/webpki/issues/105) — *"Support
verifying/accessing SCTs inside certificates"*, open since 2023-06-29 —
started, at which point rustls carries the verification and only the list
question remains.

## 9. Costs, measured

`cargo tree -e normal --prefix none`, unique crates, in scratch crates
outside this workspace; *new here* is the count after removing what this
tree's `Cargo.lock` already has.

| what it buys | crate set | crates | new here | build script | `-sys` | wasm32-unknown-unknown | wasm32-wasip2 |
|---|---|---|---|---|---|---|---|
| CRL parsing, signature check, serial lookup (§4.1) | none — `rustls-webpki` is already under `rustls` | **0** | **0** | — | — | — | — |
| reading a CRL distribution point out of a certificate | `x509-cert` 0.3.0, `--no-default-features` | 10 | **5** (`x509-cert`, `der`, `der_derive`, `spki`, `flagset`) | none | none | builds | builds |
| the same, plus parsing embedded SCTs | `x509-cert` 0.3.0, `--no-default-features --features sct` | 14 | **8** (the five above, plus `tls_codec`, `tls_codec_derive`, `zeroize_derive`) | none | none | builds | builds |
| verifying an SCT signature | `sct` 0.7.1 | 6 | **1** (`ring`, `untrusted`, `getrandom`, `libc`, `cfg-if` are already here) | none | none | **fails** — `getrandom` needs its `js` feature under `ring` | builds |

Two readings. **The CRL half costs nothing at all** if `Preloaded` ships
first, because `Preloaded` never reads a distribution point — the five crates
buy `Fetched`, and forty lines of DER walking buys the same thing for zero,
at the price of forty lines of DER walking this workspace would then own. The
precedent cuts both ways and is recent: `base64`, `form_urlencoded` and
`percent-encoding` were taken over hand-written equivalents once the cost was
*measured* rather than asserted, and the rule that decided it was whether a
wrong answer is visible. A misparsed CDP is visible — the fetch 404s. A
misparsed extension boundary is not.

**The CT half's crate cost is not what makes it expensive**, which is the
point of putting the table under §8: 8 crates and no build script is
affordable by this workspace's own standards. What is unaffordable is §8.4.

## 10. Deliberately not done, each with the reason

**A CRLite- or CRLSet-shaped compressed set.** These are the right answer for
a browser and they are measured: Mozilla's CRLite is a 4 MB snapshot every 45
days plus deltas, about **300 kB/day**, updated every 12 hours, queried
locally with no network at connection time — *"one thousand times more
bandwidth-efficient than daily CRL downloads"* — and Chrome's CRLSets are
600 kB covering *"about 1% of all revocations (thirty-five thousand of the
four million total)"*. Both are refused here for the same reason and it is
not the format: **somebody has to build the artefact.** Mozilla builds
CRLite daily from CT logs plus roughly three thousand CCADB-disclosed CRLs;
Google builds CRLSets from CCADB. That is infrastructure this workspace
would have to run, forever, correctly, or the feature is a stale file
pretending to be a revocation check — §8.4's defect wearing a different hat.

**Consuming somebody else's artefact is the interesting version and is
open.** Mozilla publishes CRLite filters; a `CrlSource` over them would be
`Preloaded` with a fetcher, and the design above already has the slot. What
it needs first is an answer to the question §8.4 asks about Chrome's log
list: is this published *for* third parties, with a freshness contract, or
merely published. Nobody has asked.

**A policy engine.** §5 is five states and two rustls flags; there is nothing
to configure that a constructor cannot say. The moment it needs an engine is
the moment somebody wants per-origin revocation policy, and that is
`RedirectPolicy`'s question, answerable then with `RedirectPolicy`'s answer.

**OCSP, in all three shapes.** §1 and §1.1.

**Revocation on `hclient-tls-native-tls`.** Nothing to build: the platform is
already doing it (§2.4) and `native-tls` exposes no hook to change what it
does. The crate reports `Unchecked` and its module doc says why, which is the
same paragraph that already explains the missing protocol version.

**A CRL cache shared with `hclient::cache`.** Tempting and wrong: RFC 9111
freshness is about an HTTP response and `nextUpdate` is about a signed
statement, and §4.4's whole argument is that the second is the one to obey.
Two lifetimes for one artefact is how they come to disagree.

## 11. The order this should be built in

1. **The typed TLS error** (§6, second half). It is independent of
   everything else here, it fixes a defect that exists today, and without it
   a revoked certificate is a string.
2. **`TlsInfo::revocation`** and the one setter (§6, first half), with every
   backend at the `Unchecked` default. Nothing checks anything yet; the
   vocabulary exists.
3. **`Rustls::with_revocation(roots, Preloaded)`** (§4.1, §4.2) with the
   five-state outcome and both rustls knobs set as §5 argues. Testable with
   `rcgen`, no socket, no clock — the CRL is bytes a test writes.
4. **`Fetched<C: Transport>`** (§4.2, §4.3), the `https://`-CDP refusal, and
   `refresh` (§4.5). This is where the crate-cost decision of §9 is actually
   made.
5. **`refreshing_every`** behind `R: Spawn`, on `multiplexed()`'s shape, if
   anybody asks.

Certificate Transparency is not on this list, and §8.6 says what would put it
there.
