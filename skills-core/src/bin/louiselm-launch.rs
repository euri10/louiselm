//! Privileged Session launcher command-line entrypoint.

use std::{
    ffi::OsStr,
    fs::File,
    io::{self, BufReader},
    os::fd::AsFd,
    path::Path,
    process::ExitCode,
    sync::{Arc, mpsc},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use louiselm_skills::{
    launch_supervisor::{
        InstalledLaunchSigner, LaunchSigner, LaunchSupervisor, RelayStdio, SYSTEM_REGISTRY_ROOT,
        SYSTEM_SESSIONS_ROOT, SystemLaunchPlatform, connect_control_broker, read_launch_frame,
    },
    launcher_install::{LauncherConfig, LauncherPaths, runtime_config_with_deadline},
    registry::Registry,
    release,
    sandbox::bootstrap,
};

const BROKER_TIMEOUT: Duration = Duration::from_secs(5);

fn main() -> ExitCode {
    let mut arguments = std::env::args_os().skip(1);
    let verb = arguments.next();
    // Internal bootstrap/probe verbs use inherited authority only. Sudoers
    // grants exactly run/prepare/certify, never these internal worker verbs.
    if verb.as_deref() == Some(OsStr::new(bootstrap::ARGUMENT)) {
        if bootstrap::run(&arguments.collect::<Vec<_>>()).is_ok() {
            return ExitCode::SUCCESS;
        }
        eprintln!("louiselm-launch: sandbox bootstrap failed");
        return ExitCode::FAILURE;
    }
    if arguments.next().is_some() {
        eprintln!("louiselm-launch: unexpected arguments");
        return ExitCode::FAILURE;
    }
    let result = match verb.as_deref() {
        Some(value) if value == OsStr::new("run") => run(),
        Some(value) if value == OsStr::new("prepare") => preparation(),
        Some(value) if value == OsStr::new("certify") => certification(false),
        Some(value) if value == OsStr::new("cleanup") => cleanup(),
        Some(value) if value == OsStr::new("__conformance-worker") => certification(true),
        Some(value) if value == OsStr::new("__conformance-probe") => {
            louiselm_skills::conformance::installed::serve_probe()
                .map(|()| 0)
                .map_err(|_| "probe failed")
        }
        _ => Err("expected exactly 'run', 'prepare', 'certify' or root-only 'cleanup'"),
    };
    match result {
        Ok(code) => u8::try_from(code).map_or(ExitCode::FAILURE, ExitCode::from),
        Err(message) => {
            eprintln!("louiselm-launch: {message}");
            ExitCode::FAILURE
        }
    }
}

fn cleanup() -> Result<i32, &'static str> {
    use std::io::Write;
    let (_, config) = authority(false)?;
    let now_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|elapsed| elapsed.as_millis().try_into().ok())
        .ok_or("system clock unavailable")?;
    let report = louiselm_skills::workspace::retention::cleanup_expired(
        (config.broker_uid, config.broker_gid),
        now_ms,
    )
    .map_err(|_| "workspace cleanup refused; retained storage requires inspection")?;
    let bytes = serde_json::to_vec(&report).map_err(|_| "cleanup report unavailable")?;
    io::stdout()
        .lock()
        .write_all(&bytes)
        .map_err(|_| "cleanup output unavailable")?;
    Ok(i32::from(report.failed != 0))
}

fn authority(operator_required: bool) -> Result<(LauncherPaths, LauncherConfig), &'static str> {
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
    if operator_required {
        let sudo_uid =
            canonical_sudo_uid().ok_or("launcher invocation is not the installed operator")?;
        if sudo_uid != config.operator_uid {
            return Err("launcher invocation is not the installed operator");
        }
    }
    Ok((paths, config))
}

fn certification(worker: bool) -> Result<i32, &'static str> {
    use louiselm_skills::conformance::{ReportResult, installed};
    use std::io::{Read, Write};
    let (paths, config) = authority(!worker)?;
    let deadline = std::time::Instant::now() + Duration::from_mins(3);
    let certificate = if worker {
        let mut input = String::new();
        io::stdin().take(32).read_to_string(&mut input).map_err(|_| "certifier owner unavailable")?;
        let parent: u32 = input.parse().map_err(|_| "certifier owner invalid")?;
        if input != parent.to_string() { return Err("certifier owner invalid"); }
        installed::certify(&paths, deadline, parent)
    } else {
        installed::certify_isolated(&paths, &config, deadline)
    }.map_err(|_| "certification unavailable; inspect protected conformance state and required host profile")?;
    let bytes = certificate
        .canonical_bytes()
        .map_err(|_| "invalid certification evidence")?;
    io::stdout()
        .lock()
        .write_all(&bytes)
        .map_err(|_| "certification output unavailable")?;
    Ok(i32::from(
        certificate.observations.result() != Ok(ReportResult::Passed),
    ))
}

fn run() -> Result<i32, &'static str> {
    let (paths, config) = authority(true)?;

    let input = io::stdin()
        .as_fd()
        .try_clone_to_owned()
        .map_err(|_| "controller stdin unavailable")?;
    let output = io::stdout()
        .as_fd()
        .try_clone_to_owned()
        .map_err(|_| "controller stdout unavailable")?;
    let mut input = BufReader::new(File::from(input));
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
        .relay_stdio(
            RelayStdio::new(input, File::from(output))
                .map_err(|_| "controller stdio unavailable")?,
        )
        .map_err(|_| "Agent relay failed")
}

fn preparation() -> Result<i32, &'static str> {
    use std::io::Write;
    let (paths, config) = authority(true)?;
    let request =
        read_launch_frame(&mut io::stdin().lock()).map_err(|_| "launch document rejected")?;
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|time| u64::try_from(time.as_millis()).ok())
        .ok_or("system clock unavailable")?;
    let prepared = louiselm_skills::conformance::preparation::prepare(
        &paths,
        &config,
        &request,
        now,
        std::time::Instant::now() + BROKER_TIMEOUT,
    )
    .map_err(
        |_| "conformance preparation refused; restore host evidence or inspect launcher policy",
    )?;
    let bytes = serde_json::to_vec(&prepared).map_err(|_| "preparation unavailable")?;
    io::stdout()
        .lock()
        .write_all(&bytes)
        .map_err(|_| "preparation output unavailable")?;
    Ok(0)
}

fn canonical_sudo_uid() -> Option<u32> {
    let raw = std::env::var("SUDO_UID").ok()?;
    let parsed = raw.parse::<u32>().ok()?;
    (parsed != 0 && parsed.to_string() == raw).then_some(parsed)
}
