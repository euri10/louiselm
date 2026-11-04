//! Behavioral coverage for importing Endorsement records.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "Test fixtures abort on setup failure and assert failures directly."
)]

//! Imported records are kept byte for byte and append-only. Trust is not
//! decided here; it is recomputed at use. Withdrawals can never be pruned.

mod support;

use std::{fs, process::Command};

use louiselm_skills::{
    Digest, ReviewDepth, SshKeygenSigner,
    endorsement::{
        self, Batch, EndorsedPackage, EndorserRole, Signing, StatementKind,
        import::{self, ImportError, ImportOutcome},
    },
};
use support::{Fixture, SshKey, write_file};

const BINARY: &str = env!("CARGO_BIN_EXE_louiselm-skills");
const ISSUED: u64 = 1_800_000_000_000;

fn sign(key: &SshKey, batch: &Batch, expires_at_ms: u64) -> Vec<Vec<u8>> {
    endorsement::sign_batch(
        &Signing {
            signer_role: EndorserRole::Publisher,
            signer_key: key.public_key(),
            issued_at_ms: ISSUED,
            expires_at_ms,
        },
        batch,
        &SshKeygenSigner::new(key.private_key_path()),
    )
    .unwrap()
}

fn endorsed(subject: &[u8]) -> Batch {
    Batch::Endorsements(vec![EndorsedPackage {
        package_digest: Digest::of(subject),
        policy_digest: Digest::of(b"policy"),
        dossier_digest: None,
        assessment: None,
        review_depth: ReviewDepth::Read,
        assumptions: Vec::new(),
    }])
}

#[test]
fn an_import_keeps_exact_bytes_and_is_idempotent() {
    let fixture = Fixture::new();
    let key = SshKey::generate(&fixture, "publisher");
    let bytes = sign(&key, &endorsed(b"one"), ISSUED + 1).remove(0);
    let store = fixture.store();

    let first = import::import_bytes(&store, &bytes).unwrap();
    let again = import::import_bytes(&store, &bytes).unwrap();

    assert_eq!(first.outcome, ImportOutcome::Imported);
    assert_eq!(again.outcome, ImportOutcome::Existing);
    assert_eq!(first.digest, Digest::of(&bytes));
    let stored = import::records(&store).unwrap();
    assert_eq!(stored.records.len(), 1);
    assert_eq!(stored.records[0].to_bytes(), bytes);
    assert_eq!(stored.unreadable, Vec::new());
}

#[test]
fn malformed_input_is_refused_by_name_and_never_stored() {
    let fixture = Fixture::new();
    let store = fixture.store();

    let error = import::import_bytes(&store, b"{\"schema\":\"something/else\"}").unwrap_err();
    assert!(
        matches!(
            error,
            ImportError::Record(endorsement::EndorsementError::UnsupportedSchema(_))
        ),
        "{error:?}"
    );
    assert_eq!(import::records(&store).unwrap().records.len(), 0);
}

#[test]
fn a_directory_import_reports_each_file_and_keeps_the_good_ones() {
    let fixture = Fixture::new();
    let key = SshKey::generate(&fixture, "publisher");
    let directory = fixture.candidate("skills/endorsements");
    for (index, bytes) in sign(
        &key,
        &Batch::Endorsements(
            ["one", "two"]
                .iter()
                .map(|subject| EndorsedPackage {
                    package_digest: Digest::of(subject.as_bytes()),
                    policy_digest: Digest::of(b"policy"),
                    dossier_digest: None,
                    assessment: None,
                    review_depth: ReviewDepth::Read,
                    assumptions: Vec::new(),
                })
                .collect(),
        ),
        ISSUED + 1,
    )
    .into_iter()
    .enumerate()
    {
        fs::write(directory.join(format!("{index}.json")), bytes).unwrap();
    }
    write_file(&directory.join("junk.json"), "not json");
    write_file(&directory.join("README.md"), "ignored: not a record file");

    let report = import::import_path(&fixture.store(), &directory).unwrap();

    assert_eq!(report.imported.len(), 2);
    assert_eq!(report.refused.len(), 1);
    assert!(report.refused[0].path.ends_with("junk.json"));
    assert_eq!(import::records(&fixture.store()).unwrap().records.len(), 2);
}

#[test]
fn pruning_removes_expired_endorsements_only_and_never_a_withdrawal() {
    let fixture = Fixture::new();
    let key = SshKey::generate(&fixture, "publisher");
    let store = fixture.store();
    let expired = sign(&key, &endorsed(b"old"), ISSUED + 10).remove(0);
    let fresh = sign(&key, &endorsed(b"new"), ISSUED + 1_000).remove(0);
    let withdrawal = sign(
        &key,
        &Batch::Withdrawals(vec![Digest::of(b"old")]),
        ISSUED + 10,
    )
    .remove(0);
    for bytes in [&expired, &fresh, &withdrawal] {
        import::import_bytes(&store, bytes).unwrap();
    }

    let pruned = import::prune_expired(&store, ISSUED + 100).unwrap();

    assert_eq!(pruned, vec![Digest::of(&expired)]);
    let kinds: Vec<StatementKind> = import::records(&store)
        .unwrap()
        .records
        .iter()
        .map(endorsement::Record::kind)
        .collect();
    assert_eq!(kinds.len(), 2);
    assert!(kinds.contains(&StatementKind::Withdrawal));
}

#[test]
fn a_stored_file_whose_bytes_do_not_match_its_name_is_reported_not_returned() {
    let fixture = Fixture::new();
    let key = SshKey::generate(&fixture, "publisher");
    let store = fixture.store();
    let first =
        import::import_bytes(&store, &sign(&key, &endorsed(b"one"), ISSUED + 1).remove(0)).unwrap();
    let other = sign(&key, &endorsed(b"two"), ISSUED + 1).remove(0);
    let path = fixture
        .store_root()
        .join("endorsements")
        .join(format!("{}.json", first.digest.directory_name()));
    fs::write(&path, other).unwrap();

    let stored = import::records(&store).unwrap();
    assert_eq!(stored.records.len(), 0);
    assert_eq!(stored.unreadable.len(), 1);
}

#[test]
fn packaging_from_a_skills_tree_imports_its_sibling_endorsements() {
    let fixture = Fixture::new();
    let key = SshKey::generate(&fixture, "publisher");
    let skill = fixture.candidate("checkout/skills/demo");
    write_file(
        &skill.join("SKILL.md"),
        "---\nname: demo\ndescription: The demo skill.\n---\n\nBody.\n",
    );
    let endorsements = fixture.candidate("checkout/skills/endorsements");
    fs::write(
        endorsements.join("batch.json"),
        sign(&key, &endorsed(b"demo"), ISSUED + 1).remove(0),
    )
    .unwrap();

    let output = Command::new(BINARY)
        .args(["package", skill.to_str().unwrap(), "--robot-json"])
        .env("LOUISELM_SKILLS_STORE", fixture.store_root())
        .output()
        .unwrap();

    assert_eq!(
        output.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let result: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(result["endorsements"]["imported"], 1);
    assert_eq!(import::records(&fixture.store()).unwrap().records.len(), 1);

    let lone = fixture.candidate("elsewhere/lone");
    write_file(
        &lone.join("SKILL.md"),
        "---\nname: lone\ndescription: The lone skill.\n---\n\nBody.\n",
    );
    let output = Command::new(BINARY)
        .args(["package", lone.to_str().unwrap(), "--robot-json"])
        .env("LOUISELM_SKILLS_STORE", fixture.store_root())
        .output()
        .unwrap();
    let result: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert!(
        result.get("endorsements").is_none(),
        "no sibling directory, no field"
    );
}
