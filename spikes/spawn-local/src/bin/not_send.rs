//! Negative control 1: `Tokio` and `Smol` reject the very future
//! `TokioLocal`/`SmolLocal` accept. Must NOT compile.
//!
//! `cargo build --bin not_send --features must-fail`

use http_ng_rt::Spawn;
use std::cell::Cell;
use std::rc::Rc;

fn main() {
    let flag = Rc::new(Cell::new(0u32));
    let f = async move {
        flag.set(flag.get() + 1);
    };
    Spawn::spawn(&http_ng_rt_tokio::Tokio, f);

    let flag2 = Rc::new(Cell::new(0u32));
    let g = async move {
        flag2.set(flag2.get() + 1);
    };
    Spawn::spawn(&http_ng_rt_smol::Smol, g);
}
