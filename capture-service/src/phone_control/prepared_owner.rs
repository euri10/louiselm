//! Owner publication of an exact private Run selection; never an admission.

use std::{
    fs, io,
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
};

use thiserror::Error;

use super::{
    prepared_selection::{SelectionError, read_selection},
    source::{PhoneSourceError, PhoneSourceStore, require_installed_skills},
    source_checkout::{CheckoutProof, verify_checkout},
};
use crate::{
    PreparedRunError, PreparedRunRegistration, PreparedRunRevision, PreparedRunStore,
    PreparedRunSummary, RunProjectBinding, RunStore, RunStoreError,
};

#[derive(Debug, Error)]
pub(super) enum OwnerPreparedError {
    #[error("prepared Run publication refused: {0}")]
    Invalid(&'static str),
    #[error("prepared Run source unavailable")]
    Source(#[from] PhoneSourceError),
    #[error("prepared Run identity unavailable")]
    Run(#[from] RunStoreError),
    #[error("prepared Run storage unavailable")]
    Prepared(#[from] PreparedRunError),
    #[error("prepared Run selection unavailable")]
    Io(#[from] io::Error),
    #[error("prepared Run selection is invalid")]
    Selection(#[from] SelectionError),
}

pub(super) struct Publication<'a> {
    pub(super) run_id: &'a str,
    pub(super) source_id: &'a str,
    pub(super) checkout_id: &'a str,
    pub(super) selection: &'a Path,
    pub(super) previous: Option<(&'a str, u64)>,
}

/// Local owner operations share the service's actual Run ledger and state root.
#[derive(Clone)]
pub(super) struct PreparedOwner {
    root: PathBuf,
    runs: RunStore,
    integration: fn() -> Result<(), PhoneSourceError>,
}

/// Read-only view; the receiver never receives owner administration methods.
#[derive(Clone)]
pub(super) struct PreparedReader(PreparedOwner);

impl PreparedOwner {
    pub(super) fn new(root: PathBuf, runs: RunStore) -> Self {
        Self {
            root,
            runs,
            integration: require_installed_skills,
        }
    }

    pub(super) fn reader(&self) -> PreparedReader {
        PreparedReader(self.clone())
    }

    #[cfg(test)]
    pub(super) fn set_test_integration(
        &mut self,
        integration: fn() -> Result<(), PhoneSourceError>,
    ) {
        self.integration = integration;
    }

    fn store(&self) -> Result<PreparedRunStore, OwnerPreparedError> {
        Ok(PreparedRunStore::open(self.root.join("prepared-runs"))?)
    }

    pub(super) fn publish(
        &self,
        request: &Publication<'_>,
    ) -> Result<PreparedRunSummary, OwnerPreparedError> {
        let sources = PhoneSourceStore::open(self.root.join("run-sources"))?;
        publish(&sources, &self.store()?, &self.runs, request)
    }

    pub(super) fn list(&self) -> Result<Vec<PreparedRunSummary>, OwnerPreparedError> {
        require_installed_skills()?;
        Ok(self.store()?.list()?)
    }

    pub(super) fn inspect(&self, id: &str) -> Result<PreparedRunRegistration, OwnerPreparedError> {
        require_installed_skills()?;
        Ok(self.store()?.inspect(id)?)
    }

    pub(super) fn unpublish(&self, id: &str, revision: u64) -> Result<(), OwnerPreparedError> {
        // Removal must remain possible when the optional integration is absent.
        Ok(self.store()?.unpublish(id, revision)?)
    }

    /// The caller holds current authority and pairing across this callback.
    pub(super) fn with_published<T>(
        &self,
        id: &str,
        revision: u64,
        inspect: impl FnOnce(&PreparedRunRevision, u32) -> T,
    ) -> Result<T, OwnerPreparedError> {
        self.with_published_by(id, revision, self.integration, inspect)
    }

    pub(super) fn with_published_by<T>(
        &self,
        id: &str,
        revision: u64,
        integration: impl FnOnce() -> Result<(), PhoneSourceError>,
        inspect: impl FnOnce(&PreparedRunRevision, u32) -> T,
    ) -> Result<T, OwnerPreparedError> {
        integration()?;
        let owner = fs::metadata(&self.root)?.uid();
        let prepared = self.store()?;
        let candidate = prepared.read(id, revision)?;
        let sources = PhoneSourceStore::open(self.root.join("run-sources"))?;
        let source = sources.read(&candidate.binding.run_id)?;
        let checkout = sources.read_checkout(&candidate.binding.run_id)?;
        if source.project_id != candidate.binding.project_id
            || source.source_id != candidate.binding.source_id
            || checkout.project_id != source.project_id
            || checkout.source_id != source.source_id
            || checkout.checkout_id != candidate.binding.checkout_id
            || checkout.snapshot_digest != source.snapshot_digest
        {
            return Err(OwnerPreparedError::Invalid(
                "retained source binding differs",
            ));
        }
        let result = self.runs.with_unused_run(&candidate.binding.run_id, || {
            prepared.with_published(id, revision, |saved| {
                if saved != &candidate {
                    return Err(OwnerPreparedError::Invalid("prepared revision changed"));
                }
                Ok(inspect(saved, owner))
            })
        })?;
        result?
    }
}

impl PreparedReader {
    /// Candidate identities are private until the caller checks current scope.
    pub(super) fn candidates(
        &self,
    ) -> Result<Vec<(PreparedRunSummary, RunProjectBinding)>, OwnerPreparedError> {
        (self.0.integration)()?;
        Ok(self.0.store()?.list_bindings()?)
    }

    pub(super) fn with_published<T>(
        &self,
        id: &str,
        revision: u64,
        inspect: impl FnOnce(&PreparedRunRevision, u32) -> T,
    ) -> Result<T, OwnerPreparedError> {
        self.0.with_published(id, revision, inspect)
    }
}

pub(super) fn publish(
    sources: &PhoneSourceStore,
    prepared: &PreparedRunStore,
    runs: &RunStore,
    request: &Publication<'_>,
) -> Result<PreparedRunSummary, OwnerPreparedError> {
    require_installed_skills()?;
    publish_with(sources, prepared, runs, request, verify_checkout)
}

pub(super) fn publish_with(
    sources: &PhoneSourceStore,
    prepared: &PreparedRunStore,
    runs: &RunStore,
    request: &Publication<'_>,
    verify: impl FnOnce(&CheckoutProof) -> Result<(), PhoneSourceError>,
) -> Result<PreparedRunSummary, OwnerPreparedError> {
    let (bytes, selection) = read_selection(request.selection, fs::metadata(&sources.root)?.uid())?;
    if selection.run_id() != request.run_id {
        return Err(OwnerPreparedError::Invalid(
            "selection Run identity differs",
        ));
    }
    sources.with_current_checkout_by(
        request.run_id,
        request.source_id,
        request.checkout_id,
        verify,
        |source, checkout| {
            if !selection.matches_checkout(&checkout.checkout.path, &checkout.head) {
                return Err(OwnerPreparedError::Invalid("selection checkout differs"));
            }
            let binding = RunProjectBinding {
                run_id: request.run_id.into(),
                project_id: source.project_id.clone(),
                source_id: source.source_id.clone(),
                checkout_id: checkout.checkout_id.clone(),
                agents: vec![selection.agent_id().to_owned()],
            };
            // Lock order: source/checkout, Run, then prepared registration.
            runs.with_unused_run(request.run_id, || {
                prepared.publish(request.previous, &binding, &bytes)
            })?
            .map_err(OwnerPreparedError::from)
        },
    )?
}
