//! Spike 3b: **can `Native`'s deliberately-`!Send` IO actually be handed
//! to the local spawner as a connection driver?**
//!
//! `cargo run --bin native_driver`
//!
//! This is the half of question 1 that the reaper does not answer. A
//! reaper touches the pool; a *driver* takes the connection itself — the
//! thing `h1.rs` currently polls from `H1Body::poll_frame` — and moves it
//! into a background task. If `Native`'s IO cannot go there, the whole
//! discussion is academic.
//!
//! The IO here is a real `tokio::net::TcpStream` (through
//! `http_ng_rt_tokio::Tokio::connect`, the same call `Native` makes)
//! wrapped in a type that holds an `Rc<()>` — `connect.rs`'s `FakeStream`
//! trick, but over a socket that really carries bytes.
//!
//! Two runs:
//!
//! - A: the driver spawned on `TokioLocal`. The request completes and the
//!   body arrives.
//! - B: the control — nothing spawned, nothing else polling the
//!   connection. The request must hang.

use bytes::Bytes;
use http_body_util::{BodyExt, Empty};
use http_ng_rt::{Spawn, TcpConnect, TcpOpts};
use spawn_local_spike::TokioLocal;
use spawn_local_spike::reaper::Discard;
use std::io::{Read as _, Write as _};
use std::net::TcpListener;
use std::pin::Pin;
use std::rc::Rc;
use std::task::{Context, Poll};
use std::time::Duration;

/// Real IO, made `!Send` on purpose.
struct RcIo<I> {
    inner: I,
    _proof: Rc<()>,
}

impl<I: hyper::rt::Read + Unpin> hyper::rt::Read for RcIo<I> {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: hyper::rt::ReadBufCursor<'_>,
    ) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.inner).poll_read(cx, buf)
    }
}

impl<I: hyper::rt::Write + Unpin> hyper::rt::Write for RcIo<I> {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        Pin::new(&mut self.inner).poll_write(cx, buf)
    }
    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.inner).poll_flush(cx)
    }
    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.inner).poll_shutdown(cx)
    }
}

fn assert_not_send<T>(_: &T) {}

/// A one-shot HTTP/1.1 server on a thread. No hyper on this side.
fn server() -> std::net::SocketAddr {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = l.local_addr().unwrap();
    std::thread::spawn(move || {
        for s in l.incoming() {
            let mut s = s.unwrap();
            std::thread::spawn(move || {
                let mut buf = [0u8; 1024];
                let _ = s.read(&mut buf);
                let _ = s.write_all(
                    b"HTTP/1.1 200 OK\r\nContent-Length: 24\r\n\r\ndriven by a spawned task",
                );
                let _ = s.flush();
                std::thread::sleep(Duration::from_millis(300));
            });
        }
    });
    addr
}

async fn connect(addr: std::net::SocketAddr) -> RcIo<<http_ng_rt_tokio::Tokio as TcpConnect>::Stream>
{
    let stream = http_ng_rt_tokio::Tokio
        .connect(addr, &TcpOpts::default())
        .await
        .unwrap();
    RcIo {
        inner: stream,
        _proof: Rc::new(()),
    }
}

fn main() {
    let addr = server();
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();

    // --- A: the driver goes to TokioLocal ----------------------------------
    rt.block_on(async {
        let local = TokioLocal::new();
        local
            .run_until(async {
                let io = connect(addr).await;
                assert_not_send(&io);
                let (mut send, conn) = hyper::client::conn::http1::handshake::<_, Empty<Bytes>>(io)
                    .await
                    .unwrap();

                // `Connection<I, B>` is a NAMED struct, which is what makes
                // `Spawn<F>`'s shape usable here at all — unlike the reaper,
                // no `NamedTimer` is needed. `Discard` only adapts
                // `Output = Result<(), hyper::Error>` to `Output = ()`.
                let driver: Discard<
                    hyper::client::conn::http1::Connection<
                        RcIo<<http_ng_rt_tokio::Tokio as TcpConnect>::Stream>,
                        Empty<Bytes>,
                    >,
                > = Discard(conn);
                assert_not_send(&driver);
                println!(
                    "A. spawning Discard<hyper::client::conn::http1::Connection<RcIo<TokioIo>, Empty<Bytes>>> (!Send) on TokioLocal"
                );
                Spawn::spawn(&local, driver);

                let req = http::Request::builder()
                    .uri("/")
                    .header("host", "localhost")
                    .body(Empty::<Bytes>::new())
                    .unwrap();
                let res = send.send_request(req).await.unwrap();
                println!("   status = {}", res.status());
                let body = res.into_body().collect().await.unwrap().to_bytes();
                println!("   body   = {:?}", String::from_utf8_lossy(&body));
                assert_eq!(&body[..], b"driven by a spawned task");
            })
            .await;
    });

    // --- B: the control — nobody drives the connection ---------------------
    rt.block_on(async {
        let local = TokioLocal::new();
        local
            .run_until(async {
                let io = connect(addr).await;
                let (mut send, _conn) = hyper::client::conn::http1::handshake::<_, Empty<Bytes>>(io)
                    .await
                    .unwrap();
                let req = http::Request::builder()
                    .uri("/")
                    .header("host", "localhost")
                    .body(Empty::<Bytes>::new())
                    .unwrap();
                println!("B. control: the same request with NOTHING driving the connection");
                match tokio::time::timeout(Duration::from_millis(500), send.send_request(req)).await
                {
                    Ok(r) => println!("   UNEXPECTED: got {:?}", r.map(|r| r.status())),
                    Err(_) => println!(
                        "   timed out after 500ms, as it must — the response only arrives because run A spawned the driver"
                    ),
                }
            })
            .await;
    });
}
