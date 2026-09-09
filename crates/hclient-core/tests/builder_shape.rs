//! The four properties the constructors must have, from outside the crate.
//!
//! These were `bon`'s job and are now hand-written, so they need pinning
//! rather than assuming: a generated builder that stops being generated is
//! exactly where a property goes quietly missing.

use hclient_core::caps::TimeoutSupport;
use hclient_core::req::Timeouts;
use std::time::Duration;

/// **A `const`.** `hclient-dns-doh`'s `DEFAULT_TIMEOUTS` is one, and
/// `Default::default()` is not a `const fn` — so without a `const fn`
/// constructor that call site has no way to build this type at all under
/// `#[non_exhaustive]`, which closes both the literal and
/// `..Default::default()` from outside.
const DEFAULT: Timeouts = Timeouts::new()
    .with_connect(Duration::from_secs(2))
    .with_first_byte(Duration::from_secs(5));

/// The same for the capability mirror, whose constructor is named for
/// what it means rather than for emptiness: no bound enforced.
const NOTHING_ENFORCED: TimeoutSupport = TimeoutSupport::none();

#[test]
fn the_constructors_work_in_a_const() {
    assert_eq!(DEFAULT.connect, Some(Duration::from_secs(2)));
    assert_eq!(DEFAULT.first_byte, Some(Duration::from_secs(5)));
    assert_eq!(DEFAULT.resolve, None, "an unset bound stays unset");
    let nothing_enforced = NOTHING_ENFORCED;
    assert!(!nothing_enforced.connect);
}

/// **Field assignment, which `#[non_exhaustive]` does not close.** This is
/// the fact the `bon` decision missed: the attribute closes the literal
/// and the functional-update form, and leaves this alone. A caller
/// narrowing a value it was handed uses it.
#[test]
fn a_caller_outside_the_crate_can_assign_a_field() {
    let mut t = Timeouts::new();
    t.connect = Some(Duration::from_secs(1));
    assert_eq!(t.connect, Some(Duration::from_secs(1)));

    let mut s = TimeoutSupport::none();
    s.first_byte = true;
    assert!(s.first_byte && !s.connect);
}

/// **The forwarding form.** A caller that already holds an `Option` should
/// not have to write a `match`; `connect` is the only bound forwarded in
/// this workspace, so it is the only one with this form — a setter that
/// exists for symmetry is a setter with no caller.
#[test]
fn an_option_can_be_forwarded_without_a_match() {
    assert_eq!(
        Timeouts::new()
            .maybe_connect(Some(Duration::from_secs(3)))
            .connect,
        Some(Duration::from_secs(3))
    );
    assert_eq!(Timeouts::new().maybe_connect(None).connect, None);
}

/// **A dropped chain is a warning, not silence.** Every setter takes
/// `self` by value and returns it, so discarding the result discards the
/// whole configuration — the mistake `#[must_use]` exists to catch.
///
/// Asserted here as behaviour rather than as an attribute: the value comes
/// back and carries what was set, which is what the caller who *keeps* it
/// relies on. The attribute itself is checked by `just lint`, which runs
/// clippy with `-D warnings` over this workspace's own call sites.
#[test]
fn a_setter_returns_the_value_rather_than_mutating_in_place() {
    let base = Timeouts::new();
    let one = base.with_connect(Duration::from_secs(1));
    assert_eq!(base.connect, None, "the original is untouched");
    assert_eq!(one.connect, Some(Duration::from_secs(1)));
}
