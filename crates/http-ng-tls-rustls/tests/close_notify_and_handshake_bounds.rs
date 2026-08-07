//! Two properties from the fix round 1 review (Verdict B #2 and #4),
//! adapted from the reviewer's saved probes
//! (`review-task-9-close-notify-and-hangs.rs`) after independently
//! re-verifying both in this tree rather than inheriting their text as-is.
//!
//! Finding 2 (now fixed, `src/stream.rs`): a clean `close_notify` without a
//! matching raw TCP close used to hang `poll_read` forever, because `Ok(0)`
//! from `conn.reader().read()` (rustls's own signal for "clean close,
//! nothing more ever") was treated the same as `WouldBlock` ("nothing yet,
//! keep waiting on the transport"). This file's first test now asserts the
//! CORRECT behaviour (prompt clean EOF), not the bug - re-confirmed here by
//! reverting `stream.rs`'s `Ok(0) => return Poll::Ready(Ok(()))` back to
//! `Ok(0) => {}` via `cp` and watching this exact test go red with the
//! bound's own timeout message before restoring (see fix round 1 report).
//!
//! Finding 4: `Rustls::connect` has no internal bound against a peer that
//! accepts the TCP connection and then goes silent - confirmed still true
//! after this round's fixes, and documented here as intentional: `TlsRequest`
//! (Task 8's contract) carries no deadline, so `Rustls::connect` has nothing
//! to bound itself against. This mirrors Task 3's own resolution for its
//! four unbounded sites - the bound belongs at the call site / a higher
//! layer, not invented locally; what IS required, and is followed here, is
//! that every WAIT IN A TEST stays bounded so a real regression fails loudly
//! instead of hanging the job.
use http_ng_rt::TcpConnect;
use http_ng_rt_tokio::Tokio;
use http_ng_tls::{TlsConnect, TlsRequest};
use http_ng_tls_rustls::Rustls;
use std::io::Write;
use std::sync::Arc;
use std::time::Duration;

#[tokio::test]
async fn close_notify_without_a_raw_tcp_close_resolves_as_clean_eof_not_a_hang() {
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
        // Fully synchronous, blocking server for exact control over which
        // bytes hit the wire and when - specifically, over sending
        // close_notify WITHOUT ever shutting down or dropping the raw
        // socket (tokio_rustls's own TlsAcceptor closes the socket on
        // shutdown, which would mask the isolation this test needs).
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
        while conn.wants_write() {
            conn.write_tls(&mut sock).unwrap();
        }

        // Send close_notify - and ONLY close_notify. No socket shutdown, no
        // drop: hold the raw TCP connection open indefinitely afterward so
        // the only signal the client gets is the TLS-level alert.
        conn.send_close_notify();
        while conn.wants_write() {
            conn.write_tls(&mut sock).unwrap();
        }
        sock.flush().ok();

        std::thread::sleep(Duration::from_secs(30)); // outlive the test's own bound
        drop(sock);
    });

    let mut roots = rustls::RootCertStore::empty();
    roots.add(ca_der.into()).unwrap();
    let cfg = rustls::ClientConfig::builder()
        .with_root_certificates(roots)
        .with_no_client_auth();
    let tls = Rustls::from_config(Arc::new(cfg));
    let tcp = Tokio
        .connect(addr, &http_ng_rt::TcpOpts::default())
        .await
        .unwrap();
    let (mut stream, _info) = tls
        .connect(
            tcp,
            TlsRequest {
                server_name: "localhost",
                alpn: &[],
                ech: None,
            },
        )
        .await
        .expect("handshake");

    let mut store = [0u8; 16];
    let result = tokio::time::timeout(Duration::from_secs(3), async {
        let mut rb = hyper::rt::ReadBuf::new(&mut store);
        std::future::poll_fn(|cx| {
            hyper::rt::Read::poll_read(std::pin::Pin::new(&mut stream), cx, rb.unfilled())
        })
        .await
        .map(|()| rb.filled().len())
    })
    .await;

    match result {
        Err(_elapsed) => panic!(
            "poll_read did not resolve within 3s after close_notify even though the peer already \
             signalled clean TLS-level closure - it kept waiting on the still-open raw transport \
             instead (this is the fix round 1 bug, finding 2, if it reproduces)"
        ),
        Ok(Err(e)) => panic!("poll_read resolved with an unexpected error: {e}"),
        Ok(Ok(n)) => assert_eq!(
            n, 0,
            "close_notify alone must resolve as a clean EOF (nothing filled), not as data"
        ),
    }
}

#[tokio::test]
async fn handshake_against_a_silent_peer_has_no_internal_bound_by_design() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    listener.set_nonblocking(true).unwrap();
    std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async move {
            let listener = tokio::net::TcpListener::from_std(listener).unwrap();
            let (_tcp, _) = listener.accept().await.unwrap();
            std::future::pending::<()>().await; // accept, then never speak again
        });
    });

    let roots = rustls::RootCertStore::empty();
    let cfg = rustls::ClientConfig::builder()
        .with_root_certificates(roots)
        .with_no_client_auth();
    let tls = Rustls::from_config(Arc::new(cfg));
    let tcp = Tokio
        .connect(addr, &http_ng_rt::TcpOpts::default())
        .await
        .unwrap();

    let result = tokio::time::timeout(
        Duration::from_secs(2),
        tls.connect(
            tcp,
            TlsRequest {
                server_name: "localhost",
                alpn: &[],
                ech: None,
            },
        ),
    )
    .await;

    // Documents current, intentional behaviour, not a defect: `TlsRequest`
    // (Task 8) carries no deadline, so `Rustls::connect` has nothing of its
    // own to bound against - the outer timeout here is standing in for
    // whatever higher layer is expected to supply one, same as Task 3's
    // four sites bound at their OWN call sites rather than inside the
    // runtime capability itself.
    assert!(
        result.is_err(),
        "expected the outer 2s timeout to fire because Rustls::connect has no internal bound; \
         if this assertion fails, something now bounds the handshake internally and this test \
         (and its doc comment) needs updating to match"
    );
}
