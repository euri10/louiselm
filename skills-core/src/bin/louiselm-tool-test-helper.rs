//! Fixed measured helper: forwards bounded stdin work through its own capability socket.
//! It never executes commands, loads configuration or receives the Agent channel.

#[path = "tool_fixture/channel.rs"]
mod channel;

use louiselm_skills::launch_protocol::{
    CommandMessage, CommandOperation, MAX_PROTOCOL_MESSAGE_BYTES,
};
use std::{
    io::{self, Read},
    path::Path,
    process::ExitCode,
};

fn run() -> Result<(), ()> {
    let mut bytes = Vec::new();
    io::stdin()
        .take((MAX_PROTOCOL_MESSAGE_BYTES + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|_| ())?;
    if bytes.len() > MAX_PROTOCOL_MESSAGE_BYTES {
        return Err(());
    }
    let message: CommandMessage = serde_json::from_slice(&bytes).map_err(|_| ())?;
    message.validate().map_err(|_| ())?;
    let CommandOperation::Delegate { grant, mut command } = message.operation else {
        return Err(());
    };
    let channel =
        channel::Channel::connect(Path::new("/tmp/louiselm-tool.sock")).map_err(|_| ())?;
    // This deterministic helper repeats finite grants and performs one initial
    // command for an uncapped grant; workload size is not an authorization quota.
    for sequence in 1..=u64::from(grant.uses.unwrap_or(1)) {
        command.sequence = sequence;
        let probe_child = command.request_id == "probe-child";
        command.request_id = format!("tool-{}-{sequence}", grant.sequence);
        if probe_child {
            // Deliberately transfer the connected descriptor to a new process.
            // The same executable and ancestry still confer no grant authority.
            let status = std::process::Command::new(std::env::current_exe().map_err(|_| ())?)
                .arg("--probe-child")
                .arg(serde_json::to_string(&command).map_err(|_| ())?)
                .stdin(std::process::Stdio::from(
                    rustix::io::dup(&channel.0).map_err(|_| ())?,
                ))
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status()
                .map_err(|_| ())?;
            return if status.success() { Ok(()) } else { Err(()) };
        }
        channel.send(&command.canonical_bytes()).map_err(|_| ())?;
        let reply = channel.receive().map_err(|_| ())?;
        if reply.request_id != command.request_id
            || !matches!(reply.operation, CommandOperation::Result { .. })
        {
            return Err(());
        }
    }
    Ok(())
}

fn main() -> ExitCode {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    if arguments.len() == 2 && arguments[0] == "--probe-child" {
        let denied = (|| {
            let request: louiselm_skills::launch_protocol::ToolExecutionRequest =
                serde_json::from_str(&arguments[1]).map_err(|_| ())?;
            request.validate().map_err(|_| ())?;
            let channel = channel::Channel(rustix::io::dup(std::io::stdin()).map_err(|_| ())?);
            channel.send(&request.canonical_bytes()).map_err(|_| ())?;
            Ok::<_, ()>(channel.receive().is_err())
        })();
        return if denied == Ok(true) {
            ExitCode::SUCCESS
        } else {
            ExitCode::FAILURE
        };
    }
    if !arguments.is_empty() {
        return ExitCode::FAILURE;
    }
    if run().is_ok() {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}
