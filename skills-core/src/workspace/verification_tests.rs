#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "Tests assert fixture construction and observable outcomes."
)]

use super::*;
use crate::workspace::provenance::{OutputProvenance, OutputProvenanceCode};
use crate::workspace::{SnapshotRecord, SourceFile, SourceFiles};
use std::os::unix::fs::PermissionsExt;

#[test]
#[expect(
    clippy::too_many_lines,
    reason = "One exported workspace fixture covers inherited taint and detached metadata at each artifact boundary."
)]
fn stages_exact_inputs_and_exports_the_observed_workspace_without_execution() {
    let temp = tempfile::tempdir().unwrap();
    let snapshot = temp.path().join("snapshot");
    fs::create_dir(&snapshot).unwrap();
    let files = SourceFiles::from([(
        "src/main.rs".into(),
        SourceFile {
            bytes: b"baseline".to_vec(),
            executable: false,
        },
    )]);
    let record = SnapshotRecord {
        schema: super::super::SNAPSHOT_SCHEMA.into(),
        base_commit: "a".repeat(40),
        files: entries(&files),
        selected: vec![],
        changes: vec![],
    };
    let bytes = serde_json::to_vec(&record).unwrap();
    filesystem::write_files(&snapshot.join("files"), &files, true).unwrap();
    fs::write(snapshot.join("snapshot.json"), &bytes).unwrap();
    let plan = temp.path().join("plan.json");
    let plan_bytes = br#"{"schema":"louiselm.workspace.verification-plan/1","commands":[{"argv":["sh","-c","touch SHOULD_NOT_EXECUTE"],"cwd":".","timeout_ms":1000}]}"#;
    fs::write(&plan, plan_bytes).unwrap();
    let staged = temp.path().join("staged");
    let input = stage_inputs(
        &snapshot,
        &Digest::of(&bytes),
        &plan,
        &Digest::of(plan_bytes),
        &staged,
    )
    .unwrap();
    let workspace = temp.path().join("workspace");
    fs::create_dir_all(workspace.join("src")).unwrap();
    fs::write(workspace.join("src/main.rs"), b"proposed").unwrap();
    let output = temp.path().join("export");
    let preview = export_job(&staged, &input, &workspace, &output, &Digest::of(&bytes)).unwrap();
    assert_eq!(preview.plan_digest, Digest::of(plan_bytes).to_string());
    assert_eq!(preview.command_count, 1);
    assert_eq!(
        preview.output_provenance.code,
        OutputProvenanceCode::Unknown
    );
    assert!(!workspace.join("SHOULD_NOT_EXECUTE").exists());
    assert_eq!(
        fs::read(output.join("job/source/src/main.rs")).unwrap(),
        b"proposed"
    );
    fs::write(workspace.join("src/main.rs"), b"later").unwrap();
    assert_eq!(
        inspect(
            &output.join("job"),
            &Digest::parse(&preview.job_digest).unwrap()
        )
        .unwrap()
        .job_digest,
        preview.job_digest
    );
    assert!(
        export_job(
            &staged,
            &Digest::of(b"wrong"),
            &workspace,
            &temp.path().join("refused"),
            &Digest::of(&bytes)
        )
        .is_err()
    );
    assert!(!temp.path().join("refused").exists());
    assert!(
        export_job(
            &staged,
            &input,
            &workspace,
            &temp.path().join("substituted-base"),
            &Digest::of(b"different launch source")
        )
        .is_err()
    );
    assert!(!temp.path().join("substituted-base").exists());

    // A derivative keeps a taint even after its independent source check.
    let bundle_path = output.join("bundle/bundle.json");
    fs::set_permissions(&bundle_path, fs::Permissions::from_mode(0o600)).unwrap();
    let original = String::from_utf8(fs::read(&bundle_path).unwrap()).unwrap();
    let taint = Digest::of(b"canonical Session taint");
    let unknown = serde_json::to_string(&OutputProvenance::unknown()).unwrap();
    let tainted = serde_json::to_string(&OutputProvenance::tainted(&taint.to_string())).unwrap();
    let changed = original.replace(&unknown, &tainted);
    assert_ne!(changed, original);
    fs::write(&bundle_path, &changed).unwrap();
    let derivative = temp.path().join("tainted-job");
    let inherited = prepare(
        &snapshot,
        &Digest::of(&bytes),
        &output.join("bundle"),
        &Digest::of(changed.as_bytes()),
        &plan,
        &Digest::of(plan_bytes),
        &derivative,
    )
    .unwrap();
    assert_eq!(
        inherited.output_provenance.taint_digest,
        Some(taint.to_string())
    );
    assert_eq!(
        inspect(&derivative, &Digest::parse(&inherited.job_digest).unwrap())
            .unwrap()
            .output_provenance,
        inherited.output_provenance
    );
    let job_path = derivative.join("job.json");
    fs::set_permissions(&job_path, fs::Permissions::from_mode(0o600)).unwrap();
    let job = String::from_utf8(fs::read(&job_path).unwrap()).unwrap();
    let clean = serde_json::to_string(&OutputProvenance::untainted()).unwrap();
    let false_claim = job.replace(&tainted, &clean);
    assert_ne!(false_claim, job);
    fs::write(&job_path, &false_claim).unwrap();
    assert!(matches!(
        inspect(&derivative, &Digest::of(false_claim.as_bytes())),
        Err(WorkspaceError::Invalid(
            "verification job cannot claim clean provenance"
        ))
    ));
    let detached_job = job.replace(&format!("\"output_provenance\":{tainted},"), "");
    assert_ne!(detached_job, job);
    fs::write(&job_path, &detached_job).unwrap();
    assert!(matches!(
        inspect(&derivative, &Digest::of(detached_job.as_bytes())),
        Err(WorkspaceError::Record(_))
    ));

    let false_bundle = original.replace(&unknown, &clean);
    assert_ne!(false_bundle, original);
    fs::write(&bundle_path, &false_bundle).unwrap();
    assert!(matches!(
        prepare(
            &snapshot,
            &Digest::of(&bytes),
            &output.join("bundle"),
            &Digest::of(false_bundle.as_bytes()),
            &plan,
            &Digest::of(plan_bytes),
            &temp.path().join("false-bundle-job"),
        ),
        Err(WorkspaceError::Invalid(
            "bundle cannot claim clean provenance"
        ))
    ));

    // A digest selected after detaching metadata cannot make it clean.
    let detached = changed.replace(&format!("\"output_provenance\":{tainted},"), "");
    assert_ne!(detached, changed);
    fs::write(&bundle_path, &detached).unwrap();
    assert!(matches!(
        prepare(
            &snapshot,
            &Digest::of(&bytes),
            &output.join("bundle"),
            &Digest::of(detached.as_bytes()),
            &plan,
            &Digest::of(plan_bytes),
            &temp.path().join("detached-job"),
        ),
        Err(WorkspaceError::Record(_))
    ));
    assert!(!temp.path().join("detached-job").exists());
}
