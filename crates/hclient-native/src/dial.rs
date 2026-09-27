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
    pub(crate) fn new(
        rt: &'a R,
        dns: &'a D,
        tls: &'a L,
        opts: &'a TcpOpts,
        ipc: Option<DialIpc<R>>,
        budget: Option<Duration>,
        began: Option<R::Instant>,
    ) -> Self {
        Self {
            rt,
            dns,
            tls,
            opts,
            ipc,
            budget,
            lent: rt.now(),
            began,
            attempted: Mutex::new(None),
            _h: PhantomData,
        }
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use hclient_core::hooks::NoHooks;
    use hclient_proxy::Dial as _;

    fn opts() -> hclient_rt::TcpOpts {
        hclient_rt::TcpOpts::default()
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
        );
        assert_eq!(dial.remaining(), None);
    }
}
