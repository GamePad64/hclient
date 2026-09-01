//! Two failures, and neither of them is "the name does not exist".
//!
//! That absence is the whole shape of this module. A name with no
//! addresses and a name with no HTTPS records are both ordinary answers —
//! an empty stream — and giving either one a variant here is the first
//! step toward a caller reading a name without records as a broken
//! resolver. So what is left is the two ways the *question* failed:
//! `getaddrinfo` refusing it ([`ResolveFailed`]), and a system SVCB
//! lookup that could not produce an RRSet it is willing to stand behind
//! ([`SvcbLookupError`]).
//!
//! **Both keep somebody else's own answer rather than a category of
//! ours.** `ResolveFailed` carries the `io::Error` as a `#[source]` so a
//! caller can reach the errno without parsing text; `SvcbLookupError`
//! carries `system_resolver::Error` and the decoder's `DecodeError` for
//! the same reason. Inventing a lossy category over either would throw
//! away the only thing that tells a timeout from a misconfigured resolver.
//!
//! **The platform vocabulary left with the platform code.** This enum used
//! to carry `NoResponse`, `ResponseCode`, `Truncated`, `HeaderTruncated`,
//! `LengthUnavailable` and `WindowsDnsError` — six variants describing how
//! a *resolver* fails, in a crate that no longer talks to one. They are
//! `system_resolver::Error`'s, and reaching them is a `source()` away
//! rather than a second copy that could drift.
//!
//! Neither type is public and neither becomes so by moving: `ResolveFailed`
//! reaches a caller only through `Error::source`, and `SvcbLookupError` is
//! `pub(crate)`.

/// `getaddrinfo` said no, with the name it was asked about kept alongside
/// the reason.
///
/// The name is in the message AND the `io::Error` is a `#[source]`, which
/// is not redundant: `hclient_core::Error` chains `source()`, so a caller
/// that wants the errno downcasts to `std::io::Error` through this without
/// parsing text, while a caller that only logs still sees which name
/// failed. Dropping `#[source]` would leave the second reader with the
/// same message and the first with nothing — see
/// `a_resolve_failure_keeps_the_io_error_reachable_by_downcast`.
#[derive(Debug, thiserror::Error)]
#[error("failed to resolve `{0}`: {1}")]
pub(crate) struct ResolveFailed(pub(crate) String, #[source] pub(crate) std::io::Error);

/// A system SVCB lookup that did not produce records, for a reason that is
/// not "there are none".
///
/// "There are none" is deliberately absent from this enum: it is not an
/// error, it is an empty stream, and giving it a variant here would be the
/// first step toward a caller treating a name without HTTPS records as a
/// broken resolver. "The name does not exist" is absent for the same
/// reason at one remove — `system_resolver` reports it, and this crate
/// turns it back into an empty answer, because the A/AAAA lookup for the
/// same name reports the missing name in its own stream where a caller is
/// actually looking for it.
///
/// `Clone` and `Eq` are deliberately absent, and after the decoder change
/// only one variant still owes them: `domain::base::wire::ParseError` is
/// `Clone + Copy + Eq`, where its predecessor was neither, so what is left
/// is `Resolver`. Flattening a source into a `String` to recover the two
/// derives was rejected then and is rejected now: it would break the
/// `source()` chain that lets a caller reach the real cause without
/// parsing a message, which is what `hclient_core::Error` is built
/// around.
///
/// `Malformed` and `Resolver` carry `#[source]` explicitly and not
/// `#[from]`: an automatic conversion would make `?` compile anywhere in
/// this crate, and the two places that convert somebody else's failure are
/// `map_err`s that deserve to stay visible. Every other variant has no
/// source, which is a property with a test of its own — a `{0}`
/// interpolation without `#[source]` reads identically in a message and
/// silently truncates the chain a caller downcasts through.
#[derive(Debug, PartialEq, thiserror::Error)]
pub(crate) enum SvcbLookupError {
    /// The system resolver could not answer. Its own error is the
    /// `#[source]`, because the vocabulary is the platform's and this
    /// crate has no better word for a timeout than the one the platform
    /// used.
    #[error("the system resolver could not answer: {0}")]
    Resolver(#[source] system_resolver::Error),
    /// The record arrived and the decoder refused it — its RDATA is not
    /// an HTTPS record, or one of its SvcParams is not what its key says
    /// it is. RFC 9460 §2.2 requires rejecting the whole RRSet in that
    /// case, and since the decoder now reads one record at a time that
    /// rule is a line in `svcb::wire` rather than a property of a
    /// message-level decoder.
    #[error("malformed answer: {0}")]
    Malformed(#[source] domain::base::wire::ParseError),
    /// RFC 9460 §8: the record's `mandatory` list names a key the record
    /// does not actually carry. Checked here rather than by the decoder,
    /// because it is a statement about the record as a whole and not about
    /// any one parameter's encoding.
    #[error("SvcParamKey {key} is listed as mandatory but is not present in the record")]
    MandatoryKeyAbsent { key: u16 },
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::assert_matches;
    use rstest::rstest;

    /// Every variant, once, so the tables below can range over all of them
    /// and a variant that is not added here shows up as an unused-variant
    /// warning rather than as silently untested.
    fn one_of_every_variant() -> Vec<SvcbLookupError> {
        vec![
            SvcbLookupError::Resolver(system_resolver::Error::ResponseCode { rcode: 2 }),
            SvcbLookupError::Malformed(a_decode_error()),
            SvcbLookupError::MandatoryKeyAbsent { key: 3 },
        ]
    }

    /// A real `ParseError`, obtained the only way one can be: by handing
    /// the decoder something it refuses.
    fn a_decode_error() -> domain::base::wire::ParseError {
        use domain::dep::octseq::parse::Parser;
        domain::rdata::svcb::Https::parse(&mut Parser::from_ref(&[0xffu8; 3][..]))
            .expect_err("three 0xff octets are not an HTTPS record")
    }

    /// Each message has to carry the one value that tells it from another
    /// instance of the SAME variant. A `#[error]` string that drops its
    /// interpolation still reads plausibly in a log and leaves a reader
    /// unable to say which resolver failure or which key was involved.
    #[rstest]
    #[case::the_resolvers_own_words(
        SvcbLookupError::Resolver(system_resolver::Error::ResponseCode { rcode: 5 }),
        "5"
    )]
    #[case::the_key(SvcbLookupError::MandatoryKeyAbsent { key: 7 }, "7")]
    #[case::the_decoders_own_words(SvcbLookupError::Malformed(a_decode_error()), &a_decode_error().to_string())]
    fn a_lookup_failure_names_the_value_that_distinguishes_it(
        #[case] error: SvcbLookupError,
        #[case] expected: &str,
    ) {
        let rendered = error.to_string();
        assert!(
            rendered.contains(expected),
            "`{rendered}` does not carry `{expected}`, so two failures of this kind read \
             identically to whoever has to act on one"
        );
    }

    /// Five variants, five distinct messages. Cheap, and it catches the one
    /// mistake a table of `#[error]` strings invites: the same message
    /// pasted onto a second variant, which makes two different failures
    /// indistinguishable in a log while every other test still passes.
    #[test]
    fn no_two_lookup_failures_render_the_same_message() {
        let all = one_of_every_variant();
        let mut rendered: Vec<String> = all.iter().map(ToString::to_string).collect();
        rendered.sort();
        let before = rendered.len();
        rendered.dedup();
        assert_eq!(
            rendered.len(),
            before,
            "two variants share a message: {rendered:?}"
        );
    }

    /// The `source()` chain is the whole reason this enum keeps somebody
    /// else's error type instead of flattening it into a `String` (see the
    /// type doc). Exactly the two variants that wrap another crate's
    /// failure must expose it, pointing at **that crate's** type, and no
    /// other variant may invent one: `hclient_core::Error` chains
    /// `source()`, so a caller reaches the real cause by downcast rather
    /// than by reading a message.
    #[test]
    fn only_a_wrapped_failure_carries_a_source_and_it_is_the_other_crates_own() {
        use std::error::Error as _;

        for error in one_of_every_variant() {
            match error {
                SvcbLookupError::Malformed(_) => {
                    let source = error.source().expect("the decoder's error stays reachable");
                    assert!(
                        source
                            .downcast_ref::<domain::base::wire::ParseError>()
                            .is_some(),
                        "a caller downcasts to the decoder's error, so `#[source]` has to \
                         point at it and not at some wrapper: got `{source}`"
                    );
                }
                SvcbLookupError::Resolver(_) => {
                    let source = error
                        .source()
                        .expect("the resolver's error stays reachable");
                    assert!(
                        source.downcast_ref::<system_resolver::Error>().is_some(),
                        "a caller downcasts to the resolver's own error: got `{source}`"
                    );
                }
                other => assert_matches!(
                    other.source(),
                    None,
                    "only a wrapped failure has an underlying cause; a source invented for \
                     another variant would make `{other}` look like a wrapper over something"
                ),
            }
        }
    }
}
