//! Deterministic measured integration, not a wrapper for a vendor Agent.
//!
//! Its entire input contract is opaque stdio echo. It never interprets workspace
//! bytes, loads plugins, executes commands, reads configuration or opens sockets.
//! The test broker separately authorizes tools through the supervisor protocol.

use std::{io, process::ExitCode};

fn main() -> ExitCode {
    match io::copy(&mut io::stdin().lock(), &mut io::stdout().lock()) {
        Ok(_) => ExitCode::SUCCESS,
        Err(_) => ExitCode::FAILURE,
    }
}
