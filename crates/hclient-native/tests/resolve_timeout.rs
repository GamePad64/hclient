//! `Timeouts::resolve`, against resolvers that answer late, never, and at
//! once.
//!
//! Every assertion here is on **which phase failed**, because that is the
//! whole of the feature: without a separate bound a hanging resolver and an
//! unreachable origin are the same `Timeout(Connect)`, and only one of them
//! is worth a different retry.
#![cfg(not(target_family = "wasm"))]

use hclient_core::unversioned::Transport;
use hclient_core::{ErrorKind, Phase, RequestBody, Timeouts};
use hclient_dns::{RData, Record, Resolve, SvcbEndpoint, rtype};
use hclient_native::{Native, ResolveTimedOut};
use hclient_rt_tokio::Tokio;
use hclient_tls_rustls::Rustls;
use std::error::Error as StdError;
use std::net::{IpAddr, Ipv4Addr};
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

/// A resolver whose v4 answer arrives after `0`, and whose v6 answer never
/// comes at all.
///
/// `None` for the duration is *never*: a stream that stays `Pending`
/// forever, which is what a resolver talking to a black hole looks like
/// from in here. `tokio::time::sleep` rather than a thread sleep, for the
/// reason `hooks_timing.rs`' fixture gives one file over — a blocking
/// resolver measures the executor rather than the phase.
#[derive(Clone, Copy)]
struct Answering(Option<Duration>);

impl Resolve for Answering {
    type Records<'a>
        = std::pin::Pin<
        Box<dyn futures_core::Stream<Item = Result<Record, hclient_core::Error>> + Send + 'a>,
    >
    where
        Self: 'a;

    fn supports(&self, rtype: u16) -> bool {
        matches!(rtype, rtype::A | rtype::AAAA)
    }

    fn lookup<'a>(&'a self, name: &str, rtype: u16) -> Self::Records<'a> {
        let _ = name;
        match rtype {
            rtype::A => Box::pin({
                let d = self.0;
                futures_util::stream::once(async move {
                    match d {
                        Some(d) => tokio::time::sleep(d).await,
                        None => std::future::pending::<()>().await,
                    }
                    Ok(Record::new(RData::from(IpAddr::V4(Ipv4Addr::LOCALHOST))))
                })
            }),
            rtype::AAAA => Box::pin(futures_util::stream::empty()),
            _ => Box::pin(futures_util::stream::empty()),
        }
    }
}

/// A resolver that fails both families at once. Not slow: what it tests is
/// that the bound does not turn a precise diagnosis into a vague one.
#[derive(Clone, Copy)]
struct Failing;

impl Resolve for Failing {
    type Records<'a>
        = std::pin::Pin<
        Box<dyn futures_core::Stream<Item = Result<Record, hclient_core::Error>> + Send + 'a>,
    >
    where
        Self: 'a;

    fn supports(&self, rtype: u16) -> bool {
        matches!(rtype, rtype::A | rtype::AAAA)
    }

    fn lookup<'a>(&'a self, name: &str, rtype: u16) -> Self::Records<'a> {
        let _ = name;
        match rtype {
            rtype::A => Box::pin({
                futures_util::stream::once(async {
                    Err(hclient_core::Error::new(
                        ErrorKind::Resolve,
                        std::io::Error::other("no such host"),
                    ))
                })
            }),
            rtype::AAAA => Box::pin(futures_util::stream::empty()),
            _ => Box::pin(futures_util::stream::empty()),
        }
    }
}

fn rt() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime")
}

fn request(port: u16, t: Timeouts) -> http::Request<RequestBody> {
    let mut req = http::Request::builder()
        .uri(format!("http://example.invalid:{port}/x"))
        .body(RequestBody::Empty)
        .expect("request");
    req.extensions_mut().insert(t);
    req
}

/// A server that accepts and answers, so that the *success* arms really do
/// complete rather than failing one phase later for another reason.
fn server() -> u16 {
    let l = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = l.local_addr().expect("addr").port();
    std::thread::spawn(move || {
        for s in l.incoming() {
            let Ok(mut s) = s else { continue };
            use std::io::{Read as _, Write as _};
            let mut buf = [0u8; 2048];
            let _ = s.read(&mut buf);
            let _ = s.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok");
            let _ = s.flush();
        }
    });
    port
}

fn go<D: Resolve + Clone>(dns: D, port: u16, t: Timeouts) -> Result<u16, hclient_core::Error> {
    let transport = Native::new(Tokio, Rustls::with_webpki_roots(), dns);
    rt().block_on(async { transport.execute(request(port, t)).await })
        .map(|r| r.status().as_u16())
}

/// **A resolver that never answers fails as `Timeout(Resolve)`, and the
/// same client with no `resolve` bound does not** — which is the whole
/// point, and needs both halves: without the second, a client that always
/// failed here would pass.
///
/// The control arm is bounded by `connect` instead, so it fails too — but
/// as a *different phase*, which is the distinction a caller could not make
/// before.
#[test]
fn a_resolver_that_never_answers_is_a_resolve_timeout_and_not_a_connect_one() {
    let port = server();

    let err = go(
        Answering(None),
        port,
        Timeouts {
            resolve: Some(Duration::from_millis(150)),
            connect: Some(Duration::from_secs(30)),
            ..Default::default()
        },
    )
    .expect_err("nothing will ever resolve");
    assert_eq!(*err.kind(), ErrorKind::Timeout(Phase::Resolve), "{err:?}");
    let timed_out = StdError::source(&err)
        .and_then(|s| s.downcast_ref::<ResolveTimedOut>())
        .unwrap_or_else(|| panic!("the typed failure, carrying the bound: {err:?}"));
    assert_eq!(timed_out.0, Duration::from_millis(150));

    // The control, and it is the state of the world before this field
    // existed: the same hang, diagnosed as a connect.
    let err = go(
        Answering(None),
        port,
        Timeouts {
            connect: Some(Duration::from_millis(150)),
            ..Default::default()
        },
    )
    .expect_err("nothing will ever resolve");
    assert_eq!(
        *err.kind(),
        ErrorKind::Timeout(Phase::Connect),
        "with no resolve bound the hang is still a connect failure: {err:?}"
    );
}

/// **A resolver that answers inside the bound is untouched**, and the
/// request completes. Without this the test above passes for a bound that
/// fires unconditionally.
#[test]
fn a_resolver_that_answers_in_time_costs_nothing() {
    let port = server();
    let began = Instant::now();
    let status = go(
        Answering(Some(Duration::from_millis(20))),
        port,
        Timeouts {
            resolve: Some(Duration::from_millis(2000)),
            ..Default::default()
        },
    )
    .expect("the resolver answered well inside the bound");
    assert_eq!(status, 200);
    assert!(
        began.elapsed() < Duration::from_millis(1500),
        "and it did not wait out the bound: {:?}",
        began.elapsed()
    );
}

/// **A resolver that fails keeps its own diagnosis.** The bound stops
/// waiting when both families are done rather than sitting out its
/// duration, so a name that does not exist is `ErrorKind::Resolve` and not
/// a timeout — the vague answer this feature exists to avoid, arrived at
/// from the other direction.
#[test]
fn a_resolver_that_fails_reports_the_failure_rather_than_the_bound() {
    let port = server();
    let began = Instant::now();
    let err = go(
        Failing,
        port,
        Timeouts {
            resolve: Some(Duration::from_secs(10)),
            ..Default::default()
        },
    )
    .expect_err("the name does not resolve");
    assert_eq!(*err.kind(), ErrorKind::Resolve, "{err:?}");
    assert!(
        began.elapsed() < Duration::from_secs(5),
        "and it did not wait for a bound whose subject had already answered: {:?}",
        began.elapsed()
    );
}

/// A resolver that never answers an address query but **does** serve an
/// HTTPS record carrying an address hint, on a caller-chosen port.
#[derive(Clone)]
struct Hinting(u16);

impl Resolve for Hinting {
    type Records<'a>
        = std::pin::Pin<
        Box<dyn futures_core::Stream<Item = Result<Record, hclient_core::Error>> + Send + 'a>,
    >
    where
        Self: 'a;

    fn supports(&self, rtype: u16) -> bool {
        matches!(rtype, rtype::A | rtype::AAAA | rtype::HTTPS)
    }

    fn lookup<'a>(&'a self, name: &str, rtype: u16) -> Self::Records<'a> {
        let _ = name;
        match rtype {
            rtype::A => Box::pin(futures_util::stream::once(std::future::pending())),
            rtype::AAAA => Box::pin(futures_util::stream::empty()),
            rtype::HTTPS => Box::pin({
                futures_util::stream::once(std::future::ready(Ok(Record::new(RData::Https(
                    SvcbEndpoint::new(1, "example.invalid".to_string())
                        .port(Some(self.0))
                        .ipv4hint(vec![Ipv4Addr::LOCALHOST]),
                )))))
            }),
            _ => Box::pin(futures_util::stream::empty()),
        }
    }
}

/// **The bound does not apply where the connection does not need the
/// resolver.** An HTTPS record carrying an address hint gives the
/// connector somewhere to go with no address answer at all, so waiting for
/// one would bound a query whose result is not on the path.
///
/// Written because the skip was first offered as a mutation **control** —
/// *"no real record does this"* — and address hints are RFC 9460 §7.3's
/// ordinary case, so it was reachable and untested: a gap, not a control.
///
/// **Asserted causally, on what the server saw.** The exchange cannot
/// succeed — the fixture speaks plaintext and the record's own rule
/// confines discovery to `https` — and after the hinted attempt fails the
/// connector legitimately falls back to the resolver, which here never
/// answers. So the question is not how it ends but whether the fixture was
/// reached at all: with the bound applied to a hint, it never would be.
/// A `connect` bound is set so the fallback terminates rather than hanging,
/// which is the connector behaving correctly and not this test's subject.
#[test]
fn an_address_hint_from_a_record_is_not_made_to_wait_for_the_resolver() {
    let (port, accepts) = counting_server();
    let transport = Native::new(Tokio, Rustls::with_webpki_roots(), Hinting(port));
    let mut req = http::Request::builder()
        .uri("https://example.invalid/x")
        .body(RequestBody::Empty)
        .expect("request");
    req.extensions_mut().insert(Timeouts {
        // A tenth of the connect bound below: if the hint had to wait for
        // the resolver, this would fire first and the fixture would see
        // nothing at all.
        resolve: Some(Duration::from_millis(100)),
        connect: Some(Duration::from_secs(1)),
        ..Default::default()
    });
    let _ = rt().block_on(async { transport.execute(req).await });
    assert!(
        accepts.load(Ordering::SeqCst) >= 1,
        "the hinted address was dialled, which only a connector that did \
         not wait for the resolver could have done"
    );
}

/// [`server`] with a count of the connections it accepted.
fn counting_server() -> (u16, Arc<std::sync::atomic::AtomicUsize>) {
    let l = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = l.local_addr().expect("addr").port();
    let n = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let counter = Arc::clone(&n);
    std::thread::spawn(move || {
        for s in l.incoming() {
            let Ok(mut s) = s else { continue };
            counter.fetch_add(1, Ordering::SeqCst);
            use std::io::{Read as _, Write as _};
            let mut buf = [0u8; 2048];
            let _ = s.read(&mut buf);
            let _ = s.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok");
            let _ = s.flush();
        }
    });
    (port, n)
}
