//! Command-line wiring for the capture service.

use std::{
    env,
    fs::{self, File, OpenOptions},
    io::Write,
    net::SocketAddr,
    path::{Path, PathBuf},
    sync::Arc,
    thread,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use qrcode::{
    QrCode,
    render::{svg, unicode},
};
use serde::Serialize;
use thiserror::Error;

use crate::{
    BeadsCleanup, BeadsGenerator, CaptureDraft, CaptureRecord, CaptureSource, CaptureState,
    GenerateRequest, GeneratedWorkReservation, GenerationError, IdentityError, NetworkProfile,
    NetworkProfileError, NetworkProfileKind, OpenAiTranscriber, PairingError, PairingRegistry,
    Receiver, ReserveResult, RunAdmission, RunDraft, RunSession, RunSocket, RunSocketError,
    RunStore, RunStoreError, Store, StoreError, TlsIdentity, Transcript, TranscriptionWorker,
};

const DEFAULT_MODEL: &str = "gpt-4o-transcribe";
const PAIRING_TTL_MS: u64 = 10 * 60 * 1_000;

/// CLI failure with actionable local context and no credentials.
#[derive(Debug, Error)]
pub enum CliError {
    /// Required argument or environment configuration is invalid.
    #[error("invalid command: {0}")]
    Invalid(String),
    /// Filesystem or socket operation failed.
    #[error("capture service I/O failed: {0}")]
    Io(#[from] std::io::Error),
    /// Capture store operation failed.
    #[error(transparent)]
    Store(#[from] StoreError),
    /// Durable Run-store operation failed.
    #[error(transparent)]
    Run(#[from] RunStoreError),
    /// Local Run observation socket failed.
    #[error(transparent)]
    RunSocket(#[from] RunSocketError),
    /// Generated-work broker failed.
    #[error(transparent)]
    Generation(#[from] GenerationError),
    /// Ambiguous generated work can be retried with the same safe mutation identity.
    #[error("{source}; retry with LOUISELM_MUTATION_ID={mutation_id}")]
    GenerationRetry {
        mutation_id: String,
        source: GenerationError,
    },
    /// Pairing operation failed.
    #[error(transparent)]
    Pairing(#[from] PairingError),
    /// TLS identity operation failed.
    #[error(transparent)]
    Identity(#[from] IdentityError),
    /// Network-profile validation or persistence failed.
    #[error(transparent)]
    Network(#[from] NetworkProfileError),
    /// JSON output failed.
    #[error("JSON output failed: {0}")]
    Json(#[from] serde_json::Error),
    /// QR encoding failed.
    #[error("pairing QR could not be created: {0}")]
    Qr(#[from] qrcode::types::QrError),
}

/// Parse process arguments and execute one capture-service command.
///
/// # Errors
///
/// Returns explicit command, configuration, storage, identity, and service errors.
pub async fn run() -> Result<(), CliError> {
    let arguments = env::args().skip(1).collect::<Vec<_>>();
    let Some(command) = arguments.first().map(String::as_str) else {
        print_help();
        return Ok(());
    };
    if command == "help" || command == "--help" || command == "-h" {
        print_help();
        return Ok(());
    }
    let options = &arguments[1..];
    let paths = Paths::discover()?;
    let store = Store::new(paths.captures())?;

    match command {
        "ingest-local" => ingest_local(&store, options),
        "list" => list(&store),
        "status" => status(&store, &paths),
        "retry" => retry(&store, options),
        "transcribe-once" => transcribe_once(&store),
        "run" => run_command(&paths, options),
        "configure-network" => configure_network(&paths, options),
        "pair" => pair(&paths, options),
        "revoke-device" => revoke_device(&paths, options),
        "serve" => serve(store, &paths, options).await,
        other => Err(CliError::Invalid(format!("unknown command '{other}'"))),
    }
}

fn run_command(paths: &Paths, arguments: &[String]) -> Result<(), CliError> {
    let Some((command, options)) = arguments.split_first() else {
        return Err(CliError::Invalid("run requires a subcommand".to_owned()));
    };
    if command == "list" {
        let runs = RunStore::new(paths.runs())?.list_resumable(now_ms())?;
        println!("{}", serde_json::to_string(&runs)?);
        return Ok(());
    }
    if command == "admit" {
        let token = uuid::Uuid::new_v4().to_string();
        let admission = RunAdmission {
            id: required_option(options, "--id")?.to_owned(),
            generated_work_ceiling: positive_integer(options, "--generated-work-max")?,
            park_ttl_ms: positive_integer(options, "--park-ttl-ms")?,
        };
        RunStore::new(paths.runs())?.admit(admission.clone(), &token)?;
        println!(
            "{}",
            serde_json::to_string(&serde_json::json!({
                "id": admission.id,
                "state": "active",
                "token": token,
                "generated_work": {
                    "ceiling": admission.generated_work_ceiling,
                    "consumed": 0,
                    "reserved": 0
                }
            }))?
        );
        return Ok(());
    }
    if command == "attach" {
        let session = RunSession {
            id: required_option(options, "--id")?.to_owned(),
            session_id: required_option(options, "--session-id")?.to_owned(),
            agent: required_option(options, "--agent")?.to_owned(),
            acp_session_id: required_option(options, "--acp-session-id")?.to_owned(),
            working_dir: required_option(options, "--cwd")?.to_owned(),
            load_session: required_option(options, "--load-session")? == "true",
        };
        RunStore::new(paths.runs())?.attach(session.clone())?;
        println!(
            "{}",
            serde_json::to_string(&serde_json::json!({"id": session.id, "state": "active"}))?
        );
        return Ok(());
    }
    if command == "generate" {
        return generate(paths, options);
    }
    if matches!(command.as_str(), "reserve" | "confirm" | "release") {
        return reservation(paths, command, options);
    }
    if command != "park" {
        return Err(CliError::Invalid(
            "run supports only admit, attach, confirm, generate, list, park, release, and reserve"
                .to_owned(),
        ));
    }
    let claims = required_option(options, "--claims")?
        .split(',')
        .filter(|claim| !claim.is_empty())
        .map(str::to_owned)
        .collect::<Vec<_>>();
    let draft = RunDraft {
        id: required_option(options, "--id")?.to_owned(),
        session_id: required_option(options, "--session-id")?.to_owned(),
        agent: required_option(options, "--agent")?.to_owned(),
        acp_session_id: required_option(options, "--acp-session-id")?.to_owned(),
        working_dir: required_option(options, "--cwd")?.to_owned(),
        load_session: required_option(options, "--load-session")? == "true",
        claimed_issue_ids: claims,
    };
    RunStore::new(paths.runs())?.park_cold(draft.clone(), now_ms())?;
    println!(
        "{}",
        serde_json::to_string(&serde_json::json!({"id": draft.id, "state": "cold_parked"}))?
    );
    Ok(())
}

/// Drive one generated-work reservation against the Run ledger.
///
/// The Run id and generate token arrive through the environment rather than as
/// arguments, matching `run generate`: a token in argv is readable from the
/// process table by every other user on the host. Every outcome the ledger can
/// legitimately report — including `exhausted` — succeeds and is named in the
/// JSON `state`, because exhaustion is a Park awaiting an operator decision and
/// not a failure of the command. Each response carries the resulting budget so
/// a caller never has to re-read the Run to learn what its request did.
fn reservation(paths: &Paths, command: &str, arguments: &[String]) -> Result<(), CliError> {
    let run_id = required_environment("LOUISELM_RUN_ID")?;
    let token = required_environment("LOUISELM_RUN_TOKEN")?;
    let mutation_id = required_option(arguments, "--mutation-id")?.to_owned();
    let store = RunStore::new(paths.runs())?;
    let state = match command {
        "reserve" => {
            let result = store.reserve_generated_work(
                GeneratedWorkReservation {
                    run_id: run_id.clone(),
                    token: token.clone(),
                    mutation_id,
                    kind: required_option(arguments, "--kind")?.to_owned(),
                    units: positive_integer(arguments, "--units")?,
                },
                now_ms(),
            )?;
            match result {
                ReserveResult::Reserved => "reserved",
                ReserveResult::Pending => "pending",
                ReserveResult::Consumed => "consumed",
                ReserveResult::Exhausted => "exhausted",
            }
        }
        "confirm" => {
            store.confirm_generated_work(
                &run_id,
                &token,
                &mutation_id,
                required_option(arguments, "--issue-id")?,
            )?;
            "confirmed"
        }
        _ => {
            store.release_generated_work(&run_id, &token, &mutation_id)?;
            "released"
        }
    };
    let budget = store.run(&run_id)?.generated_work;
    println!(
        "{}",
        serde_json::to_string(&serde_json::json!({
            "id": run_id,
            "state": state,
            "generated_work": {
                "ceiling": budget.ceiling,
                "consumed": budget.consumed,
                "reserved": budget.reserved
            }
        }))?
    );
    Ok(())
}

fn generate(paths: &Paths, arguments: &[String]) -> Result<(), CliError> {
    let separator = arguments
        .iter()
        .position(|argument| argument == "--")
        .ok_or_else(|| {
            CliError::Invalid("run generate requires '--' before br arguments".to_owned())
        })?;
    let options = &arguments[..separator];
    let mutation_id = env::var("LOUISELM_MUTATION_ID")
        .ok()
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    let request = GenerateRequest {
        run_id: required_environment("LOUISELM_RUN_ID")?,
        token: required_environment("LOUISELM_RUN_TOKEN")?,
        mutation_id: mutation_id.clone(),
        command: required_option(options, "--command")?.to_owned(),
        arguments: arguments[separator + 1..].to_vec(),
    };
    let generator = BeadsGenerator::new(
        required_environment("LOUISELM_REAL_BR")?,
        required_environment("BEADS_DB")?,
    );
    let command = request.command.clone();
    let issue = generator
        .generate(&RunStore::new(paths.runs())?, request, now_ms())
        .map_err(|source| match source {
            GenerationError::Ambiguous(_) => CliError::GenerationRetry {
                mutation_id,
                source,
            },
            source => CliError::Generation(source),
        })?;
    if command == "q" {
        println!("{}", issue.id);
    } else {
        print!("{}", issue.stdout);
        if !issue.stdout.ends_with('\n') {
            println!();
        }
    }
    Ok(())
}

fn required_environment(name: &str) -> Result<String, CliError> {
    env::var(name)
        .ok()
        .filter(|value| !value.is_empty())
        .ok_or_else(|| CliError::Invalid(format!("{name} must be set")))
}

fn ingest_local(store: &Store, arguments: &[String]) -> Result<(), CliError> {
    let path = PathBuf::from(required_option(arguments, "--file")?);
    let id = option(arguments, "--id")
        .map(str::to_owned)
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    let recorded_at_ms = positive_integer(arguments, "--recorded-at-ms")?;
    let duration_ms = positive_integer(arguments, "--duration-ms")?;
    let mime_type = required_option(arguments, "--mime")?.to_owned();
    let outcome = store.ingest(
        CaptureDraft {
            id: id.clone(),
            source: CaptureSource::Neovim,
            recorded_at_ms,
            duration_ms,
            mime_type,
        },
        File::open(path)?,
    )?;
    println!(
        "{}",
        serde_json::to_string(
            &serde_json::json!({"id": id, "outcome": format!("{outcome:?}").to_lowercase()})
        )?
    );
    Ok(())
}

fn list(store: &Store) -> Result<(), CliError> {
    let captures = store.list()?;
    let output = captures
        .into_iter()
        .map(|capture| {
            let transcript = match store.transcript(&capture.record.id) {
                Ok(transcript) => Some(transcript),
                Err(StoreError::NotFound(_)) => None,
                Err(error) => return Err(error),
            };
            Ok(CaptureStatus {
                record: capture.record,
                state: capture.state,
                audio_path: capture.audio_path,
                transcript,
            })
        })
        .collect::<Result<Vec<_>, StoreError>>()?;
    println!("{}", serde_json::to_string_pretty(&output)?);
    Ok(())
}

fn status(store: &Store, paths: &Paths) -> Result<(), CliError> {
    let captures = store.list()?;
    let pairing = PairingRegistry::open(paths.pairing())?;
    let pairing_status = pairing.status()?;
    let network = NetworkProfile::load_or_default(&paths.network())?;
    let phone_reachable = network.phone_reachable();
    let paired_device_count = pairing_status.devices.len();
    let delivery_state = if phone_reachable {
        "reachable"
    } else if paired_device_count == 0 {
        "unconfigured"
    } else {
        "degraded"
    };
    let delivery_warning = (delivery_state == "degraded")
        .then_some("paired devices cannot reach the loopback-only receiver");
    let mut pending = 0;
    let mut retrying = 0;
    let mut failed = 0;
    let mut completed = 0;
    for capture in &captures {
        match capture.state.transcription.status.as_str() {
            "pending" => pending += 1,
            "retrying" => retrying += 1,
            "failed" => failed += 1,
            "completed" => completed += 1,
            _ => {}
        }
    }
    println!(
        "{}",
        serde_json::to_string_pretty(&serde_json::json!({
            "captures": captures.len(),
            "transcription": {
                "pending": pending,
                "retrying": retrying,
                "failed": failed,
                "completed": completed,
            },
            "paired_device_count": paired_device_count,
            "devices": pairing_status.devices,
            "delivery": {
                "state": delivery_state,
                "warning": delivery_warning,
            },
            "network": {
                "profile": network.kind(),
                "bind": network.bind().to_string(),
                "receiver_url": network.receiver_url(),
                "phone_reachable": phone_reachable,
            },
        }))?
    );
    Ok(())
}

fn retry(store: &Store, arguments: &[String]) -> Result<(), CliError> {
    let id = positional(arguments, 0, "capture UUID")?;
    store.retry_transcription(id)?;
    println!("{id}");
    Ok(())
}

fn transcribe_once(store: &Store) -> Result<(), CliError> {
    let provider = openai_provider()?;
    let processed = TranscriptionWorker::new(store, &provider).process_ready(now_ms())?;
    println!("{processed}");
    Ok(())
}

fn configure_network(paths: &Paths, arguments: &[String]) -> Result<(), CliError> {
    let profile = required_option(arguments, "--profile")?.parse::<NetworkProfileKind>()?;
    let bind = required_option(arguments, "--bind")?
        .parse::<SocketAddr>()
        .map_err(|_| CliError::Invalid("--bind must be an explicit IP:port".to_owned()))?;
    let network = NetworkProfile::new(profile, bind, required_option(arguments, "--url")?)?;
    network.save(&paths.network())?;
    println!("{}", serde_json::to_string_pretty(&network)?);
    Ok(())
}

fn pair(paths: &Paths, arguments: &[String]) -> Result<(), CliError> {
    let svg_path = match arguments {
        [] => None,
        [flag, path] if flag == "--svg" && !path.is_empty() => Some(PathBuf::from(path)),
        _ => {
            return Err(CliError::Invalid(
                "pair accepts either no arguments or --svg PATH".to_owned(),
            ));
        }
    };
    let network = NetworkProfile::load_or_default(&paths.network())?;
    let receiver_url = network.receiver_url().ok_or_else(|| {
        CliError::Invalid(
            "phone pairing requires configure-network with one private receiver profile".to_owned(),
        )
    })?;
    let identity = TlsIdentity::load_or_create(paths.tls())?;
    let registry = PairingRegistry::open(paths.pairing())?;
    let offer = registry.issue(
        receiver_url,
        identity.public_key_sha256(),
        now_ms(),
        PAIRING_TTL_MS,
    )?;
    let payload = serde_json::to_string(&offer)?;
    let code = QrCode::new(payload.as_bytes())?;
    if let Some(path) = svg_path {
        let rendered = code
            .render::<svg::Color>()
            .quiet_zone(true)
            .min_dimensions(1024, 1024)
            .build();
        write_pairing_svg(&path, rendered.as_bytes())?;
        println!("{}", path.display());
        return Ok(());
    }
    let rendered = code.render::<unicode::Dense1x2>().quiet_zone(true).build();
    println!("{rendered}");
    Ok(())
}

fn write_pairing_svg(path: &Path, contents: &[u8]) -> Result<(), std::io::Error> {
    let mut options = OpenOptions::new();
    options.create_new(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;

        options.mode(0o600);
    }
    let mut file = options.open(path)?;
    let result = (|| {
        file.write_all(contents)?;
        file.sync_all()
    })();
    drop(file);
    if result.is_err() {
        let _ = fs::remove_file(path);
    }
    result
}

fn revoke_device(paths: &Paths, arguments: &[String]) -> Result<(), CliError> {
    let id = positional(arguments, 0, "device UUID")?;
    PairingRegistry::open(paths.pairing())?.revoke(id)?;
    println!("{id}");
    Ok(())
}

async fn serve(store: Store, paths: &Paths, arguments: &[String]) -> Result<(), CliError> {
    no_arguments(arguments, "serve")?;
    let bind = NetworkProfile::load_or_default(&paths.network())?.bind();
    let identity = TlsIdentity::load_or_create(paths.tls())?;
    let pairing = Arc::new(PairingRegistry::open(paths.pairing())?);
    let receiver = Receiver::new(
        store.clone(),
        pairing,
        paths.uploads(),
        identity.public_key_sha256(),
    )?;
    let run_socket = RunSocket::bind(
        paths.run_socket(),
        paths.operator_capability(),
        RunStore::new(paths.runs())?,
    )
    .await?;
    if let Some(workspace) =
        env::var_os("LOUISELM_BEADS_WORKSPACE").filter(|value| !value.is_empty())
    {
        let runs = RunStore::new(paths.runs())?;
        // A bare "br" would resolve against this process's own PATH, which a long-running
        // daemon's environment (e.g. a systemd unit's minimal default) is not guaranteed to
        // contain; require an explicit path rather than fail silently (louiselm-hvot).
        let br_executable = required_environment("LOUISELM_REAL_BR")?;
        let cleanup = BeadsCleanup::new(PathBuf::from(workspace), br_executable)?;
        thread::spawn(move || {
            loop {
                match runs.reap_expired(now_ms(), |action| cleanup.release(action)) {
                    Ok(summary) if summary.disposed > 0 || !summary.failed.is_empty() => {
                        eprintln!(
                            "louiselm-capture: reap pass disposed {} Run(s), {} release(s) still failing: {:?}",
                            summary.disposed,
                            summary.failed.len(),
                            summary.failed
                        );
                    }
                    Ok(_) => {}
                    Err(error) => eprintln!("louiselm-capture: reap pass failed: {error}"),
                }
                thread::sleep(Duration::from_secs(10));
            }
        });
    }
    if let Ok(provider) = openai_provider() {
        thread::spawn(move || {
            loop {
                let _ = TranscriptionWorker::new(&store, &provider).process_ready(now_ms());
                thread::sleep(Duration::from_secs(10));
            }
        });
    }
    let tls = axum_server::tls_rustls::RustlsConfig::from_pem_file(
        identity.certificate_path(),
        identity.private_key_path(),
    )
    .await?;
    let network_server =
        axum_server::bind_rustls(bind, tls).serve(receiver.router().into_make_service());
    tokio::select! {
        result = run_socket.serve() => result?,
        result = network_server => result?,
    }
    Ok(())
}

fn openai_provider() -> Result<OpenAiTranscriber, CliError> {
    let api_key = env::var("OPENAI_API_KEY").map_err(|_| {
        CliError::Invalid("OPENAI_API_KEY is required for transcription".to_owned())
    })?;
    let model =
        env::var("LOUISELM_TRANSCRIPTION_MODEL").unwrap_or_else(|_| DEFAULT_MODEL.to_owned());
    OpenAiTranscriber::new(api_key, model).map_err(CliError::Invalid)
}

fn required_option<'a>(arguments: &'a [String], name: &str) -> Result<&'a str, CliError> {
    option(arguments, name)
        .ok_or_else(|| CliError::Invalid(format!("required option is missing: {name}")))
}

fn option<'a>(arguments: &'a [String], name: &str) -> Option<&'a str> {
    arguments
        .windows(2)
        .find(|pair| pair[0] == name)
        .map(|pair| pair[1].as_str())
}

fn positive_integer(arguments: &[String], name: &str) -> Result<u64, CliError> {
    let value = required_option(arguments, name)?
        .parse::<u64>()
        .map_err(|_| CliError::Invalid(format!("{name} must be a positive integer")))?;
    if value == 0 {
        return Err(CliError::Invalid(format!(
            "{name} must be a positive integer"
        )));
    }
    Ok(value)
}

fn positional<'a>(arguments: &'a [String], index: usize, label: &str) -> Result<&'a str, CliError> {
    arguments
        .get(index)
        .map(String::as_str)
        .filter(|value| !value.starts_with('-'))
        .ok_or_else(|| CliError::Invalid(format!("{label} is required")))
}

fn no_arguments(arguments: &[String], command: &str) -> Result<(), CliError> {
    if arguments.is_empty() {
        return Ok(());
    }
    Err(CliError::Invalid(format!(
        "{command} takes no arguments; use configure-network"
    )))
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_millis() as u64)
}

#[derive(Serialize)]
struct CaptureStatus {
    record: CaptureRecord,
    state: CaptureState,
    audio_path: PathBuf,
    transcript: Option<Transcript>,
}

struct Paths {
    config: PathBuf,
    data: PathBuf,
    state: PathBuf,
}

impl Paths {
    fn discover() -> Result<Self, CliError> {
        Ok(Self {
            config: configured_root("LOUISELM_CAPTURE_CONFIG_DIR", "XDG_CONFIG_HOME", ".config")?,
            data: configured_root("LOUISELM_CAPTURE_DATA_DIR", "XDG_DATA_HOME", ".local/share")?,
            state: configured_root(
                "LOUISELM_CAPTURE_STATE_DIR",
                "XDG_STATE_HOME",
                ".local/state",
            )?,
        })
    }

    fn captures(&self) -> PathBuf {
        self.data.join("louiselm/captures")
    }

    fn network(&self) -> PathBuf {
        self.config.join("louiselm/capture-network.json")
    }

    fn pairing(&self) -> PathBuf {
        self.state.join("louiselm/capture/pairing")
    }

    fn tls(&self) -> PathBuf {
        self.state.join("louiselm/capture/tls")
    }

    fn uploads(&self) -> PathBuf {
        self.state.join("louiselm/capture/uploads")
    }

    fn runs(&self) -> PathBuf {
        self.state.join("louiselm/workflow/runs")
    }

    fn run_socket(&self) -> PathBuf {
        self.state.join("louiselm/workflow/run.sock")
    }

    fn operator_capability(&self) -> PathBuf {
        self.state.join("louiselm/workflow/operator-capability")
    }
}

fn configured_root(
    override_name: &str,
    xdg_name: &str,
    fallback: &str,
) -> Result<PathBuf, CliError> {
    if let Some(value) = env::var_os(override_name).filter(|value| !value.is_empty()) {
        return Ok(PathBuf::from(value));
    }
    if let Some(value) = env::var_os(xdg_name).filter(|value| !value.is_empty()) {
        return Ok(PathBuf::from(value));
    }
    let home = env::var_os("HOME")
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            CliError::Invalid(format!("{override_name}, {xdg_name}, or HOME is required"))
        })?;
    Ok(Path::new(&home).join(fallback))
}

fn print_help() {
    println!(
        "louiselm-capture commands:\n  configure-network --profile lan|overlay|private --bind IP:PORT --url HTTPS_URL\n  serve\n  run admit --id UUID --generated-work-max N --park-ttl-ms N\n  run attach --id UUID --session-id ID --agent NAME --acp-session-id ID --cwd PATH --load-session true|false\n  run generate --command create|q -- BR_ARGS\n  run list\n  run park --id UUID --session-id ID --agent NAME --acp-session-id ID --cwd PATH --load-session true --claims ISSUE_IDS\n  pair [--svg PATH]\n  revoke-device DEVICE_UUID\n  ingest-local --file PATH --recorded-at-ms N --duration-ms N --mime TYPE [--id UUID]\n  list\n  status\n  retry CAPTURE_UUID\n  transcribe-once"
    );
}
