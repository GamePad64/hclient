mod decode;
mod lines;

pub use decode::{SseDecoder, SseError, SseEvent};
pub(crate) use lines::LineSplitter;

/// Default limit — matches `rmcp::DEFAULT_MAX_SSE_EVENT_SIZE` so the adapter
/// doesn't change behavior.
pub const DEFAULT_MAX_EVENT_SIZE: usize = 16 * 1024 * 1024;
