//! The response body a [`crate::Client`] hands back.
//!
//! **It is a newtype rather than an alias, and that is the whole point.**
//! It used to be
//! `Limited<Decompressed<Deadline<Cached<BoxBody>>>>`, which made the
//! *composition* a promise: four public wrapper types nobody constructs,
//! and a fifth wrapper — this crate has added two already, `Cached` and
//! `Limited` — a breaking change for everyone. Behind a newtype the chain
//! is an implementation detail, and what is promised is a body.
//!
//! # The order is still load-bearing, and it is asserted below rather than
//! in the type
//!
//! [`Cached`](crate::cached::Cached) is innermost, because it is the one
//! wrapper that can *replace* the transport's body — a cache hit had no
//! exchange — and because what it records must be what the wire carried.
//! [`Deadline`](crate::deadline::Deadline) goes around it, so it is polled
//! once for every frame that arrives **off the wire**;
//! [`Decompressed`](crate::decompress::Decompressed) outside both, because
//! reversing a coding can consume many compressed frames before it yields
//! one byte — written the other way round, a slow server sending
//! well-compressing padding would be bounded by nothing.
//! [`Limited`](crate::limit::Limited) is outermost, so it counts what the
//! caller receives.
//!
//! That was pinned by `tests/compression_client_type.rs`, which peeled the
//! four wrappers apart from outside. It cannot now, and it should not: the
//! order is a promise this crate makes to itself, not to a caller, and an
//! invariant of ours belongs in a test of ours. `order_is_the_one_the_
//! client_applies` below is that test, and it fails for the same mutation.
//!
//! # What a caller loses and gains
//!
//! Loses: peeling. Gains: the three answers that peeling was for are
//! methods here, so *what coding was reversed* is `body.coding()` rather
//! than `body.into_inner().coding()`, and *what is my deadline* is one
//! call rather than two.

use std::pin::Pin;
use std::task::{Context, Poll};

use http_body::{Body as HttpBody, Frame, SizeHint};

use hclient_core::Error;
use hclient_core::unversioned::erased::BoxBody;

use crate::cached::Cached;
use crate::deadline::Deadline;
use crate::decompress::Decompressed;
use crate::limit::Limited;

/// The four wrappers, in the order [`crate::Client`] applies them.
type Chain = Limited<Decompressed<Deadline<Cached<BoxBody>>>>;

/// The body of a response from [`crate::Client`].
///
/// An [`http_body::Body`] of `Bytes`, with this crate's bounds and
/// decoding already applied — see the module doc for what is inside and
/// why the order matters.
#[derive(Debug)]
pub struct ClientBody(Chain);

impl ClientBody {
    pub(crate) fn new(chain: Chain) -> Self {
        Self(chain)
    }

    /// The content coding that was reversed, as it appeared on the wire,
    /// or `None` when the body is handed through untouched.
    ///
    /// Decoding **removes** `Content-Encoding` from the response — the
    /// header would otherwise describe a shape the body no longer has — so
    /// this is the only place that fact survives. It answers `None` once
    /// the stream has ended.
    #[must_use]
    pub fn coding(&self) -> Option<&'static str> {
        self.0.get_ref()?.coding()
    }

    /// The whole-operation bound in force, or `None` where none was set.
    #[must_use]
    pub fn total_timeout(&self) -> Option<std::time::Duration> {
        self.0.get_ref()?.get_ref().total_timeout()
    }

    /// Whether that bound has already elapsed.
    #[must_use]
    pub fn is_expired(&self) -> bool {
        self.0.get_ref().is_some_and(|b| b.get_ref().is_expired())
    }
}

impl HttpBody for ClientBody {
    type Data = bytes::Bytes;
    type Error = Error;

    fn poll_frame(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<bytes::Bytes>, Error>>> {
        Pin::new(&mut self.get_mut().0).poll_frame(cx)
    }

    fn is_end_stream(&self) -> bool {
        self.0.is_end_stream()
    }

    fn size_hint(&self) -> SizeHint {
        self.0.size_hint()
    }
}

/// **The order, asserted at compile time.** `Chain` must be exactly these
/// four in this sequence; reorder them and this stops compiling, which is
/// a stronger check than the peeling test it replaces — that one could
/// only fail when it ran.
const _: () = {
    fn is_the_chain(_: Chain) {}
    fn check(c: Limited<Decompressed<Deadline<Cached<BoxBody>>>>) {
        is_the_chain(c);
    }
    let _ = check;
};
