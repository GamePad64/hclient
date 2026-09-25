//! The DNS message, in and out. RFC 1035 framing, RFC 8484 carriage.
//!
//! **Nothing here parses bytes by hand.** `domain` does the decoding —
//! the same crate `hclient-dns-system` reads RDATA with, which is the
//! point: this workspace had **two** DNS decoders, and the reason was
//! granularity rather than quality. `dns-message-parser` exposes decoding
//! at the **message** level and nothing smaller, which is what this
//! module wants and is exactly what `system-resolver` cannot supply;
//! `domain` does both ([`Message::from_octets`] here,
//! [`Https::parse`](domain::rdata::svcb::Https::parse) there), so the
//! second decoder lost its subject.
//!
//! **The citation this replaces had gone stale on its own**, before and
//! independently of the change: it justified `dns-message-parser` "for
//! the reasons `hclient-dns-system`'s `svcb.rs` wrote down when it chose
//! that crate", and that file had since chosen a different one. A claim
//! about a neighbour is exactly as perishable as the neighbour — this
//! workspace's own recurring defect, met here from the direction where
//! the argument outlived the file it pointed at.
//!
//! What was true of the old decoder and is true of this one: no `unsafe`
//! anywhere in its `src`, a `Result` on every path rather than a panic,
//! and name decompression that cannot loop. Over a `DoH` response that
//! matters more than it does next door, not less: the bytes come from an
//! HTTP body, so a compromised or hostile endpoint chooses every one of
//! them.
//!
//! **The encode path is this module's alone** — nothing else in the
//! workspace builds a DNS message — so it is checked rather than
//! inherited. `tests/query_bytes.rs` settles it against a hand-written
//! expected encoding of a query, byte for byte, and **passed unchanged
//! across the decoder swap**, which is what says the wire bytes did not
//! move.
//!
//! **What this module refuses, and why each refusal is not an empty
//! answer.** RFC 8484 leaves DNS semantics exactly where they were, so the
//! distinction `Resolve` draws between "asked and found nothing" and
//! "could not ask" has to survive the HTTP layer intact:
//!
//! | condition | result |
//! |---|---|
//! | NOERROR, no records of the type | empty `Vec` — an answer |
//! | NXDOMAIN | empty `Vec` — an answer, from an authority |
//! | any other RCODE | [`DohError::ResponseCode`] |
//! | `QR` clear (a query came back) | [`DohError::NotAResponse`] |
//! | `TC` set | [`DohError::Truncated`] — see below |
//! | the question does not match the one sent | [`DohError::QuestionMismatch`] |
//! | anything the decoder refuses | [`DohError::Malformed`] |
//!
//! `TC` deserves a note because `DoH` is the one transport where it should
//! never appear: the response travels over TCP-or-better with no 512-byte
//! limit, so a truncated answer means the *server's own* upstream lookup
//! was truncated and it passed that on. There is no retry this crate can
//! make that the server has not already made, so it is an error rather than
//! a partial `RRSet` — the same call `hclient-dns-system` makes for the same
//! reason.
//!
//! [`Message::from_octets`]: domain::base::Message::from_octets

use crate::error::DohError;
use bytes::Bytes;
use domain::base::iana::{Class, Rcode, Rtype};
use domain::base::{Message, MessageBuilder, Name, Question};
use domain::rdata::{A, Aaaa, Https};
use hclient_dns::svcb::{RawBinding, RawParam, endpoint_from_binding};
use hclient_dns::{RData, Record};
use std::time::Duration;

/// The largest response body this crate will read, in bytes.
///
/// The width of the length field that frames a DNS message over TCP, so no
/// legitimate answer is cut by it — and a bound is needed, because the body
/// length is chosen by a server the client has not yet decided to trust.
pub const MAX_RESPONSE_BYTES: usize = 65_535;

/// The three questions this crate knows how to ask.
///
/// A closed enum rather than a `u16` passed around: it is what makes
/// [`decode_answer`]'s question check total, and it keeps `lookup`
/// from being able to ask for `AAAA` by getting an argument wrong.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Query {
    A,
    Aaaa,
    Https,
}

impl Query {
    fn rtype(self) -> Rtype {
        match self {
            Self::A => Rtype::A,
            Self::Aaaa => Rtype::AAAA,
            Self::Https => Rtype::HTTPS,
        }
    }
}

/// The two questions an *address* lookup can ask.
///
/// A second, narrower enum rather than reusing [`Query`], because the
/// address path and the SVCB path are genuinely different: `lookup`
/// does not go through `Doh::recover`, so a `recover` taking a `Query` had
/// an `Https` arm nothing could reach. Mutation testing found it — an
/// unreachable arm cannot be killed by any test, which is exactly the kind
/// of line that reads as load-bearing and proves nothing. This type is the
/// fix: the arm is gone rather than covered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Family {
    V4,
    V6,
}

impl Family {
    pub(crate) fn query(self) -> Query {
        match self {
            Self::V4 => Query::A,
            Self::V6 => Query::Aaaa,
        }
    }
}

/// What one exchange produced. Only one of the two is ever non-empty — the
/// other belongs to a question that was not asked.
#[derive(Debug, Default)]
pub(crate) struct Answer {
    pub(crate) addrs: Vec<Record>,
    pub(crate) endpoints: Vec<Record>,
}

/// One RFC 8484 query, ready to be a request body.
///
/// **The ID is zero, deliberately** (RFC 8484 §4.1: "the DNS ID SHOULD be
/// 0"). Over UDP the ID is what binds an answer to its question against an
/// off-path attacker; over HTTP that binding is the HTTP exchange itself,
/// and a varying ID only makes two identical queries look different to a
/// cache. The question echoed in the response is checked instead — see
/// [`DohError::QuestionMismatch`].
///
/// `rd` is set: this is a stub resolver asking a recursive one.
///
/// **The builder is imperative where its predecessor was declarative**, so
/// the two facts above are set by two calls rather than read off a
/// literal. Nothing else moves: `MessageBuilder::new_vec` starts at ID 0
/// with every flag clear, so `set_rd(true)` is the only flag written and
/// the ID is left where it already is — deliberately not `set_id(0)`,
/// which would be a line that cannot fail and reads as though it could.
/// `set_random_id` is the call this must never grow; the paragraph above
/// is why.
pub(crate) fn encode_query(name: &str, query: Query) -> Result<Bytes, DohError> {
    // No bracket stripping here, deliberately. `Uri::host()` brackets an
    // IPv6 literal (`[::1]`) and that is the string `Resolve` receives —
    // but a literal never reaches this function, because `Doh::addrs`
    // answers it from the string itself and makes no query at all. What
    // could still arrive bracketed is a thing that is *not* a literal, and
    // turning `[foo]` into a query for `foo` would be inventing a name the
    // caller did not give.
    //
    // `Name::<Vec<u8>>::vec_from_str` takes a relative name and absolutises
    // it, so `example.com` and `example.com.` both arrive as the same
    // absolute name — which is what the wire wants and what
    // `check_question` compares against.
    let qname = Name::<Vec<u8>>::vec_from_str(name).map_err(|e| DohError::NameNotUsable {
        name: name.to_owned(),
        reason: e.to_string(),
    })?;

    let mut builder = MessageBuilder::new_vec();
    builder.header_mut().set_rd(true);

    let mut question = builder.question();
    question
        .push(Question::new_in(&qname, query.rtype()))
        .map_err(|e| DohError::Encode(e.to_string()))?;

    Ok(Bytes::from(question.finish()))
}

/// A response body, as the records a caller may act on.
/// **It borrows the body rather than taking it**, which the change to a
/// borrowed message made honest: nothing here keeps the bytes past the
/// call, so consuming them would be claiming an ownership this function
/// does not use. `&[u8]` rather than `&Bytes` for the same reason one
/// step further — a `Bytes` is what the caller happens to hold, and this
/// wants only the octets.
pub(crate) fn decode_answer(body: &[u8], name: &str, query: Query) -> Result<Answer, DohError> {
    // **A borrowed message over the body, rather than an owned one**, and
    // that is a dependency decision rather than a style one: `Bytes`
    // implements `domain`'s `Octets` only under that crate's `bytes`
    // feature, which turns on `octseq/bytes` as well. Measured, it adds no
    // crate — `bytes` is already here — but a slice needs no feature at
    // all, and everything this function produces is owned before `body`
    // drops, so there is nothing for the borrow to outlive. The narrower
    // statement is the one to make.
    //
    // `from_slice` checks only that the header is there; every section is
    // parsed lazily below, so a message that is short or self-contradictory
    // past the header fails at the section that reads it rather than here.
    // That is why each `answer()`/iterator step below carries its own
    // `Malformed` rather than one decode at the top.
    let dns = Message::from_slice(body).map_err(|e| DohError::Malformed(e.to_string()))?;
    let header = dns.header();

    if !header.qr() {
        return Err(DohError::NotAResponse);
    }
    if header.tc() {
        return Err(DohError::Truncated);
    }
    check_question(dns, name, query)?;
    match header.rcode() {
        Rcode::NOERROR => {}
        // The name does not exist, said by an authority. A definitive
        // answer — there are no records of any type — and not a failure to
        // ask, so it is an empty `Vec` rather than an error, exactly as
        // `hclient-dns-system` treats the same RCODE.
        Rcode::NXDOMAIN => return Ok(Answer::default()),
        rcode => {
            return Err(DohError::ResponseCode {
                rcode: rcode.to_int(),
            });
        }
    }

    let section = dns
        .answer()
        .map_err(|e| DohError::Malformed(e.to_string()))?;

    let mut answer = Answer::default();
    // Records of a type that was not asked for — a CNAME chain, most
    // commonly — are stepped over rather than rejected, which is what
    // `limit_to_in` does: it yields only records of the one type this
    // build asked for, in class IN, and skips the rest. The owner name is
    // deliberately NOT compared against the question: a CNAME means the
    // addresses are legitimately owned by a different name, and requiring
    // a match would break every aliased host.
    //
    // **`limit_to_in` rather than `limit_to`, and the `_in` is
    // load-bearing**: a record of the right type in the wrong class is not
    // an answer to an IN question, and the unsuffixed form would take one.
    // `check_question` already refuses a response whose *question* is in
    // another class; this is the same rule about the records, which that
    // check cannot see.
    match query {
        Query::A => {
            for record in section.limit_to_in::<A>() {
                let record = record.map_err(|e| DohError::Malformed(e.to_string()))?;
                answer.addrs.push(
                    Record::new(RData::A(record.data().addr()))
                        .ttl(Some(Duration::from_secs(u64::from(record.ttl().as_secs())))),
                );
            }
        }
        Query::Aaaa => {
            for record in section.limit_to_in::<Aaaa>() {
                let record = record.map_err(|e| DohError::Malformed(e.to_string()))?;
                answer.addrs.push(
                    Record::new(RData::Aaaa(record.data().addr()))
                        .ttl(Some(Duration::from_secs(u64::from(record.ttl().as_secs())))),
                );
            }
        }
        Query::Https => {
            // The RDATA's names are `ParsedName`s over the message, so a
            // `TargetName` written as a compression pointer resolves
            // against this message — which RFC 9460 §2.2 forbids a sender
            // from writing and this decoder still reads. That is the one
            // place this path is wider than `hclient-dns-system`'s, which
            // parses RDATA in isolation and so cannot follow a pointer out
            // of the record at all. It is not a new exposure: the previous
            // decoder resolved pointers against the whole message too, and
            // the message here is one the server sent rather than one this
            // crate assembled.
            //
            // `Https<_, _>` rather than a named pair: the octets are the
            // message's own range type and the name is a `ParsedName` over
            // it, and `domain` exports no alias for that combination — so
            // spelling them out would be writing down two types inference
            // already knows and that a future `domain` is free to change.
            for record in section.limit_to_in::<Https<_, _>>() {
                let record = record.map_err(|e| DohError::Malformed(e.to_string()))?;
                let owner = record.owner().to_string();
                let ttl = Duration::from_secs(u64::from(record.ttl().as_secs()));
                let binding = binding_from_decoded(record.data(), &owner, ttl)
                    .map_err(|e| DohError::Malformed(e.to_string()))?;
                if let Some(endpoint) = endpoint_from_binding(&binding).map_err(|e| match e {
                    hclient_dns::svcb::SvcbRecordError::MandatoryKeyAbsent { key } => {
                        DohError::MandatoryKeyAbsent { key }
                    }
                })? {
                    answer.endpoints.push(endpoint);
                }
            }
        }
    }
    Ok(answer)
}

/// The response must echo exactly the question that was asked.
///
/// **Compared case-insensitively on the name**, because a server is free
/// to apply DNS 0x20 randomisation or to echo the name in a different
/// case, and neither is a different question. That is `domain`'s own
/// `PartialEq` for a name rather than a fold of ours: `ToName::name_eq`
/// compares the flat label octets with `eq_ignore_ascii_case`, read in
/// 0.12.2 rather than assumed. Relying on it is the narrower statement —
/// a fold over `Display` output would also have to get the trailing dot
/// and the label boundaries right, which comparing names structurally
/// does not have to.
///
/// The names are absolute on both sides, so the trailing-dot question the
/// previous string comparison had to answer does not arise: `encode_query`
/// absolutises whatever the caller gave, and a name off the wire is
/// absolute by construction. `example.com` and `example.com.` are
/// therefore the same question here for a structural reason rather than
/// because something trims.
fn check_question(dns: &Message<[u8]>, name: &str, query: Query) -> Result<(), DohError> {
    let asked = format!("{}/{}", name.trim_end_matches('.'), query.rtype());

    let wanted = Name::<Vec<u8>>::vec_from_str(name).map_err(|e| DohError::NameNotUsable {
        name: name.to_owned(),
        reason: e.to_string(),
    })?;

    let Some(question) = dns.question().next() else {
        return Err(DohError::QuestionMismatch {
            asked,
            got: "no question section".to_owned(),
        });
    };
    let question = question.map_err(|e| DohError::Malformed(e.to_string()))?;

    let got_name = question.qname().to_string();
    let got = format!(
        "{}/{}",
        got_name.strip_suffix('.').unwrap_or(&got_name),
        question.qtype()
    );
    if question.qname() != &wanted
        || question.qtype() != query.rtype()
        || question.qclass() != Class::IN
    {
        return Err(DohError::QuestionMismatch { asked, got });
    }
    Ok(())
}

// Maintainer notes (not rendered):
//
// been in this crate. `ca5ab5e4` is the precedent, one crate over and one
// dependency along: `winnow` came off `hclient-proto`'s public surface on
// exactly this argument, *before the freeze*, because a leaked foreign
// type is cheap to withdraw from a pre-release and a major version
// afterwards.
//
// **Not a regression from the decoder change**, which is worth saying
// because the timing invites it: the parent of `99672f92` had
// `pub fn binding_from_decoded(binding: &dns_message_parser::rr::ServiceBinding)`
// — the same leak with a different foreign crate. Moving to `domain`
// replaced one leaked type with another rather than introducing the
// defect.
/// A `domain`-decoded HTTPS record, reduced to the backend-neutral
/// [`RawBinding`] every resolver in this workspace produces.
///
/// **This lived in `hclient_dns::svcb` as a `pub fn` and moved here,
/// because it is the one function in that pair that cannot be written
/// without naming a decoder.** Its signature mentioned `domain` in four
/// places — the parameter, the error type and both bounds — and
/// `hclient-dns` re-exports no `domain`, so an outside caller could not
/// name those types without adding the crate to their own manifest at a
/// matching version. That made `domain`'s major version part of the
/// `Resolve` seam's promise, for a function whose only caller has always
/// been in this crate.
///
/// **What stayed in `hclient-dns` is the half that names nothing.**
/// [`RawBinding`], [`RawParam`] and [`endpoint_from_binding`] are the
/// neutral seam — `hclient-dns-system` reads the RFC 9460 §2.4/§2.5/§8
/// rules over the same three, filling a `RawBinding` from RDATA where this
/// crate fills one from a whole message. Both decode with `domain`; each
/// now says so in its own manifest rather than through a feature on the
/// crate between them.
///
/// **The owner name and the TTL are parameters rather than fields of
/// `https`, because in DNS they are the record's and not the RDATA's.**
/// That is where the wire puts them and where `domain` keeps them — on the
/// enclosing [`Record`](domain::base::Record) — and it is the same split
/// `hclient-dns-system`'s `binding_from_rdata` makes one crate over.
///
/// # Errors
///
/// A `SvcParam` whose octets are not a well-formed value of its own key.
/// `domain` reports that per parameter, from the iterator, rather than at
/// the record — so a record can parse and one of its parameters still
/// refuse. RFC 9460 §2.2 makes that a reason to reject the whole record,
/// which is what returning `Err` here achieves; [`decode_answer`] turns it
/// into [`DohError::Malformed`].
// **`iter` with the value type written out, not `iter_all`**, and the
// difference is a lifetime rather than a convenience. `iter_all` is
// `iter::<AllValues<Octs>>`, whose `Iterator` impl then needs
// `Octs::Range<'a> == Octs` — an equality that can only be stated for all
// `'a`, which makes the octets `'static` and so refuses a message borrowed
// from a response body. Naming `AllValues<Octs::Range<'a>>` instead ties
// the values to the borrow they are parsed out of, which is what they
// actually are; `raw_param` copies everything it keeps, so nothing of the
// borrow survives the call.
//
// This is the one place the two callers of `RawBinding` genuinely differ,
// and it is why `hclient-dns-system` can use `iter_all` where this cannot:
// it parses one record's RDATA in isolation, so its octets are `'static`
// and the equality holds.
//
// `Display` on the name is `ToName`'s missing half: a `ParsedName` prints
// only where it is `Display`, and the target has to become a string for
// `RawBinding`, which holds no borrowed memory by design.
fn binding_from_decoded<'a, Octs, Name>(
    https: &'a domain::rdata::svcb::Https<Octs, Name>,
    owner: &str,
    ttl: Duration,
) -> Result<RawBinding, domain::base::wire::ParseError>
where
    Octs: domain::dep::octseq::Octets,
    Name: domain::base::name::ToName + core::fmt::Display,
{
    let mut params = Vec::new();
    for value in https
        .params()
        .iter::<domain::rdata::svcb::value::AllValues<Octs::Range<'a>>>()
    {
        params.push(raw_param(value?));
    }

    Ok(RawBinding {
        priority: https.priority(),
        owner: owner.trim_end_matches('.').to_owned(),
        // **The trim is a no-op on this path, and the comment it arrived
        // with said the opposite — corrected here rather than carried.**
        // It read *"`Display` for a name writes the root as `.`, so
        // trimming leaves the empty string"*. Measured against `domain`
        // 0.12.2 through `Https::parse` rather than reasoned about: a
        // parsed `target()` prints `""` for a root target and
        // `"svc.example.net"` for a non-root one, with no trailing dot in
        // either case, so there is never a dot to trim.
        // `hclient-dns-system`'s copy of this comment has it right —
        // *"the root as the empty string"* — and the two have disagreed in
        // prose while agreeing in behaviour.
        //
        // So the expression is unfalsifiable and the mutant dropping it
        // survives. That was checked in **both** trees, and the pair is
        // the point: the same 75 tests stay green with it gone here and
        // with it gone at its old site in `hclient-dns`, so the move
        // neither caused the survivor nor lost a kill.
        //
        // It is kept because it costs nothing and `RawBinding::target`
        // documents the root as the empty string: a future `domain` that
        // printed absolute names in RFC 1035 presentation form, with the
        // dot, would otherwise put `svc.example.net.` into every
        // consumer's pool key and TLS server name. No test can reach the
        // input that would prove that, which is why this is a comment and
        // not an assertion.
        target: https.target().to_string().trim_end_matches('.').to_owned(),
        params,
        // The wire always carries a TTL, so this is always `Some` on this
        // path — the `Option` is for the backends that genuinely may not
        // know.
        ttl: Some(ttl),
    })
}

/// One decoded `SvcParam`, in [`RawParam`]'s vocabulary.
///
/// **The three keys `domain` models and this workspace does not become
/// `Other`, and that is load-bearing rather than tidy.** `dohpath` (7),
/// `ohttp` (8) and `tls-supported-groups` (9) are real, registered, and
/// acted on nowhere here; [`RawParam::Other`] is what RFC 9460 §8's
/// `mandatory` check reads to refuse a record that makes one of them
/// mandatory. Dropping them instead would let such a record through as
/// usable — and that is not hypothetical here, because
/// `a_record_making_dohpath_mandatory_is_ignored_and_the_usable_one_is_kept`
/// is exactly the test that would go green over a usable record.
///
/// **`Unknown` is not only "a key nobody models".** `AllValues` answers it
/// for a known key whose value did not parse as that key — measured,
/// `domain` yields an `Err` from the iterator for the shapes this crate
/// can produce, but the fallback exists — so a key inside
/// `hclient_dns::svcb`'s recognised set arriving as `Unknown` is a
/// malformed record wearing a recognised number.
///
/// **This is `hclient-dns-system`'s `raw_param`, and the duplication is
/// the owner's decision to make rather than a thing to tidy.** Compared
/// arm by arm rather than asserted: all eleven `AllValues` arms are
/// identical, and the only textual difference is `rustfmt` bracing the
/// `Alpn` arm's body where the copy sits one level deeper. Both are over
/// the same `domain` type, and the obvious
/// home for one shared copy is `hclient-dns` — which is where this one
/// came from, and where it was the whole reason that crate had a `codec`
/// feature and a `domain` dependency at all. Putting it back would put
/// `hclient-dns-system` back on a feature it deliberately stopped asking
/// for when it moved to RDATA-level decoding, and would re-leak `domain`
/// through the seam this move exists to clear. The two copies differ in
/// nothing today; if one is changed, the other is the thing to read.
fn raw_param<Octs: domain::dep::octseq::Octets>(
    value: domain::rdata::svcb::value::AllValues<Octs>,
) -> RawParam {
    use domain::base::iana::SvcParamKey;
    use domain::rdata::svcb::value::AllValues;

    match value {
        AllValues::Mandatory(keys) => {
            RawParam::Mandatory(keys.iter().map(SvcParamKey::to_int).collect())
        }
        AllValues::Alpn(alpn) => RawParam::Alpn(alpn.iter().map(|p| p.as_ref().to_vec()).collect()),
        AllValues::NoDefaultAlpn(_) => RawParam::NoDefaultAlpn,
        AllValues::Port(port) => RawParam::Port(port.port()),
        // RFC 9460 §7.3's ECHConfigList, **including the redundant
        // two-octet length prefix**, which is the form [`RawParam::Ech`] is
        // documented to carry and the form rustls parses: the
        // SvcParamValue for key 5 *is* the ECHConfigList, and `domain`
        // wraps the value's octets without stripping anything.
        //
        // **This is where the decoder swap changed code rather than
        // prose.** `dns-message-parser` validated the prefix and handed
        // back the payload without it, so this arm used to put two bytes
        // back on; putting them back on `domain`'s output would give
        // rustls a doubly-prefixed list. Measured by the round-trip test
        // that caught the original stripping, which asserts the bytes
        // `00 03 ab cd ef` reach `SvcbEndpoint::ech_config_list`.
        AllValues::Ech(ech) => RawParam::Ech(ech.as_slice().to_vec()),
        AllValues::Ipv4Hint(hint) => RawParam::Ipv4Hint(hint.iter().collect()),
        AllValues::Ipv6Hint(hint) => RawParam::Ipv6Hint(hint.iter().collect()),
        // The key numbers are named rather than written: `domain` keeps
        // the `SvcParamValue` trait that carries `key()` private, so a
        // value of one of these types cannot be asked for its own key from
        // outside that crate.
        AllValues::DohPath(_) => RawParam::Other(SvcParamKey::DOHPATH.to_int()),
        AllValues::Ohttp(_) => RawParam::Other(SvcParamKey::OHTTP.to_int()),
        AllValues::TlsSupportedGroups(_) => {
            RawParam::Other(SvcParamKey::TLS_SUPPORTED_GROUPS.to_int())
        }
        AllValues::Unknown(unknown) => RawParam::Other(unknown.key().to_int()),
    }
}
