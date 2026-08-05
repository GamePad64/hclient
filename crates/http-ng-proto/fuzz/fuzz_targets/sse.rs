#![no_main]
use http_ng_proto::sse::SseDecoder;
use libfuzzer_sys::fuzz_target;

// Инвариант из брифа: декодер никогда не паникует и никогда не растёт сверх
// лимита (EventTooLarge — легальный терминальный исход).
fuzz_target!(|data: &[u8]| {
    const LIMIT: usize = 4096;

    let mut d = SseDecoder::new(LIMIT);
    for chunk in data.chunks(7) {
        if d.push(chunk).is_err() {
            break; // EventTooLarge — легальный терминальный исход
        }
        while d.next().is_some() {}
    }

    // Дополнительный инвариант (Task 4 ре-ревью): декодер никогда не должен
    // заряжать в счёт лимита меньше байт, чем реально потребил. `SseDecoder`
    // не экспонирует счётчик заряженных байт, так что тезис "charged >=
    // consumed" не проверить напрямую через паблик API без выделенного
    // аксессора — а заводить его ради фаззера мы не стали. У недоучёта есть
    // наблюдаемое снаружи следствие, и оно проверяется здесь: учёт байт
    // обязан быть инвариантен к дроблению входа на чанки. Если бы побайтовая
    // доставка занижала счёт (как было до фикса `carried_terminator` в
    // lines.rs — LF, проглоченный на границе чанка между CR и LF, нигде не
    // начислялся), она могла бы принять поток, который одноразовая подача
    // того же входа отвергла бы как EventTooLarge. Проверяем под самым
    // враждебным дроблением — побайтовым, — потому что именно оно даёт
    // максимум точек разрыва CRLF.
    let byte_at_a_time_rejected = {
        let mut d = SseDecoder::new(LIMIT);
        let mut rejected = false;
        for &b in data {
            if d.push(&[b]).is_err() {
                rejected = true;
                break;
            }
            while d.next().is_some() {}
        }
        rejected
    };
    let single_shot_rejected = SseDecoder::new(LIMIT).push(data).is_err();

    assert!(
        !(single_shot_rejected && !byte_at_a_time_rejected),
        "byte-at-a-time delivery accepted an event that single-shot delivery \
         rejected as EventTooLarge — chunk-boundary byte accounting under-counted"
    );
});
