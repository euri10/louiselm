//! Installed certification on Debian 13; explicit refusal on unsupported CI hosts.

use super::*;
use crate::conformance::{
    ReportResult,
    admission::{Attendance, Condition, Enforcement},
    installed::{CertificateStore, CertificationError, certify, measure},
};
use crate::launch_protocol::{ConformanceWaiver, LAUNCH_AUTHORIZATION_SCHEMA};

#[test]
fn privileged_installed_certification_owns_probes_and_retains_exact_evidence() {
    if std::env::var_os("LOUISELM_REQUIRE_CERTIFICATION").is_none() {
        eprintln!("requires disposable root and an owned network namespace");
        return;
    }
    assert!(rustix::process::geteuid().is_root());
    assert_ne!(
        fs::read_link("/proc/self/ns/net").unwrap(),
        fs::read_link("/proc/1/ns/net").unwrap()
    );
    let _account = BrokerAccount::create();
    let root = tempfile::tempdir().unwrap();
    let (paths, mut config, _) = install_fixture_with_slots(root.path(), 3);
    // Activation belongs to these disposable root-owned install records.
    // Certification must measure the enforced policy, not its old default.
    config.conformance = Enforcement::Enforced;
    write_json(&paths.state_root.join("config.json"), &config);
    write_json(&paths.state_root.join("public-config.json"), &config);
    let deadline = Instant::now() + Duration::from_mins(3);
    let parent = rustix::process::getppid()
        .unwrap()
        .as_raw_nonzero()
        .get()
        .cast_unsigned();
    let result = certify(&paths, deadline, parent);
    let os = fs::read_to_string("/usr/lib/os-release").unwrap();
    if !os.lines().any(|line| line == "ID=debian")
        || !os.lines().any(|line| line == "VERSION_ID=\"13\"")
    {
        assert!(
            matches!(result, Err(CertificationError::Unsupported)),
            "{result:?}"
        );
        assert!(!paths.state_root.join("conformance").exists());
        eprintln!("unsupported host correctly refused; no installed certification claimed");
        return;
    }
    let certificate = result.unwrap();
    assert_eq!(
        certificate.observations.result().unwrap(),
        ReportResult::Passed,
        "{:?}",
        certificate.observations
    );
    let host = measure(&paths, &config, deadline).unwrap();
    assert!(certificate.is_current(&host));
    let status = CertificateStore::inspect(&paths.state_root.join("conformance"), &host).unwrap();
    assert!(!status.pending);
    assert!(status.history.failures.is_empty());
    assert_installed_admission(&paths, &config, &certificate.observations, deadline);
    assert_eq!(status.certificate, Some(certificate));
    for slot in 0..3 {
        crate::launcher_install::acquire_identity(&paths, slot)
            .unwrap()
            .release()
            .unwrap();
    }
    let mut changed = host.clone();
    changed.inputs.insert(
        "backend".into(),
        Digest::of(b"different backend").to_string(),
    );
    assert!(
        CertificateStore::inspect(&paths.state_root.join("conformance"), &changed)
            .unwrap()
            .certificate
            .is_none()
    );
    let mut rebooted = host;
    rebooted.boot_id = "00000000-0000-0000-0000-000000000002".into();
    assert!(
        CertificateStore::inspect(&paths.state_root.join("conformance"), &rebooted)
            .unwrap()
            .certificate
            .is_none()
    );
    verify_interrupted_cleanup(&paths, deadline, parent);
}

fn authorization(config: &LauncherConfig) -> LaunchAuthorization {
    let request = request();
    let request_digest = request.digest().to_string();
    LaunchAuthorization {
        conformance: crate::launch_protocol::ConformanceAuthorization::default(),
        schema: LAUNCH_AUTHORIZATION_SCHEMA.into(),
        protocol_version: PROTOCOL_VERSION,
        authorization_id: request.authorization_id,
        request_id: request.request_id.clone(),
        request_digest,
        controller_uid: config.operator_uid,
        session_id: request.session_id,
        run_id: request.run_id,
        envelope_revision: request.envelope_revision,
        identity_slot: 0,
        assigned_uid: AGENT_UID,
        assigned_gid: AGENT_UID,
        expires_at_ms: u64::MAX,
        broker_loss_grace_ms: 5000,
    }
}

fn assert_installed_admission(
    paths: &LauncherPaths,
    config: &LauncherConfig,
    report: &crate::conformance::Report,
    deadline: Instant,
) {
    let mut authorization = authorization(config);
    let inspect = crate::launch_supervisor::conformance::inspect;
    let admitted = inspect(paths, config, &authorization, 1000, deadline).unwrap();
    assert_eq!(
        admitted.evidence,
        crate::launch_receipt::ConformanceEvidence::Certified {
            report_digest: report.digest().unwrap().to_string(),
        }
    );
    assert_eq!(
        admitted.report_bytes,
        Some(report.canonical_bytes().unwrap())
    );

    let mut ordinary = config.clone();
    ordinary.conformance = Enforcement::PreCutover;
    let ordinary_admission =
        inspect(paths, &ordinary, &authorization, 1000, Instant::now()).unwrap();
    assert_eq!(
        ordinary_admission.evidence,
        crate::launch_receipt::ConformanceEvidence::Unevaluated
    );
    assert!(ordinary_admission.report_bytes.is_none());

    // Even an exact approved waiver cannot turn unreadable protected state
    // into empty history. The renamed file is disposable test-owned evidence.
    let state = paths.state_root.join("conformance/state.json");
    let retained = paths.state_root.join("conformance/state.retained");
    fs::rename(&state, &retained).unwrap();
    approve_waiver(&mut authorization, Condition::Missing);
    assert!(matches!(
        inspect(paths, config, &authorization, 1000, deadline),
        Err(SupervisorError::ConformanceUnavailable)
    ));
    fs::rename(retained, state).unwrap();
}

fn approve_waiver(authorization: &mut LaunchAuthorization, condition: Condition) {
    authorization.conformance.attendance = Attendance::Interactive;
    authorization.conformance.waiver = Some(ConformanceWaiver {
        session_id: authorization.session_id.clone(),
        request_digest: authorization.request_digest.clone(),
        operator_uid: authorization.controller_uid,
        condition,
        expires_at_ms: u64::MAX,
        receipt_digest: Digest::of(b"fixture-authorized-waiver").to_string(),
    });
}

fn verify_interrupted_cleanup(paths: &LauncherPaths, deadline: Instant, parent: u32) {
    crate::conformance::installed::CANCEL_AFTER_FIRST_GROUP.with(|cancel| cancel.set(true));
    let cancelled = certify(paths, deadline, parent).unwrap();
    assert_eq!(
        cancelled.observations.result().unwrap(),
        ReportResult::Incomplete
    );
    assert!(
        !cancelled.observations.checks.is_empty(),
        "real probes ran before cancellation"
    );
    assert_eq!(
        cancelled.observations.cleanup,
        crate::conformance::Cleanup::Confirmed
    );
    let config = crate::launcher_install::runtime_config(paths).unwrap();
    let mut authorization = authorization(&config);
    let inspect = crate::launch_supervisor::conformance::inspect;
    assert_eq!(
        inspect(paths, &config, &authorization, 1000, deadline).err(),
        Some(SupervisorError::ConformanceRefused(Condition::Stale))
    );
    approve_waiver(&mut authorization, Condition::Stale);
    let waived = inspect(paths, &config, &authorization, 1000, deadline).unwrap();
    assert_eq!(
        waived.evidence,
        crate::launch_receipt::ConformanceEvidence::Waived {
            condition: Condition::Stale,
            report_digest: Some(cancelled.observations.digest().unwrap().to_string()),
        }
    );
    assert_eq!(
        waived.report_bytes,
        Some(cancelled.observations.canonical_bytes().unwrap())
    );
    for slot in 0..3 {
        crate::launcher_install::acquire_identity(paths, slot)
            .unwrap()
            .release()
            .unwrap();
    }
    crate::conformance::installed::FORCE_UNCONFIRMED_CLEANUP.with(|fail| fail.set(true));
    let uncertain = certify(paths, deadline, parent).unwrap();
    assert_eq!(
        uncertain.observations.result().unwrap(),
        ReportResult::Failed(vec!["cleanup".into()])
    );
    assert!(matches!(
        inspect(paths, &config, &authorization, 1000, deadline),
        Err(SupervisorError::ConformanceRefused(
            Condition::ContainmentFailure
        ))
    ));
    for slot in 0..3 {
        assert!(matches!(
            crate::launcher_install::acquire_identity(paths, slot),
            Err(crate::launcher_install::LauncherError::Poisoned { .. })
        ));
    }
    let resources: serde_json::Value = serde_json::from_slice(
        &fs::read(paths.state_root.join("conformance/attempt.json")).unwrap(),
    )
    .unwrap();
    let retained = Path::new(resources["directory"].as_str().unwrap());
    assert_eq!(retained.parent(), Some(Path::new("/var/tmp")));
    assert!(
        retained
            .file_name()
            .unwrap()
            .to_str()
            .unwrap()
            .starts_with("louiselm-cert-")
    );
    // Only the proof result was injected; actual probe processes were disposed.
    // Remove this test-owned retained directory, never repair production leases.
    fs::remove_dir_all(retained).unwrap();
}
