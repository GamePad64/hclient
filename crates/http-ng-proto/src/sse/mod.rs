mod decode;
mod lines;

pub use decode::{SseDecoder, SseError, SseEvent};
pub(crate) use lines::LineSplitter;

/// Лимит по умолчанию — совпадает с `rmcp::DEFAULT_MAX_SSE_EVENT_SIZE`,
/// чтобы адаптер не менял поведение.
pub const DEFAULT_MAX_EVENT_SIZE: usize = 16 * 1024 * 1024;
