//! System resolver on top of `std::net::ToSocketAddrs` (i.e. `getaddrinfo`).
//!
//! `getaddrinfo` blocks on every platform, so this crate requires the
//! `Blocking` capability — and is therefore unavailable wherever that
//! capability isn't (wasm).
//!
//! **A limitation worth knowing:** `getaddrinfo` will never return
//! HTTPS/SVCB records. That means neither ECH nor first-request HTTP/3
//! discovery is reachable through the system resolver. `lookup_svcb` is
//! honestly empty — and `supports_svcb()` is honestly `false`, the
//! `Resolve` default left unoverridden: overriding only `lookup_svcb`
//! would assert a capability that isn't there (see the
//! `Resolve::supports_svcb` doc comment in `http-ng-dns`).
//!
//! **A known limitation — and it's worse than it sounds: TODAY, two
//! `getaddrinfo` calls for one name, and neither gives an early result.**
//! `lookup_ipv4` and `lookup_ipv6` don't share one resolution attempt —
//! each calls `self.lookup` independently, and `std::net::ToSocketAddrs`
//! for a `(host, port)` pair resolves BOTH families at once via a single
//! system `getaddrinfo`. That means any Happy Eyeballs consumer that
//! calls both methods for one name (which the Task 5 `Scheduler` is
//! required to do) actually triggers TWO full dual-family `getaddrinfo`
//! calls — measured with a counter wrapped around `Blocking::run` (see
//! the task report, fix round 1): `2` calls for one name via
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
#![forbid(unsafe_code)]

use futures_core::Stream;
use futures_util::StreamExt;
use http_ng_core::{Error, ErrorKind};
use http_ng_dns::{Resolve, ResolvedAddr};
use http_ng_rt::{Blocking, Cancelled};
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

#[derive(Debug)]
struct ResolveFailed(String, std::io::Error);
impl std::fmt::Display for ResolveFailed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "failed to resolve `{}`: {}", self.0, self.1)
    }
}
impl std::error::Error for ResolveFailed {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.1)
    }
}

impl<B: Blocking> SystemDns<B> {
    /// `Blocking::run` (Task 1, `amendment-C5`) returns `Result<T,
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
    ///   `supports_svcb()` and the empty `lookup_svcb` in `http-ng-dns`,
    ///   applied here not to an absent capability but to a failed
    ///   attempt. So `Cancelled` is wrapped in `ErrorKind::Cancelled` (fix
    ///   round 1: it was originally `ErrorKind::Other`, the same class of
    ///   miss as `Other` versus `Resolve` — the standard "the caller must
    ///   be able to tell `kind()` apart without a downcast," applied
    ///   retroactively to itself. `Other` is the honest answer for a
    ///   TRULY opaque backend error; cancellation is a known, already
    ///   typed condition (`http_ng_rt::Cancelled`) that every future
    ///   `Blocking` consumer will encounter, not just this one crate —
    ///   the full justification for the variant is in the
    ///   `ErrorKind::Cancelled` doc comment in `http-ng-core`).
    fn lookup(&self, name: &str, want_v6: bool) -> impl Stream<Item = Result<ResolvedAddr, Error>> {
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
    fn lookup_ipv4(&self, name: &str) -> impl Stream<Item = Result<ResolvedAddr, Error>> {
        self.lookup(name, false)
    }
    fn lookup_ipv6(&self, name: &str) -> impl Stream<Item = Result<ResolvedAddr, Error>> {
        self.lookup(name, true)
    }
    // `supports_svcb`/`lookup_svcb` deliberately not overridden: the
    // `Resolve` defaults (`false` / empty stream) are the precise, honest
    // answer for `getaddrinfo`, which can't return SVCB/HTTPS records in
    // principle.
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures_util::StreamExt;
    use http_ng_rt::{Blocking, Cancelled};

    struct Inline;
    impl Blocking for Inline {
        async fn run<T: Send + 'static, F: FnOnce() -> T + Send + 'static>(
            &self,
            f: F,
        ) -> Result<T, Cancelled> {
            Ok(f())
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
        // comment in `http-ng-dns` — "Ordering guarantee within a stream:
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
    /// apart silently (fix round 1: the doc previously described a single
    /// shared call that the code never made).
    #[test]
    fn both_families_of_one_name_cost_two_separate_blocking_calls_today() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicUsize, Ordering};

        struct Counting(Arc<AtomicUsize>);
        impl Blocking for Counting {
            async fn run<T: Send + 'static, F: FnOnce() -> T + Send + 'static>(
                &self,
                f: F,
            ) -> Result<T, Cancelled> {
                self.0.fetch_add(1, Ordering::SeqCst);
                Ok(f())
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

    #[test]
    fn svcb_is_empty_because_getaddrinfo_cannot_return_it() {
        let r = SystemDns::new(Inline);
        let got: Vec<_> = futures_executor::block_on(r.lookup_svcb("example.com").collect());
        assert!(got.is_empty());
        // An empty stream is ambiguous on its own (see the
        // `Resolve::supports_svcb` doc comment in `http-ng-dns`): without
        // the paired capability check, this test would also pass for a
        // resolver that claims it can do SVCB but found nothing.
        // `getaddrinfo` honestly can't do SVCB at all — both halves of
        // the pair must confirm that together.
        assert!(
            !r.supports_svcb(),
            "an empty lookup_svcb without supports_svcb() == false is a lie by default"
        );
    }

    #[test]
    fn cancellation_is_not_mistaken_for_an_empty_or_dns_error_stream() {
        struct AlwaysCancelled;
        impl Blocking for AlwaysCancelled {
            async fn run<T: Send + 'static, F: FnOnce() -> T + Send + 'static>(
                &self,
                _f: F,
            ) -> Result<T, Cancelled> {
                Err(Cancelled)
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
        // Fix round 1: previously there was only a negative check
        // (`assert_ne!` against `Resolve`) here — it would also have
        // passed for `ErrorKind::Other`, which is what cancellation was
        // wrapped in before this round. A precise check of the exact code
        // is not weaker than the negative one, but stricter: `Cancelled`
        // is a concrete variant introduced specifically for this
        // condition (see the `ErrorKind::Cancelled` doc comment in
        // `http-ng-core`), and the test must name it directly.
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
