#![no_main]
use hclient_proto::sse::SseDecoder;
use libfuzzer_sys::fuzz_target;

// Invariant (Task 4 re-review, round 1): the decoder must never charge
// fewer bytes against the limit than it actually consumed. `SseDecoder`
// doesn't expose a counter of charged bytes, so the claim "charged >=
// consumed" can't be checked directly through the public API without a
// dedicated accessor — and we chose not to add one just for the fuzzer.
// Undercounting has an externally observable consequence, and that's what's
// checked here: byte accounting must be invariant to how the input is
// chunked. If byte-at-a-time delivery undercounted (as it did before the
// `carried_terminator` fix in lines.rs — an LF swallowed at a chunk
// boundary between CR and LF was never credited anywhere), it could accept
// more on the same bytes than a single-shot delivery would. We check under
// the most adversarial chunking — byte-at-a-time — because that's exactly
// what produces the most CRLF split points.
//
// Broken out into a separate target (round 1 re-review): living inside
// `sse`, this check doubled the push calls per iteration (the
// byte-at-a-time pass plus the single-shot one), and under
// ASan/SanitizerCoverage, growing toward max_len=4096, this collapsed
// throughput from tens of thousands of exec/s down to single digits — the
// fuzzer barely made progress on coverage. Here `-max_len=256` keeps the
// byte-at-a-time pass short, and LIMIT is set low, to 64, so that even
// short inputs can cross the threshold and exercise the code on both sides
// of the limit.
//
// In the byte-at-a-time pass, `break`, not `return`, deliberately:
// `return` would silently skip the comparison with single-shot delivery
// for any input rejected along the way, narrowing the set actually
// checked without warning.
//
// IMPORTANT, about the shape of the comparison itself (found and fixed
// during this round's verification): comparing "did byte-at-a-time ever
// reject ANYWHERE across the whole `data`" against "did single-shot
// delivery ever reject ANYWHERE across the whole `data`" is almost always
// vacuous for a sufficiently long input: an undercounting byte-at-a-time
// decoder will also eventually cross its own (inflated) threshold, just
// later. Because of this, this check's first version silently failed to
// catch the bug it was meant to catch (found by running exactly the
// re-reviewer's construction below — 513 lines of "data:X\r\n" — against
// the reverted fix: the assert didn't fire). The correct comparison isn't
// "did both ever reject," but "do they agree on the SAME prefix": how many
// bytes byte-at-a-time delivery accepted without error (k — either the
// position of the first error, or the whole input if there was no error),
// and what single-shot delivery of THOSE SAME k bytes says. If it rejects
// them while byte-at-a-time accepted them — that's the undercounting.
fuzz_target!(|data: &[u8]| {
    const LIMIT: usize = 64;

    let mut accepted_without_error = data.len();
    {
        let mut d = SseDecoder::new(LIMIT);
        for (i, &b) in data.iter().enumerate() {
            if d.push(&[b]).is_err() {
                accepted_without_error = i; // bytes [0..i) were accepted without errors
                break;
            }
            while d.next().is_some() {}
        }
    }

    let same_prefix_rejected_single_shot = SseDecoder::new(LIMIT)
        .push(&data[..accepted_without_error])
        .is_err();

    assert!(
        !same_prefix_rejected_single_shot,
        "byte-at-a-time delivery accepted a byte range that single-shot delivery \
         of the identical bytes rejects as EventTooLarge — chunk-boundary byte \
         accounting under-counted"
    );
});
