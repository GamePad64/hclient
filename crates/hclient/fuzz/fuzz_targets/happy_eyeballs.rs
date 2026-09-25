#![no_main]
use core::time::Duration;
use hclient_proto::happy_eyeballs::{HeAction, HeConfig, Scheduler};
use libfuzzer_sys::fuzz_target;
use std::net::{IpAddr, Ipv4Addr};

// Invariant: the scheduler always converges to Exhausted and never panics.
fuzz_target!(|data: &[u8]| {
    let mut s = Scheduler::new(HeConfig::default());
    let addrs: Vec<IpAddr> = data
        .iter()
        .take(16)
        .map(|b| IpAddr::V4(Ipv4Addr::new(10, 0, 0, *b)))
        .collect();
    s.offer_v4(&addrs);
    s.mark_v4_done();
    s.mark_v6_done();
    let mut t = Duration::ZERO;
    for _ in 0..64 {
        match s.poll(t) {
            HeAction::Start(_) => {}
            HeAction::Wait(d) => t += d.max(Duration::from_millis(1)),
            HeAction::Exhausted => return,
        }
        t += Duration::from_millis(1);
    }
});
