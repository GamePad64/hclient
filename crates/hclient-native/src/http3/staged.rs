//! Connect now, send later — and what that turns out to mean on a stack
//! that has already claimed everything it makes.
//!
//! The shape comes from `hclient-native`'s side of the same seam. What
//! this crate's half needs is the weakest form of it — did this origin's
//! QUIC connect succeed —
//! for which a handle may be more than is required."*
//!
//! # It needs a handle, and the reason is the bound rather than the
//! connection
//!
//! The weakest form is a success/failure signal: `connect` returns
//! `Result<(), Error>`, the connection stays in this transport's pool, and
//! the caller then makes an ordinary `Transport::execute` that finds it
//! there. That is refused here for one measured reason, and it is not about
//! ownership. `H3::execute` **resolves the origin's address before it looks
//! in the pool** — a real lookup, through the caller's `Resolve` — and it
//! runs that lookup, and any dial the pool cannot save it from, inside
//! `Timeouts::connect`. So a second call reads the same `connect` bound off
//! the same request and can spend it again, and a caller who set
//! `connect: Some(C)` can be made to wait `2C`. *"A bound a server can
//! double is not a bound"*, one crate over.
//!
//! [`StagedConnect::exchange`] here is handed a resolved address, a
//! checked-out connection and an h3 `SendRequest`. It resolves nothing,
//! looks in no pool and dials nothing, so the bound is not ignored by it —
//! it is absent from it.
//!
//! # What the handle is, and it is **not** an unclaimed connection
//!
//! This is the honest difference from `hclient_native::Staged`, and it is
//! the same fact `hclient-webtransport` meets from the other side.
//! `H3::connect` builds an h3 client on every connection it
//! makes and spawns that client's driver **before** it has anything to hand
//! back, and `checkout` inserts the result into the pool before its caller
//! sees it. There is no state in which this transport holds a connection
//! nobody has claimed — which is why a connect-only entry point could never
//! have served WebTransport, and why a second h3 client on the same QUIC
//! connection is `H3_STREAM_CREATION_ERROR` rather than a second opinion.
//!
//! So [`Staged`] is a **claim on a connection the pool also holds**, where
//! `hclient_native::Staged` *owns* the connection it took out. Both satisfy
//! the property the seam exists for, because that property is about the
//! second call's code path and not about who owns the socket.
//!
//! Two consequences follow, and both are the answer to a question the
//! document left open:
//!
//! - **A dropped handle needs no `Drop`.** `hclient-native` had to give
//!   its handle one, so that a connection made for a request that went
//!   elsewhere becomes a warm pooled connection rather than a closed
//!   socket. Here the pool already has it; dropping a [`Staged`] drops a
//!   `SendRequest` clone and nothing else happens.
//! - **The connection keeps pinging.** `DEFAULT_KEEP_ALIVE` is 5 s, and a
//!   pooled QUIC connection either gets pinged or dies. A caller that
//!   staged a connect and then declined it has left a live connection at an
//!   origin it did not use, for as long as it keeps the transport. That is
//!   the same trade every pooled HTTP/3 connection makes and is written
//!   down at [`crate::http3::DEFAULT_KEEP_ALIVE`]; what is new is only that the
//!   caller may not have wanted this one. It matters to a **race**, which
//!   this workspace does not build; the one consumer here connects on the
//!   arm it intends to use.
//!
//! # There is no `Prepared` on this side, and that is not an omission
//!
//! `hclient-h3` makes no HTTPS-record lookup at all, so there is nothing
//! for a `Prefetch` to save and nothing for [`StagedConnect::connect`] to
//! take but the request itself.

use crate::http3::{CheckedOut, H3, H3Runtime, PoolKey, SendRequest, ZeroRtt, hooks::ConnState};
use hclient_core::unversioned::{ConnectTiming, Connected, Event, Hooks, Reused, Transport};
use hclient_core::{Error, ErrorKind, RequestBody, Timeouts, check_version};
use hclient_tls::quic::QuicTlsConnect;
use std::fmt;
use std::future::Future;
use std::sync::Arc;

/// A transport whose connect can be asked for on its own, and whose answer
/// can then be spent on exactly one request.
///
/// The same shape `hclient_native::StagedConnect` has, declared separately
/// rather than shared: a trait is declared by the crate that implements
/// it, `hclient-select` owns both members concretely and needs no
/// polymorphism between them, and
/// the two do not agree on what `connect` takes — `Native` takes a
/// `hclient_native::Prepared`, because it has a record lookup worth
/// composing with, and this takes a request, because it has none.
///
/// It is emphatically **not** a method on `Transport`. `wasi:http` 0.3's
/// client interface is one function with no connection resource in the WIT,
/// and `hclient-fetch` declares `timeouts.connect = false` because
/// `AbortSignal` is one deadline for the whole exchange; a
/// `Transport::connect` would be `Unsupported` for two of four backends.
pub trait StagedConnect: Transport {
    /// A connection this transport made or found, together with the
    /// request it was made for.
    ///
    /// Opaque, and produced only by [`Self::connect`], so the
    /// wrong-connection question cannot be asked rather than being answered
    /// by a check.
    type Staged;

    /// Everything `Transport::execute` does up to and including *"a
    /// connection that can carry this request"* — the version demand, the
    /// scheme, the early-data admission, address resolution, the QUIC
    /// handshake and h3's own SETTINGS — and not one byte more.
    ///
    /// On failure the request comes back untouched, in [`Refused`], because
    /// the caller asked this in order to decide something and the failure is
    /// half the answer. Nothing was sent, so a caller sending it elsewhere
    /// is not retrying: *this is not a second request, it is the first one,
    /// which never left.*
    /// Associated types rather than RPITITs, so that `http3::arm`'s
    /// erasure can *name* these when it declares its own boxes `Send` —
    /// the same reason `hclient_rt::TcpConnect::Connecting` is one.
    type Connecting<'a>: Future<Output = Result<Self::Staged, Refused>>
    where
        Self: 'a;
    /// [`Connecting`](Self::Connecting)'s counterpart for the second half.
    type Exchanging<'a>: Future<Output = Result<http::Response<Self::Body>, Self::Error>>
    where
        Self: 'a;

    fn connect(&self, req: http::Request<RequestBody>) -> Self::Connecting<'_>;

    /// The rest of `Transport::execute`, on the connection
    /// [`Self::connect`] produced — including the 0-RTT replay, which is
    /// this transport's business and not the caller's.
    fn exchange(&self, staged: Self::Staged) -> Self::Exchanging<'_>;
}

/// A connect that did not produce a connection, with the request back.
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

/// A claim on a connection, with a request to spend it on.
///
/// See the module doc for why "claim" rather than "connection": the pool
/// holds this connection too, and its driver is already spawned.
pub struct Staged<R, H>
where
    R: hclient_rt::Timer,
{
    pub(crate) send: SendRequest,
    pub(crate) zero_rtt: Option<ZeroRtt>,
    pub(crate) conn: quinn::Connection,
    pub(crate) state: Option<Arc<ConnState>>,
    pub(crate) req: http::Request<RequestBody>,
    pub(crate) uri: http::Uri,
    /// When this transport committed to doing work, for `Head::elapsed` —
    /// `None` when nobody is watching.
    pub(crate) began: Option<R::Instant>,
    pub(crate) hooks: std::marker::PhantomData<H>,
}

/// Hand-written for [`H3`]'s reason: a derive would demand `Debug` from the
/// runtime for the benefit of a formatter.
impl<R, H> fmt::Debug for Staged<R, H>
where
    R: hclient_rt::Timer,
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Staged")
            .field("uri", &self.uri)
            .field("early_data", &self.zero_rtt.is_some())
            .finish_non_exhaustive()
    }
}

/// The one implementation, and the contract is on the trait — for
/// `Prefetch`'s reason one crate over: an inherent method of the same name
/// wins method resolution over a trait one.
/// Short and named, so the marker sits where `cargo fmt` leaves it — the
/// rule amendment C12 records about where a bound is written.
type SendStaging<'a, S> = std::pin::Pin<Box<dyn Future<Output = Result<S, Refused>> + Send + 'a>>; // send-bound-exception: amendment-C15

type SendExchange<'a, B> =
    std::pin::Pin<Box<dyn Future<Output = Result<http::Response<B>, Error>> + Send + 'a>>; // send-bound-exception: amendment-C15

impl<R, T, D, H> StagedConnect for H3<R, T, D, H>
where
    R: H3Runtime,
    R::Sleep: Send + 'static, // send-bound-exception: amendment-C10
    R::Socket: fmt::Debug + Send + Sync + 'static, // send-bound-exception: amendment-C10
    T: QuicTlsConnect,
    D: hclient_dns::Resolve,
    // Nameable, which is the point: `H3`'s own handshake is `Send` when
    // its resolver's answers are, and the associated types let that be
    // said. A resolver that cannot — `hclient-dns-doh` — leaves this
    // `H3` without a `StagedConnect`, which is narrower than the arm
    // being `!Send` for everybody.
    Self: Sync,                // send-bound-exception: amendment-C15
    Staged<R, H>: Send,        // send-bound-exception: amendment-C15
    H: Send + Sync,            // send-bound-exception: amendment-C15
    R::Instant: Send + Sync,   // send-bound-exception: amendment-C15
    for<'a> D::Ipv4<'a>: Send, // send-bound-exception: amendment-C15
    for<'a> D::Ipv6<'a>: Send, // send-bound-exception: amendment-C15
    for<'a> D::Svcb<'a>: Send, // send-bound-exception: amendment-C15
    H: Hooks + Clone,
{
    type Staged = Staged<R, H>;

    type Connecting<'a>
        = SendStaging<'a, Self::Staged>
    where
        Self: 'a;

    type Exchanging<'a>
        = SendExchange<'a, Self::Body>
    where
        Self: 'a;

    fn connect(&self, req: http::Request<RequestBody>) -> Self::Connecting<'_> {
        Box::pin(async move {
            match self.stage(req).await {
                Ok(staged) => Ok(staged),
                Err((error, request)) => Err(Refused { error, request }),
            }
        })
    }

    fn exchange(&self, staged: Self::Staged) -> Self::Exchanging<'_> {
        Box::pin(async move { self.finish(staged).await })
    }
}

/// What the checks before the connect establish, so that the connect itself
/// has no `?` that could take the request with it.
pub(crate) struct Admitted {
    pub(crate) host: String,
    pub(crate) port: u16,
    pub(crate) wants_early: bool,
    pub(crate) timeouts: Timeouts,
    /// The client identity, read from the request and already resolved:
    /// the name for `QuicTlsRequest`, and the id for the pool key.
    pub(crate) identity: Option<hclient_core::ClientIdentity>,
    pub(crate) identity_id: Option<hclient_tls::TlsConfigId>,
}

impl<R, T, D, H> H3<R, T, D, H>
where
    R: H3Runtime,
    R::Sleep: Send + 'static, // send-bound-exception: amendment-C10
    R::Socket: fmt::Debug + Send + Sync + 'static, // send-bound-exception: amendment-C10
    T: QuicTlsConnect,
    D: hclient_dns::Resolve,
    H: Hooks + Clone,
{
    /// Every question that can be answered from the request alone, in the
    /// order `execute` has always asked them.
    ///
    /// Borrows the request rather than taking it, which is the whole reason
    /// this is a function: a staged connect has to be able to hand the
    /// request back, and a `?` on a value it had already moved could not.
    pub(crate) fn admit(&self, req: &http::Request<RequestBody>) -> Result<Admitted, Error> {
        // **A `RequireVersion` demand, answered before anything else** —
        // before the scheme check, before resolution, before a QUIC packet.
        // This transport speaks exactly one version, so the answer is a
        // pure function of the request.
        check_version(req.extensions(), http::Version::HTTP_3)?;
        let uri = req.uri();
        if uri.scheme_str() != Some("https") {
            return Err(Error::new(
                ErrorKind::Connect,
                std::io::Error::other(format!(
                    "HTTP/3 runs over QUIC, which is always TLS; `{}` has no plaintext form",
                    uri.scheme_str().unwrap_or("(no scheme)")
                )),
            ));
        }
        let host = uri
            .host()
            .ok_or_else(|| {
                Error::new(
                    ErrorKind::Connect,
                    std::io::Error::other("request URI has no host"),
                )
            })?
            .to_string();
        let port = uri.port_u16().unwrap_or(443);

        let wants_early = crate::http3::early::admits_early_data(req);
        // **Read and resolved before the key is built.** A name this
        // backend has not got is a refusal, never a dial with the default
        // identity — see `hclient-native`'s TCP path, which does the same
        // one screen over, and `docs/mtls-design.md` for why both paths
        // must.
        let identity = req
            .extensions()
            .get::<hclient_core::ClientIdentity>()
            .cloned();
        let identity_id = match identity.as_ref().map(hclient_core::ClientIdentity::name) {
            None => None,
            Some(name) => match hclient_tls::TlsIdentity::config_id_for(&self.tls, name) {
                Some(cfg) => Some(cfg),
                None => {
                    return Err(crate::Error::new(
                        hclient_core::ErrorKind::Tls,
                        crate::UnknownClientIdentity(name.to_owned()),
                    ));
                }
            },
        };
        if req
            .extensions()
            .get::<hclient_core::AllowEarlyData>()
            .is_some()
            && self.caps.early_data == hclient_core::EarlyDataSupport::None
        {
            return Err(crate::http3::early::refuse_early_data("hclient-h3"));
        }

        // `Timeouts` is read field by field, from a copy, and never
        // branched on as a whole — `Transport::execute`'s doc comment
        // states the reading literally, and "presence is not intent" is
        // what it means. Only `connect` is honoured here; the other two are
        // declared `false`.
        Ok(Admitted {
            host,
            port,
            wants_early,
            identity,
            identity_id,
            timeouts: req
                .extensions()
                .get::<Timeouts>()
                .copied()
                .unwrap_or_default(),
        })
    }

    /// From a request to a connection that can carry it, and no further.
    ///
    /// This is `execute`'s first half, and `execute` is now written as
    /// `stage` then [`Self::finish`] — one sequencing with two entry points,
    /// which is what stops the staged path and the ordinary one from
    /// drifting into two different transports.
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
    pub(crate) async fn stage(
        &self,
        req: http::Request<RequestBody>,
    ) -> Result<Staged<R, H>, (Error, http::Request<RequestBody>)> {
        let Admitted {
            host,
            port,
            wants_early,
            timeouts,
            identity,
            identity_id,
        } = match self.admit(&req) {
            Ok(a) => a,
            Err(e) => return Err((e, req)),
        };
        let uri = req.uri().clone();

        // When this transport committed to doing work, for `Head::elapsed`
        // and for `ConnectTiming`'s `dns` and `total` — and `None` when
        // nobody is watching. The three figures share one mark
        // deliberately: the pair (`Head::elapsed`, `ConnectTiming::total`)
        // is what answers "was it the connection or was it the server".
        let began = crate::http3::mark::<H, R>(&self.rt);

        // Everything between "a URI" and "a connection that can carry a
        // request" — address resolution, the QUIC handshake, and h3's own
        // settings exchange on top of it — under one bound.
        //
        // The same scope `hclient-native` gives `connect`, deliberately: a
        // portable setting that meant "DNS included" on one transport and
        // "handshake only" on another would be a capability that lies in
        // the most tiresome way — by being true.
        //
        // A pooled checkout does no I/O at all, so it is inside the bound
        // for want of a reason to write a second path rather than because
        // it needs one.
        let connect = async {
            let addr = self.resolve(&host, port).await?;
            let dns = crate::http3::since::<R>(&self.rt, began);
            // The identity is resolved before the key is built, so a
            // name this backend has not got refuses here rather than
            // dialling with the default one.
            let key = PoolKey {
                host,
                port,
                tls: identity_id.unwrap_or_else(|| self.tls.config_id()),
                early_data: wants_early,
                identity: identity.clone(),
            };
            self.checkout(&key, addr, dns).await
        };
        let checked = match timeouts.connect {
            Some(d) => crate::http3::within_connect(&self.rt, d, connect).await,
            None => connect.await,
        };
        let CheckedOut {
            send,
            zero_rtt,
            conn,
            state,
            made,
        } = match checked {
            Ok(c) => c,
            Err(e) => return Err((e, req)),
        };

        // Emitted here, outside `checkout`, because the pool's mutex is
        // held inside it and no hook is ever called under a lock. The
        // *branch* still comes from the pool: `made` is `Some` only on the
        // path that dialled, so there is no flag anywhere that could
        // disagree with the behaviour.
        //
        // And emitted by the connect rather than by the exchange, which is
        // the honest instant on both entry points: the connection exists
        // and nothing has been spoken on it. A caller that stages a connect
        // and never spends it has still made a connection, and a `Connected`
        // it never heard about would be a connection nothing could account
        // for.
        let id = ConnState::id(state.as_ref());
        match &made {
            Some(m) => self.hooks.on(Event::Connected(
                Connected::new(id, &uri, http::Version::HTTP_3)
                    .remote(Some(m.remote))
                    .timing(
                        ConnectTiming::new()
                            .dns(m.dns)
                            .tcp(m.handshake)
                            .tls(None)
                            .total(crate::http3::since::<R>(&self.rt, began)),
                    ),
            )),
            None => self
                .hooks
                .on(Event::Reused(Reused::new(id, &uri, http::Version::HTTP_3))),
        }

        Ok(Staged {
            send,
            zero_rtt,
            conn,
            state,
            req,
            uri,
            began,
            hooks: std::marker::PhantomData,
        })
    }

    /// `execute`'s second half: one attempt, the 0-RTT verdict, and — where
    /// the verdict says the early keys were refused — the replay.
    pub(crate) async fn finish(
        &self,
        staged: Staged<R, H>,
    ) -> Result<http::Response<crate::http3::H3Body<H>>, Error> {
        let Staged {
            mut send,
            zero_rtt,
            conn,
            state,
            req,
            uri,
            began,
            hooks: _,
        } = staged;
        let id = ConnState::id(state.as_ref());
        // Built once and cloned per attempt: the 0-RTT replay is a second
        // stream on the same connection, and both attempts' bodies report
        // through the same `ConnState`, which is what keeps one
        // connection's end to one event.
        let watch = self.watch(&conn, &state);

        let (parts, body) = req.into_parts();
        // Taken before the first attempt, because after it the body is gone
        // — but **only when there is something a replay could be needed
        // for**. `zero_rtt.is_some()` is exactly the condition: it is
        // `Some` only when this connection really went out with early data.
        let spare = if zero_rtt.is_some() {
            body.rewind()
        } else {
            None
        };
        let head = http::Request::from_parts(parts.clone(), ());
        let first = Self::one_attempt(&mut send, head, body, watch.clone()).await;

        // The second of the three 0-RTT failure paths (`crate::http3::early` has
        // the table). The rejection is detected by AWAITING THE VERDICT,
        // not by matching on an error string: h3 surfaces the QUIC error as
        // an opaque `Undefined(..)` whose `Display` is not a stable
        // interface, and the verdict future is the authority on the
        // question anyway.
        let e = match first {
            Ok(resp) => {
                self.report_head(&resp, id, &uri, began);
                return Ok(resp);
            }
            Err(e) => e,
        };
        let Some(verdict) = zero_rtt else {
            // Nothing went into early data, so this error is the caller's.
            self.report_failed(&watch, &e);
            return Err(e);
        };
        if verdict.await {
            // Early data was accepted; the failure is a real one.
            self.report_failed(&watch, &e);
            return Err(e);
        }
        let Some(body) = spare else {
            // Unreachable through `execute`: `admits_early_data` refuses a
            // `RetryKind::Impossible` body, which is the only kind `rewind`
            // returns `None` for. Kept as a typed error rather than an
            // `unwrap`, because the two checks live in different files and
            // the invariant between them is not one the compiler holds.
            self.report_failed(&watch, &e);
            return Err(e);
        };
        let head = http::Request::from_parts(parts, ());
        match Self::one_attempt(&mut send, head, body, watch.clone()).await {
            Ok(resp) => {
                self.report_head(&resp, id, &uri, began);
                Ok(resp)
            }
            Err(e) => {
                self.report_failed(&watch, &e);
                Err(e)
            }
        }
    }
}
