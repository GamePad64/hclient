//! The one place this crate asks "can the system resolver answer an HTTPS
//! query on this target?" — and the only place that can answer it.
//!
//! **Why `supports_svcb` and the lookup are exported from the same module,
//! and never written down twice.** `Resolve::supports_svcb` and
//! `Resolve::lookup_svcb` have to agree — a `true` over a lookup that
//! cannot produce a record is the "capability that lies" this project has
//! caught three times across three backends. Two `#[cfg]` expressions, one
//! on the capability and one on the lookup, would let exactly that drift
//! back in the moment somebody adds a target to one and not the other. So
//! there is a single pair of mutually negated `#[cfg]`s below, selecting a
//! single module that defines BOTH items; `supports_svcb()` forwards to
//! that module's answer and `lookup_svcb` calls its lookup. They
//! cannot disagree, because there is no edit that changes one without
//! changing the other.
//!
//! **Why this file is not `#![forbid(unsafe_code)]` when its siblings
//! are.** `forbid` propagates into child modules, and `res_query` — the
//! children selected on Unix and on Windows — are the crate's
//! foreign-function boundaries (spec amendment C8). `forbid` here would
//! make those modules impossible, so the crate root's `deny` stands
//! instead. Nothing in this file is unsafe, and nothing may become so:
//! CI's `no-unsafe-code` job path-scopes the C8 marker to
//! `sys/res_query.rs` and `sys/windows.rs` alone, so an `unsafe` block
//! added HERE fails the build exactly as it would in any other crate.

/// The Unix backend needs two things this crate cannot check for at
/// runtime: a `res_query` symbol to link against, and a libc whose
/// resolver state is per-thread (see `res_query`'s own notes on both). The
/// list is deliberately the set of targets whose behaviour was established
/// rather than assumed — glibc and musl were checked by reading the
/// exported symbols out of the installed libraries, Apple by reading
/// `libresolv.9.tbd`. Anything else gets the honest `false` below, which
/// is not a gap to be embarrassed about: an absent capability costs a
/// caller one fallback, a capability that lies costs it a wrong
/// connection.
#[cfg(any(
    all(target_os = "linux", any(target_env = "gnu", target_env = "musl")),
    target_vendor = "apple"
))]
#[path = "res_query.rs"]
mod imp;

#[cfg(all(
    windows,
    not(any(
        all(target_os = "linux", any(target_env = "gnu", target_env = "musl")),
        target_vendor = "apple"
    ))
))]
#[path = "windows.rs"]
mod imp;

#[cfg(not(any(
    all(target_os = "linux", any(target_env = "gnu", target_env = "musl")),
    target_vendor = "apple",
    windows
)))]
#[path = "unsupported.rs"]
mod imp;

pub(crate) use crate::error::SvcbLookupError;
pub(crate) use imp::{SUPPORTS_SVCB, lookup};

/// What the resolver handed back, in plain Rust, with no borrowed memory
/// and nothing left for a caller to free.
///
/// This is the entire vocabulary of the FFI boundary: everything above it
/// works on these three shapes and never sees a pointer.
///
/// **Only the `res_query` backend produces one of these**, because only it
/// deals in raw DNS messages: Windows is handed records the OS has already
/// parsed and goes straight to `svcb::RawBinding`, and `unsupported`
/// produces nothing at all. A build compiles exactly one backend, so on a
/// Windows or unsupported target this type is entirely unreachable — which
/// is what the `dead_code` allowance is for, rather than a second copy of
/// the `#[cfg]` above.
#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(
    dead_code,
    reason = "each backend constructs a different subset, and a build compiles exactly one backend; repeating the target list of the `#[cfg]` above to narrow this would reintroduce the drift that single `#[cfg]` pair exists to prevent"
)]
pub(crate) enum RawAnswer {
    /// A complete response message, of known length.
    Message(Vec<u8>),
    /// The resolver call reported failure, which is what `res_query` does
    /// for the ordinary "this name has no HTTPS record" case. **Only the
    /// header is here, because only the header is knowable:** the failure
    /// path returns no length, so the rest of the buffer has no
    /// trustworthy end. Measured, not assumed — see `res_query`'s doc
    /// comment for the exact observation.
    ///
    /// This also covers "nothing arrived at all": the FFI zeroes the
    /// buffer before the call and every DNS response has `QR` set, so a
    /// header with `QR` clear is a buffer nothing was written into. That
    /// distinction is drawn in `svcb::endpoints_from_answer`, in safe,
    /// tested code, rather than in the FFI module — which is why this enum
    /// has no separate "no response" variant for the FFI to choose.
    HeaderOnly([u8; 12]),
    /// This build has no system SVCB backend — the only value
    /// `unsupported::query_https` ever returns. It maps to an empty result
    /// rather than an error, which paired with `supports_svcb() == false` is
    /// exactly what `Resolve` documents an absent capability to look like:
    /// "this resolver can't do SVCB", not "it asked and the network broke".
    NotSupported,
}

/// A DNS message is framed by a 16-bit length field over TCP, so it cannot
/// exceed this.
#[allow(
    dead_code,
    reason = "each backend constructs a different subset, and a build compiles exactly one backend; repeating the target list of the `#[cfg]` above to narrow this would reintroduce the drift that single `#[cfg]` pair exists to prevent"
)]
pub(crate) const MAX_MESSAGE: usize = 65535;

/// What a non-negative `res_query` return means for the buffer it was
/// given.
///
/// Lives here, not in the FFI module, on purpose: this is the bound that
/// decides whether a length the C library reported reaches a slice index,
/// and it is worth more as ordinary safe code with tests around it than as
/// three lines inside the one file where a mistake is not a panic.
///
/// The rule is not "`n` is the length". Measured on glibc 2.43: given a
/// 20-byte buffer for a 116-byte answer, `res_query` returns **20** — the
/// buffer's size, with no indication that anything was lost. So a return
/// that reaches the buffer's end is indistinguishable from a silent
/// truncation and must be retried at the largest a DNS message can be,
/// never truncated to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(
    dead_code,
    reason = "each backend constructs a different subset, and a build compiles exactly one backend; repeating the target list of the `#[cfg]` above to narrow this would reintroduce the drift that single `#[cfg]` pair exists to prevent"
)]
pub(crate) enum Written {
    /// Strictly inside the buffer: a complete answer of this length.
    Complete(usize),
    /// At or past the buffer's end. Possibly truncated, so unusable; retry
    /// at `MAX_MESSAGE`.
    Retry,
    /// A maximum-sized buffer came back full. Nothing larger is a DNS
    /// message, so this is a failure rather than a result.
    TooLarge,
}

#[allow(
    dead_code,
    reason = "each backend constructs a different subset, and a build compiles exactly one backend; repeating the target list of the `#[cfg]` above to narrow this would reintroduce the drift that single `#[cfg]` pair exists to prevent"
)]
pub(crate) fn classify_written(written: usize, buf_len: usize) -> Written {
    if written < buf_len {
        Written::Complete(written)
    } else if buf_len < MAX_MESSAGE {
        Written::Retry
    } else {
        Written::TooLarge
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use assert_matches::assert_matches;
    use rstest::rstest;

    /// Every variant, once, so a table below can range over all of them and
    /// a new variant that is not added here shows up as an unused-variant
    /// warning rather than as silently untested.
    fn one_of_every_variant() -> Vec<SvcbLookupError> {
        vec![
            SvcbLookupError::NameNotUsable {
                name: "host.example".to_owned(),
            },
            SvcbLookupError::NoResponse,
            SvcbLookupError::ResponseCode { rcode: 2 },
            SvcbLookupError::Truncated,
            SvcbLookupError::AnswerTooLarge,
            SvcbLookupError::LengthUnavailable { ancount: 3 },
            SvcbLookupError::HeaderTruncated { got: 7 },
            SvcbLookupError::Malformed(a_decode_error()),
            SvcbLookupError::WindowsDnsError { code: 9002 },
            SvcbLookupError::MandatoryKeyAbsent { key: 3 },
        ]
    }

    /// A real `DecodeError`, obtained the only way one can be: by handing
    /// the decoder something it refuses.
    fn a_decode_error() -> dns_message_parser::DecodeError {
        dns_message_parser::Dns::decode(bytes::Bytes::from_static(&[0u8; 3]))
            .expect_err("three bytes are not a DNS message")
    }

    #[test]
    fn a_length_strictly_inside_the_buffer_is_a_complete_answer() {
        assert_eq!(classify_written(116, 4096), Written::Complete(116));
        assert_eq!(classify_written(0, 4096), Written::Complete(0));
        assert_eq!(classify_written(4095, 4096), Written::Complete(4095));
    }

    #[test]
    fn a_length_that_reaches_the_buffers_end_is_retried_never_truncated_to() {
        // The measured glibc behaviour: a 20-byte buffer for a 116-byte
        // answer returns 20. Treating that as "a complete 20-byte answer"
        // is the bug this bound exists to stop.
        assert_eq!(classify_written(20, 20), Written::Retry);
        assert_eq!(classify_written(4096, 4096), Written::Retry);
    }

    #[test]
    fn a_length_past_the_buffers_end_is_also_retried_rather_than_indexed() {
        // A libc that reports the size it NEEDED rather than the size it
        // wrote must not reach `buf[..n]` either.
        assert_eq!(classify_written(9000, 4096), Written::Retry);
        assert_eq!(classify_written(usize::MAX, 4096), Written::Retry);
    }

    #[test]
    fn a_full_maximum_sized_buffer_is_a_failure_not_a_result() {
        assert_eq!(
            classify_written(MAX_MESSAGE, MAX_MESSAGE),
            Written::TooLarge
        );
        assert_eq!(classify_written(usize::MAX, MAX_MESSAGE), Written::TooLarge);
        // ... but a maximum-sized buffer that came back with room to spare
        // still holds a real answer.
        assert_eq!(
            classify_written(MAX_MESSAGE - 1, MAX_MESSAGE),
            Written::Complete(MAX_MESSAGE - 1)
        );
    }

    /// `Written::Retry` tells `query_https` to grow its buffer and go
    /// round again, so a `Retry` at the largest buffer the loop can build
    /// would be an infinite loop against a resolver that keeps filling it.
    /// The bound that rules that out is this one, not anything in the FFI
    /// file: at `MAX_MESSAGE` every over-long return is `TooLarge`.
    #[rstest]
    #[case::exactly_full(MAX_MESSAGE)]
    #[case::one_past_the_end(MAX_MESSAGE + 1)]
    #[case::wildly_over(usize::MAX)]
    fn nothing_retries_at_the_maximum_buffer_so_the_query_loop_cannot_spin(#[case] written: usize) {
        assert_eq!(
            classify_written(written, MAX_MESSAGE),
            Written::TooLarge,
            "a Retry here would send query_https back round the loop with a buffer it \
             cannot grow — the second iteration would classify the same way and never end"
        );
    }

    /// Each message has to carry the one value that tells it from another
    /// instance of the SAME variant. A `#[error]` string that drops its
    /// interpolation still reads plausibly in a log and leaves a reader
    /// unable to say which name, which RCODE, or which key was involved.
    #[rstest]
    #[case::the_name(SvcbLookupError::NameNotUsable { name: "bad\u{0}host".to_owned() }, "bad\u{0}host")]
    #[case::the_rcode(SvcbLookupError::ResponseCode { rcode: 5 }, "5")]
    #[case::the_record_count(SvcbLookupError::LengthUnavailable { ancount: 42 }, "42")]
    #[case::the_length(SvcbLookupError::HeaderTruncated { got: 7 }, "7")]
    #[case::the_win32_status(SvcbLookupError::WindowsDnsError { code: 9002 }, "9002")]
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

    /// Ten variants, ten distinct messages. Cheap, and it catches the one
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

    /// The `source()` chain is the whole reason this enum kept
    /// `DecodeError` instead of flattening it into a `String` (see the type
    /// doc). `Malformed` must expose it and nothing else may invent one:
    /// `hclient_core::Error` chains `source()`, so a caller reaches the
    /// decoder's own error by downcast rather than by reading a message.
    #[test]
    fn only_a_malformed_answer_carries_a_source_and_it_is_the_decoders_own() {
        use std::error::Error as _;

        for error in one_of_every_variant() {
            match error {
                SvcbLookupError::Malformed(_) => {
                    let source = error
                        .source()
                        .expect("the decoder's error must stay reachable");
                    assert!(
                        source
                            .downcast_ref::<dns_message_parser::DecodeError>()
                            .is_some(),
                        "a caller downcasts to the decoder's error, so `#[source]` has to \
                         point at it and not at some wrapper: got `{source}`"
                    );
                }
                other => assert_matches!(
                    other.source(),
                    None,
                    "only Malformed has an underlying cause; a source invented for another \
                     variant would make `{other}` look like a wrapper over something"
                ),
            }
        }
    }
}
