//! `DNSServiceQueryRecord` — macOS, iOS and their siblings.
//!
//! # Why not `res_9_query`, which is what this crate used to call
//!
//! Two reasons, and the first is not a matter of taste.
//!
//! **It cannot be used from more than one thread.** Measured on macOS 27:
//! the same query, sixty-four times, answers 64/64 run one after another
//! and **12/64 run from eight threads**, with 46 of the failures leaving
//! the answer buffer untouched — the call returning before anything went
//! out. `sys/mod.rs` names this as one of the two things a `res_query`
//! backend needs, "a libc whose resolver state is per-thread", and for
//! Apple that half had been taken from `libresolv.9.tbd`, which shows a
//! symbol exists and says nothing about its state.
//!
//! It matters because this crate's caller runs lookups on a blocking pool:
//! two concurrent HTTPS lookups on a Mac were most of the way to failing,
//! while the capability said they would work.
//!
//! **And it answers from the wrong resolver.** `resolver(5)` describes
//! macOS as running several DNS clients with a "Super" meta-client routing
//! between them by best domain match; `/etc/resolv.conf` is "configuration
//! for the default (or `primary`) DNS resolver client". `resolver(3)` — the
//! page Apple ships for `res_query` — mentions none of it: it "reads the
//! configuration file" and sends "to the local server". So a VPN's
//! split-DNS zone and the per-domain files in `/etc/resolver/` are the
//! Super client's, and that call does not consult them.
//!
//! Measured on the one supplemental resolver every Mac already has —
//! `scutil --dns` lists `domain: local, options: mdns` as a client of its
//! own. Asking this machine for its own name, type A:
//!
//! | call | answer |
//! |---|---|
//! | `res_9_query` | **failed, rcode 3** — the primary nameserver, which knows nothing of `.local` |
//! | `DNSServiceQueryRecord` | `172.21.0.151` |
//!
//! The control is an ordinary unicast name, which both answer.
//!
//! # What this one is
//!
//! `DNSServiceQueryRecord` is the API Apple documents for asking about a
//! record type, and its callback is this crate's [`Record`] almost field
//! for field: `rrtype`, `rrclass`, `rdlen`, "the raw rdata of the resource
//! record", a TTL, the full name, and `interfaceIndex` — "the interface on
//! which the query was **resolved**", which is the routing `res_9_query`
//! does not get.
//!
//! It hands back RDATA rather than a message, so this is the one Unix
//! platform that does not walk one — the same shape as Windows, for the
//! same reason, and with the same consequence: no header, so no `TC` and
//! no rcode beyond what an error code carries.
//!
//! # Blocking, and why the loop terminates
//!
//! The API is asynchronous over a socket. `DNSServiceProcessResult` runs
//! the callback for whatever the daemon has sent; this module waits on the
//! socket and calls it until the callback says the answer is complete,
//! which makes the whole thing blocking, as this crate's seam is.
//!
//! # The flag that says "there is no such record", which is not the one
//! # its name suggests
//!
//! **Without `kDNSServiceFlagsReturnIntermediates` the daemon never tells
//! a client that a name has no record of the type asked for.** Measured on
//! macOS 27, `no-such-name-xyzzy.example.com` type 65, one query per row:
//!
//! | flags | what arrived |
//! |---|---|
//! | none | **nothing at all**, in four seconds |
//! | `kDNSServiceFlagsSuppressUnusable` | nothing at all |
//! | `kDNSServiceFlagsTimeout` | nothing until `kDNSServiceErr_Timeout` at 30.0 s |
//! | `kDNSServiceFlagsReturnIntermediates` | `kDNSServiceErr_NoSuchRecord` at **1.2 ms** |
//!
//! The header describes that flag as being about *intermediate* results —
//! the CNAMEs a chain passes through — and says nothing about negatives.
//! It is nonetheless the difference between an answer and an indefinite
//! wait, and it is why `dns-sd -Q` reports "No Such Record" in
//! milliseconds where a first version of this module waited thirty
//! seconds and then reported a timeout.
//!
//! `kDNSServiceFlagsTimeout` looked like the way to bound the wait and is
//! worse than useless: the header says only that the query "will be
//! stopped irrespective of whether a response was given earlier or not",
//! and measured, it also suppresses the negative that would otherwise
//! arrive. It is not passed.
//!
//! So the bound is this module's own: the query's socket is polled with a
//! deadline, and a deadline that expires is [`Error::NoResponse`]. Two
//! things end the loop before it — a callback without
//! `kDNSServiceFlagsMoreComing`, and an error code — and with the flag
//! above both arrive as fast as the resolver answers.
#![allow(
    unsafe_code, // unsafe-code-exception: amendment-C8
    reason = "DNSServiceQueryRecord is the only Apple API that answers for an arbitrary record type through the system's own resolver; see spec amendment C8"
)]

use crate::error::Error;
use crate::sys::query_name;
use crate::{CLASS_IN, Record};
use core::ffi::{c_char, c_int, c_short, c_uint, c_void};
use std::time::{Duration, Instant};

/// The wire message never appears, but every type can still be asked for:
/// the daemon hands over one record at a time with its RDATA.
pub(crate) fn support() -> crate::Support {
    crate::Support::any()
}

/// An opaque handle to one query, from `dns_sd.h`.
#[repr(transparent)]
#[derive(Clone, Copy)]
struct ServiceRef(*mut c_void);

/// `kDNSServiceFlagsMoreComing`, `kDNSServiceFlagsTimeout`,
/// `kDNSServiceInterfaceIndexAny`, `kDNSServiceClass_IN` and the three
/// error codes this module reads — all copied from the `dns_sd.h` in this
/// machine's SDK rather than from memory.
const MORE_COMING: u32 = 0x1;
const RETURN_INTERMEDIATES: u32 = 0x1000;
const INTERFACE_ANY: u32 = 0;
const ERR_NO_ERROR: i32 = 0;
const ERR_NO_SUCH_RECORD: i32 = -65554;

/// `POLLIN`, from `sys/poll.h`.
const POLLIN: c_short = 0x0001;

/// How long this module waits for the daemon before giving up.
///
/// **Ours, and it has to be**: the daemon does not end a query that
/// nothing answers, and the flag that would have it do so suppresses the
/// answers that do arrive. Thirty seconds is the figure that flag itself
/// uses — measured, since the header says it is "determined by the system
/// and cannot be configured" — so a query the daemon would have abandoned
/// is abandoned here at the same moment, and a query that is answered is
/// answered as fast as the resolver manages.
const DEADLINE: Duration = Duration::from_secs(30);

type QueryRecordReply = unsafe extern "C" fn(
    // unsafe-code-exception: amendment-C8
    sd_ref: ServiceRef,
    flags: u32,
    interface_index: u32,
    error_code: i32,
    fullname: *const c_char,
    rrtype: u16,
    rrclass: u16,
    rdlen: u16,
    rdata: *const c_void,
    ttl: u32,
    context: *mut c_void,
);

// `libSystem` carries these; there is no separate library to name.
unsafe extern "C" {
    // unsafe-code-exception: amendment-C8
    fn DNSServiceQueryRecord(
        sd_ref: *mut ServiceRef,
        flags: u32,
        interface_index: u32,
        fullname: *const c_char,
        rrtype: u16,
        rrclass: u16,
        callback: QueryRecordReply,
        context: *mut c_void,
    ) -> i32;
    fn DNSServiceProcessResult(sd_ref: ServiceRef) -> i32;
    fn DNSServiceRefDeallocate(sd_ref: ServiceRef);
    /// The socket the daemon answers on. Apple documents it for exactly
    /// this: "the client is responsible for calling
    /// `DNSServiceProcessResult`" when data arrives on it.
    fn DNSServiceRefSockFD(sd_ref: ServiceRef) -> c_int;
}

/// `struct pollfd`, from `sys/poll.h`.
#[repr(C)]
struct PollFd {
    fd: c_int,
    events: c_short,
    revents: c_short,
}

unsafe extern "C" {
    // unsafe-code-exception: amendment-C8
    fn poll(fds: *mut PollFd, nfds: c_uint, timeout: c_int) -> c_int;
}

/// Deallocates the query on every exit path.
///
/// A guard rather than a call at the end: the loop below returns early on
/// an error from the daemon, and a `Drop` impl is the only way to be sure
/// that path releases the connection too.
struct Query(ServiceRef);

impl Drop for Query {
    fn drop(&mut self) {
        // SAFETY: `self.0` came from a `DNSServiceQueryRecord` that
        // returned `kDNSServiceErr_NoError`, and this type is the only
        // owner — it is not `Clone` and the handle is never copied out.
        unsafe { DNSServiceRefDeallocate(self.0) }; // unsafe-code-exception: amendment-C8
    }
}

/// What the callback fills in, and what the loop reads.
#[derive(Default)]
struct Collected {
    records: Vec<Record>,
    /// The first error the daemon reported, if any.
    failed: Option<i32>,
    /// Set when a callback arrives without `kDNSServiceFlagsMoreComing`,
    /// or on any error: either way there is nothing further to wait for.
    done: bool,
}

/// The completion callback. Runs on the thread inside
/// `DNSServiceProcessResult`, so it borrows the caller's own state rather
/// than owning anything.
unsafe extern "C" fn reply(
    // unsafe-code-exception: amendment-C8
    _sd_ref: ServiceRef,
    flags: u32,
    _interface_index: u32,
    error_code: i32,
    fullname: *const c_char,
    rrtype: u16,
    rrclass: u16,
    rdlen: u16,
    rdata: *const c_void,
    ttl: u32,
    context: *mut c_void,
) {
    // unsafe-code-exception: amendment-C8
    // SAFETY: `context` is the pointer `query` passed to
    // `DNSServiceQueryRecord`, which borrows a `Collected` that outlives
    // the loop driving this callback. The callback is never called after
    // `DNSServiceRefDeallocate`, which the `Query` guard performs after
    // that loop.
    let out = unsafe { &mut *context.cast::<Collected>() }; // unsafe-code-exception: amendment-C8

    if error_code != ERR_NO_ERROR {
        out.failed = Some(error_code);
        out.done = true;
        return;
    }

    // SAFETY: `fullname` is the NUL-terminated name the daemon allocated
    // for this call; the `CStr` borrows for this expression only.
    let name = unsafe { core::ffi::CStr::from_ptr(fullname) }; // unsafe-code-exception: amendment-C8
    let Ok(name) = name.to_str() else {
        // A name that is not UTF-8 cannot be a host name, and a lossy
        // conversion would name a different one.
        out.failed = Some(ERR_NO_SUCH_RECORD);
        out.done = true;
        return;
    };

    // SAFETY: `rdata` and `rdlen` are the daemon's own pointer and length
    // for this record, valid for the duration of the callback. The bytes
    // are copied, so nothing borrows past it. An empty record is reported
    // as a null pointer with a zero length.
    let bytes = if rdata.is_null() || rdlen == 0 {
        Vec::new()
    } else {
        unsafe {
            // unsafe-code-exception: amendment-C8
            core::slice::from_raw_parts(rdata.cast::<u8>(), usize::from(rdlen))
        }
        .to_vec()
    };

    out.records.push(Record::new(
        name.strip_suffix('.').unwrap_or(name),
        rrtype,
        rrclass,
        Duration::from_secs(u64::from(ttl)),
        bytes,
    ));
    // **The end of the answer is a callback without this flag**, which is
    // the daemon saying it has nothing more queued for this query.
    out.done = flags & MORE_COMING == 0;
}

/// Asks the system resolver for `name`'s records of type `rtype`.
///
/// Blocking: `DNSServiceProcessResult` waits for the daemon.
pub(crate) fn query(name: &str, rtype: u16) -> Result<Vec<Record>, Error> {
    // Refused here for the same reason every other backend refuses here:
    // one answer for a name with no wire form, on every platform.
    let cname = query_name(name)?;
    let mut collected = Collected::default();

    let mut handle = ServiceRef(core::ptr::null_mut());
    // SAFETY: `cname` outlives the call and the loop below. `handle` is a
    // live local the callee fills in. `context` borrows `collected`, which
    // outlives the `Query` guard and therefore every callback.
    let started = unsafe {
        // unsafe-code-exception: amendment-C8
        DNSServiceQueryRecord(
            &raw mut handle,
            // **The one flag, and the header does not say why it is
            // needed**: without it the daemon never reports that a name
            // has no record of this type. See this module's table.
            RETURN_INTERMEDIATES,
            INTERFACE_ANY,
            cname.as_ptr(),
            rtype,
            CLASS_IN,
            reply,
            std::ptr::from_mut(&mut collected).cast::<c_void>(),
        )
    };
    if started != ERR_NO_ERROR {
        // Nothing was created, so there is nothing to deallocate.
        return Err(Error::Platform {
            code: started.unsigned_abs(),
        });
    }
    let query = Query(handle);
    let started = Instant::now();

    // **`collected` is mutated through the pointer the daemon calls back
    // through**, inside `DNSServiceProcessResult`, so clippy is right that
    // nothing in this body touches it and wrong that the loop cannot end.
    // Stated rather than silenced: the flag is the callback's, and the
    // three ways it becomes true are in this module's header.
    #[allow(
        clippy::while_immutable_condition,
        reason = "the callback writes it through `context`; see above"
    )]
    while !collected.done {
        let left = DEADLINE.saturating_sub(started.elapsed());
        if left.is_zero() {
            return Err(Error::NoResponse);
        }
        let mut waiting = PollFd {
            // SAFETY: the handle the call above filled in, owned by
            // `query` and not yet deallocated.
            fd: unsafe { DNSServiceRefSockFD(query.0) }, // unsafe-code-exception: amendment-C8
            events: POLLIN,
            revents: 0,
        };
        // `as` rather than `try_into`: the deadline is a constant well
        // under `c_int`'s range and the subtraction only shrinks it.
        let millis = left.as_millis().min(i128::from(c_int::MAX).unsigned_abs()) as c_int;
        // SAFETY: one live `PollFd` and a count of one.
        let ready = unsafe { poll(&raw mut waiting, 1, millis) }; // unsafe-code-exception: amendment-C8
        if ready == 0 {
            return Err(Error::NoResponse);
        }
        if ready < 0 {
            // An interrupted wait is not a failure; anything else is the
            // socket's, and there is nothing to read.
            continue;
        }
        // SAFETY: as above. The daemon has data, so this runs the callback
        // rather than blocking.
        let processed = unsafe { DNSServiceProcessResult(query.0) }; // unsafe-code-exception: amendment-C8
        if processed != ERR_NO_ERROR {
            return Err(Error::Platform {
                code: processed.unsigned_abs(),
            });
        }
    }

    match collected.failed {
        None => Ok(collected.records),
        // **The daemon does not separate "no such name" from "no records
        // of this type"** — both are `kDNSServiceErr_NoSuchRecord`, and
        // there is no header to read an rcode out of. So this crate
        // reports the answer it can stand behind: no records. A caller
        // that needs the other distinction finds a missing name in the
        // address lookup, where `getaddrinfo` does report it.
        Some(ERR_NO_SUCH_RECORD) => Ok(Vec::new()),
        Some(code) => Err(Error::Platform {
            code: code.unsigned_abs(),
        }),
    }
}
