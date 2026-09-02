//! A resolver that answers from a script and writes down what it was
//! asked.
//!
//! Two things are being observed through it. The **answer** is what the
//! tests vary — an HTTPS record offering `h3` or not — and the **log** is
//! what makes "no lookup happened" an assertion rather than an absence:
//! `RequireVersion` and `http://` are both claimed to skip discovery
//! entirely, and a counter is the only way to see that from outside.
#![cfg(not(target_family = "wasm"))]
#![allow(dead_code)]

use hclient_dns::{RData, Record, Resolve, SvcbEndpoint, rtype};
use std::net::IpAddr;
use std::sync::{Arc, Mutex};

#[derive(Clone, Debug)]
pub struct FakeDns {
    inner: Arc<Inner>,
}

#[derive(Debug)]
struct Inner {
    supports_svcb: bool,
    records: Vec<SvcbEndpoint>,
    /// Every name `lookup` was called with, in call order — by this
    /// transport and by `hclient-native`'s own connector alike, which is
    /// what makes the duplicate query in `tests/dns_cost.rs` visible.
    svcb_names: Mutex<Vec<String>>,
}

impl Default for FakeDns {
    fn default() -> Self {
        Self::new()
    }
}

impl FakeDns {
    /// A resolver that can do SVCB and has no records to give — the shape
    /// of an origin that publishes none.
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Inner {
                supports_svcb: true,
                records: Vec::new(),
                svcb_names: Mutex::new(Vec::new()),
            }),
        }
    }

    pub fn with_records(records: Vec<SvcbEndpoint>) -> Self {
        Self {
            inner: Arc::new(Inner {
                supports_svcb: true,
                records,
                svcb_names: Mutex::new(Vec::new()),
            }),
        }
    }

    /// A resolver that **cannot** ask, holding records it would have given
    /// if it could.
    ///
    /// The contradiction is the point: `Resolve::supports` exists so
    /// that "cannot ask" and "asked and found nothing" are distinguishable,
    /// and a transport that inferred the answer from an empty stream would
    /// behave identically for both. Here the stream is not empty, so
    /// anything but asking the capability would choose QUIC.
    pub fn cannot_ask_but_would_have_said(records: Vec<SvcbEndpoint>) -> Self {
        Self {
            inner: Arc::new(Inner {
                supports_svcb: false,
                records,
                svcb_names: Mutex::new(Vec::new()),
            }),
        }
    }

    /// The names `lookup` was asked for, in order.
    pub fn svcb_names(&self) -> Vec<String> {
        self.inner.svcb_names.lock().expect("fake dns log").clone()
    }

    pub fn svcb_lookups(&self) -> usize {
        self.inner.svcb_names.lock().expect("fake dns log").len()
    }
}

impl Resolve for FakeDns {
    type Records<'a>
        = std::pin::Pin<
        Box<dyn futures_core::Stream<Item = Result<Record, hclient_core::Error>> + Send + 'a>,
    >
    where
        Self: 'a;

    /// Addresses always; HTTPS only where the fixture was built to say
    /// it can ask. `cannot_ask_but_would_have_said` is the arm that needs
    /// the field: it holds a record and reports that it cannot ask for
    /// one, which is the state `supports` exists to separate from an
    /// empty stream — so writing this as a constant list of types makes
    /// the control test vacuous, which is exactly what happened once.
    fn supports(&self, rtype: u16) -> bool {
        match rtype {
            rtype::A | rtype::AAAA => true,
            rtype::HTTPS => self.inner.supports_svcb,
            _ => false,
        }
    }

    fn lookup<'a>(&'a self, name: &str, rtype: u16) -> Self::Records<'a> {
        let _ = name;
        match rtype {
            rtype::A => Box::pin({
                futures_util::stream::iter(vec![Ok(Record::new(RData::from(IpAddr::from([
                    127, 0, 0, 1,
                ]))))])
            }),
            rtype::AAAA => Box::pin(futures_util::stream::iter(Vec::new())),
            rtype::HTTPS => Box::pin({
                self.inner
                    .svcb_names
                    .lock()
                    .expect("fake dns log")
                    .push(name.to_owned());
                futures_util::stream::iter(
                    self.inner
                        .records
                        .clone()
                        .into_iter()
                        .map(|e| Ok(Record::new(RData::Https(e))))
                        .collect::<Vec<_>>(),
                )
            }),
            _ => Box::pin(futures_util::stream::empty()),
        }
    }
}

/// A ServiceMode record: a real priority (RFC 9460 §2.4.2 — anything but
/// zero) and an ALPN list.
///
/// `target` is set to the origin's own name, which is also the only value
/// this transport could act on if it read the field at all — it does not:
/// the record is consulted for one bit, whether `h3` is in `alpn`, and
/// addresses stay each member's own business.
pub fn service_record(priority: u16, alpn: &[&[u8]]) -> SvcbEndpoint {
    assert_ne!(priority, 0, "priority 0 is AliasMode; use `alias_record`");
    SvcbEndpoint::new(priority, "both-stacks.test".to_string())
        .alpn(alpn.iter().map(|a| a.to_vec()).collect())
}

/// An AliasMode record, as `hclient-dns-system`'s parser emits one:
/// `priority: 0` and every other field empty, because RFC 9460 §2.4.1 says
/// a recipient MUST ignore an AliasMode record's SvcParams.
///
/// Zero is also numerically *below* every ServiceMode priority, which is
/// why a selection that ranked without skipping these would choose the one
/// record whose ALPN list is empty, every time.
pub fn alias_record() -> SvcbEndpoint {
    SvcbEndpoint::new(0, "alias-target.test".to_string())
}
