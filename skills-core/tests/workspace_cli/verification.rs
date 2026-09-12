use super::*;
use louiselm_skills::Digest;

fn plan(temp: &TempDir) -> String {
    let bytes = serde_json::to_vec(&serde_json::json!({
        "schema": "louiselm.workspace.verification-plan/1",
        "commands": [{"argv": ["touch", temp.path().join("private-command-marker")], "cwd": ".", "timeout_ms": 1000}]
    })).unwrap();
    fs::write(temp.path().join("plan.json"), &bytes).unwrap();
    Digest::of(&bytes).to_string()
}

fn prepare_job(temp: &TempDir, snapshot: &str, bundle: &str, plan: &str, output: &str) -> Output {
    run(&[
        "verification",
        "prepare",
        "--snapshot",
        temp.path().join("snapshot").to_str().unwrap(),
        "--digest",
        snapshot,
        "--bundle",
        temp.path().join("bundle").to_str().unwrap(),
        "--bundle-digest",
        bundle,
        "--plan",
        temp.path().join("plan.json").to_str().unwrap(),
        "--plan-digest",
        plan,
        "--output",
        temp.path().join(output).to_str().unwrap(),
        "--robot-json",
    ])
}

fn inspect_job(temp: &TempDir, digest: &str) -> Output {
    run(&[
        "verification",
        "inspect",
        "--job",
        temp.path().join("job").to_str().unwrap(),
        "--digest",
        digest,
        "--robot-json",
    ])
}

#[test]
fn prepares_and_inspects_exact_inputs_without_executing_or_claiming_verification() {
    let temp = fixture();
    let snapshot = prepare(&temp, "snapshot", &[]);
    let snapshot_digest = snapshot["snapshot_digest"].as_str().unwrap();
    successful(&materialize(&temp, snapshot_digest, "work"));
    fs::write(temp.path().join("work/tracked.txt"), b"proposed bytes").unwrap();
    fs::create_dir_all(temp.path().join("work/.git/hooks")).unwrap();
    fs::write(
        temp.path().join("work/.git/hooks/post-checkout"),
        format!(
            "#!/bin/sh\ntouch '{}'\n",
            temp.path().join("private-hook-marker").display()
        ),
    )
    .unwrap();
    fs::set_permissions(
        temp.path().join("work/.git/hooks/post-checkout"),
        fs::Permissions::from_mode(0o700),
    )
    .unwrap();
    let bundle = successful(&export_bundle(&temp, snapshot_digest, "bundle"));
    let plan_digest = plan(&temp);
    let job = successful(&prepare_job(
        &temp,
        snapshot_digest,
        bundle["bundle_digest"].as_str().unwrap(),
        &plan_digest,
        "job",
    ));
    assert_eq!(job["schema"], "louiselm.workspace.verification-preview/1");
    assert_eq!(job["snapshot_digest"], snapshot_digest);
    assert_eq!(job["bundle_digest"], bundle["bundle_digest"]);
    assert_eq!(job["result_digest"], bundle["result_digest"]);
    assert_eq!(job["plan_digest"], plan_digest);
    assert_eq!(job["command_count"], 1);
    assert_eq!(job["state"], "prepared");
    assert_eq!(
        successful(&inspect_job(&temp, job["job_digest"].as_str().unwrap())),
        job
    );
    assert_eq!(
        fs::read(temp.path().join("job/source/tracked.txt")).unwrap(),
        b"proposed bytes"
    );
    assert_eq!(
        fs::metadata(temp.path().join("job/source/tracked.txt"))
            .unwrap()
            .mode()
            & 0o777,
        0o400
    );
    assert!(!temp.path().join("job/source/.git").exists());
    assert!(!temp.path().join("private-command-marker").exists());
    assert!(!temp.path().join("private-hook-marker").exists());
    assert!(
        !String::from_utf8(inspect_job(&temp, job["job_digest"].as_str().unwrap()).stdout)
            .unwrap()
            .contains("private-command-marker")
    );
    let human = run(&[
        "verification",
        "inspect",
        "--job",
        temp.path().join("job").to_str().unwrap(),
        "--digest",
        job["job_digest"].as_str().unwrap(),
    ]);
    assert!(human.status.success());
    let text = String::from_utf8(human.stdout).unwrap();
    assert!(text.contains("not verification or promotion authority"));
    assert!(!text.contains("private-command-marker"));
}

fn inputs(temp: &TempDir) -> (String, String, String) {
    let snapshot = prepare(temp, "snapshot", &[]);
    let digest = snapshot["snapshot_digest"].as_str().unwrap().to_owned();
    successful(&materialize(temp, &digest, "work"));
    let bundle = successful(&export_bundle(temp, &digest, "bundle"));
    (
        digest,
        bundle["bundle_digest"].as_str().unwrap().to_owned(),
        plan(temp),
    )
}

fn refused(output: &Output) {
    assert_eq!(output.status.code(), Some(1));
    assert!(!String::from_utf8_lossy(&output.stdout).contains("private-command-marker"));
    assert!(!String::from_utf8_lossy(&output.stderr).contains("private-command-marker"));
}

#[test]
fn substituted_inputs_and_existing_outputs_are_refused_and_plan_changes_rebind_the_job() {
    let temp = fixture();
    let (snapshot, bundle, plan) = inputs(&temp);
    let wrong = Digest::of(b"different").to_string();
    for (source, change, commands) in [
        (&wrong, &bundle, &plan),
        (&snapshot, &wrong, &plan),
        (&snapshot, &bundle, &wrong),
    ] {
        refused(&prepare_job(&temp, source, change, commands, "refused"));
        assert!(!temp.path().join("refused").exists());
    }
    let job = successful(&prepare_job(&temp, &snapshot, &bundle, &plan, "job"));
    let again = successful(&prepare_job(&temp, &snapshot, &bundle, &plan, "again"));
    assert_eq!(job, again);
    refused(&prepare_job(&temp, &snapshot, &bundle, &plan, "job"));
    assert_eq!(
        successful(&inspect_job(&temp, job["job_digest"].as_str().unwrap())),
        job
    );
    refused(&inspect_job(&temp, &wrong));
    refused(&prepare_job(
        &temp,
        &snapshot,
        &bundle,
        &plan,
        "bundle/nested",
    ));
    let mut bytes = fs::read(temp.path().join("plan.json")).unwrap();
    bytes.push(b' ');
    fs::write(temp.path().join("plan.json"), &bytes).unwrap();
    refused(&prepare_job(&temp, &snapshot, &bundle, &plan, "stale-plan"));
    let changed = successful(&prepare_job(
        &temp,
        &snapshot,
        &bundle,
        &Digest::of(&bytes).to_string(),
        "changed-plan",
    ));
    assert_ne!(changed["job_digest"], job["job_digest"]);
    assert_eq!(changed["result_digest"], job["result_digest"]);
}

#[test]
fn inspection_refuses_source_plan_record_and_file_kind_tampering() {
    for case in 0..9 {
        let temp = fixture();
        let (snapshot, bundle, plan) = inputs(&temp);
        let job = successful(&prepare_job(&temp, &snapshot, &bundle, &plan, "job"));
        let source = temp.path().join("job/source/tracked.txt");
        match case {
            0 => {
                fs::set_permissions(&source, fs::Permissions::from_mode(0o600)).unwrap();
                fs::write(&source, b"changed source").unwrap();
            }
            1 => fs::set_permissions(&source, fs::Permissions::from_mode(0o500)).unwrap(),
            2 => fs::remove_file(&source).unwrap(),
            3 => fs::write(temp.path().join("job/source/extra"), b"extra").unwrap(),
            4 => {
                let path = temp.path().join("job/plan.json");
                fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
                fs::write(path, b"{} private-command-marker").unwrap();
            }
            5 => {
                let path = temp.path().join("job/job.json");
                fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
                fs::write(path, b"{}").unwrap();
            }
            6 => {
                fs::remove_file(&source).unwrap();
                symlink("/etc/passwd", &source).unwrap();
            }
            7 => symlink("/missing", temp.path().join("job/source/.git")).unwrap(),
            _ => fs::hard_link(&source, temp.path().join("job/source/alias")).unwrap(),
        }
        refused(&inspect_job(&temp, job["job_digest"].as_str().unwrap()));
    }
}

#[test]
fn invalid_or_unbounded_plans_never_publish_even_with_the_matching_digest() {
    let temp = fixture();
    let (snapshot, bundle, _) = inputs(&temp);
    let baseline: Value =
        serde_json::from_slice(&fs::read(temp.path().join("plan.json")).unwrap()).unwrap();
    for case in 0..14 {
        let mut document = baseline.clone();
        match case {
            0 => document["schema"] = "unsupported".into(),
            1 => document["commands"] = serde_json::json!([]),
            2 => document["commands"][0]["argv"] = serde_json::json!([]),
            3 => document["commands"][0]["argv"] = serde_json::json!([""]),
            4 => document["commands"][0]["argv"] = serde_json::json!(["bad\u{0}argument"]),
            5 => document["commands"][0]["cwd"] = "../escape".into(),
            6 => document["commands"][0]["cwd"] = "absent".into(),
            7 => document["commands"][0]["timeout_ms"] = 0.into(),
            8 => document["commands"][0]["timeout_ms"] = u64::MAX.into(),
            9 => {
                document["commands"][0]["environment"] =
                    serde_json::json!({"SECRET": "private-command-marker"});
            }
            10 => document["private-command-marker"] = true.into(),
            11 => document["commands"] = vec![document["commands"][0].clone(); 33].into(),
            12 => document["commands"][0]["argv"] = vec!["x"; 129].into(),
            _ => document["commands"][0]["argv"] = serde_json::json!(["-c", "sh"]),
        }
        let bytes = serde_json::to_vec(&document).unwrap();
        fs::write(temp.path().join("plan.json"), &bytes).unwrap();
        refused(&prepare_job(
            &temp,
            &snapshot,
            &bundle,
            &Digest::of(&bytes).to_string(),
            "refused",
        ));
        assert!(!temp.path().join("refused").exists());
    }
    for bytes in [
        b"{\"schema\":\"a\",\"schema\":\"b\"}".to_vec(),
        vec![b' '; 65_537],
    ] {
        fs::write(temp.path().join("plan.json"), &bytes).unwrap();
        refused(&prepare_job(
            &temp,
            &snapshot,
            &bundle,
            &Digest::of(&bytes).to_string(),
            "refused",
        ));
        assert!(!temp.path().join("refused").exists());
    }
}

#[test]
fn verification_command_parser_is_closed_and_redacted() {
    for args in [
        vec!["verification", "private-command-marker"],
        vec!["verification", "prepare", "--private-command-marker"],
        vec![
            "verification",
            "inspect",
            "--plan",
            "private-command-marker",
        ],
        vec!["verification", "inspect", "--job", "private-command-marker"],
        vec![
            "verification",
            "inspect",
            "--job",
            "a",
            "--job",
            "private-command-marker",
        ],
        vec!["verification", "inspect", "--robot-json", "--robot-json"],
    ] {
        refused(&run(&args));
    }
}
