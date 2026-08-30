//! # Semver quarantine
//!
//! The traits in this module are the contract for backend and runtime
//! authors. It has not yet been validated against every backend, so:
//!
//! **Breaking changes in `unversioned` ship in a minor version, not a major.**
//!
//! This trick is borrowed from `ureq`. Without it, 1.0 is unshippable: you
//! can't freeze a trait without having checked it against native, wasi:http,
//! and fetch.

pub mod erased;
mod hooks;
mod timer;
mod transport;
mod websocket;

pub use hooks::{
    And, Attempt, ClientCertAsk, ClientCertRequest, CloseReason, Closed, ConnectTiming, Connected,
    ConnectionId, Counting, Direction, Event, Head, Hooks, HooksExt, Informational, Meter, Metered,
    NoHooks, Progress, Reporting, RequestId, Reused, mark, meter, since,
};
pub use timer::{Discard, Timer};
pub use transport::{BoxSendExchange, SendTransport, Transport};
pub use websocket::{CloseFrame, Message, WebSocket, WebSocketConnect};
