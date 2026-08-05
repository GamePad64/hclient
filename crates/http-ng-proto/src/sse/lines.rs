/// Разбивает байтовый поток на строки по правилам WHATWG EventSource:
/// снимается ровно один ведущий BOM, терминаторы — CRLF, LF или одиночный CR.
/// Переживает разрыв чанка в любом месте, включая середину BOM и между CR и LF.
#[derive(Debug)]
pub(crate) struct LineSplitter {
    buf: Vec<u8>,
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
            bom_seen: 0,
            bom_done: false,
            pending_cr: false,
        }
    }

    pub(crate) fn push(&mut self, chunk: &[u8]) {
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
        let pos = self.buf.iter().position(|&b| b == b'\n' || b == b'\r')?;
        let term = self.buf[pos];
        let line: Vec<u8> = self.buf.drain(..pos).collect();
        self.buf.remove(0); // сам терминатор
        if term == b'\r' {
            if self.buf.first() == Some(&b'\n') {
                self.buf.remove(0); // CRLF внутри буфера
            } else if self.buf.is_empty() {
                self.pending_cr = true; // CR в конце — LF может прийти следующим чанком
            }
        }
        Some(line)
    }

    pub(crate) fn buffered_len(&self) -> usize {
        self.buf.len()
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
        fn chunking_does_not_change_lines(data: Vec<u8>, split_at in 0usize..64) {
            let whole = collect(&[&data]);
            let at = split_at.min(data.len());
            let (a, b) = data.split_at(at);
            let split = collect(&[a, b]);
            prop_assert_eq!(whole, split);
        }
    }
}
