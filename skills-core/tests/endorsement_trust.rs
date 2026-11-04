//! Behavioral coverage for local endorser trust decisions.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "Test fixtures abort on setup failure and assert failures directly."
)]

//! Trust is local and recomputed at use: release defaults plus signed local
//! additions, minus removals, decide whether a verified record may focus
//! review. A refusal always names its reason.

mod support;

use louiselm_skills::{
    Digest, ReviewDepth, SshKeygenSigner,
    endorsement::{
        self, Batch, EndorsedPackage, EndorsementError, EndorserRole, Signing,
        trust::{
            Distrust, EndorserEntry, EndorserError, EndorserSet, MAX_LIFETIME_MS, TrustDecision,
        },
    },
    sshsig::{SignatureError, SkPolicy},
};
use support::{Fixture, SshKey};

const ISSUED: u64 = 1_800_000_000_000;
const DAY: u64 = 86_400_000;

fn digest(byte: char) -> Digest {
    Digest::parse(&byte.to_string().repeat(64)).unwrap()
}

fn entry(key: &SshKey, roles: &[EndorserRole]) -> EndorserEntry {
    EndorserEntry {
        public_key: key.public_key(),
        roles: roles.to_vec(),
        label: "Benoît Barthelet, LouiseLM publisher".into(),
        sk_policy: SkPolicy::none(),
        max_lifetime_ms: 180 * DAY,
    }
}

fn record(key: &SshKey, role: EndorserRole, days: u64) -> endorsement::Record {
    let signing = Signing {
        signer_role: role,
        signer_key: key.public_key(),
        issued_at_ms: ISSUED,
        expires_at_ms: ISSUED + days * DAY,
    };
    let package = EndorsedPackage {
        package_digest: digest('a'),
        policy_digest: digest('b'),
        dossier_digest: None,
        assessment: None,
        review_depth: ReviewDepth::Skimmed,
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

fn set(
    defaults: &[EndorserEntry],
    additions: &[EndorserEntry],
    removals: &[String],
) -> EndorserSet {
    EndorserSet::effective(defaults, additions, removals).expect("valid layers combine")
}

#[test]
fn a_release_default_publisher_is_trusted_for_its_role() {
    let fixture = Fixture::new();
    let key = SshKey::generate(&fixture, "publisher");
    let endorsers = set(&[entry(&key, &[EndorserRole::Publisher])], &[], &[]);

    let decision = endorsers.decide(&record(&key, EndorserRole::Publisher, 90), ISSUED);

    assert!(
        matches!(decision, TrustDecision::Trusted { ref label } if label == "Benoît Barthelet, LouiseLM publisher")
    );
}

#[test]
fn a_removal_always_wins_over_defaults_and_additions() {
    let fixture = Fixture::new();
    let key = SshKey::generate(&fixture, "publisher");
    let publisher = entry(&key, &[EndorserRole::Publisher]);

    for endorsers in [
        set(std::slice::from_ref(&publisher), &[], &[key.public_key()]),
        set(&[], std::slice::from_ref(&publisher), &[key.public_key()]),
    ] {
        assert!(matches!(
            endorsers.decide(&record(&key, EndorserRole::Publisher, 90), ISSUED),
            TrustDecision::Untrusted(Distrust::UnknownSigner)
        ));
    }
}

#[test]
fn an_unknown_signer_is_untrusted_even_with_a_valid_signature() {
    let fixture = Fixture::new();
    let known = SshKey::generate(&fixture, "publisher");
    let stranger = SshKey::generate(&fixture, "stranger");
    let endorsers = set(&[entry(&known, &[EndorserRole::Publisher])], &[], &[]);

    assert!(matches!(
        endorsers.decide(&record(&stranger, EndorserRole::Publisher, 90), ISSUED),
        TrustDecision::Untrusted(Distrust::UnknownSigner)
    ));
}

#[test]
fn a_publisher_only_entry_refuses_reviewer_statements_by_the_same_key() {
    let fixture = Fixture::new();
    let key = SshKey::generate(&fixture, "publisher");
    let endorsers = set(&[entry(&key, &[EndorserRole::Publisher])], &[], &[]);

    assert!(matches!(
        endorsers.decide(&record(&key, EndorserRole::Reviewer, 90), ISSUED),
        TrustDecision::Untrusted(Distrust::RoleNotAllowed {
            role: EndorserRole::Reviewer
        })
    ));
}

#[test]
fn a_window_longer_than_the_entry_cap_is_untrusted_despite_a_valid_signature() {
    let fixture = Fixture::new();
    let key = SshKey::generate(&fixture, "publisher");
    let mut capped = entry(&key, &[EndorserRole::Publisher]);
    capped.max_lifetime_ms = 30 * DAY;
    let endorsers = set(&[capped], &[], &[]);

    assert!(
        matches!(endorsers.decide(&record(&key, EndorserRole::Publisher, 30), ISSUED), TrustDecision::Trusted { ref label } if label == "Benoît Barthelet, LouiseLM publisher")
    );
    assert!(matches!(
        endorsers.decide(&record(&key, EndorserRole::Publisher, 31), ISSUED),
        TrustDecision::Untrusted(Distrust::LifetimeExceedsCap {
            lifetime_ms,
            cap_ms
        }) if lifetime_ms == 31 * DAY && cap_ms == 30 * DAY
    ));
}

#[test]
fn the_entry_hardware_policy_is_enforced_on_verification() {
    let fixture = Fixture::new();
    let key = SshKey::generate(&fixture, "publisher");
    let mut hardware = entry(&key, &[EndorserRole::Publisher]);
    hardware.sk_policy = SkPolicy::require_presence_and_verification();
    let endorsers = set(&[hardware], &[], &[]);

    let decision = endorsers.decide(&record(&key, EndorserRole::Publisher, 90), ISSUED);
    assert!(
        matches!(
            decision,
            TrustDecision::Untrusted(Distrust::Refused(EndorsementError::Signature(
                SignatureError::NotHardwareBacked { .. }
            )))
        ),
        "{decision:?}"
    );
}

#[test]
fn expiry_and_future_issue_are_refusals_with_their_times() {
    let fixture = Fixture::new();
    let key = SshKey::generate(&fixture, "publisher");
    let endorsers = set(&[entry(&key, &[EndorserRole::Publisher])], &[], &[]);
    let record = record(&key, EndorserRole::Publisher, 90);

    assert!(matches!(
        endorsers.decide(&record, ISSUED + 90 * DAY),
        TrustDecision::Untrusted(Distrust::Refused(EndorsementError::Expired { .. }))
    ));
    assert!(matches!(
        endorsers.decide(&record, ISSUED - 1),
        TrustDecision::Untrusted(Distrust::Refused(EndorsementError::NotYetValid { .. }))
    ));
}

#[test]
fn a_signed_local_addition_replaces_the_default_for_the_same_key() {
    let fixture = Fixture::new();
    let key = SshKey::generate(&fixture, "publisher");
    let default = entry(&key, &[EndorserRole::Publisher]);
    let mut widened = entry(&key, &[EndorserRole::Publisher, EndorserRole::Reviewer]);
    widened.label = "Benoît, also reviewing".into();
    let endorsers = set(&[default], &[widened], &[]);

    assert!(
        matches!(endorsers.decide(&record(&key, EndorserRole::Reviewer, 90), ISSUED), TrustDecision::Trusted { ref label } if label == "Benoît, also reviewing")
    );
    assert_eq!(endorsers.entries().len(), 1);
}

#[test]
fn malformed_or_duplicate_entries_are_refused_by_name() {
    let fixture = Fixture::new();
    let key = SshKey::generate(&fixture, "publisher");
    let good = entry(&key, &[EndorserRole::Publisher]);
    let with = |change: fn(&mut EndorserEntry)| {
        let mut entry = good.clone();
        change(&mut entry);
        entry
    };

    let cases = [
        (with(|entry| entry.roles.clear()), "roles"),
        (with(|entry| entry.label = "two\nlines".into()), "label"),
        (
            with(|entry| entry.public_key.push_str(" comment")),
            "public_key",
        ),
        (with(|entry| entry.max_lifetime_ms = 0), "max_lifetime_ms"),
        (
            with(|entry| entry.max_lifetime_ms = MAX_LIFETIME_MS + 1),
            "max_lifetime_ms",
        ),
    ];
    for (entry, field) in cases {
        let error = EndorserSet::effective(&[entry], &[], &[]).unwrap_err();
        assert!(
            matches!(error, EndorserError::Invalid { field: found, .. } if found == field),
            "{field}: {error:?}"
        );
    }

    let error = EndorserSet::effective(&[good.clone(), good.clone()], &[], &[]).unwrap_err();
    assert!(
        matches!(error, EndorserError::Duplicate { .. }),
        "{error:?}"
    );
    let error = EndorserSet::effective(&[], &[good.clone(), good], &[]).unwrap_err();
    assert!(
        matches!(error, EndorserError::Duplicate { .. }),
        "{error:?}"
    );
}

#[test]
fn the_maximum_cap_is_one_hundred_eighty_days() {
    assert_eq!(MAX_LIFETIME_MS, 180 * DAY);
}
