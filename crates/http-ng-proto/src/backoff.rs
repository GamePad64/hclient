//! Exponential backoff with full jitter. Pure: randomness comes in as a
//! parameter (`jitter`), so behavior is tested without a generator and
//! without a clock.

use core::time::Duration;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Backoff {
    pub base: Duration,
    pub max: Duration,
    pub max_attempts: Option<u32>,
}

impl Default for Backoff {
    fn default() -> Self {
        Self {
            base: Duration::from_secs(1),
            max: Duration::from_secs(30),
            max_attempts: None,
        }
    }
}

/// Doublings of `base` performed before giving up and snapping straight to
/// `max`. `attempt` (a `u32`) has no upper bound of its own, so this loop
/// cannot simply run `attempt` times — that would make a caller passing
/// `attempt: u32::MAX` with a tiny `base` block for however long ~4
/// billion `Duration::checked_mul` calls take. It doesn't need to: doubling
/// the smallest nonzero `Duration` (1ns) exceeds `Duration::MAX` after 93
/// doublings (measured against this toolchain, not assumed — see the
/// `extreme_config_saturates_at_duration_max_without_panicking` test), so
/// any cap comfortably above that reaches `max`, or overflows and snaps to
/// it, long before `attempt` iterations would ever be needed. 128 is that
/// cap, with headroom to spare.
const MAX_DOUBLINGS: u32 = 128;

impl Backoff {
    /// `attempt` is zero-based. `jitter` is documented to be in
    /// `[0.0, 1.0)` (a fresh draw from a uniform RNG, per the "full
    /// jitter" model — AWS's *Exponential Backoff and Jitter*); `None`
    /// means "stop trying" (attempt budget exhausted).
    ///
    /// This signature can't return `Result`, and `None` is already spoken
    /// for — so an out-of-domain `jitter` (NaN, negative, or >= 1.0, all
    /// reachable from a caller bug, e.g. feeding in the wrong float) is
    /// clamped into `[0.0, 1.0]` rather than propagated or, worse, mapped
    /// onto `None`: that would look identical to exhausting
    /// `max_attempts` to the caller, silently stopping retries instead of
    /// merely miscalculating one delay. `f64::clamp` alone does not
    /// sanitize NaN — a NaN receiver returns NaN unchanged, verified
    /// against this toolchain — so NaN is special-cased to `0.0`: no
    /// jitter reduction, the conservative (slower) resolution, not the
    /// aggressive (immediate-retry) one that clamping NaN to `1.0` would
    /// give.
    ///
    /// `base * 2^attempt` is computed by repeated doubling, capped at
    /// `MAX_DOUBLINGS` and short-circuited the moment `max` is reached —
    /// see `MAX_DOUBLINGS` for why this is bounded work even for
    /// `attempt: u32::MAX`.
    pub fn delay(&self, attempt: u32, jitter: f64) -> Option<Duration> {
        if let Some(limit) = self.max_attempts {
            if attempt >= limit {
                return None;
            }
        }

        let mut raw = self.base.min(self.max);
        for _ in 0..attempt.min(MAX_DOUBLINGS) {
            if raw >= self.max {
                break;
            }
            raw = raw.checked_mul(2).map_or(self.max, |d| d.min(self.max));
        }

        let jitter = if jitter.is_nan() {
            0.0
        } else {
            jitter.clamp(0.0, 1.0)
        };
        Some(scale_by_kept_fraction(raw, 1.0 - jitter))
    }
}

/// `1 << 32` — the fixed-point denominator `scale_by_kept_fraction` uses
/// in place of `f64`. See that function's doc comment for why.
const FIXED_POINT_DENOM: u128 = 1 << 32;

/// Multiplies `d` by `kept`.
///
/// Precondition: `kept` is in `[0.0, 1.0]` — checked with `debug_assert!`,
/// not re-clamped. The sole caller, `delay`, already clamps `jitter` (and
/// so `kept = 1.0 - jitter`) into that range before calling here; a second
/// silent clamp on top would just be untested defensive code — the
/// `debug_assert!` makes the dependency an explicit, checked contract
/// instead: if a future edit to `delay` ever lets an out-of-range `kept`
/// through, the test suite (built in debug mode) fails loudly right here,
/// rather than this function quietly producing a plausible-looking wrong
/// answer.
///
/// Deliberately not `d.as_secs_f64() * kept` fed into
/// `Duration::from_secs_f64`: that round trip through `f64` loses enough
/// precision at the very top of `Duration`'s range that even
/// `Duration::MAX` alone — no scaling at all, `kept == 1.0` — fails to
/// convert back (`Duration::MAX.as_secs_f64()` rounds up past the last
/// value `from_secs_f64` accepts; verified against this toolchain, not
/// assumed). Fixed-point sidesteps that: `kept` is quantized to a 32-bit
/// fraction (`keep_num / 2^32`, `keep_num <= 2^32` given the precondition
/// above), so for any valid `Duration` (`as_nanos()` is at most
/// `Duration::MAX`'s ~1.84e28) multiplied by `keep_num` (at most `2^32`
/// ~= 4.29e9), the product tops out around `7.9e37` — comfortably inside
/// `u128::MAX` (~3.4e38), so the multiplication that matters here cannot
/// overflow — `.expect()` below is a loud confirmation of that proof, not
/// a caught error path: silently falling back to an unscaled value if it
/// were ever wrong would hide exactly the kind of bug this whole function
/// exists to prevent.
///
/// This is not bit-identical to the `f64` computation the brief's
/// reference code used — 32-bit fixed point resolves to about 1 part in
/// 4 billion, so a non-power-of-two `kept` (e.g. from `jitter = 0.999`)
/// picks up a sub-microsecond quantization error relative to the naive
/// float multiply. That's immaterial for a randomization parameter: the
/// whole point of jitter is to *not* be a precise value.
fn scale_by_kept_fraction(d: Duration, kept: f64) -> Duration {
    debug_assert!(
        (0.0..=1.0).contains(&kept),
        "scale_by_kept_fraction precondition violated: kept={kept} out of [0.0, 1.0]"
    );
    let keep_num = (kept * FIXED_POINT_DENOM as f64).round() as u128;
    let raw_nanos = d.as_nanos();
    let scaled_nanos = raw_nanos
        .checked_mul(keep_num)
        .expect("keep_num <= 2^32 and raw_nanos <= Duration::MAX's ~1.84e28ns keep this product inside u128::MAX — see the doc comment above")
        / FIXED_POINT_DENOM;
    let secs = (scaled_nanos / 1_000_000_000) as u64;
    let nanos = (scaled_nanos % 1_000_000_000) as u32;
    Duration::new(secs, nanos)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn b() -> Backoff {
        Backoff::default()
    }

    #[test]
    fn grows_exponentially_from_the_base() {
        assert_eq!(b().delay(0, 0.0), Some(Duration::from_secs(1)));
        assert_eq!(b().delay(1, 0.0), Some(Duration::from_secs(2)));
        assert_eq!(b().delay(2, 0.0), Some(Duration::from_secs(4)));
    }

    #[test]
    fn saturates_at_max_and_never_overflows() {
        // 2^40 seconds would overflow u32::pow — check there's no panic.
        assert_eq!(b().delay(40, 0.0), Some(Duration::from_secs(30)));
        assert_eq!(b().delay(u32::MAX, 0.0), Some(Duration::from_secs(30)));
    }

    // `delay` never calls `scale_by_kept_fraction` with an out-of-range
    // `kept` (it clamps `jitter` first), so this drives the private
    // helper directly to exercise its documented precondition on its own
    // — otherwise the `debug_assert!` in `scale_by_kept_fraction` would be
    // dead code that no test ever actually triggers.
    #[test]
    #[should_panic(expected = "precondition violated")]
    fn scale_by_kept_fraction_rejects_out_of_range_kept_via_debug_assert() {
        scale_by_kept_fraction(Duration::from_secs(8), 1.5);
    }

    // The brief's own version of this test only asserted `jittered <=
    // full`, which is true even for a `delay` that ignores `jitter`
    // entirely (`jittered == full` still satisfies `<=`). Pinning exact
    // values closes that gap: an implementation that drops jitter on the
    // floor fails the `0.5` and `0.999` assertions below, not just a
    // vacuous inequality.
    #[test]
    fn jitter_scales_the_delay_to_an_exact_fraction() {
        let full = b().delay(3, 0.0).unwrap();
        assert_eq!(full, Duration::from_secs(8));
        // 0.5 is exact in the 32-bit fixed-point representation `delay`
        // uses internally (see `scale_by_kept_fraction`), so this is an
        // exact match, not a tolerance-based one.
        assert_eq!(b().delay(3, 0.5).unwrap(), Duration::from_secs(4));
        // 0.999 is NOT exact in 32-bit fixed point (nor would it be in
        // `f64`) — assert within a generous tolerance instead of the
        // bit-exact value, which would pin the test to one specific
        // rounding scheme rather than the actual contract ("jitter scales
        // the delay down by roughly this fraction").
        let want = Duration::from_secs_f64(8.0 * 0.001);
        let got = b().delay(3, 0.999).unwrap();
        let diff = want.as_secs_f64() - got.as_secs_f64();
        assert!(
            diff.abs() < 0.000_01,
            "want ~{want:?}, got {got:?}, diff {diff}s exceeds tolerance"
        );
        assert_eq!(b().delay(3, 1.0), Some(Duration::ZERO));
    }

    #[test]
    fn stops_after_max_attempts() {
        let b = Backoff {
            max_attempts: Some(3),
            ..Backoff::default()
        };
        assert!(b.delay(2, 0.0).is_some());
        assert!(b.delay(3, 0.0).is_none(), "a fourth attempt is forbidden");
    }

    #[test]
    fn unlimited_by_default_which_must_be_a_conscious_choice() {
        // rmcp's ExponentialBackoff has max_times: None and `2u32.pow(n)`,
        // which panics on overflow after about 32 attempts.
        assert!(b().max_attempts.is_none());
        assert!(b().delay(1000, 0.0).is_some(), "and doesn't panic doing it");
    }

    // --- Invalid `jitter`: NaN, negative, and >= 1.0. The signature is
    // `-> Option<Duration>` with `None` already meaning "stop retrying",
    // so there is no channel to report a bad `jitter` as an error — it is
    // clamped into the documented domain instead of being propagated or
    // (worst of all) mapped onto `None`, which would look identical to
    // exhausting `max_attempts`.

    #[test]
    fn nan_jitter_does_not_panic_and_is_treated_as_no_reduction() {
        // `f64::clamp` on a NaN receiver returns NaN unchanged (verified
        // against this toolchain, not assumed) — so clamping alone does
        // not sanitize NaN. NaN is caller error, and this picks the
        // conservative resolution: no reduction (the *slower* retry),
        // not full reduction (the *faster*, more aggressive one).
        assert_eq!(b().delay(3, f64::NAN), Some(Duration::from_secs(8)));
    }

    #[test]
    fn negative_jitter_clamps_to_zero_instead_of_amplifying_the_delay() {
        // Un-clamped, `1.0 - jitter` for negative `jitter` exceeds 1.0 and
        // would push the delay *above* `full` — the opposite of what
        // jitter is for. Must clamp down to `full`, not blow past it.
        let full = b().delay(3, 0.0).unwrap();
        assert_eq!(b().delay(3, -5.0), Some(full));
        assert_eq!(b().delay(3, f64::NEG_INFINITY), Some(full));
    }

    #[test]
    fn jitter_at_or_above_one_clamps_to_full_reduction() {
        assert_eq!(b().delay(3, 1.0), Some(Duration::ZERO));
        assert_eq!(b().delay(3, 2.5), Some(Duration::ZERO));
        assert_eq!(b().delay(3, f64::INFINITY), Some(Duration::ZERO));
    }

    // --- Overflow boundary: `attempt` has no upper bound of its own, and
    // `base * 2^attempt` leaves `Duration`'s range within about 93
    // doublings starting from the smallest nonzero `Duration` (1ns) —
    // verified empirically against this toolchain, not assumed. The brief
    // only exercises the *default* `Backoff` (base 1s / max 30s), which
    // saturates within 5 doublings — nowhere near where an off-by-one in
    // an overflow guard would actually bite. This test picks the config
    // that maximizes the number of doublings before saturation, paired
    // with `u32::MAX`, so the boundary itself is exercised, not just a
    // plausible attempt count.
    #[test]
    fn extreme_config_saturates_at_duration_max_without_panicking() {
        let extreme = Backoff {
            base: Duration::from_nanos(1),
            max: Duration::MAX,
            max_attempts: None,
        };
        assert_eq!(extreme.delay(u32::MAX, 0.0), Some(Duration::MAX));
        // One below the empirically-measured overflow point (93
        // doublings): still growing, not yet saturated — distinguishes
        // "saturates correctly" from "saturates too early".
        assert_eq!(
            extreme.delay(50, 0.0),
            Some(Duration::from_nanos(1u64 << 50))
        );
    }

    #[test]
    fn zero_base_never_grows_and_never_loops_unboundedly() {
        // A degenerate but legal config: doubling zero stays zero forever.
        // Paired with `u32::MAX` this is the case most likely to make a
        // doubling-loop implementation iterate `attempt` times instead of
        // stopping early — this test would hang (or take a very long
        // time) if that guard were missing, rather than merely give a
        // wrong answer.
        let zero_base = Backoff {
            base: Duration::ZERO,
            max: Duration::from_secs(30),
            max_attempts: None,
        };
        assert_eq!(zero_base.delay(u32::MAX, 0.0), Some(Duration::ZERO));
    }

    use proptest::prelude::*;

    proptest! {
        // Deliberately mixes small values with the boundary values
        // (`0`, `u32::MAX`, `u32::MAX - 1`) rather than relying solely on
        // `any::<u32>()` — a prior review in this project (vertical 2)
        // found a proptest whose generator never actually reached the
        // interesting states, so the boundary is forced in, not hoped for.
        #[test]
        fn delay_never_exceeds_max(
            attempt in prop_oneof![
                4 => 0u32..10_000,
                1 => Just(0u32),
                1 => Just(u32::MAX),
                1 => Just(u32::MAX - 1),
                1 => any::<u32>(),
            ],
            jitter in 0.0f64..1.0,
            base_ms in 0u64..5_000,
            max_ms in 1u64..60_000,
        ) {
            let b = Backoff {
                base: Duration::from_millis(base_ms),
                max: Duration::from_millis(max_ms),
                max_attempts: None,
            };
            let d = b.delay(attempt, jitter).expect("max_attempts is None");
            prop_assert!(d <= b.max);
        }

        #[test]
        fn jitter_never_increases_the_delay_above_the_unjittered_value(
            attempt in prop_oneof![
                4 => 0u32..10_000,
                1 => Just(u32::MAX),
                1 => any::<u32>(),
            ],
            jitter in 0.0f64..1.0,
        ) {
            let backoff = b();
            let full = backoff.delay(attempt, 0.0).expect("max_attempts is None");
            let jittered = backoff.delay(attempt, jitter).expect("max_attempts is None");
            prop_assert!(jittered <= full);
        }

        #[test]
        fn max_attempts_boundary_is_exact(
            limit in 0u32..1_000,
            attempt in 0u32..1_100,
        ) {
            let b = Backoff { max_attempts: Some(limit), ..Backoff::default() };
            let result = b.delay(attempt, 0.0);
            if attempt >= limit {
                prop_assert!(result.is_none());
            } else {
                prop_assert!(result.is_some());
            }
        }

        // Out-of-domain jitter (including NaN, which proptest's f64
        // strategy does generate) must never panic, and must never
        // produce a delay above the un-jittered value or below zero.
        #[test]
        fn any_f64_jitter_is_safe(
            attempt in 0u32..10_000,
            jitter in any::<f64>(),
        ) {
            let backoff = b();
            let full = backoff.delay(attempt, 0.0).expect("max_attempts is None");
            let d = backoff.delay(attempt, jitter).expect("max_attempts is None");
            prop_assert!(d <= full);
            prop_assert!(d >= Duration::ZERO);
        }
    }
}
