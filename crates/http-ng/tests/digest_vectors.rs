//! The RFC's own numbers, and the parser's corners.
//!
//! Every `response=` here is **RFC 7616 §3.9's**, copied from the document
//! rather than from a run of this code — which is the whole point of
//! `digest::answer` taking `cnonce` as a parameter instead of drawing it:
//! a hash function checked against its own output is green for any
//! self-consistent mistake about what digest is.
#![cfg(feature = "digest-auth")]

use http_ng::digest::{Algorithm, Challenge, answer, best_challenge};

fn hv(s: &str) -> http::HeaderValue {
    http::HeaderValue::from_str(s).expect("header value")
}

/// **RFC 7616 §3.9.1, the SHA-256 example**, verbatim.
///
/// The document's own inputs: `Mufasa`/`Circle of Life`, realm
/// `http-auth@example.org`, `GET /dir/index.html`, and the nonce and
/// cnonce it prints. If this line changes, either the hashing or the
/// concatenation is wrong, and no test of ours agreeing with itself would
/// say so.
#[test]
fn the_rfcs_own_sha256_example_reproduces() {
    let c = Challenge {
        realm: "http-auth@example.org".into(),
        nonce: "7ypf/xlj9XXwfDPEoM4URrv/xwf94BcCAzFZH4GiTo0v".into(),
        algorithm: Algorithm::Sha256,
        opaque: Some("FQhe/qaU925kfnzjCev0ciny7QMkPqMAFRtzCUYo5tdS".into()),
        qop_auth: true,
        only_auth_int: false,
        stale: false,
    };
    let got = answer(
        &c,
        "Mufasa",
        "Circle of Life",
        &http::Method::GET,
        "/dir/index.html",
        "f2/wE4q74E6zIJEtWaHKaf5wv/H5QzzpXusqGemxURZJ",
    );
    assert!(
        got.contains(
            "response=\"753927fa0e85d155564e2e272a28d1802ca10daf4496794697cf8db5856cb6c1\""
        ),
        "RFC 7616 §3.9.1's own answer: {got}"
    );
    assert!(got.contains("algorithm=SHA-256"), "{got}");
    assert!(got.contains("qop=auth"), "{got}");
    assert!(got.contains("nc=00000001"), "{got}");
    assert!(
        got.contains("opaque=\"FQhe/qaU925kfnzjCev0ciny7QMkPqMAFRtzCUYo5tdS\""),
        "the server's own state goes back unchanged: {got}"
    );
}

/// **The same section's MD5 example.** Same inputs, same everything except
/// the algorithm — which is what makes the pair evidence that the
/// algorithm is *used* rather than merely echoed in the header.
#[test]
fn the_rfcs_own_md5_example_reproduces() {
    let c = Challenge {
        realm: "http-auth@example.org".into(),
        nonce: "7ypf/xlj9XXwfDPEoM4URrv/xwf94BcCAzFZH4GiTo0v".into(),
        algorithm: Algorithm::Md5,
        opaque: Some("FQhe/qaU925kfnzjCev0ciny7QMkPqMAFRtzCUYo5tdS".into()),
        qop_auth: true,
        only_auth_int: false,
        stale: false,
    };
    let got = answer(
        &c,
        "Mufasa",
        "Circle of Life",
        &http::Method::GET,
        "/dir/index.html",
        "f2/wE4q74E6zIJEtWaHKaf5wv/H5QzzpXusqGemxURZJ",
    );
    assert!(
        got.contains("response=\"8ca523f5e9506fed4657c9700eebdbec\""),
        "RFC 7616 §3.9.1's MD5 answer: {got}"
    );
    assert!(got.contains("algorithm=MD5"), "{got}");
}

/// **RFC 2069's form**, which §3.4.1 keeps for a server that sends no
/// `qop`: the response folds only `HA1:nonce:HA2`, and neither `nc` nor
/// `cnonce` goes back. Asserted because the two formulas differ by more
/// than a field, and a client that always used the `qop` one would be
/// rejected by exactly the old servers this exists for.
#[test]
fn a_challenge_without_qop_takes_the_rfc_2069_shape() {
    let c = Challenge {
        realm: "testrealm@host.com".into(),
        nonce: "dcd98b7102dd2f0e8b11d0f600bfb0c093".into(),
        algorithm: Algorithm::Md5,
        opaque: None,
        qop_auth: false,
        only_auth_int: false,
        stale: false,
    };
    let got = answer(
        &c,
        "Mufasa",
        "Circle Of Life",
        &http::Method::GET,
        "/dir/index.html",
        "unused",
    );
    // RFC 2617 §3.5's own example, which is the same computation.
    assert!(
        got.contains("response=\"670fd8c2df070c60b045671b8b24ff02\""),
        "{got}"
    );
    assert!(!got.contains("qop="), "no qop was offered: {got}");
    assert!(!got.contains("nc="), "{got}");
    assert!(!got.contains("cnonce="), "{got}");
}

/// **The strongest challenge wins, across header lines.** A server sending
/// SHA-256 and MD5 as two `WWW-Authenticate` values is the ordinary case,
/// and a client taking the first would answer MD5 to a server that offered
/// better. Both orders are tried, or the test would pass for a client that
/// simply took the last.
#[test]
fn the_strongest_offered_algorithm_is_the_one_answered() {
    let md5 = hv("Digest realm=\"r\", nonce=\"n\", algorithm=MD5, qop=\"auth\"");
    let sha = hv("Digest realm=\"r\", nonce=\"n\", algorithm=SHA-256, qop=\"auth\"");
    for order in [vec![&md5, &sha], vec![&sha, &md5]] {
        let c = best_challenge(order.into_iter()).expect("one of them is usable");
        assert_eq!(c.algorithm, Algorithm::Sha256);
    }
}

/// A `Basic` challenge beside a `Digest` one is ignored rather than
/// fatal — a different scheme is not a malformed digest — and an unknown
/// *algorithm* is skipped so a weaker challenge beside it can still win.
#[test]
fn other_schemes_and_unknown_algorithms_are_stepped_over() {
    let v = hv(
        "Basic realm=\"b\", Digest realm=\"r\", nonce=\"n\", algorithm=SHA3-512, \
         Digest realm=\"r\", nonce=\"n\", algorithm=MD5",
    );
    let c = best_challenge([&v].into_iter()).expect("the MD5 one is answerable");
    assert_eq!(c.algorithm, Algorithm::Md5);
}

/// **A comma inside a quoted value does not start a new challenge**, which
/// is the one place a naive split is wrong: `realm="a, b"` is one realm.
#[test]
fn a_comma_inside_a_quoted_realm_is_not_a_separator() {
    let v = hv("Digest realm=\"a, b\", nonce=\"n\", qop=\"auth\"");
    let c = best_challenge([&v].into_iter()).expect("one challenge");
    assert_eq!(c.realm, "a, b");
    assert!(c.qop_auth);
}

/// An escaped quote inside a realm survives both ways: unescaped on the
/// way in, re-escaped on the way out. A realm is free text a deployment
/// chooses, so this is reachable rather than contrived — and a bare `"` in
/// the `Authorization` value would end the field and let the rest be read
/// as further parameters.
#[test]
fn an_escaped_quote_in_a_realm_survives_the_round_trip() {
    let v = hv("Digest realm=\"a\\\"b\", nonce=\"n\", algorithm=MD5, qop=\"auth\"");
    let c = best_challenge([&v].into_iter()).expect("one challenge");
    assert_eq!(c.realm, "a\"b", "unescaped on the way in");
    let got = answer(&c, "u", "p", &http::Method::GET, "/", "cn");
    assert!(
        got.contains("realm=\"a\\\"b\""),
        "and re-escaped on the way out: {got}"
    );
}

/// **`auth-int` alone is a named refusal, not a wrong answer.** Computing
/// `auth` where the server asked for `auth-int` yields a `401` a caller
/// cannot diagnose; refusing says which of the two it was.
#[test]
fn auth_int_alone_is_refused_by_name() {
    let v = hv("Digest realm=\"r\", nonce=\"n\", qop=\"auth-int\"");
    assert_eq!(
        best_challenge([&v].into_iter()),
        Err(http_ng::digest::DigestError::AuthIntUnsupported)
    );

    // The control: offered beside `auth`, it costs nothing.
    let v = hv("Digest realm=\"r\", nonce=\"n\", qop=\"auth,auth-int\"");
    let c = best_challenge([&v].into_iter()).expect("auth is on offer");
    assert!(c.qop_auth);
    assert!(!c.only_auth_int);
}

/// A challenge missing a required parameter names it. Both are checked,
/// because "the error is produced" and "the error says which" are
/// different claims and only the second is useful at 3am.
#[test]
fn a_missing_required_parameter_is_named() {
    use http_ng::digest::DigestError::MissingParameter;
    for (v, want) in [
        ("Digest nonce=\"n\"", "realm"),
        ("Digest realm=\"r\"", "nonce"),
    ] {
        let v = hv(v);
        assert_eq!(
            best_challenge([&v].into_iter()),
            Err(MissingParameter { parameter: want })
        );
    }
}

/// **A malformed challenge beside a usable one must not hide it**, which
/// is why the parse error is remembered rather than returned.
#[test]
fn a_broken_challenge_does_not_hide_a_usable_one() {
    let broken = hv("Digest realm=\"r\"");
    let good = hv("Digest realm=\"r\", nonce=\"n\", algorithm=MD5");
    let c = best_challenge([&broken, &good].into_iter()).expect("the second one works");
    assert_eq!(c.nonce, "n");
}

/// An absent `algorithm` is MD5, RFC 7616 §3.3 — and `stale=true` is read,
/// since it is what tells an expired nonce from a wrong password.
#[test]
fn an_absent_algorithm_is_md5_and_stale_is_read() {
    let v = hv("Digest realm=\"r\", nonce=\"n\", stale=TRUE");
    let c = best_challenge([&v].into_iter()).expect("usable");
    assert_eq!(c.algorithm, Algorithm::Md5);
    assert!(c.stale, "matched case-insensitively, as servers spell it");

    let v = hv("Digest realm=\"r\", nonce=\"n\"");
    assert!(!best_challenge([&v].into_iter()).expect("usable").stale);
}
