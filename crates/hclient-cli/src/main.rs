//! `hc` — one HTTP request from a command line, on a backend chosen at
//! runtime.
//!
//! The tool's one promise over curl is in `backend.rs`: a backend this
//! build does not carry is refused **by name**, beside the list of what it
//! has, where `CURL_SSL_BACKEND` in a non-`MultiSSL` build is accepted and
//! ignored.

mod args;
mod backend;
mod mode;
mod output;
mod run;
mod sse;
mod timings;

use clap::Parser;
use std::io::{IsTerminal, Write};

fn main() -> std::process::ExitCode {
    // `--version` before clap's own parse would be cleaner, but clap owns
    // the arguments; this is the flag with `disable_version_flag` so that
    // the backend list is printed rather than a bare version string.
    let raw: Vec<String> = std::env::args().collect();
    if raw.iter().any(|a| a == "-V" || a == "--version") {
        let mut out = std::io::stdout().lock();
        return match run::print_version(&mut out).and_then(|()| out.flush()) {
            Ok(()) => std::process::ExitCode::SUCCESS,
            Err(_) => std::process::ExitCode::from(7),
        };
    }

    let cli = args::Cli::parse();
    let is_tty = std::io::stdout().is_terminal();
    // Decided here and handed down, rather than by setting `NO_COLOR` in
    // the environment for `anstream` to read back: the environment is
    // process-global state and writing it needs `unsafe` under the 2024
    // edition, which this workspace forbids. `ColorChoice::Auto` still
    // honours a `NO_COLOR` the *caller* set — that reading is anstream's
    // and is not disturbed.
    let colour = if cli.no_color {
        anstream::ColorChoice::Never
    } else {
        anstream::ColorChoice::Auto
    };

    let rt = match tokio::runtime::Runtime::new() {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("hc: could not start a runtime: {e}");
            return std::process::ExitCode::from(7);
        }
    };
    match rt.block_on(run::run(cli, is_tty, colour)) {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(fail) => {
            let code = fail.code();
            eprintln!("hc: {fail}");
            std::process::ExitCode::from(u8::try_from(code).unwrap_or(1))
        }
    }
}
