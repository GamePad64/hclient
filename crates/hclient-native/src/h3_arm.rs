//! The QUIC arm, with its type erased — and the erasure is what keeps
//! HTTP/3 from taxing every build of this transport.
//!
//! # Why erased and not `Option<H3<R, T, D>>`
//!
//! `H3`'s own declaration carries no where-clause, so naming the type
//! costs nothing. The cost is in `impl Transport for Native`: routing to a
//! concrete `H3<R, T, D>` means calling into it, which pulls
//! `H3<R, T, D>: StagedConnect` into that impl's where-clause — and from
//! there `R: UdpBind + Spawn<QuinnTask> + Send + Sync + 'static` and
//! `T: QuicTlsConnect`, **unconditionally**, because Cargo's features are
//! additive. `Native<Embassy, NoTls, IpLiteralOnly>` would stop compiling
//! for a caller who never asked for HTTP/3 and cannot opt out.
//!
//! Behind `dyn`, `execute` calls [`BoxedStagedConnect::connect_boxed`] and
//! demands nothing of `R` or `T`. Every bound lives on
//! [`crate::Native::http3`], which such a caller never writes.
//!
//! # Two traits, because the handle outlives the call that made it
//!
//! `StagedConnect::exchange` takes `&self` **and** the handle, so an
//! erased handle has to carry the transport it came from. That is
//! [`StagedOver`], and it is why [`BoxedStaged`] exists rather than the
//! handle being a `Box<dyn Any>` the connector downcasts: a downcast can
//! be given the wrong handle and panic, where a borrow cannot be given
//! anything at all.
//!
//! The lifetime is the visible price, and it is on the **trait** rather
//! than only on the trait object: `exchange_boxed` returns a future that
//! borrows the transport through the handle, and a method cannot name a
//! lifetime its trait does not have. `BoxExchange<'static>` there is a
//! borrow-check error, which is the check that the borrow is real — so a
//! handle cannot outlive the `&self` that produced it, which is exactly
//! the contract `StagedConnect` already has and the reason this is safe
//! to erase at all.

use crate::h3::{Refused, StagedConnect};
use bytes::Bytes;
use hclient_core::Error as CoreError;
use hclient_core::RequestBody;
use std::future::Future;
use std::pin::Pin;

/// The arm's response body, boxed **and `Send`**.
///
/// `hclient_core`'s own `BoxBody` declares no auto trait, because one
/// erased body there serves every backend and the browser's holds a `dyn
/// Stream` with none. Here the set of backends is one — `hclient-h3`, over
/// quinn, which requires `Send` of everything on that path anyway — and
/// the property at stake is this crate's own: `NativeBody: Send` is
/// asserted in `tests/shape.rs`, and a body variant without it would take
/// that away from every build in a graph where a neighbour switched
/// `http3` on. Which is the harm the erasure exists to prevent, arriving
/// through the body instead of the bounds.
///
/// Caught by that assertion rather than by reading.
pub(crate) type SendBoxBody = Pin<Box<dyn http_body::Body<Data = Bytes, Error = CoreError> + Send>>; // send-bound-exception: amendment-C12

/// What [`BoxedStaged::exchange_boxed`] hands back.
type SendExchange<'a> =
    Pin<Box<dyn Future<Output = Result<http::Response<SendBoxBody>, CoreError>> + 'a>>;

/// The QUIC arm as a field type: erased, shareable, and named so the
/// bound sits on one short line.
///
/// **`Send + Sync` here is load-bearing.** Without it a `Native` holding
/// the field would be `!Send`, and Cargo's features being additive that
/// would happen to every build in a graph where anyone switched `http3`
/// on — a transport that stops crossing a thread because of a neighbour's
/// manifest. The concrete `H3` satisfies both anyway: quinn requires them
/// of the runtime it is given.
pub(crate) type Arm = dyn BoxedStagedConnect + Send + Sync; // send-bound-exception: amendment-C12

/// What [`BoxedStagedConnect::connect_boxed`] hands back, named because
/// the type outgrew its line — and `clippy::type_complexity` said so
/// before a reader had to.
///
/// The `'a` is the transport's: the handle borrows it, which is what makes
/// erasing the pair safe at all.
type Staging<'a> =
    Pin<Box<dyn Future<Output = Result<Box<dyn BoxedStaged<'a> + 'a>, Refused>> + 'a>>;

/// A staged connect whose transport type is gone.
///
/// `Debug` is a supertrait because `Native` derives it and a field this
/// one sits in cannot opt out — the alternative is a hand-written `Debug`
/// for a struct with a dozen fields, which drifts. `H3` derives it.
///
/// Blanket-implemented over every [`StagedConnect`], so `hclient-h3`
/// implements nothing for it — the same arrangement
/// `hclient_core::unversioned::erased::BoxedTransport` has, and for the
/// same reason: a seam a backend has to opt into is a seam backends forget.
pub(crate) trait BoxedStagedConnect: std::fmt::Debug {
    /// [`StagedConnect::connect`], boxed.
    ///
    /// `Refused` carries the request back untouched, which is the whole
    /// reason the staged pair exists rather than `execute`: a QUIC connect
    /// that fails must leave the request available to be sent over TCP,
    /// and a request already handed to a stream is not.
    fn connect_boxed<'a>(&'a self, req: http::Request<RequestBody>) -> Staging<'a>;
}

/// A connection staged by a [`BoxedStagedConnect`], with one thing left to
/// do to it.
pub(crate) trait BoxedStaged<'a> {
    /// [`StagedConnect::exchange`], boxed. Takes `Box<Self>` because a
    /// staged connection is spent exactly once.
    ///
    /// The `'a` is the transport's, carried on the trait rather than left
    /// to the trait object's own `+ 'a`: the returned future borrows the
    /// transport through the handle, and a method signature cannot name a
    /// lifetime the trait does not have. Writing `BoxExchange<'static>`
    /// here does not compile, which is the check that the borrow is real.
    fn exchange_boxed(self: Box<Self>) -> SendExchange<'a>;
}

/// The transport and the handle together, which is what makes the handle
/// erasable: `exchange` needs both and the caller holds neither.
struct StagedOver<'a, T: StagedConnect> {
    transport: &'a T,
    staged: T::Staged,
}

impl<T> BoxedStagedConnect for T
where
    T: StagedConnect<Error = CoreError> + std::fmt::Debug + 'static,
    T::Body: 'static,
    T::Body: http_body::Body<Data = Bytes, Error = CoreError> + Send, // send-bound-exception: amendment-C12
    T::Staged: 'static,
{
    fn connect_boxed<'a>(&'a self, req: http::Request<RequestBody>) -> Staging<'a> {
        Box::pin(async move {
            let staged = self.connect(req).await?;
            let handle: Box<dyn BoxedStaged<'a> + 'a> = Box::new(StagedOver {
                transport: self,
                staged,
            });
            Ok(handle)
        })
    }
}

impl<'a, T> BoxedStaged<'a> for StagedOver<'a, T>
where
    T: StagedConnect<Error = CoreError>,
    T::Body: http_body::Body<Data = Bytes, Error = CoreError> + Send + 'static, // send-bound-exception: amendment-C12
{
    fn exchange_boxed(self: Box<Self>) -> SendExchange<'a> {
        let me = *self;
        Box::pin(async move {
            me.transport
                .exchange(me.staged)
                .await
                .map(|r| r.map(|b| Box::pin(b) as SendBoxBody))
        })
    }
}
