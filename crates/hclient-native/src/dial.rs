//! [`NativeDial`]: what `Native` lends an egress filter — its own way of
//! reaching a host by name, a same-machine socket, and what is left of
//! the request's connect bound.

use std::marker::PhantomData;
use std::sync::Mutex;
use std::time::Duration;

use hclient_core::error::{Error, ErrorKind};
use hclient_core::hooks::Hooks;
use hclient_dns::Resolve;
use hclient_rt::{TcpConnect, TcpOpts, Timer};
use hclient_tls::TlsConnect;

use crate::DialIpc;
use crate::connect::Attempted;

/// How long an HTTP/3 tunnel attempt may take before HTTP/2 is tried in
/// its place, where the request will take either.
///
/// A proxy that does not speak QUIC gives no answer at all, so this is a
/// guess at how long a proxy that does would take: the earliest honest
/// sign of a lost first flight is the probe timeout off QUIC's guessed
/// 333 ms initial round trip, about a second, and half a second more lets
/// a retransmitted handshake finish on an ordinary path.
const H3_TUNNEL_BEFORE_H2: Duration = Duration::from_millis(1500);

/// The key a proxy's failed HTTP/3 tunnels are remembered under: its
/// authority, the host without the brackets a v6 literal is written in.
#[cfg(feature = "http3")]
fn tunnel_origin(req: &hclient_proxy::TunnelRequest<'_>) -> crate::altsvc_cache::Origin {
    crate::altsvc_cache::Origin::new(hclient_core::url::bare_host(req.proxy_host), req.proxy_port)
}

/// What stands in for the QUIC arm without the `http3` feature: nothing,
/// and nothing can be made of it.
#[cfg(not(feature = "http3"))]
enum NoArm {}

#[cfg(not(feature = "http3"))]
impl NoArm {
    /// The `http3` arm's signature, with a body that cannot run.
    fn tunnel_boxed(
        &self,
        _req: hclient_proxy::TunnelRequest<'_>,
        _budget: Option<Duration>,
    ) -> std::future::Ready<Result<hclient_proxy::Tunnel, Error>> {
        match *self {}
    }
}

/// The QUIC arm a context opens HTTP/3 tunnels through, and the memory of
/// the proxies whose HTTP/3 tunnels have failed.
#[cfg(feature = "http3")]
pub(crate) struct LentH3<'a, R: Timer> {
    pub(crate) arm: &'a crate::http3::arm::Arm,
    /// Keyed by the proxy's authority; read only where HTTP/2 can follow,
    /// so a tunnel that will take HTTP/3 alone still tries it.
    pub(crate) failures: &'a crate::failures::H3Failures,
    /// What the memory's windows are measured from, on the transport's
    /// clock.
    pub(crate) epoch: R::Instant,
}

#[cfg(feature = "http3")]
impl<R: Timer> Clone for LentH3<'_, R> {
    fn clone(&self) -> Self {
        *self
    }
}

#[cfg(feature = "http3")]
impl<R: Timer> Copy for LentH3<'_, R> {}

/// `Native`'s [`hclient_proxy::Dial`], built per connection.
///
/// Its futures are `impl Future`, so a filter called concretely keeps
/// whatever auto traits the runtime's and resolver's futures have: a
/// `Send` runtime gives a `Send` connect, and a `!Send` one (embassy) is
/// asked for nothing.
pub(crate) struct NativeDial<'a, R: TcpConnect + Timer, D: ?Sized, L, H> {
    rt: &'a R,
    dns: &'a D,
    /// The transport's TLS backend, which [`connect_tls`](hclient_proxy::Dial::connect_tls)
    /// runs over a lent stream.
    tls: &'a L,
    opts: &'a TcpOpts,
    ipc: Option<DialIpc<R>>,
    /// How this transport binds a UDP socket of its own runtime, when
    /// [`Native::http3`](crate::Native::http3) has installed one. `None`
    /// refuses [`bind_udp`](hclient_proxy::Dial::bind_udp) naming that
    /// constructor, exactly as a missing `ipc` refuses
    /// [`connect_ipc`](hclient_proxy::Dial::connect_ipc).
    udp: Option<crate::BindUdp<R>>,
    /// How this transport opens an HTTP/2 tunnel to a proxy over a
    /// connection this context opened, when [`Native::egress`](crate::Native::egress)
    /// installed the filter it is lent to. `None` refuses
    /// [`connect_tunnel`](hclient_proxy::Dial::connect_tunnel) before any
    /// socket is opened.
    tunnel_h2: Option<crate::TunnelH2<R, L>>,
    /// The QUIC arm an HTTP/3 tunnel to a proxy is opened through, when
    /// [`Native::http3`](crate::Native::http3) installed one and the filter
    /// this context is lent was installed with [`Native::egress`](crate::Native::egress).
    /// `None` refuses a tunnel that will take HTTP/3 alone before any
    /// packet is sent.
    #[cfg(feature = "http3")]
    tunnel_h3: Option<LentH3<'a, R>>,
    budget: Option<Duration>,
    /// When the context was lent, on the transport's clock — what
    /// [`remaining`](hclient_proxy::Dial::remaining) counts the budget down
    /// from.
    lent: R::Instant,
    began: Option<R::Instant>,
    /// What the last [`connect`](hclient_proxy::Dial::connect) reported
    /// for the hooks, taken by the caller that emits `Connected`. A
    /// `Mutex` rather than a `Cell` so the context stays `Sync`, which an
    /// erased filter's `SharedDial` needs.
    attempted: Mutex<Option<Box<Attempted>>>,
    _h: PhantomData<fn() -> H>,
}

impl<'a, R: TcpConnect + Timer, D: ?Sized, L, H> NativeDial<'a, R, D, L, H> {
    #[expect(
        clippy::too_many_arguments,
        reason = "every argument is one of what `Native` lends a connection; splitting the context into a second type would only move the count"
    )]
    pub(crate) fn new(
        rt: &'a R,
        dns: &'a D,
        tls: &'a L,
        opts: &'a TcpOpts,
        ipc: Option<DialIpc<R>>,
        budget: Option<Duration>,
        began: Option<R::Instant>,
        udp: Option<crate::BindUdp<R>>,
    ) -> Self {
        Self {
            rt,
            dns,
            tls,
            opts,
            ipc,
            udp,
            tunnel_h2: None,
            #[cfg(feature = "http3")]
            tunnel_h3: None,
            budget,
            lent: rt.now(),
            began,
            attempted: Mutex::new(None),
            _h: PhantomData,
        }
    }

    /// Lend HTTP/2 tunnels through `open`, or none.
    pub(crate) fn with_tunnel_h2(mut self, open: Option<crate::TunnelH2<R, L>>) -> Self {
        self.tunnel_h2 = open;
        self
    }

    /// Lend HTTP/3 tunnels through `arm`, or none.
    #[cfg(feature = "http3")]
    pub(crate) fn with_tunnel_h3(mut self, lent: Option<LentH3<'a, R>>) -> Self {
        self.tunnel_h3 = lent;
        self
    }

    /// What the last connect by name reported for the hooks, if anything.
    pub(crate) fn take_attempted(&self) -> Option<Box<Attempted>> {
        self.attempted
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
    }
}

impl<R, D, L, H> NativeDial<'_, R, D, L, H>
where
    R: TcpConnect + Timer,
    D: Resolve + ?Sized,
    L: TlsConnect,
    H: Hooks,
{
    /// [`Dial::connect`](hclient_proxy::Dial::connect)'s socket, before it
    /// is wrapped — what the erased path boxes directly.
    async fn connect_raw(&self, host: &str, port: u16) -> Result<R::Stream, Error> {
        let (stream, attempted) = crate::connect::dial_by_name::<R, D, H>(
            self.rt, self.dns, host, port, self.opts, self.began,
        )
        .await?;
        *self
            .attempted
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = attempted;
        Ok(stream)
    }

    /// [`Dial::connect_tunnel`](hclient_proxy::Dial::connect_tunnel), for
    /// both the concrete and the erased context.
    ///
    /// HTTP/3 through the QUIC arm where the request will take it and the
    /// arm is lent; HTTP/2 over a connection this context opens where the
    /// request will take that. A request that prefers HTTP/3 and will take
    /// HTTP/2 goes over HTTP/2 when the HTTP/3 attempt fails for any reason
    /// and an HTTP/2 tunnel is lent — no tunnel was handed out, so nothing
    /// the filter sends is sent twice — and the HTTP/3 error stands when it
    /// is not. Refusals that need no socket come before one is opened.
    ///
    /// Where HTTP/2 can follow, the HTTP/3 attempt is bounded so HTTP/2
    /// keeps a real share: [`H3_TUNNEL_BEFORE_H2`] with no connect bound,
    /// and the lesser of that and half of what is left with one. A proxy
    /// with nothing listening on UDP answers QUIC with silence, not a
    /// refusal, so without this bound the fallback would wait for QUIC's
    /// idle timeout — or spend the whole connect bound — first. Where
    /// HTTP/3 is all the request will take, it gets everything that is left.
    ///
    /// A proxy whose HTTP/3 tunnel failed is remembered for as long as a
    /// failed QUIC connect to an origin is, and while it is, a request that
    /// will take HTTP/2 goes straight there; `Native::network_changed`
    /// forgets it. A request that will take HTTP/3 alone never reads the
    /// memory.
    async fn connect_tunnel_raw<'b>(
        &'b self,
        req: hclient_proxy::TunnelRequest<'b>,
    ) -> Result<hclient_proxy::Tunnel, Error> {
        use hclient_proxy::TunnelVersion;
        let (h3, h2) = match req.effective_version() {
            TunnelVersion::Http3 => (true, false),
            TunnelVersion::Http2 => (false, true),
            TunnelVersion::Http3ThenHttp2 => (true, true),
            // A version this transport does not know is refused, as one
            // that lends no tunnels refuses them all — never read as the
            // nearest version it does know.
            other => {
                return Err(Error::new(
                    ErrorKind::Unsupported,
                    std::io::Error::other(format!(
                        "this transport cannot open a tunnel over {other:?}"
                    )),
                ));
            }
        };
        let mut h3_failed = None;
        let h2_follows = h2 && self.tunnel_h2.is_some();
        if h3 {
            match self.h3_arm() {
                // A proxy whose HTTP/3 tunnel already failed is not asked
                // again while HTTP/2 can carry this one: into a silent UDP
                // port each attempt would cost the whole of
                // `H3_TUNNEL_BEFORE_H2`.
                Some(_) if h2_follows && self.h3_remembered_failing(&req) => {}
                Some(arm) => {
                    let left = hclient_proxy::Dial::remaining(self);
                    let budget = if h2_follows {
                        Some(left.map_or(H3_TUNNEL_BEFORE_H2, |d| (d / 2).min(H3_TUNNEL_BEFORE_H2)))
                    } else {
                        left
                    };
                    match arm.tunnel_boxed(req.clone(), budget).await {
                        Ok(t) => return Ok(t),
                        Err(e) => {
                            self.remember_h3_failing(&req);
                            h3_failed = Some(e);
                        }
                    }
                }
                None if !h2 => {
                    return Err(Error::new(
                        ErrorKind::Unsupported,
                        std::io::Error::other(
                            "this transport lends HTTP/3 tunnels only with an HTTP/3 arm \
                             (`Native::http3`) and to a filter installed with `Native::egress`",
                        ),
                    ));
                }
                None => {}
            }
        }
        let Some(open) = self.tunnel_h2.filter(|_| h2) else {
            return Err(h3_failed.unwrap_or_else(|| {
                Error::new(
                    ErrorKind::Unsupported,
                    std::io::Error::other(
                        "this transport lends HTTP/2 tunnels only to a filter installed \
                         with `Native::egress`, and only with the `http2` feature",
                    ),
                )
            }));
        };
        let raw = self.connect_raw(req.proxy_host, req.proxy_port).await?;
        // When HTTP/2 fails here after HTTP/3 already did, the HTTP/2
        // error is the answer and the HTTP/3 error is not carried beside
        // it: the earlier attempt was the bounded probe, its outcome is in
        // the failure memory, and an error carries one cause — the same
        // last-error-wins shape `after_quic_failed` gives a fallback to
        // TCP on the direct path.
        open(self.tls, raw, req).await
    }

    /// The QUIC arm HTTP/3 tunnels are opened through, if one is lent.
    #[cfg(feature = "http3")]
    fn h3_arm(&self) -> Option<&crate::http3::arm::Arm> {
        self.tunnel_h3.map(|l| l.arm)
    }

    /// Whether an HTTP/3 tunnel to `req`'s proxy failed recently enough to
    /// be remembered.
    #[cfg(feature = "http3")]
    fn h3_remembered_failing(&self, req: &hclient_proxy::TunnelRequest<'_>) -> bool {
        self.tunnel_h3.is_some_and(|l| {
            l.failures
                .suppressed(&tunnel_origin(req), self.rt.elapsed_since(l.epoch))
        })
    }

    /// An HTTP/3 tunnel to `req`'s proxy failed.
    #[cfg(feature = "http3")]
    fn remember_h3_failing(&self, req: &hclient_proxy::TunnelRequest<'_>) {
        if let Some(l) = self.tunnel_h3 {
            l.failures
                .note(&tunnel_origin(req), self.rt.elapsed_since(l.epoch));
        }
    }

    /// Nothing is remembered without the `http3` feature.
    #[cfg(not(feature = "http3"))]
    #[expect(
        clippy::unused_self,
        reason = "the `http3` twin reads `self`, and the call site stays free of a `#[cfg]`"
    )]
    fn h3_remembered_failing(&self, _req: &hclient_proxy::TunnelRequest<'_>) -> bool {
        false
    }

    /// Nothing is remembered without the `http3` feature.
    #[cfg(not(feature = "http3"))]
    #[expect(
        clippy::unused_self,
        reason = "the `http3` twin reads `self`, and the call site stays free of a `#[cfg]`"
    )]
    fn remember_h3_failing(&self, _req: &hclient_proxy::TunnelRequest<'_>) {}

    /// No HTTP/3 tunnels without the `http3` feature.
    #[cfg(not(feature = "http3"))]
    #[expect(
        clippy::unused_self,
        reason = "the `http3` twin reads `self`, and the call site stays free of a `#[cfg]`"
    )]
    fn h3_arm(&self) -> Option<&NoArm> {
        None
    }

    /// [`Dial::connect_ipc`](hclient_proxy::Dial::connect_ipc)'s socket,
    /// before it is wrapped.
    async fn connect_ipc_raw(&self, addr: &hclient_rt::IpcAddr) -> Result<R::Stream, Error> {
        let Some(dial) = self.ipc else {
            return Err(Error::new(
                ErrorKind::Unsupported,
                std::io::Error::other(
                    "this transport opens no same-machine connections; \
                     see `Native::unix_socket` and `Native::proxy_over_ipc`",
                ),
            ));
        };
        dial(self.rt, addr)
            .await
            .map_err(|e| Error::new(ErrorKind::Connect, e))
    }
}

impl<R, D, L, H> hclient_proxy::Dial for NativeDial<'_, R, D, L, H>
where
    R: TcpConnect + Timer,
    D: Resolve + ?Sized,
    L: TlsConnect,
    H: Hooks,
{
    type Stream = crate::DialStream<R::Stream, L>;

    async fn connect<'a>(&'a self, host: &'a str, port: u16) -> Result<Self::Stream, Error> {
        self.connect_raw(host, port)
            .await
            .map(crate::DialStream::raw)
    }

    async fn connect_ipc<'a>(
        &'a self,
        addr: &'a hclient_rt::IpcAddr,
    ) -> Result<Self::Stream, Error> {
        self.connect_ipc_raw(addr).await.map(crate::DialStream::raw)
    }

    fn remaining(&self) -> Option<Duration> {
        self.budget
            .map(|b| b.saturating_sub(self.rt.elapsed_since(self.lent)))
    }

    // Maintainer notes (not rendered):
    // No up-front `config_id_for` check here, unlike `Native::execute`'s:
    // that one refuses an unknown label before a socket is opened, and
    // here the socket is already open. The backend owes the refusal
    // itself — `TlsIdentity`'s contract, a refusal naming the label and
    // never a substitution — and `connect_tls_refuses_an_unknown_identity`
    // asserts it arrives. A second check was written and removed: deleting
    // it left every test green, so it was a check that could not fail.
    async fn connect_tls<'b>(
        &'b self,
        stream: Self::Stream,
        req: hclient_proxy::ProxyTls<'b>,
    ) -> Result<Self::Stream, Error> {
        let tls_req =
            hclient_tls::TlsRequest::new(req.server_name, req.alpn).identity(req.identity);
        let (s, _info) = self.tls.connect(stream, tls_req).await?;
        Ok(crate::DialStream::tls(s))
    }

    fn bind_udp(&self, local: std::net::SocketAddr) -> Result<hclient_proxy::BoxUdp, Error> {
        let Some(bind) = self.udp else {
            return Err(Error::new(
                ErrorKind::Unsupported,
                std::io::Error::other(
                    "this transport lends UDP only with an HTTP/3 arm; see `Native::http3`",
                ),
            ));
        };
        bind(self.rt, local).map_err(|e| Error::new(ErrorKind::Connect, e))
    }

    // Maintainer notes (not rendered):
    // The proxy's own name, and never the origin's — a filtered path must
    // not resolve the origin locally, which is exactly the leak a proxy
    // exists to avoid. Both families are asked, the way `connect::connect`
    // asks them, so a caller racing the addresses this hands back gets the
    // same Happy Eyeballs shape a direct connect would.
    async fn resolve<'a>(
        &'a self,
        host: &'a str,
        port: u16,
    ) -> Result<Vec<std::net::SocketAddr>, Error> {
        use futures_util::StreamExt as _;
        use hclient_dns::rtype;

        if let Ok(ip) = hclient_core::url::bare_host(host).parse::<std::net::IpAddr>() {
            return Ok(vec![std::net::SocketAddr::new(ip, port)]);
        }
        let mut out = Vec::new();
        for t in [rtype::AAAA, rtype::A] {
            let mut s = std::pin::pin!(self.dns.lookup(host, t));
            while let Some(r) = s.next().await {
                if let Ok(Some(ip)) = r.map(|rec| rec.rdata.addr()) {
                    out.push(std::net::SocketAddr::new(ip, port));
                }
            }
        }
        if out.is_empty() {
            return Err(Error::new(
                ErrorKind::Resolve,
                std::io::Error::other(format!("no address for {host}")),
            ));
        }
        Ok(out)
    }

    async fn connect_tunnel<'a>(
        &'a self,
        req: hclient_proxy::TunnelRequest<'a>,
    ) -> Result<hclient_proxy::Tunnel, Error> {
        self.connect_tunnel_raw(req).await
    }
}

// Erased for an external filter, where the runtime's and the resolver's
// futures have been proven `Send` — see `crate::external`.
impl<R, D, L, H> hclient_proxy::DynDial for NativeDial<'_, R, D, L, H>
where
    R: TcpConnect + Timer + Sync,    // send-bound-exception: amendment-C15
    R::Stream: Send + 'static,       // send-bound-exception: amendment-C15
    R::Instant: Send + Sync,         // send-bound-exception: amendment-C15
    R::Sleep: Send,                  // send-bound-exception: amendment-C15
    for<'x> R::Connecting<'x>: Send, // send-bound-exception: amendment-C15
    D: Resolve + Sync,               // send-bound-exception: amendment-C15
    for<'x> D::Records<'x>: Send,    // send-bound-exception: amendment-C15
    L: TlsConnect + Sync,            // send-bound-exception: amendment-C15
    L::Stream<hclient_proxy::BoxIo>: Send + 'static, // send-bound-exception: amendment-C15
    for<'x> L::Handshake<'x, hclient_proxy::BoxIo>: Send, // send-bound-exception: amendment-C15
    H: Hooks,
{
    fn connect_boxed<'a>(&'a self, host: &'a str, port: u16) -> hclient_proxy::BoxDialing<'a> {
        Box::pin(async move {
            self.connect_raw(host, port)
                .await
                .map(hclient_proxy::BoxIo::new)
        })
    }

    fn connect_ipc_boxed<'a>(
        &'a self,
        addr: &'a hclient_rt::IpcAddr,
    ) -> hclient_proxy::BoxDialing<'a> {
        Box::pin(async move {
            self.connect_ipc_raw(addr)
                .await
                .map(hclient_proxy::BoxIo::new)
        })
    }

    fn remaining(&self) -> Option<Duration> {
        hclient_proxy::Dial::remaining(self)
    }

    fn connect_tls_boxed<'a>(
        &'a self,
        stream: hclient_proxy::BoxIo,
        req: hclient_proxy::ProxyTls<'a>,
    ) -> hclient_proxy::BoxDialing<'a> {
        Box::pin(async move {
            let tls_req =
                hclient_tls::TlsRequest::new(req.server_name, req.alpn).identity(req.identity);
            let (s, _info) = self.tls.connect(stream, tls_req).await?;
            Ok(hclient_proxy::BoxIo::new(s))
        })
    }

    fn bind_udp_boxed(&self, local: std::net::SocketAddr) -> Result<hclient_proxy::BoxUdp, Error> {
        hclient_proxy::Dial::bind_udp(self, local)
    }

    fn resolve_boxed<'a>(&'a self, host: &'a str, port: u16) -> hclient_proxy::BoxResolving<'a> {
        Box::pin(hclient_proxy::Dial::resolve(self, host, port))
    }

    fn connect_tunnel_boxed<'a>(
        &'a self,
        req: hclient_proxy::TunnelRequest<'a>,
    ) -> hclient_proxy::BoxTunnelling<'a> {
        Box::pin(self.connect_tunnel_raw(req))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hclient_core::hooks::NoHooks;
    use hclient_proxy::Dial as _;

    fn opts() -> hclient_rt::TcpOpts {
        hclient_rt::TcpOpts::default()
    }

    /// A resolver answering a fixed set of addresses, split by family per
    /// `rtype` the way a real backend would — so a caller asking both
    /// families back to back gets each address exactly once.
    struct Fixed(Vec<std::net::IpAddr>);

    impl hclient_dns::Resolve for Fixed {
        type Records<'a>
            = std::pin::Pin<
            Box<dyn futures_core::Stream<Item = Result<hclient_dns::Record, Error>> + 'a>,
        >
        where
            Self: 'a;

        fn supports(&self, rtype: u16) -> bool {
            matches!(rtype, hclient_dns::rtype::A | hclient_dns::rtype::AAAA)
        }

        fn lookup<'a>(&'a self, _name: &str, rtype: u16) -> Self::Records<'a> {
            let want_v6 = rtype == hclient_dns::rtype::AAAA;
            let want_v4 = rtype == hclient_dns::rtype::A;
            Box::pin(futures_util::stream::iter(
                self.0
                    .iter()
                    .copied()
                    .filter(move |a| a.is_ipv6() == want_v6 && (want_v4 || want_v6))
                    .map(|addr| Ok(hclient_dns::Record::new(hclient_dns::RData::from(addr)))),
            ))
        }
    }

    /// A minimal executor for a future that must not need tokio's own
    /// `#[tokio::test]` machinery — [`hclient_proxy::Dial::resolve`] and
    /// [`hclient_proxy::Dial::bind_udp`] are plain trait methods, and this
    /// is enough to drive the one `await` `resolve` needs.
    fn tokio_test_block_on<F: Future>(fut: F) -> F::Output {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(fut)
    }

    /// The context runs TLS with the transport's own backend: `NoTls`
    /// refuses every handshake, so the refusal proves the call reached it
    /// rather than the `Dial` default, which answers `Unsupported`.
    #[tokio::test]
    async fn connect_tls_runs_the_transports_backend() {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        let rt = hclient_rt_tokio::Tokio;
        let opts = opts();
        let tls = hclient_tls::NoTls;
        let dial = NativeDial::<_, _, _, NoHooks>::new(
            &rt,
            &hclient_dns::IpLiteralOnly,
            &tls,
            &opts,
            None,
            None,
            None,
            None,
        );
        let s = dial.connect("127.0.0.1", port).await.expect("connected");
        let Err(err) = dial
            .connect_tls(s, hclient_proxy::ProxyTls::new("proxy.test"))
            .await
        else {
            panic!("NoTls speaks no TLS");
        };
        assert_eq!(*err.kind(), ErrorKind::Tls);
    }

    /// A label the backend does not know is refused naming it, never
    /// swapped for the default identity.
    #[tokio::test]
    async fn connect_tls_refuses_an_unknown_identity() {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        let rt = hclient_rt_tokio::Tokio;
        let opts = opts();
        let tls = hclient_tls_rustls::Rustls::from_config(std::sync::Arc::new(
            rustls::ClientConfig::builder()
                .with_root_certificates(rustls::RootCertStore::empty())
                .with_no_client_auth(),
        ));
        let dial = NativeDial::<_, _, _, NoHooks>::new(
            &rt,
            &hclient_dns::IpLiteralOnly,
            &tls,
            &opts,
            None,
            None,
            None,
            None,
        );
        let s = dial.connect("127.0.0.1", port).await.expect("connected");
        let answer = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            dial.connect_tls(
                s,
                hclient_proxy::ProxyTls::new("proxy.test").identity(Some("tenant-a")),
            ),
        )
        .await
        .expect("refused before any handshake could wait on the peer");
        let Err(err) = answer else {
            panic!("an unknown label must be refused");
        };
        assert_eq!(*err.kind(), ErrorKind::Tls);
        assert!(format!("{err:?}").contains("tenant-a"), "{err:?}");
    }

    /// The context dials a real listener by literal, through the same
    /// resolve-and-race path a direct request takes.
    #[tokio::test]
    async fn connect_reaches_a_listener_by_literal() {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        let rt = hclient_rt_tokio::Tokio;
        let opts = opts();
        let dial = NativeDial::<_, _, _, NoHooks>::new(
            &rt,
            &hclient_dns::IpLiteralOnly,
            &hclient_tls::NoTls,
            &opts,
            None,
            None,
            None,
            None,
        );
        let _s = dial.connect("127.0.0.1", port).await.expect("connected");
        assert!(l.accept().is_ok());
    }

    /// A name the resolver cannot answer is a resolution failure, not a
    /// connection to somewhere else.
    #[tokio::test]
    async fn connect_resolves_through_the_transports_resolver() {
        let rt = hclient_rt_tokio::Tokio;
        let opts = opts();
        let dial = NativeDial::<_, _, _, NoHooks>::new(
            &rt,
            &hclient_dns::IpLiteralOnly,
            &hclient_tls::NoTls,
            &opts,
            None,
            None,
            None,
            None,
        );
        assert!(dial.connect("proxy.test", 1).await.is_err());
    }

    #[tokio::test]
    async fn ipc_without_a_dialler_is_unsupported() {
        let rt = hclient_rt_tokio::Tokio;
        let opts = opts();
        let dial = NativeDial::<_, _, _, NoHooks>::new(
            &rt,
            &hclient_dns::IpLiteralOnly,
            &hclient_tls::NoTls,
            &opts,
            None,
            None,
            None,
            None,
        );
        let err = dial
            .connect_ipc(&hclient_rt::IpcAddr::unix("/nonexistent"))
            .await
            .unwrap_err();
        assert_eq!(*err.kind(), hclient_core::error::ErrorKind::Unsupported);
    }

    /// What is left, not what was given: a filter chaining two hops must
    /// see the first hop's cost taken out of the bound before the second.
    #[tokio::test]
    async fn remaining_counts_down_from_what_it_was_given() {
        let rt = hclient_rt_tokio::Tokio;
        let opts = opts();
        let d = std::time::Duration::from_millis(250);
        let dial = NativeDial::<_, _, _, NoHooks>::new(
            &rt,
            &hclient_dns::IpLiteralOnly,
            &hclient_tls::NoTls,
            &opts,
            None,
            Some(d),
            None,
            None,
        );
        tokio::time::sleep(std::time::Duration::from_millis(60)).await;
        let left = dial.remaining().expect("a bound was given");
        assert!(
            left <= std::time::Duration::from_millis(200),
            "{left:?} left of {d:?} after 60 ms"
        );
        assert!(left > std::time::Duration::ZERO, "{left:?}");
    }

    #[test]
    fn no_bound_is_no_bound() {
        let rt = hclient_rt_tokio::Tokio;
        let opts = opts();
        let dial = NativeDial::<_, _, _, NoHooks>::new(
            &rt,
            &hclient_dns::IpLiteralOnly,
            &hclient_tls::NoTls,
            &opts,
            None,
            None,
            None,
            None,
        );
        assert_eq!(dial.remaining(), None);
    }

    /// The proxy's own name, resolved through the transport's resolver —
    /// never the origin's, which a filtered path must not look up locally.
    #[test]
    fn resolve_answers_both_families_from_the_transports_resolver() {
        let dns = Fixed(vec![
            "192.0.2.1".parse().unwrap(),
            "2001:db8::1".parse().unwrap(),
        ]);
        let rt = hclient_rt_tokio::Tokio;
        let opts = opts();
        let dial = NativeDial::<_, _, hclient_tls::NoTls, NoHooks>::new(
            &rt,
            &dns,
            &hclient_tls::NoTls,
            &opts,
            None,
            None,
            None,
            None,
        );
        let got =
            tokio_test_block_on(hclient_proxy::Dial::resolve(&dial, "proxy.test", 1080)).unwrap();
        assert_eq!(got.len(), 2);
        assert!(got.iter().all(|a| a.port() == 1080));
    }

    /// No binder was ever installed — [`Native::http3`] is the only
    /// constructor that installs one — so the request is refused rather
    /// than silently ignored.
    #[test]
    fn bind_udp_refuses_without_an_installed_binder() {
        let rt = hclient_rt_tokio::Tokio;
        let dns = Fixed(vec![]);
        let opts = opts();
        let dial = NativeDial::<_, _, hclient_tls::NoTls, NoHooks>::new(
            &rt,
            &dns,
            &hclient_tls::NoTls,
            &opts,
            None,
            None,
            None,
            None,
        );
        let e = hclient_proxy::Dial::bind_udp(&dial, "0.0.0.0:0".parse().unwrap()).unwrap_err();
        assert_eq!(*e.kind(), ErrorKind::Unsupported);
    }
}
