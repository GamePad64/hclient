//! `Collected::text_with_charset`, and the method it deliberately is not.
//!
//! Every test here is a pair or names its control, because the failure
//! this feature guards against is *plausible text*: a `windows-1251` body
//! read as UTF-8 does not throw, it produces mojibake, and mojibake is
//! only visibly wrong to someone who can read the language.
#![cfg(all(
    feature = "charset",
    feature = "test-util",
    not(target_family = "wasm")
))]

use http_ng::mock::MockTransport;
use http_ng::{CharsetError, Client};

/// "Привет" in windows-1251. Not valid UTF-8 — `0xCF` opens a two-byte
/// sequence and `0xF0` is not a continuation byte — which is what makes
/// the control below fail rather than merely disagree.
const PRIVET_1251: &[u8] = &[0xCF, 0xF0, 0xE8, 0xE2, 0xE5, 0xF2];

fn collected(content_type: Option<&'static str>, body: &'static [u8]) -> http_ng::Collected {
    let t = MockTransport::new();
    let mut b = http::Response::builder().status(200);
    if let Some(ct) = content_type {
        b = b.header(http::header::CONTENT_TYPE, ct);
    }
    t.push_response_bytes(b.body(vec![bytes::Bytes::from_static(body)]).unwrap());
    let c = Client::builder(t).build().expect("build");
    futures_executor::block_on(async { c.get("https://a/x").send().await?.collect().await })
        .expect("the body arrives whatever it says it is")
}

/// **The headline, and its control in the same test.** The same six bytes
/// under the same header: `text_with_charset` reads them, `text` refuses
/// them. Either half alone would be satisfied by a method that ignored the
/// declaration.
#[test]
fn a_declared_windows_1251_body_is_text_where_plain_utf8_refuses_it() {
    let got = collected(Some("text/plain; charset=windows-1251"), PRIVET_1251);
    assert_eq!(got.text_with_charset().expect("declared"), "Привет");
    assert!(
        got.text().is_err(),
        "the control: these bytes are not UTF-8, and `text` must go on \
         saying so whatever this feature does"
    );
}

/// **No declaration means UTF-8**, which is the same answer `text` gives —
/// RFC 7231 removed RFC 2616's ISO-8859-1 default, and this crate does not
/// sniff. Both directions: valid UTF-8 comes back, the 1251 bytes do not.
#[test]
fn without_a_charset_parameter_the_answer_is_utf8_either_way() {
    for ct in [None, Some("text/plain"), Some("text/plain; boundary=x")] {
        let ok = collected(ct, "naïve".as_bytes());
        assert_eq!(ok.text_with_charset().expect("utf-8"), "naïve", "{ct:?}");

        let bad = collected(ct, PRIVET_1251);
        assert!(
            bad.text_with_charset().is_err(),
            "{ct:?}: nothing said windows-1251, so guessing it would be \
             this client inventing an answer"
        );
    }
}

/// **An unknown label is an error naming it, not a silent fall back to
/// UTF-8.** The server said something we did not understand; decoding
/// anyway would turn that into mojibake with nothing to show for it.
#[test]
fn an_unknown_label_is_refused_and_the_error_names_it() {
    let got = collected(Some("text/plain; charset=x-mystery-9"), PRIVET_1251);
    let err = got.text_with_charset().expect_err("no such encoding");
    match std::error::Error::source(&err).and_then(|s| s.downcast_ref::<CharsetError>()) {
        Some(CharsetError::UnknownLabel { label }) => assert_eq!(label, "x-mystery-9"),
        other => panic!("the typed refusal, naming the label: {other:?} / {err:?}"),
    }
}

/// **Malformed bytes are an error, not U+FFFD.** `text` refuses invalid
/// UTF-8 rather than patching it, and this method agrees rather than
/// running a second policy under a similar name.
#[test]
fn malformed_bytes_are_an_error_rather_than_replacement_characters() {
    let got = collected(Some("text/plain; charset=utf-8"), &[b'o', b'k', 0xff]);
    let err = got.text_with_charset().expect_err("0xff is not UTF-8");
    match std::error::Error::source(&err).and_then(|s| s.downcast_ref::<CharsetError>()) {
        Some(CharsetError::Malformed { charset }) => assert_eq!(*charset, "UTF-8"),
        other => panic!("the typed refusal: {other:?} / {err:?}"),
    }
}

/// The parameter is found **past other parameters and past a `;` inside a
/// quoted value**, which is the one place a naive `split(';')` is wrong.
/// The control is the same header with the quotes removed, where the `;`
/// really is a separator and `charset` is still found.
#[test]
fn the_parameter_survives_a_semicolon_inside_a_quoted_value() {
    let quoted = collected(
        Some("text/plain; boundary=\"a;charset=utf-8;b\"; charset=windows-1251"),
        PRIVET_1251,
    );
    assert_eq!(
        quoted.text_with_charset().expect("declared"),
        "Привет",
        "the `charset=utf-8` inside the quoted boundary is not a parameter"
    );

    let unquoted = collected(Some("text/plain; x=1; charset=windows-1251"), PRIVET_1251);
    assert_eq!(unquoted.text_with_charset().expect("declared"), "Привет");

    // And an escaped quote INSIDE the quoted value, which is the case a
    // quote-counting scanner without `\\` handling gets wrong: it would
    // see the escaped `"` as the closing one, treat the following `;` as
    // a separator and read `charset=utf-8` as a parameter. Reachable from
    // a server, so it is a test rather than a mutation control.
    let escaped = collected(
        Some("text/plain; boundary=\"a\\\";charset=utf-8\"; charset=windows-1251"),
        PRIVET_1251,
    );
    assert_eq!(escaped.text_with_charset().expect("declared"), "Привет");
}

/// A quoted label is a label: RFC 9110 §5.6.6 allows either form and
/// servers send both.
#[test]
fn a_quoted_label_is_read_like_a_bare_one() {
    let got = collected(Some("text/plain; charset=\"windows-1251\""), PRIVET_1251);
    assert_eq!(got.text_with_charset().expect("declared"), "Привет");
}

/// **A byte order mark overrides the declaration**, which is the Encoding
/// Standard's rule and every browser's behaviour — and the BOM itself is
/// not part of the text. Inherited from `encoding_rs::Encoding::decode`
/// rather than written here, so it is pinned rather than assumed.
#[test]
fn a_utf8_bom_overrides_the_declared_charset_and_is_not_returned() {
    let got = collected(
        Some("text/plain; charset=windows-1251"),
        &[0xEF, 0xBB, 0xBF, b'o', b'k'],
    );
    assert_eq!(
        got.text_with_charset().expect("the BOM decides"),
        "ok",
        "read as windows-1251 these three bytes would be `п»ї`"
    );
}

/// The label is matched case-insensitively and with the Encoding
/// Standard's own aliases, because that is `for_label`'s job rather than
/// ours — asserted so that a future hand-rolled lookup would fail here.
#[test]
fn labels_are_matched_by_the_encoding_standards_own_rules() {
    for label in ["WINDOWS-1251", "cp1251", "  windows-1251  "] {
        let ct: &'static str = Box::leak(format!("text/plain; charset={label}").into_boxed_str());
        assert_eq!(
            collected(Some(ct), PRIVET_1251)
                .text_with_charset()
                .unwrap_or_else(|e| panic!("{label}: {e:?}")),
            "Привет"
        );
    }
}
