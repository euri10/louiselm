//! Behavioral coverage for portable Endorsement records.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "Test fixtures abort on setup failure and assert failures directly."
)]

//! An Endorsement is evidence about exact portable bytes, signed in its own
//! namespace. These cases use real software-key signatures; the hardware
//! assertion path is covered by the SSHSIG suite's crafted blobs.

mod support;

use louiselm_skills::{
    Digest, ReviewDepth, SshKeygenSigner,
    endorsement::{
        self, ENDORSEMENT_NAMESPACE, EndorsementError, EndorserRole, StatedAssessment, Statement,
    },
    sshsig::{self, ADMISSION_NAMESPACE, SignatureError, SkPolicy},
};
use serde_json::Value;
use support::{Fixture, SshKey, write_file};

const ISSUED: u64 = 1_800_000_000_000;
const EXPIRES: u64 = ISSUED + 86_400_000;

fn digest(byte: char) -> Digest {
    Digest::parse(&byte.to_string().repeat(64)).unwrap()
}

fn statement(key: &SshKey, package: Digest) -> Statement {
    Statement {
        package_digest: package,
        policy_digest: digest('b'),
        dossier_digest: Some(digest('c')),
        assessment: Some(StatedAssessment {
            model: "gpt-5.6-luna".into(),
            prompt_version: "assessment/3".into(),
        }),
        review_depth: ReviewDepth::Read,
        assumptions: vec!["Agent runs inside a Contained Session".into()],
        signer_role: EndorserRole::Reviewer,
        signer_key: key.public_key(),
        issued_at_ms: ISSUED,
        expires_at_ms: EXPIRES,
    }
}

fn signed(fixture: &Fixture, name: &str) -> (SshKey, Vec<u8>) {
    let key = SshKey::generate(fixture, name);
    let bytes = endorsement::sign(
        &statement(&key, digest('a')),
        &SshKeygenSigner::new(key.private_key_path()),
    )
    .expect("a valid statement signs");
    (key, bytes)
}

fn edit(bytes: &[u8], change: impl FnOnce(&mut Value)) -> Vec<u8> {
    let mut value: Value = serde_json::from_slice(bytes).unwrap();
    change(&mut value);
    serde_json::to_vec(&value).unwrap()
}

#[test]
fn a_signed_endorsement_verifies_and_reports_its_statement() {
    let fixture = Fixture::new();
    let (key, bytes) = signed(&fixture, "reviewer");

    let record = endorsement::parse(&bytes).expect("signed bytes parse");
    let verified = record
        .verify(ISSUED, SkPolicy::none())
        .expect("a fresh signature by the stated key verifies");

    assert_eq!(verified, &statement(&key, digest('a')));
    assert!(record.covers(&digest('a')));
    assert!(!record.covers(&digest('d')));
    assert_eq!(record.to_bytes(), bytes);
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
    let mut stated = statement(&key, left.digest.clone());
    stated.signer_role = EndorserRole::Publisher;
    let bytes = endorsement::sign(&stated, &SshKeygenSigner::new(key.private_key_path())).unwrap();

    let record = endorsement::parse(&bytes).unwrap();
    assert!(record.covers(&right.digest));
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

    for forged in [
        edit(&bytes, |value| {
            value["payload"]["installed_set"] = Value::from("leaky");
        }),
        edit(&bytes, |value| value["mirror"] = Value::from("x")),
    ] {
        match endorsement::parse(&forged) {
            Err(EndorsementError::Malformed(reason)) => assert!(
                reason.contains("installed_set") || reason.contains("mirror"),
                "{reason}"
            ),
            other => panic!("unexpected {other:?}"),
        }
    }
}

#[test]
fn a_tampered_statement_fails_cryptographic_verification() {
    let fixture = Fixture::new();
    let (_, bytes) = signed(&fixture, "reviewer");
    let forged = edit(&bytes, |value| {
        value["payload"]["review_depth"] = Value::from("reproduced");
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
fn endorsement_and_admission_signatures_never_substitute() {
    let fixture = Fixture::new();
    let (key, bytes) = signed(&fixture, "reviewer");
    let record = endorsement::parse(&bytes).unwrap();

    let error = sshsig::verify(
        record.signature(),
        ADMISSION_NAMESPACE,
        record.statement_bytes(),
        &key.public_key(),
        SkPolicy::none(),
    )
    .unwrap_err();
    assert!(
        matches!(error, SignatureError::NamespaceMismatch { .. }),
        "{error}"
    );

    let admission_signature = key.sign(ADMISSION_NAMESPACE, record.statement_bytes());
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
            value["payload"]["signer_role"] = Value::from(role);
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
        value["signature"] =
            Value::from(other.sign(ENDORSEMENT_NAMESPACE, record.statement_bytes()));
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

    let error = endorsement::sign(
        &statement(&stated, digest('a')),
        &SshKeygenSigner::new(actual.private_key_path()),
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
fn unsupported_schema_and_algorithm_are_named_before_any_signature_check() {
    let fixture = Fixture::new();
    let (_, bytes) = signed(&fixture, "reviewer");

    let future_record = edit(&bytes, |value| {
        value["schema"] = Value::from("louiselm.skills.endorsement-record/2");
        value["payload"]["privacy_proof"] = Value::from("zk");
    });
    assert!(matches!(
        endorsement::parse(&future_record),
        Err(EndorsementError::UnsupportedSchema(found))
            if found == "louiselm.skills.endorsement-record/2"
    ));

    let future_statement = edit(&bytes, |value| {
        value["payload"]["schema"] = Value::from("louiselm.skills.endorsement/2");
    });
    assert!(matches!(
        endorsement::parse(&future_statement),
        Err(EndorsementError::UnsupportedSchema(found))
            if found == "louiselm.skills.endorsement/2"
    ));

    let future_algorithm = edit(&bytes, |value| {
        value["payload"]["signature_algorithm"] = Value::from("ml-dsa/1");
    });
    assert!(matches!(
        endorsement::parse(&future_algorithm),
        Err(EndorsementError::UnsupportedAlgorithm(found)) if found == "ml-dsa/1"
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
fn impossible_statements_are_refused_at_signing_and_parsing() {
    let fixture = Fixture::new();
    let key = SshKey::generate(&fixture, "reviewer");
    let signer = SshKeygenSigner::new(key.private_key_path());

    let mut backwards = statement(&key, digest('a'));
    backwards.expires_at_ms = ISSUED;
    assert!(matches!(
        endorsement::sign(&backwards, &signer),
        Err(EndorsementError::Invalid {
            field: "expires_at_ms",
            ..
        })
    ));

    let mut multiline = statement(&key, digest('a'));
    multiline.assumptions = vec!["one\nsigned: by someone else".into()];
    assert!(matches!(
        endorsement::sign(&multiline, &signer),
        Err(EndorsementError::Invalid {
            field: "assumptions",
            ..
        })
    ));

    let (_, bytes) = signed(&fixture, "other");
    for (field, value) in [
        ("package_digest", Value::from("sha256-abc")),
        (
            "signer_key",
            Value::from("ssh-ed25519 AAAA trailing-comment"),
        ),
        ("review_depth", Value::from("thorough")),
    ] {
        let forged = edit(&bytes, |record| record["payload"][field] = value);
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
