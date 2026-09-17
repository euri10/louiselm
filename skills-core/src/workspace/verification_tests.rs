#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "Tests assert fixture construction and observable outcomes."
)]

use super::*;
use crate::workspace::{SnapshotRecord, SourceFile, SourceFiles};

#[test]
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
}
