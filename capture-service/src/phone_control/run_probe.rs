//! Bounded observations from exact, currently connected Run controllers.

use std::{collections::HashSet, sync::Arc};

use serde::Deserialize;
use serde_json::{Value, json};
use tokio::task::JoinSet;

use super::{MAX_FRAME, MAX_INTEGER, fixed, socket::ControllerRegistry};

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Reply {
    v: u8,
    #[serde(rename = "type")]
    kind: String,
    controller_id: String,
    run_id: String,
    state: String,
    run: Option<LiveRun>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct LiveRun {
    pub(super) run_id: String,
    pub(super) owner_id: String,
    pub(super) registration_id: String,
    pub(super) registration_revision: u64,
    pub(super) admitted: bool,
    pub(super) status: String,
    pub(super) phase: String,
    pub(super) bead_index: u64,
    pub(super) bead_total: u64,
    pub(super) bead_id: String,
    pub(super) sessions: Vec<LiveSession>,
    pub(super) sessions_truncated: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct LiveSession {
    pub(super) subject_id: String,
    pub(super) agent: String,
    pub(super) status: String,
}

pub(super) enum Observation {
    Absent,
    Unreachable,
    Connected {
        controller: String,
        run: Box<LiveRun>,
    },
}

pub(super) struct Probe {
    /// None means the controller set could not be bounded, so no absence or
    /// individual owner is asserted.
    checked: Option<Vec<String>>,
    pub(super) observation: Observation,
}

impl Probe {
    pub(super) fn current(&self, controllers: &ControllerRegistry) -> bool {
        self.checked.as_ref().is_none_or(|expected| {
            controllers
                .list_after("", 51)
                .is_ok_and(|now| &now == expected)
        })
    }
}

/// Probe every attached connection, or report uncertainty instead of guessing.
pub(super) async fn collect(controllers: Arc<ControllerRegistry>, run_id: &str) -> Probe {
    let ids = match controllers.list_after("", 51) {
        Ok(ids) if ids.len() <= 50 => ids,
        _ => {
            return Probe {
                checked: None,
                observation: Observation::Unreachable,
            };
        }
    };
    let mut tasks = JoinSet::new();
    for id in &ids {
        let id = id.clone();
        let controllers = controllers.clone();
        let frame = json!({"v":1,"type":"run_probe","controller_id":id,"run_id":run_id})
            .to_string()
            .into_bytes();
        tasks.spawn(async move {
            let response = controllers.dispatch(&id, frame).await;
            (id, response)
        });
    }
    let mut found = None;
    let mut uncertain = false;
    while let Some(result) = tasks.join_next().await {
        match result {
            Ok((id, Ok(bytes))) => match validate(&bytes, &id, run_id) {
                Ok(Some(run)) if found.is_none() => found = Some((id, run)),
                Ok(Some(_)) | Err(()) => uncertain = true,
                Ok(None) => {}
            },
            _ => uncertain = true,
        }
    }
    if !uncertain && let Some((id, first)) = found.take() {
        let frame = json!({"v":1,"type":"run_probe","controller_id":id,"run_id":run_id})
            .to_string()
            .into_bytes();
        match controllers.dispatch(&id, frame).await {
            Ok(bytes) => match validate(&bytes, &id, run_id) {
                Ok(Some(current))
                    if current.owner_id == first.owner_id
                        && current.registration_id == first.registration_id
                        && current.registration_revision == first.registration_revision =>
                {
                    found = Some((id, current));
                }
                _ => uncertain = true,
            },
            Err(_) => uncertain = true,
        }
    }
    let observation = if uncertain {
        Observation::Unreachable
    } else if let Some((controller, run)) = found {
        Observation::Connected {
            controller,
            run: Box::new(run),
        }
    } else {
        Observation::Absent
    };
    Probe {
        checked: Some(ids),
        observation,
    }
}

fn validate(bytes: &[u8], controller: &str, run_id: &str) -> Result<Option<LiveRun>, ()> {
    if bytes.len() >= MAX_FRAME {
        return Err(());
    }
    let reply: Reply = serde_json::from_slice(bytes).map_err(|_| ())?;
    if reply.v != 1
        || reply.kind != "run_probe"
        || reply.controller_id != controller
        || reply.run_id != run_id
    {
        return Err(());
    }
    match (reply.state.as_str(), reply.run) {
        ("absent", None) => Ok(None),
        ("found", Some(run)) if valid_run(&run, run_id) => Ok(Some(run)),
        _ => Err(()),
    }
}

fn valid_run(run: &LiveRun, run_id: &str) -> bool {
    let valid_id = |id: &str| fixed::<16>(id).is_ok_and(|bytes| bytes != [0; 16]);
    if run.run_id != run_id
        || !valid_id(&run.owner_id)
        || !valid_id(&run.registration_id)
        || !(1..=MAX_INTEGER).contains(&run.registration_revision)
        || !matches!(
            run.status.as_str(),
            "new" | "active" | "held" | "completed" | "failed"
        )
        || !(1..=4096).contains(&run.bead_total)
        || run.sessions.len() > 50
        || (!run.admitted && !run.sessions.is_empty())
        || (run.admitted && run.status == "new")
    {
        return false;
    }
    if run.phase == "none" {
        if run.bead_index != 0 || !run.bead_id.is_empty() {
            return false;
        }
    } else if run.status != "active"
        || !matches!(
            run.phase.as_str(),
            "authorizing"
                | "preparing"
                | "working"
                | "verifying"
                | "awaiting_promotion"
                | "promoting"
        )
        || !(1..=run.bead_total).contains(&run.bead_index)
        || run.bead_id.is_empty()
        || run.bead_id.len() > 512
        || run.bead_id.contains('\0')
    {
        return false;
    }
    let mut subjects = HashSet::new();
    run.sessions.iter().all(|session| {
        valid_id(&session.subject_id)
            && subjects.insert(&session.subject_id)
            && !session.agent.is_empty()
            && session.agent.len() <= 512
            && !session.agent.contains('\0')
            && matches!(
                session.status.as_str(),
                "starting"
                    | "ready"
                    | "admitting"
                    | "configuring"
                    | "preparing"
                    | "prompting"
                    | "running"
                    | "waiting_permission"
                    | "cancelling"
                    | "error"
            )
    })
}

pub(super) fn project(controller: &str, run: &LiveRun) -> Value {
    let sessions: Vec<_> = run
        .sessions
        .iter()
        .map(|session| {
            json!({"subject_id":session.subject_id,
        "agent":session.agent,"status":session.status})
        })
        .collect();
    json!({"controller_id":controller,"owner_id":run.owner_id,"status":run.status,"phase":run.phase,
        "bead_index":run.bead_index,"bead_total":run.bead_total,"bead_id":run.bead_id,
        "sessions":sessions,"sessions_truncated":run.sessions_truncated})
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, reason = "fixture assertions")]
    use super::validate;
    use serde_json::json;

    #[test]
    fn rejects_duplicate_unknown_or_inconsistent_live_identities() {
        let controller = "11".repeat(16);
        let run_id = "11111111-1111-4111-8111-111111111111";
        let mut reply = json!({"v":1,"type":"run_probe","controller_id":controller,"run_id":run_id,
            "state":"found","run":{"run_id":run_id,"owner_id":"44".repeat(16),"registration_id":"22".repeat(16),
            "registration_revision":1,"admitted":true,"status":"active","phase":"working",
            "bead_index":1,"bead_total":1,"bead_id":"b-1","sessions":[{"subject_id":"33".repeat(16),
            "agent":"codex","status":"ready"}],"sessions_truncated":false}});
        assert!(
            validate(reply.to_string().as_bytes(), &controller, run_id)
                .unwrap()
                .is_some()
        );
        let duplicate = reply["run"]["sessions"][0].clone();
        reply["run"]["sessions"]
            .as_array_mut()
            .unwrap()
            .push(duplicate);
        assert!(validate(reply.to_string().as_bytes(), &controller, run_id).is_err());
        reply["run"]["sessions"] = json!([]);
        reply["run"]["unexpected"] = json!(true);
        assert!(validate(reply.to_string().as_bytes(), &controller, run_id).is_err());
        reply["run"].as_object_mut().unwrap().remove("unexpected");
        reply["run"]["registration_revision"] = json!(0);
        assert!(validate(reply.to_string().as_bytes(), &controller, run_id).is_err());
    }
}
