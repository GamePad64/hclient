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

use hclient_dns::{Resolve, ResolvedAddr, SvcbEndpoint};
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
    /// Every name `lookup_svcb` was called with, in call order — by this
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
    /// The contradiction is the point: `Resolve::supports_svcb` exists so
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

    /// The names `lookup_svcb` was asked for, in order.
    pub fn svcb_names(&self) -> Vec<String> {
        self.inner.svcb_names.lock().expect("fake dns log").clone()
    }

    pub fn svcb_lookups(&self) -> usize {
        self.inner.svcb_names.lock().expect("fake dns log").len()
    }
}

impl Resolve for FakeDns {
    /// Every name resolves to loopback: the servers are there, and a
    /// resolver that answered differently per name would be a second thing
    /// under test.
    type Ipv4<'a>
        = std::pin::Pin<
        Box<dyn futures_core::Stream<Item = Result<ResolvedAddr, hclient_core::Error>> + Send + 'a>,
    >
    where
        Self: 'a;

    fn lookup_ipv4<'a>(&'a self, _name: &str) -> Self::Ipv4<'a> {
        Box::pin({
            futures_util::stream::iter(vec![Ok(ResolvedAddr {
                addr: IpAddr::from([127, 0, 0, 1]),
                ttl: None,
            })])
        })
    }

    /// Empty, and that is an answer rather than a failure — the servers
    /// are bound on the v4 loopback, and RFC 8305 has both families
    /// queried in parallel.
    type Ipv6<'a>
        = std::pin::Pin<
        Box<dyn futures_core::Stream<Item = Result<ResolvedAddr, hclient_core::Error>> + Send + 'a>,
    >
    where
        Self: 'a;

    fn lookup_ipv6<'a>(&'a self, _name: &str) -> Self::Ipv6<'a> {
        Box::pin(futures_util::stream::iter(Vec::new()))
    }

    fn supports_svcb(&self) -> bool {
        self.inner.supports_svcb
    }

    type Svcb<'a>
        = std::pin::Pin<
        Box<dyn futures_core::Stream<Item = Result<SvcbEndpoint, hclient_core::Error>> + Send + 'a>,
    >
    where
        Self: 'a;

    fn lookup_svcb<'a>(&'a self, name: &str) -> Self::Svcb<'a> {
        Box::pin({
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
                    .map(Ok)
                    .collect::<Vec<_>>(),
            )
        })
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
    SvcbEndpoint {
        priority,
        target: "both-stacks.test".to_string(),
        alpn: alpn.iter().map(|a| a.to_vec()).collect(),
        port: None,
        ipv4hint: Vec::new(),
        ipv6hint: Vec::new(),
        ech_config_list: None,
    }
}

/// An AliasMode record, as `hclient-dns-system`'s parser emits one:
/// `priority: 0` and every other field empty, because RFC 9460 §2.4.1 says
/// a recipient MUST ignore an AliasMode record's SvcParams.
///
/// Zero is also numerically *below* every ServiceMode priority, which is
/// why a selection that ranked without skipping these would choose the one
/// record whose ALPN list is empty, every time.
pub fn alias_record() -> SvcbEndpoint {
    SvcbEndpoint {
        priority: 0,
        target: "alias-target.test".to_string(),
        alpn: Vec::new(),
        port: None,
        ipv4hint: Vec::new(),
        ipv6hint: Vec::new(),
        ech_config_list: None,
    }
}
