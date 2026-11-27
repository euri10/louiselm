//! Admission of capture-owned voice mutations under the current phone grant.

use std::{
    path::{Component, Path},
    time::Instant,
};

use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use super::super::{
    Frame, verify_phone_control_frame,
    voice_job::{VoiceJobCommand, VoiceOwnerScope, voice_job_command, voice_source_digest},
};
use super::{Authority, ReadPermit, Refusal, State, StoredGrant, refusal};
use crate::{CaptureDraft, CaptureSource, PairingRegistry, VoiceArchive, VoiceAssociation};

impl Authority {
    pub(in crate::phone_control) fn finish_voice_mutation(
        &self,
        permit: &ReadPermit,
        pairing: &PairingRegistry,
        credential: &str,
        archive: &VoiceArchive,
    ) -> Result<Value, Refusal> {
        let now = Instant::now();
        let wall_ms = crate::time::now_ms();
        let started = Instant::now();
        let mut state = self.lock()?;
        // Grant revocation and pairing revocation serialize before archive mutation.
        let result = pairing.with_authenticated_device(credential, |device| {
            if device != Some(permit.device.as_str()) { return Err(Refusal::Revoked); }
            let (frame, grant) = self.check_voice_request(&mut state, permit, now, started)?;
            let request = &frame.request;
            let command = voice_job_command(request).map_err(refusal)?;
            let id = match &command {
                VoiceJobCommand::Cancel { id } | VoiceJobCommand::Retry { id, .. } => id,
                VoiceJobCommand::Admit { .. } => return Err(Refusal::Unsupported),
            };
            let association = archive.job(id).map_err(|_| Refusal::Unavailable)?.association;
            if association.controller != request.controller_id || association.subject != request.subject_id {
                return Err(Refusal::OutOfScope);
            }
            self.check_voice_job_scope(&grant, &association)?;
            // Recheck immediately before the durable operation while both locks remain held.
            let action_now = Instant::now();
            self.check_pending_read_locked(&mut state, permit, action_now, action_now)?;
            match &command {
                VoiceJobCommand::Cancel { .. } => archive.cancel(id).map_err(|_| Refusal::Unavailable)?,
                VoiceJobCommand::Retry { retry_id, .. } => archive.retry(id, retry_id).map_err(|_| Refusal::Unavailable)?,
                VoiceJobCommand::Admit { .. } => return Err(Refusal::Unsupported),
            }
            let job = archive.job(id).map_err(|_| Refusal::Unavailable)?;
            self.finish_read_locked(&mut state, permit, true, now, wall_ms, started)?;
            Ok(json!({"v":1,"type":"voice_job","operation_id":request.operation_id,"id":id,"state":job.status}))
        });
        if !matches!(&result, Ok(Ok(_))) {
            state.pending_reads.remove(&permit.operation_id);
        }
        result.map_err(|_| Refusal::Unavailable)?
    }

    pub(in crate::phone_control) fn finish_voice_admission(
        &self,
        permit: &ReadPermit,
        pairing: &PairingRegistry,
        credential: &str,
        archive: &VoiceArchive,
        scope: VoiceOwnerScope,
        controller_attached: impl FnOnce() -> bool,
    ) -> Result<Value, Refusal> {
        let now = Instant::now();
        let wall_ms = crate::time::now_ms();
        let started = Instant::now();
        let mut state = self.lock()?;
        let result = pairing.with_authenticated_device(credential, |device| {
            if device != Some(permit.device.as_str()) { return Err(Refusal::Revoked); }
            let (frame, grant) = self.check_voice_request(&mut state, permit, now, started)?;
            let request = &frame.request;
            let VoiceJobCommand::Admit { id, digest, recorded_at_ms, duration_ms, bytes, draft, revision, source_digest } =
                voice_job_command(request).map_err(refusal)? else { return Err(Refusal::Unsupported); };
            if voice_source_digest(&scope.project, &scope.agent).map_err(refusal)? != source_digest {
                return Err(Refusal::OutOfScope);
            }
            let association = VoiceAssociation {
                receiver: self.receiver_identity.clone(), controller: request.controller_id.clone(),
                subject: request.subject_id.clone(), project: scope.project, agent: scope.agent, draft, revision,
            };
            self.check_voice_job_scope(&grant, &association)?;
            let action_now = Instant::now();
            self.check_pending_read_locked(&mut state, permit, action_now, action_now)?;
            if !controller_attached() { return Err(Refusal::StaleSubject); }
            let capture = CaptureDraft { id: id.clone(), source: CaptureSource::Android,
                recorded_at_ms, duration_ms, mime_type: "audio/mp4".into() };
            archive.publish_staged(&capture, &association, &digest, bytes).map_err(|_| Refusal::Unavailable)?;
            let job = archive.job(&id).map_err(|_| Refusal::Unavailable)?;
            self.finish_read_locked(&mut state, permit, true, now, wall_ms, started)?;
            Ok(json!({"v":1,"type":"voice_job","operation_id":request.operation_id,"id":id,"state":job.status}))
        });
        if !matches!(&result, Ok(Ok(_))) {
            state.pending_reads.remove(&permit.operation_id);
        }
        result.map_err(|_| Refusal::Unavailable)?
    }

    pub(in crate::phone_control) fn discard_voice_permit(
        &self,
        permit: &ReadPermit,
    ) -> Result<(), Refusal> {
        self.lock()?.pending_reads.remove(&permit.operation_id);
        Ok(())
    }

    pub(super) fn check_voice_request(
        &self,
        state: &mut State,
        permit: &ReadPermit,
        now: Instant,
        started: Instant,
    ) -> Result<(Frame, StoredGrant), Refusal> {
        self.check_pending_read_locked(state, permit, now, started)?;
        if Sha256::digest(&permit.frame).as_slice() != permit.frame_sha256 {
            return Err(Refusal::Unauthenticated);
        }
        verify_phone_control_frame(&permit.frame, &self.receiver_identity).map_err(refusal)?;
        let frame: Frame = serde_json::from_slice(&permit.frame).map_err(|_| Refusal::Malformed)?;
        let request = &frame.request;
        if request.operation_id != permit.operation_id
            || request.grant_id != permit.grant_id
            || request.period_id != permit.period_id
        {
            return Err(Refusal::Unauthenticated);
        }
        let grant = state
            .grants
            .get(&permit.device)
            .cloned()
            .ok_or(Refusal::Revoked)?;
        grant.require_approved_scope()?;
        if grant.grant_id != permit.grant_id
            || grant.grant_revision != request.grant_revision
            || grant.enrolled_key != frame.grant.enrolled_key
            || grant.receiver_identity != self.receiver_identity
        {
            return Err(Refusal::Revoked);
        }
        if !grant
            .scope
            .groups
            .iter()
            .any(|group| group == "operate_sessions")
        {
            return Err(Refusal::OutOfScope);
        }
        Ok((frame, grant))
    }

    fn check_voice_job_scope(
        &self,
        grant: &StoredGrant,
        association: &VoiceAssociation,
    ) -> Result<(), Refusal> {
        if association.receiver != self.receiver_identity {
            return Err(Refusal::OutOfScope);
        }
        self.check_owner_scope(grant, Path::new(&association.project), &association.agent)
    }

    pub(super) fn check_owner_scope(
        &self,
        grant: &StoredGrant,
        project: &Path,
        agent: &str,
    ) -> Result<(), Refusal> {
        let roots = grant.project_roots()?.ok_or(Refusal::Unavailable)?;
        let projects = self.projects.as_ref().map_err(|_| Refusal::Unavailable)?;
        if roots.len() != grant.scope.projects.len() {
            return Err(Refusal::Unavailable);
        }
        for (id, path) in roots.iter().zip(&grant.scope.projects) {
            if projects
                .check_current_root(id)
                .map_err(|_| Refusal::Unavailable)?
                != Path::new(path)
            {
                return Err(Refusal::Unavailable);
            }
        }
        if !project.is_absolute()
            || project
                .components()
                .any(|part| matches!(part, Component::ParentDir | Component::Prefix(_)))
            || !grant.scope.agents.iter().any(|allowed| allowed == agent)
            || !grant
                .scope
                .projects
                .iter()
                .any(|root| project.starts_with(root))
        {
            return Err(Refusal::OutOfScope);
        }
        Ok(())
    }
}
