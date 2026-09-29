//! The default filter: an ordered list of proxy rules, first match wins,
//! plus a Unix-socket policy that matches everything.

use std::net::SocketAddr;
use std::sync::Arc;

use bytes::BytesMut;
use hclient_core::error::Error;

use crate::egress::{
    Attempt, BoxDial, BoxOpening, Decision, Dial, EgressFilter, FilterSupport, Io, Opened,
    RequestForm, Route, SendEgressFilter, Target,
};
use crate::{Approach, Handshake, Proxy, Reach, Step};

/// A [`Handshake`] whose protocol is chosen at run time, so one list can
/// hold HTTP and SOCKS rules together.
///
/// Declares no auto traits; [`BoxHandshake`] demands them where stored.
pub trait DynHandshake: Handshake {
    /// A fresh state machine for one connection.
    fn fresh(&self) -> BoxHandshake;
}

impl<H> DynHandshake for H
where
    H: Handshake + Clone + Send + Sync + 'static, // send-bound-exception: amendment-C16
{
    fn fresh(&self) -> BoxHandshake {
        Box::new(self.clone())
    }
}

/// A handshake of any protocol, as a rule list holds it.
pub type BoxHandshake = Box<dyn DynHandshake + Send + Sync>; // send-bound-exception: amendment-C16

impl Handshake for BoxHandshake {
    fn approach(&self, use_tls: bool) -> Approach {
        (**self).approach(use_tls)
    }
    fn begin(&mut self, host: &str, port: u16) -> Result<bytes::Bytes, Error> {
        (**self).begin(host, port)
    }
    fn advance(&mut self, from_peer: &mut BytesMut) -> Result<Step, Error> {
        (**self).advance(from_peer)
    }
    fn proxy_authorization(&self) -> Option<&http::HeaderValue> {
        (**self).proxy_authorization()
    }
    fn associate(&self) -> Option<crate::Association> {
        (**self).associate()
    }
}

#[derive(Clone)]
// Each rule carries its pool key, computed once when it is pushed:
// `route` is asked several times per request and lends the key out as a
// borrow rather than formatting it again. A proxy rule also carries
// whether it relays datagrams, for the same reason: asking the protocol
// builds an association, which `route` has no business allocating.
enum Rule {
    Proxy(Arc<Proxy<BoxHandshake>>, Box<str>, bool),
    Unix(Arc<hclient_rt::IpcAddr>, Box<str>),
}

/// The default [`EgressFilter`]: proxy rules in order, the first that
/// serves a target carries it, and a target no rule serves is direct.
///
/// First-match-wins rather than most-specific-wins: a precedence rule
/// would have to be learned, where an ordered list is read off the builder
/// chain that wrote it. A bypass belongs to the rule that carries it, so a
/// bypassed target falls through to the next rule rather than going direct.
///
/// A Unix-socket rule ([`unix`](Self::unix)) serves every target, so rules
/// after it are never reached.
#[derive(Clone, Default)]
pub struct Rules {
    rules: Vec<Rule>,
}

impl std::fmt::Debug for Rules {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Rules")
            .field("len", &self.rules.len())
            .finish()
    }
}

impl Rules {
    /// No rules: every request is direct.
    pub fn new() -> Self {
        Self::default()
    }

    /// Append a proxy rule.
    #[must_use]
    pub fn push<P>(mut self, proxy: Proxy<P>) -> Self
    where
        P: Handshake + Clone + Send + Sync + 'static, // send-bound-exception: amendment-C16
    {
        let proxy = proxy.map_protocol(|p| Box::new(p) as BoxHandshake);
        let key = match proxy.reach() {
            Reach::Tcp { host, port } if proxy.is_tls() => format!("tls:{host}:{port}").into(),
            Reach::Tcp { host, port } => format!("{host}:{port}").into(),
            Reach::Ipc(addr) => ipc_key(addr),
        };
        // A relay is a UDP address, so only a proxy reached over the
        // network can name one this host can send to.
        let udp =
            matches!(proxy.reach(), Reach::Tcp { .. }) && proxy.protocol().associate().is_some();
        self.rules.push(Rule::Proxy(Arc::new(proxy), key, udp));
        self
    }

    /// Append a rule for a proxy reached over a same-machine socket.
    ///
    /// Reaching it needs a same-machine dialler, and nothing here can check
    /// that one exists: [`Dial::connect_ipc`](crate::Dial::connect_ipc)
    /// answers `Unsupported` on a runtime that has none, so every request
    /// this rule serves fails. A transport that proves the dialler at
    /// configuration — `hclient_native::Native::proxy_over_ipc` — is the
    /// way to install one where that should be a compile error instead.
    #[must_use]
    pub fn push_ipc<P>(self, proxy: crate::IpcProxy<P>) -> Self
    where
        P: Handshake + Clone + Send + Sync + 'static, // send-bound-exception: amendment-C16
    {
        self.push(proxy.into_proxy())
    }

    /// Append the Unix-socket policy: every request not served by an
    /// earlier rule goes over `addr`.
    #[must_use]
    pub fn unix(mut self, addr: hclient_rt::IpcAddr) -> Self {
        let key = ipc_key(&addr);
        self.rules.push(Rule::Unix(Arc::new(addr), key));
        self
    }

    /// Append every rule of `other`, in its order, after this list's own —
    /// so a rule here is still asked first.
    #[must_use]
    pub fn append(mut self, other: Rules) -> Self {
        self.rules.extend(other.rules);
        self
    }

    /// Whether any rule is installed.
    pub fn is_empty(&self) -> bool {
        self.rules.is_empty()
    }

    fn first(&self, t: &Target<'_>) -> Option<&Rule> {
        self.rules.iter().find(|r| match r {
            Rule::Proxy(p, ..) => p.serves(t.use_tls, t.host, t.port),
            Rule::Unix(..) => true,
        })
    }
}

/// The pool key for a same-machine address: two connections to different
/// sockets must never be interchangeable.
fn ipc_key(addr: &hclient_rt::IpcAddr) -> Box<str> {
    match addr {
        hclient_rt::IpcAddr::Unix(path) => format!("unix:{}", path.display()),
        other => format!("{}:{other:?}", other.kind()),
    }
    .into_boxed_str()
}

impl EgressFilter for Rules {
    /// The built-in protocols only run handshakes over the stream they are
    /// lent; they never wrap it.
    type Wrapped<S: Io> = S;

    fn route(&self, t: &Target<'_>) -> Decision<'_> {
        match self.first(t) {
            None => Decision::Direct,
            Some(Rule::Unix(_, key)) => Decision::Filtered(Route::new(
                FilterSupport::STREAM,
                &**key,
                RequestForm::Origin,
            )),
            Some(Rule::Proxy(p, key, udp)) => {
                let form = match p.protocol().approach(t.use_tls) {
                    Approach::Absolute => {
                        RequestForm::absolute(p.protocol().proxy_authorization().cloned())
                    }
                    Approach::Tunnel => RequestForm::Origin,
                };
                let support = if *udp {
                    FilterSupport::STREAM.with_datagrams()
                } else {
                    FilterSupport::STREAM
                };
                Decision::Filtered(Route::new(support, &**key, form))
            }
        }
    }

    async fn open_stream<'a, C: Dial + 'a>(
        &'a self,
        t: Target<'a>,
        ctx: &'a C,
    ) -> Result<Opened<C::Stream, C::Stream>, Attempt>
    where
        Self: Sized,
    {
        let Some(rule) = self.first(&t) else {
            // The transport only opens what `route` filtered, so this
            // is a transport that did not ask first. Refused rather than
            // opened direct: never around the filter.
            return Err(Attempt::Failed(Error::new(
                hclient_core::error::ErrorKind::Connect,
                std::io::Error::other("no rule serves this target; route answered Direct"),
            )));
        };
        let proxy = match rule {
            Rule::Unix(addr, _) => {
                return ctx
                    .connect_ipc(addr)
                    .await
                    .map(Opened::Raw)
                    .map_err(Attempt::Failed);
            }
            Rule::Proxy(p, ..) => p,
        };
        let mut stream = match proxy.reach() {
            Reach::Tcp { host, port } => reach_tcp(proxy, host, *port, ctx).await?,
            Reach::Ipc(addr) => ctx.connect_ipc(addr).await.map_err(Attempt::Failed)?,
        };
        if proxy.protocol().approach(t.use_tls) == Approach::Absolute {
            return Ok(Opened::Raw(stream));
        }
        let mut h = proxy.protocol().fresh();
        crate::drive_exact(&mut stream, &mut h, t.host, t.port)
            .await
            .map_err(Attempt::Failed)?;
        Ok(Opened::Raw(stream))
    }

    async fn open_datagrams<'a, C: Dial + 'a>(
        &'a self,
        t: Target<'a>,
        ctx: &'a C,
    ) -> Result<crate::BoxPath, Attempt>
    where
        Self: Sized,
        C::Stream: Send + 'static, // send-bound-exception: amendment-C16
    {
        let Some(Rule::Proxy(proxy, _, true)) = self.first(&t) else {
            return Err(Attempt::Unsupported(Error::new(
                hclient_core::error::ErrorKind::Unsupported,
                std::io::Error::other("the rule serving this target carries no datagrams"),
            )));
        };
        let Reach::Tcp { host, port } = proxy.reach() else {
            unreachable!("a rule is marked for datagrams only when reached over TCP")
        };
        let mut control = reach_tcp(proxy, host, *port, ctx).await?;
        let mut a = proxy
            .protocol()
            .associate()
            .expect("a rule is marked for datagrams only when it associates");
        let relay = match crate::socks5_udp::drive_associate(&mut control, &mut a).await {
            Ok(r) => r,
            Err(crate::error::AssociateError::Unsupported(e)) => {
                return Err(Attempt::Unsupported(e));
            }
            Err(crate::error::AssociateError::Failed(e)) => return Err(Attempt::Failed(e)),
        };
        let relay = match relay {
            crate::socks5_udp::RelayAddr::Ip(a) => a,
            // §4: the proxy's own address, which this host already dialled
            // by name — so the proxy's name, never the origin's.
            crate::socks5_udp::RelayAddr::Unspecified(p) => {
                let bare = hclient_core::url::bare_host(host);
                match bare.parse::<std::net::IpAddr>() {
                    Ok(ip) => SocketAddr::new(ip, p),
                    Err(_) => first_addr(ctx.resolve(bare, p).await)?,
                }
            }
            crate::socks5_udp::RelayAddr::Name(n, p) => first_addr(ctx.resolve(&n, p).await)?,
        };
        let local = if relay.is_ipv6() {
            SocketAddr::from((std::net::Ipv6Addr::UNSPECIFIED, 0))
        } else {
            SocketAddr::from((std::net::Ipv4Addr::UNSPECIFIED, 0))
        };
        // Only a runtime that lends no UDP is a refusal to switch on; a
        // socket that would not bind (out of descriptors, a port taken) is
        // this host failing now, which is final and not remembered.
        let udp = ctx.bind_udp(local).map_err(|e| {
            if matches!(e.kind(), hclient_core::error::ErrorKind::Unsupported) {
                Attempt::Unsupported(e)
            } else {
                Attempt::Failed(e)
            }
        })?;
        let header = crate::socks5_udp::header_for(t.host, t.port).map_err(Attempt::Failed)?;
        Ok(crate::BoxPath::new(crate::socks5_udp::Socks5Path::new(
            control, udp, relay, header,
        )))
    }
}

/// Dial a proxy reached over the network, and run TLS to it where it
/// asks for that.
async fn reach_tcp<C: Dial>(
    proxy: &Proxy<BoxHandshake>,
    host: &str,
    port: u16,
    ctx: &C,
) -> Result<C::Stream, Attempt> {
    let s = ctx.connect(host, port).await.map_err(Attempt::Failed)?;
    if !proxy.is_tls() {
        return Ok(s);
    }
    // The proxy was not reached if its handshake failed — a certificate
    // this transport does not trust is not the proxy declining the target.
    // The certificate is checked against the address itself, which a
    // bracketed v6 host names only inside its brackets.
    ctx.connect_tls(s, crate::ProxyTls::new(hclient_core::url::bare_host(host)))
        .await
        .map_err(Attempt::Failed)
}

/// The first address a resolver answered, or a failure naming none.
fn first_addr(r: Result<Vec<SocketAddr>, Error>) -> Result<SocketAddr, Attempt> {
    r.map_err(Attempt::Failed)?
        .into_iter()
        .next()
        .ok_or_else(|| {
            Attempt::Failed(Error::new(
                hclient_core::error::ErrorKind::Resolve,
                std::io::Error::other("the SOCKS5 relay's name resolved to no address"),
            ))
        })
}

// `Rules` is a `SendEgressFilter` so that an engine holding filters erased
// can hold it too. `Native` never uses this impl: it calls `open_stream` on
// `Rules` concretely, which is what keeps the default path unboxed.
impl SendEgressFilter for Rules {
    fn open_stream_send<'a>(&'a self, t: Target<'a>, ctx: &'a BoxDial<'a>) -> BoxOpening<'a> {
        Box::pin(async move { self.open_stream(t, ctx).await.map(crate::erase) })
    }

    fn open_datagrams_send<'a>(
        &'a self,
        t: Target<'a>,
        ctx: &'a BoxDial<'a>,
    ) -> crate::BoxPathOpening<'a> {
        Box::pin(self.open_datagrams(t, ctx))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{HttpConnect, ProxyScheme, ProxySpokeFirst, Socks5};
    use std::io;
    use std::pin::Pin;
    use std::sync::Mutex;
    use std::task::{Context, Poll};

    /// A stream that answers with a fixed reply and records what was written.
    struct Script {
        reply: Vec<u8>,
        at: usize,
        written: Arc<Mutex<Vec<u8>>>,
    }
    impl futures_io::AsyncRead for Script {
        fn poll_read(
            mut self: Pin<&mut Self>,
            _: &mut Context<'_>,
            buf: &mut [u8],
        ) -> Poll<io::Result<usize>> {
            let n = buf.len().min(self.reply.len() - self.at);
            let at = self.at;
            buf[..n].copy_from_slice(&self.reply[at..at + n]);
            self.at += n;
            Poll::Ready(Ok(n))
        }
    }
    impl futures_io::AsyncWrite for Script {
        fn poll_write(
            self: Pin<&mut Self>,
            _: &mut Context<'_>,
            b: &[u8],
        ) -> Poll<io::Result<usize>> {
            self.written.lock().unwrap().extend_from_slice(b);
            Poll::Ready(Ok(b.len()))
        }
        fn poll_flush(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<io::Result<()>> {
            Poll::Ready(Ok(()))
        }
        fn poll_close(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<io::Result<()>> {
            Poll::Ready(Ok(()))
        }
    }
    impl hclient_rt::Shutdown for Script {
        fn poll_shutdown(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<io::Result<()>> {
            Poll::Ready(Ok(()))
        }
    }

    /// Records every dial and answers each with the same scripted reply.
    #[derive(Default)]
    struct Dials {
        reply: Vec<u8>,
        tcp: Mutex<Vec<(String, u16)>>,
        ipc: Mutex<usize>,
        written: Arc<Mutex<Vec<u8>>>,
        /// Server names `connect_tls` was asked for, in order.
        tls: Mutex<Vec<String>>,
        tls_fails: bool,
    }
    impl Dials {
        fn script(&self) -> Script {
            Script {
                reply: self.reply.clone(),
                at: 0,
                written: self.written.clone(),
            }
        }
    }
    impl Dial for Dials {
        type Stream = Script;
        fn connect<'a>(
            &'a self,
            host: &'a str,
            port: u16,
        ) -> impl Future<Output = Result<Script, Error>> + 'a {
            self.tcp.lock().unwrap().push((host.to_owned(), port));
            std::future::ready(Ok(self.script()))
        }
        fn connect_ipc<'a>(
            &'a self,
            _: &'a hclient_rt::IpcAddr,
        ) -> impl Future<Output = Result<Script, Error>> + 'a {
            *self.ipc.lock().unwrap() += 1;
            std::future::ready(Ok(self.script()))
        }
        fn remaining(&self) -> Option<std::time::Duration> {
            None
        }
        fn connect_tls<'a>(
            &'a self,
            stream: Script,
            req: crate::ProxyTls<'a>,
        ) -> impl Future<Output = Result<Script, Error>> + 'a {
            self.tls.lock().unwrap().push(req.server_name.to_owned());
            // A marker in the write log, so a test can see TLS ran before
            // anything the protocol wrote.
            self.written.lock().unwrap().extend_from_slice(b"<tls>");
            std::future::ready(if self.tls_fails {
                Err(Error::new(
                    hclient_core::error::ErrorKind::Tls,
                    io::Error::other("bad certificate"),
                ))
            } else {
                Ok(stream)
            })
        }
    }

    fn t(host: &str, port: u16, use_tls: bool) -> Target<'_> {
        Target::new(host, port, use_tls)
    }

    fn block<F: Future>(f: F) -> F::Output {
        futures_executor::block_on(f)
    }

    fn key<'a>(d: &'a Decision<'a>) -> &'a str {
        match d {
            Decision::Filtered(route) => &route.pool_key,
            Decision::Direct => panic!("expected Filtered, got Direct"),
        }
    }

    #[test]
    fn a_tls_proxy_is_keyed_apart_from_a_plain_one_at_the_same_address() {
        let plain = Rules::new().push(Proxy::new(HttpConnect::new(), "p", 1));
        let tls = Rules::new().push(Proxy::new(HttpConnect::new(), "p", 1).tls());
        assert_eq!(key(&plain.route(&t("a", 443, true))), "p:1");
        assert_eq!(key(&tls.route(&t("a", 443, true))), "tls:p:1");
    }

    #[test]
    fn a_tls_proxy_is_greeted_by_its_own_name_before_the_handshake() {
        let dials = Dials {
            reply: b"HTTP/1.1 200 OK\r\n\r\n".to_vec(),
            ..Default::default()
        };
        let r = Rules::new().push(Proxy::new(HttpConnect::new(), "proxy.test", 8443).tls());
        block(r.open_stream(t("origin.test", 443, true), &dials)).expect("tunnel open");
        assert_eq!(*dials.tls.lock().unwrap(), ["proxy.test"]);
        assert!(
            dials
                .written
                .lock()
                .unwrap()
                .starts_with(b"<tls>CONNECT origin.test:443")
        );
    }

    #[test]
    fn a_bracketed_v6_proxy_is_greeted_by_its_bare_address() {
        let dials = Dials::default();
        let r = Rules::new().push(Proxy::new(HttpConnect::new(), "[::1]", 8443).tls());
        block(r.open_stream(t("origin.test", 80, false), &dials)).expect("connection");
        assert_eq!(*dials.tls.lock().unwrap(), ["::1"]);
    }

    #[test]
    fn absolute_form_through_a_tls_proxy_still_runs_tls() {
        let dials = Dials::default();
        let r = Rules::new().push(Proxy::new(HttpConnect::new(), "proxy.test", 8443).tls());
        block(r.open_stream(t("origin.test", 80, false), &dials)).expect("connection");
        assert_eq!(*dials.tls.lock().unwrap(), ["proxy.test"]);
        assert_eq!(*dials.written.lock().unwrap(), b"<tls>");
    }

    #[test]
    fn a_plain_proxy_runs_no_tls() {
        let dials = Dials {
            reply: b"HTTP/1.1 200 OK\r\n\r\n".to_vec(),
            ..Default::default()
        };
        let r = Rules::new().push(Proxy::new(HttpConnect::new(), "proxy.test", 8080));
        block(r.open_stream(t("origin.test", 443, true), &dials)).expect("tunnel open");
        assert!(dials.tls.lock().unwrap().is_empty());
    }

    #[test]
    fn a_tls_failure_to_the_proxy_fails_with_the_backends_error() {
        let dials = Dials {
            tls_fails: true,
            ..Default::default()
        };
        let r = Rules::new().push(Proxy::new(HttpConnect::new(), "proxy.test", 8443).tls());
        let Err(err) = block(r.open_stream(t("origin.test", 443, true), &dials)) else {
            panic!("the handshake failed");
        };
        match err {
            Attempt::Failed(e) => {
                assert_eq!(*e.kind(), hclient_core::error::ErrorKind::Tls);
            }
            other @ Attempt::Unsupported(_) => panic!("expected Failed, got {other:?}"),
        }
    }

    #[test]
    fn no_rule_is_direct() {
        assert_eq!(
            Rules::new().route(&t("example.com", 443, true)),
            Decision::Direct
        );
    }

    #[test]
    fn a_tunnel_rule_filters_with_todays_pool_key() {
        let r = Rules::new().push(Proxy::new(HttpConnect::new(), "Proxy.Corp", 8080));
        assert_eq!(
            r.route(&t("example.com", 443, true)),
            Decision::Filtered(Route::new(
                FilterSupport::STREAM,
                "Proxy.Corp:8080",
                RequestForm::Origin
            ))
        );
    }

    #[test]
    fn a_rules_pool_key_is_lent_rather_than_formatted_per_call() {
        // A transport asks `route` several times per request; the key was
        // formatted once, when the rule was pushed, and is lent from there.
        let r = Rules::new()
            .push(Proxy::new(HttpConnect::new(), "p", 1).tls())
            .unix(hclient_rt::IpcAddr::Unix("/s".into()));
        let Decision::Filtered(route) = r.route(&t("example.com", 443, true)) else {
            panic!("filtered");
        };
        assert!(matches!(
            route.pool_key,
            std::borrow::Cow::Borrowed("tls:p:1")
        ));
        let only_unix = Rules::new().unix(hclient_rt::IpcAddr::Unix("/s".into()));
        let Decision::Filtered(route) = only_unix.route(&t("example.com", 443, true)) else {
            panic!("filtered");
        };
        assert!(matches!(
            route.pool_key,
            std::borrow::Cow::Borrowed("unix:/s")
        ));
    }

    #[test]
    fn plain_http_through_an_http_proxy_is_absolute_form() {
        let r = Rules::new().push(Proxy::new(HttpConnect::new(), "p", 1));
        assert!(matches!(
            r.route(&t("example.com", 80, false)),
            Decision::Filtered(Route {
                form: RequestForm::Absolute { .. },
                ..
            })
        ));
    }

    #[test]
    fn absolute_form_carries_the_proxys_own_credential() {
        // The rule list holds the handshake erased, and the header is read
        // back through the erasure: a `BoxHandshake` that forgot to forward
        // it would send every request to an authenticating proxy without
        // one, and the proxy's `407` would be the first anybody heard.
        let http = HttpConnect::new().basic_auth("alice", "hunter2").unwrap();
        let want = http.proxy_authorization().cloned();
        assert!(want.is_some());
        let r = Rules::new().push(Proxy::new(http, "p", 1));
        let Decision::Filtered(Route {
            form: RequestForm::Absolute {
                proxy_authorization,
            },
            ..
        }) = r.route(&t("example.com", 80, false))
        else {
            panic!("expected absolute-form");
        };
        assert_eq!(proxy_authorization, want);
    }

    #[test]
    fn the_first_rule_that_serves_wins_and_a_bypass_falls_through() {
        let r = Rules::new()
            .push(Proxy::new(Socks5::new(), "specific", 1080).only_for(ProxyScheme::Https))
            .push(Proxy::new(Socks5::new(), "catch-all", 1080));
        assert_eq!(key(&r.route(&t("example.com", 443, true))), "specific:1080");
        assert_eq!(
            key(&r.route(&t("example.com", 80, false))),
            "catch-all:1080"
        );

        // A bypass on the first rule falls through to the next, which is
        // what makes a per-proxy list right and a global `NO_PROXY` wrong.
        let r = Rules::new()
            .push(
                Proxy::new(Socks5::new(), "first", 1080)
                    .bypass(["example.com"])
                    .unwrap(),
            )
            .push(Proxy::new(Socks5::new(), "second", 1080));
        assert_eq!(key(&r.route(&t("example.com", 443, true))), "second:1080");
    }

    #[test]
    fn an_appended_list_is_asked_after_this_one_and_in_its_own_order() {
        let first = Rules::new()
            .push(Proxy::new(HttpConnect::new(), "https-only", 1).only_for(ProxyScheme::Https));
        let second = Rules::new()
            .push(Proxy::new(HttpConnect::new(), "everything", 2))
            .push(Proxy::new(HttpConnect::new(), "never", 3));
        let r = first.append(second);
        assert_eq!(key(&r.route(&t("example.com", 443, true))), "https-only:1");
        assert_eq!(key(&r.route(&t("example.com", 80, false))), "everything:2");
        assert!(Rules::new().append(Rules::new()).is_empty());
    }

    #[test]
    fn a_list_is_empty_until_a_rule_is_pushed() {
        assert!(Rules::new().is_empty());
        assert!(
            !Rules::new()
                .push(Proxy::new(Socks5::new(), "p", 1))
                .is_empty()
        );
        assert!(
            !Rules::new()
                .unix(hclient_rt::IpcAddr::Unix("/s".into()))
                .is_empty()
        );
    }

    #[test]
    fn a_bypassed_origin_is_direct() {
        let r = Rules::new().push(
            Proxy::new(HttpConnect::new(), "p", 1)
                .bypass(["example.com"])
                .unwrap(),
        );
        assert_eq!(r.route(&t("example.com", 443, true)), Decision::Direct);
    }

    #[test]
    fn rules_of_different_protocols_share_one_list() {
        let r = Rules::new()
            .push(Proxy::new(HttpConnect::new(), "h", 1).only_for(ProxyScheme::Http))
            .push(Proxy::new(Socks5::new(), "s", 2));
        assert_eq!(key(&r.route(&t("a", 80, false))), "h:1");
        assert_eq!(key(&r.route(&t("a", 443, true))), "s:2");
    }

    #[test]
    fn the_unix_policy_matches_everything_and_shadows_later_rules() {
        let r = Rules::new()
            .unix(hclient_rt::IpcAddr::unix("/run/x.sock"))
            .push(Proxy::new(HttpConnect::new(), "p", 1));
        assert_eq!(key(&r.route(&t("a", 443, true))), "unix:/run/x.sock");
        assert_eq!(key(&r.route(&t("a", 80, false))), "unix:/run/x.sock");
    }

    #[test]
    fn a_tunnel_dials_the_proxy_and_drives_its_handshake() {
        let dials = Dials {
            reply: b"HTTP/1.1 200 OK\r\n\r\n".to_vec(),
            ..Default::default()
        };
        let r = Rules::new().push(Proxy::new(HttpConnect::new(), "proxy", 8080));
        let opened =
            block(r.open_stream(t("example.com", 443, true), &dials)).expect("tunnel open");
        assert!(matches!(opened, Opened::Raw(_)));
        assert_eq!(*dials.tcp.lock().unwrap(), [("proxy".to_owned(), 8080)]);
        assert!(
            dials
                .written
                .lock()
                .unwrap()
                .starts_with(b"CONNECT example.com:443")
        );
    }

    #[test]
    fn absolute_form_dials_the_proxy_and_writes_nothing() {
        let dials = Dials::default();
        let r = Rules::new().push(Proxy::new(HttpConnect::new(), "proxy", 8080));
        block(r.open_stream(t("example.com", 80, false), &dials)).expect("connection to the proxy");
        assert_eq!(*dials.tcp.lock().unwrap(), [("proxy".to_owned(), 8080)]);
        assert!(dials.written.lock().unwrap().is_empty());
    }

    #[test]
    fn socks5_over_a_unix_socket_dials_ipc_and_never_tcp() {
        // Greeting reply (no authentication), then a grant with an IPv4
        // bind address — `socks5.rs`'s own `GRANTED`.
        let reply = [
            &[0x05u8, 0x00][..],
            &[0x05, 0x00, 0x00, 0x01, 0, 0, 0, 0, 0, 0],
        ]
        .concat();
        let dials = Dials {
            reply,
            ..Default::default()
        };
        let r = Rules::new().push_ipc(crate::IpcProxy::new(
            Socks5::new(),
            hclient_rt::IpcAddr::unix("/run/tor/socks"),
        ));
        assert_eq!(
            key(&r.route(&t("example.com", 443, true))),
            "unix:/run/tor/socks"
        );
        block(r.open_stream(t("example.com", 443, true), &dials)).expect("tunnel open");
        assert_eq!(*dials.ipc.lock().unwrap(), 1);
        assert!(dials.tcp.lock().unwrap().is_empty());
    }

    /// The source of a failed attempt, which is where *why* travels now
    /// that `Attempt` no longer sorts refusals from outages.
    fn failure(r: &Rules, dials: &Dials) -> Error {
        match block(r.open_stream(t("example.com", 443, true), dials)) {
            Err(Attempt::Failed(e)) => e,
            Err(other @ Attempt::Unsupported(_)) => panic!("expected Failed, got {other:?}"),
            Ok(_) => panic!("expected a failure"),
        }
    }

    #[test]
    fn a_refusing_proxy_fails_with_its_status_on_the_error() {
        let dials = Dials {
            reply: b"HTTP/1.1 403 Forbidden\r\n\r\n".to_vec(),
            ..Default::default()
        };
        let r = Rules::new().push(Proxy::new(HttpConnect::new(), "proxy", 8080));
        let e = failure(&r, &dials);
        let refused = std::error::Error::source(&e)
            .and_then(|s| s.downcast_ref::<crate::ProxyRefused>())
            .expect("the refusal is readable off the error");
        assert_eq!(refused.status, http::StatusCode::FORBIDDEN);
    }

    #[test]
    fn a_socks5_refusal_carries_its_rep() {
        // Greeting accepted, then `REP = 5`, connection refused.
        let reply = [
            &[0x05u8, 0x00][..],
            &[0x05, 0x05, 0x00, 0x01, 0, 0, 0, 0, 0, 0],
        ]
        .concat();
        let dials = Dials {
            reply,
            ..Default::default()
        };
        let r = Rules::new().push(Proxy::new(Socks5::new(), "socks", 1080));
        let e = failure(&r, &dials);
        let refused = std::error::Error::source(&e)
            .and_then(|s| s.downcast_ref::<crate::Socks5Refused>())
            .expect("the refusal is readable off the error");
        assert_eq!(refused.rep, 0x05);
    }

    #[test]
    fn a_proxy_that_hangs_up_mid_handshake_fails_as_a_connect_error() {
        let dials = Dials {
            reply: b"HTTP/1.1 2".to_vec(),
            ..Default::default()
        };
        let r = Rules::new().push(Proxy::new(HttpConnect::new(), "proxy", 8080));
        let e = failure(&r, &dials);
        assert_eq!(*e.kind(), hclient_core::error::ErrorKind::Connect);
        assert!(
            std::error::Error::source(&e)
                .and_then(|s| s.downcast_ref::<crate::ProxyRefused>())
                .is_none(),
            "a hang-up is not a refusal: {e:?}"
        );
    }

    #[test]
    fn bytes_past_the_handshake_fail_the_attempt() {
        let dials = Dials {
            reply: b"HTTP/1.1 200 OK\r\n\r\nextra".to_vec(),
            ..Default::default()
        };
        let r = Rules::new().push(Proxy::new(HttpConnect::new(), "proxy", 8080));
        let err = failure(&r, &dials);
        assert!(
            std::error::Error::source(&err)
                .and_then(|s| s.downcast_ref::<ProxySpokeFirst>())
                .is_some()
        );
    }

    /// A `Dial` for the datagram tests: every connect answers `reply`,
    /// `bind_udp` records where it bound and lends a [`FakeUdp`], and
    /// `resolve` records the name and answers `ip` at the port asked for.
    struct UdpDial {
        reply: Vec<u8>,
        ip: std::net::IpAddr,
        udp: crate::socks5_udp::fake::FakeUdp,
        bound: Mutex<Vec<std::net::SocketAddr>>,
        resolved: Mutex<Vec<String>>,
        /// Where set, `bind_udp` fails with this kind instead of binding.
        bind_fails: Option<hclient_core::error::ErrorKind>,
    }
    impl UdpDial {
        fn new(reply: Vec<u8>, resolves_to: &str) -> Self {
            let a: std::net::SocketAddr = resolves_to.parse().unwrap();
            Self {
                reply,
                ip: a.ip(),
                udp: crate::socks5_udp::fake::FakeUdp::default(),
                bound: Mutex::default(),
                resolved: Mutex::default(),
                bind_fails: None,
            }
        }
        fn udp_sent_to(&self) -> Vec<std::net::SocketAddr> {
            self.udp.sent().into_iter().map(|(to, _)| to).collect()
        }
        fn resolved(&self) -> Vec<String> {
            self.resolved.lock().unwrap().clone()
        }
        fn bound(&self) -> Vec<std::net::SocketAddr> {
            self.bound.lock().unwrap().clone()
        }
    }
    impl Dial for UdpDial {
        type Stream = Script;
        fn connect<'a>(
            &'a self,
            _: &'a str,
            _: u16,
        ) -> impl Future<Output = Result<Script, Error>> + 'a {
            std::future::ready(Ok(Script {
                reply: [&[0x05u8, 0x00][..], &self.reply].concat(),
                at: 0,
                written: Arc::default(),
            }))
        }
        fn connect_ipc<'a>(
            &'a self,
            _: &'a hclient_rt::IpcAddr,
        ) -> impl Future<Output = Result<Script, Error>> + 'a {
            std::future::ready(Err(Error::new(
                hclient_core::error::ErrorKind::Unsupported,
                io::Error::other("no ipc"),
            )))
        }
        fn remaining(&self) -> Option<std::time::Duration> {
            None
        }
        fn bind_udp(&self, local: std::net::SocketAddr) -> Result<crate::BoxUdp, Error> {
            if let Some(kind) = &self.bind_fails {
                return Err(Error::new(kind.clone(), io::Error::other("bind refused")));
            }
            self.bound.lock().unwrap().push(local);
            Ok(crate::BoxUdp::new(self.udp.clone()))
        }
        fn resolve<'a>(
            &'a self,
            host: &'a str,
            port: u16,
        ) -> impl Future<Output = Result<Vec<std::net::SocketAddr>, Error>> + 'a {
            self.resolved.lock().unwrap().push(host.to_owned());
            std::future::ready(Ok(vec![std::net::SocketAddr::new(self.ip, port)]))
        }
    }

    #[test]
    fn a_socks5_rule_with_udp_declares_datagrams_and_one_without_does_not() {
        let t = Target::new("o", 443, true);
        let with = Rules::new().push(Proxy::new(Socks5::new().with_udp(), "px", 1080));
        let Decision::Filtered(r) = with.route(&t) else {
            panic!()
        };
        assert!(r.support.datagrams && r.support.stream);
        let without = Rules::new().push(Proxy::new(Socks5::new(), "px", 1080));
        let Decision::Filtered(r) = without.route(&t) else {
            panic!()
        };
        assert!(!r.support.datagrams);
    }

    #[test]
    fn open_datagrams_associates_binds_and_resolves_an_unspecified_relay_to_the_proxy() {
        // The greeting reply is `UdpDial`'s own; this is the association's.
        let dial = UdpDial::new(
            vec![0x05, 0x00, 0x00, 0x01, 0, 0, 0, 0, 0x1f, 0x90],
            "192.0.2.50:1080",
        );
        let rules = Rules::new().push(Proxy::new(Socks5::new().with_udp(), "px", 1080));
        let path = block(rules.open_datagrams(Target::new("o", 443, true), &dial)).unwrap();
        crate::DatagramPath::try_send(&path, b"q").unwrap();
        assert_eq!(
            dial.udp_sent_to(),
            ["192.0.2.50:8080".parse::<std::net::SocketAddr>().unwrap()]
        );
        assert_eq!(dial.resolved(), ["px"]);
        assert_eq!(
            dial.bound(),
            ["0.0.0.0:0".parse::<std::net::SocketAddr>().unwrap()]
        );
    }

    #[test]
    fn a_proxy_that_does_not_relay_udp_is_unsupported() {
        let dial = UdpDial::new(
            vec![0x05, 0x07, 0x00, 0x01, 0, 0, 0, 0, 0, 0],
            "192.0.2.50:1080",
        );
        let rules = Rules::new().push(Proxy::new(Socks5::new().with_udp(), "px", 1080));
        let a = block(rules.open_datagrams(Target::new("o", 443, true), &dial)).unwrap_err();
        assert!(a.permits_switch());
        assert!(
            dial.bound().is_empty(),
            "no socket is bound for a proxy that refused"
        );
    }

    #[test]
    fn a_socket_that_will_not_bind_is_failed_and_only_an_unsupported_one_switches() {
        let reply = vec![0x05, 0x00, 0x00, 0x01, 192, 0, 2, 50, 0x1f, 0x90];
        let rules = Rules::new().push(Proxy::new(Socks5::new().with_udp(), "px", 1080));

        // Out of descriptors, the port taken: a fault of this host now,
        // not a proxy that cannot carry datagrams — final, not a switch.
        let mut dial = UdpDial::new(reply.clone(), "192.0.2.50:1080");
        dial.bind_fails = Some(hclient_core::error::ErrorKind::Other);
        let a = block(rules.open_datagrams(Target::new("o", 443, true), &dial)).unwrap_err();
        assert!(!a.permits_switch(), "{a:?}");

        // A runtime that lends no UDP at all is the refusal that switches.
        let mut dial = UdpDial::new(reply, "192.0.2.50:1080");
        dial.bind_fails = Some(hclient_core::error::ErrorKind::Unsupported);
        let a = block(rules.open_datagrams(Target::new("o", 443, true), &dial)).unwrap_err();
        assert!(a.permits_switch(), "{a:?}");
    }

    #[test]
    fn a_proxy_that_fails_the_association_is_failed_not_unsupported() {
        let dial = UdpDial::new(
            vec![0x05, 0x02, 0x00, 0x01, 0, 0, 0, 0, 0, 0],
            "192.0.2.50:1080",
        );
        let rules = Rules::new().push(Proxy::new(Socks5::new().with_udp(), "px", 1080));
        let a = block(rules.open_datagrams(Target::new("o", 443, true), &dial)).unwrap_err();
        assert!(!a.permits_switch());
    }

    #[test]
    fn a_named_relay_is_resolved_and_an_ip_relay_is_not() {
        let named = UdpDial::new(
            vec![
                0x05, 0x00, 0x00, 0x03, 5, b'r', b'e', b'l', b'a', b'y', 0x1f, 0x90,
            ],
            "192.0.2.51:1",
        );
        let rules = Rules::new().push(Proxy::new(Socks5::new().with_udp(), "px", 1080));
        let path = block(rules.open_datagrams(Target::new("o", 443, true), &named)).unwrap();
        crate::DatagramPath::try_send(&path, b"q").unwrap();
        assert_eq!(named.resolved(), ["relay"]);
        assert_eq!(
            named.udp_sent_to(),
            ["192.0.2.51:8080".parse::<std::net::SocketAddr>().unwrap()]
        );

        let ip = UdpDial::new(
            vec![
                0x05, 0x00, 0x00, 0x04, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1, 0x1f, 0x90,
            ],
            "192.0.2.51:1",
        );
        let path = block(rules.open_datagrams(Target::new("o", 443, true), &ip)).unwrap();
        crate::DatagramPath::try_send(&path, b"q").unwrap();
        assert!(ip.resolved().is_empty(), "an address needs no resolver");
        assert_eq!(
            ip.bound(),
            ["[::]:0".parse::<std::net::SocketAddr>().unwrap()],
            "a v6 relay is sent to from a v6 socket"
        );
    }

    #[test]
    fn an_unspecified_relay_behind_a_literal_proxy_needs_no_resolver() {
        let dial = UdpDial::new(
            vec![0x05, 0x00, 0x00, 0x01, 0, 0, 0, 0, 0x1f, 0x90],
            "192.0.2.50:1",
        );
        let rules = Rules::new().push(Proxy::new(Socks5::new().with_udp(), "[2001:db8::1]", 1080));
        let path = block(rules.open_datagrams(Target::new("o", 443, true), &dial)).unwrap();
        crate::DatagramPath::try_send(&path, b"q").unwrap();
        assert!(dial.resolved().is_empty());
        assert_eq!(
            dial.udp_sent_to(),
            ["[2001:db8::1]:8080"
                .parse::<std::net::SocketAddr>()
                .unwrap()]
        );
    }

    #[test]
    fn a_rule_without_udp_opens_no_datagram_path() {
        let dial = UdpDial::new(Vec::new(), "192.0.2.50:1080");
        let rules = Rules::new().push(Proxy::new(Socks5::new(), "px", 1080));
        let a = block(rules.open_datagrams(Target::new("o", 443, true), &dial)).unwrap_err();
        assert!(a.permits_switch());
        assert!(dial.bound().is_empty());
    }

    #[test]
    fn an_http_proxy_and_a_unix_rule_declare_no_datagrams() {
        let t = Target::new("o", 443, true);
        let http = Rules::new().push(Proxy::new(HttpConnect::new(), "px", 3128));
        let Decision::Filtered(r) = http.route(&t) else {
            panic!()
        };
        assert!(!r.support.datagrams);
        let unix = Rules::new().unix(hclient_rt::IpcAddr::Unix("/tmp/s".into()));
        let Decision::Filtered(r) = unix.route(&t) else {
            panic!()
        };
        assert!(!r.support.datagrams);
    }

    #[test]
    fn a_socks5_proxy_over_ipc_declares_no_datagrams() {
        // The association's relay is a UDP address; a proxy reached over
        // a same-machine socket has none to offer from here.
        let r = Rules::new().push_ipc(crate::IpcProxy::new(
            Socks5::new().with_udp(),
            hclient_rt::IpcAddr::unix("/run/s"),
        ));
        let Decision::Filtered(route) = r.route(&t("o", 443, true)) else {
            panic!()
        };
        assert!(!route.support.datagrams);
    }

    #[test]
    fn a_boxed_handshake_forwards_its_association() {
        let h: BoxHandshake = Box::new(Socks5::new().with_udp());
        assert!(h.associate().is_some());
        let h: BoxHandshake = Box::new(Socks5::new());
        assert!(h.associate().is_none());
    }
}
