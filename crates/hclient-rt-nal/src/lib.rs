//! `hclient_rt::TcpConnect` over `embedded-nal-async`, and the `Send` a
//! generic adapter could not have.
//!
//! # Why this is a macro and not a blanket impl
//!
//! `embedded_nal_async::TcpConnect::connect` is an `async fn` in trait, so
//! its future is an RPITIT with **no name**. A generic
//! `impl<S: TcpConnect> hclient_rt::TcpConnect for Adapter<S>` must
//! therefore box it, and a box decides the answer for every stack at once:
//! boxed plain it is `!Send` even for stacks that genuinely are, and boxed
//! `+ Send` it has to *prove* the RPITIT `Send` for a type parameter,
//! which needs return type notation — unstable, and the audience for this
//! is measurably on stable.
//!
//! This workspace's own rule is the way out: **at a concrete type `Send`
//! is inferred, in a generic impl it must be proven.** A macro moves the
//! impl to the concrete stack, where `Box::pin(stack.connect(addr))`
//! coerces into a `Send` box with nothing to prove and no bound to write.
//!
//! # And it does not claim `Send` for everybody
//!
//! [`adapt!`] expands a `Send` box, so a stack whose connect future is not
//! `Send` **fails to compile** — at the boxing site, naming the future.
//! That is the property, not a limitation: each stack answers for itself,
//! exactly as an associated future type would let it. [`adapt_local!`] is
//! the other answer, a plain box, for a stack that cannot make the claim —
//! the same split `Transport` and `SendTransport` already are one layer
//! up, where a backend that cannot promise `Send` keeps everything else.
//!
//! ```
//! # use hclient_rt_nal::adapt;
//! # struct MyStack;
//! # struct MyConn;
//! # impl embedded_io_async::ErrorType for MyConn { type Error = embedded_io_async::ErrorKind; }
//! # impl embedded_io_async::Read for MyConn {
//! #     async fn read(&mut self, _b: &mut [u8]) -> Result<usize, Self::Error> { Ok(0) }
//! # }
//! # impl embedded_io_async::Write for MyConn {
//! #     async fn write(&mut self, b: &[u8]) -> Result<usize, Self::Error> { Ok(b.len()) }
//! #     async fn flush(&mut self) -> Result<(), Self::Error> { Ok(()) }
//! # }
//! # impl embedded_nal_async::TcpConnect for MyStack {
//! #     type Error = embedded_io_async::ErrorKind;
//! #     type Connection<'a> = MyConn where Self: 'a;
//! #     async fn connect<'a>(&'a self, _r: core::net::SocketAddr)
//! #         -> Result<Self::Connection<'a>, Self::Error> { Ok(MyConn) }
//! # }
//! adapt!(MyRuntime, MyStack);
//!
//! // `MyRuntime` is now an `hclient_rt::TcpConnect` whose `Connecting`
//! // future is `Send`, so `Native<MyRuntime, ..>` can back an
//! // `hclient::Client`.
//! fn assert_send<T: Send>() {}
//! assert_send::<<MyRuntime as hclient_rt::TcpConnect>::Connecting<'static>>();
//! ```
//!
//! # The stack must be `'static`, and that is the embedded idiom anyway
//!
//! `hclient_rt::TcpConnect::Stream` carries no lifetime, while
//! `embedded_nal_async::TcpConnect::Connection<'a>` borrows the stack. So
//! the adapter holds a `&'static S` and hands back a
//! `Connection<'static>` — which is what a `StaticCell` gives you and what
//! every embassy program already does.

#![forbid(unsafe_code)]

mod io;

pub use io::{DEFAULT_CHUNK, NalIo};

#[doc(hidden)]
pub mod reexport {
    pub use embedded_nal_async;
    pub use hclient_rt;
    pub use hyper;
}

/// Adapt a concrete `embedded-nal-async` stack, claiming `Send`.
///
/// `adapt!(Name, Stack)` defines `pub struct Name(pub &'static Stack)` and
/// implements [`hclient_rt::TcpConnect`] for it. A stack whose connect
/// future is not `Send` will not compile — use [`adapt_local!`].
///
/// # The refusal, asserted rather than described
///
/// A stack that cannot promise `Send` does not compile through this
/// macro. That is the property the whole design rests on — a macro that
/// claimed `Send` for everybody would be a capability that lies — so it
/// is a fence rather than a paragraph:
///
/// ```compile_fail
/// # use hclient_rt_nal::adapt;
/// struct LocalStack(std::rc::Rc<()>);
/// struct LocalConn(std::rc::Rc<()>);
/// # impl embedded_io_async::ErrorType for LocalConn { type Error = embedded_io_async::ErrorKind; }
/// # impl embedded_io_async::Read for LocalConn {
/// #     async fn read(&mut self, _b: &mut [u8]) -> Result<usize, Self::Error> { Ok(0) }
/// # }
/// # impl embedded_io_async::Write for LocalConn {
/// #     async fn write(&mut self, b: &[u8]) -> Result<usize, Self::Error> { Ok(b.len()) }
/// #     async fn flush(&mut self) -> Result<(), Self::Error> { Ok(()) }
/// # }
/// # impl embedded_nal_async::TcpConnect for LocalStack {
/// #     type Error = embedded_io_async::ErrorKind;
/// #     type Connection<'a> = LocalConn where Self: 'a;
/// #     async fn connect<'a>(&'a self, _r: core::net::SocketAddr)
/// #         -> Result<Self::Connection<'a>, Self::Error> { Ok(LocalConn(self.0.clone())) }
/// # }
/// // An `Rc` in the stack, so the connect future cannot cross a thread:
/// // this does not compile, and `adapt_local!` is the answer.
/// adapt!(Wrong, LocalStack);
/// ```
///
/// `TcpOpts` are **not** applied: `embedded-nal-async` exposes no socket
/// options at all, so `APPLIES` stays at `TcpOptsSupport::NONE` and a
/// caller who set one gets the named `Unsupported` the seam already
/// produces. That is the understating direction, which is this workspace's
/// rule for every capability constant.
#[macro_export]
macro_rules! adapt {
    ($name:ident, $stack:ty) => {
        // The one place this crate declares the bound, and it is
        // amendment C15's subject exactly: an implementor naming the auto
        // traits of its **own** future. The macro puts that declaration at
        // the concrete stack, where it is inferred rather than proven —
        // and a stack that cannot keep it fails here, which is the
        // `compile_fail` fence above.
        $crate::__adapt_impl!($name, $stack, + ::core::marker::Send); // send-bound-exception: amendment-C15
    };
}

/// Adapt a concrete stack that **cannot** promise `Send` — `embassy-net`
/// and anything else holding a `RefCell`.
///
/// The resulting runtime is a full `hclient_rt::TcpConnect`; what it
/// cannot do is back an `hclient::Client`, which asks for a `Send`
/// transport. Everything below that seam works unchanged.
///
/// **Where the `&'static` comes from is worth knowing here and not
/// above.** A stack that is not `Send` is usually not `Sync` either, and a
/// `static` item requires `Sync` — so `static S: MyStack = ..` will not
/// compile. `Box::leak`, or the `StaticCell` an embassy program already
/// uses, is how you get the reference; a `&'static T` is fine to hold on
/// one thread whatever `T` is.
#[macro_export]
macro_rules! adapt_local {
    ($name:ident, $stack:ty) => {
        $crate::__adapt_impl!($name, $stack,);
    };
}

#[doc(hidden)]
#[macro_export]
macro_rules! __adapt_impl {
    ($name:ident, $stack:ty, $($send:tt)*) => {
        pub struct $name(pub &'static $stack);

        impl $crate::reexport::hclient_rt::TcpConnect for $name {
            type Stream = $crate::NalIo<
                <$stack as $crate::reexport::embedded_nal_async::TcpConnect>::Connection<'static>,
            >;

            type Connecting<'a> = ::core::pin::Pin<
                ::std::boxed::Box<
                    dyn ::core::future::Future<
                        Output = ::std::io::Result<Self::Stream>,
                    > $($send)* + 'a,
                >,
            >;

            fn connect<'a>(
                &'a self,
                addr: ::std::net::SocketAddr,
                _opts: &$crate::reexport::hclient_rt::TcpOpts,
            ) -> Self::Connecting<'a> {
                let stack = self.0;
                ::std::boxed::Box::pin(async move {
                    let conn = $crate::reexport::embedded_nal_async::TcpConnect::connect(
                        stack, addr,
                    )
                    .await
                    .map_err(|e| {
                        ::std::io::Error::new(
                            ::std::io::ErrorKind::ConnectionRefused,
                            "embedded-nal connect",
                        )
                    })?;
                    ::core::result::Result::Ok($crate::NalIo::new(conn))
                })
            }

            type ConnectingUnix<'a> =
                $crate::reexport::hclient_rt::UnixUnsupported<Self::Stream>;

            fn connect_unix<'a>(&'a self, _path: &::std::path::Path) -> Self::ConnectingUnix<'a> {
                $crate::reexport::hclient_rt::UnixUnsupported::new()
            }
        }
    };
}
