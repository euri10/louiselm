//! Exact reviewed QA-only policy transition; never signs, certifies or restores implicitly.
use super::{Digest, LauncherConfig, LauncherError, LauncherPaths};
use crate::conformance::admission::Enforcement;
use serde::Serialize;
use std::time::Instant;

#[derive(Serialize)]
pub(crate) struct Outcome {
    pub(crate) release_id: String,
    pub(crate) operator_uid: u32,
    pub(crate) previous_policy_digest: String,
    pub(crate) policy_digest: String,
    pub(crate) conformance: Enforcement,
}

pub(crate) fn policy_digest(config: &LauncherConfig) -> Result<Digest, LauncherError> {
    serde_json::to_vec(config)
        .map(|bytes| Digest::of(&bytes))
        .map_err(|error| LauncherError::Malformed(error.to_string()))
}

pub(crate) fn apply(
    paths: &LauncherPaths,
    expected: &Digest,
    operator: u32,
    release_id: &str,
    target: Enforcement,
    deadline: Instant,
) -> Result<Outcome, LauncherError> {
    super::require_system_install_context(paths)?;
    super::require_system_state_dirs(paths)?;
    let _installation = super::acquire_install_lock(paths)?;
    let current = super::runtime_config_with_deadline(paths, deadline)?;
    if current.release_id != release_id || super::public_runtime_config(paths)? != current {
        return Err(LauncherError::Invalid(
            "QA policy authority mismatch".into(),
        ));
    }
    let changed = proposed(&current, expected, operator, target)?;
    // Lock ordering: installer, then every identity slot. Never spawn a helper
    // under the pool guard. A cached launch must recheck policy after its lease.
    let _idle = super::identity::idle_pool::hold(paths, current.pool.slots)?;
    let reread: LauncherConfig = super::read_required_json(&paths.config())?;
    let public: LauncherConfig = super::read_required_json(&paths.public_config())?;
    if reread != current || public != current || Instant::now() >= deadline {
        return Err(LauncherError::Invalid("QA policy authority changed".into()));
    }
    super::write_json_atomic(&paths.config(), &changed, 0o600)?;
    super::write_json_atomic(&paths.public_config(), &changed, 0o444)?;
    // A partial publication is an error, not an automatic retry/restore. Existing
    // certificates/history are retained and become stale through the policy digest.
    Ok(Outcome {
        release_id: current.release_id,
        operator_uid: operator,
        previous_policy_digest: expected.to_string(),
        policy_digest: policy_digest(&changed)?.to_string(),
        conformance: target,
    })
}

fn proposed(
    current: &LauncherConfig,
    expected: &Digest,
    operator: u32,
    target: Enforcement,
) -> Result<LauncherConfig, LauncherError> {
    if &policy_digest(current)? != expected
        || operator == 0
        || current.operator_uid != operator
        || current.conformance == target
    {
        return Err(LauncherError::Invalid("QA policy review mismatch".into()));
    }
    let mut changed = current.clone();
    changed.conformance = target;
    Ok(changed)
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    reason = "Pure policy fixture asserts exact changes/refusals."
)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn exact_policy_review_changes_only_conformance_and_requires_a_new_review_to_restore() {
        let original = super::super::tests::config_with_measured_bwrap(Path::new("/usr/bin/true"));
        let expected = Digest::of(&serde_json::to_vec(&original).unwrap());
        let changed = proposed(
            &original,
            &expected,
            original.operator_uid,
            Enforcement::Enforced,
        )
        .unwrap();
        assert_eq!(changed.conformance, Enforcement::Enforced);
        let mut unchanged = changed.clone();
        unchanged.conformance = original.conformance;
        assert_eq!(unchanged, original);
        let restore = Digest::of(&serde_json::to_vec(&changed).unwrap());
        assert_eq!(
            proposed(
                &changed,
                &restore,
                changed.operator_uid,
                Enforcement::PreCutover
            )
            .unwrap(),
            original
        );
    }

    #[test]
    fn stale_foreign_and_no_op_reviews_refuse_exactly() {
        let original = super::super::tests::config_with_measured_bwrap(Path::new("/usr/bin/true"));
        let expected = Digest::of(&serde_json::to_vec(&original).unwrap());
        for (digest, operator, target) in [
            (
                Digest::of(b"stale review"),
                original.operator_uid,
                Enforcement::Enforced,
            ),
            (
                expected.clone(),
                original.operator_uid + 1,
                Enforcement::Enforced,
            ),
            (expected, original.operator_uid, original.conformance),
        ] {
            assert!(matches!(proposed(&original, &digest, operator, target),
                Err(LauncherError::Invalid(message)) if message == "QA policy review mismatch"));
        }
    }
}
