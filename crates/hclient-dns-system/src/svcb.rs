//! RFC 9460 client semantics over a decoded HTTPS record.
//!
//! **The wire parsing is not done here.** `domain` decodes the record —
//! NLnet Labs', `no_std`-capable, and chosen over `dns-message-parser`
//! for one reason above the rest: [`Https::parse`] takes a parser over
//! **one record's RDATA**, which is the only shape `system-resolver` can
//! hand over on all five of its platforms. The previous decoder exposed
//! decoding at the message level alone, so this module used to build a
//! synthetic DNS response around every record it wanted read — ninety
//! lines of header, question section and length-prefixed names, written
//! only to be taken apart again by the next call.
//!
//! What is left in this module is the part a DNS decoder correctly
//! refuses to do — deciding, per RFC 9460, which of the decoded records a
//! *client* may act on, and what the shape of "no records" is.
//!
//! Verified against the same byte vectors that used to test the
//! hand-written parser and then the envelope, because the question they
//! answer has only moved, not gone away: a truncated record and an
//! RDLENGTH that overruns still have to become an `Err` here rather than
//! a panic or a hang.
//!
//! **Three things the change bought, each pinned by a test below.** An
//! AliasMode record carrying SvcParams is now *ignored* per §2.4.1
//! instead of rejecting the whole RRSet, which this crate had recorded as
//! a deliberate divergence and which the shared client rules had always
//! implemented correctly and never been reached with. An RRSet larger
//! than a DNS message can frame is answered rather than refused, because
//! the 65535-octet limit was the envelope's and never the resolver's. And
//! a compression pointer inside RDATA — which §2.2 forbids and a
//! non-conformant sender may still write — can now only reach this
//! record's own octets, where before it was resolved against a message
//! this crate had assembled out of several.
//!
//! **What is no longer parsed by hand: everything.** The twelve-byte
//! header this module used to read went with `res_query` when the
//! platform calls moved to `system-resolver`, and the envelope went with
//! the decoder that needed one.
//!
//! [`Https::parse`]: domain::rdata::svcb::Https::parse
#![forbid(unsafe_code)]

use crate::error::SvcbLookupError;
use hclient_dns::SvcbEndpoint;

pub(crate) use wire::endpoints_from_records;

/// RR type 65, RFC 9460 §14.1 — the one type this crate asks about.
pub(crate) const TYPE_HTTPS: u16 = 65;

/// Asks the system resolver for `name`'s HTTPS records and applies RFC
/// 9460's client rules to them.
///
/// **This is the whole of the adapter, and it is three lines because the
/// split is in the right place.** `system-resolver` owns every platform
/// call and every rule about DNS; this crate owns every rule about what a
/// *client* may do with an HTTPS record. Nothing here knows what a socket
/// or a `DNS_RECORD` is.
///
/// Blocking — the platform calls are. The caller runs this on a `Blocking`
/// thread; see the crate root.
///
/// **`NameDoesNotExist` becomes an empty answer**, which is the one place
/// this adapter deliberately loses a distinction the crate below it keeps.
/// A caller looking for *this host does not exist* finds it in the A/AAAA
/// stream, where it belongs; reporting it here as well would make a name
/// with no HTTPS record and a name with nothing at all fail the same
/// lookup twice, and only one of those is this stream's business.
pub(crate) fn lookup(name: &str) -> Result<Vec<SvcbEndpoint>, SvcbLookupError> {
    match system_resolver::lookup(name, TYPE_HTTPS) {
        Ok(records) => endpoints_from_records(&records),
        Err(system_resolver::Error::NameDoesNotExist) => Ok(Vec::new()),
        // A build with no backend answers `Unsupported`, and it is an
        // error here rather than an empty answer for the reason
        // `supports_svcb` exists: the pair has to say *absent capability*
        // rather than *asked and found none*, and `Resolve`'s own contract
        // is that a caller reads the first from the capability and never
        // from the stream.
        Err(other) => Err(SvcbLookupError::Resolver(other)),
    }
}

/// The RFC 9460 client semantics, and this crate's thin wrapper over them.
///
/// `RawBinding`, `RawParam` and the decision procedure over them used to
/// live in this file. They moved to `hclient_dns::svcb` when
/// `hclient-dns-doh` became a third backend that has to apply exactly the
/// same rules to exactly the same decoded record — the drift this file's
/// own doc comment warned about, one crate further out. Nothing about the
/// rules changed in the move; what stayed here is the mapping from the
/// shared error into this crate's own taxonomy, so that a caller of
/// `hclient-dns-system` still sees a single `SvcbLookupError`.
#[allow(
    unused_imports,
    reason = "`RawParam` is built by this file's tests alone now that the platform backends have left; the re-export is what keeps their `use` lines pointing at one place"
)]
pub(crate) use hclient_dns::svcb::{RawBinding, RawParam};

/// [`hclient_dns::svcb::endpoint_from_binding`], with its error mapped into
/// this crate's enum.
///
/// A wrapper rather than a re-export: `SvcbLookupError` describes how a
/// *platform resolver* can fail, and its `MandatoryKeyAbsent` variant is
/// the one member of that enum that is a statement about a record instead.
/// Keeping the variant means the move is invisible from outside this
/// crate — including to the tests at the bottom of this file, which are
/// the tests the rules were accepted against and which have not been
/// touched.
pub(crate) fn endpoint_from_binding(
    binding: &RawBinding,
) -> Result<Option<SvcbEndpoint>, SvcbLookupError> {
    hclient_dns::svcb::endpoint_from_binding(binding).map_err(|e| match e {
        hclient_dns::svcb::SvcbRecordError::MandatoryKeyAbsent { key } => {
            SvcbLookupError::MandatoryKeyAbsent { key }
        }
    })
}

/// Turning the records a resolver reported into endpoints.
///
/// **This crate no longer talks to a resolver, and it no longer builds a
/// DNS message either.** `system-resolver` does the first — five
/// platforms, one seam, `Vec<Record>` out — and `domain` does the second:
/// [`Https::parse`] takes a parser over one record's RDATA, which is the
/// shape a resolver hands over. What is left here is the half that is
/// about HTTPS records rather than about DNS: RFC 9460's client rules.
mod wire {
    use super::endpoint_from_binding;
    use crate::error::SvcbLookupError;
    use domain::base::iana::SvcParamKey;
    use domain::dep::octseq::{Octets, parse::Parser};
    use domain::rdata::svcb::Https;
    use domain::rdata::svcb::value::AllValues;
    use hclient_dns::SvcbEndpoint;
    use hclient_dns::svcb::{RawBinding, RawParam};
    use system_resolver::Record;

    /// RR type 65 (RFC 9460 §14.1).
    const TYPE_HTTPS: u16 = 65;

    /// The endpoints in `records`, by RFC 9460's client rules.
    ///
    /// Records of another type are stepped over rather than rejected — a
    /// CNAME chain answers with the CNAME beside the records it leads to,
    /// and on the platforms that hand over a whole message those arrive
    /// here too.
    ///
    /// An empty `Vec` is *there are no usable HTTPS records*, which
    /// includes a record the client rules refuse; every `Err` is a record
    /// that could not be read at all.
    ///
    /// **RFC 9460 §2.2 — "If any RRs are malformed, the client MUST reject
    /// the entire RRSet" — is the `?` in the loop**, and it is worth
    /// naming because it used to be somebody else's. `Dns::decode` failed
    /// the whole message on one bad record, so the rule was obeyed by a
    /// property of the decoder rather than by a line here. Reading records
    /// one at a time means the rule has to be stated, and stating it is
    /// what makes it testable: keeping the records that happened to parse
    /// would let anyone able to inject one malformed record steer a client
    /// onto whichever ones survived.
    pub(crate) fn endpoints_from_records(
        records: &[Record],
    ) -> Result<Vec<SvcbEndpoint>, SvcbLookupError> {
        let mut found = Vec::new();
        for record in records.iter().filter(|r| r.rtype == TYPE_HTTPS) {
            if let Some(endpoint) = endpoint_from_binding(&binding_from_rdata(record)?)? {
                found.push(endpoint);
            }
        }
        Ok(found)
    }

    /// One record's RDATA, read as an HTTPS record and reduced to the form
    /// every backend in this workspace produces.
    ///
    /// **The bytes are parsed where they arrive, and that is narrower than
    /// what came before.** RFC 9460 §2.2 requires SVCB TargetName to be
    /// sent uncompressed; a non-conformant sender's compression pointer is
    /// resolved against the parser's octets, and those are this record's
    /// own RDATA and nothing else — so the only bytes a pointer can reach
    /// are the ones already being decoded. The envelope this replaced had
    /// to argue that same point about a message it had assembled out of
    /// several records.
    ///
    /// The owner name and the TTL are the resolver's, taken off the
    /// `Record` rather than off the wire, because they are what the
    /// platform reported rather than what the RDATA carries.
    fn binding_from_rdata(record: &Record) -> Result<RawBinding, SvcbLookupError> {
        let mut parser = Parser::from_ref(record.rdata.as_slice());
        let https = Https::parse(&mut parser).map_err(SvcbLookupError::Malformed)?;

        let mut params = Vec::new();
        for value in https.params().iter_all() {
            params.push(raw_param(value.map_err(SvcbLookupError::Malformed)?));
        }

        Ok(RawBinding {
            priority: https.priority(),
            owner: record.name.trim_end_matches('.').to_owned(),
            // `Display` for a name writes the root as the empty string,
            // which is exactly what `RawBinding::target` documents it
            // means — so there is no special case here, and the
            // `trim_end_matches` is for every other name.
            target: https.target().to_string().trim_end_matches('.').to_owned(),
            params,
            ttl: Some(record.ttl),
        })
    }

    /// One decoded SvcParam, in `RawParam`'s vocabulary.
    ///
    /// **The three keys `domain` models and this workspace does not become
    /// `Other`, and that is load-bearing rather than tidy.** `dohpath`
    /// (7), `ohttp` (8) and `tls-supported-groups` (9) are real,
    /// registered, and acted on nowhere here; `RawParam::Other` is what
    /// RFC 9460 §8's `mandatory` check reads to refuse a record that makes
    /// one of them mandatory. Dropping them instead would let such a
    /// record through as usable.
    ///
    /// **`Unknown` is not only "a key nobody models".** `AllValues`
    /// answers it for a known key whose value did not parse as that key —
    /// measured, `domain` yields an `Err` from the iterator for the shapes
    /// this crate can produce, but the fallback exists — so a key inside
    /// [`RECOGNISED_KEYS`] arriving as `Unknown` is a malformed record
    /// wearing a recognised number. It is refused by the caller for that
    /// reason rather than reported as an unmodelled key.
    ///
    /// [`RECOGNISED_KEYS`]: hclient_dns::svcb
    fn raw_param<Octs: Octets>(value: AllValues<Octs>) -> RawParam {
        match value {
            AllValues::Mandatory(keys) => {
                RawParam::Mandatory(keys.iter().map(SvcParamKey::to_int).collect())
            }
            AllValues::Alpn(alpn) => {
                RawParam::Alpn(alpn.iter().map(|p| p.as_ref().to_vec()).collect())
            }
            AllValues::NoDefaultAlpn(_) => RawParam::NoDefaultAlpn,
            AllValues::Port(port) => RawParam::Port(port.port()),
            // RFC 9460 §7.3's ECHConfigList, **including the redundant
            // two-octet length prefix**, which is the form `RawParam::Ech`
            // is documented to carry and the form rustls parses: the
            // SvcParamValue for key 5 *is* the ECHConfigList, and `domain`
            // wraps the value's octets without stripping anything.
            AllValues::Ech(ech) => RawParam::Ech(ech.as_slice().to_vec()),
            AllValues::Ipv4Hint(hint) => RawParam::Ipv4Hint(hint.iter().collect()),
            AllValues::Ipv6Hint(hint) => RawParam::Ipv6Hint(hint.iter().collect()),
            // The key numbers are named rather than written: `domain`
            // keeps the `SvcParamValue` trait that carries `key()`
            // private, so a value of one of these types cannot be asked
            // for its own key from outside the crate.
            AllValues::DohPath(_) => RawParam::Other(SvcParamKey::DOHPATH.to_int()),
            AllValues::Ohttp(_) => RawParam::Other(SvcParamKey::OHTTP.to_int()),
            AllValues::TlsSupportedGroups(_) => {
                RawParam::Other(SvcParamKey::TLS_SUPPORTED_GROUPS.to_int())
            }
            AllValues::Unknown(unknown) => RawParam::Other(unknown.key().to_int()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rstest::rstest;
    use std::assert_matches;
    use std::time::Duration;
    use system_resolver::Record;
    // Named here rather than at the top of the file since the RFC 9460
    // client semantics moved to `hclient_dns::svcb`: nothing outside these
    // tests builds a `RawParam` on this platform any more.
    use std::net::{Ipv4Addr, Ipv6Addr};

    /// RR type 65, RFC 9460 §14.1.
    const TYPE_HTTPS: u16 = 65;
    /// SvcParamKeys, RFC 9460 §14.3.2, for building test records.
    const KEY_MANDATORY: u16 = 0;
    const KEY_ALPN: u16 = 1;
    const KEY_NO_DEFAULT_ALPN: u16 = 2;
    const KEY_PORT: u16 = 3;
    const KEY_ECH: u16 = 5;
    /// RFC 9461's `dohpath`: real, registered, and not acted on here.
    const KEY_DOHPATH: u16 = 7;

    /// Test vectors are written as hex. `const_hex` rather than a
    /// hand-rolled loop: the crate is already in this workspace's graph,
    /// its error names which character was wrong, and an odd number of
    /// digits is a refusal rather than a silently short vector.
    fn hex(s: &str) -> Vec<u8> {
        const_hex::decode(s).expect("a test vector must be hex")
    }

    /// A name in RFC 1035 §3.1 label form. `""` is the root.
    fn name_wire(name: &str) -> Vec<u8> {
        let mut out = Vec::new();
        for label in name.split('.').filter(|l| !l.is_empty()) {
            out.push(u8::try_from(label.len()).expect("label under 256"));
            out.extend_from_slice(label.as_bytes());
        }
        out.push(0);
        out
    }

    fn svcb_rdata(priority: u16, target: &str, params: &[(u16, Vec<u8>)]) -> Vec<u8> {
        let mut out = priority.to_be_bytes().to_vec();
        out.extend(name_wire(target));
        for (key, value) in params {
            out.extend_from_slice(&key.to_be_bytes());
            out.extend_from_slice(
                &u16::try_from(value.len())
                    .expect("value fits")
                    .to_be_bytes(),
            );
            out.extend_from_slice(value);
        }
        out
    }

    /// The records a resolver reported, as `system-resolver` hands them
    /// over — which is what a backend gives this crate now, so it is what
    /// a test gives it too.
    fn response(_qname: &str, answers: &[(&str, u16, Vec<u8>)]) -> Vec<Record> {
        answers
            .iter()
            .map(|(owner, rtype, rdata)| {
                Record::new(
                    *owner,
                    *rtype,
                    system_resolver::CLASS_IN,
                    Duration::from_secs(300),
                    rdata.clone(),
                )
            })
            .collect()
    }

    fn one_record(priority: u16, target: &str, params: &[(u16, Vec<u8>)]) -> Vec<Record> {
        response(
            "example.com",
            &[(
                "example.com",
                TYPE_HTTPS,
                svcb_rdata(priority, target, params),
            )],
        )
    }

    fn endpoints(records: Vec<Record>) -> Result<Vec<SvcbEndpoint>, SvcbLookupError> {
        endpoints_from_records(&records)
    }

    // ---- what the decoder does not let a test get wrong -------------

    /// **`domain` derives no `PartialEq` on `AllValues` at all**, which
    /// closes by construction the trap the previous decoder had to be
    /// pinned against: `dns-message-parser`'s `ServiceParameter` compared
    /// and hashed by key number alone, so `assert_eq!(param,
    /// ServiceParameter::ALPN { alpn_ids: vec!["h2"] })` passed without
    /// checking a single value, and two tests here existed to say so.
    ///
    /// The style those tests bought is kept — every assertion below
    /// reaches into the extracted [`SvcbEndpoint`] fields, which are
    /// ordinary `Vec`s and `Option`s — and it is now the style rather than
    /// a defence. This test is what says the hazard cannot come back
    /// silently: a `PartialEq` added upstream makes the commented line
    /// compile, and a decoder swapped for one that has it fails here.
    #[test]
    fn a_decoded_parameter_cannot_be_compared_at_all_so_a_test_cannot_be_fooled_by_one() {
        // Uncommenting this must not compile. `AllValues` derives `Debug`
        // and `Clone` and nothing else, so there is no equality to be
        // wrong about — which is the assertion, made the only way a type
        // property can be made from inside a test that must still build.
        //   let _ = |a: AllValues<&[u8]>, b: AllValues<&[u8]>| a == b;
        //
        // What is checkable at run time is that the values this crate
        // does compare are the extracted ones, and that two ALPN lists
        // differing only in content really do come out different.
        let h2 = one_record(1, "", &[(KEY_ALPN, vec![2, b'h', b'2'])]);
        let h3 = one_record(1, "", &[(KEY_ALPN, vec![2, b'h', b'3'])]);
        assert_ne!(
            endpoints(h2).expect("decodes"),
            endpoints(h3).expect("decodes"),
            "two records differing only inside a parameter must differ after decoding, or              every assertion in this module is comparing key numbers"
        );
    }

    // ---- a real answer, end to end -------------------------------------

    // ---- what happens when the decoder refuses -------------------------
    //
    // These are the vectors that used to test a hand-written parser. They
    // now test the seam that replaced it: that a refusal becomes an `Err`
    // from this crate rather than a panic, a hang, or a silently empty
    // result. That question did not go away with the parser.

    #[test]
    fn one_malformed_record_rejects_the_whole_rrset_including_the_good_ones() {
        // RFC 9460 §2.2: "If any RRs are malformed, the client MUST reject
        // the entire RRSet." `Dns::decode` fails the whole message on one
        // bad record, so this holds structurally — but it is the behaviour
        // a caller depends on, so it is pinned rather than inferred.
        let good = svcb_rdata(1, "good.example.com", &[(KEY_ALPN, vec![2, b'h', b'2'])]);
        let bad = svcb_rdata(2, "bad.example.com", &[(KEY_PORT, vec![0x01])]);
        let msg = response(
            "example.com",
            &[
                ("example.com", TYPE_HTTPS, good),
                ("example.com", TYPE_HTTPS, bad),
            ],
        );
        let err = endpoints(msg).expect_err("a one-byte port is not a port");
        assert_matches!(
            err,
            SvcbLookupError::Malformed(_),
            "the good record must not survive the malformed one"
        );
    }

    // ---- RFC 9460 client semantics, which are this module's job ---------

    #[test]
    fn a_servicemode_target_of_root_becomes_the_records_owner_name() {
        let msg = response(
            "example.com",
            &[("owner.example.com", TYPE_HTTPS, svcb_rdata(1, "", &[]))],
        );
        let got = endpoints(msg).expect("valid");
        assert_eq!(
            got[0].target, "owner.example.com",
            "RFC 9460 §2.5 — substituting it here means no consumer has to know the rule"
        );
    }

    #[test]
    fn an_aliasmode_record_keeps_its_target_and_carries_no_params() {
        let msg = one_record(0, "alias.example.com", &[]);
        let got = endpoints(msg).expect("valid");
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].priority, 0, "priority 0 is what marks AliasMode");
        assert_eq!(got[0].target, "alias.example.com");
        assert!(got[0].alpn.is_empty() && got[0].port.is_none());
    }

    /// **The divergence from RFC 9460 that used to be pinned here is
    /// gone, and the test that pinned it named this exact outcome.**
    /// §2.4.1 says a recipient MUST *ignore* SvcParams found in an
    /// AliasMode record — the record stays usable. `dns-message-parser`
    /// read SvcParams only when `priority != 0`, so such a record left
    /// bytes unconsumed and failed the whole message; this crate accepted
    /// that as a fail-safe divergence and wrote down what would end it:
    /// *"if this test ever fails, upstream started honouring §2.4.1 and
    /// `endpoint_from_binding`'s AliasMode branch becomes reachable with
    /// real parameters for the first time."*
    ///
    /// It failed on the first run after the decoder changed. Upstream did
    /// nothing; the record is read by a decoder that parses SvcParams
    /// whatever the priority says, and the shared client rules — which had
    /// implemented §2.4.1 correctly all along — finally get to apply it.
    ///
    /// So the assertion is the MUST rather than the divergence, and it
    /// discriminates all three possible behaviours: rejecting the record
    /// fails the `expect`, applying the params fails the last two lines,
    /// and ignoring them is what passes.
    #[test]
    fn an_aliasmode_records_params_are_ignored_rather_than_applied_or_rejected() {
        let msg = one_record(
            0,
            "alias.example.com",
            &[
                (KEY_ALPN, vec![2, b'h', b'2']),
                (KEY_PORT, vec![0x01, 0xbb]),
            ],
        );
        let got = endpoints(msg).expect("RFC 9460 §2.4.1: ignore the params, keep the record");
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].priority, 0, "priority 0 is what marks AliasMode");
        assert_eq!(got[0].target, "alias.example.com");
        assert!(
            got[0].alpn.is_empty(),
            "§2.4.1 makes ignoring an AliasMode record's SvcParams a MUST, and an `h2` here              would be one applied"
        );
        assert_eq!(
            got[0].port, None,
            "the same MUST, read through the parameter whose effect a caller would notice              first: a port of 443 taken from an alias record redirects the connection"
        );
    }

    #[test]
    fn an_aliasmode_record_targeting_the_root_is_dropped_as_no_service() {
        let got = endpoints(one_record(0, "", &[])).expect("not malformed, just unusable");
        assert!(
            got.is_empty(),
            "RFC 9460 §2.4.2: an AliasMode target of `.` means the service does not exist — \
             emitting it would hand the caller a resolution loop"
        );
    }

    #[test]
    fn a_mandatory_key_we_do_not_understand_drops_only_that_record() {
        let usable = svcb_rdata(1, "good.example.com", &[(KEY_ALPN, vec![2, b'h', b'2'])]);
        let requires_dohpath = svcb_rdata(
            2,
            "bad.example.com",
            &[
                (KEY_MANDATORY, KEY_DOHPATH.to_be_bytes().to_vec()),
                (KEY_ALPN, vec![2, b'h', b'2']),
                (KEY_DOHPATH, b"/dns-query".to_vec()),
            ],
        );
        let msg = response(
            "example.com",
            &[
                ("example.com", TYPE_HTTPS, usable),
                ("example.com", TYPE_HTTPS, requires_dohpath),
            ],
        );
        let got = endpoints(msg).expect("an unsupported mandatory key is not malformed");
        assert_eq!(
            got.len(),
            1,
            "RFC 9460 §8 drops the record that requires it, not the RRSet"
        );
        assert_eq!(got[0].target, "good.example.com");
    }

    #[test]
    fn a_mandatory_key_we_do_understand_keeps_the_record() {
        let msg = one_record(
            1,
            "svc.example.com",
            &[
                (KEY_MANDATORY, KEY_ALPN.to_be_bytes().to_vec()),
                (KEY_ALPN, vec![2, b'h', b'2']),
            ],
        );
        let got = endpoints(msg).expect("valid");
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].alpn, vec![b"h2".to_vec()]);
    }

    #[test]
    fn a_mandatory_key_absent_from_the_record_is_an_error() {
        let msg = one_record(
            1,
            "svc.example.com",
            &[(KEY_MANDATORY, KEY_PORT.to_be_bytes().to_vec())],
        );
        assert_eq!(
            endpoints(msg),
            Err(SvcbLookupError::MandatoryKeyAbsent { key: KEY_PORT }),
            "RFC 9460 §8: a key declared mandatory has to be there — a check the decoder does \
             not make, because it is about the record as a whole"
        );
    }

    #[test]
    fn an_unknown_svcparamkey_is_stepped_over_without_dropping_the_record() {
        // Not registered, no field in `SvcbEndpoint`, and not mandatory —
        // RFC 9460 §2.1 says that is not an error and the record around it
        // stays usable.
        let msg = one_record(
            1,
            "svc.example.com",
            &[
                (KEY_ALPN, vec![2, b'h', b'2']),
                (31337, vec![0xde, 0xad, 0xbe, 0xef]),
            ],
        );
        let got = endpoints(msg).expect("an unknown key is not malformed");
        assert_eq!(got.len(), 1, "the record must survive the unknown key");
        assert_eq!(got[0].alpn, vec![b"h2".to_vec()]);
        assert_eq!(got[0].target, "svc.example.com");
    }

    #[test]
    fn no_default_alpn_is_understood_but_has_no_field_and_is_dropped() {
        // The distinction `RECOGNISED_KEYS` encodes: understood well
        // enough to honour a `mandatory` entry, with nothing in
        // `SvcbEndpoint` to put it in — so it is dropped rather than given
        // an invented field.
        let msg = one_record(
            1,
            "svc.example.com",
            &[
                (KEY_MANDATORY, KEY_NO_DEFAULT_ALPN.to_be_bytes().to_vec()),
                (KEY_ALPN, vec![2, b'h', b'2']),
                (KEY_NO_DEFAULT_ALPN, vec![]),
            ],
        );
        let got = endpoints(msg).expect("valid");
        assert_eq!(
            got.len(),
            1,
            "a mandatory key with no field is still a key this client understands"
        );
        assert_eq!(got[0].alpn, vec![b"h2".to_vec()]);
    }

    /// The ECHConfigList must come out byte-for-byte as it went in,
    /// **including the two-byte length prefix** RFC 9460 §7.3 makes part of
    /// the SvcParamValue — that prefixed form is what rustls parses.
    ///
    /// This is the test that caught the decoder stripping it: written as a
    /// plain round-trip, it failed with `Some([254, 13, 0, 1, 2])` against
    /// `Some([0, 5, 254, 13, 0, 1, 2])`. Without a round-trip assertion the
    /// field would have looked populated and failed at the point of use,
    /// inside rustls, far from here.
    #[test]
    fn the_ech_config_list_round_trips_including_its_length_prefix() {
        let payload = [0xfeu8, 0x0d, 0x00, 0x01, 0x02];
        let mut ech = (payload.len() as u16).to_be_bytes().to_vec();
        ech.extend_from_slice(&payload);
        let msg = one_record(1, "svc.example.com", &[(KEY_ECH, ech.clone())]);
        let got = endpoints(msg).expect("valid");
        assert_eq!(
            got[0].ech_config_list.as_deref(),
            Some(ech.as_slice()),
            "an ECHConfigList is opaque here — it feeds rustls, it is not interpreted, and \
             it is not reshaped either"
        );
    }

    #[test]
    fn a_port_is_carried_through() {
        let msg = one_record(1, "svc.example.com", &[(KEY_PORT, vec![0x01, 0xbb])]);
        let got = endpoints(msg).expect("valid");
        assert_eq!(got[0].port, Some(443));
    }

    #[test]
    fn records_of_other_types_in_the_answer_section_are_stepped_over() {
        // A CNAME ahead of the HTTPS record is the ordinary shape of a real
        // answer, and it must neither be interpreted nor rejected.
        let msg = response(
            "example.com",
            &[
                ("example.com", 5, name_wire("real.example.com")),
                (
                    "real.example.com",
                    TYPE_HTTPS,
                    svcb_rdata(1, "svc.example.com", &[]),
                ),
            ],
        );
        let got = endpoints(msg).expect("valid");
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].target, "svc.example.com");
    }

    #[test]
    fn a_response_with_no_answers_is_empty_rather_than_an_error() {
        assert_eq!(endpoints(response("example.com", &[])), Ok(Vec::new()));
    }

    // ---- the semantics both backends share ------------------------------
    //
    // Everything above reaches `endpoint_from_binding` through the wire
    // decoder, which is the Unix path. These go in through `RawBinding`
    // directly — the same door `windows.rs` uses. That matters more than it
    // looks: the Windows FFI has never been executed on any machine, so
    // this is the only coverage its RESULT has. What is proved here is that
    // once a Windows record has been turned into a `RawBinding`, every RFC
    // 9460 rule applied to it is the same code, tested the same way, as on
    // Unix.

    fn binding(priority: u16, owner: &str, target: &str, params: Vec<RawParam>) -> RawBinding {
        RawBinding {
            ttl: None,
            priority,
            owner: owner.to_owned(),
            target: target.to_owned(),
            params,
        }
    }

    #[test]
    fn a_binding_from_either_backend_gets_the_same_root_target_substitution() {
        let got = endpoint_from_binding(&binding(1, "owner.example.com", "", vec![]))
            .expect("valid")
            .expect("usable");
        assert_eq!(
            got.target, "owner.example.com",
            "RFC 9460 §2.5 is applied to a Windows-shaped binding exactly as to a decoded one"
        );
    }

    #[test]
    fn a_binding_in_aliasmode_targeting_the_root_is_dropped_whichever_backend_built_it() {
        assert_eq!(
            endpoint_from_binding(&binding(0, "example.com", "", vec![])).expect("valid"),
            None
        );
    }

    #[test]
    fn a_binding_in_aliasmode_discards_params_whichever_backend_built_it() {
        // Unreachable through the Unix decoder, which refuses such a record
        // outright (see `an_aliasmode_record_carrying_params_is_rejected_by
        // _the_decoder`). It IS reachable from Windows, where the OS parses
        // the params and hands them over regardless of mode — so this rule
        // is not dead code there, and this is the only test that can prove
        // it holds.
        let got = endpoint_from_binding(&binding(
            0,
            "example.com",
            "alias.example.com",
            vec![RawParam::Alpn(vec![b"h2".to_vec()]), RawParam::Port(443)],
        ))
        .expect("valid")
        .expect("usable");
        assert_eq!(got.target, "alias.example.com");
        assert!(
            got.alpn.is_empty() && got.port.is_none(),
            "RFC 9460 §2.4.1: recipients MUST ignore SvcParams in AliasMode"
        );
    }

    #[test]
    fn mandatory_semantics_are_the_same_for_a_binding_from_either_backend() {
        // Understood and present: usable.
        let ok = endpoint_from_binding(&binding(
            1,
            "example.com",
            "svc.example.com",
            vec![
                RawParam::Mandatory(vec![1]),
                RawParam::Alpn(vec![b"h2".to_vec()]),
            ],
        ))
        .expect("valid");
        assert!(ok.is_some());

        // Present but not understood: drop the record, not the RRSet.
        let dropped = endpoint_from_binding(&binding(
            1,
            "example.com",
            "svc.example.com",
            vec![RawParam::Mandatory(vec![7]), RawParam::Other(7)],
        ))
        .expect("an unsupported mandatory key is not malformed");
        assert_eq!(dropped, None, "RFC 9460 §8");

        // Declared mandatory and absent: an error.
        assert_eq!(
            endpoint_from_binding(&binding(
                1,
                "example.com",
                "svc.example.com",
                vec![RawParam::Mandatory(vec![3])],
            )),
            Err(SvcbLookupError::MandatoryKeyAbsent { key: 3 })
        );
    }

    #[test]
    fn every_modelled_param_reaches_the_endpoint_from_a_binding() {
        let ech = vec![0x00, 0x03, 0xfe, 0x0d, 0x01];
        let got = endpoint_from_binding(&binding(
            2,
            "example.com",
            "svc.example.com",
            vec![
                RawParam::Alpn(vec![b"h3".to_vec(), b"h2".to_vec()]),
                RawParam::NoDefaultAlpn,
                RawParam::Port(8443),
                RawParam::Ipv4Hint(vec!["192.0.2.1".parse().unwrap()]),
                RawParam::Ech(ech.clone()),
                RawParam::Ipv6Hint(vec!["2001:db8::1".parse().unwrap()]),
                RawParam::Other(31337),
            ],
        ))
        .expect("valid")
        .expect("usable");
        assert_eq!(got.priority, 2);
        assert_eq!(got.target, "svc.example.com");
        assert_eq!(got.alpn, vec![b"h3".to_vec(), b"h2".to_vec()]);
        assert_eq!(got.port, Some(8443));
        assert_eq!(got.ipv4hint, vec!["192.0.2.1".parse::<Ipv4Addr>().unwrap()]);
        assert_eq!(
            got.ipv6hint,
            vec!["2001:db8::1".parse::<Ipv6Addr>().unwrap()]
        );
        assert_eq!(
            got.ech_config_list.as_deref(),
            Some(ech.as_slice()),
            "the ECHConfigList arrives length-prefixed and stays that way — Windows hands it \
             over verbatim, the Unix decoder has the prefix restored, and this is the point \
             where the two must look identical"
        );
    }

    // ---- classification of what the FFI hands back ----------------------

    // ---- the header's counts against the body they describe -------------

    // ---- more than one record ------------------------------------------

    /// Every usable record in the RRSet reaches the caller, not just the
    /// first one the loop happens to find. Ordering is deliberately not
    /// asserted: `Resolve` promises none (see the `hclient-dns` module doc),
    /// and priority selection is the consumer's, so this checks the set.
    #[test]
    fn every_usable_record_in_an_rrset_reaches_the_caller() {
        let msg = response(
            "example.com",
            &[
                (
                    "example.com",
                    TYPE_HTTPS,
                    svcb_rdata(3, "third.example.com", &[]),
                ),
                (
                    "example.com",
                    TYPE_HTTPS,
                    svcb_rdata(1, "first.example.com", &[]),
                ),
                (
                    "example.com",
                    TYPE_HTTPS,
                    svcb_rdata(2, "second.example.com", &[]),
                ),
            ],
        );
        let got = endpoints(msg).expect("valid");
        let mut seen: Vec<(u16, &str)> = got
            .iter()
            .map(|e| (e.priority, e.target.as_str()))
            .collect();
        seen.sort_unstable();
        assert_eq!(
            seen,
            vec![
                (1, "first.example.com"),
                (2, "second.example.com"),
                (3, "third.example.com")
            ],
            "an RRSet is all of its usable records — dropping any of them would silently \
             narrow the choice a Happy Eyeballs consumer gets"
        );
    }

    // ---- the ECHConfigList's length prefix, at more than one length -----

    /// The prefix RFC 9460 §7.3 makes part of the SvcParamValue is written
    /// back on as a **two-byte, big-endian** length. One five-byte payload
    /// cannot tell that apart from a single byte, or from a constant: 300
    /// can, because its high byte is not zero and its low byte is 44.
    #[rstest]
    fn the_ech_length_prefix_is_two_big_endian_bytes_of_the_real_length(
        #[values(1, 5, 255, 300)] payload_len: usize,
    ) {
        let payload: Vec<u8> = (0..payload_len).map(|i| (i % 251) as u8).collect();
        let mut ech = u16::try_from(payload.len())
            .expect("under 64 KiB")
            .to_be_bytes()
            .to_vec();
        ech.extend_from_slice(&payload);
        let msg = one_record(1, "svc.example.com", &[(KEY_ECH, ech.clone())]);
        let got = endpoints(msg).expect("valid");
        assert_eq!(
            got[0].ech_config_list.as_deref(),
            Some(ech.as_slice()),
            "rustls reads an ECHConfigList as a TLS vector — a length that is short by a \
             byte, or written little-endian, fails inside rustls and not here"
        );
    }

    // ---- AliasMode meets `mandatory`, which only Windows can produce ----
    //
    // RFC 9460 §2.4.1 says SvcParams in an AliasMode record are ignored,
    // and `endpoint_from_binding` returns before it ever looks at
    // `mandatory`. On the Unix path the decoder refuses such a record
    // outright, so these two rules only meet on the Windows path — where
    // the OS parses the params and hands them over whatever the priority
    // says. That makes these the only tests that can reach the
    // interaction, and it is the one place "ignored" has to mean ignored
    // rather than "checked and then dropped".

    #[test]
    fn an_aliasmode_record_stays_usable_despite_a_mandatory_key_we_do_not_understand() {
        let got = endpoint_from_binding(&binding(
            0,
            "example.com",
            "alias.example.com",
            vec![RawParam::Mandatory(vec![7]), RawParam::Other(7)],
        ))
        .expect("valid")
        .expect(
            "RFC 9460 §2.4.1: the params are ignored, so nothing here can make it \
                 unusable",
        );
        assert_eq!(got.target, "alias.example.com");
    }

    #[test]
    fn an_aliasmode_record_with_a_mandatory_key_that_is_absent_is_not_malformed() {
        assert_matches!(
            endpoint_from_binding(&binding(
                0,
                "example.com",
                "alias.example.com",
                vec![RawParam::Mandatory(vec![3])],
            )),
            Ok(Some(_)),
            "the §8 check is about SvcParams, and §2.4.1 has already said there are none to \
             check — rejecting the RRSet over one would be strictly worse than ignoring it"
        );
    }

    /// **A known asymmetry, pinned so it is a decision rather than a
    /// surprise.** `endpoint_from_binding` walks the `mandatory` list in
    /// wire order and stops at the first key that is either absent (an
    /// error, which rejects the whole RRSet) or unrecognised (drop this
    /// record only). So one record that is both — naming an absent key AND
    /// an unrecognised one — resolves differently depending on which comes
    /// first in a list whoever wrote the record chose the order of.
    ///
    /// Both outcomes are safe in isolation: neither yields an endpoint from
    /// the offending record. What differs is the fate of the OTHER records
    /// in the RRSet, and that is worth knowing: a server (or anyone able to
    /// inject a record) can pick "reject everything" or "drop just this
    /// one" by ordering two numbers. RFC 9460 §8 reads as if the malformed
    /// check settles it — a record whose `mandatory` names an absent key is
    /// malformed, and §2.2 rejects the RRSet for a malformed record —
    /// which would mean checking every key for presence before acting on
    /// any of them.
    #[rstest]
    #[case::absent_first(vec![3, 7])]
    #[case::unrecognised_first(vec![7, 3])]
    fn the_mandatory_scan_stops_at_the_first_offending_key_in_wire_order(
        #[case] mandatory: Vec<u16>,
    ) {
        // Key 3 (port) is absent; key 7 (dohpath) is present and not one
        // this client acts on. Only the order differs between the cases.
        let got = endpoint_from_binding(&binding(
            1,
            "example.com",
            "svc.example.com",
            vec![RawParam::Mandatory(mandatory.clone()), RawParam::Other(7)],
        ));
        match mandatory[0] {
            3 => assert_eq!(
                got,
                Err(SvcbLookupError::MandatoryKeyAbsent { key: 3 }),
                "an absent key reached first rejects the whole RRSet"
            ),
            _ => assert_eq!(
                got,
                Ok(None),
                "an unrecognised key reached first drops this record and keeps the RRSet — \
                 the same record, the same two keys, a different blast radius"
            ),
        }
    }

    // ---- a record as a resolver hands it over ---------------------------

    /// The 61 RDATA octets of `cloudflare.com`'s HTTPS record, exactly as
    /// a resolver reports them.
    ///
    /// Captured twice from two platforms that share no code: through
    /// `res_query` on Linux, and through `DnsQuery_UTF8` on Windows 11 on
    /// 2026-09-01, where `wDataLength` is 61 and the record's union holds
    /// these bytes. They agreed byte for byte, which is the fact
    /// `system-resolver`'s Windows support matrix rests on and the reason
    /// one fixture serves every platform here.
    const REAL_HTTPS_RDATA: &str = concat!(
        "0001000001000602683302683200040008681084e5681085e500060020260647",
        "000000000000000000681084e5260647000000000000000000681085e5",
    );

    /// One record, decoded — the whole of this crate below its `Resolve`
    /// impl.
    fn from_rdata(
        owner: &str,
        ttl: u32,
        rdata: Vec<u8>,
    ) -> Result<Vec<SvcbEndpoint>, SvcbLookupError> {
        let record = Record::new(
            owner,
            TYPE_HTTPS,
            system_resolver::CLASS_IN,
            Duration::from_secs(u64::from(ttl)),
            rdata,
        );
        endpoints_from_records(std::slice::from_ref(&record))
    }

    /// **A real record, end to end.** The values are written out rather
    /// than compared against another run of the same code: this is the one
    /// test here whose fixture came off a wire, so it is the one place an
    /// expectation is worth stating in full.
    #[test]
    fn a_real_record_becomes_the_endpoint_the_origin_published() {
        let found = from_rdata("cloudflare.com", 300, hex(REAL_HTTPS_RDATA))
            .expect("a real record decodes");
        let endpoint = found.first().expect("one endpoint");
        // A root TargetName means the owner name, RFC 9460 §2.5.
        assert_eq!(endpoint.target, "cloudflare.com");
        assert_eq!(endpoint.alpn, vec![b"h3".to_vec(), b"h2".to_vec()]);
        assert_eq!(endpoint.ipv4hint.len(), 2);
        assert_eq!(endpoint.ipv6hint.len(), 2);
        assert_eq!(endpoint.ttl, Some(Duration::from_secs(300)));
    }

    /// **The TTL is not in the RDATA**, so it is the one field of the
    /// binding that has to be read off the `Record` and carried across by
    /// hand — which is what makes it worth its own test whichever decoder
    /// is underneath.
    #[test]
    fn a_records_endpoint_carries_the_resolvers_own_ttl() {
        let found =
            from_rdata("cloudflare.com", 45, hex(REAL_HTTPS_RDATA)).expect("the record decodes");
        assert_eq!(
            found.first().expect("one endpoint").ttl,
            Some(Duration::from_secs(45))
        );
    }

    /// **The control that says the RDATA is decoded rather than trusted.**
    /// Bytes that are not an SVCB record must come back as a refusal from
    /// the decoder; a path that passed them through would be green on
    /// every other test in this section.
    #[test]
    fn rdata_that_is_not_an_svcb_record_is_refused() {
        let refused = from_rdata("example.com", 300, vec![0xff; 3]);
        assert_matches!(refused, Err(SvcbLookupError::Malformed(_)));
    }

    /// **An RRSet far larger than a DNS message could carry is answered
    /// rather than refused**, which is the capability the envelope cost.
    /// This test is the previous one inverted: the same thousand records
    /// used to exceed the 65535 octets a message is framed by, and
    /// `AnswerTooLarge` was this crate's own refusal for a limit that was
    /// never the resolver's. Reading RDATA one record at a time, the limit
    /// has no subject — a platform that hands over a thousand records
    /// hands over a thousand endpoints.
    ///
    /// The variant went with it. Nothing else raised it, and an error
    /// nothing can raise is worse than no error, because a caller writes a
    /// match arm for a case that cannot happen.
    #[test]
    fn an_rrset_too_large_for_a_dns_message_is_answered_rather_than_refused() {
        let one = Record::new(
            "example.com",
            TYPE_HTTPS,
            system_resolver::CLASS_IN,
            Duration::from_secs(300),
            hex(REAL_HTTPS_RDATA),
        );
        let many = vec![one; 1000];
        assert_eq!(
            endpoints_from_records(&many)
                .expect("a large RRSet decodes")
                .len(),
            1000
        );
    }

    /// **A record of another type is stepped over, not rejected.** A CNAME
    /// beside the answer is ordinary, and on the platforms that hand over
    /// a whole message it arrives here; refusing the RRSet because of it
    /// would make every aliased name unreachable over h3.
    #[test]
    fn a_record_of_another_type_beside_the_answer_is_stepped_over() {
        let cname = Record::new(
            "example.com",
            5,
            system_resolver::CLASS_IN,
            Duration::from_secs(300),
            vec![0x00],
        );
        let https = Record::new(
            "example.com",
            TYPE_HTTPS,
            system_resolver::CLASS_IN,
            Duration::from_secs(300),
            svcb_rdata(1, "", &[]),
        );
        let found = endpoints_from_records(&[cname, https]).expect("decodes");
        assert_eq!(found.len(), 1);
    }

    /// And a set with no HTTPS record at all is an empty answer rather
    /// than an error — the control for the test above, and the shape a
    /// caller sees for a name that publishes none.
    #[test]
    fn records_of_only_other_types_are_an_empty_answer() {
        let cname = Record::new(
            "example.com",
            5,
            system_resolver::CLASS_IN,
            Duration::from_secs(300),
            vec![0x00],
        );
        assert_eq!(endpoints_from_records(&[cname]), Ok(Vec::new()));
    }
}
