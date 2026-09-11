//! What a caller asks of **one request**, carried in its
//! [`http::Extensions`].
//!
//! **Separate from [`crate::caps`], and the line between them is
//! measured rather than felt.** A `*Support` value is what a transport
//! *can* do, reported once at construction and read by the layer above to
//! decide whether to ask; these are what a caller *asked for*, read back
//! out of the request by whichever transport serves it. Counted across
//! this workspace, the two sets do not overlap at a single site: these
//! three appear only where extensions are read, and `Capabilities` and its
//! enums only where `capabilities()` is called.
//!
//! **An extension is readable by any transport in the graph**, including
//! one this workspace did not write — which is why a credential is not
//! among them and digest's password travels as an argument instead.

use crate::error::VersionNotAvailable;

/// The timeout triple — `wasi:http`'s shape, the richest of the ambient
/// models.
///
/// Collapses to a single `AbortController` in fetch; on native it splits
/// into connector / response-wait / body-idle. A single `Duration` throws
/// away information the WASI backend knows how to use.
///
/// Lives in `hclient-core` because transports read it from the request's
/// `http::Extensions`, and they don't depend on `hclient`.
///
/// # `#[non_exhaustive]`, and what it does and does not close
///
/// A fifth bound is a real prospect — `resolve` joined three in v0.4 —
/// and an out-of-tree caller should not need a major version for one.
/// The attribute is what buys that, and the objection to it was that its
/// whole use is `Timeouts { connect: Some(d), ..Default::default() }`,
/// which the attribute closes from outside this crate along with the
/// plain literal: `E0639` for both, measured on a two-crate probe.
///
/// **What it does not close is field assignment**, and that is the fact
/// the objection missed. `let mut t = Timeouts::new(); t.connect =
/// Some(d);` compiles from another crate and inside a `const`, so the
/// whole of what was needed is a `const fn` constructor —
/// [`Self::new`] — because `Default::default()` is not one and
/// `hclient-dns-doh`'s `DEFAULT_TIMEOUTS` is a `const`. The `with_*`
/// setters chain on top of it so a call site reads as it did before.
///
/// This was a generated `bon` builder for one release and is not any
/// more. What that cost was a public typestate — every setter's signature
/// named `SetConnect<S>` and `S::Connect: IsUnset`, from a module `bon`
/// marks `#[doc(hidden)]`, so a caller could meet those names in a
/// signature and in a compiler error and could not write them. Five
/// crates in the graph and a hole in the public API, to reach a
/// constructor that is nine lines.
///
/// **The mirror is [`crate::caps::TimeoutSupport`], and it takes the same
/// shape for the reason the two look opposite.** A bound left unset here
/// means *the caller did not ask*; a claim left unset there means *this
/// transport does not enforce it* — and both are what a new field should
/// say on the day it arrives, because a caller has not heard of it and a
/// transport has not implemented it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct Timeouts {
    /// A bound on **getting an address to try**, separate from the connect
    /// budget that follows it.
    ///
    /// # What it bounds, which is not a phase boundary
    ///
    /// Happy Eyeballs interleaves resolution with connecting on purpose —
    /// the resolver is a `Stream` and `hclient-native` starts connecting to
    /// the first address while the rest are still arriving — so there is no
    /// instant at which *resolution finished*, and a bound on one would
    /// have nothing to attach to. What this bounds is the wait for the
    /// **first** address from either family, which is exactly the failure a
    /// caller cannot otherwise diagnose: a resolver that hangs looks like
    /// an origin that is unreachable, and only the first is worth a
    /// different retry.
    ///
    /// It therefore does **not** apply where the connection does not depend
    /// on the resolver — an IP literal, and an HTTPS record carrying
    /// address hints, both of which give a connector somewhere to go
    /// without an answer.
    ///
    /// # Why not simply a smaller `connect`
    ///
    /// Because the two answer different questions and a caller who cares
    /// wants both: `resolve` says *how long may I wait to learn where to
    /// go*, `connect` says *and how long may going there take*. Folding
    /// them loses which one failed, which is the whole gap. Overlapping
    /// budgets are the caller's to reconcile; nothing here subtracts one
    /// from the other, because a resolver that answered in 10 ms has not
    /// spent any of the connect budget in any sense a connector can see.
    pub resolve: Option<core::time::Duration>,
    pub connect: Option<core::time::Duration>,
    pub first_byte: Option<core::time::Duration>,
    pub between_bytes: Option<core::time::Duration>,
}

/// One bound of a [`Timeouts`], beside whether the transport enforces it
/// and the name a refusal carries.
///
/// # Why [`Timeouts::support_checks`] hands back these rather than an array
///
/// The whole reason [`Timeouts`] is `#[non_exhaustive]` is that a fifth
/// bound must not be a breaking change, and a `[_; 4]` return type would
/// have made it one — the count is exactly the thing not to promise. So
/// the method returns an iterator, the arity stays private, and a caller
/// writes the same loop before and after a bound is added.
///
/// Named fields rather than a tuple for the same reason one layer down:
/// `(bool, bool, &str)` puts two booleans side by side with nothing but
/// position to tell *the caller asked for this* from *the transport
/// enforces it*, and swapping them type-checks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct BoundSupport {
    /// The caller set this bound.
    pub requested: bool,
    /// The transport enforces it.
    pub supported: bool,
    /// The name a refusal carries, e.g. `"connect_timeout"`.
    pub what: &'static str,
}

impl Timeouts {
    /// No bound at all — every phase waits as long as it waits.
    ///
    /// `const`, and that is what the type needs rather than a nicety:
    /// [`Self`] is `#[non_exhaustive]`, which closes the struct literal
    /// *and* `..Default::default()` to a caller outside this crate
    /// (`E0639` for both), and `Default::default()` is not a `const fn`.
    /// `hclient-dns-doh`'s `DEFAULT_TIMEOUTS` is a `const`, so without
    /// this there is no expression that builds one there.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            resolve: None,
            connect: None,
            first_byte: None,
            between_bytes: None,
        }
    }

    /// Bound the wait for the first address. See [`Self::resolve`].
    #[must_use]
    pub const fn with_resolve(mut self, d: core::time::Duration) -> Self {
        self.resolve = Some(d);
        self
    }

    /// Bound the connect. See [`Self::connect`].
    #[must_use]
    pub const fn with_connect(mut self, d: core::time::Duration) -> Self {
        self.connect = Some(d);
        self
    }

    /// Bound the wait for the response head. See [`Self::first_byte`].
    #[must_use]
    pub const fn with_first_byte(mut self, d: core::time::Duration) -> Self {
        self.first_byte = Some(d);
        self
    }

    /// Bound the gap between body frames. See [`Self::between_bytes`].
    #[must_use]
    pub const fn with_between_bytes(mut self, d: core::time::Duration) -> Self {
        self.between_bytes = Some(d);
        self
    }

    /// Set the resolve bound to whatever this `Option` holds, `None`
    /// included. See [`Self::maybe_connect`].
    #[must_use]
    pub const fn maybe_resolve(mut self, d: Option<core::time::Duration>) -> Self {
        self.resolve = d;
        self
    }

    /// Set the connect bound to whatever this `Option` holds, `None`
    /// included.
    ///
    /// The `with_*` setters take a `Duration` because a caller writing one
    /// down knows it is setting a bound; a caller **forwarding** one
    /// already has an `Option` and would otherwise write a `match`. Every
    /// bound has this form because every bound is forwarded somewhere —
    /// `hclient`'s own timeout tests forward all four.
    #[must_use]
    pub const fn maybe_connect(mut self, d: Option<core::time::Duration>) -> Self {
        self.connect = d;
        self
    }

    /// Set the first-byte bound to whatever this `Option` holds, `None`
    /// included. See [`Self::maybe_connect`].
    #[must_use]
    pub const fn maybe_first_byte(mut self, d: Option<core::time::Duration>) -> Self {
        self.first_byte = d;
        self
    }

    /// Set the between-bytes bound to whatever this `Option` holds, `None`
    /// included. See [`Self::maybe_connect`].
    #[must_use]
    pub const fn maybe_between_bytes(mut self, d: Option<core::time::Duration>) -> Self {
        self.between_bytes = d;
        self
    }

    /// This value's bounds, falling back to `base` wherever this one is
    /// unset — the per-request-over-client merge, field by field.
    ///
    /// **In this crate for [`Self::support_checks`]' reason.** A merge
    /// that forgot a bound would drop the caller's setting silently, and
    /// written in `hclient` under `#[non_exhaustive]` it would have to
    /// carry a `..` — which a fifth bound joins without a word. Here the
    /// destructure has no rest pattern, so a new bound is `E0027` on this
    /// line, in the crate that grew it.
    #[must_use]
    pub fn or(&self, base: &Self) -> Self {
        // No `..`: a bound added to this struct must fail this line.
        let Self {
            resolve,
            connect,
            first_byte,
            between_bytes,
        } = self;
        Self {
            resolve: resolve.or(base.resolve),
            connect: connect.or(base.connect),
            first_byte: first_byte.or(base.first_byte),
            between_bytes: between_bytes.or(base.between_bytes),
        }
    }

    /// This value with the connect budget replaced and the resolve bound
    /// dropped — what a transport hands to one arm of a connection race.
    ///
    /// # Why this is a method and not a builder call
    ///
    /// The two call sites in `hclient-native` **narrow** a caller's
    /// `Timeouts`: the resolve already happened, before the race, and each
    /// arm gets what is left of the connect budget. Everything else the
    /// caller set has to travel through untouched.
    ///
    /// A builder cannot express that, `bon`'s included, because a builder
    /// starts from nothing: the site would have to re-list the bounds it
    /// means to preserve —
    /// `builder().maybe_first_byte(t.first_byte).maybe_between_bytes(..)`
    /// — and a fifth bound would then be **dropped silently, still
    /// compiling**. Measured on a two-crate probe before this method was
    /// written, in both shapes: the re-listing form loses the new bound
    /// and this one carries it with no edit at all.
    ///
    /// So it is the same rule as [`Self::support_checks`] a second time.
    /// A builder answers for whoever *creates* a value; every place that
    /// **transforms** one needs the transformation to live in the crate
    /// that owns the fields, or a new field arrives somewhere nobody
    /// looks.
    #[must_use]
    pub fn narrowed_to_connect(mut self, connect: core::time::Duration) -> Self {
        // Field-wise rather than a literal, so a bound added to this
        // struct is carried through here by construction — the property
        // the doc above is about.
        self.resolve = None;
        self.connect = Some(connect);
        self
    }

    /// Every bound this value sets, paired with the
    /// [`crate::caps::TimeoutSupport`] field that says whether a transport
    /// enforces it, and with the name a refusal carries.
    ///
    /// # This method is why the struct can be `#[non_exhaustive]`
    ///
    /// It holds the destructure that `hclient`'s
    /// `check_timeouts_supported` used to hold, moved into the crate
    /// where the attribute does not apply — see the type's own doc for
    /// what the move buys. The pattern below has no `..`, so a fifth
    /// bound is a compile error here rather than a bound nothing checks.
    ///
    /// # What it does not promise
    ///
    /// That the pairing is right. The compiler forces a new bound to be
    /// *named*; whether it is given its own capability field is this
    /// function's own correctness, pinned by
    /// `every_bound_names_its_own_support_field` below rather than
    /// asserted.
    pub fn support_checks(
        &self,
        support: &crate::caps::TimeoutSupport,
    ) -> impl Iterator<Item = BoundSupport> {
        // No `..`: a bound added to this struct must fail this line.
        let Self {
            resolve,
            connect,
            first_byte,
            between_bytes,
        } = self;
        [
            BoundSupport {
                requested: resolve.is_some(),
                supported: support.resolve,
                what: "resolve_timeout",
            },
            BoundSupport {
                requested: connect.is_some(),
                supported: support.connect,
                what: "connect_timeout",
            },
            BoundSupport {
                requested: first_byte.is_some(),
                supported: support.first_byte,
                what: "first_byte_timeout",
            },
            BoundSupport {
                requested: between_bytes.is_some(),
                supported: support.between_bytes,
                what: "between_bytes_timeout",
            },
        ]
        .into_iter()
    }
}

/// The caller's per-request statement that this request may go into TLS 1.3
/// early data (0-RTT).
///
/// Put into `http::Extensions` on the request. Absent, the request waits
/// for the handshake to complete, and **there is no configuration in which
/// a request the caller did not mark ends up in early data**. Present
/// against a transport whose
/// [`early_data`](crate::caps::Capabilities::early_data) is `false`, it is
/// a typed
/// [`UnsupportedCapability`](crate::error::UnsupportedCapability) rather than a silent no-op.
///
/// # What marking a request asserts, and what it does not
///
/// **It is an assertion that replaying this request is SAFE — not that
/// replaying it is POSSIBLE.** Those are different questions, and only the
/// caller can answer the first one.
///
/// [`RequestBody::retry_kind`](crate::body::RequestBody::retry_kind) answers the
/// second: `Free`, `ViaFactory`, `Impossible` — *can I send these bytes
/// again*. A transport needs that answer, because a rejected 0-RTT request
/// has to be replayed after the handshake and a
/// [`RetryKind::Impossible`](crate::body::RetryKind::Impossible) body cannot be.
/// So `RetryKind` is a **correctness** precondition here, and it is checked
/// as one.
///
/// It is emphatically **not** a safety condition, and reading it as one is
/// the mistake to avoid. `POST /transfer` with a
/// fully buffered body is `RetryKind::Free` — trivially replayable, and
/// precisely the request that must never enter early data, because *an
/// attacker* can replay it too. quinn says the same in one line: *"this
/// enables transmission of 0-RTT data, which is vulnerable to replay
/// attacks, and should therefore never invoke non-idempotent operations"*.
///
/// The notion that would answer the safety question — method safety and
/// idempotency — deliberately does not exist in this codebase, and its
/// absence is written down where the one v0.2 retry lives. RFC 8470 §2 puts
/// the default on the conservative side (*"clients MAY send requests with
/// safe HTTP methods … and MUST NOT send unsafe methods (or methods whose
/// safety is not known) in early data"*) and, in the same sentence, says
/// why a method table cannot be the whole answer: *"absent other
/// information"*. `GET` is not safe on plenty of real APIs, and only the
/// caller knows which. Hence this extension: **a caller-visible decision,
/// with a method check beneath it, rather than a table hidden in a
/// transport.**
///
/// # The third failure path
///
/// A request placed in early data can fail in three places, not one: no
/// usable key material (nothing was risked, fall back silently), the server
/// rejecting the 0-RTT keys (replay on the same connection once the
/// handshake finishes — the transport's job, invisible to the caller), and
/// **HTTP `425 Too Early`** (RFC 8470 §5.2), which arrives a full round
/// trip later and must be retried *not* in early data. The third is a
/// status-code branch in the client, not in a transport.
///
/// **A retry built for a `425` must remove this extension from the request
/// it replays.** RFC 8470 requires it, and it is not a formality: on
/// `hclient_native::H3` this mark is part of the connection pool's key, so a
/// replay that kept it would ask for the early-data connection and — if
/// that one has been evicted or closed since — would open a fresh one and
/// go out in early data again, to the server that just refused to risk it.
/// See `hclient_h3::early`.
///
/// # The other boundary: an origin
///
/// The mark does not cross one, and `hclient`'s redirect stage drops it on
/// the same condition that drops `Cookie` and `Authorization` — the host or
/// scheme changed.
///
/// The asymmetry is the point and the two halves are easy to conflate. This
/// is a claim about what a request does **at a server**, so a caller who
/// marked a request for origin A never judged origin B, and carrying it
/// across would act on a judgement nobody made. A *method* change is the
/// opposite case and the mark stays: a `303` rewriting `POST` to `GET`
/// leaves a request strictly less consequential than the one already
/// vouched for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AllowEarlyData;

/// The caller's per-request statement that this request needs a particular
/// HTTP version, and must fail rather than go out over another one.
///
/// Put into `http::Extensions` on the request, and read by the transport at
/// the moment the protocol becomes known — which is **before the head is
/// written**, on every transport here that honours one. Absent, the
/// transport picks as it always did.
///
/// # It is [`AllowEarlyData`]'s mechanism with the polarity reversed
///
/// Same shape: a mark in the request's extensions that a transport reads
/// and acts on before sending, `Copy`, defined in this crate because
/// transports read it and do not depend on `hclient`. The difference is
/// that one is a permission and this is a requirement, and that difference
/// is why both have to be per request rather than per client — see below.
///
/// # Why a demand and not a question
///
/// [`crate::caps::Capabilities::full_duplex`] and its neighbours report the **floor**:
/// the value that holds on the worst protocol a transport might negotiate.
/// That is right for a static answer and cannot be otherwise — Cargo
/// unifies features across a graph, so a library built on `hclient` can
/// never know whether some other crate turned `http2` on — but it leaves a
/// caller who genuinely needs HTTP/2 with no way to act.
///
/// The two answers that do not work:
///
/// - **Per response.** `Response::version()` already answers it, honestly,
///   and *after the fact*. A caller structured for bidirectional streaming
///   has to decide before it sends.
/// - **Per connection.** There is no connection handle in the public API,
///   so it means either a new seam or a query answered from a pool — and
///   the pooled answer is racy in the way that matters: the entry can be
///   evicted between the answer and the request that relied on it. It
///   would be a fact about the past presented as a promise about the next
///   request.
///
/// This is the third: the caller states the requirement, and the transport
/// converts "the floor says no" into "this connection says yes" for one
/// request, or fails it before committing to a shape that would deadlock.
///
/// # Why it cannot be a client-level setting
///
/// Turning an ALPN outcome into a request failure is **correct for gRPC**,
/// whose RPC cannot proceed over HTTP/1.1 at all, and **wrong for a
/// browser-shaped client**, which should degrade quietly. Only the caller
/// knows which of the two it is — the same argument that put
/// [`AllowEarlyData`] in the caller's hands rather than in a transport's
/// configuration.
///
/// # Exact match, deliberately, not a minimum
///
/// `RequireVersion(HTTP_2)` is satisfied by HTTP/2 and by nothing else. It
/// is tempting to read it as "at least", and there is no ordering that
/// makes that mean anything: a caller who needs h2 framing does not want
/// HTTP/3 instead, and a caller who needs HTTP/1.1 — to keep an upgrade
/// path open, say — wants strictly less than HTTP/2, not more. A "minimum"
/// reading would satisfy the first demand with the wrong protocol and be
/// unable to express the second at all.
///
/// # Refusal, and the two shapes it takes
///
/// - The **backend cannot honour demands at all**
///   ([`crate::caps::Capabilities::version_select`] is `false` — `hclient-fetch` and
///   `hclient-wasi`, neither of which chooses or even learns the version):
///   a typed [`UnsupportedCapability`](crate::error::UnsupportedCapability) from `Client`, the same arm a
///   `RedirectPolicy` against
///   [`crate::caps::RedirectSupport::Internal`] takes. It fires whatever version was
///   demanded, because the backend cannot answer for any of them.
/// - The **backend honours demands and this connection does not match**:
///   a typed [`crate::error::VersionNotAvailable`] under
///   [`ErrorKind::Unsupported`](crate::error::ErrorKind::Unsupported), raised by
///   the transport before the head goes out.
///
/// A transport that always speaks one version still *honours* demands —
/// `hclient_native::H3` reports `version_select: true` and answers
/// `RequireVersion(HTTP_3)` by proceeding and everything else with
/// [`crate::error::VersionNotAvailable`]. Reporting `false` there would refuse the one
/// demand it trivially satisfies.
///
/// # The origin boundary, and why this one crosses it
///
/// [`AllowEarlyData`] comes off on a cross-origin redirect, because
/// "replaying this is safe" is a claim about what a request does *at a
/// server* and the caller judged only the first one. **This mark is not
/// that kind of claim.** It is a statement about the caller's own code —
/// "the thing I am about to do needs this protocol" — and it is equally
/// true at hop 1 and at hop 4. Dropping it across an origin would mean a
/// redirect could silently deliver over HTTP/1.1 exactly the request that
/// said it could not use HTTP/1.1, which is the failure the mark exists to
/// prevent, arriving through the one door left open.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RequireVersion(pub http::Version);

/// The one comparison, shared by every transport that honours a demand.
///
/// `Ok(())` when there is no demand or `negotiated` satisfies it; a typed
/// [`crate::error::VersionNotAvailable`] under
/// [`ErrorKind::Unsupported`](crate::error::ErrorKind::Unsupported) otherwise.
///
/// A function here rather than a `==` at each call site so that the rule —
/// exact match, absence means no demand — has one definition. Two
/// transports enforce it today and they must not drift.
///
/// **What it does not do is decide *when* to call it.** That is the whole
/// content of the guarantee: `check_version` at the wrong point is a check
/// that reports a violation after the bytes are already gone. Each caller
/// places it where the protocol is first known and no head has been
/// written, and pins that placement with a test that asserts the server
/// saw nothing.
///
/// # Errors
///
/// A typed [`crate::error::VersionNotAvailable`] under
/// [`ErrorKind::Unsupported`](crate::error::ErrorKind::Unsupported) when a
/// [`RequireVersion`] demand is present and `negotiated` does not match it
/// exactly.
pub fn check_version(
    extensions: &http::Extensions,
    negotiated: http::Version,
) -> Result<(), crate::error::Error> {
    match extensions.get::<RequireVersion>() {
        Some(&RequireVersion(required)) if required != negotiated => Err(crate::error::Error::new(
            crate::error::ErrorKind::Unsupported,
            VersionNotAvailable {
                required,
                negotiated,
            },
        )),
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::error::Error as StdError;

    /// **The narrowing keeps every bound it does not touch, and drops
    /// exactly the one it must.**
    ///
    /// [`Timeouts::narrowed_to_connect`] has two callers, both on the QUIC
    /// path (`hclient-native`'s `route` and `race`), and both need a probe
    /// bounded by what is *left* of the caller's connect budget — which is
    /// the arithmetic this workspace states as *a bound a server can
    /// double is not a bound*. Neither caller is reachable without a
    /// network, so the property is pinned here, where the method is a pure
    /// function of its input.
    ///
    /// What a mutation found is that replacing the whole body with
    /// `Default::default()` — every bound `None`, so nothing is bounded at
    /// all — left all 2372 tests green. The three assertions below are the
    /// three halves of that: `connect` becomes the value asked for,
    /// `resolve` is cleared because the probe is past resolution, and the
    /// two body bounds ride through untouched, since narrowing a connect
    /// says nothing about how long a body may take.
    #[test]
    fn narrowing_to_a_connect_bound_keeps_the_body_bounds_and_drops_resolve() {
        let before = Timeouts::new()
            .with_resolve(core::time::Duration::from_millis(10))
            .with_connect(core::time::Duration::from_millis(500))
            .with_first_byte(core::time::Duration::from_millis(20))
            .with_between_bytes(core::time::Duration::from_millis(30));

        let after = before.narrowed_to_connect(core::time::Duration::from_millis(120));

        assert_eq!(
            after.connect,
            Some(core::time::Duration::from_millis(120)),
            "the probe is bounded by what was left, not by the original"
        );
        assert_eq!(
            after.resolve, None,
            "the probe has an address already; a resolve bound would bound a query off its path"
        );
        assert_eq!(
            after.first_byte, before.first_byte,
            "narrowing a connect says nothing about the head"
        );
        assert_eq!(
            after.between_bytes, before.between_bytes,
            "nor about the gaps between frames"
        );
    }

    /// The control for the test above, and the half that makes it a claim
    /// about *narrowing* rather than about four fields.
    ///
    /// A body replaced by `Default::default()` answers `None` everywhere,
    /// which the assertions above would catch. A body that merely *kept*
    /// the caller's own `connect` would not — it passes three of the four
    /// — so this asserts the one thing that separates them: the value must
    /// be the one passed in, even where the caller's own bound was longer.
    #[test]
    fn the_narrowed_bound_is_the_one_passed_in_and_never_the_callers_own() {
        let generous = Timeouts::new().with_connect(core::time::Duration::from_secs(30));
        let narrowed = generous.narrowed_to_connect(core::time::Duration::from_millis(1));
        assert_eq!(narrowed.connect, Some(core::time::Duration::from_millis(1)));
        assert_ne!(
            narrowed.connect, generous.connect,
            "a narrowing that returns the caller's own bound has narrowed nothing"
        );

        // Zero is a real answer rather than an absent one: `route` writes
        // it deliberately for a race winner with nothing left, and the
        // member answers with its own `Timeout(Connect)`. `None` there
        // would mean *unbounded*, which is the opposite.
        let spent = generous.narrowed_to_connect(core::time::Duration::ZERO);
        assert_eq!(spent.connect, Some(core::time::Duration::ZERO));
    }

    /// **Each bound is paired with its own support field, and with the
    /// name a caller reads in the refusal.**
    ///
    /// [`Timeouts::support_checks`]' destructure makes a new bound a
    /// compile error; nothing about it says the *pairing* is right, and a
    /// mis-paired row is a bound checked against another bound's
    /// capability — a setting refused for the wrong reason, or honoured
    /// where it should be refused. So each row is exercised alone:
    /// exactly one bound set, exactly one support field withheld, and the
    /// row that must complain is the row that names it.
    ///
    /// A table rather than four tests, because what it asserts is a
    /// property of the whole set — that no two rows answer for each other
    /// — and a fifth bound should extend a list rather than need a fifth
    /// copy of a test.
    #[test]
    fn every_bound_names_its_own_support_field() {
        use crate::caps::TimeoutSupport;
        use core::time::Duration;

        /// One row: build a `Timeouts` with exactly this bound set,
        /// withhold exactly this support, and expect exactly this name.
        type Row = (
            fn(Duration) -> Timeouts,
            fn(&mut TimeoutSupport),
            &'static str,
        );

        let all = TimeoutSupport::none()
            .with_resolve(true)
            .with_connect(true)
            .with_first_byte(true)
            .with_between_bytes(true);
        let d = Duration::from_secs(1);

        // (set exactly this bound, withhold exactly this support, expect this name)
        let table: [Row; 4] = [
            (
                |d| Timeouts::new().with_resolve(d),
                |s| s.resolve = false,
                "resolve_timeout",
            ),
            (
                |d| Timeouts::new().with_connect(d),
                |s| s.connect = false,
                "connect_timeout",
            ),
            (
                |d| Timeouts::new().with_first_byte(d),
                |s| s.first_byte = false,
                "first_byte_timeout",
            ),
            (
                |d| Timeouts::new().with_between_bytes(d),
                |s| s.between_bytes = false,
                "between_bytes_timeout",
            ),
        ];

        for (build, withhold, name) in table {
            let t = build(d);
            let mut s = all;
            withhold(&mut s);

            let complaints: Vec<&str> = t
                .support_checks(&s)
                .filter(|b| b.requested && !b.supported)
                .map(|b| b.what)
                .collect();
            assert_eq!(
                complaints,
                vec![name],
                "setting `{name}`'s bound while withholding `{name}`'s support \
                 must complain about `{name}` and about nothing else"
            );

            // The control: with every support granted the same bound is fine,
            // so the row above is discriminating on the withheld field rather
            // than complaining about everything.
            let none: Vec<&str> = t
                .support_checks(&all)
                .filter(|b| b.requested && !b.supported)
                .map(|b| b.what)
                .collect();
            assert!(
                none.is_empty(),
                "`{name}` must be accepted when its support is reported"
            );
        }
    }

    /// An unset bound asks nothing of a transport, whatever it reports.
    ///
    /// This is the property that makes a *new* bound safe to add under
    /// `#[non_exhaustive]`: a caller who never heard of it leaves it
    /// unset, and unset must never be a refusal.
    #[test]
    fn an_unset_bound_is_never_refused() {
        use crate::caps::TimeoutSupport;
        let refused: Vec<&str> = Timeouts::default()
            .support_checks(&TimeoutSupport::default())
            .filter(|b| b.requested && !b.supported)
            .map(|b| b.what)
            .collect();
        assert!(refused.is_empty(), "{refused:?}");
    }

    /// The merge takes this value's bound where it has one and the base's
    /// otherwise, per field and with no field answering for another.
    #[test]
    fn the_merge_is_field_by_field() {
        use core::time::Duration;
        let client = Timeouts::new()
            .with_connect(Duration::from_secs(1))
            .with_first_byte(Duration::from_secs(2));
        let request = Timeouts::new().with_connect(Duration::from_secs(9));

        let eff = request.or(&client);
        assert_eq!(eff.connect, Some(Duration::from_secs(9)), "request wins");
        assert_eq!(
            eff.first_byte,
            Some(Duration::from_secs(2)),
            "unset in the request, so the client's stands"
        );
        assert_eq!(eff.resolve, None, "set by neither");
    }

    /// A `const` is where the builder earns its place: `Default::default()`
    /// is not a `const fn`, so a `#[non_exhaustive]` struct written into
    /// one has no functional-update form to fall back on. Consumers do
    /// write them — `hclient-dns-doh`'s `DEFAULT_TIMEOUTS` is one.
    #[test]
    fn the_builder_composes_in_a_const() {
        use core::time::Duration;
        const T: Timeouts = Timeouts::new()
            .with_connect(Duration::from_secs(2))
            .with_first_byte(Duration::from_secs(5));
        assert_eq!(T.connect, Some(Duration::from_secs(2)));
        assert_eq!(T.between_bytes, None);
    }

    #[test]
    fn an_unmarked_request_is_satisfied_by_every_version() {
        let e = http::Extensions::new();
        for v in [
            http::Version::HTTP_09,
            http::Version::HTTP_10,
            http::Version::HTTP_11,
            http::Version::HTTP_2,
            http::Version::HTTP_3,
        ] {
            assert!(check_version(&e, v).is_ok(), "{v:?}");
        }
    }

    #[test]
    fn a_demand_the_connection_meets_passes() {
        let mut e = http::Extensions::new();
        e.insert(RequireVersion(http::Version::HTTP_2));
        assert!(check_version(&e, http::Version::HTTP_2).is_ok());
    }

    /// The refusal carries both halves and is `Unsupported`, not `Other`:
    /// a caller sorting failures by `kind()` must be able to tell "this
    /// connection cannot do what I asked" from a genuine transport
    /// failure without a downcast.
    #[test]
    fn a_demand_the_connection_misses_is_a_typed_unsupported() {
        let mut e = http::Extensions::new();
        e.insert(RequireVersion(http::Version::HTTP_2));
        let err = check_version(&e, http::Version::HTTP_11).unwrap_err();
        assert_eq!(*err.kind(), crate::error::ErrorKind::Unsupported);
        let named = StdError::source(&err)
            .and_then(|s| s.downcast_ref::<VersionNotAvailable>())
            .expect("the source must be the typed refusal, not an opaque string");
        assert_eq!(
            *named,
            VersionNotAvailable {
                required: http::Version::HTTP_2,
                negotiated: http::Version::HTTP_11,
            }
        );
    }

    /// Exact match in **both** directions, and the second one is the
    /// interesting half: a caller demanding HTTP/1.1 — to keep an upgrade
    /// path open — must not be quietly served over HTTP/2 on the grounds
    /// that HTTP/2 is "newer". A `>=` comparison would pass this test's
    /// sibling above and fail here, which is why the pair is written out
    /// rather than parameterised into one loop over "mismatches".
    #[test]
    fn a_newer_version_does_not_satisfy_a_demand_for_an_older_one() {
        let mut e = http::Extensions::new();
        e.insert(RequireVersion(http::Version::HTTP_11));
        let err = check_version(&e, http::Version::HTTP_2).unwrap_err();
        assert_eq!(*err.kind(), crate::error::ErrorKind::Unsupported);
    }

    /// The message names both versions. Not a `Display` assertion for its
    /// own sake: `VersionNotAvailable` reaches a log or a `{e}` far more
    /// often than it reaches a downcast, and a message naming only one of
    /// the two versions leaves the reader unable to tell which end was
    /// wrong.
    #[test]
    fn the_refusal_message_names_both_versions() {
        let msg = VersionNotAvailable {
            required: http::Version::HTTP_2,
            negotiated: http::Version::HTTP_11,
        }
        .to_string();
        assert!(msg.contains("HTTP/2.0"), "{msg}");
        assert!(msg.contains("HTTP/1.1"), "{msg}");
    }
}
