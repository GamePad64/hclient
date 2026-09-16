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
use hclient_dns::svcb::{binding_from_decoded, endpoint_from_binding};
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
