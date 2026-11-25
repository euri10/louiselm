//! Owner publication of an exact private Run selection; never an admission.

use std::{
    fs, io,
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
};

use sha2::{Digest, Sha256};
use thiserror::Error;

use super::{
    prepared_selection::{SelectionError, parse_saved, read_selection},
    source::{PhoneSourceError, PhoneSourceStore, require_installed_skills},
    source_checkout::{CheckoutProof, verify_checkout},
};
use crate::{
    PreparedRunError, PreparedRunRegistration, PreparedRunRevision, PreparedRunStore,
    PreparedRunSummary, RunAdmission, RunProjectBinding, RunStore, RunStoreError, RunView,
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

pub(super) struct PreparedAdmission<'a> {
    pub(super) registration_id: &'a str,
    pub(super) revision: u64,
    pub(super) selection_sha256: &'a str,
    pub(super) generated_work_max: u64,
    pub(super) park_ttl_ms: u64,
    pub(super) token: &'a str,
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
pub(super) struct OwnerRunReader(PreparedOwner);

impl PreparedOwner {
    pub(super) fn new(root: PathBuf, runs: RunStore) -> Self {
        Self {
            root,
            runs,
            integration: require_installed_skills,
        }
    }

    pub(super) fn reader(&self) -> OwnerRunReader {
        OwnerRunReader(self.clone())
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

    pub(super) fn admit(
        &self,
        request: &PreparedAdmission<'_>,
    ) -> Result<String, OwnerPreparedError> {
        (self.integration)()?;
        let sources = PhoneSourceStore::open(self.root.join("run-sources"))?;
        admit_with(
            &sources,
            &self.store()?,
            &self.runs,
            request,
            verify_checkout,
        )
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

impl OwnerRunReader {
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

    pub(super) fn run_ids(&self) -> Result<Vec<String>, OwnerPreparedError> {
        (self.0.integration)()?;
        Ok(self.0.runs.phone_ids()?)
    }

    pub(super) fn run_binding(
        &self,
        id: &str,
    ) -> Result<Option<RunProjectBinding>, OwnerPreparedError> {
        (self.0.integration)()?;
        match self
            .0
            .runs
            .with_phone_binding(id, 0, |binding, _| binding.clone())
        {
            Ok(binding) => Ok(Some(binding)),
            Err(RunStoreError::Invalid(_)) => Ok(None),
            Err(error) => Err(error.into()),
        }
    }

    /// Match the live owner's exact retained selection, including after
    /// unpublication. A published replacement cannot pass the unused-Run fence
    /// after this Run is admitted.
    pub(super) fn registration_matches(
        &self,
        id: &str,
        revision: u64,
        binding: &RunProjectBinding,
    ) -> Result<bool, OwnerPreparedError> {
        let registration = match self.0.store()?.inspect(id) {
            Ok(registration) => registration,
            Err(PreparedRunError::Invalid(_)) => return Ok(false),
            Err(error) => return Err(error.into()),
        };
        Ok(registration.revisions.last().is_some_and(|saved| {
            saved.revision == revision
                && saved.binding.run_id == binding.run_id
                && saved.binding.project_id == binding.project_id
                && saved.binding.source_id == binding.source_id
                && saved.binding.checkout_id == binding.checkout_id
                && saved
                    .binding
                    .agents
                    .iter()
                    .all(|agent| binding.agents.contains(agent))
        }))
    }

    pub(super) fn with_bound_run<T>(
        &self,
        id: &str,
        revision: u64,
        inspect: impl FnOnce(&RunProjectBinding, &RunView) -> T,
    ) -> Result<T, OwnerPreparedError> {
        (self.0.integration)()?;
        let sources = PhoneSourceStore::open(self.0.root.join("run-sources"))?;
        let source = sources.read(id)?;
        let checkout = sources.read_checkout(id)?;
        self.0
            .runs
            .with_phone_binding(id, revision, |binding, view| {
                if source.project_id != binding.project_id
                    || source.source_id != binding.source_id
                    || checkout.project_id != source.project_id
                    || checkout.source_id != source.source_id
                    || checkout.checkout_id != binding.checkout_id
                    || checkout.snapshot_digest != source.snapshot_digest
                {
                    return Err(OwnerPreparedError::Invalid(
                        "retained source binding differs",
                    ));
                }
                Ok(inspect(binding, view))
            })?
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

pub(super) fn admit_with(
    sources: &PhoneSourceStore,
    prepared: &PreparedRunStore,
    runs: &RunStore,
    request: &PreparedAdmission<'_>,
    verify: impl FnOnce(&CheckoutProof) -> Result<(), PhoneSourceError>,
) -> Result<String, OwnerPreparedError> {
    let candidate = prepared.read(request.registration_id, request.revision)?;
    if crate::hex::lower_hex(&Sha256::digest(candidate.selection.as_bytes()))
        != request.selection_sha256
    {
        return Err(OwnerPreparedError::Invalid("selection digest differs"));
    }
    let selection = parse_saved(&candidate, fs::metadata(&sources.root)?.uid())?;
    let binding = candidate.binding.clone();
    let generated_work_max = u64::try_from(selection.bead_count())
        .map_err(|_| OwnerPreparedError::Invalid("too many Beads"))?;
    if request.generated_work_max != generated_work_max || request.park_ttl_ms != 3_600_000 {
        return Err(OwnerPreparedError::Invalid("Run accounting limits differ"));
    }
    let admission = RunAdmission {
        id: binding.run_id.clone(),
        generated_work_ceiling: generated_work_max,
        park_ttl_ms: request.park_ttl_ms,
    };
    sources.with_current_checkout_by(
        &binding.run_id,
        &binding.source_id,
        &binding.checkout_id,
        verify,
        |source, checkout| {
            if source.project_id != binding.project_id
                || !selection.matches_checkout(&checkout.checkout.path, &checkout.head)
            {
                return Err(OwnerPreparedError::Invalid("selection checkout differs"));
            }
            // Hold source, Run, then publication locks through the admission write.
            runs.admit_phone_bound_fenced(&admission, request.token, &binding, |commit| {
                prepared.with_published(request.registration_id, request.revision, |saved| {
                    if saved != &candidate {
                        return Err(OwnerPreparedError::Invalid("prepared revision changed"));
                    }
                    commit().map_err(OwnerPreparedError::from)
                })?
            })?;
            Ok(binding.run_id.clone())
        },
    )?
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
