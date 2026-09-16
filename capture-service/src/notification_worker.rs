//! CLI ownership of the optional blocking FCM sender.

use std::{
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use super::CliError;
use crate::{AttentionStore, NotificationHealth, PairingRegistry, fcm::FcmSender, time::now_ms};

pub(super) async fn run(
    attention: AttentionStore,
    pairing: Arc<PairingRegistry>,
    credentials: Option<PathBuf>,
    stop: Arc<AtomicBool>,
) -> Result<(), CliError> {
    let registry = pairing.clone();
    let mut sender = tokio::task::spawn_blocking(move || {
        let (sender, health) = match credentials {
            None => (None, NotificationHealth::Unconfigured),
            Some(path) => match FcmSender::load(&path) {
                Ok(sender) => (Some(sender), NotificationHealth::Ready),
                Err(_) => (None, NotificationHealth::ConfigurationError),
            },
        };
        registry.configure_notifications(health)?;
        Ok::<_, CliError>((sender, health))
    })
    .await
    .map_err(|_| CliError::NotificationWorker)??;
    while !stop.load(Ordering::Acquire) {
        let registry = pairing.clone();
        let attention = attention.clone();
        let cancelled = stop.clone();
        // Own and await the blocking pass even when shutdown is requested. Each
        // request is bounded to ten seconds; the next device observes cancellation.
        sender = tokio::task::spawn_blocking(move || {
            if let Some(provider) = sender.0.as_mut() {
                registry.deliver_notifications(
                    || {
                        if cancelled.load(Ordering::Acquire) {
                            return Ok(None);
                        }
                        let snapshot = attention.snapshot()?;
                        Ok(snapshot
                            .items
                            .iter()
                            .any(|item| item.eligible)
                            .then_some(snapshot.generation))
                    },
                    now_ms(),
                    |token, generation| provider.send(token, generation, now_ms()),
                )?;
            } else {
                // A CLI retry cannot manufacture a usable sender in this process.
                registry.configure_notifications(sender.1)?;
            }
            Ok::<_, CliError>(sender)
        })
        .await
        .map_err(|_| CliError::NotificationWorker)??;
        if !stop.load(Ordering::Acquire) {
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
    }
    // reqwest's blocking client owns a runtime; also destroy it off the executor.
    tokio::task::spawn_blocking(move || drop(sender))
        .await
        .map_err(|_| CliError::NotificationWorker)?;
    Ok(())
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "Tests run the production worker with absent credentials and bounded shutdown."
)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn absent_or_invalid_credentials_preserve_inbox_and_worker_shutdown() {
        for configured in [false, true] {
            let root = tempfile::tempdir().unwrap();
            let pairing = Arc::new(PairingRegistry::open(root.path().join("pairing")).unwrap());
            pairing
                .configure_notifications(NotificationHealth::Ready)
                .unwrap();
            let attention = AttentionStore::new(
                root.path().join("attention"),
                Some(crate::RunStore::new(root.path().join("runs")).unwrap()),
            )
            .unwrap();
            let before = attention.snapshot().unwrap();
            let stop = Arc::new(AtomicBool::new(false));
            let path = configured.then(|| root.path().join("absent-credential.json"));
            let task = tokio::spawn(run(attention.clone(), pairing.clone(), path, stop.clone()));
            let expected = if configured {
                NotificationHealth::ConfigurationError
            } else {
                NotificationHealth::Unconfigured
            };
            tokio::time::timeout(Duration::from_secs(5), async {
                loop {
                    if pairing.notification_status().unwrap().health == expected {
                        break;
                    }
                    tokio::task::yield_now().await;
                }
            })
            .await
            .unwrap();
            stop.store(true, Ordering::Release);
            tokio::time::timeout(Duration::from_secs(5), task)
                .await
                .unwrap()
                .unwrap()
                .unwrap();
            assert_eq!(attention.snapshot().unwrap(), before);
        }
    }
}
