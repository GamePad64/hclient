//! Pure state machines for hclient's protocol layers.
//!
//! Everything in here is **sans-io**: bytes, URIs and durations go in,
//! decisions come out, and no function opens a socket, reads a clock or
//! draws entropy. `hclient` and its transports drive these machines over
//! real connections; you can drive them over anything, and test them with
//! nothing at all.
//!
//! Crate invariant: no `async fn`, no runtime dependency, anywhere. Anything
//! that depends on time takes `now` as a parameter. Enforced in CI.
//!
//! ```
//! use hclient_proto::sse::{SseDecoder, SseEvent};
//! use hclient_proto::uri;
//!
//! // A `text/event-stream` body, in whatever pieces the network cut it into.
//! let mut sse = SseDecoder::new(64 * 1024);
//! sse.push(b"event: tick\ndata: 1\n").unwrap();
//! assert_eq!(sse.next(), None); // no blank line yet, so no event
//! sse.push(b"\n").unwrap();
//! assert_eq!(
//!     sse.next(),
//!     Some(SseEvent::Message { event: Some("tick".into()), data: "1".into(), id: None }),
//! );
//!
//! // A relative `Location:` resolved against the URL that answered with it.
//! let base: http::Uri = "https://example.com/a/b".parse().unwrap();
//! let next = uri::resolve_reference(&base, "../c?x=1").unwrap();
//! assert_eq!(next.to_string(), "https://example.com/c?x=1");
//! ```
//!
//! # What is here
//!
//! - [`redirect`] — whether to follow a redirect: the
//!   [`RedirectPolicy`](redirect::RedirectPolicy) trait, the stock
//!   policies ([`Limit`](redirect::Limit),
//!   [`SameOriginOnly`](redirect::SameOriginOnly),
//!   [`HttpsOnly`](redirect::HttpsOnly)), and the RFC 9110 mechanism
//!   around them — method rewriting, credentials across origins.
//! - [`retry`] and [`backoff`] — whether to send a request again and how
//!   long to wait: [`retry::Standard`] decides from one outcome,
//!   [`backoff::Backoff`] is exponential with full jitter, the jitter
//!   handed in rather than drawn.
//! - [`happy_eyeballs`] — the RFC 8305 connection-racing
//!   [`Scheduler`](happy_eyeballs::Scheduler), with elapsed time as a
//!   parameter.
//! - [`sse`] and [`lines`] — the WHATWG `EventSource` decoder, and a line
//!   splitter for NDJSON and logs; both hold a partial line across chunks.
//! - [`uri`] — the one place a string becomes an [`http::Uri`], and RFC
//!   3986 reference resolution.
//! - [`head`] — an RFC 9112 response head parsed from bytes, for a
//!   `CONNECT` tunnel or anything else that reads HTTP/1 by hand.
//! - [`link`] — RFC 8288 `Link:` headers, for paginated APIs.
//! - [`encode`] — base64 and `application/x-www-form-urlencoded`,
//!   encode only.
//!
//! # Features
//!
//! - `idn` (default) — non-ASCII host names are converted to their ASCII
//!   form through `hclient-idn`. Without it such a host is refused as
//!   [`UriError::NonAsciiHost`](uri::UriError::NonAsciiHost), naming the
//!   A-label to send instead, and the Unicode tables leave the build.
//!
//! # Where to go next
//!
//! Most callers meet these machines through `hclient`, which re-exports
//! the policy types a caller configures (`hclient::redirect`,
//! `hclient::retry`). `hclient-core` holds the traits the transports
//! implement.
#![forbid(unsafe_code)]
#![warn(missing_docs)]

mod error;

pub mod backoff;
pub mod encode;
// `#[doc(hidden)]` and not a promise — the reason is the module's own
// first paragraph. A `///` here rather than a `//` would resolve that
// module's `//!` links in *this* scope instead of its own, which is the
// defect this workspace already paid for once when the jar and the cache
// became modules.
#[doc(hidden)]
pub mod field;
pub mod happy_eyeballs;
pub mod head;
pub mod lines;
pub mod link;
pub mod redirect;
pub mod retry;
pub mod sse;
pub mod uri;
