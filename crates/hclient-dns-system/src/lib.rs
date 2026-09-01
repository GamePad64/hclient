//! System resolver on top of `std::net::ToSocketAddrs` (i.e. `getaddrinfo`).
//!
//! `getaddrinfo` blocks on every platform, so this crate requires the
//! `Blocking` capability — and is therefore unavailable wherever that
//! capability isn't (wasm).
//!
//! **`getaddrinfo` still cannot return an HTTPS/SVCB record, and never
//! will** — its result type is a list of `sockaddr`s. So SVCB does not
//! come from `getaddrinfo` here; it comes from a second system call
//! alongside it, and **that call is `system-resolver`'s now**:
//! `res_query(3)`, `android_res_nquery`, `DnsQueryRaw` and
//! `DnsQuery_UTF8`, one seam over five platforms, `Vec<Record>` out.
//!
//! **What is left in this crate is the half that is about HTTPS records
//! rather than about DNS**, and the split is why this crate has no
//! `unsafe` in it at all any more. Its own boundary used to be the
//! project's only `unsafe` outside `hclient-fetch` (spec amendment C8);
//! that moved with the code.
//!
//! The RDATA a resolver reports is decoded by `domain`, not by hand, and
//! it is decoded **as RDATA**: `Https::parse` reads one record's octets,
//! which is the only shape all five platforms below have. The decoder
//! this replaced read whole messages and nothing smaller, so this crate
//! used to assemble a synthetic DNS response around every record it
//! wanted read. `svcb` is left with the part
//! a DNS decoder correctly declines to do: deciding, per RFC 9460, which
//! decoded records a *client* may act on (§2.4/§2.5 modes and root
//! targets, §8 `mandatory` semantics), and classifying what "no records"
//! means.
//!
//! # glibc: what this crate needs, measured rather than assumed
//!
//! **The supported minimum is glibc 2.34**, and the honest shape of that
//! claim is a policy rather than a hard wall: `res_query` has been in
//! glibc since `libresolv` had a version number, so nothing here *fails*
//! on an older one. What 2.34 is, is the version from which the symbol
//! lives in `libc.so.6` — measured on 2.43: `res_query@@GLIBC_2.34` there,
//! and no `res_query` in `libresolv.so.2` at all. A binary built against
//! this crate on such a host carries `res_query@GLIBC_2.34` as an
//! undefined symbol and **does not load `libresolv` at run time**, because
//! `--as-needed` drops a library that contributed nothing.
//!
//! Below 2.34 the same source links the symbol out of `libresolv.so.2`
//! instead, so the deployment gains a run-time dependency on that library.
//! That configuration is expected to work and **is not tested here**,
//! which is what "supported minimum" means: it is the version this project
//! builds and runs against, not a version below which the code is known
//! broken.
//!
//! **This crate raises nobody's floor**, which is the part worth knowing
//! before blaming it for one: on a 2.34-or-later build host the standard
//! library already pins the binary there through
//! `__libc_start_main@GLIBC_2.34`, measured on a probe that links no
//! `res_query` at all. The effective floor of any glibc program is its
//! build host's glibc, exactly as it always is.
//!
//! **The one requirement a packager can get wrong is at link time**, and
//! it arrives through `system-resolver` rather than from any file here.
//! Its `res_query` backend puts `-lresolv` on the link line for
//! `target_env = "gnu"`, so `libresolv.so` — the development symlink, not
//! the runtime `.so.2` — must be installed to build, even on a glibc where
//! the library contributes not one symbol to the result. musl gets no such
//! flag, because Rust's self-contained musl sysroot ships no `libresolv.a`
//! and the symbol is inside `libc.a`; the reasoning for each target is in
//! that file, beside the `#[cfg_attr]`s that carry it.
//!
//! **`supports_svcb()` says what this build can do, and nothing more.**
//! It asks `system_resolver::support()` whether RR type 65 is answerable,
//! so the capability and the code behind it are one statement rather than
//! two that could drift. `true` on Linux (glibc or musl), Apple, Android
//! and Windows; `false` everywhere else.
//!
//! A `true` over a lookup that cannot produce a record would be the exact
//! defect class — a capability that lies — that the
//! `Resolve::supports_svcb` doc comment in `hclient-dns` exists to
//! prevent.
//!
//! **Windows resolves a symbol at run time again, and it does not change
//! this answer.** `system-resolver` uses `DnsQueryRaw` where the machine
//! has it and `DnsQuery_UTF8` where it does not, which is a genuine
//! run-time choice — but type 65 is answerable through *both*, so what
//! this method reports is still decided by the build. That is a fact about
//! HTTPS records rather than about the platform, and it is why the test
//! guarding this can still compare against a `cfg!`.
//!
//! **Both SVCB backends block too**, so `lookup_svcb` goes through the
//! same `Blocking` capability as `lookup_ipv4`/`lookup_ipv6` and has the
//! same three outcomes (see `lookup` below) — with one addition specific
//! to it: a name with no HTTPS records yields an EMPTY stream, not an
//! error. That case is the common one, and `res_query` reports it as a
//! failure; turning that report into an `Error` would tell every caller
//! its DNS was broken for every host that simply has no HTTPS record. See
//! `svcb::endpoints_from_answer` for where that line is drawn.
//!
//! **A known limitation — and it's worse than it sounds: TODAY, two
//! `getaddrinfo` calls for one name, and neither gives an early result.**
//! `lookup_ipv4` and `lookup_ipv6` don't share one resolution attempt —
//! each calls `self.lookup` independently, and `std::net::ToSocketAddrs`
//! for a `(host, port)` pair resolves BOTH families at once via a single
//! system `getaddrinfo`. That means any Happy Eyeballs consumer that
//! calls both methods for one name (which `Scheduler` is required to do)
//! actually triggers TWO full dual-family `getaddrinfo` calls — measured
//! with a counter wrapped around `Blocking::run`: `2` calls for one name
//! via
//! `lookup_ipv4` + `lookup_ipv6`. Each call gets both families back and
//! throws away half of it with the `is_ipv6() == want_v6` filter — i.e.
//! the A records from the v6 call and the AAAA records from the v4 call
//! are both discarded for nothing. Neither of the two calls returns an
//! early partial result — curl 8.20 makes **two** calls ON PURPOSE, on
//! separate threads, each for ONE family, which is exactly what gives it
//! its win (v6 can answer before v4, so Happy Eyeballs starts sooner);
//! here both calls wait on the same full dual-family answer, so there is
//! no time advantage at all — only the doubled cost of the system call. A
//! single resolution feeding both streams (or, like curl, two
//! single-family calls) is v0.2 work, not current behavior; the shape of
//! the `Resolve` trait already allows for it today (separate
//! `lookup_ipv4`/`lookup_ipv6`, not one method returning both families at
//! once), but `SystemDns` doesn't use that possibility yet.
// **`forbid` again, and the relaxation left with the code that needed
// it.** This crate carried `deny` under spec amendment C8 because
// `sys/res_query.rs` and `sys/windows.rs` each needed a scoped `#[allow]`
// for their foreign declarations — and `forbid` cannot be relaxed from
// inside a crate (`E0453`). Those files are `system-resolver`'s now, and
// with them every `unsafe` this crate ever had. What is left talks to a
// Rust API and a decoder, so the strongest form of the rule is available
// again and is taken.
#![forbid(unsafe_code)]

mod error;
mod svcb;

use futures_core::Stream;

/// The two stream shapes this crate hands back, named so the marker sits
/// on a line `cargo fmt` has no reason to reflow — the rule amendment C12
/// records about where a bound is written.
type SendAddrs<'a> =
    std::pin::Pin<Box<dyn futures_core::Stream<Item = Result<ResolvedAddr, Error>> + Send + 'a>>; // send-bound-exception: amendment-C15
type SendRecords<'a> =
    std::pin::Pin<Box<dyn futures_core::Stream<Item = Result<SvcbEndpoint, Error>> + Send + 'a>>; // send-bound-exception: amendment-C15
use crate::error::ResolveFailed;
#[cfg(test)]
use futures_core::future::BoxFuture;
use futures_util::StreamExt;
use hclient_core::{Error, ErrorKind};
use hclient_dns::{Resolve, ResolvedAddr, SvcbEndpoint};
use hclient_rt::{Blocking, Cancelled};
use std::net::{IpAddr, ToSocketAddrs};

#[derive(Debug, Clone)]
pub struct SystemDns<B> {
    blocking: B,
}

impl<B> SystemDns<B> {
    pub fn new(blocking: B) -> Self {
        Self { blocking }
    }
}

impl<B: Blocking> SystemDns<B> {
    /// `Blocking::run` returns `Result<T,
    /// Cancelled>`, and `T` here is itself `Result<Vec<IpAddr>,
    /// ResolveFailed>` — so `res` below is nested two layers deep, and it
    /// has EXACTLY three inhabited shapes, not two:
    ///
    /// - `Ok(Ok(addrs))` — `getaddrinfo` ran and returned something (may
    ///   be an empty list — that's not an error).
    /// - `Ok(Err(ResolveFailed))` — `getaddrinfo` genuinely failed: the
    ///   name doesn't resolve, this is `ErrorKind::Resolve`.
    /// - `Err(Cancelled)` — the background thread pool went away before
    ///   the task got to run (usually the runtime shutting down). This is
    ///   NOT `ErrorKind::Resolve`: the name may be perfectly fine, there
    ///   just will never be an answer. Conflating it with a DNS failure
    ///   would tell the caller its DNS is broken when actually the
    ///   process is shutting down; silently turning it into an empty
    ///   stream would make it indistinguishable from "the resolver asked
    ///   and found nothing" — the same principle that split
    ///   `supports_svcb()` and the empty `lookup_svcb` in `hclient-dns`,
    ///   applied here not to an absent capability but to a failed
    ///   attempt. So `Cancelled` is wrapped in `ErrorKind::Cancelled` (fix
    ///   round 1: it was originally `ErrorKind::Other`, the same class of
    ///   miss as `Other` versus `Resolve` — the standard "the caller must
    ///   be able to tell `kind()` apart without a downcast," applied
    ///   retroactively to itself. `Other` is the honest answer for a
    ///   TRULY opaque backend error; cancellation is a known, already
    ///   typed condition (`hclient_rt::Cancelled`) that every future
    ///   `Blocking` consumer will encounter, not just this one crate —
    ///   the full justification for the variant is in the
    ///   `ErrorKind::Cancelled` doc comment in `hclient-core`).
    // `use<'a>` so the stream does NOT capture `name`: it owns a copy on
    // the line below, and `Resolve`'s associated types are parameterised
    // by `&self`'s lifetime alone. Edition 2024 captures every lifetime in
    // scope unless told otherwise.
    fn lookup<'a>(
        &'a self,
        name: &str,
        want_v6: bool,
    ) -> impl Stream<Item = Result<ResolvedAddr, Error>> + use<'a, B> {
        let owned = name.to_owned();
        let fut = self.blocking.run(move || {
            (owned.as_str(), 0u16)
                .to_socket_addrs()
                .map(|it| it.map(|s| s.ip()).collect::<Vec<IpAddr>>())
                .map_err(|e| ResolveFailed(owned.clone(), e))
        });
        futures_util::stream::once(fut).flat_map(move |res| match res {
            Ok(Ok(addrs)) => futures_util::stream::iter(
                addrs
                    .into_iter()
                    .filter(|a| a.is_ipv6() == want_v6)
                    .map(|addr| Ok(ResolvedAddr { addr, ttl: None }))
                    .collect::<Vec<_>>(),
            ),
            Ok(Err(e)) => futures_util::stream::iter(vec![Err(Error::new(ErrorKind::Resolve, e))]),
            Err(Cancelled) => {
                futures_util::stream::iter(vec![Err(Error::new(ErrorKind::Cancelled, Cancelled))])
            }
        })
    }
}

impl<B: Blocking> Resolve for SystemDns<B> {
    type Ipv4<'a>
        = SendAddrs<'a>
    // send-bound-exception: amendment-C15
    where
        Self: 'a;

    fn lookup_ipv4<'a>(&'a self, name: &str) -> Self::Ipv4<'a> {
        Box::pin(self.lookup(name, false))
    }
    type Ipv6<'a>
        = SendAddrs<'a>
    // send-bound-exception: amendment-C15
    where
        Self: 'a;

    fn lookup_ipv6<'a>(&'a self, name: &str) -> Self::Ipv6<'a> {
        Box::pin(self.lookup(name, true))
    }

    /// Both SVCB methods are overridden together, which is the only way
    /// `Resolve` permits either to be: the constant below and the query
    /// inside `lookup_svcb` are the same `#[cfg]`-selected module's two
    /// items, so a target where the lookup cannot work reports `false`
    /// here without anyone having to remember to change it.
    fn supports_svcb(&self) -> bool {
        system_resolver::support().allows(svcb::TYPE_HTTPS)
    }

    type Svcb<'a>
        = SendRecords<'a>
    // send-bound-exception: amendment-C15
    where
        Self: 'a;

    fn lookup_svcb<'a>(&'a self, name: &str) -> Self::Svcb<'a> {
        Box::pin({
            let owned = name.to_owned();
            let fut = self.blocking.run(move || svcb::lookup(&owned));
            futures_util::stream::once(fut).flat_map(move |res| match res {
                // An empty `Vec` here is a real answer — "asked, found none" —
                // and becomes an empty stream, exactly like the `Resolve`
                // default. It is `supports_svcb()` above that keeps the two
                // distinguishable, which is the whole reason that method
                // exists.
                Ok(Ok(endpoints)) => {
                    futures_util::stream::iter(endpoints.into_iter().map(Ok).collect::<Vec<_>>())
                }
                Ok(Err(e)) => {
                    futures_util::stream::iter(vec![Err(Error::new(ErrorKind::Resolve, e))])
                }
                // Same reasoning as `lookup`: the pool going away is not a DNS
                // failure and not an absence of records.
                Err(Cancelled) => futures_util::stream::iter(vec![Err(Error::new(
                    ErrorKind::Cancelled,
                    Cancelled,
                ))]),
            })
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures_util::StreamExt;
    use hclient_rt::{Blocking, Cancelled};

    struct Inline;
    impl Blocking for Inline {
        fn run<T: Send + 'static, F: FnOnce() -> T + Send + 'static>(
            &self,
            f: F,
        ) -> BoxFuture<'_, Result<T, Cancelled>> {
            Box::pin(async move { Ok(f()) })
        }
    }

    #[test]
    fn resolves_localhost_into_the_right_family_streams() {
        let r = SystemDns::new(Inline);
        let v4: Vec<_> = futures_executor::block_on(r.lookup_ipv4("localhost").collect());
        let v4: Vec<_> = v4.into_iter().filter_map(Result::ok).collect();
        assert!(
            v4.iter().all(|a| a.addr.is_ipv4()),
            "the v4 stream must contain only v4"
        );

        let v6: Vec<_> = futures_executor::block_on(r.lookup_ipv6("localhost").collect());
        let v6: Vec<_> = v6.into_iter().filter_map(Result::ok).collect();
        assert!(
            v6.iter().all(|a| a.addr.is_ipv6()),
            "the v6 stream must contain only v6"
        );

        // `localhost` resolves differently on different machines and
        // different CI images: v4 only, v6 only, or both, with no
        // ordering guarantee within a family (see the `Resolve` doc
        // comment in `hclient-dns` — "Ordering guarantee within a stream:
        // there isn't one"). Only invariants are checked here (the
        // per-family partitioning above, and that something resolves at
        // all), not the exact contents — otherwise the test would pass on
        // one machine and fail on another for a reason unrelated to the
        // code.
        assert!(
            !v4.is_empty() || !v6.is_empty(),
            "localhost must resolve to something"
        );
    }

    #[test]
    fn unresolvable_name_yields_an_error_not_an_empty_stream() {
        let r = SystemDns::new(Inline);
        let got: Vec<_> = futures_executor::block_on(r.lookup_ipv4("invalid.invalid.").collect());
        assert!(
            got.iter().any(|x| x.is_err()),
            "an empty stream is indistinguishable from \"policy filtered everything out\""
        );
        let err = got.into_iter().find_map(Result::err).unwrap();
        assert_eq!(
            err.kind(),
            &ErrorKind::Resolve,
            "a genuine getaddrinfo failure must be classified as Resolve"
        );
    }

    /// Locks the module doc's "two calls, neither early" claim to the
    /// actual code, so a change that shares one resolution across both
    /// families (the stated v0.2 direction) forces this test — and the doc
    /// comment it mirrors — to be updated together rather than drifting
    /// apart silently: a doc describing a single shared call the code never
    /// makes is exactly the drift this pins.
    #[test]
    fn both_families_of_one_name_cost_two_separate_blocking_calls_today() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicUsize, Ordering};

        struct Counting(Arc<AtomicUsize>);
        impl Blocking for Counting {
            fn run<T: Send + 'static, F: FnOnce() -> T + Send + 'static>(
                &self,
                f: F,
            ) -> BoxFuture<'_, Result<T, Cancelled>> {
                Box::pin(async move {
                    self.0.fetch_add(1, Ordering::SeqCst);
                    Ok(f())
                })
            }
        }

        let count = Arc::new(AtomicUsize::new(0));
        let r = SystemDns::new(Counting(count.clone()));
        let _v4: Vec<_> = futures_executor::block_on(r.lookup_ipv4("localhost").collect());
        let _v6: Vec<_> = futures_executor::block_on(r.lookup_ipv6("localhost").collect());
        assert_eq!(
            count.load(Ordering::SeqCst),
            2,
            "lookup_ipv4/lookup_ipv6 do not share one resolution — each fires its own getaddrinfo"
        );
    }

    /// The capability must say what this build can actually do, and the
    /// target list here is a deliberate SECOND copy of the one in
    /// `system_resolver::sys`. The first copy decides which backend
    /// compiles; this one states, in a place a reviewer of *this* crate
    /// reads, what that is expected to mean. Adding a target there without
    /// touching this test fails on that target; the two are meant to be
    /// edited together, and CI's three-OS matrix exercises both answers.
    ///
    /// **Windows resolves a symbol at run time again, and this test did
    /// not have to change — which is the interesting part.** Its own
    /// earlier note said that if run-time detection came back, this is the
    /// test that would have to change with it. It came back:
    /// `system-resolver` reaches `DnsQueryRaw` through `GetProcAddress`
    /// where the machine has it and falls back to `DnsQuery_UTF8` where it
    /// does not.
    ///
    /// The prediction was right about the mechanism and wrong about the
    /// consequence. What that choice decides is which *types* are
    /// answerable, and RR type 65 is answerable through both calls — it is
    /// not one of the types Windows parses into a structure of its own. So
    /// the capability this test guards is still decided by the build, and
    /// a `cfg!` is still the honest comparison. A future backend that made
    /// type 65 itself a run-time question is the one that would have to
    /// change this — deliberately, not by being weakened until it passes.
    #[test]
    fn supports_svcb_is_cfg_accurate_rather_than_optimistic() {
        let expected = cfg!(any(
            all(
                target_os = "linux",
                any(target_env = "gnu", target_env = "musl")
            ),
            target_vendor = "apple",
            windows
        ));
        assert_eq!(
            SystemDns::new(Inline).supports_svcb(),
            expected,
            "supports_svcb() must be true exactly where a backend compiles — a `true` over \
             a lookup that cannot produce a record is the defect class \
             `Resolve::supports_svcb` exists to prevent, and a `false` where the backend \
             does exist hides a working capability"
        );
    }

    /// The half of the pair the capability check cannot cover: on a build
    /// WITH a backend the lookup has to be able to produce records, and on
    /// one without it has to produce an empty stream and no error. Neither
    /// half needs the network — the first is proved by
    /// `svcb::tests::parses_a_real_https_answer_captured_from_the_system_resolver`
    /// over a captured answer, the second is what this asserts.
    #[test]
    fn without_a_backend_the_lookup_is_empty_and_not_an_error() {
        if SystemDns::new(Inline).supports_svcb() {
            return;
        }
        let got: Vec<_> =
            futures_executor::block_on(SystemDns::new(Inline).lookup_svcb("example.com").collect());
        assert!(
            got.is_empty(),
            "an absent capability is an empty stream, never an error: telling a caller its \
             DNS is broken because this build has no backend would be a different lie"
        );
    }

    #[test]
    fn svcb_cancellation_is_not_mistaken_for_an_empty_or_dns_error_stream() {
        struct AlwaysCancelled;
        impl Blocking for AlwaysCancelled {
            fn run<T: Send + 'static, F: FnOnce() -> T + Send + 'static>(
                &self,
                _f: F,
            ) -> BoxFuture<'_, Result<T, Cancelled>> {
                Box::pin(async move { Err(Cancelled) })
            }
        }

        let r = SystemDns::new(AlwaysCancelled);
        let got: Vec<_> = futures_executor::block_on(r.lookup_svcb("example.com").collect());
        assert_eq!(got.len(), 1, "the pool going away is not silence");
        let err = got
            .into_iter()
            .next()
            .unwrap()
            .expect_err("must be an error");
        assert_eq!(
            err.kind(),
            &ErrorKind::Cancelled,
            "same reasoning as lookup_ipv4: cancellation is neither a DNS failure nor an \
             absence of records"
        );
    }

    /// Network-dependent, and therefore off by default: CI has no
    /// guaranteed outbound DNS, and a test that needs one is a test that
    /// goes red for reasons unrelated to this code. Run it deliberately
    /// with `cargo test -p hclient-dns-system -- --ignored`.
    #[test]
    #[ignore = "requires outbound DNS to a resolver that answers HTTPS (RR type 65) queries"]
    fn live_lookup_of_a_name_that_publishes_https_records() {
        let r = SystemDns::new(Inline);
        assert!(
            r.supports_svcb(),
            "this test is only meaningful on a build that has a backend"
        );
        let got: Vec<_> = futures_executor::block_on(r.lookup_svcb("cloudflare.com").collect());
        let endpoints: Vec<_> = got
            .into_iter()
            .collect::<Result<Vec<_>, _>>()
            .expect("a live answer");
        assert!(
            !endpoints.is_empty(),
            "cloudflare.com publishes HTTPS records; an empty stream here means the local \
             resolver strips RR type 65 rather than that the parser is wrong"
        );
        assert!(
            endpoints.iter().any(|e| e.alpn.iter().any(|a| a == b"h3")),
            "the point of the whole path is h3 discovery without Alt-Svc"
        );
    }

    #[test]
    fn cancellation_is_not_mistaken_for_an_empty_or_dns_error_stream() {
        struct AlwaysCancelled;
        impl Blocking for AlwaysCancelled {
            fn run<T: Send + 'static, F: FnOnce() -> T + Send + 'static>(
                &self,
                _f: F,
            ) -> BoxFuture<'_, Result<T, Cancelled>> {
                Box::pin(async move { Err(Cancelled) })
            }
        }

        let r = SystemDns::new(AlwaysCancelled);
        let got: Vec<_> = futures_executor::block_on(r.lookup_ipv4("example.com").collect());
        assert_eq!(
            got.len(),
            1,
            "the pool going away is neither an empty stream nor silence"
        );
        let err = got
            .into_iter()
            .next()
            .unwrap()
            .expect_err("must be an error");
        // A negative check (`assert_ne!` against `Resolve`) is not enough
        // here — it also passes for `ErrorKind::Other`, which is what a
        // wrapped cancellation looks like. A precise check of the exact
        // code
        // is not weaker than the negative one, but stricter: `Cancelled`
        // is a concrete variant introduced specifically for this
        // condition (see the `ErrorKind::Cancelled` doc comment in
        // `hclient-core`), and the test must name it directly.
        assert_eq!(
            err.kind(),
            &ErrorKind::Cancelled,
            "the pool going away is neither a DNS failure (Resolve) nor an opaque error (Other), but Cancelled"
        );
        assert!(err.is_cancelled());
        assert_ne!(
            err.kind(),
            &ErrorKind::Resolve,
            "pool cancellation must not be confused with a DNS failure — the name may be perfectly fine"
        );
    }
}
