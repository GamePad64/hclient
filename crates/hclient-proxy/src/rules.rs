//! The default filter: an ordered list of proxy rules, first match wins,
//! plus a Unix-socket policy that matches everything.

use std::sync::Arc;

use bytes::BytesMut;
use hclient_core::error::Error;

use crate::egress::{
    Attempt, BoxDial, BoxOpening, Decision, Dial, EgressFilter, FilterSupport, Io, Opened,
    RequestForm, SendEgressFilter, Target,
};
use crate::{Approach, Handshake, Proxy, ProxySpokeFirst, Reach, Step};

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
}

#[derive(Clone)]
enum Rule {
    Proxy(Arc<Proxy<BoxHandshake>>),
    Unix(Arc<hclient_rt::IpcAddr>),
}

/// The default [`EgressFilter`]: proxy rules in order, the first that
/// serves a target carries it, and a target no rule serves is direct.
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
        self.rules.push(Rule::Proxy(Arc::new(proxy)));
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
        self.rules.push(Rule::Unix(Arc::new(addr)));
        self
    }

    /// Whether any rule is installed.
    pub fn is_empty(&self) -> bool {
        self.rules.is_empty()
    }

    fn first(&self, t: &Target<'_>) -> Option<&Rule> {
        self.rules.iter().find(|r| match r {
            Rule::Proxy(p) => p.serves(t.use_tls, t.host, t.port),
            Rule::Unix(_) => true,
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

/// A protocol's own refusal — the proxy declining this target — as
/// opposed to a failure that says the proxy is unusable.
fn is_refusal(e: &Error) -> bool {
    std::error::Error::source(e).is_some_and(|s| {
        s.is::<crate::ProxyRefused>()
            || s.is::<crate::Socks5Refused>()
            || s.is::<crate::Socks4Refused>()
            || s.is::<ProxySpokeFirst>()
    })
}

impl EgressFilter for Rules {
    /// The built-in protocols only run handshakes over the stream they are
    /// lent; they never wrap it.
    type Wrapped<S: Io> = S;

    fn route(&self, t: &Target<'_>) -> Decision {
        match self.first(t) {
            None => Decision::Direct,
            Some(Rule::Unix(addr)) => Decision::Filtered {
                support: FilterSupport::STREAM,
                pool_key: ipc_key(addr),
                form: RequestForm::Origin,
            },
            Some(Rule::Proxy(p)) => {
                let pool_key = match p.reach() {
                    Reach::Tcp { host, port } if p.is_tls() => {
                        format!("tls:{host}:{port}").into_boxed_str()
                    }
                    Reach::Tcp { host, port } => format!("{host}:{port}").into_boxed_str(),
                    Reach::Ipc(addr) => ipc_key(addr),
                };
                let form = match p.protocol().approach(t.use_tls) {
                    Approach::Absolute => RequestForm::Absolute {
                        proxy_authorization: p.protocol().proxy_authorization().cloned(),
                    },
                    Approach::Tunnel => RequestForm::Origin,
                };
                Decision::Filtered {
                    support: FilterSupport::STREAM,
                    pool_key,
                    form,
                }
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
            return Err(Attempt::Refused(Error::new(
                hclient_core::error::ErrorKind::Connect,
                std::io::Error::other("no rule serves this target; route answered Direct"),
            )));
        };
        let proxy = match rule {
            Rule::Unix(addr) => {
                return ctx
                    .connect_ipc(addr)
                    .await
                    .map(Opened::Raw)
                    .map_err(Attempt::Unreachable);
            }
            Rule::Proxy(p) => p,
        };
        let mut stream = match proxy.reach() {
            Reach::Tcp { host, port } => {
                let s = ctx
                    .connect(host, *port)
                    .await
                    .map_err(Attempt::Unreachable)?;
                if proxy.is_tls() {
                    // The proxy was not reached if its handshake failed — a
                    // certificate this transport does not trust is not the
                    // proxy declining the target.
                    // The certificate is checked against the address itself, which a
                    // bracketed v6 host names only inside its brackets.
                    ctx.connect_tls(s, crate::ProxyTls::new(hclient_core::url::bare_host(host)))
                        .await
                        .map_err(Attempt::Unreachable)?
                } else {
                    s
                }
            }
            Reach::Ipc(addr) => ctx.connect_ipc(addr).await.map_err(Attempt::Unreachable)?,
        };
        if proxy.protocol().approach(t.use_tls) == Approach::Absolute {
            return Ok(Opened::Raw(stream));
        }
        let mut h = proxy.protocol().fresh();
        crate::drive_exact(&mut stream, &mut h, t.host, t.port)
            .await
            .map_err(|e| {
                // A protocol's own refusal, or bytes past the handshake, is
                // the proxy declining this target; anything else is the
                // proxy being unusable.
                if is_refusal(&e) {
                    Attempt::Refused(e)
                } else {
                    Attempt::Unreachable(e)
                }
            })?;
        Ok(Opened::Raw(stream))
    }
}

// `Rules` is a `SendEgressFilter` so that an engine holding filters erased
// can hold it too. `Native` never uses this impl: it calls `open_stream` on
// `Rules` concretely, which is what keeps the default path unboxed.
impl SendEgressFilter for Rules {
    fn open_stream_send<'a>(&'a self, t: Target<'a>, ctx: &'a BoxDial<'a>) -> BoxOpening<'a> {
        Box::pin(async move { self.open_stream(t, ctx).await.map(crate::erase) })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{HttpConnect, ProxyScheme, Socks5};
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
        fn connect(
            &self,
            host: &str,
            port: u16,
        ) -> impl Future<Output = Result<Script, Error>> + '_ {
            self.tcp.lock().unwrap().push((host.to_owned(), port));
            std::future::ready(Ok(self.script()))
        }
        fn connect_ipc(
            &self,
            _: &hclient_rt::IpcAddr,
        ) -> impl Future<Output = Result<Script, Error>> + '_ {
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
        Target {
            host,
            port,
            use_tls,
        }
    }

    fn block<F: Future>(f: F) -> F::Output {
        futures_executor::block_on(f)
    }

    fn key(d: &Decision) -> &str {
        match d {
            Decision::Filtered { pool_key, .. } => pool_key,
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
    fn a_tls_failure_to_the_proxy_is_unreachable_not_refused() {
        let dials = Dials {
            tls_fails: true,
            ..Default::default()
        };
        let r = Rules::new().push(Proxy::new(HttpConnect::new(), "proxy.test", 8443).tls());
        let Err(err) = block(r.open_stream(t("origin.test", 443, true), &dials)) else {
            panic!("the handshake failed");
        };
        match err {
            Attempt::Unreachable(e) => {
                assert_eq!(*e.kind(), hclient_core::error::ErrorKind::Tls);
            }
            other => panic!("expected Unreachable, got {other:?}"),
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
            Decision::Filtered {
                support: FilterSupport::STREAM,
                pool_key: "Proxy.Corp:8080".into(),
                form: RequestForm::Origin,
            }
        );
    }

    #[test]
    fn plain_http_through_an_http_proxy_is_absolute_form() {
        let r = Rules::new().push(Proxy::new(HttpConnect::new(), "p", 1));
        assert!(matches!(
            r.route(&t("example.com", 80, false)),
            Decision::Filtered {
                form: RequestForm::Absolute { .. },
                ..
            }
        ));
    }

    #[test]
    fn a_bypassed_origin_is_direct() {
        let r = Rules::new().push(Proxy::new(HttpConnect::new(), "p", 1).bypass(["example.com"]));
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

    #[test]
    fn a_refusing_proxy_is_refused_not_unreachable() {
        let dials = Dials {
            reply: b"HTTP/1.1 403 Forbidden\r\n\r\n".to_vec(),
            ..Default::default()
        };
        let r = Rules::new().push(Proxy::new(HttpConnect::new(), "proxy", 8080));
        assert!(matches!(
            block(r.open_stream(t("example.com", 443, true), &dials)),
            Err(Attempt::Refused(_))
        ));
    }

    #[test]
    fn a_socks5_refusal_is_refused_too() {
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
        assert!(matches!(
            block(r.open_stream(t("example.com", 443, true), &dials)),
            Err(Attempt::Refused(_))
        ));
    }

    #[test]
    fn a_proxy_that_hangs_up_mid_handshake_is_unreachable() {
        let dials = Dials {
            reply: b"HTTP/1.1 2".to_vec(),
            ..Default::default()
        };
        let r = Rules::new().push(Proxy::new(HttpConnect::new(), "proxy", 8080));
        assert!(matches!(
            block(r.open_stream(t("example.com", 443, true), &dials)),
            Err(Attempt::Unreachable(_))
        ));
    }

    #[test]
    fn bytes_past_the_handshake_are_refused() {
        let dials = Dials {
            reply: b"HTTP/1.1 200 OK\r\n\r\nextra".to_vec(),
            ..Default::default()
        };
        let r = Rules::new().push(Proxy::new(HttpConnect::new(), "proxy", 8080));
        // `Refused`, not `Unreachable`: the proxy answered, and what it said
        // is a reason to refuse this target rather than proof it is down.
        let Err(Attempt::Refused(err)) = block(r.open_stream(t("example.com", 443, true), &dials))
        else {
            panic!("leftover bytes are a refusal");
        };
        assert!(
            std::error::Error::source(&err)
                .and_then(|s| s.downcast_ref::<ProxySpokeFirst>())
                .is_some()
        );
    }
}
