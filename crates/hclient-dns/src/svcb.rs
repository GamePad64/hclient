//! RFC 9460 client semantics, over a record some backend has already
//! decoded.
//!
//! **Why this is in the trait crate and not in a backend.** It arrived in
//! `hclient-dns-system`, where its own doc comment gave the reason it
//! could not stay there: the rules that decide whether a record is usable
//! at all — `AliasMode` versus `ServiceMode` (§2.4), a root `TargetName` meaning
//! the owner name (§2.5), `mandatory` semantics (§8) — "are the part of
//! this crate most likely to be got subtly wrong, and they are identical
//! on every platform." That argument was made about two backends inside
//! one crate (`res_query` and `DnsQuery_UTF8`). `hclient-dns-doh` is a
//! third, in a different crate, and it decodes the same wire format the
//! `res_query` path does — so either the rules move to where every backend
//! can reach them, or the `DoH` backend gets a second copy of them and the
//! copies drift. They moved.
//!
//! **What did not move: any wire parsing, and no decoder is named here at
//! all.** Nothing in this module reads bytes. [`RawBinding`] holds no
//! borrowed memory and no platform detail, and each backend fills it in
//! from whatever its decoder produced — a `domain`-decoded `Https` record
//! on the `res_query` and `DoH` paths, an OS-parsed `DNS_SVCB_DATA` on
//! Windows. That is what keeps this crate free of a DNS codec: a consumer
//! who only ever uses `IpLiteralOnly` does not link one.
//!
//! **That was true of the dependency and false of the signature, for two
//! decoders in a row.** A `binding_from_decoded` lived here, behind a
//! `codec` feature, taking a decoded record and naming its decoder in
//! four places — the parameter, the error type and two bounds — while
//! this crate re-exported no such crate, so an outside caller could not
//! name the types it demanded without adding that crate to their own
//! manifest at a matching version. It read
//! `&dns_message_parser::rr::ServiceBinding` before it read
//! `&domain::rdata::svcb::Https<..>`, which is the point: the leak
//! outlived the decoder it leaked. It is `hclient-dns-doh`'s now — its
//! only caller throughout — and this crate's public surface names no
//! foreign crate but `bytes`, `futures-core` and `hclient-core`, all of
//! which it re-exports or shares through its own seam. `ca5ab5e4` did the
//! same for `winnow` in `hclient-proto`, and for the same reason: a
//! pre-release is where a leaked foreign type is cheap to withdraw.
//!
//! So what a backend shares with every other backend is [`RawBinding`],
//! [`RawParam`] and [`endpoint_from_binding`] — the RFC's rules and the
//! vocabulary they read, which is the part identical on every platform and
//! the whole of the argument at the top of this file. Reaching a decoded
//! record into that vocabulary is each backend's own, because the decoder
//! is each backend's own.

pub use crate::error::SvcbRecordError;

use crate::{RData, Record, SvcbEndpoint};
use bytes::Bytes;
use std::net::{Ipv4Addr, Ipv6Addr};
use std::time::Duration;

/// The `SvcParamKeys` this client understands well enough to honour a
/// `mandatory` requirement for (RFC 9460 §8).
///
/// "Understood" is deliberately not the same as "has a field in
/// `SvcbEndpoint`". `no-default-alpn` (2) has no field — it modifies how
/// `alpn` is read, and a client that only ever offers protocols it found
/// in `alpn` behaves correctly either way — but it IS understood, so a
/// record naming it as mandatory stays usable. Everything outside this
/// list is the opposite case: `dohpath` (7, RFC 9461) is real and
/// registered, nothing here acts on it, so a record that makes it
/// mandatory is one this client must not use.
///
/// `dohpath` is worth a second sentence now that `hclient-dns-doh` exists:
/// that crate takes its endpoint as a whole URI from its caller and
/// discovers nothing by DNS, so key 7 is still acted on by nothing here,
/// and this list is still right. Recognising it would mean resolving a `DoH`
/// endpoint by DNS, which is circular for the first lookup — see that
/// crate's module doc.
const RECOGNISED_KEYS: &[u16] = &[0, 1, 2, 3, 4, 5, 6];

/// One HTTPS record, in terms every backend can produce.
///
/// **Why an intermediate type rather than each backend building a
/// `SvcbEndpoint` itself.** The RFC 9460 rules that decide whether a record
/// is usable at all — `AliasMode` versus `ServiceMode` (§2.4), a root
/// `TargetName` meaning the owner name (§2.5), `mandatory` semantics (§8) —
/// are the part of SVCB support most likely to be got subtly wrong, and
/// they are identical on every platform. Writing them once, over a type
/// that holds no borrowed memory and no platform detail, means no backend
/// can drift from another: `hclient-dns-system`'s `windows.rs` fills this
/// in from an OS-parsed `DNS_SVCB_DATA`, its `svcb.rs` fills it in from an
/// `Https` record parsed over one record's RDATA, `hclient-dns-doh` fills
/// it in from the same decoder reading a whole message off an HTTP
/// response body, and all three then go through [`endpoint_from_binding`].
///
/// **The fields are `pub` for exactly that reason, and it is why the
/// decoder-shaped half of each of those conversions belongs to the backend
/// rather than here.** A type filled in from outside has to be nameable
/// and writable from outside, and this one names nothing foreign. A
/// *function* taking an already-decoded record cannot manage that: it has
/// to name the decoder, which would put that crate's major version into
/// this crate's promise. One such function lived here behind a feature and
/// is `hclient-dns-doh`'s now — see this module's own doc for the two
/// decoders it leaked in turn.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawBinding {
    pub priority: u16,
    /// The record's owner name, without a trailing dot.
    pub owner: String,
    /// The `TargetName`, without a trailing dot. **Empty means the root**
    /// (`.` on the wire), which is what §2.4.2 and §2.5 give their special
    /// meanings to.
    pub target: String,
    pub params: Vec<RawParam>,
    /// The record's TTL, as the resolver reported it — `None` where it
    /// reported none. See [`crate::Record::ttl`], which this becomes.
    pub ttl: Option<Duration>,
}

/// One `SvcParam`, reduced to what `SvcbEndpoint` can hold plus the key
/// number of everything it cannot.
#[derive(Debug, Clone, PartialEq, Eq)]
// **Both public enums in this module are deliberately exhaustive**, which
// is the opposite of the rule the fourteen error types elsewhere in this
// workspace follow, and the difference is who stands on the other side.
//
// `Other(u16)` already carries every key this crate does not model, so a
// new variant here never means "IANA registered something" — it means
// *this crate now parses that parameter*, and a `_` arm would silently
// drop one we had gone to the trouble of reading.
//
// `SvcbRecordError` below is the sharper case, and the compiler found it
// where a reading had not: `hclient-dns-system` and `hclient-dns-doh` both
// **translate** it, variant by variant, into their own error types. An
// error that reaches an end caller can afford `#[non_exhaustive]`, because
// there the caller's `_` arm says *something else went wrong*, which is
// true. An error that crosses a seam into a translator cannot: there the
// `_` arm is a mapping, and a new variant would quietly acquire the wrong
// one.
//
// So this is `Event`'s rule, twice over: exhaustiveness is the mechanism,
// and the compile error is the feature.
pub enum RawParam {
    Mandatory(Vec<u16>),
    Alpn(Vec<Vec<u8>>),
    NoDefaultAlpn,
    Port(u16),
    Ipv4Hint(Vec<Ipv4Addr>),
    Ipv6Hint(Vec<Ipv6Addr>),
    /// The `ECHConfigList` **including RFC 9460 §7.3's redundant length
    /// prefix**, which is the form rustls parses. Backends are responsible
    /// for handing it over in that form; see each one's note, because they
    /// differ in whether the prefix survives their decoder.
    Ech(Vec<u8>),
    /// A parameter this crate does not model, carried as its key number
    /// only — enough for the `mandatory` check below, and nothing else.
    Other(u16),
}

impl RawParam {
    /// The `SvcParamKey` this parameter came from (RFC 9460 §14.3.2).
    ///
    /// Total and closed over the enum on purpose — no `_ =>` arm — so a new
    /// variant becomes a compile error here rather than a key that silently
    /// reports as something else and quietly satisfies a `mandatory` entry
    /// it should not.
    fn key(&self) -> u16 {
        match self {
            Self::Mandatory(_) => 0,
            Self::Alpn(_) => 1,
            Self::NoDefaultAlpn => 2,
            Self::Port(_) => 3,
            Self::Ipv4Hint(_) => 4,
            Self::Ech(_) => 5,
            Self::Ipv6Hint(_) => 6,
            Self::Other(key) => *key,
        }
    }
}

/// One record, as an endpoint this client may act on.
///
/// `Ok(None)` means "well-formed but must not be used" — the two cases RFC
/// 9460 gives for that are an unsupported `mandatory` key (§8) and an
/// `AliasMode` record whose target is the root (§2.4.2, "the service is not
/// available").
///
/// It hands back a [`Record`] rather than a bare [`SvcbEndpoint`] because
/// the TTL belongs to the record and not to what the record says — so
/// there is one field carrying it, on the type every backend already has
/// to build, and no way to fill an endpoint's copy and forget the
/// record's.
///
/// # Errors
///
/// `Err` is reserved for the one client-side check RFC 9460 calls
/// malformed and no decoder makes: a `mandatory` list naming a key the
/// record does not carry.
pub fn endpoint_from_binding(binding: &RawBinding) -> Result<Option<Record>, SvcbRecordError> {
    // RFC 9460 §2.4.1: "In AliasMode, ... recipients MUST ignore any
    // SvcParams that are present", so none of them reach the endpoint.
    if binding.priority == 0 {
        // §2.4.2: an AliasMode target of "." means the service does not
        // exist. Emitting it would hand the caller an endpoint pointing at
        // the name it just asked about — a resolution loop dressed up as
        // an answer.
        if binding.target.is_empty() {
            return Ok(None);
        }
        return Ok(Some(
            Record::new(RData::Https(SvcbEndpoint {
                priority: 0,
                target: binding.target.clone(),
                alpn: Vec::new(),
                port: None,
                ipv4hint: Vec::new(),
                ipv6hint: Vec::new(),
                ech_config_list: None,
            }))
            // An AliasMode record has a TTL like any other, and it is the one
            // a caller following the alias would have to respect.
            .ttl(binding.ttl),
        ));
    }

    let mut endpoint = SvcbEndpoint {
        priority: binding.priority,
        // RFC 9460 §2.5: in ServiceMode a TargetName of "." means the
        // record's own owner name. Substituting it here means every
        // `SvcbEndpoint` this crate emits carries a name that can be
        // connected to, so no consumer has to know the "." convention.
        target: if binding.target.is_empty() {
            binding.owner.clone()
        } else {
            binding.target.clone()
        },
        alpn: Vec::new(),
        port: None,
        ipv4hint: Vec::new(),
        ipv6hint: Vec::new(),
        ech_config_list: None,
    };

    let mut mandatory: &[u16] = &[];
    for parameter in &binding.params {
        match parameter {
            RawParam::Mandatory(key_ids) => mandatory = key_ids,
            RawParam::Alpn(ids) => ids.clone_into(&mut endpoint.alpn),
            RawParam::Port(port) => endpoint.port = Some(*port),
            RawParam::Ipv4Hint(hints) => hints.clone_into(&mut endpoint.ipv4hint),
            RawParam::Ipv6Hint(hints) => hints.clone_into(&mut endpoint.ipv6hint),
            RawParam::Ech(config_list) => {
                endpoint.ech_config_list = Some(Bytes::from(config_list.clone()));
            }
            // Understood, but with nothing in `SvcbEndpoint` to hold it —
            // see `RECOGNISED_KEYS`. Dropped rather than given an invented
            // field.
            //
            // Kept separate from the arm below, though both bodies are
            // empty: this key is *understood* and simply has no field;
            // the next is *not modelled at all*. Merging them would lose
            // that distinction, which `RECOGNISED_KEYS`'s doc relies on.
            #[allow(
                clippy::match_same_arms,
                reason = "Understood, but with nothing in `SvcbEndpoint` to hold it — see `RECOGNISED_KEYS`. Dropped rather than given an invented field. Kept separate from the arm below, though both bodies are empty: this key is *understood* and simply has no field; the next is *not modelled at all*. Merging them would l..."
            )]
            RawParam::NoDefaultAlpn => {}
            // Not modelled; kept out of the endpoint, but still visible to
            // the `mandatory` check below through its key number.
            RawParam::Other(_) => {}
        }
    }

    for key in mandatory {
        if !binding.params.iter().any(|p| p.key() == *key) {
            // RFC 9460 §8 — a key declared mandatory has to be present.
            // No decoder checks this: it is a statement about the record as
            // a whole, not about any one parameter's encoding.
            return Err(SvcbRecordError::MandatoryKeyAbsent { key: *key });
        }
        // RFC 9460 §8: "If the client is unable to comply [with a
        // mandatory key], the client MUST ignore this SVCB RR." Ignoring
        // one record is not rejecting the RRSet — see this function's doc.
        if !RECOGNISED_KEYS.contains(key) {
            return Ok(None);
        }
    }

    Ok(Some(Record::new(RData::Https(endpoint)).ttl(binding.ttl)))
}
