//! The layering decision behind `pump_incoming`/`poll_read` in
//! `src/stream.rs` — an unclean TCP close
//! (no `close_notify`) is now surfaced to the caller as a real
//! `UnexpectedEof`, but WHETHER that should fail an in-flight HTTP request
//! is not this crate's call. It belongs to whatever layer understands HTTP
//! framing (`Content-Length`/chunked) - here, real `hyper`, driven directly
//! over `TlsStream` via `hyper::client::conn::http1`, not a hand-rolled
//! substitute: the whole point is to check what hyper ACTUALLY does, not
//! what a test author assumes it does.
//!
//! Four tests, three requested plus one added after mutation testing showed
//! the requested pair alone doesn't isolate this fix (see below):
//! - `close_notify_and_a_bare_fin_are_observably_different_at_the_stream_level`
//!   - the direct, stream-level check: same "no close_notify, socket held
//!     open" vs. "bare FIN, no close_notify" pair, asserting that the FIN
//!     case produces `UnexpectedEof` rather than merely not hanging.
//! - `complete_response_survives_an_unclean_close_after_it` - a full,
//!   `Content-Length`-satisfied response followed by a bare FIN (no
//!   `close_notify`) must still succeed at the HTTP layer.
//! - `truncated_response_is_reported_as_an_error_not_a_short_success` - a
//!   `Content-Length` promise that the connection closes before fulfilling
//!   must fail, not silently hand back a short body that looks complete.
//!   Passes even against the OLD short-circuit code (see the report):
//!   hyper's own byte counting already catches a short `Content-Length`
//!   body regardless of the stream-level EOF-vs-error signal, so this test
//!   alone can't tell the fix apart from its absence.
//! - `close_delimited_truncation_is_the_case_this_fix_is_actually_for` - no
//!   `Content-Length`, so hyper falls back to "connection closed = body
//!   complete." This is the one that actually flips red on the old code -
//!   there's no byte count to fall back on, so the stream-level signal is
//!   the ONLY thing standing between a truncated body and a silent,
//!   "successful" short read.
use hclient_rt::TcpConnect;
use hclient_rt_tokio::Tokio;
use hclient_tls::{TlsConnect, TlsRequest};
use hclient_tls_rustls::Rustls;
use hyper::body::Bytes;
use std::future::poll_fn;
use std::io::Read as _;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

const OP_TIMEOUT: Duration = Duration::from_secs(10);

async fn bounded<F: std::future::Future>(fut: F) -> F::Output {
    tokio::time::timeout(OP_TIMEOUT, fut).await.unwrap_or_else(|_| {
        panic!(
            "operation did not resolve within {OP_TIMEOUT:?} - treating a stall as a regression \
             (FAILED), not letting it hang the job with no diagnosis"
        )
    })
}

/// Real, hand-driven `rustls::ServerConnection` (not `tokio_rustls`, whose
/// `TlsAcceptor`/shutdown path closes the raw socket cleanly and would mask
/// the abrupt-close case this file exists to test). Drains the incoming
/// request first - reading the socket's receive buffer down to nothing
/// before the server closes it matters here: on Linux, closing a socket
/// with UNREAD data in its receive buffer sends an RST instead of a clean
/// FIN, which would silently turn this into the round 1 RST test instead
/// of the plain-FIN case round 2 is actually about.
fn spawn_response_server(response_head: String, body: &'static [u8]) -> (SocketAddr, Vec<u8>) {
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
        sock.set_nonblocking(false).unwrap();
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

        // Drain the request up to the end of headers - a bodyless GET is
        // exactly `...\r\n\r\n` terminated, nothing more to read after that.
        let mut request = Vec::new();
        loop {
            if conn.wants_read() {
                conn.read_tls(&mut sock).unwrap();
                conn.process_new_packets().unwrap();
            }
            let mut chunk = [0u8; 1024];
            match conn.reader().read(&mut chunk) {
                Ok(0) => break,
                Ok(n) => {
                    request.extend_from_slice(&chunk[..n]);
                    if request.windows(4).any(|w| w == b"\r\n\r\n") {
                        break;
                    }
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => continue,
                Err(_) => break,
            }
        }

        use std::io::Write as _;
        conn.writer().write_all(response_head.as_bytes()).unwrap();
        conn.writer().write_all(body).unwrap();
        while conn.wants_write() {
            conn.write_tls(&mut sock).unwrap();
        }
        // No `send_close_notify()`, no `SO_LINGER(0)` - plain FIN via a
        // straightforward drop, request already fully drained above so the
        // kernel has nothing left to RST over.
        drop(sock);
    });

    (addr, ca_der)
}

async fn fetch_body(addr: SocketAddr, ca_der: Vec<u8>) -> Result<Vec<u8>, String> {
    let mut roots = rustls::RootCertStore::empty();
    roots.add(ca_der.into()).unwrap();
    let cfg = rustls::ClientConfig::builder()
        .with_root_certificates(roots)
        .with_no_client_auth();
    let tls = Rustls::from_config(Arc::new(cfg));
    let tcp = Tokio
        .connect(addr, &hclient_rt::TcpOpts::default())
        .await
        .unwrap();
    let (stream, _info) = bounded(tls.connect(
        tcp,
        TlsRequest {
            server_name: "localhost",
            alpn: &[],
            ech: None,
            early_data: None,
        },
    ))
    .await
    .expect("handshake");

    let (mut sender, conn) = bounded(hyper::client::conn::http1::handshake(stream))
        .await
        .expect("http1 handshake");
    // The connection driver errors out once the abrupt close reaches it
    // (that IS the fixed signal, `UnexpectedEof`) - that's expected and
    // deliberately not asserted on here; what matters is whether the
    // ALREADY-DISPATCHED response/body observed by `fetch_body`'s caller
    // was affected by it, which is what each test below actually checks.
    tokio::spawn(async move {
        let _ = conn.await;
    });

    let req = hyper::Request::builder()
        .uri("/")
        .header("Host", "localhost")
        .body(http_body_util::Empty::<Bytes>::new())
        .unwrap();

    let resp = bounded(sender.send_request(req))
        .await
        .map_err(|e| format!("send_request failed: {e}"))?;
    let collected = bounded(http_body_util::BodyExt::collect(resp.into_body()))
        .await
        .map_err(|e| format!("body collection failed: {e}"))?;
    Ok(collected.to_bytes().to_vec())
}

#[tokio::test]
async fn complete_response_survives_an_unclean_close_after_it() {
    let head = "HTTP/1.1 200 OK\r\nContent-Length: 5\r\nConnection: close\r\n\r\n".to_string();
    let (addr, ca_der) = spawn_response_server(head, b"hello");

    let body = fetch_body(addr, ca_der)
        .await
        .expect("a Content-Length-satisfied response must succeed even though the TLS stream errors right after it - the framing was already complete");
    assert_eq!(body, b"hello");
}

#[tokio::test]
async fn close_delimited_truncation_is_the_case_this_fix_is_actually_for() {
    // No `Content-Length`, no chunked encoding: hyper falls back to
    // close-delimited framing, where "the connection closed" IS the
    // end-of-body signal - there is no byte count to catch a truncation
    // independently. This is the one demonstrated empirically to actually
    // flip under mutation (see the report): a `Content-Length` truncation
    // is already caught by hyper's own byte counting regardless of the
    // stream-level EOF-vs-error signal, but a close-delimited body has
    // nothing else to catch it with. Confirmed against the code this fix
    // reverts to: the peer sent "hel" (3 bytes) of an intended "hello" (5),
    // closed uncleanly, and the old short-circuit resolved that as a
    // complete, successful 3-byte response - `Ok(b"hel")`, not an error.
    let head = "HTTP/1.1 200 OK\r\nConnection: close\r\n\r\n".to_string();
    let (addr, ca_der) = spawn_response_server(head, b"hel");
    let result = fetch_body(addr, ca_der).await;
    assert!(
        result.is_err(),
        "a close-delimited body cut short must fail, not silently succeed as a complete \
         3-byte response - got Ok({:?})",
        result.ok()
    );
}

#[tokio::test]
async fn truncated_response_is_reported_as_an_error_not_a_short_success() {
    // Promises 5 bytes, sends 3, then closes uncleanly - hyper must not
    // hand back "hel" as if it were a complete, successful response.
    let head = "HTTP/1.1 200 OK\r\nContent-Length: 5\r\nConnection: close\r\n\r\n".to_string();
    let (addr, ca_der) = spawn_response_server(head, b"hel");

    let result = fetch_body(addr, ca_der).await;
    assert!(
        result.is_err(),
        "a response cut short mid-body must fail, not silently succeed with a truncated body - \
         got Ok({:?})",
        result.ok()
    );
}

#[tokio::test]
async fn close_notify_and_a_bare_fin_are_observably_different_at_the_stream_level() {
    // The direct stream-level pair, both driven through the same
    // `TlsStream::poll_read`, no HTTP layer involved - isolates exactly
    // what `pump_incoming`'s fix changed, independent of hyper.
    use hclient_tls_rustls::TlsStream;
    use hyper::rt::ReadBuf;
    use std::pin::Pin;

    async fn read_after_close(send_close_notify: bool) -> std::io::Result<usize> {
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
            if send_close_notify {
                conn.send_close_notify();
                while conn.wants_write() {
                    conn.write_tls(&mut sock).unwrap();
                }
            }
            // Either way: plain FIN via drop, no SO_LINGER, no unread
            // request bytes to trigger an RST.
            drop(sock);
        });

        let mut roots = rustls::RootCertStore::empty();
        roots.add(ca_der.into()).unwrap();
        let cfg = rustls::ClientConfig::builder()
            .with_root_certificates(roots)
            .with_no_client_auth();
        let tls = Rustls::from_config(Arc::new(cfg));
        let tcp = Tokio
            .connect(addr, &hclient_rt::TcpOpts::default())
            .await
            .unwrap();
        let (mut stream, _info): (TlsStream<_>, _) = bounded(tls.connect(
            tcp,
            TlsRequest {
                server_name: "localhost",
                alpn: &[],
                ech: None,
                early_data: None,
            },
        ))
        .await
        .expect("handshake");

        let mut store = [0u8; 16];
        let mut rb = ReadBuf::new(&mut store);
        bounded(poll_fn(|cx| {
            hyper::rt::Read::poll_read(Pin::new(&mut stream), cx, rb.unfilled())
        }))
        .await
        .map(|()| rb.filled().len())
    }

    let clean = read_after_close(true).await;
    assert_eq!(
        clean.ok(),
        Some(0),
        "a real close_notify must still resolve as a clean EOF (Ok(0))"
    );

    let unclean = read_after_close(false).await;
    let err = unclean.expect_err(
        "a bare FIN with no close_notify must now be reported as an error, not resolve \
         identically to a clean close_notify - that identical-resolution was the truncation gap",
    );
    assert_eq!(
        err.kind(),
        std::io::ErrorKind::UnexpectedEof,
        "expected rustls's own unclean-closure signal, got {err:?}"
    );
}
