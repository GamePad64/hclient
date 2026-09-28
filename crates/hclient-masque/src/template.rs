//! RFC 9298 §2's URI template, expanded without a URI-template library.
//!
//! CONNECT-UDP names its target through a URI template of exactly two
//! variables, `target_host` and `target_port`, so pulling in a general
//! RFC 6570 implementation for two fixed substitutions would be a crate
//! for a job this module does in a few lines. What is kept from RFC 6570
//! is its rule for *simple string expansion* — every unreserved character
//! (`ALPHA` / `DIGIT` / `-` / `.` / `_` / `~`) passes through unchanged,
//! everything else is percent-encoded — because that rule is what a
//! `target_host` naming an IPv6 literal actually needs: RFC 9298 §2's own
//! example expands `[2001:db8::1]` to `2001%3Adb8%3A%3A1`, brackets gone
//! and every `:` percent-encoded.

use std::fmt::Write as _;

use crate::error::TemplateError;

/// Expands `template` — an RFC 9298 §2 URI template — for `host` and
/// `port`.
///
/// `{target_host}` and `{target_port}` are replaced; `host` is
/// percent-encoded per RFC 6570 simple string expansion, and an IPv6
/// literal's brackets are stripped first so its `:` separators are
/// encoded along with everything else non-ASCII-alphanumeric.
///
/// # Errors
///
/// Returns [`TemplateError::MissingHost`] or [`TemplateError::MissingPort`]
/// when `template` has no place to put one of the two — a template naming
/// neither is not a CONNECT-UDP template at all, and substituting into it
/// would silently produce a path nothing asked for.
pub fn expand(template: &str, host: &str, port: u16) -> Result<String, TemplateError> {
    if !template.contains("{target_host}") {
        return Err(TemplateError::MissingHost);
    }
    if !template.contains("{target_port}") {
        return Err(TemplateError::MissingPort);
    }

    let bare_host = host
        .strip_prefix('[')
        .and_then(|rest| rest.strip_suffix(']'))
        .unwrap_or(host);

    Ok(template
        .replace("{target_host}", &percent_encode_unreserved(bare_host))
        .replace("{target_port}", &port.to_string()))
}

/// RFC 6570 simple string expansion's character class, applied byte by
/// byte: unreserved bytes pass through, everything else becomes `%XX`.
fn percent_encode_unreserved(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for byte in input.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~') {
            out.push(char::from(byte));
        } else {
            write!(out, "%{byte:02X}").expect("writing a byte's hex form to a String cannot fail");
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_default_template_expands_a_name_and_a_port() {
        assert_eq!(
            expand(
                "/.well-known/masque/udp/{target_host}/{target_port}/",
                "example.com",
                443
            )
            .unwrap(),
            "/.well-known/masque/udp/example.com/443/"
        );
    }

    #[test]
    fn an_ipv6_literal_is_percent_encoded_without_brackets() {
        assert_eq!(
            expand("/u/{target_host}/{target_port}/", "[2001:db8::1]", 443).unwrap(),
            "/u/2001%3Adb8%3A%3A1/443/"
        );
    }

    #[test]
    fn a_template_missing_a_variable_is_refused() {
        assert!(expand("/u/{target_host}/", "a", 1).is_err());
    }

    #[test]
    fn a_template_missing_the_host_variable_is_refused() {
        assert_eq!(
            expand("/u/{target_port}/", "a", 1),
            Err(TemplateError::MissingHost)
        );
    }
}
