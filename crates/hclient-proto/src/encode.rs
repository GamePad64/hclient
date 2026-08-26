//! The two encodings HTTP asks for, both taken rather than written.
//!
//! Both are **encode only**: nothing in this workspace decodes either, and
//! a decoder is where the sharp edges live.
//!
//! # `form_urlencoded` does not bring `url` back, and this file said it did
//!
//! For two verticals this module carried twenty lines of WHATWG serialiser
//! under the claim that the crate *"would bring `url` straight back"* —
//! the crate `uri.rs` was rewritten to remove, at the cost of a 96-pair
//! differential corpus, because it reached `idna` and the ICU tables.
//!
//! **Measured, and false.** `form_urlencoded` is its own crate and depends
//! on `percent-encoding` alone: two crates, no `url`, no `idna`, no ICU,
//! no build script, and both wasm targets build. Its output matches the
//! lines it replaces on every probed input — the space that becomes `+`,
//! the `*` that survives, the `~` that does not, and the empty string.
//!
//! The claim was never checked, and it was restated once after `base64`
//! landed here. That is this workspace's own rule about a claim being as
//! perishable as the thing it describes, met from the direction where
//! nothing ever forced a re-measurement.
//!
//! What the measurement *does* rule out is the near neighbour:
//! `urlencoding::encode` is a different function, disagreeing on 3 of 11
//! probed inputs including the space (`a b` becomes `a%20b`), and one
//! module over it escapes `/`, `?`, `&` and `#`, turning `/a/b?x=1&y=2`
//! into `%2Fa%2Fb%3Fx%3D1%26y%3D2`. The right crate was available all
//! along; the wrong one is one letter of a name away.

use alloc::string::String;
use base64::Engine as _;

/// RFC 4648 §4, encode only.
///
/// Used for `Authorization: Basic` and `Proxy-Authorization: Basic`
/// (RFC 7617), which is every use this workspace has for it.
pub fn base64(input: &[u8]) -> String {
    // `STANDARD` is §4's alphabet with padding, which is what RFC 7617
    // wants; `STANDARD_NO_PAD` and the URL-safe engines are the three
    // wrong answers one import away, so the choice is named here once and
    // the callers keep taking a function.
    base64::engine::general_purpose::STANDARD.encode(input)
}

/// `application/x-www-form-urlencoded`, from name/value pairs.
///
/// The WHATWG URL Standard's serialiser, which is **not** RFC 3986
/// percent-encoding and differs in two places that bite: a space becomes
/// `+` rather than `%20`, and `*`, `-`, `.` and `_` are the only
/// punctuation that survives. [`crate::uri`]'s encoder is the other one
/// and they are not interchangeable — a query built with that set and
/// read by a form parser gets `+` back as a literal plus.
pub fn form_urlencoded<K: AsRef<str>, V: AsRef<str>>(
    pairs: impl IntoIterator<Item = (K, V)>,
) -> String {
    let mut ser = form_urlencoded::Serializer::new(String::new());
    for (k, v) in pairs {
        ser.append_pair(k.as_ref(), v.as_ref());
    }
    ser.finish()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[allow(unused_imports)]
    use crate::test_prelude::*;

    /// RFC 4648 §10's own vectors, plus RFC 7617 §2's `Aladdin` line —
    /// the one this function exists to produce.
    #[test]
    fn base64_matches_the_rfc_vectors() {
        for (input, want) in [
            ("", ""),
            ("f", "Zg=="),
            ("fo", "Zm8="),
            ("foo", "Zm9v"),
            ("foob", "Zm9vYg=="),
            ("fooba", "Zm9vYmE="),
            ("foobar", "Zm9vYmFy"),
            ("Aladdin:open sesame", "QWxhZGRpbjpvcGVuIHNlc2FtZQ=="),
        ] {
            assert_eq!(base64(input.as_bytes()), want, "input {input:?}");
        }
    }

    /// **The two places this differs from RFC 3986** are the whole reason
    /// it is written out rather than borrowed from `uri.rs`: a space is
    /// `+`, and only `*-._` survive as punctuation.
    #[test]
    fn the_form_serialiser_is_not_rfc_3986_percent_encoding() {
        assert_eq!(form_urlencoded([("a", "one two")]), "a=one+two");
        assert_eq!(form_urlencoded([("k", "*-._")]), "k=*-._");
        assert_eq!(form_urlencoded([("k", "~!()'")]), "k=%7E%21%28%29%27");
        // A literal `+` must not survive as itself, or it would read back
        // as a space — the round trip this encoding is most often wrong
        // about.
        assert_eq!(form_urlencoded([("k", "a+b")]), "k=a%2Bb");
    }

    /// The separators are encoded inside a component and only ever appear
    /// as separators between them.
    #[test]
    fn separators_inside_a_component_are_escaped() {
        assert_eq!(
            form_urlencoded([("a=b", "c&d")]),
            "a%3Db=c%26d",
            "or one pair would read as two"
        );
        assert_eq!(form_urlencoded([("a", "1"), ("b", "2")]), "a=1&b=2");
        assert_eq!(form_urlencoded::<&str, &str>([]), "");
    }

    /// Non-ASCII is UTF-8 then percent-encoded, byte by byte.
    #[test]
    fn non_ascii_is_utf8_percent_encoded() {
        assert_eq!(form_urlencoded([("q", "é")]), "q=%C3%A9");
        assert_eq!(form_urlencoded([("q", "日")]), "q=%E6%97%A5");
    }

    /// **Order is preserved**, because a signed query string is signed as
    /// bytes.
    #[test]
    fn the_order_given_is_the_order_sent() {
        assert_eq!(form_urlencoded([("z", "1"), ("a", "2")]), "z=1&a=2");
    }
}
