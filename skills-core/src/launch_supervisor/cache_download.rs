//! Owned cache writer; anonymous preparation precedes the local revocation boundary.

use super::{SupervisorCompletion, SupervisorError, command::CommandEnforcer};
use crate::{Digest, cache::DownloadWriter};
use std::{
    sync::Arc,
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

pub(super) struct Worker(Option<JoinHandle<()>>);

impl Worker {
    pub(super) fn join(&mut self) -> Result<(), SupervisorError> {
        self.0.take().map_or(Ok(()), |worker| {
            worker
                .join()
                .map_err(|_| SupervisorError::WorkerUnavailable)
        })
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        // Joining observes termination even after a panic. No writer can outlive
        // this owner; a panic is reported by explicit lifecycle joins above.
        let _ = self.join();
    }
}

pub(super) fn spawn(
    writer: DownloadWriter,
    digest: Digest,
    bytes: Vec<u8>,
    enforcer: Arc<CommandEnforcer>,
    expires_at_ms: u64,
    complete: SupervisorCompletion<String>,
) -> Result<Worker, SupervisorError> {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .and_then(|value| u64::try_from(value.as_millis()).ok())
        .ok_or(SupervisorError::AuthorizationRejected)?;
    let remaining = expires_at_ms
        .checked_sub(now)
        .filter(|ms| *ms > 0)
        .ok_or(SupervisorError::AuthorizationRejected)?;
    let deadline = Instant::now()
        .checked_add(Duration::from_millis(remaining))
        .ok_or(SupervisorError::AuthorizationRejected)?;
    thread::Builder::new()
        .name("louiselm-dependency-cache".into())
        .spawn(move || {
            let result = writer
                .prepare(&digest, &bytes)
                .map_err(|_| SupervisorError::DurabilityUnavailable)
                .and_then(|prepared| {
                    enforcer.publish_cache(deadline, || {
                        prepared
                            .publish()
                            .map_err(|_| SupervisorError::DurabilityUnavailable)
                    })
                });
            complete(result);
        })
        .map(|worker| Worker(Some(worker)))
        .map_err(|_| SupervisorError::WorkerUnavailable)
}
