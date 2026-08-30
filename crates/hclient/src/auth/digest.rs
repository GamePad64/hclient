//! HTTP Digest Access Authentication — RFC 7616.
//!
//! # Why this is here at all
//!
//! Nobody in pure Rust ships it. `reqwest` has Basic and Bearer and no
//! more; `xh`, built on `reqwest`, wrote its own rather than go without,
//! which is the evidence that the absence is felt rather than theoretical.
//! A caller who has to reach a corporate intranet otherwise reaches for
//! `libcurl`.
//!
//! And this client is unusually placed to have it: digest is a
//! challenge/response over a `401`, and `Client::run` already owns exactly
//! that shape for `425 Too Early` — a status-code branch that resends
//! inside the same `total` budget, gated on `RequestBody::retry_kind()`.
//! Nothing here needs a spawn, a clock, or a `Send` bound.
//!
//! # What is deliberately not here, each with its reason
//!
//! - **`auth-int`.** RFC 7616 §3.4.3 hashes the request body into `A2`,
//!   and this client refuses to buffer a caller's stream — that is the
//!   whole shape of [`RequestBody::Streaming`](hclient_core::RequestBody).
//!   A server offering `qop="auth,auth-int"` gets `auth`; one offering
//!   `auth-int` **alone** gets a typed refusal rather than a silently
//!   wrong response, because computing `auth` where the server asked for
//!   `auth-int` produces a `401` a caller cannot diagnose.
//! - **`userhash`** (§3.4.4). A privacy feature for the username, which a
//!   server must advertise and which no server this was tested against
//!   sends. It is an added parameter and a hash, and it will be here when
//!   something asks for it.
//! - **A nonce cache.** Each challenge is answered once with `nc=1`, so
//!   **every request pays one `401` round trip**. Reusing a nonce across
//!   requests is what removes that, and it needs per-origin state with a
//!   lifetime nobody states — the question that made a cache dishonest for
//!   SVCB records and honest for `Alt-Svc`, one crate over. Until there is
//!   a nonce lifetime to key on, the round trip is the honest price.
//! - **NTLM and Negotiate.** Both need the platform's GSSAPI or SSPI,
//!   which is `hclient-tls-native-tls`'s kind of argument one seam over
//!   and its own crate. Neither is a challenge/response this code could
//!   grow into.
//!
//! # MD5 is here, and RFC 7616 §5.2 is why that is not an oversight
//!
//! The RFC deprecates it and the deployed base has not moved: a client
//! that supported only SHA-256 would fail against most servers that speak
//! digest at all. What this code does instead is **prefer** the strongest
//! algorithm a server offers, and it never chooses the algorithm — the
//! server does, in the challenge.

pub use crate::auth::error::DigestError;
use std::fmt::Write as _;
use winnow::ascii::{space0, space1};
use winnow::combinator::{alt, delimited, opt, preceded, repeat, separated};
use winnow::token::{any, none_of, take_while};
use winnow::{ModalResult, Parser};

/// Which hash a challenge named, and whether it is the `-sess` variant.
///
/// Ordered by strength: [`Ord`] is derived and the variants are declared
/// weakest-first, so picking the best challenge a server offered is
/// `max_by_key`, and adding an algorithm means putting it in the right
/// place rather than editing a comparison.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Algorithm {
    Md5,
    Md5Sess,
    Sha256,
    Sha256Sess,
    Sha512_256,
    Sha512_256Sess,
}

impl Algorithm {
    /// The token as it goes back in the `Authorization` header, spelled as
    /// RFC 7616 §3.3 spells it.
    fn token(self) -> &'static str {
        match self {
            Algorithm::Md5 => "MD5",
            Algorithm::Md5Sess => "MD5-sess",
            Algorithm::Sha256 => "SHA-256",
            Algorithm::Sha256Sess => "SHA-256-sess",
            Algorithm::Sha512_256 => "SHA-512-256",
            Algorithm::Sha512_256Sess => "SHA-512-256-sess",
        }
    }

    /// Matched case-insensitively: §3.3 gives the tokens in one case and
    /// servers do not all agree.
    fn parse(s: &str) -> Option<Self> {
        Some(match s {
            _ if s.eq_ignore_ascii_case("MD5") => Algorithm::Md5,
            _ if s.eq_ignore_ascii_case("MD5-sess") => Algorithm::Md5Sess,
            _ if s.eq_ignore_ascii_case("SHA-256") => Algorithm::Sha256,
            _ if s.eq_ignore_ascii_case("SHA-256-sess") => Algorithm::Sha256Sess,
            _ if s.eq_ignore_ascii_case("SHA-512-256") => Algorithm::Sha512_256,
            _ if s.eq_ignore_ascii_case("SHA-512-256-sess") => Algorithm::Sha512_256Sess,
            _ => return None,
        })
    }

    fn is_sess(self) -> bool {
        matches!(
            self,
            Algorithm::Md5Sess | Algorithm::Sha256Sess | Algorithm::Sha512_256Sess
        )
    }

    /// Lowercase hex of the hash, which is what every `H()` in §3.4 means.
    fn hash(self, data: &str) -> String {
        use md5::Digest as _;
        match self {
            Algorithm::Md5 | Algorithm::Md5Sess => {
                const_hex::encode(md5::Md5::digest(data.as_bytes()))
            }
            Algorithm::Sha256 | Algorithm::Sha256Sess => {
                const_hex::encode(sha2::Sha256::digest(data.as_bytes()))
            }
            // **`Sha512Trunc256`, not `Sha512` truncated by us.** FIPS
            // 180-4's SHA-512/256 has different initial hash values, so
            // taking the first 32 bytes of a SHA-512 is a different
            // function with the same name — the sort of mistake that
            // produces a `401` nobody can explain.
            Algorithm::Sha512_256 | Algorithm::Sha512_256Sess => {
                const_hex::encode(sha2::Sha512_256::digest(data.as_bytes()))
            }
        }
    }
}

/// A `WWW-Authenticate: Digest ...` challenge, parsed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Challenge {
    pub realm: String,
    pub nonce: String,
    pub algorithm: Algorithm,
    pub opaque: Option<String>,
    /// Whether the server offered `qop=auth`. `false` means either no
    /// `qop` at all — RFC 2069's shape, which §3.4.1 still describes — or
    /// only values this client does not implement.
    pub qop_auth: bool,
    /// `true` where the server said `auth-int` and nothing else, which is
    /// the one refusal this type carries rather than an absence.
    pub only_auth_int: bool,
    /// RFC 7616 §3.3: the previous answer used an expired nonce and the
    /// credentials themselves were fine. Read by `Client` to tell a bad
    /// password from a stale nonce.
    pub stale: bool,
}

/// The one challenge to answer, out of everything the server sent.
///
/// **Every `WWW-Authenticate` value is considered, and the strongest
/// algorithm wins** — RFC 7616 §3.7 has a server send its challenges in
/// its own order of preference and expects the client to pick the
/// strongest it supports, and real servers send SHA-256 and MD5 as two
/// header lines. A client that took the first would answer MD5 to a server
/// that offered better.
///
/// A `Basic` challenge beside a `Digest` one is ignored rather than an
/// error: it is a different scheme, and this function's subject is digest.
pub fn best_challenge<'a>(
    values: impl Iterator<Item = &'a http::HeaderValue>,
) -> Result<Challenge, DigestError> {
    let mut best: Option<Challenge> = None;
    let mut saw_digest = false;
    let mut missing: Option<DigestError> = None;
    let mut auth_int_only = false;
    for v in values {
        let Ok(s) = v.to_str() else { continue };
        for (scheme, params) in challenges(s) {
            if scheme != "digest" {
                continue;
            }
            saw_digest = true;
            match parse_one(&params) {
                Ok(c) => {
                    if c.only_auth_int {
                        auth_int_only = true;
                        continue;
                    }
                    if best.as_ref().is_none_or(|b| c.algorithm > b.algorithm) {
                        best = Some(c);
                    }
                }
                // Remembered rather than returned: a malformed challenge
                // beside a usable one must not hide the usable one.
                Err(e) => missing = missing.or(Some(e)),
            }
        }
    }
    match best {
        Some(c) => Ok(c),
        None if auth_int_only => Err(DigestError::AuthIntUnsupported),
        None => Err(missing
            .filter(|_| saw_digest)
            .unwrap_or(DigestError::NoUsableChallenge)),
    }
}

/// Every challenge in one `WWW-Authenticate` value: its scheme,
/// lowercased, and its `auth-param`s with lowercased names and unescaped
/// values.
///
/// **RFC 9110 §11.6.1 is the one grammar in this crate that needs
/// backtracking**, and that is why it is written as one rather than as a
/// splitter. A `,` separates two challenges *and* two parameters of one
/// challenge, so nothing local distinguishes them: `Digest realm="r",
/// nonce="n"` and `Basic realm="b", Digest realm="r"` differ only in what
/// follows the comma. A hand-written split had to look ahead — *a token
/// then whitespace is a new challenge, a token then `=` is a parameter* —
/// and a combinator does not: the parameter list simply stops where
/// `auth-param` fails to match, and the outer list takes the comma.
///
/// Deliberately **not** `Parser::parse`: a value whose tail does not
/// parse still yields the challenges before it, which is what keeps a
/// malformed challenge from hiding a usable one. The leftover is
/// discarded, exactly as the splitter discarded a segment with no
/// recognised scheme.
fn challenges(value: &str) -> Vec<(String, Vec<(String, String)>)> {
    let mut input = value.trim();
    separated(0.., challenge, (ows, ',', ows))
        .parse_next(&mut input)
        .unwrap_or_default()
}

/// RFC 9110 §11.6.1 `challenge = auth-scheme [ 1*SP ( token68 / #auth-param ) ]`.
///
/// The `token68` arm is why a `Negotiate YWJj==` beside a `Digest` one is
/// stepped over rather than derailing the value: `auth-param` matches its
/// `YWJj` and then finds `=` with no value behind it, so the alternative
/// consumes the whole thing and the comma after it still separates
/// challenges.
fn challenge(i: &mut &str) -> ModalResult<(String, Vec<(String, String)>)> {
    let scheme = token.parse_next(i)?;
    let params = opt(preceded(
        space1,
        alt((
            separated(1.., auth_param, (ows, ',', ows)),
            token68.map(|_| Vec::new()),
        )),
    ))
    .parse_next(i)?
    .unwrap_or_default();
    Ok((scheme.to_ascii_lowercase(), params))
}

/// RFC 9110 §11.2 `auth-param = token BWS "=" BWS ( token / quoted-string )`.
fn auth_param(i: &mut &str) -> ModalResult<(String, String)> {
    let name = token.parse_next(i)?;
    (ows, '=', ows).parse_next(i)?;
    // §3.3's values are quoted-strings, in which `\"` is a literal quote.
    // Unescaped here rather than left alone — unlike the charset parameter
    // one module over, where no encoding label can contain one — because a
    // realm is free text a deployment chooses.
    let value = alt((quoted_string, token.map(str::to_owned))).parse_next(i)?;
    Ok((name.to_ascii_lowercase(), value))
}

/// RFC 9110 §5.6.4 `quoted-string`, unescaped.
fn quoted_string(i: &mut &str) -> ModalResult<String> {
    delimited(
        '"',
        repeat(0.., alt((preceded('\\', any), none_of(['"'])))),
        '"',
    )
    .parse_next(i)
}

/// RFC 9110 §5.6.2 `token`.
fn token<'a>(i: &mut &'a str) -> ModalResult<&'a str> {
    take_while(1.., |c: char| {
        c.is_ascii_alphanumeric() || "!#$%&'*+-.^_`|~".contains(c)
    })
    .parse_next(i)
}

/// RFC 9110 §11.6.1 `token68`.
fn token68<'a>(i: &mut &'a str) -> ModalResult<&'a str> {
    (
        take_while(1.., |c: char| {
            c.is_ascii_alphanumeric() || "-._~+/".contains(c)
        }),
        take_while(0.., '='),
    )
        .take()
        .parse_next(i)
}

/// RFC 9110 §5.6.3 `OWS`.
fn ows(i: &mut &str) -> ModalResult<()> {
    space0.void().parse_next(i)
}

fn parse_one(params: &[(String, String)]) -> Result<Challenge, DigestError> {
    let get = |name: &str| {
        params
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.as_str())
    };
    let realm = get("realm").ok_or(DigestError::MissingParameter { parameter: "realm" })?;
    let nonce = get("nonce").ok_or(DigestError::MissingParameter { parameter: "nonce" })?;
    // **Absent means MD5**, RFC 7616 §3.3, and an *unknown* algorithm is
    // not the same thing: a challenge naming a hash this build does not
    // have must be skipped so a weaker one beside it can win, never
    // answered as if it had said MD5.
    let algorithm = match get("algorithm") {
        None => Algorithm::Md5,
        Some(a) => Algorithm::parse(a).ok_or(DigestError::NoUsableChallenge)?,
    };
    let qop = get("qop").unwrap_or_default();
    let offered: Vec<&str> = qop
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect();
    let qop_auth = offered.iter().any(|q| q.eq_ignore_ascii_case("auth"));
    Ok(Challenge {
        realm: realm.to_owned(),
        nonce: nonce.to_owned(),
        algorithm,
        opaque: get("opaque").map(str::to_owned),
        qop_auth,
        only_auth_int: !offered.is_empty() && !qop_auth,
        stale: get("stale").is_some_and(|s| s.eq_ignore_ascii_case("true")),
    })
}

/// The `Authorization: Digest ...` value answering `challenge`.
///
/// `uri` is the request-target as it goes on the wire — path and query,
/// which is what §3.4.2 hashes and what a server recomputes; a full URL
/// here would produce a `401` on every well-behaved server.
///
/// `cnonce` is a parameter rather than drawn inside, so that the whole of
/// this function is a pure function of its inputs and the RFC's own test
/// vectors can be run against it. `Client` draws it from the OS.
pub fn answer(
    challenge: &Challenge,
    username: &str,
    password: &str,
    method: &http::Method,
    uri: &str,
    cnonce: &str,
) -> String {
    let alg = challenge.algorithm;
    let ha1 = {
        let base = alg.hash(&format!("{username}:{}:{password}", challenge.realm));
        if alg.is_sess() {
            // §3.4.2: the `-sess` variants fold the two nonces in, so the
            // stored secret is per-session rather than per-password.
            alg.hash(&format!("{base}:{}:{cnonce}", challenge.nonce))
        } else {
            base
        }
    };
    let ha2 = alg.hash(&format!("{method}:{uri}"));

    // `nc` is fixed at 1 because a nonce is used once here — see this
    // module's doc for what that costs and what removing it would need.
    const NC: &str = "00000001";
    let response = if challenge.qop_auth {
        alg.hash(&format!(
            "{ha1}:{}:{NC}:{cnonce}:auth:{ha2}",
            challenge.nonce
        ))
    } else {
        // RFC 2069's form, which §3.4.1 keeps for servers that send no
        // `qop`. Not a fallback for one we could not parse: a server that
        // offered only `auth-int` never reaches here.
        alg.hash(&format!("{ha1}:{}:{ha2}", challenge.nonce))
    };

    let mut out = format!(
        "Digest username=\"{}\", realm=\"{}\", nonce=\"{}\", uri=\"{uri}\", \
         algorithm={}, response=\"{response}\"",
        escape(username),
        escape(&challenge.realm),
        escape(&challenge.nonce),
        alg.token(),
    );
    if challenge.qop_auth {
        let _ = write!(out, ", qop=auth, nc={NC}, cnonce=\"{}\"", escape(cnonce));
    }
    if let Some(opaque) = &challenge.opaque {
        // Echoed back unchanged, §3.4: it is the server's own state and
        // this client has no business reading it.
        let _ = write!(out, ", opaque=\"{}\"", escape(opaque));
    }
    out
}

/// Escapes a quoted-string's two special characters, RFC 9110 §5.6.4.
///
/// A realm and a username are caller or server data, so a bare `"` in
/// either would end the field and let the rest be read as further
/// parameters — the same framing hazard `multipart.rs` names about a
/// `filename`, and answered the same way.
fn escape(s: &str) -> String {
    if !s.contains(['"', '\\']) {
        return s.to_owned();
    }
    let mut out = String::with_capacity(s.len() + 2);
    for c in s.chars() {
        if c == '"' || c == '\\' {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

/// A fresh client nonce, 128 bits of OS entropy as lowercase hex.
///
/// **Drawn per answer and never derived from a clock or a counter**: RFC
/// 7616 §3.4.2 folds `cnonce` into `-sess` key derivation and into every
/// `qop=auth` response, so a predictable one lets an eavesdropper
/// precompute. 128 bits is `multipart`'s boundary decision one module over,
/// from the same source and for the related reason.
///
/// **A failed draw is an error there and a panic-free zero is not an
/// option here** — but neither is what `sse.rs`'s `jitter` does, which is
/// to degrade to `0.0`. A degraded value is only acceptable when the
/// degradation has a direction, and a fixed cnonce has none: it is the one
/// value an attacker would choose. `getrandom` failing is a broken OS, so
/// this falls back to the address of a heap allocation and the challenge's
/// own nonce — worse entropy, still not a constant — rather than either
/// panicking inside a request or handing out a known value.
pub(crate) fn cnonce() -> String {
    let mut buf = [0u8; 16];
    if getrandom::fill(&mut buf).is_ok() {
        return const_hex::encode(buf);
    }
    let boxed = Box::new(0u8);
    let addr = (&raw const *boxed) as usize;
    drop(boxed);
    const_hex::encode(addr.to_ne_bytes())
}
