/// Splits a byte stream into lines: exactly one leading BOM is stripped,
/// terminators are CRLF, LF, or a lone CR. Survives a chunk break at any
/// point, including mid-BOM and between CR and LF.
///
/// # It is SSE's splitter, promoted rather than copied
///
/// These are the WHATWG EventSource rules, and they were written here for
/// `SseDecoder` alone. [`crate::lines`] is the public door onto this type,
/// opened when a general line adapter was wanted for NDJSON and log
/// tailing: the overlap was measured before it was believed, and it is the
/// whole file — terminator set, chunk-boundary survival, byte accounting,
/// the compaction that keeps it linear. Nothing was changed to make the
/// second caller fit, and [`Self::take_unterminated`] is the only method
/// added for it, which `SseDecoder` does not call.
///
/// **The file stays under `sse/` and the door is elsewhere**, which is a
/// wart with a reason: `just test-sse-complexity` pins
/// `sse::lines::tests::parsing_scales_linearly_not_quadratically` by its
/// exact path, because it is the one test that must run on a runner of its
/// own. Moving the module would rename that test, and a recipe that
/// silently stops running its one test is the defect this workspace keeps
/// finding.
#[derive(Debug)]
pub struct LineSplitter {
    buf: Vec<u8>,
    /// How many bytes from the start of `buf` have already been handed out.
    ///
    /// Exists for complexity reasons: shifting the buffer on every line
    /// (`drain(..pos)` + `remove(0)`) costs O(n·k) for a chunk with k
    /// lines. Measured on the shifting version: 50k short lines — 51 ms,
    /// 100k — 225 ms, 200k — 925 ms, i.e. 4× per doubling. This is a
    /// parser for an untrusted response body, so quadratic behavior here
    /// is an attack vector.
    start: usize,
    /// How many BOM bytes have already been confirmed. 3 = the BOM has
    /// been resolved (stripped or rejected).
    bom_seen: usize,
    bom_done: bool,
    /// The previous byte was CR — the next LF must be swallowed.
    pending_cr: bool,
    /// A terminator byte swallowed at a chunk boundary (an LF that caught
    /// up with a CR from the previous `push`). Credited to the next line
    /// returned: otherwise the event size limit undercounts by up to ~1.5×
    /// under byte-at-a-time delivery.
    carried_terminator: usize,
}

const BOM: [u8; 3] = [0xEF, 0xBB, 0xBF];

impl Default for LineSplitter {
    fn default() -> Self {
        Self::new()
    }
}

impl LineSplitter {
    pub fn new() -> Self {
        Self {
            buf: Vec::new(),
            start: 0,
            bom_seen: 0,
            bom_done: false,
            pending_cr: false,
            carried_terminator: 0,
        }
    }

    pub fn push(&mut self, chunk: &[u8]) {
        // Compaction once per push, not once per line: linear overall.
        if self.start > 0 {
            self.buf.drain(..self.start);
            self.start = 0;
        }

        let mut rest = chunk;

        // BOM phase: accumulate up to three bytes, decide once.
        while !self.bom_done && !rest.is_empty() {
            let b = rest[0];
            if b == BOM[self.bom_seen] {
                self.bom_seen += 1;
                rest = &rest[1..];
                if self.bom_seen == 3 {
                    self.bom_done = true; // BOM stripped in full
                }
            } else {
                // Not a BOM: what we accumulated is ordinary data.
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

    /// Returns the line and the number of bytes actually consumed
    /// **including the terminator**. The consumer must count the limit
    /// against this number, not against `line.len() + 1`: CRLF takes two
    /// bytes, and assuming a one-byte terminator undercounts, growing with
    /// the number of lines.
    ///
    /// Accurate even when a CRLF is split by a chunk boundary: the LF
    /// swallowed in `push` accumulates in `carried_terminator` and is
    /// credited here to whichever line is returned first after it arrives.
    pub fn next_line(&mut self) -> Option<(Vec<u8>, usize)> {
        let hay = &self.buf[self.start..];
        let pos = hay.iter().position(|&b| b == b'\n' || b == b'\r')?;
        let term = hay[pos];
        let line = hay[..pos].to_vec();
        let mut consumed = pos + 1 + core::mem::take(&mut self.carried_terminator);
        self.start += pos + 1; // the line plus the terminator itself
        if term == b'\r' {
            if self.buf.get(self.start) == Some(&b'\n') {
                self.start += 1; // CRLF
                consumed += 1;
            } else if self.start == self.buf.len() {
                self.pending_cr = true; // CR at the end — the LF may arrive in the next chunk
            }
        }
        Some((line, consumed))
    }

    pub fn buffered_len(&self) -> usize {
        // BOM bytes not yet resolved are physically held. Not counting
        // them would let the event size limit in the decoder be bypassed.
        // carried_terminator is the same case: an LF swallowed at a chunk
        // boundary, not yet handed to any line (there simply isn't a next
        // line yet), but already actually consumed off the wire.
        (self.buf.len() - self.start)
            + if self.bom_done { 0 } else { self.bom_seen }
            + self.carried_terminator
    }

    /// The bytes held with no terminator behind them, taken at the end of
    /// the input.
    ///
    /// **Only a caller that knows the stream has ended may call this**,
    /// which is why it is not `next_line`'s business: mid-stream, an
    /// unterminated run is a line that has not finished arriving, and
    /// handing it over would cut a line in half at a frame boundary.
    ///
    /// An undecided BOM prefix is data here rather than a marker. One or
    /// two bytes of `EF BB BF` with nothing behind them are not a BOM —
    /// a BOM is three bytes — so at end of input they are the last thing
    /// the sender wrote, and dropping them would be a silent loss of the
    /// only kind this splitter can have.
    ///
    /// `SseDecoder` does not call this, and that is not an oversight: the
    /// WHATWG rules dispatch an event on a blank line, so a trailing
    /// partial event is deliberately discarded there.
    pub fn take_unterminated(&mut self) -> Vec<u8> {
        if !self.bom_done {
            self.buf.extend_from_slice(&BOM[..self.bom_seen]);
            self.bom_done = true;
        }
        let rest = self.buf.split_off(self.start);
        self.buf.clear();
        self.start = 0;
        self.carried_terminator = 0;
        rest
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
        // a second BOM is ordinary data
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

    /// Regression for undercounting an LF swallowed at a chunk boundary.
    /// Before the `carried_terminator` fix, this byte was never credited
    /// anywhere: the sum of `consumed` across both lines came out to 6
    /// instead of the 7 bytes actually consumed.
    #[test]
    fn carries_swallowed_lf_across_push_to_next_line_accounting() {
        let mut s = LineSplitter::new();
        s.push(b"ab\r");
        let (line1, consumed1) = s.next_line().expect("CR terminates the first line");
        assert_eq!(line1, b"ab");
        assert_eq!(
            s.next_line(),
            None,
            "the buffer is empty, CR is waiting for a possible LF"
        );

        s.push(b"\ncd\n");
        let (line2, consumed2) = s.next_line().expect("LF terminates the second line");
        assert_eq!(line2, b"cd");

        assert_eq!(
            consumed1 + consumed2,
            7,
            "total consumed must match the sum of chunk lengths \
             (\"ab\\r\" = 3 + \"\\ncd\\n\" = 4 = 7), otherwise the LF swallowed \
             at the chunk boundary is lost and the event size limit undercounts"
        );
    }

    /// Regression for undercounting in `buffered_len()`: between two
    /// consecutive CRLF splits, `carried_terminator` can hold 1 byte not
    /// yet handed to any line (there isn't a next line in the buffer at
    /// all yet), but already actually consumed off the wire. `"x\r"` → line
    /// "x" (CR at the end, LF unknown); `"\ny\r"` → the LF from the
    /// previous chunk is swallowed and credited to line "y", a new CR
    /// hangs again; `"\nz"` → the LF from the previous chunk is swallowed,
    /// there's no line (there's no terminator in "z"), and this byte must
    /// be visible in `buffered_len()`, or the sum consumed is less than the
    /// sum fed in.
    #[test]
    fn buffered_len_counts_a_pending_carried_terminator() {
        let mut s = LineSplitter::new();

        s.push(b"x\r");
        let (line1, consumed1) = s.next_line().expect("CR terminates the first line");
        assert_eq!(line1, b"x");
        assert_eq!(s.next_line(), None);

        s.push(b"\ny\r");
        let (line2, consumed2) = s.next_line().expect("LF terminates the second line");
        assert_eq!(line2, b"y");
        assert_eq!(s.next_line(), None);

        s.push(b"\nz");
        assert_eq!(
            s.next_line(),
            None,
            "\"z\" isn't terminated, there's no line yet"
        );

        assert_eq!(
            consumed1 + consumed2 + s.buffered_len(),
            7,
            "total accounted for (consumed of both lines + buffered_len) must \
             match the sum of chunk lengths (\"x\\r\" = 2 + \"\\ny\\r\" = 3 + \
             \"\\nz\" = 2 = 7); otherwise an unclaimed carried_terminator is lost"
        );
    }

    #[test]
    fn lone_cr_at_chunk_end_then_non_lf() {
        assert_eq!(
            collect(&[b"a\r", b"b\n"]),
            vec![b"a".to_vec(), b"b".to_vec()]
        );
    }

    /// `take_unterminated` is the general line stream's half of the
    /// contract and SSE's non-half: at end of input a trailing run with no
    /// terminator is the last line for one caller and a discarded partial
    /// event for the other, so it is a separate method rather than a
    /// change to `next_line`.
    #[test]
    fn an_unterminated_tail_is_taken_only_when_asked_for() {
        let mut s = LineSplitter::new();
        s.push(b"a\nb");
        assert_eq!(s.next_line().map(|(l, _)| l), Some(b"a".to_vec()));
        assert_eq!(s.next_line(), None, "`b` has no terminator behind it");
        assert_eq!(s.take_unterminated(), b"b".to_vec());
        assert_eq!(s.buffered_len(), 0);
        assert_eq!(s.take_unterminated(), Vec::<u8>::new(), "and only once");
    }

    #[test]
    fn a_terminated_stream_has_no_tail_to_take() {
        // The case that separates "yield the tail" from "yield an empty
        // last line": after a terminator there is nothing buffered, so a
        // body ending in `\n` does not gain a phantom line.
        let mut s = LineSplitter::new();
        s.push(b"a\n");
        assert_eq!(s.next_line().map(|(l, _)| l), Some(b"a".to_vec()));
        assert_eq!(s.take_unterminated(), Vec::<u8>::new());
    }

    /// One or two bytes of `EF BB BF` with nothing behind them are not a
    /// BOM, because a BOM is three bytes. `buffered_len` already counts
    /// them, so dropping them here would make the bytes taken out of this
    /// splitter fewer than the bytes put in — the same accounting defect
    /// `carries_swallowed_lf_across_push_to_next_line_accounting` pins one
    /// field over.
    #[test]
    fn an_undecided_bom_prefix_comes_back_as_data_at_end_of_input() {
        let mut s = LineSplitter::new();
        s.push(&[0xEF, 0xBB]);
        assert_eq!(s.buffered_len(), 2);
        assert_eq!(s.take_unterminated(), vec![0xEF, 0xBB]);
        assert_eq!(s.buffered_len(), 0);
    }

    #[test]
    fn a_complete_bom_is_still_stripped_and_leaves_no_tail() {
        let mut s = LineSplitter::new();
        s.push(&[0xEF, 0xBB, 0xBF]);
        assert_eq!(s.take_unterminated(), Vec::<u8>::new());
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

    /// The one test covering accounting for BOM bytes not yet resolved.
    /// `incomplete_line_is_withheld` doesn't cover this: there's no BOM
    /// there at all.
    #[test]
    fn buffered_len_counts_bytes_held_inside_an_undecided_bom() {
        let mut s = LineSplitter::new();
        s.push(&[0xEF, 0xBB]); // two of the three BOM bytes — not yet resolved
        assert_eq!(
            s.buffered_len(),
            2,
            "undercounting lets the event size limit in the decoder be bypassed"
        );
        assert_eq!(s.next_line(), None);

        s.push(&[0xBF]); // the BOM is complete and stripped
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
            // A random Vec<u8> will almost never start with EF BB BF
            // (1 in 16 million), so the BOM is inserted explicitly.
            let mut input = Vec::new();
            if prefix_bom { input.extend_from_slice(&[0xEF, 0xBB, 0xBF]) }
            input.extend_from_slice(&data);

            let whole = collect(&[&input]);

            // An arbitrary number of pieces at arbitrary points: two isn't
            // enough — the state (pending_cr, the BOM phase) must survive
            // several consecutive boundaries.
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

    /// Regression for quadratic behavior.
    ///
    /// A threshold on absolute time is useless, and this has been
    /// verified: a time-bounded version of this test passes in 0.26 s
    /// against a 2 s budget on quadratic code. So
    /// what's checked is the complexity CLASS — the ratio of times under a
    /// fourfold growth in input. Linearity predicts ~4×, quadratic behavior
    /// ~16×; an 8× threshold leaves headroom for scheduler noise while
    /// still separating one class from the other.
    ///
    /// Runs in its own CI job (`sse-complexity-guard`, `ci.yml`), NOT
    /// sharing a runner with the whole-workspace test job — this
    /// is the primary defense against flakiness. Sustained runner
    /// overcommit (several heavy test processes at once) has been measured
    /// to break best-of-N as the main antidote: the long "large" measurement has no structural way to
    /// dodge preemption in any of its attempts, while the short "small" one
    /// does, so the minimum over five attempts converged to the same
    /// inflated ratio (up to 18.9× against an 8.0× threshold) as a single
    /// measurement, rather than filtering it out — best-of-N amplified the
    /// bias instead of damping it. Isolation eliminates specifically
    /// sustained overcommit; `best_of_three` below is a secondary defense
    /// ONLY against one-off scheduler noise (a GC pause, a random neighbor
    /// on a shared cloud runner's hypervisor), which isolation alone
    /// doesn't rule out. The 8× threshold is untouched: it's the one part
    /// that already worked, and it's exactly what separates linearity from
    /// O(n²) — widening the threshold doesn't work: under that same
    /// sustained overcommit, known-linear code reached 18.9×, so a
    /// threshold robust to that kind of noise would let a genuine quadratic
    /// regression through too.
    ///
    /// Calibration (a 30 ms threshold, not 1 ms) is a separate fix on top
    /// of isolation: isolation removes the overcommit noise and exposes a
    /// DIFFERENT noise the overcommit was masking. At a 1 ms
    /// calibration threshold, the test would stop at n on the order of
    /// 8–16 thousand lines, where the measurement itself lands at 1–7 ms —
    /// deep in timer/allocator/cache noise: 8 runs of the isolated job with
    /// a deliberately reintroduced quadratic `next_line` (before the fix on
    /// `start`, character-by-character `drain`/`remove(0)`) gave 5 honest
    /// failures and 3 false passes, with ratios of 7.7–7.9 against the 8.0
    /// threshold — the test was confusing measurement noise with signal.
    /// Measured separately: the same quadratic mutation at n from 50k to
    /// 400k gives a steady ~4× time per input doubling (not ~2×, as for
    /// linear code) — the signal itself is real, only the measurement's
    /// size was insufficient. At a 30 ms threshold, calibration on this
    /// machine stops at n=400,000 (small ≈ 47–53 ms, large ≈ 195–197 ms,
    /// ratio 3.7–4.16 on linear code across 10 consecutive runs, without a
    /// single miss) — the same order of magnitude as the quadratic
    /// implementation's own numbers (50k/100k/200k lines — 51/225/925 ms).
    /// The linear one over that input range puts the signal well clear of
    /// the noise rather than just touching it. The whole test's cost, on
    /// the same order of fractions of a second, is acceptable precisely
    /// because it does not share a runner with anything else.
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

        // Minimum of three attempts — secondary defense, see the test's
        // doc comment for why this isn't the primary defense.
        fn best_of_three(lines: usize) -> f64 {
            (0..3)
                .map(|_| parse_millis(lines))
                .fold(f64::INFINITY, f64::min)
        }

        // Warm-up: the first run pays for the allocator and cache warming.
        let _ = parse_millis(2_000);

        // Raise the base size while the measurement is drowning in timer
        // resolution: the ratio of two noises means nothing. Calibration
        // uses a single measurement: only a rough order-of-magnitude
        // estimate is needed here, not a fight against noise (that only
        // starts with the real measurement below). The 30 ms threshold
        // (not 1 ms) and the 4,000,000 ceiling (not 64,000) — see the
        // test's doc comment on the misses at the previous threshold.
        let mut n = 50_000;
        while parse_millis(n) < 30.0 && n < 4_000_000 {
            n *= 2;
        }

        let small = best_of_three(n);
        let large = best_of_three(n * 4);

        let ratio = large / small.max(0.001);
        assert!(
            ratio < 8.0,
            "input grew 4×, time grew {ratio:.1}× ({small:.2} ms -> {large:.2} ms \
             at n={n}): looks like O(n^2)"
        );
    }
}
