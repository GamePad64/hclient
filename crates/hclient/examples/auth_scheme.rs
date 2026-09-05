//! An authentication scheme of your own — the seam NTLM was left room for.
//!
//! [`hclient::auth::Auth`] and [`AuthFlow`](hclient::auth::AuthFlow) exist
//! because of a measurement rather than a taste for abstraction: NTLM and
//! Negotiate need a platform's own security provider, and those crates
//! have real users — `libgssapi`, `sspi` and `cross-krb5` are each around
//! half a million downloads a month — while there is **no HTTP glue over
//! any of them** on crates.io. No `reqwest-ntlm`, no `hyper-ntlm`, because
//! those clients have nowhere to put one. So this crate does not grow a
//! Kerberos dependency; it grows the two traits somebody else needs to
//! write one in their own crate.
//!
//! Nobody has written one yet, which makes this file the only evidence
//! that they are writable at all.
//!
//! ```text
//! cargo run -p hclient --example auth_scheme --features test-util
//! ```
//!
//! # Two traits, and the split is the whole design
//!
//! [`Auth`](hclient::auth::Auth) is the **configuration**, shared by every
//! clone of the client and every request in flight. [`AuthFlow`] is **one
//! exchange's state**, made fresh per hop — because a scheme with three
//! legs has to remember which leg it is on, and a value shared across
//! requests cannot. It is httpx's generator-based `Auth` —
//! `response = yield request` — written as a state machine, which is what
//! Rust has instead.
//!
//! # Three rules the client enforces and a flow cannot override
//!
//! A body that cannot be replayed ends the exchange, checked before every
//! extra leg exactly as for a `425` and a retry. Credentials do not cross
//! an origin. And `MAX_LEGS` bounds a flow that never says `Done`, which
//! would otherwise be an infinite exchange against a server that keeps
//! challenging.
//!
//! # What the first shape got wrong
//!
//! It made two flows, one before the send and one after the response — so
//! the second had never seen the request, and a scheme that hashes the
//! method and target into its answer had nothing to hash. One flow per
//! hop, made before the first send, and it sits *outside* the retry loop:
//! a retry re-sends the same request, and a flow counting legs must not
//! count an attempt that failed for the network's reasons.
#![cfg(feature = "test-util")]

use hclient::Client;
use hclient::auth::{Auth, AuthFlow, AuthRequest, AuthStep, BoxedFlow};
use hclient::mock::MockTransport;

/// A two-leg scheme: send nothing, read the challenge, answer it.
///
/// Deliberately not a real one — the arithmetic of a real scheme is its
/// own crate's problem, and what this shows is the shape around it.
#[derive(Debug)]
struct Token {
    secret: String,
}

impl Auth for Token {
    /// One flow per hop. It carries what `authorize` learned from
    /// `on_response`, which is the state a shared value could not hold.
    fn start(&self) -> BoxedFlow {
        Box::new(TokenFlow {
            secret: self.secret.clone(),
            challenge: None,
        })
    }
}

struct TokenFlow {
    secret: String,
    /// `None` until the server has challenged. This is the whole reason a
    /// flow exists per exchange rather than per client.
    challenge: Option<String>,
}

impl AuthFlow for TokenFlow {
    /// Called before **every** attempt, including the first. A scheme that
    /// can authenticate pre-emptively does it here and never needs a
    /// second leg; this one has nothing to say until it has been
    /// challenged.
    fn authorize(&mut self, req: &AuthRequest<'_>, headers: &mut http::HeaderMap) {
        let Some(challenge) = &self.challenge else {
            return;
        };
        // A real scheme signs `req.method()` and the request-target here —
        // which is why `authorize` is handed them, and why the first
        // design that made a second flow after the response could not
        // work.
        let answer = format!("Token {}:{}:{}", challenge, self.secret, req.method());
        let mut value = http::HeaderValue::from_str(&answer).expect("ASCII");
        // Keeps the credential out of a `Debug`, which is the only place
        // it would otherwise show.
        value.set_sensitive(true);
        headers.insert(http::header::AUTHORIZATION, value);
    }

    /// Only the head is offered, deliberately: a scheme that needed the
    /// body would make this client buffer every response before deciding.
    fn on_response(&mut self, status: http::StatusCode, headers: &http::HeaderMap) -> AuthStep {
        if status != http::StatusCode::UNAUTHORIZED || self.challenge.is_some() {
            // Either it worked, or it failed with our answer already sent —
            // and a scheme that kept trying would be an infinite exchange
            // that only `MAX_LEGS` could stop.
            return AuthStep::Done;
        }
        let Some(nonce) = headers
            .get(http::header::WWW_AUTHENTICATE)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.strip_prefix("Token nonce="))
        else {
            return AuthStep::Done;
        };
        self.challenge = Some(nonce.to_owned());
        // Send it again, and `authorize` above now has something to say.
        AuthStep::Again
    }
}

fn main() {
    let transport = MockTransport::new();
    transport.push_response(
        http::Response::builder()
            .status(401)
            .header("www-authenticate", "Token nonce=xyz")
            .body("")
            .unwrap(),
    );
    transport.push_response(
        http::Response::builder()
            .status(200)
            .body("welcome")
            .unwrap(),
    );

    let client = Client::builder(transport.clone())
        .base_url("https://example.test".parse().unwrap())
        .build()
        .expect("nothing here needs a capability the mock lacks");

    let body = futures_executor::block_on(async {
        client
            .get("/private")
            .auth(Token {
                secret: "s3cret".to_owned(),
            })
            .send()
            .await?
            .collect()
            .await?
            .text()
    })
    .expect("challenge, then answer");

    assert_eq!(body, "welcome");

    let sent = transport.requests();
    assert_eq!(
        sent.len(),
        2,
        "one leg for the challenge, one for the answer"
    );
    assert!(
        !sent[0].headers.contains_key("authorization"),
        "nothing to say before the server has challenged"
    );
    assert_eq!(
        sent[1].headers["authorization"], "Token xyz:s3cret:GET",
        "the answer carries what the flow learned, and the method it signs"
    );

    println!("legs: {}", sent.len());
    println!(
        "first carried an Authorization: {}",
        sent[0].headers.contains_key("authorization")
    );
    println!("body: {body}");
}
