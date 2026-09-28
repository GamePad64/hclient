//! `CapsulePath` received in one task and sent in another, which is how a
//! QUIC stack uses a socket — over an in-memory stream small enough that
//! nearly every capsule is written in pieces and nearly every send waits.
#![cfg(not(target_family = "wasm"))]

use std::io;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll, ready};
use std::time::Duration;

use hclient_masque::CapsulePath;
use hclient_proxy::{BoxIo, DatagramPath};

/// A `tokio` in-memory stream in the seam's shape. Each direction of a
/// `DuplexStream` keeps its own waker, so it is itself safe to read in one
/// task and write in another.
struct Duplex(tokio::io::DuplexStream);

impl futures_io::AsyncRead for Duplex {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut [u8],
    ) -> Poll<io::Result<usize>> {
        let mut rb = tokio::io::ReadBuf::new(buf);
        ready!(tokio::io::AsyncRead::poll_read(
            Pin::new(&mut self.0),
            cx,
            &mut rb
        ))?;
        Poll::Ready(Ok(rb.filled().len()))
    }
}

impl futures_io::AsyncWrite for Duplex {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        tokio::io::AsyncWrite::poll_write(Pin::new(&mut self.0), cx, buf)
    }
    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        tokio::io::AsyncWrite::poll_flush(Pin::new(&mut self.0), cx)
    }
    fn poll_close(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        tokio::io::AsyncWrite::poll_shutdown(Pin::new(&mut self.0), cx)
    }
}

impl hclient_rt::Shutdown for Duplex {
    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        tokio::io::AsyncWrite::poll_shutdown(Pin::new(&mut self.0), cx)
    }
}

fn payload(i: usize) -> Vec<u8> {
    let len = 1 + (i * 37) % 1000;
    (0..len)
        .map(|j| u8::try_from((i + j) % 251).unwrap())
        .collect()
}

const COUNT: usize = 300;

/// Every datagram arrives, whole and in order, when the sender and the
/// receiver are different tasks and the stream between them holds 64
/// bytes. A wait that did not register its own task's waker — or a send
/// that replaced the waiting writer's — leaves one side parked for ever,
/// which the bound turns into a failure.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_capsule_path_carries_datagrams_between_two_tasks() {
    let (near, far) = tokio::io::duplex(64);
    // The far end echoes every byte, so capsules come back as they went.
    tokio::spawn(async move {
        let (mut r, mut w) = tokio::io::split(far);
        let _ = tokio::io::copy(&mut r, &mut w).await;
    });
    let path = Arc::new(CapsulePath::new(BoxIo::new(Duplex(near))));

    let sender = {
        let path = Arc::clone(&path);
        tokio::spawn(async move {
            let mut waited = 0usize;
            for i in 0..COUNT {
                let d = payload(i);
                loop {
                    std::future::poll_fn(|cx| {
                        let p = path.poll_writable(cx);
                        if p.is_pending() {
                            waited += 1;
                        }
                        p
                    })
                    .await
                    .unwrap();
                    match path.try_send(&d) {
                        Ok(()) => break,
                        Err(e) if e.kind() == io::ErrorKind::WouldBlock => {}
                        Err(e) => panic!("{e}"),
                    }
                }
            }
            waited
        })
    };
    let receiver = {
        let path = Arc::clone(&path);
        tokio::spawn(async move {
            let mut buf = vec![0u8; 2048];
            for i in 0..COUNT {
                let n = std::future::poll_fn(|cx| path.poll_recv(cx, &mut buf))
                    .await
                    .unwrap();
                assert_eq!(buf[..n], payload(i)[..], "datagram {i}");
            }
        })
    };
    let both = async {
        let waited = sender.await.unwrap();
        receiver.await.unwrap();
        waited
    };
    let waited = tokio::time::timeout(Duration::from_secs(20), both)
        .await
        .expect("neither task was left parked");
    // Not an assertion about timing: with 64 bytes of room and capsules up
    // to a kilobyte, a sender that never had to wait would mean the stream
    // was not the one under test.
    assert!(waited > 0);
}

#[tokio::test]
async fn a_capsule_path_refuses_what_it_cannot_carry_and_ends_with_its_stream() {
    let (near, far) = tokio::io::duplex(4096);
    let path = CapsulePath::new(BoxIo::new(Duplex(near)));
    assert_eq!(path.max_datagram_size(), hclient_masque::CAPSULE_MAX);
    let too_big = vec![0u8; hclient_masque::CAPSULE_MAX + 1];
    assert_eq!(
        path.try_send(&too_big).unwrap_err().kind(),
        io::ErrorKind::InvalidInput
    );
    // The control: exactly the size it declares is accepted.
    path.try_send(&vec![0u8; hclient_masque::CAPSULE_MAX])
        .unwrap();
    drop(far);
    let mut buf = [0u8; 16];
    let e = std::future::poll_fn(|cx| path.poll_recv(cx, &mut buf))
        .await
        .unwrap_err();
    assert_eq!(e.kind(), io::ErrorKind::ConnectionAborted);
}

/// A proxy does not echo, so nothing inbound wakes the receiver. The
/// sender writes a burst and stops — as a QUIC stack does after the last
/// packet of a flight — while the receiver is parked waiting for a reply
/// that never comes. The last capsule, only partly taken by a 64-byte
/// stream, must still reach the far end whole: the receiver owns the kept
/// tail's write interest, and nothing else is going to push it on.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_last_capsule_of_a_burst_reaches_a_proxy_that_only_reads() {
    use tokio::io::AsyncReadExt as _;
    const BURST: usize = 5;
    let (near, mut far) = tokio::io::duplex(64);
    let path = Arc::new(CapsulePath::new(BoxIo::new(Duplex(near))));

    // Parked first, while there is no tail at all.
    let receiver = {
        let path = Arc::clone(&path);
        tokio::spawn(async move {
            let mut buf = vec![0u8; 2048];
            let _ = std::future::poll_fn(|cx| path.poll_recv(cx, &mut buf)).await;
        })
    };
    tokio::time::sleep(Duration::from_millis(50)).await;

    // Counts complete capsules, and reads — nothing is ever written back.
    let counted = tokio::spawn(async move {
        let mut got = Vec::new();
        let mut buf = [0u8; 256];
        let mut complete = 0usize;
        while complete < BURST {
            let n = far.read(&mut buf).await.unwrap();
            assert_ne!(n, 0, "the near end closed");
            got.extend_from_slice(&buf[..n]);
            // Each capsule: type 0x00, a two-byte length varint, context 0,
            // then the payload.
            while got.len() >= 3 {
                let len = usize::from(u16::from_be_bytes([got[1] & 0x3f, got[2]]));
                if got.len() < 3 + len {
                    break;
                }
                assert_eq!(got[3], 0, "context id 0");
                got.drain(..3 + len);
                complete += 1;
            }
        }
        complete
    });

    for i in 0..BURST {
        let d = vec![u8::try_from(i).unwrap(); 500];
        loop {
            std::future::poll_fn(|cx| path.poll_writable(cx))
                .await
                .unwrap();
            match path.try_send(&d) {
                Ok(()) => break,
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => {}
                Err(e) => panic!("{e}"),
            }
        }
    }
    // The sender is done: nothing calls `poll_writable` or `try_send` again.

    let complete = tokio::time::timeout(Duration::from_secs(5), counted)
        .await
        .expect("the last capsule was stranded in the path")
        .unwrap();
    assert_eq!(complete, BURST);
    receiver.abort();
}
