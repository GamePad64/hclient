#![no_main]
use http_ng_proto::sse::SseDecoder;
use libfuzzer_sys::fuzz_target;

// Invariant: the decoder never panics and never grows past the limit.
fuzz_target!(|data: &[u8]| {
    const LIMIT: usize = 4096;
    let mut d = SseDecoder::new(LIMIT);
    for chunk in data.chunks(7) {
        if d.push(chunk).is_err() {
            return; // EventTooLarge — a legal terminal outcome
        }
        while d.next().is_some() {}
    }
});
