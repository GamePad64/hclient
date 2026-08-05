/// Разбивает байтовый поток на строки по правилам WHATWG EventSource:
/// снимается ровно один ведущий BOM, терминаторы — CRLF, LF или одиночный CR.
/// Переживает разрыв чанка в любом месте, включая середину BOM и между CR и LF.
#[derive(Debug)]
pub(crate) struct LineSplitter {
    buf: Vec<u8>,
    /// Сколько байт с начала `buf` уже отдано наружу.
    ///
    /// Существует ради сложности: сдвигать буфер на каждой строке
    /// (`drain(..pos)` + `remove(0)`) стоит O(n·k) для чанка с k строками.
    /// Замерено на прошлой версии: 50k коротких строк — 51 мс, 100k — 225 мс,
    /// 200k — 925 мс, то есть 4× на каждое удвоение. Это парсер недоверенного
    /// тела ответа, так что квадратичность здесь — вектор атаки.
    start: usize,
    /// Сколько байт BOM уже подтверждено. 3 = BOM обработан (снят или отвергнут).
    bom_seen: usize,
    bom_done: bool,
    /// Предыдущий байт был CR — следующий LF надо проглотить.
    pending_cr: bool,
}

const BOM: [u8; 3] = [0xEF, 0xBB, 0xBF];

impl LineSplitter {
    pub(crate) fn new() -> Self {
        Self {
            buf: Vec::new(),
            start: 0,
            bom_seen: 0,
            bom_done: false,
            pending_cr: false,
        }
    }

    pub(crate) fn push(&mut self, chunk: &[u8]) {
        // Компактация раз в push, а не раз в строку: суммарно линейно.
        if self.start > 0 {
            self.buf.drain(..self.start);
            self.start = 0;
        }

        let mut rest = chunk;

        // Фаза BOM: копим до трёх байт, решаем один раз.
        while !self.bom_done && !rest.is_empty() {
            let b = rest[0];
            if b == BOM[self.bom_seen] {
                self.bom_seen += 1;
                rest = &rest[1..];
                if self.bom_seen == 3 {
                    self.bom_done = true; // BOM снят целиком
                }
            } else {
                // Не BOM: то, что накопили, — обычные данные.
                self.buf.extend_from_slice(&BOM[..self.bom_seen]);
                self.bom_done = true;
            }
        }

        for &b in rest {
            if self.pending_cr {
                self.pending_cr = false;
                if b == b'\n' {
                    continue; // LF после CR уже учтён терминатором
                }
            }
            self.buf.push(b);
        }
    }

    pub(crate) fn next_line(&mut self) -> Option<Vec<u8>> {
        let hay = &self.buf[self.start..];
        let pos = hay.iter().position(|&b| b == b'\n' || b == b'\r')?;
        let term = hay[pos];
        let line = hay[..pos].to_vec();
        self.start += pos + 1; // строка плюс сам терминатор
        if term == b'\r' {
            if self.buf.get(self.start) == Some(&b'\n') {
                self.start += 1; // CRLF
            } else if self.start == self.buf.len() {
                self.pending_cr = true; // CR в конце — LF может прийти следующим чанком
            }
        }
        Some(line)
    }

    pub(crate) fn buffered_len(&self) -> usize {
        // Байты BOM, по которым решение ещё не принято, физически удержаны.
        // Не учитывать их — значит дать обойти лимит размера события в декодере.
        (self.buf.len() - self.start) + if self.bom_done { 0 } else { self.bom_seen }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn collect(chunks: &[&[u8]]) -> Vec<Vec<u8>> {
        let mut s = LineSplitter::new();
        let mut out = Vec::new();
        for c in chunks {
            s.push(c);
            while let Some(l) = s.next_line() {
                out.push(l)
            }
        }
        out
    }

    #[test]
    fn splits_on_all_three_terminators() {
        assert_eq!(
            collect(&[b"a\nb\r\nc\rd\n"]),
            vec![b"a".to_vec(), b"b".to_vec(), b"c".to_vec(), b"d".to_vec()]
        );
    }

    #[test]
    fn strips_exactly_one_bom() {
        assert_eq!(collect(&[b"\xEF\xBB\xBFa\n"]), vec![b"a".to_vec()]);
        // второй BOM — обычные данные
        assert_eq!(
            collect(&[b"\xEF\xBB\xBF\xEF\xBB\xBFa\n"]),
            vec![b"\xEF\xBB\xBFa".to_vec()]
        );
    }

    #[test]
    fn bom_split_across_chunks() {
        assert_eq!(
            collect(&[b"\xEF", b"\xBB", b"\xBFa\n"]),
            vec![b"a".to_vec()]
        );
    }

    #[test]
    fn crlf_split_across_chunks_yields_one_line() {
        assert_eq!(
            collect(&[b"a\r", b"\nb\n"]),
            vec![b"a".to_vec(), b"b".to_vec()]
        );
    }

    #[test]
    fn lone_cr_at_chunk_end_then_non_lf() {
        assert_eq!(
            collect(&[b"a\r", b"b\n"]),
            vec![b"a".to_vec(), b"b".to_vec()]
        );
    }

    #[test]
    fn incomplete_line_is_withheld() {
        let mut s = LineSplitter::new();
        s.push(b"partial");
        assert_eq!(s.next_line(), None);
        assert_eq!(s.buffered_len(), 7);
    }

    #[test]
    fn empty_line_is_yielded() {
        assert_eq!(
            collect(&[b"a\n\nb\n"]),
            vec![b"a".to_vec(), Vec::new(), b"b".to_vec()]
        );
    }

    use proptest::prelude::*;

    proptest! {
        #[test]
        fn chunking_does_not_change_lines(
            prefix_bom: bool,
            data: Vec<u8>,
            splits in proptest::collection::vec(0usize..4096, 0..4),
        ) {
            // Случайный Vec<u8> практически никогда не начнётся с EF BB BF
            // (1 к 16 млн), поэтому BOM подставляется явно.
            let mut input = Vec::new();
            if prefix_bom { input.extend_from_slice(&[0xEF, 0xBB, 0xBF]) }
            input.extend_from_slice(&data);

            let whole = collect(&[&input]);

            // Произвольное число кусков в произвольных местах: двух мало —
            // состояние (pending_cr, фаза BOM) должно переживать несколько
            // границ подряд.
            let mut cuts: Vec<usize> = splits.iter().map(|s| s % (input.len() + 1)).collect();
            cuts.sort_unstable();
            let mut chunks: Vec<&[u8]> = Vec::new();
            let mut prev = 0;
            for c in cuts {
                chunks.push(&input[prev..c]);
                prev = c;
            }
            chunks.push(&input[prev..]);

            prop_assert_eq!(whole, collect(&chunks));
        }
    }

    /// Регресс на измеренную квадратичность: много коротких строк в одном чанке.
    #[test]
    fn many_lines_in_one_chunk_is_linear() {
        let mut input = Vec::new();
        for _ in 0..50_000 {
            input.extend_from_slice(b"data: x\n");
        }
        let start = std::time::Instant::now();
        let lines = collect(&[&input]);
        let elapsed = start.elapsed();
        assert_eq!(lines.len(), 50_000);
        // Прежняя версия давала ~51 мс в release и кратно больше в debug.
        // Порог намеренно щедрый: ловим класс O(n^2), а не микросекунды.
        assert!(
            elapsed < std::time::Duration::from_secs(2),
            "разбор занял {elapsed:?}"
        );
    }
}
