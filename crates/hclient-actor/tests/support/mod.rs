//! A transport that cannot cross a thread, standing in for
//! `Native<Embassy, ..>` on a host where no TAP device is needed.
//!
//! It holds an `Rc`, which is the same reason embassy's cannot:
//! `embassy_net::Stack<'d>` is `&'d RefCell<Inner>` and that crate carries
//! no `unsafe impl Send` anywhere. Using a double rather than the real
//! runtime is what lets the boundary be tested at all today — the real one
//! needs a TAP device, and `no_std` is still out.

use bytes::Bytes;
use hclient_core::unversioned::Transport;
use hclient_core::{Capabilities, Error, ErrorKind, RequestBody};
use std::cell::Cell;
use std::rc::Rc;

/// What the local transport did, readable from the test.
#[derive(Debug, Default)]
pub struct Log {
    pub requests: Cell<usize>,
    pub dropped_midway: Cell<bool>,
}

pub struct Local {
    pub log: Rc<Log>,
    pub body: &'static [u8],
    /// Never answer, so a test can drop the caller's future while the
    /// exchange is genuinely in flight.
    pub hang: bool,
    caps: Capabilities,
}

impl Local {
    pub fn new(body: &'static [u8]) -> Self {
        Self {
            log: Rc::new(Log::default()),
            body,
            hang: false,
            caps: Capabilities::default(),
        }
    }

    /// Only `tests/cancel.rs` needs this, and a shared `mod support` is
    /// compiled into every test binary — so the two that do not use it
    /// would warn without this.
    #[allow(dead_code, reason = "used by tests/cancel.rs alone")]
    pub fn hanging() -> Self {
        Self {
            hang: true,
            ..Self::new(b"")
        }
    }
}

/// Sets a flag if dropped before the exchange finished — which is how a
/// test observes that a cancellation reached the far side of the channel.
struct Guard(Rc<Log>, bool);

impl Drop for Guard {
    fn drop(&mut self) {
        if !self.1 {
            self.0.dropped_midway.set(true);
        }
    }
}

impl Transport for Local {
    type Body = http_body_util::Full<Bytes>;
    type Error = Error;

    async fn execute(
        &self,
        _req: http::Request<RequestBody>,
    ) -> Result<http::Response<Self::Body>, Self::Error> {
        self.log.requests.set(self.log.requests.get() + 1);
        let mut guard = Guard(Rc::clone(&self.log), false);
        if self.hang {
            std::future::pending::<()>().await;
        }
        guard.1 = true;
        http::Response::builder()
            .status(200)
            .body(http_body_util::Full::new(Bytes::from_static(self.body)))
            .map_err(|e| Error::new(ErrorKind::Other, e))
    }

    fn to_error(&self, e: Self::Error) -> Error {
        e
    }

    fn capabilities(&self) -> &Capabilities {
        &self.caps
    }
}
