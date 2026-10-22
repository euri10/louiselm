//! Actual installed root CLI, exact broker peer and real reboot fixture history.
use super::*;
use crate::{
    launch_receipt::{ReceiptAuthority, ReceiptCause, ReceiptOutcome, SessionState, SignedReceipt},
    launch_recovery::{Inspection, InspectionOutcome, Proof},
    launcher_install::LauncherSigner,
};

fn command(paths: &LauncherPaths, operator: u32, id: &str) -> std::process::Output {
    Command::new(paths.release_prefix.join("current/bin/louiselm-launch"))
        .args(["recover-inspect", id])
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("SUDO_UID", operator.to_string())
        .stdin(Stdio::null())
        .output()
        .unwrap()
}

fn inspect(paths: &LauncherPaths, config: &LauncherConfig, id: &str) -> Inspection {
    let output = command(paths, config.operator_uid, id);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(output.stderr, [] as [u8; 0]);
    let inspection: Inspection = serde_json::from_slice(&output.stdout).unwrap();
    inspection.validate().unwrap();
    assert_eq!(inspection.binding.authorization.authorization_id, id);
    inspection
}

pub(super) fn before_arm(paths: &LauncherPaths, config: &LauncherConfig) {
    let directory = paths.state_root.join("consumed-recovery");
    assert!(!directory.exists());
    let inspection = inspect(paths, config, "approval");
    assert_eq!(inspection.outcome, InspectionOutcome::Unrecovered {});
    assert!(inspection.binding.receipt_head.is_none());
    assert!(inspection.receipt_state.is_none());
    assert!(
        !directory.exists(),
        "inspection must not create an operation or arm"
    );
    let foreign = command(paths, config.operator_uid + 1, "approval");
    assert!(!foreign.status.success());
    assert_eq!(foreign.stdout, [] as [u8; 0]);
    assert_eq!(
        String::from_utf8(foreign.stderr).unwrap().trim(),
        "louiselm-launch: launcher invocation is not the installed operator"
    );
    let unknown = command(paths, config.operator_uid, "unknown");
    assert!(!unknown.status.success());
    assert_eq!(unknown.stdout, [] as [u8; 0]);
    assert_eq!(
        String::from_utf8(unknown.stderr).unwrap().trim(),
        "louiselm-launch: unknown consumed authorization; check the authorization ID"
    );
    assert!(!directory.exists());
}

pub(super) fn unrecovered(paths: &LauncherPaths, config: &LauncherConfig) {
    let inspection = inspect(paths, config, "approval-receipted");
    assert_eq!(inspection.outcome, InspectionOutcome::Unrecovered {});
    assert_eq!(inspection.receipt_state, Some(SessionState::Parked));
    assert_eq!(inspection.binding.receipt_head.unwrap().sequence, 2);
    assert!(matches!(
        crate::launcher_install::acquire_identity(paths, 2),
        Err(crate::launcher_install::LauncherError::Poisoned { slot: 2 })
    ));
}

pub(super) fn recovered(paths: &LauncherPaths, config: &LauncherConfig, proof: &Proof) {
    let inspection = inspect(paths, config, "approval-receipted");
    assert_eq!(inspection.binding, proof.payload.binding);
    assert_eq!(inspection.receipt_state, Some(SessionState::Parked));
    assert_eq!(
        inspection.outcome,
        InspectionOutcome::RecoveredAfterReboot {
            proof: Box::new(proof.clone())
        }
    );
    let receiptless = inspect(paths, config, "approval");
    assert!(receiptless.binding.receipt_head.is_none());
    assert!(receiptless.receipt_state.is_none());
    assert!(matches!(
        receiptless.outcome,
        InspectionOutcome::RecoveredAfterReboot { .. }
    ));
}

pub(super) fn normal_disposal(root: &Path, paths: &LauncherPaths, config: &LauncherConfig) {
    // The already granted successor is consumed only after the original-history
    // preservation assertions. This adds no authorization and refills no budget.
    fs::write(
        root.join("state/consume-successor"),
        b"fixture normal Disposal",
    )
    .unwrap();
    let before = inspect(paths, config, "approval-successor");
    assert_eq!(before.outcome, InspectionOutcome::Unrecovered {});
    let signer = LauncherSigner::open(paths).unwrap();
    let sign = |payload: crate::launch_receipt::ReceiptPayload| SignedReceipt {
        schema: crate::launch_receipt::SIGNED_RECEIPT_SCHEMA.into(),
        signature: signer
            .sign_receipt(&payload.signing_key_id, &payload.canonical_bytes())
            .unwrap(),
        payload,
    };
    let launch = sign(super::super::receipt_history::genesis(
        &signer,
        &before.binding.authorization,
    ));
    let auth = &before.binding.authorization;
    let start = sign(crate::launch_receipt::ReceiptPayload {
        sequence: 1,
        previous_receipt_digest: Some(launch.digest().to_string()),
        request_id: "start-successor".into(),
        outcome: ReceiptOutcome::Start {
            evidence: crate::launch_receipt::StartEvidence {
                agent_pid: std::process::id(),
                assigned_uid: auth.assigned_uid,
                assigned_gid: auth.assigned_gid,
                tool_isolation_digest: Digest::of(b"synthetic normal disposal tools").to_string(),
                sender_guard_required: false,
            },
            authority: ReceiptAuthority::Cause {
                cause: ReceiptCause::LaunchAcknowledged,
            },
        },
        resulting_state: SessionState::Running,
        ..launch.payload.clone()
    });
    let disposal = sign(crate::launch_receipt::ReceiptPayload {
        sequence: 2,
        previous_receipt_digest: Some(start.digest().to_string()),
        request_id: "dispose-successor".into(),
        outcome: ReceiptOutcome::Disposal {
            authority: ReceiptAuthority::Cause {
                cause: ReceiptCause::ControllerLost,
            },
        },
        resulting_state: SessionState::Terminal,
        ..launch.payload.clone()
    });
    write_json(
        &root.join("successor-chain.json"),
        &vec![launch, start, disposal.clone()],
    );
    let after = inspect(paths, config, "approval-successor");
    assert_eq!(after.outcome, InspectionOutcome::Disposed {});
    assert_eq!(after.receipt_state, Some(SessionState::Terminal));
    assert_eq!(
        after.binding.receipt_head.unwrap().digest,
        disposal.digest().to_string()
    );
}

pub(super) fn invalid_proof(
    root: &Path,
    paths: &LauncherPaths,
    config: &LauncherConfig,
    proof: &Proof,
) {
    let path = root
        .join("state/receipts/consumed-recoveries")
        .join("approval-receipted.json");
    let original = fs::read(&path).unwrap();
    assert_eq!(original, proof.canonical_bytes());
    // Preserve the exact accepted bytes; only this disposable fixture corrupts
    // its own copy to prove the CLI never downgrades invalid evidence to success.
    fs::write(&path, b"malformed private fixture").unwrap();
    let output = command(paths, config.operator_uid, "approval-receipted");
    fs::write(&path, &original).unwrap();
    assert!(!output.status.success());
    assert_eq!(output.stdout, [] as [u8; 0]);
    assert_eq!(
        String::from_utf8(output.stderr).unwrap().trim(),
        "louiselm-launch: invalid or conflicting recovery evidence; preserve the original history"
    );
    recovered(paths, config, proof);
    println!("RECOVERY_INSTALLED_INSPECTION_VERIFIED");
}
