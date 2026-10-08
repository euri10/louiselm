//! Socket-activated, dedicated-identity Control broker process.

use std::{
    ffi::OsStr,
    fs::File,
    os::fd::{AsFd, OwnedFd},
    path::Path,
    process::ExitCode,
    sync::Arc,
    thread::{self, JoinHandle},
};

use louiselm_skills::{
    broker::{BrokerError, InstalledBroker},
    launch_transport::{SeqpacketListener, TransportError},
    launcher_install::LauncherPaths,
    release,
};

#[path = "control/arguments.rs"]
mod arguments;
#[path = "control/attention.rs"]
mod attention;
#[path = "control/beads.rs"]
mod beads;
#[path = "control/dependencies.rs"]
mod dependencies;
#[path = "control/inspection.rs"]
mod inspection;
#[path = "control/launch_inputs.rs"]
mod launch_inputs;
#[path = "control/promotion.rs"]
mod promotion;
#[path = "control/provider_extension.rs"]
mod provider_extension;
#[path = "control/run_authorization.rs"]
mod run_authorization;
#[path = "control/verification.rs"]
mod verification;
#[path = "control/waiver.rs"]
mod waiver;

fn main() -> ExitCode {
    let args: Vec<_> = std::env::args_os().collect();
    let parsed = match arguments::command().try_get_matches_from(&args) {
        Ok(matches) => Some(matches),
        Err(error) if error.kind() == clap::error::ErrorKind::DisplayHelp => {
            return if error.print().is_ok() {
                ExitCode::SUCCESS
            } else {
                ExitCode::FAILURE
            };
        }
        Err(_) => None,
    };
    // Syntax refusals retain the selected operation's machine error schema.
    // The raw argument is used only to select a static handler, never displayed.
    let (verb, input) = parsed
        .as_ref()
        .and_then(clap::ArgMatches::subcommand)
        .map_or_else(
            || {
                (
                    args.get(1).and_then(|arg| arg.to_str()).unwrap_or(""),
                    Err(()),
                )
            },
            |(verb, matches)| (verb, Ok(matches)),
        );
    match verb {
        "waiver" => return ExitCode::from(waiver::cli(input)),
        "provider-extend" => return ExitCode::from(provider_extension::cli(input)),
        "dependencies" => return ExitCode::from(dependencies::cli(input)),
        "session" => return ExitCode::from(inspection::cli(input)),
        "beads" => return ExitCode::from(beads::cli(input)),
        "run" => return ExitCode::from(run_authorization::cli(input)),
        "launch-inputs" => return ExitCode::from(launch_inputs::cli(input)),
        "verification" => return ExitCode::from(verification::cli(input)),
        "promotion" => return ExitCode::from(promotion::cli(input)),
        "skill-request" => return ExitCode::from(inspection::skill_cli(input)),
        _ => (),
    }
    let result = match verb {
        "serve" if input.is_ok() => start(),
        "adopt-state" if input.is_ok() => adopt_state(),
        _ => Err("expected 'serve', 'adopt-state --confirm', 'run authorize --json', 'launch-inputs stage --json', 'session inspect|conformance ID --json', 'beads inspect OPERATION_UUID --json', 'skill-request inspect|reject|cancel ID --json', 'dependencies inspect|approve SESSION [CANDIDATE...] --json', 'waiver inspect|plan|apply|result|revoke SESSION [DIGEST] --json', or 'provider-extend SESSION REQUEST_ID REQUESTS [EXPIRES_AT_MS] --json'".to_owned()),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("louiselm-control: {message}");
            ExitCode::FAILURE
        }
    }
}

fn start() -> Result<(), String> {
    // Take the listener off stdin before opening files, starting a thread or
    // spawning a child, so nothing else can observe or inherit fd 0.
    let fd = activated_listener()?;
    // Verify it before the installed-authority checks, so a manual start names
    // the missing socket rather than an unrelated authority refusal.
    let listener = SeqpacketListener::adopt(fd).map_err(|_| {
        "serve requires a listening Unix SOCK_SEQPACKET socket on standard input (StandardInput=socket)"
            .to_owned()
    })?;
    let paths = installed_paths()?;
    let broker = InstalledBroker::over(&paths, Path::new("/var/lib/louiselm/broker"), listener)
        .map_err(|error| error.to_string())?;
    serve(broker).map_err(|error| error.to_string())
}

fn adopt_state() -> Result<(), String> {
    // sudo authenticates the invoking user before switching to the dedicated
    // broker account. Only that account can access the mode-0700 state. A
    // same-broker process already owns that state; this is not a boundary
    // against a compromised broker or root forging the environment/marker.
    let raw =
        std::env::var("SUDO_UID").map_err(|_| "sudo operator identity required".to_owned())?;
    let uid = raw
        .parse::<u32>()
        .map_err(|_| "sudo operator identity required".to_owned())?;
    if uid == 0 || uid.to_string() != raw {
        return Err("sudo operator identity required".to_owned());
    }
    let paths = installed_paths()?;
    let changed = InstalledBroker::adopt_state(&paths, Path::new("/var/lib/louiselm/broker"), uid)
        .map_err(|error| error.to_string())?;
    println!(
        "{}",
        if changed {
            "broker state identity adopted"
        } else {
            "broker state identity already matches; unchanged"
        }
    );
    Ok(())
}

fn installed_paths() -> Result<LauncherPaths, String> {
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
    let paths = LauncherPaths::system();
    let config = louiselm_skills::launcher_install::public_runtime_config(&paths)
        .map_err(|_| "installed broker authority is unavailable".to_owned())?;
    if running.release_id.as_deref() != Some(config.release_id.as_str()) {
        return Err("running broker release does not match installed authority".into());
    }
    Ok(paths)
}

/// Takes the socket-activated listener that systemd places on standard input.
///
/// The service unit's `StandardInput=socket` hands the manager-held listener
/// over as fd 0, without `LISTEN_FDS`. A close-on-exec duplicate becomes the
/// broker's owned listener; fd 0 is then pointed at `/dev/null`, so no child
/// process can inherit the listener as its standard input.
fn activated_listener() -> Result<OwnedFd, String> {
    let fd = std::io::stdin().as_fd().try_clone_to_owned().map_err(|_| {
        "serve requires the broker socket on standard input (StandardInput=socket)".to_owned()
    })?;
    File::open("/dev/null")
        .and_then(|null| rustix::stdio::dup2_stdin(&null).map_err(std::io::Error::from))
        .map_err(|_| "serve cannot detach the broker socket from standard input".to_owned())?;
    Ok(fd)
}

fn serve(broker: InstalledBroker) -> Result<(), BrokerError> {
    let broker = Arc::new(broker);
    let queries = inspection::Queries::start(Arc::clone(&broker))?;
    let _attention_worker = attention::start(Arc::clone(&broker))?;
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
        let queries = Arc::clone(&queries);
        workers.push(
            thread::Builder::new()
                .name("louiselm-broker-session".into())
                .spawn(move || {
                    let mut session = owner.serve_accepted(channel)?;
                    queries.run_session(&owner, &mut session)
                })
                .map_err(|_| BrokerError::Transport(TransportError::WorkerUnavailable))?,
        );
    }
}
