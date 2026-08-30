//! `Native::h2_opts`, read off the wire by the peer that has to obey it.
//!
//! The claim is about a **`SETTINGS` frame**, so nothing here is timed. A
//! throughput measurement on loopback would say almost nothing anyway —
//! the window bounds bytes *in flight*, and with a round trip near zero a
//! sender refills it as fast as it drains, which is exactly why the
//! default is only a ceiling on a long fat pipe. What is observable
//! without a network is the number itself, and the honest place to observe
//! it is where it takes effect: an `h2::server` reserving capacity on a
//! stream is told, by its own flow control, how much the client said it
//! would accept.
//!
//! `FakeTls` and the plaintext-h2-over-a-fake-ALPN arrangement are
//! `tests/grpc_shape.rs`', for its reason: the bytes on the wire really
//! are HTTP/2, so the server is decoding what the client wrote rather than
//! agreeing with it through a mock.
#![cfg(all(feature = "http2", not(target_family = "wasm")))]

use hclient_core::RequestBody;
use hclient_core::unversioned::Transport;
use hclient_dns_system::SystemDns;
use hclient_native::{H2Opts, Native};
use hclient_rt_tokio::Tokio;
use hclient_tls::{TlsConfigId, TlsConnect, TlsIdentity, TlsInfo, TlsRequest};
use std::sync::{Arc, Mutex};
use std::time::Duration;

const BOUND: Duration = Duration::from_secs(30);

/// RFC 9113 §6.9.2's default, and what this test exists to move.
const RFC_DEFAULT_WINDOW: usize = 65_535;

/// RFC 9113 §6.5.2's default `SETTINGS_MAX_FRAME_SIZE`.
const RFC_DEFAULT_FRAME: usize = 16_384;

/// What the server answers with — big enough to be cut into several DATA
/// frames at either frame size under test.
const BODY: usize = 512 * 1024;

/// The window the window test asks for.
///
/// **Deliberately below h2's own 400 KiB send buffer**
/// (`proto::DEFAULT_MAX_SEND_BUFFER_SIZE`, read rather than guessed),
/// because `SendStream::capacity` is bounded by *both* the peer's window
/// and the sender's buffer — asking for a megabyte is answered with
/// 409 600, which discriminates but does not measure. Under the
/// buffer the window is the only thing binding, so the number the server
/// is granted is the number the client advertised, exactly.
const RAISED_WINDOW: usize = 256 * 1024;

#[derive(Clone)]
struct FakeTls {
    id: TlsConfigId,
}

impl TlsIdentity for FakeTls {
    fn config_id(&self) -> TlsConfigId {
        self.id
    }
}

impl TlsConnect for FakeTls {
    type Stream<S>
        = S
    where
        S: hyper::rt::Read + hyper::rt::Write + Unpin;

    fn reports_alpn(&self) -> bool {
        true
    }

    type Handshake<'a, S>
        = std::future::Ready<Result<(S, TlsInfo), hclient_core::Error>>
    where
        Self: 'a,
        S: hyper::rt::Read + hyper::rt::Write + Unpin + 'a;

    fn connect<'a, S>(&'a self, io: S, _req: TlsRequest<'a>) -> Self::Handshake<'a, S>
    where
        S: hyper::rt::Read + hyper::rt::Write + Unpin + 'a,
    {
        std::future::ready(Ok((io, TlsInfo::default().alpn(Some(b"h2".to_vec())))))
    }
}

fn transport(opts: H2Opts) -> Native<Tokio, FakeTls, SystemDns<Tokio>> {
    Native::new(
        Tokio,
        FakeTls {
            id: TlsConfigId::new_unique(),
        },
        SystemDns::new(Tokio),
    )
    .h2_opts(opts)
}

/// What the server learned about the client's settings on one connection.
#[derive(Debug, Default, Clone, Copy)]
struct Observed {
    /// The most capacity h2 would grant the server's `SendStream` — i.e.
    /// the client's `SETTINGS_INITIAL_WINDOW_SIZE`, since nothing has been
    /// sent yet and no `WINDOW_UPDATE` has arrived.
    stream_capacity: usize,
}

/// A one-connection `h2::server` that records what the client advertised
/// and answers with an empty `200`.
fn spawn_server() -> (std::net::SocketAddr, Arc<Mutex<Vec<Observed>>>) {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = listener.local_addr().expect("addr");
    listener.set_nonblocking(true).expect("nonblocking");
    let seen: Arc<Mutex<Vec<Observed>>> = Arc::new(Mutex::new(Vec::new()));
    let rec = Arc::clone(&seen);

    std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");
        rt.block_on(async move {
            let listener = tokio::net::TcpListener::from_std(listener).expect("from_std");
            loop {
                let Ok((tcp, _)) = listener.accept().await else {
                    continue;
                };
                let rec = Arc::clone(&rec);
                tokio::spawn(async move {
                    let Ok(mut conn) = h2::server::handshake(tcp).await else {
                        return;
                    };
                    while let Some(Ok((req, mut respond))) = conn.accept().await {
                        let mut b = http::Response::builder().status(200);
                        if req.uri().path() == "/big" {
                            // Well past any small `max_header_list_size`
                            // and well under h2's 16 MiB default, so the
                            // two arms differ by the setting alone.
                            b = b.header("x-large", "v".repeat(8 * 1024));
                        }
                        let Ok(mut send) = respond.send_response(b.body(()).expect("resp"), false)
                        else {
                            return;
                        };
                        // Ask for far more than any window here allows.
                        // What comes back is the client's advertised
                        // number, because this is the first byte of the
                        // stream and no `WINDOW_UPDATE` has been sent.
                        send.reserve_capacity(64 * 1024 * 1024);
                        rec.lock().expect("seen").push(Observed {
                            stream_capacity: send.capacity(),
                        });
                        // One large write, left to h2 to cut into DATA
                        // frames — which it does at the client's
                        // `SETTINGS_MAX_FRAME_SIZE`, and that is how the
                        // frame-size test below observes it.
                        let _ = send.send_data(bytes::Bytes::from_static(&[b'x'; BODY]), true);
                    }
                });
            }
        });
    });
    (addr, seen)
}

/// Sends one request and hands back the size of each DATA frame the body
/// arrived in.
///
/// One `Bytes` per DATA frame is `h2::RecvStream`'s own shape and survives
/// through `NativeBody`, so this is a wire observation rather than a
/// buffering artefact — which is also why the assertion below is on the
/// **largest** chunk: h2 may cut a frame short at a window boundary, but
/// it can never exceed the size the client advertised.
fn one_request(opts: H2Opts, addr: std::net::SocketAddr) -> Vec<usize> {
    try_request(opts, addr, "/x").expect("the exchange completes")
}

/// The fallible form, for the one test whose subject is a refusal.
fn try_request(
    opts: H2Opts,
    addr: std::net::SocketAddr,
    path: &str,
) -> Result<Vec<usize>, hclient_core::Error> {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime");
    let t = transport(opts);
    let resp = rt.block_on(async {
        tokio::time::timeout(
            BOUND,
            t.execute(
                http::Request::builder()
                    .uri(format!("https://127.0.0.1:{}{path}", addr.port()))
                    .body(RequestBody::Empty)
                    .expect("request"),
            ),
        )
        .await
        .expect("must not hang")
    })?;
    assert_eq!(resp.status(), 200);
    assert_eq!(resp.version(), http::Version::HTTP_2, "the premise");

    let mut sizes = Vec::new();
    let mut total = 0usize;
    rt.block_on(async {
        use http_body_util::BodyExt as _;
        let mut body = std::pin::pin!(resp.into_body());
        while let Some(frame) = tokio::time::timeout(BOUND, body.as_mut().frame())
            .await
            .expect("must not hang")
        {
            let Ok(frame) = frame else { break };
            if let Ok(d) = frame.into_data() {
                total += d.len();
                sizes.push(d.len());
            }
        }
    });
    assert_eq!(total, BODY, "the whole body arrives whatever the framing");
    Ok(sizes)
}

/// **The window a caller asks for is the window the server is granted**,
/// and the default is the control in the same test.
///
/// Both halves are needed and neither is enough. Without the default arm,
/// a client that ignored `h2_opts` entirely and happened to advertise the
/// asked-for number would pass; without the raised arm, a client that
/// advertised 65 535 unconditionally would.
#[test]
fn the_stream_window_a_caller_sets_is_the_one_the_peer_is_granted() {
    let (addr, seen) = spawn_server();

    one_request(H2Opts::default(), addr);
    one_request(
        H2Opts {
            initial_window_size: Some(RAISED_WINDOW as u32),
            // Raised with it: the connection window would otherwise cap
            // the stream at 65 535 whatever the stream setting says, which
            // is the trap the setter's doc names.
            initial_connection_window_size: Some(RAISED_WINDOW as u32),
            ..H2Opts::default()
        },
        addr,
    );

    let seen = seen.lock().expect("seen");
    assert_eq!(seen.len(), 2, "two connections, two observations");
    assert_eq!(
        seen[0].stream_capacity, RFC_DEFAULT_WINDOW,
        "the control: with no opts the RFC's default is what goes out"
    );
    assert_eq!(
        seen[1].stream_capacity, RAISED_WINDOW,
        "and the caller's number reaches the peer that has to obey it"
    );
}

/// **`max_frame_size` reaches the peer too**, and it needs its own
/// assertion rather than being assumed to ride along with the window: it
/// is a different `SETTINGS` parameter set through a different method, and
/// a `handshake` that forwarded one field and dropped another is exactly
/// the defect a one-field test cannot see.
///
/// Observed as the peer's **behaviour** rather than as a number, because
/// `h2::server::Connection` exposes no accessor for what the client
/// advertised. It does not need to: the server hands h2 one 512 KiB write
/// and h2 cuts it into DATA frames at the client's limit, one `Bytes` per
/// frame all the way through `NativeBody`.
#[test]
fn the_frame_size_a_caller_sets_changes_how_the_peer_frames_its_body() {
    let (addr, _) = spawn_server();

    // Windows raised in both arms, so the frame size is the only thing
    // that differs — otherwise the 65 535-byte window would be the
    // binding constraint and both arms would look alike.
    let base = H2Opts {
        initial_window_size: Some(4 << 20),
        initial_connection_window_size: Some(4 << 20),
        ..H2Opts::default()
    };
    let small = one_request(base, addr);
    let large = one_request(
        H2Opts {
            max_frame_size: Some(1 << 20),
            ..base
        },
        addr,
    );

    assert_eq!(
        small.iter().copied().max(),
        Some(RFC_DEFAULT_FRAME),
        "the control: RFC 9113 §6.5.2's default is what an unset field means"
    );
    assert!(
        large.iter().copied().max().unwrap_or(0) > RFC_DEFAULT_FRAME,
        "a raised limit lets the peer send bigger frames: {large:?}"
    );
    assert!(
        large.len() < small.len(),
        "and therefore fewer of them: {} against {}",
        large.len(),
        small.len()
    );
}

/// **The connection window is the ceiling for the stream one**, which is
/// why the setter's doc says to raise them together — and it is asserted
/// rather than described, because a caller who reads only the field name
/// would set the stream window alone and measure no change.
#[test]
fn raising_only_the_stream_window_leaves_the_connection_as_the_ceiling() {
    let (addr, seen) = spawn_server();
    one_request(
        H2Opts {
            initial_window_size: Some(RAISED_WINDOW as u32),
            ..H2Opts::default()
        },
        addr,
    );
    let seen = seen.lock().expect("seen");
    assert_eq!(
        seen[0].stream_capacity, RFC_DEFAULT_WINDOW,
        "the stream says 256 KiB and the connection still says 65 535, \
         so 65 535 is what the peer may send"
    );
}

/// **`max_header_list_size` reaches the wire too**, and this is the fourth
/// forwarded field — written because three tested fields and one untested
/// one is how a forwarding function loses a line without anybody noticing.
///
/// It is the client's own ceiling on a response head, so the observable is
/// local: `h2` enforces what it advertised
/// (`codec/framed_read.rs`'s `max_header_list_size`), so a response
/// carrying 8 KiB of header fails under a 1 KiB limit and succeeds under
/// h2's 16 MiB default. The control is the same server, the same path and
/// the same 8 KiB header.
#[test]
fn the_header_list_ceiling_a_caller_sets_is_the_one_enforced() {
    let (addr, _) = spawn_server();

    try_request(H2Opts::default(), addr, "/big")
        .expect("the control: 8 KiB of header is nothing to h2's default");

    let err = try_request(
        H2Opts {
            max_header_list_size: Some(1024),
            ..H2Opts::default()
        },
        addr,
        "/big",
    )
    .expect_err("8 KiB of header past a 1 KiB ceiling");
    assert!(
        matches!(
            *err.kind(),
            hclient_core::ErrorKind::Body | hclient_core::ErrorKind::Connect
        ),
        "the refusal is h2's, and which side of the head it lands on is \
         h2's business rather than this test's: {err:?}"
    );
}

/// The `SETTINGS` frame itself, read off the socket rather than inferred
/// from a peer's behaviour.
///
/// The four tests above each found a local observable — a granted window,
/// a DATA frame size, an enforced ceiling. `header_table_size` and
/// `enable_push` have none: `h2` never accepts a pushed stream whatever
/// the setting says, and HPACK table sizing shows up only as compression
/// behaviour. **What they do is change what goes on the wire, so the wire
/// is where they are checked.**
///
/// This also pins the claim `H2Opts`' own doc makes and nothing else
/// asserted: with every field `None`, this client sends a `SETTINGS` frame
/// with **no entries at all**.
fn settings_frame(listener: std::net::TcpListener) -> Vec<(u16, u32)> {
    use std::io::Read as _;

    let (mut sock, _) = listener.accept().expect("accept");
    sock.set_read_timeout(Some(BOUND)).expect("timeout");

    // RFC 9113 §3.4: the 24-byte connection preface, then frames.
    let mut preface = [0u8; 24];
    sock.read_exact(&mut preface).expect("preface");
    assert_eq!(&preface[..], b"PRI * HTTP/2.0\r\n\r\nSM\r\n\r\n");

    // RFC 9113 §4.1: length(24) type(8) flags(8) r+stream(32).
    let mut head = [0u8; 9];
    sock.read_exact(&mut head).expect("frame header");
    let len = u32::from_be_bytes([0, head[0], head[1], head[2]]) as usize;
    assert_eq!(
        head[3], 0x4,
        "the first frame after the preface is SETTINGS"
    );
    assert_eq!(head[4] & 0x1, 0, "and it is not the ACK");

    let mut body = vec![0u8; len];
    sock.read_exact(&mut body).expect("frame body");
    assert_eq!(len % 6, 0, "SETTINGS is a sequence of 6-octet entries");
    body.as_chunks::<6>()
        .0
        .iter()
        .map(|e| {
            (
                u16::from_be_bytes([e[0], e[1]]),
                u32::from_be_bytes([e[2], e[3], e[4], e[5]]),
            )
        })
        .collect()
}

/// Drives one request at a listener that will never answer, purely to make
/// the client send its preface. The request's failure is the expected
/// outcome and carries no information.
fn provoke_settings(opts: H2Opts) -> Vec<(u16, u32)> {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = listener.local_addr().expect("addr");

    let reader = std::thread::spawn(move || settings_frame(listener));

    let t = transport(opts);
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime");
    let _ = rt.block_on(async {
        tokio::time::timeout(
            BOUND,
            t.execute(
                http::Request::builder()
                    .uri(format!("https://{addr}/"))
                    .body(RequestBody::Empty)
                    .expect("request"),
            ),
        )
        .await
    });

    reader.join().expect("reader")
}

/// **A plain client announces nothing**, which is the good default and is
/// also what makes this client distinctive on the wire — see `H2Opts`.
#[test]
fn the_default_settings_frame_is_empty() {
    assert_eq!(
        provoke_settings(H2Opts::default()),
        Vec::new(),
        "every field is `None`, so there is nothing to announce"
    );
}

/// The two fields with no local observable, checked where they land.
///
/// `SETTINGS_HEADER_TABLE_SIZE` is `0x1` and `SETTINGS_ENABLE_PUSH` is
/// `0x2`, RFC 9113 §6.5.2. The pair is one test because the point is that
/// *both* reach the frame: a forwarding function that dropped one would
/// otherwise be caught by neither.
#[test]
fn the_two_settings_with_no_local_observable_reach_the_frame() {
    let seen = provoke_settings(H2Opts {
        header_table_size: Some(8192),
        enable_push: Some(false),
        ..H2Opts::default()
    });
    assert!(
        seen.contains(&(0x1, 8192)),
        "SETTINGS_HEADER_TABLE_SIZE must carry the number asked for: {seen:?}"
    );
    assert!(
        seen.contains(&(0x2, 0)),
        "SETTINGS_ENABLE_PUSH must carry the flag asked for: {seen:?}"
    );
}
