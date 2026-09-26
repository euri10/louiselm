//! Retention policy belongs to the broker; privileged mechanics only consume it.

use super::{AuthorizationStore, BrokerError, BrokerService, InstalledBroker};
use crate::{
    Digest,
    launch::LaunchRequest,
    workspace::{
        filesystem,
        provenance::OutputProvenance,
        retention::{EvidenceReferences, InputReferences, RetentionInspection, Store},
    },
};

impl AuthorizationStore {
    pub(super) fn retention_store(&self) -> Result<Store, BrokerError> {
        Ok(Store::lock(
            &self.root.join("workspace-retention"),
            (
                rustix::process::geteuid().as_raw(),
                rustix::process::getegid().as_raw(),
            ),
        )?)
    }
}

impl BrokerService {
    pub(super) fn retain_workspace_until(
        &self,
        id: &str,
        expires_at_ms: u64,
    ) -> Result<(), BrokerError> {
        let mut store = self.authorizations().retention_store()?;
        let mut record = store.read(id)?;
        if record.primary_evidence != crate::workspace::retention::PrimaryEvidence::NotChecked {
            return Err(BrokerError::InvalidGrant);
        }
        record.expires_at_ms = record.expires_at_ms.max(expires_at_ms);
        store.write(&record)?;
        Ok(())
    }
    /// Reads durable workspace policy/evidence or changes its explicit retention pin.
    /// The caller is the authenticated operator; pinning grants no execution,
    /// recovery, verification or promotion authority. Blocking broker-worker API.
    /// # Errors
    /// Refuses foreign controllers, unknown Sessions, unavailable/corrupt state,
    /// busy cleanup, or pin changes after primary deletion started.
    pub fn workspace_retention(
        &self,
        operator_uid: u32,
        session_id: &str,
        pin: Option<bool>,
    ) -> Result<RetentionInspection, BrokerError> {
        let authorization = self
            .authorizations()
            .consumed_for_session(session_id)?
            .ok_or(BrokerError::UnknownAuthorization)?;
        if authorization.controller_uid != operator_uid {
            return Err(BrokerError::ControllerMismatch);
        }
        let mut store = self.authorizations().retention_store()?;
        let record = store.read(session_id)?;
        if record.launch.digest().to_string() != authorization.request_digest {
            return Err(BrokerError::RequestMismatch);
        }
        let record = if let Some(pin) = pin {
            store.pin(session_id, pin)?
        } else {
            record
        };
        Ok(RetentionInspection {
            output_provenance: self.workspace_output_provenance(&record.launch)?,
            record,
            quarantined: self.lifecycle.is_quarantined(session_id)?,
        })
    }

    pub(super) fn workspace_output_provenance(
        &self,
        launch: &LaunchRequest,
    ) -> Result<OutputProvenance, BrokerError> {
        let authorization = self
            .authorizations()
            .consumed_for_session(&launch.session_id)?
            .ok_or(BrokerError::UnknownAuthorization)?;
        if authorization.request_digest != launch.digest().to_string() {
            return Err(BrokerError::RequestMismatch);
        }
        let quarantined = self.lifecycle.is_quarantined(&launch.session_id)?;
        let taint = self.lifecycle.skill_taint(&launch.session_id)?;
        if let Some(taint) = taint {
            if taint.run_id() != launch.run_id || taint.generation() != launch.skill_generation_id {
                return Err(super::corrupt(
                    "Session output taint conflicts with workspace launch",
                ));
            }
            Ok(OutputProvenance::tainted(taint.digest()))
        } else if quarantined {
            Ok(OutputProvenance::unknown())
        } else {
            Ok(OutputProvenance::untainted())
        }
    }

    pub(super) fn workspace_output_provenance_for_session(
        &self,
        session_id: &str,
    ) -> Result<OutputProvenance, BrokerError> {
        let record = self.authorizations().retention_store()?.read(session_id)?;
        self.workspace_output_provenance(&record.launch)
    }

    pub(super) fn retain_workspace_inputs(
        &self,
        request: &LaunchRequest,
    ) -> Result<(), BrokerError> {
        let digest = Digest::parse(&request.session_input_manifest_id)
            .map_err(|_| BrokerError::InvalidGrant)?;
        let input = self
            .verification_inputs
            .parent()
            .ok_or(BrokerError::InvalidGrant)?
            .join("workspace-inputs")
            .join(digest.hex());
        // Non-installed protocol fixtures can have no workspace. Real launch
        // refuses absent staged inputs at the privileged preparation boundary.
        match std::fs::symlink_metadata(&input) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(BrokerError::Storage(error)),
            Ok(_) => (),
        }
        let root = filesystem::open_directory(&input)?;
        let bytes = filesystem::read_source(&root, "manifest.json", 1024 * 1024)?
            .ok_or(BrokerError::InvalidGrant)?;
        let manifest = crate::session_manifest::SessionInputManifest::parse(&bytes.bytes)
            .map_err(|_| BrokerError::InvalidGrant)?;
        if manifest.digest() != digest {
            return Err(BrokerError::RequestMismatch);
        }
        let references = InputReferences::from_manifest(&manifest)?;
        self.retain_workspace_reference(&request.session_id, |evidence| {
            evidence.inputs = Some(references);
        })
    }

    pub(super) fn retain_workspace_reference(
        &self,
        id: &str,
        update: impl FnOnce(&mut EvidenceReferences),
    ) -> Result<(), BrokerError> {
        let mut store = self.authorizations().retention_store()?;
        let mut record = store.read(id)?;
        update(&mut record.evidence);
        store.write(&record)?;
        Ok(())
    }
}

impl InstalledBroker {
    /// Authenticated operator inspection/pinning, including after supervisor exit.
    /// Historical references remain inspectable under quarantine and never grant
    /// recovery or promotion authority. Run on the broker I/O worker.
    /// # Errors
    /// Refuses wrong operator, missing/corrupt state, lock contention or late pins.
    pub fn workspace_retention(
        &self,
        operator_uid: u32,
        session_id: &str,
        pin: Option<bool>,
    ) -> Result<RetentionInspection, BrokerError> {
        self.service
            .workspace_retention(operator_uid, session_id, pin)
    }
}
