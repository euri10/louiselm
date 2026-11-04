//! Behavioral coverage for portable Endorsement records.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "Test fixtures abort on setup failure and assert failures directly."
)]

//! An Endorsement batch is one signature over a Merkle root of blinded
//! leaves; each package exports as a standalone record. These cases use real
//! software-key signatures; the hardware assertion path is covered by the
//! SSHSIG suite's crafted blobs.

mod support;

use std::cell::Cell;

use louiselm_skills::{
    Digest, ReviewDepth, Signer, SshKeygenSigner,
    endorsement::{
        self, Batch, ENDORSEMENT_NAMESPACE, EndorsedPackage, EndorsementError, EndorserRole,
        MAX_BATCH, Record, Signing, StatedAssessment, StatementKind, Subject,
    },
    signer::SignerError,
    sshsig::{self, ADMISSION_NAMESPACE, SignatureError, SkPolicy},
};
use serde_json::Value;
use support::{Fixture, SshKey, write_file};

const ISSUED: u64 = 1_800_000_000_000;
const EXPIRES: u64 = ISSUED + 86_400_000;

fn digest(byte: char) -> Digest {
    Digest::parse(&byte.to_string().repeat(64)).unwrap()
}

fn signing(key: &SshKey) -> Signing {
    Signing {
        signer_role: EndorserRole::Reviewer,
        signer_key: key.public_key(),
        issued_at_ms: ISSUED,
        expires_at_ms: EXPIRES,
    }
}

fn package(subject: Digest) -> EndorsedPackage {
    EndorsedPackage {
        package_digest: subject,
        policy_digest: digest('b'),
        dossier_digest: Some(digest('c')),
        assessment: Some(StatedAssessment {
            model: "gpt-5.6-luna".into(),
            prompt_version: "assessment/3".into(),
        }),
        review_depth: ReviewDepth::Read,
        assumptions: vec!["Agent runs inside a Contained Session".into()],
    }
}

fn signer(key: &SshKey) -> SshKeygenSigner {
    SshKeygenSigner::new(key.private_key_path())
}

fn sign(key: &SshKey, batch: &Batch) -> Vec<Vec<u8>> {
    endorsement::sign_batch(&signing(key), batch, &signer(key)).expect("a valid batch signs")
}

fn signed(fixture: &Fixture, name: &str) -> (SshKey, Vec<u8>) {
    let key = SshKey::generate(fixture, name);
    let mut records = sign(&key, &Batch::Endorsements(vec![package(digest('a'))]));
    (key, records.remove(0))
}

fn edit(bytes: &[u8], change: impl FnOnce(&mut Value)) -> Vec<u8> {
    let mut value: Value = serde_json::from_slice(bytes).unwrap();
    change(&mut value);
    serde_json::to_vec(&value).unwrap()
}

fn verified(bytes: &[u8]) -> Record {
    let record = endorsement::parse(bytes).expect("record parses");
    record
        .verify(ISSUED, SkPolicy::none())
        .expect("record verifies");
    record
}

struct CountingSigner<'a> {
    inner: SshKeygenSigner,
    calls: &'a Cell<usize>,
}

impl Signer for CountingSigner<'_> {
    fn sign(&self, namespace: &str, payload: &[u8]) -> Result<String, SignerError> {
        self.calls.set(self.calls.get() + 1);
        self.inner.sign(namespace, payload)
    }
}

#[test]
fn a_signed_endorsement_verifies_and_reports_its_subject() {
    let fixture = Fixture::new();
    let (key, bytes) = signed(&fixture, "reviewer");

    let record = endorsement::parse(&bytes).expect("signed bytes parse");
    let subject = record
        .verify(ISSUED, SkPolicy::none())
        .expect("a fresh signature by the stated key verifies");

    assert_eq!(subject, &Subject::Endorsed(package(digest('a'))));
    assert_eq!(record.kind(), StatementKind::Endorsement);
    assert_eq!(record.signing(), &signing(&key));
    assert!(record.covers(&digest('a')));
    assert!(!record.covers(&digest('d')));
    assert_eq!(record.to_bytes(), bytes);
}

#[test]
fn one_signature_covers_a_batch_and_each_record_stands_alone() {
    let fixture = Fixture::new();
    let key = SshKey::generate(&fixture, "publisher");
    let calls = Cell::new(0);
    let subjects: Vec<Digest> = "12345".chars().map(digest).collect();
    let batch = Batch::Endorsements(subjects.iter().cloned().map(package).collect());

    let records = endorsement::sign_batch(
        &signing(&key),
        &batch,
        &CountingSigner {
            inner: signer(&key),
            calls: &calls,
        },
    )
    .unwrap();

    assert_eq!(calls.get(), 1, "one touch signs the whole batch");
    assert_eq!(records.len(), subjects.len());
    for subject in &subjects {
        let mine: Vec<_> = records
            .iter()
            .filter(|bytes| verified(bytes).covers(subject))
            .collect();
        assert_eq!(mine.len(), 1, "exactly one record per subject");
        let text = String::from_utf8(mine[0].clone()).unwrap();
        for other in subjects.iter().filter(|other| *other != subject) {
            assert!(
                !text.contains(other.hex()),
                "record for {subject} reveals {other}"
            );
        }
    }
}

#[test]
fn blinding_makes_sibling_hashes_unlinkable_across_batches() {
    let fixture = Fixture::new();
    let key = SshKey::generate(&fixture, "reviewer");
    let batch = Batch::Endorsements(vec![package(digest('a')), package(digest('e'))]);
    let proof_for_a = |records: Vec<Vec<u8>>| {
        let bytes = records
            .into_iter()
            .find(|bytes| endorsement::parse(bytes).unwrap().covers(&digest('a')))
            .unwrap();
        let value: Value = serde_json::from_slice(&bytes).unwrap();
        value["proof"].clone()
    };

    let first = proof_for_a(sign(&key, &batch));
    let second = proof_for_a(sign(&key, &batch));

    assert_eq!(first.as_array().unwrap().len(), 1);
    assert_ne!(
        first, second,
        "the same sibling must not hash the same way twice"
    );
}

#[test]
fn equal_bytes_in_different_places_share_one_subject_without_leaking_paths() {
    let here = Fixture::new();
    let there = Fixture::new();
    let first = here.candidate("home/alice/skills/demo");
    write_file(&first.join("SKILL.md"), "---\nname: demo\n---\nbody\n");
    let second = there.candidate("srv/elsewhere/renamed");
    write_file(&second.join("SKILL.md"), "---\nname: demo\n---\nbody\n");
    let (left, _) = here.capture(&first).unwrap();
    let (right, _) = there.capture(&second).unwrap();

    let key = SshKey::generate(&here, "publisher");
    let mut publisher = signing(&key);
    publisher.signer_role = EndorserRole::Publisher;
    let bytes = endorsement::sign_batch(
        &publisher,
        &Batch::Endorsements(vec![package(left.digest.clone())]),
        &signer(&key),
    )
    .unwrap()
    .remove(0);

    assert!(verified(&bytes).covers(&right.digest));
    let text = String::from_utf8(bytes).unwrap();
    for local in [&first, &second, &left.root, &right.root] {
        assert!(
            !text.contains(&*local.to_string_lossy()),
            "record leaks local path {}",
            local.display()
        );
    }
    assert!(!text.contains("alice") && !text.contains("renamed"));
}

#[test]
fn the_canonical_form_is_sorted_compact_json() {
    let fixture = Fixture::new();
    let (_, bytes) = signed(&fixture, "reviewer");

    assert_eq!(edit(&bytes, |_| ()), bytes);
    let pretty =
        serde_json::to_vec_pretty(&serde_json::from_slice::<Value>(&bytes).unwrap()).unwrap();
    assert!(matches!(
        endorsement::parse(&pretty),
        Err(EndorsementError::NonCanonical)
    ));
}

#[test]
fn unknown_fields_are_refused_with_their_name() {
    let fixture = Fixture::new();
    let (_, bytes) = signed(&fixture, "reviewer");

    for (forged, name) in [
        (
            edit(&bytes, |value| {
                value["leaf"]["installed_set"] = Value::from("leaky");
            }),
            "installed_set",
        ),
        (
            edit(&bytes, |value| {
                value["header"]["machine_id"] = Value::from("host");
            }),
            "machine_id",
        ),
        (
            edit(&bytes, |value| value["mirror"] = Value::from("x")),
            "mirror",
        ),
    ] {
        match endorsement::parse(&forged) {
            Err(EndorsementError::Malformed(reason)) => {
                assert!(reason.contains(name), "{reason}");
            }
            other => panic!("unexpected {other:?}"),
        }
    }
}

#[test]
fn a_tampered_leaf_breaks_the_proof_and_a_tampered_header_the_signature() {
    let fixture = Fixture::new();
    let (_, bytes) = signed(&fixture, "reviewer");

    let leaf = edit(&bytes, |value| {
        value["leaf"]["review_depth"] = Value::from("reproduced");
    });
    let error = endorsement::parse(&leaf)
        .unwrap()
        .verify(ISSUED, SkPolicy::none())
        .unwrap_err();
    assert!(
        matches!(error, EndorsementError::ProofMismatch),
        "{error:?}"
    );

    let header = edit(&bytes, |value| {
        value["header"]["expires_at_ms"] = Value::from(EXPIRES + 1);
    });
    let error = endorsement::parse(&header)
        .unwrap()
        .verify(ISSUED, SkPolicy::none())
        .unwrap_err();
    assert!(
        matches!(
            error,
            EndorsementError::Signature(SignatureError::Cryptographic(_))
        ),
        "{error:?}"
    );
}

#[test]
fn the_signed_header_binds_the_tree_size_an_audit_path_cannot() {
    let fixture = Fixture::new();
    let key = SshKey::generate(&fixture, "reviewer");
    let records = sign(
        &key,
        &Batch::Endorsements("123".chars().map(digest).map(package).collect()),
    );
    let first = records
        .iter()
        .find(|bytes| serde_json::from_slice::<Value>(bytes).unwrap()["leaf_index"] == 0)
        .unwrap();

    // RFC 9162: this left-subtree path also hashes to the root as a 4-leaf
    // tree. Only the signature over leaf_count refuses the forgery.
    let forged = edit(first, |value| {
        value["header"]["leaf_count"] = Value::from(4);
    });
    let error = endorsement::parse(&forged)
        .unwrap()
        .verify(ISSUED, SkPolicy::none())
        .unwrap_err();
    assert!(
        matches!(
            error,
            EndorsementError::Signature(SignatureError::Cryptographic(_))
        ),
        "{error:?}"
    );
}

#[test]
fn a_moved_or_altered_proof_is_refused() {
    let fixture = Fixture::new();
    let key = SshKey::generate(&fixture, "reviewer");
    let records = sign(
        &key,
        &Batch::Endorsements("123".chars().map(digest).map(package).collect()),
    );
    let bytes = &records[0];
    let value: Value = serde_json::from_slice(bytes).unwrap();
    let index = value["leaf_index"].as_u64().unwrap();
    let other_proof = serde_json::from_slice::<Value>(&records[1]).unwrap()["proof"].clone();

    for forged in [
        edit(bytes, |value| {
            value["leaf_index"] = Value::from((index + 1) % 3);
        }),
        edit(bytes, |value| value["proof"] = other_proof),
        edit(bytes, |value| {
            value["proof"][0] = Value::from(digest('f').to_string());
        }),
        edit(bytes, |value| {
            value["proof"].as_array_mut().unwrap().pop();
        }),
    ] {
        let error = endorsement::parse(&forged)
            .unwrap()
            .verify(ISSUED, SkPolicy::none())
            .unwrap_err();
        assert!(
            matches!(error, EndorsementError::ProofMismatch),
            "{error:?}"
        );
    }

    let outside = edit(bytes, |value| value["leaf_index"] = Value::from(3));
    assert!(matches!(
        endorsement::parse(&outside),
        Err(EndorsementError::Invalid {
            field: "leaf_index",
            ..
        })
    ));
}

#[test]
fn endorsement_and_admission_signatures_never_substitute() {
    let fixture = Fixture::new();
    let (key, bytes) = signed(&fixture, "reviewer");
    let record = endorsement::parse(&bytes).unwrap();

    let error = sshsig::verify(
        record.signature(),
        ADMISSION_NAMESPACE,
        record.header_bytes(),
        &key.public_key(),
        SkPolicy::none(),
    )
    .unwrap_err();
    assert!(
        matches!(error, SignatureError::NamespaceMismatch { .. }),
        "{error}"
    );

    let admission_signature = key.sign(ADMISSION_NAMESPACE, record.header_bytes());
    let forged = edit(&bytes, |value| {
        value["signature"] = Value::from(admission_signature);
    });
    let error = endorsement::parse(&forged)
        .unwrap()
        .verify(ISSUED, SkPolicy::none())
        .unwrap_err();
    assert!(
        matches!(
            &error,
            EndorsementError::Signature(SignatureError::NamespaceMismatch { expected, .. })
                if expected == ENDORSEMENT_NAMESPACE
        ),
        "{error:?}"
    );
}

#[test]
fn local_authority_roles_cannot_be_claimed_by_an_endorser() {
    let fixture = Fixture::new();
    let (_, bytes) = signed(&fixture, "reviewer");

    for role in ["primary", "release", "recovery"] {
        let forged = edit(&bytes, |value| {
            value["header"]["signer_role"] = Value::from(role);
        });
        assert!(
            matches!(
                endorsement::parse(&forged),
                Err(EndorsementError::Malformed(_))
            ),
            "{role} accepted"
        );
    }
}

#[test]
fn a_signature_by_another_key_is_refused_before_cryptography() {
    let fixture = Fixture::new();
    let (_, bytes) = signed(&fixture, "reviewer");
    let other = SshKey::generate(&fixture, "other");
    let record = endorsement::parse(&bytes).unwrap();
    let forged = edit(&bytes, |value| {
        value["signature"] = Value::from(other.sign(ENDORSEMENT_NAMESPACE, record.header_bytes()));
    });

    let error = endorsement::parse(&forged)
        .unwrap()
        .verify(ISSUED, SkPolicy::none())
        .unwrap_err();
    assert!(
        matches!(
            error,
            EndorsementError::Signature(SignatureError::KeyMismatch { .. })
        ),
        "{error:?}"
    );
}

#[test]
fn signing_refuses_a_signer_that_is_not_the_stated_key() {
    let fixture = Fixture::new();
    let stated = SshKey::generate(&fixture, "stated");
    let actual = SshKey::generate(&fixture, "actual");

    let error = endorsement::sign_batch(
        &signing(&stated),
        &Batch::Endorsements(vec![package(digest('a'))]),
        &signer(&actual),
    )
    .unwrap_err();
    assert!(
        matches!(
            error,
            EndorsementError::Signature(SignatureError::KeyMismatch { .. })
        ),
        "{error:?}"
    );
}

#[test]
fn unsupported_schema_algorithm_and_kind_are_named_before_any_signature_check() {
    let fixture = Fixture::new();
    let (_, bytes) = signed(&fixture, "reviewer");

    let future_record = edit(&bytes, |value| {
        value["schema"] = Value::from("louiselm.skills.endorsement-record/2");
        value["header"]["privacy_proof"] = Value::from("zk");
    });
    assert!(matches!(
        endorsement::parse(&future_record),
        Err(EndorsementError::UnsupportedSchema(found))
            if found == "louiselm.skills.endorsement-record/2"
    ));

    let future_header = edit(&bytes, |value| {
        value["header"]["schema"] = Value::from("louiselm.skills.endorsement/2");
    });
    assert!(matches!(
        endorsement::parse(&future_header),
        Err(EndorsementError::UnsupportedSchema(found))
            if found == "louiselm.skills.endorsement/2"
    ));

    let future_algorithm = edit(&bytes, |value| {
        value["header"]["signature_algorithm"] = Value::from("ml-dsa/1");
    });
    assert!(matches!(
        endorsement::parse(&future_algorithm),
        Err(EndorsementError::UnsupportedAlgorithm(found)) if found == "ml-dsa/1"
    ));

    let future_kind = edit(&bytes, |value| {
        value["header"]["kind"] = Value::from("negative_review");
    });
    assert!(matches!(
        endorsement::parse(&future_kind),
        Err(EndorsementError::UnsupportedKind(found)) if found == "negative_review"
    ));
}

#[test]
fn a_withdrawal_names_only_its_subject_and_cannot_pose_as_an_endorsement() {
    let fixture = Fixture::new();
    let key = SshKey::generate(&fixture, "reviewer");
    let records = sign(&key, &Batch::Withdrawals(vec![digest('a'), digest('e')]));
    assert_eq!(records.len(), 2);

    let record = verified(&records[0]);
    assert_eq!(record.kind(), StatementKind::Withdrawal);
    let Subject::Withdrawn(subject) = record.subject() else {
        panic!("withdrawal parsed as {:?}", record.subject());
    };
    assert!([digest('a'), digest('e')].contains(subject));

    let as_endorsement = edit(&records[0], |value| {
        value["header"]["kind"] = Value::from("endorsement");
    });
    assert!(matches!(
        endorsement::parse(&as_endorsement),
        Err(EndorsementError::Malformed(_))
    ));

    let (_, endorsed) = signed(&fixture, "other");
    let as_withdrawal = edit(&endorsed, |value| {
        value["header"]["kind"] = Value::from("withdrawal");
    });
    assert!(matches!(
        endorsement::parse(&as_withdrawal),
        Err(EndorsementError::Malformed(_))
    ));
}

#[test]
fn validity_is_half_open_from_issue_to_expiry() {
    let fixture = Fixture::new();
    let (_, bytes) = signed(&fixture, "reviewer");
    let record = endorsement::parse(&bytes).unwrap();

    assert!(record.verify(EXPIRES - 1, SkPolicy::none()).is_ok());
    assert!(matches!(
        record.verify(EXPIRES, SkPolicy::none()),
        Err(EndorsementError::Expired { expires_at_ms }) if expires_at_ms == EXPIRES
    ));
    assert!(matches!(
        record.verify(ISSUED - 1, SkPolicy::none()),
        Err(EndorsementError::NotYetValid { issued_at_ms }) if issued_at_ms == ISSUED
    ));
}

#[test]
fn impossible_batches_are_refused_before_signing() {
    let fixture = Fixture::new();
    let key = SshKey::generate(&fixture, "reviewer");
    let calls = Cell::new(0);
    let counting = CountingSigner {
        inner: signer(&key),
        calls: &calls,
    };
    let refuse = |signing: &Signing, batch: &Batch| {
        endorsement::sign_batch(signing, batch, &counting).unwrap_err()
    };

    let mut backwards = signing(&key);
    backwards.expires_at_ms = ISSUED;
    let one = Batch::Endorsements(vec![package(digest('a'))]);
    let mut multiline = package(digest('a'));
    multiline.assumptions = vec!["one\nsigned: by someone else".into()];
    let too_many = Batch::Withdrawals(
        (0..=MAX_BATCH)
            .map(|index| Digest::of(&index.to_le_bytes()))
            .collect(),
    );

    for (error, field) in [
        (refuse(&backwards, &one), "expires_at_ms"),
        (
            refuse(&signing(&key), &Batch::Endorsements(vec![multiline])),
            "assumptions",
        ),
        (
            refuse(&signing(&key), &Batch::Endorsements(Vec::new())),
            "batch",
        ),
        (
            refuse(
                &signing(&key),
                &Batch::Withdrawals(vec![digest('a'), digest('a')]),
            ),
            "package_digest",
        ),
        (refuse(&signing(&key), &too_many), "batch"),
    ] {
        assert!(
            matches!(error, EndorsementError::Invalid { field: found, .. } if found == field),
            "{field}: {error:?}"
        );
    }
    assert_eq!(calls.get(), 0, "no signature was requested");
}

#[test]
fn impossible_fields_are_refused_at_parsing() {
    let fixture = Fixture::new();
    let (_, bytes) = signed(&fixture, "reviewer");

    for (part, field, value) in [
        ("leaf", "package_digest", Value::from("sha256-abc")),
        ("leaf", "review_depth", Value::from("thorough")),
        ("leaf", "nonce", Value::from("not-random")),
        (
            "header",
            "signer_key",
            Value::from("ssh-ed25519 AAAA trailing-comment"),
        ),
        ("header", "root", Value::from("sha256:short")),
    ] {
        let forged = edit(&bytes, |record| record[part][field] = value);
        assert!(
            matches!(
                endorsement::parse(&forged),
                Err(EndorsementError::Invalid { field: found, .. }) if found == field
            ),
            "{field} accepted"
        );
    }
}

#[test]
fn oversized_input_is_refused_before_parsing() {
    let huge = vec![b' '; endorsement::MAX_RECORD_BYTES + 1];
    assert!(matches!(
        endorsement::parse(&huge),
        Err(EndorsementError::Oversized { .. })
    ));
}

#[test]
fn a_software_key_does_not_satisfy_a_hardware_requirement() {
    let fixture = Fixture::new();
    let (_, bytes) = signed(&fixture, "reviewer");

    let error = endorsement::parse(&bytes)
        .unwrap()
        .verify(ISSUED, SkPolicy::require_presence_and_verification())
        .unwrap_err();
    assert!(
        matches!(
            error,
            EndorsementError::Signature(SignatureError::NotHardwareBacked { .. })
        ),
        "{error:?}"
    );
}
