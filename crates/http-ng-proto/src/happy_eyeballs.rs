//! The Happy Eyeballs v2 scheduler (RFC 8305). Pure: time comes in as the
//! `elapsed` parameter, so the constants can be checked without `sleep`.

use core::time::Duration;
use std::collections::VecDeque;
use std::net::IpAddr;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HeConfig {
    /// RFC 8305 §3: "This delay will be referred to as the 'Resolution
    /// Delay'. The recommended value for the Resolution Delay is 50
    /// milliseconds." — how long to wait for the AAAA response before
    /// falling back to A.
    pub resolution_delay: Duration,
    /// RFC 8305 §5: "This delay is referred to as the 'Connection Attempt
    /// Delay'. One recommended value for a default delay is 250
    /// milliseconds." — the pause between launching attempts. The actual
    /// value is clamped to a range after `Scheduler` is constructed, see
    /// `ATTEMPT_MIN` and `ATTEMPT_MAX`.
    pub attempt_delay: Duration,
    /// RFC 8305 §4/§8: "Recommended to be 1; 2 may be used to more
    /// aggressively favor a particular address family." — how many
    /// addresses of the first (IPv6) family go consecutively before
    /// interleaving with the other.
    pub first_family_count: usize,
}

impl Default for HeConfig {
    fn default() -> Self {
        Self {
            resolution_delay: Duration::from_millis(50),
            attempt_delay: Duration::from_millis(250),
            first_family_count: 1,
        }
    }
}

/// RFC 8305 §5/§8, "Minimum Connection Attempt Delay": "The recommended
/// minimum value is 100 milliseconds ... This minimum value is required to
/// avoid congestion collapse in the presence of high packet-loss rates."
///
/// The RFC separately names an even smaller number — "a subsequent
/// connection MUST NOT be started within 10 milliseconds of the previous
/// attempt" (§5) — but that's its absolute hard floor, not a
/// recommendation: the RFC itself names 100 ms as the recommended value and
/// explains why (protection against congestion collapse under high packet
/// loss). Since this crate claims RFC 8305 compatibility, the default
/// clamp takes the recommended value, not the legal minimum.
const ATTEMPT_MIN: Duration = Duration::from_millis(100);

/// RFC 8305 §5/§8, "Maximum Connection Attempt Delay": "The current
/// recommended value is 2 seconds."
const ATTEMPT_MAX: Duration = Duration::from_secs(2);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HeAction {
    Start(IpAddr),
    Wait(Duration),
    Exhausted,
}

/// The Happy Eyeballs v2 scheduler's state for a single connection attempt.
///
/// A pure state machine: it knows nothing about clocks or sockets. The
/// caller (Task 11, the connector) feeds it resolver results through
/// `offer_v6` / `offer_v4` / `mark_v6_done` / `mark_v4_done` and advances
/// it with calls to `poll(elapsed)`, where `elapsed` is the time since the
/// attempt started, on the caller's clock. `elapsed` must be monotonically
/// non-decreasing across calls; `poll` doesn't panic if it decreases, but
/// `Wait` stops being bounded by `max(attempt_delay, resolution_delay)` —
/// that's a precondition of the interface, not a checked invariant.
///
/// `Scheduler` doesn't sort addresses within a family and hands them back
/// in the order they arrived in `offer_v6` / `offer_v4`. Sorting by
/// Destination Address Selection (RFC 8305 §4, RFC 6724 §6) is the
/// caller's concern, before `offer_*`; it isn't done here, deliberately,
/// not by oversight.
#[derive(Debug)]
pub struct Scheduler {
    cfg: HeConfig,
    v6: VecDeque<IpAddr>,
    v4: VecDeque<IpAddr>,
    v6_done: bool,
    v4_done: bool,
    started: usize,
    last_start: Option<Duration>,
    /// How many addresses of the first family (IPv6) have already been
    /// handed out consecutively since the last switch to the other family.
    run_in_first_family: usize,
}

impl Scheduler {
    /// Constructs the scheduler. `cfg.attempt_delay` outside the
    /// `[ATTEMPT_MIN, ATTEMPT_MAX]` range is clamped, not rejected with an
    /// error: a `Duration` outside this range isn't meaningless input
    /// (unlike, say, an invalid URI), but a value for which RFC 8305 §5
    /// itself gives only recommendations, not a mandatory protocol format,
    /// and by this task's interface, this function must return `Self`, not
    /// `Result`. The value isn't discarded silently: the effective config
    /// can always be checked against the requested one via `config()`.
    pub fn new(mut cfg: HeConfig) -> Self {
        cfg.attempt_delay = cfg.attempt_delay.clamp(ATTEMPT_MIN, ATTEMPT_MAX);
        Self {
            cfg,
            v6: VecDeque::new(),
            v4: VecDeque::new(),
            v6_done: false,
            v4_done: false,
            started: 0,
            last_start: None,
            run_in_first_family: 0,
        }
    }

    /// The effective config after clamping — see `new`'s doc comment.
    pub fn config(&self) -> &HeConfig {
        &self.cfg
    }

    pub fn offer_v6(&mut self, addrs: &[IpAddr]) {
        self.v6.extend(addrs.iter().copied());
    }
    pub fn offer_v4(&mut self, addrs: &[IpAddr]) {
        self.v4.extend(addrs.iter().copied());
    }
    pub fn mark_v6_done(&mut self) {
        self.v6_done = true;
    }
    pub fn mark_v4_done(&mut self) {
        self.v4_done = true;
    }

    /// Advances the state machine. `elapsed` is the time since the attempt
    /// started, on the caller's clock (see the struct's doc comment on
    /// monotonicity).
    pub fn poll(&mut self, elapsed: Duration) -> HeAction {
        // Nothing left to offer, and both resolvers have confirmed no new
        // addresses are coming: report it immediately, without waiting out
        // the Connection Attempt Delay from the last start — there's
        // nothing left to wait for.
        if self.v6.is_empty() && self.v4.is_empty() && self.v6_done && self.v4_done {
            return HeAction::Exhausted;
        }

        // RFC 8305 §5: the pause between launching attempts (Connection
        // Attempt Delay).
        if let Some(last) = self.last_start {
            let next_at = last + self.cfg.attempt_delay;
            if elapsed < next_at {
                return HeAction::Wait(next_at - elapsed);
            }
        }

        // RFC 8305 §3: while AAAA hasn't arrived and the resolver isn't
        // done, hold IPv4 back for the Resolution Delay.
        if self.v6.is_empty() && !self.v6_done && elapsed < self.cfg.resolution_delay {
            return HeAction::Wait(self.cfg.resolution_delay - elapsed);
        }

        // RFC 8305 §4: IPv6 goes first; after `first_family_count`
        // addresses in a row, we interleave families, until one of them
        // runs dry — then we drain the rest without interleaving.
        let take_v6 = if self.v6.is_empty() {
            false
        } else if self.v4.is_empty() || self.started == 0 {
            // Either the other family doesn't exist at all, or this is the
            // very first pick — and IPv6 always goes first.
            true
        } else {
            self.run_in_first_family < self.cfg.first_family_count
        };

        let picked = if take_v6 {
            self.v6.pop_front()
        } else {
            self.v4.pop_front()
        };

        let Some(addr) = picked else {
            // Both families are empty right now, but at least one resolver
            // hasn't said "done" yet (otherwise the early return above
            // would have fired) — addresses may still arrive. Ask the
            // caller to poll again no sooner than one Resolution Delay
            // from now.
            return HeAction::Wait(self.cfg.resolution_delay);
        };

        self.started += 1;
        self.last_start = Some(elapsed);
        self.run_in_first_family = if take_v6 {
            self.run_in_first_family + 1
        } else {
            0
        };
        HeAction::Start(addr)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
    use std::time::Duration;

    fn v6(n: u16) -> IpAddr {
        IpAddr::V6(Ipv6Addr::new(0x20, 0, 0, 0, 0, 0, 0, n))
    }
    fn v4(n: u8) -> IpAddr {
        IpAddr::V4(Ipv4Addr::new(10, 0, 0, n))
    }
    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    #[test]
    fn prefers_ipv6_first() {
        let mut s = Scheduler::new(HeConfig::default());
        s.offer_v6(&[v6(1)]);
        s.offer_v4(&[v4(1)]);
        s.mark_v6_done();
        s.mark_v4_done();
        assert_eq!(s.poll(ms(0)), HeAction::Start(v6(1)));
    }

    #[test]
    fn waits_resolution_delay_for_ipv6_before_falling_back_to_ipv4() {
        let mut s = Scheduler::new(HeConfig::default());
        s.offer_v4(&[v4(1)]);
        s.mark_v4_done();
        // AAAA hasn't arrived yet: RFC 8305 §3 says wait out the Resolution Delay.
        assert_eq!(s.poll(ms(0)), HeAction::Wait(ms(50)));
        assert_eq!(s.poll(ms(50)), HeAction::Start(v4(1)));
    }

    #[test]
    fn interleaves_families_with_first_family_count_of_one() {
        let mut s = Scheduler::new(HeConfig::default());
        s.offer_v6(&[v6(1), v6(2)]);
        s.offer_v4(&[v4(1), v4(2)]);
        s.mark_v6_done();
        s.mark_v4_done();
        assert_eq!(s.poll(ms(0)), HeAction::Start(v6(1)));
        assert_eq!(s.poll(ms(250)), HeAction::Start(v4(1)));
        assert_eq!(s.poll(ms(500)), HeAction::Start(v6(2)));
        assert_eq!(s.poll(ms(750)), HeAction::Start(v4(2)));
    }

    #[test]
    fn interleaves_with_a_first_family_count_greater_than_one() {
        // RFC 8305 §4/§8: "2 may be used to more aggressively favor a
        // particular address family" — 2 here, to distinguish the block
        // pattern from the strict 1:1 interleaving in the test above.
        let cfg = HeConfig {
            first_family_count: 2,
            ..Default::default()
        };
        let mut s = Scheduler::new(cfg);
        s.offer_v6(&[v6(1), v6(2), v6(3)]);
        s.offer_v4(&[v4(1), v4(2), v4(3)]);
        s.mark_v6_done();
        s.mark_v4_done();
        assert_eq!(s.poll(ms(0)), HeAction::Start(v6(1)));
        assert_eq!(s.poll(ms(250)), HeAction::Start(v6(2)));
        assert_eq!(s.poll(ms(500)), HeAction::Start(v4(1)));
        assert_eq!(s.poll(ms(750)), HeAction::Start(v6(3)));
        assert_eq!(s.poll(ms(1000)), HeAction::Start(v4(2)));
        assert_eq!(s.poll(ms(1250)), HeAction::Start(v4(3)));
    }

    #[test]
    fn enforces_the_attempt_delay_between_starts() {
        let mut s = Scheduler::new(HeConfig::default());
        s.offer_v6(&[v6(1), v6(2)]);
        s.mark_v6_done();
        s.mark_v4_done();
        assert_eq!(s.poll(ms(0)), HeAction::Start(v6(1)));
        assert_eq!(s.poll(ms(100)), HeAction::Wait(ms(150)));
        assert_eq!(s.poll(ms(250)), HeAction::Start(v6(2)));
    }

    #[test]
    fn reports_exhausted_when_everything_is_started_and_resolvers_are_done() {
        let mut s = Scheduler::new(HeConfig::default());
        s.offer_v6(&[v6(1)]);
        s.mark_v6_done();
        s.mark_v4_done();
        assert_eq!(s.poll(ms(0)), HeAction::Start(v6(1)));
        assert_eq!(s.poll(ms(999)), HeAction::Exhausted);
    }

    #[test]
    fn exhausted_is_reported_immediately_even_before_the_attempt_delay_elapses() {
        // Review resolution: a naive implementation gates Exhausted behind
        // the same check as the pause between attempts (Connection Attempt
        // Delay), and answers Wait(240 ms) instead of Exhausted, even
        // though there's nothing left to start — nothing left to wait for.
        let mut s = Scheduler::new(HeConfig::default());
        s.offer_v6(&[v6(1)]);
        s.mark_v6_done();
        s.mark_v4_done();
        assert_eq!(s.poll(ms(0)), HeAction::Start(v6(1)));
        assert_eq!(
            s.poll(ms(10)),
            HeAction::Exhausted,
            "no addresses left and both resolvers are done — no reason to wait out the rest of attempt_delay"
        );
    }

    #[test]
    fn poll_after_exhausted_keeps_returning_exhausted() {
        let mut s = Scheduler::new(HeConfig::default());
        s.offer_v6(&[v6(1)]);
        s.mark_v6_done();
        s.mark_v4_done();
        assert_eq!(s.poll(ms(0)), HeAction::Start(v6(1)));
        assert_eq!(s.poll(ms(1_000)), HeAction::Exhausted);
        assert_eq!(
            s.poll(ms(50_000)),
            HeAction::Exhausted,
            "a repeat poll after Exhausted must not panic or change the answer"
        );
    }

    #[test]
    fn falls_back_to_ipv4_immediately_when_ipv6_resolver_reports_zero_addresses() {
        let mut s = Scheduler::new(HeConfig::default());
        s.offer_v4(&[v4(1)]);
        s.mark_v6_done(); // the AAAA resolver ran and returned no addresses
        s.mark_v4_done();
        assert_eq!(
            s.poll(ms(0)),
            HeAction::Start(v4(1)),
            "AAAA is definitely not coming — no reason to wait out the Resolution Delay"
        );
    }

    #[test]
    fn uses_only_ipv6_when_ipv4_resolver_reports_zero_addresses() {
        let mut s = Scheduler::new(HeConfig::default());
        s.offer_v6(&[v6(1), v6(2)]);
        s.mark_v6_done();
        s.mark_v4_done(); // the A resolver ran and returned no addresses
        assert_eq!(s.poll(ms(0)), HeAction::Start(v6(1)));
        assert_eq!(s.poll(ms(250)), HeAction::Start(v6(2)));
        assert_eq!(s.poll(ms(500)), HeAction::Exhausted);
    }

    #[test]
    fn late_ipv6_arrival_after_resolution_delay_is_still_attempted() {
        let mut s = Scheduler::new(HeConfig::default());
        s.offer_v4(&[v4(1), v4(2)]);
        s.mark_v4_done();
        assert_eq!(s.poll(ms(0)), HeAction::Wait(ms(50)));
        assert_eq!(s.poll(ms(50)), HeAction::Start(v4(1)));
        // The AAAA response arrives late, already during IPv4 attempts —
        // RFC 8305 §3: "the newly received IPv6 addresses are incorporated
        // into the list of available candidate addresses ... and the
        // process of connection attempts will continue with the IPv6
        // addresses added".
        s.offer_v6(&[v6(1)]);
        s.mark_v6_done();
        assert_eq!(
            s.poll(ms(300)),
            HeAction::Start(v6(1)),
            "a late AAAA must be taken into account, not dropped"
        );
        assert_eq!(s.poll(ms(550)), HeAction::Start(v4(2)));
    }

    #[test]
    fn more_addresses_offered_after_the_queues_run_dry_mid_schedule() {
        let mut s = Scheduler::new(HeConfig::default());
        s.offer_v6(&[v6(1)]);
        // Neither resolver has said "done" yet — the second address may
        // arrive later.
        assert_eq!(s.poll(ms(0)), HeAction::Start(v6(1)));
        // The queues are empty, but the resolvers aren't done — this is
        // NOT Exhausted, it's a signal to "ask again later."
        assert_eq!(s.poll(ms(250)), HeAction::Wait(ms(50)));

        s.offer_v4(&[v4(1)]);
        s.mark_v6_done();
        s.mark_v4_done();
        assert_eq!(s.poll(ms(300)), HeAction::Start(v4(1)));
        assert_eq!(s.poll(ms(600)), HeAction::Exhausted);
    }

    #[test]
    fn first_family_count_exceeding_available_addresses_falls_through_to_other_family() {
        let cfg = HeConfig {
            first_family_count: 5,
            ..Default::default()
        };
        let mut s = Scheduler::new(cfg);
        s.offer_v6(&[v6(1)]);
        s.offer_v4(&[v4(1), v4(2), v4(3)]);
        s.mark_v6_done();
        s.mark_v4_done();
        assert_eq!(s.poll(ms(0)), HeAction::Start(v6(1)));
        assert_eq!(s.poll(ms(250)), HeAction::Start(v4(1)));
        assert_eq!(s.poll(ms(500)), HeAction::Start(v4(2)));
        assert_eq!(s.poll(ms(750)), HeAction::Start(v4(3)));
        assert_eq!(s.poll(ms(1000)), HeAction::Exhausted);
    }

    #[test]
    fn ipv6_still_goes_first_when_first_family_count_is_zero() {
        // `first_family_count` isn't clamped (the RFC gives no bounds for
        // it), so 0 is a legal, reachable value (and the proptest below
        // generates it). For FAFC >= 1 the condition `run_in_first_family <
        // first_family_count` alone would already pick IPv6 first (the run
        // starts at 0), so the `|| self.started == 0` disjunct in `poll`
        // changes nothing for most FAFC values — except zero, where it's
        // the only thing keeping the "IPv6 first" promise (RFC 8305 §2)
        // from silently breaking.
        let cfg = HeConfig {
            first_family_count: 0,
            ..Default::default()
        };
        let mut s = Scheduler::new(cfg);
        s.offer_v6(&[v6(1)]);
        s.offer_v4(&[v4(1)]);
        s.mark_v6_done();
        s.mark_v4_done();
        assert_eq!(
            s.poll(ms(0)),
            HeAction::Start(v6(1)),
            "RFC 8305 §2: IPv6 is preferred first regardless of first_family_count"
        );
    }

    #[test]
    fn attempt_delay_is_clamped_to_the_rfc_recommended_range() {
        // RFC 8305 §5/§8, "Minimum Connection Attempt Delay": "The
        // recommended minimum value is 100 milliseconds". NOT 10 ms — that
        // smaller threshold in the RFC's text describes something else: an
        // absolute hard floor ("a subsequent connection MUST NOT be started
        // within 10 milliseconds of the previous attempt"), not the
        // default recommendation for the clamp.
        let c = HeConfig {
            attempt_delay: ms(1),
            ..Default::default()
        };
        assert_eq!(
            Scheduler::new(c).config().attempt_delay,
            ms(100),
            "RFC 8305 §5/§8: recommended minimum Connection Attempt Delay is 100 ms"
        );
        // RFC 8305 §5/§8, "Maximum Connection Attempt Delay": "The current
        // recommended value is 2 seconds."
        let c = HeConfig {
            attempt_delay: Duration::from_secs(30),
            ..Default::default()
        };
        assert_eq!(
            Scheduler::new(c).config().attempt_delay,
            Duration::from_secs(2),
            "RFC 8305 §5/§8: recommended maximum Connection Attempt Delay is 2 s"
        );
    }

    #[test]
    fn clamped_attempt_delay_is_discoverable_via_config() {
        // Resolution for "no silent no-ops": `Scheduler::new` can't return
        // a Result (the signature is fixed by this task's interface) and
        // doesn't panic on an out-of-range attempt_delay, but it doesn't
        // hide the substitution either — the effective value can always be
        // compared against the requested one via `config()`.
        let requested = ms(1);
        let s = Scheduler::new(HeConfig {
            attempt_delay: requested,
            ..Default::default()
        });
        assert_ne!(
            s.config().attempt_delay,
            requested,
            "a substituted value must be visible via config()"
        );
    }

    /// Runs the scheduler until `Exhausted`, accumulating `elapsed`, and
    /// along the way checks that every `Wait` doesn't exceed
    /// `max(attempt_delay, resolution_delay)`. Returns addresses in the
    /// order `Start` handed them out.
    ///
    /// `MAX_STEPS` isn't an unbounded wait (the test is synchronous,
    /// there's no `sleep` anywhere): it's an upper bound on the number of
    /// steps, and a panic on exceeding it signals a convergence bug, not a
    /// real timeout.
    fn drain_to_exhausted(s: &mut Scheduler) -> Vec<IpAddr> {
        const MAX_STEPS: usize = 10_000;
        let bound = s.config().attempt_delay.max(s.config().resolution_delay);
        let mut elapsed = Duration::ZERO;
        let mut starts = Vec::new();
        for _ in 0..MAX_STEPS {
            match s.poll(elapsed) {
                HeAction::Start(addr) => starts.push(addr),
                HeAction::Wait(d) => {
                    assert!(
                        d <= bound,
                        "Wait({d:?}) exceeds max(attempt_delay, resolution_delay) = {bound:?}"
                    );
                    elapsed += d;
                }
                HeAction::Exhausted => return starts,
            }
        }
        panic!("scheduler failed to converge to Exhausted within {MAX_STEPS} steps");
    }

    /// An independent oracle for RFC 8305 §4's interleaving rule: a round
    /// is a block of the first family (IPv6) sized `first_family_count`
    /// (but no less than 1 in the very first round — RFC 8305 §2, IPv6
    /// always goes first), followed by one address of the second; rounds
    /// repeat until one of the families runs dry, after which the rest of
    /// the other is drained in a row, with no further interleaving —
    /// replicating the `v4.is_empty() / v6.is_empty()` branch in `poll`,
    /// not the block arithmetic.
    ///
    /// Implemented differently from `Scheduler::poll`: an explicit loop
    /// over the indices of two slices, with a block size computed per
    /// round, rather than accumulating state through a counter like
    /// `run_in_first_family` and a `started` flag. The point of the
    /// difference in form is that a bug specifically in the state-machine
    /// version inside `poll` (say, in the `<` comparison at a block
    /// boundary, or in resetting the counter on a family switch) is less
    /// likely to reproduce the same way here and get caught by the
    /// comparison, rather than slip past a test that's really just
    /// checking itself.
    fn expected_interleave(v6: &[IpAddr], v4: &[IpAddr], first_family_count: usize) -> Vec<IpAddr> {
        if v4.is_empty() {
            return v6.to_vec();
        }
        if v6.is_empty() {
            return v4.to_vec();
        }
        let mut out = Vec::new();
        let (mut vi, mut fi) = (0usize, 0usize);
        let mut round = 0usize;
        while vi < v6.len() && fi < v4.len() {
            let block = if round == 0 {
                first_family_count.max(1)
            } else {
                first_family_count
            };
            let take = block.min(v6.len() - vi);
            out.extend_from_slice(&v6[vi..vi + take]);
            vi += take;
            if vi >= v6.len() {
                // IPv6 ran dry mid-round — the remaining IPv4 is drained
                // below without interleaving; this round doesn't get an
                // IPv4 pick.
                break;
            }
            out.push(v4[fi]);
            fi += 1;
            round += 1;
        }
        out.extend_from_slice(&v6[vi..]);
        out.extend_from_slice(&v4[fi..]);
        out
    }

    use proptest::prelude::*;

    proptest! {
        #[test]
        fn starts_match_the_rfc8305_interleave_order_and_waits_are_bounded(
            v6_n in 0usize..6,
            v4_n in 0usize..6,
            first_family_count in 0usize..4,
            resolution_delay_ms in 0u64..200,
            attempt_delay_ms in 0u64..500,
        ) {
            let v6_addrs: Vec<IpAddr> = (0..v6_n as u16).map(v6).collect();
            let v4_addrs: Vec<IpAddr> = (0..v4_n as u8).map(v4).collect();

            let cfg = HeConfig {
                resolution_delay: Duration::from_millis(resolution_delay_ms),
                attempt_delay: Duration::from_millis(attempt_delay_ms),
                first_family_count,
            };
            let mut s = Scheduler::new(cfg);
            s.offer_v6(&v6_addrs);
            s.offer_v4(&v4_addrs);
            s.mark_v6_done();
            s.mark_v4_done();

            let starts = drain_to_exhausted(&mut s);
            let expected = expected_interleave(&v6_addrs, &v4_addrs, first_family_count);

            // Matching full sequences already implies "every address
            // exactly once" (permutation equality is a weaker consequence
            // of componentwise equality), so a separate multiset check
            // would be redundant.
            prop_assert_eq!(starts, expected);
        }
    }
}
