//! The transport.

use std::future::poll_fn;
use std::sync::Arc;
use std::task::Poll;

use hclient_core::unversioned::Transport;
use hclient_core::{
    CancelSupport, Capabilities, DecompressionSupport, Error, ErrorKind, RedirectSupport,
    RequestBody, TlsSupport,
};
use objc2::rc::Retained;
use objc2_foundation::{
    NSData, NSMutableURLRequest, NSOperationQueue, NSString, NSURL, NSURLSession,
    NSURLSessionConfiguration, NSURLSessionTask,
};

use crate::body::{Cancelling, UrlSessionBody};
use crate::delegate::{Chunk, Delegate, Shared};

/// What `URLSession` said went wrong, in Apple's own words.
///
/// `NSError`'s `localizedDescription` and nothing more: its `domain` and
/// `code` are stable enough to match on, but mapping them onto this
/// workspace's `ErrorKind` would be a second vocabulary invented at the
/// boundary — the same reason `hclient-fetch` reports what the browser
/// said rather than a translation of it.
#[derive(Debug, thiserror::Error)]
#[error("URLSession: {0}")]
pub struct UrlSessionError(pub String);

/// Apple's `URLSession` as a [`Transport`].
#[derive(Debug)]
pub struct UrlSession {
    session: Retained<NSURLSession>,
    caps: Capabilities,
}

impl UrlSession {
    /// A session that persists nothing of its own.
    ///
    /// `ephemeral` is Apple's name for a configuration with no on-disk
    /// cache, no cookie storage and no credential storage. The cookie
    /// storage is then set to `nil` as well, because `ephemeral` gives an
    /// in-memory jar rather than none — and an in-memory jar is still a
    /// second jar, which is the thing this backend is refusing to be.
    ///
    /// See the crate doc for why: cookies, caching and redirects are
    /// portable behaviour this workspace already implements once, and a
    /// caller must not lose `hclient`'s versions by choosing this
    /// backend for the OS-owned things it *is* here for.
    pub fn new() -> Self {
        let cfg = NSURLSessionConfiguration::ephemeralSessionConfiguration();
        cfg.setHTTPCookieStorage(None);
        cfg.setURLCache(None);
        cfg.setHTTPShouldSetCookies(false);
        let queue = NSOperationQueue::new();
        // **Serial**, which Apple requires of a delegate queue and a fresh
        // `NSOperationQueue` is not: its default concurrency is
        // system-chosen, and `URLSession` asks for one operation at a time
        // so that a task's callbacks arrive in the order they happened.
        // Out of order, a `didReceiveData:` could reach the queue before
        // the `didReceiveResponse:` it follows.
        queue.setMaxConcurrentOperationCount(1);
        // **No session-level delegate, and that was a bug before it was a
        // decision.** One delegate per session means one queue for every
        // task, so `execute` polled a `Shared` nothing ever pushed to and
        // every request hung — found by running on macOS, where the
        // Linux-side `cargo check` had been clean. Each task gets its own
        // delegate below, which is what makes one `Shared` per exchange
        // possible at all.
        let session = unsafe {
            // unsafe-code-exception: amendment-C11
            NSURLSession::sessionWithConfiguration_delegate_delegateQueue(&cfg, None, Some(&queue))
        };
        Self {
            session,
            // Read **once**, at construction, which makes the report a
            // snapshot: a machine whose proxy is switched on afterwards
            // is not re-read, exactly as `Native::system_proxy`'s list is
            // not. See `capabilities` for what the value means.
            caps: capabilities(
                hclient_proxy::system::SystemProxies::detect_platform().names_a_proxy(),
            ),
        }
    }
}

impl Default for UrlSession {
    fn default() -> Self {
        Self::new()
    }
}

/// Built from [`Capabilities::default`] and turned on field by field, the
/// shape every backend here uses: a field added later arrives as the
/// conservative default rather than as a compile error somebody silences
/// by copying its neighbour.
fn capabilities(proxied: bool) -> Capabilities {
    let mut c = Capabilities::default();
    // **The OS proxies, so this transport does**, and reporting `false`
    // while the machine routes every request through a proxy was a
    // capability that lies — the one class of defect this workspace
    // treats as worse than a missing feature, because a caller can act on
    // a capability where a silent downgrade only disappoints them.
    //
    // The value is a parameter rather than a read taken here, so that the
    // half of this that cannot run on the machine this workspace is
    // developed on holds no decision: every rule behind it is
    // `hclient_proxy::system::SystemProxies::names_a_proxy`, which is
    // tested on any host, over `detect_platform`, which is tested to
    // ignore the environment. That is `hclient-dns-system`'s split
    // between `sys` and its parsers, applied here.
    //
    // **Why the platform settings and not `detect`.** `detect` reads
    // `HTTPS_PROXY` first, as curl and reqwest do; `URLSession` takes its
    // proxies from the system configuration and no account of it says it
    // reads the environment. So a value off `detect` would report `true`
    // on a machine whose only proxy is a variable this transport ignores.
    // The platform read is the understating one of the two in either
    // reading: exact if `URLSession` honours the system configuration
    // alone, and short of the truth rather than ahead of it if it turned
    // out to honour more.
    //
    // **What `true` claims, and what it cannot.** It says the machine
    // names a proxy that this transport hands to the OS — not that any
    // particular request goes through it, which the exceptions list and,
    // for a PAC machine, a JavaScript program decide per request. That is
    // the same reading `hclient-native` gives the field, where a proxy
    // list with a bypass that matches everything still reports `true`.
    // A PAC script therefore reads as `true` here and the script may
    // answer `DIRECT` for every URL; the alternative is to report a
    // proxied machine as direct, which is the failure a caller cannot
    // diagnose from the outside.
    c.proxy = proxied;
    // The delegate refuses every redirect, so a `3xx` is an ordinary
    // response and `Client` decides — see `delegate.rs`. This is the one
    // capability where this backend is stronger than `hclient-fetch`,
    // which has no way to refuse.
    c.redirects = RedirectSupport::Transparent;
    // `URLSessionTask::cancel` on drop, held by the body — see
    // `body::Cancelling`.
    c.cancel_on_drop = CancelSupport::Supported;
    // `ephemeral` plus an explicit `nil`: this session keeps neither, so
    // `Client`'s own jar and cache are the ones in force.
    c.owns_cookie_jar = false;
    c.owns_cache = false;
    // **`URLSession` decodes `Content-Encoding` itself and does not say
    // so**, which is the same shape `hclient-fetch` reports for the same
    // reason: it also sets `Accept-Encoding` on its own, and a body
    // handed back has already been decoded.
    c.response_decompression = DecompressionSupport::Internal;
    // `None`, the same value `hclient-fetch` reports and for the same
    // reason: no TLS configuration is reachable through this seam. The
    // trust decisions are the OS's — which is the whole reason to be here
    // — and a caller who needs to choose roots wants
    // `hclient-native` with `hclient-tls-rustls`, not this.
    c.tls_config = TlsSupport::None;
    c
}

impl Transport for UrlSession {
    type Body = UrlSessionBody;
    type Error = Error;

    async fn execute(
        &self,
        req: http::Request<RequestBody>,
    ) -> Result<http::Response<UrlSessionBody>, Error> {
        let shared = Arc::new(Shared::default());
        let task = self.start(req, Arc::clone(&shared))?;
        let cancel = Cancelling(task);

        // The head, and nothing before it: a task that fails to connect
        // reports `End` without ever producing a `Head`, and that is an
        // `execute` error rather than a body one.
        let head = poll_fn(|cx| match shared.poll_next(cx) {
            Poll::Pending => Poll::Pending,
            Poll::Ready(Chunk::Head(s, h)) => Poll::Ready(Ok((s, h))),
            Poll::Ready(Chunk::End(msg)) => Poll::Ready(Err(Error::new(
                ErrorKind::Connect,
                UrlSessionError(msg.unwrap_or_else(|| "the task ended without a response".into())),
            ))),
            // Data before a head is not representable and not reachable:
            // `didReceiveData:` cannot precede `didReceiveResponse:`.
            Poll::Ready(Chunk::Data(_)) => Poll::Ready(Err(Error::new(
                ErrorKind::Decode,
                UrlSessionError("body bytes arrived before the response head".into()),
            ))),
        })
        .await?;

        let (status, headers) = head;
        let mut resp = http::Response::new(UrlSessionBody::new(shared, cancel));
        *resp.status_mut() = status;
        *resp.headers_mut() = headers;
        Ok(resp)
    }

    fn capabilities(&self) -> &Capabilities {
        &self.caps
    }
}

/// The `Send` half of the seam.
///
/// `URLSession` and its delegates are thread-safe by Apple's own contract
/// — a session hands work to its own queue — and `execute` awaits a
/// channel this crate owns, so the future is `Send` by inference and this
/// is one line of forwarding.
impl hclient_core::unversioned::SendTransport for UrlSession {
    fn execute_send(
        &self,
        req: http::Request<RequestBody>,
    ) -> hclient_core::unversioned::BoxSendExchange<'_, Self::Body, Self::Error> {
        Box::pin(<Self as Transport>::execute(self, req))
    }
}

impl UrlSession {
    /// Build the `NSURLRequest` and start a data task on it.
    /// Takes the request **by value**, because the body has to be: it
    /// ends up inside an `NSData` either way, and
    /// `hclient_core::RequestBody::reduce` consumes it — which is what
    /// replaced this file's own copy of the unwrapping.
    fn start(
        &self,
        req: http::Request<RequestBody>,
        shared: Arc<Shared>,
    ) -> Result<Retained<NSURLSessionTask>, Error> {
        let url = NSString::from_str(&req.uri().to_string());
        let Some(url) = NSURL::URLWithString(&url) else {
            return Err(Error::new(
                ErrorKind::Connect,
                UrlSessionError(format!("`{}` is not a URL NSURL accepts", req.uri())),
            ));
        };
        let request = NSMutableURLRequest::requestWithURL(&url);
        request.setHTTPMethod(&NSString::from_str(req.method().as_str()));
        for (name, value) in req.headers() {
            let Ok(value) = value.to_str() else { continue };
            request.setValue_forHTTPHeaderField(
                Some(&NSString::from_str(value)),
                &NSString::from_str(name.as_str()),
            );
        }
        match resolve_body(req.into_body())? {
            Some(bytes) => {
                let data = NSData::with_bytes(&bytes);
                request.setHTTPBody(Some(&data));
            }
            None => {}
        }
        let task = self.session.dataTaskWithRequest(&request);
        // The task-scoped delegate: `URLSessionTask.delegate` is a
        // **strong** reference, unusually for Cocoa, so the task keeps it
        // alive and this crate does not have to.
        let delegate = Delegate::new(shared);
        task.setDelegate(Some(&Delegate::as_task_protocol(&delegate)));
        task.resume();
        Ok(Retained::into_super(task))
    }
}

/// How deep a `Rewindable` factory may nest before this gives up.
///
// The rewind bound moved to `hclient_core::MAX_REWIND_DEPTH`.

/// The bytes to put on the request, or `None` for no body at all.
///
/// **A body this backend cannot send is a typed error, never a silent
/// drop.** `Client` does not gate on
/// `Capabilities::streaming_request_body`, so a `Streaming` body reaching
/// here would otherwise go out as a request with no body — the request
/// would succeed, and the payload would simply be gone. That is the
/// silent no-op this workspace refuses everywhere else: an unsupported
/// setting is an error, not an omission.
///
/// A `Rewindable` is unwrapped rather than refused, because its factory
/// usually hands back a `Full` and that is a body this backend can send —
/// the same bounded loop `hclient`'s multipart encoder uses, and for the
/// same reason.
fn resolve_body(body: RequestBody) -> Result<Option<bytes::Bytes>, Error> {
    let refuse = |what: &str| {
        Err(Error::new(
            ErrorKind::Unsupported,
            UrlSessionError(format!(
                "URLSession here sends a buffered body only, and this request carries {what}; \
                 `Capabilities::streaming_request_body` is `false` and says so"
            )),
        ))
    };
    // The unwrapping and its depth bound are `hclient_core`'s — this was
    // the seventh copy of one reduction in this workspace, and the bound
    // it applies is one number now instead of three answers. A body that
    // streams is still this backend's own refusal, because that is a fact
    // about `URLSession` rather than about the body.
    match body
        .reduce()
        .map_err(|e| Error::new(ErrorKind::Unsupported, UrlSessionError(e.to_string())))?
    {
        hclient_core::Reduced::Empty => Ok(None),
        hclient_core::Reduced::Bytes(b) => Ok(Some(b)),
        hclient_core::Reduced::Streaming(_) => refuse("a streaming one"),
    }
}

#[cfg(test)]
mod tests {
    use super::capabilities;

    /// The pair, because either half alone passes for a transport that
    /// hardcodes the other: a `capabilities` ignoring its argument
    /// satisfies exactly one of these two lines.
    ///
    /// It runs only on a Mac — this whole crate is `#![cfg(target_vendor
    /// = "apple")]` — which is why the *reading* it consumes lives in
    /// `hclient-proxy` and is tested on any host.
    #[test]
    fn the_proxy_report_is_the_answer_it_was_given() {
        assert!(capabilities(true).proxy);
        assert!(!capabilities(false).proxy);
    }
}
