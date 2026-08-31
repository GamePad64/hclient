//! Windows: [`raw`] where the machine has `DnsQueryRaw`, [`parsed`] where
//! it does not.
//!
//! **This file makes the choice and touches nothing foreign**, which is
//! why it is the one module in this backend with no `unsafe` in it. Each of
//! the two paths is its own file because they share no bytes: one waits on
//! a completion routine for a wire message, the other walks a linked list
//! of structures, and the only thing they have in common is the answer they
//! produce.
//!
//! # Two calls, and the choice is visible in [`support`]
//!
//! `DnsQueryRaw` hands back the **wire message**, so on a machine that has
//! it this platform behaves exactly like the other four: any record type,
//! walked by the same code, with the same rules about truncation and
//! rcodes. It arrived in **Windows 11 / Server 2025**.
//!
//! `DnsQuery_UTF8` has been present since Windows 2000 and hands back
//! records the OS has already taken apart, so it can only answer for the
//! types the OS does *not* parse — [`Support::AnyExcept`], which is most of
//! the registry. [`parsed`] has the rule and the measurement behind it.
//!
//! # The detection has to be dynamic, and that is not a style choice
//!
//! `windows-sys` emits both `DnsQueryRaw` **and** `DnsQueryRawResultFree`
//! as `raw-dylib` imports, and the loader resolves imports at process
//! start — so a binary that so much as names one of them **fails to start**
//! on Windows 10, whether or not the code path runs. Both are therefore
//! fetched with `GetProcAddress` in [`raw`], and neither is ever named as a
//! linked symbol anywhere in this crate.
//!
//! **Nothing in this file is unsafe and nothing may become so**, and it is
//! not `#![forbid(unsafe_code)]` for `sys/mod.rs`'s reason: `forbid`
//! propagates into child modules, and both children are foreign-function
//! boundaries that need the `deny` a scoped `#[allow]` can override. What
//! holds the rule here instead is CI's `no-unsafe-code` job, which
//! path-scopes the amendment C8 marker to `raw.rs` and `parsed.rs` alone —
//! so an `unsafe` block added HERE fails the build exactly as it would in
//! any other crate.
//!
//! [`Support::AnyExcept`]: crate::Support::AnyExcept

mod parsed;
mod raw;

use crate::error::Error;
use crate::{Record, Support};

/// What this build can be asked for, decided by whether the machine has
/// `DnsQueryRaw`.
///
/// A function rather than a constant, and this is the one place in this
/// workspace where that is not machinery: the answer genuinely differs
/// between two machines running the same binary.
pub(crate) fn support() -> Support {
    if raw::available() {
        Support::Any
    } else {
        Support::AnyExcept(parsed::PARSED_BY_WINDOWS)
    }
}

/// Asks the system resolver for `name`'s records of type `rtype`.
///
/// Blocking — both calls do their own I/O, and the raw one is asynchronous
/// only in the sense that it answers on a thread pool thread this function
/// waits for.
pub(crate) fn query(name: &str, rtype: u16) -> Result<Vec<Record>, Error> {
    match raw::api() {
        Some(api) => raw::query(api, name, rtype),
        None => parsed::query(name, rtype),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// **The Windows 10 path, exercised on a Windows 11 machine.**
    ///
    /// On a machine that has `DnsQueryRaw`, [`support`] answers
    /// [`Support::Any`] and nothing else in this crate ever reaches
    /// [`parsed::query`] — so the code for the platform this project cannot
    /// get hold of would be the only code here that never runs. Calling it
    /// directly is what makes that untrue.
    ///
    /// The two paths are compared against each other rather than against a
    /// written expectation, which is the instrument the RDATA claim
    /// already uses against Linux: an expectation written here could be
    /// wrong in the same way twice. Only the RDATA is compared — the TTLs
    /// are a resolver's countdown and differ between two calls a moment
    /// apart.
    ///
    /// Run on Windows 11 on 2026-09-01, where `support()` is `Any`: the
    /// two agreed.
    #[test]
    #[ignore = "needs a name server"]
    fn the_parsed_path_answers_the_same_rdata_as_the_raw_one() {
        let Support::Any = support() else {
            // A machine with no `DnsQueryRaw` reaches `parsed::query`
            // through every other test, so there is nothing here to add.
            return;
        };
        let by_raw = query("cloudflare.com", 65).expect("the raw path answers");
        let by_parsed = parsed::query("cloudflare.com", 65).expect("the parsed path answers");

        let bytes = |records: &[Record]| {
            let mut all: Vec<Vec<u8>> = records.iter().map(|r| r.rdata.clone()).collect();
            all.sort_unstable();
            all
        };
        assert!(
            !by_raw.is_empty(),
            "cloudflare.com publishes an HTTPS record"
        );
        assert_eq!(
            bytes(&by_raw),
            bytes(&by_parsed),
            "the two Windows calls disagree about the record's bytes"
        );
    }
}
