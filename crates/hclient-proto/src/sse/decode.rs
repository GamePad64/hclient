pub use crate::error::SseError;

use super::LineSplitter;
use core::time::Duration;
use std::collections::VecDeque;

/// An SSE event. `Comment` and `Retry` are first-class on purpose: without
/// the former you can't build a keep-alive detector, without the latter
/// blocks containing only `retry:` are lost.
///
/// **Deliberately not `#[non_exhaustive]`**:
/// exhaustiveness is the mechanism. Both consumers here branch on every
/// arm with no `_` — `hclient-cli` renders each one differently and
/// `hclient`'s reconnecting stream acts on `Retry` alone — so a fourth
/// kind of event must be a compile error at each of them rather than
/// silently taking whichever arm a wildcard held. That is the same
/// bargain `hclient_core::hooks::Event` makes, and the reason to name it
/// here is that the type looks like the *handed back and only read*
/// shape until you notice every reader is a `match`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SseEvent {
    /// A dispatched message: a `data` buffer plus whatever `event:` and
    /// `id:` were in force when the blank line arrived.
    Message {
        /// The `event:` field, `None` where the block named none — which
        /// WHATWG makes the event type `message`. Not defaulted here,
        /// because *the server named it* and *the server did not* are
        /// different facts and only the caller knows whether it cares.
        event: Option<String>,
        /// The `data:` buffer with its single trailing newline removed,
        /// so several `data:` lines arrive as one string joined by `\n`.
        data: String,
        /// The last event ID **in force**, which is not the same as one
        /// this block carried: WHATWG's buffer persists across blocks
        /// and across reconnects, so a message with no `id:` of its own
        /// still reports whatever the last one established.
        id: Option<String>,
    },
    /// A `: comment` line, with exactly one leading space stripped after
    /// the colon. Kept rather than swallowed because a comment is how
    /// every real SSE deployment sends a keep-alive, so a caller
    /// detecting a dead stream has nothing else to look at.
    Comment(String),
    /// A `retry:` line — the server's instruction for how long to wait
    /// before reconnecting. Reported rather than acted on: this crate is
    /// sans-io and does not reconnect.
    Retry(Duration),
}

/// WHATWG's `EventSource` decoder, sans-io: bytes in through
/// [`push`](Self::push), events out through [`next`](Self::next).
///
/// The two must be **interleaved** — `push` parses and queues, `next`
/// drains one at a time — which is why this is not an `Iterator`:
/// `Iterator::next` has nowhere to report [`SseError`] and no way to be
/// handed more bytes between calls.
///
/// It holds a partial event across chunk boundaries, so a caller feeds it
/// whatever a frame delivered without aligning anything. What it does
/// **not** do is reconnect, sleep, or act on a `retry:` — those need a
/// clock and a socket, and live in `hclient`'s SSE stream.
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
    /// A decoder bounded at `max_event_size` raw bytes per event —
    /// [`crate::sse::DEFAULT_MAX_EVENT_SIZE`] is the usual argument.
    ///
    /// The bound counts bytes **off the wire**, including terminators and
    /// including a trailing line that has not terminated yet, so it
    /// cannot be walked around with a line that never ends.
    #[must_use]
    pub fn new(max_event_size: usize) -> Self {
        Self {
            lines: LineSplitter::new(),
            max_event_size,
            event_bytes: 0,
            data: String::new(),
            event_type: None,
            last_event_id: None,
            ready: VecDeque::default(),
        }
    }

    /// Like [`Self::new`], but seeds the last event ID buffer instead of
    /// starting it at `None`.
    ///
    /// Exists for reconnect (`hclient`'s `ReconnectingSseStream`): WHATWG's
    /// last event ID buffer is a property of the `EventSource` as a whole,
    /// not of one connection — a message dispatched on a fresh connection
    /// that hasn't sent its own `id:` field yet must still carry forward
    /// whatever id the PREVIOUS connection last established, not `None`.
    /// Without this, every `SseDecoder` created for a reconnect would start
    /// id-less regardless of what the caller already knew, and every
    /// `SseEvent::Message` dispatched before the new connection's own first
    /// `id:` line would silently under-report its `id` — observable
    /// directly by a consumer matching WHATWG's `MessageEvent.lastEventId`
    /// against ours.
    pub fn new_with_last_event_id(max_event_size: usize, last_event_id: Option<String>) -> Self {
        Self {
            last_event_id,
            ..Self::new(max_event_size)
        }
    }

    /// The last event ID buffer as it stands now.
    ///
    /// What a reconnecting caller sends as `Last-Event-ID`, and what to
    /// carry into [`Self::new_with_last_event_id`] for the next
    /// connection's decoder.
    #[must_use]
    pub fn last_event_id(&self) -> Option<&str> {
        self.last_event_id.as_deref()
    }

    /// # Errors
    ///
    /// [`SseError::EventTooLarge`] once the current event — including an
    /// incomplete trailing line still buffered, so the limit cannot be
    /// bypassed with a line that never terminates — exceeds
    /// `max_event_size`.
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

    /// The next event [`push`](Self::push) has already parsed, or `None`
    /// when the queue is empty.
    ///
    /// `None` means *nothing ready*, never *the stream ended*: this type
    /// has no notion of an end, and more bytes may produce more events.
    /// Drain it in a loop after every `push`.
    // Named `next`, not `Iterator::next`, deliberately: the decoder
    // requires interleaving with `push` and can't be an iterator in the
    // ordinary sense — `Iterator` has no way to report `SseError`, and
    // `push` mutates the buffer between calls. The name is fixed by this
    // task's interface.
    #[allow(
        clippy::should_implement_trait,
        reason = "Named `next`, not `Iterator::next`, deliberately: the decoder requires interleaving with `push` and can't be an iterator in the ordinary sense — `Iterator` has no way to report `SseError`, and `push` mutates the buffer between calls. The name is fixed by this task's interface."
    )]
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
            b"retry" if !value.is_empty() && value.iter().all(u8::is_ascii_digit) => {
                // The guard already proved every byte is an ASCII digit, so the
                // only way `parse` can fail is overflow — and a value too large
                // for `u64` is still a value the spec says to honour. Saturate
                // rather than drop it: swallowing the `Err` would leave a
                // digits-only field silently doing nothing, and how long a wait
                // may really be is `Backoff::max`'s call, not this parser's.
                let ms = core::str::from_utf8(value)
                    .expect("ASCII digits are valid UTF-8")
                    .parse::<u64>()
                    .unwrap_or(u64::MAX);
                self.ready
                    .push_back(SseEvent::Retry(Duration::from_millis(ms)));
            }
            _ => {} // an unknown field is ignored
        }
    }

    fn dispatch(&mut self) {
        // Taken BEFORE the early return below: a block with no data dispatches
        // nothing, but it must still clear the event type, or the type leaks
        // forward onto whatever event comes next.
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
            out.push(e);
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

    /// `new_with_last_event_id` — reconnect's reason for existing: a
    /// message dispatched before the seeded decoder sees its OWN `id:`
    /// line must still carry the seed forward, not `None`.
    #[test]
    fn seeded_last_event_id_carries_into_a_message_with_no_id_field_of_its_own() {
        let mut d = SseDecoder::new_with_last_event_id(1024, Some("seed".into()));
        d.push(b"data: a\n\n").unwrap();
        assert_eq!(
            d.next().unwrap(),
            SseEvent::Message {
                event: None,
                data: "a".into(),
                id: Some("seed".into())
            }
        );
        assert_eq!(d.last_event_id(), Some("seed"));
    }

    /// The seed is only a STARTING value, not sticky: the decoder's own
    /// `id:` line still overrides it exactly like it would override any
    /// other previously-set id.
    #[test]
    fn seeded_last_event_id_is_overridden_by_the_decoders_own_id_field() {
        let mut d = SseDecoder::new_with_last_event_id(1024, Some("seed".into()));
        d.push(b"id: fresh\ndata: a\n\n").unwrap();
        assert_eq!(
            d.next().unwrap(),
            SseEvent::Message {
                event: None,
                data: "a".into(),
                id: Some("fresh".into())
            }
        );
    }

    /// A seed of `None` behaves exactly like plain `new` — the constructor
    /// used for the very first connection, which has nothing to seed.
    #[test]
    fn no_seed_behaves_like_plain_new() {
        let mut d = SseDecoder::new_with_last_event_id(1024, None);
        d.push(b"data: a\n\n").unwrap();
        assert_eq!(
            d.next().unwrap(),
            SseEvent::Message {
                event: None,
                data: "a".into(),
                id: None
            }
        );
    }

    /// A NUL in `id:` makes the field ignored — which is not the same as
    /// making it empty. The existing last-event-ID must survive untouched,
    /// so the next message still carries it. Guarding only the `None` case
    /// would pass a test that starts from no id at all.
    #[test]
    fn an_id_containing_nul_leaves_an_established_id_standing() {
        let mut d = SseDecoder::new(1024);
        d.push(b"id: 7\ndata: a\n\n").unwrap();
        d.push(b"id: b\0d\ndata: c\n\n").unwrap();
        let mut out = Vec::new();
        while let Some(e) = d.next() {
            out.push(e);
        }
        assert_eq!(
            out,
            vec![
                SseEvent::Message {
                    event: None,
                    data: "a".into(),
                    id: Some("7".into())
                },
                SseEvent::Message {
                    event: None,
                    data: "c".into(),
                    id: Some("7".into())
                }
            ]
        );
    }

    /// An `event:` with no data dispatches nothing, but it must not leave the
    /// event type behind for whoever comes next. That rests entirely on
    /// `dispatch` taking the type *before* it returns early on an empty data
    /// buffer; move the `take` below that return and every other test here
    /// still passes while types silently leak forward across events.
    #[test]
    fn an_event_type_does_not_leak_out_of_a_block_that_dispatched_nothing() {
        assert_eq!(
            events(b"event: orphan\n\ndata: x\n\n"),
            vec![SseEvent::Message {
                event: None,
                data: "x".into(),
                id: None
            }]
        );
    }

    #[test]
    fn retry_rejects_non_ascii_digits() {
        assert_eq!(events(b"retry: +5000\n\n"), vec![]);
        assert_eq!(events(b"retry: 1e3\n\n"), vec![]);
    }

    /// A digits-only `retry:` too large for `u64` still sets a reconnection
    /// time. WHATWG says to read the value as a base-10 integer and use it;
    /// it gives no leave to ignore one for being large, and the field passed
    /// the digits check, so dropping it here would be the silent no-op this
    /// crate refuses to ship. Saturating is the honest reading: the caller's
    /// own `Backoff::max` is what decides how long a wait may actually be.
    #[test]
    fn an_overflowing_retry_saturates_rather_than_vanishing() {
        assert_eq!(
            events(b"retry: 99999999999999999999999\n\n"),
            vec![SseEvent::Retry(Duration::from_millis(u64::MAX))]
        );
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

    /// **An event of exactly the limit is not oversized.**
    ///
    /// `max_event_size` is a ceiling, and the two comparisons that enforce
    /// it — one per complete line, one for the bytes still buffered in an
    /// unterminated one — both read `>`. A mutation run found each of them
    /// survivable as `>=`, which turns the ceiling into a value the
    /// decoder refuses: every event sized exactly to the caller's limit
    /// becomes a fatal error, and `oversized_event_is_a_fatal_error`
    /// beside this one cannot see it, because it feeds 27 bytes into a
    /// limit of 16.
    ///
    /// Both sites are exercised, because they answer about different
    /// bytes: the first about lines already terminated, the second about
    /// an incomplete line the limit must still charge for — the check that
    /// stops an infinite line from bypassing the bound. A fix to one that
    /// missed the other leaves half the boundary wrong.
    #[test]
    fn an_event_of_exactly_the_limit_is_accepted_at_both_bounds() {
        // Terminated lines: `data: 0123456789\n` is 17 on the wire, so a
        // limit of 17 is the exact boundary. One byte more must fail, and
        // that pair is what says the limit is still enforced at all.
        let mut d = SseDecoder::new(17);
        d.push(b"data: 0123456789\n\n")
            .expect("an event of exactly the limit fits");

        let mut d = SseDecoder::new(16);
        d.push(b"data: 0123456789\n\n")
            .expect_err("one byte over must still be refused");

        // The unterminated-line bound, which is a different expression
        // over different bytes: nothing has been dispatched, so the whole
        // charge comes from what sits in the splitter.
        let mut d = SseDecoder::new(16);
        d.push(b"data: 0123456789")
            .expect("a buffered line of exactly the limit fits");

        let mut d = SseDecoder::new(15);
        d.push(b"data: 0123456789")
            .expect_err("one byte over, buffered, must still be refused");
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
