//! Behavioral coverage for release-bound default endorsers and the local layer.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "Test fixtures abort on setup failure and assert failures directly."
)]

//! Raising trust needs the operator's Primary key; lowering it never does.
//! Stored additions are re-verified on every load against the current Primary
//! key, so tampering or a key rotation fails safe: the addition is dropped.

mod support;

use std::fs;

use louiselm_skills::{
    Digest, ReviewDepth, SshKeygenSigner, TrustStore,
    endorsement::{
        self, Batch, EndorsedPackage, EndorserRole, Signing,
        local::{self, LocalError},
        trust::{
            self, DEFAULTS_SCHEMA, EndorserEntry, EndorserError, MAX_LIFETIME_MS, TrustDecision,
        },
    },
    sshsig::{SignatureError, SkPolicy},
};
use support::{Fixture, SshKey};

const NOW: u64 = 1_800_000_000_000;

struct Operator {
    fixture: Fixture,
    primary: SshKey,
}

impl Operator {
    fn new() -> Self {
        let fixture = Fixture::new();
        let primary = SshKey::generate(&fixture, "primary");
        let release = SshKey::generate(&fixture, "release");
        TrustStore::bootstrap(
            &fixture.store(),
            "louiselm/skills",
            &primary.public_key(),
            &release.public_key(),
            SkPolicy::none(),
            NOW,
        )
        .unwrap();
        Self { fixture, primary }
    }

    fn primary(&self) -> SshKeygenSigner {
        SshKeygenSigner::new(self.primary.private_key_path())
    }

    fn local_file(&self) -> std::path::PathBuf {
        self.fixture.store_root().join("endorsers/local.json")
    }
}

fn entry(key: &SshKey) -> EndorserEntry {
    EndorserEntry {
        label: "Benoît Barthelet, LouiseLM publisher".into(),
        max_lifetime_ms: MAX_LIFETIME_MS,
        public_key: key.public_key(),
        roles: vec![EndorserRole::Publisher],
        sk_policy: SkPolicy::none(),
    }
}

fn record(key: &SshKey) -> endorsement::Record {
    let signing = Signing {
        signer_role: EndorserRole::Publisher,
        signer_key: key.public_key(),
        issued_at_ms: NOW,
        expires_at_ms: NOW + 1,
    };
    let package = EndorsedPackage {
        package_digest: Digest::of(b"package"),
        policy_digest: Digest::of(b"policy"),
        dossier_digest: None,
        assessment: None,
        review_depth: ReviewDepth::Read,
        assumptions: Vec::new(),
    };
    let bytes = endorsement::sign_batch(
        &signing,
        &Batch::Endorsements(vec![package]),
        &SshKeygenSigner::new(key.private_key_path()),
    )
    .unwrap()
    .remove(0);
    endorsement::parse(&bytes).unwrap()
}

fn trusted(operator: &Operator, key: &SshKey) -> bool {
    let (set, _) = local::effective(&operator.fixture.store()).unwrap();
    matches!(set.decide(&record(key), NOW), TrustDecision::Trusted { .. })
}

#[test]
fn the_embedded_default_list_is_valid_and_matches_the_release_asset() {
    let defaults = trust::embedded_defaults();
    assert_eq!(
        defaults,
        vec![EndorserEntry {
            label: "LouiseLM publisher".into(),
            max_lifetime_ms: MAX_LIFETIME_MS,
            public_key: concat!(
                "sk-ssh-ed25519@openssh.com ",
                "AAAAGnNrLXNzaC1lZDI1NTE5QG9wZW5zc2guY29t",
                "AAAAIOIQYUGXsQeLgJJ15D0I14Z1SthIv/vsztEBCQVqsjG8AAAABHNzaDo="
            )
            .into(),
            roles: vec![EndorserRole::Publisher],
            sk_policy: SkPolicy::require_presence_and_verification(),
        }]
    );
    assert_eq!(
        trust::parse_defaults(trust::embedded_default_bytes()).unwrap(),
        defaults
    );
}

#[test]
fn a_default_list_refuses_other_schemas_unknown_fields_and_duplicates() {
    let fixture = Fixture::new();
    let key = SshKey::generate(&fixture, "publisher");
    let listed = serde_json::to_value(entry(&key)).unwrap();

    let other = br#"{"endorsers":[],"schema":"louiselm.skills.endorsers/2"}"#;
    assert!(matches!(
        trust::parse_defaults(other),
        Err(EndorserError::UnsupportedSchema(found)) if found == "louiselm.skills.endorsers/2"
    ));
    let mut extra = listed.clone();
    extra["trusted_for_admission"] = true.into();
    let unknown = serde_json::json!({"endorsers": [extra], "schema": DEFAULTS_SCHEMA});
    assert!(matches!(
        trust::parse_defaults(&serde_json::to_vec(&unknown).unwrap()),
        Err(EndorserError::Malformed(reason)) if reason.contains("trusted_for_admission")
    ));
    let twice =
        serde_json::json!({"endorsers": [listed.clone(), listed], "schema": DEFAULTS_SCHEMA});
    assert!(matches!(
        trust::parse_defaults(&serde_json::to_vec(&twice).unwrap()),
        Err(EndorserError::Duplicate { .. })
    ));
}

#[test]
fn a_primary_signed_addition_is_trusted_after_reload() {
    let operator = Operator::new();
    let publisher = SshKey::generate(&operator.fixture, "publisher");
    assert!(!trusted(&operator, &publisher));

    local::add(
        &operator.fixture.store(),
        &entry(&publisher),
        &operator.primary(),
        NOW,
    )
    .unwrap();

    assert!(trusted(&operator, &publisher));
    let (_, layer) = local::effective(&operator.fixture.store()).unwrap();
    assert_eq!(layer.additions, vec![entry(&publisher)]);
    assert_eq!(layer.dropped, Vec::new());
}

#[test]
fn an_addition_not_signed_by_primary_is_refused_and_nothing_is_written() {
    let operator = Operator::new();
    let publisher = SshKey::generate(&operator.fixture, "publisher");
    let impostor = SshKey::generate(&operator.fixture, "impostor");

    let error = local::add(
        &operator.fixture.store(),
        &entry(&publisher),
        &SshKeygenSigner::new(impostor.private_key_path()),
        NOW,
    )
    .unwrap_err();

    assert!(
        matches!(
            error,
            LocalError::Signature(SignatureError::KeyMismatch { .. })
        ),
        "{error:?}"
    );
    assert!(!operator.local_file().exists());
}

#[test]
fn a_removal_needs_no_signature_and_beats_a_signed_addition() {
    let operator = Operator::new();
    let publisher = SshKey::generate(&operator.fixture, "publisher");
    let store = operator.fixture.store();
    local::add(&store, &entry(&publisher), &operator.primary(), NOW).unwrap();

    local::remove(&store, &publisher.public_key()).unwrap();
    assert!(!trusted(&operator, &publisher));

    local::add(&store, &entry(&publisher), &operator.primary(), NOW + 1).unwrap();
    assert!(
        !trusted(&operator, &publisher),
        "a later addition does not undo a removal; only a signed restore does"
    );
}

#[test]
fn restoring_a_removed_key_needs_primary() {
    let operator = Operator::new();
    let publisher = SshKey::generate(&operator.fixture, "publisher");
    let impostor = SshKey::generate(&operator.fixture, "impostor");
    let store = operator.fixture.store();
    local::add(&store, &entry(&publisher), &operator.primary(), NOW).unwrap();

    assert!(matches!(
        local::restore(&store, &publisher.public_key(), &operator.primary(), NOW),
        Err(LocalError::NotRemoved { .. })
    ));
    local::remove(&store, &publisher.public_key()).unwrap();
    let error = local::restore(
        &store,
        &publisher.public_key(),
        &SshKeygenSigner::new(impostor.private_key_path()),
        NOW,
    )
    .unwrap_err();
    assert!(
        matches!(
            error,
            LocalError::Signature(SignatureError::KeyMismatch { .. })
        ),
        "{error:?}"
    );
    assert!(!trusted(&operator, &publisher));

    local::restore(&store, &publisher.public_key(), &operator.primary(), NOW).unwrap();
    assert!(trusted(&operator, &publisher));
}

#[test]
fn a_tampered_stored_addition_is_dropped_and_reported() {
    let operator = Operator::new();
    let publisher = SshKey::generate(&operator.fixture, "publisher");
    local::add(
        &operator.fixture.store(),
        &entry(&publisher),
        &operator.primary(),
        NOW,
    )
    .unwrap();

    let path = operator.local_file();
    let mut document: serde_json::Value =
        serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    document["additions"][0]["payload"]["entry"]["roles"] =
        serde_json::json!(["publisher", "reviewer"]);
    fs::write(&path, serde_json::to_vec(&document).unwrap()).unwrap();

    let (_, layer) = local::effective(&operator.fixture.store()).unwrap();
    assert_eq!(layer.additions, Vec::new());
    assert_eq!(layer.dropped.len(), 1);
    assert_eq!(layer.dropped[0].public_key, publisher.public_key());
    assert!(!trusted(&operator, &publisher));
}

#[test]
fn additions_signed_under_another_primary_fail_safe() {
    let first = Operator::new();
    let second = Operator::new();
    let publisher = SshKey::generate(&first.fixture, "publisher");
    local::add(
        &first.fixture.store(),
        &entry(&publisher),
        &first.primary(),
        NOW,
    )
    .unwrap();

    fs::create_dir_all(second.local_file().parent().unwrap()).unwrap();
    fs::copy(first.local_file(), second.local_file()).unwrap();

    let (_, layer) = local::effective(&second.fixture.store()).unwrap();
    assert_eq!(layer.additions, Vec::new());
    assert_eq!(layer.dropped.len(), 1);
    assert!(!trusted(&second, &publisher));
}

#[test]
fn without_an_enrolled_primary_nothing_can_be_added() {
    let fixture = Fixture::new();
    let primary = SshKey::generate(&fixture, "primary");
    let publisher = SshKey::generate(&fixture, "publisher");

    assert!(matches!(
        local::add(
            &fixture.store(),
            &entry(&publisher),
            &SshKeygenSigner::new(primary.private_key_path()),
            NOW
        ),
        Err(LocalError::NoPrimary)
    ));
    local::remove(&fixture.store(), &publisher.public_key()).unwrap();
}

#[test]
fn removal_refuses_a_malformed_key() {
    let operator = Operator::new();
    assert!(matches!(
        local::remove(&operator.fixture.store(), "ssh-ed25519 AAAA with comment"),
        Err(LocalError::Endorser(EndorserError::Invalid {
            field: "public_key",
            ..
        }))
    ));
}
