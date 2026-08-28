//! The FFI boundary: WinHTTP's handles, its status callback, and the
//! state the two sides share.
//!
//! **Every `unsafe` in this crate is in this file**, which is the split
//! `hclient-dns-system` already draws between `sys/` and its parsers: the
//! OS-touching half holds no decisions, and everything above it —
//! capabilities, which OS features to refuse, how a body ends — is
//! ordinary safe Rust that can be read without checking a contract.
//!
//! # The three obligations this file rests on
//!
//! WinHTTP in asynchronous mode is a callback API with a `dwContext`, and
//! three of its rules are load-bearing here. Each is stated at the code
//! that depends on it, and none of them has been *observed* — see the
//! crate doc on what has and has not been run.
//!
//! 1. **A buffer handed to `WinHttpReadData` belongs to WinHTTP until
//!    `WINHTTP_CALLBACK_STATUS_READ_COMPLETE`.** Touching it before then
//!    races the OS thread writing into it. [`Buf`] makes that structural
//!    rather than a discipline: while the read is in flight there is no
//!    `Box` to read, only a pointer, so safe code upstream cannot reach
//!    the bytes even by mistake.
//! 2. **`WINHTTP_CALLBACK_STATUS_HANDLE_CLOSING` is the last callback a
//!    handle receives.** That is where the `Arc<Exchange>` handed to
//!    WinHTTP as the context is released. If it were ever *not* delivered
//!    the cost is a leaked `Arc` and a leaked buffer — never a dangling
//!    pointer, because the raw reference is what keeps the `Exchange`
//!    alive in the first place.
//! 3. **A handle is usable from any thread.** WinHTTP documents its
//!    handles as thread-agnostic, which is what the `Send` impls below
//!    say; the callback arrives on a thread-pool thread, and the polling
//!    side is wherever the caller's executor put it.
//!
//! # Why the request headers are added before the send
//!
//! `WinHttpSendRequest` takes an `lpszHeaders` pointer, and whether that
//! buffer must outlive the call is a question with an unpleasant answer
//! either way. [`Request::add_headers`] uses `WinHttpAddRequestHeaders`
//! instead — a synchronous call that copies — so the send passes no
//! header pointer at all and the question does not arise.

use std::collections::VecDeque;
use std::ffi::c_void;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll, Waker};

use bytes::Bytes;
use windows_sys::Win32::Foundation::GetLastError;
use windows_sys::Win32::Networking::WinHttp as w;

/// The default read buffer, in bytes.
///
/// One buffer per exchange, allocated once and lent to WinHTTP for each
/// read — so this is the largest chunk a body can hand back, not a total.
/// 16 KiB is `hyper`'s own initial read size for the same job.
const READ_BUF: usize = 16 * 1024;

/// A UTF-16, null-terminated copy of `s`, for the `PCWSTR` parameters.
///
/// Every WinHTTP call that takes one here is synchronous and copies what
/// it needs, so the `Vec` may die at the end of the statement.
fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// What WinHTTP last told us, in the order it said it.
#[derive(Debug)]
pub(crate) enum Event {
    /// The request, headers and body are on the wire.
    SendComplete,
    /// The response head is available to query.
    HeadersAvailable,
    /// Bytes were read into the buffer. `0` is end of body.
    ReadComplete(usize),
    /// A Win32 error code from `WINHTTP_ASYNC_RESULT::dwError`.
    Failed(u32),
    /// A TLS failure, with `WINHTTP_CALLBACK_STATUS_SECURE_FAILURE`'s
    /// flags. Kept apart from [`Event::Failed`] because it is the only
    /// error this API reports twice — a `SECURE_FAILURE` is followed by a
    /// `REQUEST_ERROR` carrying the generic
    /// `ERROR_WINHTTP_SECURE_FAILURE`, and the flags say which check
    /// failed where the code does not.
    SecureFailure(u32),
}

/// The read buffer, and which side owns it.
///
/// The two variants are the whole of obligation 1 in the module doc.
/// While a read is in flight the `Box` does not exist, so no safe code
/// above this file can read bytes WinHTTP is still writing.
#[derive(Debug)]
enum Buf {
    /// Ours. Safe to read.
    Home(Box<[u8]>),
    /// WinHTTP's, until `READ_COMPLETE` or `HANDLE_CLOSING`.
    Loaned { ptr: *mut u8, len: usize },
}

#[derive(Debug)]
struct Inner {
    events: VecDeque<Event>,
    buf: Buf,
    /// The request body, held only until `SENDREQUEST_COMPLETE`: the
    /// pointer handed to `WinHttpSendRequest` points into this
    /// allocation, and `Bytes` keeps it alive and immovable.
    sending: Option<Bytes>,
}

/// The state the callback and the polling side share.
#[derive(Debug)]
pub(crate) struct Exchange {
    inner: Mutex<Inner>,
    waker: Mutex<Option<Waker>>,
}

// SAFETY: `Inner` holds a raw pointer only in `Buf::Loaned`, and it is an
// allocation this crate owns — `Box::into_raw` on one side,
// `Box::from_raw` on the other. What crosses a thread is ownership of that
// allocation, which is exactly what a `Box` may do; the pointer carries no
// thread affinity of its own. Everything else in `Inner` is `Send`
// already.
#[allow(
    unsafe_code, // unsafe-code-exception: amendment-C18
    reason = "the only raw pointer is an owned allocation; see the SAFETY note"
)]
unsafe impl Send for Exchange {} // unsafe-code-exception: amendment-C18
#[allow(
    unsafe_code, // unsafe-code-exception: amendment-C18
    reason = "as above; every field is behind a Mutex"
)]
unsafe impl Sync for Exchange {} // unsafe-code-exception: amendment-C18

impl Exchange {
    pub(crate) fn new() -> Self {
        Self {
            inner: Mutex::new(Inner {
                events: VecDeque::new(),
                buf: Buf::Home(vec![0u8; READ_BUF].into_boxed_slice()),
                sending: None,
            }),
            waker: Mutex::new(None),
        }
    }

    fn push(&self, e: Event) {
        self.inner
            .lock()
            .expect("winhttp exchange poisoned")
            .events
            .push_back(e);
        if let Some(w) = self.waker.lock().expect("winhttp waker poisoned").take() {
            w.wake();
        }
    }

    /// The next thing WinHTTP said, or `Pending` with `cx` registered.
    ///
    /// The re-check after registering is the ordinary lost-wakeup race:
    /// the callback may have pushed between the pop and the lock.
    pub(crate) fn poll_next(&self, cx: &Context<'_>) -> Poll<Event> {
        if let Some(e) = self
            .inner
            .lock()
            .expect("winhttp exchange poisoned")
            .events
            .pop_front()
        {
            return Poll::Ready(e);
        }
        *self.waker.lock().expect("winhttp waker poisoned") = Some(cx.waker().clone());
        match self
            .inner
            .lock()
            .expect("winhttp exchange poisoned")
            .events
            .pop_front()
        {
            Some(e) => Poll::Ready(e),
            None => Poll::Pending,
        }
    }

    /// The first `n` bytes of the buffer, copied out.
    ///
    /// Callable only between reads: the buffer is `Home` exactly then,
    /// and a `Loaned` one here would mean the state machine handed out an
    /// `Event::ReadComplete` without reclaiming, which is a bug in this
    /// file rather than something a caller can cause.
    pub(crate) fn take_read(&self, n: usize) -> Bytes {
        let inner = self.inner.lock().expect("winhttp exchange poisoned");
        match &inner.buf {
            Buf::Home(b) => Bytes::copy_from_slice(&b[..n.min(b.len())]),
            Buf::Loaned { .. } => {
                unreachable!("the buffer is reclaimed before ReadComplete is pushed")
            }
        }
    }
}

/// Releases the `Arc<Exchange>` WinHTTP holds, and reclaims a buffer that
/// is still lent out.
///
/// Only ever called from `HANDLE_CLOSING`. The buffer half is belt and
/// braces: a cancelled read reports `REQUEST_ERROR` and reclaims there,
/// but if a future Windows ever skipped that, this is what keeps the
/// allocation from leaking.
#[allow(
    unsafe_code, // unsafe-code-exception: amendment-C18
    reason = "reclaims the one owned reference and the lent buffer; see obligation 2"
)]
unsafe fn release(context: usize) {
    // unsafe-code-exception: amendment-C18
    // SAFETY: `context` is the pointer `Arc::into_raw` produced in
    // `Request::set_context`, and `HANDLE_CLOSING` is delivered once per
    // handle — so this reconstitutes exactly one strong reference and
    // consumes it.
    let ex = unsafe { Arc::from_raw(context as *const Exchange) }; // unsafe-code-exception: amendment-C18
    reclaim(&ex);
    drop(ex);
}

/// Puts a lent buffer back in Rust's hands, if it is lent.
#[allow(
    unsafe_code, // unsafe-code-exception: amendment-C18
    reason = "Box::from_raw over the allocation Box::into_raw produced in `read`"
)]
fn reclaim(ex: &Exchange) {
    let mut inner = ex.inner.lock().expect("winhttp exchange poisoned");
    if let Buf::Loaned { ptr, len } = inner.buf {
        // SAFETY: `ptr`/`len` came from `Box::into_raw` on a
        // `Box<[u8]>` of that length in `Request::read`, and WinHTTP has
        // finished with it — this is called from `READ_COMPLETE`,
        // `REQUEST_ERROR` or `HANDLE_CLOSING`, after each of which no
        // further write to the buffer happens.
        let b = unsafe { Box::from_raw(std::ptr::slice_from_raw_parts_mut(ptr, len)) }; // unsafe-code-exception: amendment-C18
        inner.buf = Buf::Home(b);
    }
}

/// WinHTTP's status callback.
///
/// Installed once on the session and inherited by every handle derived
/// from it. It runs on a WinHTTP thread-pool thread, so it does the least
/// it can: reclaim a buffer, push an event, wake.
#[allow(
    unsafe_code, // unsafe-code-exception: amendment-C18
    reason = "the callback WinHTTP calls; every dereference is documented at its line"
)]
unsafe extern "system" fn callback(
    // unsafe-code-exception: amendment-C18
    _handle: *mut c_void,
    context: usize,
    status: u32,
    info: *mut c_void,
    info_len: u32,
) {
    // Zero is a handle this crate never gave a context to — the connect
    // handle, or a request between `WinHttpOpenRequest` and
    // `set_context`. Nothing to report and nothing to release.
    if context == 0 {
        return;
    }
    if status == w::WINHTTP_CALLBACK_STATUS_HANDLE_CLOSING {
        // SAFETY: obligation 2 — the last callback for this handle, so
        // the owned reference is consumed exactly once.
        unsafe { release(context) }; // unsafe-code-exception: amendment-C18
        return;
    }
    // SAFETY: the pointer is kept alive by the strong reference
    // `set_context` handed over, which is released only in the arm above.
    // A borrow, not a reconstitution: consuming here would free the state
    // the next callback needs.
    let ex: &Exchange = unsafe { &*(context as *const Exchange) }; // unsafe-code-exception: amendment-C18

    match status {
        w::WINHTTP_CALLBACK_STATUS_SENDREQUEST_COMPLETE => {
            // The body pointer handed to `WinHttpSendRequest` is dead
            // from here, so the `Bytes` keeping it alive can go.
            ex.inner
                .lock()
                .expect("winhttp exchange poisoned")
                .sending
                .take();
            ex.push(Event::SendComplete);
        }
        w::WINHTTP_CALLBACK_STATUS_HEADERS_AVAILABLE => ex.push(Event::HeadersAvailable),
        w::WINHTTP_CALLBACK_STATUS_READ_COMPLETE => {
            // Obligation 1's other end: the buffer is ours again before
            // anything is allowed to look at it.
            reclaim(ex);
            ex.push(Event::ReadComplete(info_len as usize));
        }
        w::WINHTTP_CALLBACK_STATUS_SECURE_FAILURE => {
            reclaim(ex);
            // SAFETY: for this status WinHTTP documents
            // `lpvStatusInformation` as a `*mut u32` of flags. Checked
            // for null rather than trusted, because a zero here would
            // otherwise read address zero.
            let flags = if info.is_null() {
                0
            } else {
                unsafe { *info.cast::<u32>() } // unsafe-code-exception: amendment-C18
            };
            ex.push(Event::SecureFailure(flags));
        }
        w::WINHTTP_CALLBACK_STATUS_REQUEST_ERROR => {
            reclaim(ex);
            // SAFETY: for this status WinHTTP documents
            // `lpvStatusInformation` as a `*mut WINHTTP_ASYNC_RESULT`.
            // Null-checked for the same reason as above.
            let code = if info.is_null() {
                0
            } else {
                unsafe { (*info.cast::<w::WINHTTP_ASYNC_RESULT>()).dwError } // unsafe-code-exception: amendment-C18
            };
            ex.push(Event::Failed(code));
        }
        // Every other notification — resolving, connecting, redirect,
        // data available — is progress this crate does not report. They
        // arrive because the callback is registered for all of them, and
        // it is registered for all of them because `HANDLE_CLOSING` has
        // no flag of its own in `windows-sys`.
        _ => {}
    }
}

/// A WinHTTP handle, closed on drop.
///
/// One type for all three kinds — session, connect, request — because
/// `WinHttpCloseHandle` is the whole of what this crate does differently
/// between them, and it does not differ.
#[derive(Debug)]
pub(crate) struct Handle(*mut c_void);

// SAFETY: obligation 3 — WinHTTP handles are documented as usable from
// any thread. What the pointer identifies lives in WinHTTP, not in this
// process's Rust heap, and nothing here reads through it directly.
#[allow(
    unsafe_code, // unsafe-code-exception: amendment-C18
    reason = "WinHTTP handles are thread-agnostic; see obligation 3"
)]
unsafe impl Send for Handle {} // unsafe-code-exception: amendment-C18
#[allow(
    unsafe_code, // unsafe-code-exception: amendment-C18
    reason = "as above"
)]
unsafe impl Sync for Handle {} // unsafe-code-exception: amendment-C18

impl Drop for Handle {
    #[allow(
        unsafe_code, // unsafe-code-exception: amendment-C18
        reason = "closes a handle this type owns"
    )]
    fn drop(&mut self) {
        // SAFETY: `self.0` came from one of the three WinHTTP open calls
        // and has not been closed — this type is the only owner and is
        // not `Clone`.
        //
        // Closing a request handle with a read in flight is how
        // cancellation works: WinHTTP cancels the pending operation,
        // reports `REQUEST_ERROR`, and then `HANDLE_CLOSING`.
        unsafe { w::WinHttpCloseHandle(self.0) }; // unsafe-code-exception: amendment-C18
    }
}

/// What WinHTTP said went wrong, as a Win32 error code.
///
/// The code and nothing more: WinHTTP's own `FormatMessage` text needs
/// `winhttp.dll` loaded as a message source, and mapping the codes onto
/// this workspace's `ErrorKind` at this layer would be a second
/// vocabulary invented at the boundary — the same reason
/// `hclient-urlsession` reports what Apple said rather than a translation
/// of it. `session.rs` maps the handful that have an unambiguous kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("WinHTTP error {0}")]
pub struct Win32Error(pub u32);

#[allow(
    unsafe_code, // unsafe-code-exception: amendment-C18
    reason = "reads the thread's last-error after a failed call"
)]
fn last_error() -> Win32Error {
    // SAFETY: no preconditions; reads this thread's last-error value.
    Win32Error(unsafe { GetLastError() }) // unsafe-code-exception: amendment-C18
}

/// The session handle: one per transport, callback installed.
#[derive(Debug)]
pub(crate) struct Session(Handle);

impl Session {
    /// Opens an **asynchronous** session that resolves the proxy the way
    /// the machine says to.
    ///
    /// `WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY` is the whole reason this
    /// crate exists: WPAD discovery and any PAC script are WinHTTP's to
    /// run, per request, in the OS. `WINHTTP_FLAG_ASYNC` is what makes
    /// every later call complete through [`callback`] rather than
    /// blocking the caller's thread.
    #[allow(
        unsafe_code, // unsafe-code-exception: amendment-C18
        reason = "WinHttpOpen and WinHttpSetStatusCallback"
    )]
    pub(crate) fn open(agent: &str) -> Result<Self, Win32Error> {
        let agent = wide(agent);
        // SAFETY: `agent` is null-terminated and outlives the call, which
        // copies what it keeps; the two proxy parameters are the
        // documented nulls for an automatic access type.
        let h = unsafe {
            // unsafe-code-exception: amendment-C18
            w::WinHttpOpen(
                agent.as_ptr(),
                w::WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY,
                std::ptr::null(),
                std::ptr::null(),
                w::WINHTTP_FLAG_ASYNC,
            )
        };
        if h.is_null() {
            return Err(last_error());
        }
        let session = Self(Handle(h));
        // SAFETY: `h` is the handle just opened. `ALL_NOTIFICATIONS`
        // rather than a narrower mask because `HANDLE_CLOSING` — which
        // obligation 2 rests on — has no flag constant of its own.
        let prev = unsafe {
            // unsafe-code-exception: amendment-C18
            w::WinHttpSetStatusCallback(
                h,
                Some(callback),
                w::WINHTTP_CALLBACK_FLAG_ALL_NOTIFICATIONS,
                0,
            )
        };
        // `WINHTTP_INVALID_STATUS_CALLBACK` is the failure value, and it
        // is `-1` as a function pointer. A previous callback of `None` is
        // the success case for a fresh handle.
        if prev.is_some_and(|f| f as usize == usize::MAX) {
            return Err(last_error());
        }
        Ok(session)
    }

    /// Names the origin. No network happens here — `WinHttpConnect` only
    /// records the host and port for the requests that follow.
    #[allow(
        unsafe_code, // unsafe-code-exception: amendment-C18
        reason = "WinHttpConnect"
    )]
    pub(crate) fn connect(&self, host: &str, port: u16) -> Result<Connect, Win32Error> {
        let host = wide(host);
        // SAFETY: the session handle is live for the borrow, and `host`
        // is null-terminated and copied by the call.
        let h = unsafe { w::WinHttpConnect((self.0).0, host.as_ptr(), port, 0) }; // unsafe-code-exception: amendment-C18
        if h.is_null() {
            return Err(last_error());
        }
        Ok(Connect(Handle(h)))
    }
}

/// A named origin. Cheap, and dropped with the exchange.
#[derive(Debug)]
pub(crate) struct Connect(Handle);

impl Connect {
    #[allow(
        unsafe_code, // unsafe-code-exception: amendment-C18
        reason = "WinHttpOpenRequest"
    )]
    pub(crate) fn open_request(
        &self,
        verb: &str,
        target: &str,
        secure: bool,
    ) -> Result<Request, Win32Error> {
        let verb = wide(verb);
        let target = wide(target);
        let flags = if secure { w::WINHTTP_FLAG_SECURE } else { 0 };
        // SAFETY: both strings are null-terminated and copied by the
        // call; the three nulls are the documented defaults for version,
        // referrer and accept types.
        let h = unsafe {
            // unsafe-code-exception: amendment-C18
            w::WinHttpOpenRequest(
                (self.0).0,
                verb.as_ptr(),
                target.as_ptr(),
                std::ptr::null(),
                std::ptr::null(),
                std::ptr::null(),
                flags,
            )
        };
        if h.is_null() {
            return Err(last_error());
        }
        Ok(Request(Handle(h)))
    }
}

/// One request. Dropping it cancels whatever is in flight.
#[derive(Debug)]
pub(crate) struct Request(Handle);

impl Request {
    /// Hands WinHTTP an owned reference to the shared state.
    ///
    /// Set here rather than passed to `WinHttpSendRequest`, so that a
    /// request abandoned before the send still releases it: every path to
    /// `Drop` goes through `HANDLE_CLOSING` with a non-zero context.
    #[allow(
        unsafe_code, // unsafe-code-exception: amendment-C18
        reason = "WinHttpSetOption(CONTEXT_VALUE) with a leaked Arc"
    )]
    pub(crate) fn set_context(&self, ex: &Arc<Exchange>) -> Result<(), Win32Error> {
        let raw = Arc::into_raw(Arc::clone(ex)) as usize;
        // SAFETY: the option takes a `usize` by pointer and copies it.
        // The reference `raw` represents is released in `HANDLE_CLOSING`.
        let ok = unsafe {
            // unsafe-code-exception: amendment-C18
            w::WinHttpSetOption(
                (self.0).0,
                w::WINHTTP_OPTION_CONTEXT_VALUE,
                std::ptr::from_ref(&raw).cast::<c_void>(),
                u32::try_from(size_of::<usize>()).expect("a pointer is not that wide"),
            )
        };
        if ok == 0 {
            // The handover did not happen, so take the reference back
            // rather than leak it.
            //
            // SAFETY: `raw` is the pointer just produced and WinHTTP did
            // not keep it, so this is the only reconstitution.
            drop(unsafe { Arc::from_raw(raw as *const Exchange) }); // unsafe-code-exception: amendment-C18
            return Err(last_error());
        }
        Ok(())
    }

    /// Turns off the two things `Client` already does — see the crate
    /// doc.
    #[allow(
        unsafe_code, // unsafe-code-exception: amendment-C18
        reason = "WinHttpSetOption(DISABLE_FEATURE)"
    )]
    pub(crate) fn disable_redirects_and_cookies(&self) -> Result<(), Win32Error> {
        let flags: u32 = w::WINHTTP_DISABLE_REDIRECTS | w::WINHTTP_DISABLE_COOKIES;
        // SAFETY: the option takes a `u32` by pointer and copies it.
        let ok = unsafe {
            // unsafe-code-exception: amendment-C18
            w::WinHttpSetOption(
                (self.0).0,
                w::WINHTTP_OPTION_DISABLE_FEATURE,
                std::ptr::from_ref(&flags).cast::<c_void>(),
                u32::try_from(size_of::<u32>()).expect("four"),
            )
        };
        if ok == 0 {
            return Err(last_error());
        }
        Ok(())
    }

    /// Adds the caller's headers, synchronously — see the module doc on
    /// why not through `WinHttpSendRequest`.
    #[allow(
        unsafe_code, // unsafe-code-exception: amendment-C18
        reason = "WinHttpAddRequestHeaders"
    )]
    pub(crate) fn add_headers(&self, crlf: &str) -> Result<(), Win32Error> {
        if crlf.is_empty() {
            return Ok(());
        }
        let h = wide(crlf);
        // SAFETY: `h` is null-terminated; `u32::MAX` is WinHTTP's own
        // sentinel for "null-terminated, measure it yourself". The call
        // copies what it keeps.
        let ok = unsafe {
            // unsafe-code-exception: amendment-C18
            w::WinHttpAddRequestHeaders(
                (self.0).0,
                h.as_ptr(),
                u32::MAX,
                w::WINHTTP_ADDREQ_FLAG_ADD | w::WINHTTP_ADDREQ_FLAG_REPLACE,
            )
        };
        if ok == 0 {
            return Err(last_error());
        }
        Ok(())
    }

    /// Sends head and body. Completes with [`Event::SendComplete`].
    ///
    /// The body is stashed in the [`Exchange`] first, because the pointer
    /// handed over must stay valid until the completion — a `Bytes` is
    /// heap-stable and immutable, so a live clone is the whole of what
    /// that needs.
    #[allow(
        unsafe_code, // unsafe-code-exception: amendment-C18
        reason = "WinHttpSendRequest with a borrowed body pointer"
    )]
    pub(crate) fn send(&self, ex: &Arc<Exchange>, body: Option<Bytes>) -> Result<(), Win32Error> {
        let (ptr, len) = {
            let mut inner = ex.inner.lock().expect("winhttp exchange poisoned");
            inner.sending = body;
            match &inner.sending {
                Some(b) if !b.is_empty() => (
                    b.as_ptr().cast::<c_void>(),
                    u32::try_from(b.len()).unwrap_or(u32::MAX),
                ),
                _ => (std::ptr::null(), 0),
            }
        };
        // SAFETY: `ptr` points into the `Bytes` now held in
        // `inner.sending`, which is cleared only on `SENDREQUEST_COMPLETE`
        // — so the allocation outlives WinHTTP's use of it. Headers are
        // null because `add_headers` already put them on. The context is
        // zero here because `set_context` set it as an option, which is
        // the value WinHTTP will pass to the callback.
        let ok =
            unsafe { w::WinHttpSendRequest((self.0).0, std::ptr::null(), 0, ptr, len, len, 0) }; // unsafe-code-exception: amendment-C18
        if ok == 0 {
            return Err(last_error());
        }
        Ok(())
    }

    /// Asks for the response head. Completes with
    /// [`Event::HeadersAvailable`].
    #[allow(
        unsafe_code, // unsafe-code-exception: amendment-C18
        reason = "WinHttpReceiveResponse"
    )]
    pub(crate) fn receive_response(&self) -> Result<(), Win32Error> {
        // SAFETY: the reserved parameter is documented as null.
        let ok = unsafe { w::WinHttpReceiveResponse((self.0).0, std::ptr::null_mut()) }; // unsafe-code-exception: amendment-C18
        if ok == 0 {
            return Err(last_error());
        }
        Ok(())
    }

    /// The whole response head as WinHTTP holds it: the status line, the
    /// headers, CRLF-delimited, terminated by a blank line.
    ///
    /// Handed back as bytes so that `hclient_proto::head::parse_response`
    /// reads it — the same RFC 9112 §4 parser `hclient-proxy` uses for a
    /// `CONNECT` response, rather than a second header parser written
    /// here.
    #[allow(
        unsafe_code, // unsafe-code-exception: amendment-C18
        reason = "WinHttpQueryHeaders, twice: once for the length, once for the bytes"
    )]
    pub(crate) fn raw_headers(&self) -> Result<Vec<u8>, Win32Error> {
        let mut len: u32 = 0;
        // SAFETY: the documented way to ask for the size — a null buffer
        // and a zero length, which fails with
        // `ERROR_INSUFFICIENT_BUFFER` and writes the needed size.
        unsafe {
            // unsafe-code-exception: amendment-C18
            w::WinHttpQueryHeaders(
                (self.0).0,
                w::WINHTTP_QUERY_RAW_HEADERS_CRLF,
                std::ptr::null(),
                std::ptr::null_mut(),
                &raw mut len,
                std::ptr::null_mut(),
            )
        };
        if len == 0 {
            return Err(last_error());
        }
        // `len` is a byte count for a UTF-16 buffer.
        let mut utf16 = vec![0u16; (len as usize).div_ceil(2)];
        // SAFETY: the buffer is at least `len` bytes and `len` says so.
        let ok = unsafe {
            // unsafe-code-exception: amendment-C18
            w::WinHttpQueryHeaders(
                (self.0).0,
                w::WINHTTP_QUERY_RAW_HEADERS_CRLF,
                std::ptr::null(),
                utf16.as_mut_ptr().cast::<c_void>(),
                &raw mut len,
                std::ptr::null_mut(),
            )
        };
        if ok == 0 {
            return Err(last_error());
        }
        utf16.truncate((len as usize) / 2);
        // A response head is ASCII by RFC 9112 §5, and a header value
        // that is not is bytes the parser refuses rather than something
        // to transcode. `to_string_lossy` would invent replacement
        // characters inside a value; this keeps the bytes WinHTTP has.
        Ok(utf16.iter().map(|&u| u as u8).collect())
    }

    /// Lends the read buffer to WinHTTP. Completes with
    /// [`Event::ReadComplete`], whose `0` is end of body.
    #[allow(
        unsafe_code, // unsafe-code-exception: amendment-C18
        reason = "WinHttpReadData over a buffer this hands to WinHTTP; see obligation 1"
    )]
    pub(crate) fn read(&self, ex: &Arc<Exchange>) -> Result<(), Win32Error> {
        let (ptr, len) = {
            let mut inner = ex.inner.lock().expect("winhttp exchange poisoned");
            let b = match std::mem::replace(
                &mut inner.buf,
                Buf::Loaned {
                    ptr: std::ptr::null_mut(),
                    len: 0,
                },
            ) {
                Buf::Home(b) => b,
                // A second read while one is in flight would hand the
                // same buffer over twice. `body.rs` polls one read at a
                // time, so this is a bug here rather than a caller's.
                Buf::Loaned { .. } => unreachable!("two reads in flight on one exchange"),
            };
            let len = b.len();
            let ptr = Box::into_raw(b).cast::<u8>();
            inner.buf = Buf::Loaned { ptr, len };
            (ptr, len)
        };
        // SAFETY: obligation 1. The buffer is `Loaned` from here until a
        // completion reclaims it, and while it is, no `Box` exists for
        // safe code to read through.
        let ok = unsafe {
            // unsafe-code-exception: amendment-C18
            w::WinHttpReadData(
                (self.0).0,
                ptr.cast::<c_void>(),
                u32::try_from(len).unwrap_or(u32::MAX),
                std::ptr::null_mut(),
            )
        };
        if ok == 0 {
            // The read never started, so nothing will reclaim it.
            reclaim(ex);
            return Err(last_error());
        }
        Ok(())
    }
}
