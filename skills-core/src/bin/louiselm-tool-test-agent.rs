//! Deterministic measured integration, not a wrapper for a vendor Agent.
//!
//! Ordinary stdin bytes are echoed. A record separator introduces one bounded
//! JSON capability request terminated by newline. Only this native process opens
//! its own capability socket; it never executes commands or loads workspace code.

#[path = "tool_fixture/channel.rs"]
mod channel;
#[path = "tool_fixture/recovery.rs"]
mod recovery;

use louiselm_skills::launch_protocol::{
    CommandOperation, MAX_PROTOCOL_MESSAGE_BYTES, ProtocolMessage, decode_message,
};
use std::{
    io::{self, BufRead, Read, Write},
    process::ExitCode,
};

fn run() -> io::Result<()> {
    let mut input = io::stdin().lock();
    let mut output = io::stdout().lock();
    let mut channel = None;
    let mut byte = [0];
    let mut counter = 0_u64;
    while input.read(&mut byte)? != 0 {
        if byte[0] == 0x1d {
            recovery::handle(&mut input, &mut output, &mut counter)?;
            continue;
        }
        if byte[0] != 0x1e {
            counter = counter
                .checked_add(1)
                .ok_or_else(|| io::Error::other("fixture counter exhausted"))?;
            output.write_all(&byte)?;
            output.flush()?;
            continue;
        }
        let mut bytes = Vec::new();
        (&mut input)
            .take((MAX_PROTOCOL_MESSAGE_BYTES + 1) as u64)
            .read_until(b'\n', &mut bytes)?;
        if bytes.len() > MAX_PROTOCOL_MESSAGE_BYTES {
            return Err(io::Error::other("oversized fixture request"));
        }
        let message = decode_message(&bytes).map_err(io::Error::other)?;
        if !matches!(&message, ProtocolMessage::ToolExecution(_))
            && !matches!(&message, ProtocolMessage::Command(message) if matches!(message.operation, CommandOperation::Delegate { .. }))
        {
            return Err(io::Error::other("unsupported fixture request"));
        }
        if channel.is_none() {
            channel = Some(channel::Channel::connect(std::path::Path::new(
                "/tmp/louiselm-capability.sock",
            ))?);
        }
        let connected = channel
            .as_ref()
            .ok_or_else(|| io::Error::other("missing fixture channel"))?;
        connected.send(&bytes)?;
        let reply = connected.receive()?;
        output.write_all(&[0x1e])?;
        output.write_all(&reply.canonical_bytes())?;
        output.write_all(b"\n")?;
        output.flush()?;
    }
    Ok(())
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            // This fixture constructs only normalized protocol/transport errors;
            // no command, output or workspace content is included.
            eprintln!("measured Agent fixture refused: {error}");
            ExitCode::FAILURE
        }
    }
}
