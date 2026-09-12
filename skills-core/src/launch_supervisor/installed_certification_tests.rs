//! Installed certification on Debian 13; explicit refusal on unsupported CI hosts.

use super::*;
use crate::conformance::{
    ReportResult,
    installed::{CertificateStore, CertificationError, certify, measure},
};

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
    let (paths, config, _) = install_fixture_with_slots(root.path(), 3);
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
