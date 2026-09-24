//! Adversarial test suite for `TlsStream`, modelled on the sibling suites
//! for the runtime I/O bridges (`crates/hclient-rt-tokio/tests/adversarial_tokio_io.rs`,
//! `crates/hclient-rt-smol/tests/adversarial_smol_io.rs`) — both built
//! because mutation testing revealed real gaps in those bridges, and
//! `TlsStream` sits in the identical architectural position (a
//! `futures-io` + `Shutdown` adapter wrapping another such adapter) without
//! ever getting the same treatment. Two defects — ciphertext loss under
//! transport backpressure, and a hang on `close_notify` without a raw TCP
//! close — both went unfound without this kind of coverage; this file
//! exists so the next one does not.
//!
//! `Scripted<S>` wraps a real transport (a real TCP loopback pair, driven
//! through a real rustls handshake against `server::spawn_tls_echo()`) and
//! lets a test inject exactly N artificial `Pending`s or a specific I/O
//! error on the NEXT `poll_write`/`poll_read`, deterministically, without
//! racing a real socket's actual timing. Every wait in this file is bounded
//! (`bounded()` below) — a regression here must report FAILED, not hang the
//! job with no diagnosis (the same discipline `adversarial_tokio_io.rs`
//! documents for the same reason).
use futures_io::{AsyncRead as SeamRead, AsyncWrite as SeamWrite};
use hclient_rt::Shutdown;
use hclient_rt::{TcpConnect, TcpOpts};
use hclient_rt_tokio::{Tokio, TokioIo};
use hclient_tls::{TlsConnect, TlsRequest};
use hclient_tls_rustls::{Rustls, TlsStream};
use std::future::poll_fn;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};
use std::time::Duration;

mod server;

const OP_TIMEOUT: Duration = Duration::from_secs(5);

/// Every blocking wait in this file goes through this, so a regression that
/// makes `TlsStream` hang reports a named FAILED test instead of eating the
/// whole CI job's time budget with nothing to investigate (same reasoning as
/// `adversarial_tokio_io.rs`'s `read_ready`, generalised to any future).
async fn bounded<F: std::future::Future>(fut: F) -> F::Output {
    tokio::time::timeout(OP_TIMEOUT, fut).await.unwrap_or_else(|_| {
        panic!(
            "operation did not resolve within {OP_TIMEOUT:?} - treating a stall as a regression \
             (FAILED), not letting it hang the job with no diagnosis"
        )
    })
}

#[derive(Default)]
struct ScriptState {
    pending_writes: u32,
    pending_reads: u32,
    write_error: Option<std::io::ErrorKind>,
}

/// Shared handle a test uses to arm a `Scripted<S>` transport from the
/// outside, after `TlsStream` has already taken ownership of it.
#[derive(Clone, Default)]
struct Script(Arc<Mutex<ScriptState>>);

impl Script {
    fn arm_pending_write(&self, n: u32) {
        self.0.lock().unwrap().pending_writes = n;
    }
    fn arm_pending_read(&self, n: u32) {
        self.0.lock().unwrap().pending_reads = n;
    }
    fn arm_write_error(&self, kind: std::io::ErrorKind) {
        self.0.lock().unwrap().write_error = Some(kind);
    }
}

/// Wraps a real transport; on each `poll_write`/`poll_read`, first checks
/// whether the test armed an artificial `Pending` or error for THIS call,
/// falling through to the real transport otherwise. The armed count is
/// consumed one-shot (decremented to 0), matching the reviewer's own
/// `PendingOnceThenSink` shape in `review-task-9-flush-outgoing-bug.rs`.
struct Scripted<S> {
    inner: S,
    script: Script,
}

impl<S> Scripted<S> {
    fn new(inner: S) -> (Self, Script) {
        let script = Script::default();
        (
            Self {
                inner,
                script: script.clone(),
            },
            script,
        )
    }
}

impl<S: SeamWrite + Unpin> SeamWrite for Scripted<S> {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        let this = self.get_mut();
        {
            let mut st = this.script.0.lock().unwrap();
            if let Some(kind) = st.write_error.take() {
                return Poll::Ready(Err(kind.into()));
            }
            if st.pending_writes > 0 {
                st.pending_writes -= 1;
                cx.waker().wake_by_ref();
                return Poll::Pending;
            }
        }
        Pin::new(&mut this.inner).poll_write(cx, buf)
    }
    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.get_mut().inner).poll_flush(cx)
    }
    fn poll_close(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.get_mut().inner).poll_close(cx)
    }
}

/// Forwarded rather than scripted: no test here arms a half-close, and a
/// double that answered one of its own would be asserting the fixture
/// instead of `TlsStream`.
impl<S: Shutdown + Unpin> Shutdown for Scripted<S> {
    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.get_mut().inner).poll_shutdown(cx)
    }
}

impl<S: SeamRead + Unpin> SeamRead for Scripted<S> {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut [u8],
    ) -> Poll<std::io::Result<usize>> {
        let this = self.get_mut();
        {
            let mut st = this.script.0.lock().unwrap();
            if st.pending_reads > 0 {
                st.pending_reads -= 1;
                cx.waker().wake_by_ref();
                return Poll::Pending;
            }
        }
        Pin::new(&mut this.inner).poll_read(cx, buf)
    }
}

async fn scripted_client(
    addr: SocketAddr,
    ca_der: Vec<u8>,
) -> (TlsStream<Scripted<TokioIo>>, Script) {
    let mut roots = rustls::RootCertStore::empty();
    roots.add(ca_der.into()).unwrap();
    let cfg = rustls::ClientConfig::builder()
        .with_root_certificates(roots)
        .with_no_client_auth();
    let tls = Rustls::from_config(Arc::new(cfg));
    let tcp = Tokio.connect(addr, &TcpOpts::default()).await.unwrap();
    let (scripted, script) = Scripted::new(tcp);
    let (stream, _info) = bounded(tls.connect(scripted, TlsRequest::new("localhost", &[])))
        .await
        .expect("handshake");
    (stream, script)
}

/// Drives `poll_write` to completion, retrying with the SAME remaining
/// slice on `Pending` — exactly the contract
/// `futures_io::AsyncWrite::poll_write` documents and the one a real
/// caller (hyper's own request writer, through `HyperIo`) relies
/// on. This is deliberately the naive, spec-compliant caller: it does not
/// know or care whether `TlsStream` can avoid returning `Pending` after
/// partially queueing data — it just does what the contract allows it to
/// do. If `TlsStream` ever queues the same bytes twice under this exact
/// usage, this is the caller shape that would trigger it.
async fn write_all<S: SeamWrite + Unpin>(stream: &mut S, mut data: &[u8]) {
    bounded(poll_fn(|cx| {
        while !data.is_empty() {
            match Pin::new(&mut *stream).poll_write(cx, data) {
                Poll::Ready(Ok(0)) => panic!("poll_write returned 0 for non-empty input"),
                Poll::Ready(Ok(n)) => data = &data[n..],
                Poll::Ready(Err(e)) => panic!("write failed: {e}"),
                Poll::Pending => return Poll::Pending,
            }
        }
        Poll::Ready(())
    }))
    .await;
}

async fn flush<S: SeamWrite + Unpin>(stream: &mut S) {
    bounded(poll_fn(|cx| Pin::new(&mut *stream).poll_flush(cx)))
        .await
        .unwrap();
}

async fn read_n<S: SeamRead + Unpin>(stream: &mut S, n: usize) -> Vec<u8> {
    let mut out = Vec::new();
    let mut store = [0u8; 64];
    bounded(poll_fn(|cx| {
        while out.len() < n {
            match Pin::new(&mut *stream).poll_read(cx, &mut store) {
                // A count of zero is `futures-io`'s end of stream, which
                // the cursor form spelled as a buffer left untouched.
                Poll::Ready(Ok(0)) => panic!("EOF before {n} bytes arrived, got {out:?}"),
                Poll::Ready(Ok(got)) => out.extend_from_slice(&store[..got]),
                Poll::Ready(Err(e)) => panic!("read failed: {e}"),
                Poll::Pending => return Poll::Pending,
            }
        }
        Poll::Ready(())
    }))
    .await;
    out
}

/// Unlike `bounded`, here `Pending` forever (nothing more arrives) is the
/// desired outcome, not a regression — a short timeout suffices because a
/// real loopback echo would deliver a duplicate almost immediately if there
/// were one; there is nothing to wait longer for.
async fn nothing_more_arrives<S: SeamRead + Unpin>(stream: &mut S) -> Vec<u8> {
    let mut store = [0u8; 64];
    match tokio::time::timeout(
        Duration::from_millis(300),
        poll_fn(|cx| Pin::new(&mut *stream).poll_read(cx, &mut store)),
    )
    .await
    {
        Ok(Ok(n)) => store[..n].to_vec(),
        Ok(Err(e)) => panic!("unexpected read error while checking for a duplicate: {e}"),
        Err(_elapsed) => Vec::new(),
    }
}

// ---------------------------------------------------------------------
// A. Pending on the very first transport write must not lose the write
//    nor duplicate it if the caller retries with the same buffer per the
//    `futures_io::AsyncWrite` contract — `poll_write` queueing `data` into
//    rustls BEFORE learning whether the transport can accept it makes a
//    `Pending`-then-retry cycle queue the same plaintext twice.
// ---------------------------------------------------------------------
#[tokio::test]
async fn pending_on_first_write_neither_loses_nor_duplicates_the_data() {
    let (addr, ca_der) = server::spawn_tls_echo();
    let (mut stream, script) = scripted_client(addr, ca_der).await;

    script.arm_pending_write(1);
    write_all(&mut stream, b"ping").await;
    flush(&mut stream).await;

    let echoed = read_n(&mut stream, 4).await;
    assert_eq!(
        echoed, b"ping",
        "the peer's echo must be exactly the four bytes sent once - not empty (lost) and not \
         a truncated/garbled prefix (sequence desync)"
    );
    let extra = nothing_more_arrives(&mut stream).await;
    assert!(
        extra.is_empty(),
        "peer echoed more than the single write back - data was duplicated: {extra:?}"
    );
}

// ---------------------------------------------------------------------
// B. Pending on the transport read, before any ciphertext has arrived,
//    must not be confused with EOF or with data (mirrors
//    adversarial_tokio_io.rs section A, one layer up the stack).
// ---------------------------------------------------------------------
#[tokio::test]
async fn pending_on_read_before_any_ciphertext_is_not_confused_with_eof_or_data() {
    let (addr, ca_der) = server::spawn_tls_echo();
    let (mut stream, script) = scripted_client(addr, ca_der).await;

    script.arm_pending_read(1);
    write_all(&mut stream, b"ping").await;
    flush(&mut stream).await;

    // The armed Pending fires on the read that's about to receive the
    // echo; the waker wakes immediately (see `Scripted::poll_read`), so
    // this must still resolve well within the bound, with the real data,
    // not an empty/EOF-shaped result.
    let echoed = read_n(&mut stream, 4).await;
    assert_eq!(echoed, b"ping");
}

// ---------------------------------------------------------------------
// C. A real transport error on write must be propagated, not swallowed
//    as WouldBlock/Pending and retried forever.
// ---------------------------------------------------------------------
#[tokio::test]
async fn transport_write_error_is_propagated_not_swallowed_as_pending() {
    let (addr, ca_der) = server::spawn_tls_echo();
    let (mut stream, script) = scripted_client(addr, ca_der).await;

    script.arm_write_error(std::io::ErrorKind::BrokenPipe);
    let result = bounded(poll_fn(|cx| Pin::new(&mut stream).poll_write(cx, b"ping"))).await;
    match result {
        Err(e) => assert_eq!(
            e.kind(),
            std::io::ErrorKind::BrokenPipe,
            "expected the injected error to surface with its real kind, got {e:?}"
        ),
        Ok(n) => panic!("expected the injected transport error to propagate, got Ok({n})"),
    }
}

// ---------------------------------------------------------------------
// D. An abrupt raw TCP close (RST, no close_notify) must still surface as
//    a real error, not silently as the clean EOF an honest
//    `close_notify` produces. Confirmed empirically
//    (not assumed) that a plain FIN without close_notify - as opposed to
//    the RST forced here - currently resolves identically to a clean
//    close_notify (`Ok(())`, nothing filled): `pump_incoming` short-
//    circuits on the raw transport's own `Ok(0)` before ever handing it
//    to `conn.read_tls`, so rustls's own truncation bookkeeping never
//    sees that read at all. That gap is real but is a separate,
//    judgment-call-shaped finding from 1 and 2 (reported alongside this
//    round, not fixed in it) - not tested here because a plain-FIN
//    variant of this test would be asserting the CURRENT gap as
//    contractually correct, which it is not.
// ---------------------------------------------------------------------
#[tokio::test]
async fn abrupt_rst_close_without_close_notify_is_reported_as_a_real_error() {
    // Server: real handshake, then drop the raw socket immediately -
    // no close_notify record ever sent, matching `SO_LINGER(0)` so the
    // kernel sends a TCP RST rather than a clean FIN (same technique as
    // `adversarial_tokio_io.rs` section C).
    let cert = rcgen::generate_simple_self_signed(vec!["localhost".into()]).unwrap();
    let cert_der = cert.cert.der().to_vec();
    let key_der = cert.signing_key.serialize_der();
    let ca_der = cert_der.clone();
    let server_cfg = rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(
            vec![cert_der.into()],
            rustls_pki_types::PrivateKeyDer::Pkcs8(key_der.into()),
        )
        .unwrap();
    let server_cfg = Arc::new(server_cfg);
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    std::thread::spawn(move || {
        let (mut sock, _) = listener.accept().unwrap();
        let mut conn = rustls::ServerConnection::new(server_cfg).unwrap();
        while conn.is_handshaking() {
            if conn.wants_write() {
                conn.write_tls(&mut sock).unwrap();
            }
            if conn.wants_read() {
                conn.read_tls(&mut sock).unwrap();
                conn.process_new_packets().unwrap();
            }
        }
        while conn.wants_write() {
            conn.write_tls(&mut sock).unwrap();
        }
        // No `send_close_notify()` - abrupt close, RST via SO_LINGER(0).
        socket2::Socket::from(sock)
            .set_linger(Some(Duration::ZERO))
            .unwrap();
        // dropping the socket2::Socket sends the RST.
    });

    let mut roots = rustls::RootCertStore::empty();
    roots.add(ca_der.into()).unwrap();
    let cfg = rustls::ClientConfig::builder()
        .with_root_certificates(roots)
        .with_no_client_auth();
    let tls = Rustls::from_config(Arc::new(cfg));
    let tcp = Tokio.connect(addr, &TcpOpts::default()).await.unwrap();
    let (mut stream, _info) = bounded(tls.connect(tcp, TlsRequest::new("localhost", &[])))
        .await
        .expect("handshake");

    let mut store = [0u8; 16];
    let result = bounded(poll_fn(|cx| {
        Pin::new(&mut stream).poll_read(cx, &mut store)
    }))
    .await;

    // Confirmed empirically: an RST is a real socket-layer error, caught
    // before `pump_incoming`'s own `Ok(0)`-as-EOF short-circuit ever gets a
    // chance to run - `poll_read`'s raw transport read itself fails with
    // `ConnectionReset`, which propagates straight through (`Err(e) =>
    // return Poll::Ready(Err(e))`, never swallowed as EOF). This is
    // observably different from a clean `close_notify` (`Ok(())`, nothing
    // filled, per `close_notify_without_tcp_close_hangs_the_read` in the
    // review's saved probe) - the two remain distinguishable after finding
    // 2's fix, for THIS abrupt-close mechanism.
    let e = result.expect_err("an RST-closed connection must not resolve as a clean read");
    assert_eq!(
        e.kind(),
        std::io::ErrorKind::ConnectionReset,
        "expected ConnectionReset from the RST, got {e:?}"
    );
}

/// **A server that pushes a large body without waiting**, read by a slow
/// caller — which is what a blob fetch is and what the echo server is
/// not: that one reads 1 KiB and answers it, so its bytes arrive in the
/// reader's own rhythm and nothing accumulates.
///
/// `rustls::read_tls` documents its refusal as *backpressure* rather than
/// failure: "errors of `ErrorKind::Other` are emitted to signal
/// backpressure … you should empty it through the `reader()`". The
/// plaintext buffer holds 64 KiB by default, and `pump_incoming` drives a
/// whole 16 KiB transport read through `read_tls`/`process_new_packets`
/// before it returns — so a caller taking 64 bytes at a time falls
/// behind, `is_full()` becomes true, and the signal surfaces as
/// `tls: received plaintext buffer full`.
///
/// Reported from the field against a real `act` pull: blobs over about a
/// megabyte fail reproducibly, and size is the trigger — the same pull
/// with `Accept-Encoding: identity` fails identically, so it is not a
/// decoder's buffering.
#[tokio::test]
async fn a_pushed_body_larger_than_the_plaintext_buffer_survives_a_slow_reader() {
    const N: usize = 1024 * 1024;
    let (addr, ca) = server::spawn_tls_pusher(N);
    let (mut stream, _script) = scripted_client(addr, ca).await;

    let got = read_n(&mut stream, N).await;
    assert_eq!(got.len(), N, "the whole body must come back");
    assert!(
        got.iter()
            .enumerate()
            .all(|(i, b)| *b == u8::try_from(i % 251).unwrap_or(0)),
        "and byte for byte"
    );
}
