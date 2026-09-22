//! What can be checked without a TLS server: the seam is implemented, and
//! the one refusal this backend makes happens before it touches the
//! transport.

use hclient_core::error::ErrorKind;
use hclient_tls::{TlsConnect, TlsRequest};
use hclient_tls_native_tls::NativeTls;
use std::pin::Pin;
use std::task::{Context, Poll};

/// A transport that fails the test if anything reads or writes it. Any use
/// at all means the check under test happened too late.
#[derive(Debug)]
struct NeverTouched;

impl futures_io::AsyncRead for NeverTouched {
    fn poll_read(
        self: Pin<&mut Self>,
        _: &mut Context<'_>,
        _: &mut [u8],
    ) -> Poll<std::io::Result<usize>> {
        panic!("the transport was read — the refusal came after the handshake started");
    }
}

impl futures_io::AsyncWrite for NeverTouched {
    fn poll_write(
        self: Pin<&mut Self>,
        _: &mut Context<'_>,
        _: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        panic!("the transport was written — the refusal came after the handshake started");
    }
    fn poll_flush(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        panic!("the transport was flushed — the refusal came after the handshake started");
    }
    fn poll_close(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        panic!("the transport was shut down — the refusal came after the handshake started");
    }
}

impl hclient_rt::Shutdown for NeverTouched {
    fn poll_shutdown(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        panic!("the transport was shut down — the refusal came after the handshake started");
    }
}

fn assert_tls_connect<T: TlsConnect>() {}

#[test]
fn native_tls_implements_the_seam() {
    assert_tls_connect::<NativeTls>();
}

/// ECH is refused, not silently skipped — and refused before a single byte
/// reaches the wire. Connecting without the encryption the caller asked for
/// would leak the SNI they were trying to hide, so "best effort" here is
/// worse than an error. `NeverTouched` panics on any IO, so a refusal that
/// arrived after the handshake began would fail this test rather than pass.
#[test]
fn ech_is_refused_before_the_transport_is_touched() {
    let tls = NativeTls::new();
    let req = TlsRequest::new("example.com", &[b"h2"]).ech(Some(&[1, 2, 3]));
    let err = futures_executor::block_on(tls.connect(NeverTouched, req))
        .expect_err("ECH must be refused, not ignored");
    assert_eq!(*err.kind(), ErrorKind::Tls, "{err}");
    assert!(
        err.to_string().to_lowercase().contains("ech"),
        "the error must name what was refused, so a caller can tell it from a handshake failure: {err}"
    );
}

/// The same request without ECH gets past the check and into the handshake
/// — proving the refusal above is conditional on `ech` and not on something
/// incidental. It panics inside `NeverTouched`, which is exactly the
/// evidence wanted: the code reached the transport.
#[test]
#[should_panic(expected = "the transport was")]
fn without_ech_the_handshake_actually_starts() {
    let tls = NativeTls::new();
    let req = TlsRequest::new("example.com", &[b"h2"]);
    let _ = futures_executor::block_on(tls.connect(NeverTouched, req));
}

/// The two properties the port exists for, and neither was true before it.
///
/// # `Send` follows from the IO, and is declared nowhere
///
/// `TlsConnect::Handshake` is an associated type so a consumer can name
/// it; this backend answers with a concrete struct rather than a box, so
/// `Send` is *derived* from `S` instead of being fixed for every `S`. That
/// is what lets `hclient::Client` — which requires `SendTransport` — be
/// built over the platform TLS stack at all. Checked in the failing
/// direction while writing: with the handshake behind a `dyn` with no
/// declared auto trait, `Client::builder` over this backend does not
/// compile, which is the state this crate shipped in.
///
/// The `!Send` half is asserted too, because a struct that were `Send`
/// unconditionally would pass the first assertion while lying: the whole
/// point is that the answer tracks the IO.
#[test]
#[allow(
    clippy::items_after_statements,
    reason = "SendIo/UnsendIo/Probe/Fallback are the Send-probe fixtures this test is entirely about — they belong beside the assertions that use them, not at module scope where nothing else needs them"
)]
#[allow(
    clippy::too_many_lines,
    reason = "106 lines, and the excess is the two fixtures' `Shutdown` impls: the seam split `poll_shutdown` off `Write`, so each double owes one more impl. Moving them to module scope is what the allow above deliberately refuses."
)]
fn the_handshake_is_send_exactly_when_the_io_is() {
    fn is_send<T: Send>() {}

    // A `Send` transport, which is what a real one is.
    struct SendIo;
    impl futures_io::AsyncRead for SendIo {
        fn poll_read(
            self: std::pin::Pin<&mut Self>,
            _: &mut std::task::Context<'_>,
            _: &mut [u8],
        ) -> std::task::Poll<std::io::Result<usize>> {
            unreachable!("never polled")
        }
    }
    impl futures_io::AsyncWrite for SendIo {
        fn poll_write(
            self: std::pin::Pin<&mut Self>,
            _: &mut std::task::Context<'_>,
            _: &[u8],
        ) -> std::task::Poll<std::io::Result<usize>> {
            unreachable!("never polled")
        }
        fn poll_flush(
            self: std::pin::Pin<&mut Self>,
            _: &mut std::task::Context<'_>,
        ) -> std::task::Poll<std::io::Result<()>> {
            unreachable!("never polled")
        }
        fn poll_close(
            self: std::pin::Pin<&mut Self>,
            _: &mut std::task::Context<'_>,
        ) -> std::task::Poll<std::io::Result<()>> {
            unreachable!("never polled")
        }
    }

    impl hclient_rt::Shutdown for SendIo {
        fn poll_shutdown(
            self: std::pin::Pin<&mut Self>,
            _: &mut std::task::Context<'_>,
        ) -> std::task::Poll<std::io::Result<()>> {
            unreachable!("never polled")
        }
    }

    is_send::<<NativeTls as TlsConnect>::Handshake<'static, SendIo>>();

    // And the negation, at a type that holds an `Rc`.
    struct UnsendIo(
        #[allow(
            dead_code,
            reason = "the `Rc` is what makes this IO `!Send`, which is the whole of what the fixture asserts; no code reads it"
        )]
        std::rc::Rc<()>,
    );
    impl futures_io::AsyncRead for UnsendIo {
        fn poll_read(
            self: std::pin::Pin<&mut Self>,
            _: &mut std::task::Context<'_>,
            _: &mut [u8],
        ) -> std::task::Poll<std::io::Result<usize>> {
            unreachable!("never polled")
        }
    }
    impl futures_io::AsyncWrite for UnsendIo {
        fn poll_write(
            self: std::pin::Pin<&mut Self>,
            _: &mut std::task::Context<'_>,
            _: &[u8],
        ) -> std::task::Poll<std::io::Result<usize>> {
            unreachable!("never polled")
        }
        fn poll_flush(
            self: std::pin::Pin<&mut Self>,
            _: &mut std::task::Context<'_>,
        ) -> std::task::Poll<std::io::Result<()>> {
            unreachable!("never polled")
        }
        fn poll_close(
            self: std::pin::Pin<&mut Self>,
            _: &mut std::task::Context<'_>,
        ) -> std::task::Poll<std::io::Result<()>> {
            unreachable!("never polled")
        }
    }

    impl hclient_rt::Shutdown for UnsendIo {
        fn poll_shutdown(
            self: std::pin::Pin<&mut Self>,
            _: &mut std::task::Context<'_>,
        ) -> std::task::Poll<std::io::Result<()>> {
            unreachable!("never polled")
        }
    }
    // A real negative, not `fn assert_not_send<T>() {}` — which accepts
    // anything and would have passed for a handshake that was `Send`
    // unconditionally, i.e. for the exact defect this half exists to
    // catch. Inherent methods win over trait ones, so the answer is
    // `true` only where the bound holds.
    struct Probe<T>(std::marker::PhantomData<T>);
    trait Fallback {
        fn is_send() -> bool {
            false
        }
    }
    impl<T> Fallback for Probe<T> {}
    impl<T: Send> Probe<T> {
        fn is_send() -> bool {
            true
        }
    }

    assert!(
        Probe::<<NativeTls as TlsConnect>::Handshake<'static, SendIo>>::is_send(),
        "a handshake over Send IO must be Send — this is what Client rests on",
    );
    assert!(
        !Probe::<<NativeTls as TlsConnect>::Handshake<'static, UnsendIo>>::is_send(),
        "and one over !Send IO must not be: the answer tracks the IO rather than being fixed",
    );
}

/// **`reports_alpn` is `true`**, which this crate's own documentation
/// called impossible for two verticals.
///
/// It is not a claim about the platform: `native_tls::TlsStream::
/// negotiated_alpn` was always public, and only `async_native_tls::
/// TlsStream`'s failure to re-export it made the answer unreadable. Owning
/// the stream made it reachable, and `Native` will offer `h2` over this
/// backend because of this one `bool`.
#[test]
fn alpn_is_reported_now() {
    assert!(
        NativeTls::new().reports_alpn(),
        "owning the stream is what makes the negotiated protocol readable",
    );
}
