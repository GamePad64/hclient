//! RFC 9460 client semantics, over a record some backend has already
//! decoded.
//!
//! **Why this is in the trait crate and not in a backend.** It arrived in
//! `hclient-dns-system`, where its own doc comment gave the reason it
//! could not stay there: the rules that decide whether a record is usable
//! at all — AliasMode versus ServiceMode (§2.4), a root TargetName meaning
//! the owner name (§2.5), `mandatory` semantics (§8) — "are the part of
//! this crate most likely to be got subtly wrong, and they are identical
//! on every platform." That argument was made about two backends inside
//! one crate (`res_query` and `DnsQuery_UTF8`). `hclient-dns-doh` is a
//! third, in a different crate, and it decodes the same wire format the
//! `res_query` path does — so either the rules move to where every backend
//! can reach them, or the DoH backend gets a second copy of them and the
//! copies drift. They moved.
//!
//! **What did not move: any wire parsing.** Nothing here reads bytes.
//! [`RawBinding`] holds no borrowed memory and no platform detail, and
//! each backend fills it in from whatever its decoder produced — a
//! `dns-message-parser` `ServiceBinding` on the `res_query` and DoH paths,
//! an OS-parsed `DNS_SVCB_DATA` on Windows. That is what keeps this crate
//! free of a DNS codec: a consumer who only ever uses `IpLiteralOnly` does
//! not link one.

use crate::SvcbEndpoint;
use alloc::string::String;
use alloc::vec::Vec;
/// Only the `codec` conversion below builds an owned name out of a
/// `DomainName`; without the feature these two are dead, and `#![no_std]`
/// makes that visible where the std prelude used to hide it.
///
/// A `///` rather than a `//`, because `cargo fmt` reorders this block and
/// carries a doc comment with its item where it leaves a plain one behind
/// — which is the hazard AGENTS.md records about the `send-bound-exception`
/// markers, met here in a `use` list.
#[cfg(feature = "codec")]
use alloc::{borrow::ToOwned, string::ToString};
use bytes::Bytes;
use core::net::{Ipv4Addr, Ipv6Addr};

/// The SvcParamKeys this client understands well enough to honour a
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
/// and this list is still right. Recognising it would mean resolving a DoH
/// endpoint by DNS, which is circular for the first lookup — see that
/// crate's module doc.
const RECOGNISED_KEYS: &[u16] = &[0, 1, 2, 3, 4, 5, 6];

/// One HTTPS record, in terms every backend can produce.
///
/// **Why an intermediate type rather than each backend building a
/// `SvcbEndpoint` itself.** The RFC 9460 rules that decide whether a record
/// is usable at all — AliasMode versus ServiceMode (§2.4), a root
/// TargetName meaning the owner name (§2.5), `mandatory` semantics (§8) —
/// are the part of SVCB support most likely to be got subtly wrong, and
/// they are identical on every platform. Writing them once, over a type
/// that holds no borrowed memory and no platform detail, means no backend
/// can drift from another: `hclient-dns-system`'s `windows.rs` fills this
/// in from an OS-parsed `DNS_SVCB_DATA`, its `svcb.rs` fills it in from a
/// `dns-message-parser` `ServiceBinding`, `hclient-dns-doh` fills it in
/// from the same decoder over an HTTP response body, and all three then go
/// through [`endpoint_from_binding`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawBinding {
    pub priority: u16,
    /// The record's owner name, without a trailing dot.
    pub owner: String,
    /// The TargetName, without a trailing dot. **Empty means the root**
    /// (`.` on the wire), which is what §2.4.2 and §2.5 give their special
    /// meanings to.
    pub target: String,
    pub params: Vec<RawParam>,
}

/// One SvcParam, reduced to what `SvcbEndpoint` can hold plus the key
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
    /// The ECHConfigList **including RFC 9460 §7.3's redundant length
    /// prefix**, which is the form rustls parses. Backends are responsible
    /// for handing it over in that form; see each one's note, because they
    /// differ in whether the prefix survives their decoder.
    Ech(Vec<u8>),
    /// A parameter this crate does not model, carried as its key number
    /// only — enough for the `mandatory` check below, and nothing else.
    Other(u16),
}

impl RawParam {
    /// The SvcParamKey this parameter came from (RFC 9460 §14.3.2).
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

/// The one way a well-decoded record can still be malformed as a *record*.
///
/// A single-variant enum rather than a bare `u16`, and its own type rather
/// than a variant of some backend's error: every backend that decodes SVCB
/// has a large error enum of its own describing how its transport can
/// fail, and none of those failures can happen here — this function reads
/// no bytes, opens no socket and calls no OS API. Each backend maps this
/// into its own enum at the call site (`hclient-dns-system` into
/// `SvcbLookupError::MandatoryKeyAbsent`, `hclient-dns-doh` into
/// `DohError::MandatoryKeyAbsent`), which keeps their public taxonomies
/// unchanged by the move.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum SvcbRecordError {
    /// RFC 9460 §8: the record's `mandatory` list names a key the record
    /// does not actually carry. Checked here rather than by a decoder,
    /// because it is a statement about the record as a whole and not about
    /// any one parameter's encoding.
    #[error("SvcParamKey {key} is listed as mandatory but is not present in the record")]
    MandatoryKeyAbsent { key: u16 },
}

/// One record, as an endpoint this client may act on.
///
/// `Ok(None)` means "well-formed but must not be used" — the two cases RFC
/// 9460 gives for that are an unsupported `mandatory` key (§8) and an
/// AliasMode record whose target is the root (§2.4.2, "the service is not
/// available"). `Err` is reserved for the one client-side check RFC 9460
/// calls malformed and no decoder makes: a `mandatory` list naming a key
/// the record does not carry.
pub fn endpoint_from_binding(
    binding: &RawBinding,
) -> Result<Option<SvcbEndpoint>, SvcbRecordError> {
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
        return Ok(Some(SvcbEndpoint {
            priority: 0,
            target: binding.target.clone(),
            alpn: Vec::new(),
            port: None,
            ipv4hint: Vec::new(),
            ipv6hint: Vec::new(),
            ech_config_list: None,
        }));
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
            RawParam::Alpn(ids) => endpoint.alpn = ids.clone(),
            RawParam::Port(port) => endpoint.port = Some(*port),
            RawParam::Ipv4Hint(hints) => endpoint.ipv4hint = hints.clone(),
            RawParam::Ipv6Hint(hints) => endpoint.ipv6hint = hints.clone(),
            RawParam::Ech(config_list) => {
                endpoint.ech_config_list = Some(Bytes::from(config_list.clone()));
            }
            // Understood, but with nothing in `SvcbEndpoint` to hold it —
            // see `RECOGNISED_KEYS`. Dropped rather than given an invented
            // field.
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

    Ok(Some(endpoint))
}

/// A `dns-message-parser` record, reduced to the backend-neutral form.
///
/// Behind the `codec` feature, because it is the one function here that
/// names a decoder. Every backend that reads wire-format DNS with
/// `dns-message-parser` — `hclient-dns-system`'s `res_query` path and
/// `hclient-dns-doh` — goes through this, so the ECH note below is written
/// down once instead of once per backend. `hclient-dns-system`'s Windows
/// path does not: `DnsQuery_UTF8` hands back records the OS has already
/// parsed, and that path builds a [`RawBinding`] itself.
#[cfg(feature = "codec")]
pub fn binding_from_decoded(binding: &dns_message_parser::rr::ServiceBinding) -> RawBinding {
    use dns_message_parser::rr::ServiceParameter;

    let params = binding
        .parameters
        .iter()
        .map(|parameter| match parameter {
            ServiceParameter::MANDATORY { key_ids } => RawParam::Mandatory(key_ids.clone()),
            // `alpn_ids` are `String` because the decoder reads them as
            // UTF-8; ALPN is a byte protocol, so they go back to bytes. The
            // conversion is lossless in this direction — a non-UTF-8 ALPN
            // id would have failed to decode upstream.
            ServiceParameter::ALPN { alpn_ids } => {
                RawParam::Alpn(alpn_ids.iter().map(|id| id.as_bytes().to_vec()).collect())
            }
            ServiceParameter::NO_DEFAULT_ALPN => RawParam::NoDefaultAlpn,
            ServiceParameter::PORT { port } => RawParam::Port(*port),
            ServiceParameter::IPV4_HINT { hints } => RawParam::Ipv4Hint(hints.clone()),
            ServiceParameter::IPV6_HINT { hints } => RawParam::Ipv6Hint(hints.clone()),
            // **The two-byte length prefix is put back on, and that is not
            // cosmetic.** RFC 9460 §7.3 defines the `ech` SvcParamValue as
            // an ECHConfigList "including the redundant length prefix", and
            // that prefixed form is what rustls parses — an ECHConfigList
            // is a TLS vector, so its codec reads a `u16` length first.
            // This decoder validates the prefix and then returns the
            // payload WITHOUT it (`decode/rr/draft_ietf_dnsop_svcb_https
            // .rs`, key 5, measured — the round-trip test is what caught
            // it). Windows does not do this: `DnsQuery_UTF8` hands back the
            // SvcParamValue verbatim, prefix included, so
            // `hclient-dns-system`'s `windows.rs` adds nothing. Storing the
            // stripped form would leave `SvcbEndpoint::ech_config_list`,
            // whose stated purpose is to feed `rustls::EchConfig` directly,
            // holding something rustls cannot parse — a field that looks
            // populated and fails far from here.
            ServiceParameter::ECH { config_list } => {
                let len = u16::try_from(config_list.len()).expect(
                    "the decoder read this list out of a u16-length SvcParam and compared \
                     the two, so it cannot exceed u16::MAX",
                );
                let mut prefixed = Vec::with_capacity(config_list.len() + 2);
                prefixed.extend_from_slice(&len.to_be_bytes());
                prefixed.extend_from_slice(config_list);
                RawParam::Ech(prefixed)
            }
            ServiceParameter::PRIVATE { number, .. } => RawParam::Other(*number),
            ServiceParameter::KEY_65535 => RawParam::Other(65535),
        })
        .collect();

    RawBinding {
        priority: binding.priority,
        owner: name_to_string(&binding.name),
        target: name_to_string(&binding.target_name),
        params,
    }
}

/// A decoded name as a host name, without the trailing dot.
///
/// `DomainName`'s own `Display` emits `cloudflare.com.`, and `.` for the
/// root — which [`RawBinding::target`] represents as the empty string, so
/// the root maps to `""` here rather than to `"."`. `SvcbEndpoint::target`
/// feeds a connector, and every other name on that path is written without
/// the root dot.
#[cfg(feature = "codec")]
pub fn name_to_string(name: &dns_message_parser::DomainName) -> String {
    // The root needs no special case: `DomainName`'s `Display` writes
    // it as `.`, and stripping the trailing dot from that leaves the
    // empty string, which is exactly how `RawBinding::target` spells
    // the root. An explicit `is_root()` branch was here and was removed
    // — mutation testing found it equivalent to the code below, i.e.
    // unreachable-in-effect and therefore untestable, which is the kind
    // of line that reads as load-bearing while proving nothing.
    let text = name.to_string();
    text.strip_suffix('.').unwrap_or(&text).to_owned()
}
