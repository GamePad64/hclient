//! An observer on the wire: a UDP relay that reads QUIC packet headers as
//! they go past, and forwards every byte untouched.
//!
//! # Why anything this literal is needed
//!
//! 0-RTT *rejection* can be observed from either end — the request comes
//! back as an error, or it does not. **Acceptance cannot**, and the two
//! obvious substitutes are both the client's own opinion restated: the
//! transport awaiting `quinn::ZeroRttAccepted`, or a timestamp taken inside
//! the client. `docs/v03-acceptance.md` recorded the gap in those words —
//! *"0-RTT ACCEPTANCE has not been observed end to end here; rejection
//! has"*.
//!
//! What settles it is a fact about the wire, and it needs no clock:
//! **a 0-RTT packet exists only to carry application data sent before the
//! handshake completed.** There is no other reason for the packet type to
//! be on the wire, and no way for a client that waited for the handshake to
//! emit one. So "did the request go out in early data" becomes "was a 0-RTT
//! packet sent, was it sent before the client's first Handshake packet —
//! which is where its `Finished` lives — and did it carry enough bytes to
//! be the request rather than only the h3 control stream".
//!
//! Ordering, not timing: all three facts are read off one thread's
//! sequential log of the datagrams it forwarded, so there is nothing here
//! for a busy runner to reorder.
//!
//! # What this can and cannot see
//!
//! Long headers are **not encrypted in the part that matters**: RFC 9000
//! §17.2 puts the version, the packet type and the length in cleartext, and
//! header protection covers only the packet-number field and the low bits
//! of the first byte. So the type and the length — which is all that is
//! needed to walk a datagram's coalesced packets and to size them — are
//! readable by anybody on the path. That is the property this file relies
//! on, and equally the reason it can say nothing whatever about *contents*:
//! it can prove that four kilobytes of application data went out before the
//! handshake, and cannot read a byte of them.
//!
//! # `tests/wire/mod.rs` and not `tests/wire.rs`
//!
//! Every `.rs` directly under `tests/` is its own integration-test target,
//! so a flat file here would be compiled a second time on its own and its
//! unit tests below would run twice under two names. A directory module is
//! not a target. (`tests/server.rs` is flat and gets away with it only
//! because it has no tests of its own.)

#![cfg(not(target_family = "wasm"))]
#![allow(dead_code)]

use std::net::{SocketAddr, UdpSocket};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// A QUIC packet's type, as far as the cleartext part of its header says.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Initial,
    /// The one this file exists for.
    ZeroRtt,
    Handshake,
    Retry,
    /// A short header: 1-RTT, and always the last packet in its datagram
    /// because it carries no length field.
    OneRtt,
    /// A datagram this walker could not follow — a version it does not
    /// know, or a truncation. Recorded rather than skipped, so a misparse
    /// shows up as a gap in the log instead of as a clean absence.
    Unparsed,
}

/// One packet on the wire: what it was, and how much it carried.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Packet {
    pub kind: Kind,
    /// The header's `Length` field: packet number plus protected payload.
    /// Zero for a short header, whose length is implicit.
    pub len: usize,
}

/// A UDP relay in front of a server, logging what the client sends.
pub struct Wire {
    /// The address to dial *instead of* the server's.
    pub addr: SocketAddr,
    seen: Arc<Mutex<Vec<Packet>>>,
    hold_until: Arc<Mutex<Option<Instant>>>,
    _thread: std::thread::JoinHandle<()>,
}

impl std::fmt::Debug for Wire {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Wire").field("addr", &self.addr).finish()
    }
}

impl Wire {
    /// Bind a relay that forwards to `server`. Returns once it is bound, so
    /// a test never races it.
    pub fn in_front_of(server: SocketAddr) -> Self {
        let sock = UdpSocket::bind("127.0.0.1:0").expect("an unprivileged loopback UDP bind");
        let addr = sock.local_addr().unwrap();
        let seen: Arc<Mutex<Vec<Packet>>> = Arc::new(Mutex::new(Vec::new()));
        let hold_until: Arc<Mutex<Option<Instant>>> = Arc::new(Mutex::new(None));
        let (log, hold) = (seen.clone(), hold_until.clone());

        // A plain blocking thread, deliberately: this is the observer, and
        // it must not share an executor with either endpoint under test.
        let thread = std::thread::spawn(move || {
            // 64 KiB because that is the largest a UDP datagram can be;
            // QUIC's are ~1200, but a buffer that could truncate would
            // silently turn a coalesced datagram into an `Unparsed`.
            let mut buf = vec![0u8; 65535];
            let mut client: Option<SocketAddr> = None;
            loop {
                let Ok((n, from)) = sock.recv_from(&mut buf) else {
                    return;
                };
                if let Some(c) = client.filter(|_| from == server) {
                    // Server -> client, and the one direction that can be
                    // held back. See `hold_server_flight`.
                    let until = *hold.lock().unwrap();
                    if let Some(t) = until {
                        let now = Instant::now();
                        if now < t {
                            std::thread::sleep(t - now);
                        }
                    }
                    let _ = sock.send_to(&buf[..n], c);
                } else {
                    client = Some(from);
                    log.lock().unwrap().extend(packets(&buf[..n]));
                    let _ = sock.send_to(&buf[..n], server);
                }
            }
        });

        Self {
            addr,
            seen,
            hold_until,
            _thread: thread,
        }
    }

    /// Hold every server-to-client datagram for the next `d`, so that the
    /// client's handshake **cannot** complete inside that window.
    ///
    /// Not padding against a slow runner: it is what turns "the request
    /// probably went out before the handshake finished" into "it could not
    /// have gone out any other way". A client's handshake completes when it
    /// processes the server's flight; if that flight is still sitting in
    /// this relay, everything the client sends meanwhile is early data by
    /// construction, whatever the scheduler did.
    ///
    /// The window has to stay well under a QUIC PTO (quinn's first is about
    /// a second, from a 333 ms assumed initial RTT) or the endpoints start
    /// retransmitting and the log fills with duplicates.
    pub fn hold_server_flight(&self, d: Duration) {
        *self.hold_until.lock().unwrap() = Some(Instant::now() + d);
    }

    /// Everything the client has sent since the last [`Wire::forget`], in
    /// order.
    pub fn client_sent(&self) -> Vec<Packet> {
        self.seen.lock().unwrap().clone()
    }

    /// Drop the log, so a later phase of a test is not read through an
    /// earlier connection's packets.
    pub fn forget(&self) {
        self.seen.lock().unwrap().clear();
    }
}

/// A QUIC variable-length integer: the top two bits of the first byte give
/// the length, the rest of the bits are the value (RFC 9000 §16).
fn varint(d: &[u8]) -> Option<(u64, usize)> {
    let first = *d.first()?;
    let len = 1usize << (first >> 6);
    if d.len() < len {
        return None;
    }
    let mut v = u64::from(first & 0x3f);
    for &b in &d[1..len] {
        v = (v << 8) | u64::from(b);
    }
    Some((v, len))
}

/// Walk a datagram's coalesced packets and name each one's type and size.
///
/// Walking rather than reading the first byte and stopping is not
/// thoroughness for its own sake: a client's first flight **coalesces its
/// Initial and its 0-RTT packets into one datagram**, so a reader that
/// looked only at the front would report `Initial` and conclude that no
/// early data was ever sent.
fn packets(mut d: &[u8]) -> Vec<Packet> {
    let mut out = Vec::new();
    let stop = |out: &mut Vec<Packet>, kind: Kind, len: usize| {
        out.push(Packet { kind, len });
    };
    loop {
        let Some(&first) = d.first() else { return out };
        if first & 0x80 == 0 {
            // Short header: no length field, so it runs to the end of the
            // datagram and there is nothing after it to walk to.
            stop(&mut out, Kind::OneRtt, 0);
            return out;
        }
        if d.len() < 5 || u32::from_be_bytes([d[1], d[2], d[3], d[4]]) != 1 {
            // Truncated, or version negotiation, or a QUIC this walker does
            // not know. Everything after the version field is
            // version-specific, so guessing would be worse than stopping.
            stop(&mut out, Kind::Unparsed, 0);
            return out;
        }
        let kind = match (first & 0x30) >> 4 {
            0 => Kind::Initial,
            1 => Kind::ZeroRtt,
            2 => Kind::Handshake,
            _ => Kind::Retry,
        };
        let mut p = 5;
        // Both connection IDs: an 8-bit length, then that many bytes.
        for _ in 0..2 {
            let Some(&len) = d.get(p) else {
                stop(&mut out, Kind::Unparsed, 0);
                return out;
            };
            p += 1 + usize::from(len);
        }
        if kind == Kind::Retry {
            // A Retry carries a token and an integrity tag to the end of
            // the datagram, and no length field.
            stop(&mut out, kind, d.len().saturating_sub(p));
            return out;
        }
        if kind == Kind::Initial {
            let Some((token_len, n)) = d.get(p..).and_then(varint) else {
                stop(&mut out, Kind::Unparsed, 0);
                return out;
            };
            p += n + token_len as usize;
        }
        let Some((len, n)) = d.get(p..).and_then(varint) else {
            stop(&mut out, Kind::Unparsed, 0);
            return out;
        };
        p += n + len as usize;
        out.push(Packet {
            kind,
            len: len as usize,
        });
        if p >= d.len() {
            return out;
        }
        d = &d[p..];
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The walker's own arithmetic, on a datagram assembled by hand, so
    /// that a green live test cannot be a misparse that happened to look
    /// right.
    #[test]
    fn a_coalesced_initial_and_0_rtt_reads_as_both() {
        // Initial: 0xc0, version 1, dcid(4), scid(0), token length 0,
        // length 2, two bytes of payload.
        let mut d = vec![0xc0, 0, 0, 0, 1, 4, 1, 2, 3, 4, 0, 0x00, 0x02, 0xaa, 0xbb];
        // 0-RTT, appended in the same datagram: 0xd0, version 1, dcid(4),
        // scid(0), length 3.
        d.extend_from_slice(&[0xd0, 0, 0, 0, 1, 4, 1, 2, 3, 4, 0, 0x03, 1, 2, 3]);
        assert_eq!(
            packets(&d),
            vec![
                Packet {
                    kind: Kind::Initial,
                    len: 2
                },
                Packet {
                    kind: Kind::ZeroRtt,
                    len: 3
                },
            ]
        );
    }

    #[test]
    fn a_short_header_ends_the_walk() {
        assert_eq!(
            packets(&[0x40, 1, 2, 3]),
            vec![Packet {
                kind: Kind::OneRtt,
                len: 0
            }]
        );
    }

    #[test]
    fn a_truncated_header_is_reported_and_not_guessed() {
        assert_eq!(packets(&[0xc0, 0, 0])[0].kind, Kind::Unparsed);
    }

    #[test]
    fn a_version_this_walker_does_not_know_stops_it() {
        // Version negotiation is version 0, and everything after the
        // version field is version-specific.
        assert_eq!(packets(&[0xc0, 0, 0, 0, 0, 0])[0].kind, Kind::Unparsed);
    }

    #[test]
    fn varints_of_every_length() {
        assert_eq!(varint(&[0x25]), Some((37, 1)));
        assert_eq!(varint(&[0x7b, 0xbd]), Some((15293, 2)));
        assert_eq!(varint(&[0x9d, 0x7f, 0x3e, 0x7d]), Some((494_878_333, 4)));
        assert_eq!(varint(&[0x40]), None, "two bytes promised, one supplied");
    }
}
