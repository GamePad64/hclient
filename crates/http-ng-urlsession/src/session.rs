//! The transport.

use std::sync::Arc;
use std::task::Poll;

use http_ng_core::unversioned::Transport;
use http_ng_core::{
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
/// boundary — the same reason `http-ng-fetch` reports what the browser
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
    /// caller must not lose `http-ng`'s versions by choosing this
    /// backend for the OS-owned things it *is* here for.
    pub fn new() -> Self {
        let cfg = NSURLSessionConfiguration::ephemeralSessionConfiguration();
        cfg.setHTTPCookieStorage(None);
        cfg.setURLCache(None);
        cfg.setHTTPShouldSetCookies(false);
        let queue = NSOperationQueue::new();
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
            caps: capabilities(),
        }
    }
}

impl Default for UrlSession {
    fn default() -> Self {
        Self::new()
    }
}

/// Built from [`Capabilities::none`] and turned on field by field, the
/// shape every backend here uses: a field added later arrives as the
/// conservative default rather than as a compile error somebody silences
/// by copying its neighbour.
fn capabilities() -> Capabilities {
    let mut c = Capabilities::none();
    // The delegate refuses every redirect, so a `3xx` is an ordinary
    // response and `Client` decides — see `delegate.rs`. This is the one
    // capability where this backend is stronger than `http-ng-fetch`,
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
    // so**, which is the same shape `http-ng-fetch` reports for the same
    // reason: it also sets `Accept-Encoding` on its own, and a body
    // handed back has already been decoded.
    c.response_decompression = DecompressionSupport::Internal;
    // `None`, the same value `http-ng-fetch` reports and for the same
    // reason: no TLS configuration is reachable through this seam. The
    // trust decisions are the OS's — which is the whole reason to be here
    // — and a caller who needs to choose roots wants
    // `http-ng-native` with `http-ng-tls-rustls`, not this.
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
        let task = self.start(&req, Arc::clone(&shared))?;
        let cancel = Cancelling(task);

        // The head, and nothing before it: a task that fails to connect
        // reports `End` without ever producing a `Head`, and that is an
        // `execute` error rather than a body one.
        let head = std::future::poll_fn(|cx| match shared.poll_next(cx) {
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

impl UrlSession {
    /// Build the `NSURLRequest` and start a data task on it.
    fn start(
        &self,
        req: &http::Request<RequestBody>,
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
        if let RequestBody::Full(bytes) = req.body() {
            let data = NSData::with_bytes(bytes);
            request.setHTTPBody(Some(&data));
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
