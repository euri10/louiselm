//! Operator preview and exact acceptance for one verified Run Bead.

use louiselm_skills::{
    Digest,
    broker::{
        operator::{self, VerificationControlRequest, VerificationControlResponse},
        promotion::{PromotionRequest, SavedAcceptance, SavedResult, SavedSelection as Selection},
        verification::VerificationStatus,
    },
    workspace::promotion::{self, DestinationIdentity, PromotionClient},
};
use serde::{Deserialize, Serialize};
use std::{
    io::{self, Read as _, Write as _},
    path::Path,
    time::{SystemTime, UNIX_EPOCH},
};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Command {
    selection: Option<Selection>,
    result_id: Option<String>,
    approval_digest: Option<String>,
}

#[derive(Serialize)]
struct Preview<'a> {
    schema: &'static str,
    selection: &'a Selection,
    changes: &'a promotion::ChangePreview,
    approval_digest: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    tainted_review: Option<&'a louiselm_skills::broker::promotion::PromotionReview>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tainted_review_digest: Option<String>,
}

#[derive(Serialize)]
struct Committed {
    schema: &'static str,
    commit: String,
    bead_id: String,
}

fn now_ms() -> Result<u64, &'static str> {
    u64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| "clock unavailable")?
            .as_millis(),
    )
    .map_err(|_| "clock unavailable")
}

#[expect(
    clippy::too_many_lines,
    reason = "The preview and commit recheck one exact selection through the same broker transaction."
)]
fn operation(verb: &str, mut command: Command) -> Result<Vec<u8>, &'static str> {
    let paths = super::installed_paths().map_err(|_| "installed broker unavailable")?;
    let config = louiselm_skills::launcher_install::public_runtime_config(&paths)
        .map_err(|_| "installed broker unavailable")?;
    let (selection, saved) = match (command.selection.take(), command.result_id.take()) {
        (Some(selection), None) if verb == "save" => (selection, None),
        (None, Some(id)) if verb != "save" => {
            let saved = saved_result(config.broker_uid, &id, false)?;
            (saved.selection.clone(), Some(saved))
        }
        _ => return Err("select exactly one saved result or publication context"),
    };
    let selection = &selection;
    if selection.schema != "louiselm.run-promotion-selection/1"
        || selection.run_id.is_empty()
        || selection.bead_id.is_empty()
        || selection.bead_id.len() > 128
        || !selection.bead_id.bytes().all(|byte| {
            byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_' || byte == b'.'
        })
        || selection.producer_session_id == selection.verifier_session_id
        || (verb != "commit" && command.approval_digest.is_some())
        || (verb == "commit" && command.approval_digest.is_none())
    {
        return Err("invalid promotion selection");
    }
    if verb == "inspect" || verb == "discard" {
        let result = saved_result(config.broker_uid, &selection.request_id, verb == "discard")?;
        return serde_json::to_vec(&VerificationControlResponse::SavedResult {
            result: Box::new(result),
        })
        .map_err(|_| "saved result unavailable");
    }
    if let Some(saved) = &saved {
        if saved.discarded {
            return Err("saved result was explicitly discarded");
        }
        if let Some(accepted) = &saved.accepted {
            if verb != "commit"
                || command.approval_digest.as_deref() != Some(&accepted.approval_digest)
            {
                return Err("saved result already accepted; inspect its commit");
            }
            return serde_json::to_vec(&Committed {
                schema: "louiselm.run-promotion-commit/1",
                commit: accepted.commit.clone(),
                bead_id: selection.bead_id.clone(),
            })
            .map_err(|_| "saved commit unavailable");
        }
        if saved.application != louiselm_skills::broker::promotion::PromotionStatus::NotRequested {
            return Err(
                "prior application exists; inspect saved result and local journals before retry",
            );
        }
    }
    // Preview spends no application authority: its short transport is dropped.
    // Explicit Accept creates a new exact transaction, never renewing a prior one.
    let expires = now_ms()?.checked_add(240_000).ok_or("clock unavailable")?;
    if verb != "save" {
        let head = promotion::clean_run_head(&selection.checkout, &selection.run_id)
            .map_err(|_| "Run worktree is dirty or on the wrong branch")?;
        if head != selection.expected_head {
            return Err("Run worktree HEAD changed; refresh required");
        }
    }
    let status = operator::verification(
        Path::new(operator::SOCKET),
        config.broker_uid,
        &VerificationControlRequest::Status {
            session_id: selection.verifier_session_id.clone(),
        },
        operator::TIMEOUT,
    )
    .map_err(|_| "verification evidence unavailable")?;
    let VerificationControlResponse::Status {
        status: VerificationStatus::Completed(record),
        commands_passed: true,
    } = status
    else {
        return Err("verification did not pass");
    };
    if record.producer.request.launch.session_id != selection.producer_session_id
        || record.producer.request.launch.run_id != selection.run_id
        || record.execution.request.launch.run_id != selection.run_id
    {
        return Err("verification belongs to another Run");
    }
    if verb != "save" {
        let mut plan = Vec::new();
        std::fs::File::open(&selection.plan)
            .map_err(|_| "verification rules unavailable; refresh required")?
            .take(65_537)
            .read_to_end(&mut plan)
            .map_err(|_| "verification rules unavailable; refresh required")?;
        if plan.len() > 65_536 || Digest::of(&plan).to_string() != record.execution.job.plan_digest
        {
            return Err("verification rules changed; refresh required");
        }
        let inputs = louiselm_skills::launch_supervisor::verification_inputs::current()
            .map_err(|_| "verification tools unavailable; refresh required")?;
        if inputs.to_string() != record.execution.verifier_inputs_digest {
            return Err("verification tools changed; refresh required");
        }
    }
    let request = PromotionRequest {
        schema: "louiselm.workspace.promotion/1".into(),
        request_id: selection.request_id.clone(),
        producer_session_id: selection.producer_session_id.clone(),
        verifier_session_id: selection.verifier_session_id.clone(),
        verification_digest: Digest::of(
            &serde_json::to_vec(&record).map_err(|_| "invalid verification record")?,
        )
        .to_string(),
        job: record.execution.job.clone(),
        destination: DestinationIdentity::inspect(&selection.checkout)
            .map_err(|_| "Run worktree identity unavailable")?,
        expires_at_ms: expires,
    };
    let selected_request = request.clone();
    if verb == "save" {
        let response = operator::verification(
            Path::new(operator::SOCKET),
            config.broker_uid,
            &VerificationControlRequest::SaveResult {
                request,
                selection: Box::new(selection.clone()),
            },
            operator::TIMEOUT,
        )
        .map_err(|_| "result publication unconfirmed; preserve producer")?;
        let VerificationControlResponse::SavedResult { result } = response else {
            return Err("result publication unconfirmed; preserve producer");
        };
        return serde_json::to_vec(&serde_json::json!({
            "schema": "louiselm.run-saved-result/1", "result_id": result.request.request_id
        }))
        .map_err(|_| "saved result unavailable");
    }
    let stream = operator::promotion_stream(
        Path::new(operator::PROMOTION_SOCKET),
        config.broker_uid,
        &selection.request_id,
    )
    .map_err(|_| "promotion broker unavailable")?;
    let client = PromotionClient::prepare(
        stream,
        config.broker_uid,
        request,
        &selection.checkout,
        &selection.journal_parent,
    )
    .map_err(|_| "promotion preview refused")?;
    let digest = approval_digest(
        selection,
        &selected_request,
        client.preview(),
        client.tainted_review(),
    )
    .map_err(|_| "promotion preview invalid")?;
    if verb == "preview" {
        let reviewed = stable_review(&selected_request, client.tainted_review())
            .map_err(|_| "promotion review invalid")?;
        let review = reviewed
            .as_ref()
            .map(|value| value.digest().map_err(|_| "promotion review invalid"))
            .transpose()?;
        serde_json::to_vec(&Preview {
            schema: "louiselm.run-promotion-preview/1",
            selection,
            changes: client.preview(),
            approval_digest: digest,
            tainted_review: reviewed.as_ref(),
            tainted_review_digest: review,
        })
        .map_err(|_| "promotion preview invalid")
    } else {
        if command.approval_digest.as_deref() != Some(digest.as_str()) {
            return Err("promotion preview changed before acceptance");
        }
        let review = client
            .tainted_review()
            .map(|value| value.digest().map_err(|_| "promotion review invalid"))
            .transpose()?;
        let applied = match review {
            Some(review) => client.commit_tainted(&review),
            None => client.commit(),
        }
        .map_err(|_| "promotion effect uncertain; inspect journals")?;
        if !applied.complete {
            return Err("promotion effect uncertain; inspect journals");
        }
        let commit = promotion::commit_run(
            &selection.checkout,
            &selection.run_id,
            &selection.bead_id,
            &selection.expected_head,
            &record.execution.job.result_digest,
        )
        .map_err(|_| "promotion applied but Git commit failed; inspect worktree")?;
        operator::verification(
            Path::new(operator::SOCKET),
            config.broker_uid,
            &VerificationControlRequest::AcceptSavedResult {
                result_id: selection.request_id.clone(),
                accepted: SavedAcceptance {
                    commit: commit.clone(),
                    approval_digest: digest,
                    request_digest: Digest::of(
                        &serde_json::to_vec(&selected_request)
                            .map_err(|_| "invalid application record")?,
                    )
                    .to_string(),
                },
            },
            operator::TIMEOUT,
        )
        .map_err(|_| "Git commit created but saved decision unconfirmed; inspect before retry")?;
        serde_json::to_vec(&Committed {
            schema: "louiselm.run-promotion-commit/1",
            commit,
            bead_id: selection.bead_id.clone(),
        })
        .map_err(|_| "promotion result unavailable")
    }
}

fn approval_digest(
    selection: &Selection,
    request: &PromotionRequest,
    changes: &promotion::ChangePreview,
    review: Option<&louiselm_skills::broker::promotion::PromotionReview>,
) -> Result<String, serde_json::Error> {
    let review = stable_review(request, review)?;
    let mut request = request.clone();
    request.expires_at_ms = 0;
    Ok(Digest::of(&serde_json::to_vec(&(selection, request, changes, review))?).to_string())
}

fn stable_review(
    request: &PromotionRequest,
    review: Option<&louiselm_skills::broker::promotion::PromotionReview>,
) -> Result<Option<louiselm_skills::broker::promotion::PromotionReview>, serde_json::Error> {
    let mut request = request.clone();
    request.expires_at_ms = 0;
    review
        .map(|review| {
            let mut review = review.clone();
            review.request_digest = Digest::of(&serde_json::to_vec(&request)?).to_string();
            Ok(review)
        })
        .transpose()
}

fn saved_result(uid: u32, id: &str, discard: bool) -> Result<SavedResult, &'static str> {
    let response = operator::verification(
        Path::new(operator::SOCKET),
        uid,
        &VerificationControlRequest::SavedResult {
            result_id: id.into(),
            discard,
        },
        operator::TIMEOUT,
    )
    .map_err(|_| "saved result unavailable; inspect before retry")?;
    let VerificationControlResponse::SavedResult { result } = response else {
        return Err("saved result unavailable");
    };
    Ok(*result)
}

pub(super) fn cli(arguments: Result<&clap::ArgMatches, ()>) -> u8 {
    let result = (|| {
        let (verb, _) = super::arguments::operation(arguments)
            .map_err(|()| "expected promotion save|inspect|discard|preview|commit --json")?;
        let mut bytes = Vec::new();
        io::stdin()
            .take((louiselm_skills::launch_protocol::MAX_PROTOCOL_MESSAGE_BYTES + 1) as u64)
            .read_to_end(&mut bytes)
            .map_err(|_| "invalid promotion input")?;
        if bytes.is_empty()
            || bytes.len() > louiselm_skills::launch_protocol::MAX_PROTOCOL_MESSAGE_BYTES
        {
            return Err("invalid promotion input");
        }
        let command: Command =
            serde_json::from_slice(&bytes).map_err(|_| "invalid promotion input")?;
        operation(verb, command)
    })();
    match result {
        Ok(bytes) => match io::stdout().lock().write_all(&bytes) {
            Ok(()) => 0,
            Err(_) => 1,
        },
        Err(message) => {
            // Every operation error is a static, payload-free diagnostic.
            eprintln!(
                "{}",
                serde_json::json!({
                    "schema": "louiselm.run-promotion-error/1",
                    "message": message,
                })
            );
            1
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, reason = "Fixture failures abort tests.")]

    use super::*;
    use louiselm_skills::workspace::{provenance::OutputProvenance, verification::JobPreview};
    use std::path::PathBuf;

    #[test]
    fn approval_binds_selected_output_even_when_changed_paths_are_identical() {
        let hash = Digest::of(b"fixture").to_string();
        let selection = Selection {
            schema: "louiselm.run-promotion-selection/1".into(),
            run_id: "run-one".into(),
            bead_id: "bead-one".into(),
            producer_session_id: "worker-one".into(),
            verifier_session_id: "verifier-one".into(),
            request_id: "promote-one".into(),
            checkout: PathBuf::from("/worktree"),
            journal_parent: PathBuf::from("/journal"),
            expected_head: "a".repeat(40),
            plan: PathBuf::from("/plan.json"),
        };
        let mut request = PromotionRequest {
            schema: "louiselm.workspace.promotion/1".into(),
            request_id: selection.request_id.clone(),
            producer_session_id: selection.producer_session_id.clone(),
            verifier_session_id: selection.verifier_session_id.clone(),
            verification_digest: hash.clone(),
            job: JobPreview {
                schema: "louiselm.workspace.verification-preview/1".into(),
                state: "prepared".into(),
                job_digest: hash.clone(),
                snapshot_digest: hash.clone(),
                bundle_digest: hash.clone(),
                base_digest: hash.clone(),
                result_digest: hash.clone(),
                plan_digest: hash,
                output_provenance: OutputProvenance::unknown(),
                command_count: 1,
            },
            destination: DestinationIdentity {
                device: 1,
                inode: 2,
                uid: 1000,
            },
            expires_at_ms: 1_000_000,
        };
        let changes = promotion::ChangePreview {
            added: vec!["same-path".into()],
            modified: vec![],
            deleted: vec![],
        };
        let approved = approval_digest(&selection, &request, &changes, None).unwrap();
        request.expires_at_ms += 1;
        assert_eq!(
            approved,
            approval_digest(&selection, &request, &changes, None).unwrap(),
            "review approval binds the exact work, not a spent application lifetime"
        );
        request.job.result_digest = Digest::of(b"different-bytes").to_string();
        assert_ne!(
            approved,
            approval_digest(&selection, &request, &changes, None).unwrap()
        );
        request.job.result_digest = Digest::of(b"fixture").to_string();
        request.verification_digest = Digest::of(b"different-verification").to_string();
        assert_ne!(
            approved,
            approval_digest(&selection, &request, &changes, None).unwrap()
        );
    }
}
