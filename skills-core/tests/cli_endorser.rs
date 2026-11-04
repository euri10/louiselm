//! Behavioral coverage for the endorser command.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "Test fixtures abort on setup failure and assert failures directly."
)]

//! The local endorser layer as an operator drives it, from the command line.

mod support;

use std::process::Command;

use serde_json::Value;
use support::{Fixture, SshKey};

const BINARY: &str = env!("CARGO_BIN_EXE_louiselm-skills");

fn run(fixture: &Fixture, arguments: &[&str]) -> (i32, String, String) {
    let output = Command::new(BINARY)
        .args(arguments)
        .env("LOUISELM_SKILLS_STORE", fixture.store_root())
        .output()
        .expect("the tool runs");
    (
        output.status.code().expect("the tool exits normally"),
        String::from_utf8(output.stdout).unwrap(),
        String::from_utf8(output.stderr).unwrap(),
    )
}

fn listing(fixture: &Fixture) -> Value {
    let (status, stdout, stderr) = run(fixture, &["endorser", "list", "--robot-json"]);
    assert_eq!(status, 0, "{stderr}");
    serde_json::from_str(&stdout).unwrap()
}

fn path(key: &SshKey) -> &str {
    key.private_key_path().to_str().unwrap()
}

#[test]
fn an_operator_adds_removes_and_restores_an_endorser() {
    let fixture = Fixture::new();
    let primary = SshKey::generate(&fixture, "primary");
    let release = SshKey::generate(&fixture, "release");
    let publisher = SshKey::generate(&fixture, "publisher");
    let (status, _, stderr) = run(
        &fixture,
        &[
            "trust",
            "bootstrap",
            "--primary",
            &primary.public_key(),
            "--release",
            &release.public_key(),
        ],
    );
    assert_eq!(status, 0, "{stderr}");
    assert_eq!(listing(&fixture)["effective"], serde_json::json!([]));

    let publisher_key = format!("{} publisher@laptop", publisher.public_key());
    let (status, _, stderr) = run(
        &fixture,
        &[
            "endorser",
            "add",
            "--endorser",
            &publisher_key,
            "--label",
            "Benoît Barthelet, LouiseLM publisher",
            "--role",
            "publisher",
            "--max-lifetime-days",
            "180",
            "--key",
            path(&primary),
        ],
    );
    assert_eq!(status, 0, "{stderr}");
    let effective = &listing(&fixture)["effective"];
    assert_eq!(effective[0]["public_key"], publisher.public_key().as_str());
    assert_eq!(effective[0]["roles"], serde_json::json!(["publisher"]));
    assert_eq!(
        effective[0]["sk_policy"]["require_hardware"], true,
        "a local endorser requires hardware unless the signed addition opts out"
    );

    let (status, _, stderr) = run(
        &fixture,
        &["endorser", "remove", "--endorser", &publisher.public_key()],
    );
    assert_eq!(status, 0, "{stderr}");
    assert_eq!(listing(&fixture)["effective"], serde_json::json!([]));

    let (status, _, _) = run(
        &fixture,
        &[
            "endorser",
            "restore",
            "--endorser",
            &publisher.public_key(),
            "--key",
            path(&publisher),
        ],
    );
    assert_eq!(status, 1, "a restore not signed by Primary is refused");
    assert_eq!(listing(&fixture)["effective"], serde_json::json!([]));

    let (status, _, stderr) = run(
        &fixture,
        &[
            "endorser",
            "restore",
            "--endorser",
            &publisher.public_key(),
            "--key",
            path(&primary),
        ],
    );
    assert_eq!(status, 0, "{stderr}");
    assert_eq!(
        listing(&fixture)["effective"][0]["public_key"],
        publisher.public_key().as_str()
    );
}

#[test]
fn adding_needs_the_primary_key_and_a_known_role() {
    let fixture = Fixture::new();
    let publisher = SshKey::generate(&fixture, "publisher");

    let (status, _, stderr) = run(
        &fixture,
        &[
            "endorser",
            "add",
            "--endorser",
            &publisher.public_key(),
            "--label",
            "x",
            "--role",
            "publisher",
        ],
    );
    assert_eq!(status, 1);
    assert!(stderr.contains("key"), "{stderr}");

    let (status, _, stderr) = run(
        &fixture,
        &[
            "endorser",
            "add",
            "--endorser",
            &publisher.public_key(),
            "--label",
            "x",
            "--role",
            "primary",
            "--key",
            path(&publisher),
        ],
    );
    assert_eq!(status, 1);
    assert!(stderr.contains("unknown endorser role"), "{stderr}");
}

#[test]
fn listed_endorsements_show_the_trust_decision_recomputed_now() {
    use louiselm_skills::{
        Digest, ReviewDepth, SshKeygenSigner,
        endorsement::{self, Batch, EndorsedPackage, EndorserRole, Signing},
    };

    let fixture = Fixture::new();
    let primary = SshKey::generate(&fixture, "primary");
    let release = SshKey::generate(&fixture, "release");
    let publisher = SshKey::generate(&fixture, "publisher");
    let (status, _, stderr) = run(
        &fixture,
        &[
            "trust",
            "bootstrap",
            "--primary",
            &primary.public_key(),
            "--release",
            &release.public_key(),
        ],
    );
    assert_eq!(status, 0, "{stderr}");

    let now = u64::try_from(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis(),
    )
    .unwrap();
    let bytes = endorsement::sign_batch(
        &Signing {
            signer_role: EndorserRole::Publisher,
            signer_key: publisher.public_key(),
            issued_at_ms: now - 60_000,
            expires_at_ms: now + 86_400_000,
        },
        &Batch::Endorsements(vec![EndorsedPackage {
            package_digest: Digest::of(b"demo"),
            policy_digest: Digest::of(b"policy"),
            dossier_digest: None,
            assessment: None,
            review_depth: ReviewDepth::Read,
            assumptions: Vec::new(),
        }]),
        &SshKeygenSigner::new(publisher.private_key_path()),
    )
    .unwrap()
    .remove(0);
    let file = fixture.path("incoming/record.json");
    std::fs::create_dir_all(file.parent().unwrap()).unwrap();
    std::fs::write(&file, bytes).unwrap();
    let junk = fixture.path("incoming/junk.json");
    std::fs::write(&junk, "{}").unwrap();

    let (status, _, _) = run(
        &fixture,
        &[
            "endorsement",
            "import",
            file.parent().unwrap().to_str().unwrap(),
        ],
    );
    assert_eq!(status, 1, "a refused file makes the import report failure");

    let decision = |fixture: &Fixture| {
        let (status, stdout, stderr) = run(fixture, &["endorsement", "list", "--robot-json"]);
        assert_eq!(status, 0, "{stderr}");
        let listing: Value = serde_json::from_str(&stdout).unwrap();
        assert_eq!(listing["records"].as_array().unwrap().len(), 1);
        listing["records"][0].clone()
    };
    assert_eq!(decision(&fixture)["trusted"], false);
    assert_eq!(decision(&fixture)["trust"], "untrusted: unknown signer");

    let (status, _, stderr) = run(
        &fixture,
        &[
            "endorser",
            "add",
            "--endorser",
            &publisher.public_key(),
            "--label",
            "Test publisher",
            "--role",
            "publisher",
            "--allow-software-key",
            "--key",
            path(&primary),
        ],
    );
    assert_eq!(status, 0, "{stderr}");
    assert_eq!(decision(&fixture)["trusted"], true);
    assert_eq!(
        decision(&fixture)["trust"],
        "trusted endorser Test publisher"
    );

    let (status, _, stderr) = run(
        &fixture,
        &["endorser", "remove", "--endorser", &publisher.public_key()],
    );
    assert_eq!(status, 0, "{stderr}");
    assert_eq!(decision(&fixture)["trusted"], false);
}

struct Publishing {
    fixture: Fixture,
    primary: SshKey,
    publisher: SshKey,
    packages: Vec<String>,
}

impl Publishing {
    fn new() -> Self {
        let fixture = Fixture::new();
        let primary = SshKey::generate(&fixture, "primary");
        let release = SshKey::generate(&fixture, "release");
        let publisher = SshKey::generate(&fixture, "publisher");
        let (status, _, stderr) = run(
            &fixture,
            &[
                "trust",
                "bootstrap",
                "--primary",
                &primary.public_key(),
                "--release",
                &release.public_key(),
            ],
        );
        assert_eq!(status, 0, "{stderr}");
        let packages = ["alpha", "beta"]
            .iter()
            .map(|name| {
                let skill = fixture.candidate(&format!("skills/{name}"));
                support::write_file(
                    &skill.join("SKILL.md"),
                    &format!("---\nname: {name}\ndescription: The {name} skill.\n---\n\nBody.\n"),
                );
                let (status, stdout, stderr) = run(
                    &fixture,
                    &["package", skill.to_str().unwrap(), "--robot-json"],
                );
                assert_eq!(status, 0, "{stderr}");
                let result: Value = serde_json::from_str(&stdout).unwrap();
                result["digest"].as_str().unwrap().to_owned()
            })
            .collect();
        Self {
            fixture,
            primary,
            publisher,
            packages,
        }
    }

    fn sign(&self, verb: &str, endorser: &SshKey, output: &str) -> (i32, String, String) {
        let mut arguments = vec![
            "endorsement".to_owned(),
            verb.to_owned(),
            "--role".to_owned(),
            "publisher".to_owned(),
            "--endorser".to_owned(),
            endorser.public_key(),
            "--key".to_owned(),
            path(endorser).to_owned(),
            "--output".to_owned(),
            self.fixture.path(output).to_str().unwrap().to_owned(),
            "--review-depth".to_owned(),
            "read".to_owned(),
            "--robot-json".to_owned(),
        ];
        for package in &self.packages {
            arguments.push("--member".to_owned());
            arguments.push(package.clone());
        }
        let arguments: Vec<&str> = arguments.iter().map(String::as_str).collect();
        run(&self.fixture, &arguments)
    }

    fn written(&self, output: &str) -> Vec<louiselm_skills::endorsement::Record> {
        let mut records: Vec<_> = std::fs::read_dir(self.fixture.path(output))
            .unwrap()
            .map(|entry| {
                louiselm_skills::endorsement::parse(&std::fs::read(entry.unwrap().path()).unwrap())
                    .unwrap()
            })
            .collect();
        records.sort_by_key(louiselm_skills::endorsement::Record::digest);
        records
    }
}

#[test]
fn a_publisher_signs_one_batch_whose_records_import_and_cover_each_package() {
    use louiselm_skills::{Digest, endorsement::StatementKind};

    let publishing = Publishing::new();
    let (status, _, stderr) = publishing.sign("sign-batch", &publishing.publisher, "out");
    assert_eq!(status, 0, "{stderr}");

    let records = publishing.written("out");
    assert_eq!(records.len(), 2);
    for package in &publishing.packages {
        let digest = Digest::parse(package).unwrap();
        assert_eq!(
            records
                .iter()
                .filter(|record| record.covers(&digest))
                .count(),
            1
        );
    }
    let signing = records[0].signing();
    assert_eq!(
        signing.expires_at_ms - signing.issued_at_ms,
        90 * 86_400_000
    );
    assert_eq!(records[0].kind(), StatementKind::Endorsement);

    let output = publishing.fixture.path("out");
    let (status, _, stderr) = run(
        &publishing.fixture,
        &["endorsement", "import", output.to_str().unwrap()],
    );
    assert_eq!(status, 0, "{stderr}");

    let digest = records[0].digest().to_string();
    let exported = publishing.fixture.path("exported.json");
    let (status, _, stderr) = run(
        &publishing.fixture,
        &[
            "endorsement",
            "export",
            &digest,
            "--output",
            exported.to_str().unwrap(),
        ],
    );
    assert_eq!(status, 0, "{stderr}");
    assert_eq!(std::fs::read(&exported).unwrap(), records[0].to_bytes());

    let (status, _, _) = run(
        &publishing.fixture,
        &[
            "endorsement",
            "export",
            &digest,
            "--output",
            exported.to_str().unwrap(),
        ],
    );
    assert_eq!(status, 1, "export never overwrites a file");
}

#[test]
fn a_withdrawal_batch_names_each_package() {
    use louiselm_skills::endorsement::StatementKind;

    let publishing = Publishing::new();
    let (status, _, stderr) = publishing.sign("withdraw", &publishing.publisher, "withdrawn");
    assert_eq!(status, 0, "{stderr}");
    let records = publishing.written("withdrawn");
    assert_eq!(records.len(), 2);
    assert!(
        records
            .iter()
            .all(|record| record.kind() == StatementKind::Withdrawal)
    );
}

#[test]
fn local_authority_keys_cannot_sign_endorsements() {
    let publishing = Publishing::new();
    let (status, _, stderr) = publishing.sign("sign-batch", &publishing.primary, "out");
    assert_eq!(status, 1);
    assert!(stderr.contains("enrolled"), "{stderr}");
    assert!(!publishing.fixture.path("out").exists());
}
