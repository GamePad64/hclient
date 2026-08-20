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
    held: Arc<Mutex<Held>>,
    _thread: std::thread::JoinHandle<()>,
}

/// Ends a [`Wire`]'s hold from somewhere that does not hold the `Wire` —
/// which is where the decision has to be made, because the test thread is
/// inside `execute` while the hold is on.
#[derive(Clone)]
pub struct Releaser {
    hold_until: Arc<Mutex<Option<Instant>>>,
    held: Arc<Mutex<Held>>,
}

impl Releaser {
    pub fn release(&self) {
        *self.hold_until.lock().unwrap() = None;
    }

    /// What the hold has caught **so far**, for a watcher deciding whether
    /// releasing now would leave the test's premise unestablished.
    ///
    /// A watcher that releases on the server's event alone can win a race
    /// against the relay: with early data the request rides in the client's
    /// *first* flight, so the server can resolve it before its own reply
    /// has reached this relay to be held at all. The release then happens
    /// with nothing held, the handshake was never delayed, and the ordering
    /// the test asserts is a coincidence rather than a guarantee.
    pub fn held(&self) -> Held {
        *self.held.lock().unwrap()
    }
}

/// What the current hold actually did, **on the relay's own clock**.
///
/// The relay is the only party that can say whether a datagram waited, and
/// it is the only one whose clock the answer belongs to. A test that asked
/// the *server* how long the handshake took and compared that against the
/// hold would be subtracting two durations with different origins — see
/// `zero_rtt.rs`, where exactly that was a flake.
#[derive(Debug, Default, Clone, Copy)]
pub struct Held {
    /// How many server-to-client datagrams this hold actually delayed.
    pub datagrams: usize,
    /// The longest single wait it imposed.
    pub longest: Duration,
    /// How many have **begun** waiting — the same datagrams, counted at the
    /// other end of the wait.
    ///
    /// `datagrams` is incremented when a wait *finishes*, which makes it
    /// useless to a watcher deciding whether to end the hold: while the
    /// relay is holding something, that count is still zero, so a condition
    /// written on it can never become true and the hold runs to its
    /// backstop. This one is incremented when a wait *starts*, which is the
    /// question a watcher is actually asking — *has the relay got anything
    /// in hand yet*.
    pub entered: usize,
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
        let held: Arc<Mutex<Held>> = Arc::new(Mutex::new(Held::default()));
        let (log, hold, holds) = (seen.clone(), hold_until.clone(), held.clone());

        // Two plain blocking threads, deliberately: this is the observer,
        // and it must not share an executor with either endpoint under
        // test.
        //
        // **Two, and not one, and that is a correctness matter rather than
        // a tidiness one.** A single loop that slept on a held
        // server-to-client datagram would not be calling `recv_from`, so it
        // would stop forwarding the CLIENT's datagrams too — and the client
        // is in the middle of sending early data. Measured with one thread
        // and an event-driven hold: ten runs in thirty-two under load ended
        // at the backstop, because the server's first flight reached the
        // relay before the last of the client's 0-RTT datagrams did, and
        // the rest of the request then sat here until the hold ended. The
        // reader never blocks now; held datagrams wait in a queue that one
        // sender drains in order.
        let (queue, pending) = std::sync::mpsc::channel::<(Vec<u8>, SocketAddr)>();
        let sender_sock = sock.try_clone().expect("a UDP socket can be cloned");
        let sender = std::thread::spawn(move || {
            while let Ok((datagram, to)) = pending.recv() {
                let started = Instant::now();
                let mut waited = false;
                // Polled in slices rather than slept through in one,
                // because `Wire::release` has to be able to end the hold
                // early — which is what lets a test hold until an EVENT
                // rather than for a duration it had to guess.
                loop {
                    let Some(t) = *hold.lock().unwrap() else {
                        break;
                    };
                    let now = Instant::now();
                    if now >= t {
                        break;
                    }
                    if !waited {
                        waited = true;
                        // Visible NOW, not when the wait ends — see
                        // `Held::entered`.
                        holds.lock().unwrap().entered += 1;
                    }
                    std::thread::sleep((t - now).min(Duration::from_millis(1)));
                }
                if waited {
                    let mut h = holds.lock().unwrap();
                    h.datagrams += 1;
                    h.longest = h.longest.max(started.elapsed());
                }
                let _ = sender_sock.send_to(&datagram, to);
            }
        });

        let thread = std::thread::spawn(move || {
            let _sender = sender;
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
                    // held back. Queued unconditionally, so the order of
                    // this direction is preserved whether or not a hold is
                    // on. See `hold_server_flight`.
                    let _ = queue.send((buf[..n].to_vec(), c));
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
            held,
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
    /// `d` is a **backstop**, not the plan: the hold normally ends at
    /// [`Wire::release`]. Make it long enough that reaching it means
    /// something went wrong, and short enough that the test fails on an
    /// assertion instead of wedging.
    pub fn hold_server_flight(&self, d: Duration) {
        *self.held.lock().unwrap() = Held::default();
        *self.hold_until.lock().unwrap() = Some(Instant::now() + d);
    }

    /// End the hold now.
    ///
    /// # Why a hold ends on an event and not on a clock
    ///
    /// The thing a 0-RTT test wants is an ordering — *the server resolved a
    /// request while its handshake was still outstanding* — and a hold
    /// measured in milliseconds turns that into a race with the server's
    /// scheduler. Measured on a 28-core host, with the hold at 400 ms and
    /// twenty spinning CPU hogs alongside eight concurrent suites: three
    /// runs in thirty-two had the server resolve the request at 401 ms
    /// against a handshake at 400 ms — not because early data was
    /// discarded, but because the server's thread had not been given a core
    /// until the flight was released and woke it. Nothing about the client
    /// was wrong, and no window rules that out; a longer one only makes it
    /// rarer.
    ///
    /// Releasing on the event removes the race rather than shrinking it.
    /// The handshake **cannot** complete while the flight is here, so a
    /// request resolved before the release is a request resolved before the
    /// handshake, at any speed and under any load. And the failure the test
    /// exists to catch is preserved and sharpened: if the server discards
    /// the early data it can only resolve the request after the handshake,
    /// which cannot happen until something releases the hold — so nothing
    /// releases it, the backstop expires, and the test says so.
    pub fn release(&self) {
        *self.hold_until.lock().unwrap() = None;
    }

    /// A handle that can [`release`](Releaser::release) this relay's hold
    /// from another task.
    pub fn releaser(&self) -> Releaser {
        Releaser {
            hold_until: self.hold_until.clone(),
            held: self.held.clone(),
        }
    }

    /// End the hold as soon as the client has actually put **something into
    /// early data**, on a thread of the relay's own.
    ///
    /// The event this releases on is the second one this file supports, and
    /// it exists for the same reason as the first: an ordering that a clock
    /// can only make likely. `docs/v04-h3-0rtt-control-stream.md`'s subject
    /// is a rejection that has to land on h3's control stream *after* the
    /// stream was opened and *before* its write finished — a window of
    /// microseconds on loopback, and a test that waits for one is the flake
    /// it was written to replace. Held, it is not a window at all: a client
    /// whose connection cannot learn of the rejection opens early-data
    /// streams whenever the scheduler next gives it a core, so a 0-RTT
    /// packet on this wire **is** "the streams are open and written to",
    /// whatever the machine was doing in between.
    ///
    /// A 0-RTT packet exists only to carry application data sent before the
    /// handshake completed, which is what makes it the right signal and not
    /// merely a convenient one.
    ///
    /// The returned handle is joinable but need not be joined; the thread
    /// gives up after `patience` so a test that never sends early data
    /// fails on its own backstop rather than on this one.
    pub fn release_on_early_data(&self, patience: Duration) -> std::thread::JoinHandle<bool> {
        let (seen, hold_until) = (self.seen.clone(), self.hold_until.clone());
        std::thread::spawn(move || {
            let deadline = Instant::now() + patience;
            while Instant::now() < deadline {
                if seen.lock().unwrap().iter().any(|p| p.kind == Kind::ZeroRtt) {
                    *hold_until.lock().unwrap() = None;
                    return true;
                }
                std::thread::sleep(Duration::from_micros(200));
            }
            false
        })
    }

    /// What the current hold actually did — see [`Held`].
    ///
    /// This exists because "the hold happened" has to be checkable, and the
    /// only honest checker is the relay. A test cannot infer it from a
    /// duration either endpoint reports: those are measured from when that
    /// endpoint entered the exchange, which is after
    /// [`Wire::hold_server_flight`] armed it, so the endpoint's number is
    /// smaller than the hold by an amount nobody bounded.
    pub fn held(&self) -> Held {
        *self.held.lock().unwrap()
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
