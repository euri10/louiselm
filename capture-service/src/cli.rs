//! Command-line wiring for the capture service.

use std::{
    env,
    fs::File,
    net::SocketAddr,
    path::{Path, PathBuf},
    sync::Arc,
    thread,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use qrcode::{QrCode, render::unicode};
use serde::Serialize;
use thiserror::Error;

use crate::{
    CaptureDraft, CaptureRecord, CaptureSource, CaptureState, IdentityError, OpenAiTranscriber,
    PairingError, PairingRegistry, Receiver, Store, StoreError, TlsIdentity, Transcript,
    TranscriptionWorker,
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
    /// Pairing operation failed.
    #[error(transparent)]
    Pairing(#[from] PairingError),
    /// TLS identity operation failed.
    #[error(transparent)]
    Identity(#[from] IdentityError),
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
        "pair" => pair(&paths, options),
        "revoke-device" => revoke_device(&paths, options),
        "serve" => serve(store, &paths, options).await,
        other => Err(CliError::Invalid(format!("unknown command '{other}'"))),
    }
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
            "devices": pairing.status()?.devices,
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

fn pair(paths: &Paths, arguments: &[String]) -> Result<(), CliError> {
    let receiver_url = required_option(arguments, "--url")?;
    let identity = TlsIdentity::load_or_create(paths.tls())?;
    let registry = PairingRegistry::open(paths.pairing())?;
    let offer = registry.issue(
        receiver_url,
        identity.certificate_sha256(),
        now_ms(),
        PAIRING_TTL_MS,
    )?;
    let payload = serde_json::to_string(&offer)?;
    let code = QrCode::new(payload.as_bytes())?;
    let rendered = code.render::<unicode::Dense1x2>().quiet_zone(true).build();
    println!("{rendered}");
    Ok(())
}

fn revoke_device(paths: &Paths, arguments: &[String]) -> Result<(), CliError> {
    let id = positional(arguments, 0, "device UUID")?;
    PairingRegistry::open(paths.pairing())?.revoke(id)?;
    println!("{id}");
    Ok(())
}

async fn serve(store: Store, paths: &Paths, arguments: &[String]) -> Result<(), CliError> {
    let bind = required_option(arguments, "--bind")?
        .parse::<SocketAddr>()
        .map_err(|_| CliError::Invalid("--bind must be an explicit IP:port".to_owned()))?;
    let identity = TlsIdentity::load_or_create(paths.tls())?;
    let pairing = Arc::new(PairingRegistry::open(paths.pairing())?);
    let receiver = Receiver::new(store.clone(), pairing, paths.uploads())?;
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
    axum_server::bind_rustls(bind, tls)
        .serve(receiver.router().into_make_service())
        .await?;
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
    data: PathBuf,
    state: PathBuf,
}

impl Paths {
    fn discover() -> Result<Self, CliError> {
        Ok(Self {
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

    fn pairing(&self) -> PathBuf {
        self.state.join("louiselm/capture/pairing")
    }

    fn tls(&self) -> PathBuf {
        self.state.join("louiselm/capture/tls")
    }

    fn uploads(&self) -> PathBuf {
        self.state.join("louiselm/capture/uploads")
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
        "louiselm-capture commands:\n  serve --bind IP:PORT\n  pair --url HTTPS_URL\n  revoke-device DEVICE_UUID\n  ingest-local --file PATH --recorded-at-ms N --duration-ms N --mime TYPE [--id UUID]\n  list\n  status\n  retry CAPTURE_UUID\n  transcribe-once"
    );
}
