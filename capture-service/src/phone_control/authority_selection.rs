//! Scoped selection read using current owner and project identities.

use std::{path::Path, time::Instant};

use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use super::super::{
    Frame, MAX_FRAME, Request, fixed,
    socket::{ControllerRegistry, validate_read_response},
    valid_string, verify_phone_control_frame,
};
use super::{Authority, ReadPermit, Refusal, StoredGrant, refusal};
use crate::{PairingRegistry, PhoneProjectError, PhoneProjectIdentity};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct OwnerChoices {
    v: u64,
    #[serde(rename = "type")]
    kind: String,
    controller_id: String,
    operation_id: String,
    agents: Vec<String>,
}

impl Authority {
    pub(in crate::phone_control) fn finish_selection(
        &self,
        permit: &ReadPermit,
        pairing: &PairingRegistry,
        credential: &str,
        controllers: &ControllerRegistry,
        response: Result<Vec<u8>, Refusal>,
    ) -> Result<Value, Refusal> {
        let now = Instant::now();
        let wall_ms = crate::time::now_ms();
        let started = Instant::now();
        let mut state = self.lock()?;
        let result = pairing.with_authenticated_device(credential, |device| {
            if device != Some(permit.device.as_str()) {
                return Err(Refusal::Revoked);
            }
            if Sha256::digest(&permit.frame).as_slice() != permit.frame_sha256 {
                return Err(Refusal::Unauthenticated);
            }
            verify_phone_control_frame(&permit.frame, &self.receiver_identity).map_err(refusal)?;
            let frame: Frame = serde_json::from_slice(&permit.frame).map_err(|_| Refusal::Malformed)?;
            let request = &frame.request;
            if request.action != 50
                || request.operation_id != permit.operation_id
                || request.grant_id != permit.grant_id
                || request.period_id != permit.period_id
            {
                return Err(Refusal::Unauthenticated);
            }
            let grant = state.grants.get(&permit.device).cloned().ok_or(Refusal::Revoked)?;
            grant.require_approved_scope()?;
            if grant.grant_id != permit.grant_id
                || grant.grant_revision != request.grant_revision
                || grant.enrolled_key != frame.grant.enrolled_key
                || grant.receiver_identity != self.receiver_identity
            {
                return Err(Refusal::Revoked);
            }
            if !grant.scope.groups.iter().any(|group| group == "read") {
                return Err(Refusal::OutOfScope);
            }
            if !controllers.contains(&request.controller_id) {
                return Err(Refusal::StaleSubject);
            }
            let bytes = response?;
            let agents = selection_agents(&bytes, request, &grant)?;
            let (projects, next_after) = self.current_projects(&grant, request)?;
            if !controllers.contains(&request.controller_id) {
                return Err(Refusal::StaleSubject);
            }
            self.finish_read_locked(&mut state, permit, true, now, wall_ms, started)?;
            Ok(json!({"v":1,"type":"selection_list","controller_id":request.controller_id,
                "operation_id":request.operation_id,"agents":agents,"projects":projects,
                "next_after":next_after,"can_create":grant.scope.groups.iter().any(|group| group == "operate_sessions")}))
        });
        if !matches!(&result, Ok(Ok(_))) {
            state.pending_reads.remove(&permit.operation_id);
        }
        result.map_err(|_| Refusal::Unavailable)?
    }

    fn current_projects(
        &self,
        grant: &StoredGrant,
        request: &Request,
    ) -> Result<(Vec<Value>, String), Refusal> {
        let projects = self.projects.as_ref().map_err(|_| Refusal::Unavailable)?;
        let payload = fixed::<17>(&request.payload).map_err(|_| Refusal::Malformed)?;
        let mut cursor = if payload[..16] == [0; 16] {
            String::new()
        } else {
            request.payload[..32].to_owned()
        };
        let limit = usize::from(payload[16]);
        let mut rows = Vec::new();
        loop {
            let page = projects
                .projects_after(&cursor)
                .map_err(|_| Refusal::Unavailable)?;
            for project in page.projects {
                if !self.current_selection_project(grant, &project)? {
                    continue;
                }
                if project.path.len() > 512 || !valid_string(&project.path) {
                    return Err(Refusal::Unavailable);
                }
                rows.push(json!({"id":project.id,"path":project.path}));
                if rows.len() > limit {
                    rows.pop();
                    let next = rows
                        .last()
                        .and_then(|row| row["id"].as_str())
                        .ok_or(Refusal::Unavailable)?
                        .to_owned();
                    return Ok((rows, next));
                }
            }
            match page.next_after {
                Some(next) => cursor = next,
                None => return Ok((rows, String::new())),
            }
        }
    }

    fn current_selection_project(
        &self,
        grant: &StoredGrant,
        project: &PhoneProjectIdentity,
    ) -> Result<bool, Refusal> {
        let projects = self.projects.as_ref().map_err(|_| Refusal::Unavailable)?;
        let mut authorized = false;
        if let Some(roots) = grant.project_roots()? {
            for (root, path) in roots.iter().zip(&grant.scope.projects) {
                if !project.root_ids.contains(root) {
                    continue;
                }
                match projects.check_current_root(root) {
                    Ok(current) if current == Path::new(path) => {
                        authorized = true;
                        break;
                    }
                    Ok(_) | Err(PhoneProjectError::Invalid("approved root identity changed")) => {}
                    Err(PhoneProjectError::Io(error))
                        if error.kind() == std::io::ErrorKind::NotFound => {}
                    Err(_) => return Err(Refusal::Unavailable),
                }
            }
        } else {
            authorized = grant.legacy_project_confirmed(&project.id)?
                && projects
                    .matches_current_parent(&project.id, &grant.scope.projects)
                    .map_err(|_| Refusal::Unavailable)?;
        }
        if !authorized {
            return Ok(false);
        }
        match projects.check_current_project(&project.id) {
            Ok(path) => Ok(path == Path::new(&project.path)),
            Err(PhoneProjectError::Invalid("project identity changed")) => Ok(false),
            Err(PhoneProjectError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {
                Ok(false)
            }
            Err(_) => Err(Refusal::Unavailable),
        }
    }
}

fn selection_agents(
    bytes: &[u8],
    request: &Request,
    grant: &StoredGrant,
) -> Result<Vec<String>, Refusal> {
    if bytes.len() >= MAX_FRAME {
        return Err(Refusal::Unavailable);
    }
    let agents: OwnerChoices = match serde_json::from_slice(bytes) {
        Ok(agents) => agents,
        Err(_) => return validate_read_response(bytes, request).and(Err(Refusal::Unavailable)),
    };
    if agents.v != 1
        || agents.kind != "selection_agents"
        || agents.controller_id != request.controller_id
        || agents.operation_id != request.operation_id
        || agents.agents.len() > 50
        || agents.agents.iter().any(|agent| {
            agent.is_empty()
                || agent.len() > 512
                || !valid_string(agent)
                || !grant.scope.agents.contains(agent)
        })
        || agents.agents.windows(2).any(|pair| pair[0] >= pair[1])
    {
        return Err(Refusal::Unavailable);
    }
    Ok(agents.agents)
}
