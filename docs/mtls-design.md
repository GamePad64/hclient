# Client certificates: a design

mTLS across `hclient-tls-rustls`, `hclient-tls-native-tls` and backends
nobody has written yet — under this workspace's standing principle that a
full browser should be buildable on top of `hclient`.

Nothing here is implemented. What is measured is marked as measured.

## 1. What was measured first

**rustls cannot suspend a handshake to choose a certificate.**
`ResolvesClientCert::resolve` is called synchronously from inside the
state machine and returns `Option<Arc<CertifiedKey>>`; there is no
`WANT_X509_LOOKUP`-shaped pause anywhere in `rustls` 0.23.43. Browsers
that show a certificate picker do so on stacks that can pause —
BoringSSL and NSS — and that option is not on the table here.

**rustls ships no resolver that chooses.** `SingleCertAndKey` is the only
non-test implementation in the crate, and its body is:

```rust
fn resolve(
    &self,
    _root_hint_subjects: &[&[u8]],
    _sigschemes: &[SignatureScheme],
) -> Option<Arc<CertifiedKey>> {
    Some(self.0.clone())
}
```

Both narrowing parameters are discarded by name, and
`with_client_auth_cert(chain, key)` builds exactly this. So rustls
**delivers** the server's `CertificateRequest` — `certreq.canames` in TLS
1.2, `certreq.extensions.authority_names` in 1.3 — and **decides
nothing**. The same shape as `redirect::decide` and `RedirectPolicy`:
mechanism parses and hands over, policy chooses.

**Half the filtering is rustls' after all.** `ClientAuthDetails::resolve`
calls `certkey.key.choose_scheme(sigschemes)` on whatever the resolver
returned and falls back to `Empty` when nothing matches. So signature
schemes are handled; the acceptable-CA list is not.

**Sending no certificate is a legal, expressible outcome.** A resolver
returning `None` yields `ClientAuthDetails::Empty` and the handshake
continues. A design that could not express "I have nothing for you" would
be wrong; this one gets it for free.

**native-tls has one identity per connector and no callback at all.**
`TlsConnectorBuilder::identity(Identity)` is the whole surface, and
`Identity` is built from bytes — `from_pkcs12` or `from_pkcs8` — with no
third constructor. Its own SChannel backend imports a PFX into a
**memory** store; it does not read the user's store.

**So neither shipped backend reads a client certificate out of a system
store today.** `AGENTS.md` names smartcards as a reason
`hclient-tls-native-tls` exists; that part promises more than the crate
delivers, and should be corrected.

## 2. The binding constraint, and what it rules out

native-tls can only be told *which connector to use*, and only before any
handshake begins. rustls can decide during a handshake but cannot wait
for anything while doing so.

**Therefore the seam must express selection before the handshake.**
Anything that decides *during* one is a rustls capability and cannot be
the portable surface. This rules out, as seam-level designs:

- a callback the seam invokes with the server's CA list — native-tls has
  nowhere to call it;
- carrying an identity in the request — every representation excludes a
  real deployment: `CertificateDer + PrivateKeyDer` excludes any store
  whose key is not extractable, `native_tls::Identity` excludes rustls, a
  file path excludes both (formats differ, and a store-resident key has
  no path);
- carrying the platform's own identifier — Windows searches by
  thumbprint/subject/issuer, PKCS#11 by `CKA_ID`/`CKA_LABEL`, Android by
  a KeyChain **alias**. Four identifiers is not one identifier, and
  putting them in the seam re-imports the provider-specificity the seam
  exists to remove.

What is left, and is the same on all four platforms, is a **name the
caller invented**.

## 3. The design, in three layers

### 3.1 The seam: a label, resolved before the handshake

```rust
// hclient-core — a request extension, `RequireVersion`'s shape
pub struct ClientIdentity(pub &'static str);

// hclient-tls
pub trait TlsIdentity {
    fn config_id(&self) -> TlsConfigId;

    /// The identity the caller named, or `None` — this backend has none
    /// by that name.
    fn config_id_for(&self, name: &str) -> Option<TlsConfigId> {
        let _ = name;
        None
    }

    fn presents_client_certs(&self) -> bool { false }
}

// TlsRequest gains one borrowed field
pub identity: Option<&'a str>,
```

The default **refuses every name**. A backend that knows nothing about
labels says so, rather than silently connecting with its default
identity — the understating direction, and the same rule as
`reports_alpn`, `applies_ech` and `SUPPORTS_UNIX`.

The label is not a credential, so it may travel in `http::Extensions`
alongside `RequireVersion` and `AllowEarlyData`. A *certificate* may not,
for the reason digest credentials do not: extensions reach
`Transport::execute` and are readable by any transport, including one
this workspace did not write.

### 3.2 The transport: resolution and isolation

`hclient-native` reads `ClientIdentity(name)`, calls `config_id_for`, and:

- `None` → a typed refusal naming the label, before any socket;
- `Some(id)` → that `TlsConfigId` goes into `PoolKey`, and
  `identity: Some(name)` into `TlsRequest`.

**`PoolKey` needs no change.** `Security::Tls(TlsConfigId)` is already a
component of it, so two labels resolving to two ids cannot share a
connection — by construction rather than by a check. This is the single
most important correctness property here, because its failure mode is
presenting one tenant's identity to a server on another's behalf, and it
comes for free.

It still needs a test in the dangerous direction: two labels, one origin,
and an assertion that the server saw two connections.

### 3.3 The backend: where secrets and platform code live

How a label becomes a configuration is each backend's own business,
exactly as per-ALPN config caching already is.

```rust
let tls = Rustls::builder()
    .identity("corp", IdentitySource::Pem { chain, key })?
    .identity("personal", IdentitySource::WindowsStore(
        StoreQuery::sha256(thumbprint),
    ))?
    .build()?;
```

`IdentitySource` is per backend and not part of the seam. For rustls:

| variant | needs | feature |
|---|---|---|
| `Pem { chain, key }` | `rustls-pemfile` | — |
| `Resolver(Arc<dyn ResolvesClientCert>)` | nothing | — |
| `WindowsStore(StoreQuery)` | `rustls-cng` | `windows-store` |
| `Keychain(KeychainQuery)` | `security-framework` + a `SigningKey` we write | `keychain` |
| `Pkcs11(TokenQuery)` | `cryptoki` + a `SigningKey` we write | `pkcs11` |

`Resolver(..)` is the escape hatch that keeps this from being a ceiling:
anything rustls can do, a caller can reach, including filtering by the
server's CA hints.

For native-tls the map is label → `TlsConnector`, each built with its own
`identity(..)`. That is the whole of what the library allows, and it is
enough for the seam.

## 4. What each platform actually requires

| | the query a caller writes | how the key signs | bridge |
|---|---|---|---|
| **Windows** | store + SHA-256 / subject / issuer | CNG `NCryptKey`, not extractable | **`rustls-cng` 0.7.1, complete** |
| **macOS** | Keychain, `SecIdentity` | `SecKey::create_signature` | parts exist, **no bridge**: a `SigningKey` over `SecKey` is ours to write |
| **Linux** | a PEM file, or PKCS#11 `CKA_ID`/`CKA_LABEL` | token | file works today; **`rustls-pkcs11` is at 29 downloads a month** and is not a foundation |
| **Android** | `KeyChain.getPrivateKey(alias)` | hardware Keystore, never extractable | **nothing**; JNI, as `rustls-platform-verifier` already does for server verification |

Two things follow. **Linux has no system store for client identities at
all** — `/etc/ssl/certs` holds trusted roots, which are other people's
certificates; there is nothing to select between, so the label rarely
earns its place there. And **Windows is the only platform where the work
is assembly rather than authorship**, which is where an implementation
should start.

## 5. The browser story, and its honest cost

A browser filters its installed identities by the server's acceptable
CAs, shows a picker when several match, remembers the answer per origin,
and sends nothing when none match. Of those, this design gives:

- **filter by the server's CAs** — a `ResolvesClientCert` doing it, wrapped
  as `IdentitySource::Resolver`, or a store-backed source using
  `find_by_issuer_str`;
- **send nothing** — `resolve` returning `None`, measured above;
- **remember per origin** — the caller's, keyed however they like, applied
  as a label per request;
- **a picker** — *not* inside the handshake, because nothing here can wait.

The portable way to reach a human is **two phases**:

1. Connect with an identity-less configuration. The handshake completes;
   the server's `CertificateRequest` is recorded and surfaced. The
   application sees whatever the server does about an empty certificate —
   usually `401`, `403`, or a close.
2. The caller decides — dialog, policy, whatever — sets
   `ClientIdentity(label)` and sends again. A different `TlsConfigId`
   means a fresh connection, which is what a different identity requires
   anyway.

The cost is one extra handshake per origin per session, paid once. It is
worse than suspending, and it is the only thing that works on a stack
that cannot suspend.

For phase 1 to be possible the hints must be observable, which is one
addition:

```rust
// TlsInfo, filled by a recording resolver
pub client_auth_requested: bool,
pub requested_authorities: Vec<Vec<u8>>,   // DER DNs, empty when the
                                           // server sent none
```

and, since `Connected` already carries `tls_version`, `tls_cipher` and
`alpn`, one more borrowed field there so a hook can see it. native-tls
fills neither and reports `false` — the understating direction again, and
honest: it genuinely cannot see the request.

**`Option` versus empty matters here** and must not be flattened. The CA
list is `Option<&[DistinguishedName]>` in rustls, and its own
documentation says: *if the list is empty, the client should send
whatever certificate it has*. So *the server named no authorities* and
*the server did not ask for a certificate at all* are different facts,
and only `client_auth_requested` separates them.

## 6. What is refused, and why each refusal rather than a fallback

- **A label the backend does not know** — an error naming the label. Not
  the default identity, which would be a setting silently replaced by a
  different one.
- **A label on an `http://` request** — an error naming it. There is no
  handshake to present anything in, and a caller who wrote it meant it.
- **A label against a backend reporting `presents_client_certs == false`**
  — the same refusal, reached by `config_id_for` returning `None`. It
  cannot be a `build()`-time gate, because the label is per request; that
  is a fact about where the setting lives rather than an omission.

## 7. Costs, and the window

**`TlsRequest` is not `#[non_exhaustive]`**, so adding `identity` is a
breaking change for anyone who constructs one — that is transport
authors, not TLS backend authors, who only read it. Measured in this
tree: **three** production sites, all in `hclient-native/src/connect.rs`,
and six more in the TLS backends' own tests. Before the first stable
release this is free; after it, a major version.

**The QUIC path is a second, parallel change and is easy to miss.**
`QuicTlsRequest` in `hclient-tls/src/quic.rs` is a **separate type**, and
deliberately so — its own doc says reusing `TlsRequest` would mean a type
whose fields mean different things on the two paths. So `identity` has to
be added there too, and `hclient-h3` has to read it. Skipping that does
not produce an error: it produces HTTP/3 connections that silently
present no client certificate while the HTTP/1 and HTTP/2 paths present
one — the same request answered differently by protocol, which is the
worst shape a gap can take. `TlsIdentity` is shared by both connect
traits already, so `config_id_for` needs writing once.

**The same change should give `TlsRequest` the attribute**, with a
constructor and setters — `Connected`'s treatment — so that the *next*
field is not a second breaking change. It is the classic answer 1 shape
(the caller builds it), and the cost of the attribute is exactly the
setters.

**`TlsIdentity::config_id_for` is a defaulted method**, so it breaks
nobody.

**`TlsInfo`'s two new fields** cost every backend nothing: both default to
the understating value.

## 8. Deliberately not done, each with what it needs

- **A picker inside the handshake.** Needs a TLS stack that can suspend.
  Not rustls, not native-tls, not soon.
- **Selecting by the server's CA list at the seam.** It is a rustls
  capability, reachable through `IdentitySource::Resolver`, and putting
  it in the seam would make native-tls answer a question it cannot hear.
- **macOS, PKCS#11 and Android sources.** Each needs a `SigningKey` over a
  platform key handle, and Android needs a JNI layer besides. Windows
  first, because it is the only one where the bridge exists.
- **Renegotiating an existing connection with a different identity.**
  TLS 1.3 has no renegotiation, and a different identity means a
  different connection here anyway — which the pool key already enforces.

## 9. The order this should be built in

1. `ClientIdentity`, `config_id_for`, `TlsRequest::identity`,
   `QuicTlsRequest::identity`, and the transport's resolution and refusal
   — with the two-labels-two-connections test **on both protocol paths**,
   since a label honoured over TCP and ignored over QUIC is worse than one
   honoured nowhere. Nothing platform-specific, and it makes the
   file-based case selectable, which is the case that already works.
2. `TlsInfo::{client_auth_requested, requested_authorities}` and the
   `Connected` field. This is what makes phase 1 of the browser story
   possible and costs one recording resolver.
3. `IdentitySource::WindowsStore` over `rustls-cng`, behind a feature.
   Assembly rather than authorship.
4. macOS, then PKCS#11, then Android — in descending order of how much of
   each has to be written from nothing.

Step 1 is worth doing on its own: today a caller with two client
certificates and one `hclient::Client` has no way to choose between them
at all.
