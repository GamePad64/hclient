//! RFC 7838 §3, and what this parser does with everything that is not it.
//!
//! A field value arrives from a remote peer, so the interesting half of a
//! parser like this one is the malformed half: every decision below is a
//! decision rather than an `unwrap`, and the last test in the file is the
//! blunt instrument that says so — a corpus of mutations of a valid field,
//! asserting only that none of them panics.
//!
//! Nothing here reads a clock or opens a socket. The parser is sans-io and
//! the cache it feeds is clockless (`tests/altsvc_cache.rs`); the two are
//! joined to real servers in `tests/alt_svc.rs`.
#![cfg(not(target_family = "wasm"))]

use hclient_select::altsvc::{Alternative, DEFAULT_MAX_AGE, FieldValue, Origin, parse};

/// The alternatives a field value yields, or an empty list — `Clear` is a
/// different instruction and the tests that expect it say so.
fn alternatives(value: &str) -> Vec<Alternative> {
    match parse(value.as_bytes()) {
        FieldValue::Alternatives(a) => a,
        FieldValue::Clear => panic!("expected alternatives, got `clear`: {value}"),
    }
}

/// The single alternative a field value yields, when the test is about one.
fn one(value: &str) -> Alternative {
    let mut a = alternatives(value);
    assert_eq!(a.len(), 1, "expected exactly one alt-value in `{value}`");
    a.remove(0)
}

// --- the shape the whole feature is for ---------------------------------

/// The advertisement every h3-capable origin actually sends.
#[test]
fn the_field_every_origin_sends() {
    let a = one(r#"h3=":443"; ma=86400"#);
    assert_eq!(a.protocol_id, b"h3");
    assert_eq!(a.host, None, "an omitted uri-host means the origin's own");
    assert_eq!(a.port, 443);
    assert_eq!(a.max_age, 86400);
    assert!(!a.persist);
}

/// A list, with a named host, `persist`, and a protocol nobody here
/// speaks — all three at once, because a field value is a list and the
/// parser has to keep its members apart.
#[test]
fn a_list_keeps_its_members_apart() {
    let a = alternatives(r#"h2="alt.example:8443"; ma=3600, h3=":443"; persist=1"#);
    assert_eq!(a.len(), 2);
    assert_eq!(a[0].protocol_id, b"h2");
    assert_eq!(a[0].host.as_deref(), Some("alt.example"));
    assert_eq!(a[0].port, 8443);
    assert_eq!(a[0].max_age, 3600);
    assert_eq!(a[1].protocol_id, b"h3");
    assert_eq!(a[1].max_age, DEFAULT_MAX_AGE, "no `ma` means 24 hours");
    assert!(a[1].persist);
}

/// RFC 7838 §3.1: *"considered fresh for 24 hours"* where the field says
/// nothing. The constant is checked rather than the literal, so the two
/// cannot drift.
#[test]
fn the_default_max_age_is_the_rfcs_twenty_four_hours() {
    assert_eq!(DEFAULT_MAX_AGE, 86_400);
    assert_eq!(one(r#"h3=":443""#).max_age, DEFAULT_MAX_AGE);
}

// --- `clear` ------------------------------------------------------------

/// `clear` on its own is the instruction, not an empty list.
#[test]
fn clear_is_its_own_instruction() {
    assert_eq!(parse(b"clear"), FieldValue::Clear);
    assert_eq!(parse(b"  clear  "), FieldValue::Clear, "OWS around it");
}

/// `%s"clear"` is case-sensitive, so `CLEAR` is not it — and it is not an
/// alt-value either, having no `=`, so it is dropped.
#[test]
fn clear_is_case_sensitive_and_a_different_case_is_not_an_alt_value() {
    assert_eq!(parse(b"CLEAR"), FieldValue::Alternatives(Vec::new()));
    assert_eq!(parse(b"Clear"), FieldValue::Alternatives(Vec::new()));
}

/// RFC 7838 §3 decides this one explicitly: `clear` invalidates
/// everything *"including those specified in the same response, in case of
/// an invalid reply containing both 'clear' and alternative services"*.
#[test]
fn clear_beats_alternatives_in_the_same_field() {
    assert_eq!(parse(br#"h3=":443", clear"#), FieldValue::Clear);
    assert_eq!(parse(br#"clear, h3=":443""#), FieldValue::Clear);
}

// --- quoting ------------------------------------------------------------

/// The alt-authority is a quoted-string, so a comma inside it is not a
/// member boundary. A splitter that did not know that would read this as
/// two broken members and drop both.
#[test]
fn a_comma_inside_the_quotes_is_not_a_member_boundary() {
    let a = one(r#"h3=",:443""#);
    assert_eq!(a.host.as_deref(), Some(","));
    assert_eq!(a.port, 443);
}

/// RFC 9110 §5.6.4 `quoted-pair`: the backslash goes, the octet stays.
#[test]
fn a_quoted_pair_is_unescaped() {
    let a = one(r#"h3="\"x\\y:443""#);
    assert_eq!(a.host.as_deref(), Some(r#""x\y"#));
}

/// A parameter value may be a quoted-string too — `parameter = token "="
/// ( token / quoted-string )` — and `ma` is a number either way.
#[test]
fn a_parameter_value_may_be_quoted() {
    assert_eq!(one(r#"h3=":443"; ma="60""#).max_age, 60);
    assert!(one(r#"h3=":443"; persist="1""#).persist);
}

/// An unterminated quoted-string costs its own member and no other. The
/// boundaries were known before any member was parsed, so a broken member
/// says nothing about its neighbours.
#[test]
fn an_unterminated_quote_costs_only_its_own_member() {
    let a = alternatives(r#"h3=":443", h2=":8443"#);
    assert_eq!(a.len(), 1);
    assert_eq!(a[0].protocol_id, b"h3");
}

// --- the alt-authority --------------------------------------------------

/// An IPv6 literal's own colons are not the port's, and the brackets stay
/// on — `http::Uri::host` also returns them, so the two compare directly.
#[test]
fn an_ipv6_literal_keeps_its_brackets_and_its_port() {
    let a = one(r#"h3="[2001:db8::1]:8443""#);
    assert_eq!(a.host.as_deref(), Some("[2001:db8::1]"));
    assert_eq!(a.port, 8443);
}

/// Every way an alt-authority can fail to name a port, and the member goes
/// with it. Port `0` is in the list because nothing is reachable there: a
/// member naming it could only ever cost a connect.
#[test]
fn an_alt_authority_without_a_usable_port_drops_its_member() {
    for value in [
        r#"h3="example.com""#,                 // no colon at all
        r#"h3=":""#,                           // colon, no digits
        r#"h3=":https""#,                      // a name, not a number
        r#"h3=":65536""#,                      // one past a u16
        r#"h3=":99999999999999999999999999""#, // past a u64, saturating to a non-port
        r#"h3=":0""#,                          // a port nothing listens on
        r#"h3=":-1""#,                         // not 1*DIGIT
        r#"h3="[::1]443""#,                    // a bracketed host with no port colon
        "h3=:443",                             // not a quoted-string at all
    ] {
        assert_eq!(
            alternatives(value),
            Vec::new(),
            "expected `{value}` to yield nothing"
        );
    }
}

/// The `uri-host` half is **not** validated, and that is a decision rather
/// than an omission.
///
/// Validating it would mean a second URI parser here (reg-name, IPv6
/// literal, percent-encoding — `hclient-proto` owns that job), and it
/// would buy nothing: the host is only ever *compared* to the origin's, so
/// a host that is not a host cannot match one, and the only outcome
/// leniency can produce is an alternative nobody acts on. Rejecting it
/// early and rejecting it late are the same answer, and the late one is
/// the one that needs no code.
#[test]
fn the_uri_host_is_not_validated_because_it_is_only_ever_compared() {
    let a = one(r#"h3=" :443""#);
    assert_eq!(a.host.as_deref(), Some(" "), "kept as it was written");
    assert!(
        !a.is_at(&Origin::new("example.com", 443)),
        "and unreachable for exactly the reason it is nonsense"
    );
}

// --- the protocol id ----------------------------------------------------

/// RFC 7838 §3 calls the protocol-id a *"percent-encoded ALPN protocol
/// name"*, so these are the same protocol.
#[test]
fn a_percent_encoded_protocol_id_is_decoded() {
    assert_eq!(one(r#"%68%33=":443""#).protocol_id, b"h3");
    assert_eq!(one(r#"h%33=":443""#).protocol_id, b"h3");
}

/// `%` is itself a token character, so a malformed escape is a malformed
/// escape rather than a literal percent sign — and it drops its member.
#[test]
fn a_bad_percent_escape_drops_its_member() {
    for value in [
        r#"%zz=":443""#,
        r#"%4=":443""#,
        r#"%=":443""#,
        r#"h3%=":443""#,
    ] {
        assert_eq!(alternatives(value), Vec::new(), "{value}");
    }
}

/// A protocol-id has to be a token, and an empty one is not one.
#[test]
fn a_protocol_id_that_is_not_a_token_drops_its_member() {
    for value in [r#"=":443""#, r#"h 3=":443""#, r#"h"3=":443""#] {
        assert_eq!(alternatives(value), Vec::new(), "{value}");
    }
}

/// An unrecognised protocol is parsed, not rejected: this parser does not
/// know which protocols the caller speaks, and the cache that does filter
/// on it says so where it filters.
#[test]
fn an_unknown_protocol_id_parses_and_is_somebody_elses_problem() {
    let a = one(r#"quantum-http=":443""#);
    assert_eq!(a.protocol_id, b"quantum-http");
    // …and the cache, which does know, will not act on it.
    assert!(!a.protocol_id.eq_ignore_ascii_case(b"h3"));
}

// --- parameters ---------------------------------------------------------

/// RFC 7838 §3: *"Unknown parameters MUST be ignored. That is, the values
/// (alt-value) they appear in MUST be processed as if the unknown
/// parameter was not present."*
#[test]
fn an_unknown_parameter_is_ignored_and_its_member_survives() {
    let a = one(r#"h3=":443"; ma=60; futureparam=whatever; another="quoted, value""#);
    assert_eq!(a.port, 443);
    assert_eq!(a.max_age, 60);
}

/// RFC 7838 §3.1: *"Clients MUST ignore 'persist' parameters with values
/// other than '1'."* Ignoring the parameter is what leaves `persist`
/// false, which is the direction that forgets on a network change.
#[test]
fn persist_is_only_ever_the_literal_one() {
    assert!(one(r#"h3=":443"; persist=1"#).persist);
    for value in [
        r#"h3=":443"; persist=0"#,
        r#"h3=":443"; persist=2"#,
        r#"h3=":443"; persist=yes"#,
        r#"h3=":443"; persist="01""#,
    ] {
        assert!(!one(value).persist, "{value}");
    }
}

/// `ma` is `delta-seconds`, and a value that is not one invalidates its
/// member rather than falling back to the 24-hour default. This is the one
/// place a *known* parameter's bad value is fatal to a member, and the
/// reason is the direction of the mistake: caching for a day on the
/// strength of a number nobody could read is worse than not caching.
#[test]
fn a_ma_that_is_not_a_number_drops_its_member() {
    for value in [
        r#"h3=":443"; ma=abc"#,
        r#"h3=":443"; ma="#,
        r#"h3=":443"; ma=-1"#,
        r#"h3=":443"; ma=1.5"#,
        r#"h3=":443"; ma=" ""#,
    ] {
        assert_eq!(alternatives(value), Vec::new(), "{value}");
    }
}

/// RFC 9110 §5.6.7: *"a recipient that receives a value larger than it can
/// represent MUST use the largest value it can represent."* So an
/// enormous `ma` is a very long lease rather than a dropped member — and
/// the cache adds it saturatingly.
#[test]
fn a_ma_too_large_to_represent_saturates_rather_than_dropping() {
    let a = one(r#"h3=":443"; ma=99999999999999999999999999999999"#);
    assert_eq!(a.max_age, u64::MAX);
}

/// `ma=0` parses, and it parses as zero. What that *means* is the cache's
/// (`tests/altsvc_cache.rs`), and it means removal.
#[test]
fn ma_zero_is_zero_and_not_a_missing_value() {
    assert_eq!(one(r#"h3=":443"; ma=0"#).max_age, 0);
}

/// The RFC writes parameter names lowercase and marks only `clear`
/// case-sensitive. Folding is a judgement, made in the direction where
/// being wrong is cheaper: reading `MA=0` as an unknown parameter would
/// leave a 24-hour entry the origin asked to expire at once.
#[test]
fn parameter_names_are_matched_without_regard_to_case() {
    assert_eq!(one(r#"h3=":443"; MA=60"#).max_age, 60);
    assert!(one(r#"h3=":443"; Persist=1"#).persist);
}

/// No RFC basis either way; last-wins is the choice, and it is a choice.
#[test]
fn a_repeated_parameter_takes_its_last_value() {
    assert_eq!(one(r#"h3=":443"; ma=10; ma=20"#).max_age, 20);
}

/// The grammar admits OWS only around `;` and `,`, and `alt-value` ends
/// with its last parameter. Trailing junk means the member is not an
/// alt-value.
#[test]
fn junk_after_a_member_drops_it() {
    for value in [
        r#"h3=":443" junk"#,
        r#"h3=":443";"#,
        r#"h3=":443"; "#,
        r#"h3=":443"; ; ma=1"#,
        r#"h3=":443"; =1"#,
        r#"h3=":443"; ma"#,
    ] {
        assert_eq!(alternatives(value), Vec::new(), "{value}");
    }
}

/// OWS where the grammar puts it, on both sides of both delimiters.
#[test]
fn ows_around_the_delimiters_is_accepted() {
    let a = alternatives("h3=\":443\" ;\tma=1 ,\t h2=\":8443\"");
    assert_eq!(a.len(), 2);
    assert_eq!(a[0].max_age, 1);
    assert_eq!(a[1].port, 8443);
}

// --- list hygiene -------------------------------------------------------

/// RFC 9110 §5.6.1.2 asks a recipient to skip empty list elements rather
/// than to reject the list.
#[test]
fn empty_members_are_skipped() {
    let a = alternatives(r#",, h3=":443" ,, , h2=":443","#);
    assert_eq!(a.len(), 2);
}

/// A field that says nothing usable is an empty list, and that is
/// deliberately the same value an empty field gives: there is no action
/// that distinguishes them, and the cache treats both as "this field
/// advertised no h3 here", which RFC 7838 §3 makes a removal.
#[test]
fn a_field_with_nothing_usable_in_it_is_an_empty_list() {
    for value in ["", "   ", ",,,", "garbage", "h3", r#"h3=""#] {
        assert_eq!(alternatives(value), Vec::new(), "{value}");
    }
}

/// One bad member does not take its neighbours with it — there is no
/// whole-field rejection in this parser.
#[test]
fn one_bad_member_does_not_discard_the_good_ones() {
    let a = alternatives(r#"broken, h3=":443", also broken, h2=":8443""#);
    assert_eq!(a.len(), 2);
    assert_eq!(a[0].protocol_id, b"h3");
    assert_eq!(a[1].protocol_id, b"h2");
}

// --- what the cache will and will not act on ----------------------------

/// `Alternative::is_at` is the whole of "this transport can act on it":
/// the request keeps the origin's authority whatever the field names, so
/// an alternative anywhere else cannot be reached without a connect
/// address crossing the `Transport` seam.
#[test]
fn only_an_alternative_at_the_origins_own_authority_is_actionable() {
    let origin = Origin::new("example.com", 443);

    assert!(one(r#"h3=":443""#).is_at(&origin), "an omitted host");
    assert!(one(r#"h3="example.com:443""#).is_at(&origin), "named");
    assert!(
        one(r#"h3="EXAMPLE.COM:443""#).is_at(&origin),
        "a host is not case-sensitive"
    );

    assert!(!one(r#"h3=":8443""#).is_at(&origin), "another port");
    assert!(
        !one(r#"h3="other.example:443""#).is_at(&origin),
        "another host"
    );
}

// --- and the blunt instrument -------------------------------------------

/// It must not panic on anything, and this is the part that says so
/// without appealing to the reasoning above.
///
/// A deterministic corpus: every single-byte deletion, substitution and
/// insertion over a valid field value, plus every prefix of it, plus the
/// hostile shapes a hand-written list would think of. Deterministic
/// rather than random so that a failure is reproducible from the file
/// alone; `parse` is total, so the assertion is the absence of a panic
/// and there is nothing else to check.
#[test]
fn no_input_makes_the_parser_panic() {
    let seed = br#"h3=":443"; ma=86400; persist=1, h2="a\"b:8443", clear"#;
    let interesting: &[u8] = b"\"\\,;=%0.[]: \tzh3\x00\xff";

    let mut corpus: Vec<Vec<u8>> = Vec::new();
    for i in 0..=seed.len() {
        corpus.push(seed[..i].to_vec()); // every prefix, i.e. every truncation
        for &b in interesting {
            let mut v = seed.to_vec();
            v.insert(i, b);
            corpus.push(v);
        }
        if i < seed.len() {
            let mut v = seed.to_vec();
            v.remove(i);
            corpus.push(v);
            for &b in interesting {
                let mut v = seed.to_vec();
                v[i] = b;
                corpus.push(v);
            }
        }
    }
    corpus.extend(
        [
            "",
            "%",
            "%%",
            "%f",
            "\"",
            "\\",
            "a=\"\\",
            "a=\"",
            "=",
            ";",
            ",",
            "h3=\"[:443\"",
            "h3=\"]:443\"",
            "h3=\"[]:443\"",
            "h3=\"::\"",
            "h3=\":443\";;;;;;;;",
            "\u{1f600}=\":443\"",
            "h3=\"\u{1f600}:443\"",
        ]
        .iter()
        .map(|s| s.as_bytes().to_vec()),
    );
    // A long one, because a parser that recursed per member would not
    // survive it and a `while` loop that forgot to advance would hang.
    corpus.push(br#"h3=":443","#.repeat(2000));
    corpus.push(b"\"".repeat(5000));
    corpus.push(b"\\".repeat(5000));

    assert!(corpus.len() > 1000, "the corpus did not get built");
    for input in &corpus {
        // The only assertion is that this returns.
        let _ = parse(input);
    }
}
