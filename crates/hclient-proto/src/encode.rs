//! The two encodings HTTP asks for and no dependency is worth carrying.
//!
//! Both are **encode only**, which is what makes them small enough to be
//! here rather than in a crate: nothing in this workspace decodes either,
//! and a decoder is where the sharp edges live.
//!
//! # Why not `base64` and `form_urlencoded`
//!
//! `url` was removed from this workspace's graph at real cost — see
//! `uri.rs` and the 96-pair differential corpus that replaced it — and its
//! `form_urlencoded` module would bring it straight back. `base64` is one
//! crate for twenty lines. The same trade `hclient-webtransport` made for
//! its close capsule, which is 59 lines against a 48-crate dependency.

/// RFC 4648 §4, encode only.
///
/// Used for `Authorization: Basic` and `Proxy-Authorization: Basic`
/// (RFC 7617), which is every use this workspace has for it.
pub fn base64(input: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(input.len().div_ceil(3) * 4);
    for chunk in input.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        for i in 0..4 {
            // `chunk.len() + 1` output characters carry data; the rest is
            // padding, which is `=` rather than the alphabet's `A`.
            if i <= chunk.len() {
                out.push(ALPHABET[((n >> (18 - 6 * i)) & 0x3F) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

/// One `application/x-www-form-urlencoded` name or value, appended.
///
/// The WHATWG URL Standard's serialiser, which is **not** RFC 3986
/// percent-encoding and differs in two places that bite: a space becomes
/// `+` rather than `%20`, and `*`, `-`, `.` and `_` are the only
/// punctuation that survives. `uri.rs`'s `percent_encode_into` is the
/// other one and they are not interchangeable — a query built with that
/// set and read by a form parser gets `+` back as a literal plus.
fn encode_component(raw: &str, out: &mut String) {
    for &b in raw.as_bytes() {
        match b {
            b'*' | b'-' | b'.' | b'_' => out.push(b as char),
            b'0'..=b'9' | b'A'..=b'Z' | b'a'..=b'z' => out.push(b as char),
            b' ' => out.push('+'),
            _ => {
                const HEX: &[u8; 16] = b"0123456789ABCDEF";
                out.push('%');
                out.push(HEX[usize::from(b >> 4)] as char);
                out.push(HEX[usize::from(b & 0x0F)] as char);
            }
        }
    }
}

/// `a=1&b=2`, from pairs, in the order given.
///
/// Order is preserved rather than sorted: a server that signs a query
/// string — which is how most request-signing schemes work — signs the
/// bytes it was sent, and reordering them silently would break it.
pub fn form_urlencoded<K: AsRef<str>, V: AsRef<str>>(
    pairs: impl IntoIterator<Item = (K, V)>,
) -> String {
    let mut out = String::new();
    for (k, v) in pairs {
        if !out.is_empty() {
            out.push('&');
        }
        encode_component(k.as_ref(), &mut out);
        out.push('=');
        encode_component(v.as_ref(), &mut out);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

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
