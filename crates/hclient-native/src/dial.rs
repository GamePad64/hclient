//! [`NativeDial`]: what `Native` lends an egress filter — its own way of
//! reaching a host by name, a same-machine socket, and what is left of
//! the request's connect bound.

use std::future::Future;
use std::marker::PhantomData;
use std::sync::Mutex;
use std::time::Duration;

use hclient_core::error::{Error, ErrorKind};
use hclient_core::hooks::Hooks;
use hclient_dns::Resolve;
use hclient_rt::{TcpConnect, TcpOpts, Timer};

use crate::DialIpc;
use crate::connect::Attempted;

/// `Native`'s [`hclient_proxy::Dial`], built per connection.
///
/// Its futures are `impl Future`, so a filter called concretely keeps
/// whatever auto traits the runtime's and resolver's futures have: a
/// `Send` runtime gives a `Send` connect, and a `!Send` one (embassy) is
/// asked for nothing.
pub(crate) struct NativeDial<'a, R: TcpConnect + Timer, D: ?Sized, H> {
    rt: &'a R,
    dns: &'a D,
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

impl<'a, R: TcpConnect + Timer, D: ?Sized, H> NativeDial<'a, R, D, H> {
    pub(crate) fn new(
        rt: &'a R,
        dns: &'a D,
        opts: &'a TcpOpts,
        ipc: Option<DialIpc<R>>,
        budget: Option<Duration>,
        began: Option<R::Instant>,
    ) -> Self {
        Self {
            rt,
            dns,
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

impl<R, D, H> hclient_proxy::Dial for NativeDial<'_, R, D, H>
where
    R: TcpConnect + Timer,
    D: Resolve + ?Sized,
    H: Hooks,
{
    type Stream = R::Stream;

    fn connect(
        &self,
        host: &str,
        port: u16,
    ) -> impl Future<Output = Result<R::Stream, Error>> + '_ {
        let host = host.to_owned();
        async move {
            let (stream, attempted) = crate::connect::dial_by_name::<R, D, H>(
                self.rt, self.dns, &host, port, self.opts, self.began,
            )
            .await?;
            *self
                .attempted
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = attempted;
            Ok(stream)
        }
    }

    fn connect_ipc(
        &self,
        addr: &hclient_rt::IpcAddr,
    ) -> impl Future<Output = Result<R::Stream, Error>> + '_ {
        let dial = self.ipc;
        let addr = addr.clone();
        async move {
            let Some(dial) = dial else {
                return Err(Error::new(
                    ErrorKind::Unsupported,
                    std::io::Error::other(
                        "this transport opens no same-machine connections; \
                         see `Native::unix_socket` and `Native::proxy_over_ipc`",
                    ),
                ));
            };
            dial(self.rt, &addr)
                .await
                .map_err(|e| Error::new(ErrorKind::Connect, e))
        }
    }

    fn remaining(&self) -> Option<Duration> {
        self.budget
            .map(|b| b.saturating_sub(self.rt.elapsed_since(self.lent)))
    }
}

// Erased for an external filter, where the runtime's and the resolver's
// futures have been proven `Send` — see `crate::external`.
impl<R, D, H> hclient_proxy::DynDial for NativeDial<'_, R, D, H>
where
    R: TcpConnect + Timer + Sync,    // send-bound-exception: amendment-C15
    R::Stream: Send + 'static,       // send-bound-exception: amendment-C15
    R::Instant: Send + Sync,         // send-bound-exception: amendment-C15
    R::Sleep: Send,                  // send-bound-exception: amendment-C15
    for<'x> R::Connecting<'x>: Send, // send-bound-exception: amendment-C15
    D: Resolve + Sync,               // send-bound-exception: amendment-C15
    for<'x> D::Records<'x>: Send,    // send-bound-exception: amendment-C15
    H: Hooks,
{
    fn connect_boxed<'a>(&'a self, host: &'a str, port: u16) -> hclient_proxy::BoxDialing<'a> {
        Box::pin(async move {
            hclient_proxy::Dial::connect(self, host, port)
                .await
                .map(hclient_proxy::BoxIo::new)
        })
    }

    fn connect_ipc_boxed<'a>(
        &'a self,
        addr: &'a hclient_rt::IpcAddr,
    ) -> hclient_proxy::BoxDialing<'a> {
        Box::pin(async move {
            hclient_proxy::Dial::connect_ipc(self, addr)
                .await
                .map(hclient_proxy::BoxIo::new)
        })
    }

    fn remaining(&self) -> Option<Duration> {
        hclient_proxy::Dial::remaining(self)
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

    /// The context dials a real listener by literal, through the same
    /// resolve-and-race path a direct request takes.
    #[tokio::test]
    async fn connect_reaches_a_listener_by_literal() {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        let rt = hclient_rt_tokio::Tokio;
        let opts = opts();
        let dial = NativeDial::<_, _, NoHooks>::new(
            &rt,
            &hclient_dns::IpLiteralOnly,
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
        let dial = NativeDial::<_, _, NoHooks>::new(
            &rt,
            &hclient_dns::IpLiteralOnly,
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
        let dial = NativeDial::<_, _, NoHooks>::new(
            &rt,
            &hclient_dns::IpLiteralOnly,
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
        let dial = NativeDial::<_, _, NoHooks>::new(
            &rt,
            &hclient_dns::IpLiteralOnly,
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
        let dial = NativeDial::<_, _, NoHooks>::new(
            &rt,
            &hclient_dns::IpLiteralOnly,
            &opts,
            None,
            None,
            None,
        );
        assert_eq!(dial.remaining(), None);
    }
}
