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

use crate::error::Win32Error;
use std::ffi::c_void;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};

use bytes::{BufMut, Bytes, BytesMut};
use futures_channel::mpsc;
use futures_core::Stream as _;
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
    Home(BytesMut),
    /// Lent to WinHTTP until `READ_COMPLETE` or `HANDLE_CLOSING`.
    ///
    /// **The buffer is kept here rather than given away**, which is what
    /// changed when it became a `BytesMut`: the allocation must stay alive
    /// and unmoved for as long as WinHTTP holds the pointer `read` handed
    /// it, and holding the `BytesMut` is how. That pointer is into the
    /// **spare capacity**, so the initialised prefix — always empty here,
    /// because every completed read is split off at once — is untouched.
    ///
    /// **This arm carries no raw pointer**, which is the second thing the
    /// `BytesMut` bought: `read` computes the pointer, hands it to WinHTTP
    /// and forgets it, and `reclaim` moves the buffer back out rather than
    /// reconstructing it. Moving this enum moves the `BytesMut` struct and
    /// not its heap allocation, so what WinHTTP holds stays valid. What
    /// must not happen while lent is a `reserve` on `held`, which could
    /// reallocate; nothing between `read` and `reclaim` touches it, and
    /// `take_read` refuses this arm outright.
    Loaned { held: BytesMut },
}

#[derive(Debug)]
struct Inner {
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
    /// What WinHTTP said, in order.
    ///
    /// **An unbounded channel rather than a `VecDeque` and a hand-rolled
    /// waker**, for `hclient-urlsession`'s reason and with the same
    /// producer: WinHTTP's status callback is a **synchronous C
    /// function**, invoked on a thread this crate does not own, and it
    /// cannot wait. A bounded channel would make a full queue a dropped
    /// completion, which here is not a slow body but a lost `ReadComplete`
    /// — the buffer would never be reclaimed. Nothing is lost that
    /// existed: the `VecDeque` was unbounded too.
    tx: mpsc::UnboundedSender<Event>,
    rx: Mutex<mpsc::UnboundedReceiver<Event>>,
}

// **`Exchange` needs no `unsafe impl Send`/`Sync` any more**, and the two
// it carried are gone. They existed because `Inner` held a raw pointer:
// the buffer was a `Box<[u8]>` given away with `Box::into_raw` and taken
// back with `Box::from_raw`, and the note here argued that what crossed a
// thread was ownership of an allocation, which is what a `Box` may do.
//
// With the buffer a `BytesMut` that `Buf::Loaned` **holds**, `read`
// computes the pointer, hands it to WinHTTP and keeps none of it — so
// every field of `Exchange` is `Send + Sync` already and the auto impls
// apply. Two `unsafe` blocks removed from a crate not one line of which
// has ever been executed, which is where they were least affordable.

impl Exchange {
    pub(crate) fn new() -> Self {
        let (tx, rx) = mpsc::unbounded();
        Self {
            inner: Mutex::new(Inner {
                buf: Buf::Home(BytesMut::with_capacity(READ_BUF)),
                sending: None,
            }),
            tx,
            rx: Mutex::new(rx),
        }
    }

    fn push(&self, e: Event) {
        // A closed receiver means the polling side went away, which is an
        // ordinary end: the callback has no one to tell and nothing it
        // could do about it.
        let _ = self.tx.unbounded_send(e);
    }

    /// The next thing WinHTTP said, or `Pending` with `cx` registered.
    ///
    /// The pop / register / pop-again dance this replaced was the ordinary
    /// lost-wakeup race written out by hand — the callback can push
    /// between the first pop and the lock. A channel owns that race.
    ///
    /// The `Mutex` is over the **receiver** and is never contended: one
    /// body polls one exchange. It is there because callers hold an
    /// `Arc<Exchange>`, so this takes `&self` where `Stream::poll_next`
    /// wants `&mut`.
    pub(crate) fn poll_next(&self, cx: &mut Context<'_>) -> Poll<Event> {
        let mut rx = self.rx.lock().expect("winhttp receiver poisoned");
        match Pin::new(&mut *rx).poll_next(cx) {
            Poll::Ready(Some(e)) => Poll::Ready(e),
            // Every sender is gone, which happens only when the exchange
            // is being torn down; `HANDLE_CLOSING` is the last event and
            // arrives before that.
            Poll::Ready(None) => Poll::Pending,
            Poll::Pending => Poll::Pending,
        }
    }

    /// The bytes WinHTTP just wrote, **split off rather than copied**.
    ///
    /// `split().freeze()` hands back a `Bytes` sharing this buffer's
    /// allocation, where `Bytes::copy_from_slice` allocated and memcpy'd
    /// once per chunk. The trade is stated rather than assumed: the
    /// returned handle keeps the whole allocation alive until it is
    /// dropped, so a caller holding one small frame pins up to `READ_BUF`.
    /// The next `read` reserves again, which reuses the allocation when
    /// the previous chunk has been dropped and allocates a fresh one when
    /// it has not — so the worst case is today's allocation count with
    /// today's copy removed, and the ordinary case is one allocation per
    /// `READ_BUF` bytes rather than one per chunk.
    ///
    /// Callable only between reads: the buffer is `Home` exactly then,
    /// and a `Loaned` one here would mean the state machine handed out an
    /// `Event::ReadComplete` without reclaiming, which is a bug in this
    /// file rather than something a caller can cause.
    pub(crate) fn take_read(&self, n: usize) -> Bytes {
        let mut inner = self.inner.lock().expect("winhttp exchange poisoned");
        match &mut inner.buf {
            Buf::Home(b) => {
                let n = n.min(b.capacity() - b.len());
                // SAFETY: WinHTTP wrote `n` bytes into the spare capacity
                // this buffer lent it — `n` is the count it reported in
                // `WINHTTP_CALLBACK_STATUS_READ_COMPLETE`, and `read`
                // lent exactly `spare_capacity_mut()`. The `min` above
                // bounds it by what was lent rather than trusting it.
                unsafe { b.advance_mut(n) }; // unsafe-code-exception: amendment-C18
                b.split().freeze()
            }
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
    // **No `unsafe` here any more**, and that is what the `BytesMut`
    // bought besides the copy: the allocation was never given away, so
    // taking it back is moving a value out of an enum arm. What this used
    // to be was `Box::from_raw` over a pointer `Box::into_raw` had
    // produced, with the obligation that WinHTTP was finished with it —
    // the obligation is unchanged and is now discharged by the state
    // machine alone.
    if let Buf::Loaned { held, .. } =
        std::mem::replace(&mut inner.buf, Buf::Home(BytesMut::with_capacity(READ_BUF)))
    {
        inner.buf = Buf::Home(held);
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

#[allow(
    unsafe_code, // unsafe-code-exception: amendment-C18
    reason = "reads the thread's last-error after a failed call"
)]
fn last_error() -> Win32Error {
    // SAFETY: no preconditions; reads this thread's last-error value.
    Win32Error(unsafe { GetLastError() }) // unsafe-code-exception: amendment-C18
}

/// Sets a `DWORD`-valued option on any handle.
///
/// Every option this crate sets bar the context pointer is one `u32`, so
/// the FFI is written once here rather than at each of them: three of the
/// four callers arrived with the HTTP/2 and HTTP/3 work, and a fourth
/// copy of the same six lines would have been four places to get the
/// buffer length wrong.
///
/// **A refused option is returned rather than swallowed**, which is the
/// decision the callers rest on. WinHTTP answers `ERROR_WINHTTP_INVALID_
/// OPTION` for an option this Windows does not have — every one of these
/// is newer than the API itself — and .NET's `WinHttpHandler` logs that
/// and carries on, which is the *silently ignored setting* this workspace
/// closes wherever it finds one. Here it becomes a named error and the
/// caller decides.
#[allow(
    unsafe_code, // unsafe-code-exception: amendment-C18
    reason = "WinHttpSetOption over a DWORD the call copies"
)]
fn set_dword(handle: *const c_void, option: u32, value: u32) -> Result<(), Win32Error> {
    // SAFETY: the option takes a `u32` by pointer and copies it before
    // returning; `handle` is borrowed from a live wrapper by every
    // caller.
    let ok = unsafe {
        // unsafe-code-exception: amendment-C18
        w::WinHttpSetOption(
            handle,
            option,
            std::ptr::from_ref(&value).cast::<c_void>(),
            u32::try_from(size_of::<u32>()).expect("four"),
        )
    };
    if ok == 0 {
        return Err(last_error());
    }
    Ok(())
}

/// Reads a `DWORD`-valued option back off a handle.
#[allow(
    unsafe_code, // unsafe-code-exception: amendment-C18
    reason = "WinHttpQueryOption into a DWORD this frame owns"
)]
fn query_dword(handle: *mut c_void, option: u32) -> Result<u32, Win32Error> {
    let mut value: u32 = 0;
    let mut len = u32::try_from(size_of::<u32>()).expect("four");
    // SAFETY: the buffer is one `u32` and `len` says so, which is what
    // this option's documented type is; the call writes at most that.
    let ok = unsafe {
        // unsafe-code-exception: amendment-C18
        w::WinHttpQueryOption(
            handle,
            option,
            std::ptr::from_mut(&mut value).cast::<c_void>(),
            &raw mut len,
        )
    };
    if ok == 0 {
        return Err(last_error());
    }
    Ok(value)
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

    /// Asks WinHTTP to keep an idle HTTP/2 or HTTP/3 connection alive.
    ///
    /// `WINHTTP_OPTION_HTTP2_KEEPALIVE` and
    /// `WINHTTP_OPTION_HTTP3_KEEPALIVE` are documented on the **session**
    /// handle and take a timeout in milliseconds, after which WinHTTP
    /// begins sending HTTP/2 `PING` frames or QUIC keep-alives on a
    /// connection with no activity. That is the OS holding the clock this
    /// workspace holds itself one crate over — `Native::h2_keep_alive`
    /// spawns a driver to send the ping and `hclient-h3` sets quinn's
    /// `keep_alive_interval` — and it is the reason this is reachable at
    /// all here, where nothing of ours drives a pooled connection.
    ///
    /// Both are set from one call for the reason `Native::watching_1xx`
    /// switches on both protocols at once: a keep-alive that held on one
    /// of the two would be a promise the other could not keep, and which
    /// of them a request gets is the server's choice rather than the
    /// caller's.
    pub(crate) fn set_keep_alive(&self, millis: u32) -> Result<(), Win32Error> {
        set_dword((self.0).0, w::WINHTTP_OPTION_HTTP2_KEEPALIVE, millis)?;
        set_dword((self.0).0, w::WINHTTP_OPTION_HTTP3_KEEPALIVE, millis)
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

    /// Names the advanced HTTP versions this request may negotiate.
    ///
    /// `WINHTTP_OPTION_ENABLE_HTTP_PROTOCOL` is a bitmask of
    /// `WINHTTP_PROTOCOL_FLAG_HTTP2` (`0x1`) and
    /// `WINHTTP_PROTOCOL_FLAG_HTTP3` (`0x2`), and its documented default
    /// is `0x0` — *"restricts the request to HTTP/1.1 and prior"*. So
    /// HTTP/1.1 is not something a caller can switch off, and the mask
    /// says only what may be negotiated **above** it.
    ///
    /// Set on the **request** handle rather than the session, although
    /// WinHTTP accepts either. Per request is what lets a
    /// `RequireVersion` demand narrow the mask for one exchange without
    /// changing what every other request on this transport offers, which
    /// is the whole of `session.rs`'s `mask_for`. .NET's
    /// `WinHttpHandler` sets it on the request handle too, from
    /// `SetRequestHandleHttp2Options`.
    pub(crate) fn set_protocols(&self, mask: u32) -> Result<(), Win32Error> {
        set_dword((self.0).0, w::WINHTTP_OPTION_ENABLE_HTTP_PROTOCOL, mask)
    }

    /// Refuses a version outside the mask rather than falling back to it.
    ///
    /// `WINHTTP_OPTION_HTTP_PROTOCOL_REQUIRED` — *"prevents protocol
    /// versions other than those enabled by
    /// **WINHTTP_OPTION_ENABLE_HTTP_PROTOCOL** from being used for the
    /// request"* — and it is what makes
    /// [`Capabilities::version_select`](hclient_core::Capabilities::version_select)
    /// honest here. Without it a demand could only be *checked* after the
    /// head came back, which is `check_version`'s own definition of a
    /// check placed too late: the request would already be at the server.
    ///
    /// **The buffer type is the one thing here the documentation does not
    /// state.** Every other boolean option in WinHTTP takes a `DWORD`,
    /// and that is the assumption. It is a safe one to make in this
    /// direction and it is worth saying why: a wrong length is
    /// `ERROR_INVALID_PARAMETER` from `WinHttpSetOption`, which
    /// `session.rs` turns into a named refusal *before the request is
    /// sent* — so the failure of the guess is a request that does not go,
    /// never one that goes out over a version the caller ruled out.
    pub(crate) fn require_protocols(&self) -> Result<(), Win32Error> {
        set_dword((self.0).0, w::WINHTTP_OPTION_HTTP_PROTOCOL_REQUIRED, 1)
    }

    /// Which advanced version WinHTTP actually used, as the same bitmask.
    ///
    /// **`WINHTTP_QUERY_VERSION` cannot answer this, and this crate's own
    /// doc named it as the way to.** That header query reads the status
    /// line, and an HTTP/2 or HTTP/3 response has none — WinHTTP
    /// synthesises `HTTP/1.1` for the raw header block, so a client
    /// reading it reports every h2 and h3 response as HTTP/1.1.
    /// `WINHTTP_OPTION_HTTP_PROTOCOL_USED` is the option that says
    /// otherwise, and .NET's `WinHttpResponseParser` bypasses the status
    /// line entirely when it answers non-zero.
    ///
    /// Queried on the request handle once the head is available. `0` is
    /// HTTP/1.1 or prior, which is also what an older Windows without the
    /// option effectively means — hence the caller treating a failure as
    /// `0` rather than as an error.
    pub(crate) fn protocol_used(&self) -> Result<u32, Win32Error> {
        query_dword((self.0).0, w::WINHTTP_OPTION_HTTP_PROTOCOL_USED)
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
            let mut b = match std::mem::replace(&mut inner.buf, Buf::Home(BytesMut::new())) {
                Buf::Home(b) => b,
                // A second read while one is in flight would hand the
                // same buffer over twice. `body.rs` polls one read at a
                // time, so this is a bug here rather than a caller's.
                Buf::Loaned { .. } => unreachable!("two reads in flight on one exchange"),
            };
            // Reserving before lending is what makes the pointer valid for
            // `len`, and it is also the only place this buffer may
            // reallocate — which is why it happens here, before the
            // pointer exists, rather than anywhere between here and
            // `reclaim`.
            b.reserve(READ_BUF);
            let spare = b.spare_capacity_mut();
            let len = spare.len();
            let ptr = spare.as_mut_ptr().cast::<u8>();
            inner.buf = Buf::Loaned { held: b };
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
