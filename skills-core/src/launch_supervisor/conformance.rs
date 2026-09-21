//! Installed policy and host observations meet only inside the privileged owner.

use super::{LaunchAuthorization, SupervisorError};
use crate::{
    conformance::{
        admission::{self, Admission, Enforcement, Request, Waiver},
        installed::{CertificateStore, measure},
    },
    launch_receipt::ConformanceEvidence,
    launcher_install::{LauncherConfig, LauncherPaths},
};
use std::time::Instant;

/// A trusted platform's admission outcome and the exact report it inspected.
/// This is original admission history, not a source of current posture or timers.
#[derive(Clone)]
pub struct ConformanceAdmission {
    /// Decision recorded in the signed sequence-zero receipt.
    pub evidence: ConformanceEvidence,
    /// Exact canonical observations when the decision binds a report digest.
    pub report_bytes: Option<Vec<u8>>,
}

pub(super) fn inspect(
    paths: &LauncherPaths,
    config: &LauncherConfig,
    authorization: &LaunchAuthorization,
    now_ms: u64,
    deadline: Instant,
) -> Result<ConformanceAdmission, SupervisorError> {
    inspect_current(paths, config, authorization, now_ms, deadline, true)
}

pub(super) fn inspect_current(
    paths: &LauncherPaths,
    config: &LauncherConfig,
    authorization: &LaunchAuthorization,
    now_ms: u64,
    deadline: Instant,
    admitting: bool,
) -> Result<ConformanceAdmission, SupervisorError> {
    if config.conformance == Enforcement::PreCutover {
        return Ok(ConformanceAdmission {
            evidence: ConformanceEvidence::Unevaluated,
            report_bytes: None,
        });
    }
    let started = Instant::now();
    if !admitting {
        crate::launcher_install::require_session_policy(paths, config)
            .map_err(|_| SupervisorError::ConformanceUnavailable)?;
    }
    let host =
        measure(paths, config, deadline).map_err(|_| SupervisorError::ConformanceUnavailable)?;
    let status = CertificateStore::inspect(&paths.state_root.join("conformance"), &host)
        .map_err(|_| SupervisorError::ConformanceUnavailable)?;
    if !status.history.failures.is_empty() {
        return Err(SupervisorError::ConformanceRefused(
            admission::Condition::ContainmentFailure,
        ));
    }
    if Instant::now() >= deadline {
        return Err(SupervisorError::ConformanceUnavailable);
    }
    let now_ms =
        now_ms.saturating_add(u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX));
    if admitting && now_ms >= authorization.expires_at_ms {
        return Err(SupervisorError::AuthorizationRejected);
    }
    let waiver = authorization
        .conformance
        .waiver
        .as_ref()
        .filter(|waiver| admitting || now_ms < waiver.expires_at_ms)
        .map(|waiver| Waiver {
            session_id: waiver.session_id.clone(),
            condition: waiver.condition,
        });
    let decision = admission::evaluate(
        &status,
        &host,
        &Request {
            session_id: &authorization.session_id,
            attendance: authorization.conformance.attendance,
            waiver: waiver.as_ref(),
        },
    );
    if admitting || matches!(decision, Admission::Waived { .. }) {
        authorization
            .conformance
            .validate_for(
                &authorization.session_id,
                &authorization.request_digest,
                authorization.controller_uid,
                now_ms,
            )
            .map_err(|_| SupervisorError::AuthorizationRejected)?;
    }
    let evidence = match decision {
        Admission::Admitted { report_digest } => ConformanceEvidence::Certified { report_digest },
        Admission::Waived {
            condition,
            report_digest,
        } => ConformanceEvidence::Waived {
            condition,
            report_digest,
        },
        Admission::Waivable(condition) | Admission::Refused(condition) => {
            return Err(SupervisorError::ConformanceRefused(condition));
        }
    };
    let report_bytes = status
        .certificate
        .as_ref()
        .map(|certificate| {
            certificate
                .observations
                .canonical_bytes()
                .map_err(|_| SupervisorError::ConformanceUnavailable)
        })
        .transpose()?;
    Ok(ConformanceAdmission {
        evidence,
        report_bytes,
    })
}
