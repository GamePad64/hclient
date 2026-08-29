//! The decision to follow a redirect. A pure function: no I/O, no time.

use http::{HeaderName, HeaderValue, Method, StatusCode, Uri};

/// Headers stripped when moving to a different origin.
pub const SENSITIVE_HEADERS: [HeaderName; 3] = [
    http::header::AUTHORIZATION,
    http::header::COOKIE,
    http::header::PROXY_AUTHORIZATION,
];

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Allow {
    /// Keep the method a 301, 302 or 303 would otherwise rewrite to `GET`.
    ///
    /// curl's `--post301`/`--post302`/`--post303`. **Which** method
    /// results is not a policy's to say — RFC 9110 §15.4's table has one
    /// home, in [`decide`] — so this only says *do not rewrite*.
    pub preserve_method: bool,
    /// Let `Authorization` and other credentials survive a hop that
    /// crosses an origin.
    ///
    /// curl's `--location-trusted`, and the sharpest edge in any redirect
    /// implementation: this is how a bearer token reaches a redirect's
    /// target. Granted per hop rather than per client, so a policy can
    /// trust one destination without trusting every future one.
    pub keep_credentials: bool,
}

impl Allow {
    /// Both grants, for a policy that wants curl's `--location-trusted`
    /// and `--post30x` together. Named rather than spelled out, because a
    /// struct literal of two `true`s reads as a default and is the
    /// opposite of one.
    #[must_use]
    pub const fn everything() -> Self {
        Self {
            preserve_method: true,
            keep_credentials: true,
        }
    }

    /// Field-wise `AND`: a grant survives only if both policies gave it.
    #[must_use]
    pub const fn and(self, other: Self) -> Self {
        Self {
            preserve_method: self.preserve_method && other.preserve_method,
            keep_credentials: self.keep_credentials && other.keep_credentials,
        }
    }
}

/// What a policy says about one hop.
///
/// The three arms are ordered `Follow < Stop < Refuse`, and combining two
/// verdicts takes the **more conservative**: any `Refuse` wins, then any
/// `Stop`, and two `Follow`s meet their [`Allow`]s. That is the one rule
/// a reader has to learn about composition, and it holds for every field
/// of every policy.
///
/// **`Stop` and `Refuse` are the distinction the old `RedirectPolicy`
/// enum existed for.** `Stop` hands the `3xx` back as an ordinary
/// response, `Location` intact — what `Forbid` meant.
/// `Refuse` is an error — what `Limited(0)` meant. Folding them would put
/// a discontinuity inside one value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RedirectVerdict {
    /// Follow it, granting these relaxations and no others.
    Follow(Allow),
    /// Do not follow. The `3xx` is the caller's answer.
    Stop,
    /// Do not follow, and make it an error naming this reason.
    ///
    /// A `&'static str` rather than a `String` so the verdict stays
    /// `Copy`, which is what lets the lattice operations be `const` and
    /// allocation-free. A policy needing a computed message can carry it
    /// in its own state and return a borrow of a leaked literal, or use
    /// [`RedirectVerdict::Stop`] and let the caller read the `3xx`.
    Refuse(&'static str),
}

impl RedirectVerdict {
    /// Follow with no relaxations — the common answer, spelled once.
    #[must_use]
    pub const fn follow() -> Self {
        Self::Follow(Allow {
            preserve_method: false,
            keep_credentials: false,
        })
    }

    /// The more conservative of two verdicts.
    ///
    /// Order is unobservable, which is what separates this from a
    /// middleware chain: composing policies is a meet on a lattice, not
    /// function composition.
    #[must_use]
    pub const fn and(self, other: Self) -> Self {
        match (self, other) {
            (Self::Refuse(r), _) => Self::Refuse(r),
            (_, Self::Refuse(r)) => Self::Refuse(r),
            (Self::Stop, _) | (_, Self::Stop) => Self::Stop,
            (Self::Follow(a), Self::Follow(b)) => Self::Follow(a.and(b)),
        }
    }
}

/// A hop [`decide`] has worked out and is about to take, offered to the
/// policy.
///
/// **Everything here is `decide`'s *output*, never its input**, which is
/// what keeps a policy from computing any of it a second time: a policy
/// refusing cross-origin hops and the client stripping `Authorization`
/// read the same `cross_origin`, so they cannot disagree about what an
/// origin is.
#[derive(Debug, Clone, Copy)]
pub struct ProposedRedirect<'a> {
    from: &'a Uri,
    to: &'a Uri,
    status: StatusCode,
    method: &'a Method,
    cross_origin: bool,
    hops: u8,
    previous: &'a [Uri],
}

impl<'a> ProposedRedirect<'a> {
    /// Builds one.
    ///
    /// **Public because policies are written by callers**, and a seam
    /// whose implementations live outside this workspace needs a way to
    /// drive them in a unit test. The alternative is what `Response` was
    /// found to be: a type with no constructor, which reads as a wall and
    /// sends people to a network they did not need.
    ///
    /// `decide` is the only thing that builds one in anger, and what it
    /// passes is its own output — see the type's own doc for why nothing
    /// here should be recomputed by a policy.
    #[must_use]
    #[allow(
        clippy::too_many_arguments,
        reason = "seven facts about one hop, each named"
    )]
    pub fn new(
        from: &'a Uri,
        to: &'a Uri,
        status: StatusCode,
        method: &'a Method,
        cross_origin: bool,
        hops: u8,
        previous: &'a [Uri],
    ) -> Self {
        Self {
            from,
            to,
            status,
            method,
            cross_origin,
            hops,
            previous,
        }
    }

    /// The URI that answered with the `3xx`.
    #[must_use]
    pub fn from(&self) -> &'a Uri {
        self.from
    }
    /// Where the hop would go — absolute, with a relative `Location`
    /// already resolved against [`Self::from`].
    #[must_use]
    pub fn to(&self) -> &'a Uri {
        self.to
    }
    /// The `3xx` that proposed it.
    #[must_use]
    pub fn status(&self) -> StatusCode {
        self.status
    }
    /// The method the hop would carry **before** any rewrite this
    /// policy's [`Allow::preserve_method`] might prevent.
    #[must_use]
    pub fn method(&self) -> &'a Method {
        self.method
    }
    /// Whether the hop crosses an origin — scheme, host or port.
    ///
    /// Computed once, here, and handed over rather than recomputed.
    #[must_use]
    pub fn cross_origin(&self) -> bool {
        self.cross_origin
    }
    /// Hops already taken, so the first proposal reports `0`.
    #[must_use]
    pub fn hops(&self) -> u8 {
        self.hops
    }
    /// Every URI already visited in this chain, oldest first — for a
    /// policy that detects loops.
    #[must_use]
    pub fn previous(&self) -> &'a [Uri] {
        self.previous
    }
}

/// Whether to follow a redirect, and what to relax if so.
///
/// **One method, and everything else is mechanism.** Resolving a relative
/// `Location`, the RFC 9110 §15.4 method table, deciding what an origin
/// is — those stay in [`decide`], because a client that let them be
/// overridden would be letting a policy move the target, and getting them
/// wrong is an open redirect rather than a surprise.
///
/// Compose with [`and`](RedirectPolicyExt::and); the combined policy answers
/// with the more conservative of the two verdicts.
pub trait RedirectPolicy: core::fmt::Debug {
    /// Whether to take this hop.
    ///
    /// Defaulted to *yes, with nothing relaxed*, so an implementation
    /// states only what it has an opinion about.
    fn follow(&self, hop: &ProposedRedirect<'_>) -> RedirectVerdict {
        let _ = hop;
        RedirectVerdict::follow()
    }
}

/// A reference is a policy, so `&Limit::new(10)` composes like the value.
impl<T: RedirectPolicy + ?Sized> RedirectPolicy for &T {
    fn follow(&self, hop: &ProposedRedirect<'_>) -> RedirectVerdict {
        (**self).follow(hop)
    }
}

/// So is a `Box`, which is what makes [`All`]'s elements ordinary
/// policies rather than a special case.
impl<T: RedirectPolicy + ?Sized> RedirectPolicy for std::boxed::Box<T> {
    fn follow(&self, hop: &ProposedRedirect<'_>) -> RedirectVerdict {
        (**self).follow(hop)
    }
}

/// And an `Arc` — the shape a client stores one in, so the stored form is
/// usable everywhere the trait is rather than needing a `&*` at each site.
impl<T: RedirectPolicy + ?Sized> RedirectPolicy for std::sync::Arc<T> {
    fn follow(&self, hop: &ProposedRedirect<'_>) -> RedirectVerdict {
        (**self).follow(hop)
    }
}

/// [`and`](RedirectPolicyExt::and), kept off the trait so the trait stays
/// object-safe.
pub trait RedirectPolicyExt: RedirectPolicy + Sized {
    /// Both policies must agree; the more conservative answer wins.
    #[must_use]
    fn and<B: RedirectPolicy>(self, other: B) -> And<Self, B> {
        And(self, other)
    }
}

impl<T: RedirectPolicy + Sized> RedirectPolicyExt for T {}

/// Two policies, both of which must permit. See [`RedirectPolicyExt::and`].
#[derive(Debug, Clone, Copy)]
pub struct And<A, B>(pub A, pub B);

impl<A: RedirectPolicy, B: RedirectPolicy> RedirectPolicy for And<A, B> {
    fn follow(&self, hop: &ProposedRedirect<'_>) -> RedirectVerdict {
        let first = self.0.follow(hop);
        // Short-circuits at the top of the lattice and nowhere else: a
        // `Refuse` cannot be raised further, where a `Stop` can.
        if let RedirectVerdict::Refuse(_) = first {
            return first;
        }
        first.and(self.1.follow(hop))
    }
}

/// Never follow. The `3xx` is handed back as an ordinary response.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Forbid;

impl RedirectPolicy for Forbid {
    fn follow(&self, _: &ProposedRedirect<'_>) -> RedirectVerdict {
        RedirectVerdict::Stop
    }
}

/// Follow at most `n` hops; the next one is an error.
///
/// `Limit::new(0)` refuses the first redirect, which is deliberately not
/// [`Forbid`]: one is an error, the other is an answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limit(pub u8);

impl Limit {
    #[must_use]
    pub const fn new(n: u8) -> Self {
        Self(n)
    }
}

impl Default for Limit {
    /// Ten, which is what this crate has always defaulted to.
    fn default() -> Self {
        Self(10)
    }
}

impl RedirectPolicy for Limit {
    fn follow(&self, hop: &ProposedRedirect<'_>) -> RedirectVerdict {
        if hop.hops() >= self.0 {
            RedirectVerdict::Refuse("redirect limit reached")
        } else {
            RedirectVerdict::follow()
        }
    }
}

/// Refuse any hop that leaves the origin — scheme, host or port.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SameOriginOnly;

impl RedirectPolicy for SameOriginOnly {
    fn follow(&self, hop: &ProposedRedirect<'_>) -> RedirectVerdict {
        if hop.cross_origin() {
            RedirectVerdict::Refuse("redirect leaves the origin")
        } else {
            RedirectVerdict::follow()
        }
    }
}

/// Refuse a hop that downgrades `https` to anything else.
///
/// curl's `--proto-redir`, and it needs no method of its own: the scheme
/// is on the proposal, so this is an ordinary policy.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct HttpsOnly;

impl RedirectPolicy for HttpsOnly {
    fn follow(&self, hop: &ProposedRedirect<'_>) -> RedirectVerdict {
        if hop.from().scheme_str() == Some("https") && hop.to().scheme_str() != Some("https") {
            RedirectVerdict::Refuse("redirect downgrades https")
        } else {
            RedirectVerdict::follow()
        }
    }
}

/// Every policy in a list must permit, for a chain built at run time.
///
/// [`and`](RedirectPolicyExt::and) composes at the type level and costs
/// nothing; this is for the case that cannot — a list read from a config
/// file, where the length is not known when the code is written. Same
/// reason `hclient-native` keeps its proxies in a `Vec` rather than a
/// tuple.
///
/// An empty list permits everything, which is what a meet over nothing
/// means and is worth knowing before reading one out of a file.
#[derive(Debug, Default)]
pub struct All(pub Vec<Box<dyn RedirectPolicy + Send + Sync>>); // send-bound-exception: amendment-C12

impl RedirectPolicy for All {
    fn follow(&self, hop: &ProposedRedirect<'_>) -> RedirectVerdict {
        let mut verdict = RedirectVerdict::follow();
        for policy in &self.0 {
            verdict = verdict.and(policy.follow(hop));
            if let RedirectVerdict::Refuse(_) = verdict {
                return verdict;
            }
        }
        verdict
    }
}

/// A closure as a policy — what the separate redirect predicate used to
/// be, now one implementation among the rest.
#[derive(Clone, Copy)]
pub struct FromFn<F>(pub F);

/// A closure has no `Debug`, and [`RedirectPolicy`] requires one so that a
/// client can print the policy it holds. Same trade the separate redirect
/// predicate already made.
impl<F> core::fmt::Debug for FromFn<F> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str("FromFn(..)")
    }
}

impl<F> RedirectPolicy for FromFn<F>
where
    F: Fn(&ProposedRedirect<'_>) -> RedirectVerdict,
{
    fn follow(&self, hop: &ProposedRedirect<'_>) -> RedirectVerdict {
        (self.0)(hop)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct Follow {
    /// Where to go. May carry userinfo sent by the server
    /// (`https://user:pass@host/`) — it's not part of the origin per RFC
    /// 6454 and so doesn't factor into `strip_sensitive`, but it's foreign
    /// input: don't silently promote it into trusted credentials further
    /// down the stack.
    pub uri: Uri,
    pub method: Method,
    /// Strip `SENSITIVE_HEADERS`: the host or scheme changed.
    pub strip_sensitive: bool,
    /// The method was downgraded to GET — the body must not be sent.
    pub drop_body: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RedirectAction {
    /// Not a redirect, or a redirect with no `Location` — return the response as-is.
    Stop,
    Follow(Follow),
    /// A policy refused, naming why — a limit, an origin, a caller's own
    /// rule. One variant where there used to be `TooManyRedirects` alone,
    /// because with a policy trait "too many" is one refusal among
    /// several and the reason is what a caller needs. `to` is carried
    /// because the refusal is about a resolved target, and an error that
    /// named only a reason would leave a caller unable to say which hop.
    Refused {
        why: &'static str,
        to: Uri,
    },
    InvalidLocation,
}

/// Port with the scheme's default substituted in.
///
/// Neither side of the comparison is normalized any more. `current`
/// arrives from the caller as written, and since `url` was removed the
/// resolved target keeps whatever the `Location` header said — `url` used
/// to strip a default port on serialization, so this function used to
/// correct an asymmetry and now covers both sides. Without it,
/// `https://a:443/` → `https://a/` reads as an origin change and strips
/// Authorization on every hop.
fn port_of(uri: &Uri) -> Option<u16> {
    uri.port_u16().or_else(|| match uri.scheme_str() {
        Some("https") => Some(443),
        Some("http") => Some(80),
        _ => None,
    })
}

pub fn decide(
    policy: &dyn RedirectPolicy,
    hops: u8,
    current: &Uri,
    method: &Method,
    status: StatusCode,
    location: Option<&[u8]>,
    previous: &[Uri],
) -> RedirectAction {
    if !matches!(status.as_u16(), 301 | 302 | 303 | 307 | 308) {
        return RedirectAction::Stop;
    }
    let Some(location) = location else {
        return RedirectAction::Stop;
    };

    let Ok(header) = HeaderValue::from_bytes(location) else {
        return RedirectAction::InvalidLocation;
    };
    let Ok(location) = core::str::from_utf8(header.as_bytes()) else {
        return RedirectAction::InvalidLocation;
    };
    // Shared with `ClientBuilder::base_url`'s RFC 3986 §5 implementation —
    // see `crate::uri`'s doc comment: this client has exactly one rule for
    // resolving a relative reference, regardless of whether the server sent
    // it in `Location:` or the caller sent it in `client.get(..)`.
    let Ok(uri) = crate::uri::resolve_reference(current, location) else {
        return RedirectAction::InvalidLocation;
    };

    // The host is case-insensitive (RFC 3986 §6.2.2.1) and nothing
    // lower-cases it any more: `url` used to, on the target only, so
    // `https://A.test/` + `Location: y` compared `a.test` against `A.test`
    // and stripped Authorization from a request that never left the host
    // it started on. `scheme_str` needs no such care — `http::Uri`
    // lower-cases the scheme itself.
    let same_host = match (uri.host(), current.host()) {
        (Some(target), Some(from)) => target.eq_ignore_ascii_case(from),
        (target, from) => target == from,
    };
    let cross_origin =
        !same_host || uri.scheme_str() != current.scheme_str() || port_of(&uri) != port_of(current);

    // 303 is always GET (except HEAD). Browsers and reqwest downgrade
    // 301/302 with POST to GET; diverging from 303 here would be
    // inconsistent.
    // **The policy is asked here and nowhere else**, with everything
    // above already worked out — which is what lets it hold a rule rather
    // than a copy of the mechanism. Asking after the `Location` is
    // resolved rather than before costs one changed error in one corner:
    // a malformed `Location` on the hop that would have exceeded a limit
    // now reports `InvalidLocation` rather than the limit. No hop goes
    // anywhere different, and resolving a reference we will not follow is
    // a parse with no IO.
    let proposal = ProposedRedirect {
        from: current,
        to: &uri,
        status,
        method,
        cross_origin,
        hops,
        previous,
    };
    let allow = match policy.follow(&proposal) {
        RedirectVerdict::Follow(allow) => allow,
        RedirectVerdict::Stop => return RedirectAction::Stop,
        RedirectVerdict::Refuse(why) => return RedirectAction::Refused { why, to: uri },
    };

    // RFC 9110 §15.4's table, and it stays here: a policy says *do not
    // rewrite*, never *rewrite to this*.
    let downgrade = !allow.preserve_method
        && match status.as_u16() {
            303 => *method != Method::HEAD,
            301 | 302 => *method == Method::POST,
            _ => false,
        };
    let new_method = if downgrade {
        Method::GET
    } else {
        method.clone()
    };

    RedirectAction::Follow(Follow {
        uri,
        method: new_method,
        strip_sensitive: cross_origin && !allow.keep_credentials,
        drop_body: downgrade,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use http::{Method, StatusCode, Uri};

    /// The old six-argument shape, so the existing corpus reads
    /// unchanged: `previous` is only for a policy that detects loops, and
    /// none of these has one.
    fn d6(
        policy: &dyn RedirectPolicy,
        hops: u8,
        current: &Uri,
        method: &Method,
        status: StatusCode,
        location: Option<&[u8]>,
    ) -> RedirectAction {
        decide(policy, hops, current, method, status, location, &[])
    }

    fn p() -> Limit {
        Limit::new(10)
    }
    fn u(s: &str) -> Uri {
        s.parse().unwrap()
    }

    fn go(status: u16, from: &str, to: &str, m: Method) -> RedirectAction {
        d6(
            &p(),
            0,
            &u(from),
            &m,
            StatusCode::from_u16(status).unwrap(),
            Some(to.as_bytes()),
        )
    }

    #[test]
    fn does_not_follow_300_304_305() {
        for s in [300u16, 304, 305, 306] {
            assert!(
                matches!(
                    go(s, "https://a/", "https://b/", Method::GET),
                    RedirectAction::Stop
                ),
                "status {s} must not be followed"
            );
        }
    }

    #[test]
    fn follows_the_five_real_redirects() {
        for s in [301u16, 302, 303, 307, 308] {
            assert!(
                matches!(
                    go(s, "https://a/", "https://a/x", Method::GET),
                    RedirectAction::Follow(_)
                ),
                "status {s}"
            );
        }
    }

    #[test]
    fn strips_sensitive_on_host_change() {
        let RedirectAction::Follow(f) = go(302, "https://a/", "https://b/", Method::GET) else {
            panic!()
        };
        assert!(f.strip_sensitive);
    }

    #[test]
    fn strips_sensitive_on_scheme_change_same_host() {
        let RedirectAction::Follow(f) = go(302, "https://a/", "http://a/", Method::GET) else {
            panic!()
        };
        assert!(f.strip_sensitive, "downgrade https->http must strip");
    }

    #[test]
    fn keeps_sensitive_on_same_origin() {
        let RedirectAction::Follow(f) = go(302, "https://a/one", "https://a/two", Method::GET)
        else {
            panic!()
        };
        assert!(!f.strip_sensitive);
    }

    #[test]
    fn post_downgrades_to_get_on_301_302_303() {
        for s in [301u16, 302, 303] {
            let RedirectAction::Follow(f) = go(s, "https://a/", "https://a/x", Method::POST) else {
                panic!("status {s}")
            };
            assert_eq!(f.method, Method::GET, "status {s}");
            assert!(f.drop_body, "status {s}");
        }
    }

    #[test]
    fn post_is_preserved_on_307_308() {
        for s in [307u16, 308] {
            let RedirectAction::Follow(f) = go(s, "https://a/", "https://a/x", Method::POST) else {
                panic!()
            };
            assert_eq!(f.method, Method::POST);
            assert!(!f.drop_body);
        }
    }

    #[test]
    fn head_stays_head_on_303() {
        let RedirectAction::Follow(f) = go(303, "https://a/", "https://a/x", Method::HEAD) else {
            panic!()
        };
        assert_eq!(f.method, Method::HEAD);
    }

    #[test]
    fn resolves_relative_location() {
        let RedirectAction::Follow(f) = go(302, "https://a/one/two", "../three", Method::GET)
        else {
            panic!()
        };
        assert_eq!(f.uri, u("https://a/three"));
    }

    #[test]
    fn missing_location_stops() {
        let r = d6(
            &p(),
            0,
            &u("https://a/"),
            &Method::GET,
            StatusCode::FOUND,
            None,
        );
        assert!(matches!(r, RedirectAction::Stop));
    }

    #[test]
    fn limit_is_enforced() {
        let r = d6(
            &Limit::new(2),
            2,
            &u("https://a/"),
            &Method::GET,
            StatusCode::FOUND,
            Some(b"https://a/x"),
        );
        assert!(matches!(r, RedirectAction::Refused { .. }));
    }

    /// `None` is the whole reason this type is an enum, and until this test
    /// the crate that OWNS it never constructed the variant: collapsing
    /// `None` back into `Limited(0)` left all 92 of this crate's tests
    /// green, and was caught only by a test in `hclient` that reaches the
    /// semantics through `examples/portable.rs` — an example, the artifact
    /// most likely to be rewritten or trimmed.
    ///
    /// A 302 WITH a `Location` is the case that discriminates: without one,
    /// `decide` returns `Stop` for every policy, so the assertion would hold
    /// for the wrong reason.
    #[test]
    fn none_stops_without_following_and_does_not_report_too_many() {
        let r = d6(
            &Forbid,
            0,
            &u("https://a/"),
            &Method::GET,
            StatusCode::FOUND,
            Some(b"https://a/x"),
        );
        assert!(
            matches!(r, RedirectAction::Stop),
            "`None` must hand the 3xx back, not follow it and not error: {r:?}"
        );
    }

    /// The other side of the same distinction, in the owning crate:
    /// `Limited(0)` follows zero hops, so the first redirect IS an error.
    /// Together with the test above, neither variant's behaviour can be
    /// satisfied by the other's code path.
    #[test]
    fn limited_zero_errors_where_none_would_stop() {
        let r = d6(
            &Limit::new(0),
            0,
            &u("https://a/"),
            &Method::GET,
            StatusCode::FOUND,
            Some(b"https://a/x"),
        );
        assert!(
            matches!(r, RedirectAction::Refused { .. }),
            "`Limited(0)` must error where `None` stops: {r:?}"
        );
    }

    /// QUERY is safe and idempotent, so 301 and 302 preserve it — method
    /// AND body — exactly as they do for PUT or PATCH. The rewrite-to-GET
    /// in `decide` applies to POST alone, which is what RFC 9110 §15.4.2
    /// and §15.4.3 describe as the historical behaviour being codified.
    ///
    /// This holds today only because QUERY is not POST. It would be easy to
    /// "fix" into corruption by anyone who groups QUERY with POST for
    /// having a body — which is why it is pinned rather than left implied.
    /// Dropping the body of a QUERY does not degrade the request, it
    /// changes what was asked.
    #[test]
    fn query_survives_301_and_302_with_its_body() {
        for status in [301u16, 302] {
            let r = d6(
                &p(),
                0,
                &u("https://a/search"),
                &Method::QUERY,
                StatusCode::from_u16(status).unwrap(),
                Some(b"https://a/search2"),
            );
            let RedirectAction::Follow(f) = r else {
                panic!("expected a Follow for status {status}, got {r:?}");
            };
            assert_eq!(
                f.method,
                Method::QUERY,
                "QUERY is safe; only POST is rewritten to GET on {status}"
            );
            assert!(
                !f.drop_body,
                "the body IS the query on {status} — dropping it changes the question, \
                 it does not merely weaken the request"
            );
        }
    }

    /// 303 is the exception, and QUERY claims none: "retrieve the result
    /// with GET" is what the status means, so the method becomes GET and
    /// the body goes. The pair with the test above is the point — one
    /// without the other would let a blanket rule pass for half the wrong
    /// reason.
    #[test]
    fn query_is_still_downgraded_by_303_like_every_other_method() {
        let r = d6(
            &p(),
            0,
            &u("https://a/search"),
            &Method::QUERY,
            StatusCode::SEE_OTHER,
            Some(b"https://a/result"),
        );
        let RedirectAction::Follow(f) = r else {
            panic!("expected a Follow, got {r:?}");
        };
        assert_eq!(f.method, Method::GET);
        assert!(f.drop_body);
    }

    #[test]
    fn garbage_location_is_reported() {
        let r = d6(
            &p(),
            0,
            &u("https://a/"),
            &Method::GET,
            StatusCode::FOUND,
            Some(b"ht!tp://\x00"),
        );
        assert!(matches!(r, RedirectAction::InvalidLocation));
    }

    // ── asymmetry in default ports ─────────────────────────────────
    //
    // `current` arrives as-is from the caller (it may carry an explicit
    // `:443`), and so does the redirect target now that `url` — which
    // stripped the default port on serialization — is gone. Without
    // `port_of` this would read as an origin change and strip
    // Authorization on every hop.

    #[test]
    fn keeps_sensitive_when_current_has_explicit_default_port() {
        let RedirectAction::Follow(f) = go(302, "https://a:443/", "https://a/", Method::GET) else {
            panic!()
        };
        assert!(
            !f.strip_sensitive,
            "explicit :443 on current must not read as cross-origin"
        );
    }

    #[test]
    fn keeps_sensitive_when_location_has_explicit_default_port() {
        let RedirectAction::Follow(f) = go(302, "https://a/", "https://a:443/", Method::GET) else {
            panic!()
        };
        assert!(
            !f.strip_sensitive,
            "explicit :443 on the target must not read as cross-origin"
        );
    }

    #[test]
    fn keeps_sensitive_when_current_has_explicit_default_port_http() {
        let RedirectAction::Follow(f) = go(302, "http://a:80/", "http://a/", Method::GET) else {
            panic!()
        };
        assert!(
            !f.strip_sensitive,
            "explicit :80 on current must not read as cross-origin"
        );
    }

    #[test]
    fn a_genuinely_different_port_is_still_cross_origin() {
        let RedirectAction::Follow(f) = go(302, "https://a:8443/", "https://a/", Method::GET)
        else {
            panic!()
        };
        assert!(
            f.strip_sensitive,
            "8443 vs default 443 is a real origin change"
        );
    }

    // ── the same asymmetry, in the host ─────────────────────────────
    //
    // The host is case-insensitive (RFC 3986 §6.2.2.1) and nothing
    // lower-cases it any more: `url` did, on the RESOLVED TARGET ONLY,
    // while `current` came from the caller as written. So before `url` was
    // removed, `current = https://A.test/` plus ANY Location — even a
    // relative one — compared `a.test` against `A.test` and stripped
    // Authorization from a request that never left the host it started on.
    // Both directions are checked because the old asymmetry only broke one
    // of them, and a fix that only handles that one would look right.

    #[test]
    fn a_host_differing_only_in_case_is_not_a_change_of_origin() {
        for (from, to) in [
            ("https://A.test/", "https://a.test/x"),
            ("https://a.test/", "https://A.TEST/x"),
            ("https://A.test/", "y"),
        ] {
            let RedirectAction::Follow(f) = go(302, from, to, Method::GET) else {
                panic!("{from} -> {to} must be followed at all")
            };
            assert!(
                !f.strip_sensitive,
                "{from} -> {to}: a host that differs only in case is the same origin"
            );
        }
    }

    #[test]
    fn a_genuinely_different_host_is_still_cross_origin() {
        let RedirectAction::Follow(f) = go(302, "https://a.test/", "https://b.test/x", Method::GET)
        else {
            panic!()
        };
        assert!(
            f.strip_sensitive,
            "case-insensitivity must not turn into host-insensitivity"
        );
    }

    // ── the contents of SENSITIVE_HEADERS ──────────────────────────
    //
    // The mutation that matters: replacing the constant with three copies
    // of content-type leaves every other test green, because nothing reads
    // the
    // constant itself.

    #[test]
    fn sensitive_headers_are_exactly_the_three_credential_carriers() {
        assert_eq!(
            SENSITIVE_HEADERS,
            [
                http::header::AUTHORIZATION,
                http::header::COOKIE,
                http::header::PROXY_AUTHORIZATION,
            ]
        );
    }

    #[test]
    fn strip_sensitive_removes_only_the_credential_headers() {
        let RedirectAction::Follow(f) = go(302, "https://a/", "https://b/", Method::GET) else {
            panic!()
        };
        assert!(f.strip_sensitive);

        // Simulate what the caller code must do: strip only
        // SENSITIVE_HEADERS, leave the rest of the headers untouched.
        let mut headers = http::HeaderMap::new();
        headers.insert(http::header::AUTHORIZATION, "secret".parse().unwrap());
        headers.insert(
            http::header::CONTENT_TYPE,
            "application/json".parse().unwrap(),
        );
        if f.strip_sensitive {
            for name in &SENSITIVE_HEADERS {
                headers.remove(name);
            }
        }
        assert!(
            !headers.contains_key(http::header::AUTHORIZATION),
            "Authorization must be stripped"
        );
        assert!(
            headers.contains_key(http::header::CONTENT_TYPE),
            "unrelated headers must survive"
        );
    }

    // ── Location validation, and the ecosystem's tolerance ─────────
    //
    // `HeaderValue::from_bytes` closes CR/LF injection (C0 control bytes
    // and DEL). But `to_str()` additionally rejects any byte >= 0x80, and
    // raw non-ASCII in Location — a non-percent-encoded path, a "raw" IDN
    // host — shows up in practice; reqwest (through tower_http) follows
    // such a Location. We check both sides: non-ASCII passes, control
    // bytes don't.

    #[test]
    fn raw_utf8_path_is_followed() {
        let RedirectAction::Follow(f) = go(302, "https://a/", "/caf\u{e9}", Method::GET) else {
            panic!("raw UTF-8 path must not be rejected as InvalidLocation")
        };
        assert_eq!(f.uri, u("https://a/caf%C3%A9"));
    }

    /// The `idn` feature is what decides this one, and the two answers are
    /// written out rather than one being `cfg`-ed away, so neither build
    /// can quietly stop testing it. With the feature: the same A-label
    /// `url` used to produce. Without it: `InvalidLocation`, because the
    /// crate has no Unicode tables and will not guess — the error that
    /// says so and names the A-label is `uri::UriError::NonAsciiHost`,
    /// which `decide` flattens into `InvalidLocation` the same way it
    /// flattens every other resolution failure.
    #[cfg(feature = "idn")]
    #[test]
    fn raw_utf8_idn_host_is_followed() {
        let RedirectAction::Follow(f) = go(
            302,
            "https://a/",
            "https://m\u{fc}nchen.example/",
            Method::GET,
        ) else {
            panic!("raw UTF-8 IDN host must not be rejected as InvalidLocation")
        };
        assert_eq!(f.uri, u("https://xn--mnchen-3ya.example/"));
        assert!(f.strip_sensitive, "host actually changed");
    }

    #[cfg(not(feature = "idn"))]
    #[test]
    fn raw_utf8_idn_host_is_rejected_without_the_idn_feature() {
        let r = go(
            302,
            "https://a/",
            "https://m\u{fc}nchen.example/",
            Method::GET,
        );
        assert!(
            matches!(r, RedirectAction::InvalidLocation),
            "without `idn` there is nothing to convert a U-label with, and inventing \
             a host would be worse than refusing: {r:?}"
        );
        // The A-label form of the same host is ASCII and still followed —
        // the feature removes IDNA, not internationalised hosts.
        let RedirectAction::Follow(f) = go(
            302,
            "https://a/",
            "https://xn--mnchen-3ya.example/",
            Method::GET,
        ) else {
            panic!("an A-label needs no Unicode tables and must still be followed")
        };
        assert_eq!(f.uri, u("https://xn--mnchen-3ya.example/"));
    }

    #[test]
    fn bare_cr_in_location_is_rejected() {
        let r = d6(
            &p(),
            0,
            &u("https://a/"),
            &Method::GET,
            StatusCode::FOUND,
            Some(b"https://b/\r"),
        );
        assert!(matches!(r, RedirectAction::InvalidLocation));
    }

    #[test]
    fn bare_lf_in_location_is_rejected() {
        let r = d6(
            &p(),
            0,
            &u("https://a/"),
            &Method::GET,
            StatusCode::FOUND,
            Some(b"https://b/\n"),
        );
        assert!(matches!(r, RedirectAction::InvalidLocation));
    }

    #[test]
    fn crlf_header_injection_is_rejected() {
        let r = d6(
            &p(),
            0,
            &u("https://a/"),
            &Method::GET,
            StatusCode::FOUND,
            Some(b"https://b/\r\nX-Injected: 1"),
        );
        assert!(matches!(r, RedirectAction::InvalidLocation));
    }
}
