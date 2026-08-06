//! Утверждения о форме публичного API, вынесенные за пределы `src`.
//!
//! Проверка `no-declared-send` в CI сканирует только `crates/*/src`, поэтому
//! обычная генерик-форма здесь не конфликтует с ней, а список исключений
//! сохраняет смысл «обоснованное исключение в продакшн-коде».

use http_ng_core::{Capabilities, Timeouts, UnsupportedCapability};

fn assert_send_sync<T: Send + Sync>() {}

#[test]
fn capability_types_are_send_and_sync() {
    assert_send_sync::<Capabilities>();
    assert_send_sync::<Timeouts>();
    assert_send_sync::<UnsupportedCapability>();
}
