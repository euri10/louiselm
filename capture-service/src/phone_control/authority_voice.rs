//! Current grant and pairing checks for retained voice reads.

use std::{
    path::{Component, Path},
    time::Instant,
};

use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use super::super::{Frame, VoiceQuery, verify_phone_control_frame, voice_query};
use super::{Authority, ReadPermit, Refusal, refusal};
use crate::{PairingRegistry, VoiceArchive, VoiceAssociation};

impl Authority {
    pub(in crate::phone_control) fn finish_voice_delivery(
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
        // Authority -> pairing -> archive; archive operations never acquire authority.
        let result = pairing.with_authenticated_device(credential, |device| {
            if device != Some(permit.device.as_str()) { return Err(Refusal::Revoked); }
            if Sha256::digest(&permit.frame).as_slice() != permit.frame_sha256 {
                return Err(Refusal::Unauthenticated);
            }
            verify_phone_control_frame(&permit.frame, &self.receiver_identity).map_err(refusal)?;
            let frame: Frame = serde_json::from_slice(&permit.frame).map_err(|_| Refusal::Malformed)?;
            let request = &frame.request;
            if request.operation_id != permit.operation_id || request.grant_id != permit.grant_id ||
                request.period_id != permit.period_id { return Err(Refusal::Unauthenticated); }
            let grant = state.grants.get(&permit.device).cloned().ok_or(Refusal::Revoked)?;
            grant.require_approved_scope()?;
            if grant.grant_id != permit.grant_id || grant.grant_revision != request.grant_revision ||
                grant.enrolled_key != frame.grant.enrolled_key || grant.receiver_identity != self.receiver_identity {
                return Err(Refusal::Revoked);
            }
            if !grant.scope.groups.iter().any(|group| group == "read") { return Err(Refusal::OutOfScope); }
            let roots = grant.project_roots()?.ok_or(Refusal::Unavailable)?;
            let projects = self.projects.as_ref().map_err(|_| Refusal::Unavailable)?;
            if roots.len() != grant.scope.projects.len() { return Err(Refusal::Unavailable); }
            for (id, path) in roots.iter().zip(&grant.scope.projects) {
                if projects.check_current_root(id).map_err(|_| Refusal::Unavailable)? != Path::new(path) {
                    return Err(Refusal::Unavailable);
                }
            }
            let allowed = |association: &VoiceAssociation| {
                let path = Path::new(&association.project);
                association.receiver == self.receiver_identity && path.is_absolute() &&
                    !path.components().any(|part| matches!(part, Component::ParentDir | Component::Prefix(_))) &&
                    grant.scope.agents.contains(&association.agent) &&
                    grant.scope.projects.iter().any(|root| path.starts_with(root))
            };
            let response = match voice_query(request).map_err(refusal)? {
                VoiceQuery::History { after, limit } => {
                    // Twenty maximum-length, JSON-escaped rows fit the phone's 256 KiB reply limit.
                    let page = archive.page_after(&after, limit.min(20), allowed).map_err(|_| Refusal::Unavailable)?;
                    let records: Vec<_> = page.records.into_iter().map(|row| json!({
                        "id": row.id, "controller": row.association.controller, "subject": row.association.subject,
                        "project": row.association.project, "agent": row.association.agent, "has_result": row.has_result,
                        "state": row.state, "failure": row.failure
                    })).collect();
                    json!({"v":1, "type":"voice_history", "operation_id":request.operation_id,
                        "records":records, "next_after":page.next_after.unwrap_or_default()})
                }
                VoiceQuery::Result { id, offset } => {
                    let job = archive.job(&id).map_err(|_| Refusal::Unavailable)?;
                    let association = job.association;
                    if !allowed(&association) || association.controller != request.controller_id ||
                        association.subject != request.subject_id { return Err(Refusal::OutOfScope); }
                    let transcript = archive.transcript(&id).map_err(|_| Refusal::Unavailable)?;
                    if offset > transcript.text.len() || !transcript.text.is_char_boundary(offset) {
                        return Err(Refusal::Malformed);
                    }
                    let mut end = offset.saturating_add(32 * 1024).min(transcript.text.len());
                    while !transcript.text.is_char_boundary(end) { end -= 1; }
                    let next = if end == transcript.text.len() { 0 } else { end };
                    json!({"v":1, "type":"voice_result", "operation_id":request.operation_id, "id":id,
                        "receiver":association.receiver, "controller":association.controller, "subject":association.subject,
                        "project":association.project, "agent":association.agent, "draft":association.draft,
                        "revision":association.revision, "offset":offset, "next_offset":next, "total_bytes":transcript.text.len(),
                        "text":&transcript.text[offset..end]})
                }
            };
            self.finish_read_locked(&mut state, permit, true, now, wall_ms, started)?;
            Ok(response)
        });
        if !matches!(&result, Ok(Ok(_))) {
            state.pending_reads.remove(&permit.operation_id);
        }
        result.map_err(|_| Refusal::Unavailable)?
    }
}
