//! The one place this crate asks *what can the platform be asked, and
//! how* — and the only place that can answer it.
//!
//! **Why [`support`] and [`query`] come from the same module and are never
//! written down twice.** They have to agree: a `Support` over a backend
//! that cannot produce a record is the *capability that lies* this project
//! has caught across several backends. Two `#[cfg]` expressions, one on
//! each, would let exactly that drift back in the moment somebody adds a
//! target to one and not the other. So there is a single set of mutually
//! exclusive `#[cfg]`s below selecting a single module that defines BOTH,
//! and there is no edit that changes one without changing the other.
//!
//! **Why this file is not `#![forbid(unsafe_code)]`.** `forbid` propagates
//! into child modules, and the children are this workspace's
//! foreign-function boundaries (spec amendment C8). The crate's `deny`
//! stands instead. Nothing in this file is unsafe and nothing may become
//! so: CI's `no-unsafe-code` job path-scopes the C8 marker to the backend
//! modules alone, so an `unsafe` block added HERE fails the build exactly
//! as it would in any other crate.

use crate::error::Error;

// **`cfg_if!` rather than four `#[cfg]`s, and the reason is the drift this
// module's own header is about.** Written as plain attributes, the arms
// have to be made mutually exclusive by hand: the Unix one carried
// `not(target_os = "android")`, the Windows one a `not(any(..))` of the
// two before it, and the fallback a `not(any(..))` of all three. That is
// the target list written **four** times, three of them negated, so adding
// a platform meant editing four places correctly or silently compiling two
// backends — or none.
//
// `cfg_if!` is `if`/`else if`/`else`: the arms are ordered, so each
// condition states only its own targets and the fallback states nothing at
// all. One crate, no dependencies of its own, no build script, and already
// in any graph that has this workspace's runtimes.
cfg_if::cfg_if! {
    // Android first. It is `target_os = "android"` and **not**
    // `target_os = "linux"`, so ordering is not what keeps it out of the
    // arm below — it is first because the reason it needs its own module
    // is not the cfg at all: the `res_*` family is not in the NDK's stable
    // ABI, and `android_res_nquery` is.
    if #[cfg(target_os = "android")] {
        #[path = "android.rs"]
        mod imp;
    }
    // Apple next, and it used to share the arm below. **It was moved out
    // because `res_9_query` failed the second of that arm's two
    // requirements**: measured on macOS 27, the same query answers 64/64
    // serially and 12/64 from eight threads, so the resolver state there
    // is shared rather than per-thread. `apple.rs` has the numbers and the
    // second reason.
    else if #[cfg(target_vendor = "apple")] {
        #[path = "apple.rs"]
        mod imp;
    }
    // The `res_query` backend needs two things this crate cannot check for
    // at run time: the symbol to link against, and a libc whose resolver
    // state is per-thread. The list is deliberately the set of targets
    // whose behaviour was established rather than assumed — glibc and musl
    // by reading the exported symbols out of the installed libraries, and
    // by running `concurrent_lookups_all_answer_where_a_serial_burst_does`
    // on both, which is the test that names this requirement.
    //
    // **That second clause was here before the test was**, which is this
    // workspace's own defect met inside the sentence stating the rule: the
    // crate contained no threaded case at all, and the property Apple's
    // arm failed was asserted by nothing. It names the test now, so the
    // claim is exactly as perishable as the check behind it.
    else if #[cfg(all(
        target_os = "linux",
        any(target_env = "gnu", target_env = "musl")
    ))] {
        #[path = "res_query.rs"]
        mod imp;
    }
    // **FreeBSD, on the same module and in an arm of its own, because its
    // evidence is not the same evidence.** Both requirements above are
    // answered, and only one of them is answered the way the arm above
    // answers it:
    //
    // - *the symbol*: `lib/libc/resolv/Symbol.map` exports `res_query` and
    //   `__res_query`, each under `FBSD_1.0`, so the plain name links out
    //   of `libc` with no `link_name` and no `-lresolv`. Read out of the
    //   exported symbols, which is exactly how glibc and musl were
    //   settled — and it fails **loudly**, at link, if it is ever wrong.
    // - *the per-thread state*: `resolver(3)` says *"This implementation
    //   of the resolver is thread-safe"* and calls `_res` *"the per-thread
    //   version"*. That is a claim in the platform's own words, where
    //   Apple's arm had only a symbol list that says nothing about state —
    //   but it is **read rather than run**, and this crate exists partly
    //   because a manual page was wrong about a Mac.
    //
    // So this arm is added on the owner's decision with that gap named
    // rather than papered over. What closes it is one command on a FreeBSD
    // machine, and it is the same command that established the arm above:
    // `cargo test -p system-resolver --test live -- --ignored`, whose
    // `concurrent_lookups_all_answer_where_a_serial_burst_does` is written
    // for exactly this question. Until somebody runs it, this row is the
    // best-supported unrun arm in the crate — which is a rank, not a
    // guarantee.
    else if #[cfg(target_os = "freebsd")] {
        #[path = "res_query.rs"]
        mod imp;
    } else if #[cfg(windows)] {
        #[path = "windows/mod.rs"]
        mod imp;
    }
    // Anything else gets the honest `Support::None`, which is not a gap to
    // be embarrassed about: an absent capability costs a caller one
    // fallback, where a capability that lies costs it a wrong answer.
    else {
        #[path = "unsupported.rs"]
        mod imp;
    }
}

pub(crate) use imp::{query, support};

/// A DNS message is framed by a 16-bit length field over TCP, so it cannot
/// exceed this.
#[allow(
    dead_code,
    reason = "each backend uses a different subset, and a build compiles exactly one backend; repeating the target list of the `#[cfg]`s above to narrow this would reintroduce the drift that single set exists to prevent"
)]
pub(crate) const MAX_MESSAGE: usize = 65535;

/// RFC 1035 §2.3.4 — the wire form of a name is at most 255 octets, and
/// the textual form is never shorter than the wire form, so this rejects
/// nothing a resolver could have answered.
#[allow(
    dead_code,
    reason = "each backend uses a different subset, and a build compiles exactly one backend; repeating the target list of the `#[cfg]`s above to narrow this would reintroduce the drift that single set exists to prevent"
)]
pub(crate) const MAX_NAME_LEN: usize = 255;

/// The query name as a C string, or the reason it has no wire form.
///
/// Every backend calls this before it calls anything foreign, so a name
/// with no wire form is refused identically on all of them rather than
/// handed to whatever the local resolver does with it. Checking it here
/// also means the `as c_int` casts in the backends are over values already
/// known to be small, rather than trusted to be.
#[allow(
    dead_code,
    reason = "each backend uses a different subset, and a build compiles exactly one backend; repeating the target list of the `#[cfg]`s above to narrow this would reintroduce the drift that single set exists to prevent"
)]
pub(crate) fn query_name(name: &str) -> Result<std::ffi::CString, Error> {
    let unusable = || Error::NameNotUsable {
        name: name.to_owned(),
    };
    if name.len() > MAX_NAME_LEN {
        return Err(unusable());
    }
    std::ffi::CString::new(name).map_err(|_| unusable())
}

/// What a non-negative return means for the buffer it was given.
///
/// Lives here, not in a backend, on purpose: this is the bound that
/// decides whether a length the C library reported reaches a slice index,
/// and it is worth more as ordinary safe code with tests around it than as
/// three lines inside the one file where a mistake is not a panic.
///
/// The rule is not *`n` is the length*. Measured on glibc 2.43: given a
/// 20-byte buffer for a 116-byte answer, `res_query` returns **20** — the
/// buffer's size, with no indication that anything was lost. So a return
/// that reaches the buffer's end is indistinguishable from a silent
/// truncation and must be retried at the largest a DNS message can be,
/// never truncated to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(
    dead_code,
    reason = "each backend uses a different subset, and a build compiles exactly one backend; repeating the target list of the `#[cfg]`s above to narrow this would reintroduce the drift that single set exists to prevent"
)]
pub(crate) enum Written {
    /// Strictly inside the buffer: a complete answer of this length.
    Complete(usize),
    /// At or past the buffer's end. Possibly truncated, so unusable; retry
    /// at [`MAX_MESSAGE`].
    Retry,
    /// A maximum-sized buffer came back full. Nothing larger is a DNS
    /// message, so this is a failure rather than a result.
    TooLarge,
}

#[allow(
    dead_code,
    reason = "each backend uses a different subset, and a build compiles exactly one backend; repeating the target list of the `#[cfg]`s above to narrow this would reintroduce the drift that single set exists to prevent"
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

/// Nothing here needs a backend, which is the point: `query` is the only
/// function in this module a target can change.
#[cfg(test)]
mod tests {
    use super::*;
    use assert_matches::assert_matches;

    #[test]
    fn a_length_strictly_inside_the_buffer_is_a_complete_answer() {
        assert_eq!(classify_written(116, 4096), Written::Complete(116));
        assert_eq!(classify_written(0, 4096), Written::Complete(0));
        assert_eq!(classify_written(4095, 4096), Written::Complete(4095));
    }

    /// The measured glibc behaviour: a 20-byte buffer for a 116-byte
    /// answer returns 20. Treating that as *a complete 20-byte answer* is
    /// the defect this bound exists to stop.
    #[test]
    fn a_length_that_reaches_the_buffers_end_is_retried_never_truncated_to() {
        assert_eq!(classify_written(20, 20), Written::Retry);
        assert_eq!(classify_written(4096, 4096), Written::Retry);
    }

    #[test]
    fn a_full_maximum_buffer_is_a_failure_because_nothing_larger_is_a_message() {
        assert_eq!(
            classify_written(MAX_MESSAGE, MAX_MESSAGE),
            Written::TooLarge
        );
    }

    #[test]
    fn a_name_with_no_wire_form_is_refused_before_any_platform_sees_it() {
        assert_matches!(
            query_name(&"a".repeat(256)),
            Err(Error::NameNotUsable { .. })
        );
        assert_matches!(query_name("has\0a nul"), Err(Error::NameNotUsable { .. }));
        assert_matches!(query_name("example.com"), Ok(_));
    }
}
