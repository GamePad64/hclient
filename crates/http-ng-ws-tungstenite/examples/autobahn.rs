//! The Autobahn TestSuite client driver: an echo client, and nothing else.
//!
//! # Why this exists
//!
//! Every fixture in `tests/websocket.rs` was written beside the
//! implementation it observes. That is the arrangement in which a fixture
//! can agree with a bug, and no amount of care inside one repository
//! removes it. The Autobahn TestSuite is the external oracle: ~520 client
//! cases written by people who have never seen this code, driven by
//! `wstest --mode fuzzingserver`, which serves
//! `ws://host:9001/runCase?case=N&agent=…` and scores what comes back.
//!
//! # What it must not do
//!
//! **Nothing here may interpret a case.** The whole value of an external
//! oracle is that it disagrees with us; a driver that special-cased a
//! frame, retried a failure or suppressed an error would be tuning the run
//! to look good, which is the one outcome that makes the exercise
//! worthless. So this file echoes text as text and binary as binary, polls
//! the `Stream` to its documented end, and reports nothing of its own.
//! Ping and pong never reach it — RFC 6455 §5.5.2 makes answering a ping
//! the endpoint's duty and `http_ng_core::unversioned::Message` has no
//! variant for one, deliberately.
//!
//! The verdict comes from `/updateReports`, is written by the suite, and
//! is read by `scripts/autobahn-report.py`. This program's own exit code
//! says only whether it managed to drive the suite at all.
//!
//! # Run it
//!
//! ```text
//! just test-autobahn          # container, driver and verdict together
//! ```
//!
//! or, against a `fuzzingserver` already listening:
//!
//! ```text
//! cargo run -p http-ng-ws-tungstenite --example autobahn \
//!     -- ws://127.0.0.1:9001 http-ng-ws-tungstenite
//! ```

use futures_util::{SinkExt, StreamExt};
use http_ng_core::unversioned::{Message, WebSocketConnect};
use http_ng_dns_system::SystemDns;
use http_ng_native::Native;
use http_ng_rt_tokio::Tokio;
use http_ng_tls_rustls::Rustls;
use http_ng_ws_tungstenite::Tungstenite;

type Client<'a> = Tungstenite<'a, Tokio, Rustls, SystemDns<Tokio>>;

/// One connection, opened and driven to its end.
///
/// `f` sees every message the server sent. The `Stream` is polled until it
/// yields `None` even after a `Message::Close`, because that is what the
/// seam documents ("keep polling the `Stream` until it ends if the peer's
/// answer matters") and because the close *reply* tungstenite queues on
/// our behalf is only written by a later poll. A driver that broke out on
/// `Close` would leave the closing handshake unfinished on every one of
/// the ~520 cases and blame the library for it.
async fn drive<F>(client: &Client<'_>, uri: &str, mut f: F) -> Result<(), String>
where
    F: AsyncEcho,
{
    let req = http::Request::builder()
        .uri(uri)
        .body(())
        .map_err(|e| format!("{uri}: {e}"))?;
    let mut ws = client
        .websocket(req)
        .await
        .map_err(|e| format!("{uri}: handshake: {e}"))?;
    while let Some(msg) = ws.next().await {
        match msg {
            Ok(m) => {
                if let Some(reply) = f.on(m)
                    && let Err(e) = ws.send(reply).await
                {
                    // Not an error of the run: a case that fails the
                    // connection from the server's side makes the very
                    // next write fail, and that is the case doing its job.
                    return Err(format!("{uri}: send: {e}"));
                }
            }
            Err(e) => return Err(format!("{uri}: recv: {e}")),
        }
    }
    Ok(())
}

/// What to do with each inbound message. A trait rather than a closure
/// because the two callers want different things and one of them wants
/// to keep state.
trait AsyncEcho {
    fn on(&mut self, msg: Message) -> Option<Message>;
}

/// The echo the suite scores: text back as text, binary back as binary,
/// close left to the layer that owns it.
struct Echo;

impl AsyncEcho for Echo {
    fn on(&mut self, msg: Message) -> Option<Message> {
        match msg {
            Message::Text(t) => Some(Message::Text(t)),
            Message::Binary(b) => Some(Message::Binary(b)),
            // The peer is closing. `tungstenite` has already queued the
            // matching reply; echoing a second close here would send two.
            Message::Close(_) => None,
        }
    }
}

/// Collects the first text message, for `/getCaseCount`.
struct First(Option<String>);

impl AsyncEcho for First {
    fn on(&mut self, msg: Message) -> Option<Message> {
        if let Message::Text(t) = msg
            && self.0.is_none()
        {
            self.0 = Some(t);
        }
        None
    }
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> std::process::ExitCode {
    let mut args = std::env::args().skip(1);
    let base = args.next().unwrap_or_else(|| "ws://127.0.0.1:9001".into());
    let agent = args
        .next()
        .unwrap_or_else(|| "http-ng-ws-tungstenite".into());
    let base = base.trim_end_matches('/').to_owned();

    let transport = Native::new(Tokio, Rustls::with_webpki_roots(), SystemDns::new(Tokio));
    // The connector borrows the transport rather than owning it — see
    // `Tungstenite`. Here that costs one binding.
    let client = Tungstenite::new(&transport);

    let mut count = First(None);
    if let Err(e) = drive(&client, &format!("{base}/getCaseCount"), &mut count).await {
        eprintln!("::error::could not ask the fuzzingserver for its case count: {e}");
        return std::process::ExitCode::FAILURE;
    }
    let Some(total) = count
        .0
        .as_deref()
        .and_then(|s| s.trim().parse::<u32>().ok())
    else {
        eprintln!(
            "::error::the fuzzingserver's case count is not a number: {:?}",
            count.0
        );
        return std::process::ExitCode::FAILURE;
    };
    if total == 0 {
        eprintln!("::error::the fuzzingserver reports zero cases — there is nothing to run");
        return std::process::ExitCode::FAILURE;
    }
    println!("autobahn: {total} cases against {base} as {agent}");

    for case in 1..=total {
        let uri = format!("{base}/runCase?case={case}&agent={agent}");
        // A case that ends in an error is the *normal* outcome for the
        // several dozen that exist to make a client fail the connection.
        // The suite scores it; this line only makes the run readable.
        if let Err(e) = drive(&client, &uri, Echo).await {
            println!("case {case}: ended with {e}");
        }
    }

    let uri = format!("{base}/updateReports?agent={agent}");
    if let Err(e) = drive(&client, &uri, First(None)).await {
        eprintln!("::error::the fuzzingserver refused to write its report: {e}");
        return std::process::ExitCode::FAILURE;
    }
    println!("autobahn: {total} cases run, report written");
    std::process::ExitCode::SUCCESS
}

impl<T: AsyncEcho> AsyncEcho for &mut T {
    fn on(&mut self, msg: Message) -> Option<Message> {
        (**self).on(msg)
    }
}
