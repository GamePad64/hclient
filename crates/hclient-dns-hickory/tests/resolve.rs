//! The `Resolve` impl itself, driven against a scripted upstream.
//!
//! Everything this crate does below `to_endpoint` — which `RecordType` each
//! lookup asks for, the TTL that reaches `ResolvedAddr`, what happens to an
//! answer record that is not an address, how an upstream failure is
//! reported — is only observable through a real hickory `Resolver`. Pointing
//! one at a live server would make these tests depend on outbound DNS, which
//! CI does not have. So the *connection* underneath the resolver is replaced
//! rather than the resolver itself: hickory's own message parsing, caching,
//! CNAME chasing and record filtering all still run, and the only thing
//! faked is the answer that arrives from the wire.

use std::assert_matches;
use futures_util::StreamExt;
use futures_util::stream;
use hclient_core::ErrorKind;
use hclient_dns::{Resolve, ResolvedAddr};
use hclient_dns_hickory::Hickory;
use hickory_resolver::config::{
    ConnectionConfig, NameServerConfig, ResolveHosts, ResolverConfig, ResolverOpts,
};
use hickory_resolver::net::NetError;
use hickory_resolver::net::runtime::TokioRuntimeProvider;
use hickory_resolver::net::xfer::DnsHandle;
use hickory_resolver::proto::op::{DnsRequest, DnsResponse, Message, Query, ResponseCode};
use hickory_resolver::proto::rr::rdata::svcb::{Alpn, SvcParamKey, SvcParamValue};
use hickory_resolver::proto::rr::rdata::{A, AAAA, CNAME, HTTPS, SVCB, TXT};
use hickory_resolver::proto::rr::{Name, RData, Record, RecordType};
use hickory_resolver::{ConnectionProvider, PoolContext, Resolver};
use std::collections::HashMap;
use std::fmt;
use std::future::Future;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::pin::Pin;
use std::str::FromStr;
use std::sync::{Arc, Mutex};
use std::task::Poll;
use std::time::Duration;

const HOST: &str = "example.com.";

fn host() -> Name {
    Name::from_str(HOST).expect("a literal FQDN parses")
}

// ---------------------------------------------------------------- harness

/// What the scripted upstream does when asked for one `RecordType`.
#[derive(Clone, Debug)]
enum Reply {
    /// Answer with exactly these records, in this order.
    Answer(Vec<Record>),
    /// Fail the exchange the way a dead upstream would.
    Fail,
    /// Answer NXDOMAIN: the name does not exist at all. Distinct from the
    /// unscripted default (NOERROR with an empty answer section) — hickory
    /// reports BOTH as `NoRecordsFound`, and they differ only by
    /// `response_code`, which is exactly why the crate tests that field
    /// rather than the variant.
    NxDomain,
    /// Answer SERVFAIL: the server accepted the question and failed to
    /// answer it. Unlike the two above, this never becomes `NoRecordsFound`
    /// at all — hickory turns it into `DnsError::ResponseCode` — which is
    /// what makes it the check that a guard widened to the whole of
    /// `DnsError` would fail.
    ServFail,
    /// Never answer at all. Holds one query open while another runs.
    Silence,
}

/// The scripted upstream: a reply per `RecordType`, plus the log of what was
/// actually asked for. The log is the point — several properties here ("this
/// lookup asks for AAAA, not A") are about the *question*, and a test that
/// only inspected the answer would pass with the question wrong.
#[derive(Debug, Default)]
struct Upstream {
    replies: HashMap<RecordType, Reply>,
    asked: Arc<Mutex<Vec<Query>>>,
}

impl Upstream {
    fn answering(mut self, record_type: RecordType, records: Vec<Record>) -> Self {
        self.replies.insert(record_type, Reply::Answer(records));
        self
    }

    fn failing(mut self, record_type: RecordType) -> Self {
        self.replies.insert(record_type, Reply::Fail);
        self
    }

    fn nxdomain(mut self, record_type: RecordType) -> Self {
        self.replies.insert(record_type, Reply::NxDomain);
        self
    }

    fn servfail(mut self, record_type: RecordType) -> Self {
        self.replies.insert(record_type, Reply::ServFail);
        self
    }

    fn silent(mut self, record_type: RecordType) -> Self {
        self.replies.insert(record_type, Reply::Silence);
        self
    }

    /// Build the resolver under test, and the log of what it asks for.
    fn wire(self) -> (Hickory<Canned>, Asked) {
        let asked = Asked(Arc::clone(&self.asked));
        let provider = Canned {
            runtime: TokioRuntimeProvider::default(),
            script: Arc::new(self),
        };

        let mut options = ResolverOpts::default();
        // `Auto` would consult /etc/hosts, whose contents differ between a
        // developer's machine and CI. Nothing here wants that.
        options.use_hosts_file = ResolveHosts::Never;
        // One attempt: `Fail` must surface as an error rather than being
        // retried against the same scripted upstream first.
        options.attempts = 1;

        // The address is never dialled — `Canned::new_connection` hands back
        // the scripted handle without touching a socket — but a pool with no
        // name servers has nothing to send to.
        let config = ResolverConfig::from_parts(
            None,
            Vec::new(),
            vec![NameServerConfig::udp(IpAddr::V4(Ipv4Addr::LOCALHOST))],
        );

        let resolver = Resolver::builder_with_config(config, provider)
            .with_options(options)
            .build()
            .expect("a resolver over a scripted provider needs no I/O to build");

        (Hickory::new(resolver), asked)
    }
}

/// The queries the upstream was actually sent.
#[derive(Debug)]
struct Asked(Arc<Mutex<Vec<Query>>>);

impl Asked {
    fn record_types(&self) -> Vec<RecordType> {
        self.0
            .lock()
            .expect("no test panics while holding this lock")
            .iter()
            .map(Query::query_type)
            .collect()
    }
}

/// A `ConnectionProvider` whose connections answer from the script.
#[derive(Clone)]
struct Canned {
    runtime: TokioRuntimeProvider,
    script: Arc<Upstream>,
}

/// Hand-written because `TokioRuntimeProvider` is not `Debug`, and the
/// workspace's `missing_debug_implementations` lint reaches test targets too.
impl fmt::Debug for Canned {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Canned")
            .field("script", &self.script)
            .finish_non_exhaustive()
    }
}

impl ConnectionProvider for Canned {
    type Conn = CannedConn;
    type FutureConn = Pin<Box<dyn Future<Output = Result<Self::Conn, NetError>> + Send>>;
    type RuntimeProvider = TokioRuntimeProvider;

    fn new_connection(
        &self,
        _ip: IpAddr,
        _config: &ConnectionConfig,
        _cx: &PoolContext,
    ) -> Result<Self::FutureConn, NetError> {
        let conn = CannedConn {
            script: Arc::clone(&self.script),
        };
        Ok(Box::pin(std::future::ready(Ok(conn))))
    }

    fn runtime_provider(&self) -> &Self::RuntimeProvider {
        &self.runtime
    }
}

#[derive(Clone, Debug)]
struct CannedConn {
    script: Arc<Upstream>,
}

impl CannedConn {
    fn noerror(request: &DnsRequest, query: Query, records: Vec<Record>) -> DnsResponse {
        Self::coded(request, query, ResponseCode::NoError, records)
    }

    fn coded(
        request: &DnsRequest,
        query: Query,
        code: ResponseCode,
        records: Vec<Record>,
    ) -> DnsResponse {
        let mut message = Message::response(request.id, request.op_code);
        message.metadata.response_code = code;
        message.add_query(query);
        message.add_answers(records);
        DnsResponse::from_message(message).expect("a response message re-encodes")
    }
}

impl DnsHandle for CannedConn {
    type Response = Pin<Box<dyn stream::Stream<Item = Result<DnsResponse, NetError>> + Send>>;
    type Runtime = TokioRuntimeProvider;

    fn send(&self, request: DnsRequest) -> Self::Response {
        let query = request.queries.first().cloned().expect("a query was sent");
        self.script
            .asked
            .lock()
            .expect("no test panics while holding this lock")
            .push(query.clone());

        match self.script.replies.get(&query.query_type()).cloned() {
            Some(Reply::Answer(records)) => Box::pin(stream::once(std::future::ready(Ok(
                Self::noerror(&request, query, records),
            )))),
            Some(Reply::Fail) => Box::pin(stream::once(std::future::ready(Err(
                NetError::Message("the scripted upstream is down"),
            )))),
            Some(Reply::NxDomain) => Box::pin(stream::once(std::future::ready(Ok(Self::coded(
                &request,
                query,
                ResponseCode::NXDomain,
                Vec::new(),
            ))))),
            Some(Reply::ServFail) => Box::pin(stream::once(std::future::ready(Ok(Self::coded(
                &request,
                query,
                ResponseCode::ServFail,
                Vec::new(),
            ))))),
            Some(Reply::Silence) => Box::pin(stream::pending()),
            // Nothing scripted for this type: NOERROR with an empty answer
            // section, which is what a server says for "asked, found none".
            None => Box::pin(stream::once(std::future::ready(Ok(Self::noerror(
                &request,
                query,
                Vec::new(),
            ))))),
        }
    }
}

fn a_record(ttl: u32, addr: Ipv4Addr) -> Record {
    Record::from_rdata(host(), ttl, RData::A(A(addr)))
}

fn aaaa_record(ttl: u32, addr: Ipv6Addr) -> Record {
    Record::from_rdata(host(), ttl, RData::AAAA(AAAA(addr)))
}

fn https_record(ttl: u32, svcb: SVCB) -> Record {
    Record::from_rdata(host(), ttl, RData::HTTPS(HTTPS(svcb)))
}

const V6: Ipv6Addr = Ipv6Addr::new(0x2001, 0xdb8, 0, 0, 0, 0, 0, 1);

// ------------------------------------------------------------------ tests

#[tokio::test]
async fn a_v4_lookup_asks_for_a_and_a_v6_lookup_asks_for_aaaa() {
    let (resolve, asked) = Upstream::default()
        .answering(
            RecordType::A,
            vec![a_record(60, Ipv4Addr::new(192, 0, 2, 1))],
        )
        .wire();

    let _ = resolve.lookup_ipv4(HOST).collect::<Vec<_>>().await;
    assert_eq!(
        asked.record_types(),
        vec![RecordType::A],
        "lookup_ipv4 must put A on the wire — a swapped record type would still \
         produce a plausible-looking stream, just of the other family"
    );

    let (resolve, asked) = Upstream::default()
        .answering(RecordType::AAAA, vec![aaaa_record(60, V6)])
        .wire();

    let _ = resolve.lookup_ipv6(HOST).collect::<Vec<_>>().await;
    assert_eq!(asked.record_types(), vec![RecordType::AAAA]);
}

#[tokio::test]
async fn each_address_carries_the_ttl_of_its_own_record() {
    // Deliberately unequal, and deliberately not ascending: an implementation
    // that took the RRset's minimum, its maximum, or the cache's own
    // `valid_until` would agree with a single-TTL fixture.
    let (resolve, _) = Upstream::default()
        .answering(
            RecordType::A,
            vec![
                a_record(900, Ipv4Addr::new(192, 0, 2, 1)),
                a_record(30, Ipv4Addr::new(192, 0, 2, 2)),
            ],
        )
        .wire();

    let got: Vec<_> = resolve
        .lookup_ipv4(HOST)
        .map(|r| r.expect("the scripted answer is a success"))
        .collect()
        .await;

    assert_eq!(
        got,
        vec![
            ResolvedAddr {
                addr: IpAddr::V4(Ipv4Addr::new(192, 0, 2, 1)),
                ttl: Some(Duration::from_secs(900)),
            },
            ResolvedAddr {
                addr: IpAddr::V4(Ipv4Addr::new(192, 0, 2, 2)),
                ttl: Some(Duration::from_secs(30)),
            },
        ],
        "the TTL is the record's own, not the RRset's minimum and not a constant"
    );
}

#[tokio::test]
async fn an_answer_record_that_is_not_an_address_is_skipped_not_an_error() {
    // A CNAME chain, which is what a real answer for an aliased host looks
    // like: hickory keeps the CNAME in the answer section, and this crate has
    // to walk past it rather than fail the whole lookup.
    let alias = Name::from_str("alias.example.net.").expect("a literal FQDN parses");
    let (resolve, _) = Upstream::default()
        .answering(
            RecordType::A,
            vec![
                Record::from_rdata(host(), 60, RData::CNAME(CNAME(alias.clone()))),
                Record::from_rdata(alias, 60, RData::A(A(Ipv4Addr::new(192, 0, 2, 7)))),
            ],
        )
        .wire();

    let got: Vec<_> = resolve.lookup_ipv4(HOST).collect().await;
    assert_eq!(got.len(), 1, "the CNAME is skipped, the A is kept");
    let addr = got
        .into_iter()
        .next()
        .expect("length was just asserted")
        .expect("a CNAME in the answer section is not a failure");
    assert_eq!(addr.addr, IpAddr::V4(Ipv4Addr::new(192, 0, 2, 7)));
}

#[tokio::test]
async fn an_upstream_failure_arrives_as_error_kind_resolve() {
    let (resolve, _) = Upstream::default().failing(RecordType::A).wire();

    let got: Vec<_> = resolve.lookup_ipv4(HOST).collect().await;
    assert_eq!(got.len(), 1, "a failed lookup yields exactly one error");
    let err = got
        .into_iter()
        .next()
        .expect("length was just asserted")
        .expect_err("the scripted upstream failed");
    assert_matches!(
        err.kind(),
        ErrorKind::Resolve,
        "a DNS failure must not be reported as any other kind"
    );
}

#[tokio::test]
async fn a_v6_lookup_completes_while_the_v4_lookup_is_still_outstanding() {
    // RFC 8305 §3: IPv6 attempts start without waiting for the IPv4 answer.
    // The two lookups being separate streams is what makes that possible, so
    // this holds the A query open forever and requires the AAAA one to finish
    // anyway. An implementation that merged the two, or that made either wait
    // on the other, hangs here instead of failing an assertion — hence the
    // timeout rather than a bare await.
    let (resolve, _) = Upstream::default()
        .silent(RecordType::A)
        .answering(RecordType::AAAA, vec![aaaa_record(60, V6)])
        .wire();

    let mut v4 = Box::pin(resolve.lookup_ipv4(HOST));
    let mut v6 = Box::pin(resolve.lookup_ipv6(HOST));

    // Start the A query and leave it hanging.
    assert_matches!(
        futures_util::poll!(v4.next()),
        Poll::Pending,
        "the scripted upstream never answers A, so this lookup cannot be done"
    );

    let first_v6 = tokio::time::timeout(Duration::from_secs(5), v6.next())
        .await
        .expect("the AAAA lookup must not be gated on the unanswered A lookup")
        .expect("the scripted answer has one AAAA record")
        .expect("the scripted answer is a success");
    assert_eq!(first_v6.addr, IpAddr::V6(V6));
}

#[tokio::test]
async fn svcb_support_is_claimed_and_backed_by_a_real_https_query() {
    // `Resolve`'s own doc requires these two to move together: a resolver
    // that answers SVCB must also say so, and one that says so must ask. A
    // test that checked only the boolean would pass for an implementation
    // that had quietly inherited the default empty-stream `lookup_svcb`.
    let (resolve, asked) = Upstream::default()
        .answering(
            RecordType::HTTPS,
            vec![https_record(
                60,
                SVCB::new(
                    1,
                    Name::from_str("svc.example.net.").expect("a literal FQDN parses"),
                    vec![(
                        SvcParamKey::Alpn,
                        SvcParamValue::Alpn(Alpn(vec!["h3".to_owned()])),
                    )],
                ),
            )],
        )
        .wire();

    assert!(resolve.supports_svcb());

    let got: Vec<_> = resolve
        .lookup_svcb(HOST)
        .map(|r| r.expect("the scripted answer is a success"))
        .collect()
        .await;

    assert_eq!(
        asked.record_types(),
        vec![RecordType::HTTPS],
        "the query is HTTPS (type 65), not SVCB (type 64) — a web origin publishes \
         the former, and asking for the latter finds nothing"
    );
    assert_eq!(got.len(), 1);
    assert_eq!(got[0].target, "svc.example.net.");
    assert_eq!(got[0].alpn, vec![b"h3".to_vec()]);
}

#[tokio::test]
async fn support_for_svcb_survives_a_lookup_that_finds_nothing() {
    // `supports_svcb()` answers "can this resolver ask?", never "did the last
    // answer contain anything?". It is a constant here for exactly that
    // reason, and this pins that a fruitless lookup does not touch it.
    let (resolve, asked) = Upstream::default().wire();

    let _ = resolve.lookup_svcb(HOST).collect::<Vec<_>>().await;

    assert_eq!(
        asked.record_types(),
        vec![RecordType::HTTPS],
        "the lookup really went to the wire — it did not short-circuit"
    );
    assert!(
        resolve.supports_svcb(),
        "finding no records must not downgrade the claim; that distinction is \
         the whole reason the flag exists"
    );
}

/// NODATA is an answer, and it arrives as an empty stream.
///
/// hickory does not return an empty `Lookup` for "the name exists but has
/// no record of this type": it returns `NoRecordsFound` with
/// `response_code: NoError`. This crate mapped that, like any other
/// resolver error, to `ErrorKind::Resolve` — so a host with no HTTPS
/// record, which is nearly every host on today's web, yielded a failure
/// instead of nothing.
///
/// `hclient-dns` states the convention outright in `IpLiteralOnly`'s doc: a
/// family with nothing in it "produces an empty stream rather than an
/// error, because ... erroring there would make every literal connection
/// report a failure it did not have."
#[tokio::test]
async fn no_https_record_yields_an_empty_stream_not_an_error() {
    let (resolve, _) = Upstream::default().wire();

    let got: Vec<_> = resolve.lookup_svcb(HOST).collect().await;
    assert!(
        got.is_empty(),
        "NODATA is 'asked, found none' — an empty stream, not a failure"
    );
}

/// The same on the address lookups, where it cost more.
///
/// A v4-only host answers NOERROR with an empty answer section for AAAA.
/// While that arrived as `ErrorKind::Resolve`, `hclient-native`'s
/// `ResolveErrors` recorded a v6 failure on every request to such a host —
/// and when the v4 attempts then failed for their own reasons, the reported
/// cause blamed a resolver that had worked.
#[tokio::test]
async fn a_family_with_no_records_yields_an_empty_stream_not_an_error() {
    let (resolve, _) = Upstream::default()
        .answering(
            RecordType::A,
            vec![a_record(60, Ipv4Addr::new(192, 0, 2, 1))],
        )
        .wire();

    let got: Vec<_> = resolve.lookup_ipv6(HOST).collect().await;
    assert!(
        got.is_empty(),
        "a v4-only host has no AAAA, and that is an answer, not a failure"
    );
    let v4: Vec<_> = resolve.lookup_ipv4(HOST).collect().await;
    assert_eq!(
        v4.len(),
        1,
        "the family that does have records is unaffected"
    );
}

/// Which lookup a case drives.
///
/// The guard is written TWICE — once in `lookup_ips`, once in `lookup_svcb`
/// — so it can be right in one and wrong in the other. Checking a response
/// code against only one lookup cannot see that, so the two tests below run
/// every code past all three entry points.
#[derive(Debug, Clone, Copy)]
enum Lookup {
    V4,
    V6,
    Svcb,
}

impl Lookup {
    const ALL: [(Self, RecordType); 3] = [
        (Self::V4, RecordType::A),
        (Self::V6, RecordType::AAAA),
        (Self::Svcb, RecordType::HTTPS),
    ];

    /// Drive the lookup and flatten to a shape the three share. The items
    /// differ in type — `ResolvedAddr` against `SvcbEndpoint` — and nothing
    /// here cares about their contents, only about how many arrived and
    /// whether they were errors.
    async fn run(self, resolve: &Hickory<Canned>) -> Vec<Result<(), hclient_core::Error>> {
        match self {
            Self::V4 => {
                resolve
                    .lookup_ipv4(HOST)
                    .map(|r| r.map(drop))
                    .collect()
                    .await
            }
            Self::V6 => {
                resolve
                    .lookup_ipv6(HOST)
                    .map(|r| r.map(drop))
                    .collect()
                    .await
            }
            Self::Svcb => {
                resolve
                    .lookup_svcb(HOST)
                    .map(|r| r.map(drop))
                    .collect()
                    .await
            }
        }
    }

    /// The single error this lookup must have produced.
    fn sole_error(got: Vec<Result<(), hclient_core::Error>>, what: Self) -> hclient_core::Error {
        assert_eq!(
            got.len(),
            1,
            "{what:?}: expected exactly one item, a failure"
        );
        got.into_iter()
            .next()
            .expect("length was just asserted")
            .expect_err("this response code is a failure, not an answer")
    }
}

/// The other side of the same guard, and the reason it tests
/// `response_code` rather than the `NoRecordsFound` variant alone.
///
/// NXDOMAIN arrives as `NoRecordsFound` too — hickory-net 0.26's
/// `DnsError::from_response` folds `NXDomain | NoError if !contains_answer()
/// && !truncation` into the one variant — but the name does not exist, which
/// is a real failure. A guard matching the variant alone swallows it, and
/// that is the mirror defect: "asked and found nothing" reported for a
/// domain that is not there at all, leaving a caller to retry a name that
/// can never resolve.
#[tokio::test]
async fn nxdomain_stays_an_error_rather_than_an_empty_stream() {
    for (lookup, queried) in Lookup::ALL {
        let (resolve, _) = Upstream::default().nxdomain(queried).wire();

        let err = Lookup::sole_error(lookup.run(&resolve).await, lookup);
        assert_matches!(
            err.kind(),
            ErrorKind::Resolve,
            "{lookup:?}: a nonexistent name is a failure, not nothing"
        );
    }
}

/// SERVFAIL never reaches the NODATA guard at all: hickory turns it into
/// `DnsError::ResponseCode`, a different variant from `NoRecordsFound`.
///
/// That makes it work today by construction rather than by check — which is
/// exactly why it needs one. Widening the guard to the whole of `DnsError`
/// would compile, would read as a simplification, and would report a broken
/// server as "this name has nothing of this type." Nothing else in this file
/// would notice.
#[tokio::test]
async fn servfail_stays_an_error_because_a_failed_server_is_not_an_empty_name() {
    for (lookup, queried) in Lookup::ALL {
        let (resolve, _) = Upstream::default().servfail(queried).wire();

        let err = Lookup::sole_error(lookup.run(&resolve).await, lookup);
        assert_matches!(
            err.kind(),
            ErrorKind::Resolve,
            "{lookup:?}: a server that failed is not a name that has nothing"
        );
    }
}

/// The NODATA side, run past all three lookups for the same reason: the
/// guard that makes it an empty stream is written once per `flat_map`.
#[tokio::test]
async fn nodata_is_an_empty_stream_on_every_lookup() {
    for (lookup, queried) in Lookup::ALL {
        // Scripting nothing for the type yields NOERROR with an empty answer
        // section, which is what a server says for "asked, found none".
        let (resolve, asked) = Upstream::default().wire();

        let got = lookup.run(&resolve).await;
        assert!(
            got.is_empty(),
            "{lookup:?}: NODATA is an answer, not a failure"
        );
        assert_eq!(
            asked.record_types(),
            vec![queried],
            "{lookup:?}: empty because it asked and got nothing, not because \
             it never asked"
        );
    }
}

#[tokio::test]
async fn a_non_https_answer_record_does_not_become_an_endpoint() {
    let (resolve, _) = Upstream::default()
        .answering(
            RecordType::HTTPS,
            vec![
                Record::from_rdata(host(), 60, RData::TXT(TXT::new(vec!["v=spf1".to_owned()]))),
                https_record(
                    60,
                    SVCB::new(
                        2,
                        Name::from_str("svc.example.net.").expect("a literal FQDN parses"),
                        Vec::new(),
                    ),
                ),
            ],
        )
        .wire();

    let got: Vec<_> = resolve
        .lookup_svcb(HOST)
        .map(|r| r.expect("the scripted answer is a success"))
        .collect()
        .await;
    assert_eq!(got.len(), 1, "the TXT record is skipped, the HTTPS is kept");
    assert_eq!(got[0].priority, 2);
}

#[tokio::test]
async fn an_upstream_failure_on_the_https_query_arrives_as_error_kind_resolve() {
    let (resolve, _) = Upstream::default().failing(RecordType::HTTPS).wire();

    let got: Vec<_> = resolve.lookup_svcb(HOST).collect().await;
    assert_eq!(got.len(), 1, "a failed lookup yields exactly one error");
    let err = got
        .into_iter()
        .next()
        .expect("length was just asserted")
        .expect_err("the scripted upstream failed");
    assert_matches!(err.kind(), ErrorKind::Resolve);
}

#[tokio::test]
async fn clones_share_one_resolver_rather_than_multiplying_upstream_traffic() {
    // `Clone` here is hand-written precisely so a clone shares the `Arc`, and
    // so shares one cache. A derive-shaped clone that rebuilt the resolver
    // would look identical from the outside except in this one respect: the
    // upstream would be asked again for a question already answered.
    let (resolve, asked) = Upstream::default()
        .answering(
            RecordType::A,
            vec![a_record(600, Ipv4Addr::new(192, 0, 2, 1))],
        )
        .wire();
    let twin = resolve.clone();

    let first: Vec<_> = resolve.lookup_ipv4(HOST).collect().await;
    let second: Vec<_> = twin.lookup_ipv4(HOST).collect().await;

    assert_eq!(first.len(), 1, "the original resolved the name");
    assert_eq!(second.len(), 1, "so did the clone");
    assert_eq!(
        asked.record_types(),
        vec![RecordType::A],
        "one query for two lookups — the clone reads the cache the original filled"
    );
    // `get_ref` is exercised here rather than on its own: an accessor that
    // returns the crate's single field has no wrong answer to return, so a
    // standalone test for it could not be made to fail. What it is good for
    // is stating the sharing structurally, next to the behaviour above.
    assert!(
        std::ptr::eq(resolve.get_ref(), twin.get_ref()),
        "one resolver seen twice, not two that happen to agree"
    );
    assert_eq!(
        resolve.get_ref().options().attempts,
        1,
        "and it is the resolver `wire` built, which set exactly this"
    );
}

/// The module doc's design claim, pinned where it can be seen: `Hickory` is
/// generic over `P: ConnectionProvider`, so a provider that is not hickory's
/// tokio one plugs in without a change to this crate. `Canned` is such a
/// provider, and it is what every test above runs through.
///
/// The bound below is what carries the weight. Narrowing the crate's
/// `impl<P: ConnectionProvider> Resolve for Hickory<P>` to one concrete `P`
/// would fail to compile here, which is the failure this test is for — the
/// tokio dependency the module doc admits to is a *feature* choice, and this
/// is the line between that and a design that could not be undone.
#[tokio::test]
async fn the_resolve_impl_is_generic_over_any_connection_provider() {
    async fn resolves_over_any_provider<P: ConnectionProvider>(h: &Hickory<P>) -> usize {
        // Through the trait, not through `Hickory`'s inherent methods: only a
        // blanket impl satisfies a call made behind this bound.
        assert!(Resolve::supports_svcb(h));
        Resolve::lookup_ipv4(h, HOST).count().await
    }

    let (resolve, _) = Upstream::default()
        .answering(
            RecordType::A,
            vec![a_record(60, Ipv4Addr::new(192, 0, 2, 1))],
        )
        .wire();
    assert_eq!(resolves_over_any_provider(&resolve).await, 1);
}
