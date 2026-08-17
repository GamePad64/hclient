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

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll, Waker};

use bytes::Bytes;
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
/// `Send + Sync` because `NSURLSessionDelegate` is declared
/// `NSObjectProtocol + Send + Sync` and `define_class!` derives the
/// class's auto traits from its ivars — so this is what makes the class
/// legal rather than a choice about our own API. Nothing on this crate's
/// public surface declares either.
#[derive(Debug, Default)]
pub(crate) struct Shared {
    queue: Mutex<VecDeque<Chunk>>,
    waker: Mutex<Option<Waker>>,
}

impl Shared {
    pub(crate) fn push(&self, c: Chunk) {
        self.queue
            .lock()
            .expect("urlsession queue poisoned")
            .push_back(c);
        if let Some(w) = self.waker.lock().expect("urlsession waker poisoned").take() {
            w.wake();
        }
    }

    /// The next thing the delegate saw, or `Pending` with `cx` registered.
    pub(crate) fn poll_next(&self, cx: &Context<'_>) -> Poll<Chunk> {
        if let Some(c) = self
            .queue
            .lock()
            .expect("urlsession queue poisoned")
            .pop_front()
        {
            return Poll::Ready(c);
        }
        *self.waker.lock().expect("urlsession waker poisoned") = Some(cx.waker().clone());
        // Re-checked after registering: the delegate may have pushed
        // between the pop and the lock, which is the ordinary lost-wakeup
        // race and the reason this is not a plain early return.
        match self
            .queue
            .lock()
            .expect("urlsession queue poisoned")
            .pop_front()
        {
            Some(c) => Poll::Ready(c),
            None => Poll::Pending,
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
    #[name = "HttpNgUrlSessionDelegate"]
    #[ivars = Ivars]
    pub(crate) struct Delegate;

    unsafe impl NSObjectProtocol for Delegate {}
    // unsafe-code-exception: amendment-C11
    unsafe impl NSURLSessionDelegate for Delegate {}
    // unsafe-code-exception: amendment-C11

    unsafe impl NSURLSessionTaskDelegate for Delegate {
        // unsafe-code-exception: amendment-C11
        /// **Refused, always**, and this is the decision that separates
        /// this backend from `http-ng-fetch`. Answering the completion
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

    pub(crate) fn as_protocol(
        this: &Retained<Self>,
    ) -> Retained<ProtocolObject<dyn NSURLSessionDelegate>> {
        ProtocolObject::from_retained(this.clone())
    }
}
