//! 0-RTT admission: who decides, what is checked, and what is not covered.
//!
//! # The verdict is a future, not a field
//!
//! `TlsInfo::early_data_accepted: Option<bool>` is the right shape for
//! TLS 1.3 over TCP, where the answer is known by the time the handshake
//! completes. **In QUIC it is not.** Measured on a live loopback
//! exchange: the connection exists at 1.27 ms, the h3 layer is up at
//! 1.68 ms, the response arrives at **8.58 ms**, and the acceptance verdict
//! resolves at **8.63 ms** — fifty microseconds *after* the response body.
//! A field on a handshake result could only hold that by making the connect
//! wait for the handshake, which is precisely the round trip 0-RTT exists
//! to skip.
//!
//! So this crate never touches `TlsInfo::early_data_accepted`, and the
//! quantity it would have held is `quinn::ZeroRttAccepted`, a
//! `Future<Output = bool>` that nobody outside this module ever sees.
//!
//! # Three failure paths, and only two of them are covered
//!
//! | failure | where it appears | what happens here |
//! |---|---|---|
//! | no usable key material | `into_0rtt()` returns the `Connecting` back | fall back to a full handshake, silently: nothing was risked |
//! | the server rejected the 0-RTT keys | `ZeroRttRejected` on the h3 stream | replayed on the same connection once the handshake completes; the caller sees a normal response |
//! | the server refuses to risk it | HTTP **`425 Too Early`** (RFC 8470 §5.2) | **not this layer\'s — see below** |
//!
//! ## `425 Too Early` is not, and cannot be, this transport\'s
//!
//! RFC 8470 §5.2 gives a server `425` to say *"I am unwilling to risk
//! processing a request that might be replayed"*, and requires that *"a
//! user agent SHOULD retry automatically, but any retries MUST NOT be sent
//! in early data"*. That is a **status-code branch in `Client`**, not in a
//! transport: only the client owns the retry loop. `Transport::execute`
//! returns one response for one request, and a transport that resent a
//! request on a `425` would be making a redirect-shaped decision behind
//! the caller\'s back.
//!
//! So a `425` leaves this crate as an ordinary response —
//! `a_425_reaches_the_caller_untouched` in `tests/live.rs` pins that, and
//! it stays true however `Client` evolves, because it is a claim about
//! this layer.
//!
//! ### What the client\'s retry owes this module, and why it is not vacuous
//!
//! *"Any retries MUST NOT be sent in early data"* is a duty on whoever
//! writes the retry, and discharging it is one line: **remove
//! [`AllowEarlyData`] from the replayed request\'s extensions.** The mark
//! is the only thing that can put a request into early data
//! ([`admits_early_data`]), so removing it is both necessary and
//! sufficient.
//!
//! It is worth saying that this duty is **real rather than theoretical**,
//! because the obvious reason to think otherwise is wrong. One might
//! reason: by the time a `425` has come back, the connection\'s handshake
//! has long completed, and streams opened after that are 1-RTT whatever
//! the request says — so the replay could not go into early data even if
//! it tried. That is true of the connection this crate happens to have
//! pooled, and it is an accident of it still being there. The mark is part
//! of `crate::h3::PoolKey`, so a marked replay asks for the early-data
//! connection specifically; if that entry has been evicted, closed by the
//! peer, or timed out in the meantime, the replay builds a **fresh**
//! connection and `into_0rtt` puts it straight back into early data —
//! against the very server that just said it would not risk one.
//!
//! Stripping the mark also routes the replay to a different pool key, and
//! therefore to a connection that was never built to offer early data at
//! all. That is the belt as well as the braces, and it is the reason the
//! strip belongs at the `425` branch rather than here: this module cannot
//! see that a response was a `425`.

use hclient_core::{AllowEarlyData, Error, ErrorKind, RequestBody, RetryKind};

/// Whether this request may go into early data, and why not when it may
/// not.
///
/// # The two conditions are not the same kind of condition
///
/// **The caller's mark is the gate.** [`AllowEarlyData`] in the request's
/// extensions is an assertion that replaying this request is *safe* — a
/// judgement about the operation, which only the caller can make. Without
/// it, nothing goes into early data, whatever else is true.
///
/// **`RetryKind` is a correctness precondition beneath it, and not a second
/// gate.** It answers "can these bytes be sent again", because a rejected
/// 0-RTT request must be replayed after the handshake and a
/// [`RetryKind::Impossible`] body cannot be. It does **not** answer "may an
/// attacker send them again", which is the question that decides exposure —
/// and the two come apart in the direction that matters: `POST /transfer`
/// with a fully buffered body is `RetryKind::Free`, trivially replayable,
/// and exactly the request that must never enter early data.
///
/// Reading `RetryKind` as a safety check is the mistake this function's
/// shape is built to prevent: the caller's mark is checked first and
/// unconditionally, and no combination of body kinds admits a request that
/// was not marked.
pub(crate) fn admits_early_data(req: &http::Request<RequestBody>) -> bool {
    if req.extensions().get::<AllowEarlyData>().is_none() {
        return false;
    }
    // Correctness, not safety: a body we could not put back on the wire
    // after a rejection would strand the request.
    req.body().retry_kind() != RetryKind::Impossible
}

/// The caller asked for early data from a transport that does not offer it.
///
/// A typed refusal rather than a silent downgrade to 1-RTT, on the same
/// argument `check_supported` already makes for a `RedirectPolicy` against
/// an internal-redirect backend: a setting the caller wrote and the stack
/// ignored is worse than one it refused, because only the second is
/// visible.
pub(crate) fn refuse_early_data(backend: &'static str) -> Error {
    Error::new(
        ErrorKind::Unsupported,
        std::io::Error::other(format!(
            "{backend} cannot offer TLS 1.3 early data, and the request carries \
             AllowEarlyData; it was refused rather than silently sent at 1-RTT"
        )),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use bytes::Bytes;
    use http_body_util::BodyExt;
    use std::convert::Infallible;
    use std::sync::Arc;

    fn req(body: RequestBody, marked: bool) -> http::Request<RequestBody> {
        let mut r = http::Request::new(body);
        if marked {
            r.extensions_mut().insert(AllowEarlyData);
        }
        r
    }

    #[test]
    fn an_unmarked_request_never_enters_early_data() {
        // Every body kind, including the two that are trivially replayable.
        // The point of the table is that no property of the BODY can admit
        // a request the caller did not mark: a mutation that reordered the
        // two checks would pass a test that only used one body kind.
        for body in [
            RequestBody::Empty,
            RequestBody::Full(Bytes::from_static(b"x")),
            RequestBody::Rewindable(Arc::new(|| RequestBody::Empty)),
        ] {
            assert!(!admits_early_data(&req(body, false)));
        }
    }

    #[test]
    fn a_marked_request_with_a_replayable_body_is_admitted() {
        assert!(admits_early_data(&req(RequestBody::Empty, true)));
        assert!(admits_early_data(&req(
            RequestBody::Full(Bytes::from_static(b"x")),
            true
        )));
        assert!(admits_early_data(&req(
            RequestBody::Rewindable(Arc::new(|| RequestBody::Empty)),
            true
        )));
    }

    #[test]
    fn a_marked_request_whose_body_cannot_be_replayed_is_refused() {
        // Correctness, not safety: a rejected 0-RTT request is replayed by
        // this transport, and a single-pass body has nothing to replay.
        let body = RequestBody::Streaming(Box::new(
            http_body_util::Empty::<Bytes>::new().map_err(|e: Infallible| match e {}),
        ));
        assert!(!admits_early_data(&req(body, true)));
    }
}
