//! The `NSURLSessionDataDelegate` and the queue it feeds.
//!
//! `URLSession` is callback-shaped and `Transport::execute` is a future,
//! so something has to bridge them. That something is [`Shared`]: a queue
//! the delegate pushes into from whatever thread the session's operation
//! queue runs on, and a waker the polling side leaves behind.
//!
//! # Why the delegate rather than the completion-handler API
//!
//! `dataTask(with:completionHandler:)` hands back one `NSData` when the
//! response is complete. That is a **buffered** body, and this workspace
//! has spent three backends establishing that a body which cannot stream
//! is a body that cannot be trusted with a large one. The delegate's
//! `didReceiveData:` is called as bytes arrive, so the body here streams
//! and `Capabilities::streaming_request_body`'s counterpart on the
//! response side is real rather than declared.

use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};

use bytes::Bytes;
use futures_channel::mpsc;
use futures_core::Stream as _;
use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2::{AnyThread, DefinedClass, define_class};
use objc2_foundation::{
    NSData, NSError, NSHTTPURLResponse, NSObject, NSObjectProtocol, NSString, NSURLRequest,
    NSURLResponse, NSURLSession, NSURLSessionDataDelegate, NSURLSessionDataTask,
    NSURLSessionDelegate, NSURLSessionResponseDisposition, NSURLSessionTask,
    NSURLSessionTaskDelegate,
};

/// One thing the delegate saw, in the order it saw it.
#[derive(Debug)]
pub(crate) enum Chunk {
    Head(http::StatusCode, http::HeaderMap),
    Data(Bytes),
    /// The task finished. `Some` is the failure's description — Apple's
    /// own localised one, which is all `NSError` reliably gives.
    End(Option<String>),
}

/// The queue between the delegate and whoever is polling.
///
/// **An unbounded channel rather than a mutex, a `VecDeque` and a hand-
/// rolled waker**, which is what this was. The three moved together and
/// the third was the interesting one: `poll_next` had to pop, register
/// the waker, and then pop **again**, because the delegate can push
/// between the first pop and the lock — the ordinary lost-wakeup race,
/// written out by hand each time. A channel owns that race.
///
/// **Unbounded, and the bound is not a knob we declined to set.** The
/// producer is a **synchronous Objective-C callback**: `URLSession`
/// invokes it on its own queue and it cannot `.await`. On a bounded
/// channel a full queue makes `try_send` an error, and there is nothing
/// here that could wait instead — so a bound would convert backpressure
/// into a dropped body. `hclient-fetch` uses `mpsc::channel(0)` for the
/// same job and can, because its producer is a spawned task with
/// somewhere to park. What is lost is backpressure this code never had:
/// the `VecDeque` was unbounded too.
///
/// `Send + Sync` because `NSURLSessionDelegate` is declared
/// `NSObjectProtocol + Send + Sync` and `define_class!` derives the
/// class's auto traits from its ivars — so this is what makes the class
/// legal rather than a choice about our own API. Measured before the
/// swap: `futures_channel::mpsc::UnboundedSender<T>` is `Send + Sync`,
/// so the class stays legal. Nothing on this crate's public surface
/// declares either.
#[derive(Debug)]
pub(crate) struct Shared {
    tx: mpsc::UnboundedSender<Chunk>,
    rx: Mutex<mpsc::UnboundedReceiver<Chunk>>,
}

impl Default for Shared {
    fn default() -> Self {
        let (tx, rx) = mpsc::unbounded();
        Self {
            tx,
            rx: Mutex::new(rx),
        }
    }
}

impl Shared {
    pub(crate) fn push(&self, c: Chunk) {
        // A closed receiver means the polling side went away, which is an
        // ordinary end rather than an error: the delegate has no one to
        // tell and nothing to do about it.
        let _ = self.tx.unbounded_send(c);
    }

    /// The next thing the delegate saw, or `Pending` with `cx` registered.
    ///
    /// The `Mutex` is over the **receiver**, not over a queue, and it is
    /// never contended: one body polls one exchange. It is there because
    /// `poll_next` takes `&self` — the delegate's ivar is an `Arc<Shared>`
    /// — where `Stream::poll_next` wants `&mut`.
    pub(crate) fn poll_next(&self, cx: &mut Context<'_>) -> Poll<Chunk> {
        let mut rx = self.rx.lock().expect("urlsession receiver poisoned");
        match Pin::new(&mut *rx).poll_next(cx) {
            // A closed channel with the delegate gone means the exchange
            // ended without a terminal `Chunk`, which `body.rs` reads as
            // the end it is.
            Poll::Ready(Some(c)) => Poll::Ready(c),
            Poll::Ready(None) => Poll::Ready(Chunk::End(None)),
            Poll::Pending => Poll::Pending,
        }
    }
}

pub(crate) struct Ivars {
    pub(crate) shared: Arc<Shared>,
}

define_class!(
    // SAFETY: `NSObject` imposes no subclassing requirements and this
    // class adds no `Drop`.
    #[unsafe(super(NSObject))]
    // unsafe-code-exception: amendment-C11
    #[name = "HClientUrlSessionDelegate"]
    #[ivars = Ivars]
    pub(crate) struct Delegate;

    unsafe impl NSObjectProtocol for Delegate {}
    // unsafe-code-exception: amendment-C11
    unsafe impl NSURLSessionDelegate for Delegate {}
    // unsafe-code-exception: amendment-C11

    unsafe impl NSURLSessionTaskDelegate for Delegate {
        // unsafe-code-exception: amendment-C11
        /// **Refused, always**, and this is the decision that separates
        /// this backend from `hclient-fetch`. Answering the completion
        /// handler with `nil` tells `URLSession` not to follow the
        /// redirect, so the `3xx` is handed to the caller as an ordinary
        /// response and `Client`'s redirect policy — its hop limit, its
        /// `Authorization` stripping across origins — is what decides.
        /// A browser gives no such choice, which is why that backend must
        /// report `RedirectSupport::Internal` and this one does not.
        #[unsafe(method(URLSession:task:willPerformHTTPRedirection:newRequest:completionHandler:))]
        // unsafe-code-exception: amendment-C11
        fn will_perform_redirection(
            &self,
            _session: &NSURLSession,
            _task: &NSURLSessionTask,
            _response: &NSHTTPURLResponse,
            _request: &NSURLRequest,
            completion_handler: &block2::DynBlock<dyn Fn(*mut NSURLRequest)>,
        ) {
            completion_handler.call((std::ptr::null_mut(),));
        }

        #[unsafe(method(URLSession:task:didCompleteWithError:))]
        // unsafe-code-exception: amendment-C11
        fn did_complete(
            &self,
            _session: &NSURLSession,
            _task: &NSURLSessionTask,
            error: Option<&NSError>,
        ) {
            let msg = error.map(|e| e.localizedDescription().to_string());
            self.ivars().shared.push(Chunk::End(msg));
        }
    }

    unsafe impl NSURLSessionDataDelegate for Delegate {
        // unsafe-code-exception: amendment-C11
        #[unsafe(method(URLSession:dataTask:didReceiveResponse:completionHandler:))]
        // unsafe-code-exception: amendment-C11
        fn did_receive_response(
            &self,
            _session: &NSURLSession,
            _task: &NSURLSessionDataTask,
            response: &NSURLResponse,
            completion_handler: &block2::DynBlock<dyn Fn(NSURLSessionResponseDisposition)>,
        ) {
            if let Some(http) = response.downcast_ref::<NSHTTPURLResponse>() {
                let status = http::StatusCode::from_u16(http.statusCode() as u16)
                    .unwrap_or(http::StatusCode::OK);
                let mut headers = http::HeaderMap::new();
                let all = http.allHeaderFields();
                // Indexed rather than iterated: `NSArray`'s `Iterator`
                // lives behind a feature this crate does not enable, and
                // `count`/`objectAtIndex` are the always-present pair.
                let keys = all.allKeys();
                for i in 0..keys.count() {
                    let k = keys.objectAtIndex(i);
                    let (Some(ks), Some(v)) = (
                        k.downcast_ref::<NSString>(),
                        all.objectForKey(&k)
                            .and_then(|v| v.downcast::<NSString>().ok()),
                    ) else {
                        continue;
                    };
                    let k = ks;
                    let (name, value) = (k.to_string(), v.to_string());
                    if let (Ok(n), Ok(v)) = (
                        http::HeaderName::try_from(name.as_str()),
                        http::HeaderValue::try_from(value.as_str()),
                    ) {
                        headers.append(n, v);
                    }
                }
                self.ivars().shared.push(Chunk::Head(status, headers));
            }
            // `Allow` rather than `BecomeDownload`: the body is streamed
            // through `didReceiveData:` below, which is the whole reason
            // this uses a delegate.
            completion_handler.call((NSURLSessionResponseDisposition::Allow,));
        }

        #[unsafe(method(URLSession:dataTask:didReceiveData:))]
        // unsafe-code-exception: amendment-C11
        fn did_receive_data(
            &self,
            _session: &NSURLSession,
            _task: &NSURLSessionDataTask,
            data: &NSData,
        ) {
            self.ivars()
                .shared
                .push(Chunk::Data(Bytes::from(data.to_vec())));
        }
    }
);

impl Delegate {
    pub(crate) fn new(shared: Arc<Shared>) -> Retained<Self> {
        let this = Self::alloc().set_ivars(Ivars { shared });
        unsafe { objc2::msg_send![super(this), init] }
        // unsafe-code-exception: amendment-C11
    }

    /// As the **task** delegate, which is the one that matters here:
    /// each task carries its own so that its callbacks reach its own
    /// `Shared`. The session-level slot is `None` — see `session.rs`.
    pub(crate) fn as_task_protocol(
        this: &Retained<Self>,
    ) -> Retained<ProtocolObject<dyn NSURLSessionTaskDelegate>> {
        ProtocolObject::from_retained(this.clone())
    }
}
