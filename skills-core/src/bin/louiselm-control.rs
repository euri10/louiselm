//! Socket-activated, dedicated-identity Control broker process.

use std::{
    ffi::OsStr,
    os::fd::{FromRawFd, OwnedFd},
    path::Path,
    process::ExitCode,
    sync::Arc,
    thread::{self, JoinHandle},
};

use louiselm_skills::{
    broker::{BrokerError, InstalledBroker},
    launch_transport::{SeqpacketListener, TransportError, inherited_descriptor},
    launcher_install::LauncherPaths,
    release,
};

fn main() -> ExitCode {
    let mut arguments = std::env::args_os().skip(1);
    if arguments.next().as_deref() != Some(OsStr::new("serve")) || arguments.next().is_some() {
        eprintln!("louiselm-control: expected exactly 'serve'");
        return ExitCode::FAILURE;
    }
    match start() {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("louiselm-control: {message}");
            ExitCode::FAILURE
        }
    }
}

fn start() -> Result<(), String> {
    // Acquire inherited ownership before opening files or starting any thread.
    let fd = activated_descriptor().map_err(|_| {
        "socket activation requires exactly one listening descriptor for this process".to_owned()
    })?;
    let running = release::running_identity();
    if !running.verified
        || running
            .executable
            .as_deref()
            .and_then(|path| Path::new(path).file_name())
            != Some(OsStr::new("louiselm-control"))
    {
        return Err("running broker release is untrusted".into());
    }
    let listener = SeqpacketListener::adopt(fd).map_err(|_| {
        "socket activation requires a listening Unix SOCK_SEQPACKET socket".to_owned()
    })?;
    let paths = LauncherPaths::system();
    let config = louiselm_skills::launcher_install::public_runtime_config(&paths)
        .map_err(|_| "installed broker authority is unavailable".to_owned())?;
    if running.release_id.as_deref() != Some(config.release_id.as_str()) {
        return Err("running broker release does not match installed authority".into());
    }
    let broker = InstalledBroker::over(&paths, Path::new("/var/lib/louiselm/broker"), listener)
        .map_err(|error| error.to_string())?;
    serve(broker).map_err(|error| error.to_string())
}

#[expect(
    unsafe_code,
    reason = "Reviewed single-threaded process-entry ownership of systemd fd3; louiselm-96pv.3."
)]
fn activated_descriptor() -> Result<OwnedFd, TransportError> {
    let raw = inherited_descriptor(
        std::env::var("LISTEN_PID").ok().as_deref(),
        std::env::var("LISTEN_FDS").ok().as_deref(),
        std::process::id(),
    )?;
    std::fs::metadata(format!("/proc/self/fd/{raw}"))
        .map_err(|_| TransportError::InheritedDescriptor)?;
    // SAFETY: The procfs stat verified that fd3 is open. This fresh process has
    // opened no descriptors and spawned no threads; no Rust owner or concurrent
    // closer exists for the inherited fd. Take its sole ownership exactly once.
    Ok(unsafe { OwnedFd::from_raw_fd(raw) })
}

fn serve(broker: InstalledBroker) -> Result<(), BrokerError> {
    let broker = Arc::new(broker);
    let mut workers: Vec<JoinHandle<Result<(), BrokerError>>> = Vec::new();
    // SIGTERM deliberately keeps its native terminating action. Kernel process
    // teardown closes listener and Session descriptors, even during blocked I/O.
    // No drain, cleanup claim or synthetic receipt precedes exit; supervisors
    // observe Broker loss and use the existing durable reconnect protocol.
    loop {
        let channel = match broker.accept_connection() {
            Ok(channel) => channel,
            Err(BrokerError::Transport(TransportError::PeerCredentialsMismatch { .. })) => continue,
            Err(error) => return Err(error),
        };
        let mut index = 0;
        while index < workers.len() {
            if workers[index].is_finished() {
                match workers.swap_remove(index).join() {
                    Ok(Ok(())) => {}
                    Ok(Err(error)) => eprintln!("louiselm-control: Session worker: {error}"),
                    Err(_) => eprintln!("louiselm-control: Session worker failed"),
                }
            } else {
                index += 1;
            }
        }
        let owner = Arc::clone(&broker);
        workers.push(
            thread::Builder::new()
                .name("louiselm-broker-session".into())
                .spawn(move || {
                    let mut session = owner.serve_accepted(channel)?;
                    while !owner.step(&mut session)? {}
                    Ok(())
                })
                .map_err(|_| BrokerError::Transport(TransportError::WorkerUnavailable))?,
        );
    }
}
