//! `Alt-Svc` — the slow discovery tier: RFC 7838's field value, and the
//! memory that makes a response header able to help the *next* request.
//!
//! # Why there is a cache here when [`crate`] has none for the fast tier
//!
//! This crate's module doc says, about the HTTPS record, that there is no
//! cache *deliberately*, and gives `hclient-native`'s reason: *"this origin
//! has no HTTPS record" is a DNS answer with a TTL of its own, which
//! `SvcbEndpoint` does not carry, and inventing a lifetime for someone
//! else's answer is how a resolver's cache and ours drift apart.*
//!
//! **That reason does not transfer, and the next reader will arrive
//! knowing the opposite rule.** RFC 7838 §3.1's `ma` parameter *is* a
//! max-age, given by the origin, for exactly this advertisement — *"the
//! number of seconds since the response was generated for which the
//! alternative service is considered fresh"* — with a default of 24 hours
//! stated by the same section. So the lifetime is not invented here: it is
//! read off the wire, and a cache without one would be the dishonest
//! shape. Alt-Svc is *more* cacheable than the fast tier, not less.
//!
//! The clock is the transport's `R: Timer`, never `std::time::Instant::
//! now()`, for the reason `hclient-native`'s negative cache gives for its
//! own: `Timer` is the one seam through which time reaches a transport
//! here, and a wall-clock read would disagree with a caller testing under
//! `tokio::time::pause()`. This module never reads a clock at all — `now`
//! arrives as a parameter, exactly as it does on
//! `hclient_native::discovery::NegativeCache::suppressed` — so the cache
//! is sans-io and clockless and can be tested by handing it times.
//!
//! # What the cache stores is narrower than what the parser returns, and
//! that is deliberate
//!
//! [`parse`] is a general RFC 7838 parser: every alternative, every
//! protocol id, host, port, `ma` and `persist`. [`AltSvcCache`] stores one
//! bit per origin — *this origin advertised `h3` at its own authority* —
//! because that is the only part of an advertisement this transport can
//! act on, and *"a field carried but never read is how the previous round
//! of this plumbing came to sit unused"* (`hclient-native`'s `discovery`).
//!
//! **An alternative at a different host or port is parsed, understood, and
//! not acted on.** RFC 7838 §2: *"the Host header field … is still derived
//! from the origin, not the alternative service (just as it would if a
//! CNAME were being used)"* — so honouring `h3="other:8443"` means
//! connecting to one authority while the request keeps another's, and
//! `Transport::execute` has nowhere to say that: this crate hands the
//! request to a member whole, and the member connects to the URI's own
//! authority. It is the same wall the fast tier hit from the other side —
//! no record can cross the `Transport` seam, so `hclient-native` fetches
//! its own. Acting on it is a change to a
//! member, not a change here.

use std::collections::HashMap;
use std::fmt::Debug;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use winnow::ascii::{digit1, hex_uint, space0};
use winnow::combinator::{alt, delimited, preceded, repeat, terminated};
use winnow::token::{any, none_of, one_of, take_while};
use winnow::{ModalResult, Parser};

/// RFC 7838 §3.1: *"When an alternative service is advertised using
/// Alt-Svc, it is considered fresh for 24 hours from generation of the
/// message."*
pub const DEFAULT_MAX_AGE: u64 = 86_400;

/// What one `Alt-Svc` field value said.
///
/// The two arms are RFC 7838 §3's own top-level alternation, `Alt-Svc =
/// clear / 1#alt-value`, and they are kept apart rather than collapsed
/// into an empty list because they are different instructions: `clear`
/// invalidates what is remembered, where a list nobody could parse says
/// nothing at all. What [`AltSvcCache::note`] does with each is in its own
/// doc, and it is *not* the same thing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FieldValue {
    /// The field value was exactly `clear` — RFC 7838 §3: *"the origin
    /// requests all alternatives for that origin to be invalidated"*.
    Clear,
    /// The alternatives that parsed, in the order they appeared. Empty
    /// when nothing in the field did.
    Alternatives(Vec<Alternative>),
}

/// One RFC 7838 §3 `alt-value`: an alternative and its parameters.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Alternative {
    /// The `protocol-id`, **percent-decoded** — RFC 7838 §3 defines it as
    /// a *"percent-encoded ALPN protocol name"*, so `h3` and `%68%33` are
    /// the same protocol and this field holds what they both decode to.
    ///
    /// Bytes rather than a `String` for `SvcbEndpoint::alpn`'s reason: an
    /// ALPN name is an octet string, and percent-decoding can produce
    /// octets that are not UTF-8.
    pub protocol_id: Vec<u8>,
    /// The `uri-host` of the alt-authority, or `None` where the field
    /// omitted it — RFC 7838 §3's `[ uri-host ] ":" port` — which means
    /// the origin's own host.
    ///
    /// An IPv6 literal keeps its brackets, as `http::Uri::host` also
    /// returns them, so the two can be compared without unwrapping either.
    pub host: Option<String>,
    /// The alt-authority's port. Never zero: see [`parse`].
    pub port: u16,
    /// `ma`, or [`DEFAULT_MAX_AGE`] where the field carried none.
    pub max_age: u64,
    /// `persist=1`, and *only* that value — RFC 7838 §3.1: *"Clients MUST
    /// ignore 'persist' parameters with values other than '1'"*, so
    /// `persist=0` and `persist=yes` both arrive here as `false`, which is
    /// what ignoring the parameter means.
    pub persist: bool,
}

impl Alternative {
    /// Whether this alternative is reachable **at the origin's own
    /// authority**, which is the only kind this transport can act on — see
    /// this module's doc for why the others cannot cross the `Transport`
    /// seam.
    ///
    /// The host is compared ASCII-case-insensitively, as
    /// [`Origin::new`] lowercases the one it stores, because
    /// `Example.COM` and `example.com` are one origin.
    pub fn is_at(&self, origin: &Origin) -> bool {
        self.port == origin.port
            && match &self.host {
                None => true,
                Some(h) => h.eq_ignore_ascii_case(&origin.host),
            }
    }
}

/// The origin an advertisement is remembered against: a host and the port
/// the request actually named, the scheme's default already applied.
///
/// `https` only, and the scheme is therefore not a field. An `http://`
/// origin never reaches this cache in either direction — it cannot be
/// chosen onto QUIC (HTTP/3 has no cleartext form) so a remembered
/// advertisement for one could never be acted on, and a key that could
/// hold one would invite exactly that.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Origin {
    host: Box<str>,
    port: u16,
}

impl Origin {
    /// The host is ASCII-lowercased for the reason `hclient-native`'s pool
    /// key and negative cache both lowercase theirs: `Example.COM` and
    /// `example.com` are one origin, and a key that told them apart would
    /// remember an advertisement under a name the next request does not
    /// use.
    pub fn new(host: &str, port: u16) -> Self {
        Self {
            host: host.to_ascii_lowercase().into_boxed_str(),
            port,
        }
    }
}

/// Which origins have advertised `h3` at their own authority, and until
/// when.
///
/// Cheap to clone (an `Arc` bump) and every clone is the same cache — it
/// lives on one [`Selecting`](crate::Native) and is shared by every
/// request that transport makes, which is the whole point: a memory that
/// lasted one request would be no memory at all.
///
/// # Scope: in memory, per transport, and the network change is the
/// caller's to report
///
/// RFC 7838 §2.2 asks for one thing this crate cannot do on its own:
/// *"clients SHOULD remove from cache all alternative services that lack
/// the 'persist' flag with the value '1' when they detect such a change,
/// **when information about network state is available**"* (emphasis
/// added, and the qualifier is the whole answer). A `Transport` has no
/// notification of an interface coming up, a default route changing or a
/// VPN connecting; nothing in `hclient-rt` carries one, and inventing one
/// would be a runtime seam rather than a transport.
///
/// So the honest scope is stated in two places rather than assumed:
///
/// - **Nothing here is persisted.** The cache is a field of one
///   `Selecting`, never written to disk and never shared between
///   transports, so an advertisement outlives at most the transport that
///   heard it. A caller that drops its client on a network change has
///   already done the whole job.
/// - **[`AltSvcCache::network_changed`] is the event's only entry point**,
///   and it is public on [`Selecting`](crate::Native::network_changed)
///   for the caller that *can* see the change — an application usually
///   can, where a transport cannot. Until it is called, every entry
///   behaves as though it carried `persist=1`, which is the unsafe
///   direction and is therefore said out loud here rather than left to be
///   discovered: a laptop that moved networks is advertising an
///   alt-authority that was reachable *somewhere else*.
///
/// `persist` is not a field carried and never read: it is exactly what
/// [`AltSvcCache::network_changed`] keeps.
#[derive(Clone, Default)]
pub struct AltSvcCache {
    entries: Arc<Mutex<HashMap<Origin, Entry>>>,
}

/// One remembered advertisement.
#[derive(Debug, Clone, Copy)]
struct Entry {
    /// Elapsed time, on the owning transport's `Timer` and from that
    /// transport's epoch, past which this advertisement is stale.
    expires_at: Duration,
    /// RFC 7838 §3.1 `persist=1` — read by
    /// [`AltSvcCache::network_changed`] and by nothing else.
    persist: bool,
}

impl Debug for AltSvcCache {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AltSvcCache")
            .field(
                "advertised",
                &self.entries.lock().map(|m| m.len()).unwrap_or(0),
            )
            .finish()
    }
}

impl AltSvcCache {
    /// Whether `origin` currently advertises `h3` at its own authority —
    /// and, in the same pass, forgetting the entry when its `ma` has run
    /// out.
    ///
    /// Expiry is applied here rather than by a background sweep for
    /// `hclient-native`'s reason: this is the only place that asks, an
    /// entry nobody looks up costs one small map slot until the next
    /// lookup for that origin, and a transport is not usually built for
    /// one origin it then never contacts again.
    ///
    /// The comparison is strict, so an entry whose window has exactly
    /// closed is stale. That is what makes `ma=0` a removal rather than a
    /// zero-length lease nobody can distinguish from one.
    pub fn advertises_h3(&self, origin: &Origin, now: Duration) -> bool {
        let mut entries = self.entries.lock().expect("alt-svc cache poisoned");
        match entries.get(origin) {
            Some(e) if now < e.expires_at => true,
            Some(_) => {
                entries.remove(origin);
                false
            }
            None => false,
        }
    }

    /// Take in what one response said about `origin`.
    ///
    /// # A present field always replaces, and that is RFC 7838 §3
    ///
    /// *"When an Alt-Svc response header field is received from an origin,
    /// its value invalidates and replaces all cached alternative services
    /// for that origin."* So a field that advertises `h3` at this origin
    /// stores it, and **a field that does not — including one whose every
    /// member was malformed — removes what was stored**. That direction is
    /// deliberate twice over: it is what the sentence says, and it is the
    /// safe direction, because forgetting means going back to TCP, so the
    /// worst a garbled or hostile field can do is cost a request the
    /// faster protocol.
    ///
    /// A response with **no** `Alt-Svc` field at all is a different thing
    /// and does not reach here: absence is not an instruction, and
    /// [`Selecting`](crate::Native) does not call this when the header
    /// is missing.
    ///
    /// # `clear`
    ///
    /// Removes the entry. RFC 7838 §3 makes it beat everything in the same
    /// response — *"including those specified in the same response, in
    /// case of an invalid reply containing both 'clear' and alternative
    /// services"* — which is why [`FieldValue::Clear`] is a variant rather
    /// than a member of a list.
    ///
    /// # The first `h3` at this origin wins
    ///
    /// Not the longest-lived and not the last: RFC 7838 gives list order
    /// no meaning beyond being the order the origin wrote them in, and
    /// choosing among duplicates by `ma` would be this crate preferring
    /// the entry that keeps itself alive longest.
    pub fn note(&self, origin: &Origin, value: &FieldValue, now: Duration) {
        let mut entries = self.entries.lock().expect("alt-svc cache poisoned");
        let found = match value {
            FieldValue::Clear => None,
            FieldValue::Alternatives(list) => list
                .iter()
                .find(|a| a.protocol_id == crate::ALPN_H3 && a.is_at(origin)),
        };
        match found {
            Some(a) => {
                entries.insert(
                    origin.clone(),
                    Entry {
                        // Saturating for `hclient-native`'s reason: an
                        // elapsed time near `Duration::MAX` is not a case
                        // to panic on, and `ma` is a number a peer chose.
                        expires_at: now.saturating_add(Duration::from_secs(a.max_age)),
                        persist: a.persist,
                    },
                );
            }
            None => {
                entries.remove(origin);
            }
        }
    }

    /// The caller has seen a network configuration change: forget every
    /// advertisement that did not ask to survive one.
    ///
    /// RFC 7838 §2.2, and the `persist` parameter's only reader. See this
    /// type's doc for why the event arrives from outside.
    pub fn network_changed(&self) {
        self.entries
            .lock()
            .expect("alt-svc cache poisoned")
            .retain(|_, e| e.persist);
    }
}

/// One `Alt-Svc` field value, parsed.
///
/// Bytes rather than `&str`, because that is what a `HeaderValue` is and
/// because the grammar is ASCII: a field carrying anything else is a field
/// whose members will not match a token, which this function already has
/// an answer for.
///
/// # It must not panic, and the decisions are what it does instead
///
/// This is fed by a remote peer, so every branch below is a decision
/// rather than an `unwrap`. In order:
///
/// - **Members are split on `,` outside a quoted-string**, so a comma
///   inside an alt-authority does not invent a member boundary.
/// - **A member that does not parse is dropped and the rest stand.** There
///   is no whole-field rejection: boundaries are known before any member
///   is parsed, so one bad member says nothing about its neighbours. An
///   unterminated quoted-string therefore costs exactly the member it is
///   in — that member runs to the end of the field and fails.
/// - **Empty members are skipped**, RFC 9110 §5.6.1.2.
/// - **`clear` anywhere in the field returns [`FieldValue::Clear`]** and
///   discards the rest, which is RFC 7838 §3's own rule for *"an invalid
///   reply containing both 'clear' and alternative services"*. It is
///   matched case-sensitively, as `%s"clear"` requires.
/// - **A `protocol-id` that is not a token, or carries a bad percent
///   escape**, drops its member. `%` is itself a token character, so
///   `%zz` is a malformed escape rather than a literal.
/// - **An alt-authority that is not a quoted-string, has no port, has a
///   non-numeric port, a port that does not fit a `u16`, or port `0`**,
///   drops its member. Zero is not a port anything can be reached at, and
///   a member naming it can only ever waste a connect.
/// - **A `ma` that is not `1*DIGIT` drops its member** — the one place a
///   *known* parameter's bad value invalidates rather than being ignored,
///   because the alternative is to cache for the 24-hour default on the
///   strength of a number nobody could read. A `ma` too large for a `u64`
///   saturates instead, which is RFC 9110 §5.6.7's rule for
///   `delta-seconds`: *"a recipient that receives a value larger than it
///   can represent MUST use the largest value it can represent"*.
/// - **Unknown parameters are ignored**, RFC 7838 §3: *"the values
///   (alt-value) they appear in MUST be processed as if the unknown
///   parameter was not present"*. So is `persist` with any value but `1`,
///   which §3.1 makes a MUST of its own.
/// - **A repeated parameter takes its last value.** RFC 7838 says nothing
///   about repeats; last-wins is the choice, and it is written here
///   because it is a choice.
/// - **Parameter names are compared ASCII-case-insensitively.** The RFC
///   writes them lowercase and marks only `clear` case-sensitive, so this
///   is a judgement — made in the direction where being wrong is cheaper:
///   reading `MA=0` as unknown would leave a 24-hour entry the origin
///   asked to expire immediately.
/// - **Anything after a member's parameters that is not `;` drops the
///   member**, and so does a `;` with no parameter behind it. The grammar
///   admits OWS only around `;` and `,`.
pub fn parse(value: &[u8]) -> FieldValue {
    let mut out = Vec::new();
    for member in members(value) {
        let member = trim_ows(member);
        if member.is_empty() {
            continue;
        }
        if member == b"clear" {
            return FieldValue::Clear;
        }
        if let Some(a) = alternative(member) {
            out.push(a);
        }
    }
    FieldValue::Alternatives(out)
}

/// The field value's members, split on the commas that are *not* inside a
/// quoted-string.
///
/// **Deliberately not a winnow parser, unlike everything below it.** This
/// is a *cut* rather than a grammar: its job is to find the boundary at
/// which a malformed member can be dropped while its neighbours survive,
/// which is why `parse` can afford to refuse a whole member. Expressed as
/// a combinator it would have to decide what an *unterminated* quote
/// does, and the answer here — everything after it is inside the string,
/// so no further comma splits — is a property of this loop rather than of
/// the grammar.
fn members(value: &[u8]) -> Vec<&[u8]> {
    let mut out = Vec::new();
    let (mut start, mut i, mut quoted) = (0usize, 0usize, false);
    while i < value.len() {
        let b = value[i];
        if quoted {
            match b {
                // The escaped octet, whatever it is, is not a delimiter.
                b'\\' => i += 1,
                b'"' => quoted = false,
                _ => {}
            }
        } else if b == b'"' {
            quoted = true;
        } else if b == b',' {
            out.push(&value[start..i]);
            start = i + 1;
        }
        i += 1;
    }
    // `start` is only ever set to one past a comma, so it is at most
    // `value.len()` and this cannot be out of range.
    out.push(&value[start.min(value.len())..]);
    out
}

/// One `alt-value`, or `None` when it is not one.
///
/// `Parser::parse` requires the whole member to be consumed, which is
/// what implements this module's rule that *anything after a member's
/// parameters that is not `;` drops the member*: there is no separate
/// emptiness check to forget.
fn alternative(member: &[u8]) -> Option<Alternative> {
    alt_value.parse(member).ok()
}

/// `alt-value = alternative *( OWS ";" OWS parameter )`, RFC 7838 §3.
fn alt_value(input: &mut &[u8]) -> ModalResult<Alternative> {
    let protocol_id = terminated(protocol_id, "=").parse_next(input)?;
    let authority = quoted_string.parse_next(input)?;
    let (host, port) = alt_authority(&authority).ok_or_else(refuse)?;

    let params: Vec<(&[u8], Vec<u8>)> =
        repeat(0.., preceded((ows, ";", ows), parameter)).parse_next(input)?;
    ows.parse_next(input)?;

    let mut max_age = DEFAULT_MAX_AGE;
    let mut persist = false;
    for (name, value) in params {
        if name.eq_ignore_ascii_case(b"ma") {
            max_age = digits(&value).ok_or_else(refuse)?;
        } else if name.eq_ignore_ascii_case(b"persist") {
            persist = value == b"1";
        }
    }

    Ok(Alternative {
        protocol_id,
        host,
        port,
        max_age,
        persist,
    })
}

/// `token`, percent-decoded — RFC 7838 §3's *"percent-encoded ALPN
/// protocol name"*.
///
/// The percent arm comes first and the literal arm excludes `%`, because
/// `%` is itself a `tchar`: letting it fall through would read a
/// malformed escape as a literal percent sign, where the RFC's answer is
/// that this is not a protocol-id at all.
fn protocol_id(input: &mut &[u8]) -> ModalResult<Vec<u8>> {
    repeat(1.., alt((escape, one_of(|b: u8| is_tchar(b) && b != b'%')))).parse_next(input)
}

/// One `%XX`.
///
/// The width is stated by `take_while` rather than by `hex_uint`, which
/// would otherwise run on: `%0aa` is an escape and a literal `a`, not a
/// three-digit number.
fn escape(input: &mut &[u8]) -> ModalResult<u8> {
    preceded("%", take_while(2..=2, |b: u8| b.is_ascii_hexdigit()))
        .and_then(hex_uint)
        .parse_next(input)
}

/// RFC 9110 §5.6.4 `quoted-string`, unescaped.
///
/// The `quoted-pair` arm is first, so a backslash never reaches the
/// literal arm; a backslash as the last octet leaves `any` with nothing
/// and the string unterminated, which is the same refusal as a missing
/// closing quote.
fn quoted_string(input: &mut &[u8]) -> ModalResult<Vec<u8>> {
    delimited(
        "\"",
        repeat(0.., alt((preceded("\\", any), none_of(['"'])))),
        "\"",
    )
    .parse_next(input)
}

/// `token "=" ( token / quoted-string )`.
fn parameter<'a>(input: &mut &'a [u8]) -> ModalResult<(&'a [u8], Vec<u8>)> {
    let name = terminated(token, "=").parse_next(input)?;
    let value = alt((quoted_string, token.map(<[u8]>::to_vec))).parse_next(input)?;
    Ok((name, value))
}

/// RFC 9110 §5.6.2 `token`.
fn token<'a>(input: &mut &'a [u8]) -> ModalResult<&'a [u8]> {
    take_while(1.., is_tchar).parse_next(input)
}

/// RFC 9110 §5.6.3 `OWS`, as a parser.
///
/// `space0` is `*( SP / HTAB )` exactly, which is the production — and not
/// `multispace0`, which would also swallow a bare `\n` a field value
/// cannot contain. [`trim_ows`] is the same rule applied to a slice from
/// both ends, which is what the member split needs and a parser cannot do.
fn ows(input: &mut &[u8]) -> ModalResult<()> {
    space0.void().parse_next(input)
}

/// A refusal from a check the grammar could not express — an alt-authority
/// that is not `[host] ":" port`, or an `ma` that is not a number.
fn refuse() -> winnow::error::ErrMode<winnow::error::ContextError> {
    winnow::error::ErrMode::Backtrack(winnow::error::ContextError::new())
}

/// `[ uri-host ] ":" port`, from inside the alt-authority's quotes.
fn alt_authority(bytes: &[u8]) -> Option<(Option<String>, u16)> {
    let colon = if bytes.first() == Some(&b'[') {
        // An IPv6 literal: the port's colon is the one after the bracket,
        // never one of the address's own.
        let close = bytes.iter().position(|&b| b == b']')?;
        if bytes.get(close + 1) != Some(&b':') {
            return None;
        }
        close + 1
    } else {
        bytes.iter().rposition(|&b| b == b':')?
    };
    let port = digits(bytes.get(colon + 1..)?)?;
    let port = u16::try_from(port).ok()?;
    if port == 0 {
        return None;
    }
    let host = bytes.get(..colon)?;
    let host = if host.is_empty() {
        None
    } else {
        Some(std::str::from_utf8(host).ok()?.to_owned())
    };
    Some((host, port))
}

/// `1*DIGIT`, saturating rather than overflowing — RFC 9110 §5.6.7's rule
/// for `delta-seconds`, which `ma` is, and the same arithmetic the port
/// wants: a number too large for a `u16` is not a port, and this is where
/// it stops being one number rather than two.
fn digits(v: &[u8]) -> Option<u64> {
    // `Parser::parse` over `digit1` is `1*DIGIT` and nothing else — the
    // whole slice, at least one octet, every one a digit.
    let v = digit1::<_, winnow::error::ContextError>.parse(v).ok()?;
    // All digits, so the only way this fails is overflow, and §5.6.7 says
    // what to do about that: use the largest value we can represent.
    Some(
        std::str::from_utf8(v)
            .ok()?
            .parse::<u64>()
            .unwrap_or(u64::MAX),
    )
}

/// RFC 9110 §5.6.2 `tchar`.
fn is_tchar(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&b)
}

/// RFC 9110 §5.6.3 `OWS`, from both ends.
fn trim_ows(mut s: &[u8]) -> &[u8] {
    while let Some((b' ' | b'\t', rest)) = s.split_first() {
        s = rest;
    }
    while let Some((b' ' | b'\t', rest)) = s.split_last() {
        s = rest;
    }
    s
}
