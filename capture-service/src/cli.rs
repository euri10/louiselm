//! Command-line wiring for the capture service.

use std::{
    env,
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    net::SocketAddr,
    path::{Path, PathBuf},
};

use clap::{CommandFactory, Parser};
use qrcode::{
    QrCode,
    render::{svg, unicode},
};
use serde::Serialize;
use thiserror::Error;

mod arguments;
use arguments::{Attention, Cli, Command, PhoneControl, Run};

#[path = "notification_worker.rs"]
mod notification_worker;
mod service;

use crate::time::now_ms;
use crate::{
    AttentionError, AttentionSocketError, AttentionStore, BeadsGenerator, CaptureDraft,
    CaptureRecord, CaptureSource, CaptureState, GenerateRequest, GeneratedWorkReservation,
    GenerationError, IdentityError, NetworkProfile, NetworkProfileError, NetworkProfileKind,
    OpenAiTranscriber, PairingError, PairingRegistry, ReserveResult, RunAdmission, RunDraft,
    RunSession, RunSocketError, RunStore, RunStoreError, Store, StoreError, TlsIdentity,
    Transcript, TranscriptionWorker,
};

const DEFAULT_MODEL: &str = "gpt-transcribe";
const PAIRING_TTL_MS: u64 = 10 * 60 * 1_000;

/// CLI failure with actionable local context and no credentials.
#[derive(Debug, Error)]
pub enum CliError {
    /// The owned notification worker failed; no provider payload is displayed.
    #[error("notification worker is unavailable; inspect local storage and restart serve")]
    NotificationWorker,
    /// Required argument or environment configuration is invalid.
    #[error("invalid command: {0}")]
    Invalid(String),
    /// Filesystem or socket operation failed.
    #[error("capture service I/O failed: {0}")]
    Io(#[from] std::io::Error),
    /// Capture store operation failed.
    #[error(transparent)]
    Store(#[from] StoreError),
    /// Attention store operation failed.
    #[error(transparent)]
    Attention(#[from] AttentionError),
    /// Durable Run-store operation failed.
    #[error(transparent)]
    Run(#[from] RunStoreError),
    /// Local Run observation socket failed.
    #[error(transparent)]
    RunSocket(#[from] RunSocketError),
    /// Local Attention socket failed.
    #[error(transparent)]
    AttentionSocket(#[from] AttentionSocketError),
    /// Generated-work broker failed.
    #[error(transparent)]
    Generation(#[from] GenerationError),
    /// Ambiguous generated work can be retried with the same safe mutation identity.
    #[error("{source}; retry with LOUISELM_MUTATION_ID={mutation_id}")]
    GenerationRetry {
        /// Retry identity that prevents duplicate generated work.
        mutation_id: String,
        /// Failure that left the mutation outcome uncertain.
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
    let cli = match Cli::try_parse() {
        Ok(cli) => cli,
        Err(error)
            if matches!(
                error.kind(),
                clap::error::ErrorKind::DisplayHelp | clap::error::ErrorKind::DisplayVersion
            ) =>
        {
            error.print()?;
            return Ok(());
        }
        Err(_) => {
            return Err(CliError::Invalid(
                "invalid command or argument; use --help".into(),
            ));
        }
    };
    if cli
        .require_interface
        .is_some_and(|required| required != crate::compatibility::metadata().interfaces.capture)
    {
        return Err(CliError::Invalid(format!(
            "louiselm-capture {}: incompatible capture interface; install the capture release required by the plugin",
            env!("CARGO_PKG_VERSION")
        )));
    }
    let Some(command) = cli.command else {
        Cli::command().print_help()?;
        println!();
        return Ok(());
    };
    if matches!(command, Command::Metadata) {
        println!(
            "{}",
            serde_json::to_string(&crate::compatibility::metadata())?
        );
        return Ok(());
    }
    let paths = Paths::discover()?;
    match command {
        Command::IngestLocal {
            file,
            id,
            recorded_at_ms,
            duration_ms,
            mime,
        } => ingest_local(
            &Store::new(paths.captures())?,
            &file,
            id,
            recorded_at_ms,
            duration_ms,
            mime,
        ),
        Command::List => list(&Store::new(paths.captures())?),
        Command::Status => status(
            &Store::new(paths.captures())?,
            &attention_store(&paths)?,
            &paths,
        ),
        Command::Attention { command } => attention_command(&attention_store(&paths)?, command),
        Command::PhoneControl {
            command: PhoneControl::Verify,
        } => tokio::task::spawn_blocking(move || verify_phone_control(&paths))
            .await
            .map_err(std::io::Error::other)?,
        Command::PhoneControl {
            command: PhoneControl::OwnerJournal,
        } => tokio::task::spawn_blocking(move || phone_action_owner_journal(&paths))
            .await
            .map_err(std::io::Error::other)?,
        Command::PhoneControl { command } => {
            let response = phone_control_admin(&paths, command).await?;
            println!("{}", serde_json::to_string(&response)?);
            Ok(())
        }
        Command::Retry { id } => retry(&Store::new(paths.captures())?, &id),
        Command::TranscribeOnce => tokio::task::spawn_blocking(move || {
            // Keep the blocking HTTP client's construction, work, and drop off
            // the async executor, including early error returns.
            transcribe_once(&Store::new(paths.captures())?)
        })
        .await
        .map_err(std::io::Error::other)?,
        Command::Run { command } => run_command(&paths, command),
        Command::ConfigureNetwork { profile, bind, url } => {
            configure_network(&paths, &profile, bind, &url)
        }
        Command::Pair { svg } => pair(&paths, svg.as_deref()),
        Command::RevokeDevice { id } => revoke_device(&paths, &id).await,
        Command::RetryNotifications => {
            PairingRegistry::open(paths.pairing())?.retry_notifications()?;
            Ok(())
        }
        Command::Serve => service::serve(paths).await,
        Command::Metadata => Ok(()),
    }
}

fn verify_phone_control(paths: &Paths) -> Result<(), CliError> {
    let mut input = Vec::new();
    std::io::stdin()
        .take(u64::try_from(crate::phone_control::MAX_FRAME + 1).map_err(std::io::Error::other)?)
        .read_to_end(&mut input)?;
    let line = input.strip_suffix(b"\n").unwrap_or(&input);
    let outcome = if input.len() > crate::phone_control::MAX_FRAME
        || line.contains(&b'\n')
        || line.contains(&b'\r')
    {
        Err(crate::PhoneControlError::Malformed)
    } else {
        TlsIdentity::existing_public_key_sha256(&paths.tls())
            .map_err(|_| crate::PhoneControlError::Unavailable)
            .and_then(|identity| crate::verify_phone_control_frame(line, &identity))
    };
    match outcome {
        Ok(()) => println!("{{\"v\":1,\"type\":\"verified\"}}"),
        Err(code) => println!("{{\"v\":1,\"type\":\"error\",\"code\":\"{code}\"}}"),
    }
    Ok(())
}

fn phone_action_owner_journal(paths: &Paths) -> Result<(), CliError> {
    let mut input = Vec::new();
    std::io::stdin()
        .take(u64::try_from(crate::phone_control::MAX_FRAME + 1).map_err(std::io::Error::other)?)
        .read_to_end(&mut input)?;
    let response = crate::phone_control::owner_journal::handle(
        &paths.state.join("louiselm/workflow/phone-action-owner"),
        &input,
    );
    println!("{}", serde_json::to_string(&response)?);
    Ok(())
}

async fn phone_control_admin(
    paths: &Paths,
    command: PhoneControl,
) -> Result<serde_json::Value, CliError> {
    let stream = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        tokio::net::UnixStream::connect(paths.phone_control_admin_socket()),
    )
    .await
    .map_err(|_| CliError::Invalid("phone-control administration timed out".into()))??;
    phone_control_admin_stream(paths, command, stream).await
}

async fn phone_control_admin_stream(
    paths: &Paths,
    command: PhoneControl,
    stream: tokio::net::UnixStream,
) -> Result<serde_json::Value, CliError> {
    let timeout = if matches!(
        &command,
        PhoneControl::FreezeRunSource { .. } | PhoneControl::PrepareRunCheckout { .. }
    ) {
        std::time::Duration::from_secs(195)
    } else {
        std::time::Duration::from_secs(5)
    };
    tokio::time::timeout(
        timeout,
        phone_control_admin_exchange(paths, command, stream),
    )
    .await
    .map_err(|_| CliError::Invalid("phone-control administration timed out".into()))?
}

async fn phone_control_admin_exchange(
    paths: &Paths,
    command: PhoneControl,
    stream: tokio::net::UnixStream,
) -> Result<serde_json::Value, CliError> {
    let capability = tokio::fs::read_to_string(paths.operator_capability()).await?;
    let (request, expected) = phone_control_admin_request(command, capability.trim())?;
    phone_control_write_request(stream, &request, expected).await
}

fn phone_control_admin_request(
    command: PhoneControl,
    capability: &str,
) -> Result<(serde_json::Value, &'static str), CliError> {
    Ok(match command {
        PhoneControl::PrepareRunCheckout {
            run_id,
            source_id,
            checkout,
        } => (
            serde_json::json!({"v":1,"type":"prepare_run_checkout","capability":capability,
                "run_id":run_id,"source_id":source_id,"checkout":checkout}),
            "run_checkout",
        ),
        PhoneControl::InspectRunCheckout { run_id } => (
            serde_json::json!({"v":1,"type":"inspect_run_checkout","capability":capability,"run_id":run_id}),
            "run_checkout",
        ),
        PhoneControl::FreezeRunSource {
            project_id,
            run_id,
            snapshot,
            includes,
        } => (
            serde_json::json!({"v":1,"type":"freeze_run_source","capability":capability,
                "project_id":project_id,"run_id":run_id,"snapshot":snapshot,"includes":includes}),
            "run_source",
        ),
        PhoneControl::InspectRunSource { run_id } => (
            serde_json::json!({"v":1,"type":"inspect_run_source","capability":capability,"run_id":run_id}),
            "run_source",
        ),
        PhoneControl::Projects { after } => (
            serde_json::json!({"v":1,"type":"projects","capability":capability,"after":after}),
            "projects",
        ),
        PhoneControl::ProjectRoots { after } => (
            serde_json::json!({"v":1,"type":"project_roots","capability":capability,"after":after}),
            "project_roots",
        ),
        PhoneControl::RegisterProject { path, root_ids } => (
            serde_json::json!({"v":1,"type":"register_project","capability":capability,"path":path,"root_ids":root_ids}),
            "project_registered",
        ),
        PhoneControl::LinkProjectRoots {
            project_id,
            root_ids,
        } => (
            serde_json::json!({"v":1,"type":"link_project_roots","capability":capability,"project_id":project_id,"root_ids":root_ids}),
            "project_roots_linked",
        ),
        PhoneControl::RegisterRoot { path } => (
            serde_json::json!({"v":1,"type":"register_root","capability":capability,"path":path}),
            "root_registered",
        ),
        PhoneControl::InspectGrant { device_id } => (
            serde_json::json!({"v":1,"type":"inspect_grant","capability":capability,"device_id":device_id}),
            "grant_inspection",
        ),
        PhoneControl::ConfirmLegacyProject {
            device_id,
            grant_id,
            grant_revision,
            scope_digest,
            project_id,
        } => (
            serde_json::json!({"v":1,"type":"confirm_legacy_project","capability":capability,
                "device_id":device_id,"grant_id":grant_id,"grant_revision":grant_revision,
                "scope_digest":scope_digest,"project_id":project_id}),
            "legacy_project_confirmed",
        ),
        enrollment => return phone_control_enrollment_request(enrollment, capability),
    })
}

fn phone_control_enrollment_request(
    command: PhoneControl,
    capability: &str,
) -> Result<(serde_json::Value, &'static str), CliError> {
    Ok(match command {
        PhoneControl::Start { device_id } => (
            serde_json::json!({"v":1,"type":"start","capability":capability,"device_id":device_id}),
            "enrollment_started",
        ),
        PhoneControl::Inspect { device_id } => (
            serde_json::json!({"v":1,"type":"inspect","capability":capability,"device_id":device_id}),
            "enrollment_inspection",
        ),
        PhoneControl::Cancel {
            device_id,
            enrollment_id,
        } => (
            serde_json::json!({"v":1,"type":"cancel","capability":capability,"device_id":device_id,"enrollment_id":enrollment_id}),
            "enrollment_cancelled",
        ),
        PhoneControl::Approve {
            device_id,
            enrollment_id,
            enrolled_key,
            projects,
            project_root_ids,
            agents,
            groups,
        } => (
            serde_json::json!({"v":2,"type":"approve","capability":capability,"device_id":device_id,
                "enrollment_id":enrollment_id,"enrolled_key":enrolled_key,
                "scope":{"projects":projects,"agents":agents,"groups":groups},
                "project_root_ids":project_root_ids}),
            "enrollment_approved",
        ),
        PhoneControl::Disable { device_id } => (
            serde_json::json!({"v":1,"type":"disable","capability":capability,"device_id":device_id}),
            "disabled",
        ),
        PhoneControl::Forget { device_id } => (
            serde_json::json!({"v":1,"type":"forget","capability":capability,"device_id":device_id}),
            "forgotten",
        ),
        _ => {
            return Err(CliError::Invalid(
                "expected an administration command".into(),
            ));
        }
    })
}

async fn phone_control_write_request(
    mut stream: tokio::net::UnixStream,
    request: &serde_json::Value,
    expected: &str,
) -> Result<serde_json::Value, CliError> {
    use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};

    let bytes = serde_json::to_vec(request)?;
    if bytes.len() >= crate::phone_control::MAX_FRAME {
        return Err(CliError::Invalid(
            "phone-control request is too large".into(),
        ));
    }
    stream.write_all(&bytes).await?;
    stream.write_all(b"\n").await?;
    let mut reader =
        BufReader::new(stream.take(
            u64::try_from(crate::phone_control::MAX_FRAME + 1).map_err(std::io::Error::other)?,
        ));
    let mut reply = Vec::new();
    reader.read_until(b'\n', &mut reply).await?;
    if reply.len() > crate::phone_control::MAX_FRAME || reply.last() != Some(&b'\n') {
        return Err(CliError::Invalid(
            "phone-control response is invalid".into(),
        ));
    }
    phone_control_admin_response(&reply, expected)
}

fn phone_control_admin_response(
    reply: &[u8],
    expected: &str,
) -> Result<serde_json::Value, CliError> {
    let response: serde_json::Value = serde_json::from_slice(reply)?;
    let expected_version = if response.get("type") == Some(&serde_json::json!("error")) {
        1
    } else if expected == "enrollment_approved" {
        2
    } else {
        1
    };
    if response.get("v") != Some(&serde_json::json!(expected_version)) {
        return Err(CliError::Invalid(
            "phone-control response is invalid".into(),
        ));
    }
    match response.get("type").and_then(serde_json::Value::as_str) {
        Some(kind) if kind == expected => Ok(response),
        Some("error") => {
            let code = response
                .get("code")
                .and_then(serde_json::Value::as_str)
                .filter(|code| {
                    matches!(
                        *code,
                        "unauthenticated"
                            | "expired"
                            | "revoked"
                            | "out_of_scope"
                            | "stale_subject"
                            | "unavailable"
                            | "malformed"
                            | "unsupported"
                    )
                })
                .unwrap_or("unavailable");
            Err(CliError::Invalid(format!(
                "phone-control request refused: {code}"
            )))
        }
        _ => Err(CliError::Invalid(
            "phone-control response is invalid".into(),
        )),
    }
}

fn attention_store(paths: &Paths) -> Result<AttentionStore, CliError> {
    let runs = if paths.runs().try_exists()? {
        Some(RunStore::new(paths.runs())?)
    } else {
        None
    };
    Ok(AttentionStore::new(paths.attention(), runs)?)
}

fn run_command(paths: &Paths, command: Run) -> Result<(), CliError> {
    match command {
        Run::List => {
            let runs = RunStore::new(paths.runs())?.list_resumable(now_ms())?;
            println!("{}", serde_json::to_string(&runs)?);
        }
        Run::Admit {
            id,
            generated_work_max,
            park_ttl_ms,
        } => {
            let token = uuid::Uuid::new_v4().to_string();
            let admission = RunAdmission {
                id,
                generated_work_ceiling: generated_work_max,
                park_ttl_ms,
            };
            RunStore::new(paths.runs())?.admit(admission.clone(), &token)?;
            println!(
                "{}",
                serde_json::to_string(&serde_json::json!({
                    "id": admission.id, "state": "active", "token": token,
                    "generated_work": {"ceiling": admission.generated_work_ceiling, "consumed": 0, "reserved": 0}
                }))?
            );
        }
        Run::Attach(session) => {
            let session = RunSession {
                id: session.id,
                session_id: session.session_id,
                agent: session.agent,
                acp_session_id: session.acp_session_id,
                working_dir: session.cwd,
                load_session: session.load_session,
            };
            RunStore::new(paths.runs())?.attach(session.clone())?;
            println!(
                "{}",
                serde_json::to_string(&serde_json::json!({"id": session.id, "state": "active"}))?
            );
        }
        Run::Generate { command, arguments } => return generate(paths, command, arguments),
        Run::Park { session, claims } => {
            let draft = RunDraft {
                id: session.id,
                session_id: session.session_id,
                agent: session.agent,
                acp_session_id: session.acp_session_id,
                working_dir: session.cwd,
                load_session: session.load_session,
                claimed_issue_ids: claims
                    .split(',')
                    .filter(|claim| !claim.is_empty())
                    .map(str::to_owned)
                    .collect(),
            };
            RunStore::new(paths.runs())?.park_cold(draft.clone(), now_ms())?;
            println!(
                "{}",
                serde_json::to_string(
                    &serde_json::json!({"id": draft.id, "state": "cold_parked"})
                )?
            );
        }
        command @ (Run::Reserve { .. } | Run::Confirm { .. } | Run::Release { .. }) => {
            return reservation(paths, command);
        }
    }
    Ok(())
}

/// Drive one generated-work reservation against the Run ledger.
/// Credentials arrive through the environment, never argv.
/// Exhaustion is a successful Park outcome named in JSON.
fn reservation(paths: &Paths, command: Run) -> Result<(), CliError> {
    let run_id = required_environment("LOUISELM_RUN_ID")?;
    let token = required_environment("LOUISELM_RUN_TOKEN")?;
    let store = RunStore::new(paths.runs())?;
    let state = match command {
        Run::Reserve {
            mutation_id,
            kind,
            units,
        } => {
            match store.reserve_generated_work(
                GeneratedWorkReservation {
                    run_id: run_id.clone(),
                    token: token.clone(),
                    mutation_id,
                    kind,
                    units,
                },
                now_ms(),
            )? {
                ReserveResult::Reserved => "reserved",
                ReserveResult::Pending => "pending",
                ReserveResult::Consumed => "consumed",
                ReserveResult::Exhausted => "exhausted",
            }
        }
        Run::Confirm {
            mutation_id,
            issue_id,
        } => {
            store.confirm_generated_work(&run_id, &token, &mutation_id, &issue_id)?;
            "confirmed"
        }
        Run::Release { mutation_id } => {
            store.release_generated_work(&run_id, &token, &mutation_id)?;
            "released"
        }
        _ => return Err(CliError::Invalid("expected a reservation command".into())),
    };
    let budget = store.run(&run_id)?.generated_work;
    println!(
        "{}",
        serde_json::to_string(&serde_json::json!({
            "id": run_id, "state": state,
            "generated_work": {"ceiling": budget.ceiling, "consumed": budget.consumed, "reserved": budget.reserved}
        }))?
    );
    Ok(())
}

fn generate(paths: &Paths, command: String, arguments: Vec<String>) -> Result<(), CliError> {
    let mutation_id = env::var("LOUISELM_MUTATION_ID")
        .ok()
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    let request = GenerateRequest {
        run_id: required_environment("LOUISELM_RUN_ID")?,
        token: required_environment("LOUISELM_RUN_TOKEN")?,
        mutation_id: mutation_id.clone(),
        command,
        arguments,
    };
    let generator = BeadsGenerator::new(
        required_environment("LOUISELM_REAL_BR")?,
        required_environment("BEADS_DB")?,
    );
    let command = request.command.clone();
    let issue = generator
        .generate(&RunStore::new(paths.runs())?, &request, now_ms())
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

fn ingest_local(
    store: &Store,
    path: &Path,
    id: Option<String>,
    recorded_at_ms: u64,
    duration_ms: u64,
    mime_type: String,
) -> Result<(), CliError> {
    let id = id.unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    let outcome = store.ingest(
        &CaptureDraft {
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

fn status(store: &Store, attention: &AttentionStore, paths: &Paths) -> Result<(), CliError> {
    let captures = store.list()?;
    let pairing = PairingRegistry::open(paths.pairing())?;
    let pairing_status = pairing.status()?;
    let notifications = pairing.notification_status()?;
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
    let attention = match attention.summary() {
        Ok(summary) => serde_json::to_value(summary)?,
        Err(_) => serde_json::json!({
            "storage_error": "Attention state is unavailable"
        }),
    };
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
            "attention": attention,
            "notifications": notifications,
        }))?
    );
    Ok(())
}

fn attention_command(store: &AttentionStore, command: Attention) -> Result<(), CliError> {
    match command {
        Attention::List => println!("{}", serde_json::to_string_pretty(&store.snapshot()?)?),
        Attention::Status => println!("{}", serde_json::to_string_pretty(&store.summary()?)?),
    }
    Ok(())
}

fn retry(store: &Store, id: &str) -> Result<(), CliError> {
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

fn configure_network(
    paths: &Paths,
    profile: &str,
    bind: SocketAddr,
    url: &str,
) -> Result<(), CliError> {
    let network = NetworkProfile::new(profile.parse::<NetworkProfileKind>()?, bind, url)?;
    network.save(&paths.network())?;
    println!("{}", serde_json::to_string_pretty(&network)?);
    Ok(())
}

fn pair(paths: &Paths, svg_path: Option<&Path>) -> Result<(), CliError> {
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
        write_pairing_svg(path, rendered.as_bytes())?;
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

async fn revoke_device(paths: &Paths, id: &str) -> Result<(), CliError> {
    let connection = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        tokio::net::UnixStream::connect(paths.phone_control_admin_socket()),
    )
    .await
    .map_err(|_| CliError::Invalid("phone-control administration timed out".into()))?;
    match connection {
        Ok(stream) => {
            phone_control_admin_stream(
                paths,
                PhoneControl::Forget {
                    device_id: id.to_owned(),
                },
                stream,
            )
            .await?;
        }
        Err(error)
            if matches!(
                error.kind(),
                std::io::ErrorKind::NotFound | std::io::ErrorKind::ConnectionRefused
            ) =>
        {
            let path = paths.pairing();
            let device = id.to_owned();
            tokio::task::spawn_blocking(move || PairingRegistry::open(path)?.revoke(&device))
                .await
                .map_err(std::io::Error::other)??;
        }
        Err(error) => return Err(error.into()),
    }
    println!("{id}");
    Ok(())
}

fn openai_provider() -> Result<OpenAiTranscriber, CliError> {
    configure_openai_provider(
        env::var("LOUISELM_CAPTURE_OPENAI_API_KEY"),
        env::var("LOUISELM_TRANSCRIPTION_MODEL"),
    )
}

fn configure_openai_provider(
    api_key: Result<String, env::VarError>,
    model: Result<String, env::VarError>,
) -> Result<OpenAiTranscriber, CliError> {
    let api_key = api_key.map_err(|_| {
        CliError::Invalid(
            "LOUISELM_CAPTURE_OPENAI_API_KEY is required for transcription".to_owned(),
        )
    })?;
    let model = model.unwrap_or_else(|_| DEFAULT_MODEL.to_owned());
    OpenAiTranscriber::new(api_key, model).map_err(CliError::Invalid)
}

#[derive(Serialize)]
struct CaptureStatus {
    record: CaptureRecord,
    state: CaptureState,
    audio_path: PathBuf,
    transcript: Option<Transcript>,
}

#[derive(Clone)]
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

    fn attention(&self) -> PathBuf {
        self.state.join("louiselm/workflow/attention")
    }

    fn attention_socket(&self) -> PathBuf {
        self.state.join("louiselm/workflow/attention.sock")
    }

    fn phone_control_store(&self) -> PathBuf {
        self.state
            .join("louiselm/capture/phone-control/grants.json")
    }

    fn phone_control_socket(&self) -> PathBuf {
        self.state.join("louiselm/workflow/phone-control.sock")
    }

    fn phone_control_admin_socket(&self) -> PathBuf {
        self.state
            .join("louiselm/workflow/phone-control-admin.sock")
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

#[cfg(test)]
#[allow(
    clippy::expect_used,
    reason = "Test fixtures abort on setup failure and assert failures directly."
)]
mod tests {
    use super::*;
    use crate::Transcriber;

    #[tokio::test]
    async fn phone_control_admin_uses_the_authenticated_local_socket() {
        use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

        let root = tempfile::tempdir().expect("temporary state");
        let paths = Paths {
            config: root.path().join("config"),
            data: root.path().join("data"),
            state: root.path().join("state"),
        };
        let socket = paths.phone_control_admin_socket();
        std::fs::create_dir_all(socket.parent().expect("socket parent")).expect("socket parent");
        std::fs::write(
            paths.operator_capability(),
            "11111111-1111-4111-8111-111111111111",
        )
        .expect("capability");
        let listener = tokio::net::UnixListener::bind(socket).expect("listener");
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.expect("connection");
            let mut reader = BufReader::new(stream);
            let mut line = String::new();
            reader.read_line(&mut line).await.expect("request");
            let request: serde_json::Value = serde_json::from_str(&line).expect("JSON request");
            assert_eq!(
                request["capability"],
                "11111111-1111-4111-8111-111111111111"
            );
            assert_eq!(request["type"], "start");
            assert_eq!(request["device_id"], "22222222-2222-4222-8222-222222222222");
            reader
                .get_mut()
                .write_all(b"{\"v\":1,\"type\":\"enrollment_started\"}\n")
                .await
                .expect("response");
        });
        let response = phone_control_admin(
            &paths,
            PhoneControl::Start {
                device_id: "22222222-2222-4222-8222-222222222222".into(),
            },
        )
        .await
        .expect("admin response");
        assert_eq!(response["type"], "enrollment_started");
        server.await.expect("server");
    }

    #[tokio::test]
    async fn revoke_device_uses_the_live_phone_authority_to_revoke_both() {
        use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

        let root = tempfile::tempdir().expect("temporary state");
        let paths = Paths {
            config: root.path().join("config"),
            data: root.path().join("data"),
            state: root.path().join("state"),
        };
        let socket = paths.phone_control_admin_socket();
        std::fs::create_dir_all(socket.parent().expect("socket parent")).expect("socket parent");
        std::fs::write(
            paths.operator_capability(),
            "11111111-1111-4111-8111-111111111111",
        )
        .expect("capability");
        let listener = tokio::net::UnixListener::bind(socket).expect("listener");
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.expect("connection");
            let mut reader = BufReader::new(stream);
            let mut line = String::new();
            reader.read_line(&mut line).await.expect("request");
            let request: serde_json::Value = serde_json::from_str(&line).expect("JSON request");
            assert_eq!(request["type"], "forget");
            reader
                .get_mut()
                .write_all(b"{\"v\":1,\"type\":\"forgotten\"}\n")
                .await
                .expect("response");
        });
        revoke_device(&paths, "22222222-2222-4222-8222-222222222222")
            .await
            .expect("revoke");
        server.await.expect("server");
    }

    #[test]
    fn transcription_defaults_to_the_file_model() {
        let provider = configure_openai_provider(
            Ok("unused-test-credential".to_owned()),
            Err(env::VarError::NotPresent),
        )
        .expect("default transcription provider");
        assert_eq!(provider.model(), "gpt-transcribe");
    }

    #[test]
    fn transcription_preserves_explicit_model_overrides() {
        for model in [
            "gpt-4o-transcribe",
            "operator-pinned-transcription-2026-01-01",
        ] {
            let provider = configure_openai_provider(
                Ok("unused-test-credential".to_owned()),
                Ok(model.to_owned()),
            )
            .expect("explicit transcription provider");
            assert_eq!(provider.model(), model);
        }
    }

    #[test]
    fn transcription_rejects_empty_model_overrides() {
        assert!(matches!(
            configure_openai_provider(Ok("unused-test-credential".to_owned()), Ok(String::new())),
            Err(CliError::Invalid(message)) if message == "OpenAI transcription model must not be empty"
        ));
    }
}
