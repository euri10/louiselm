//! Promotion effects remain truthful across lost acknowledgements and broker restart.

use super::*;
use louiselm_skills::{
    broker::promotion::{PromotionRequest, PromotionStatus},
    workspace::{
        promotion::{ApplicationResult, DestinationIdentity},
        provenance::OutputProvenanceCode,
        verification::JobPreview,
    },
};

pub(super) fn selection() -> PromotionRequest {
    let digest = Digest::of(b"fixture").to_string();
    PromotionRequest {
        schema: "louiselm.workspace.promotion/1".into(),
        request_id: "promotion".into(),
        producer_session_id: "producer".into(),
        verifier_session_id: "verifier".into(),
        verification_digest: digest.clone(),
        job: JobPreview {
            schema: "louiselm.workspace.verification-preview/1".into(),
            state: "prepared".into(),
            job_digest: digest.clone(),
            snapshot_digest: digest.clone(),
            base_digest: digest.clone(),
            bundle_digest: digest.clone(),
            result_digest: digest.clone(),
            plan_digest: digest,
            output_provenance: louiselm_skills::workspace::provenance::OutputProvenance::unknown(),
            command_count: 1,
        },
        destination: DestinationIdentity {
            device: 1,
            inode: 2,
            uid: 1000,
        },
        expires_at_ms: 30000,
    }
}

#[test]
fn interrupted_effects_survive_restart_without_manufactured_completion() {
    let root = TempDir::new().unwrap();
    let service = verification::reopen(root.path(), "first.sock");
    assert_eq!(
        service.promotion_status("promotion").unwrap(),
        PromotionStatus::NotRequested
    );
    let directory = root.path().join("authorizations/promotions/promotion");
    fs::create_dir_all(&directory).unwrap();
    fs::write(
        directory.join("request.json"),
        serde_json::to_vec(&selection()).unwrap(),
    )
    .unwrap();
    fs::write(directory.join("0.grant"), b"0").unwrap();
    assert!(matches!(
        service.promotion_status("promotion").unwrap(),
        PromotionStatus::Unknown {
            granted_steps: 1,
            completed_steps: 0,
            output_provenance
        } if output_provenance.code == OutputProvenanceCode::Unknown
    ));
    drop(service);
    let restarted = verification::reopen(root.path(), "second.sock");
    assert!(matches!(
        restarted.promotion_status("promotion").unwrap(),
        PromotionStatus::Unknown {
            granted_steps: 1,
            completed_steps: 0,
            output_provenance
        } if output_provenance.code == OutputProvenanceCode::Unknown
    ));
    fs::write(directory.join("0.done"), b"0").unwrap();
    fs::write(directory.join("1.grant"), b"1").unwrap();
    assert!(matches!(
        restarted.promotion_status("promotion").unwrap(),
        PromotionStatus::Unknown {
            granted_steps: 2,
            completed_steps: 1,
            output_provenance
        } if output_provenance.code == OutputProvenanceCode::Unknown
    ));
    assert!(!directory.join("result.json").exists());
}

#[test]
fn only_complete_matching_observations_can_be_reported_as_complete() {
    let root = TempDir::new().unwrap();
    let service = verification::reopen(root.path(), "broker.sock");
    let directory = root.path().join("authorizations/promotions/promotion");
    fs::create_dir_all(&directory).unwrap();
    fs::write(
        directory.join("request.json"),
        serde_json::to_vec(&selection()).unwrap(),
    )
    .unwrap();
    fs::write(directory.join("0.grant"), b"0").unwrap();
    let result = ApplicationResult {
        complete: true,
        completed_steps: 1,
        output_provenance: serde_json::from_value(serde_json::json!({
            "schema": "louiselm.workspace.output-provenance/1",
            "code": "untainted",
            "taint_digest": null,
            "clean_review_refs": []
        }))
        .unwrap(),
    };
    fs::write(
        directory.join("result.json"),
        serde_json::to_vec(&result).unwrap(),
    )
    .unwrap();
    assert!(
        service.promotion_status("promotion").is_err(),
        "missing effect acknowledgement cannot pass"
    );
    fs::write(directory.join("0.done"), b"0").unwrap();
    assert!(matches!(
        service.promotion_status("promotion").unwrap(),
        PromotionStatus::Completed { result: actual, output_provenance }
            if actual == result && output_provenance.code == OutputProvenanceCode::Unknown
    ));
    fs::write(directory.join("unexpected.json"), b"{}").unwrap();
    assert!(service.promotion_status("promotion").is_err());
    fs::remove_file(directory.join("unexpected.json")).unwrap();
    fs::write(directory.join("0.done"), b"1").unwrap();
    assert!(service.promotion_status("promotion").is_err());
}

#[test]
fn a_request_cannot_claim_its_own_job_is_clean() {
    let root = TempDir::new().unwrap();
    let service = verification::reopen(root.path(), "broker.sock");
    let directory = root.path().join("authorizations/promotions/promotion");
    fs::create_dir_all(&directory).unwrap();
    let mut request = selection();
    request.job.output_provenance.code = OutputProvenanceCode::Untainted;
    fs::write(
        directory.join("request.json"),
        serde_json::to_vec(&request).unwrap(),
    )
    .unwrap();
    assert!(service.promotion_status("promotion").is_err());
}
