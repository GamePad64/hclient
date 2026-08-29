mod decode;
// `pub(crate)` rather than private: `crate::lines` is the public door
// onto `LineSplitter`, and a sibling module cannot reach into a private
// one. See that module, and the splitter's own doc, for why the file
// stays here.
pub(crate) mod lines;

pub use decode::{SseDecoder, SseError, SseEvent};
pub(crate) use lines::LineSplitter;

/// Default limit — matches `rmcp::DEFAULT_MAX_SSE_EVENT_SIZE` so the adapter
/// doesn't change behavior.
pub const DEFAULT_MAX_EVENT_SIZE: usize = 16 * 1024 * 1024;
