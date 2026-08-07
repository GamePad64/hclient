//! `Native<R, T, D>: Transport` — интеграционные тесты этого крейта против
//! `http_ng::Client`, не только против `crate::testing::*` напрямую: то, что
//! проверяют тесты ниже (категория ошибки, честность `Capabilities`,
//! проверка таймаутов на `build()`) — свойства ШВА `Client::execute` ↔
//! `Transport`, а не одной функции этого крейта, так что и проверяются они
//! через реальный `Client`.
//!
//! # Почему тут нет `filter_map`-резолюции из брифа задачи
//!
//! Черновик задачи (`task-13-brief.md`) собирал адреса вручную —
//! `self.dns.lookup_ipv6(&host).filter_map(|r| async { r.ok()... })` — что
//! отбрасывает ЛЮБУЮ ошибку резолвера (`ErrorKind::Cancelled` включительно)
//! и синтезирует единый `ErrorKind::Resolve`, если оба стрима пусты. Review
//! Task 7 уже поймал это ровно на этом месте: `Cancelled` (обычное
//! завершение рантайма) неотличимо от «имя не резолвится», а
//! circuit-breaker, ключующийся на `Resolve`, ошибочно занёс бы живой хост в
//! чёрный список во время обычного shutdown.
//!
//! Task 11 решила это ОДИН РАЗ, структурно, в `connect::drive`/
//! `ResolveErrors::distinguishing_error` — она проверяется ДО обеих веток
//! отказа, так что отбрасывание кода ошибки, отличного от синтетического
//! `Resolve`, структурно недостижимо. `Native::execute` (`src/lib.rs`)
//! поэтому не резолвит сам: он вызывает `connect::connect`, ту же самую
//! точку входа, которую уже гоняют юнит-тесты `connect.rs` — и
//! `resolver_cancelled_error_reaches_the_caller_through_execute_not_flattened`
//! ниже проверяет, что это свойство доживает до ВЕСЬ путь: от
//! `Resolve::lookup_ipv4`/`lookup_ipv6`, через `Native::execute`, через
//! `Client::execute` (у которого есть собственный шаг `.map_err(|e|
//! self.transport.to_error(e))`), и до `kind()`, который видит вызывающая
//! сторона.
mod net_fixtures;

use http_ng::Client;
use http_ng_core::ErrorKind;
use http_ng_core::unversioned::Transport;
use http_ng_dns::{Resolve, ResolvedAddr};
use http_ng_dns_system::SystemDns;
use http_ng_native::Native;
use http_ng_rt_tokio::Tokio;
use http_ng_tls_rustls::Rustls;
use std::io::{Read, Write};
use std::time::Duration;

const BOUND: Duration = Duration::from_secs(30);

fn spawn_h1_server() -> std::net::SocketAddr {
    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = l.local_addr().unwrap();
    std::thread::spawn(move || {
        for s in l.incoming() {
            let Ok(mut s) = s else { continue };
            let mut b = [0u8; 2048];
            let _ = s.read(&mut b);
            let _ = s.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok");
        }
    });
    addr
}

#[tokio::test]
async fn end_to_end_over_plain_tcp() {
    let addr = spawn_h1_server();
    let t = Native::new(Tokio, Rustls::with_webpki_roots(), SystemDns::new(Tokio));
    let c = Client::builder(t).build().unwrap();
    let resp = tokio::time::timeout(BOUND, c.get(&format!("http://{addr}/")).send())
        .await
        .expect("must not hang")
        .unwrap();
    assert_eq!(resp.status(), 200);
    assert_eq!(resp.collect().await.unwrap().text().unwrap(), "ok");
}

/// `end_to_end_over_plain_tcp` proves a response comes back, but the
/// fixture server above answers with the same fixed bytes no matter what it
/// receives — a mutation that dropped `Native`'s origin-form/`Host:`
/// rewrite entirely (leaving the absolute-form URI and no `Host:` header)
/// would still pass it, because nothing checks what was actually sent on
/// the wire. This test captures the raw request bytes and checks the
/// request-line and `Host:` header directly, so that rewrite has real
/// coverage.
#[tokio::test]
async fn request_line_is_origin_form_and_host_header_is_set() {
    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = l.local_addr().unwrap();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        if let Ok((mut s, _)) = l.accept() {
            let mut buf = [0u8; 4096];
            let n = s.read(&mut buf).unwrap_or(0);
            let _ = tx.send(buf[..n].to_vec());
            let _ = s.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok");
        }
    });
    let t = Native::new(Tokio, Rustls::with_webpki_roots(), SystemDns::new(Tokio));
    let c = Client::builder(t).build().unwrap();
    let resp = tokio::time::timeout(BOUND, c.get(&format!("http://{addr}/hello")).send())
        .await
        .expect("must not hang")
        .unwrap();
    assert_eq!(resp.status(), 200);

    let raw = rx
        .recv_timeout(BOUND)
        .expect("server must have seen a request");
    let text = String::from_utf8_lossy(&raw);
    let request_line = text.lines().next().unwrap_or_default();
    assert_eq!(
        request_line, "GET /hello HTTP/1.1",
        "request-line must be origin-form (path only), not absolute-form: {text:?}"
    );
    // hyper's h1 writer lowercases header names on the wire by default
    // (no `http1_preserve_header_case`), so the comparison below is
    // case-insensitive on the header name — the VALUE (the authority) is
    // what this test actually cares about.
    let host_line = text
        .lines()
        .find(|l| l.to_ascii_lowercase().starts_with("host:"))
        .unwrap_or_else(|| panic!("no Host: header sent: {text:?}"));
    assert_eq!(
        host_line.trim().to_ascii_lowercase(),
        format!("host: {addr}"),
        "Host: header must name the authority actually connected to: {text:?}"
    );
}

#[tokio::test]
async fn capabilities_are_honest_about_v01_limits() {
    let t = Native::new(Tokio, Rustls::with_webpki_roots(), SystemDns::new(Tokio));
    let caps = t.capabilities();
    assert!(!caps.streaming_request_body, "в v0.1 тело буферизовано");
    assert!(caps.timeouts.connect);
    assert!(
        !caps.timeouts.first_byte,
        "нет пула и таймера ответа — заявлять нельзя"
    );
    assert!(
        !caps.timeouts.between_bytes,
        "нет таймера ответа — заявлять нельзя"
    );
    assert_eq!(caps.upgrade, http_ng_core::UpgradeSupport::None);
    assert_eq!(caps.tls_config, http_ng_core::TlsSupport::Full);
    assert_eq!(caps.redirects, http_ng_core::RedirectSupport::Configurable);
    assert!(caps.version_reported);
}

/// `capabilities_are_honest_about_v01_limits` above only samples the fields
/// the brief calls out. This one destructures the rest of the struct so a
/// field nobody asked `Native` to turn on can't silently end up
/// `true`/non-`None`.
///
/// **What this test cannot do, despite what an earlier name implied.** Unlike
/// `Capabilities::none_is_the_conservative_base` in `http-ng-core` (which
/// lives inside that crate and can destructure with no `..` rest pattern, so
/// a brand-new field is a compile error naming it), this file is an
/// *external* crate — `Capabilities` is `#[non_exhaustive]`, so `..` is
/// mandatory here (E0638), and that `..` silently absorbs any field added
/// later. A reviewer built exactly this scenario (added a seventeenth field
/// to `Capabilities`): `http-ng-core`'s own internal test failed to compile
/// as designed, and this test compiled and passed without noticing.
/// Renamed from `every_undeclared_capability_stays_at_the_conservative_default`
/// to stop promising that it does — it checks that the fields enumerated
/// below are at their conservative defaults *today*, nothing more. The
/// exhaustiveness guarantee (a new field becomes a compile error, not a
/// silent pass) exists exactly once, inside `http-ng-core`'s own
/// `Capabilities::none_is_the_conservative_base` — `#[non_exhaustive]` makes
/// that guarantee structurally unavailable to any test outside the crate
/// that owns the type, this one included. Any future capabilities-
/// completeness check belongs there, not in a transport crate like this one.
/// (Note for whoever wires this to a spec citation later: this is *not* the
/// same thing as design-doc amendment C3, which is specifically about where
/// `Send`/`Sync` assertions live so `no-declared-send`'s `src`-only grep
/// doesn't trip on its own test text — a different rule that happens to
/// share the "belongs in `tests/`, not here" shape.)
#[tokio::test]
async fn undeclared_capability_fields_match_their_conservative_defaults_today() {
    let t = Native::new(Tokio, Rustls::with_webpki_roots(), SystemDns::new(Tokio));
    let http_ng_core::Capabilities {
        streaming_request_body: _,
        full_duplex,
        request_trailers,
        response_trailers,
        redirects: _,
        tls_config: _,
        client_certs,
        proxy,
        owns_cookie_jar,
        owns_cache,
        version_select,
        version_reported: _,
        timeouts: _,
        informational_1xx,
        upgrade: _,
        forbidden_request_headers,
        ..
    } = *t.capabilities();
    assert!(!full_duplex);
    assert!(!request_trailers);
    assert!(!response_trailers);
    assert!(!client_certs);
    assert!(!proxy);
    assert!(!owns_cookie_jar);
    assert!(!owns_cache);
    assert!(!version_select);
    assert!(!informational_1xx);
    assert!(forbidden_request_headers.is_empty());
}

/// Категория, которую проставил `Native`, обязана дожить до вызывающей
/// стороны через весь путь `Client::execute` (см. doc-комментарий модуля).
/// Тест проверяет именно этот путь целиком, а не дефолт `to_error` — тот
/// проверен в `http-ng-core/tests/shape.rs` и сам по себе гарантирует
/// пропуск `Error` насквозь.
///
/// Хост выбран несуществующим намеренно (`.invalid`, RFC 2606 — гарантированно
/// никогда не резолвится): это единственный отказ, который `execute`
/// производит без сети и без сервера, и `wasi`-аналог этого теста устроен
/// так же — гоняет реальный классификатор бэкенда, а не сконструированную
/// вручную `Error`.
#[tokio::test]
async fn transport_error_kind_survives_the_client_instead_of_flattening_to_other() {
    let t = Native::new(Tokio, Rustls::with_webpki_roots(), SystemDns::new(Tokio));
    let c = Client::builder(t).build().unwrap();
    let err = tokio::time::timeout(BOUND, c.get("http://nonexistent.invalid/").send())
        .await
        .expect("must not hang")
        .unwrap_err();

    assert_eq!(
        *err.kind(),
        ErrorKind::Resolve,
        "категория обязана дожить до вызывающей стороны, а не расплющиться в Other: {err}"
    );
    assert!(
        !err.to_string().starts_with("Other:"),
        "категория печатается один раз, и это настоящая категория: {err}"
    );
}

/// Резолвер, который всегда отдаёт `ErrorKind::Cancelled` — синтетическая
/// замена реального завершения пула фоновых потоков (Task 7), достаточная,
/// чтобы проверить, что `Native::execute` не заворачивает и не расплющивает
/// её по дороге к `Client`.
struct CancelledDns;

#[derive(Debug)]
struct FakeCancelled;
impl std::fmt::Display for FakeCancelled {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("resolver background pool went away")
    }
}
impl std::error::Error for FakeCancelled {}

impl Resolve for CancelledDns {
    fn lookup_ipv4(
        &self,
        _name: &str,
    ) -> impl futures_util::Stream<Item = Result<ResolvedAddr, http_ng_core::Error>> {
        futures_util::stream::once(async {
            Err(http_ng_core::Error::new(
                ErrorKind::Cancelled,
                FakeCancelled,
            ))
        })
    }
    fn lookup_ipv6(
        &self,
        _name: &str,
    ) -> impl futures_util::Stream<Item = Result<ResolvedAddr, http_ng_core::Error>> {
        futures_util::stream::once(async {
            Err(http_ng_core::Error::new(
                ErrorKind::Cancelled,
                FakeCancelled,
            ))
        })
    }
}

/// Constraint from this task's brief: "an `ErrorKind::Cancelled` from the
/// resolver must reach the caller as `Cancelled` through `Transport::execute`,
/// and a mutation that flattens it must go red." This is that test, run
/// through the real `Client::execute` seam (not just `connect::drive`'s own
/// unit tests in `src/connect.rs`, which cover the same property one layer
/// lower) — see the module doc for why `Native::execute` must not re-derive
/// resolution itself.
#[tokio::test]
async fn resolver_cancelled_error_reaches_the_caller_through_execute_not_flattened() {
    let t = Native::new(Tokio, Rustls::with_webpki_roots(), CancelledDns);
    let c = Client::builder(t).build().unwrap();
    let err = tokio::time::timeout(BOUND, c.get("http://example.invalid/").send())
        .await
        .expect("must not hang")
        .unwrap_err();
    assert_eq!(
        *err.kind(),
        ErrorKind::Cancelled,
        "рантайм завершает работу — это не 'имя не резолвится': {err}"
    );
}

#[tokio::test]
async fn unsupported_timeout_is_rejected_at_build_time() {
    let t = Native::new(Tokio, Rustls::with_webpki_roots(), SystemDns::new(Tokio));
    let err = Client::builder(t)
        .timeouts(http_ng::Timeouts {
            between_bytes: Some(Duration::from_secs(1)),
            ..Default::default()
        })
        .build()
        .unwrap_err();
    assert_eq!(err.what, "between_bytes_timeout");
}

/// TLS-хендшейк, который отказывает (сервер принял TCP-соединение и сразу
/// его уронил, не сказав ни байта TLS) — проверяет, что категория `Tls`
/// (`TlsConnect::connect`, Task 8/9) тоже доживает до вызывающей стороны
/// через `Native::execute`, не только `Resolve`/`Cancelled` выше.
#[tokio::test]
async fn tls_handshake_failure_reports_tls_kind_through_the_client() {
    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = l.local_addr().unwrap();
    std::thread::spawn(move || {
        // Принимает и сразу роняет — ни байта TLS не отправлено.
        let _ = l.accept();
    });
    let t = Native::new(Tokio, Rustls::with_webpki_roots(), SystemDns::new(Tokio));
    let c = Client::builder(t).build().unwrap();
    let err = tokio::time::timeout(
        BOUND,
        c.get(&format!("https://{}:{}/", addr.ip(), addr.port()))
            .send(),
    )
    .await
    .expect("must not hang")
    .unwrap_err();
    assert_eq!(*err.kind(), ErrorKind::Tls, "{err}");
}

// --- The remaining two of the brief's five `ErrorKind` fidelity properties ---
//
// The tests above cover `Resolve`, `Cancelled` and `Tls` surviving
// `Client::execute`. `Connect` and `Body` were the other two the brief
// named, and fix round 1's review found neither had a test of its own at
// this composed layer — both pass on unmodified code (no bug), but nothing
// held that property in place: wrapping `h1::exchange`'s error in a fresh
// `Error::new(ErrorKind::Connect, e)` in `src/lib.rs` turns the `Body` test
// below red while every other test in this file stays green (verified
// directly, see this task's report). The two tests below close that gap.

/// `ErrorKind::Connect` (`connect::connect`'s own `AllAttemptsFailed`,
/// `ErrorKind::Connect` — Task 11) surviving `Client::execute`, the same
/// property `Resolve`/`Cancelled`/`Tls` already have tests for above.
/// `net_fixtures::closed_port` (not a hand-rolled bind-then-drop) reuses the
/// helper this crate already has for "a port that genuinely refuses" — see
/// its doc comment for why a hand-rolled version is a trap worth avoiding a
/// second time, and why a supposedly-unroutable *address* would not do here
/// (this container's `tun0` makes those connect successfully; a closed
/// *local* port still gets a real `ECONNREFUSED` from the kernel regardless).
#[tokio::test]
async fn connect_refused_kind_survives_the_client() {
    let addr = net_fixtures::closed_port();
    let t = Native::new(Tokio, Rustls::with_webpki_roots(), SystemDns::new(Tokio));
    let c = Client::builder(t).build().unwrap();
    let err = tokio::time::timeout(BOUND, c.get(&format!("http://{addr}/")).send())
        .await
        .expect("must not hang")
        .unwrap_err();
    assert_eq!(*err.kind(), ErrorKind::Connect, "{err}");
}

/// A one-shot `RequestBody::Streaming` whose only frame is an error —
/// exercises the same "outgoing body fails mid-stream" shape `h1.rs`'s own
/// unit test already proves one layer down
/// (`exchange_recovers_error_kind_through_hyper_error_not_flattening_it`,
/// which calls `h1::exchange` directly), but through the full
/// `Native::execute` + `Client::execute` composition instead.
struct OneShotErrBody(Option<http_ng_core::Error>);
impl http_body::Body for OneShotErrBody {
    type Data = bytes::Bytes;
    type Error = http_ng_core::Error;
    fn poll_frame(
        mut self: std::pin::Pin<&mut Self>,
        _cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Result<http_body::Frame<bytes::Bytes>, http_ng_core::Error>>> {
        std::task::Poll::Ready(self.0.take().map(Err))
    }
    fn is_end_stream(&self) -> bool {
        self.0.is_none()
    }
}

/// `ErrorKind::Body` (the outgoing request body itself failing, not the
/// response) surviving `Client::execute` — see the module-level comment
/// above for the mutation that proves this test is load-bearing, not
/// decorative.
#[tokio::test]
async fn streaming_request_body_error_kind_survives_the_client() {
    let addr = spawn_h1_server();
    let t = Native::new(Tokio, Rustls::with_webpki_roots(), SystemDns::new(Tokio));
    let c = Client::builder(t).build().unwrap();
    let body = http_ng_core::RequestBody::Streaming(Box::new(OneShotErrBody(Some(
        http_ng_core::Error::new(ErrorKind::Body, std::io::Error::other("stream broke")),
    ))));
    let err = tokio::time::timeout(BOUND, c.post(&format!("http://{addr}/")).body(body).send())
        .await
        .expect("must not hang")
        .unwrap_err();
    assert_eq!(*err.kind(), ErrorKind::Body, "{err}");
}
