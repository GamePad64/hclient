//! `ClientBuilder::base_url()` — разрешение относительных URI запроса.
//!
//! До этого раунда значение хранилось в `Config` и не читалось НИКЕМ:
//! `client.get("/v1/things")` доезжал до транспорта как URI `/v1/things`, без
//! схемы и authority. Третий случай «сохранили и проигнорировали» в проекте,
//! второй в этой же структуре, и переживший полное ревью всей ветки, которое
//! поймало его близнеца (B1, таймауты клиента).
//!
//! **Семантика — RFC 3986 §5, та же, какой `redirect::decide` разрешает
//! `Location:`.** Один клиент не должен разрешать относительные ссылки двумя
//! разными правилами в зависимости от того, пришли они от вызывающей стороны
//! или из заголовка ответа; общая реализация — `http_ng_proto::uri::
//! resolve_reference`, её же зовёт стадия редиректа.

// `http_ng::mock` живёт за фичей `test-util` (см. `mock.rs`).
#![cfg(feature = "test-util")]

use http_ng::mock::MockTransport;
use http_ng::{Client, ErrorKind, InvalidBaseUrl, RequestBody};

fn client_with_base(base: &str) -> Client<MockTransport> {
    let m = MockTransport::new();
    m.push_response(http::Response::builder().status(200).body("").unwrap());
    Client::builder(m)
        .base_url(base.parse().unwrap())
        .build()
        .unwrap()
}

fn sent_uri(c: &Client<MockTransport>) -> String {
    c.transport().requests()[0].uri.to_string()
}

/// Случай, ради которого настройка существует, и ровно тот, что был тихим
/// no-op: относительная ссылка + база.
#[test]
fn a_relative_request_uri_is_resolved_against_the_base() {
    let c = client_with_base("https://example.test/api/");
    futures_executor::block_on(c.get("v1/things").send()).unwrap();
    assert_eq!(sent_uri(&c), "https://example.test/api/v1/things");
}

/// Абсолютный URI запроса базу игнорирует — RFC 3986 §5.2.2: если у ссылки
/// есть схема, разрешение возвращает её саму.
#[test]
fn an_absolute_request_uri_ignores_the_base() {
    let c = client_with_base("https://example.test/api/");
    futures_executor::block_on(c.get("https://other.test/direct").send()).unwrap();
    assert_eq!(sent_uri(&c), "https://other.test/direct");
}

/// Без базы URI уходит как есть — в том числе относительный. Ничего не
/// выдумываем за вызывающую сторону; транспорт, которому нужен absolute-form,
/// отвергнет его сам и типизированно (`WasiHttp::execute` → `scheme_of`).
#[test]
fn without_a_base_the_uri_reaches_the_transport_unchanged() {
    let m = MockTransport::new();
    m.push_response(http::Response::builder().status(200).body("").unwrap());
    let c = Client::builder(m).build().unwrap();
    futures_executor::block_on(c.get("/v1/things").send()).unwrap();
    assert_eq!(sent_uri(&c), "/v1/things");
}

/// Острый угол RFC 3986 и единственная неочевидная часть правила: ссылка,
/// начинающаяся со `/`, заменяет ВЕСЬ путь базы, а не дописывается к нему.
/// Закрепляем оба варианта одним тестом, чтобы «починка» одного не прошла
/// незамеченной.
#[test]
fn an_absolute_path_replaces_the_bases_path_while_a_relative_one_extends_it() {
    let c = client_with_base("https://example.test/api/");
    futures_executor::block_on(c.get("/v1/things").send()).unwrap();
    assert_eq!(
        sent_uri(&c),
        "https://example.test/v1/things",
        "ведущий / заменяет путь базы (RFC 3986 §5.2.2), а не дописывается к нему"
    );

    let c2 = client_with_base("https://example.test/api/");
    futures_executor::block_on(c2.get("v1/things").send()).unwrap();
    assert_eq!(sent_uri(&c2), "https://example.test/api/v1/things");
}

/// Вторая половина того же острого угла: база БЕЗ завершающего слэша теряет
/// свой последний сегмент при разрешении относительной ссылки — это merge из
/// RFC 3986 §5.3, а не наша самодеятельность. Задокументировано у сеттера;
/// тест держит документацию честной.
#[test]
fn a_base_without_a_trailing_slash_drops_its_last_segment() {
    let c = client_with_base("https://example.test/api");
    futures_executor::block_on(c.get("v1/things").send()).unwrap();
    assert_eq!(
        sent_uri(&c),
        "https://example.test/v1/things",
        "`/api` без слэша — не каталог: RFC 3986 §5.3 отбрасывает последний сегмент"
    );
}

/// Запрос без пути вообще: база должна доехать целиком, а не превратиться в
/// свой корень.
#[test]
fn an_empty_reference_resolves_to_the_base_itself() {
    let c = client_with_base("https://example.test/api/things");
    futures_executor::block_on(c.get("").send()).unwrap();
    assert_eq!(sent_uri(&c), "https://example.test/api/things");
}

/// Query у ссылки переживает разрешение.
#[test]
fn the_query_of_a_relative_reference_survives() {
    let c = client_with_base("https://example.test/api/");
    futures_executor::block_on(c.get("search?q=1&n=2").send()).unwrap();
    assert_eq!(sent_uri(&c), "https://example.test/api/search?q=1&n=2");
}

/// База сама обязана быть абсолютной: относительная разрешать не от чего.
/// Это типизированная ошибка с именуемым источником, а не тихо
/// проигнорированная настройка — то есть тот же контракт, что у
/// неподдерживаемого таймаута (M3).
#[test]
fn a_relative_base_is_a_typed_error_not_a_silently_ignored_setting() {
    let c = client_with_base("/api/");
    let err = futures_executor::block_on(c.get("v1/things").send())
        .expect_err("относительная база непригодна — это обязано быть ошибкой");

    assert_eq!(*err.kind(), ErrorKind::Other, "{err}");
    let src = std::error::Error::source(&err).expect("Error::new всегда кладёт source");
    let bad = src
        .downcast_ref::<InvalidBaseUrl>()
        .expect("источник обязан называть саму проблему, а не быть строкой");
    assert_eq!(bad.base, "/api/".parse::<http::Uri>().unwrap());
    assert_eq!(bad.requested, "v1/things");
    assert!(
        c.transport().requests().is_empty(),
        "запрос с непригодной базой не должен доходить до транспорта"
    );
}

/// `Response::url()` обязан называть URL, по которому запрос реально ушёл, а
/// не то, что напечатала вызывающая сторона. Иначе разрешение базы было бы
/// видно транспорту и невидимо потребителю.
#[test]
fn response_url_reports_the_resolved_uri_not_the_relative_one() {
    let c = client_with_base("https://example.test/api/");
    let resp = futures_executor::block_on(c.get("v1/things").send()).unwrap();
    assert_eq!(
        resp.url(),
        &"https://example.test/api/v1/things"
            .parse::<http::Uri>()
            .unwrap()
    );
}

/// `Client::execute` — публичный вход, принимающий готовый `http::Request`;
/// база обязана применяться и здесь, иначе настройка работает только через
/// `RequestBuilder` и снова частично.
///
/// Ссылка здесь `/v1/things`, а не `v1/things`, и это не выбор теста:
/// `http::Request::builder().uri("v1/things")` не собирается вовсе —
/// `http::Uri` не представляет path-relative ссылку. Через этот вход
/// выразимы только origin-form и absolute-form, поэтому база может дать
/// такому запросу схему и authority, но не путь. Ровно то же самое сказал бы
/// RFC 3986 §5.2.2 про любую ссылку с ведущим `/`.
#[test]
fn client_execute_resolves_the_base_too_not_only_request_builder() {
    let c = client_with_base("https://example.test/api/");
    let req = http::Request::builder()
        .uri("/v1/things")
        .body(RequestBody::Empty)
        .unwrap();
    futures_executor::block_on(c.execute(req)).unwrap();
    assert_eq!(sent_uri(&c), "https://example.test/v1/things");
}

/// Обратная сторона предыдущего: путь-относительную ссылку `http::Uri` не
/// представляет, поэтому единственный способ ею воспользоваться —
/// `RequestBuilder`, который разрешает исходную СТРОКУ до разбора. Тест
/// закрепляет, что ограничение именно такое, а не «`get` тоже не умеет».
#[test]
fn a_path_relative_reference_is_expressible_through_the_builder_only() {
    assert!(
        "v1/things".parse::<http::Uri>().is_err(),
        "если http::Uri научится этой форме, разрешение можно будет унифицировать на Uri"
    );
    let c = client_with_base("https://example.test/api/");
    futures_executor::block_on(c.get("v1/things").send()).unwrap();
    assert_eq!(sent_uri(&c), "https://example.test/api/v1/things");
}
