use std::{
    ffi::OsStr,
    io::{self, BufReader},
    path::Path,
    process::ExitCode,
    sync::{Arc, mpsc},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use louiselm_skills::{
    launch_supervisor::{
        InstalledLaunchSigner, LaunchSigner, LaunchSupervisor, SYSTEM_REGISTRY_ROOT,
        SYSTEM_SESSIONS_ROOT, SystemLaunchPlatform, connect_control_broker, read_launch_frame,
    },
    launcher_install::{LauncherPaths, runtime_config_with_deadline},
    registry::Registry,
    release,
};

const BROKER_TIMEOUT: Duration = Duration::from_secs(5);

fn main() -> ExitCode {
    let mut arguments = std::env::args_os().skip(1);
    if arguments.next().as_deref() != Some(OsStr::new("run")) || arguments.next().is_some() {
        eprintln!("louiselm-launch: expected exactly 'run'");
        return ExitCode::FAILURE;
    }
    match run() {
        Ok(code) if (0..=255).contains(&code) => ExitCode::from(code as u8),
        Ok(_) => ExitCode::FAILURE,
        Err(message) => {
            eprintln!("louiselm-launch: {message}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<i32, &'static str> {
    if !rustix::process::geteuid().is_root() {
        return Err("root launcher authority required");
    }
    let running = release::running_identity();
    if !running.verified
        || running
            .executable
            .as_deref()
            .and_then(|path| Path::new(path).file_name())
            != Some(OsStr::new("louiselm-launch"))
    {
        return Err("running launcher release is untrusted");
    }

    let paths = LauncherPaths::system();
    let authority_deadline = std::time::Instant::now()
        .checked_add(BROKER_TIMEOUT)
        .unwrap_or_else(std::time::Instant::now);
    let config = runtime_config_with_deadline(&paths, authority_deadline)
        .map_err(|_| "launcher authority is invalid")?;
    if running.release_id.as_deref() != Some(config.release_id.as_str()) {
        return Err("running launcher release does not match installed authority");
    }
    let sudo_uid =
        canonical_sudo_uid().ok_or("launcher invocation is not the installed operator")?;
    if sudo_uid != config.operator_uid {
        return Err("launcher invocation is not the installed operator");
    }

    let mut input = BufReader::new(io::stdin());
    let request = read_launch_frame(&mut input).map_err(|_| "launch document rejected")?;
    let registry = Arc::new(
        Registry::open_trusted(Path::new(SYSTEM_REGISTRY_ROOT))
            .map_err(|_| "launch registry is untrusted")?,
    );
    let signer = Arc::new(
        InstalledLaunchSigner::open(&paths, BROKER_TIMEOUT)
            .map_err(|_| "launcher signing authority rejected")?,
    );
    if signer.release_id() != config.release_id {
        return Err("launcher signing release does not match installed authority");
    }
    let platform = Arc::new(
        SystemLaunchPlatform::new(paths, config.clone(), BROKER_TIMEOUT)
            .map_err(|_| "launcher platform authority rejected")?,
    );
    let broker = connect_control_broker(&config, BROKER_TIMEOUT)
        .map_err(|_| "Control broker unavailable")?;
    let supervisor = LaunchSupervisor::new(
        broker,
        signer,
        platform,
        registry,
        Path::new(SYSTEM_SESSIONS_ROOT).to_owned(),
        BROKER_TIMEOUT,
    );
    let now_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| "system clock unavailable")?
        .as_millis()
        .try_into()
        .map_err(|_| "system clock unavailable")?;
    let (sender, receiver) = mpsc::sync_channel(1);
    supervisor
        .launch(
            request,
            config.operator_uid,
            now_ms,
            Box::new(move |result| {
                let _ = sender.try_send(result);
            }),
        )
        .map_err(|_| "launch supervisor unavailable")?;
    let session = receiver
        .recv()
        .map_err(|_| "launch supervisor unavailable")?
        .map_err(|_| "authorized launch failed")?;
    session
        .relay_stdio(Box::new(input), Box::new(io::stdout()))
        .map_err(|_| "Agent relay failed")
}

fn canonical_sudo_uid() -> Option<u32> {
    let raw = std::env::var("SUDO_UID").ok()?;
    let parsed = raw.parse::<u32>().ok()?;
    (parsed != 0 && parsed.to_string() == raw).then_some(parsed)
}
