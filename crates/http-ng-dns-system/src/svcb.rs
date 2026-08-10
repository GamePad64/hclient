//! RFC 9460 client semantics over a decoded DNS message.
//!
//! **The wire parsing is not done here.** `dns-message-parser` decodes the
//! response — a crate with no `unsafe` anywhere in its `src`, whose
//! decoder returns `DecodeResult` on every path and whose name
//! decompression terminates by tracking visited offsets in a `HashSet`
//! (`decode/domain_name.rs`, `EndlessRecursion` / `MaxRecursion`). That
//! removes the highest-risk code this task would otherwise have
//! contained: hand-rolled bounds checks over attacker-chosen bytes. What
//! is left in this module is the part a DNS decoder correctly refuses to
//! do — deciding, per RFC 9460, which of the decoded records a *client*
//! may act on, and what the shape of "no records" is.
//!
//! Verified against the same byte vectors that used to test the
//! hand-written parser, because the question they answer has only moved,
//! not gone away: a truncated answer, an `RDLENGTH` that overruns the
//! message, and a compression-pointer cycle all still have to become an
//! `Err` here rather than a panic or a hang. See the tests at the bottom —
//! they now assert about *this crate's* behaviour when the decoder
//! refuses, which is exactly the seam that could still be got wrong.
//!
//! **What is still parsed by hand, and why it has to be.** The twelve-byte
//! header, in `read_header`. `res_query` reports the ordinary "this name
//! has no HTTPS record" case as a *failure*, and on that path it returns
//! no length — so all that can be trusted is the fixed-size header, and
//! `Dns::decode` cannot read one on its own (measured: given exactly
//! twelve bytes it fails with "not enough bytes ... offset 13", because it
//! goes on to look for the question section). Twenty lines of bit
//! twiddling with tests around them is the whole of it.
#![forbid(unsafe_code)]

use crate::sys::SvcbLookupError;
use http_ng_dns::SvcbEndpoint;

#[allow(
    unused_imports,
    reason = "only the res_query backend calls this; see the `wire` module's own note"
)]
pub(crate) use wire::endpoints_from_answer;

/// The RFC 9460 client semantics, and this crate's thin wrapper over them.
///
/// `RawBinding`, `RawParam` and the decision procedure over them used to
/// live in this file. They moved to `http_ng_dns::svcb` when
/// `http-ng-dns-doh` became a third backend that has to apply exactly the
/// same rules to exactly the same decoded record — the drift this file's
/// own doc comment warned about, one crate further out. Nothing about the
/// rules changed in the move; what stayed here is the mapping from the
/// shared error into this crate's own taxonomy, so that a caller of
/// `http-ng-dns-system` still sees a single `SvcbLookupError`.
#[allow(
    unused_imports,
    reason = "`RawParam` is built by the Windows backend and by this file's tests; a Unix build compiles neither, exactly as for `endpoints_from_answer` just above"
)]
pub(crate) use http_ng_dns::svcb::{RawBinding, RawParam};

/// [`http_ng_dns::svcb::endpoint_from_binding`], with its error mapped into
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
    http_ng_dns::svcb::endpoint_from_binding(binding).map_err(|e| match e {
        http_ng_dns::svcb::SvcbRecordError::MandatoryKeyAbsent { key } => {
            SvcbLookupError::MandatoryKeyAbsent { key }
        }
    })
}

/// The raw-wire path: everything that turns a DNS response **message**
/// into endpoints.
///
/// Only the `res_query` backend produces such a message; Windows is handed
/// records the OS has already parsed and goes straight to `RawBinding`. So
/// on a Windows build nothing in here is reachable, which is what the
/// `dead_code` allowance is for — one on the module rather than eight on
/// its items, and deliberately **not** a `#[cfg]`: a second copy of `sys`'s
/// target list is exactly the drift that single `#[cfg]` pair exists to
/// prevent. The same reasoning leaves `dns-message-parser` an
/// unconditional dependency; a `[target.'cfg(...)']` entry for it would be
/// a third copy of the same list, and a few seconds of Windows build time
/// is the cheaper of the two.
#[allow(
    dead_code,
    reason = "only the res_query backend feeds a raw DNS message through this; a build compiles exactly one backend, and repeating sys's target list here would reintroduce the drift its single #[cfg] pair exists to prevent"
)]
mod wire {
    use super::endpoint_from_binding;
    use crate::sys::{RawAnswer, SvcbLookupError};
    use bytes::Bytes;
    use dns_message_parser::Dns;
    use dns_message_parser::rr::RR;
    use http_ng_dns::SvcbEndpoint;
    use http_ng_dns::svcb::binding_from_decoded;

    /// RFC 1035 §4.1.1 — `ID`, flags, and the four section counts.
    const HEADER_LEN: usize = 12;
    /// RFC 1035 §4.1.1 / RFC 6895 §2.3.
    const RCODE_NOERROR: u8 = 0;
    const RCODE_NXDOMAIN: u8 = 3;

    /// RFC 1035 §4.1.1, as much of it as this crate acts on.
    ///
    /// Read by hand rather than through `Dns::decode` because the FFI's
    /// failure path yields twelve bytes and no length — see the module doc.
    /// The same function serves the full-message path too, so there is one
    /// classification with one set of tests, rather than one rule for a header
    /// and a second, differently-worded one for a message.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub(crate) struct Header {
        /// `QR` — set on a response. The one bit that separates "the resolver
        /// received an answer" from "the buffer we zeroed is still zeroed",
        /// which is how a failed `res_query` is classified at all.
        pub(crate) is_response: bool,
        /// `TC` — the answer did not fit and libc's own TCP retry did not
        /// replace it. A truncated answer is not a complete RRSet.
        pub(crate) truncated: bool,
        pub(crate) rcode: u8,
        /// Only ever compared against zero, never used as a bound.
        pub(crate) ancount: u16,
    }

    /// The twelve-byte header, or `HeaderTruncated` if there is not one.
    pub(crate) fn read_header(msg: &[u8]) -> Result<Header, SvcbLookupError> {
        if msg.len() < HEADER_LEN {
            return Err(SvcbLookupError::HeaderTruncated { got: msg.len() });
        }
        Ok(Header {
            is_response: msg[2] & 0x80 != 0,
            truncated: msg[2] & 0x02 != 0,
            rcode: msg[3] & 0x0F,
            ancount: u16::from_be_bytes([msg[6], msg[7]]),
        })
    }

    /// Turns what the FFI handed back into endpoints, or into the reason there
    /// are none.
    ///
    /// This is where "the resolver asked and found nothing" is kept apart from
    /// "the resolver could not ask" — the distinction `Resolve::supports_svcb`
    /// exists for, applied one level down to a single query. An empty `Vec` is
    /// the first; every `Err` is the second.
    pub(crate) fn endpoints_from_answer(
        answer: &RawAnswer,
    ) -> Result<Vec<SvcbEndpoint>, SvcbLookupError> {
        let msg: &[u8] = match answer {
            // No backend on this target. Empty, not an error: `supports_svcb()`
            // is `false` here and the pair says "absent capability".
            RawAnswer::NotSupported => return Ok(Vec::new()),
            RawAnswer::HeaderOnly(header) => header,
            RawAnswer::Message(msg) => msg,
        };
        let header = read_header(msg)?;
        if !header.is_response {
            return Err(SvcbLookupError::NoResponse);
        }
        if header.truncated {
            return Err(SvcbLookupError::Truncated);
        }
        match header.rcode {
            RCODE_NOERROR => {}
            // The name does not exist, said by an authority. A definitive
            // "there are no HTTPS records", not a failure to ask — and the
            // A/AAAA lookup for the same name reports the missing name in its
            // own stream, where a caller is actually looking for it.
            RCODE_NXDOMAIN => return Ok(Vec::new()),
            rcode => return Err(SvcbLookupError::ResponseCode { rcode }),
        }
        if let RawAnswer::HeaderOnly(_) = answer {
            // `res_query` only reports failure on a NOERROR response when the
            // answer section is empty, so this is the ordinary "no HTTPS
            // record for this name" case and there is nothing to decode.
            //
            // If a libc ever breaks that contract it must not become a silent
            // under-report: the records are provably there and provably
            // unreadable, because the failure path returns no length to bound
            // them with. That is an error, and a distinct one.
            return if header.ancount == 0 {
                Ok(Vec::new())
            } else {
                Err(SvcbLookupError::LengthUnavailable {
                    ancount: header.ancount,
                })
            };
        }

        // `Bytes::copy_from_slice` rather than a move: the caller owns the
        // buffer and this is the only allocation the decode path adds. A
        // 4 KiB copy per SVCB lookup is not worth restructuring the FFI's
        // ownership for.
        let dns = Dns::decode(Bytes::copy_from_slice(msg)).map_err(SvcbLookupError::Malformed)?;

        // RFC 9460 §2.2: "If any RRs are malformed, the client MUST reject the
        // entire RRSet and fall back to non-SVCB connection establishment."
        // That is what the `?` above already does, and it is worth naming:
        // `Dns::decode` fails the whole message on one bad record, so there is
        // no path here that keeps the records that happened to parse. Keeping
        // them would let anyone able to inject one malformed record steer a
        // client onto whichever ones survived.
        let mut found = Vec::new();
        for rr in &dns.answers {
            // Records of other types in the answer section — a CNAME chain,
            // most commonly — are stepped over, not rejected.
            if let RR::HTTPS(binding) = rr
                && let Some(endpoint) = endpoint_from_binding(&binding_from_decoded(binding))?
            {
                found.push(endpoint);
            }
        }
        Ok(found)
    }
}

#[cfg(test)]
mod tests {
    use super::wire::{Header, read_header};
    use super::*;
    use crate::sys::RawAnswer;
    use assert_matches::assert_matches;
    use dns_message_parser::rr::ServiceParameter;
    use rstest::rstest;
    // Named here rather than at the top of the file since the RFC 9460
    // client semantics moved to `http_ng_dns::svcb`: nothing outside these
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

    fn hex(s: &str) -> Vec<u8> {
        assert!(s.len().is_multiple_of(2), "hex needs whole bytes");
        (0..s.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&s[i..i + 2], 16).expect("hex digit"))
            .collect()
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

    /// A NOERROR response to one HTTPS question, carrying the given records
    /// verbatim. Owner names are written out in full rather than
    /// compressed, so a test that wants compression has to ask for it.
    fn response(qname: &str, answers: &[(&str, u16, Vec<u8>)]) -> Vec<u8> {
        let mut m = hex("123481800001");
        m.extend_from_slice(
            &u16::try_from(answers.len())
                .expect("few answers")
                .to_be_bytes(),
        );
        m.extend_from_slice(&0u16.to_be_bytes());
        m.extend_from_slice(&0u16.to_be_bytes());
        m.extend(name_wire(qname));
        m.extend_from_slice(&TYPE_HTTPS.to_be_bytes());
        m.extend_from_slice(&1u16.to_be_bytes());
        for (owner, rtype, rdata) in answers {
            m.extend(name_wire(owner));
            m.extend_from_slice(&rtype.to_be_bytes());
            m.extend_from_slice(&1u16.to_be_bytes());
            m.extend_from_slice(&300u32.to_be_bytes());
            m.extend_from_slice(
                &u16::try_from(rdata.len())
                    .expect("rdata fits")
                    .to_be_bytes(),
            );
            m.extend_from_slice(rdata);
        }
        m
    }

    fn one_record(priority: u16, target: &str, params: &[(u16, Vec<u8>)]) -> Vec<u8> {
        response(
            "example.com",
            &[(
                "example.com",
                TYPE_HTTPS,
                svcb_rdata(priority, target, params),
            )],
        )
    }

    fn endpoints(msg: Vec<u8>) -> Result<Vec<SvcbEndpoint>, SvcbLookupError> {
        endpoints_from_answer(&RawAnswer::Message(msg))
    }

    /// Captured verbatim from this crate's own `res_query` path against the
    /// system resolver (`cloudflare.com`, RR type 65, 116 bytes on the
    /// wire), and kept through the switch to `dns-message-parser` because
    /// what it proves has only moved: that a real answer — compression
    /// pointer in the owner name, root TargetName, an EDNS0 `OPT` record in
    /// the additional section — survives the whole path and lands in
    /// `SvcbEndpoint` with the right values.
    const REAL_CLOUDFLARE_ANSWER: &str = concat!(
        "825d818000010001000000010a636c6f7564666c61726503636f6d0000410001",
        "c00c004100010000012c003d000100000100060268330268320004000868108",
        "4e5681085e500060020260647000000000000000000681084e5260647000000",
        "000000000000681085e5000029ffd6000000000000",
    );

    // ---- the dependency's one sharp edge -------------------------------

    /// **`ServiceParameter`'s `PartialEq` compares only the key number.**
    /// This is not a guess about upstream, it is asserted here so that the
    /// day it changes, this test fails and the reason the rest of this
    /// module avoids `assert_eq!` on whole variants can be revisited
    /// deliberately rather than found again by accident.
    ///
    /// The consequence is the point: any test written as `assert_eq!(param,
    /// ServiceParameter::ALPN { alpn_ids: vec!["h2"] })` passes **without
    /// checking a single value** — it looks like it compares the ALPN list
    /// and actually compares the number `1`. Every assertion in this module
    /// therefore reaches into the extracted `SvcbEndpoint` fields
    /// (`alpn`, `port`, `ipv4hint`, `ech_config_list`), which are ordinary
    /// `Vec`s and `Option`s with ordinary equality.
    #[test]
    fn service_parameter_equality_ignores_values_which_is_why_tests_assert_on_fields() {
        let h2 = ServiceParameter::ALPN {
            alpn_ids: vec!["h2".to_owned()],
        };
        let h3 = ServiceParameter::ALPN {
            alpn_ids: vec!["h3".to_owned()],
        };
        assert_eq!(
            h2, h3,
            "upstream compares SvcParamKey numbers only — if this ever fails, upstream fixed \
             its PartialEq and the field-by-field style in this module can be revisited"
        );
    }

    // ---- a real answer, end to end -------------------------------------

    #[test]
    fn parses_a_real_https_answer_captured_from_the_system_resolver() {
        let msg = hex(REAL_CLOUDFLARE_ANSWER);
        assert_eq!(msg.len(), 116, "the captured answer was 116 bytes");
        let got = endpoints(msg).expect("a real answer must parse");
        assert_eq!(got.len(), 1);
        let ep = &got[0];
        assert_eq!(ep.priority, 1);
        assert_eq!(
            ep.target, "cloudflare.com",
            "TargetName was `.`, which RFC 9460 §2.5 defines as the owner name — and the \
             owner here is itself a compression pointer back to the question"
        );
        assert_eq!(
            ep.alpn,
            vec![b"h3".to_vec(), b"h2".to_vec()],
            "the values, not the key: see service_parameter_equality_ignores_values"
        );
        assert_eq!(ep.port, None);
        assert_eq!(
            ep.ipv4hint,
            vec![
                "104.16.132.229".parse::<Ipv4Addr>().unwrap(),
                "104.16.133.229".parse::<Ipv4Addr>().unwrap()
            ]
        );
        assert_eq!(
            ep.ipv6hint,
            vec![
                "2606:4700::6810:84e5".parse::<Ipv6Addr>().unwrap(),
                "2606:4700::6810:85e5".parse::<Ipv6Addr>().unwrap()
            ]
        );
        assert_eq!(ep.ech_config_list, None);
    }

    // ---- what happens when the decoder refuses -------------------------
    //
    // These are the vectors that used to test a hand-written parser. They
    // now test the seam that replaced it: that a refusal becomes an `Err`
    // from this crate rather than a panic, a hang, or a silently empty
    // result. That question did not go away with the parser.

    #[test]
    fn a_message_cut_mid_record_is_an_error_not_a_silently_short_answer() {
        let full = one_record(1, "svc.example.com", &[(KEY_ALPN, vec![2, b'h', b'2'])]);
        let err = endpoints(full[..full.len() - 4].to_vec()).expect_err("half a record");
        assert_matches!(
            err,
            SvcbLookupError::Malformed(_),
            "expected a decode failure"
        );
    }

    #[test]
    fn an_rdlength_that_overruns_the_message_is_an_error() {
        let mut msg = one_record(1, "svc.example.com", &[]);
        let rdata_len = svcb_rdata(1, "svc.example.com", &[]).len();
        let rdlength_at = msg.len() - rdata_len - 2;
        msg[rdlength_at] = 0xff;
        msg[rdlength_at + 1] = 0xff;
        let err = endpoints(msg).expect_err("65535 bytes of RDATA are not there");
        assert_matches!(
            err,
            SvcbLookupError::Malformed(_),
            "the length claimed on the wire must not be believed"
        );
    }

    /// The one property this crate genuinely depends on the decoder for,
    /// so it is asserted rather than assumed: a compression-pointer cycle
    /// terminates. `dns-message-parser` tracks visited offsets in a
    /// `HashSet` and returns `EndlessRecursion`; a decoder that instead
    /// looped would hang this test rather than fail it, which is itself
    /// the signal.
    #[test]
    fn a_compression_pointer_cycle_terminates_as_an_error() {
        let mut msg = hex(REAL_CLOUDFLARE_ANSWER);
        msg[12] = 0xc0;
        msg[13] = 0x0c; // the question name points at itself
        let err = endpoints(msg).expect_err("a cycle is not a name");
        assert_matches!(
            err,
            SvcbLookupError::Malformed(_),
            "expected the decoder's recursion guard"
        );

        // And a two-hop cycle, which a naive "pointers must point
        // backwards" rule would accept and loop on forever.
        let mut msg = vec![0u8; 64];
        msg[..12].copy_from_slice(&hex(REAL_CLOUDFLARE_ANSWER)[..12]);
        msg[12] = 0xc0;
        msg[13] = 20;
        msg[20] = 0xc0;
        msg[21] = 12;
        assert_matches!(
            endpoints(msg).expect_err("a two-hop cycle is not a name"),
            SvcbLookupError::Malformed(_)
        );
    }

    #[test]
    fn a_message_shorter_than_the_header_is_an_error() {
        assert_eq!(
            endpoints(vec![0u8; 11]),
            Err(SvcbLookupError::HeaderTruncated { got: 11 })
        );
        assert_eq!(
            endpoints(Vec::new()),
            Err(SvcbLookupError::HeaderTruncated { got: 0 })
        );
    }

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

    /// Pins a divergence from RFC 9460, so it is a known cost rather than a
    /// surprise. §2.4.1 says recipients MUST *ignore* SvcParams found in an
    /// AliasMode record — i.e. the record stays usable. The decoder reads
    /// SvcParams only when `priority != 0`, so such a record leaves bytes
    /// unconsumed and fails the whole message instead.
    ///
    /// Accepted, for two reasons worth stating: it only triggers for a
    /// server already violating §2.4.1's "SHOULD be empty", and it fails
    /// safe — the RRSet is rejected and the caller falls back to non-SVCB,
    /// rather than connecting somewhere on a record nobody agrees about.
    /// If this test ever fails, upstream started honouring §2.4.1 and
    /// `endpoint_from_binding`'s AliasMode branch becomes reachable with
    /// real parameters for the first time.
    #[test]
    fn an_aliasmode_record_carrying_params_is_rejected_by_the_decoder() {
        let msg = one_record(
            0,
            "alias.example.com",
            &[
                (KEY_ALPN, vec![2, b'h', b'2']),
                (KEY_PORT, vec![0x01, 0xbb]),
            ],
        );
        let err = endpoints(msg).expect_err("stricter than RFC 9460 §2.4.1, deliberately");
        assert_matches!(
            err,
            SvcbLookupError::Malformed(_),
            "expected the decoder to refuse the unconsumed params"
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

    #[test]
    fn a_name_with_no_https_records_is_an_empty_result_not_an_error() {
        // The shape `res_query` actually produces for this, measured on
        // glibc 2.43 against `ftp.gnu.org`: ret=-1 with QR set, RCODE 0 and
        // ANCOUNT 0. It is the commonest outcome of an HTTPS query and must
        // never look like a broken resolver.
        let header = hex("03e2818000010000000100010000000000000000")[..12]
            .try_into()
            .unwrap();
        assert_eq!(
            endpoints_from_answer(&RawAnswer::HeaderOnly(header)),
            Ok(Vec::new())
        );
    }

    #[test]
    fn nxdomain_is_an_empty_result_not_an_error() {
        // Measured shape for a name that does not exist: ret=-1, QR set,
        // RCODE 3.
        let header = hex("f18585a300010000000000010000000000000000")[..12]
            .try_into()
            .unwrap();
        assert_eq!(
            endpoints_from_answer(&RawAnswer::HeaderOnly(header)),
            Ok(Vec::new()),
            "the name not existing is a definitive `no HTTPS records`, not a failure to ask — \
             the A/AAAA stream is where a caller looks for the missing name"
        );
    }

    #[test]
    fn a_failure_rcode_is_an_error_not_an_empty_result() {
        for rcode in [1u8, 2, 4, 5] {
            let mut header = [0u8; 12];
            header[2] = 0x81;
            header[3] = rcode;
            assert_eq!(
                endpoints_from_answer(&RawAnswer::HeaderOnly(header)),
                Err(SvcbLookupError::ResponseCode { rcode }),
                "SERVFAIL and friends must not be indistinguishable from `no records`"
            );
        }
    }

    #[test]
    fn nothing_arriving_is_an_error_not_an_empty_result() {
        // The buffer the FFI hands back when no response arrived: all
        // zeros, so QR is clear. It must not be read as a NOERROR answer
        // with no records.
        assert_eq!(
            endpoints_from_answer(&RawAnswer::HeaderOnly([0u8; 12])),
            Err(SvcbLookupError::NoResponse),
            "a zeroed buffer is not a response with RCODE 0"
        );
    }

    #[test]
    fn a_truncated_answer_is_an_error_not_a_partial_rrset() {
        let mut header = [0u8; 12];
        header[2] = 0x82; // QR | TC
        assert_eq!(
            endpoints_from_answer(&RawAnswer::HeaderOnly(header)),
            Err(SvcbLookupError::Truncated)
        );
    }

    #[test]
    fn a_failed_call_whose_header_claims_records_is_an_error_not_a_silent_zero() {
        let mut header = [0u8; 12];
        header[2] = 0x81;
        header[7] = 2; // ANCOUNT = 2, with no length to read them by
        assert_eq!(
            endpoints_from_answer(&RawAnswer::HeaderOnly(header)),
            Err(SvcbLookupError::LengthUnavailable { ancount: 2 }),
            "records that provably exist and provably cannot be read are not `no records`"
        );
    }

    #[test]
    fn a_target_without_a_backend_reports_no_records_and_no_error() {
        assert_eq!(
            endpoints_from_answer(&RawAnswer::NotSupported),
            Ok(Vec::new()),
            "paired with supports_svcb() == false, this is an absent capability, not a failure"
        );
    }

    // ---- the twelve bytes that are still parsed by hand ----------------
    //
    // `read_header` is the only wire parsing left in this crate, and the
    // reason it exists at all is measured two tests below. Everything above
    // reaches it through `endpoints_from_answer`, which can only observe it
    // through the outcome; these read the four fields directly, because a
    // header field that lands in the wrong variable is invisible from a
    // distance whenever some other field happens to reject the message
    // first.
    //
    // `assert_eq!` on a whole `Header` is safe in a way it is NOT for a
    // `ServiceParameter` (see the top of this module): `Header` is four
    // primitives with a derived `PartialEq`, so every field is genuinely
    // compared.

    /// `ID`, flags, and the four RFC 1035 §4.1.1 counts, written out so a
    /// case below can put a value in one field and zero everywhere else.
    fn header_bytes(flags_hi: u8, flags_lo: u8, qdcount: u16, ancount: u16) -> Vec<u8> {
        let mut out = vec![0x12, 0x34, flags_hi, flags_lo];
        out.extend_from_slice(&qdcount.to_be_bytes());
        out.extend_from_slice(&ancount.to_be_bytes());
        out.extend_from_slice(&0u16.to_be_bytes()); // NSCOUNT
        out.extend_from_slice(&0u16.to_be_bytes()); // ARCOUNT
        out
    }

    #[rstest]
    // The header of the captured answer, so at least one case is a shape a
    // resolver really produced rather than one this file invented.
    #[case::a_real_answer(
        hex(REAL_CLOUDFLARE_ANSWER)[..12].to_vec(),
        Header { is_response: true, truncated: false, rcode: 0, ancount: 1 }
    )]
    // The buffer the FFI zeroed and nothing was written into. Every field
    // false or zero — and `is_response` false is the only thing that keeps
    // it from reading as a perfectly good NOERROR answer with no records.
    #[case::nothing_arrived(
        vec![0u8; 12],
        Header { is_response: false, truncated: false, rcode: 0, ancount: 0 }
    )]
    // `AA` without `QR`: only bit 0x80 means "response".
    #[case::authoritative_but_not_a_response(
        header_bytes(0x40, 0x00, 0, 0),
        Header { is_response: false, truncated: false, rcode: 0, ancount: 0 }
    )]
    // `RD` is the neighbouring bit to `TC` and is set on nearly every real
    // query; reading it as truncation would reject almost every answer.
    #[case::recursion_desired_is_not_truncation(
        header_bytes(0x01, 0x00, 0, 0),
        Header { is_response: false, truncated: false, rcode: 0, ancount: 0 }
    )]
    #[case::truncated(
        header_bytes(0x02, 0x00, 0, 0),
        Header { is_response: false, truncated: true, rcode: 0, ancount: 0 }
    )]
    // RCODE shares its byte with `RA` and the `Z`/`AD`/`CD` bits, all of
    // which are set here. Unmasked, this reads as RCODE 243 and NXDOMAIN
    // stops being NXDOMAIN.
    #[case::the_rcode_is_the_low_nibble_only(
        header_bytes(0x82, 0xf3, 1, 0),
        Header { is_response: true, truncated: true, rcode: 3, ancount: 0 }
    )]
    // ANCOUNT is bytes 6..8, big-endian. Swapping the pair gives 513 and
    // reading QDCOUNT instead gives 1, so this one case rules out both.
    #[case::ancount_is_the_third_count_and_network_order(
        header_bytes(0x81, 0x80, 1, 258),
        Header { is_response: true, truncated: false, rcode: 0, ancount: 258 }
    )]
    // The mirror image: a huge QDCOUNT with no answers must still be no
    // answers.
    #[case::a_large_qdcount_is_not_an_ancount(
        header_bytes(0x81, 0x80, 0xffff, 0),
        Header { is_response: true, truncated: false, rcode: 0, ancount: 0 }
    )]
    fn the_hand_parsed_header_reads_each_field_from_its_own_bits(
        #[case] bytes: Vec<u8>,
        #[case] expected: Header,
    ) {
        assert_eq!(read_header(&bytes), Ok(expected));
    }

    /// Anything shorter than the fixed twelve bytes is refused with the
    /// length it did have, and the boundary is exact: eleven is short,
    /// twelve is a header. The bound guards four indexed reads (`msg[2]`,
    /// `msg[3]`, `msg[6]`, `msg[7]`), so an off-by-one here is a panic on a
    /// short answer, not a wrong value.
    #[rstest]
    fn fewer_than_twelve_bytes_is_never_a_header(#[values(0, 1, 6, 8, 11)] len: usize) {
        assert_eq!(
            read_header(&vec![0u8; len]),
            Err(SvcbLookupError::HeaderTruncated { got: len })
        );
    }

    #[test]
    fn exactly_twelve_bytes_is_a_header() {
        assert_matches!(read_header(&[0u8; 12]), Ok(_));
    }

    /// Why `read_header` exists at all, pinned rather than left in a
    /// comment: `Dns::decode` cannot read a bare header, so the FFI's
    /// failure path — which yields twelve bytes and no length — has nothing
    /// else to be classified by.
    ///
    /// If this ever starts succeeding, the hand-written header parser can
    /// be deleted, and that is a decision worth being told about.
    #[test]
    fn the_decoder_cannot_read_a_bare_header_which_is_why_one_is_parsed_by_hand() {
        // One question, which is the shape a failed `res_query` really
        // leaves behind: the query it echoed, and no answer section.
        let bare = header_bytes(0x81, 0x80, 1, 0);
        assert_eq!(bare.len(), 12);
        assert_matches!(
            endpoints(bare),
            Err(SvcbLookupError::Malformed(_)),
            "the decoder goes on to look for the question the header promised, so twelve \
             bytes are not a message to it — only to `read_header`"
        );

        // And the other direction, which is the more dangerous half: a
        // header claiming NO questions decodes cleanly into a message with
        // nothing in it. Routing the FFI's failure path through
        // `Dns::decode` would therefore not simply fail — it would return
        // "no HTTPS records" for whatever a header said, ANCOUNT included.
        // `endpoints_from_answer` never lets that happen, because
        // `RawAnswer::HeaderOnly` is classified from the header's own
        // fields and never handed to the decoder at all.
        assert_matches!(endpoints(header_bytes(0x81, 0x80, 0, 0)), Ok(records) if records.is_empty());
    }

    /// The header is read BEFORE the body, on the full-message path too.
    ///
    /// Both of these carry the captured answer's hundred-odd valid bytes
    /// and differ from it by one bit, so a decode that ran first would
    /// happily produce the endpoint and the flag would never be consulted.
    #[rstest]
    #[case::qr_clear(0x81 & !0x80, SvcbLookupError::NoResponse)]
    #[case::tc_set(0x81 | 0x02, SvcbLookupError::Truncated)]
    fn a_flag_in_the_header_rejects_an_otherwise_decodable_answer(
        #[case] flags_hi: u8,
        #[case] expected: SvcbLookupError,
    ) {
        let mut msg = hex(REAL_CLOUDFLARE_ANSWER);
        assert!(
            endpoints(msg.clone()).is_ok(),
            "the unmodified answer must parse, or this case proves nothing"
        );
        msg[2] = flags_hi;
        assert_eq!(endpoints(msg), Err(expected));
    }

    // ---- the header's counts against the body they describe -------------

    /// ANCOUNT is never used as a bound in this crate — it is only compared
    /// against zero — so the question these two answer is what the decoder
    /// does when the count and the section disagree, in both directions.
    /// Both must be refusals: a count larger than the records present is
    /// the shape that would index past a section, and a count smaller than
    /// them is the shape that would let an injected record ride along
    /// unread behind a header claiming fewer.
    #[test]
    fn an_ancount_larger_than_the_records_present_is_refused() {
        let mut msg = one_record(1, "svc.example.com", &[]);
        msg[6..8].copy_from_slice(&3u16.to_be_bytes());
        assert_matches!(
            endpoints(msg),
            Err(SvcbLookupError::Malformed(_)),
            "three records claimed, one supplied — the decoder must run out of bytes rather \
             than read past the section"
        );
    }

    #[test]
    fn an_ancount_smaller_than_the_records_present_is_refused_not_silently_truncated() {
        let good = svcb_rdata(1, "good.example.com", &[]);
        let extra = svcb_rdata(2, "extra.example.com", &[]);
        let mut msg = response(
            "example.com",
            &[
                ("example.com", TYPE_HTTPS, good),
                ("example.com", TYPE_HTTPS, extra),
            ],
        );
        msg[6..8].copy_from_slice(&1u16.to_be_bytes());
        assert_matches!(
            endpoints(msg),
            Err(SvcbLookupError::Malformed(_)),
            "a message with bytes left over after its own counts is not a message with one \
             record in it — accepting the prefix would be the whole RRSet decided by \
             whoever wrote the header"
        );
    }

    // ---- more than one record ------------------------------------------

    /// Every usable record in the RRSet reaches the caller, not just the
    /// first one the loop happens to find. Ordering is deliberately not
    /// asserted: `Resolve` promises none (see the `http-ng-dns` module doc),
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

    /// The other half of the sharp edge at the top of this module:
    /// `ServiceParameter` hashes by key as well as comparing by it, so a
    /// `HashSet` of them — which is what the decoder returns
    /// (`ServiceBinding::parameters`) — holds at most one parameter per
    /// key, and a set-based assertion proves nothing about a value either.
    #[test]
    fn service_parameter_hashes_by_key_so_a_set_of_them_holds_one_per_key() {
        use std::collections::HashSet;

        let mut set = HashSet::new();
        set.insert(ServiceParameter::ALPN {
            alpn_ids: vec!["h2".to_owned()],
        });
        set.insert(ServiceParameter::ALPN {
            alpn_ids: vec!["h3".to_owned()],
        });
        assert_eq!(
            set.len(),
            1,
            "upstream hashes SvcParamKey numbers only — if this ever fails, upstream fixed \
             its Hash impl and the field-by-field style in this module can be revisited"
        );
    }
}
