//! Spike 3c: **does the proposed shape carry the driver of an idle QUIC
//! connection?**
//!
//! `cargo run` (in `spikes/quic-idle`)
//!
//! `docs/h3-research.md` §1.5 measured the failure: a QUIC connection
//! nobody polls across a gap is not idle, it is dying — request 2 comes
//! back `reset by peer`. The research listed "make `Spawn` usable" as way
//! out 3 and set it aside because "the seam's implementations also require
//! `'static`".
//!
//! This asks the narrower question the brief asks: does the *type* fit,
//! and does spawning through `http_ng_rt::Spawn` actually keep the
//! connection alive across the gap?
//!
//! `quinn::Runtime::spawn` (quinn 0.11.11, `src/runtime.rs:21`) is
//!
//! ```text
//! fn spawn(&self, future: Pin<Box<dyn Future<Output = ()> + Send>>);
//! ```
//!
//! — a **named** type (so `Spawn<F>`'s "F must be nameable" wall does not
//! apply), which is `Send` and `'static`. That is exactly what
//! `impl<F: Future<Output = ()> + Send + 'static> Spawn<F> for Tokio`
//! already accepts, unchanged.
//!
//! Two runs, one gap, one difference:
//!
//! - A: `QueueRuntime` — quinn's driver futures are queued and nobody
//!   drains the queue during the gap. This is `docs/h3-research.md`'s
//!   `nospawn.rs` shape.
//! - B: `SeamRuntime<Tokio>` — the same futures are handed to
//!   `http_ng_rt::Spawn::spawn`, and nothing else changes.

use http_ng_rt::Spawn;
use quinn::rustls;
use std::future::Future;
use std::net::{SocketAddr, UdpSocket};
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// The gap. Longer than the connection's idle timeout on purpose.
const GAP: Duration = Duration::from_millis(1500);
const IDLE_TIMEOUT: Duration = Duration::from_millis(1000);

// ---------------------------------------------------------------------------
// Runtime A: queue the futures, never run them
// ---------------------------------------------------------------------------

/// What "no spawn" really looks like: quinn insists on a spawner, so it
/// gets one that only queues. Nothing drains it here — that is the point.
struct QueueRuntime {
    inner: quinn::TokioRuntime,
    queued: Arc<Mutex<Vec<Pin<Box<dyn Future<Output = ()> + Send>>>>>,
}

impl std::fmt::Debug for QueueRuntime {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("QueueRuntime").finish_non_exhaustive()
    }
}

impl quinn::Runtime for QueueRuntime {
    fn new_timer(&self, i: std::time::Instant) -> Pin<Box<dyn quinn::AsyncTimer>> {
        self.inner.new_timer(i)
    }
    fn spawn(&self, future: Pin<Box<dyn Future<Output = ()> + Send>>) {
        self.queued.lock().unwrap().push(future);
    }
    fn wrap_udp_socket(&self, t: UdpSocket) -> std::io::Result<Arc<dyn quinn::AsyncUdpSocket>> {
        self.inner.wrap_udp_socket(t)
    }
}

// ---------------------------------------------------------------------------
// Runtime B: hand the futures to `http_ng_rt::Spawn`
// ---------------------------------------------------------------------------

/// The whole proposal, applied. `R` is a runtime from this workspace's own
/// seam; the only thing this type does is forward `spawn` to it.
///
/// The bound is written with the *named* type quinn hands over. No
/// `Send`/`'static` is declared here — `R`'s own `Spawn` impl decides
/// whether it accepts that type, which is what `Spawn<F>`'s doc comment
/// means by "`Send` is added by the `impl`, not the trait".
#[derive(Debug)]
struct SeamRuntime<R> {
    inner: quinn::TokioRuntime,
    rt: R,
}

impl<R> quinn::Runtime for SeamRuntime<R>
where
    R: Spawn<Pin<Box<dyn Future<Output = ()> + Send>>> + std::fmt::Debug + Send + Sync + 'static,
{
    fn new_timer(&self, i: std::time::Instant) -> Pin<Box<dyn quinn::AsyncTimer>> {
        self.inner.new_timer(i)
    }
    fn spawn(&self, future: Pin<Box<dyn Future<Output = ()> + Send>>) {
        Spawn::spawn(&self.rt, future);
    }
    fn wrap_udp_socket(&self, t: UdpSocket) -> std::io::Result<Arc<dyn quinn::AsyncUdpSocket>> {
        self.inner.wrap_udp_socket(t)
    }
}

// ---------------------------------------------------------------------------

struct Certs {
    der: rustls::pki_types::CertificateDer<'static>,
    key: rustls::pki_types::PrivateKeyDer<'static>,
}

fn certs() -> Certs {
    let c = rcgen::generate_simple_self_signed(vec!["localhost".into()]).unwrap();
    Certs {
        der: c.cert.into(),
        key: rustls::pki_types::PrivatePkcs8KeyDer::from(c.key_pair.serialize_der()).into(),
    }
}

/// A server that answers every bidi stream with `b"pong"`. Runs on an
/// ordinary tokio runtime — the question is about the client.
async fn server(c: &Certs) -> (SocketAddr, quinn::Endpoint) {
    let mut cfg = quinn::ServerConfig::with_single_cert(
        vec![c.der.clone()],
        c.key.clone_key(),
    )
    .unwrap();
    let mut tp = quinn::TransportConfig::default();
    tp.max_idle_timeout(Some(IDLE_TIMEOUT.try_into().unwrap()));
    cfg.transport_config(Arc::new(tp));

    let ep = quinn::Endpoint::server(cfg, "127.0.0.1:0".parse().unwrap()).unwrap();
    let addr = ep.local_addr().unwrap();
    let acceptor = ep.clone();
    tokio::spawn(async move {
        while let Some(inc) = acceptor.accept().await {
            tokio::spawn(async move {
                let Ok(conn) = inc.await else { return };
                while let Ok((mut send, mut recv)) = conn.accept_bi().await {
                    let _ = recv.read_to_end(64).await;
                    let _ = send.write_all(b"pong").await;
                    let _ = send.finish();
                }
            });
        }
    });
    (addr, ep)
}

fn client_config(c: &Certs) -> quinn::ClientConfig {
    let mut roots = rustls::RootCertStore::empty();
    roots.add(c.der.clone()).unwrap();
    let crypto = rustls::ClientConfig::builder()
        .with_root_certificates(roots)
        .with_no_client_auth();
    let mut cfg = quinn::ClientConfig::new(Arc::new(
        quinn::crypto::rustls::QuicClientConfig::try_from(crypto).unwrap(),
    ));
    let mut tp = quinn::TransportConfig::default();
    tp.max_idle_timeout(Some(IDLE_TIMEOUT.try_into().unwrap()));
    // No keep-alive: the connection lives only if somebody drives it.
    tp.keep_alive_interval(None);
    cfg.transport_config(Arc::new(tp));
    cfg
}

async fn roundtrip(conn: &quinn::Connection) -> Result<String, String> {
    let (mut send, mut recv) = conn.open_bi().await.map_err(|e| e.to_string())?;
    send.write_all(b"ping").await.map_err(|e| e.to_string())?;
    send.finish().map_err(|e| e.to_string())?;
    let buf = recv.read_to_end(64).await.map_err(|e| e.to_string())?;
    Ok(String::from_utf8_lossy(&buf).into_owned())
}

async fn run(label: &str, rt: Arc<dyn quinn::Runtime>, c: &Certs, addr: SocketAddr) {
    let sock = UdpSocket::bind("127.0.0.1:0").unwrap();
    let mut ep =
        quinn::Endpoint::new(quinn::EndpointConfig::default(), None, sock, rt).unwrap();
    ep.set_default_client_config(client_config(c));

    let conn = match ep.connect(addr, "localhost").unwrap().await {
        Ok(c) => c,
        Err(e) => {
            println!("  {label}: could not even connect: {e}");
            return;
        }
    };
    println!("  {label}: request 1 -> {:?}", roundtrip(&conn).await);
    println!(
        "  {label}: gap of {}ms with idle_timeout {}ms and no keep-alive",
        GAP.as_millis(),
        IDLE_TIMEOUT.as_millis()
    );
    tokio::time::sleep(GAP).await;
    println!("  {label}: request 2 -> {:?}", roundtrip(&conn).await);
}

#[tokio::main(flavor = "current_thread")]
async fn main() {
    rustls::crypto::ring::default_provider()
        .install_default()
        .ok();
    let c = certs();
    let (addr, _server_ep) = server(&c).await;

    println!("A. QueueRuntime — quinn's driver futures queued, nobody drains them");
    let queued: Arc<Mutex<Vec<Pin<Box<dyn Future<Output = ()> + Send>>>>> =
        Arc::new(Mutex::new(Vec::new()));
    run(
        "A",
        Arc::new(QueueRuntime {
            inner: quinn::TokioRuntime,
            queued: queued.clone(),
        }),
        &c,
        addr,
    )
    .await;
    println!(
        "  A: quinn futures queued and never run: {}",
        queued.lock().unwrap().len()
    );

    println!("\nB. SeamRuntime<http_ng_rt_tokio::Tokio> — the SAME futures, handed to http_ng_rt::Spawn::spawn");
    run(
        "B",
        Arc::new(SeamRuntime {
            inner: quinn::TokioRuntime,
            rt: http_ng_rt_tokio::Tokio,
        }),
        &c,
        addr,
    )
    .await;
}
