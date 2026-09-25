//! WHATWG's `EventSource` decoder, sans-io.
//!
//! [`SseDecoder`] turns bytes from a `text/event-stream` response body into
//! [`SseEvent`]s. It holds no socket and no clock: a caller pushes bytes as
//! they arrive and drains whatever events that made ready, and reconnection,
//! waiting out a `retry:` interval and honouring `Last-Event-ID` are left to
//! whoever owns the connection.

mod decode;
// `pub(crate)` rather than private: `crate::lines` is the public door
// onto `LineSplitter`, and a sibling module cannot reach into a private
// one. See that module, and the splitter's own doc, for why the file
// stays here.
pub(crate) mod lines;

pub use decode::{SseDecoder, SseError, SseEvent};
pub(crate) use lines::LineSplitter;

// Maintainer notes (not rendered):
//
// **The number came from `rmcp::DEFAULT_MAX_SSE_EVENT_SIZE`**, so that
// an adapter over that crate kept its behaviour. That adapter is in no
// manifest here and the reason has outlived it, which is worth writing
// down rather than deleting: the figure is now a plain guess at *no
// event anybody means to send is this large*, and nothing makes it
// track `rmcp` if `rmcp` moves.

/// The ceiling [`SseDecoder::new`] is usually handed: 16 MiB for one
/// event, counting the raw bytes rather than the decoded `data`.
///
/// What is load-bearing is that there **is** a bound — an SSE stream is
/// unframed, so a peer that never sends a blank line would otherwise
/// grow the decoder's buffer without limit — and that is
/// [`SseError::EventTooLarge`]'s subject rather than this value's.
/// `hclient`'s `DEFAULT_MAX_LINE` writes out the same 16 MiB for its own
/// reason and deliberately does not reference this one: the two are
/// equal by coincidence and one moving is no reason for the other to.
pub const DEFAULT_MAX_EVENT_SIZE: usize = 16 * 1024 * 1024;
