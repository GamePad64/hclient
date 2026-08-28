//! Two synthetic stacks: one whose connection crosses a thread and one
//! whose does not. They are the whole subject — the adapter's claim is
//! about which of the two it can make, so a test needs both.

use core::net::SocketAddr;
use embedded_io_async::{ErrorKind, ErrorType, Read, Write};
use embedded_nal_async::TcpConnect;
use std::sync::{Arc, Mutex};

/// What a connection did, readable from the test.
#[derive(Debug, Default)]
pub struct Log {
    pub written: Vec<u8>,
    pub flushes: usize,
}

/// A connection that serves canned bytes and records writes. `Send`.
#[derive(Debug)]
pub struct SendConn {
    pub to_read: std::collections::VecDeque<u8>,
    pub log: Arc<Mutex<Log>>,
}

impl ErrorType for SendConn {
    type Error = ErrorKind;
}

impl Read for SendConn {
    async fn read(&mut self, buf: &mut [u8]) -> Result<usize, Self::Error> {
        let n = buf.len().min(self.to_read.len());
        for slot in buf.iter_mut().take(n) {
            *slot = self.to_read.pop_front().expect("checked by the min above");
        }
        Ok(n)
    }
}

impl Write for SendConn {
    async fn write(&mut self, buf: &[u8]) -> Result<usize, Self::Error> {
        self.log.lock().unwrap().written.extend_from_slice(buf);
        Ok(buf.len())
    }
    async fn flush(&mut self) -> Result<(), Self::Error> {
        self.log.lock().unwrap().flushes += 1;
        Ok(())
    }
}

#[derive(Debug)]
pub struct SendStack {
    pub body: &'static [u8],
    pub log: Arc<Mutex<Log>>,
}

impl TcpConnect for SendStack {
    type Error = ErrorKind;
    type Connection<'a>
        = SendConn
    where
        Self: 'a;

    async fn connect<'a>(
        &'a self,
        _remote: SocketAddr,
    ) -> Result<Self::Connection<'a>, Self::Error> {
        Ok(SendConn {
            to_read: self.body.iter().copied().collect(),
            log: Arc::clone(&self.log),
        })
    }
}

/// The control: a stack holding an `Rc`, so nothing it produces is `Send`.
/// `embassy-net` is the real instance of this shape.
#[derive(Debug)]
pub struct LocalStack(pub std::rc::Rc<()>);
#[derive(Debug)]
pub struct LocalConn(
    #[allow(dead_code, reason = "held to make the type !Send")] pub std::rc::Rc<()>,
);

impl ErrorType for LocalConn {
    type Error = ErrorKind;
}
impl Read for LocalConn {
    async fn read(&mut self, _b: &mut [u8]) -> Result<usize, Self::Error> {
        Ok(0)
    }
}
impl Write for LocalConn {
    async fn write(&mut self, b: &[u8]) -> Result<usize, Self::Error> {
        Ok(b.len())
    }
    async fn flush(&mut self) -> Result<(), Self::Error> {
        Ok(())
    }
}
impl TcpConnect for LocalStack {
    type Error = ErrorKind;
    type Connection<'a>
        = LocalConn
    where
        Self: 'a;
    async fn connect<'a>(
        &'a self,
        _remote: SocketAddr,
    ) -> Result<Self::Connection<'a>, Self::Error> {
        Ok(LocalConn(self.0.clone()))
    }
}
