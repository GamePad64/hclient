//! A handshake written outside this crate that wraps a SOCKS5 one can hand
//! on its UDP association, although it cannot make one of its own.
//!
//! This file is an outside crate as far as visibility goes, so it can
//! name only what `hclient-proxy` makes public: the association is an
//! opaque value, and forwarding it is the whole of what a wrapper does.

use bytes::{Bytes, BytesMut};
use hclient_core::error::Error;
use hclient_proxy::{
    Approach, Association, Decision, EgressFilter, Handshake, Proxy, Rules, Socks5, Step, Target,
};

/// A foreign protocol: SOCKS5 underneath, and a trace of every call
/// would go here in a real one.
#[derive(Clone)]
struct Wrapped(Socks5);

impl Handshake for Wrapped {
    fn approach(&self, use_tls: bool) -> Approach {
        self.0.approach(use_tls)
    }
    fn begin(&mut self, host: &str, port: u16) -> Result<Bytes, Error> {
        self.0.begin(host, port)
    }
    fn advance(&mut self, from_peer: &mut BytesMut) -> Result<Step, Error> {
        self.0.advance(from_peer)
    }
    fn associate(&self) -> Option<Association> {
        self.0.associate()
    }
}

fn datagrams(protocol: Wrapped) -> bool {
    let rules = Rules::new().push(Proxy::new(protocol, "px", 1080));
    let Decision::Filtered(route) = rules.route(&Target::new("o", 443, true)) else {
        panic!("a proxy rule filters");
    };
    route.support.datagrams
}

#[test]
fn a_foreign_handshake_forwards_a_wrapped_socks5_association() {
    let w = Wrapped(Socks5::new().with_udp());
    assert!(w.associate().is_some());
    assert!(datagrams(w));
}

#[test]
fn a_foreign_handshake_over_socks5_without_udp_declares_no_datagrams() {
    let w = Wrapped(Socks5::new());
    assert!(w.associate().is_none());
    assert!(!datagrams(w));
}
