//! A response body read as lines.
//!
//! NDJSON and log tailing are the two cases: a body that is a sequence of
//! records separated by newlines, arriving in frames that have nothing to
//! do with where the newlines are. [`LineStream`] holds the part of a line
//! that has arrived until the rest of it does, so a record cut in half by
//! a frame boundary comes out whole.
//!
//! The splitting itself is [`hclient_proto::lines`], which is where the
//! terminator set and the leading-BOM rule are argued. This module is the
//! part that has a body to read: the bound, the ordering of an error
//! against the lines that were already whole when it happened, and what
//! becomes of a final line with no terminator.

use std::collections::VecDeque;
use std::error::Error as StdError;
use std::fmt::Debug;

use bytes::Bytes;
use hclient_core::{Error, ErrorKind};
use hclient_proto::lines::LineSplitter;
use http_body::Body as HttpBody;

use crate::error::LineTooLong;
use crate::response::Response;

/// The ceiling [`Response::lines`] applies to a single line.
///
/// 16 MiB, which is the same number [`hclient_proto::sse::DEFAULT_MAX_EVENT_SIZE`]
/// carries — **written out rather than referenced**, because the two are
/// the same size for different reasons: that one matches
/// `rmcp::DEFAULT_MAX_SSE_EVENT_SIZE` so an adapter's behaviour does not
/// change, and this one is a guess at *no line anybody means to send is
/// this long*. One of them moving is not a reason for the other to.
///
/// The value is arbitrary in absolute terms and the existence of a bound
/// is not — see [`LineStream`] for why `ClientBuilder::response_limit`
/// does not stand in for it.
pub const DEFAULT_MAX_LINE: usize = 16 * 1024 * 1024;

// Hand-written, so that it carries **no `B: Debug` bound** — the same
// reason `Response` and `SseStream` write theirs out: after erasure the
// body is a `dyn http_body::Body`, which is not `Debug`, and a derive here
// would take `.unwrap()` on this type away from every caller.
impl<B> Debug for LineStream<B> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LineStream").finish_non_exhaustive()
    }
}

/// Why the body stopped, once it has.
///
/// The distinction is not decoration: a clean end hands over a final line
/// that never got a terminator, and a failed one must not — see
/// [`LineStream::next`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Ending {
    Eof,
    Failed,
}

/// A stream of lines over any response body.
///
/// # The bound is its own, and `response_limit` cannot stand in for it
///
/// [`ClientBuilder::response_limit`](crate::ClientBuilder::response_limit)
/// bounds the **whole body** and defaults to no limit at all. The two are
/// orthogonal in both directions, which is why this has a bound of its
/// own:
///
/// - a 10 GiB log of 80-byte lines is exactly what tailing is *for*, and a
///   total bound that allowed the lines would have to allow the 10 GiB;
/// - a 1 MiB body that is one unterminated run is nothing at all to a
///   total bound and is the whole of the memory this type would hold.
///
/// That is the same shape as `Timeouts`' `total` against `between_bytes`,
/// which this workspace already argues one crate over: neither bound
/// implies the other, and a caller who set only one has bounded only one
/// thing. Setting `response_limit` as well is still worth doing — it is
/// the bound on the sum, and it is enforced in the body rather than here.
///
/// **The count is the buffered tail, and it over-counts by at most three
/// bytes**: an undecided leading BOM and a terminator swallowed at a frame
/// boundary are held bytes that no line has been credited with yet.
/// Over-counting is the direction a ceiling may err in.
pub struct LineStream<B> {
    resp: Response<B>,
    splitter: LineSplitter,
    /// Lines already split out of the buffer and not yet handed over.
    ///
    /// They are drained eagerly, once per chunk, and that is what makes
    /// the bound mean *one line*: `LineSplitter::buffered_len` is the
    /// whole buffer, so checking it before draining would refuse ten
    /// legitimate lines that happened to arrive in one frame.
    ready: VecDeque<Vec<u8>>,
    max_line: usize,
    /// Held back until `ready` is empty — see [`Self::next`].
    fatal: Option<Error>,
    ending: Option<Ending>,
}

impl<B> LineStream<B>
where
    B: HttpBody<Data = Bytes> + Unpin,
    // `next()` calls `Response::chunk`, which is only defined under this
    // bound (amendment C1), and Rust does not propagate a callee's bound
    // through a call — the same restatement `SseStream` carries one module
    // over, for the same reason.
    B::Error: StdError + Send + Sync + 'static, // send-bound-exception: amendment-C1
{
    /// A line stream over `resp`, refusing any single line longer than
    /// `max_line` bytes.
    ///
    /// [`Response::lines`] is this with [`DEFAULT_MAX_LINE`]; this is the
    /// door for a caller who knows their records' size, in either
    /// direction.
    pub fn new(resp: Response<B>, max_line: usize) -> Self {
        Self {
            resp,
            splitter: LineSplitter::new(),
            ready: VecDeque::new(),
            max_line,
            fatal: None,
            ending: None,
        }
    }

    /// The next line, without its terminator.
    ///
    /// Reads as many chunks from the body as it takes to complete one
    /// line — frame boundaries are not line boundaries, and a caller
    /// should never have to know where either was.
    ///
    /// # Three decisions live here
    ///
    /// **`Bytes`, not `String`.** UTF-8 validation is a second policy, and
    /// putting it here would make one bad byte in a log the end of the
    /// stream rather than one line a caller can look at. It is also the
    /// currency [`Response::chunk`] already deals in, so the line costs no
    /// copy. A caller who wants text writes `str::from_utf8`, and one
    /// reading NDJSON hands the slice to their deserialiser without one
    /// either way.
    ///
    /// **A final line with no terminator is yielded**, on a clean end of
    /// body only. A file whose last record has no trailing newline is
    /// ordinary — half the NDJSON writers in the world produce one — and
    /// dropping it would lose a whole record with nothing said, which is
    /// the silent-loss shape this workspace refuses. The WHATWG rules
    /// [`hclient_proto::lines`] otherwise implements go the other way for
    /// SSE, and that is a fact about *events*: an event is dispatched by a
    /// blank line, so a partial one was never a whole anything.
    ///
    /// **After an error, the tail is not yielded.** An unterminated run
    /// behind a failure is not a short line, it is a line that was cut
    /// off, and handing it over would be indistinguishable from a
    /// complete record — the one place truncation could pass for data.
    /// Lines that *were* whole before the failure are handed over first,
    /// which is [`crate::sse::SseStream`]'s ordering and its reason:
    /// records that arrived correctly are not sacrificed for an earlier
    /// error message.
    ///
    /// The error is terminal and arrives exactly once; every call after it
    /// is `None`.
    pub async fn next(&mut self) -> Option<Result<Bytes, Error>> {
        loop {
            if let Some(line) = self.ready.pop_front() {
                return Some(Ok(Bytes::from(line)));
            }
            if let Some(e) = self.fatal.take() {
                return Some(Err(e));
            }
            match self.ending {
                Some(Ending::Failed) => return None,
                Some(Ending::Eof) => {
                    // `take_unterminated` empties the buffer, so a second
                    // call answers `None` without a flag of its own.
                    let tail = self.splitter.take_unterminated();
                    return (!tail.is_empty()).then(|| Ok(Bytes::from(tail)));
                }
                None => match self.resp.chunk().await {
                    Some(Ok(chunk)) => {
                        self.splitter.push(&chunk);
                        while let Some((line, _)) = self.splitter.next_line() {
                            self.ready.push_back(line);
                        }
                        let buffered = self.splitter.buffered_len();
                        if buffered > self.max_line {
                            self.ending = Some(Ending::Failed);
                            self.fatal = Some(Error::new(
                                ErrorKind::Decode,
                                LineTooLong {
                                    limit: self.max_line,
                                    seen: buffered,
                                },
                            ));
                        }
                    }
                    // Already classified by `Response::chunk`, which is
                    // what keeps a transport's own `ErrorKind` — a
                    // cancelled runtime, a mid-stream TLS failure — from
                    // being flattened into `Body` here.
                    Some(Err(e)) => {
                        self.ending = Some(Ending::Failed);
                        self.fatal = Some(e);
                    }
                    None => self.ending = Some(Ending::Eof),
                },
            }
        }
    }
}
