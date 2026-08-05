use super::LineSplitter;
use core::time::Duration;
use std::collections::VecDeque;

/// Событие SSE. `Comment` и `Retry` — первого класса намеренно: без первого
/// нельзя построить детектор keep-alive, без второго теряются блоки,
/// содержащие только `retry:`.
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
    /// Превышен лимит размера сырого события. Фатально и **не ретраится**.
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
    /// Байт, накопленных в текущем событии (сырых, до парсинга).
    event_bytes: usize,
    data: String,
    event_type: Option<String>,
    last_event_id: Option<String>,
    /// Готовые к выдаче события.
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
        // Незавершённая строка тоже считается — иначе лимит обходится
        // бесконечной строкой без терминатора.
        if self.event_bytes + self.lines.buffered_len() > self.max_event_size {
            return Err(SseError::EventTooLarge {
                limit: self.max_event_size,
            });
        }
        Ok(())
    }

    // Названо `next`, а не `Iterator::next`, намеренно: декодер требует
    // чередования с `push` и не может быть итератором в обычном смысле —
    // `Iterator` не умеет сообщать о `SseError`, а `push` меняет буфер между
    // вызовами. Имя закреплено интерфейсом задачи.
    #[allow(clippy::should_implement_trait)]
    pub fn next(&mut self) -> Option<SseEvent> {
        self.ready.pop_front()
    }

    fn handle_line(&mut self, line: &[u8]) {
        if line[0] == b':' {
            // Снимается РОВНО один ведущий пробел, как и у полей.
            // `trim_start_matches(' ')` снял бы все и потерял бы значащие.
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
                // WHATWG: к буферу дописывается значение И перевод строки.
                // Один хвостовой перевод снимается при диспатче. Схема
                // «разделитель только между непустыми» даёт другой результат
                // для пустого первого поля.
                self.data.push_str(&String::from_utf8_lossy(value));
                self.data.push('\n');
            }
            b"event" => {
                // Повтор поля — последнее побеждает, а НЕ ошибка.
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
            _ => {} // неизвестное поле игнорируется
        }
    }

    fn dispatch(&mut self) {
        let event = self.event_type.take();
        if self.data.is_empty() {
            // Пустой буфер данных: сброс без диспатча.
            // last_event_id при этом НЕ сбрасывается.
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
        // "data" эквивалентно "data:"
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

    /// Регресс на недоучёт CRLF: старая версия заряжала `line.len() + 1`,
    /// то есть предполагала однобайтовый терминатор. `"x:0\r\n"` — 5 байт
    /// провода, из них 3 — сама строка. При лимите 16 старый код заряжал
    /// 4 байта на строку (3 + 1) и пропускал 4 такие строки — 16 ≤ 16 — хотя
    /// реальный объём провода уже 20 байт, на четверть больше лимита.
    #[test]
    fn crlf_terminators_are_charged_at_their_real_width() {
        // 4 строки × 5 байт = 20 байт провода — обязаны быть отвергнуты.
        let mut d = SseDecoder::new(16);
        let err = d.push(b"x:0\r\nx:0\r\nx:0\r\nx:0\r\n").unwrap_err();
        assert_eq!(err, SseError::EventTooLarge { limit: 16 });

        // Граница не смещена в другую сторону: 3 строки × 5 байт = 15 байт
        // провода — ровно под лимитом — обязаны пройти.
        let mut d = SseDecoder::new(16);
        d.push(b"x:0\r\nx:0\r\nx:0\r\n")
            .expect("15 байт CRLF-строк умещается в лимит 16");

        // LF (однобайтовый терминатор, поведение не менялось): 12 байт под
        // лимитом 16 обязаны пройти.
        let mut d = SseDecoder::new(16);
        d.push(b"data: abcde\n")
            .expect("12 байт LF-строки умещается в лимит 16");
    }
}
