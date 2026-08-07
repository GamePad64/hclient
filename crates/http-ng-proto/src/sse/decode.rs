use super::LineSplitter;
use core::time::Duration;
use std::collections::VecDeque;

/// An SSE event. `Comment` and `Retry` are first-class on purpose: without
/// the former you can't build a keep-alive detector, without the latter
/// blocks containing only `retry:` are lost.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SseEvent {
    Message {
        event: Option<String>,
        data: String,
        id: Option<String>,
    },
    Comment(String),
    Retry(Duration),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SseError {
    /// The raw event size limit was exceeded. Fatal and **not retried**.
    EventTooLarge { limit: usize },
}

impl core::fmt::Display for SseError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            SseError::EventTooLarge { limit } => write!(f, "SSE event exceeds {limit} bytes"),
        }
    }
}
impl std::error::Error for SseError {}

#[derive(Debug)]
pub struct SseDecoder {
    lines: LineSplitter,
    max_event_size: usize,
    /// Bytes accumulated in the current event (raw, before parsing).
    event_bytes: usize,
    data: String,
    event_type: Option<String>,
    last_event_id: Option<String>,
    /// Events ready to be handed out.
    ready: VecDeque<SseEvent>,
}

impl SseDecoder {
    pub fn new(max_event_size: usize) -> Self {
        Self {
            lines: LineSplitter::new(),
            max_event_size,
            event_bytes: 0,
            data: String::new(),
            event_type: None,
            last_event_id: None,
            ready: Default::default(),
        }
    }

    pub fn last_event_id(&self) -> Option<&str> {
        self.last_event_id.as_deref()
    }

    pub fn push(&mut self, chunk: &[u8]) -> Result<(), SseError> {
        self.lines.push(chunk);
        while let Some((line, consumed)) = self.lines.next_line() {
            if line.is_empty() {
                self.dispatch();
                self.event_bytes = 0;
                continue;
            }
            self.event_bytes = self.event_bytes.saturating_add(consumed);
            if self.event_bytes > self.max_event_size {
                return Err(SseError::EventTooLarge {
                    limit: self.max_event_size,
                });
            }
            self.handle_line(&line);
        }
        // An incomplete line counts too — otherwise the limit can be
        // bypassed with an infinite line that never terminates.
        if self.event_bytes + self.lines.buffered_len() > self.max_event_size {
            return Err(SseError::EventTooLarge {
                limit: self.max_event_size,
            });
        }
        Ok(())
    }

    // Named `next`, not `Iterator::next`, deliberately: the decoder
    // requires interleaving with `push` and can't be an iterator in the
    // ordinary sense — `Iterator` has no way to report `SseError`, and
    // `push` mutates the buffer between calls. The name is fixed by this
    // task's interface.
    #[allow(clippy::should_implement_trait)]
    pub fn next(&mut self) -> Option<SseEvent> {
        self.ready.pop_front()
    }

    fn handle_line(&mut self, line: &[u8]) {
        if line[0] == b':' {
            // EXACTLY one leading space is stripped, same as for fields.
            // `trim_start_matches(' ')` would strip them all and lose
            // significant ones.
            let raw = &line[1..];
            let raw = if raw.first() == Some(&b' ') {
                &raw[1..]
            } else {
                raw
            };
            self.ready
                .push_back(SseEvent::Comment(String::from_utf8_lossy(raw).into_owned()));
            return;
        }
        let (name, value) = match line.iter().position(|&b| b == b':') {
            Some(i) => {
                let v = &line[i + 1..];
                let v = if v.first() == Some(&b' ') { &v[1..] } else { v };
                (&line[..i], v)
            }
            None => (line, &line[line.len()..]),
        };
        match name {
            b"data" => {
                // WHATWG: the value AND a newline are appended to the
                // buffer. One trailing newline is stripped on dispatch. A
                // "separator only between non-empty fields" scheme gives a
                // different result for an empty first field.
                self.data.push_str(&String::from_utf8_lossy(value));
                self.data.push('\n');
            }
            b"event" => {
                // A repeated field — the last one wins, and it's NOT an error.
                self.event_type = Some(String::from_utf8_lossy(value).into_owned());
            }
            b"id" => {
                if !value.contains(&0) {
                    self.last_event_id = Some(String::from_utf8_lossy(value).into_owned());
                }
            }
            b"retry" if !value.is_empty() && value.iter().all(|b| b.is_ascii_digit()) => {
                if let Ok(ms) = core::str::from_utf8(value).unwrap_or("").parse::<u64>() {
                    self.ready
                        .push_back(SseEvent::Retry(Duration::from_millis(ms)));
                }
            }
            _ => {} // an unknown field is ignored
        }
    }

    fn dispatch(&mut self) {
        let event = self.event_type.take();
        if self.data.is_empty() {
            // Empty data buffer: reset without dispatch.
            // last_event_id is NOT reset here.
            return;
        }
        let mut data = core::mem::take(&mut self.data);
        if data.ends_with('\n') {
            data.pop();
        }
        self.ready.push_back(SseEvent::Message {
            event,
            data,
            id: self.last_event_id.clone(),
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::time::Duration;

    fn events(input: &[u8]) -> Vec<SseEvent> {
        let mut d = SseDecoder::new(1024);
        d.push(input).unwrap();
        let mut out = Vec::new();
        while let Some(e) = d.next() {
            out.push(e)
        }
        out
    }

    #[test]
    fn dispatches_simple_message() {
        assert_eq!(
            events(b"data: hello\n\n"),
            vec![SseEvent::Message {
                event: None,
                data: "hello".into(),
                id: None
            }]
        );
    }

    #[test]
    fn strips_exactly_one_leading_space_after_colon() {
        assert_eq!(
            events(b"data:  two spaces\n\n"),
            vec![SseEvent::Message {
                event: None,
                data: " two spaces".into(),
                id: None
            }]
        );
    }

    #[test]
    fn joins_multiple_data_lines_with_lf_and_trims_trailing() {
        assert_eq!(
            events(b"data: a\ndata: b\n\n"),
            vec![SseEvent::Message {
                event: None,
                data: "a\nb".into(),
                id: None
            }]
        );
    }

    #[test]
    fn repeated_event_field_last_wins_not_an_error() {
        assert_eq!(
            events(b"event: a\nevent: b\ndata: x\n\n"),
            vec![SseEvent::Message {
                event: Some("b".into()),
                data: "x".into(),
                id: None
            }]
        );
    }

    #[test]
    fn comment_is_surfaced_not_swallowed() {
        assert_eq!(
            events(b": keep-alive\n"),
            vec![SseEvent::Comment("keep-alive".into())]
        );
    }

    #[test]
    fn retry_only_block_is_not_lost() {
        assert_eq!(
            events(b"retry: 5000\n\n"),
            vec![SseEvent::Retry(Duration::from_millis(5000))]
        );
    }

    #[test]
    fn retry_rejects_non_ascii_digits() {
        assert_eq!(events(b"retry: +5000\n\n"), vec![]);
        assert_eq!(events(b"retry: 1e3\n\n"), vec![]);
    }

    #[test]
    fn id_persists_across_events_and_nul_is_ignored() {
        let mut d = SseDecoder::new(1024);
        d.push(b"id: 42\ndata: a\n\ndata: b\n\n").unwrap();
        let a = d.next().unwrap();
        let b = d.next().unwrap();
        assert_eq!(
            a,
            SseEvent::Message {
                event: None,
                data: "a".into(),
                id: Some("42".into())
            }
        );
        assert_eq!(
            b,
            SseEvent::Message {
                event: None,
                data: "b".into(),
                id: Some("42".into())
            }
        );
        assert_eq!(d.last_event_id(), Some("42"));

        let mut d2 = SseDecoder::new(1024);
        d2.push(b"id: 4\x002\ndata: a\n\n").unwrap();
        assert_eq!(
            d2.next().unwrap(),
            SseEvent::Message {
                event: None,
                data: "a".into(),
                id: None
            }
        );
    }

    #[test]
    fn empty_data_buffer_dispatches_nothing_but_id_survives() {
        let mut d = SseDecoder::new(1024);
        d.push(b"id: 7\n\ndata: x\n\n").unwrap();
        assert_eq!(
            d.next().unwrap(),
            SseEvent::Message {
                event: None,
                data: "x".into(),
                id: Some("7".into())
            }
        );
        assert!(d.next().is_none());
    }

    #[test]
    fn field_without_colon_is_name_with_empty_value() {
        // "data" is equivalent to "data:"
        assert_eq!(
            events(b"data\ndata: x\n\n"),
            vec![SseEvent::Message {
                event: None,
                data: "\nx".into(),
                id: None
            }]
        );
    }

    #[test]
    fn oversized_event_is_a_fatal_error() {
        let mut d = SseDecoder::new(16);
        let err = d.push(b"data: 0123456789abcdefghij\n\n").unwrap_err();
        assert_eq!(err, SseError::EventTooLarge { limit: 16 });
    }

    /// Regression for undercounting CRLF: the old version charged
    /// `line.len() + 1`, i.e. assumed a one-byte terminator. `"x:0\r\n"` is
    /// 5 bytes on the wire, of which 3 are the line itself. At a limit of
    /// 16, the old code charged 4 bytes per line (3 + 1) and let through 4
    /// such lines — 16 ≤ 16 — even though the real wire volume was already
    /// 20 bytes, a quarter over the limit.
    #[test]
    fn crlf_terminators_are_charged_at_their_real_width() {
        // 4 lines × 5 bytes = 20 bytes on the wire — must be rejected.
        let mut d = SseDecoder::new(16);
        let err = d.push(b"x:0\r\nx:0\r\nx:0\r\nx:0\r\n").unwrap_err();
        assert_eq!(err, SseError::EventTooLarge { limit: 16 });

        // The boundary hasn't shifted the other way either: 3 lines × 5
        // bytes = 15 bytes on the wire — exactly under the limit — must
        // pass.
        let mut d = SseDecoder::new(16);
        d.push(b"x:0\r\nx:0\r\nx:0\r\n")
            .expect("15 bytes of CRLF lines fit within the 16-byte limit");

        // LF (one-byte terminator, behavior unchanged): 12 bytes under the
        // 16-byte limit must pass.
        let mut d = SseDecoder::new(16);
        d.push(b"data: abcde\n")
            .expect("12 bytes of an LF line fit within the 16-byte limit");
    }
}
