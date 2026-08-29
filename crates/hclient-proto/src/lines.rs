//! Splitting a byte stream into lines, sans-io.
//!
//! [`LineSplitter`] is fed whatever bytes arrived and asked for whole
//! lines; it holds an unterminated tail until the rest of it turns up, so
//! a line cut in half by a frame boundary comes out whole. `hclient`'s
//! `lines()` is the adapter that drives it over a response body — NDJSON
//! and log tailing are the cases — and this is where the rules live.
//!
//! # It is SSE's splitter, and the overlap was measured rather than
//! assumed
//!
//! The rule this workspace applies before sharing code is the one the two
//! date parsers were split on: measure what is genuinely common before
//! extracting anything. Here it is the whole file. `SseDecoder` needs
//! *split a body into lines, survive any chunk boundary, count the bytes
//! for a size limit, stay linear*, and so does a general line stream —
//! there is no SSE-shaped half to leave behind, because SSE's own
//! particulars (`data:`/`event:` fields, one leading space stripped after
//! a `:`, dispatch on a blank line) are all in the decoder above it and
//! none of them are in the splitter.
//!
//! So nothing was extracted and nothing was copied: the type became
//! public where it stood, one method was added that SSE does not call
//! ([`LineSplitter::take_unterminated`]), and SSE's behaviour is byte for
//! byte what it was — its own 13 tests and the decoder's are unchanged.
//!
//! # Three terminators, and a lone CR is one of them
//!
//! `LF`, `CRLF` and a bare `CR`, which is the set the WHATWG EventSource
//! rules already fixed here.
//!
//! **One grammar rather than two**, which is `head.rs`'s argument in this
//! same crate — *a parser with two line grammars is one that two
//! implementations can disagree about* — pointed the other way: there it
//! refuses a bare LF because RFC 9112 forbids a sender to write one, and
//! here nothing forbids anything, so the widest set is the one that never
//! withholds a line somebody meant to send. Accepting a bare CR is also
//! what makes this the same code SSE runs rather than a near-copy of it.
//!
//! The cost is real and is worth knowing before pointing this at a
//! terminal capture: a body that uses a bare CR as an *in-line* control
//! character — a progress bar rewriting its own line — is split at every
//! one of them. Nothing can tell that CR from a terminator, because on the
//! wire there is nothing to tell.
//!
//! # A leading BOM is stripped, once
//!
//! Exactly one `EF BB BF` at the very start of the stream, which is what
//! every text reader does with an encoding marker (Python's `utf-8-sig`,
//! .NET's `StreamReader`) and what the EventSource rules require. A second
//! one is ordinary data. The cost lands on a caller splitting something
//! that is not text, and it is three bytes of a body that opens with them.

pub use crate::sse::lines::LineSplitter;
