//! Explicit service capability selection and ownership of listeners and workers.

use std::{
    env,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::Duration,
};

use tokio::task::JoinSet;

use super::{CliError, Paths, notification_worker, openai_provider, required_environment};
use crate::{
    AttentionSocket, AttentionStore, BeadsCleanup, BrokerAttentionConfig, BrokerAttentionSocket,
    NetworkProfile, PairingRegistry, Receiver, RunSocket, RunStore, Store, TlsIdentity,
    TranscriptionWorker, time::now_ms,
};

#[derive(Clone, Copy)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "These are five independent operator opt-ins, not mutually exclusive states."
)]
struct Features {
    attention: bool,
    runs: bool,
    receiver: bool,
    transcription: bool,
    push: bool,
}

fn enabled(name: &str) -> Result<bool, CliError> {
    match env::var(name) {
        Err(env::VarError::NotPresent) => Ok(false),
        Ok(value) if value == "false" || value.is_empty() => Ok(false),
        Ok(value) if value == "true" => Ok(true),
        _ => Err(CliError::Invalid(format!("{name} must be true or false"))),
    }
}

impl Features {
    fn read() -> Result<Self, CliError> {
        let features = Self {
            attention: enabled("LOUISELM_ATTENTION_ENABLED")?,
            runs: enabled("LOUISELM_RUNS_ENABLED")?,
            receiver: enabled("LOUISELM_RECEIVER_ENABLED")?,
            transcription: enabled("LOUISELM_TRANSCRIPTION_ENABLED")?,
            push: enabled("LOUISELM_PUSH_ENABLED")?,
        };
        if !(features.attention
            || features.runs
            || features.receiver
            || features.transcription
            || features.push)
        {
            return Err(CliError::Invalid("serve has no capabilities enabled; set the required LOUISELM_*_ENABLED=true choices in capture.env".into()));
        }
        if features.push && !features.attention {
            return Err(CliError::Invalid(
                "push requires explicit LOUISELM_ATTENTION_ENABLED=true".into(),
            ));
        }
        if features.transcription {
            required_environment("OPENAI_API_KEY")?;
        }
        if features.push {
            required_environment("GOOGLE_APPLICATION_CREDENTIALS")?;
        }
        Ok(features)
    }
}

struct Prepared {
    paths: Paths,
    runs: Option<RunStore>,
    attention: Option<AttentionStore>,
    captures: Option<Store>,
    cleanup: Option<BeadsCleanup>,
    receiver: Option<(Receiver, NetworkProfile, TlsIdentity)>,
    pairing: Option<Arc<PairingRegistry>>,
}

fn prepare(paths: Paths, features: Features) -> Result<Prepared, CliError> {
    if !features.runs && RunStore::has_pending(&paths.runs())? {
        return Err(CliError::Invalid("retained Runs still require cleanup; keep LOUISELM_RUNS_ENABLED=true until they are disposed".into()));
    }
    let cleanup = if features.runs {
        Some(BeadsCleanup::new(
            PathBuf::from(required_environment("LOUISELM_BEADS_WORKSPACE")?),
            required_environment("LOUISELM_REAL_BR")?,
        )?)
    } else {
        None
    };
    let runs = features
        .runs
        .then(|| RunStore::new(paths.runs()))
        .transpose()?;
    let attention = features
        .attention
        .then(|| AttentionStore::new(paths.attention(), runs.clone()))
        .transpose()?;
    let captures = (features.receiver || features.transcription)
        .then(|| Store::new(paths.captures()))
        .transpose()?;
    let pairing = (features.receiver || features.push)
        .then(|| PairingRegistry::open(paths.pairing()).map(Arc::new))
        .transpose()?;
    let receiver = if features.receiver {
        let identity = TlsIdentity::load_or_create(paths.tls())?;
        let network = NetworkProfile::load_or_default(&paths.network())?;
        let (Some(store), Some(pairing)) = (&captures, &pairing) else {
            return Err(CliError::Invalid("receiver storage is unavailable".into()));
        };
        let receiver = match &attention {
            Some(attention) => Receiver::with_attention(
                store.clone(),
                attention.clone(),
                pairing.clone(),
                paths.uploads(),
                identity.public_key_sha256(),
            )?,
            None => Receiver::new(
                store.clone(),
                pairing.clone(),
                paths.uploads(),
                identity.public_key_sha256(),
            )?,
        };
        Some((receiver, network, identity))
    } else {
        None
    };
    Ok(Prepared {
        paths,
        runs,
        attention,
        captures,
        cleanup,
        receiver,
        pairing,
    })
}

fn worker_pause(stop: &AtomicBool) {
    for _ in 0..10 {
        if stop.load(Ordering::Acquire) {
            break;
        }
        thread::sleep(Duration::from_secs(1));
    }
}

async fn transcribe(store: Store, stop: Arc<AtomicBool>) -> Result<(), CliError> {
    tokio::task::spawn_blocking(move || {
        // Construct and drop reqwest's blocking client outside the async executor.
        let provider = openai_provider()?;
        while !stop.load(Ordering::Acquire) {
            TranscriptionWorker::new(&store, &provider).process_ready(now_ms())?;
            worker_pause(&stop);
        }
        Ok(())
    })
    .await
    .map_err(std::io::Error::other)?
}

async fn reap(
    runs: RunStore,
    cleanup: BeadsCleanup,
    stop: Arc<AtomicBool>,
) -> Result<(), CliError> {
    tokio::task::spawn_blocking(move || {
        while !stop.load(Ordering::Acquire) {
            let summary = runs.reap_expired(now_ms(), |action| cleanup.release(action))?;
            if !summary.failed.is_empty() {
                eprintln!(
                    "louiselm-capture: {} Run cleanup actions still require retry",
                    summary.failed.len()
                );
            }
            worker_pause(&stop);
        }
        Ok(())
    })
    .await
    .map_err(std::io::Error::other)?
}

pub(super) async fn serve(paths: Paths) -> Result<(), CliError> {
    let features = Features::read()?;
    let credentials = features
        .push
        .then(|| required_environment("GOOGLE_APPLICATION_CREDENTIALS").map(PathBuf::from))
        .transpose()?;
    let (prepared, broker) = tokio::task::spawn_blocking(move || {
        let broker = if features.attention {
            BrokerAttentionConfig::load_installed()?
        } else {
            None
        };
        Ok::<_, CliError>((prepare(paths, features)?, broker))
    })
    .await
    .map_err(std::io::Error::other)??;
    serve_prepared(prepared, features, credentials, broker).await
}

async fn serve_prepared(
    prepared: Prepared,
    features: Features,
    credentials: Option<PathBuf>,
    broker: Option<BrokerAttentionConfig>,
) -> Result<(), CliError> {
    let mut servers = JoinSet::new();
    // Bind every listener before starting workers; startup failures cannot leave
    // transcription or cleanup detached from their service owner.
    if let Some(runs) = &prepared.runs {
        let socket = RunSocket::bind(
            prepared.paths.run_socket(),
            prepared.paths.operator_capability(),
            runs.clone(),
        )
        .await?;
        servers.spawn(async move { socket.serve().await.map_err(CliError::from) });
    }
    if let Some(attention) = &prepared.attention {
        let socket = AttentionSocket::bind(
            prepared.paths.attention_socket(),
            prepared.paths.operator_capability(),
            attention.clone(),
        )
        .await?;
        servers.spawn(async move { socket.serve().await.map_err(CliError::from) });
        if let Some(config) = broker {
            let socket = BrokerAttentionSocket::bind(config, attention.clone()).await?;
            servers.spawn(async move { socket.serve().await.map_err(CliError::from) });
        }
    }
    if let Some((receiver, network, identity)) = prepared.receiver {
        let tls = axum_server::tls_rustls::RustlsConfig::from_pem_file(
            identity.certificate_path(),
            identity.private_key_path(),
        )
        .await?;
        servers.spawn(async move {
            axum_server::bind_rustls(network.bind(), tls)
                .serve(receiver.router().into_make_service())
                .await
                .map_err(CliError::from)
        });
    }
    let stop = Arc::new(AtomicBool::new(false));
    let mut workers = JoinSet::new();
    if let (Some(runs), Some(cleanup)) = (prepared.runs, prepared.cleanup) {
        workers.spawn(reap(runs, cleanup, stop.clone()));
    }
    if features.transcription
        && let Some(store) = prepared.captures
    {
        workers.spawn(transcribe(store, stop.clone()));
    }
    if features.push
        && let (Some(attention), Some(pairing)) = (prepared.attention, prepared.pairing)
    {
        workers.spawn(notification_worker::run(
            attention,
            pairing,
            credentials,
            stop.clone(),
        ));
    }
    let result = tokio::select! {
        result = servers.join_next(), if !servers.is_empty() => result,
        result = workers.join_next(), if !workers.is_empty() => result,
    };
    stop.store(true, Ordering::Release);
    servers.abort_all();
    while servers.join_next().await.is_some() {}
    let mut outcome = match result {
        Some(result) => result
            .map_err(|err| CliError::Io(std::io::Error::other(err)))
            .and_then(|result| result),
        None => Err(CliError::Invalid(
            "service has no active capabilities".into(),
        )),
    };
    while let Some(result) = workers.join_next().await {
        let worker_result = result
            .map_err(|err| CliError::Io(std::io::Error::other(err)))
            .and_then(|result| result);
        if outcome.is_ok() {
            outcome = worker_result;
        }
    }
    outcome
}

#[cfg(test)]
mod tests;
