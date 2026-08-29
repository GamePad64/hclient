//! Connect now, send later — on the one kind of backend that has a
//! connector to stage.
//!
//! # It is not a method on `Transport`, and that is the third refusal of
//! the same shape
//!
//! `Transport` is the seam **every** backend fills in. `wasi:http` 0.3's
//! client interface is one function — `send: async func(request) ->
//! result<response, error-code>` — with no connection resource anywhere in
//! the WIT, so `hclient-wasi` could answer nothing at all; and the
//! browser's one connect-shaped API is a `<link rel="preconnect">` hint,
//! which yields no handle, no readiness signal and no way to bind a later
//! `fetch()` to whatever it opened, so `hclient-fetch` would be
//! implementing *"ask the browser nicely, then return `Ok(())`"*. A
//! `Transport::connect` would be `Unsupported` for two of four backends and
//! dishonest rather than merely unimplemented for one of them.
//!
//! The nearer precedent is in this crate: [`crate::Prefetch`] staged the
//! phase one step earlier — name resolution — and refused the seam with the
//! sentence that decides this one too, about a phase that had not come up
//! yet: *"a `fetch`-shaped transport has no DNS of its own to save, and a
//! `wasi:http` one has no connector at all."*
//!
//! A trait rather than two inherent methods for [`crate::Prefetch`]'s
//! mechanical reason: a caller generic over `Native<R, T, D>` reaches it
//! through a `where` bound, and an inherent method would make that caller
//! repeat every structural bound [`crate::Native`]'s exchange impl declares
//! — and then still not be able to name the response body, because
//! `<Native<..> as Transport>::Body` behind a `where` clause is an opaque
//! projection.
//!
//! # A handle, not a warm pool, and `Timeouts::connect` is why
//!
//! The weaker shape a reader will reach for is *"dial and leave it in the
//! pool; the ordinary `execute` will find it"*. It fixes the same
//! duplicate-request problem and is refused for a different one: under it
//! the second call **may still connect**, so it reads `Timeouts::connect`
//! off the same request and applies it again, and a caller who set
//! `connect: Some(C)` can be made to wait `2C`. That is the defect
//! `hclient::Client`'s `425` replay had to be built around — *"a bound a
//! server can double by answering `425` is not a bound"*.
//!
//! Here [`StagedConnect::exchange`] is handed a connection. There is no
//! connect for a bound to bound — not *ignored*, which would need a comment
//! and a test, but **absent**, because the code path is absent.
//! `TimeoutSupport::connect` is untouched: it is a claim about
//! `Transport::execute`, and `execute` is unchanged.
//!
//! # What the pool says about a connection held outside it
//!
//! `Pool`, `PoolKey`, `CheckIn` and `Established` are all `pub(crate)`, so
//! *"a connection produced outside this crate and handed to `execute`"* is
//! not expressible — and [`Staged`] keeps it that way: it is produced by
//! [`StagedConnect::connect`] and consumed by [`StagedConnect::exchange`],
//! and the fact that a caller holds it in between changes nothing about who
//! made it. It carries **its own check-in**, minted from the key the
//! connect computed, rather than having `exchange` recompute one: the
//! protocol is known only at the end of the connect, and two places holding
//! one fact is the class of invariant this crate tries not to have.
//!
//! `pool.rs`'s *"nobody polls an idle connection, and that is the design"*
//! is what makes holding one across a caller's decision cost nothing new:
//! a [`Staged`] is in exactly the state a pooled connection is in, and the
//! residual window that module records — *"a server may close between our
//! check and our write"* — widens by however long the handle is held and is
//! otherwise the same window.
//!
//! # A dropped handle goes back to the pool
//!
//! What happens to a losing racer's connection when it finishes rather
//! than being dropped mid-handshake is [`Staged`]'s `Drop`, and it is the
//! same answer the pool gives every other connection: a handle dropped
//! without an exchange **checks its connection in**, so a connection made
//! for a request that went elsewhere is a warm connection rather than a
//! closed socket. Nothing was spoken on it, so there is nothing to make it
//! unfit; `is_reusable` polls it at the next checkout exactly as it polls
//! every other entry.
//!
//! Under `Native::without_pool()` there is no check-in and the drop closes
//! the socket, which is that setting behaving as it says.
//!
//! # The one thing `exchange` does not do, and it is deliberate
//!
//! `Native::run` retries once when hyper hands the request back **unsent**
//! — a pooled connection the server had closed. `exchange` does not: a
//! retry means either another pooled candidate or a fresh dial, and the
//! fresh dial is precisely the code path the section above requires to be
//! absent. Half a retry — pool-only, never dialling — would be the rule
//! with an exception, and an exception here is worse than the absence.
//!
//! So a [`Staged`] whose connection died in the caller's window costs this
//! request, where `execute` would have opened another. That is the price of
//! the bound being unspendable twice, it is paid only on the staged path,
//! and it is the reason `connect` is worth calling as late as the caller
//! can manage.

use crate::established::{self, Established};
use crate::pool::CheckIn;
use crate::{
    Native, NativeIo, Prepared, body, connect, connection_id, discovery, handshake_for, mark,
    negotiated_protocol, protocol_admissible, since, spoken_version, with_connect_timeout,
};
use hclient_core::unversioned::{
    ConnectTiming, Connected, ConnectionId, Event, Hooks, Reused, Transport,
};
use hclient_core::{Error, RequestBody, Timeouts, check_version};
use hclient_dns::Resolve;
use hclient_rt::{TcpConnect, Timer};
use hclient_tls::TlsConnect;
use std::fmt::Debug;
use std::future::Future;
use std::time::Duration;

/// A transport whose connect can be asked for on its own, and whose answer
/// can then be spent on exactly one request.
///
/// # Who this is for
///
/// A caller that owns more than one protocol stack and has to find out
/// whether one of them can reach an origin *before* it decides which one a
/// request goes to. `hclient-select` is the one in this workspace: it asks
/// the QUIC stack to connect, and where that fails it routes the request —
/// **untouched, unsent, never handed to a transport** — over TCP. Nothing
/// is retried, because nothing was sent, and this crate's own sentence is
/// true of it verbatim: *this is not a second request, it is the first one,
/// which never left.*
///
/// # Two calls, one budget
///
/// [`Self::connect`] reads `Timeouts::connect` off the request and spends
/// it. [`Self::exchange`] cannot spend it again, because it cannot connect
/// — see the module doc. `Timeouts::first_byte` and
/// `Timeouts::between_bytes` are read by `connect` too, off the same
/// request, and carried in the handle: they bound the exchange, and the
/// exchange is not the call that was handed the request.
pub trait StagedConnect: Transport {
    /// A connection this transport made or found, together with the
    /// request it was made for.
    ///
    /// Opaque, and produced only by [`Self::connect`]: the wrong-connection
    /// question is not answered here, it cannot be asked — which is
    /// [`Prepared`]'s own argument for pairing a record with the request it
    /// was fetched for, one phase earlier.
    type Staged;

    /// Everything `Transport::execute` does up to and including *"a
    /// connection that can carry this request"*, and not one byte more.
    ///
    /// On failure the request comes back untouched, in [`Refused`], because
    /// the caller asked this question in order to decide something and the
    /// failure is half the answer.
    ///
    /// Written as `-> impl Future` rather than `async fn` for
    /// `Transport::execute`'s reason: no `Send` bound is added anywhere in
    /// this workspace's seams, and this one is on the same footing.
    fn connect(&self, prepared: Prepared) -> impl Future<Output = Result<Self::Staged, Refused>>;

    /// The rest of `Transport::execute`, on the connection
    /// [`Self::connect`] produced.
    fn exchange(
        &self,
        staged: Self::Staged,
    ) -> impl Future<Output = Result<http::Response<Self::Body>, Self::Error>>;
}

/// A connect that did not produce a connection, with the request back.
///
/// The pair rather than an error alone, and it is the shape
/// `established::Failed::NotSent` already has for the same purpose: a
/// caller that is going to send this request somewhere else needs the
/// request, and a caller that is not can take the error and drop the rest.
#[derive(Debug)]
pub struct Refused {
    error: Error,
    request: http::Request<RequestBody>,
}

impl Refused {
    /// Why, without taking the request.
    pub fn error(&self) -> &Error {
        &self.error
    }

    /// Why, for a caller with nowhere else to send this.
    pub fn into_error(self) -> Error {
        self.error
    }

    /// Both halves, for the caller this type exists for.
    pub fn into_parts(self) -> (Error, http::Request<RequestBody>) {
        (self.error, self.request)
    }
}

impl From<Refused> for Error {
    fn from(r: Refused) -> Error {
        r.error
    }
}

/// A connection with a request to spend it on.
///
/// Named `Staged` rather than `Connected`, for a dull reason: `Connected`
/// is already the name of the hook event this module emits three lines
/// after making one, and two meanings of the word in one file is how the
/// wrong one gets read.
pub struct Staged<R, T, H>
where
    R: TcpConnect + Timer,
    T: TlsConnect,
{
    /// `None` only between [`StagedConnect::exchange`] taking the contents
    /// and the value being dropped, which is what makes [`Drop`] writable
    /// at all: moving out of a field of a type with a destructor is what an
    /// `Option` here buys.
    held: Option<Held<R, T, H>>,
}

struct Held<R, T, H>
where
    R: TcpConnect + Timer,
    T: TlsConnect,
{
    est: Established<NativeIo<R, T>>,
    /// The connection's way home, minted from the key the connect
    /// computed — `None` when reuse is off.
    checkin: Option<CheckIn<NativeIo<R, T>>>,
    req: http::Request<RequestBody>,
    /// The URI as the caller wrote it, kept because `established::exchange`
    /// rewrites the request into its protocol's shape and has to be able to
    /// put it back.
    uri: http::Uri,
    id: ConnectionId,
    /// When this transport committed to doing work, for `Head::elapsed` —
    /// `None` when nobody is watching.
    began: Option<R::Instant>,
    /// The two bounds the exchange still owes. Read in `connect`, off the
    /// request, and carried rather than re-read: `connect` is where a
    /// `Timeouts` is read on this path, and one reader is what keeps the
    /// third field from creeping back in beside them.
    first_byte: Option<Duration>,
    between_bytes: Option<Duration>,
    hooks: H,
}

/// Hand-written for `Native`'s reason: a derive would demand `Debug` from
/// the runtime and the TLS backend for the benefit of a formatter.
impl<R, T, H> Debug for Staged<R, T, H>
where
    R: TcpConnect + Timer,
    T: TlsConnect,
{
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Staged")
            .field("spent", &self.held.is_none())
            .field("uri", &self.held.as_ref().map(|h| &h.uri))
            .finish_non_exhaustive()
    }
}

impl<R, T, H> Drop for Staged<R, T, H>
where
    R: TcpConnect + Timer,
    T: TlsConnect,
{
    /// A handle nobody spent hands its connection back to the pool.
    ///
    /// See the module doc: nothing was spoken on it, so it is exactly the
    /// connection the pool would have held if this request had never been
    /// staged. With reuse off there is no check-in and the drop closes the
    /// socket, which is what `without_pool()` means.
    ///
    /// **A shared connection is dropped rather than checked in**, and the
    /// two are opposites here rather than variations: a
    /// `crate::established::Established::H2Shared` was *borrowed* from
    /// the pool and never left it, so putting it back would leave two
    /// entries naming one connection — and the pool would then be holding
    /// a connection open one entry longer than the origin's traffic
    /// justifies. Dropping the clone is exactly right: the pool's own copy
    /// is what keeps the connection alive.
    fn drop(&mut self) {
        if let Some(held) = self.held.take()
            && held.est.borrowed().is_none()
            && let Some(checkin) = held.checkin
        {
            checkin.put(held.est);
        }
    }
}

/// The one implementation, and the contract is on the trait — for
/// [`Prefetch`](crate::Prefetch)'s reason: an inherent method of the same
/// name wins method resolution over a trait one, so a caller with the trait
/// in scope would silently get the other function.
impl<R, T, D, H, P> StagedConnect for Native<R, T, D, H, P>
where
    R: TcpConnect + Timer + Clone,
    R::Stream: 'static,
    T: TlsConnect,
    T::Stream<R::Stream>: 'static,
    D: Resolve,
    H: Hooks + Clone + Unpin,
    P: crate::proxy::Handshake + Clone,
{
    type Staged = Staged<R, T, H>;

    /// `Native::run`'s steps 1 and 2 — the pool, then a fresh connection —
    /// and then nothing.
    ///
    /// # It is allowed to answer "I already had one"
    ///
    /// `run` looks in the pool before it dials, and a staged connect that
    /// always dialled would cost a connection at every origin the pool was
    /// already serving. [`Native::upgrade`] is the counter-example that
    /// shows this is a choice rather than an omission: it *does* refuse the
    /// pool, because a socket that stops speaking HTTP is not a connection
    /// any later request could use. A staged one is.
    ///
    /// # What the `Prepared` is used for, and what it is not
    ///
    /// The request. The record it carries is **not** handed to the
    /// connector on this path: the caller staging a connect is by
    /// construction one that has already read the record for itself, and
    /// `Prepared` is taken rather than a bare request so that this entry
    /// point composes with [`Prefetch::prepare`](crate::Prefetch::prepare)
    /// instead of competing with it. `Refused` hands the *request* back
    /// rather than the `Prepared`, because a record the connector has
    /// already tried and failed through is not an answer worth passing on.
    ///
    /// # Where this and `run` are kept in step
    ///
    /// Every step that **decides** anything is a private function shared
    /// with `run`: `key_parts` (which is also where an unsupported scheme
    /// and a missing host become typed errors), `protocol_admissible`,
    /// `pooled_candidates`, `checkout`, `may_speak_h2` through the ALPN
    /// narrowing, `negotiated_protocol`, `check_version` and `checkin_for`.
    /// What is written twice is the *order*, and the two orders differ in
    /// exactly one place: `run` retries across candidates and this cannot,
    /// because it returns one connection.
    async fn connect(&self, prepared: Prepared) -> Result<Self::Staged, Refused> {
        match self.stage(prepared.req).await {
            Ok(staged) => Ok(staged),
            Err((error, request)) => Err(Refused { error, request }),
        }
    }

    /// The exchange, on the connection already in hand.
    ///
    /// The same `established::exchange` `run` calls, under the same
    /// `first_byte` bound, reporting the same `Head` event and wrapped in
    /// the same `between_bytes` body — what differs is upstream of here and
    /// not in here.
    async fn exchange(
        &self,
        mut staged: Self::Staged,
    ) -> Result<http::Response<Self::Body>, Error> {
        let Held {
            est,
            checkin,
            req,
            uri,
            id,
            began,
            first_byte,
            between_bytes,
            hooks,
        } = staged
            .held
            .take()
            .expect("a Staged is emptied only by this method, which consumes it");
        let (parts, body) = req.into_parts();
        let outgoing = body::OutgoingBody::from_request_body(body)?;
        let sent = hclient_core::unversioned::meter::<H>(outgoing.expected())
            .map(std::sync::Arc::new)
            .inspect(|_| ());
        let outgoing = outgoing.counting(sent.clone());
        let req = http::Request::from_parts(parts, outgoing);
        let via = self.via(&uri);
        let gate = self.continue_gate(&req);
        let attempt = established::exchange(
            est,
            req,
            checkin,
            &uri,
            hooks,
            established::Dispatch {
                via,
                watch_1xx: self.watch_1xx,
                gate: gate.clone(),
            },
        );
        let attempt = std::pin::pin!(self.within_first_byte_gated(first_byte, gate, attempt));
        let resp =
            hclient_core::unversioned::Reporting::new(attempt, &self.hooks, id, &uri, sent.clone())
                .await
                .map_err(established::Failed::into_error)?;
        self.report_head(&resp, id, &uri, began);
        Ok(self.bound_body(resp, between_bytes, crate::Counted::new(id, &uri, sent)))
    }
}

impl<R, T, D, H, P> Native<R, T, D, H, P>
where
    R: TcpConnect + Timer + Clone,
    R::Stream: 'static,
    T: TlsConnect,
    T::Stream<R::Stream>: 'static,
    D: Resolve,
    H: Hooks + Clone + Unpin,
    P: crate::proxy::Handshake + Clone,
{
    /// The body of [`StagedConnect::connect`], written where the `Refused`
    /// packing is not, so that each failure arm is a pair rather than a
    /// four-line struct literal.
    // Measured rather than boxed: the pair is 288 bytes, of which **264 are
    // `http::Request<RequestBody>`** — a foreign type — and 24 are `Error`.
    // Handing the request back is the contract rather than an accident:
    // `Refused` exists so a caller can retry a request that never left.
    //
    // Boxing here would silence the lint and shrink nothing a caller sees.
    // The public form is `connect`'s `Result<Self::Staged, Refused>`, which
    // carries the same 288 bytes and which clippy does not flag because it
    // is a trait implementation. Shrinking that is a change to a seam, for
    // an allocation on every refusal against a `Result` returned once per
    // connection rather than once per request — so it is a decision for
    // whoever needs it, not a lint fix.
    #[allow(clippy::result_large_err)]
    async fn stage(
        &self,
        req: http::Request<RequestBody>,
    ) -> Result<Staged<R, T, H>, (Error, http::Request<RequestBody>)> {
        // Read field by field from a copy, never branched on as a whole —
        // `Transport::execute`'s doc states the reading literally, and
        // "presence is not intent" is what it means.
        let timeouts = req
            .extensions()
            .get::<Timeouts>()
            .copied()
            .unwrap_or_default();
        // Read and resolved here for the reason `Native::run` does the
        // same one file over: a name this backend has not got is a
        // refusal, never a connection with the default identity.
        let named = req.extensions().get::<hclient_core::ClientIdentity>();
        let identity_id = match named {
            None => None,
            Some(id) => match hclient_tls::TlsIdentity::config_id_for(&self.tls, id.name()) {
                Some(cfg) => Some(cfg),
                None => {
                    let e = Error::new(
                        hclient_core::ErrorKind::Tls,
                        crate::UnknownClientIdentity(id.name().to_owned()),
                    );
                    return Err((e, req));
                }
            },
        };
        let identity = named.map(hclient_core::ClientIdentity::name);

        let parts_of_key = match self.key_parts(req.uri(), identity_id) {
            Ok(p) => p,
            Err(e) => return Err((e, req)),
        };
        let uri = req.uri().clone();
        // The same reading of the runtime's clock both ends of the pool's
        // bookkeeping use in `run`, for the same reason: which entries are
        // too old to hand out and when this one becomes too old are one
        // measurement, not two.
        let now = self.rt.elapsed_since(self.epoch);
        let began = mark::<H, R>(&self.rt);

        // 1. Somebody else's connection, if one is still alive. Identical
        //    to `run`'s first step down to the `Reused` event, and it is
        //    the same event: a caller counting reuse must not be able to
        //    tell a staged connect from an ordinary one.
        for &protocol in self.pooled_candidates(&parts_of_key) {
            if !protocol_admissible(req.extensions(), Some(protocol)) {
                continue;
            }
            let key = parts_of_key.key(protocol);
            let Some(est) = self.checkout(&key, now).await else {
                continue;
            };
            let id = est.id();
            self.hooks.on(&Event::Reused(Reused::new(
                id,
                &uri,
                spoken_version(Some(protocol)),
            )));
            let checkin = self.checkin_for(&key, now);
            return Ok(self.hold(est, checkin, req, uri, id, began, timeouts));
        }

        // 2. A fresh one, under `Timeouts::connect` and under nothing else.
        //    This is the only place in the staged pair that connects, which
        //    is the whole of the module doc's claim about the bound.
        let offered_h2 = self.may_speak_h2(&parts_of_key)
            && check_version(req.extensions(), http::Version::HTTP_2).is_ok();
        let alpn: &[&[u8]] = if offered_h2 {
            &[b"h2", b"http/1.1"]
        } else {
            &[b"http/1.1"]
        };
        let connect_fut = connect::connect::<R, D, T, P, H>(
            &self.rt,
            &self.dns,
            &self.tls,
            &self.proxies,
            self.unix_socket.as_deref(),
            &uri,
            &self.opts,
            alpn,
            identity,
            &self.svcb_failures,
            now,
            discovery::Prefetched::NotConsulted,
            timeouts.resolve,
        );
        let (conn, tls_info, attempted) =
            match with_connect_timeout(&self.rt, timeouts.connect, connect_fut).await {
                Ok(v) => v,
                Err(e) => return Err((e, req)),
            };
        let connect_took = since::<R>(&self.rt, began);
        let protocol = negotiated_protocol(
            tls_info.as_ref().and_then(|i| i.alpn.as_deref()),
            offered_h2,
        );
        let id = connection_id::<H>();
        if let Some(attempted) = attempted {
            self.hooks.on(&Event::Connected(
                Connected::new(id, &uri, spoken_version(protocol))
                    .remote(attempted.remote)
                    // The handshake's own report, which this transport has
                    // had in hand at this line since TLS became a seam and
                    // has been discarding: `TlsInfo` carries the version,
                    // the suite and the ALPN, and nothing above
                    // `hclient-native` could see any of it.
                    .tls(
                        tls_info
                            .as_ref()
                            .and_then(|i| i.protocol_version.as_deref()),
                        tls_info.as_ref().and_then(|i| i.cipher_suite.as_deref()),
                        tls_info.as_ref().and_then(|i| i.alpn.as_deref()),
                        // Cloned rather than borrowed: it is not a slice of
                        // the `TlsInfo`, and the clone happens only in the
                        // `Asked` arm. Over `http://` there is no `TlsInfo`
                        // at all, which is `Unobserved` — there was no
                        // handshake to watch.
                        tls_info
                            .as_ref()
                            .map(|i| i.client_cert.clone())
                            .unwrap_or_default(),
                    )
                    .timing(
                        ConnectTiming::new()
                            .dns(attempted.dns)
                            .tcp(attempted.tcp)
                            .tls(attempted.tls)
                            .total(connect_took),
                    ),
            ));
        }
        // Before the handshake, exactly as in `run`: a demand this
        // connection cannot meet costs a TCP connection and a TLS handshake
        // and not one byte of HTTP.
        if let Err(e) = check_version(req.extensions(), spoken_version(protocol)) {
            return Err((e, req));
        }
        let checkin = match protocol {
            Some(p) => self.checkin_for(&parts_of_key.key(p), now),
            None => None,
        };
        let est = match handshake_for(
            conn,
            protocol,
            id,
            self.h1_opts,
            #[cfg(feature = "http2")]
            self.h2_opts,
        )
        .await
        {
            Ok(e) => e,
            Err(e) => return Err((e, req)),
        };
        Ok(self.hold(est, checkin, req, uri, id, began, timeouts))
    }

    /// The handle, packed from the two places that produce one.
    ///
    /// A method rather than two literals so that a connection found in the
    /// pool and one just made carry the same fields — a difference between
    /// them would be a difference `exchange` could act on, and there is
    /// nothing about the exchange that should know which it got.
    #[expect(
        clippy::too_many_arguments,
        reason = "one struct literal, written once instead of twice"
    )]
    fn hold(
        &self,
        est: Established<NativeIo<R, T>>,
        checkin: Option<CheckIn<NativeIo<R, T>>>,
        req: http::Request<RequestBody>,
        uri: http::Uri,
        id: ConnectionId,
        began: Option<R::Instant>,
        timeouts: Timeouts,
    ) -> Staged<R, T, H> {
        Staged {
            held: Some(Held {
                est,
                checkin,
                req,
                uri,
                id,
                began,
                first_byte: timeouts.first_byte,
                between_bytes: timeouts.between_bytes,
                hooks: self.hooks.clone(),
            }),
        }
    }
}
