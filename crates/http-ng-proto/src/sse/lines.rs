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
    /// Байт терминатора, проглоченный на границе чанка (LF, догнавший CR из
    /// предыдущего `push`). Начисляется следующей возвращённой строке: иначе
    /// лимит размера события недоучитывает до ~1.5× при побайтовой доставке.
    carried_terminator: usize,
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
            carried_terminator: 0,
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
                    self.carried_terminator += 1;
                    continue;
                }
            }
            self.buf.push(b);
        }
    }

    /// Возвращает строку и число реально потреблённых байт **включая
    /// терминатор**. Потребитель обязан считать лимит по этому числу, а не по
    /// `line.len() + 1`: CRLF занимает два байта, и предположение об
    /// однобайтовом терминаторе даёт недоучёт, растущий с числом строк.
    ///
    /// Точен и при разрыве CRLF границей чанка: проглоченный в `push` LF
    /// накапливается в `carried_terminator` и начисляется здесь той строке,
    /// что вернётся первой после его прихода.
    pub(crate) fn next_line(&mut self) -> Option<(Vec<u8>, usize)> {
        let hay = &self.buf[self.start..];
        let pos = hay.iter().position(|&b| b == b'\n' || b == b'\r')?;
        let term = hay[pos];
        let line = hay[..pos].to_vec();
        let mut consumed = pos + 1 + core::mem::take(&mut self.carried_terminator);
        self.start += pos + 1; // строка плюс сам терминатор
        if term == b'\r' {
            if self.buf.get(self.start) == Some(&b'\n') {
                self.start += 1; // CRLF
                consumed += 1;
            } else if self.start == self.buf.len() {
                self.pending_cr = true; // CR в конце — LF может прийти следующим чанком
            }
        }
        Some((line, consumed))
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
            while let Some((l, _)) = s.next_line() {
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

    /// Регресс на недоучёт LF, проглоченного на границе чанка. До фикса
    /// `carried_terminator` этот байт нигде не начислялся: сумма `consumed`
    /// по обеим строкам получалась 6 вместо 7 реально потреблённых байт.
    #[test]
    fn carries_swallowed_lf_across_push_to_next_line_accounting() {
        let mut s = LineSplitter::new();
        s.push(b"ab\r");
        let (line1, consumed1) = s.next_line().expect("CR terminates the first line");
        assert_eq!(line1, b"ab");
        assert_eq!(s.next_line(), None, "буфер пуст, CR ждёт возможный LF");

        s.push(b"\ncd\n");
        let (line2, consumed2) = s.next_line().expect("LF terminates the second line");
        assert_eq!(line2, b"cd");

        assert_eq!(
            consumed1 + consumed2,
            7,
            "суммарно потреблено должно совпадать с суммой длин чанков \
             (\"ab\\r\" = 3 + \"\\ncd\\n\" = 4 = 7), иначе LF, проглоченный на \
             границе чанка, потерян и лимит размера события недоучитывает"
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

    /// Единственный тест, покрывающий учёт байт BOM, по которым решение ещё не
    /// принято. `incomplete_line_is_withheld` его не покрывает: там BOM нет вовсе.
    #[test]
    fn buffered_len_counts_bytes_held_inside_an_undecided_bom() {
        let mut s = LineSplitter::new();
        s.push(&[0xEF, 0xBB]); // два из трёх байт BOM — решение ещё не принято
        assert_eq!(
            s.buffered_len(),
            2,
            "недоучёт даёт обойти лимит размера события в декодере"
        );
        assert_eq!(s.next_line(), None);

        s.push(&[0xBF]); // BOM собрался целиком и снят
        assert_eq!(s.buffered_len(), 0);

        s.push(b"ab");
        assert_eq!(s.buffered_len(), 2);
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

    /// Регресс на квадратичность.
    ///
    /// Порог по абсолютному времени бесполезен, и это проверено: ре-ревью
    /// скопировало тело прежней версии этого теста в квадратичный код, и оно
    /// прошло за 0.26 с при бюджете 2 с. Поэтому проверяется КЛАСС сложности —
    /// отношение времён при четырёхкратном росте входа. Линейность предсказывает
    /// ~4×, квадратичность ~16×; порог 8× оставляет запас на шум планировщика и
    /// при этом отделяет один класс от другого.
    #[test]
    fn parsing_scales_linearly_not_quadratically() {
        fn parse_millis(lines: usize) -> f64 {
            let mut input = Vec::with_capacity(lines * 8);
            for _ in 0..lines {
                input.extend_from_slice(b"data: x\n");
            }
            let start = std::time::Instant::now();
            let got = collect(&[&input]);
            let elapsed = start.elapsed().as_secs_f64() * 1000.0;
            assert_eq!(got.len(), lines);
            elapsed
        }

        // Разогрев: первый прогон платит за аллокатор и прогрев кэша.
        let _ = parse_millis(2_000);

        // Поднимаем базовый размер, пока замер тонет в разрешении таймера:
        // отношение двух шумов не значит ничего.
        let mut n = 4_000;
        let (small, large) = loop {
            let small = parse_millis(n);
            if small >= 1.0 || n >= 64_000 {
                break (small, parse_millis(n * 4));
            }
            n *= 2;
        };

        let ratio = large / small.max(0.001);
        assert!(
            ratio < 8.0,
            "вход вырос в 4 раза, время — в {ratio:.1} ({small:.2} мс -> {large:.2} мс \
             при n={n}): похоже на O(n^2)"
        );
    }
}
