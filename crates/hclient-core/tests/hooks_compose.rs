//! Two hooks as one: what composition promises, and the one thing it
//! cannot promise.
//!
//! Composition here is **sequencing**, not the meet the two policy seams
//! compose by — `Hooks::on` returns `()`, so there is no verdict to
//! combine. What that leaves is an order, which is observable, and a
//! `WATCHING` rule that goes the other way from a verdict lattice.

use hclient_core::unversioned::{
    CloseReason, Closed, ConnectionId, Event, Hooks, HooksExt, NoHooks,
};
use std::cell::RefCell;
use std::rc::Rc;

/// Writes its own name into a shared log, so the *order* two hooks ran in
/// is a fact a test can read rather than one it has to trust.
#[derive(Clone)]
struct Says {
    name: &'static str,
    log: Rc<RefCell<Vec<&'static str>>>,
}

impl Hooks for Says {
    fn on(&self, _event: &Event<'_>) {
        self.log.borrow_mut().push(self.name);
    }
}

/// Panics on every event. Used to pin what a panicking hook does to its
/// neighbour — which is: the neighbour after it never runs.
#[derive(Clone, Default)]
struct Explodes;

impl Hooks for Explodes {
    fn on(&self, _event: &Event<'_>) {
        panic!("this hook panics on purpose");
    }
}

/// A hook that declares it is not watching while still counting, so
/// `WATCHING`'s composition can be checked separately from whether `on`
/// is called.
#[derive(Clone, Default)]
struct QuietCounter(Rc<RefCell<usize>>);

impl Hooks for QuietCounter {
    const WATCHING: bool = false;
    fn on(&self, _event: &Event<'_>) {
        *self.0.borrow_mut() += 1;
    }
}

fn an_event(f: impl FnOnce(&Event<'_>)) {
    let closed = Closed::new(ConnectionId::UNWATCHED, CloseReason::Ended);
    f(&Event::Closed(closed));
}

/// **Both hooks see it, and `self` runs first.**
///
/// The order is a promise rather than an artefact: two hooks with side
/// effects — a log and a metric — write in the order the caller composed
/// them, which is the whole difference between this and a policy lattice,
/// where `redirect`'s own doc says order is unobservable.
#[test]
fn both_halves_see_every_event_in_the_order_they_were_composed() {
    let log = Rc::new(RefCell::new(Vec::new()));
    let both = Says {
        name: "first",
        log: log.clone(),
    }
    .and(Says {
        name: "second",
        log: log.clone(),
    });

    an_event(|e| both.on(e));
    assert_eq!(*log.borrow(), vec!["first", "second"]);

    // And the other order, so the assertion above is about the argument
    // rather than about which name sorts first.
    log.borrow_mut().clear();
    let swapped = Says {
        name: "second",
        log: log.clone(),
    }
    .and(Says {
        name: "first",
        log: log.clone(),
    });
    an_event(|e| swapped.on(e));
    assert_eq!(*log.borrow(), vec!["second", "first"]);
}

/// Three, by nesting, because `and` is what a chain is made of and a
/// caller writing `a.and(b).and(c)` should get one left-to-right pass.
#[test]
fn a_chain_of_three_runs_left_to_right() {
    let log = Rc::new(RefCell::new(Vec::new()));
    let all = Says {
        name: "a",
        log: log.clone(),
    }
    .and(Says {
        name: "b",
        log: log.clone(),
    })
    .and(Says {
        name: "c",
        log: log.clone(),
    });
    an_event(|e| all.on(e));
    assert_eq!(*log.borrow(), vec!["a", "b", "c"]);
}

/// **`WATCHING` is a join, and `NoHooks` is the identity of composition.**
///
/// The mutation this exists against is `&&`, which is what a reader who
/// has just come from `RedirectPolicy::and` would write: with it,
/// `NoHooks.and(mine)` answers `false`, every backend skips the
/// measurement, and `mine` fires never while compiling and installing
/// perfectly.
#[test]
fn watching_composes_as_a_join_so_a_silent_neighbour_cannot_switch_a_hook_off() {
    fn watching<H: Hooks>(_: &H) -> bool {
        H::WATCHING
    }
    let counter = QuietCounter::default();
    let says = Says {
        name: "x",
        log: Rc::new(RefCell::new(Vec::new())),
    };

    assert!(
        watching(&NoHooks.and(says.clone())),
        "a watching hook beside a silent one is still watching",
    );
    assert!(
        watching(&says.clone().and(NoHooks)),
        "and the same in the other order",
    );
    assert!(
        watching(&counter.clone().and(says)),
        "`WATCHING == false` on one half does not silence the other",
    );
    assert!(
        !watching(&NoHooks.and(NoHooks)),
        "two hooks that want nothing still want nothing — `NoHooks` is the identity",
    );
    assert!(
        !watching(&counter.clone().and(NoHooks)),
        "and the const is read from the halves rather than defaulted to `true`",
    );
}

/// **A panic in the first hook means the second never sees that event**,
/// and it unwinds to the caller rather than being swallowed.
///
/// The module doc says why there is no `catch_unwind` here — `UnwindSafe`
/// would become a bound on the caller's own type, and the call does
/// nothing at all under `panic = "abort"`. This test is what stops that
/// from being quietly "fixed" into isolation that holds in some builds and
/// not others.
#[test]
fn a_panic_in_the_first_hook_stops_the_second_and_reaches_the_caller() {
    let log = Rc::new(RefCell::new(Vec::new()));
    let both = Explodes.and(Says {
        name: "never",
        log: log.clone(),
    });

    let out = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        an_event(|e| both.on(e));
    }));
    assert!(out.is_err(), "the panic is the caller's to see");
    assert!(
        log.borrow().is_empty(),
        "the hook after the panicking one never ran",
    );
}

/// The mirror: a panic in the **second** leaves the first's work done.
/// Together with the test above this says the composition is a plain
/// sequence and nothing is buffered or undone.
#[test]
fn a_panic_in_the_second_hook_leaves_the_first_hooks_work_done() {
    let log = Rc::new(RefCell::new(Vec::new()));
    let both = Says {
        name: "ran",
        log: log.clone(),
    }
    .and(Explodes);

    let out = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        an_event(|e| both.on(e));
    }));
    assert!(out.is_err());
    assert_eq!(*log.borrow(), vec!["ran"]);
}

/// A shared hook composes like an owned one, which is what makes
/// `a.and(b)` usable when `a` is the shared handle a caller already
/// installed somewhere else.
///
/// Through an `Rc` rather than an `Arc`, and that is the honest choice
/// here rather than a lint being appeased: `Says` holds an
/// `Rc<RefCell<..>>` and is therefore genuinely `!Send`, which is P13's
/// own subject — the seam declares no `Send`, so a single-threaded hook
/// composes exactly like a threaded one. The `Arc` impl is the same three
/// lines and is exercised by every `hclient-native` suite.
#[test]
fn a_shared_hook_composes_and_forwards_its_watching_const() {
    fn watching<H: Hooks>(_: &H) -> bool {
        H::WATCHING
    }
    let log = Rc::new(RefCell::new(Vec::new()));
    let shared = Rc::new(Says {
        name: "shared",
        log: log.clone(),
    });
    let both = shared.and(NoHooks);
    assert!(watching(&both));
    an_event(|e| both.on(e));
    assert_eq!(*log.borrow(), vec!["shared"]);
}
